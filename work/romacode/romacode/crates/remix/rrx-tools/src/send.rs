//! Send: real signed transactions to real networks.
//!
//! - `transfer`  : plain ETH transfer (EIP-1559)
//! - `write`     : state-changing contract call (signed, real funds at risk)
//!
//! Fee caps default to LIVE fee-history-derived values when not given.
//! Mainnet (chain id 1) requires explicit confirmation from the CLI layer.

use rrx_chain::tx::{sign_transaction, TxType, UnsignedTx};
use rrx_crypto::KeyPair;
use rrx_rpc::Provider;
use rrx_util::{u256_from_str, wei_to_strings, H160, H256, U256};

pub struct SendOutcome {
    pub tx_hash: H256,
    pub to: H160,
    pub value: U256,
    pub gas_used: u64,
    pub effective_gas_price: U256,
    pub block_number: u64,
    pub status: bool,
    pub logs: usize,
}

pub struct SendResult {
    pub outcome: Option<SendOutcome>,
    pub tx_hash: H256,
    pub timed_out: bool,
}

/// live-fee-aware EIP-1559 caps: (max_fee_gwei, max_priority_gwei)
pub fn auto_fees(p: &Provider) -> (u64, u64) {
    if let Ok((base_hist, rewards)) = p.fee_history(10) {
        let g = |v: &U256| (v / U256::from(1_000_000_000u64)).as_u64();
        if let Some(base) = base_hist.last() {
            let base = g(base);
            let tip = rewards
                .last()
                .and_then(|r| r.get(1).copied())
                .map(|t| g(&t).max(1))
                .unwrap_or(2);
            // headroom ×2 over the latest base fee so it stays valid across a few blocks
            return (base * 2 + tip, tip);
        }
    }
    (20, 2)
}

fn wait_receipt(p: &Provider, hash: &H256) -> Result<SendResult, String> {
    for _ in 0..90 {
        if let Some(r) = p.transaction_receipt(hash)? {
            let status = r.get("status").and_then(|s| s.as_str()).unwrap_or("0x0") == "0x1";
            let gas_used = r
                .get("gasUsed")
                .and_then(|g| g.as_str())
                .and_then(|s| u256_from_str(s).ok())
                .map(|g| g.as_u64())
                .unwrap_or(0);
            let block = r
                .get("blockNumber")
                .and_then(|g| g.as_str())
                .and_then(|s| u256_from_str(s).ok())
                .map(|g| g.as_u64())
                .unwrap_or(0);
            let egp = r
                .get("effectiveGasPrice")
                .and_then(|g| g.as_str())
                .and_then(|s| u256_from_str(s).ok())
                .unwrap_or_default();
            let logs = r.get("logs").and_then(|l| l.as_array()).map(|a| a.len()).unwrap_or(0);
            return Ok(SendResult {
                tx_hash: *hash,
                timed_out: false,
                outcome: Some(SendOutcome {
                    tx_hash: *hash,
                    to: H160::zero(),
                    value: U256::zero(),
                    gas_used,
                    effective_gas_price: egp,
                    block_number: block,
                    status,
                    logs,
                }),
            });
        }
        std::thread::sleep(std::time::Duration::from_secs(2));
    }
    Ok(SendResult { outcome: None, tx_hash: *hash, timed_out: true })
}

fn sign_and_send(
    p: &Provider,
    key: &KeyPair,
    to: H160,
    value: U256,
    data: Vec<u8>,
    gas_limit: u64,
    max_fee_gwei: u64,
    max_priority_gwei: u64,
) -> Result<SendResult, String> {
    let sender = key.address()?;
    let chain_id = p.chain_id()?;
    let nonce = p.nonce(&sender, "pending")?;
    let tx = sign_transaction(
        &UnsignedTx {
                max_fee_per_blob_gas: Default::default(),
                blob_versioned_hashes: vec![],
            tx_type: TxType::Eip1559,
            chain_id,
            nonce,
            gas_price: U256::zero(),
            max_fee: U256::from(max_fee_gwei) * U256::from(1_000_000_000u64),
            max_priority_fee: U256::from(max_priority_gwei) * U256::from(1_000_000_000u64),
            gas_limit,
            to: Some(to),
            value,
            data,
            access_list: vec![],
        },
        key,
    )?;
    let hash = tx.hash();
    p.send_raw_transaction(&tx.rlp_encode())?;
    wait_receipt(p, &hash)
}

/// Plain ETH transfer on a real network.
pub fn transfer(
    p: &Provider,
    key: &KeyPair,
    to: H160,
    amount_eth: &str,
    max_fee_gwei: Option<u64>,
    max_priority_gwei: Option<u64>,
) -> Result<(SendResult, String), String> {
    let value = parse_eth(amount_eth)?;
    let (sender_bal, _n) = (p.balance(&key.address()?, "pending")?, p.nonce(&key.address()?, "pending")?);
    if sender_bal < value {
        return Err(format!(
            "insufficient balance: wallet holds {:.6} ETH, transfer needs {amount_eth} ETH (+gas)",
            sender_bal.as_u128() as f64 / 1e18
        ));
    }
    let (mf, mp) = match (max_fee_gwei, max_priority_gwei) {
        (Some(a), Some(b)) => (a, b),
        _ => auto_fees(p),
    };
    let res = sign_and_send(p, key, to, value, vec![], 21_000, mf, mp)?;
    Ok((res, format!("max fee {mf} gwei, priority {mp} gwei")))
}

/// Signed state-changing contract call on a real network.
#[allow(clippy::too_many_arguments)]
pub fn write(
    p: &Provider,
    key: &KeyPair,
    to: H160,
    data: Vec<u8>,
    value: U256,
    max_fee_gwei: Option<u64>,
    max_priority_gwei: Option<u64>,
) -> Result<(SendResult, String), String> {
    let sender = key.address()?;
    let gas = p.estimate_gas(Some(sender), Some(&to), &data, value)?;
    let (mf, mp) = match (max_fee_gwei, max_priority_gwei) {
        (Some(a), Some(b)) => (a, b),
        _ => auto_fees(p),
    };
    let res = sign_and_send(p, key, to, value, data, gas + gas / 10, mf, mp)?;
    Ok((res, format!("estimated {gas} gas, max fee {mf} gwei, priority {mp} gwei")))
}

/// "1.5", "0.001" etc → wei
pub fn parse_eth(amount: &str) -> Result<U256, String> {
    let s = amount.trim().trim_end_matches("ether").trim();
    if s.is_empty() {
        return Err("empty amount".into());
    }
    let (whole, frac) = match s.split_once('.') {
        Some((w, f)) => (w, f),
        None => (s, ""),
    };
    let whole_n: u128 = whole
        .trim()
        .parse()
        .map_err(|_| format!("bad amount '{amount}'"))?;
    let mut frac_n: u128 = 0;
    let mut digits = 0;
    for c in frac.chars() {
        if !c.is_ascii_digit() {
            return Err(format!("bad amount '{amount}'"));
        }
        if digits < 18 {
            frac_n = frac_n * 10 + (c as u8 - b'0') as u128;
            digits += 1;
        }
    }
    while digits < 18 {
        frac_n *= 10;
        digits += 1;
    }
    Ok(U256::from(whole_n) * U256::from(10u128.pow(18)) + U256::from(frac_n))
}

pub fn print_outcome(res: &SendResult, label: &str, fees_note: &str) -> String {
    let mut out = String::new();
    out.push_str(&format!("{label}\n  tx {:#x}\n  fees: {fees_note}\n", res.tx_hash));
    match &res.outcome {
        Some(o) => {
            let (_g, fee_eth) = wei_to_strings(&(U256::from(o.gas_used) * o.effective_gas_price));
            out.push_str(&format!(
                "  status   {} (block {})\n  gas      {}\n  fee      {fee_eth} ETH\n",
                if o.status { "SUCCESS" } else { "REVERTED" },
                o.block_number,
                o.gas_used
            ));
        }
        None if res.timed_out => out.push_str("  status   broadcast ok, receipt not seen in 3 min — check the explorer\n"),
        None => out.push_str("  status   broadcast ok\n"),
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn eth_parsing() {
        assert_eq!(parse_eth("1").unwrap().as_u128(), 1_000_000_000_000_000_000);
        assert_eq!(parse_eth("1.5").unwrap().as_u128(), 1_500_000_000_000_000_000);
        assert_eq!(parse_eth("0.000000001").unwrap().as_u128(), 1_000_000_000); // 1 gwei
        assert_eq!(parse_eth("0.000000000000000001").unwrap().as_u128(), 1);
        assert_eq!(parse_eth("42 ether").unwrap().as_u128(), 42_000_000_000_000_000_000);
        assert!(parse_eth("1.2.3").is_err());
        assert!(parse_eth("").is_err());
        // 19th fractional digit is dropped (sub-wei precision)
        assert_eq!(parse_eth("0.0000000000000000009").unwrap().as_u128(), 0);
    }
}
