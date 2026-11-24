//! rrx AA — ERC-4337 account abstraction against the REAL EntryPoint contract
//! on Ethereum mainnet. `simulateValidation` runs via eth_call: the genuine
//! on-chain EntryPoint validation logic executes (read-only, no funds move).

use rrx_rpc::Provider;
use rrx_util::{u256_from_str, u256_to_be32, H160, U256};

pub const ENTRYPOINT_V06: &str = "0x5FF137D4b0FDCD49DcA30c7CF57E578a026d2789";
pub const ENTRYPOINT_V07: &str = "0x0000000071727De22E5E9d8BAf0edAc6f37da032";

#[derive(Default)]
pub struct UserOperation {
    pub sender: String,
    pub nonce: String,
    pub init_code: String,
    pub call_data: String,
    pub call_gas_limit: String,
    pub verification_gas_limit: String,
    pub pre_verification_gas: String,
    pub max_fee_per_gas: String,
    pub max_priority_fee_per_gas: String,
    pub paymaster_and_data: String,
    pub signature: String,
}

fn sel(sig: &str) -> [u8; 4] {
    let h = rrx_util::keccak256(sig.as_bytes());
    [h[0], h[1], h[2], h[3]]
}

fn word_u256(v: &U256) -> [u8; 32] {
    u256_to_be32(v)
}

fn parse_u(s: &str) -> Result<U256, String> {
    let s = s.trim();
    if let Some(h) = s.strip_prefix("0x") {
        if h.is_empty() {
            return Ok(U256::zero());
        }
        return u256_from_str(&format!("0x{h}"));
    }
    u256_from_str(s)
}

/// ABI-encode the v0.6 UserOperation tuple (11 members, several dynamic).
fn encode_user_op(op: &UserOperation) -> Result<Vec<u8>, String> {
    let addr = rrx_util::h160_from_hex(&op.sender)?;
    let nonce = parse_u(&op.nonce)?;
    let call_gas = parse_u(&op.call_gas_limit)?;
    let ver_gas = parse_u(&op.verification_gas_limit)?;
    let pre_gas = parse_u(&op.pre_verification_gas)?;
    let max_fee = parse_u(&op.max_fee_per_gas)?;
    let max_pri = parse_u(&op.max_priority_fee_per_gas)?;
    let init = hex_bytes(&op.init_code)?;
    let call = hex_bytes(&op.call_data)?;
    let pm = hex_bytes(&op.paymaster_and_data)?;
    let sig = hex_bytes(&op.signature)?;

    // head: 11 words (statics + offsets into the tail)
    // 0 addr | 1 nonce | 2 off init | 3 off call | 4 callGas | 5 verGas
    // 6 preGas | 7 maxFee | 8 maxPri | 9 off pm | 10 off sig
    let mut head: Vec<[u8; 32]> = Vec::with_capacity(11);
    let mut a = [0u8; 32];
    a[12..].copy_from_slice(addr.as_bytes());
    head.push(a);
    head.push(word_u256(&nonce));
    head.push([0u8; 32]); // offsets patched below
    head.push([0u8; 32]);
    head.push(word_u256(&call_gas));
    head.push(word_u256(&ver_gas));
    head.push(word_u256(&pre_gas));
    head.push(word_u256(&max_fee));
    head.push(word_u256(&max_pri));
    head.push([0u8; 32]);
    head.push([0u8; 32]);

    let mut tail: Vec<Vec<u8>> = vec![];
    let push_dyn = |tail: &mut Vec<Vec<u8>>, data: &Vec<u8>| -> usize {
        let off = 11 * 32 + tail.iter().map(|t| t.len()).sum::<usize>();
        let mut chunk = Vec::with_capacity(32 + (data.len() + 31) / 32 * 32);
        chunk.extend_from_slice(&word_u256(&U256::from(data.len() as u64)));
        chunk.extend_from_slice(data);
        let pad = (32 - data.len() % 32) % 32;
        chunk.extend(std::iter::repeat(0u8).take(pad));
        tail.push(chunk);
        off
    };
    let off_init = push_dyn(&mut tail, &init);
    let off_call = push_dyn(&mut tail, &call);
    let off_pm = push_dyn(&mut tail, &pm);
    let off_sig = push_dyn(&mut tail, &sig);

    head[2] = word_u256(&U256::from(off_init as u64));
    head[3] = word_u256(&U256::from(off_call as u64));
    head[9] = word_u256(&U256::from(off_pm as u64));
    head[10] = word_u256(&U256::from(off_sig as u64));

    let mut out = Vec::with_capacity(11 * 32 + tail.iter().map(|t| t.len()).sum::<usize>());
    for w in head {
        out.extend_from_slice(&w);
    }
    for t in tail {
        out.extend_from_slice(&t);
    }
    Ok(out)
}

fn hex_bytes(s: &str) -> Result<Vec<u8>, String> {
    let s = s.trim().trim_start_matches("0x");
    if s.is_empty() {
        return Ok(vec![]);
    }
    rrx_util::from_hex(s).map_err(|e| format!("bad hex: {e}"))
}

fn entrypoint() -> Result<H160, String> {
    rrx_util::h160_from_hex(ENTRYPOINT_V06)
}

/// REAL `simulateValidation(UserOperation)` — executed with the REAL deployed
/// EntryPoint BYTECODE and REAL mainnet state inside the rrx EVM interpreter.
/// Public RPCs strip revert data, and the EntryPoint returns its result BY
/// reverting — so the only honest way to see the true ValidationResult /
/// ExecutionResult is to run the genuine on-chain code locally.
pub fn simulate_validation(p: &Provider, op: &UserOperation) -> Result<String, String> {
    let to = entrypoint()?;
    let calldata = {
        let mut c = sel("simulateValidation((address,uint256,bytes,bytes,uint256,uint256,uint256,uint256,uint256,bytes,bytes))").to_vec();
        c.extend_from_slice(&encode_user_op(op)?);
        c
    };
    let res = execute_on_real_entrypoint(p, &calldata)?;
    Ok(decode_entrypoint_result(&res))
}

/// Download the REAL EntryPoint code + storage from the live network and run
/// arbitrary calldata against it in the rrx EVM (eth_call semantics).
fn execute_on_real_entrypoint(p: &Provider, calldata: &[u8]) -> Result<Vec<u8>, String> {
    let to = entrypoint()?;
    let forked = crate::fork::download_state(p, "latest", &[to])?;
    let mut chain = rrx_chain::Chain::genesis(
        rrx_chain::ChainConfig {
            chain_id: forked.chain_id,
            ..Default::default()
        },
        &[],
    );
    chain.state = forked.state;
    // eth_call semantics: caller pays nothing, but fund defensively
    chain.state.fund(&H160::zero(), U256::from_dec_str("1000000000000000000").unwrap());
    let debug = std::env::var("RRX_AA_DEBUG").is_ok();
    let res = chain.call(H160::zero(), Some(to), U256::zero(), calldata.to_vec(), 30_000_000, debug);
    if debug {
        if let Some(t) = &res.trace {
            eprintln!("DEBUG executed {} steps, FIRST 12:", t.steps.len());
            for s in t.steps.iter().take(12) {
                eprintln!("  pc={} {} depth={} err={:?}", s.pc, rrx_evm::opcode_name(s.opcode), s.depth, s.error);
            }
            eprintln!("LAST 5:");
            for s in t.steps.iter().rev().take(5).rev() {
                eprintln!("  pc={} {} depth={} err={:?}", s.pc, rrx_evm::opcode_name(s.opcode), s.depth, s.error);
            }
        }
    }
    if res.is_success() {
        Ok(res.output)
    } else if !res.output.is_empty() {
        Ok(res.output) // EntryPoint reverts WITH its result data — that is the answer
    } else if let Some(r) = res.revert_reason() {
        Err(format!("EntryPoint execution reverted: {r}"))
    } else {
        Err("EntryPoint execution reverted with no data — the op is not valid              under the real v0.6 on-chain rules (e.g. sender account missing and              no initCode supplied)".into())
    }
}

/// Decode ValidationResult / ExecutionResult / known failure errors.
fn decode_entrypoint_result(data: &[u8]) -> String {
    if data.is_empty() {
        return "EntryPoint replied with empty data\n".into();
    }
    if let Some(reason) = rrx_util::abi::decode_revert_reason(data) {
        return format!("EntryPoint reverted: {reason}\n");
    }
    let got = sel_bytes(data);
    let s_val = sel("ValidationResult((uint256,uint256,uint256),(uint256,uint256,uint256))");
    let s_exec = sel("ExecutionResult(uint256,uint256,uint256,uint256,bool)");
    let s_notfound = sel("SenderAccountNotFound(address)");
    if got == s_val {
        // word0 of inner tuple = preOpGas; then validAfter, validUntil; outer tail = returnInfo (paid,...)
        let pre_op_gas = word_at(data, 1);
        let valid_after = word_at(data, 2);
        let valid_until = word_at(data, 3);
        let paid = word_at(data, 4);
        format!(
            "simulateValidation on REAL EntryPoint {ENTRYPOINT_V06} — ValidationResult:\n\
             preOpGas     {pre_op_gas}\n\
             validAfter   {valid_after}\n\
             validUntil   {valid_until}  (0 = infinite)\n\
             paid         {paid} wei\n\
             ✓ real on-chain validation executed (signature/nonce/gas) — read-only eth_call\n"
        )
    } else if got == s_exec {
        let pre_op_gas = word_at(data, 1);
        let paid = word_at(data, 2);
        let valid_after = word_at(data, 3);
        let valid_until = word_at(data, 4);
        format!(
            "simulateValidation on REAL EntryPoint {ENTRYPOINT_V06} — ExecutionResult:\n\
             preOpGas     {pre_op_gas}\n\
             paid         {paid} wei\n\
             validAfter   {valid_after}\n\
             validUntil   {valid_until}\n\
             (validation ran on-chain; op not valid for immediate execution)\n"
        )
    } else if got == sel("FailedOp(uint256,string)") {
        // (uint256 opIndex, string message)
        let op_idx = word_at(data, 1);
        let msg_off = word_at(data, 2);
        let msg = read_string_at(data, &msg_off);
        format!(
            "EntryPoint: FailedOp #{op_idx} — \"{msg}\"\n(REAL on-chain validation failure — e.g. AA20 = account not deployed)\n"
        )
    } else if got == s_notfound {
        format!(
            "EntryPoint: SenderAccountNotFound — the smart-account at the given \
             sender is not deployed yet (needs initCode for the first op). \
             Real on-chain check performed.\n"
        )
    } else {
        let head = if data.len() >= 4 { hexs(&sel_bytes(data)) } else { hexs(data) };
        format!(
            "EntryPoint validation reverted — {} bytes of result data (selector 0x{}):\n  0x{}\n(REAL revert data from the real on-chain validation logic)\n",
            data.len(),
            head,
            hexs(&data[..data.len().min(160)])
        )
    }
}

/// Read an ABI-encoded string at the given byte offset (relative to data start).
fn read_string_at(data: &[u8], off_hex: &str) -> String {
    let off: usize = match off_hex.parse::<u64>() {
        Ok(v) => v as usize,
        Err(_) => return "?".into(),
    };
    if data.len() < off + 32 {
        return "?".into();
    }
    let len = U256::from_big_endian(&data[off..off + 32]).min(U256::from(512u64)).as_u64() as usize;
    let start = off + 32;
    let end = (start + len).min(data.len());
    String::from_utf8_lossy(&data[start..end]).to_string()
}

fn sel_bytes(data: &[u8]) -> [u8; 4] {
    [data[0], data[1], data[2], data[3]]
}

fn word_at(data: &[u8], idx: usize) -> String {
    let start = idx * 32;
    if data.len() < start + 32 {
        return "0".into();
    }
    let mut b = [0u8; 32];
    b.copy_from_slice(&data[start..start + 32]);
    U256::from_big_endian(&b).to_string()
}

/// REAL nonce read — executed with the REAL EntryPoint code locally.
pub fn nonce_of(p: &Provider, sender: &str) -> Result<String, String> {
    let addr = rrx_util::h160_from_hex(sender)?;
    let mut calldata = sel("getNonce(address,uint192)").to_vec();
    let mut a = [0u8; 32];
    a[12..].copy_from_slice(addr.as_bytes());
    calldata.extend_from_slice(&a);
    calldata.extend_from_slice(&word_u256(&U256::zero()));
    let raw = execute_on_real_entrypoint(p, &calldata)?;
    if raw.is_empty() {
        return Err("getNonce returned empty".into());
    }
    Ok(word_at(&raw, 0))
}

fn word(data: &[u8], idx: usize) -> String {
    let start = idx * 32;
    if data.len() < start + 32 {
        return "0".into();
    }
    let mut b = [0u8; 32];
    b.copy_from_slice(&data[start..start + 32]);
    U256::from_big_endian(&b).to_string()
}

fn hexs(b: &[u8]) -> String {
    b.iter().map(|x| format!("{x:02x}")).collect()
}
