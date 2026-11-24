//! The in-memory tool [`Registry`] — a thread-safe store of every loaded
//! tool, indexed by name. The registry is the spine of the framework: sources
//! feed it, the executor reads it, composition reads it to resolve steps, and
//! the CLI lists it.

use crate::error::{ToolError, ToolResult};
use crate::tool::ToolArc;
use std::collections::HashMap;
use std::sync::RwLock;

/// A thread-safe registry of tools. Cheap to clone (it is `Arc`-backed).
#[derive(Clone, Default)]
pub struct Registry {
    inner: std::sync::Arc<RwLock<HashMap<String, ToolArc>>>,
}

/// A read-only snapshot of one registered tool's identity.
#[derive(Debug, Clone)]
pub struct ToolInfo {
    pub name: String,
    pub description: String,
    pub category: String,
    pub source: String,
    pub version: String,
}

impl Registry {
    pub fn new() -> Self {
        Self {
            inner: std::sync::Arc::new(RwLock::new(HashMap::new())),
        }
    }

    /// Register a tool. If a tool with the same name exists, it is replaced
    /// (the new version wins — see the lifecycle manager for upgrade policy).
    /// Returns the previous tool if one was displaced.
    pub fn register(&self, tool: ToolArc) -> Option<ToolArc> {
        let mut w = self.inner.write().unwrap();
        w.insert(tool.name().to_string(), tool)
    }

    /// Register many tools at once (e.g. from a source's `tools()`).
    pub fn register_all(&self, tools: Vec<ToolArc>) {
        let mut w = self.inner.write().unwrap();
        for t in tools {
            w.insert(t.name().to_string(), t);
        }
    }

    /// Look up a tool by name.
    pub fn get(&self, name: &str) -> ToolResult<ToolArc> {
        self.inner
            .read()
            .unwrap()
            .get(name)
            .cloned()
            .ok_or_else(|| ToolError::NotFound(name.to_string()))
    }

    /// Does a tool with this name exist?
    pub fn contains(&self, name: &str) -> bool {
        self.inner.read().unwrap().contains_key(name)
    }

    /// Remove a tool (retire it). Returns the removed tool if present.
    pub fn remove(&self, name: &str) -> Option<ToolArc> {
        self.inner.write().unwrap().remove(name)
    }

    /// Number of registered tools.
    pub fn len(&self) -> usize {
        self.inner.read().unwrap().len()
    }

    pub fn is_empty(&self) -> bool {
        self.inner.read().unwrap().is_empty()
    }

    /// A sorted snapshot of every tool's identity (for `rc list`).
    pub fn list(&self) -> Vec<ToolInfo> {
        let r = self.inner.read().unwrap();
        let mut out: Vec<ToolInfo> = r
            .values()
            .map(|t| ToolInfo {
                name: t.name().to_string(),
                description: t.description().to_string(),
                category: t.category().to_string(),
                source: t.source().as_str().to_string(),
                version: t.version().to_string(),
            })
            .collect();
        out.sort_by(|a, b| a.name.cmp(&b.name));
        out
    }

    /// All registered tool names, sorted.
    pub fn names(&self) -> Vec<String> {
        let mut n: Vec<String> = self.inner.read().unwrap().keys().cloned().collect();
        n.sort();
        n
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::schema::{ParamType, Parameter, Schema};
    use crate::tool::{ToolContext, ToolSourceKind};
    use crate::Tool;
    use async_trait::async_trait;
    use serde_json::Value;

    struct Dummy {
        name: &'static str,
    }

    #[async_trait]
    impl Tool for Dummy {
        fn name(&self) -> &str {
            self.name
        }
        fn description(&self) -> &str {
            "dummy"
        }
        fn schema(&self) -> Schema {
            Schema::new().param(Parameter::required("x", ParamType::Integer))
        }
        fn source(&self) -> ToolSourceKind {
            ToolSourceKind::Builtin
        }
        async fn execute(&self, input: &Value, _ctx: &ToolContext) -> ToolResult<Value> {
            Ok(serde_json::json!({ "echo": input }))
        }
    }

    #[test]
    fn register_get_remove() {
        let r = Registry::new();
        assert!(r.is_empty());
        r.register(crate::tool::arc(Dummy { name: "test.dummy" }));
        assert_eq!(r.len(), 1);
        assert!(r.contains("test.dummy"));
        assert!(r.get("test.dummy").is_ok());
        assert!(r.get("nope").is_err());
        assert!(r.remove("test.dummy").is_some());
        assert!(r.is_empty());
    }

    #[test]
    fn list_sorted() {
        let r = Registry::new();
        r.register(crate::tool::arc(Dummy { name: "b.x" }));
        r.register(crate::tool::arc(Dummy { name: "a.x" }));
        let names: Vec<_> = r.list().into_iter().map(|i| i.name).collect();
        assert_eq!(names, vec!["a.x", "b.x"]);
    }
}
