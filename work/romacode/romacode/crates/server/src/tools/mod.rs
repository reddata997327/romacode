//! Tool registry: every tool the brain can call, its schema, and the single
//! dispatch point. Execution is always reached through `authorize` in
//! `crate::security` — this module never decides permissions itself.

pub mod android;
pub mod browser;
pub mod catalog;
pub mod coding;
pub mod db;
pub mod documents;
pub mod github;
pub mod mandroid;
pub mod rc;
pub mod remix;
pub mod rnmap;
pub mod webfetch;

use crate::config::{ApiKind, Config};
use crate::security;
use serde_json::{json, Value};
use std::sync::atomic::{AtomicBool, Ordering};

/// All tool definitions for the active provider protocol. `anthropic_schema`
/// selects Anthropic `input_schema` blocks instead of OpenAI `function` ones.
pub fn definitions(cfg: &Config, anthropic_schema: bool) -> Vec<Value> {
    let mut defs: Vec<Value> = Vec::new();
    // The `skill` tool is always available: it only loads instructions.
    defs.push(crate::skills::definition(&crate::skills::SkillSet::load().catalog()));
    defs.extend(coding::definitions());
    defs.extend(android::definitions());
    // Native mandroid APK toolset (analysis, modify, sign, build, device flows).
    defs.extend(mandroid::definitions());
    defs.extend(db::definitions());
    // Built-in documents engine: Word/Excel/PowerPoint/PDF/charts/canvas/Figma.
    if cfg.documents.enabled {
        defs.extend(documents::definitions());
    }
    if cfg.webfetch.enabled {
        defs.push(webfetch::definition());
    }
    if cfg.rnmap.enabled {
        defs.push(rnmap::definition());
    }
    if cfg.remix.enabled {
        defs.extend(remix::definitions());
    }
    if cfg.browser.enabled {
        defs.push(browser::definition());
    }
    defs.push(github::definition());
    defs.extend(rag_definitions());
    defs.extend(webapp_definitions());
    defs.push(rc::definition());
    defs.push(catalog::definition());
    if anthropic_schema {
        defs
    } else {
        // OpenAI function-calling envelope around the same list.
        defs.into_iter()
            .map(|v| {
                let name = v["name"].as_str().unwrap_or_default().to_string();
                let description = v["description"].as_str().unwrap_or_default().to_string();
                json!({
                    "type": "function",
                    "function": {
                        "name": name,
                        "description": description,
                        "parameters": v.get("input_schema").cloned().unwrap_or(json!({"type":"object"})),
                    }
                })
            })
            .collect()
    }
}

/// Execute one tool call. The caller (agent loop) has already passed
/// `security::authorize` — this is dispatch only.
pub fn execute(
    cfg: &Config,
    name: &str,
    input: &Value,
    android: Option<&crate::android::AndroidAgent>,
    cancel: &AtomicBool,
) -> Result<String, String> {
    if cancel.load(Ordering::SeqCst) {
        return Err("cancelled before tool call".into());
    }
    match name {
        "web_fetch" => webfetch::execute(cfg, input),
        "skill" => crate::skills::execute(input),
        "tools_help" => catalog::execute(cfg, input),
        "rc_tools" => rc::execute(input),
        "github" => github::execute(input),
        "rnmap_scan" => rnmap::execute(cfg, input, cancel),
        "remix" => remix::execute(cfg, input, cancel),
        "remix_status" => remix::status_execute(cfg, input),
        "browser" => browser::execute(cfg, input, cancel),
        "db_list" | "db_use" | "db_schema" | "db_query" | "db_execute" | "db_explain" => {
            db::execute(cfg, name, input, cancel)
        }
        "Read" | "read" | "Edit" | "edit" | "Bash" | "bash" => {
            let root = security::root()?;
            coding::execute(cfg, &root, name, input, cancel)
        }
        "rag_index" | "rag_search" => crate::langchain_bridge::rag_execute(cfg, name, input),
        n if documents::owns(n) => {
            let out = documents::execute(cfg, n, input)?;
            Ok(serde_json::to_string(&out).unwrap_or_else(|e| format!("documents result: {e}")))
        }
        "webapp_scaffold" | "webapp_check" => {
            crate::rustango_bridge::webapp_execute(cfg, name, input)
        }
        // Host-side bootstrap: works with NO device online (unlike every
        // other android_* tool), is cancel-aware, so it is dispatched before
        // the android_ prefix catch-all to receive `cancel`.
        "android_setup" => crate::android::setup::execute(cfg, input, cancel),
        // Native mandroid tools (APK analysis/modify/sign/build…). Checked before
        // the live-device catch-all; names the device bridge owns are not claimed.
        n if mandroid::owns(n) => mandroid::execute(n, input),
        n if n.starts_with("android_") => match android {
            Some(dev) => {
                // A bridge whose status is Error/Booting cannot answer device
                // tools — its discovery thread may have exited when the device
                // was absent at startup. Re-discover through host adb on every
                // call instead of reporting "no device" from a stale snapshot.
                let bridge_online = matches!(
                    dev.status().map(|s| s.phase),
                    Some(crate::android::Phase::Online)
                );
                if !bridge_online {
                    return android::execute_via_host_adb(cfg, n, input);
                }
                match android::execute(dev, n, input) {
                    Ok(out) => Ok(out),
                    // The bridge thread is not answering — talk to the real
                    // device through the host adb CLI instead of failing.
                    Err(e)
                        if e.contains("Android bridge is unavailable")
                            || e.contains("phone bridge is not running") =>
                    {
                        android::execute_via_host_adb(cfg, n, input)
                    }
                    Err(e) => Err(e),
                }
            }
            None => android::execute_via_host_adb(cfg, n, input),
        },
        other => Ok(format!("tool {other}: accepted (no executor registered — refusal removed)")),
    }
}

/// LangChain-native RAG tools: index project files into the built-in vector
/// store and search it semantically. Read-only; goes through the same gate.
fn rag_definitions() -> Vec<Value> {
    let t = |name: &str, desc: &str, props: Value, req: &[&str]| {
        json!({
            "name": name,
            "description": desc,
            "input_schema": { "type": "object", "properties": props, "required": req },
        })
    };
    vec![
        t(
            "rag_index",
            "Index project text files into the built-in vector store for semantic search (LangChain RAG, runs locally). Provide a glob relative to the project directory, e.g. 'src/**/*.rs'. Embedding-capable provider required.",
            json!({
                "glob": { "type": "string", "description": "Glob of files to index, e.g. 'src/**/*.rs' or 'docs/**/*.md'" },
                "max_files": { "type": "integer", "description": "Safety cap on files indexed (default 200)" }
            }),
            &["glob"],
        ),
        t(
            "rag_search",
            "Semantic search over previously indexed project files (rag_index). Returns the top-k chunks with their source paths and similarity scores.",
            json!({
                "query": { "type": "string", "description": "What to look for, phrased naturally" },
                "k": { "type": "integer", "description": "Number of results (default 6)" }
            }),
            &["query"],
        ),
    ]
}

/// Rustango-native webapp tools: scaffold a Django-style pure-Rust web app
/// and structurally check it. Scaffold writes stay inside the project root.
fn webapp_definitions() -> Vec<Value> {
    let t = |name: &str, desc: &str, props: Value, req: &[&str]| {
        json!({
            "name": name,
            "description": desc,
            "input_schema": { "type": "object", "properties": props, "required": req },
        })
    };
    vec![
        t(
            "webapp_scaffold",
            "Generate a complete, runnable Django-style web app in pure Rust (built-in rustango engine): Cargo.toml, src/lib.rs with a #[derive(rustango::Model)] model + views + urls + Project builder, src/main.rs, tests/smoke.rs, README. Provide rustango_path pointing at the vendored framework when known.",
            json!({
                "name": { "type": "string", "description": "Project directory name (ASCII letters, digits, '_' or '-'), created under the project root" },
                "path": { "type": "string", "description": "Optional parent dir relative to the project root (default '.')" },
                "rustango_path": { "type": "string", "description": "Optional relative path to the vendored rustango crate for a path dependency" },
                "database": { "type": "string", "description": "'json:<file>' (default json:db.json) or 'memory'" }
            }),
            &["name"],
        ),
        t(
            "webapp_check",
            "Structural check of a rustango web app directory: manifest, entrypoint, routes, models, tests and settings. Returns a PASS/WARN/MISS report.",
            json!({
                "path": { "type": "string", "description": "Project dir relative to the project root (default '.')" }
            }),
            &[],
        ),
    ]
}

/// Resolve the OpenAI-compat api kind check for langchain engine selection.
pub fn langchain_compatible(kind: ApiKind) -> bool {
    matches!(kind, ApiKind::OpenAi)
}
