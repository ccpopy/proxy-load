use std::collections::{HashMap, HashSet};

use crate::models::ProxyRecord;

/// Immutable, atomically published configuration. Runtime routing never queries SQLite.
#[derive(Default)]
pub struct RoutingSnapshot {
    pub generation: u64,
    pub proxies: Vec<ProxyRecord>,
    pub node_generations: HashMap<i64, u64>,
    pub pools: Vec<RoutingPool>,
}

pub struct RoutingPool {
    pub id: i64,
    pub name: String,
    pub is_default: bool,
    pub members: HashSet<i64>,
    pub rules: Vec<String>,
    pub algorithm_override: Option<String>,
    pub sticky_failover_seconds: i64,
    pub revision: u64,
}

pub fn normalize_algorithm(value: Option<&str>) -> anyhow::Result<Option<String>> {
    match value.map(str::trim).filter(|v| !v.is_empty()) {
        None => Ok(None),
        Some("adaptive" | "round_robin" | "least_connections" | "sticky_host") => {
            Ok(value.map(str::trim).map(str::to_string))
        }
        Some("weighted_round_robin") => Ok(Some("round_robin".into())),
        Some(_) => anyhow::bail!("不支持的分组负载算法"),
    }
}

pub fn validate_hold(seconds: i64) -> anyhow::Result<i64> {
    if !(0..=86400).contains(&seconds) {
        anyhow::bail!("故障切换保持期必须在 0 到 86400 秒之间");
    }
    Ok(seconds)
}

impl RoutingSnapshot {
    pub fn pool(&self, host: &str) -> Option<&RoutingPool> {
        let host = normalize_host(host);
        let mut selected = None;
        let mut specificity = 0;
        let mut fallback = None;
        for pool in &self.pools {
            if pool.is_default {
                fallback = Some(pool);
            }
            for rule in &pool.rules {
                let length = rule.trim_start_matches('*').len();
                if domain_matches(&host, rule) && (selected.is_none() || length > specificity) {
                    selected = Some(pool);
                    specificity = length;
                }
            }
        }
        selected.or(fallback)
    }
}

pub fn normalize_host(host: &str) -> String {
    let host = host.trim().trim_end_matches('.');
    url::Host::parse(host)
        .map(|host| host.to_string().to_ascii_lowercase())
        .unwrap_or_else(|_| host.to_ascii_lowercase())
}

pub fn normalize_rule(rule: &str) -> String {
    if rule == "*" {
        return rule.into();
    }
    match rule.strip_prefix("*.") {
        Some(host) => format!("*.{}", normalize_host(host)),
        None => normalize_host(rule),
    }
}

pub fn domain_matches(host: &str, rule: &str) -> bool {
    if rule == "*" {
        return true;
    }
    match rule.strip_prefix("*.") {
        Some(suffix) => {
            host == suffix
                || host
                    .strip_suffix(suffix)
                    .is_some_and(|prefix| prefix.ends_with('.'))
        }
        None => host == rule,
    }
}

pub fn same_node(left: &ProxyRecord, right: &ProxyRecord) -> bool {
    left.proxy_type == right.proxy_type
        && left.host == right.host
        && left.port == right.port
        && left.username == right.username
        && left.password == right.password
        && left.enabled == right.enabled
        && left.test_url == right.test_url
        && left.test_timeout == right.test_timeout
        && left.skip_cert_verify == right.skip_cert_verify
}

/// FNV-1a followed by a fixed avalanche; no process-random state, priority or array index.
pub fn affinity(pool: i64, host: &str, proxy: i64) -> u64 {
    let mut hash = 0xcbf29ce484222325u64;
    for byte in pool
        .to_le_bytes()
        .into_iter()
        .chain(host.bytes())
        .chain([0])
        .chain(proxy.to_le_bytes())
    {
        hash = (hash ^ u64::from(byte)).wrapping_mul(0x100000001b3);
    }
    hash ^= hash >> 33;
    hash = hash.wrapping_mul(0xff51afd7ed558ccd);
    hash ^= hash >> 33;
    hash = hash.wrapping_mul(0xc4ceb9fe1a85ec53);
    hash ^ (hash >> 33)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn host_identity_includes_idna_case_and_trailing_dot() {
        assert_eq!(normalize_host("BÜCHER.Example."), "xn--bcher-kva.example");
        assert!(domain_matches(
            &normalize_host("API.Example.COM."),
            &normalize_rule("*.EXAMPLE.com.")
        ));
    }
    #[test]
    fn rendezvous_only_moves_keys_to_or_from_changed_member() {
        for i in 0..10000 {
            let host = format!("host{i}.test");
            let winner = |ids: &[i64]| {
                *ids.iter()
                    .max_by_key(|id| affinity(1, &host, **id))
                    .unwrap()
            };
            let old = winner(&[1, 2, 3]);
            let removed = winner(&[1, 3]);
            if old != 2 {
                assert_eq!(old, removed);
            }
            let added = winner(&[4, 3, 2, 1]);
            assert!(added == 4 || added == old);
        }
    }
}
