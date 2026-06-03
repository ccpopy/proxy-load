use std::{collections::HashMap, net::SocketAddr, sync::Arc};

use anyhow::{anyhow, Result};
use axum::{
    extract::{
        ws::{WebSocket, WebSocketUpgrade},
        Path, Query, State,
    },
    http::StatusCode,
    response::{IntoResponse, Response},
    routing::{get, post, put},
    Json, Router,
};
use futures_util::{SinkExt, StreamExt};
use serde::Deserialize;
use serde_json::{json, Map, Value};
use tower_http::{cors::CorsLayer, trace::TraceLayer};

use crate::{
    models::{DnsInput, ProxyGroupInput, ProxyInput},
    proxy_tester,
    state::AppState,
    version,
};

type SharedState = Arc<AppState>;
type ApiResult<T> = Result<Json<T>, ApiError>;

pub async fn serve(state: SharedState) -> Result<()> {
    let router = Router::new()
        .route("/ws", get(ws_handler))
        .route("/api/proxies", get(list_proxies).post(create_proxy))
        .route("/api/proxies/priorities", post(update_priorities))
        .route(
            "/api/proxies/{id}",
            get(get_proxy).put(update_proxy).delete(delete_proxy),
        )
        .route("/api/proxies/{id}/priority", put(update_priority))
        .route("/api/proxies/{id}/test", post(test_proxy))
        .route(
            "/api/proxy-groups",
            get(list_proxy_groups).post(create_proxy_group),
        )
        .route(
            "/api/proxy-groups/{id}",
            put(update_proxy_group).delete(delete_proxy_group),
        )
        .route("/api/settings", get(get_settings).post(save_settings))
        .route(
            "/api/advanced-config",
            get(get_advanced_config).post(save_advanced_config),
        )
        .route("/api/advanced-config/reset", post(reset_advanced_config))
        .route("/api/advanced-config/export", get(export_config))
        .route("/api/stats/overview", get(stats_overview))
        .route("/api/stats/hourly", get(stats_hourly))
        .route("/api/stats/proxy-usage", get(stats_proxy_usage))
        .route("/api/stats/targets", get(stats_targets))
        .route("/api/stats/failed-targets", get(stats_failed_targets))
        .route("/api/stats/circuit-breakers", get(stats_circuit_breakers))
        .route("/api/stats/connection-pools", get(stats_connection_pools))
        .route("/api/dns-mappings", get(list_dns).post(create_dns))
        .route("/api/dns-mappings/{id}", put(update_dns).delete(delete_dns))
        .route("/api/dns-mappings/{id}/toggle", put(toggle_dns))
        .route("/api/test-urls", get(test_urls))
        .route(
            "/api/traffic-logs",
            get(traffic_logs).delete(clear_traffic_logs),
        )
        .route("/api/version", get(version_info))
        .layer(CorsLayer::permissive())
        .layer(TraceLayer::new_for_http())
        .with_state(state.clone());

    let addr = SocketAddr::from(([127, 0, 0, 1], state.api_port));
    let listener = tokio::net::TcpListener::bind(addr).await?;
    println!("管理 API 运行在 http://{addr}");
    axum::serve(listener, router).await?;
    Ok(())
}

async fn ws_handler(State(state): State<SharedState>, ws: WebSocketUpgrade) -> Response {
    ws.on_upgrade(move |socket| websocket_loop(state, socket))
}

async fn websocket_loop(state: SharedState, socket: WebSocket) {
    let (mut sender, mut receiver) = socket.split();
    let _ = sender
        .send(axum::extract::ws::Message::Text(
            json!({ "type": "connected", "timestamp": crate::state::now_millis() })
                .to_string()
                .into(),
        ))
        .await;

    let mut rx = state.events.subscribe();
    loop {
        tokio::select! {
            event = rx.recv() => {
                match event {
                    Ok(event) => {
                        if sender
                            .send(axum::extract::ws::Message::Text(
                                serde_json::to_string(&event).unwrap_or_default().into()
                            ))
                            .await
                            .is_err()
                        {
                            break;
                        }
                    }
                    Err(tokio::sync::broadcast::error::RecvError::Lagged(_)) => continue,
                    Err(_) => break,
                }
            }
            incoming = receiver.next() => {
                if incoming.is_none() {
                    break;
                }
            }
        }
    }
}

async fn list_proxies(State(state): State<SharedState>) -> ApiResult<Value> {
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
    Ok(Json(serde_json::to_value(proxies)?))
}

async fn get_proxy(State(state): State<SharedState>, Path(id): Path<i64>) -> ApiResult<Value> {
    let proxy = state
        .db
        .get_proxy(id)?
        .ok_or_else(|| ApiError::not_found("代理不存在"))?;
    Ok(Json(serde_json::to_value(proxy)?))
}

async fn create_proxy(
    State(state): State<SharedState>,
    Json(input): Json<ProxyInput>,
) -> ApiResult<Value> {
    let proxy = state.db.create_proxy(input)?;
    state.emit("proxy_created", serde_json::to_value(&proxy)?);
    Ok(Json(serde_json::to_value(proxy)?))
}

async fn update_proxy(
    State(state): State<SharedState>,
    Path(id): Path<i64>,
    Json(input): Json<ProxyInput>,
) -> ApiResult<Value> {
    let proxy = state.db.update_proxy(id, input)?;
    state.emit("proxy_updated", serde_json::to_value(&proxy)?);
    Ok(Json(serde_json::to_value(proxy)?))
}

async fn delete_proxy(State(state): State<SharedState>, Path(id): Path<i64>) -> ApiResult<Value> {
    state.db.delete_proxy(id)?;
    state.emit("proxy_deleted", json!({ "id": id }));
    Ok(Json(json!({ "message": "代理已删除" })))
}

#[derive(Deserialize)]
struct PriorityInput {
    priority: i64,
}

async fn update_priority(
    State(state): State<SharedState>,
    Path(id): Path<i64>,
    Json(input): Json<PriorityInput>,
) -> ApiResult<Value> {
    state.db.update_proxy_priority(id, input.priority)?;
    Ok(Json(json!({ "message": "优先级已更新" })))
}

#[derive(Deserialize)]
struct PrioritiesInput {
    priorities: HashMap<String, i64>,
}

async fn update_priorities(
    State(state): State<SharedState>,
    Json(input): Json<PrioritiesInput>,
) -> ApiResult<Value> {
    for (id, priority) in input.priorities {
        let id = id.parse::<i64>().map_err(|_| anyhow!("代理ID无效: {id}"))?;
        state.db.update_proxy_priority(id, priority)?;
    }
    Ok(Json(json!({ "message": "优先级批量更新成功" })))
}

async fn test_proxy(State(state): State<SharedState>, Path(id): Path<i64>) -> ApiResult<Value> {
    let proxy = state
        .db
        .get_proxy(id)?
        .ok_or_else(|| ApiError::not_found("代理不存在"))?;
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
    Ok(Json(serde_json::to_value(result)?))
}

async fn list_proxy_groups(State(state): State<SharedState>) -> ApiResult<Value> {
    Ok(Json(serde_json::to_value(state.db.list_proxy_groups()?)?))
}

async fn create_proxy_group(
    State(state): State<SharedState>,
    Json(input): Json<ProxyGroupInput>,
) -> ApiResult<Value> {
    let group = state.db.create_proxy_group(input)?;
    state.emit("proxy_group_created", serde_json::to_value(&group)?);
    Ok(Json(serde_json::to_value(group)?))
}

async fn update_proxy_group(
    State(state): State<SharedState>,
    Path(id): Path<i64>,
    Json(input): Json<ProxyGroupInput>,
) -> ApiResult<Value> {
    let group = state.db.update_proxy_group(id, input)?;
    state.emit("proxy_group_updated", serde_json::to_value(&group)?);
    Ok(Json(serde_json::to_value(group)?))
}

async fn delete_proxy_group(
    State(state): State<SharedState>,
    Path(id): Path<i64>,
) -> ApiResult<Value> {
    state.db.delete_proxy_group(id)?;
    state.emit("proxy_group_deleted", json!({ "id": id }));
    Ok(Json(json!({ "message": "分组已删除" })))
}

async fn get_settings(State(state): State<SharedState>) -> ApiResult<Value> {
    Ok(Json(serde_json::to_value(state.db.settings_map()?)?))
}

async fn save_settings(
    State(state): State<SharedState>,
    Json(settings): Json<Map<String, Value>>,
) -> ApiResult<Value> {
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
    Ok(Json(json!({ "message": "设置已保存" })))
}

async fn get_advanced_config(State(state): State<SharedState>) -> ApiResult<Value> {
    Ok(Json(state.db.load_advanced_config()?))
}

async fn save_advanced_config(
    State(state): State<SharedState>,
    Json(config): Json<Map<String, Value>>,
) -> ApiResult<Value> {
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
    Ok(Json(json!({
        "success": true,
        "requiresRestart": current_port != next_port,
        "message": if current_port != next_port { "部分配置需要重启服务才能生效" } else { "配置已应用" }
    })))
}

async fn reset_advanced_config(State(state): State<SharedState>) -> ApiResult<Value> {
    state.db.reset_advanced_config()?;
    Ok(Json(
        json!({ "success": true, "message": "已恢复默认配置" }),
    ))
}

async fn export_config(State(state): State<SharedState>) -> ApiResult<Value> {
    Ok(Json(state.db.exported_config()?))
}

async fn stats_overview(State(state): State<SharedState>) -> ApiResult<Value> {
    Ok(Json(state.db.overview(state.uptime_seconds())?))
}

async fn stats_hourly(State(state): State<SharedState>) -> ApiResult<Value> {
    Ok(Json(state.db.scalar_json(
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
    )?))
}

async fn stats_proxy_usage(State(state): State<SharedState>) -> ApiResult<Value> {
    Ok(Json(state.db.scalar_json(
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
    )?))
}

async fn stats_targets(State(state): State<SharedState>) -> ApiResult<Value> {
    Ok(Json(state.db.scalar_json(
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
    )?))
}

async fn stats_failed_targets(State(state): State<SharedState>) -> ApiResult<Value> {
    Ok(Json(state.db.scalar_json(
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
    )?))
}

async fn stats_circuit_breakers(State(state): State<SharedState>) -> ApiResult<Value> {
    Ok(Json(Value::Array(
        state.proxy_runtime.circuit_breaker_stats().await,
    )))
}

async fn stats_connection_pools(State(state): State<SharedState>) -> ApiResult<Value> {
    Ok(Json(Value::Array(
        state.proxy_runtime.connection_pool_stats().await,
    )))
}

async fn list_dns(State(state): State<SharedState>) -> ApiResult<Value> {
    Ok(Json(serde_json::to_value(state.db.list_dns_mappings()?)?))
}

async fn create_dns(
    State(state): State<SharedState>,
    Json(input): Json<DnsInput>,
) -> ApiResult<Value> {
    let mapping = state.db.create_dns_mapping(input)?;
    state.proxy_runtime.refresh_dns_cache().await?;
    state.emit("dns_mapping_added", serde_json::to_value(&mapping)?);
    Ok(Json(serde_json::to_value(mapping)?))
}

async fn update_dns(
    State(state): State<SharedState>,
    Path(id): Path<i64>,
    Json(input): Json<DnsInput>,
) -> ApiResult<Value> {
    let mapping = state.db.update_dns_mapping(id, input)?;
    state.proxy_runtime.refresh_dns_cache().await?;
    state.emit("dns_mapping_updated", serde_json::to_value(&mapping)?);
    Ok(Json(json!({ "success": true })))
}

async fn delete_dns(State(state): State<SharedState>, Path(id): Path<i64>) -> ApiResult<Value> {
    state.db.delete_dns_mapping(id)?;
    state.proxy_runtime.refresh_dns_cache().await?;
    state.emit("dns_mapping_deleted", json!({ "id": id }));
    Ok(Json(json!({ "success": true })))
}

async fn toggle_dns(State(state): State<SharedState>, Path(id): Path<i64>) -> ApiResult<Value> {
    let enabled = state.db.toggle_dns_mapping(id)?;
    state.proxy_runtime.refresh_dns_cache().await?;
    state.emit(
        "dns_mapping_toggled",
        json!({ "id": id, "enabled": enabled }),
    );
    Ok(Json(json!({ "success": true, "enabled": enabled })))
}

async fn test_urls(State(state): State<SharedState>) -> ApiResult<Value> {
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
    Ok(Json(Value::Array(urls)))
}

async fn traffic_logs(
    State(state): State<SharedState>,
    Query(query): Query<HashMap<String, String>>,
) -> ApiResult<Value> {
    let page = parse_positive_query(query.get("page"), 1, "page")?;
    let page_size = parse_positive_query(query.get("page_size"), 50, "page_size")?;
    let (items, total) = state.db.traffic_logs(page, page_size)?;
    let total_pages = (total as f64 / page_size as f64).ceil().max(1.0) as i64;
    Ok(Json(json!({
        "items": items,
        "page": page,
        "pageSize": page_size,
        "total": total,
        "totalPages": total_pages
    })))
}

async fn clear_traffic_logs(State(state): State<SharedState>) -> ApiResult<Value> {
    let deleted = state.db.clear_traffic_logs()?;
    state.emit("traffic_logs_cleared", json!({ "deleted": deleted }));
    Ok(Json(json!({ "deleted": deleted })))
}

async fn version_info() -> ApiResult<Value> {
    Ok(Json(version::version_info()))
}

fn parse_positive_query(
    value: Option<&String>,
    default_value: i64,
    field: &str,
) -> Result<i64, ApiError> {
    match value {
        None => Ok(default_value),
        Some(value) if value.trim().is_empty() => Ok(default_value),
        Some(value) => {
            let number = value
                .parse::<i64>()
                .map_err(|_| ApiError::bad_request(format!("{field}必须是正整数")))?;
            if number < 1 {
                Err(ApiError::bad_request(format!("{field}必须是正整数")))
            } else {
                Ok(number)
            }
        }
    }
}

struct ApiError {
    status: StatusCode,
    message: String,
}

impl ApiError {
    fn bad_request(message: impl Into<String>) -> Self {
        Self {
            status: StatusCode::BAD_REQUEST,
            message: message.into(),
        }
    }

    fn not_found(message: impl Into<String>) -> Self {
        Self {
            status: StatusCode::NOT_FOUND,
            message: message.into(),
        }
    }
}

impl IntoResponse for ApiError {
    fn into_response(self) -> Response {
        (self.status, Json(json!({ "error": self.message }))).into_response()
    }
}

impl From<anyhow::Error> for ApiError {
    fn from(error: anyhow::Error) -> Self {
        Self {
            status: StatusCode::INTERNAL_SERVER_ERROR,
            message: error.to_string(),
        }
    }
}

impl From<serde_json::Error> for ApiError {
    fn from(error: serde_json::Error) -> Self {
        Self {
            status: StatusCode::INTERNAL_SERVER_ERROR,
            message: error.to_string(),
        }
    }
}

impl From<rusqlite::Error> for ApiError {
    fn from(error: rusqlite::Error) -> Self {
        Self {
            status: StatusCode::INTERNAL_SERVER_ERROR,
            message: error.to_string(),
        }
    }
}
