//! rrx fuzz — property-based fuzzing and invariant testing executed on the REAL
//! rrx EVM (real opcodes, real gas, real reverts). Deterministic seeds make any
//! counterexample reproducible; shrinking reduces failing inputs.

use std::path::Path;

use rrx_chain::Chain;
use rrx_util::U256;

use crate::compile::{self, CompileOutput};
use crate::deploy::{self, AccountSpec};

// ---------------------------------------------------------------------------
// tiny deterministic RNG (xorshift64*) — no external deps
// ---------------------------------------------------------------------------

pub struct Rng(u64);

impl Rng {
    pub fn new(seed: u64) -> Rng {
        Rng(if seed == 0 { 0x9E3779B97F4A7C15 } else { seed })
    }
    pub fn next_u64(&mut self) -> u64 {
        let mut x = self.0;
        x ^= x >> 12;
        x ^= x << 25;
        x ^= x >> 27;
        self.0 = x;
        x.wrapping_mul(0x2545F4914F6CDD1D)
    }
    pub fn below(&mut self, n: u64) -> u64 {
        if n == 0 {
            0
        } else {
            self.next_u64() % n
        }
    }
    pub fn pick<'a, T>(&mut self, items: &'a [T]) -> &'a T {
        let i = self.below(items.len() as u64) as usize;
        &items[i]
    }
}

// ---------------------------------------------------------------------------
// ABI type-aware input generation
// ---------------------------------------------------------------------------

/// Generate a random argument string for a Solidity type.
pub fn gen_arg(rng: &mut Rng, ty: &str) -> String {
    let t = ty.trim();
    if let Some(base) = t.strip_suffix("[]") {
        // small fixed arrays
        let len = 1 + rng.below(3) as usize;
        let items: Vec<String> = (0..len).map(|_| gen_arg(rng, base)).collect();
        return format!("[{}]", items.join(","));
    }
    if t == "address" {
        return format!("0x{:040x}", rng.next_u64() as u128 | ((rng.next_u64() as u128) << 64));
    }
    if t == "bool" {
        return (rng.next_u64() % 2 == 0).to_string();
    }
    if t == "string" {
        const WORDS: &[&str] = &["alpha", "beta", "gamma", "delta", "", "x", "rrx fuzz", "Ω"];
        let mut s = rng.pick(WORDS).to_string();
        if rng.next_u64() % 3 == 0 {
            s.push_str(rng.pick(WORDS));
        }
        return s;
    }
    if t == "bytes" || t.starts_with("bytes") && t[5..].parse::<usize>().is_err() {
        let len = 1 + rng.below(8) as usize;
        let hex: Vec<String> = (0..len).map(|_| format!("{:02x}", rng.below(256))).collect();
        return hex.join("");
    }
    if t.starts_with("uint") || t.starts_with("int") {
        let bits: u64 = t
            .trim_start_matches("uint")
            .trim_start_matches("int")
            .parse()
            .unwrap_or(256);
        let bits = if bits == 0 { 8 } else { bits.min(256) };
        let max: u128 = if bits >= 128 { u128::MAX } else { (1u128 << bits) - 1 };
        let mut v = rng.next_u64() as u128;
        // bias: hit interesting corners sometimes
        match rng.below(10) {
            0 => v = 0,
            1 => v = max,
            2 => v = max / 2,
            3 => v = 1,
            _ => {}
        }
        if t.starts_with("int") && rng.next_u64() % 2 == 0 {
            return format!("-{v}");
        }
        return v.to_string();
    }
    // bytesN fixed
    if let Some(rest) = t.strip_prefix("bytes") {
        let n: usize = rest.parse().unwrap_or(32);
        let hex: Vec<String> = (0..n).map(|_| format!("{:02x}", rng.below(256))).collect();
        return hex.join("");
    }
    // fallback: uint
    rng.next_u64().to_string()
}

/// Shrink a failing argument towards zero/simpler values.
pub fn shrink_arg(ty: &str, val: &str) -> String {
    let t = ty.trim();
    if t == "bool" || t == "address" || t == "string" {
        return val.to_string();
    }
    if let Ok(n) = val.parse::<u128>() {
        return (n / 2).to_string();
    }
    if let Ok(n) = val.parse::<i128>() {
        return (n / 2).to_string();
    }
    val.to_string()
}

// ---------------------------------------------------------------------------
// fuzz runner
// ---------------------------------------------------------------------------

pub struct FuzzConfig {
    pub runs: u64,
    pub seed: u64,
    pub account: AccountSpec,
}

pub struct FuzzOutcome {
    pub passed: bool,
    pub runs_executed: u64,
    pub reverts: u64,
    pub gas_used: u64,
    pub counterexample: Option<(String, Vec<String>)>, // (function, args)
    pub log: String,
}

fn abi_types_for(output: &CompileOutput, contract: &str, function: &str) -> Option<(String, Vec<String>)> {
    for (name, art) in &output.artifacts {
        let short = name.rsplit(':').next().unwrap_or(name);
        if short != contract {
            continue;
        }
        let abi = compile::abi_of(art);
        for entry in abi.as_array()? {
            if entry.get("type").and_then(|t| t.as_str()) == Some("function")
                && entry.get("name").and_then(|n| n.as_str()) == Some(function)
            {
                let types: Vec<String> = entry
                    .get("inputs")
                    .and_then(|i| i.as_array())
                    .map(|arr| {
                        arr.iter()
                            .filter_map(|x| x.get("type").and_then(|t| t.as_str()).map(|s| s.to_string()))
                            .collect()
                    })
                    .unwrap_or_default();
                return Some((name.clone(), types));
            }
        }
    }
    None
}

fn selector_types(output: &CompileOutput, contract: &str) -> Vec<(String, Vec<String>)> {
    let mut fns = vec![];
    for (name, art) in &output.artifacts {
        let short = name.rsplit(':').next().unwrap_or(name);
        if short != contract {
            continue;
        }
        if let Some(entries) = compile::abi_of(art).as_array() {
            for entry in entries {
                if entry.get("type").and_then(|t| t.as_str()) != Some("function") {
                    continue;
                }
                let state = entry.get("stateMutability").and_then(|s| s.as_str()).unwrap_or("");
                if state == "view" || state == "pure" {
                    continue;
                }
                let name = entry.get("name").and_then(|n| n.as_str()).unwrap_or("");
                if name.is_empty() {
                    continue;
                }
                let types: Vec<String> = entry
                    .get("inputs")
                    .and_then(|i| i.as_array())
                    .map(|arr| {
                        arr.iter()
                            .filter_map(|x| x.get("type").and_then(|t| t.as_str()).map(|s| s.to_string()))
                            .collect()
                    })
                    .unwrap_or_default();
                fns.push((name.to_string(), types));
            }
        }
    }
    fns
}

/// Fuzz one function: N runs with random args on a REAL EVM execution.
/// Reverts are failures (counterexample kept); everything else passes.
pub fn fuzz_function(
    workspace: &Path,
    contract: &str,
    function: &str,
    cfg: &FuzzConfig,
) -> Result<FuzzOutcome, String> {
    let output = compile::compile_workspace(workspace, None, false, "shanghai")?;
    let (types, artifact) = match abi_types_for(&output, contract, function) {
        Some((_, types)) => {
            let artifact = compile::find_artifact(&output.artifacts, contract)
                .cloned()
                .ok_or("artifact vanished")?;
            (types, artifact)
        }
        None => return Err(format!("function {contract}.{function} not found in ABI")),
    };

    let mut chain = fresh_chain();
    let outcome = deploy::deploy_local(workspace, &mut chain, &artifact, contract, &cfg.account, &[], &[], U256::zero())?;
    let mut rng = Rng::new(cfg.seed);
    let mut log = String::new();
    let mut reverts = 0u64;
    let mut gas = 0u64;
    log.push_str(&format!("deployed {contract} at {:?} (gas {})\n", outcome.address, outcome.gas_used));

    for run in 0..cfg.runs {
        let args: Vec<String> = types.iter().map(|t| gen_arg(&mut rng, t)).collect();
        let arg_refs: Vec<&str> = args.iter().map(|s| s.as_str()).collect();
        let type_refs: Vec<&str> = types.iter().map(|s| s.as_str()).collect();
        let call = deploy::call_write(
            &mut chain,
            &cfg.account,
            outcome.address,
            function,
            &type_refs,
            &arg_refs,
            U256::zero(),
        );
        match call {
            Ok((_, g)) => gas += g,
            Err(e) => {
                reverts += 1;
                // shrink once per failing arg for a minimal counterexample
                let shrunk: Vec<String> = types
                    .iter()
                    .zip(&args)
                    .map(|(t, a)| shrink_arg(t, a))
                    .collect();
                log.push_str(&format!(
                    "FAIL run #{} args=({}) → {}\nshrunk=({})\n",
                    run,
                    args.join(", "),
                    e,
                    shrunk.join(", ")
                ));
                return Ok(FuzzOutcome {
                    passed: false,
                    runs_executed: run + 1,
                    reverts,
                    gas_used: gas,
                    counterexample: Some((function.to_string(), shrunk)),
                    log,
                });
            }
        }
    }
    log.push_str(&format!(
        "PASSED {}/{} runs on real EVM (total gas {gas})\n",
        cfg.runs, cfg.runs
    ));
    Ok(FuzzOutcome {
        passed: true,
        runs_executed: cfg.runs,
        reverts,
        gas_used: gas,
        counterexample: None,
        log,
    })
}

/// Invariant testing: random call sequences across every state-changing
/// function, then the `invariant_<Contract>` (or given) view must hold.
pub fn fuzz_invariants(
    workspace: &Path,
    contract: &str,
    invariant_fn: Option<&str>,
    runs: u64,
    depth: u64,
    seed: u64,
) -> Result<FuzzOutcome, String> {
    let output = compile::compile_workspace(workspace, None, false, "shanghai")?;
    let artifact = compile::find_artifact(&output.artifacts, contract)
        .cloned()
        .ok_or_else(|| format!("contract {contract} not found"))?;
    let inv = invariant_fn
        .map(|s| s.to_string())
        .unwrap_or_else(|| format!("invariant_{contract}"));
    // invariant must exist as a view
    let mut inv_exists = false;
    if let Some(entries) = compile::abi_of(&artifact).as_array() {
        for e in entries {
            if e.get("name").and_then(|n| n.as_str()) == Some(inv.as_str()) {
                inv_exists = true;
            }
        }
    }
    if !inv_exists {
        return Err(format!(
            "invariant function `{inv}` not found; add `function {inv}() public view returns (bool)`"
        ));
    }
    let fns = selector_types(&output, contract);
    if fns.is_empty() {
        return Err("no state-changing functions to fuzz".into());
    }
    let mut chain = fresh_chain();
    let outcome = deploy::deploy_local(workspace, &mut chain, &artifact, contract, &AccountSpec::Dev(0), &[], &[], U256::zero())?;
    let mut rng = Rng::new(seed);
    let mut log = String::new();
    let mut gas = 0u64;
    log.push_str(&format!(
        "invariant harness: {contract} @ {:?}, {runs} sequences x {depth} calls\n",
        outcome.address
    ));

    for seq in 0..runs {
        for _ in 0..depth {
            let (fname, types) = rng.pick(&fns);
            let args: Vec<String> = types.iter().map(|t| gen_arg(&mut rng, t)).collect();
            let arg_refs: Vec<&str> = args.iter().map(|s| s.as_str()).collect();
            let type_refs: Vec<&str> = types.iter().map(|s| s.as_str()).collect();
            // ignore individual call reverts inside a sequence (e.g. overflow guards)
            if let Ok((_, g)) = deploy::call_write(
                &mut chain,
                &AccountSpec::Dev(1),
                outcome.address,
                fname,
                &type_refs,
                &arg_refs,
                U256::zero(),
            ) {
                gas += g;
            }
        }
        // check invariant
        let check = deploy::call_read(
            &chain,
            rrx_chain::dev::dev_address(0),
            outcome.address,
            &inv,
            &[],
            &[],
            &[],
        );
        match check {
            Ok(v) if v.trim() == "true" => {}
            Ok(v) => {
                log.push_str(&format!(
                    "INVARIANT VIOLATED in sequence #{seq}: {inv}() = {v}\n"
                ));
                return Ok(FuzzOutcome {
                    passed: false,
                    runs_executed: seq + 1,
                    reverts: 0,
                    gas_used: gas,
                    counterexample: Some((inv.clone(), vec![format!("after sequence {seq}")])),
                    log,
                });
            }
            Err(e) => {
                log.push_str(&format!("invariant call reverted: {e}\n"));
                return Ok(FuzzOutcome {
                    passed: false,
                    runs_executed: seq + 1,
                    reverts: 0,
                    gas_used: gas,
                    counterexample: Some((inv.clone(), vec![format!("revert in sequence {seq}")])),
                    log,
                });
            }
        }
    }
    log.push_str(&format!("PASSED all {runs} invariant sequences\n"));
    Ok(FuzzOutcome {
        passed: true,
        runs_executed: runs,
        reverts: 0,
        gas_used: gas,
        counterexample: None,
        log,
    })
}

fn fresh_chain() -> Chain {
    let alloc: Vec<_> = (0..10)
        .map(|i| {
            let kp = rrx_chain::dev::dev_keypair(i);
            (
                kp.address().unwrap(),
                U256::from_dec_str("10000000000000000000000").unwrap(),
            )
        })
        .collect();
    Chain::genesis(Default::default(), &alloc)
}
