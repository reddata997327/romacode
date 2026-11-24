//! RemoteHost: run the rrx EVM interpreter DIRECTLY against live network state.
//!
//! Every read (balance / nonce / code / storage) is a real JSON-RPC round trip
//! to the node, cached after first use; every write is journaled in-memory so
//! frames can roll back exactly like the local chain. This is what makes
//! replaying a REAL mainnet transaction in OUR interpreter possible — no
//! simulation, the actual state of the actual chain.

use std::cell::RefCell;
use std::collections::{HashMap, HashSet};

use rrx_evm::{Host, Log, Spec};
use rrx_util::KECCAK_EMPTY;
use rrx_rpc::Provider;
use rrx_util::{h256_from_hex, u256_from_str, H160, H256, U256};

#[derive(Default, Clone)]
struct Overlay {
    balances: HashMap<H160, U256>,
    nonces: HashMap<H160, u64>,
    codes: HashMap<H160, Vec<u8>>,
    storage: HashMap<(H160, U256), U256>,
    deleted: HashSet<H160>,
    transient: HashMap<(H160, U256), U256>,
}


pub struct RemoteHost {
    pub p: Provider,
    /// block tag all reads are served from (the PRE-tx block when replaying)
    pub block: String,
    /// mainnet fork boundary for this block
    pub spec: Spec,
    accounts: RefCell<HashMap<H160, (U256, u64)>>,
    codes: RefCell<HashMap<H160, Vec<u8>>>,
    storage: RefCell<HashMap<(H160, U256), U256>>,
    block_hashes: RefCell<HashMap<u64, H256>>,
    journal: RefCell<Overlay>,
    collected_logs: RefCell<Vec<Log>>,
    checkpoints: RefCell<Vec<(u64, Overlay)>>,
    next_checkpoint: RefCell<u64>,
    /// stats for the final report
    pub rpc_reads: RefCell<usize>,
}

impl RemoteHost {
    pub fn new(p: Provider, block: String, spec: Spec) -> RemoteHost {
        RemoteHost {
            p,
            block,
            spec,
            accounts: RefCell::new(HashMap::new()),
            codes: RefCell::new(HashMap::new()),
            storage: RefCell::new(HashMap::new()),
            block_hashes: RefCell::new(HashMap::new()),
            journal: RefCell::new(Overlay::default()),
            collected_logs: RefCell::new(Vec::new()),
            checkpoints: RefCell::new(Vec::new()),
            next_checkpoint: RefCell::new(0),
            rpc_reads: RefCell::new(0),
        }
    }

    fn fetch_account(&self, addr: &H160) -> (U256, u64) {
        if let Some(hit) = self.accounts.borrow().get(addr) {
            return *hit;
        }
        *self.rpc_reads.borrow_mut() += 2;
        let balance = self.p.balance(addr, &self.block).unwrap_or_default();
        let nonce = self.p.nonce(addr, &self.block).unwrap_or(0);
        let entry = (balance, nonce);
        self.accounts.borrow_mut().insert(*addr, entry);
        entry
    }

    fn fetch_code(&self, addr: &H160) -> Vec<u8> {
        if let Some(hit) = self.codes.borrow().get(addr) {
            return hit.clone();
        }
        *self.rpc_reads.borrow_mut() += 1;
        let code = self.p.code(addr, &self.block).unwrap_or_default();
        self.codes.borrow_mut().insert(*addr, code.clone());
        code
    }

    fn fetch_storage(&self, addr: &H160, key: &U256) -> U256 {
        let k = (*addr, *key);
        if let Some(hit) = self.storage.borrow().get(&k) {
            return *hit;
        }
        *self.rpc_reads.borrow_mut() += 1;
        let v = self.p.storage_at(addr, key, &self.block).unwrap_or_default();
        self.storage.borrow_mut().insert(k, v);
        v
    }

    /// value of a slot in the CURRENT (journaled) state
    fn live_storage(&self, addr: &H160, key: &U256) -> U256 {
        if let Some(v) = self.journal.borrow().storage.get(&(*addr, *key)) {
            return *v;
        }
        self.fetch_storage(addr, key)
    }
}

impl Host for RemoteHost {
    fn balance(&self, address: &H160) -> U256 {
        if let Some(b) = self.journal.borrow().balances.get(address) {
            return *b;
        }
        self.fetch_account(address).0
    }
    fn nonce(&self, address: &H160) -> u64 {
        if let Some(n) = self.journal.borrow().nonces.get(address) {
            return *n;
        }
        self.fetch_account(address).1
    }
    fn code_hash(&self, address: &H160) -> H256 {
        let code = self.code(address);
        if code.is_empty() {
            KECCAK_EMPTY
        } else {
            H256::from(rrx_util::keccak256(&code))
        }
    }
    fn code(&self, address: &H160) -> Vec<u8> {
        if let Some(c) = self.journal.borrow().codes.get(address) {
            return c.clone();
        }
        self.fetch_code(address)
    }
    fn storage(&self, address: &H160, key: &U256) -> U256 {
        self.live_storage(address, key)
    }
    /// During a replay the remote node state IS the pre-tx original state.
    fn original_storage(&self, address: &H160, key: &U256) -> U256 {
        self.fetch_storage(address, key)
    }
    fn exists(&self, address: &H160) -> bool {
        if self.journal.borrow().deleted.contains(address) {
            return false;
        }
        let (balance, nonce) = self.fetch_account(address);
        let has_code = !self.fetch_code(address).is_empty();
        nonce > 0 || balance > U256::zero() || has_code
    }
    fn block_hash(&self, number: u64) -> Option<H256> {
        if let Some(h) = self.block_hashes.borrow().get(&number) {
            return Some(*h);
        }
        *self.rpc_reads.borrow_mut() += 1;
        let b = self.p.block_by_number(number, false).ok()??;
        let s = b.get("hash").and_then(|x| x.as_str())?;
        let h = h256_from_hex(s).ok()?;
        self.block_hashes.borrow_mut().insert(number, h);
        Some(h)
    }

    fn set_balance(&mut self, address: &H160, value: U256) {
        self.journal.borrow_mut().balances.insert(*address, value);
    }
    fn set_nonce(&mut self, address: &H160, nonce: u64) {
        self.journal.borrow_mut().nonces.insert(*address, nonce);
    }
    fn inc_nonce(&mut self, address: &H160) -> u64 {
        let n = self.nonce(address) + 1;
        self.set_nonce(address, n);
        n
    }
    fn set_storage(&mut self, address: &H160, key: &U256, value: U256) {
        self.journal
            .borrow_mut()
            .storage
            .insert((*address, *key), value);
    }
    fn set_code(&mut self, address: &H160, code: Vec<u8>) {
        self.journal.borrow_mut().codes.insert(*address, code);
    }
    fn delete_account(&mut self, address: &H160) {
        let mut j = self.journal.borrow_mut();
        j.deleted.insert(*address);
        j.storage.retain(|(a, _), _| a != address);
        j.balances.insert(*address, U256::zero());
    }
    fn emit_log(&mut self, log: Log) {
        self.collected_logs.borrow_mut().push(log);
    }
    fn tstore(&mut self, address: &H160, key: &U256, value: U256) {
        self.journal
            .borrow_mut()
            .transient
            .insert((*address, *key), value);
    }
    fn tload(&self, address: &H160, key: &U256) -> U256 {
        self.journal
            .borrow()
            .transient
            .get(&(*address, *key))
            .copied()
            .unwrap_or_default()
    }
    fn checkpoint(&mut self) -> u64 {
        let id = *self.next_checkpoint.borrow();
        *self.next_checkpoint.borrow_mut() += 1;
        self.checkpoints.borrow_mut().push((id, self.journal.borrow().clone()));
        id
    }
    fn rollback(&mut self, checkpoint: u64) {
        let mut cps = self.checkpoints.borrow_mut();
        if let Some(pos) = cps.iter().rposition(|(id, _)| *id == checkpoint) {
            let (_, snapshot) = cps.split_off(pos).into_iter().next().unwrap();
            *self.journal.borrow_mut() = snapshot;
            cps.pop();
        }
    }
    fn commit(&mut self) {
        let mut cps = self.checkpoints.borrow_mut();
        if let Some(pos) = cps.iter().rposition(|(_, _)| true) {
            cps.truncate(pos);
        }
    }
}

/// Real Ethereum mainnet fork blocks (activation heights).
pub const ISTANBUL_BLOCK: u64 = 9_069_005;
pub const BERLIN_BLOCK: u64 = 12_244_000;
pub const LONDON_BLOCK: u64 = 12_965_000;
pub const PARIS_BLOCK: u64 = 15_537_394;
pub const SHANGHAI_BLOCK: u64 = 17_034_870;
pub const CANCUN_BLOCK: u64 = 19_426_587;

/// Pick the interpreter spec that matches a MAINNET height.
/// Public snapshot of the journaled writes (for the block builder).
#[derive(Default, Clone)]
pub struct JournalDelta {
    pub balances: HashMap<H160, U256>,
    pub nonces: HashMap<H160, u64>,
    pub codes: HashMap<H160, Vec<u8>>,
    pub storage: HashMap<(H160, U256), U256>,
    pub deleted: Vec<H160>,
}

impl RemoteHost {
    /// Drain logs emitted since the last call (per-tx collection for the builder).
    pub fn take_logs(&self) -> Vec<Log> {
        std::mem::take(&mut *self.collected_logs.borrow_mut())
    }

    /// Clone the current journal (all writes since creation).
    pub fn journal_delta(&self) -> JournalDelta {
        let j = self.journal.borrow();
        JournalDelta {
            balances: j.balances.clone(),
            nonces: j.nonces.clone(),
            codes: j.codes.clone(),
            storage: j.storage.clone(),
            deleted: j.deleted.iter().cloned().collect(),
        }
    }

    /// All addresses touched by writes so far.
    pub fn touched(&self) -> Vec<H160> {
        let j = self.journal.borrow();
        let mut out: Vec<H160> = j.balances.keys().cloned().collect();
        for a in j.nonces.keys() {
            if !out.contains(a) {
                out.push(*a);
            }
        }
        for a in j.codes.keys() {
            if !out.contains(a) {
                out.push(*a);
            }
        }
        for (a, _) in j.storage.keys() {
            if !out.contains(a) {
                out.push(*a);
            }
        }
        out
    }
}

pub fn mainnet_spec(block: u64) -> Spec {
    if block >= CANCUN_BLOCK {
        Spec::cancun()
    } else if block >= SHANGHAI_BLOCK {
        Spec::shanghai()
    } else if block >= PARIS_BLOCK {
        Spec::paris()
    } else if block >= LONDON_BLOCK {
        Spec::london()
    } else if block >= BERLIN_BLOCK {
        Spec::berlin()
    } else {
        Spec::istanbul()
    }
}

/// live network name for a chain id (dashboard nicety)
pub fn chain_name(chain_id: u64) -> &'static str {
    match chain_id {
        1 => "Ethereum Mainnet",
        11155111 => "Sepolia",
        8453 => "Base",
        84532 => "Base Sepolia",
        137 => "Polygon",
        42161 => "Arbitrum One",
        10 => "Optimism",
        _ => "unknown network",
    }
}

/// decode a JSON access list from a tx object into [(address, [slots])]
pub fn access_list_from_tx(tx: &serde_json::Value) -> Vec<(H160, Vec<U256>)> {
    let mut out = vec![];
    if let Some(items) = tx.get("accessList").and_then(|a| a.as_array()) {
        for item in items {
            let addr = item["address"]
                .as_str()
                .and_then(|s| rrx_util::h160_from_hex(s).ok());
            let keys: Vec<U256> = item["storageKeys"]
                .as_array()
                .map(|a| a.iter().filter_map(|k| k.as_str().and_then(|s| u256_from_str(s).ok())).collect())
                .unwrap_or_default();
            if let Some(a) = addr {
                out.push((a, keys));
            }
        }
    }
    out
}

/// small helper shared by debugtx: fetch the block header env
pub fn block_env_of(p: &Provider, number: u64) -> Result<rrx_evm::BlockEnv, String> {
    let b = p
        .block_by_number(number, false)?
        .ok_or_else(|| format!("block {number} not found"))?;
    let base_fee = b
        .get("baseFeePerGas")
        .and_then(|x| x.as_str())
        .and_then(|s| u256_from_str(s).ok())
        .unwrap_or_default();
    Ok(rrx_evm::BlockEnv {
        number,
        coinbase: b
            .get("miner")
            .and_then(|x| x.as_str())
            .and_then(|s| rrx_util::h160_from_hex(s).ok())
            .unwrap_or_default(),
        timestamp: b
            .get("timestamp")
            .and_then(|x| x.as_str())
            .and_then(|s| u256_from_str(s).ok())
            .map(|v| v.as_u64())
            .unwrap_or(0),
        gas_limit: b
            .get("gasLimit")
            .and_then(|x| x.as_str())
            .and_then(|s| u256_from_str(s).ok())
            .map(|v| v.as_u64())
            .unwrap_or(30_000_000),
        base_fee,
        prevrandao: b
            .get("mixHash")
            .and_then(|x| x.as_str())
            .and_then(|s| h256_from_hex(s).ok())
            .unwrap_or_default(),
        excess_blob_gas: b
            .get("excessBlobGas")
            .and_then(|x| x.as_str())
            .and_then(|s| u256_from_str(s).ok())
            .map(|v| v.as_u64())
            .unwrap_or(0),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn mainnet_spec_boundaries() {
        assert_eq!(mainnet_spec(1).push0, false);
        assert_eq!(mainnet_spec(SHANGHAI_BLOCK).push0, true);
        assert_eq!(mainnet_spec(CANCUN_BLOCK).transient_storage, true);
        assert_eq!(mainnet_spec(SHANGHAI_BLOCK).transient_storage, false);
        assert_eq!(mainnet_spec(1).eip3860, false);
    }

    #[test]
    fn access_list_decode() {
        let tx = serde_json::json!({
            "accessList": [
                {"address": "0x0000000000000000000000000000000000000001",
                 "storageKeys": ["0x0000000000000000000000000000000000000000000000000000000000000001"]}
            ]
        });
        let al = access_list_from_tx(&tx);
        assert_eq!(al.len(), 1);
        assert_eq!(al[0].1.len(), 1);
        assert_eq!(access_list_from_tx(&serde_json::json!({})).len(), 0);
    }
}
