//! Probe observations are facts; readiness is a separate, explicit routing policy.
use crate::{models::TestResult, proxy::failure::FailureScope};
use serde::{Deserialize, Serialize};

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum HealthMode {
    #[default]
    TransportOnly,
    RequiredProbe,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct HealthPolicy {
    pub mode: HealthMode,
    pub failure_threshold: u32,
    pub recovery_threshold: u32,
    pub expected_statuses: Vec<u16>,
    pub max_age_seconds: u64,
}
impl Default for HealthPolicy {
    fn default() -> Self {
        Self {
            mode: HealthMode::TransportOnly,
            failure_threshold: 2,
            recovery_threshold: 2,
            expected_statuses: vec![200, 204],
            max_age_seconds: 600,
        }
    }
}
impl HealthPolicy {
    pub fn required(&self) -> bool {
        self.mode == HealthMode::RequiredProbe
    }
    pub fn validate(&self) -> anyhow::Result<()> {
        if !(30..=86400).contains(&self.max_age_seconds) {
            anyhow::bail!("业务就绪有效期必须在 30 到 86400 秒之间");
        }
        if !(1..=10).contains(&self.failure_threshold)
            || !(1..=10).contains(&self.recovery_threshold)
        {
            anyhow::bail!("业务就绪的失败 / 恢复阈值必须在 1 到 10 之间");
        }
        if self.expected_statuses.is_empty()
            || self.expected_statuses.len() > 20
            || self
                .expected_statuses
                .iter()
                .any(|s| !(200..=499).contains(s) || (300..400).contains(s) || *s == 407)
        {
            anyhow::bail!(
                "预期状态码应为 200–299 或 400–499（不含 407），最多 20 个；业务测活不接受重定向"
            );
        }
        Ok(())
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Readiness {
    #[default]
    Unknown,
    Ready,
    Degraded,
    NotReady,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ProbeObservation {
    pub outcome: String,
    pub observed_at: i64,
    pub probe_url: String,
    pub url_source: String,
    pub response_time: i64,
    pub status_code: Option<u16>,
    // Preserve the typed diagnostic's serialized facts, never raw credentials/errors.
    pub diagnostics: serde_json::Value,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(default)]
pub struct ProbeHealth {
    pub transport_status: String,
    pub readiness_status: Readiness,
    pub last_probe_result: Option<ProbeObservation>,
    pub probe_success_count: u64,
    pub probe_failure_count: u64,
    pub probe_excluded_count: u64,
    pub statistics_started_at: Option<i64>,
    pub consecutive_failures: u32,
    pub consecutive_successes: u32,
    pub last_success_at: Option<i64>,
    pub fresh: bool,
}
impl Default for ProbeHealth {
    fn default() -> Self {
        Self {
            transport_status: "unknown".into(),
            readiness_status: Readiness::Unknown,
            last_probe_result: None,
            probe_success_count: 0,
            probe_failure_count: 0,
            probe_excluded_count: 0,
            statistics_started_at: None,
            consecutive_failures: 0,
            consecutive_successes: 0,
            last_success_at: None,
            fresh: false,
        }
    }
}
impl ProbeHealth {
    pub fn invalidated(mut self) -> Self {
        self.fresh = false;
        self.transport_status = "unknown".into();
        self.readiness_status = Readiness::Unknown;
        self.consecutive_failures = 0;
        self.consecutive_successes = 0;
        self
    }
    pub fn eligible(&self, policy: &HealthPolicy) -> bool {
        !policy.required()
            || (self.fresh
                && self.proof_current(policy, crate::state::now_millis())
                && matches!(
                    self.readiness_status,
                    Readiness::Ready | Readiness::Degraded
                ))
    }
    pub fn proof_current(&self, policy: &HealthPolicy, now: i64) -> bool {
        self.last_success_at.is_some_and(|last| {
            now >= last && now.saturating_sub(last) <= (policy.max_age_seconds * 1000) as i64
        })
    }
    pub fn expire(self, policy: &HealthPolicy, now: i64) -> Self {
        if matches!(
            self.readiness_status,
            Readiness::Ready | Readiness::Degraded
        ) && !self.proof_current(policy, now)
        {
            self.invalidated()
        } else {
            self
        }
    }
    pub fn observe(
        mut self,
        policy: &HealthPolicy,
        result: &TestResult,
        probe_url: &str,
        url_source: &str,
        now: i64,
    ) -> Self {
        let remote_failure = matches!(
            result.diagnostics.scope,
            Some(FailureScope::Proxy | FailureScope::TargetRoute)
        );
        self.statistics_started_at.get_or_insert(now);
        self.fresh = true;
        if result.diagnostics.proxy_proven() {
            self.transport_status = "reachable".into();
        } else if result.diagnostics.scope == Some(FailureScope::Proxy) {
            self.transport_status = "unreachable".into();
        } else {
            self.transport_status = "unknown".into();
        }
        let outcome = if result.success {
            self.probe_success_count = self.probe_success_count.saturating_add(1);
            self.last_success_at = Some(now);
            self.consecutive_failures = 0;
            self.consecutive_successes = self.consecutive_successes.saturating_add(1);
            if !policy.required() || self.consecutive_successes >= policy.recovery_threshold {
                self.readiness_status = Readiness::Ready;
            }
            "success"
        } else if remote_failure {
            self.probe_failure_count = self.probe_failure_count.saturating_add(1);
            self.consecutive_successes = 0;
            self.consecutive_failures = self.consecutive_failures.saturating_add(1);
            if self.consecutive_failures >= policy.failure_threshold {
                self.readiness_status = Readiness::NotReady;
            } else if self.readiness_status == Readiness::Ready {
                self.readiness_status = Readiness::Degraded;
            }
            "failure"
        } else {
            self.probe_excluded_count = self.probe_excluded_count.saturating_add(1);
            // An inconclusive observation never proves recovery or a remote outage.
            self.consecutive_successes = 0;
            "unknown"
        };
        self.last_probe_result = Some(ProbeObservation {
            outcome: outcome.into(),
            observed_at: now,
            probe_url: safe_probe_url(probe_url),
            url_source: url_source.into(),
            response_time: result.response_time,
            status_code: result.status_code,
            diagnostics: serde_json::to_value(&result.diagnostics).unwrap_or_default(),
        });
        self
    }
}

fn safe_probe_url(raw: &str) -> String {
    url::Url::parse(raw)
        .map(|mut url| {
            let _ = url.set_username("");
            let _ = url.set_password(None);
            url.set_query(None);
            url.set_fragment(None);
            url.to_string()
        })
        .unwrap_or_else(|_| "invalid-url".into())
}

pub struct HealthEntry {
    pub observation_revision: u64,
    pub generation: Option<u64>,
    pub settings_revision: u64,
    pub health: ProbeHealth,
}

pub struct ProbeWrite {
    pub observation_revision: u64,
    pub generation: Option<u64>,
    pub settings_revision: u64,
    pub status_revision: u64,
    pub status: Option<String>,
    pub response_time: Option<i64>,
    pub health: ProbeHealth,
}
