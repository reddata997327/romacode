//! **Pipeline** — chain tools so each step's output feeds the next. A step may
//! take the previous output directly (no template) or take a template that
//! references `{{prev}}` / `{{input}}`. The pipeline is itself a `Tool`, so
//! pipelines can be steps in other pipelines.

use crate::templating::{compose_kind, resolve};
use async_trait::async_trait;
use rc_core::error::{ToolError, ToolResult};
use rc_core::executor::Executor;
use rc_core::schema::Schema;
use rc_core::tool::{arc, Tool, ToolArc, ToolContext, ToolSourceKind};
use serde::{Deserialize, Serialize};
use serde_json::Value;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PipelineStep {
    /// The registered tool name to run.
    pub tool: String,
    /// Optional input template. If `None`, the step receives the previous
    /// step's output directly (the first step receives the pipeline input).
    /// If `Some`, the template is resolved against `prev` + `input`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub input: Option<Value>,
}

pub struct PipelineTool {
    pub name: String,
    pub description: String,
    pub schema: Schema,
    pub executor: Executor,
    pub steps: Vec<PipelineStep>,
}

impl PipelineTool {
    pub fn new(
        name: impl Into<String>,
        description: impl Into<String>,
        schema: Schema,
        executor: Executor,
        steps: Vec<PipelineStep>,
    ) -> Self {
        Self {
            name: name.into(),
            description: description.into(),
            schema,
            executor,
            steps,
        }
    }
    /// Wrap as a `ToolArc` ready to register.
    pub fn into_arc(self) -> ToolArc {
        arc(self)
    }
}

#[async_trait]
impl Tool for PipelineTool {
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
        compose_kind("pipeline")
    }
    async fn execute(&self, input: &Value, ctx: &ToolContext) -> ToolResult<Value> {
        if ctx.is_cancelled() {
            return Err(rc_core::error::ToolError::Other("pipeline cancelled".into()));
        }
        if self.steps.is_empty() {
            return Err(rc_core::error::ToolError::Other(
                "pipeline has no steps".into(),
            ));
        }
        let mut prev: Value = Value::Null;
        for (i, step) in self.steps.iter().enumerate() {
            let step_input = if i == 0 {
                // First step: template against prev=Null, or pass input directly.
                match &step.input {
                    Some(t) => resolve(t, &Value::Null, input),
                    None => input.clone(),
                }
            } else {
                match &step.input {
                    Some(t) => resolve(t, &prev, input),
                    None => prev.clone(),
                }
            };
            let out = self.executor.run_inner(&step.tool, &step_input).await?;
            prev = out;
        }
        Ok(prev)
    }
}

/// Convenience builder: a pipeline where each step takes the previous step's
/// output directly (the shell-pipe model). Errors on an empty step list
/// (instead of panicking on `steps[0]`).
pub fn pipe(executor: Executor, name: &str, steps: &[&str]) -> ToolResult<PipelineTool> {
    if steps.is_empty() {
        return Err(ToolError::Validation(
            "pipe() needs at least one step".to_string(),
        ));
    }
    let pipeline_steps: Vec<PipelineStep> = steps
        .iter()
        .map(|s| PipelineStep {
            tool: s.to_string(),
            input: None,
        })
        .collect();
    // The pipeline's input schema is the first step's schema (if any), else
    // a permissive empty schema.
    let schema = executor
        .registry
        .get(steps[0])
        .map(|t| t.schema())
        .unwrap_or_default();
    Ok(PipelineTool::new(
        name,
        format!("pipeline: {}", steps.join(" → ")),
        schema,
        executor,
        pipeline_steps,
    ))
}

#[cfg(test)]
mod tests {
    use super::*;
    use rc_core::permissions::PermissionPolicy;
    use rc_core::sandbox::Sandbox;
    use rc_core::Registry;

    /// Build an executor with the builtin math+text tools registered.
    fn exec() -> Executor {
        let r = Registry::new();
        r.register_all(rc_builtin::all_tools());
        Executor::new(r, PermissionPolicy::default(), Sandbox::default())
    }

    #[tokio::test]
    async fn pipe_chains_text_then_math() {
        // text.length({s: "hello"}) → 5 ; math.sqrt({n: {{prev}}}) → √5.
        // Uses templates because the tools' shapes don't match for bare piping.
        let ex = exec();
        let p = PipelineTool::new(
            "compose.demo",
            "length then sqrt",
            rc_core::schema::Schema::new()
                .param(rc_core::schema::Parameter::required("s", rc_core::schema::ParamType::String)),
            ex,
            vec![
                PipelineStep {
                    tool: "text.length".into(),
                    input: Some(serde_json::json!({ "s": "{{input.s}}" })),
                },
                PipelineStep {
                    tool: "math.sqrt".into(),
                    input: Some(serde_json::json!({ "n": "{{prev}}" })),
                },
            ],
        );
        let out = p
            .execute(&serde_json::json!({ "s": "hello" }), &ToolContext::default())
            .await
            .unwrap();
        let v = out.as_f64().unwrap();
        assert!((v - 5.0_f64.sqrt()).abs() < 1e-9);
    }

    #[tokio::test]
    async fn pipeline_with_input_template() {
        // Step 1: math.multiply {a: {{input.x}}, b: {{input.x}}}  → x*x
        // Step 2: math.add {a: {{prev}}, b: {{input.y}}}           → x*x + y
        let ex = exec();
        let p = PipelineTool::new(
            "compose.template",
            "x² + y",
            rc_core::schema::Schema::new()
                .param(rc_core::schema::Parameter::required("x", rc_core::schema::ParamType::Number))
                .param(rc_core::schema::Parameter::required("y", rc_core::schema::ParamType::Number)),
            ex,
            vec![
                PipelineStep {
                    tool: "math.multiply".into(),
                    input: Some(serde_json::json!({ "a": "{{input.x}}", "b": "{{input.x}}" })),
                },
                PipelineStep {
                    tool: "math.add".into(),
                    input: Some(serde_json::json!({ "a": "{{prev}}", "b": "{{input.y}}" })),
                },
            ],
        );
        let out = p
            .execute(&serde_json::json!({ "x": 3, "y": 4 }), &ToolContext::default())
            .await
            .unwrap();
        assert!((out.as_f64().unwrap() - 13.0).abs() < 1e-9); // 3*3 + 4
    }

    #[test]
    fn pipe_rejects_empty_steps() {
        // Empty step lists must be a proper error, not a panic on steps[0].
        let msg = match pipe(exec(), "compose.empty", &[]) {
            Err(e) => e.to_string(),
            Ok(_) => panic!("expected pipe() to reject an empty step list"),
        };
        assert!(msg.contains("at least one step"));
    }

    #[tokio::test]
    async fn pipe_single_step_works() {
        let ex = exec();
        let p = pipe(ex, "compose.single", &["text.upper"]).unwrap();
        let out = p
            .execute(&serde_json::json!({ "s": "hi" }), &ToolContext::default())
            .await
            .unwrap();
        assert_eq!(out, "HI");
    }
}
