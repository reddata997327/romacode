//! Bridge between the **sync** ratatui/crossterm event loop and the **async**
//! RomaCode tools builder. Holds a dedicated tokio runtime + a permissive
//! [`Lifecycle`] preloaded with the builtin source, and exposes plain sync
//! calls the Ctrl+P settings panel's Tools section uses: `count`, `list`,
//! `categories`, `schema_json`, `run`.
//!
//! The tools builder is the "unlimited" half of RomaCode — 38 built-in tools
//! across 8 categories (math, text, data, code, file, web, system, time),
//! reachable here through the same `ToolSource` trait as every other source.
//! The full framework (8 sources, 5 composition primitives) lives in the
//! `rc-*` crates; this bridge surfaces its builtin catalog to the TUI.

use rc_lifecycle::Lifecycle;
use rc_sources::builtin::BuiltinSource;
use serde_json::Value;

/// One row of the Tools list (a snapshot of a registered tool's identity).
#[derive(Clone, Debug)]
pub struct ToolEntry {
    pub name: String,
    pub category: String,
    pub description: String,
    /// Which tool-source mechanism produced this tool (builtin, declarative…).
    /// Surfaced for a future "info" view; not rendered in the list today.
    #[allow(dead_code)]
    pub source: String,
}

/// A sync façade over the async tools builder, kept on the `App` and reused
/// across settings-panel opens. Constructed once; cheap to borrow.
pub struct Tools {
    rt: tokio::runtime::Runtime,
    life: Lifecycle,
}

impl Tools {
    /// Build the runtime, construct a permissive lifecycle, register the
    /// builtin source, and discover every tool. Synchronous from the caller's
    /// point of view (the discovery future is driven on the runtime).
    pub fn new() -> Self {
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .expect("tools builder: tokio runtime");
        let life = rt.block_on(async {
            let mut life = Lifecycle::permissive();
            life.add_source(BuiltinSource);
            // The built-in documents engine: Word, Excel, PowerPoint, PDF,
            // charts, canvas and Figma tools.
            life.add_source(rc_docs_source::DocumentsSource);
            let _ = life.discover().await;
            life
        });
        Self { rt, life }
    }

    /// Total number of registered tools (the builtin catalog = 38).
    #[allow(dead_code)]
    pub fn count(&self) -> usize {
        self.life.registry.len()
    }

    /// Every registered tool, sorted by name.
    pub fn list(&self) -> Vec<ToolEntry> {
        self.life
            .registry
            .list()
            .into_iter()
            .map(|i| ToolEntry {
                name: i.name,
                category: i.category,
                description: i.description,
                source: i.source,
            })
            .collect()
    }

    /// Distinct categories, sorted (math, text, data, code, file, web, …).
    pub fn categories(&self) -> Vec<String> {
        let mut cats: Vec<String> = self.list().iter().map(|t| t.category.clone()).collect();
        cats.sort();
        cats.dedup();
        cats
    }

    /// The JSON schema of one tool (for an info view), or `None` if unknown.
    pub fn schema_json(&self, name: &str) -> Option<String> {
        let t = self.life.registry.get(name).ok()?;
        serde_json::to_string_pretty(&t.schema().to_json_schema()).ok()
    }

    /// Run a tool by name with a JSON input. Auto-approves (the Tools section
    /// is an explicit user action). Returns the tool's JSON output or an error
    /// string. Driven on the runtime so it never blocks the TUI's own (absent)
    /// async context — though a slow tool (web fetch) will block the UI thread
    /// for up to the sandbox timeout (15 s).
    pub fn run(&self, name: &str, input: &Value) -> Result<Value, String> {
        self.rt
            .block_on(self.life.execute(name, input, true))
            .map_err(|e| e.to_string())
    }

    /// Per-tool metrics snapshot (calls / successes / failures / latency).
    #[allow(dead_code)]
    pub fn metrics(&self, name: &str) -> Option<rc_lifecycle::ToolMetrics> {
        self.life.metrics_for(name)
    }
}

impl Default for Tools {
    fn default() -> Self {
        Self::new()
    }
}
