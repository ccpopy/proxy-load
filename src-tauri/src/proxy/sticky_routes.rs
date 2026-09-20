use super::{monotonic_millis, TargetRequest};
use crate::{
    models::ProxyRecord,
    routing::{self, RoutingSnapshot},
};
use std::collections::HashMap;

const MAX_BINDINGS: usize = 4096;

#[derive(Clone, Hash, PartialEq, Eq)]
struct Key {
    pool: i64,
    host: String,
    resolved: String,
    port: u16,
}

#[derive(Clone)]
pub(super) struct Policy {
    key: Key,
    revision: u64,
    ttl_ms: i64,
    pub preferred: i64,
}

struct Binding {
    proxy: i64,
    generation: u64,
    revision: u64,
    expires: i64,
}

#[derive(Default)]
pub(super) struct StickyRoutes(HashMap<Key, Binding>);

impl Policy {
    pub(super) fn for_request(
        snapshot: &RoutingSnapshot,
        request: &TargetRequest,
        global_algorithm: &str,
    ) -> Option<Self> {
        let pool = snapshot.pool(&request.original_host)?;
        if pool
            .algorithm_override
            .as_deref()
            .unwrap_or(global_algorithm)
            != "sticky_host"
            || pool.sticky_failover_seconds <= 0
        {
            return None;
        }
        let host = routing::normalize_host(&request.original_host);
        let preferred = snapshot
            .proxies
            .iter()
            .filter(|p| pool.members.contains(&p.id))
            .max_by_key(|p| (routing::affinity(pool.id, &host, p.id), p.id))?
            .id;
        Some(Self {
            key: Key {
                pool: pool.id,
                host,
                resolved: routing::normalize_host(&request.host),
                port: request.port,
            },
            revision: pool.revision,
            ttl_ms: pool.sticky_failover_seconds * 1000,
            preferred,
        })
    }
}

impl StickyRoutes {
    pub(super) fn prefer(
        &mut self,
        policy: &Policy,
        snapshot: &RoutingSnapshot,
        candidates: &mut [ProxyRecord],
    ) {
        let Some(binding) = self.0.get(&policy.key) else {
            return;
        };
        let position = candidates
            .iter()
            .position(|p| p.id == binding.proxy && p.status.as_deref() != Some("inactive"));
        if binding.expires <= monotonic_millis()
            || binding.revision != policy.revision
            || snapshot.node_generations.get(&binding.proxy) != Some(&binding.generation)
            || position.is_none()
        {
            self.0.remove(&policy.key);
            return;
        }
        candidates.swap(0, position.unwrap());
    }

    pub(super) fn remember(
        &mut self,
        policy: &Policy,
        snapshot: &RoutingSnapshot,
        proxy: i64,
        generation: u64,
    ) {
        if proxy == policy.preferred
            || snapshot.node_generations.get(&proxy) != Some(&generation)
            || !snapshot
                .pools
                .iter()
                .any(|p| p.id == policy.key.pool && p.revision == policy.revision)
        {
            return;
        }
        let now = monotonic_millis();
        // A busy site must not renew the binding forever. TTL starts at failover,
        // not at every successful request through the backup.
        if self
            .0
            .get(&policy.key)
            .is_some_and(|b| b.proxy == proxy && b.revision == policy.revision && b.expires > now)
        {
            return;
        }
        self.0.retain(|_, b| b.expires > now);
        if self.0.len() >= MAX_BINDINGS {
            if let Some(oldest) = self
                .0
                .iter()
                .min_by_key(|(_, b)| b.expires)
                .map(|(key, _)| key.clone())
            {
                self.0.remove(&oldest);
            }
        }
        self.0.insert(
            policy.key.clone(),
            Binding {
                proxy,
                generation,
                revision: policy.revision,
                expires: now.saturating_add(policy.ttl_ms),
            },
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn binding_cache_is_bounded_and_success_does_not_extend_ttl() {
        let runtime = super::super::routing_tests::runtime();
        let a = super::super::routing_tests::add_proxy(&runtime, 19001);
        let b = super::super::routing_tests::add_proxy(&runtime, 19002);
        let group = runtime
            .db
            .create_proxy_group(crate::models::ProxyGroupInput {
                name: Some("test".into()),
                domains: Some(vec!["*".into()]),
                proxy_ids: Some(vec![a.id, b.id]),
                algorithm_override: Some(Some("sticky_host".into())),
                sticky_failover_seconds: Some(60),
                ..Default::default()
            })
            .unwrap();
        let snapshot = runtime.db.routing_snapshot();
        let mut cache = StickyRoutes::default();
        for i in 0..MAX_BINDINGS + 20 {
            let request = super::super::routing_tests::request(&format!("host{i}.test"));
            let policy = Policy::for_request(&snapshot, &request, "adaptive").unwrap();
            let backup = if policy.preferred == a.id { b.id } else { a.id };
            cache.remember(
                &policy,
                &snapshot,
                backup,
                snapshot.node_generations[&backup],
            );
            let expires = cache.0[&policy.key].expires;
            cache.remember(
                &policy,
                &snapshot,
                backup,
                snapshot.node_generations[&backup],
            );
            assert_eq!(cache.0[&policy.key].expires, expires);
            assert!(cache.0.len() <= MAX_BINDINGS);
        }
        assert_eq!(cache.0.len(), MAX_BINDINGS);
        assert_eq!(snapshot.pools[0].id, group.id);
    }
}
