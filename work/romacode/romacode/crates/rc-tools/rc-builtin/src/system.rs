//! `system.*` tools — environment introspection. The default policy *denies*
//! `system.*` (it can leak secrets and host info); a user must explicitly
//! allow a name or pattern to use these.

use rc_core::schema::{ParamType, Parameter, Schema};
use rc_core::tool::{ToolContext, ToolSourceKind, ToolArc};
use rc_core::{arc, Tool, ToolResult};
use async_trait::async_trait;
use serde_json::Value;

use crate::need_param;

pub struct Env;
#[async_trait]
impl Tool for Env {
    fn name(&self) -> &str { "system.env" }
    fn description(&self) -> &str {
        "Read one environment variable by name (returns null if unset). WARNING: can leak secrets — denied by default."
    }
    fn schema(&self) -> Schema {
        Schema::new().param(Parameter::required("name", ParamType::String))
    }
    fn source(&self) -> ToolSourceKind { ToolSourceKind::Builtin }
    async fn execute(&self, input: &Value, _ctx: &ToolContext) -> ToolResult<Value> {
        let name = need_param(input, "name", ParamType::String, Value::as_str)?;
        let val = std::env::var(name).ok();
        Ok(match val {
            Some(v) => Value::String(v),
            None => Value::Null,
        })
    }
}

pub struct Hostname;
#[async_trait]
impl Tool for Hostname {
    fn name(&self) -> &str { "system.hostname" }
    fn description(&self) -> &str { "The machine's hostname." }
    fn schema(&self) -> Schema { Schema::new() }
    fn source(&self) -> ToolSourceKind { ToolSourceKind::Builtin }
    async fn execute(&self, _input: &Value, _ctx: &ToolContext) -> ToolResult<Value> {
        let h = hostname().unwrap_or_else(|| "unknown".into());
        Ok(Value::String(h))
    }
}

pub struct Cwd;
#[async_trait]
impl Tool for Cwd {
    fn name(&self) -> &str { "system.cwd" }
    fn description(&self) -> &str { "The current working directory." }
    fn schema(&self) -> Schema { Schema::new() }
    fn source(&self) -> ToolSourceKind { ToolSourceKind::Builtin }
    async fn execute(&self, _input: &Value, _ctx: &ToolContext) -> ToolResult<Value> {
        let c = std::env::current_dir()?
            .to_string_lossy()
            .to_string();
        Ok(Value::String(c))
    }
}

fn hostname() -> Option<String> {
    // Prefer /etc/hostname, fall back to HOSTNAME env, else the uname nodename.
    if let Ok(s) = std::fs::read_to_string("/etc/hostname") {
        return Some(s.trim().to_string());
    }
    if let Ok(h) = std::env::var("HOSTNAME") {
        return Some(h);
    }
    None
}

pub fn all() -> Vec<ToolArc> {
    vec![arc(Env), arc(Hostname), arc(Cwd)]
}
