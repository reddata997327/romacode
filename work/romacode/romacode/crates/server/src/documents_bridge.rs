//! Bridge between the brain and the built-in **documents engine** (`rc-docs`
//! — vendored pure Rust under `crates/documents`).
//!
//! Word / Excel / PowerPoint / PDF / charts / canvas / Figma tools surface
//! here exactly like the other vendored engines (rustango, remix): JSON tool
//! definitions come from the engine, execution anchors at the project root,
//! and the same catalog/security gate classifies every call. The tools are
//! also exposed to the rc-tools builder framework as a [`DocumentsSource`],
//! so the `rc_tools` gateway and the Ctrl+P Tools screen list them for free.

use crate::config::Config;
use crate::security;
use rc_docs::tools as engine;
use serde_json::{json, Value};

/// Every document tool definition (Anthropic-schema JSON).
pub fn definitions() -> Vec<Value> {
    engine::definitions()
}

/// Does this tool name belong to the documents engine?
pub fn owns(name: &str) -> bool {
    engine::owns(name)
}

/// Execute one documents tool. Output is capped by
/// `documents.max_output_chars` (0 = unlimited) so a huge `xlsx_read` cannot
/// flood the context.
pub fn execute(cfg: &Config, name: &str, input: &Value) -> Result<Value, String> {
    let root = security::root()?;
    let out = engine::execute(name, input, &root)?;
    let cap = cfg.documents.max_output_chars;
    if cap > 0 {
        let s = serde_json::to_string(&out).unwrap_or_default();
        if s.chars().count() > cap {
            let mut trimmed: Value = out;
            if let Some(t) = trimmed.get_mut("text").and_then(|t| t.as_str().map(String::from)) {
                let t: String = t.chars().take(cap).collect();
                trimmed["text"] = json!(t);
                trimmed["note"] = json!("output truncated to documents.max_output_chars");
                return Ok(trimmed);
            }
            return Ok(json!({
                "truncated": true,
                "cap_chars": cap,
                "head": s.chars().take(cap).collect::<String>(),
                "note": "output truncated to documents.max_output_chars",
            }));
        }
    }
    Ok(out)
}

// ── rc-tools integration ────────────────────────────────────────────────────

/// One documents engine tool wrapped as an rc-core `Tool`.
pub struct RcDocsTool {
    name: String,
    description: String,
    schema: rc_core::schema::Schema,
}

#[async_trait::async_trait]
impl rc_core::Tool for RcDocsTool {
    fn name(&self) -> &str {
        &self.name
    }

    fn description(&self) -> &str {
        &self.description
    }

    fn schema(&self) -> rc_core::schema::Schema {
        self.schema.clone()
    }

    fn source(&self) -> rc_core::ToolSourceKind {
        rc_core::ToolSourceKind::Builtin
    }

    fn category(&self) -> &str {
        // docx_create → "docx", figma_file → "figma", chart_render → "chart"
        match self.name.split('_').next().unwrap_or("docs") {
            "chart" => "chart",
            other => other,
        }
    }

    async fn execute(&self, input: &Value, _ctx: &rc_core::ToolContext) -> Result<Value, rc_core::ToolError> {
        let base = std::env::current_dir()
            .map_err(|e| rc_core::ToolError::ToolFailed { tool: self.name.clone(), inner: format!("project dir: {e}") })?;
        engine::execute(&self.name, input, &base)
            .map_err(|inner| rc_core::ToolError::ToolFailed { tool: self.name.clone(), inner })
    }
}

/// The rc-tools `ToolSource` for the documents engine: registers every
/// rc-docs tool under its own source id so `rc list` / the TUI Tools screen
/// show a `documents` source group.
pub struct DocumentsSource;

#[async_trait::async_trait]
impl rc_sources::ToolSource for DocumentsSource {
    fn kind(&self) -> rc_core::ToolSourceKind {
        rc_core::ToolSourceKind::Builtin
    }

    fn id(&self) -> &str {
        "documents"
    }

    async fn tools(&self) -> Result<Vec<rc_core::ToolArc>, rc_sources::SourceError> {
        let mut out = Vec::new();
        for def in engine::definitions() {
            let name = def["name"].as_str().unwrap_or_default().to_string();
            let description = def["description"].as_str().unwrap_or_default().to_string();
            let mut schema = rc_core::schema::Schema::new();
            if let Some(props) = def["input_schema"]["properties"].as_object() {
                let required: Vec<&str> = def["input_schema"]["required"]
                    .as_array()
                    .map(|a| a.iter().filter_map(|r| r.as_str()).collect())
                    .unwrap_or_default();
                for (key, spec) in props {
                    let typ = match spec["type"].as_str().unwrap_or("string") {
                        "number" => rc_core::schema::ParamType::Number,
                        "integer" => rc_core::schema::ParamType::Integer,
                        "boolean" => rc_core::schema::ParamType::Boolean,
                        "object" => rc_core::schema::ParamType::Object,
                        "array" => rc_core::schema::ParamType::Array(Box::new(rc_core::schema::ParamType::Any)),
                        _ => rc_core::schema::ParamType::String,
                    };
                    let mut p = if required.contains(&key.as_str()) {
                        rc_core::schema::Parameter::required(key, typ)
                    } else {
                        rc_core::schema::Parameter::optional(key, typ)
                    };
                    if let Some(desc) = spec["description"].as_str() {
                        p = p.with_desc(desc);
                    }
                    if let Some(en) = spec["enum"].as_array() {
                        p = p.with_enum(en.clone());
                    }
                    schema = schema.param(p);
                }
            }
            out.push(rc_core::arc(RcDocsTool { name, description, schema }));
        }
        Ok(out)
    }
}

/// Engine + tool inventory for the doctor / status output.
pub fn status() -> Value {
    json!({
        "engine": format!("rc-docs {}", rc_docs::VERSION),
        "tools": engine::definitions().iter().filter_map(|d| d["name"].as_str()).collect::<Vec<_>>(),
        "rc_tools_source": "documents",
    })
}
