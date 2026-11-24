//! `rust.*` tools — the Rust language pack. Eight developer-workflow tools,
//! each a first-class [`rc_core::Tool`], that wrap the Rust toolchain
//! (`cargo`, `rustc`, `rustfmt`) behind the framework's sandbox + permission
//! gate. They are side-effecting (they spawn subprocesses, touch the
//! filesystem), so the default permission policy leaves them at `Ask` — the
//! caller must approve before the executor will run them.
//!
//! | tool | what it runs |
//! |------|--------------|
//! | `rust.toolchain`   | `rustc --version` + `cargo --version` |
//! | `rust.cargo_check` | `cargo check` |
//! | `rust.cargo_build` | `cargo build` |
//! | `rust.cargo_test`  | `cargo test` |
//! | `rust.cargo_fmt`   | `cargo fmt` (or `--check`) |
//! | `rust.cargo_clippy`| `cargo clippy` |
//! | `rust.cargo_doc`   | `cargo doc --no-deps` |
//! | `rust.fmt_file`    | `rustfmt <path>` |
//!
//! Every subprocess tool returns the same shape the `cli`/`declarative` sources
//! produce: `{ "exit_code": int, "stdout": str, "stderr": str }`, so a
//! composed pipeline can treat a `rust.*` step exactly like any other.

use async_trait::async_trait;
use rc_core::error::{ToolError, ToolResult};
use rc_core::schema::{ParamType, Parameter, Schema};
use rc_core::tool::{arc, Tool, ToolArc, ToolContext, ToolSourceKind};
use serde_json::Value;
use std::process::Stdio;

/// Run a command and return the standard `{exit_code, stdout, stderr}` JSON.
/// Honours `ctx.is_cancelled()` (returns early without spawning) so a parent
/// can abort a long `cargo build`; kills the child when the future is dropped
/// (executor timeout / cancellation) so no build process is left running
/// detached. Spawn failures (binary not on `$PATH`) surface as
/// `ToolError::ToolFailed` rather than panicking.
async fn run(program: &str, args: &[&str], cwd: Option<&str>, ctx: &ToolContext) -> ToolResult<Value> {
    if ctx.is_cancelled() {
        return Err(ToolError::Other("cancelled before rust tool".to_string()));
    }
    let mut cmd = tokio::process::Command::new(program);
    cmd.args(args);
    // A timed-out or cancelled tool call must not leave `cargo build` etc.
    // running as an orphan: the child dies when this future is dropped.
    cmd.kill_on_drop(true);
    if let Some(dir) = cwd {
        cmd.current_dir(dir);
    }
    cmd.stdout(Stdio::piped()).stderr(Stdio::piped());
    let output = cmd.output().await.map_err(|e| ToolError::ToolFailed {
        tool: program.to_string(),
        inner: format!("spawn {program}: {e}"),
    })?;
    let stdout = String::from_utf8_lossy(&output.stdout).to_string();
    let stderr = String::from_utf8_lossy(&output.stderr).to_string();
    let code = output.status.code().unwrap_or(-1);
    Ok(serde_json::json!({
        "exit_code": code,
        "stdout": stdout,
        "stderr": stderr,
    }))
}

/// Pull an optional string param out of the input JSON (None if absent/null).
fn opt_str<'a>(input: &'a Value, key: &str) -> Option<&'a str> {
    input.get(key).and_then(|v| v.as_str())
}

/// Pull an optional array-of-strings param, tolerating non-string elements by
/// stringifying them (matches the `ShellTool` rendering rule).
fn opt_string_array(input: &Value, key: &str) -> Vec<String> {
    input
        .get(key)
        .and_then(|v| v.as_array())
        .map(|arr| {
            arr.iter()
                .map(|v| match v {
                    Value::String(s) => s.clone(),
                    Value::Null => String::new(),
                    other => other.to_string(),
                })
                .collect()
        })
        .unwrap_or_default()
}

// ─── rust.toolchain ──────────────────────────────────────────────────────────

pub struct Toolchain;

#[async_trait]
impl Tool for Toolchain {
    fn name(&self) -> &str { "rust.toolchain" }
    fn description(&self) -> &str {
        "Report the installed Rust toolchain versions (`rustc` + `cargo`)."
    }
    fn schema(&self) -> Schema { Schema::new() }
    fn source(&self) -> ToolSourceKind { ToolSourceKind::Builtin }
    async fn execute(&self, _input: &Value, ctx: &ToolContext) -> ToolResult<Value> {
        let rustc = run("rustc", &["--version"], None, ctx).await?;
        let cargo = run("cargo", &["--version"], None, ctx).await?;
        // Collapse to the version lines (first line of stdout); fall back to
        // the full stdout if the binary printed something unexpected.
        let rustc_v = rustc["stdout"].as_str().map(|s| s.lines().next().unwrap_or(s).to_string())
            .unwrap_or_default();
        let cargo_v = cargo["stdout"].as_str().map(|s| s.lines().next().unwrap_or(s).to_string())
            .unwrap_or_default();
        Ok(serde_json::json!({
            "rustc": rustc_v,
            "cargo": cargo_v,
            "rustc_exit": rustc["exit_code"],
            "cargo_exit": cargo["exit_code"],
        }))
    }
}

// ─── the cargo subcommand tools ──────────────────────────────────────────────

/// A schema shared by every `cargo <sub>` tool: an optional `cwd`, an optional
/// `package` (`-p`), and optional `extra_args` forwarded verbatim.
fn cargo_schema() -> Schema {
    Schema::new()
        .param(Parameter::optional("cwd", ParamType::String)
            .with_desc("directory to run cargo in (defaults to current dir)"))
        .param(Parameter::optional("package", ParamType::String)
            .with_desc("cargo `-p` package selector"))
        .param(Parameter::optional("extra_args", ParamType::Array(Box::new(ParamType::String)))
            .with_desc("extra arguments forwarded after the subcommand"))
}

/// Build the argv for a `cargo <sub>` invocation from the input.
fn cargo_argv(sub: &str, input: &Value) -> Vec<String> {
    let mut argv = vec![sub.to_string()];
    if let Some(pkg) = opt_str(input, "package") {
        argv.push("-p".into());
        argv.push(pkg.to_string());
    }
    argv.extend(opt_string_array(input, "extra_args"));
    argv
}

macro_rules! cargo_tool {
    ($struct:ident, $name:expr, $desc:expr, $sub:expr) => {
        pub struct $struct;
        #[async_trait]
        impl Tool for $struct {
            fn name(&self) -> &str { $name }
            fn description(&self) -> &str { $desc }
            fn schema(&self) -> Schema { cargo_schema() }
            fn source(&self) -> ToolSourceKind { ToolSourceKind::Builtin }
            async fn execute(&self, input: &Value, ctx: &ToolContext) -> ToolResult<Value> {
                let argv = cargo_argv($sub, input);
                let refs: Vec<&str> = argv.iter().map(|s| s.as_str()).collect();
                run("cargo", &refs, opt_str(input, "cwd"), ctx).await
            }
        }
    };
}

cargo_tool!(CargoCheck,  "rust.cargo_check",  "Run `cargo check` (type-check without codegen).", "check");
cargo_tool!(CargoBuild,  "rust.cargo_build",  "Run `cargo build` (compile the workspace).", "build");
cargo_tool!(CargoTest,   "rust.cargo_test",   "Run `cargo test` (the workspace's test suite).", "test");
cargo_tool!(CargoClippy, "rust.cargo_clippy", "Run `cargo clippy` (the linter).", "clippy");
cargo_tool!(CargoDoc,    "rust.cargo_doc",    "Run `cargo doc --no-deps` (build docs).", "doc");

// `cargo fmt` is special: it takes a `--check` flag and never accepts
// `extra_args` the same way (rustfmt flags, not cargo flags), so it gets a
// hand-written impl instead of the macro.
pub struct CargoFmt;

#[async_trait]
impl Tool for CargoFmt {
    fn name(&self) -> &str { "rust.cargo_fmt" }
    fn description(&self) -> &str {
        "Run `cargo fmt` on the workspace. Set `check=true` for `--check` (dry-run)."
    }
    fn schema(&self) -> Schema {
        Schema::new()
            .param(Parameter::optional("cwd", ParamType::String)
                .with_desc("directory to run cargo fmt in"))
            .param(Parameter::optional("check", ParamType::Boolean)
                .with_desc("if true, run `cargo fmt --check` (report, don't rewrite)")
                .with_default(serde_json::json!(false)))
            .param(Parameter::optional("package", ParamType::String)
                .with_desc("cargo `-p` package selector"))
    }
    fn source(&self) -> ToolSourceKind { ToolSourceKind::Builtin }
    async fn execute(&self, input: &Value, ctx: &ToolContext) -> ToolResult<Value> {
        let mut argv = vec!["fmt".to_string()];
        if let Some(pkg) = opt_str(input, "package") {
            argv.push("-p".into());
            argv.push(pkg.to_string());
        }
        if input.get("check").and_then(|v| v.as_bool()).unwrap_or(false) {
            argv.push("--check".into());
        }
        let refs: Vec<&str> = argv.iter().map(|s| s.as_str()).collect();
        run("cargo", &refs, opt_str(input, "cwd"), ctx).await
    }
}

// ─── rust.fmt_file ───────────────────────────────────────────────────────────

pub struct FmtFile;

#[async_trait]
impl Tool for FmtFile {
    fn name(&self) -> &str { "rust.fmt_file" }
    fn description(&self) -> &str {
        "Format a single Rust file in place with `rustfmt`."
    }
    fn schema(&self) -> Schema {
        Schema::new()
            .param(Parameter::required("path", ParamType::String)
                .with_desc("path to the .rs file to format"))
            .param(Parameter::optional("edition", ParamType::String)
                .with_desc("rustfmt `--edition` (e.g. 2021)"))
    }
    fn source(&self) -> ToolSourceKind { ToolSourceKind::Builtin }
    async fn execute(&self, input: &Value, ctx: &ToolContext) -> ToolResult<Value> {
        let path = input["path"].as_str().ok_or_else(|| ToolError::BadParam {
            name: "path".into(),
            expected: ParamType::String,
            detail: "missing or not a string".into(),
        })?;
        let mut argv = vec!["rustfmt".to_string()];
        if let Some(ed) = opt_str(input, "edition") {
            argv.push("--edition".into());
            argv.push(ed.to_string());
        }
        argv.push(path.to_string());
        let refs: Vec<&str> = argv.iter().map(|s| s.as_str()).collect();
        run("rustfmt", &refs, None, ctx).await
    }
}

/// Every `rust.*` tool, ready to register.
pub fn all() -> Vec<ToolArc> {
    vec![
        arc(Toolchain),
        arc(CargoCheck),
        arc(CargoBuild),
        arc(CargoTest),
        arc(CargoFmt),
        arc(CargoClippy),
        arc(CargoDoc),
        arc(FmtFile),
    ]
}

/// The number of `rust.*` tools (for `rc sources rust`).
pub const COUNT: usize = 8;

/// The category label every `rust.*` tool carries (derived from the name's
/// first segment).
pub const CATEGORY: &str = "rust";

#[cfg(test)]
mod tests {
    use super::*;
    use rc_core::Registry;

    #[test]
    fn all_tools_unique_and_complete() {
        let tools = all();
        assert_eq!(tools.len(), COUNT);
        let mut names: Vec<String> = tools.iter().map(|t| t.name().to_string()).collect();
        names.sort();
        let mut deduped = names.clone();
        deduped.dedup();
        assert_eq!(names.len(), deduped.len(), "duplicate rust tool names: {names:?}");
        // every name is under the `rust.` namespace
        for t in &tools {
            assert!(t.name().starts_with("rust."), "{}", t.name());
            assert_eq!(t.category(), CATEGORY);
            assert_eq!(t.source(), ToolSourceKind::Builtin);
        }
    }

    #[test]
    fn registers_cleanly() {
        let r = Registry::new();
        r.register_all(all());
        assert_eq!(r.len(), COUNT);
        for t in all() {
            assert!(r.contains(t.name()));
        }
    }

    #[tokio::test]
    async fn cancelled_does_not_spawn() {
        // A cancelled context must short-circuit before touching the toolchain,
        // so this test passes even on a host without rustc installed.
        let ctx = ToolContext {
            cancelled: true,
            ..ToolContext::default()
        };
        let out = Toolchain.execute(&serde_json::json!({}), &ctx).await;
        assert!(out.is_err(), "expected cancellation error, got {out:?}");
        let err = out.unwrap_err().to_string();
        assert!(err.contains("cancelled"), "unexpected error: {err}");
    }

    #[test]
    fn cargo_argv_assembles_package_and_extras() {
        let input = serde_json::json!({
            "package": "rc-core",
            "extra_args": ["--lib", "--message-format=json"]
        });
        let argv = cargo_argv("check", &input);
        assert_eq!(argv, vec!["check", "-p", "rc-core", "--lib", "--message-format=json"]);
    }

    #[tokio::test]
    async fn toolchain_reports_versions() {
        // Only meaningful when rustc/cargo are on $PATH; skip gracefully if not.
        if std::process::Command::new("rustc").arg("--version").output().is_err() {
            eprintln!("skipping toolchain_reports_versions: rustc not on PATH");
            return;
        }
        let out = Toolchain
            .execute(&serde_json::json!({}), &ToolContext::default())
            .await
            .expect("toolchain runs");
        let rustc = out["rustc"].as_str().unwrap_or("");
        assert!(rustc.contains("rustc"), "rustc line: {rustc:?}");
    }
}
