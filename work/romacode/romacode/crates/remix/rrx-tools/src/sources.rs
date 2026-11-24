//! Fetch VERIFIED source code of any deployed contract into the workspace.
//!
//! Providers (in order):
//!  1. Sourcify      — no API key needed, full standard-json
//!  2. Etherscan V2  — needs ETHERSCAN_API_KEY, any supported chain

use std::path::Path;

pub struct SourceFile {
    pub path: String,
    pub content: String,
}

fn sanitize(path: &str) -> String {
    let cleaned: Vec<&str> = path
        .split(['/', '\\'])
        .filter(|s| !s.is_empty() && *s != "." && *s != "..")
        .collect();
    let joined = cleaned.join("/");
    if joined.is_empty() {
        "unnamed.sol".into()
    } else {
        joined
    }
}

/// Sourcify v2 contract endpoint — public, keyless.
pub fn fetch_sourcify(chain: u64, address: &str) -> Result<Vec<SourceFile>, String> {
    let url = format!(
        "https://sourcify.dev/server/v2/contract/{chain}/{}?fields=sources",
        address.to_lowercase()
    );
    let resp = ureq::get(&url)
        .set("User-Agent", "rrx/0.3 (ethereum tooling)")
        .timeout(std::time::Duration::from_secs(30))
        .call()
        .map_err(|e| format!("sourcify: {e}"))?;
    let v: serde_json::Value = resp.into_json().map_err(|e| e.to_string())?;
    let sources = v
        .get("sources")
        .and_then(|s| s.as_object())
        .ok_or("sourcify: contract not found or not verified")?;
    let mut out = vec![];
    for (path, body) in sources {
        let content = body.get("content").and_then(|c| c.as_str()).unwrap_or("");
        if !content.is_empty() {
            out.push(SourceFile { path: sanitize(path), content: content.to_string() });
        }
    }
    if out.is_empty() {
        return Err("sourcify: matched but empty".into());
    }
    Ok(out)
}

/// Etherscan V2 unified API (chainid parameter covers 60+ networks).
pub fn fetch_etherscan(chain: u64, address: &str, api_key: &str) -> Result<Vec<SourceFile>, String> {
    let url = format!(
        "https://api.etherscan.io/v2/api?chainid={chain}&module=contract&action=getsourcecode&address={address}&apikey={api_key}"
    );
    let resp = ureq::get(&url)
        .timeout(std::time::Duration::from_secs(30))
        .call()
        .map_err(|e| format!("etherscan: {e}"))?;
    let v: serde_json::Value = resp.into_json().map_err(|e| e.to_string())?;
    let status = v["status"].as_str().unwrap_or("0");
    if status != "1" {
        return Err(format!(
            "etherscan: {}",
            v["result"].as_str().unwrap_or("API error")
        ));
    }
    let item = v["result"]
        .as_array()
        .and_then(|a| a.first())
        .ok_or("etherscan: empty result")?;
    let contract = item["ContractName"].as_str().unwrap_or("contract");
    let src = item["SourceCode"].as_str().unwrap_or("");
    if src.is_empty() {
        return Err("etherscan: contract is not verified".into());
    }
    let mut out = vec![];
    let trimmed = src.trim();
    if trimmed.starts_with("{{") {
        // standard-json payload wrapped in double braces
        let inner = &trimmed[1..trimmed.len() - 1];
        let sj: serde_json::Value = serde_json::from_str(inner).map_err(|e| e.to_string())?;
        if let Some(sources) = sj.get("sources").and_then(|s| s.as_object()) {
            for (path, body) in sources {
                let content = body.get("content").and_then(|c| c.as_str()).unwrap_or("");
                out.push(SourceFile { path: sanitize(path), content: content.to_string() });
            }
        }
    } else if trimmed.starts_with('{') {
        // multi-file single-brace JSON: {"sources": {...}} or {path: {content}}
        if let Ok(sj) = serde_json::from_str::<serde_json::Value>(trimmed) {
            let sources = sj.get("sources").unwrap_or(&sj);
            if let Some(map) = sources.as_object() {
                for (path, body) in map {
                    let content = body
                        .get("content")
                        .and_then(|c| c.as_str())
                        .unwrap_or_else(|| body.as_str().unwrap_or(""));
                    out.push(SourceFile { path: sanitize(path), content: content.to_string() });
                }
            }
        }
    } else {
        // single flat file
        out.push(SourceFile { path: sanitize(&format!("{contract}.sol")), content: src.to_string() });
    }
    if out.is_empty() {
        return Err("etherscan: could not parse source payload".into());
    }
    Ok(out)
}

/// fetch via Sourcify then Etherscan, write into `out_dir`, return a report
pub fn fetch_and_save(
    chain: u64,
    address: &str,
    etherscan_key: Option<&str>,
    out_dir: &Path,
) -> Result<String, String> {
    let (files, via) = match fetch_sourcify(chain, address) {
        Ok(f) => (f, "sourcify (full/partial match)"),
        Err(se) => {
            let key = etherscan_key
                .map(|k| k.to_string())
                .or_else(|| std::env::var("ETHERSCAN_API_KEY").ok())
                .ok_or_else(|| format!("sourcify failed ({se}) and no ETHERSCAN_API_KEY for the Etherscan fallback"))?;
            (fetch_etherscan(chain, address, &key)?, "etherscan v2")
        }
    };
    let mut written = vec![];
    for f in &files {
        let dest = out_dir.join(&f.path);
        if let Some(parent) = dest.parent() {
            std::fs::create_dir_all(parent).map_err(|e| e.to_string())?;
        }
        std::fs::write(&dest, &f.content).map_err(|e| e.to_string())?;
        written.push(f.path.clone());
    }
    let mut out = format!(
        "verified sources for {address} (chain {chain}) via {via}\n  saved to {}\n",
        out_dir.display()
    );
    for w in &written {
        out.push_str(&format!("    {w}\n"));
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn path_sanitizing() {
        assert_eq!(sanitize("/abs/../evil.sol"), "abs/evil.sol");
        assert_eq!(sanitize("a/b/c.sol"), "a/b/c.sol");
        assert_eq!(sanitize(""), "unnamed.sol");
        assert_eq!(sanitize("..\\..\\x.sol"), "x.sol");
        assert_eq!(sanitize("contracts/token/Token.sol"), "contracts/token/Token.sol");
    }
}
