//! Bounded streaming transport with a digest authenticated by the signed release manifest.
use anyhow::{anyhow, bail, Result};
use std::{
    fs::{self, File, OpenOptions},
    io::{Read, Seek, SeekFrom, Write},
    path::{Path, PathBuf},
    sync::{
        atomic::{AtomicU64, Ordering},
        Arc, Mutex,
    },
};

pub const MAX_DOWNLOAD_BYTES: u64 = 1024 * 1024 * 1024;

pub fn matches_architecture(name: &str, kind: &str, architecture: &str) -> bool {
    let normalized = name
        .to_ascii_lowercase()
        .replace("x86_64", "x64")
        .replace("amd64", "x64")
        .replace("arm64", "aarch64");
    let tokens = normalized
        .split(|c: char| !c.is_ascii_alphanumeric())
        .collect::<Vec<_>>();
    if kind == "macos-dmg" && tokens.contains(&"universal") {
        return true;
    }
    let expected = match architecture {
        "x86_64" => "x64",
        "aarch64" => "aarch64",
        _ => return false,
    };
    tokens.contains(&expected)
        && !tokens.iter().any(|token| {
            matches!(*token, "x64" | "aarch64" | "i386" | "i686" | "x86") && *token != expected
        })
}

pub fn validate_file_name(name: &str) -> Result<()> {
    if name.is_empty()
        || name.len() > 200
        || name.starts_with('.')
        || name.ends_with('.')
        || !name
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b"._-".contains(&b))
    {
        bail!("更新包文件名不安全");
    }
    let base = name
        .split('.')
        .next()
        .unwrap_or_default()
        .to_ascii_uppercase();
    if matches!(base.as_str(), "CON" | "PRN" | "AUX" | "NUL")
        || (base.len() == 4
            && (base.starts_with("COM") || base.starts_with("LPT"))
            && matches!(base.as_bytes()[3], b'1'..=b'9'))
    {
        bail!("更新包文件名是系统保留名称");
    }
    Ok(())
}

struct PartialDownload {
    file: Option<File>,
    part: PathBuf,
    directory: PathBuf,
}
impl Drop for PartialDownload {
    fn drop(&mut self) {
        self.file.take();
        let _ = fs::remove_file(&self.part);
        // Only our newly-created, empty session directory; never recursive.
        let _ = fs::remove_dir(&self.directory);
    }
}

fn validate_format(file: &mut File, kind: &str, length: u64) -> Result<()> {
    file.seek(SeekFrom::Start(0))?;
    let mut header = [0u8; 4096];
    let n = file.read(&mut header)?;
    let bytes = &header[..n];
    let valid = match kind {
        "windows-portable" | "windows-nsis" | "windows-exe" => {
            if bytes.len() < 64 || !bytes.starts_with(b"MZ") {
                false
            } else {
                let offset = u32::from_le_bytes(bytes[60..64].try_into().unwrap()) as u64;
                if offset > length.saturating_sub(6) {
                    false
                } else {
                    file.seek(SeekFrom::Start(offset))?;
                    let mut pe = [0; 6];
                    file.read_exact(&mut pe)?;
                    let machine = u16::from_le_bytes([pe[4], pe[5]]);
                    // NSIS may use a 32-bit launcher for a 64-bit payload. Payload
                    // architecture is selected by release asset name, not stub bitness.
                    pe[..4] == *b"PE\0\0"
                        && (kind == "windows-nsis" && machine == 0x014c
                            || machine
                                == if cfg!(target_arch = "aarch64") {
                                    0xaa64
                                } else {
                                    0x8664
                                })
                }
            }
        }
        "windows-msi" => bytes.starts_with(&[0xd0, 0xcf, 0x11, 0xe0, 0xa1, 0xb1, 0x1a, 0xe1]),
        "linux-deb" => bytes.starts_with(b"!<arch>\n"),
        "linux-rpm" => bytes.starts_with(&[0xed, 0xab, 0xee, 0xdb]),
        "linux-appimage" => {
            bytes.len() >= 20
                && bytes.starts_with(b"\x7fELF")
                && bytes[18..20]
                    == if cfg!(target_arch = "aarch64") {
                        [183, 0]
                    } else {
                        [62, 0]
                    }
        }
        "macos-dmg" if length >= 512 => {
            file.seek(SeekFrom::End(-512))?;
            let mut magic = [0; 4];
            file.read_exact(&mut magic)?;
            magic == *b"koly"
        }
        _ => false,
    };
    if !valid {
        bail!("更新包格式或架构不匹配，已拒绝安装");
    }
    Ok(())
}

pub async fn download_verified(
    response: reqwest::Response,
    directory: &Path,
    name: &str,
    kind: &str,
    verified: crate::update_auth::VerifiedArtifact,
) -> Result<PathBuf> {
    download_inner(
        response,
        directory,
        name,
        kind,
        Some(verified.size),
        Some(verified.sha256),
    )
    .await
}

#[cfg(test)]
async fn download(
    response: reqwest::Response,
    directory: &Path,
    name: &str,
    kind: &str,
    size: Option<u64>,
) -> Result<PathBuf> {
    download_inner(response, directory, name, kind, size, None).await
}

async fn download_inner(
    mut response: reqwest::Response,
    directory: &Path,
    name: &str,
    kind: &str,
    expected_size: Option<u64>,
    expected_digest: Option<[u8; 32]>,
) -> Result<PathBuf> {
    validate_file_name(name)?;
    if !response.status().is_success() {
        bail!("下载更新包失败: HTTP {}", response.status());
    }
    if response
        .headers()
        .get(reqwest::header::CONTENT_TYPE)
        .and_then(|v| v.to_str().ok())
        .is_some_and(|v| v.contains("text/html"))
    {
        bail!("更新服务返回了网页而非更新包");
    }
    let declared = response.content_length();
    if declared
        .into_iter()
        .chain(expected_size)
        .any(|n| n == 0 || n > MAX_DOWNLOAD_BYTES)
    {
        bail!("更新包大小超出允许范围");
    }
    static SEQUENCE: AtomicU64 = AtomicU64::new(0);
    let nonce = format!(
        "{}-{}-{}",
        std::process::id(),
        crate::state::now_millis(),
        SEQUENCE.fetch_add(1, Ordering::Relaxed)
    );
    let directory = directory.join(".proxy-load-updates").join(nonce);
    let name = name.to_string();
    let partial = tokio::task::spawn_blocking(move || -> Result<_> {
        fs::create_dir_all(directory.parent().unwrap())?;
        fs::create_dir(&directory)?;
        let part = directory.join(format!("{name}.part"));
        let mut state = PartialDownload {
            file: None,
            part,
            directory,
        };
        state.file = Some(
            OpenOptions::new()
                .create_new(true)
                .read(true)
                .write(true)
                .open(&state.part)?,
        );
        Ok(Arc::new(Mutex::new(state)))
    })
    .await??;
    let mut received = 0u64;
    let mut digest = ring::digest::Context::new(&ring::digest::SHA256);
    while let Some(chunk) = response.chunk().await? {
        received = received
            .checked_add(chunk.len() as u64)
            .ok_or_else(|| anyhow!("更新包长度溢出"))?;
        if received > MAX_DOWNLOAD_BYTES || expected_size.is_some_and(|size| received > size) {
            bail!("更新包超过允许大小");
        }
        digest.update(&chunk);
        let state = partial.clone();
        tokio::task::spawn_blocking(move || -> Result<()> {
            state
                .lock()
                .map_err(|_| anyhow!("下载文件锁损坏"))?
                .file
                .as_mut()
                .unwrap()
                .write_all(&chunk)?;
            Ok(())
        })
        .await??;
    }
    if received == 0
        || expected_size
            .into_iter()
            .chain(declared)
            .any(|size| size != received)
    {
        bail!("更新包下载不完整或大小不符");
    }
    let actual_digest = digest.finish();
    if expected_digest.is_some_and(|expected| actual_digest.as_ref() != expected) {
        bail!("更新包内容与签名摘要不符，已拒绝安装");
    }
    let kind = kind.to_string();
    tokio::task::spawn_blocking(move || -> Result<PathBuf> {
        let mut state = partial.lock().map_err(|_| anyhow!("下载文件锁损坏"))?;
        let file = state.file.as_mut().unwrap();
        file.sync_all()?;
        validate_format(file, &kind, received)?;
        state.file.take();
        let target = state.part.with_extension("");
        fs::rename(&state.part, &target)?;
        Ok(target)
    })
    .await?
}

#[cfg(test)]
mod tests {
    use super::*;
    use tokio::{
        io::{AsyncReadExt, AsyncWriteExt},
        net::TcpListener,
    };

    async fn response(body: Vec<u8>, extra: &str, declared: usize) -> reqwest::Response {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let header = format!(
            "HTTP/1.1 200 OK\r\nContent-Length: {declared}\r\nConnection: close\r\n{extra}\r\n"
        );
        tokio::spawn(async move {
            let (mut socket, _) = listener.accept().await.unwrap();
            let mut request = [0; 2048];
            let _ = socket.read(&mut request).await;
            socket.write_all(header.as_bytes()).await.unwrap();
            for chunk in body.chunks(7) {
                if socket.write_all(chunk).await.is_err() {
                    break;
                }
            }
        });
        reqwest::Client::builder()
            .no_proxy()
            .build()
            .unwrap()
            .get(format!("http://{address}/asset"))
            .send()
            .await
            .unwrap()
    }
    fn directory() -> PathBuf {
        static NEXT: AtomicU64 = AtomicU64::new(0);
        std::env::temp_dir().join(format!(
            "proxy-load-download-test-{}-{}-{}",
            std::process::id(),
            crate::state::now_millis(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ))
    }
    fn pe() -> Vec<u8> {
        let mut data = vec![0; 128];
        data[..2].copy_from_slice(b"MZ");
        data[60..64].copy_from_slice(&64u32.to_le_bytes());
        data[64..68].copy_from_slice(b"PE\0\0");
        data[68..70].copy_from_slice(
            &if cfg!(target_arch = "aarch64") {
                0xaa64u16
            } else {
                0x8664u16
            }
            .to_le_bytes(),
        );
        data
    }
    #[tokio::test]
    async fn streamed_download_validates_before_atomic_publication() {
        let dir = directory();
        let data = pe();
        let path = download(
            response(data.clone(), "", data.len()).await,
            &dir,
            "update.exe",
            "windows-portable",
            Some(128),
        )
        .await
        .unwrap();
        assert_eq!(fs::read(&path).unwrap(), data);
        let path2 = download(
            response(data.clone(), "", data.len()).await,
            &dir,
            "update.exe",
            "windows-portable",
            Some(128),
        )
        .await
        .unwrap();
        assert_ne!(path, path2);
        assert_eq!(fs::read(&path).unwrap(), data);
        for p in [path, path2] {
            fs::remove_file(&p).unwrap();
            fs::remove_dir(p.parent().unwrap()).unwrap();
        }
        fs::remove_dir(dir.join(".proxy-load-updates")).unwrap();
        fs::remove_dir(dir).unwrap();
    }

    #[tokio::test]
    async fn structurally_valid_but_tampered_package_is_never_published() {
        let dir = directory();
        let mut bytes = pe();
        let expected: [u8; 32] = ring::digest::digest(&ring::digest::SHA256, &bytes)
            .as_ref()
            .try_into()
            .unwrap();
        bytes[100] ^= 1; // PE headers and size still valid.
        let error = download_verified(
            response(bytes, "", 128).await,
            &dir,
            "update.exe",
            "windows-portable",
            crate::update_auth::VerifiedArtifact {
                size: 128,
                sha256: expected,
            },
        )
        .await
        .unwrap_err();
        assert!(error.to_string().contains("摘要不符"));
        assert_eq!(
            fs::read_dir(dir.join(".proxy-load-updates"))
                .unwrap()
                .count(),
            0
        );
        fs::remove_dir(dir.join(".proxy-load-updates")).unwrap();
        fs::remove_dir(dir).unwrap();
    }
    #[tokio::test]
    async fn html_truncation_oversize_and_wrong_architecture_are_rejected_without_parts() {
        for (data, header, length) in [
            (
                b"<html>oops</html>".to_vec(),
                "Content-Type: text/html\r\n",
                17,
            ),
            (pe(), "", 129),
            (pe(), "", MAX_DOWNLOAD_BYTES as usize + 1),
            (vec![0; 128], "", 128),
        ] {
            let dir = directory();
            let result = download(
                response(data, header, length).await,
                &dir,
                "update.exe",
                "windows-portable",
                None,
            )
            .await;
            assert!(result.is_err());
            let root = dir.join(".proxy-load-updates");
            if root.exists() {
                assert_eq!(fs::read_dir(&root).unwrap().count(), 0);
                fs::remove_dir(root).unwrap();
                fs::remove_dir(dir).unwrap();
            }
        }
    }
    #[test]
    fn filenames_reject_cross_platform_paths_and_devices() {
        for name in [
            "../evil.exe",
            "a/b.exe",
            "a\\b.exe",
            "C:evil.exe",
            "CON.exe",
            "x%2f.exe",
            "x.exe ",
            "x.exe.",
            "x\n.exe",
        ] {
            assert!(validate_file_name(name).is_err(), "{name}");
        }
        assert!(validate_file_name("proxy-load_26.9.11_x64-portable.exe").is_ok());
    }

    #[test]
    fn release_asset_architectures_are_not_interchangeable() {
        assert!(matches_architecture(
            "proxy-load_26.9.11_macos_aarch64.dmg",
            "macos-dmg",
            "aarch64"
        ));
        assert!(!matches_architecture(
            "proxy-load_26.9.11_macos_x64.dmg",
            "macos-dmg",
            "aarch64"
        ));
        assert!(matches_architecture(
            "proxy-load_26.9.11_linux_amd64.deb",
            "linux-deb",
            "x86_64"
        ));
        assert!(matches_architecture(
            "proxy-load_26.9.11_x64-portable.exe",
            "windows-portable",
            "x86_64"
        ));
        assert!(!matches_architecture("unknown.dmg", "macos-dmg", "x86_64"));
    }

    #[tokio::test]
    async fn nsis_32bit_stub_does_not_reject_a_64bit_installer() {
        let dir = directory();
        let mut data = pe();
        data[68..70].copy_from_slice(&0x014cu16.to_le_bytes());
        let path = download(
            response(data, "", 128).await,
            &dir,
            "update_x64-setup.exe",
            "windows-nsis",
            Some(128),
        )
        .await
        .unwrap();
        fs::remove_file(&path).unwrap();
        fs::remove_dir(path.parent().unwrap()).unwrap();
        fs::remove_dir(dir.join(".proxy-load-updates")).unwrap();
        fs::remove_dir(dir).unwrap();
    }

    #[tokio::test]
    async fn cancelling_a_stream_removes_its_partial_file() {
        let dir = directory();
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let server = tokio::spawn(async move {
            let (mut stream, _) = listener.accept().await.unwrap();
            let mut buf = [0; 1024];
            assert!(stream.read(&mut buf).await.unwrap() > 0);
            stream
                .write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 10000\r\n\r\nMZ")
                .await
                .unwrap();
            std::future::pending::<()>().await;
        });
        let response = reqwest::Client::builder()
            .no_proxy()
            .build()
            .unwrap()
            .get(format!("http://{address}/"))
            .send()
            .await
            .unwrap();
        let task = tokio::spawn({
            let dir = dir.clone();
            async move { download(response, &dir, "update.exe", "windows-portable", None).await }
        });
        let root = dir.join(".proxy-load-updates");
        tokio::time::timeout(std::time::Duration::from_secs(2), async {
            while !root.exists() || fs::read_dir(&root).unwrap().count() == 0 {
                tokio::task::yield_now().await;
            }
        })
        .await
        .unwrap();
        task.abort();
        let _ = task.await;
        tokio::time::timeout(std::time::Duration::from_secs(2), async {
            while fs::read_dir(&root).unwrap().count() != 0 {
                tokio::task::yield_now().await;
            }
        })
        .await
        .unwrap();
        fs::remove_dir(root).unwrap();
        fs::remove_dir(dir).unwrap();
        server.abort();
        let _ = server.await;
    }
}
