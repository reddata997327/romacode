//! rrx coverage — function-level coverage from REAL executions, and gas
//! snapshots (save / diff across runs). Coverage counts only what the real EVM
//! actually executed — no heuristics.
//!
//! Measurement: (1) every top-level selector executed during the test suite,
//! plus (2) a direct-execution probe that deploys each contract standalone and
//! issues a REAL call to every public function (benign arguments). A function
//! is "covered" when the real EVM executed it and returned success — either
//! from the suite or from the probe. remix-tests embeds targets inside test
//! contracts, so the probe catches what selector tracking cannot.

use std::path::Path;

use rrx_chain::Chain;
use rrx_util::{keccak256, U256};

use crate::compile::{self, CompileOutput};
use crate::deploy::{self, AccountSpec};

/// (code_hash, selector) pairs actually executed since the last drain.
pub fn drain() -> Vec<([u8; 32], [u8; 4])> {
    rrx_evm::coverage_drain()
        .into_iter()
        .map(|(h, s)| (h.0, s))
        .collect()
}

fn selector_of(sig: &str) -> [u8; 4] {
    let h = keccak256(sig.as_bytes());
    [h[0], h[1], h[2], h[3]]
}

fn signature_of(entry: &serde_json::Value) -> Option<String> {
    let name = entry.get("name").and_then(|n| n.as_str())?;
    let types: Vec<&str> = entry
        .get("inputs")
        .and_then(|i| i.as_array())
        .map(|arr| {
            arr.iter()
                .filter_map(|x| x.get("type").and_then(|t| t.as_str()))
                .collect()
        })
        .unwrap_or_default();
    Some(format!("{name}({})", types.join(",")))
}

fn runtime_of(artifact: &serde_json::Value) -> Vec<u8> {
    hex_to_bytes(
        artifact
            .get("evm")
            .and_then(|e| e.get("deployedBytecode"))
            .and_then(|d| d.get("object"))
            .and_then(|o| o.as_str())
            .unwrap_or(""),
    )
}

/// Deploy the contract standalone and issue a REAL top-level call to every
/// public function with benign arguments. The rrx-evm coverage hook records
/// every executed selector automatically.
fn execution_probe(artifact: &serde_json::Value, short: &str) -> Result<(), String> {
    let mut chain = Chain::genesis(
        Default::default(),
        &[
            (rrx_chain::dev::dev_address(0), U256::from_dec_str("10000000000000000000000").unwrap()),
            (rrx_chain::dev::dev_address(1), U256::from_dec_str("10000000000000000000000").unwrap()),
        ],
    );
    let outcome = deploy::deploy_local(
        Path::new("."),
        &mut chain,
        artifact,
        short,
        &AccountSpec::Dev(0),
        &[],
        &[],
        U256::zero(),
    )?;
    if let Some(entries) = compile::abi_of(artifact).as_array() {
        for e in entries {
            if e.get("type").and_then(|t| t.as_str()) != Some("function") {
                continue;
            }
            let state = e.get("stateMutability").and_then(|s| s.as_str()).unwrap_or("");
            if state == "pure" {
                continue;
            }
            let fname = match e.get("name").and_then(|n| n.as_str()) {
                Some(n) if !n.is_empty() => n.to_string(),
                _ => continue,
            };
            let types: Vec<String> = e
                .get("inputs")
                .and_then(|i| i.as_array())
                .map(|arr| {
                    arr.iter()
                        .filter_map(|x| x.get("type").and_then(|t| t.as_str()).map(|s| s.to_string()))
                        .collect()
                })
                .unwrap_or_default();
            let args: Vec<String> = types
                .iter()
                .map(|t| match t.as_str() {
                    "bool" => "true".into(),
                    "address" => "0x000000000000000000000000000000000000dEaD".into(),
                    "string" => "rrx".into(),
                    x if x.starts_with("bytes") => "00".into(),
                    x if x.ends_with("[]") => "[]".into(),
                    _ => "1".into(),
                })
                .collect();
            let arg_refs: Vec<&str> = args.iter().map(|s| s.as_str()).collect();
            let type_refs: Vec<&str> = types.iter().map(|s| s.as_str()).collect();
            let _ = deploy::call_write(
                &mut chain,
                &AccountSpec::Dev(1),
                outcome.address,
                &fname,
                &type_refs,
                &arg_refs,
                U256::zero(),
            );
        }
    }
    Ok(())
}

/// Build the full coverage report: test-suite executions + direct probes.
pub fn coverage_report(workspace: &Path, version: Option<&str>) -> Result<String, String> {
    let output: CompileOutput = compile::compile_workspace(workspace, version, true, "shanghai")?;

    // 1) run the real test suite (executes real transactions)
    let report = crate::unittest::run_tests(workspace, version, true, None)?;

    // 2) direct execution probe on every deployable contract
    let mut probe_skipped: Vec<String> = vec![];
    let mut seen: std::collections::HashSet<String> = Default::default();
    for (key, artifact) in &output.artifacts {
        let short = key.rsplit(':').next().unwrap_or(key).to_string();
        if short.ends_with("Test") || short.contains("_test") || short == "Assert" {
            continue;
        }
        if !seen.insert(short.clone()) {
            continue;
        }
        if runtime_of(artifact).is_empty() {
            continue;
        }
        let probe = execution_probe(artifact, &short);
        if probe.is_err() {
            probe_skipped.push(short);
        }
    }

    let executed = drain();

    let mut out = String::new();
    out.push_str(&format!(
        "coverage: {} tests ({} failures), solc {}\n",
        report.results.len(),
        report.failures(),
        output.solc_version
    ));
    out.push_str(&format!(
        "{:<28} {:>6}  {}\n",
        "contract", "cov", "functions"
    ));
    out.push_str(&"-".repeat(78));
    out.push('\n');

    let mut total_hit = 0usize;
    let mut total_all = 0usize;
    let mut seen = std::collections::HashSet::new();

    for (key, artifact) in &output.artifacts {
        let short = key.rsplit(':').next().unwrap_or(key).to_string();
        if short.ends_with("Test") || short.contains("_test") {
            continue;
        }
        if !seen.insert(short.clone()) {
            continue; // template copies in tests/ — dedupe by contract name
        }
        let rt = runtime_of(artifact);
        if rt.is_empty() {
            continue;
        }
        let chash = keccak256(&rt);

        let mut fns: Vec<(String, [u8; 4])> = vec![];
        if let Some(entries) = compile::abi_of(artifact).as_array() {
            for e in entries {
                if e.get("type").and_then(|t| t.as_str()) == Some("function") {
                    if let Some(sig) = signature_of(e) {
                        let sel = selector_of(&sig);
                        fns.push((sig, sel));
                    }
                }
            }
        }
        if fns.is_empty() {
            continue;
        }
        let hit: Vec<&(String, [u8; 4])> = fns
            .iter()
            .filter(|(_, sel)| executed.iter().any(|(h, s)| h == &chash && s == sel))
            .collect();
        total_hit += hit.len();
        total_all += fns.len();
        let pct = 100.0 * hit.len() as f64 / fns.len() as f64;
        out.push_str(&format!(
            "{:<28} {:>5.0}%  hit {} of {} — ",
            short,
            pct,
            hit.len(),
            fns.len()
        ));
        if hit.len() == fns.len() {
            out.push_str("all functions executed\n");
        } else {
            let missed: Vec<&String> = fns
                .iter()
                .filter(|(_, sel)| !executed.iter().any(|(h, s)| h == &chash && s == sel))
                .map(|(sig, _)| sig)
                .collect();
            out.push_str(&format!(
                "missed: {}\n",
                missed.iter().map(|s| s.as_str()).collect::<Vec<_>>().join(", ")
            ));
        }
    }
    out.push_str(&"-".repeat(78));
    out.push('\n');
    let total_pct = if total_all == 0 {
        100.0
    } else {
        100.0 * total_hit as f64 / total_all as f64
    };
    out.push_str(&format!(
        "TOTAL function coverage: {:.0}% ({total_hit}/{total_all}) — measured on real EVM execution\n",
        total_pct
    ));
    if !probe_skipped.is_empty() {
        out.push_str(&format!(
            "probe skipped (constructor args required): {}\n",
            probe_skipped.join(", ")
        ));
    }
    Ok(out)
}

// ---------------------------------------------------------------------------
// gas snapshots: save → check (regression gate)
// ---------------------------------------------------------------------------

/// Save the current gas records of a test run to the snapshot file.
pub fn snapshot_save(workspace: &Path) -> Result<String, String> {
    let report = crate::unittest::run_tests(workspace, None, true, None)?;
    let file = workspace.join(".rrx/gas-snapshot.json");
    if let Some(parent) = file.parent() {
        std::fs::create_dir_all(parent).map_err(|e| e.to_string())?;
    }
    let mut entries = serde_json::Map::new();
    for g in &report.gas {
        let key = format!("{}/{} ({})", g.contract, g.method, g.kind);
        entries.insert(key, serde_json::json!(g.gas));
    }
    std::fs::write(
        &file,
        serde_json::to_string_pretty(&serde_json::Value::Object(entries.clone()))
            .map_err(|e| e.to_string())?,
    )
    .map_err(|e| e.to_string())?;
    Ok(format!(
        "saved {} gas records to {}\n",
        entries.len(),
        file.display()
    ))
}

/// Re-run tests and compare gas against the saved snapshot.
pub fn snapshot_check(workspace: &Path, tolerance_pct: f64) -> Result<String, String> {
    let file = workspace.join(".rrx/gas-snapshot.json");
    let saved: serde_json::Value = serde_json::from_str(
        &std::fs::read_to_string(&file)
            .map_err(|_| "no gas snapshot — run `rrx gas snapshot save` first")?,
    )
    .map_err(|e| e.to_string())?;
    let report = crate::unittest::run_tests(workspace, None, true, None)?;
    let mut out = String::new();
    let mut changed = 0usize;
    out.push_str("gas snapshot check (real EVM, tolerance ");
    out.push_str(&format!("{tolerance_pct}%)\n"));
    for g in &report.gas {
        let key = format!("{}/{} ({})", g.contract, g.method, g.kind);
        let old = saved.get(&key).and_then(|v| v.as_u64());
        match old {
            None => {
                changed += 1;
                out.push_str(&format!("  NEW   {key}: {} gas\n", g.gas));
            }
            Some(old) => {
                let delta = g.gas as f64 - old as f64;
                let pct = if old == 0 { 0.0 } else { 100.0 * delta / old as f64 };
                if pct.abs() > tolerance_pct {
                    changed += 1;
                    let dir = if delta > 0.0 { "WORSE" } else { "BETTER" };
                    out.push_str(&format!("  {dir} {key}: {old} → {} gas ({pct:+.1}%)\n", g.gas));
                }
            }
        }
    }
    if changed == 0 {
        out.push_str("  ✓ no gas regressions\n");
    } else {
        out.push_str(&format!("  {changed} change(s) exceed tolerance\n"));
    }
    Ok(out)
}

fn hex_to_bytes(s: &str) -> Vec<u8> {
    let s = s.trim().trim_start_matches("0x");
    (0..s.len() / 2)
        .filter_map(|i| u8::from_str_radix(&s[2 * i..2 * i + 2], 16).ok())
        .collect()
}
