use std::{
    collections::{HashMap, HashSet, VecDeque},
    net::{Ipv4Addr, SocketAddr},
    sync::{Arc, Mutex as SyncMutex, RwLock as SyncRwLock},
    time::Instant,
};

use anyhow::{anyhow, Context, Result};
use base64::{engine::general_purpose, Engine as _};
use serde::Serialize;
use serde_json::{json, Map, Value};
use tokio::{
    io::{self, AsyncReadExt, AsyncWriteExt},
    net::{TcpListener, TcpStream},
    sync::{broadcast, Mutex, RwLock},
    time::{timeout, Duration},
};

use crate::{
    database::{Database, RequestLogEntry},
    models::{ProxyRecord, ServerEvent},
    state::now_millis,
};

const SOCKS_VERSION: u8 = 0x05;
const SOCKS_CMD_CONNECT: u8 = 0x01;
const ADDR_IPV4: u8 = 0x01;
const ADDR_DOMAIN: u8 = 0x03;
const SOCKS_AUTH_NONE: u8 = 0x00;
const SOCKS_AUTH_USERNAME_PASSWORD: u8 = 0x02;
const SOCKS_AUTH_REJECTED: u8 = 0xff;
const METRICS_WINDOW_MS: i64 = 5 * 60 * 1000;
const MAX_TARGET_CIRCUITS: usize = 4096;
const MAX_METRIC_SAMPLES: usize = 2048;
mod http_observer;
mod sticky_routes;
mod telemetry;

#[derive(Clone)]
pub struct ProxyRuntime {
    db: Database,
    database_worker: crate::database_worker::DatabaseWorker,
    telemetry: Arc<telemetry::Telemetry>,
    events: broadcast::Sender<ServerEvent>,
    service_status: Arc<RwLock<ProxyServiceStatus>>,
    metrics: Arc<SyncRwLock<HashMap<i64, ProxyMetrics>>>,
    circuit_breakers: Arc<SyncRwLock<HashMap<i64, CircuitBreaker>>>,
    target_circuits: Arc<SyncRwLock<HashMap<TargetRouteKey, TargetCircuit>>>,
    active_connections: Arc<std::sync::Mutex<HashMap<i64, i64>>>,
    connection_slots: Arc<tokio::sync::Semaphore>,
    handshake_slots: Arc<tokio::sync::Semaphore>,
    global_dial_slots: Arc<tokio::sync::Semaphore>,
    dial_slots: Arc<SyncMutex<HashMap<i64, Arc<tokio::sync::Semaphore>>>>,
    dial_ready: Arc<tokio::sync::Notify>,
    dns_cache: Arc<RwLock<HashMap<String, String>>>,
    round_robin_index: Arc<SyncMutex<HashMap<i64, i64>>>,
    adaptive_sequence: Arc<SyncMutex<HashMap<i64, u64>>>,
    sticky_routes: Arc<SyncMutex<sticky_routes::StickyRoutes>>,
    selection_lock: Arc<SyncMutex<()>>,
    runtime_settings: Arc<SyncRwLock<RuntimeSettings>>,
    status_locks: Arc<Mutex<HashMap<i64, Arc<Mutex<()>>>>>,
    shutdown: tokio::sync::watch::Sender<bool>,
    #[cfg(test)]
    injected_connect_errors: Arc<SyncMutex<HashMap<i64, std::io::ErrorKind>>>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ProxyServiceStatus {
    pub state: String,
    pub running: bool,
    pub host: String,
    pub port: u16,
    pub error: Option<String>,
}

#[derive(Debug, Clone)]
struct TargetRequest {
    host: String,
    port: u16,
    address_type: u8,
    original_host: String,
    inbound: InboundProtocol,
    initial_payload: Vec<u8>,
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
struct TargetRouteKey {
    proxy_id: i64,
    original_host: String,
    resolved_host: String,
    port: u16,
}

impl TargetRouteKey {
    fn new(proxy_id: i64, request: &TargetRequest) -> Self {
        Self {
            proxy_id,
            original_host: crate::routing::normalize_host(&request.original_host),
            resolved_host: crate::routing::normalize_host(&request.host),
            port: request.port,
        }
    }
}

struct TargetCircuit {
    breaker: CircuitBreaker,
    last_failure: i64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum FailureScope {
    Proxy,
    Target,
    LocalRoute,
}

impl FailureScope {
    fn label(self) -> &'static str {
        match self {
            Self::Proxy => "代理连接/认证",
            Self::Target => "目标链路",
            Self::LocalRoute => "本地路由不可达",
        }
    }
}

struct ConnectedUpstream {
    stream: TcpStream,
    outbound_initial_payload: Option<Vec<u8>>,
    prefetched_response: Vec<u8>,
    target_verified: bool,
    proxy_latency_us: u64,
}

struct ConnectionLease {
    proxy: ProxyRecord,
    generation: u64,
    active: Arc<std::sync::Mutex<HashMap<i64, i64>>>,
    attempt: Arc<()>,
    dial_permit: Option<tokio::sync::OwnedSemaphorePermit>,
    global_dial_permit: Option<tokio::sync::OwnedSemaphorePermit>,
    failover_policy: Option<sticky_routes::Policy>,
    dial_ready: Arc<tokio::sync::Notify>,
}
impl std::ops::Deref for ConnectionLease {
    type Target = ProxyRecord;
    fn deref(&self) -> &ProxyRecord {
        &self.proxy
    }
}
impl Drop for ConnectionLease {
    fn drop(&mut self) {
        let had_permit = self.dial_permit.take().is_some();
        let had_global_permit = self.global_dial_permit.take().is_some();
        if had_permit || had_global_permit {
            self.dial_ready.notify_waiters();
        }
        let mut active = self.active.lock().unwrap_or_else(|e| e.into_inner());
        if let Some(count) = active.get_mut(&self.proxy.id) {
            *count -= 1;
            if *count <= 0 {
                active.remove(&self.proxy.id);
            }
        }
    }
}

pub struct ProxyRuntimeStats {
    pub score: f64,
    pub active_connections: i64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum InboundProtocol {
    Socks5,
    HttpConnect,
    HttpForward,
}

#[derive(Debug, Clone)]
struct ProxyMetrics {
    requests: VecDeque<RequestMetric>,
    score: f64,
    last_used: i64,
    last_success: i64,
    pushed_status: Option<String>,
    successes: i64,
    time_sum: u64,
    time_count: u64,
    learning_remaining: u8,
}

#[derive(Debug, Clone)]
struct RequestMetric {
    timestamp: i64,
    success: bool,
    latency_us: Option<u64>,
}

#[derive(Debug, Clone, Serialize)]
struct CircuitBreaker {
    state: String,
    failures: i64,
    threshold: i64,
    timeout_ms: i64,
    next_attempt: i64,
    #[serde(skip)]
    attempt: Option<std::sync::Weak<()>>,
}

#[derive(Clone, Default)]
struct InboundAuth {
    enabled: bool,
    username: String,
    password: String,
}

#[derive(Debug, Clone, Copy)]
struct CircuitConfig {
    failure_threshold: i64,
    timeout_ms: i64,
}

#[derive(Clone)]
struct RuntimeSettings {
    inbound_auth: InboundAuth,
    circuit: CircuitConfig,
    fail_fast: FailFastConfig,
    algorithm: String,
}

#[derive(Debug, Clone, Copy)]
struct FailFastConfig {
    enabled: bool,
    max_attempts: usize,
    attempt_timeout_ms: u64,
    total_timeout_ms: u64,
}

impl RuntimeSettings {
    fn from_advanced(config: &Value) -> Result<Self> {
        let inbound_auth = InboundAuth {
            enabled: config
                .get("inbound_auth_enabled")
                .and_then(Value::as_bool)
                .ok_or_else(|| anyhow!("inbound_auth_enabled 必须是布尔值"))?,
            username: config
                .get("inbound_auth_username")
                .and_then(Value::as_str)
                .ok_or_else(|| anyhow!("inbound_auth_username 必须是字符串"))?
                .to_string(),
            password: config
                .get("inbound_auth_password")
                .and_then(Value::as_str)
                .ok_or_else(|| anyhow!("inbound_auth_password 必须是字符串"))?
                .to_string(),
        };
        if inbound_auth.enabled
            && (inbound_auth.username.is_empty() || inbound_auth.password.is_empty())
        {
            return Err(anyhow!("启用入站认证时用户名和密码不能为空"));
        }

        let circuit = CircuitConfig {
            failure_threshold: positive_i64_setting(config, "circuit_failure_threshold")?,
            timeout_ms: positive_i64_setting(config, "circuit_timeout")?,
        };
        let attempt_timeout = positive_i64_setting(config, "failfast_attempt_timeout")?;
        let total_timeout = positive_i64_setting(config, "failfast_total_timeout")?;
        if total_timeout < attempt_timeout {
            return Err(anyhow!("failfast_total_timeout 不能小于单次连接超时"));
        }
        let fail_fast = FailFastConfig {
            enabled: config
                .get("failfast_enabled")
                .and_then(Value::as_bool)
                .ok_or_else(|| anyhow!("failfast_enabled 必须是布尔值"))?,
            max_attempts: usize::try_from(positive_i64_setting(config, "failfast_max_attempts")?)
                .context("failfast_max_attempts 超出有效范围")?,
            attempt_timeout_ms: u64::try_from(attempt_timeout)
                .context("failfast_attempt_timeout 超出有效范围")?,
            total_timeout_ms: u64::try_from(total_timeout)
                .context("failfast_total_timeout 超出有效范围")?,
        };

        Ok(Self {
            inbound_auth,
            circuit,
            fail_fast,
            algorithm: "adaptive".to_string(),
        })
    }

    fn set_algorithm(&mut self, algorithm: &str) -> Result<()> {
        self.algorithm = match algorithm {
            "adaptive" | "round_robin" | "least_connections" | "sticky_host" => algorithm,
            "weighted_round_robin" => "round_robin",
            other => return Err(anyhow!("不支持的代理选择算法: {other}")),
        }
        .to_string();
        Ok(())
    }
}

fn positive_i64_setting(config: &Value, key: &str) -> Result<i64> {
    let value = config
        .get(key)
        .and_then(Value::as_i64)
        .ok_or_else(|| anyhow!("{key} 必须是整数"))?;
    if value <= 0 {
        return Err(anyhow!("{key} 必须大于 0"));
    }
    Ok(value)
}

impl ProxyRuntime {
    pub fn new(
        db: Database,
        events: broadcast::Sender<ServerEvent>,
        listen_host: &str,
        listen_port: u16,
        advanced: &Value,
    ) -> Result<Self> {
        let mut runtime_settings = RuntimeSettings::from_advanced(advanced)?;
        if let Some(algorithm) = db.settings_map()?.get("algorithm") {
            runtime_settings.set_algorithm(algorithm)?;
        }
        let database_worker =
            crate::database_worker::DatabaseWorker::new(db.clone(), events.clone())?;
        Ok(Self {
            db,
            database_worker,
            telemetry: Arc::new(telemetry::Telemetry::default()),
            events,
            service_status: Arc::new(RwLock::new(ProxyServiceStatus {
                state: "starting".to_string(),
                running: false,
                host: listen_host.to_string(),
                port: listen_port,
                error: Some("代理服务正在启动".to_string()),
            })),
            metrics: Arc::new(SyncRwLock::new(HashMap::new())),
            circuit_breakers: Arc::new(SyncRwLock::new(HashMap::new())),
            target_circuits: Arc::new(SyncRwLock::new(HashMap::new())),
            active_connections: Arc::new(std::sync::Mutex::new(HashMap::new())),
            connection_slots: Arc::new(tokio::sync::Semaphore::new(1024)),
            handshake_slots: Arc::new(tokio::sync::Semaphore::new(128)),
            global_dial_slots: Arc::new(tokio::sync::Semaphore::new(64)),
            dial_slots: Arc::new(SyncMutex::new(HashMap::new())),
            dial_ready: Arc::new(tokio::sync::Notify::new()),
            dns_cache: Arc::new(RwLock::new(HashMap::new())),
            round_robin_index: Arc::new(SyncMutex::new(HashMap::new())),
            adaptive_sequence: Arc::new(SyncMutex::new(HashMap::new())),
            sticky_routes: Arc::new(SyncMutex::new(sticky_routes::StickyRoutes::default())),
            selection_lock: Arc::new(SyncMutex::new(())),
            runtime_settings: Arc::new(SyncRwLock::new(runtime_settings)),
            status_locks: Arc::new(Mutex::new(HashMap::new())),
            shutdown: tokio::sync::watch::channel(false).0,
            #[cfg(test)]
            injected_connect_errors: Arc::new(SyncMutex::new(HashMap::new())),
        })
    }

    pub async fn service_status(&self) -> ProxyServiceStatus {
        self.service_status.read().await.clone()
    }

    pub fn flush_logs(&self, budget: Duration) -> bool {
        self.database_worker.flush(budget)
    }
    pub fn request_stop(&self) {
        self.shutdown.send_replace(true);
    }
    pub fn is_stopping(&self) -> bool {
        *self.shutdown.borrow()
    }
    pub async fn cancelled(&self) {
        let mut signal = self.shutdown.subscribe();
        let _ = signal.wait_for(|stopping| *stopping).await;
    }
    pub fn stop_and_flush_logs(&self, budget: Duration) -> bool {
        self.request_stop();
        self.database_worker.seal_and_flush(budget)
    }
    pub fn database_stats(&self) -> Value {
        let mut stats = self.database_worker.stats();
        stats["timings"] = self.telemetry.snapshot();
        stats
    }

    async fn set_service_status(&self, status: ProxyServiceStatus) {
        *self.service_status.write().await = status.clone();
        let _ = self.events.send(ServerEvent {
            event_type: "proxy_service_status_changed".to_string(),
            data: json!(status),
            timestamp: now_millis(),
        });
    }

    pub async fn refresh_dns_cache(&self) -> Result<()> {
        let mappings = self
            .db
            .active_dns_mappings()?
            .into_iter()
            .map(|(host, ip)| (crate::routing::normalize_host(&host), ip))
            .collect();
        *self.dns_cache.write().await = mappings;
        Ok(())
    }

    pub async fn update_advanced_config(&self, advanced: &Value) -> Result<()> {
        let mut current = self
            .runtime_settings
            .write()
            .unwrap_or_else(|e| e.into_inner());
        let mut next = RuntimeSettings::from_advanced(advanced)?;
        next.algorithm = current.algorithm.clone();
        let circuit = next.circuit;
        *current = next;
        drop(current);
        for breaker in self
            .circuit_breakers
            .write()
            .unwrap_or_else(|e| e.into_inner())
            .values_mut()
        {
            breaker.apply_config(circuit);
        }
        for target in self
            .target_circuits
            .write()
            .unwrap_or_else(|e| e.into_inner())
            .values_mut()
        {
            target.breaker.apply_config(circuit);
        }
        Ok(())
    }

    pub async fn update_load_settings(&self, settings: &Map<String, Value>) -> Result<()> {
        if let Some(algorithm) = settings.get("algorithm").and_then(Value::as_str) {
            self.runtime_settings
                .write()
                .unwrap_or_else(|e| e.into_inner())
                .set_algorithm(algorithm)?;
            *self.sticky_routes.lock().unwrap_or_else(|e| e.into_inner()) =
                sticky_routes::StickyRoutes::default();
        }
        Ok(())
    }

    pub async fn reset_proxy_state(&self, proxy_id: i64) {
        self.dial_slots
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .remove(&proxy_id);
        self.metrics
            .write()
            .unwrap_or_else(|e| e.into_inner())
            .remove(&proxy_id);
        self.circuit_breakers
            .write()
            .unwrap_or_else(|e| e.into_inner())
            .remove(&proxy_id);
        self.target_circuits
            .write()
            .unwrap_or_else(|e| e.into_inner())
            .retain(|key, _| key.proxy_id != proxy_id);
    }

    fn proxy_configuration_is_current(&self, proxy: &ProxyRecord) -> Result<bool> {
        Ok(self
            .db
            .routing_snapshot()
            .proxies
            .iter()
            .any(|latest| latest.id == proxy.id && same_routing_configuration(proxy, latest)))
    }

    async fn inbound_auth(&self) -> InboundAuth {
        self.runtime_settings
            .read()
            .unwrap_or_else(|e| e.into_inner())
            .inbound_auth
            .clone()
    }

    pub async fn stats(&self) -> HashMap<i64, ProxyRuntimeStats> {
        let mut metrics = self.metrics.write().unwrap_or_else(|e| e.into_inner());
        for metric in metrics.values_mut() {
            metric.prune(monotonic_millis());
        }
        let active = self
            .active_connections
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        let mut stats = HashMap::with_capacity(metrics.len().max(active.len()));
        for (proxy_id, metric) in metrics.iter() {
            stats.insert(
                *proxy_id,
                ProxyRuntimeStats {
                    score: (metric.score * 100.0).round() / 100.0,
                    active_connections: active.get(proxy_id).copied().unwrap_or(0),
                },
            );
        }
        for (proxy_id, active_connections) in active.iter() {
            stats.entry(*proxy_id).or_insert(ProxyRuntimeStats {
                score: 50.0,
                active_connections: *active_connections,
            });
        }
        stats
    }

    pub async fn circuit_breaker_stats(&self) -> Vec<Value> {
        let breakers = self
            .circuit_breakers
            .read()
            .unwrap_or_else(|e| e.into_inner());
        breakers
            .iter()
            .map(|(proxy_id, breaker)| {
                json!({
                    "proxyId": proxy_id,
                    "state": breaker.state,
                    "failures": breaker.failures,
                    "canAttempt": breaker.can_attempt_snapshot()
                })
            })
            .collect()
    }

    fn increment_active(&self, proxy_id: i64) {
        let mut active = self
            .active_connections
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        *active.entry(proxy_id).or_insert(0) += 1;
    }

    #[cfg(test)]
    async fn decrement_active(&self, proxy_id: i64) {
        let mut active = self
            .active_connections
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        match active.get_mut(&proxy_id) {
            Some(value) if *value > 1 => *value -= 1,
            Some(_) => {
                active.remove(&proxy_id);
            }
            None => {}
        }
    }

    // 调用方持有代理状态锁，并已校验配置仍然有效。
    async fn record_connection_success_locked(&self, proxy_id: i64, latency_us: u64) {
        let mark_active = {
            let mut metrics = self.metrics.write().unwrap_or_else(|e| e.into_inner());
            let metric = metrics.entry(proxy_id).or_insert_with(ProxyMetrics::new);
            let changed = metric.pushed_status.as_deref() != Some("active");
            if metric.pushed_status.as_deref() == Some("inactive") {
                metric.learning_remaining = 3;
            }
            metric.push(true, Some(latency_us));
            metric.pushed_status = Some("active".to_string());
            changed
        };
        if mark_active {
            // SQLite/UI remain millisecond-compatible; routing keeps microseconds.
            self.apply_passive_status_locked(
                proxy_id,
                "active",
                Some((latency_us.min(86_400_000_000) / 1000) as i64),
            );
        }
    }

    async fn record_request(&self, entry: RequestLogEntry<'_>) {
        self.database_worker.log(entry);
    }

    #[cfg(test)]
    fn select_proxies(
        &self,
        request: &TargetRequest,
        excluded: &HashSet<i64>,
    ) -> Result<Vec<ProxyRecord>> {
        self.select_from_snapshot(&self.db.routing_snapshot(), request, excluded, true)
    }

    fn select_from_snapshot(
        &self,
        snapshot: &crate::routing::RoutingSnapshot,
        request: &TargetRequest,
        excluded: &HashSet<i64>,
        commit_selection: bool,
    ) -> Result<Vec<ProxyRecord>> {
        let mut proxies = snapshot.proxies.clone();
        if proxies.is_empty() {
            return Err(anyhow!("没有可用的代理"));
        }

        let group_key = crate::routing::normalize_host(&request.original_host);
        let mut pool_id = 0;
        if let Some(selection) = snapshot.pool(&group_key) {
            pool_id = selection.id;
            proxies.retain(|proxy| selection.members.contains(&proxy.id));
            if proxies.is_empty() {
                return Err(anyhow!(
                    "目标 {} 匹配的代理分组「{}」没有可用代理",
                    group_key,
                    selection.name
                ));
            }
        }
        self.round_robin_index
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .retain(|id, _| *id == 0 || snapshot.pools.iter().any(|pool| pool.id == *id));

        let mut algorithm = self
            .runtime_settings
            .read()
            .unwrap_or_else(|e| e.into_inner())
            .algorithm
            .clone();
        if let Some(override_algorithm) = snapshot
            .pool(&group_key)
            .and_then(|pool| pool.algorithm_override.as_ref())
        {
            algorithm.clone_from(override_algorithm);
        }

        let mut eligible = Vec::new();
        {
            // Normalize the route once and acquire each read lock once per pool,
            // not three locks and two IDNA conversions per candidate.
            let base_key = TargetRouteKey::new(0, request);
            let metrics = self.metrics.read().unwrap_or_else(|e| e.into_inner());
            let breakers = self
                .circuit_breakers
                .read()
                .unwrap_or_else(|e| e.into_inner());
            let targets = self
                .target_circuits
                .read()
                .unwrap_or_else(|e| e.into_inner());
            for mut proxy in proxies {
                if let Some(status) = metrics.get(&proxy.id).and_then(|m| m.pushed_status.clone()) {
                    proxy.status = Some(status);
                }
                let key = TargetRouteKey {
                    proxy_id: proxy.id,
                    ..base_key.clone()
                };
                if excluded.contains(&proxy.id)
                    || !breakers
                        .get(&proxy.id)
                        .is_none_or(CircuitBreaker::can_attempt_snapshot)
                    || !targets
                        .get(&key)
                        .is_none_or(|target| target.breaker.can_attempt_snapshot())
                {
                    continue;
                }
                eligible.push(proxy);
            }
        }

        let mut ordered = self.order_proxies_in_pool(
            eligible,
            &algorithm,
            &group_key,
            pool_id,
            commit_selection,
        )?;
        if let Some(policy) = sticky_routes::Policy::for_request(snapshot, request, &algorithm) {
            self.sticky_routes
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .prefer(&policy, snapshot, &mut ordered);
        }
        Ok(ordered)
    }

    async fn reserve_proxy(
        &self,
        request: &TargetRequest,
        excluded: &HashSet<i64>,
    ) -> Result<Option<ConnectionLease>> {
        loop {
            // Tokio's semaphore queues global capacity waiters fairly. Waking
            // every global waiter for each released slot causes contention and
            // lets recently scheduled requests repeatedly overtake older ones.
            let queued = Instant::now();
            let global_dial_permit = self.global_dial_slots.clone().acquire_owned().await?;
            self.telemetry.record(
                "global_dial_queue_wait",
                queued.elapsed().as_micros() as u64,
            );
            // Register before looking at capacity so release cannot be missed.
            let ready = self.dial_ready.notified();
            tokio::pin!(ready);
            ready.as_mut().enable();
            let (lease, saturated) =
                self.try_reserve_proxy(request, excluded, global_dial_permit)?;
            if lease.is_some() || !saturated {
                return Ok(lease);
            }
            // Capacity waits remain asynchronous and bounded by the caller's deadline.
            let queued = Instant::now();
            ready.await;
            self.telemetry
                .record("dial_queue_wait", queued.elapsed().as_micros() as u64);
        }
    }

    // Pure in-memory selection + reservation. No await, SQL, network or filesystem
    // operations may be added while this short lock is held.
    fn try_reserve_proxy(
        &self,
        request: &TargetRequest,
        excluded: &HashSet<i64>,
        global_dial_permit: tokio::sync::OwnedSemaphorePermit,
    ) -> Result<(Option<ConnectionLease>, bool)> {
        let wait = Instant::now();
        let _selection = self
            .selection_lock
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        self.telemetry
            .record("selection_wait", wait.elapsed().as_micros() as u64);
        let selection_started = Instant::now();
        let snapshot = self.db.routing_snapshot();
        let mut saturated = false;
        let mut unavailable = excluded.clone();
        {
            let slots = self.dial_slots.lock().unwrap_or_else(|e| e.into_inner());
            for (id, capacity) in slots.iter() {
                if capacity.available_permits() == 0 {
                    unavailable.insert(*id);
                }
            }
        }
        // Remember whether THIS pool has eligible but temporarily full nodes.
        let candidates = self.select_from_snapshot(&snapshot, request, excluded, false)?;
        saturated |= candidates
            .iter()
            .any(|proxy| unavailable.contains(&proxy.id));
        let group_key = crate::routing::normalize_host(&request.original_host);
        let pool = snapshot.pool(&group_key);
        let algorithm = pool
            .and_then(|pool| pool.algorithm_override.clone())
            .unwrap_or_else(|| {
                self.runtime_settings
                    .read()
                    .unwrap_or_else(|e| e.into_inner())
                    .algorithm
                    .clone()
            });
        let failover_policy = sticky_routes::Policy::for_request(&snapshot, request, &algorithm)
            .filter(|policy| {
                !candidates.iter().any(|proxy| {
                    proxy.id == policy.preferred && proxy.status.as_deref() != Some("inactive")
                })
            });
        loop {
            let mut candidates =
                self.select_from_snapshot(&snapshot, request, &unavailable, false)?;
            if candidates.is_empty() {
                break;
            }
            let proxy = candidates.swap_remove(0);
            unavailable.insert(proxy.id);
            let Some(generation) = snapshot.node_generations.get(&proxy.id).copied() else {
                continue;
            };
            if self.db.routing_snapshot().node_generations.get(&proxy.id) != Some(&generation) {
                continue;
            }
            let slots = self
                .dial_slots
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .entry(proxy.id)
                .or_insert_with(|| Arc::new(tokio::sync::Semaphore::new(32)))
                .clone();
            let Ok(permit) = slots.try_acquire_owned() else {
                saturated = true;
                continue;
            };
            let token = Arc::new(());
            if self.try_begin_attempt_token(proxy.id, request, Some(&token)) {
                self.commit_pool_selection(pool.map_or(0, |pool| pool.id), &algorithm, proxy.id);
                self.increment_active(proxy.id);
                self.telemetry.record(
                    "selection_hold",
                    selection_started.elapsed().as_micros() as u64,
                );
                return Ok((
                    Some(ConnectionLease {
                        proxy,
                        generation,
                        active: self.active_connections.clone(),
                        attempt: token,
                        dial_permit: Some(permit),
                        global_dial_permit: Some(global_dial_permit),
                        failover_policy,
                        dial_ready: self.dial_ready.clone(),
                    }),
                    false,
                ));
            }
        }
        self.telemetry.record(
            "selection_hold",
            selection_started.elapsed().as_micros() as u64,
        );
        drop(global_dial_permit);
        Ok((None, saturated))
    }

    #[cfg(test)]
    fn order_proxies(
        &self,
        proxies: Vec<ProxyRecord>,
        algorithm: &str,
        host_key: &str,
    ) -> Result<Vec<ProxyRecord>> {
        self.order_proxies_in_pool(proxies, algorithm, host_key, 0, true)
    }

    fn commit_pool_selection(&self, pool_id: i64, algorithm: &str, proxy_id: i64) {
        let advance_cursor = match algorithm {
            "least_connections" | "round_robin" => true,
            "sticky_host" => false,
            _ => {
                let mut sequences = self
                    .adaptive_sequence
                    .lock()
                    .unwrap_or_else(|e| e.into_inner());
                let sequence = sequences.entry(pool_id).or_default();
                *sequence = sequence.wrapping_add(1);
                sequence.is_multiple_of(16)
            }
        };
        if advance_cursor {
            self.round_robin_index
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .insert(pool_id, proxy_id);
        }
    }

    fn order_proxies_in_pool(
        &self,
        mut proxies: Vec<ProxyRecord>,
        algorithm: &str,
        host_key: &str,
        pool_id: i64,
        commit_selection: bool,
    ) -> Result<Vec<ProxyRecord>> {
        if proxies.is_empty() {
            return Ok(proxies);
        }
        match algorithm {
            "least_connections" => {
                let cursors = self
                    .round_robin_index
                    .lock()
                    .unwrap_or_else(|e| e.into_inner());
                let active = self
                    .active_connections
                    .lock()
                    .unwrap_or_else(|e| e.into_inner());
                let last = cursors.get(&pool_id).copied().unwrap_or(0);
                // One O(N) selection. Retries reselect with their excluded set.
                if let Some((index, _)) = proxies.iter().enumerate().min_by_key(|(_, p)| {
                    (
                        p.status.as_deref() == Some("inactive"),
                        active.get(&p.id).copied().unwrap_or(0),
                        p.id <= last,
                        p.id,
                    )
                }) {
                    proxies.swap(0, index);
                }
            }
            "round_robin" => {
                // Stable ID ring: health/exclusion skips do not redefine cursor positions.
                // Priority edits do not move the ring; a removed cursor resumes at its successor.
                proxies.sort_by_key(|proxy| proxy.id);
                let cursors = self
                    .round_robin_index
                    .lock()
                    .unwrap_or_else(|e| e.into_inner());
                let last = cursors.get(&pool_id).copied().unwrap_or(0);
                if !proxies.is_empty() {
                    let selected = proxies
                        .iter()
                        .position(|proxy| proxy.id > last)
                        .unwrap_or(0);
                    proxies.rotate_left(selected);
                    proxies = prioritize_route_status(proxies);
                }
            }
            "sticky_host" => {
                let host = crate::routing::normalize_host(host_key);
                proxies.sort_by_cached_key(|proxy| {
                    std::cmp::Reverse(crate::routing::affinity(pool_id, &host, proxy.id))
                });
            }
            _ => {
                let mut sequences = self
                    .adaptive_sequence
                    .lock()
                    .unwrap_or_else(|e| e.into_inner());
                sequences.retain(|pool, _| {
                    *pool == 0
                        || self
                            .db
                            .routing_snapshot()
                            .pools
                            .iter()
                            .any(|p| p.id == *pool)
                });
                let sequence = sequences
                    .get(&pool_id)
                    .copied()
                    .unwrap_or_default()
                    .wrapping_add(1);
                let learning_turn = sequence.is_multiple_of(16);
                let cursors = self
                    .round_robin_index
                    .lock()
                    .unwrap_or_else(|e| e.into_inner());
                let last = cursors.get(&pool_id).copied().unwrap_or(0);
                let mut metrics = self.metrics.write().unwrap_or_else(|e| e.into_inner());
                let now = monotonic_millis();
                for proxy in &proxies {
                    if let Some(metric) = metrics.get_mut(&proxy.id) {
                        metric.prune(now);
                    }
                }
                let active = self
                    .active_connections
                    .lock()
                    .unwrap_or_else(|e| e.into_inner());
                let effective_score = |proxy: &ProxyRecord| {
                    let quality = metrics
                        .get(&proxy.id)
                        .filter(|metric| !metric.requests.is_empty())
                        .map(|metric| metric.score)
                        .unwrap_or_else(|| score_of(proxy));
                    quality / (1 + active.get(&proxy.id).copied().unwrap_or(0).max(0)) as f64
                };
                let learner = if learning_turn {
                    proxies
                        .iter()
                        .enumerate()
                        .filter(|(_, p)| {
                            p.status.as_deref() != Some("inactive")
                                && metrics.get(&p.id).is_none_or(|m| m.learning_remaining > 0)
                        })
                        .min_by_key(|(_, p)| (p.id <= last, p.id))
                        .map(|(index, _)| index)
                } else {
                    None
                };
                let best = learner.or_else(|| {
                    proxies
                        .iter()
                        .enumerate()
                        .max_by(|(_, a), (_, b)| {
                            (a.status.as_deref() != Some("inactive"))
                                .cmp(&(b.status.as_deref() != Some("inactive")))
                                .then_with(|| effective_score(a).total_cmp(&effective_score(b)))
                                .then_with(|| b.id.cmp(&a.id))
                        })
                        .map(|(index, _)| index)
                });
                if let Some(index) = best {
                    proxies.swap(0, index);
                }
            }
        }
        let proxies = prioritize_route_status(proxies);
        if commit_selection {
            self.commit_pool_selection(pool_id, algorithm, proxies[0].id);
        }
        Ok(proxies)
    }

    #[cfg(test)]
    async fn is_candidate_available(&self, proxy_id: i64, request: &TargetRequest) -> bool {
        let proxy_available = self
            .circuit_breakers
            .read()
            .unwrap_or_else(|e| e.into_inner())
            .get(&proxy_id)
            .is_none_or(CircuitBreaker::can_attempt_snapshot);
        proxy_available
            && self
                .target_circuits
                .read()
                .unwrap_or_else(|e| e.into_inner())
                .get(&TargetRouteKey::new(proxy_id, request))
                .is_none_or(|target| target.breaker.can_attempt_snapshot())
    }

    #[cfg(test)]
    async fn try_begin_attempt(&self, proxy_id: i64, request: &TargetRequest) -> bool {
        self.try_begin_attempt_token(proxy_id, request, None)
    }

    fn try_begin_attempt_token(
        &self,
        proxy_id: i64,
        request: &TargetRequest,
        token: Option<&Arc<()>>,
    ) -> bool {
        let config = self
            .runtime_settings
            .read()
            .unwrap_or_else(|e| e.into_inner())
            .circuit;
        let mut breakers = self
            .circuit_breakers
            .write()
            .unwrap_or_else(|e| e.into_inner());
        let breaker = breakers
            .entry(proxy_id)
            .or_insert_with(|| CircuitBreaker::new(config));
        breaker.apply_config(config);
        let mut targets = self
            .target_circuits
            .write()
            .unwrap_or_else(|e| e.into_inner());
        let target = targets.get_mut(&TargetRouteKey::new(proxy_id, request));
        if !breaker.can_attempt_snapshot()
            || target
                .as_ref()
                .is_some_and(|target| !target.breaker.can_attempt_snapshot())
        {
            return false;
        }
        if let Some(target) = target {
            target.breaker.try_begin_attempt();
            if target.breaker.state == "HALF_OPEN" {
                target.breaker.attempt = token.map(Arc::downgrade);
            }
        }
        let begun = breaker.try_begin_attempt();
        if breaker.state == "HALF_OPEN" {
            breaker.attempt = token.map(Arc::downgrade);
        }
        begun
    }

    async fn record_breaker_success(&self, proxy_id: i64) {
        let config = self
            .runtime_settings
            .read()
            .unwrap_or_else(|e| e.into_inner())
            .circuit;
        let mut breakers = self
            .circuit_breakers
            .write()
            .unwrap_or_else(|e| e.into_inner());
        let breaker = breakers
            .entry(proxy_id)
            .or_insert_with(|| CircuitBreaker::new(config));
        breaker.apply_config(config);
        breaker.record_success();
    }

    // A delayed HTTP response must still belong to this configuration and probe.
    fn owns_current_attempt(&self, proxy: &ConnectionLease, request: &TargetRequest) -> bool {
        let owns = |breaker: &CircuitBreaker| {
            breaker
                .attempt
                .as_ref()
                .is_none_or(|token| token.ptr_eq(&Arc::downgrade(&proxy.attempt)))
        };
        self.db.routing_snapshot().node_generations.get(&proxy.id) == Some(&proxy.generation)
            && self
                .circuit_breakers
                .read()
                .unwrap_or_else(|e| e.into_inner())
                .get(&proxy.id)
                .is_none_or(owns)
            && self
                .target_circuits
                .read()
                .unwrap_or_else(|e| e.into_inner())
                .get(&TargetRouteKey::new(proxy.id, request))
                .is_none_or(|target| owns(&target.breaker))
    }

    async fn record_verified_connection_locked(
        &self,
        proxy: &ConnectionLease,
        request: &TargetRequest,
        proxy_latency_us: u64,
    ) {
        self.record_breaker_success(proxy.id).await;
        self.target_circuits
            .write()
            .unwrap_or_else(|e| e.into_inner())
            .remove(&TargetRouteKey::new(proxy.id, request));
        if let Some(policy) = &proxy.failover_policy {
            self.sticky_routes
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .remember(
                    policy,
                    &self.db.routing_snapshot(),
                    proxy.id,
                    proxy.generation,
                );
        }
        self.record_connection_success_locked(proxy.id, proxy_latency_us)
            .await;
    }

    async fn observe_forward_response(
        &self,
        proxy: &ConnectionLease,
        request: &TargetRequest,
        status: u16,
        already_verified: bool,
        proxy_latency_us: u64,
    ) -> bool {
        let _guard = self.lock_proxy_status(proxy.id).await;
        if !self.owns_current_attempt(proxy, request) {
            return false;
        }
        if status == 407 {
            self.record_route_failure_locked(proxy.id, request, FailureScope::Proxy)
                .await;
        } else if !already_verified {
            // A final HTTP response proves forwarding/auth works, not business success.
            self.record_verified_connection_locked(proxy, request, proxy_latency_us)
                .await;
        }
        true
    }

    async fn cancel_target_half_open_attempt(&self, proxy_id: i64, request: &TargetRequest) {
        if let Some(target) = self
            .target_circuits
            .write()
            .unwrap_or_else(|e| e.into_inner())
            .get_mut(&TargetRouteKey::new(proxy_id, request))
        {
            target.breaker.cancel_half_open_attempt();
        }
    }

    #[cfg(test)]
    async fn cancel_half_open_attempt(&self, proxy_id: i64, request: &TargetRequest) {
        if let Some(breaker) = self
            .circuit_breakers
            .write()
            .unwrap_or_else(|e| e.into_inner())
            .get_mut(&proxy_id)
        {
            breaker.cancel_half_open_attempt();
        }
        self.cancel_target_half_open_attempt(proxy_id, request)
            .await;
    }

    async fn record_route_failure_locked(
        &self,
        proxy_id: i64,
        request: &TargetRequest,
        scope: FailureScope,
    ) {
        if scope == FailureScope::LocalRoute {
            // One local path failing is not evidence against any proxy/destination.
            return;
        }
        if scope == FailureScope::Proxy {
            self.record_breaker_failure_locked(proxy_id).await;
            self.cancel_target_half_open_attempt(proxy_id, request)
                .await;
            return;
        }
        // 已连上代理，失败属于此目标链路；不影响其他网站的评分、状态或全局熔断。
        self.record_breaker_success(proxy_id).await;
        let config = self
            .runtime_settings
            .read()
            .unwrap_or_else(|e| e.into_inner())
            .circuit;
        let key = TargetRouteKey::new(proxy_id, request);
        let now = monotonic_millis();
        let mut targets = self
            .target_circuits
            .write()
            .unwrap_or_else(|e| e.into_inner());
        if !targets.contains_key(&key) {
            targets.retain(|_, target| {
                target.breaker.half_open_in_flight()
                    || now.saturating_sub(target.last_failure)
                        < METRICS_WINDOW_MS.max(target.breaker.timeout_ms)
            });
            if targets.len() >= MAX_TARGET_CIRCUITS {
                let oldest = targets
                    .iter()
                    .filter(|(_, target)| !target.breaker.half_open_in_flight())
                    .min_by_key(|(_, target)| target.last_failure)
                    .map(|(key, _)| key.clone());
                if let Some(oldest) = oldest {
                    targets.remove(&oldest);
                } else {
                    return;
                }
            }
        }
        let target = targets.entry(key).or_insert_with(|| TargetCircuit {
            breaker: CircuitBreaker::new(config),
            last_failure: now,
        });
        target.last_failure = now;
        target.breaker.apply_config(config);
        target.breaker.record_failure();
    }

    async fn record_breaker_failure_locked(&self, proxy_id: i64) {
        let config = self
            .runtime_settings
            .read()
            .unwrap_or_else(|e| e.into_inner())
            .circuit;
        {
            let mut metrics = self.metrics.write().unwrap_or_else(|e| e.into_inner());
            metrics
                .entry(proxy_id)
                .or_insert_with(ProxyMetrics::new)
                .push(false, None);
        }
        let just_opened = {
            let mut breakers = self
                .circuit_breakers
                .write()
                .unwrap_or_else(|e| e.into_inner());
            let breaker = breakers
                .entry(proxy_id)
                .or_insert_with(|| CircuitBreaker::new(config));
            breaker.apply_config(config);
            let was_open = breaker.state == "OPEN";
            breaker.record_failure();
            !was_open && breaker.state == "OPEN"
        };
        if just_opened {
            // 熔断器刚打开，说明真实流量已连续失败，立即把状态刷成 inactive，
            // 让主动测活以更短的“恢复间隔”盯住它。
            {
                let mut metrics = self.metrics.write().unwrap_or_else(|e| e.into_inner());
                let metric = metrics.entry(proxy_id).or_insert_with(ProxyMetrics::new);
                metric.pushed_status = Some("inactive".to_string());
            }
            self.apply_passive_status_locked(proxy_id, "inactive", None);
        }
    }

    /// 各代理最近一次“真实流量成功”的时间戳（毫秒），供主动测活判断是否可跳过。
    pub async fn recent_success_map(&self) -> HashMap<i64, i64> {
        self.metrics
            .read()
            .unwrap_or_else(|e| e.into_inner())
            .iter()
            .map(|(id, metric)| (*id, metric.last_success))
            .collect()
    }

    pub async fn record_probe_result(
        &self,
        proxy: &ProxyRecord,
        generation: Option<u64>,
        probe_started_at: i64,
        desired_status: Option<&str>,
        response_time: Option<i64>,
        success: bool,
    ) -> Result<(Option<String>, bool)> {
        let proxy_id = proxy.id;
        let status_lock = self.status_lock(proxy_id).await;
        let _guard = status_lock.lock().await;
        if self
            .db
            .routing_snapshot()
            .node_generations
            .get(&proxy_id)
            .copied()
            != generation
        {
            return Err(anyhow!("代理配置代号在测试期间已变化，已丢弃旧测试结果"));
        }
        let traffic_proved_alive = self
            .metrics
            .read()
            .unwrap_or_else(|e| e.into_inner())
            .get(&proxy_id)
            .is_some_and(|metric| metric.last_success > probe_started_at);
        let applied_status = if desired_status == Some("inactive") && traffic_proved_alive {
            None
        } else {
            desired_status
        };
        let revision = if applied_status.is_some() {
            self.db.reserve_status_revision(proxy_id)
        } else {
            self.db.status_revision(proxy_id)
        };
        if success {
            self.record_breaker_success(proxy_id).await;
        }
        if let Some(status) = applied_status {
            let mut metrics = self.metrics.write().unwrap_or_else(|e| e.into_inner());
            let metric = metrics.entry(proxy_id).or_insert_with(ProxyMetrics::new);
            if success && metric.pushed_status.as_deref() == Some("inactive") {
                metric.learning_remaining = 3;
            }
            metric.pushed_status = Some(status.to_string());
        }
        // Publish runtime health under the short status guard, then persist off
        // the async executor. SQL must never keep a business connection waiting
        // for this guard. Generation + revision reject stale queued writes.
        drop(_guard);
        let db = self.db.clone();
        let observed = proxy.clone();
        let stored_status = applied_status.map(str::to_string);
        let updated = tokio::task::spawn_blocking(move || {
            db.persist_probe_result(
                &observed,
                generation,
                revision,
                stored_status.as_deref(),
                response_time,
                success,
            )
        })
        .await??;
        if !updated {
            return Err(anyhow!(
                "代理配置在测试结果写入前发生变化，已丢弃旧测试结果"
            ));
        }
        Ok((applied_status.map(str::to_string), traffic_proved_alive))
    }

    async fn status_lock(&self, proxy_id: i64) -> Arc<Mutex<()>> {
        let mut locks = self.status_locks.lock().await;
        let snapshot = self.db.routing_snapshot();
        locks.retain(|id, lock| {
            Arc::strong_count(lock) > 1 || snapshot.node_generations.contains_key(id)
        });
        locks
            .entry(proxy_id)
            .or_insert_with(|| Arc::new(Mutex::new(())))
            .clone()
    }

    pub async fn lock_proxy_status(&self, proxy_id: i64) -> tokio::sync::OwnedMutexGuard<()> {
        self.status_lock(proxy_id).await.lock_owned().await
    }

    // 调用方必须持有该代理的 status lock，保证状态写入与配置修改串行。
    fn apply_passive_status_locked(
        &self,
        proxy_id: i64,
        status: &str,
        response_time: Option<i64>,
    ) -> bool {
        let snapshot = self.db.routing_snapshot();
        snapshot
            .proxies
            .iter()
            .find(|proxy| proxy.id == proxy_id)
            .is_some_and(|proxy| {
                self.database_worker.status(
                    proxy.clone(),
                    snapshot.node_generations[&proxy_id],
                    status,
                    response_time,
                )
            })
    }

    async fn resolve_target(&self, mut request: TargetRequest) -> TargetRequest {
        if request.address_type == ADDR_DOMAIN {
            let mappings = self.dns_cache.read().await;
            if let Some(mapped) = mappings.get(&crate::routing::normalize_host(&request.host)) {
                request.host = mapped.clone();
                request.address_type = ADDR_IPV4;
            }
        }
        request
    }
}

pub async fn serve(runtime: Arc<ProxyRuntime>, host: String, port: u16) -> Result<()> {
    if runtime.is_stopping() {
        return Ok(());
    }
    if let Err(error) = runtime.refresh_dns_cache().await {
        runtime
            .set_service_status(ProxyServiceStatus {
                state: "failed".to_string(),
                running: false,
                host: host.clone(),
                port,
                error: Some(format!("DNS 缓存初始化失败: {error:#}")),
            })
            .await;
        return Err(error);
    }

    let listener = match TcpListener::bind((host.as_str(), port)).await {
        Ok(listener) => listener,
        Err(error) => {
            let message = format!("代理服务无法监听 {host}:{port}: {error}");
            runtime
                .set_service_status(ProxyServiceStatus {
                    state: "failed".to_string(),
                    running: false,
                    host: host.clone(),
                    port,
                    error: Some(message.clone()),
                })
                .await;
            return Err(anyhow!(message));
        }
    };

    runtime
        .set_service_status(ProxyServiceStatus {
            state: "running".to_string(),
            running: true,
            host: host.clone(),
            port,
            error: None,
        })
        .await;
    println!("混合代理负载均衡服务器运行在 {host}:{port}（SOCKS5/HTTP）");

    let mut clients = tokio::task::JoinSet::new();
    let result = tokio::select! {
        biased;
        _ = runtime.cancelled() => Ok(()),
        result = accept_clients(runtime.clone(), &listener, &mut clients, host, port) => result,
    };
    drop(listener);
    clients.abort_all();
    while clients.join_next().await.is_some() {}
    result
}

async fn accept_clients(
    runtime: Arc<ProxyRuntime>,
    listener: &TcpListener,
    clients: &mut tokio::task::JoinSet<()>,
    host: String,
    port: u16,
) -> Result<()> {
    loop {
        while clients.try_join_next().is_some() {}
        // Admission before spawn: at most 1024 live client tasks, no unbounded waiting queue.
        let connection_permit = runtime.connection_slots.clone().acquire_owned().await?;
        let (client, addr) = match listener.accept().await {
            Ok(accepted) => accepted,
            Err(error) => {
                if matches!(
                    error.kind(),
                    std::io::ErrorKind::Interrupted
                        | std::io::ErrorKind::ConnectionAborted
                        | std::io::ErrorKind::WouldBlock
                ) {
                    tokio::time::sleep(Duration::from_millis(100)).await;
                    continue;
                }
                if error
                    .raw_os_error()
                    .is_some_and(|code| [23, 24, 10024, 10055].contains(&code))
                {
                    eprintln!("代理 accept 资源不足，保留监听并退避: {error}");
                    tokio::time::sleep(Duration::from_secs(1)).await;
                    continue;
                }
                runtime
                    .set_service_status(ProxyServiceStatus {
                        state: "failed".to_string(),
                        running: false,
                        host: host.clone(),
                        port,
                        error: Some(format!("代理服务接收连接失败: {error}")),
                    })
                    .await;
                return Err(error.into());
            }
        };
        let runtime = runtime.clone();
        clients.spawn(async move {
            let _connection_permit = connection_permit;
            if let Err(error) = handle_client(runtime, client, addr).await {
                eprintln!("处理客户端连接失败 {addr}: {error:#}");
            }
        });
    }
}

async fn handle_client(
    runtime: Arc<ProxyRuntime>,
    mut client: TcpStream,
    _addr: SocketAddr,
) -> Result<()> {
    let start = Instant::now();
    let inbound_auth = runtime.inbound_auth().await;
    let request = timeout(Duration::from_secs(5), async {
        let _handshake_permit = runtime.handshake_slots.clone().acquire_owned().await?;
        let initial = sniff_protocol(&mut client).await?;
        if initial.first().copied() == Some(SOCKS_VERSION) {
            handle_socks5_handshake(&mut client, initial, &inbound_auth).await
        } else if looks_like_http_proxy_request(&initial) {
            handle_http_proxy_header(&mut client, initial, &inbound_auth).await
        } else {
            Err(anyhow!("不支持的入站代理协议"))
        }
    })
    .await
    .map_err(|_| anyhow!("入站代理握手超时"))??;

    let original_host = request.original_host.clone();
    let original_port = request.port;
    let request = runtime.resolve_target(request).await;
    let connect_start = Instant::now();
    let budget = Duration::from_millis(
        runtime
            .runtime_settings
            .read()
            .unwrap_or_else(|e| e.into_inner())
            .fail_fast
            .total_timeout_ms,
    );
    let deadline = tokio::time::Instant::from_std(connect_start + budget);
    let connection = tokio::time::timeout_at(
        deadline,
        connect_with_fail_fast(runtime.clone(), &request, connect_start),
    )
    .await
    .unwrap_or_else(|_| Err(anyhow!("代理建连总截止时间已到")));
    match connection {
        Ok((selected_proxy, mut upstream)) => {
            let proxy_id = selected_proxy.id;
            let prefetched_bytes = upstream.prefetched_response.len() as u64;
            let initial_bytes = upstream
                .outbound_initial_payload
                .as_ref()
                .map_or(request.initial_payload.len(), Vec::len)
                as u64;
            if let Err(error) = tokio::time::timeout_at(
                deadline,
                complete_client_handshake(&mut client, &mut upstream, &request),
            )
            .await
            .unwrap_or_else(|_| Err(anyhow!("代理连接交付超时")))
            {
                let error_message = error.to_string();
                runtime
                    .record_request(RequestLogEntry {
                        proxy_id: Some(proxy_id),
                        target_host: &original_host,
                        target_port: i64::from(original_port),
                        success: false,
                        response_time: Some(start.elapsed().as_millis() as i64),
                        error_message: Some(&error_message),
                        result_type: "tunnel_setup_error",
                    })
                    .await;
                return Err(error);
            }
            let response_time = start.elapsed().as_millis() as i64;
            runtime
                .record_request(RequestLogEntry {
                    proxy_id: Some(proxy_id),
                    target_host: &original_host,
                    target_port: i64::from(original_port),
                    success: upstream.target_verified
                        && request.inbound != InboundProtocol::HttpForward,
                    response_time: Some(response_time),
                    error_message: None,
                    result_type: if upstream.target_verified
                        && request.inbound != InboundProtocol::HttpForward
                    {
                        "tunnel_established"
                    } else {
                        "forwarded_unverified"
                    },
                })
                .await;
            let transfer_start = Instant::now();
            let counters = Arc::new(http_observer::TransferCounters::default());
            counters
                .read
                .store(prefetched_bytes, std::sync::atomic::Ordering::Relaxed);
            counters
                .written
                .store(initial_bytes, std::sync::atomic::Ordering::Relaxed);
            let copy_result = if request.inbound == InboundProtocol::HttpForward {
                let (sender, receiver) = tokio::sync::oneshot::channel();
                let transfer = async {
                    let mut observed = http_observer::HttpObserver::with_counters(
                        &mut upstream.stream,
                        Some(sender),
                        counters.clone(),
                    );
                    io::copy_bidirectional(&mut client, &mut observed).await
                };
                let observe = async {
                    if let Ok(status) = receiver.await {
                        if runtime
                            .observe_forward_response(
                                &selected_proxy,
                                &request,
                                status,
                                upstream.target_verified,
                                upstream.proxy_latency_us,
                            )
                            .await
                        {
                            let detail = format!("HTTP {status}");
                            runtime
                                .record_request(RequestLogEntry {
                                    proxy_id: Some(proxy_id),
                                    target_host: &original_host,
                                    target_port: i64::from(original_port),
                                    success: status == 101 || (200..400).contains(&status),
                                    response_time: Some(start.elapsed().as_millis() as i64),
                                    error_message: Some(&detail),
                                    result_type: "upstream_response_observed",
                                })
                                .await;
                        }
                    }
                };
                tokio::join!(transfer, observe).0
            } else {
                let mut observed = http_observer::HttpObserver::with_counters(
                    &mut upstream.stream,
                    None,
                    counters.clone(),
                );
                io::copy_bidirectional(&mut client, &mut observed).await
            };
            let details = json!({"durationMs": transfer_start.elapsed().as_millis(), "bytesSent": counters.written.load(std::sync::atomic::Ordering::Relaxed), "bytesReceived": counters.read.load(std::sync::atomic::Ordering::Relaxed), "closeReason": copy_result.as_ref().err().map_or_else(|| "eof".to_string(), ToString::to_string), "stage": "transfer"}).to_string();
            runtime
                .record_request(RequestLogEntry {
                    proxy_id: Some(proxy_id),
                    target_host: &original_host,
                    target_port: i64::from(original_port),
                    success: copy_result.is_ok(),
                    response_time: None,
                    error_message: Some(&details),
                    result_type: if copy_result.is_ok() {
                        "transfer_finished"
                    } else {
                        "transfer_error"
                    },
                })
                .await;
            drop(selected_proxy);
            if let Err(error) = copy_result {
                eprintln!(
                    "代理隧道传输中断: target={original_host}:{original_port}, proxy_id={proxy_id}, error={error}"
                );
            }
            Ok(())
        }
        Err(error) => {
            let error_message = error.to_string();
            send_inbound_error(&mut client, &request, &error_message).await?;
            runtime
                .record_request(RequestLogEntry {
                    proxy_id: None,
                    target_host: &original_host,
                    target_port: i64::from(original_port),
                    success: false,
                    response_time: Some(start.elapsed().as_millis() as i64),
                    error_message: Some(&error_message),
                    result_type: "proxy_exhausted",
                })
                .await;
            Err(error)
        }
    }
}

async fn connect_with_fail_fast(
    runtime: Arc<ProxyRuntime>,
    request: &TargetRequest,
    start: Instant,
) -> Result<(ConnectionLease, ConnectedUpstream)> {
    let config = runtime
        .runtime_settings
        .read()
        .unwrap_or_else(|e| e.into_inner())
        .fail_fast;

    let total_timeout = Duration::from_millis(config.total_timeout_ms);
    let mut attempted = 0usize;
    let mut excluded = HashSet::new();
    let mut errors = Vec::new();
    let mut failover_started: Option<Instant> = None;
    while !config.enabled || attempted < config.max_attempts {
        if start.elapsed() >= total_timeout {
            return Err(anyhow!("总超时: {}", errors.join("; ")));
        }
        let Some(mut proxy) = timeout(
            total_timeout.saturating_sub(start.elapsed()),
            runtime.reserve_proxy(request, &excluded),
        )
        .await
        .map_err(|_| anyhow!("选路等待超时"))??
        else {
            break;
        };
        excluded.insert(proxy.id);
        let remaining = total_timeout.saturating_sub(start.elapsed());
        if remaining.is_zero() {
            return Err(anyhow!("总超时: {}", errors.join("; ")));
        }
        attempted += 1;
        let attempt_started = Instant::now();
        let mut scope = FailureScope::Proxy;
        let attempt = timeout(
            Duration::from_millis(config.attempt_timeout_ms).min(remaining),
            async {
                #[cfg(test)]
                if let Some(kind) = runtime
                    .injected_connect_errors
                    .lock()
                    .unwrap()
                    .remove(&proxy.id)
                {
                    return Err(std::io::Error::from(kind).into());
                }
                connect_through_proxy(&proxy, request, &mut scope).await
            },
        )
        .await
        .unwrap_or_else(|_| Err(anyhow!("{}超时", scope.label())));
        let attempt_elapsed_us = attempt_started.elapsed().as_micros() as u64;
        if attempt.as_ref().err().is_some_and(is_local_network_error) {
            scope = FailureScope::LocalRoute;
        }
        runtime.telemetry.record(
            "attempt_total",
            attempt_started.elapsed().as_micros() as u64,
        );
        let _status_guard = runtime.lock_proxy_status(proxy.id).await;
        let configuration_current = match runtime.proxy_configuration_is_current(&proxy) {
            Ok(current) => current,
            Err(error) => {
                return Err(error.context("连接结束后校验代理配置失败"));
            }
        };
        if !configuration_current || !runtime.owns_current_attempt(&proxy, request) {
            attempted = attempted.saturating_sub(1);
            errors.push(format!("{}: 连接期间代理配置已变化", proxy.name));
            continue;
        }

        match attempt {
            Ok(stream) => {
                if let Some(failed_at) = failover_started {
                    runtime.telemetry.record(
                        "failover_after_failure",
                        failed_at.elapsed().as_micros() as u64,
                    );
                }
                runtime
                    .telemetry
                    .record("proxy_tcp_auth", stream.proxy_latency_us);
                runtime.telemetry.record(
                    "target_handshake",
                    attempt_elapsed_us.saturating_sub(stream.proxy_latency_us),
                );
                runtime
                    .telemetry
                    .record("connect_total", start.elapsed().as_micros() as u64);
                proxy.dial_permit = None;
                proxy.global_dial_permit = None;
                runtime.dial_ready.notify_waiters();
                if stream.target_verified {
                    // 评分只计当前节点自身的建连耗时；请求日志仍记录用户等待的总耗时。
                    runtime
                        .record_verified_connection_locked(&proxy, request, stream.proxy_latency_us)
                        .await;
                }
                #[cfg(all(debug_assertions, not(test)))]
                eprintln!(
                    "代理路由成功: target={}:{}, proxy_id={}, proxy_name={}, proxy_type={}",
                    request.original_host, request.port, proxy.id, proxy.name, proxy.proxy_type
                );
                return Ok((proxy, stream));
            }
            Err(error) => {
                failover_started.get_or_insert_with(Instant::now);
                if scope == FailureScope::LocalRoute {
                    runtime
                        .telemetry
                        .record("local_network_failure", attempt_elapsed_us);
                    errors.push(format!("{} [{}]: {error:#}", proxy.name, scope.label()));
                    // No independent evidence of global offline: keep the same pool,
                    // total deadline and attempt cap, without penalizing node health.
                    continue;
                }
                runtime.telemetry.record(
                    if scope == FailureScope::Proxy {
                        "proxy_failure"
                    } else {
                        "target_failure"
                    },
                    attempt_started.elapsed().as_micros() as u64,
                );
                runtime
                    .record_route_failure_locked(proxy.id, request, scope)
                    .await;
                #[cfg(all(debug_assertions, not(test)))]
                eprintln!(
                    "代理路由尝试失败: target={}:{}, proxy_id={}, proxy_name={}, scope={}, error={error:#}",
                    request.original_host, request.port, proxy.id, proxy.name, scope.label()
                );
                errors.push(format!("{} [{}]: {error:#}", proxy.name, scope.label()));
            }
        }
    }

    if attempted == 0 && errors.is_empty() {
        return Err(anyhow!("所有候选代理当前均处于熔断状态或拨号容量已满"));
    }
    Err(anyhow!("所有代理都失败: {}", errors.join("; ")))
}

async fn connect_through_proxy(
    proxy: &ProxyRecord,
    request: &TargetRequest,
    scope: &mut FailureScope,
) -> Result<ConnectedUpstream> {
    match proxy.proxy_type.as_str() {
        "socks5" => {
            connect_socks5(proxy, request, scope)
                .await
                .map(|(stream, proxy_latency_us)| ConnectedUpstream {
                    stream,
                    outbound_initial_payload: None,
                    prefetched_response: Vec::new(),
                    target_verified: true,
                    proxy_latency_us,
                })
        }
        "socks4" => {
            connect_socks4(proxy, request, scope)
                .await
                .map(|(stream, proxy_latency_us)| ConnectedUpstream {
                    stream,
                    outbound_initial_payload: None,
                    prefetched_response: Vec::new(),
                    target_verified: true,
                    proxy_latency_us,
                })
        }
        "http" | "https" => connect_http_proxy(proxy, request, scope).await,
        other => Err(anyhow!("不支持的代理类型: {other}")),
    }
}

async fn connect_socks5(
    proxy: &ProxyRecord,
    request: &TargetRequest,
    scope: &mut FailureScope,
) -> Result<(TcpStream, u64)> {
    let start = Instant::now();
    let mut stream = TcpStream::connect((proxy.host.as_str(), proxy.port as u16)).await?;
    let use_auth = proxy
        .username
        .as_deref()
        .is_some_and(|value| !value.is_empty());
    if use_auth {
        stream.write_all(&[0x05, 0x02, 0x00, 0x02]).await?;
    } else {
        stream.write_all(&[0x05, 0x01, 0x00]).await?;
    }
    let mut response = [0u8; 2];
    stream.read_exact(&mut response).await?;
    if response[0] != 0x05 {
        return Err(anyhow!("SOCKS5握手失败"));
    }
    if response[1] == 0x02 {
        if !use_auth {
            return Err(anyhow!("SOCKS5服务器要求用户名密码，但该代理未配置凭据"));
        }
        let username = proxy.username.as_deref().unwrap_or("");
        let password = proxy.password.as_deref().unwrap_or("");
        if username.len() > 255 || password.len() > 255 {
            return Err(anyhow!("SOCKS5用户名或密码过长"));
        }
        let mut auth = vec![0x01, username.len() as u8];
        auth.extend_from_slice(username.as_bytes());
        auth.push(password.len() as u8);
        auth.extend_from_slice(password.as_bytes());
        stream.write_all(&auth).await?;
        let mut auth_response = [0u8; 2];
        stream.read_exact(&mut auth_response).await?;
        if auth_response[0] != 0x01 || auth_response[1] != 0x00 {
            return Err(anyhow!("SOCKS5认证失败"));
        }
    } else if response[1] != 0x00 {
        return Err(anyhow!("SOCKS5服务器未接受认证方式"));
    }

    let proxy_latency_us = start.elapsed().as_micros() as u64;
    *scope = FailureScope::Target;
    stream
        .write_all(&build_socks5_connect_request(request)?)
        .await?;
    let mut header = [0u8; 4];
    stream.read_exact(&mut header).await?;
    if header[0] != 0x05 || header[2] != 0x00 {
        *scope = FailureScope::Proxy;
        return Err(anyhow!("无效的 SOCKS5 目标连接响应"));
    }
    if header[1] != 0x00 {
        // RFC 1928: 0x01 是服务器故障；规则拒绝/目标不可达等只影响当前目标。
        if header[1] == 0x01 || header[1] == 0x07 || header[1] > 0x08 {
            *scope = FailureScope::Proxy;
        }
        return Err(anyhow!("SOCKS5连接目标失败，响应码 {}", header[1]));
    }
    *scope = FailureScope::Proxy;
    read_socks5_bind_address(&mut stream, header[3]).await?;
    Ok((stream, proxy_latency_us))
}

async fn connect_socks4(
    proxy: &ProxyRecord,
    request: &TargetRequest,
    scope: &mut FailureScope,
) -> Result<(TcpStream, u64)> {
    let start = Instant::now();
    let mut stream = TcpStream::connect((proxy.host.as_str(), proxy.port as u16)).await?;
    // SOCKS4 只有 USERID 字段，没有密码认证；配置层会拒绝 SOCKS4 密码。
    let userid = proxy.username.as_deref().unwrap_or("");
    if userid.len() > 255 {
        return Err(anyhow!("SOCKS4 用户标识过长"));
    }
    let proxy_latency_us = start.elapsed().as_micros() as u64;
    *scope = FailureScope::Target;
    let packet = build_socks4_connect_request(request, userid)?;
    stream.write_all(&packet).await?;
    let mut response = [0u8; 8];
    stream.read_exact(&mut response).await?;
    if response[0] != 0x00 {
        *scope = FailureScope::Proxy;
        return Err(anyhow!("无效的 SOCKS4 目标连接响应"));
    }
    if response[1] != 0x5a {
        if response[1] != 0x5b {
            *scope = FailureScope::Proxy;
        }
        return Err(anyhow!("{}", socks4_reply_message(response[1])));
    }
    Ok((stream, proxy_latency_us))
}

fn build_socks4_connect_request(request: &TargetRequest, userid: &str) -> Result<Vec<u8>> {
    let mut packet = vec![
        0x04,
        SOCKS_CMD_CONNECT,
        (request.port >> 8) as u8,
        (request.port & 0xff) as u8,
    ];
    if let Ok(target_ip) = request.host.parse::<Ipv4Addr>() {
        packet.extend_from_slice(&target_ip.octets());
        packet.extend_from_slice(userid.as_bytes());
        packet.push(0x00);
    } else {
        if request.host.contains(':') {
            return Err(anyhow!("SOCKS4a暂不支持IPv6地址"));
        }
        if request.host.len() > 255 {
            return Err(anyhow!("SOCKS4a目标域名过长"));
        }
        packet.extend_from_slice(&[0x00, 0x00, 0x00, 0x01]);
        packet.extend_from_slice(userid.as_bytes());
        packet.push(0x00);
        packet.extend_from_slice(request.host.as_bytes());
        packet.push(0x00);
    }
    Ok(packet)
}

fn socks4_reply_message(code: u8) -> String {
    match code {
        0x5b => "SOCKS4 请求被拒绝或失败(0x5b)".to_string(),
        0x5c => "SOCKS4 请求失败(0x5c)：无法连接到客户端 identd 服务".to_string(),
        0x5d => "SOCKS4 请求失败(0x5d)：identd 无法确认用户标识".to_string(),
        other => format!("SOCKS4 连接目标失败，响应码 0x{other:02x}"),
    }
}

async fn connect_http_proxy(
    proxy: &ProxyRecord,
    request: &TargetRequest,
    scope: &mut FailureScope,
) -> Result<ConnectedUpstream> {
    let start = Instant::now();
    let mut stream = TcpStream::connect((proxy.host.as_str(), proxy.port as u16)).await?;
    let proxy_latency_us = start.elapsed().as_micros() as u64;
    *scope = FailureScope::Target;
    if request.inbound == InboundProtocol::HttpForward {
        let payload = build_http_forward_proxy_payload(proxy, request)?;
        return Ok(ConnectedUpstream {
            stream,
            outbound_initial_payload: Some(payload),
            prefetched_response: Vec::new(),
            target_verified: false,
            proxy_latency_us,
        });
    }

    let mut connect_request = format!(
        "CONNECT {}:{} HTTP/1.1\r\nHost: {}:{}\r\n",
        request.host, request.port, request.host, request.port
    );
    if let Some(username) = proxy.username.as_deref().filter(|value| !value.is_empty()) {
        let password = proxy.password.as_deref().unwrap_or("");
        let auth = general_purpose::STANDARD.encode(format!("{username}:{password}"));
        connect_request.push_str(&format!("Proxy-Authorization: Basic {auth}\r\n"));
    }
    connect_request.push_str("\r\n");
    stream.write_all(connect_request.as_bytes()).await?;
    let (header, prefetched_response) =
        read_http_request_header(&mut stream, Vec::new(), 5000).await?;
    let first = header.lines().next().unwrap_or_default();
    let mut parts = first.split_whitespace();
    let version = parts.next().unwrap_or_default();
    let code = parts.next().and_then(|value| value.parse::<u16>().ok());
    if !matches!(version, "HTTP/1.0" | "HTTP/1.1")
        || !code.is_some_and(|code| (100..=599).contains(&code))
    {
        *scope = FailureScope::Proxy;
        return Err(anyhow!("无效的 HTTP 代理响应: {first}"));
    }
    if code == Some(407) {
        *scope = FailureScope::Proxy;
        return Err(anyhow!("HTTP代理认证失败: {first}"));
    }
    if !code.is_some_and(|code| (200..300).contains(&code)) {
        return Err(anyhow!("HTTP代理CONNECT失败: {first}"));
    }
    Ok(ConnectedUpstream {
        stream,
        outbound_initial_payload: None,
        prefetched_response,
        target_verified: true,
        proxy_latency_us,
    })
}

fn build_http_forward_proxy_payload(
    proxy: &ProxyRecord,
    request: &TargetRequest,
) -> Result<Vec<u8>> {
    let (header, body) = split_http_header(&request.initial_payload)
        .ok_or_else(|| anyhow!("普通 HTTP 代理请求缺少完整请求头"))?;
    let header = std::str::from_utf8(header).context("HTTP请求头不是有效UTF-8")?;
    let is_upgrade = http_is_upgrade(header);
    let mut lines = header.trim_end_matches("\r\n\r\n").split("\r\n");
    let request_line = lines.next().unwrap_or_default();
    let mut parts = request_line.split_whitespace();
    let method = parts.next().ok_or_else(|| anyhow!("HTTP请求缺少方法"))?;
    let path = parts.next().ok_or_else(|| anyhow!("HTTP请求缺少路径"))?;
    let version = parts
        .next()
        .ok_or_else(|| anyhow!("HTTP请求缺少协议版本"))?;
    if parts.next().is_some() {
        return Err(anyhow!("无效HTTP代理请求行"));
    }

    let authority = format_authority(&request.host, request.port, 80);
    let absolute_target = if path == "*" {
        "*".to_string()
    } else {
        format!("http://{authority}{path}")
    };
    let mut rewritten = vec![format!("{method} {absolute_target} {version}")];
    for line in lines {
        if let Some((name, _)) = line.split_once(':') {
            if name.eq_ignore_ascii_case("proxy-connection")
                || name.eq_ignore_ascii_case("proxy-authorization")
            {
                continue;
            }
        }
        rewritten.push(line.to_string());
    }
    if let Some(username) = proxy.username.as_deref().filter(|value| !value.is_empty()) {
        let password = proxy.password.as_deref().unwrap_or_default();
        let encoded = general_purpose::STANDARD.encode(format!("{username}:{password}"));
        rewritten.push(format!("Proxy-Authorization: Basic {encoded}"));
    }
    if !is_upgrade {
        rewritten.push("Proxy-Connection: close".to_string());
    }
    let mut payload = format!("{}\r\n\r\n", rewritten.join("\r\n")).into_bytes();
    payload.extend_from_slice(body);
    Ok(payload)
}

async fn handle_socks5_handshake(
    client: &mut TcpStream,
    mut initial: Vec<u8>,
    auth: &InboundAuth,
) -> Result<TargetRequest> {
    let greeting = read_exact_buffered(client, &mut initial, 2, 1000).await?;
    if greeting[0] != SOCKS_VERSION {
        return Err(anyhow!("无效的 SOCKS5 协议版本"));
    }
    let methods = read_exact_buffered(client, &mut initial, greeting[1] as usize, 1000).await?;
    let selected_method = if auth.enabled {
        SOCKS_AUTH_USERNAME_PASSWORD
    } else {
        SOCKS_AUTH_NONE
    };
    if !methods.contains(&selected_method) {
        client
            .write_all(&[SOCKS_VERSION, SOCKS_AUTH_REJECTED])
            .await?;
        return Err(anyhow!(if auth.enabled {
            "SOCKS5 客户端不支持用户名密码认证"
        } else {
            "SOCKS5 客户端未提供免认证方式"
        }));
    }
    client.write_all(&[SOCKS_VERSION, selected_method]).await?;

    if auth.enabled {
        authenticate_socks5_client(client, &mut initial, auth).await?;
    }

    let header = read_exact_buffered(client, &mut initial, 4, 1000).await?;
    if header[0] != 0x05 || header[1] != SOCKS_CMD_CONNECT {
        return Err(anyhow!("仅支持 SOCKS5 CONNECT 命令"));
    }
    let (host, port, address_type) =
        read_socks_address_buffered(client, &mut initial, header[3]).await?;
    if port == 0 {
        return Err(anyhow!("目标端口不能为 0"));
    }
    Ok(TargetRequest {
        original_host: host.clone(),
        host,
        port,
        address_type,
        inbound: InboundProtocol::Socks5,
        initial_payload: initial,
    })
}

async fn authenticate_socks5_client(
    client: &mut TcpStream,
    buffer: &mut Vec<u8>,
    auth: &InboundAuth,
) -> Result<()> {
    let header = read_exact_buffered(client, buffer, 2, 1000).await?;
    if header[0] != 0x01 {
        client.write_all(&[0x01, 0x01]).await?;
        return Err(anyhow!("无效的 SOCKS5 用户名密码认证版本"));
    }
    let username = read_exact_buffered(client, buffer, header[1] as usize, 1000).await?;
    let password_length = read_exact_buffered(client, buffer, 1, 1000).await?[0] as usize;
    let password = read_exact_buffered(client, buffer, password_length, 1000).await?;
    let authorized = constant_time_eq(&username, auth.username.as_bytes())
        && constant_time_eq(&password, auth.password.as_bytes());
    client
        .write_all(&[0x01, if authorized { 0x00 } else { 0x01 }])
        .await?;
    if !authorized {
        return Err(anyhow!("SOCKS5 入站认证失败"));
    }
    Ok(())
}

async fn handle_http_proxy_header(
    client: &mut TcpStream,
    initial: Vec<u8>,
    auth: &InboundAuth,
) -> Result<TargetRequest> {
    let (header, prefetched) = read_http_request_header(client, initial, 5000).await?;
    if !http_proxy_authorized(&header, auth) {
        send_http_auth_required(client).await?;
        return Err(anyhow!("HTTP 入站代理认证失败"));
    }
    parse_http_proxy_request(&header, prefetched)
}

fn parse_http_proxy_request(header: &str, prefetched: Vec<u8>) -> Result<TargetRequest> {
    let mut lines = header.split("\r\n");
    let request_line = lines.next().unwrap_or_default();
    let parts = request_line.split_whitespace().collect::<Vec<_>>();
    if parts.len() != 3 {
        return Err(anyhow!("无效HTTP代理请求行"));
    }
    let method = parts[0].to_uppercase();
    let target = parts[1];
    if method == "CONNECT" {
        let (host, port) = parse_authority(target, 443)?;
        return Ok(TargetRequest {
            original_host: host.clone(),
            address_type: address_type(&host)?,
            host,
            port,
            inbound: InboundProtocol::HttpConnect,
            initial_payload: prefetched,
        });
    }

    let mut host_header = None;
    for line in lines {
        if let Some((name, value)) = line.split_once(':') {
            if name.eq_ignore_ascii_case("host") {
                if host_header.is_some() {
                    return Err(anyhow!("HTTP 代理请求包含重复 Host 头"));
                }
                host_header = Some(value.trim().to_string());
            }
        }
    }
    let (host, port, path) = if target == "*" || target.starts_with('/') {
        let host_header = host_header.ok_or_else(|| anyhow!("普通HTTP代理请求缺少Host头"))?;
        let (host, port) = parse_authority(&host_header, 80)?;
        (host, port, target.to_string())
    } else {
        let url = url::Url::parse(target)?;
        if url.scheme() != "http" {
            return Err(anyhow!(
                "普通 HTTP 代理请求只支持 http:// 绝对地址；HTTPS 目标必须使用 CONNECT"
            ));
        }
        let host = url
            .host_str()
            .ok_or_else(|| anyhow!("普通HTTP代理请求缺少目标主机"))?
            .to_string();
        let port = url.port_or_known_default().unwrap_or(80);
        let path = format!(
            "{}{}",
            if url.path().is_empty() {
                "/"
            } else {
                url.path()
            },
            url.query()
                .map(|query| format!("?{query}"))
                .unwrap_or_default()
        );
        (host, port, path)
    };
    if port == 0 {
        return Err(anyhow!("目标端口不能为 0"));
    }

    let authority = format_authority(&host, port, 80);
    let mut initial_payload =
        rewrite_http_forward_header(header, &method, &path, &authority).into_bytes();
    initial_payload.extend_from_slice(&prefetched);
    Ok(TargetRequest {
        original_host: host.clone(),
        address_type: address_type(&host)?,
        host,
        port,
        inbound: InboundProtocol::HttpForward,
        initial_payload,
    })
}

fn rewrite_http_forward_header(header: &str, method: &str, path: &str, authority: &str) -> String {
    let is_upgrade = http_is_upgrade(header);
    let mut lines = header.trim_end_matches("\r\n\r\n").split("\r\n");
    let version = lines
        .next()
        .and_then(|line| line.split_whitespace().nth(2))
        .unwrap_or("HTTP/1.1");
    let mut rewritten = vec![
        format!("{method} {path} {version}"),
        format!("Host: {authority}"),
    ];
    for line in lines {
        if let Some((name, _)) = line.split_once(':') {
            if name.eq_ignore_ascii_case("proxy-connection")
                || name.eq_ignore_ascii_case("proxy-authorization")
                || name.eq_ignore_ascii_case("connection")
                || name.eq_ignore_ascii_case("keep-alive")
                || name.eq_ignore_ascii_case("host")
            {
                continue;
            }
        }
        rewritten.push(line.to_string());
    }
    rewritten.push(if is_upgrade {
        "Connection: Upgrade".to_string()
    } else {
        "Connection: close".to_string()
    });
    format!("{}\r\n\r\n", rewritten.join("\r\n"))
}

fn http_is_upgrade(header: &str) -> bool {
    let mut has_upgrade = false;
    let mut connection_upgrade = false;
    for line in header.lines().skip(1) {
        let Some((name, value)) = line.split_once(':') else {
            continue;
        };
        if name.eq_ignore_ascii_case("upgrade") && !value.trim().is_empty() {
            has_upgrade = true;
        } else if name.eq_ignore_ascii_case("connection") {
            connection_upgrade = value
                .split(',')
                .any(|token| token.trim().eq_ignore_ascii_case("upgrade"));
        }
    }
    has_upgrade && connection_upgrade
}

async fn complete_client_handshake(
    client: &mut TcpStream,
    upstream: &mut ConnectedUpstream,
    request: &TargetRequest,
) -> Result<()> {
    match request.inbound {
        InboundProtocol::Socks5 => {
            client
                .write_all(&[
                    0x05, 0x00, 0x00, ADDR_IPV4, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
                ])
                .await?;
            if !request.initial_payload.is_empty() {
                upstream.stream.write_all(&request.initial_payload).await?;
            }
        }
        InboundProtocol::HttpConnect => {
            client
                .write_all(b"HTTP/1.1 200 Connection Established\r\n\r\n")
                .await?;
            if !request.initial_payload.is_empty() {
                upstream.stream.write_all(&request.initial_payload).await?;
            }
        }
        InboundProtocol::HttpForward => {
            if let Some(payload) = upstream.outbound_initial_payload.take() {
                upstream.stream.write_all(&payload).await?;
            } else {
                upstream.stream.write_all(&request.initial_payload).await?;
            }
        }
    }
    let prefetched_response = std::mem::take(&mut upstream.prefetched_response);
    if !prefetched_response.is_empty() {
        client.write_all(&prefetched_response).await?;
    }
    Ok(())
}

async fn send_inbound_error(
    client: &mut TcpStream,
    request: &TargetRequest,
    message: &str,
) -> Result<()> {
    match request.inbound {
        InboundProtocol::Socks5 => {
            client
                .write_all(&[
                    0x05, 0x04, 0x00, ADDR_IPV4, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
                ])
                .await?;
        }
        InboundProtocol::HttpConnect | InboundProtocol::HttpForward => {
            let body = format!("502 Bad Gateway\n{message}\n");
            let response = format!(
                "HTTP/1.1 502 Bad Gateway\r\nConnection: close\r\nContent-Type: text/plain; charset=utf-8\r\nContent-Length: {}\r\n\r\n{}",
                body.len(),
                body
            );
            client.write_all(response.as_bytes()).await?;
        }
    }
    Ok(())
}

#[cfg(test)]
async fn read_some_with_timeout(stream: &mut TcpStream, timeout_ms: u64) -> Result<Vec<u8>> {
    let mut buffer = vec![0u8; 1024];
    let size = timeout(Duration::from_millis(timeout_ms), stream.read(&mut buffer)).await??;
    if size == 0 {
        return Err(anyhow!("连接已关闭"));
    }
    buffer.truncate(size);
    Ok(buffer)
}

async fn read_exact_buffered(
    stream: &mut TcpStream,
    buffer: &mut Vec<u8>,
    length: usize,
    timeout_ms: u64,
) -> Result<Vec<u8>> {
    while buffer.len() < length {
        let mut temp = vec![0u8; (length - buffer.len()).max(1)];
        let size = timeout(Duration::from_millis(timeout_ms), stream.read(&mut temp)).await??;
        if size == 0 {
            return Err(anyhow!("连接在读取协议数据时关闭"));
        }
        buffer.extend_from_slice(&temp[..size]);
    }
    Ok(buffer.drain(..length).collect())
}

async fn read_socks_address_buffered(
    stream: &mut TcpStream,
    buffer: &mut Vec<u8>,
    address_type: u8,
) -> Result<(String, u16, u8)> {
    match address_type {
        ADDR_IPV4 => {
            let rest = read_exact_buffered(stream, buffer, 6, 1000).await?;
            let host = format!("{}.{}.{}.{}", rest[0], rest[1], rest[2], rest[3]);
            let port = u16::from_be_bytes([rest[4], rest[5]]);
            Ok((host, port, ADDR_IPV4))
        }
        ADDR_DOMAIN => {
            let len = read_exact_buffered(stream, buffer, 1, 1000).await?[0] as usize;
            let domain = read_exact_buffered(stream, buffer, len, 1000).await?;
            let port_bytes = read_exact_buffered(stream, buffer, 2, 1000).await?;
            let host = String::from_utf8(domain).context("SOCKS5域名不是有效UTF-8")?;
            let port = u16::from_be_bytes([port_bytes[0], port_bytes[1]]);
            Ok((host, port, ADDR_DOMAIN))
        }
        0x04 => Err(anyhow!("暂不支持IPv6地址")),
        other => Err(anyhow!("不支持的SOCKS5地址类型: {other}")),
    }
}

async fn read_socks5_bind_address(stream: &mut TcpStream, address_type: u8) -> Result<()> {
    match address_type {
        ADDR_IPV4 => {
            let mut rest = [0u8; 6];
            stream.read_exact(&mut rest).await?;
        }
        ADDR_DOMAIN => {
            let mut len = [0u8; 1];
            stream.read_exact(&mut len).await?;
            let mut rest = vec![0u8; len[0] as usize + 2];
            stream.read_exact(&mut rest).await?;
        }
        0x04 => {
            let mut rest = [0u8; 18];
            stream.read_exact(&mut rest).await?;
        }
        _ => return Err(anyhow!("不支持的SOCKS5绑定地址类型")),
    }
    Ok(())
}

async fn read_http_request_header(
    stream: &mut TcpStream,
    initial: Vec<u8>,
    timeout_ms: u64,
) -> Result<(String, Vec<u8>)> {
    let mut buffer = initial;
    let started_at = Instant::now();
    let maximum_wait = Duration::from_millis(timeout_ms);
    loop {
        if let Some((header, remainder)) = split_http_header(&buffer) {
            if header.len() > 64 * 1024 {
                return Err(anyhow!("HTTP请求头过大"));
            }
            return Ok((
                String::from_utf8(header.to_vec()).context("HTTP请求头不是有效UTF-8")?,
                remainder.to_vec(),
            ));
        }
        let mut temp = [0u8; 1024];
        let remaining = maximum_wait.saturating_sub(started_at.elapsed());
        if remaining.is_zero() {
            return Err(anyhow!("HTTP请求头读取超时"));
        }
        let size = timeout(remaining, stream.read(&mut temp)).await??;
        if size == 0 {
            return Err(anyhow!("HTTP请求头未完整读取"));
        }
        buffer.extend_from_slice(&temp[..size]);
        if buffer.len() > 64 * 1024 && split_http_header(&buffer).is_none() {
            return Err(anyhow!("HTTP请求头过大"));
        }
    }
}

fn split_http_header(buffer: &[u8]) -> Option<(&[u8], &[u8])> {
    let start = buffer.windows(4).position(|window| window == b"\r\n\r\n")?;
    let end = start + 4;
    Some((&buffer[..end], &buffer[end..]))
}

fn http_proxy_authorized(header: &str, auth: &InboundAuth) -> bool {
    if !auth.enabled {
        return true;
    }
    let expected = format!("{}:{}", auth.username, auth.password);
    header.lines().any(|line| {
        let Some((name, value)) = line.split_once(':') else {
            return false;
        };
        if !name.eq_ignore_ascii_case("proxy-authorization") {
            return false;
        }
        let mut parts = value.split_whitespace();
        let Some(scheme) = parts.next() else {
            return false;
        };
        let Some(encoded) = parts.next() else {
            return false;
        };
        if !scheme.eq_ignore_ascii_case("basic") || parts.next().is_some() {
            return false;
        }
        general_purpose::STANDARD
            .decode(encoded)
            .ok()
            .is_some_and(|decoded| constant_time_eq(&decoded, expected.as_bytes()))
    })
}

fn constant_time_eq(left: &[u8], right: &[u8]) -> bool {
    if left.len() != right.len() {
        return false;
    }
    left.iter()
        .zip(right)
        .fold(0u8, |difference, (left, right)| difference | (left ^ right))
        == 0
}

async fn send_http_auth_required(client: &mut TcpStream) -> Result<()> {
    client
        .write_all(
            b"HTTP/1.1 407 Proxy Authentication Required\r\n\
Proxy-Authenticate: Basic realm=\"proxy-load\"\r\n\
Connection: close\r\n\
Content-Length: 0\r\n\r\n",
        )
        .await?;
    Ok(())
}

fn looks_like_http_proxy_request(data: &[u8]) -> bool {
    let prefix = String::from_utf8_lossy(&data[..data.len().min(16)]).to_uppercase();
    [
        "CONNECT ", "GET ", "POST ", "HEAD ", "PUT ", "DELETE ", "OPTIONS ", "PATCH ", "TRACE ",
    ]
    .iter()
    .any(|method| prefix.starts_with(method))
}

async fn sniff_protocol(stream: &mut TcpStream) -> Result<Vec<u8>> {
    const METHODS: [&[u8]; 9] = [
        b"CONNECT ",
        b"GET ",
        b"POST ",
        b"HEAD ",
        b"PUT ",
        b"DELETE ",
        b"OPTIONS ",
        b"PATCH ",
        b"TRACE ",
    ];
    let mut prefix = Vec::with_capacity(8);
    loop {
        prefix.push(stream.read_u8().await?);
        if prefix[0] == SOCKS_VERSION || looks_like_http_proxy_request(&prefix) {
            return Ok(prefix);
        }
        if !METHODS.iter().any(|method| method.starts_with(&prefix)) {
            return Err(anyhow!("不支持的入站代理协议"));
        }
    }
}

fn build_socks5_connect_request(request: &TargetRequest) -> Result<Vec<u8>> {
    let mut packet = vec![0x05, SOCKS_CMD_CONNECT, 0x00];
    match address_type(&request.host)? {
        ADDR_IPV4 => {
            packet.push(ADDR_IPV4);
            let ip = request.host.parse::<Ipv4Addr>()?;
            packet.extend_from_slice(&ip.octets());
        }
        ADDR_DOMAIN => {
            let bytes = request.host.as_bytes();
            if bytes.len() > 255 {
                return Err(anyhow!("目标域名过长"));
            }
            packet.push(ADDR_DOMAIN);
            packet.push(bytes.len() as u8);
            packet.extend_from_slice(bytes);
        }
        _ => return Err(anyhow!("暂不支持IPv6地址")),
    }
    packet.extend_from_slice(&request.port.to_be_bytes());
    Ok(packet)
}

fn parse_authority(authority: &str, default_port: u16) -> Result<(String, u16)> {
    let authority = authority.trim();
    if authority.is_empty() {
        return Err(anyhow!("缺少目标主机"));
    }
    if authority.starts_with('[') || authority.matches(':').count() > 1 {
        return Err(anyhow!("暂不支持IPv6地址"));
    }
    if let Some((host, port)) = authority.rsplit_once(':') {
        let parsed_port = port.parse::<u16>().context("无效端口")?;
        if parsed_port == 0 {
            return Err(anyhow!("目标端口不能为 0"));
        }
        if host.trim().is_empty() {
            return Err(anyhow!("缺少目标主机"));
        }
        Ok((host.trim().to_string(), parsed_port))
    } else {
        Ok((authority.to_string(), default_port))
    }
}

fn format_authority(host: &str, port: u16, default_port: u16) -> String {
    if port == default_port {
        host.to_string()
    } else {
        format!("{host}:{port}")
    }
}

fn address_type(host: &str) -> Result<u8> {
    if host.parse::<Ipv4Addr>().is_ok() {
        return Ok(ADDR_IPV4);
    }
    if host.contains(':') {
        return Err(anyhow!("暂不支持IPv6地址"));
    }
    Ok(ADDR_DOMAIN)
}

fn same_routing_configuration(left: &ProxyRecord, right: &ProxyRecord) -> bool {
    left.proxy_type == right.proxy_type
        && left.host == right.host
        && left.port == right.port
        && left.username == right.username
        && left.password == right.password
        && left.enabled == right.enabled
}

fn score_of(proxy: &ProxyRecord) -> f64 {
    let success = proxy.success_count.max(0) as f64;
    let failed = proxy.fail_count.max(0) as f64;
    let success_rate = if success + failed > 0.0 {
        success / (success + failed)
    } else {
        0.5
    };
    let latency = latency_score(
        proxy
            .response_time
            .filter(|time| *time >= 0)
            .map(|time| time as f64),
    );
    let priority = ((1000 - proxy.priority).clamp(0, 1000) as f64) / 10.0;
    (success_rate * 70.0 + latency * 0.25 + priority * 0.05).clamp(0.01, 100.0)
}

fn latency_score(response_time_ms: Option<f64>) -> f64 {
    match response_time_ms.filter(|time| time.is_finite() && *time >= 0.0) {
        Some(time) => 100.0 / (1.0 + time / 500.0),
        None => 50.0,
    }
}

fn prioritize_route_status(proxies: Vec<ProxyRecord>) -> Vec<ProxyRecord> {
    let mut preferred = Vec::new();
    let mut degraded = Vec::new();
    for proxy in proxies {
        if proxy.status.as_deref() == Some("inactive") {
            degraded.push(proxy);
        } else {
            preferred.push(proxy);
        }
    }
    preferred.extend(degraded);
    preferred
}

impl ProxyMetrics {
    fn new() -> Self {
        Self {
            requests: VecDeque::with_capacity(MAX_METRIC_SAMPLES),
            score: 50.0,
            last_used: monotonic_millis(),
            last_success: 0,
            pushed_status: None,
            successes: 0,
            time_sum: 0,
            time_count: 0,
            learning_remaining: 3,
        }
    }

    fn push(&mut self, success: bool, latency_us: Option<u64>) {
        let now = monotonic_millis();
        self.prune(now);
        if self.requests.len() == MAX_METRIC_SAMPLES {
            self.remove_oldest();
        }
        self.successes += i64::from(success);
        self.learning_remaining = if success {
            self.learning_remaining.saturating_sub(1)
        } else {
            0
        };
        // MAX_METRIC_SAMPLES * one day in microseconds fits comfortably in u64.
        let latency_us = latency_us.map(|time| time.min(86_400_000_000));
        if let Some(time) = latency_us {
            self.time_sum += time;
            self.time_count += 1;
        }
        self.requests.push_back(RequestMetric {
            timestamp: now,
            success,
            latency_us,
        });
        self.last_used = now;
        if success {
            self.last_success = now;
        }
        self.score = self.calculate_score();
    }

    fn prune(&mut self, now: i64) {
        let old_len = self.requests.len();
        while self
            .requests
            .front()
            .map(|metric| now.saturating_sub(metric.timestamp) > METRICS_WINDOW_MS)
            .unwrap_or(false)
        {
            self.remove_oldest();
        }
        if self.requests.len() != old_len {
            self.score = if self.requests.is_empty() {
                50.0
            } else {
                self.calculate_score()
            };
        }
    }

    fn summary(&self) -> (i64, i64, Option<f64>) {
        (
            self.successes,
            self.requests.len() as i64 - self.successes,
            if self.time_count == 0 {
                None
            } else {
                Some(self.time_sum as f64 / self.time_count as f64 / 1000.0)
            },
        )
    }

    fn remove_oldest(&mut self) {
        if let Some(sample) = self.requests.pop_front() {
            self.successes -= i64::from(sample.success);
            if let Some(time) = sample.latency_us {
                self.time_sum -= time;
                self.time_count -= 1;
            }
        }
    }

    fn calculate_score(&self) -> f64 {
        let (success, failed, avg_rt) = self.summary();
        // A small neutral prior avoids treating a single fast success as certainty.
        let total = (success + failed + 4) as f64;
        let success_rate = (success + 2) as f64 / total;
        (success_rate * 75.0 + latency_score(avg_rt) * 0.25).clamp(0.01, 100.0)
    }
}

pub(crate) fn monotonic_millis() -> i64 {
    static START: std::sync::OnceLock<Instant> = std::sync::OnceLock::new();
    START
        .get_or_init(Instant::now)
        .elapsed()
        .as_millis()
        .min((i64::MAX - 1) as u128) as i64
        + 1
}

/// Uses the same TCP/auth/target failure boundary as business routing, without health mutation.
pub async fn probe_connection_health(
    proxy: &ProxyRecord,
    target: &url::Url,
    budget: Duration,
) -> Result<(), (&'static str, String)> {
    let host = target.host_str().unwrap_or_default();
    let request = TargetRequest {
        host: host.into(),
        original_host: host.into(),
        port: target.port_or_known_default().unwrap_or(443),
        address_type: address_type(host).unwrap_or(ADDR_DOMAIN),
        inbound: InboundProtocol::HttpConnect,
        initial_payload: Vec::new(),
    };
    let mut scope = FailureScope::Proxy;
    timeout(budget, connect_through_proxy(proxy, &request, &mut scope))
        .await
        .unwrap_or_else(|_| Err(anyhow!("{}超时", scope.label())))
        .map(|_| ())
        .map_err(|error| {
            (
                if is_local_network_error(&error) {
                    "network"
                } else if scope == FailureScope::Proxy {
                    "proxy"
                } else {
                    "target"
                },
                format!("{}: {error:#}", scope.label()),
            )
        })
}

fn is_local_network_error(error: &anyhow::Error) -> bool {
    error
        .chain()
        .filter_map(|cause| cause.downcast_ref::<std::io::Error>())
        .any(|io| {
            matches!(
                io.kind(),
                std::io::ErrorKind::NetworkDown | std::io::ErrorKind::NetworkUnreachable
            ) || cfg!(windows) && matches!(io.raw_os_error(), Some(10050 | 10051))
                || cfg!(target_os = "linux") && matches!(io.raw_os_error(), Some(100 | 101))
                || cfg!(target_os = "macos") && matches!(io.raw_os_error(), Some(50 | 51))
        })
}

impl CircuitBreaker {
    fn new(config: CircuitConfig) -> Self {
        Self {
            state: "CLOSED".to_string(),
            failures: 0,
            threshold: config.failure_threshold,
            timeout_ms: config.timeout_ms,
            next_attempt: 0,
            attempt: None,
        }
    }

    fn apply_config(&mut self, config: CircuitConfig) {
        if self.state == "OPEN" && self.timeout_ms != config.timeout_ms {
            let opened_at = self.next_attempt.saturating_sub(self.timeout_ms);
            self.next_attempt = opened_at.saturating_add(config.timeout_ms);
        }
        self.threshold = config.failure_threshold;
        self.timeout_ms = config.timeout_ms;
    }

    fn try_begin_attempt(&mut self) -> bool {
        if self.state == "CLOSED" {
            return true;
        }
        if self.can_attempt_snapshot() {
            self.state = "HALF_OPEN".to_string();
            return true;
        }
        false
    }

    fn can_attempt_snapshot(&self) -> bool {
        if self.state == "CLOSED" {
            return true;
        }
        (self.state == "OPEN" && monotonic_millis() >= self.next_attempt)
            || (self.state == "HALF_OPEN"
                && self
                    .attempt
                    .as_ref()
                    .is_some_and(|token| token.upgrade().is_none()))
    }

    fn half_open_in_flight(&self) -> bool {
        self.state == "HALF_OPEN"
            && self
                .attempt
                .as_ref()
                .is_none_or(|token| token.strong_count() > 0)
    }

    fn record_success(&mut self) {
        self.failures = 0;
        self.state = "CLOSED".to_string();
        self.next_attempt = 0;
        self.attempt = None;
    }

    fn record_failure(&mut self) {
        self.failures += 1;
        if self.state == "HALF_OPEN" || self.failures >= self.threshold {
            self.state = "OPEN".to_string();
            self.next_attempt = monotonic_millis().saturating_add(self.timeout_ms);
            self.attempt = None;
        }
    }

    fn cancel_half_open_attempt(&mut self) {
        if self.state == "HALF_OPEN" {
            self.state = "OPEN".to_string();
            self.next_attempt = monotonic_millis().saturating_add(self.timeout_ms);
            self.attempt = None;
        }
    }
}

#[cfg(test)]
mod routing_tests;

#[cfg(test)]
mod benchmarks;

#[cfg(test)]
mod acceptance_tests;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn submillisecond_latency_score_is_monotonic_and_unknown_is_explicit() {
        let scores = [0, 200, 900, 1000, 5000].map(|latency| {
            let mut metrics = ProxyMetrics::new();
            metrics.push(true, Some(latency));
            assert_eq!(metrics.summary().2, Some(latency as f64 / 1000.0));
            metrics.score
        });
        assert!(scores.windows(2).all(|pair| pair[0] > pair[1]));
        let mut unknown = ProxyMetrics::new();
        unknown.push(true, None);
        assert!(scores[4] > unknown.score);
        assert_eq!(unknown.summary().2, None);
        assert!(latency_score(Some(0.0)) > latency_score(None));
        assert_eq!(latency_score(Some(f64::NAN)), latency_score(None));
    }

    #[test]
    fn microsecond_metrics_keep_zero_and_bound_extreme_samples_without_overflow() {
        let mut metrics = ProxyMetrics::new();
        for _ in 0..MAX_METRIC_SAMPLES * 2 {
            metrics.push(true, Some(u64::MAX));
        }
        assert_eq!(metrics.time_sum, MAX_METRIC_SAMPLES as u64 * 86_400_000_000);
        assert_eq!(metrics.summary().2, Some(86_400_000.0));
        for _ in 0..MAX_METRIC_SAMPLES {
            metrics.push(true, Some(0));
        }
        assert_eq!(metrics.time_sum, 0);
        assert_eq!(metrics.time_count, MAX_METRIC_SAMPLES as u64);
        assert_eq!(metrics.summary().2, Some(0.0));
    }

    #[test]
    fn prioritize_route_status_keeps_inactive_candidates_after_preferred() {
        let ordered = prioritize_route_status(vec![
            proxy_with_status(1, "inactive"),
            proxy_with_status(2, "active"),
            proxy_with_status(3, "unknown"),
            proxy_with_status(4, "inactive"),
        ]);

        let ids = ordered.iter().map(|proxy| proxy.id).collect::<Vec<_>>();
        assert_eq!(ids, vec![2, 3, 1, 4]);
    }

    #[test]
    fn routing_results_are_invalidated_when_proxy_endpoint_changes() {
        let original = proxy_with_status(1, "active");
        let mut updated = original.clone();
        updated.status = Some("inactive".to_string());
        assert!(same_routing_configuration(&original, &updated));

        updated.host = "127.0.0.2".to_string();
        assert!(!same_routing_configuration(&original, &updated));

        let mut updated = original.clone();
        updated.test_url = Some("https://probe.example.test/".into());
        assert!(!crate::routing::same_node(&original, &updated));
        let mut updated = original.clone();
        updated.test_timeout = Some(original.test_timeout.unwrap_or(0) + 1);
        assert!(!crate::routing::same_node(&original, &updated));
        let mut updated = original.clone();
        updated.skip_cert_verify = 1 - original.skip_cert_verify;
        assert!(!crate::routing::same_node(&original, &updated));
    }

    #[test]
    fn adaptive_score_distinguishes_latency_and_failures_without_saturating() {
        let mut fast = proxy_with_status(1, "active");
        fast.success_count = 10;
        fast.response_time = Some(100);
        let mut slow = fast.clone();
        slow.response_time = Some(1000);
        let mut unreliable = fast.clone();
        unreliable.fail_count = 10;

        assert!(score_of(&fast) < 100.0);
        assert!(score_of(&fast) > score_of(&slow));
        assert!(score_of(&fast) > score_of(&unreliable));
    }

    #[test]
    fn circuit_breaker_uses_configured_threshold_and_single_half_open_probe() {
        let config = CircuitConfig {
            failure_threshold: 2,
            timeout_ms: 60_000,
        };
        let mut breaker = CircuitBreaker::new(config);
        breaker.record_failure();
        assert_eq!(breaker.state, "CLOSED");
        breaker.record_failure();
        assert_eq!(breaker.state, "OPEN");

        breaker.next_attempt = 0;
        assert!(breaker.can_attempt_snapshot());
        assert_eq!(breaker.state, "OPEN");
        assert!(breaker.try_begin_attempt());
        assert_eq!(breaker.state, "HALF_OPEN");
        assert!(!breaker.try_begin_attempt());
        breaker.record_success();
        assert_eq!(breaker.state, "CLOSED");
    }

    #[test]
    fn circuit_breaker_applies_threshold_and_timeout_changes_to_existing_state() {
        let mut breaker = CircuitBreaker::new(CircuitConfig {
            failure_threshold: 2,
            timeout_ms: 60_000,
        });
        breaker.record_failure();
        breaker.record_failure();
        assert_eq!(breaker.state, "OPEN");
        let old_deadline = breaker.next_attempt;

        breaker.apply_config(CircuitConfig {
            failure_threshold: 4,
            timeout_ms: 10_000,
        });
        assert_eq!(breaker.threshold, 4);
        assert_eq!(breaker.next_attempt, old_deadline - 50_000);

        breaker.next_attempt = 0;
        assert!(breaker.try_begin_attempt());
        breaker.cancel_half_open_attempt();
        assert_eq!(breaker.state, "OPEN");
        assert!(breaker.next_attempt > 0);
    }

    #[test]
    fn http_proxy_auth_requires_matching_basic_credentials() {
        let auth = InboundAuth {
            enabled: true,
            username: "proxy-user".to_string(),
            password: "secret-password".to_string(),
        };
        let valid = general_purpose::STANDARD.encode("proxy-user:secret-password");
        let invalid = general_purpose::STANDARD.encode("proxy-user:wrong");

        assert!(http_proxy_authorized(
            &format!(
                "CONNECT example.com:443 HTTP/1.1\r\nProxy-Authorization: Basic {valid}\r\n\r\n"
            ),
            &auth
        ));
        assert!(!http_proxy_authorized(
            &format!(
                "CONNECT example.com:443 HTTP/1.1\r\nproxy-authorization: Basic {invalid}\r\n\r\n"
            ),
            &auth
        ));
        assert!(!http_proxy_authorized(
            "CONNECT example.com:443 HTTP/1.1\r\n\r\n",
            &auth
        ));
    }

    #[test]
    fn disabled_http_proxy_auth_accepts_missing_credentials() {
        let auth = InboundAuth::default();
        assert!(http_proxy_authorized(
            "CONNECT example.com:443 HTTP/1.1\r\n\r\n",
            &auth
        ));
    }

    #[tokio::test]
    async fn socks5_auth_accepts_coalesced_handshake_and_preserves_payload() {
        let listener = TcpListener::bind(("127.0.0.1", 0)).await.unwrap();
        let address = listener.local_addr().unwrap();
        let auth = InboundAuth {
            enabled: true,
            username: "proxy-user".to_string(),
            password: "secret-password".to_string(),
        };
        let server = tokio::spawn(async move {
            let (mut stream, _) = listener.accept().await.unwrap();
            let initial = read_some_with_timeout(&mut stream, 1000).await.unwrap();
            handle_socks5_handshake(&mut stream, initial, &auth).await
        });

        let mut client = TcpStream::connect(address).await.unwrap();
        let mut packet = vec![SOCKS_VERSION, 0x01, SOCKS_AUTH_USERNAME_PASSWORD];
        packet.extend_from_slice(&[0x01, 10]);
        packet.extend_from_slice(b"proxy-user");
        packet.push(15);
        packet.extend_from_slice(b"secret-password");
        packet.extend_from_slice(&[SOCKS_VERSION, SOCKS_CMD_CONNECT, 0x00, ADDR_DOMAIN, 11]);
        packet.extend_from_slice(b"example.com");
        packet.extend_from_slice(&443u16.to_be_bytes());
        packet.extend_from_slice(b"prefetched-payload");
        client.write_all(&packet).await.unwrap();

        let mut responses = [0u8; 4];
        client.read_exact(&mut responses).await.unwrap();
        assert_eq!(
            responses,
            [SOCKS_VERSION, SOCKS_AUTH_USERNAME_PASSWORD, 0x01, 0x00]
        );

        let request = server.await.unwrap().unwrap();
        assert_eq!(request.host, "example.com");
        assert_eq!(request.port, 443);
        assert_eq!(request.initial_payload, b"prefetched-payload");
    }

    #[test]
    fn http_header_split_preserves_prefetched_body() {
        let request =
            b"POST http://example.com/upload HTTP/1.1\r\nHost: example.com\r\n\r\npayload";
        let (header, remainder) = split_http_header(request).expect("应识别完整请求头");

        assert_eq!(
            header,
            b"POST http://example.com/upload HTTP/1.1\r\nHost: example.com\r\n\r\n"
        );
        assert_eq!(remainder, b"payload");
    }

    #[test]
    fn http_forward_canonicalizes_host_and_rejects_absolute_https() {
        let request = parse_http_proxy_request(
            "GET http://example.com:8080/path HTTP/1.1\r\nHost: wrong.example\r\nConnection: keep-alive\r\n\r\n",
            Vec::new(),
        )
        .unwrap();
        let payload = String::from_utf8(request.initial_payload).unwrap();
        assert!(payload.starts_with("GET /path HTTP/1.1\r\nHost: example.com:8080\r\n"));
        assert!(payload.contains("Connection: close\r\n"));
        assert!(!payload.contains("wrong.example"));

        let origin_form = parse_http_proxy_request(
            "GET /fetch?url=http://nested.example/ HTTP/1.1\r\nHost: example.com\r\n\r\n",
            Vec::new(),
        )
        .unwrap();
        assert_eq!(origin_form.host, "example.com");
        assert!(String::from_utf8(origin_form.initial_payload)
            .unwrap()
            .starts_with("GET /fetch?url=http://nested.example/ HTTP/1.1\r\n"));

        assert!(parse_http_proxy_request(
            "GET https://example.com/ HTTP/1.1\r\nHost: example.com\r\n\r\n",
            Vec::new(),
        )
        .is_err());
    }

    #[test]
    fn rewrite_http_header_removes_proxy_credentials_case_insensitively() {
        let header = "GET http://example.com/path HTTP/1.1\r\nHost: wrong.example\r\nConnection: keep-alive\r\nKeep-Alive: timeout=5\r\nproxy-authorization: Basic c2VjcmV0\r\nPROXY-CONNECTION: keep-alive\r\n\r\n";
        let rewritten = rewrite_http_forward_header(header, "GET", "/path", "example.com");

        assert!(!rewritten
            .to_ascii_lowercase()
            .contains("proxy-authorization"));
        assert!(!rewritten.to_ascii_lowercase().contains("proxy-connection"));
        assert!(rewritten.starts_with("GET /path HTTP/1.1\r\n"));
        assert!(rewritten.contains("Host: example.com\r\n"));
        assert!(rewritten.contains("Connection: close\r\n"));
        assert!(!rewritten.contains("wrong.example"));
        assert!(!rewritten.to_ascii_lowercase().contains("keep-alive"));
    }

    #[test]
    fn http_forward_preserves_websocket_upgrade_semantics() {
        let header = "GET http://example.com/socket HTTP/1.1\r\nHost: example.com\r\nConnection: keep-alive, Upgrade\r\nUpgrade: websocket\r\n\r\n";
        let rewritten = rewrite_http_forward_header(header, "GET", "/socket", "example.com");

        assert!(rewritten.contains("Upgrade: websocket\r\n"));
        assert!(rewritten.contains("Connection: Upgrade\r\n"));
        assert!(!rewritten.contains("Connection: close\r\n"));
    }

    #[test]
    fn http_upstream_forward_uses_absolute_uri_auth_and_preserves_body() {
        let mut proxy = proxy_with_status(1, "active");
        proxy.proxy_type = "http".to_string();
        proxy.username = Some("upstream-user".to_string());
        proxy.password = Some("upstream-password".to_string());
        let request = TargetRequest {
            host: "example.com".to_string(),
            port: 8080,
            address_type: ADDR_DOMAIN,
            original_host: "example.com".to_string(),
            inbound: InboundProtocol::HttpForward,
            initial_payload:
                b"POST /upload?q=1 HTTP/1.1\r\nHost: example.com:8080\r\nProxy-Authorization: Basic bG9jYWw=\r\nContent-Length: 7\r\nConnection: close\r\n\r\npayload"
                    .to_vec(),
        };

        let payload = build_http_forward_proxy_payload(&proxy, &request).unwrap();
        let payload = String::from_utf8(payload).unwrap();
        let expected_auth = general_purpose::STANDARD.encode("upstream-user:upstream-password");

        assert!(payload.starts_with("POST http://example.com:8080/upload?q=1 HTTP/1.1\r\n"));
        assert!(payload.contains(&format!("Proxy-Authorization: Basic {expected_auth}\r\n")));
        assert_eq!(payload.matches("Proxy-Authorization:").count(), 1);
        assert!(payload.contains("Proxy-Connection: close\r\n"));
        assert!(payload.ends_with("\r\npayload"));
    }

    fn proxy_with_status(id: i64, status: &str) -> ProxyRecord {
        ProxyRecord {
            id,
            name: format!("proxy-{id}"),
            proxy_type: "socks5".to_string(),
            host: "127.0.0.1".to_string(),
            port: 1080,
            username: None,
            password: None,
            status: Some(status.to_string()),
            last_test: None,
            response_time: None,
            success_count: 0,
            fail_count: 0,
            priority: 999,
            enabled: 1,
            skip_cert_verify: 0,
            test_url: None,
            test_timeout: None,
            score: None,
            active_connections: None,
        }
    }
}
