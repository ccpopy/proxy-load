//! Native TCP acceptance tests: cancellation boundaries and streaming, without public internet.
use super::routing_tests::{add_proxy, request, runtime};
use super::*;

#[tokio::test]
async fn small_connection_handshake_limits_bound_bursts_and_shutdown_reclaims_permits() {
    let db = Database::open_in_memory().unwrap();
    let (events, _) = broadcast::channel(16);
    let mut config = crate::database::default_advanced_config();
    for (key, value) in [
        ("max_connections", 2),
        ("max_handshakes", 1),
        ("max_global_dials", 1),
        ("max_proxy_dials", 1),
    ] {
        config.insert(key.into(), json!(value));
    }
    let runtime = Arc::new(ProxyRuntime::new(db, events, "127.0.0.1", 0, &json!(config)).unwrap());
    let probe = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = probe.local_addr().unwrap();
    drop(probe);
    let server = tokio::spawn(serve(runtime.clone(), "127.0.0.1".into(), address.port()));
    timeout(Duration::from_secs(2), async {
        while !runtime.service_status().await.running {
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap();
    let mut clients = Vec::new();
    for _ in 0..8 {
        let mut stream = TcpStream::connect(address).await.unwrap();
        stream.write_all(b"G").await.unwrap();
        clients.push(stream);
    }
    timeout(Duration::from_secs(2), async {
        while runtime.connection_slots.available_permits() > 0
            || runtime.handshake_slots.available_permits() > 0
        {
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap();
    assert_eq!(runtime.global_dial_slots.available_permits(), 1);
    runtime.request_stop();
    timeout(Duration::from_secs(2), server)
        .await
        .unwrap()
        .unwrap()
        .unwrap();
    assert_eq!(runtime.connection_slots.available_permits(), 2);
    assert_eq!(runtime.handshake_slots.available_permits(), 1);
    drop(clients);
}

#[tokio::test]
async fn shutdown_stops_admission_cancels_clients_and_releases_the_listener() {
    let runtime = runtime();
    let probe = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = probe.local_addr().unwrap();
    drop(probe);
    let service = tokio::spawn(serve(runtime.clone(), "127.0.0.1".into(), address.port()));
    timeout(Duration::from_secs(2), async {
        while !runtime.service_status().await.running {
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap();
    let mut client = TcpStream::connect(address).await.unwrap();
    client.write_all(b"G").await.unwrap();
    tokio::task::yield_now().await;
    runtime.request_stop();
    timeout(Duration::from_secs(2), service)
        .await
        .unwrap()
        .unwrap()
        .unwrap();
    assert_eq!(runtime.connection_slots.available_permits(), 1024);
    assert_eq!(runtime.handshake_slots.available_permits(), 128);
    assert!(TcpListener::bind(address).await.is_ok());
    assert!(runtime.stop_and_flush_logs(Duration::from_secs(2)));
}

async fn pair() -> (TcpStream, TcpStream, SocketAddr) {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let client = TcpStream::connect(listener.local_addr().unwrap())
        .await
        .unwrap();
    let (server, address) = listener.accept().await.unwrap();
    (client, server, address)
}

#[tokio::test]
#[ignore = "native TCP half-close environment diagnostic; also run explicitly in CI"]
async fn native_tcp_half_close_baseline() {
    let (mut client, mut server, _) = pair().await;
    let remote = tokio::spawn(async move {
        let mut data = Vec::new();
        server.read_to_end(&mut data).await.unwrap();
        server.write_all(b"after-eof").await.unwrap();
        server.shutdown().await.unwrap();
    });
    client.write_all(b"upload").await.unwrap();
    client.shutdown().await.unwrap();
    let mut data = Vec::new();
    timeout(Duration::from_secs(5), client.read_to_end(&mut data))
        .await
        .expect("native TCP half-close baseline timed out")
        .unwrap();
    timeout(Duration::from_secs(5), remote)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(data, b"after-eof");
}

fn released(runtime: &ProxyRuntime, id: i64) {
    assert!(runtime.active_connections.lock().unwrap().is_empty());
    assert_eq!(
        runtime.dial_slots.lock().unwrap()[&id].available_permits(),
        32
    );
    assert!(runtime.selection_lock.try_lock().is_ok());
    assert_eq!(runtime.global_dial_slots.available_permits(), 64);
}

#[tokio::test]
async fn cancellation_during_socks_auth_releases_capacity_without_global_failure() {
    let runtime = runtime();
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let proxy = add_proxy(&runtime, listener.local_addr().unwrap().port());
    runtime
        .db
        .update_proxy(
            proxy.id,
            crate::models::ProxyInput {
                name: proxy.name.clone(),
                proxy_type: "socks5".into(),
                host: proxy.host.clone(),
                port: proxy.port,
                username: Some("user".into()),
                password: Some("pass".into()),
                enabled: Some(1),
                test_url: None,
                test_timeout: None,
                skip_cert_verify: None,
            },
        )
        .unwrap();
    let (ready, waiting) = tokio::sync::oneshot::channel();
    let server = tokio::spawn(async move {
        let (mut stream, _) = listener.accept().await.unwrap();
        let mut greeting = [0; 4];
        stream.read_exact(&mut greeting).await.unwrap();
        stream.write_all(&[5, 2]).await.unwrap();
        let mut auth = [0; 11];
        stream.read_exact(&mut auth).await.unwrap();
        ready.send(()).unwrap();
        std::future::pending::<()>().await;
    });
    let task = tokio::spawn({
        let runtime = runtime.clone();
        async move { connect_with_fail_fast(runtime, &request("example.com"), Instant::now()).await }
    });
    timeout(Duration::from_secs(3), waiting)
        .await
        .unwrap()
        .unwrap();
    task.abort();
    assert!(task.await.err().unwrap().is_cancelled());
    released(&runtime, proxy.id);
    assert_eq!(
        runtime.circuit_breakers.read().unwrap()[&proxy.id].state,
        "CLOSED"
    );
    server.abort();
    let _ = server.await;
}

#[tokio::test]
async fn cancellation_during_success_delivery_and_transfer_releases_the_lease() {
    for delivery in [true, false] {
        let runtime = runtime();
        let proxy = add_proxy(&runtime, 19001);
        let lease = runtime
            .reserve_proxy(&request("example.com"), &HashSet::new())
            .await
            .unwrap()
            .unwrap();
        let (mut client, mut accepted, _) = pair().await;
        let (upstream_socket, _remote, _) = pair().await;
        let task = tokio::spawn(async move {
            let _lease = lease;
            let mut upstream = ConnectedUpstream {
                stream: upstream_socket,
                outbound_initial_payload: None,
                prefetched_response: if delivery {
                    vec![7; 16 * 1024 * 1024]
                } else {
                    Vec::new()
                },
                target_verified: true,
                proxy_latency_us: 1,
            };
            complete_client_handshake(&mut accepted, &mut upstream, &request("example.com"))
                .await
                .unwrap();
            io::copy_bidirectional(&mut accepted, &mut upstream.stream).await
        });
        let mut header = [0; 39];
        timeout(Duration::from_secs(3), client.read_exact(&mut header))
            .await
            .unwrap()
            .unwrap();
        assert_eq!(&header, b"HTTP/1.1 200 Connection Established\r\n\r\n");
        assert!(!task.is_finished());
        task.abort();
        assert!(task.await.unwrap_err().is_cancelled());
        released(&runtime, proxy.id);
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore = "native half-close acceptance; this host also fails the bare TCP baseline"]
async fn native_half_close_streams_large_download_past_connection_deadline() {
    native_streaming_download(true).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn native_streams_large_download_past_connection_deadline() {
    native_streaming_download(false).await;
}

async fn native_streaming_download(half_close: bool) {
    let runtime = runtime();
    runtime
        .runtime_settings
        .write()
        .unwrap()
        .fail_fast
        .total_timeout_ms = 2000;
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let proxy = add_proxy(&runtime, listener.local_addr().unwrap().port());
    const SIZE: usize = 4 * 1024 * 1024;
    let server = tokio::spawn(async move {
        let (mut remote, _) = listener.accept().await.unwrap();
        let (_, mut uploaded) = read_http_request_header(&mut remote, Vec::new(), 2000)
            .await
            .unwrap();
        remote
            .write_all(b"HTTP/1.1 200 OK\r\n\r\nprefetched")
            .await
            .unwrap();
        if half_close {
            remote.read_to_end(&mut uploaded).await.unwrap();
        } else {
            let mut remaining = vec![0; 65536 - uploaded.len()];
            remote.read_exact(&mut remaining).await.unwrap();
            uploaded.extend_from_slice(&remaining);
        }
        assert_eq!(uploaded, vec![3; 65536]);
        // The established tunnel must outlive its connection deadline.
        tokio::time::sleep(Duration::from_millis(2100)).await;
        let block = [5; 32768];
        for _ in 0..SIZE / block.len() {
            remote.write_all(&block).await.unwrap();
        }
        remote.shutdown().await.unwrap();
    });
    let (mut client, accepted, address) = pair().await;
    let service = tokio::spawn({
        let runtime = runtime.clone();
        async move { handle_client(runtime, accepted, address).await }
    });
    client
        .write_all(b"CONNECT example.com:443 HTTP/1.1\r\nHost: example.com\r\n\r\n")
        .await
        .unwrap();
    let (_, prefetched) = read_http_request_header(&mut client, Vec::new(), 3000)
        .await
        .unwrap();
    let (mut reader, mut writer) = client.into_split();
    writer.write_all(&vec![3; 65536]).await.unwrap();
    if half_close {
        writer.shutdown().await.unwrap();
    }
    let mut downloaded = prefetched;
    timeout(Duration::from_secs(15), reader.read_to_end(&mut downloaded))
        .await
        .unwrap()
        .unwrap();
    if !half_close {
        writer.shutdown().await.unwrap();
    }
    timeout(Duration::from_secs(5), server)
        .await
        .unwrap()
        .unwrap();
    timeout(Duration::from_secs(5), service)
        .await
        .unwrap()
        .unwrap()
        .unwrap();
    assert_eq!(&downloaded[..10], b"prefetched");
    assert_eq!(downloaded.len(), 10 + SIZE);
    assert!(downloaded[10..].iter().all(|byte| *byte == 5));
    released(&runtime, proxy.id);
    assert!(runtime.flush_logs(Duration::from_secs(2)));
    let (logs, _) = runtime.db.traffic_logs(1, 25, None).unwrap();
    let transfer = logs
        .iter()
        .find(|log| log.result_type.as_deref() == Some("transfer_finished"))
        .unwrap();
    let detail: Value = serde_json::from_str(transfer.error_message.as_deref().unwrap()).unwrap();
    assert_eq!(detail["bytesSent"], 65536);
    assert_eq!(detail["bytesReceived"], 10 + SIZE);
}
