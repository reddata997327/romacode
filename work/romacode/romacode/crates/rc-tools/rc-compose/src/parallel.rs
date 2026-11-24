//! **Parallel** — run N tools concurrently and return their results as an
//! array. Each step's input is a template resolved against the composition's
//! input. Errors in individual steps become `{"error": "…"}` entries so one
//! failure doesn't abort the rest.
//!
//! Steps are spawned as tasks but never orphaned: each step shares the
//! caller's live cancellation flag ([`ToolContext::cancel`]), so when the
//! executor's timeout fires (or a parent cancels), every in-flight step
//! observes it, drops its work — which (with `kill_on_drop`) kills any
//! subprocess it had spawned — and the runner joins every task before
//! returning.

use crate::templating::{compose_kind, resolve};
use async_trait::async_trait;
use rc_core::error::{ToolError, ToolResult};
use rc_core::executor::Executor;
use rc_core::schema::Schema;
use rc_core::tool::{arc, Tool, ToolArc, ToolContext, ToolSourceKind};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::Duration;

/// Poll the shared cancellation flag until it fires.
async fn wait_cancelled(flag: Arc<AtomicBool>) {
    loop {
        if flag.load(Ordering::Relaxed) {
            return;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ParallelStep {
    pub tool: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub input: Option<Value>,
}

pub struct ParallelTool {
    pub name: String,
    pub description: String,
    pub schema: Schema,
    pub executor: Executor,
    pub steps: Vec<ParallelStep>,
}

impl ParallelTool {
    pub fn new(
        name: impl Into<String>,
        description: impl Into<String>,
        schema: Schema,
        executor: Executor,
        steps: Vec<ParallelStep>,
    ) -> Self {
        Self {
            name: name.into(),
            description: description.into(),
            schema,
            executor,
            steps,
        }
    }
    pub fn into_arc(self) -> ToolArc {
        arc(self)
    }
}

#[async_trait]
impl Tool for ParallelTool {
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
        compose_kind("parallel")
    }
    async fn execute(&self, input: &Value, ctx: &ToolContext) -> ToolResult<Value> {
        if ctx.is_cancelled() {
            return Err(ToolError::Other("parallel cancelled".into()));
        }
        // Resolve every step's input, then run all concurrently.
        let inputs: Vec<(String, Value)> = self
            .steps
            .iter()
            .map(|s| {
                let inp = match &s.input {
                    Some(t) => resolve(t, &Value::Null, input),
                    None => input.clone(),
                };
                (s.tool.clone(), inp)
            })
            .collect();
        let mut futs = Vec::new();
        for (tool, inp) in inputs {
            let exec = self.executor.clone();
            // Child context shares the parent's live cancellation flag, so a
            // timeout/cancel on the parent call reaches the spawned step.
            let child = ToolContext {
                cancelled: ctx.cancelled,
                cancel: ctx.cancel.clone(),
                ..ToolContext::default()
            };
            let cancel = ctx.cancel.clone();
            futs.push(tokio::spawn(async move {
                // Race the step against the shared flag: if the parent times
                // out or cancels, drop the in-flight step (killing its
                // subprocesses via kill_on_drop) instead of running to
                // completion detached.
                tokio::select! {
                    biased;
                    _ = wait_cancelled(cancel) => {
                        Err(ToolError::Other(format!("parallel step `{tool}` cancelled")))
                    }
                    r = exec.run_inner_ctx(&tool, &inp, &child) => r,
                }
            }));
        }
        // Join every spawned task — nothing is left detached. On the success
        // path each task has already finished; if cancellation fires while we
        // are still joining, the tasks self-abort via the flag (see above)
        // and their errors are reported as `{"error": …}` entries as usual.
        let mut results = Vec::with_capacity(futs.len());
        for h in futs {
            let r = h
                .await
                .map_err(|e| ToolError::ToolFailed {
                    tool: "parallel".into(),
                    inner: format!("task join: {e}"),
                })?;
            results.push(match r {
                Ok(v) => v,
                Err(e) => serde_json::json!({ "error": e.to_string() }),
            });
        }
        Ok(Value::Array(results))
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
    async fn fan_out_three() {
        let ex = exec();
        let p = ParallelTool::new(
            "compose.par",
            "run 3 in parallel",
            Schema::new().param(rc_core::schema::Parameter::required("x", rc_core::schema::ParamType::Number)),
            ex,
            vec![
                ParallelStep {
                    tool: "math.sqrt".into(),
                    input: Some(serde_json::json!({ "n": "{{input.x}}" })),
                },
                ParallelStep {
                    tool: "math.factorial".into(),
                    input: Some(serde_json::json!({ "n": "{{input.x}}" })),
                },
                ParallelStep {
                    tool: "math.multiply".into(),
                    input: Some(serde_json::json!({ "a": "{{input.x}}", "b": "{{input.x}}" })),
                },
            ],
        );
        let out = p
            .execute(&serde_json::json!({ "x": 5 }), &ToolContext::default())
            .await
            .unwrap();
        assert_eq!(out.as_array().unwrap().len(), 3);
        assert!((out[0].as_f64().unwrap() - 5.0_f64.sqrt()).abs() < 1e-9);
        assert_eq!(out[1], "120"); // 5! = 120 (returned as string by factorial)
        assert!((out[2].as_f64().unwrap() - 25.0).abs() < 1e-9);
    }

    #[tokio::test]
    async fn spawned_steps_observe_cancellation() {
        // Flip the shared flag while a slow step is running: the step must
        // bail out promptly via the flag (well before its 5s sleep ends)
        // instead of running to completion detached.
        let ex = exec();
        let p = ParallelTool::new(
            "compose.par.slow",
            "one slow step",
            Schema::new(),
            ex,
            vec![ParallelStep {
                tool: "time.sleep".into(),
                input: Some(serde_json::json!({ "ms": 5_000 })),
            }],
        );
        let ctx = ToolContext::default();
        let flag = ctx.cancel.clone();
        let flipper = tokio::spawn(async move {
            tokio::time::sleep(Duration::from_millis(100)).await;
            flag.store(true, Ordering::Relaxed);
        });
        let start = std::time::Instant::now();
        let out = p
            .execute(&serde_json::json!({}), &ctx)
            .await
            .unwrap();
        flipper.await.unwrap();
        let elapsed = start.elapsed();
        // The step reported cancellation as an error entry…
        let arr = out.as_array().unwrap();
        assert_eq!(arr.len(), 1);
        assert!(arr[0]["error"].as_str().unwrap().contains("cancelled"));
        // …and did NOT wait out the full 5s sleep.
        assert!(elapsed < std::time::Duration::from_secs(2), "took {elapsed:?}");
    }
}
