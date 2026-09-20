use anyhow::{anyhow, Result};
use serde::Serialize;
use serde_json::Value;

/// Immutable for the lifetime of a runtime; saving settings never replaces live semaphores.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub struct ConcurrencyLimits {
    pub max_connections: usize,
    pub max_handshakes: usize,
    pub max_global_dials: usize,
    pub max_proxy_dials: usize,
}
impl ConcurrencyLimits {
    pub fn from_advanced(config: &Value) -> Result<Self> {
        let read = |key, default, maximum| -> Result<usize> {
            let value = match config.get(key) {
                None => default,
                Some(value) => value
                    .as_u64()
                    .ok_or_else(|| anyhow!("{key} 必须是正整数"))?,
            };
            if value == 0 || value > maximum {
                return Err(anyhow!("{key} 必须在 1 到 {maximum} 之间"));
            }
            Ok(value as usize)
        };
        let limits = Self {
            max_connections: read("max_connections", 1024, 16384)?,
            max_handshakes: read("max_handshakes", 128, 4096)?,
            max_global_dials: read("max_global_dials", 64, 2048)?,
            max_proxy_dials: read("max_proxy_dials", 32, 512)?,
        };
        if limits.max_proxy_dials > limits.max_global_dials
            || limits.max_global_dials > limits.max_connections
            || limits.max_handshakes > limits.max_connections
        {
            return Err(anyhow!(
                "并发限制须满足：单节点拨号 ≤ 全局业务拨号 ≤ 业务连接，入站握手 ≤ 业务连接"
            ));
        }
        Ok(limits)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    #[test]
    fn old_configs_keep_defaults_and_invalid_limits_are_rejected_atomically() {
        let defaults = ConcurrencyLimits::from_advanced(&json!({})).unwrap();
        assert_eq!(
            defaults,
            ConcurrencyLimits {
                max_connections: 1024,
                max_handshakes: 128,
                max_global_dials: 64,
                max_proxy_dials: 32
            }
        );
        for invalid in [
            json!({"max_connections":0}),
            json!({"max_handshakes":2.5}),
            json!({"max_global_dials":true}),
            json!({"max_proxy_dials":513}),
            json!({"max_proxy_dials":65}),
            json!({"max_connections":64}),
            json!({"max_connections":null}),
        ] {
            assert!(ConcurrencyLimits::from_advanced(&invalid).is_err());
        }
    }
}
