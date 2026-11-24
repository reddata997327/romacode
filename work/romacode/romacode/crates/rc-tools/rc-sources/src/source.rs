//! The [`ToolSource`] trait — the single contract every tool-source mechanism
//! implements. A source discovers and produces ready-to-register tools. The
//! lifecycle manager walks a [`SourceRegistry`] of sources, asks each for its
//! tools, and registers them.
//!
//! This is the "unlimited" surface: any new mechanism (a new protocol, a new
//! registry, a new generator) plugs in by implementing one async method.

use async_trait::async_trait;
use rc_core::tool::{ToolArc, ToolSourceKind};
use thiserror::Error;

/// Errors a source can surface during discovery or loading. Note: the field
/// is named `src`, not `source` — `source` is a magic name in `thiserror`
/// (it auto-joins the error chain) and our value is a plain `String` id.
#[derive(Debug, Error)]
pub enum SourceError {
    #[error("source `{src}`: {detail}")]
    Other { src: String, detail: String },

    #[error("source `{src}`: i/o error: {inner}")]
    Io {
        src: String,
        #[source]
        inner: std::io::Error,
    },

    #[error("source `{src}`: json error: {inner}")]
    Json {
        src: String,
        #[source]
        inner: serde_json::Error,
    },

    #[error("source `{src}`: yaml error: {inner}")]
    Yaml {
        src: String,
        #[source]
        inner: serde_yaml::Error,
    },

    #[error("source `{src}`: http error: {inner}")]
    Http { src: String, inner: String },

    #[error("source `{src}`: not implemented — {detail}")]
    NotImplemented { src: String, detail: String },
}

impl SourceError {
    pub fn other(src: impl Into<String>, detail: impl Into<String>) -> Self {
        Self::Other {
            src: src.into(),
            detail: detail.into(),
        }
    }
}

/// The contract every tool source implements. `tools()` discovers and loads in
/// one step: it returns fully-built, ready-to-register [`ToolArc`] handles.
#[async_trait]
pub trait ToolSource: Send + Sync {
    /// Which of the 8 mechanisms this source is.
    fn kind(&self) -> ToolSourceKind;
    /// A short id for this source instance (e.g. `builtin`, `openapi:petstore`,
    /// `declarative:./tools.yaml`).
    fn id(&self) -> &str;
    /// Produce every tool this source can. Called by the lifecycle manager
    /// during the discover+load phase.
    async fn tools(&self) -> Result<Vec<ToolArc>, SourceError>;
}

/// A registry of sources — what the lifecycle manager iterates over.
#[derive(Default)]
pub struct SourceRegistry {
    sources: Vec<std::sync::Arc<dyn ToolSource>>,
}

impl SourceRegistry {
    pub fn new() -> Self {
        Self::default()
    }
    pub fn add(&mut self, source: impl ToolSource + 'static) {
        self.sources.push(std::sync::Arc::new(source));
    }
    pub fn len(&self) -> usize {
        self.sources.len()
    }
    pub fn is_empty(&self) -> bool {
        self.sources.is_empty()
    }
    /// Run every source's `tools()` concurrently and flatten the results.
    pub async fn all_tools(&self) -> Result<Vec<ToolArc>, SourceError> {
        let mut all = Vec::new();
        for s in &self.sources {
            let tools = s.tools().await?;
            all.extend(tools);
        }
        Ok(all)
    }
    /// Iterate sources (for `rc sources` listing).
    pub fn iter(&self) -> impl Iterator<Item = std::sync::Arc<dyn ToolSource>> + '_ {
        self.sources.iter().cloned()
    }
}
