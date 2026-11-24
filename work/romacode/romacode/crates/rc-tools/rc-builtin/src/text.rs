//! `text.*` tools — string operations, no I/O. The default policy `allow`s
//! these (pure functions).

use rc_core::schema::{ParamType, Parameter, Schema};
use rc_core::tool::{ToolContext, ToolSourceKind, ToolArc};
use rc_core::{arc, Tool, ToolResult};
use async_trait::async_trait;
use serde_json::Value;

use crate::need_param;

macro_rules! str1 {
    ($struct:ident, $name:expr, $desc:expr, $body:expr) => {
        pub struct $struct;
        #[async_trait]
        impl Tool for $struct {
            fn name(&self) -> &str { $name }
            fn description(&self) -> &str { $desc }
            fn schema(&self) -> Schema {
                Schema::new().param(Parameter::required("s", ParamType::String))
            }
            fn source(&self) -> ToolSourceKind { ToolSourceKind::Builtin }
            async fn execute(&self, input: &Value, _ctx: &ToolContext) -> ToolResult<Value> {
                let s = need_param(input, "s", ParamType::String, Value::as_str)?;
                Ok(serde_json::json!(($body)(s)))
            }
        }
    };
}

str1!(Upper, "text.upper", "Uppercase a string.", |s: &str| s.to_uppercase());
str1!(Lower, "text.lower", "Lowercase a string.", |s: &str| s.to_lowercase());
str1!(Length, "text.length", "Character count of a string (Unicode-aware).", |s: &str| s.chars().count());
str1!(Reverse, "text.reverse", "Reverse a string (by char, not byte).", |s: &str| s.chars().rev().collect::<String>());
str1!(Trim, "text.trim", "Strip leading/trailing whitespace.", |s: &str| s.trim().to_string());

pub struct Replace;
#[async_trait]
impl Tool for Replace {
    fn name(&self) -> &str { "text.replace" }
    fn description(&self) -> &str { "Replace all occurrences of a substring." }
    fn schema(&self) -> Schema {
        Schema::new()
            .param(Parameter::required("s", ParamType::String))
            .param(Parameter::required("from", ParamType::String))
            .param(Parameter::required("to", ParamType::String))
    }
    fn source(&self) -> ToolSourceKind { ToolSourceKind::Builtin }
    async fn execute(&self, input: &Value, _ctx: &ToolContext) -> ToolResult<Value> {
        let s = need_param(input, "s", ParamType::String, Value::as_str)?;
        let from = need_param(input, "from", ParamType::String, Value::as_str)?;
        let to = need_param(input, "to", ParamType::String, Value::as_str)?;
        Ok(serde_json::json!(s.replace(from, to)))
    }
}

pub struct Split;
#[async_trait]
impl Tool for Split {
    fn name(&self) -> &str { "text.split" }
    fn description(&self) -> &str { "Split a string on a delimiter into an array." }
    fn schema(&self) -> Schema {
        Schema::new()
            .param(Parameter::required("s", ParamType::String))
            .param(Parameter::required("delim", ParamType::String))
    }
    fn source(&self) -> ToolSourceKind { ToolSourceKind::Builtin }
    async fn execute(&self, input: &Value, _ctx: &ToolContext) -> ToolResult<Value> {
        let s = need_param(input, "s", ParamType::String, Value::as_str)?;
        let delim = need_param(input, "delim", ParamType::String, Value::as_str)?;
        let parts: Vec<Value> = s.split(delim).map(Value::from).collect();
        Ok(Value::Array(parts))
    }
}

pub struct Join;
#[async_trait]
impl Tool for Join {
    fn name(&self) -> &str { "text.join" }
    fn description(&self) -> &str { "Join an array of strings with a delimiter." }
    fn schema(&self) -> Schema {
        Schema::new()
            .param(Parameter::required("parts", ParamType::Array(Box::new(ParamType::String))))
            .param(Parameter::required("delim", ParamType::String))
    }
    fn source(&self) -> ToolSourceKind { ToolSourceKind::Builtin }
    async fn execute(&self, input: &Value, _ctx: &ToolContext) -> ToolResult<Value> {
        let parts = need_param(
            input,
            "parts",
            ParamType::Array(Box::new(ParamType::String)),
            Value::as_array,
        )?;
        let delim = need_param(input, "delim", ParamType::String, Value::as_str)?;
        let strings: Vec<String> = parts
            .iter()
            .map(|v| v.as_str().unwrap_or("").to_string())
            .collect();
        Ok(serde_json::json!(strings.join(delim)))
    }
}

pub struct Contains;
#[async_trait]
impl Tool for Contains {
    fn name(&self) -> &str { "text.contains" }
    fn description(&self) -> &str { "Whether a string contains a substring." }
    fn schema(&self) -> Schema {
        Schema::new()
            .param(Parameter::required("s", ParamType::String))
            .param(Parameter::required("needle", ParamType::String))
    }
    fn source(&self) -> ToolSourceKind { ToolSourceKind::Builtin }
    async fn execute(&self, input: &Value, _ctx: &ToolContext) -> ToolResult<Value> {
        let s = need_param(input, "s", ParamType::String, Value::as_str)?;
        let needle = need_param(input, "needle", ParamType::String, Value::as_str)?;
        Ok(serde_json::json!(s.contains(needle)))
    }
}

pub fn all() -> Vec<ToolArc> {
    vec![
        arc(Upper), arc(Lower), arc(Length), arc(Reverse), arc(Trim),
        arc(Replace), arc(Split), arc(Join), arc(Contains),
    ]
}

#[cfg(test)]
mod tests {
    use super::*;
    use rc_core::Tool;

    #[tokio::test]
    async fn missing_params_error_not_panic() {
        // Schema-validated input with missing/wrong-shape params must be an
        // error result, never a panic.
        let ctx = ToolContext::default();
        assert!(Upper.execute(&serde_json::json!({}), &ctx).await.is_err());
        assert!(Replace.execute(&serde_json::json!({ "s": "a" }), &ctx).await.is_err());
        assert!(Split.execute(&serde_json::json!({ "s": "a", "delim": 3 }), &ctx).await.is_err());
        assert!(Join.execute(&serde_json::json!({ "parts": "nope", "delim": "," }), &ctx).await.is_err());
        assert!(Contains.execute(&serde_json::json!({ "s": "a" }), &ctx).await.is_err());
        // Sane inputs still work.
        let out = Upper.execute(&serde_json::json!({ "s": "hi" }), &ctx).await.unwrap();
        assert_eq!(out, "HI");
    }
}
