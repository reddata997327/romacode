//! Fp6 = Fp2[v]/(v^3 - (1+u)) and Fp12 = Fp6[w]/(w^2 - v) — the BLS12-381 tower.

use super::fp2::Fp2;

/// ξ = 1 + u  (the Fp6 defining element: v^3 = ξ)
pub fn xi() -> Fp2 {
    Fp2::new(Fp2::one().c0, Fp2::one().c1.add(&Fp2::one().c0))
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Fp6 {
    pub c0: Fp2,
    pub c1: Fp2,
    pub c2: Fp2,
}

impl Fp6 {
    pub fn zero() -> Self { Fp6 { c0: Fp2::zero(), c1: Fp2::zero(), c2: Fp2::zero() } }
    pub fn one() -> Self { Fp6 { c0: Fp2::one(), c1: Fp2::zero(), c2: Fp2::zero() } }
    pub fn is_zero(&self) -> bool { self.c0.is_zero() && self.c1.is_zero() && self.c2.is_zero() }

    pub fn new(c0: Fp2, c1: Fp2, c2: Fp2) -> Self { Fp6 { c0, c1, c2 } }

    pub fn add(&self, o: &Fp6) -> Fp6 {
        Fp6::new(self.c0.add(&o.c0), self.c1.add(&o.c1), self.c2.add(&o.c2))
    }
    pub fn sub(&self, o: &Fp6) -> Fp6 {
        Fp6::new(self.c0.sub(&o.c0), self.c1.sub(&o.c1), self.c2.sub(&o.c2))
    }
    pub fn neg(&self) -> Fp6 { Fp6::new(self.c0.neg(), self.c1.neg(), self.c2.neg()) }

    pub fn mul(&self, o: &Fp6) -> Fp6 {
        // Karatsuba-style with reduction v^3 = ξ
        let t0 = self.c0.mul(&o.c0);
        let t1 = self.c1.mul(&o.c1);
        let t2 = self.c2.mul(&o.c2);
        let s01 = self.c0.add(&self.c1).mul(&o.c0.add(&o.c1)).sub(&t0).sub(&t1); // a0b1+a1b0
        let s12 = self.c1.add(&self.c2).mul(&o.c1.add(&o.c2)).sub(&t1).sub(&t2); // a1b2+a2b1
        let s02 = self.c0.add(&self.c2).mul(&o.c0.add(&o.c2)).sub(&t0).sub(&t2); // a0b2+a2b0
        let k = xi();
        Fp6::new(
            t0.add(&s12.mul(&k)),
            s01.add(&t2.mul(&k)),
            s02.add(&t1),
        )
    }

    pub fn square(&self) -> Fp6 { self.mul(self) }

    pub fn mul_fp2(&self, s: &Fp2) -> Fp6 {
        Fp6::new(self.c0.mul(s), self.c1.mul(s), self.c2.mul(s))
    }

    pub fn inv(&self) -> Result<Fp6, String> {
        // Standard Fp6 inversion (cf. zkcrypto/pairing):
        let k = xi();
        let a0 = self.c0.square().sub(&k.mul(&self.c1.mul(&self.c2)));
        let a1 = k.mul(&self.c2.square()).sub(&self.c0.mul(&self.c1));
        let a2 = self.c1.square().sub(&self.c0.mul(&self.c2));
        let denom = self.c0.mul(&a0)
            .add(&k.mul(&self.c2.mul(&a1).add(&self.c1.mul(&a2))));
        let d = denom.inv()?;
        Ok(Fp6::new(a0.mul(&d), a1.mul(&d), a2.mul(&d)))
    }

    pub fn pow(&self, exp: &num_bigint::BigUint) -> Fp6 {
        let mut r = Fp6::one();
        let bits: Vec<bool> = exp.to_bytes_be().iter().flat_map(|b| (0..8).rev().map(move |i| (b >> i) & 1 == 1)).collect();
        for bit in bits {
            r = r.square();
            if bit {
                r = r.mul(self);
            }
        }
        r
    }

    /// Embed an Fp2 element (c0 + c1·0·v + 0·v^2).
    pub fn from_fp2(a: &Fp2) -> Fp6 { Fp6::new(a.clone(), Fp2::zero(), Fp2::zero()) }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Fp12 {
    pub c0: Fp6,
    pub c1: Fp6,
}

impl Fp12 {
    pub fn zero() -> Self { Fp12 { c0: Fp6::zero(), c1: Fp6::zero() } }
    pub fn one() -> Self { Fp12 { c0: Fp6::one(), c1: Fp6::zero() } }
    pub fn is_zero(&self) -> bool { self.c0.is_zero() && self.c1.is_zero() }

    pub fn new(c0: Fp6, c1: Fp6) -> Self { Fp12 { c0, c1 } }

    pub fn add(&self, o: &Fp12) -> Fp12 { Fp12::new(self.c0.add(&o.c0), self.c1.add(&o.c1)) }
    pub fn sub(&self, o: &Fp12) -> Fp12 { Fp12::new(self.c0.sub(&o.c0), self.c1.sub(&o.c1)) }
    pub fn neg(&self) -> Fp12 { Fp12::new(self.c0.neg(), self.c1.neg()) }

    pub fn mul(&self, o: &Fp12) -> Fp12 {
        // (a0 + a1 w)(b0 + b1 w) = (a0b0 + v·a1b1) + (a0b1 + a1b0) w   [w^2 = v]
        let t0 = self.c0.mul(&o.c0);
        let t1 = self.c1.mul(&o.c1);
        let v_t1 = Fp6::new(t1.c2.mul(&xi()), t1.c0.clone(), t1.c1.clone()); // v·t1 = ξ·c2 + c0·v + c1·v²
        Fp12::new(t0.add(&v_t1), self.c0.mul(&o.c1).add(&self.c1.mul(&o.c0)))
    }

    pub fn square(&self) -> Fp12 { self.mul(self) }

    pub fn double(&self) -> Fp12 { self.add(self) }

    pub fn mul_fp6(&self, s: &Fp6) -> Fp12 { Fp12::new(self.c0.mul(s), self.c1.mul(s)) }

    /// p^6-th power = conjugation over Fp6 (c0 - c1·w).
    pub fn conjugate(&self) -> Fp12 { Fp12::new(self.c0.clone(), self.c1.neg()) }

    pub fn inv(&self) -> Result<Fp12, String> {
        // 1/(a0 + a1 w) = (a0 - a1 w) / (a0^2 - v a1^2)
        let d = self.c0.square().sub(&mul_v(&self.c1.square()));
        let dinv = d.inv()?;
        Ok(Fp12::new(self.c0.mul(&dinv), self.c1.mul(&dinv).neg()))
    }

    pub fn pow(&self, exp: &num_bigint::BigUint) -> Fp12 {
        let mut r = Fp12::one();
        let bits: Vec<bool> = exp.to_bytes_be().iter().flat_map(|b| (0..8).rev().map(move |i| (b >> i) & 1 == 1)).collect();
        for bit in bits {
            r = r.square();
            if bit {
                r = r.mul(self);
            }
        }
        r
    }
}

/// multiply an Fp6 element by v (the Fp6 generator): v·(c0 + c1 v + c2 v^2) = c2·ξ + c0·v + c1·v^2
pub fn mul_v(a: &Fp6) -> Fp6 {
    Fp6::new(a.c2.mul(&xi()), a.c0.clone(), a.c1.clone())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::fp::Fp;

    #[test]
    fn fp6_fp12_ring_ops() {
        let a = Fp6::new(
            Fp2::new(Fp::from_u64(1), Fp::from_u64(2)),
            Fp2::new(Fp::from_u64(3), Fp::from_u64(4)),
            Fp2::new(Fp::from_u64(5), Fp::from_u64(6)),
        );
        let b = Fp6::new(
            Fp2::new(Fp::from_u64(7), Fp::from_u64(8)),
            Fp2::new(Fp::from_u64(9), Fp::from_u64(10)),
            Fp2::new(Fp::from_u64(11), Fp::from_u64(12)),
        );
        assert_eq!(a.mul(&b).inv().unwrap().mul(&a.mul(&b)), Fp6::one());
        assert_eq!(a.mul(&b), b.mul(&a));

        let x = Fp12::new(a.clone(), b.clone());
        let y = Fp12::new(b, a);
        let p = x.mul(&y);
        assert_eq!(p.inv().unwrap().mul(&p), Fp12::one());
        assert_eq!(x.mul(&y), y.mul(&x));
        assert_eq!(x.conjugate().conjugate(), x);
    }

    #[test]
    fn mul_v_consistent() {
        let a = Fp6::new(
            Fp2::new(Fp::from_u64(1), Fp::from_u64(0)),
            Fp2::new(Fp::from_u64(2), Fp::from_u64(0)),
            Fp2::new(Fp::from_u64(3), Fp::from_u64(0)),
        );
        // v * a must equal Fp6 mul with v embedded: v = (0, 1, 0)
        let v = Fp6::new(Fp2::zero(), Fp2::one(), Fp2::zero());
        assert_eq!(mul_v(&a), a.mul(&v));
    }
}
