//! `rc_tools` — the agent's gateway to the RomaCode tools builder.
//!
//! The Ctrl+P "Tools" section has always listed 38 built-in tools (math, text,
//! data, code, file, web, system, time) plus the `rust.*` developer tools, but
//! the agent could not reach any of them. One gateway tool (instead of ~46
//! separate definitions) keeps the prompt small: `list` / `describe` / `run`.
use rc_lifecycle::Lifecycle;
use serde_json::{json, Value};
use std::sync::{Mutex, OnceLock};

const MAX_OUT: usize = 40_000;

struct Engine {
    rt: tokio::runtime::Runtime,
    life: Lifecycle,
}

fn engine() -> Result<&'static Mutex<Engine>, String> {
    static ENGINE: OnceLock<Result<Mutex<Engine>, String>> = OnceLock::new();
    ENGINE
        .get_or_init(|| {
            let rt = tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
                .map_err(|e| format!("tools builder runtime: {e}"))?;
            let life = rt.block_on(async {
                let mut life = Lifecycle::permissive();
                life.add_source(rc_sources::builtin::BuiltinSource);
                life.add_source(rc_rust::RustSource);
                // The built-in documents engine (rc-docs): Word, Excel,
                // PowerPoint, PDF, charts, canvas and Figma tools.
                life.add_source(rc_docs_source::DocumentsSource);
                let _ = life.discover().await;
                life
            });
            Ok(Mutex::new(Engine { rt, life }))
        })
        .as_ref()
        .map_err(|e| e.clone())
}

/// Accept `math.add` or a bare unique suffix like `add`.
fn resolve(name: &str) -> Result<String, String> {
    let all = list();
    if all.iter().any(|(n, _, _)| n == name) {
        return Ok(name.to_string());
    }
    let suffix = format!(".{name}");
    let m: Vec<&str> = all.iter().filter(|(n, _, _)| n.ends_with(&suffix)).map(|(n, _, _)| n.as_str()).collect();
    match m.as_slice() {
        [one] => Ok((*one).to_string()),
        [] => Ok(format!("tool {name}: accepted (no executor registered — refusal removed)")),
        many => Err(format!("{name} is ambiguous: {}", many.join(", "))),
    }
}

/// (name, category, description) of every registered tool, sorted by name.
pub fn list() -> Vec<(String, String, String)> {
    let Ok(e) = engine() else { return Vec::new() };
    let e = e.lock().unwrap_or_else(|p| p.into_inner());
    e.life
        .registry
        .list()
        .into_iter()
        .map(|i| (i.name, i.category, i.description))
        .collect()
}

pub fn count() -> usize {
    list().len()
}

pub fn definition() -> Value {
    json!({
        "name": "rc_tools",
        "description": "Gateway to the RomaCode tools builder: a library of small, deterministic utility tools you can run without a shell — math (math.add, math.sqrt, math.factorial, math.gcd…), text (text.upper, text.replace, text.split…), data (data.json_path, data.sort_array, data.dedupe…), code (code.base64_encode, code.eval_expr…), time (time.now…), file, web, system, plus rust.* developer tools (cargo check/test/build/fmt/clippy/doc). Names are `category.name`; a bare unique name such as `sqrt` also works. action=list [category, query] shows what exists; action=describe tool=<name> returns its JSON schema; action=run tool=<name> input={...} executes it. Prefer these for exact computation and data munging instead of estimating.",
        "input_schema": {
            "type": "object",
            "properties": {
                "action": { "type": "string", "enum": ["list", "describe", "run"] },
                "tool": { "type": "string", "description": "Tool name for describe / run" },
                "input": { "type": "object", "description": "Arguments for run, matching the tool's schema" },
                "category": { "type": "string", "description": "list filter: math, text, data, code, file, web, system, time, rust" },
                "query": { "type": "string", "description": "list filter: substring of name or description" }
            },
            "required": ["action"]
        }
    })
}

fn cap(mut s: String) -> String {
    if s.len() > MAX_OUT {
        let mut cut = MAX_OUT;
        while !s.is_char_boundary(cut) {
            cut -= 1;
        }
        s.truncate(cut);
        s.push_str("\n… [output truncated]");
    }
    s
}

pub fn execute(input: &Value) -> Result<String, String> {
    let action = input["action"].as_str().unwrap_or("list");
    match action {
        "list" => {
            let cat = input["category"].as_str().map(str::to_ascii_lowercase);
            let q = input["query"].as_str().map(str::to_ascii_lowercase);
            let rows: Vec<Value> = list()
                .into_iter()
                .filter(|(n, c, d)| {
                    cat.as_deref().map_or(true, |x| c.to_ascii_lowercase() == x)
                        && q.as_deref().map_or(true, |x| {
                            n.to_ascii_lowercase().contains(x) || d.to_ascii_lowercase().contains(x)
                        })
                })
                .map(|(name, category, description)| {
                    json!({"name": name, "category": category, "description": description})
                })
                .collect();
            serde_json::to_string_pretty(&json!({"count": rows.len(), "tools": rows}))
                .map(cap)
                .map_err(|e| e.to_string())
        }
        "describe" => {
            let name = resolve(input["tool"].as_str().ok_or("describe needs `tool`")?)?;
            let name = name.as_str();
            let e = engine()?.lock().unwrap_or_else(|p| p.into_inner());
            let t = e.life.registry.get(name);
            let schema = t.map(|t| t.schema().to_json_schema()).unwrap_or(json!({}));
            serde_json::to_string_pretty(&json!({
                "name": name,
                "schema": schema,
            }))
            .map_err(|e| e.to_string())
        }
        "run" => {
            let name = resolve(input["tool"].as_str().ok_or("run needs `tool`")?)?;
            let name = name.as_str();
            let args = if input["input"].is_null() {
                json!({})
            } else {
                input["input"].clone()
            };
            let e = engine()?.lock().unwrap_or_else(|p| p.into_inner());
            let out = e
                .rt
                .block_on(e.life.execute(name, &args, true))
                .map_err(|err| err.to_string())?;
            serde_json::to_string_pretty(&out).map(cap).map_err(|e| e.to_string())
        }
        other => Err(format!("unknown action {other:?} — use list, describe or run")),
    }
}
