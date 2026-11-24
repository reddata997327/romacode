//! ZIP read/rebuild for APK packaging.
//!
//! The `zip` crate reads entries for us, but signing needs byte-exact control
//! over the layout — the APK Signing Block is spliced between the last entry and
//! the central directory, and the signature covers each of those regions
//! separately. So we build the archive ourselves and hand the caller the three
//! regions the APK signature schemes digest.

use std::io::Read;
use std::path::Path;

use anyhow::{anyhow, bail, Context, Result};
use flate2::write::DeflateEncoder;
use flate2::Compression;
use std::io::Write;

use crate::util::crc32;

/// One entry of an APK, decompressed.
#[derive(Debug, Clone)]
pub struct Entry {
    pub name: String,
    pub data: Vec<u8>,
    /// 0 = stored, 8 = deflate.
    pub method: u16,
    pub dos_time: u16,
    pub dos_date: u16,
}

impl Entry {
    pub fn new(name: impl Into<String>, data: Vec<u8>) -> Self {
        Entry {
            name: name.into(),
            data,
            method: 8,
            dos_time: 0,
            dos_date: 0x21, // 1980-01-01, the ZIP epoch
        }
    }

    /// Entries that must not be compressed: the resource table (required
    /// uncompressed from API 30) and native libraries (kept uncompressed so a
    /// `extractNativeLibs=false` APK stays installable).
    pub fn must_stay_stored(&self) -> bool {
        self.name == "resources.arsc" || self.name.ends_with(".so")
    }

    pub fn is_signature_file(&self) -> bool {
        let upper = self.name.to_ascii_uppercase();
        if !upper.starts_with("META-INF/") {
            return false;
        }
        upper == "META-INF/MANIFEST.MF"
            || upper.ends_with(".SF")
            || upper.ends_with(".RSA")
            || upper.ends_with(".DSA")
            || upper.ends_with(".EC")
    }

    fn encoded(&self) -> Result<(Vec<u8>, u16)> {
        if self.method == 0 || self.must_stay_stored() {
            return Ok((self.data.clone(), 0));
        }
        let mut encoder = DeflateEncoder::new(Vec::new(), Compression::new(6));
        encoder.write_all(&self.data)?;
        let compressed = encoder.finish()?;
        // If deflating made it bigger, storing is the better legal choice.
        if compressed.len() >= self.data.len() {
            Ok((self.data.clone(), 0))
        } else {
            Ok((compressed, 8))
        }
    }
}

/// Read every entry of an APK, decompressed, keeping the original method hint.
pub fn read_apk(path: &Path) -> Result<Vec<Entry>> {
    let file = std::fs::File::open(path).with_context(|| format!("opening {}", path.display()))?;
    let mut archive = zip::ZipArchive::new(file)
        .with_context(|| format!("reading {} as a ZIP/APK", path.display()))?;
    let mut entries = Vec::with_capacity(archive.len());
    for i in 0..archive.len() {
        let mut member = archive.by_index(i)?;
        if member.is_dir() {
            continue;
        }
        let name = member.name().to_string();
        let mut data = Vec::with_capacity(member.size() as usize);
        member
            .read_to_end(&mut data)
            .with_context(|| format!("decompressing {name}"))?;
        entries.push(Entry {
            name,
            data,
            method: match member.compression() {
                zip::CompressionMethod::Stored => 0,
                _ => 8,
            },
            dos_time: member.last_modified().map(dos_time_of).unwrap_or(0),
            dos_date: member.last_modified().map(dos_date_of).unwrap_or(0x21),
        });
    }
    if entries.is_empty() {
        bail!("{} contains no entries", path.display());
    }
    Ok(entries)
}

fn dos_time_of(dt: zip::DateTime) -> u16 {
    ((dt.hour() as u16) << 11) | ((dt.minute() as u16) << 5) | (dt.second() as u16 / 2)
}

fn dos_date_of(dt: zip::DateTime) -> u16 {
    (((dt.year() as u16).saturating_sub(1980)) << 9) | ((dt.month() as u16) << 5) | dt.day() as u16
}

/// The three ZIP regions an APK signature covers, plus the raw EOCD record.
///
/// `content` is everything before where the signing block goes; `central_directory`
/// and `eocd` follow it. The signing block is spliced between `content` and
/// `central_directory`, and the EOCD has to be rewritten once its final size is
/// known — hence [`eocd`] being a separate function.
#[derive(Debug, Clone)]
pub struct Sections {
    pub content: Vec<u8>,
    pub central_directory: Vec<u8>,
    pub eocd: Vec<u8>,
}

/// Build the archive regions with no signing block.
pub fn build_sections(entries: &[Entry]) -> Result<Sections> {
    let mut content = Vec::new();
    let mut central = Vec::new();
    let mut count: u16 = 0;

    for entry in entries {
        let (payload, method) = entry.encoded()?;
        let name = entry.name.as_bytes();
        let crc = crc32(&entry.data);
        let offset = content.len() as u32;

        // Align stored payloads (notably resources.arsc) to 4 bytes, which is
        // what the platform expects for the resource table.
        let mut extra: Vec<u8> = Vec::new();
        if method == 0 {
            let pad = (4 - ((content.len() + 30 + name.len()) % 4)) % 4;
            // `pad` is always in 0..=3 by construction.
            debug_assert!(pad <= 3, "alignment pad is at most 3 bytes");
            if pad > 0 {
                // A self-describing ZIP extra field: id 0xd935 (Android's
                // alignment field), data_size = `pad` (declares the field
                // body is exactly `pad` bytes long), then `pad` zero bytes
                // which double as the alignment padding itself. Total
                // field = 4 + pad bytes; combined with the 30-byte fixed
                // local-header prefix and the name, the data offset lands
                // on a 4-byte boundary by construction:
                //   (30 + name.len() + 4 + pad) ≡ (30 + name.len() + pad) ≡ 0 (mod 4).
                extra.push(0x35);
                extra.push(0xd9);
                extra.extend_from_slice(&(pad as u16).to_le_bytes());
                for _ in 0..pad {
                    extra.push(0);
                }
            }
        }

        let mut local = Vec::with_capacity(30 + name.len() + extra.len() + payload.len());
        local.extend_from_slice(&0x04034b50u32.to_le_bytes());
        local.extend_from_slice(&20u16.to_le_bytes()); // version needed
        local.extend_from_slice(&0u16.to_le_bytes()); // flags
        local.extend_from_slice(&method.to_le_bytes());
        local.extend_from_slice(&entry.dos_time.to_le_bytes());
        local.extend_from_slice(&entry.dos_date.to_le_bytes());
        local.extend_from_slice(&crc.to_le_bytes());
        local.extend_from_slice(&(payload.len() as u32).to_le_bytes());
        local.extend_from_slice(&(entry.data.len() as u32).to_le_bytes());
        local.extend_from_slice(&(name.len() as u16).to_le_bytes());
        local.extend_from_slice(&(extra.len() as u16).to_le_bytes());
        local.extend_from_slice(name);
        local.extend_from_slice(&extra);
        local.extend_from_slice(&payload);
        content.extend_from_slice(&local);

        central.extend_from_slice(&0x02014b50u32.to_le_bytes());
        central.extend_from_slice(&20u16.to_le_bytes()); // version made by
        central.extend_from_slice(&20u16.to_le_bytes()); // version needed
        central.extend_from_slice(&0u16.to_le_bytes()); // flags
        central.extend_from_slice(&method.to_le_bytes());
        central.extend_from_slice(&entry.dos_time.to_le_bytes());
        central.extend_from_slice(&entry.dos_date.to_le_bytes());
        central.extend_from_slice(&crc.to_le_bytes());
        central.extend_from_slice(&(payload.len() as u32).to_le_bytes());
        central.extend_from_slice(&(entry.data.len() as u32).to_le_bytes());
        central.extend_from_slice(&(name.len() as u16).to_le_bytes());
        central.extend_from_slice(&(extra.len() as u16).to_le_bytes());
        central.extend_from_slice(&0u16.to_le_bytes()); // comment length
        central.extend_from_slice(&0u16.to_le_bytes()); // disk number start
        central.extend_from_slice(&0u16.to_le_bytes()); // internal attributes
        central.extend_from_slice(&0u32.to_le_bytes()); // external attributes
        central.extend_from_slice(&offset.to_le_bytes());
        central.extend_from_slice(name);
        central.extend_from_slice(&extra);

        count += 1;
    }

    let eocd = eocd(content.len() as u32, central.len() as u32, count);
    Ok(Sections {
        content,
        central_directory: central,
        eocd,
    })
}

/// Build the ZIP End of Central Directory record.
pub fn eocd(cd_offset: u32, cd_size: u32, entries: u16) -> Vec<u8> {
    let mut out = Vec::with_capacity(22);
    out.extend_from_slice(&0x06054b50u32.to_le_bytes());
    out.extend_from_slice(&0u16.to_le_bytes()); // this disk
    out.extend_from_slice(&0u16.to_le_bytes()); // disk with CD
    out.extend_from_slice(&entries.to_le_bytes());
    out.extend_from_slice(&entries.to_le_bytes());
    out.extend_from_slice(&cd_size.to_le_bytes());
    out.extend_from_slice(&cd_offset.to_le_bytes());
    out.extend_from_slice(&0u16.to_le_bytes()); // comment length
    out
}

/// Assemble the final file: content, optional signing block, central directory,
/// End of Central Directory.
pub fn assemble(content: &[u8], block: Option<&[u8]>, cd: &[u8], entries: u16) -> Vec<u8> {
    let block_len = block.map(|b| b.len() as u32).unwrap_or(0);
    let cd_offset = content.len() as u32 + block_len;
    let mut out = Vec::with_capacity(content.len() + block_len as usize + cd.len() + 22);
    out.extend_from_slice(content);
    if let Some(b) = block {
        out.extend_from_slice(b);
    }
    out.extend_from_slice(cd);
    out.extend_from_slice(&eocd(cd_offset, cd.len() as u32, entries));
    out
}

/// Locate the APK Signing Block. Returns `(offset of the block's first size
/// field, start of the ID-value pairs, block size)`.
pub fn find_signing_block(data: &[u8]) -> Option<(usize, usize, u64)> {
    const MAGIC: &[u8; 16] = b"APK Sig Block 42";
    if data.len() < 22 + 32 {
        return None;
    }
    let eocd_off = data.len() - 22;
    if &data[eocd_off..eocd_off + 4] != b"PK\x05\x06" {
        return None;
    }
    let cd_off = u32::from_le_bytes(data[eocd_off + 16..eocd_off + 20].try_into().ok()?) as usize;
    if cd_off < 32 || cd_off > data.len() {
        return None;
    }
    if &data[cd_off - 16..cd_off] != MAGIC {
        return None;
    }
    let size = u64::from_le_bytes(data[cd_off - 24..cd_off - 16].try_into().ok()?);
    let start = (cd_off as u64).checked_sub(size + 8)? as usize;
    let leading = u64::from_le_bytes(data[start..start + 8].try_into().ok()?);
    if leading != size {
        return None;
    }
    Some((start, start + 8, size))
}

/// Split a signed APK into the three regions a signature covers.
/// Returns `(content, central_directory, eocd_off)`.
pub fn split_signed(data: &[u8]) -> Result<(usize, usize, usize)> {
    let eocd_off = data.len() - 22;
    if &data[eocd_off..eocd_off + 4] != b"PK\x05\x06" {
        bail!("no ZIP End of Central Directory at the end of the file");
    }
    let cd_off =
        u32::from_le_bytes(data[eocd_off + 16..eocd_off + 20].try_into().unwrap()) as usize;
    let (block_start, _, _) =
        find_signing_block(data).ok_or_else(|| anyhow!("no APK Signing Block found"))?;
    Ok((block_start, cd_off, eocd_off))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample() -> Vec<Entry> {
        vec![
            Entry::new("AndroidManifest.xml", vec![0x03, 0x00, 0x08, 0x00]),
            Entry::new(
                "classes.dex",
                (0..5000u32).map(|i| (i % 251) as u8).collect(),
            ),
            Entry::new("resources.arsc", vec![0x02, 0x00, 0x0c, 0x00, 0xff]),
        ]
    }

    #[test]
    fn crc32_of_a_zip_entry_is_computed_over_uncompressed_data() {
        assert_eq!(crc32(b""), 0);
        assert_eq!(crc32(b"abc"), 0x3524_41c2);
    }

    #[test]
    fn built_archive_is_readable_by_a_zip_reader() {
        let entries = sample();
        let sections = build_sections(&entries).unwrap();
        let bytes = assemble(&sections.content, None, &sections.central_directory, 3);
        let mut archive = zip::ZipArchive::new(std::io::Cursor::new(bytes)).expect("valid zip");
        assert_eq!(archive.len(), 3);
        let mut dex = Vec::new();
        archive
            .by_name("classes.dex")
            .unwrap()
            .read_to_end(&mut dex)
            .unwrap();
        assert_eq!(dex, entries[1].data, "deflate round-trip must be lossless");
        let mut arsc = Vec::new();
        archive
            .by_name("resources.arsc")
            .unwrap()
            .read_to_end(&mut arsc)
            .unwrap();
        assert_eq!(arsc, entries[2].data);
    }

    #[test]
    fn resources_arsc_is_stored_uncompressed() {
        let entries = vec![Entry::new("resources.arsc", vec![7u8; 4096])];
        let sections = build_sections(&entries).unwrap();
        // Find the local header and check the method field.
        assert_eq!(&sections.content[0..4], &0x04034b50u32.to_le_bytes());
        let method = u16::from_le_bytes([sections.content[8], sections.content[9]]);
        assert_eq!(method, 0, "resources.arsc must be stored, not deflated");
    }

    #[test]
    fn signature_entries_are_recognised_for_removal() {
        assert!(Entry::new("META-INF/MANIFEST.MF", vec![]).is_signature_file());
        assert!(Entry::new("META-INF/CERT.SF", vec![]).is_signature_file());
        assert!(Entry::new("META-INF/CERT.RSA", vec![]).is_signature_file());
        assert!(Entry::new("META-INF/cert.rsa", vec![]).is_signature_file());
        assert!(!Entry::new("META-INF/services/foo", vec![]).is_signature_file());
        assert!(!Entry::new("classes.dex", vec![]).is_signature_file());
    }

    #[test]
    fn eocd_points_at_the_central_directory_it_was_given() {
        let e = eocd(1234, 567, 9);
        assert_eq!(e.len(), 22);
        assert_eq!(&e[0..4], b"PK\x05\x06");
        assert_eq!(u16::from_le_bytes([e[8], e[9]]), 9);
        assert_eq!(u32::from_le_bytes(e[12..16].try_into().unwrap()), 567);
        assert_eq!(u32::from_le_bytes(e[16..20].try_into().unwrap()), 1234);
    }
}
