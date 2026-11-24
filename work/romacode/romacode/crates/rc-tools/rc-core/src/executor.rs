//! The [`Executor`] — the single point through which a tool is actually run.
//! It holds a [`Registry`], a [`PermissionPolicy`], and a [`Sandbox`], and for
//! every call it:
//!
//! 1. looks the tool up in the registry,
//! 2. asks the policy for a decision (`Allow` / `Ask` / `Deny`),
//! 3. validates the input against the tool's schema (filling defaults),
//! 4. enforces the sandbox (timeout, output cap),
//! 5. runs the tool's `execute`,
//! 6. clamps the output to the sandbox's size limit.
//!
//! The executor never prompts a human — `Ask` is surfaced as a decision the
//! caller (CLI, agent loop) must handle before calling `run` with
//! `approved = true`.

use crate::error::{ToolError, ToolResult};
use crate::permissions::{Permission, PermissionPolicy};
use crate::registry::Registry;
use crate::sandbox::Sandbox;
use crate::tool::ToolContext;
use serde_json::Value;
use std::sync::Arc;
use std::time::Duration;

#[derive(Clone)]
pub struct Executor {
    pub registry: Registry,
    pub policy: Arc<PermissionPolicy>,
    pub sandbox: Arc<Sandbox>,
}

impl Executor {
    pub fn new(registry: Registry, policy: PermissionPolicy, sandbox: Sandbox) -> Self {
        Self {
            registry,
            policy: Arc::new(policy),
            sandbox: Arc::new(sandbox),
        }
    }

    /// What would the policy decide for this tool? Callers use this to decide
    /// whether to prompt the user (`Ask`) before calling `run`.
    pub fn authorize(&self, name: &str) -> ToolResult<Permission> {
        let t = self.registry.get(name)?;
        Ok(self.policy.decide(t.as_ref()))
    }

    /// Run a tool by name. `approved` signals the caller already obtained
    /// human approval for an `Ask` decision.
    pub async fn run(
        &self,
        name: &str,
        input: &Value,
        approved: bool,
    ) -> ToolResult<Value> {
        let tool = self.registry.get(name)?;

        // 2. permission gate
        match self.policy.decide(tool.as_ref()) {
            Permission::Deny => {
                return Err(ToolError::PermissionDenied {
                    tool: name.to_string(),
                    reason: "denied by policy".to_string(),
                });
            }
            Permission::Ask if !approved => {
                return Err(ToolError::PermissionDenied {
                    tool: name.to_string(),
                    reason: "requires approval (Ask)".to_string(),
                });
            }
            _ => {}
        }

        // 3. validate input (fills defaults)
        let validated = crate::validator::validate(&tool.schema(), input)?;

        // 4+5. sandbox: timeout + execute. The context carries a *shared*
        // cancellation flag: when the timeout fires we flip it before
        // returning, so any sub-task the tool spawned (e.g. a parallel
        // fan-out) observes cancellation and stops, instead of running on
        // detached after the caller has already seen a Timeout.
        let ctx = ToolContext {
            correlation_id: name.to_string(),
            timeout_ms: self.sandbox.timeout_ms,
            cancelled: false,
            cancel: Arc::new(std::sync::atomic::AtomicBool::new(false)),
            allow_net: self.sandbox.allow_net,
            allow_fs_paths: self.sandbox.allow_fs_paths.clone(),
        };
        let fut = tool.execute(&validated, &ctx);
        let out = if self.sandbox.timeout_ms == 0 {
            fut.await?
        } else {
            match tokio::time::timeout(Duration::from_millis(self.sandbox.timeout_ms), fut).await {
                Ok(inner) => inner?,
                Err(_elapsed) => {
                    ctx.cancel();
                    return Err(ToolError::Timeout {
                        timeout_ms: self.sandbox.timeout_ms,
                    });
                }
            }
        };

        // 6. clamp output
        let mut o = out;
        self.sandbox.clamp_output(&mut o)?;
        Ok(o)
    }

    /// Run without the permission gate — used internally by composition, where
    /// the *outer* composed tool has already been authorized and the inner
    /// steps are trusted parts of the same call. The inner tool sees a default
    /// context (its own fresh cancellation flag).
    pub async fn run_inner(&self, name: &str, input: &Value) -> ToolResult<Value> {
        self.run_inner_ctx(name, input, &ToolContext::default()).await
    }

    /// Like [`Executor::run_inner`], but the inner tool inherits the caller's
    /// cancellation state: it sees the parent's `cancelled` snapshot and
    /// *shares the parent's live cancellation flag*, so a timeout or cancel
    /// on the parent call propagates into spawned sub-tasks. Composition
    /// tools that fan out (parallel) use this.
    pub async fn run_inner_ctx(
        &self,
        name: &str,
        input: &Value,
        parent_ctx: &ToolContext,
    ) -> ToolResult<Value> {
        let tool = self.registry.get(name)?;
        let validated = crate::validator::validate(&tool.schema(), input)?;
        let ctx = ToolContext {
            correlation_id: name.to_string(),
            timeout_ms: self.sandbox.timeout_ms,
            cancelled: parent_ctx.cancelled || parent_ctx.is_cancelled(),
            cancel: parent_ctx.cancel.clone(),
            allow_net: self.sandbox.allow_net,
            allow_fs_paths: self.sandbox.allow_fs_paths.clone(),
        };
        let fut = tool.execute(&validated, &ctx);
        // A step timing out produces an error result for that step only — it
        // must NOT flip the shared parent flag (that would cancel siblings).
        let out = if self.sandbox.timeout_ms == 0 {
            fut.await?
        } else {
            tokio::time::timeout(Duration::from_millis(self.sandbox.timeout_ms), fut)
                .await
                .map_err(|_| ToolError::Timeout {
                    timeout_ms: self.sandbox.timeout_ms,
                })??
        };
        let mut o = out;
        self.sandbox.clamp_output(&mut o)?;
        Ok(o)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::schema::{ParamType, Parameter, Schema};
    use crate::tool::{ToolContext, ToolSourceKind};
    use crate::Tool;
    use async_trait::async_trait;

    struct Add;
    #[async_trait]
    impl Tool for Add {
        fn name(&self) -> &str { "math.add" }
        fn description(&self) -> &str { "add two ints" }
        fn schema(&self) -> Schema {
            Schema::new()
                .param(Parameter::required("a", ParamType::Integer))
                .param(Parameter::required("b", ParamType::Integer))
        }
        fn source(&self) -> ToolSourceKind { ToolSourceKind::Builtin }
        async fn execute(&self, input: &Value, _: &ToolContext) -> ToolResult<Value> {
            let a = input["a"].as_i64().unwrap();
            let b = input["b"].as_i64().unwrap();
            Ok(serde_json::json!(a + b))
        }
    }

    #[tokio::test]
    async fn run_allow() {
        let r = Registry::new();
        r.register(crate::tool::arc(Add));
        let ex = Executor::new(r, PermissionPolicy::default(), Sandbox::default());
        // math.add is Allow in the default policy
        let out = ex.run("math.add", &serde_json::json!({ "a": 2, "b": 3 }), false).await.unwrap();
        assert_eq!(out, 5);
    }

    #[tokio::test]
    async fn run_ask_needs_approval() {
        let r = Registry::new();
        r.register(crate::tool::arc(Add));
        // force Ask for math.add via a policy with no allow rules
        let policy = PermissionPolicy {
            deny: vec![],
            ask: vec![crate::permissions::PermissionRule::new("*", Permission::Ask)],
            allow: vec![],
            default: Permission::Ask,
        };
        let ex = Executor::new(r, policy, Sandbox::default());
        let err = ex.run("math.add", &serde_json::json!({ "a": 1, "b": 1 }), false).await;
        assert!(err.is_err());
        // with approval it runs
        let out = ex.run("math.add", &serde_json::json!({ "a": 1, "b": 1 }), true).await.unwrap();
        assert_eq!(out, 2);
    }

    #[tokio::test]
    async fn validation_fills_defaults() {
        struct Echo;
        #[async_trait]
        impl Tool for Echo {
            fn name(&self) -> &str { "text.echo" }
            fn description(&self) -> &str { "" }
            fn schema(&self) -> Schema {
                Schema::new()
                    .param(Parameter::required("msg", ParamType::String))
                    .param(Parameter::optional("loud", ParamType::Boolean).with_default(serde_json::json!(false)))
            }
            fn source(&self) -> ToolSourceKind { ToolSourceKind::Builtin }
            async fn execute(&self, input: &Value, _: &ToolContext) -> ToolResult<Value> {
                let loud = input["loud"].as_bool().unwrap();
                let msg = input["msg"].as_str().unwrap();
                Ok(serde_json::json!(if loud { msg.to_uppercase() } else { msg.to_string() }))
            }
        }
        let r = Registry::new();
        r.register(crate::tool::arc(Echo));
        let ex = Executor::new(r, PermissionPolicy::default(), Sandbox::default());
        let out = ex.run("text.echo", &serde_json::json!({ "msg": "hi" }), false).await.unwrap();
        assert_eq!(out, "hi");
    }

    /// A tool that shells out like the real `ShellTool`/`rust.*` tools
    /// (`kill_on_drop(true)`), sleeping long enough to hit the timeout.
    struct SlowShell(std::path::PathBuf);
    #[async_trait]
    impl Tool for SlowShell {
        fn name(&self) -> &str { "slow.shell" }
        fn description(&self) -> &str { "sleeps, then drops a marker file" }
        fn schema(&self) -> Schema { Schema::new() }
        fn source(&self) -> ToolSourceKind { ToolSourceKind::Builtin }
        async fn execute(&self, _: &Value, _: &ToolContext) -> ToolResult<Value> {
            // Quote the marker path like ShellTool does — it contains
            // characters (e.g. parentheses from ThreadId debug output) that
            // are shell-special.
            let q = format!("'{}'", self.0.display().to_string().replace('\'', "'\\''"));
            let mut cmd = tokio::process::Command::new("sh");
            cmd.arg("-c")
                .arg(format!("sleep 2; touch {q}"));
            cmd.kill_on_drop(true);
            let out = cmd.output().await.map_err(|e| ToolError::Other(e.to_string()))?;
            Ok(serde_json::json!({ "exit_code": out.status.code() }))
        }
    }

    #[tokio::test]
    async fn timeout_kills_child_process_and_returns_timeout() {
        // H16: the executor's timeout must not leave the tool's child process
        // running detached. The child would drop a marker file after 2s if it
        // survived; the kill_on_drop'd child is killed at the 200ms timeout,
        // so the marker must never appear.
        let marker = std::env::temp_dir().join(format!(
            "rc-h16-alive-{}-{:?}",
            std::process::id(),
            std::thread::current().id()
        ));
        let r = Registry::new();
        r.register(crate::tool::arc(SlowShell(marker.clone())));
        let sandbox = Sandbox {
            timeout_ms: 200,
            ..Sandbox::default()
        };
        let ex = Executor::new(r, PermissionPolicy::default(), sandbox);
        let start = std::time::Instant::now();
        let res = ex.run("slow.shell", &serde_json::json!({}), true).await;
        // The timeout error comes back promptly.
        assert!(
            matches!(res, Err(ToolError::Timeout { timeout_ms: 200 })),
            "expected Timeout, got {res:?}"
        );
        assert!(start.elapsed() < std::time::Duration::from_secs(1));
        // Give a (hypothetically) orphaned child time to drop its marker…
        tokio::time::sleep(std::time::Duration::from_millis(2_500)).await;
        // …then assert it never did: the child was killed on drop.
        assert!(
            !marker.exists(),
            "child process outlived the executor timeout (orphan)"
        );
        std::fs::remove_file(&marker).ok();
    }
}
