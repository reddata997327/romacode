//! Typed transactions: Legacy (EIP-155), EIP-2930, EIP-1559 and EIP-4844 blob
//! transactions (type 0x03, incl. KZG sidecar encode/decode).
//! Real signing with secp256k1 and sender recovery.

use rrx_crypto::KeyPair;
use rrx_util::{keccak256, rlp::Rlp, H160, H256, U256};

pub type AccessList = Vec<(H160, Vec<U256>)>;

static EMPTY_AL: AccessList = Vec::new();

#[derive(Clone, Debug, PartialEq)]
pub enum Transaction {
    Legacy {
        nonce: u64,
        gas_price: U256,
        gas_limit: u64,
        to: Option<H160>,
        value: U256,
        data: Vec<u8>,
        chain_id: Option<u64>,
        v: u64,
        r: [u8; 32],
        s: [u8; 32],
    },
    Eip2930 {
        chain_id: u64,
        nonce: u64,
        gas_price: U256,
        gas_limit: u64,
        to: Option<H160>,
        value: U256,
        data: Vec<u8>,
        access_list: AccessList,
        y_parity: bool,
        r: [u8; 32],
        s: [u8; 32],
    },
    Eip1559 {
        chain_id: u64,
        nonce: u64,
        max_priority_fee: U256,
        max_fee: U256,
        gas_limit: u64,
        to: Option<H160>,
        value: U256,
        data: Vec<u8>,
        access_list: AccessList,
        y_parity: bool,
        r: [u8; 32],
        s: [u8; 32],
    },
    Eip4844 {
        chain_id: u64,
        nonce: u64,
        max_priority_fee: U256,
        max_fee: U256,
        gas_limit: u64,
        to: H160,
        value: U256,
        data: Vec<u8>,
        access_list: AccessList,
        max_fee_per_blob_gas: U256,
        blob_versioned_hashes: Vec<H256>,
        y_parity: bool,
        r: [u8; 32],
        s: [u8; 32],
    },
}

/// EIP-4844 blob transaction sidecar (network wrapper payload, not signed over).
#[derive(Clone, Debug, PartialEq)]
pub struct BlobSidecar {
    pub blobs: Vec<Vec<u8>>,
    pub commitments: Vec<[u8; 48]>,
    pub proofs: Vec<[u8; 48]>,
}

fn al_to_rlp(al: &AccessList) -> Rlp {
    Rlp::list(
        al.iter()
            .map(|(addr, keys)| {
                Rlp::list(vec![
                    Rlp::h160(addr),
                    Rlp::list(keys.iter().map(Rlp::u256).collect()),
                ])
            })
            .collect(),
    )
}

impl Transaction {
    pub fn nonce(&self) -> u64 {
        match self {
            Transaction::Legacy { nonce, .. } => *nonce,
            Transaction::Eip2930 { nonce, .. } => *nonce,
            Transaction::Eip1559 { nonce, .. } => *nonce,
            Transaction::Eip4844 { nonce, .. } => *nonce,
        }
    }

    pub fn gas_limit(&self) -> u64 {
        match self {
            Transaction::Legacy { gas_limit, .. } => *gas_limit,
            Transaction::Eip2930 { gas_limit, .. } => *gas_limit,
            Transaction::Eip1559 { gas_limit, .. } => *gas_limit,
            Transaction::Eip4844 { gas_limit, .. } => *gas_limit,
        }
    }

    pub fn to(&self) -> Option<H160> {
        match self {
            Transaction::Legacy { to, .. } => *to,
            Transaction::Eip2930 { to, .. } => *to,
            Transaction::Eip1559 { to, .. } => *to,
            Transaction::Eip4844 { to, .. } => Some(*to),
        }
    }

    pub fn value(&self) -> U256 {
        match self {
            Transaction::Legacy { value, .. } => *value,
            Transaction::Eip2930 { value, .. } => *value,
            Transaction::Eip1559 { value, .. } => *value,
            Transaction::Eip4844 { value, .. } => *value,
        }
    }

    pub fn data(&self) -> &[u8] {
        match self {
            Transaction::Legacy { data, .. } => data,
            Transaction::Eip2930 { data, .. } => data,
            Transaction::Eip1559 { data, .. } => data,
            Transaction::Eip4844 { data, .. } => data,
        }
    }

    pub fn access_list(&self) -> &AccessList {
        match self {
            Transaction::Legacy { .. } => &EMPTY_AL,
            Transaction::Eip2930 { access_list, .. } => access_list,
            Transaction::Eip1559 { access_list, .. } => access_list,
            Transaction::Eip4844 { access_list, .. } => access_list,
        }
    }

    pub fn chain_id(&self) -> Option<u64> {
        match self {
            Transaction::Legacy { chain_id, .. } => *chain_id,
            Transaction::Eip2930 { chain_id, .. } => Some(*chain_id),
            Transaction::Eip1559 { chain_id, .. } => Some(*chain_id),
            Transaction::Eip4844 { chain_id, .. } => Some(*chain_id),
        }
    }

    pub fn gas_price_field(&self) -> U256 {
        match self {
            Transaction::Legacy { gas_price, .. } => *gas_price,
            Transaction::Eip2930 { gas_price, .. } => *gas_price,
            Transaction::Eip1559 { max_fee, .. } => *max_fee,
            Transaction::Eip4844 { max_fee, .. } => *max_fee,
        }
    }

    /// keccak hash of the wire encoding (the tx hash).
    pub fn hash(&self) -> H256 {
        H256(keccak256(&self.rlp_encode()))
    }

    /// Hash that was actually signed.
    pub fn signing_hash(&self) -> H256 {
        H256(keccak256(&self.signing_payload()))
    }

    fn signing_payload(&self) -> Vec<u8> {
        match self {
            Transaction::Legacy { nonce, gas_price, gas_limit, to, value, data, chain_id, .. } => {
                let mut items = vec![
                    Rlp::u64(*nonce),
                    Rlp::u256(gas_price),
                    Rlp::u64(*gas_limit),
                    to.map_or(Rlp::Bytes(vec![]), |t| Rlp::h160(&t)),
                    Rlp::u256(value),
                    Rlp::Bytes(data.clone()),
                ];
                if let Some(cid) = chain_id {
                    items.push(Rlp::u64(*cid));
                    items.push(Rlp::u64(0));
                    items.push(Rlp::u64(0));
                }
                Rlp::list(items).encode()
            }
            Transaction::Eip2930 { chain_id, nonce, gas_price, gas_limit, to, value, data, access_list, .. } => {
                let mut out = vec![0x01u8];
                out.extend_from_slice(
                    &Rlp::list(vec![
                        Rlp::u64(*chain_id),
                        Rlp::u64(*nonce),
                        Rlp::u256(gas_price),
                        Rlp::u64(*gas_limit),
                        to.map_or(Rlp::Bytes(vec![]), |t| Rlp::h160(&t)),
                        Rlp::u256(value),
                        Rlp::Bytes(data.clone()),
                        al_to_rlp(access_list),
                    ])
                    .encode(),
                );
                out
            }
            Transaction::Eip1559 { chain_id, nonce, max_priority_fee, max_fee, gas_limit, to, value, data, access_list, .. } => {
                let mut out = vec![0x02u8];
                out.extend_from_slice(
                    &Rlp::list(vec![
                        Rlp::u64(*chain_id),
                        Rlp::u64(*nonce),
                        Rlp::u256(max_priority_fee),
                        Rlp::u256(max_fee),
                        Rlp::u64(*gas_limit),
                        to.map_or(Rlp::Bytes(vec![]), |t| Rlp::h160(&t)),
                        Rlp::u256(value),
                        Rlp::Bytes(data.clone()),
                        al_to_rlp(access_list),
                    ])
                    .encode(),
                );
                out
            }
            Transaction::Eip4844 { chain_id, nonce, max_priority_fee, max_fee, gas_limit, to, value, data, access_list, max_fee_per_blob_gas, blob_versioned_hashes, .. } => {
                let mut out = vec![0x03u8];
                out.extend_from_slice(
                    &Rlp::list(vec![
                        Rlp::u64(*chain_id),
                        Rlp::u64(*nonce),
                        Rlp::u256(max_priority_fee),
                        Rlp::u256(max_fee),
                        Rlp::u64(*gas_limit),
                        Rlp::h160(to),
                        Rlp::u256(value),
                        Rlp::Bytes(data.clone()),
                        al_to_rlp(access_list),
                        Rlp::u256(max_fee_per_blob_gas),
                        Rlp::list(blob_versioned_hashes.iter().map(|h| Rlp::h256(h)).collect()),
                    ])
                    .encode(),
                );
                out
            }
        }
    }

    /// Final wire encoding (with signature).
    pub fn rlp_encode(&self) -> Vec<u8> {
        match self {
            Transaction::Legacy { nonce, gas_price, gas_limit, to, value, data, chain_id: _, v, r, s } => {
                Rlp::list(vec![
                    Rlp::u64(*nonce),
                    Rlp::u256(gas_price),
                    Rlp::u64(*gas_limit),
                    to.map_or(Rlp::Bytes(vec![]), |t| Rlp::h160(&t)),
                    Rlp::u256(value),
                    Rlp::Bytes(data.clone()),
                    Rlp::u64(*v),
                    Rlp::Bytes(rrx_util::rlp::u256_trimmed_be(&U256::from_big_endian(r))),
                    Rlp::Bytes(rrx_util::rlp::u256_trimmed_be(&U256::from_big_endian(s))),
                ])
                .encode()
            }
            Transaction::Eip2930 { chain_id, nonce, gas_price, gas_limit, to, value, data, access_list, y_parity, r, s } => {
                let mut out = vec![0x01u8];
                out.extend_from_slice(
                    &Rlp::list(vec![
                        Rlp::u64(*chain_id),
                        Rlp::u64(*nonce),
                        Rlp::u256(gas_price),
                        Rlp::u64(*gas_limit),
                        to.map_or(Rlp::Bytes(vec![]), |t| Rlp::h160(&t)),
                        Rlp::u256(value),
                        Rlp::Bytes(data.clone()),
                        al_to_rlp(access_list),
                        Rlp::u64(*y_parity as u64),
                        Rlp::Bytes(rrx_util::rlp::u256_trimmed_be(&U256::from_big_endian(r))),
                        Rlp::Bytes(rrx_util::rlp::u256_trimmed_be(&U256::from_big_endian(s))),
                    ])
                    .encode(),
                );
                out
            }
            Transaction::Eip1559 { chain_id, nonce, max_priority_fee, max_fee, gas_limit, to, value, data, access_list, y_parity, r, s } => {
                let mut out = vec![0x02u8];
                out.extend_from_slice(
                    &Rlp::list(vec![
                        Rlp::u64(*chain_id),
                        Rlp::u64(*nonce),
                        Rlp::u256(max_priority_fee),
                        Rlp::u256(max_fee),
                        Rlp::u64(*gas_limit),
                        to.map_or(Rlp::Bytes(vec![]), |t| Rlp::h160(&t)),
                        Rlp::u256(value),
                        Rlp::Bytes(data.clone()),
                        al_to_rlp(access_list),
                        Rlp::u64(*y_parity as u64),
                        Rlp::Bytes(rrx_util::rlp::u256_trimmed_be(&U256::from_big_endian(r))),
                        Rlp::Bytes(rrx_util::rlp::u256_trimmed_be(&U256::from_big_endian(s))),
                    ])
                    .encode(),
                );
                out
            }
            Transaction::Eip4844 { chain_id, nonce, max_priority_fee, max_fee, gas_limit, to, value, data, access_list, max_fee_per_blob_gas, blob_versioned_hashes, y_parity, r, s } => {
                let mut out = vec![0x03u8];
                out.extend_from_slice(
                    &Rlp::list(vec![
                        Rlp::u64(*chain_id),
                        Rlp::u64(*nonce),
                        Rlp::u256(max_priority_fee),
                        Rlp::u256(max_fee),
                        Rlp::u64(*gas_limit),
                        Rlp::h160(to),
                        Rlp::u256(value),
                        Rlp::Bytes(data.clone()),
                        al_to_rlp(access_list),
                        Rlp::u256(max_fee_per_blob_gas),
                        Rlp::list(blob_versioned_hashes.iter().map(|h| Rlp::h256(h)).collect()),
                        Rlp::u64(*y_parity as u64),
                        Rlp::Bytes(rrx_util::rlp::u256_trimmed_be(&U256::from_big_endian(r))),
                        Rlp::Bytes(rrx_util::rlp::u256_trimmed_be(&U256::from_big_endian(s))),
                    ])
                    .encode(),
                );
                out
            }
        }
    }

    /// Recover the sender from the signature.
    pub fn sender(&self) -> Result<H160, String> {
        let hash = self.signing_hash();
        match self {
            Transaction::Legacy { chain_id, v, r, s, .. } => {
                let recid = if let Some(cid) = chain_id {
                    if *v < 35 {
                        return Err("legacy tx without EIP-155 chain id".into());
                    }
                    let computed = 35 + 2 * cid;
                    let parity = if *v == computed { 0 } else if *v == computed + 1 { 1 } else {
                        return Err("invalid EIP-155 v".into());
                    };
                    parity
                } else {
                    if !(*v == 27 || *v == 28) {
                        return Err("invalid v".into());
                    }
                    (*v - 27) as u8
                };
                rrx_crypto::recover_from_rsv(&hash, r, s, recid)
            }
            Transaction::Eip2930 { y_parity, r, s, .. } | Transaction::Eip1559 { y_parity, r, s, .. } | Transaction::Eip4844 { y_parity, r, s, .. } => {
                rrx_crypto::recover_from_rsv(&hash, r, s, *y_parity as u8)
            }
        }
    }

    /// Decode a raw transaction (typed envelope or legacy).
    pub fn decode(raw: &[u8]) -> Result<Transaction, String> {
        if raw.is_empty() {
            return Err("empty tx".into());
        }
        match raw[0] {
            0x01 => decode_typed(&raw[1..], 1),
            0x02 => decode_typed(&raw[1..], 2),
            0x03 => decode_typed(&raw[1..], 3),
            _ => decode_legacy(raw),
        }
    }
}

fn be32_trim(r: &[u8]) -> [u8; 32] {
    let mut out = [0u8; 32];
    let n = r.len().min(32);
    out[32 - n..].copy_from_slice(&r[r.len() - n..]);
    out
}

/// Minimal RLP decoder good enough for transaction lists.
struct Dec<'a> {
    data: &'a [u8],
    pos: usize,
}

impl<'a> Dec<'a> {
    fn item(&mut self) -> Result<&'a [u8], String> {
        if self.pos >= self.data.len() {
            return Err("rlp: eof".into());
        }
        let b = self.data[self.pos];
        let (payload_start, payload_len) = if b < 0x80 {
            (self.pos, 1)
        } else if b < 0xb8 {
            let l = (b - 0x80) as usize;
            (self.pos + 1, l)
        } else if b < 0xc0 {
            let ll = (b - 0xb7) as usize;
            if self.pos + 1 + ll > self.data.len() {
                return Err("rlp: unexpected end of input".into());
            }
            let l = be_usize(&self.data[self.pos + 1..self.pos + 1 + ll])?;
            (self.pos + 1 + ll, l)
        } else if b < 0xf8 {
            let l = (b - 0xc0) as usize;
            (self.pos + 1, l)
        } else {
            let ll = (b - 0xf7) as usize;
            if self.pos + 1 + ll > self.data.len() {
                return Err("rlp: unexpected end of input".into());
            }
            let l = be_usize(&self.data[self.pos + 1..self.pos + 1 + ll])?;
            (self.pos + 1 + ll, l)
        };
        if payload_start.saturating_add(payload_len) > self.data.len() {
            return Err("rlp: truncated".into());
        }
        self.pos = payload_start;
        let out = &self.data[payload_start..payload_start + payload_len];
        self.pos += payload_len;
        Ok(out)
    }

    fn enter_list(&mut self) -> Result<usize, String> {
        if self.pos >= self.data.len() {
            return Err("rlp: unexpected end of input".into());
        }
        let b = self.data[self.pos];
        let (payload_start, payload_len) = if b < 0xc0 {
            return Err("rlp: expected list".into());
        } else if b < 0xf8 {
            ((self.pos + 1), (b - 0xc0) as usize)
        } else {
            let ll = (b - 0xf7) as usize;
            if self.pos + 1 + ll > self.data.len() {
                return Err("rlp: unexpected end of input".into());
            }
            let l = be_usize(&self.data[self.pos + 1..self.pos + 1 + ll])?;
            (self.pos + 1 + ll, l)
        };
        if payload_start.saturating_add(payload_len) > self.data.len() {
            return Err("rlp: truncated list".into());
        }
        self.pos = payload_start;
        Ok(payload_start + payload_len)
    }
}

fn be_usize(b: &[u8]) -> Result<usize, String> {
    if b.len() > 8 {
        return Err("rlp: length too long".into());
    }
    let mut v: usize = 0;
    for &x in b {
        v = (v << 8) | x as usize;
    }
    Ok(v)
}

fn u64_from(b: &[u8]) -> Result<u64, String> {
    if b.len() > 8 {
        return Err("uint too big".into());
    }
    Ok(be_usize(b)? as u64)
}

fn u256_from(b: &[u8]) -> U256 {
    U256::from_big_endian(b)
}

fn h160_from(b: &[u8]) -> Result<H160, String> {
    if b.len() != 20 {
        return Err("bad address".into());
    }
    Ok(H160::from_slice(b))
}

fn decode_legacy(raw: &[u8]) -> Result<Transaction, String> {
    let mut d = Dec { data: raw, pos: 0 };
    let end = d.enter_list()?;
    let nonce = u64_from(d.item()?)?;
    let gas_price = u256_from(d.item()?);
    let gas_limit = u64_from(d.item()?)?;
    let to_field = d.item()?;
    let to = if to_field.is_empty() { None } else { Some(h160_from(to_field)?) };
    let value = u256_from(d.item()?);
    let data = d.item()?.to_vec();
    let v = u64_from(d.item()?)?;
    let r = be32_trim(d.item()?);
    let s = be32_trim(d.item()?);
    let chain_id = if v >= 35 { Some((v - 35) / 2) } else { None };
    if d.pos != end {
        return Err("rlp: trailing data".into());
    }
    Ok(Transaction::Legacy { nonce, gas_price, gas_limit, to, value, data, chain_id, v, r, s })
}

fn decode_typed(raw: &[u8], ty: u8) -> Result<Transaction, String> {
    let mut d = Dec { data: raw, pos: 0 };
    let _end = d.enter_list()?;
    let chain_id = u64_from(d.item()?)?;
    let nonce = u64_from(d.item()?)?;
    match ty {
        1 => {
            let gas_price = u256_from(d.item()?);
            let gas_limit = u64_from(d.item()?)?;
            let to_field = d.item()?;
            let to = if to_field.is_empty() { None } else { Some(h160_from(to_field)?) };
            let value = u256_from(d.item()?);
            let data = d.item()?.to_vec();
            let access_list = decode_access_list(&mut d)?;
            let y_parity = u64_from(d.item()?)? == 1;
            let r = be32_trim(d.item()?);
            let s = be32_trim(d.item()?);
            Ok(Transaction::Eip2930 { chain_id, nonce, gas_price, gas_limit, to, value, data, access_list, y_parity, r, s })
        }
        2 => {
            let max_priority_fee = u256_from(d.item()?);
            let max_fee = u256_from(d.item()?);
            let gas_limit = u64_from(d.item()?)?;
            let to_field = d.item()?;
            let to = if to_field.is_empty() { None } else { Some(h160_from(to_field)?) };
            let value = u256_from(d.item()?);
            let data = d.item()?.to_vec();
            let access_list = decode_access_list(&mut d)?;
            let y_parity = u64_from(d.item()?)? == 1;
            let r = be32_trim(d.item()?);
            let s = be32_trim(d.item()?);
            Ok(Transaction::Eip1559 { chain_id, nonce, max_priority_fee, max_fee, gas_limit, to, value, data, access_list, y_parity, r, s })
        }
        3 => {
            let max_priority_fee = u256_from(d.item()?);
            let max_fee = u256_from(d.item()?);
            let gas_limit = u64_from(d.item()?)?;
            let to_field = d.item()?;
            let to = if to_field.is_empty() { return Err("blob tx must have a 'to' address".into()); } else { h160_from(to_field)? };
            let value = u256_from(d.item()?);
            let data = d.item()?.to_vec();
            let access_list = decode_access_list(&mut d)?;
            let max_fee_per_blob_gas = u256_from(d.item()?);
            let vh_end = d.enter_list()?;
            let mut blob_versioned_hashes = vec![];
            while d.pos < vh_end {
                let b = d.item()?;
                if b.len() != 32 {
                    return Err("blob versioned hash must be 32 bytes".into());
                }
                let mut h = H256::zero();
                h.0.copy_from_slice(b);
                blob_versioned_hashes.push(h);
            }
            d.pos = vh_end;
            let y_parity = u64_from(d.item()?)? == 1;
            let r = be32_trim(d.item()?);
            let s = be32_trim(d.item()?);
            Ok(Transaction::Eip4844 { chain_id, nonce, max_priority_fee, max_fee, gas_limit, to, value, data, access_list, max_fee_per_blob_gas, blob_versioned_hashes, y_parity, r, s })
        }
        _ => Err("unknown tx type".into()),
    }
}

fn decode_access_list(d: &mut Dec) -> Result<AccessList, String> {
    let end = d.enter_list()?;
    let mut al = vec![];
    while d.pos < end {
        let e_end = d.enter_list()?;
        let addr = h160_from(d.item()?)?;
        let mut keys = vec![];
        let k_end = d.enter_list()?;
        while d.pos < k_end {
            keys.push(u256_from(d.item()?));
        }
        d.pos = k_end;
        al.push((addr, keys));
        d.pos = e_end;
    }
    Ok(al)
}

/// Unsigned tx "template" used for building + signing.
pub struct UnsignedTx {
    pub tx_type: TxType,
    pub chain_id: u64,
    pub nonce: u64,
    pub gas_price: U256,       // legacy / 2930
    pub max_fee: U256,         // 1559 / 4844
    pub max_priority_fee: U256, // 1559 / 4844
    pub gas_limit: u64,
    pub to: Option<H160>,
    pub value: U256,
    pub data: Vec<u8>,
    pub access_list: AccessList,
    pub max_fee_per_blob_gas: U256,        // 4844
    pub blob_versioned_hashes: Vec<H256>,  // 4844
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum TxType {
    Legacy,
    Eip2930,
    Eip1559,
    Eip4844,
}

/// Build and sign a real transaction with the given key.
pub fn sign_transaction(unsigned: &UnsignedTx, key: &KeyPair) -> Result<Transaction, String> {
    use rrx_crypto::KeyPair as K;
    let _ = std::marker::PhantomData::<K>;
    let template = Transaction::Legacy {
        nonce: unsigned.nonce,
        gas_price: unsigned.gas_price,
        gas_limit: unsigned.gas_limit,
        to: unsigned.to,
        value: unsigned.value,
        data: unsigned.data.clone(),
        chain_id: Some(unsigned.chain_id),
        v: 0,
        r: [0u8; 32],
        s: [0u8; 32],
    };
    // choose the actual type
    let tx = match unsigned.tx_type {
        TxType::Legacy => template,
        TxType::Eip2930 => Transaction::Eip2930 {
            chain_id: unsigned.chain_id,
            nonce: unsigned.nonce,
            gas_price: unsigned.gas_price,
            gas_limit: unsigned.gas_limit,
            to: unsigned.to,
            value: unsigned.value,
            data: unsigned.data.clone(),
            access_list: unsigned.access_list.clone(),
            y_parity: false,
            r: [0u8; 32],
            s: [0u8; 32],
        },
        TxType::Eip1559 => Transaction::Eip1559 {
            chain_id: unsigned.chain_id,
            nonce: unsigned.nonce,
            max_priority_fee: unsigned.max_priority_fee,
            max_fee: unsigned.max_fee,
            gas_limit: unsigned.gas_limit,
            to: unsigned.to,
            value: unsigned.value,
            data: unsigned.data.clone(),
            access_list: unsigned.access_list.clone(),
            y_parity: false,
            r: [0u8; 32],
            s: [0u8; 32],
        },
        TxType::Eip4844 => {
            let to = unsigned.to.ok_or("blob tx requires a 'to' contract address")?;
            Transaction::Eip4844 {
                chain_id: unsigned.chain_id,
                nonce: unsigned.nonce,
                max_priority_fee: unsigned.max_priority_fee,
                max_fee: unsigned.max_fee,
                gas_limit: unsigned.gas_limit,
                to,
                value: unsigned.value,
                data: unsigned.data.clone(),
                access_list: unsigned.access_list.clone(),
                max_fee_per_blob_gas: unsigned.max_fee_per_blob_gas,
                blob_versioned_hashes: unsigned.blob_versioned_hashes.clone(),
                y_parity: false,
                r: [0u8; 32],
                s: [0u8; 32],
            }
        }
    };
    let hash = tx.signing_hash();
    let (sig, recid) = key.sign_hash(&hash)?;
    let raw = sig.to_bytes();
    let mut r = [0u8; 32];
    r.copy_from_slice(&raw[..32]);
    let mut s = [0u8; 32];
    s.copy_from_slice(&raw[32..]);
    let signed = match tx {
        Transaction::Legacy { nonce, gas_price, gas_limit, to, value, data, chain_id, .. } => {
            let v = match chain_id {
                Some(cid) => recid.to_byte() as u64 + 35 + 2 * cid,
                None => recid.to_byte() as u64 + 27,
            };
            Transaction::Legacy { nonce, gas_price, gas_limit, to, value, data, chain_id, v, r, s }
        }
        Transaction::Eip2930 { chain_id, nonce, gas_price, gas_limit, to, value, data, access_list, .. } => {
            Transaction::Eip2930 { chain_id, nonce, gas_price, gas_limit, to, value, data, access_list, y_parity: recid.to_byte() == 1, r, s }
        }
        Transaction::Eip1559 { chain_id, nonce, max_priority_fee, max_fee, gas_limit, to, value, data, access_list, .. } => {
            Transaction::Eip1559 { chain_id, nonce, max_priority_fee, max_fee, gas_limit, to, value, data, access_list, y_parity: recid.to_byte() == 1, r, s }
        }
        Transaction::Eip4844 { chain_id, nonce, max_priority_fee, max_fee, gas_limit, to, value, data, access_list, max_fee_per_blob_gas, blob_versioned_hashes, .. } => {
            Transaction::Eip4844 { chain_id, nonce, max_priority_fee, max_fee, gas_limit, to, value, data, access_list, max_fee_per_blob_gas, blob_versioned_hashes, y_parity: recid.to_byte() == 1, r, s }
        }
    };
    // sanity: sender recovery must succeed and match
    let sender = signed.sender()?;
    let expected = key.address()?;
    if sender != expected {
        return Err(format!("sender mismatch: {:?} != {:?}", sender, expected));
    }
    Ok(signed)
}
