//! The rc-tools `ToolSource` for the built-in **documents engine** (`rc-docs`).
//!
//! Registers every Word / Excel / PowerPoint / PDF / chart / canvas / Figma
//! tool with the tools-builder registry under the `documents` source id, so
//! the `rc_tools` gateway, the server and the Ctrl+P Tools screen all list
//! and run them through the same lifecycle as every other source.

use rc_core::{schema::Parameter, schema::ParamType, schema::Schema, Tool, ToolArc, ToolContext, ToolError, ToolSourceKind};
use rc_sources::SourceError;
use serde_json::Value;

/// One documents engine tool wrapped as an rc-core `Tool`.
pub struct RcDocsTool {
    name: String,
    description: String,
    schema: Schema,
}

#[async_trait::async_trait]
impl Tool for RcDocsTool {
    fn name(&self) -> &str {
        &self.name
    }

    fn description(&self) -> &str {
        &self.description
    }

    fn schema(&self) -> Schema {
        self.schema.clone()
    }

    fn source(&self) -> ToolSourceKind {
        ToolSourceKind::Builtin
    }

    fn category(&self) -> &str {
        match self.name.split('_').next().unwrap_or("docs") {
            "chart" => "chart",
            other => other,
        }
    }

    async fn execute(&self, input: &Value, _ctx: &ToolContext) -> Result<Value, ToolError> {
        let base = std::env::current_dir()
            .map_err(|e| ToolError::ToolFailed { tool: self.name.clone(), inner: format!("project dir: {e}") })?;
        rc_docs::tools::execute(&self.name, input, &base)
            .map_err(|inner| ToolError::ToolFailed { tool: self.name.clone(), inner })
    }
}

/// The `ToolSource` for the documents engine.
pub struct DocumentsSource;

#[async_trait::async_trait]
impl rc_sources::ToolSource for DocumentsSource {
    fn kind(&self) -> ToolSourceKind {
        ToolSourceKind::Builtin
    }

    fn id(&self) -> &str {
        "documents"
    }

    async fn tools(&self) -> Result<Vec<ToolArc>, SourceError> {
        let mut out = Vec::new();
        for def in rc_docs::tools::definitions() {
            let name = def["name"].as_str().unwrap_or_default().to_string();
            let description = def["description"].as_str().unwrap_or_default().to_string();
            let mut schema = Schema::new();
            if let Some(props) = def["input_schema"]["properties"].as_object() {
                let required: Vec<&str> = def["input_schema"]["required"]
                    .as_array()
                    .map(|a| a.iter().filter_map(|r| r.as_str()).collect())
                    .unwrap_or_default();
                for (key, spec) in props {
                    let typ = match spec["type"].as_str().unwrap_or("string") {
                        "number" => ParamType::Number,
                        "integer" => ParamType::Integer,
                        "boolean" => ParamType::Boolean,
                        "object" => ParamType::Object,
                        "array" => ParamType::Array(Box::new(ParamType::Any)),
                        _ => ParamType::String,
                    };
                    let mut p = if required.contains(&key.as_str()) {
                        Parameter::required(key, typ)
                    } else {
                        Parameter::optional(key, typ)
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
