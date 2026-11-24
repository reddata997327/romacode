//! PDF reader — pure Rust: metadata, page inventory, best-effort text
//! extraction (FlateDecode streams, literal + hex strings, ToUnicode CMaps
//! for Identity-H fonts) and lossless page merge/split via object-graph
//! rewriting. Complex PDFs (encrypted, cross-reference streams with object
//! streams) are handled on a best-effort basis and reported honestly.

use std::collections::{BTreeMap, HashMap, HashSet};
use std::io::{Read, Write};

// ── object model ────────────────────────────────────────────────────────────

struct RawObj {
    /// Dict body (and any non-stream payload) between `obj` and `endobj`/`stream`.
    dict: String,
    /// Raw (still-compressed) stream bytes, if the object is a stream.
    stream: Option<Vec<u8>>,
}

struct PdfFile<'a> {
    data: &'a [u8],
    /// object number → object (brute-force scan; covers compressed-xref
    /// files whose objects are still top-level).
    objs: BTreeMap<u32, RawObj>,
    trailer_root: Option<(u32, u16)>,
}

impl<'a> PdfFile<'a> {
    fn parse(data: &'a [u8]) -> Self {
        let mut objs: BTreeMap<u32, RawObj> = BTreeMap::new();
        let hay = data;
        let mut i = 0;
        // scan for "N G obj": walk back over the generation number, the
        // separating whitespace and the object number, with digit-boundary
        // checks so binary data containing " obj" yields no phantom ids.
        while let Some(pos) = find(hay, i, b" obj") {
            let mut p = pos;
            while p > 0 && hay[p - 1].is_ascii_whitespace() {
                p -= 1;
            }
            let gen_end = p;
            while p > 0 && hay[p - 1].is_ascii_digit() {
                p -= 1;
            }
            let gen_start = p;
            while p > 0 && hay[p - 1].is_ascii_whitespace() {
                p -= 1;
            }
            let id_end = p;
            while p > 0 && hay[p - 1].is_ascii_digit() {
                p -= 1;
            }
            let id_start = p;
            let boundary_ok = id_start == 0 || !hay[id_start - 1].is_ascii_digit();
            let _ = gen_start;
            if id_start < id_end && gen_start < gen_end && boundary_ok {
                let id: u32 = std::str::from_utf8(&hay[id_start..id_end]).unwrap_or("0").parse().unwrap_or(0);
                if id > 0 && id < 100_000_000 {
                    let body_start = pos + 4;
                    let body_end = find(hay, body_start, b"endobj").unwrap_or(hay.len());
                    let body = &hay[body_start.min(hay.len())..body_end.min(hay.len())];
                    // stream?
                    let (dict, stream) = match find(hay, body_start, b"stream") {
                        Some(sp) if sp < body_end => {
                            let dict_end = sp;
                            // skip EOL after 'stream'
                            let mut ds = sp + 6;
                            if ds < hay.len() && hay[ds] == b'\r' {
                                ds += 1;
                            }
                            if ds < hay.len() && hay[ds] == b'\n' {
                                ds += 1;
                            }
                            let dict_str = String::from_utf8_lossy(&hay[body_start.min(hay.len())..dict_end]).into_owned();
                            // /Length may be an indirect ref — fall back to endstream scan
                            let end = find(hay, ds, b"endstream").unwrap_or(body_end);
                            let mut de = end;
                            if de > ds && hay[de - 1] == b'\n' {
                                de -= 1;
                            }
                            if de > ds && hay[de - 1] == b'\r' {
                                de -= 1;
                            }
                            let len_from_dict = dict_str
                                .split("/Length")
                                .nth(1)
                                .map(|rest| {
                                    let t: String = rest.chars().skip_while(|c| c.is_whitespace() || *c == '/' || c.is_alphabetic()).take_while(|c| c.is_ascii_digit()).collect();
                                    t.parse::<usize>().unwrap_or(0)
                                })
                                .unwrap_or(0);
                            let end2 = if len_from_dict > 0 && ds + len_from_dict <= hay.len() { ds + len_from_dict } else { de };
                            (dict_str, Some(hay[ds..end2.min(hay.len())].to_vec()))
                        }
                        _ => (String::from_utf8_lossy(body).into_owned(), None),
                    };
                    objs.insert(id, RawObj { dict, stream });
                    i = body_end.max(pos + 4);
                    continue;
                }
            }
            i = pos + 4;
        }
        // trailer /Root
        let mut trailer_root = None;
        if let Some(tp) = find(data, 0, b"trailer") {
            let seg = &data[tp..(tp + 600).min(data.len())];
            let s = String::from_utf8_lossy(seg);
            if let Some(r) = parse_ref(&s, "/Root") {
                trailer_root = Some(r);
            }
        }
        if trailer_root.is_none() {
            // no classic trailer: locate /Type /Catalog by scan
            for (id, o) in &objs {
                if o.dict.contains("/Type") && o.dict.contains("/Catalog") {
                    trailer_root = Some((*id, 0));
                    break;
                }
            }
        }
        Self { data, objs, trailer_root }
    }

    fn inflate(&self, raw: &[u8]) -> Option<Vec<u8>> {
        if raw.starts_with(b"\x78") {
            // zlib stream
            let mut out = Vec::new();
            let mut dec = flate2::read::ZlibDecoder::new(raw);
            dec.read_to_end(&mut out).ok()?;
            return Some(out);
        }
        // raw deflate
        let mut out = Vec::new();
        let mut dec = flate2::read::DeflateDecoder::new(raw);
        dec.read_to_end(&mut out).ok()?;
        Some(out)
    }

    fn stream_content(&self, id: u32) -> Option<Vec<u8>> {
        let o = self.objs.get(&id)?;
        let raw = o.stream.as_ref()?;
        if o.dict.contains("/FlateDecode") || o.dict.contains("/Fl") {
            self.inflate(raw).or_else(|| Some(raw.clone()))
        } else {
            Some(raw.clone())
        }
    }

    /// Walk the page tree from the catalog, in order.
    fn pages(&self) -> Vec<u32> {
        let mut out = Vec::new();
        let mut seen = HashSet::new();
        let root = self.trailer_root.map(|(id, _)| id);
        if let Some(cat) = root.and_then(|id| self.objs.get(&id)) {
            if let Some(pages_ref) = parse_ref(&cat.dict, "/Pages") {
                self.walk_pages(pages_ref.0, &mut out, &mut seen, 0);
            }
        }
        if out.is_empty() {
            // fallback: any object that looks like a page
            for (id, o) in &self.objs {
                if o.dict.contains("/Type /Page") && !o.dict.contains("/Pages") {
                    out.push(*id);
                }
            }
            out.sort_unstable();
        }
        out
    }

    fn walk_pages(&self, id: u32, out: &mut Vec<u32>, seen: &mut HashSet<u32>, depth: usize) {
        if depth > 64 || !seen.insert(id) {
            return;
        }
        let Some(o) = self.objs.get(&id) else { return };
        if o.dict.contains("/Type /Page") && !o.dict.contains("/Type /Pages") {
            out.push(id);
            return;
        }
        // interior node: kids
        if let Some(kids_seg) = extract_array(&o.dict, "/Kids") {
            for rf in refs_in(&kids_seg) {
                self.walk_pages(rf.0, out, seen, depth + 1);
            }
        }
    }
}

fn find(hay: &[u8], from: usize, pat: &[u8]) -> Option<usize> {
    if pat.is_empty() || hay.len() < pat.len() {
        return None;
    }
    let mut i = from;
    while i + pat.len() <= hay.len() {
        if &hay[i..i + pat.len()] == pat {
            return Some(i);
        }
        i += 1;
    }
    None
}

/// Parse `(id gen) R` after a key.
fn parse_ref(dict: &str, key: &str) -> Option<(u32, u16)> {
    let idx = dict.find(key)?;
    let rest = dict[idx + key.len()..].trim_start();
    let rest = rest.strip_prefix('[').unwrap_or(rest); // /Kids [ ... ] not handled here
    let mut it = rest.split_whitespace();
    let id: u32 = it.next()?.parse().ok()?;
    let gen: u16 = it.next()?.parse().ok()?;
    if it.next()? == "R" {
        Some((id, gen))
    } else {
        None
    }
}

/// All refs `N G R` inside a string.
fn refs_in(s: &str) -> Vec<(u32, u16)> {
    let mut out = Vec::new();
    let b = s.as_bytes();
    let mut i = 0;
    while i + 4 < b.len() {
        if b[i].is_ascii_digit() {
            let start = i;
            while i < b.len() && b[i].is_ascii_digit() {
                i += 1;
            }
            let num = &s[start..i];
            // " G R"
            let mut j = i;
            while j < b.len() && b[j].is_ascii_whitespace() {
                j += 1;
            }
            let gstart = j;
            while j < b.len() && b[j].is_ascii_digit() {
                j += 1;
            }
            let gen = &s[gstart..j];
            let mut k = j;
            while k < b.len() && (b[k] == b' ' || b[k] == b'\n' || b[k] == b'\r') {
                k += 1;
            }
            if k + 1 < b.len() && b[k] == b'R' && (k + 1 == b.len() || b[k + 1].is_ascii_whitespace() || b[k + 1] == b']') && !num.is_empty() && !gen.is_empty() {
                if let (Ok(n), Ok(g)) = (num.parse::<u32>(), gen.parse::<u16>()) {
                    out.push((n, g));
                }
            }
        }
        i += 1;
    }
    out
}

/// Extract the contents of `/Key [ ... ]` (balanced brackets).
fn extract_array(dict: &str, key: &str) -> Option<String> {
    let idx = dict.find(key)?;
    let rest = dict[idx + key.len()..].trim_start();
    if !rest.starts_with('[') {
        return None;
    }
    let mut depth = 0i32;
    let mut end = 0;
    for (i, c) in rest.char_indices() {
        match c {
            '[' => depth += 1,
            ']' => {
                depth -= 1;
                if depth == 0 {
                    end = i + 1;
                    break;
                }
            }
            _ => {}
        }
    }
    Some(rest[..end.max(1)].to_string())
}

// ── public read API ─────────────────────────────────────────────────────────

#[derive(Debug, Default)]
pub struct PdfInfo {
    pub page_count: usize,
    pub page_sizes: Vec<(f64, f64)>,
    pub title: Option<String>,
    pub author: Option<String>,
    pub producer: Option<String>,
    pub encrypted: bool,
    pub text: String,
    pub extraction_note: String,
}

fn parse_page_box(dict: &str) -> (f64, f64) {
    if let Some(seg) = extract_array(dict, "/MediaBox") {
        let nums: Vec<f64> = seg
            .split(|c: char| c == ' ' || c == '[' || c == ']' || c == '\n' || c == '\r')
            .filter(|t| !t.is_empty())
            .filter_map(|t| t.parse().ok())
            .collect();
        if nums.len() == 4 && nums[2] > nums[0] && nums[3] > nums[1] {
            return (nums[2] - nums[0], nums[3] - nums[1]);
        }
    }
    (595.28, 841.89)
}

/// Read metadata + text from PDF bytes.
pub fn read(data: &[u8]) -> Result<PdfInfo, String> {
    if !data.starts_with(b"%PDF") && !contains_header(data) {
        return Err("not a PDF (missing %PDF header)".into());
    }
    let f = PdfFile::parse(data);
    let encrypted = data.windows(8).any(|w| w == b"/Encrypt");
    let pages = f.pages();
    let mut info = PdfInfo {
        page_count: pages.len(),
        encrypted,
        ..Default::default()
    };
    for pid in &pages {
        if let Some(o) = f.objs.get(pid) {
            info.page_sizes.push(parse_page_box(&o.dict));
        }
    }
    // Info dict via trailer
    if let Some(tp) = find(data, 0, b"trailer") {
        let seg = String::from_utf8_lossy(&data[tp..(tp + 800).min(data.len())]).into_owned();
        if let Some((info_id, _)) = parse_ref(&seg, "/Info") {
            if let Some(o) = f.objs.get(&info_id) {
                info.title = pdf_string_value(&o.dict, "/Title");
                info.author = pdf_string_value(&o.dict, "/Author");
                info.producer = pdf_string_value(&o.dict, "/Producer");
            }
        }
    }
    let (text, note) = extract_text(&f, &pages);
    info.text = text;
    info.extraction_note = note;
    Ok(info)
}

fn contains_header(data: &[u8]) -> bool {
    data.windows(4).any(|w| w == b"%PDF")
}

/// Best-effort literal string value for /Key (literal or hex, unescaped).
fn pdf_string_value(dict: &str, key: &str) -> Option<String> {
    let idx = dict.find(key)?;
    let rest = dict[idx + key.len()..].trim_start();
    if let Some(h) = rest.strip_prefix('<') {
        let end = rest.find('>')?;
        let hex: String = rest[1..end].chars().filter(|c| c.is_ascii_hexdigit()).collect();
        let bytes: Vec<u8> = (0..hex.len() / 2).filter_map(|i| u8::from_str_radix(&hex[i * 2..i * 2 + 2], 16).ok()).collect();
        return Some(decode_utf16be(&bytes).unwrap_or_else(|| String::from_utf8_lossy(&bytes).into_owned()));
    }
    if let Some(l) = rest.strip_prefix('(') {
        let mut out = String::new();
        let mut depth = 1;
        let mut esc = false;
        for c in l.chars().skip(1) {
            if esc {
                match c {
                    'n' => out.push('\n'),
                    'r' => out.push('\r'),
                    't' => out.push('\t'),
                    other => out.push(other),
                }
                esc = false;
                continue;
            }
            match c {
                '\\' => esc = true,
                '(' => {
                    depth += 1;
                    out.push('(');
                }
                ')' => {
                    depth -= 1;
                    if depth == 0 {
                        break;
                    }
                    out.push(')');
                }
                other => out.push(other),
            }
        }
        return Some(out);
    }
    None
}

fn decode_utf16be(bytes: &[u8]) -> Option<String> {
    if bytes.len() >= 2 && bytes[0] == 0xFE && bytes[1] == 0xFF {
        let units: Vec<u16> = bytes[2..].chunks_exact(2).map(|c| u16::from_be_bytes([c[0], c[1]])).collect();
        return Some(String::from_utf16_lossy(&units));
    }
    None
}

// ── text extraction ─────────────────────────────────────────────────────────

/// Decode a ToUnicode CMap (bfchar + bfrange entries) → map.
fn parse_tounicode(cmap: &str) -> HashMap<u32, String> {
    let mut map = HashMap::new();
    let mut rest = cmap;
    while let Some(p) = rest.find("beginbfchar") {
        let seg = &rest[p..];
        let end = seg.find("endbfchar").unwrap_or(seg.len());
        for line in seg[..end].lines() {
            let line = line.trim();
            if let Some(src_end) = line.find("> <") {
                if line.starts_with('<') {
                    let gid = u32::from_str_radix(&line[1..src_end], 16).unwrap_or(0);
                    let dst = &line[src_end + 3..];
                    if let Some(close) = dst.find('>') {
                        let hex: String = dst[..close].chars().filter(|c| c.is_ascii_hexdigit()).collect();
                        let bytes: Vec<u8> = (0..hex.len() / 2)
                            .filter_map(|i| u8::from_str_radix(&hex[i * 2..i * 2 + 2], 16).ok())
                            .collect();
                        let s = decode_utf16be(&bytes).unwrap_or_else(|| {
                            // single UTF-16BE code unit (no BOM)
                            if bytes.len() == 2 {
                                String::from_utf16_lossy(&[u16::from_be_bytes([bytes[0], bytes[1]])])
                            } else {
                                String::new()
                            }
                        });
                        map.insert(gid, s);
                    }
                }
            }
        }
        rest = &rest[p + end..];
    }
    // bfrange: <start> <end> <dst>
    let mut rest2 = cmap;
    while let Some(p) = rest2.find("beginbfrange") {
        let seg = &rest2[p..];
        let end = seg.find("endbfrange").unwrap_or(seg.len());
        for line in seg[..end].lines() {
            let line = line.trim();
            if line.starts_with('<') {
                let parts: Vec<&str> = line.split("> <").collect();
                if parts.len() >= 3 {
                    let s = u32::from_str_radix(parts[0].trim_start_matches('<'), 16).unwrap_or(0);
                    let e = u32::from_str_radix(parts[1], 16).unwrap_or(0);
                    let dst_hex: String = parts[2].chars().take_while(|c| c.is_ascii_hexdigit()).collect();
                    let dst: Vec<u8> = (0..dst_hex.len() / 2).filter_map(|i| u8::from_str_radix(&dst_hex[i * 2..i * 2 + 2], 16).ok()).collect();
                    for gid in s..=e.min(s + 65535) {
                        // single-byte/simple case: dst + (gid - s)
                        let bytes = if dst.len() == 2 {
                            let v = u16::from_be_bytes([dst[0], dst[1]]) + (gid - s) as u16;
                            vec![(v >> 8) as u8, (v & 0xFF) as u8]
                        } else {
                            dst.clone()
                        };
                        let txt = decode_utf16be(&bytes).unwrap_or_else(|| {
                            if bytes.len() == 2 {
                                String::from_utf16_lossy(&[u16::from_be_bytes([bytes[0], bytes[1]])])
                            } else {
                                String::from_utf8_lossy(&bytes).into_owned()
                            }
                        });
                        map.insert(gid, txt);
                    }
                }
            }
        }
        rest2 = &rest2[p + end..];
    }
    map
}

/// Find the ToUnicode CMap for a font object id.
fn font_tounicode(f: &PdfFile, font_id: u32) -> HashMap<u32, String> {
    let mut map = HashMap::new();
    let Some(o) = f.objs.get(&font_id) else { return map };
    if let Some((tou_id, _)) = parse_ref(&o.dict, "/ToUnicode") {
        if let Some(content) = f.stream_content(tou_id) {
            let s = String::from_utf8_lossy(&content).into_owned();
            map = parse_tounicode(&s);
        }
    } else if let Some((desc_id, _)) = parse_ref(&o.dict, "/DescendantFonts") {
        // descendant font may carry it (rare)
        if let Some(d) = f.objs.get(&desc_id) {
            if let Some((tou_id, _)) = parse_ref(&d.dict, "/ToUnicode") {
                if let Some(content) = f.stream_content(tou_id) {
                    map = parse_tounicode(&String::from_utf8_lossy(&content));
                }
            }
        }
    }
    map
}

/// Page font resources: key (e.g. "F0") → ToUnicode map.
fn page_fonts(f: &PdfFile, page_id: u32) -> HashMap<String, HashMap<u32, String>> {
    let mut out = HashMap::new();
    let Some(p) = f.objs.get(&page_id) else { return out };
    let res_id = parse_ref(&p.dict, "/Resources").map(|(id, _)| id);
    let res_dict = match res_id.and_then(|id| f.objs.get(&id)) {
        Some(r) => r.dict.clone(),
        None => {
            // inline resources dict on the page
            match extract_dict(&p.dict, "/Resources") {
                Some(d) => d,
                None => return out,
            }
        }
    };
    if let Some(fonts_seg) = extract_dict(&res_dict, "/Font") {
        // /Font << /F0 N 0 R /F1 N 0 R >>
        for (k, v) in parse_name_refs(&fonts_seg) {
            out.insert(k, font_tounicode(f, v));
        }
    }
    out
}

/// Extract a balanced `<< ... >>` dict after a key.
fn extract_dict(dict: &str, key: &str) -> Option<String> {
    let idx = dict.find(key)?;
    let rest = dict[idx + key.len()..].trim_start();
    if !rest.starts_with("<<") {
        return None;
    }
    let mut depth = 0i32;
    let bytes = rest.as_bytes();
    let mut i = 0;
    let mut end = rest.len();
    while i + 1 < bytes.len() {
        if bytes[i] == b'<' && bytes[i + 1] == b'<' {
            depth += 1;
            i += 2;
            continue;
        }
        if bytes[i] == b'>' && i > 0 && bytes[i - 1] == b'>' {
            depth -= 1;
            if depth == 0 {
                end = i + 1;
                break;
            }
            i += 1;
            continue;
        }
        i += 1;
    }
    Some(rest[..end].to_string())
}

/// Parse `/Name N G R` pairs inside a font dict.
fn parse_name_refs(seg: &str) -> Vec<(String, u32)> {
    let mut out = Vec::new();
    let mut rest = seg;
    while let Some(p) = rest.find('/') {
        let after = &rest[p + 1..];
        let name: String = after.chars().take_while(|c| !c.is_whitespace() && *c != '/' && *c != '<' && *c != '>').collect();
        let tail = &after[name.len()..];
        let mut it = tail.trim_start().split_whitespace();
        if let (Some(id), Some(gen), Some(r)) = (it.next(), it.next(), it.next()) {
            if r == "R" {
                if let Ok(idn) = id.parse::<u32>() {
                    let advance = p + 1 + name.len();
                    out.push((name, idn));
                    rest = &rest[advance..];
                    continue;
                }
            }
        }
        rest = &rest[p + 1..];
    }
    out
}

fn extract_text(f: &PdfFile, pages: &[u32]) -> (String, String) {
    let mut out = String::new();
    let mut decoded_gids = 0usize;
    let mut raw_ascii = 0usize;
    for pid in pages {
        let fonts = page_fonts(f, *pid);
        let Some(p) = f.objs.get(pid) else { continue };
        let mut content_ids = Vec::new();
        if let Some((cid, _)) = parse_ref(&p.dict, "/Contents") {
            content_ids.push(cid);
        } else if let Some(arr) = extract_array(&p.dict, "/Contents") {
            content_ids.extend(refs_in(&arr).into_iter().map(|(id, _)| id));
        }
        for cid in content_ids {
            let Some(content) = f.stream_content(cid) else { continue };
            let s = String::from_utf8_lossy(&content).into_owned();
            let mut cur_font: HashMap<u32, String> = HashMap::new();
            let mut i = 0;
            let bytes = s.as_bytes();
            while i < bytes.len() {
                match bytes[i] {
                    b'/' => {
                        // font switch: /F0 10 Tf
                        let name_start = i + 1;
                        let mut j = name_start;
                        while j < bytes.len() && (bytes[j].is_ascii_alphanumeric() || bytes[j] == b'_') {
                            j += 1;
                        }
                        let name = &s[name_start..j];
                        let tail = &s[j..];
                        let mut it = tail.trim_start().split_whitespace();
                        let sz = it.next().unwrap_or("");
                        let op = it.next().unwrap_or("");
                        if op == "Tf" {
                            if let Some(m) = fonts.get(name) {
                                cur_font = m.clone();
                            }
                        }
                        let _ = sz;
                        i = j;
                    }
                    b'(' => {
                        // literal string … find the matching close
                        let (txt, end) = read_literal(&s, i);
                        if !txt.is_empty() {
                            out.push_str(&txt);
                            raw_ascii += 1;
                        }
                        i = end;
                    }
                    b'<' if i + 1 < bytes.len() && bytes[i + 1] != b'<' => {
                        // hex string (CID string for Identity-H fonts)
                        let close = match s[i..].find('>') {
                            Some(c) => i + c,
                            None => break,
                        };
                        let hex: String = s[i + 1..close].chars().filter(|c| c.is_ascii_hexdigit()).collect();
                        let bytes_hex: Vec<u8> = (0..hex.len() / 2).filter_map(|k| u8::from_str_radix(&hex[k * 2..k * 2 + 2], 16).ok()).collect();
                        if !cur_font.is_empty() {
                            // 2-byte GIDs → ToUnicode
                            for chunk in bytes_hex.chunks(2) {
                                let hi = chunk[0];
                                let lo = chunk.get(1).copied().unwrap_or(0);
                                let gid = u16::from_be_bytes([hi, lo]) as u32;
                                if let Some(t) = cur_font.get(&gid) {
                                    out.push_str(t);
                                    decoded_gids += 1;
                                }
                            }
                        } else if let Some(t) = decode_utf16be(&bytes_hex) {
                            out.push_str(&t);
                        }
                        i = close + 1;
                    }
                    b'T' | b'\n' | b'\r' => {
                        if bytes[i] == b'T' && i + 1 < bytes.len() && (bytes[i + 1] == b'd' || bytes[i + 1] == b'D' || bytes[i + 1] == b'*' || bytes[i + 1] == b'J') {
                            out.push(' ');
                        }
                        i += 1;
                    }
                    _ => i += 1,
                }
            }
            out.push('\n');
        }
        out.push('\n');
    }
    let note = if decoded_gids > 0 {
        format!("extracted via ToUnicode CMaps ({} glyph runs, {} ascii runs) — best effort", decoded_gids, raw_ascii)
    } else if raw_ascii > 0 {
        format!("extracted raw ascii runs ({}) — fonts without ToUnicode maps may be missing", raw_ascii)
    } else {
        "no text runs decoded (scanned image PDFs and exotic encodings are not supported by the best-effort extractor)".into()
    };
    (collapse_ws(&out), note)
}

fn read_literal(s: &str, start: usize) -> (String, usize) {
    let b = s.as_bytes();
    let mut i = start + 1;
    let mut out = String::new();
    let mut depth = 1;
    let mut esc = false;
    while i < b.len() {
        let c = b[i] as char;
        if esc {
            match c {
                'n' => out.push('\n'),
                'r' => out.push('\r'),
                't' => out.push('\t'),
                'b' => out.push('\u{8}'),
                'f' => out.push('\u{c}'),
                '\\' => out.push('\\'),
                '(' => out.push('('),
                ')' => out.push(')'),
                d if d.is_ascii_digit() => {
                    // octal escape up to 3 digits
                    let mut oct = String::new();
                    let mut j = i;
                    while j < b.len() && oct.len() < 3 && (b[j] as char).is_ascii_digit() {
                        oct.push(b[j] as char);
                        j += 1;
                    }
                    if let Ok(v) = u8::from_str_radix(&oct, 8) {
                        out.push(v as char);
                    }
                    i = j;
                    esc = false;
                    continue;
                }
                other => out.push(other),
            }
            esc = false;
            i += 1;
            continue;
        }
        match c {
            '\\' => esc = true,
            '(' => {
                depth += 1;
                out.push('(');
            }
            ')' => {
                depth -= 1;
                if depth == 0 {
                    return (out, i + 1);
                }
                out.push(')');
            }
            other => out.push(other),
        }
        i += 1;
    }
    (out, i)
}

fn collapse_ws(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut last_nl = false;
    let mut last_sp = false;
    for c in s.chars() {
        match c {
            '\n' => {
                if !last_nl {
                    out.push('\n');
                    last_nl = true;
                    last_sp = false;
                }
            }
            ' ' | '\t' | '\r' => {
                if !last_sp && !last_nl {
                    out.push(' ');
                    last_sp = true;
                }
            }
            other => {
                out.push(other);
                last_nl = false;
                last_sp = false;
            }
        }
    }
    out.trim().to_string()
}

// ── merge / split ───────────────────────────────────────────────────────────

/// Copy `root_id` plus everything reachable from its object graph into the
/// writer, remapping ids. Pass 1 collects the reachable set and reserves new
/// ids; pass 2 emits dicts with every `N G R` rewritten. Returns the new id
/// of the root object.
fn copy_graph(f: &PdfFile, root_id: u32, w: &mut MergeWriter, map: &mut HashMap<u32, usize>) -> Option<usize> {
    // pass 1: reachability
    let mut seen: HashSet<u32> = HashSet::new();
    let mut reach: Vec<u32> = Vec::new();
    let mut stack = vec![root_id];
    while let Some(id) = stack.pop() {
        if !seen.insert(id) {
            continue;
        }
        let o = f.objs.get(&id)?;
        reach.push(id);
        for (n, _) in refs_in(&o.dict) {
            if f.objs.contains_key(&n) && !seen.contains(&n) {
                stack.push(n);
            }
        }
    }
    // pass 2: reserve ids for everything, then emit with rewritten refs
    for id in &reach {
        let new_id = w.reserve();
        map.insert(*id, new_id);
    }
    for id in &reach {
        let o = f.objs.get(id)?;
        let new_id = map[id];
        let new_dict = rewrite_refs(&o.dict, map);
        match &o.stream {
            Some(raw) => {
                let content = f.stream_content(*id).unwrap_or_else(|| raw.clone());
                let filtered = {
                    let mut enc = flate2::write::ZlibEncoder::new(Vec::new(), flate2::Compression::fast());
                    let _ = enc.write_all(&content);
                    enc.finish().unwrap_or_else(|_| content.clone())
                };
                let (comp, filter) = if filtered.len() < content.len() {
                    (filtered, "/Filter /FlateDecode ")
                } else {
                    (content.clone(), "")
                };
                w.add_stream_reserved(new_id, new_dict, format!("{filter}/Length {}", comp.len()), comp);
            }
            None => {
                w.add_reserved(new_id, new_dict);
            }
        }
    }
    map.get(&root_id).copied()
}

/// Rewrite every `N G R` in a dict body to the mapped new id.
fn rewrite_refs(dict: &str, map: &HashMap<u32, usize>) -> String {
    let mut out = String::with_capacity(dict.len());
    let mut rest = dict;
    let mut last_end = 0usize;
    let bytes = dict.as_bytes();
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i].is_ascii_digit() {
            let start = i;
            while i < bytes.len() && bytes[i].is_ascii_digit() {
                i += 1;
            }
            let num_str = &dict[start..i];
            let mut j = i;
            while j < bytes.len() && bytes[j] == b' ' {
                j += 1;
            }
            let gstart = j;
            while j < bytes.len() && bytes[j].is_ascii_digit() {
                j += 1;
            }
            let gen_str = &dict[gstart..j];
            let mut k = j;
            while k < bytes.len() && bytes[k] == b' ' {
                k += 1;
            }
            if k < bytes.len() && bytes[k] == b'R' && (k + 1 >= bytes.len() || bytes[k + 1].is_ascii_whitespace()) && !gen_str.is_empty() {
                if let Ok(n) = num_str.parse::<u32>() {
                    if let Some(new_id) = map.get(&n) {
                        out.push_str(&dict[last_end..start]);
                        out.push_str(&format!("{new_id} 0 R"));
                        last_end = k + 1;
                    }
                }
            }
            i += 1;
        } else {
            i += 1;
        }
    }
    out.push_str(&dict[last_end.min(bytes.len())..]);
    let _ = rest;
    out
}

/// Simple sequential writer used by merge/split.
struct MergeWriter {
    objs: Vec<(String, Option<(String, Vec<u8>)>)>,
}

impl MergeWriter {
    fn new() -> Self {
        // id 1 = catalog, 2 = pages tree, both filled at the end
        Self { objs: Vec::new() }
    }
    fn reserve(&mut self) -> usize {
        self.objs.push((String::new(), None));
        self.objs.len()
    }
    fn add_reserved(&mut self, id: usize, dict: String) {
        let idx = id - 1;
        if idx < self.objs.len() {
            self.objs[idx].0 = dict;
        }
    }
    fn add_stream_reserved(&mut self, id: usize, dict: String, extra: String, data: Vec<u8>) {
        let idx = id - 1;
        if idx < self.objs.len() {
            self.objs[idx] = (dict, Some((extra, data)));
        }
    }
    fn serialize(&self, page_ids: &[usize]) -> Vec<u8> {
        let mut out = Vec::new();
        out.extend_from_slice(b"%PDF-1.7\n%\xe2\xe3\xcf\xd3\n");
        let mut offsets = Vec::new();
        for (i, (dict, stream)) in self.objs.iter().enumerate() {
            offsets.push(out.len() as u32);
            let id = i + 1;
            out.extend_from_slice(format!("{id} 0 obj\n").as_bytes());
            if let Some((extra, data)) = stream {
                let mut enc = flate2::write::ZlibEncoder::new(Vec::new(), flate2::Compression::fast());
                let _ = enc.write_all(data);
                let compressed = enc.finish().unwrap_or_else(|_| data.clone());
                let comp = if compressed.len() < data.len() { compressed } else { data.clone() };
                let filter = if comp.len() < data.len() { "/Filter /FlateDecode " } else { "" };
                out.extend_from_slice(format!("<<{dict} {extra} {filter}/Length {} >>\nstream\n", comp.len()).as_bytes());
                out.extend_from_slice(&comp);
                out.extend_from_slice(b"\nendstream\n");
            } else {
                out.extend_from_slice(format!("<<{dict} >>\n").as_bytes());
            }
            out.extend_from_slice(b"endobj\n");
        }
        let xref_off = out.len() as u32;
        let n = self.objs.len() + 1;
        out.extend_from_slice(format!("xref\n0 {n}\n").as_bytes());
        out.extend_from_slice(b"0000000000 65535 f \n");
        for off in &offsets {
            out.extend_from_slice(format!("{off:010} 00000 n \n").as_bytes());
        }
        let kids: String = page_ids.iter().map(|id| format!("{id} 0 R ")).collect();
        out.extend_from_slice(
            format!(
                "trailer\n<< /Size {n} /Root 1 0 R /Info 3 0 R >>\nstartxref\n{xref_off}\n%%EOF\n"
            )
            .as_bytes(),
        );
        let _ = kids;
        out
    }
}

/// Merge PDFs (bytes) into one; pages appear in input order.
pub fn merge(inputs: &[Vec<u8>]) -> Result<Vec<u8>, String> {
    let mut w = MergeWriter::new();
    w.reserve(); // 1 catalog
    w.reserve(); // 2 pages tree
    w.reserve(); // 3 info
    let mut page_ids = Vec::new();
    for (idx, data) in inputs.iter().enumerate() {
        let f = PdfFile::parse(data);
        let pages = f.pages();
        if pages.is_empty() {
            return Err(format!("input {}: no pages found (encrypted or unsupported structure)", idx + 1));
        }
        let mut map: HashMap<u32, usize> = HashMap::new();
        for p in pages {
            let new_pid = copy_graph(&f, p, &mut w, &mut map).ok_or("merge: page object missing")?;
            page_ids.push(new_pid);
        }
    }
    // info (id 3)
    let info = format!(
        "/Producer (RomaCode documents engine {}) /Title (Merged document)",
        crate::VERSION
    );
    w.add_reserved(3, info);
    // pages tree (id 2), catalog (id 1)
    let kids: String = page_ids.iter().map(|id| format!("{id} 0 R ")).collect();
    w.add_reserved(2, format!("/Type /Pages /Kids [ {kids}] /Count {}", page_ids.len()));
    w.add_reserved(1, "/Type /Catalog /Pages 2 0 R".into());
    Ok(w.serialize(&page_ids))
}

/// Extract a page range (1-based, inclusive) into a new PDF.
pub fn split(data: &[u8], from_page: usize, to_page: usize) -> Result<Vec<u8>, String> {
    let f = PdfFile::parse(data);
    let pages = f.pages();
    if pages.is_empty() {
        return Err("no pages found".into());
    }
    let from = from_page.max(1);
    let to = to_page.max(from).min(pages.len());
    let mut w = MergeWriter::new();
    w.reserve();
    w.reserve();
    w.reserve();
    let mut page_ids = Vec::new();
    for p in &pages[from - 1..to] {
        let mut map: HashMap<u32, usize> = HashMap::new();
        let new_pid = copy_graph(&f, *p, &mut w, &mut map).ok_or("split: page object missing")?;
        page_ids.push(new_pid);
    }
    let info = format!("/Producer (RomaCode documents engine {}) /Title (Extracted pages)", crate::VERSION);
    w.add_reserved(3, info);
    let kids: String = page_ids.iter().map(|id| format!("{id} 0 R ")).collect();
    w.add_reserved(2, format!("/Type /Pages /Kids [ {kids}] /Count {}", page_ids.len()));
    w.add_reserved(1, "/Type /Catalog /Pages 2 0 R".into());
    Ok(w.serialize(&page_ids))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::pdfgen::{build_document, Align, Block, Meta, PageSize};
    use crate::color::Rgba;

    fn sample() -> Vec<u8> {
        let meta = Meta { title: "Sample".into(), author: "Ada".into(), subject: "test".into() };
        build_document(
            &meta,
            &[
                Block::Heading { level: 1, text: "Introduction".into() },
                Block::Para { text: "The quick brown fox jumps over the lazy dog near the river bank at dawn.".into(), align: Align::Justify, size_pt: None, color: None, line_spacing: 1.2 },
                Block::PageBreak,
                Block::Para { text: "Second page content here.".into(), align: Align::Left, size_pt: None, color: None, line_spacing: 1.2 },
            ],
            PageSize::A4,
            56.0,
            false,
            Rgba::new(31, 119, 180),
        )
        .unwrap()
    }

    #[test]
    fn read_own_pdf_metadata_and_text() {
        let pdf = sample();
        let info = read(&pdf).unwrap();
        assert_eq!(info.page_count, 2);
        assert_eq!(info.title.as_deref(), Some("Sample"));
        assert!(info.text.contains("quick brown fox"), "text: {:?}", info.text);
        assert!(info.text.contains("Second page"));
    }

    #[test]
    fn merge_two_pdfs() {
        let a = sample();
        let b = sample();
        let merged = merge(&[a, b]).unwrap();
        let info = read(&merged).unwrap();
        assert_eq!(info.page_count, 4);
    }

    #[test]
    fn split_range() {
        let pdf = sample();
        let part = split(&pdf, 2, 2).unwrap();
        let info = read(&part).unwrap();
        assert_eq!(info.page_count, 1);
        assert!(info.text.contains("Second page"));
    }
}
