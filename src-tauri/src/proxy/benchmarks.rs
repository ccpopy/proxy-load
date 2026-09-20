//! Reproducible local mock benchmark, deliberately excluded from ordinary CI.
use super::routing_tests::{add_proxy, request, runtime};
use super::*;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use tokio::task::JoinSet;

fn percentiles(mut values: Vec<u64>) -> Value {
    if values.is_empty() {
        return json!(null);
    }
    values.sort_unstable();
    let at = |p: usize| values[(values.len() * p / 100).min(values.len() - 1)];
    json!({"samples": values.len(), "p50_us": at(50), "p95_us": at(95), "p99_us": at(99)})
}

fn resources() -> Value {
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        let script = format!("Get-Process -Id {} | Select-Object CPU,WorkingSet64,PeakWorkingSet64,HandleCount,@{{n='ThreadCount';e={{$_.Threads.Count}}}} | ConvertTo-Json -Compress", std::process::id());
        std::process::Command::new("pwsh")
            .args(["-NoProfile", "-Command", &script])
            .creation_flags(0x08000000)
            .output()
            .ok()
            .and_then(|o| serde_json::from_slice(&o.stdout).ok())
            .unwrap_or(Value::Null)
    }
    #[cfg(not(windows))]
    {
        let output = std::process::Command::new("ps")
            .args(["-o", "rss=,time=", "-p", &std::process::id().to_string()])
            .output()
            .ok();
        json!({"ps_rss_kib_cpu": output.map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string()), "open_fds": std::fs::read_dir("/proc/self/fd").ok().map(|r| r.count())})
    }
}

struct MockTask(Arc<AtomicUsize>);
impl Drop for MockTask {
    fn drop(&mut self) {
        self.0.fetch_sub(1, Ordering::Relaxed);
    }
}

fn selected(value: impl ToString, variable: &str) -> bool {
    std::env::var(variable).map_or(true, |filter| {
        filter.split(',').any(|v| v == value.to_string())
    })
}

struct BenchmarkDatabase(Option<std::path::PathBuf>);
impl Drop for BenchmarkDatabase {
    fn drop(&mut self) {
        if let Some(path) = &self.0 {
            // Delete only this test's known files, never a recursive directory.
            for suffix in ["", "-wal", "-shm"] {
                let _ = std::fs::remove_file(format!("{}{suffix}", path.display()));
            }
        }
    }
}

fn benchmark_runtime(case: usize) -> (BenchmarkDatabase, Arc<ProxyRuntime>, usize) {
    if std::env::var("PROXY_LOAD_BENCH_DB").as_deref() != Ok("disk") {
        return (BenchmarkDatabase(None), runtime(), 0);
    }
    let path = std::env::temp_dir().join(format!(
        "proxy-load-bench-{}-{}-{case}.db",
        std::process::id(),
        now_millis()
    ));
    let history = std::env::var("PROXY_LOAD_BENCH_HISTORY")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(100_000);
    let db = Database::open_test_file(path.clone(), history).unwrap();
    let (events, _) = broadcast::channel(16);
    let mut config = crate::database::default_advanced_config();
    config.insert("circuit_failure_threshold".into(), json!(1));
    (
        BenchmarkDatabase(Some(path)),
        Arc::new(ProxyRuntime::new(db, events, "127.0.0.1", 0, &json!(config)).unwrap()),
        history,
    )
}

async fn mock(
    index: usize,
    scenario: &'static str,
    active: Arc<AtomicUsize>,
    peak: Arc<AtomicUsize>,
) -> (u16, tokio::task::JoinHandle<()>) {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();
    let task = tokio::spawn(async move {
        let mut clients = JoinSet::new();
        loop {
            tokio::select! {
                result = listener.accept() => {
                    let Ok((mut stream, _)) = result else { break; };
                    let current = active.fetch_add(1, Ordering::Relaxed) + 1;
                    peak.fetch_max(current, Ordering::Relaxed);
                    let counter = MockTask(active.clone());
                    clients.spawn(async move {
                        let _counter = counter;
                        if scenario == "slow_auth" {
                            let result: std::io::Result<()> = async {
                                let mut greeting = [0; 4];
                                stream.read_exact(&mut greeting).await?;
                                stream.write_all(&[5, 2]).await?;
                                let mut auth = [0; 11]; // test user/pass, never real credentials
                                stream.read_exact(&mut auth).await?;
                                tokio::time::sleep(Duration::from_millis(if index == 0 { 50 } else { 1 })).await;
                                stream.write_all(&[1, 0]).await?;
                                let mut header = [0; 5];
                                stream.read_exact(&mut header).await?;
                                let mut address = vec![0; usize::from(header[4]) + 2];
                                stream.read_exact(&mut address).await?;
                                stream.write_all(&[5, 0, 0, 1, 127, 0, 0, 1, 0, 80]).await?;
                                let mut buffer = [0; 128];
                                while stream.read(&mut buffer).await? > 0 {}
                                Ok(())
                            }.await;
                            let _ = result;
                            return;
                        }
                        let Ok((header, _)) = read_http_request_header(&mut stream, Vec::new(), 1000).await else { return; };
                        if index == 0 && scenario == "timeout" {
                            tokio::time::sleep(Duration::from_secs(2)).await;
                            return;
                        }
                        let status = if index == 0 && scenario == "auth_failure" { 407 }
                            else if index == 0 && scenario == "target_failure" && header.contains("blocked.test") { 502 }
                            else { 200 };
                        if stream.write_all(format!("HTTP/1.1 {status} Mock\r\n\r\n").as_bytes()).await.is_ok() {
                            let mut buffer = [0; 128];
                            while stream.read(&mut buffer).await.is_ok_and(|n| n > 0) {}
                        }
                    });
                }
                _ = clients.join_next(), if !clients.is_empty() => {}
            }
        }
    });
    (port, task)
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore = "90-case loopback benchmark; run explicitly with --ignored --nocapture"]
async fn benchmark_loopback_matrix() {
    let started = Instant::now();
    let before = resources();
    let mut cases = Vec::new();
    for nodes in [3, 10, 100] {
        if !selected(nodes, "PROXY_LOAD_BENCH_NODES") {
            continue;
        }
        for concurrency in [1usize, 32, 256] {
            if !selected(concurrency, "PROXY_LOAD_BENCH_CONCURRENCY") {
                continue;
            }
            for scenario in [
                "short",
                "long",
                "mixed",
                "auth_failure",
                "timeout",
                "target_failure",
                "slow_auth",
                "queue_full",
                "global_half_open",
                "target_half_open",
            ] {
                if !selected(scenario, "PROXY_LOAD_BENCH_SCENARIOS") {
                    continue;
                }
                let (_database_files, runtime, history_rows) = benchmark_runtime(cases.len());
                {
                    let mut settings = runtime
                        .runtime_settings
                        .write()
                        .unwrap_or_else(|e| e.into_inner());
                    settings.fail_fast.attempt_timeout_ms = 500;
                    settings.fail_fast.total_timeout_ms = 5000;
                }
                let mut servers = Vec::new();
                let active_mock_tasks = Arc::new(AtomicUsize::new(0));
                let peak_mock_tasks = Arc::new(AtomicUsize::new(0));
                for index in 0..nodes {
                    let (port, server) = mock(
                        index,
                        scenario,
                        active_mock_tasks.clone(),
                        peak_mock_tasks.clone(),
                    )
                    .await;
                    let proxy = add_proxy(&runtime, port);
                    if scenario == "slow_auth" {
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
                    }
                    servers.push(server);
                }
                let pause = if scenario == "queue_full" {
                    let pause = runtime.database_worker.pause_for_test();
                    for _ in 0..4096 {
                        runtime.database_worker.log(RequestLogEntry {
                            proxy_id: None,
                            target_host: "queue-fixture",
                            target_port: 80,
                            success: false,
                            response_time: None,
                            error_message: None,
                            result_type: "proxy_exhausted",
                        });
                    }
                    Some(pause)
                } else {
                    None
                };
                if matches!(scenario, "global_half_open" | "target_half_open") {
                    let mut config = runtime.runtime_settings.write().unwrap();
                    config.algorithm = "round_robin".into();
                    let mut breaker = CircuitBreaker::new(config.circuit);
                    breaker.record_failure();
                    breaker.next_attempt = 0;
                    drop(config);
                    if scenario == "global_half_open" {
                        runtime.circuit_breakers.write().unwrap().insert(1, breaker);
                    } else {
                        runtime.target_circuits.write().unwrap().insert(
                            TargetRouteKey::new(1, &request("blocked.test")),
                            TargetCircuit {
                                breaker,
                                last_failure: monotonic_millis(),
                            },
                        );
                    }
                }
                let done = Arc::new(AtomicBool::new(false));
                let lag = tokio::spawn({
                    let done = done.clone();
                    async move {
                        let mut samples = Vec::new();
                        while !done.load(Ordering::Relaxed) {
                            let timer = Instant::now();
                            tokio::time::sleep(Duration::from_millis(1)).await;
                            samples.push(timer.elapsed().as_micros().saturating_sub(1000) as u64);
                        }
                        samples
                    }
                });
                let observer = tokio::task::spawn_blocking({
                    let done = done.clone();
                    let db = runtime.db.clone();
                    move || {
                        let mut updates = 0;
                        while !done.load(Ordering::Relaxed) {
                            db.overview(0).unwrap();
                            // Same backend reads used by the visible charts/log page.
                            db.traffic_logs(1, 25, None).unwrap();
                            if updates % 8 == 0 {
                                db.update_proxy_priority(1, updates).unwrap();
                            }
                            updates += 1;
                            std::thread::sleep(Duration::from_millis(2));
                        }
                        updates
                    }
                });
                let total = concurrency.max(32) * 2;
                let mut next = 0;
                let mut clients = JoinSet::new();
                let mut results = Vec::new();
                loop {
                    while next < total && clients.len() < concurrency {
                        let index = next;
                        next += 1;
                        let runtime = runtime.clone();
                        clients.spawn(async move {
                            let req = request(if index % 2 == 0 {
                                "blocked.test"
                            } else {
                                "healthy.test"
                            });
                            let start = Instant::now();
                            let result = connect_with_fail_fast(runtime.clone(), &req, start).await;
                            let elapsed = start.elapsed().as_micros() as u64;
                            let winner = result.as_ref().ok().map(|(lease, _)| lease.id);
                            if scenario == "long" || scenario == "mixed" && index % 4 == 0 {
                                tokio::time::sleep(Duration::from_millis(100)).await;
                            }
                            runtime
                                .record_request(RequestLogEntry {
                                    proxy_id: winner,
                                    target_host: &req.original_host,
                                    target_port: 443,
                                    success: winner.is_some(),
                                    response_time: Some((elapsed / 1000) as i64),
                                    error_message: None,
                                    result_type: "benchmark",
                                })
                                .await;
                            drop(result);
                            (index, elapsed, winner)
                        });
                    }
                    let Some(result) = clients.join_next().await else {
                        break;
                    };
                    results.push(result.unwrap());
                }
                done.store(true, Ordering::Relaxed);
                let lag = lag.await.unwrap();
                let updates = observer.await.unwrap();
                drop(pause);
                assert!(runtime.flush_logs(Duration::from_secs(5)));
                assert!(runtime.active_connections.lock().unwrap().is_empty());
                let mut distribution = HashMap::<i64, usize>::new();
                for (_, _, id) in &results {
                    if let Some(id) = id {
                        *distribution.entry(*id).or_default() += 1;
                    }
                }
                let successful = results.iter().filter(|r| r.2.is_some()).count();
                let case = json!({"nodes": nodes, "concurrency": concurrency, "scenario": scenario, "requests": total, "successful": successful, "reachability": successful as f64 / total as f64, "cold_first_half": percentiles(results.iter().filter(|r| r.0 < total/2).map(|r| r.1).collect()), "warm_second_half": percentiles(results.iter().filter(|r| r.0 >= total/2).map(|r| r.1).collect()), "event_loop_lag": percentiles(lag), "node_distribution": distribution, "db_and_timings": runtime.database_stats(), "statistics_polls": updates, "driver_task_bound": concurrency + nodes + 2, "peak_mock_tasks": peak_mock_tasks.load(Ordering::Relaxed), "active_leases_after": 0});
                eprintln!("BENCH nodes={nodes} concurrency={concurrency} scenario={scenario} success={successful}/{total}");
                let mut case = case;
                case["history_rows"] = json!(history_rows);
                cases.push(case);
                for server in servers {
                    server.abort();
                    let _ = server.await;
                }
            }
        }
    }
    let report = json!({"profile": if cfg!(debug_assertions) { "debug" } else { "release" }, "database": std::env::var("PROXY_LOAD_BENCH_DB").unwrap_or_else(|_| "memory".into()), "platform": std::env::consts::OS, "arch": std::env::consts::ARCH, "seed": 0, "elapsed_ms": started.elapsed().as_millis(), "resources_before": before, "resources_after": resources(), "cases": cases});
    if let Ok(path) = std::env::var("PROXY_LOAD_BENCH_OUTPUT") {
        std::fs::write(path, serde_json::to_vec_pretty(&report).unwrap()).unwrap();
    }
    eprintln!(
        "BENCH completed cases={} elapsed_ms={}",
        cases.len(),
        started.elapsed().as_millis()
    );
}
