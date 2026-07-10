use std::{
    collections::{HashMap, HashSet},
    sync::Arc,
    time::{Duration, SystemTime, UNIX_EPOCH},
};

use anyhow::{anyhow, Context, Result};
use serde::Serialize;
use serde_json::{json, Value};
use tokio::{
    sync::{broadcast, Mutex, Notify, Semaphore},
    task::JoinSet,
    time,
};
use url::Url;

use crate::{
    database::Database,
    models::{ProxyRecord, ServerEvent, TestResult},
    proxy::{self, ProxyRuntime},
    proxy_tester,
};

const PROXY_LISTEN_RETRY_INTERVAL: Duration = Duration::from_secs(5);

#[derive(Clone)]
pub struct AppState {
    pub db: Database,
    pub events: broadcast::Sender<ServerEvent>,
    pub started_at: i64,
    pub proxy_host: String,
    pub proxy_port: u16,
    pub proxy_runtime: Arc<ProxyRuntime>,
    probe_locks: Arc<Mutex<HashMap<i64, Arc<Mutex<()>>>>>,
    probe_failures: Arc<Mutex<HashMap<i64, u32>>>,
    forced_probes: Arc<Mutex<HashSet<i64>>>,
    settings_update_lock: Arc<Mutex<()>>,
    probe_notify: Arc<Notify>,
    dns_notify: Arc<Notify>,
}

#[derive(Debug, Clone, Serialize)]
pub struct ServiceInfo {
    pub proxy_host: String,
    pub proxy_port: u16,
    pub database_path: String,
    pub started_at: i64,
}

impl AppState {
    pub fn bootstrap() -> Result<Self> {
        let db = Database::open()?;
        let (events, _) = broadcast::channel(256);
        let started_at = now_millis();
        let advanced = db.load_advanced_config()?;
        let proxy_port = advanced
            .get("proxy_port")
            .and_then(Value::as_u64)
            .and_then(|value| u16::try_from(value).ok())
            .unwrap_or(5678);
        let proxy_host = if advanced
            .get("allow_lan")
            .and_then(Value::as_bool)
            .unwrap_or(false)
        {
            "0.0.0.0"
        } else {
            "127.0.0.1"
        }
        .to_string();
        let proxy_runtime = Arc::new(ProxyRuntime::new(
            db.clone(),
            events.clone(),
            &proxy_host,
            proxy_port,
            &advanced,
        )?);

        spawn_proxy_server(proxy_runtime.clone(), proxy_host.clone(), proxy_port);

        let state = Self {
            db,
            events,
            started_at,
            proxy_host,
            proxy_port,
            proxy_runtime,
            probe_locks: Arc::new(Mutex::new(HashMap::new())),
            probe_failures: Arc::new(Mutex::new(HashMap::new())),
            forced_probes: Arc::new(Mutex::new(HashSet::new())),
            settings_update_lock: Arc::new(Mutex::new(())),
            probe_notify: Arc::new(Notify::new()),
            dns_notify: Arc::new(Notify::new()),
        };
        state.spawn_periodic_proxy_tests();
        state.spawn_dynamic_dns_refresh();
        state.spawn_log_maintenance();
        Ok(state)
    }

    pub fn uptime_seconds(&self) -> i64 {
        ((now_millis() - self.started_at) / 1000).max(0)
    }

    pub fn emit(&self, event_type: impl Into<String>, data: Value) {
        let _ = self.events.send(ServerEvent {
            event_type: event_type.into(),
            data,
            timestamp: now_millis(),
        });
    }

    pub fn service_info(&self) -> ServiceInfo {
        ServiceInfo {
            proxy_host: self.proxy_host.clone(),
            proxy_port: self.proxy_port,
            database_path: self.db.path().display().to_string(),
            started_at: self.started_at,
        }
    }

    pub async fn test_proxy_by_id(&self, id: i64) -> Result<TestResult> {
        let proxy = self
            .db
            .get_proxy(id)?
            .ok_or_else(|| anyhow!("代理不存在"))?;
        self.test_proxy_record(proxy, ProbeOrigin::Manual)
            .await?
            .ok_or_else(|| anyhow!("代理已停用，无法测试"))
    }

    pub fn notify_proxy_config_changed(&self) {
        self.probe_notify.notify_one();
    }

    pub async fn settings_update_guard(&self) -> tokio::sync::MutexGuard<'_, ()> {
        self.settings_update_lock.lock().await
    }

    pub async fn proxy_configuration_changed(&self, proxy_id: i64) {
        self.probe_failures.lock().await.remove(&proxy_id);
        self.forced_probes.lock().await.insert(proxy_id);
        self.proxy_runtime.reset_proxy_state(proxy_id).await;
        self.notify_proxy_config_changed();
    }

    pub async fn proxy_deleted(&self, proxy_id: i64) {
        self.probe_failures.lock().await.remove(&proxy_id);
        self.forced_probes.lock().await.remove(&proxy_id);
        let mut locks = self.probe_locks.lock().await;
        if locks
            .get(&proxy_id)
            .is_some_and(|lock| Arc::strong_count(lock) == 1)
        {
            locks.remove(&proxy_id);
        }
        drop(locks);
        self.proxy_runtime.reset_proxy_state(proxy_id).await;
        self.notify_proxy_config_changed();
    }

    pub fn notify_advanced_config_changed(&self) {
        self.probe_notify.notify_one();
        self.dns_notify.notify_one();
    }

    fn spawn_dynamic_dns_refresh(&self) {
        let state = self.clone();
        tauri::async_runtime::spawn(async move {
            state.dynamic_dns_loop().await;
        });
    }

    async fn dynamic_dns_loop(self) {
        loop {
            if let Err(error) = self.refresh_dynamic_mappings().await {
                eprintln!("动态DNS刷新失败: {error:#}");
            }
            let interval = match self.dns_refresh_interval() {
                Ok(interval) => interval,
                Err(error) => {
                    eprintln!("动态DNS刷新配置无效，将在 30 秒后重试: {error:#}");
                    Duration::from_secs(30)
                }
            };
            tokio::select! {
                _ = time::sleep(interval) => {}
                _ = self.dns_notify.notified() => {}
            }
        }
    }

    /// 解析所有“启用+动态”的映射，IP 变化才写库并刷新代理 DNS 缓存，广播事件让前端同步。
    async fn refresh_dynamic_mappings(&self) -> Result<()> {
        let mappings = self.db.list_dynamic_dns_mappings()?;
        if mappings.is_empty() {
            return Ok(());
        }
        let mut changed = false;
        for mapping in mappings {
            let Some(ip) = resolve_ipv4(&mapping.domain).await else {
                eprintln!("动态DNS解析失败，保留原地址: {}", mapping.domain);
                continue;
            };
            if ip == mapping.ip {
                continue;
            }
            match self.db.update_dns_ip_if_unchanged(
                mapping.id,
                &mapping.domain,
                &mapping.ip,
                mapping.enabled,
                mapping.dynamic,
                &ip,
            ) {
                Ok(true) => {}
                Ok(false) => continue,
                Err(error) => {
                    eprintln!("动态DNS更新失败 {}: {error:#}", mapping.domain);
                    continue;
                }
            }
            changed = true;
            let updated = self.db.get_dns_mapping(mapping.id)?;
            self.emit(
                "dns_mapping_updated",
                json!({
                    "mapping": updated,
                    "previousIp": mapping.ip,
                    "ip": ip,
                    "dynamic": true
                }),
            );
        }
        if changed {
            self.proxy_runtime.refresh_dns_cache().await?;
        }
        Ok(())
    }

    fn dns_refresh_interval(&self) -> Result<Duration> {
        let config = self.db.load_advanced_config()?;
        let ms = config
            .get("dns_refresh_interval")
            .and_then(Value::as_u64)
            .ok_or_else(|| anyhow!("dns_refresh_interval 必须是数字"))?;
        if ms < 30_000 {
            return Err(anyhow!("dns_refresh_interval 不能小于 30000 毫秒"));
        }
        Ok(Duration::from_millis(ms))
    }

    fn spawn_log_maintenance(&self) {
        let state = self.clone();
        tauri::async_runtime::spawn(async move {
            loop {
                match state.log_retention_days() {
                    Ok(retention_days) => match state.db.prune_request_logs(retention_days) {
                        Ok(deleted) if deleted > 0 => {
                            eprintln!("已清理 {deleted} 条过期流量日志");
                        }
                        Ok(_) => {}
                        Err(error) => eprintln!("清理过期流量日志失败: {error:#}"),
                    },
                    Err(error) => {
                        eprintln!("日志保留配置无效，本轮未执行清理: {error:#}");
                    }
                }
                time::sleep(Duration::from_secs(60 * 60)).await;
            }
        });
    }

    fn log_retention_days(&self) -> Result<i64> {
        let config = self.db.load_advanced_config()?;
        let days = config
            .get("log_retention_days")
            .and_then(Value::as_i64)
            .ok_or_else(|| anyhow!("log_retention_days 必须是整数"))?;
        if days < 1 {
            return Err(anyhow!("log_retention_days 必须大于 0"));
        }
        Ok(days)
    }

    fn spawn_periodic_proxy_tests(&self) {
        let state = self.clone();
        tauri::async_runtime::spawn(async move {
            state.periodic_proxy_tests().await;
        });
    }

    async fn periodic_proxy_tests(self) {
        // 记录每个代理上一次“主动测活”的时间，配合真实流量的成功时间做自适应调度。
        let mut last_probe: HashMap<i64, i64> = HashMap::new();
        loop {
            let schedule = match self.probe_schedule() {
                Ok(schedule) => schedule,
                Err(error) => {
                    eprintln!("定期代理测试配置无效，将稍后重试: {error:#}");
                    time::sleep(Duration::from_secs(30)).await;
                    continue;
                }
            };
            if let Err(error) = self.run_probe_cycle(&schedule, &mut last_probe).await {
                eprintln!("定期代理测试失败: {error:#}");
            }
            tokio::select! {
                _ = time::sleep(schedule.tick) => {}
                _ = self.probe_notify.notified() => {}
            }
        }
    }

    /// 一轮自适应测活：
    /// - 近期有真实流量成功的代理直接跳过（真实流量已覆盖心跳）；
    /// - 活跃代理按基础间隔发起轻量心跳，失败/未知代理按更短的恢复间隔重测；
    /// - 到期代理并发测活，缩短整轮耗时。
    async fn run_probe_cycle(
        &self,
        schedule: &ProbeSchedule,
        last_probe: &mut HashMap<i64, i64>,
    ) -> Result<()> {
        let proxies = self.db.list_enabled_proxies()?;
        let live_ids: HashSet<i64> = proxies.iter().map(|proxy| proxy.id).collect();
        last_probe.retain(|id, _| live_ids.contains(id));
        self.probe_failures
            .lock()
            .await
            .retain(|id, _| live_ids.contains(id));
        let forced_probes = {
            let mut forced = self.forced_probes.lock().await;
            forced.retain(|id| live_ids.contains(id));
            std::mem::take(&mut *forced)
        };

        let recent_success = self.proxy_runtime.recent_success_map().await;
        let now = now_millis();
        let active_window = schedule.active_window.as_millis() as i64;
        let base_interval = schedule.base_interval.as_millis() as i64;
        let recovery_interval = schedule.recovery_interval.as_millis() as i64;

        let mut due = Vec::new();
        for proxy in proxies {
            if forced_probes.contains(&proxy.id) {
                due.push(proxy);
                continue;
            }
            let last_success = recent_success.get(&proxy.id).copied().unwrap_or(0);
            if last_success > 0 && now - last_success < active_window {
                continue;
            }
            let interval = if proxy.status.as_deref() == Some("active") {
                base_interval
            } else {
                recovery_interval
            };
            let due_now = last_probe
                .get(&proxy.id)
                .is_none_or(|last| now - last >= interval);
            if due_now {
                due.push(proxy);
            }
        }

        if due.is_empty() {
            return Ok(());
        }
        for proxy in &due {
            last_probe.insert(proxy.id, now);
        }

        let semaphore = Arc::new(Semaphore::new(schedule.concurrency));
        let mut tasks = JoinSet::new();
        for proxy in due {
            let Ok(permit) = semaphore.clone().acquire_owned().await else {
                break;
            };
            let state = self.clone();
            tasks.spawn(async move {
                let _permit = permit;
                if let Err(error) = state.test_proxy_record(proxy, ProbeOrigin::Periodic).await {
                    eprintln!("代理定期测试失败: {error:#}");
                }
            });
        }
        while let Some(result) = tasks.join_next().await {
            if let Err(error) = result {
                eprintln!("代理定期测试任务异常退出: {error}");
            }
        }
        Ok(())
    }

    async fn test_proxy_record(
        &self,
        scheduled_proxy: ProxyRecord,
        origin: ProbeOrigin,
    ) -> Result<Option<TestResult>> {
        let probe_lock = {
            let mut locks = self.probe_locks.lock().await;
            locks
                .entry(scheduled_proxy.id)
                .or_insert_with(|| Arc::new(Mutex::new(())))
                .clone()
        };
        let _probe_guard = probe_lock.lock().await;
        let Some(proxy) = self.db.get_proxy(scheduled_proxy.id)? else {
            return Ok(None);
        };
        if origin == ProbeOrigin::Periodic && proxy.enabled != 1 {
            return Ok(None);
        }

        let settings = self.db.settings_map()?;
        let global_url = settings
            .get("test_url")
            .cloned()
            .unwrap_or_else(|| "https://cms.zjzwfw.gov.cn/favicon.ico".to_string());
        let global_timeout_seconds = match settings.get("timeout") {
            Some(raw) => raw
                .parse::<u64>()
                .with_context(|| format!("默认测试超时不是有效整数: {raw:?}"))?,
            None => 10,
        };
        if !(1..=300).contains(&global_timeout_seconds) {
            return Err(anyhow!("默认测试超时必须在 1 到 300 秒之间"));
        }
        let global_timeout = global_timeout_seconds * 1000;
        let test_url = proxy.test_url.clone().unwrap_or(global_url);
        let timeout = proxy
            .test_timeout
            .and_then(|value| u64::try_from(value).ok())
            .map(|value| value * 1000)
            .unwrap_or(global_timeout);
        let target = Url::parse(&test_url).with_context(|| format!("测试地址无效: {test_url}"))?;
        if target.host_str().is_none() || !matches!(target.scheme(), "http" | "https") {
            return Err(anyhow!("测试地址必须是包含主机名的 HTTP 或 HTTPS URL"));
        }

        let probe_started_at = now_millis();
        if origin == ProbeOrigin::Manual {
            self.db
                .update_proxy_status(proxy.id, "testing", proxy.response_time, 0, 0)?;
            self.emit("proxy_testing", json!({ "id": proxy.id }));
        }

        let result = proxy_tester::test_proxy(&proxy, &test_url, timeout).await;
        let Some(latest) = self.db.get_proxy(proxy.id)? else {
            return Ok(Some(result));
        };
        if !same_probe_configuration(&proxy, &latest) {
            return Err(anyhow!("代理配置在测试期间发生变化，已丢弃旧测试结果"));
        }

        let recent_success = self.proxy_runtime.recent_success_map().await;
        let traffic_was_alive_before_apply =
            recent_success.get(&proxy.id).copied().unwrap_or(0) > probe_started_at;
        let desired_status = if result.success {
            self.probe_failures.lock().await.remove(&proxy.id);
            Some("active")
        } else if traffic_was_alive_before_apply {
            self.probe_failures.lock().await.remove(&proxy.id);
            None
        } else {
            let failures = {
                let mut failures = self.probe_failures.lock().await;
                let value = failures.entry(proxy.id).or_insert(0);
                *value = value.saturating_add(1);
                *value
            };
            let threshold = self.probe_failure_threshold()?;
            (origin == ProbeOrigin::Manual || failures >= threshold).then_some("inactive")
        };

        let record_result = self
            .proxy_runtime
            .record_probe_result(
                &proxy,
                probe_started_at,
                desired_status,
                result.success.then_some(result.response_time),
                result.success,
            )
            .await;
        let (applied_status, traffic_proved_alive) = match record_result {
            Ok(recorded) => recorded,
            Err(error) => {
                self.probe_failures.lock().await.remove(&proxy.id);
                return Err(error);
            }
        };
        if traffic_proved_alive {
            self.probe_failures.lock().await.remove(&proxy.id);
        }

        let updated = self.db.get_proxy(proxy.id)?;
        self.emit(
            "proxy_tested",
            json!({
                "proxy": updated,
                "result": result,
                "statusApplied": applied_status.is_some(),
                "trafficProvedAlive": traffic_proved_alive
            }),
        );
        Ok(Some(result))
    }

    fn probe_failure_threshold(&self) -> Result<u32> {
        let config = self.db.load_advanced_config()?;
        let value = config
            .get("probe_failure_threshold")
            .and_then(Value::as_u64)
            .ok_or_else(|| anyhow!("probe_failure_threshold 必须是正整数"))?;
        let value = u32::try_from(value).context("probe_failure_threshold 超出有效范围")?;
        if value == 0 {
            return Err(anyhow!("probe_failure_threshold 必须大于 0"));
        }
        Ok(value)
    }

    fn probe_schedule(&self) -> Result<ProbeSchedule> {
        let config = self.db.load_advanced_config()?;
        let base_ms = match config.get("periodic_test_interval") {
            Some(value) => value
                .as_u64()
                .ok_or_else(|| anyhow!("periodic_test_interval 必须是数字"))?,
            None => 3 * 60 * 1000,
        };
        if base_ms == 0 {
            return Err(anyhow!("periodic_test_interval 必须大于 0"));
        }
        let recovery_ms = config
            .get("probe_recovery_interval")
            .and_then(Value::as_u64)
            .ok_or_else(|| anyhow!("probe_recovery_interval 必须是数字"))?;
        if recovery_ms == 0 {
            return Err(anyhow!("probe_recovery_interval 必须大于 0"));
        }
        let concurrency = config
            .get("probe_concurrency")
            .and_then(Value::as_u64)
            .ok_or_else(|| anyhow!("probe_concurrency 必须是数字"))?;
        if !(1..=64).contains(&concurrency) {
            return Err(anyhow!("probe_concurrency 必须在 1 到 64 之间"));
        }
        Ok(ProbeSchedule {
            base_interval: Duration::from_millis(base_ms),
            recovery_interval: Duration::from_millis(recovery_ms),
            // 真实流量在基础间隔内成功即视为“新鲜”，可跳过主动心跳。
            active_window: Duration::from_millis(base_ms),
            concurrency: concurrency as usize,
            tick: Duration::from_millis(base_ms.min(recovery_ms)),
        })
    }
}

fn spawn_proxy_server(runtime: Arc<ProxyRuntime>, proxy_host: String, proxy_port: u16) {
    tauri::async_runtime::spawn(async move {
        loop {
            if let Err(error) = proxy::serve(runtime.clone(), proxy_host.clone(), proxy_port).await
            {
                eprintln!(
                    "代理服务监听失败，将在 {} 秒后重试: {error:#}",
                    PROXY_LISTEN_RETRY_INTERVAL.as_secs()
                );
                time::sleep(PROXY_LISTEN_RETRY_INTERVAL).await;
                continue;
            }
            break;
        }
    });
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum ProbeOrigin {
    Manual,
    Periodic,
}

fn same_probe_configuration(left: &ProxyRecord, right: &ProxyRecord) -> bool {
    left.proxy_type == right.proxy_type
        && left.host == right.host
        && left.port == right.port
        && left.username == right.username
        && left.password == right.password
        && left.enabled == right.enabled
        && left.test_url == right.test_url
        && left.test_timeout == right.test_timeout
        && left.skip_cert_verify == right.skip_cert_verify
}

struct ProbeSchedule {
    base_interval: Duration,
    recovery_interval: Duration,
    active_window: Duration,
    concurrency: usize,
    tick: Duration,
}

pub fn now_millis() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_millis() as i64)
        .unwrap_or_default()
}

/// 用系统解析器（getaddrinfo，与命令行 ping/nslookup 一致）解析域名，取首个 IPv4。
pub async fn resolve_ipv4(domain: &str) -> Option<String> {
    let host = domain.trim();
    if host.is_empty() {
        return None;
    }
    let addrs = time::timeout(
        Duration::from_secs(10),
        tokio::net::lookup_host((host, 0u16)),
    )
    .await
    .ok()?
    .ok()?;
    for addr in addrs {
        if let std::net::IpAddr::V4(ipv4) = addr.ip() {
            return Some(ipv4.to_string());
        }
    }
    None
}
