//! Tool-facing API.
//!
//! One implementation shared by the CLI and the MCP server, so a tool call and a
//! terminal command can never disagree about what a finding is.

use anyhow::Result;
use serde_json::{json, Value};

use crate::apk::Apk;
use crate::dex::Dex;
use crate::manifest::Manifest;
use crate::scan::{self, Rules};

/// Gather manifest + DEX strings once and hand back everything derived.
pub struct Analysis {
    pub manifest: Manifest,
    pub dex: Vec<(String, String, usize)>,
    pub strings: Vec<String>,
}

pub fn analyze(path: &str) -> Result<Analysis> {
    let mut apk = Apk::open(path)?;
    let root = apk.manifest()?;
    let manifest = Manifest::from_node(&root);

    let mut dex = Vec::new();
    let mut strings = Vec::new();
    for name in apk.dex_names() {
        let Ok(bytes) = apk.read(&name) else { continue };
        let Ok(d) = Dex::parse(&bytes) else { continue };
        let s = d.strings();
        dex.push((name, d.version(), s.len()));
        strings.extend(s);
    }
    Ok(Analysis {
        manifest,
        dex,
        strings,
    })
}

pub fn info(path: &str) -> Result<Value> {
    let mut apk = Apk::open(path)?;
    let entries = apk.entries();
    let libs = apk.native_libs();
    let mut abis: Vec<String> = libs.iter().map(|(a, _)| a.clone()).collect();
    abis.sort();
    abis.dedup();
    let a = analyze(path)?;
    Ok(json!({
        "path": path,
        "entries": entries.len(),
        "dex": a.dex.iter().map(|(n, v, c)| json!({"file": n, "version": v, "strings": c})).collect::<Vec<_>>(),
        "abis": abis,
        "native_libs": libs.len(),
        "has_resources_arsc": apk.has("resources.arsc"),
        "package": a.manifest.package,
        "min_sdk": a.manifest.min_sdk,
        "target_sdk": a.manifest.target_sdk,
        "components": a.manifest.components.len(),
    }))
}

/// The decoded manifest as JSON — the model, not the raw tree.
pub fn manifest(path: &str) -> Result<Value> {
    let a = analyze(path)?;
    Ok(serde_json::to_value(&a.manifest)?)
}

/// Static security analysis.
pub fn scan(path: &str, min_severity: &str) -> Result<Value> {
    let a = analyze(path)?;
    let rules = Rules::load()?;
    let findings = scan::scan(&a.manifest, &rules, &a.strings);
    let min = scan::severity_rank(min_severity);
    let shown: Vec<&scan::Finding> = findings
        .iter()
        .filter(|f| scan::severity_rank(&f.severity) >= min)
        .collect();
    let count = |s: &str| findings.iter().filter(|f| f.severity == s).count();
    Ok(json!({
        "package": a.manifest.package,
        "version_name": a.manifest.version_name,
        "min_sdk": a.manifest.min_sdk,
        "target_sdk": a.manifest.target_sdk,
        "rules_version": rules.version,
        "dex": a.dex.iter().map(|(n, v, c)| json!({"file": n, "version": v, "strings": c})).collect::<Vec<_>>(),
        "components": a.manifest.components,
        "counts": {
            "total": findings.len(),
            "critical": count("critical"),
            "high": count("high"),
            "medium": count("medium"),
            "low": count("low"),
            "info": count("info"),
        },
        "findings": shown,
    }))
}

/// DEX string search. `needle` is a case-insensitive substring; empty lists all
/// (bounded by `limit`).
pub fn strings(path: &str, needle: &str, limit: usize) -> Result<Value> {
    let a = analyze(path)?;
    let limit = limit.clamp(1, 5000);
    let n = needle.to_ascii_lowercase();
    let mut hits: Vec<&String> = a
        .strings
        .iter()
        .filter(|s| n.is_empty() || s.to_ascii_lowercase().contains(&n))
        .collect();
    let total = hits.len();
    hits.truncate(limit);

    // Flag which hits look like secrets, without echoing them in full.
    // Loading the rules file is best-effort: a malformed JSON or a missing
    // file must never break the string scan, but it should be loud on stderr
    // so an operator notices the rule pack is not actually being applied.
    let r = match Rules::load() {
        Ok(r) => r,
        Err(e) => {
            eprintln!(
                "romacode: android rules failed to load: {e:#}; using empty rule set"
            );
            Rules {
                version: String::new(),
                secret_patterns: vec![],
                rules: vec![],
            }
        }
    };
    let mut flagged = Vec::new();
    for sp in &r.secret_patterns {
        if let Ok(re) = regex::Regex::new(&sp.regex) {
            if a.strings.iter().any(|s| re.is_match(s)) {
                flagged.push(json!({"id": sp.id, "title": sp.title, "severity": sp.severity}));
            }
        }
    }

    Ok(json!({
        "total_strings": a.strings.len(),
        "matched": total,
        "returned": hits.len(),
        "secret_patterns_hit": flagged,
        "strings": hits,
    }))
}

/// The rule pack, so an agent can see (and explain) the judgement it applies.
pub fn rules() -> Result<Value> {
    let r = Rules::load()?;
    Ok(json!({
        "version": r.version,
        "rule_count": r.rules.len(),
        "secret_pattern_count": r.secret_patterns.len(),
        "rules": r.rules.iter().map(|x| json!({
            "id": x.id, "title": x.title, "severity": x.severity,
            "category": x.category, "cwe": x.cwe,
        })).collect::<Vec<_>>(),
    }))
}
