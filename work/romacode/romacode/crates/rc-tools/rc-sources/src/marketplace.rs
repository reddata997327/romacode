//! Source 8 of 8 — **marketplace / URL import**. Fetch a manifest of
//! declarative tool specs (the same JSON/YAML shape the `declarative` source
//! reads) from a URL or a file, and produce [`ShellTool`]s tagged
//! `marketplace`. Point this at any tool registry that serves the manifest
//! format — a git raw URL, a company-internal index, a community catalog.
//!
//! This is the community-driven infinite-supply mechanism: anyone can publish
//! a manifest, anyone can pull it.

use crate::declarative::DeclarativeSpec;
use crate::llm_gen::tool_from_declarative;
use crate::source::{SourceError, ToolSource};
use async_trait::async_trait;
use rc_core::tool::{ToolArc, ToolSourceKind};

pub struct MarketplaceSource {
    spec: DeclarativeSpec,
    origin: String,
}

impl MarketplaceSource {
    pub fn from_str(content: &str, origin: impl Into<String>) -> Result<Self, SourceError> {
        let spec = DeclarativeSpec::parse(content)?;
        Ok(Self {
            spec,
            origin: origin.into(),
        })
    }
    pub async fn from_file(path: &str) -> Result<Self, SourceError> {
        let content = tokio::fs::read_to_string(path)
            .await
            .map_err(|e| SourceError::Io {
                src: "marketplace".into(),
                inner: e,
            })?;
        Self::from_str(&content, path)
    }
    /// Fetch a manifest from a URL.
    pub async fn from_url(url: &str) -> Result<Self, SourceError> {
        let resp = reqwest::get(url).await.map_err(|e| SourceError::Http {
            src: "marketplace".into(),
            inner: e.to_string(),
        })?;
        let content = resp.text().await.map_err(|e| SourceError::Http {
            src: "marketplace".into(),
            inner: e.to_string(),
        })?;
        Self::from_str(&content, url)
    }
}

#[async_trait]
impl ToolSource for MarketplaceSource {
    fn kind(&self) -> ToolSourceKind {
        ToolSourceKind::Marketplace
    }
    fn id(&self) -> &str {
        &self.origin
    }
    async fn tools(&self) -> Result<Vec<ToolArc>, SourceError> {
        Ok(self
            .spec
            .tools
            .iter()
            .map(|t| tool_from_declarative(t.clone(), ToolSourceKind::Marketplace))
            .collect())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use rc_core::Registry;

    #[tokio::test]
    async fn from_str_registers() {
        let yaml = r#"
tools:
  - name: market.greet
    description: greet
    command: "echo hi {{name}}"
    params:
      - { name: name, type: string, required: true }
"#;
        let src = MarketplaceSource::from_str(yaml, "test").unwrap();
        let tools = src.tools().await.unwrap();
        let r = Registry::new();
        r.register_all(tools);
        let t = r.get("market.greet").unwrap();
        assert_eq!(t.source(), ToolSourceKind::Marketplace);
        assert_eq!(t.schema().parameters.len(), 1);
    }
}
