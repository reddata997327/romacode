//! Script Runner: a small deterministic DSL that drives the REAL chain.
//!
//! Example (.rrx script):
//! ```text
//! # deploy the token and check a transfer
//! compile .
//! account 0
//! deploy RrxToken --args "Rrx Token,RRX,1000000000000000000000000" -> token
//! send token.transfer(0xdead000000000000000000000000000000000001, 1000)
//! assert-eq call token.balanceOf(0xdead000000000000000000000000000000000001) 1000
//! expect-revert send token.transfer(0xdead000000000000000000000000000000000001, 999999999)
//! gas report
//! ```

use std::path::Path;

use rrx_chain::dev::dev_keypair;
use rrx_chain::{Chain, ChainConfig};
use rrx_util::{abi, H160, U256};

pub struct ScriptEnv {
    pub chain: Chain,
    pub account: usize,
    pub artifacts: Vec<(String, serde_json::Value)>,
    pub named: std::collections::BTreeMap<String, (H160, String)>,
    pub gas_records: Vec<serde_json::Value>,
    pub out: Vec<String>,
}

pub fn run_script(root: &Path, path: &Path) -> Result<String, String> {
    let script = std::fs::read_to_string(path).map_err(|e| e.to_string())?;
    let mut env = ScriptEnv {
        chain: Chain::genesis(
            ChainConfig::default(),
            &(0..10)
                .map(|i| {
                    let kp = dev_keypair(i);
                    (kp.address().unwrap(), U256::from_dec_str("10000000000000000000000").unwrap())
                })
                .collect::<Vec<_>>(),
        ),
        account: 0,
        artifacts: vec![],
        named: Default::default(),
        gas_records: vec![],
        out: vec![],
    };

    for (lineno, raw) in script.lines().enumerate() {
        let line = raw.trim();
        if line.is_empty() || line.starts_with('#') || line.starts_with("//") {
            continue;
        }
        if let Err(e) = exec_line(root, &mut env, line) {
            return Err(format!("script line {}: {e}\n> {line}", lineno + 1));
        }
    }

    // persist gas records for the gas report tool
    let gas_path = root.join(".rrx").join("gas.json");
    let mut existing: Vec<serde_json::Value> = std::fs::read_to_string(&gas_path)
        .ok()
        .and_then(|s| serde_json::from_str(&s).ok())
        .unwrap_or_default();
    existing.extend(env.gas_records.iter().cloned());
    if let Some(p) = gas_path.parent() {
        std::fs::create_dir_all(p).map_err(|e| e.to_string())?;
    }
    std::fs::write(&gas_path, serde_json::to_string_pretty(&existing).map_err(|e| e.to_string())?)
        .map_err(|e| e.to_string())?;

    Ok(env.out.join("\n"))
}

fn exec_line(root: &Path, env: &mut ScriptEnv, line: &str) -> Result<(), String> {
    let (cmd, rest) = line.split_once(' ').unwrap_or((line, ""));

    // handle `... -> name` targets
    let (rest, target) = match rest.split_once("->") {
        Some((a, b)) => (a.trim(), Some(b.trim().to_string())),
        None => (rest, None),
    };

    match cmd {
        "compile" => {
            let out = crate::compile::compile_workspace(root, None, true, "shanghai")?;
            env.artifacts = out.artifacts;
            env.out.push(format!("compiled {} contract(s) with solc {}", env.artifacts.len(), out.solc_version));
        }
        "account" => {
            env.account = rest.trim().parse().map_err(|_| "account index must be a number")?;
            env.out.push(format!("account = dev#{}", env.account));
        }
        "deploy" => {
            let mut parts = rest.splitn(2, "--args");
            let name = parts.next().unwrap_or("").trim();
            let artifact = crate::compile::find_artifact(&env.artifacts, name)
                .ok_or_else(|| format!("artifact '{name}' not found (compile first)"))?
                .clone();
            let (args, types) = match parts.next() {
                Some(argstr) => {
                    let mut a = argstr.trim();
                    if a.starts_with('"') && a.ends_with('"') && a.len() >= 2 {
                        a = &a[1..a.len() - 1];
                    }
                    let vals = split_args(a);
                    let types = ctor_types(&artifact, vals.len())?;
                    (vals, types)
                }
                None => (vec![], vec![]),
            };
            let arg_refs: Vec<&str> = args.iter().map(|s| s.as_str()).collect();
            let type_refs: Vec<&str> = types.iter().map(|s| s.as_str()).collect();
            let acct = rrx_tools_account(env);
            let outcome = crate::deploy::deploy_local(
                root,
                &mut env.chain,
                &artifact,
                name,
                &acct,
                &arg_refs,
                &type_refs,
                U256::zero(),
            )?;
            env.gas_records.push(serde_json::json!({
                "contract": name, "method": "constructor", "kind": "deploy", "gas": outcome.gas_used
            }));
            if let Some(t) = target {
                env.named.insert(t, (outcome.address, name.to_string()));
            }
            let _ = crate::deploy::save_deployment(root, &outcome, &crate::compile::abi_of(&artifact), &args, &types);
            env.out.push(format!(
                "deployed {name} at 0x{:x} (gas {})",
                outcome.address, outcome.gas_used
            ));
        }
        "call" | "send" => {
            let expr = rest.trim();
            let (obj, call) = expr
                .split_once('.')
                .ok_or("expected name.method(args)")?;
            let (method, argstr) = split_call(call)?;
            let addr = resolve_name(env, obj)?;
            let artifact = artifact_for_alias(env, obj)?.clone();
            let (types, out_types) = fn_signature(&artifact, &method, argstr)?;
            let arg_vals = split_args(argstr);
            let arg_refs: Vec<&str> = arg_vals.iter().map(|s| s.as_str()).collect();
            let type_refs: Vec<&str> = types.iter().map(|s| s.as_str()).collect();
            if cmd == "call" {
                let from = dev_keypair(env.account).address().unwrap();
                let out_refs: Vec<&str> = out_types.iter().map(|s| s.as_str()).collect();
                let res = crate::deploy::call_read(&env.chain, from, addr, &method, &type_refs, &arg_refs, &out_refs)?;
                env.out.push(format!("{}.{}() = {}", obj, method, res));
            } else {
                let acct = rrx_tools_account(env);
                let (hash, gas) = crate::deploy::call_write(
                    &mut env.chain,
                    &acct,
                    addr,
                    &method,
                    &type_refs,
                    &arg_refs,
                    U256::zero(),
                )?;
                let cname = env.named.get(obj).map(|(_, c)| c.clone()).unwrap_or_else(|| obj.to_string());
                env.gas_records.push(serde_json::json!({
                    "contract": cname, "method": method, "kind": "call", "gas": gas
                }));
                env.out.push(format!("{}.{}() mined 0x{:x} gas={}", obj, method, hash, gas));
            }
        }
        "assert-eq" => {
            let mut rest = rest.trim();
            if let Some(inner) = rest.strip_prefix("call ") {
                rest = inner.trim();
            }
            let mut parts = rest.splitn(2, char::is_whitespace);
            let lhs = parts.next().unwrap_or("").trim();
            let rhs = parts.next().unwrap_or("").trim();
            let actual = eval_expr(env, lhs)?;
            let expected = rhs.trim_matches('"');
            if actual != expected {
                return Err(format!("assert-eq failed: {lhs} = {actual}, expected {expected}"));
            }
            env.out.push(format!("assert-eq ok: {} == {}", lhs, expected));
        }
        "expect-revert" => {
            let inner = rest.trim();
            let (cmd2, rest2) = inner.split_once(' ').ok_or("expected: expect-revert send ...")?;
            if cmd2 != "send" {
                return Err("expect-revert only supports send".into());
            }
            let (obj, call) = rest2.trim().split_once('.').ok_or("expected name.method(args)")?;
            let (method, argstr) = split_call(call)?;
            let addr = resolve_name(env, obj)?;
            let artifact = artifact_for_alias(env, obj)?.clone();
            let (types, _) = fn_signature(&artifact, &method, argstr)?;
            let arg_vals = split_args(argstr);
            let arg_refs: Vec<&str> = arg_vals.iter().map(|s| s.as_str()).collect();
            let type_refs: Vec<&str> = types.iter().map(|s| s.as_str()).collect();
            let acct = rrx_tools_account(env);
            let res = crate::deploy::call_write(
                &mut env.chain,
                &acct,
                addr,
                &method,
                &type_refs,
                &arg_refs,
                U256::zero(),
            );
            match res {
                Err(_) => env.out.push(format!("expect-revert ok: {}.{} reverted", obj, method)),
                Ok(_) => return Err(format!("expect-revert failed: {}.{} succeeded", obj, method)),
            }
        }
        "print" => {
            let v = eval_expr(env, rest.trim())?;
            env.out.push(v);
        }
        "gas" => {
            if rest.trim() == "report" {
                env.out.push(crate::gasfee::report_from_records(&env.gas_records, "table"));
            } else {
                return Err("usage: gas report".into());
            }
        }
        other => return Err(format!("unknown command '{other}'")),
    }
    Ok(())
}

fn rrx_tools_account(env: &ScriptEnv) -> crate::deploy::AccountSpec {
    crate::deploy::AccountSpec::Dev(env.account)
}

fn resolve_name(env: &ScriptEnv, name: &str) -> Result<H160, String> {
    if let Some((a, _)) = env.named.get(name) {
        return Ok(*a);
    }
    rrx_util::h160_from_hex(name)
}

fn artifact_for_alias<'a>(
    env: &'a ScriptEnv,
    alias: &str,
) -> Result<&'a serde_json::Value, String> {
    if let Some((_, contract)) = env.named.get(alias) {
        if let Some(art) = crate::compile::find_artifact(&env.artifacts, contract) {
            return Ok(art);
        }
    }
    Err(format!("no compiled artifact for alias '{alias}'"))
}

/// (inputTypes, outputTypes) for a function; if unknown signature, infer basic ones from args.
fn fn_signature(
    artifact: &serde_json::Value,
    method: &str,
    argstr: &str,
) -> Result<(Vec<String>, Vec<String>), String> {
    let abi = artifact.get("abi").and_then(|a| a.as_array()).ok_or("artifact has no abi")?;
    for entry in abi {
        if entry["type"] == "function" && entry["name"] == method {
            let types: Vec<String> = entry["inputs"]
                .as_array()
                .map(|a| a.iter().map(|i| i["type"].as_str().unwrap_or("uint256").to_string()).collect())
                .unwrap_or_default();
            let outs: Vec<String> = entry["outputs"]
                .as_array()
                .map(|a| a.iter().map(|i| i["type"].as_str().unwrap_or("uint256").to_string()).collect())
                .unwrap_or_default();
            let given = split_args(argstr).len();
            if types.len() != given {
                return Err(format!(
                    "{method} expects {} args ({}), got {given}",
                    types.len(),
                    types.join(",")
                ));
            }
            return Ok((types, outs));
        }
    }
    Err(format!("function '{method}' not in ABI"))
}

fn ctor_types(artifact: &serde_json::Value, given: usize) -> Result<Vec<String>, String> {
    let abi = artifact.get("abi").and_then(|a| a.as_array()).ok_or("artifact has no abi")?;
    for entry in abi {
        if entry["type"] == "constructor" {
            let types: Vec<String> = entry["inputs"]
                .as_array()
                .map(|a| a.iter().map(|i| i["type"].as_str().unwrap_or("uint256").to_string()).collect())
                .unwrap_or_default();
            if types.len() != given {
                return Err(format!("constructor expects {} args ({})", types.len(), types.join(",")));
            }
            return Ok(types);
        }
    }
    if given == 0 {
        Ok(vec![])
    } else {
        Err("constructor takes no args".into())
    }
}

fn split_args(s: &str) -> Vec<String> {
    let s = s.trim();
    let s = s.strip_prefix('(').unwrap_or(s);
    let s = s.strip_suffix(')').unwrap_or(s);
    let mut out = vec![];
    let mut cur = String::new();
    let mut in_q = false;
    let mut depth = 0;
    for c in s.chars() {
        match c {
            '"' => {
                in_q = !in_q;
            }
            ',' if !in_q && depth == 0 => {
                out.push(cur.trim().to_string());
                cur.clear();
            }
            '[' | '(' if !in_q => {
                depth += 1;
                cur.push(c);
            }
            ']' | ')' if !in_q => {
                depth -= 1;
                cur.push(c);
            }
            _ => cur.push(c),
        }
    }
    if !cur.trim().is_empty() {
        out.push(cur.trim().trim_matches('"').to_string());
    } else if out.is_empty() && s.trim().is_empty() {
        // no args
    }
    out.into_iter()
        .filter(|s| !s.is_empty())
        .map(|s| s.trim_matches('"').to_string())
        .collect()
}

fn split_call(call: &str) -> Result<(String, &str), String> {
    let open = call.find('(').ok_or("expected method(args)")?;
    let method = call[..open].trim().to_string();
    let close = call.rfind(')').ok_or("missing closing paren")?;
    Ok((method, &call[open + 1..close]))
}

/// Evaluate `name.method(args)` or `balance(0x..|name)`.
fn eval_expr(env: &mut ScriptEnv, expr: &str) -> Result<String, String> {
    if let Some(inner) = expr.strip_prefix("balance(").and_then(|s| s.strip_suffix(')')) {
        let who = resolve_name(env, inner.trim())?;
        use rrx_evm::Host;
        return Ok(env.chain.state.balance(&who).to_string());
    }
    let (obj, call) = expr.split_once('.').ok_or("unsupported expression")?;
    let (method, argstr) = split_call(call)?;
    let addr = resolve_name(env, obj)?;
    let artifact = artifact_for_alias(env, obj)?.clone();
    let (types, outs) = fn_signature(&artifact, &method, argstr)?;
    let arg_vals = split_args(argstr);
    let arg_refs: Vec<&str> = arg_vals.iter().map(|s| s.as_str()).collect();
    let type_refs: Vec<&str> = types.iter().map(|s| s.as_str()).collect();
    let out_refs: Vec<&str> = outs.iter().map(|s| s.as_str()).collect();
    let from = dev_keypair(env.account).address().unwrap();
    crate::deploy::call_read(&env.chain, from, addr, &method, &type_refs, &arg_refs, &out_refs)
}

/// ABI selector of a function from types (re-exported for completeness)
pub fn selector_of(name: &str, types: &[String]) -> [u8; 4] {
    let refs: Vec<&str> = types.iter().map(|s| s.as_str()).collect();
    abi::selector(name, &refs)
}
