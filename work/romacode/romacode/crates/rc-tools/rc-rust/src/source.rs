//! The `RustSource` adapter — presents the `rust.*` language pack through the
//! same [`rc_sources::ToolSource`] contract as every other source (builtin,
//! declarative, openapi, …). Adding it to a [`SourceRegistry`] makes the eight
//! Rust-dev tools discoverable by the lifecycle manager alongside the rest.
//!
//! `RustSource` reports [`ToolSourceKind::Builtin`] because, like
//! `rc-builtin`, these tools ship with the framework — the `rust` category is
//! carried in each tool's name, not in the source kind.

use async_trait::async_trait;
use rc_core::tool::{ToolArc, ToolSourceKind};
use rc_sources::{SourceError, ToolSource};

/// A tool source that yields the eight `rust.*` developer-workflow tools.
#[derive(Debug, Default, Clone, Copy)]
pub struct RustSource;

#[async_trait]
impl ToolSource for RustSource {
    fn kind(&self) -> ToolSourceKind {
        ToolSourceKind::Builtin
    }
    fn id(&self) -> &str {
        "rust"
    }
    async fn tools(&self) -> Result<Vec<ToolArc>, SourceError> {
        Ok(crate::tools::all())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use rc_sources::SourceRegistry;

    #[tokio::test]
    async fn yields_every_rust_tool() {
        let src = RustSource;
        assert_eq!(src.kind(), ToolSourceKind::Builtin);
        assert_eq!(src.id(), "rust");
        let tools = src.tools().await.expect("rust source loads");
        assert_eq!(tools.len(), crate::tools::COUNT);
        assert!(tools.iter().all(|t| t.name().starts_with("rust.")));
    }

    #[tokio::test]
    async fn registers_via_source_registry() {
        let mut sr = SourceRegistry::new();
        sr.add(RustSource);
        assert_eq!(sr.len(), 1);
        let tools = sr.all_tools().await.expect("all_tools");
        assert_eq!(tools.len(), crate::tools::COUNT);
    }
}
