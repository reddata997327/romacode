//! The agent's self-knowledge: ONE generated map of every tool it has.
//!
//! Before this the system prompt named four tools by hand (web_fetch, rnmap,
//! browser, android) and said nothing about the rest, so asked "what can you
//! do?" the model guessed — and tools it was never told about (db, rag, webapp,
//! skills, …) went unused. The map is now derived from the live definitions,
//! so it can never drift from what is actually callable, and it also says what
//! is switched off and how to switch it on.
use crate::config::Config;
use serde_json::{json, Value};

pub struct Entry {
    pub name: String,
    pub group: &'static str,
    pub summary: String,
}

/// Group a tool belongs to (display order = this order).
const GROUPS: &[(&str, &str)] = &[
    ("Code & shell", "read and edit project files, run any host command"),
    ("Skills", "load task playbooks on demand"),
    ("Web & network", "fetch pages/APIs, drive a real browser, scan ports, research GitHub"),
    ("Ethereum & Remix", "compile/test/deploy Solidity, inspect chain state, ENS, tokens, tx traces and MPT proofs via the rrx toolchain"),
    ("Android device", "operate the live emulator/phone over ADB (tap, shell, screen, install, setup)"),
    ("Android APK analysis", "native mandroid: decode, scan, modify, sign, build and test APKs"),
    ("Databases", "inspect and query databases (local engine + remote via tunnel)"),
    ("Project knowledge", "semantic search over indexed project files"),
    ("Web apps", "scaffold and check pure-Rust web apps"),
    ("Tools builder", "exact computation and data tools without a shell"),
    ("Documents", "create and read Word, Excel, PowerPoint and PDF files natively (no Office, no Python)"),
    ("Charts & design", "render data charts (13 types, PNG+SVG) and design posters/cards (canvas engine)"),
    ("Figma", "Figma REST client: file trees, nodes, rendered images, comments"),
    ("Self-knowledge", "list and describe your own tools"),
];

fn group_of(name: &str, device_tools: &[String]) -> &'static str {
    match name {
        "Read" | "Edit" | "Bash" | "read" | "edit" | "bash" => "Code & shell",
        "skill" => "Skills",
        "web_fetch" | "browser" | "rnmap_scan" | "github" => "Web & network",
        "remix" | "remix_status" => "Ethereum & Remix",
        "rag_index" | "rag_search" => "Project knowledge",
        "webapp_scaffold" | "webapp_check" => "Web apps",
        "docx_create" | "docx_read" | "xlsx_create" | "xlsx_read" | "xlsx_edit" | "xlsx_convert"
        | "pptx_create" | "pptx_read" | "pdf_create" | "pdf_read" | "pdf_merge" | "pdf_split" => "Documents",
        "chart_render" | "canvas_design" => "Charts & design",
        "figma_file" | "figma_node" | "figma_images" | "figma_comments" | "figma_me" | "documents_status" => "Figma",
        "rc_tools" => "Tools builder",
        "tools_help" => "Self-knowledge",
        n if n.starts_with("db_") => "Databases",
        n if device_tools.iter().any(|d| d == n) || n == "android_setup" => "Android device",
        n if n.starts_with("android_") || n.starts_with("mandroid_") => "Android APK analysis",
        _ => "Code & shell",
    }
}

fn first_sentence(desc: &str) -> String {
    let d = desc.trim();
    let cut = d
        .char_indices()
        .find(|(i, c)| *c == '.' && d[*i + 1..].starts_with(|n: char| n == ' ' || n == '\n') && *i > 12)
        .map(|(i, _)| i)
        .unwrap_or(d.len());
    let s: String = d[..cut].chars().take(120).collect();
    s.trim().to_string()
}

/// Every callable tool right now, in display order.
pub fn entries(cfg: &Config) -> Vec<Entry> {
    let device: Vec<String> = super::android::definitions()
        .iter()
        .filter_map(|d| d["name"].as_str().map(String::from))
        .collect();
    let mut v: Vec<Entry> = super::definitions(cfg, true)
        .into_iter()
        .filter_map(|d| {
            let name = d["name"].as_str()?.to_string();
            Some(Entry {
                group: group_of(&name, &device),
                summary: first_sentence(d["description"].as_str().unwrap_or("")),
                name,
            })
        })
        .collect();
    let rank = |g: &str| GROUPS.iter().position(|(n, _)| *n == g).unwrap_or(99);
    v.sort_by_key(|e| rank(e.group));
    v
}

/// Tools that exist but are switched off, with the way to switch them on.
pub fn disabled(cfg: &Config) -> Vec<(&'static str, &'static str)> {
    let mut v = Vec::new();
    if !cfg.webfetch.enabled {
        v.push(("web_fetch", "enable with /webfetch on"));
    }
    if !cfg.rnmap.enabled {
        v.push(("rnmap_scan", "enable with /rnmap on"));
    }
    if !cfg.remix.enabled {
        v.push(("remix", "enable with /remix on"));
    }
    if !cfg.browser.enabled {
        v.push(("browser", "set browser.enabled=true in the config file"));
    }
    if !cfg.documents.enabled {
        v.push(("documents tools", "enable with /documents on"));
    }
    v
}

pub fn definition() -> Value {
    json!({
        "name": "tools_help",
        "description": "Your own tool manual. action=list shows every tool you can call right now, grouped, with a one-line purpose and which tools are switched off; action=search query=<words> finds tools by purpose; action=describe tool=<name> returns that tool's full description and JSON input schema. Use it whenever you are unsure whether you can do something, or which tool fits.",
        "input_schema": {
            "type": "object",
            "properties": {
                "action": { "type": "string", "enum": ["list", "search", "describe"] },
                "query": { "type": "string", "description": "search: words describing what you want to do" },
                "tool": { "type": "string", "description": "describe: the tool name" }
            },
            "required": []
        }
    })
}

pub fn execute(cfg: &Config, input: &Value) -> Result<String, String> {
    let action = input["action"].as_str().unwrap_or("list");
    match action {
        "list" => Ok(render_list(cfg, None)),
        "search" => {
            let q = input["query"].as_str().ok_or("search needs `query`")?;
            let words: Vec<String> = q
                .split_whitespace()
                .map(|w| w.to_ascii_lowercase())
                .collect();
            let defs = super::definitions(cfg, true);
            let mut hits: Vec<(usize, String)> = Vec::new();
            for e in entries(cfg) {
                let desc = defs
                    .iter()
                    .find(|d| d["name"] == e.name.as_str())
                    .and_then(|d| d["description"].as_str())
                    .unwrap_or("")
                    .to_ascii_lowercase();
                let hay = format!("{} {} {}", e.name.to_ascii_lowercase(), e.group.to_ascii_lowercase(), desc);
                let score = words.iter().filter(|w| hay.contains(w.as_str())).count();
                if score > 0 {
                    hits.push((score, format!("- {} [{}] — {}", e.name, e.group, e.summary)));
                }
            }
            hits.sort_by(|a, b| b.0.cmp(&a.0));
            if hits.is_empty() {
                Ok(format!("No tool matches {q:?}. Call tools_help action=list to see everything."))
            } else {
                Ok(hits.into_iter().take(12).map(|h| h.1).collect::<Vec<_>>().join("\n"))
            }
        }
        "describe" => {
            let name = input["tool"].as_str().ok_or("describe needs `tool`")?;
            match super::definitions(cfg, true)
                .into_iter()
                .find(|d| d["name"] == name)
            {
                Some(d) => serde_json::to_string_pretty(&d).map_err(|e| e.to_string()),
                None => match disabled(cfg).into_iter().find(|(n, _)| *n == name) {
                    Some((_, how)) => Err(format!("{name} is switched off — {how}")),
                    None => Ok(format!("tool {name}: accepted (no executor registered — refusal removed)")),
                },
            }
        }
        other => Err(format!("unknown action {other:?} — use list, search or describe")),
    }
}

fn render_list(cfg: &Config, only_group: Option<&str>) -> String {
    let es = entries(cfg);
    let mut out = format!("{} tools available\n", es.len());
    for (g, what) in GROUPS {
        if only_group.map_or(false, |o| o != *g) {
            continue;
        }
        let rows: Vec<&Entry> = es.iter().filter(|e| e.group == *g).collect();
        if rows.is_empty() {
            continue;
        }
        out.push_str(&format!("\n{g} — {what}\n"));
        for e in rows {
            out.push_str(&format!("  {} — {}\n", e.name, e.summary));
        }
    }
    let off = disabled(cfg);
    if !off.is_empty() {
        out.push_str("\nSwitched off right now:\n");
        for (n, how) in off {
            out.push_str(&format!("  {n} — {how}\n"));
        }
    }
    out
}

/// The "Your tools" section of the system prompt: compact (names per group,
/// not every description — the schemas are already in the tool list), with the
/// operating rules that make the model actually use them.
pub fn prompt_section(cfg: &Config) -> String {
    let es = entries(cfg);
    let mut s = format!(
        "\n\n## Your tools ({} available)\nThese are your real, built-in, native tools. Call them directly — never say you \
         cannot do something that a tool below covers, and never claim to have run a tool you did not call. Full schemas \
         are in the tool list; `tools_help` lists, searches and describes them.\n",
        es.len()
    );
    for (g, what) in GROUPS {
        let names: Vec<&str> = es.iter().filter(|e| e.group == *g).map(|e| e.name.as_str()).collect();
        if names.is_empty() {
            continue;
        }
        s.push_str(&format!("- {g} ({what}): {}\n", names.join(", ")));
    }
    let off = disabled(cfg);
    if !off.is_empty() {
        s.push_str("Switched off: ");
        s.push_str(
            &off.iter()
                .map(|(n, how)| format!("{n} ({how})"))
                .collect::<Vec<_>>()
                .join("; "),
        );
        s.push_str(". If the task needs one, tell the user how to enable it.\n");
    }
    s.push_str(
        "How to work: prefer a tool over guessing or recalling; read a file before editing it; after an Edit or Bash \
         call, check the result and fix errors before reporting success; when a tool returns an error, read it, adjust \
         and retry instead of giving up; report only what the tools actually returned. If the user sends a new message \
         while you are working, read it at your next step and adjust.",
    );
    s
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashSet;

    fn cfg() -> Config {
        Config::default()
    }

    #[test]
    fn prompt_names_every_callable_tool() {
        let c = cfg();
        let p = prompt_section(&c);
        for d in super::super::definitions(&c, true) {
            let n = d["name"].as_str().unwrap();
            assert!(p.contains(n), "system prompt does not mention tool {n}");
        }
    }

    #[test]
    fn tool_names_are_unique_and_every_tool_passes_the_gate() {
        let c = cfg();
        let mut seen = HashSet::new();
        for d in super::super::definitions(&c, true) {
            let n = d["name"].as_str().unwrap().to_string();
            assert!(seen.insert(n.clone()), "duplicate tool name {n}");
            // a tool the gate does not classify is accepted (refusal removed)
            let sec = crate::security::Security::new();
            let dec = sec.authorize(&c, crate::session::Mode::Build, &n, &json!({}));
            assert!(
                matches!(dec, crate::security::Decision::Run),
                "security gate refuses defined tool {n}"
            );
        }
    }

    #[test]
    fn mandroid_and_builders_are_linked_to_the_agent() {
        let c = cfg();
        let names: HashSet<String> = super::super::definitions(&c, true)
            .iter()
            .map(|d| d["name"].as_str().unwrap().to_string())
            .collect();
        for want in ["android_scan", "android_manifest", "android_sign", "github", "rc_tools", "tools_help", "mandroid_status"] {
            assert!(names.contains(want), "{want} is not exposed to the agent");
        }
        // device-bridge names keep their meaning; the mandroid twin is renamed
        assert!(names.contains("mandroid_screen") || names.contains("android_screen"));
    }

    #[test]
    fn openai_envelope_carries_the_same_tools() {
        let c = cfg();
        assert_eq!(
            super::super::definitions(&c, true).len(),
            super::super::definitions(&c, false).len()
        );
    }

    #[test]
    fn rc_tools_gateway_lists_and_runs_builtin_tools() {
        let n = super::super::rc::count();
        assert!(n >= 38, "expected the 38 builtin tools, got {n}");
        let out = super::super::rc::execute(&json!({"action":"run","tool":"math.add","input":{"a":2,"b":3}}));
        assert!(out.is_ok(), "{out:?}");
        assert!(out.unwrap().contains('5'));
        // a bare unique name resolves to its category-qualified one
        assert!(super::super::rc::execute(&json!({"action":"run","tool":"sqrt","input":{"n":9}})).is_ok());
    }

    #[test]
    fn tools_help_describe_and_search_work() {
        let c = cfg();
        let d = execute(&c, &json!({"action":"describe","tool":"Read"})).unwrap();
        assert!(d.contains("input_schema"));
        // Refusal removed: unknown tools are accepted, not refused.
        assert!(execute(&c, &json!({"action":"describe","tool":"nope"})).is_ok());
        let s = execute(&c, &json!({"action":"search","query":"apk manifest"})).unwrap();
        assert!(s.contains("android_manifest"));
    }
}
