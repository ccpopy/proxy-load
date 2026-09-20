use super::probe_tests::{add, state};
use super::*;
use crate::{
    models::{ProxyGroupInput, ProxyInput},
    probe_health::{HealthMode, Readiness},
};
use std::sync::atomic::{AtomicU8, AtomicUsize, Ordering};
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::TcpListener,
};

// 0: CONNECT hangs; 1: 204; 2: login redirect; 3: unexpected 404.
async fn socks_fixture() -> (
    u16,
    Arc<AtomicU8>,
    Arc<AtomicUsize>,
    tokio::task::JoinHandle<()>,
) {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();
    let mode = Arc::new(AtomicU8::new(1));
    let count = Arc::new(AtomicUsize::new(0));
    let task = tokio::spawn({
        let mode = mode.clone();
        let count = count.clone();
        async move {
            let mut sockets = JoinSet::new();
            loop {
                tokio::select! {
                    accepted = listener.accept() => {
                        let (mut io, _) = accepted.unwrap();
                        let mode = mode.clone(); let count = count.clone();
                        sockets.spawn(async move {
                            assert_eq!(io.read_u8().await.unwrap(), 5);
                            let methods = io.read_u8().await.unwrap();
                            io.read_exact(&mut vec![0; methods as usize]).await.unwrap();
                            io.write_all(&[5, 0]).await.unwrap();
                            let mut head = [0;4]; io.read_exact(&mut head).await.unwrap();
                            assert_eq!(&head[..3], &[5,1,0]);
                            let length = match head[3] { 1 => 4, 3 => io.read_u8().await.unwrap() as usize, _ => panic!("address") };
                            let mut host = vec![0;length]; io.read_exact(&mut host).await.unwrap();
                            let _port = io.read_u16().await.unwrap();
                            count.fetch_add(1, Ordering::SeqCst);
                            if mode.load(Ordering::SeqCst) == 0 {
                                let _ = io.read_to_end(&mut Vec::new()).await;
                                return;
                            }
                            io.write_all(&[5,0,0,1,127,0,0,1,0,80]).await.unwrap();
                            if host == b"public.test" {
                                let (mut read, mut write) = io.split();
                                let _ = tokio::io::copy(&mut read, &mut write).await;
                            } else {
                                let mut request = Vec::new();
                                while !request.ends_with(b"\r\n\r\n") { request.push(io.read_u8().await.unwrap()); }
                                let response = match mode.load(Ordering::SeqCst) {
                                    2 => b"HTTP/1.1 302 Found\r\nLocation: http://login.test/\r\nContent-Length: 0\r\n\r\n".as_slice(),
                                    3 => b"HTTP/1.1 404 Not Found\r\nContent-Length: 0\r\n\r\n".as_slice(),
                                    _ => b"HTTP/1.1 204 No Content\r\n\r\n".as_slice(),
                                };
                                let _ = io.write_all(response).await;
                            }
                        });
                    }
                    result = sockets.join_next(), if !sockets.is_empty() => { result.unwrap().unwrap(); }
                }
            }
        }
    });
    (port, mode, count, task)
}

fn dedicated(state: &AppState, port: u16, threshold: u32) -> ProxyRecord {
    let proxy = add(state, port);
    let mut input: ProxyInput = serde_json::from_value(json!(proxy)).unwrap();
    input.proxy_type = "socks5".into();
    input.test_url = Some("http://vpn-only.test/health".into());
    input.test_timeout = Some(1);
    input.health_policy.mode = HealthMode::RequiredProbe;
    input.health_policy.failure_threshold = threshold;
    state.db.update_proxy(proxy.id, input).unwrap().0
}
async fn probe(state: &AppState, proxy: &ProxyRecord) -> TestResult {
    state
        .test_proxy_record(proxy.clone(), ProbeOrigin::Periodic)
        .await
        .unwrap()
        .unwrap()
}

#[tokio::test]
async fn required_probe_connect_hang_isolates_all_algorithms_and_background_recovers() {
    for threshold in [1, 2, 3] {
        let state = state();
        let (port, mode, count, server) = socks_fixture().await;
        let proxy = dedicated(&state, port, threshold);
        let outside = add(&state, if port == 19998 { 19997 } else { 19998 });
        state
            .db
            .create_proxy_group(ProxyGroupInput {
                name: Some("VPN".into()),
                domains: Some(vec!["vpn-only.test".into(), "public.test".into()]),
                proxy_ids: Some(vec![proxy.id]),
                enabled: Some(1),
                ..Default::default()
            })
            .unwrap();
        assert!(probe(&state, &proxy).await.success);
        assert!(!state
            .db
            .current_probe_health(&proxy)
            .eligible(&proxy.health_policy));
        assert!(probe(&state, &proxy).await.success);
        let mut tunnel = state
            .proxy_runtime
            .readiness_dial("public.test")
            .await
            .unwrap();
        tunnel.echo().await;
        mode.store(0, Ordering::SeqCst);
        for n in 1..=threshold {
            let result = probe(&state, &proxy).await;
            assert!(!result.success);
            assert!(result.diagnostics.proxy_proven());
            assert_eq!(
                result.diagnostics.phase,
                Some(proxy::failure::ConnectPhase::TunnelConnect)
            );
            assert_eq!(
                result.diagnostics.scope,
                Some(proxy::failure::FailureScope::TargetRoute)
            );
            assert_eq!(
                result.diagnostics.code,
                Some(proxy::failure::FailureCode::Timeout)
            );
            let current = state.db.get_proxy(proxy.id).unwrap().unwrap();
            assert_eq!(current.probe_health.probe_success_count, 2);
            assert_eq!(current.probe_health.probe_failure_count, u64::from(n));
            assert_eq!(current.probe_health.transport_status, "reachable");
            assert_eq!(
                current.probe_health.readiness_status,
                if n == threshold {
                    Readiness::NotReady
                } else {
                    Readiness::Degraded
                }
            );
        }
        state
            .proxy_runtime
            .readiness_business_success(proxy.id)
            .await;
        for algorithm in [
            "adaptive",
            "round_robin",
            "least_connections",
            "sticky_host",
        ] {
            let error = state
                .proxy_runtime
                .readiness_candidates("vpn-only.test", algorithm)
                .await
                .unwrap_err();
            assert!(error.to_string().contains("业务未就绪"));
            let started = time::Instant::now();
            assert!(state
                .proxy_runtime
                .readiness_dial("vpn-only.test")
                .await
                .is_err());
            assert!(started.elapsed() < Duration::from_millis(250));
            assert!(state
                .proxy_runtime
                .readiness_candidates("unrelated.test", algorithm)
                .await
                .unwrap()
                .contains(&outside.id));
        }
        tunnel.echo().await;
        let before = count.load(Ordering::SeqCst);
        mode.store(1, Ordering::SeqCst);
        let mut last = HashMap::new();
        let schedule = ProbeSchedule {
            base_interval: Duration::from_millis(1),
            recovery_interval: Duration::from_millis(1),
            active_window: Duration::from_secs(600),
            concurrency: 2,
            tick: Duration::from_millis(1),
            startup_probe_enabled: true,
        };
        state.run_probe_cycle(&schedule, &mut last).await.unwrap();
        assert!(count.load(Ordering::SeqCst) > before);
        assert_eq!(
            state.db.current_probe_health(&proxy).readiness_status,
            Readiness::NotReady
        );
        last.clear();
        state.run_probe_cycle(&schedule, &mut last).await.unwrap();
        assert_eq!(
            state.db.current_probe_health(&proxy).readiness_status,
            Readiness::Ready
        );
        for algorithm in [
            "adaptive",
            "round_robin",
            "least_connections",
            "sticky_host",
        ] {
            assert_eq!(
                state
                    .proxy_runtime
                    .readiness_candidates("vpn-only.test", algorithm)
                    .await
                    .unwrap(),
                vec![proxy.id]
            );
        }
        drop(tunnel);
        server.abort();
    }
}

#[tokio::test]
async fn required_probe_rejects_login_redirect_unexpected_status_and_old_url_success() {
    let state = state();
    let (port, mode, _, server) = socks_fixture().await;
    let proxy = dedicated(&state, port, 1);
    for value in [2, 3] {
        mode.store(value, Ordering::SeqCst);
        assert!(!probe(&state, &proxy).await.success);
        assert_eq!(
            state.db.current_probe_health(&proxy).readiness_status,
            Readiness::NotReady
        );
    }
    mode.store(1, Ordering::SeqCst);
    assert!(probe(&state, &proxy).await.success);
    assert!(probe(&state, &proxy).await.success);
    let mut input: ProxyInput = serde_json::from_value(json!(proxy)).unwrap();
    input.test_url = Some("http://different-service.test/health".into());
    let changed = state.db.update_proxy(proxy.id, input).unwrap().0;
    assert_eq!(changed.probe_health.readiness_status, Readiness::Unknown);
    assert!(!changed.probe_health.fresh);
    assert!(state
        .proxy_runtime
        .readiness_candidates("any.test", "adaptive")
        .await
        .is_err());
    server.abort();
}

#[tokio::test]
async fn old_status_never_promotes_readiness_and_general_proxy_remains_routable() {
    for old_status in ["active", "inactive", "unknown"] {
        for required in [true, false] {
            let state = state();
            let (port, mode, _, server) = socks_fixture().await;
            let mut proxy = dedicated(&state, port, 1);
            if !required {
                let mut input: ProxyInput = serde_json::from_value(json!(proxy)).unwrap();
                input.health_policy.mode = HealthMode::TransportOnly;
                proxy = state.db.update_proxy(proxy.id, input).unwrap().0;
            }
            state
                .db
                .update_proxy_status(proxy.id, old_status, Some(329), 12, 4)
                .unwrap();
            mode.store(0, Ordering::SeqCst);
            assert!(!probe(&state, &proxy).await.success);
            let current = state.db.get_proxy(proxy.id).unwrap().unwrap();
            assert_eq!(current.status.as_deref(), Some(old_status));
            assert_eq!(current.success_count, 12);
            assert_eq!(current.fail_count, 4);
            assert_eq!(current.probe_health.probe_success_count, 0);
            assert_eq!(current.probe_health.probe_failure_count, 1);
            for algorithm in [
                "adaptive",
                "round_robin",
                "least_connections",
                "sticky_host",
            ] {
                let candidates = state
                    .proxy_runtime
                    .readiness_candidates("public.test", algorithm)
                    .await;
                if required {
                    assert!(candidates.is_err());
                } else {
                    assert_eq!(candidates.unwrap(), vec![proxy.id]);
                }
            }
            server.abort();
        }
    }
}

#[tokio::test]
async fn probe_uses_business_dns_mapping_and_original_http_host() {
    let state = state();
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let proxy = add(&state, listener.local_addr().unwrap().port());
    state
        .db
        .save_settings(
            json!({"test_url":"http://vpn.test/health?check=1"})
                .as_object()
                .unwrap(),
        )
        .unwrap();
    state
        .db
        .create_dns_mapping(crate::models::DnsInput {
            domain: "vpn.test".into(),
            ip: "10.11.12.13".into(),
            description: None,
            enabled: Some(1),
            dynamic: Some(0),
        })
        .unwrap();
    state.proxy_runtime.refresh_dns_cache().await.unwrap();
    let server = tokio::spawn(async move {
        let (mut io, _) = listener.accept().await.unwrap();
        let mut request = Vec::new();
        while !request.ends_with(b"\r\n\r\n") {
            request.push(io.read_u8().await.unwrap());
        }
        let request = String::from_utf8(request).unwrap().to_lowercase();
        assert!(request.starts_with("get http://10.11.12.13/health?check=1 "));
        assert!(request.contains("host: vpn.test:80\r\n"));
        io.write_all(b"HTTP/1.1 204 No Content\r\n\r\n")
            .await
            .unwrap();
    });
    assert!(probe(&state, &proxy).await.success);
    server.await.unwrap();
    let health = state.db.current_probe_health(&proxy);
    assert_eq!(
        health.last_probe_result.unwrap().probe_url,
        "http://vpn.test/health"
    );
    state
        .db
        .delete_dns_mapping(state.db.list_dns_mappings().unwrap()[0].id)
        .unwrap();
    state.proxy_runtime.refresh_dns_cache().await.unwrap();
    assert!(!state.db.current_probe_health(&proxy).fresh);
}

#[tokio::test]
async fn startup_always_validates_required_nodes_but_never_disabled_ones() {
    let state = state();
    let (port, _, count, server) = socks_fixture().await;
    let proxy = dedicated(&state, port, 2);
    let general = add(&state, if port == 19991 { 19990 } else { 19991 });
    let disabled = dedicated(&state, if port == 19992 { 19993 } else { 19992 }, 2);
    let mut input: ProxyInput = serde_json::from_value(json!(disabled)).unwrap();
    input.enabled = Some(0);
    state.db.update_proxy(disabled.id, input).unwrap();
    let mut schedule = state.probe_schedule().unwrap();
    schedule.startup_probe_enabled = false;
    state
        .run_startup_probe(&schedule, &mut HashMap::new())
        .await
        .unwrap();
    assert_eq!(count.load(Ordering::SeqCst), 1);
    assert_eq!(
        state
            .db
            .get_proxy(proxy.id)
            .unwrap()
            .unwrap()
            .probe_health
            .probe_success_count,
        1
    );
    assert_eq!(
        state.db.get_proxy(general.id).unwrap().unwrap().last_test,
        None
    );
    assert_eq!(
        state.db.get_proxy(disabled.id).unwrap().unwrap().last_test,
        None
    );
    server.abort();
}
