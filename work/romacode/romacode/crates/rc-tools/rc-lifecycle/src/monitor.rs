//! Per-tool execution metrics, recorded by the lifecycle manager on every
//! call. Thread-safe (`Arc<RwLock<…>>`), cheap to clone.

use serde::Serialize;
use std::collections::HashMap;
use std::sync::{Arc, RwLock};

#[derive(Debug, Clone, Default, Serialize)]
pub struct ToolMetrics {
    pub calls: u64,
    pub successes: u64,
    pub failures: u64,
    /// Cumulative latency in microseconds across all calls.
    pub total_latency_us: u128,
    /// The most recent failure message, if any.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub last_error: Option<String>,
}

impl ToolMetrics {
    pub fn success_rate(&self) -> f64 {
        if self.calls == 0 {
            return 0.0;
        }
        self.successes as f64 / self.calls as f64
    }
    pub fn avg_latency_us(&self) -> f64 {
        if self.calls == 0 {
            return 0.0;
        }
        self.total_latency_us as f64 / self.calls as f64
    }
}

#[derive(Clone, Default)]
pub struct MetricsStore {
    inner: Arc<RwLock<HashMap<String, ToolMetrics>>>,
}

impl MetricsStore {
    pub fn new() -> Self {
        Self::default()
    }

    /// Record one call's outcome. `latency_us` is the wall-clock cost.
    pub fn record(&self, name: &str, ok: bool, latency_us: u128, err: Option<String>) {
        let mut w = self.inner.write().unwrap();
        let m = w.entry(name.to_string()).or_default();
        m.calls += 1;
        m.total_latency_us += latency_us;
        if ok {
            m.successes += 1;
        } else {
            m.failures += 1;
            m.last_error = err;
        }
    }

    pub fn get(&self, name: &str) -> Option<ToolMetrics> {
        self.inner.read().unwrap().get(name).cloned()
    }

    /// Every tool's metrics, sorted by name.
    pub fn all(&self) -> Vec<(String, ToolMetrics)> {
        let r = self.inner.read().unwrap();
        let mut out: Vec<(String, ToolMetrics)> =
            r.iter().map(|(k, v)| (k.clone(), v.clone())).collect();
        out.sort_by(|a, b| a.0.cmp(&b.0));
        out
    }

    pub fn clear(&self) {
        self.inner.write().unwrap().clear();
    }
}
