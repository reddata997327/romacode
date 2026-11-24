//! Android binary XML (AXML) decoder.
//!
//! `AndroidManifest.xml` inside an APK is not text: it is a compiled resource
//! chunk stream (RES_XML_TYPE) containing a string pool, a resource map and a
//! sequence of start/end element events. `aapt`, `apktool` and `jadx` all exist
//! mostly to decode this one format, so we do it ourselves.
//!
//! Layout (little-endian throughout):
//! ```text
//! 0x0003 RES_XML_TYPE      header(8)  + size
//!   0x0001 RES_STRING_POOL  header(28) + stringCount/styleCount/flags + offsets + data
//!   0x0180 RES_XML_RESOURCE_MAP   the resource IDs the string pool's first N
//!                                 entries correspond to (attribute names)
//!   0x0100/0x0101 namespace start/end
//!   0x0102 RES_XML_START_ELEMENT  ns, name, attrStart, attrSize, attrCount, attrs[]
//!   0x0103 RES_XML_END_ELEMENT
//!   0x0104 RES_XML_CDATA
//! ```

use anyhow::{anyhow, bail, Result};

const RES_STRING_POOL: u16 = 0x0001;
const RES_XML_RESOURCE_MAP: u16 = 0x0180;
const RES_XML_START_NAMESPACE: u16 = 0x0100;
const RES_XML_END_NAMESPACE: u16 = 0x0101;
const RES_XML_START_ELEMENT: u16 = 0x0102;
const RES_XML_END_ELEMENT: u16 = 0x0103;
const RES_XML_CDATA: u16 = 0x0104;

/// android:* attribute resource IDs we care about. In AXML an attribute's name
/// is usually a string-pool entry, but when it is empty the resource map plus
/// this table is the only way to name it.
const ATTR_RESOURCE_NAMES: &[(u32, &str)] = &[
    (0x0101_0001, "label"),
    (0x0101_0003, "name"),
    (0x0101_0006, "permission"),
    (0x0101_000f, "debuggable"),
    (0x0101_0010, "exported"),
    (0x0101_0018, "process"),
    (0x0101_001e, "targetPackage"),
    (0x0101_0020, "authorities"),
    (0x0101_0021, "grantUriPermissions"),
    (0x0101_0022, "readPermission"),
    (0x0101_0023, "writePermission"),
    (0x0101_0027, "launchMode"),
    (0x0101_0028, "screenOrientation"),
    (0x0101_002e, "taskAffinity"),
    (0x0101_0034, "minSdkVersion"),
    (0x0101_0035, "versionCode"),
    (0x0101_0036, "versionName"),
    (0x0101_0037, "targetSdkVersion"),
    (0x0101_0040, "maxSdkVersion"),
    (0x0101_0043, "configChanges"),
    (0x0101_005b, "allowTaskReparenting"),
    (0x0101_00a1, "protectionLevel"),
    (0x0101_0280, "allowBackup"),
    (0x0101_0281, "glEsVersion"),
    (0x0101_0283, "anyDensity"),
    (0x0101_04a0, "extractNativeLibs"),
    (0x0101_04ec, "usesCleartextTraffic"),
    (0x0101_04ed, "networkSecurityConfig"),
    (0x0101_04ee, "fullBackupContent"),
    (0x0101_0574, "requestLegacyExternalStorage"),
    (0x0101_0595, "dataExtractionRules"),
];

fn attr_resource_name(id: u32) -> Option<&'static str> {
    ATTR_RESOURCE_NAMES
        .iter()
        .find(|(rid, _)| *rid == id)
        .map(|(_, n)| *n)
}

/// Reverse of [`ATTR_RESOURCE_NAMES`]: framework attribute local name ->
/// resource id. Exposed so a caller (the AXML encoder) can rebuild the
/// resource-map chunk when it has decoded the tree back to names. Returns
/// `None` for names that are not in this table.
pub fn attr_resource_id(name: &str) -> Option<u32> {
    ATTR_RESOURCE_NAMES
        .iter()
        .find(|(_, n)| *n == name)
        .map(|(id, _)| *id)
}

#[derive(Debug, Clone, PartialEq)]
pub enum AttrValue {
    Str(String),
    Int(i32),
    Bool(bool),
    /// `@0x7f…` resource reference, decoded as the raw resource id.
    Reference(u32),
    Hex(u32),
    Float(f32),
    Null,
}

impl AttrValue {
    /// Human/JSON rendering. References stay symbolic so a human can tell a
    /// literal from a resource lookup.
    pub fn display(&self) -> String {
        match self {
            AttrValue::Str(s) => s.clone(),
            AttrValue::Int(i) => i.to_string(),
            AttrValue::Bool(b) => b.to_string(),
            AttrValue::Reference(r) => format!("@0x{r:08x}"),
            AttrValue::Hex(h) => format!("0x{h:x}"),
            AttrValue::Float(f) => f.to_string(),
            AttrValue::Null => "null".into(),
        }
    }
    pub fn as_bool(&self) -> Option<bool> {
        match self {
            AttrValue::Bool(b) => Some(*b),
            AttrValue::Int(i) => Some(*i != 0),
            _ => None,
        }
    }
    pub fn as_int(&self) -> Option<i32> {
        match self {
            AttrValue::Int(i) => Some(*i),
            AttrValue::Hex(h) => Some(*h as i32),
            _ => None,
        }
    }
    pub fn as_str(&self) -> Option<&str> {
        match self {
            AttrValue::Str(s) => Some(s),
            _ => None,
        }
    }
}

#[derive(Debug, Clone)]
pub struct Attr {
    pub namespace: Option<String>,
    pub name: String,
    pub value: AttrValue,
}

#[derive(Debug, Clone)]
pub struct Node {
    pub name: String,
    pub namespace: Option<String>,
    pub attrs: Vec<Attr>,
    pub children: Vec<Node>,
    pub line: u32,
}

impl Node {
    pub fn attr(&self, name: &str) -> Option<&AttrValue> {
        // Match on the local name, ignoring any namespace prefix.
        self.attrs
            .iter()
            .find(|a| a.name == name || a.name.ends_with(&format!(":{name}")))
            .map(|a| &a.value)
    }
    pub fn attr_str(&self, name: &str) -> Option<String> {
        self.attr(name).map(|v| v.display())
    }
    pub fn attr_bool(&self, name: &str) -> Option<bool> {
        self.attr(name).and_then(|v| v.as_bool())
    }
    pub fn attr_int(&self, name: &str) -> Option<i32> {
        self.attr(name).and_then(|v| v.as_int())
    }
    /// Depth-first walk over this node and all descendants.
    pub fn walk<'a>(&'a self, out: &mut Vec<&'a Node>) {
        out.push(self);
        for c in &self.children {
            c.walk(out);
        }
    }
    pub fn find<'a>(&'a self, name: &str, out: &mut Vec<&'a Node>) {
        if self.name == name {
            out.push(self);
        }
        for c in &self.children {
            c.find(name, out);
        }
    }
}

// ---------------------------------------------------------------------------

struct Reader<'a> {
    b: &'a [u8],
    pos: usize,
}

impl<'a> Reader<'a> {
    fn new(b: &'a [u8]) -> Self {
        Reader { b, pos: 0 }
    }
    fn u8(&mut self) -> Result<u8> {
        let v = *self.b.get(self.pos).ok_or_else(|| anyhow!("axml: eof"))?;
        self.pos += 1;
        Ok(v)
    }
    fn u16(&mut self) -> Result<u16> {
        let s = self.take(2)?;
        Ok(u16::from_le_bytes([s[0], s[1]]))
    }
    fn u32(&mut self) -> Result<u32> {
        let s = self.take(4)?;
        Ok(u32::from_le_bytes([s[0], s[1], s[2], s[3]]))
    }
    fn take(&mut self, n: usize) -> Result<&'a [u8]> {
        if self.pos + n > self.b.len() {
            bail!("axml: truncated (want {n} at {})", self.pos);
        }
        let s = &self.b[self.pos..self.pos + n];
        self.pos += n;
        Ok(s)
    }
    fn seek(&mut self, p: usize) -> Result<()> {
        if p > self.b.len() {
            bail!("axml: seek {p} past end {}", self.b.len());
        }
        self.pos = p;
        Ok(())
    }
}

/// The string pool. Strings are UTF-8 or UTF-16 depending on flag bit 0x100.
pub struct StringPool {
    pub strings: Vec<String>,
    utf8: bool,
}

impl StringPool {
    fn parse(chunk: &[u8]) -> Result<StringPool> {
        let mut r = Reader::new(chunk);
        let _ty = r.u16()?;
        let header_size = r.u16()? as usize;
        let _size = r.u32()?;
        // sanity-cap the declared count against the chunk (mirrors the dex
        // parser) — a raw u32 count must not drive the allocation
        let count = (r.u32()? as usize).min(chunk.len() / 4).min(1 << 20);
        let _styles = r.u32()?;
        let flags = r.u32()?;
        let strings_start = r.u32()? as usize;
        let _styles_start = r.u32()?;
        let utf8 = flags & 0x100 != 0;

        let mut offsets = Vec::with_capacity(count);
        for _ in 0..count {
            offsets.push(r.u32()? as usize);
        }
        let _ = header_size;

        let mut strings = Vec::with_capacity(count);
        for off in offsets {
            let abs = strings_start + off;
            if abs >= chunk.len() {
                strings.push(String::new());
                continue;
            }
            let s = if utf8 {
                decode_utf8(&chunk[abs..])?
            } else {
                decode_utf16(&chunk[abs..])?
            };
            strings.push(s);
        }
        Ok(StringPool { strings, utf8 })
    }

    pub fn get(&self, idx: u32) -> Option<&str> {
        if idx == u32::MAX {
            return None;
        }
        self.strings.get(idx as usize).map(|s| s.as_str())
    }

    pub fn is_utf8(&self) -> bool {
        self.utf8
    }
}

/// UTF-8 pool entry: length is 1 byte, or 2 when the high bit is set. The
/// length counts bytes and the stream is NUL-terminated.
fn decode_utf8(b: &[u8]) -> Result<String> {
    if b.is_empty() {
        return Ok(String::new());
    }
    let (len, hdr) = if b[0] & 0x80 != 0 {
        if b.len() < 2 {
            return Ok(String::new());
        }
        ((((b[0] & 0x7f) as usize) << 8) | b[1] as usize, 2usize)
    } else {
        (b[0] as usize, 1usize)
    };
    if hdr + len > b.len() {
        return Ok(String::new());
    }
    let raw = &b[hdr..hdr + len];
    // Trailing NUL is included in `len` by some producers.
    let raw = raw.strip_suffix(&[0u8]).unwrap_or(raw);
    Ok(String::from_utf8_lossy(raw).to_string())
}

/// UTF-16 pool entry: length in code units, 1 word or 2 when high bit set.
fn decode_utf16(b: &[u8]) -> Result<String> {
    if b.len() < 2 {
        return Ok(String::new());
    }
    let first = u16::from_le_bytes([b[0], b[1]]);
    let (len, hdr) = if first & 0x8000 != 0 {
        if b.len() < 4 {
            return Ok(String::new());
        }
        let second = u16::from_le_bytes([b[2], b[3]]);
        (
            (((first & 0x7fff) as usize) << 16) | second as usize,
            4usize,
        )
    } else {
        (first as usize, 2usize)
    };
    if hdr + len * 2 > b.len() {
        return Ok(String::new());
    }
    let units: Vec<u16> = (0..len)
        .map(|i| u16::from_le_bytes([b[hdr + i * 2], b[hdr + i * 2 + 1]]))
        .collect();
    Ok(String::from_utf16_lossy(&units))
}

fn decode_value(pool: &StringPool, data_type: u8, data: u32) -> AttrValue {
    match data_type {
        0x00 => AttrValue::Null,
        0x01 => AttrValue::Reference(data),
        0x02 => AttrValue::Reference(data),
        0x03 => AttrValue::Str(pool.get(data).unwrap_or("").to_string()),
        0x04 => AttrValue::Float(f32::from_bits(data)),
        0x10 => AttrValue::Int(data as i32),
        0x11 => AttrValue::Hex(data),
        0x12 => AttrValue::Bool(data != 0),
        0x1c..=0x1f => AttrValue::Hex(data),
        _ => AttrValue::Hex(data),
    }
}

/// Decode a compiled XML document into a node tree.
pub fn decode(bytes: &[u8]) -> Result<Node> {
    if bytes.len() < 8 {
        bail!("axml: too short");
    }
    let mut r = Reader::new(bytes);
    let ty = r.u16()?;
    let _hdr = r.u16()?;
    let _size = r.u32()?;
    if ty != 0x0003 {
        bail!("axml: not a RES_XML document (type 0x{ty:04x})");
    }

    let mut pool: Option<StringPool> = None;
    let mut resource_map: Vec<u32> = Vec::new();
    let mut stack: Vec<Node> = Vec::new();
    let mut root: Option<Node> = None;

    while r.pos + 8 <= bytes.len() {
        let chunk_start = r.pos;
        let ctype = r.u16()?;
        // The chunk header size is NOT constant: string pools say 28, the
        // resource map 8, and XML element chunks 16 (they carry lineNumber and
        // comment). Hardcoding 8 here silently shifts every element by 8 bytes.
        let chdr = r.u16()? as usize;
        let csize = r.u32()? as usize;
        if csize < 8 || chunk_start + csize > bytes.len() {
            bail!("axml: bad chunk size {csize} at {chunk_start}");
        }
        // header_size is file-controlled too; index with it only inside the
        // chunk
        if chdr < 8 || chdr > csize {
            bail!("axml: bad chunk header size {chdr} at {chunk_start}");
        }
        let chunk = &bytes[chunk_start..chunk_start + csize];

        match ctype {
            RES_STRING_POOL => pool = Some(StringPool::parse(chunk)?),
            RES_XML_RESOURCE_MAP => {
                let n = csize.saturating_sub(chdr) / 4;
                let mut rr = Reader::new(&chunk[chdr..]);
                for _ in 0..n {
                    resource_map.push(rr.u32()?);
                }
            }
            RES_XML_START_ELEMENT => {
                let p = pool
                    .as_ref()
                    .ok_or_else(|| anyhow!("axml: element before string pool"))?;
                let line = if chdr >= 12 && csize >= 12 {
                    u32::from_le_bytes([chunk[8], chunk[9], chunk[10], chunk[11]])
                } else {
                    0
                };
                let mut er = Reader::new(&chunk[chdr..]);
                // ns, name
                let ns_idx = er.u32()?;
                let name_idx = er.u32()?;
                let _attr_start = er.u16()?;
                let attr_size = er.u16()? as usize;
                let attr_count = er.u16()? as usize;
                // idIndex, classIndex, styleIndex
                let _ = er.u16()?;
                let _ = er.u16()?;
                let _ = er.u16()?;

                let mut attrs = Vec::with_capacity(attr_count);
                for _ in 0..attr_count {
                    let a_ns = er.u32()?;
                    let a_name = er.u32()?;
                    let _raw = er.u32()?;
                    let _vsize = er.u16()?;
                    let _res0 = er.u8()?;
                    let data_type = er.u8()?;
                    let data = er.u32()?;
                    // Pad to the declared attribute record size.
                    let consumed = 20;
                    if attr_size > consumed {
                        let _ = er.take(attr_size - consumed);
                    }
                    // Prefer the pool name; fall back to the resource map when
                    // the name is empty (common for framework attributes).
                    let name = p
                        .get(a_name)
                        .filter(|s| !s.is_empty())
                        .map(|s| s.to_string())
                        .or_else(|| {
                            resource_map
                                .get(a_name as usize)
                                .and_then(|id| attr_resource_name(*id))
                                .map(|s| s.to_string())
                        })
                        .or_else(|| {
                            resource_map
                                .get(a_name as usize)
                                .map(|id| format!("0x{id:08x}"))
                        })
                        .unwrap_or_else(|| format!("attr{a_name}"));
                    attrs.push(Attr {
                        namespace: p.get(a_ns).map(|s| s.to_string()),
                        name,
                        value: decode_value(p, data_type, data),
                    });
                }
                let node = Node {
                    name: p.get(name_idx).unwrap_or("?").to_string(),
                    namespace: p.get(ns_idx).map(|s| s.to_string()),
                    attrs,
                    children: Vec::new(),
                    line,
                };
                stack.push(node);
            }
            RES_XML_END_ELEMENT => {
                let node = stack
                    .pop()
                    .ok_or_else(|| anyhow!("axml: unbalanced end element"))?;
                match stack.last_mut() {
                    Some(parent) => parent.children.push(node),
                    None => root = Some(node),
                }
            }
            RES_XML_START_NAMESPACE | RES_XML_END_NAMESPACE | RES_XML_CDATA => {}
            _ => {}
        }
        r.seek(chunk_start + csize)?;
    }

    // Unclosed elements: close them so a truncated manifest still yields a tree.
    while let Some(node) = stack.pop() {
        match stack.last_mut() {
            Some(parent) => parent.children.push(node),
            None => root = Some(node),
        }
    }
    root.ok_or_else(|| anyhow!("axml: no root element"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn utf16_pool_entry_decodes() {
        // "hi": len=2, 'h','i', terminator
        let mut b = vec![2u8, 0];
        b.extend_from_slice(&('h' as u16).to_le_bytes());
        b.extend_from_slice(&('i' as u16).to_le_bytes());
        b.extend_from_slice(&0u16.to_le_bytes());
        assert_eq!(decode_utf16(&b).unwrap(), "hi");
    }

    #[test]
    fn utf8_pool_entry_decodes_with_and_without_nul() {
        let mut b = vec![2u8];
        b.extend_from_slice(b"hi");
        assert_eq!(decode_utf8(&b).unwrap(), "hi");
        let mut b2 = vec![3u8];
        b2.extend_from_slice(b"hi\0");
        assert_eq!(decode_utf8(&b2).unwrap(), "hi");
    }

    #[test]
    fn two_byte_length_forms_are_handled() {
        // UTF-8 with the high bit set: 0x80 0x03 -> length 3
        let mut b = vec![0x80u8, 0x03];
        b.extend_from_slice(b"abc");
        assert_eq!(decode_utf8(&b).unwrap(), "abc");
    }

    #[test]
    fn resource_ids_name_framework_attributes() {
        assert_eq!(attr_resource_name(0x0101_0010), Some("exported"));
        assert_eq!(
            attr_resource_name(0x0101_04ec),
            Some("usesCleartextTraffic")
        );
        assert_eq!(attr_resource_name(0xDEAD_BEEF), None);
    }

    #[test]
    fn attribute_names_reverse_to_resource_ids() {
        assert_eq!(attr_resource_id("exported"), Some(0x0101_0010));
        assert_eq!(attr_resource_id("debuggable"), Some(0x0101_000f));
        assert_eq!(attr_resource_id("not_a_framework_attr"), None);
    }

    #[test]
    fn value_decoding_covers_the_types_we_use() {
        let pool = StringPool {
            strings: vec!["hello".into()],
            utf8: false,
        };
        assert_eq!(decode_value(&pool, 0x03, 0), AttrValue::Str("hello".into()));
        assert_eq!(decode_value(&pool, 0x12, 1), AttrValue::Bool(true));
        assert_eq!(decode_value(&pool, 0x12, 0), AttrValue::Bool(false));
        assert_eq!(decode_value(&pool, 0x10, 31), AttrValue::Int(31));
        assert_eq!(
            decode_value(&pool, 0x11, 0x7f010000),
            AttrValue::Hex(0x7f010000)
        );
        assert_eq!(decode_value(&pool, 0x01, 42), AttrValue::Reference(42));
    }

    #[test]
    fn node_lookup_ignores_namespace_prefixes() {
        let n = Node {
            name: "activity".into(),
            namespace: None,
            attrs: vec![Attr {
                namespace: Some("http://schemas.android.com/apk/res/android".into()),
                name: "exported".into(),
                value: AttrValue::Bool(true),
            }],
            children: vec![],
            line: 0,
        };
        assert_eq!(n.attr_bool("exported"), Some(true));
        assert_eq!(n.attr("missing"), None);
    }
}
