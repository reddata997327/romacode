//! `remix` — RomaCode's native Ethereum toolchain, wired to the **rrx**
//! binary (Remix IDE tools rebuilt in pure Rust: real EVM interpreter
//! Istanbul→Cancun, real MPT state, real gas metering, real solc, real
//! secp256k1 signing — hard-wired to Ethereum mainnet with endpoint
//! failover). One binary, two consumers: the agent's `remix`/`remix_status`
//! tools and the human CLI.
//!
//! Integration model — the same shape every other builtin uses (rnmap,
//! browser, android):
//!
//! * the engine lives here; the thin `tools/remix.rs` wrapper re-exports it;
//! * the binary is **discovered**, never downloaded: explicit config path →
//!   exe-adjacent `rrx` (a single workspace `cargo build --release` puts
//!   both binaries in `target/release/`) and the shipped `bin/rrx` layout →
//!   data dir → PATH → project `bin/`;
//! * every command the agent can run is on a curated allowlist; daemon-style
//!   commands (`node`, `explorer`, `mcp`, `lsp`, `ws`, `follow`) are excluded
//!   because they never return on their own;
//! * chain-writing commands (`send`, `deploy`, `call`, script runs, demo
//!   deploys) are refused unless `remix.allow_chain_writes = true` in the
//!   config — and when refused, the child never sees `RRX_PRIVATE_KEY`;
//! * the command runs with the Bash tool's confinement discipline: own
//!   process group, cancel-aware, timeout cap, output captured through
//!   unlinked private temp files, response size capped.

use crate::config::Config;
use serde_json::{json, Value};
use std::path::PathBuf;
use std::sync::atomic::Ordering;
use std::time::Duration;

// ── command policy ──────────────────────────────────────────────────────────

/// Every rrx command the agent may run (clap's kebab-case spellings). Daemon
/// commands are NOT on this list: they listen forever and would burn the
/// timeout. The full CLI still offers them to the human via Bash.
pub const SUPPORTED_COMMANDS: &[&str] = &[
    // project / file plane
    "workspace", "file", "search", "compile", "test", "analyze", "debug",
    "gas", "flatten", "uml", "doc", "script", "git", "coverage",
    "fuzz", "invariant", "reentrancy", "layout", "bytediff", "verify",
    // account plane
    "keys", "sign", "precompiles", "aa",
    // chain-read plane (live mainnet with failover)
    "net", "chain", "chains", "tx", "block", "account", "trace", "ethcall",
    "debugtx", "logs", "ens", "token", "sources", "proof", "gasprice",
    "fork", "blob", "verkle", "build-block", "index",
    // chain-write plane (gated by remix.allow_chain_writes + RRX_PRIVATE_KEY)
    "send", "deploy", "call",
    // toolchain / meta
    "wildtoken", "fakeasset", "utils", "solc", "plugins",
];

/// Commands that broadcast transactions to a REAL network. Refused unless
/// `remix.allow_chain_writes` is set in the config; the child never sees
/// `RRX_PRIVATE_KEY` while the gate is closed.
pub const CHAIN_WRITE_COMMANDS: &[&str] = &["send", "deploy", "call", "script"];

/// Commands excluded from the agent surface because they are long-lived
/// daemons or TUIs (`rrx mcp` is exposed to MCP clients directly; the agent
/// does not need to run it).
pub const EXCLUDED_DAEMONS: &[&str] = &["node", "explorer", "follow", "mcp", "lsp", "ws"];

/// The rrx binary this build was integrated against (for the status report).
pub const RRX_INTEGRATION_VERSION: &str = "0.8.0";

fn is_chain_write(command: &str, argv: &[String]) -> bool {
    if CHAIN_WRITE_COMMANDS.contains(&command) {
        return true;
    }
    // The demo stacks only hit the chain through their `deploy` sub-action.
    if matches!(command, "wildtoken" | "fakeasset") {
        return argv.first().map(String::as_str) == Some("deploy");
    }
    false
}

// ── tool definitions ────────────────────────────────────────────────────────

/// Tool schemas in Anthropic shape (converted for OpenAI-style providers by
/// the registry, exactly like every other tool).
pub fn definitions() -> Vec<Value> {
    vec![
        json!({
            "name": "remix",
            "description": "Run one rrx command — RomaCode's native Remix/Ethereum toolchain (real EVM, real solc, live Ethereum mainnet with endpoint failover). \
                Project plane: workspace file search compile test analyze debug gas flatten uml doc script git coverage fuzz invariant reentrancy layout bytediff verify. \
                Account plane: keys sign precompiles aa. \
                Chain plane: net chain chains tx block account trace ethcall debugtx logs ens token sources proof gasprice fork blob verkle build-block index. \
                Meta: wildtoken fakeasset utils solc plugins. \
                Pass `command` = one of those words and `args` = its arguments, e.g. command=\"ethcall\", args=[\"0xdAC17F…1ec7\", \"totalSupply()\", \"--returns\", \"uint256\"]. \
                Chain-writing commands (send/deploy/call/script and wildtoken|fakeasset deploy) broadcast REAL transactions: they are refused unless remix.allow_chain_writes=true AND RRX_PRIVATE_KEY is set — never pass --yes yourself, and never echo a private key. \
                Daemon commands (node explorer follow mcp lsp ws) are not available here; use remix_status first if a call fails oddly.",
            "input_schema": {
                "type": "object",
                "properties": {
                    "command": { "type": "string", "description": "One rrx command from the list, or \"help\" for the syntax reference." },
                    "args": { "type": "array", "items": { "type": "string" }, "description": "Arguments for the command, one per element, e.g. [\"latest\"], [\"0x<txhash>\"], [\"--rpc\", \"https://…\"]" },
                    "timeout_secs": { "type": "integer", "description": "Override the whole-command timeout for this call (1..1800). Slow runs: compile/test/fuzz/invariant/coverage." }
                },
                "required": ["command"]
            }
        }),
        json!({
            "name": "remix_status",
            "description": "Doctor for the built-in Remix/rrx Ethereum toolchain: which rrx binary was found and where, its version, the active RPC policy, the command allowlist, and the chain-write safety flag. Call it before the first remix command of a session, and whenever a remix call fails oddly.",
            "input_schema": {
                "type": "object",
                "properties": {},
                "required": []
            }
        }),
    ]
}

// ── execution ───────────────────────────────────────────────────────────────

/// Execute the `remix` tool. Cancel-safe like the Bash tool: polls the flag
/// while the child runs and kills the whole process group on cancel/timeout.
pub fn execute(
    cfg: &Config,
    input: &Value,
    cancel: &std::sync::atomic::AtomicBool,
) -> Result<String, String> {
    let command = input["command"]
        .as_str()
        .map(str::trim)
        .filter(|c| !c.is_empty())
        .ok_or("missing `command` — one of the rrx commands (try command=\"help\")")?;

    if command == "help" {
        return Ok(help_text());
    }

    let argv: Vec<String> = match &input["args"] {
        Value::Array(a) => a
            .iter()
            .map(|v| match v {
                Value::String(s) => s.clone(),
                other => other.to_string(),
            })
            .collect(),
        Value::Null => Vec::new(),
        other => return Err(format!("`args` must be an array of strings, got {other}")),
    };
    if argv.iter().any(|a| a.contains('\0')) {
        return Err("args contain a NUL byte".into());
    }

    // Policy: allowlist first, then the chain-write gate.
    if !SUPPORTED_COMMANDS.contains(&command) {
        if EXCLUDED_DAEMONS.contains(&command) {
            return Err(format!(
                "rrx {command} is a long-lived daemon and is not available as a tool — \
                 start it with the Bash tool if the task really needs it"
            ));
        }
        return Err(format!(
            "unknown rrx command {command:?} — call remix command=\"help\" for the list"
        ));
    }

    let writes = is_chain_write(command, &argv);
    if writes && !cfg.remix.allow_chain_writes {
        return Err(
            "this command broadcasts REAL transactions to the network, and remix.allow_chain_writes \
             is false in the config — flip it to true (and export RRX_PRIVATE_KEY) if the task \
             genuinely needs to move funds or deploy; dry-run equivalents (ethcall, gas estimate, \
             fork) need no flag"
                .into(),
        );
    }
    // The model must never opt itself into broadcasting: --yes is a human
    // decision made in the config, not an argument the agent may add.
    if writes && argv.iter().any(|a| a == "--yes") {
        return Err(
            "--yes is not accepted from the agent: set remix.allow_chain_writes=true in the \
             config file instead"
                .into(),
        );
    }

    let bin = ensure_binary(cfg)?;

    // Per-call timeout override, clamped; config value stays the default.
    let timeout = match input["timeout_secs"].as_u64() {
        Some(0) | None => cfg.remix.timeout_secs,
        Some(v) => v.clamp(1, 1_800),
    };

    if cancel.load(Ordering::SeqCst) {
        return Err("cancelled before remix command".into());
    }

    let (stdout, stderr, status_note) = run(cfg, &bin, command, &argv, timeout, cancel)?;
    let mut text = format_report(&stdout, &stderr, &status_note, command);

    // Response cap with a char-boundary-safe cut (same discipline as github).
    let cap = cfg.remix.max_output_chars;
    if text.chars().count() > cap {
        text = text.chars().take(cap).collect();
        text.push_str("\n… [output truncated at remix.max_output_chars — narrow the command]");
    }
    Ok(text)
}

/// Execute the `remix_status` tool (the doctor). Cheap and offline: binary
/// discovery + `--version` + one local smoke command.
pub fn status_execute(cfg: &Config, _input: &Value) -> Result<String, String> {
    Ok(status_report(cfg))
}

// ── help ────────────────────────────────────────────────────────────────────

fn help_text() -> String {
    format!(
        "rrx {} — Remix IDE tools rebuilt in pure Rust (headless, real EVM, real gas).\n\
         Syntax: rrx <command> [args…] — call the remix tool with command=<word> and args=[\"…\"].\n\n\
         Project plane (run inside a workspace: rrx workspace init <template> creates one):\n\
         file ls|cat|write|rm|mv|mkdir|tree · search <pattern> [--regex] · compile [file] [--solc v] [--no-opt] · \
         test [file] · analyze · reentrancy · coverage · gas estimate|report|price|trace · flatten <file> · \
         uml <file> · doc <file> · debug <Contract> <Function> · layout <Old> <New> · bytediff <addr> <Contract> · \
         fuzz <Contract> <fn> [--runs n] [--seed s] · invariant <Contract> [--sequences n] [--depth n] · \
         verify <addr> … · script <file.rrx> · git init|add|commit|status|log\n\n\
         Account plane:\n\
         keys new|list|dev · sign <payload> / verify-message · precompiles · aa --sender --call-data\n\n\
         Chain plane (LIVE mainnet; RRX_RPC overrides, --rpc <url> overrides per call):\n\
         net · chain · chains [slug] [--json] · block <n|latest|finalized> · tx 0x<hash> · trace 0x<hash> · \
         debugtx 0x<hash> [--max-steps n] [--top n] [--json] · account <ADDRESS> (ENS names go through `ens` first) · \
         logs --address <a> [--topic0 <sig>] [--from] [--to] · ens <name> | --reverse <addr> · token <addr> [--holder <h>] · \
         sources <addr> [--chain 1] · proof <addr> [--slot n] [--block tag] · gasprice · blob decode|kzg|fetch · \
         verkle · build-block [--max-txs n] · index\n\n\
         Meta:\n\
         wildtoken source|compile|demo|deploy · fakeasset registry|variants|source|compile|deploy|audit|verify|flasher|pending|demo · \
         utils keccak|unit|selector|namehash · solc install|list|use · plugins\n\n\
         Excluded daemons (start via Bash if ever needed): {}\n\
         Chain writes (send/deploy/call/script, demo deploys) need remix.allow_chain_writes=true + RRX_PRIVATE_KEY.",
        RRX_INTEGRATION_VERSION,
        EXCLUDED_DAEMONS.join(" ")
    )
}

// ── binary discovery ────────────────────────────────────────────────────────

/// Where the rrx binary is: explicit config path → exe-adjacent (the shipped
/// `bin/` layout) → data dir → PATH → project `bin/`. Read-only; the message
/// lists every place searched so `remix_status` can teach the install.
pub fn ensure_binary(cfg: &Config) -> Result<PathBuf, String> {
    let mut searched: Vec<String> = Vec::new();

    fn usable(p: &PathBuf) -> bool {
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            p.metadata()
                .map(|m| m.is_file() && (m.permissions().mode() & 0o111 != 0))
                .unwrap_or(false)
        }
        #[cfg(not(unix))]
        {
            p.is_file()
        }
    }

    if !cfg.remix.binary.trim().is_empty() {
        let p = PathBuf::from(cfg.remix.binary.trim());
        if usable(&p) {
            return Ok(p);
        }
        return Err(format!(
            "remix.binary points at '{}' but it is not an executable file",
            cfg.remix.binary.trim()
        ));
    }

    // exe-adjacent: <exe>/rrx, <exe>/bin/rrx, <exe>/../bin/rrx — a single
    // workspace `cargo build --release` drops romacode-server and rrx
    // together in target/release/ (first candidate), and the shipped
    // packaging keeps them side by side under bin/.
    if let Ok(exe) = std::env::current_exe() {
        if let Some(dir) = exe.parent() {
            for cand in [
                dir.join("rrx"),
                dir.join("bin").join("rrx"),
                dir.join("..").join("bin").join("rrx"),
            ] {
                if usable(&cand) {
                    return Ok(cand);
                }
                searched.push(cand.display().to_string());
            }
        }
    }

    // Data dir install target (<data>/romacode/bin/rrx).
    if let Some(data) = dirs::data_dir() {
        let cand = data.join("romacode").join("bin").join("rrx");
        if usable(&cand) {
            return Ok(cand);
        }
        searched.push(cand.display().to_string());
    }

    // PATH scan.
    if let Ok(path_var) = std::env::var("PATH") {
        let exe_name = if cfg!(windows) { "rrx.exe" } else { "rrx" };
        for dir in std::env::split_paths(&path_var) {
            let cand = dir.join(exe_name);
            if usable(&cand) {
                return Ok(cand);
            }
        }
    }

    // Project-local bin/.
    if let Ok(cwd) = std::env::current_dir() {
        let cand = cwd.join("bin").join("rrx");
        if usable(&cand) {
            return Ok(cand);
        }
        searched.push(cand.display().to_string());
    }

    Err(format!(
        "the rrx binary (Remix toolchain, v{}) was not found. Searched: {}. \
         Install it with `cargo build --release` at the workspace root — rrx is a workspace \
         member (crates/remix/rrx-cli) and target/release/rrx appears next to the server \
         binary — or copy that rrx binary into bin/ next to the romacode binary, or set \
         remix.binary in the config file",
        RRX_INTEGRATION_VERSION,
        if searched.is_empty() {
            "(nothing)".into()
        } else {
            searched.join(", ")
        }
    ))
}

// ── child process (rnmap's discipline: process group, cancel, timeout) ─────

/// Run one rrx command with cancel + timeout, capturing output through
/// unlinked private temp files. Returns (stdout, stderr, status note).
fn run(
    cfg: &Config,
    bin: &PathBuf,
    command: &str,
    argv: &[String],
    timeout_secs: u64,
    cancel: &std::sync::atomic::AtomicBool,
) -> Result<(String, String, String), String> {
    use std::io::Seek;
    use std::io::SeekFrom;
    use std::os::unix::fs::OpenOptionsExt;
    use std::process::{Command, Stdio};
    use std::time::Instant;

    let mk = |tag: &str| -> Result<std::fs::File, String> {
        let p = std::env::temp_dir().join(format!(
            "romacode-remix-{}-{}-{}",
            std::process::id(),
            tag,
            nonce()
        ));
        let f = std::fs::OpenOptions::new()
            .read(true)
            .write(true)
            .create_new(true)
            .mode(0o600)
            .open(&p)
            .map_err(|e| format!("capture file: {e}"))?;
        std::fs::remove_file(&p).map_err(|e| format!("unlink capture file: {e}"))?;
        Ok(f)
    };
    let mut f_out = mk("out")?;
    let mut f_err = mk("err")?;
    let stdout = f_out.try_clone().map_err(|e| e.to_string())?;
    let stderr = f_err.try_clone().map_err(|e| e.to_string())?;

    let mut cmd = Command::new(bin);
    cmd.arg(command)
        .args(argv)
        .stdin(Stdio::null())
        .stdout(Stdio::from(stdout))
        .stderr(Stdio::from(stderr));
    // Own process group: a kill must reap any helper the command spawned.
    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt;
        cmd.process_group(0);
    }
    // RPC override for the chain-facing commands.
    if !cfg.remix.rpc.trim().is_empty() {
        cmd.env("RRX_RPC", cfg.remix.rpc.trim());
    }
    // Chain-write gate: without the flag the child must not see a signing
    // key even if the server process itself has one in its environment.
    if !cfg.remix.allow_chain_writes {
        cmd.env_remove("RRX_PRIVATE_KEY");
    }
    if cancel.load(Ordering::SeqCst) {
        return Err("cancelled before remix command".into());
    }
    let mut child = cmd
        .spawn()
        .map_err(|e| format!("start rrx {command}: {e}"))?;

    let timeout = if timeout_secs == 0 {
        Duration::MAX
    } else {
        Duration::from_secs(timeout_secs)
    };
    let started = Instant::now();
    let mut note = String::new();
    let finished: Option<std::process::ExitStatus> = loop {
        match child.try_wait() {
            Ok(Some(s)) => break Some(s),
            Ok(None) => {}
            Err(e) => {
                note = format!("wait: {e}");
                break None;
            }
        }
        if cancel.load(Ordering::SeqCst) {
            note = "cancelled".into();
            break None;
        }
        if started.elapsed() >= timeout {
            note = format!("timed out after {timeout_secs}s (remix.timeout_secs)");
            break None;
        }
        std::thread::sleep(Duration::from_millis(25));
    };

    if finished.is_none() {
        kill_group(&mut child);
        if note.is_empty() {
            note = "killed".into();
        }
    }

    let read_cap = |f: &mut std::fs::File| -> String {
        let _ = f.seek(SeekFrom::Start(0));
        let mut s = String::new();
        let _ = std::io::Read::read_to_string(f, &mut s);
        s
    };
    let out = read_cap(&mut f_out);
    let err = read_cap(&mut f_err);

    match finished {
        Some(s) if s.success() => Ok((out, err, note)),
        Some(s) => {
            let code = s.code().unwrap_or(-1);
            Err(format!(
                "rrx {command} exited with status {code}{}",
                if err.trim().is_empty() {
                    String::new()
                } else {
                    format!(": {}", err.trim().chars().take(600).collect::<String>())
                }
            ))
        }
        None => Err(format!(
            "rrx {command} did not finish ({note}); partial output kept below:\n{out}"
        )),
    }
}

fn kill_group(child: &mut std::process::Child) {
    #[cfg(unix)]
    {
        if let Ok(pid) = i32::try_from(child.id()) {
            // SIGTERM the whole group first; a bare kill leaves helpers alive.
            unsafe { libc::kill(-pid, libc::SIGTERM) };
        }
    }
    let _ = child.kill();
    let _ = child.wait();
}

fn nonce() -> u128 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0)
}

// ── output shaping ──────────────────────────────────────────────────────────

/// Combine the captured streams into one model-facing report. rrx prints
/// human tables by default; --json requests give structured output.
fn format_report(stdout: &str, stderr: &str, note: &str, command: &str) -> String {
    let mut out = String::new();
    if !stdout.trim().is_empty() {
        out.push_str(stdout.trim_end());
        out.push('\n');
    }
    if !stderr.trim().is_empty() {
        out.push_str("\n[stderr]\n");
        out.push_str(stderr.trim_end());
        out.push('\n');
    }
    if !note.is_empty() {
        out.push_str(&format!("\n[{command}] {note}\n"));
    }
    out.trim_end().to_string()
}

// ── status / doctor ─────────────────────────────────────────────────────────

fn status_report(cfg: &Config) -> String {
    let mut s = String::from("Remix (rrx) Ethereum toolchain — status\n");
    s.push_str(&format!(
        "  enabled             {}\n  chain writes        {} (remix.allow_chain_writes)\n  timeout             {}s (remix.timeout_secs)\n  output cap          {} chars\n  rpc override        {}\n",
        if cfg.remix.enabled { "yes" } else { "no" },
        if cfg.remix.allow_chain_writes { "ALLOWED" } else { "refused" },
        cfg.remix.timeout_secs,
        cfg.remix.max_output_chars,
        if cfg.remix.rpc.trim().is_empty() {
            "none — rrx built-in mainnet failover list"
        } else {
            cfg.remix.rpc.trim()
        },
    ));
    match ensure_binary(cfg) {
        Ok(bin) => {
            s.push_str(&format!("  binary              {}\n", bin.display()));
            // Offline smoke: version + one local command. Network probes
            // (`rrx net`) belong to the agent, not the doctor.
            if let Ok((out, err, _)) = run_quiet(&bin, &["--version".into()]) {
                let line = if out.trim().is_empty() { err.trim() } else { out.trim() };
                s.push_str(&format!(
                    "  version             {}\n",
                    line.lines().next().unwrap_or("?")
                ));
            }
            if let Ok((out, _err, _)) =
                run_quiet(&bin, &["utils".into(), "keccak".into(), "remix".into()])
            {
                s.push_str(&format!(
                    "  smoke (utils keccak) {}\n",
                    out.trim().lines().next().unwrap_or("(no output)")
                ));
            }
        }
        Err(e) => {
            s.push_str(&format!("  binary              NOT FOUND\n  → {e}\n"));
        }
    }
    s.push_str(&format!(
        "  agent surface       {} commands (remix tool) + doctor\n  excluded daemons    {}\n",
        SUPPORTED_COMMANDS.len(),
        EXCLUDED_DAEMONS.join(" ")
    ));
    s
}

/// Timeout-free run used only by the doctor (version + smoke are instant).
fn run_quiet(bin: &PathBuf, args: &[String]) -> Result<(String, String, String), String> {
    use std::process::{Command, Stdio};
    let out = Command::new(bin)
        .args(args)
        .stdin(Stdio::null())
        .output()
        .map_err(|e| format!("run rrx: {e}"))?;
    Ok((
        String::from_utf8_lossy(&out.stdout).into_owned(),
        String::from_utf8_lossy(&out.stderr).into_owned(),
        String::new(),
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cfg() -> Config {
        Config::default()
    }

    #[test]
    fn definitions_carry_both_tools() {
        let defs = definitions();
        assert_eq!(defs.len(), 2);
        assert_eq!(defs[0]["name"], "remix");
        assert_eq!(defs[1]["name"], "remix_status");
        assert!(defs[0]["description"].as_str().unwrap().contains("ethcall"));
    }

    #[test]
    fn every_supported_command_is_unique_and_no_daemon_leaks_in() {
        let mut seen = std::collections::HashSet::new();
        for c in SUPPORTED_COMMANDS {
            assert!(seen.insert(*c), "duplicate remix command {c}");
            assert!(
                !EXCLUDED_DAEMONS.contains(c),
                "daemon {c} must not be on the agent allowlist"
            );
        }
        for c in EXCLUDED_DAEMONS {
            assert!(!SUPPORTED_COMMANDS.contains(c), "daemon {c} on the allowlist");
        }
    }

    #[test]
    fn chain_write_gate_refuses_before_anything_else() {
        let cfg = cfg();
        let input = json!({"command": "send", "args": ["--to", "vitalik.eth", "--value", "0.001"]});
        let err = execute(&cfg, &input, &Default::default()).unwrap_err();
        assert!(err.contains("allow_chain_writes"), "{err}");
    }

    #[test]
    fn yes_is_never_accepted_from_the_agent() {
        let mut cfg = cfg();
        cfg.remix.allow_chain_writes = true;
        // The binary may be absent in the test environment, but the --yes
        // check must fire before discovery regardless.
        let input = json!({"command": "send", "args": ["--to", "0xabc", "--value", "1", "--yes"]});
        match execute(&cfg, &input, &Default::default()) {
            Err(e) => assert!(e.contains("--yes"), "{e}"),
            Ok(_) => panic!("--yes must never pass the tool"),
        }
    }

    #[test]
    fn unknown_and_daemon_commands_explain_themselves() {
        let cfg = cfg();
        let err = execute(&cfg, &json!({"command": "nonsense"}), &Default::default()).unwrap_err();
        assert!(err.contains("unknown rrx command"), "{err}");
        let err = execute(&cfg, &json!({"command": "mcp"}), &Default::default()).unwrap_err();
        assert!(err.contains("daemon"), "{err}");
    }

    #[test]
    fn demo_deploys_are_chain_writes_but_sources_are_not() {
        let d = ["deploy".to_string()];
        assert!(is_chain_write("wildtoken", &d));
        assert!(is_chain_write("fakeasset", &d));
        assert!(!is_chain_write("wildtoken", &["source".to_string()]));
        assert!(!is_chain_write("fakeasset", &["audit".to_string()]));
        assert!(is_chain_write("send", &[]));
        assert!(is_chain_write("call", &[]));
        assert!(!is_chain_write("ethcall", &[]));
        assert!(!is_chain_write("debugtx", &[]));
    }

    #[test]
    fn status_report_reachable_without_a_binary() {
        // With no rrx anywhere the doctor must still produce a full report
        // (discovery failure becomes guidance, not an Err).
        let s = status_report(&cfg());
        assert!(s.contains("Remix (rrx)"));
        assert!(s.contains("chain writes"));
        assert!(s.contains("excluded daemons"));
    }

    #[test]
    fn help_lists_the_plane_structure() {
        let h = help_text();
        for plane in ["Project plane", "Account plane", "Chain plane", "Meta:"] {
            assert!(h.contains(plane), "help missing {plane}");
        }
        assert!(h.contains("build-block"));
    }

    #[test]
    fn help_and_status_are_available_without_args_or_binary() {
        let cfg = cfg();
        let h = execute(&cfg, &json!({"command": "help"}), &Default::default()).unwrap();
        assert!(h.contains("rrx"));
        let s = execute(&cfg, &json!({"command": "remix_status"}), &Default::default()).is_err();
        // remix_status is dispatched by name in tools/mod.rs, not here.
        assert!(s);
    }
}
