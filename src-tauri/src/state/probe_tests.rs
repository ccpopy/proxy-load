use super::*;
use crate::models::ProxyInput;
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::TcpListener,
};

fn state() -> AppState {
    let db = Database::open_in_memory().unwrap();
    let (events, _) = broadcast::channel(32);
    let runtime = Arc::new(
        ProxyRuntime::new(
            db.clone(),
            events.clone(),
            "127.0.0.1",
            0,
            &json!(crate::database::default_advanced_config()),
        )
        .unwrap(),
    );
    AppState {
        db,
        events,
        proxy_runtime: runtime,
        proxy_host: "127.0.0.1".into(),
        proxy_port: 0,
        started_at: now_millis(),
        started_monotonic: proxy::monotonic_millis(),
        probe_locks: Default::default(),
        probe_failures: Default::default(),
        forced_probes: Default::default(),
        settings_update_lock: Default::default(),
        probe_notify: Default::default(),
        dns_notify: Default::default(),
        probe_slots: Arc::new(Semaphore::new(64)),
        probe_results: Default::default(),
    }
}
fn add(state: &AppState, port: u16) -> ProxyRecord {
    state
        .db
        .create_proxy(ProxyInput {
            name: "probe-state".into(),
            proxy_type: "http".into(),
            host: "127.0.0.1".into(),
            port: port.into(),
            username: None,
            password: None,
            enabled: Some(1),
            test_url: None,
            test_timeout: None,
            skip_cert_verify: None,
        })
        .unwrap()
}
async fn header(stream: &mut tokio::net::TcpStream) {
    let mut data = Vec::new();
    while !data.ends_with(b"\r\n\r\n") {
        data.push(stream.read_u8().await.unwrap());
    }
}

#[tokio::test]
async fn concurrent_manual_and_periodic_probes_share_one_completed_observation() {
    let state = state();
    state
        .db
        .save_settings(
            json!({"test_url":"http://probe.test/"})
                .as_object()
                .unwrap(),
        )
        .unwrap();
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let proxy = add(&state, listener.local_addr().unwrap().port());
    let ready = Arc::new(Notify::new());
    let release = Arc::new(Notify::new());
    let server = tokio::spawn({
        let ready = ready.clone();
        let release = release.clone();
        async move {
            let (mut stream, _) = listener.accept().await.unwrap();
            header(&mut stream).await;
            ready.notify_one();
            release.notified().await;
            stream
                .write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 0\r\n\r\n")
                .await
                .unwrap();
            assert!(time::timeout(Duration::from_millis(150), listener.accept())
                .await
                .is_err());
        }
    });
    let periodic = tokio::spawn({
        let state = state.clone();
        let proxy = proxy.clone();
        async move { state.test_proxy_record(proxy, ProbeOrigin::Periodic).await }
    });
    ready.notified().await;
    let manual = tokio::spawn({
        let state = state.clone();
        let proxy = proxy.clone();
        async move { state.test_proxy_record(proxy, ProbeOrigin::Manual).await }
    });
    time::sleep(Duration::from_millis(20)).await;
    release.notify_one();
    assert!(periodic.await.unwrap().unwrap().unwrap().success);
    assert!(manual.await.unwrap().unwrap().unwrap().success);
    server.await.unwrap();
    assert_eq!(
        state.db.get_proxy(proxy.id).unwrap().unwrap().success_count,
        1
    );
    assert_eq!(state.probe_slots.available_permits(), 64);
}

#[tokio::test]
async fn unknown_does_not_clear_auth_failures_but_target_503_preserves_proxy_evidence() {
    for (bytes, proven) in [
        (b"".as_slice(), false), // Unclassified EOF before any response evidence.
        (
            b"HTTP/1.1 503 Unavailable\r\nContent-Length: 0\r\n\r\n".as_slice(),
            true,
        ),
    ] {
        let state = state();
        state
            .db
            .save_settings(
                json!({"test_url":"http://probe.test/"})
                    .as_object()
                    .unwrap(),
            )
            .unwrap();
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let proxy = add(&state, listener.local_addr().unwrap().port());
        state.probe_failures.lock().await.insert(proxy.id, 1);
        let server = tokio::spawn(async move {
            let (mut stream, _) = listener.accept().await.unwrap();
            header(&mut stream).await;
            stream.write_all(bytes).await.unwrap();
        });
        let result = state
            .test_proxy_record(proxy.clone(), ProbeOrigin::Periodic)
            .await
            .unwrap()
            .unwrap();
        server.await.unwrap();
        assert!(!result.success);
        assert_eq!(result.diagnostics.proxy_proven(), proven);
        let updated = state.db.get_proxy(proxy.id).unwrap().unwrap();
        assert_eq!(updated.fail_count, 0);
        assert_eq!(updated.success_count, i64::from(proven));
        assert_eq!(
            state.probe_failures.lock().await.contains_key(&proxy.id),
            !proven
        );
    }
}

#[tokio::test]
async fn changed_test_settings_or_shutdown_discard_inflight_probe_without_health_writes() {
    for action in ["change", "aba", "stop"] {
        let state = state();
        state
            .db
            .save_settings(
                json!({"test_url":"http://probe.test/"})
                    .as_object()
                    .unwrap(),
            )
            .unwrap();
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let proxy = add(&state, listener.local_addr().unwrap().port());
        let ready = Arc::new(Notify::new());
        let release = Arc::new(Notify::new());
        let server = tokio::spawn({
            let ready = ready.clone();
            let release = release.clone();
            async move {
                let (mut stream, _) = listener.accept().await.unwrap();
                header(&mut stream).await;
                ready.notify_one();
                release.notified().await;
                let _ = stream
                    .write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 0\r\n\r\n")
                    .await;
            }
        });
        let task = tokio::spawn({
            let state = state.clone();
            let proxy = proxy.clone();
            async move { state.test_proxy_record(proxy, ProbeOrigin::Manual).await }
        });
        ready.notified().await;
        if action == "stop" {
            state.proxy_runtime.request_stop();
        } else {
            state
                .db
                .save_settings(
                    json!({"test_url":"http://changed.test/"})
                        .as_object()
                        .unwrap(),
                )
                .unwrap();
            if action == "aba" {
                state
                    .db
                    .save_settings(
                        json!({"test_url":"http://probe.test/"})
                            .as_object()
                            .unwrap(),
                    )
                    .unwrap();
            }
        }
        release.notify_one();
        assert!(task.await.unwrap().is_err());
        server.await.unwrap();
        let updated = state.db.get_proxy(proxy.id).unwrap().unwrap();
        assert_eq!(updated.success_count, 0);
        assert_eq!(updated.fail_count, 0);
        assert_eq!(state.probe_slots.available_permits(), 64);
        assert!(state.probe_results.lock().await.is_empty());
    }
}

#[tokio::test]
async fn queue_cancellation_releases_node_lock_without_recording_a_failure() {
    let state = state();
    let proxy = add(&state, 19999);
    let permits = state
        .probe_slots
        .clone()
        .acquire_many_owned(64)
        .await
        .unwrap();
    let task = tokio::spawn({
        let state = state.clone();
        let proxy = proxy.clone();
        async move { state.test_proxy_record(proxy, ProbeOrigin::Manual).await }
    });
    time::sleep(Duration::from_millis(20)).await;
    state.proxy_runtime.request_stop();
    assert!(task.await.unwrap().is_err());
    drop(permits);
    assert_eq!(state.probe_slots.available_permits(), 64);
    assert_eq!(state.db.get_proxy(proxy.id).unwrap().unwrap().fail_count, 0);
    assert!(state
        .probe_locks
        .lock()
        .await
        .get(&proxy.id)
        .unwrap()
        .try_lock()
        .is_ok());
}
