//! debugtx: replay a REAL confirmed mainnet transaction in the rrx interpreter.
//!
//! Pipeline (all against the live chain, no simulation):
//!  1. fetch the tx + receipt from the network
//!  2. build the block env from the real header, the interpreter spec from the
//!     real fork heights, and the PRE-tx state (block N-1) via RemoteHost
//!  3. execute with our own EVM, then compare gas/status against the receipt
//!  4. opcode-level gas profile + optional step trace

use rrx_evm::{tx_intrinsic_gas, Machine, TxEnv};
use rrx_rpc::Provider;
use rrx_util::{h160_from_hex, h256_from_hex, u256_from_str, wei_to_strings, H160, U256};

use crate::remotehost::{access_list_from_tx, block_env_of, mainnet_spec, RemoteHost};

fn qty(v: &serde_json::Value, key: &str) -> Option<U256> {
    v.get(key).and_then(|x| x.as_str()).and_then(|s| u256_from_str(s).ok())
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

/// Replay a confirmed transaction and render the full debugging report.
pub fn debug_mainnet_tx(p: &Provider, hash_str: &str, max_steps: usize, top_ops: usize) -> Result<String, String> {
    let hash = h256_from_hex(hash_str)?;
    let tx = p
        .get_transaction(&hash)?
        .ok_or("transaction not found (pending or unknown)")?;
    let receipt = p
        .transaction_receipt(&hash)?
        .ok_or("transaction not confirmed yet — no receipt")?;

    let block_n = qty(&receipt, "blockNumber")
        .map(|b| b.as_u64())
        .filter(|b| *b > 0)
        .ok_or("receipt has no block number")?;
    let chain_id = p.chain_id()?;
    let spec = mainnet_spec(block_n);

    let from = h160_from_hex(tx.get("from").and_then(|x| x.as_str()).ok_or("tx has no from")?)?;
    let to_opt: Option<H160> = match tx.get("to").and_then(|x| x.as_str()) {
        Some(s) if !s.is_empty() && s != "0x" => Some(h160_from_hex(s)?),
        _ => None,
    };
    let value = qty(&tx, "value").unwrap_or_default();
    let input = tx
        .get("input")
        .and_then(|i| i.as_str())
        .and_then(|s| rrx_util::from_hex(s).ok())
        .unwrap_or_default();
    let gas_cap = qty(&tx, "gas").map(|g| g.as_u64()).unwrap_or(30_000_000);
    let access_list = access_list_from_tx(&tx);

    // effective gas price
    let effective = qty(&receipt, "effectiveGasPrice")
        .or_else(|| qty(&tx, "gasPrice"))
        .unwrap_or_default();

    let mut out = String::new();
    out.push_str(&format!("debugtx {hash_str}\n"));
    out.push_str(&format!(
        "  network    chain {chain_id} — replayed in the rrx EVM interpreter ({})\n",
        if spec.push0 { if spec.transient_storage { "cancun" } else { "shanghai" } } else { "pre-shanghai" }
    ));
    out.push_str(&format!("  block      {} (state read from block {})\n", fmt_u64(block_n), fmt_u64(block_n - 1)));
    out.push_str(&format!(
        "  from       {}\n  to         {}\n",
        from,
        to_opt.map(|t| format!("0x{t:x}")).unwrap_or_else(|| "(contract creation)".into())
    ));
    let (_g, eth) = wei_to_strings(&value);
    out.push_str(&format!("  value      {eth} ETH\n"));
    out.push_str(&format!(
        "  calldata   {} bytes, selector {}\n",
        input.len(),
        if input.len() >= 4 { format!("0x{}", rrx_util::to_hex(&input[..4])) } else { "—".into() }
    ));

    // ---------------------------------------------------------------- replay
    let state_block = format!("0x{:x}", block_n - 1);
    let block_env = block_env_of(p, block_n)?;
    let tx_env_full = TxEnv {
        caller: from,
        gas_price: effective,
        value,
        data: input.clone(),
        gas_limit: gas_cap,
        chain_id,
        to: to_opt,
        access_list: access_list.clone(),
        blob_hashes: vec![],
    };
    let intrinsic = tx_intrinsic_gas(&tx_env_full, &spec);
    let mut env = tx_env_full.clone();
    env.gas_limit = gas_cap.saturating_sub(intrinsic);

    let mut host = RemoteHost::new(p.clone(), state_block, spec.clone());
    let machine = Machine::new(&mut host, spec, block_env, env, true);
    let result = machine.run_tx();
    let replayed_gas = result.gas_used + intrinsic;

    let chain_status = receipt.get("status").and_then(|s| s.as_str()).unwrap_or("0x0");
    let chain_gas = qty(&receipt, "gasUsed").map(|g| g.as_u64()).unwrap_or(0);
    let replay_ok = result.is_success();
    let chain_ok = chain_status == "0x1";

    out.push_str(&format!(
        "  chain      {} — {} gas used @ {} gwei  (fee ≈ {:.6} ETH)\n",
        if chain_ok { "SUCCESS" } else { "REVERTED" },
        fmt_u64(chain_gas),
        effective / U256::from(1_000_000_000u64),
        (U256::from(chain_gas) * effective).as_u128() as f64 / 1e18
    ));
    let match_note = if replay_ok == chain_ok && replayed_gas == chain_gas {
        "exact match".to_string()
    } else if replay_ok == chain_ok {
        format!(
            "status matches, gas Δ {} ({:.2}%) — access-list / warm-state differences",
            (replayed_gas as i128 - chain_gas as i128),
            if chain_gas > 0 { (replayed_gas as f64 - chain_gas as f64).abs() * 100.0 / chain_gas as f64 } else { 0.0 }
        )
    } else {
        "status DIVERGES (state of earlier txs in the same block affects this tx)".to_string()
    };
    out.push_str(&format!(
        "  replay     {} — {} gas used  [{match_note}]\n",
        if replay_ok { "SUCCESS" } else { "REVERTED" },
        fmt_u64(replayed_gas)
    ));
    if let Some(r) = result.revert_reason() {
        out.push_str(&format!("  revert     {r}\n"));
    }

    // ------------------------------------------------------- opcode profile
    if let Some(trace) = &result.trace {
        out.push_str(&format!(
            "\nopcode gas profile ({} steps, {} metered gas, {} RPC state reads):\n",
            trace.steps.len(),
            trace.total_gas,
            host.rpc_reads.borrow()
        ));
        let mut ops: Vec<(u8, u128)> = trace.gas_by_opcode.iter().map(|(k, v)| (*k, *v)).collect();
        ops.sort_by(|a, b| b.1.cmp(&a.1));
        for (op, gas) in ops.iter().take(top_ops) {
            let pct = (*gas as f64 / trace.total_gas.max(1) as f64) * 100.0;
            out.push_str(&format!("  {:<14} {:>12} gas  {:>5.1}%\n", rrx_evm::opcode_name(*op), fmt_u64(*gas as u64), pct));
        }

        if max_steps > 0 {
            out.push_str(&format!("\nfirst {max_steps} steps:\n"));
            out.push_str("     pc  opcode            gas  stack(top)          depth\n");
            for (i, s) in trace.steps.iter().take(max_steps).enumerate() {
                let stack_top: String = s
                    .stack
                    .as_ref()
                    .and_then(|v| v.first())
                    .map(|v| format!("0x{v:x}"))
                    .unwrap_or_else(|| "—".into());
                out.push_str(&format!(
                    "  {:>5}  {:<14} {:>6}  {:<18} {}\n",
                    s.pc,
                    rrx_evm::opcode_name(s.opcode),
                    s.gas_cost,
                    stack_top,
                    s.depth
                ));
                let _ = i;
            }
            if trace.steps.len() > max_steps {
                out.push_str(&format!("  … {} more steps (increase --max-steps to see)\n", trace.steps.len() - max_steps));
            }
        }
    }
    Ok(out)
}
