//! **Branch** — conditionally route to one of N tools. Each branch carries a
//! condition (`path` into the input that must `equal` a value) and a tool to
//! run if it matches; the first match wins. A branch with no condition is the
//! fallback. The selected tool's output is the branch tool's output.

use crate::templating::{compose_kind, resolve};
use async_trait::async_trait;
use rc_core::error::ToolResult;
use rc_core::executor::Executor;
use rc_core::schema::Schema;
use rc_core::tool::{arc, Tool, ToolArc, ToolContext, ToolSourceKind};
use serde::{Deserialize, Serialize};
use serde_json::Value;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BranchArm {
    /// Dot-path into the input to test (e.g. "op"). Empty/None = always match
    /// (the fallback arm).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub path: Option<String>,
    /// The value `path` must equal for this arm to fire. Ignored when `path`
    /// is empty (fallback arm).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub equals: Option<Value>,
    /// The tool to run if this arm fires.
    pub tool: String,
    /// Optional input template for the selected tool.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub input: Option<Value>,
}

pub struct BranchTool {
    pub name: String,
    pub description: String,
    pub schema: Schema,
    pub executor: Executor,
    pub arms: Vec<BranchArm>,
}

impl BranchTool {
    pub fn new(
        name: impl Into<String>,
        description: impl Into<String>,
        schema: Schema,
        executor: Executor,
        arms: Vec<BranchArm>,
    ) -> Self {
        Self {
            name: name.into(),
            description: description.into(),
            schema,
            executor,
            arms,
        }
    }
    pub fn into_arc(self) -> ToolArc {
        arc(self)
    }

    fn select(&self, input: &Value) -> Option<&BranchArm> {
        for arm in &self.arms {
            let matches = match (&arm.path, &arm.equals) {
                (Some(path), Some(expected)) => {
                    crate::templating::lookup(input, path) == *expected
                }
                // No path → fallback arm.
                _ => true,
            };
            if matches {
                return Some(arm);
            }
        }
        None
    }
}

#[async_trait]
impl Tool for BranchTool {
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
        compose_kind("branch")
    }
    async fn execute(&self, input: &Value, ctx: &ToolContext) -> ToolResult<Value> {
        if ctx.is_cancelled() {
            return Err(rc_core::error::ToolError::Other("branch cancelled".into()));
        }
        let arm = self.select(input).ok_or_else(|| {
            rc_core::error::ToolError::Other(format!(
                "no branch arm matched input {}",
                input
            ))
        })?;
        let step_input = match &arm.input {
            Some(t) => resolve(t, &Value::Null, input),
            None => input.clone(),
        };
        self.executor.run_inner(&arm.tool, &step_input).await
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use rc_core::permissions::PermissionPolicy;
    use rc_core::sandbox::Sandbox;
    use rc_core::Registry;

    fn exec() -> Executor {
        let r = Registry::new();
        r.register_all(rc_builtin::all_tools());
        Executor::new(r, PermissionPolicy::default(), Sandbox::default())
    }

    #[tokio::test]
    async fn routes_on_op_field() {
        let ex = exec();
        let b = BranchTool::new(
            "compose.calc",
            "add or multiply",
            Schema::new()
                .param(rc_core::schema::Parameter::required("op", rc_core::schema::ParamType::String))
                .param(rc_core::schema::Parameter::required("a", rc_core::schema::ParamType::Number))
                .param(rc_core::schema::Parameter::required("b", rc_core::schema::ParamType::Number)),
            ex,
            vec![
                BranchArm {
                    path: Some("op".into()),
                    equals: Some(serde_json::json!("add")),
                    tool: "math.add".into(),
                    input: Some(serde_json::json!({ "a": "{{input.a}}", "b": "{{input.b}}" })),
                },
                BranchArm {
                    path: Some("op".into()),
                    equals: Some(serde_json::json!("mul")),
                    tool: "math.multiply".into(),
                    input: Some(serde_json::json!({ "a": "{{input.a}}", "b": "{{input.b}}" })),
                },
                BranchArm {
                    path: None,
                    equals: None,
                    tool: "math.subtract".into(),
                    input: Some(serde_json::json!({ "a": "{{input.a}}", "b": "{{input.b}}" })),
                },
            ],
        );
        let add = b
            .execute(&serde_json::json!({ "op": "add", "a": 2, "b": 3 }), &ToolContext::default())
            .await
            .unwrap();
        assert!((add.as_f64().unwrap() - 5.0).abs() < 1e-9);
        let mul = b
            .execute(&serde_json::json!({ "op": "mul", "a": 2, "b": 3 }), &ToolContext::default())
            .await
            .unwrap();
        assert!((mul.as_f64().unwrap() - 6.0).abs() < 1e-9);
        let fallback = b
            .execute(&serde_json::json!({ "op": "wat", "a": 10, "b": 3 }), &ToolContext::default())
            .await
            .unwrap();
        assert!((fallback.as_f64().unwrap() - 7.0).abs() < 1e-9); // subtract fallback
    }
}
