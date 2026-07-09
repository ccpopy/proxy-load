use std::{
    collections::{HashMap, HashSet},
    sync::Arc,
    time::{Duration, SystemTime, UNIX_EPOCH},
};

use anyhow::{anyhow, Context, Result};
use serde::Serialize;
use serde_json::{json, Value};
use tokio::{sync::broadcast, sync::Semaphore, task::JoinSet, time};
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
    pub proxy_port: u16,
    pub proxy_runtime: Arc<ProxyRuntime>,
}

#[derive(Debug, Clone, Serialize)]
pub struct ServiceInfo {
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
        let proxy_runtime = Arc::new(ProxyRuntime::new(db.clone(), events.clone(), proxy_port));

        spawn_proxy_server(proxy_runtime.clone(), proxy_port);

        let state = Self {
            db,
            events,
            started_at,
            proxy_port,
            proxy_runtime,
        };
        state.spawn_periodic_proxy_tests();
        state.spawn_dynamic_dns_refresh();
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
        self.test_proxy_record(proxy).await
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
            time::sleep(self.dns_refresh_interval()).await;
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
                continue;
            };
            if ip == mapping.ip {
                continue;
            }
            if let Err(error) = self.db.update_dns_ip(mapping.id, &ip) {
                eprintln!("动态DNS更新失败 {}: {error:#}", mapping.domain);
                continue;
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

    fn dns_refresh_interval(&self) -> Duration {
        let ms = self
            .db
            .load_advanced_config()
            .ok()
            .and_then(|config| config.get("dns_refresh_interval").and_then(Value::as_u64))
            .filter(|value| *value > 0)
            .unwrap_or(5 * 60 * 1000);
        Duration::from_millis(ms)
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
                    eprintln!("定期代理测试停止: {error:#}");
                    break;
                }
            };
            if let Err(error) = self.run_probe_cycle(&schedule, &mut last_probe).await {
                eprintln!("定期代理测试失败: {error:#}");
            }
            time::sleep(schedule.tick).await;
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

        let recent_success = self.proxy_runtime.recent_success_map().await;
        let now = now_millis();
        let active_window = schedule.active_window.as_millis() as i64;
        let base_interval = schedule.base_interval.as_millis() as i64;
        let recovery_interval = schedule.recovery_interval.as_millis() as i64;

        let mut due = Vec::new();
        for proxy in proxies {
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
                .map_or(true, |last| now - last >= interval);
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
                if let Err(error) = state.test_proxy_record(proxy).await {
                    eprintln!("代理定期测试失败: {error:#}");
                }
            });
        }
        while tasks.join_next().await.is_some() {}
        Ok(())
    }

    async fn test_proxy_record(&self, proxy: ProxyRecord) -> Result<TestResult> {
        let settings = self.db.settings_map()?;
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
        let target = Url::parse(&test_url).with_context(|| format!("测试地址无效: {test_url}"))?;
        let target_host = target.host_str().unwrap_or_default().to_string();
        let target_port = target.port_or_known_default().unwrap_or(80);

        self.db
            .update_proxy_status(proxy.id, "testing", None, 0, 0)?;
        self.emit("proxy_testing", json!({ "id": proxy.id }));

        let result = proxy_tester::test_proxy(&proxy, &test_url, timeout).await;
        if result.success {
            self.db
                .update_proxy_status(proxy.id, "active", Some(result.response_time), 1, 0)?;
            self.proxy_runtime.record_probe_success(proxy.id).await;
            self.db.log_request(
                Some(proxy.id),
                &target_host,
                i64::from(target_port),
                true,
                Some(result.response_time),
                None,
                "health_success",
            )?;
        } else {
            self.db
                .update_proxy_status(proxy.id, "inactive", None, 0, 1)?;
            self.db.log_request(
                Some(proxy.id),
                &target_host,
                i64::from(target_port),
                false,
                None,
                result.error.as_deref(),
                "health_failure",
            )?;
        }

        let updated = self.db.get_proxy(proxy.id)?;
        self.proxy_runtime
            .note_pushed_status(proxy.id, if result.success { "active" } else { "inactive" })
            .await;
        self.emit(
            "proxy_tested",
            json!({
                "proxy": updated,
                "result": result
            }),
        );
        Ok(result)
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
            .unwrap_or(60 * 1000)
            .clamp(1, base_ms);
        let concurrency = config
            .get("probe_concurrency")
            .and_then(Value::as_u64)
            .unwrap_or(8)
            .clamp(1, 64) as usize;
        Ok(ProbeSchedule {
            base_interval: Duration::from_millis(base_ms),
            recovery_interval: Duration::from_millis(recovery_ms),
            // 真实流量在基础间隔内成功即视为“新鲜”，可跳过主动心跳。
            active_window: Duration::from_millis(base_ms),
            concurrency,
            tick: Duration::from_millis(recovery_ms),
        })
    }
}

fn spawn_proxy_server(runtime: Arc<ProxyRuntime>, proxy_port: u16) {
    tauri::async_runtime::spawn(async move {
        loop {
            if let Err(error) = proxy::serve(runtime.clone(), proxy_port).await {
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
    let addrs = tokio::net::lookup_host((host, 0u16)).await.ok()?;
    for addr in addrs {
        if let std::net::IpAddr::V4(ipv4) = addr.ip() {
            return Some(ipv4.to_string());
        }
    }
    None
}
