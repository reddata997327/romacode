//! G1 (over Fp) and G2 (over Fp2) — BLS12-381 curve points, projective ops,
//! multi-scalar multiplication (Pippenger), and EIP-2537 byte encodings.

use super::fp::{r_suborder, Fp};
use super::fp2::Fp2;
use num_bigint::BigUint;
use num_traits::Zero;

pub const B_G1: u64 = 4;
pub const B_G2_C0: u64 = 4;
pub const B_G2_C1: u64 = 4;

/// EIP-2537 G1 generator (hex, from the spec).
pub const G1_GEN_X: &str = "17f1d3a73197d7942695638c4fa9ac0fc3688c4f9774b905a14e3a3f171bac586c55e83ff97a1aeffb3af00adb22c6bb";
pub const G1_GEN_Y: &str = "08b3f481e3aaa0f1a09e30ed741d8ae4fcf5e095d5d00af600db18cb2c04b3edd03cc744a2888ae40caa232946c5e7e1";

/// EIP-2537 G2 generator (c0/c1 pairs).
pub const G2_GEN_X_C0: &str = "024aa2b2f08f0a91260805272dc51051c6e47ad4fa403b02b4510b647ae3d1770bac0326a805bbefd48056c8c121bdb8";
pub const G2_GEN_X_C1: &str = "13e02b6052719f607dacd3a088274f65596bd0d09920b61ab5da61bbdc7f5049334cf11213945d57e5ac7d055d042b7e";
pub const G2_GEN_Y_C0: &str = "0ce5d527727d6e118cc9cdc6da2e351aadfd9baa8cbdd3a76d429a695160d12c923ac9cc3baca289e193548608b82801";
pub const G2_GEN_Y_C1: &str = "0606c4a02ea734cc32acd2b02bc28b99cb3e287e85a763af267492ab572e99ab3f370d275cec1da1aaa9075ff05f79be";

fn parse_fp_hex(s: &str) -> Fp {
    Fp::from_be_bytes(&hex::decode(s).unwrap())
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct G1Affine {
    pub x: Fp,
    pub y: Fp,
    pub infinity: bool,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct G2Affine {
    pub x: Fp2,
    pub y: Fp2,
    pub infinity: bool,
}

pub fn g1_generator() -> G1Affine {
    G1Affine { x: parse_fp_hex(G1_GEN_X), y: parse_fp_hex(G1_GEN_Y), infinity: false }
}

pub fn g2_generator() -> G2Affine {
    G2Affine {
        x: Fp2::new(parse_fp_hex(G2_GEN_X_C0), parse_fp_hex(G2_GEN_X_C1)),
        y: Fp2::new(parse_fp_hex(G2_GEN_Y_C0), parse_fp_hex(G2_GEN_Y_C1)),
        infinity: false,
    }
}

impl G1Affine {
    pub fn identity() -> G1Affine { G1Affine { x: Fp::zero(), y: Fp::zero(), infinity: true } }

    pub fn is_on_curve(&self) -> bool {
        if self.infinity {
            return self.x.is_zero() && self.y.is_zero();
        }
        // y^2 == x^3 + 4
        self.y.square() == self.x.square().mul(&self.x).add(&Fp::from_u64(B_G1))
    }

    pub fn is_in_subgroup(&self) -> bool {
        if self.infinity { return true; }
        self.mul(&r_suborder()).infinity
    }

    pub fn neg(&self) -> G1Affine {
        if self.infinity { self.clone() } else { G1Affine { x: self.x.clone(), y: self.y.neg(), infinity: false } }
    }

    pub fn add(&self, o: &G1Affine) -> G1Affine {
        if self.infinity { return o.clone(); }
        if o.infinity { return self.clone(); }
        if self.x == o.x {
            if self.y == o.y {
                return self.double();
            }
            return G1Affine::identity();
        }
        // chord slope
        let lambda = o.y.sub(&self.y).mul(&o.x.sub(&self.x).inv().unwrap());
        let x3 = lambda.square().sub(&self.x).sub(&o.x);
        let y3 = lambda.mul(&self.x.sub(&x3)).sub(&self.y);
        G1Affine { x: x3, y: y3, infinity: false }
    }

    pub fn double(&self) -> G1Affine {
        if self.infinity || self.y.is_zero() {
            return G1Affine::identity();
        }
        let lambda = self.x.square().mul(&Fp::from_u64(3)).mul(&self.y.double().inv().unwrap());
        let x3 = lambda.square().sub(&self.x.double());
        let y3 = lambda.mul(&self.x.sub(&x3)).sub(&self.y);
        G1Affine { x: x3, y: y3, infinity: false }
    }

    pub fn mul(&self, k: &BigUint) -> G1Affine {
        let mut r = G1Jac::identity();
        let bytes = k.to_bytes_be();
        for byte in &bytes {
            for i in (0..8).rev() {
                r = r.double();
                if (byte >> i) & 1 == 1 {
                    r = r.add_affine(self);
                }
            }
        }
        r.to_affine()
    }

    /// EIP-2537 uncompressed encoding: x (64B BE) || y (64B BE); infinity = 128 zero bytes.
    pub fn encode128(&self) -> [u8; 128] {
        let mut out = [0u8; 128];
        if !self.infinity {
            out[..64].copy_from_slice(&be64(&self.x));
            out[64..].copy_from_slice(&be64(&self.y));
        }
        out
    }

    pub fn decode128(bytes: &[u8]) -> Result<G1Affine, String> {
        if bytes.len() != 128 {
            return Err("G1: need 128 bytes".into());
        }
        let x = be64_to_fp(&bytes[..64])?;
        let y = be64_to_fp(&bytes[64..])?;
        if x.is_zero() && y.is_zero() {
            return Ok(G1Affine::identity());
        }
        let p = G1Affine { x, y, infinity: false };
        if !p.is_on_curve() {
            return Err("G1: point not on curve".into());
        }
        Ok(p)
    }

    /// ZCash-format compressed encoding (48 bytes): x with flag bits
    /// 0x80 = compressed, 0x40 = infinity, 0x20 = y-lexicographically-greater.
    pub fn compress(&self) -> [u8; 48] {
        let mut out = self.x.to_be48();
        if self.infinity {
            out[0] |= 0xc0; // compressed + infinity
            return out;
        }
        out[0] |= 0x80;
        if is_y_greater_fp(&self.y) {
            out[0] |= 0x20;
        }
        out
    }

    pub fn decompress(bytes: &[u8]) -> Result<G1Affine, String> {
        if bytes.len() != 48 {
            return Err("G1: need 48 bytes".into());
        }
        if bytes[0] & 0x80 == 0 {
            return Err("G1: uncompressed point not supported here".into());
        }
        if bytes[0] & 0x40 != 0 {
            return Ok(G1Affine::identity());
        }
        let mut xbytes = [0u8; 48];
        xbytes.copy_from_slice(bytes);
        xbytes[0] &= 0x1f;
        let x = Fp::from_be48(&xbytes)?;
        let y2 = x.square().mul(&x).add(&Fp::from_u64(B_G1));
        let mut y = y2.sqrt().map_err(|_| "G1: x is not on curve".to_string())?;
        let greater_flag = bytes[0] & 0x20 != 0;
        if is_y_greater_fp(&y) != greater_flag {
            y = y.neg();
        }
        let p = G1Affine { x, y, infinity: false };
        if !p.is_on_curve() {
            return Err("G1: decompressed point not on curve".into());
        }
        Ok(p)
    }
}

fn is_y_greater_fp(y: &Fp) -> bool {
    // "greater" = y > p - y  (lexicographic on canonical BE bytes = numeric compare)
    let neg = y.neg();
    y.0 > neg.0
}

fn is_y_greater_fp2(y: &Fp2) -> bool {
    // compare c1 first, then c0 (big-endian coefficient order per zcash/blst)
    let neg = y.neg();
    if y.c1.0 != neg.c1.0 {
        y.c1.0 > neg.c1.0
    } else {
        y.c0.0 > neg.c0.0
    }
}

fn be64(v: &Fp) -> [u8; 64] {
    let b = v.0.to_bytes_be();
    let mut out = [0u8; 64];
    let start = 64usize.saturating_sub(b.len());
    out[start..].copy_from_slice(&b);
    out
}

fn be64_to_fp(bytes: &[u8]) -> Result<Fp, String> {
    if bytes.len() != 64 {
        return Err("Fp: need 64 bytes".into());
    }
    // top 16 bytes must be zero (value < 381-bit p in 64-byte encoding)
    if bytes[..16].iter().any(|&b| b != 0) {
        return Err("Fp: top 16 bytes must be zero".into());
    }
    Ok(Fp::from_be48(&bytes[16..])?)
}

impl G2Affine {
    pub fn identity() -> G2Affine { G2Affine { x: Fp2::zero(), y: Fp2::zero(), infinity: true } }

    pub fn is_on_curve(&self) -> bool {
        if self.infinity {
            return self.x.is_zero() && self.y.is_zero();
        }
        // y^2 == x^3 + 4(1+u)
        let b = Fp2::new(Fp::from_u64(B_G2_C0), Fp::from_u64(B_G2_C1));
        self.y.square() == self.x.square().mul(&self.x).add(&b)
    }

    pub fn is_in_subgroup(&self) -> bool {
        if self.infinity { return true; }
        self.mul(&r_suborder()).infinity
    }

    pub fn neg(&self) -> G2Affine {
        if self.infinity { self.clone() } else { G2Affine { x: self.x.clone(), y: self.y.neg(), infinity: false } }
    }

    pub fn add(&self, o: &G2Affine) -> G2Affine {
        if self.infinity { return o.clone(); }
        if o.infinity { return self.clone(); }
        if self.x == o.x {
            if self.y == o.y {
                return self.double();
            }
            return G2Affine::identity();
        }
        let lambda = o.y.sub(&self.y).mul(&o.x.sub(&self.x).inv().unwrap());
        let x3 = lambda.square().sub(&self.x).sub(&o.x);
        let y3 = lambda.mul(&self.x.sub(&x3)).sub(&self.y);
        G2Affine { x: x3, y: y3, infinity: false }
    }

    pub fn double(&self) -> G2Affine {
        if self.infinity || self.y.is_zero() {
            return G2Affine::identity();
        }
        let lambda = self.x.square().mul(&Fp2::new(Fp::from_u64(3), Fp::zero())).mul(&self.y.double().inv().unwrap());
        let x3 = lambda.square().sub(&self.x.double());
        let y3 = lambda.mul(&self.x.sub(&x3)).sub(&self.y);
        G2Affine { x: x3, y: y3, infinity: false }
    }

    pub fn mul(&self, k: &BigUint) -> G2Affine {
        let mut r = G2Affine::identity();
        let bytes = k.to_bytes_be();
        for byte in &bytes {
            for i in (0..8).rev() {
                r = r.double();
                if (byte >> i) & 1 == 1 {
                    r = r.add(self);
                }
            }
        }
        r
    }

    /// EIP-2537 uncompressed: x(c0||c1 128B) || y(c0||c1 128B); infinity = 256 zeros.
    pub fn encode256(&self) -> [u8; 256] {
        let mut out = [0u8; 256];
        if !self.infinity {
            out[..64].copy_from_slice(&be64(&self.x.c0));
            out[64..128].copy_from_slice(&be64(&self.x.c1));
            out[128..192].copy_from_slice(&be64(&self.y.c0));
            out[192..].copy_from_slice(&be64(&self.y.c1));
        }
        out
    }

    pub fn decode256(bytes: &[u8]) -> Result<G2Affine, String> {
        if bytes.len() != 256 {
            return Err("G2: need 256 bytes".into());
        }
        let x = Fp2::new(be64_to_fp(&bytes[..64])?, be64_to_fp(&bytes[64..128])?);
        let y = Fp2::new(be64_to_fp(&bytes[128..192])?, be64_to_fp(&bytes[192..])?);
        if x.is_zero() && y.is_zero() {
            return Ok(G2Affine::identity());
        }
        let p = G2Affine { x, y, infinity: false };
        if !p.is_on_curve() {
            return Err("G2: point not on curve".into());
        }
        Ok(p)
    }

    /// ZCash-format compressed (96 bytes): x_c1 || x_c0 with flags in first byte.
    pub fn compress(&self) -> [u8; 96] {
        let mut out = [0u8; 96];
        out[..48].copy_from_slice(&self.x.c1.to_be48());
        out[48..].copy_from_slice(&self.x.c0.to_be48());
        if self.infinity {
            out[0] |= 0xc0;
            return out;
        }
        out[0] |= 0x80;
        if is_y_greater_fp2(&self.y) {
            out[0] |= 0x20;
        }
        out
    }

    pub fn decompress(bytes: &[u8]) -> Result<G2Affine, String> {
        if bytes.len() != 96 {
            return Err("G2: need 96 bytes".into());
        }
        if bytes[0] & 0x80 == 0 {
            return Err("G2: uncompressed point not supported here".into());
        }
        if bytes[0] & 0x40 != 0 {
            return Ok(G2Affine::identity());
        }
        let mut xbytes = [0u8; 96];
        xbytes.copy_from_slice(bytes);
        xbytes[0] &= 0x1f;
        let xc1 = Fp::from_be48(&xbytes[..48])?;
        let xc0 = Fp::from_be48(&xbytes[48..])?;
        let x = Fp2::new(xc0, xc1);
        let b = Fp2::new(Fp::from_u64(B_G2_C0), Fp::from_u64(B_G2_C1));
        let y2 = x.square().mul(&x).add(&b);
        let mut y = y2.sqrt().map_err(|_| "G2: x is not on curve".to_string())?;
        let greater_flag = bytes[0] & 0x20 != 0;
        if is_y_greater_fp2(&y) != greater_flag {
            y = y.neg();
        }
        let p = G2Affine { x, y, infinity: false };
        if !p.is_on_curve() {
            return Err("G2: decompressed point not on curve".into());
        }
        Ok(p)
    }
}

// ---------- projective (Jacobian) arithmetic: removes per-op inversions ----------

#[derive(Clone, Debug)]
struct G1Jac {
    x: Fp,
    y: Fp,
    z: Fp,
}

impl G1Jac {
    fn identity() -> G1Jac {
        G1Jac { x: Fp::one(), y: Fp::one(), z: Fp::zero() }
    }

    fn from_affine(p: &G1Affine) -> G1Jac {
        if p.infinity {
            Self::identity()
        } else {
            G1Jac { x: p.x.clone(), y: p.y.clone(), z: Fp::one() }
        }
    }

    fn is_identity(&self) -> bool {
        self.z.is_zero()
    }

    fn double(&self) -> G1Jac {
        if self.is_identity() {
            return self.clone();
        }
        // dbl-2007-bl (a = 0)
        let a = self.x.square();
        let b = self.y.square();
        let c = b.square();
        let xb = self.x.add(&b);
        let d = xb.square().sub(&a).sub(&c).double();
        let e = a.double().add(&a);
        let f = e.square();
        let x3 = f.sub(&d.double());
        let y3 = e.mul(&d.sub(&x3)).sub(&c.double().double().double());
        let z3 = self.y.double().mul(&self.z);
        G1Jac { x: x3, y: y3, z: z3 }
    }

    fn add_affine(&self, q: &G1Affine) -> G1Jac {
        if q.infinity {
            return self.clone();
        }
        if self.is_identity() {
            return G1Jac::from_affine(q);
        }
        // madd-2007-bl with Z2 = 1
        let z1z1 = self.z.square();
        let u2 = q.x.mul(&z1z1);
        let s2 = q.y.mul(&self.z).mul(&z1z1);
        let h = u2.sub(&self.x);
        if h.is_zero() {
            if self.y == s2 {
                return self.double();
            }
            return Self::identity();
        }
        let hh = h.square();
        let i = hh.double().double();
        let j = h.mul(&i);
        let r = s2.sub(&self.y).double();
        let v = self.x.mul(&i);
        let x3 = r.square().sub(&j).sub(&v.double());
        let y3 = r.mul(&v.sub(&x3)).sub(&self.y.mul(&j).double());
        let z3 = self.z.add(&h).square().sub(&z1z1).sub(&hh);
        G1Jac { x: x3, y: y3, z: z3 }
    }

    fn to_affine(&self) -> G1Affine {
        if self.is_identity() {
            return G1Affine::identity();
        }
        let zinv = self.z.inv().unwrap();
        let zinv2 = zinv.square();
        G1Affine {
            x: self.x.mul(&zinv2),
            y: self.y.mul(&zinv2.mul(&zinv)),
            infinity: false,
        }
    }
}

/// Pippenger multi-scalar multiplication over G1. Scalars are full-width (mod r applied by caller if desired).
pub fn msm_g1(points: &[G1Affine], scalars: &[BigUint]) -> G1Affine {
    assert_eq!(points.len(), scalars.len());
    if points.is_empty() {
        return G1Affine::identity();
    }
    if points.len() == 1 {
        return points[0].mul(&scalars[0]);
    }
    const W: usize = 5; // window bits
    let nbits = 255;
    let num_windows = (nbits + W - 1) / W;
    let mut acc = G1Jac::identity();

    for w in (0..num_windows).rev() {
        // bucket accumulate: 2^W buckets (index 0 unused)
        let nbuckets = 1 << W;
        let mut buckets: Vec<G1Jac> = vec![G1Jac::identity(); nbuckets];
        for (p, s) in points.iter().zip(scalars.iter()) {
            if p.infinity || s.is_zero() {
                continue;
            }
            let window = {
                // extract W bits starting at w*W from BE representation
                let bytes = s.to_bytes_be();
                let bit_len = bytes.len() * 8;
                let bit_index = w * W;
                if bit_index >= bit_len {
                    continue;
                }
                let mut val = 0u32;
                for b in 0..W {
                    let bi = bit_index + b;
                    if bi >= bit_len { break; }
                    let byte_i = bytes.len() - 1 - bi / 8;
                    let bit_i = bi % 8;
                    val |= (((bytes[byte_i] >> bit_i) & 1) as u32) << b;
                }
                val as usize
            };
            if window == 0 {
                continue;
            }
            buckets[window] = buckets[window].add_affine(p);
        }
        // sum bucket chain
        let mut running = G1Jac::identity();
        for j in (1..nbuckets).rev() {
            running = running.add_affine(&buckets[j].to_affine());
            acc = acc.add_affine(&running.to_affine());
        }
        if w > 0 {
            for _ in 0..W {
                acc = acc.double();
            }
        }
    }
    acc.to_affine()
}

/// Pippenger MSM over G2.
pub fn msm_g2(points: &[G2Affine], scalars: &[BigUint]) -> G2Affine {
    assert_eq!(points.len(), scalars.len());
    if points.is_empty() {
        return G2Affine::identity();
    }
    if points.len() == 1 {
        return points[0].mul(&scalars[0]);
    }
    const W: usize = 4;
    let nbits = 255;
    let num_windows = (nbits + W - 1) / W;
    let mut acc = G2Affine::identity();

    for w in (0..num_windows).rev() {
        let nbuckets = 1 << W;
        let mut buckets: Vec<G2Affine> = vec![G2Affine::identity(); nbuckets];
        for (p, s) in points.iter().zip(scalars.iter()) {
            if p.infinity || s.is_zero() {
                continue;
            }
            let bytes = s.to_bytes_be();
            let bit_len = bytes.len() * 8;
            let bit_index = w * W;
            if bit_index >= bit_len {
                continue;
            }
            let mut val = 0u32;
            for b in 0..W {
                let bi = bit_index + b;
                if bi >= bit_len { break; }
                let byte_i = bytes.len() - 1 - bi / 8;
                let bit_i = bi % 8;
                val |= (((bytes[byte_i] >> bit_i) & 1) as u32) << b;
            }
            if val == 0 {
                continue;
            }
            buckets[val as usize] = buckets[val as usize].add(p);
        }
        let mut running = G2Affine::identity();
        for j in (1..nbuckets).rev() {
            running = running.add(&buckets[j]);
            acc = acc.add(&running);
        }
        if w > 0 {
            for _ in 0..W {
                acc = acc.double();
            }
        }
    }
    acc
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn generators_on_curve_and_in_subgroup() {
        let g = g1_generator();
        assert!(g.is_on_curve());
        assert!(g.is_in_subgroup());
        let h = g2_generator();
        assert!(h.is_on_curve());
        assert!(h.is_in_subgroup());
    }

    #[test]
    fn g1_group_laws() {
        let g = g1_generator();
        let two_g = g.double();
        assert_eq!(g.add(&g), two_g);
        assert_eq!(g.add(&g.neg()), G1Affine::identity());
        assert_eq!(g.add(&G1Affine::identity()), g);
        // scalar consistency: 5G via mul vs repeated add
        let five = BigUint::from(5u32);
        let m = g.mul(&five);
        assert_eq!(m, two_g.add(&two_g).add(&g));
        // (r+1)P == P
        let k = r_suborder() + 1u32;
        assert_eq!(g.mul(&k), g);
    }

    #[test]
    fn g1_encoding_roundtrip() {
        let g = g1_generator();
        let enc = g.encode128();
        let dec = G1Affine::decode128(&enc).unwrap();
        assert_eq!(g, dec);
        // compressed roundtrip
        let comp = g.compress();
        let dec2 = G1Affine::decompress(&comp).unwrap();
        assert_eq!(g, dec2);
        // identity encodings
        let inf = G1Affine::identity();
        assert!(G1Affine::decode128(&inf.encode128()).unwrap().infinity);
        let c = inf.compress();
        assert!(G1Affine::decompress(&c).unwrap().infinity);
    }

    #[test]
    fn g2_group_laws_and_encoding() {
        let h = g2_generator();
        assert_eq!(h.add(&h), h.double());
        assert_eq!(h.add(&h.neg()), G2Affine::identity());
        let comp = h.compress();
        let dec = G2Affine::decompress(&comp).unwrap();
        assert_eq!(h, dec);
        let enc = h.encode256();
        assert_eq!(G2Affine::decode256(&enc).unwrap(), h);
        // (r+1)H == H
        let k = r_suborder() + 1u32;
        assert_eq!(h.mul(&k), h);
    }

    #[test]
    fn msm_matches_naive() {
        let g = g1_generator();
        let pts = vec![g.clone(), g.double(), g.double().double(), g.mul(&BigUint::from(7u32))];
        let scs = vec![BigUint::from(3u32), BigUint::from(11u32), BigUint::from(2u32), BigUint::from(5u32)];
        let pipp = msm_g1(&pts, &scs);
        let naive = pts
            .iter()
            .zip(scs.iter())
            .fold(G1Affine::identity(), |acc, (p, s)| acc.add(&p.mul(s)));
        assert_eq!(pipp, naive);
    }
}
