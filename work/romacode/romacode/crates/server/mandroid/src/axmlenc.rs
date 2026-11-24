//! Android binary XML (AXML) encoder — the inverse of [`crate::axml::decode`].
//!
//! A modified [`crate::axml::Node`] tree can be serialised back into the
//! compiled form Android expects inside an APK, with no `aapt`/`apktool`.
//!
//! The wire layout emitted here mirrors, byte for byte, what [`crate::axml::decode`]
//! reads (see the module docs of `axml.rs`): a `RES_XML_TYPE` (0x0003) file
//! header, a UTF-16 string pool, an optional resource-map chunk, the
//! start/end-namespace pair, and the start/end-element chunk sequence.
//!
//! ## Two deliberate format choices
//!
//! * **UTF-16 string pool, exactly matching `aapt`.** Each entry carries a
//!   u16 length prefix (the number of UTF-16 code units, not counting the
//!   trailing NUL), the data as UTF-16LE, and a 2-byte NUL terminator. When
//!   the unit count is `>= 0x8000`, the length is split across two u16s
//!   (`0x8000 | high` then `low`) — the two-word form the decoder also reads.
//!   The pool's flag word is `0x0` (no `UTF8_FLAG`), which is what AOSP's
//!   `aapt` produces and Android's resource manager accepts; the existing
//!   [`crate::axml::decode_utf16`] decoder reads this form unmodified.
//! * **Resource ids only for namespaced framework attributes.** Plain manifest
//!   attributes (`package`, `platformBuildVersionCode`, …) carry no namespace
//!   and therefore no resource id, exactly like aapt.
//!
//! One lossy case is inherited from the decoder: [`crate::axml::AttrValue`] has
//! no `Dimension`/`Color` variants, so `decode` folds `dataType` `0x05` and
//! `0x1c..=0x1f` into `Hex(u32)`. Re-encoding such an attribute writes `0x11`
//! (integer/hex). A dimension or colour can only round-trip as an exact value,
//! not as its original type tag.

use std::collections::HashMap;

use anyhow::{bail, Result};

use crate::axml::{Attr, AttrValue, Node};

const RES_XML_TYPE: u16 = 0x0003;
const RES_STRING_POOL: u16 = 0x0001;
const RES_XML_RESOURCE_MAP: u16 = 0x0180;
const RES_XML_START_NAMESPACE: u16 = 0x0100;
const RES_XML_END_NAMESPACE: u16 = 0x0101;
const RES_XML_START_ELEMENT: u16 = 0x0102;
const RES_XML_END_ELEMENT: u16 = 0x0103;

/// `0xffffffff` marks "no index" for namespaces/strings in the chunk stream.
const NO_INDEX: u32 = u32::MAX;

/// The `android` namespace URI a manifest's framework attributes live in.
pub const ANDROID_NS: &str = "http://schemas.android.com/apk/res/android";

/// Pool flags: `0x0` — UTF-16 mode, exactly matching what AOSP's `aapt`
/// emits. The previous UTF-8 single-length form was rejected by Android's
/// resource manager; UTF-16 is the canonical pool format the decoder already
/// reads (see [`crate::axml::decode_utf16`]).
const POOL_FLAG: u32 = 0x0000_0000;

/// Framework attribute name -> resource id pairs extracted from a real
/// aapt-compiled manifest (`testdata/test-app.apk`, resource-map chunk). These
/// take precedence over [`crate::axml::attr_resource_id`], whose table has a
/// handful of stale ids.
const VERIFIED_ATTR_IDS: &[(&str, u32)] = &[
    ("theme", 0x0101_0000),
    ("label", 0x0101_0001),
    ("icon", 0x0101_0002),
    ("name", 0x0101_0003),
    ("permission", 0x0101_0006),
    ("protectionLevel", 0x0101_0009),
    ("enabled", 0x0101_000e),
    ("debuggable", 0x0101_000f),
    ("exported", 0x0101_0010),
    ("process", 0x0101_0011),
    ("finishOnTaskLaunch", 0x0101_0014),
    ("clearTaskOnLaunch", 0x0101_0015),
    ("stateNotNeeded", 0x0101_0016),
    ("excludeFromRecents", 0x0101_0017),
    ("authorities", 0x0101_0018),
    ("grantUriPermissions", 0x0101_001b),
    ("launchMode", 0x0101_001d),
    ("screenOrientation", 0x0101_001e),
    ("configChanges", 0x0101_001f),
    ("resource", 0x0101_0025),
    ("mimeType", 0x0101_0026),
    ("scheme", 0x0101_0027),
    ("host", 0x0101_0028),
    ("pathPrefix", 0x0101_002b),
    ("pathPattern", 0x0101_002c),
    ("targetActivity", 0x0101_0202),
    ("minSdkVersion", 0x0101_020c),
    ("versionCode", 0x0101_021b),
    ("versionName", 0x0101_021c),
    ("windowSoftInputMode", 0x0101_022b),
    ("noHistory", 0x0101_022d),
    ("targetSdkVersion", 0x0101_0270),
    ("maxSdkVersion", 0x0101_0271),
    ("allowBackup", 0x0101_0280),
    ("required", 0x0101_028e),
    ("parentActivityName", 0x0101_03a7),
    ("supportsRtl", 0x0101_03af),
    ("banner", 0x0101_03f2),
    ("extractNativeLibs", 0x0101_04ea),
    ("fullBackupContent", 0x0101_04eb),
    ("usesCleartextTraffic", 0x0101_04ec),
    ("autoVerify", 0x0101_04ee),
    ("directBootAware", 0x0101_0505),
    ("networkSecurityConfig", 0x0101_0527),
    ("compileSdkVersion", 0x0101_0572),
    ("compileSdkVersionCodename", 0x0101_0573),
    ("appComponentFactory", 0x0101_057a),
    ("foregroundServiceType", 0x0101_0599),
    ("memtagMode", 0x0101_0624),
    ("dataExtractionRules", 0x0101_063e),
    ("usesPermissionFlags", 0x0101_0644),
    ("localeConfig", 0x0101_065b),
    ("enableOnBackInvokedCallback", 0x0101_066c),
];

/// Resource id for a framework attribute local name, or `None` when the name is
/// not a known framework attribute (in which case aapt gives it no resource id
/// either). Consults the empirically verified table first, then the decoder's
/// own table exposed by [`crate::axml::attr_resource_id`].
pub fn framework_attr_id(name: &str) -> Option<u32> {
    VERIFIED_ATTR_IDS
        .iter()
        .find(|(n, _)| *n == name)
        .map(|(_, id)| *id)
        .or_else(|| crate::axml::attr_resource_id(name))
}

// ---------------------------------------------------------------------------
// Byte helpers

fn put_u16(out: &mut Vec<u8>, v: u16) {
    out.extend_from_slice(&v.to_le_bytes());
}
fn put_u32(out: &mut Vec<u8>, v: u32) {
    out.extend_from_slice(&v.to_le_bytes());
}

// ---------------------------------------------------------------------------
// String pool

#[derive(Default)]
struct Pool {
    index: HashMap<String, u32>,
    strings: Vec<String>,
}

impl Pool {
    /// Intern a string, returning its pool index. Order of first insertion is
    /// preserved, which is what lets the resource-map prefix line up.
    fn intern(&mut self, s: &str) -> u32 {
        if let Some(i) = self.index.get(s) {
            return *i;
        }
        let i = self.strings.len() as u32;
        self.strings.push(s.to_string());
        self.index.insert(s.to_string(), i);
        i
    }

    /// Index of an already-interned string, or [`NO_INDEX`].
    fn get(&self, s: &str) -> u32 {
        self.index.get(s).copied().unwrap_or(NO_INDEX)
    }

    fn ns(&self, ns: &Option<String>) -> u32 {
        match ns {
            Some(s) => self.get(s),
            None => NO_INDEX,
        }
    }
}

/// Encode the single length prefix the decoder reads: 1 byte when it fits,
/// otherwise `0x80 | high7` followed by the low byte (max 15 bits).
///
/// Not used by [`write_string_pool`] since the pool switched to UTF-16; kept
/// around because the unit tests below exercise it directly.
#[allow(dead_code)]
fn encode_len(out: &mut Vec<u8>, len: usize) -> Result<()> {
    if len < 0x80 {
        out.push(len as u8);
    } else if len <= 0x7fff {
        out.push(0x80 | ((len >> 8) as u8));
        out.push((len & 0xff) as u8);
    } else {
        bail!("axmlenc: string pool entry too long ({len} bytes)");
    }
    Ok(())
}

/// Serialise the string pool as UTF-16 (flag `0x0`) with empty style data.
/// Each entry is: u16 length prefix (UTF-16 code units, excluding the NUL),
/// the string bytes as UTF-16LE, then a 2-byte NUL terminator. Lengths of
/// `0x8000` or more use the two-word form (`0x8000 | high` then `low`).
fn write_string_pool(out: &mut Vec<u8>, pool: &Pool) -> Result<()> {
    let count = pool.strings.len();
    let strings_start = 28 + 4 * count; // header + offset table (styles empty)

    let mut data: Vec<u8> = Vec::new();
    let mut offsets: Vec<u32> = Vec::with_capacity(count);
    for s in &pool.strings {
        offsets.push(data.len() as u32);
        let units: Vec<u16> = s.encode_utf16().collect();
        let len = units.len();
        if len < 0x8000 {
            // Single u16 length prefix (little-endian).
            put_u16(&mut data, len as u16);
        } else if len <= 0x7fff_ffff {
            // Two-word form: 0x8000 | (len >> 16), then len & 0xffff.
            put_u16(&mut data, (0x8000 | ((len >> 16) as u16)) as u16);
            put_u16(&mut data, (len & 0xffff) as u16);
        } else {
            bail!("axmlenc: string pool entry too long ({len} code units)");
        }
        for u in &units {
            put_u16(&mut data, *u);
        }
        // 2-byte NUL terminator (a single UTF-16 NUL code unit).
        data.push(0);
        data.push(0);
    }
    while data.len() % 4 != 0 {
        data.push(0);
    }

    put_u16(out, RES_STRING_POOL);
    put_u16(out, 28);
    put_u32(out, (strings_start + data.len()) as u32);
    put_u32(out, count as u32);
    put_u32(out, 0); // styleCount
    put_u32(out, POOL_FLAG);
    put_u32(out, strings_start as u32);
    put_u32(out, 0); // stylesStart
    for o in offsets {
        put_u32(out, o);
    }
    out.extend_from_slice(&data);
    Ok(())
}

// ---------------------------------------------------------------------------
// Namespaces

/// Pick a prefix for a namespace URI. Manifests only ever use the `android`
/// namespace; anything else gets a deterministic placeholder.
fn prefix_for(uri: &str, idx: usize) -> String {
    if uri == ANDROID_NS {
        return "android".to_string();
    }
    let tail = uri.rsplit('/').find(|s| !s.is_empty()).unwrap_or("");
    let cleaned: String = tail
        .chars()
        .filter(|c| c.is_ascii_alphanumeric() || *c == '_')
        .collect();
    if cleaned.is_empty() {
        format!("ns{idx}")
    } else {
        cleaned
    }
}

fn write_ns_chunk(out: &mut Vec<u8>, ty: u16, line: u32, prefix: u32, uri: u32) {
    put_u16(out, ty);
    put_u16(out, 16);
    put_u32(out, 24);
    put_u32(out, line);
    put_u32(out, NO_INDEX); // comment
    put_u32(out, prefix);
    put_u32(out, uri);
}

// ---------------------------------------------------------------------------
// Elements

fn encode_value(attr: &Attr, pool: &Pool) -> (u8, u32) {
    match &attr.value {
        AttrValue::Null => (0x00, 0),
        AttrValue::Reference(r) => (0x01, *r),
        AttrValue::Str(s) => (0x03, pool.get(s)),
        AttrValue::Float(f) => (0x04, f.to_bits()),
        AttrValue::Int(i) => (0x10, *i as u32),
        AttrValue::Hex(h) => (0x11, *h),
        AttrValue::Bool(b) => (0x12, u32::from(*b)),
    }
}

fn emit_node(out: &mut Vec<u8>, node: &Node, pool: &Pool) -> Result<()> {
    let attr_count = node.attrs.len();
    if attr_count > u16::MAX as usize {
        bail!("axmlenc: too many attributes on <{}>", node.name);
    }

    // Start element: 16-byte header + ResXMLTree_attrExt (20) + N * 20.
    put_u16(out, RES_XML_START_ELEMENT);
    put_u16(out, 16);
    put_u32(out, (36 + 20 * attr_count) as u32);
    put_u32(out, node.line);
    put_u32(out, NO_INDEX); // comment
    put_u32(out, pool.ns(&node.namespace));
    put_u32(out, pool.get(&node.name));
    put_u16(out, 20); // attributeStart
    put_u16(out, 20); // attributeSize
    put_u16(out, attr_count as u16);
    put_u16(out, 0); // idIndex
    put_u16(out, 0); // classIndex
    put_u16(out, 0); // styleIndex

    for a in &node.attrs {
        put_u32(out, pool.ns(&a.namespace));
        put_u32(out, pool.get(&a.name));
        put_u32(out, NO_INDEX); // rawValue: use the typed value
        put_u16(out, 8); // Res_value.size
        out.push(0); // res0
        let (data_type, data) = encode_value(a, pool);
        out.push(data_type);
        put_u32(out, data);
    }

    for child in &node.children {
        emit_node(out, child, pool)?;
    }

    // End element: 16-byte header + ns, name.
    put_u16(out, RES_XML_END_ELEMENT);
    put_u16(out, 16);
    put_u32(out, 24);
    put_u32(out, node.line);
    put_u32(out, NO_INDEX); // comment
    put_u32(out, pool.ns(&node.namespace));
    put_u32(out, pool.get(&node.name));
    Ok(())
}

// ---------------------------------------------------------------------------
// Public API

/// Serialize a manifest tree to Android binary XML bytes.
pub fn encode(root: &Node) -> Result<Vec<u8>> {
    // Pre-order walk of the whole document.
    let mut nodes: Vec<&Node> = Vec::new();
    root.walk(&mut nodes);

    // 1. Namespaces, first-seen order.
    let mut ns_uris: Vec<String> = Vec::new();
    for n in &nodes {
        if let Some(ns) = &n.namespace {
            if !ns_uris.iter().any(|u| u == ns) {
                ns_uris.push(ns.clone());
            }
        }
        for a in &n.attrs {
            if let Some(ns) = &a.namespace {
                if !ns_uris.iter().any(|u| u == ns) {
                    ns_uris.push(ns.clone());
                }
            }
        }
    }

    // 2. Resource-mapped attribute names, first-seen order. They must occupy
    //    pool indices 0..N-1 so the resource-map chunk lines up.
    let mut res_names: Vec<String> = Vec::new();
    let mut res_ids: Vec<u32> = Vec::new();
    for n in &nodes {
        for a in &n.attrs {
            if a.namespace.as_deref() != Some(ANDROID_NS) {
                continue; // plain or foreign-namespace attributes get no id
            }
            if let Some(id) = framework_attr_id(&a.name) {
                if !res_names.iter().any(|x| x == &a.name) {
                    res_names.push(a.name.clone());
                    res_ids.push(id);
                }
            }
        }
    }

    // 3. Build the pool: resource-mapped names first, then everything else.
    let mut pool = Pool::default();
    for name in &res_names {
        pool.intern(name);
    }
    let mut ns_chunks: Vec<(u32, u32)> = Vec::with_capacity(ns_uris.len());
    for (i, uri) in ns_uris.iter().enumerate() {
        let prefix = prefix_for(uri, i);
        ns_chunks.push((pool.intern(&prefix), pool.intern(uri)));
    }
    for n in &nodes {
        pool.intern(&n.name);
        for a in &n.attrs {
            pool.intern(&a.name);
            if let AttrValue::Str(s) = &a.value {
                pool.intern(s);
            }
        }
    }

    // 4. Serialise the chunk stream that follows the 8-byte file header.
    let mut body: Vec<u8> = Vec::new();
    write_string_pool(&mut body, &pool)?;

    if !res_ids.is_empty() {
        put_u16(&mut body, RES_XML_RESOURCE_MAP);
        put_u16(&mut body, 8);
        put_u32(&mut body, (8 + 4 * res_ids.len()) as u32);
        for id in &res_ids {
            put_u32(&mut body, *id);
        }
    }

    let line = root.line;
    for (prefix, uri) in &ns_chunks {
        write_ns_chunk(&mut body, RES_XML_START_NAMESPACE, line, *prefix, *uri);
    }

    emit_node(&mut body, root, &pool)?;

    for (prefix, uri) in ns_chunks.iter().rev() {
        write_ns_chunk(&mut body, RES_XML_END_NAMESPACE, line, *prefix, *uri);
    }

    // 5. File header: type, headerSize 8, size = whole document.
    let mut out: Vec<u8> = Vec::with_capacity(body.len() + 8);
    put_u16(&mut out, RES_XML_TYPE);
    put_u16(&mut out, 8);
    put_u32(&mut out, (body.len() + 8) as u32);
    out.extend_from_slice(&body);
    Ok(out)
}

/// Decode `bytes`, re-encode the decoded tree, then decode the result again.
/// Returns `(original, re-decoded)` so callers can compare the two trees.
pub fn round_trip_check(bytes: &[u8]) -> Result<(Node, Node)> {
    let original = crate::axml::decode(bytes)?;
    let encoded = encode(&original)?;
    let again = crate::axml::decode(&encoded)?;
    Ok((original, again))
}

// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    // -- helpers ------------------------------------------------------------

    /// Walk the *inner* chunk stream, i.e. everything after the 8-byte
    /// `RES_XML_TYPE` file header, returning `(type, headerSize, size, offset)`.
    fn chunks(bytes: &[u8]) -> Vec<(u16, u16, usize, usize)> {
        let mut out = Vec::new();
        let mut pos = 8usize;
        while pos + 8 <= bytes.len() {
            let ty = u16::from_le_bytes([bytes[pos], bytes[pos + 1]]);
            let hs = u16::from_le_bytes([bytes[pos + 2], bytes[pos + 3]]);
            let sz = u32::from_le_bytes([
                bytes[pos + 4],
                bytes[pos + 5],
                bytes[pos + 6],
                bytes[pos + 7],
            ]) as usize;
            out.push((ty, hs, sz, pos));
            if sz < 8 {
                break;
            }
            pos += sz;
        }
        out
    }

    /// Decode the UTF-16 string pool the encoder wrote (u16 length prefix).
    fn pool_strings(bytes: &[u8]) -> Vec<String> {
        let chunk = chunks(bytes)
            .into_iter()
            .find(|(t, _, _, _)| *t == RES_STRING_POOL)
            .expect("string pool chunk");
        let (_, _, size, off) = chunk;
        let b = &bytes[off..off + size];
        let count = u32::from_le_bytes([b[8], b[9], b[10], b[11]]) as usize;
        let flags = u32::from_le_bytes([b[16], b[17], b[18], b[19]]);
        assert_eq!(flags & 0x100, 0, "pool must be UTF-16 (flag bit 0x100 clear)");
        let strings_start = u32::from_le_bytes([b[20], b[21], b[22], b[23]]) as usize;
        let mut out = Vec::with_capacity(count);
        for i in 0..count {
            let o = u32::from_le_bytes([b[28 + i * 4], b[29 + i * 4], b[30 + i * 4], b[31 + i * 4]])
                as usize;
            let p = &b[strings_start + o..];
            // u16 length prefix (UTF-16 code units, not counting the NUL).
            let first = u16::from_le_bytes([p[0], p[1]]);
            let (len, hdr) = if first & 0x8000 != 0 {
                // Two-word form: high in first (with top bit), low in second.
                let second = u16::from_le_bytes([p[2], p[3]]);
                ((((first & 0x7fff) as usize) << 16) | second as usize, 4)
            } else {
                (first as usize, 2)
            };
            let raw = &p[hdr..hdr + len * 2];
            let units: Vec<u16> = (0..len)
                .map(|i| u16::from_le_bytes([raw[i * 2], raw[i * 2 + 1]]))
                .collect();
            out.push(String::from_utf16_lossy(&units));
            // (Trailing 2-byte NUL terminator is simply not consumed here.)
        }
        out
    }

    fn resource_map(bytes: &[u8]) -> Vec<u32> {
        if let Some((_, hs, size, off)) = chunks(bytes)
            .into_iter()
            .find(|(t, _, _, _)| *t == RES_XML_RESOURCE_MAP)
        {
            let n = (size - hs as usize) / 4;
            let b = &bytes[off + hs as usize..off + size];
            (0..n)
                .map(|i| u32::from_le_bytes([b[i * 4], b[i * 4 + 1], b[i * 4 + 2], b[i * 4 + 3]]))
                .collect()
        } else {
            Vec::new()
        }
    }

    fn real_manifest() -> Vec<u8> {
        use std::io::Read;
        let path = concat!(env!("CARGO_MANIFEST_DIR"), "/testdata/test-app.apk");
        let file = std::fs::File::open(path).expect("testdata/test-app.apk");
        let mut zip = zip::ZipArchive::new(file).expect("read apk");
        let mut entry = zip
            .by_name("AndroidManifest.xml")
            .expect("AndroidManifest.xml");
        let mut buf = Vec::new();
        entry.read_to_end(&mut buf).expect("read manifest");
        buf
    }

    /// Recursive structural comparison: element names, namespaces, attributes
    /// (name, namespace, value) and children.
    fn assert_equivalent(a: &Node, b: &Node, path: &str) {
        assert_eq!(a.name, b.name, "element name at {path}");
        assert_eq!(a.namespace, b.namespace, "element namespace at {path}");
        assert_eq!(
            a.attrs.len(),
            b.attrs.len(),
            "attribute count on <{}> ({path})",
            a.name
        );
        for (i, (x, y)) in a.attrs.iter().zip(b.attrs.iter()).enumerate() {
            let at = format!("{path}/<{}>@[{i}]", a.name);
            assert_eq!(x.name, y.name, "attribute name at {at}");
            assert_eq!(x.namespace, y.namespace, "attribute namespace at {at}");
            assert_eq!(x.value, y.value, "attribute value at {at}");
        }
        assert_eq!(
            a.children.len(),
            b.children.len(),
            "child count on <{}> ({path})",
            a.name
        );
        for (i, (x, y)) in a.children.iter().zip(b.children.iter()).enumerate() {
            assert_equivalent(x, y, &format!("{path}/<{}>[{i}]", a.name));
        }
    }

    const ANDROID: &str = ANDROID_NS;

    fn attr(ns: Option<&str>, name: &str, value: AttrValue) -> Attr {
        Attr {
            namespace: ns.map(|s| s.to_string()),
            name: name.to_string(),
            value,
        }
    }

    // -- tests --------------------------------------------------------------

    #[test]
    fn axmlenc_round_trips_the_real_apk_manifest() {
        let original_bytes = real_manifest();
        assert!(original_bytes.len() > 8, "manifest present");

        let original = crate::axml::decode(&original_bytes).expect("decode real manifest");
        let encoded = encode(&original).expect("encode real manifest");
        let again = crate::axml::decode(&encoded).expect("re-decode real manifest");

        assert_equivalent(&original, &again, "$");

        // The tree really is non-trivial (guards against a vacuous pass).
        let mut nodes = Vec::new();
        original.walk(&mut nodes);
        // Calibrated to the generated fixture's manifest (uses-sdk + 6
        // permissions + application + activity + intent-filter + action +
        // category + service + receiver + filter + action ≈ 17 nodes).
        assert!(
            nodes.len() >= 15,
            "expected a substantial manifest, got {}",
            nodes.len()
        );

        println!(
            "fmandroid AndroidManifest.xml: original {} bytes, re-encoded {} bytes",
            original_bytes.len(),
            encoded.len()
        );
    }

    #[test]
    fn axmlenc_chunks_tile_the_buffer_exactly() {
        let original = crate::axml::decode(&real_manifest()).expect("decode");
        let encoded = encode(&original).expect("encode");

        // File header: type, headerSize 8, size = whole buffer.
        let file_ty = u16::from_le_bytes([encoded[0], encoded[1]]);
        let file_hs = u16::from_le_bytes([encoded[2], encoded[3]]);
        let file_sz = u32::from_le_bytes([encoded[4], encoded[5], encoded[6], encoded[7]]) as usize;
        assert_eq!(file_ty, 0x0003, "starts with RES_XML_TYPE");
        assert_eq!(file_hs, 8, "file header size");
        assert_eq!(file_sz, encoded.len(), "file chunk covers whole buffer");

        // Inner chunks start right after the 8-byte file header and must tile
        // the rest of the buffer with no gaps and no overruns.
        let mut pos = 8usize;
        let mut seen = 0usize;
        while pos + 8 <= encoded.len() {
            let ty = u16::from_le_bytes([encoded[pos], encoded[pos + 1]]);
            let sz = u32::from_le_bytes([
                encoded[pos + 4],
                encoded[pos + 5],
                encoded[pos + 6],
                encoded[pos + 7],
            ]) as usize;
            assert!(sz >= 8, "chunk 0x{ty:04x} has bogus size {sz}");
            assert!(
                pos + sz <= encoded.len(),
                "chunk 0x{ty:04x} at {pos} overruns the buffer"
            );
            pos += sz;
            seen += 1;
        }
        assert_eq!(pos, encoded.len(), "chunks tiled exactly");
        assert!(
            seen >= 3,
            "expected string pool + elements, saw {seen} chunks"
        );
    }

    #[test]
    fn axmlenc_preserves_framework_resource_ids() {
        let original = crate::axml::decode(&real_manifest()).expect("decode");
        let encoded = encode(&original).expect("encode");

        // The android namespace URI survives in the UTF-16 string pool. We
        // compare against its UTF-16LE byte encoding so the assertion is
        // encoding-faithful (the URI is ASCII-only, so each unit is `b, 0x00`).
        let android_utf16: Vec<u8> = ANDROID
            .encode_utf16()
            .flat_map(|u| u.to_le_bytes())
            .collect();
        assert!(
            encoded.windows(android_utf16.len()).any(|w| w == &android_utf16[..]),
            "android namespace URI missing from the pool"
        );

        let ids = resource_map(&encoded);
        assert!(!ids.is_empty(), "resource map emitted");
        assert!(ids.iter().any(|id| *id != 0), "at least one non-zero id");

        let strings = pool_strings(&encoded);
        // `android:exported` is used by the fmandroid manifest; its id must be in
        // the map at the same index as its name in the pool.
        let exported = ids
            .iter()
            .position(|id| *id == 0x0101_0010)
            .expect("exported id present in resource map");
        assert_eq!(strings[exported], "exported");
        // ... and `versionCode` resolves too.
        let vc = ids
            .iter()
            .position(|id| *id == 0x0101_021b)
            .expect("versionCode id present");
        assert_eq!(strings[vc], "versionCode");
    }

    #[test]
    fn axmlenc_round_trips_a_synthetic_manifest() {
        let root = Node {
            name: "manifest".into(),
            namespace: None,
            line: 1,
            attrs: vec![
                attr(None, "package", AttrValue::Str("com.example.app".into())),
                attr(Some(ANDROID), "versionCode", AttrValue::Int(7)),
                attr(Some(ANDROID), "versionName", AttrValue::Str("1.2.3".into())),
                attr(Some(ANDROID), "debuggable", AttrValue::Bool(true)),
                attr(Some(ANDROID), "theme", AttrValue::Reference(0x7f0e0001)),
                attr(Some(ANDROID), "targetSdkVersion", AttrValue::Int(33)),
                attr(Some(ANDROID), "configChanges", AttrValue::Hex(0x4a0)),
                attr(Some(ANDROID), "minSdkVersion", AttrValue::Int(21)),
                attr(None, "customFloat", AttrValue::Float(1.5)),
                attr(None, "customNull", AttrValue::Null),
            ],
            children: vec![Node {
                name: "application".into(),
                namespace: None,
                line: 2,
                attrs: vec![
                    attr(Some(ANDROID), "label", AttrValue::Str("Example".into())),
                    attr(Some(ANDROID), "allowBackup", AttrValue::Bool(false)),
                ],
                children: vec![Node {
                    name: "activity".into(),
                    namespace: None,
                    line: 3,
                    attrs: vec![
                        attr(Some(ANDROID), "name", AttrValue::Str(".Main".into())),
                        attr(Some(ANDROID), "exported", AttrValue::Bool(true)),
                    ],
                    children: vec![Node {
                        name: "intent-filter".into(),
                        namespace: None,
                        line: 4,
                        attrs: vec![],
                        children: vec![],
                    }],
                }],
            }],
        };

        let encoded = encode(&root).expect("encode synthetic");
        let decoded = crate::axml::decode(&encoded).expect("decode synthetic");
        assert_equivalent(&root, &decoded, "$");

        // Sanity: namespaced values keep their types.
        let app = &decoded.children[0];
        assert_eq!(app.attr_bool("allowBackup"), Some(false));
        assert_eq!(
            app.children[0].attr("exported"),
            Some(&AttrValue::Bool(true))
        );
        assert_eq!(app.children[0].attr_str("name").as_deref(), Some(".Main"));

        // Ids and namespaces survive in the synthetic document too.
        let ids = resource_map(&encoded);
        assert!(ids.contains(&0x0101_000f), "debuggable id 0x0101000f");
        assert!(ids.contains(&0x0101_0010), "exported id 0x01010010");
        assert!(ids.contains(&0x0101_021b), "versionCode id 0x0101021b");
        assert!(
            {
                let android_utf16: Vec<u8> = ANDROID
                    .encode_utf16()
                    .flat_map(|u| u.to_le_bytes())
                    .collect();
                encoded.windows(android_utf16.len()).any(|w| w == &android_utf16[..])
            },
            "android namespace URI in pool"
        );
    }

    #[test]
    fn axmlenc_public_round_trip_check_agrees() {
        let bytes = real_manifest();
        let (a, b) = round_trip_check(&bytes).expect("round trip");
        assert_equivalent(&a, &b, "$");
    }

    #[test]
    fn axmlenc_length_prefixes_cover_both_forms() {
        let mut short = Vec::new();
        encode_len(&mut short, 0x7f).unwrap();
        assert_eq!(short, vec![0x7f]);

        let mut long = Vec::new();
        encode_len(&mut long, 0x123).unwrap();
        assert_eq!(long, vec![0x80 | 0x01, 0x23]);

        assert!(encode_len(&mut Vec::new(), 0x8000).is_err());
    }
}
