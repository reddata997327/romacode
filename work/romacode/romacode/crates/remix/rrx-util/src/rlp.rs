//! Minimal, spec-conformant RLP encoder.

use crate::U256;

#[derive(Clone, Debug, PartialEq)]
pub enum Rlp {
    Bytes(Vec<u8>),
    List(Vec<Rlp>),
}

impl Rlp {
    pub fn bytes(b: impl Into<Vec<u8>>) -> Rlp {
        Rlp::Bytes(b.into())
    }

    pub fn list(items: Vec<Rlp>) -> Rlp {
        Rlp::List(items)
    }

    pub fn empty_list() -> Rlp {
        Rlp::List(vec![])
    }

    /// Encode a U256 with minimal big-endian representation (empty for zero).
    pub fn u256(v: &U256) -> Rlp {
        Rlp::Bytes(u256_trimmed_be(v))
    }

    pub fn u64(v: u64) -> Rlp {
        Rlp::Bytes(u64_trimmed_be(v))
    }

    /// Fixed-width (keeps leading zeros) encodings used in headers/txs.
    pub fn h256(h: &crate::H256) -> Rlp {
        Rlp::Bytes(h.as_bytes().to_vec())
    }

    pub fn h160(h: &crate::H160) -> Rlp {
        Rlp::Bytes(h.as_bytes().to_vec())
    }

    pub fn encode(&self) -> Vec<u8> {
        let mut out = Vec::new();
        self.encode_to(&mut out);
        out
    }

    pub fn encode_to(&self, out: &mut Vec<u8>) {
        match self {
            Rlp::Bytes(b) => {
                if b.len() == 1 && b[0] < 0x80 {
                    out.push(b[0]);
                } else if b.len() <= 55 {
                    out.push(0x80 + b.len() as u8);
                    out.extend_from_slice(b);
                } else {
                    let len_be = usize_be(b.len());
                    out.push(0xb7 + len_be.len() as u8);
                    out.extend_from_slice(&len_be);
                    out.extend_from_slice(b);
                }
            }
            Rlp::List(items) => {
                let mut payload = Vec::new();
                for i in items {
                    i.encode_to(&mut payload);
                }
                if payload.len() <= 55 {
                    out.push(0xc0 + payload.len() as u8);
                } else {
                    let len_be = usize_be(payload.len());
                    out.push(0xf7 + len_be.len() as u8);
                    out.extend_from_slice(&len_be);
                }
                out.extend_from_slice(&payload);
            }
        }
    }
}

fn usize_be(n: usize) -> Vec<u8> {
    if n == 0 {
        return vec![];
    }
    let bytes = n.to_be_bytes();
    let first_nonzero = bytes.iter().position(|&b| b != 0).unwrap();
    bytes[first_nonzero..].to_vec()
}

pub fn u64_trimmed_be(v: u64) -> Vec<u8> {
    if v == 0 {
        return vec![];
    }
    let b = v.to_be_bytes();
    let i = b.iter().position(|&x| x != 0).unwrap();
    b[i..].to_vec()
}

pub fn u256_trimmed_be(v: &U256) -> Vec<u8> {
    let b = crate::u256_to_be32(v);
    let i = b.iter().position(|&x| x != 0).unwrap_or(32);
    b[i..].to_vec()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{keccak256, to_hex, EMPTY_LIST_HASH, EMPTY_TRIE_HASH};

    #[test]
    fn empty_string_and_list_hashes() {
        assert_eq!(keccak256(&Rlp::bytes(Vec::<u8>::new()).encode()), EMPTY_TRIE_HASH.0);
        assert_eq!(keccak256(&Rlp::empty_list().encode()), EMPTY_LIST_HASH.0);
    }

    #[test]
    fn single_low_byte() {
        assert_eq!(to_hex(&Rlp::u64(0).encode()), "80");
        assert_eq!(to_hex(&Rlp::u64(15).encode()), "0f");
        assert_eq!(to_hex(&Rlp::u64(1024).encode()), "820400");
        assert_eq!(to_hex(&Rlp::bytes("dog".as_bytes()).encode()), "83646f67");
        let l = Rlp::list(vec![Rlp::bytes("cat".as_bytes()), Rlp::bytes("dog".as_bytes())]);
        assert_eq!(to_hex(&l.encode()), "c88363617483646f67");
    }

    #[test]
    fn long_list() {
        let items: Vec<Rlp> = (0..32).map(|i| Rlp::bytes(vec![i as u8; 32])).collect();
        let enc = Rlp::list(items).encode();
        // payload 32*33 = 1056 bytes -> 0xf9 + 2 length bytes (0x04 0x20)
        assert_eq!(enc[0], 0xf9);
        assert_eq!(enc[1], 0x04);
        assert_eq!(enc[2], 0x20);
    }
}
