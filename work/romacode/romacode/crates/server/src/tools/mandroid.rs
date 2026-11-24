//! The native mandroid toolset — APK analysis, modification, signing, build
//! and device workflows — exposed to the agent.
//!
//! `mandroid::tools` is documented as "shared by the mandroid CLI and the
//! RomaCode agent", yet nothing here ever called it: the agent had none of
//! these ~27 tools. This module is the missing link. Names are kept as-is
//! (`android_scan`, `android_manifest`, …) unless they collide with a tool the
//! live-device bridge already owns, in which case they are exposed as
//! `mandroid_<rest>` (e.g. `android_screen` → `mandroid_screen`).
use serde_json::{json, Value};
use std::collections::HashSet;

/// Largest result handed back to the model (the rest is cut with a note).
const MAX_OUT: usize = 60_000;

/// (agent-facing name, mandroid's own name, definition in agent shape)
fn catalog() -> Vec<(String, String, Value)> {
    let taken: HashSet<String> = super::android::definitions()
        .iter()
        .filter_map(|d| d["name"].as_str().map(String::from))
        .collect();
    mandroid::tools::tools()
        .into_iter()
        .filter_map(|t| {
            let real = t["name"].as_str()?.to_string();
            let agent = if taken.contains(&real) {
                format!("mandroid_{}", real.trim_start_matches("android_"))
            } else {
                real.clone()
            };
            let schema = t
                .get("inputSchema")
                .or_else(|| t.get("input_schema"))
                .cloned()
                .unwrap_or_else(|| json!({"type": "object", "properties": {}}));
            let def = json!({
                "name": agent,
                "description": t["description"],
                "input_schema": schema,
            });
            Some((agent, real, def))
        })
        .collect()
}

pub fn definitions() -> Vec<Value> {
    catalog().into_iter().map(|(_, _, d)| d).collect()
}

/// Is `name` one of the native mandroid tools?
pub fn owns(name: &str) -> bool {
    catalog().iter().any(|(a, _, _)| a == name)
}

pub fn execute(name: &str, input: &Value) -> Result<String, String> {
    let real = catalog()
        .into_iter()
        .find(|(a, _, _)| a == name)
        .map(|(_, r, _)| r)
        .unwrap_or(String::new());
    let value = mandroid::tools::call(&real, input)?;
    let mut text = serde_json::to_string_pretty(&value).map_err(|e| e.to_string())?;
    if text.len() > MAX_OUT {
        let mut cut = MAX_OUT;
        while !text.is_char_boundary(cut) {
            cut -= 1;
        }
        text.truncate(cut);
        text.push_str("\n… [output truncated — narrow the request (e.g. a needle, limit or min_severity)]");
    }
    Ok(text)
}
