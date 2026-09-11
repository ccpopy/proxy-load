use super::*;
use crate::{
    database::default_advanced_config,
    models::{ProxyGroupInput, ProxyInput},
};

fn runtime() -> Arc<ProxyRuntime> {
    let db = Database::open_in_memory().unwrap();
    let (events, _) = broadcast::channel(16);
    let mut config = default_advanced_config();
    config.insert("circuit_failure_threshold".into(), json!(1));
    Arc::new(ProxyRuntime::new(db, events, "127.0.0.1", 0, &json!(config)).unwrap())
}

fn request(host: &str) -> TargetRequest {
    TargetRequest {
        host: host.into(),
        original_host: host.into(),
        port: 443,
        address_type: ADDR_DOMAIN,
        inbound: InboundProtocol::HttpConnect,
        initial_payload: Vec::new(),
    }
}

fn add_proxy(runtime: &ProxyRuntime, port: u16) -> ProxyRecord {
    runtime
        .db
        .create_proxy(ProxyInput {
            name: format!("test-{port}"),
            proxy_type: "http".into(),
            host: "127.0.0.1".into(),
            port: i64::from(port),
            username: None,
            password: None,
            enabled: Some(1),
            test_url: None,
            test_timeout: None,
            skip_cert_verify: None,
        })
        .unwrap()
}

fn change_proxy(
    runtime: &ProxyRuntime,
    proxy: &ProxyRecord,
    kind: &str,
    enabled: i64,
) -> ProxyRecord {
    runtime
        .db
        .update_proxy(
            proxy.id,
            ProxyInput {
                name: proxy.name.clone(),
                proxy_type: kind.into(),
                host: proxy.host.clone(),
                port: proxy.port,
                username: None,
                password: None,
                enabled: Some(enabled),
                test_url: None,
                test_timeout: None,
                skip_cert_verify: None,
            },
        )
        .unwrap()
        .0
}

async fn http_proxy(responses: Vec<&'static str>) -> (u16, tokio::task::JoinHandle<()>) {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();
    let task = tokio::spawn(async move {
        for response in responses {
            let (mut stream, _) = listener.accept().await.unwrap();
            read_http_request_header(&mut stream, Vec::new(), 2000)
                .await
                .unwrap();
            stream.write_all(response.as_bytes()).await.unwrap();
        }
    });
    (port, task)
}

#[tokio::test]
async fn target_failure_does_not_disable_proxy_for_other_hosts() {
    let runtime = runtime();
    let (port, server) = http_proxy(vec![
        "HTTP/1.1 502 Bad Gateway\r\n\r\n",
        "HTTP/1.1 200 Connection Established\r\n\r\n",
    ])
    .await;
    let proxy = add_proxy(&runtime, port);

    assert!(
        connect_with_fail_fast(runtime.clone(), &request("github.com"), Instant::now())
            .await
            .is_err()
    );
    assert_eq!(
        runtime
            .db
            .get_proxy(proxy.id)
            .unwrap()
            .unwrap()
            .status
            .as_deref(),
        Some("unknown"),
        "一个目标失败不应把整个代理标记为不可用"
    );
    assert!(runtime.metrics.read().await.get(&proxy.id).is_none());

    let (selected, _) = connect_with_fail_fast(
        runtime.clone(),
        &request("cms.zjzwfw.gov.cn"),
        Instant::now(),
    )
    .await
    .expect("同一个代理仍应能连接其他目标");
    assert_eq!(selected.id, proxy.id);
    runtime.decrement_active(proxy.id).await;
    server.await.unwrap();
}

#[tokio::test]
async fn adaptive_selection_accounts_for_live_connections() {
    let runtime = runtime();
    let first = add_proxy(&runtime, 10001);
    let second = add_proxy(&runtime, 10002);
    runtime.increment_active(first.id).await;
    let ordered = runtime
        .order_proxies(vec![first, second.clone()], "adaptive", "example.com")
        .await
        .unwrap();
    assert_eq!(ordered[0].id, second.id);
}

#[tokio::test]
async fn adaptive_selection_expires_old_failure_scores() {
    let runtime = runtime();
    let first = add_proxy(&runtime, 10001);
    let second = add_proxy(&runtime, 10002);
    let mut metric = ProxyMetrics::new();
    metric.push(false, None);
    metric.requests.front_mut().unwrap().timestamp = now_millis() - 6 * 60 * 1000;
    runtime.metrics.write().await.insert(first.id, metric);
    let ordered = runtime
        .order_proxies(vec![first.clone(), second], "adaptive", "example.com")
        .await
        .unwrap();
    assert_eq!(ordered[0].id, first.id, "过期的失败不应一直降低排序");
}

#[test]
fn new_fail_fast_defaults_are_five_and_fifteen_seconds() {
    let config = default_advanced_config();
    assert_eq!(config["failfast_attempt_timeout"], json!(5000));
    assert_eq!(config["failfast_total_timeout"], json!(15000));
    assert_eq!(config["failfast_max_attempts"], json!(3));
}

#[test]
fn saved_timeout_preferences_are_not_overwritten() {
    let runtime = runtime();
    runtime
        .db
        .save_settings(&Map::from_iter([
            ("failfast_attempt_timeout".into(), json!(10000)),
            ("failfast_total_timeout".into(), json!(30000)),
        ]))
        .unwrap();
    let config = runtime.db.load_advanced_config().unwrap();
    assert_eq!(config["failfast_attempt_timeout"], json!(10000));
    assert_eq!(config["failfast_total_timeout"], json!(30000));
}

#[tokio::test]
async fn concurrent_reservations_distribute_load_without_snapshot_herding() {
    let runtime = runtime();
    let first = add_proxy(&runtime, 10001);
    let second = add_proxy(&runtime, 10002);
    let mut tasks = tokio::task::JoinSet::new();
    for _ in 0..20 {
        let runtime = runtime.clone();
        tasks.spawn(async move {
            runtime
                .reserve_proxy(&request("example.com"), &HashSet::new())
                .await
                .unwrap()
                .unwrap()
                .id
        });
    }
    let mut selected = HashMap::<i64, usize>::new();
    while let Some(result) = tasks.join_next().await {
        *selected.entry(result.unwrap()).or_default() += 1;
    }
    assert_eq!(selected[&first.id], 10);
    assert_eq!(selected[&second.id], 10);
    assert_eq!(runtime.active_connections.read().await[&first.id], 10);
    assert_eq!(runtime.active_connections.read().await[&second.id], 10);
}

#[tokio::test]
async fn target_cooldown_is_scoped_and_not_cleared_by_an_unrelated_probe() {
    let runtime = runtime();
    let first = add_proxy(&runtime, 10001);
    let second = add_proxy(&runtime, 10002);
    let blocked = request("github.com");
    runtime
        .record_route_failure_locked(first.id, &blocked, FailureScope::Target)
        .await;
    assert!(!runtime.is_candidate_available(first.id, &blocked).await);
    assert!(runtime.is_candidate_available(second.id, &blocked).await);
    assert!(
        runtime
            .is_candidate_available(first.id, &request("cms.zjzwfw.gov.cn"))
            .await
    );
    let mut other_port = blocked.clone();
    other_port.port = 8443;
    assert!(runtime.is_candidate_available(first.id, &other_port).await);
    let mut new_mapping = blocked.clone();
    new_mapping.host = "192.0.2.1".into();
    assert!(runtime.is_candidate_available(first.id, &new_mapping).await);

    runtime
        .record_probe_result(&first, now_millis(), Some("active"), Some(30), true)
        .await
        .unwrap();
    assert!(!runtime.is_candidate_available(first.id, &blocked).await);
    let key = TargetRouteKey::new(first.id, &blocked);
    runtime
        .target_circuits
        .write()
        .await
        .get_mut(&key)
        .unwrap()
        .breaker
        .next_attempt = 0;
    assert!(runtime.try_begin_attempt(first.id, &blocked).await);
    assert!(
        !runtime.try_begin_attempt(first.id, &blocked).await,
        "每条冷却链路只允许一次半开试探"
    );
    runtime.cancel_half_open_attempt(first.id, &blocked).await;
    assert!(!runtime.is_candidate_available(first.id, &blocked).await);
    runtime.reset_proxy_state(first.id).await;
    assert!(runtime.is_candidate_available(first.id, &blocked).await);
}

#[tokio::test]
async fn auth_failure_opens_global_circuit_and_releases_connection_count() {
    let runtime = runtime();
    let (port, server) =
        http_proxy(vec!["HTTP/1.1 407 Proxy Authentication Required\r\n\r\n"]).await;
    let proxy = add_proxy(&runtime, port);
    let error = connect_with_fail_fast(runtime.clone(), &request("example.com"), Instant::now())
        .await
        .err()
        .unwrap()
        .to_string();
    assert!(error.contains("代理连接/认证"));
    assert!(
        !runtime
            .is_candidate_available(proxy.id, &request("other.example.com"))
            .await
    );
    assert!(runtime.active_connections.read().await.is_empty());
    assert!(runtime.target_circuits.read().await.is_empty());
    assert_eq!(
        runtime
            .db
            .get_proxy(proxy.id)
            .unwrap()
            .unwrap()
            .status
            .as_deref(),
        Some("inactive")
    );
    server.await.unwrap();
}

#[tokio::test]
async fn retries_stay_in_the_selected_group_and_forward_prefetched_bytes() {
    let runtime = runtime();
    let (failed_port, failed_server) = http_proxy(vec!["HTTP/1.1 502 Bad Gateway\r\n\r\n"]).await;
    let (good_port, good_server) = http_proxy(vec!["HTTP/1.1 200 OK\r\n\r\nready"]).await;
    let first = add_proxy(&runtime, failed_port);
    let second = add_proxy(&runtime, good_port);
    let outside = add_proxy(&runtime, 10003);
    runtime
        .db
        .create_proxy_group(ProxyGroupInput {
            name: Some("指定分组".into()),
            domains: Some(vec!["*.example.com".into()]),
            proxy_ids: Some(vec![first.id, second.id]),
            is_default: Some(0),
            enabled: Some(1),
        })
        .unwrap();
    let (selected, mut upstream) =
        connect_with_fail_fast(runtime.clone(), &request("api.example.com"), Instant::now())
            .await
            .unwrap();
    assert_eq!(selected.id, second.id);
    let mut bytes = upstream.prefetched_response;
    upstream.stream.read_to_end(&mut bytes).await.unwrap();
    assert_eq!(bytes, b"ready");
    let active = runtime.active_connections.read().await;
    assert!(!active.contains_key(&first.id));
    assert!(!active.contains_key(&outside.id));
    assert_eq!(active[&second.id], 1);
    drop(active);
    runtime.decrement_active(second.id).await;
    assert!(runtime.active_connections.read().await.is_empty());
    assert!(
        runtime.metrics.read().await[&second.id]
            .requests
            .back()
            .unwrap()
            .success
    );
    failed_server.await.unwrap();
    good_server.await.unwrap();
}

#[tokio::test]
async fn exhausted_group_never_falls_through_to_an_outside_proxy() {
    let runtime = runtime();
    let (port, server) = http_proxy(vec!["HTTP/1.1 502 Bad Gateway\r\n\r\n"]).await;
    let member = add_proxy(&runtime, port);
    let outside = add_proxy(&runtime, 10002);
    runtime
        .db
        .create_proxy_group(ProxyGroupInput {
            name: Some("默认分组".into()),
            domains: Some(Vec::new()),
            proxy_ids: Some(vec![member.id]),
            is_default: Some(1),
            enabled: Some(1),
        })
        .unwrap();
    assert!(
        connect_with_fail_fast(runtime.clone(), &request("example.com"), Instant::now())
            .await
            .is_err()
    );
    assert!(!runtime
        .circuit_breakers
        .read()
        .await
        .contains_key(&outside.id));
    assert!(runtime.active_connections.read().await.is_empty());
    server.await.unwrap();
}

#[tokio::test]
async fn attempt_limit_stops_retries_and_disabled_limit_allows_the_next_proxy() {
    let runtime = runtime();
    let (first_port, first_server) = http_proxy(vec!["HTTP/1.1 502 Bad Gateway\r\n\r\n"]).await;
    let (second_port, second_server) = http_proxy(vec!["HTTP/1.1 200 OK\r\n\r\n"]).await;
    let first = add_proxy(&runtime, first_port);
    let second = add_proxy(&runtime, second_port);
    runtime
        .runtime_settings
        .write()
        .await
        .fail_fast
        .max_attempts = 1;
    assert!(
        connect_with_fail_fast(runtime.clone(), &request("example.com"), Instant::now())
            .await
            .is_err()
    );
    assert!(!runtime
        .circuit_breakers
        .read()
        .await
        .contains_key(&second.id));
    assert!(runtime
        .target_circuits
        .read()
        .await
        .contains_key(&TargetRouteKey::new(first.id, &request("example.com"))));
    runtime.runtime_settings.write().await.fail_fast.enabled = false;
    let (selected, _) =
        connect_with_fail_fast(runtime.clone(), &request("example.com"), Instant::now())
            .await
            .unwrap();
    assert_eq!(selected.id, second.id);
    runtime.decrement_active(second.id).await;
    first_server.await.unwrap();
    second_server.await.unwrap();
}

#[tokio::test]
async fn connection_timeout_is_classified_by_the_stage_that_stalled() {
    for expected_scope in [FailureScope::Proxy, FailureScope::Target] {
        let runtime = runtime();
        runtime
            .runtime_settings
            .write()
            .await
            .fail_fast
            .attempt_timeout_ms = 250;
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let mut proxy = add_proxy(&runtime, listener.local_addr().unwrap().port());
        let (updated, _) = runtime
            .db
            .update_proxy(
                proxy.id,
                ProxyInput {
                    name: proxy.name.clone(),
                    proxy_type: "socks5".into(),
                    host: proxy.host.clone(),
                    port: proxy.port,
                    username: None,
                    password: None,
                    enabled: Some(1),
                    test_url: None,
                    test_timeout: None,
                    skip_cert_verify: None,
                },
            )
            .unwrap();
        proxy = updated;
        let server = tokio::spawn(async move {
            let (mut stream, _) = listener.accept().await.unwrap();
            let mut greeting = [0; 3];
            stream.read_exact(&mut greeting).await.unwrap();
            if expected_scope == FailureScope::Target {
                stream.write_all(&[5, 0]).await.unwrap();
            }
            std::future::pending::<()>().await;
        });
        let error =
            connect_with_fail_fast(runtime.clone(), &request("example.com"), Instant::now())
                .await
                .err()
                .unwrap()
                .to_string();
        assert!(error.contains(expected_scope.label()), "{error}");
        assert!(error.contains("超时"));
        assert!(runtime.active_connections.read().await.is_empty());
        assert_eq!(
            runtime
                .is_candidate_available(proxy.id, &request("other.example.com"))
                .await,
            expected_scope == FailureScope::Target
        );
        server.abort();
        let _ = server.await;
    }
}

#[tokio::test]
async fn successful_node_score_excludes_time_spent_before_its_own_attempt() {
    let runtime = runtime();
    let (port, server) = http_proxy(vec!["HTTP/1.1 200 OK\r\n\r\n"]).await;
    let proxy = add_proxy(&runtime, port);
    let start = Instant::now() - Duration::from_secs(5);
    connect_with_fail_fast(runtime.clone(), &request("example.com"), start)
        .await
        .unwrap();
    let metrics = runtime.metrics.read().await;
    assert!(
        metrics[&proxy.id]
            .requests
            .back()
            .unwrap()
            .response_time
            .unwrap()
            < 5000
    );
    drop(metrics);
    runtime.decrement_active(proxy.id).await;
    server.await.unwrap();
}

#[tokio::test]
async fn expired_total_budget_does_not_reserve_or_dial_a_proxy() {
    let runtime = runtime();
    add_proxy(&runtime, 10001);
    let error = connect_with_fail_fast(
        runtime.clone(),
        &request("example.com"),
        Instant::now() - Duration::from_secs(30),
    )
    .await
    .err()
    .unwrap()
    .to_string();
    assert!(error.contains("总超时"));
    assert!(runtime.active_connections.read().await.is_empty());
    assert!(runtime.circuit_breakers.read().await.is_empty());
}

#[tokio::test]
async fn target_circuit_cache_is_bounded_and_reclaims_old_destinations() {
    let runtime = runtime();
    let proxy = add_proxy(&runtime, 10001);
    let config = runtime.runtime_settings.read().await.circuit;
    {
        let mut targets = runtime.target_circuits.write().await;
        for index in 0..MAX_TARGET_CIRCUITS {
            targets.insert(
                TargetRouteKey::new(proxy.id, &request(&format!("{index}.example.com"))),
                TargetCircuit {
                    breaker: CircuitBreaker::new(config),
                    last_failure: now_millis(),
                },
            );
        }
    }
    let new_target = request("new.example.com");
    runtime
        .record_route_failure_locked(proxy.id, &new_target, FailureScope::Target)
        .await;
    assert_eq!(
        runtime.target_circuits.read().await.len(),
        MAX_TARGET_CIRCUITS
    );
    assert!(!runtime.is_candidate_available(proxy.id, &new_target).await);
    for target in runtime.target_circuits.write().await.values_mut() {
        target.last_failure = now_millis() - METRICS_WINDOW_MS - 1;
    }
    runtime
        .record_route_failure_locked(
            proxy.id,
            &request("fresh.example.com"),
            FailureScope::Target,
        )
        .await;
    assert_eq!(runtime.target_circuits.read().await.len(), 1);
}

#[tokio::test]
async fn disabled_group_member_is_skipped_until_reenabled_without_editing_group() {
    let runtime = runtime();
    let proxy = add_proxy(&runtime, 10001);
    change_proxy(&runtime, &proxy, "http", 0);
    runtime
        .db
        .create_proxy_group(ProxyGroupInput {
            name: Some("默认分组".into()),
            domains: Some(Vec::new()),
            proxy_ids: Some(vec![proxy.id]),
            is_default: Some(1),
            enabled: Some(1),
        })
        .unwrap();
    assert!(runtime
        .reserve_proxy(&request("example.com"), &HashSet::new())
        .await
        .is_err());
    assert!(runtime.active_connections.read().await.is_empty());
    change_proxy(&runtime, &proxy, "http", 1);
    let selected = runtime
        .reserve_proxy(&request("example.com"), &HashSet::new())
        .await
        .unwrap()
        .unwrap();
    assert_eq!(selected.id, proxy.id);
    runtime.decrement_active(proxy.id).await;
}

#[tokio::test]
async fn socks5_protocol_errors_are_distinct_from_target_rejections() {
    for (reply, expected_scope) in [
        (1, FailureScope::Proxy),
        (2, FailureScope::Target),
        (3, FailureScope::Target),
        (4, FailureScope::Target),
        (5, FailureScope::Target),
        (6, FailureScope::Target),
        (7, FailureScope::Proxy),
        (8, FailureScope::Target),
    ] {
        let runtime = runtime();
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let proxy = add_proxy(&runtime, listener.local_addr().unwrap().port());
        let proxy = change_proxy(&runtime, &proxy, "socks5", 1);
        let server = tokio::spawn(async move {
            let (mut stream, _) = listener.accept().await.unwrap();
            let mut greeting = [0; 3];
            stream.read_exact(&mut greeting).await.unwrap();
            stream.write_all(&[5, 0]).await.unwrap();
            let mut request_header = [0; 4];
            stream.read_exact(&mut request_header).await.unwrap();
            read_socks5_bind_address(&mut stream, request_header[3])
                .await
                .unwrap();
            stream
                .write_all(&[5, reply, 0, 1, 0, 0, 0, 0, 0, 0])
                .await
                .unwrap();
        });
        let mut scope = FailureScope::Proxy;
        assert!(
            connect_through_proxy(&proxy, &request("example.com"), &mut scope)
                .await
                .is_err()
        );
        assert_eq!(scope, expected_scope, "SOCKS5 response {reply}");
        server.await.unwrap();
    }
}

#[tokio::test]
async fn socks4_identification_failures_are_distinct_from_target_rejection() {
    for (reply, expected_scope) in [
        (0x5b, FailureScope::Target),
        (0x5c, FailureScope::Proxy),
        (0x5d, FailureScope::Proxy),
    ] {
        let runtime = runtime();
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let proxy = add_proxy(&runtime, listener.local_addr().unwrap().port());
        let proxy = change_proxy(&runtime, &proxy, "socks4", 1);
        let server = tokio::spawn(async move {
            let (mut stream, _) = listener.accept().await.unwrap();
            let mut packet = [0; 9];
            timeout(Duration::from_secs(2), stream.read_exact(&mut packet))
                .await
                .unwrap()
                .unwrap();
            assert_eq!(packet, [4, 1, 1, 187, 192, 0, 2, 1, 0]);
            stream
                .write_all(&[0, reply, 0, 0, 0, 0, 0, 0])
                .await
                .unwrap();
        });
        let mut scope = FailureScope::Proxy;
        assert!(timeout(
            Duration::from_secs(3),
            connect_through_proxy(&proxy, &request("192.0.2.1"), &mut scope)
        )
        .await
        .unwrap()
        .is_err());
        assert_eq!(scope, expected_scope, "SOCKS4 response {reply}");
        server.await.unwrap();
    }
}

#[test]
fn socks4a_request_preserves_domain_userid_and_terminators() {
    let packet = build_socks4_connect_request(&request("example.com"), "user").unwrap();
    assert_eq!(
        packet,
        b"\x04\x01\x01\xbb\x00\x00\x00\x01user\x00example.com\x00"
    );
    assert!(build_socks4_connect_request(&request("2001:db8::1"), "").is_err());
}

#[tokio::test]
async fn malformed_http_response_is_a_proxy_failure_but_connect_2xx_is_success() {
    let runtime = runtime();
    let (port, server) = http_proxy(vec!["Not HTTP\r\n\r\n", "HTTP/1.1 201 Created\r\n\r\n"]).await;
    let proxy = add_proxy(&runtime, port);
    let mut scope = FailureScope::Proxy;
    assert!(
        connect_through_proxy(&proxy, &request("example.com"), &mut scope)
            .await
            .is_err()
    );
    assert_eq!(scope, FailureScope::Proxy);
    let upstream = connect_through_proxy(&proxy, &request("example.com"), &mut scope)
        .await
        .unwrap();
    assert!(upstream.target_verified);
    server.await.unwrap();
}

#[tokio::test]
async fn target_half_open_success_restores_only_that_route() {
    let runtime = runtime();
    let (port, server) = http_proxy(vec!["HTTP/1.1 200 OK\r\n\r\n"]).await;
    let proxy = add_proxy(&runtime, port);
    let recovering = request("example.com");
    let still_blocked = request("blocked.example.com");
    runtime
        .record_route_failure_locked(proxy.id, &recovering, FailureScope::Target)
        .await;
    runtime
        .record_route_failure_locked(proxy.id, &still_blocked, FailureScope::Target)
        .await;
    runtime
        .target_circuits
        .write()
        .await
        .get_mut(&TargetRouteKey::new(proxy.id, &recovering))
        .unwrap()
        .breaker
        .next_attempt = 0;
    connect_with_fail_fast(runtime.clone(), &recovering, Instant::now())
        .await
        .unwrap();
    assert!(runtime.is_candidate_available(proxy.id, &recovering).await);
    assert!(
        !runtime
            .is_candidate_available(proxy.id, &still_blocked)
            .await
    );
    runtime.decrement_active(proxy.id).await;
    server.await.unwrap();
}

#[tokio::test]
async fn forwarded_post_body_is_not_replayed_when_the_upstream_disconnects() {
    let runtime = runtime();
    let upstream_listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let unused_listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let first = add_proxy(&runtime, upstream_listener.local_addr().unwrap().port());
    let second = add_proxy(&runtime, unused_listener.local_addr().unwrap().port());
    let server = tokio::spawn(async move {
        let (mut stream, _) = upstream_listener.accept().await.unwrap();
        let (header, mut body) = read_http_request_header(&mut stream, Vec::new(), 2000)
            .await
            .unwrap();
        while body.len() < 7 {
            let mut rest = vec![0; 7 - body.len()];
            stream.read_exact(&mut rest).await.unwrap();
            body.extend(rest);
        }
        assert!(header.starts_with("POST http://example.com/upload HTTP/1.1"));
        assert_eq!(body, b"payload");
        // 模拟收完业务数据后断开，此时重放 POST 会有重复提交风险。
    });
    let inbound = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let mut client = TcpStream::connect(inbound.local_addr().unwrap())
        .await
        .unwrap();
    let (accepted, address) = inbound.accept().await.unwrap();
    let service_runtime = runtime.clone();
    let service =
        tokio::spawn(async move { handle_client(service_runtime, accepted, address).await });
    client.write_all(b"POST http://example.com/upload HTTP/1.1\r\nHost: example.com\r\nContent-Length: 7\r\n\r\npayload").await.unwrap();
    client.shutdown().await.unwrap();
    let mut response = Vec::new();
    timeout(Duration::from_secs(2), client.read_to_end(&mut response))
        .await
        .unwrap()
        .unwrap();
    server.await.unwrap();
    service.await.unwrap().unwrap();
    assert!(runtime.active_connections.read().await.is_empty());
    assert!(!runtime
        .circuit_breakers
        .read()
        .await
        .contains_key(&second.id));
    assert!(
        runtime.metrics.read().await.get(&first.id).is_none(),
        "HTTP 转发未验证目标，不应记录为目标建连成功"
    );
    let (logs, total) = runtime.db.traffic_logs(1, 25, None).unwrap();
    assert_eq!(total, 1);
    assert_eq!(logs[0].result_type.as_deref(), Some("request_forwarded"));
}
