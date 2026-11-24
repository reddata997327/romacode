//! Tools — the Rust mirror of `langchain_core.tools` (`BaseTool`).
//!
//! A tool is a name + description + JSON-schema parameters + an async executor.
//! The schema is sent to the model (OpenAI function calling); the executor is
//! what actually runs when the model selects the tool.

use async_trait::async_trait;
use serde_json::Value;
use std::sync::Arc;

use crate::errors::{LangchainError, Result};
use crate::messages::ToolSpec;

/// Async executor backing a [`Tool`].
#[async_trait]
pub trait ToolFn: Send + Sync {
    /// Run the tool with the model-supplied arguments.
    async fn call(&self, args: Value) -> Result<String>;
}

/// A callable tool the agent can invoke — `BaseTool` + `@tool` decorator equivalent.
#[derive(Clone)]
pub struct Tool {
    pub spec: ToolSpec,
    executor: Arc<dyn ToolFn>,
}

impl std::fmt::Debug for Tool {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Tool")
            .field("name", &self.spec.name)
            .field("description", &self.spec.description)
            .finish_non_exhaustive()
    }
}

impl Tool {
    /// Assemble a tool from a spec and an executor.
    pub fn new(spec: ToolSpec, executor: Arc<dyn ToolFn>) -> Self {
        Self { spec, executor }
    }

    /// Describe the tool for the model.
    pub fn name(&self) -> &str {
        &self.spec.name
    }

    /// JSON Schema of the arguments.
    pub fn parameters(&self) -> &Value {
        &self.spec.parameters
    }

    /// Run the tool.
    pub async fn call(&self, args: Value) -> Result<String> {
        self.executor.call(args).await
    }
}

/// Build a [`Tool`] from an async closure — the `@tool` decorator equivalent.
///
/// ```ignore
/// let echo = tool_fn("echo", "Repeat the input", json!({
///     "type": "object",
///     "properties": {"text": {"type": "string"}},
///     "required": ["text"]
/// }), |args| async move {
///     let text = args["text"].as_str().unwrap_or("").to_string();
///     Ok(text)
/// });
/// ```
pub fn tool_fn<F, Fut>(
    name: impl Into<String>,
    description: impl Into<String>,
    parameters: Value,
    f: F,
) -> Tool
where
    F: Fn(Value) -> Fut + Send + Sync + 'static,
    Fut: std::future::Future<Output = Result<String>> + Send + 'static,
{
    let spec = ToolSpec {
        name: name.into(),
        description: description.into(),
        parameters,
    };
    Tool::new(spec, Arc::new(ClosureTool { f }))
}

/// Adapter turning an async closure into a [`ToolFn`].
struct ClosureTool<F> {
    f: F,
}

#[async_trait]
impl<F, Fut> ToolFn for ClosureTool<F>
where
    F: Fn(Value) -> Fut + Send + Sync,
    Fut: std::future::Future<Output = Result<String>> + Send,
{
    async fn call(&self, args: Value) -> Result<String> {
        (self.f)(args).await
    }
}

/// Validate `args` against the very small subset of JSON Schema we rely on:
/// required keys + top-level types. Full JSON Schema validation is intentionally
/// out of scope for a core rebuild.
pub fn validate_args(spec: &ToolSpec, args: &Value) -> Result<()> {
    if !args.is_object() {
        return Err(LangchainError::ToolExecution(
            spec.name.clone(),
            "arguments must be a JSON object".into(),
        ));
    }
    let required = spec
        .parameters
        .get("required")
        .and_then(|r| r.as_array())
        .cloned()
        .unwrap_or_default();
    for req in required {
        let key = req.as_str().unwrap_or_default();
        if key.is_empty() {
            continue;
        }
        if args.get(key).is_none() {
            return Err(LangchainError::ToolExecution(
                spec.name.clone(),
                format!("missing required argument `{key}`"),
            ));
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[tokio::test]
    async fn closure_tool_runs() {
        let t = tool_fn("echo", "echo text", json!({"type": "object"}), |args| async move {
            Ok(args["text"].as_str().unwrap_or("").to_uppercase())
        });
        let out = t.call(json!({"text": "hello"})).await.unwrap();
        assert_eq!(out, "HELLO");
    }

    #[test]
    fn validate_required_keys() {
        let spec = ToolSpec {
            name: "t".into(),
            description: "".into(),
            parameters: json!({"type": "object", "required": ["path"]}),
        };
        assert!(validate_args(&spec, &json!({"path": "/tmp"})).is_ok());
        assert!(validate_args(&spec, &json!({})).is_err());
        assert!(validate_args(&spec, &json!([1])).is_err());
    }
}
