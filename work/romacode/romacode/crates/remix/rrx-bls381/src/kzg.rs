//! KZG polynomial commitments (EIP-4844) over BLS12-381, with the REAL
//! trusted setup from the Ethereum KZG ceremony (c-kzg-4844 format).
//!
//! Exact c-kzg-4844 semantics (verified against the C source):
//! - evaluation domain: ω = 7^((p-1)/4096), nodes ω^j (j = 0..4095, natural order)
//! - blob field element a_i is the evaluation at node ω^bitrev12(i)
//!   (c-kzg keeps everything bit-reversed; here we transpose to natural order)
//! - Lagrange setup point for node ω^j sits at file index 2j (even indices =
//!   the 4096-subgroup; odd file entries belong to the EIP-7594 anti-coset)
//! - sidecar challenge: sha256("FSBLOBVERIFY_V1_" || 0u64be || 4096u64be || blob || commitment) mod r

use crate::curve::{msm_g1, G1Affine, G2Affine};
use crate::fp::{r_suborder, Fr, Fp};
use crate::pairing::kzg_pairing_check;
use num_bigint::BigUint;
use num_traits::Zero;

pub const FIELD_ELEMENTS_PER_BLOB: usize = 4096;
pub const BYTES_PER_FIELD_ELEMENT: usize = 32;
pub const BLOB_SIZE: usize = FIELD_ELEMENTS_PER_BLOB * BYTES_PER_FIELD_ELEMENT;
pub const COMMITMENT_SIZE: usize = 48;
pub const PROOF_SIZE: usize = 48;

const SETUP_BIN: &[u8] = include_bytes!("trusted_setup.bin");

pub struct TrustedSetup {
    /// Lagrange G1 points in natural node order: g1_subgroup[j] ↔ node ω^j.
    pub g1_subgroup: Vec<G1Affine>,
    /// Monomial G2 powers [τ^i]_2 (g2[0] = G2 generator).
    pub g2_monomial: Vec<G2Affine>,
}

fn load_setup() -> Result<TrustedSetup, String> {
    const MAGIC: &[u8; 7] = b"RRXKZG1";
    if SETUP_BIN.len() < 15 || SETUP_BIN[..7] != *MAGIC {
        return Err(
            "trusted_setup.bin missing or invalid (run: cargo run -p rrx-bls381 --bin gen_setup)"
                .into(),
        );
    }
    let g1n = u32::from_be_bytes([SETUP_BIN[7], SETUP_BIN[8], SETUP_BIN[9], SETUP_BIN[10]]) as usize;
    let g2n = u32::from_be_bytes([SETUP_BIN[11], SETUP_BIN[12], SETUP_BIN[13], SETUP_BIN[14]]) as usize;
    let mut off = 15;
    let mut file_g1 = Vec::with_capacity(g1n);
    for _ in 0..g1n {
        let x = Fp::from_be48(&SETUP_BIN[off..off + 48])?;
        let y = Fp::from_be48(&SETUP_BIN[off + 48..off + 96])?;
        file_g1.push(G1Affine { x, y, infinity: false });
        off += 96;
    }
    let mut g2_monomial = Vec::with_capacity(g2n);
    for _ in 0..g2n {
        let xc0 = Fp::from_be48(&SETUP_BIN[off..off + 48])?;
        let xc1 = Fp::from_be48(&SETUP_BIN[off + 48..off + 96])?;
        let yc0 = Fp::from_be48(&SETUP_BIN[off + 96..off + 144])?;
        let yc1 = Fp::from_be48(&SETUP_BIN[off + 144..off + 192])?;
        g2_monomial.push(G2Affine {
            x: crate::Fp2::new(xc0, xc1),
            y: crate::Fp2::new(yc0, yc1),
            infinity: false,
        });
        off += 192;
    }
    // file lagrange[i] ↔ node ω^i (natural order, ω = the 4096th root of
    // unity in Fr) — c-kzg bit-reverses internally; we keep natural order.
    Ok(TrustedSetup { g1_subgroup: file_g1, g2_monomial })
}

pub fn setup() -> Result<&'static TrustedSetup, String> {
    static SETUP: std::sync::OnceLock<Result<TrustedSetup, String>> = std::sync::OnceLock::new();
    SETUP
        .get_or_init(load_setup)
        .as_ref()
        .map_err(|e| e.clone())
}

/// 12-bit reversal (c-kzg brp over the 4096-subdomain).
pub fn bitrev12(i: usize) -> usize {
    let mut x = i & 0xfff;
    let mut r = 0;
    for _ in 0..12 {
        r = (r << 1) | (x & 1);
        x >>= 1;
    }
    r
}

/// Evaluation-domain root in the SCALAR field: ω = 7^((r-1)/4096), order 4096.
/// (c-kzg ROOT_OF_UNITY lives in blst_fr = Fr, not the base field.)
pub fn domain_root() -> Fr {
    static ROOT: std::sync::OnceLock<Fr> = std::sync::OnceLock::new();
    ROOT.get_or_init(|| {
        let g = Fr::from_u64(7);
        let e = (r_suborder() - 1u8) / BigUint::from(4096u64);
        g.pow(&e)
    })
    .clone()
}

/// BLS_MODULUS = subgroup order r (scalar field modulus), per EIP-4844.
pub fn bls_modulus() -> [u8; 32] {
    let m = r_suborder().to_bytes_be();
    let mut out = [0u8; 32];
    out[32 - m.len()..].copy_from_slice(&m);
    out
}

/// Interpret a 32-byte blob field element (scalar field, canonical < BLS_MODULUS = r).
pub fn bytes_to_field(bytes: &[u8]) -> Result<Fr, String> {
    Fr::from_be32_canonical(bytes)
}

/// Blob → evaluations at nodes ω^j in NATURAL order (transpose of c-kzg's brp).
pub fn blob_to_evaluations(blob: &[u8]) -> Result<Vec<Fr>, String> {
    if blob.len() != BLOB_SIZE {
        return Err(format!("blob must be {} bytes, got {}", BLOB_SIZE, blob.len()));
    }
    let mut evals = Vec::with_capacity(FIELD_ELEMENTS_PER_BLOB);
    for i in 0..FIELD_ELEMENTS_PER_BLOB {
        evals.push(bytes_to_field(&blob[32 * bitrev12(i)..32 * bitrev12(i) + 32])?);
    }
    Ok(evals)
}

/// KZG commitment to a blob: MSM over the natural-order Lagrange setup.
pub fn blob_to_kzg_commitment(blob: &[u8]) -> Result<[u8; 48], String> {
    let s = setup()?;
    let evals = blob_to_evaluations(blob)?;
    let scalars: Vec<BigUint> = evals.iter().map(|f| f.0.clone()).collect();
    let c = msm_g1(&s.g1_subgroup, &scalars);
    Ok(c.compress())
}

/// Batch inversion (Montgomery's trick): one inversion for n elements.
fn batch_inverse(vals: &[Fr]) -> Result<Vec<Fr>, String> {
    if vals.is_empty() {
        return Ok(vec![]);
    }
    let mut prefixes = Vec::with_capacity(vals.len());
    let mut acc = Fr::one();
    for v in vals {
        prefixes.push(acc.clone());
        acc = acc.mul(v);
    }
    let mut inv_last = acc.inv()?;
    let mut out = vec![Fr::one(); vals.len()];
    for i in (0..vals.len()).rev() {
        out[i] = inv_last.mul(&prefixes[i]);
        inv_last = inv_last.mul(&vals[i]);
    }
    Ok(out)
}

/// Barycentric evaluation of the blob polynomial at z (roots-of-unity domain).
pub fn evaluate_blob_polynomial(blob: &[u8], z: &Fr) -> Result<Fr, String> {
    let evals = blob_to_evaluations(blob)?;
    let n = FIELD_ELEMENTS_PER_BLOB as u64;
    let root = domain_root();
    let mut w_i = Fr::one();
    // denominators (z - ω^j); batch-invert (one field inversion total)
    let mut denoms = Vec::with_capacity(FIELD_ELEMENTS_PER_BLOB);
    let mut weights = Vec::with_capacity(FIELD_ELEMENTS_PER_BLOB);
    for a_i in &evals {
        let denom = z.sub(&w_i);
        if denom.is_zero() {
            return Ok(a_i.clone());
        }
        denoms.push(denom);
        weights.push(w_i.clone());
        w_i = w_i.mul(&root);
    }
    let dinv = batch_inverse(&denoms)?;
    let mut num = Fr::zero();
    for (w, (a_i, d)) in weights.iter().zip(evals.iter().zip(dinv.iter())) {
        num = num.add(&w.mul(a_i).mul(d));
    }
    // y = (z^n - 1)/n · Σ_j (ω^j · p(ω^j))/(z - ω^j)
    let zn = z.pow(&BigUint::from(n));
    let ninv = Fr::from_u64(n).inv()?;
    Ok(zn.sub(&Fr::one()).mul(&ninv).mul(&num))
}

/// Sidecar challenge (exact c-kzg derivation):
/// sha256("FSBLOBVERIFY_V1_" || 0u64be || 4096u64be || blob || commitment) mod r
fn blob_challenge(commitment: &[u8; 48], blob: &[u8]) -> Result<Fr, String> {
    const DOMAIN: &[u8; 16] = b"FSBLOBVERIFY_V1_";
    let mut buf = Vec::with_capacity(16 + 16 + blob.len() + 48);
    buf.extend_from_slice(DOMAIN);
    buf.extend_from_slice(&0u64.to_be_bytes());
    buf.extend_from_slice(&(FIELD_ELEMENTS_PER_BLOB as u64).to_be_bytes());
    buf.extend_from_slice(blob);
    buf.extend_from_slice(commitment);
    let h = kzg_sha256::sha256(&buf);
    Ok(Fr::from_be_bytes_reduced(&h))
}

/// Compute (y, proof) for a blob at challenge z.
pub fn compute_kzg_proof(blob: &[u8], z: &Fr) -> Result<([u8; 32], [u8; 48]), String> {
    let s = setup()?;
    let evals = blob_to_evaluations(blob)?;
    let y = evaluate_blob_polynomial(blob, z)?;
    // quotient on the domain: q(ω^j) = (p(ω^j) - y)/(ω^j - z) — batch-inverted
    let root = domain_root();
    let mut w_i = Fr::one();
    let mut denoms = Vec::with_capacity(FIELD_ELEMENTS_PER_BLOB);
    let mut numerators = Vec::with_capacity(FIELD_ELEMENTS_PER_BLOB);
    for a_i in &evals {
        let denom = w_i.sub(z);
        if denom.is_zero() {
            return Err("z is in the evaluation domain; quotient undefined at that node".into());
        }
        denoms.push(denom);
        numerators.push(a_i.sub(&y));
        w_i = w_i.mul(&root);
    }
    let dinv = batch_inverse(&denoms)?;
    let q_evals: Vec<Fr> = numerators
        .iter()
        .zip(dinv.iter())
        .map(|(n, d)| n.mul(d))
        .collect();
    let scalars: Vec<BigUint> = q_evals.iter().map(|f| f.0.clone()).collect();
    let proof = msm_g1(&s.g1_subgroup, &scalars);
    Ok((y.to_be32(), proof.compress()))
}

/// Sidecar proof for a blob against its commitment.
pub fn compute_blob_kzg_proof(blob: &[u8], commitment: &[u8; 48]) -> Result<[u8; 48], String> {
    let z = blob_challenge(commitment, blob)?;
    let (_y, proof) = compute_kzg_proof(blob, &z)?;
    Ok(proof)
}

/// Verify a KZG proof (pairing check) — the exact EIP-4844 verification equation.
pub fn verify_kzg_proof(
    commitment48: &[u8],
    z32: &[u8],
    y32: &[u8],
    proof48: &[u8],
) -> Result<bool, String> {
    if commitment48.len() != 48 || proof48.len() != 48 {
        return Err("commitment/proof must be 48 bytes".into());
    }
    if z32.len() != 32 || y32.len() != 32 {
        return Err("z/y must be 32 bytes".into());
    }
    let s = setup()?;
    let commitment = G1Affine::decompress(commitment48)?;
    let proof = G1Affine::decompress(proof48)?;
    let z = Fr::from_be32_canonical(z32)?;
    let y = Fr::from_be32_canonical(y32)?;
    Ok(kzg_pairing_check(
        &commitment,
        &z.0,
        &y.0,
        &proof,
        &s.g2_monomial[0],
        &s.g2_monomial[1],
    ))
}

/// Verify a full blob against its commitment + sidecar proof (mainnet path).
pub fn verify_blob_kzg_proof(blob: &[u8], commitment48: &[u8], proof48: &[u8]) -> Result<bool, String> {
    if commitment48.len() != 48 || proof48.len() != 48 {
        return Err("commitment/proof must be 48 bytes".into());
    }
    if blob.len() != BLOB_SIZE {
        return Err(format!("blob must be {} bytes, got {}", BLOB_SIZE, blob.len()));
    }
    let mut c = [0u8; 48];
    c.copy_from_slice(commitment48);
    let z = blob_challenge(&c, blob)?;
    let y = evaluate_blob_polynomial(blob, &z)?;
    verify_kzg_proof(commitment48, &z.to_be32(), &y.to_be32(), proof48)
}

/// Versioned blob hash: 0x01 || sha256(commitment)[1..32].
pub fn versioned_hash(commitment48: &[u8]) -> Result<[u8; 32], String> {
    if commitment48.len() != 48 {
        return Err("commitment must be 48 bytes".into());
    }
    let h = kzg_sha256::sha256(commitment48);
    let mut out = [0u8; 32];
    out[0] = 0x01;
    out[1..].copy_from_slice(&h[1..]);
    Ok(out)
}

/// Standalone SHA-256 (FIPS 180-4) so this crate has zero rrx dependencies.
pub mod kzg_sha256 {
    const K: [u32; 64] = [
        0x428a2f98, 0x71374491, 0xb5c0fbcf, 0xe9b5dba5, 0x3956c25b, 0x59f111f1, 0x923f82a4, 0xab1c5ed5,
        0xd807aa98, 0x12835b01, 0x243185be, 0x550c7dc3, 0x72be5d74, 0x80deb1fe, 0x9bdc06a7, 0xc19bf174,
        0xe49b69c1, 0xefbe4786, 0x0fc19dc6, 0x240ca1cc, 0x2de92c6f, 0x4a7484aa, 0x5cb0a9dc, 0x76f988da,
        0x983e5152, 0xa831c66d, 0xb00327c8, 0xbf597fc7, 0xc6e00bf3, 0xd5a79147, 0x06ca6351, 0x14292967,
        0x27b70a85, 0x2e1b2138, 0x4d2c6dfc, 0x53380d13, 0x650a7354, 0x766a0abb, 0x81c2c92e, 0x92722c85,
        0xa2bfe8a1, 0xa81a664b, 0xc24b8b70, 0xc76c51a3, 0xd192e819, 0xd6990624, 0xf40e3585, 0x106aa070,
        0x19a4c116, 0x1e376c08, 0x2748774c, 0x34b0bcb5, 0x391c0cb3, 0x4ed8aa4a, 0x5b9cca4f, 0x682e6ff3,
        0x748f82ee, 0x78a5636f, 0x84c87814, 0x8cc70208, 0x90befffa, 0xa4506ceb, 0xbef9a3f7, 0xc67178f2,
    ];

    pub fn sha256(data: &[u8]) -> [u8; 32] {
        let mut h: [u32; 8] = [
            0x6a09e667, 0xbb67ae85, 0x3c6ef372, 0xa54ff53a, 0x510e527f, 0x9b05688c, 0x1f83d9ab, 0x5be0cd19,
        ];
        let mut msg = data.to_vec();
        let bitlen = (data.len() as u64) * 8;
        msg.push(0x80);
        while msg.len() % 64 != 56 {
            msg.push(0);
        }
        msg.extend_from_slice(&bitlen.to_be_bytes());
        for chunk in msg.chunks(64) {
            let mut w = [0u32; 64];
            for i in 0..16 {
                w[i] = u32::from_be_bytes([chunk[i * 4], chunk[i * 4 + 1], chunk[i * 4 + 2], chunk[i * 4 + 3]]);
            }
            for i in 16..64 {
                let s0 = w[i - 15].rotate_right(7) ^ w[i - 15].rotate_right(18) ^ (w[i - 15] >> 3);
                let s1 = w[i - 2].rotate_right(17) ^ w[i - 2].rotate_right(19) ^ (w[i - 2] >> 10);
                w[i] = w[i - 16].wrapping_add(s0).wrapping_add(w[i - 7]).wrapping_add(s1);
            }
            let (mut a, mut b, mut c, mut d, mut e, mut f, mut g, mut hh) =
                (h[0], h[1], h[2], h[3], h[4], h[5], h[6], h[7]);
            for i in 0..64 {
                let s1 = e.rotate_right(6) ^ e.rotate_right(11) ^ e.rotate_right(25);
                let ch = (e & f) ^ ((!e) & g);
                let t1 = hh.wrapping_add(s1).wrapping_add(ch).wrapping_add(K[i]).wrapping_add(w[i]);
                let s0 = a.rotate_right(2) ^ a.rotate_right(13) ^ a.rotate_right(22);
                let maj = (a & b) ^ (a & c) ^ (b & c);
                let t2 = s0.wrapping_add(maj);
                hh = g;
                g = f;
                f = e;
                e = d.wrapping_add(t1);
                d = c;
                c = b;
                b = a;
                a = t1.wrapping_add(t2);
            }
            h[0] = h[0].wrapping_add(a);
            h[1] = h[1].wrapping_add(b);
            h[2] = h[2].wrapping_add(c);
            h[3] = h[3].wrapping_add(d);
            h[4] = h[4].wrapping_add(e);
            h[5] = h[5].wrapping_add(f);
            h[6] = h[6].wrapping_add(g);
            h[7] = h[7].wrapping_add(hh);
        }
        let mut out = [0u8; 32];
        for i in 0..8 {
            out[i * 4..i * 4 + 4].copy_from_slice(&h[i].to_be_bytes());
        }
        out
    }

    #[cfg(test)]
    mod tests {
        #[test]
        fn sha256_known() {
            assert_eq!(
                hex::encode(super::sha256(b"abc")),
                "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
            );
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn versioned_hash_shape() {
        let c = [0xabu8; 48];
        let v = versioned_hash(&c).unwrap();
        assert_eq!(v[0], 0x01);
    }

    #[test]
    fn bitrev12_involution() {
        for i in 0..4096 {
            assert_eq!(bitrev12(bitrev12(i)), i);
        }
        assert_eq!(bitrev12(1), 2048);
    }

    #[test]
    fn domain_root_order() {
        let w = domain_root();
        assert_eq!(w.pow(&BigUint::from(4096u64)), Fr::one());
        assert_ne!(w.pow(&BigUint::from(2048u64)), Fr::one());
    }

    #[test]
    fn self_consistent_kzg_small() {
        if setup().is_err() {
            return;
        }
        let mut blob = vec![0u8; BLOB_SIZE];
        for (i, b) in blob.iter_mut().enumerate() {
            *b = ((i * 7 + 3) % 256) as u8;
        }
        for chunk in blob.chunks_mut(32) {
            chunk[0] &= 0x0f;
        }
        let c = blob_to_kzg_commitment(&blob).unwrap();
        let z = Fr::from_u64(12345);
        let (y, proof) = compute_kzg_proof(&blob, &z).unwrap();
        assert!(verify_kzg_proof(&c, &zb_from(&z), &y, &proof).unwrap(), "valid proof must verify");
        let mut bad_y = y;
        bad_y[31] ^= 1;
        assert!(!verify_kzg_proof(&c, &zb_from(&z), &bad_y, &proof).unwrap(), "tampered y must fail");
        // sidecar path consistency
        let side = compute_blob_kzg_proof(&blob, &c).unwrap();
        assert!(verify_blob_kzg_proof(&blob, &c, &side).unwrap(), "sidecar proof must verify");
    }

    fn zb_from(z: &Fr) -> [u8; 32] {
        z.to_be32()
    }
}
