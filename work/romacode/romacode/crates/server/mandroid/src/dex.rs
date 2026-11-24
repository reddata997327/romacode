//! DEX reader.
//!
//! For static triage the string table is what matters: hardcoded secrets, URLs,
//! and API class names all live there. This parses the DEX header and string
//! pool directly — no `dexdump`, no `baksmali`.

use anyhow::{bail, Result};

/// DEX header, little-endian. We only read the fields we need.
pub struct Dex<'a> {
    pub bytes: &'a [u8],
    pub string_ids_size: u32,
    pub string_ids_off: u32,
    pub type_ids_size: u32,
    pub method_ids_size: u32,
    pub class_defs_size: u32,
}

fn u32_at(b: &[u8], off: usize) -> Result<u32> {
    if off + 4 > b.len() {
        bail!("dex: read u32 past end at {off}");
    }
    Ok(u32::from_le_bytes([
        b[off],
        b[off + 1],
        b[off + 2],
        b[off + 3],
    ]))
}

/// Unsigned LEB128.
fn uleb128(b: &[u8], mut p: usize) -> Result<(u32, usize)> {
    let mut result: u32 = 0;
    let mut shift = 0;
    loop {
        if p >= b.len() {
            bail!("dex: truncated uleb128");
        }
        let byte = b[p];
        p += 1;
        result |= ((byte & 0x7f) as u32) << shift;
        if byte & 0x80 == 0 {
            break;
        }
        shift += 7;
        if shift > 28 {
            bail!("dex: uleb128 too long");
        }
    }
    Ok((result, p))
}

impl<'a> Dex<'a> {
    pub fn parse(bytes: &'a [u8]) -> Result<Dex<'a>> {
        if bytes.len() < 0x70 {
            bail!("dex: file too small ({} bytes)", bytes.len());
        }
        // "dex\n0NN\0"
        if &bytes[0..4] != b"dex\n" {
            bail!("dex: bad magic {:02x?}", &bytes[0..4]);
        }
        Ok(Dex {
            bytes,
            string_ids_size: u32_at(bytes, 0x38)?,
            string_ids_off: u32_at(bytes, 0x3c)?,
            type_ids_size: u32_at(bytes, 0x40)?,
            method_ids_size: u32_at(bytes, 0x58)?,
            class_defs_size: u32_at(bytes, 0x60)?,
        })
    }

    pub fn version(&self) -> String {
        // dex\n035\0 -> "035"
        String::from_utf8_lossy(&self.bytes[4..7]).to_string()
    }

    /// Every string in the pool. Mutf-8 is decoded lossily: we want to search
    /// it, not round-trip it.
    pub fn strings(&self) -> Vec<String> {
        let mut out = Vec::with_capacity(self.string_ids_size as usize);
        let base = self.string_ids_off as usize;
        for i in 0..self.string_ids_size as usize {
            let Ok(off) = u32_at(self.bytes, base + i * 4) else {
                break;
            };
            let off = off as usize;
            if off >= self.bytes.len() {
                continue;
            }
            // string_data_item: uleb128 utf16 length, then mutf-8 bytes, NUL.
            let Ok((_len, p)) = uleb128(self.bytes, off) else {
                continue;
            };
            let end = match self.bytes[p..].iter().position(|&b| b == 0) {
                Some(n) => p + n,
                None => continue,
            };
            let raw = &self.bytes[p..end];
            // Modified UTF-8 encodes NUL as C0 80 and uses CESU-8 for non-BMP;
            // for scanning, a lossy decode is exactly what we want.
            out.push(String::from_utf8_lossy(raw).to_string());
        }
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn uleb(mut v: u32) -> Vec<u8> {
        let mut out = vec![];
        loop {
            let mut b = (v & 0x7f) as u8;
            v >>= 7;
            if v != 0 {
                b |= 0x80;
            }
            out.push(b);
            if v == 0 {
                break;
            }
        }
        out
    }

    fn build_dex(strings: &[&str]) -> Vec<u8> {
        // Minimal but structurally real: header + string_ids + data.
        let mut bytes = vec![0u8; 0x70];
        bytes[0..8].copy_from_slice(b"dex\n035\0");
        let header_size = 0x70u32;
        let strings_off = header_size;
        let data_off = strings_off + (strings.len() as u32) * 4;

        let mut data = Vec::new();
        let mut offsets = Vec::new();
        for s in strings {
            offsets.push(data_off + data.len() as u32);
            data.extend_from_slice(&uleb(s.chars().count() as u32));
            data.extend_from_slice(s.as_bytes());
            data.push(0);
        }
        let total = data_off as usize + data.len();
        let mut out = vec![0u8; total];
        out[..0x70].copy_from_slice(&bytes);
        out[0..8].copy_from_slice(b"dex\n035\0");
        for (i, o) in offsets.iter().enumerate() {
            out[strings_off as usize + i * 4..strings_off as usize + i * 4 + 4]
                .copy_from_slice(&o.to_le_bytes());
        }
        out[data_off as usize..].copy_from_slice(&data);
        // header fields
        out[0x20..0x24].copy_from_slice(&(total as u32).to_le_bytes()); // file_size
        out[0x24..0x28].copy_from_slice(&header_size.to_le_bytes()); // header_size
        out[0x38..0x3c].copy_from_slice(&(strings.len() as u32).to_le_bytes());
        out[0x3c..0x40].copy_from_slice(&strings_off.to_le_bytes());
        out
    }

    #[test]
    fn parses_strings_from_a_synthetic_dex() {
        let dex = build_dex(&["hello", "AKIAIOSFODNN7EXAMPLE", "https://example.com"]);
        let d = Dex::parse(&dex).unwrap();
        assert_eq!(d.version(), "035");
        let s = d.strings();
        assert_eq!(s.len(), 3);
        assert_eq!(s[0], "hello");
        assert_eq!(s[1], "AKIAIOSFODNN7EXAMPLE");
    }

    #[test]
    fn rejects_non_dex() {
        assert!(Dex::parse(b"not a dex file at all, definitely not").is_err());
        assert!(Dex::parse(&[0u8; 4]).is_err());
    }

    #[test]
    fn uleb128_handles_multi_byte_values() {
        assert_eq!(uleb128(&[0x00], 0).unwrap().0, 0);
        assert_eq!(uleb128(&[0x7f], 0).unwrap().0, 127);
        assert_eq!(uleb128(&[0x80, 0x01], 0).unwrap().0, 128);
        assert_eq!(uleb128(&[0xff, 0x7f], 0).unwrap().0, 16383);
    }
}
