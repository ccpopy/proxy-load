//! Publisher authentication for the custom updater. The mirror never supplies a trust root.
use anyhow::{anyhow, bail, Context, Result};
use base64::{engine::general_purpose::STANDARD, Engine};
use ring::signature::{UnparsedPublicKey, ED25519};
use serde::Deserialize;

pub const MAX_MANIFEST_BYTES: usize = 16 * 1024;

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Envelope {
    payload: String,
    signature: String,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Manifest {
    schema: u32,
    app: String,
    version: String,
    platform: String,
    arch: String,
    kind: String,
    file_name: String,
    size: u64,
    sha256: String,
}

pub struct ExpectedArtifact<'a> {
    pub file_name: &'a str,
    pub version: &'a str,
    pub platform: &'a str,
    pub arch: &'a str,
    pub kind: &'a str,
    pub size: Option<u64>,
}

// Can only be constructed after signature and policy verification.
pub struct VerifiedArtifact {
    pub(crate) size: u64,
    pub(crate) sha256: [u8; 32],
}

pub fn public_key() -> Result<Vec<u8>> {
    decode_key(option_env!("PROXY_LOAD_UPDATE_PUBLIC_KEY").unwrap_or_default())
}

pub fn manual_install_reason(has_manifest: bool) -> Result<Option<&'static str>> {
    installation_policy(
        option_env!("PROXY_LOAD_UPDATE_MODE").unwrap_or("signed"),
        option_env!("PROXY_LOAD_UPDATE_PUBLIC_KEY").unwrap_or_default(),
        has_manifest,
    )
}

fn installation_policy(mode: &str, key: &str, has_manifest: bool) -> Result<Option<&'static str>> {
    if !matches!(mode, "signed" | "manual") {
        bail!("当前构建的更新模式无效");
    }
    if mode == "manual" || key.trim().is_empty() {
        return Ok(Some("此版本使用手动更新，请从官方 Releases 下载并安装。"));
    }
    decode_key(key)?;
    if !has_manifest {
        return Ok(Some(
            "此更新包未提供签名清单，请前往官方 Releases 手动安装。",
        ));
    }
    Ok(None)
}

fn decode_key(encoded: &str) -> Result<Vec<u8>> {
    let key = STANDARD
        .decode(encoded.trim())
        .context("更新验签公钥格式无效")?;
    if key.len() != 32 {
        bail!("当前构建未配置有效的更新验签公钥，无法自动安装更新");
    }
    Ok(key)
}

pub fn verify(
    bytes: &[u8],
    key: &[u8],
    expected: &ExpectedArtifact<'_>,
) -> Result<VerifiedArtifact> {
    if bytes.len() > MAX_MANIFEST_BYTES {
        bail!("更新签名清单过大");
    }
    let envelope: Envelope = serde_json::from_slice(bytes).context("更新签名清单无效")?;
    let payload = STANDARD
        .decode(&envelope.payload)
        .context("更新签名内容无效")?;
    let signature = STANDARD
        .decode(&envelope.signature)
        .context("更新签名编码无效")?;
    UnparsedPublicKey::new(&ED25519, key)
        .verify(&payload, &signature)
        .map_err(|_| anyhow!("更新包签名验证失败，已拒绝安装"))?;
    let manifest: Manifest = serde_json::from_slice(&payload).context("更新签名内容格式无效")?;
    if manifest.schema != 1
        || manifest.app != "ccpopy/proxy-load"
        || manifest.version != expected.version
        || manifest.platform != expected.platform
        || manifest.arch != expected.arch
        || manifest.kind != expected.kind
        || manifest.file_name != expected.file_name
        || manifest.size == 0
        || manifest.size > crate::update_download::MAX_DOWNLOAD_BYTES
        || expected.size.is_some_and(|size| size != manifest.size)
    {
        bail!("更新签名与版本、平台或安装包不匹配，已拒绝安装");
    }
    crate::update_download::validate_file_name(&manifest.file_name)?;
    let sha256: [u8; 32] = STANDARD
        .decode(manifest.sha256)
        .context("更新摘要编码无效")?
        .try_into()
        .map_err(|_| anyhow!("更新摘要长度无效"))?;
    Ok(VerifiedArtifact {
        size: manifest.size,
        sha256,
    })
}

pub async fn read_manifest(mut response: reqwest::Response) -> Result<Vec<u8>> {
    if !response.status().is_success() {
        bail!("更新签名清单不可用: HTTP {}", response.status());
    }
    if response
        .content_length()
        .is_some_and(|size| size > MAX_MANIFEST_BYTES as u64)
    {
        bail!("更新签名清单过大");
    }
    let mut bytes = Vec::new();
    while let Some(chunk) = response.chunk().await? {
        if bytes.len() + chunk.len() > MAX_MANIFEST_BYTES {
            bail!("更新签名清单过大");
        }
        bytes.extend_from_slice(&chunk);
    }
    Ok(bytes)
}

#[cfg(test)]
mod tests {
    use super::*;
    use ring::{
        rand::SystemRandom,
        signature::{Ed25519KeyPair, KeyPair},
    };
    use serde_json::{json, Value};

    #[test]
    fn update_policy_allows_manual_distribution_but_never_unsigned_execution() {
        let key = STANDARD.encode([1; 32]);
        assert!(installation_policy("manual", "", false).unwrap().is_some());
        assert!(installation_policy("signed", "", true).unwrap().is_some());
        assert!(installation_policy("signed", &key, false)
            .unwrap()
            .is_some());
        assert!(installation_policy("signed", &key, true).unwrap().is_none());
        assert!(installation_policy("signed", "invalid", true).is_err());
        assert!(installation_policy("unknown", &key, true).is_err());
    }

    fn signed(key: &Ed25519KeyPair, payload: &Value) -> Vec<u8> {
        let bytes = serde_json::to_vec(payload).unwrap();
        serde_json::to_vec(&json!({"payload":STANDARD.encode(&bytes), "signature":STANDARD.encode(key.sign(&bytes).as_ref())})).unwrap()
    }
    fn key() -> Ed25519KeyPair {
        Ed25519KeyPair::from_pkcs8(
            Ed25519KeyPair::generate_pkcs8(&SystemRandom::new())
                .unwrap()
                .as_ref(),
        )
        .unwrap()
    }
    #[test]
    fn node_ed25519_fixture_is_accepted_by_rust() {
        // Generated with Node crypto.sign; no private key is retained in this fixture.
        let key = decode_key("Y8B7L4qnuOB4gY/Mnnfx1l434VzsWiaKmfAeKYpUZ7g=").unwrap();
        let envelope = br#"{"payload":"eyJzY2hlbWEiOjEsImFwcCI6ImNjcG9weS9wcm94eS1sb2FkIiwidmVyc2lvbiI6IjI2LjkuMjAiLCJwbGF0Zm9ybSI6IndpbmRvd3MiLCJhcmNoIjoieDg2XzY0Iiwia2luZCI6IndpbmRvd3MtcG9ydGFibGUiLCJmaWxlX25hbWUiOiJ1cGRhdGVfeDY0LmV4ZSIsInNpemUiOjEyOCwic2hhMjU2IjoiQndjSEJ3Y0hCd2NIQndjSEJ3Y0hCd2NIQndjSEJ3Y0hCd2NIQndjSEJ3Yz0ifQ==","signature":"MunlwNOBNO9AEVQpRg9ljoIAPbyVmy4wrfpCWNwhSfkGf/tbIoZ3bzXHfPvA8p6N6y4qok1QcIHFtGGCQXZuAw=="}"#;
        let expected = ExpectedArtifact {
            file_name: "update_x64.exe",
            version: "26.9.20",
            platform: "windows",
            arch: "x86_64",
            kind: "windows-portable",
            size: Some(128),
        };
        assert_eq!(verify(envelope, &key, &expected).unwrap().sha256, [7; 32]);
    }

    #[test]
    fn signatures_bind_bytes_version_platform_kind_name_and_size() {
        let key = key();
        let manifest = json!({"schema":1,"app":"ccpopy/proxy-load","version":"26.9.20","platform":"windows","arch":"x86_64","kind":"windows-portable","file_name":"update_x64.exe","size":128,"sha256":STANDARD.encode([7;32])});
        let expected = ExpectedArtifact {
            file_name: "update_x64.exe",
            version: "26.9.20",
            platform: "windows",
            arch: "x86_64",
            kind: "windows-portable",
            size: Some(128),
        };
        let bytes = signed(&key, &manifest);
        assert_eq!(
            verify(&bytes, key.public_key().as_ref(), &expected)
                .unwrap()
                .sha256,
            [7; 32]
        );
        for (field, value) in [
            ("version", json!("99.1.1")),
            ("platform", json!("macos")),
            ("arch", json!("aarch64")),
            ("kind", json!("windows-nsis")),
            ("file_name", json!("other.exe")),
            ("size", json!(129)),
            ("schema", json!(2)),
            ("app", json!("attacker/app")),
        ] {
            let mut changed = manifest.clone();
            changed[field] = value;
            assert!(
                verify(
                    &signed(&key, &changed),
                    key.public_key().as_ref(),
                    &expected
                )
                .is_err(),
                "{field}"
            );
        }
        let mut envelope: Value = serde_json::from_slice(&bytes).unwrap();
        let mut changed = STANDARD
            .decode(envelope["payload"].as_str().unwrap())
            .unwrap();
        changed[10] ^= 1;
        envelope["payload"] = json!(STANDARD.encode(changed));
        assert!(verify(
            &serde_json::to_vec(&envelope).unwrap(),
            key.public_key().as_ref(),
            &expected
        )
        .is_err());
        assert!(verify(b"{}", key.public_key().as_ref(), &expected).is_err());
        assert!(verify(&bytes, &[0; 32], &expected).is_err());
        assert!(decode_key("").is_err());
    }
}
