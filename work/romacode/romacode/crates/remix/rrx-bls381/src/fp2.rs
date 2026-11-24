//! Fp2 = Fp[u]/(u^2 + 1) — BLS12-381 quadratic extension.

use super::fp::Fp;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Fp2 {
    pub c0: Fp,
    pub c1: Fp,
}

impl Fp2 {
    pub fn zero() -> Self { Fp2 { c0: Fp::zero(), c1: Fp::zero() } }
    pub fn one() -> Self { Fp2 { c0: Fp::one(), c1: Fp::zero() } }
    pub fn is_zero(&self) -> bool { self.c0.is_zero() && self.c1.is_zero() }

    pub fn new(c0: Fp, c1: Fp) -> Self { Fp2 { c0, c1 } }

    pub fn add(&self, o: &Fp2) -> Fp2 { Fp2::new(self.c0.add(&o.c0), self.c1.add(&o.c1)) }
    pub fn sub(&self, o: &Fp2) -> Fp2 { Fp2::new(self.c0.sub(&o.c0), self.c1.sub(&o.c1)) }
    pub fn neg(&self) -> Fp2 { Fp2::new(self.c0.neg(), self.c1.neg()) }
    pub fn double(&self) -> Fp2 { self.add(self) }

    pub fn mul(&self, o: &Fp2) -> Fp2 {
        // (a0 + a1 u)(b0 + b1 u) = (a0 b0 - a1 b1) + (a0 b1 + a1 b0) u   [u^2 = -1]
        let t0 = self.c0.mul(&o.c0);
        let t1 = self.c1.mul(&o.c1);
        let t2 = self.c0.mul(&o.c1);
        let t3 = self.c1.mul(&o.c0);
        Fp2::new(t0.sub(&t1), t2.add(&t3))
    }

    pub fn square(&self) -> Fp2 {
        // (a0 + a1 u)^2 = (a0+a1)(a0-a1) + 2 a0 a1 u
        let s = self.c0.add(&self.c1);
        let d = self.c0.sub(&self.c1);
        Fp2::new(s.mul(&d), self.c0.mul(&self.c1).double())
    }

    pub fn conjugate(&self) -> Fp2 { Fp2::new(self.c0.clone(), self.c1.neg()) }

    pub fn mul_fp(&self, s: &Fp) -> Fp2 { Fp2::new(self.c0.mul(s), self.c1.mul(s)) }

    pub fn inv(&self) -> Result<Fp2, String> {
        // 1/(a+bu) = conj / norm, norm = a^2 + b^2  (since u^2 = -1)
        let norm = self.c0.square().add(&self.c1.square());
        let ninv = norm.inv()?;
        Ok(self.conjugate().mul_fp(&ninv))
    }

    pub fn pow(&self, exp: &num_bigint::BigUint) -> Fp2 {
        let mut r = Fp2::one();
        let bits: Vec<bool> = exp.to_bytes_be().iter().flat_map(|b| (0..8).rev().map(move |i| (b >> i) & 1 == 1)).collect();
        for bit in bits {
            r = r.square();
            if bit {
                r = r.mul(self);
            }
        }
        r
    }

    /// Square root in Fp2 (p ≡ 3 mod 4). Algorithm:
    /// b=0 → Fp sqrt; else n = sqrt(a^2+b^2) in Fp, x = sqrt((a+n)/2), y = b/(2x).
    pub fn sqrt(&self) -> Result<Fp2, String> {
        if self.is_zero() {
            return Ok(Fp2::zero());
        }
        if self.c1.is_zero() {
            let r = self.c0.sqrt()?;
            return Ok(Fp2::new(r, Fp::zero()));
        }
        let norm = self.c0.square().add(&self.c1.square());
        let n = norm.sqrt()?;
        let half = Fp::from_u64(2).inv()?;
        // x² = (a+n)/2 or (a-n)/2 — try both
        let mut x = self.c0.add(&n).mul(&half).sqrt();
        if x.is_err() {
            x = self.c0.sub(&n).mul(&half).sqrt();
        }
        let x = x.map_err(|_| "Fp2 sqrt: not a quadratic residue".to_string())?;
        if x.is_zero() {
            return Err("Fp2 sqrt: degenerate".into());
        }
        let y = self.c1.mul(&Fp::from_u64(2).mul(&x).inv()?);
        let r = Fp2::new(x, y);
        if r.square() == *self {
            Ok(r)
        } else {
            Err("Fp2 sqrt: not a quadratic residue".into())
        }
    }

    pub fn to_be96(&self) -> [u8; 96] {
        let mut out = [0u8; 96];
        out[..48].copy_from_slice(&self.c0.to_be48());
        out[48..].copy_from_slice(&self.c1.to_be48());
        out
    }

    pub fn from_be96(bytes: &[u8]) -> Result<Self, String> {
        if bytes.len() != 96 {
            return Err("Fp2: need 96 bytes".into());
        }
        Ok(Fp2::new(Fp::from_be48(&bytes[..48])?, Fp::from_be48(&bytes[48..])?))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fp2_ops() {
        let a = Fp2::new(Fp::from_u64(3), Fp::from_u64(4));
        let b = Fp2::new(Fp::from_u64(1), Fp::from_u64(2));
        let prod = a.mul(&b);
        // (3+4u)(1+2u) = 3 + 6u + 4u + 8u^2 = (3-8) + 10u = -5 + 10u
        assert_eq!(prod, Fp2::new(Fp::from_u64(5).neg(), Fp::from_u64(10)));
        assert_eq!(prod.inv().unwrap().mul(&prod), Fp2::one());
        assert_eq!(a.square(), a.mul(&a));
    }

    #[test]
    fn fp2_sqrt() {
        // (1 + 1u)^2 = 1 + 2u + u^2 = 2u  → sqrt(2u) = 1+u
        let c = Fp2::new(Fp::zero(), Fp::from_u64(2));
        let r = c.sqrt().unwrap();
        assert_eq!(r, Fp2::new(Fp::one(), Fp::one()));
        // random QR case
        let t = Fp2::new(Fp::from_u64(7), Fp::from_u64(11));
        let sq = t.square();
        let r = sq.sqrt().unwrap();
        assert!(r == t || r == t.neg());
    }
}
