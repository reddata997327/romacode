//! Source 2 of 8 — **declarative**. A user writes a YAML or JSON file listing
//! tools (each a name, description, command template, and params); the source
//! builds a [`ShellTool`] for each. No Rust code is written — this is how a
//! non-developper adds tools.
//!
//! Values substituted into `{{param}}` placeholders are always shell-quoted
//! into single literal shell words (see `rc_sources::tools::shell_quote`), so
//! templates must not wrap placeholders in their own quotes.
//!
//! ```yaml
//! tools:
//!   - name: git.status
//!     description: Show git working-tree status
//!     command: "git -C {{repo}} status --porcelain"
//!     params:
//!       - { name: repo, type: string, required: true, description: repo path }
//!   - name: disk.du
//!     description: Disk usage of a directory
//!     command: "du -sh {{path}}"
//!     params:
//!       - { name: path, type: string, required: true }
//! ```

use crate::source::{SourceError, ToolSource};
use crate::tools::ShellTool;
use async_trait::async_trait;
use rc_core::schema::{ParamType, Parameter, Schema};
use rc_core::tool::{ToolArc, ToolSourceKind};
use serde::{Deserialize, Serialize};
use serde_json::Value;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DeclarativeSpec {
    #[serde(default)]
    pub tools: Vec<DeclarativeTool>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DeclarativeTool {
    pub name: String,
    #[serde(default)]
    pub description: String,
    pub command: String,
    #[serde(default)]
    pub params: Vec<DeclarativeParam>,
    #[serde(default)]
    pub cwd: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DeclarativeParam {
    pub name: String,
    #[serde(rename = "type")]
    pub typ: String,
    #[serde(default)]
    pub required: bool,
    #[serde(default)]
    pub description: String,
    #[serde(default)]
    pub default: Option<Value>,
}

pub fn parse_param_type(s: &str) -> ParamType {
    match s {
        "string" => ParamType::String,
        "integer" => ParamType::Integer,
        "number" => ParamType::Number,
        "boolean" => ParamType::Boolean,
        "object" => ParamType::Object,
        "any" => ParamType::Any,
        _ => ParamType::String, // lenient default
    }
}

impl DeclarativeSpec {
    /// Parse a YAML or JSON string. JSON is valid YAML, so we try YAML first.
    pub fn parse(content: &str) -> Result<Self, SourceError> {
        serde_yaml::from_str::<DeclarativeSpec>(content).map_err(|e| SourceError::Yaml {
            src: "declarative".into(),
            inner: e,
        })
    }
    /// Load from a file path (auto-detects YAML/JSON by content).
    pub async fn load_file(path: &str) -> Result<Self, SourceError> {
        let content = tokio::fs::read_to_string(path).await.map_err(|e| {
            SourceError::Io {
                src: "declarative".into(),
                inner: e,
            }
        })?;
        Self::parse(&content)
    }
    /// Build the spec's tools as ready-to-register `ShellTool`s.
    pub fn build_tools(&self) -> Vec<ToolArc> {
        self.tools
            .iter()
            .map(|t| {
                let mut schema = Schema::new();
                for p in &t.params {
                    let mut param = Parameter {
                        name: p.name.clone(),
                        description: p.description.clone(),
                        typ: parse_param_type(&p.typ),
                        required: p.required,
                        default: p.default.clone(),
                        enum_values: Vec::new(),
                    };
                    if param.description.is_empty() {
                        param.description = String::new();
                    }
                    schema = schema.param(param);
                }
                let mut shell = ShellTool::new(
                    t.name.clone(),
                    t.description.clone(),
                    schema,
                    t.command.clone(),
                    ToolSourceKind::Declarative,
                );
                shell.cwd = t.cwd.clone();
                rc_core::tool::arc(shell)
            })
            .collect()
    }
}

pub struct DeclarativeSource {
    spec: DeclarativeSpec,
    origin: String,
}

impl DeclarativeSource {
    /// Build from an already-parsed spec.
    pub fn from_spec(spec: DeclarativeSpec, origin: impl Into<String>) -> Self {
        Self {
            spec,
            origin: origin.into(),
        }
    }
    /// Load a YAML/JSON spec file.
    pub async fn from_file(path: &str) -> Result<Self, SourceError> {
        let spec = DeclarativeSpec::load_file(path).await?;
        Ok(Self::from_spec(spec, path))
    }
}

#[async_trait]
impl ToolSource for DeclarativeSource {
    fn kind(&self) -> ToolSourceKind {
        ToolSourceKind::Declarative
    }
    fn id(&self) -> &str {
        &self.origin
    }
    async fn tools(&self) -> Result<Vec<ToolArc>, SourceError> {
        Ok(self.spec.build_tools())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use rc_core::Registry;

    #[test]
    fn parse_and_build_yaml() {
        let yaml = r#"
tools:
  - name: git.status
    description: git status
    command: "git -C {{repo}} status"
    params:
      - { name: repo, type: string, required: true, description: repo path }
  - name: echo.hi
    description: echo
    command: "echo hi"
"#;
        let spec = DeclarativeSpec::parse(yaml).unwrap();
        assert_eq!(spec.tools.len(), 2);
        let tools = spec.build_tools();
        assert_eq!(tools.len(), 2);
        let r = Registry::new();
        r.register_all(tools);
        assert!(r.contains("git.status"));
        assert!(r.contains("echo.hi"));
        // git.status has a required string param "repo"
        let t = r.get("git.status").unwrap();
        assert_eq!(t.schema().parameters.len(), 1);
        assert!(t.schema().parameters[0].required);
    }

    #[test]
    fn parse_json() {
        let json = r#"{"tools":[{"name":"x.y","description":"d","command":"echo {{s}}","params":[{"name":"s","type":"string","required":true}]}]}"#;
        let spec = DeclarativeSpec::parse(json).unwrap();
        assert_eq!(spec.tools.len(), 1);
    }
}
