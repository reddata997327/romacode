//! Event logs: live eth_getLogs queries with known-signature annotation.

use rrx_rpc::Provider;
use rrx_util::{keccak256, to_hex, u256_from_str, H160};

/// known event topic0 signatures (keccak-computed, so always right)
pub fn known_topic0() -> std::collections::BTreeMap<[u8; 32], &'static str> {
    let mut m = std::collections::BTreeMap::new();
    let put = |m: &mut std::collections::BTreeMap<[u8; 32], &'static str>, sig: &'static str, name: &'static str| {
        m.insert(keccak256(sig.as_bytes()), name);
    };
    put(&mut m, "Transfer(address,address,uint256)", "Transfer");
    put(&mut m, "Approval(address,address,uint256)", "Approval");
    put(&mut m, "ApprovalForAll(address,address,bool)", "ApprovalForAll");
    put(&mut m, "Deposit(address,uint256)", "Deposit");
    put(&mut m, "Withdrawal(address,uint256)", "Withdrawal");
    put(&mut m, "Swap(address,uint256,uint256,uint256,uint256,address)", "Swap");
    put(&mut m, "Mint(address,uint256)", "Mint");
    put(&mut m, "Burn(address,uint256)", "Burn");
    m
}

fn topic_name(map: &std::collections::BTreeMap<[u8; 32], &'static str>, t0: &str) -> String {
    let clean = t0.trim_start_matches("0x");
    if clean.len() == 64 {
        if let Ok(bytes) = rrx_util::from_hex(clean) {
            if bytes.len() == 32 {
                let key: [u8; 32] = bytes.try_into().unwrap();
                if let Some(name) = map.get(&key) {
                    return name.to_string();
                }
            }
        }
    }
    "unknown".into()
}

fn fmt_u64(n: u64) -> String {
    let s = n.to_string();
    let b = s.as_bytes();
    let mut out = String::new();
    for (i, c) in b.iter().enumerate() {
        if i > 0 && (b.len() - i) % 3 == 0 {
            out.push(',');
        }
        out.push(*c as char);
    }
    out
}

/// annotate + decode a log's topics/data where we know the signature
fn describe_log(map: &std::collections::BTreeMap<[u8; 32], &'static str>, log: &serde_json::Value) -> String {
    let topics: Vec<&str> = log
        .get("topics")
        .and_then(|t| t.as_array())
        .map(|a| a.iter().filter_map(|x| x.as_str()).collect())
        .unwrap_or_default();
    if topics.is_empty() {
        return String::new();
    }
    let name = topic_name(map, topics[0]);
    let data_hex = log.get("data").and_then(|d| d.as_str()).unwrap_or("0x");
    let data = rrx_util::from_hex(data_hex).unwrap_or_default();
    match name.as_str() {
        "Transfer" if topics.len() == 3 && data.len() >= 32 => {
            let from = &topics[1][26..];
            let to = &topics[2][26..];
            let amount = data
                .chunks(32)
                .next()
                .and_then(|c| if c.len() == 32 { u256_from_str(&format!("0x{}", to_hex(c))).ok() } else { None })
                .unwrap_or_default();
            format!("      {name}(from 0x{from} → to 0x{to}, value {amount})")
        }
        "Approval" if topics.len() == 3 && data.len() >= 32 => {
            let owner = &topics[1][26..];
            let spender = &topics[2][26..];
            let amount = data
                .chunks(32)
                .next()
                .and_then(|c| if c.len() == 32 { u256_from_str(&format!("0x{}", to_hex(c))).ok() } else { None })
                .unwrap_or_default();
            format!("      {name}(owner 0x{owner} → spender 0x{spender}, amount {amount})")
        }
        "Deposit" | "Withdrawal" | "Mint" | "Burn" if topics.len() == 2 && data.len() >= 32 => {
            let who = &topics[1][26..];
            let amount = data
                .chunks(32)
                .next()
                .and_then(|c| if c.len() == 32 { u256_from_str(&format!("0x{}", to_hex(c))).ok() } else { None })
                .unwrap_or_default();
            format!("      {name}(who 0x{who}, amount {amount})")
        }
        "unknown" => String::new(),
        _ => format!("      {name} ({} topics, {} bytes data)", topics.len(), data.len()),
    }
}

pub fn query(
    p: &Provider,
    address: Option<&H160>,
    topics: &[String],
    from_block: &str,
    to_block: &str,
    limit: usize,
) -> Result<String, String> {
    let logs = p.get_logs(address, topics, from_block, to_block)?;
    let map = known_topic0();
    let mut out = String::from(&format!(
        "logs  address {}  topics [{}]  range {} → {}\n",
        address.map(|a| format!("0x{a:x}")).unwrap_or_else(|| "(any)".into()),
        topics.join(", "),
        from_block,
        to_block
    ));
    out.push_str(&format!("  {} log(s) returned, showing up to {limit}\n", logs.len()));
    for log in logs.iter().take(limit) {
        let block = log
            .get("blockNumber")
            .and_then(|b| b.as_str())
            .and_then(|s| u256_from_str(s).ok())
            .map(|b| fmt_u64(b.as_u64()))
            .unwrap_or_else(|| "?".into());
        let txh = log.get("transactionHash").and_then(|t| t.as_str()).unwrap_or("?");
        let addr = log.get("address").and_then(|a| a.as_str()).unwrap_or("?");
        let idx = log.get("logIndex").and_then(|i| i.as_str()).unwrap_or("0x0");
        let topics_list: Vec<&str> = log
            .get("topics")
            .and_then(|t| t.as_array())
            .map(|a| a.iter().filter_map(|x| x.as_str()).collect())
            .unwrap_or_default();
        out.push_str(&format!("  block {block}  log#{idx}  0x{}\n", addr.trim_start_matches("0x")));
        if let Some(first) = topics_list.first() {
            out.push_str(&format!("    topic0 {} ({})\n", first, topic_name(&map, first)));
        }
        for t in topics_list.iter().skip(1) {
            out.push_str(&format!("    topic {}\n", t));
        }
        let data = log.get("data").and_then(|d| d.as_str()).unwrap_or("0x");
        if data != "0x" {
            let n = (data.len() - 2) / 2;
            out.push_str(&format!("    data   {data}  ({n} bytes)\n"));
        }
        let desc = describe_log(&map, log);
        if !desc.is_empty() {
            out.push_str(&format!("{desc}\n"));
        }
        out.push_str(&format!("    tx     {txh}\n"));
    }
    if logs.len() > limit {
        out.push_str(&format!("  … {} more\n", logs.len() - limit));
    }
    Ok(out)
}

/// helper to build a topic0 filter string from a signature
pub fn topic0_of(sig: &str) -> String {
    format!("0x{}", to_hex(&keccak256(sig.as_bytes())))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn transfer_topic0_is_the_canonical_one() {
        assert_eq!(
            topic0_of("Transfer(address,address,uint256)"),
            "0xddf252ad1be2c89b69c2b068fc378daa952ba7f163c4a11628f55a4df523b3ef"
        );
        assert_eq!(
            topic0_of("Approval(address,address,uint256)"),
            "0x8c5be1e5ebec7d5bd14f71427d1e84f3dd0314c0f7b2291e5b200ac8c7c3b925"
        );
    }
}
