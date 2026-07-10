use std::{
    collections::{HashMap, HashSet},
    fs,
    path::{Path, PathBuf},
    process::Command,
    sync::Arc,
    time::Duration,
};

#[cfg(target_os = "windows")]
use anyhow::{anyhow, Context, Result as AnyhowResult};
#[cfg(target_os = "windows")]
use std::os::windows::process::CommandExt;
#[cfg(target_os = "windows")]
use std::{
    ffi::OsString,
    time::{SystemTime, UNIX_EPOCH},
};

use serde::{Deserialize, Serialize};
use serde_json::{json, Map, Value};
use tauri::AppHandle;

use crate::{
    database::default_advanced_config,
    models::{ConfigBundle, DnsInput, ProxyGroupInput, ProxyInput, CONFIG_BUNDLE_KIND},
    proxy::ProxyServiceStatus,
    state::AppState,
    version,
};

const GITHUB_LATEST_RELEASE_URL: &str =
    "https://api.github.com/repos/ccpopy/proxy-load/releases/latest";
const GITHUB_RELEASES_URL: &str = "https://github.com/ccpopy/proxy-load/releases";
// gh-proxy（hunshcn/gh-proxy）镜像，仅支持 github.com 页面与资产下载，不支持 api.github.com
const GH_PROXY_BASE: &str = "https://gh.lessdo.top/";
const GITHUB_TOKEN_ENV: &str = "PROXY_LOAD_GITHUB_TOKEN";
#[cfg(target_os = "windows")]
const CREATE_NO_WINDOW: u32 = 0x08000000;
#[cfg(target_os = "windows")]
const UPDATE_EXIT_DELAY: Duration = Duration::from_millis(800);
#[cfg(target_os = "windows")]
const UPDATE_HELPER_ARG: &str = "--proxy-load-update-helper";
#[cfg(target_os = "windows")]
const UPDATE_HELPER_PARENT_PID_ARG: &str = "--parent-pid";
#[cfg(target_os = "windows")]
const UPDATE_HELPER_INSTALLER_PATH_ARG: &str = "--installer-path";
#[cfg(target_os = "windows")]
const UPDATE_HELPER_INSTALLER_KIND_ARG: &str = "--installer-kind";
#[cfg(target_os = "windows")]
const UPDATE_HELPER_INSTALL_DIR_ARG: &str = "--install-dir";
#[cfg(target_os = "windows")]
const UPDATE_HELPER_LAUNCH_PATH_ARG: &str = "--launch-path";

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

impl From<reqwest::Error> for CommandError {
    fn from(error: reqwest::Error) -> Self {
        Self::new(error.to_string())
    }
}

#[tauri::command]
pub async fn list_proxies(state: tauri::State<'_, Arc<AppState>>) -> CommandResult<Value> {
    let state = state.inner().clone();
    let mut proxies = state.db.list_proxies()?;
    let stats = state.proxy_runtime.stats().await;
    for proxy in &mut proxies {
        if let Some(stat) = stats.get(&proxy.id) {
            proxy.score = Some(stat.score);
            proxy.active_connections = Some(stat.active_connections);
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
    state.notify_proxy_config_changed();
    state.emit("proxy_created", serde_json::to_value(&proxy)?);
    Ok(serde_json::to_value(proxy)?)
}

#[tauri::command]
pub async fn update_proxy(
    state: tauri::State<'_, Arc<AppState>>,
    id: i64,
    input: ProxyInput,
) -> CommandResult<Value> {
    let _status_guard = state.proxy_runtime.lock_proxy_status(id).await;
    let (proxy, connection_changed) = state.db.update_proxy(id, input)?;
    if connection_changed {
        state.proxy_configuration_changed(id).await;
    }
    state.emit("proxy_updated", serde_json::to_value(&proxy)?);
    Ok(serde_json::to_value(proxy)?)
}

#[tauri::command]
pub async fn delete_proxy(state: tauri::State<'_, Arc<AppState>>, id: i64) -> CommandResult<Value> {
    let _status_guard = state.proxy_runtime.lock_proxy_status(id).await;
    state.db.delete_proxy(id)?;
    state.proxy_deleted(id).await;
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
    let priorities = priorities
        .into_iter()
        .map(|(id, priority)| Ok((id.parse::<i64>()?, priority)))
        .collect::<CommandResult<Vec<_>>>()?;
    state.db.update_proxy_priorities(&priorities)?;
    Ok(json!({ "message": "优先级批量更新成功" }))
}

#[tauri::command]
pub async fn test_proxy(state: tauri::State<'_, Arc<AppState>>, id: i64) -> CommandResult<Value> {
    let result = state.inner().test_proxy_by_id(id).await?;
    Ok(serde_json::to_value(result)?)
}

#[tauri::command]
pub async fn proxy_service_status(
    state: tauri::State<'_, Arc<AppState>>,
) -> CommandResult<ProxyServiceStatus> {
    let state = state.inner().clone();
    Ok(state.proxy_runtime.service_status().await)
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
    let mut settings = state.db.settings_map()?;
    settings.retain(|key, _| matches!(key.as_str(), "algorithm" | "test_url" | "timeout"));
    Ok(serde_json::to_value(settings)?)
}

#[tauri::command]
pub async fn save_settings(
    state: tauri::State<'_, Arc<AppState>>,
    settings: Map<String, Value>,
) -> CommandResult<Value> {
    let normalized = normalize_load_settings(settings)?;
    let _settings_guard = state.settings_update_guard().await;
    state.db.save_settings(&normalized)?;
    state
        .proxy_runtime
        .update_load_settings(&normalized)
        .await?;
    state.notify_proxy_config_changed();
    Ok(json!({ "message": "设置已保存" }))
}

#[tauri::command]
pub fn get_advanced_config(state: tauri::State<'_, Arc<AppState>>) -> CommandResult<Value> {
    Ok(state.db.load_advanced_config()?)
}

#[tauri::command]
pub async fn save_advanced_config(
    state: tauri::State<'_, Arc<AppState>>,
    config: Map<String, Value>,
) -> CommandResult<Value> {
    let _settings_guard = state.settings_update_guard().await;
    let current = state.db.load_advanced_config()?;
    let normalized = normalize_advanced_config(&current, config)?;
    let next = Value::Object(normalized.clone());
    let requires_restart = restart_required(&current, &next);

    state.db.save_settings(&normalized)?;
    state.proxy_runtime.update_advanced_config(&next).await?;
    state.notify_advanced_config_changed();
    Ok(json!({
        "success": true,
        "requiresRestart": requires_restart,
        "message": if requires_restart { "设置已保存；监听地址或端口将在重启应用后生效" } else { "配置已应用" }
    }))
}

#[tauri::command]
pub async fn reset_advanced_config(state: tauri::State<'_, Arc<AppState>>) -> CommandResult<Value> {
    let _settings_guard = state.settings_update_guard().await;
    let current = state.db.load_advanced_config()?;
    state.db.reset_advanced_config()?;
    let next = state.db.load_advanced_config()?;
    state.proxy_runtime.update_advanced_config(&next).await?;
    state.notify_advanced_config_changed();
    let requires_restart = restart_required(&current, &next);
    Ok(json!({
        "success": true,
        "requiresRestart": requires_restart,
        "message": if requires_restart { "已恢复默认配置；监听地址或端口将在重启应用后生效" } else { "已恢复默认配置" }
    }))
}

fn normalize_load_settings(settings: Map<String, Value>) -> CommandResult<Map<String, Value>> {
    const ALLOWED_KEYS: &[&str] = &["algorithm", "test_url", "timeout"];
    let mut normalized = Map::new();
    for (key, value) in settings {
        if !ALLOWED_KEYS.contains(&key.as_str()) {
            return Err(CommandError::new(format!("不支持的负载设置项: {key}")));
        }
        match key.as_str() {
            "algorithm" => {
                let algorithm = value
                    .as_str()
                    .ok_or_else(|| CommandError::new("代理选择算法必须是字符串"))?;
                let algorithm = match algorithm {
                    "adaptive" | "round_robin" | "least_connections" | "sticky_host" => algorithm,
                    "weighted_round_robin" => "round_robin",
                    _ => return Err(CommandError::new("不支持的代理选择算法")),
                };
                normalized.insert(key, Value::String(algorithm.to_string()));
            }
            "test_url" => {
                let test_url = value
                    .as_str()
                    .map(str::trim)
                    .ok_or_else(|| CommandError::new("默认测试地址必须是字符串"))?;
                validate_test_url(test_url)?;
                normalized.insert(key, Value::String(test_url.to_string()));
            }
            "timeout" => {
                let timeout = value
                    .as_str()
                    .and_then(|value| value.parse::<i64>().ok())
                    .or_else(|| value.as_i64())
                    .ok_or_else(|| CommandError::new("默认测试超时必须是整数"))?;
                if !(1..=300).contains(&timeout) {
                    return Err(CommandError::new("默认测试超时必须在 1 到 300 秒之间"));
                }
                normalized.insert(key, Value::String(timeout.to_string()));
            }
            _ => unreachable!(),
        }
    }
    Ok(normalized)
}

fn normalize_advanced_config(
    current: &Value,
    patch: Map<String, Value>,
) -> CommandResult<Map<String, Value>> {
    let defaults = default_advanced_config();
    for key in patch.keys() {
        if !defaults.contains_key(key) {
            return Err(CommandError::new(format!("不支持的高级设置项: {key}")));
        }
    }

    let mut merged = current
        .as_object()
        .cloned()
        .unwrap_or_else(|| defaults.clone());
    merged.retain(|key, _| defaults.contains_key(key));
    for (key, value) in patch {
        merged.insert(key, value);
    }
    for (key, value) in defaults {
        merged.entry(key).or_insert(value);
    }

    validate_integer_range(&merged, "proxy_port", "代理服务端口", 1, Some(65_535))?;
    validate_integer_range(
        &merged,
        "periodic_test_interval",
        "活跃节点心跳间隔",
        30_000,
        None,
    )?;
    validate_integer_range(
        &merged,
        "probe_recovery_interval",
        "失败节点重测间隔",
        30_000,
        None,
    )?;
    validate_integer_range(&merged, "probe_concurrency", "并发测活数", 1, Some(64))?;
    validate_integer_range(&merged, "probe_failure_threshold", "连续失败阈值", 1, None)?;
    validate_integer_range(
        &merged,
        "dns_refresh_interval",
        "动态 DNS 刷新间隔",
        30_000,
        None,
    )?;
    validate_integer_range(&merged, "log_retention_days", "日志保留天数", 1, None)?;
    validate_integer_range(
        &merged,
        "circuit_failure_threshold",
        "熔断失败阈值",
        1,
        None,
    )?;
    validate_integer_range(&merged, "circuit_timeout", "熔断时长", 1_000, None)?;
    validate_integer_range(
        &merged,
        "failfast_max_attempts",
        "快速失败最大尝试次数",
        1,
        None,
    )?;
    let attempt_timeout = validate_integer_range(
        &merged,
        "failfast_attempt_timeout",
        "单次连接超时",
        100,
        None,
    )?;
    let total_timeout =
        validate_integer_range(&merged, "failfast_total_timeout", "总连接超时", 100, None)?;
    if total_timeout < attempt_timeout {
        return Err(CommandError::new("总连接超时不能小于单次连接超时"));
    }

    for (key, label) in [
        ("allow_lan", "允许局域网连接"),
        ("inbound_auth_enabled", "入站认证"),
        ("background_run", "后台运行"),
        ("start_minimized", "启动时最小化"),
        ("failfast_enabled", "快速失败"),
    ] {
        if merged.get(key).and_then(Value::as_bool).is_none() {
            return Err(CommandError::new(format!("{label}必须是布尔值")));
        }
    }

    let username = merged
        .get("inbound_auth_username")
        .and_then(Value::as_str)
        .ok_or_else(|| CommandError::new("入站认证用户名必须是字符串"))?
        .trim()
        .to_string();
    let password = merged
        .get("inbound_auth_password")
        .and_then(Value::as_str)
        .ok_or_else(|| CommandError::new("入站认证密码必须是字符串"))?
        .to_string();
    if username.len() > 255 || username.contains(':') || username.chars().any(char::is_control) {
        return Err(CommandError::new(
            "入站认证用户名不能超过 255 字节，且不能包含冒号或控制字符",
        ));
    }
    if password.len() > 255 || password.chars().any(char::is_control) {
        return Err(CommandError::new(
            "入站认证密码不能超过 255 字节，且不能包含控制字符",
        ));
    }
    if merged
        .get("inbound_auth_enabled")
        .and_then(Value::as_bool)
        .unwrap_or(false)
        && (username.is_empty() || password.is_empty())
    {
        return Err(CommandError::new("启用入站认证前必须填写用户名和密码"));
    }
    merged.insert("inbound_auth_username".to_string(), Value::String(username));
    merged.insert("inbound_auth_password".to_string(), Value::String(password));
    Ok(merged)
}

fn validate_integer_range(
    config: &Map<String, Value>,
    key: &str,
    label: &str,
    minimum: i64,
    maximum: Option<i64>,
) -> CommandResult<i64> {
    let value = config
        .get(key)
        .and_then(Value::as_i64)
        .ok_or_else(|| CommandError::new(format!("{label}必须是整数")))?;
    if value < minimum || maximum.is_some_and(|maximum| value > maximum) {
        let range = maximum.map_or_else(
            || format!("不小于 {minimum}"),
            |maximum| format!("在 {minimum} 到 {maximum} 之间"),
        );
        return Err(CommandError::new(format!("{label}必须{range}")));
    }
    Ok(value)
}

fn validate_test_url(value: &str) -> CommandResult<()> {
    let parsed = url::Url::parse(value)
        .map_err(|error| CommandError::new(format!("默认测试地址无效: {error}")))?;
    if !matches!(parsed.scheme(), "http" | "https") || parsed.host_str().is_none() {
        return Err(CommandError::new(
            "默认测试地址必须是包含主机名的 HTTP 或 HTTPS URL",
        ));
    }
    Ok(())
}

fn restart_required(current: &Value, next: &Value) -> bool {
    ["proxy_port", "allow_lan"]
        .into_iter()
        .any(|key| current.get(key) != next.get(key))
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ExportSelection {
    #[serde(default)]
    pub proxy_ids: Vec<i64>,
    #[serde(default)]
    pub dns_ids: Vec<i64>,
    #[serde(default)]
    pub group_ids: Vec<i64>,
}

#[tauri::command]
pub async fn export_selected_config(
    state: tauri::State<'_, Arc<AppState>>,
    selection: ExportSelection,
) -> CommandResult<Value> {
    if selection.proxy_ids.is_empty()
        && selection.dns_ids.is_empty()
        && selection.group_ids.is_empty()
    {
        return Err(CommandError::new("请先选择要导出的代理、DNS 映射或分组"));
    }

    let bundle = state.db.export_bundle(
        &selection.proxy_ids,
        &selection.dns_ids,
        &selection.group_ids,
    )?;
    let counts = json!({
        "proxies": bundle.proxies.len(),
        "dnsMappings": bundle.dns_mappings.len(),
        "proxyGroups": bundle.proxy_groups.len()
    });
    let content = serde_json::to_vec_pretty(&bundle)?;

    let default_name = format!(
        "proxy-load-config-{}.json",
        chrono::Local::now().format("%Y%m%d-%H%M%S")
    );
    let target = tauri::async_runtime::spawn_blocking(move || {
        rfd::FileDialog::new()
            .set_title("导出配置")
            .set_file_name(&default_name)
            .add_filter("JSON 配置文件", &["json"])
            .save_file()
    })
    .await
    .map_err(|error| CommandError::new(format!("打开保存对话框失败: {error}")))?;

    let Some(path) = target else {
        return Ok(json!({ "canceled": true }));
    };
    fs::write(&path, content)?;
    Ok(json!({
        "canceled": false,
        "path": path.display().to_string(),
        "counts": counts
    }))
}

#[tauri::command]
pub async fn import_config_file(state: tauri::State<'_, Arc<AppState>>) -> CommandResult<Value> {
    let state = state.inner().clone();
    let target = tauri::async_runtime::spawn_blocking(|| {
        rfd::FileDialog::new()
            .set_title("导入配置")
            .add_filter("JSON 配置文件", &["json"])
            .pick_file()
    })
    .await
    .map_err(|error| CommandError::new(format!("打开文件对话框失败: {error}")))?;

    let Some(path) = target else {
        return Ok(json!({ "canceled": true }));
    };
    let raw = fs::read_to_string(&path)?;
    let bundle: ConfigBundle = serde_json::from_str(&raw)
        .map_err(|error| CommandError::new(format!("配置文件格式无效: {error}")))?;
    if bundle.kind != CONFIG_BUNDLE_KIND {
        return Err(CommandError::new(
            "选中的文件不是本系统导出的配置文件，无法导入",
        ));
    }

    let summary = state.db.import_bundle(&bundle)?;
    state.proxy_runtime.refresh_dns_cache().await?;
    state.notify_proxy_config_changed();
    state.emit("config_imported", serde_json::to_value(&summary)?);
    Ok(json!({ "canceled": false, "summary": summary }))
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
          AND COALESCE(result_type, '') NOT IN ('health_success', 'health_failure')
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
          AND COALESCE(rl.result_type, '') NOT IN ('health_success', 'health_failure')
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
          AND COALESCE(result_type, '') NOT IN ('health_success', 'health_failure')
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
          AND COALESCE(result_type, '') NOT IN ('health_success', 'health_failure')
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
pub fn list_dns_mappings(state: tauri::State<'_, Arc<AppState>>) -> CommandResult<Value> {
    Ok(serde_json::to_value(state.db.list_dns_mappings()?)?)
}

/// 动态映射在保存前先按域名解析一次拿到当前真实 IP；解析失败则回退到用户填写的 IP。
async fn resolve_dynamic_input(mut input: DnsInput) -> CommandResult<DnsInput> {
    if input.dynamic == Some(1) {
        match crate::state::resolve_ipv4(&input.domain).await {
            Some(ip) => input.ip = ip,
            None if input.ip.trim().is_empty() => {
                return Err(CommandError::new(format!(
                    "动态解析失败：无法解析域名 {}，请先填写一个初始/备用 IP",
                    input.domain.trim()
                )));
            }
            None => {}
        }
    }
    Ok(input)
}

#[tauri::command]
pub async fn create_dns_mapping(
    state: tauri::State<'_, Arc<AppState>>,
    input: DnsInput,
) -> CommandResult<Value> {
    let state = state.inner().clone();
    let input = resolve_dynamic_input(input).await?;
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
    let input = resolve_dynamic_input(input).await?;
    let mapping = state.db.update_dns_mapping(id, input)?;
    state.proxy_runtime.refresh_dns_cache().await?;
    state.emit("dns_mapping_updated", serde_json::to_value(&mapping)?);
    Ok(json!({ "success": true }))
}

#[tauri::command]
pub async fn refresh_dns_mapping(
    state: tauri::State<'_, Arc<AppState>>,
    id: i64,
) -> CommandResult<Value> {
    let state = state.inner().clone();
    let mapping = state
        .db
        .get_dns_mapping(id)?
        .ok_or_else(|| CommandError::new("DNS 映射不存在"))?;
    match crate::state::resolve_ipv4(&mapping.domain).await {
        Some(ip) => {
            let changed = ip != mapping.ip;
            if changed {
                let applied = state.db.update_dns_ip_if_unchanged(
                    id,
                    &mapping.domain,
                    &mapping.ip,
                    mapping.enabled,
                    mapping.dynamic,
                    &ip,
                )?;
                if !applied {
                    return Err(CommandError::new(
                        "DNS 映射在解析期间发生变化，已丢弃旧解析结果",
                    ));
                }
                state.proxy_runtime.refresh_dns_cache().await?;
                let updated = state.db.get_dns_mapping(id)?;
                state.emit("dns_mapping_updated", serde_json::to_value(&updated)?);
            }
            Ok(json!({
                "success": true,
                "changed": changed,
                "ip": ip,
                "previousIp": mapping.ip
            }))
        }
        None => Err(CommandError::new(format!(
            "无法解析域名 {}，请检查网络或 DNS 配置",
            mapping.domain
        ))),
    }
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
    proxy_search: Option<String>,
) -> CommandResult<Value> {
    let page = ensure_positive(page.unwrap_or(1), "page")?;
    let page_size = ensure_positive(page_size.unwrap_or(50), "pageSize")?;
    let (items, total) = state
        .db
        .traffic_logs(page, page_size, proxy_search.as_deref())?;
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
    download_url: String,
    version: String,
    kind: String,
    is_newer: bool,
    size: Option<u64>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct UpdateInfo {
    current_version: String,
    app_dir: String,
    download_dir: String,
    install_mode: String,
    source: String,
    has_update: bool,
    latest: Option<UpdateArtifact>,
    artifacts: Vec<UpdateArtifact>,
}

#[tauri::command]
pub async fn check_for_updates(use_mirror: Option<bool>) -> CommandResult<UpdateInfo> {
    build_update_info(use_mirror.unwrap_or(false)).await
}

#[tauri::command]
pub async fn install_update(
    app: AppHandle,
    artifact_path: Option<String>,
    use_mirror: Option<bool>,
) -> CommandResult<Value> {
    let use_mirror = use_mirror.unwrap_or(false);
    let info = build_update_info(use_mirror).await?;
    let app_dir = PathBuf::from(&info.app_dir);
    let download_dir = PathBuf::from(&info.download_dir);
    let selected = artifact_path
        .and_then(|path| {
            info.artifacts
                .iter()
                .find(|artifact| artifact.path == path)
                .cloned()
        })
        .or(info.latest.clone())
        .ok_or_else(|| CommandError::new("没有可安装的更新包"))?;

    if !selected.is_newer {
        return Err(CommandError::new("选中的更新包版本不高于当前版本"));
    }

    fs::create_dir_all(&download_dir)?;
    let selected_path = download_dir.join(&selected.file_name);
    if selected_path == std::env::current_exe()? {
        return Err(CommandError::new(
            "更新包文件名与当前运行程序相同，无法在运行中覆盖自身",
        ));
    }
    download_release_asset(&selected.download_url, &selected_path, !use_mirror).await?;
    launch_update_installer(&app, &selected_path, &app_dir, &selected.kind)?;

    let message = match selected.kind.as_str() {
        "windows-portable" => "已下载便携更新包到当前应用目录，应用即将启动新版本",
        "windows-nsis" | "windows-msi" => {
            "已下载更新包并静默启动安装，安装完成后会自动重新启动应用"
        }
        "macos-dmg" => {
            "已下载并打开 macOS 更新包，请退出当前应用后在 DMG 中拖动应用到 Applications 并覆盖旧版"
        }
        _ => "已下载 GitHub Release 更新包，并启动安装程序，安装目录已指向当前应用所在目录",
    };

    Ok(json!({
        "success": true,
        "installDir": app_dir,
        "artifactPath": selected_path,
        "message": message
    }))
}

fn launch_update_installer(
    app: &AppHandle,
    selected_path: &Path,
    app_dir: &Path,
    kind: &str,
) -> CommandResult<()> {
    let extension = selected_path
        .extension()
        .and_then(|value| value.to_str())
        .unwrap_or_default()
        .to_ascii_lowercase();

    if kind == "windows-portable" {
        return launch_portable_update(app, selected_path, app_dir);
    }
    if kind == "macos-dmg" {
        return launch_macos_dmg_update(selected_path);
    }

    #[cfg(target_os = "windows")]
    if matches!(kind, "windows-nsis" | "windows-msi") {
        return launch_windows_installer_update(app, selected_path, app_dir, kind);
    }

    match extension.as_str() {
        "exe" => {
            Command::new(selected_path)
                .arg("/S")
                .arg(format!("/D={}", app_dir.display()))
                .current_dir(app_dir)
                .spawn()?;
        }
        "msi" => {
            Command::new("msiexec")
                .arg("/i")
                .arg(selected_path)
                .arg("/qn")
                .arg("/norestart")
                .arg(format!("TARGETDIR={}", app_dir.display()))
                .current_dir(app_dir)
                .spawn()?;
        }
        _ => {
            return Err(CommandError::new(format!(
                "当前平台暂不支持直接安装 {} 更新包",
                selected_path.display()
            )));
        }
    }

    #[cfg(target_os = "windows")]
    if matches!(kind, "windows-nsis" | "windows-msi") {
        tauri_plugin_single_instance::destroy(app);
        schedule_update_exit();
    }

    Ok(())
}

#[cfg(target_os = "windows")]
fn launch_windows_installer_update(
    app: &AppHandle,
    selected_path: &Path,
    app_dir: &Path,
    kind: &str,
) -> CommandResult<()> {
    tauri_plugin_single_instance::destroy(app);

    let launch_path = std::env::current_exe()?;
    let helper_path = windows_update_helper_path()?;
    fs::copy(&launch_path, &helper_path)?;

    Command::new(&helper_path)
        .arg(UPDATE_HELPER_ARG)
        .arg(UPDATE_HELPER_PARENT_PID_ARG)
        .arg(std::process::id().to_string())
        .arg(UPDATE_HELPER_INSTALLER_PATH_ARG)
        .arg(selected_path)
        .arg(UPDATE_HELPER_INSTALLER_KIND_ARG)
        .arg(kind)
        .arg(UPDATE_HELPER_INSTALL_DIR_ARG)
        .arg(app_dir)
        .arg(UPDATE_HELPER_LAUNCH_PATH_ARG)
        .arg(&launch_path)
        .current_dir(app_dir)
        .creation_flags(CREATE_NO_WINDOW)
        .spawn()?;

    schedule_update_exit();
    Ok(())
}

#[cfg(target_os = "windows")]
fn windows_update_helper_path() -> CommandResult<PathBuf> {
    let stamp = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_err(|error| CommandError::new(format!("无法生成更新辅助进程文件名: {error}")))?
        .as_millis();
    Ok(std::env::temp_dir().join(format!(
        "proxy-load-update-helper-{}-{stamp}.exe",
        std::process::id()
    )))
}

#[cfg(target_os = "windows")]
fn launch_portable_update(
    app: &AppHandle,
    selected_path: &Path,
    app_dir: &Path,
) -> CommandResult<()> {
    tauri_plugin_single_instance::destroy(app);

    Command::new(selected_path)
        .current_dir(app_dir)
        .creation_flags(CREATE_NO_WINDOW)
        .spawn()?;

    schedule_update_exit();

    Ok(())
}

#[cfg(target_os = "windows")]
fn schedule_update_exit() {
    std::thread::spawn(|| {
        std::thread::sleep(UPDATE_EXIT_DELAY);
        std::process::exit(0);
    });
}

#[cfg(target_os = "windows")]
pub fn run_windows_update_helper_from_args() -> AnyhowResult<bool> {
    let Some(args) = WindowsUpdateHelperArgs::from_process_args()? else {
        return Ok(false);
    };

    wait_for_process_exit(args.parent_pid)
        .with_context(|| format!("等待旧应用进程退出失败: {}", args.parent_pid))?;

    let installer_status =
        spawn_windows_update_installer(&args).context("启动或等待 Windows 更新安装器失败")?;
    if !installer_status.success() {
        return Err(anyhow!(
            "Windows 更新安装器退出码异常: {}",
            installer_status
        ));
    }

    Command::new(&args.launch_path)
        .current_dir(&args.install_dir)
        .creation_flags(CREATE_NO_WINDOW)
        .spawn()
        .with_context(|| format!("启动更新后的应用失败: {}", args.launch_path.display()))?;

    Ok(true)
}

#[cfg(target_os = "windows")]
fn spawn_windows_update_installer(
    args: &WindowsUpdateHelperArgs,
) -> std::io::Result<std::process::ExitStatus> {
    let mut command = match args.installer_kind.as_str() {
        "windows-nsis" => {
            let mut command = Command::new(&args.installer_path);
            command
                .arg("/S")
                .arg(format!("/D={}", args.install_dir.display()));
            command
        }
        "windows-msi" => {
            let mut command = Command::new("msiexec");
            command
                .arg("/i")
                .arg(&args.installer_path)
                .arg("/qn")
                .arg("/norestart")
                .arg(format!("TARGETDIR={}", args.install_dir.display()));
            command
        }
        other => {
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidInput,
                format!("不支持的 Windows 更新安装器类型: {other}"),
            ));
        }
    };

    command
        .current_dir(&args.install_dir)
        .creation_flags(CREATE_NO_WINDOW)
        .spawn()?
        .wait()
}

#[cfg(target_os = "windows")]
#[derive(Debug)]
struct WindowsUpdateHelperArgs {
    parent_pid: u32,
    installer_path: PathBuf,
    installer_kind: String,
    install_dir: PathBuf,
    launch_path: PathBuf,
}

#[cfg(target_os = "windows")]
impl WindowsUpdateHelperArgs {
    fn from_process_args() -> AnyhowResult<Option<Self>> {
        let args = std::env::args_os().skip(1).collect::<Vec<_>>();
        if !args
            .iter()
            .any(|arg| arg.to_string_lossy() == UPDATE_HELPER_ARG)
        {
            return Ok(None);
        }
        Self::parse(args).map(Some)
    }

    fn parse(args: Vec<OsString>) -> AnyhowResult<Self> {
        let mut parent_pid = None;
        let mut installer_path = None;
        let mut installer_kind = None;
        let mut install_dir = None;
        let mut launch_path = None;

        let mut iter = args.into_iter();
        while let Some(arg) = iter.next() {
            let key = arg.to_string_lossy();
            match key.as_ref() {
                UPDATE_HELPER_ARG => {}
                UPDATE_HELPER_PARENT_PID_ARG => {
                    let value = next_update_helper_value(&mut iter, UPDATE_HELPER_PARENT_PID_ARG)?;
                    parent_pid = Some(
                        value
                            .to_string_lossy()
                            .parse::<u32>()
                            .context("更新辅助进程 parent pid 无效")?,
                    );
                }
                UPDATE_HELPER_INSTALLER_PATH_ARG => {
                    installer_path = Some(PathBuf::from(next_update_helper_value(
                        &mut iter,
                        UPDATE_HELPER_INSTALLER_PATH_ARG,
                    )?));
                }
                UPDATE_HELPER_INSTALLER_KIND_ARG => {
                    installer_kind = Some(
                        next_update_helper_value(&mut iter, UPDATE_HELPER_INSTALLER_KIND_ARG)?
                            .to_string_lossy()
                            .to_string(),
                    );
                }
                UPDATE_HELPER_INSTALL_DIR_ARG => {
                    install_dir = Some(PathBuf::from(next_update_helper_value(
                        &mut iter,
                        UPDATE_HELPER_INSTALL_DIR_ARG,
                    )?));
                }
                UPDATE_HELPER_LAUNCH_PATH_ARG => {
                    launch_path = Some(PathBuf::from(next_update_helper_value(
                        &mut iter,
                        UPDATE_HELPER_LAUNCH_PATH_ARG,
                    )?));
                }
                other => return Err(anyhow!("未知更新辅助进程参数: {other}")),
            }
        }

        Ok(Self {
            parent_pid: parent_pid.ok_or_else(|| anyhow!("缺少更新辅助进程 parent pid"))?,
            installer_path: installer_path.ok_or_else(|| anyhow!("缺少更新安装器路径"))?,
            installer_kind: installer_kind.ok_or_else(|| anyhow!("缺少更新安装器类型"))?,
            install_dir: install_dir.ok_or_else(|| anyhow!("缺少更新安装目录"))?,
            launch_path: launch_path.ok_or_else(|| anyhow!("缺少更新后启动路径"))?,
        })
    }
}

#[cfg(target_os = "windows")]
fn next_update_helper_value(
    iter: &mut impl Iterator<Item = OsString>,
    key: &str,
) -> AnyhowResult<OsString> {
    iter.next()
        .ok_or_else(|| anyhow!("更新辅助进程参数 {key} 缺少值"))
}

#[cfg(target_os = "windows")]
fn wait_for_process_exit(pid: u32) -> std::io::Result<()> {
    const SYNCHRONIZE: u32 = 0x0010_0000;
    const WAIT_OBJECT_0: u32 = 0x0000_0000;
    const WAIT_FAILED: u32 = 0xffff_ffff;
    const INFINITE: u32 = 0xffff_ffff;
    const ERROR_INVALID_PARAMETER: u32 = 87;

    unsafe {
        let handle = OpenProcess(SYNCHRONIZE, 0, pid);
        if handle.is_null() {
            let error = GetLastError();
            if error == ERROR_INVALID_PARAMETER {
                return Ok(());
            }
            return Err(std::io::Error::from_raw_os_error(error as i32));
        }

        let wait = WaitForSingleObject(handle, INFINITE);
        let close_result = CloseHandle(handle);
        if close_result == 0 {
            return Err(std::io::Error::last_os_error());
        }
        if wait == WAIT_OBJECT_0 {
            Ok(())
        } else if wait == WAIT_FAILED {
            Err(std::io::Error::last_os_error())
        } else {
            Err(std::io::Error::other(format!(
                "等待进程退出返回未知状态: {wait}"
            )))
        }
    }
}

#[cfg(target_os = "windows")]
type WindowsHandle = *mut std::ffi::c_void;

#[cfg(target_os = "windows")]
extern "system" {
    fn OpenProcess(dwDesiredAccess: u32, bInheritHandle: i32, dwProcessId: u32) -> WindowsHandle;
    fn WaitForSingleObject(hHandle: WindowsHandle, dwMilliseconds: u32) -> u32;
    fn CloseHandle(hObject: WindowsHandle) -> i32;
    fn GetLastError() -> u32;
}

#[cfg(not(target_os = "windows"))]
fn launch_portable_update(
    _app: &AppHandle,
    selected_path: &Path,
    _app_dir: &Path,
) -> CommandResult<()> {
    Err(CommandError::new(format!(
        "当前平台暂不支持直接安装便携更新包: {}",
        selected_path.display()
    )))
}

#[cfg(target_os = "macos")]
fn launch_macos_dmg_update(selected_path: &Path) -> CommandResult<()> {
    Command::new("open").arg(selected_path).spawn()?;
    Ok(())
}

#[cfg(not(target_os = "macos"))]
fn launch_macos_dmg_update(selected_path: &Path) -> CommandResult<()> {
    Err(CommandError::new(format!(
        "当前平台暂不支持直接安装 macOS 更新包: {}",
        selected_path.display()
    )))
}

fn ensure_positive(value: i64, field: &str) -> CommandResult<i64> {
    if value < 1 {
        Err(CommandError::new(format!("{field}必须是正整数")))
    } else {
        Ok(value)
    }
}

async fn build_update_info(use_mirror: bool) -> CommandResult<UpdateInfo> {
    if cfg!(debug_assertions) {
        return Err(CommandError::new(
            "开发环境不允许检查更新；生产环境将从 GitHub Releases 获取更新包",
        ));
    }

    let app_dir = current_app_dir()?;
    let download_dir = current_update_download_dir(&app_dir)?;
    let executable = std::env::current_exe()?;
    let install_mode = current_install_mode(&executable);

    let current_version = version::VERSION.to_string();
    let current = VersionParts::parse(version::VERSION)
        .ok_or_else(|| CommandError::new("当前版本号格式无效"))?;
    let (release_tag, assets) = if use_mirror {
        fetch_latest_release_via_mirror().await?
    } else {
        fetch_latest_release_via_api().await?
    };
    let release_version_text = release_tag.trim_start_matches('v').to_string();
    let release_version = VersionParts::parse(&release_version_text).ok_or_else(|| {
        CommandError::new(format!("GitHub Release 标签不是有效版本号: {release_tag}"))
    })?;

    let mut artifacts = assets
        .into_iter()
        .filter_map(|asset| {
            let kind = artifact_kind_from_name(&asset.name)?;
            if !is_current_platform_artifact(kind, install_mode) {
                return None;
            }
            Some(UpdateArtifact {
                file_name: asset.name,
                path: asset.download_url.clone(),
                download_url: asset.download_url,
                version: release_version_text.clone(),
                kind: kind.to_string(),
                is_newer: release_version > current,
                size: asset.size,
            })
        })
        .collect::<Vec<_>>();
    artifacts.sort_by(|left, right| compare_artifacts(right, left));
    let latest = artifacts.iter().find(|artifact| artifact.is_newer).cloned();

    Ok(UpdateInfo {
        current_version,
        app_dir: app_dir.display().to_string(),
        download_dir: download_dir.display().to_string(),
        install_mode: install_mode.to_string(),
        source: if use_mirror {
            "gh-proxy-mirror".to_string()
        } else {
            "github-releases".to_string()
        },
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

fn current_update_download_dir(app_dir: &Path) -> CommandResult<PathBuf> {
    if cfg!(target_os = "macos") {
        let home = std::env::var_os("HOME")
            .ok_or_else(|| CommandError::new("无法确定 macOS 下载目录，缺少 HOME 环境变量"))?;
        return Ok(PathBuf::from(home).join("Downloads"));
    }
    Ok(app_dir.to_path_buf())
}

fn current_install_mode(executable: &Path) -> &'static str {
    let file_name = executable
        .file_name()
        .and_then(|value| value.to_str())
        .unwrap_or_default()
        .to_ascii_lowercase();

    if cfg!(target_os = "windows") && file_name.contains("portable") {
        "portable"
    } else {
        "installed"
    }
}

struct ReleaseAssetRef {
    name: String,
    download_url: String,
    size: Option<u64>,
}

async fn fetch_latest_release_via_api() -> CommandResult<(String, Vec<ReleaseAssetRef>)> {
    let client = github_client(Duration::from_secs(20))?;
    let response = github_get(&client, GITHUB_LATEST_RELEASE_URL, true)
        .send()
        .await?;
    let response = ensure_github_success(response, "查询").await?;
    let release: GithubRelease = response.json().await?;
    let assets = release
        .assets
        .into_iter()
        .map(|asset| ReleaseAssetRef {
            name: asset.name,
            download_url: asset.browser_download_url,
            size: Some(asset.size),
        })
        .collect();
    Ok((release.tag_name, assets))
}

/// gh-proxy 镜像不代理 api.github.com，改用 releases/latest 的 302 重定向获取最新
/// 标签，再从 expanded_assets 页面提取资产文件名。
async fn fetch_latest_release_via_mirror() -> CommandResult<(String, Vec<ReleaseAssetRef>)> {
    let client = reqwest::Client::builder()
        .timeout(Duration::from_secs(20))
        .user_agent(format!("proxy-load/{}", version::VERSION))
        .redirect(reqwest::redirect::Policy::none())
        .build()?;
    let response = client
        .get(format!("{GH_PROXY_BASE}{GITHUB_RELEASES_URL}/latest"))
        .send()
        .await?;
    let status = response.status();
    let tag = if status.is_redirection() {
        response
            .headers()
            .get(reqwest::header::LOCATION)
            .and_then(|value| value.to_str().ok())
            .and_then(extract_release_tag)
    } else if status.is_success() {
        extract_release_tag(&response.text().await?)
    } else {
        return Err(CommandError::new(format!(
            "国内加速查询最新版本失败: HTTP {status}（镜像 {GH_PROXY_BASE}）"
        )));
    };
    let tag = tag
        .ok_or_else(|| CommandError::new("国内加速未能解析最新版本号，请稍后重试或关闭国内加速"))?;

    let client = github_client(Duration::from_secs(20))?;
    let response = client
        .get(format!(
            "{GH_PROXY_BASE}{GITHUB_RELEASES_URL}/expanded_assets/{tag}"
        ))
        .send()
        .await?;
    let status = response.status();
    if !status.is_success() {
        return Err(CommandError::new(format!(
            "国内加速获取更新包列表失败: HTTP {status}（镜像 {GH_PROXY_BASE}）"
        )));
    }
    let html = response.text().await?;
    let names = extract_release_asset_names(&html, &tag);
    if names.is_empty() {
        return Err(CommandError::new(
            "国内加速未返回任何更新包，可能镜像暂不可用，请稍后重试或关闭国内加速",
        ));
    }

    let assets = names
        .into_iter()
        .map(|name| ReleaseAssetRef {
            download_url: format!("{GH_PROXY_BASE}{GITHUB_RELEASES_URL}/download/{tag}/{name}"),
            name,
            size: None,
        })
        .collect();
    Ok((tag, assets))
}

fn extract_release_tag(text: &str) -> Option<String> {
    let marker = "/releases/tag/";
    let start = text.find(marker)? + marker.len();
    let rest = &text[start..];
    let end = rest
        .find(|c: char| c == '"' || c == '\'' || c == '?' || c == '#' || c.is_whitespace())
        .unwrap_or(rest.len());
    let tag = rest[..end].trim_end_matches('/');
    if tag.is_empty() {
        None
    } else {
        Some(tag.to_string())
    }
}

fn extract_release_asset_names(html: &str, tag: &str) -> Vec<String> {
    let marker = format!("/releases/download/{tag}/");
    let mut seen = HashSet::new();
    let mut names = Vec::new();
    let mut rest = html;
    while let Some(position) = rest.find(&marker) {
        let after = &rest[position + marker.len()..];
        if let Some(end) = after.find('"') {
            let name = &after[..end];
            if !name.is_empty() && !name.contains('/') && seen.insert(name.to_string()) {
                names.push(name.to_string());
            }
        }
        rest = &rest[position + marker.len()..];
    }
    names
}

async fn download_release_asset(
    download_url: &str,
    target_path: &Path,
    with_token: bool,
) -> CommandResult<()> {
    let client = github_client(Duration::from_secs(120))?;
    let response = github_get(&client, download_url, with_token).send().await?;
    let response = ensure_github_success(response, "下载").await?;

    fs::write(target_path, response.bytes().await?)?;
    Ok(())
}

fn github_client(timeout: Duration) -> CommandResult<reqwest::Client> {
    Ok(reqwest::Client::builder()
        .timeout(timeout)
        .user_agent(format!("proxy-load/{}", version::VERSION))
        .build()?)
}

/// 直连 GitHub 时附加可选 Token；镜像请求不携带，避免凭据泄露给第三方。
fn github_get(client: &reqwest::Client, url: &str, with_token: bool) -> reqwest::RequestBuilder {
    let request = client.get(url);
    if !with_token {
        return request;
    }
    match std::env::var(GITHUB_TOKEN_ENV) {
        Ok(token) if !token.trim().is_empty() => request.bearer_auth(token.trim().to_string()),
        _ => request,
    }
}

async fn ensure_github_success(
    response: reqwest::Response,
    action: &str,
) -> CommandResult<reqwest::Response> {
    let status = response.status();
    if status.is_success() {
        return Ok(response);
    }

    let detail = response.text().await.unwrap_or_default();
    let detail = detail.trim();
    let detail = if detail.is_empty() {
        String::new()
    } else {
        format!("；GitHub 返回: {detail}")
    };

    let message = match status {
        reqwest::StatusCode::UNAUTHORIZED => format!(
            "GitHub Release {action}失败: HTTP 401。GitHub Token 无效或缺少权限。私有仓库请设置环境变量 {GITHUB_TOKEN_ENV}，并授予读取私有仓库 Release 的 repo 权限{detail}"
        ),
        reqwest::StatusCode::FORBIDDEN => format!(
            "GitHub Release {action}失败: HTTP 403。当前 Token 没有读取 Release 的权限，或触发了 GitHub API 限制{detail}"
        ),
        reqwest::StatusCode::NOT_FOUND => format!(
            "GitHub Release {action}失败: HTTP 404。私有仓库未认证访问时 GitHub 会返回 404；请设置环境变量 {GITHUB_TOKEN_ENV}，或将发布仓库改为公开{detail}"
        ),
        _ => format!("GitHub Release {action}失败: HTTP {status}{detail}"),
    };

    Err(CommandError::new(message))
}

fn artifact_kind_from_name(file_name: &str) -> Option<&'static str> {
    let lower_name = file_name.to_ascii_lowercase();
    let extension = Path::new(file_name)
        .extension()?
        .to_string_lossy()
        .to_ascii_lowercase();

    match extension.as_str() {
        "exe" if lower_name.contains("portable") => Some("windows-portable"),
        "exe" if lower_name.contains("setup") => Some("windows-nsis"),
        "exe" => Some("windows-exe"),
        "msi" => Some("windows-msi"),
        "dmg" => Some("macos-dmg"),
        "deb" => Some("linux-deb"),
        "rpm" => Some("linux-rpm"),
        "appimage" => Some("linux-appimage"),
        _ => None,
    }
}

fn is_current_platform_artifact(kind: &str, install_mode: &str) -> bool {
    if cfg!(target_os = "windows") {
        return if install_mode == "portable" {
            kind == "windows-portable"
        } else {
            matches!(kind, "windows-nsis" | "windows-msi")
        };
    }
    if cfg!(target_os = "macos") {
        return kind == "macos-dmg";
    }
    if cfg!(target_os = "linux") {
        return matches!(kind, "linux-deb" | "linux-rpm" | "linux-appimage");
    }
    false
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
        _ => 10,
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
struct VersionParts {
    major: u64,
    minor: u64,
    patch: u64,
    revision: u64,
}

impl VersionParts {
    fn parse(value: &str) -> Option<Self> {
        let (version, metadata) = value
            .split_once('+')
            .map_or((value, None), |(version, metadata)| {
                (version, Some(metadata))
            });
        let core = version.split_once('-').map_or(version, |(core, _)| core);
        let mut parts = core.split('.');
        let raw_major: u64 = parts.next()?.parse().ok()?;
        let minor = parts.next()?.parse().ok()?;
        let raw_patch: u64 = parts.next()?.parse().ok()?;
        if parts.next().is_some() {
            return None;
        }
        let metadata_revision = match metadata {
            Some(value) => value.parse().ok()?,
            None => 0,
        };

        let major = if raw_major >= 2000 {
            raw_major - 2000
        } else {
            raw_major
        };
        let (patch, encoded_revision) = if major < 100 && raw_patch >= 100 {
            (raw_patch / 100, raw_patch % 100)
        } else {
            (raw_patch, 0)
        };
        let revision = if metadata.is_some() {
            metadata_revision
        } else {
            encoded_revision
        };

        Some(Self {
            major,
            minor,
            patch,
            revision,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::{
        extract_release_asset_names, extract_release_tag, normalize_advanced_config,
        normalize_load_settings, restart_required, VersionParts,
    };
    use crate::database::default_advanced_config;
    use serde_json::{json, Map, Value};

    #[test]
    fn load_settings_reject_removed_manual_mode_and_normalizes_round_robin() {
        let unsupported = Map::from_iter([("load_mode".to_string(), json!("manual"))]);
        assert!(normalize_load_settings(unsupported).is_err());

        let settings = Map::from_iter([
            ("algorithm".to_string(), json!("weighted_round_robin")),
            ("test_url".to_string(), json!("https://example.com/ping")),
            ("timeout".to_string(), json!("15")),
        ]);
        let normalized = normalize_load_settings(settings).unwrap();
        assert_eq!(normalized["algorithm"], json!("round_robin"));
        assert_eq!(normalized["timeout"], json!("15"));
    }

    #[test]
    fn advanced_config_requires_persistent_credentials_when_auth_is_enabled() {
        let current = Value::Object(default_advanced_config());
        let missing_credentials =
            Map::from_iter([("inbound_auth_enabled".to_string(), json!(true))]);
        assert!(normalize_advanced_config(&current, missing_credentials).is_err());

        let valid = Map::from_iter([
            ("inbound_auth_enabled".to_string(), json!(true)),
            (
                "inbound_auth_username".to_string(),
                json!("f7e7497d-ae52-47d5-9f0d-2d780aac9c65"),
            ),
            (
                "inbound_auth_password".to_string(),
                json!("32-character-persistent-password"),
            ),
        ]);
        let normalized = normalize_advanced_config(&current, valid).unwrap();
        assert_eq!(normalized["inbound_auth_enabled"], json!(true));
    }

    #[test]
    fn advanced_config_rejects_obsolete_fields_and_reports_listener_restart() {
        let current = Value::Object(default_advanced_config());
        let obsolete = Map::from_iter([("pool_max_size".to_string(), json!(50))]);
        assert!(normalize_advanced_config(&current, obsolete).is_err());

        let next = Value::Object(
            normalize_advanced_config(
                &current,
                Map::from_iter([("allow_lan".to_string(), json!(true))]),
            )
            .unwrap(),
        );
        assert!(restart_required(&current, &next));
    }

    #[test]
    fn compares_legacy_date_versions_with_windows_safe_versions() {
        let legacy = VersionParts::parse("2026.6.5").unwrap();
        let current = VersionParts::parse("26.6.501").unwrap();

        assert!(current > legacy);
    }

    #[test]
    fn compares_new_calendar_day_after_previous_revision() {
        assert!(VersionParts::parse("26.6.10").unwrap() > VersionParts::parse("26.6.501").unwrap());
        assert!(
            VersionParts::parse("26.6.16").unwrap() > VersionParts::parse("26.6.1002").unwrap()
        );
        assert!(
            VersionParts::parse("26.6.1601").unwrap() > VersionParts::parse("26.6.16").unwrap()
        );
    }

    #[test]
    fn compares_same_day_encoded_revision_after_base_release() {
        assert!(
            VersionParts::parse("26.6.1001").unwrap() > VersionParts::parse("26.6.10").unwrap()
        );
        assert!(
            VersionParts::parse("26.6.1002").unwrap() > VersionParts::parse("26.6.1001").unwrap()
        );
    }

    #[test]
    fn keeps_same_day_versions_equal_without_revision() {
        assert_eq!(
            VersionParts::parse("2026.6.5").unwrap(),
            VersionParts::parse("26.6.5").unwrap()
        );
    }

    #[test]
    fn treats_semver_metadata_as_same_day_revision() {
        assert_eq!(
            VersionParts::parse("2026.6.10+1").unwrap(),
            VersionParts::parse("26.6.1001").unwrap()
        );
        assert_eq!(
            VersionParts::parse("2026.6.10+2").unwrap(),
            VersionParts::parse("26.6.1002").unwrap()
        );
    }

    #[test]
    fn extracts_tag_from_mirror_location_header() {
        assert_eq!(
            extract_release_tag("/https://github.com/ccpopy/proxy-load/releases/tag/v26.6.1601"),
            Some("v26.6.1601".to_string())
        );
        assert_eq!(
            extract_release_tag("https://github.com/ccpopy/proxy-load/releases/tag/v1.2.3?x=1"),
            Some("v1.2.3".to_string())
        );
        assert_eq!(
            extract_release_tag("https://github.com/ccpopy/proxy-load"),
            None
        );
    }

    #[test]
    fn extracts_asset_names_from_expanded_assets_html() {
        let html = r#"
            <a href="/ccpopy/proxy-load/releases/download/v26.6.1601/proxy-load_26.6.1601_windows_x64-setup.exe">a</a>
            <a href="/ccpopy/proxy-load/releases/download/v26.6.1601/proxy-load_26.6.1601_windows_x64-setup.exe">dup</a>
            <a href="/ccpopy/proxy-load/releases/download/v26.6.1601/proxy-load_26.6.1601_x64-portable.exe">b</a>
        "#;
        assert_eq!(
            extract_release_asset_names(html, "v26.6.1601"),
            vec![
                "proxy-load_26.6.1601_windows_x64-setup.exe".to_string(),
                "proxy-load_26.6.1601_x64-portable.exe".to_string()
            ]
        );
    }
}

#[derive(Debug, Deserialize)]
struct GithubRelease {
    tag_name: String,
    assets: Vec<GithubAsset>,
}

#[derive(Debug, Deserialize)]
struct GithubAsset {
    name: String,
    browser_download_url: String,
    size: u64,
}
