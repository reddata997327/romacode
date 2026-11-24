//! Gas Fee tool — the headline feature:
//! - `estimate`: real execution-based gas estimation with an intrinsic-cost breakdown
//! - `report`  : per-contract/per-method gas report (like hardhat gas-analytics)
//! - `price`   : live network gas prices (eth_gasPrice + eth_feeHistory) or local base fee
//! - `trace`   : real per-opcode gas metering from the EVM interpreter

use std::path::Path;

use rrx_chain::Chain;
use rrx_evm::{opcode_name, tx_intrinsic_gas};
use rrx_util::{wei_to_strings, H160, U256};

pub const BLOCK_GAS_LIMIT: u64 = 30_000_000;

// ---------------------------------------------------------------- estimate

pub struct EstimateResult {
    pub total: u64,
    pub intrinsic: u64,
    pub execution: u64,
    pub base_21000: u64,
    pub calldata_cost: u64,
    pub create_cost: u64,
    pub access_list_cost: u64,
    pub initcode_cost: u64,
    pub gas_price: U256,
    pub cost_wei: U256,
}

#[allow(clippy::too_many_arguments)]
pub fn estimate_local(
    chain: &Chain,
    from: H160,
    to: Option<H160>,
    data: Vec<u8>,
    value: U256,
) -> Result<EstimateResult, String> {
    let base_fee = chain.latest_header().base_fee_per_gas;
    let tx_env = rrx_evm::TxEnv {
        caller: from,
        gas_price: base_fee,
        value,
        data: data.clone(),
        gas_limit: chain.config.gas_limit,
        chain_id: chain.config.chain_id,
        to,
        access_list: vec![],
        blob_hashes: vec![],
    };
    let intrinsic = tx_intrinsic_gas(&tx_env, &chain.config.spec);

    // component breakdown
    let base_21000 = 21_000u64;
    let mut calldata_cost = 0u64;
    for b in &data {
        calldata_cost += if *b == 0 { 4 } else { 16 };
    }
    let create_cost = if to.is_none() { 32_000 } else { 0 };
    let initcode_cost = if to.is_none() { 2 * rrx_evm::interp_words(data.len()) } else { 0 };
    let access_list_cost = 0u64;
    let _execution = intrinsic - (base_21000 + calldata_cost.min(intrinsic) + create_cost);

    let est = chain.estimate_gas(from, to, value, data.clone())?;
    let gas_price = base_fee;
    Ok(EstimateResult {
        total: est,
        intrinsic,
        execution: est.saturating_sub(intrinsic),
        base_21000,
        calldata_cost,
        create_cost,
        access_list_cost,
        initcode_cost,
        gas_price,
        cost_wei: U256::from(est) * gas_price,
    })
}

impl EstimateResult {
    pub fn print(&self, target: &str) -> String {
        let (gwei, eth) = wei_to_strings(&self.cost_wei);
        let mut out = String::new();
        out.push_str(&format!("gas estimate for {target}\n"));
        out.push_str(&format!(
            "  intrinsic      {:>10}  (21000 base + {} calldata + {} create + {} initcode)\n",
            self.intrinsic,
            self.calldata_cost,
            self.create_cost,
            self.initcode_cost
        ));
        out.push_str(&format!("  execution      {:>10}  (real EVM run)\n", self.execution));
        out.push_str(&format!("  TOTAL          {:>10}\n", self.total));
        out.push_str(&format!(
            "  at gas price {} wei/gwei → {} wei ≈ {gwei} gwei ≈ {eth} ETH\n",
            self.gas_price, self.cost_wei
        ));
        out
    }
}

// ---------------------------------------------------------------- report

#[derive(Default, Clone)]
struct MethodStat {
    calls: u64,
    min: u64,
    max: u64,
    total: u128,
}

/// Aggregate gas records (from the test runner / script runner) into a report.
pub fn report_from_records(records: &[serde_json::Value], style: &str) -> String {
    let mut stats: std::collections::BTreeMap<(String, String), MethodStat> = Default::default();
    for r in records {
        let contract = r["contract"].as_str().unwrap_or("?").to_string();
        let method = r["method"].as_str().unwrap_or("?").to_string();
        let gas = r["gas"].as_u64().unwrap_or(0);
        let e = stats.entry((contract, method)).or_default();
        e.calls += 1;
        e.total += gas as u128;
        e.min = if e.min == 0 { gas } else { e.min.min(gas) };
        e.max = e.max.max(gas);
    }
    if stats.is_empty() {
        return "no gas data yet — run `rrx test` or `rrx script` first\n".into();
    }

    let mut out = String::new();
    let mut current = String::new();
    let mut total_gas: u128 = 0;
    let mut rows = String::new();
    for ((contract, method), s) in &stats {
        if contract != &current {
            out.push_str(&rows);
            rows.clear();
            current = contract.clone();
            rows.push_str(&format!("\n▶ {contract}\n"));
            rows.push_str(&format!(
                "  {:40} {:>6} {:>10} {:>10} {:>10} {:>8}\n",
                "method", "calls", "min", "avg", "max", "% block"
            ));
        }
        let avg = (s.total / s.calls as u128) as u64;
        let pct = (avg as f64 / BLOCK_GAS_LIMIT as f64) * 100.0;
        total_gas += s.total;
        rows.push_str(&format!(
            "  {method:40} {:>6} {:>10} {:>10} {:>10} {:>7.3}%\n",
            s.calls, s.min, avg, s.max, pct
        ));
    }
    out.push_str(&rows);
    out.push_str(&format!(
        "\n  total gas burned: {total_gas} ≈ {} gwei at 1 gwei\n",
        total_gas / 1_000_000_000
    ));

    if style == "markdown" || style == "md" {
        let mut md = String::from("| contract | method | calls | min | avg | max | % of block |\n|---|---|---|---|---|---|---|\n");
        for ((contract, method), s) in &stats {
            let avg = (s.total / s.calls as u128) as u64;
            let pct = (avg as f64 / BLOCK_GAS_LIMIT as f64) * 100.0;
            md.push_str(&format!(
                "| {contract} | {method} | {} | {} | {} | {} | {pct:.3}% |\n",
                s.calls, s.min, avg, s.max
            ));
        }
        out.push_str("\n");
        out.push_str(&md);
    }
    out
}

pub fn report_from_file(path: &Path, style: &str) -> Result<String, String> {
    let records: Vec<serde_json::Value> = std::fs::read_to_string(path)
        .ok()
        .and_then(|s| serde_json::from_str(&s).ok())
        .unwrap_or_default();
    Ok(report_from_records(&records, style))
}

// ---------------------------------------------------------------- price

pub struct PriceInfo {
    pub base_fee: U256,
    pub suggestions: [(String, U256); 3],
}

pub fn price_local(chain: &Chain) -> PriceInfo {
    let base = chain.latest_header().base_fee_per_gas;
    PriceInfo {
        base_fee: base,
        suggestions: [
            ("slow".into(), base + U256::from(100_000_000u64)),
            ("standard".into(), base + U256::from(1_000_000_000u64)),
            ("fast".into(), base + U256::from(2_000_000_000u64)),
        ],
    }
}

pub fn price_rpc(provider: &rrx_rpc::Provider) -> Result<PriceInfo, String> {
    let base_fees = provider.fee_history(5)?.0;
    let base = base_fees.last().copied().unwrap_or_default();
    let (_hist, rewards) = provider.fee_history(5)?;
    let median = |mut v: Vec<U256>| -> U256 {
        v.sort();
        v.get(v.len() / 2).copied().unwrap_or_default()
    };
    let p25: Vec<U256> = rewards.iter().filter_map(|r| r.first().copied()).collect();
    let p50: Vec<U256> = rewards.iter().filter_map(|r| r.get(1).copied()).collect();
    let p75: Vec<U256> = rewards.iter().filter_map(|r| r.get(2).copied()).collect();
    Ok(PriceInfo {
        base_fee: base,
        suggestions: [
            ("slow".into(), base + median(p25)),
            ("standard".into(), base + median(p50)),
            ("fast".into(), base + median(p75)),
        ],
    })
}

impl PriceInfo {
    pub fn print(&self, network: &str) -> String {
        let mut out = String::new();
        out.push_str(&format!("gas price on {network}\n"));
        let (bg, _be) = wei_to_strings(&self.base_fee);
        out.push_str(&format!("  base fee: {} gwei\n", bg));
        for (name, wei) in &self.suggestions {
            let (g, _e) = wei_to_strings(wei);
            out.push_str(&format!("  {name:9} tip → {} gwei total\n", g));
        }
        out.push_str("\n  reference costs at base fee:\n");
        for (label, gas) in [
            ("plain ETH transfer", 21_000u64),
            ("ERC-20 transfer", 50_000),
            ("ERC-20 approve", 46_000),
            ("uniswap-style swap", 150_000),
            ("contract deploy (small)", 500_000),
        ] {
            let cost = U256::from(gas) * self.base_fee;
            let (g, e) = wei_to_strings(&cost);
            out.push_str(&format!("    {label:26} {gas:>8} gas ≈ {g} gwei ≈ {e} ETH\n"));
        }
        out
    }
}

// ---------------------------------------------------------------- trace

pub fn trace_execution(
    chain: &Chain,
    from: H160,
    to: Option<H160>,
    data: Vec<u8>,
    value: U256,
    top: usize,
) -> Result<String, String> {
    let res = chain.call(from, to, value, data, chain.config.gas_limit, true);
    let trace = res.trace.as_ref().ok_or("no trace recorded")?;
    let mut ops: Vec<(u8, u128)> = trace.gas_by_opcode.iter().map(|(k, v)| (*k, *v)).collect();
    ops.sort_by(|a, b| b.1.cmp(&a.1));
    let mut out = String::new();
    out.push_str(&format!(
        "execution trace: {} step(s), total metered gas {} (result: {:?})\n\n",
        trace.steps.len(),
        trace.total_gas,
        res.kind
    ));
    out.push_str(&format!(
        "  {:6} {:>14} {}\n",
        "% gas", "gas", "opcode"
    ));
    for (op, gas) in ops.iter().take(top) {
        let pct = (*gas as f64 / trace.total_gas.max(1) as f64) * 100.0;
        out.push_str(&format!(
            "  {pct:5.1}% {:>14} {} (0x{op:02x})\n",
            gas,
            opcode_name(*op)
        ));
    }
    Ok(out)
}
