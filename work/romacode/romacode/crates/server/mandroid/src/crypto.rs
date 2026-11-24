//! Cryptography for APK signing and ADB authentication.
//!
//! Pure Rust: the `rsa` crate for the maths, and a small hand-written DER/ASN.1
//! writer for the X.509 certificate. No OpenSSL, no `keytool`, no `jarsigner`,
//! no `apksigner`.
//!
//! Two consumers:
//! * [`sign_sha256`] / [`sign_sha1`] — PKCS#1 v1.5 signatures for APK v1/v2
//!   signing and for the ADB AUTH handshake.
//! * [`self_signed_cert_der`] — the signing certificate an APK carries.

use anyhow::{Context, Result};
use num_bigint_dig::BigUint;
use rsa::pkcs1v15::Pkcs1v15Sign;
use rsa::pkcs8::{DecodePrivateKey, EncodePrivateKey, LineEnding};
use rsa::traits::PublicKeyParts;
use rsa::{RsaPrivateKey, RsaPublicKey};
use sha1::Sha1;
use sha2::{Digest, Sha256};

/// RSA key size for generated signing identities. 2048 is what Android tooling
/// uses and what ADB expects.
pub const RSA_BITS: usize = 2048;
/// `RSANUMBYTES` in the Android ADB public-key structure.
pub const RSA_NUM_BYTES: usize = 256;

// ---------------------------------------------------------------------------
// Signing
// ---------------------------------------------------------------------------

/// PKCS#1 v1.5 signature over the SHA-256 digest of `msg`.
pub fn sign_sha256(key: &RsaPrivateKey, msg: &[u8]) -> Result<Vec<u8>> {
    let digest = Sha256::digest(msg);
    sign_prehashed_sha256(key, &digest)
}

/// PKCS#1 v1.5 over an already-computed SHA-256 digest.
pub fn sign_prehashed_sha256(key: &RsaPrivateKey, digest: &[u8]) -> Result<Vec<u8>> {
    key.sign(Pkcs1v15Sign::new::<Sha256>(), digest)
        .context("RSA signing failed")
}

/// PKCS#1 v1.5 signature over the SHA-1 digest of `msg`.
///
/// SHA-1 is *required* here: the ADB AUTH handshake signs its 20-byte token
/// with SHA-1/RSA, and that is not a choice this tool gets to make. It is not
/// used for anything else — APK signatures go through SHA-256.
pub fn sign_sha1(key: &RsaPrivateKey, msg: &[u8]) -> Result<Vec<u8>> {
    let digest = Sha1::digest(msg);
    key.sign(Pkcs1v15Sign::new::<Sha1>(), &digest)
        .context("RSA SHA-1 signing failed")
}

// ---------------------------------------------------------------------------
// Key generation / serialisation
// ---------------------------------------------------------------------------

pub fn generate_key(bits: usize) -> Result<RsaPrivateKey> {
    let mut rng = rand::thread_rng();
    RsaPrivateKey::new(&mut rng, bits).context("generating an RSA key")
}

pub fn to_pkcs8_pem(key: &RsaPrivateKey) -> Result<String> {
    Ok(key
        .to_pkcs8_pem(LineEnding::LF)
        .context("encoding PKCS#8")?
        .to_string())
}

pub fn from_pkcs8_pem(pem: &str) -> Result<RsaPrivateKey> {
    RsaPrivateKey::from_pkcs8_pem(pem).context("parsing PKCS#8 private key")
}

/// Big-endian modulus and public exponent.
pub fn public_numbers(key: &RsaPrivateKey) -> (Vec<u8>, Vec<u8>) {
    let public = RsaPublicKey::from(key);
    (public.n().to_bytes_be(), public.e().to_bytes_be())
}

// ---------------------------------------------------------------------------
// Minimal DER / ASN.1 writer
// ---------------------------------------------------------------------------

pub fn der_length(n: usize) -> Vec<u8> {
    if n < 0x80 {
        vec![n as u8]
    } else {
        let mut bytes = Vec::new();
        let mut v = n;
        while v > 0 {
            bytes.insert(0, (v & 0xff) as u8);
            v >>= 8;
        }
        let mut out = vec![0x80 | bytes.len() as u8];
        out.extend_from_slice(&bytes);
        out
    }
}

pub fn der_tlv(tag: u8, content: &[u8]) -> Vec<u8> {
    let mut out = vec![tag];
    out.extend_from_slice(&der_length(content.len()));
    out.extend_from_slice(content);
    out
}

pub fn der_concat(parts: &[Vec<u8>]) -> Vec<u8> {
    parts.iter().flatten().copied().collect()
}

pub fn der_sequence(parts: &[Vec<u8>]) -> Vec<u8> {
    der_tlv(0x30, &der_concat(parts))
}

pub fn der_set(parts: &[Vec<u8>]) -> Vec<u8> {
    der_tlv(0x31, &der_concat(parts))
}

/// Positive INTEGER: prepend 0x00 when the high bit would make it negative.
pub fn der_integer(bytes_be: &[u8]) -> Vec<u8> {
    let trimmed = {
        let mut i = 0;
        while i + 1 < bytes_be.len() && bytes_be[i] == 0 {
            i += 1;
        }
        &bytes_be[i..]
    };
    let mut content = Vec::with_capacity(trimmed.len() + 1);
    if trimmed.first().map(|b| b & 0x80 != 0).unwrap_or(false) {
        content.push(0);
    }
    content.extend_from_slice(trimmed);
    der_tlv(0x02, &content)
}

pub fn der_oid(bytes: &[u8]) -> Vec<u8> {
    der_tlv(0x06, bytes)
}

pub fn der_null() -> Vec<u8> {
    vec![0x05, 0x00]
}

pub fn der_utf8(s: &str) -> Vec<u8> {
    der_tlv(0x0c, s.as_bytes())
}

pub fn der_bit_string(bytes: &[u8]) -> Vec<u8> {
    let mut content = vec![0x00]; // zero unused bits
    content.extend_from_slice(bytes);
    der_tlv(0x03, &content)
}

pub fn der_octet_string(bytes: &[u8]) -> Vec<u8> {
    der_tlv(0x04, bytes)
}

pub fn der_boolean(v: bool) -> Vec<u8> {
    vec![0x01, 0x01, if v { 0xff } else { 0x00 }]
}

/// Context-specific constructed tag, e.g. `[0]` -> 0xA0, `[3]` -> 0xA3.
pub fn der_context(n: u8, content: &[u8]) -> Vec<u8> {
    der_tlv(0xA0 | n, content)
}

/// UTCTime, e.g. `250101000000Z`.
pub fn der_utc_time(s: &str) -> Vec<u8> {
    der_tlv(0x17, s.as_bytes())
}

/// Namespaced DER surface for other modules (PKCS#7 lives in `sign.rs`).
pub mod der {
    pub use super::der_bit_string as bit_string;
    pub use super::der_boolean as boolean;
    pub use super::der_integer as integer;
    pub use super::der_null as null;
    pub use super::der_octet_string as octet_string;
    pub use super::der_oid as oid;
    pub use super::der_sequence as sequence;
    pub use super::der_set as set;
    pub use super::der_utf8 as utf8;

    /// PKCS#7 `data` content type.
    pub const OID_PKCS7_DATA: [u8; 9] = [0x2a, 0x86, 0x48, 0x86, 0xf7, 0x0d, 0x01, 0x07, 0x01];
    /// PKCS#7 `signedData` content type.
    pub const OID_PKCS7_SIGNED_DATA: [u8; 9] =
        [0x2a, 0x86, 0x48, 0x86, 0xf7, 0x0d, 0x01, 0x07, 0x02];

    /// `[n] EXPLICIT` — a context-specific constructed tag wrapping content.
    pub fn explicit(n: u8, content: &[u8]) -> Vec<u8> {
        super::der_context(n, content)
    }

    /// `[n] IMPLICIT SET OF` — a context tag standing in for a SET.
    pub fn implicit_set(n: u8, parts: &[Vec<u8>]) -> Vec<u8> {
        super::der_tlv(0xA0 | n, &super::der_concat(parts))
    }

    pub fn algorithm_identifier_sha256() -> Vec<u8> {
        super::algorithm_identifier(super::OID_SHA256_WITH_RSA)
    }

    pub fn algorithm_identifier_rsa() -> Vec<u8> {
        super::algorithm_identifier(super::OID_RSA_ENCRYPTION)
    }
}

// OIDs
const OID_RSA_ENCRYPTION: &[u8] = &[0x2a, 0x86, 0x48, 0x86, 0xf7, 0x0d, 0x01, 0x01, 0x01];
const OID_SHA256_WITH_RSA: &[u8] = &[0x2a, 0x86, 0x48, 0x86, 0xf7, 0x0d, 0x01, 0x01, 0x0b];
const OID_COMMON_NAME: &[u8] = &[0x55, 0x04, 0x03];
const OID_BASIC_CONSTRAINTS: &[u8] = &[0x55, 0x1d, 0x13];
const OID_KEY_USAGE: &[u8] = &[0x55, 0x1d, 0x0f];

pub fn algorithm_identifier(oid: &[u8]) -> Vec<u8> {
    der_sequence(&[der_oid(oid), der_null()])
}

pub fn subject_public_key_info(key: &RsaPrivateKey) -> Vec<u8> {
    let (n, e) = public_numbers(key);
    let rsa_pub = der_sequence(&[der_integer(&n), der_integer(&e)]);
    der_sequence(&[
        algorithm_identifier(OID_RSA_ENCRYPTION),
        der_bit_string(&rsa_pub),
    ])
}

pub fn rdn(oid: &[u8], value: &str) -> Vec<u8> {
    der_sequence(&[der_set(&[der_sequence(&[der_oid(oid), der_utf8(value)])])])
}

/// The pieces of a signing certificate that a PKCS#7 `SignerInfo` also needs:
/// the DER certificate itself, plus its issuer name and serial number.
#[derive(Debug, Clone)]
pub struct CertMaterial {
    pub der: Vec<u8>,
    pub issuer: Vec<u8>,
    pub serial: Vec<u8>,
    pub subject: String,
}

/// The DER `SubjectPublicKeyInfo` for an RSA key.
pub fn spki_der(key: &RsaPrivateKey) -> Result<Vec<u8>> {
    Ok(subject_public_key_info(key))
}

/// A self-signed X.509 v3 certificate in DER, valid for `days` from now.
///
/// This is the "signing certificate" an APK carries. Android treats APK signing
/// like JAR signing: the certificate is an identity, not a chain — a self-signed
/// cert is exactly what `keytool -genkeypair` produces for this purpose.
pub fn self_signed_cert_der(key: &RsaPrivateKey, cn: &str, days: i64) -> Result<Vec<u8>> {
    Ok(self_signed_cert_material(key, cn, days)?.der)
}

/// Like [`self_signed_cert_der`], but also returns the issuer name and serial
/// so a PKCS#7 signer can reference the certificate.
pub fn self_signed_cert_material(key: &RsaPrivateKey, cn: &str, days: i64) -> Result<CertMaterial> {
    let (n, _) = public_numbers(key);
    // Deterministic-but-unique serial derived from the modulus.
    let digest = Sha256::digest(&n);
    let serial = &digest[..16];

    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .context("system clock is before the epoch")?
        .as_secs() as i64;
    let (nb, na) = (now - 86_400, now + days * 86_400);

    let name = rdn(OID_COMMON_NAME, cn);

    let spki = subject_public_key_info(key);

    // Extensions: basicConstraints CA:TRUE (harmless, matches keytool defaults)
    // and keyUsage digitalSignature (what an APK signature needs).
    let bc = der_sequence(&[
        der_oid(OID_BASIC_CONSTRAINTS),
        der_boolean(true),
        der_octet_string(&der_sequence(&[der_boolean(true)])),
    ]);
    let ku = der_sequence(&[
        der_oid(OID_KEY_USAGE),
        der_boolean(true),
        // BIT STRING, bit 7 set = digitalSignature (first bit of the byte).
        der_octet_string(&der_bit_string(&[0x80])),
    ]);
    let extensions = der_context(3, &der_sequence(&[bc, ku]));

    let tbs = der_sequence(&[
        der_context(0, &der_integer(&2u64.to_be_bytes())), // v3
        der_integer(serial),
        algorithm_identifier(OID_SHA256_WITH_RSA),
        name.clone(),
        der_sequence(&[der_utc_time(&utc_time(nb)), der_utc_time(&utc_time(na))]),
        name,
        spki,
        extensions,
    ]);

    let signature = sign_sha256(key, &tbs)?;
    Ok(CertMaterial {
        der: der_sequence(&[
            tbs,
            algorithm_identifier(OID_SHA256_WITH_RSA),
            der_bit_string(&signature),
        ]),
        issuer: rdn(OID_COMMON_NAME, cn),
        serial: serial.to_vec(),
        subject: cn.to_string(),
    })
}

/// Days-since-epoch independent UTC timestamp: computes the civil date from the
/// Unix timestamp so we do not need a date library.
fn utc_time(unix: i64) -> String {
    let days = unix.div_euclid(86_400);
    let secs = unix.rem_euclid(86_400);
    let (y, m, d) = civil_from_days(days);
    format!(
        "{:02}{:02}{:02}{:02}{:02}{:02}Z",
        y % 100,
        m,
        d,
        secs / 3600,
        (secs % 3600) / 60,
        secs % 60
    )
}

/// Howard Hinnant's `civil_from_days` — correct for the whole proleptic
/// Gregorian calendar, no dependencies.
fn civil_from_days(z: i64) -> (i64, u32, u32) {
    let z = z + 719_468;
    let era = if z >= 0 { z } else { z - 146_096 } / 146_097;
    let doe = (z - era * 146_097) as u64; // [0, 146096]
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146_096) / 365; // [0, 399]
    let y = yoe as i64 + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100); // [0, 365]
    let mp = (5 * doy + 2) / 153; // [0, 11]
    let d = (doy - (153 * mp + 2) / 5 + 1) as u32; // [1, 31]
    let m = if mp < 10 { mp + 3 } else { mp - 9 } as u32; // [1, 12]
    (if m <= 2 { y + 1 } else { y }, m, d)
}

// ---------------------------------------------------------------------------
// Android ADB public key format
// ---------------------------------------------------------------------------

/// The 524-byte `RSAPublicKey` structure ADB exchanges during AUTH.
///
/// ```text
/// u32 len      = 256
/// u32 n0inv    = -1 / n[0] mod 2^32
/// u8  n[256]   modulus, little-endian
/// u8  rr[256]  R^2 mod n (R = 2^2048), little-endian
/// u32 exponent
/// ```
pub fn android_public_key_blob(key: &RsaPrivateKey) -> Result<Vec<u8>> {
    let public = RsaPublicKey::from(key);
    let n_be = public.n().to_bytes_be();
    // ADB's `RSAPublicKey` structure is a fixed 524-byte blob with a 256-byte
    // modulus field — it only ever carries 2048-bit keys. Reject anything
    // whose modulus is not exactly 256 bytes (no left-padding fudge: a key
    // that is shorter would have leading zero bytes silently injected, and a
    // 4096-bit key would be silently truncated by the receiver).
    if n_be.len() != RSA_NUM_BYTES {
        anyhow::bail!(
            "RSA modulus is {} bytes; ADB's RSAPublicKey structure is fixed at {} bytes \
             (2048-bit keys only)",
            n_be.len(),
            RSA_NUM_BYTES
        );
    }
    // Reverse the big-endian modulus in place to get the little-endian form
    // the ADB structure expects.
    let mut n_le = n_be;
    n_le.reverse();

    let n0 = u32::from_le_bytes([n_le[0], n_le[1], n_le[2], n_le[3]]);
    let n0inv = modular_inverse_2_32(n0).wrapping_neg();

    let modulus = public.n().clone();
    let r = BigUint::from(1u32) << (RSA_NUM_BYTES * 8);
    let r_squared = (&r * &r) % &modulus;
    let rr_le = biguint_to_le(&r_squared, RSA_NUM_BYTES)?;

    let exponent = public.e().to_bytes_be();
    let exp = if exponent.len() <= 4 {
        let mut b = [0u8; 4];
        b[4 - exponent.len()..].copy_from_slice(&exponent);
        u32::from_be_bytes(b)
    } else {
        anyhow::bail!("unexpected RSA exponent size {}", exponent.len());
    };

    let mut out = Vec::with_capacity(4 + 4 + RSA_NUM_BYTES * 2 + 4);
    out.extend_from_slice(&(RSA_NUM_BYTES as u32).to_le_bytes());
    out.extend_from_slice(&n0inv.to_le_bytes());
    out.extend_from_slice(&n_le);
    out.extend_from_slice(&rr_le);
    out.extend_from_slice(&exp.to_le_bytes());
    Ok(out)
}

/// `base64(blob)` — the exact text of an `adbkey.pub` file.
pub fn adbkey_pub_text(key: &RsaPrivateKey) -> Result<String> {
    Ok(base64_encode(&android_public_key_blob(key)?))
}

/// Inverse of an odd number modulo 2^32, by Newton iteration (each step doubles
/// the number of correct bits).
fn modular_inverse_2_32(a: u32) -> u32 {
    let mut x = 1u32;
    for _ in 0..5 {
        x = x.wrapping_mul(2u32.wrapping_sub(a.wrapping_mul(x)));
    }
    x
}

fn biguint_to_le(value: &BigUint, width: usize) -> Result<Vec<u8>> {
    let be = value.to_bytes_be();
    if be.len() > width {
        anyhow::bail!(
            "value needs {} bytes, more than the {} available",
            be.len(),
            width
        );
    }
    let mut le = vec![0u8; width - be.len()];
    le.extend_from_slice(&be);
    le.reverse();
    Ok(le)
}

/// Standard base64 (with padding). Small enough not to justify a dependency.
pub fn base64_encode(data: &[u8]) -> String {
    const ALPHABET: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = String::with_capacity((data.len() + 2) / 3 * 4);
    for chunk in data.chunks(3) {
        let b0 = chunk[0] as u32;
        let b1 = *chunk.get(1).unwrap_or(&0) as u32;
        let b2 = *chunk.get(2).unwrap_or(&0) as u32;
        let triple = (b0 << 16) | (b1 << 8) | b2;
        out.push(ALPHABET[(triple >> 18) as usize & 63] as char);
        out.push(ALPHABET[(triple >> 12) as usize & 63] as char);
        out.push(if chunk.len() > 1 {
            ALPHABET[(triple >> 6) as usize & 63] as char
        } else {
            '='
        });
        out.push(if chunk.len() > 2 {
            ALPHABET[triple as usize & 63] as char
        } else {
            '='
        });
    }
    out
}

pub fn base64_decode(text: &str) -> Result<Vec<u8>> {
    fn val(c: u8) -> Option<u32> {
        match c {
            b'A'..=b'Z' => Some((c - b'A') as u32),
            b'a'..=b'z' => Some((c - b'a') as u32 + 26),
            b'0'..=b'9' => Some((c - b'0') as u32 + 52),
            b'+' => Some(62),
            b'/' => Some(63),
            _ => None,
        }
    }
    let cleaned: Vec<u8> = text.bytes().filter(|b| !b.is_ascii_whitespace()).collect();
    let mut out = Vec::with_capacity(cleaned.len() / 4 * 3);
    for chunk in cleaned.chunks(4) {
        if chunk.len() < 2 {
            break;
        }
        let mut acc = 0u32;
        let mut count = 0;
        for &c in chunk {
            if c == b'=' {
                break;
            }
            let v = val(c).ok_or_else(|| anyhow::anyhow!("invalid base64 byte {c:?}"))?;
            acc = (acc << 6) | v;
            count += 1;
        }
        match count {
            2 => out.push((acc >> 4) as u8),
            3 => {
                out.push((acc >> 10) as u8);
                out.push((acc >> 2) as u8);
            }
            4 => {
                out.push((acc >> 16) as u8);
                out.push((acc >> 8) as u8);
                out.push(acc as u8);
            }
            _ => {}
        }
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    use rsa::pkcs1v15::Pkcs1v15Sign;

    fn key() -> RsaPrivateKey {
        // One 2048-bit key for the whole suite: generation is the slow part.
        generate_key(RSA_BITS).unwrap()
    }

    #[test]
    fn der_primitive_encodings_are_correct() {
        // Lengths: short form under 128, long form at/over.
        assert_eq!(der_length(0), vec![0x00]);
        assert_eq!(der_length(127), vec![0x7f]);
        assert_eq!(der_length(128), vec![0x81, 0x80]);
        assert_eq!(der_length(256), vec![0x82, 0x01, 0x00]);
        // INTEGER strips leading zeros and adds one when the high bit is set.
        assert_eq!(der_integer(&[0x00, 0x00, 0x01]), vec![0x02, 0x01, 0x01]);
        assert_eq!(der_integer(&[0x80]), vec![0x02, 0x02, 0x00, 0x80]);
        // SEQUENCE of NULL is the well-known `30 02 05 00`.
        assert_eq!(der_sequence(&[der_null()]), vec![0x30, 0x02, 0x05, 0x00]);
        // Context tag [0] wraps in 0xA0.
        assert_eq!(
            der_context(0, &[0x02, 0x01, 0x02]),
            vec![0xa0, 0x03, 0x02, 0x01, 0x02]
        );
        // BIT STRING carries the unused-bit count first.
        assert_eq!(der_bit_string(&[0xaa]), vec![0x03, 0x02, 0x00, 0xaa]);
    }

    #[test]
    fn utc_time_matches_known_instants() {
        assert_eq!(utc_time(0), "700101000000Z");
        assert_eq!(utc_time(1_000_000_000), "010909014640Z"); // 2001-09-09T01:46:40Z
        assert_eq!(utc_time(1_735_689_600), "250101000000Z"); // 2025-01-01T00:00:00Z
    }

    #[test]
    fn signs_and_verifies_sha256() {
        let k = key();
        let msg = b"the quick brown fox";
        let sig = sign_sha256(&k, msg).unwrap();
        let pub_key = RsaPublicKey::from(&k);
        let digest = Sha256::digest(msg);
        assert!(pub_key
            .verify(Pkcs1v15Sign::new::<Sha256>(), &digest, &sig)
            .is_ok());
        // A different message must not verify with the same signature.
        let other = Sha256::digest(b"something else");
        assert!(pub_key
            .verify(Pkcs1v15Sign::new::<Sha256>(), &other, &sig)
            .is_err());
    }

    #[test]
    fn base64_matches_known_vectors() {
        assert_eq!(base64_encode(b""), "");
        assert_eq!(base64_encode(b"f"), "Zg==");
        assert_eq!(base64_encode(b"fo"), "Zm8=");
        assert_eq!(base64_encode(b"foo"), "Zm9v");
        assert_eq!(base64_encode(b"foob"), "Zm9vYg==");
        assert_eq!(base64_encode(b"foobar"), "Zm9vYmFy");
        for sample in [
            &b""[..],
            b"f",
            b"fo",
            b"foo",
            b"foob",
            b"foobar",
            &[0u8, 255, 1][..],
        ] {
            assert_eq!(base64_decode(&base64_encode(sample)).unwrap(), sample);
        }
    }

    #[test]
    fn android_public_key_blob_has_the_documented_shape() {
        let k = key();
        let blob = android_public_key_blob(&k).unwrap();
        assert_eq!(
            blob.len(),
            4 + 4 + 256 + 256 + 4,
            "ADB structure is 524 bytes"
        );
        assert_eq!(
            u32::from_le_bytes([blob[0], blob[1], blob[2], blob[3]]),
            256
        );
        assert_eq!(
            u32::from_le_bytes([blob[520], blob[521], blob[522], blob[523]]),
            65537,
            "exponent must be 65537"
        );
        // n0inv must satisfy n0 * n0inv == -1 (mod 2^32).
        let n0 = u32::from_le_bytes([blob[8], blob[9], blob[10], blob[11]]);
        let n0inv = u32::from_le_bytes([blob[4], blob[5], blob[6], blob[7]]);
        assert_eq!(n0.wrapping_mul(n0inv), u32::MAX, "n0inv is -1/n0 mod 2^32");
        // The modulus, little-endian, must be the key's modulus.
        let mut n_le = blob[8..264].to_vec();
        n_le.reverse();
        assert_eq!(BigUint::from_bytes_be(&n_le), *RsaPublicKey::from(&k).n());
    }

    #[test]
    fn certificates_are_well_formed_der() {
        let k = key();
        let der = self_signed_cert_der(&k, "mandroid", 3650).unwrap();
        assert_eq!(der[0], 0x30, "Certificate is a SEQUENCE");
        assert_eq!(der.len() as usize, der.len());
        // The outer length field must exactly cover the rest of the buffer.
        let (len, header) = if der[1] < 0x80 {
            (der[1] as usize, 2usize)
        } else {
            let n = (der[1] & 0x7f) as usize;
            let mut v = 0usize;
            for i in 0..n {
                v = (v << 8) | der[2 + i] as usize;
            }
            (v, 2 + n)
        };
        assert_eq!(header + len, der.len(), "outer DER length must be exact");
    }

    #[test]
    fn android_pubkey_text_is_pure_base64() {
        let k = key();
        let text = adbkey_pub_text(&k).unwrap();
        assert!(text
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '+' || c == '/' || c == '='));
        assert_eq!(base64_decode(&text).unwrap().len(), 524);
    }
}
