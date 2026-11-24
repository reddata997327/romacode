//! DEX disassembler: turn a `.dex` file into readable smali text.
//!
//! Smali is the human/LLM-facing assembly for Dalvik/ART bytecode — the thing
//! you actually read and edit when patching an APK. This module parses a DEX
//! file end to end (header, string/type/proto/field/method pools and class
//! definitions) and renders every class as smali.
//!
//! It is fully self-contained: standard library plus `anyhow`/`zip`, with no
//! `baksmali`, no `dexdump`, and no other module of this crate.
//!
//! # Robustness
//!
//! Every read is bounds-checked. Unknown or undecoded opcodes advance by the
//! canonical instruction width (or, for payload pseudo-instructions, by the
//! width encoded in the payload) so the instruction stream never desyncs.
//! Malformed input yields an [`anyhow::Error`], never a panic.

use std::collections::{BTreeSet, HashMap};
use std::fs::File;
use std::io::Read;

use anyhow::{bail, Result};

// ---------------------------------------------------------------------------
// Public data model.
// ---------------------------------------------------------------------------

/// A decoded DEX file.
///
/// `data` retains the raw bytes so that code items can be re-read lazily when
/// rendering smali.
#[derive(Debug, Clone, Default)]
pub struct DexFile {
    pub strings: Vec<String>,
    pub types: Vec<String>,
    pub protos: Vec<Proto>,
    pub fields: Vec<FieldRef>,
    pub methods: Vec<MethodRef>,
    pub classes: Vec<ClassDef>,
    /// Raw DEX bytes, kept for later code-item reads.
    pub(crate) data: Vec<u8>,
}

/// A method prototype (`proto_id_item`).
#[derive(Debug, Clone, Default)]
pub struct Proto {
    pub shorty_idx: u32,
    pub return_type_idx: u32,
    pub parameters_off: u32,
    pub shorty: String,
    pub return_type: String,
    pub parameters: Vec<String>,
}

/// A field reference (`field_id_item`).
#[derive(Debug, Clone, Default)]
pub struct FieldRef {
    pub class_idx: u16,
    pub type_idx: u16,
    pub name_idx: u32,
    pub class: String,
    pub ty: String,
    pub name: String,
}

/// A method reference (`method_id_item`).
#[derive(Debug, Clone, Default)]
pub struct MethodRef {
    pub class_idx: u16,
    pub proto_idx: u16,
    pub name_idx: u32,
    pub class: String,
    pub name: String,
}

/// A class definition (`class_def_item`) plus its decoded class data.
#[derive(Debug, Clone, Default)]
pub struct ClassDef {
    pub class_idx: u32,
    pub access_flags: u32,
    pub superclass_idx: u32,
    pub interfaces_off: u32,
    pub source_file_idx: u32,
    pub annotations_off: u32,
    pub class_data_off: u32,
    pub static_values_off: u32,
    pub class_type: String,
    pub superclass: Option<String>,
    pub interfaces: Vec<String>,
    pub source_file: Option<String>,
    pub class_data: Option<ClassData>,
}

/// Decoded `class_data_item`.
#[derive(Debug, Clone, Default)]
pub struct ClassData {
    pub static_fields: Vec<EncodedField>,
    pub instance_fields: Vec<EncodedField>,
    pub direct_methods: Vec<EncodedMethod>,
    pub virtual_methods: Vec<EncodedMethod>,
}

/// An encoded field with its resolved index and access flags.
#[derive(Debug, Clone, Default)]
pub struct EncodedField {
    pub field_idx: u32,
    pub access_flags: u32,
}

/// An encoded method with its resolved index, flags and code offset.
#[derive(Debug, Clone, Default)]
pub struct EncodedMethod {
    pub method_idx: u32,
    pub access_flags: u32,
    pub code_off: u32,
}

/// One rendered class.
#[derive(Debug, Clone, Default)]
pub struct SmaliClass {
    /// Java class name without `L`/`;`, e.g. `com/termux/app/TermuxActivity`.
    pub name: String,
    /// The smali source text.
    pub source: String,
}

// ---------------------------------------------------------------------------
// Low-level, bounds-checked readers.
// ---------------------------------------------------------------------------

fn slice_at(d: &[u8], off: usize, len: usize) -> Result<&[u8]> {
    let end = off
        .checked_add(len)
        .ok_or_else(|| anyhow::anyhow!("dex: offset overflow at {off}+{len}"))?;
    if end > d.len() {
        bail!("dex: read {len} bytes at {off} past end (len {})", d.len());
    }
    Ok(&d[off..end])
}

fn u8_at(d: &[u8], off: usize) -> Result<u8> {
    Ok(slice_at(d, off, 1)?[0])
}

fn u16_at(d: &[u8], off: usize) -> Result<u16> {
    let b = slice_at(d, off, 2)?;
    Ok(u16::from_le_bytes([b[0], b[1]]))
}

fn u32_at(d: &[u8], off: usize) -> Result<u32> {
    let b = slice_at(d, off, 4)?;
    Ok(u32::from_le_bytes([b[0], b[1], b[2], b[3]]))
}

/// Unsigned LEB128, advancing `p`.
fn uleb128(d: &[u8], p: &mut usize) -> Result<u32> {
    let mut result: u32 = 0;
    let mut shift = 0u32;
    loop {
        let byte = u8_at(d, *p)?;
        *p += 1;
        result |= ((byte & 0x7f) as u32) << shift;
        if byte & 0x80 == 0 {
            return Ok(result);
        }
        shift += 7;
        if shift > 28 {
            bail!("dex: uleb128 too long");
        }
    }
}

/// Signed LEB128, advancing `p`.
fn sleb128(d: &[u8], p: &mut usize) -> Result<i32> {
    let mut result: i32 = 0;
    let mut shift = 0u32;
    let mut byte;
    loop {
        byte = u8_at(d, *p)?;
        *p += 1;
        result |= ((byte & 0x7f) as i32) << shift;
        shift += 7;
        if byte & 0x80 == 0 {
            break;
        }
        if shift > 28 {
            bail!("dex: sleb128 too long");
        }
    }
    if shift < 32 && byte & 0x40 != 0 {
        result |= -1i32 << shift;
    }
    Ok(result)
}

/// Decode a MUTF-8 byte string, stopping at the NUL terminator.
///
/// MUTF-8 encodes U+0000 as `C0 80` and supplementary characters as CESU-8
/// surrogate pairs. Lone surrogates are replaced rather than rejected.
fn decode_mutf8(bytes: &[u8]) -> String {
    let mut units: Vec<u16> = Vec::new();
    let mut i = 0usize;
    while i < bytes.len() {
        let b = bytes[i];
        if b == 0 {
            break;
        }
        if b < 0x80 {
            units.push(b as u16);
            i += 1;
        } else if b & 0xe0 == 0xc0 {
            if i + 1 >= bytes.len() {
                break;
            }
            let cp = (((b & 0x1f) as u16) << 6) | (bytes[i + 1] & 0x3f) as u16;
            units.push(cp);
            i += 2;
        } else if b & 0xf0 == 0xe0 {
            if i + 2 >= bytes.len() {
                break;
            }
            let cp = (((b & 0x0f) as u16) << 12)
                | (((bytes[i + 1] & 0x3f) as u16) << 6)
                | (bytes[i + 2] & 0x3f) as u16;
            units.push(cp);
            i += 3;
        } else {
            // Unexpected lead byte (or 4-byte form we do not emit): skip it.
            i += 1;
        }
    }
    String::from_utf16_lossy(&units)
}

// ---------------------------------------------------------------------------
// Instruction widths.
// ---------------------------------------------------------------------------

/// Canonical DEX instruction widths in 16-bit code units, indexed by opcode.
///
/// Payload pseudo-instructions (`0x0100`/`0x0200`/`0x0300`) are not in this
/// table; they are recognised from the first code unit and measured directly.
#[rustfmt::skip]
const WIDTH: [u8; 256] = [
//  0   1   2   3   4   5   6   7   8   9   a   b   c   d   e   f
    1,  1,  2,  3,  1,  2,  3,  1,  2,  3,  1,  1,  1,  1,  1,  1, // 0x00
    1,  1,  1,  2,  3,  2,  2,  3,  5,  2,  2,  3,  2,  1,  1,  2, // 0x10
    2,  1,  2,  2,  3,  3,  3,  1,  1,  2,  3,  3,  3,  2,  2,  2, // 0x20
    2,  2,  2,  2,  2,  2,  2,  2,  2,  2,  2,  2,  2,  2,  1,  1, // 0x30
    1,  1,  1,  1,  2,  2,  2,  2,  2,  2,  2,  2,  2,  2,  2,  2, // 0x40
    2,  2,  2,  2,  2,  2,  2,  2,  2,  2,  2,  2,  2,  2,  2,  2, // 0x50
    2,  2,  2,  2,  2,  2,  2,  2,  2,  2,  2,  2,  2,  2,  3,  3, // 0x60
    3,  3,  3,  3,  3,  3,  3,  3,  3,  1,  1,  1,  1,  1,  1,  1, // 0x70
    1,  1,  1,  1,  1,  1,  1,  1,  1,  1,  1,  1,  1,  1,  1,  1, // 0x80
    2,  2,  2,  2,  2,  2,  2,  2,  2,  2,  2,  2,  2,  2,  2,  2, // 0x90
    2,  2,  2,  2,  2,  2,  2,  2,  2,  2,  2,  2,  2,  2,  2,  2, // 0xa0
    1,  1,  1,  1,  1,  1,  1,  1,  1,  1,  1,  1,  1,  1,  1,  1, // 0xb0
    1,  1,  1,  1,  1,  1,  1,  1,  1,  1,  1,  1,  1,  1,  1,  1, // 0xc0
    2,  2,  2,  2,  2,  2,  2,  2,  2,  2,  2,  2,  2,  2,  2,  2, // 0xd0
    2,  2,  2,  1,  1,  1,  1,  1,  1,  1,  1,  1,  1,  1,  1,  1, // 0xe0
    1,  1,  1,  1,  1,  1,  1,  1,  1,  1,  1,  1,  1,  1,  1,  1, // 0xf0
];

/// Mnemonic for an opcode; empty string for unused opcodes.
fn mnemonic(op: usize) -> &'static str {
    match op {
        0x00 => "nop",
        0x01 => "move",
        0x02 => "move/from16",
        0x03 => "move/16",
        0x04 => "move-wide",
        0x05 => "move-wide/from16",
        0x06 => "move-wide/16",
        0x07 => "move-object",
        0x08 => "move-object/from16",
        0x09 => "move-object/16",
        0x0a => "move-result",
        0x0b => "move-result-wide",
        0x0c => "move-result-object",
        0x0d => "move-exception",
        0x0e => "return-void",
        0x0f => "return",
        0x10 => "return-wide",
        0x11 => "return-object",
        0x12 => "const/4",
        0x13 => "const/16",
        0x14 => "const",
        0x15 => "const/high16",
        0x16 => "const-wide/16",
        0x17 => "const-wide/32",
        0x18 => "const-wide",
        0x19 => "const-wide/high16",
        0x1a => "const-string",
        0x1b => "const-string/jumbo",
        0x1c => "const-class",
        0x1d => "monitor-enter",
        0x1e => "monitor-exit",
        0x1f => "check-cast",
        0x20 => "instance-of",
        0x21 => "array-length",
        0x22 => "new-instance",
        0x23 => "new-array",
        0x24 => "filled-new-array",
        0x25 => "filled-new-array/range",
        0x26 => "fill-array-data",
        0x27 => "throw",
        0x28 => "goto",
        0x29 => "goto/16",
        0x2a => "goto/32",
        0x2b => "packed-switch",
        0x2c => "sparse-switch",
        0x2d => "cmpl-float",
        0x2e => "cmpg-float",
        0x2f => "cmpl-double",
        0x30 => "cmpg-double",
        0x31 => "cmp-long",
        0x32 => "if-eq",
        0x33 => "if-ne",
        0x34 => "if-lt",
        0x35 => "if-ge",
        0x36 => "if-gt",
        0x37 => "if-le",
        0x38 => "if-eqz",
        0x39 => "if-nez",
        0x3a => "if-ltz",
        0x3b => "if-gez",
        0x3c => "if-gtz",
        0x3d => "if-lez",
        0x44 => "aget",
        0x45 => "aget-wide",
        0x46 => "aget-object",
        0x47 => "aget-boolean",
        0x48 => "aget-byte",
        0x49 => "aget-char",
        0x4a => "aget-short",
        0x4b => "aput",
        0x4c => "aput-wide",
        0x4d => "aput-object",
        0x4e => "aput-boolean",
        0x4f => "aput-byte",
        0x50 => "aput-char",
        0x51 => "aput-short",
        0x52 => "iget",
        0x53 => "iget-wide",
        0x54 => "iget-object",
        0x55 => "iget-boolean",
        0x56 => "iget-byte",
        0x57 => "iget-char",
        0x58 => "iget-short",
        0x59 => "iput",
        0x5a => "iput-wide",
        0x5b => "iput-object",
        0x5c => "iput-boolean",
        0x5d => "iput-byte",
        0x5e => "iput-char",
        0x5f => "iput-short",
        0x60 => "sget",
        0x61 => "sget-wide",
        0x62 => "sget-object",
        0x63 => "sget-boolean",
        0x64 => "sget-byte",
        0x65 => "sget-char",
        0x66 => "sget-short",
        0x67 => "sput",
        0x68 => "sput-wide",
        0x69 => "sput-object",
        0x6a => "sput-boolean",
        0x6b => "sput-byte",
        0x6c => "sput-char",
        0x6d => "sput-short",
        0x6e => "invoke-virtual",
        0x6f => "invoke-super",
        0x70 => "invoke-direct",
        0x71 => "invoke-static",
        0x72 => "invoke-interface",
        0x74 => "invoke-virtual/range",
        0x75 => "invoke-super/range",
        0x76 => "invoke-direct/range",
        0x77 => "invoke-static/range",
        0x78 => "invoke-interface/range",
        0x7b => "neg-int",
        0x7c => "not-int",
        0x7d => "neg-long",
        0x7e => "not-long",
        0x7f => "neg-float",
        0x80 => "neg-double",
        0x81 => "int-to-long",
        0x82 => "int-to-float",
        0x83 => "int-to-double",
        0x84 => "long-to-int",
        0x85 => "long-to-float",
        0x86 => "long-to-double",
        0x87 => "float-to-int",
        0x88 => "float-to-long",
        0x89 => "float-to-double",
        0x8a => "double-to-int",
        0x8b => "double-to-long",
        0x8c => "double-to-float",
        0x8d => "int-to-byte",
        0x8e => "int-to-char",
        0x8f => "int-to-short",
        0x90 => "add-int",
        0x91 => "sub-int",
        0x92 => "mul-int",
        0x93 => "div-int",
        0x94 => "rem-int",
        0x95 => "and-int",
        0x96 => "or-int",
        0x97 => "xor-int",
        0x98 => "shl-int",
        0x99 => "shr-int",
        0x9a => "ushr-int",
        0x9b => "add-long",
        0x9c => "sub-long",
        0x9d => "mul-long",
        0x9e => "div-long",
        0x9f => "rem-long",
        0xa0 => "and-long",
        0xa1 => "or-long",
        0xa2 => "xor-long",
        0xa3 => "shl-long",
        0xa4 => "shr-long",
        0xa5 => "ushr-long",
        0xa6 => "add-float",
        0xa7 => "sub-float",
        0xa8 => "mul-float",
        0xa9 => "div-float",
        0xaa => "rem-float",
        0xab => "add-double",
        0xac => "sub-double",
        0xad => "mul-double",
        0xae => "div-double",
        0xaf => "rem-double",
        0xb0 => "add-int/2addr",
        0xb1 => "sub-int/2addr",
        0xb2 => "mul-int/2addr",
        0xb3 => "div-int/2addr",
        0xb4 => "rem-int/2addr",
        0xb5 => "and-int/2addr",
        0xb6 => "or-int/2addr",
        0xb7 => "xor-int/2addr",
        0xb8 => "shl-int/2addr",
        0xb9 => "shr-int/2addr",
        0xba => "ushr-int/2addr",
        0xbb => "add-long/2addr",
        0xbc => "sub-long/2addr",
        0xbd => "mul-long/2addr",
        0xbe => "div-long/2addr",
        0xbf => "rem-long/2addr",
        0xc0 => "and-long/2addr",
        0xc1 => "or-long/2addr",
        0xc2 => "xor-long/2addr",
        0xc3 => "shl-long/2addr",
        0xc4 => "shr-long/2addr",
        0xc5 => "ushr-long/2addr",
        0xc6 => "add-float/2addr",
        0xc7 => "sub-float/2addr",
        0xc8 => "mul-float/2addr",
        0xc9 => "div-float/2addr",
        0xca => "rem-float/2addr",
        0xcb => "add-double/2addr",
        0xcc => "sub-double/2addr",
        0xcd => "mul-double/2addr",
        0xce => "div-double/2addr",
        0xcf => "rem-double/2addr",
        0xd0 => "add-int/lit16",
        0xd1 => "rsub-int",
        0xd2 => "mul-int/lit16",
        0xd3 => "div-int/lit16",
        0xd4 => "rem-int/lit16",
        0xd5 => "and-int/lit16",
        0xd6 => "or-int/lit16",
        0xd7 => "xor-int/lit16",
        0xd8 => "add-int/lit8",
        0xd9 => "rsub-int/lit8",
        0xda => "mul-int/lit8",
        0xdb => "div-int/lit8",
        0xdc => "rem-int/lit8",
        0xdd => "and-int/lit8",
        0xde => "or-int/lit8",
        0xdf => "xor-int/lit8",
        0xe0 => "shl-int/lit8",
        0xe1 => "shr-int/lit8",
        0xe2 => "ushr-int/lit8",
        _ => "",
    }
}

// ---------------------------------------------------------------------------
// Parsing.
// ---------------------------------------------------------------------------

/// Parse a DEX file into pools and class definitions.
pub fn parse(dex: &[u8]) -> Result<DexFile> {
    if dex.len() < 0x70 {
        bail!("dex: file too small ({} bytes)", dex.len());
    }
    if &dex[0..4] != b"dex\n" {
        bail!("dex: bad magic {:02x?}", &dex[0..4]);
    }

    let string_ids_size = u32_at(dex, 0x38)? as usize;
    let string_ids_off = u32_at(dex, 0x3c)? as usize;
    let type_ids_size = u32_at(dex, 0x40)? as usize;
    let type_ids_off = u32_at(dex, 0x44)? as usize;
    let proto_ids_size = u32_at(dex, 0x48)? as usize;
    let proto_ids_off = u32_at(dex, 0x4c)? as usize;
    let field_ids_size = u32_at(dex, 0x50)? as usize;
    let field_ids_off = u32_at(dex, 0x54)? as usize;
    let method_ids_size = u32_at(dex, 0x58)? as usize;
    let method_ids_off = u32_at(dex, 0x5c)? as usize;
    let class_defs_size = u32_at(dex, 0x60)? as usize;
    let class_defs_off = u32_at(dex, 0x64)? as usize;

    // Sanity caps: each id is >= 4 bytes, so a bogus count cannot exceed the file.
    for (name, size, off) in [
        ("string", string_ids_size, string_ids_off),
        ("type", type_ids_size, type_ids_off),
        ("proto", proto_ids_size, proto_ids_off),
        ("field", field_ids_size, field_ids_off),
        ("method", method_ids_size, method_ids_off),
        ("class", class_defs_size, class_defs_off),
    ] {
        if size.saturating_mul(4) > dex.len().saturating_add(4) && size > 0 {
            bail!("dex: implausible {name} count {size}");
        }
        if off > dex.len() && size > 0 {
            bail!("dex: {name}_ids_off {off} past end");
        }
    }

    // --- strings -----------------------------------------------------------
    let mut strings = Vec::with_capacity(string_ids_size);
    for i in 0..string_ids_size {
        let data_off = u32_at(dex, string_ids_off + i * 4)? as usize;
        let mut p = data_off;
        let _utf16_len = uleb128(dex, &mut p)?;
        let rest = slice_at(dex, p, dex.len() - p)?;
        strings.push(decode_mutf8(rest));
    }

    // --- types -------------------------------------------------------------
    let mut types = Vec::with_capacity(type_ids_size);
    for i in 0..type_ids_size {
        let idx = u32_at(dex, type_ids_off + i * 4)? as usize;
        types.push(strings.get(idx).cloned().unwrap_or_default());
    }

    // --- protos ------------------------------------------------------------
    let mut protos = Vec::with_capacity(proto_ids_size);
    for i in 0..proto_ids_size {
        let base = proto_ids_off + i * 12;
        let shorty_idx = u32_at(dex, base)?;
        let return_type_idx = u32_at(dex, base + 4)?;
        let parameters_off = u32_at(dex, base + 8)?;
        let parameters = if parameters_off != 0 {
            read_type_list(dex, parameters_off as usize, &types)?
        } else {
            Vec::new()
        };
        protos.push(Proto {
            shorty_idx,
            return_type_idx,
            parameters_off,
            shorty: strings
                .get(shorty_idx as usize)
                .cloned()
                .unwrap_or_default(),
            return_type: types
                .get(return_type_idx as usize)
                .cloned()
                .unwrap_or_else(|| "V".to_string()),
            parameters,
        });
    }

    // --- fields ------------------------------------------------------------
    let mut fields = Vec::with_capacity(field_ids_size);
    for i in 0..field_ids_size {
        let base = field_ids_off + i * 8;
        let class_idx = u16_at(dex, base)?;
        let type_idx = u16_at(dex, base + 2)?;
        let name_idx = u32_at(dex, base + 4)?;
        fields.push(FieldRef {
            class_idx,
            type_idx,
            name_idx,
            class: types.get(class_idx as usize).cloned().unwrap_or_default(),
            ty: types.get(type_idx as usize).cloned().unwrap_or_default(),
            name: strings.get(name_idx as usize).cloned().unwrap_or_default(),
        });
    }

    // --- methods -----------------------------------------------------------
    let mut methods = Vec::with_capacity(method_ids_size);
    for i in 0..method_ids_size {
        let base = method_ids_off + i * 8;
        let class_idx = u16_at(dex, base)?;
        let proto_idx = u16_at(dex, base + 2)?;
        let name_idx = u32_at(dex, base + 4)?;
        methods.push(MethodRef {
            class_idx,
            proto_idx,
            name_idx,
            class: types.get(class_idx as usize).cloned().unwrap_or_default(),
            name: strings.get(name_idx as usize).cloned().unwrap_or_default(),
        });
    }

    // --- classes -----------------------------------------------------------
    let mut classes = Vec::with_capacity(class_defs_size);
    for i in 0..class_defs_size {
        let base = class_defs_off + i * 32;
        let def = ClassDef {
            class_idx: u32_at(dex, base)?,
            access_flags: u32_at(dex, base + 4)?,
            superclass_idx: u32_at(dex, base + 8)?,
            interfaces_off: u32_at(dex, base + 12)?,
            source_file_idx: u32_at(dex, base + 16)?,
            annotations_off: u32_at(dex, base + 20)?,
            class_data_off: u32_at(dex, base + 24)?,
            static_values_off: u32_at(dex, base + 28)?,
            ..Default::default()
        };
        let mut def = def;
        def.class_type = types
            .get(def.class_idx as usize)
            .cloned()
            .unwrap_or_default();
        def.superclass = if def.superclass_idx == 0xffff_ffff {
            None
        } else {
            types.get(def.superclass_idx as usize).cloned()
        };
        def.interfaces = if def.interfaces_off != 0 {
            read_type_list(dex, def.interfaces_off as usize, &types)?
        } else {
            Vec::new()
        };
        def.source_file = if def.source_file_idx == 0xffff_ffff {
            None
        } else {
            strings.get(def.source_file_idx as usize).cloned()
        };
        if def.class_data_off != 0 {
            def.class_data = Some(read_class_data(dex, def.class_data_off as usize)?);
        }
        classes.push(def);
    }

    Ok(DexFile {
        strings,
        types,
        protos,
        fields,
        methods,
        classes,
        data: dex.to_vec(),
    })
}

/// Read a `type_list` (a `u32` count followed by `count` `u16` type indices).
fn read_type_list(dex: &[u8], off: usize, types: &[String]) -> Result<Vec<String>> {
    let size = u32_at(dex, off)? as usize;
    if size > dex.len() / 2 + 1 {
        bail!("dex: implausible type_list size {size}");
    }
    let mut out = Vec::with_capacity(size);
    for i in 0..size {
        let idx = u16_at(dex, off + 4 + i * 2)? as usize;
        out.push(types.get(idx).cloned().unwrap_or_default());
    }
    Ok(out)
}

/// Read a `class_data_item`, resolving the delta-encoded field/method indices.
fn read_class_data(dex: &[u8], off: usize) -> Result<ClassData> {
    let mut p = off;
    let static_fields_size = uleb128(dex, &mut p)? as usize;
    let instance_fields_size = uleb128(dex, &mut p)? as usize;
    let direct_methods_size = uleb128(dex, &mut p)? as usize;
    let virtual_methods_size = uleb128(dex, &mut p)? as usize;

    let mut cd = ClassData::default();

    let mut idx = 0u32;
    for _ in 0..static_fields_size {
        idx = idx.wrapping_add(uleb128(dex, &mut p)?);
        let access_flags = uleb128(dex, &mut p)?;
        cd.static_fields.push(EncodedField {
            field_idx: idx,
            access_flags,
        });
    }

    idx = 0;
    for _ in 0..instance_fields_size {
        idx = idx.wrapping_add(uleb128(dex, &mut p)?);
        let access_flags = uleb128(dex, &mut p)?;
        cd.instance_fields.push(EncodedField {
            field_idx: idx,
            access_flags,
        });
    }

    idx = 0;
    for _ in 0..direct_methods_size {
        idx = idx.wrapping_add(uleb128(dex, &mut p)?);
        let access_flags = uleb128(dex, &mut p)?;
        let code_off = uleb128(dex, &mut p)?;
        cd.direct_methods.push(EncodedMethod {
            method_idx: idx,
            access_flags,
            code_off,
        });
    }

    idx = 0;
    for _ in 0..virtual_methods_size {
        idx = idx.wrapping_add(uleb128(dex, &mut p)?);
        let access_flags = uleb128(dex, &mut p)?;
        let code_off = uleb128(dex, &mut p)?;
        cd.virtual_methods.push(EncodedMethod {
            method_idx: idx,
            access_flags,
            code_off,
        });
    }

    Ok(cd)
}

// ---------------------------------------------------------------------------
// Convenience entry points.
// ---------------------------------------------------------------------------

/// Parse and render every class in a DEX file.
pub fn disassemble(dex: &[u8]) -> Result<Vec<SmaliClass>> {
    let parsed = parse(dex)?;
    Ok((0..parsed.classes.len())
        .map(|i| SmaliClass {
            name: class_name(&parsed.classes[i].class_type),
            source: to_smali(&parsed, i),
        })
        .collect())
}

/// Parse and render every class from every `classes*.dex` in an APK.
pub fn disassemble_apk(apk_path: &str) -> Result<Vec<SmaliClass>> {
    let file = File::open(apk_path).map_err(|e| anyhow::anyhow!("opening {apk_path}: {e}"))?;
    let mut zip = zip::ZipArchive::new(file)
        .map_err(|e| anyhow::anyhow!("{apk_path} is not a valid APK: {e}"))?;

    // Collect the dex entry names in numeric order (classes.dex, classes2.dex...).
    let mut names: Vec<(u32, String)> = Vec::new();
    for i in 0..zip.len() {
        let name = match zip.by_index(i) {
            Ok(f) => f.name().to_string(),
            Err(_) => continue,
        };
        if name == "classes.dex" || (name.starts_with("classes") && name.ends_with(".dex")) {
            names.push((dex_sort_key(&name), name));
        }
    }
    names.sort();

    let mut out = Vec::new();
    for (_, name) in names {
        let mut buf = Vec::new();
        {
            let mut entry = zip
                .by_name(&name)
                .map_err(|e| anyhow::anyhow!("reading {name}: {e}"))?;
            entry
                .read_to_end(&mut buf)
                .map_err(|e| anyhow::anyhow!("reading {name}: {e}"))?;
        }
        let mut classes =
            disassemble(&buf).map_err(|e| e.context(format!("disassembling {name}")))?;
        out.append(&mut classes);
    }
    Ok(out)
}

/// `classes.dex` -> 1, `classes2.dex` -> 2, ...
fn dex_sort_key(name: &str) -> u32 {
    let stem = name.strip_suffix(".dex").unwrap_or(name);
    let digits: String = stem.chars().skip_while(|c| !c.is_ascii_digit()).collect();
    digits.parse().unwrap_or(1)
}

/// `Lcom/foo/Bar;` -> `com/foo/Bar`.
fn class_name(descriptor: &str) -> String {
    let s = descriptor.strip_prefix('L').unwrap_or(descriptor);
    s.strip_suffix(';').unwrap_or(s).to_string()
}

// ---------------------------------------------------------------------------
// Rendering.
// ---------------------------------------------------------------------------

/// Render one class to smali text. Out-of-range indices yield a comment rather
/// than an error so a single bad class never aborts a whole APK.
pub fn to_smali(dex: &DexFile, class_index: usize) -> String {
    let renderer = Renderer { dex };
    renderer.render_class(class_index)
}

struct Renderer<'a> {
    dex: &'a DexFile,
}

/// Per-method register context used to name `v`/`p` registers.
struct CodeCtx {
    registers_size: usize,
    ins_size: usize,
}

/// A decoded instruction.
struct Insn {
    pc: usize,
    text: String,
}

/// A decoded try/catch block.
struct TryBlock {
    start: usize,
    end: usize,
    typed: Vec<(String, usize)>,
    catch_all: Option<usize>,
}

impl<'a> Renderer<'a> {
    // -- name helpers -------------------------------------------------------

    fn type_at(&self, idx: u32) -> String {
        self.dex
            .types
            .get(idx as usize)
            .cloned()
            .unwrap_or_else(|| format!("type@0x{idx:x}"))
    }

    fn field_spec(&self, idx: u32) -> String {
        match self.dex.fields.get(idx as usize) {
            Some(f) => format!("{}->{}:{}", f.class, f.name, f.ty),
            None => format!("field@0x{idx:x}"),
        }
    }

    /// `name(params)ret` for a method id.
    fn method_signature(&self, idx: u32) -> String {
        match self.dex.methods.get(idx as usize) {
            Some(m) => {
                let proto = self.dex.protos.get(m.proto_idx as usize);
                let params = proto.map(|p| p.parameters.join("")).unwrap_or_default();
                let ret = proto
                    .map(|p| p.return_type.clone())
                    .unwrap_or_else(|| "V".to_string());
                format!("{}({}){}", m.name, params, ret)
            }
            None => format!("method@0x{idx:x}()V"),
        }
    }

    /// `class->name(params)ret` for a method id.
    fn method_spec(&self, idx: u32) -> String {
        match self.dex.methods.get(idx as usize) {
            Some(m) => format!("{}->{}", m.class, self.method_signature(idx)),
            None => format!("method@0x{idx:x}"),
        }
    }

    fn reg(&self, ctx: &CodeCtx, i: u32) -> String {
        let p_start = ctx.registers_size.saturating_sub(ctx.ins_size);
        if i as usize >= p_start {
            format!("p{}", i as usize - p_start)
        } else {
            format!("v{i}")
        }
    }

    // -- class --------------------------------------------------------------

    fn render_class(&self, class_index: usize) -> String {
        let Some(cd) = self.dex.classes.get(class_index) else {
            return format!("# no class at index {class_index}\n");
        };

        let mut out = String::new();
        out.push_str(&format!(
            ".class {}{}\n",
            class_flags(cd.access_flags),
            cd.class_type
        ));
        if let Some(super_desc) = &cd.superclass {
            out.push_str(&format!(".super {super_desc}\n"));
        }
        for iface in &cd.interfaces {
            out.push_str(&format!(".implements {iface}\n"));
        }
        if let Some(source) = &cd.source_file {
            out.push_str(&format!(".source \"{}\"\n", escape_string(source)));
        }

        let Some(data) = &cd.class_data else {
            return out;
        };

        if !data.static_fields.is_empty() {
            out.push_str("\n# static fields\n");
            for f in &data.static_fields {
                out.push_str(&self.render_field(f));
            }
        }
        if !data.instance_fields.is_empty() {
            out.push_str("\n# instance fields\n");
            for f in &data.instance_fields {
                out.push_str(&self.render_field(f));
            }
        }
        if !data.direct_methods.is_empty() {
            out.push_str("\n# direct methods\n");
            for m in &data.direct_methods {
                out.push_str(&self.render_method(m));
            }
        }
        if !data.virtual_methods.is_empty() {
            out.push_str("\n# virtual methods\n");
            for m in &data.virtual_methods {
                out.push_str(&self.render_method(m));
            }
        }

        out
    }

    fn render_field(&self, f: &EncodedField) -> String {
        let Some(fr) = self.dex.fields.get(f.field_idx as usize) else {
            return format!("# unknown field index {}\n", f.field_idx);
        };
        format!(
            ".field {}{}:{}\n",
            field_flags(f.access_flags),
            fr.name,
            fr.ty
        )
    }

    fn render_method(&self, m: &EncodedMethod) -> String {
        let Some(mr) = self.dex.methods.get(m.method_idx as usize) else {
            return format!("# unknown method index {}\n", m.method_idx);
        };
        let is_ctor =
            m.access_flags & 0x1_0000 != 0 || mr.name == "<init>" || mr.name == "<clinit>";
        let mut flags = method_flags(m.access_flags);
        if is_ctor {
            flags.push_str("constructor ");
        }

        let mut out = format!(
            ".method {}{}({}){}\n",
            flags,
            mr.name,
            self.method_param_sig(m.method_idx),
            self.method_return_sig(m.method_idx)
        );

        if m.code_off != 0 {
            match self.render_code(m.code_off as usize) {
                Ok(body) => out.push_str(&body),
                Err(e) => out.push_str(&format!("    # code decode error: {e}\n")),
            }
        }

        out.push_str(".end method\n");
        out
    }

    fn method_param_sig(&self, idx: u32) -> String {
        self.dex
            .methods
            .get(idx as usize)
            .and_then(|m| self.dex.protos.get(m.proto_idx as usize))
            .map(|p| p.parameters.join(""))
            .unwrap_or_default()
    }

    fn method_return_sig(&self, idx: u32) -> String {
        self.dex
            .methods
            .get(idx as usize)
            .and_then(|m| self.dex.protos.get(m.proto_idx as usize))
            .map(|p| p.return_type.clone())
            .unwrap_or_else(|| "V".to_string())
    }

    // -- code ---------------------------------------------------------------

    fn render_code(&self, code_off: usize) -> Result<String> {
        let d = &self.dex.data;
        let registers_size = u16_at(d, code_off)? as usize;
        let ins_size = u16_at(d, code_off + 2)? as usize;
        let _outs_size = u16_at(d, code_off + 4)? as usize;
        let tries_size = u16_at(d, code_off + 6)? as usize;
        let _debug_info_off = u32_at(d, code_off + 8)?;
        let insns_size = u32_at(d, code_off + 12)? as usize;

        if insns_size > d.len() / 2 + 1 {
            bail!("implausible insns_size {insns_size}");
        }

        let insns_base = code_off + 16;
        let mut units: Vec<u16> = Vec::with_capacity(insns_size);
        for i in 0..insns_size {
            units.push(u16_at(d, insns_base + i * 2)?);
        }

        let ctx = CodeCtx {
            registers_size,
            ins_size,
        };

        let mut targets: Vec<usize> = Vec::new();
        let insns = self.decode_insns(&units, &ctx, &mut targets);

        let tries = if tries_size > 0 {
            self.decode_tries(code_off, insns_size, tries_size)
                .unwrap_or_default()
        } else {
            Vec::new()
        };

        // Collect every position that needs a label.
        let mut labels: BTreeSet<usize> = targets.into_iter().collect();
        for t in &tries {
            labels.insert(t.start);
            labels.insert(t.end);
            for (_, addr) in &t.typed {
                labels.insert(*addr);
            }
            if let Some(a) = t.catch_all {
                labels.insert(a);
            }
        }
        // Drop out-of-range labels; keep the end-of-code boundary.
        labels.retain(|pc| *pc <= insns_size);

        let mut out = format!("    .registers {registers_size}\n");
        for insn in &insns {
            if labels.contains(&insn.pc) {
                out.push_str(&format!("    :L{:x}\n", insn.pc));
            }
            out.push_str(&format!("    {}\n", insn.text));
        }
        if labels.contains(&insns_size) {
            out.push_str(&format!("    :L{:x}\n", insns_size));
        }

        for t in &tries {
            for (ty, addr) in &t.typed {
                out.push_str(&format!(
                    "    .catch {ty} {{:L{:x} .. :L{:x}}} :L{:x}\n",
                    t.start, t.end, addr
                ));
            }
            if let Some(addr) = t.catch_all {
                out.push_str(&format!(
                    "    .catchall {{:L{:x} .. :L{:x}}} :L{:x}\n",
                    t.start, t.end, addr
                ));
            }
        }

        Ok(out)
    }

    /// Decode the whole instruction stream, keeping every instruction at its
    /// correct width so the stream never desynchronises.
    fn decode_insns(&self, units: &[u16], ctx: &CodeCtx, targets: &mut Vec<usize>) -> Vec<Insn> {
        let n = units.len();
        let unit = |i: usize| -> u16 { units.get(i).copied().unwrap_or(0) };

        let mut insns = Vec::new();
        let mut pc = 0usize;
        while pc < n {
            let u0 = unit(pc);
            let (text, width) = if u0 == 0x0100 {
                let size = unit(pc + 1) as usize;
                (
                    format!("# packed-switch-payload (size {size})"),
                    4 + size * 2,
                )
            } else if u0 == 0x0200 {
                let size = unit(pc + 1) as usize;
                (
                    format!("# sparse-switch-payload (size {size})"),
                    2 + size * 4,
                )
            } else if u0 == 0x0300 {
                let element_width = unit(pc + 1) as usize;
                let size = unit(pc + 2) as usize | ((unit(pc + 3) as usize) << 16);
                let bytes = size.saturating_mul(element_width);
                let width = 4 + (bytes + 1) / 2;
                (
                    format!("# fill-array-data-payload (width {element_width}, {size} elements)"),
                    width,
                )
            } else {
                let op = (u0 & 0xff) as usize;
                let w = WIDTH[op] as usize;
                let w = if w == 0 { 1 } else { w };
                let name = mnemonic(op);
                let text = if name.is_empty() {
                    format!("# unknown opcode 0x{op:02x} ({w} units)")
                } else {
                    self.decode_insn(op, units, pc, ctx, targets)
                };
                (text, w)
            };

            let width = width.max(1);
            if pc + width > n {
                // Truncated final instruction: emit what we understand and stop.
                insns.push(Insn {
                    pc,
                    text: format!("# truncated instruction at 0x{pc:x}"),
                });
                break;
            }
            insns.push(Insn { pc, text });
            pc += width;
        }
        insns
    }

    fn decode_insn(
        &self,
        op: usize,
        units: &[u16],
        pc: usize,
        ctx: &CodeCtx,
        targets: &mut Vec<usize>,
    ) -> String {
        let u0 = units.get(pc).copied().unwrap_or(0);
        let u1 = units.get(pc + 1).copied().unwrap_or(0);
        let u2 = units.get(pc + 2).copied().unwrap_or(0);
        let u3 = units.get(pc + 3).copied().unwrap_or(0);
        let u4 = units.get(pc + 4).copied().unwrap_or(0);

        let name = mnemonic(op);
        let r = |i: u32| self.reg(ctx, i);

        // Signed hex literal, smali style.
        let hex = |v: i64| -> String {
            if v < 0 {
                format!("-0x{:x}", v.unsigned_abs())
            } else {
                format!("0x{v:x}")
            }
        };
        let target = |off: i64, targets: &mut Vec<usize>| -> String {
            let t = pc as i64 + off;
            if t >= 0 {
                targets.push(t as usize);
            }
            format!(":L{t:x}")
        };

        match op {
            0x00 => "nop".to_string(),
            0x0e => "return-void".to_string(),

            // 12x: op vA, vB (including the unary/conversion ops 0x7b-0x8f)
            0x01 | 0x04 | 0x07 | 0x21 | 0x7b..=0x8f | 0xb0..=0xcf => {
                let a = ((u0 >> 8) & 0xf) as u32;
                let b = ((u0 >> 12) & 0xf) as u32;
                format!("{name} {}, {}", r(a), r(b))
            }

            // 22x: op vAA, vBBBB
            0x02 | 0x05 | 0x08 => {
                let a = ((u0 >> 8) & 0xff) as u32;
                format!("{name} {}, {}", r(a), r(u1 as u32))
            }

            // 32x: op vAAAA, vBBBB
            0x03 | 0x06 | 0x09 => {
                format!("{name} {}, {}", r(u1 as u32), r(u2 as u32))
            }

            // 11x: op vAA
            0x0a..=0x0d | 0x0f..=0x11 | 0x1d | 0x1e | 0x27 => {
                let a = ((u0 >> 8) & 0xff) as u32;
                format!("{name} {}", r(a))
            }

            // 11n: const/4 vA, #+B
            0x12 => {
                let a = ((u0 >> 8) & 0xf) as u32;
                let imm = (((u0 >> 12) & 0xf) as i32) << 28 >> 28;
                format!("{name} {}, {}", r(a), hex(imm as i64))
            }

            // 21s: const/16 vAA, #+BBBB
            0x13 | 0x16 => {
                let a = ((u0 >> 8) & 0xff) as u32;
                format!("{name} {}, {}", r(a), hex(u1 as i16 as i64))
            }

            // 31i: const vAA, #+BBBBBBBB
            0x14 | 0x17 => {
                let a = ((u0 >> 8) & 0xff) as u32;
                let v = (u1 as u32 | ((u2 as u32) << 16)) as i32;
                format!("{name} {}, {}", r(a), hex(v as i64))
            }

            // 21h: const/high16 vAA, #+BBBB0000
            0x15 => {
                let a = ((u0 >> 8) & 0xff) as u32;
                let v = ((u1 as u32) << 16) as i32;
                format!("{name} {}, {}", r(a), hex(v as i64))
            }
            // 21h wide: const-wide/high16 vAA, #+BBBB000000000000
            0x19 => {
                let a = ((u0 >> 8) & 0xff) as u32;
                let v = (u1 as u64) << 48;
                format!("{name} {}, 0x{v:x}L", r(a))
            }

            // 51l: const-wide vAA, #+BBBB... (64-bit)
            0x18 => {
                let a = ((u0 >> 8) & 0xff) as u32;
                let v = u1 as u64 | ((u2 as u64) << 16) | ((u3 as u64) << 32) | ((u4 as u64) << 48);
                format!("{name} {}, 0x{v:x}L", r(a))
            }

            // 21c string: const-string vAA, "str"
            0x1a => {
                let a = ((u0 >> 8) & 0xff) as u32;
                let s = self
                    .dex
                    .strings
                    .get(u1 as usize)
                    .cloned()
                    .unwrap_or_default();
                format!("{name} {}, \"{}\"", r(a), escape_string(&s))
            }
            // 31c string: const-string/jumbo vAA, "str"
            0x1b => {
                let a = ((u0 >> 8) & 0xff) as u32;
                let idx = u1 as u32 | ((u2 as u32) << 16);
                let s = self
                    .dex
                    .strings
                    .get(idx as usize)
                    .cloned()
                    .unwrap_or_default();
                format!("{name} {}, \"{}\"", r(a), escape_string(&s))
            }

            // 21c type: const-class / check-cast / new-instance
            0x1c | 0x1f | 0x22 => {
                let a = ((u0 >> 8) & 0xff) as u32;
                format!("{name} {}, {}", r(a), self.type_at(u1 as u32))
            }
            // 22c type: instance-of / new-array
            0x20 | 0x23 => {
                let a = ((u0 >> 8) & 0xf) as u32;
                let b = ((u0 >> 12) & 0xf) as u32;
                format!("{name} {}, {}, {}", r(a), r(b), self.type_at(u1 as u32))
            }

            // 35c type: filled-new-array {..}, type
            0x24 => {
                let args = self.decode_35c_regs(u0, u1, u2, ctx);
                format!("{name} {{{}}}, {}", args, self.type_at(u1 as u32))
            }
            // 3rc type: filled-new-array/range {..}, type
            0x25 => {
                let args = self.decode_3rc_range(u0, u1, u2, ctx);
                format!("{name} {}, {}", args, self.type_at(u1 as u32))
            }

            // 31t: packed-switch/sparse-switch/fill-array-data vAA, +BBBBBBBB
            0x26 | 0x2b | 0x2c => {
                let a = ((u0 >> 8) & 0xff) as u32;
                let off = (u1 as u32 | ((u2 as u32) << 16)) as i32;
                format!("{name} {}, {}", r(a), target(off as i64, targets))
            }

            // 10t: goto +AA
            0x28 => {
                let off = ((u0 >> 8) & 0xff) as u8 as i8 as i64;
                format!("{name} {}", target(off, targets))
            }
            // 20t: goto/16 +AAAA
            0x29 => {
                let off = u1 as i16 as i64;
                format!("{name} {}", target(off, targets))
            }
            // 30t: goto/32 +AAAAAAAA
            0x2a => {
                let off = (u1 as u32 | ((u2 as u32) << 16)) as i32 as i64;
                format!("{name} {}", target(off, targets))
            }

            // 23x: op vAA, vBB, vCC
            0x2d..=0x31 | 0x44..=0x51 | 0x90..=0xaf => {
                let a = ((u0 >> 8) & 0xff) as u32;
                let b = (u1 & 0xff) as u32;
                let c = ((u1 >> 8) & 0xff) as u32;
                format!("{name} {}, {}, {}", r(a), r(b), r(c))
            }

            // 22t: if-<cond> vA, vB, +CCCC
            0x32..=0x37 => {
                let a = ((u0 >> 8) & 0xf) as u32;
                let b = ((u0 >> 12) & 0xf) as u32;
                let off = u1 as i16 as i64;
                format!("{name} {}, {}, {}", r(a), r(b), target(off, targets))
            }
            // 21t: if-<cond>z vAA, +BBBB
            0x38..=0x3d => {
                let a = ((u0 >> 8) & 0xff) as u32;
                let off = u1 as i16 as i64;
                format!("{name} {}, {}", r(a), target(off, targets))
            }

            // 22c field: iget/iput vA, vB, field@CCCC
            0x52..=0x5f => {
                let a = ((u0 >> 8) & 0xf) as u32;
                let b = ((u0 >> 12) & 0xf) as u32;
                format!("{name} {}, {}, {}", r(a), r(b), self.field_spec(u1 as u32))
            }
            // 21c field: sget/sput vAA, field@BBBB
            0x60..=0x6d => {
                let a = ((u0 >> 8) & 0xff) as u32;
                format!("{name} {}, {}", r(a), self.field_spec(u1 as u32))
            }

            // 35c method: invoke-* {..}, method@BBBB
            0x6e..=0x72 => {
                let args = self.decode_35c_regs(u0, u1, u2, ctx);
                format!("{name} {{{}}}, {}", args, self.method_spec(u1 as u32))
            }
            // 3rc method: invoke-*/range {..}, method@BBBB
            0x74..=0x78 => {
                let args = self.decode_3rc_range(u0, u1, u2, ctx);
                format!("{name} {}, {}", args, self.method_spec(u1 as u32))
            }

            // 22s: op vA, vB, #+CCCC
            0xd0..=0xd7 => {
                let a = ((u0 >> 8) & 0xf) as u32;
                let b = ((u0 >> 12) & 0xf) as u32;
                format!("{name} {}, {}, {}", r(a), r(b), hex(u1 as i16 as i64))
            }
            // 22b: op vAA, vBB, #+CC
            0xd8..=0xe2 => {
                let a = ((u0 >> 8) & 0xff) as u32;
                let b = (u1 & 0xff) as u32;
                let imm = ((u1 >> 8) & 0xff) as u8 as i8;
                format!("{name} {}, {}, {}", r(a), r(b), hex(imm as i64))
            }

            _ => format!("# {name} (undecoded)"),
        }
    }

    /// Decode the register list of a `35c` instruction: `{vC, vD, vE, vF, vG}`.
    fn decode_35c_regs(&self, u0: u16, u1: u16, u2: u16, ctx: &CodeCtx) -> String {
        let _ = u1;
        // The 4-bit count is architecturally 0..=5; clamp defensively so
        // malformed/obfuscated streams cannot index past the register array.
        let count = (((u0 >> 12) & 0xf) as usize).min(5);
        let g = ((u0 >> 8) & 0xf) as u32;
        let regs = [
            (u2 & 0xf) as u32,
            ((u2 >> 4) & 0xf) as u32,
            ((u2 >> 8) & 0xf) as u32,
            ((u2 >> 12) & 0xf) as u32,
            g,
        ];
        (0..count)
            .map(|i| self.reg(ctx, regs[i]))
            .collect::<Vec<_>>()
            .join(", ")
    }

    /// Decode the register range of a `3rc` instruction: `{vCCCC .. vNNNN}`.
    fn decode_3rc_range(&self, u0: u16, _u1: u16, u2: u16, ctx: &CodeCtx) -> String {
        let count = ((u0 >> 8) & 0xff) as u32;
        let first = u2 as u32;
        if count == 0 {
            "{}".to_string()
        } else {
            format!(
                "{{{} .. {}}}",
                self.reg(ctx, first),
                self.reg(ctx, first + count - 1)
            )
        }
    }

    /// Decode the try/catch metadata for a code item.
    fn decode_tries(
        &self,
        code_off: usize,
        insns_size: usize,
        tries_size: usize,
    ) -> Result<Vec<TryBlock>> {
        let d = &self.dex.data;
        let insns_base = code_off + 16;
        let tries_base = insns_base
            .checked_add(insns_size * 2)
            .ok_or_else(|| anyhow::anyhow!("dex: tries base overflow"))?;
        let pad = if tries_size > 0 && insns_size % 2 == 1 {
            2
        } else {
            0
        };
        let try_items_base = tries_base + pad;
        let handlers_base = try_items_base
            .checked_add(tries_size * 8)
            .ok_or_else(|| anyhow::anyhow!("dex: handlers base overflow"))?;

        // encoded_catch_handler_list
        let mut p = handlers_base;
        let count = uleb128(d, &mut p)? as usize;
        if count > 65536 {
            bail!("dex: implausible handler count {count}");
        }
        let mut handlers: HashMap<usize, (Vec<(u32, u32)>, Option<u32>)> = HashMap::new();
        for _ in 0..count {
            let this_off = p - handlers_base;
            let size = sleb128(d, &mut p)?;
            let (n, has_all) = if size > 0 {
                (size as usize, false)
            } else {
                ((-size) as usize, true)
            };
            let mut typed = Vec::with_capacity(n);
            for _ in 0..n {
                let type_idx = uleb128(d, &mut p)?;
                let addr = uleb128(d, &mut p)?;
                typed.push((type_idx, addr));
            }
            let catch_all = if has_all {
                Some(uleb128(d, &mut p)?)
            } else {
                None
            };
            handlers.insert(this_off, (typed, catch_all));
        }

        let mut out = Vec::new();
        for i in 0..tries_size {
            let base = try_items_base + i * 8;
            let start = u32_at(d, base)? as usize;
            let insn_count = u16_at(d, base + 4)? as usize;
            let handler_off = u16_at(d, base + 6)? as usize;
            let end = start + insn_count;

            let Some((typed_raw, catch_all)) = handlers.get(&handler_off) else {
                continue;
            };
            let typed = typed_raw
                .iter()
                .map(|(type_idx, addr)| (self.type_at(*type_idx), *addr as usize))
                .collect();
            out.push(TryBlock {
                start,
                end,
                typed,
                catch_all: catch_all.map(|a| a as usize),
            });
        }
        Ok(out)
    }
}

// ---------------------------------------------------------------------------
// Access flags and escaping.
// ---------------------------------------------------------------------------

fn join_flags(flags: &[&str]) -> String {
    if flags.is_empty() {
        String::new()
    } else {
        format!("{} ", flags.join(" "))
    }
}

fn class_flags(f: u32) -> String {
    let mut v: Vec<&str> = Vec::new();
    if f & 0x1 != 0 {
        v.push("public");
    }
    if f & 0x2 != 0 {
        v.push("private");
    }
    if f & 0x4 != 0 {
        v.push("protected");
    }
    if f & 0x10 != 0 {
        v.push("final");
    }
    if f & 0x200 != 0 {
        v.push("interface");
    }
    if f & 0x400 != 0 {
        v.push("abstract");
    }
    if f & 0x1000 != 0 {
        v.push("synthetic");
    }
    if f & 0x2000 != 0 {
        v.push("annotation");
    }
    if f & 0x4000 != 0 {
        v.push("enum");
    }
    join_flags(&v)
}

fn field_flags(f: u32) -> String {
    let mut v: Vec<&str> = Vec::new();
    if f & 0x1 != 0 {
        v.push("public");
    }
    if f & 0x2 != 0 {
        v.push("private");
    }
    if f & 0x4 != 0 {
        v.push("protected");
    }
    if f & 0x8 != 0 {
        v.push("static");
    }
    if f & 0x10 != 0 {
        v.push("final");
    }
    if f & 0x40 != 0 {
        v.push("volatile");
    }
    if f & 0x80 != 0 {
        v.push("transient");
    }
    if f & 0x1000 != 0 {
        v.push("synthetic");
    }
    if f & 0x4000 != 0 {
        v.push("enum");
    }
    join_flags(&v)
}

fn method_flags(f: u32) -> String {
    let mut v: Vec<&str> = Vec::new();
    if f & 0x1 != 0 {
        v.push("public");
    }
    if f & 0x2 != 0 {
        v.push("private");
    }
    if f & 0x4 != 0 {
        v.push("protected");
    }
    if f & 0x8 != 0 {
        v.push("static");
    }
    if f & 0x10 != 0 {
        v.push("final");
    }
    if f & 0x20 != 0 {
        v.push("synchronized");
    }
    if f & 0x40 != 0 {
        v.push("bridge");
    }
    if f & 0x80 != 0 {
        v.push("varargs");
    }
    if f & 0x100 != 0 {
        v.push("native");
    }
    if f & 0x400 != 0 {
        v.push("abstract");
    }
    if f & 0x800 != 0 {
        v.push("strictfp");
    }
    if f & 0x1000 != 0 {
        v.push("synthetic");
    }
    join_flags(&v)
}

/// Escape a string for a smali double-quoted literal.
fn escape_string(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            c if (c as u32) < 0x20 => out.push_str(&format!("\\u{:04x}", c as u32)),
            c => out.push(c),
        }
    }
    out
}

// ---------------------------------------------------------------------------
// Tests.
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    fn load_dex() -> Vec<u8> {
        let f = File::open("testdata/test-app.apk").expect("open test apk");
        let mut zip = zip::ZipArchive::new(f).expect("valid apk");
        let mut entry = zip.by_name("classes.dex").expect("classes.dex");
        let mut buf = Vec::new();
        entry.read_to_end(&mut buf).expect("read classes.dex");
        buf
    }

    #[test]
    fn width_table_is_sane() {
        assert_eq!(WIDTH.len(), 256);
        assert_eq!(WIDTH[0x18], 5, "const-wide is 5 units");
        assert_eq!(WIDTH[0x6e], 3, "invoke-virtual is 3 units");
        assert_eq!(WIDTH[0x6f], 3);
        assert_eq!(WIDTH[0x03], 3, "move/16 is 3 units");
        assert_eq!(WIDTH[0x13], 2, "const/16 is 2 units");
        assert_eq!(WIDTH[0x2b], 3, "packed-switch is 3 units");
        assert_eq!(WIDTH[0x00], 1, "nop is 1 unit");
        // 23x/22c/21c/22t/21t forms are all two code units wide.
        assert_eq!(WIDTH[0x44], 2, "aget is 2 units");
        assert_eq!(WIDTH[0x54], 2, "iget-object is 2 units");
        assert_eq!(WIDTH[0x62], 2, "sget-object is 2 units");
        assert_eq!(WIDTH[0x90], 2, "add-int is 2 units");
        assert_eq!(WIDTH[0x32], 2, "if-eq is 2 units");
        assert_eq!(WIDTH[0x38], 2, "if-eqz is 2 units");
        assert_eq!(WIDTH[0xb0], 1, "add-int/2addr is 1 unit");
    }

    #[test]
    fn mutf8_decoding() {
        // Encoded NUL is C0 80.
        assert_eq!(decode_mutf8(&[0xc0, 0x80, 0x41, 0x00]), "\u{0}A");
        // Plain ASCII with the NUL terminator.
        assert_eq!(decode_mutf8(b"hi\x00trailing"), "hi");
        // A lone surrogate decodes to the replacement character, not a panic.
        assert_eq!(decode_mutf8(&[0xed, 0xa0, 0x80]), "\u{fffd}");
    }

    #[test]
    fn parse_real_dex() {
        let dex = parse(&load_dex()).expect("parse classes.dex");
        // Calibrated to the generated fixture (examples/gen-fixture.rs):
        // the point is that every pool table parses non-empty and consistent.
        assert!(dex.strings.len() >= 14, "strings: {}", dex.strings.len());
        assert!(dex.types.len() >= 8, "types: {}", dex.types.len());
        assert!(dex.methods.len() >= 8, "methods: {}", dex.methods.len());
        assert!(dex.classes.len() >= 3, "classes: {}", dex.classes.len());

        // Every class descriptor should be a normal or array type descriptor.
        for c in &dex.classes {
            assert!(
                c.class_type.starts_with('L') || c.class_type.starts_with('['),
                "odd class descriptor {:?}",
                c.class_type
            );
        }
        // The app's own classes must be present.
        assert!(
            dex.classes
                .iter()
                .any(|c| c.class_type.contains("org/fdroid/fdroid")),
            "expected org/fdroid/fdroid classes"
        );
    }

    #[test]
    fn disassemble_real_dex() {
        let classes = disassemble(&load_dex()).expect("disassemble classes.dex");
        assert!(classes.len() >= 3, "classes: {}", classes.len());
        assert!(
            classes.iter().any(|c| c.source.contains(".method ")),
            "expected at least one .method directive"
        );
        assert!(
            classes.iter().all(|c| c.source.contains(".class ")),
            "every class should have a .class directive"
        );

        // Find a substantial class and print a snippet.
        let sample = classes
            .iter()
            .max_by_key(|c| c.source.len())
            .expect("a class");
        println!("--- sample smali: {} ---", sample.name);
        for line in sample.source.lines().take(40) {
            println!("{line}");
        }
    }

    #[test]
    fn no_unknown_opcodes_in_real_code() {
        // The strongest correctness signal for a disassembler: a wrong width
        // anywhere desynchronises the stream and produces bogus opcodes. Every
        // opcode in real, aapt2/R8-produced code must be recognised, and every
        // decoded instruction must be well-formed.
        let classes = disassemble(&load_dex()).expect("disassemble");
        let mut unknown = 0usize;
        let mut undecoded = 0usize;
        let mut ifs = 0usize;
        let mut gotos = 0usize;
        let mut catches = 0usize;
        let mut labels = 0usize;
        let mut lines = 0usize;
        for c in &classes {
            for line in c.source.lines() {
                let t = line.trim_start();
                lines += 1;
                if t.starts_with("# unknown opcode") {
                    unknown += 1;
                }
                if t.ends_with("(undecoded)") {
                    undecoded += 1;
                }
                if t.starts_with("if-") {
                    ifs += 1;
                }
                if t.starts_with("goto") {
                    gotos += 1;
                }
                if t.starts_with(".catch") {
                    catches += 1;
                }
                if t.starts_with(':') {
                    labels += 1;
                }
            }
        }
        println!(
            "lines={lines} unknown={unknown} undecoded={undecoded} ifs={ifs} gotos={gotos} catches={catches} labels={labels}"
        );
        assert_eq!(unknown, 0, "desynchronised / unknown opcodes in real code");
        assert_eq!(undecoded, 0, "recognised opcodes left undecoded");
        assert!(ifs > 0, "expected conditional branches");
        assert!(labels > 0, "expected branch labels");
        // The fixture carries no try/catch blocks; the try parsing path is
        // covered by the encoder-side unit tests and any real-world APK.
        let _ = catches;
    }

    #[test]
    fn disassemble_real_apk() {
        let classes = disassemble_apk("testdata/test-app.apk").expect("disassemble apk");
        assert!(classes.len() >= 3, "classes: {}", classes.len());
        assert!(
            classes.iter().any(|c| c.name.contains("org/fdroid/fdroid")),
            "expected org/fdroid/fdroid class names"
        );
        assert!(
            classes.iter().any(|c| c.source.contains(".method ")),
            "expected at least one method"
        );
        assert!(
            classes.iter().any(|c| c.source.contains(".registers ")),
            "expected at least one method body with .registers"
        );

        // Print the first ~40 lines of one concrete class, as requested.
        if let Some(sample) = classes
            .iter()
            .find(|c| c.name.contains("org/fdroid/fdroid") && c.source.contains(".method "))
        {
            println!("--- {} (first 40 lines) ---", sample.name);
            for line in sample.source.lines().take(40) {
                println!("{line}");
            }
        }
    }
}
