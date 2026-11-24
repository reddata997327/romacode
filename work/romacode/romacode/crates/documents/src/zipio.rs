//! Minimal ZIP reader + writer (STORE and DEFLATE), built on flate2's
//! pure-Rust backend. Enough for OOXML containers (docx/xlsx/pptx) — the
//! only ZIP features Office files actually need.

use std::io::{Read, Write};

// ── CRC-32 (IEEE 802.3), table-driven ───────────────────────────────────────

fn crc32_table() -> [u32; 256] {
    let mut t = [0u32; 256];
    let mut i = 0;
    while i < 256 {
        let mut c = i as u32;
        let mut k = 0;
        while k < 8 {
            c = if c & 1 != 0 { 0xEDB8_8320 ^ (c >> 1) } else { c >> 1 };
            k += 1;
        }
        t[i] = c;
        i += 1;
    }
    t
}

pub fn crc32(data: &[u8]) -> u32 {
    let t = crc32_table();
    let mut c = 0xFFFF_FFFFu32;
    for &b in data {
        c = t[((c ^ b as u32) & 0xFF) as usize] ^ (c >> 8);
    }
    c ^ 0xFFFF_FFFF
}

// ── Writer ──────────────────────────────────────────────────────────────────

/// One file entry staged for writing.
struct Staged {
    name: String,
    data: Vec<u8>,
    crc: u32,
    compressed: Vec<u8>,
    method: u16, // 0 = stored, 8 = deflate
}

/// Streaming ZIP writer: stage entries with [`ZipWriter::add`], finish with
/// [`ZipWriter::finish`]. Compression is deflate when it actually helps,
/// stored otherwise (like real archivers do for tiny/incompressible parts).
pub struct ZipWriter {
    entries: Vec<Staged>,
}

impl Default for ZipWriter {
    fn default() -> Self {
        Self::new()
    }
}

impl ZipWriter {
    pub fn new() -> Self {
        Self { entries: Vec::new() }
    }

    /// Add one file. Duplicate names overwrite the earlier entry (OOXML
    /// rels files can repeat names when callers regenerate parts).
    pub fn add(&mut self, name: &str, data: &[u8]) {
        let crc = crc32(data);
        let mut compressed = Vec::with_capacity(data.len() / 2 + 16);
        {
            let mut enc = flate2::write::DeflateEncoder::new(&mut compressed, flate2::Compression::fast());
            let _ = enc.write_all(data);
            let _ = enc.finish();
        }
        let method = if compressed.len() < data.len() { 8u16 } else { 0u16 };
        if method == 0 {
            compressed = data.to_vec();
        }
        let name = name.to_string();
        if let Some(e) = self.entries.iter_mut().find(|e| e.name == name) {
            e.data = data.to_vec();
            e.crc = crc;
            e.compressed = compressed;
            e.method = method;
        } else {
            self.entries.push(Staged { name, data: data.to_vec(), crc, compressed, method });
        }
    }

    /// Serialize the archive (local headers + data + central directory).
    pub fn finish(self) -> Vec<u8> {
        let mut out = Vec::new();
        let mut central: Vec<u8> = Vec::new();
        for e in &self.entries {
            let off = out.len() as u32;
            let name = e.name.as_bytes();
            // local file header
            out.extend_from_slice(&0x0403_4b50u32.to_le_bytes());
            out.extend_from_slice(&20u16.to_le_bytes()); // version needed
            out.extend_from_slice(&0x0800u16.to_le_bytes()); // utf-8 flag
            out.extend_from_slice(&e.method.to_le_bytes());
            out.extend_from_slice(&0u16.to_le_bytes()); // time
            out.extend_from_slice(&0x21u16.to_le_bytes()); // date (1980-01-01)
            out.extend_from_slice(&e.crc.to_le_bytes());
            out.extend_from_slice(&(e.compressed.len() as u32).to_le_bytes());
            out.extend_from_slice(&(e.data.len() as u32).to_le_bytes());
            out.extend_from_slice(&(name.len() as u16).to_le_bytes());
            out.extend_from_slice(&0u16.to_le_bytes()); // extra len
            out.extend_from_slice(name);
            out.extend_from_slice(&e.compressed);

            central.extend_from_slice(&0x0201_4b50u32.to_le_bytes());
            central.extend_from_slice(&20u16.to_le_bytes()); // version made by
            central.extend_from_slice(&20u16.to_le_bytes()); // version needed
            central.extend_from_slice(&0x0800u16.to_le_bytes());
            central.extend_from_slice(&e.method.to_le_bytes());
            central.extend_from_slice(&0u16.to_le_bytes());
            central.extend_from_slice(&0x21u16.to_le_bytes());
            central.extend_from_slice(&e.crc.to_le_bytes());
            central.extend_from_slice(&(e.compressed.len() as u32).to_le_bytes());
            central.extend_from_slice(&(e.data.len() as u32).to_le_bytes());
            central.extend_from_slice(&(name.len() as u16).to_le_bytes());
            central.extend_from_slice(&0u16.to_le_bytes()); // extra
            central.extend_from_slice(&0u16.to_le_bytes()); // comment
            central.extend_from_slice(&0u16.to_le_bytes()); // disk
            central.extend_from_slice(&0u16.to_le_bytes()); // int attrs
            central.extend_from_slice(&0u32.to_le_bytes()); // ext attrs
            central.extend_from_slice(&off.to_le_bytes());
            central.extend_from_slice(name);
        }
        let cd_off = out.len() as u32;
        let cd_len = central.len() as u32;
        out.extend_from_slice(&central);
        // end of central directory
        out.extend_from_slice(&0x0605_4b50u32.to_le_bytes());
        out.extend_from_slice(&0u16.to_le_bytes()); // disk
        out.extend_from_slice(&0u16.to_le_bytes()); // cd disk
        out.extend_from_slice(&(self.entries.len() as u16).to_le_bytes());
        out.extend_from_slice(&(self.entries.len() as u16).to_le_bytes());
        out.extend_from_slice(&cd_len.to_le_bytes());
        out.extend_from_slice(&cd_off.to_le_bytes());
        out.extend_from_slice(&0u16.to_le_bytes()); // comment len
        out
    }
}

// ── Reader ──────────────────────────────────────────────────────────────────

/// One entry of a parsed archive.
pub struct ZipEntry {
    pub name: String,
    pub method: u16,
    pub offset: u64, // local header offset
    pub compressed_size: u32,
    pub uncompressed_size: u32,
}

/// Random-access ZIP reader over an in-memory archive.
pub struct ZipReader<'a> {
    data: &'a [u8],
    entries: Vec<ZipEntry>,
}

impl<'a> ZipReader<'a> {
    pub fn open(data: &'a [u8]) -> Result<Self, String> {
        // Locate EOCD from the tail.
        if data.len() < 22 {
            return Err("zip: file too small".into());
        }
        let mut eocd = None;
        let max_scan = data.len().saturating_sub(22).min(65_536 + 22);
        let start = data.len() - 22;
        let mut i = 0;
        while i <= max_scan {
            let p = start - i;
            if data[p..p + 4] == [0x50, 0x4b, 0x05, 0x06] {
                eocd = Some(p);
                break;
            }
            i += 1;
        }
        let eocd = eocd.ok_or("zip: end-of-central-directory not found")?;
        let count = le16(data, eocd + 10) as usize;
        let cd_off = le32(data, eocd + 16) as usize;
        let mut entries = Vec::with_capacity(count);
        let mut p = cd_off;
        for _ in 0..count {
            if p + 46 > data.len() || data[p..p + 4] != [0x50, 0x4b, 0x01, 0x02] {
                break;
            }
            let method = le16(data, p + 10);
            let csize = le32(data, p + 20);
            let usize_ = le32(data, p + 24);
            let nlen = le16(data, p + 28) as usize;
            let elen = le16(data, p + 30) as usize;
            let clen = le16(data, p + 32) as usize;
            let lho = le32(data, p + 42) as u64;
            if p + 46 + nlen > data.len() {
                break;
            }
            let name = String::from_utf8_lossy(&data[p + 46..p + 46 + nlen]).into_owned();
            entries.push(ZipEntry { name, method, offset: lho, compressed_size: csize, uncompressed_size: usize_ });
            p += 46 + nlen + elen + clen;
        }
        Ok(Self { data, entries })
    }

    pub fn names(&self) -> Vec<String> {
        self.entries.iter().map(|e| e.name.clone()).collect()
    }

    pub fn entry(&self, name: &str) -> Option<&ZipEntry> {
        self.entries.iter().find(|e| e.name == name)
    }

    /// Decompress one entry. Handles stored and deflate; returns the raw
    /// bytes as stored in the archive otherwise (deflate64/bzip2 are not
    /// used by Office writers).
    pub fn read(&self, name: &str) -> Result<Vec<u8>, String> {
        let e = self
            .entry(name)
            .ok_or_else(|| format!("zip: missing entry {name:?}"))?;
        let start = e.offset as usize;
        if start + 30 > self.data.len() || self.data[start..start + 4] != [0x50, 0x4b, 0x03, 0x04] {
            return Err(format!("zip: bad local header for {name:?}"));
        }
        let nlen = le16(self.data, start + 26) as usize;
        let elen = le16(self.data, start + 28) as usize;
        let dstart = start + 30 + nlen + elen;
        let dend = dstart + e.compressed_size as usize;
        if dend > self.data.len() {
            return Err(format!("zip: truncated entry {name:?}"));
        }
        let raw = &self.data[dstart..dend];
        match e.method {
            0 => Ok(raw.to_vec()),
            8 => {
                let mut out = Vec::with_capacity(e.uncompressed_size as usize);
                let mut dec = flate2::read::DeflateDecoder::new(raw);
                dec.read_to_end(&mut out)
                    .map_err(|err| format!("zip: inflate {name:?}: {err}"))?;
                Ok(out)
            }
            m => Err(format!("zip: unsupported method {m} for {name:?}")),
        }
    }
}

fn le16(d: &[u8], p: usize) -> u16 {
    u16::from_le_bytes([d[p], d[p + 1]])
}
fn le32(d: &[u8], p: usize) -> u32 {
    u32::from_le_bytes([d[p], d[p + 1], d[p + 2], d[p + 3]])
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn round_trip() {
        let mut w = ZipWriter::new();
        w.add("a.txt", b"hello world hello world hello world");
        w.add("dir/b.xml", b"<x/>\n");
        let bytes = w.finish();
        let r = ZipReader::open(&bytes).unwrap();
        assert_eq!(r.read("a.txt").unwrap(), b"hello world hello world hello world");
        assert_eq!(r.read("dir/b.xml").unwrap(), b"<x/>\n");
        assert!(r.read("missing").is_err());
    }

    #[test]
    fn reads_deflate_archives() {
        // Written above with deflate method 8 — verify method and inflate.
        let mut w = ZipWriter::new();
        let big = "abc".repeat(400);
        w.add("big", big.as_bytes());
        let bytes = w.finish();
        let r = ZipReader::open(&bytes).unwrap();
        assert_eq!(r.entry("big").unwrap().method, 8);
        assert_eq!(r.read("big").unwrap(), big.as_bytes());
    }

    #[test]
    fn crc() {
        assert_eq!(crc32(b"123456789"), 0xCBF4_3926);
    }
}
