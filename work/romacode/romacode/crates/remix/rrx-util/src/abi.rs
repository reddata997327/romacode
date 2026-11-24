//! Minimal but real Ethereum ABI codec: head/tail encoding, dynamic types, arrays.

use crate::{keccak256, H160, U256};

#[derive(Clone, Debug, PartialEq)]
pub enum AbiValue {
    Uint(U256),
    /// Two's-complement 256-bit integer.
    Int(U256),
    Bool(bool),
    Address(H160),
    FixedBytes(Vec<u8>),
    Bytes(Vec<u8>),
    Str(String),
    Array(Vec<AbiValue>),
}

#[derive(Clone, Debug, PartialEq)]
enum Ty {
    Uint,
    Int,
    Bool,
    Address,
    FixedBytes(usize),
    Bytes,
    Str,
    Array { base: Box<Ty>, len: Option<usize> },
}

fn parse_type(t: &str) -> Result<Ty, String> {
    let t = t.trim();
    if let Some(open) = t.rfind('[') {
        if !t.ends_with(']') {
            return Err(format!("bad type {t}"));
        }
        let base = parse_type(&t[..open])?;
        let inner = &t[open + 1..t.len() - 1];
        let len = if inner.is_empty() {
            None
        } else {
            Some(inner.parse::<usize>().map_err(|_| format!("bad array length in {t}"))?)
        };
        return Ok(Ty::Array { base: Box::new(base), len });
    }
    match t {
        "uint" | "uint256" => Ok(Ty::Uint),
        "int" | "int256" => Ok(Ty::Int),
        "bool" => Ok(Ty::Bool),
        "address" => Ok(Ty::Address),
        "bytes" => Ok(Ty::Bytes),
        "string" => Ok(Ty::Str),
        _ => {
            if let Some(rest) = t.strip_prefix("uint") {
                let n: usize = rest.parse().map_err(|_| format!("bad type {t}"))?;
                if n == 0 || n > 256 || n % 8 != 0 {
                    return Err(format!("bad uint width {t}"));
                }
                Ok(Ty::Uint)
            } else if let Some(rest) = t.strip_prefix("int") {
                let n: usize = rest.parse().map_err(|_| format!("bad type {t}"))?;
                if n == 0 || n > 256 || n % 8 != 0 {
                    return Err(format!("bad int width {t}"));
                }
                Ok(Ty::Int)
            } else if let Some(rest) = t.strip_prefix("bytes") {
                let n: usize = rest.parse().map_err(|_| format!("bad type {t}"))?;
                if n == 0 || n > 32 {
                    return Err(format!("bad bytes width {t}"));
                }
                Ok(Ty::FixedBytes(n))
            } else {
                Err(format!("unsupported type {t} (tuples not supported)"))
            }
        }
    }
}

fn is_dynamic(t: &Ty) -> bool {
    match t {
        Ty::Bytes | Ty::Str => true,
        Ty::Array { base, len } => len.is_none() || is_dynamic(base),
        _ => false,
    }
}

fn be32(v: &U256) -> [u8; 32] {
    crate::u256_to_be32(v)
}

fn slot_uint(n: usize) -> Vec<u8> {
    be32(&U256::from(n)).to_vec()
}

fn pad32_right(data: &[u8]) -> Vec<u8> {
    // exactly ((len + 31) / 32) * 32 bytes — zero bytes for empty input
    let mut v = data.to_vec();
    while v.len() % 32 != 0 {
        v.push(0);
    }
    v
}

/// keccak256("name(type1,type2)")[0..4]
pub fn selector(name: &str, types: &[&str]) -> [u8; 4] {
    let sig = format!("{}({})", name, types.join(","));
    let h = keccak256(sig.as_bytes());
    [h[0], h[1], h[2], h[3]]
}

fn encode_static(t: &Ty, v: &AbiValue) -> Result<Vec<u8>, String> {
    Ok(match (t, v) {
        (Ty::Uint, AbiValue::Uint(u)) => be32(u).to_vec(),
        (Ty::Int, AbiValue::Int(u)) => be32(u).to_vec(),
        (Ty::Int, AbiValue::Uint(u)) => be32(u).to_vec(),
        (Ty::Bool, AbiValue::Bool(b)) => {
            let mut s = [0u8; 32];
            s[31] = *b as u8;
            s.to_vec()
        }
        (Ty::Address, AbiValue::Address(a)) => {
            let mut s = vec![0u8; 12];
            s.extend_from_slice(a.as_bytes());
            s
        }
        (Ty::FixedBytes(n), AbiValue::FixedBytes(b)) => {
            if b.len() != *n {
                return Err(format!("bytes{} expected, got {} bytes", n, b.len()));
            }
            pad32_right(b)
        }
        _ => return Err(format!("type/value mismatch for {:?}", t)),
    })
}

fn encode_dynamic(t: &Ty, v: &AbiValue) -> Result<Vec<u8>, String> {
    match (t, v) {
        (Ty::Bytes, AbiValue::Bytes(b)) => {
            let mut out = slot_uint(b.len());
            out.extend(pad32_right(b));
            Ok(out)
        }
        (Ty::Str, AbiValue::Str(s)) => {
            let mut out = slot_uint(s.len());
            out.extend(pad32_right(s.as_bytes()));
            Ok(out)
        }
        (Ty::Array { base, len }, AbiValue::Array(items)) => {
            if let Some(n) = len {
                if items.len() != *n {
                    return Err(format!("array length mismatch: expected {n}, got {}", items.len()));
                }
            }
            let types: Vec<Ty> = (0..items.len()).map(|_| (**base).clone()).collect();
            if len.is_some() {
                // fixed-length array: contents only
                encode_seq(&types, items)
            } else {
                let mut out = slot_uint(items.len());
                out.extend(encode_seq(&types, items)?);
                Ok(out)
            }
        }
        _ => Err(format!("type/value mismatch for dynamic {:?}", t)),
    }
}

fn encode_seq(types: &[Ty], values: &[AbiValue]) -> Result<Vec<u8>, String> {
    if types.len() != values.len() {
        return Err(format!("{} types but {} values", types.len(), values.len()));
    }
    let mut total_head = 0usize;
    for t in types {
        if is_dynamic(t) {
            total_head += 32;
        } else if let Ty::Array { base, len: Some(n) } = t {
            if is_dynamic(base) {
                total_head += 32;
            } else {
                total_head += 32 * n;
            }
        } else {
            total_head += 32;
        }
    }
    let mut head: Vec<u8> = Vec::with_capacity(total_head);
    let mut tail: Vec<u8> = Vec::new();
    for (t, v) in types.iter().zip(values) {
        if is_dynamic(t) {
            let offset = total_head + tail.len();
            head.extend_from_slice(&slot_uint(offset));
            tail.extend_from_slice(&encode_dynamic(t, v)?);
        } else if let Ty::Array { base, len: Some(n) } = t {
            if let AbiValue::Array(items) = v {
                if items.len() != *n {
                    return Err(format!("array length mismatch: expected {n}, got {}", items.len()));
                }
                for it in items {
                    head.extend_from_slice(&encode_static(base, it)?);
                }
            } else {
                return Err(format!("expected array for {:?}", t));
            }
        } else {
            head.extend_from_slice(&encode_static(t, v)?);
        }
    }
    head.extend_from_slice(&tail);
    Ok(head)
}

/// Encode arguments for `types` (no function selector).
pub fn encode(types: &[&str], values: &[AbiValue]) -> Result<Vec<u8>, String> {
    let tys: Vec<Ty> = types.iter().map(|t| parse_type(t)).collect::<Result<_, _>>()?;
    encode_seq(&tys, values)
}

fn read_slot(data: &[u8], off: usize) -> Result<[u8; 32], String> {
    if off + 32 > data.len() {
        return Err(format!("abi decode: out of bounds at {off} (len {})", data.len()));
    }
    let mut s = [0u8; 32];
    s.copy_from_slice(&data[off..off + 32]);
    Ok(s)
}

fn decode_static(t: &Ty, slot: &[u8; 32]) -> Result<AbiValue, String> {
    Ok(match t {
        Ty::Uint => AbiValue::Uint(U256::from_big_endian(slot)),
        Ty::Int => AbiValue::Int(U256::from_big_endian(slot)),
        Ty::Bool => AbiValue::Bool(slot[31] != 0),
        Ty::Address => AbiValue::Address(H160::from_slice(&slot[12..])),
        Ty::FixedBytes(n) => AbiValue::FixedBytes(slot[..*n].to_vec()),
        _ => return Err(format!("not a static type: {:?}", t)),
    })
}

/// Sanity cap for attacker-controlled dynamic lengths/offsets (1 MiB).
const ABI_MAX_DYNAMIC: usize = 1_048_576;

fn decode_len(region: &[u8]) -> Result<usize, String> {
    let len = U256::from_big_endian(&read_slot(region, 0)?);
    if len > U256::from(ABI_MAX_DYNAMIC) {
        return Err("abi decode: dynamic length too large".into());
    }
    let len = len.as_u64() as usize;
    if 32u64.saturating_add(len as u64) > region.len() as u64 {
        return Err("abi decode: dynamic data out of bounds".into());
    }
    Ok(len)
}

fn decode_dynamic(t: &Ty, region: &[u8]) -> Result<AbiValue, String> {
    match t {
        Ty::Bytes => {
            let len = decode_len(region)?;
            Ok(AbiValue::Bytes(region[32..32 + len].to_vec()))
        }
        Ty::Str => {
            let len = decode_len(region)?;
            Ok(AbiValue::Str(String::from_utf8_lossy(&region[32..32 + len]).into_owned()))
        }
        Ty::Array { base, len: None } => {
            let cnt = U256::from_big_endian(&read_slot(region, 0)?);
            if cnt > U256::from(ABI_MAX_DYNAMIC) {
                return Err("abi decode: array length too large".into());
            }
            let cnt = cnt.as_u64() as usize;
            let types: Vec<Ty> = (0..cnt).map(|_| (**base).clone()).collect();
            Ok(AbiValue::Array(decode_seq(&types, &region[32..])?))
        }
        Ty::Array { base, len: Some(n) } => {
            let types: Vec<Ty> = (0..*n).map(|_| (**base).clone()).collect();
            Ok(AbiValue::Array(decode_seq(&types, region)?))
        }
        _ => Err(format!("not a dynamic type: {:?}", t)),
    }
}

fn decode_seq(types: &[Ty], data: &[u8]) -> Result<Vec<AbiValue>, String> {
    let mut out = Vec::new();
    let mut off = 0usize;
    for t in types {
        if is_dynamic(t) {
            let slot = read_slot(data, off)?;
            let target = U256::from_big_endian(&slot);
            if target > U256::from(data.len()) {
                return Err("abi decode: bad offset".into());
            }
            let target = target.as_u64() as usize;
            out.push(decode_dynamic(t, &data[target..])?);
            off += 32;
        } else if let Ty::Array { base, len: Some(n) } = t {
            let types: Vec<Ty> = (0..*n).map(|_| (**base).clone()).collect();
            out.push(AbiValue::Array(decode_seq(&types, &data[off..])?));
            off += 32 * n;
        } else {
            let slot = read_slot(data, off)?;
            out.push(decode_static(t, &slot)?);
            off += 32;
        }
    }
    Ok(out)
}

/// Decode output/argument data for `types`.
pub fn decode(types: &[&str], data: &[u8]) -> Result<Vec<AbiValue>, String> {
    let tys: Vec<Ty> = types.iter().map(|t| parse_type(t)).collect::<Result<_, _>>()?;
    decode_seq(&tys, data)
}

/// Decode a Solidity revert payload: `Error(string)` (0x08c379a0) or `Panic(uint256)` (0x4e487b71).
pub fn decode_revert_reason(output: &[u8]) -> Option<String> {
    if output.len() < 4 {
        return None;
    }
    if output[0..4] == [0x08, 0xc3, 0x79, 0xa0] {
        if let Ok(vals) = decode(&["string"], &output[4..]) {
            if let Some(AbiValue::Str(s)) = vals.into_iter().next() {
                return Some(s);
            }
        }
        return Some("Error(string) <undecodable>".into());
    }
    if output[0..4] == [0x4e, 0x48, 0x7b, 0x71] {
        if let Ok(vals) = decode(&["uint256"], &output[4..]) {
            if let Some(AbiValue::Uint(p)) = vals.into_iter().next() {
                return Some(format!("Panic({})", p));
            }
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{from_hex, to_hex};

    #[test]
    fn known_selectors() {
        assert_eq!(to_hex(&selector("transfer", &["address", "uint256"])), "a9059cbb");
        assert_eq!(to_hex(&selector("balanceOf", &["address"])), "70a08231");
    }

    #[test]
    fn encode_static_args() {
        let a = AbiValue::Address(H160::from_slice(&from_hex("1111111111111111111111111111111111111111").unwrap()));
        let data = encode(&["address", "uint256"], &[a, AbiValue::Uint(U256::from(1000u64))]).unwrap();
        assert_eq!(data.len(), 64);
        assert_eq!(to_hex(&data[12..32]), "1111111111111111111111111111111111111111");
        let vals = decode(&["address", "uint256"], &data).unwrap();
        assert_eq!(
            vals[0],
            AbiValue::Address(H160::from_slice(&from_hex("1111111111111111111111111111111111111111").unwrap()))
        );
        assert_eq!(vals[1], AbiValue::Uint(U256::from(1000u64)));
    }

    #[test]
    fn dynamic_roundtrip() {
        let vals = vec![
            AbiValue::Str("hi there".into()),
            AbiValue::Array(vec![AbiValue::Uint(U256::from(1u64)), AbiValue::Uint(U256::from(2u64))]),
            AbiValue::Bytes(vec![0xde, 0xad]),
        ];
        let data = encode(&["string", "uint256[]", "bytes"], &vals).unwrap();
        let back = decode(&["string", "uint256[]", "bytes"], &data).unwrap();
        assert_eq!(back, vals);
    }

    #[test]
    fn fixed_array_roundtrip() {
        let vals = vec![AbiValue::Array(vec![
            AbiValue::Uint(U256::from(7u64)),
            AbiValue::Uint(U256::from(9u64)),
        ])];
        let data = encode(&["uint256[2]"], &vals).unwrap();
        assert_eq!(data.len(), 64);
        let back = decode(&["uint256[2]"], &data).unwrap();
        assert_eq!(back, vals);
    }

    #[test]
    fn revert_reason() {
        let inner = encode(&["string"], &[AbiValue::Str("insufficient balance".into())]).unwrap();
        let mut out = vec![0x08, 0xc3, 0x79, 0xa0];
        out.extend(inner);
        assert_eq!(decode_revert_reason(&out).as_deref(), Some("insufficient balance"));
    }
}
