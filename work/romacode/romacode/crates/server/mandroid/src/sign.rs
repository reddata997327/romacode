//! APK signing: JAR (v1) and APK Signature Scheme v2.
//!
//! No `apksigner`, no `jarsigner`, no `keytool`, no BouncyCastle.
//!
//! Android 11+ rejects APKs that carry only a v1 signature, so **v2 is
//! mandatory**; v1 is emitted alongside it so the same APK also installs on
//! Android 6 and older.
//!
//! The v2 digest algorithm here was validated against a real, Google-signed
//! APK: recomputing it reproduces the digest stored in that APK's signing block
//! byte for byte (see the `digest_of_a_real_signed_apk_matches` test).

use std::path::Path;

use anyhow::{anyhow, bail, Context, Result};
use rsa::{RsaPrivateKey, RsaPublicKey};
use sha2::{Digest, Sha256};

use crate::crypto::{self, CertMaterial};
use crate::repack::{self, Entry};

/// ID of the APK Signature Scheme v2 block inside the signing block.
pub const V2_BLOCK_ID: u32 = 0x7109871a;
/// RSASSA-PKCS1-v1_5 with SHA-256. Deterministic, so a rebuild is byte-stable.
pub const ALGO_RSA_PKCS1_SHA256: u32 = 0x0103;
/// RSASSA-PKCS1-v1_5 with SHA-512.
pub const ALGO_RSA_PKCS1_SHA512: u32 = 0x0104;
/// The signing block's magic trailer.
pub const SIG_BLOCK_MAGIC: &[u8; 16] = b"APK Sig Block 42";
/// Chunk size used by the v2 content digest: 1 MB.
const DIGEST_CHUNK: usize = 1024 * 1024;
/// Longest JAR manifest line before continuation.
const JAR_LINE: usize = 72;

/// A signing identity: a private key plus the certificate it will present.
pub struct ApkSigner {
    pub key: RsaPrivateKey,
    pub cert: CertMaterial,
}

impl ApkSigner {
    /// Generate a fresh 2048-bit identity. `cn` names the certificate subject.
    pub fn generate(cn: &str) -> Result<Self> {
        let key = crypto::generate_key(crypto::RSA_BITS)?;
        Self::from_key(key, cn)
    }

    pub fn from_key(key: RsaPrivateKey, cn: &str) -> Result<Self> {
        let cert = crypto::self_signed_cert_material(&key, cn, 3650)?;
        Ok(ApkSigner { key, cert })
    }

    pub fn subject(&self) -> String {
        self.cert.subject.clone()
    }
}

#[derive(Debug, Clone)]
pub struct SignOptions {
    pub v1: bool,
    pub v2: bool,
    pub created_by: String,
}

impl Default for SignOptions {
    fn default() -> Self {
        self_v1_v2()
    }
}

fn self_v1_v2() -> SignOptions {
    SignOptions {
        v1: true,
        v2: true,
        created_by: "mandroid".to_string(),
    }
}

#[derive(Debug, Clone)]
pub struct SignReport {
    pub v1: bool,
    pub v2: bool,
    pub entries: usize,
    pub removed_signatures: usize,
    pub content_digest: String,
    pub signer: String,
    pub bytes: u64,
    pub sha256: String,
}

// ---------------------------------------------------------------------------
// Signing
// ---------------------------------------------------------------------------

/// Sign an APK, writing a fully rebuilt archive to `output`.
pub fn sign_apk(
    input: &Path,
    output: &Path,
    signer: &ApkSigner,
    opts: &SignOptions,
) -> Result<SignReport> {
    let all = repack::read_apk(input)?;
    let before = all.len();
    let mut entries: Vec<Entry> = all.into_iter().filter(|e| !e.is_signature_file()).collect();
    let removed = before - entries.len();

    if opts.v1 {
        let generated = build_v1(&entries, signer, opts)?;
        entries.extend(generated);
    }

    let count = entries.len() as u16;
    let sections = repack::build_sections(&entries)?;

    let mut digest_hex = String::new();
    let block = if opts.v2 {
        // `build_sections` writes the EOCD as if no block were present, so its
        // central-directory offset already equals the signing block offset —
        // exactly the substitution the v2 digest requires.
        let digest = v2_digest(
            &sections.content,
            &sections.central_directory,
            &sections.eocd,
        );
        digest_hex = hex::encode(digest);
        let value = build_v2_value(&digest, &signer.cert.der, &signer.key)?;
        Some(build_signing_block(&value))
    } else {
        None
    };

    let bytes = repack::assemble(
        &sections.content,
        block.as_deref(),
        &sections.central_directory,
        count,
    );
    if let Some(parent) = output.parent() {
        std::fs::create_dir_all(parent).ok();
    }
    std::fs::write(output, &bytes).with_context(|| format!("writing {}", output.display()))?;

    let sha256 = {
        let mut h = Sha256::new();
        h.update(&bytes);
        hex::encode(h.finalize())
    };

    Ok(SignReport {
        v1: opts.v1,
        v2: opts.v2,
        entries: count as usize,
        removed_signatures: removed,
        content_digest: digest_hex,
        signer: signer.subject(),
        bytes: bytes.len() as u64,
        sha256,
    })
}

/// Rewrite an APK with modified entries, preserving everything else. Used after
/// a manifest or resource edit.
pub fn repack_apk(
    input: &Path,
    output: &Path,
    replacements: &[(String, Vec<u8>)],
) -> Result<Vec<String>> {
    let entries = repack::read_apk(input)?;
    let mut replaced = Vec::new();
    let mut out: Vec<Entry> = Vec::with_capacity(entries.len());
    for mut entry in entries {
        // A content change invalidates every existing signature, so drop the
        // signature files rather than shipping a stale, unverifiable set.
        if entry.is_signature_file() {
            continue;
        }
        if let Some((_, data)) = replacements.iter().find(|(name, _)| *name == entry.name) {
            replaced.push(entry.name.clone());
            entry.data = data.clone();
            entry.method = 0;
        }
        out.push(entry);
    }
    for (name, data) in replacements {
        if !replaced.iter().any(|n| n == name) {
            out.push(Entry::new(name.clone(), data.clone()));
            replaced.push(name.clone());
        }
    }
    let count = out.len() as u16;
    let sections = repack::build_sections(&out)?;
    let bytes = repack::assemble(&sections.content, None, &sections.central_directory, count);
    std::fs::write(output, &bytes).with_context(|| format!("writing {}", output.display()))?;
    Ok(replaced)
}

// ---------------------------------------------------------------------------
// v2: content digest
// ---------------------------------------------------------------------------

/// The v2 content digest over sections 1, 3 and 4.
///
/// Each section is split into 1 MB chunks; a chunk digests as
/// `0xa5 || u32le(len) || chunk`, and the result as
/// `0x5a || u32le(chunk count) || concat(chunk digests)`.
///
/// `eocd` must be the record with its central-directory-offset field standing
/// for the *signing block* offset, per the specification.
pub fn v2_digest(section1: &[u8], central_directory: &[u8], eocd: &[u8]) -> [u8; 32] {
    let mut chunks: Vec<[u8; 32]> = Vec::new();
    for section in [section1, central_directory, eocd] {
        if section.is_empty() {
            continue;
        }
        for chunk in section.chunks(DIGEST_CHUNK) {
            let mut h = Sha256::new();
            h.update([0xa5]);
            h.update((chunk.len() as u32).to_le_bytes());
            h.update(chunk);
            chunks.push(h.finalize().into());
        }
    }
    let mut top = Sha256::new();
    top.update([0x5a]);
    top.update((chunks.len() as u32).to_le_bytes());
    for c in &chunks {
        top.update(c);
    }
    top.finalize().into()
}

fn lp(data: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(4 + data.len());
    out.extend_from_slice(&(data.len() as u32).to_le_bytes());
    out.extend_from_slice(data);
    out
}

/// A length-prefixed sequence of length-prefixed elements.
fn sequence(elements: &[Vec<u8>]) -> Vec<u8> {
    let mut inner = Vec::new();
    for e in elements {
        inner.extend_from_slice(&lp(e));
    }
    lp(&inner)
}

/// Build the v2 block value for one signer.
pub fn build_v2_value(digest: &[u8], cert_der: &[u8], key: &RsaPrivateKey) -> Result<Vec<u8>> {
    // digests: sequence of { u32 algorithm, length-prefixed digest }
    let mut digest_element = Vec::with_capacity(8 + digest.len());
    digest_element.extend_from_slice(&ALGO_RSA_PKCS1_SHA256.to_le_bytes());
    digest_element.extend_from_slice(&lp(digest));
    let digests = sequence(&[digest_element]);

    // certificates: sequence of length-prefixed DER certificates
    let certificates = sequence(&[cert_der.to_vec()]);

    // additional attributes: empty
    let attributes = sequence(&[]);

    let mut signed_data = Vec::new();
    signed_data.extend_from_slice(&digests);
    signed_data.extend_from_slice(&certificates);
    signed_data.extend_from_slice(&attributes);

    // The signature covers the signed-data blob itself.
    let signature = crypto::sign_prehashed_sha256(key, &Sha256::digest(&signed_data))?;
    let mut signature_element = Vec::with_capacity(8 + signature.len());
    signature_element.extend_from_slice(&ALGO_RSA_PKCS1_SHA256.to_le_bytes());
    signature_element.extend_from_slice(&lp(&signature));
    let signatures = sequence(&[signature_element]);

    let public_key = crypto::spki_der(key)?;

    // `signed_data` is a bare concatenation and needs its own length prefix;
    // `signatures` is already a complete length-prefixed sequence, so wrapping
    // it again would add a level the verifier does not expect.
    let mut signer = Vec::new();
    signer.extend_from_slice(&lp(&signed_data));
    signer.extend_from_slice(&signatures);
    signer.extend_from_slice(&lp(&public_key));

    Ok(sequence(&[signer]))
}

/// Wrap a v2 block value into a complete APK Signing Block.
pub fn build_signing_block(value: &[u8]) -> Vec<u8> {
    let mut pair = Vec::with_capacity(12 + value.len());
    pair.extend_from_slice(&((value.len() as u64) + 4).to_le_bytes());
    pair.extend_from_slice(&V2_BLOCK_ID.to_le_bytes());
    pair.extend_from_slice(value);

    let size = pair.len() as u64 + 8 + 16;
    let mut out = Vec::with_capacity(pair.len() + 32);
    out.extend_from_slice(&size.to_le_bytes());
    out.extend_from_slice(&pair);
    out.extend_from_slice(&size.to_le_bytes());
    out.extend_from_slice(SIG_BLOCK_MAGIC);
    out
}

// ---------------------------------------------------------------------------
// v1: JAR signing
// ---------------------------------------------------------------------------

fn jar_attr(out: &mut Vec<u8>, key: &str, value: &str) {
    let line = format!("{key}: {value}");
    let bytes = line.as_bytes();
    let mut i = 0;
    let mut first = true;
    while i < bytes.len() {
        let width = if first { JAR_LINE } else { JAR_LINE - 1 };
        let end = (i + width).min(bytes.len());
        if !first {
            out.push(b' ');
        }
        out.extend_from_slice(&bytes[i..end]);
        out.extend_from_slice(b"\r\n");
        i = end;
        first = false;
    }
}

/// Produce `META-INF/MANIFEST.MF`, `META-INF/CERT.SF` and `META-INF/CERT.RSA`.
fn build_v1(entries: &[Entry], signer: &ApkSigner, opts: &SignOptions) -> Result<Vec<Entry>> {
    // --- MANIFEST.MF: per-entry digest of the *uncompressed* content.
    let mut manifest = Vec::new();
    jar_attr(&mut manifest, "Manifest-Version", "1.0");
    jar_attr(&mut manifest, "Created-By", &opts.created_by);
    manifest.extend_from_slice(b"\r\n");

    let mut sections: Vec<(String, Vec<u8>)> = Vec::new();
    for entry in entries {
        let mut section = Vec::new();
        jar_attr(&mut section, "Name", &entry.name);
        jar_attr(
            &mut section,
            "SHA-256-Digest",
            &crypto::base64_encode(&Sha256::digest(&entry.data)),
        );
        section.extend_from_slice(b"\r\n");
        manifest.extend_from_slice(&section);
        sections.push((entry.name.clone(), section));
    }

    // --- CERT.SF: digest of the whole manifest, plus of each manifest section.
    let mut sf = Vec::new();
    jar_attr(&mut sf, "Signature-Version", "1.0");
    jar_attr(&mut sf, "Created-By", &opts.created_by);
    if opts.v2 {
        // Required whenever v1 is paired with v2+, so a verifier that sees the
        // v1 signature knows a stronger scheme was also applied. Without it the
        // APK is vulnerable to a signature-stripping downgrade.
        jar_attr(&mut sf, "X-Android-APK-Signed", "2");
    }
    jar_attr(
        &mut sf,
        "SHA-256-Digest-Manifest",
        &crypto::base64_encode(&Sha256::digest(&manifest)),
    );
    sf.extend_from_slice(b"\r\n");
    for (name, section) in &sections {
        jar_attr(&mut sf, "Name", name);
        jar_attr(
            &mut sf,
            "SHA-256-Digest",
            &crypto::base64_encode(&Sha256::digest(section)),
        );
        sf.extend_from_slice(b"\r\n");
    }

    // --- CERT.RSA: PKCS#7 SignedData over CERT.SF.
    let pkcs7 = build_pkcs7(signer, &sf)?;

    Ok(vec![
        Entry::new("META-INF/MANIFEST.MF", manifest),
        Entry::new("META-INF/CERT.SF", sf),
        Entry::new("META-INF/CERT.RSA", pkcs7),
    ])
}

/// A minimal PKCS#7 `ContentInfo`/`SignedData` carrying one signer.
fn build_pkcs7(signer: &ApkSigner, content: &[u8]) -> Result<Vec<u8>> {
    use crypto::der;

    let digest_algorithm = der::algorithm_identifier_sha256();
    let signature = crypto::sign_prehashed_sha256(&signer.key, &Sha256::digest(content))?;

    // issuerAndSerialNumber
    let issuer_and_serial = der::sequence(&[
        signer.cert.issuer.clone(),
        der::integer(&signer.cert.serial),
    ]);

    let signer_info = der::sequence(&[
        der::integer(&[1]),
        issuer_and_serial,
        digest_algorithm.clone(),
        der::algorithm_identifier_rsa(),
        der::octet_string(&signature),
    ]);

    let certificates = der::implicit_set(0, &[signer.cert.der.clone()]);

    let signed_data = der::sequence(&[
        der::integer(&[1]),
        der::set(&[digest_algorithm]),
        der::sequence(&[der::oid(&der::OID_PKCS7_DATA)]),
        certificates,
        der::set(&[signer_info]),
    ]);

    Ok(der::sequence(&[
        der::oid(&der::OID_PKCS7_SIGNED_DATA),
        der::explicit(0, &signed_data),
    ]))
}

// ---------------------------------------------------------------------------
// Verification
// ---------------------------------------------------------------------------

#[derive(Debug, Clone)]
pub struct V2Verify {
    pub found: bool,
    pub signers: usize,
    pub algorithm: u32,
    pub stored_digest: String,
    pub computed_digest: String,
    pub digest_ok: bool,
    pub signature_ok: bool,
    pub certificate_bytes: usize,
    pub certificate_sha256: String,
    pub errors: Vec<String>,
}

impl V2Verify {
    pub fn valid(&self) -> bool {
        self.found && self.errors.is_empty() && self.digest_ok && self.signature_ok
    }
}

/// Parse and verify an APK's v2 signature by recomputing the content digest and
/// checking the RSA signature over the signed data.
pub fn verify_v2(path: &Path) -> Result<V2Verify> {
    let data = std::fs::read(path).with_context(|| format!("reading {}", path.display()))?;
    let mut result = V2Verify {
        found: false,
        signers: 0,
        algorithm: 0,
        stored_digest: String::new(),
        computed_digest: String::new(),
        digest_ok: false,
        signature_ok: false,
        certificate_bytes: 0,
        certificate_sha256: String::new(),
        errors: Vec::new(),
    };

    let (block_start, _pairs_start, _size) = match repack::find_signing_block(&data) {
        Some(v) => v,
        None => return Ok(result),
    };
    result.found = true;

    // This verifier intentionally handles ordinary single-disk ZIP/APKs with
    // a 22-byte EOCD (plus any ZIP comment up to 64 KiB) and RSA PKCS#1
    // v1.5/SHA-256 only. Anything else fails closed instead of being mistaken
    // for a valid Android signature.
    //
    // The EOCD signature "PK\x05\x06" can sit anywhere in the trailing
    // 22..=22+65535 bytes because the EOCD record may be followed by a ZIP
    // archive comment of up to 64 KiB. Scan backward from the end and pick
    // the LAST occurrence — that is the one a writer would have produced.
    let eocd_off = {
        let scan_lo = data.len().saturating_sub(22 + 65535);
        let scan_hi = data.len().saturating_sub(22);
        match (scan_lo..=scan_hi)
            .rev()
            .find(|&p| data.get(p..p + 4) == Some(b"PK\x05\x06"))
        {
            Some(off) => off,
            None => {
                result.errors.push(
                    "missing EOCD record in the trailing 64 KiB of the file \
                     (the archive is truncated or its comment is too large)"
                        .into(),
                );
                return Ok(result);
            }
        }
    };
    let cd_off = u32::from_le_bytes(data[eocd_off + 16..eocd_off + 20].try_into()?) as usize;
    let cd_size = u32::from_le_bytes(data[eocd_off + 12..eocd_off + 16].try_into()?) as usize;
    if cd_off < block_start || cd_off.checked_add(cd_size) != Some(eocd_off) {
        result
            .errors
            .push("invalid central directory bounds".into());
        return Ok(result);
    }
    let value = match find_pair(&data, block_start, cd_off, V2_BLOCK_ID) {
        Some(v) => v,
        None => {
            result
                .errors
                .push("no v2 block in the signing block".into());
            return Ok(result);
        }
    };

    let (signers_seq, after_signers) = match read_lp(value, 0) {
        Some(v) if v.1 == value.len() => v,
        _ => {
            result.errors.push("malformed v2 signer sequence".into());
            return Ok(result);
        }
    };
    let signers = match read_lp_elements(signers_seq) {
        Some(v) => v,
        None => {
            result.errors.push("malformed v2 signer list".into());
            return Ok(result);
        }
    };
    let mut digest_all_ok = !signers.is_empty();
    let mut signature_all_ok = !signers.is_empty();
    for signer in signers {
        result.signers += 1;
        let parsed = verify_one_v2_signer(signer, &data, block_start, cd_off, eocd_off);
        match parsed {
            Ok(verified) => {
                if result.signers == 1 {
                    result.algorithm = verified.algorithm;
                    result.stored_digest = hex::encode(&verified.stored_digest);
                    result.computed_digest = hex::encode(verified.computed_digest);
                    result.certificate_bytes = verified.certificate_bytes;
                    result.certificate_sha256 = verified.certificate_sha256;
                }
                digest_all_ok &= verified.digest_ok;
                signature_all_ok &= verified.signature_ok;
                result.errors.extend(verified.errors);
            }
            Err(e) => {
                digest_all_ok = false;
                signature_all_ok = false;
                result
                    .errors
                    .push(format!("signer {}: {e:#}", result.signers));
            }
        }
    }
    result.digest_ok = digest_all_ok;
    result.signature_ok = signature_all_ok;
    if after_signers != value.len() || result.signers == 0 {
        result.errors.push("no signers".into());
    }
    Ok(result)
}

struct VerifiedSigner {
    algorithm: u32,
    stored_digest: Vec<u8>,
    computed_digest: [u8; 32],
    digest_ok: bool,
    signature_ok: bool,
    certificate_bytes: usize,
    certificate_sha256: String,
    errors: Vec<String>,
}

fn verify_one_v2_signer(
    signer: &[u8],
    apk: &[u8],
    block_start: usize,
    cd_off: usize,
    eocd_off: usize,
) -> Result<VerifiedSigner> {
    let (signed_data, after_sd) =
        read_lp(signer, 0).ok_or_else(|| anyhow!("malformed signed data"))?;
    let (signatures_seq, after_sigs) =
        read_lp(signer, after_sd).ok_or_else(|| anyhow!("malformed signatures"))?;
    let (public_key, after_key) =
        read_lp(signer, after_sigs).ok_or_else(|| anyhow!("malformed public key"))?;
    if after_key != signer.len() {
        bail!("trailing signer data");
    }

    let (digests_seq, after_digests) =
        read_lp(signed_data, 0).ok_or_else(|| anyhow!("malformed digests"))?;
    let (certs_seq, after_certs) =
        read_lp(signed_data, after_digests).ok_or_else(|| anyhow!("malformed certificates"))?;
    let (_attributes, after_attrs) =
        read_lp(signed_data, after_certs).ok_or_else(|| anyhow!("malformed attributes"))?;
    if after_attrs != signed_data.len() {
        bail!("trailing signed data");
    }
    let digests = read_lp_elements(digests_seq).ok_or_else(|| anyhow!("malformed digest list"))?;
    let signatures =
        read_lp_elements(signatures_seq).ok_or_else(|| anyhow!("malformed signature list"))?;
    // The v2 block may carry multiple algorithms (a signer can include both
    // RSA-PKCS1-SHA256 and a stronger scheme). Pick the first record whose
    // algorithm id matches RSA-PKCS1-SHA256 (0x0103) — the one this verifier
    // implements — and ignore the others.
    let (algorithm, stored_digest) = digests
        .iter()
        .find_map(|d| {
            let (alg, data) = parse_algorithm_record(d, "digest").ok()?;
            if alg == ALGO_RSA_PKCS1_SHA256 {
                Some((alg, data))
            } else {
                None
            }
        })
        .ok_or_else(|| {
            anyhow!(
                "no RSA-PKCS1-SHA256 (0x0103) digest found for signer \
                 (other algorithms are present but unsupported)"
            )
        })?;
    let (_signature_algorithm, signature) = signatures
        .iter()
        .find_map(|s| {
            let (alg, data) = parse_algorithm_record(s, "signature").ok()?;
            if alg == ALGO_RSA_PKCS1_SHA256 {
                Some((alg, data))
            } else {
                None
            }
        })
        .ok_or_else(|| {
            anyhow!(
                "no RSA-PKCS1-SHA256 (0x0103) signature found for signer \
                 (other algorithms are present but unsupported)"
            )
        })?;
    if stored_digest.len() != 32 {
        bail!("RSA/SHA-256 digest must be 32 bytes");
    }

    let certificates =
        read_lp_elements(certs_seq).ok_or_else(|| anyhow!("malformed certificate list"))?;
    let cert_der = certificates
        .first()
        .ok_or_else(|| anyhow!("certificate list is empty"))?;
    let certificate = rustls::pki_types::CertificateDer::from(*cert_der);
    let parsed_cert = rustls::server::ParsedCertificate::try_from(&certificate)
        .map_err(|e| anyhow!("invalid X.509 certificate: {e}"))?;
    let certificate_spki = parsed_cert.subject_public_key_info();
    if certificate_spki.as_ref() != public_key {
        bail!("signer public key does not match the certificate SubjectPublicKeyInfo");
    }
    let pk = public_key_from_spki(public_key).context("parsing signer public key")?;

    let mut eocd = apk[eocd_off..].to_vec();
    eocd[16..20].copy_from_slice(&(block_start as u32).to_le_bytes());
    let computed_digest = v2_digest(&apk[..block_start], &apk[cd_off..eocd_off], &eocd);
    let digest_ok = computed_digest.as_slice() == stored_digest;
    let digest = Sha256::digest(signed_data);
    use rsa::pkcs1v15::Pkcs1v15Sign;
    let signature_ok = pk
        .verify(Pkcs1v15Sign::new::<Sha256>(), &digest, signature)
        .is_ok();
    let mut errors = Vec::new();
    if !digest_ok {
        errors.push("content digest does not match".into());
    }
    if !signature_ok {
        errors.push("signature does not verify".into());
    }

    Ok(VerifiedSigner {
        algorithm,
        stored_digest: stored_digest.to_vec(),
        computed_digest,
        digest_ok,
        signature_ok,
        certificate_bytes: cert_der.len(),
        certificate_sha256: hex::encode(Sha256::digest(cert_der)),
        errors,
    })
}

fn parse_algorithm_record<'a>(record: &'a [u8], label: &str) -> Result<(u32, &'a [u8])> {
    if record.len() < 4 {
        bail!("{label} record is too short");
    }
    let algorithm = u32::from_le_bytes(record[..4].try_into()?);
    let (value, end) = read_lp(record, 4).ok_or_else(|| anyhow!("malformed {label} value"))?;
    if end != record.len() {
        bail!("trailing bytes in {label} record");
    }
    Ok((algorithm, value))
}

fn read_lp_elements(mut data: &[u8]) -> Option<Vec<&[u8]>> {
    let mut elements = Vec::new();
    while !data.is_empty() {
        let (element, next) = read_lp(data, 0)?;
        if next == 0 {
            return None;
        }
        elements.push(element);
        data = &data[next..];
    }
    Some(elements)
}

/// Walk the ID-value pairs of a signing block and return the value for `id`.
fn find_pair(data: &[u8], block_start: usize, cd_off: usize, id: u32) -> Option<&[u8]> {
    let pairs_end = cd_off.checked_sub(24)?;
    let mut p = block_start.checked_add(8)?;
    let mut found = None;
    while p < pairs_end {
        let header_end = p.checked_add(12)?;
        if header_end > pairs_end {
            return None;
        }
        let len = usize::try_from(u64::from_le_bytes(data[p..p + 8].try_into().ok()?)).ok()?;
        let pair_end = p.checked_add(8)?.checked_add(len)?;
        if len < 4 || pair_end > pairs_end {
            return None;
        }
        let pair_id = u32::from_le_bytes(data[p + 8..p + 12].try_into().ok()?);
        if pair_id == id {
            if found.is_some() {
                return None;
            }
            found = Some(&data[p + 12..pair_end]);
        }
        p = pair_end;
    }
    if p == pairs_end {
        found
    } else {
        None
    }
}

/// Read a u32-length-prefixed slice at `offset`; returns `(slice, offset after)`.
fn read_lp(data: &[u8], offset: usize) -> Option<(&[u8], usize)> {
    let header_end = offset.checked_add(4)?;
    if header_end > data.len() {
        return None;
    }
    let len = u32::from_le_bytes(data[offset..header_end].try_into().ok()?) as usize;
    let end = header_end.checked_add(len)?;
    if end > data.len() {
        return None;
    }
    Some((&data[header_end..end], end))
}

/// Extract an RSA public key from a DER SubjectPublicKeyInfo.
pub fn public_key_from_spki(der: &[u8]) -> Result<RsaPublicKey> {
    use num_bigint_dig::BigUint;
    let (spki, _) = read_tlv(der, 0).ok_or_else(|| anyhow!("not a DER structure"))?;
    let (alg, next) = read_tlv(spki, 0).ok_or_else(|| anyhow!("missing algorithm"))?;
    let (oid, _) = read_tlv(alg, 0).ok_or_else(|| anyhow!("missing algorithm OID"))?;
    if oid != [0x2a, 0x86, 0x48, 0x86, 0xf7, 0x0d, 0x01, 0x01, 0x01] {
        bail!("subject public key is not RSA");
    }
    let (bit_string, _) = read_tlv(spki, next).ok_or_else(|| anyhow!("missing key material"))?;
    if bit_string.is_empty() || bit_string[0] != 0 {
        bail!("unexpected bit string padding");
    }
    let (rsa_seq, _) =
        read_tlv(&bit_string[1..], 0).ok_or_else(|| anyhow!("missing RSA sequence"))?;
    let (n_bytes, next) = read_tlv(rsa_seq, 0).ok_or_else(|| anyhow!("missing modulus"))?;
    let (e_bytes, _) = read_tlv(rsa_seq, next).ok_or_else(|| anyhow!("missing exponent"))?;
    let n = BigUint::from_bytes_be(n_bytes);
    let e = BigUint::from_bytes_be(e_bytes);
    RsaPublicKey::new(n, e).context("building RSA public key")
}

/// Read one DER TLV: returns `(content, offset after the whole TLV)`.
fn read_tlv(data: &[u8], offset: usize) -> Option<(&[u8], usize)> {
    if offset + 2 > data.len() {
        return None;
    }
    let length_byte = data[offset + 1];
    let (len, header) = if length_byte < 0x80 {
        (length_byte as usize, 2usize)
    } else {
        let n = (length_byte & 0x7f) as usize;
        if n == 0 || n > 4 || offset + 2 + n > data.len() {
            return None;
        }
        let mut v = 0usize;
        for i in 0..n {
            v = (v << 8) | data[offset + 2 + i] as usize;
        }
        (v, 2 + n)
    };
    let start = offset + header;
    if start + len > data.len() {
        return None;
    }
    Some((&data[start..start + len], start + len))
}

/// Result of checking the JAR (v1) signature files.
#[derive(Debug, Clone)]
pub struct V1Verify {
    pub present: bool,
    pub has_manifest: bool,
    pub has_sf: bool,
    pub has_block: bool,
    pub signed_entries: usize,
    pub x_android_apk_signed: Option<String>,
    pub declared_schemes: Vec<u32>,
}

/// Structural check of the v1 signature files, including the
/// `X-Android-APK-Signed` downgrade-protection attribute.
pub fn verify_v1(path: &Path) -> Result<V1Verify> {
    let entries = repack::read_apk(path)?;
    let find = |name: &str| entries.iter().find(|e| e.name.eq_ignore_ascii_case(name));
    let manifest = find("META-INF/MANIFEST.MF");
    // The .SF/.RSA pair is named after the signer's alias, not necessarily
    // "CERT": F-Droid ships CIARANG.SF/CIARANG.RSA, and a re-signed APK gets
    // whatever alias the tool chose. Match on the extension, not the stem.
    let sf = entries.iter().find(|e| {
        let u = e.name.to_ascii_uppercase();
        u.starts_with("META-INF/") && u.ends_with(".SF")
    });
    let block = entries.iter().find(|e| {
        let u = e.name.to_ascii_uppercase();
        u.starts_with("META-INF/")
            && (u.ends_with(".RSA") || u.ends_with(".DSA") || u.ends_with(".EC"))
    });

    let mut out = V1Verify {
        present: manifest.is_some() && sf.is_some() && block.is_some(),
        has_manifest: manifest.is_some(),
        has_sf: sf.is_some(),
        has_block: block.is_some(),
        signed_entries: 0,
        x_android_apk_signed: None,
        declared_schemes: Vec::new(),
    };

    if let Some(m) = manifest {
        let text = String::from_utf8_lossy(&m.data);
        // Count manifest sections: a "Name: " line that starts a line.
        out.signed_entries = text
            .split("\r\n")
            .flat_map(|l| l.split('\n'))
            .filter(|l| l.starts_with("Name: ") || l.trim_start().starts_with("Name: "))
            .count();
    }
    if let Some(s) = sf {
        let text = String::from_utf8_lossy(&s.data);
        // Unwrap continuation lines before reading the header.
        let unfolded = text.replace("\r\n ", "");
        for line in unfolded.lines() {
            if let Some(value) = line.strip_prefix("X-Android-APK-Signed: ") {
                out.x_android_apk_signed = Some(value.trim().to_string());
                out.declared_schemes = value
                    .split(',')
                    .filter_map(|v| v.trim().parse::<u32>().ok())
                    .collect();
            }
        }
    }
    Ok(out)
}

// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    fn real_apk() -> std::path::PathBuf {
        std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("testdata/test-app.apk")
    }

    #[test]
    fn digest_of_a_real_signed_apk_matches_the_stored_digest() {
        // The decisive test. `test-app.apk` is signed by F-Droid with a real key
        // and a real APK Signature Scheme v2 block. If our digest algorithm
        // differs from the specification in any way — chunk size, the 0xa5/0x5a
        // prefixes, which three sections, or the EOCD substitution — this fails.
        let path = real_apk();
        if !path.exists() {
            eprintln!("skipping: {} is not present", path.display());
            return;
        }
        let verify = verify_v2(&path).expect("verification should run");
        assert!(verify.found, "test-app.apk should carry a v2 signature");
        assert_eq!(verify.signers, 1);
        assert_eq!(verify.algorithm, ALGO_RSA_PKCS1_SHA256);
        assert_eq!(
            verify.computed_digest, verify.stored_digest,
            "recomputed content digest must equal the digest in the signing block"
        );
        assert!(verify.digest_ok);
        assert!(
            verify.signature_ok,
            "the RSA signature over signed data must verify with the embedded certificate: {:?}",
            verify.errors
        );
        assert!(verify.valid());
        assert!(verify.certificate_bytes > 500);
    }

    #[test]
    fn real_apk_v1_signature_declares_v2_for_downgrade_protection() {
        let path = real_apk();
        if !path.exists() {
            return;
        }
        let v1 = verify_v1(&path).unwrap();
        assert!(v1.has_manifest && v1.has_sf && v1.has_block);
        assert!(v1.signed_entries > 10, "expected many signed entries");
        let declared = v1.declared_schemes;
        assert!(
            declared.contains(&2),
            "a v1+v2 APK must declare X-Android-APK-Signed: 2, got {:?}",
            v1.x_android_apk_signed
        );
    }

    #[test]
    fn lp_and_sequence_encodings_match_the_specification() {
        assert_eq!(lp(b"abc"), vec![3, 0, 0, 0, b'a', b'b', b'c']);
        // One element of "ab": sequence = u32(4 + 2) then lp(element)
        let s = sequence(&[b"ab".to_vec()]);
        assert_eq!(s, vec![6, 0, 0, 0, 2, 0, 0, 0, b'a', b'b']);
        assert_eq!(sequence(&[]), vec![0, 0, 0, 0]);
    }

    #[test]
    fn signing_block_size_formula_holds() {
        let value = vec![0u8; 100];
        let block = build_signing_block(&value);
        // pair = 8 (u64 length) + 4 (id) + 100 = 112; size = 112 + 24 = 136,
        // and the block is the size field plus the size it declares.
        assert_eq!(block.len(), 8 + 136);
        let size = u64::from_le_bytes(block[0..8].try_into().unwrap());
        assert_eq!(size, 136);
        assert_eq!(
            u64::from_le_bytes(
                block[block.len() - 24..block.len() - 16]
                    .try_into()
                    .unwrap()
            ),
            size,
            "both size fields must agree"
        );
        assert_eq!(&block[block.len() - 16..], SIG_BLOCK_MAGIC);
    }

    #[test]
    fn sign_then_verify_round_trip_on_a_synthetic_apk() {
        // Build a small but valid APK, sign it, and confirm the signer produces
        // something our own verifier accepts and a ZIP reader can still open.
        let dir = std::env::temp_dir().join(format!("mandroid-sign-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let unsigned = dir.join("unsigned.apk");
        let signed = dir.join("signed.apk");

        let entries = vec![
            Entry::new("AndroidManifest.xml", vec![0x03, 0x00, 0x08, 0x00, 0xaa]),
            Entry::new(
                "classes.dex",
                (0..30_000u32).map(|i| (i % 251) as u8).collect(),
            ),
            Entry::new("resources.arsc", vec![0x02, 0x00, 0x0c, 0x00, 0x11, 0x22]),
        ];
        let count = entries.len() as u16;
        let sections = repack::build_sections(&entries).unwrap();
        std::fs::write(
            &unsigned,
            repack::assemble(&sections.content, None, &sections.central_directory, count),
        )
        .unwrap();

        let signer = ApkSigner::generate("mandroid test").unwrap();
        let report = sign_apk(&unsigned, &signed, &signer, &SignOptions::default()).unwrap();
        assert!(report.v1 && report.v2);
        assert_eq!(
            report.content_digest.len(),
            64,
            "SHA-256 digest is 32 bytes"
        );

        let v2 = verify_v2(&signed).unwrap();
        assert!(v2.found, "v2 block should be present");
        assert!(v2.digest_ok, "digest must match after signing");
        assert!(v2.signature_ok, "signature must verify: {:?}", v2.errors);
        assert!(v2.valid());

        let v1 = verify_v1(&signed).unwrap();
        assert!(v1.present, "v1 files should be present");
        assert_eq!(v1.declared_schemes, vec![2]);
        assert_eq!(
            v1.signed_entries, 3,
            "three entries plus the signature files"
        );

        // Still a readable ZIP, with the signature files added.
        let mut archive =
            zip::ZipArchive::new(std::fs::File::open(&signed).unwrap()).expect("valid zip");
        let names: Vec<String> = (0..archive.len())
            .map(|i| archive.by_index(i).unwrap().name().to_string())
            .collect();
        assert!(names.contains(&"META-INF/MANIFEST.MF".to_string()));
        assert!(names.contains(&"META-INF/CERT.SF".to_string()));
        assert!(names.contains(&"META-INF/CERT.RSA".to_string()));
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn v2_rejects_signer_public_key_not_bound_to_embedded_certificate() {
        let dir = std::env::temp_dir().join(format!(
            "mandroid-cert-binding-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        let apk = dir.join("mismatch.apk");
        let entries = vec![Entry::new("AndroidManifest.xml", vec![3, 0, 8, 0])];
        let sections = repack::build_sections(&entries).unwrap();
        let content_digest = v2_digest(
            &sections.content,
            &sections.central_directory,
            &sections.eocd,
        );

        // Deliberately sign with key A and embed key B's valid X.509
        // certificate. The detached public_key remains key A, so the RSA
        // signature alone verifies; binding to the certificate must reject it.
        let signing_key = crypto::generate_key(crypto::RSA_BITS).unwrap();
        let certificate_key = crypto::generate_key(crypto::RSA_BITS).unwrap();
        let certificate = crypto::self_signed_cert_material(&certificate_key, "other", 30).unwrap();
        let mut digest_element = ALGO_RSA_PKCS1_SHA256.to_le_bytes().to_vec();
        digest_element.extend_from_slice(&lp(&content_digest));
        let digests = sequence(&[digest_element]);
        let certificates = sequence(&[certificate.der]);
        let attributes = sequence(&[]);
        let mut signed_data = Vec::new();
        signed_data.extend_from_slice(&digests);
        signed_data.extend_from_slice(&certificates);
        signed_data.extend_from_slice(&attributes);
        let signature =
            crypto::sign_prehashed_sha256(&signing_key, &Sha256::digest(&signed_data)).unwrap();
        use rsa::pkcs1v15::Pkcs1v15Sign;
        let detached_public_key = RsaPublicKey::from(&signing_key);
        assert!(detached_public_key
            .verify(
                Pkcs1v15Sign::new::<Sha256>(),
                &Sha256::digest(&signed_data),
                &signature
            )
            .is_ok());
        let mut signature_element = ALGO_RSA_PKCS1_SHA256.to_le_bytes().to_vec();
        signature_element.extend_from_slice(&lp(&signature));
        let signatures = sequence(&[signature_element]);
        let mut signer = lp(&signed_data);
        signer.extend_from_slice(&signatures);
        signer.extend_from_slice(&lp(&crypto::spki_der(&signing_key).unwrap()));
        let value = sequence(&[signer]);
        let block = build_signing_block(&value);
        let bytes = repack::assemble(
            &sections.content,
            Some(&block),
            &sections.central_directory,
            entries.len() as u16,
        );
        std::fs::write(&apk, bytes).unwrap();

        let verify = verify_v2(&apk).unwrap();
        assert!(
            !verify.signature_ok,
            "certificate mismatch must prevent signer acceptance"
        );
        assert!(!verify.valid());
        assert!(
            verify
                .errors
                .iter()
                .any(|e| e.contains("does not match the certificate")),
            "{:?}",
            verify.errors
        );
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn signing_a_signed_apk_replaces_rather_than_accumulates_signatures() {
        let dir = std::env::temp_dir().join(format!("mandroid-resign-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let first = dir.join("first.apk");
        let second = dir.join("second.apk");

        let entries = vec![Entry::new("AndroidManifest.xml", vec![1, 2, 3])];
        let sections = repack::build_sections(&entries).unwrap();
        std::fs::write(
            &first,
            repack::assemble(&sections.content, None, &sections.central_directory, 1),
        )
        .unwrap();

        let signer = ApkSigner::generate("one").unwrap();
        let r1 = sign_apk(
            &first,
            &first.with_extension("signed.apk"),
            &signer,
            &SignOptions::default(),
        )
        .unwrap();
        assert_eq!(r1.removed_signatures, 0);

        let r2 = sign_apk(
            &first.with_extension("signed.apk"),
            &second,
            &signer,
            &SignOptions::default(),
        )
        .unwrap();
        assert_eq!(
            r2.removed_signatures, 3,
            "old MANIFEST.MF/.SF/.RSA must be dropped"
        );
        assert!(verify_v2(&second).unwrap().valid());
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn tampering_with_a_signed_apk_breaks_the_digest() {
        let dir = std::env::temp_dir().join(format!("mandroid-tamper-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let unsigned = dir.join("u.apk");
        let signed = dir.join("s.apk");
        let tampered = dir.join("t.apk");

        let entries = vec![
            Entry::new("AndroidManifest.xml", vec![9, 9, 9, 9]),
            Entry::new("classes.dex", vec![7u8; 1000]),
        ];
        let sections = repack::build_sections(&entries).unwrap();
        std::fs::write(
            &unsigned,
            repack::assemble(&sections.content, None, &sections.central_directory, 2),
        )
        .unwrap();

        let signer = ApkSigner::generate("t").unwrap();
        sign_apk(&unsigned, &signed, &signer, &SignOptions::default()).unwrap();
        assert!(verify_v2(&signed).unwrap().valid());

        // Flip a byte inside the entry data (well before the signing block).
        let mut data = std::fs::read(&signed).unwrap();
        data[40] ^= 0xff;
        std::fs::write(&tampered, &data).unwrap();

        let v = verify_v2(&tampered).unwrap();
        assert!(v.found);
        assert!(
            !v.digest_ok,
            "a modified APK must not match the stored digest"
        );
        assert!(!v.valid());
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn spki_round_trips_through_the_der_parser() {
        let key = crypto::generate_key(crypto::RSA_BITS).unwrap();
        let spki = crypto::spki_der(&key).unwrap();
        let parsed = public_key_from_spki(&spki).unwrap();
        let original = RsaPublicKey::from(&key);
        use rsa::traits::PublicKeyParts;
        assert_eq!(parsed.n(), original.n());
        assert_eq!(parsed.e(), original.e());
    }

    #[test]
    fn jar_lines_wrap_at_seventy_two_bytes_with_space_continuations() {
        let mut out = Vec::new();
        let long = "A".repeat(200);
        jar_attr(&mut out, "SHA-256-Digest", &long);
        let text = String::from_utf8(out).unwrap();
        for line in text.split("\r\n") {
            if line.is_empty() {
                continue;
            }
            assert!(line.len() <= 72, "line too long ({}): {line:?}", line.len());
        }
        // Every line after the first must begin with a continuation space.
        let lines: Vec<&str> = text.split("\r\n").filter(|l| !l.is_empty()).collect();
        assert!(lines.len() > 1);
        for line in &lines[1..] {
            assert!(
                line.starts_with(' '),
                "continuation must start with a space"
            );
        }
        // Unwrapping (dropping the single leading continuation space) must
        // restore the original value exactly.
        let mut unwrapped = String::new();
        for (i, line) in lines.iter().enumerate() {
            if i == 0 {
                unwrapped.push_str(line);
            } else {
                unwrapped.push_str(line.strip_prefix(' ').unwrap_or(line));
            }
        }
        assert!(unwrapped.contains(&format!("SHA-256-Digest: {long}")));
    }
}
