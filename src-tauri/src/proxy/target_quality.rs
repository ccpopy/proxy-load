//! Optional target-only evidence. O(log N) eviction, bounded across the whole runtime.
use anyhow::{anyhow, Result};
use serde_json::Value;
use std::collections::{BTreeMap, HashMap};

pub(super) const MAX_ENTRIES: usize = 4096;
const TTL_MS: i64 = 300_000;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Mode {
    Off,
    Observe,
    Adaptive,
}
impl Mode {
    pub(crate) fn from_advanced(config: &Value) -> Result<Self> {
        match config.get("target_quality_mode") {
            None => Ok(Self::Off),
            Some(Value::String(value)) => match value.as_str() {
                "off" => Ok(Self::Off),
                "observe" => Ok(Self::Observe),
                "adaptive" => Ok(Self::Adaptive),
                _ => Err(anyhow!("目标质量模式必须是 off、observe 或 adaptive")),
            },
            _ => Err(anyhow!("目标质量模式必须是字符串")),
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub(super) enum Measurement {
    SocksTargetConnect,
    HttpConnectResponse,
}

#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub(super) struct Key {
    pub proxy: i64,
    pub generation: u64,
    pub host: String,
    pub destination: String,
    pub port: u16,
    pub measurement: Measurement,
}

struct Sample {
    successes: f64,
    failures: f64,
    latency_us: Option<f64>,
    observed: i64,
    last_exploration: i64,
    order: (i64, u64),
}
#[derive(Default)]
pub(super) struct TargetQuality {
    entries: HashMap<Key, Sample>,
    expiry: BTreeMap<(i64, u64), Key>,
    sequence: u64,
}
impl TargetQuality {
    pub fn len(&self) -> usize {
        self.entries.len()
    }
    pub fn clear(&mut self) {
        self.entries.clear();
        self.expiry.clear();
    }
    fn prune(&mut self, now: i64) {
        while self
            .expiry
            .first_key_value()
            .is_some_and(|((observed, _), _)| now.saturating_sub(*observed) >= TTL_MS)
        {
            let (_, key) = self.expiry.pop_first().unwrap();
            self.entries.remove(&key);
        }
    }
    pub fn record(&mut self, key: Key, success: bool, latency_us: Option<u64>, now: i64) {
        self.prune(now);
        self.sequence = self.sequence.wrapping_add(1);
        let order = (now, self.sequence);
        let mut sample = self.entries.remove(&key).unwrap_or(Sample {
            successes: 0.0,
            failures: 0.0,
            latency_us: None,
            observed: now,
            last_exploration: now,
            order,
        });
        self.expiry.remove(&sample.order);
        let decay = 2f64.powf(-(now.saturating_sub(sample.observed).max(0) as f64) / TTL_MS as f64);
        sample.successes *= decay;
        sample.failures *= decay;
        if sample.successes + sample.failures >= 64.0 {
            sample.successes *= 0.5;
            sample.failures *= 0.5;
        }
        if success {
            sample.successes += 1.0;
            if let Some(us) = latency_us {
                let us = us.min(300_000_000) as f64;
                sample.latency_us = Some(sample.latency_us.map_or(us, |old| 0.8 * old + 0.2 * us));
            }
        } else {
            sample.failures += 1.0;
        }
        sample.observed = now;
        sample.order = order;
        while self.entries.len() >= MAX_ENTRIES {
            let (_, oldest) = self.expiry.pop_first().unwrap();
            self.entries.remove(&oldest);
        }
        self.expiry.insert(order, key.clone());
        self.entries.insert(key, sample);
    }
    pub fn multiplier(&self, key: &Key, now: i64) -> f64 {
        let Some(sample) = self.entries.get(key) else {
            return 1.0;
        };
        let age = now.saturating_sub(sample.observed).max(0);
        if age >= TTL_MS {
            return 1.0;
        }
        let freshness = 1.0 - age as f64 / TTL_MS as f64;
        let n = sample.successes + sample.failures;
        let confidence = (n / (n + 8.0)).min(0.8) * freshness;
        let probability = (sample.successes + 1.0) / (n + 2.0);
        let latency = sample
            .latency_us
            .map_or(1.0, |us| 1.0 / (1.0 + us / 500_000.0));
        (1.0 - confidence + confidence * probability * latency).clamp(0.2, 1.0)
    }
    pub fn exploration_due(&self, key: &Key, now: i64) -> bool {
        self.entries.get(key).is_some_and(|sample| {
            now.saturating_sub(sample.observed.max(sample.last_exploration)) >= 30_000
        })
    }
    pub fn reserved(&mut self, key: &Key, now: i64) {
        if let Some(sample) = self.entries.get_mut(key) {
            sample.last_exploration = now;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn key(host: &str) -> Key {
        Key {
            proxy: 1,
            generation: 1,
            host: host.into(),
            destination: "127.0.0.1".into(),
            port: 443,
            measurement: Measurement::HttpConnectResponse,
        }
    }
    #[test]
    fn target_quality_is_scoped_bounded_expires_and_rewards_faster_reliable_samples() {
        let mut slow = TargetQuality::default();
        let mut fast = TargetQuality::default();
        let mut failed = TargetQuality::default();
        for _ in 0..20 {
            slow.record(key("x"), true, Some(900_000), 1000);
            fast.record(key("x"), true, Some(10_000), 1000);
            failed.record(key("x"), false, None, 1000);
        }
        assert!(fast.multiplier(&key("x"), 1000) > slow.multiplier(&key("x"), 1000));
        assert!(fast.multiplier(&key("x"), 1000) > failed.multiplier(&key("x"), 1000));
        assert_eq!(slow.multiplier(&key("y"), 1000), 1.0);
        let mut changed = key("x");
        changed.generation += 1;
        assert_eq!(slow.multiplier(&changed, 1000), 1.0);
        changed = key("x");
        changed.destination = "127.0.0.2".into();
        assert_eq!(slow.multiplier(&changed, 1000), 1.0);
        assert_eq!(slow.multiplier(&key("x"), 301_000), 1.0);
        for i in 0..10000 {
            slow.record(key(&i.to_string()), true, Some(0), 1000 + i);
        }
        assert_eq!(slow.len(), MAX_ENTRIES);
        assert_eq!(slow.expiry.len(), MAX_ENTRIES);
        for i in 0..10000 {
            slow.record(key("repeat"), true, Some(0), 12000 + i);
        }
        assert!(slow.len() <= MAX_ENTRIES);
        assert_eq!(slow.len(), slow.expiry.len());
        slow.clear();
        assert_eq!(slow.multiplier(&key("repeat"), 22000), 1.0);
    }

    #[test]
    fn slow_route_recovery_is_probed_at_bounded_intervals() {
        let mut quality = TargetQuality::default();
        quality.record(key("slow"), true, Some(1_000_000), 1000);
        assert!(!quality.exploration_due(&key("slow"), 30_999));
        assert!(quality.exploration_due(&key("slow"), 31_000));
        quality.reserved(&key("slow"), 31_000);
        assert!(!quality.exploration_due(&key("slow"), 31_001));
        assert!(quality.exploration_due(&key("slow"), 61_000));
        let old = quality.multiplier(&key("slow"), 61_000);
        for _ in 0..20 {
            quality.record(key("slow"), true, Some(1000), 61_000);
        }
        assert!(quality.multiplier(&key("slow"), 61_000) > old);
    }
}
