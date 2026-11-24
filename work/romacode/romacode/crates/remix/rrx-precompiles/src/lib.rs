//! rrx-precompiles: all Ethereum precompiled contracts (addresses 0x01..0x09),
//! implemented to the specifications (EIP-2, EIP-197 + EIP-1108, EIP-152, EIP-2565, EIP-2537 n/a).
//!
//! - 0x01 ecrecover, 0x02 sha256, 0x03 ripemd160, 0x04 identity
//! - 0x05 modexp (EIP-2565 gas)
//! - 0x06 alt_bn128 G1 add, 0x07 G1 mul, 0x08 pairing (EIP-1108 gas)
//! - 0x09 blake2 F compression function (EIP-152)

use num_bigint::BigUint;
use rrx_util::{keccak256, H256, U256};
use sha2::Digest;
use substrate_bn::Group;

pub const ECRECOVER: u8 = 0x01;
pub const SHA256: u8 = 0x02;
pub const RIPEMD160: u8 = 0x03;
pub const IDENTITY: u8 = 0x04;
pub const MODEXP: u8 = 0x05;
pub const BN_ADD: u8 = 0x06;
pub const BN_MUL: u8 = 0x07;
pub const BN_PAIRING: u8 = 0x08;
pub const BLAKE2F: u8 = 0x09;

/// Highest active precompile address (Shanghai-era).
pub const LAST_PRECOMPILE: u8 = 0x09;

pub mod bls381;

pub enum PrecompileOutput {
    /// Successful execution; gas_used is the precompile's fixed/dynamic cost.
    Success { gas_used: u64, output: Vec<u8> },
    /// Failure (bad input or insufficient gas): ALL provided gas is consumed,
    /// no output — matching mainnet semantics.
    Failure { gas_used: u64 },
}

/// Execute precompile `addr` with `input` and at most `gas_provided` units.
pub fn execute(addr: u8, input: &[u8], gas_provided: u64) -> PrecompileOutput {
    let cost = gas_cost(addr, input);
    if cost > gas_provided as u128 {
        return PrecompileOutput::Failure { gas_used: gas_provided };
    }
    match run(addr, input) {
        Ok(output) => PrecompileOutput::Success { gas_used: cost as u64, output },
        Err(_) => PrecompileOutput::Failure { gas_used: gas_provided },
    }
}

/// Static/dynamic gas cost of a precompile call for this input.
pub fn gas_cost(addr: u8, input: &[u8]) -> u128 {
    match addr {
        ECRECOVER => 3000,
        SHA256 => 60 + 12 * words(input.len()) as u128,
        RIPEMD160 => 600 + 120 * words(input.len()) as u128,
        IDENTITY => 15 + 3 * words(input.len()) as u128,
        MODEXP => modexp_gas(input),
        BN_ADD => 150,
        BN_MUL => 6000,
        BN_PAIRING => {
            if input.len() % 192 != 0 {
                return u128::MAX; // invalid input shape: unpayable -> always fails
            }
            34000 + 45000 * (input.len() / 192) as u128
        }
        BLAKE2F => {
            if input.len() != 213 {
                return u128::MAX;
            }
            u128::from(u32::from_be_bytes([input[0], input[1], input[2], input[3]]))
        }
        bls381::POINT_EVAL
        | bls381::G1ADD
        | bls381::G1MSM
        | bls381::G2ADD
        | bls381::G2MSM
        | bls381::PAIRING
        | bls381::MAP_FP_TO_G1
        | bls381::MAP_FP2_TO_G2 => bls381::gas_cost(addr, input),
        _ => u128::MAX,
    }
}

fn words(len: usize) -> u64 {
    (len as u64).div_ceil(32)
}

fn run(addr: u8, input: &[u8]) -> Result<Vec<u8>, String> {
    match addr {
        ECRECOVER => ecrecover(input),
        SHA256 => {
            let mut h = sha2::Sha256::new();
            h.update(input);
            Ok(h.finalize().to_vec())
        }
        RIPEMD160 => {
            let mut h = ripemd::Ripemd160::new();
            h.update(input);
            let d = h.finalize();
            let mut out = vec![0u8; 32];
            out[12..32].copy_from_slice(&d);
            Ok(out)
        }
        IDENTITY => Ok(input.to_vec()),
        MODEXP => modexp_run(input),
        BN_ADD => bn_add(input),
        BN_MUL => bn_mul(input),
        BN_PAIRING => bn_pairing(input),
        BLAKE2F => blake2f(input),
        bls381::POINT_EVAL
        | bls381::G1ADD
        | bls381::G1MSM
        | bls381::G2ADD
        | bls381::G2MSM
        | bls381::PAIRING
        | bls381::MAP_FP_TO_G1
        | bls381::MAP_FP2_TO_G2 => bls381::run(addr, input),
        _ => Err(format!("unknown precompile address 0x{addr:02x}")),
    }
}

// ---------- 0x01 ecrecover ----------

fn ecrecover(input: &[u8]) -> Result<Vec<u8>, String> {
    // geth zero-pads shorter inputs to 128 bytes (and truncates longer ones);
    // every failure case returns success with empty output at the 3000 gas cost.
    let mut padded = [0u8; 128];
    let n = input.len().min(128);
    padded[..n].copy_from_slice(&input[..n]);
    let input: &[u8] = &padded;

    let mut hash = [0u8; 32];
    hash.copy_from_slice(&input[0..32]);
    let v = U256::from_big_endian(&input[32..64]);
    let mut r = [0u8; 32];
    r.copy_from_slice(&input[64..96]);
    let mut s = [0u8; 32];
    s.copy_from_slice(&input[96..128]);

    // EIP-2: v must be 27 or 28; r, s must be in [1, n-1]
    if v != U256::from(27u8) && v != U256::from(28u8) {
        return Ok(vec![]);
    }
    if r.iter().all(|&b| b == 0) || s.iter().all(|&b| b == 0) {
        return Ok(vec![]);
    }
    let recid = v.as_u32() as u8 - 27;
    match rrx_crypto::recover_from_rsv(&H256(hash), &r, &s, recid) {
        Ok(addr) => {
            let mut out = vec![0u8; 32];
            out[12..32].copy_from_slice(addr.as_bytes());
            Ok(out)
        }
        Err(_) => Ok(vec![]),
    }
}

// ---------- 0x05 modexp (EIP-2565) ----------

const MODEXP_MAX_LEN: usize = 4096;

fn read_len(input: &[u8], off: usize) -> Result<usize, String> {
    if off + 32 > input.len() {
        return Err("modexp input too short".into());
    }
    let v = U256::from_big_endian(&input[off..off + 32]);
    if v > U256::from(MODEXP_MAX_LEN as u64) {
        return Err("modexp length too large".into());
    }
    Ok(v.as_usize())
}

/// Zero-padded read of `len` bytes at `off` (missing bytes read as zero, per spec).
fn read_bytes(input: &[u8], off: usize, len: usize) -> Vec<u8> {
    let mut out = vec![0u8; len];
    let avail = input.len().saturating_sub(off).min(len);
    out[..avail].copy_from_slice(&input[off..off + avail]);
    out
}

fn bit_len(bytes: &[u8]) -> u64 {
    for (i, &b) in bytes.iter().enumerate() {
        if b != 0 {
            let leading = b.leading_zeros() as u64;
            return (bytes.len() - i - 1) as u64 * 8 + (8 - leading);
        }
    }
    0
}

pub fn modexp_gas(input: &[u8]) -> u128 {
    if input.len() < 96 {
        return 200;
    }
    let b_len = U256::from_big_endian(&input[0..32]);
    let e_len = U256::from_big_endian(&input[32..64]);
    let m_len = U256::from_big_endian(&input[64..96]);
    let cap = U256::from(MODEXP_MAX_LEN as u64);
    if b_len > cap || e_len > cap || m_len > cap {
        return u128::MAX;
    }
    let (b_len, e_len, m_len) = (b_len.as_usize(), e_len.as_usize(), m_len.as_usize());

    if b_len == 0 && e_len == 0 && m_len == 0 {
        return 200;
    }
    let max_len = b_len.max(e_len).max(m_len);
    let complexity = (((max_len + 7) / 8) as u128).pow(2);

    let iter: u128 = if e_len <= 32 {
        let exp_head = read_bytes(input, 96 + b_len, e_len);
        let bits = bit_len(&exp_head);
        if bits == 0 {
            0
        } else {
            (bits - 1) as u128
        }
    } else {
        let first32 = read_bytes(input, 96 + b_len, 32);
        let bits = bit_len(&first32);
        if bits == 0 {
            (8 * (e_len - 32)) as u128
        } else {
            8 * (e_len as u128 - 32) + bits as u128 - 1
        }
    };

    std::cmp::max(200, complexity * iter / 3)
}

fn modexp_run(input: &[u8]) -> Result<Vec<u8>, String> {
    // geth zero-pads inputs shorter than the 96-byte header (which then decodes
    // as all-zero lengths and succeeds with empty output) instead of failing.
    let mut padded = input.to_vec();
    if padded.len() < 96 {
        padded.resize(96, 0);
    }
    let input: &[u8] = &padded;
    let b_len = read_len(input, 0)?;
    let e_len = read_len(input, 32)?;
    let m_len = read_len(input, 64)?;
    if 96 + b_len.checked_add(e_len).ok_or("overflow")?.checked_add(m_len).ok_or("overflow")? > usize::MAX
        && false
    {
        return Err("overflow".into());
    }
    let base = BigUint::from_bytes_be(&read_bytes(input, 96, b_len));
    let exp = BigUint::from_bytes_be(&read_bytes(input, 96 + b_len, e_len));
    let modulus = BigUint::from_bytes_be(&read_bytes(input, 96 + b_len + e_len, m_len));

    let result = if modulus == BigUint::ZERO {
        BigUint::ZERO
    } else {
        base.modpow(&exp, &modulus)
    };
    let bytes = result.to_bytes_be();
    let mut out = vec![0u8; m_len];
    let copy_len = bytes.len().min(m_len);
    out[m_len - copy_len..].copy_from_slice(&bytes[bytes.len() - copy_len..]);
    Ok(out)
}

// ---------- 0x06/0x07/0x08 alt_bn128 ----------

fn to_bn(v: &U256) -> substrate_bn::arith::U256 {
    substrate_bn::arith::U256::from(v.0)
}

fn from_bn(v: substrate_bn::arith::U256) -> U256 {
    let lo = v.0[0];
    let hi = v.0[1];
    U256([lo as u64, (lo >> 64) as u64, hi as u64, (hi >> 64) as u64])
}

fn word(input: &[u8], off: usize) -> Result<U256, String> {
    if off + 32 > input.len() {
        return Err("input too short".into());
    }
    Ok(U256::from_big_endian(&input[off..off + 32]))
}

fn fq(word: U256) -> Result<substrate_bn::Fq, String> {
    substrate_bn::Fq::from_u256(to_bn(&word)).map_err(|_| "field element out of range".into())
}

fn write_fq(out: &mut Vec<u8>, f: substrate_bn::Fq) -> Result<(), String> {
    let mut buf = [0u8; 32];
    f.to_big_endian(&mut buf).map_err(|e| format!("{e:?}"))?;
    out.extend_from_slice(&buf);
    Ok(())
}

/// G1 from 64 bytes (x, y). All-zero point is infinity.
fn read_g1(input: &[u8], off: usize) -> Result<substrate_bn::G1, String> {
    let x = word(input, off)?;
    let y = word(input, off + 32)?;
    if x.is_zero() && y.is_zero() {
        return Ok(substrate_bn::G1::zero());
    }
    let affine = substrate_bn::AffineG1::new(fq(x)?, fq(y)?).map_err(|e| format!("{e:?}"))?;
    Ok(substrate_bn::G1::from(affine))
}

/// G2 from 128 bytes. EIP-197 Fq2 encoding: a*i + b encoded as (a, b) —
/// imaginary coefficient FIRST, then real. substrate-bn Fq2::new(real, imag).
fn read_g2(input: &[u8], off: usize) -> Result<substrate_bn::G2, String> {
    let x_im = word(input, off)?;
    let x_re = word(input, off + 32)?;
    let y_im = word(input, off + 64)?;
    let y_re = word(input, off + 96)?;
    if x_im.is_zero() && x_re.is_zero() && y_im.is_zero() && y_re.is_zero() {
        return Ok(substrate_bn::G2::zero());
    }
    let x = substrate_bn::Fq2::new(fq(x_re)?, fq(x_im)?);
    let y = substrate_bn::Fq2::new(fq(y_re)?, fq(y_im)?);
    let affine = substrate_bn::AffineG2::new(x, y).map_err(|e| format!("{e:?}"))?;
    Ok(substrate_bn::G2::from(affine))
}

fn write_g1(out: &mut Vec<u8>, p: substrate_bn::G1) -> Result<(), String> {
    if p.is_zero() {
        out.extend_from_slice(&[0u8; 64]);
        return Ok(());
    }
    let mut p = p;
    p.normalize();
    write_fq(out, p.x())?;
    write_fq(out, p.y())?;
    Ok(())
}

fn bn_add(input: &[u8]) -> Result<Vec<u8>, String> {
    if input.len() != 128 {
        return Err("bn128_add input must be 128 bytes".into());
    }
    let a = read_g1(input, 0)?;
    let b = read_g1(input, 64)?;
    let mut out = vec![];
    write_g1(&mut out, a + b)?;
    Ok(out)
}

fn bn_mul(input: &[u8]) -> Result<Vec<u8>, String> {
    if input.len() != 96 {
        return Err("bn128_mul input must be 96 bytes".into());
    }
    let p = read_g1(input, 0)?;
    let k = word(input, 64)?;
    let fr = substrate_bn::Fr::new_mul_factor(to_bn(&k));
    let mut out = vec![];
    write_g1(&mut out, p * fr)?;
    Ok(out)
}

fn bn_pairing(input: &[u8]) -> Result<Vec<u8>, String> {
    if input.is_empty() || input.len() % 192 != 0 {
        return Err("bn128_pairing input must be a multiple of 192 bytes".into());
    }
    let mut pairs = Vec::new();
    let mut off = 0;
    while off < input.len() {
        let g1 = read_g1(input, off)?;
        let g2 = read_g2(input, off + 64)?;
        pairs.push((g1, g2));
        off += 192;
    }
    let product = substrate_bn::pairing_batch(&pairs);
    let mut out = vec![0u8; 32];
    out[31] = if product == substrate_bn::Gt::one() { 1 } else { 0 };
    Ok(out)
}

// ---------- 0x09 blake2 F (EIP-152) ----------

const BLAKE2_IV: [u64; 8] = [
    0x6a09e667f3bcc908,
    0xbb67ae8584caa73b,
    0x3c6ef372fe94f82b,
    0xa54ff53a5f1d36f1,
    0x510e527fade682d1,
    0x9b05688c2b3e6c1f,
    0x1f83d9abfb41bd6b,
    0x5be0cd19137e2179,
];

const SIGMA: [[usize; 16]; 10] = [
    [0, 1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 12, 13, 14, 15],
    [14, 10, 4, 8, 9, 15, 13, 6, 1, 12, 0, 2, 11, 7, 5, 3],
    [11, 8, 12, 0, 5, 2, 15, 13, 10, 14, 3, 6, 7, 1, 9, 4],
    [7, 9, 3, 1, 13, 12, 11, 14, 2, 6, 5, 10, 4, 0, 15, 8],
    [9, 0, 5, 7, 2, 4, 10, 15, 14, 1, 11, 12, 6, 8, 3, 13],
    [2, 12, 6, 10, 0, 11, 8, 3, 4, 13, 7, 5, 15, 14, 1, 9],
    [12, 5, 1, 15, 14, 13, 4, 10, 0, 7, 6, 3, 9, 2, 8, 11],
    [13, 11, 7, 14, 12, 1, 3, 9, 5, 0, 15, 4, 8, 6, 2, 10],
    [6, 15, 14, 9, 11, 3, 0, 8, 12, 2, 13, 7, 1, 4, 10, 5],
    [10, 2, 8, 4, 7, 6, 1, 5, 15, 11, 9, 14, 3, 12, 13, 0],
];

fn g(v: &mut [u64; 16], a: usize, b: usize, c: usize, d: usize, x: u64, y: u64) {
    v[a] = v[a].wrapping_add(v[b]).wrapping_add(x);
    v[d] = (v[d] ^ v[a]).rotate_right(32);
    v[c] = v[c].wrapping_add(v[d]);
    v[b] = (v[b] ^ v[c]).rotate_right(24);
    v[a] = v[a].wrapping_add(v[b]).wrapping_add(y);
    v[d] = (v[d] ^ v[a]).rotate_right(16);
    v[c] = v[c].wrapping_add(v[d]);
    v[b] = (v[b] ^ v[c]).rotate_right(63);
}

fn le_u64(bytes: &[u8]) -> u64 {
    let mut b = [0u8; 8];
    b.copy_from_slice(bytes);
    u64::from_le_bytes(b)
}

fn blake2f(input: &[u8]) -> Result<Vec<u8>, String> {
    if input.len() != 213 {
        return Err("input length for BLAKE2 F precompile should be exactly 213 bytes".into());
    }
    let rounds = u32::from_be_bytes([input[0], input[1], input[2], input[3]]) as usize;
    let mut h = [0u64; 8];
    for i in 0..8 {
        h[i] = le_u64(&input[4 + i * 8..12 + i * 8]);
    }
    let mut m = [0u64; 16];
    for i in 0..16 {
        m[i] = le_u64(&input[68 + i * 8..76 + i * 8]);
    }
    let t0 = le_u64(&input[196..204]);
    let t1 = le_u64(&input[204..212]);
    let last = match input[212] {
        0 => false,
        1 => true,
        _ => return Err("incorrect final block indicator flag".into()),
    };

    let mut v = [0u64; 16];
    v[..8].copy_from_slice(&h);
    v[8..].copy_from_slice(&BLAKE2_IV);
    v[12] ^= t0;
    v[13] ^= t1;
    if last {
        v[14] ^= u64::MAX;
    }
    for r in 0..rounds {
        let s = &SIGMA[r % 10];
        g(&mut v, 0, 4, 8, 12, m[s[0]], m[s[1]]);
        g(&mut v, 1, 5, 9, 13, m[s[2]], m[s[3]]);
        g(&mut v, 2, 6, 10, 14, m[s[4]], m[s[5]]);
        g(&mut v, 3, 7, 11, 15, m[s[6]], m[s[7]]);
        g(&mut v, 0, 5, 10, 15, m[s[8]], m[s[9]]);
        g(&mut v, 1, 6, 11, 12, m[s[10]], m[s[11]]);
        g(&mut v, 2, 7, 8, 13, m[s[12]], m[s[13]]);
        g(&mut v, 3, 4, 9, 14, m[s[14]], m[s[15]]);
    }
    for i in 0..8 {
        h[i] ^= v[i] ^ v[i + 8];
    }

    let mut out = Vec::with_capacity(64);
    for w in h {
        out.extend_from_slice(&w.to_le_bytes());
    }
    Ok(out)
}

/// keccak256 convenience used by tests and tools.
pub fn keccak(data: &[u8]) -> [u8; 32] {
    keccak256(data)
}

#[cfg(test)]
mod tests {
    use super::*;
    use rrx_util::{from_hex, to_hex};

    #[test]
    fn sha256_vector() {
        let out = run(SHA256, b"abc").unwrap();
        assert_eq!(to_hex(&out), "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad");
    }

    #[test]
    fn ripemd160_vector() {
        let out = run(RIPEMD160, b"abc").unwrap();
        assert_eq!(to_hex(&out), "0000000000000000000000008eb208f7e05d987a9b044a8e98c6b087f15a0bfc");
    }

    #[test]
    fn identity_vector() {
        let out = run(IDENTITY, &[1, 2, 3]).unwrap();
        assert_eq!(out, vec![1, 2, 3]);
        match execute(IDENTITY, &[1u8; 64], 1000) {
            PrecompileOutput::Success { gas_used, .. } => assert_eq!(gas_used, 15 + 3 * 2),
            _ => panic!("identity should succeed"),
        }
    }

    #[test]
    fn ecrecover_roundtrip() {
        let kp = rrx_crypto::KeyPair::new_random();
        let hash = H256(keccak256(b"some message"));
        let (sig, recid) = kp.sign_hash(&hash).unwrap();
        let raw = sig.to_bytes();
        let mut input = vec![0u8; 128];
        input[..32].copy_from_slice(hash.as_bytes());
        input[63] = 27 + recid.to_byte();
        input[64..96].copy_from_slice(&raw[..32]);
        input[96..128].copy_from_slice(&raw[32..]);
        let out = run(ECRECOVER, &input).unwrap();
        let addr = rrx_util::h160_from_hex(&to_hex(&out[12..32])).unwrap();
        assert_eq!(addr, kp.address().unwrap());
        // invalid v: geth semantics — ECRECOVER failures are SUCCESS with
        // empty output at the same gas cost, never an Err (see the doc
        // comment on `ecrecover`). Only out-of-gas / unknown addresses Err.
        input[63] = 29;
        assert!(run(ECRECOVER, &input).unwrap().is_empty());
    }

    #[test]
    fn modexp_small() {
        // 6^9 mod 11 = 2, all lengths 32
        let mut input = vec![0u8; 96 + 32 * 3];
        input[31] = 32;
        input[63] = 32;
        input[95] = 32;
        input[95 + 32] = 6; // base at 96..128
        input[127 + 32] = 9; // exp at 128..160
        input[159 + 32] = 11; // modulus at 160..192
        let out = run(MODEXP, &input).unwrap();
        assert_eq!(out.len(), 32);
        assert_eq!(out[31], 2);
        match execute(MODEXP, &input, u64::MAX) {
            PrecompileOutput::Success { gas_used, .. } => {
                // max_len=32 -> words 4 -> complexity 16; bitlen(9)=4 -> iter 3; 16*3/3=16 -> min 200
                assert_eq!(gas_used, 200);
            }
            _ => panic!("modexp should succeed"),
        }
    }

    #[test]
    fn modexp_zero_exp() {
        let mut input = vec![0u8; 96 + 32 * 3];
        input[31] = 32;
        input[63] = 32;
        input[95] = 32;
        input[95 + 32] = 7;
        // exp = 0, modulus = 5 -> 7^0 mod 5 = 1
        input[159 + 32] = 5;
        let out = run(MODEXP, &input).unwrap();
        assert_eq!(out[31], 1);
    }

    #[test]
    fn modexp_big_matches_bigint() {
        // (123456789)^987654321 mod 1000000007, odd exponent length 3
        let mut input = vec![0u8; 96 + 32 * 3];
        input[31] = 4;
        input[63] = 3;
        input[95] = 4;
        input[96..100].copy_from_slice(&123456789u32.to_be_bytes());
        // packed layout: exp at 96+4, modulus at 96+4+3
        input[100..103].copy_from_slice(&987654321u32.to_be_bytes()[1..]);
        input[103..107].copy_from_slice(&1000000007u32.to_be_bytes());
        let out = run(MODEXP, &input).unwrap();
        let b = BigUint::from_bytes_be(&123456789u32.to_be_bytes());
        let e = BigUint::from_bytes_be(&987654321u32.to_be_bytes()[1..]);
        let m = BigUint::from_bytes_be(&1000000007u32.to_be_bytes());
        let expected = b.modpow(&e, &m).to_bytes_be();
        assert_eq!(out, expected);
    }

    #[test]
    fn bn_add_mul_consistency() {
        // g1 + g1 == g1 * 2
        let one = substrate_bn::G1::one();
        let mut g1_bytes = vec![];
        write_g1(&mut g1_bytes, one).unwrap();
        let mut input = g1_bytes.clone();
        input.extend_from_slice(&g1_bytes);
        let sum = run(BN_ADD, &input).unwrap();

        let mut mul_input = g1_bytes.clone();
        mul_input.extend_from_slice(&rrx_util::u256_to_be32(&U256::from(2u8)));
        let double = run(BN_MUL, &mul_input).unwrap();
        assert_eq!(sum, double);
    }

    #[test]
    fn bn_g1_encoding_roundtrip() {
        let mut out = vec![];
        write_g1(&mut out, substrate_bn::G1::one()).unwrap();
        let p = read_g1(&out, 0).unwrap();
        assert_eq!(p, substrate_bn::G1::one());
        // not on curve -> failure
        let mut bad = vec![0u8; 64];
        bad[0] = 1;
        assert!(read_g1(&bad, 0).is_err());
    }

    #[test]
    fn bn_g2_encoding_order() {
        // EIP-197: Fq2 = a*i + b encoded as (a, b) = (imag, real)
        let g2 = substrate_bn::G2::one();
        let mut g2n = g2;
        g2n.normalize();
        let mut bytes = vec![];
        write_fq(&mut bytes, g2n.x().imaginary()).unwrap();
        write_fq(&mut bytes, g2n.x().real()).unwrap();
        write_fq(&mut bytes, g2n.y().imaginary()).unwrap();
        write_fq(&mut bytes, g2n.y().real()).unwrap();
        let back = read_g2(&bytes, 0).unwrap();
        assert_eq!(back, g2);
    }

    #[test]
    fn bn_pairing_bilinearity() {
        let g1 = substrate_bn::G1::one();
        let g2 = substrate_bn::G2::one();
        let two = substrate_bn::Fr::new_mul_factor(to_bn(&U256::from(2u8)));
        // e(2*g1, g2) == e(g1, 2*g2)
        let a = substrate_bn::pairing(g1 * two, g2);
        let b = substrate_bn::pairing(g1, g2 * two);
        assert!(a == b);
        // pairing precompile: e(g1,g2) * e(-g1,g2) == 1 -> output 1
        let mut bytes = vec![];
        write_g1(&mut bytes, g1).unwrap();
        let mut neg = -g1;
        neg.normalize();
        let mut neg_bytes = vec![];
        write_g1(&mut neg_bytes, neg).unwrap();
        let mut input = bytes.clone();
        {
            let g2n = {
                let mut g = g2;
                g.normalize();
                g
            };
            let mut gb = vec![];
            write_fq(&mut gb, g2n.x().imaginary()).unwrap();
            write_fq(&mut gb, g2n.x().real()).unwrap();
            write_fq(&mut gb, g2n.y().imaginary()).unwrap();
            write_fq(&mut gb, g2n.y().real()).unwrap();
            input.extend_from_slice(&gb);
        }
        let mut input2 = input.clone();
        input2.extend_from_slice(&neg_bytes);
        {
            let g2n = {
                let mut g = g2;
                g.normalize();
                g
            };
            let mut gb = vec![];
            write_fq(&mut gb, g2n.x().imaginary()).unwrap();
            write_fq(&mut gb, g2n.x().real()).unwrap();
            write_fq(&mut gb, g2n.y().imaginary()).unwrap();
            write_fq(&mut gb, g2n.y().real()).unwrap();
            input2.extend_from_slice(&gb);
        }
        let out = run(BN_PAIRING, &input2).unwrap();
        assert_eq!(out[31], 1);
        assert_eq!(run(BN_PAIRING, &input).unwrap().len(), 32);
    }

    #[test]
    fn blake2f_eip152_vectors() {
        fn input_for(rounds: u32, f: u8) -> Vec<u8> {
            let mut v = Vec::new();
            v.extend_from_slice(&rounds.to_be_bytes());
            v.extend_from_slice(
                &from_hex(
                    "48c9bdf267e6096a3ba7ca8485ae67bb2bf894fe72f36e3cf1361d5f3af54fa5d182e6ad7f520e511f6c3e2b8c68059b6bbd41fbabd9831f79217e1319cde05b",
                )
                .unwrap(),
            );
            let mut m = from_hex("616263").unwrap();
            m.resize(128, 0);
            v.extend_from_slice(&m);
            v.extend_from_slice(&3u64.to_le_bytes());
            v.extend_from_slice(&0u64.to_le_bytes());
            v.push(f);
            assert_eq!(v.len(), 213);
            v
        }

        // test vector 4: 0 rounds
        let out = run(BLAKE2F, &input_for(0, 1)).unwrap();
        assert_eq!(
            to_hex(&out),
            "08c9bcf367e6096a3ba7ca8485ae67bb2bf894fe72f36e3cf1361d5f3af54fa5d282e6ad7f520e511f6c3e2b8c68059b9442be0454267ce079217e1319cde05b"
        );

        // test vector 5: 12 rounds
        let out = run(BLAKE2F, &input_for(12, 1)).unwrap();
        assert_eq!(
            to_hex(&out),
            "ba80a53f981c4d0d6a2797b69f12f6e94c212f14685ac4b74b12bb6fdbffa2d17d87c5392aab792dc252d5de4533cc9518d38aa8dbf1925ab92386edd4009923"
        );

        // test vector 7: 1 round
        let out = run(BLAKE2F, &input_for(1, 1)).unwrap();
        assert_eq!(
            to_hex(&out),
            "b63a380cb2897d521994a85234ee2c181b5f844d2c624c002677e9703449d2fba551b3a8333bcdf5f2f7e08993d53923de3d64fcc68c034e717b9293fed7a421"
        );

        // bad final flag -> error
        let mut bad = input_for(12, 1);
        *bad.last_mut().unwrap() = 2;
        assert!(run(BLAKE2F, &bad).is_err());
    }
}
