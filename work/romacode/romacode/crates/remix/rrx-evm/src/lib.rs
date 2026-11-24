//! rrx-evm: a real, spec-compliant EVM interpreter (Istanbul → Cancun feature set).
//!
//! Implements the full instruction set with the real gas schedule:
//! EIP-2200 (SSTORE), EIP-2929 (warm/cold access), EIP-3529 (refunds), EIP-3541 (0xEF),
//! EIP-3855 (PUSH0), EIP-3860 (initcode limit), EIP-1153 (transient storage),
//! EIP-5656 (MCOPY), EIP-150 (63/64), EIP-161, EIP-684, EIP-211 (returndata).

pub mod interp;

pub use interp::{opcode_name, tx_intrinsic_gas, words as interp_words, Frame, FrameKind, Machine, Step, Tracer};

use std::sync::Mutex;

use rrx_util::{keccak256, rlp::Rlp, H160, H256, U256};

// ---------------------------------------------------------------------------
// coverage collector: every top-level (target_code_hash, selector) executed.
// Populated by Machine::run_tx; drained by the rrx coverage tool.
// ---------------------------------------------------------------------------
static COVERAGE: Mutex<Vec<(H256, [u8; 4])>> = Mutex::new(Vec::new());

pub fn coverage_note(code_hash: H256, selector: [u8; 4]) {
    if let Ok(mut v) = COVERAGE.lock() {
        if !v.contains(&(code_hash, selector)) {
            v.push((code_hash, selector));
        }
    }
}

pub fn coverage_drain() -> Vec<(H256, [u8; 4])> {
    COVERAGE.lock().map(|mut v| std::mem::take(&mut *v)).unwrap_or_default()
}

// ---------------------------------------------------------------- Spec

#[derive(Clone, Debug, PartialEq)]
pub struct Spec {
    pub eip2929: bool,      // warm/cold access (Berlin)
    pub eip2200: bool,      // SSTORE semantics (Istanbul)
    pub eip3529: bool,      // refund schedule (London)
    pub eip3541: bool,      // reject 0xEF code (London)
    pub push0: bool,        // EIP-3855 (Shanghai)
    pub eip3860: bool,      // initcode word cost + limit (Shanghai)
    pub transient_storage: bool, // EIP-1153 (Cancun)
    pub mcopy: bool,        // EIP-5656 (Cancun)
    pub blob_ops: bool,     // EIP-4844 (Cancun)
    pub prevrandao: bool,   // EIP-4399 (Paris): DIFFICULTY returns mixHash
}

impl Spec {
    pub fn istanbul() -> Self {
        Spec {
            eip2929: false,
            eip2200: true,
            eip3529: false,
            eip3541: false,
            push0: false,
            eip3860: false,
            transient_storage: false,
            mcopy: false,
            blob_ops: false,
            prevrandao: false,
        }
    }
    pub fn berlin() -> Self {
        let mut s = Spec::istanbul();
        s.eip2929 = true;
        s
    }
    pub fn london() -> Self {
        let mut s = Spec::berlin();
        s.eip3529 = true;
        s.eip3541 = true;
        s
    }
    pub fn paris() -> Self {
        let mut s = Spec::london();
        s.prevrandao = true;
        s
    }
    pub fn shanghai() -> Self {
        let mut s = Spec::paris();
        s.push0 = true;
        s.eip3860 = true;
        s
    }
    pub fn cancun() -> Self {
        let mut s = Spec::shanghai();
        s.transient_storage = true;
        s.mcopy = true;
        s.blob_ops = true;
        s
    }

    pub fn from_name(name: &str) -> Result<Spec, String> {
        match name.to_ascii_lowercase().as_str() {
            "istanbul" => Ok(Spec::istanbul()),
            "berlin" => Ok(Spec::berlin()),
            "london" => Ok(Spec::london()),
            "paris" | "merge" => Ok(Spec::paris()),
            "shanghai" => Ok(Spec::shanghai()),
            "cancun" => Ok(Spec::cancun()),
            other => Err(format!("unknown spec: {other} (supported: istanbul..cancun)")),
        }
    }
}

// ---------------------------------------------------------------- Environment

#[derive(Clone, Debug)]
pub struct BlockEnv {
    pub number: u64,
    pub coinbase: H160,
    pub timestamp: u64,
    pub gas_limit: u64,
    pub base_fee: U256,
    /// difficulty / prevrandao (EIP-4399)
    pub prevrandao: H256,
    pub excess_blob_gas: u64,
}

impl Default for BlockEnv {
    fn default() -> Self {
        BlockEnv {
            number: 1,
            coinbase: H160::zero(),
            timestamp: 1,
            gas_limit: 30_000_000,
            base_fee: U256::from(1_000_000_000u64),
            prevrandao: H256::zero(),
            excess_blob_gas: 0,
        }
    }
}

#[derive(Clone, Debug)]
pub struct TxEnv {
    pub caller: H160,
    /// effective gas price (max_fee for 1559 execution)
    pub gas_price: U256,
    pub value: U256,
    pub data: Vec<u8>,
    pub gas_limit: u64,
    pub chain_id: u64,
    pub to: Option<H160>,
    pub access_list: Vec<(H160, Vec<U256>)>,
    pub blob_hashes: Vec<H256>,
}

impl Default for TxEnv {
    fn default() -> Self {
        TxEnv {
            caller: H160::zero(),
            gas_price: U256::zero(),
            value: U256::zero(),
            data: vec![],
            gas_limit: 30_000_000,
            chain_id: 1,
            to: None,
            access_list: vec![],
            blob_hashes: vec![],
        }
    }
}

// ---------------------------------------------------------------- State interface

#[derive(Clone, Debug)]
pub struct Log {
    pub address: H160,
    pub topics: Vec<H256>,
    pub data: Vec<u8>,
}

/// Read/write interface the EVM executes against. The implementing chain owns
/// real state (tries) and journals every write so frames can be rolled back.
pub trait Host {
    fn balance(&self, address: &H160) -> U256;
    fn nonce(&self, address: &H160) -> u64;
    fn code_hash(&self, address: &H160) -> H256;
    fn code(&self, address: &H160) -> Vec<u8>;
    fn storage(&self, address: &H160, key: &U256) -> U256;
    /// Storage value at the START of the current transaction (EIP-2200 "original").
    fn original_storage(&self, address: &H160, key: &U256) -> U256;
    /// EIP-161 existence: nonce > 0 || balance > 0 || code present.
    fn exists(&self, address: &H160) -> bool;
    fn block_hash(&self, number: u64) -> Option<H256>;

    fn set_balance(&mut self, address: &H160, value: U256);
    fn add_balance(&mut self, address: &H160, delta: U256) {
        // delta may be a two's-complement negative (0 - value)
        let b = self.balance(address);
        self.set_balance(address, b.overflowing_add(delta).0);
    }
    fn set_nonce(&mut self, address: &H160, nonce: u64);
    /// Returns the new nonce.
    fn inc_nonce(&mut self, address: &H160) -> u64;
    fn set_storage(&mut self, address: &H160, key: &U256, value: U256);
    fn set_code(&mut self, address: &H160, code: Vec<u8>);
    /// Full deletion used only for contracts created in the same tx (EIP-6780).
    fn delete_account(&mut self, address: &H160);
    fn emit_log(&mut self, log: Log);

    fn tstore(&mut self, address: &H160, key: &U256, value: U256);
    fn tload(&self, address: &H160, key: &U256) -> U256;

    /// Journal control: checkpoint / rollback to it. Commit is a no-op marker.
    fn checkpoint(&mut self) -> u64;
    fn rollback(&mut self, checkpoint: u64);
    fn commit(&mut self) {}
}

// ---------------------------------------------------------------- addresses

/// keccak(rlp([sender, nonce]))[12..] — contract address for CREATE.
pub fn create_address(sender: &H160, nonce: u64) -> H160 {
    let rlp = Rlp::list(vec![Rlp::h160(sender), Rlp::u64(nonce)]);
    let h = keccak256(&rlp.encode());
    H160::from_slice(&h[12..])
}

/// keccak(0xff || sender || salt || keccak(init_code))[12..] — CREATE2.
pub fn create2_address(sender: &H160, salt: &H256, init_code_hash: &H256) -> H160 {
    let mut buf = Vec::with_capacity(85);
    buf.push(0xff);
    buf.extend_from_slice(sender.as_bytes());
    buf.extend_from_slice(salt.as_bytes());
    buf.extend_from_slice(init_code_hash.as_bytes());
    H160::from_slice(&keccak256(&buf)[12..])
}

// ---------------------------------------------------------------- results

#[derive(Clone, Debug, PartialEq)]
pub enum ExitKind {
    Stop,
    Return,
    Revert,
    SelfDestruct,
    Error(EvmError),
}

#[derive(Clone, Debug, PartialEq)]
pub enum EvmError {
    OutOfGas,
    StackOverflow,
    StackUnderflow,
    InvalidJumpDestination,
    InvalidJumpDestMarker,
    InvalidOpcode(u8),
    StaticStateChange,
    CallTooDeep,
    OutOfBounds,
    MemoryOOG,
    CodeTooLarge,
    InvalidCode,
    InitCodeTooLarge,
}

impl std::fmt::Display for EvmError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            EvmError::OutOfGas => write!(f, "out of gas"),
            EvmError::StackOverflow => write!(f, "stack overflow"),
            EvmError::StackUnderflow => write!(f, "stack underflow"),
            EvmError::InvalidJumpDestination => write!(f, "invalid jump destination"),
            EvmError::InvalidJumpDestMarker => write!(f, "jump destination not marked with JUMPDEST"),
            EvmError::InvalidOpcode(op) => write!(f, "invalid opcode 0x{op:02x}"),
            EvmError::StaticStateChange => write!(f, "static call state modification"),
            EvmError::CallTooDeep => write!(f, "call depth exhausted (1024)"),
            EvmError::OutOfBounds => write!(f, "out of bounds read"),
            EvmError::MemoryOOG => write!(f, "memory expansion out of gas"),
            EvmError::CodeTooLarge => write!(f, "contract code size exceeds 24576 bytes (EIP-170)"),
            EvmError::InvalidCode => write!(f, "invalid contract code (EIP-3541)"),
            EvmError::InitCodeTooLarge => write!(f, "initcode size exceeds limit (EIP-3860)"),
        }
    }
}

#[derive(Clone, Debug)]
pub struct FrameResult {
    pub kind: ExitKind,
    pub gas_remaining: u64,
    pub output: Vec<u8>,
    pub created_address: Option<H160>,
}

#[derive(Clone, Debug)]
pub struct ExecutionResult {
    pub kind: ExitKind,
    pub gas_used: u64,
    pub gas_remaining: u64,
    pub output: Vec<u8>,
    pub created_address: Option<H160>,
    pub refund: i64,
    pub trace: Option<interp::Tracer>,
}

impl ExecutionResult {
    pub fn is_success(&self) -> bool {
        matches!(self.kind, ExitKind::Stop | ExitKind::Return | ExitKind::SelfDestruct)
    }
    pub fn revert_reason(&self) -> Option<String> {
        if matches!(self.kind, ExitKind::Revert) {
            rrx_util::abi::decode_revert_reason(&self.output)
        } else {
            None
        }
    }
}
