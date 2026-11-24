//! Contract Verification: Etherscan V2 API (works for 60+ chains via `chainid`).

use serde_json::json;

pub fn verify(
    api_key: &str,
    chain_id: u64,
    contract_address: &str,
    contract_name: &str, // file:name or name
    compiler_version: &str, // e.g. v0.8.24+commit.e11b9ed9
    standard_json_input: serde_json::Value,
    optimization_used: bool,
    runs: u32,
) -> Result<(String, String), String> {
    // 1) submit
    let source_code = serde_json::to_string(&standard_json_input).map_err(|e| e.to_string())?;
    let chain_id_s = chain_id.to_string();
    let runs_s = runs.to_string();
    let form: Vec<(&str, &str)> = vec![
        ("apikey", api_key),
        ("chainid", &chain_id_s),
        ("module", "contract"),
        ("action", "verifysourcecode"),
        ("codeformat", "solidity-standard-json-input"),
        ("contractaddress", contract_address),
        ("contractname", contract_name),
        ("compilerversion", compiler_version),
        ("optimizationUsed", if optimization_used { "1" } else { "0" }),
        ("runs", &runs_s),
        ("sourceCode", &source_code),
    ];
    let resp = ureq::post("https://api.etherscan.io/v2/api")
        .timeout(std::time::Duration::from_secs(120))
        .send_form(&form)
        .map_err(|e| format!("etherscan request failed: {e}"))?;
    let v: serde_json::Value = resp.into_json().map_err(|e| e.to_string())?;
    let status = v["status"].as_str().unwrap_or("0").to_string();
    let message = v["result"]
        .as_str()
        .or_else(|| v["message"].as_str())
        .unwrap_or("?")
        .to_string();
    if status != "1" {
        return Err(format!("submit failed: {message}"));
    }

    // 2) poll for the result
    let guid = message.clone();
    for _ in 0..30 {
        std::thread::sleep(std::time::Duration::from_secs(5));
        let check = ureq::get("https://api.etherscan.io/v2/api")
            .query("apikey", api_key)
            .query("chainid", &chain_id.to_string())
            .query("module", "contract")
            .query("action", "checkverifystatus")
            .query("guid", &guid)
            .timeout(std::time::Duration::from_secs(60))
            .call()
            .map_err(|e| e.to_string())?;
        let cv: serde_json::Value = check.into_json().map_err(|e| e.to_string())?;
        let s = cv["result"].as_str().unwrap_or("");
        if s == "Pass - Verified" {
            return Ok(("verified".into(), guid));
        }
        if s.starts_with("Fail") || s.starts_with("Error") || s.contains("NOTOK") {
            return Err(format!("verification failed: {s}"));
        }
    }
    Ok(("pending".into(), guid))
}

pub fn json_source_input(sources: &std::collections::BTreeMap<String, String>, optimizer: bool, evm_version: &str) -> serde_json::Value {
    json!({
        "language": "Solidity",
        "sources": sources.iter().map(|(k, v)| (k.clone(), json!({"content": v}))).collect::<serde_json::Map<_,_>>(),
        "settings": {
            "optimizer": { "enabled": optimizer, "runs": 200 },
            "evmVersion": evm_version,
        }
    })
}
