//! Debugger: real step-level debugging of executed transactions using the EVM tracer
//! plus source maps from solc artifacts.

use rrx_chain::Chain;
use rrx_evm::{opcode_name, Host};
use rrx_util::U256;
use serde_json::{json, Value};

pub struct DebugOptions {
    pub max_steps: usize,
    pub breakpoint_line: Option<(String, usize)>,
    pub json: bool,
}

/// Re-execute a call locally with full tracing and pretty-print the run.
#[allow(clippy::too_many_arguments)]
pub fn debug_call(
    chain: &Chain,
    from: rrx_util::H160,
    to: rrx_util::H160,
    data: Vec<u8>,
    value: rrx_util::U256,
    opts: &DebugOptions,
    artifacts: &[(String, Value)],
) -> Result<String, String> {
    let res = chain.call(from, Some(to), value, data.clone(), chain.config.gas_limit, true);
    let trace = res.trace.as_ref().ok_or("no trace recorded")?;

    // pc -> (file, line) via source map + artifact
    let srcmap: Option<(String, Vec<(u32, u32, i32, u32, u32)>)> = artifacts
        .iter()
        .find(|(_, a)| {
            let deployed = a["evm"]["deployedBytecode"]["object"]
                .as_str()
                .unwrap_or("0x");
            let code = rrx_util::from_hex(deployed).unwrap_or_default();
            code.len() == chain.state.code(&to).len()
        })
        .and_then(|(k, a)| {
            let sm = a["evm"]["deployedBytecode"]["sourceMap"].as_str()?.to_string();
            Some((k.clone(), parse_source_map(&sm)))
        });

    let mut out = String::new();
    if opts.json {
        let steps: Vec<Value> = trace
            .steps
            .iter()
            .take(opts.max_steps)
            .map(|s| {
                json!({
                    "pc": s.pc, "opcode": opcode_name(s.opcode), "opByte": format!("0x{:02x}", s.opcode),
                    "depth": s.depth, "gasBefore": s.gas_before, "gasCost": s.gas_cost,
                    "error": s.error,
                    "source": map_pc(&srcmap, s.pc),
                })
            })
            .collect();
        return serde_json::to_string_pretty(&json!({
            "result": format!("{:?}", res.kind),
            "gasUsed": res.gas_used,
            "output": rrx_util::to_hex_prefixed(&res.output),
            "steps": steps,
        }))
        .map_err(|e| e.to_string());
    }

    out.push_str(&format!(
        "debug session — result: {:?}, gas used: {}, steps: {}\n",
        res.kind,
        res.gas_used,
        trace.steps.len()
    ));
    if let Some(reason) = res.revert_reason() {
        out.push_str(&format!("revert reason: {reason}\n"));
    }

    let mut shown = 0usize;
    for (i, s) in trace.steps.iter().enumerate() {
        if shown >= opts.max_steps {
            out.push_str(&format!("  ... {} more steps (use --max-steps)\n", trace.steps.len() - i));
            break;
        }
        if let Some((file, line)) = &opts.breakpoint_line {
            if let Some(src) = map_pc(&srcmap, s.pc) {
                if src.split(':').next() != Some(file.as_str())
                    || src.rsplit(':').next().and_then(|l| l.parse::<usize>().ok()) != Some(*line)
                {
                    continue;
                }
            }
        }
        let mut line = format!(
            "  #{:<5} depth={:<3} pc={:<6} gas={:<8} (cost {:>5}) {}",
            i, s.depth, s.pc, s.gas_before, s.gas_cost, opcode_name(s.opcode)
        );
        if let Some(src) = map_pc(&srcmap, s.pc) {
            line.push_str(&format!("   @ {src}"));
        }
        if let Some(err) = &s.error {
            line.push_str(&format!("   !! {err}"));
        }
        out.push_str(&line);
        out.push('\n');
        shown += 1;
    }
    Ok(out)
}

/// Parse a Solidity source map: "s:l:f:j:m;..." entries.
fn parse_source_map(sm: &str) -> Vec<(u32, u32, i32, u32, u32)> {
    let mut out = vec![];
    let mut prev = (0u32, 0u32, -1i32);
    for entry in sm.split(';') {
        let parts: Vec<&str> = entry.split(':').collect();
        let s = parts.first().and_then(|x| x.parse().ok());
        let l = parts.get(1).and_then(|x| x.parse().ok());
        let f = parts.get(2).and_then(|x| x.parse::<i32>().ok());
        let s = s.unwrap_or(prev.0);
        let l = l.unwrap_or(prev.1);
        let f = f.unwrap_or(prev.2);
        out.push((s, l, f, 0, 0));
        prev = (s, l, f);
    }
    out
}

fn map_pc(srcmap: &Option<(String, Vec<(u32, u32, i32, u32, u32)>)>, pc: usize) -> Option<String> {
    let (label, entries) = srcmap.as_ref()?;
    let (s, l, _f, _, _) = *entries.get(pc)?;
    if l == 0 {
        return None;
    }
    Some(format!("{label}:{s}+{l}"))
}

/// Show stack state at step N (headless watch).
pub fn step_stack(trace_json: &str, n: usize) -> Result<String, String> {
    let v: Value = serde_json::from_str(trace_json).map_err(|e| e.to_string())?;
    let step = v["steps"]
        .get(n)
        .ok_or("step out of range")?;
    Ok(step.to_string())
}

/// helper used by CLI to format a U256 stack item
pub fn fmt_word(v: &U256) -> String {
    if *v > U256::from(0xffff_ffffu64) {
        rrx_util::to_hex_prefixed(&rrx_util::u256_to_be32(v))
    } else {
        v.to_string()
    }
}
