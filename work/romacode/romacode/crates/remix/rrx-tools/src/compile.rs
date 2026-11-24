//! Solidity Compiler tool: compile a workspace with the real solc and emit artifacts.

use std::path::Path;

use rrx_solc::{build_input, extract_artifacts};
use serde_json::json;

pub struct CompileOutput {
    pub artifacts: Vec<(String, serde_json::Value)>,
    pub solc_version: String,
}

/// Compile all workspace sources (or `files` subset) via real solc --standard-json.
pub fn compile_workspace(
    root: &Path,
    version: Option<&str>,
    optimizer: bool,
    evm_version: &str,
) -> Result<CompileOutput, String> {
    let ws = crate::workspace::Workspace::at(root);
    let sources = crate::unittest::prepare_sources(&ws)?;
    let solc = match version {
        Some(v) => rrx_solc::use_version(v)?,
        None => rrx_solc::detect()?,
    };
    let input = build_input(&sources, optimizer, evm_version);
    let output = solc.compile(&input)?;
    let artifacts = extract_artifacts(&output)?;

    // persist artifacts like Remix does
    let dir = ws.artifacts_dir();
    std::fs::create_dir_all(&dir).map_err(|e| e.to_string())?;
    for (key, artifact) in &artifacts {
        let name = key.rsplit(':').next().unwrap_or("contract");
        let file = dir.join(format!("{name}.json"));
        std::fs::write(&file, serde_json::to_vec_pretty(artifact).map_err(|e| e.to_string())?)
            .map_err(|e| e.to_string())?;
    }
    Ok(CompileOutput { artifacts, solc_version: solc.version })
}

/// ABI of an artifact.
pub fn abi_of(artifact: &serde_json::Value) -> serde_json::Value {
    artifact.get("abi").cloned().unwrap_or(json!([]))
}

/// Creation bytecode of an artifact.
pub fn bytecode_of(artifact: &serde_json::Value) -> Vec<u8> {
    let hex = artifact["evm"]["bytecode"]["object"]
        .as_str()
        .unwrap_or("0x");
    rrx_util::from_hex(hex).unwrap_or_default()
}

/// Find an artifact by contract name (file:name or bare name).
pub fn find_artifact<'a>(artifacts: &'a [(String, serde_json::Value)], name: &str) -> Option<&'a serde_json::Value> {
    if let Some((_, a)) = artifacts.iter().find(|(k, _)| k == name) {
        return Some(a);
    }
    artifacts
        .iter()
        .find(|(k, _)| k.rsplit(':').next() == Some(name))
        .map(|(_, a)| a)
}
