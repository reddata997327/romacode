//! The lifecycle manager — walks the sources, registers every tool, validates
//! them, executes calls (timing + recording metrics), and retires tools. This
//! is L3: discover → register → validate → load → execute → monitor → version
//! → retire.

use crate::monitor::{MetricsStore, ToolMetrics};
use rc_core::error::ToolResult;
use rc_core::executor::Executor;
use rc_core::permissions::PermissionPolicy;
use rc_core::registry::Registry;
use rc_core::sandbox::Sandbox;
use rc_core::tool::ToolArc;
use rc_sources::{SourceError, SourceRegistry};
use serde_json::Value;
use std::time::Instant;

pub struct Lifecycle {
    pub registry: Registry,
    pub executor: Executor,
    pub sources: SourceRegistry,
    pub metrics: MetricsStore,
    /// The policy in force (for `rc validate` and re-binding).
    pub policy: PermissionPolicy,
    /// The sandbox in force.
    pub sandbox: Sandbox,
}

impl Lifecycle {
    /// Build a lifecycle with the given policy + sandbox and an empty
    /// registry. Add sources with [`add_source`], then call [`discover`].
    pub fn new(policy: PermissionPolicy, sandbox: Sandbox) -> Self {
        let registry = Registry::new();
        let executor = Executor::new(
            registry.clone(),
            policy.clone(),
            sandbox.clone(),
        );
        Self {
            registry,
            executor,
            sources: SourceRegistry::new(),
            metrics: MetricsStore::new(),
            policy,
            sandbox,
        }
    }

    /// A fully-permissive lifecycle (allow everything, default sandbox) — handy
    /// for examples and tests.
    pub fn permissive() -> Self {
        Self::new(
            PermissionPolicy::default().with_default(rc_core::permissions::Permission::Allow),
            Sandbox::default(),
        )
    }

    /// Register a source. Its tools are loaded on the next [`discover`].
    pub fn add_source(&mut self, source: impl rc_sources::ToolSource + 'static) {
        self.sources.add(source);
    }

    /// Run every source's `tools()` and register the results. Returns the
    /// number of tools registered.
    pub async fn discover(&self) -> Result<usize, SourceError> {
        let tools = self.sources.all_tools().await?;
        let n = tools.len();
        self.registry.register_all(tools);
        Ok(n)
    }

    /// Register a single already-built tool (e.g. a composition) directly.
    pub fn register(&self, tool: ToolArc) {
        self.registry.register(tool);
    }

    /// Register many tools directly.
    pub fn register_all(&self, tools: Vec<ToolArc>) {
        self.registry.register_all(tools);
    }

    /// Validate every registered tool's schema: no duplicate parameter names,
    /// every parameter has a name. Returns a list of issue strings (empty =
    /// all good).
    pub fn validate_all(&self) -> Vec<String> {
        let mut issues = Vec::new();
        for info in self.registry.list() {
            if info.name.is_empty() {
                issues.push(format!("tool with empty name"));
                continue;
            }
            if let Ok(tool) = self.registry.get(&info.name) {
                let schema = tool.schema();
                let mut seen = std::collections::HashSet::new();
                for p in &schema.parameters {
                    if p.name.is_empty() {
                        issues.push(format!("{}: parameter with empty name", info.name));
                    }
                    if !seen.insert(p.name.clone()) {
                        issues.push(format!("{}: duplicate parameter `{}`", info.name, p.name));
                    }
                }
            }
        }
        issues
    }

    /// Execute a tool by name, timing the call and recording metrics. The
    /// `approved` flag signals the caller already obtained human approval for
    /// an `Ask` decision.
    pub async fn execute(
        &self,
        name: &str,
        input: &Value,
        approved: bool,
    ) -> ToolResult<Value> {
        let start = Instant::now();
        let result = self.executor.run(name, input, approved).await;
        let elapsed = start.elapsed().as_micros();
        match &result {
            Ok(_) => self.metrics.record(name, true, elapsed, None),
            Err(e) => self.metrics.record(name, false, elapsed, Some(e.to_string())),
        }
        result
    }

    /// Retire (remove) a tool from the registry. Returns true if it was
    /// present.
    pub fn retire(&self, name: &str) -> bool {
        self.registry.remove(name).is_some()
    }

    /// Metrics for one tool.
    pub fn metrics_for(&self, name: &str) -> Option<ToolMetrics> {
        self.metrics.get(name)
    }

    /// Every tool's metrics, sorted by name.
    pub fn all_metrics(&self) -> Vec<(String, ToolMetrics)> {
        self.metrics.all()
    }

    /// A version report: every tool's name + version + source, sorted.
    pub fn version_report(&self) -> Vec<(String, String, String)> {
        let mut out: Vec<(String, String, String)> = self
            .registry
            .list()
            .into_iter()
            .map(|i| (i.name, i.version, i.source))
            .collect();
        out.sort();
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use rc_sources::builtin::BuiltinSource;

    #[tokio::test]
    async fn discover_registers_builtins() {
        let mut life = Lifecycle::permissive();
        life.add_source(BuiltinSource);
        let n = life.discover().await.unwrap();
        assert_eq!(n, rc_builtin::COUNT);
        assert!(life.registry.contains("math.add"));
    }

    #[tokio::test]
    async fn execute_records_metrics() {
        let mut life = Lifecycle::permissive();
        life.add_source(BuiltinSource);
        life.discover().await.unwrap();
        let out = life
            .execute("math.add", &serde_json::json!({ "a": 2, "b": 3 }), false)
            .await
            .unwrap();
        assert!((out.as_f64().unwrap() - 5.0).abs() < 1e-9);
        let m = life.metrics_for("math.add").unwrap();
        assert_eq!(m.calls, 1);
        assert_eq!(m.successes, 1);
        assert!(m.avg_latency_us() >= 0.0);
    }

    #[tokio::test]
    async fn execute_records_failures() {
        let mut life = Lifecycle::permissive();
        life.add_source(BuiltinSource);
        life.discover().await.unwrap();
        // bad input: missing required param
        let err = life.execute("math.add", &serde_json::json!({ "a": 2 }), false).await;
        assert!(err.is_err());
        let m = life.metrics_for("math.add").unwrap();
        assert_eq!(m.calls, 1);
        assert_eq!(m.failures, 1);
        assert!(m.last_error.is_some());
    }

    #[test]
    fn validate_all_passes_for_builtins() {
        let mut life = Lifecycle::permissive();
        life.add_source(BuiltinSource);
        // discover is async; block_on via current_thread runtime would need a
        // tokio test attribute. Instead register directly.
        life.register_all(rc_builtin::all_tools());
        let issues = life.validate_all();
        assert!(issues.is_empty(), "issues: {issues:?}");
    }

    #[tokio::test]
    async fn retire_removes_a_tool() {
        let mut life = Lifecycle::permissive();
        life.add_source(BuiltinSource);
        life.discover().await.unwrap();
        assert!(life.registry.contains("text.upper"));
        assert!(life.retire("text.upper"));
        assert!(!life.registry.contains("text.upper"));
    }

    #[tokio::test]
    async fn version_report_lists_tools() {
        let mut life = Lifecycle::permissive();
        life.add_source(BuiltinSource);
        life.discover().await.unwrap();
        let report = life.version_report();
        assert!(report.iter().any(|(n, _, _)| n == "math.add"));
        assert!(report.iter().all(|(_, _, s)| s == "builtin"));
    }
}
