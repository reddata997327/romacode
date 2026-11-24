//! A [`Manifest`] — the serializable description of a tool, independent of
//! its runtime `Tool` impl. Sources produce manifests during discovery; the
//! lifecycle manager serializes them to disk (marketplace, declarative specs);
//! the registry holds live `Tool` impls. A manifest is everything you need to
//! *know* about a tool without loading it.

use crate::schema::Schema;
use crate::tool::Tool;
use serde::{Deserialize, Serialize};

/// The serializable identity + contract of a tool.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Manifest {
    pub name: String,
    pub description: String,
    pub category: String,
    pub source: String,
    pub version: String,
    #[serde(default)]
    pub schema: Schema,
    /// The permission class this tool belongs to (`read`, `ask`, `deny`, …).
    /// Empty = derive from category.
    #[serde(default)]
    pub permission: String,
    /// Optional: the path or URL or spec this tool was loaded from, for
    /// provenance and `rc info`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub origin: Option<String>,
}

impl Manifest {
    /// Build a manifest from a live `Tool` (snapshots its identity + schema).
    pub fn from_tool(t: &dyn Tool) -> Self {
        Self {
            name: t.name().to_string(),
            description: t.description().to_string(),
            category: t.category().to_string(),
            source: t.source().as_str().to_string(),
            version: t.version().to_string(),
            schema: t.schema(),
            permission: String::new(),
            origin: None,
        }
    }

    /// Render this manifest as a pretty JSON string (for `rc info`, files).
    pub fn to_json_pretty(&self) -> Result<String, serde_json::Error> {
        serde_json::to_string_pretty(self)
    }
}

/// A directory of manifests — what a source advertises before loading. The
/// lifecycle manager turns these into live tools on demand.
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct Catalog {
    pub manifests: Vec<Manifest>,
}

impl Catalog {
    pub fn new() -> Self {
        Self::default()
    }
    pub fn push(&mut self, m: Manifest) {
        self.manifests.push(m);
    }
    pub fn len(&self) -> usize {
        self.manifests.len()
    }
    pub fn is_empty(&self) -> bool {
        self.manifests.is_empty()
    }
    pub fn names(&self) -> Vec<String> {
        let mut n: Vec<String> = self.manifests.iter().map(|m| m.name.clone()).collect();
        n.sort();
        n
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::schema::{ParamType, Parameter};
    use crate::tool::{ToolContext, ToolSourceKind};

    struct T;
    #[async_trait::async_trait]
    impl Tool for T {
        fn name(&self) -> &str { "math.add" }
        fn description(&self) -> &str { "add two ints" }
        fn schema(&self) -> Schema {
            Schema::new().param(Parameter::required("a", ParamType::Integer))
        }
        fn source(&self) -> ToolSourceKind { ToolSourceKind::Builtin }
        async fn execute(&self, _: &serde_json::Value, _: &ToolContext) -> crate::error::ToolResult<serde_json::Value> { Ok(serde_json::json!({})) }
    }

    #[test]
    fn from_tool_roundtrip() {
        let m = Manifest::from_tool(&T);
        assert_eq!(m.name, "math.add");
        assert_eq!(m.category, "math");
        assert_eq!(m.source, "builtin");
        let j = m.to_json_pretty().unwrap();
        assert!(j.contains("\"math.add\""));
    }
}
