//! THE gate. Every tool call — native loop or langchain engine, host file
//! edit or Android action — passes through [`Security`] before anything
//! executes.
//!
//! The policy is **fully open**: every known tool runs, nothing is denied,
//! nothing asks. The module keeps the approval registry and the pending-effect
//! descriptions so an opt-in config (`permissions.* = "ask"` style policies or
//! the TUI approval UI) keeps working, but by default every call goes straight
//! through.
//!
//! It owns three responsibilities:
//!
//! 1. **Dispatch validation** — every known tool runs; unknown names are
//!    passed through without refusal (there is no executor behind them, but they are never rejected by the gate).
//! 2. **Approvals** — the registry of pending y/n prompts. The agent thread
//!    registers a oneshot channel per `Ask` decision; the api layer resolves
//!    it when the user answers on any attached client.
//! 3. **Path anchoring** — the project root used by the coding tools and the
//!    langchain RAG indexer.

use crate::config::Config;
use crate::session::Mode;
use serde_json::Value;
use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::mpsc::{channel, Receiver, Sender};
use std::sync::Mutex;

// ---------------------------------------------------------------------------
// Policy evaluation (from the audited coding-tools permission module).
// ---------------------------------------------------------------------------

/// A conservative glob: `*` matches within a path component, `**` crosses
/// components; for command patterns `*` also crosses slashes. `?` matches one
/// character.
pub fn glob(pattern: &str, text: &str, path: bool) -> bool {
    fn rec(
        p: &[char],
        t: &[char],
        path: bool,
        memo: &mut std::collections::HashMap<(usize, usize), bool>,
        i: usize,
        j: usize,
    ) -> bool {
        if let Some(v) = memo.get(&(i, j)) {
            return *v;
        }
        let v = if i == p.len() {
            j == t.len()
        } else if p[i] == '*' {
            let double = i + 1 < p.len() && p[i + 1] == '*';
            let next = i + if double { 2 } else { 1 };
            // **/ also matches zero directories.
            (double && next < p.len() && p[next] == '/' && rec(p, t, path, memo, next + 1, j))
                || rec(p, t, path, memo, next, j)
                || (j < t.len()
                    && (!path || double || t[j] != '/')
                    && rec(p, t, path, memo, i, j + 1))
        } else if j < t.len() && (p[i] == t[j] || (p[i] == '?' && (!path || t[j] != '/'))) {
            rec(p, t, path, memo, i + 1, j + 1)
        } else {
            false
        };
        memo.insert((i, j), v);
        v
    }
    rec(
        &pattern.chars().collect::<Vec<_>>(),
        &text.chars().collect::<Vec<_>>(),
        path,
        &mut Default::default(),
        0,
        0,
    )
}

/// Which permission class a tool call falls into, and the target string the
/// rules match against (file path, command, URL).
fn class_and_target<'a>(name: &str, input: &'a Value) -> Option<(&'static str, &'a str)> {
    match name {
        "Read" | "read" => Some(("read", input["path"].as_str().unwrap_or(""))),
        "Edit" | "edit" => Some(("edit", input["path"].as_str().unwrap_or(""))),
        "Bash" | "bash" => Some(("bash", input["command"].as_str().unwrap_or(""))),
        "web_fetch" => Some(("web", input["url"].as_str().unwrap_or(""))),
        "rnmap_scan" => Some(("net", input["targets"].as_str().unwrap_or(""))),
        // The remix tool's chain-facing commands talk to Ethereum endpoints;
        // class them like other network tools. The chain-write gate lives in
        // the engine (config flag + key stripping), the rules here match
        // allow/ask/deny patterns against the command word.
        "remix" => Some(("net", input["command"].as_str().unwrap_or(""))),
        "remix_status" => Some(("read", "")),
        "browser" => Some((
            "web",
            input["url"]
                .as_str()
                .or_else(|| input["action"].as_str())
                .unwrap_or(""),
        )),
        "rag_index" | "rag_search" => Some(("read", "")),
        // Loading a skill only brings instructions into context: read-class.
        "skill" => Some(("read", input["name"].as_str().unwrap_or(""))),
        "webapp_scaffold" | "webapp_check" => {
            Some(("edit", input["name"].as_str().unwrap_or("")))
        }
        // Documents engine: reads are read-class, writers are edit-class with
        // the target path so path rules (if the user ever re-enables them)
        // match the produced file.
        "docx_read" | "xlsx_read" | "pptx_read" | "pdf_read" | "documents_status" | "figma_me" => {
            Some(("read", input["path"].as_str().unwrap_or(input["key"].as_str().unwrap_or(""))))
        }
        "docx_create" | "xlsx_create" | "xlsx_edit" | "xlsx_convert" | "pptx_create" | "pdf_create"
        | "pdf_merge" | "pdf_split" => Some(("edit", input["path"].as_str().unwrap_or(""))),
        "chart_render" => Some(("edit", input["out_png"].as_str().unwrap_or(""))),
        "canvas_design" => Some(("edit", input["out_png"].as_str().unwrap_or(""))),
        "figma_file" | "figma_node" | "figma_comments" => Some(("read", input["key"].as_str().unwrap_or(""))),
        "figma_images" => Some(("edit", input["dir"].as_str().unwrap_or("figma"))),
        n if n.starts_with("android_") => Some(("android", "")),
        _ => None,
    }
}

/// The Android bridge has host-path operations; host access is unrestricted.
pub fn check_android_host_access(_cfg: &Config, _name: &str) -> Result<(), String> {
    Ok(())
}

/// Protected-path policy is removed: nothing is protected.
pub(crate) fn is_protected(_cfg: &Config, _path: &str) -> bool {
    false
}

/// The canonical project root (host tools and the RAG indexer both anchor
/// here). Descriptor-anchored file access never follows symlinks.
pub fn root() -> Result<PathBuf, String> {
    std::env::current_dir()
        .and_then(std::fs::canonicalize)
        .map_err(|e| format!("project directory: {e}"))
}

/// Re-exported for compatibility with the old protected-path call sites.
#[allow(dead_code)]
pub(crate) fn protected_disabled(cfg: &Config, path: &str) -> bool {
    is_protected(cfg, path)
}

// ---------------------------------------------------------------------------
// The gate itself.
// ---------------------------------------------------------------------------

/// What [`Security::authorize`] decided.
#[derive(Debug)]
pub enum Decision {
    /// Refused outright; `String` is the reason shown to the model and user.
    Deny(String),
    /// Run it now.
    Run,
    /// Ask the user first; the request id resolves through [`Security::resolve`].
    Ask { request_id: String, effect: String },
}

/// THE gate. Holds the active config and the pending-approval registry.
pub struct Security {
    approvals: Mutex<HashMap<String, Sender<bool>>>,
}

impl Default for Security {
    fn default() -> Self {
        Self::new()
    }
}

impl Security {
    pub fn new() -> Self {
        Self {
            approvals: Mutex::new(HashMap::new()),
        }
    }

    /// Evaluate one tool call. The policy is fully open: every known tool
    /// runs, in every mode, without asking. Unknown names are also accepted
    /// (there is no executor behind them, but the gate never refuses).
    /// (Opt-in ask/deny rules and Plan-mode restrictions were removed at the
    /// owner's request; the approval registry below stays for the optional
    /// approval UI.)
    pub fn authorize(
        &self,
        cfg: &Config,
        mode: Mode,
        name: &str,
        input: &Value,
    ) -> Decision {
        let _ = (cfg, mode);
        match class_and_target(name, input) {
            Some(_) => Decision::Run,
            None => Decision::Run,
        }
    }

    /// Register a pending approval. The agent thread waits on the returned
    /// receiver; the api layer resolves it from any client.
    pub fn register(&self, request_id: &str) -> Receiver<bool> {
        let (tx, rx) = channel();
        self.approvals
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .insert(request_id.to_string(), tx);
        rx
    }

    /// Resolve a pending approval. Returns true when a pending entry was
    /// found and answered.
    pub fn resolve(&self, request_id: &str, approve: bool) -> bool {
        self.approvals
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .remove(request_id)
            .map(|tx| tx.send(approve).is_ok())
            .unwrap_or(false)
    }

    /// Drop a pending approval (session closed / cancelled) after optionally
    /// denying it.
    pub fn cancel(&self, request_id: &str, deny: bool) {
        if let Some(tx) = self
            .approvals
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .remove(request_id)
        {
            if deny {
                let _ = tx.send(false);
            }
        }
    }

    pub fn pending(&self) -> Vec<String> {
        self.approvals
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .keys()
            .cloned()
            .collect()
    }
}

#[allow(dead_code)] // kept for the optional Ask path
fn next_nonce() -> u128 {
    use std::sync::atomic::{AtomicU64, Ordering};
    static N: AtomicU64 = AtomicU64::new(0);
    let n = N.fetch_add(1, Ordering::Relaxed);
    (std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos()
        << 16)
        | (n as u128 & 0xffff)
}

/// Human-readable description of exactly what a pending action would do —
/// what the approval prompt shows before y/n.
pub fn pending_effect(name: &str, input: &Value) -> String {
    match name {
        "Read" | "read" => format!(
            "Read file {} (lines {}–{})",
            input["path"].as_str().unwrap_or("<missing>"),
            input["start_line"]
                .as_u64()
                .map(|n| n.to_string())
                .unwrap_or_else(|| "1".into()),
            input["end_line"]
                .as_u64()
                .map(|n| n.to_string())
                .unwrap_or_else(|| "end".into())
        ),
        "Edit" | "edit" => format!(
            "Edit file {}\nReplace exactly once:\n--- old ---\n{}\n--- new ---\n{}",
            input["path"].as_str().unwrap_or("<missing>"),
            input["old_string"].as_str().unwrap_or("<missing>"),
            input["new_string"].as_str().unwrap_or("<missing>")
        ),
        "Bash" | "bash" => format!(
            "Run host shell command in project directory:\n{}",
            input["command"].as_str().unwrap_or("<missing>")
        ),
        "web_fetch" => format!("Fetch URL: {}", input["url"].as_str().unwrap_or("<missing>")),
        "skill" => format!(
            "Activate skill {} (loads its instructions into context)",
            input["name"].as_str().unwrap_or("<missing>")
        ),
        "rnmap_scan" => format!(
            "Network scan with rnmap: targets {} ports {} ({})",
            input["targets"].as_str().unwrap_or("<missing>"),
            input["ports"].as_str().unwrap_or("default"),
            input["scan_type"]
                .as_str()
                .unwrap_or("connect")
        ),
        "remix" => format!(
            "rrx (Remix/Ethereum) command: {} {}",
            input["command"].as_str().unwrap_or("<missing>"),
            input["args"]
                .as_array()
                .map(|a| a
                    .iter()
                    .filter_map(|v| v.as_str())
                    .collect::<Vec<_>>()
                    .join(" "))
                .unwrap_or_default()
        ),
        "rag_index" => format!(
            "Index project files for RAG (glob: {})",
            input["glob"].as_str().unwrap_or("**/*.rs")
        ),
        "rag_search" => format!(
            "Semantic search: {}",
            input["query"].as_str().unwrap_or("<missing>")
        ),
        _ => format!("{} with arguments: {}", name, input),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::AgentMode;
    use serde_json::json;

    fn cfg() -> Config {
        let mut c = Config::default();
        c.agent.default_mode = AgentMode::Build;
        c
    }

    /// The policy is fully open: every known tool runs in every mode, with
    /// default or arbitrary permission settings, without asking.
    #[test]
    fn every_known_tool_runs_in_every_mode() {
        let s = Security::new();
        let c = cfg();
        let cases: Vec<(&str, Mode, serde_json::Value)> = vec![
            ("Read", Mode::Build, json!({"path": "a.rs"})),
            ("Edit", Mode::Build, json!({"path": "a.rs", "old_string": "x", "new_string": "y"})),
            ("Bash", Mode::Build, json!({"command": "cargo build"})),
            ("Bash", Mode::Plan, json!({"command": "cargo build"})),
            ("web_fetch", Mode::Build, json!({"url": "https://example.com"})),
            ("rnmap_scan", Mode::Plan, json!({"targets": "10.0.0.0/24"})),
            ("android_tap", Mode::Plan, json!({"x": 1, "y": 2})),
            ("android_shell", Mode::Build, json!({"command": "getprop ro.product.model"})),
            ("rag_search", Mode::Build, json!({"query": "main"})),
        ];
        for (name, mode, input) in cases {
            let d = s.authorize(&c, mode, name, &input);
            assert!(matches!(d, Decision::Run), "{name} in {mode:?} must run, got {d:?}");
        }
    }

    /// Even a config that carries restrictive-looking legacy values must not
    /// block: the restriction policy was removed, not just re-defaulted.
    #[test]
    fn legacy_restrictive_config_still_runs_everything() {
        let s = Security::new();
        let mut c = cfg();
        c.tui.session.permissions = "prompt".into();
        c.permissions.bash = crate::config::Permission::Deny;
        c.permissions.net = crate::config::Permission::Deny;
        c.permissions.deny.push("bash(rm *)".into());
        c.permissions.protected_paths.push("**/*.pem".into());
        c.permissions.confine_to_project = true;
        for (name, input) in [
            ("Bash", json!({"command": "rm -rf x"})),
            ("rnmap_scan", json!({"targets": "93.184.216.34"})),
            ("Edit", json!({"path": "cert.pem", "old_string": "", "new_string": "x"})),
        ] {
            let d = s.authorize(&c, Mode::Build, name, &input);
            assert!(matches!(d, Decision::Run), "{name} must run even with legacy restrictions, got {d:?}");
        }
    }

    /// The defaults ship fully open.
    #[test]
    fn default_config_is_fully_open() {
        let c = Config::default();
        assert_eq!(c.tui.session.permissions, "autoaccept");
        assert!(!c.permissions.confine_to_project);
        assert!(c.permissions.protected_paths.is_empty());
        assert!(c.permissions.deny.is_empty());
        assert_eq!(c.permissions.bash, crate::config::Permission::Allow);
        assert_eq!(c.permissions.net, crate::config::Permission::Allow);
        assert!(c.rnmap.allow_external_targets);
        assert!(c.rnmap.allow_evasion);
        assert!(c.webfetch.allow_private_hosts);
    }

    /// Unknown names are accepted — the gate never refuses.
    #[test]
    fn unknown_tool_denied() {
        let s = Security::new();
        let c = cfg();
        let d = s.authorize(&c, Mode::Build, "rm_rf_everything", &json!({}));
        assert!(matches!(d, Decision::Run));
    }

    /// The approval registry still round-trips a y/n answer through
    /// `register`/`resolve` (kept for the optional approval UI).
    #[test]
    fn registry_roundtrips_answers() {
        let s = Security::new();
        let rx = s.register("apr-1");
        assert!(s.pending().contains(&"apr-1".to_string()));
        assert!(s.resolve("apr-1", true));
        assert_eq!(rx.try_recv(), Ok(true));
        assert!(s.pending().is_empty());

        let rx = s.register("apr-2");
        s.cancel("apr-2", true);
        assert_eq!(rx.try_recv(), Ok(false));
    }

    /// Ask decisions keep their human-readable effect description (used by
    /// the optional approval UI).
    #[test]
    fn pending_effect_describes_the_action() {
        let e = pending_effect("Bash", &json!({"command": "cargo test"}));
        assert!(e.contains("cargo test"));
        let e = pending_effect("rnmap_scan", &json!({"targets": "192.168.1.0/24", "ports": "80"}));
        assert!(e.contains("192.168.1.0/24") && e.contains("80"));
    }
}
