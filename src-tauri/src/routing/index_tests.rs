use super::*;
use crate::{database::Database, models::ProxyInput};
use serde_json::json;
use std::{hint::black_box, time::Instant};

fn nodes(count: usize) -> Vec<ProxyRecord> {
    let db = Database::open_in_memory().unwrap();
    let base = db
        .create_proxy(ProxyInput {
            name: "index".into(),
            proxy_type: "http".into(),
            host: "127.0.0.1".into(),
            port: 9999,
            username: None,
            password: None,
            enabled: Some(1),
            test_url: None,
            test_timeout: None,
            health_policy: Default::default(),
            skip_cert_verify: None,
        })
        .unwrap();
    (0..count)
        .map(|i| ProxyRecord {
            id: i as i64 + 1,
            ..base.clone()
        })
        .collect()
}
fn pool(id: i64, rules: &[&str], members: &[i64], default: bool) -> RoutingPool {
    RoutingPool {
        id,
        name: id.to_string(),
        is_default: default,
        members: members.iter().copied().collect(),
        rules: rules.iter().map(|r| normalize_rule(r)).collect(),
        algorithm_override: None,
        sticky_failover_seconds: 0,
        revision: 1,
    }
}
#[test]
fn indexed_rules_preserve_legacy_rank_ties_idna_empty_and_default_pools() {
    let snapshot = RoutingSnapshot::new(
        1,
        nodes(4),
        HashMap::new(),
        vec![
            pool(
                1,
                &["example.com", "*.org", "*.bücher.example"],
                &[1, 2],
                false,
            ),
            pool(
                2,
                &["*.example.com", "api.example.com", "*.org"],
                &[3],
                false,
            ),
            pool(3, &["api.example.com", "empty.test"], &[], false),
            pool(4, &[], &[4], true),
        ],
        &RoutingSnapshot::default(),
    );
    for host in [
        "example.com",
        "API.EXAMPLE.COM.",
        "a.b.example.com",
        "empty.test",
        "no-match.test",
        "BÜCHER.Example.",
        "api.bücher.example",
        "org",
        "api.org",
        "evil-example.com",
        "notexample.com",
        "127.0.0.1",
    ] {
        assert_eq!(
            snapshot.pool(host).map(|p| p.id),
            snapshot.linear_pool(host).map(|p| p.id),
            "{host}"
        );
    }
    assert_eq!(snapshot.pool("example.com").unwrap().id, 2); // wildcard root historically outranks exact root
    assert_eq!(snapshot.pool("api.example.com").unwrap().id, 2); // first tie wins
    assert!(RouteContext::new(&snapshot, "empty.test", "adaptive")
        .members
        .is_empty());
    assert_eq!(
        RouteContext::new(&snapshot, "none.test", "adaptive").members,
        &[3]
    );
    for i in 0..10000 {
        let host = format!("x{i}.sub{}.example.com", i % 13);
        assert_eq!(
            snapshot.pool(&host).map(|p| p.id),
            snapshot.linear_pool(&host).map(|p| p.id)
        );
    }
    let catch_all = RoutingSnapshot::new(
        2,
        nodes(4),
        HashMap::new(),
        vec![pool(1, &["*"], &[], false), pool(2, &["*"], &[1], true)],
        &snapshot,
    );
    assert_eq!(catch_all.pool("none.test").unwrap().id, 1);
    let global = RoutingSnapshot::new(
        1,
        nodes(4),
        HashMap::new(),
        vec![],
        &RoutingSnapshot::default(),
    );
    assert_eq!(
        RouteContext::new(&global, "none.test", "adaptive").members,
        &[0, 1, 2, 3]
    );
}

#[test]
fn health_publication_reuses_static_index_and_member_changes_rebuild_it() {
    let first = RoutingSnapshot::new(
        1,
        nodes(3),
        HashMap::new(),
        vec![pool(1, &["*.test"], &[3, 1], false)],
        &RoutingSnapshot::default(),
    );
    let mut changed = nodes(3);
    changed[0].status = Some("inactive".into());
    let second = RoutingSnapshot::new(
        2,
        changed,
        HashMap::new(),
        vec![pool(1, &["*.test"], &[3, 1], false)],
        &first,
    );
    assert!(Arc::ptr_eq(&first.index, &second.index));
    assert_eq!(
        RouteContext::new(&second, "x.test", "adaptive").members,
        &[0, 2]
    );
    let third = RoutingSnapshot::new(
        3,
        nodes(3),
        HashMap::new(),
        vec![pool(1, &["*.test"], &[2], false)],
        &second,
    );
    assert!(!Arc::ptr_eq(&second.index, &third.index));
    assert_eq!(
        RouteContext::new(&third, "x.test", "adaptive").members,
        &[1]
    );
    assert_eq!(
        RouteContext::new(&first, "x.test", "adaptive").members,
        &[0, 2]
    );
}

#[test]
#[ignore = "explicit release-mode domain/member index benchmark"]
fn benchmark_routing_index_matrix() {
    let mut cases = Vec::new();
    for node_count in [3, 10, 100] {
        for rules in [10, 1000, 10000] {
            for round in 1..=5 {
                let groups = node_count.min(30);
                let mut pools: Vec<_> = (0..groups)
                    .map(|i| pool(i as i64 + 1, &[], &[i as i64 + 1], false))
                    .collect();
                for i in 0..rules {
                    pools[i % groups].rules.push(format!("*.site{i}.test"));
                }
                let proxies = nodes(node_count);
                let build = Instant::now();
                let snapshot = RoutingSnapshot::new(
                    1,
                    proxies,
                    HashMap::new(),
                    pools,
                    &RoutingSnapshot::default(),
                );
                let build_us = build.elapsed().as_micros() as u64;
                let hosts: Vec<_> = (0..1000)
                    .map(|i| format!("api.site{}.test", i * 7919 % rules))
                    .collect();
                for host in &hosts[..100] {
                    black_box(snapshot.pool(host));
                    black_box(snapshot.linear_pool(host));
                }
                let measure = |indexed: bool| {
                    let mut samples = Vec::new();
                    for host in &hosts {
                        let start = Instant::now();
                        let selected = if indexed {
                            snapshot.pool(black_box(host))
                        } else {
                            snapshot.linear_pool(black_box(host))
                        };
                        black_box(selected);
                        samples.push(start.elapsed().as_nanos() as u64);
                    }
                    samples.sort_unstable();
                    json!({"p50_ns":samples[500], "p95_ns":samples[950], "p99_ns":samples[990], "samples":1000})
                };
                let (before, after) = if round % 2 == 0 {
                    let after = measure(true);
                    (measure(false), after)
                } else {
                    (measure(false), measure(true))
                };
                cases.push(json!({"nodes":node_count,"groups":groups,"rules":rules,"round":round,"index_build_us":build_us,"linear":before,"indexed":after}));
            }
        }
    }
    if let Ok(path) = std::env::var("PROXY_LOAD_INDEX_BENCH_OUTPUT") {
        std::fs::write(
            path,
            serde_json::to_vec_pretty(&json!({"warmup_per_case":100,"cases":cases})).unwrap(),
        )
        .unwrap();
    }
}
