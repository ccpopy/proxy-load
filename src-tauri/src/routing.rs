use std::{
    collections::{HashMap, HashSet},
    sync::Arc,
};

use crate::models::ProxyRecord;

/// Immutable, atomically published configuration. Runtime routing never queries SQLite.
#[derive(Default)]
pub struct RoutingSnapshot {
    pub generation: u64,
    pub proxies: Vec<ProxyRecord>,
    pub node_generations: HashMap<i64, u64>,
    pub pools: Vec<RoutingPool>,
    index: Arc<RoutingIndex>,
}

#[derive(Default)]
struct RoutingIndex {
    nodes_by_id: HashMap<i64, usize>,
    global: Vec<usize>,
    members: Vec<Vec<usize>>,
    exact: HashMap<String, RankedPool>,
    suffix: HashMap<String, RankedPool>,
    catch_all: Option<RankedPool>,
    default_pool: Option<usize>,
}

#[derive(Clone, Copy, Debug)]
struct RankedPool {
    pool: usize,
    specificity: usize,
    order: usize,
}
fn preferred(a: RankedPool, b: RankedPool) -> RankedPool {
    if (a.specificity, std::cmp::Reverse(a.order)) >= (b.specificity, std::cmp::Reverse(b.order)) {
        a
    } else {
        b
    }
}

pub struct RouteContext<'a> {
    pub snapshot: &'a RoutingSnapshot,
    pub host: String,
    pub pool: Option<&'a RoutingPool>,
    pub members: &'a [usize],
    pub algorithm: String,
}
impl<'a> RouteContext<'a> {
    pub fn new(snapshot: &'a RoutingSnapshot, host: &str, global_algorithm: &str) -> Self {
        let host = normalize_host(host);
        let pool_index = snapshot.pool_index(&host);
        let pool = pool_index.map(|i| &snapshot.pools[i]);
        Self {
            snapshot,
            host,
            pool,
            members: pool_index.map_or(&snapshot.index.global, |i| &snapshot.index.members[i]),
            algorithm: pool
                .and_then(|p| p.algorithm_override.as_deref())
                .unwrap_or(global_algorithm)
                .to_string(),
        }
    }
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
    pub fn new(
        generation: u64,
        proxies: Vec<ProxyRecord>,
        node_generations: HashMap<i64, u64>,
        pools: Vec<RoutingPool>,
        previous: &Self,
    ) -> Self {
        let same_index = proxies.len() == previous.proxies.len()
            && proxies
                .iter()
                .zip(&previous.proxies)
                .all(|(a, b)| a.id == b.id)
            && pools.len() == previous.pools.len()
            && pools.iter().zip(&previous.pools).all(|(a, b)| {
                a.id == b.id
                    && a.members == b.members
                    && a.rules == b.rules
                    && a.is_default == b.is_default
            });
        let index = if same_index {
            previous.index.clone()
        } else {
            Arc::new(RoutingIndex::build(&proxies, &pools))
        };
        Self {
            generation,
            proxies,
            node_generations,
            pools,
            index,
        }
    }
    pub fn proxy(&self, id: i64) -> Option<&ProxyRecord> {
        self.index.nodes_by_id.get(&id).map(|i| &self.proxies[*i])
    }
    #[cfg(test)]
    pub fn pool(&self, host: &str) -> Option<&RoutingPool> {
        let host = normalize_host(host);
        self.pool_index(&host).map(|i| &self.pools[i])
    }
    fn pool_index(&self, host: &str) -> Option<usize> {
        let mut best = self.index.catch_all;
        let mut consider = |candidate: Option<&RankedPool>| {
            if let Some(candidate) = candidate {
                best = Some(best.map_or(*candidate, |old| preferred(old, *candidate)));
            }
        };
        consider(self.index.exact.get(host));
        consider(self.index.suffix.get(host));
        for (i, _) in host.match_indices('.') {
            consider(self.index.suffix.get(&host[i + 1..]));
        }
        best.map(|candidate| candidate.pool)
            .or(self.index.default_pool)
    }
    #[cfg(test)]
    fn linear_pool(&self, host: &str) -> Option<&RoutingPool> {
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

impl RoutingIndex {
    fn build(proxies: &[ProxyRecord], pools: &[RoutingPool]) -> Self {
        let mut index = Self {
            nodes_by_id: proxies.iter().enumerate().map(|(i, p)| (p.id, i)).collect(),
            global: (0..proxies.len()).collect(),
            ..Default::default()
        };
        let mut order = 0;
        for (pool_index, pool) in pools.iter().enumerate() {
            let mut members: Vec<_> = pool
                .members
                .iter()
                .filter_map(|id| index.nodes_by_id.get(id).copied())
                .collect();
            members.sort_unstable(); // Preserve the old snapshot/priority/ID traversal order.
            index.members.push(members);
            if pool.is_default {
                index.default_pool = Some(pool_index);
            }
            for rule in &pool.rules {
                let rank = RankedPool {
                    pool: pool_index,
                    specificity: rule.trim_start_matches('*').len(),
                    order,
                };
                order += 1;
                if rule == "*" {
                    index.catch_all =
                        Some(index.catch_all.map_or(rank, |old| preferred(old, rank)));
                } else {
                    let (map, key) = match rule.strip_prefix("*.") {
                        Some(suffix) => (&mut index.suffix, suffix),
                        None => (&mut index.exact, rule.as_str()),
                    };
                    map.entry(key.to_string())
                        .and_modify(|old| *old = preferred(*old, rank))
                        .or_insert(rank);
                }
            }
        }
        index
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

#[cfg(test)]
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

#[cfg(test)]
mod index_tests;
