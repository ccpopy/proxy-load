//! Portable updates replace the original executable, never launch the staging path.
use anyhow::{anyhow, bail, Context, Result};
use std::{
    ffi::OsString,
    fs,
    io::Read,
    path::{Path, PathBuf},
};

pub fn inherited_data_dir(value: Option<OsString>, original_cwd: &Path) -> Option<PathBuf> {
    value.map(PathBuf::from).map(|path| {
        if path.is_absolute() {
            path
        } else {
            original_cwd.join(path)
        }
    })
}

#[cfg(target_os = "windows")]
pub const READY_ENV: &str = "PROXY_LOAD_UPDATE_READY_FILE";

pub struct InstallPlan {
    staged: PathBuf,
    pub entry: PathBuf,
    pub backup: PathBuf,
    session: PathBuf,
    digest: [u8; 32],
}

impl InstallPlan {
    pub fn new(staged: &Path, entry: &Path, digest: [u8; 32]) -> Result<Self> {
        let entry = fs::canonicalize(entry)?;
        let root = entry.parent().ok_or_else(|| anyhow!("应用入口缺少目录"))?;
        if root
            .components()
            .any(|part| part.as_os_str() == ".proxy-load-updates")
        {
            bail!("当前应用位于临时更新目录，请先手动恢复到原便携目录");
        }
        let staged = fs::canonicalize(staged)?;
        let session = staged
            .parent()
            .ok_or_else(|| anyhow!("更新包缺少目录"))?
            .to_path_buf();
        let expected = root.join(".proxy-load-updates");
        if session.parent() != Some(fs::canonicalize(&expected)?.as_path()) || staged == entry {
            bail!("便携更新包必须位于本应用的独立 staging 目录");
        }
        if !staged.is_file() || !entry.is_file() {
            bail!("便携更新入口不是普通文件");
        }
        let nonce = session
            .file_name()
            .ok_or_else(|| anyhow!("更新目录无效"))?
            .to_string_lossy();
        let name = entry
            .file_name()
            .ok_or_else(|| anyhow!("应用入口无效"))?
            .to_string_lossy();
        let backup = root.join(format!(".{name}.rollback-{nonce}"));
        if backup.exists() {
            bail!("回滚文件已存在，拒绝覆盖");
        }
        Ok(Self {
            staged,
            entry,
            backup,
            session,
            digest,
        })
    }

    /// The launcher must confirm startup, or stop and reap the failed child
    /// before returning an error. No database file is touched by this transaction.
    pub fn install(
        &self,
        mut launch: impl FnMut(&Path, Option<&Path>) -> Result<()>,
    ) -> Result<()> {
        if file_digest(&self.staged)? != self.digest {
            bail!("待安装文件已变化，拒绝更新");
        }
        let ready = self.session.join("startup.ready");
        if ready.exists() {
            bail!("启动确认文件已存在，拒绝复用更新会话");
        }
        fs::rename(&self.entry, &self.backup).context("无法保留旧程序")?;
        if let Err(error) = fs::rename(&self.staged, &self.entry) {
            fs::rename(&self.backup, &self.entry).context("恢复旧入口失败，请使用回滚文件")?;
            return Err(error.into());
        }
        if let Err(error) = launch(&self.entry, Some(&ready)) {
            // Failed child is already stopped. Keep the failed artifact for diagnostics.
            fs::rename(&self.entry, &self.staged).context("无法移走启动失败的新程序")?;
            fs::rename(&self.backup, &self.entry).context("恢复旧程序失败，请使用回滚文件")?;
            launch(&self.entry, None).context("已恢复旧程序，但重新启动失败")?;
            return Err(error.context("新程序未完成启动，已恢复旧入口"));
        }
        let _ = fs::remove_file(ready);
        // Only remove our now-empty staging directory. Keep rollback files and all user data.
        let _ = fs::remove_dir(&self.session);
        Ok(())
    }
}

pub fn file_digest(path: &Path) -> Result<[u8; 32]> {
    let mut file = fs::File::open(path)?;
    let mut digest = ring::digest::Context::new(&ring::digest::SHA256);
    let mut chunk = [0; 64 * 1024];
    loop {
        let count = file.read(&mut chunk)?;
        if count == 0 {
            break;
        }
        digest.update(&chunk[..count]);
    }
    Ok(digest.finish().as_ref().try_into().unwrap())
}

/// Called after app setup. This is only an acknowledgement, never a data-directory override.
#[cfg(target_os = "windows")]
pub fn mark_ready() -> Result<()> {
    let Some(path) = std::env::var_os(READY_ENV) else {
        return Ok(());
    };
    let path = PathBuf::from(path);
    let entry = fs::canonicalize(std::env::current_exe()?)?;
    let root = entry.parent().ok_or_else(|| anyhow!("应用目录无效"))?;
    let parent = fs::canonicalize(path.parent().ok_or_else(|| anyhow!("启动确认路径无效"))?)?;
    if parent.parent() != Some(root.join(".proxy-load-updates").as_path())
        || path.file_name().is_none_or(|name| name != "startup.ready")
    {
        bail!("启动确认路径不属于本应用更新会话");
    }
    use std::io::Write;
    let mut ready = fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(path)?;
    write!(ready, "{}", std::process::id())?;
    ready.sync_all()?;
    Ok(())
}

#[cfg(target_os = "windows")]
pub fn launch_and_confirm(entry: &Path, ready: Option<&Path>) -> Result<()> {
    use std::{
        os::windows::process::CommandExt,
        process::Command,
        time::{Duration, Instant},
    };
    let mut command = Command::new(entry);
    command
        .current_dir(entry.parent().unwrap())
        .creation_flags(0x08000000);
    // DATA_DIR is deliberately inherited unchanged, including paths outside the app.
    command.env_remove(READY_ENV);
    if let Some(path) = ready {
        command.env(READY_ENV, path);
    }
    let mut child = command.spawn()?;
    let Some(ready) = ready else {
        return Ok(());
    };
    let start = Instant::now();
    let outcome = loop {
        match child.try_wait() {
            Ok(Some(status)) => break Err(anyhow!("新程序提前退出: {status}")),
            Err(error) => break Err(error.into()),
            Ok(None) => {}
        }
        if fs::read_to_string(ready).is_ok_and(|pid| pid == child.id().to_string()) {
            break Ok(());
        }
        if start.elapsed() >= Duration::from_secs(30) {
            break Err(anyhow!("新程序启动确认超时"));
        }
        std::thread::sleep(Duration::from_millis(50));
    };
    if outcome.is_err() {
        let _ = child.kill();
        child.wait().context("等待失败的新进程退出")?;
    }
    outcome
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn explicit_data_dir_is_preserved_when_helper_changes_working_directory() {
        let root = std::env::temp_dir();
        assert_eq!(inherited_data_dir(None, &root), None);
        assert_eq!(
            inherited_data_dir(Some("数据目录".into()), &root),
            Some(root.join("数据目录"))
        );
        let absolute = root.join("external data");
        assert_eq!(
            inherited_data_dir(Some(absolute.clone().into_os_string()), &root),
            Some(absolute)
        );
    }
    static SEQ: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
    fn directory() -> PathBuf {
        let root = std::env::temp_dir().join(format!(
            "proxy portable 中文 {} {}",
            std::process::id(),
            SEQ.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
        ));
        fs::create_dir(&root).unwrap();
        root
    }
    fn staged(root: &Path, version: &str) -> PathBuf {
        let session = root.join(".proxy-load-updates").join(version);
        fs::create_dir_all(&session).unwrap();
        let path = session.join("download.exe");
        fs::write(&path, version).unwrap();
        path
    }
    #[test]
    fn portable_two_updates_keep_original_entry_and_data_and_rollback_on_start_failure() {
        let root = directory();
        let entry = root.join("原入口 portable.exe");
        fs::write(&entry, b"v1").unwrap();
        fs::create_dir(root.join("data")).unwrap();
        let data = root.join("data/proxy.db");
        fs::write(&data, b"existing proxies/groups/DNS/auth/logs").unwrap();
        for version in ["v2", "v3"] {
            let source = staged(&root, version);
            let plan = InstallPlan::new(&source, &entry, file_digest(&source).unwrap()).unwrap();
            plan.install(|path, ready| {
                assert_eq!(path, fs::canonicalize(&entry)?);
                assert_eq!(fs::read(path)?, version.as_bytes());
                assert!(ready.is_some());
                Ok(())
            })
            .unwrap();
            assert!(plan.backup.exists());
            assert!(!source.parent().unwrap().exists());
            assert_eq!(
                fs::read(&data).unwrap(),
                b"existing proxies/groups/DNS/auth/logs"
            );
        }
        let source = staged(&root, "bad");
        let plan = InstallPlan::new(&source, &entry, file_digest(&source).unwrap()).unwrap();
        let mut launches = 0;
        assert!(plan
            .install(|path, ready| {
                launches += 1;
                if ready.is_some() {
                    bail!("injected startup failure");
                }
                assert_eq!(fs::read(path)?, b"v3");
                Ok(())
            })
            .is_err());
        assert_eq!(launches, 2);
        assert_eq!(fs::read(&entry).unwrap(), b"v3");
        assert_eq!(
            fs::read(&data).unwrap(),
            b"existing proxies/groups/DNS/auth/logs"
        );
        // Only the uniquely-created test directory, never a supplied/user path.
        fs::remove_dir_all(root).unwrap();
    }
    #[test]
    fn portable_rejects_tampered_staging_and_paths_outside_update_root() {
        let root = directory();
        let entry = root.join("portable.exe");
        fs::write(&entry, b"v1").unwrap();
        let source = staged(&root, "v2");
        let digest = file_digest(&source).unwrap();
        let plan = InstallPlan::new(&source, &entry, digest).unwrap();
        fs::write(&source, b"tampered").unwrap();
        assert!(plan.install(|_, _| panic!("must not launch")).is_err());
        assert_eq!(fs::read(&entry).unwrap(), b"v1");
        assert!(InstallPlan::new(&entry, &source, digest).is_err());
        assert!(InstallPlan::new(&entry, &entry, digest).is_err());
        fs::remove_dir_all(root).unwrap();
    }
}
