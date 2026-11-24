//! Source 1 of 8 — **builtin**. Wraps [`rc_builtin::all_tools`] so the
//! framework's shipped catalog is reachable through the same `ToolSource`
//! trait as every other source.

use crate::source::{SourceError, ToolSource};
use async_trait::async_trait;
use rc_core::tool::{ToolArc, ToolSourceKind};

pub struct BuiltinSource;

impl Default for BuiltinSource {
    fn default() -> Self {
        Self
    }
}

#[async_trait]
impl ToolSource for BuiltinSource {
    fn kind(&self) -> ToolSourceKind {
        ToolSourceKind::Builtin
    }
    fn id(&self) -> &str {
        "builtin"
    }
    async fn tools(&self) -> Result<Vec<ToolArc>, SourceError> {
        Ok(rc_builtin::all_tools())
    }
}
