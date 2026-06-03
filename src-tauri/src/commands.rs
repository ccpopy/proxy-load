use std::{
    collections::HashMap,
    fs,
    path::{Path, PathBuf},
    process::Command,
    sync::Arc,
};

use anyhow::anyhow;
use serde::Serialize;
use serde_json::{json, Map, Value};

use crate::{
    models::{DnsInput, ProxyGroupInput, ProxyInput},
    proxy_tester,
    state::AppState,
    version,
};

type CommandResult<T> = Result<T, CommandError>;

#[derive(Debug, Serialize)]
pub struct CommandError {
    message: String,
}

impl CommandError {
    fn new(message: impl Into<String>) -> Self {
        Self {
            message: message.into(),
        }
    }
}

impl From<anyhow::Error> for CommandError {
    fn from(error: anyhow::Error) -> Self {
        Self::new(error.to_string())
    }
}

impl From<serde_json::Error> for CommandError {
    fn from(error: serde_json::Error) -> Self {
        Self::new(error.to_string())
    }
}

impl From<rusqlite::Error> for CommandError {
    fn from(error: rusqlite::Error) -> Self {
        Self::new(error.to_string())
    }
}

impl From<std::io::Error> for CommandError {
    fn from(error: std::io::Error) -> Self {
        Self::new(error.to_string())
    }
}

impl From<std::num::ParseIntError> for CommandError {
    fn from(error: std::num::ParseIntError) -> Self {
        Self::new(error.to_string())
    }
}

impl From<url::ParseError> for CommandError {
    fn from(error: url::ParseError) -> Self {
        Self::new(error.to_string())
    }
}

#[tauri::command]
pub async fn list_proxies(state: tauri::State<'_, Arc<AppState>>) -> CommandResult<Value> {
    let state = state.inner().clone();
    let mut proxies = state.db.list_proxies()?;
    let mode = state
        .db
        .settings_map()?
        .get("load_mode")
        .cloned()
        .unwrap_or_else(|| "auto".to_string());

    if mode == "auto" {
        let stats = state.proxy_runtime.stats().await;
        for proxy in &mut proxies {
            if let Some(stat) = stats
                .iter()
                .find(|item| item.get("proxyId").and_then(Value::as_i64) == Some(proxy.id))
            {
                proxy.score = stat.get("weight").and_then(Value::as_f64);
                proxy.active_connections = stat.get("activeConnections").and_then(Value::as_i64);
            }
        }
    }

    Ok(serde_json::to_value(proxies)?)
}

#[tauri::command]
pub fn get_proxy(state: tauri::State<'_, Arc<AppState>>, id: i64) -> CommandResult<Value> {
    let proxy = state
        .db
        .get_proxy(id)?
        .ok_or_else(|| CommandError::new("代理不存在"))?;
    Ok(serde_json::to_value(proxy)?)
}

#[tauri::command]
pub fn create_proxy(
    state: tauri::State<'_, Arc<AppState>>,
    input: ProxyInput,
) -> CommandResult<Value> {
    let proxy = state.db.create_proxy(input)?;
    state.emit("proxy_created", serde_json::to_value(&proxy)?);
    Ok(serde_json::to_value(proxy)?)
}

#[tauri::command]
pub fn update_proxy(
    state: tauri::State<'_, Arc<AppState>>,
    id: i64,
    input: ProxyInput,
) -> CommandResult<Value> {
    let proxy = state.db.update_proxy(id, input)?;
    state.emit("proxy_updated", serde_json::to_value(&proxy)?);
    Ok(serde_json::to_value(proxy)?)
}

#[tauri::command]
pub fn delete_proxy(state: tauri::State<'_, Arc<AppState>>, id: i64) -> CommandResult<Value> {
    state.db.delete_proxy(id)?;
    state.emit("proxy_deleted", json!({ "id": id }));
    Ok(json!({ "message": "代理已删除" }))
}

#[tauri::command]
pub fn update_proxy_priority(
    state: tauri::State<'_, Arc<AppState>>,
    id: i64,
    priority: i64,
) -> CommandResult<Value> {
    state.db.update_proxy_priority(id, priority)?;
    Ok(json!({ "message": "优先级已更新" }))
}

#[tauri::command]
pub fn update_proxy_priorities(
    state: tauri::State<'_, Arc<AppState>>,
    priorities: HashMap<String, i64>,
) -> CommandResult<Value> {
    for (id, priority) in priorities {
        let id = id.parse::<i64>()?;
        state.db.update_proxy_priority(id, priority)?;
    }
    Ok(json!({ "message": "优先级批量更新成功" }))
}

#[tauri::command]
pub async fn test_proxy(state: tauri::State<'_, Arc<AppState>>, id: i64) -> CommandResult<Value> {
    let state = state.inner().clone();
    let proxy = state
        .db
        .get_proxy(id)?
        .ok_or_else(|| CommandError::new("代理不存在"))?;
    let settings = state.db.settings_map()?;
    let global_url = settings
        .get("test_url")
        .cloned()
        .unwrap_or_else(|| "https://cms.zjzwfw.gov.cn/favicon.ico".to_string());
    let global_timeout = settings
        .get("timeout")
        .and_then(|value| value.parse::<u64>().ok())
        .unwrap_or(10)
        * 1000;
    let test_url = proxy.test_url.clone().unwrap_or(global_url);
    let timeout = proxy
        .test_timeout
        .and_then(|value| u64::try_from(value).ok())
        .map(|value| value * 1000)
        .unwrap_or(global_timeout);

    state
        .db
        .update_proxy_status(proxy.id, "testing", None, 0, 0)?;
    state.emit("proxy_testing", json!({ "id": proxy.id }));

    let result = proxy_tester::test_proxy(&proxy, &test_url, timeout).await;
    let target = url::Url::parse(&test_url).map_err(|error| anyhow!("测试地址无效: {error}"))?;
    let target_host = target.host_str().unwrap_or_default().to_string();
    let target_port = target.port_or_known_default().unwrap_or(80);

    if result.success {
        state
            .db
            .update_proxy_status(proxy.id, "active", Some(result.response_time), 1, 0)?;
        state.db.log_request(
            Some(proxy.id),
            &target_host,
            i64::from(target_port),
            true,
            Some(result.response_time),
            None,
            "health_success",
        )?;
    } else {
        state
            .db
            .update_proxy_status(proxy.id, "inactive", None, 0, 1)?;
        state.db.log_request(
            Some(proxy.id),
            &target_host,
            i64::from(target_port),
            false,
            None,
            result.error.as_deref(),
            "health_failure",
        )?;
    }

    let updated = state.db.get_proxy(proxy.id)?;
    state.emit(
        "proxy_tested",
        json!({
            "proxy": updated,
            "result": result
        }),
    );
    Ok(serde_json::to_value(result)?)
}

#[tauri::command]
pub fn list_proxy_groups(state: tauri::State<'_, Arc<AppState>>) -> CommandResult<Value> {
    Ok(serde_json::to_value(state.db.list_proxy_groups()?)?)
}

#[tauri::command]
pub fn create_proxy_group(
    state: tauri::State<'_, Arc<AppState>>,
    input: ProxyGroupInput,
) -> CommandResult<Value> {
    let group = state.db.create_proxy_group(input)?;
    state.emit("proxy_group_created", serde_json::to_value(&group)?);
    Ok(serde_json::to_value(group)?)
}

#[tauri::command]
pub fn update_proxy_group(
    state: tauri::State<'_, Arc<AppState>>,
    id: i64,
    input: ProxyGroupInput,
) -> CommandResult<Value> {
    let group = state.db.update_proxy_group(id, input)?;
    state.emit("proxy_group_updated", serde_json::to_value(&group)?);
    Ok(serde_json::to_value(group)?)
}

#[tauri::command]
pub fn delete_proxy_group(state: tauri::State<'_, Arc<AppState>>, id: i64) -> CommandResult<Value> {
    state.db.delete_proxy_group(id)?;
    state.emit("proxy_group_deleted", json!({ "id": id }));
    Ok(json!({ "message": "分组已删除" }))
}

#[tauri::command]
pub fn get_settings(state: tauri::State<'_, Arc<AppState>>) -> CommandResult<Value> {
    Ok(serde_json::to_value(state.db.settings_map()?)?)
}

#[tauri::command]
pub fn save_settings(
    state: tauri::State<'_, Arc<AppState>>,
    settings: Map<String, Value>,
) -> CommandResult<Value> {
    let mut normalized = Map::new();
    for (key, value) in settings {
        if key == "algorithm" {
            let value = value.as_str().unwrap_or("adaptive");
            let normalized_value = match value {
                "weighted_round_robin" | "least_connections" | "adaptive" | "sticky_host" => value,
                _ => "adaptive",
            };
            normalized.insert(key, Value::String(normalized_value.to_string()));
        } else {
            normalized.insert(key, value);
        }
    }
    state.db.save_settings(&normalized)?;
    Ok(json!({ "message": "设置已保存" }))
}

#[tauri::command]
pub fn get_advanced_config(state: tauri::State<'_, Arc<AppState>>) -> CommandResult<Value> {
    Ok(state.db.load_advanced_config()?)
}

#[tauri::command]
pub fn save_advanced_config(
    state: tauri::State<'_, Arc<AppState>>,
    config: Map<String, Value>,
) -> CommandResult<Value> {
    let current = state.db.load_advanced_config()?;
    let current_port = current
        .get("proxy_port")
        .and_then(Value::as_i64)
        .unwrap_or(5678);
    let next_port = config
        .get("proxy_port")
        .and_then(Value::as_i64)
        .unwrap_or(current_port);

    state.db.save_settings(&config)?;
    Ok(json!({
        "success": true,
        "requiresRestart": current_port != next_port,
        "message": if current_port != next_port { "部分配置需要重启服务才能生效" } else { "配置已应用" }
    }))
}

#[tauri::command]
pub fn reset_advanced_config(state: tauri::State<'_, Arc<AppState>>) -> CommandResult<Value> {
    state.db.reset_advanced_config()?;
    Ok(json!({ "success": true, "message": "已恢复默认配置" }))
}

#[tauri::command]
pub fn export_config(state: tauri::State<'_, Arc<AppState>>) -> CommandResult<Value> {
    Ok(state.db.exported_config()?)
}

#[tauri::command]
pub fn stats_overview(state: tauri::State<'_, Arc<AppState>>) -> CommandResult<Value> {
    Ok(state.db.overview(state.uptime_seconds())?)
}

#[tauri::command]
pub fn stats_hourly(state: tauri::State<'_, Arc<AppState>>) -> CommandResult<Value> {
    Ok(state.db.scalar_json(
        r#"
        SELECT
          strftime('%Y-%m-%d %H:00', created_at) as hour,
          COUNT(*) as total_requests,
          COALESCE(SUM(CASE WHEN success = 1 THEN 1 ELSE 0 END), 0) as success_requests,
          COALESCE(SUM(CASE WHEN success = 0 THEN 1 ELSE 0 END), 0) as failed_requests,
          AVG(CASE WHEN success = 1 THEN response_time END) as avg_response_time
        FROM request_logs
        WHERE created_at >= datetime('now', '-24 hours')
        GROUP BY hour
        ORDER BY hour DESC
        "#,
    )?)
}

#[tauri::command]
pub fn stats_proxy_usage(state: tauri::State<'_, Arc<AppState>>) -> CommandResult<Value> {
    Ok(state.db.scalar_json(
        r#"
        SELECT
          p.id,
          p.name,
          p.type,
          COUNT(rl.id) as total_requests,
          COALESCE(SUM(CASE WHEN rl.success = 1 THEN 1 ELSE 0 END), 0) as success_requests
        FROM proxies p
        LEFT JOIN request_logs rl ON p.id = rl.proxy_id
          AND rl.created_at >= datetime('now', '-24 hours')
        GROUP BY p.id
        ORDER BY total_requests DESC
        "#,
    )?)
}

#[tauri::command]
pub fn stats_targets(state: tauri::State<'_, Arc<AppState>>) -> CommandResult<Value> {
    Ok(state.db.scalar_json(
        r#"
        SELECT
          target_host,
          COUNT(*) as request_count,
          COALESCE(SUM(CASE WHEN success = 1 THEN 1 ELSE 0 END), 0) as success_count,
          AVG(CASE WHEN success = 1 THEN response_time END) as avg_response_time
        FROM request_logs
        WHERE created_at >= datetime('now', '-24 hours')
        GROUP BY target_host
        ORDER BY request_count DESC
        LIMIT 20
        "#,
    )?)
}

#[tauri::command]
pub fn stats_failed_targets(state: tauri::State<'_, Arc<AppState>>) -> CommandResult<Value> {
    Ok(state.db.scalar_json(
        r#"
        SELECT
          target_host || ':' || target_port as target,
          COUNT(*) as fail_count,
          MAX(created_at) as last_fail_time
        FROM request_logs
        WHERE success = 0 AND created_at >= datetime('now', '-24 hours')
        GROUP BY target_host, target_port
        ORDER BY fail_count DESC
        LIMIT 10
        "#,
    )?)
}

#[tauri::command]
pub async fn stats_circuit_breakers(
    state: tauri::State<'_, Arc<AppState>>,
) -> CommandResult<Value> {
    let state = state.inner().clone();
    Ok(Value::Array(
        state.proxy_runtime.circuit_breaker_stats().await,
    ))
}

#[tauri::command]
pub async fn stats_connection_pools(
    state: tauri::State<'_, Arc<AppState>>,
) -> CommandResult<Value> {
    let state = state.inner().clone();
    Ok(Value::Array(
        state.proxy_runtime.connection_pool_stats().await,
    ))
}

#[tauri::command]
pub fn list_dns_mappings(state: tauri::State<'_, Arc<AppState>>) -> CommandResult<Value> {
    Ok(serde_json::to_value(state.db.list_dns_mappings()?)?)
}

#[tauri::command]
pub async fn create_dns_mapping(
    state: tauri::State<'_, Arc<AppState>>,
    input: DnsInput,
) -> CommandResult<Value> {
    let state = state.inner().clone();
    let mapping = state.db.create_dns_mapping(input)?;
    state.proxy_runtime.refresh_dns_cache().await?;
    state.emit("dns_mapping_added", serde_json::to_value(&mapping)?);
    Ok(serde_json::to_value(mapping)?)
}

#[tauri::command]
pub async fn update_dns_mapping(
    state: tauri::State<'_, Arc<AppState>>,
    id: i64,
    input: DnsInput,
) -> CommandResult<Value> {
    let state = state.inner().clone();
    let mapping = state.db.update_dns_mapping(id, input)?;
    state.proxy_runtime.refresh_dns_cache().await?;
    state.emit("dns_mapping_updated", serde_json::to_value(&mapping)?);
    Ok(json!({ "success": true }))
}

#[tauri::command]
pub async fn delete_dns_mapping(
    state: tauri::State<'_, Arc<AppState>>,
    id: i64,
) -> CommandResult<Value> {
    let state = state.inner().clone();
    state.db.delete_dns_mapping(id)?;
    state.proxy_runtime.refresh_dns_cache().await?;
    state.emit("dns_mapping_deleted", json!({ "id": id }));
    Ok(json!({ "success": true }))
}

#[tauri::command]
pub async fn toggle_dns_mapping(
    state: tauri::State<'_, Arc<AppState>>,
    id: i64,
) -> CommandResult<Value> {
    let state = state.inner().clone();
    let enabled = state.db.toggle_dns_mapping(id)?;
    state.proxy_runtime.refresh_dns_cache().await?;
    state.emit(
        "dns_mapping_toggled",
        json!({ "id": id, "enabled": enabled }),
    );
    Ok(json!({ "success": true, "enabled": enabled }))
}

#[tauri::command]
pub fn test_urls(state: tauri::State<'_, Arc<AppState>>) -> CommandResult<Value> {
    let settings = state.db.settings_map()?;
    let mut urls = Vec::new();
    if let Some(url) = settings.get("test_url").filter(|value| !value.is_empty()) {
        urls.push(Value::String(url.clone()));
    }
    for proxy in state.db.list_proxies()? {
        if let Some(url) = proxy.test_url.filter(|value| !value.is_empty()) {
            if !urls.iter().any(|item| item.as_str() == Some(url.as_str())) {
                urls.push(Value::String(url));
            }
        }
    }
    Ok(Value::Array(urls))
}

#[tauri::command]
pub fn traffic_logs(
    state: tauri::State<'_, Arc<AppState>>,
    page: Option<i64>,
    page_size: Option<i64>,
) -> CommandResult<Value> {
    let page = ensure_positive(page.unwrap_or(1), "page")?;
    let page_size = ensure_positive(page_size.unwrap_or(50), "pageSize")?;
    let (items, total) = state.db.traffic_logs(page, page_size)?;
    let total_pages = (total as f64 / page_size as f64).ceil().max(1.0) as i64;
    Ok(json!({
        "items": items,
        "page": page,
        "pageSize": page_size,
        "total": total,
        "totalPages": total_pages
    }))
}

#[tauri::command]
pub fn clear_traffic_logs(state: tauri::State<'_, Arc<AppState>>) -> CommandResult<Value> {
    let deleted = state.db.clear_traffic_logs()?;
    state.emit("traffic_logs_cleared", json!({ "deleted": deleted }));
    Ok(json!({ "deleted": deleted }))
}

#[tauri::command]
pub fn version_info() -> CommandResult<Value> {
    Ok(version::version_info())
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct UpdateArtifact {
    file_name: String,
    path: String,
    version: String,
    kind: String,
    is_newer: bool,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct UpdateInfo {
    current_version: String,
    app_dir: String,
    release_dir: String,
    has_update: bool,
    latest: Option<UpdateArtifact>,
    artifacts: Vec<UpdateArtifact>,
}

#[tauri::command]
pub fn check_for_updates() -> CommandResult<UpdateInfo> {
    build_update_info()
}

#[tauri::command]
pub fn install_update(artifact_path: Option<String>) -> CommandResult<Value> {
    let info = build_update_info()?;
    let app_dir = PathBuf::from(&info.app_dir);
    let release_dir = PathBuf::from(&info.release_dir);
    let selected_path = artifact_path
        .map(PathBuf::from)
        .or_else(|| info.latest.map(|artifact| PathBuf::from(artifact.path)))
        .ok_or_else(|| CommandError::new("没有可安装的更新包"))?;
    let selected_path = selected_path.canonicalize()?;
    let release_dir = release_dir.canonicalize()?;

    if !selected_path.starts_with(&release_dir) {
        return Err(CommandError::new("更新包必须位于 release 目录内"));
    }

    let extension = selected_path
        .extension()
        .and_then(|value| value.to_str())
        .unwrap_or_default()
        .to_ascii_lowercase();
    let file_name = selected_path
        .file_name()
        .and_then(|value| value.to_str())
        .unwrap_or_default()
        .to_ascii_lowercase();

    if extension == "exe" && file_name.contains("portable") {
        return Err(CommandError::new(
            "便携 exe 只用于本机验证，不能在应用运行中直接覆盖当前程序。请使用 setup/msi 更新包，或关闭应用后手动替换 exe。",
        ));
    }

    match extension.as_str() {
        "exe" => {
            Command::new(&selected_path)
                .arg(format!("/D={}", app_dir.display()))
                .current_dir(&app_dir)
                .spawn()?;
        }
        "msi" => {
            Command::new("msiexec")
                .arg("/i")
                .arg(&selected_path)
                .arg(format!("TARGETDIR={}", app_dir.display()))
                .current_dir(&app_dir)
                .spawn()?;
        }
        _ => {
            return Err(CommandError::new(format!(
                "当前平台暂不支持直接安装 {} 更新包",
                selected_path.display()
            )));
        }
    }

    Ok(json!({
        "success": true,
        "installDir": app_dir,
        "artifactPath": selected_path,
        "message": "已启动更新安装程序，安装目录已指向当前应用所在目录"
    }))
}

fn ensure_positive(value: i64, field: &str) -> CommandResult<i64> {
    if value < 1 {
        Err(CommandError::new(format!("{field}必须是正整数")))
    } else {
        Ok(value)
    }
}

fn build_update_info() -> CommandResult<UpdateInfo> {
    let app_dir = current_app_dir()?;
    let release_dir = release_dir(&app_dir);
    fs::create_dir_all(&release_dir)?;

    let current_version = version::VERSION.to_string();
    let current = VersionParts::parse(version::VERSION)
        .ok_or_else(|| CommandError::new("当前版本号格式无效"))?;
    let mut artifacts = scan_update_artifacts(&release_dir, current)?;
    artifacts.sort_by(|left, right| compare_artifacts(right, left));
    let latest = artifacts.iter().find(|artifact| artifact.is_newer).cloned();

    Ok(UpdateInfo {
        current_version,
        app_dir: app_dir.display().to_string(),
        release_dir: release_dir.display().to_string(),
        has_update: latest.is_some(),
        latest,
        artifacts,
    })
}

fn current_app_dir() -> CommandResult<PathBuf> {
    let executable = std::env::current_exe()?;
    executable
        .parent()
        .map(Path::to_path_buf)
        .ok_or_else(|| CommandError::new("无法确定当前应用目录"))
}

fn release_dir(app_dir: &Path) -> PathBuf {
    if cfg!(debug_assertions) {
        PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .parent()
            .map(Path::to_path_buf)
            .unwrap_or_else(|| app_dir.to_path_buf())
            .join("release")
    } else {
        app_dir.join("release")
    }
}

fn scan_update_artifacts(
    release_dir: &Path,
    current: VersionParts,
) -> CommandResult<Vec<UpdateArtifact>> {
    let mut artifacts = Vec::new();
    for entry in fs::read_dir(release_dir)? {
        let entry = entry?;
        let metadata = entry.metadata()?;
        if !metadata.is_file() {
            continue;
        }

        let path = entry.path();
        let Some(kind) = artifact_kind(&path) else {
            continue;
        };
        let file_name = entry.file_name().to_string_lossy().to_string();
        let Some((version, parsed)) = extract_version(&file_name) else {
            continue;
        };

        artifacts.push(UpdateArtifact {
            file_name,
            path: path.display().to_string(),
            version,
            kind: kind.to_string(),
            is_newer: parsed > current,
        });
    }
    Ok(artifacts)
}

fn artifact_kind(path: &Path) -> Option<&'static str> {
    let file_name = path.file_name()?.to_string_lossy().to_ascii_lowercase();
    let extension = path.extension()?.to_string_lossy().to_ascii_lowercase();

    match extension.as_str() {
        "exe" if file_name.contains("portable") => Some("windows-portable"),
        "exe" if file_name.contains("setup") => Some("windows-nsis"),
        "exe" => Some("windows-exe"),
        "msi" => Some("windows-msi"),
        "dmg" => Some("macos-dmg"),
        "deb" => Some("linux-deb"),
        "rpm" => Some("linux-rpm"),
        "appimage" => Some("linux-appimage"),
        _ => None,
    }
}

fn compare_artifacts(left: &UpdateArtifact, right: &UpdateArtifact) -> std::cmp::Ordering {
    let left_version = VersionParts::parse(&left.version);
    let right_version = VersionParts::parse(&right.version);
    left_version
        .cmp(&right_version)
        .then_with(|| artifact_priority(left).cmp(&artifact_priority(right)))
        .then_with(|| left.file_name.cmp(&right.file_name))
}

fn artifact_priority(artifact: &UpdateArtifact) -> i32 {
    match artifact.kind.as_str() {
        "windows-nsis" => 40,
        "windows-msi" => 30,
        "windows-exe" => 20,
        "windows-portable" => 15,
        _ => 10,
    }
}

fn extract_version(file_name: &str) -> Option<(String, VersionParts)> {
    let chars: Vec<char> = file_name.chars().collect();
    for start in 0..chars.len() {
        if !chars[start].is_ascii_digit() {
            continue;
        }

        let mut end = start;
        while end < chars.len() && (chars[end].is_ascii_digit() || chars[end] == '.') {
            end += 1;
        }

        let candidate: String = chars[start..end].iter().collect();
        if let Some(version) = VersionParts::parse(&candidate) {
            return Some((candidate, version));
        }
    }
    None
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
struct VersionParts {
    major: u64,
    minor: u64,
    patch: u64,
}

impl VersionParts {
    fn parse(value: &str) -> Option<Self> {
        let core = value.split_once('-').map_or(value, |(core, _)| core);
        let mut parts = core.split('.');
        let major = parts.next()?.parse().ok()?;
        let minor = parts.next()?.parse().ok()?;
        let patch = parts.next()?.parse().ok()?;
        if parts.next().is_some() {
            return None;
        }
        Some(Self {
            major,
            minor,
            patch,
        })
    }
}
