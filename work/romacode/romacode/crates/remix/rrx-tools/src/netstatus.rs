//! Live mainnet status tools: connectivity matrix, chain dashboard,
//! transaction / block / account lookups and call-tree traces.
//!
//! These are the "har link main chain se connected" reports — everything
//! is fetched live from real Ethereum mainnet JSON-RPC endpoints.

use crate::deploy;
use rrx_rpc::endpoints::{probe_all, MAINNET_ENDPOINTS, MAINNET_CHAIN_ID};
use rrx_rpc::Provider;
use rrx_util::{h256_from_hex, h160_from_hex, u256_from_str, wei_to_strings, H256, U256};
use serde_json::Value;

fn gwei(v: &U256) -> String {
    let whole = *v / U256::from(1_000_000_000u64);
    let rem = *v % U256::from(1_000_000_000u64);
    let frac = format!("{:09}", rem.as_u64().max(0)).trim_end_matches('0').to_string();
    if frac.is_empty() { format!("{} gwei", whole) } else { format!("{}.{} gwei", whole, frac) }
}

fn gwei_u(v: &U256) -> String {
    gwei(v)
}

fn qty(v: &Value, key: &str) -> Option<U256> {
    v.get(key).and_then(|x| x.as_str()).and_then(|s| u256_from_str(s).ok())
}

fn qty_u64(v: &Value, key: &str) -> Option<u64> {
    qty(v, key).map(|x| x.as_u64())
}

fn fmt_u64(n: u64) -> String {
    let s = n.to_string();
    let mut out = String::new();
    let bytes = s.as_bytes();
    for (i, c) in bytes.iter().enumerate() {
        if i > 0 && (bytes.len() - i) % 3 == 0 {
            out.push(',');
        }
        out.push(*c as char);
    }
    out
}

/// Days-from-civil algorithm (Hinnant): unix seconds → "YYYY-MM-DD HH:MM:SS UTC".
pub fn utc_string(epoch: u64) -> String {
    let days = epoch / 86_400;
    let secs = epoch % 86_400;
    let z = days as i64 + 719_468;
    let era = z / 146_097;
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = if m <= 2 { y + 1 } else { y };
    format!(
        "{y:04}-{m:02}-{d:02} {:02}:{:02}:{:02} UTC",
        secs / 3600,
        (secs % 3600) / 60,
        secs % 60
    )
}

/// Connectivity matrix over every built-in mainnet endpoint.
pub fn net_table() -> String {
    let statuses = probe_all();
    let mut out = String::from("mainnet endpoints (probed live):\n");
    let mut first_ok = None;
    for st in &statuses {
        let mark = if st.ok { "up  " } else { "DOWN" };
        let info = if st.ok {
            first_ok = first_ok.or(Some(st.url.clone()));
            format!(
                "chain {}  block {}  {} ms  {}",
                st.chain_id.map(|c| c.to_string()).unwrap_or("?".into()),
                st.block.map(fmt_u64).unwrap_or_else(|| "?".into()),
                st.latency_ms,
                st.client.clone().unwrap_or_default()
            )
        } else {
            st.error.clone().unwrap_or_else(|| "unreachable".into())
        };
        out.push_str(&format!("  [{mark}] {:<40} {}\n", st.url, info));
    }
    match first_ok {
        Some(u) => out.push_str(&format!("\nselected endpoint: {u}\nfailover order: {}\n", MAINNET_ENDPOINTS.join(" → "))),
        None => out.push_str("\nno endpoint healthy — check internet connection\n"),
    }
    out
}

/// One-screen live chain dashboard.
pub fn chain_dashboard(p: &Provider) -> Result<String, String> {
    let chain_id = p.chain_id()?;
    let client = p.client_version().unwrap_or_else(|_| "unknown".into());
    let latest = p.block_number()?;
    let block = p.block_by_tag("latest", false)?.ok_or("latest block not found")?;
    let ts = qty_u64(&block, "timestamp").unwrap_or(0);
    let age = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs().saturating_sub(ts))
        .unwrap_or(0);
    let base_fee = qty(&block, "baseFeePerGas").unwrap_or_default();
    let gas_price = p.gas_price()?;
    let (base_hist, rewards) = p.fee_history(20)?;
    let base_next = base_hist.last().cloned().unwrap_or(base_fee);
    let pick = |i: usize| -> U256 {
        rewards
            .get(rewards.len() / 2)
            .and_then(|row| row.get(i).copied())
            .unwrap_or_default()
    };
    let mut out = String::new();
    let name = match chain_id {
        1 => "Ethereum Mainnet",
        11155111 => "Sepolia",
        84532 => "Base Sepolia",
        8453 => "Base",
        137 => "Polygon",
        42161 => "Arbitrum One",
        10 => "Optimism",
        _ => "unknown network",
    };
    out.push_str(&format!("network        chain {chain_id} — {name}\n"));
    out.push_str(&format!("endpoint       {}\n", p.url));
    out.push_str(&format!("client         {client}\n"));
    out.push_str(&format!("latest block   {}  (age {age}s)\n", fmt_u64(latest)));
    out.push_str(&format!("base fee       {}   next block ~{}\n", gwei(&base_fee), gwei_u(&base_next)));
    out.push_str(&format!("gas price      {}\n", gwei(&gas_price)));
    out.push_str(&format!(
        "priority fees  slow {} | standard {} | fast {}\n",
        gwei_u(&pick(0)),
        gwei_u(&pick(1)),
        gwei_u(&pick(2))
    ));
    let (_g, eth) = wei_to_strings(&(U256::from(21_000u64) * gas_price));
    out.push_str(&format!("transfer cost  21000 gas ≈ {eth} ETH\n"));
    let _ = MAINNET_CHAIN_ID;
    Ok(out)
}

fn tx_type_name(v: &Value) -> String {
    match v.get("type").and_then(|t| t.as_str()) {
        Some("0x0") => "legacy".into(),
        Some("0x1") => "EIP-2930".into(),
        Some("0x2") => "EIP-1559".into(),
        Some("0x3") => "EIP-4844 blob".into(),
        Some(t) => format!("type {t}"),
        None => "legacy".into(),
    }
}

/// Live transaction + receipt report.
pub fn tx_report(p: &Provider, hash_str: &str) -> Result<String, String> {
    let hash: H256 = h256_from_hex(hash_str)?;
    let tx = p.get_transaction(&hash)?.ok_or("transaction not found (pending or unknown)")?;
    let receipt = p.transaction_receipt(&hash)?;
    let mut out = String::from(&format!("tx {}\n", tx.get("hash").and_then(|h| h.as_str()).unwrap_or(hash_str)));
    out.push_str(&format!("  type      {}\n", tx_type_name(&tx)));
    out.push_str(&format!("  from      {}\n", tx.get("from").and_then(|x| x.as_str()).unwrap_or("?")));
    let to = tx.get("to").and_then(|x| x.as_str()).unwrap_or("");
    out.push_str(&format!(
        "  to        {}\n",
        if to.is_empty() { "(contract creation)".to_string() } else { to.to_string() }
    ));
    if let Some(val) = qty(&tx, "value") {
        let (_g, eth) = wei_to_strings(&val);
        out.push_str(&format!("  value     {eth} ETH\n"));
    }
    if let Some(r) = &receipt {
        let status = r.get("status").and_then(|s| s.as_str()).unwrap_or("0x0");
        let block = qty_u64(r, "blockNumber").unwrap_or(0);
        out.push_str(&format!(
            "  status    {} (block {})\n",
            if status == "0x1" { "SUCCESS" } else { "REVERTED" },
            fmt_u64(block)
        ));
        let used = qty(r, "gasUsed").unwrap_or_default();
        let effective = qty(r, "effectiveGasPrice").unwrap_or_default();
        let fee = used * effective;
        let (_g, fee_eth) = wei_to_strings(&fee);
        out.push_str(&format!(
            "  gas       {} used @ {}  → fee {fee_eth} ETH\n",
            fmt_u64(used.as_u64()),
            gwei(&effective)
        ));
        let logs = r.get("logs").and_then(|l| l.as_array()).map(|a| a.len()).unwrap_or(0);
        out.push_str(&format!("  logs      {logs}\n"));
    } else {
        out.push_str("  status    pending (no receipt yet)\n");
        if let Some(gp) = qty(&tx, "gasPrice") {
            out.push_str(&format!("  gasPrice  {}\n", gwei(&gp)));
        }
        if let Some(mf) = qty(&tx, "maxFeePerGas") {
            out.push_str(&format!("  maxFee    {}  tip {}\n", gwei(&mf), gwei(&qty(&tx, "maxPriorityFeePerGas").unwrap_or_default())));
        }
    }
    if let Some(input) = tx.get("input").and_then(|i| i.as_str()) {
        let len = (input.len().saturating_sub(2)) / 2;
        if len >= 4 {
            out.push_str(&format!("  input     {len} bytes, selector {}\n", &input[..10]));
        } else if len > 0 {
            out.push_str(&format!("  input     {len} bytes\n"));
        }
    }
    if let Some(n) = qty_u64(&tx, "nonce") {
        out.push_str(&format!("  nonce     {n}\n"));
    }
    Ok(out)
}

/// Live block summary ("latest", "finalized", or a decimal number).
pub fn block_report(p: &Provider, tag: &str) -> Result<String, String> {
    let b = p.block_by_tag(tag, false)?.ok_or_else(|| format!("block '{tag}' not found"))?;
    let number = qty_u64(&b, "number").unwrap_or(0);
    let ts = qty_u64(&b, "timestamp").unwrap_or(0);
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    let gas_used = qty_u64(&b, "gasUsed").unwrap_or(0);
    let gas_limit = qty_u64(&b, "gasLimit").unwrap_or(1);
    let base = qty(&b, "baseFeePerGas").unwrap_or_default();
    let txs = b.get("transactions").and_then(|t| t.as_array()).map(|a| a.len()).unwrap_or(0);
    let mut out = String::from(&format!(
        "block {} ({})\n",
        fmt_u64(number),
        if tag.chars().all(|c| c.is_ascii_digit()) { "by number" } else { tag }
    ));
    out.push_str(&format!("  hash        {}\n", b.get("hash").and_then(|h| h.as_str()).unwrap_or("?")));
    out.push_str(&format!(
        "  time        {}  (age {}s)\n",
        utc_string(ts),
        now.saturating_sub(ts)
    ));
    out.push_str(&format!("  transactions {}\n", txs));
    let pct = if gas_limit > 0 { gas_used * 100 / gas_limit } else { 0 };
    out.push_str(&format!(
        "  gas         {} / {} ({}%)\n",
        fmt_u64(gas_used),
        fmt_u64(gas_limit),
        pct
    ));
    out.push_str(&format!("  base fee    {}\n", gwei(&base)));
    out.push_str(&format!("  proposed by {}\n", b.get("miner").and_then(|m| m.as_str()).unwrap_or("?")));
    Ok(out)
}

/// Live account report: balance, nonce, code.
pub fn account_report(p: &Provider, addr_str: &str) -> Result<String, String> {
    let addr = h160_from_hex(addr_str)?;
    let block = "latest";
    let balance = p.balance(&addr, block)?;
    let nonce = p.nonce(&addr, block)?;
    let code = p.code(&addr, block)?;
    let (_g, eth) = wei_to_strings(&balance);
    let mut out = String::from(&format!("account 0x{:x}\n", addr));
    out.push_str(&format!("  balance     {eth} ETH\n"));
    out.push_str(&format!("  nonce       {nonce}\n"));
    if code.is_empty() {
        out.push_str("  code        — (EOA wallet)\n");
    } else {
        out.push_str(&format!("  code        {} bytes (smart contract)\n", code.len()));
    }
    Ok(out)
}

/// Live call-tree trace of a confirmed transaction (callTracer).
pub fn trace_report(p: &Provider, hash_str: &str) -> Result<String, String> {
    let hash: H256 = h256_from_hex(hash_str)?;
    let tree = match p.trace_call_tree(&hash) {
        Ok(t) => t,
        Err(e) => {
            return Err(format!(
                "this endpoint does not support debug_traceTransaction ({e}) — try another endpoint with --rpc (archive/trace nodes required)"
            ))
        }
    };
    let mut out = String::from(&format!("trace {}\n", hash_str));
    format_call(&tree, 0, &mut out, true);
    Ok(out)
}

fn format_call(node: &Value, depth: usize, out: &mut String, root: bool) {
    if depth > 6 {
        return;
    }
    let typ = node.get("type").and_then(|t| t.as_str()).unwrap_or("CALL").to_uppercase();
    let from = node.get("from").and_then(|x| x.as_str()).unwrap_or("?");
    let to = node.get("to").and_then(|x| x.as_str()).unwrap_or("(create)");
    let gas_used = node.get("gasUsed").and_then(|g| g.as_str()).and_then(|s| u256_from_str(s).ok()).unwrap_or_default();
    let value = node.get("value").and_then(|g| g.as_str()).and_then(|s| u256_from_str(s).ok()).unwrap_or_default();
    let err = node.get("error").and_then(|e| e.as_str());
    let (_g, eth) = wei_to_strings(&value);
    let pad = "  ".repeat(depth);
    if root {
        out.push_str(&format!(
            "{pad}{}  {} → {}\n{pad}  gasUsed {}  value {eth} ETH  {}\n",
            typ, from, to, fmt_u64(gas_used.as_u64()), err.unwrap_or("status success")
        ));
    } else {
        out.push_str(&format!(
            "{pad}├─ {} {}  gas {}  value {eth}{}\n",
            typ,
            to,
            fmt_u64(gas_used.as_u64()),
            if let Some(e) = err { format!("  ERROR: {e}") } else { String::new() }
        ));
    }
    if let Some(calls) = node.get("calls").and_then(|c| c.as_array()) {
        for (i, c) in calls.iter().take(10).enumerate() {
            let _ = i;
            format_call(c, depth + 1, out, false);
        }
        if calls.len() > 10 {
            out.push_str(&format!("{}   … {} more calls\n", "  ".repeat(depth + 1), calls.len() - 10));
        }
    }
}

/// Read-only eth_call against a real network with ABI decode.
pub fn remote_call(
    p: &Provider,
    to_str: &str,
    function: &str,
    types: &[&str],
    args: &[&str],
    out_types: &[&str],
    block: &str,
) -> Result<String, String> {
    let to = h160_from_hex(to_str)?;
    let data = deploy::encode_call(function, types, args)?;
    let raw = p.call(None, &to, &data, block)?;
    if raw.is_empty() {
        return Ok("(empty return)".into());
    }
    deploy::format_output(out_types, &raw)
}
