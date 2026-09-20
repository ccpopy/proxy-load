use serde_json::{json, Value};
use std::{
    collections::{HashMap, VecDeque},
    sync::Mutex,
};

#[derive(Default)]
pub(super) struct Telemetry(Mutex<HashMap<&'static str, VecDeque<u64>>>);
impl Telemetry {
    pub(super) fn record(&self, stage: &'static str, micros: u64) {
        let mut all = self.0.lock().unwrap_or_else(|e| e.into_inner());
        let samples = all
            .entry(stage)
            .or_insert_with(|| VecDeque::with_capacity(1024));
        if samples.len() == 1024 {
            samples.pop_front();
        }
        samples.push_back(micros);
    }
    pub(super) fn snapshot(&self) -> Value {
        let all = self.0.lock().unwrap_or_else(|e| e.into_inner()).clone();
        Value::Object(all.into_iter().map(|(stage, values)| {
            let mut sorted = values.into_iter().collect::<Vec<_>>(); sorted.sort_unstable();
            let at = |percent: usize| sorted[(sorted.len() * percent / 100).min(sorted.len() - 1)];
            (stage.into(), json!({"samples": sorted.len(), "p50_us": at(50), "p95_us": at(95), "p99_us": at(99)}))
        }).collect())
    }
}
