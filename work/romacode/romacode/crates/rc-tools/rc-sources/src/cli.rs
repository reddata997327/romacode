//! Source 6 of 8 — **CLI binary wrapper**. Wrap any installed binary as a
//! tool: the `command` template is the binary + its arg template, with
//! `{{param}}` placeholders. This wraps the entire non-HTTP, non-MCP world of
//! existing command-line tools.

use crate::source::{SourceError, ToolSource};
use crate::tools::ShellTool;
use async_trait::async_trait;
use rc_core::schema::Schema;
use rc_core::tool::{arc, ToolArc, ToolSourceKind};

pub struct CliSource {
    tool: ShellTool,
}

impl CliSource {
    pub fn new(
        name: impl Into<String>,
        description: impl Into<String>,
        schema: Schema,
        command: impl Into<String>,
    ) -> Self {
        Self {
            tool: ShellTool::new(name, description, schema, command, ToolSourceKind::Cli),
        }
    }
    pub fn with_cwd(mut self, cwd: impl Into<String>) -> Self {
        self.tool.cwd = Some(cwd.into());
        self
    }
}

#[async_trait]
impl ToolSource for CliSource {
    fn kind(&self) -> ToolSourceKind {
        ToolSourceKind::Cli
    }
    fn id(&self) -> &str {
        &self.tool.name
    }
    async fn tools(&self) -> Result<Vec<ToolArc>, SourceError> {
        Ok(vec![arc(self.tool.clone())])
    }
}
