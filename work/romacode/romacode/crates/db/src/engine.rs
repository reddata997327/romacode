// ============================================================================
// dbengine 1.0.0 — a pure-Rust single-file database engine shipped as an MCP server
// ----------------------------------------------------------------------------
// ONE source file -> ONE binary. Zero external crates. std only.
//   Build:   rustc -O --edition 2021 dbengine.rs -o dbengine
//   Run:     ./dbengine serve                (MCP stdio, JSON-RPC 2.0)
//   Test:    ./dbengine selftest
//   Home:    DBENGINE_HOME or $HOME/.dbengine   (--home <dir> to override)
//
// Design centerpiece — KEYS. The whole engine is architected around where key
// data lives in databases: primary keys, foreign keys, unique keys and indexes
// are first-class (local catalog.json + remote information_schema/pg_catalog/
// INFORMATION_SCHEMA), and every novel tool (key-graph navigation, key-addressed
// snapshots/branches/diffs/merges, FK-topological extraction, key-based sync)
// is built on that key-graph foundation.
// ============================================================================

use std::collections::{BTreeMap, HashMap, HashSet};
use std::env;
use std::fs;
use std::io::{BufRead, Read, Write as IoWrite, BufWriter};
use std::net::TcpStream;
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

pub type R<T> = Result<T, String>;

// ============================================================================
// JSON — value model, parser, writer, pretty printer
// ============================================================================

#[derive(Clone, Debug, PartialEq)]
pub enum J {
    Null,
    Bool(bool),
    Int(i64),
    Num(f64),
    Str(String),
    Arr(Vec<J>),
    Obj(Vec<(String, J)>),
}

impl J {
    pub fn get(&self, k: &str) -> Option<&J> {
        match self {
            J::Obj(m) => m.iter().find(|(kk, _)| kk == k).map(|(_, v)| v),
            _ => None,
        }
    }
    pub fn as_str(&self) -> Option<&str> {
        match self {
            J::Str(s) => Some(s),
            _ => None,
        }
    }
    pub fn as_i64(&self) -> Option<i64> {
        match self {
            J::Int(i) => Some(*i),
            J::Num(f) if f.fract() == 0.0 && f.is_finite() => Some(*f as i64),
            _ => None,
        }
    }
    pub fn as_f64(&self) -> Option<f64> {
        match self {
            J::Int(i) => Some(*i as f64),
            J::Num(f) => Some(*f),
            _ => None,
        }
    }
    pub fn as_bool(&self) -> Option<bool> {
        match self {
            J::Bool(b) => Some(*b),
            _ => None,
        }
    }
    pub fn as_arr(&self) -> Option<&Vec<J>> {
        match self {
            J::Arr(a) => Some(a),
            _ => None,
        }
    }
    pub fn as_obj(&self) -> Option<&Vec<(String, J)>> {
        match self {
            J::Obj(o) => Some(o),
            _ => None,
        }
    }
    pub fn is_null(&self) -> bool {
        matches!(self, J::Null)
    }
    pub fn s(v: &str) -> J {
        J::Str(v.to_string())
    }
    pub fn obj(pairs: Vec<(&str, J)>) -> J {
        J::Obj(pairs.into_iter().map(|(k, v)| (k.to_string(), v)).collect())
    }
}

struct P<'a> {
    b: &'a [u8],
    i: usize,
}

pub fn jparse(s: &str) -> R<J> {
    let mut p = P { b: s.as_bytes(), i: 0 };
    p.ws();
    let v = p.val(0)?;
    p.ws();
    if p.i != p.b.len() {
        return Err("trailing data after json".into());
    }
    Ok(v)
}

fn utf8_len(b: u8) -> usize {
    if b < 0x80 {
        1
    } else if b >> 5 == 0b110 {
        2
    } else if b >> 4 == 0b1110 {
        3
    } else {
        4
    }
}

impl<'a> P<'a> {
    fn ws(&mut self) {
        while self.i < self.b.len() && matches!(self.b[self.i], b' ' | b'\t' | b'\r' | b'\n') {
            self.i += 1;
        }
    }
    fn peek(&self) -> Option<u8> {
        self.b.get(self.i).copied()
    }
    fn val(&mut self, d: u32) -> R<J> {
        if d > 256 {
            return Err("json too deep".into());
        }
        match self.peek().ok_or("unexpected end of json")? {
            b'{' => {
                self.i += 1;
                let mut m = Vec::new();
                self.ws();
                if self.peek() == Some(b'}') {
                    self.i += 1;
                    return Ok(J::Obj(m));
                }
                loop {
                    self.ws();
                    let k = match self.val(d + 1)? {
                        J::Str(s) => s,
                        _ => return Err("object key must be string".into()),
                    };
                    self.ws();
                    if self.peek() != Some(b':') {
                        return Err("expected ':' in object".into());
                    }
                    self.i += 1;
                    self.ws();
                    let v = self.val(d + 1)?;
                    m.push((k, v));
                    self.ws();
                    match self.peek() {
                        Some(b',') => {
                            self.i += 1;
                        }
                        Some(b'}') => {
                            self.i += 1;
                            break;
                        }
                        _ => return Err("expected ',' or '}' in object".into()),
                    }
                }
                Ok(J::Obj(m))
            }
            b'[' => {
                self.i += 1;
                let mut a = Vec::new();
                self.ws();
                if self.peek() == Some(b']') {
                    self.i += 1;
                    return Ok(J::Arr(a));
                }
                loop {
                    self.ws();
                    a.push(self.val(d + 1)?);
                    self.ws();
                    match self.peek() {
                        Some(b',') => {
                            self.i += 1;
                        }
                        Some(b']') => {
                            self.i += 1;
                            break;
                        }
                        _ => return Err("expected ',' or ']' in array".into()),
                    }
                }
                Ok(J::Arr(a))
            }
            b'"' => {
                self.i += 1;
                let mut out = String::new();
                loop {
                    let c = self.peek().ok_or("unterminated string")?;
                    self.i += 1;
                    match c {
                        b'"' => break,
                        b'\\' => {
                            let e = self.peek().ok_or("bad escape")?;
                            self.i += 1;
                            match e {
                                b'"' => out.push('"'),
                                b'\\' => out.push('\\'),
                                b'/' => out.push('/'),
                                b'b' => out.push('\u{0008}'),
                                b'f' => out.push('\u{000C}'),
                                b'n' => out.push('\n'),
                                b'r' => out.push('\r'),
                                b't' => out.push('\t'),
                                b'u' => {
                                    let cp = self.hex4()?;
                                    if (0xD800..0xDC00).contains(&cp) {
                                        if self.peek() == Some(b'\\') {
                                            self.i += 1;
                                            if self.peek() == Some(b'u') {
                                                self.i += 1;
                                                let lo = self.hex4()?;
                                                let c =
                                                    0x10000 + ((cp - 0xD800) << 10) + (lo - 0xDC00);
                                                out.push(
                                                    char::from_u32(c).ok_or("bad surrogate")?,
                                                );
                                                continue;
                                            }
                                        }
                                        return Err("bad surrogate pair".into());
                                    }
                                    out.push(char::from_u32(cp).ok_or("bad unicode")?);
                                }
                                _ => return Err("bad escape char".into()),
                            }
                        }
                        _ => {
                            let start = self.i - 1;
                            let len = utf8_len(c);
                            if start + len > self.b.len() {
                                return Err("bad utf8".into());
                            }
                            self.i = start + len;
                            let s = std::str::from_utf8(&self.b[start..self.i])
                                .map_err(|_| "bad utf8")?;
                            out.push_str(s);
                        }
                    }
                }
                Ok(J::Str(out))
            }
            b't' => {
                self.expect_word("true")?;
                Ok(J::Bool(true))
            }
            b'f' => {
                self.expect_word("false")?;
                Ok(J::Bool(false))
            }
            b'n' => {
                self.expect_word("null")?;
                Ok(J::Null)
            }
            b'-' | b'0'..=b'9' => self.number(),
            _ => Err("unexpected character in json".into()),
        }
    }
    fn expect_word(&mut self, w: &str) -> R<()> {
        if self.b[self.i..].starts_with(w.as_bytes()) {
            self.i += w.len();
            Ok(())
        } else {
            Err(format!("expected '{}'", w))
        }
    }
    fn hex4(&mut self) -> R<u32> {
        if self.i + 4 > self.b.len() {
            return Err("bad \\u escape".into());
        }
        let s = std::str::from_utf8(&self.b[self.i..self.i + 4]).map_err(|_| "bad \\u")?;
        let v = u32::from_str_radix(s, 16).map_err(|_| "bad \\u escape")?;
        self.i += 4;
        Ok(v)
    }
    fn number(&mut self) -> R<J> {
        let start = self.i;
        if self.peek() == Some(b'-') {
            self.i += 1;
        }
        while let Some(c) = self.peek() {
            if c.is_ascii_digit() {
                self.i += 1;
            } else {
                break;
            }
        }
        let mut float = false;
        if self.peek() == Some(b'.') {
            float = true;
            self.i += 1;
            while let Some(c) = self.peek() {
                if c.is_ascii_digit() {
                    self.i += 1;
                } else {
                    break;
                }
            }
        }
        if matches!(self.peek(), Some(b'e') | Some(b'E')) {
            float = true;
            self.i += 1;
            if matches!(self.peek(), Some(b'+') | Some(b'-')) {
                self.i += 1;
            }
            while let Some(c) = self.peek() {
                if c.is_ascii_digit() {
                    self.i += 1;
                } else {
                    break;
                }
            }
        }
        let s = std::str::from_utf8(&self.b[start..self.i]).map_err(|_| "bad number")?;
        if !float {
            if let Ok(i) = s.parse::<i64>() {
                return Ok(J::Int(i));
            }
        }
        s.parse::<f64>().map(J::Num).map_err(|_| "bad number".into())
    }
}

fn jwrite(v: &J, out: &mut String) {
    match v {
        J::Null => out.push_str("null"),
        J::Bool(b) => out.push_str(if *b { "true" } else { "false" }),
        J::Int(i) => out.push_str(&i.to_string()),
        J::Num(f) => {
            if f.is_finite() {
                out.push_str(&format!("{}", f));
            } else {
                out.push_str("null");
            }
        }
        J::Str(s) => {
            out.push('"');
            for c in s.chars() {
                match c {
                    '"' => out.push_str("\\\""),
                    '\\' => out.push_str("\\\\"),
                    '\n' => out.push_str("\\n"),
                    '\r' => out.push_str("\\r"),
                    '\t' => out.push_str("\\t"),
                    c if (c as u32) < 0x20 => {
                        out.push_str(&format!("\\u{:04x}", c as u32));
                    }
                    c => out.push(c),
                }
            }
            out.push('"');
        }
        J::Arr(a) => {
            out.push('[');
            for (i, x) in a.iter().enumerate() {
                if i > 0 {
                    out.push(',');
                }
                jwrite(x, out);
            }
            out.push(']');
        }
        J::Obj(o) => {
            out.push('{');
            for (i, (k, x)) in o.iter().enumerate() {
                if i > 0 {
                    out.push(',');
                }
                jwrite(&J::Str(k.clone()), out);
                out.push(':');
                jwrite(x, out);
            }
            out.push('}');
        }
    }
}

pub fn jstr(v: &J) -> String {
    let mut s = String::new();
    jwrite(v, &mut s);
    s
}

fn pad(out: &mut String, n: usize) {
    for _ in 0..n {
        out.push(' ');
    }
}

fn pw(v: &J, ind: usize, out: &mut String) {
    match v {
        J::Arr(a) if !a.is_empty() => {
            out.push_str("[\n");
            for (i, x) in a.iter().enumerate() {
                pad(out, ind + 2);
                pw(x, ind + 2, out);
                if i + 1 < a.len() {
                    out.push(',');
                }
                out.push('\n');
            }
            pad(out, ind);
            out.push(']');
        }
        J::Obj(o) if !o.is_empty() => {
            out.push_str("{\n");
            for (i, (k, x)) in o.iter().enumerate() {
                pad(out, ind + 2);
                jwrite(&J::Str(k.clone()), out);
                out.push_str(": ");
                pw(x, ind + 2, out);
                if i + 1 < o.len() {
                    out.push(',');
                }
                out.push('\n');
            }
            pad(out, ind);
            out.push('}');
        }
        _ => jwrite(v, out),
    }
}

pub fn jpretty(v: &J) -> String {
    let mut s = String::new();
    pw(v, 0, &mut s);
    s
}

// ---------------- base64 ----------------

const B64T: &[u8] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";

fn b64e(d: &[u8]) -> String {
    let mut out = String::new();
    for ch in d.chunks(3) {
        let b0 = ch[0] as u32;
        let b1 = *ch.get(1).unwrap_or(&0) as u32;
        let b2 = *ch.get(2).unwrap_or(&0) as u32;
        let n = (b0 << 16) | (b1 << 8) | b2;
        out.push(B64T[((n >> 18) & 63) as usize] as char);
        out.push(B64T[((n >> 12) & 63) as usize] as char);
        if ch.len() > 1 {
            out.push(B64T[((n >> 6) & 63) as usize] as char);
        }
        if ch.len() > 2 {
            out.push(B64T[(n & 63) as usize] as char);
        }
    }
    while out.len() % 4 != 0 {
        out.push('=');
    }
    out
}

fn b64d(s: &str) -> R<Vec<u8>> {
    let mut vals = Vec::new();
    for c in s.bytes() {
        match c {
            b'=' => break,
            b'\r' | b'\n' | b' ' => continue,
            b'A'..=b'Z' => vals.push((c - b'A') as u8),
            b'a'..=b'z' => vals.push((c - b'a' + 26) as u8),
            b'0'..=b'9' => vals.push((c - b'0' + 52) as u8),
            b'+' => vals.push(62),
            b'/' => vals.push(63),
            _ => return Err("bad base64".into()),
        }
    }
    let mut out = Vec::new();
    for ch in vals.chunks(4) {
        match ch.len() {
            4 => {
                out.push(ch[0] << 2 | ch[1] >> 4);
                out.push((ch[1] & 0x0f) << 4 | ch[2] >> 2);
                out.push((ch[2] & 0x03) << 6 | ch[3]);
            }
            3 => {
                out.push(ch[0] << 2 | ch[1] >> 4);
                out.push((ch[1] & 0x0f) << 4 | ch[2] >> 2);
            }
            2 => {
                out.push(ch[0] << 2 | ch[1] >> 4);
            }
            _ => {}
        }
    }
    Ok(out)
}

// ---------------- small arg helpers ----------------

pub fn arg_s(a: &J, k: &str) -> R<String> {
    a.get(k)
        .and_then(|x| x.as_str())
        .map(|s| s.to_string())
        .ok_or_else(|| format!("missing string param '{}'", k))
}
pub fn arg_opt_s(a: &J, k: &str) -> Option<String> {
    a.get(k).and_then(|x| x.as_str()).map(|s| s.to_string())
}
pub fn arg_i64(a: &J, k: &str) -> R<i64> {
    a.get(k)
        .and_then(|x| x.as_i64())
        .ok_or_else(|| format!("missing integer param '{}'", k))
}
pub fn arg_u64_d(a: &J, k: &str, dflt: u64) -> u64 {
    a.get(k).and_then(|x| x.as_i64()).map(|v| v.max(0) as u64).unwrap_or(dflt)
}
pub fn arg_bool_d(a: &J, k: &str, dflt: bool) -> bool {
    a.get(k).and_then(|x| x.as_bool()).unwrap_or(dflt)
}

fn jget(m: &[(String, J)], k: &str) -> Option<J> {
    m.iter().find(|(kk, _)| kk == k).map(|(_, v)| v.clone())
}

// ============================================================================
// Crypto — MD5, SHA1, SHA256, HMAC, PBKDF2 (needed for Postgres md5/SCRAM and
// MySQL mysql_native_password / caching_sha2 fast auth). std only.
// ============================================================================

const MD5_K: [u32; 64] = [
    0xd76aa478, 0xe8c7b756, 0x242070db, 0xc1bdceee, 0xf57c0faf, 0x4787c62a, 0xa8304613, 0xfd469501,
    0x698098d8, 0x8b44f7af, 0xffff5bb1, 0x895cd7be, 0x6b901122, 0xfd987193, 0xa679438e, 0x49b40821,
    0xf61e2562, 0xc040b340, 0x265e5a51, 0xe9b6c7aa, 0xd62f105d, 0x02441453, 0xd8a1e681, 0xe7d3fbc8,
    0x21e1cde6, 0xc33707d6, 0xf4d50d87, 0x455a14ed, 0xa9e3e905, 0xfcefa3f8, 0x676f02d9, 0x8d2a4c8a,
    0xfffa3942, 0x8771f681, 0x6d9d6122, 0xfde5380c, 0xa4beea44, 0x4bdecfa9, 0xf6bb4b60, 0xbebfbc70,
    0x289b7ec6, 0xeaa127fa, 0xd4ef3085, 0x04881d05, 0xd9d4d039, 0xe6db99e5, 0x1fa27cf8, 0xc4ac5665,
    0xf4292244, 0x432aff97, 0xab9423a7, 0xfc93a039, 0x655b59c3, 0x8f0ccc92, 0xffeff47d, 0x85845dd1,
    0x6fa87e4f, 0xfe2ce6e0, 0xa3014314, 0x4e0811a1, 0xf7537e82, 0xbd3af235, 0x2ad7d2bb, 0xeb86d391,
];
const MD5_S: [u32; 64] = [
    7, 12, 17, 22, 7, 12, 17, 22, 7, 12, 17, 22, 7, 12, 17, 22, 5, 9, 14, 20, 5, 9, 14, 20, 5, 9,
    14, 20, 5, 9, 14, 20, 4, 11, 16, 23, 4, 11, 16, 23, 4, 11, 16, 23, 4, 11, 16, 23, 6, 10, 15,
    21, 6, 10, 15, 21, 6, 10, 15, 21, 6, 10, 15, 21,
];

fn md5(msg: &[u8]) -> [u8; 16] {
    let mut data = msg.to_vec();
    let blen = (data.len() as u64) * 8;
    data.push(0x80);
    while data.len() % 64 != 56 {
        data.push(0);
    }
    data.extend_from_slice(&blen.to_le_bytes());
    let (mut a0, mut b0, mut c0, mut d0) =
        (0x67452301u32, 0xefcdab89u32, 0x98badcfeu32, 0x10325476u32);
    for ch in data.chunks(64) {
        let mut m = [0u32; 16];
        for i in 0..16 {
            m[i] = u32::from_le_bytes(ch[i * 4..i * 4 + 4].try_into().unwrap());
        }
        let (mut a, mut b, mut c, mut d) = (a0, b0, c0, d0);
        for i in 0..64 {
            let (f, g) = match i {
                0..=15 => ((b & c) | (!b & d), i),
                16..=31 => ((d & b) | (!d & c), (5 * i + 1) % 16),
                32..=47 => (b ^ c ^ d, (3 * i + 5) % 16),
                _ => (c ^ (b | !d), (7 * i) % 16),
            };
            let tmp = d;
            d = c;
            c = b;
            let x = a
                .wrapping_add(f)
                .wrapping_add(MD5_K[i])
                .wrapping_add(m[g]);
            b = b.wrapping_add(x.rotate_left(MD5_S[i]));
            a = tmp;
        }
        a0 = a0.wrapping_add(a);
        b0 = b0.wrapping_add(b);
        c0 = c0.wrapping_add(c);
        d0 = d0.wrapping_add(d);
    }
    let mut out = [0u8; 16];
    out[0..4].copy_from_slice(&a0.to_le_bytes());
    out[4..8].copy_from_slice(&b0.to_le_bytes());
    out[8..12].copy_from_slice(&c0.to_le_bytes());
    out[12..16].copy_from_slice(&d0.to_le_bytes());
    out
}

fn sha1(msg: &[u8]) -> [u8; 20] {
    let mut data = msg.to_vec();
    let blen = (data.len() as u64) * 8;
    data.push(0x80);
    while data.len() % 64 != 56 {
        data.push(0);
    }
    data.extend_from_slice(&blen.to_be_bytes());
    let mut h: [u32; 5] = [0x67452301, 0xEFCDAB89, 0x98BADCFE, 0x10325476, 0xC3D2E1F0];
    for ch in data.chunks(64) {
        let mut w = [0u32; 80];
        for i in 0..16 {
            w[i] = u32::from_be_bytes(ch[i * 4..i * 4 + 4].try_into().unwrap());
        }
        for i in 16..80 {
            w[i] = (w[i - 3] ^ w[i - 8] ^ w[i - 14] ^ w[i - 16]).rotate_left(1);
        }
        let (mut a, mut b, mut c, mut d, mut e) = (h[0], h[1], h[2], h[3], h[4]);
        for i in 0..80 {
            let (f, k) = match i {
                0..=19 => ((b & c) | (!b & d), 0x5A827999u32),
                20..=39 => (b ^ c ^ d, 0x6ED9EBA1u32),
                40..=59 => ((b & c) | (b & d) | (c & d), 0x8F1BBCDCu32),
                _ => (b ^ c ^ d, 0xCA62C1D6u32),
            };
            let t = a
                .rotate_left(5)
                .wrapping_add(f)
                .wrapping_add(e)
                .wrapping_add(k)
                .wrapping_add(w[i]);
            e = d;
            d = c;
            c = b.rotate_left(30);
            b = a;
            a = t;
        }
        h[0] = h[0].wrapping_add(a);
        h[1] = h[1].wrapping_add(b);
        h[2] = h[2].wrapping_add(c);
        h[3] = h[3].wrapping_add(d);
        h[4] = h[4].wrapping_add(e);
    }
    let mut out = [0u8; 20];
    for i in 0..5 {
        out[i * 4..i * 4 + 4].copy_from_slice(&h[i].to_be_bytes());
    }
    out
}

const SHA256_K: [u32; 64] = [
    0x428a2f98, 0x71374491, 0xb5c0fbcf, 0xe9b5dba5, 0x3956c25b, 0x59f111f1, 0x923f82a4, 0xab1c5ed5,
    0xd807aa98, 0x12835b01, 0x243185be, 0x550c7dc3, 0x72be5d74, 0x80deb1fe, 0x9bdc06a7, 0xc19bf174,
    0xe49b69c1, 0xefbe4786, 0x0fc19dc6, 0x240ca1cc, 0x2de92c6f, 0x4a7484aa, 0x5cb0a9dc, 0x76f988da,
    0x983e5152, 0xa831c66d, 0xb00327c8, 0xbf597fc7, 0xc6e00bf3, 0xd5a79147, 0x06ca6351, 0x14292967,
    0x27b70a85, 0x2e1b2138, 0x4d2c6dfc, 0x53380d13, 0x650a7354, 0x766a0abb, 0x81c2c92e, 0x92722c85,
    0xa2bfe8a1, 0xa81a664b, 0xc24b8b70, 0xc76c51a3, 0xd192e819, 0xd6990624, 0xf40e3585, 0x106aa070,
    0x19a4c116, 0x1e376c08, 0x2748774c, 0x34b0bcb5, 0x391c0cb3, 0x4ed8aa4a, 0x5b9cca4f, 0x682e6ff3,
    0x748f82ee, 0x78a5636f, 0x84c87814, 0x8cc70208, 0x90befffa, 0xa4506ceb, 0xbef9a3f7, 0xc67178f2,
];

fn sha256(msg: &[u8]) -> [u8; 32] {
    let mut data = msg.to_vec();
    let blen = (data.len() as u64) * 8;
    data.push(0x80);
    while data.len() % 64 != 56 {
        data.push(0);
    }
    data.extend_from_slice(&blen.to_be_bytes());
    let mut h: [u32; 8] = [
        0x6a09e667, 0xbb67ae85, 0x3c6ef372, 0xa54ff53a, 0x510e527f, 0x9b05688c, 0x1f83d9ab,
        0x5be0cd19,
    ];
    for ch in data.chunks(64) {
        let mut w = [0u32; 64];
        for i in 0..16 {
            w[i] = u32::from_be_bytes(ch[i * 4..i * 4 + 4].try_into().unwrap());
        }
        for i in 16..64 {
            let s0 = w[i - 15].rotate_right(7) ^ w[i - 15].rotate_right(18) ^ (w[i - 15] >> 3);
            let s1 = w[i - 2].rotate_right(17) ^ w[i - 2].rotate_right(19) ^ (w[i - 2] >> 10);
            w[i] = w[i - 16]
                .wrapping_add(s0)
                .wrapping_add(w[i - 7])
                .wrapping_add(s1);
        }
        let mut v = h;
        for i in 0..64 {
            let s1 = v[4].rotate_right(6) ^ v[4].rotate_right(11) ^ v[4].rotate_right(25);
            let ch = (v[4] & v[5]) ^ (!v[4] & v[6]);
            let t1 = v[7]
                .wrapping_add(s1)
                .wrapping_add(ch)
                .wrapping_add(SHA256_K[i])
                .wrapping_add(w[i]);
            let s0 = v[0].rotate_right(2) ^ v[0].rotate_right(13) ^ v[0].rotate_right(22);
            let mj = (v[0] & v[1]) ^ (v[0] & v[2]) ^ (v[1] & v[2]);
            let t2 = s0.wrapping_add(mj);
            v[7] = v[6];
            v[6] = v[5];
            v[5] = v[4];
            v[4] = v[3].wrapping_add(t1);
            v[3] = v[2];
            v[2] = v[1];
            v[1] = v[0];
            v[0] = t1.wrapping_add(t2);
        }
        for i in 0..8 {
            h[i] = h[i].wrapping_add(v[i]);
        }
    }
    let mut out = [0u8; 32];
    for i in 0..8 {
        out[i * 4..i * 4 + 4].copy_from_slice(&h[i].to_be_bytes());
    }
    out
}

fn hmac_sha256(key: &[u8], msg: &[u8]) -> Vec<u8> {
    let mut k = key.to_vec();
    if k.len() > 64 {
        k = sha256(&k).to_vec();
    }
    k.resize(64, 0);
    let mut ipad = [0x36u8; 64];
    let mut opad = [0x5cu8; 64];
    for i in 0..64 {
        ipad[i] ^= k[i];
        opad[i] ^= k[i];
    }
    let mut m = ipad.to_vec();
    m.extend_from_slice(msg);
    let ih = sha256(&m);
    let mut mo = opad.to_vec();
    mo.extend_from_slice(&ih);
    sha256(&mo).to_vec()
}

fn hmac_sha1(key: &[u8], msg: &[u8]) -> Vec<u8> {
    let mut k = key.to_vec();
    if k.len() > 64 {
        k = sha1(&k).to_vec();
    }
    k.resize(64, 0);
    let mut ipad = [0x36u8; 64];
    let mut opad = [0x5cu8; 64];
    for i in 0..64 {
        ipad[i] ^= k[i];
        opad[i] ^= k[i];
    }
    let mut m = ipad.to_vec();
    m.extend_from_slice(msg);
    let ih = sha1(&m);
    let mut mo = opad.to_vec();
    mo.extend_from_slice(&ih);
    sha1(&mo).to_vec()
}

fn pbkdf2_sha256(pass: &[u8], salt: &[u8], iters: u32, dklen: usize) -> Vec<u8> {
    let mut out = Vec::new();
    let mut block: u32 = 1;
    while out.len() < dklen {
        let mut saltb = salt.to_vec();
        saltb.extend_from_slice(&block.to_be_bytes());
        let mut u = hmac_sha256(pass, &saltb);
        let mut t = u.clone();
        for _ in 1..iters.max(1) {
            u = hmac_sha256(pass, &u);
            for j in 0..t.len() {
                t[j] ^= u[j];
            }
        }
        out.extend_from_slice(&t);
        block += 1;
    }
    out.truncate(dklen);
    out
}

fn hexe(b: &[u8]) -> String {
    b.iter().map(|x| format!("{:02x}", x)).collect()
}

fn xor_vec(a: &[u8], b: &[u8]) -> Vec<u8> {
    a.iter().zip(b.iter()).map(|(x, y)| x ^ y).collect()
}

fn rand_bytes(n: usize) -> Vec<u8> {
    let mut v = vec![0u8; n];
    if let Ok(mut f) = fs::File::open("/dev/urandom") {
        if f.read_exact(&mut v).is_ok() {
            return v;
        }
    }
    let t = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_nanos() as u64)
        .unwrap_or(12345);
    let mut st = t;
    for i in 0..n {
        st = st.wrapping_mul(0x9E3779B97F4A7C15).wrapping_add(0xBF58476D1CE4E5B9);
        v[i] = (st >> 33) as u8;
    }
    v
}

// ============================================================================
// Small utilities
// ============================================================================

fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

fn sanitize_name(s: &str) -> R<String> {
    if s.is_empty() || s.len() > 64 {
        return Err("name must be 1..64 chars".into());
    }
    for c in s.chars() {
        if !(c.is_ascii_alphanumeric() || c == '_' || c == '-') {
            return Err(format!("invalid character '{}' in name (allowed: A-Za-z0-9_-)", c));
        }
    }
    Ok(s.to_string())
}

fn sanitize_col(s: &str) -> R<String> {
    if s.is_empty() || s.len() > 64 {
        return Err("column name must be 1..64 chars".into());
    }
    for c in s.chars() {
        if !(c.is_ascii_alphanumeric() || c == '_') {
            return Err(format!("invalid character '{}' in column name (allowed: A-Za-z0-9_)", c));
        }
    }
    Ok(s.to_string())
}

/// Sanitize a table name for use in WAL file paths (`wal/<table>.jsonl`):
/// keep [A-Za-z0-9_-] and replace every other character with '_' (so path
/// separators and leading dots can never escape the WAL directory), with a
/// fallback for names that would otherwise be empty.
fn wal_table_name(tname: &str) -> String {
    let s: String = tname
        .chars()
        .map(|c| if c.is_ascii_alphanumeric() || c == '_' || c == '-' { c } else { '_' })
        .collect();
    if s.is_empty() {
        "table".to_string()
    } else {
        s
    }
}

fn atomic_write(path: &Path, data: &str) -> R<()> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).map_err(|e| format!("mkdir {}: {}", parent.display(), e))?;
    }
    let tmp = path.with_extension("tmp");
    fs::write(&tmp, data).map_err(|e| format!("write {}: {}", tmp.display(), e))?;
    fs::rename(&tmp, path).map_err(|e| format!("rename {}: {}", path.display(), e))?;
    Ok(())
}

fn like_match(pat: &str, s: &str) -> bool {
    like_rec(pat.as_bytes(), s.as_bytes())
}
fn like_rec(p: &[u8], s: &[u8]) -> bool {
    if p.is_empty() {
        return s.is_empty();
    }
    match p[0] {
        b'%' => {
            for i in 0..=s.len() {
                if like_rec(&p[1..], &s[i..]) {
                    return true;
                }
            }
            false
        }
        b'_' => !s.is_empty() && like_rec(&p[1..], &s[1..]),
        c => !s.is_empty() && s[0] == c && like_rec(&p[1..], &s[1..]),
    }
}

use std::cmp::Ordering;

fn cmp_vals(a: &J, b: &J) -> Option<Ordering> {
    match (a, b) {
        (J::Int(x), J::Int(y)) => Some(x.cmp(y)),
        (J::Int(x), J::Num(y)) => (*x as f64).partial_cmp(y),
        (J::Num(x), J::Int(y)) => x.partial_cmp(&(*y as f64)),
        (J::Num(x), J::Num(y)) => x.partial_cmp(y),
        (J::Str(x), J::Str(y)) => Some(x.cmp(y)),
        (J::Bool(x), J::Bool(y)) => Some(x.cmp(y)),
        // cross numeric-string
        (J::Str(x), J::Int(y)) => x.trim().parse::<i64>().ok().map(|p| p.cmp(y)),
        (J::Int(x), J::Str(y)) => y.trim().parse::<i64>().ok().map(|p| x.cmp(&p)),
        (J::Str(x), J::Num(y)) => x
            .trim()
            .parse::<f64>()
            .ok()
            .and_then(|p| p.partial_cmp(y)),
        (J::Num(x), J::Str(y)) => y
            .trim()
            .parse::<f64>()
            .ok()
            .and_then(|p| x.partial_cmp(&p)),
        _ => None,
    }
}

fn jeq(a: &J, b: &J) -> bool {
    matches!(cmp_vals(a, b), Some(Ordering::Equal))
}

// ============================================================================
// STORAGE ENGINE — local databases, tables, rows, keys, WAL, indexes
// ----------------------------------------------------------------------------
// Where key data lives (local engine):
//   <home>/databases/<db>/catalog.json    -> schema + PK/FK/unique/index definitions
//   <home>/databases/<db>/wal/<table>.jsonl -> rows, each line keyed by PK value
//   in-memory BTreeMap indexes            -> rebuilt from rows on open
//   <home>/snapshots/<db>/<name>.json     -> key-addressed snapshots
//   <home>/extracts/<name>.json           -> portable extract bundles
// ============================================================================

#[derive(Clone, Debug)]
struct Col {
    name: String,
    dtype: String, // integer | real | text | boolean | json | any
    nullable: bool,
    auto: bool, // auto-increment (integer)
}

#[derive(Clone, Debug)]
struct Fk {
    cols: Vec<String>,
    ref_table: String,
    ref_cols: Vec<String>,
    on_delete: String, // restrict | cascade
}

#[derive(Clone, Debug)]
struct IndexDef {
    name: String,
    cols: Vec<String>,
    unique: bool,
}

#[derive(Clone, Debug, Default)]
struct Table {
    name: String,
    cols: Vec<Col>,
    pk: Vec<String>,
    fks: Vec<Fk>,
    uniques: Vec<Vec<String>>,
    indexes: Vec<IndexDef>,
    auto: BTreeMap<String, i64>,
    nocheck: bool, // skip FK enforcement (used for cyclic-key graphs)
    rows: HashMap<String, Vec<(String, J)>>,
    idx: HashMap<String, BTreeMap<String, Vec<String>>>,
    wlines: u64,
}

impl Table {
    fn all_index_defs(&self) -> Vec<(String, Vec<String>, bool)> {
        let mut v = Vec::new();
        if !self.pk.is_empty() {
            v.push(("pk".to_string(), self.pk.clone(), true));
        }
        for (i, u) in self.uniques.iter().enumerate() {
            v.push((format!("u#{}", i), u.clone(), true));
        }
        for (i, f) in self.fks.iter().enumerate() {
            v.push((format!("f#{}", i), f.cols.clone(), false));
        }
        for x in &self.indexes {
            v.push((x.name.clone(), x.cols.clone(), x.unique));
        }
        v
    }
    fn def_for_cols(&self, cols: &[String]) -> Option<String> {
        if !self.pk.is_empty() && self.pk == cols {
            return Some("pk".to_string());
        }
        for (i, u) in self.uniques.iter().enumerate() {
            if u == cols {
                return Some(format!("u#{}", i));
            }
        }
        None
    }
    fn rm_indexes(&mut self, key: &str) {
        for (_, map) in self.idx.iter_mut() {
            map.retain(|_, v| {
                if let Some(p) = v.iter().position(|x| x == key) {
                    v.remove(p);
                }
                !v.is_empty()
            });
        }
    }
    fn add_indexes(&mut self, key: &str, row: &[(String, J)]) {
        for (name, cols, _u) in self.all_index_defs() {
            let vals = colvals(row, &cols);
            let ik = jstr(&J::Arr(vals));
            self.idx
                .entry(name)
                .or_default()
                .entry(ik)
                .or_default()
                .push(key.to_string());
        }
    }
    fn rebuild_indexes(&mut self) {
        self.idx.clear();
        let items: Vec<(String, Vec<(String, J)>)> = self
            .rows
            .iter()
            .map(|(k, v)| (k.clone(), v.clone()))
            .collect();
        for (k, row) in items {
            self.add_indexes(&k, &row);
        }
    }
}

fn colvals(row: &[(String, J)], cols: &[String]) -> Vec<J> {
    cols.iter()
        .map(|c| {
            row.iter()
                .find(|(k, _)| k == c)
                .map(|(_, v)| v.clone())
                .unwrap_or(J::Null)
        })
        .collect()
}

#[derive(Clone, Debug, Default)]
struct Db {
    name: String,
    tables: BTreeMap<String, Table>,
}

// ---------------- coercion ----------------

fn coerce(v: J, dtype: &str, col: &str) -> R<J> {
    Ok(match dtype {
        "integer" => match v {
            J::Null => J::Null,
            J::Int(i) => J::Int(i),
            J::Num(f) => {
                if f.fract() == 0.0 && f.is_finite() {
                    J::Int(f as i64)
                } else {
                    return Err(format!("column '{}': not an integer: {}", col, f));
                }
            }
            J::Bool(b) => J::Int(if b { 1 } else { 0 }),
            J::Str(s) => J::Int(
                s.trim()
                    .parse::<i64>()
                    .map_err(|_| format!("column '{}': invalid integer {:?}", col, s))?,
            ),
            _ => return Err(format!("column '{}': invalid integer value", col)),
        },
        "real" => match v {
            J::Null => J::Null,
            J::Int(i) => J::Num(i as f64),
            J::Num(f) => J::Num(f),
            J::Str(s) => J::Num(
                s.trim()
                    .parse::<f64>()
                    .map_err(|_| format!("column '{}': invalid number {:?}", col, s))?,
            ),
            _ => return Err(format!("column '{}': invalid number value", col)),
        },
        "text" => match v {
            J::Null => J::Null,
            J::Str(s) => J::Str(s),
            J::Bool(b) => J::Str(b.to_string()),
            J::Int(i) => J::Str(i.to_string()),
            J::Num(f) => J::Str(format!("{}", f)),
            _ => return Err(format!("column '{}': invalid text value", col)),
        },
        "boolean" => match v {
            J::Null => J::Null,
            J::Bool(b) => J::Bool(b),
            J::Int(i) => J::Bool(i != 0),
            J::Str(s) => {
                let l = s.trim().to_ascii_lowercase();
                match l.as_str() {
                    "true" | "t" | "yes" | "y" | "1" => J::Bool(true),
                    "false" | "f" | "no" | "n" | "0" => J::Bool(false),
                    _ => return Err(format!("column '{}': invalid boolean {:?}", col, s)),
                }
            }
            _ => return Err(format!("column '{}': invalid boolean value", col)),
        },
        "json" | "any" => v,
        other => return Err(format!("column '{}': unknown dtype '{}'", col, other)),
    })
}

// ---------------- engine ----------------

pub struct Engine {
    pub home: PathBuf,
    dbs: HashMap<String, Db>,
}

impl Engine {
    pub fn new(home: PathBuf) -> Engine {
        Engine { home, dbs: HashMap::new() }
    }

    fn db(&mut self, name: &str) -> R<&mut Db> {
        let sname = sanitize_name(name)?;
        if !self.dbs.contains_key(&sname) {
            self.open_db(&sname)?;
        }
        Ok(self.dbs.get_mut(&sname).unwrap())
    }

    fn has_db(&self, name: &str) -> bool {
        self.dbs.contains_key(name)
            || self.home.join("databases").join(name).join("catalog.json").exists()
    }

    fn db_dir(&self, name: &str) -> PathBuf {
        self.home.join("databases").join(name)
    }

    fn open_db(&mut self, sname: &str) -> R<()> {
        let dir = self.db_dir(sname);
        if !dir.exists() {
            fs::create_dir_all(dir.join("wal"))
                .map_err(|e| format!("create db dir: {}", e))?;
            let empty = Db { name: sname.to_string(), tables: BTreeMap::new() };
            atomic_write(&dir.join("catalog.json"), &jstr(&db_to_json(&empty)))?;
        }
        let catp = dir.join("catalog.json");
        let cat = fs::read_to_string(&catp)
            .map_err(|e| format!("read catalog {}: {}", catp.display(), e))?;
        let j = jparse(&cat).map_err(|e| format!("catalog corrupt: {}", e))?;
        let mut db = Db { name: sname.to_string(), tables: BTreeMap::new() };
        if let Some(ts) = j.get("tables").and_then(|x| x.as_arr()) {
            for tv in ts {
                let mut t = table_from_json(tv)?;
                let wal = dir.join("wal").join(format!("{}.jsonl", wal_table_name(&t.name)));
                if let Ok(data) = fs::read_to_string(&wal) {
                    let mut lines = 0u64;
                    for line in data.lines() {
                        if line.trim().is_empty() {
                            continue;
                        }
                        lines += 1;
                        if let Ok(op) = jparse(line) {
                            let kind = op.get("op").and_then(|x| x.as_str()).unwrap_or("");
                            match kind {
                                "put" => {
                                    if let (Some(k), Some(r)) = (
                                        op.get("k").and_then(|x| x.as_str()).map(|s| s.to_string()),
                                        op.get("r"),
                                    ) {
                                        if let J::Obj(m) = r.clone() {
                                            t.rows.insert(k, m);
                                        }
                                    }
                                }
                                "del" => {
                                    if let Some(k) = op.get("k").and_then(|x| x.as_str()) {
                                        t.rows.remove(k);
                                    }
                                }
                                _ => {}
                            }
                        }
                    }
                    t.wlines = lines;
                }
                t.rebuild_indexes();
                db.tables.insert(t.name.clone(), t);
            }
        }
        self.dbs.insert(sname.to_string(), db);
        // compaction pass
        let tnames: Vec<String> = {
            let d = self.dbs.get(sname).unwrap();
            d.tables.keys().cloned().collect()
        };
        for tn in tnames {
            self.maybe_compact(sname, &tn)?;
        }
        Ok(())
    }

    fn save_catalog(&mut self, dbname: &str) -> R<()> {
        let sname = sanitize_name(dbname)?;
        let db = self
            .dbs
            .get(&sname)
            .ok_or_else(|| format!("database '{}' not open", sname))?;
        let path = self.db_dir(&sname).join("catalog.json");
        atomic_write(&path, &jstr(&db_to_json(db)))
    }

    fn wal_line(&mut self, dbname: &str, tname: &str, line: &J) -> R<()> {
        let sname = sanitize_name(dbname)?;
        let path = self
            .db_dir(&sname)
            .join("wal")
            .join(format!("{}.jsonl", wal_table_name(tname)));
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent).map_err(|e| format!("mkdir: {}", e))?;
        }
        let mut f = fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(&path)
            .map_err(|e| format!("open wal: {}", e))?;
        f.write_all(jstr(line).as_bytes())
            .and_then(|_| f.write_all(b"\n"))
            .map_err(|e| format!("append wal: {}", e))?;
        Ok(())
    }

    fn maybe_compact(&mut self, dbname: &str, tname: &str) -> R<()> {
        let sname = sanitize_name(dbname)?;
        let (wlines, nrows) = {
            let d = self.dbs.get(&sname).unwrap();
            let t = match d.tables.get(tname) {
                Some(t) => t,
                None => return Ok(()),
            };
            (t.wlines, t.rows.len())
        };
        if wlines > (nrows * 2).max(256) as u64 {
            let sname2 = sname.clone();
            let tname2 = tname.to_string();
            let mut lines = String::new();
            {
                let d = self.dbs.get(&sname2).unwrap();
                let t = d.tables.get(&tname2).unwrap();
                let mut keys: Vec<&String> = t.rows.keys().collect();
                keys.sort();
                for k in keys {
                    let row = &t.rows[k];
                    let op = J::obj(vec![("op", J::s("put")), ("k", J::s(k)), ("r", J::Obj(row.clone()))]);
                    lines.push_str(&jstr(&op));
                    lines.push('\n');
                }
            }
            let path = self
                .db_dir(&sname2)
                .join("wal")
                .join(format!("{}.jsonl", wal_table_name(&tname2)));
            atomic_write(&path, &lines)?;
            let d = self.dbs.get_mut(&sname2).unwrap();
            let t = d.tables.get_mut(&tname2).unwrap();
            t.wlines = t.rows.len() as u64;
        }
        Ok(())
    }

    // -------- row ops --------

    fn put_row(&mut self, dbname: &str, tname: &str, row: &J, upsert: bool) -> R<String> {
        let m = match row {
            J::Obj(m) => m.clone(),
            _ => return Err("row must be an object".into()),
        };
        // phase 1: defs
        let (cols, pk, fks, defs, nocheck) = {
            let db = self.db(dbname)?;
            let t = db
                .tables
                .get(tname)
                .ok_or_else(|| format!("table '{}' not found", tname))?;
            (t.cols.clone(), t.pk.clone(), t.fks.clone(), t.all_index_defs(), t.nocheck)
        };
        for (k, _) in &m {
            if !cols.iter().any(|c| &c.name == k) {
                return Err(format!("unknown column '{}'", k));
            }
        }
        let mut r: Vec<(String, J)> = Vec::new();
        for c in &cols {
            let v = m
                .iter()
                .find(|(k, _)| *k == c.name)
                .map(|(_, v)| v.clone())
                .unwrap_or(J::Null);
            let v = coerce(v, &c.dtype, &c.name)?;
            if v.is_null() && !c.nullable && !c.auto {
                return Err(format!("column '{}' is not nullable", c.name));
            }
            r.push((c.name.clone(), v));
        }
        // auto-increment
        let mut seq_changed = false;
        for c in &cols {
            if c.auto {
                let pos = r.iter().position(|(k, _)| *k == c.name).unwrap();
                if r[pos].1.is_null() {
                    let nv = {
                        let db = self.db(dbname)?;
                        let t = db.tables.get_mut(tname).unwrap();
                        let e = t.auto.entry(c.name.clone()).or_insert(0);
                        *e += 1;
                        *e
                    };
                    r[pos].1 = J::Int(nv);
                    seq_changed = true;
                } else if let Some(iv) = r[pos].1.as_i64() {
                    let db = self.db(dbname)?;
                    let t = db.tables.get_mut(tname).unwrap();
                    let e = t.auto.entry(c.name.clone()).or_insert(0);
                    if iv > *e {
                        *e = iv;
                        seq_changed = true;
                    }
                }
            }
        }
        if pk.is_empty() {
            return Err(format!("table '{}' has no primary key", tname));
        }
        let pkv = colvals(&r, &pk);
        if pkv.iter().any(|v| v.is_null()) {
            return Err("primary key columns cannot be null".into());
        }
        let key = jstr(&J::Arr(pkv));
        let existed = {
            let db = self.db(dbname)?;
            db.tables.get(tname).unwrap().rows.contains_key(&key)
        };
        if existed && !upsert {
            return Err(format!("duplicate primary key {} (row already exists)", key));
        }
        // phase 2: checks
        {
            let db = self.db(dbname)?;
            let t = db.tables.get(tname).unwrap();
            for (iname, icols, unique) in &defs {
                if !*unique {
                    continue;
                }
                let vals = colvals(&r, icols);
                if vals.iter().any(|v| v.is_null()) {
                    continue; // SQL semantics: NULLs are distinct in unique keys
                }
                let ik = jstr(&J::Arr(vals));
                if let Some(ks) = t.idx.get(iname).and_then(|m| m.get(&ik)) {
                    if ks.iter().any(|x| x != &key) {
                        return Err(format!(
                            "unique constraint violated on ({})",
                            icols.join(",")
                        ));
                    }
                }
            }
        }
        if !nocheck {
            let db = self.db(dbname)?;
            for f in &fks {
                let vals = colvals(&r, &f.cols);
                if vals.iter().any(|v| v.is_null()) {
                    continue;
                }
                let ik = jstr(&J::Arr(vals));
                let pt = db
                    .tables
                    .get(&f.ref_table)
                    .ok_or_else(|| format!("foreign key references missing table '{}'", f.ref_table))?;
                let dname = pt
                    .def_for_cols(&f.ref_cols)
                    .ok_or_else(|| format!("fk references non-unique columns ({})", f.ref_cols.join(",")))?;
                let ok = pt
                    .idx
                    .get(&dname)
                    .and_then(|m| m.get(&ik))
                    .map(|v| !v.is_empty())
                    .unwrap_or(false);
                if !ok {
                    return Err(format!(
                        "foreign key violation: ({})={} not found in {}.({})",
                        f.cols.join(","),
                        ik,
                        f.ref_table,
                        f.ref_cols.join(",")
                    ));
                }
            }
        }
        // phase 3: write
        {
            let db = self.db(dbname)?;
            let t = db.tables.get_mut(tname).unwrap();
            if existed {
                t.rm_indexes(&key);
            }
            t.rows.insert(key.clone(), r.clone());
            t.add_indexes(&key, &r);
            t.wlines += 1;
        }
        let op = J::obj(vec![("op", J::s("put")), ("k", J::s(&key)), ("r", J::Obj(r))]);
        self.wal_line(dbname, tname, &op)?;
        if seq_changed {
            self.save_catalog(dbname)?;
        }
        self.maybe_compact(dbname, tname)?;
        Ok(key)
    }

    fn delete_key(&mut self, dbname: &str, tname: &str, key: &str, depth: usize) -> R<u64> {
        if depth > 32 {
            return Err("cascade depth exceeded (possible cycle)".into());
        }
        let row = {
            let db = self.db(dbname)?;
            let t = db
                .tables
                .get(tname)
                .ok_or_else(|| format!("table '{}' not found", tname))?;
            t.rows
                .get(key)
                .cloned()
                .ok_or_else(|| format!("row {} not found in {}", key, tname))?
        };
        // children groups (all tables referencing tname)
        struct CG {
            table: String,
            fki: usize,
            keys: Vec<String>,
        }
        let mut groups: Vec<CG> = Vec::new();
        {
            let db = self.db(dbname)?;
            let names: Vec<String> = db.tables.keys().cloned().collect();
            for cn in &names {
                let ct = &db.tables[cn];
                for (i, f) in ct.fks.iter().enumerate() {
                    if f.ref_table == tname {
                        let pv = colvals(&row, &f.ref_cols);
                        if pv.iter().any(|v| v.is_null()) {
                            continue;
                        }
                        let ik = jstr(&J::Arr(pv));
                        if let Some(ks) =
                            ct.idx.get(&format!("f#{}", i)).and_then(|m| m.get(&ik))
                        {
                            if !ks.is_empty() {
                                groups.push(CG { table: cn.clone(), fki: i, keys: ks.clone() });
                            }
                        }
                    }
                }
            }
        }
        // restrict check across ALL groups first
        for g in &groups {
            let casc = {
                let db = self.db(dbname)?;
                db.tables[&g.table].fks[g.fki].on_delete == "cascade"
            };
            if !casc && !g.keys.is_empty() {
                return Err(format!(
                    "cannot delete: {} child row(s) in '{}' reference this row (use on_delete=cascade)",
                    g.keys.len(),
                    g.table
                ));
            }
        }
        let mut n = 1u64;
        for g in groups {
            for ck in g.keys {
                n += self.delete_key(dbname, &g.table, &ck, depth + 1)?;
            }
        }
        {
            let db = self.db(dbname)?;
            let t = db.tables.get_mut(tname).unwrap();
            t.rows.remove(key);
            t.rm_indexes(key);
            t.wlines += 1;
        }
        let op = J::obj(vec![("op", J::s("del")), ("k", J::s(key))]);
        self.wal_line(dbname, tname, &op)?;
        self.maybe_compact(dbname, tname)?;
        Ok(n)
    }

    // -------- scan --------

    fn scan(
        &mut self,
        dbname: &str,
        tname: &str,
        filters: &[(String, String, J)],
        order: Option<(String, bool)>,
        limit: usize,
        offset: usize,
        proj: Option<Vec<String>>,
    ) -> R<J> {
        let mut out: Vec<(String, Vec<(String, J)>)> = Vec::new();
        {
            let db = self.db(dbname)?;
            let t = db
                .tables
                .get(tname)
                .ok_or_else(|| format!("table '{}' not found", tname))?;
            // index fast path: single equality filter on an indexed column
            let mut used_index = false;
            if filters.len() == 1 && filters[0].1 == "eq" {
                let (fc, _, fv) = (&filters[0].0, &filters[0].1, &filters[0].2);
                if !fv.is_null() {
                    let defs = t.all_index_defs();
                    for (iname, icols, _u) in &defs {
                        if icols.len() == 1 && &icols[0] == fc {
                            let ik = jstr(&J::Arr(vec![fv.clone()]));
                            if let Some(ks) = t.idx.get(iname).and_then(|m| m.get(&ik)) {
                                for k in ks.clone() {
                                    if let Some(row) = t.rows.get(&k) {
                                        out.push((k.clone(), row.clone()));
                                    }
                                }
                            }
                            used_index = true;
                            break;
                        }
                    }
                }
            }
            if !used_index {
                for (k, row) in &t.rows {
                    let mut ok = true;
                    for (c, op, v) in filters {
                        if !match_filter(row, c, op, v) {
                            ok = false;
                            break;
                        }
                    }
                    if ok {
                        out.push((k.clone(), row.clone()));
                    }
                }
            }
            if let Some((ocol, desc)) = &order {
                out.sort_by(|a, b| {
                    let av = colvals(&a.1, std::slice::from_ref(ocol));
                    let bv = colvals(&b.1, std::slice::from_ref(ocol));
                    let c = cmp_vals(&av[0], &bv[0]).unwrap_or(Ordering::Equal);
                    if *desc {
                        c.reverse()
                    } else {
                        c
                    }
                });
            }
        }
        let total = out.len();
        let start = offset.min(total);
        let end = (start + limit).min(total);
        let mut rows = Vec::new();
        let mut keys = Vec::new();
        for (k, row) in out.into_iter().skip(start).take(end - start) {
            let rj = match &proj {
                Some(p) => J::Obj(colvals(&row, p).into_iter().zip(p.iter()).map(|(v, c)| (c.clone(), v)).collect()),
                None => J::Obj(row),
            };
            keys.push(J::s(&k));
            rows.push(rj);
        }
        Ok(J::obj(vec![
            ("table", J::s(tname)),
            ("total", J::Int(total as i64)),
            ("returned", J::Int(rows.len() as i64)),
            ("keys", J::Arr(keys)),
            ("rows", J::Arr(rows)),
        ]))
    }

    fn count_where(&mut self, dbname: &str, tname: &str, filters: &[(String, String, J)]) -> R<i64> {
        let db = self.db(dbname)?;
        let t = db
            .tables
            .get(tname)
            .ok_or_else(|| format!("table '{}' not found", tname))?;
        if filters.is_empty() {
            return Ok(t.rows.len() as i64);
        }
        let mut n = 0i64;
        for row in t.rows.values() {
            let mut ok = true;
            for (c, op, v) in filters {
                if !match_filter(row, c, op, v) {
                    ok = false;
                    break;
                }
            }
            if ok {
                n += 1;
            }
        }
        Ok(n)
    }

    fn clear_table(&mut self, dbname: &str, tname: &str) -> R<()> {
        {
            let db = self.db(dbname)?;
            let t = db
                .tables
                .get_mut(tname)
                .ok_or_else(|| format!("table '{}' not found", tname))?;
            t.rows.clear();
            t.idx.clear();
            t.wlines = 0;
        }
        let sname = sanitize_name(dbname)?;
        let path = self
            .db_dir(&sname)
            .join("wal")
            .join(format!("{}.jsonl", wal_table_name(tname)));
        atomic_write(&path, "")
    }
}

// ---------------- filter matching ----------------

fn match_filter(row: &[(String, J)], col: &str, op: &str, val: &J) -> bool {
    let v = row
        .iter()
        .find(|(k, _)| k == col)
        .map(|(_, v)| v.clone())
        .unwrap_or(J::Null);
    match op {
        "is_null" => v.is_null(),
        "not_null" => !v.is_null(),
        "in" => match val {
            J::Arr(a) => a.iter().any(|x| jeq(&v, x)),
            _ => false,
        },
        _ => {
            if v.is_null() || val.is_null() {
                return false;
            }
            match op {
                "eq" | "=" | "==" => jeq(&v, val),
                "ne" | "!=" | "<>" => !jeq(&v, val),
                "lt" | "<" => matches!(cmp_vals(&v, val), Some(Ordering::Less)),
                "le" | "<=" => matches!(cmp_vals(&v, val), Some(Ordering::Less) | Some(Ordering::Equal)),
                "gt" | ">" => matches!(cmp_vals(&v, val), Some(Ordering::Greater)),
                "ge" | ">=" => matches!(cmp_vals(&v, val), Some(Ordering::Greater) | Some(Ordering::Equal)),
                "like" => match (&v, val) {
                    (J::Str(s), J::Str(p)) => like_match(p, s),
                    _ => false,
                },
                "contains" => match (&v, val) {
                    (J::Str(s), J::Str(p)) => s.contains(p.as_str()),
                    _ => false,
                },
                _ => false,
            }
        }
    }
}

pub fn parse_filters(a: &J) -> R<Vec<(String, String, J)>> {
    let mut out = Vec::new();
    if let Some(w) = a.get("where") {
        let arr = w.as_arr().ok_or("'where' must be an array of [col, op, value]")?;
        for item in arr {
            match item {
                J::Arr(t) if t.len() == 3 => {
                    let c = t[0].as_str().ok_or("where: col must be string")?.to_string();
                    let op = t[1].as_str().ok_or("where: op must be string")?.to_string();
                    out.push((c, op, t[2].clone()));
                }
                J::Obj(_) => {
                    let c = arg_s(item, "col")?;
                    let op = arg_s(item, "op")?;
                    let v = item.get("value").cloned().unwrap_or(J::Null);
                    out.push((c, op, v));
                }
                _ => return Err("where item must be [col, op, value]".into()),
            }
        }
    }
    Ok(out)
}

// ---------------- catalog (de)serialization ----------------

fn table_to_json(t: &Table) -> J {
    let cols = J::Arr(
        t.cols
            .iter()
            .map(|c| {
                J::obj(vec![
                    ("name", J::s(&c.name)),
                    ("dtype", J::s(&c.dtype)),
                    ("nullable", J::Bool(c.nullable)),
                    ("auto", J::Bool(c.auto)),
                ])
            })
            .collect(),
    );
    let fks = J::Arr(
        t.fks
            .iter()
            .map(|f| {
                J::obj(vec![
                    ("columns", J::Arr(f.cols.iter().map(|x| J::s(x)).collect())),
                    ("ref_table", J::s(&f.ref_table)),
                    ("ref_columns", J::Arr(f.ref_cols.iter().map(|x| J::s(x)).collect())),
                    ("on_delete", J::s(&f.on_delete)),
                ])
            })
            .collect(),
    );
    let uniques = J::Arr(
        t.uniques
            .iter()
            .map(|u| J::Arr(u.iter().map(|x| J::s(x)).collect()))
            .collect(),
    );
    let indexes = J::Arr(
        t.indexes
            .iter()
            .map(|x| {
                J::obj(vec![
                    ("name", J::s(&x.name)),
                    ("columns", J::Arr(x.cols.iter().map(|c| J::s(c)).collect())),
                    ("unique", J::Bool(x.unique)),
                ])
            })
            .collect(),
    );
    let auto = J::Obj(t.auto.iter().map(|(k, v)| (k.clone(), J::Int(*v))).collect());
    J::obj(vec![
        ("name", J::s(&t.name)),
        ("columns", cols),
        ("primary_key", J::Arr(t.pk.iter().map(|x| J::s(x)).collect())),
        ("foreign_keys", fks),
        ("unique_keys", uniques),
        ("indexes", indexes),
        ("auto_increment", auto),
        ("nocheck", J::Bool(t.nocheck)),
    ])
}

fn jarr_str(v: Option<&J>) -> Vec<String> {
    v.and_then(|x| x.as_arr())
        .map(|a| a.iter().filter_map(|x| x.as_str().map(|s| s.to_string())).collect())
        .unwrap_or_default()
}

fn table_from_json(v: &J) -> R<Table> {
    let name = arg_s(v, "name")?;
    let mut cols = Vec::new();
    if let Some(ca) = v.get("columns").and_then(|x| x.as_arr()) {
        for cv in ca {
            cols.push(Col {
                name: sanitize_col(&arg_s(cv, "name")?)?,
                dtype: arg_s(cv, "dtype")?,
                nullable: arg_bool_d(cv, "nullable", true),
                auto: arg_bool_d(cv, "auto", false),
            });
        }
    }
    let pk = jarr_str(v.get("primary_key"));
    let mut fks = Vec::new();
    if let Some(fa) = v.get("foreign_keys").and_then(|x| x.as_arr()) {
        for fv in fa {
            fks.push(Fk {
                cols: jarr_str(fv.get("columns")),
                ref_table: arg_s(fv, "ref_table")?,
                ref_cols: jarr_str(fv.get("ref_columns")),
                on_delete: arg_opt_s(fv, "on_delete").unwrap_or_else(|| "restrict".into()),
            });
        }
    }
    let mut uniques = Vec::new();
    if let Some(ua) = v.get("unique_keys").and_then(|x| x.as_arr()) {
        for u in ua {
            uniques.push(jarr_str(Some(u)));
        }
    }
    let mut indexes = Vec::new();
    if let Some(ia) = v.get("indexes").and_then(|x| x.as_arr()) {
        for iv in ia {
            indexes.push(IndexDef {
                name: arg_s(iv, "name")?,
                cols: jarr_str(iv.get("columns")),
                unique: arg_bool_d(iv, "unique", false),
            });
        }
    }
    let mut auto = BTreeMap::new();
    if let Some(aa) = v.get("auto_increment").and_then(|x| x.as_obj()) {
        for (k, val) in aa {
            if let Some(i) = val.as_i64() {
                auto.insert(k.clone(), i);
            }
        }
    }
    Ok(Table {
        name,
        cols,
        pk,
        fks,
        uniques,
        indexes,
        auto,
        nocheck: arg_bool_d(v, "nocheck", false),
        rows: HashMap::new(),
        idx: HashMap::new(),
        wlines: 0,
    })
}

fn db_to_json(db: &Db) -> J {
    J::obj(vec![
        ("name", J::s(&db.name)),
        ("tables", J::Arr(db.tables.values().map(table_to_json).collect())),
    ])
}

// ============================================================================
// KEYS ENGINE — the novel layer built on top of key data
//   db_keys  : full key-graph map (where keys are stored + PK/FK/unique/index)
//   db_topo  : FK-topological ordering (+ cycle detection)
//   db_graph : walk FK edges from a row (children/parents across tables)
//   db_diff  : key-addressed diff between two databases
//   db_snapshot / db_branch / db_merge : git-like versioning by primary key
//   db_sql   : mini SQL (SELECT subset) executed by the key engine
// ============================================================================

fn key_map_json(e: &mut Engine, dbname: &str) -> R<J> {
    let sname = sanitize_name(dbname)?;
    let db = e.db(&sname)?;
    let mut tables = Vec::new();
    let mut edges = Vec::new();
    let names: Vec<String> = db.tables.keys().cloned().collect();
    for n in &names {
        let t = &db.tables[n];
        let mut index_list = Vec::new();
        for x in &t.indexes {
            index_list.push(J::obj(vec![
                ("name", J::s(&x.name)),
                ("columns", J::Arr(x.cols.iter().map(|c| J::s(c)).collect())),
                ("unique", J::Bool(x.unique)),
            ]));
        }
        tables.push(J::obj(vec![
            ("name", J::s(n)),
            ("row_count", J::Int(t.rows.len() as i64)),
            ("primary_key", J::Arr(t.pk.iter().map(|x| J::s(x)).collect())),
            (
                "foreign_keys",
                J::Arr(
                    t.fks
                        .iter()
                        .map(|f| {
                            J::obj(vec![
                                ("columns", J::Arr(f.cols.iter().map(|x| J::s(x)).collect())),
                                ("references", J::s(&format!("{}.({})", f.ref_table, f.ref_cols.join(",")))),
                                ("on_delete", J::s(&f.on_delete)),
                            ])
                        })
                        .collect(),
                ),
            ),
            ("unique_keys", J::Arr(t.uniques.iter().map(|u| J::Arr(u.iter().map(|x| J::s(x)).collect())).collect())),
            ("indexes", J::Arr(index_list)),
            (
                "auto_increment",
                J::Obj(t.auto.iter().map(|(k, v)| (k.clone(), J::Int(v + 1))).collect()),
            ),
            ("fk_enforcement_skipped", J::Bool(t.nocheck)),
        ]));
        for f in &t.fks {
            edges.push(J::obj(vec![
                (
                    "from",
                    J::obj(vec![
                        ("table", J::s(n)),
                        ("columns", J::Arr(f.cols.iter().map(|x| J::s(x)).collect())),
                    ]),
                ),
                (
                    "to",
                    J::obj(vec![
                        ("table", J::s(&f.ref_table)),
                        ("columns", J::Arr(f.ref_cols.iter().map(|x| J::s(x)).collect())),
                    ]),
                ),
                ("on_delete", J::s(&f.on_delete)),
            ]));
        }
    }
    let order = topo_order(&db.tables).unwrap_or_default();
    let home_disp = e.home.display().to_string();
    Ok(J::obj(vec![
        ("database", J::s(&sname)),
        ("kind", J::s("local")),
        (
            "where_keys_are_stored",
            J::obj(vec![
                ("schema_and_key_definitions", J::s(&format!("{}/databases/{}/catalog.json", home_disp, sname))),
                ("row_keys", J::s(&format!(
                    "{}/databases/{}/wal/<table>.jsonl — every line is a row addressed by its primary-key value",
                    home_disp, sname
                ))),
                ("indexes", J::s("in-memory BTreeMap<key_values, row_primary_keys>, rebuilt from rows on open")),
                ("snapshots", J::s(&format!("{}/snapshots/{}/*.json", home_disp, sname))),
                ("extract_bundles", J::s(&format!("{}/extracts/*.json", home_disp))),
            ]),
        ),
        ("tables", J::Arr(tables)),
        ("edges", J::Arr(edges)),
        ("topological_order", J::Arr(order.iter().map(|x| J::s(x)).collect())),
    ]))
}

fn topo_order(tables: &BTreeMap<String, Table>) -> R<Vec<String>> {
    let names: Vec<String> = tables.keys().cloned().collect();
    let mut indeg: HashMap<String, usize> = HashMap::new();
    let mut adj: HashMap<String, Vec<String>> = HashMap::new();
    for n in &names {
        indeg.entry(n.clone()).or_insert(0);
    }
    for t in tables.values() {
        for f in &t.fks {
            if f.ref_table != t.name && tables.contains_key(&f.ref_table) {
                adj.entry(f.ref_table.clone()).or_default().push(t.name.clone());
                *indeg.entry(t.name.clone()).or_insert(0) += 1;
            }
        }
    }
    let mut q: Vec<String> = names
        .iter()
        .filter(|n| indeg.get(*n).copied().unwrap_or(0) == 0)
        .cloned()
        .collect();
    let mut order = Vec::new();
    while let Some(n) = q.pop() {
        order.push(n.clone());
        if let Some(ch) = adj.get(&n).cloned() {
            for c in ch {
                let e = indeg.get_mut(&c).unwrap();
                *e -= 1;
                if *e == 0 {
                    q.push(c);
                }
            }
        }
    }
    if order.len() < names.len() {
        let rest: Vec<String> = names
            .iter()
            .filter(|n| !order.contains(n))
            .cloned()
            .collect();
        return Err(format!("circular foreign keys among: {}", rest.join(", ")));
    }
    Ok(order)
}

fn graph_walk(e: &mut Engine, dbname: &str, tname: &str, key: &str, hops: usize, dir: &str) -> R<J> {
    let mut nodes: Vec<J> = Vec::new();
    let mut edges: Vec<J> = Vec::new();
    let mut visited: HashSet<(String, String)> = HashSet::new();
    let mut queue: Vec<(String, String, usize)> = vec![(tname.to_string(), key.to_string(), 0)];
    visited.insert((tname.to_string(), key.to_string()));
    while let Some((t, k, depth)) = queue.pop() {
        let row = {
            let db = e.db(dbname)?;
            match db.tables.get(&t).and_then(|tt| tt.rows.get(&k)) {
                Some(r) => r.clone(),
                None => {
                    if nodes.is_empty() {
                        return Err(format!("row {} not found in {}", k, t));
                    }
                    continue;
                }
            }
        };
        nodes.push(J::obj(vec![
            ("table", J::s(&t)),
            ("key", J::s(&k)),
            ("row", J::Obj(row.clone())),
            ("depth", J::Int(depth as i64)),
        ]));
        if nodes.len() >= 200 || depth >= hops {
            continue;
        }
        // outward: children referencing me
        if dir == "out" || dir == "both" {
            let refs: Vec<(String, usize, Vec<String>)> = {
                let db = e.db(dbname)?;
                let mut v = Vec::new();
                let names: Vec<String> = db.tables.keys().cloned().collect();
                for cn in &names {
                    let ct = &db.tables[cn];
                    for (i, f) in ct.fks.iter().enumerate() {
                        if f.ref_table == t {
                            let pv = colvals(&row, &f.ref_cols);
                            if pv.iter().any(|x| x.is_null()) {
                                continue;
                            }
                            let ik = jstr(&J::Arr(pv));
                            if let Some(ks) =
                                ct.idx.get(&format!("f#{}", i)).and_then(|m| m.get(&ik))
                            {
                                v.push((cn.clone(), i, ks.clone()));
                            }
                        }
                    }
                }
                v
            };
            for (cn, _i, ks) in refs {
                for ck in ks {
                    let edge = J::obj(vec![
                        ("from", J::obj(vec![("table", J::s(&t)), ("key", J::s(&k))])),
                        ("to", J::obj(vec![("table", J::s(&cn)), ("key", J::s(&ck))])),
                        ("relation", J::s("referenced_by")),
                    ]);
                    if edges.len() < 500 {
                        edges.push(edge);
                    }
                    if visited.insert((cn.clone(), ck.clone())) && nodes.len() < 200 {
                        queue.push((cn.clone(), ck.clone(), depth + 1));
                    }
                }
            }
        }
        // inward: parents I reference
        if dir == "in" || dir == "both" {
            let fks: Vec<Fk> = {
                let db = e.db(dbname)?;
                db.tables[&t].fks.clone()
            };
            for f in &fks {
                let pv = colvals(&row, &f.cols);
                if pv.iter().any(|x| x.is_null()) {
                    continue;
                }
                let ik = jstr(&J::Arr(pv.clone()));
                let parents: Vec<String> = {
                    let db = e.db(dbname)?;
                    match db.tables.get(&f.ref_table) {
                        Some(pt) => match pt.def_for_cols(&f.ref_cols) {
                            Some(d) => pt
                                .idx
                                .get(&d)
                                .and_then(|m| m.get(&ik))
                                .cloned()
                                .unwrap_or_default(),
                            None => Vec::new(),
                        },
                        None => Vec::new(),
                    }
                };
                for pk_ in parents {
                    let edge = J::obj(vec![
                        ("from", J::obj(vec![("table", J::s(&t)), ("key", J::s(&k))])),
                        ("to", J::obj(vec![("table", J::s(&f.ref_table)), ("key", J::s(&pk_))])),
                        ("relation", J::s(&format!("references {}.({})", f.ref_table, f.ref_cols.join(",")))),
                    ]);
                    if edges.len() < 500 {
                        edges.push(edge);
                    }
                    if visited.insert((f.ref_table.clone(), pk_.clone())) && nodes.len() < 200 {
                        queue.push((f.ref_table.clone(), pk_.clone(), depth + 1));
                    }
                }
            }
        }
    }
    Ok(J::obj(vec![
        ("start", J::s(&format!("{}#{}", tname, key))),
        ("nodes", J::Int(nodes.len() as i64)),
        ("edges_count", J::Int(edges.len() as i64)),
        ("node_list", J::Arr(nodes)),
        ("edge_list", J::Arr(edges)),
    ]))
}

fn row_canon(row: &[(String, J)]) -> Vec<(String, J)> {
    let mut v = row.to_vec();
    v.sort_by(|a, b| a.0.cmp(&b.0));
    v
}

fn row_eq(a: &[(String, J)], b: &[(String, J)]) -> bool {
    row_canon(a) == row_canon(b)
}

fn diff_databases(e: &mut Engine, dfa: &str, dfb: &str, limit: usize) -> R<J> {
    let sa = sanitize_name(dfa)?;
    let sb = sanitize_name(dfb)?;
    let mut tables = Vec::new();
    let names: Vec<String> = {
        let dba = e.db(&sa)?;
        dba.tables.keys().cloned().collect()
    };
    let mut total_changes = 0i64;
    for n in &names {
        let a_rows = {
            let dba = e.db(&sa)?;
            dba.tables[n].rows.clone()
        };
        let b_rows = {
            let dbb = e.db(&sb)?;
            dbb.tables.get(n).map(|t| t.rows.clone())
        };
        let (a_rows, b_rows, b_exists) = match b_rows {
            Some(r) => (a_rows, r, true),
            None => (a_rows, HashMap::new(), false),
        };
        if !b_exists {
            tables.push(J::obj(vec![
                ("table", J::s(n)),
                ("status", J::s("only_in_a")),
                ("rows_in_a", J::Int(a_rows.len() as i64)),
            ]));
            continue;
        }
        let mut added = Vec::new();
        let mut removed = Vec::new();
        let mut changed = Vec::new();
        for (k, br) in &b_rows {
            match a_rows.get(k) {
                None => added.push(J::s(k)),
                Some(ar) => {
                    if !row_eq(ar, br) {
                        changed.push(J::s(k));
                    }
                }
            }
        }
        for k in a_rows.keys() {
            if !b_rows.contains_key(k) {
                removed.push(J::s(k));
            }
        }
        total_changes += (added.len() + removed.len() + changed.len()) as i64;
        let cap = |v: &Vec<J>| -> J {
            let mut vv = v.clone();
            vv.truncate(limit);
            J::Arr(vv)
        };
        tables.push(J::obj(vec![
            ("table", J::s(n)),
            ("added", J::Int(added.len() as i64)),
            ("removed", J::Int(removed.len() as i64)),
            ("changed", J::Int(changed.len() as i64)),
            ("added_keys", cap(&added)),
            ("removed_keys", cap(&removed)),
            ("changed_keys", cap(&changed)),
        ]));
    }
    Ok(J::obj(vec![
        ("a", J::s(&sa)),
        ("b", J::s(&sb)),
        ("total_changes", J::Int(total_changes)),
        ("tables", J::Arr(tables)),
    ]))
}

// ---------------- snapshots / branches / merge ----------------

fn snapshot_create(e: &mut Engine, dbname: &str, name: &str) -> R<J> {
    let sname = sanitize_name(dbname)?;
    let nm = sanitize_name(name)?;
    let (catalog, rows) = {
        let db = e.db(&sname)?;
        let mut rows = BTreeMap::new();
        for (tn, t) in &db.tables {
            let mut list: Vec<Vec<(String, J)>> = t.rows.values().cloned().collect();
            list.sort_by(|a, b| jstr(&J::Obj(a.clone())).cmp(&jstr(&J::Obj(b.clone()))));
            rows.insert(
                tn.clone(),
                J::Arr(list.into_iter().map(|r| J::Obj(r.clone())).collect()),
            );
        }
        (db_to_json(db), J::Obj(rows.into_iter().collect()))
    };
    let snap = J::obj(vec![
        ("kind", J::s("dbengine-snapshot")),
        ("db", J::s(&sname)),
        ("name", J::s(&nm)),
        ("created", J::Int(now_ms() as i64)),
        ("catalog", catalog),
        ("rows", rows),
    ]);
    let path = e.home.join("snapshots").join(&sname).join(format!("{}.json", nm));
    atomic_write(&path, &jpretty(&snap))?;
    Ok(J::obj(vec![
        ("snapshot", J::s(&format!("{}/{}", sname, nm))),
        ("path", J::s(&path.display().to_string())),
        ("bytes", J::Int(jstr(&snap).len() as i64)),
    ]))
}

fn load_snapshot_file(e: &Engine, dbname: &str, name: &str) -> R<J> {
    let sname = sanitize_name(dbname)?;
    let nm = sanitize_name(name)?;
    let path = e.home.join("snapshots").join(&sname).join(format!("{}.json", nm));
    let data = fs::read_to_string(&path).map_err(|_| format!("snapshot '{}' not found ({})", nm, path.display()))?;
    jparse(&data)
}

fn db_from_snapshot(j: &J, new_name: &str) -> R<Db> {
    let mut db = Db { name: new_name.to_string(), tables: BTreeMap::new() };
    if let Some(ts) = j.get("catalog").and_then(|c| c.get("tables")).and_then(|x| x.as_arr()) {
        for tv in ts {
            let mut t = table_from_json(tv)?;
            if let Some(rs) = j.get("rows").and_then(|r| r.get(&t.name)).and_then(|x| x.as_arr()) {
                for rv in rs {
                    if let J::Obj(m) = rv {
                        let pkv = colvals(m, &t.pk);
                        if !pkv.iter().any(|v| v.is_null()) && !t.pk.is_empty() {
                            let k = jstr(&J::Arr(pkv));
                            t.rows.insert(k, m.clone());
                        }
                    }
                }
            }
            t.rebuild_indexes();
            t.wlines = t.rows.len() as u64;
            db.tables.insert(t.name.clone(), t);
        }
    }
    Ok(db)
}

fn write_db_files(e: &mut Engine, dbname: &str) -> R<()> {
    let sname = sanitize_name(dbname)?;
    e.save_catalog(&sname)?;
    let dir = e.db_dir(&sname);
    let tnames: Vec<String> = {
        let d = e.dbs.get(&sname).unwrap();
        d.tables.keys().cloned().collect()
    };
    for tn in &tnames {
        let mut lines = String::new();
        {
            let d = e.dbs.get(&sname).unwrap();
            let t = d.tables.get(tn).unwrap();
            let mut keys: Vec<&String> = t.rows.keys().collect();
            keys.sort();
            for k in keys {
                let op = J::obj(vec![
                    ("op", J::s("put")),
                    ("k", J::s(k)),
                    ("r", J::Obj(t.rows[k].clone())),
                ]);
                lines.push_str(&jstr(&op));
                lines.push('\n');
            }
        }
        atomic_write(
            &dir.join("wal").join(format!("{}.jsonl", wal_table_name(tn))),
            &lines,
        )?;
        let d = e.dbs.get_mut(&sname).unwrap();
        let t = d.tables.get_mut(tn).unwrap();
        t.wlines = t.rows.len() as u64;
    }
    Ok(())
}

fn snapshot_restore(e: &mut Engine, dbname: &str, name: &str) -> R<J> {
    let sname = sanitize_name(dbname)?;
    let snap = load_snapshot_file(e, &sname, name)?;
    let db = db_from_snapshot(&snap, &sname)?;
    let nrows: usize = db.tables.values().map(|t| t.rows.len()).sum();
    let ntables = db.tables.len();
    e.dbs.insert(sname.clone(), db);
    write_db_files(e, &sname)?;
    Ok(J::obj(vec![
        ("restored", J::s(&format!("{}/{}", sname, name))),
        ("tables", J::Int(ntables as i64)),
        ("rows", J::Int(nrows as i64)),
    ]))
}

fn snapshot_list(e: &mut Engine, dbname: &str) -> R<J> {
    let sname = sanitize_name(dbname)?;
    let dir = e.home.join("snapshots").join(&sname);
    let mut out = Vec::new();
    if let Ok(rd) = fs::read_dir(&dir) {
        let mut items: Vec<_> = rd
            .filter_map(|x| x.ok())
            .filter(|x| x.path().extension().map(|e| e == "json").unwrap_or(false))
            .map(|x| {
                let meta = x.metadata().ok();
                let size = meta.as_ref().map(|m| m.len()).unwrap_or(0);
                let modt = meta
                    .and_then(|m| m.modified().ok())
                    .and_then(|t| t.duration_since(UNIX_EPOCH).ok())
                    .map(|d| d.as_millis() as u64)
                    .unwrap_or(0);
                J::obj(vec![
                    ("name", J::s(&x.path().file_stem().unwrap_or_default().to_string_lossy())),
                    ("bytes", J::Int(size as i64)),
                    ("mtime_ms", J::Int(modt as i64)),
                ])
            })
            .collect();
        items.sort_by(|a, b| {
            let ka = match a.get("name") {
                Some(J::Str(s)) => s.clone(),
                _ => String::new(),
            };
            let kb = match b.get("name") {
                Some(J::Str(s)) => s.clone(),
                _ => String::new(),
            };
            ka.cmp(&kb)
        });
        out = items;
    }
    Ok(J::obj(vec![("database", J::s(&sname)), ("snapshots", J::Arr(out))]))
}

fn snapshot_drop(e: &mut Engine, dbname: &str, name: &str) -> R<J> {
    let sname = sanitize_name(dbname)?;
    let nm = sanitize_name(name)?;
    let path = e.home.join("snapshots").join(&sname).join(format!("{}.json", nm));
    match fs::remove_file(&path) {
        Ok(_) => Ok(J::obj(vec![("dropped", J::s(&format!("{}/{}", sname, nm)))])),
        Err(_) => Err(format!("snapshot '{}' not found", nm)),
    }
}

fn branch_create(e: &mut Engine, dbname: &str, branch: &str, from_snapshot: Option<&str>) -> R<J> {
    let src = sanitize_name(dbname)?;
    let bname = sanitize_name(branch)?;
    let db = match from_snapshot {
        Some(sn) => {
            let snap = load_snapshot_file(e, &src, sn)?;
            db_from_snapshot(&snap, &bname)?
        }
        None => {
            e.db(&src)?; // ensure open
            let mut d = e.dbs.get(&src).unwrap().clone();
            d.name = bname.clone();
            d
        }
    };
    let nrows: usize = db.tables.values().map(|t| t.rows.len()).sum();
    let ntables = db.tables.len();
    e.dbs.insert(bname.clone(), db);
    write_db_files(e, &bname)?;
    Ok(J::obj(vec![
        ("branch", J::s(&bname)),
        ("from", J::s(&src)),
        ("tables", J::Int(ntables as i64)),
        ("rows", J::Int(nrows as i64)),
    ]))
}

fn merge_databases(e: &mut Engine, from: &str, into: &str, strategy: &str) -> R<J> {
    let sf = sanitize_name(from)?;
    let si = sanitize_name(into)?;
    if sf == si {
        return Err("cannot merge a database into itself".into());
    }
    e.db(&sf)?;
    e.db(&si)?;
    let order = {
        let d = e.dbs.get(&sf).unwrap();
        topo_order(&d.tables)?
    };
    // ensure tables exist in target (clone defs from source)
    for tn in &order {
        let def = {
            let d = e.dbs.get(&sf).unwrap();
            table_to_json(&d.tables[tn])
        };
        let ti = e.dbs.get_mut(&si).unwrap();
        if !ti.tables.contains_key(tn) {
            let mut t = table_from_json(&def)?;
            t.wlines = 0;
            ti.tables.insert(tn.clone(), t);
        }
    }
    if strategy == "replace" {
        for tn in order.iter().rev() {
            e.clear_table(&si, tn)?;
        }
    }
    let mut report = Vec::new();
    for tn in &order {
        let items: Vec<(String, Vec<(String, J)>)> = {
            let d = e.dbs.get(&sf).unwrap();
            d.tables[tn]
                .rows
                .iter()
                .map(|(k, v)| (k.clone(), v.clone()))
                .collect()
        };
        let mut inserted = 0i64;
        let mut updated = 0i64;
        for (k, row) in &items {
            let existed = {
                let d = e.dbs.get(&si).unwrap();
                d.tables[tn].rows.contains_key(k)
            };
            if strategy == "skip" && existed {
                continue;
            }
            e.put_row(&si, tn, &J::Obj(row.clone()), true)?;
            if existed {
                updated += 1;
            } else {
                inserted += 1;
            }
        }
        report.push(J::obj(vec![
            ("table", J::s(tn)),
            ("inserted", J::Int(inserted)),
            ("updated", J::Int(updated)),
        ]));
    }
    e.save_catalog(&si)?;
    Ok(J::obj(vec![
        ("from", J::s(&sf)),
        ("into", J::s(&si)),
        ("strategy", J::s(strategy)),
        ("tables", J::Arr(report)),
    ]))
}

// ============================================================================
// mini SQL — SELECT subset executed by the key engine
// ============================================================================

#[derive(Clone, Debug)]
enum STok {
    W(String),
    S(String),
    Num(String),
    P(String),
}

fn stok(q: &str) -> R<Vec<STok>> {
    let b = q.as_bytes();
    let mut i = 0usize;
    let mut out = Vec::new();
    while i < b.len() {
        let c = b[i];
        if c.is_ascii_whitespace() {
            i += 1;
            continue;
        }
        if c == b'\'' {
            i += 1;
            let mut s = String::new();
            loop {
                if i >= b.len() {
                    return Err("unterminated string literal".into());
                }
                if b[i] == b'\'' {
                    if i + 1 < b.len() && b[i + 1] == b'\'' {
                        s.push('\'');
                        i += 2;
                        continue;
                    }
                    i += 1;
                    break;
                }
                let st = i;
                let l = utf8_len(b[i]).min(b.len() - i);
                i += l;
                s.push_str(&String::from_utf8_lossy(&b[st..i]));
            }
            out.push(STok::S(s));
            continue;
        }
        if c == b'"' || c == b'`' {
            i += 1;
            let st = i;
            while i < b.len() && b[i] != c {
                i += 1;
            }
            if i >= b.len() {
                return Err("unterminated quoted identifier".into());
            }
            out.push(STok::W(String::from_utf8_lossy(&b[st..i]).to_string()));
            i += 1;
            continue;
        }
        if c.is_ascii_digit() {
            let st = i;
            while i < b.len() && (b[i].is_ascii_digit() || b[i] == b'.') {
                i += 1;
            }
            out.push(STok::Num(String::from_utf8_lossy(&b[st..i]).to_string()));
            continue;
        }
        if c.is_ascii_alphanumeric() || c == b'_' {
            let st = i;
            while i < b.len() && (b[i].is_ascii_alphanumeric() || b[i] == b'_') {
                i += 1;
            }
            out.push(STok::W(String::from_utf8_lossy(&b[st..i]).to_string()));
            continue;
        }
        let two: String = String::from_utf8_lossy(&b[i..(i + 2).min(b.len())]).to_string();
        if two == "<=" || two == ">=" || two == "!=" || two == "<>" || two == "==" {
            out.push(STok::P(two));
            i += 2;
            continue;
        }
        out.push(STok::P((c as char).to_string()));
        i += 1;
    }
    Ok(out)
}

struct SP {
    t: Vec<STok>,
    i: usize,
}

impl SP {
    fn peek(&self) -> Option<&STok> {
        self.t.get(self.i)
    }
    fn next(&mut self) -> R<STok> {
        let v = self.t.get(self.i).cloned().ok_or("unexpected end of sql")?;
        self.i += 1;
        Ok(v)
    }
    fn kw(&self, w: &str) -> bool {
        matches!(self.peek(), Some(STok::W(s)) if s.eq_ignore_ascii_case(w))
    }
    fn eat_kw(&mut self, w: &str) -> bool {
        if self.kw(w) {
            self.i += 1;
            true
        } else {
            false
        }
    }
    fn expect_kw(&mut self, w: &str) -> R<()> {
        if self.eat_kw(w) {
            Ok(())
        } else {
            Err(format!("expected keyword {}", w.to_uppercase()))
        }
    }
    fn expect_p(&mut self, p: &str) -> R<()> {
        match self.next()? {
            STok::P(x) if x == p => Ok(()),
            _ => Err(format!("expected '{}'", p)),
        }
    }
    fn ident(&mut self) -> R<String> {
        match self.next()? {
            STok::W(s) => Ok(s),
            _ => Err("expected identifier".into()),
        }
    }
    fn literal(&mut self) -> R<J> {
        match self.next()? {
            STok::S(s) => Ok(J::Str(s)),
            STok::Num(n) => {
                if let Ok(i) = n.parse::<i64>() {
                    Ok(J::Int(i))
                } else {
                    n.parse::<f64>().map(J::Num).map_err(|_| "bad number".into())
                }
            }
            STok::W(w) => match w.to_ascii_lowercase().as_str() {
                "null" => Ok(J::Null),
                "true" => Ok(J::Bool(true)),
                "false" => Ok(J::Bool(false)),
                _ => Err(format!("unexpected word '{}' in expression", w)),
            },
            _ => Err("expected literal value".into()),
        }
    }
}

struct SqlQ {
    cols: Vec<String>,
    star: bool,
    table: String,
    conds: Vec<(String, String, J)>,
    order: Option<(String, bool)>,
    limit: usize,
    offset: usize,
}

fn sql_parse(q: &str) -> R<SqlQ> {
    let toks = stok(q)?;
    let mut p = SP { t: toks, i: 0 };
    p.expect_kw("select")?;
    let mut cols = Vec::new();
    let mut star = false;
    if matches!(p.peek(), Some(STok::P(x)) if x == "*") {
        p.i += 1;
        star = true;
    } else {
        loop {
            cols.push(p.ident()?);
            if matches!(p.peek(), Some(STok::P(x)) if x == ",") {
                p.i += 1;
            } else {
                break;
            }
        }
    }
    p.expect_kw("from")?;
    let table = p.ident()?;
    let mut conds = Vec::new();
    if p.eat_kw("where") {
        loop {
            let col = p.ident()?;
            let op: String = match p.next()? {
                STok::P(x) => x,
                STok::W(w) => w.to_ascii_lowercase(),
                _ => return Err("expected operator".into()),
            };
            let op = match op.as_str() {
                "==" => "=".to_string(),
                "<>" => "!=".to_string(),
                o => o.to_string(),
            };
            if op == "is" {
                if p.eat_kw("not") {
                    p.expect_kw("null")?;
                    conds.push((col, "not_null".into(), J::Null));
                } else {
                    p.expect_kw("null")?;
                    conds.push((col, "is_null".into(), J::Null));
                }
            } else if op == "in" {
                p.expect_p("(")?;
                let mut items = Vec::new();
                loop {
                    items.push(p.literal()?);
                    match p.next()? {
                        STok::P(x) if x == "," => continue,
                        STok::P(x) if x == ")" => break,
                        _ => return Err("expected ',' or ')' in IN list".into()),
                    }
                }
                conds.push((col, "in".into(), J::Arr(items)));
            } else {
                let v = p.literal()?;
                conds.push((col, op, v));
            }
            if p.eat_kw("and") {
                continue;
            }
            break;
        }
    }
    let mut order = None;
    if p.eat_kw("order") {
        p.expect_kw("by")?;
        let col = p.ident()?;
        let mut desc = false;
        if p.eat_kw("desc") {
            desc = true;
        } else {
            p.eat_kw("asc");
        }
        order = Some((col, desc));
    }
    let mut limit = 200usize;
    let mut offset = 0usize;
    if p.eat_kw("limit") {
        match p.next()? {
            STok::Num(n) => limit = n.parse::<usize>().unwrap_or(200),
            _ => return Err("LIMIT expects a number".into()),
        }
    }
    if p.eat_kw("offset") {
        match p.next()? {
            STok::Num(n) => offset = n.parse::<usize>().unwrap_or(0),
            _ => return Err("OFFSET expects a number".into()),
        }
    }
    if p.i != p.t.len() {
        return Err("unexpected trailing tokens in sql".into());
    }
    Ok(SqlQ { cols, star, table, conds, order, limit, offset })
}

fn sql_exec(e: &mut Engine, dbname: &str, q: &str) -> R<J> {
    let parsed = sql_parse(q)?;
    let res = e.scan(
        dbname,
        &parsed.table,
        &parsed.conds,
        parsed.order.clone(),
        parsed.limit,
        parsed.offset,
        None,
    )?;
    if parsed.star {
        return Ok(res);
    }
    // project requested columns
    let rows = res.get("rows").and_then(|x| x.as_arr()).cloned().unwrap_or_default();
    let mut proj = Vec::new();
    for r in &rows {
        let mut m = Vec::new();
        for c in &parsed.cols {
            m.push((c.clone(), r.get(c).cloned().unwrap_or(J::Null)));
        }
        proj.push(J::Obj(m));
    }
    Ok(J::obj(vec![
        ("table", J::s(&parsed.table)),
        ("columns", J::Arr(parsed.cols.iter().map(|x| J::s(x)).collect())),
        ("returned", J::Int(proj.len() as i64)),
        ("rows", J::Arr(proj)),
    ]))
}

// ============================================================================
// REMOTE — real wire-protocol clients, written from scratch (std TCP only)
//   postgres : startup + md5 / cleartext / SCRAM-SHA-256 auth + simple query
//   mysql    : handshake v10 + mysql_native_password / caching_sha2 fast auth
//              + COM_QUERY text resultsets
//   http     : plain-HTTP JSON endpoints (REST). HTTPS needs a local proxy.
// Plus: catalog introspection (keys from pg_catalog / INFORMATION_SCHEMA),
// FK-topological extractor (resumable by key), key-based incremental sync.
// ============================================================================

struct QRes {
    cols: Vec<String>,
    rows: Vec<Vec<J>>,
    tag: String,
}

fn text_to_j(v: &str) -> J {
    if let Ok(i) = v.parse::<i64>() {
        return J::Int(i);
    }
    if let Ok(f) = v.parse::<f64>() {
        if f.is_finite() {
            return J::Num(f);
        }
    }
    J::Str(v.to_string())
}

fn be16(b: &[u8]) -> u16 {
    ((b[0] as u16) << 8) | b[1] as u16
}
fn be32(b: &[u8]) -> u32 {
    ((b[0] as u32) << 24) | ((b[1] as u32) << 16) | ((b[2] as u32) << 8) | b[3] as u32
}
fn le16(b: &[u8]) -> u16 {
    (b[0] as u16) | ((b[1] as u16) << 8)
}
fn le24(b: &[u8]) -> usize {
    b[0] as usize | ((b[1] as usize) << 8) | ((b[2] as usize) << 16)
}
fn le32(b: &[u8]) -> u32 {
    (b[0] as u32) | ((b[1] as u32) << 8) | ((b[2] as u32) << 16) | ((b[3] as u32) << 24)
}
fn le64(b: &[u8]) -> u64 {
    let mut v = 0u64;
    for i in 0..8 {
        v |= (b[i] as u64) << (8 * i);
    }
    v
}

fn read_cstr(b: &[u8], mut i: usize) -> (String, usize) {
    if i > b.len() {
        // Out-of-range start index: never slice past the end.
        return (String::new(), b.len());
    }
    let st = i;
    while i < b.len() && b[i] != 0 {
        i += 1;
    }
    (String::from_utf8_lossy(&b[st..i]).to_string(), (i + 1).min(b.len().max(1)))
}

// ---------------- POSTGRES ----------------

struct PgConn {
    s: TcpStream,
}

fn pg_err(pl: &[u8]) -> String {
    let mut i = 0usize;
    let mut sev = String::new();
    let mut code = String::new();
    let mut msg = String::new();
    while i < pl.len() && pl[i] != 0 {
        let f = pl[i];
        i += 1;
        let (v, ni) = read_cstr(pl, i);
        i = ni;
        match f {
            b'S' | b'V' => sev = v,
            b'C' => code = v,
            b'M' => msg = v,
            _ => {}
        }
    }
    format!("postgres error [{} {}]: {}", sev, code, msg)
}

impl PgConn {
    fn connect(host: &str, port: u16, user: &str, password: &str, dbname: &str) -> R<PgConn> {
        let s = TcpStream::connect((host, port)).map_err(|e| format!("postgres connect {}: {}", host, e))?;
        s.set_read_timeout(Some(Duration::from_secs(60))).ok();
        s.set_write_timeout(Some(Duration::from_secs(60))).ok();
        let mut c = PgConn { s };
        let mut st = Vec::new();
        st.extend_from_slice(&196608u32.to_be_bytes());
        for (k, v) in [("user", user), ("database", dbname)] {
            st.extend_from_slice(k.as_bytes());
            st.push(0);
            st.extend_from_slice(v.as_bytes());
            st.push(0);
        }
        st.push(0);
        let mut framed = Vec::new();
        framed.extend_from_slice(&(st.len() as i32 + 4).to_be_bytes());
        framed.extend_from_slice(&st);
        c.s.write_all(&framed).map_err(|e| format!("pg write: {}", e))?;
        loop {
            let (t, pl) = c.msg()?;
            match t {
                b'R' => c.auth_msg(&pl, user, password)?,
                b'K' | b'S' | b'N' => {}
                b'Z' => break,
                b'E' => return Err(pg_err(&pl)),
                _ => {}
            }
        }
        Ok(c)
    }

    fn msg(&mut self) -> R<(u8, Vec<u8>)> {
        let mut h = [0u8; 5];
        self.s.read_exact(&mut h).map_err(|e| format!("pg read: {}", e))?;
        let t = h[0];
        let len = be32(&h[1..5]) as usize;
        if len < 4 {
            return Err("pg bad message length".into());
        }
        // Cap the declared payload (matches the tunnel's frame cap) so a
        // hostile server cannot force a multi-GB allocation.
        if len - 4 > 64 * 1024 * 1024 {
            return Err("postgres message too large".into());
        }
        let mut pl = vec![0u8; len - 4];
        if len > 4 {
            self.s.read_exact(&mut pl).map_err(|e| format!("pg read: {}", e))?;
        }
        Ok((t, pl))
    }

    fn send_msg(&mut self, t: u8, payload: &[u8]) -> R<()> {
        let mut v = Vec::with_capacity(payload.len() + 5);
        v.push(t);
        v.extend_from_slice(&((payload.len() as i32 + 4).to_be_bytes()));
        v.extend_from_slice(payload);
        self.s.write_all(&v).map_err(|e| format!("pg write: {}", e))
    }

    fn auth_msg(&mut self, pl: &[u8], user: &str, password: &str) -> R<()> {
        if pl.len() < 4 {
            return Err("pg bad auth message".into());
        }
        let code = be32(&pl[0..4]) as i32;
        match code {
            0 => Ok(()),
            3 => {
                // cleartext
                let mut p = password.as_bytes().to_vec();
                p.push(0);
                self.send_msg(b'p', &p)
            }
            5 => {
                // md5
                if pl.len() < 8 {
                    return Err("pg bad md5 salt".into());
                }
                let salt = &pl[4..8];
                let h1 = hexe(&md5(format!("{}{}", password, user).as_bytes()));
                let mut m = h1.as_bytes().to_vec();
                m.extend_from_slice(salt);
                let h2 = hexe(&md5(&m));
                let resp = format!("md5{}", h2);
                let mut p = resp.as_bytes().to_vec();
                p.push(0);
                self.send_msg(b'p', &p)
            }
            10 => {
                // SASL — SCRAM-SHA-256
                let mut mechs = Vec::new();
                let mut i = 4usize;
                while i < pl.len() && pl[i] != 0 {
                    let (m, ni) = read_cstr(pl, i);
                    i = ni;
                    mechs.push(m);
                }
                if !mechs.iter().any(|m| m == "SCRAM-SHA-256") {
                    return Err(format!("pg: unsupported SASL mechanisms {:?}", mechs));
                }
                let cnonce = b64e(&rand_bytes(18));
                let cfbare = format!("n=,r={}", cnonce);
                let cfirst = format!("n,,{}", cfbare);
                let mut pld = Vec::new();
                pld.extend_from_slice(b"SCRAM-SHA-256");
                pld.push(0);
                pld.extend_from_slice(&(cfbare.len() as i32).to_be_bytes());
                pld.extend_from_slice(cfirst.as_bytes());
                self.send_msg(b'p', &pld)?;
                let (t2, pl2) = self.msg()?;
                if t2 != b'R' {
                    return Err("pg: expected SASLContinue".into());
                }
                if pl2.len() < 4 || be32(&pl2[0..4]) as i32 != 11 {
                    return Err("pg: expected SASLContinue(11)".into());
                }
                let sfirst = String::from_utf8_lossy(&pl2[4..]).to_string();
                let mut rnonce = String::new();
                let mut salt_s = String::new();
                let mut iters_s = String::new();
                for part in sfirst.split(',') {
                    if let Some(v) = part.strip_prefix("r=") {
                        rnonce = v.to_string();
                    }
                    if let Some(v) = part.strip_prefix("s=") {
                        salt_s = v.to_string();
                    }
                    if let Some(v) = part.strip_prefix("i=") {
                        iters_s = v.to_string();
                    }
                }
                if rnonce.is_empty() || salt_s.is_empty() {
                    return Err("pg: bad server-first-message".into());
                }
                let iters: u32 = iters_s.parse().unwrap_or(4096);
                let salt = b64d(&salt_s)?;
                let salted = pbkdf2_sha256(password.as_bytes(), &salt, iters, 32);
                let ckey = hmac_sha256(&salted, b"Client Key");
                let skey = sha256(&ckey);
                let cfinalbare = format!("c=biws,r={}", rnonce);
                let authmsg = format!("{},{},{}", cfbare, sfirst, cfinalbare);
                let csig = hmac_sha256(&skey, authmsg.as_bytes());
                let proof = xor_vec(&ckey, &csig);
                let fin = format!("{},p={}", cfinalbare, b64e(&proof));
                self.send_msg(b'p', fin.as_bytes())?;
                // read until auth completes (do not consume ReadyForQuery)
                loop {
                    let (t3, pl3) = self.msg()?;
                    match t3 {
                        b'R' => {
                            let c3 = be32(&pl3[0..4]) as i32;
                            match c3 {
                                12 => {
                                    // SASLFinal: verify server signature
                                    let smsg = String::from_utf8_lossy(&pl3[4..]).to_string();
                                    if let Some(v) = smsg.strip_prefix("v=") {
                                        let server_key = hmac_sha256(&salted, b"Server Key");
                                        let expect = b64e(&hmac_sha256(&server_key, authmsg.as_bytes()));
                                        if v != expect {
                                            return Err("pg: SCRAM server signature mismatch".into());
                                        }
                                    }
                                }
                                0 => return Ok(()),
                                _ => return Err(format!("pg: unexpected SASL stage {}", c3)),
                            }
                        }
                        b'E' => return Err(pg_err(&pl3)),
                        b'S' | b'N' | b'K' => {}
                        _ => return Err("pg: unexpected message during SASL".into()),
                    }
                }
            }
            other => Err(format!("pg: unsupported auth code {} (supported: cleartext, md5, SCRAM-SHA-256)", other)),
        }
    }

    fn query(&mut self, sql: &str) -> R<QRes> {
        let mut p = sql.as_bytes().to_vec();
        p.push(0);
        self.send_msg(b'Q', &p)?;
        let mut cols = Vec::new();
        let mut rows = Vec::new();
        let mut tag = String::new();
        loop {
            let (t, pl) = self.msg()?;
            match t {
                b'T' => {
                    if pl.len() < 2 {
                        continue;
                    }
                    let n = be16(&pl[0..2]) as usize;
                    let mut i = 2usize;
                    for _ in 0..n {
                        if i >= pl.len() {
                            break;
                        }
                        let (name, ni) = read_cstr(&pl, i);
                        // Fixed trailing fields per column (table oid,
                        // attnum, type oid, typlen, typmod, format = 18B).
                        if ni + 18 > pl.len() {
                            break;
                        }
                        i = ni + 4 + 2 + 4 + 2 + 4 + 2;
                        cols.push(name);
                    }
                }
                b'D' => {
                    if pl.len() < 2 {
                        continue;
                    }
                    let n = be16(&pl[0..2]) as usize;
                    let mut i = 2usize;
                    let mut row = Vec::new();
                    for _ in 0..n {
                        if i + 4 > pl.len() {
                            row.push(J::Null);
                            continue;
                        }
                        let l = be32(&pl[i..i + 4]) as i32;
                        i += 4;
                        if l < 0 {
                            row.push(J::Null);
                        } else {
                            let e = (i + l as usize).min(pl.len());
                            let v = String::from_utf8_lossy(&pl[i..e]).to_string();
                            i = e;
                            row.push(text_to_j(&v));
                        }
                    }
                    let _ = n;
                    rows.push(row);
                }
                b'C' => {
                    let (s, _) = read_cstr(&pl, 0);
                    tag = s;
                }
                b'E' => {
                    let e = pg_err(&pl);
                    loop {
                        let (t2, _) = self.msg()?;
                        if t2 == b'Z' {
                            break;
                        }
                    }
                    return Err(e);
                }
                b'Z' => return Ok(QRes { cols, rows, tag }),
                _ => {}
            }
        }
    }
}

// ---------------- MYSQL ----------------

const MY_CL_LONG_PASSWORD: u32 = 1;
const MY_CL_PROTOCOL_41: u32 = 512;
const MY_CL_TRANSACTIONS: u32 = 8192;
const MY_CL_SECURE_CONNECTION: u32 = 32768;
const MY_CL_MULTI_RESULTS: u32 = 1 << 17;
const MY_CL_PLUGIN_AUTH: u32 = 1 << 19;
const MY_CL_CONNECT_WITH_DB: u32 = 1 << 3;

fn native_pw(password: &[u8], nonce: &[u8]) -> Vec<u8> {
    let p1 = sha1(password);
    let p2 = sha1(&p1);
    let mut n = nonce.to_vec();
    n.extend_from_slice(&p2);
    let p3 = sha1(&n);
    xor_vec(&p1, &p3)
}

fn caching_scramble(password: &[u8], nonce: &[u8]) -> Vec<u8> {
    let s1 = sha256(password);
    let s2 = sha256(&s1);
    let mut msg = s2.to_vec();
    msg.extend_from_slice(nonce);
    let s3 = sha256(&msg);
    xor_vec(&s1, &s3)
}

fn my_err(pl: &[u8]) -> String {
    if pl.len() < 3 {
        return "mysql error packet".into();
    }
    let errno = le16(&pl[1..3]);
    let mut i = 3usize;
    if pl.len() > 3 && pl[3] == b'#' {
        i = 9;
    }
    let msg = String::from_utf8_lossy(&pl[i.min(pl.len())..]).to_string();
    format!("mysql error {}: {}", errno, msg)
}

fn lenenc(b: &[u8], i: usize) -> R<(Option<Vec<u8>>, usize)> {
    if i >= b.len() {
        return Err("mysql short packet".into());
    }
    let f = b[i];
    match f {
        0xFB => Ok((None, i + 1)),
        0xFC => {
            if i + 3 > b.len() {
                return Err("mysql short packet".into());
            }
            let l = le16(&b[i + 1..i + 3]) as usize;
            if i + 3 + l > b.len() {
                return Err("mysql short packet".into());
            }
            Ok((Some(b[i + 3..i + 3 + l].to_vec()), i + 3 + l))
        }
        0xFD => {
            if i + 4 > b.len() {
                return Err("mysql short packet".into());
            }
            let l = le24(&b[i + 1..i + 4]);
            if i + 4 + l > b.len() {
                return Err("mysql short packet".into());
            }
            Ok((Some(b[i + 4..i + 4 + l].to_vec()), i + 4 + l))
        }
        0xFE => {
            if i + 9 > b.len() {
                return Err("mysql short packet".into());
            }
            let l = le64(&b[i + 1..i + 9]) as usize;
            if i + 9 + l > b.len() {
                return Err("mysql short packet".into());
            }
            Ok((Some(b[i + 9..i + 9 + l].to_vec()), i + 9 + l))
        }
        _ => {
            let l = f as usize;
            if i + 1 + l > b.len() {
                return Err("mysql short packet".into());
            }
            Ok((Some(b[i + 1..i + 1 + l].to_vec()), i + 1 + l))
        }
    }
}

/// Read a MySQL lenenc *integer* (not a lenenc string): 1-byte value if
/// < 0xFB, 0xFC -> u16 LE, 0xFD -> 24-bit LE, 0xFE -> u64 LE, 0xFB -> NULL
/// (reported as 0).
fn lenenc_int(b: &[u8], i: usize) -> R<(u64, usize)> {
    if i >= b.len() {
        return Err("mysql short packet".into());
    }
    match b[i] {
        0xFB => Ok((0, i + 1)),
        0xFC => {
            if i + 3 > b.len() {
                return Err("mysql short packet".into());
            }
            Ok((le16(&b[i + 1..i + 3]) as u64, i + 3))
        }
        0xFD => {
            if i + 4 > b.len() {
                return Err("mysql short packet".into());
            }
            Ok((le24(&b[i + 1..i + 4]) as u64, i + 4))
        }
        0xFE => {
            if i + 9 > b.len() {
                return Err("mysql short packet".into());
            }
            Ok((le64(&b[i + 1..i + 9]), i + 9))
        }
        v => Ok((v as u64, i + 1)),
    }
}

struct MyConn {
    s: TcpStream,
    seq: u8,
}

impl MyConn {
    fn read_pkt(&mut self) -> R<Vec<u8>> {
        let mut h = [0u8; 4];
        self.s.read_exact(&mut h).map_err(|e| format!("mysql read: {}", e))?;
        self.seq = h[3].wrapping_add(1);
        let len = le24(&h[0..3]);
        let mut pl = vec![0u8; len];
        if len > 0 {
            self.s.read_exact(&mut pl).map_err(|e| format!("mysql read: {}", e))?;
        }
        if len == 0xffffff {
            loop {
                let mut h2 = [0u8; 4];
                self.s.read_exact(&mut h2).map_err(|e| format!("mysql read: {}", e))?;
                self.seq = h2[3].wrapping_add(1);
                let l2 = le24(&h2[0..3]);
                let mut p2 = vec![0u8; l2];
                if l2 > 0 {
                    self.s.read_exact(&mut p2).map_err(|e| format!("mysql read: {}", e))?;
                }
                pl.extend_from_slice(&p2);
                if l2 < 0xffffff {
                    break;
                }
            }
        }
        Ok(pl)
    }

    fn write_pkt(&mut self, pl: &[u8]) -> R<()> {
        let seq = self.seq;
        self.seq = self.seq.wrapping_add(1);
        let mut v = Vec::with_capacity(pl.len() + 4);
        v.extend_from_slice(&(pl.len() as u32).to_le_bytes()[0..3]);
        v.push(seq);
        v.extend_from_slice(pl);
        self.s.write_all(&v).map_err(|e| format!("mysql write: {}", e))
    }

    fn connect(host: &str, port: u16, user: &str, password: &str, dbname: &str) -> R<MyConn> {
        let s = TcpStream::connect((host, port)).map_err(|e| format!("mysql connect {}: {}", host, e))?;
        s.set_read_timeout(Some(Duration::from_secs(60))).ok();
        s.set_write_timeout(Some(Duration::from_secs(60))).ok();
        let mut c = MyConn { s, seq: 0 };
        let p = c.read_pkt()?;
        if p.is_empty() {
            return Err("mysql: empty handshake".into());
        }
        if p[0] == 0xFF {
            return Err(my_err(&p));
        }
        if p[0] != 10 {
            return Err(format!("mysql: unsupported handshake protocol {}", p[0]));
        }
        let (_, mut i) = read_cstr(&p, 1); // server version
        i += 4; // thread id
        if i + 8 > p.len() {
            return Err("mysql: bad handshake".into());
        }
        let auth1 = p[i..i + 8].to_vec();
        i += 8;
        i += 1; // filler
        if i + 2 > p.len() {
            return Err("mysql: bad handshake".into());
        }
        let cap_low = le16(&p[i..i + 2]) as u32;
        i += 2;
        if i + 1 + 2 + 2 + 1 + 10 > p.len() {
            return Err("mysql: bad handshake".into());
        }
        let _charset = p[i];
        i += 1;
        let _status = le16(&p[i..i + 2]);
        i += 2;
        let cap_high = le16(&p[i..i + 2]) as u32;
        i += 2;
        let adl = p[i] as usize;
        let _ = adl;
        i += 1;
        i += 10; // reserved
        let caps_server = cap_low | (cap_high << 16);
        let mut nonce = auth1.clone();
        let mut plugin = "mysql_native_password".to_string();
        if caps_server & MY_CL_PLUGIN_AUTH != 0 {
            let (part2, ni) = read_cstr(&p, i);
            let mut p2 = part2.into_bytes();
            if p2.len() > 13 {
                p2.truncate(13);
            }
            nonce.extend_from_slice(&p2);
            i = ni;
            let (pn, _) = read_cstr(&p, i);
            plugin = pn;
        }
        if nonce.len() > 20 {
            nonce.truncate(20);
        }
        let mut caps = MY_CL_LONG_PASSWORD
            | MY_CL_PROTOCOL_41
            | MY_CL_TRANSACTIONS
            | MY_CL_SECURE_CONNECTION
            | MY_CL_PLUGIN_AUTH
            | MY_CL_MULTI_RESULTS;
        if !dbname.is_empty() {
            caps |= MY_CL_CONNECT_WITH_DB;
        }
        // Passwordless accounts must send a zero-length auth response;
        // computing a scramble over an empty password makes the server
        // reject the login.
        let tok = if password.is_empty() {
            Vec::new()
        } else {
            match plugin.as_str() {
                "mysql_native_password" => native_pw(password.as_bytes(), &nonce),
                "caching_sha2_password" => caching_scramble(password.as_bytes(), &nonce),
                other => return Err(format!("mysql: unsupported auth plugin '{}'", other)),
            }
        };
        let mut r = Vec::new();
        r.extend_from_slice(&caps.to_le_bytes());
        r.extend_from_slice(&0x01000000u32.to_le_bytes());
        r.push(45); // utf8mb4_general_ci
        r.extend_from_slice(&[0u8; 23]);
        r.extend_from_slice(user.as_bytes());
        r.push(0);
        r.push(tok.len() as u8);
        r.extend_from_slice(&tok);
        if !dbname.is_empty() {
            r.extend_from_slice(dbname.as_bytes());
            r.push(0);
        }
        r.extend_from_slice(plugin.as_bytes());
        r.push(0);
        c.write_pkt(&r)?;
        loop {
            let p = c.read_pkt()?;
            if p.is_empty() {
                continue;
            }
            match p[0] {
                0x00 => break,
                0xFF => return Err(my_err(&p)),
                0xFE => {
                    // auth switch
                    let (plugin2, ni) = read_cstr(&p, 1);
                    let nonce2 = if ni < p.len() { &p[ni..] } else { &[][..] };
                    let mut n2 = nonce2.to_vec();
                    while n2.last() == Some(&0) {
                        n2.pop();
                    }
                    if n2.len() > 20 {
                        n2.truncate(20);
                    }
                    let tok2 = if password.is_empty() {
                        Vec::new()
                    } else {
                        match plugin2.as_str() {
                            "mysql_native_password" => native_pw(password.as_bytes(), &n2),
                            "caching_sha2_password" => caching_scramble(password.as_bytes(), &n2),
                            other => {
                                return Err(format!(
                                    "mysql: unsupported auth switch to '{}'",
                                    other
                                ))
                            }
                        }
                    };
                    c.write_pkt(&tok2)?;
                }
                0x01 => {
                    if p.len() > 1 {
                        match p[1] {
                            3 => {} // fast auth success; OK follows
                            4 => {
                                return Err(
                                    "mysql: server demands full caching_sha2_password (RSA/TLS) — create the user with mysql_native_password, e.g. CREATE USER ... IDENTIFIED WITH mysql_native_password BY '...'"
                                        .into(),
                                )
                            }
                            _ => {}
                        }
                    }
                }
                _ => {}
            }
        }
        Ok(c)
    }

    fn ok_affected(pl: &[u8]) -> u64 {
        if pl.is_empty() {
            return 0;
        }
        // OK packet: [0x00][affected_rows lenenc][last_insert_id lenenc]...
        match lenenc_int(pl, 1) {
            Ok((n, _)) => n,
            Err(_) => 0,
        }
    }

    fn query(&mut self, sql: &str) -> R<QRes> {
        self.seq = 0;
        let mut p = vec![0x03u8];
        p.extend_from_slice(sql.as_bytes());
        self.write_pkt(&p)?;
        let first = self.read_pkt()?;
        if first.is_empty() {
            return Err("mysql: empty response".into());
        }
        if first[0] == 0xFF {
            return Err(my_err(&first));
        }
        if first[0] == 0x00 {
            return Ok(QRes {
                cols: vec![],
                rows: vec![],
                tag: format!("affected={}", MyConn::ok_affected(&first)),
            });
        }
        let cc = match lenenc(&first, 0)? {
            (Some(v), _) => v.first().copied().unwrap_or(0) as usize,
            _ => 0,
        };
        let mut cols = Vec::new();
        for _ in 0..cc {
            let cp = self.read_pkt()?;
            // catalog, schema, table, org_table, name, org_name
            let mut i = 0usize;
            let mut name = String::new();
            for nth in 0..6 {
                let (v, ni) = lenenc(&cp, i)?;
                i = ni;
                if nth == 4 {
                    if let Some(vb) = v {
                        name = String::from_utf8_lossy(&vb).to_string();
                    }
                }
            }
            cols.push(name);
        }
        let eof = self.read_pkt()?; // EOF before rows
        if eof.first() == Some(&0xFF) {
            return Err(my_err(&eof));
        }
        let mut rows = Vec::new();
        loop {
            let rp = self.read_pkt()?;
            if rp.is_empty() {
                break;
            }
            if rp[0] == 0xFE && rp.len() < 9 {
                break; // EOF
            }
            if rp[0] == 0xFF {
                return Err(my_err(&rp));
            }
            if rp[0] == 0x00 {
                break; // OK (deprecated EOF)
            }
            let mut i = 0usize;
            let mut row = Vec::new();
            for _ in 0..cc {
                let (v, ni) = lenenc(&rp, i)?;
                i = ni;
                row.push(match v {
                    None => J::Null,
                    Some(b) => text_to_j(&String::from_utf8_lossy(&b)),
                });
            }
            rows.push(row);
        }
        let tag = format!("{} rows", rows.len());
        Ok(QRes { cols, rows, tag })
    }
}

// ---------------- HTTP ----------------

fn parse_url(u: &str) -> R<(String, u16, String)> {
    let rest = u
        .strip_prefix("http://")
        .ok_or("http connector supports http:// URLs only (for https use a local proxy or the camoufox browser)")?;
    let (hostport, path) = match rest.find('/') {
        Some(ix) => (&rest[..ix], &rest[ix..]),
        None => (rest, "/"),
    };
    let (host, port) = match hostport.rfind(':') {
        Some(ix) => {
            let h = &hostport[..ix];
            let p = hostport[ix + 1..].parse::<u16>().map_err(|_| "bad port")?;
            (h.to_string(), p)
        }
        None => (hostport.to_string(), 80),
    };
    Ok((host, port, path.to_string()))
}

fn http_request(method: &str, url: &str, headers: &[(String, String)], body: Option<&str>) -> R<(u16, String)> {
    let (host, port, path) = parse_url(url)?;
    let mut s = TcpStream::connect((host.as_str(), port)).map_err(|e| format!("http connect: {}", e))?;
    s.set_read_timeout(Some(Duration::from_secs(60))).ok();
    s.set_write_timeout(Some(Duration::from_secs(60))).ok();
    let mut req = format!(
        "{} {} HTTP/1.0\r\nHost: {}\r\nUser-Agent: dbengine/1.0\r\nAccept: */*\r\nConnection: close\r\n",
        method, path, host
    );
    for (k, v) in headers {
        req.push_str(&format!("{}: {}\r\n", k, v));
    }
    if let Some(b) = body {
        req.push_str(&format!("Content-Length: {}\r\n", b.len()));
    }
    req.push_str("\r\n");
    if let Some(b) = body {
        req.push_str(b);
    }
    s.write_all(req.as_bytes()).map_err(|e| format!("http write: {}", e))?;
    let mut resp = Vec::new();
    s.read_to_end(&mut resp).map_err(|e| format!("http read: {}", e))?;
    let text = String::from_utf8_lossy(&resp).to_string();
    let split = text.find("\r\n\r\n").unwrap_or(text.len());
    let head = &text[..split];
    let body = if split < text.len() { text[split + 4..].to_string() } else { String::new() };
    let status_line = head.lines().next().unwrap_or("");
    let code: u16 = status_line
        .split_whitespace()
        .nth(1)
        .and_then(|x| x.parse().ok())
        .unwrap_or(0);
    Ok((code, body))
}

// ---------------- connection profiles ----------------

fn connections_path(e: &Engine) -> PathBuf {
    e.home.join("connections.json")
}

fn load_connections(e: &Engine) -> BTreeMap<String, J> {
    let mut out = BTreeMap::new();
    if let Ok(data) = fs::read_to_string(connections_path(e)) {
        if let Ok(J::Obj(m)) = jparse(&data) {
            for (k, v) in m {
                out.insert(k, v);
            }
        }
    }
    out
}

fn save_connections(e: &Engine, conns: &BTreeMap<String, J>) -> R<()> {
    let j = J::Obj(conns.iter().map(|(k, v)| (k.clone(), v.clone())).collect());
    atomic_write(&connections_path(e), &jpretty(&j))
}

fn resolve_conn(e: &Engine, a: &J) -> R<J> {
    if let Some(name) = arg_opt_s(a, "connection") {
        let conns = load_connections(e);
        return conns
            .get(&name)
            .cloned()
            .ok_or_else(|| format!("connection '{}' not found (use db_connect first)", name));
    }
    if let Some(c) = a.get("conn") {
        if matches!(c, J::Obj(_)) {
            return Ok(c.clone());
        }
    }
    Err("remote tool needs either a saved 'connection' name or an inline 'conn' object".into())
}

// ---------------- remote ops (enum dispatch) ----------------

enum RC {
    Pg(PgConn),
    My(MyConn),
    Http { url: String, method: String, headers: Vec<(String, String)>, body: Option<String> },
}

fn rc_connect(p: &J) -> R<RC> {
    let kind = arg_s(p, "kind")?;
    match kind.as_str() {
        "postgres" | "postgresql" | "pg" => {
            let host = arg_opt_s(p, "host").unwrap_or_else(|| "127.0.0.1".into());
            let port = p.get("port").and_then(|x| x.as_i64()).unwrap_or(5432) as u16;
            let user = arg_opt_s(p, "user").unwrap_or_else(|| "postgres".into());
            let pass = arg_opt_s(p, "password").unwrap_or_default();
            let dbn = arg_opt_s(p, "database").unwrap_or_else(|| user.clone());
            Ok(RC::Pg(PgConn::connect(&host, port, &user, &pass, &dbn)?))
        }
        "mysql" => {
            let host = arg_opt_s(p, "host").unwrap_or_else(|| "127.0.0.1".into());
            let port = p.get("port").and_then(|x| x.as_i64()).unwrap_or(3306) as u16;
            let user = arg_opt_s(p, "user").unwrap_or_else(|| "root".into());
            let pass = arg_opt_s(p, "password").unwrap_or_default();
            let dbn = arg_opt_s(p, "database").unwrap_or_default();
            Ok(RC::My(MyConn::connect(&host, port, &user, &pass, &dbn)?))
        }
        "http" => {
            let url = arg_s(p, "url")?;
            let method = arg_opt_s(p, "method").unwrap_or_else(|| "GET".into()).to_uppercase();
            let mut headers = Vec::new();
            if let Some(h) = p.get("headers").and_then(|x| x.as_obj()) {
                for (k, v) in h {
                    headers.push((k.clone(), jstr(v).trim_matches('"').to_string()));
                }
            }
            let body = arg_opt_s(p, "body");
            Ok(RC::Http { url, method, headers, body })
        }
        other => Err(format!("unknown connection kind '{}' (postgres | mysql | http)", other)),
    }
}

fn rc_query(c: &mut RC, sql: &str) -> R<QRes> {
    match c {
        RC::Pg(p) => p.query(sql),
        RC::My(m) => m.query(sql),
        RC::Http { .. } => Err("http connections do not accept SQL — they return JSON payloads".into()),
    }
}

fn rc_http_fetch(c: &mut RC) -> R<(u16, J)> {
    match c {
        RC::Http { url, method, headers, body } => {
            let (code, text) = http_request(method, url, headers, body.as_deref())?;
            if text.trim().is_empty() {
                return Ok((code, J::Null));
            }
            let j = jparse(&text).map_err(|e| format!("http body is not valid json: {}", e))?;
            Ok((code, j))
        }
        _ => Err("not an http connection".into()),
    }
}

fn qres_to_objects(res: &QRes) -> Vec<J> {
    let mut out = Vec::new();
    for row in &res.rows {
        let mut m = Vec::new();
        for (i, c) in res.cols.iter().enumerate() {
            let v = row.get(i).cloned().unwrap_or(J::Null);
            m.push((c.clone(), v));
        }
        out.push(J::Obj(m));
    }
    out
}

fn qi(name: &str, kind: &str) -> String {
    match kind {
        "pg" => format!("\"{}\"", name.replace('"', "\"\"")),
        _ => format!("`{}`", name.replace('`', "``")),
    }
}

fn sql_lit(v: &J) -> String {
    match v {
        J::Null => "NULL".into(),
        J::Bool(b) => if *b { "TRUE".into() } else { "FALSE".into() },
        J::Int(i) => i.to_string(),
        J::Num(f) => format!("{}", f),
        J::Str(s) => format!("'{}'", s.replace('\'', "''")),
        other => format!("'{}'", jstr(other).replace('\'', "''")),
    }
}

fn pg_dtype(t: &str) -> (&'static str, bool) {
    match t {
        "smallint" | "integer" | "bigint" => ("integer", false),
        "real" | "double precision" | "numeric" => ("real", false),
        "boolean" => ("boolean", false),
        "json" | "jsonb" => ("json", false),
        _ => ("text", false),
    }
}

fn my_dtype(t: &str) -> &'static str {
    match t {
        "int" | "tinyint" | "smallint" | "mediumint" | "bigint" | "year" => "integer",
        "float" | "double" | "decimal" | "numeric" => "real",
        "json" => "json",
        _ => "text",
    }
}

fn parse_pg_cols(list: &str) -> Vec<String> {
    list.trim()
        .trim_start_matches('(')
        .trim_end_matches(')')
        .split(',')
        .map(|x| x.trim().trim_matches('"').to_string())
        .filter(|x| !x.is_empty())
        .collect()
}

fn pg_introspect(c: &mut PgConn, schema: &str) -> R<Vec<Table>> {
    let cols_q = format!(
        "SELECT table_name, column_name, data_type, is_nullable, column_default FROM information_schema.columns WHERE table_schema='{}' ORDER BY table_name, ordinal_position",
        schema.replace('\'', "''")
    );
    let cons_q = format!(
        "SELECT conrelid::regclass::text AS tbl, contype, pg_get_constraintdef(oid) AS def FROM pg_constraint WHERE connamespace='{}'::regnamespace AND contype IN ('p','f','u') ORDER BY tbl, conname",
        schema.replace('\'', "''")
    );
    let idx_q = format!(
        "SELECT tablename, indexname, indexdef FROM pg_indexes WHERE schemaname='{}'",
        schema.replace('\'', "''")
    );
    let cres = c.query(&cols_q)?;
    let nres = c.query(&cons_q)?;
    let ires = c.query(&idx_q)?;
    let mut tables: BTreeMap<String, Table> = BTreeMap::new();
    for row in qres_to_objects(&cres) {
        let tn = row.get("table_name").and_then(|x| x.as_str()).unwrap_or("").to_string();
        let cn = row.get("column_name").and_then(|x| x.as_str()).unwrap_or("").to_string();
        let dt = row.get("data_type").and_then(|x| x.as_str()).unwrap_or("text").to_string();
        let nul = row.get("is_nullable").and_then(|x| x.as_str()).unwrap_or("YES") != "NO";
        let dflt = row.get("column_default").and_then(|x| x.as_str()).unwrap_or("").to_string();
        let (dtype, _) = pg_dtype(&dt);
        let t = tables.entry(tn.clone()).or_insert_with(|| Table {
            name: tn.clone(),
            ..Default::default()
        });
        t.cols.push(Col {
            name: sanitize_col(&cn)?,
            dtype: dtype.to_string(),
            nullable: nul,
            auto: dflt.contains("nextval"),
        });
    }
    for row in qres_to_objects(&nres) {
        let tn = row.get("tbl").and_then(|x| x.as_str()).unwrap_or("").to_string();
        let tn = tn.trim_matches('"').to_string();
        let ct = row.get("contype").and_then(|x| x.as_str()).unwrap_or("").to_string();
        let def = row.get("def").and_then(|x| x.as_str()).unwrap_or("").to_string();
        let t = match tables.get_mut(&tn) {
            Some(t) => t,
            None => continue,
        };
        match ct.as_str() {
            "p" => {
                if let Some(ix) = def.find('(') {
                    let cols = parse_pg_cols(&def[ix..]);
                    t.pk = cols;
                }
            }
            "u" => {
                if let Some(ix) = def.find('(') {
                    let cols = parse_pg_cols(&def[ix..]);
                    if !cols.is_empty() {
                        t.uniques.push(cols);
                    }
                }
            }
            "f" => {
                // FOREIGN KEY (a, b) REFERENCES reftab (c, d) [ON DELETE ...]
                if let Some(fx) = def.find("FOREIGN KEY") {
                    let rest = &def[fx..];
                    if let Some(p1) = rest.find('(') {
                        if let Some(rx) = rest.find("REFERENCES") {
                            let lcols = parse_pg_cols(&rest[p1..rx]);
                            let rrest = &rest[rx + "REFERENCES".len()..];
                            if let Some(p2) = rrest.find('(') {
                                let (rtab, rcols) = {
                                    let rt = rrest[..p2].trim().trim_matches('"').to_string();
                                    (rt, parse_pg_cols(&rrest[p2..]))
                                };
                                let casc = rest.contains("ON DELETE CASCADE");
                                if !lcols.is_empty() && !rcols.is_empty() {
                                    t.fks.push(Fk {
                                        cols: lcols,
                                        ref_table: rtab,
                                        ref_cols: rcols,
                                        on_delete: if casc { "cascade".into() } else { "restrict".into() },
                                    });
                                }
                            }
                        }
                    }
                }
            }
            _ => {}
        }
    }
    for row in qres_to_objects(&ires) {
        let tn = row.get("tablename").and_then(|x| x.as_str()).unwrap_or("").to_string();
        let iname = row.get("indexname").and_then(|x| x.as_str()).unwrap_or("").to_string();
        let idef = row.get("indexdef").and_then(|x| x.as_str()).unwrap_or("").to_string();
        let t = match tables.get_mut(&tn) {
            Some(t) => t,
            None => continue,
        };
        if t.pk.is_empty() && iname.ends_with("_pkey") {
            continue;
        }
        if let Some(ix) = idef.rfind('(') {
            let cols = parse_pg_cols(&idef[ix..]);
            if !cols.is_empty() {
                t.indexes.push(IndexDef {
                    name: iname,
                    cols,
                    unique: idef.contains("CREATE UNIQUE"),
                });
            }
        }
    }
    Ok(tables.into_values().collect())
}

fn my_introspect(c: &mut MyConn, database: &str) -> R<Vec<Table>> {
    let dbq = database.replace('\'', "''");
    let cols_q = format!(
        "SELECT TABLE_NAME, COLUMN_NAME, DATA_TYPE, IS_NULLABLE, COLUMN_KEY, EXTRA FROM INFORMATION_SCHEMA.COLUMNS WHERE TABLE_SCHEMA='{}' ORDER BY TABLE_NAME, ORDINAL_POSITION",
        dbq
    );
    let fk_q = format!(
        "SELECT TABLE_NAME, CONSTRAINT_NAME, COLUMN_NAME, ORDINAL_POSITION, REFERENCED_TABLE_NAME, REFERENCED_COLUMN_NAME FROM INFORMATION_SCHEMA.KEY_COLUMN_USAGE WHERE TABLE_SCHEMA='{}' AND REFERENCED_TABLE_NAME IS NOT NULL ORDER BY TABLE_NAME, CONSTRAINT_NAME, ORDINAL_POSITION",
        dbq
    );
    let stat_q = format!(
        "SELECT TABLE_NAME, INDEX_NAME, COLUMN_NAME, NON_UNIQUE, SEQ_IN_INDEX FROM INFORMATION_SCHEMA.STATISTICS WHERE TABLE_SCHEMA='{}' ORDER BY TABLE_NAME, INDEX_NAME, SEQ_IN_INDEX",
        dbq
    );
    let cres = c.query(&cols_q)?;
    let fres = c.query(&fk_q)?;
    let sres = c.query(&stat_q)?;
    let mut tables: BTreeMap<String, Table> = BTreeMap::new();
    for row in qres_to_objects(&cres) {
        let tn = row.get("TABLE_NAME").and_then(|x| x.as_str()).unwrap_or("").to_string();
        let cn = row.get("COLUMN_NAME").and_then(|x| x.as_str()).unwrap_or("").to_string();
        let dt = row.get("DATA_TYPE").and_then(|x| x.as_str()).unwrap_or("text").to_string();
        let nul = row.get("IS_NULLABLE").and_then(|x| x.as_str()).unwrap_or("YES") == "YES";
        let key = row.get("COLUMN_KEY").and_then(|x| x.as_str()).unwrap_or("").to_string();
        let extra = row.get("EXTRA").and_then(|x| x.as_str()).unwrap_or("").to_string();
        let t = tables.entry(tn.clone()).or_insert_with(|| Table {
            name: tn.clone(),
            ..Default::default()
        });
        t.cols.push(Col {
            name: sanitize_col(&cn)?,
            dtype: my_dtype(&dt).to_string(),
            nullable: nul,
            auto: extra.contains("auto_increment"),
        });
        if key == "PRI" {
            t.pk.push(sanitize_col(&cn)?);
        }
        if key == "UNI" {
            t.uniques.push(vec![cn.clone()]);
        }
    }
    let mut fk_map: BTreeMap<(String, String), Fk> = BTreeMap::new();
    for row in qres_to_objects(&fres) {
        let tn = row.get("TABLE_NAME").and_then(|x| x.as_str()).unwrap_or("").to_string();
        let cnm = row.get("CONSTRAINT_NAME").and_then(|x| x.as_str()).unwrap_or("").to_string();
        let col = row.get("COLUMN_NAME").and_then(|x| x.as_str()).unwrap_or("").to_string();
        let rt = row.get("REFERENCED_TABLE_NAME").and_then(|x| x.as_str()).unwrap_or("").to_string();
        let rc = row.get("REFERENCED_COLUMN_NAME").and_then(|x| x.as_str()).unwrap_or("").to_string();
        let e = fk_map.entry((tn, cnm)).or_insert_with(|| Fk {
            cols: Vec::new(),
            ref_table: rt.clone(),
            ref_cols: Vec::new(),
            on_delete: "restrict".into(),
        });
        e.cols.push(col);
        e.ref_cols.push(rc);
    }
    for ((tn, _), fk) in fk_map {
        if let Some(t) = tables.get_mut(&tn) {
            t.fks.push(fk);
        }
    }
    let mut seen_uni: HashSet<String> = HashSet::new();
    let mut idx_map: BTreeMap<(String, String), (bool, Vec<String>)> = BTreeMap::new();
    for row in qres_to_objects(&sres) {
        let tn = row.get("TABLE_NAME").and_then(|x| x.as_str()).unwrap_or("").to_string();
        let iname = row.get("INDEX_NAME").and_then(|x| x.as_str()).unwrap_or("").to_string();
        let col = row.get("COLUMN_NAME").and_then(|x| x.as_str()).unwrap_or("").to_string();
        let nu = row.get("NON_UNIQUE").and_then(|x| x.as_i64()).unwrap_or(1) == 1;
        if iname == "PRIMARY" {
            continue;
        }
        let e = idx_map.entry((tn, iname.clone())).or_insert((!nu, Vec::new()));
        e.1.push(col);
    }
    for ((tn, iname), (unique, cols)) in idx_map {
        if let Some(t) = tables.get_mut(&tn) {
            if unique {
                let dup = t.uniques.iter().any(|u| *u == cols);
                if !dup {
                    t.uniques.push(cols);
                    seen_uni.insert(format!("{}.{}", tn, iname));
                }
            } else {
                t.indexes.push(IndexDef { name: iname, cols, unique: false });
            }
        }
    }
    Ok(tables.into_values().collect())
}

fn remote_schema(p: &J) -> R<(Vec<Table>, Vec<String>, String)> {
    let kind = arg_s(p, "kind")?;
    let _ = &kind;
    let mut notes = Vec::new();
    match kind.as_str() {
        "postgres" | "postgresql" | "pg" => {
            let host = arg_opt_s(p, "host").unwrap_or_else(|| "127.0.0.1".into());
            let port = p.get("port").and_then(|x| x.as_i64()).unwrap_or(5432) as u16;
            let user = arg_opt_s(p, "user").unwrap_or_else(|| "postgres".into());
            let pass = arg_opt_s(p, "password").unwrap_or_default();
            let dbn = arg_opt_s(p, "database").unwrap_or_else(|| user.clone());
            let schema = arg_opt_s(p, "schema").unwrap_or_else(|| "public".into());
            let mut c = PgConn::connect(&host, port, &user, &pass, &dbn)?;
            let version = c
                .query("SELECT version()")
                .ok()
                .and_then(|r| r.rows.first().and_then(|x| x.first().cloned()))
                .map(|v| jstr(&v).trim_matches('"').to_string())
                .unwrap_or_default();
            notes.push(format!("server: {}", version));
            notes.push(
                "key storage: pg_catalog — pg_constraint (PRIMARY/FOREIGN/UNIQUE), pg_index, pg_class; exposed via information_schema.table_constraints / key_column_usage / pg_indexes".into(),
            );
            let ts = pg_introspect(&mut c, &schema)?;
            Ok((ts, notes, "postgres".into()))
        }
        "mysql" => {
            let host = arg_opt_s(p, "host").unwrap_or_else(|| "127.0.0.1".into());
            let port = p.get("port").and_then(|x| x.as_i64()).unwrap_or(3306) as u16;
            let user = arg_opt_s(p, "user").unwrap_or_else(|| "root".into());
            let pass = arg_opt_s(p, "password").unwrap_or_default();
            let dbn = arg_opt_s(p, "database").unwrap_or_default();
            let mut c = MyConn::connect(&host, port, &user, &pass, &dbn)?;
            let version = c
                .query("SELECT VERSION()")
                .ok()
                .and_then(|r| r.rows.first().and_then(|x| x.first().cloned()))
                .map(|v| jstr(&v).trim_matches('"').to_string())
                .unwrap_or_default();
            notes.push(format!("server: {}", version));
            notes.push(
                "key storage: INFORMATION_SCHEMA — TABLE_CONSTRAINTS, KEY_COLUMN_USAGE, COLUMNS.COLUMN_KEY, STATISTICS".into(),
            );
            let ts = my_introspect(&mut c, &dbn)?;
            Ok((ts, notes, "mysql".into()))
        }
        "http" => Err("http connections have no catalog — use db_fingerprint to infer a schema from the JSON payload".into()),
        other => Err(format!("unknown connection kind '{}'", other)),
    }
}

fn remote_key_map(e: &Engine, a: &J) -> R<J> {
    let p = resolve_conn(e, a)?;
    let kind = arg_s(&p, "kind")?;
    let (tables, notes, k) = remote_schema(&p)?;
    let mut tables_j = Vec::new();
    let mut edges = Vec::new();
    for t in &tables {
        let mut index_list = Vec::new();
        for x in &t.indexes {
            index_list.push(J::obj(vec![
                ("name", J::s(&x.name)),
                ("columns", J::Arr(x.cols.iter().map(|c| J::s(c)).collect())),
                ("unique", J::Bool(x.unique)),
            ]));
        }
        tables_j.push(J::obj(vec![
            ("name", J::s(&t.name)),
            ("columns", J::Int(t.cols.len() as i64)),
            ("primary_key", J::Arr(t.pk.iter().map(|x| J::s(x)).collect())),
            (
                "foreign_keys",
                J::Arr(
                    t.fks
                        .iter()
                        .map(|f| {
                            J::obj(vec![
                                ("columns", J::Arr(f.cols.iter().map(|x| J::s(x)).collect())),
                                ("references", J::s(&format!("{}.({})", f.ref_table, f.ref_cols.join(",")))),
                                ("on_delete", J::s(&f.on_delete)),
                            ])
                        })
                        .collect(),
                ),
            ),
            ("unique_keys", J::Arr(t.uniques.iter().map(|u| J::Arr(u.iter().map(|x| J::s(x)).collect())).collect())),
            ("indexes", J::Arr(index_list)),
        ]));
        for f in &t.fks {
            edges.push(J::obj(vec![
                (
                    "from",
                    J::obj(vec![
                        ("table", J::s(&t.name)),
                        ("columns", J::Arr(f.cols.iter().map(|x| J::s(x)).collect())),
                    ]),
                ),
                (
                    "to",
                    J::obj(vec![
                        ("table", J::s(&f.ref_table)),
                        ("columns", J::Arr(f.ref_cols.iter().map(|x| J::s(x)).collect())),
                    ]),
                ),
            ]));
        }
    }
    Ok(J::obj(vec![
        ("kind", J::s(&k)),
        ("where_keys_are_stored", J::Arr(notes.iter().map(|x| J::s(x)).collect())),
        ("tables", J::Arr(tables_j)),
        ("edges", J::Arr(edges)),
    ]))
}

// ---------------- EXTRACTOR ----------------

fn extract_remote(e: &mut Engine, a: &J) -> R<J> {
    let p = resolve_conn(e, a)?;
    let kind = arg_s(&p, "kind")?;
    let target = arg_s(a, "target_db")?;
    let tname = sanitize_name(&target)?;
    let page: usize = arg_u64_d(a, "page_size", 500) as usize;
    let save_bundle = arg_bool_d(a, "save_bundle", true);
    let only_tables: Option<Vec<String>> = a
        .get("tables")
        .and_then(|x| x.as_arr())
        .map(|arr| arr.iter().filter_map(|x| x.as_str().map(|s| s.to_string())).collect());

    let mut per_table = Vec::new();
    let mut bundle_rows: BTreeMap<String, J> = BTreeMap::new();

    if kind == "http" {
        // API -> inferred schema -> local database (the fingerprint pipeline)
        let (code, payload) = {
            let mut c = rc_connect(&p)?;
            rc_http_fetch(&mut c)?
        };
        if code >= 400 {
            return Err(format!("http endpoint returned status {}", code));
        }
        let proposals = fp_analyze_payload(&payload)?;
        if proposals.is_empty() {
            return Err("no arrays of objects found in the http payload — nothing to extract".into());
        }
        e.db(&tname)?;
        for pr in &proposals {
            if let Some(want) = &only_tables {
                if !want.contains(&pr.name) {
                    continue;
                }
            }
            ensure_table(e, &tname, &pr)?;
            let mut n = 0i64;
            for row in &pr.rows {
                e.put_row(&tname, &pr.name, row, true)?;
                n += 1;
            }
            bundle_rows.insert(pr.name.clone(), J::Arr(pr.rows.clone()));
            per_table.push(J::obj(vec![
                ("table", J::s(&pr.name)),
                ("rows", J::Int(n)),
                ("primary_key", J::Arr(pr.pk.iter().map(|x| J::s(x)).collect())),
                ("source", J::s("inferred-from-api")),
            ]));
        }
        for pr in &proposals {
            for fk in &pr.fk_proposals {
                if let Some(want) = &only_tables {
                    if !want.contains(&pr.name) {
                        continue;
                    }
                }
                let mut add_fk = false;
                {
                    let db = e.db(&tname)?;
                    if let Some(t) = db.tables.get_mut(&pr.name) {
                        if !t.fks.iter().any(|f| f.cols == fk.cols) {
                            add_fk = true;
                        }
                    }
                }
                if add_fk {
                    let has_ref = {
                        let db = e.db(&tname)?;
                        db.tables.contains_key(&fk.ref_table)
                    };
                    if has_ref {
                        let db = e.db(&tname)?;
                        if let Some(t) = db.tables.get_mut(&pr.name) {
                            t.fks.push(fk.clone());
                        }
                    }
                }
            }
        }
        e.save_catalog(&tname)?;
    } else {
        let (tables, notes, rkind) = remote_schema(&p)?;
        e.db(&tname)?;
        // topological order; cyclic tables get FK checks disabled
        let mut tmp = Db { name: tname.clone(), tables: BTreeMap::new() };
        for t in &tables {
            tmp.tables.insert(t.name.clone(), t.clone());
        }
        let (order, cyclic) = match topo_order(&tmp.tables) {
            Ok(o) => (o, Vec::new()),
            Err(_) => {
                // break cycles: drop fk edges until acyclic
                loop {
                    let mut broke = false;
                    'outer: for tn in tmp.tables.keys().cloned().collect::<Vec<String>>() {
                        if tmp.tables[&tn].fks.is_empty() {
                            continue;
                        }
                        tmp.tables.get_mut(&tn).unwrap().fks.remove(0);
                        broke = true;
                        break 'outer;
                    }
                    if !broke {
                        break;
                    }
                    if let Ok(o) = topo_order(&tmp.tables) {
                        let mut cyc: Vec<String> = Vec::new();
                        for t in &tables {
                            if !o.contains(&t.name) && !cyc.contains(&t.name) {
                                cyc.push(t.name.clone());
                            }
                        }
                        for cn in &cyc {
                            if let Some(t) = tmp.tables.get_mut(cn) {
                                t.nocheck = true;
                            }
                        }
                        let mut full = o;
                        for cn in &cyc {
                            if !full.contains(cn) {
                                full.push(cn.clone());
                            }
                        }
                        break;
                    }
                }
                let o = topo_order(&tmp.tables).unwrap_or_default();
                let mut cyc: Vec<String> = Vec::new();
                for t in &tables {
                    if tmp.tables.get(&t.name).map(|x| x.nocheck).unwrap_or(false) && !cyc.contains(&t.name) {
                        cyc.push(t.name.clone());
                    }
                }
                (o, cyc)
            }
        };
        for note in &notes {
            per_table.push(J::obj(vec![("note", J::s(note))]));
        }
        if !cyclic.is_empty() {
            per_table.push(J::obj(vec![
                ("note", J::s(&format!(
                    "circular foreign keys detected — FK enforcement skipped for: {}",
                    cyclic.join(", ")
                ))),
            ]));
        }
        let mut c = rc_connect(&p)?;
        for tn in &order {
            if let Some(want) = &only_tables {
                if !want.contains(tn) {
                    continue;
                }
            }
            let tdef = tmp.tables.get(tn).cloned().unwrap();
            ensure_table_from(e, &tname, &tdef)?;
            let ident = qi(tn, if rkind == "postgres" { "pg" } else { "my" });
            let total = match rc_query(&mut c, &format!("SELECT COUNT(*) FROM {}", ident)) {
                Ok(r) => r.rows.first().and_then(|x| x.first()).and_then(|x| x.as_i64()).unwrap_or(-1),
                Err(_) => -1,
            };
            let mut n = 0i64;
            let mut off = 0usize;
            loop {
                let sql = if rkind == "postgres" {
                    format!("SELECT * FROM {} LIMIT {} OFFSET {}", ident, page, off)
                } else {
                    format!("SELECT * FROM {} LIMIT {} OFFSET {}", ident, page, off)
                };
                let res = rc_query(&mut c, &sql)?;
                let got = res.rows.len();
                if got == 0 {
                    break;
                }
                let objs = qres_to_objects(&res);
                let mut bcols: BTreeMap<String, J> = BTreeMap::new();
                for row in &objs {
                    e.put_row(&tname, tn, row, true)?;
                    n += 1;
                    bcols.insert(
                        format!("{}", n),
                        J::Obj(colvals_map(row)),
                    );
                }
                if !save_bundle {
                    // still collect bundle rows lazily only when needed
                } else {
                    let entry = bundle_rows.entry(tn.clone()).or_insert(J::Arr(vec![]));
                    if let J::Arr(a) = entry {
                        for row in &objs {
                            a.push(J::Obj(colvals_map(row)));
                        }
                    }
                    let _ = &bcols;
                }
                off += got;
                if got < page {
                    break;
                }
                if n > 5_000_000 {
                    return Err("row cap exceeded (5M) — use 'tables' filter to extract in batches".into());
                }
            }
            per_table.push(J::obj(vec![
                ("table", J::s(tn)),
                ("rows", J::Int(n)),
                ("total_remote", J::Int(total)),
                ("primary_key", J::Arr(tdef.pk.iter().map(|x| J::s(x)).collect())),
                ("source", J::s(&rkind)),
            ]));
        }
    }

    // save portable bundle
    let mut bundle_path = String::new();
    if save_bundle {
        let bn = arg_opt_s(a, "bundle_name").unwrap_or_else(|| tname.clone());
        let bname = sanitize_name(&bn)?;
        let (catalog, _) = {
            let db = e.db(&tname)?;
            (db_to_json(db), 0)
        };
        let bundle = J::obj(vec![
            ("kind", J::s("dbengine-extract")),
            ("source_kind", J::s(&kind)),
            ("db", J::s(&tname)),
            ("created", J::Int(now_ms() as i64)),
            ("catalog", catalog),
            ("rows", J::Obj(bundle_rows.into_iter().collect())),
        ]);
        let path = e.home.join("extracts").join(format!("{}.json", bname));
        atomic_write(&path, &jpretty(&bundle))?;
        bundle_path = path.display().to_string();
    }
    Ok(J::obj(vec![
        ("target_db", J::s(&tname)),
        ("extracted", J::Arr(per_table)),
        ("bundle_path", J::s(&bundle_path)),
        ("resumable", J::Bool(true)),
    ]))
}

fn colvals_map(row: &J) -> Vec<(String, J)> {
    match row {
        J::Obj(m) => m.clone(),
        _ => Vec::new(),
    }
}

fn ensure_table(e: &mut Engine, dbname: &str, pr: &FpTable) -> R<()> {
    ensure_table_from(e, dbname, &pr.to_table())
}

fn ensure_table_from(e: &mut Engine, dbname: &str, tdef: &Table) -> R<()> {
    let db = e.db(dbname)?;
    if db.tables.contains_key(&tdef.name) {
        return Ok(());
    }
    let mut t = tdef.clone();
    t.wlines = 0;
    db.tables.insert(t.name.clone(), t);
    e.save_catalog(dbname)
}

// ---------------- SYNC (key-based incremental) ----------------

fn sync_run(e: &mut Engine, a: &J) -> R<J> {
    let p = resolve_conn(e, a)?;
    let kind = arg_s(&p, "kind")?;
    if kind == "http" {
        return Err("sync requires a database connection (postgres | mysql), not http".into());
    }
    let direction = arg_opt_s(a, "direction").unwrap_or_else(|| "plan".into());
    let local = arg_s(a, "local_db")?;
    let lname = sanitize_name(&local)?;
    let limit = arg_u64_d(a, "limit", 2000) as usize;
    let rkind = if kind == "postgres" { "pg" } else { "my" };
    let (tables, _notes, _k) = remote_schema(&p)?;
    let mut c = rc_connect(&p)?;
    let want: Option<Vec<String>> = a
        .get("tables")
        .and_then(|x| x.as_arr())
        .map(|arr| arr.iter().filter_map(|x| x.as_str().map(|s| s.to_string())).collect());
    let mut report = Vec::new();
    let mut applied_total = 0usize;
    for t in &tables {
        if let Some(w) = &want {
            if !w.contains(&t.name) {
                continue;
            }
        }
        if t.pk.is_empty() {
            report.push(J::obj(vec![
                ("table", J::s(&t.name)),
                ("status", J::s("skipped: no primary key on remote side")),
            ]));
            continue;
        }
        let local_t = {
            let db = e.db(&lname)?;
            db.tables.get(&t.name).cloned()
        };
        if direction == "pull" {
            let lt = match local_t {
                Some(t2) => t2,
                None => {
                    report.push(J::obj(vec![
                        ("table", J::s(&t.name)),
                        ("status", J::s("skipped: table missing locally (use db_extract to create it)")),
                    ]));
                    continue;
                }
            };
            let ident = qi(&t.name, rkind);
            let mut n = 0i64;
            let mut off = 0usize;
            loop {
                let sql = format!("SELECT * FROM {} LIMIT {} OFFSET {}", ident, 500, off);
                let res = rc_query(&mut c, &sql)?;
                let got = res.rows.len();
                if got == 0 {
                    break;
                }
                for row in qres_to_objects(&res) {
                    e.put_row(&lname, &t.name, &row, true)?;
                    n += 1;
                }
                off += got;
                if got < 500 {
                    break;
                }
            }
            let _ = lt;
            report.push(J::obj(vec![
                ("table", J::s(&t.name)),
                ("direction", J::s("pull")),
                ("upserted", J::Int(n)),
            ]));
            continue;
        }
        // plan / push
        let lt = match local_t {
            Some(t2) => t2,
            None => {
                report.push(J::obj(vec![
                    ("table", J::s(&t.name)),
                    ("status", J::s("skipped: table missing locally")),
                ]));
                continue;
            }
        };
        let ident = qi(&t.name, rkind);
        let res = rc_query(&mut c, &format!("SELECT * FROM {}", ident))?;
        if res.rows.len() > 500_000 {
            report.push(J::obj(vec![
                ("table", J::s(&t.name)),
                ("status", J::s("skipped: remote table too large for key compare (>500k rows)")),
            ]));
            continue;
        }
        let mut rmap: HashMap<String, Vec<(String, J)>> = HashMap::new();
        for row in qres_to_objects(&res) {
            let pkv: Vec<J> = t
                .pk
                .iter()
                .map(|p| row.get(p).cloned().unwrap_or(J::Null))
                .collect();
            let key = jstr(&J::Arr(pkv));
            rmap.insert(key, colvals_map(&row));
        }
        let mut to_push: Vec<(String, Vec<(String, J)>)> = Vec::new();
        let mut same = 0i64;
        for (k, lrow) in &lt.rows {
            let lproj: Vec<(String, J)> = lt
                .cols
                .iter()
                .map(|c| (c.name.clone(), lrow.iter().find(|(kk, _)| kk == &c.name).map(|(_, v)| v.clone()).unwrap_or(J::Null)))
                .collect();
            match rmap.get(k) {
                None => to_push.push((k.clone(), lproj)),
                Some(rrow) => {
                    let rproj: Vec<(String, J)> = lt
                        .cols
                        .iter()
                        .map(|c| {
                            let rv = rrow.iter().find(|(kk, _)| kk == &c.name).map(|(_, v)| v.clone()).unwrap_or(J::Null);
                            (c.name.clone(), coerce_norm(rv, &c.dtype))
                        })
                        .collect();
                    let lnorm: Vec<(String, J)> = lproj
                        .iter()
                        .map(|(k2, v)| (k2.clone(), coerce_norm(v.clone(), &lt.cols.iter().find(|c| &c.name == k2).map(|c| c.dtype.as_str()).unwrap_or("any"))))
                        .collect();
                    if row_eq(&lnorm, &rproj) {
                        same += 1;
                    } else {
                        to_push.push((k.clone(), lproj));
                    }
                }
            }
        }
        let entry = J::obj(vec![
            ("table", J::s(&t.name)),
            ("identical", J::Int(same)),
            ("to_push", J::Int(to_push.len() as i64)),
            ("direction", J::s(if direction == "push" { "push" } else { "plan" })),
        ]);
        if direction == "push" {
            let mut applied = 0usize;
            for (k, row) in to_push {
                if applied_total + applied >= limit {
                    report.push(entry);
                    report.push(J::obj(vec![("note", J::s("apply limit reached — re-run db_sync to continue (key-addressed, idempotent)"))]));
                    break;
                }
                let pkv: Vec<J> = t.pk.iter().map(|p2| row.iter().find(|(kk, _)| kk == p2).map(|(_, v)| v.clone()).unwrap_or(J::Null)).collect();
                if pkv.iter().any(|v| v.is_null()) {
                    continue;
                }
                let collist: Vec<String> = row.iter().map(|(c, _)| qi(c, rkind)).collect();
                let vallist: Vec<String> = row.iter().map(|(_, v)| sql_lit(v)).collect();
                let upd: Vec<String> = row
                    .iter()
                    .filter(|(c, _)| !t.pk.contains(c))
                    .map(|(c, v)| {
                        if rkind == "pg" {
                            format!("{}=EXCLUDED.{}", qi(c, "pg"), qi(c, "pg"))
                        } else {
                            format!("{}=VALUES({})", qi(c, "my"), qi(c, "my"))
                        }
                    })
                    .collect();
                let mut sql = if rkind == "pg" {
                    format!(
                        "INSERT INTO {} ({}) VALUES ({})",
                        ident,
                        collist.join(", "),
                        vallist.join(", ")
                    )
                } else {
                    format!(
                        "INSERT INTO {} ({}) VALUES ({})",
                        ident,
                        collist.join(", "),
                        vallist.join(", ")
                    )
                };
                if !upd.is_empty() {
                    if rkind == "pg" {
                        sql.push_str(&format!(" ON CONFLICT ({}) DO UPDATE SET {}", t.pk.iter().map(|p2| qi(p2, "pg")).collect::<Vec<_>>().join(", "), upd.join(", ")));
                    } else {
                        sql.push_str(&format!(" ON DUPLICATE KEY UPDATE {}", upd.join(", ")));
                    }
                }
                rc_query(&mut c, &sql)?;
                applied += 1;
            }
            applied_total += applied;
            report.push(J::obj(vec![
                ("table", J::s(&t.name)),
                ("identical", J::Int(same)),
                ("to_push", J::Int(applied as i64)),
                ("direction", J::s("push")),
            ]));
        } else {
            report.push(entry);
        }
    }
    Ok(J::obj(vec![
        ("mode", J::s(&direction)),
        ("local_db", J::s(&lname)),
        ("tables", J::Arr(report)),
    ]))
}

fn coerce_norm(v: J, dtype: &str) -> J {
    coerce(v, dtype, "-").unwrap_or(J::Null)
}

// ============================================================================
// FINGERPRINT — API/JSON -> inferred schema + keys -> a real local database
// (the "which database is behind these API calls" bridge for camoufox captures)
// ============================================================================

struct FpTable {
    name: String,
    cols: Vec<Col>,
    pk: Vec<String>,
    rows: Vec<J>,
    fk_proposals: Vec<Fk>,
    notes: Vec<String>,
}

impl FpTable {
    fn to_table(&self) -> Table {
        Table {
            name: self.name.clone(),
            cols: self.cols.clone(),
            pk: self.pk.clone(),
            fks: Vec::new(),
            uniques: Vec::new(),
            indexes: Vec::new(),
            auto: BTreeMap::new(),
            nocheck: false,
            rows: HashMap::new(),
            idx: HashMap::new(),
            wlines: 0,
        }
    }
}

fn jtype_of(v: &J) -> &'static str {
    match v {
        J::Null => "null",
        J::Bool(_) => "boolean",
        J::Int(_) => "integer",
        J::Num(_) => "real",
        J::Str(_) => "text",
        J::Arr(_) => "array",
        J::Obj(_) => "object",
    }
}

fn fp_analyze_payload(payload: &J) -> R<Vec<FpTable>> {
    // collect candidate arrays of objects at depth 1 (root) and depth 2 (root fields)
    let mut arrays: Vec<(String, Vec<J>)> = Vec::new();
    match payload {
        J::Arr(a) => {
            if a.iter().all(|x| matches!(x, J::Obj(_))) {
                arrays.push(("records".to_string(), a.clone()));
            }
        }
        J::Obj(m) => {
            let scalar: Vec<(String, J)> = m.iter().filter(|(_, v)| !matches!(v, J::Arr(_)) && !matches!(v, J::Obj(_))).cloned().collect();
            if !scalar.is_empty() {
                arrays.push(("_root".to_string(), vec![J::Obj(scalar)]));
            }
            for (k, v) in m {
                if let J::Arr(a2) = v {
                    if !a2.is_empty() && a2.iter().all(|x| matches!(x, J::Obj(_))) {
                        arrays.push((sanitize_name(&k.replace('-', "_")).unwrap_or_else(|_| "table".into()), a2.clone()));
                    }
                }
            }
        }
        _ => return Ok(Vec::new()),
    }
    let mut out = Vec::new();
    for (tname, items) in &arrays {
        let mut t = fp_analyze_table(tname, items)?;
        // nested arrays inside rows become child tables
        let mut child_fields: Vec<(String, String)> = Vec::new(); // (field, child table)
        if let Some(first) = items.iter().find_map(|x| match x {
            J::Obj(m) => Some(m.clone()),
            _ => None,
        }) {
            for (k, v) in &first {
                if let J::Arr(a2) = v {
                    if !a2.is_empty() && a2.iter().all(|x| matches!(x, J::Obj(_))) {
                        let cname = sanitize_name(&format!("{}_{}", tname, k)).unwrap_or_else(|_| format!("{}_rows", tname));
                        child_fields.push((k.clone(), cname));
                    }
                }
            }
        }
        for (field, cname) in &child_fields {
            let mut child_rows = Vec::new();
            for (ri, item) in items.iter().enumerate() {
                if let J::Obj(m) = item {
                    if let Some(J::Arr(a2)) = jget(m, field) {
                        for citem in a2 {
                            if let J::Obj(cm) = citem {
                                let mut extended = cm.clone();
                                let parent_key_col = if t.pk.is_empty() { "_row".to_string() } else { t.pk[0].clone() };
                                let parent_val = if t.pk.is_empty() {
                                    J::Int((ri + 1) as i64)
                                } else {
                                    jget(m, &parent_key_col).unwrap_or(J::Null)
                                };
                                extended.push((format!("_parent_{}", parent_key_col), parent_val));
                                child_rows.push(J::Obj(extended));
                            }
                        }
                    }
                }
            }
            if !child_rows.is_empty() {
                let mut ct = fp_analyze_table(cname, &child_rows)?;
                let parent_key_col = if t.pk.is_empty() { "_row".to_string() } else { t.pk[0].clone() };
                ct.fk_proposals.push(Fk {
                    cols: vec![format!("_parent_{}", parent_key_col)],
                    ref_table: tname.clone(),
                    ref_cols: vec![parent_key_col],
                    on_delete: "cascade".into(),
                });
                ct.notes.push(format!("child of {} via {}", tname, field));
                out.push(ct);
            }
        }
        out.push(t);
    }
    // cross-table fk proposals: <sing>_id fields pointing at other tables
    // (collect first immutably, then apply mutably)
    let names: Vec<String> = out.iter().map(|t| t.name.clone()).collect();
    struct FkAdd {
        table: String,
        fk: Fk,
        note: String,
    }
    let mut adds: Vec<FkAdd> = Vec::new();
    for t in out.iter() {
        for c in &t.cols {
            let cn = &c.name;
            if let Some(base) = cn.strip_suffix("_id") {
                for other in &names {
                    if other == &t.name {
                        continue;
                    }
                    let o = other.trim_end_matches('s').trim_end_matches('e');
                    let b = base.trim_end_matches('s').trim_end_matches('e');
                    if other == base || other.replace('_', "") == base.replace('_', "") || o == b {
                        // value containment check
                        let parent = match out.iter().find(|x| &x.name == other) {
                            Some(p) => p,
                            None => break,
                        };
                        if parent.pk.is_empty() {
                            break;
                        }
                        let pcol = parent.pk[0].clone();
                        let pvals: Vec<J> = parent
                            .rows
                            .iter()
                            .filter_map(|r| r.get(&pcol).cloned())
                            .collect();
                        let cvals: Vec<J> = t
                            .rows
                            .iter()
                            .filter_map(|r| r.get(cn).cloned())
                            .filter(|v| !v.is_null())
                            .collect();
                        if !cvals.is_empty() && cvals.iter().all(|v| pvals.iter().any(|p| jeq(p, v))) {
                            adds.push(FkAdd {
                                table: t.name.clone(),
                                fk: Fk {
                                    cols: vec![cn.clone()],
                                    ref_table: other.clone(),
                                    ref_cols: vec![pcol.clone()],
                                    on_delete: "restrict".into(),
                                },
                                note: format!("inferred fk {}.{} -> {}.{}", t.name, cn, other, pcol),
                            });
                        }
                        break;
                    }
                }
            }
        }
    }
    for a in adds {
        if let Some(t) = out.iter_mut().find(|x| x.name == a.table) {
            t.fk_proposals.push(a.fk);
            t.notes.push(a.note);
        }
    }
    Ok(out)
}

fn fp_analyze_table(name: &str, items: &[J]) -> R<FpTable> {
    // gather fields
    let mut fields: Vec<String> = Vec::new();
    let mut by_field: BTreeMap<String, Vec<J>> = BTreeMap::new();
    for it in items {
        if let J::Obj(m) = it {
            for (k, v) in m {
                if !by_field.contains_key(k) {
                    fields.push(k.clone());
                }
                by_field.entry(k.clone()).or_default().push(v.clone());
            }
        }
    }
    let mut cols = Vec::new();
    let mut notes = Vec::new();
    for f in &fields {
        let vals = &by_field[f];
        let nn: Vec<&J> = vals.iter().filter(|v| !v.is_null()).collect();
        let types: HashSet<&'static str> = nn.iter().map(|v| jtype_of(v)).collect();
        let (dtype, nullable) = if nn.is_empty() {
            ("json", true)
        } else if types.len() == 1 {
            (
                match *types.iter().next().unwrap() {
                    "integer" => "integer",
                    "real" => "real",
                    "boolean" => "boolean",
                    "text" => "text",
                    _ => "json",
                },
                nn.len() < vals.len(),
            )
        } else if types.contains("integer") && types.contains("real") {
            ("real", nn.len() < vals.len())
        } else if types.contains("integer") && types.contains("text") {
            ("text", nn.len() < vals.len())
        } else if types.contains("real") && types.contains("text") {
            ("text", nn.len() < vals.len())
        } else {
            ("json", nn.len() < vals.len())
        };
        if dtype == "json" {
            notes.push(format!("field '{}' kept as json (mixed/object values)", f));
        }
        cols.push(Col {
            name: sanitize_col(f).unwrap_or_else(|_| format!("f_{}", fields.iter().position(|x| x == f).unwrap_or(0))),
            dtype: dtype.to_string(),
            nullable,
            auto: false,
        });
    }
    // candidate primary key
    let prio = ["id", "_id", "uuid", "guid", "key", "code", "pk"];
    let mut pk = Vec::new();
    let mut tried: Vec<String> = Vec::new();
    for p in prio.iter() {
        tried.push(p.to_string());
    }
    for f in &fields {
        if f.ends_with("_id") || f.ends_with("_key") || f.ends_with("_uuid") {
            tried.push(f.clone());
        }
    }
    for cand in &tried {
        if let Some(vals) = by_field.get(cand) {
            let nn: Vec<J> = vals.iter().filter(|v| !v.is_null()).cloned().collect();
            let uniq: HashSet<String> = nn.iter().map(jstr).collect();
            if !nn.is_empty() && nn.len() == vals.len() && uniq.len() == nn.len() {
                pk = vec![sanitize_col(cand).unwrap_or_else(|_| "id".into())];
                notes.push(format!("primary key inferred: {}", cand));
                break;
            }
        }
    }
    // synth _row key when nothing usable
    if pk.is_empty() {
        cols.push(Col { name: "_row".into(), dtype: "integer".into(), nullable: false, auto: true });
        notes.push("no reliable key found — synthetic auto key '_row' added".into());
    }
    // clean rows: project onto columns, assign _row when missing
    let mut rows = Vec::new();
    for (ri, it) in items.iter().enumerate() {
        if let J::Obj(m) = it {
            let mut r: Vec<(String, J)> = Vec::new();
            for c in &cols {
                let v = jget(m, &c.name).unwrap_or(J::Null);
                let v = match coerce(v, &c.dtype, &c.name) {
                    Ok(v) => v,
                    Err(_) => J::Null,
                };
                if v.is_null() && !c.nullable && !c.auto && c.name != "_row" {
                    // relax: keep null, engine insert uses upsert; make nullable
                }
                r.push((c.name.clone(), v));
            }
            if pk.len() == 1 && pk[0] == "_row" {
                if r.iter().find(|(k, _)| k == "_row").map(|(_, v)| v.is_null()).unwrap_or(true) {
                    if let Some(p) = r.iter_mut().position(|(k, _)| k == "_row") {
                        r[p].1 = J::Int((ri + 1) as i64);
                    }
                }
            }
            rows.push(J::Obj(r));
        }
    }
    // relax nullability after coercion pass (some values coerced to null)
    for c in cols.iter_mut() {
        if c.name != "_row" {
            c.nullable = true;
        }
    }
    Ok(FpTable { name: name.to_string(), cols, pk, rows, fk_proposals: Vec::new(), notes })
}

// ============================================================================
// TOOL REGISTRY — 27 tools
// ============================================================================

fn sch(name: &str, desc: &str, props: Vec<(&str, &str, &str, bool)>) -> J {
    let mut pmap: Vec<(String, J)> = Vec::new();
    let mut req = Vec::new();
    for (pn, ptype, pdesc, preq) in props {
        let mut pj: Vec<(String, J)> = vec![];
        if !ptype.is_empty() {
            pj.push(("type".to_string(), J::s(ptype)));
        }
        pj.push(("description".to_string(), J::s(pdesc)));
        pmap.push((pn.to_string(), J::Obj(pj)));
        if preq {
            req.push(J::s(pn));
        }
    }
    J::obj(vec![
        ("name", J::s(name)),
        ("description", J::s(desc)),
        (
            "inputSchema",
            J::obj(vec![
                ("type", J::s("object")),
                ("properties", J::Obj(pmap)),
                ("required", J::Arr(req)),
            ]),
        ),
    ])
}

fn tools_list() -> J {
    let tools = vec![
        sch("db_status", "Engine status: home dir, databases, tables, row counts, connections, snapshots.", vec![]),
        sch("db_create_table", "Create a table. Keys are first-class: primary_key, foreign_keys, unique_keys, indexes. dtypes: integer|real|text|boolean|json.", vec![
            ("db", "string", "database name", true),
            ("table", "string", "table name", true),
            ("columns", "array", "[{name, dtype, nullable?, auto?}] — auto=true makes an integer auto-increment key", true),
            ("primary_key", "array", "primary key column names (key-addressed identity)", false),
            ("foreign_keys", "array", "[{columns, ref_table, ref_columns, on_delete? restrict|cascade}]", false),
            ("unique_keys", "array", "list of column-combos [[a,b]] that must be unique", false),
            ("indexes", "array", "[{name?, columns, unique?}]", false),
        ]),
        sch("db_drop_table", "Drop a table.", vec![("db", "string", "database", true), ("table", "string", "table", true)]),
        sch("db_schema", "Show a table's full schema — its key structure (PK/FK/unique/index).", vec![("db", "string", "database", true), ("table", "string", "table", true)]),
        sch("db_insert", "Insert rows. Enforces PK uniqueness, FK integrity, unique keys; assigns auto keys.", vec![
            ("db", "string", "database", true),
            ("table", "string", "table", true),
            ("rows", "array", "rows to insert", true),
            ("upsert", "boolean", "upsert by primary key instead of strict insert (default false)", false),
        ]),
        sch("db_get", "Fetch a row by its primary key (key-addressed read).", vec![
            ("db", "string", "database", true),
            ("table", "string", "table", true),
            ("key", "any", "primary key value (scalar, or array for composite keys)", true),
        ]),
        sch("db_update", "Update rows by key or by filter. Cannot change primary keys.", vec![
            ("db", "string", "database", true),
            ("table", "string", "table", true),
            ("key", "any", "primary key of the row", false),
            ("where", "array", "filters [[col, op, value]] — ops: = != < <= > >= like contains in is_null not_null", false),
            ("set", "object", "column -> new value", true),
        ]),
        sch("db_delete", "Delete rows by key or filter. Honors foreign keys (restrict / cascade).", vec![
            ("db", "string", "database", true),
            ("table", "string", "table", true),
            ("key", "any", "primary key of the row", false),
            ("where", "array", "filters [[col, op, value]]", false),
        ]),
        sch("db_scan", "Scan rows with filters/order/pagination. Uses the key-index fast path for equality.", vec![
            ("db", "string", "database", true),
            ("table", "string", "table", true),
            ("where", "array", "filters [[col, op, value]]", false),
            ("order", "string", "order by column", false),
            ("desc", "boolean", "descending order", false),
            ("limit", "integer", "max rows (default 200)", false),
            ("offset", "integer", "skip rows", false),
            ("cols", "array", "project these columns", false),
        ]),
        sch("db_count", "Count rows (optionally filtered).", vec![
            ("db", "string", "database", true),
            ("table", "string", "table", true),
            ("where", "array", "filters [[col, op, value]]", false),
        ]),
        sch("db_sql", "Mini SQL: SELECT cols FROM table [WHERE ...] [ORDER BY ...] [LIMIT n [OFFSET m]]. Key-index aware.", vec![
            ("db", "string", "database", true),
            ("sql", "string", "e.g. SELECT * FROM users WHERE age > 25 ORDER BY name LIMIT 50", true),
        ]),
        sch("db_keys", "THE key-graph map: every table's primary/foreign/unique keys and indexes + exactly where that key data is stored. Works for local db or remote connection.", vec![
            ("db", "string", "local database name", false),
            ("connection", "string", "saved connection name (remote introspection mode)", false),
            ("conn", "object", "inline connection {kind: postgres|mysql, host, port, user, password, database}", false),
        ]),
        sch("db_graph", "Walk the foreign-key graph from one row: children that reference it and parents it references, across tables, N hops.", vec![
            ("db", "string", "database", true),
            ("table", "string", "start table", true),
            ("key", "any", "primary key of the start row", true),
            ("hops", "integer", "depth (default 2)", false),
            ("direction", "string", "out | in | both (default both)", false),
        ]),
        sch("db_topo", "Foreign-key topological order of tables (parents before children) + cycle detection.", vec![("db", "string", "database", true)]),
        sch("db_diff", "Key-addressed diff of two databases: added / removed / changed rows per table, compared by primary key.", vec![
            ("db_a", "string", "first database", true),
            ("db_b", "string", "second database", true),
            ("limit", "integer", "max keys listed per table (default 50)", false),
        ]),
        sch("db_snapshot", "Git-like snapshots of a database (key-addressed). actions: create | list | restore | drop.", vec![
            ("action", "string", "create | list | restore | drop", true),
            ("db", "string", "database", true),
            ("name", "string", "snapshot name", false),
        ]),
        sch("db_branch", "Create a branch: a full copy of a database (optionally from a snapshot) to work on independently.", vec![
            ("db", "string", "source database", true),
            ("branch", "string", "new branch/database name", true),
            ("from_snapshot", "string", "seed the branch from a snapshot name", false),
        ]),
        sch("db_merge", "Merge one database into another by primary key. strategies: upsert | skip | replace.", vec![
            ("from_db", "string", "source branch", true),
            ("into_db", "string", "target database", true),
            ("strategy", "string", "upsert | skip | replace (default upsert)", false),
        ]),
        sch("db_sync", "Key-based incremental sync between local and remote database: plan | pull | push. Idempotent and resumable.", vec![
            ("connection", "string", "saved connection name", false),
            ("conn", "object", "inline connection object", false),
            ("local_db", "string", "local database", true),
            ("direction", "string", "plan | pull | push (default plan)", false),
            ("tables", "array", "restrict to these tables", false),
            ("limit", "integer", "max push ops per run (default 2000)", false),
        ]),
        sch("db_connect", "Test a remote connection and save it as a named profile. kinds: postgres | mysql | http.", vec![
            ("name", "string", "profile name to save", true),
            ("kind", "string", "postgres | mysql | http", true),
            ("host", "string", "host (postgres/mysql)", false),
            ("port", "integer", "port (default 5432 / 3306)", false),
            ("user", "string", "user", false),
            ("password", "string", "password", false),
            ("database", "string", "database name", false),
            ("url", "string", "http:// URL (http kind)", false),
            ("headers", "object", "http headers", false),
            ("body", "string", "http request body", false),
            ("save", "boolean", "save profile (default true)", false),
        ]),
        sch("db_remote_query", "Run SQL on a remote postgres/mysql database, or fetch a JSON endpoint (http).", vec![
            ("connection", "string", "saved connection name", false),
            ("conn", "object", "inline connection object", false),
            ("sql", "string", "SQL statement", false),
        ]),
        sch("db_remote_schema", "Introspect a remote database's schema and key graph (from pg_catalog / INFORMATION_SCHEMA).", vec![
            ("connection", "string", "saved connection name", false),
            ("conn", "object", "inline connection object", false),
        ]),
        sch("db_extract", "THE extractor: introspect remote keys, extract tables in FK-topological order (resumable, idempotent by key), save locally + portable bundle file.", vec![
            ("connection", "string", "saved connection name", false),
            ("conn", "object", "inline connection object", false),
            ("target_db", "string", "local database to create/fill", true),
            ("tables", "array", "restrict to these tables", false),
            ("page_size", "integer", "rows per page (default 500)", false),
            ("save_bundle", "boolean", "save portable extract bundle (default true)", false),
            ("bundle_name", "string", "bundle file name", false),
        ]),
        sch("db_import_bundle", "Import a snapshot/extract bundle file into a local database (idempotent by primary key).", vec![
            ("file", "string", "bundle path", true),
            ("target_db", "string", "target local database", true),
        ]),
        sch("db_export_bundle", "Export a live local database to a portable bundle file.", vec![
            ("db", "string", "database", true),
            ("name", "string", "bundle name (default: db name)", false),
        ]),
        sch("db_fingerprint", "NOVEL: infer a database schema (tables, keys, fks) from raw API/JSON payloads — e.g. responses captured by camoufox — and optionally materialize it as a real local database.", vec![
            ("json", "string", "raw JSON payload", false),
            ("file", "string", "path to a JSON file", false),
            ("name", "string", "name for the proposal/bundle", false),
            ("create", "boolean", "create the inferred tables + insert rows", false),
            ("target_db", "string", "target local database (default: fingerprint_<name>)", false),
        ]),
        sch("db_compact", "Force WAL compaction of a database (rewrites row logs).", vec![("db", "string", "database", true)]),
    ];
    J::obj(vec![("tools", J::Arr(tools))])
}

// ============================================================================
// HANDLERS
// ============================================================================

fn h_status(e: &mut Engine, _a: &J) -> R<J> {
    let root = e.home.clone();
    let mut dbs = Vec::new();
    if let Ok(rd) = fs::read_dir(root.join("databases")) {
        for d in rd.filter_map(|x| x.ok()) {
            let dn = d.file_name().to_string_lossy().to_string();
            if !d.path().join("catalog.json").exists() {
                continue;
            }
            if e.dbs.contains_key(&dn) {
                let db = e.dbs.get(&dn).unwrap();
                let mut tables = Vec::new();
                for t in db.tables.values() {
                    tables.push(J::obj(vec![
                        ("name", J::s(&t.name)),
                        ("rows", J::Int(t.rows.len() as i64)),
                        ("primary_key", J::Arr(t.pk.iter().map(|x| J::s(x)).collect())),
                        ("foreign_keys", J::Int(t.fks.len() as i64)),
                    ]));
                }
                dbs.push(J::obj(vec![("database", J::s(&dn)), ("open", J::Bool(true)), ("tables", J::Arr(tables))]));
            } else {
                dbs.push(J::obj(vec![("database", J::s(&dn)), ("open", J::Bool(false))]));
            }
        }
    }
    let conns: Vec<String> = load_connections(e).keys().cloned().collect();
    let mut snaps = 0usize;
    if let Ok(rd) = fs::read_dir(root.join("snapshots")) {
        snaps = rd.filter_map(|x| x.ok()).count();
    }
    let mut extracts = 0usize;
    if let Ok(rd) = fs::read_dir(root.join("extracts")) {
        extracts = rd.filter_map(|x| x.ok()).count();
    }
    Ok(J::obj(vec![
        ("engine", J::s("dbengine/1.0.0")),
        ("home", J::s(&root.display().to_string())),
        ("databases", J::Arr(dbs)),
        ("connections", J::Arr(conns.iter().map(|x| J::s(x)).collect())),
        ("snapshots", J::Int(snaps as i64)),
        ("extract_bundles", J::Int(extracts as i64)),
    ]))
}

fn h_create_table(e: &mut Engine, a: &J) -> R<J> {
    let db = arg_s(a, "db")?;
    let dname = sanitize_name(&db)?;
    let tname = sanitize_col(&arg_s(a, "table")?)?;
    let cols_j = a
        .get("columns")
        .and_then(|x| x.as_arr())
        .ok_or("'columns' must be an array of {name, dtype, nullable?, auto?}")?;
    let mut cols = Vec::new();
    for cj in cols_j {
        let n = sanitize_col(&arg_s(cj, "name")?)?;
        let d = arg_s(cj, "dtype")?;
        if !matches!(d.as_str(), "integer" | "real" | "text" | "boolean" | "json" | "any") {
            return Err(format!("column '{}': dtype must be integer|real|text|boolean|json|any", n));
        }
        cols.push(Col {
            name: n,
            dtype: d,
            nullable: arg_bool_d(cj, "nullable", true),
            auto: arg_bool_d(cj, "auto", false),
        });
    }
    if cols.is_empty() {
        return Err("table needs at least one column".into());
    }
    let pk = jarr_str(a.get("primary_key"));
    for p in &pk {
        if !cols.iter().any(|c| &c.name == p) {
            return Err(format!("primary key column '{}' not in columns", p));
        }
    }
    let mut fks = Vec::new();
    if let Some(fa) = a.get("foreign_keys").and_then(|x| x.as_arr()) {
        for fv in fa {
            let fcols = jarr_str(fv.get("columns"));
            for c in &fcols {
                if !cols.iter().any(|x| &x.name == c) {
                    return Err(format!("fk column '{}' not in columns", c));
                }
            }
            fks.push(Fk {
                cols: fcols,
                ref_table: arg_s(fv, "ref_table")?,
                ref_cols: jarr_str(fv.get("ref_columns")),
                on_delete: arg_opt_s(fv, "on_delete").unwrap_or_else(|| "restrict".into()),
            });
        }
    }
    let mut uniques = Vec::new();
    if let Some(ua) = a.get("unique_keys").and_then(|x| x.as_arr()) {
        for u in ua {
            let ucols = jarr_str(Some(u));
            for c in &ucols {
                if !cols.iter().any(|x| &x.name == c) {
                    return Err(format!("unique column '{}' not in columns", c));
                }
            }
            if !ucols.is_empty() {
                uniques.push(ucols);
            }
        }
    }
    let mut indexes = Vec::new();
    if let Some(ia) = a.get("indexes").and_then(|x| x.as_arr()) {
        for iv in ia {
            let icols = jarr_str(iv.get("columns"));
            for c in &icols {
                if !cols.iter().any(|x| &x.name == c) {
                    return Err(format!("index column '{}' not in columns", c));
                }
            }
            let nm = arg_opt_s(iv, "name").unwrap_or_else(|| format!("idx_{}_{}", tname, icols.join("_")));
            indexes.push(IndexDef { name: nm, cols: icols, unique: arg_bool_d(iv, "unique", false) });
        }
    }
    let auto: Vec<String> = cols.iter().filter(|c| c.auto).map(|c| c.name.clone()).collect();
    for an in &auto {
        if !pk.contains(an) {
            // allow non-pk autos too, but warn in result
        }
    }
    e.db(&dname)?;
    {
        let d = e.db(&dname)?;
        if d.tables.contains_key(&tname) {
            return Err(format!("table '{}' already exists", tname));
        }
        let t = Table {
            name: tname.clone(),
            cols,
            pk: pk.clone(),
            fks: fks.clone(),
            uniques: uniques.clone(),
            indexes: indexes.clone(),
            ..Default::default()
        };
        d.tables.insert(tname.clone(), t);
    }
    e.save_catalog(&dname)?;
    Ok(J::obj(vec![
        ("created", J::s(&format!("{}/{}", dname, tname))),
        ("primary_key", J::Arr(pk.iter().map(|x| J::s(x)).collect())),
        ("foreign_keys", J::Int(fks.len() as i64)),
        ("unique_keys", J::Int(uniques.len() as i64)),
        ("indexes", J::Int(indexes.len() as i64)),
    ]))
}

fn h_drop_table(e: &mut Engine, a: &J) -> R<J> {
    let db = sanitize_name(&arg_s(a, "db")?)?;
    let tname = sanitize_col(&arg_s(a, "table")?)?;
    e.db(&db)?;
    {
        let d = e.db(&db)?;
        if d.tables.remove(&tname).is_none() {
            return Err(format!("table '{}' not found", tname));
        }
    }
    e.save_catalog(&db)?;
    let wal = e
        .db_dir(&db)
        .join("wal")
        .join(format!("{}.jsonl", wal_table_name(&tname)));
    let _ = fs::remove_file(wal);
    Ok(J::obj(vec![("dropped", J::s(&format!("{}/{}", db, tname)))]))
}

fn h_schema(e: &mut Engine, a: &J) -> R<J> {
    let db = sanitize_name(&arg_s(a, "db")?)?;
    let tname = sanitize_col(&arg_s(a, "table")?)?;
    let d = e.db(&db)?;
    let t = d
        .tables
        .get(&tname)
        .ok_or_else(|| format!("table '{}' not found", tname))?;
    let mut tj = table_to_json(t);
    if let J::Obj(m) = &mut tj {
        m.push(("row_count".into(), J::Int(t.rows.len() as i64)));
        m.push((
            "keys_note".into(),
            J::s("rows are addressed by primary-key value; FK edges above define the key graph"),
        ));
    }
    Ok(tj)
}

fn h_insert(e: &mut Engine, a: &J) -> R<J> {
    let db = sanitize_name(&arg_s(a, "db")?)?;
    let tname = sanitize_col(&arg_s(a, "table")?)?;
    let upsert = arg_bool_d(a, "upsert", false);
    let rows = a
        .get("rows")
        .and_then(|x| x.as_arr())
        .cloned()
        .or_else(|| a.get("row").map(|r| vec![r.clone()]))
        .ok_or("'rows' must be an array of row objects")?;
    let mut keys = Vec::new();
    for r in &rows {
        keys.push(J::s(&e.put_row(&db, &tname, r, upsert)?));
    }
    Ok(J::obj(vec![
        ("inserted", J::Int(keys.len() as i64)),
        ("keys", J::Arr(keys)),
    ]))
}

fn key_from_arg(t: &Table, k: &J) -> String {
    let pkv = match k {
        J::Arr(a) => a.clone(),
        scalar if t.pk.len() == 1 => vec![scalar.clone()],
        other => vec![other.clone()],
    };
    jstr(&J::Arr(pkv))
}

fn h_get(e: &mut Engine, a: &J) -> R<J> {
    let db = sanitize_name(&arg_s(a, "db")?)?;
    let tname = sanitize_col(&arg_s(a, "table")?)?;
    let k = a.get("key").cloned().ok_or("missing param 'key'")?;
    let d = e.db(&db)?;
    let t = d
        .tables
        .get(&tname)
        .ok_or_else(|| format!("table '{}' not found", tname))?;
    if t.pk.is_empty() {
        return Err(format!("table '{}' has no primary key", tname));
    }
    let key = key_from_arg(t, &k);
    let row = t
        .rows
        .get(&key)
        .cloned()
        .ok_or_else(|| format!("row {} not found", key))?;
    Ok(J::obj(vec![("key", J::s(&key)), ("row", J::Obj(row))]))
}

fn h_update(e: &mut Engine, a: &J) -> R<J> {
    let db = sanitize_name(&arg_s(a, "db")?)?;
    let tname = sanitize_col(&arg_s(a, "table")?)?;
    let set = a
        .get("set")
        .and_then(|x| x.as_obj())
        .cloned()
        .ok_or("'set' must be an object {col: value}")?;
    let d = e.db(&db)?;
    let t = d
        .tables
        .get(&tname)
        .ok_or_else(|| format!("table '{}' not found", tname))?;
    let pk = t.pk.clone();
    for (c, _) in &set {
        if pk.contains(c) {
            return Err(format!("cannot change primary key column '{}' via update (delete + insert instead)", c));
        }
        if !t.cols.iter().any(|x| &x.name == c) {
            return Err(format!("unknown column '{}'", c));
        }
    }
    drop(d);
    // select targets
    let mut targets = Vec::new();
    if let Some(k) = a.get("key") {
        let d = e.db(&db)?;
        let t = d.tables.get(&tname).unwrap();
        targets.push(key_from_arg(t, &k));
    } else if a.get("where").is_some() {
        let filters = parse_filters(a)?;
        let res = e.scan(&db, &tname, &filters, None, 1000, 0, None)?;
        if let Some(ks) = res.get("keys").and_then(|x| x.as_arr()) {
            for k in ks {
                if let Some(s) = k.as_str() {
                    targets.push(s.to_string());
                }
            }
        }
    } else {
        return Err("update needs 'key' or 'where'".into());
    }
    let mut updated = 0i64;
    for tk in &targets {
        let mut row = {
            let d = e.db(&db)?;
            d.tables
                .get(&tname)
                .and_then(|t| t.rows.get(tk))
                .cloned()
                .ok_or_else(|| format!("row {} not found", tk))?
        };
        for (c, v) in &set {
            if let Some(p) = row.iter_mut().position(|(k, _)| k == c) {
                row[p].1 = v.clone();
            }
        }
        e.put_row(&db, &tname, &J::Obj(row), true)?;
        updated += 1;
    }
    Ok(J::obj(vec![("updated", J::Int(updated))]))
}

fn h_delete(e: &mut Engine, a: &J) -> R<J> {
    let db = sanitize_name(&arg_s(a, "db")?)?;
    let tname = sanitize_col(&arg_s(a, "table")?)?;
    let mut targets = Vec::new();
    if let Some(k) = a.get("key") {
        let d = e.db(&db)?;
        let t = d
            .tables
            .get(&tname)
            .ok_or_else(|| format!("table '{}' not found", tname))?;
        if t.pk.is_empty() {
            return Err(format!("table '{}' has no primary key", tname));
        }
        targets.push(key_from_arg(t, &k));
    } else if a.get("where").is_some() {
        let filters = parse_filters(a)?;
        let res = e.scan(&db, &tname, &filters, None, 1000, 0, None)?;
        if let Some(ks) = res.get("keys").and_then(|x| x.as_arr()) {
            for k in ks {
                if let Some(s) = k.as_str() {
                    targets.push(s.to_string());
                }
            }
        }
    } else {
        return Err("delete needs 'key' or 'where'".into());
    }
    let mut n = 0u64;
    for tk in &targets {
        n += e.delete_key(&db, &tname, tk, 0)?;
    }
    Ok(J::obj(vec![("deleted", J::Int(n as i64))]))
}

fn h_scan(e: &mut Engine, a: &J) -> R<J> {
    let db = sanitize_name(&arg_s(a, "db")?)?;
    let tname = sanitize_col(&arg_s(a, "table")?)?;
    let filters = parse_filters(a)?;
    let order = arg_opt_s(a, "order").map(|o| (o, arg_bool_d(a, "desc", false)));
    let limit = arg_u64_d(a, "limit", 200) as usize;
    let offset = arg_u64_d(a, "offset", 0) as usize;
    let proj = a
        .get("cols")
        .and_then(|x| x.as_arr())
        .map(|arr| arr.iter().filter_map(|x| x.as_str().map(|s| s.to_string())).collect());
    e.scan(&db, &tname, &filters, order, limit, offset, proj)
}

fn h_count(e: &mut Engine, a: &J) -> R<J> {
    let db = sanitize_name(&arg_s(a, "db")?)?;
    let tname = sanitize_col(&arg_s(a, "table")?)?;
    let filters = parse_filters(a)?;
    let n = e.count_where(&db, &tname, &filters)?;
    Ok(J::obj(vec![("table", J::s(&tname)), ("count", J::Int(n))]))
}

fn h_sql(e: &mut Engine, a: &J) -> R<J> {
    let db = sanitize_name(&arg_s(a, "db")?)?;
    let q = arg_s(a, "sql")?;
    sql_exec(e, &db, &q)
}

fn h_keys(e: &mut Engine, a: &J) -> R<J> {
    if a.get("connection").is_some() || a.get("conn").is_some() {
        return remote_key_map(e, a);
    }
    let db = arg_s(a, "db")?;
    key_map_json(e, &db)
}

fn h_graph(e: &mut Engine, a: &J) -> R<J> {
    let db = sanitize_name(&arg_s(a, "db")?)?;
    let tname = sanitize_col(&arg_s(a, "table")?)?;
    let k = a.get("key").cloned().ok_or("missing param 'key'")?;
    let hops = arg_u64_d(a, "hops", 2) as usize;
    let dir = arg_opt_s(a, "direction").unwrap_or_else(|| "both".into());
    let key = {
        let d = e.db(&db)?;
        let t = d
            .tables
            .get(&tname)
            .ok_or_else(|| format!("table '{}' not found", tname))?;
        if t.pk.is_empty() {
            return Err(format!("table '{}' has no primary key", tname));
        }
        key_from_arg(t, &k)
    };
    graph_walk(e, &db, &tname, &key, hops.min(6), &dir)
}

fn h_topo(e: &mut Engine, a: &J) -> R<J> {
    let db = sanitize_name(&arg_s(a, "db")?)?;
    let order = {
        let d = e.db(&db)?;
        topo_order(&d.tables)?
    };
    Ok(J::obj(vec![
        ("database", J::s(&db)),
        ("topological_order", J::Arr(order.iter().map(|x| J::s(x)).collect())),
        ("note", J::s("parents before children — this is the safe insert/extract order for the key graph")),
    ]))
}

fn h_diff(e: &mut Engine, a: &J) -> R<J> {
    let da = arg_s(a, "db_a")?;
    let dbb = arg_s(a, "db_b")?;
    let limit = arg_u64_d(a, "limit", 50) as usize;
    diff_databases(e, &da, &dbb, limit)
}

fn h_snapshot(e: &mut Engine, a: &J) -> R<J> {
    let action = arg_s(a, "action")?;
    let db = arg_s(a, "db")?;
    match action.as_str() {
        "create" => {
            let name = arg_s(a, "name")?;
            snapshot_create(e, &db, &name)
        }
        "list" => snapshot_list(e, &db),
        "restore" => {
            let name = arg_s(a, "name")?;
            snapshot_restore(e, &db, &name)
        }
        "drop" => {
            let name = arg_s(a, "name")?;
            snapshot_drop(e, &db, &name)
        }
        other => Err(format!("unknown snapshot action '{}'", other)),
    }
}

fn h_branch(e: &mut Engine, a: &J) -> R<J> {
    let db = arg_s(a, "db")?;
    let branch = arg_s(a, "branch")?;
    let from = arg_opt_s(a, "from_snapshot");
    branch_create(e, &db, &branch, from.as_deref())
}

fn h_merge(e: &mut Engine, a: &J) -> R<J> {
    let from = arg_s(a, "from_db")?;
    let into = arg_s(a, "into_db")?;
    let strategy = arg_opt_s(a, "strategy").unwrap_or_else(|| "upsert".into());
    if !matches!(strategy.as_str(), "upsert" | "skip" | "replace") {
        return Err("strategy must be upsert | skip | replace".into());
    }
    merge_databases(e, &from, &into, &strategy)
}

fn h_sync(e: &mut Engine, a: &J) -> R<J> {
    sync_run(e, a)
}

fn h_connect(e: &mut Engine, a: &J) -> R<J> {
    let name = sanitize_name(&arg_s(a, "name")?)?;
    let kind = arg_s(a, "kind")?;
    if !matches!(kind.as_str(), "postgres" | "postgresql" | "pg" | "mysql" | "http") {
        return Err("kind must be postgres | mysql | http".into());
    }
    let mut profile_pairs: Vec<(String, J)> = Vec::new();
    if let J::Obj(m) = a {
        for (k, v) in m {
            if k == "name" || k == "save" {
                continue;
            }
            if k == "password" {
                profile_pairs.push((k.clone(), v.clone()));
            } else {
                profile_pairs.push((k.clone(), v.clone()));
            }
        }
    }
    // test
    let profile = J::Obj(profile_pairs.clone());
    let test = match kind.as_str() {
        "http" => {
            let mut c = rc_connect(&profile)?;
            let (code, body) = rc_http_fetch(&mut c)?;
            J::obj(vec![
                ("status", J::Int(code as i64)),
                (
                    "body_kind",
                    J::s(match &body {
                        J::Arr(_) => "array",
                        J::Obj(_) => "object",
                        _ => "other",
                    }),
                ),
            ])
        }
        _ => {
            let mut c = rc_connect(&profile)?;
            let res = rc_query(&mut c, if kind == "mysql" { "SELECT VERSION()" } else { "SELECT version()" })?;
            let v = res
                .rows
                .first()
                .and_then(|r| r.first().cloned())
                .map(|x| jstr(&x).trim_matches('"').to_string())
                .unwrap_or_default();
            J::obj(vec![("server", J::s(&v))])
        }
    };
    let saved = arg_bool_d(a, "save", true);
    if saved {
        let mut conns = load_connections(e);
        conns.insert(name.clone(), profile.clone());
        save_connections(e, &conns)?;
    }
    Ok(J::obj(vec![
        ("connection", J::s(&name)),
        ("tested", test),
        ("saved", J::Bool(saved)),
        ("note", J::s("profiles are stored in connections.json inside the engine home — treat as sensitive")),
    ]))
}

fn h_remote_query(e: &mut Engine, a: &J) -> R<J> {
    let p = resolve_conn(e, a)?;
    let kind = arg_s(&p, "kind")?;
    if kind == "http" {
        let mut c = rc_connect(&p)?;
        let (code, body) = rc_http_fetch(&mut c)?;
        let rows = match &body {
            J::Arr(items) if !items.is_empty() && items.iter().all(|x| matches!(x, J::Obj(_))) => {
                let mut cols = Vec::new();
                for it in items {
                    if let J::Obj(m) = it {
                        for (k, _) in m {
                            if !cols.contains(k) {
                                cols.push(k.clone());
                            }
                        }
                    }
                }
                J::obj(vec![
                    ("status", J::Int(code as i64)),
                    ("rows", J::Int(items.len() as i64)),
                    ("columns", J::Arr(cols.into_iter().map(|c| J::s(&c)).collect())),
                    ("data", body.clone()),
                ])
            }
            _ => J::obj(vec![("status", J::Int(code as i64)), ("data", body)]),
        };
        return Ok(rows);
    }
    let sql = arg_s(a, "sql")?;
    let mut c = rc_connect(&p)?;
    let res = rc_query(&mut c, &sql)?;
    let rows = qres_to_objects(&res);
    let affected = if res.tag.starts_with("affected=") {
        res.tag.trim_start_matches("affected=").parse::<i64>().unwrap_or(0)
    } else {
        0
    };
    Ok(J::obj(vec![
        ("columns", J::Arr(res.cols.iter().map(|x| J::s(x)).collect())),
        ("rows", J::Int(rows.len() as i64)),
        ("affected", J::Int(affected)),
        ("data", J::Arr(rows)),
        ("tag", J::s(&res.tag)),
    ]))
}

fn h_remote_schema(e: &mut Engine, a: &J) -> R<J> {
    remote_key_map(e, a)
}

fn h_extract(e: &mut Engine, a: &J) -> R<J> {
    extract_remote(e, a)
}

fn h_import_bundle(e: &mut Engine, a: &J) -> R<J> {
    let file = arg_s(a, "file")?;
    let target = sanitize_name(&arg_s(a, "target_db")?)?;
    let data = fs::read_to_string(&file).map_err(|err| format!("read {}: {}", file, err))?;
    let j = jparse(&data)?;
    let db = db_from_snapshot(&j, &target)?;
    let ntables = db.tables.len();
    let nrows: usize = db.tables.values().map(|t| t.rows.len()).sum();
    let kind = j.get("kind").and_then(|x| x.as_str()).unwrap_or("unknown").to_string();
    let existing = e.has_db(&target);
    if existing {
        // merge into existing: upsert rows table by table (topo order)
        let order = topo_order(&db.tables).unwrap_or_else(|_| db.tables.keys().cloned().collect());
        e.db(&target)?;
        for tn in &order {
            let tdef = table_to_json(&db.tables[tn]);
            ensure_table_from(e, &target, &db.tables[tn])?;
            if let Some(rs) = j.get("rows").and_then(|r| r.get(tn)).and_then(|x| x.as_arr()) {
                for rv in rs {
                    e.put_row(&target, tn, rv, true)?;
                }
            }
            let _ = tdef;
        }
        e.save_catalog(&target)?;
        Ok(J::obj(vec![
            ("imported", J::s(&file)),
            ("kind", J::s(&kind)),
            ("merged_into", J::s(&target)),
            ("tables", J::Int(ntables as i64)),
            ("rows_in_bundle", J::Int(nrows as i64)),
        ]))
    } else {
        e.dbs.insert(target.clone(), db);
        write_db_files(e, &target)?;
        Ok(J::obj(vec![
            ("imported", J::s(&file)),
            ("kind", J::s(&kind)),
            ("created", J::s(&target)),
            ("tables", J::Int(ntables as i64)),
            ("rows", J::Int(nrows as i64)),
        ]))
    }
}

fn h_export_bundle(e: &mut Engine, a: &J) -> R<J> {
    let db = sanitize_name(&arg_s(a, "db")?)?;
    let name = arg_opt_s(a, "name").unwrap_or_else(|| db.clone());
    let bname = sanitize_name(&name)?;
    let (catalog, rows) = {
        let d = e.db(&db)?;
        let mut rows = BTreeMap::new();
        for (tn, t) in &d.tables {
            let list: Vec<J> = t.rows.values().map(|r| J::Obj(r.clone())).collect();
            rows.insert(tn.clone(), J::Arr(list));
        }
        (db_to_json(d), J::Obj(rows.into_iter().collect()))
    };
    let bundle = J::obj(vec![
        ("kind", J::s("dbengine-export")),
        ("db", J::s(&db)),
        ("created", J::Int(now_ms() as i64)),
        ("catalog", catalog),
        ("rows", rows),
    ]);
    let path = e.home.join("extracts").join(format!("{}.json", bname));
    atomic_write(&path, &jpretty(&bundle))?;
    let ntables = bundle.get("rows").and_then(|x| x.as_obj()).map(|m| m.len()).unwrap_or(0);
    Ok(J::obj(vec![
        ("bundle", J::s(&path.display().to_string())),
        ("tables", J::Int(ntables as i64)),
    ]))
}

fn h_fingerprint(e: &mut Engine, a: &J) -> R<J> {
    let payload: J = if let Some(txt) = arg_opt_s(a, "json") {
        jparse(&txt)?
    } else if let Some(f) = arg_opt_s(a, "file") {
        let data = fs::read_to_string(&f).map_err(|err| format!("read {}: {}", f, err))?;
        jparse(&data)?
    } else {
        return Err("fingerprint needs 'json' (raw payload) or 'file'".into());
    };
    let name = arg_opt_s(a, "name").unwrap_or_else(|| "api".into());
    let create = arg_bool_d(a, "create", false);
    let proposals = fp_analyze_payload(&payload)?;
    if proposals.is_empty() {
        return Err("no arrays of objects found in payload — nothing to fingerprint".into());
    }
    let mut tables_j = Vec::new();
    for t in &proposals {
        tables_j.push(J::obj(vec![
            ("table", J::s(&t.name)),
            ("columns", J::Arr(t.cols.iter().map(|c| J::obj(vec![("name", J::s(&c.name)), ("dtype", J::s(&c.dtype)), ("nullable", J::Bool(c.nullable))])).collect())),
            ("primary_key", J::Arr(t.pk.iter().map(|x| J::s(x)).collect())),
            (
                "foreign_keys",
                J::Arr(
                    t.fk_proposals
                        .iter()
                        .map(|f| {
                            J::obj(vec![
                                ("columns", J::Arr(f.cols.iter().map(|x| J::s(x)).collect())),
                                ("references", J::s(&format!("{}.({})", f.ref_table, f.ref_cols.join(",")))),
                            ])
                        })
                        .collect(),
                ),
            ),
            ("rows", J::Int(t.rows.len() as i64)),
            ("notes", J::Arr(t.notes.iter().map(|x| J::s(x)).collect())),
        ]));
    }
    let mut result = vec![
        ("name", J::s(&name)),
        ("proposed_schema", J::Arr(tables_j)),
    ];
    if create {
        let target = arg_opt_s(a, "target_db").unwrap_or_else(|| format!("fingerprint_{}", sanitize_name(&name).unwrap_or_else(|_| "api".into())));
        let tname = sanitize_name(&target)?;
        e.db(&tname)?;
        for pr in &proposals {
            ensure_table(e, &tname, pr)?;
            for row in &pr.rows {
                e.put_row(&tname, &pr.name, row, true)?;
            }
            let db = e.db(&tname)?;
            for fk in &pr.fk_proposals {
                let mut add_fk = false;
                if let Some(t) = db.tables.get_mut(&pr.name) {
                    if !t.fks.iter().any(|f| f.cols == fk.cols) {
                        add_fk = true;
                    }
                }
                if add_fk && db.tables.contains_key(&fk.ref_table) {
                    if let Some(t) = db.tables.get_mut(&pr.name) {
                        t.fks.push(fk.clone());
                    }
                }
            }
        }
        e.save_catalog(&tname)?;
        result.push(("created", J::s(&tname)));
        let d = e.db(&tname)?;
        let counts: Vec<(String, J)> = d
            .tables
            .iter()
            .map(|(k, v)| (k.clone(), J::Int(v.rows.len() as i64)))
            .collect();
        result.push(("tables", J::Obj(counts)));
    }
    Ok(J::obj(result))
}

fn h_compact(e: &mut Engine, a: &J) -> R<J> {
    let db = sanitize_name(&arg_s(a, "db")?)?;
    e.db(&db)?;
    let tnames: Vec<String> = {
        let d = e.dbs.get(&db).unwrap();
        d.tables.keys().cloned().collect()
    };
    let mut n = 0usize;
    for tn in tnames {
        e.maybe_compact(&db, &tn)?;
        n += 1;
    }
    Ok(J::obj(vec![("compacted_tables", J::Int(n as i64))]))
}

// ============================================================================
// MCP SERVER (stdio, JSON-RPC 2.0, newline-delimited)
// ============================================================================

fn err_obj(id: J, code: i64, msg: &str) -> J {
    J::obj(vec![
        ("jsonrpc", J::s("2.0")),
        ("id", id),
        ("error", J::obj(vec![("code", J::Int(code)), ("message", J::s(msg))])),
    ])
}

pub fn tools_call(e: &mut Engine, params: &J) -> R<J> {
    let name = params.get("name").and_then(|x| x.as_str()).unwrap_or("").to_string();
    let args = params.get("arguments").cloned().unwrap_or(J::Obj(vec![]));
    let result: R<J> = match name.as_str() {
        "db_status" => h_status(e, &args),
        "db_create_table" => h_create_table(e, &args),
        "db_drop_table" => h_drop_table(e, &args),
        "db_schema" => h_schema(e, &args),
        "db_insert" => h_insert(e, &args),
        "db_get" => h_get(e, &args),
        "db_update" => h_update(e, &args),
        "db_delete" => h_delete(e, &args),
        "db_scan" => h_scan(e, &args),
        "db_count" => h_count(e, &args),
        "db_sql" => h_sql(e, &args),
        "db_keys" => h_keys(e, &args),
        "db_graph" => h_graph(e, &args),
        "db_topo" => h_topo(e, &args),
        "db_diff" => h_diff(e, &args),
        "db_snapshot" => h_snapshot(e, &args),
        "db_branch" => h_branch(e, &args),
        "db_merge" => h_merge(e, &args),
        "db_sync" => h_sync(e, &args),
        "db_connect" => h_connect(e, &args),
        "db_remote_query" => h_remote_query(e, &args),
        "db_remote_schema" => h_remote_schema(e, &args),
        "db_extract" => h_extract(e, &args),
        "db_import_bundle" => h_import_bundle(e, &args),
        "db_export_bundle" => h_export_bundle(e, &args),
        "db_fingerprint" => h_fingerprint(e, &args),
        "db_compact" => h_compact(e, &args),
        other => Err(format!("unknown tool '{}'", other)),
    };
    match result {
        Ok(v) => Ok(J::obj(vec![
            (
                "content",
                J::Arr(vec![J::obj(vec![("type", J::s("text")), ("text", J::s(&jpretty(&v)))])]),
            ),
        ])),
        Err(e2) => Ok(J::obj(vec![
            (
                "content",
                J::Arr(vec![J::obj(vec![("type", J::s("text")), ("text", J::s(&e2))])]),
            ),
            ("isError", J::Bool(true)),
        ])),
    }
}

pub fn selftest() -> i32 {
    let mut fails = 0;
    let check = |name: &str, ok: bool, fails: &mut i32| {
        println!("  [{}] {}", if ok { "ok" } else { "FAIL" }, name);
        if !ok {
            *fails += 1;
        }
    };
    // crypto vectors
    check("md5(abc)", hexe(&md5(b"abc")) == "900150983cd24fb0d6963f7d28e17f72", &mut fails);
    check("sha1(abc)", hexe(&sha1(b"abc")) == "a9993e364706816aba3e25717850c26c9cd0d89d", &mut fails);
    check(
        "sha256(abc)",
        hexe(&sha256(b"abc")) == "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad",
        &mut fails,
    );
    let hm = hmac_sha256(&[0x0bu8; 20], b"Hi There");
    check(
        "hmac-sha256 rfc4231#1",
        hexe(&hm) == "b0344c61d8db38535ca8afceaf0bf12b881dc200c9833da726e9376c2e32cff7",
        &mut fails,
    );
    let pv = pbkdf2_sha256(b"password", b"salt", 1, 32);
    check(
        "pbkdf2-sha256 vector",
        hexe(&pv) == "120fb6cffcf8b32c43e7225256c4f837a86548c92ccc35480805987cb70be17b",
        &mut fails,
    );
    // base64 + json round-trip
    let rt = b64d(&b64e(b"dbengine round trip 123 !")).unwrap_or_default();
    check("base64 roundtrip", rt == b"dbengine round trip 123 !", &mut fails);
    let sample = r#"{"a":[1,2.5,"x",null,true],"b":{"c":"é\n"}}"#;
    check("json roundtrip", jstr(&jparse(sample).unwrap_or(J::Null)) == jparse(sample).map(|x| jstr(&x)).unwrap_or_default(), &mut fails);
    // engine flow in temp home
    let home = env::temp_dir().join(format!(".dbengine_selftest_{}", now_ms()));
    let mut e = Engine::new(home.clone());
    let flow = (|| -> R<()> {
        let ta = J::obj(vec![
            ("db", J::s("t")),
            ("table", J::s("u")),
            ("columns", J::Arr(vec![
                J::obj(vec![("name", J::s("id")), ("dtype", J::s("integer")), ("nullable", J::Bool(false)), ("auto", J::Bool(true))]),
                J::obj(vec![("name", J::s("email")), ("dtype", J::s("text")), ("nullable", J::Bool(false))]),
            ])),
            ("primary_key", J::Arr(vec![J::s("id")])),
            ("unique_keys", J::Arr(vec![J::Arr(vec![J::s("email")])])),
        ]);
        h_create_table(&mut e, &ta)?;
        let tb = J::obj(vec![
            ("db", J::s("t")),
            ("table", J::s("o")),
            ("columns", J::Arr(vec![
                J::obj(vec![("name", J::s("id")), ("dtype", J::s("integer")), ("nullable", J::Bool(false)), ("auto", J::Bool(true))]),
                J::obj(vec![("name", J::s("user_id")), ("dtype", J::s("integer")), ("nullable", J::Bool(false))]),
            ])),
            ("primary_key", J::Arr(vec![J::s("id")])),
            ("foreign_keys", J::Arr(vec![J::obj(vec![
                ("columns", J::Arr(vec![J::s("user_id")])),
                ("ref_table", J::s("u")),
                ("ref_columns", J::Arr(vec![J::s("id")])),
                ("on_delete", J::s("cascade")),
            ])])),
        ]);
        h_create_table(&mut e, &tb)?;
        h_insert(&mut e, &J::obj(vec![
            ("db", J::s("t")),
            ("table", J::s("u")),
            ("rows", J::Arr(vec![J::obj(vec![("email", J::s("a@x"))]), J::obj(vec![("email", J::s("b@x"))])])),
        ]))?;
        h_insert(&mut e, &J::obj(vec![
            ("db", J::s("t")),
            ("table", J::s("o")),
            ("rows", J::Arr(vec![J::obj(vec![("user_id", J::Int(1))])])),
        ]))?;
        // reopen from disk (persistence check)
        e.dbs.clear();
        e.db("t")?;
        let g = h_get(&mut e, &J::obj(vec![("db", J::s("t")), ("table", J::s("u")), ("key", J::Int(2))]))?;
        if g.get("key").and_then(|x| x.as_str()) != Some("[2]") {
            return Err("get by key failed".into());
        }
        let km = key_map_json(&mut e, "t")?;
        if km.get("edges").and_then(|x| x.as_arr()).map(|a| a.len()).unwrap_or(0) != 1 {
            return Err("key graph edges wrong".into());
        }
        snapshot_create(&mut e, "t", "v1")?;
        h_insert(&mut e, &J::obj(vec![
            ("db", J::s("t")),
            ("table", J::s("u")),
            ("rows", J::Arr(vec![J::obj(vec![("email", J::s("c@x"))])])),
        ]))?;
        branch_create(&mut e, "t", "t_dev", None)?;
        diff_databases(&mut e, "t", "t_dev", 10)?;
        snapshot_restore(&mut e, "t", "v1")?;
        let cnt = h_count(&mut e, &J::obj(vec![("db", J::s("t")), ("table", J::s("u"))]))?;
        if cnt.get("count").and_then(|x| x.as_i64()) != Some(2) {
            return Err("snapshot restore did not roll back".into());
        }
        Ok(())
    })();
    match flow {
        Ok(_) => check("engine flow (persist/keys/graph/snapshot/branch/restore)", true, &mut fails),
        Err(e2) => {
            println!("  [FAIL] engine flow: {}", e2);
            fails += 1;
        }
    }
    let _ = fs::remove_dir_all(&home);
    if fails == 0 {
        println!("SELFTEST OK");
        0
    } else {
        println!("SELFTEST FAILED: {} failure(s)", fails);
        1
    }
}
