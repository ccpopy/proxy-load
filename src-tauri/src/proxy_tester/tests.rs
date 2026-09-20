use super::*;
use crate::models::ProxyInput;
use std::sync::{
    atomic::{AtomicUsize, Ordering},
    Mutex,
};
use tokio::{
    io::{AsyncRead, AsyncReadExt, AsyncWriteExt},
    net::{TcpListener, TcpStream},
    task::JoinHandle,
};
use tokio_rustls::TlsAcceptor;

fn proxy(port: u16, kind: &str) -> ProxyRecord {
    crate::database::Database::open_in_memory()
        .unwrap()
        .create_proxy(ProxyInput {
            name: "fixture".into(),
            proxy_type: kind.into(),
            host: "127.0.0.1".into(),
            port: port.into(),
            username: Some("probe-user".into()),
            password: (kind != "socks4").then(|| "secret-password".into()),
            enabled: Some(1),
            test_url: None,
            test_timeout: None,
            skip_cert_verify: None,
        })
        .unwrap()
}
async fn headers(io: &mut (impl AsyncRead + Unpin)) -> String {
    let mut bytes = Vec::new();
    while !bytes.ends_with(b"\r\n\r\n") {
        bytes.push(io.read_u8().await.unwrap());
        assert!(bytes.len() < 65536);
    }
    String::from_utf8(bytes).unwrap()
}
async fn zero_terminated(io: &mut TcpStream) -> Vec<u8> {
    let mut result = Vec::new();
    loop {
        let byte = io.read_u8().await.unwrap();
        if byte == 0 {
            return result;
        }
        result.push(byte);
    }
}
fn tls_pair() -> (Arc<rustls::ClientConfig>, Arc<rustls::ServerConfig>) {
    let rcgen::CertifiedKey { cert, key_pair } =
        rcgen::generate_simple_self_signed(vec!["probe.test".into(), "127.0.0.1".into()]).unwrap();
    let mut roots = rustls::RootCertStore::empty();
    roots.add(cert.der().clone()).unwrap();
    let client = probe_tls::build(false, roots);
    let mut server = rustls::ServerConfig::builder_with_provider(Arc::new(
        rustls::crypto::ring::default_provider(),
    ))
    .with_safe_default_protocol_versions()
    .unwrap()
    .with_no_client_auth()
    .with_single_cert(
        vec![cert.der().clone()],
        rustls::pki_types::PrivatePkcs8KeyDer::from(key_pair.serialize_der()).into(),
    )
    .unwrap();
    server.alpn_protocols = vec![b"http/1.1".to_vec()];
    (client, Arc::new(server))
}

async fn negotiate(
    stream: &mut TcpStream,
    kind: &str,
    tls: bool,
    request_log: &Mutex<Vec<String>>,
) {
    match kind {
        "socks5" => {
            let mut greeting = [0; 4];
            stream.read_exact(&mut greeting).await.unwrap();
            assert_eq!(greeting, [5, 2, 0, 2]);
            stream.write_all(&[5, 2]).await.unwrap();
            assert_eq!(stream.read_u8().await.unwrap(), 1);
            let len = stream.read_u8().await.unwrap();
            let mut user = vec![0; len as usize];
            stream.read_exact(&mut user).await.unwrap();
            let len = stream.read_u8().await.unwrap();
            let mut password = vec![0; len as usize];
            stream.read_exact(&mut password).await.unwrap();
            assert_eq!(user, b"probe-user");
            assert_eq!(password, b"secret-password");
            stream.write_all(&[1, 0]).await.unwrap();
            let mut header = [0; 4];
            stream.read_exact(&mut header).await.unwrap();
            assert_eq!(&header[..3], &[5, 1, 0]);
            let address = match header[3] {
                1 => {
                    let mut a = vec![0; 4];
                    stream.read_exact(&mut a).await.unwrap();
                    a
                }
                3 => {
                    let n = stream.read_u8().await.unwrap();
                    let mut a = vec![0; n as usize];
                    stream.read_exact(&mut a).await.unwrap();
                    a
                }
                _ => panic!("address"),
            };
            if header[3] == 3 {
                assert_eq!(address, b"probe.test");
            }
            let _port = stream.read_u16().await.unwrap();
            stream
                .write_all(&[5, 0, 0, 1, 0, 0, 0, 0, 0, 0])
                .await
                .unwrap();
        }
        "socks4" => {
            let mut header = [0; 8];
            stream.read_exact(&mut header).await.unwrap();
            assert_eq!(&header[..2], &[4, 1]);
            assert_eq!(zero_terminated(stream).await, b"probe-user");
            if header[4..8] == [0, 0, 0, 1] {
                assert_eq!(zero_terminated(stream).await, b"probe.test");
            }
            stream
                .write_all(&[0, 0x5a, 0, 0, 0, 0, 0, 0])
                .await
                .unwrap();
        }
        _ if tls => {
            let connect = headers(stream).await;
            assert!(connect.starts_with("CONNECT "));
            assert!(connect
                .to_lowercase()
                .contains("proxy-authorization: basic "));
            request_log.lock().unwrap().push(connect);
            stream
                .write_all(b"HTTP/1.1 200 Established\r\n\r\n")
                .await
                .unwrap();
        }
        _ => {}
    }
}

struct Mock {
    port: u16,
    accepts: Arc<AtomicUsize>,
    requests: Arc<Mutex<Vec<String>>>,
    task: JoinHandle<()>,
}
async fn mock(
    kind: &str,
    server_tls: Option<Arc<rustls::ServerConfig>>,
    response: Vec<u8>,
    fragment: bool,
) -> Mock {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();
    let accepts = Arc::new(AtomicUsize::new(0));
    let requests = Arc::new(Mutex::new(Vec::new()));
    let (counter, log, kind) = (accepts.clone(), requests.clone(), kind.to_string());
    let task = tokio::spawn(async move {
        let (mut stream, _) = listener.accept().await.unwrap();
        counter.fetch_add(1, Ordering::SeqCst);
        negotiate(&mut stream, &kind, server_tls.is_some(), &log).await;
        let mut io: BoxIo = if let Some(config) = server_tls {
            match TlsAcceptor::from(config).accept(stream).await {
                Ok(stream) => {
                    assert_eq!(stream.get_ref().1.alpn_protocol(), Some(&b"http/1.1"[..]));
                    Box::new(stream)
                }
                Err(error) => {
                    eprintln!("fixture TLS handshake failed: {error:?}");
                    return;
                }
            }
        } else {
            Box::new(stream)
        };
        let request = headers(&mut io).await;
        log.lock().unwrap().push(request);
        if fragment {
            for byte in &response {
                if io.write_all(&[*byte]).await.is_err() {
                    break;
                }
                tokio::task::yield_now().await;
            }
        } else {
            let _ = io.write_all(&response).await;
        }
        let _ = io.flush().await;
        // Keep the body open: probes must finish on final headers, not wait for a body/EOF.
        let mut b = [0];
        let _ = tokio::time::timeout(Duration::from_secs(2), io.read(&mut b)).await;
        assert!(
            tokio::time::timeout(Duration::from_millis(30), listener.accept())
                .await
                .is_err()
        );
    });
    Mock {
        port,
        accepts,
        requests,
        task,
    }
}
fn response(status: u16) -> Vec<u8> {
    format!("HTTP/1.1 {status} Fixture\r\nContent-Length: 900000000\r\n\r\n").into_bytes()
}

#[tokio::test]
#[ignore = "native SOCKS4a TCP acceptance; explicitly required in CI"]
async fn native_probe_socks4a_tcp_payload_baseline() {
    let payload = b"\x04\x01\x00\x50\x00\x00\x00\x01probe-user\x00probe.test\x00";
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let server = tokio::spawn(async move {
        let (mut stream, _) = listener.accept().await.unwrap();
        let mut bytes = Vec::new();
        while bytes.len() < payload.len() {
            if !matches!(
                tokio::time::timeout(Duration::from_millis(500), stream.read_buf(&mut bytes)).await,
                Ok(Ok(1..))
            ) {
                break;
            }
        }
        eprintln!(
            "native SOCKS4a sent={} received={}",
            payload.len(),
            bytes.len()
        );
        assert_eq!(bytes, payload);
    });
    let mut client = TcpStream::connect(address).await.unwrap();
    client.write_all(payload).await.unwrap();
    server.await.unwrap();
}

#[tokio::test]
async fn single_tcp_protocol_tls_matrix_preserves_remote_dns_and_auth_boundaries() {
    check_protocol_matrix(&["http", "https", "socks5"]).await;
}

#[tokio::test]
#[ignore = "native SOCKS4a TCP acceptance; explicitly required in CI"]
async fn native_probe_socks4a_single_tcp_matrix() {
    check_protocol_matrix(&["socks4"]).await;
}

async fn check_protocol_matrix(kinds: &[&str]) {
    for &kind in kinds {
        for tls in [false, true] {
            let (client, server) = tls_pair();
            let mock = mock(kind, tls.then_some(server), response(404), false).await;
            let p = proxy(mock.port, if kind == "https" { "http" } else { kind });
            let mut p = p;
            p.proxy_type = kind.into();
            let result = test_with_tls(
                &p,
                if tls {
                    "https://probe.test/a?q=private"
                } else {
                    "http://probe.test/a?q=private"
                },
                2000,
                client,
            )
            .await;
            assert!(result.success, "{kind} TLS={tls}: {result:?}");
            assert_eq!(result.status_code, Some(404));
            assert!(result.diagnostics.proxy_proven());
            mock.task.await.unwrap();
            assert_eq!(mock.accepts.load(Ordering::SeqCst), 1);
            let log = mock.requests.lock().unwrap();
            let get = log.last().unwrap();
            let forward = !tls && matches!(kind, "http" | "https");
            assert!(get.starts_with(if forward {
                "GET http://probe.test/a?q=private "
            } else {
                "GET /a?q=private "
            }));
            assert_eq!(get.to_lowercase().contains("proxy-authorization:"), forward);
            assert_eq!(
                result.diagnostics.final_origin.as_deref(),
                Some(if tls {
                    "https://probe.test"
                } else {
                    "http://probe.test"
                })
            );
        }
    }
}

#[tokio::test]
async fn forward_http_uses_get_not_connect_80_and_target_errors_do_not_penalize_proxy() {
    for status in [404, 503, 407] {
        let mock = mock("http", None, response(status), false).await;
        let result = test_proxy(&proxy(mock.port, "http"), "http://probe.test/", 1000).await;
        assert_eq!(result.success, status == 404);
        assert_eq!(
            result.failure_scope.as_deref(),
            match status {
                407 => Some("proxy"),
                503 => Some("target"),
                _ => None,
            }
        );
        mock.task.await.unwrap();
        assert!(mock.requests.lock().unwrap()[0].starts_with("GET http://"));
    }
}

#[tokio::test]
async fn target_tls_407_is_not_proxy_auth_and_certificate_policy_is_local() {
    for (skip, trusted, status) in [(false, false, 200), (true, false, 200), (false, true, 407)] {
        let (client, server) = tls_pair();
        let mock = mock("http", Some(server), response(status), false).await;
        let mut p = proxy(mock.port, "http");
        p.skip_cert_verify = i64::from(skip);
        let result = test_with_tls(
            &p,
            "https://probe.test/",
            1500,
            if trusted {
                client
            } else {
                probe_tls::config(skip)
            },
        )
        .await;
        if !skip && !trusted {
            assert_eq!(result.diagnostics.code, Some(Code::InvalidCertificate));
            assert_eq!(result.diagnostics.phase, Some(Phase::TargetTls));
        } else if status == 407 {
            assert_eq!(result.diagnostics.code, Some(Code::HttpStatus(407)));
        } else {
            assert!(result.success, "{result:?}");
        }
        assert!(result.diagnostics.proxy_proven());
        assert_ne!(result.failure_scope.as_deref(), Some("proxy"));
        mock.task.await.unwrap();
    }
    let (client, server) = tls_pair();
    let mock = mock("http", Some(server), response(200), false).await;
    assert!(
        test_with_tls(
            &proxy(mock.port, "http"),
            "https://127.0.0.1/",
            1000,
            client
        )
        .await
        .success
    );
    mock.task.await.unwrap();
}

#[tokio::test]
async fn informational_fragmented_oversized_and_unterminated_headers_are_bounded() {
    let mut informational =
        b"HTTP/1.1 100 Continue\r\n\r\nHTTP/1.1 103 Early Hints\r\nLink: </x>\r\n\r\n".to_vec();
    informational.extend(response(200));
    let mock = mock("http", None, informational, true).await;
    assert!(
        test_proxy(&proxy(mock.port, "http"), "http://probe.test/", 2000)
            .await
            .success
    );
    mock.task.await.unwrap();
    for payload in [
        format!(
            "HTTP/1.1 200 OK\r\nX: {}\r\n\r\n",
            "x".repeat(MAX_HEADERS + 1)
        )
        .into_bytes(),
        b"HTTP/1.1 200 OK\r\nX: unfinished".to_vec(),
    ] {
        let mock = self::mock("http", None, payload, false).await;
        let result = test_proxy(
            &proxy(mock.port, "http"),
            "http://probe.test/?secret=keep",
            150,
        )
        .await;
        assert!(!result.success);
        assert_eq!(result.failure_scope.as_deref(), Some("unknown"));
        assert!(!serde_json::to_string(&result)
            .unwrap()
            .contains("secret=keep"));
        assert!(!result.diagnostics.proxy_proven());
        mock.task.await.unwrap();
    }
}

#[tokio::test]
async fn connect_status_and_socks_evidence_are_typed() {
    for status in [407, 502] {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let p = proxy(listener.local_addr().unwrap().port(), "http");
        let server = tokio::spawn(async move {
            let (mut s, _) = listener.accept().await.unwrap();
            headers(&mut s).await;
            s.write_all(&response(status)).await.unwrap();
        });
        let result = test_proxy(&p, "https://probe.test/", 1000).await;
        assert_eq!(
            result.failure_scope.as_deref(),
            Some(if status == 407 { "proxy" } else { "target" })
        );
        server.await.unwrap();
    }
    for reply in [1, 2, 3, 4, 5, 6, 7, 8] {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let p = proxy(listener.local_addr().unwrap().port(), "socks5");
        let server = tokio::spawn(async move {
            let (mut s, _) = listener.accept().await.unwrap();
            let mut greeting = [0; 4];
            s.read_exact(&mut greeting).await.unwrap();
            s.write_all(&[5, 0]).await.unwrap();
            let mut header = [0; 5];
            s.read_exact(&mut header).await.unwrap();
            let mut target = vec![0; header[4] as usize + 2];
            s.read_exact(&mut target).await.unwrap();
            s.write_all(&[5, reply, 0, 1]).await.unwrap();
        });
        let result = test_proxy(&p, "http://probe.test/", 1000).await;
        assert_eq!(result.diagnostics.code, Some(Code::SocksReply(reply)));
        assert_eq!(
            result.failure_scope.as_deref(),
            Some(if reply == 1 || reply == 7 {
                "proxy"
            } else {
                "target"
            })
        );
        server.await.unwrap();
    }
}

#[tokio::test]
async fn cancellation_closes_socket_without_detached_drivers_at_each_boundary() {
    for phase in ["negotiation", "tls", "response"] {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let p = proxy(
            listener.local_addr().unwrap().port(),
            if phase == "negotiation" {
                "socks5"
            } else {
                "http"
            },
        );
        let (ready, signal) = tokio::sync::oneshot::channel();
        let server = tokio::spawn(async move {
            let (mut s, _) = listener.accept().await.unwrap();
            if phase != "negotiation" {
                headers(&mut s).await;
            }
            if phase == "tls" {
                s.write_all(b"HTTP/1.1 200 OK\r\n\r\n").await.unwrap();
            }
            ready.send(()).unwrap();
            let mut b = [0; 2048];
            loop {
                match tokio::time::timeout(Duration::from_secs(2), s.read(&mut b))
                    .await
                    .unwrap()
                {
                    Ok(0) | Err(_) => break,
                    _ => {}
                }
            }
        });
        let task = tokio::spawn(async move {
            test_proxy(
                &p,
                if phase == "tls" {
                    "https://probe.test/"
                } else {
                    "http://probe.test/"
                },
                10_000,
            )
            .await
        });
        signal.await.unwrap();
        task.abort();
        assert!(task.await.unwrap_err().is_cancelled());
        server.await.unwrap();
    }
}

#[tokio::test]
async fn redirects_share_deadline_and_rebuild_credentials_without_target_authorization() {
    for slow in [false, true] {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let p = proxy(listener.local_addr().unwrap().port(), "http");
        let counter = Arc::new(AtomicUsize::new(0));
        let count = counter.clone();
        let server = tokio::spawn(async move {
            loop {
                let Ok(Ok((mut s, _))) =
                    tokio::time::timeout(Duration::from_millis(400), listener.accept()).await
                else {
                    break;
                };
                count.fetch_add(1, Ordering::SeqCst);
                let request = headers(&mut s).await;
                assert!(!request.to_lowercase().contains("\r\nauthorization:"));
                if slow {
                    tokio::time::sleep(Duration::from_millis(80)).await;
                }
                let location = if request.contains("probe.test") {
                    "http://other.test/b"
                } else {
                    "/loop"
                };
                let _ = s.write_all(format!("HTTP/1.1 302 Found\r\nLocation: {location}\r\nContent-Length: 0\r\n\r\n").as_bytes()).await;
            }
        });
        let result = test_proxy(&p, "http://probe.test/a", if slow { 130 } else { 1500 }).await;
        assert!(!result.success);
        if slow {
            assert_eq!(result.diagnostics.code, Some(Code::Timeout));
            assert!(result.response_time < 300);
        } else {
            assert_eq!(result.diagnostics.code, Some(Code::RedirectLimit));
            assert_eq!(counter.load(Ordering::SeqCst), 6);
        }
        server.await.unwrap();
    }
}

#[tokio::test]
async fn prefixed_io_preserves_read_ahead_and_write_flush_shutdown() {
    let (io, mut peer) = tokio::io::duplex(64);
    let mut io = probe_transport::PrefixedIo {
        inner: io,
        prefix: b"prefix".to_vec(),
        position: 0,
    };
    peer.write_all(b"tail").await.unwrap();
    peer.shutdown().await.unwrap();
    let mut all = Vec::new();
    io.read_to_end(&mut all).await.unwrap();
    assert_eq!(all, b"prefixtail");
    io.write_all(b"request").await.unwrap();
    io.flush().await.unwrap();
    io.shutdown().await.unwrap();
    let mut all = Vec::new();
    peer.read_to_end(&mut all).await.unwrap();
    assert_eq!(all, b"request");
}

#[tokio::test]
async fn zero_budget_invalid_configuration_and_refused_tcp_never_become_target_success() {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let mut p = proxy(listener.local_addr().unwrap().port(), "http");
    drop(listener);
    let refused = test_proxy(&p, "http://probe.test/", 1000).await;
    assert_eq!(refused.failure_scope.as_deref(), Some("proxy"));
    assert_eq!(refused.diagnostics.phase, Some(Phase::TcpConnect));
    assert_eq!(
        test_proxy(&p, "http://probe.test/", 0)
            .await
            .diagnostics
            .code,
        Some(Code::Timeout)
    );
    p.port = 70000;
    assert_eq!(
        test_proxy(&p, "http://probe.test/", 1000)
            .await
            .failure_scope
            .as_deref(),
        Some("configuration")
    );
    let unsafe_url = test_proxy(&p, "https://user:password@probe.test/?secret=hidden", 1000).await;
    assert!(!serde_json::to_string(&unsafe_url)
        .unwrap()
        .contains("password"));
}

#[tokio::test]
#[ignore = "explicit single-chain probe timing and accept-count benchmark"]
async fn benchmark_probe_single_chain() {
    let mut cases = Vec::new();
    for round in 1..=5 {
        for kind in ["http", "socks5"] {
            for tls in [false, true] {
                let (client, server) = tls_pair();
                let mut samples = Vec::new();
                let mut failures = Vec::new();
                let mut accepts = 0;
                for iteration in 0..22 {
                    let mock = mock(kind, tls.then(|| server.clone()), response(200), false).await;
                    let result = test_with_tls(
                        &proxy(mock.port, kind),
                        if tls {
                            "https://probe.test/"
                        } else {
                            "http://probe.test/"
                        },
                        2000,
                        client.clone(),
                    )
                    .await;
                    mock.task.await.unwrap();
                    assert_eq!(mock.accepts.load(Ordering::SeqCst), 1);
                    if iteration >= 2 {
                        accepts += mock.accepts.load(Ordering::SeqCst);
                        if !result.success {
                            failures
                                .push(serde_json::json!({"iteration":iteration-2,"result":result}));
                        }
                        samples.push(result.diagnostics.timings);
                    }
                }
                cases.push(serde_json::json!({"round":round,"proxy":kind,"tls":tls,"warmup":2,"measured":20,"accepts":accepts,"failures":failures,"samples":samples}));
            }
        }
    }
    if let Ok(path) = std::env::var("PROXY_LOAD_PROBE_BENCH_OUTPUT") {
        std::fs::write(path, serde_json::to_vec_pretty(&serde_json::json!({"cases":cases,"socks4a":"not in timing matrix; run native_probe_ acceptance separately"})).unwrap()).unwrap();
    }
}

#[tokio::test]
#[ignore = "explicit raw CONNECT + rustls baseline without application probe transport/Hyper"]
async fn diagnostic_raw_connect_tls_baseline() {
    let (client, server) = tls_pair();
    let mut failed = 0;
    for iteration in 0..100 {
        let mut mock = mock("http", Some(server.clone()), response(200), false).await;
        let mut socket = tokio::time::timeout(
            Duration::from_secs(3),
            TcpStream::connect(("127.0.0.1", mock.port)),
        )
        .await
        .unwrap()
        .unwrap();
        socket.write_all(b"CONNECT probe.test:443 HTTP/1.1\r\nHost: probe.test:443\r\nProxy-Authorization: Basic cHJvYmUtdXNlcjpzZWNyZXQtcGFzc3dvcmQ=\r\n\r\n").await.unwrap();
        assert!(
            tokio::time::timeout(Duration::from_secs(3), headers(&mut socket))
                .await
                .unwrap()
                .starts_with("HTTP/1.1 200")
        );
        let outcome = tokio::time::timeout(Duration::from_secs(3), async {
            let mut tls = TlsConnector::from(client.clone())
                .connect(
                    rustls::pki_types::ServerName::try_from("probe.test").unwrap(),
                    socket,
                )
                .await?;
            tls.write_all(b"GET / HTTP/1.1\r\nHost: probe.test\r\nConnection: close\r\n\r\n")
                .await
                .unwrap();
            assert!(headers(&mut tls).await.starts_with("HTTP/1.1 200"));
            Ok::<(), std::io::Error>(())
        })
        .await;
        if !matches!(outcome, Ok(Ok(()))) {
            failed += 1;
            eprintln!("raw CONNECT/TLS iteration {iteration}: {outcome:?}");
        }
        if !matches!(
            tokio::time::timeout(Duration::from_secs(3), &mut mock.task).await,
            Ok(Ok(()))
        ) {
            mock.task.abort();
            failed += 1;
            eprintln!("raw CONNECT/TLS fixture {iteration} cleanup failed or timed out");
        }
    }
    eprintln!(
        "raw CONNECT/TLS failures={failed}/100; application probe transport and Hyper not used"
    );
    assert_eq!(failed, 0, "raw TLS transport baseline must also work");
}
