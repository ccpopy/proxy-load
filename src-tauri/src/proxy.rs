use std::{
    collections::{HashMap, VecDeque},
    net::{Ipv4Addr, SocketAddr},
    sync::Arc,
    time::Instant,
};

use anyhow::{anyhow, Context, Result};
use base64::{engine::general_purpose, Engine as _};
use serde::Serialize;
use serde_json::{json, Value};
use tokio::{
    io::{self, AsyncReadExt, AsyncWriteExt},
    net::{TcpListener, TcpStream},
    sync::{broadcast, Mutex, RwLock},
    time::{sleep, timeout, Duration},
};

use crate::{
    database::Database,
    models::{ProxyRecord, ServerEvent},
    state::now_millis,
};

const SOCKS_VERSION: u8 = 0x05;
const SOCKS_CMD_CONNECT: u8 = 0x01;
const ADDR_IPV4: u8 = 0x01;
const ADDR_DOMAIN: u8 = 0x03;

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
    half_open_attempts: i64,
    half_open_successes: i64,
    next_attempt: i64,
}

impl ProxyRuntime {
    pub fn new(db: Database, events: broadcast::Sender<ServerEvent>, listen_port: u16) -> Self {
        Self {
            db,
            events,
            service_status: Arc::new(RwLock::new(ProxyServiceStatus {
                state: "starting".to_string(),
                running: false,
                host: "0.0.0.0".to_string(),
                port: listen_port,
                error: Some("代理服务正在启动".to_string()),
            })),
            metrics: Arc::new(RwLock::new(HashMap::new())),
            circuit_breakers: Arc::new(RwLock::new(HashMap::new())),
            active_connections: Arc::new(RwLock::new(HashMap::new())),
            dns_cache: Arc::new(RwLock::new(HashMap::new())),
            round_robin_index: Arc::new(Mutex::new(0)),
        }
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

    pub async fn stats(&self) -> Vec<Value> {
        let metrics = self.metrics.read().await;
        let active = self.active_connections.read().await;
        metrics
            .iter()
            .map(|(proxy_id, metric)| {
                let (success, failed, avg_rt) = metric.summary();
                json!({
                    "proxyId": proxy_id,
                    "success": success,
                    "failed": failed,
                    "totalTime": 0,
                    "avgResponseTime": avg_rt,
                    "weight": (metric.score * 100.0).round() / 100.0,
                    "activeConnections": active.get(proxy_id).copied().unwrap_or(0)
                })
            })
            .collect()
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

    pub async fn connection_pool_stats(&self) -> Vec<Value> {
        let active = self.active_connections.read().await;
        active
            .iter()
            .map(|(proxy_id, active)| {
                json!({
                    "proxyId": proxy_id,
                    "created": 0,
                    "reused": 0,
                    "destroyed": 0,
                    "current": active,
                    "waiting": 0,
                    "active": active,
                    "idle": 0,
                    "total": active
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
        let entry = active.entry(proxy_id).or_insert(0);
        *entry = (*entry - 1).max(0);
    }

    async fn record_request(
        &self,
        proxy_id: Option<i64>,
        target_host: &str,
        target_port: u16,
        success: bool,
        response_time: Option<i64>,
        error: Option<&str>,
        result_type: &str,
    ) {
        if let Some(proxy_id) = proxy_id {
            let mut metrics = self.metrics.write().await;
            let metric = metrics.entry(proxy_id).or_insert_with(ProxyMetrics::new);
            metric.push(success, response_time);
        }

        if let Err(db_error) = self.db.log_request(
            proxy_id,
            target_host,
            i64::from(target_port),
            success,
            response_time,
            error,
            result_type,
        ) {
            eprintln!("写入请求日志失败: {db_error:#}");
        }

        let event = ServerEvent {
            event_type: "request_logged".to_string(),
            data: json!({
                "proxy_id": proxy_id,
                "target_host": target_host,
                "target_port": target_port,
                "success": if success { 1 } else { 0 },
                "response_time": response_time,
                "error_message": error,
                "result_type": result_type,
                "created_at": chrono::Utc::now().to_rfc3339()
            }),
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
            eprintln!(
                "代理路由命中: target={group_key}, group={}, pattern={}, default={}, candidates={:?}",
                selection.group_name,
                selection.domain_pattern.as_deref().unwrap_or("<default>"),
                selection.is_default,
                selection.proxy_ids
            );
            proxies.retain(|proxy| selection.proxy_ids.contains(&proxy.id));
            if proxies.is_empty() {
                return Err(anyhow!("目标 {} 匹配的代理分组没有可用代理", group_key));
            }
        } else {
            eprintln!("代理路由未命中分组: target={group_key}, 使用全部已启用代理");
        }

        let settings = self.db.settings_map()?;
        let algorithm = settings
            .get("algorithm")
            .map(String::as_str)
            .unwrap_or("adaptive");

        let mut eligible = Vec::new();
        for proxy in proxies {
            if !self.can_attempt(proxy.id).await {
                continue;
            }
            let status = proxy.status.as_deref().unwrap_or("unknown");
            if status == "active" || status == "unknown" || status == "testing" {
                eligible.push(proxy);
            }
        }

        if eligible.is_empty() {
            eligible = self.db.list_enabled_proxies()?;
        }

        self.order_proxies(eligible, algorithm, &group_key).await
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
            "weighted_round_robin" => {
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
                proxies.sort_by(|a, b| score_of(b).total_cmp(&score_of(a)));
            }
        }
        Ok(proxies)
    }

    async fn can_attempt(&self, proxy_id: i64) -> bool {
        let mut breakers = self.circuit_breakers.write().await;
        let breaker = breakers
            .entry(proxy_id)
            .or_insert_with(CircuitBreaker::default);
        breaker.can_attempt()
    }

    async fn record_breaker_success(&self, proxy_id: i64) {
        let mut breakers = self.circuit_breakers.write().await;
        breakers
            .entry(proxy_id)
            .or_insert_with(CircuitBreaker::default)
            .record_success();
    }

    async fn record_breaker_failure(&self, proxy_id: i64) {
        let mut breakers = self.circuit_breakers.write().await;
        breakers
            .entry(proxy_id)
            .or_insert_with(CircuitBreaker::default)
            .record_failure();
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

pub async fn serve(runtime: Arc<ProxyRuntime>, port: u16) -> Result<()> {
    if let Err(error) = runtime.refresh_dns_cache().await {
        runtime
            .set_service_status(ProxyServiceStatus {
                state: "failed".to_string(),
                running: false,
                host: "0.0.0.0".to_string(),
                port,
                error: Some(format!("DNS 缓存初始化失败: {error:#}")),
            })
            .await;
        return Err(error);
    }

    let listener = match TcpListener::bind(("0.0.0.0", port)).await {
        Ok(listener) => listener,
        Err(error) => {
            let message = format!("代理服务无法监听 0.0.0.0:{port}: {error}");
            runtime
                .set_service_status(ProxyServiceStatus {
                    state: "failed".to_string(),
                    running: false,
                    host: "0.0.0.0".to_string(),
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
            host: "0.0.0.0".to_string(),
            port,
            error: None,
        })
        .await;
    println!("混合代理负载均衡服务器运行在 0.0.0.0:{port}（SOCKS5/HTTP）");

    loop {
        let (client, addr) = match listener.accept().await {
            Ok(accepted) => accepted,
            Err(error) => {
                runtime
                    .set_service_status(ProxyServiceStatus {
                        state: "failed".to_string(),
                        running: false,
                        host: "0.0.0.0".to_string(),
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
    let initial = read_some_with_timeout(&mut client, 1000).await?;
    let request = if initial.first().copied() == Some(SOCKS_VERSION) {
        handle_socks5_handshake(&mut client, initial).await?
    } else if looks_like_http_proxy_request(&initial) {
        handle_http_proxy_header(&mut client, initial).await?
    } else {
        return Err(anyhow!("不支持的入站代理协议"));
    };

    let original_host = request.original_host.clone();
    let original_port = request.port;
    let request = runtime.resolve_target(request).await;
    match connect_with_fail_fast(runtime.clone(), &request, start).await {
        Ok((proxy_id, mut upstream)) => {
            complete_client_handshake(&mut client, &mut upstream, &request).await?;
            let response_time = start.elapsed().as_millis() as i64;
            runtime
                .record_request(
                    Some(proxy_id),
                    &original_host,
                    original_port,
                    true,
                    Some(response_time),
                    None,
                    "direct_success",
                )
                .await;
            let copy_result = io::copy_bidirectional(&mut client, &mut upstream).await;
            runtime.decrement_active(proxy_id).await;
            if let Err(error) = copy_result {
                runtime
                    .record_request(
                        Some(proxy_id),
                        &original_host,
                        original_port,
                        false,
                        Some(start.elapsed().as_millis() as i64),
                        Some(&error.to_string()),
                        "io_error",
                    )
                    .await;
            }
            Ok(())
        }
        Err(error) => {
            send_inbound_error(&mut client, &request, &error.to_string()).await?;
            runtime
                .record_request(
                    None,
                    &original_host,
                    original_port,
                    false,
                    Some(start.elapsed().as_millis() as i64),
                    Some(&error.to_string()),
                    "proxy_exhausted",
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
) -> Result<(i64, TcpStream)> {
    let config = runtime.db.load_advanced_config()?;
    let fail_fast = config
        .get("failfast_enabled")
        .and_then(Value::as_bool)
        .unwrap_or(true);
    let max_attempts = config
        .get("failfast_max_attempts")
        .and_then(Value::as_i64)
        .unwrap_or(3)
        .max(1) as usize;
    let attempt_timeout = config
        .get("failfast_attempt_timeout")
        .and_then(Value::as_i64)
        .unwrap_or(10000)
        .max(1) as u64;
    let total_timeout = config
        .get("failfast_total_timeout")
        .and_then(Value::as_i64)
        .unwrap_or(30000)
        .max(1) as u128;

    let mut proxies = runtime.select_proxies(request).await?;
    if fail_fast {
        proxies.truncate(max_attempts.min(proxies.len()));
    } else {
        proxies.truncate(1);
    }

    let mut errors = Vec::new();
    for proxy in proxies {
        if start.elapsed().as_millis() > total_timeout {
            return Err(anyhow!("总超时: {}", errors.join("; ")));
        }
        runtime.increment_active(proxy.id).await;
        let attempt = timeout(
            Duration::from_millis(attempt_timeout),
            connect_through_proxy(&proxy, request),
        )
        .await;

        match attempt {
            Ok(Ok(stream)) => {
                runtime.record_breaker_success(proxy.id).await;
                eprintln!(
                    "代理路由成功: target={}:{}, proxy_id={}, proxy_name={}, proxy_type={}",
                    request.original_host, request.port, proxy.id, proxy.name, proxy.proxy_type
                );
                return Ok((proxy.id, stream));
            }
            Ok(Err(error)) => {
                runtime.record_breaker_failure(proxy.id).await;
                runtime.decrement_active(proxy.id).await;
                eprintln!(
                    "代理路由尝试失败: target={}:{}, proxy_id={}, proxy_name={}, error={error}",
                    request.original_host, request.port, proxy.id, proxy.name
                );
                errors.push(format!("{}: {error}", proxy.name));
            }
            Err(_) => {
                runtime.record_breaker_failure(proxy.id).await;
                runtime.decrement_active(proxy.id).await;
                eprintln!(
                    "代理路由尝试超时: target={}:{}, proxy_id={}, proxy_name={}",
                    request.original_host, request.port, proxy.id, proxy.name
                );
                errors.push(format!("{}: 连接超时", proxy.name));
            }
        }
        sleep(Duration::from_millis(300)).await;
    }

    Err(anyhow!("所有代理都失败: {}", errors.join("; ")))
}

async fn connect_through_proxy(proxy: &ProxyRecord, request: &TargetRequest) -> Result<TcpStream> {
    match proxy.proxy_type.as_str() {
        "socks5" => connect_socks5(proxy, request).await,
        "socks4" => connect_socks4(proxy, request).await,
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
        if auth_response[1] != 0x00 {
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
    let mut packet = vec![
        0x04,
        SOCKS_CMD_CONNECT,
        (request.port >> 8) as u8,
        (request.port & 0xff) as u8,
    ];
    if let Ok(target_ip) = request.host.parse::<Ipv4Addr>() {
        packet.extend_from_slice(&target_ip.octets());
        packet.push(0x00);
    } else {
        if request.host.contains(':') {
            return Err(anyhow!("SOCKS4a暂不支持IPv6地址"));
        }
        if request.host.len() > 255 {
            return Err(anyhow!("SOCKS4a目标域名过长"));
        }
        packet.extend_from_slice(&[0x00, 0x00, 0x00, 0x01]);
        packet.push(0x00);
        packet.extend_from_slice(request.host.as_bytes());
        packet.push(0x00);
    }
    stream.write_all(&packet).await?;
    let mut response = [0u8; 8];
    stream.read_exact(&mut response).await?;
    if response[1] != 0x5a {
        return Err(anyhow!("SOCKS4连接目标失败，响应码 {}", response[1]));
    }
    Ok(stream)
}

async fn connect_http_proxy(proxy: &ProxyRecord, request: &TargetRequest) -> Result<TcpStream> {
    let mut stream = TcpStream::connect((proxy.host.as_str(), proxy.port as u16)).await?;
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
    let header = read_http_header(&mut stream, Vec::new(), 5000).await?;
    let first = header.lines().next().unwrap_or_default();
    if !first.contains(" 200 ") {
        return Err(anyhow!("HTTP代理CONNECT失败: {first}"));
    }
    Ok(stream)
}

async fn handle_socks5_handshake(
    client: &mut TcpStream,
    mut initial: Vec<u8>,
) -> Result<TargetRequest> {
    ensure_len(client, &mut initial, 2, 1000).await?;
    let method_count = initial[1] as usize;
    ensure_len(client, &mut initial, 2 + method_count, 1000).await?;
    client.write_all(&[0x05, 0x00]).await?;

    let mut header = [0u8; 4];
    client.read_exact(&mut header).await?;
    if header[0] != 0x05 || header[1] != SOCKS_CMD_CONNECT {
        return Err(anyhow!("仅支持 SOCKS5 CONNECT 命令"));
    }
    let (host, port, address_type) = read_socks_address(client, header[3]).await?;
    Ok(TargetRequest {
        original_host: host.clone(),
        host,
        port,
        address_type,
        inbound: InboundProtocol::Socks5,
        initial_payload: Vec::new(),
    })
}

async fn handle_http_proxy_header(
    client: &mut TcpStream,
    initial: Vec<u8>,
) -> Result<TargetRequest> {
    let header = read_http_header(client, initial, 5000).await?;
    parse_http_proxy_request(&header)
}

fn parse_http_proxy_request(header: &str) -> Result<TargetRequest> {
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
            initial_payload: Vec::new(),
        });
    }

    let mut host_header = None;
    for line in lines {
        if let Some((name, value)) = line.split_once(':') {
            if name.eq_ignore_ascii_case("host") {
                host_header = Some(value.trim().to_string());
            }
        }
    }
    let (host, port, path) = if target.starts_with("http://") {
        let url = url::Url::parse(target)?;
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
    } else {
        let host_header = host_header.ok_or_else(|| anyhow!("普通HTTP代理请求缺少Host头"))?;
        let (host, port) = parse_authority(&host_header, 80)?;
        (host, port, target.to_string())
    };

    let rewritten = rewrite_http_forward_header(header, &method, &path);
    Ok(TargetRequest {
        original_host: host.clone(),
        address_type: address_type(&host)?,
        host,
        port,
        inbound: InboundProtocol::HttpForward,
        initial_payload: rewritten.into_bytes(),
    })
}

fn rewrite_http_forward_header(header: &str, method: &str, path: &str) -> String {
    let mut lines = header.trim_end_matches("\r\n\r\n").split("\r\n");
    let version = lines
        .next()
        .and_then(|line| line.split_whitespace().nth(2))
        .unwrap_or("HTTP/1.1");
    let mut rewritten = vec![format!("{method} {path} {version}")];
    for line in lines {
        if line.starts_with("Proxy-Connection:") || line.starts_with("Proxy-Authorization:") {
            continue;
        }
        rewritten.push(line.to_string());
    }
    format!("{}\r\n\r\n", rewritten.join("\r\n"))
}

async fn complete_client_handshake(
    client: &mut TcpStream,
    upstream: &mut TcpStream,
    request: &TargetRequest,
) -> Result<()> {
    match request.inbound {
        InboundProtocol::Socks5 => {
            client
                .write_all(&[
                    0x05, 0x00, 0x00, ADDR_IPV4, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
                ])
                .await?;
        }
        InboundProtocol::HttpConnect => {
            client
                .write_all(b"HTTP/1.1 200 Connection Established\r\n\r\n")
                .await?;
        }
        InboundProtocol::HttpForward => {
            upstream.write_all(&request.initial_payload).await?;
        }
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

async fn ensure_len(
    stream: &mut TcpStream,
    buffer: &mut Vec<u8>,
    length: usize,
    timeout_ms: u64,
) -> Result<()> {
    while buffer.len() < length {
        let mut temp = vec![0u8; length - buffer.len()];
        let size = timeout(Duration::from_millis(timeout_ms), stream.read(&mut temp)).await??;
        if size == 0 {
            return Err(anyhow!("连接在读取协议数据时关闭"));
        }
        buffer.extend_from_slice(&temp[..size]);
    }
    Ok(())
}

async fn read_socks_address(stream: &mut TcpStream, address_type: u8) -> Result<(String, u16, u8)> {
    match address_type {
        ADDR_IPV4 => {
            let mut rest = [0u8; 6];
            stream.read_exact(&mut rest).await?;
            let host = format!("{}.{}.{}.{}", rest[0], rest[1], rest[2], rest[3]);
            let port = u16::from_be_bytes([rest[4], rest[5]]);
            Ok((host, port, ADDR_IPV4))
        }
        ADDR_DOMAIN => {
            let mut len = [0u8; 1];
            stream.read_exact(&mut len).await?;
            let mut domain = vec![0u8; len[0] as usize];
            stream.read_exact(&mut domain).await?;
            let mut port_bytes = [0u8; 2];
            stream.read_exact(&mut port_bytes).await?;
            let host = String::from_utf8(domain).context("SOCKS5域名不是有效UTF-8")?;
            let port = u16::from_be_bytes(port_bytes);
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

async fn read_http_header(
    stream: &mut TcpStream,
    initial: Vec<u8>,
    timeout_ms: u64,
) -> Result<String> {
    let mut buffer = initial;
    loop {
        if buffer.windows(4).any(|window| window == b"\r\n\r\n") {
            return String::from_utf8(buffer).context("HTTP请求头不是有效UTF-8");
        }
        let mut temp = [0u8; 1024];
        let size = timeout(Duration::from_millis(timeout_ms), stream.read(&mut temp)).await??;
        if size == 0 {
            return Err(anyhow!("HTTP请求头未完整读取"));
        }
        buffer.extend_from_slice(&temp[..size]);
        if buffer.len() > 64 * 1024 {
            return Err(anyhow!("HTTP请求头过大"));
        }
    }
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
        if host.trim().is_empty() {
            return Err(anyhow!("缺少目标主机"));
        }
        Ok((host.trim().to_string(), parsed_port))
    } else {
        Ok((authority.to_string(), default_port))
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

fn score_of(proxy: &ProxyRecord) -> f64 {
    let success = proxy.success_count.max(0) as f64;
    let failed = proxy.fail_count.max(0) as f64;
    let success_rate = if success + failed > 0.0 {
        success / (success + failed)
    } else {
        0.5
    };
    let rt = proxy.response_time.unwrap_or(1000).max(1) as f64;
    let rt_score = (1000.0 / rt).clamp(0.1, 5.0);
    (success_rate * 80.0 + rt_score * 20.0 + (1000 - proxy.priority).max(0) as f64 / 100.0)
        .clamp(0.01, 100.0)
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
        let rt_score = if avg_rt <= 0 {
            50.0
        } else {
            (1000.0 / avg_rt as f64 * 20.0).clamp(1.0, 100.0)
        };
        (success_rate * 75.0 + rt_score * 0.25).clamp(0.01, 100.0)
    }
}

impl Default for CircuitBreaker {
    fn default() -> Self {
        Self {
            state: "CLOSED".to_string(),
            failures: 0,
            threshold: 5,
            timeout_ms: 60000,
            half_open_attempts: 2,
            half_open_successes: 0,
            next_attempt: 0,
        }
    }
}

impl CircuitBreaker {
    fn can_attempt(&mut self) -> bool {
        if self.state == "CLOSED" {
            return true;
        }
        if self.state == "OPEN" && now_millis() >= self.next_attempt {
            self.state = "HALF_OPEN".to_string();
            self.half_open_successes = 0;
            return true;
        }
        self.state == "HALF_OPEN"
    }

    fn can_attempt_snapshot(&self) -> bool {
        if self.state == "CLOSED" || self.state == "HALF_OPEN" {
            return true;
        }
        self.state == "OPEN" && now_millis() >= self.next_attempt
    }

    fn record_success(&mut self) {
        self.failures = 0;
        if self.state == "HALF_OPEN" {
            self.half_open_successes += 1;
            if self.half_open_successes >= self.half_open_attempts {
                self.state = "CLOSED".to_string();
            }
        } else {
            self.state = "CLOSED".to_string();
        }
    }

    fn record_failure(&mut self) {
        self.failures += 1;
        if self.state == "HALF_OPEN" || self.failures >= self.threshold {
            self.state = "OPEN".to_string();
            self.next_attempt = now_millis() + self.timeout_ms;
        }
    }
}
