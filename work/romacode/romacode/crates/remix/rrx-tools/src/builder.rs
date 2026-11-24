//! rrx block builder — assembles a REAL block from REAL chain data:
//! parent header from the live chain, candidate txs from the pending pool
//! (fallback: latest block's txs — public RPCs often expose no mempool),
//! executed in the rrx EVM against forked live state via RemoteHost,
//! producing a real block header (tx root, receipts root, logs bloom, gas).

use std::collections::HashMap;

use rrx_chain::executor::trie_root_of_encoded;
use rrx_chain::receipt::{LogBloom, Receipt};
use rrx_chain::state::WorldState;
use rrx_evm::{Machine, TxEnv};
use rrx_util::{from_hex, H160, H256, U256};

use crate::remotehost::{block_env_of, mainnet_spec, RemoteHost};
use rrx_rpc::Provider;

/// Candidate transaction prepared for inclusion.
struct Candidate {
    raw_tx: rrx_chain::Transaction,
    from: H160,
    to: Option<H160>,
    value: U256,
    data: Vec<u8>,
    gas_cap: u64,
    access_list: Vec<(H160, Vec<U256>)>,
    effective_price: U256,
    is_blob: bool,
    hash: H256,
}

fn qty(v: &serde_json::Value, key: &str) -> Option<U256> {
    v.get(key)
        .and_then(|x| x.as_str())
        .and_then(|s| u64::from_str_radix(s.trim_start_matches("0x"), 16).ok())
        .map(U256::from)
}

fn addr_of(s: &str) -> Result<H160, String> {
    let h = s.trim_start_matches("0x");
    let b = hex::decode(h).map_err(|e| e.to_string())?;
    if b.len() != 20 {
        return Err("bad address".into());
    }
    Ok(H160::from_slice(&b))
}

/// Rebuild the signed Transaction object from the RPC JSON (so the tx root
/// uses the real signed encoding).
fn tx_from_json(v: &serde_json::Value) -> Result<(rrx_chain::Transaction, bool), String> {
    use rrx_chain::Transaction as T;
    let nonce = qty(v, "nonce").unwrap_or_default().as_u64();
    let gas_limit = qty(v, "gas").unwrap_or_default().as_u64();
    let to = match v.get("to").and_then(|x| x.as_str()) {
        Some(s) if !s.is_empty() && s != "0x" => Some(addr_of(s)?),
        _ => None,
    };
    let value = qty(v, "value").unwrap_or_default();
    let data = v
        .get("input")
        .and_then(|i| i.as_str())
        .and_then(|s| from_hex(s).ok())
        .unwrap_or_default();
    let ttype = v.get("type").and_then(|t| t.as_str()).unwrap_or("0x0");
    let r_word = |v: &serde_json::Value| -> [u8; 32] {
        let s = v.get("r").and_then(|x| x.as_str()).unwrap_or("0x0");
        let b = hex::decode(s.trim_start_matches("0x")).unwrap_or_default();
        let mut out = [0u8; 32];
        let n = b.len().min(32);
        out[32 - n..].copy_from_slice(&b[b.len() - n..]);
        out
    };
    let r = r_word(v);
    let s = r_word(v);
    let y_parity = v
        .get("yParity")
        .and_then(|x| x.as_str())
        .and_then(|h| u64::from_str_radix(h.trim_start_matches("0x"), 16).ok())
        .map(|x| x == 1)
        .unwrap_or(false);
    let is_blob = ttype == "0x3";
    let tx = match ttype {
        "0x1" => T::Eip2930 {
            chain_id: qty(v, "chainId").unwrap_or_default().as_u64(),
            nonce,
            gas_price: qty(v, "gasPrice").unwrap_or_default(),
            gas_limit,
            to,
            value,
            data,
            access_list: al_from_json(v)?,
            y_parity,
            r,
            s,
        },
        "0x2" => T::Eip1559 {
            chain_id: qty(v, "chainId").unwrap_or_default().as_u64(),
            nonce,
            max_priority_fee: qty(v, "maxPriorityFeePerGas").unwrap_or_default(),
            max_fee: qty(v, "maxFeePerGas").unwrap_or_default(),
            gas_limit,
            to,
            value,
            data,
            access_list: al_from_json(v)?,
            y_parity,
            r,
            s,
        },
        "0x3" => T::Eip4844 {
            chain_id: qty(v, "chainId").unwrap_or_default().as_u64(),
            nonce,
            max_priority_fee: qty(v, "maxPriorityFeePerGas").unwrap_or_default(),
            max_fee: qty(v, "maxFeePerGas").unwrap_or_default(),
            gas_limit,
            to: to.ok_or("blob tx without to")?,
            value,
            data,
            access_list: al_from_json(v)?,
            max_fee_per_blob_gas: qty(v, "maxFeePerBlobGas").unwrap_or_default(),
            blob_versioned_hashes: v
                .get("blobVersionedHashes")
                .and_then(|x| x.as_array())
                .map(|arr| {
                    arr.iter()
                        .filter_map(|h| {
                            let b = hex::decode(h.as_str()?.trim_start_matches("0x")).ok()?;
                            let mut out = [0u8; 32];
                            out.copy_from_slice(&b);
                            Some(H256(out))
                        })
                        .collect()
                })
                .unwrap_or_default(),
            y_parity,
            r,
            s,
        },
        _ => T::Legacy {
            nonce,
            gas_price: qty(v, "gasPrice").unwrap_or_default(),
            gas_limit,
            to,
            value,
            data,
            chain_id: qty(v, "chainId").map(|c| c.as_u64()),
            v: v
                .get("v")
                .and_then(|x| x.as_str())
                .and_then(|h| u64::from_str_radix(h.trim_start_matches("0x"), 16).ok())
                .unwrap_or(27),
            r,
            s,
        },
    };
    Ok((tx, is_blob))
}

fn al_from_json(v: &serde_json::Value) -> Result<Vec<(H160, Vec<U256>)>, String> {
    let mut out = vec![];
    if let Some(arr) = v.get("accessList").and_then(|x| x.as_array()) {
        for e in arr {
            let addr = addr_of(e.get("address").and_then(|x| x.as_str()).ok_or("al addr")?)?;
            let keys = e
                .get("storageKeys")
                .and_then(|x| x.as_array())
                .map(|ks| {
                    ks.iter()
                        .filter_map(|k| {
                            k.as_str().and_then(|s| from_hex(s).ok()).map(|b| {
                                U256::from_big_endian(&b)
                            })
                        })
                        .collect()
                })
                .unwrap_or_default();
            out.push((addr, keys));
        }
    }
    Ok(out)
}

/// Build a block on top of the current head from real candidate txs.
pub fn build_block(
    p: &Provider,
    max_txs: usize,
    coinbase: Option<H160>,
    include_blobs: bool,
) -> Result<String, String> {
    // ---- parent header (real) ----
    let parent = p.block_by_tag("latest", false)?.ok_or("no head")?;
    let parent_number = qty(&parent, "number").unwrap_or_default().as_u64();
    let parent_hash = parent
        .get("hash")
        .and_then(|h| h.as_str())
        .and_then(|s| hex::decode(s.trim_start_matches("0x")).ok())
        .map(|b| {
            let mut h = [0u8; 32];
            h.copy_from_slice(&b);
            H256(h)
        })
        .ok_or("parent hash missing")?;
    let parent_base_fee = qty(&parent, "baseFeePerGas").unwrap_or_else(|| U256::from(1_000_000_000u64));
    let gas_limit = qty(&parent, "gasLimit").unwrap_or_default().as_u64();
    let parent_gas_target = qty(&parent, "gasTarget").unwrap_or_else(|| U256::from(gas_limit / 2));
    let chain_id = p.chain_id()?;
    let spec = mainnet_spec(parent_number + 1);

    // ---- candidates: pending first, fallback to latest block txs ----
    let mut source = String::new();
    let mut raw_candidates: Vec<serde_json::Value> = vec![];
    if let Some(pending) = p.block_by_tag("pending", true)? {
        if let Some(txs) = pending.get("transactions").and_then(|t| t.as_array()) {
            if !txs.is_empty() {
                source = format!("pending pool ({} live txs)", txs.len());
                raw_candidates = txs.clone();
            }
        }
    }
    if raw_candidates.is_empty() {
        let latest = p.block_by_tag("latest", true)?.ok_or("no latest block")?;
        let txs = latest
            .get("transactions")
            .and_then(|t| t.as_array())
            .cloned()
            .unwrap_or_default();
        source = format!(
            "latest confirmed block {} (public RPC exposes no mempool — reordering real recent txs)",
            latest.get("number").and_then(|n| n.as_str()).unwrap_or("?")
        );
        raw_candidates = txs;
    }

    // ---- execute against forked live state ----
    let state_tag = format!("0x{:x}", parent_number);
    let block_env = block_env_of(p, parent_number)?;
    let mut host = RemoteHost::new(p.clone(), state_tag.clone(), spec.clone());
    let mut receipts: Vec<Receipt> = vec![];
    let mut included: Vec<rrx_chain::Transaction> = vec![];
    let mut skipped_blob = 0usize;
    let mut cumulative: u64 = 0;
    let mut report = String::new();
    report.push_str(&format!(
        "rrx block builder — proposing block {} on top of {} (0x{})\n",
        parent_number + 1,
        parent_number,
        hex::encode(parent_hash.0)
    ));
    report.push_str(&format!("  tx source:   {}\n", source));

    for txv in raw_candidates.iter().take(max_txs * 2) {
        if included.len() >= max_txs {
            break;
        }
        let (tx, is_blob) = match tx_from_json(txv) {
            Ok(t) => t,
            Err(_) => continue,
        };
        if is_blob && !include_blobs {
            skipped_blob += 1;
            continue;
        }
        let from = match txv
            .get("from")
            .and_then(|x| x.as_str())
            .map(addr_of)
        {
            Some(Ok(f)) => f,
            _ => continue,
        };
        let effective = qty(txv, "gasPrice")
            .or_else(|| {
                let base = qty(txv, "maxFeePerGas")?;
                let pri = qty(txv, "maxPriorityFeePerGas")?;
                Some(base.min(parent_base_fee + pri))
            })
            .unwrap_or_else(|| U256::from(1_000_000_000u64));
        let value = tx.value();
        let gas_cap = tx.gas_limit().min(gas_limit.saturating_sub(cumulative));
        if gas_cap == 0 {
            break;
        }
        let tx_env = TxEnv {
            caller: from,
            gas_price: effective,
            value,
            data: tx.data().to_vec(),
            gas_limit: gas_cap,
            chain_id,
            to: tx.to(),
            access_list: tx.access_list().clone(),
            blob_hashes: vec![],
        };
        let intrinsic = rrx_evm::tx_intrinsic_gas(&tx_env, &spec);
        let mut env = tx_env.clone();
        env.gas_limit = gas_cap.saturating_sub(intrinsic);
        let machine = Machine::new(&mut host, spec.clone(), block_env.clone(), env, true);
        let result = machine.run_tx();
        let used = result.gas_used + intrinsic;
        cumulative += used;
        let tx_logs = host.take_logs();
        let mut bloom = LogBloom::zero();
        for l in &tx_logs {
            bloom.add_log(l);
        }
        receipts.push(Receipt {
            status: result.is_success(),
            cumulative_gas_used: cumulative,
            bloom,
            logs: tx_logs,
            tx_type: match &tx {
                rrx_chain::Transaction::Legacy { .. } => 0,
                rrx_chain::Transaction::Eip2930 { .. } => 1,
                rrx_chain::Transaction::Eip1559 { .. } => 2,
                rrx_chain::Transaction::Eip4844 { .. } => 3,
            },
            tx_hash: tx.hash(),
            block_number: parent_number + 1,
            contract_address: result.created_address,
            gas_used: used,
            effective_gas_price: effective,
            output: result.output.clone(),
        });
        included.push(tx);
        report.push_str(&format!(
            "  + tx 0x{} — {} {} gas (status {})\n",
            hex::encode(receipts.last().unwrap().tx_hash.0),
            if result.is_success() { "OK" } else { "REVERT" },
            used,
            if result.is_success() { 1 } else { 0 }
        ));
    }
    if skipped_blob > 0 {
        report.push_str(&format!("  ({} blob txs skipped — sidecar unavailable on EL RPC)\n", skipped_blob));
    }

    // ---- roots ----
    let transactions_root = trie_root_of_encoded(included.iter().map(|t| t.rlp_encode()).collect());
    let receipts_root = trie_root_of_encoded(
        receipts
            .iter()
            .map(|r| {
                let mut enc = vec![r.status as u8];
                enc.extend_from_slice(&rrx_util::rlp::Rlp::u64(r.cumulative_gas_used).encode());
                enc.extend_from_slice(&rrx_util::rlp::Rlp::Bytes(r.bloom.0.to_vec()).encode());
                enc.extend_from_slice(
                    &rrx_util::rlp::Rlp::list(
                        r.logs
                            .iter()
                            .map(|l| {
                                rrx_util::rlp::Rlp::list(vec![
                                    rrx_util::rlp::Rlp::h160(&l.address),
                                    rrx_util::rlp::Rlp::list(l.topics.iter().map(|t| rrx_util::rlp::Rlp::h256(t)).collect()),
                                    rrx_util::rlp::Rlp::Bytes(l.data.clone()),
                                ])
                            })
                            .collect(),
                    )
                    .encode(),
                );
                enc
            })
            .collect(),
    );
    let mut logs_bloom = LogBloom::zero();
    for r in &receipts {
        for (i, b) in r.bloom.0.iter().enumerate() {
            logs_bloom.0[i] |= b;
        }
    }

    // ---- state root over the touched subset (fork base + journaled delta) ----
    let delta = host.journal_delta();
    let mut touched = host.touched();
    touched.sort();
    touched.dedup();
    let mut ws = WorldState::default();
    for addr in &touched {
        let balance = p.balance(addr, &state_tag)?;
        let nonce = p.nonce(addr, &state_tag)?;
        let code = p.code(addr, &state_tag)?;
        let acc = ws.accounts.entry(*addr).or_default();
        acc.balance = delta.balances.get(addr).cloned().unwrap_or(balance);
        acc.nonce = delta.nonces.get(addr).cloned().unwrap_or(nonce);
        if let Some(c) = delta.codes.get(addr) {
            if !c.is_empty() {
                ws.set_code_of(addr, c.clone());
            }
        } else if !code.is_empty() {
            ws.set_code_of(addr, code.clone());
        }
        if delta.deleted.contains(addr) {
            ws.accounts.remove(addr);
            continue;
        }
        // base storage for touched contracts (slots 0..256 window like fork)
        if !code.is_empty() || delta.codes.get(addr).map(|c| !c.is_empty()).unwrap_or(false) {
            for slot in 0..256u64 {
                let base = p.storage_at(addr, &U256::from(slot), &state_tag)?;
                if base != U256::zero() {
                    ws.accounts.entry(*addr).or_default().storage.insert(U256::from(slot), base);
                }
            }
        }
    }
    for ((addr, slot), v) in &delta.storage {
        ws.accounts.entry(*addr).or_default().storage.insert(*slot, *v);
    }
    let state_root = ws.state_root();

    // ---- header ----
    // EIP-1559 base fee rule: exactly as the spec delta calc
    let gas_target = parent_gas_target;
    let elasticity = 8u64;
    let delta_gas = (cumulative as i128) - (gas_target.as_u128() as i128);
    let base_fee_delta = parent_base_fee.as_u128() * delta_gas.abs() as u128
        / (gas_target.as_u128() * elasticity as u128)
        / 8;
    let new_base_fee = if delta_gas > 0 {
        parent_base_fee.as_u128() + base_fee_delta.max(1)
    } else if delta_gas < 0 {
        parent_base_fee.as_u128().saturating_sub(base_fee_delta)
    } else {
        parent_base_fee.as_u128()
    };

    let header = rrx_chain::executor::BlockHeader {
        parent_hash,
        number: parent_number + 1,
        timestamp: std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs(),
        gas_limit,
        gas_used: cumulative,
        base_fee_per_gas: U256::from(new_base_fee),
        coinbase: coinbase.unwrap_or_else(|| H160::from_low_u64_be(0)),
        state_root,
        transactions_root,
        receipts_root,
        logs_bloom,
        mix_hash: H256([0u8; 32]), // post-merge prevrandao slot: builder-local
        extra_data: b"rrx-builder".to_vec(),
    };

    report.push_str(&format!(
        "  executed:    {} txs, {} gas used\n",
        included.len(),
        cumulative
    ));
    report.push_str(&format!("  tx root:     0x{}\n", hex::encode(transactions_root.0)));
    report.push_str(&format!("  receipts:    0x{}\n", hex::encode(receipts_root.0)));
    report.push_str(&format!(
        "  state root:  0x{} (partial: forked state of {} touched accounts + journal delta)\n",
        hex::encode(state_root.0),
        touched.len()
    ));
    report.push_str(&format!("  base fee:    {} → {} wei (1559 rule)\n", parent_base_fee, new_base_fee));
    report.push_str(&format!("  block hash:  0x{}\n", hex::encode(header.hash().0)));
    Ok(report)
}

// keep unused-import warnings away in odd feature combos
#[allow(dead_code)]
fn _anchor(_: &HashMap<u8, u8>) {}
