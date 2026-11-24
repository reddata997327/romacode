//! Android compiled resource table (`resources.arsc`) decoder.
//!
//! `resources.arsc` is the binary resource table produced by `aapt`/`aapt2`.
//! It maps 32-bit resource ids of the form `0xPPTTEEEE` (package / type /
//! entry) to their values. The most useful lookup for static analysis is
//! turning an app label such as `@0x7f110045` into the real string
//! `"F-Droid"`.
//!
//! The parser is deliberately self-contained: it only needs the standard
//! library. Every read is bounds-checked and malformed input yields an
//! [`anyhow::Error`] rather than a panic.
//!
//! # Format
//!
//! Everything is little-endian. The file is a tree of chunks, each with the
//! header `type: u16, header_size: u16, size: u32`:
//!
//! * `0x0002` table (the root),
//! * `0x0001` string pool,
//! * `0x0200` package,
//! * `0x0201` type (carries a `ResTable_config` and entry values),
//! * `0x0202` type spec (entry metadata; skipped),
//! * `0x0203` library.
//!
//! Note: some documentation (and the task that produced this module) lists
//! `0x0201`/`0x0202` the other way round. The constants here follow the
//! AOSP `ResourceTypes.h` layout, and [`parse_type_or_spec`] additionally
//! disambiguates structurally by `header_size`, so either convention parses.

use anyhow::{bail, Result};

/// Chunk type: root resource table.
const RES_TABLE_TYPE: u16 = 0x0002;
/// Chunk type: a string pool.
const RES_STRING_POOL_TYPE: u16 = 0x0001;
/// Chunk type: a package.
const RES_TABLE_PACKAGE_TYPE: u16 = 0x0200;
/// Chunk type: a type (id + config + entries).
const RES_TABLE_TYPE_TYPE: u16 = 0x0201;
/// Chunk type: a type spec.
const RES_TABLE_TYPE_SPEC_TYPE: u16 = 0x0202;

/// Flag on [`RES_STRING_POOL_TYPE`]: strings are UTF-8 (two varint lengths).
const SORTED_FLAG: u32 = 0x0000_0001;
/// Flag on [`RES_STRING_POOL_TYPE`]: strings are encoded as UTF-8.
const UTF8_FLAG: u32 = 0x0000_0100;

/// `ResTable_type::FLAG_SPARSE`.
const TYPE_FLAG_SPARSE: u8 = 0x01;
/// `ResTable_type::FLAG_OFFSET16`.
const TYPE_FLAG_OFFSET16: u8 = 0x02;

/// `Res_value` data types we know how to render.
const TYPE_NULL: u8 = 0x00;
const TYPE_REFERENCE: u8 = 0x01;
const TYPE_ATTRIBUTE: u8 = 0x02;
const TYPE_STRING: u8 = 0x03;
const TYPE_FLOAT: u8 = 0x04;
const TYPE_DIMENSION: u8 = 0x05;
const TYPE_FRACTION: u8 = 0x06;
const TYPE_INT_DEC: u8 = 0x10;
const TYPE_INT_HEX: u8 = 0x11;
const TYPE_INT_BOOLEAN: u8 = 0x12;
const TYPE_INT_COLOR_ARGB8: u8 = 0x1c;

/// A decoded resource table.
#[derive(Debug, Clone, Default)]
pub struct ResTable {
    pub packages: Vec<Package>,
    /// The table-level string pool referenced by `TYPE_STRING` values.
    pub global_strings: Vec<String>,
}

/// One package (one `RES_TABLE_PACKAGE_TYPE` chunk).
#[derive(Debug, Clone, Default)]
pub struct Package {
    /// Package id, e.g. `0x7f` for the app.
    pub id: u32,
    /// Package name, e.g. `org.fdroid.fdroid`.
    pub name: String,
    /// Type names indexed by `type id - 1`.
    pub type_strings: Vec<String>,
    /// Key names indexed by an entry's `key` field.
    pub key_strings: Vec<String>,
    pub types: Vec<ResType>,
}

/// A resource type (e.g. `string`, `drawable`) and all of its configs.
#[derive(Debug, Clone, Default)]
pub struct ResType {
    pub id: u8,
    pub name: String,
    pub configs: Vec<ResConfig>,
}

/// A single configuration (default, `en`, `xxhdpi`, ...) of a type.
#[derive(Debug, Clone, Default)]
pub struct ResConfig {
    /// Human-readable config tag, or `"default"` for the base config.
    pub config: String,
    /// Entries indexed by entry id; `None` where the id is absent.
    pub entries: Vec<Option<ResEntry>>,
}

/// One resolved resource entry.
#[derive(Debug, Clone, Default)]
pub struct ResEntry {
    pub id: u32,
    pub key: String,
    pub value: String,
    pub is_complex: bool,
}

// ---------------------------------------------------------------------------
// Bounds-checked little-endian readers.
// ---------------------------------------------------------------------------

fn slice(d: &[u8], off: usize, len: usize) -> Result<&[u8]> {
    let end = off
        .checked_add(len)
        .ok_or_else(|| anyhow::anyhow!("arsc: offset overflow at {off}+{len}"))?;
    if end > d.len() {
        bail!("arsc: read {len} bytes at {off} past end (len {})", d.len());
    }
    Ok(&d[off..end])
}

fn rd_u8(d: &[u8], off: usize) -> Result<u8> {
    Ok(slice(d, off, 1)?[0])
}

fn rd_u16(d: &[u8], off: usize) -> Result<u16> {
    let b = slice(d, off, 2)?;
    Ok(u16::from_le_bytes([b[0], b[1]]))
}

fn rd_u32(d: &[u8], off: usize) -> Result<u32> {
    let b = slice(d, off, 4)?;
    Ok(u32::from_le_bytes([b[0], b[1], b[2], b[3]]))
}

/// A `chunk: type/header_size/size` triple.
struct Chunk {
    ty: u16,
    header_size: usize,
    size: usize,
}

fn chunk_at(d: &[u8], off: usize) -> Result<Chunk> {
    Ok(Chunk {
        ty: rd_u16(d, off)?,
        header_size: rd_u16(d, off + 2)? as usize,
        size: rd_u32(d, off + 4)? as usize,
    })
}

// ---------------------------------------------------------------------------
// String pools.
// ---------------------------------------------------------------------------

/// Decode a UTF-8 string-pool length: one byte, or two when the high bit of
/// the first is set. Returns `(value, next_offset)`.
fn dec_len8(d: &[u8], p: usize) -> Result<(usize, usize)> {
    let b0 = rd_u8(d, p)?;
    if b0 & 0x80 != 0 {
        let b1 = rd_u8(d, p + 1)?;
        Ok(((((b0 & 0x7f) as usize) << 8) | b1 as usize, p + 2))
    } else {
        Ok((b0 as usize, p + 1))
    }
}

/// Decode a UTF-16 string-pool length: one `u16`, or two when the high bit of
/// the first is set (the second is the low 16 bits). Returns `(value, next)`.
fn dec_len16(d: &[u8], p: usize) -> Result<(usize, usize)> {
    let l0 = rd_u16(d, p)?;
    if l0 & 0x8000 != 0 {
        let l1 = rd_u16(d, p + 2)?;
        Ok(((((l0 & 0x7fff) as usize) << 16) | l1 as usize, p + 4))
    } else {
        Ok((l0 as usize, p + 2))
    }
}

/// Parse a `RES_STRING_POOL_TYPE` chunk.
///
/// UTF-8 pools store two lengths per string (UTF-16 code-unit count followed
/// by byte count); UTF-16 pools store a single `u16` count. Offsets are
/// relative to the chunk start plus `strings_start`.
pub fn parse_string_pool(d: &[u8], off: usize) -> Result<Vec<String>> {
    let ch = chunk_at(d, off)?;
    if ch.ty != RES_STRING_POOL_TYPE {
        bail!("arsc: expected string pool at {off}, got 0x{:04x}", ch.ty);
    }
    let count = rd_u32(d, off + 8)? as usize;
    let flags = rd_u32(d, off + 16)?;
    let strings_start = rd_u32(d, off + 20)? as usize;

    // Guard against absurd counts from corrupt files (each offset is 4 bytes).
    if count > 10_000_000 {
        bail!("arsc: implausible string count {count}");
    }

    let offsets_base = off + ch.header_size;
    let data_base = off
        .checked_add(strings_start)
        .ok_or_else(|| anyhow::anyhow!("arsc: strings_start overflow"))?;
    let utf8 = flags & UTF8_FLAG != 0;
    let _sorted = flags & SORTED_FLAG != 0;

    let mut out = Vec::with_capacity(count);
    for i in 0..count {
        let rel = rd_u32(d, offsets_base + i * 4)? as usize;
        let p = data_base
            .checked_add(rel)
            .ok_or_else(|| anyhow::anyhow!("arsc: string offset overflow"))?;
        let s = if utf8 {
            let (_chars, p) = dec_len8(d, p)?;
            let (bytes, p) = dec_len8(d, p)?;
            let raw = slice(d, p, bytes)?;
            String::from_utf8_lossy(raw).into_owned()
        } else {
            let (len, p) = dec_len16(d, p)?;
            let raw = slice(d, p, len * 2)?;
            let units: Vec<u16> = raw
                .chunks_exact(2)
                .map(|c| u16::from_le_bytes([c[0], c[1]]))
                .collect();
            String::from_utf16_lossy(&units)
        };
        out.push(s);
    }
    Ok(out)
}

// ---------------------------------------------------------------------------
// Config rendering.
// ---------------------------------------------------------------------------

/// Render a `ResTable_config` blob into a short tag such as `"default"`,
/// `"en"`, `"en-land-xxhdpi"` or `"night"`.
///
/// Only the cheap, commonly useful fields are decoded: locale, orientation,
/// density and night mode. Anything unrecognised leaves the tag as
/// `"default"`, which also matches an all-zero config.
pub fn render_config(bytes: &[u8]) -> String {
    let mut parts: Vec<String> = Vec::new();

    let zeros = bytes.iter().skip(4).all(|b| *b == 0);
    let size = if bytes.len() >= 4 {
        u32::from_le_bytes([bytes[0], bytes[1], bytes[2], bytes[3]])
    } else {
        0
    };
    if size == 0 || zeros {
        return "default".to_string();
    }

    // language[2] country[2] at offsets 8 and 10.
    if let Some(lang) = two_char_code(bytes, 8) {
        parts.push(lang);
    }
    if let Some(country) = two_char_code(bytes, 10) {
        // Region is rendered upper-case by convention (en-rUS).
        parts.push(format!("r{}", country.to_uppercase()));
    }

    // orientation at offset 12.
    if bytes.len() >= 13 {
        match bytes[12] {
            1 => parts.push("port".into()),
            2 => parts.push("land".into()),
            3 => parts.push("square".into()),
            _ => {}
        }
    }

    // density u16 at offset 14.
    if bytes.len() >= 16 {
        let density = u16::from_le_bytes([bytes[14], bytes[15]]);
        if let Some(name) = density_name(density) {
            parts.push(name.into());
        }
    }

    // uiMode at offset 29; UI_MODE_NIGHT_YES == 0x20.
    if bytes.len() >= 30 && bytes[29] & 0x30 == 0x20 {
        parts.push("night".into());
    }

    if parts.is_empty() {
        "default".to_string()
    } else {
        parts.join("-")
    }
}

/// Read a two-letter locale code, tolerating the packed 3-letter form by
/// falling back to a hex tag. Returns `None` for an all-zero field.
fn two_char_code(bytes: &[u8], off: usize) -> Option<String> {
    if bytes.len() < off + 2 {
        return None;
    }
    let (a, b) = (bytes[off], bytes[off + 1]);
    if a == 0 && b == 0 {
        return None;
    }
    // A leading high bit means a packed 3-letter code; render it as hex.
    if a & 0x80 != 0 {
        return Some(format!("{:02x}{:02x}", a, b));
    }
    let mut s = String::new();
    for &c in &[a, b] {
        if c == 0 {
            break;
        }
        if c.is_ascii_alphanumeric() {
            s.push(c as char);
        } else {
            s.push_str(&format!("{:02x}", c));
        }
    }
    if s.is_empty() {
        None
    } else {
        Some(s)
    }
}

fn density_name(density: u16) -> Option<&'static str> {
    Some(match density {
        0 => return None,
        120 => "ldpi",
        160 => "mdpi",
        213 => "tvdpi",
        240 => "hdpi",
        320 => "xhdpi",
        480 => "xxhdpi",
        640 => "xxxhdpi",
        _ => return None,
    })
}

// ---------------------------------------------------------------------------
// Package / type parsing.
// ---------------------------------------------------------------------------

/// Read the fixed 128-`u16` package name field, stopping at the NUL.
fn decode_package_name(d: &[u8], off: usize) -> Result<String> {
    let raw = slice(d, off, 256)?;
    let units: Vec<u16> = raw
        .chunks_exact(2)
        .map(|c| u16::from_le_bytes([c[0], c[1]]))
        .take_while(|u| *u != 0)
        .collect();
    Ok(String::from_utf16_lossy(&units))
}

fn parse_package(d: &[u8], off: usize, ch: &Chunk) -> Result<Package> {
    let id = rd_u32(d, off + 8)?;
    let name = decode_package_name(d, off + 12)?;

    // id(4) + name(256) => typeStrings at +268.
    let type_strings_off = rd_u32(d, off + 268)? as usize;
    let key_strings_off = rd_u32(d, off + 276)? as usize;

    let type_strings = parse_string_pool(d, off + type_strings_off)
        .map_err(|e| e.context("package typeStrings pool"))?;
    let key_strings = parse_string_pool(d, off + key_strings_off)
        .map_err(|e| e.context("package keyStrings pool"))?;

    let mut pkg = Package {
        id,
        name,
        type_strings,
        key_strings,
        types: Vec::new(),
    };

    let start = off
        .checked_add(ch.header_size)
        .ok_or_else(|| anyhow::anyhow!("arsc: package header overflow"))?;
    let end = off
        .checked_add(ch.size)
        .ok_or_else(|| anyhow::anyhow!("arsc: package size overflow"))?;
    let end = end.min(d.len());

    let mut p = start;
    while p + 8 <= end {
        let sub = chunk_at(d, p)?;
        if sub.size < 8 || p + sub.size > end {
            break;
        }
        match sub.ty {
            RES_TABLE_TYPE_TYPE | RES_TABLE_TYPE_SPEC_TYPE => {
                if let Some((type_id, cfg)) = parse_type_or_spec(d, p, &sub, &pkg)? {
                    // Merge into the existing ResType or create a new one.
                    let name = if type_id >= 1 {
                        pkg.type_strings
                            .get(type_id as usize - 1)
                            .cloned()
                            .unwrap_or_else(|| format!("type{type_id}"))
                    } else {
                        format!("type{type_id}")
                    };
                    if let Some(existing) = pkg.types.iter_mut().find(|t| t.id == type_id) {
                        existing.configs.push(cfg);
                    } else {
                        pkg.types.push(ResType {
                            id: type_id,
                            name,
                            configs: vec![cfg],
                        });
                    }
                }
            }
            _ => {}
        }
        p += sub.size;
    }

    Ok(pkg)
}

/// Parse a type or type-spec chunk.
///
/// Returns `Ok(Some((type_id, config)))` for a real type chunk and `Ok(None)`
/// for a spec (which carries no values). The distinction is structural: a
/// type has a config and therefore a `header_size >= 20`, a spec is exactly
/// 16 bytes. This avoids depending on the (frequently mis-quoted) type
/// constants.
fn parse_type_or_spec(
    d: &[u8],
    off: usize,
    ch: &Chunk,
    pkg: &Package,
) -> Result<Option<(u8, ResConfig)>> {
    if ch.header_size < 20 {
        // Type spec: id/res0/res1 then entryCount then entryCount u32 flags.
        return Ok(None);
    }

    let type_id = rd_u8(d, off + 8)?;
    let flags = rd_u8(d, off + 9)?;
    let entry_count = rd_u32(d, off + 12)? as usize;
    let entries_start = rd_u32(d, off + 16)? as usize;
    let config_size = rd_u32(d, off + 20)? as usize;

    // The config must fit inside the declared header.
    if config_size > ch.header_size.saturating_sub(20) {
        bail!(
            "arsc: config size {config_size} exceeds header {}",
            ch.header_size
        );
    }
    let config = render_config(slice(d, off + 20, config_size)?);

    let entries_base = off
        .checked_add(entries_start)
        .ok_or_else(|| anyhow::anyhow!("arsc: entries_start overflow"))?;

    let mut map: Vec<(usize, ResEntry)> = Vec::new();

    if flags & TYPE_FLAG_SPARSE != 0 {
        // Pairs of (u16 entry index, u16 byte-offset / 4), `entry_count` of them.
        for i in 0..entry_count {
            let base = off + ch.header_size + i * 4;
            let idx = rd_u16(d, base)? as usize;
            let rel = rd_u16(d, base + 2)? as usize * 4;
            let entry = parse_entry(d, entries_base + rel, idx as u32, pkg)?;
            map.push((idx, entry));
        }
    } else if flags & TYPE_FLAG_OFFSET16 != 0 {
        // u16 offsets / 4; 0xffff marks an absent entry.
        for i in 0..entry_count {
            let raw = rd_u16(d, off + ch.header_size + i * 2)?;
            if raw == 0xffff {
                continue;
            }
            let rel = raw as usize * 4;
            let entry = parse_entry(d, entries_base + rel, i as u32, pkg)?;
            map.push((i, entry));
        }
    } else {
        // u32 offsets; 0xffffffff marks an absent entry.
        for i in 0..entry_count {
            let raw = rd_u32(d, off + ch.header_size + i * 4)?;
            if raw == 0xffff_ffff {
                continue;
            }
            let entry = parse_entry(d, entries_base + raw as usize, i as u32, pkg)?;
            map.push((i, entry));
        }
    }

    let max_idx = map.iter().map(|(i, _)| *i).max();
    let len = max_idx.map(|m| m + 1).unwrap_or(0).max(entry_count);
    let mut entries: Vec<Option<ResEntry>> = vec![None; len];
    for (idx, entry) in map {
        if idx < entries.len() {
            entries[idx] = Some(entry);
        }
    }

    Ok(Some((type_id, ResConfig { config, entries })))
}

/// Parse a single entry at an absolute offset.
fn parse_entry(d: &[u8], off: usize, entry_id: u32, pkg: &Package) -> Result<ResEntry> {
    let _size = rd_u16(d, off)?;
    let flags = rd_u16(d, off + 2)?;
    let key_idx = rd_u32(d, off + 4)? as usize;

    let key = pkg
        .key_strings
        .get(key_idx)
        .cloned()
        .unwrap_or_else(|| format!("key_{key_idx}"));

    if flags & 0x0001 != 0 {
        // COMPLEX: value is an array/map; we only note that it exists.
        return Ok(ResEntry {
            id: entry_id,
            key,
            value: "complex".to_string(),
            is_complex: true,
        });
    }

    // Res_value: size u16, res0 u8, dataType u8, data u32.
    let data_type = rd_u8(d, off + 11)?;
    let data = rd_u32(d, off + 12)?;

    Ok(ResEntry {
        id: entry_id,
        key,
        value: render_value(data_type, data),
        is_complex: false,
    })
}

/// Render a `Res_value` payload. Strings (and unknown indices) reference the
/// global string pool, which is not available here, so string values are
/// returned as a placeholder `@string/0x...` and filled in later by
/// [`ResTable::inline_strings`].
fn render_value(data_type: u8, data: u32) -> String {
    match data_type {
        TYPE_NULL => "null".to_string(),
        TYPE_REFERENCE => format!("@0x{data:08x}"),
        TYPE_ATTRIBUTE => format!("?0x{data:08x}"),
        TYPE_STRING => format!("@string/0x{data:08x}"),
        TYPE_FLOAT => format!("{}", f32::from_bits(data)),
        TYPE_DIMENSION => format!("0x{data:08x}"),
        TYPE_FRACTION => format!("0x{data:08x}"),
        TYPE_INT_DEC => format!("{}", data as i32),
        TYPE_INT_HEX => format!("0x{data:x}"),
        TYPE_INT_BOOLEAN => (data != 0).to_string(),
        TYPE_INT_COLOR_ARGB8 => format!("#{data:08x}"),
        _ => format!("0x{data:08x}"),
    }
}

// ---------------------------------------------------------------------------
// Public entry points.
// ---------------------------------------------------------------------------

/// Decode a complete `resources.arsc` byte slice.
pub fn decode(data: &[u8]) -> Result<ResTable> {
    let ch = chunk_at(data, 0)?;
    if ch.ty != RES_TABLE_TYPE {
        bail!("arsc: not a resource table (type 0x{:04x})", ch.ty);
    }

    let mut table = ResTable::default();
    let end = ch.size.min(data.len());

    let mut off = ch.header_size;
    if off < 8 {
        bail!("arsc: bad table header size {}", ch.header_size);
    }
    while off + 8 <= end {
        let sub = chunk_at(data, off)?;
        if sub.size < 8 || off + sub.size > end {
            break;
        }
        match sub.ty {
            RES_STRING_POOL_TYPE => {
                table.global_strings =
                    parse_string_pool(data, off).map_err(|e| e.context("global string pool"))?;
            }
            RES_TABLE_PACKAGE_TYPE => {
                // A broken package should not poison the whole table.
                if let Ok(pkg) = parse_package(data, off, &sub) {
                    table.packages.push(pkg);
                }
            }
            _ => {}
        }
        off += sub.size;
    }

    table.inline_strings();
    Ok(table)
}

impl ResTable {
    /// Replace `@string/0x...` placeholders with the real global-pool text.
    fn inline_strings(&mut self) {
        // Split the borrow: the pool is read-only, the packages are mutated.
        let globals = &self.global_strings;
        for pkg in &mut self.packages {
            for ty in &mut pkg.types {
                for cfg in &mut ty.configs {
                    for entry in cfg.entries.iter_mut().flatten() {
                        if let Some(hex) = entry.value.strip_prefix("@string/0x") {
                            if let Ok(idx) = u32::from_str_radix(hex, 16) {
                                if let Some(s) = globals.get(idx as usize) {
                                    entry.value = s.clone();
                                }
                            }
                        }
                    }
                }
            }
        }
    }
}

/// Build a `0xPPTTEEEE` resource id.
fn make_id(pkg: u32, ty: u8, entry: u32) -> u32 {
    (pkg << 24) | ((ty as u32) << 16) | (entry & 0xffff)
}

/// Look up one level, returning the raw stored value.
fn lookup_once(table: &ResTable, res_id: u32) -> Option<String> {
    let pkg_id = (res_id >> 24) & 0xff;
    let type_id = (res_id >> 16) & 0xff;
    let entry_id = res_id & 0xffff;

    let pkg = table.packages.iter().find(|p| p.id == pkg_id)?;
    let ty = pkg.types.iter().find(|t| t.id as u32 == type_id)?;
    let cfg = preferred_config(ty)?;
    let entry = cfg.entries.get(entry_id as usize)?.as_ref()?;
    Some(entry.value.clone())
}

/// Prefer the default configuration, falling back to the first one present.
fn preferred_config(ty: &ResType) -> Option<&ResConfig> {
    ty.configs
        .iter()
        .find(|c| c.config == "default")
        .or_else(|| ty.configs.first())
}

/// Resolve a `0xPPTTEEEE` resource id to its (string) value.
///
/// References to other resources are followed up to a small depth so that an
/// app label stored as a reference (as `aapt2` often emits) still resolves to
/// real text. The default configuration is preferred.
pub fn resolve(table: &ResTable, res_id: u32) -> Option<String> {
    let mut id = res_id;
    for _ in 0..16 {
        let value = lookup_once(table, id)?;
        if let Some(hex) = value.strip_prefix("@0x") {
            match u32::from_str_radix(hex, 16) {
                Ok(next) if next != id => {
                    id = next;
                    continue;
                }
                _ => {}
            }
        }
        return Some(value);
    }
    None
}

/// Best-effort resolution of an application label.
///
/// Without the manifest (this module deliberately has no AXML dependency) the
/// exact resource id is unknown, so we look for the conventional label keys
/// in `string`-typed resources, following references. Returns `None` when no
/// plausible label is found.
pub fn app_label(table: &ResTable) -> Option<String> {
    const KEYS: &[&str] = &[
        "app_name",
        "app_label",
        "application_name",
        "application_label",
        "appname",
        "label",
    ];

    // Pass 1: conventional keys on `string`-typed resources.
    for key in KEYS {
        for pkg in &table.packages {
            for ty in &pkg.types {
                if ty.name != "string" {
                    continue;
                }
                if let Some(v) = find_key_in_type(table, pkg, ty, key) {
                    return Some(v);
                }
            }
        }
    }

    // Pass 2: any resource type.
    for key in KEYS {
        for pkg in &table.packages {
            for ty in &pkg.types {
                if let Some(v) = find_key_in_type(table, pkg, ty, key) {
                    return Some(v);
                }
            }
        }
    }

    None
}

fn find_key_in_type(table: &ResTable, pkg: &Package, ty: &ResType, key: &str) -> Option<String> {
    // Default config first, then the rest.
    let mut configs: Vec<&ResConfig> = ty.configs.iter().collect();
    configs.sort_by_key(|c| if c.config == "default" { 0 } else { 1 });
    for cfg in configs {
        for entry in cfg.entries.iter().flatten() {
            if entry.key == key {
                let id = make_id(pkg.id, ty.id, entry.id);
                if let Some(v) = resolve(table, id) {
                    if !v.is_empty() && !v.starts_with('@') && v != "complex" {
                        return Some(v);
                    }
                }
            }
        }
    }
    None
}

// ---------------------------------------------------------------------------
// Tests.
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Read;

    fn load_arsc() -> Vec<u8> {
        let f = std::fs::File::open("testdata/test-app.apk").expect("open test apk");
        let mut zip = zip::ZipArchive::new(f).expect("valid apk zip");
        let mut entry = zip.by_name("resources.arsc").expect("resources.arsc");
        let mut buf = Vec::new();
        entry.read_to_end(&mut buf).expect("read arsc");
        buf
    }

    #[test]
    fn decode_real_table() {
        let data = load_arsc();
        let table = decode(&data).expect("decode resources.arsc");

        assert!(!table.packages.is_empty(), "expected at least one package");
        assert!(
            table.global_strings.len() > 1000,
            "global pool looks too small: {}",
            table.global_strings.len()
        );

        let pkg = &table.packages[0];
        assert_eq!(pkg.id, 0x7f, "app package id");
        assert_eq!(pkg.name, "org.fdroid.fdroid");
        assert!(!pkg.types.is_empty());

        // The `string` type must exist and contain resolvable values.
        let strings = pkg
            .types
            .iter()
            .find(|t| t.name == "string")
            .expect("string type");
        assert!(strings.id > 0);

        // Find any non-complex entry with a literal string value and confirm
        // that `resolve` returns it.
        let mut resolved = 0usize;
        for cfg in &strings.configs {
            for entry in cfg.entries.iter().flatten() {
                if entry.is_complex || entry.value.is_empty() {
                    continue;
                }
                let id = make_id(pkg.id, strings.id, entry.id);
                let got = resolve(&table, id);
                assert!(got.is_some(), "resolve failed for 0x{id:08x}");
                resolved += 1;
            }
        }
        assert!(resolved > 0, "expected at least one resolvable string");
    }

    #[test]
    fn resolves_app_label() {
        let table = decode(&load_arsc()).expect("decode");
        let label = app_label(&table);
        assert_eq!(label.as_deref(), Some("Test App"), "app label: {label:?}");
    }

    #[test]
    fn resolve_follows_reference() {
        let table = decode(&load_arsc()).expect("decode");
        // Resolve the `app_name` entry's id from the table itself (type 1 =
        // "string") and follow it — mirrors how a manifest label reference
        // resolves on a real app.
        let pkg = &table.packages[0];
        let strings_ty = pkg
            .types
            .iter()
            .find(|t| t.name == "string")
            .expect("string type");
        let app_name = strings_ty
            .configs
            .iter()
            .flat_map(|c| c.entries.iter().flatten())
            .find(|e| e.key == "app_name")
            .expect("app_name entry");
        let id = make_id(pkg.id, strings_ty.id, app_name.id);
        let v = resolve(&table, id);
        assert_eq!(v.as_deref(), Some("Test App"));
    }

    #[test]
    fn resolve_unknown_id_is_none() {
        let table = decode(&load_arsc()).expect("decode");
        assert_eq!(resolve(&table, 0xdead_beef), None);
        assert_eq!(resolve(&table, 0), None);
    }

    #[test]
    fn utf8_pool_roundtrip() {
        // Build a one-string UTF-8 pool: "hi" (2 chars, 2 bytes).
        // header(28) + one offset(4) = data at 32.
        let mut d = vec![0u8; 28 + 4 + 4];
        // type = 0x0001, header_size = 28, size = 36
        d[0..2].copy_from_slice(&1u16.to_le_bytes());
        d[2..4].copy_from_slice(&28u16.to_le_bytes());
        d[4..8].copy_from_slice(&36u32.to_le_bytes());
        d[8..12].copy_from_slice(&1u32.to_le_bytes()); // stringCount
        d[12..16].copy_from_slice(&0u32.to_le_bytes()); // styleCount
        d[16..20].copy_from_slice(&UTF8_FLAG.to_le_bytes()); // flags
        d[20..24].copy_from_slice(&32u32.to_le_bytes()); // stringsStart
        d[24..28].copy_from_slice(&0u32.to_le_bytes()); // stylesStart
        d[28..32].copy_from_slice(&0u32.to_le_bytes()); // offset[0] = 0
        d[32] = 2; // char count
        d[33] = 2; // byte count
        d[34] = b'h';
        d[35] = b'i';

        let pool = parse_string_pool(&d, 0).expect("pool");
        assert_eq!(pool, vec!["hi".to_string()]);
    }

    #[test]
    fn utf16_pool_roundtrip() {
        let mut d = vec![0u8; 28 + 4 + 8];
        d[0..2].copy_from_slice(&1u16.to_le_bytes());
        d[2..4].copy_from_slice(&28u16.to_le_bytes());
        d[4..8].copy_from_slice(&40u32.to_le_bytes());
        d[8..12].copy_from_slice(&1u32.to_le_bytes());
        d[16..20].copy_from_slice(&0u32.to_le_bytes()); // flags = 0 (UTF-16)
        d[20..24].copy_from_slice(&32u32.to_le_bytes());
        d[28..32].copy_from_slice(&0u32.to_le_bytes());
        d[32..34].copy_from_slice(&2u16.to_le_bytes()); // len
        d[34..36].copy_from_slice(&(b'o' as u16).to_le_bytes());
        d[36..38].copy_from_slice(&(b'k' as u16).to_le_bytes());
        d[38..40].copy_from_slice(&0u16.to_le_bytes()); // NUL

        let pool = parse_string_pool(&d, 0).expect("pool");
        assert_eq!(pool, vec!["ok".to_string()]);
    }

    #[test]
    fn render_config_default_and_locale() {
        assert_eq!(render_config(&[]), "default");
        assert_eq!(render_config(&[0u8; 4]), "default");

        // size=64, language "en" with no other fields set.
        let mut cfg = vec![0u8; 64];
        cfg[0] = 64;
        cfg[8] = b'e';
        cfg[9] = b'n';
        assert_eq!(render_config(&cfg), "en");
    }

    #[test]
    fn render_config_night_and_density() {
        let mut cfg = vec![0u8; 64];
        cfg[0] = 64;
        cfg[8] = b'e';
        cfg[9] = b'n';
        cfg[14] = 0xe0; // 480 -> xxhdpi (little-endian 0x01e0)
        cfg[15] = 0x01;
        cfg[29] = 0x20; // UI_MODE_NIGHT_YES
        let tag = render_config(&cfg);
        assert!(tag.contains("en"), "{tag}");
        assert!(tag.contains("night"), "{tag}");
        assert!(tag.contains("xxhdpi"), "{tag}");
    }

    #[test]
    fn rejects_garbage() {
        assert!(decode(&[]).is_err());
        assert!(decode(&[0xff, 0xff, 0, 0, 0, 0, 0, 0]).is_err());
        // Wrong chunk type at the root.
        let mut d = [0u8; 16];
        d[0] = 0x99;
        assert!(decode(&d).is_err());
    }
}
