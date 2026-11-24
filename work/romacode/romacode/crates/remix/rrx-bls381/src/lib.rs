//! rrx-bls381: from-scratch BLS12-381 for rrx — Fp/Fp2/Fp6/Fp12 tower,
//! G1/G2, optimal ate pairing, Pippenger MSM, EIP-2537 encodings, KZG (EIP-4844).
//!
//! Validated against: official EIP-2537 test vectors + REAL mainnet blob
//! commitments/proofs (KZG) fetched live from the beacon chain ecosystem.

pub mod curve;
pub mod fp;
pub mod fp2;
pub mod fp612;
pub mod pairing;
pub mod kzg;

pub use curve::{
    g1_generator, g2_generator, G1Affine, G2Affine,
};
pub use fp::Fp;
pub use fp2::Fp2;
pub use fp612::{Fp12, Fp6};
pub use pairing::{pairing, pairing_product_is_one};

#[cfg(test)]
mod tests {
    use super::*;
    use crate::curve::msm_g2;
    use num_bigint::BigUint;

    /// e(a·G, H) == e(G, a·H) — bilinearity (small scalar, keeps test fast).
    #[test]
    fn pairing_bilinear_small() {
        let g = g1_generator();
        let h = g2_generator();
        let a = BigUint::from(3u32);
        let pa = g.mul(&a);
        let ha = h.mul(&a);
        let e1 = pairing(&pa, &h);
        let e2 = pairing(&g, &ha);
        assert_eq!(e1, e2);
        assert_ne!(e1, Fp12::one());
        // e(2P, Q) == e(P, 2Q)
        let e3 = pairing(&g.double(), &h);
        let e4 = pairing(&g, &h.double());
        assert_eq!(e3, e4);
    }

    /// e(P, Q) · e(-P, Q) == 1
    #[test]
    fn pairing_negation_cancels() {
        let g = g1_generator();
        let h = g2_generator();
        let e1 = pairing(&g, &h);
        let e2 = pairing(&g.neg(), &h);
        assert_eq!(e1.mul(&e2), Fp12::one());
        // e(P, -Q) == e(-P, Q)  (EIP-2537 required property)
        assert_eq!(pairing(&g, &h.neg()), pairing(&g.neg(), &h));
    }

    /// e(P, Q)^r == 1 (pairing lands in the r-th roots of unity)
    #[test]
    fn pairing_in_target_subgroup() {
        let g = g1_generator();
        let h = g2_generator();
        let e = pairing(&g, &h);
        assert_eq!(e.pow(&crate::fp::r_suborder()), Fp12::one());
    }

    /// MSM(2 points) == naive (G2 path)
    #[test]
    fn msm_g2_matches_naive() {
        let h = g2_generator();
        let pts = vec![h.clone(), h.double()];
        let scs = vec![BigUint::from(9u32), BigUint::from(4u32)];
        let fast = msm_g2(&pts, &scs);
        let naive = h.mul(&BigUint::from(9u32)).add(&h.double().mul(&BigUint::from(4u32)));
        assert_eq!(fast, naive);
    }
}
