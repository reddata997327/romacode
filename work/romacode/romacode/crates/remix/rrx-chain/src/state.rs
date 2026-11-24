//! Trie-backed world state implementing the EVM `Host` trait with full journaling.

use std::collections::{BTreeMap, HashMap};

use rrx_evm::{Host, Log};
use rrx_trie::Trie;
use rrx_util::{keccak256, rlp::Rlp, H160, H256, KECCAK_EMPTY, U256};

#[derive(Clone, Debug, Default)]
pub struct Account {
    pub nonce: u64,
    pub balance: U256,
    pub storage: BTreeMap<U256, U256>,
    pub code_hash: H256,
}

impl Account {
    pub fn is_empty_eip161(&self) -> bool {
        self.nonce == 0 && self.balance.is_zero() && self.code_hash == KECCAK_EMPTY
    }
}

#[derive(Clone, Debug)]
enum Undo {
    Balance(H160, U256),
    Nonce(H160, u64),
    Storage(H160, U256, U256),
    Code(H160),
    Log,
    Transient(H160, U256, U256),
    Deleted(H160, Account),
    Created(H160),
}

/// Full world state: accounts, code, plus tx-scoped journal/transient/logs.
#[derive(Clone, Debug, Default)]
pub struct WorldState {
    pub accounts: HashMap<H160, Account>,
    pub code: HashMap<H256, Vec<u8>>,
    pub original_cache: HashMap<(H160, U256), U256>,
    pub transient: HashMap<(H160, U256), U256>,
    pub logs: Vec<Log>,
    /// recent block hashes for the BLOCKHASH opcode
    pub recent_block_hashes: HashMap<u64, H256>,
    journal: Vec<Undo>,
}

fn keccak_addr(a: &H160) -> [u8; 32] {
    let mut k = [0u8; 32];
    k[12..32].copy_from_slice(a.as_bytes());
    keccak256(&k)
}

fn keccak_slot(k: &U256) -> [u8; 32] {
    keccak256(&rrx_util::u256_to_be32(k))
}

/// Storage trie root for a set of (slot, value) pairs (secure trie, minimal RLP ints).
pub fn storage_trie_root(storage: &BTreeMap<U256, U256>) -> H256 {
    let mut trie = Trie::new();
    for (k, v) in storage {
        if !v.is_zero() {
            trie.insert(&keccak_slot(k), &rrx_util::rlp::u256_trimmed_be(v));
        }
    }
    trie.root_hash()
}

impl WorldState {
    pub fn ensure_account(&mut self, a: &H160) {
        self.accounts.entry(*a).or_default();
    }

    pub fn set_code_of(&mut self, a: &H160, code: Vec<u8>) {
        let hash = if code.is_empty() { KECCAK_EMPTY } else { H256(keccak256(&code)) };
        self.journal.push(Undo::Code(*a));
        let acc = self.accounts.entry(*a).or_default();
        acc.code_hash = hash;
        if !code.is_empty() {
            self.code.insert(hash, code);
        }
    }

    pub fn code_of(&self, a: &H160) -> Vec<u8> {
        let acc = self.accounts.get(a);
        match acc {
            Some(acc) if acc.code_hash != KECCAK_EMPTY => {
                self.code.get(&acc.code_hash).cloned().unwrap_or_default()
            }
            _ => vec![],
        }
    }

    /// Compute the account trie root (the real state root).
    pub fn state_root(&self) -> H256 {
        let mut trie = Trie::new();
        for (addr, acc) in &self.accounts {
            if acc.is_empty_eip161() {
                continue; // empty accounts are not in the trie
            }
            let sroot = storage_trie_root(&acc.storage);
            let val = Rlp::list(vec![
                Rlp::u64(acc.nonce),
                Rlp::u256(&acc.balance),
                Rlp::h256(&sroot),
                Rlp::h256(&acc.code_hash),
            ]);
            trie.insert(&keccak_addr(addr), &val.encode());
        }
        trie.root_hash()
    }

    pub fn clear_transient(&mut self) {
        self.transient.clear();
    }

    /// Direct funding (genesis / dev allocation) — outside any tx journal.
    pub fn fund(&mut self, address: &H160, value: U256) {
        let acc = self.accounts.entry(*address).or_default();
        acc.balance = value;
    }
}

impl Host for WorldState {
    fn balance(&self, address: &H160) -> U256 {
        self.accounts.get(address).map(|a| a.balance).unwrap_or_default()
    }
    fn nonce(&self, address: &H160) -> u64 {
        self.accounts.get(address).map(|a| a.nonce).unwrap_or(0)
    }
    fn code_hash(&self, address: &H160) -> H256 {
        self.accounts.get(address).map(|a| a.code_hash).unwrap_or(KECCAK_EMPTY)
    }
    fn code(&self, address: &H160) -> Vec<u8> {
        self.code_of(address)
    }
    fn storage(&self, address: &H160, key: &U256) -> U256 {
        self.accounts
            .get(address)
            .and_then(|a| a.storage.get(key))
            .copied()
            .unwrap_or_default()
    }
    fn original_storage(&self, address: &H160, key: &U256) -> U256 {
        if let Some(v) = self.original_cache.get(&(*address, *key)) {
            return *v;
        }
        self.storage(address, key)
    }
    fn exists(&self, address: &H160) -> bool {
        self.accounts
            .get(address)
            .map(|a| !a.is_empty_eip161())
            .unwrap_or(false)
    }
    fn block_hash(&self, number: u64) -> Option<H256> {
        self.recent_block_hashes.get(&number).copied()
    }

    fn set_balance(&mut self, address: &H160, value: U256) {
        self.journal.push(Undo::Balance(*address, self.balance(address)));
        let acc = self.accounts.entry(*address).or_default();
        acc.balance = value;
    }
    fn set_nonce(&mut self, address: &H160, nonce: u64) {
        self.journal.push(Undo::Nonce(*address, self.nonce(address)));
        let acc = self.accounts.entry(*address).or_default();
        acc.nonce = nonce;
    }
    fn inc_nonce(&mut self, address: &H160) -> u64 {
        let n = self.nonce(address) + 1;
        self.set_nonce(address, n);
        n
    }
    fn set_storage(&mut self, address: &H160, key: &U256, value: U256) {
        let cur = self.storage(address, key);
        self.original_cache.entry((*address, *key)).or_insert(cur);
        self.journal.push(Undo::Storage(*address, *key, cur));
        let acc = self.accounts.entry(*address).or_default();
        acc.storage.insert(*key, value);
    }
    fn set_code(&mut self, address: &H160, code: Vec<u8>) {
        self.set_code_of(address, code);
    }
    fn delete_account(&mut self, address: &H160) {
        let snapshot = self
            .accounts
            .get(address)
            .cloned()
            .unwrap_or_default();
        self.journal.push(Undo::Deleted(*address, snapshot));
        let acc = self.accounts.entry(*address).or_default();
        acc.storage.clear();
        acc.code_hash = KECCAK_EMPTY;
        // balance handled by the executor (selfdestruct transfer) before this call
    }
    fn emit_log(&mut self, log: Log) {
        self.journal.push(Undo::Log);
        self.logs.push(log);
    }
    fn tstore(&mut self, address: &H160, key: &U256, value: U256) {
        self.journal
            .push(Undo::Transient(*address, *key, self.tload(address, key)));
        self.transient.insert((*address, *key), value);
    }
    fn tload(&self, address: &H160, key: &U256) -> U256 {
        self.transient.get(&(*address, *key)).copied().unwrap_or_default()
    }
    fn checkpoint(&mut self) -> u64 {
        self.journal.len() as u64
    }
    fn rollback(&mut self, cp: u64) {
        while self.journal.len() as u64 > cp {
            match self.journal.pop().unwrap() {
                Undo::Balance(a, v) => {
                    self.accounts.entry(a).or_default().balance = v;
                }
                Undo::Nonce(a, v) => {
                    self.accounts.entry(a).or_default().nonce = v;
                }
                Undo::Storage(a, k, v) => {
                    let acc = self.accounts.entry(a).or_default();
                    if v.is_zero() {
                        acc.storage.remove(&k);
                    } else {
                        acc.storage.insert(k, v);
                    }
                }
                Undo::Code(a) => {
                    if let Some(acc) = self.accounts.get_mut(&a) {
                        acc.code_hash = KECCAK_EMPTY;
                    }
                }
                Undo::Log => {
                    self.logs.pop();
                }
                Undo::Transient(a, k, v) => {
                    self.transient.insert((a, k), v);
                }
                Undo::Deleted(a, snap) => {
                    self.accounts.insert(a, snap);
                }
                Undo::Created(a) => {
                    // nothing to undo; kept for traceability
                    let _ = a;
                }
            }
        }
    }
}
