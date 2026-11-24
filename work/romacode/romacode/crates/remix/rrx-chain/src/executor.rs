//! The chain: block building, transaction application, fees, calls, gas estimation.

use std::collections::HashMap;

use rrx_evm::{tx_intrinsic_gas, BlockEnv, ExecutionResult, Host, Machine, Spec, TxEnv};
use rrx_util::{keccak256, rlp::Rlp, H160, H256, U256, EMPTY_LIST_HASH, EMPTY_TRIE_HASH};

use crate::receipt::{LogBloom, Receipt};
use crate::state::WorldState;
use crate::tx::Transaction;

#[derive(Clone, Debug)]
pub struct ChainConfig {
    pub chain_id: u64,
    pub gas_limit: u64,
    pub spec: Spec,
    pub initial_base_fee: U256,
}

impl Default for ChainConfig {
    fn default() -> Self {
        ChainConfig {
            chain_id: 31337,
            gas_limit: 30_000_000,
            spec: Spec::shanghai(),
            initial_base_fee: U256::from(1_000_000_000u64),
        }
    }
}

#[derive(Clone, Debug)]
pub struct BlockHeader {
    pub parent_hash: H256,
    pub number: u64,
    pub timestamp: u64,
    pub gas_limit: u64,
    pub gas_used: u64,
    pub base_fee_per_gas: U256,
    pub coinbase: H160,
    pub state_root: H256,
    pub transactions_root: H256,
    pub receipts_root: H256,
    pub logs_bloom: LogBloom,
    /// prevrandao / mixHash
    pub mix_hash: H256,
    pub extra_data: Vec<u8>,
}

impl BlockHeader {
    /// Real mainnet-format header hash (Shanghai layout incl. baseFee + withdrawalsRoot).
    pub fn hash(&self) -> H256 {
        let rlp = Rlp::list(vec![
            Rlp::h256(&self.parent_hash),
            Rlp::h256(&EMPTY_LIST_HASH),       // ommers hash
            Rlp::h160(&self.coinbase),
            Rlp::h256(&self.state_root),
            Rlp::h256(&self.transactions_root),
            Rlp::h256(&self.receipts_root),
            Rlp::Bytes(self.logs_bloom.0.to_vec()),
            Rlp::u64(0),                        // difficulty (post-merge)
            Rlp::u64(self.number),
            Rlp::u64(self.gas_limit),
            Rlp::u64(self.gas_used),
            Rlp::u64(self.timestamp),
            Rlp::Bytes(self.extra_data.clone()),
            Rlp::h256(&self.mix_hash),
            Rlp::Bytes([0u8; 8].to_vec()),      // nonce
            Rlp::u256(&self.base_fee_per_gas),
            Rlp::h256(&EMPTY_TRIE_HASH),        // withdrawals root (no withdrawals)
        ]);
        H256(keccak256(&rlp.encode()))
    }
}

#[derive(Clone, Debug)]
pub struct Block {
    pub header: BlockHeader,
    pub transactions: Vec<Transaction>,
    pub receipts: Vec<Receipt>,
}

pub struct Chain {
    pub config: ChainConfig,
    pub state: WorldState,
    pub blocks: Vec<Block>,
    pub tx_index: HashMap<H256, (u64, usize)>,
    pub pending: Vec<Transaction>,
}

pub fn trie_root_of_encoded(items: Vec<Vec<u8>>) -> H256 {
    let mut trie = rrx_trie::Trie::new();
    for (i, item) in items.iter().enumerate() {
        let key_rlp = Rlp::u64(i as u64).encode();
        let key = keccak256(&key_rlp);
        trie.insert(&key, item);
    }
    trie.root_hash()
}

impl Chain {
    pub fn genesis(config: ChainConfig, alloc: &[(H160, U256)]) -> Chain {
        let mut state = WorldState::default();
        for (a, v) in alloc {
            state.fund(a, *v);
        }
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_secs())
            .unwrap_or(1);
        let header = BlockHeader {
            parent_hash: H256::zero(),
            number: 0,
            timestamp: now,
            gas_limit: config.gas_limit,
            gas_used: 0,
            base_fee_per_gas: config.initial_base_fee,
            coinbase: H160::zero(),
            state_root: state.state_root(),
            transactions_root: EMPTY_TRIE_HASH,
            receipts_root: EMPTY_TRIE_HASH,
            logs_bloom: LogBloom::zero(),
            mix_hash: H256::zero(),
            extra_data: b"rrx-genesis".to_vec(),
        };
        Chain {
            config,
            state,
            blocks: vec![Block { header, transactions: vec![], receipts: vec![] }],
            tx_index: HashMap::new(),
            pending: vec![],
        }
    }

    pub fn latest_header(&self) -> &BlockHeader {
        &self.blocks.last().unwrap().header
    }

    pub fn next_base_fee(&self) -> U256 {
        let parent = self.latest_header();
        let target = parent.gas_limit / 2;
        let base = parent.base_fee_per_gas;
        let used = U256::from(parent.gas_used);
        let target_u = U256::from(target);
        if used == target_u || target_u.is_zero() {
            base
        } else if used > target_u {
            let delta = ((used - target_u) * base / target_u) / U256::from(8u8);
            base + delta.max(U256::one())
        } else {
            let delta = ((target_u - used) * base / target_u) / U256::from(8u8);
            base.saturating_sub(delta)
        }
    }

    pub fn block_hash(&self, number: u64) -> Option<H256> {
        self.blocks.get(number as usize).map(|b| b.header.hash())
    }

    pub fn send_raw_transaction(&mut self, raw: &[u8]) -> Result<H256, String> {
        let tx = Transaction::decode(raw)?;
        if tx.chain_id() != Some(self.config.chain_id) {
            return Err(format!(
                "chain id mismatch: tx {:?} != chain {:?}",
                tx.chain_id(),
                self.config.chain_id
            ));
        }
        tx.sender()?;
        if self.config.spec == Spec::shanghai() {
            // automine
            let hash = tx.hash();
            self.mine_block(vec![tx])?;
            Ok(hash)
        } else {
            let hash = tx.hash();
            self.pending.push(tx);
            Ok(hash)
        }
    }

    /// Apply `txs` in a new block (real execution, real state, real receipts).
    pub fn mine_block(&mut self, txs: Vec<Transaction>) -> Result<H256, String> {
        let parent = self.latest_header().clone();
        let base_fee = self.next_base_fee();
        let number = parent.number + 1;
        let timestamp = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_secs())
            .unwrap_or(parent.timestamp + 1)
            .max(parent.timestamp + 1);

        let mut state = self.state.clone();
        state.recent_block_hashes.clear();
        for i in 0..=256 {
            if parent.number >= i {
                let n = parent.number - i;
                if let Some(b) = self.blocks.get(n as usize) {
                    state.recent_block_hashes.insert(n, b.header.hash());
                }
            }
        }
        state.original_cache.clear();
        state.logs.clear();

        let block_env = BlockEnv {
            number,
            coinbase: H160::zero(),
            timestamp,
            gas_limit: self.config.gas_limit,
            base_fee,
            prevrandao: H256::from_low_u64_be(number | 1),
            excess_blob_gas: 0,
        };

        let mut receipts = Vec::new();
        let mut cumulative = 0u64;
        let mut block_bloom = LogBloom::zero();

        let txs_len = txs.len();
        for tx_idx in 0..txs_len {
            let tx = txs[tx_idx].clone();
            let (receipt, _used) = apply_tx(
                &mut state,
                &tx,
                &self.config,
                &block_env,
                cumulative,
                base_fee,
                number,
            )?;
            for l in &receipt.logs {
                block_bloom.add_log(l);
            }
            cumulative = receipt.cumulative_gas_used;
            receipts.push(receipt);
        }

        let transactions_root = trie_root_of_encoded(txs.iter().map(|t| t.rlp_encode()).collect());
        let receipts_root =
            trie_root_of_encoded(receipts.iter().map(|r| r.rlp_encode()).collect());

        let header = BlockHeader {
            parent_hash: parent.hash(),
            number,
            timestamp,
            gas_limit: self.config.gas_limit,
            gas_used: cumulative,
            base_fee_per_gas: base_fee,
            coinbase: H160::zero(),
            state_root: state.state_root(),
            transactions_root,
            receipts_root,
            logs_bloom: block_bloom,
            mix_hash: block_env.prevrandao,
            extra_data: b"rrx".to_vec(),
        };

        for (i, tx) in txs.iter().enumerate() {
            self.tx_index.insert(tx.hash(), (number, i));
        }
        self.state = state;
        self.blocks.push(Block { header: header.clone(), transactions: txs, receipts });
        Ok(header.hash())
    }

    /// eth_call semantics on a scratch state (nothing is persisted).
    pub fn call(
        &self,
        from: H160,
        to: Option<H160>,
        value: U256,
        data: Vec<u8>,
        gas_cap: u64,
        trace: bool,
    ) -> ExecutionResult {
        let mut scratch = self.state.clone();
        scratch.original_cache.clear();
        scratch.logs.clear();
        let base_fee = self.latest_header().base_fee_per_gas;
        let tx_env = TxEnv {
            caller: from,
            gas_price: base_fee,
            value,
            data,
            gas_limit: gas_cap,
            chain_id: self.config.chain_id,
            to,
            access_list: vec![],
            blob_hashes: vec![],
        };
        let intrinsic = tx_intrinsic_gas(&tx_env, &self.config.spec);
        let mut env = tx_env.clone();
        env.gas_limit = gas_cap.saturating_sub(intrinsic);
        let machine = Machine::new(&mut scratch, self.config.spec.clone(), self.block_env(), env, trace);
        let mut result = machine.run_tx();
        result.gas_used += intrinsic;
        result
    }

    /// Real gas estimation: execute, then binary search the minimum passing gas.
    pub fn estimate_gas(
        &self,
        from: H160,
        to: Option<H160>,
        value: U256,
        data: Vec<u8>,
    ) -> Result<u64, String> {
        let cap = self.config.gas_limit;
        let probe = self.call(from, to, value, data.clone(), cap, false);
        if !probe.is_success() {
            return Err(match probe.revert_reason() {
                Some(r) => format!("execution reverted: {r}"),
                None => format!("execution reverted ({:?})", probe.kind),
            });
        }
        let mut lo = probe.gas_used;
        let mut hi = cap;
        // refine with binary search
        while lo + 1 < hi {
            let mid = lo + (hi - lo) / 2;
            let r = self.call(from, to, value, data.clone(), mid, false);
            if r.is_success() {
                hi = mid;
                lo = lo.min(r.gas_used);
            } else {
                lo = mid;
            }
            if hi - lo < cap / 1024 {
                break;
            }
        }
        Ok(hi)
    }

    fn block_env(&self) -> BlockEnv {
        let h = self.latest_header();
        BlockEnv {
            number: h.number + 1,
            coinbase: h.coinbase,
            timestamp: h.timestamp + 1,
            gas_limit: self.config.gas_limit,
            base_fee: h.base_fee_per_gas,
            prevrandao: H256::from_low_u64_be(h.number + 1 | 1),
            excess_blob_gas: 0,
        }
    }

    pub fn receipt_for(&self, tx_hash: &H256) -> Option<&Receipt> {
        let (bn, idx) = self.tx_index.get(tx_hash)?;
        self.blocks.get(*bn as usize)?.receipts.get(*idx)
    }

    pub fn block_by_number(&self, n: u64) -> Option<&Block> {
        self.blocks.get(n as usize)
    }

    pub fn latest_block_receipts(&self, tx_idx: usize) -> &crate::receipt::Receipt {
        &self.blocks.last().unwrap().receipts[tx_idx]
    }
}

fn apply_tx(
    state: &mut WorldState,
    tx: &Transaction,
    config: &crate::ChainConfig,
    block_env: &BlockEnv,
    cumulative: u64,
    base_fee: U256,
    number: u64,
) -> Result<(Receipt, u64), String> {
    let sender = tx.sender()?;
    if tx.chain_id() != Some(config.chain_id) {
        return Err("chain id mismatch".into());
    }
    if state.nonce(&sender) != tx.nonce() {
        return Err(format!(
            "nonce mismatch: have {} want {}",
            state.nonce(&sender),
            tx.nonce()
        ));
    }
    let effective_price = match tx {
        crate::tx::Transaction::Eip1559 { max_fee, max_priority_fee, .. } => {
            if *max_fee < base_fee {
                return Err(format!(
                    "max fee per gas {} below base fee {}",
                    max_fee, base_fee
                ));
            }
            let tip = (*max_priority_fee).min(*max_fee - base_fee);
            base_fee + tip
        }
        _ => tx.gas_price_field(),
    };

    let gas_limit = tx.gas_limit();
    let value = tx.value();
    let upfront = U256::from(gas_limit) * effective_price + value;
    if state.balance(&sender) < upfront {
        return Err("insufficient funds for gas * price + value".into());
    }

    let intrinsic = tx_intrinsic_gas(
        &TxEnv {
            caller: sender,
            gas_price: effective_price,
            value,
            data: tx.data().to_vec(),
            gas_limit,
            chain_id: config.chain_id,
            to: tx.to(),
            access_list: tx.access_list().to_vec(),
            blob_hashes: vec![],
        },
        &config.spec,
    );
    if gas_limit < intrinsic {
        return Err("intrinsic gas too low".into());
    }

    // Only the gas prepaid is debited here; the value transfer is performed by
    // the root frame (and rolled back if the call fails), matching geth.
    state.set_balance(&sender, state.balance(&sender) - U256::from(gas_limit) * effective_price);
    state.inc_nonce(&sender);
    state.original_cache.clear();
    let logs_before = state.logs.len();

    let tx_env = TxEnv {
        caller: sender,
        gas_price: effective_price,
        value,
        data: tx.data().to_vec(),
        gas_limit: gas_limit - intrinsic,
        chain_id: config.chain_id,
        to: tx.to(),
        access_list: tx.access_list().to_vec(),
        blob_hashes: vec![],
    };
    let machine = Machine::new(state, config.spec.clone(), block_env.clone(), tx_env, false);
    let result = machine.run_tx();

    let consumed = gas_limit - result.gas_remaining;
    let refund = (result.refund.max(0) as u64).min(consumed / 5);
    let used = consumed - refund;
    // credit unused gas plus the capped refund counter in one balance update
    let gas_credit = (gas_limit - consumed) + refund;
    state.set_balance(&sender, state.balance(&sender) + U256::from(gas_credit) * effective_price);

    let status = result.is_success();
    let logs: Vec<rrx_evm::Log> = state.logs[logs_before..].to_vec();
    let mut bloom = LogBloom::zero();
    for l in &logs {
        bloom.add_log(l);
    }
    state.clear_transient();
    state.original_cache.clear();

    let tx_type = match tx {
        crate::tx::Transaction::Legacy { .. } => 0u8,
        crate::tx::Transaction::Eip2930 { .. } => 1,
        crate::tx::Transaction::Eip1559 { .. } => 2,
        crate::tx::Transaction::Eip4844 { .. } => 3,
    };

    let receipt = Receipt {
        status,
        cumulative_gas_used: cumulative + used,
        bloom,
        logs,
        tx_type,
        tx_hash: tx.hash(),
        block_number: number,
        contract_address: result.created_address,
        gas_used: used,
        effective_gas_price: effective_price,
        output: result.output,
    };
    Ok((receipt, used))
}
