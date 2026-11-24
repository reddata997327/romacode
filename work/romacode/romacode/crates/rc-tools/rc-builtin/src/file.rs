//! `file.*` tools — filesystem read/write/list/exists via `tokio::fs`. Each
//! tool consults `ctx.check_path()` so the sandbox's FS allow-list is
//! enforced at the tool level (not just the executor).

use rc_core::schema::{ParamType, Parameter, Schema};
use rc_core::tool::{ToolContext, ToolSourceKind, ToolArc};
use rc_core::{arc, Tool, ToolResult};
use async_trait::async_trait;
use serde_json::Value;

use crate::need_param;

pub struct Read;
#[async_trait]
impl Tool for Read {
    fn name(&self) -> &str { "file.read" }
    fn description(&self) -> &str { "Read a UTF-8 text file. Enforces the sandbox FS allow-list." }
    fn schema(&self) -> Schema {
        Schema::new().param(Parameter::required("path", ParamType::String))
    }
    fn source(&self) -> ToolSourceKind { ToolSourceKind::Builtin }
    async fn execute(&self, input: &Value, ctx: &ToolContext) -> ToolResult<Value> {
        let path = need_param(input, "path", ParamType::String, Value::as_str)?;
        ctx.check_path(path)?;
        let bytes = tokio::fs::read(path).await?;
        let text = String::from_utf8(bytes).map_err(|e| {
            rc_core::error::ToolError::Other(format!("file is not valid UTF-8: {e}"))
        })?;
        Ok(Value::String(text))
    }
}

pub struct Write;
#[async_trait]
impl Tool for Write {
    fn name(&self) -> &str { "file.write" }
    fn description(&self) -> &str { "Write a UTF-8 string to a file (overwrites). Enforces the sandbox FS allow-list." }
    fn schema(&self) -> Schema {
        Schema::new()
            .param(Parameter::required("path", ParamType::String))
            .param(Parameter::required("content", ParamType::String))
    }
    fn source(&self) -> ToolSourceKind { ToolSourceKind::Builtin }
    async fn execute(&self, input: &Value, ctx: &ToolContext) -> ToolResult<Value> {
        let path = need_param(input, "path", ParamType::String, Value::as_str)?;
        let content = need_param(input, "content", ParamType::String, Value::as_str)?;
        ctx.check_path(path)?;
        // Create parent dirs if needed (best-effort).
        if let Some(parent) = std::path::Path::new(path).parent() {
            if !parent.as_os_str().is_empty() {
                let _ = tokio::fs::create_dir_all(parent).await;
            }
        }
        tokio::fs::write(path, content).await?;
        Ok(serde_json::json!({ "written": content.len(), "path": path }))
    }
}

pub struct List;
#[async_trait]
impl Tool for List {
    fn name(&self) -> &str { "file.list" }
    fn description(&self) -> &str { "List entries in a directory (names only). Enforces the sandbox FS allow-list." }
    fn schema(&self) -> Schema {
        Schema::new().param(Parameter::required("path", ParamType::String))
    }
    fn source(&self) -> ToolSourceKind { ToolSourceKind::Builtin }
    async fn execute(&self, input: &Value, ctx: &ToolContext) -> ToolResult<Value> {
        let path = need_param(input, "path", ParamType::String, Value::as_str)?;
        ctx.check_path(path)?;
        let mut entries: Vec<String> = Vec::new();
        let mut rd = tokio::fs::read_dir(path).await?;
        while let Some(e) = rd.next_entry().await? {
            entries.push(e.file_name().to_string_lossy().to_string());
        }
        entries.sort();
        Ok(Value::Array(
            entries.into_iter().map(Value::String).collect(),
        ))
    }
}

pub struct Exists;
#[async_trait]
impl Tool for Exists {
    fn name(&self) -> &str { "file.exists" }
    fn description(&self) -> &str { "Whether a path exists. Enforces the sandbox FS allow-list." }
    fn schema(&self) -> Schema {
        Schema::new().param(Parameter::required("path", ParamType::String))
    }
    fn source(&self) -> ToolSourceKind { ToolSourceKind::Builtin }
    async fn execute(&self, input: &Value, ctx: &ToolContext) -> ToolResult<Value> {
        let path = need_param(input, "path", ParamType::String, Value::as_str)?;
        ctx.check_path(path)?;
        Ok(serde_json::json!(tokio::fs::metadata(path).await.is_ok()))
    }
}

pub fn all() -> Vec<ToolArc> {
    vec![arc(Read), arc(Write), arc(List), arc(Exists)]
}
