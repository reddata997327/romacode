//! rrx-util: core utilities for the rrx toolchain (keccak256, RLP, hex, 256-bit types, ABI codec).

pub mod abi;
pub mod rlp;

pub use primitive_types::{H160, H256, U256};
use tiny_keccak::{Hasher, Keccak};

pub const ZERO_ADDRESS: H160 = H160([0u8; 20]);

/// keccak256(rlp("")) — empty trie root / empty-string-list hash.
pub const EMPTY_TRIE_HASH: H256 = H256([
    0x56, 0xe8, 0x1f, 0x17, 0x1b, 0xcc, 0x55, 0xa6, 0xff, 0x83, 0x45, 0xe6, 0x92, 0xc0, 0xf8, 0x6e,
    0x5b, 0x48, 0xe0, 0x1b, 0x99, 0x6c, 0xad, 0xc0, 0x01, 0x62, 0x2f, 0xb5, 0xe3, 0x63, 0xb4, 0x21,
]);

/// keccak256(rlp([])) — empty list hash (ommer hash of a block without uncles).
pub const EMPTY_LIST_HASH: H256 = H256([
    0x1d, 0xcc, 0x4d, 0xe8, 0xde, 0xc7, 0x5d, 0x7a, 0xab, 0x85, 0xb5, 0x67, 0xb6, 0xcc, 0xd4, 0x1a,
    0xd3, 0x12, 0x45, 0x1b, 0x94, 0x8a, 0x74, 0x13, 0xf0, 0xa1, 0x42, 0xfd, 0x40, 0xd4, 0x93, 0x47,
]);

/// keccak256("") — hash of empty code / empty data.
pub const KECCAK_EMPTY: H256 = H256([
    0xc5, 0xd2, 0x46, 0x01, 0x86, 0xf7, 0x23, 0x3c, 0x92, 0x7e, 0x7d, 0xb2, 0xdc, 0xc7, 0x03, 0xc0,
    0xe5, 0x00, 0xb6, 0x53, 0xca, 0x82, 0x27, 0x3b, 0x7b, 0xfa, 0xd8, 0x04, 0x5d, 0x85, 0xa4, 0x70,
]);

pub fn keccak256(data: &[u8]) -> [u8; 32] {
    let mut k = Keccak::v256();
    let mut out = [0u8; 32];
    k.update(data);
    k.finalize(&mut out);
    out
}

pub fn keccak256_h(data: &[u8]) -> H256 {
    H256(keccak256(data))
}

// ---------- hex helpers ----------

pub fn to_hex(data: &[u8]) -> String {
    hex::encode(data)
}

pub fn to_hex_prefixed(data: &[u8]) -> String {
    format!("0x{}", hex::encode(data))
}

pub fn from_hex(s: &str) -> Result<Vec<u8>, hex::FromHexError> {
    let s = s.strip_prefix("0x").or_else(|| s.strip_prefix("0X")).unwrap_or(s);
    hex::decode(s)
}

pub fn h160_from_hex(s: &str) -> Result<H160, String> {
    let v = from_hex(s).map_err(|e| e.to_string())?;
    if v.len() != 20 {
        return Err(format!("bad address length {}", v.len()));
    }
    Ok(H160::from_slice(&v))
}

pub fn h256_from_hex(s: &str) -> Result<H256, String> {
    let v = from_hex(s).map_err(|e| e.to_string())?;
    if v.len() != 32 {
        return Err(format!("bad hash length {}", v.len()));
    }
    Ok(H256::from_slice(&v))
}

pub fn u256_from_str(s: &str) -> Result<U256, String> {
    let t = s.trim().replace('_', "");
    if let Some(h) = t.strip_prefix("0x").or_else(|| t.strip_prefix("0X")) {
        if h.is_empty() {
            return Ok(U256::zero());
        }
        let padded = if h.len() % 2 == 1 { format!("0{}", h) } else { h.to_string() };
        let b = hex::decode(padded).map_err(|e| e.to_string())?;
        if b.len() > 32 {
            return Err("number too large for u256".into());
        }
        Ok(U256::from_big_endian(&b))
    } else {
        U256::from_dec_str(&t).map_err(|e| e.to_string())
    }
}

pub fn u256_to_be32(v: &U256) -> [u8; 32] {
    v.to_big_endian()
}

/// Format wei into (gwei, eth) human strings.
pub fn wei_to_strings(wei: &U256) -> (String, String) {
    let gwei = *wei / U256::from(1_000_000_000u64);
    let eth_int = *wei / U256::from(1_000_000_000_000_000_000u64);
    let eth_rem = *wei % U256::from(1_000_000_000_000_000_000u64);
    let frac = format!("{:018}", eth_rem).trim_end_matches('0').to_string();
    let eth = if frac.is_empty() { format!("{}", eth_int) } else { format!("{}.{}", eth_int, frac) };
    (gwei.to_string(), eth)
}
