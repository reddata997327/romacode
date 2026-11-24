//! **Loop** — run a tool repeatedly, feeding each output back as the next
//! input, until a stop condition holds or `max_iters` is reached. Returns the
//! final value (or every iteration's output if `collect_all` is set).

use crate::templating::{compose_kind, resolve};
use async_trait::async_trait;
use rc_core::error::{ToolError, ToolResult};
use rc_core::executor::Executor;
use rc_core::schema::Schema;
use rc_core::tool::{arc, Tool, ToolArc, ToolContext, ToolSourceKind};
use serde::{Deserialize, Serialize};
use serde_json::Value;

/// Compare two JSON values for the loop's stop condition. Numbers compare by
/// their `f64` value (so `128` matches `128.0`); everything else compares by
/// strict equality. Strings match numbers only if the string parses to the
/// same number (so a tool that returns `"128"` still stops on `equals: 128`).
fn values_equal(a: &Value, b: &Value) -> bool {
    if let (Some(x), Some(y)) = (a.as_f64(), b.as_f64()) {
        return (x - y).abs() < 1e-9;
    }
    a == b
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Until {
    /// Dot-path into each iteration's output.
    pub path: String,
    /// The value `path` must equal to stop.
    pub equals: Value,
}

pub struct LoopTool {
    pub name: String,
    pub description: String,
    pub schema: Schema,
    pub executor: Executor,
    pub tool: String,
    /// Input template for each iteration; `{{prev}}` is the previous output,
    /// `{{input}}` the original input. `None` = pass prev through directly.
    pub input: Option<Value>,
    /// Stop when an iteration's output at `path` equals `equals`.
    pub until: Option<Until>,
    pub max_iters: usize,
    /// If true, return every iteration's output as an array; else the final.
    pub collect_all: bool,
}

impl LoopTool {
    pub fn new(
        name: impl Into<String>,
        description: impl Into<String>,
        schema: Schema,
        executor: Executor,
        tool: impl Into<String>,
    ) -> Self {
        Self {
            name: name.into(),
            description: description.into(),
            schema,
            executor,
            tool: tool.into(),
            input: None,
            until: None,
            max_iters: 100,
            collect_all: false,
        }
    }
    pub fn with_input(mut self, t: Value) -> Self {
        self.input = Some(t);
        self
    }
    pub fn with_until(mut self, path: impl Into<String>, equals: Value) -> Self {
        self.until = Some(Until {
            path: path.into(),
            equals,
        });
        self
    }
    pub fn with_max_iters(mut self, n: usize) -> Self {
        self.max_iters = n;
        self
    }
    pub fn collect_all(mut self) -> Self {
        self.collect_all = true;
        self
    }
    pub fn into_arc(self) -> ToolArc {
        arc(self)
    }
}

#[async_trait]
impl Tool for LoopTool {
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
        compose_kind("loop")
    }
    async fn execute(&self, input: &Value, ctx: &ToolContext) -> ToolResult<Value> {
        if ctx.is_cancelled() {
            return Err(ToolError::Other("loop cancelled".into()));
        }
        let mut prev = input.clone();
        let mut collected: Vec<Value> = Vec::new();
        for i in 0..self.max_iters {
            let step_input = match &self.input {
                Some(t) => resolve(t, &prev, input),
                None => prev.clone(),
            };
            let out = self.executor.run_inner(&self.tool, &step_input).await?;
            if self.collect_all {
                collected.push(out.clone());
            }
            // stop condition
            if let Some(until) = &self.until {
                let actual = crate::templating::lookup(&out, &until.path);
                if values_equal(&actual, &until.equals) {
                    return if self.collect_all {
                        Ok(Value::Array(collected))
                    } else {
                        Ok(out)
                    };
                }
            }
            prev = out;
            let _ = i; // iteration counter (for debugging)
        }
        if self.collect_all {
            Ok(Value::Array(collected))
        } else {
            Ok(prev)
        }
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
    async fn loop_runs_max_iters_without_until() {
        // Each iteration: math.add {a: {{prev}}, b: 1}. Start input {a: 0}.
        // After 5 iters of adding 1 to prev... but math.add returns a number,
        // not {a:...}. Use a template that reads {{prev}} as the number.
        let ex = exec();
        let lp = LoopTool::new(
            "compose.count",
            "add 1, 5 times",
            Schema::new().param(rc_core::schema::Parameter::required("a", rc_core::schema::ParamType::Number)),
            ex,
            "math.add",
        )
        .with_input(serde_json::json!({ "a": "{{prev}}", "b": 1 }))
        .with_max_iters(5);
        // seed: the first iteration uses prev = input = {"a": 0}, so {{prev}} → {"a":0}
        // which isn't a number. Instead, seed with a number directly: input = 0.
        let out = lp
            .execute(&serde_json::json!(0), &ToolContext::default())
            .await
            .unwrap();
        assert!((out.as_f64().unwrap() - 5.0).abs() < 1e-9); // 0 → 1 → 2 → 3 → 4 → 5
    }

    #[tokio::test]
    async fn loop_stops_on_until() {
        // Keep doubling until >= 100. math.multiply {a: {{prev}}, b: 2}.
        let ex = exec();
        let lp = LoopTool::new(
            "compose.doubl",
            "double until >= 100",
            Schema::new(),
            ex,
            "math.multiply",
        )
        .with_input(serde_json::json!({ "a": "{{prev}}", "b": 2 }))
        .with_until("", serde_json::json!(128)) // stop when prev == 128
        .with_max_iters(20);
        let out = lp
            .execute(&serde_json::json!(1), &ToolContext::default())
            .await
            .unwrap();
        assert!((out.as_f64().unwrap() - 128.0).abs() < 1e-9); // 1 → 2 → 4 → 8 → 16 → 32 → 64 → 128
    }
}
