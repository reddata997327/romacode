//! The EVM interpreter: frame execution, full opcode set, real gas metering, tracing.

use std::collections::{BTreeMap, HashSet};

use rrx_precompiles as pre;
use primitive_types::U512;
use rrx_util::{keccak256, H160, H256, U256};

use crate::{
    create2_address, create_address, BlockEnv, EvmError, ExecutionResult, ExitKind, FrameResult,
    Host, Log, Spec, TxEnv,
};

// ---------------------------------------------------------------- gas constants

const GAS_STIPEND: u64 = 2300;
const GAS_VALUE_TRANSFER: u64 = 9000;
const GAS_NEW_ACCOUNT: u64 = 25000;
const GAS_CREATE: u64 = 32000;
const GAS_CODE_DEPOSIT: u64 = 200;
const GAS_SSTORE_SET: u64 = 20000;
const GAS_SELFDESTRUCT_REFUND: u64 = 24000;
const GAS_REFUND_CLEAR_ISTANBUL: i64 = 15000;
const GAS_REFUND_CLEAR_LONDON: i64 = 4800;
const GAS_COLD_SLOAD: u64 = 2100;
const GAS_COLD_ACCOUNT: u64 = 2600;
const GAS_WARM: u64 = 100;
const MAX_STACK: usize = 1024;
const MAX_CODE_SIZE: usize = 24576;
const MAX_INIT_CODE: usize = 49152;
const MAX_CALL_DEPTH: usize = 1024;

pub fn words(n: usize) -> u64 {
    (n as u64).div_ceil(32)
}

fn memory_words_cost(w: u64) -> u64 {
    match (w.checked_mul(3), w.checked_mul(w).and_then(|q| q.checked_div(512))) {
        (Some(a), Some(b)) => a.checked_add(b),
        _ => None,
    }
    .unwrap_or(u64::MAX)
}

fn expansion_cost(current_len: usize, needed_len: usize) -> u64 {
    let cur_w = words(current_len);
    let new_w = words(needed_len);
    if new_w <= cur_w {
        0
    } else {
        memory_words_cost(new_w).saturating_sub(memory_words_cost(cur_w))
    }
}

/// Full transaction intrinsic gas: 21000 + calldata (4/16) + create (32000 + EIP-3860)
/// + access list (EIP-2930).
pub fn tx_intrinsic_gas(tx: &TxEnv, spec: &Spec) -> u64 {
    let mut g: u64 = 21000;
    for b in &tx.data {
        match g.checked_add(if *b == 0 { 4 } else { 16 }) {
            Some(v) => g = v,
            None => return u64::MAX,
        }
    }
    if tx.to.is_none() {
        match g.checked_add(GAS_CREATE) {
            Some(v) => g = v,
            None => return u64::MAX,
        }
        if spec.eip3860 {
            match g.checked_add(2 * words(tx.data.len())) {
                Some(v) => g = v,
                None => return u64::MAX,
            }
        }
    }
    for (_, keys) in &tx.access_list {
        match g.checked_add(2400 + 1900u64.saturating_mul(keys.len() as u64)) {
            Some(v) => g = v,
            None => return u64::MAX,
        }
    }
    g
}

// ---------------------------------------------------------------- arithmetic helpers

fn sdiv(a: U256, b: U256) -> U256 {
    if b.is_zero() {
        return U256::zero();
    }
    let abs_a = if a.bit(255) { (!a).overflowing_add(U256::one()).0 } else { a };
    let abs_b = if b.bit(255) { (!b).overflowing_add(U256::one()).0 } else { b };
    let q = abs_a / abs_b;
    if a.bit(255) != b.bit(255) {
        (!q).overflowing_add(U256::one()).0
    } else {
        q
    }
}

fn smod(a: U256, b: U256) -> U256 {
    if b.is_zero() {
        return U256::zero();
    }
    let abs_a = if a.bit(255) { (!a).overflowing_add(U256::one()).0 } else { a };
    let abs_b = if b.bit(255) { (!b).overflowing_add(U256::one()).0 } else { b };
    let r = abs_a % abs_b;
    if a.bit(255) && !r.is_zero() {
        (!r).overflowing_add(U256::one()).0
    } else {
        r
    }
}

fn slt(a: &U256, b: &U256) -> bool {
    match (a.bit(255), b.bit(255)) {
        (true, false) => true,
        (false, true) => false,
        _ => a < b,
    }
}

fn sgt(a: &U256, b: &U256) -> bool {
    slt(b, a)
}

fn shl(shift: U256, x: U256) -> U256 {
    if shift >= U256::from(256u64) {
        U256::zero()
    } else {
        x << shift.as_usize()
    }
}

fn shr(shift: U256, x: U256) -> U256 {
    if shift >= U256::from(256u64) {
        U256::zero()
    } else {
        x >> shift.as_usize()
    }
}

fn sar(shift: U256, x: U256) -> U256 {
    let neg = x.bit(255);
    if shift >= U256::from(256u64) {
        return if neg { U256::MAX } else { U256::zero() };
    }
    let s = shift.as_usize();
    let logical = x >> s;
    if neg {
        logical | (U256::MAX << (256 - s))
    } else {
        logical
    }
}

fn byte_op(i: U256, x: U256) -> U256 {
    if i >= U256::from(32u8) {
        return U256::zero();
    }
    let be = rrx_util::u256_to_be32(&x);
    U256::from(be[31 - i.as_usize()])
}

fn signextend(k: U256, x: U256) -> U256 {
    if k >= U256::from(31u8) {
        return x;
    }
    let kb = k.as_usize();
    let be = rrx_util::u256_to_be32(&x);
    let fill: u8 = if be[31 - kb] & 0x80 != 0 { 0xff } else { 0x00 };
    let mut out = [0u8; 32];
    for (i, b) in out.iter_mut().enumerate() {
        *b = if i < 31 - kb { fill } else { be[i] };
    }
    U256::from_big_endian(&out)
}

fn bool_u256(b: bool) -> U256 {
    if b {
        U256::one()
    } else {
        U256::zero()
    }
}

fn addmod(a: U256, b: U256, m: U256) -> U256 {
    if m.is_zero() {
        return U256::zero();
    }
    let big = U512::from_big_endian(&rrx_util::u256_to_be32(&a))
        + U512::from_big_endian(&rrx_util::u256_to_be32(&b));
    let mb = U512::from_big_endian(&rrx_util::u256_to_be32(&m));
    let rem = (big % mb).to_big_endian();
    U256::from_big_endian(&rem[32..])
}

fn mulmod(a: U256, b: U256, m: U256) -> U256 {
    if m.is_zero() {
        return U256::zero();
    }
    let big = U512::from_big_endian(&rrx_util::u256_to_be32(&a))
        * U512::from_big_endian(&rrx_util::u256_to_be32(&b));
    let mb = U512::from_big_endian(&rrx_util::u256_to_be32(&m));
    let rem = (big % mb).to_big_endian();
    U256::from_big_endian(&rem[32..])
}

/// EIP-4844 fake_exponential(1, excess_blob_gas, 3338477) for the blob base fee.
fn blob_base_fee(excess_blob_gas: u64) -> U256 {
    let numerator = U256::from(excess_blob_gas);
    let denominator = U256::from(3338477u64);
    let mut i: u64 = 1;
    let mut output = U256::zero();
    let mut accum = U256::one() * denominator;
    // Exact for any protocol-reachable excess; saturating math plus a generous
    // iteration cap keep adversarial inputs deterministic instead of overflowing.
    while !accum.is_zero() && i <= 1024 {
        output += accum / denominator;
        accum = accum.saturating_mul(numerator) / (denominator * U256::from(i));
        i += 1;
    }
    output
}

/// Valid JUMPDEST positions (skipping PUSH data).
pub fn jumpdest_analysis(code: &[u8]) -> HashSet<usize> {
    let mut out = HashSet::new();
    let mut i = 0;
    while i < code.len() {
        let op = code[i];
        if op == 0x5b {
            out.insert(i);
        }
        if (0x60..=0x7f).contains(&op) {
            i += (op - 0x5f) as usize;
        }
        i += 1;
    }
    out
}

// ---------------------------------------------------------------- tracer

#[derive(Clone, Debug)]
pub struct Step {
    pub pc: usize,
    pub opcode: u8,
    pub depth: usize,
    pub gas_before: u64,
    pub gas_cost: u64,
    pub stack: Option<Vec<U256>>,
    pub memory_len: usize,
    pub memory: Option<Vec<u8>>,
    pub error: Option<String>,
}

#[derive(Clone, Debug, Default)]
pub struct Tracer {
    pub steps: Vec<Step>,
    pub gas_by_opcode: BTreeMap<u8, u128>,
    pub total_gas: u128,
    pub record_stack: bool,
    pub record_memory: bool,
}

impl Tracer {
    fn record(&mut self, step: Step) {
        *self.gas_by_opcode.entry(step.opcode).or_insert(0) += step.gas_cost as u128;
        self.total_gas += step.gas_cost as u128;
        self.steps.push(step);
    }
}

/// Human-readable opcode names (for gas reports and the debugger).
pub fn opcode_name(op: u8) -> &'static str {
    match op {
        0x00 => "STOP",
        0x01 => "ADD",
        0x02 => "MUL",
        0x03 => "SUB",
        0x04 => "DIV",
        0x05 => "SDIV",
        0x06 => "ADDMOD",
        0x07 => "MULMOD",
        0x08 => "EXP",
        0x09 => "SIGNEXTEND",
        0x10 => "LT",
        0x11 => "GT",
        0x12 => "SLT",
        0x13 => "SGT",
        0x14 => "EQ",
        0x15 => "ISZERO",
        0x16 => "AND",
        0x17 => "OR",
        0x18 => "XOR",
        0x19 => "NOT",
        0x1a => "BYTE",
        0x1b => "SHL",
        0x1c => "SHR",
        0x1d => "SAR",
        0x20 => "KECCAK256",
        0x30 => "ADDRESS",
        0x31 => "BALANCE",
        0x32 => "ORIGIN",
        0x33 => "CALLER",
        0x34 => "CALLVALUE",
        0x35 => "CALLDATALOAD",
        0x36 => "CALLDATASIZE",
        0x37 => "CALLDATACOPY",
        0x38 => "CODESIZE",
        0x39 => "CODECOPY",
        0x3a => "GASPRICE",
        0x3b => "EXTCODESIZE",
        0x3c => "EXTCODECOPY",
        0x3d => "RETURNDATASIZE",
        0x3e => "RETURNDATACOPY",
        0x3f => "EXTCODEHASH",
        0x40 => "BLOCKHASH",
        0x41 => "COINBASE",
        0x42 => "TIMESTAMP",
        0x43 => "NUMBER",
        0x44 => "PREVRANDAO",
        0x45 => "GASLIMIT",
        0x46 => "CHAINID",
        0x47 => "SELFBALANCE",
        0x48 => "BASEFEE",
        0x49 => "BLOBHASH",
        0x4a => "BLOBBASEFEE",
        0x50 => "POP",
        0x51 => "MLOAD",
        0x52 => "MSTORE",
        0x53 => "MSTORE8",
        0x54 => "SLOAD",
        0x55 => "SSTORE",
        0x56 => "JUMP",
        0x57 => "JUMPI",
        0x58 => "PC",
        0x59 => "MSIZE",
        0x5a => "GAS",
        0x5b => "JUMPDEST",
        0x5c => "TLOAD",
        0x5d => "TSTORE",
        0x5e => "MCOPY",
        0x5f => "PUSH0",
        0xa0 => "LOG0",
        0xa1 => "LOG1",
        0xa2 => "LOG2",
        0xa3 => "LOG3",
        0xa4 => "LOG4",
        0xf0 => "CREATE",
        0xf1 => "CALL",
        0xf2 => "CALLCODE",
        0xf3 => "RETURN",
        0xf4 => "DELEGATECALL",
        0xf5 => "CREATE2",
        0xfa => "STATICCALL",
        0xfd => "REVERT",
        0xfe => "INVALID",
        0xff => "SELFDESTRUCT",
        _ => {
            if (0x60..=0x7f).contains(&op) {
                "PUSH"
            } else if (0x80..=0x8f).contains(&op) {
                "DUP"
            } else if (0x90..=0x9f).contains(&op) {
                "SWAP"
            } else {
                "UNKNOWN"
            }
        }
    }
}

// ---------------------------------------------------------------- frames

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum FrameKind {
    Call,
    CallCode,
    DelegateCall,
    StaticCall,
    Create,
    Create2,
}

pub struct Frame {
    pub kind: FrameKind,
    pub address: H160,
    pub caller: H160,
    pub value: U256,
    pub input: Vec<u8>,
    pub gas: u64,
    pub is_static: bool,
    pub code: Vec<u8>,
    pub returndata: Vec<u8>,
    pub created_in_tx: bool,
    /// plain CALL frames transfer value to `address` (root tx calls and nested CALLs)
    pub needs_transfer: bool,
}

struct CreateOutcome {
    success: bool,
    reverted: bool,
    gas_back: u64,
    output: Vec<u8>,
    address: H160,
}

// ---------------------------------------------------------------- machine

pub struct Machine<'h> {
    host: &'h mut dyn Host,
    pub spec: Spec,
    pub block: BlockEnv,
    pub tx: TxEnv,
    pub depth: usize,
    pub warm_addresses: HashSet<H160>,
    pub warm_slots: HashSet<(H160, U256)>,
    pub refund: i64,
    pub created_in_tx: HashSet<H160>,
    pub tracer: Option<Tracer>,
}

enum Flow {
    Continue,
    Return(Vec<u8>),
    Revert(Vec<u8>),
    SelfDestruct,
    Err(EvmError),
}

fn pop(stack: &mut Vec<U256>) -> Result<U256, EvmError> {
    stack.pop().ok_or(EvmError::StackUnderflow)
}

fn push(stack: &mut Vec<U256>, v: U256) -> Result<(), EvmError> {
    if stack.len() >= MAX_STACK {
        return Err(EvmError::StackOverflow);
    }
    stack.push(v);
    Ok(())
}

fn push_flow(stack: &mut Vec<U256>, v: U256) -> Flow {
    match push(stack, v) {
        Ok(()) => Flow::Continue,
        Err(e) => Flow::Err(e),
    }
}

impl<'h> Machine<'h> {
    pub fn new(
        host: &'h mut dyn Host,
        spec: Spec,
        block: BlockEnv,
        tx: TxEnv,
        record_trace: bool,
    ) -> Self {
        Machine {
            host,
            spec,
            block,
            tx,
            depth: 0,
            warm_addresses: HashSet::new(),
            warm_slots: HashSet::new(),
            refund: 0,
            created_in_tx: HashSet::new(),
            tracer: if record_trace { Some(Tracer::default()) } else { None },
        }
    }

    /// Execute a transaction body. The caller (chain) must have validated the tx,
    /// incremented the sender nonce, and set tx.gas_limit = gas after intrinsic.
    pub fn run_tx(mut self) -> ExecutionResult {
        self.warm_addresses.insert(self.tx.caller);
        for a in 1..=9u64 {
            self.warm_addresses.insert(H160::from_low_u64_be(a));
        }
        for (addr, keys) in &self.tx.access_list {
            self.warm_addresses.insert(*addr);
            for k in keys {
                self.warm_slots.insert((*addr, *k));
            }
        }

        let gas = self.tx.gas_limit;
        // coverage: record (running code hash, 4-byte selector) for top-level calls
        if let Some(to) = self.tx.to {
            let code = self.host.code(&to);
            if code.len() >= 4 && self.tx.data.len() >= 4 {
                crate::coverage_note(
                    H256(keccak256(&code)),
                    [
                        self.tx.data[0],
                        self.tx.data[1],
                        self.tx.data[2],
                        self.tx.data[3],
                    ],
                );
            }
        }
        let (kind, gas_remaining, output, created) = match self.tx.to {
            Some(to) => {
                self.warm_addresses.insert(to);
                let code = self.host.code(&to);
                let frame = Frame {
                    kind: FrameKind::Call,
                    address: to,
                    caller: self.tx.caller,
                    value: self.tx.value,
                    input: self.tx.data.clone(),
                    gas,
                    is_static: false,
                    code,
                    returndata: vec![],
                    created_in_tx: false,
                    needs_transfer: true,
                };
                let r = self.run_frame(frame);
                (r.kind, r.gas_remaining, r.output, None)
            }
            None => {
                let nonce = self.host.nonce(&self.tx.caller);
                let addr = create_address(&self.tx.caller, nonce);
                self.created_in_tx.insert(addr);
                self.warm_addresses.insert(addr);
                let out = self.do_create(
                    self.tx.caller,
                    self.tx.value,
                    self.tx.data.clone(),
                    None,
                    gas,
                    false,
                    false,
                );
                let kind = if out.success {
                    ExitKind::Return
                } else if out.reverted {
                    ExitKind::Revert
                } else {
                    ExitKind::Error(EvmError::OutOfGas)
                };
                (kind, out.gas_back, out.output, Some(out.address))
            }
        };

        ExecutionResult {
            kind,
            gas_used: gas - gas_remaining,
            gas_remaining,
            output,
            created_address: created,
            refund: self.refund,
            trace: self.tracer.take(),
        }
    }

    // -------- helpers --------

    fn charge(&self, frame: &mut Frame, amount: u64) -> Result<(), EvmError> {
        frame.gas = frame.gas.checked_sub(amount).ok_or(EvmError::OutOfGas)?;
        Ok(())
    }

    fn account_access_cost(&mut self, addr: &H160) -> u64 {
        if self.spec.eip2929 && !self.warm_addresses.contains(addr) {
            self.warm_addresses.insert(*addr);
            GAS_COLD_ACCOUNT
        } else {
            GAS_WARM
        }
    }

    fn slot_access_cost(&mut self, addr: &H160, key: &U256) -> u64 {
        if self.spec.eip2929 && !self.warm_slots.contains(&(*addr, *key)) {
            self.warm_slots.insert((*addr, *key));
            GAS_COLD_SLOAD
        } else {
            0
        }
    }

    fn expand_memory(
        &self,
        memory: &mut Vec<u8>,
        frame: &mut Frame,
        needed: usize,
    ) -> Result<(), EvmError> {
        if needed > memory.len() {
            let cost = expansion_cost(memory.len(), needed);
            self.charge(frame, cost)?;
            memory.resize(needed, 0);
        }
        Ok(())
    }

    fn mem_region(
        &self,
        memory: &mut Vec<u8>,
        frame: &mut Frame,
        offset: U256,
        len: U256,
    ) -> Result<usize, EvmError> {
        if len.is_zero() {
            // Zero-length ops never expand memory, but the offset must still be
            // validated so caller slices `memory[off..end]` stay in-bounds and empty.
            if offset > U256::from(memory.len()) {
                return Err(EvmError::MemoryOOG);
            }
            return Ok(offset.as_usize());
        }
        let end = offset.checked_add(len).ok_or(EvmError::MemoryOOG)?;
        if end.bits() > 40 {
            return Err(EvmError::MemoryOOG);
        }
        let end = end.as_usize();
        self.expand_memory(memory, frame, end)?;
        Ok(end)
    }

    fn abort_frame(&mut self, host_cp: u64, refund_before: i64, err: EvmError) -> FrameResult {
        self.host.rollback(host_cp);
        self.refund = refund_before;
        FrameResult { kind: ExitKind::Error(err), gas_remaining: 0, output: vec![], created_address: None }
    }

    fn revert_frame(&mut self, host_cp: u64, refund_before: i64, gas: u64, data: Vec<u8>) -> FrameResult {
        self.host.rollback(host_cp);
        self.refund = refund_before;
        FrameResult { kind: ExitKind::Revert, gas_remaining: gas, output: data, created_address: None }
    }

    fn trace_step(
        &mut self,
        op: u8,
        pc: usize,
        gas_before: u64,
        gas_cost: u64,
        stack: &[U256],
        memory: &[u8],
        error: Option<String>,
    ) {
        if let Some(t) = self.tracer.as_mut() {
            let step = Step {
                pc,
                opcode: op,
                depth: self.depth,
                gas_before,
                gas_cost,
                stack: if t.record_stack { Some(stack.to_vec()) } else { None },
                memory_len: memory.len(),
                memory: if t.record_memory { Some(memory.to_vec()) } else { None },
                error,
            };
            t.record(step);
        }
    }

    // -------- create --------

    #[allow(clippy::too_many_arguments)]
    fn do_create(
        &mut self,
        creator: H160,
        value: U256,
        init_code: Vec<u8>,
        salt: Option<U256>,
        gas: u64,
        is_static: bool,
        inc_nonce: bool,
    ) -> CreateOutcome {
        let zero_addr = H160::zero();
        if is_static {
            return CreateOutcome { success: false, reverted: false, gas_back: gas, output: vec![], address: zero_addr };
        }
        if self.depth + 1 > MAX_CALL_DEPTH {
            return CreateOutcome { success: false, reverted: false, gas_back: gas, output: vec![], address: zero_addr };
        }
        if self.spec.eip3860 && init_code.len() > MAX_INIT_CODE {
            return CreateOutcome { success: false, reverted: false, gas_back: 0, output: vec![], address: zero_addr };
        }

        let address = match salt {
            None => {
                if inc_nonce {
                    let n = self.host.inc_nonce(&creator);
                    create_address(&creator, n)
                } else {
                    create_address(&creator, self.host.nonce(&creator))
                }
            }
            Some(salt) => {
                let code_hash = H256(keccak256(&init_code));
                create2_address(&creator, &H256(rrx_util::u256_to_be32(&salt)), &code_hash)
            }
        };

        // EIP-684 collision
        if self.host.exists(&address)
            && (self.host.nonce(&address) > 0 || self.host.code_hash(&address) != rrx_util::KECCAK_EMPTY)
        {
            return CreateOutcome { success: false, reverted: false, gas_back: 0, output: vec![], address };
        }

        if value > U256::zero() {
            if self.host.balance(&creator) < value {
                return CreateOutcome { success: false, reverted: false, gas_back: gas, output: vec![], address };
            }
            self.host.add_balance(&creator, U256::zero().overflowing_sub(value).0);
            self.host.add_balance(&address, value);
        }

        let frame = Frame {
            kind: FrameKind::Create,
            address,
            caller: creator,
            value,
            input: vec![],
            gas,
            is_static: false,
            code: init_code,
            returndata: vec![],
            created_in_tx: true,
            needs_transfer: false,
        };
        self.depth += 1;
        self.created_in_tx.insert(address);
        let res = self.run_frame(frame);
        self.depth -= 1;

        match res.kind {
            ExitKind::Stop | ExitKind::Return | ExitKind::SelfDestruct => {
                let code = res.output;
                if code.len() > MAX_CODE_SIZE {
                    return CreateOutcome { success: false, reverted: false, gas_back: 0, output: vec![], address };
                }
                if self.spec.eip3541 && code.first() == Some(&0xef) {
                    return CreateOutcome { success: false, reverted: false, gas_back: 0, output: vec![], address };
                }
                let deposit = GAS_CODE_DEPOSIT * code.len() as u64;
                if res.gas_remaining < deposit {
                    return CreateOutcome { success: false, reverted: false, gas_back: 0, output: vec![], address };
                }
                self.host.set_code(&address, code);
                CreateOutcome { success: true, reverted: false, gas_back: res.gas_remaining - deposit, output: vec![], address }
            }
            ExitKind::Revert => {
                CreateOutcome { success: false, reverted: true, gas_back: res.gas_remaining, output: res.output, address }
            }
            ExitKind::Error(_) => {
                CreateOutcome { success: false, reverted: false, gas_back: 0, output: vec![], address }
            }
        }
    }

    // -------- sub-calls --------

    #[allow(clippy::too_many_arguments)]
    fn exec_subcall(
        &mut self,
        kind: FrameKind,
        ctx_address: H160,
        ctx_caller: H160,
        ctx_value: U256,
        to: H160,
        transfer: U256,
        input: Vec<u8>,
        gas: u64,
        is_static: bool,
    ) -> (bool, u64, Vec<u8>) {
        if self.depth + 1 > MAX_CALL_DEPTH {
            return (false, gas, vec![]);
        }

        // precompiles (no frame): value transfer happens here, journaled by the parent scope
        let to_bytes = to.as_bytes();
        if to_bytes[..19].iter().all(|&b| b == 0) && (1..=0x11).contains(&to_bytes[19]) {
            if kind == FrameKind::Call && transfer > U256::zero() {
                if self.host.balance(&ctx_address) < transfer {
                    return (false, gas, vec![]);
                }
                self.host.add_balance(&ctx_address, U256::zero().overflowing_sub(transfer).0);
                self.host.add_balance(&to, transfer);
            }
            let num = to_bytes[19];
            let out = pre::execute(num, &input, gas);
            return match out {
                pre::PrecompileOutput::Success { gas_used, output } => (true, gas - gas_used, output),
                pre::PrecompileOutput::Failure { .. } => (false, 0, vec![]),
            };
        }

        let (address, caller, value) = match kind {
            FrameKind::DelegateCall => (ctx_address, ctx_caller, ctx_value),
            FrameKind::CallCode => (ctx_address, ctx_caller, transfer),
            FrameKind::StaticCall => (to, ctx_address, U256::zero()),
            _ => (to, ctx_address, transfer),
        };

        let code = self.host.code(&to);
        let frame = Frame {
            kind,
            address,
            caller,
            value,
            input,
            gas,
            is_static: is_static || kind == FrameKind::StaticCall,
            code,
            returndata: vec![],
            created_in_tx: false,
            needs_transfer: kind == FrameKind::Call,
        };
        self.depth += 1;
        let res = self.run_frame(frame);
        self.depth -= 1;

        match res.kind {
            ExitKind::Stop | ExitKind::Return | ExitKind::SelfDestruct => {
                (true, res.gas_remaining, res.output)
            }
            ExitKind::Revert => (false, res.gas_remaining, res.output),
            ExitKind::Error(_) => (false, 0, vec![]),
        }
    }

    // -------- main interpreter loop --------

    fn run_frame(&mut self, mut f: Frame) -> FrameResult {
        let host_cp = self.host.checkpoint();
        let refund_before = self.refund;
        if f.needs_transfer && f.value > U256::zero() {
            if self.host.balance(&f.caller) < f.value {
                return FrameResult {
                    kind: ExitKind::Revert,
                    gas_remaining: f.gas,
                    output: vec![],
                    created_address: None,
                };
            }
            self.host.add_balance(&f.caller, U256::zero().overflowing_sub(f.value).0);
            self.host.add_balance(&f.address, f.value);
        }
        let mut stack: Vec<U256> = Vec::with_capacity(32);
        let mut memory: Vec<u8> = Vec::new();
        let jumpdests = jumpdest_analysis(&f.code);
        let mut pc: usize = 0;

        loop {
            let op = if pc < f.code.len() { f.code[pc] } else { 0x00 };
            let gas_before = f.gas;

            // ---- base cost ----
            let base: u64 = match op {
                0x01 | 0x03 | 0x10..=0x1d => 3,
                0x02 | 0x04 | 0x05 | 0x09 => 5,
                0x06 | 0x07 => 8,
                0x30 | 0x32 | 0x33 | 0x34 | 0x36 | 0x38 | 0x3a | 0x3d | 0x41 | 0x42 | 0x43
                | 0x44 | 0x45 | 0x46 | 0x48 | 0x50 | 0x58 | 0x59 | 0x5f => 2,
                0x35 => 3,
                0x40 => 20,
                0x47 => 5,
                0x49 => 3,
                0x4a => 2,
                0x56 => 8,
                0x57 => 10,
                0x5a => 2,
                0x5b => 1,
                0x5c | 0x5d => 100,
                0x60..=0x7f | 0x80..=0x9f => 3,
                // dynamic: KECCAK, EXP, BALANCE, EXTCODE*, SLOAD, SSTORE, copies, memory ops,
                // LOG*, CREATE*, CALL*, SELFDESTRUCT, RETURN, REVERT, STOP, INVALID
                _ => 0,
            };
            if self.charge(&mut f, base).is_err() {
                self.trace_step(op, pc, gas_before, gas_before, &stack, &memory, Some("out of gas".into()));
                return self.abort_frame(host_cp, refund_before, EvmError::OutOfGas);
            }

            let mut next_pc = pc + 1 + if (0x60..=0x7f).contains(&op) { (op - 0x5f) as usize } else { 0 };

            let flow: Flow = match op {
                0x00 => Flow::Return(vec![]), // STOP

                0x01 => { let b = pop(&mut stack); let a = pop(&mut stack); match (a, b) { (Ok(a), Ok(b)) => push_flow(&mut stack, a.overflowing_add(b).0), _ => Flow::Err(EvmError::StackUnderflow) } }
                0x02 => { let b = pop(&mut stack); let a = pop(&mut stack); match (a, b) { (Ok(a), Ok(b)) => push_flow(&mut stack, a.overflowing_mul(b).0), _ => Flow::Err(EvmError::StackUnderflow) } }
                0x03 => { let x = pop(&mut stack); let y = pop(&mut stack); match (x, y) { (Ok(x), Ok(y)) => push_flow(&mut stack, x.overflowing_sub(y).0), _ => Flow::Err(EvmError::StackUnderflow) } }
                0x04 => { let x = pop(&mut stack); let y = pop(&mut stack); match (x, y) { (Ok(x), Ok(y)) => push_flow(&mut stack, if y.is_zero() { U256::zero() } else { x / y }), _ => Flow::Err(EvmError::StackUnderflow) } }
                0x05 => { let x = pop(&mut stack); let y = pop(&mut stack); match (x, y) { (Ok(x), Ok(y)) => push_flow(&mut stack, sdiv(x, y)), _ => Flow::Err(EvmError::StackUnderflow) } }
                0x06 => { let z = pop(&mut stack); let y = pop(&mut stack); let x = pop(&mut stack); match (x, y, z) { (Ok(x), Ok(y), Ok(z)) => push_flow(&mut stack, addmod(x, y, z)), _ => Flow::Err(EvmError::StackUnderflow) } }
                0x07 => { let z = pop(&mut stack); let y = pop(&mut stack); let x = pop(&mut stack); match (x, y, z) { (Ok(x), Ok(y), Ok(z)) => push_flow(&mut stack, mulmod(x, y, z)), _ => Flow::Err(EvmError::StackUnderflow) } }
                0x08 => {
                    let base = pop(&mut stack); let exp = pop(&mut stack);
                    match (base, exp) {
                        (Ok(base), Ok(exp)) => {
                            let cost = 10u64.saturating_add(50u64.saturating_mul(((exp.bits() + 7) / 8) as u64));
                            if self.charge(&mut f, cost).is_err() {
                                Flow::Err(EvmError::OutOfGas)
                            } else {
                                push_flow(&mut stack, base.overflowing_pow(exp).0)
                            }
                        }
                        _ => Flow::Err(EvmError::StackUnderflow),
                    }
                }
                0x09 => { let k = pop(&mut stack); let x = pop(&mut stack); match (x, k) { (Ok(x), Ok(k)) => push_flow(&mut stack, signextend(k, x)), _ => Flow::Err(EvmError::StackUnderflow) } }
                0x10 => { let x = pop(&mut stack); let y = pop(&mut stack); match (x, y) { (Ok(x), Ok(y)) => push_flow(&mut stack, bool_u256(x < y)), _ => Flow::Err(EvmError::StackUnderflow) } }
                0x11 => { let x = pop(&mut stack); let y = pop(&mut stack); match (x, y) { (Ok(x), Ok(y)) => push_flow(&mut stack, bool_u256(x > y)), _ => Flow::Err(EvmError::StackUnderflow) } }
                0x12 => { let x = pop(&mut stack); let y = pop(&mut stack); match (x, y) { (Ok(x), Ok(y)) => push_flow(&mut stack, bool_u256(slt(&x, &y))), _ => Flow::Err(EvmError::StackUnderflow) } }
                0x13 => { let x = pop(&mut stack); let y = pop(&mut stack); match (x, y) { (Ok(x), Ok(y)) => push_flow(&mut stack, bool_u256(sgt(&x, &y))), _ => Flow::Err(EvmError::StackUnderflow) } }
                0x14 => { let b = pop(&mut stack); let a = pop(&mut stack); match (a, b) { (Ok(a), Ok(b)) => push_flow(&mut stack, bool_u256(a == b)), _ => Flow::Err(EvmError::StackUnderflow) } }
                0x15 => { let a = pop(&mut stack); match a { Ok(a) => push_flow(&mut stack, bool_u256(a.is_zero())), Err(e) => Flow::Err(e) } }
                0x16 => { let b = pop(&mut stack); let a = pop(&mut stack); match (a, b) { (Ok(a), Ok(b)) => push_flow(&mut stack, a & b), _ => Flow::Err(EvmError::StackUnderflow) } }
                0x17 => { let b = pop(&mut stack); let a = pop(&mut stack); match (a, b) { (Ok(a), Ok(b)) => push_flow(&mut stack, a | b), _ => Flow::Err(EvmError::StackUnderflow) } }
                0x18 => { let b = pop(&mut stack); let a = pop(&mut stack); match (a, b) { (Ok(a), Ok(b)) => push_flow(&mut stack, a ^ b), _ => Flow::Err(EvmError::StackUnderflow) } }
                0x19 => { let a = pop(&mut stack); match a { Ok(a) => push_flow(&mut stack, !a), Err(e) => Flow::Err(e) } }
                0x1a => { let i = pop(&mut stack); let x = pop(&mut stack); match (x, i) { (Ok(x), Ok(i)) => push_flow(&mut stack, byte_op(i, x)), _ => Flow::Err(EvmError::StackUnderflow) } }
                0x1b => { let s = pop(&mut stack); let x = pop(&mut stack); match (x, s) { (Ok(x), Ok(s)) => push_flow(&mut stack, shl(s, x)), _ => Flow::Err(EvmError::StackUnderflow) } }
                0x1c => { let s = pop(&mut stack); let x = pop(&mut stack); match (x, s) { (Ok(x), Ok(s)) => push_flow(&mut stack, shr(s, x)), _ => Flow::Err(EvmError::StackUnderflow) } }
                0x1d => { let s = pop(&mut stack); let x = pop(&mut stack); match (x, s) { (Ok(x), Ok(s)) => push_flow(&mut stack, sar(s, x)), _ => Flow::Err(EvmError::StackUnderflow) } }

                0x20 => {
                    let off = pop(&mut stack); let len = pop(&mut stack);
                    match (off, len) {
                        (Ok(off), Ok(len)) => match self.mem_region(&mut memory, &mut f, off, len) {
                            Ok(end) => {
                                let cost = 30u64.saturating_add(6u64.saturating_mul(words(len.as_usize())));
                                if self.charge(&mut f, cost).is_err() {
                                    Flow::Err(EvmError::OutOfGas)
                                } else {
                                    let data = memory[off.as_usize()..end].to_vec();
                                    push_flow(&mut stack, U256::from_big_endian(&keccak256(&data)))
                                }
                            }
                            Err(e) => Flow::Err(e),
                        },
                        _ => Flow::Err(EvmError::StackUnderflow),
                    }
                }

                0x30 => push_flow(&mut stack, u256_from_h160(&f.address)),
                0x31 => {
                    let a = pop(&mut stack);
                    match a {
                        Ok(a) => {
                            let addr = h160_from_u256(&a);
                            let cost = if self.spec.eip2929 { self.account_access_cost(&addr) } else { 700 };
                            if self.charge(&mut f, cost).is_err() {
                                Flow::Err(EvmError::OutOfGas)
                            } else {
                                push_flow(&mut stack, self.host.balance(&addr))
                            }
                        }
                        Err(e) => Flow::Err(e),
                    }
                }
                0x32 => push_flow(&mut stack, u256_from_h160(&self.tx.caller)),
                0x33 => push_flow(&mut stack, u256_from_h160(&f.caller)),
                0x34 => push_flow(&mut stack, f.value),
                0x35 => {
                    let off = pop(&mut stack);
                    match off {
                        Ok(off) => {
                            let mut word = [0u8; 32];
                            if off.bits() <= 33 {
                                let start = off.as_usize();
                                for (i, b) in word.iter_mut().enumerate() {
                                    if start + i < f.input.len() {
                                        *b = f.input[start + i];
                                    }
                                }
                            }
                            push_flow(&mut stack, U256::from_big_endian(&word))
                        }
                        Err(e) => Flow::Err(e),
                    }
                }
                0x36 => push_flow(&mut stack, U256::from(f.input.len())),
                0x37 => {
                    let dst = pop(&mut stack); let off = pop(&mut stack); let len = pop(&mut stack);
                    match (dst, off, len) {
                        (Ok(dst), Ok(off), Ok(len)) => match self.mem_region(&mut memory, &mut f, dst, len) {
                            Ok(end) => {
                                let cost = 3 + 3u64.saturating_mul(words(len.as_usize()));
                                if self.charge(&mut f, cost).is_err() {
                                    Flow::Err(EvmError::OutOfGas)
                                } else if off.bits() > 64 {
                                    Flow::Err(EvmError::OutOfBounds)
                                } else {
                                    copy_zeropad(&mut memory, dst.as_usize(), &f.input, off.as_usize(), len.as_usize(), end);
                                    Flow::Continue
                                }
                            }
                            Err(e) => Flow::Err(e),
                        },
                        _ => Flow::Err(EvmError::StackUnderflow),
                    }
                }
                0x38 => push_flow(&mut stack, U256::from(f.code.len())),
                0x39 => {
                    let dst = pop(&mut stack); let off = pop(&mut stack); let len = pop(&mut stack);
                    match (dst, off, len) {
                        (Ok(dst), Ok(off), Ok(len)) => match self.mem_region(&mut memory, &mut f, dst, len) {
                            Ok(end) => {
                                let cost = 3 + 3u64.saturating_mul(words(len.as_usize()));
                                if self.charge(&mut f, cost).is_err() {
                                    Flow::Err(EvmError::OutOfGas)
                                } else if off.bits() > 64 {
                                    Flow::Err(EvmError::OutOfBounds)
                                } else {
                                    copy_zeropad(&mut memory, dst.as_usize(), &f.code, off.as_usize(), len.as_usize(), end);
                                    Flow::Continue
                                }
                            }
                            Err(e) => Flow::Err(e),
                        },
                        _ => Flow::Err(EvmError::StackUnderflow),
                    }
                }
                0x3a => push_flow(&mut stack, self.tx.gas_price),
                0x3b => {
                    let a = pop(&mut stack);
                    match a {
                        Ok(a) => {
                            let addr = h160_from_u256(&a);
                            let cost = if self.spec.eip2929 { self.account_access_cost(&addr) } else { 700 };
                            if self.charge(&mut f, cost).is_err() {
                                Flow::Err(EvmError::OutOfGas)
                            } else {
                                push_flow(&mut stack, U256::from(self.host.code(&addr).len()))
                            }
                        }
                        Err(e) => Flow::Err(e),
                    }
                }
                0x3c => {
                    let len = pop(&mut stack); let off = pop(&mut stack); let src = pop(&mut stack); let dst = pop(&mut stack);
                    match (dst, src, off, len) {
                        (Ok(dst), Ok(src), Ok(off), Ok(len)) => {
                            let addr = h160_from_u256(&dst);
                            let acc = if self.spec.eip2929 { self.account_access_cost(&addr) } else { 700 };
                            match self.mem_region(&mut memory, &mut f, src, len) {
                                Ok(end) => {
                                    let cost = acc + 3u64.saturating_mul(words(len.as_usize()));
                                    if self.charge(&mut f, cost).is_err() {
                                        Flow::Err(EvmError::OutOfGas)
                                    } else if off.bits() > 64 {
                                        Flow::Err(EvmError::OutOfBounds)
                                    } else {
                                        let code = self.host.code(&addr);
                                        copy_zeropad(&mut memory, src.as_usize(), &code, off.as_usize(), len.as_usize(), end);
                                        Flow::Continue
                                    }
                                }
                                Err(e) => Flow::Err(e),
                            }
                        }
                        _ => Flow::Err(EvmError::StackUnderflow),
                    }
                }
                0x3d => push_flow(&mut stack, U256::from(f.returndata.len())),
                0x3e => {
                    let dst = pop(&mut stack); let off = pop(&mut stack); let len = pop(&mut stack);
                    match (dst, off, len) {
                        (Ok(dst), Ok(off), Ok(len)) => {
                            if off.bits() > 33 || len.bits() > 33 || off.as_usize() + len.as_usize() > f.returndata.len() {
                                Flow::Err(EvmError::OutOfBounds)
                            } else {
                                match self.mem_region(&mut memory, &mut f, dst, len) {
                                    Ok(end) => {
                                        let cost = 3 + 3u64.saturating_mul(words(len.as_usize()));
                                        if self.charge(&mut f, cost).is_err() {
                                            Flow::Err(EvmError::OutOfGas)
                                        } else {
                                            copy_zeropad(&mut memory, dst.as_usize(), &f.returndata, off.as_usize(), len.as_usize(), end);
                                            Flow::Continue
                                        }
                                    }
                                    Err(e) => Flow::Err(e),
                                }
                            }
                        }
                        _ => Flow::Err(EvmError::StackUnderflow),
                    }
                }
                0x3f => {
                    let a = pop(&mut stack);
                    match a {
                        Ok(a) => {
                            let addr = h160_from_u256(&a);
                            let cost = if self.spec.eip2929 { self.account_access_cost(&addr) } else { 400 };
                            if self.charge(&mut f, cost).is_err() {
                                Flow::Err(EvmError::OutOfGas)
                            } else {
                                if self.host.exists(&addr) {
                                    push_flow(&mut stack, u256_from_h256(&self.host.code_hash(&addr)))
                                } else {
                                    push_flow(&mut stack, U256::zero())
                                }
                            }
                        }
                        Err(e) => Flow::Err(e),
                    }
                }
                0x40 => {
                    let n = pop(&mut stack);
                    match n {
                        Ok(n) => {
                            if n.bits() > 64 {
                                push_flow(&mut stack, U256::zero())
                            } else {
                                let num = n.as_u64();
                                let cur = self.block.number;
                                if num >= cur || (cur > 256 && num < cur - 256) {
                                    push_flow(&mut stack, U256::zero())
                                } else {
                                    push_flow(&mut stack, u256_from_h256(&self.host.block_hash(num).unwrap_or_default()))
                                }
                            }
                        }
                        Err(e) => Flow::Err(e),
                    }
                }
                0x41 => push_flow(&mut stack, u256_from_h160(&self.block.coinbase)),
                0x42 => push_flow(&mut stack, U256::from(self.block.timestamp)),
                0x43 => push_flow(&mut stack, U256::from(self.block.number)),
                0x44 => push_flow(&mut stack, u256_from_h256(&self.block.prevrandao)),
                0x45 => push_flow(&mut stack, U256::from(self.block.gas_limit)),
                0x46 => push_flow(&mut stack, U256::from(self.tx.chain_id)),
                0x47 => push_flow(&mut stack, self.host.balance(&f.address)),
                0x48 => push_flow(&mut stack, self.block.base_fee),
                0x49 => {
                    let i = pop(&mut stack);
                    match i {
                        Ok(i) => {
                            if i.bits() > 64 {
                                push_flow(&mut stack, U256::zero())
                            } else {
                                push_flow(&mut stack, u256_from_h256(self.tx.blob_hashes.get(i.as_usize()).unwrap_or(&H256::zero())))
                            }
                        }
                        Err(e) => Flow::Err(e),
                    }
                }
                0x4a => push_flow(&mut stack, blob_base_fee(self.block.excess_blob_gas)),

                0x50 => { let a = pop(&mut stack); match a { Ok(_) => Flow::Continue, Err(e) => Flow::Err(e) } }
                0x51 => {
                    let off = pop(&mut stack);
                    match off {
                        Ok(off) => match self.mem_region(&mut memory, &mut f, off, U256::from(32u8)) {
                            Ok(end) => {
                                if self.charge(&mut f, 3).is_err() {
                                    Flow::Err(EvmError::OutOfGas)
                                } else {
                                    push_flow(&mut stack, U256::from_big_endian(&memory[off.as_usize()..end]))
                                }
                            }
                            Err(e) => Flow::Err(e),
                        },
                        Err(e) => Flow::Err(e),
                    }
                }
                0x52 => {
                    let off = pop(&mut stack); let val = pop(&mut stack);
                    match (off, val) {
                        (Ok(off), Ok(val)) => match self.mem_region(&mut memory, &mut f, off, U256::from(32u8)) {
                            Ok(end) => {
                                if self.charge(&mut f, 3).is_err() {
                                    Flow::Err(EvmError::OutOfGas)
                                } else {
                                    memory[off.as_usize()..end].copy_from_slice(&rrx_util::u256_to_be32(&val));
                                    Flow::Continue
                                }
                            }
                            Err(e) => Flow::Err(e),
                        },
                        _ => Flow::Err(EvmError::StackUnderflow),
                    }
                }
                0x53 => {
                    let off = pop(&mut stack); let val = pop(&mut stack);
                    match (off, val) {
                        (Ok(off), Ok(val)) => match self.mem_region(&mut memory, &mut f, off, U256::one()) {
                            Ok(end) => {
                                if self.charge(&mut f, 3).is_err() {
                                    Flow::Err(EvmError::OutOfGas)
                                } else {
                                    memory[off.as_usize()..end].copy_from_slice(&[val.byte(0)]);
                                    Flow::Continue
                                }
                            }
                            Err(e) => Flow::Err(e),
                        },
                        _ => Flow::Err(EvmError::StackUnderflow),
                    }
                }
                0x54 => {
                    let key = pop(&mut stack);
                    match key {
                        Ok(key) => {
                            let cost = if self.spec.eip2929 {
                                self.slot_access_cost(&f.address, &key) + GAS_WARM
                            } else {
                                800
                            };
                            if self.charge(&mut f, cost).is_err() {
                                Flow::Err(EvmError::OutOfGas)
                            } else {
                                push_flow(&mut stack, self.host.storage(&f.address, &key))
                            }
                        }
                        Err(e) => Flow::Err(e),
                    }
                }
                0x55 => {
                    let key = pop(&mut stack); let val = pop(&mut stack);
                    match (key, val) {
                        (Ok(key), Ok(val)) => {
                            if f.is_static {
                                Flow::Err(EvmError::StaticStateChange)
                            } else if self.spec.eip2200 && f.gas <= GAS_STIPEND {
                                Flow::Err(EvmError::OutOfGas)
                            } else {
                                let cold = self.slot_access_cost(&f.address, &key);
                                if self.charge(&mut f, cold).is_err() {
                                    Flow::Err(EvmError::OutOfGas)
                                } else {
                                    match self.op_sstore(&mut f, key, val) {
                                        Ok(()) => Flow::Continue,
                                        Err(e) => Flow::Err(e),
                                    }
                                }
                            }
                        }
                        _ => Flow::Err(EvmError::StackUnderflow),
                    }
                }
                0x56 => {
                    let dst = pop(&mut stack);
                    match dst {
                        Ok(dst) => {
                            if dst.bits() > 32 {
                                Flow::Err(EvmError::InvalidJumpDestination)
                            } else {
                                let t = dst.as_usize();
                                if t < f.code.len() && f.code[t] == 0x5b && jumpdests.contains(&t) {
                                    next_pc = t;
                                    Flow::Continue
                                } else {
                                    Flow::Err(EvmError::InvalidJumpDestMarker)
                                }
                            }
                        }
                        Err(e) => Flow::Err(e),
                    }
                }
                0x57 => {
                    let dst = pop(&mut stack); let cond = pop(&mut stack);
                    match (dst, cond) {
                        (Ok(dst), Ok(cond)) => {
                            if !cond.is_zero() {
                                if dst.bits() > 32 {
                                    Flow::Err(EvmError::InvalidJumpDestination)
                                } else {
                                    let t = dst.as_usize();
                                    if t < f.code.len() && f.code[t] == 0x5b && jumpdests.contains(&t) {
                                        next_pc = t;
                                        Flow::Continue
                                    } else {
                                        Flow::Err(EvmError::InvalidJumpDestMarker)
                                    }
                                }
                            } else {
                                Flow::Continue
                            }
                        }
                        _ => Flow::Err(EvmError::StackUnderflow),
                    }
                }
                0x58 => push_flow(&mut stack, U256::from(pc)),
                0x59 => push_flow(&mut stack, U256::from(memory.len())),
                0x5a => push_flow(&mut stack, U256::from(f.gas)),
                0x5b => Flow::Continue,

                0x5c => {
                    let key = pop(&mut stack);
                    match key {
                        Ok(key) => {
                            if !self.spec.transient_storage {
                                Flow::Err(EvmError::InvalidOpcode(op))
                            } else {
                                push_flow(&mut stack, self.host.tload(&f.address, &key))
                            }
                        }
                        Err(e) => Flow::Err(e),
                    }
                }
                0x5d => {
                    let key = pop(&mut stack); let val = pop(&mut stack);
                    match (key, val) {
                        (Ok(key), Ok(val)) => {
                            if !self.spec.transient_storage {
                                Flow::Err(EvmError::InvalidOpcode(op))
                            } else if f.is_static {
                                Flow::Err(EvmError::StaticStateChange)
                            } else {
                                self.host.tstore(&f.address, &key, val);
                                Flow::Continue
                            }
                        }
                        _ => Flow::Err(EvmError::StackUnderflow),
                    }
                }
                0x5e => {
                    let dst = pop(&mut stack); let src = pop(&mut stack); let len = pop(&mut stack);
                    match (dst, src, len) {
                        (Ok(dst), Ok(src), Ok(len)) => {
                            if !self.spec.mcopy {
                                Flow::Err(EvmError::InvalidOpcode(op))
                            } else {
                                match self.mem_region(&mut memory, &mut f, dst, len)
                                    .and_then(|_| self.mem_region(&mut memory, &mut f, src, len))
                                {
                                    Ok(s_end) => {
                                        let cost = 3 + 3u64.saturating_mul(words(len.as_usize()));
                                        if self.charge(&mut f, cost).is_err() {
                                            Flow::Err(EvmError::OutOfGas)
                                        } else if len.as_usize() > 0 {
                                            memory.copy_within(src.as_usize()..s_end, dst.as_usize());
                                            Flow::Continue
                                        } else {
                                            Flow::Continue
                                        }
                                    }
                                    Err(e) => Flow::Err(e),
                                }
                            }
                        }
                        _ => Flow::Err(EvmError::StackUnderflow),
                    }
                }
                0x5f => {
                    if !self.spec.push0 {
                        Flow::Err(EvmError::InvalidOpcode(op))
                    } else {
                        push_flow(&mut stack, U256::zero())
                    }
                }

                0x60..=0x7f => {
                    let n = (op - 0x5f) as usize;
                    let mut bytes = [0u8; 32];
                    let end = (pc + 1 + n).min(f.code.len());
                    let avail = end - (pc + 1);
                    bytes[32 - n..32 - n + avail].copy_from_slice(&f.code[pc + 1..end]);
                    push_flow(&mut stack, U256::from_big_endian(&bytes))
                }
                0x80..=0x8f => {
                    let i = (op - 0x80) as usize;
                    if i >= stack.len() {
                        Flow::Err(EvmError::StackUnderflow)
                    } else {
                        let v = stack[stack.len() - 1 - i];
                        push_flow(&mut stack, v)
                    }
                }
                0x90..=0x9f => {
                    let i = (op - 0x90) as usize + 1;
                    let n = stack.len();
                    if i >= n {
                        Flow::Err(EvmError::StackUnderflow)
                    } else {
                        stack.swap(n - 1, n - 1 - i);
                        Flow::Continue
                    }
                }
                0xa0..=0xa4 => {
                    let ntopics = (op - 0xa0) as usize;
                    let len = pop(&mut stack); let off = pop(&mut stack);
                    let mut topics = Vec::new();
                    let mut bad = false;
                    for _ in 0..ntopics {
                        match pop(&mut stack) {
                            Ok(t) => topics.push(H256(rrx_util::u256_to_be32(&t))),
                            Err(_) => bad = true,
                        }
                    }
                    if bad {
                        Flow::Err(EvmError::StackUnderflow)
                    } else if f.is_static {
                        Flow::Err(EvmError::StaticStateChange)
                    } else {
                        match (off, len) {
                            (Ok(off), Ok(len)) => match self.mem_region(&mut memory, &mut f, off, len) {
                                Ok(end) => {
                                    let cost = 375u64.saturating_mul(1 + ntopics as u64) + 8u64.saturating_mul(len.as_usize() as u64);
                                    if self.charge(&mut f, cost).is_err() {
                                        Flow::Err(EvmError::OutOfGas)
                                    } else {
                                        self.host.emit_log(Log {
                                            address: f.address,
                                            topics,
                                            data: memory[off.as_usize()..end].to_vec(),
                                        });
                                        Flow::Continue
                                    }
                                }
                                Err(e) => Flow::Err(e),
                            },
                            _ => Flow::Err(EvmError::StackUnderflow),
                        }
                    }
                }

                0xf0 | 0xf5 => {
                    // CREATE:  value, offset, size        (value is on top)
                    // CREATE2: salt(top), size, offset, value
                    let salt = if op == 0xf5 { pop(&mut stack) } else { Ok(U256::zero()) };
                    let (value, off, len) = if op == 0xf5 {
                        let len = pop(&mut stack); let off = pop(&mut stack); let value = pop(&mut stack);
                        (value, off, len)
                    } else {
                        let value = pop(&mut stack); let off = pop(&mut stack); let len = pop(&mut stack);
                        (value, off, len)
                    };
                    match (value, off, len, salt) {
                        (Ok(value), Ok(off), Ok(len), Ok(salt)) => {
                            if f.is_static {
                                Flow::Err(EvmError::StaticStateChange)
                            } else {
                                match self.mem_region(&mut memory, &mut f, off, len) {
                                    Ok(end) => {
                                        let init: Vec<u8> = memory[off.as_usize()..end].to_vec();
                                        let mut cost = GAS_CREATE;
                                        if op == 0xf5 {
                                            cost += 6 * words(init.len());
                                        }
                                        if self.spec.eip3860 {
                                            cost += 2 * words(init.len());
                                        }
                                        if self.charge(&mut f, cost).is_err() {
                                            Flow::Err(EvmError::OutOfGas)
                                        } else if self.spec.eip3860 && init.len() > MAX_INIT_CODE {
                                            Flow::Err(EvmError::InitCodeTooLarge)
                                        } else {
                                            let child_gas = f.gas - f.gas / 64;
                                            f.gas -= child_gas;
                                            let s = if op == 0xf5 { Some(salt) } else { None };
                                            let out = self.do_create(f.address, value, init, s, child_gas, f.is_static, true);
                                            f.gas += out.gas_back;
                                            if out.success {
                                                push_flow(&mut stack, u256_from_h160(&out.address))
                                            } else {
                                                f.returndata = out.output;
                                                push_flow(&mut stack, U256::zero())
                                            }
                                        }
                                    }
                                    Err(e) => Flow::Err(e),
                                }
                            }
                        }
                        _ => Flow::Err(EvmError::StackUnderflow),
                    }
                }
                0xf1 | 0xf2 | 0xf4 | 0xfa => {
                    let kind = match op {
                        0xf1 => FrameKind::Call,
                        0xf2 => FrameKind::CallCode,
                        0xf4 => FrameKind::DelegateCall,
                        _ => FrameKind::StaticCall,
                    };
                    let gas_want = pop(&mut stack); let to = pop(&mut stack);
                    let value = if op == 0xf1 || op == 0xf2 { pop(&mut stack) } else { Ok(U256::zero()) };
                    let has_value = op == 0xf1 || op == 0xf2;
                    let in_off = pop(&mut stack); let in_len = pop(&mut stack);
                    let ret_off = pop(&mut stack); let ret_len = pop(&mut stack);
                    match (gas_want, to, in_off, in_len, ret_off, ret_len, value) {
                        (Ok(gas_want), Ok(to), Ok(in_off), Ok(in_len), Ok(ret_off), Ok(ret_len), Ok(value)) => {
                            let target = h160_from_u256(&to);
                            let in_end = match self.mem_region(&mut memory, &mut f, in_off, in_len) {
                                Ok(e) => e,
                                Err(e) => return self.abort_frame(host_cp, refund_before, e),
                            };
                            let _ret_end = match self.mem_region(&mut memory, &mut f, ret_off, ret_len) {
                                Ok(e) => e,
                                Err(e) => return self.abort_frame(host_cp, refund_before, e),
                            };
                            let acc = if self.spec.eip2929 { self.account_access_cost(&target) } else { 0 };
                            let mut cost = acc;
                            if has_value && value > U256::zero() {
                                cost += GAS_VALUE_TRANSFER;
                                if kind == FrameKind::Call && !self.host.exists(&target) {
                                    cost += GAS_NEW_ACCOUNT;
                                }
                            }
                            if f.is_static && kind == FrameKind::Call && value > U256::zero() {
                                return self.abort_frame(host_cp, refund_before, EvmError::StaticStateChange);
                            }
                            if self.charge(&mut f, cost).is_err() {
                                Flow::Err(EvmError::OutOfGas)
                            } else {
                                let mut child_gas = f.gas - f.gas / 64;
                                if gas_want.bits() <= 64 && gas_want.as_u64() < child_gas {
                                    child_gas = gas_want.as_u64();
                                }
                                f.gas -= child_gas;
                                let stipend = if has_value && value > U256::zero() { GAS_STIPEND } else { 0 };
                                let parent_paid = child_gas;
                                child_gas += stipend;
                                let input: Vec<u8> = memory[in_off.as_usize()..in_end].to_vec();
                                let (success, gas_back, output) = self.exec_subcall(
                                    kind, f.address, f.caller, f.value, target, value, input, child_gas, f.is_static,
                                );
                                f.gas += gas_back.min(parent_paid);
                                if success {
                                    let n = output.len().min(ret_len.as_usize());
                                    if n > 0 {
                                        memory[ret_off.as_usize()..ret_off.as_usize() + n].copy_from_slice(&output[..n]);
                                    }
                                    f.returndata = output;
                                    push_flow(&mut stack, U256::one())
                                } else {
                                    f.returndata = output;
                                    push_flow(&mut stack, U256::zero())
                                }
                            }
                        }
                        _ => Flow::Err(EvmError::StackUnderflow),
                    }
                }
                0xf3 => {
                    let off = pop(&mut stack); let len = pop(&mut stack);
                    match (off, len) {
                        (Ok(off), Ok(len)) => match self.mem_region(&mut memory, &mut f, off, len) {
                            Ok(end) => Flow::Return(memory[off.as_usize()..end].to_vec()),
                            Err(e) => Flow::Err(e),
                        },
                        _ => Flow::Err(EvmError::StackUnderflow),
                    }
                }
                0xfd => {
                    let off = pop(&mut stack); let len = pop(&mut stack);
                    match (off, len) {
                        (Ok(off), Ok(len)) => match self.mem_region(&mut memory, &mut f, off, len) {
                            Ok(end) => Flow::Revert(memory[off.as_usize()..end].to_vec()),
                            Err(e) => Flow::Err(e),
                        },
                        _ => Flow::Err(EvmError::StackUnderflow),
                    }
                }
                0xfe => Flow::Err(EvmError::InvalidOpcode(0xfe)),
                0xff => {
                    let ben = pop(&mut stack);
                    match ben {
                        Ok(ben) => {
                            if f.is_static {
                                Flow::Err(EvmError::StaticStateChange)
                            } else {
                                let beneficiary = h160_from_u256(&ben);
                                let cold = if self.spec.eip2929 { self.account_access_cost(&beneficiary) } else { 0 };
                                let mut cost = 5000 + cold;
                                if !self.host.exists(&beneficiary) && self.host.balance(&f.address) > U256::zero() {
                                    cost += GAS_NEW_ACCOUNT;
                                }
                                if self.charge(&mut f, cost).is_err() {
                                    Flow::Err(EvmError::OutOfGas)
                                } else {
                                    let balance = self.host.balance(&f.address);
                                    if balance > U256::zero() {
                                        self.host.add_balance(&beneficiary, balance);
                                        self.host.set_balance(&f.address, U256::zero());
                                    }
                                    if f.created_in_tx || self.created_in_tx.contains(&f.address) {
                                        self.host.delete_account(&f.address);
                                    }
                                    // EIP-3529 (London) removed the selfdestruct refund.
                                    if !self.spec.eip3529 {
                                        self.refund += GAS_SELFDESTRUCT_REFUND as i64;
                                    }
                                    Flow::SelfDestruct
                                }
                            }
                        }
                        Err(e) => Flow::Err(e),
                    }
                }
                other => Flow::Err(EvmError::InvalidOpcode(other)),
            };

            let gas_cost = gas_before - f.gas;
            let err_msg = match &flow {
                Flow::Err(e) => Some(e.to_string()),
                _ => None,
            };
            self.trace_step(op, pc, gas_before, gas_cost, &stack, &memory, err_msg);

            match flow {
                Flow::Continue => {
                    pc = next_pc;
                }
                Flow::Return(data) => {
                    return FrameResult { kind: ExitKind::Return, gas_remaining: f.gas, output: data, created_address: None };
                }
                Flow::Revert(data) => {
                    return self.revert_frame(host_cp, refund_before, f.gas, data);
                }
                Flow::SelfDestruct => {
                    return FrameResult { kind: ExitKind::SelfDestruct, gas_remaining: f.gas, output: vec![], created_address: None };
                }
                Flow::Err(e) => {
                    return self.abort_frame(host_cp, refund_before, e);
                }
            }
        }
    }

    fn op_sstore(&mut self, f: &mut Frame, key: U256, value: U256) -> Result<(), EvmError> {
        let current = self.host.storage(&f.address, &key);
        let original = self.host.original_storage(&f.address, &key);
        let clear_refund = if self.spec.eip3529 { GAS_REFUND_CLEAR_LONDON } else { GAS_REFUND_CLEAR_ISTANBUL };
        let reset_cost: u64 = if self.spec.eip2929 { GAS_WARM + 2800 } else { 5000 };

        let cost: u64 = if value == current {
            GAS_WARM
        } else if original == current {
            if original.is_zero() {
                GAS_SSTORE_SET
            } else {
                if value.is_zero() {
                    self.refund += clear_refund;
                }
                reset_cost
            }
        } else {
            let c = GAS_WARM;
            if !original.is_zero() {
                if current.is_zero() {
                    self.refund -= clear_refund;
                }
                if value.is_zero() {
                    self.refund += clear_refund;
                }
            }
            if original == value {
                if original.is_zero() {
                    self.refund += GAS_SSTORE_SET as i64 - 200;
                } else {
                    self.refund += reset_cost as i64 - GAS_WARM as i64;
                }
            }
            c
        };
        self.charge(f, cost)?;
        self.host.set_storage(&f.address, &key, value);
        Ok(())
    }
}

fn u256_from_h160(a: &H160) -> U256 {
    U256::from_big_endian(a.as_bytes())
}

fn h160_from_u256(v: &U256) -> H160 {
    let be = rrx_util::u256_to_be32(v);
    H160::from_slice(&be[12..])
}

fn u256_from_h256(h: &H256) -> U256 {
    U256::from_big_endian(h.as_bytes())
}

/// Copy `len` bytes from src[src_off..] into memory[dst..], zero-padding beyond src end.
fn copy_zeropad(memory: &mut [u8], dst: usize, src: &[u8], src_off: usize, len: usize, mem_end: usize) {
    for i in 0..len {
        let v = if src_off + i < src.len() { src[src_off + i] } else { 0 };
        if dst + i < mem_end && dst + i < memory.len() {
            memory[dst + i] = v;
        }
    }
}
