//! Source 5 of 8 — **REST endpoint wrapper**. Wrap a single HTTP endpoint as a
//! tool: method, URL template, optional body template, optional static
//! headers, and the params. Good for one-off integrations without a full
//! OpenAPI spec.

use crate::source::{SourceError, ToolSource};
use crate::tools::HttpTool;
use async_trait::async_trait;
use rc_core::schema::Schema;
use rc_core::tool::{arc, ToolArc, ToolSourceKind};

pub struct RestSource {
    tool: HttpTool,
}

impl RestSource {
    pub fn new(
        name: impl Into<String>,
        description: impl Into<String>,
        schema: Schema,
        method: impl Into<String>,
        url: impl Into<String>,
    ) -> Self {
        Self {
            tool: HttpTool::new(name, description, schema, method, url, ToolSourceKind::Rest),
        }
    }
    pub fn with_body(mut self, body: impl Into<String>) -> Self {
        self.tool.body = Some(body.into());
        self
    }
    pub fn with_header(mut self, k: impl Into<String>, v: impl Into<String>) -> Self {
        self.tool.headers.push((k.into(), v.into()));
        self
    }
}

#[async_trait]
impl ToolSource for RestSource {
    fn kind(&self) -> ToolSourceKind {
        ToolSourceKind::Rest
    }
    fn id(&self) -> &str {
        &self.tool.name
    }
    async fn tools(&self) -> Result<Vec<ToolArc>, SourceError> {
        Ok(vec![arc(self.tool.clone())])
    }
}
