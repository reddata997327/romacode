//! rrx-solc: real Solidity compiler integration — detects/installs solc binaries
//! (binaries.soliditylang.org, the same endpoint Remix uses) and compiles via
//! `solc --standard-json` exactly like remix-solidity does.

use std::io::Write;
use std::path::PathBuf;
use std::process::{Command, Stdio};

use serde_json::{json, Value};

pub struct Solc {
    pub path: PathBuf,
    pub version: String,
}

fn solc_dir() -> PathBuf {
    if let Ok(d) = std::env::var("RRX_SOLC_DIR") {
        return PathBuf::from(d);
    }
    if let Ok(base) = std::env::var("RRX_HOME") {
        return PathBuf::from(base).join(".solc");
    }
    // fallback: the toolchain-managed dir next to this crate (set at build time)
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../.solc")
}

fn fetch_json(url: &str) -> Result<Value, String> {
    let resp = ureq::get(url)
        .timeout(std::time::Duration::from_secs(60))
        .call()
        .map_err(|e| format!("download failed: {e}"))?;
    resp.into_json::<Value>().map_err(|e| e.to_string())
}

pub fn list_available() -> Result<Vec<String>, String> {
    let list = fetch_json("https://binaries.soliditylang.org/linux-amd64/list.json")?;
    let mut out = vec![];
    if let Value::Object(releases) = list.get("releases").cloned().unwrap_or_default() {
        for k in releases.keys() {
            out.push(k.clone());
        }
    }
    out.sort();
    out.reverse();
    Ok(out)
}

pub fn latest_version() -> Result<String, String> {
    let list = fetch_json("https://binaries.soliditylang.org/linux-amd64/list.json")?;
    Ok(list
        .get("latestRelease")
        .and_then(|v| v.as_str())
        .unwrap_or("0.8.24")
        .to_string())
}

/// Download and install a solc version; returns the binary path.
pub fn install(version: &str) -> Result<PathBuf, String> {
    let dir = solc_dir().join(version);
    std::fs::create_dir_all(&dir).map_err(|e| e.to_string())?;
    let bin = dir.join(format!("solc-{version}"));
    if bin.exists() {
        return Ok(bin);
    }
    let list = fetch_json("https://binaries.soliditylang.org/linux-amd64/list.json")?;
    let rel = list["releases"][version]
        .as_str()
        .ok_or_else(|| format!("version {version} not found on binaries.soliditylang.org"))?;
    let url = format!("https://binaries.soliditylang.org/linux-amd64/{rel}");
    let resp = ureq::get(&url)
        .timeout(std::time::Duration::from_secs(300))
        .call()
        .map_err(|e| format!("download failed: {e}"))?;
    let mut bytes = Vec::new();
    use std::io::Read as _;
    resp.into_reader().take(100_000_000).read_to_end(&mut bytes).map_err(|e| e.to_string())?;
    std::fs::write(&bin, &bytes).map_err(|e| e.to_string())?;
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&bin, std::fs::Permissions::from_mode(0o755))
            .map_err(|e| e.to_string())?;
    }
    Ok(bin)
}

pub fn installed_versions() -> Vec<String> {
    let dir = solc_dir();
    let mut out = vec![];
    if let Ok(entries) = std::fs::read_dir(dir) {
        for e in entries.flatten() {
            if let Ok(rd) = std::fs::read_dir(e.path()) {
                for f in rd.flatten() {
                    if let Some(name) = f.file_name().to_str() {
                        if name.starts_with("solc-") {
                            out.push(name.trim_start_matches("solc-").to_string());
                        }
                    }
                }
            }
        }
    }
    out.sort();
    out.reverse();
    out
}

/// Detect a usable solc: PATH first, then the newest installed managed version.
pub fn detect() -> Result<Solc, String> {
    if let Ok(out) = Command::new("solc").arg("--version").output() {
        if out.status.success() {
            let s = String::from_utf8_lossy(&out.stdout);
            let version = s
                .split_whitespace()
                .find(|w| w.chars().next().map(|c| c.is_ascii_digit()).unwrap_or(false))
                .unwrap_or("unknown")
                .trim_end_matches('+')
                .to_string();
            return Ok(Solc { path: PathBuf::from("solc"), version });
        }
    }
    if let Some(v) = installed_versions().first().cloned() {
        let path = solc_dir().join(&v).join(format!("solc-{v}"));
        if path.exists() {
            return Ok(Solc { path, version: v });
        }
    }
    Err("no solc found: run `rrx solc install <version>`".into())
}

/// Pin a specific version (installing if needed).
pub fn use_version(version: &str) -> Result<Solc, String> {
    let path = install(version)?;
    Ok(Solc { path, version: version.to_string() })
}

impl Solc {
    /// Run solc --standard-json with the given input (real compiler process).
    pub fn compile(&self, input: &Value) -> Result<Value, String> {
        let mut child = Command::new(&self.path)
            .arg("--standard-json")
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .map_err(|e| format!("failed to start solc: {e}"))?;
        child
            .stdin
            .as_mut()
            .unwrap()
            .write_all(input.to_string().as_bytes())
            .map_err(|e| e.to_string())?;
        let out = child.wait_with_output().map_err(|e| e.to_string())?;
        if !out.status.success() && out.stdout.is_empty() {
            return Err(format!(
                "solc exited with {}: {}",
                out.status,
                String::from_utf8_lossy(&out.stderr)
            ));
        }
        serde_json::from_slice(&out.stdout).map_err(|e| format!("invalid solc output: {e}"))
    }

    pub fn version_output(&self) -> String {
        Command::new(&self.path)
            .arg("--version")
            .output()
            .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string())
            .unwrap_or_default()
    }
}

/// Build a standard-json input (like Remix's compiler settings).
pub fn build_input(
    sources: &std::collections::BTreeMap<String, String>,
    optimizer: bool,
    evm_version: &str,
) -> Value {
    let mut language_sources = serde_json::Map::new();
    for (name, content) in sources {
        language_sources.insert(name.clone(), json!({ "content": content }));
    }
    json!({
        "language": "Solidity",
        "sources": language_sources,
        "settings": {
            "optimizer": { "enabled": optimizer, "runs": 200 },
            "evmVersion": evm_version,
            "outputSelection": {
                "*": {
                    "*": [
                        "abi", "evm.bytecode.object", "evm.bytecode.linkReferences",
                        "evm.deployedBytecode.object", "evm.deployedBytecode.sourceMap",
                        "evm.methodIdentifiers", "metadata"
                    ],
                    "": [ "ast" ]
                }
            }
        }
    })
}

/// Extract contract artifacts { name -> artifact } from standard-json output.
pub fn extract_artifacts(output: &Value) -> Result<Vec<(String, Value)>, String> {
    let mut out = vec![];
    if output.get("contracts").is_none() {
        // surface compiler errors before giving up
        if let Some(errs) = output.get("errors").and_then(|e| e.as_array()) {
            let msgs: Vec<String> = errs
                .iter()
                .map(|e| e.get("formattedMessage").or_else(|| e.get("message")).and_then(|m| m.as_str()).unwrap_or("?").to_string())
                .collect();
            return Err(if msgs.is_empty() { "no contracts in solc output".to_string() } else { msgs.join("\n") });
        }
        return Err("no contracts in solc output".into());
    }
    let contracts = output
        .get("contracts")
        .and_then(|c| c.as_object())
        .ok_or("no contracts in solc output")?;
    for (file, per_file) in contracts {
        if let Some(per_file) = per_file.as_object() {
            for (name, artifact) in per_file {
                let mut a = artifact.clone();
                a["file"] = Value::String(file.clone());
                a["name"] = Value::String(name.clone());
                out.push((format!("{file}:{name}"), a));
            }
        }
    }
    // surface compiler errors
    if let Some(errs) = output.get("errors").and_then(|e| e.as_array()) {
        let hard: Vec<&Value> = errs
            .iter()
            .filter(|e| e.get("severity").and_then(|s| s.as_str()) == Some("error"))
            .collect();
        if !hard.is_empty() && out.is_empty() {
            let msgs: Vec<String> = hard
                .iter()
                .map(|e| e.get("formattedMessage").or_else(|| e.get("message")).and_then(|m| m.as_str()).unwrap_or("?").to_string())
                .collect();
            return Err(msgs.join("\n"));
        }
    }
    Ok(out)
}
