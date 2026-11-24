//! `data.*` tools — JSON/value manipulation, no I/O. Default policy `allow`s
//! these (pure functions).

use rc_core::schema::{ParamType, Parameter, Schema};
use rc_core::tool::{ToolContext, ToolSourceKind, ToolArc};
use rc_core::{arc, Tool, ToolResult};
use async_trait::async_trait;
use serde_json::Value;

use crate::need_param;

pub struct JsonPath;
#[async_trait]
impl Tool for JsonPath {
    fn name(&self) -> &str { "data.json_path" }
    fn description(&self) -> &str {
        "Look up a dot-path in a JSON value (e.g. 'a.b.0' on {\"a\":{\"b\":[1,2]}} → 1). Returns null if not found."
    }
    fn schema(&self) -> Schema {
        Schema::new()
            .param(Parameter::required("value", ParamType::Any))
            .param(Parameter::required("path", ParamType::String))
    }
    fn source(&self) -> ToolSourceKind { ToolSourceKind::Builtin }
    async fn execute(&self, input: &Value, _ctx: &ToolContext) -> ToolResult<Value> {
        let mut current = input.get("value").cloned().unwrap_or(Value::Null);
        let path = need_param(input, "path", ParamType::String, Value::as_str)?;
        for seg in path.split('.') {
            if seg.is_empty() {
                continue;
            }
            current = match &current {
                Value::Object(map) => map.get(seg).cloned().unwrap_or(Value::Null),
                Value::Array(arr) => seg
                    .parse::<usize>()
                    .ok()
                    .and_then(|i| arr.get(i).cloned())
                    .unwrap_or(Value::Null),
                _ => Value::Null,
            };
        }
        Ok(current)
    }
}

pub struct JsonPretty;
#[async_trait]
impl Tool for JsonPretty {
    fn name(&self) -> &str { "data.json_pretty" }
    fn description(&self) -> &str { "Pretty-print a JSON value (2-space indent)." }
    fn schema(&self) -> Schema {
        Schema::new().param(Parameter::required("value", ParamType::Any))
    }
    fn source(&self) -> ToolSourceKind { ToolSourceKind::Builtin }
    async fn execute(&self, input: &Value, _ctx: &ToolContext) -> ToolResult<Value> {
        let v = input.get("value").cloned().unwrap_or(Value::Null);
        Ok(Value::String(serde_json::to_string_pretty(&v)?))
    }
}

pub struct JsonMinify;
#[async_trait]
impl Tool for JsonMinify {
    fn name(&self) -> &str { "data.json_minify" }
    fn description(&self) -> &str { "Minify a JSON value to one line." }
    fn schema(&self) -> Schema {
        Schema::new().param(Parameter::required("value", ParamType::Any))
    }
    fn source(&self) -> ToolSourceKind { ToolSourceKind::Builtin }
    async fn execute(&self, input: &Value, _ctx: &ToolContext) -> ToolResult<Value> {
        let v = input.get("value").cloned().unwrap_or(Value::Null);
        Ok(Value::String(serde_json::to_string(&v)?))
    }
}

pub struct SortArray;
#[async_trait]
impl Tool for SortArray {
    fn name(&self) -> &str { "data.sort_array" }
    fn description(&self) -> &str { "Sort a JSON array of scalars ascending. Non-arrays pass through unchanged." }
    fn schema(&self) -> Schema {
        Schema::new().param(Parameter::required("array", ParamType::Array(Box::new(ParamType::Any))))
    }
    fn source(&self) -> ToolSourceKind { ToolSourceKind::Builtin }
    async fn execute(&self, input: &Value, _ctx: &ToolContext) -> ToolResult<Value> {
        let mut arr = input["array"].as_array().cloned().unwrap_or_default();
        arr.sort_by(|a, b| {
            // Compare by JSON value: numbers by magnitude, strings lexically,
            // else by their serialized form.
            match (a, b) {
                (Value::Number(x), Value::Number(y)) => {
                    let xn = x.as_f64().unwrap_or(0.0);
                    let yn = y.as_f64().unwrap_or(0.0);
                    xn.partial_cmp(&yn).unwrap_or(std::cmp::Ordering::Equal)
                }
                (Value::String(x), Value::String(y)) => x.cmp(y),
                _ => a.to_string().cmp(&b.to_string()),
            }
        });
        Ok(Value::Array(arr))
    }
}

pub struct Dedupe;
#[async_trait]
impl Tool for Dedupe {
    fn name(&self) -> &str { "data.dedupe" }
    fn description(&self) -> &str { "Remove duplicate scalars from a JSON array, preserving first-seen order." }
    fn schema(&self) -> Schema {
        Schema::new().param(Parameter::required("array", ParamType::Array(Box::new(ParamType::Any))))
    }
    fn source(&self) -> ToolSourceKind { ToolSourceKind::Builtin }
    async fn execute(&self, input: &Value, _ctx: &ToolContext) -> ToolResult<Value> {
        let arr = need_param(
            input,
            "array",
            ParamType::Array(Box::new(ParamType::Any)),
            Value::as_array,
        )?;
        let mut seen: Vec<String> = Vec::new();
        let mut out: Vec<Value> = Vec::new();
        for v in arr {
            let key = v.to_string();
            if !seen.contains(&key) {
                seen.push(key);
                out.push(v.clone());
            }
        }
        Ok(Value::Array(out))
    }
}

pub fn all() -> Vec<ToolArc> {
    vec![arc(JsonPath), arc(JsonPretty), arc(JsonMinify), arc(SortArray), arc(Dedupe)]
}

#[cfg(test)]
mod tests {
    use super::*;
    use rc_core::Tool;

    #[tokio::test]
    async fn missing_params_error_not_panic() {
        let ctx = ToolContext::default();
        assert!(JsonPath.execute(&serde_json::json!({ "value": 1 }), &ctx).await.is_err());
        assert!(Dedupe.execute(&serde_json::json!({ "array": "nope" }), &ctx).await.is_err());
        // Sane inputs still work.
        let out = Dedupe
            .execute(&serde_json::json!({ "array": [1, 1, 2] }), &ctx)
            .await
            .unwrap();
        assert_eq!(out, serde_json::json!([1, 2]));
    }
}
