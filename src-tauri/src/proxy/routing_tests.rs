use super::*;

#[tokio::test]
async fn global_dial_capacity_wait_releases_on_cancel_without_holding_selection() {
    let runtime = runtime();
    add_proxy(&runtime, 19001);
    let capacity = runtime
        .global_dial_slots
        .clone()
        .acquire_many_owned(64)
        .await
        .unwrap();
    let pending = tokio::spawn({
        let runtime = runtime.clone();
        async move {
            runtime
                .reserve_proxy(&request("example.com"), &HashSet::new())
                .await
        }
    });
    tokio::task::yield_now().await;
    assert!(!pending.is_finished());
    assert!(runtime.selection_lock.try_lock().is_ok());
    assert!(runtime.active_connections.lock().unwrap().is_empty());
    pending.abort();
    assert!(pending.await.err().unwrap().is_cancelled());
    drop(capacity);
    assert_eq!(runtime.global_dial_slots.available_permits(), 64);
    let lease = runtime
        .reserve_proxy(&request("example.com"), &HashSet::new())
        .await
        .unwrap()
        .unwrap();
    drop(lease);
    assert_eq!(runtime.global_dial_slots.available_permits(), 64);
    assert!(runtime.active_connections.lock().unwrap().is_empty());
}
use crate::{
    database::default_advanced_config,
    models::{ProxyGroupInput, ProxyInput},
};

#[tokio::test]
async fn releasing_capacity_wakes_the_matching_pool_not_only_an_unrelated_waiter() {
    let runtime = runtime();
    let a = add_proxy(&runtime, 19001);
    let b = add_proxy(&runtime, 19002);
    for (host, proxy) in [("a.test", a.id), ("b.test", b.id)] {
        runtime
            .db
            .create_proxy_group(ProxyGroupInput {
                name: Some(host.into()),
                domains: Some(vec![host.into()]),
                proxy_ids: Some(vec![proxy]),
                ..Default::default()
            })
            .unwrap();
    }
    let mut leases = Vec::new();
    for _ in 0..32 {
        for host in ["a.test", "b.test"] {
            leases.push(
                runtime
                    .reserve_proxy(&request(host), &HashSet::new())
                    .await
                    .unwrap()
                    .unwrap(),
            );
        }
    }
    assert_eq!(runtime.global_dial_slots.available_permits(), 0);
    let waiter = |host: &'static str| {
        tokio::spawn({
            let runtime = runtime.clone();
            async move {
                runtime
                    .reserve_proxy(&request(host), &HashSet::new())
                    .await
                    .unwrap()
                    .unwrap()
            }
        })
    };
    let a_waiter = waiter("a.test");
    tokio::task::yield_now().await;
    let b_waiter = waiter("b.test");
    tokio::task::yield_now().await;
    drop(leases.pop()); // B released; A was queued first.
    let selected = timeout(Duration::from_secs(1), b_waiter)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(selected.id, b.id);
    assert!(!a_waiter.is_finished());
    a_waiter.abort();
    let _ = a_waiter.await;
    drop(selected);
    drop(leases);
    assert_eq!(runtime.global_dial_slots.available_permits(), 64);
    assert!(runtime.active_connections.lock().unwrap().is_empty());
}

#[tokio::test]
async fn successful_failover_creates_binding_without_renewing_it_on_recovery() {
    let runtime = runtime();
    let (primary_port, primary_server) = http_proxy(vec![
        "HTTP/1.1 407 Bad Auth\r\n\r\n",
        "HTTP/1.1 200 OK\r\n\r\n",
    ])
    .await;
    let (backup_port, backup_server) =
        http_proxy(vec!["HTTP/1.1 200 OK\r\n\r\n", "HTTP/1.1 200 OK\r\n\r\n"]).await;
    let primary = add_proxy(&runtime, primary_port);
    let backup = add_proxy(&runtime, backup_port);
    let group = runtime
        .db
        .create_proxy_group(ProxyGroupInput {
            name: Some("sticky".into()),
            domains: Some(vec!["*.test".into()]),
            proxy_ids: Some(vec![primary.id, backup.id]),
            algorithm_override: Some(Some("sticky_host".into())),
            sticky_failover_seconds: Some(1),
            ..Default::default()
        })
        .unwrap();
    let host = (0..100)
        .map(|i| format!("host{i}.test"))
        .find(|h| {
            crate::routing::affinity(group.id, h, primary.id)
                > crate::routing::affinity(group.id, h, backup.id)
        })
        .unwrap();
    let req = request(&host);
    let (lease, stream) = connect_with_fail_fast(runtime.clone(), &req, Instant::now())
        .await
        .unwrap();
    assert_eq!(lease.id, backup.id);
    drop((lease, stream));
    runtime
        .record_probe_result(
            &primary,
            runtime
                .db
                .routing_snapshot()
                .node_generations
                .get(&primary.id)
                .copied(),
            monotonic_millis(),
            Some("active"),
            Some(1),
            true,
        )
        .await
        .unwrap();
    let (lease, stream) = connect_with_fail_fast(runtime.clone(), &req, Instant::now())
        .await
        .unwrap();
    assert_eq!(lease.id, backup.id);
    drop((lease, stream));
    tokio::time::sleep(Duration::from_millis(1050)).await;
    let (lease, stream) = connect_with_fail_fast(runtime.clone(), &req, Instant::now())
        .await
        .unwrap();
    assert_eq!(lease.id, primary.id);
    drop((lease, stream));
    primary_server.await.unwrap();
    backup_server.await.unwrap();
    assert!(runtime.active_connections.lock().unwrap().is_empty());
}

#[tokio::test]
async fn group_algorithm_override_and_null_inheritance_are_independent() {
    let runtime = runtime();
    runtime
        .runtime_settings
        .write()
        .unwrap()
        .set_algorithm("sticky_host")
        .unwrap();
    let a = add_proxy(&runtime, 19001);
    let b = add_proxy(&runtime, 19002);
    let group = runtime
        .db
        .create_proxy_group(ProxyGroupInput {
            name: Some("round robin".into()),
            domains: Some(vec!["rr.test".into()]),
            proxy_ids: Some(vec![a.id, b.id]),
            algorithm_override: Some(Some("round_robin".into())),
            ..Default::default()
        })
        .unwrap();
    let mut counts = HashMap::new();
    let sticky = runtime
        .select_proxies(&request("other.test"), &HashSet::new())
        .unwrap()[0]
        .id;
    for _ in 0..100 {
        let lease = runtime
            .reserve_proxy(&request("rr.test"), &HashSet::new())
            .await
            .unwrap()
            .unwrap();
        *counts.entry(lease.id).or_insert(0) += 1;
        drop(lease);
        assert_eq!(
            runtime
                .select_proxies(&request("other.test"), &HashSet::new())
                .unwrap()[0]
                .id,
            sticky
        );
    }
    assert_eq!(counts, HashMap::from([(a.id, 50), (b.id, 50)]));
    runtime
        .db
        .update_proxy_group(
            group.id,
            serde_json::from_value(json!({"algorithm_override":null})).unwrap(),
        )
        .unwrap();
    let first = runtime
        .select_proxies(&request("rr.test"), &HashSet::new())
        .unwrap()[0]
        .id;
    for _ in 0..10 {
        assert_eq!(
            runtime
                .select_proxies(&request("rr.test"), &HashSet::new())
                .unwrap()[0]
                .id,
            first
        );
    }
}

#[tokio::test]
async fn sticky_failover_holds_backup_then_expires_and_invalidates_configuration() {
    let runtime = runtime();
    let a = add_proxy(&runtime, 19001);
    let b = add_proxy(&runtime, 19002);
    let group = runtime
        .db
        .create_proxy_group(ProxyGroupInput {
            name: Some("sticky".into()),
            domains: Some(vec!["*.test".into()]),
            proxy_ids: Some(vec![a.id, b.id]),
            algorithm_override: Some(Some("sticky_host".into())),
            sticky_failover_seconds: Some(1),
            ..Default::default()
        })
        .unwrap();
    let req = request("login.test");
    let primary = runtime.select_proxies(&req, &HashSet::new()).unwrap()[0].id;
    let backup = if primary == a.id { b.id } else { a.id };
    let bind = || {
        let snapshot = runtime.db.routing_snapshot();
        let policy = sticky_routes::Policy::for_request(&snapshot, &req, "adaptive").unwrap();
        runtime.sticky_routes.lock().unwrap().remember(
            &policy,
            &snapshot,
            backup,
            snapshot.node_generations[&backup],
        );
    };
    bind();
    assert_eq!(
        runtime.select_proxies(&req, &HashSet::new()).unwrap()[0].id,
        backup
    );
    runtime.db.update_proxy_priority(primary, 1).unwrap();
    assert_eq!(
        runtime.select_proxies(&req, &HashSet::new()).unwrap()[0].id,
        backup
    );
    tokio::time::sleep(Duration::from_millis(1050)).await;
    assert_eq!(
        runtime.select_proxies(&req, &HashSet::new()).unwrap()[0].id,
        primary
    );
    bind();
    runtime
        .db
        .update_proxy_group(
            group.id,
            ProxyGroupInput {
                domains: Some(vec!["login.test".into()]),
                ..Default::default()
            },
        )
        .unwrap();
    assert_eq!(
        runtime.select_proxies(&req, &HashSet::new()).unwrap()[0].id,
        primary
    );
    bind();
    let backup_record = runtime.db.get_proxy(backup).unwrap().unwrap();
    change_proxy(&runtime, &backup_record, "http", 0);
    assert_eq!(
        runtime.select_proxies(&req, &HashSet::new()).unwrap()[0].id,
        primary
    );
    change_proxy(&runtime, &backup_record, "http", 1);
    assert_eq!(
        runtime.select_proxies(&req, &HashSet::new()).unwrap()[0].id,
        primary
    );
    bind();
    runtime
        .record_route_failure_locked(backup, &req, FailureScope::Target)
        .await;
    assert_eq!(
        runtime.select_proxies(&req, &HashSet::new()).unwrap()[0].id,
        primary
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn probe_persistence_cannot_block_success_delivery_or_overwrite_newer_traffic() {
    let runtime = runtime();
    let (port, server) = http_proxy(vec!["HTTP/1.1 200 OK\r\n\r\n"]).await;
    let proxy = add_proxy(&runtime, port);
    let generation = runtime
        .db
        .routing_snapshot()
        .node_generations
        .get(&proxy.id)
        .copied();
    let (entered, ready) = std::sync::mpsc::channel();
    let (release, wait) = std::sync::mpsc::channel();
    let writer = std::thread::spawn({
        let db = runtime.db.clone();
        move || db.hold_connection_for_test(entered, wait)
    });
    ready.recv_timeout(Duration::from_secs(1)).unwrap();
    let probe = tokio::spawn({
        let runtime = runtime.clone();
        let proxy = proxy.clone();
        async move {
            runtime
                .record_probe_result(
                    &proxy,
                    generation,
                    monotonic_millis(),
                    Some("inactive"),
                    None,
                    false,
                )
                .await
        }
    });
    timeout(Duration::from_secs(1), async {
        loop {
            if runtime
                .metrics
                .read()
                .unwrap()
                .get(&proxy.id)
                .is_some_and(|m| m.pushed_status.as_deref() == Some("inactive"))
            {
                break;
            }
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap();
    let connection = timeout(
        Duration::from_millis(500),
        connect_with_fail_fast(runtime.clone(), &request("example.com"), Instant::now()),
    )
    .await;
    release.send(()).unwrap();
    writer.join().unwrap();
    assert!(connection.is_ok());
    drop(connection.unwrap().unwrap());
    probe.await.unwrap().unwrap();
    server.await.unwrap();
    assert!(runtime.flush_logs(Duration::from_secs(2)));
    assert_eq!(
        runtime
            .db
            .get_proxy(proxy.id)
            .unwrap()
            .unwrap()
            .status
            .as_deref(),
        Some("active")
    );
}

pub(super) fn runtime() -> Arc<ProxyRuntime> {
    let db = Database::open_in_memory().unwrap();
    let (events, _) = broadcast::channel(16);
    let mut config = default_advanced_config();
    config.insert("circuit_failure_threshold".into(), json!(1));
    Arc::new(ProxyRuntime::new(db, events, "127.0.0.1", 0, &json!(config)).unwrap())
}

#[tokio::test]
async fn saturated_dial_capacity_waits_without_holding_selection_or_leaking_on_cancel() {
    let runtime = runtime();
    add_proxy(&runtime, 19001);
    let mut leases = Vec::new();
    for _ in 0..32 {
        leases.push(
            runtime
                .reserve_proxy(&request("example.com"), &HashSet::new())
                .await
                .unwrap()
                .unwrap(),
        );
    }
    let waiter = tokio::spawn({
        let runtime = runtime.clone();
        async move {
            runtime
                .reserve_proxy(&request("example.com"), &HashSet::new())
                .await
                .unwrap()
                .unwrap()
        }
    });
    tokio::task::yield_now().await;
    assert!(!waiter.is_finished());
    assert!(runtime.selection_lock.try_lock().is_ok());
    drop(leases.pop());
    let lease = timeout(Duration::from_secs(1), waiter)
        .await
        .unwrap()
        .unwrap();
    assert!(timeout(
        Duration::from_millis(10),
        runtime.reserve_proxy(&request("example.com"), &HashSet::new())
    )
    .await
    .is_err());
    drop(lease);
    drop(leases);
    assert!(runtime.active_connections.lock().unwrap().is_empty());
    assert_eq!(
        runtime
            .dial_slots
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .values()
            .next()
            .unwrap()
            .available_permits(),
        32
    );
}

#[tokio::test]
async fn saturated_best_node_reselects_the_best_available_candidate() {
    for algorithm in ["least_connections", "adaptive"] {
        let runtime = runtime();
        runtime.runtime_settings.write().unwrap().algorithm = algorithm.into();
        let a = add_proxy(&runtime, 19001);
        let target = request("example.com");
        let mut pending = Vec::new();
        for _ in 0..32 {
            pending.push(
                runtime
                    .reserve_proxy(&target, &HashSet::new())
                    .await
                    .unwrap()
                    .unwrap(),
            );
        }
        let b = add_proxy(&runtime, 19002);
        let c = add_proxy(&runtime, 19003);
        runtime
            .active_connections
            .lock()
            .unwrap()
            .extend([(b.id, 100), (c.id, 40)]);
        runtime.adaptive_sequence.lock().unwrap().insert(0, 0); // not a learning turn
        assert_eq!(runtime.active_connections.lock().unwrap()[&a.id], 32);
        assert_eq!(
            runtime.dial_slots.lock().unwrap()[&a.id].available_permits(),
            0
        );
        let selected = runtime
            .reserve_proxy(&target, &HashSet::new())
            .await
            .unwrap()
            .unwrap();
        assert_eq!(
            selected.id, c.id,
            "{algorithm} must reselect C, not unsorted B"
        );
        assert!(runtime.metrics.read().unwrap().is_empty());
        drop(selected);
        drop(pending);
        assert_eq!(runtime.global_dial_slots.available_permits(), 64);
        assert_eq!(
            runtime.dial_slots.lock().unwrap()[&a.id].available_permits(),
            32
        );
    }
}

#[tokio::test]
async fn saturated_selection_does_not_advance_round_robin_or_learning_cursors() {
    for algorithm in ["round_robin", "adaptive"] {
        let runtime = runtime();
        runtime.runtime_settings.write().unwrap().algorithm = algorithm.into();
        add_proxy(&runtime, 19001);
        let target = request("example.com");
        let mut pending = Vec::new();
        for _ in 0..32 {
            pending.push(
                runtime
                    .reserve_proxy(&target, &HashSet::new())
                    .await
                    .unwrap()
                    .unwrap(),
            );
        }
        let cursor = runtime.round_robin_index.lock().unwrap().clone();
        let learning = runtime.adaptive_sequence.lock().unwrap().clone();
        assert!(timeout(
            Duration::from_millis(20),
            runtime.reserve_proxy(&target, &HashSet::new())
        )
        .await
        .is_err());
        assert_eq!(*runtime.round_robin_index.lock().unwrap(), cursor);
        assert_eq!(*runtime.adaptive_sequence.lock().unwrap(), learning);
        assert!(runtime.selection_lock.try_lock().is_ok());
        drop(pending);
        assert_eq!(runtime.global_dial_slots.available_permits(), 64);
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn hot_routing_and_logging_do_not_wait_for_the_database_connection() {
    let runtime = runtime();
    let proxy = add_proxy(&runtime, 19001);
    let (entered, ready) = std::sync::mpsc::channel();
    let (release, wait) = std::sync::mpsc::channel();
    let worker = std::thread::spawn({
        let db = runtime.db.clone();
        move || db.hold_connection_for_test(entered, wait)
    });
    ready.recv_timeout(Duration::from_secs(1)).unwrap();
    let routed = timeout(Duration::from_millis(100), async {
        let lease = runtime
            .reserve_proxy(&request("example.com"), &HashSet::new())
            .await
            .unwrap()
            .unwrap();
        runtime.record_connection_success_locked(proxy.id, 10).await;
        runtime
            .record_request(RequestLogEntry {
                proxy_id: Some(proxy.id),
                target_host: "example.com",
                target_port: 443,
                success: true,
                response_time: Some(10),
                error_message: None,
                result_type: "tunnel_established",
            })
            .await;
        drop(lease);
    })
    .await;
    release.send(()).unwrap();
    worker.join().unwrap();
    assert!(routed.is_ok());
    assert!(runtime.flush_logs(Duration::from_secs(2)));
    assert!(runtime.active_connections.lock().unwrap().is_empty());
}

#[tokio::test]
async fn queued_passive_health_cannot_overwrite_a_newer_probe_result() {
    let runtime = runtime();
    let proxy = add_proxy(&runtime, 19001);
    let generation = runtime.db.routing_snapshot().node_generations[&proxy.id];
    let revision = runtime.db.reserve_status_revision(proxy.id);
    runtime
        .record_probe_result(
            &proxy,
            Some(generation),
            monotonic_millis(),
            Some("active"),
            Some(20),
            true,
        )
        .await
        .unwrap();
    runtime
        .db
        .update_passive_status(&proxy, generation, revision, "inactive", None)
        .unwrap();
    assert_eq!(
        runtime
            .db
            .get_proxy(proxy.id)
            .unwrap()
            .unwrap()
            .status
            .as_deref(),
        Some("active")
    );
}

#[tokio::test]
async fn probe_target_failure_and_local_network_failure_do_not_poison_global_health() {
    let runtime = runtime();
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let proxy = add_proxy(&runtime, listener.local_addr().unwrap().port());
    let server = tokio::spawn(async move {
        for _ in 0..2 {
            let (mut stream, _) = listener.accept().await.unwrap();
            read_http_request_header(&mut stream, Vec::new(), 1000)
                .await
                .unwrap();
            stream
                .write_all(
                    b"HTTP/1.1 503 Unavailable\r\nContent-Length: 0\r\nConnection: close\r\n\r\n",
                )
                .await
                .unwrap();
            stream.shutdown().await.unwrap();
        }
    });
    let result = crate::proxy_tester::test_proxy(&proxy, "http://target.test/", 2000).await;
    server.await.unwrap();
    assert!(!result.success);
    assert_eq!(result.failure_scope.as_deref(), Some("target"));
    runtime
        .record_probe_result(
            &proxy,
            runtime
                .db
                .routing_snapshot()
                .node_generations
                .get(&proxy.id)
                .copied(),
            monotonic_millis(),
            None,
            None,
            false,
        )
        .await
        .unwrap();
    let latest = runtime.db.get_proxy(proxy.id).unwrap().unwrap();
    assert_eq!(latest.status.as_deref(), Some("unknown"));
    assert_eq!(latest.fail_count, 0);
    runtime
        .record_route_failure_locked(proxy.id, &request("target.test"), FailureScope::LocalRoute)
        .await;
    assert!(
        runtime
            .is_candidate_available(proxy.id, &request("target.test"))
            .await
    );
    let code = if cfg!(windows) {
        10050
    } else if cfg!(target_os = "macos") {
        50
    } else {
        100
    };
    assert!(is_local_network_error(&anyhow::Error::from(
        std::io::Error::from_raw_os_error(code)
    )));
    assert!(!is_local_network_error(&anyhow!("authentication failed")));
}

#[test]
fn million_metric_updates_remain_bounded_and_incremental() {
    let started = Instant::now();
    let mut metric = ProxyMetrics::new();
    for index in 0..1_000_000 {
        metric.push(index % 4 != 0, Some(100));
    }
    assert_eq!(metric.requests.len(), MAX_METRIC_SAMPLES);
    assert_eq!(metric.summary(), (1536, 512, 100));
    eprintln!(
        "BENCH metrics updates=1000000 retained={} elapsed_us={}",
        metric.requests.len(),
        started.elapsed().as_micros()
    );
    let last = metric.requests.back().unwrap().timestamp;
    metric.prune(last + METRICS_WINDOW_MS);
    assert!(!metric.requests.is_empty());
    metric.prune(last + METRICS_WINDOW_MS + 1);
    assert_eq!(metric.summary(), (0, 0, 0));
    assert_eq!(metric.score, 50.0);
}

#[tokio::test]
async fn local_route_failure_retries_other_path_inside_pool_without_penalty() {
    for host in ["192.0.2.1", "2001:db8::1"] {
        let runtime = runtime();
        runtime.runtime_settings.write().unwrap().algorithm = "round_robin".into();
        let first = runtime
            .db
            .create_proxy(ProxyInput {
                name: "unreachable path".into(),
                proxy_type: "http".into(),
                host: host.into(),
                port: 19001,
                enabled: Some(1),
                username: None,
                password: None,
                test_url: None,
                test_timeout: None,
                skip_cert_verify: None,
            })
            .unwrap();
        let (port, server) = http_proxy(vec!["HTTP/1.1 200 Connection Established\r\n\r\n"]).await;
        let second = add_proxy(&runtime, port);
        let outside = add_proxy(&runtime, 19003);
        runtime
            .db
            .create_proxy_group(ProxyGroupInput {
                name: Some("routes".into()),
                domains: Some(vec!["example.com".into()]),
                proxy_ids: Some(vec![first.id, second.id]),
                algorithm_override: Some(Some("round_robin".into())),
                ..Default::default()
            })
            .unwrap();
        runtime
            .injected_connect_errors
            .lock()
            .unwrap()
            .insert(first.id, std::io::ErrorKind::NetworkUnreachable);
        let (lease, _) =
            connect_with_fail_fast(runtime.clone(), &request("example.com"), Instant::now())
                .await
                .unwrap();
        assert_eq!(lease.id, second.id);
        assert!(
            runtime.injected_connect_errors.lock().unwrap().is_empty(),
            "first route must actually be attempted"
        );
        assert_eq!(
            runtime.circuit_breakers.read().unwrap()[&first.id].failures,
            0
        );
        assert!(!runtime.metrics.read().unwrap().contains_key(&first.id));
        assert!(!runtime
            .circuit_breakers
            .read()
            .unwrap()
            .contains_key(&outside.id));
        server.await.unwrap();
    }
}

#[tokio::test]
async fn local_route_failures_remain_bounded_when_all_paths_are_offline() {
    let runtime = runtime();
    {
        let mut settings = runtime.runtime_settings.write().unwrap();
        settings.algorithm = "round_robin".into();
        settings.fail_fast.max_attempts = 2;
        settings.fail_fast.total_timeout_ms = 100;
    }
    for port in [19001, 19002, 19003] {
        let proxy = add_proxy(&runtime, port);
        runtime
            .injected_connect_errors
            .lock()
            .unwrap()
            .insert(proxy.id, std::io::ErrorKind::NetworkDown);
    }
    let started = Instant::now();
    let error = connect_with_fail_fast(runtime.clone(), &request("example.com"), started)
        .await
        .err()
        .unwrap();
    assert!(error.to_string().contains("本地路由不可达"));
    assert!(started.elapsed() < Duration::from_secs(1));
    assert_eq!(runtime.injected_connect_errors.lock().unwrap().len(), 1);
    assert!(runtime.metrics.read().unwrap().is_empty());
    assert!(runtime
        .circuit_breakers
        .read()
        .unwrap()
        .values()
        .all(|breaker| breaker.failures == 0));
    assert_eq!(runtime.global_dial_slots.available_permits(), 64);
}

#[tokio::test]
async fn full_log_queue_cannot_block_immediate_health_or_latest_status_persistence() {
    let runtime = runtime();
    let proxy = add_proxy(&runtime, 19001);
    let pause = runtime.database_worker.pause_for_test();
    for _ in 0..4096 {
        runtime.database_worker.log(RequestLogEntry {
            proxy_id: None,
            target_host: "busy",
            target_port: 80,
            success: false,
            response_time: None,
            error_message: None,
            result_type: "proxy_exhausted",
        });
    }
    assert_eq!(runtime.database_worker.stats()["queueLength"], 2048);
    for _ in 0..3 {
        runtime.record_breaker_failure_locked(proxy.id).await;
        assert_eq!(
            runtime.metrics.read().unwrap()[&proxy.id]
                .pushed_status
                .as_deref(),
            Some("inactive")
        );
        runtime.record_breaker_success(proxy.id).await;
        runtime.record_connection_success_locked(proxy.id, 1).await;
        assert_eq!(
            runtime.metrics.read().unwrap()[&proxy.id]
                .pushed_status
                .as_deref(),
            Some("active")
        );
    }
    assert_eq!(
        runtime
            .db
            .get_proxy(proxy.id)
            .unwrap()
            .unwrap()
            .status
            .as_deref(),
        Some("unknown")
    );
    assert_eq!(runtime.database_worker.stats()["statusQueueLength"], 1);
    assert_eq!(runtime.database_worker.stats()["coalescedStatus"], 5);
    assert_eq!(runtime.database_worker.stats()["droppedStatus"], 0);
    drop(pause);
    assert!(runtime.flush_logs(Duration::from_secs(3)));
    assert_eq!(
        runtime
            .db
            .get_proxy(proxy.id)
            .unwrap()
            .unwrap()
            .status
            .as_deref(),
        Some("active")
    );
}

#[tokio::test]
async fn least_connections_serial_ties_rotate_and_leases_release() {
    let runtime = runtime();
    let a = add_proxy(&runtime, 19001);
    let b = add_proxy(&runtime, 19002);
    runtime
        .update_load_settings(&Map::from_iter([(
            "algorithm".into(),
            json!("least_connections"),
        )]))
        .await
        .unwrap();
    let mut counts = HashMap::<i64, usize>::new();
    for _ in 0..100 {
        let lease = runtime
            .reserve_proxy(&request("example.com"), &HashSet::new())
            .await
            .unwrap()
            .unwrap();
        *counts.entry(lease.id).or_default() += 1;
        drop(lease);
    }
    assert_eq!(counts[&a.id], 50);
    assert_eq!(counts[&b.id], 50);
    assert!(runtime.active_connections.lock().unwrap().is_empty());
}

#[tokio::test]
async fn aborting_a_half_open_lease_releases_slot_without_cancelling_its_successor() {
    let runtime = runtime();
    let proxy = add_proxy(&runtime, 19001);
    runtime.record_breaker_failure_locked(proxy.id).await;
    runtime
        .circuit_breakers
        .write()
        .unwrap_or_else(|e| e.into_inner())
        .get_mut(&proxy.id)
        .unwrap()
        .next_attempt = 0;
    let lease = runtime
        .reserve_proxy(&request("example.com"), &HashSet::new())
        .await
        .unwrap()
        .unwrap();
    assert!(
        !runtime
            .is_candidate_available(proxy.id, &request("example.com"))
            .await
    );
    let (tx, rx) = tokio::sync::oneshot::channel();
    let task = tokio::spawn(async move {
        let _lease = lease;
        let _ = tx.send(());
        std::future::pending::<()>().await;
    });
    rx.await.unwrap();
    task.abort();
    let _ = task.await;
    assert!(runtime.active_connections.lock().unwrap().is_empty());
    let next = runtime
        .reserve_proxy(&request("example.com"), &HashSet::new())
        .await
        .unwrap()
        .unwrap();
    assert!(
        !runtime
            .is_candidate_available(proxy.id, &request("example.com"))
            .await
    );
    drop(next);
    assert!(
        runtime
            .is_candidate_available(proxy.id, &request("example.com"))
            .await
    );
    assert_eq!(
        runtime.dial_slots.lock().unwrap_or_else(|e| e.into_inner())[&proxy.id].available_permits(),
        32
    );
}

#[tokio::test]
async fn cancelled_dial_releases_all_reservations() {
    let runtime = runtime();
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    add_proxy(&runtime, listener.local_addr().unwrap().port());
    let other = runtime.clone();
    let task = tokio::spawn(async move {
        connect_with_fail_fast(other, &request("example.com"), Instant::now()).await
    });
    let (_socket, _) = listener.accept().await.unwrap();
    task.abort();
    let _ = task.await;
    assert!(runtime.active_connections.lock().unwrap().is_empty());
    assert!(runtime.selection_lock.try_lock().is_ok());
    assert!(runtime.active_connections.lock().unwrap().is_empty());
}

#[tokio::test]
async fn inbound_sniff_is_bytewise_and_invalid_or_oversized_headers_are_bounded() {
    for method in [
        "CONNECT example.com:443",
        "GET http://example.com/",
        "POST http://example.com/",
    ] {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let mut sender = TcpStream::connect(listener.local_addr().unwrap())
            .await
            .unwrap();
        let (mut stream, _) = listener.accept().await.unwrap();
        let content = format!("{method} HTTP/1.1\r\nHost: example.com\r\n\r\n");
        let task = tokio::spawn(async move {
            for byte in content.bytes() {
                sender.write_all(&[byte]).await.unwrap();
                tokio::task::yield_now().await;
            }
            sender
        });
        let initial = timeout(Duration::from_secs(1), sniff_protocol(&mut stream))
            .await
            .unwrap()
            .unwrap();
        handle_http_proxy_header(&mut stream, initial, &InboundAuth::default())
            .await
            .unwrap();
        task.await.unwrap();
    }
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let mut sender = TcpStream::connect(listener.local_addr().unwrap())
        .await
        .unwrap();
    let (mut stream, _) = listener.accept().await.unwrap();
    sender.write_all(b"G").await.unwrap();
    assert!(
        timeout(Duration::from_millis(20), sniff_protocol(&mut stream))
            .await
            .is_err()
    );
    sender.write_all(b"BOGUS ").await.unwrap();
    assert!(sniff_protocol(&mut stream).await.is_err());
    let oversized = [vec![b'x'; 65536], b"\r\n\r\n".to_vec()].concat();
    assert!(read_http_request_header(&mut stream, oversized, 1)
        .await
        .is_err());
}

#[tokio::test]
async fn deleted_pools_reclaim_cursors_and_snapshot_generations_detect_aba() {
    let runtime = runtime();
    let proxy = add_proxy(&runtime, 19001);
    let old = runtime.db.routing_snapshot().node_generations[&proxy.id];
    change_proxy(&runtime, &proxy, "socks5", 1);
    change_proxy(&runtime, &proxy, "http", 1);
    assert_ne!(
        runtime.db.routing_snapshot().node_generations[&proxy.id],
        old
    );
    runtime
        .database_worker
        .status(proxy.clone(), old, "inactive", None);
    assert!(runtime
        .record_probe_result(
            &proxy,
            Some(old),
            monotonic_millis(),
            Some("active"),
            Some(10),
            true
        )
        .await
        .is_err());
    assert!(runtime.flush_logs(Duration::from_secs(2)));
    assert_eq!(
        runtime
            .db
            .get_proxy(proxy.id)
            .unwrap()
            .unwrap()
            .status
            .as_deref(),
        Some("unknown")
    );
    runtime
        .round_robin_index
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .insert(12345, 1);
    runtime
        .select_proxies(&request("example.com"), &HashSet::new())
        .unwrap();
    assert!(!runtime
        .round_robin_index
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .contains_key(&12345));
}

#[tokio::test]
async fn cold_adaptive_member_receives_bounded_learning_without_bypassing_breakers() {
    let runtime = runtime();
    let a = add_proxy(&runtime, 19001);
    let b = add_proxy(&runtime, 19002);
    for _ in 0..10 {
        runtime.record_connection_success_locked(a.id, 10).await;
    }
    let mut seen = HashSet::new();
    for _ in 0..64 {
        let lease = runtime
            .reserve_proxy(&request("example.com"), &HashSet::new())
            .await
            .unwrap()
            .unwrap();
        seen.insert(lease.id);
    }
    assert!(seen.contains(&b.id));
    runtime.record_breaker_failure_locked(b.id).await;
    for _ in 0..32 {
        let lease = runtime
            .reserve_proxy(&request("example.com"), &HashSet::new())
            .await
            .unwrap()
            .unwrap();
        assert_eq!(lease.id, a.id);
    }
}

#[tokio::test]
async fn http_forward_auth_failures_accumulate_at_realistic_thresholds() {
    for threshold in [3, 5] {
        let runtime = runtime();
        runtime
            .runtime_settings
            .write()
            .unwrap()
            .circuit
            .failure_threshold = threshold;
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let proxy = add_proxy(&runtime, listener.local_addr().unwrap().port());
        let mut target = request("example.com");
        target.inbound = InboundProtocol::HttpForward;
        target.initial_payload = b"GET / HTTP/1.1\r\nHost: example.com\r\n\r\n".to_vec();
        for failure in 1..=threshold {
            let (lease, upstream) =
                connect_with_fail_fast(runtime.clone(), &target, Instant::now())
                    .await
                    .unwrap();
            assert!(!upstream.target_verified);
            assert!(runtime
                .recent_success_map()
                .await
                .get(&proxy.id)
                .is_none_or(|time| *time == 0));
            assert!(
                runtime
                    .observe_forward_response(
                        &lease,
                        &target,
                        407,
                        false,
                        upstream.proxy_latency_us
                    )
                    .await
            );
            assert_eq!(
                runtime.circuit_breakers.read().unwrap()[&proxy.id].failures,
                failure
            );
        }
        assert!(!runtime.is_candidate_available(proxy.id, &target).await);
        assert!(runtime.flush_logs(Duration::from_secs(2)));
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
    }
}

#[tokio::test]
async fn http_forward_half_open_waits_for_response_and_rejects_old_attempts() {
    let runtime = runtime();
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let proxy = add_proxy(&runtime, listener.local_addr().unwrap().port());
    let mut target = request("example.com");
    target.inbound = InboundProtocol::HttpForward;
    target.initial_payload = b"GET / HTTP/1.1\r\nHost: example.com\r\n\r\n".to_vec();
    runtime.record_breaker_failure_locked(proxy.id).await;
    runtime
        .circuit_breakers
        .write()
        .unwrap()
        .get_mut(&proxy.id)
        .unwrap()
        .next_attempt = 0;
    let (lease, upstream) = connect_with_fail_fast(runtime.clone(), &target, Instant::now())
        .await
        .unwrap();
    assert_eq!(
        runtime.circuit_breakers.read().unwrap()[&proxy.id].state,
        "HALF_OPEN"
    );
    assert!(!runtime.is_candidate_available(proxy.id, &target).await);
    assert!(runtime.recent_success_map().await[&proxy.id] == 0);
    // Cancellation releases ownership even without any response.
    drop((lease, upstream));
    let (lease, upstream) = connect_with_fail_fast(runtime.clone(), &target, Instant::now())
        .await
        .unwrap();
    let replacement = Arc::new(());
    runtime
        .circuit_breakers
        .write()
        .unwrap()
        .get_mut(&proxy.id)
        .unwrap()
        .attempt = Some(Arc::downgrade(&replacement));
    assert!(
        !runtime
            .observe_forward_response(&lease, &target, 200, false, upstream.proxy_latency_us)
            .await
    );
    assert_eq!(
        runtime.circuit_breakers.read().unwrap()[&proxy.id].state,
        "HALF_OPEN"
    );
    runtime
        .circuit_breakers
        .write()
        .unwrap()
        .get_mut(&proxy.id)
        .unwrap()
        .attempt = Some(Arc::downgrade(&lease.attempt));
    assert!(
        runtime
            .observe_forward_response(&lease, &target, 503, false, upstream.proxy_latency_us)
            .await
    );
    assert_eq!(
        runtime.circuit_breakers.read().unwrap()[&proxy.id].state,
        "CLOSED"
    );
    assert!(runtime.recent_success_map().await[&proxy.id] > 0);
    assert!(runtime.flush_logs(Duration::from_secs(2)));
    assert_eq!(
        runtime
            .db
            .get_proxy(proxy.id)
            .unwrap()
            .unwrap()
            .status
            .as_deref(),
        Some("active")
    );
}

#[tokio::test]
async fn http_forward_observes_407_and_5xx_without_replay_or_global_target_penalty() {
    for status in [407, 503] {
        let runtime = runtime();
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let proxy = add_proxy(&runtime, listener.local_addr().unwrap().port());
        let server = tokio::spawn(async move {
            let (mut stream, _) = listener.accept().await.unwrap();
            read_http_request_header(&mut stream, Vec::new(), 1000)
                .await
                .unwrap();
            stream.write_all(format!("HTTP/1.1 100 Continue\r\n\r\nHTTP/1.1 {status} Result\r\nContent-Length: 3\r\n\r\nend").as_bytes()).await.unwrap();
            stream.shutdown().await.unwrap();
            let mut drain = Vec::new();
            stream.read_to_end(&mut drain).await.unwrap();
        });
        let front = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let mut client = TcpStream::connect(front.local_addr().unwrap())
            .await
            .unwrap();
        let (stream, address) = front.accept().await.unwrap();
        let task = tokio::spawn(handle_client(runtime.clone(), stream, address));
        client
            .write_all(b"GET http://example.com/ HTTP/1.1\r\nHost: example.com\r\n\r\n")
            .await
            .unwrap();
        let mut bytes = Vec::new();
        client.read_to_end(&mut bytes).await.unwrap();
        client.shutdown().await.unwrap();
        let handled = task.await.unwrap();
        server.await.unwrap();
        assert!(runtime.flush_logs(Duration::from_secs(2)));
        assert!(
            bytes.ends_with(b"end"),
            "response={:?}, handler={handled:?}",
            String::from_utf8_lossy(&bytes)
        );
        handled.unwrap();
        assert_eq!(
            runtime
                .is_candidate_available(proxy.id, &request("other.test"))
                .await,
            status != 407
        );
        assert!(runtime.flush_logs(Duration::from_secs(2)));
        let (logs, _) = runtime.db.traffic_logs(1, 25, None).unwrap();
        assert!(logs
            .iter()
            .filter(|log| log.result_type.as_deref() == Some("upstream_response_observed"))
            .all(|log| log.success == 0));
        assert_eq!(
            logs.iter()
                .filter(|log| log.result_type.as_deref() == Some("upstream_response_observed"))
                .count(),
            1
        );
    }
}

#[tokio::test]
async fn fragmented_http_methods_reach_routing_without_losing_bytes() {
    let runtime = runtime();
    for method in ["CONNECT", "GET", "POST"] {
        let header = if method == "CONNECT" {
            "CONNECT example.com:443 HTTP/1.1\r\nHost: example.com\r\n\r\n".to_string()
        } else {
            format!("{method} http://example.com/ HTTP/1.1\r\nHost: example.com\r\n\r\n")
        };
        for split in 1..header.len() {
            let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
            let mut sender = TcpStream::connect(listener.local_addr().unwrap())
                .await
                .unwrap();
            let (stream, addr) = listener.accept().await.unwrap();
            sender.set_nodelay(true).unwrap();
            let task = tokio::spawn(handle_client(runtime.clone(), stream, addr));
            sender.write_all(&header.as_bytes()[..split]).await.unwrap();
            tokio::time::sleep(Duration::from_millis(1)).await;
            let _ = sender.write_all(&header.as_bytes()[split..]).await;
            let error = task.await.unwrap().unwrap_err().to_string();
            assert!(
                !error.contains("不支持的入站"),
                "{method} at {split}: {error}"
            );
        }
    }
}

#[tokio::test]
async fn round_robin_is_fair_across_interleaved_pools_and_health_tiers() {
    let runtime = runtime();
    runtime
        .update_load_settings(&Map::from_iter([(
            "algorithm".into(),
            json!("round_robin"),
        )]))
        .await
        .unwrap();
    let proxies = (10001..10006)
        .map(|port| add_proxy(&runtime, port))
        .collect::<Vec<_>>();
    for (host, ids) in [
        (
            "one.test",
            vec![proxies[0].id, proxies[1].id, proxies[4].id],
        ),
        ("two.test", vec![proxies[2].id, proxies[3].id]),
    ] {
        runtime
            .db
            .create_proxy_group(ProxyGroupInput {
                name: Some(host.into()),
                domains: Some(vec![host.into()]),
                proxy_ids: Some(ids),
                is_default: Some(0),
                enabled: Some(1),
                ..Default::default()
            })
            .unwrap();
    }
    runtime
        .db
        .update_proxy_status(proxies[4].id, "inactive", None, 0, 0)
        .unwrap();
    let mut counts = HashMap::<i64, usize>::new();
    for _ in 0..100 {
        for host in ["one.test", "two.test"] {
            let ordered = runtime
                .select_proxies(&request(host), &HashSet::new())
                .unwrap();
            *counts.entry(ordered[0].id).or_default() += 1;
        }
    }
    for proxy in &proxies[..4] {
        assert_eq!(counts.get(&proxy.id), Some(&50));
    }
    assert!(!counts.contains_key(&proxies[4].id));
}

pub(super) fn request(host: &str) -> TargetRequest {
    TargetRequest {
        host: host.into(),
        original_host: host.into(),
        port: 443,
        address_type: ADDR_DOMAIN,
        inbound: InboundProtocol::HttpConnect,
        initial_payload: Vec::new(),
    }
}

pub(super) fn add_proxy(runtime: &ProxyRuntime, port: u16) -> ProxyRecord {
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
    assert!(runtime
        .metrics
        .read()
        .unwrap_or_else(|e| e.into_inner())
        .get(&proxy.id)
        .is_none());

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
    runtime.increment_active(first.id);
    let ordered = runtime
        .order_proxies(vec![first, second.clone()], "adaptive", "example.com")
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
    metric.requests.front_mut().unwrap().timestamp = monotonic_millis() - 6 * 60 * 1000;
    runtime
        .metrics
        .write()
        .unwrap_or_else(|e| e.into_inner())
        .insert(first.id, metric);
    let ordered = runtime
        .order_proxies(vec![first.clone(), second], "adaptive", "example.com")
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
        });
    }
    let mut selected = HashMap::<i64, usize>::new();
    let mut leases = Vec::new();
    while let Some(result) = tasks.join_next().await {
        let lease = result.unwrap();
        *selected.entry(lease.id).or_default() += 1;
        leases.push(lease);
    }
    assert_eq!(selected[&first.id], 10);
    assert_eq!(selected[&second.id], 10);
    assert_eq!(runtime.active_connections.lock().unwrap()[&first.id], 10);
    assert_eq!(runtime.active_connections.lock().unwrap()[&second.id], 10);
    drop(leases);
    assert!(runtime.active_connections.lock().unwrap().is_empty());
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
        .record_probe_result(
            &first,
            runtime
                .db
                .routing_snapshot()
                .node_generations
                .get(&first.id)
                .copied(),
            monotonic_millis(),
            Some("active"),
            Some(30),
            true,
        )
        .await
        .unwrap();
    assert!(!runtime.is_candidate_available(first.id, &blocked).await);
    let key = TargetRouteKey::new(first.id, &blocked);
    runtime
        .target_circuits
        .write()
        .unwrap_or_else(|e| e.into_inner())
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
    assert!(runtime.active_connections.lock().unwrap().is_empty());
    assert!(runtime.flush_logs(Duration::from_secs(2)));
    assert!(runtime
        .target_circuits
        .read()
        .unwrap_or_else(|e| e.into_inner())
        .is_empty());
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
            ..Default::default()
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
    {
        let active = runtime.active_connections.lock().unwrap();
        assert!(!active.contains_key(&first.id));
        assert!(!active.contains_key(&outside.id));
        assert_eq!(active[&second.id], 1);
    }
    runtime.decrement_active(second.id).await;
    assert!(runtime.active_connections.lock().unwrap().is_empty());
    assert!(
        runtime.metrics.read().unwrap_or_else(|e| e.into_inner())[&second.id]
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
            ..Default::default()
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
        .unwrap_or_else(|e| e.into_inner())
        .contains_key(&outside.id));
    assert!(runtime.active_connections.lock().unwrap().is_empty());
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
        .unwrap_or_else(|e| e.into_inner())
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
        .unwrap_or_else(|e| e.into_inner())
        .contains_key(&second.id));
    assert!(runtime
        .target_circuits
        .read()
        .unwrap_or_else(|e| e.into_inner())
        .contains_key(&TargetRouteKey::new(first.id, &request("example.com"))));
    runtime
        .runtime_settings
        .write()
        .unwrap_or_else(|e| e.into_inner())
        .fail_fast
        .enabled = false;
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
            .unwrap_or_else(|e| e.into_inner())
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
        assert!(runtime.active_connections.lock().unwrap().is_empty());
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
    {
        let metrics = runtime.metrics.read().unwrap_or_else(|e| e.into_inner());
        assert!(
            metrics[&proxy.id]
                .requests
                .back()
                .unwrap()
                .response_time
                .unwrap()
                < 5000
        );
    }
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
    assert!(runtime.active_connections.lock().unwrap().is_empty());
    assert!(runtime
        .circuit_breakers
        .read()
        .unwrap_or_else(|e| e.into_inner())
        .is_empty());
}

#[tokio::test]
async fn target_circuit_cache_is_bounded_and_reclaims_old_destinations() {
    let runtime = runtime();
    let proxy = add_proxy(&runtime, 10001);
    let config = runtime
        .runtime_settings
        .read()
        .unwrap_or_else(|e| e.into_inner())
        .circuit;
    {
        let mut targets = runtime
            .target_circuits
            .write()
            .unwrap_or_else(|e| e.into_inner());
        for index in 0..MAX_TARGET_CIRCUITS {
            targets.insert(
                TargetRouteKey::new(proxy.id, &request(&format!("{index}.example.com"))),
                TargetCircuit {
                    breaker: CircuitBreaker::new(config),
                    last_failure: monotonic_millis(),
                },
            );
        }
    }
    let new_target = request("new.example.com");
    runtime
        .record_route_failure_locked(proxy.id, &new_target, FailureScope::Target)
        .await;
    assert_eq!(
        runtime
            .target_circuits
            .read()
            .unwrap_or_else(|e| e.into_inner())
            .len(),
        MAX_TARGET_CIRCUITS
    );
    assert!(!runtime.is_candidate_available(proxy.id, &new_target).await);
    for target in runtime
        .target_circuits
        .write()
        .unwrap_or_else(|e| e.into_inner())
        .values_mut()
    {
        target.last_failure = monotonic_millis() - METRICS_WINDOW_MS - 1;
    }
    runtime
        .record_route_failure_locked(
            proxy.id,
            &request("fresh.example.com"),
            FailureScope::Target,
        )
        .await;
    assert_eq!(
        runtime
            .target_circuits
            .read()
            .unwrap_or_else(|e| e.into_inner())
            .len(),
        1
    );
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
            ..Default::default()
        })
        .unwrap();
    assert!(runtime
        .reserve_proxy(&request("example.com"), &HashSet::new())
        .await
        .is_err());
    assert!(runtime.active_connections.lock().unwrap().is_empty());
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
        .unwrap_or_else(|e| e.into_inner())
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
    assert!(runtime.active_connections.lock().unwrap().is_empty());
    assert!(!runtime
        .circuit_breakers
        .read()
        .unwrap_or_else(|e| e.into_inner())
        .contains_key(&second.id));
    assert!(
        runtime
            .metrics
            .read()
            .unwrap_or_else(|e| e.into_inner())
            .get(&first.id)
            .is_none(),
        "HTTP 转发未验证目标，不应记录为目标建连成功"
    );
    assert!(runtime.flush_logs(Duration::from_secs(2)));
    let (logs, total) = runtime.db.traffic_logs(1, 25, None).unwrap();
    assert_eq!(total, 2);
    assert!(logs
        .iter()
        .any(|log| log.result_type.as_deref() == Some("forwarded_unverified") && log.success == 0));
    assert!(logs
        .iter()
        .any(|log| log.result_type.as_deref() == Some("transfer_finished")));
    assert_eq!(runtime.db.overview(0).unwrap()["successRequests"], json!(0));
}
