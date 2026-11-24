//! Receipts and 2048-bit logs bloom (real Ethereum format).

use rrx_evm::Log;
use rrx_util::{keccak256, rlp::Rlp, H160, H256};

#[derive(Clone, Debug, PartialEq)]
pub struct LogBloom(pub [u8; 256]);

impl LogBloom {
    pub fn zero() -> LogBloom {
        LogBloom([0u8; 256])
    }

    fn set_bit(&mut self, k: usize) {
        self.0[k >> 3] |= 0x80 >> (k & 7);
    }

    /// m(x) per the Yellow Paper: three 11-bit indices from keccak(x).
    fn add_data(&mut self, data: &[u8]) {
        let h = keccak256(data);
        for i in [0usize, 2, 4] {
            let k = (((h[i] as usize) << 8) | h[i + 1] as usize) & 2047;
            self.set_bit(k);
        }
    }

    pub fn add_log(&mut self, log: &Log) {
        self.add_data(log.address.as_bytes());
        for t in &log.topics {
            self.add_data(t.as_bytes());
        }
    }

    pub fn contains_address(&self, addr: &H160) -> bool {
        let mut probe = LogBloom::zero();
        probe.add_data(addr.as_bytes());
        self.covers(&probe)
    }

    pub fn covers(&self, other: &LogBloom) -> bool {
        other.0.iter().enumerate().all(|(i, &b)| b & self.0[i] == b)
    }

    pub fn to_hex(&self) -> String {
        rrx_util::to_hex_prefixed(&self.0)
    }
}

#[derive(Clone, Debug)]
pub struct Receipt {
    pub status: bool,
    pub cumulative_gas_used: u64,
    pub bloom: LogBloom,
    pub logs: Vec<Log>,
    /// transaction type byte (0x00 legacy, 0x01, 0x02)
    pub tx_type: u8,
    /// filled by the executor
    pub tx_hash: H256,
    pub block_number: u64,
    pub contract_address: Option<H160>,
    pub gas_used: u64,
    pub effective_gas_price: U256X,
    pub output: Vec<u8>,
}

/// alias to avoid extra import noise
pub type U256X = rrx_util::U256;

impl Receipt {
    /// Post-Byzantium receipt RLP: [status, cumulativeGasUsed, bloom, logs]
    pub fn rlp_encode(&self) -> Vec<u8> {
        let logs = Rlp::list(
            self.logs
                .iter()
                .map(|l| {
                    Rlp::list(vec![
                        Rlp::h160(&l.address),
                        Rlp::list(l.topics.iter().map(Rlp::h256).collect()),
                        Rlp::Bytes(l.data.clone()),
                    ])
                })
                .collect(),
        );
        Rlp::list(vec![
            Rlp::u64(if self.status { 1 } else { 0 }),
            Rlp::u64(self.cumulative_gas_used),
            Rlp::Bytes(self.bloom.0.to_vec()),
            logs,
        ])
        .encode()
    }
}
