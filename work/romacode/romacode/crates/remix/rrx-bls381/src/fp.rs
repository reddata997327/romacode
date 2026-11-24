//! Fp: the 381-bit BLS12-381 base prime field (arbitrary-precision backend).

use num_bigint::BigUint;
use num_traits::{One, Zero};
use std::sync::OnceLock;

pub const MODULUS_HEX: &str = "1a0111ea397fe69a4b1ba7b6434bacd764774b84f38512bf6730d2a0f6b0f6241eabfffeb153ffffb9feffffffffaaab";

pub fn modulus() -> BigUint {
    static P: OnceLock<BigUint> = OnceLock::new();
    P.get_or_init(|| BigUint::parse_bytes(MODULUS_HEX.as_bytes(), 16).unwrap())
        .clone()
}

pub fn r_suborder() -> BigUint {
    static R: OnceLock<BigUint> = OnceLock::new();
    R.get_or_init(|| BigUint::parse_bytes(b"73eda753299d7d483339d80809a1d80553bda402fffe5bfeffffffff00000001", 16).unwrap())
        .clone()
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Fp(pub BigUint);

impl Fp {
    pub fn zero() -> Self { Fp(BigUint::zero()) }
    pub fn one() -> Self { Fp(BigUint::one()) }
    pub fn is_zero(&self) -> bool { self.0.is_zero() }

    pub fn from_u64(v: u64) -> Self {
        Fp(BigUint::from(v) % modulus())
    }

    /// Interpret big-endian bytes (any length) as an integer mod p.
    pub fn from_be_bytes(bytes: &[u8]) -> Self {
        Fp(BigUint::from_bytes_be(bytes) % modulus())
    }

    /// Canonical 48-byte big-endian encoding.
    pub fn to_be48(&self) -> [u8; 48] {
        let b = self.0.to_bytes_be();
        let mut out = [0u8; 48];
        let start = 48usize.saturating_sub(b.len());
        out[start..].copy_from_slice(&b);
        out
    }

    pub fn from_be48(bytes: &[u8]) -> Result<Self, String> {
        if bytes.len() != 48 {
            return Err("Fp: need 48 bytes".into());
        }
        let v = BigUint::from_bytes_be(bytes);
        if v >= modulus() {
            return Err("Fp: value >= modulus".into());
        }
        Ok(Fp(v))
    }

    pub fn add(&self, o: &Fp) -> Fp { Fp((&self.0 + &o.0) % modulus()) }
    pub fn sub(&self, o: &Fp) -> Fp {
        let a = &self.0;
        let b = &o.0;
        Fp(if a >= b { a - b } else { modulus() - (b - a) })
    }
    pub fn mul(&self, o: &Fp) -> Fp { Fp((&self.0 * &o.0) % modulus()) }
    pub fn neg(&self) -> Fp {
        if self.0.is_zero() { Fp::zero() } else { Fp(modulus() - &self.0) }
    }
    pub fn square(&self) -> Fp { self.mul(self) }
    pub fn double(&self) -> Fp { self.add(self) }

    pub fn pow(&self, exp: &BigUint) -> Fp {
        let mut result = Fp::one();
        let bits: Vec<bool> = exp.to_bytes_be().iter().flat_map(|b| (0..8).rev().map(move |i| (b >> i) & 1 == 1)).collect();
        for bit in bits {
            result = result.square();
            if bit {
                result = result.mul(self);
            }
        }
        result
    }

    /// Multiplicative inverse via Fermat (p is prime).
    pub fn inv(&self) -> Result<Fp, String> {
        if self.is_zero() {
            return Err("Fp: inverse of zero".into());
        }
        let e = modulus() - 2u8;
        Ok(self.pow(&e))
    }

    /// Square root (p ≡ 3 mod 4): y = c^((p+1)/4), verify.
    pub fn sqrt(&self) -> Result<Fp, String> {
        if self.is_zero() {
            return Ok(Fp::zero());
        }
        // (p+1)/4
        let e = (modulus() + 1u8) >> 2u32;
        let y = self.pow(&e);
        if y.square() == *self {
            Ok(y)
        } else {
            Err("Fp: not a quadratic residue".into())
        }
    }

    pub fn is_quadratic_residue(&self) -> bool {
        if self.is_zero() { return true; }
        // c^((p-1)/2) == 1
        let e = (modulus() - 1u8) >> 1u32;
        self.pow(&e) == Fp::one()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fp_basic() {
        let a = Fp::from_u64(7);
        let b = Fp::from_u64(5);
        assert_eq!(a.add(&b), Fp::from_u64(12));
        assert_eq!(a.mul(&b), Fp::from_u64(35));
        assert_eq!(a.sub(&b), Fp::from_u64(2));
        assert_eq!(b.sub(&a).add(&a), b);
        assert_eq!(a.inv().unwrap().mul(&a), Fp::one());
    }

    #[test]
    fn fp_sqrt() {
        let a = Fp::from_u64(9);
        let r = a.sqrt().unwrap();
        assert_eq!(r.square(), a);
        assert!(Fp::from_u64(7).sqrt().is_err() || Fp::from_u64(7).sqrt().unwrap().square() == Fp::from_u64(7));
        // non-residue check
        let mut c = Fp::from_u64(2);
        while c.is_quadratic_residue() {
            c = c.add(&Fp::one());
        }
        assert!(c.sqrt().is_err());
    }

    #[test]
    fn fp_modulus_shape() {
        // p is 381 bits: top byte 0x1a, 48 bytes total
        let p = modulus().to_bytes_be();
        assert_eq!(p.len(), 48);
        assert_eq!(p[0], 0x1a);
    }
}

/// Fr: the scalar field (subgroup order r) — where KZG polynomial
/// coefficients, evaluations, domains and challenges live.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Fr(pub BigUint);

impl Fr {
    pub fn zero() -> Self { Fr(BigUint::zero()) }
    pub fn one() -> Self { Fr(BigUint::one()) }
    pub fn is_zero(&self) -> bool { self.0.is_zero() }

    pub fn from_u64(v: u64) -> Self { Fr(BigUint::from(v) % r_suborder()) }

    /// Canonical big-endian 32-byte interpretation; errors if >= r.
    pub fn from_be32_canonical(bytes: &[u8]) -> Result<Self, String> {
        if bytes.len() != 32 {
            return Err("Fr: need 32 bytes".into());
        }
        let v = BigUint::from_bytes_be(bytes);
        if v >= r_suborder() {
            return Err("Fr: value >= subgroup order (BLS_MODULUS)".into());
        }
        Ok(Fr(v))
    }

    /// Reduce arbitrary bytes mod r.
    pub fn from_be_bytes_reduced(bytes: &[u8]) -> Self {
        Fr(BigUint::from_bytes_be(bytes) % r_suborder())
    }

    pub fn to_be32(&self) -> [u8; 32] {
        let b = self.0.to_bytes_be();
        let mut out = [0u8; 32];
        let start = 32usize.saturating_sub(b.len());
        out[start..].copy_from_slice(&b);
        out
    }

    pub fn add(&self, o: &Fr) -> Fr { Fr((&self.0 + &o.0) % r_suborder()) }
    pub fn sub(&self, o: &Fr) -> Fr {
        let a = &self.0;
        let b = &o.0;
        Fr(if a >= b { a - b } else { r_suborder() - (b - a) })
    }
    pub fn mul(&self, o: &Fr) -> Fr { Fr((&self.0 * &o.0) % r_suborder()) }
    pub fn neg(&self) -> Fr {
        if self.0.is_zero() { Fr::zero() } else { Fr(r_suborder() - &self.0) }
    }
    pub fn square(&self) -> Fr { self.mul(self) }
    pub fn double(&self) -> Fr { self.add(self) }

    pub fn pow(&self, exp: &BigUint) -> Fr {
        let mut result = Fr::one();
        let bits: Vec<bool> = exp.to_bytes_be().iter().flat_map(|b| (0..8).rev().map(move |i| (b >> i) & 1 == 1)).collect();
        for bit in bits {
            result = result.square();
            if bit {
                result = result.mul(self);
            }
        }
        result
    }

    pub fn inv(&self) -> Result<Fr, String> {
        if self.is_zero() {
            return Err("Fr: inverse of zero".into());
        }
        let e = r_suborder() - 2u8;
        Ok(self.pow(&e))
    }
}

#[cfg(test)]
mod fr_tests {
    use super::*;

    #[test]
    fn fr_basic() {
        let a = Fr::from_u64(7);
        let b = Fr::from_u64(5);
        assert_eq!(a.add(&b), Fr::from_u64(12));
        assert_eq!(a.mul(&b), Fr::from_u64(35));
        assert_eq!(a.inv().unwrap().mul(&a), Fr::one());
        assert_eq!(r_suborder().bits(), 255);
    }

    #[test]
    fn fr_root_of_unity() {
        // ω = 7^((r-1)/4096) must have exact order 4096 in Fr
        let e = (r_suborder() - 1u8) / BigUint::from(4096u64);
        let w = Fr::from_u64(7).pow(&e);
        assert_eq!(w.pow(&BigUint::from(4096u64)), Fr::one());
        assert_ne!(w.pow(&BigUint::from(2048u64)), Fr::one());
    }
}
