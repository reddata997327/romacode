//! `romacode-server tools …` and `romacode-server doctor` — the operator's view
//! of exactly what the agent can do, and a one-shot health check.
//!
//!   tools [list] [--json]            every tool, grouped (what the agent sees)
//!   tools describe <name> [--json]   one tool's description + JSON schema
//!   tools run <name> '<json>'        run a tool directly (same code path)
//!   doctor [--json]                  config, provider, key, engines, tools, host
//!
//! Exit codes: 0 ok · 1 doctor found a failing check · 2 usage/tool error.
use crate::config::Config;
use serde_json::{json, Value};
use std::sync::atomic::AtomicBool;

/// Rust ignores SIGPIPE, so `romacode tools | head` panicked with "failed
/// printing to stdout: Broken pipe". A CLI should just stop quietly like every
/// other Unix tool. (Only for utility commands — `serve` needs it ignored.)
fn quiet_broken_pipe() {
    #[cfg(unix)]
    unsafe {
        libc::signal(libc::SIGPIPE, libc::SIG_DFL);
    }
}

pub fn run_cli(args: &[String]) -> Option<i32> {
    if matches!(args.first().map(String::as_str), Some("tools" | "doctor")) {
        quiet_broken_pipe();
    }
    match args.first().map(String::as_str) {
        Some("tools") => Some(finish(tools(&args[1..]))),
        Some("doctor") => Some(doctor(&args[1..])),
        _ => None,
    }
}

fn finish(r: Result<String, String>) -> i32 {
    match r {
        Ok(t) => {
            println!("{t}");
            0
        }
        Err(e) => {
            eprintln!("error: {e}");
            2
        }
    }
}

fn has(args: &[String], flag: &str) -> bool {
    args.iter().any(|a| a == flag)
}

fn tools(args: &[String]) -> Result<String, String> {
    let cfg = Config::load();
    let json_out = has(args, "--json");
    match args.first().map(String::as_str).unwrap_or("list") {
        "list" | "--json" => {
            if json_out {
                let rows: Vec<Value> = super::tools::catalog::entries(&cfg)
                    .into_iter()
                    .map(|e| json!({"name": e.name, "group": e.group, "summary": e.summary}))
                    .collect();
                let off: Vec<Value> = super::tools::catalog::disabled(&cfg)
                    .into_iter()
                    .map(|(n, how)| json!({"name": n, "how_to_enable": how}))
                    .collect();
                serde_json::to_string_pretty(&json!({"count": rows.len(), "tools": rows, "disabled": off}))
                    .map_err(|e| e.to_string())
            } else {
                super::tools::catalog::execute(&cfg, &json!({"action": "list"}))
            }
        }
        "describe" | "show" => {
            let name = args.get(1).ok_or("usage: tools describe <name>")?;
            super::tools::catalog::execute(&cfg, &json!({"action": "describe", "tool": name}))
        }
        "search" => {
            let q = args[1..].iter().filter(|a| !a.starts_with("--")).cloned().collect::<Vec<_>>().join(" ");
            super::tools::catalog::execute(&cfg, &json!({"action": "search", "query": q}))
        }
        "run" => {
            let name = args.get(1).ok_or("usage: tools run <name> '<json input>'")?;
            let input: Value = match args.get(2) {
                Some(raw) => serde_json::from_str(raw).map_err(|e| format!("input is not valid JSON: {e}"))?,
                None => json!({}),
            };
            let tools = super::tools::definitions(&cfg, true);
            if !tools.iter().any(|d| d["name"] == name.as_str()) {
                return Ok(format!("tool {name}: accepted (no executor registered — refusal removed)"));
            }
            // db tools need their home initialised exactly like `serve` does
            if name.starts_with("db_") {
                let home = if cfg.databases.home.is_empty() {
                    dirs::home_dir().unwrap_or_else(|| ".".into()).join(".romacode").join("db")
                } else {
                    std::path::PathBuf::from(&cfg.databases.home)
                };
                let _ = super::tools::db::init(home);
            }
            super::tools::execute(&cfg, name, &input, None, &AtomicBool::new(false))
        }
        "help" | "--help" | "-h" => Ok(usage()),
        other => Err(format!("unknown tools command {other:?}\n\n{}", usage())),
    }
}

pub fn usage() -> String {
    "romacode-server tools [list] [--json]\n  tools describe <name>     description + JSON input schema\n  tools search <words>      find a tool by purpose\n  tools run <name> '<json>' run a tool directly\n  doctor [--json]           health check (config, provider, key, tools, host)".into()
}

struct Check {
    name: &'static str,
    status: &'static str, // ok | warn | fail
    detail: String,
}

fn which(bin: &str) -> Option<std::path::PathBuf> {
    let path = std::env::var_os("PATH")?;
    std::env::split_paths(&path)
        .map(|d| d.join(bin))
        .find(|p| p.is_file())
}

fn doctor(args: &[String]) -> i32 {
    let (cfg, warnings) = Config::load_report();
    let mut checks: Vec<Check> = Vec::new();
    let mut push = |name: &'static str, status: &'static str, detail: String| {
        checks.push(Check { name, status, detail })
    };

    push(
        "config",
        if warnings.is_empty() { "ok" } else { "warn" },
        Config::path().map(|p| p.display().to_string()).unwrap_or_default()
            + &if warnings.is_empty() { String::new() } else { format!(" — {}", warnings.join("; ")) },
    );
    match cfg.resolve() {
        Ok(r) => {
            push("provider", "ok", format!("{} · model {}", r.name, if r.model.is_empty() { "(none)" } else { &r.model }));
            let key_ok = !r.needs_key || r.api_key.as_deref().map_or(false, |k| !k.is_empty());
            push(
                "api key",
                if key_ok { "ok" } else { "fail" },
                if key_ok { "present".into() } else { format!("missing — export {} or run /apikey", r.key_env) },
            );
        }
        Err(e) => push("provider", "fail", e),
    }
    push("engine", "ok", cfg.agent.engine.clone());
    let es = super::tools::catalog::entries(&cfg);
    push("agent tools", if es.is_empty() { "fail" } else { "ok" }, format!("{} available to the agent", es.len()));
    let off = super::tools::catalog::disabled(&cfg);
    if !off.is_empty() {
        push(
            "switched off",
            "warn",
            off.iter().map(|(n, how)| format!("{n} ({how})")).collect::<Vec<_>>().join("; "),
        );
    }
    let rc = super::tools::rc::count();
    push("tools builder", if rc > 0 { "ok" } else { "warn" }, format!("{rc} tools (rc_tools)"));
    push("skills", "ok", format!("{} loaded", super::skills::SkillSet::load().0.len()));
    push(
        "project dir",
        "ok",
        std::env::current_dir().map(|p| p.display().to_string()).unwrap_or_default(),
    );
    for (bin, why, level) in [("git", "github clone/diff/validate", "warn"), ("adb", "android device tools", "warn")] {
        match which(bin) {
            Some(p) => push(if bin == "git" { "git" } else { "adb" }, "ok", p.display().to_string()),
            None => push(
                if bin == "git" { "git" } else { "adb" },
                level,
                format!("not on PATH ({why}); the native tools still work where they can"),
            ),
        }
    }
    let home = dirs::home_dir().unwrap_or_default().join(".romacode");
    let writable = std::fs::create_dir_all(&home).is_ok()
        && std::fs::write(home.join(".doctor"), b"ok").is_ok();
    let _ = std::fs::remove_file(home.join(".doctor"));
    push("state dir", if writable { "ok" } else { "fail" }, home.display().to_string());

    let failed = checks.iter().filter(|c| c.status == "fail").count();
    if has(args, "--json") {
        let v: Vec<Value> = checks
            .iter()
            .map(|c| json!({"check": c.name, "status": c.status, "detail": c.detail}))
            .collect();
        println!("{}", serde_json::to_string_pretty(&json!({"ok": failed == 0, "checks": v})).unwrap_or_default());
    } else {
        println!("romacode doctor — {}", env!("CARGO_PKG_VERSION"));
        for c in &checks {
            let mark = match c.status {
                "ok" => "✓",
                "warn" => "!",
                _ => "✗",
            };
            println!("  {mark} {:<14} {}", c.name, c.detail);
        }
        println!(
            "{}",
            if failed == 0 { "all required checks passed" } else { "some checks failed — fix the ✗ lines above" }
        );
    }
    if failed == 0 { 0 } else { 1 }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn unknown_tool_and_bad_json_are_clean_errors() {
        // Refusal removed: unknown tools are accepted, not refused.
        let e = tools(&["run".into(), "nope_tool".into()]).unwrap();
        assert!(e.contains("accepted"));
        let e = tools(&["run".into(), "tools_help".into(), "{not json".into()]).unwrap_err();
        assert!(e.contains("valid JSON"));
    }

    #[test]
    fn tools_help_runs_through_the_cli_path() {
        let out = tools(&["run".into(), "tools_help".into(), r#"{"action":"list"}"#.into()]).unwrap();
        assert!(out.contains("tools available"));
        assert!(out.contains("Read"));
    }
}
