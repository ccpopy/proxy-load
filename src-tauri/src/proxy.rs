use std::{
    collections::{HashMap, VecDeque},
    net::{Ipv4Addr, SocketAddr},
    sync::Arc,
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
    time::{sleep, timeout, Duration},
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

#[derive(Clone)]
pub struct ProxyRuntime {
    db: Database,
    events: broadcast::Sender<ServerEvent>,
    service_status: Arc<RwLock<ProxyServiceStatus>>,
    metrics: Arc<RwLock<HashMap<i64, ProxyMetrics>>>,
    circuit_breakers: Arc<RwLock<HashMap<i64, CircuitBreaker>>>,
    active_connections: Arc<RwLock<HashMap<i64, i64>>>,
    dns_cache: Arc<RwLock<HashMap<String, String>>>,
    round_robin_index: Arc<Mutex<usize>>,
    runtime_settings: Arc<RwLock<RuntimeSettings>>,
    status_locks: Arc<Mutex<HashMap<i64, Arc<Mutex<()>>>>>,
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

struct ConnectedUpstream {
    stream: TcpStream,
    outbound_initial_payload: Option<Vec<u8>>,
    prefetched_response: Vec<u8>,
    target_verified: bool,
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
}

#[derive(Debug, Clone)]
struct RequestMetric {
    timestamp: i64,
    success: bool,
    response_time: Option<i64>,
}

#[derive(Debug, Clone, Serialize)]
struct CircuitBreaker {
    state: String,
    failures: i64,
    threshold: i64,
    timeout_ms: i64,
    next_attempt: i64,
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
        Ok(Self {
            db,
            events,
            service_status: Arc::new(RwLock::new(ProxyServiceStatus {
                state: "starting".to_string(),
                running: false,
                host: listen_host.to_string(),
                port: listen_port,
                error: Some("代理服务正在启动".to_string()),
            })),
            metrics: Arc::new(RwLock::new(HashMap::new())),
            circuit_breakers: Arc::new(RwLock::new(HashMap::new())),
            active_connections: Arc::new(RwLock::new(HashMap::new())),
            dns_cache: Arc::new(RwLock::new(HashMap::new())),
            round_robin_index: Arc::new(Mutex::new(0)),
            runtime_settings: Arc::new(RwLock::new(runtime_settings)),
            status_locks: Arc::new(Mutex::new(HashMap::new())),
        })
    }

    pub async fn service_status(&self) -> ProxyServiceStatus {
        self.service_status.read().await.clone()
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
        let mappings = self.db.active_dns_mappings()?;
        *self.dns_cache.write().await = mappings;
        Ok(())
    }

    pub async fn update_advanced_config(&self, advanced: &Value) -> Result<()> {
        let mut current = self.runtime_settings.write().await;
        let mut next = RuntimeSettings::from_advanced(advanced)?;
        next.algorithm = current.algorithm.clone();
        let circuit = next.circuit;
        *current = next;
        drop(current);
        for breaker in self.circuit_breakers.write().await.values_mut() {
            breaker.apply_config(circuit);
        }
        Ok(())
    }

    pub async fn update_load_settings(&self, settings: &Map<String, Value>) -> Result<()> {
        if let Some(algorithm) = settings.get("algorithm").and_then(Value::as_str) {
            self.runtime_settings
                .write()
                .await
                .set_algorithm(algorithm)?;
        }
        Ok(())
    }

    pub async fn reset_proxy_state(&self, proxy_id: i64) {
        self.metrics.write().await.remove(&proxy_id);
        self.circuit_breakers.write().await.remove(&proxy_id);
    }

    fn proxy_configuration_is_current(&self, proxy: &ProxyRecord) -> Result<bool> {
        Ok(self
            .db
            .get_proxy(proxy.id)?
            .is_some_and(|latest| same_routing_configuration(proxy, &latest)))
    }

    async fn inbound_auth(&self) -> InboundAuth {
        self.runtime_settings.read().await.inbound_auth.clone()
    }

    pub async fn stats(&self) -> HashMap<i64, ProxyRuntimeStats> {
        let metrics = self.metrics.read().await;
        let active = self.active_connections.read().await;
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
        let breakers = self.circuit_breakers.read().await;
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

    async fn increment_active(&self, proxy_id: i64) {
        let mut active = self.active_connections.write().await;
        *active.entry(proxy_id).or_insert(0) += 1;
    }

    async fn decrement_active(&self, proxy_id: i64) {
        let mut active = self.active_connections.write().await;
        match active.get_mut(&proxy_id) {
            Some(value) if *value > 1 => *value -= 1,
            Some(_) => {
                active.remove(&proxy_id);
            }
            None => {}
        }
    }

    async fn record_request(
        &self,
        entry: RequestLogEntry<'_>,
        expected_proxy: Option<&ProxyRecord>,
    ) {
        if let Some(expected_proxy) = expected_proxy {
            let proxy_id = expected_proxy.id;
            let _guard = self.lock_proxy_status(proxy_id).await;
            match self.proxy_configuration_is_current(expected_proxy) {
                Ok(true) => {
                    let mark_active = {
                        let mut metrics = self.metrics.write().await;
                        let metric = metrics.entry(proxy_id).or_insert_with(ProxyMetrics::new);
                        metric.push(entry.success, entry.response_time);
                        entry.success && metric.pushed_status.as_deref() != Some("active")
                    };
                    if mark_active
                        && self.apply_passive_status_locked(proxy_id, "active", entry.response_time)
                    {
                        let mut metrics = self.metrics.write().await;
                        let metric = metrics.entry(proxy_id).or_insert_with(ProxyMetrics::new);
                        metric.pushed_status = Some("active".to_string());
                    }
                }
                Ok(false) => {}
                Err(error) => {
                    eprintln!("记录真实流量状态前校验代理配置失败: {error:#}");
                }
            }
        }

        if let Err(db_error) = self.db.log_request(&entry) {
            eprintln!("写入请求日志失败: {db_error:#}");
        }

        let event = ServerEvent {
            event_type: "request_logged".to_string(),
            data: Value::Null,
            timestamp: now_millis(),
        };
        let _ = self.events.send(event);
    }

    async fn select_proxies(&self, request: &TargetRequest) -> Result<Vec<ProxyRecord>> {
        let mut proxies = self.db.list_enabled_proxies()?;
        if proxies.is_empty() {
            return Err(anyhow!("没有可用的代理"));
        }

        let group_key = request.original_host.to_lowercase();
        if let Some(selection) = self.db.group_proxy_selection(&group_key)? {
            #[cfg(debug_assertions)]
            eprintln!(
                "代理路由命中: target={group_key}, group={}, candidates={:?}",
                selection.group_name, selection.proxy_ids
            );
            proxies.retain(|proxy| selection.proxy_ids.contains(&proxy.id));
            if proxies.is_empty() {
                return Err(anyhow!(
                    "目标 {} 匹配的代理分组「{}」没有可用代理",
                    group_key,
                    selection.group_name
                ));
            }
        } else {
            #[cfg(debug_assertions)]
            eprintln!("代理路由未命中分组: target={group_key}, 使用全部已启用代理");
        }

        let algorithm = self.runtime_settings.read().await.algorithm.clone();

        let mut eligible = Vec::new();
        for proxy in proxies {
            if !self.is_candidate_available(proxy.id).await {
                continue;
            }
            eligible.push(proxy);
        }

        if eligible.is_empty() {
            return Err(anyhow!("没有可尝试的代理"));
        }

        self.order_proxies(eligible, &algorithm, &group_key).await
    }

    async fn order_proxies(
        &self,
        mut proxies: Vec<ProxyRecord>,
        algorithm: &str,
        host_key: &str,
    ) -> Result<Vec<ProxyRecord>> {
        match algorithm {
            "least_connections" => {
                let active = self.active_connections.read().await;
                proxies.sort_by(|a, b| {
                    active
                        .get(&a.id)
                        .copied()
                        .unwrap_or(0)
                        .cmp(&active.get(&b.id).copied().unwrap_or(0))
                        .then_with(|| score_of(b).total_cmp(&score_of(a)))
                });
            }
            "round_robin" => {
                let mut index = self.round_robin_index.lock().await;
                if !proxies.is_empty() {
                    let selected = *index % proxies.len();
                    proxies.rotate_left(selected);
                    *index = index.saturating_add(1);
                }
            }
            "sticky_host" => {
                if !proxies.is_empty() && !host_key.is_empty() {
                    let idx = stable_hash(host_key) % proxies.len();
                    proxies.rotate_left(idx);
                }
            }
            _ => {
                let metrics = self.metrics.read().await;
                proxies.sort_by(|a, b| {
                    let a_score = metrics
                        .get(&a.id)
                        .filter(|metric| !metric.requests.is_empty())
                        .map(|metric| metric.score)
                        .unwrap_or_else(|| score_of(a));
                    let b_score = metrics
                        .get(&b.id)
                        .filter(|metric| !metric.requests.is_empty())
                        .map(|metric| metric.score)
                        .unwrap_or_else(|| score_of(b));
                    b_score.total_cmp(&a_score)
                });
            }
        }
        Ok(prioritize_route_status(proxies))
    }

    async fn is_candidate_available(&self, proxy_id: i64) -> bool {
        self.circuit_breakers
            .read()
            .await
            .get(&proxy_id)
            .is_none_or(CircuitBreaker::can_attempt_snapshot)
    }

    async fn try_begin_attempt(&self, proxy_id: i64) -> bool {
        let config = self.runtime_settings.read().await.circuit;
        let mut breakers = self.circuit_breakers.write().await;
        let breaker = breakers
            .entry(proxy_id)
            .or_insert_with(|| CircuitBreaker::new(config));
        breaker.apply_config(config);
        breaker.try_begin_attempt()
    }

    async fn record_breaker_success(&self, proxy_id: i64) {
        let config = self.runtime_settings.read().await.circuit;
        let mut breakers = self.circuit_breakers.write().await;
        let breaker = breakers
            .entry(proxy_id)
            .or_insert_with(|| CircuitBreaker::new(config));
        breaker.apply_config(config);
        breaker.record_success();
    }

    async fn cancel_half_open_attempt(&self, proxy_id: i64) {
        if let Some(breaker) = self.circuit_breakers.write().await.get_mut(&proxy_id) {
            breaker.cancel_half_open_attempt();
        }
    }

    async fn record_breaker_failure_locked(&self, proxy_id: i64) {
        let config = self.runtime_settings.read().await.circuit;
        {
            let mut metrics = self.metrics.write().await;
            metrics
                .entry(proxy_id)
                .or_insert_with(ProxyMetrics::new)
                .push(false, None);
        }
        let just_opened = {
            let mut breakers = self.circuit_breakers.write().await;
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
            if self.apply_passive_status_locked(proxy_id, "inactive", None) {
                let mut metrics = self.metrics.write().await;
                let metric = metrics.entry(proxy_id).or_insert_with(ProxyMetrics::new);
                metric.pushed_status = Some("inactive".to_string());
            }
        }
    }

    /// 各代理最近一次“真实流量成功”的时间戳（毫秒），供主动测活判断是否可跳过。
    pub async fn recent_success_map(&self) -> HashMap<i64, i64> {
        self.metrics
            .read()
            .await
            .iter()
            .map(|(id, metric)| (*id, metric.last_success))
            .collect()
    }

    pub async fn record_probe_result(
        &self,
        proxy: &ProxyRecord,
        probe_started_at: i64,
        desired_status: Option<&str>,
        response_time: Option<i64>,
        success: bool,
    ) -> Result<(Option<String>, bool)> {
        let proxy_id = proxy.id;
        let status_lock = self.status_lock(proxy_id).await;
        let _guard = status_lock.lock().await;
        let traffic_proved_alive = self
            .metrics
            .read()
            .await
            .get(&proxy_id)
            .is_some_and(|metric| metric.last_success > probe_started_at);
        let applied_status = if desired_status == Some("inactive") && traffic_proved_alive {
            None
        } else {
            desired_status
        };
        if !self
            .db
            .record_proxy_probe_result(proxy, applied_status, response_time, success)?
        {
            return Err(anyhow!(
                "代理配置在测试结果写入前发生变化，已丢弃旧测试结果"
            ));
        }
        if success {
            self.record_breaker_success(proxy_id).await;
        }
        if let Some(status) = applied_status {
            let mut metrics = self.metrics.write().await;
            let metric = metrics.entry(proxy_id).or_insert_with(ProxyMetrics::new);
            metric.pushed_status = Some(status.to_string());
        }
        Ok((applied_status.map(str::to_string), traffic_proved_alive))
    }

    async fn status_lock(&self, proxy_id: i64) -> Arc<Mutex<()>> {
        let mut locks = self.status_locks.lock().await;
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
        match self
            .db
            .update_proxy_status(proxy_id, status, response_time, 0, 0)
        {
            Ok(true) => {}
            Ok(false) => return false,
            Err(error) => {
                eprintln!("被动更新代理状态失败 proxy_id={proxy_id}: {error:#}");
                return false;
            }
        }
        if let Ok(Some(proxy)) = self.db.get_proxy(proxy_id) {
            let _ = self.events.send(ServerEvent {
                event_type: "proxy_tested".to_string(),
                data: json!({
                    "proxy": proxy,
                    "result": {
                        "success": status == "active",
                        "responseTime": response_time.unwrap_or(0)
                    },
                    "passive": true
                }),
                timestamp: now_millis(),
            });
        }
        true
    }

    async fn resolve_target(&self, mut request: TargetRequest) -> TargetRequest {
        if request.address_type == ADDR_DOMAIN {
            let mappings = self.dns_cache.read().await;
            if let Some(mapped) = mappings.get(&request.host.to_lowercase()) {
                request.host = mapped.clone();
                request.address_type = ADDR_IPV4;
            }
        }
        request
    }
}

pub async fn serve(runtime: Arc<ProxyRuntime>, host: String, port: u16) -> Result<()> {
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

    loop {
        let (client, addr) = match listener.accept().await {
            Ok(accepted) => accepted,
            Err(error) => {
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
        tokio::spawn(async move {
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
        let initial = read_some_with_timeout(&mut client, 1000).await?;
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
    match connect_with_fail_fast(runtime.clone(), &request, start).await {
        Ok((selected_proxy, mut upstream)) => {
            let proxy_id = selected_proxy.id;
            if let Err(error) =
                complete_client_handshake(&mut client, &mut upstream, &request).await
            {
                runtime.decrement_active(proxy_id).await;
                let error_message = error.to_string();
                runtime
                    .record_request(
                        RequestLogEntry {
                            proxy_id: Some(proxy_id),
                            target_host: &original_host,
                            target_port: i64::from(original_port),
                            success: false,
                            response_time: Some(start.elapsed().as_millis() as i64),
                            error_message: Some(&error_message),
                            result_type: "tunnel_setup_error",
                        },
                        None,
                    )
                    .await;
                return Err(error);
            }
            let response_time = start.elapsed().as_millis() as i64;
            runtime
                .record_request(
                    RequestLogEntry {
                        proxy_id: Some(proxy_id),
                        target_host: &original_host,
                        target_port: i64::from(original_port),
                        success: true,
                        response_time: Some(response_time),
                        error_message: None,
                        result_type: if upstream.target_verified {
                            "proxy_connected"
                        } else {
                            "request_forwarded"
                        },
                    },
                    upstream.target_verified.then_some(&selected_proxy),
                )
                .await;
            let copy_result = io::copy_bidirectional(&mut client, &mut upstream.stream).await;
            runtime.decrement_active(proxy_id).await;
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
                .record_request(
                    RequestLogEntry {
                        proxy_id: None,
                        target_host: &original_host,
                        target_port: i64::from(original_port),
                        success: false,
                        response_time: Some(start.elapsed().as_millis() as i64),
                        error_message: Some(&error_message),
                        result_type: "proxy_exhausted",
                    },
                    None,
                )
                .await;
            Err(error)
        }
    }
}

async fn connect_with_fail_fast(
    runtime: Arc<ProxyRuntime>,
    request: &TargetRequest,
    start: Instant,
) -> Result<(ProxyRecord, ConnectedUpstream)> {
    let config = runtime.runtime_settings.read().await.fail_fast;

    let proxies = runtime.select_proxies(request).await?;

    let total_timeout = Duration::from_millis(config.total_timeout_ms);
    let proxy_count = proxies.len();
    let mut attempted = 0usize;
    let mut errors = Vec::new();
    for (index, proxy) in proxies.into_iter().enumerate() {
        if config.enabled && attempted >= config.max_attempts {
            break;
        }
        let remaining = total_timeout.saturating_sub(start.elapsed());
        if remaining.is_zero() {
            return Err(anyhow!("总超时: {}", errors.join("; ")));
        }
        if !runtime.try_begin_attempt(proxy.id).await {
            continue;
        }
        attempted += 1;
        runtime.increment_active(proxy.id).await;
        let attempt = timeout(
            Duration::from_millis(config.attempt_timeout_ms).min(remaining),
            connect_through_proxy(&proxy, request),
        )
        .await;
        let status_guard = runtime.lock_proxy_status(proxy.id).await;
        let configuration_current = match runtime.proxy_configuration_is_current(&proxy) {
            Ok(current) => current,
            Err(error) => {
                runtime.decrement_active(proxy.id).await;
                runtime.cancel_half_open_attempt(proxy.id).await;
                return Err(error.context("连接结束后校验代理配置失败"));
            }
        };
        if !configuration_current {
            runtime.decrement_active(proxy.id).await;
            attempted = attempted.saturating_sub(1);
            errors.push(format!("{}: 连接期间代理配置已变化", proxy.name));
            continue;
        }

        match attempt {
            Ok(Ok(stream)) => {
                runtime.record_breaker_success(proxy.id).await;
                #[cfg(debug_assertions)]
                eprintln!(
                    "代理路由成功: target={}:{}, proxy_id={}, proxy_name={}, proxy_type={}",
                    request.original_host, request.port, proxy.id, proxy.name, proxy.proxy_type
                );
                return Ok((proxy, stream));
            }
            Ok(Err(error)) => {
                runtime.record_breaker_failure_locked(proxy.id).await;
                runtime.decrement_active(proxy.id).await;
                eprintln!(
                    "代理路由尝试失败: target={}:{}, proxy_id={}, proxy_name={}, error={error}",
                    request.original_host, request.port, proxy.id, proxy.name
                );
                errors.push(format!("{}: {error}", proxy.name));
            }
            Err(_) => {
                runtime.record_breaker_failure_locked(proxy.id).await;
                runtime.decrement_active(proxy.id).await;
                eprintln!(
                    "代理路由尝试超时: target={}:{}, proxy_id={}, proxy_name={}",
                    request.original_host, request.port, proxy.id, proxy.name
                );
                errors.push(format!("{}: 连接超时", proxy.name));
            }
        }
        drop(status_guard);
        if index + 1 < proxy_count && (!config.enabled || attempted < config.max_attempts) {
            let delay =
                Duration::from_millis(300).min(total_timeout.saturating_sub(start.elapsed()));
            if !delay.is_zero() {
                sleep(delay).await;
            }
        }
    }

    if attempted == 0 && errors.is_empty() {
        return Err(anyhow!("所有候选代理当前均处于熔断状态"));
    }
    Err(anyhow!("所有代理都失败: {}", errors.join("; ")))
}

async fn connect_through_proxy(
    proxy: &ProxyRecord,
    request: &TargetRequest,
) -> Result<ConnectedUpstream> {
    match proxy.proxy_type.as_str() {
        "socks5" => connect_socks5(proxy, request)
            .await
            .map(|stream| ConnectedUpstream {
                stream,
                outbound_initial_payload: None,
                prefetched_response: Vec::new(),
                target_verified: true,
            }),
        "socks4" => connect_socks4(proxy, request)
            .await
            .map(|stream| ConnectedUpstream {
                stream,
                outbound_initial_payload: None,
                prefetched_response: Vec::new(),
                target_verified: true,
            }),
        "http" | "https" => connect_http_proxy(proxy, request).await,
        other => Err(anyhow!("不支持的代理类型: {other}")),
    }
}

async fn connect_socks5(proxy: &ProxyRecord, request: &TargetRequest) -> Result<TcpStream> {
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

    stream
        .write_all(&build_socks5_connect_request(request)?)
        .await?;
    let mut header = [0u8; 4];
    stream.read_exact(&mut header).await?;
    if header[0] != 0x05 || header[1] != 0x00 {
        return Err(anyhow!("SOCKS5连接目标失败，响应码 {}", header[1]));
    }
    read_socks5_bind_address(&mut stream, header[3]).await?;
    Ok(stream)
}

async fn connect_socks4(proxy: &ProxyRecord, request: &TargetRequest) -> Result<TcpStream> {
    let mut stream = TcpStream::connect((proxy.host.as_str(), proxy.port as u16)).await?;
    // SOCKS4 只有 USERID 字段，没有密码认证；配置层会拒绝 SOCKS4 密码。
    let userid = proxy.username.as_deref().unwrap_or("");
    if userid.len() > 255 {
        return Err(anyhow!("SOCKS4 用户标识过长"));
    }
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
    stream.write_all(&packet).await?;
    let mut response = [0u8; 8];
    stream.read_exact(&mut response).await?;
    if response[1] != 0x5a {
        return Err(anyhow!("{}", socks4_reply_message(response[1])));
    }
    Ok(stream)
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
) -> Result<ConnectedUpstream> {
    let mut stream = TcpStream::connect((proxy.host.as_str(), proxy.port as u16)).await?;
    if request.inbound == InboundProtocol::HttpForward {
        let payload = build_http_forward_proxy_payload(proxy, request)?;
        return Ok(ConnectedUpstream {
            stream,
            outbound_initial_payload: Some(payload),
            prefetched_response: Vec::new(),
            target_verified: false,
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
    if first.split_whitespace().nth(1) != Some("200") {
        return Err(anyhow!("HTTP代理CONNECT失败: {first}"));
    }
    Ok(ConnectedUpstream {
        stream,
        outbound_initial_payload: None,
        prefetched_response,
        target_verified: true,
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
        if buffer.len() > 64 * 1024 {
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
    let latency = latency_score(proxy.response_time.unwrap_or(1000));
    let priority = ((1000 - proxy.priority).clamp(0, 1000) as f64) / 10.0;
    (success_rate * 70.0 + latency * 0.25 + priority * 0.05).clamp(0.01, 100.0)
}

fn latency_score(response_time: i64) -> f64 {
    if response_time <= 0 {
        return 50.0;
    }
    100.0 / (1.0 + response_time as f64 / 500.0)
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

fn stable_hash(value: &str) -> usize {
    let mut hash: i32 = 0;
    for byte in value.bytes() {
        hash = hash.wrapping_mul(31).wrapping_add(byte as i32);
    }
    hash.unsigned_abs() as usize
}

impl ProxyMetrics {
    fn new() -> Self {
        Self {
            requests: VecDeque::new(),
            score: 50.0,
            last_used: now_millis(),
            last_success: 0,
            pushed_status: None,
        }
    }

    fn push(&mut self, success: bool, response_time: Option<i64>) {
        let now = now_millis();
        self.requests.push_back(RequestMetric {
            timestamp: now,
            success,
            response_time,
        });
        while self
            .requests
            .front()
            .map(|metric| now - metric.timestamp > 5 * 60 * 1000)
            .unwrap_or(false)
        {
            self.requests.pop_front();
        }
        self.last_used = now;
        if success {
            self.last_success = now;
        }
        self.score = self.calculate_score();
    }

    fn summary(&self) -> (i64, i64, i64) {
        let success = self.requests.iter().filter(|item| item.success).count() as i64;
        let failed = self.requests.len() as i64 - success;
        let times = self
            .requests
            .iter()
            .filter_map(|item| item.response_time)
            .collect::<Vec<_>>();
        let avg = if times.is_empty() {
            0
        } else {
            times.iter().sum::<i64>() / times.len() as i64
        };
        (success, failed, avg)
    }

    fn calculate_score(&self) -> f64 {
        let (success, failed, avg_rt) = self.summary();
        let total = (success + failed).max(1) as f64;
        let success_rate = success as f64 / total;
        (success_rate * 75.0 + latency_score(avg_rt) * 0.25).clamp(0.01, 100.0)
    }
}

impl CircuitBreaker {
    fn new(config: CircuitConfig) -> Self {
        Self {
            state: "CLOSED".to_string(),
            failures: 0,
            threshold: config.failure_threshold,
            timeout_ms: config.timeout_ms,
            next_attempt: 0,
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
        if self.state == "OPEN" && now_millis() >= self.next_attempt {
            self.state = "HALF_OPEN".to_string();
            return true;
        }
        false
    }

    fn can_attempt_snapshot(&self) -> bool {
        if self.state == "CLOSED" {
            return true;
        }
        self.state == "OPEN" && now_millis() >= self.next_attempt
    }

    fn record_success(&mut self) {
        self.failures = 0;
        self.state = "CLOSED".to_string();
        self.next_attempt = 0;
    }

    fn record_failure(&mut self) {
        self.failures += 1;
        if self.state == "HALF_OPEN" || self.failures >= self.threshold {
            self.state = "OPEN".to_string();
            self.next_attempt = now_millis() + self.timeout_ms;
        }
    }

    fn cancel_half_open_attempt(&mut self) {
        if self.state == "HALF_OPEN" {
            self.state = "OPEN".to_string();
            self.next_attempt = now_millis().saturating_add(self.timeout_ms);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

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
