//! Real EVM behavior tests against an in-memory journaled state.

use std::collections::HashMap;

use rrx_evm::*;
use rrx_evm::Machine;
use sha2::Digest;
use rrx_util::{from_hex, keccak256, H160, H256, U256};

#[derive(Clone, Debug)]
enum Undo {
    Bal(H160, U256),
    Nonce(H160, u64),
    Code(H160),
    Storage(H160, U256, U256),
    TStore(H160, U256, U256),
    Log,
    Deleted(H160, U256, u64, Vec<u8>),
}

#[derive(Default)]
struct MockState {
    balances: HashMap<H160, U256>,
    nonces: HashMap<H160, u64>,
    code: HashMap<H160, Vec<u8>>,
    storage: HashMap<(H160, U256), U256>,
    transient: HashMap<(H160, U256), U256>,
    logs: Vec<Log>,
    journal: Vec<Undo>,
}

impl MockState {
    fn fund(&mut self, a: H160, v: U256) {
        self.balances.insert(a, v);
    }
}

impl Host for MockState {
    fn balance(&self, a: &H160) -> U256 {
        *self.balances.get(a).unwrap_or(&U256::zero())
    }
    fn nonce(&self, a: &H160) -> u64 {
        *self.nonces.get(a).unwrap_or(&0)
    }
    fn code_hash(&self, a: &H160) -> H256 {
        match self.code.get(a) {
            Some(c) if !c.is_empty() => H256(keccak256(c)),
            _ => rrx_util::KECCAK_EMPTY,
        }
    }
    fn code(&self, a: &H160) -> Vec<u8> {
        self.code.get(a).cloned().unwrap_or_default()
    }
    fn storage(&self, a: &H160, k: &U256) -> U256 {
        *self.storage.get(&(*a, *k)).unwrap_or(&U256::zero())
    }
    fn original_storage(&self, a: &H160, k: &U256) -> U256 {
        *self.storage.get(&(*a, *k)).unwrap_or(&U256::zero())
    }
    fn exists(&self, a: &H160) -> bool {
        self.balance(a) > U256::zero() || self.nonce(a) > 0 || self.code(a).len() > 0
    }
    fn block_hash(&self, n: u64) -> Option<H256> {
        Some(H256::from_low_u64_be(n))
    }
    fn set_balance(&mut self, a: &H160, v: U256) {
        self.journal.push(Undo::Bal(*a, self.balance(a)));
        self.balances.insert(*a, v);
    }
    fn set_nonce(&mut self, a: &H160, n: u64) {
        self.journal.push(Undo::Nonce(*a, self.nonce(a)));
        self.nonces.insert(*a, n);
    }
    fn inc_nonce(&mut self, a: &H160) -> u64 {
        let n = self.nonce(a) + 1;
        self.set_nonce(a, n);
        n
    }
    fn set_storage(&mut self, a: &H160, k: &U256, v: U256) {
        self.journal.push(Undo::Storage(*a, *k, self.storage(a, k)));
        self.storage.insert((*a, *k), v);
    }
    fn set_code(&mut self, a: &H160, c: Vec<u8>) {
        self.journal.push(Undo::Code(*a));
        self.code.insert(*a, c);
    }
    fn delete_account(&mut self, a: &H160) {
        self.journal.push(Undo::Deleted(*a, self.balance(a), self.nonce(a), self.code(a)));
        self.balances.insert(*a, U256::zero());
        self.code.insert(*a, vec![]);
    }
    fn emit_log(&mut self, l: Log) {
        self.journal.push(Undo::Log);
        self.logs.push(l);
    }
    fn tstore(&mut self, a: &H160, k: &U256, v: U256) {
        self.journal.push(Undo::TStore(*a, *k, self.tload(a, k)));
        self.transient.insert((*a, *k), v);
    }
    fn tload(&self, a: &H160, k: &U256) -> U256 {
        *self.transient.get(&(*a, *k)).unwrap_or(&U256::zero())
    }
    fn checkpoint(&mut self) -> u64 {
        self.journal.len() as u64
    }
    fn rollback(&mut self, cp: u64) {
        while self.journal.len() as u64 > cp {
            match self.journal.pop().unwrap() {
                Undo::Bal(a, v) => {
                    self.balances.insert(a, v);
                }
                Undo::Nonce(a, v) => {
                    self.nonces.insert(a, v);
                }
                Undo::Code(a) => {
                    self.code.remove(&a);
                }
                Undo::Storage(a, k, v) => {
                    self.storage.insert((a, k), v);
                }
                Undo::TStore(a, k, v) => {
                    self.transient.insert((a, k), v);
                }
                Undo::Log => {
                    self.logs.pop();
                }
                Undo::Deleted(a, bal, nonce, code) => {
                    self.balances.insert(a, bal);
                    self.nonces.insert(a, nonce);
                    self.code.insert(a, code);
                }
            }
        }
    }
}

fn alice() -> H160 {
    rrx_util::h160_from_hex("1000000000000000000000000000000000000001").unwrap()
}

const TARGET: &str = "2000000000000000000000000000000000000002";

fn run(code: &[u8], gas: u64) -> (MockState, ExecutionResult) {
    let mut state = MockState::default();
    state.fund(alice(), U256::from(1_000_000_000_000_000_000u64));
    let target = rrx_util::h160_from_hex(TARGET).unwrap();
    state.code.insert(target, code.to_vec());
    let tx = TxEnv {
        caller: alice(),
        gas_price: U256::one(),
        value: U256::zero(),
        data: vec![],
        gas_limit: gas,
        chain_id: 31337,
        to: Some(target),
        access_list: vec![],
        blob_hashes: vec![],
    };
    let machine = Machine::new(&mut state, Spec::shanghai(), BlockEnv::default(), tx, false);
    let res = machine.run_tx();
    (state, res)
}

#[test]
fn return_value_and_exact_gas() {
    // PUSH1 42; PUSH1 0; MSTORE; PUSH1 32; PUSH1 0; RETURN
    let code = from_hex("602a60005260206000f3").unwrap();
    let (_s, res) = run(&code, 100_000);
    assert!(res.is_success(), "expected success, got {:?}", res.kind);
    let mut expected = vec![0u8; 32];
    expected[31] = 42;
    assert_eq!(res.output, expected);
    // PUSH1*4 = 12, MSTORE = 3 + 3 (1 word mem), RETURN = 0
    assert_eq!(res.gas_used, 18);
}

#[test]
fn sstore_first_write_gas() {
    let code = from_hex("602a600055").unwrap();
    let (s, res) = run(&code, 100_000);
    assert!(res.is_success());
    let addr = rrx_util::h160_from_hex(TARGET).unwrap();
    assert_eq!(s.storage.get(&(addr, U256::zero())).copied().unwrap_or_default(), U256::from(42u8));
    // PUSH1*2 = 6, cold slot 2100, SSTORE set = 20000
    assert_eq!(res.gas_used, 6 + 2100 + 20000);
}

#[test]
fn revert_rolls_back_storage() {
    let code = from_hex("602a60005560006000fd").unwrap();
    let (s, res) = run(&code, 100_000);
    assert!(matches!(res.kind, ExitKind::Revert));
    let addr = rrx_util::h160_from_hex(TARGET).unwrap();
    assert_eq!(s.storage.get(&(addr, U256::zero())).copied().unwrap_or_default(), U256::zero());
    assert!(res.gas_remaining > 0);
}

#[test]
fn call_sha256_precompile() {
    let target = H160::from_low_u64_be(2);
    let mut code: Vec<u8> = vec![];
    code.extend_from_slice(&[0x7f]);
    let mut word = [0u8; 32];
    word[29..32].copy_from_slice(b"abc");
    code.extend_from_slice(&word);
    code.extend_from_slice(&[0x60, 0x00, 0x52]);
    code.extend_from_slice(&[0x60, 0x20, 0x60, 0x00, 0x60, 0x03, 0x60, 0x1d, 0x60, 0x00]);
    code.extend_from_slice(&[0x73]);
    code.extend_from_slice(target.as_bytes());
    code.extend_from_slice(&[0x61, 0x86, 0xa0]); // PUSH2 0x86a0 gas
    code.extend_from_slice(&[0xf1]);
    code.extend_from_slice(&[0x60, 0x20, 0x60, 0x00, 0xf3]);

    let (_s, res) = run(&code, 1_000_000);
    assert!(res.is_success(), "got {:?}", res.kind);
    let mut h = sha2::Sha256::new();
    h.update(b"abc");
    assert_eq!(res.output, h.finalize().to_vec());
}

#[test]
fn deploy_via_create_tx() {
    let runtime = from_hex("602a60005260206000f3").unwrap();
    let mut init = from_hex("600a600c600039600a6000f3").unwrap();
    init.extend_from_slice(&runtime);

    let mut state = MockState::default();
    state.fund(alice(), U256::from(1_000_000_000_000_000_000u64));
    let tx = TxEnv {
        caller: alice(),
        gas_price: U256::one(),
        value: U256::zero(),
        data: init,
        gas_limit: 500_000,
        chain_id: 31337,
        to: None,
        access_list: vec![],
        blob_hashes: vec![],
    };
    state.inc_nonce(&alice()); // chain increments sender nonce before execution
    let machine = Machine::new(&mut state, Spec::shanghai(), BlockEnv::default(), tx, false);
    let res = machine.run_tx();
    assert!(res.is_success(), "create failed: {:?}", res.kind);
    let created = res.created_address.unwrap();
    assert_ne!(created, H160::zero());
    assert_eq!(state.code(&created), runtime);
    assert_eq!(state.nonce(&alice()), 1); // EIP-161: exactly one increment per tx
}

#[test]
fn stack_underflow_burns_gas() {
    let code = vec![0x01u8]; // ADD with empty stack
    let (_s, res) = run(&code, 50_000);
    assert!(matches!(res.kind, ExitKind::Error(EvmError::StackUnderflow)));
    assert_eq!(res.gas_remaining, 0);
}

#[test]
fn jump_to_jumpdest() {
    let code = from_hex("6003565b00").unwrap();
    let (_s, res) = run(&code, 50_000);
    assert!(res.is_success());
}

#[test]
fn jump_into_push_data_fails() {
    // target 4 points at push-data byte 0x5b (not a real JUMPDEST)
    let code = from_hex("600456605b00").unwrap();
    let (_s, res) = run(&code, 50_000);
    assert!(matches!(res.kind, ExitKind::Error(_)));
}

#[test]
fn keccak256_correct() {
    let mut code: Vec<u8> = vec![0x7f];
    let mut word = [0u8; 32];
    word[29..32].copy_from_slice(b"abc");
    code.extend_from_slice(&word);
    code.extend_from_slice(&[0x60, 0x00, 0x52, 0x60, 0x20, 0x60, 0x00, 0x20]);
    code.extend_from_slice(&[0x60, 0x00, 0x52, 0x60, 0x20, 0x60, 0x00, 0xf3]);
    let (_s, res) = run(&code, 100_000);
    assert!(res.is_success());
    assert_eq!(res.output, keccak256(&word).to_vec());
}

#[test]
fn trace_records_gas_by_opcode() {
    let code = from_hex("602a60005260206000f3").unwrap();
    let mut state = MockState::default();
    state.fund(alice(), U256::from(1_000_000_000_000_000_000u64));
    let target = rrx_util::h160_from_hex(TARGET).unwrap();
    state.code.insert(target, code);
    let tx = TxEnv {
        caller: alice(),
        gas_limit: 100_000,
        to: Some(target),
        ..Default::default()
    };
    let mut machine = Machine::new(&mut state, Spec::shanghai(), BlockEnv::default(), tx, true);
    if let Some(t) = machine.tracer.as_mut() {
        t.record_stack = true;
    }
    let res = machine.run_tx();
    let trace = res.trace.expect("trace recorded");
    assert!(trace.steps.len() >= 6);
    assert_eq!(trace.gas_by_opcode.get(&0x60), Some(&12u128));
    assert!(trace.total_gas >= 18);
    let last = trace.steps.last().unwrap();
    assert_eq!(last.opcode, 0xf3);
}
