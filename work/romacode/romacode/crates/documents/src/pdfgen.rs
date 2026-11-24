//! PDF 1.7 writer — pure Rust, from the spec: paged documents, embedded
//! TrueType fonts (Identity-H encoding, so CJK works with system Noto fonts),
//! wrapped/justified text, tables with styled headers, images (JPEG
//! passthrough via DCTDecode, PNG decoded to RGB), colored bands, cover
//! pages and page numbers.

use crate::color::Rgba;
use crate::docx::civil_from_days;
use crate::fonts::{default_family, Face, TextMeter};
use crate::raster::decode_png;
use ab_glyph::{Font as _, ScaleFont as _};
use serde_json::Value;
use std::collections::BTreeMap;
use std::io::Write;

// ── low-level object writer ─────────────────────────────────────────────────

struct Obj {
    dict: String,
    /// `Some((stream_bytes, extra_stream_dict_entries))`
    stream: Option<(Vec<u8>, String)>,
}

struct ObjWriter {
    objs: Vec<Obj>,
}

impl ObjWriter {
    fn new() -> Self {
        Self { objs: Vec::new() }
    }

    fn add(&mut self, dict: String) -> usize {
        self.objs.push(Obj { dict, stream: None });
        self.objs.len() // id = index (1-based)
    }

    fn add_stream(&mut self, dict: String, extra: String, data: Vec<u8>) -> usize {
        self.objs.push(Obj { dict, stream: Some((data, extra)) });
        self.objs.len()
    }

    fn serialize(&self) -> Vec<u8> {
        let mut out = Vec::new();
        out.extend_from_slice(b"%PDF-1.7\n%\xc2\xa5\xc2\xb1\xc3\xab\n");
        let mut offsets = Vec::with_capacity(self.objs.len() + 1);
        for (i, o) in self.objs.iter().enumerate() {
            offsets.push(out.len() as u32);
            let id = i + 1;
            out.extend_from_slice(format!("{id} 0 obj\n").as_bytes());
            if let Some((data, extra)) = &o.stream {
                let mut enc = flate2::write::ZlibEncoder::new(Vec::new(), flate2::Compression::fast());
                let _ = enc.write_all(data);
                let compressed = enc.finish().unwrap_or_else(|_| data.clone());
                let comp = if compressed.len() < data.len() { compressed } else { data.clone() };
                let filter = if comp.len() < data.len() { "/Filter /FlateDecode " } else { "" };
                out.extend_from_slice(
                    format!("<<{} {extra} {filter}/Length {} >>\nstream\n", o.dict, comp.len()).as_bytes(),
                );
                out.extend_from_slice(&comp);
                out.extend_from_slice(b"\nendstream\n");
            } else {
                out.extend_from_slice(format!("<<{} >>\n", o.dict).as_bytes());
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
        // Info object id == objs.len() (it is the last object; ids are 1-based).
        let info_id = self.objs.len();
        out.extend_from_slice(
            format!("trailer\n<< /Size {n} /Root 1 0 R /Info {info_id} 0 R >>\nstartxref\n{xref_off}\n%%EOF\n").as_bytes(),
        );
        out
    }
}

fn esc_pdf_string(s: &str) -> String {
    let mut out = String::new();
    for c in s.chars() {
        match c {
            '(' => out.push_str("\\("),
            ')' => out.push_str("\\)"),
            '\\' => out.push_str("\\\\"),
            _ => out.push(c),
        }
    }
    out
}

/// PDF text string as UTF-16BE hex (safe for any Unicode in the Info dict).
fn pdf_text_string(s: &str) -> String {
    let mut hex = String::from("FEFF");
    for unit in s.encode_utf16() {
        hex.push_str(&format!("{unit:04X}"));
    }
    format!("<{hex}>")
}

fn num(f: f64) -> String {
    if (f - f.round()).abs() < 0.005 {
        format!("{}", f.round() as i64)
    } else {
        let s = format!("{f:.2}");
        s.trim_end_matches('0').trim_end_matches('.').to_string()
    }
}

// ── embedded font ───────────────────────────────────────────────────────────

/// A TTF font embedded with Identity-H. GIDs are collected on first use and
/// emitted as a /W array (PDF metric model: no subsetting, full font file).
struct EmbeddedFont {
    face: Option<Face>,
    /// Font file bytes for the /FontFile2 stream (None = stub without font).
    data: Option<Vec<u8>>,
    units_per_em: f32,
    widths: BTreeMap<u32, i64>, // gid → advance in 1000-unit text space
    /// gid → unicode (for the ToUnicode CMap, so text extraction works).
    used: BTreeMap<u32, char>,
}

impl EmbeddedFont {
    fn load(path: &std::path::Path) -> Option<Self> {
        let data = std::fs::read(path).ok()?;
        let face = Face::load(path).ok()?;
        let upem = face.font.units_per_em().unwrap_or(1000.0);
        Some(Self { face: Some(face), data: Some(data), units_per_em: upem, widths: BTreeMap::new(), used: BTreeMap::new() })
    }

    fn stub() -> Self {
        Self { face: None, data: None, units_per_em: 1000.0, widths: BTreeMap::new(), used: BTreeMap::new() }
    }

    fn ok(&self) -> bool {
        self.face.is_some()
    }

    fn gid_for(&mut self, ch: char) -> u32 {
        let (gid, w) = match &self.face {
            Some(face) => {
                let scaled = face.font.as_scaled(self.units_per_em);
                let g16 = scaled.glyph_id(ch);
                let gid = g16.0 as u32;
                let adv = scaled.h_advance(g16);
                let w = ((adv as f64) * 1000.0 / self.units_per_em as f64).round() as i64;
                (gid, w)
            }
            None => (0, 500),
        };
        self.widths.entry(gid).or_insert(w);
        self.used.entry(gid).or_insert(ch);
        gid
    }

    /// Advance of a char in text space at the given pt size.
    fn advance(&mut self, ch: char, pt: f64) -> f64 {
        let gid = self.gid_for(ch);
        let w = self.widths[&gid] as f64;
        w / 1000.0 * pt
    }

    /// Render a string as a PDF hex CID string + advance (pt).
    fn encode(&mut self, s: &str, pt: f64) -> (String, f64) {
        let mut hex = String::new();
        let mut adv = 0.0;
        for ch in s.chars() {
            let gid = self.gid_for(ch);
            hex.push_str(&format!("{gid:04X}"));
            adv += self.widths[&gid] as f64 / 1000.0 * pt;
        }
        (format!("<{hex}>"), adv)
    }
}

// ── document builder ────────────────────────────────────────────────────────

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum PageSize {
    A4,
    Letter,
    Custom(f64, f64),
}

impl PageSize {
    fn dims(self) -> (f64, f64) {
        match self {
            PageSize::A4 => (595.28, 841.89),
            PageSize::Letter => (612.0, 792.0),
            PageSize::Custom(w, h) => (w.max(72.0), h.max(72.0)),
        }
    }
    pub fn parse(s: &str) -> Option<Self> {
        match s.to_ascii_lowercase().as_str() {
            "a4" => Some(Self::A4),
            "letter" => Some(Self::Letter),
            _ => None,
        }
    }
}

#[derive(Clone, Debug)]
pub enum Block {
    Heading { level: u8, text: String },
    Para { text: String, align: Align, size_pt: Option<f64>, color: Option<String>, line_spacing: f64 },
    Bullets { items: Vec<String> },
    Numbered { items: Vec<String> },
    Table { header: Vec<String>, rows: Vec<Vec<String>>, widths: Option<Vec<f64>> },
    Image { path: String, width_pt: Option<f64>, caption: Option<String> },
    PageBreak,
    Spacer { pt: f64 },
    Rule { color: Option<String> },
    Cover { title: String, subtitle: String, author: String, date: String, accent: String },
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Align {
    Left,
    Center,
    Right,
    Justify,
}

pub struct Meta {
    pub title: String,
    pub author: String,
    pub subject: String,
}

enum ImgKind {
    Jpeg { data: Vec<u8>, px_w: u32, px_h: u32 },
    Rgb { data: Vec<u8>, px_w: u32, px_h: u32 },
}

struct Img {
    kind: ImgKind,
}

impl Img {
    fn px_w(&self) -> u32 {
        match &self.kind {
            ImgKind::Jpeg { px_w, .. } | ImgKind::Rgb { px_w, .. } => *px_w,
        }
    }
    fn px_h(&self) -> u32 {
        match &self.kind {
            ImgKind::Jpeg { px_h, .. } | ImgKind::Rgb { px_h, .. } => *px_h,
        }
    }
}

/// Incremental PDF builder: appends blocks, flows content across pages.
pub struct Builder {
    page_w: f64,
    page_h: f64,
    margin: f64,
    y: f64,
    pages: Vec<String>, // content ops per page
    cur: String,        // current page ops
    images: Vec<Img>,
    font: EmbeddedFont,
    meta: Meta,
    page_numbers: bool,
    page_counter: usize,
    accent: Rgba,
}

impl Builder {
    pub fn new(page: PageSize, margin: f64, meta: Meta, page_numbers: bool, accent: Rgba) -> Self {
        let (w, h) = page.dims();
        let font = default_family()
            .and_then(|fp| fp.regular.to_str().and_then(|p| EmbeddedFont::load(std::path::Path::new(p))))
            .unwrap_or_else(EmbeddedFont::stub);
        Self {
            page_w: w,
            page_h: h,
            margin,
            y: h - margin,
            pages: Vec::new(),
            cur: String::new(),
            images: Vec::new(),
            font,
            meta,
            page_numbers,
            page_counter: 1,
            accent,
        }
    }

    fn content_width(&self) -> f64 {
        self.page_w - 2.0 * self.margin
    }

    fn advance_of(&mut self, s: &str, pt: f64) -> f64 {
        let mut w = 0.0;
        for ch in s.chars() {
            w += self.font.advance(ch, pt);
        }
        w
    }

    fn draw_text_at(&mut self, x: f64, y: f64, text: &str, pt: f64, color: Rgba) {
        let (cid, _) = self.font.encode(text, pt);
        self.cur.push_str(&format!(
            "BT {r} {g} {b} rg /F0 {pt} Tf {x} {y} Td {cid} Tj ET\n",
            r = num(color.r as f64 / 255.0),
            g = num(color.g as f64 / 255.0),
            b = num(color.b as f64 / 255.0),
            pt = num(pt),
            x = num(x),
            y = num(y),
        ));
    }

    /// Draw wrapped (and optionally justified) text; returns height used.
    fn draw_para(&mut self, text: &str, x: f64, width: f64, pt: f64, color: Rgba, align: Align, line_spacing: f64) -> f64 {
        if self.font.face.is_none() {
            return 0.0;
        }
        // Scope the font borrow: wrap + metrics first, then draw.
        let (lines, ascent, descent) = {
            let f = &mut self.font;
            let meter = TextMeter::new(f.face.as_ref().unwrap());
            let lines = meter.wrap(text, pt as f32, width as f32);
            let (a, d) = match &f.face {
                Some(face) => (face.ascent(pt as f32) as f64, face.descent(pt as f32).abs() as f64),
                None => (pt * 0.8, pt * 0.2),
            };
            (lines, a, d)
        };
        let lh = (ascent + descent) * line_spacing;
        for line in &lines {
            self.ensure_space(lh);
            let lw = self.advance_of(line, pt);
            let tx = match align {
                Align::Left | Align::Justify => x,
                Align::Center => x + (width - lw) / 2.0,
                Align::Right => x + width - lw,
            };
            self.draw_text_at(tx, self.y - ascent, line, pt, color);
            self.y -= lh;
        }
        lines.len() as f64 * lh
    }

    fn ensure_space(&mut self, needed: f64) {
        if self.y - needed < self.margin {
            self.new_page();
        }
    }

    fn new_page(&mut self) {
        self.pages.push(std::mem::take(&mut self.cur));
        self.y = self.page_h - self.margin;
        self.page_counter += 1;
    }

    // ── blocks ──────────────────────────────────────────────────────────────

    pub fn add(&mut self, block: Block) -> Result<(), String> {
        match block {
            Block::Heading { level, text } => {
                let pt = match level {
                    1 => 22.0,
                    2 => 17.0,
                    3 => 14.0,
                    _ => 12.5,
                };
                let color = match level {
                    1 | 2 => self.accent,
                    _ => Rgba::new(60, 60, 60),
                };
                self.ensure_space(pt * 2.5);
                self.y -= if level == 1 { 14.0 } else { 8.0 };
                self.draw_para(&text, self.margin, self.content_width(), pt, color, Align::Left, 1.1);
                if level == 1 {
                    self.cur.push_str(&format!(
                        "{x} {y1} m {x2} {y1} l 1.2 w {r} {g} {b} RG S\n",
                        x = num(self.margin),
                        x2 = num(self.margin + self.content_width()),
                        y1 = num(self.y + 4.0),
                        r = num(self.accent.r as f64 / 255.0),
                        g = num(self.accent.g as f64 / 255.0),
                        b = num(self.accent.b as f64 / 255.0),
                    ));
                }
                self.y -= 6.0;
            }
            Block::Para { text, align, size_pt, color, line_spacing } => {
                let pt = size_pt.unwrap_or(10.5);
                let c = color.map(|s| Rgba::parse_or(&s, Rgba::new(20, 20, 20))).unwrap_or(Rgba::new(20, 20, 20));
                self.draw_para(&text, self.margin, self.content_width(), pt, c, align, line_spacing.max(0.8));
                self.y -= 6.0;
            }
            Block::Bullets { items } => {
                for it in items {
                    self.ensure_space(16.0);
                    self.draw_text_at(self.margin + 4.0, self.y - 9.0, "\u{2022}", 10.0, self.accent);
                    self.y -= 2.0;
                    self.draw_para(&it, self.margin + 18.0, self.content_width() - 18.0, 10.5, Rgba::new(20, 20, 20), Align::Left, 1.15);
                    self.y -= 3.0;
                }
            }
            Block::Numbered { items } => {
                for (i, it) in items.iter().enumerate() {
                    self.ensure_space(16.0);
                    self.draw_text_at(self.margin + 2.0, self.y - 9.0, &format!("{}.", i + 1), 10.0, self.accent);
                    self.y -= 2.0;
                    self.draw_para(it, self.margin + 22.0, self.content_width() - 22.0, 10.5, Rgba::new(20, 20, 20), Align::Left, 1.15);
                    self.y -= 3.0;
                }
            }
            Block::Table { header, rows, widths } => {
                self.draw_table(&header, &rows, widths.as_deref())?;
            }
            Block::Image { path, width_pt, caption } => {
                let img = self.load_image(&path)?;
                let (pw, ph) = (img.px_w(), img.px_h());
                let w = width_pt.unwrap_or(self.content_width().min(430.0));
                let h = w * ph as f64 / pw.max(1) as f64;
                self.ensure_space(h + 24.0);
                let x = self.margin + (self.content_width() - w) / 2.0;
                let id = self.images.len(); // /Im<i> name
                self.cur.push_str(&format!(
                    "q {w} 0 0 {h} {x} {y} cm /Im{id} Do Q\n",
                    w = num(w),
                    h = num(h),
                    x = num(x),
                    y = num(self.y - h),
                ));
                self.y -= h + 6.0;
                if let Some(cap) = caption {
                    self.draw_para(&cap, self.margin, self.content_width(), 8.5, Rgba::new(110, 110, 110), Align::Center, 1.1);
                }
                self.y -= 6.0;
            }
            Block::PageBreak => self.new_page(),
            Block::Spacer { pt } => self.y -= pt,
            Block::Rule { color } => {
                let c = color.map(|s| Rgba::parse_or(&s, self.accent)).unwrap_or(self.accent);
                self.ensure_space(14.0);
                self.cur.push_str(&format!(
                    "{x} {y} m {x2} {y} l 0.8 w {r} {g} {b} RG S\n",
                    x = num(self.margin),
                    x2 = num(self.margin + self.content_width()),
                    y = num(self.y - 6.0),
                    r = num(c.r as f64 / 255.0),
                    g = num(c.g as f64 / 255.0),
                    b = num(c.b as f64 / 255.0),
                ));
                self.y -= 14.0;
            }
            Block::Cover { title, subtitle, author, date, accent } => {
                let c = Rgba::parse_or(&accent, self.accent);
                let band_h = self.page_h * 0.18;
                self.cur.push_str(&format!(
                    "{r} {g} {b} rg 0 {bh} {pw} {bh2} re f\n",
                    r = num(c.r as f64 / 255.0),
                    g = num(c.g as f64 / 255.0),
                    b = num(c.b as f64 / 255.0),
                    bh = num(self.page_h - band_h),
                    bh2 = num(band_h),
                    pw = num(self.page_w)
                ));
                let cw = self.content_width();
                let (a34, a16, a11) = self.face_metrics(34.0, 16.0, 11.0);
                let tw = self.advance_of(&title, 34.0);
                self.draw_text_at(self.margin + (cw - tw) / 2.0, self.page_h * 0.52 - a34, &title, 34.0, Rgba::new(25, 30, 40));
                if !subtitle.is_empty() {
                    let s_tw = self.advance_of(&subtitle, 16.0);
                    self.draw_text_at(self.margin + (cw - s_tw) / 2.0, self.page_h * 0.52 - 64.0 - a16, &subtitle, 16.0, Rgba::new(95, 100, 110));
                }
                let foot = [author, date].iter().filter(|s| !s.is_empty()).cloned().collect::<Vec<_>>().join("  \u{00b7}  ");
                if !foot.is_empty() {
                    let f_tw = self.advance_of(&foot, 11.0);
                    self.draw_text_at(self.margin + (cw - f_tw) / 2.0, self.margin + 34.0 - a11, &foot, 11.0, Rgba::new(120, 125, 135));
                }
                self.new_page();
            }
        }
        Ok(())
    }

    /// (ascent at s1, ascent at s2, ascent at s3)
    fn face_metrics(&mut self, s1: f64, s2: f64, s3: f64) -> (f64, f64, f64) {
        match &self.font.face {
            Some(face) => (face.ascent(s1 as f32) as f64, face.ascent(s2 as f32) as f64, face.ascent(s3 as f32) as f64),
            None => (s1 * 0.8, s2 * 0.8, s3 * 0.8),
        }
    }

    /// Wrap one row's cells (scoped borrow, returns owned lines per column).
    fn wrap_row(&mut self, row: &[String], col_w: &[f64], font_pt: f64) -> Vec<Vec<String>> {
        if self.font.face.is_none() {
            return vec![vec![String::new()]; col_w.len()];
        }
        let width = self.content_width();
        let cols = col_w.len();
        let text = row.to_vec();
        let f = &mut self.font;
        let meter = TextMeter::new(f.face.as_ref().unwrap());
        (0..cols)
            .map(|i| {
                let cell = text.get(i).cloned().unwrap_or_default();
                meter.wrap(&cell, font_pt as f32, (width.min(col_w[i]) - 10.0).max(12.0) as f32)
            })
            .collect()
    }

    fn draw_table(&mut self, header: &[String], rows: &[Vec<String>], widths: Option<&[f64]>) -> Result<(), String> {
        if self.font.face.is_none() {
            return Err("no system font available for PDF text — set RC_DOCS_FONT to a .ttf/.otf file".into());
        }
        let cols = header.len().max(rows.iter().map(|r| r.len()).max().unwrap_or(0));
        if cols == 0 {
            return Ok(());
        }
        let pad = 5.0;
        let cw = self.content_width();
        let col_w: Vec<f64> = match widths {
            Some(w) if !w.is_empty() => {
                let sum: f64 = w.iter().sum();
                (0..cols).map(|i| cw * w.get(i).copied().unwrap_or(0.0) / sum.max(0.0001)).collect()
            }
            _ => vec![cw / cols as f64; cols],
        };
        let font_pt = 9.0;
        let ascent = match &self.font.face {
            Some(face) => face.ascent(font_pt as f32) as f64,
            None => font_pt as f64 * 0.8,
        };
        let lh = ascent * 1.9;
        let accent = self.accent;

        if !header.is_empty() {
            let cells = self.wrap_row(header, &col_w, font_pt);
            self.render_row(&cells, &col_w, lh, ascent, pad, font_pt, true, accent)?;
        }
        for (ri, row) in rows.iter().enumerate() {
            let cells = self.wrap_row(row, &col_w, font_pt);
            self.render_row(&cells, &col_w, lh, ascent, pad, font_pt, false, if ri % 2 == 1 { Rgba::new(242, 246, 251) } else { Rgba::new(255, 255, 255) })?;
        }
        self.y -= 10.0;
        Ok(())
    }

    #[allow(clippy::too_many_arguments)]
    fn render_row(&mut self, cells: &[Vec<String>], col_w: &[f64], lh: f64, ascent: f64, pad: f64, font_pt: f64, header_row: bool, fill: Rgba) -> Result<(), String> {
        let row_h = cells.iter().map(|c| c.len().max(1) as f64 * lh).fold(0.0f64, f64::max) + 2.0 * pad;
        self.ensure_space(row_h + 4.0);
        let top = self.y;
        let mut x = self.margin;
        if !header_row || fill != Rgba::new(255, 255, 255) {
            self.cur.push_str(&format!(
                "{r} {g} {b} rg {x} {y} {w} {h} re f\n",
                r = num(fill.r as f64 / 255.0),
                g = num(fill.g as f64 / 255.0),
                b = num(fill.b as f64 / 255.0),
                x = num(x),
                y = num(top - row_h),
                w = num(col_w.iter().sum::<f64>()),
                h = num(row_h)
            ));
        }
        let color = if header_row { Rgba::new(255, 255, 255) } else { Rgba::new(30, 30, 30) };
        for (i, lines) in cells.iter().enumerate() {
            let mut yy = top - pad - ascent;
            for line in lines {
                self.draw_text_at(x + pad, yy, line, font_pt, color);
                yy -= lh;
            }
            x += col_w[i];
        }
        // grid lines
        self.cur.push_str("0.72 0.72 0.72 RG 0.6 w\n");
        let mut xx = self.margin;
        for w in col_w.iter() {
            self.cur.push_str(&format!("{x} {y1} m {x} {y2} l S\n", x = num(xx), y1 = num(top), y2 = num(top - row_h)));
            xx += w;
        }
        self.cur.push_str(&format!("{x} {y1} m {x} {y2} l S\n", x = num(xx), y1 = num(top), y2 = num(top - row_h)));
        self.cur.push_str(&format!("{x1} {y} m {x2} {y} l S\n", x1 = num(self.margin), x2 = num(self.margin + col_w.iter().sum::<f64>()), y = num(top)));
        self.cur.push_str(&format!("{x1} {y} m {x2} {y} l S\n", x1 = num(self.margin), x2 = num(self.margin + col_w.iter().sum::<f64>()), y = num(top - row_h)));
        self.y = top - row_h;
        Ok(())
    }

    fn load_image(&mut self, path: &str) -> Result<Img, String> {
        let data = std::fs::read(path).map_err(|e| format!("image {path}: {e}"))?;
        let lower = path.to_ascii_lowercase();
        if lower.ends_with(".jpg") || lower.ends_with(".jpeg") {
            let (w, h) = jpeg_dims(&data).ok_or("jpeg: cannot read dimensions")?;
            Ok(Img { kind: ImgKind::Jpeg { data, px_w: w, px_h: h } })
        } else if lower.ends_with(".png") {
            let surf = decode_png(&data)?;
            let mut rgb = Vec::with_capacity((surf.w * surf.h * 3) as usize);
            for px in surf.px.chunks_exact(4) {
                let a = px[3] as u32;
                let mix = |fg: u8, bg: u8| ((fg as u32 * a + bg as u32 * (255 - a)) / 255) as u8;
                rgb.extend_from_slice(&[mix(px[0], 255), mix(px[1], 255), mix(px[2], 255)]);
            }
            Ok(Img { kind: ImgKind::Rgb { data: rgb, px_w: surf.w, px_h: surf.h } })
        } else {
            Err("unsupported image format for PDF (use png/jpg)".into())
        }
    }

    /// Finish: serialize the document.
    pub fn finish(mut self) -> Result<Vec<u8>, String> {
        if !self.font.ok() {
            return Err("no system font available for PDF text — set RC_DOCS_FONT to a .ttf/.otf file (PDF needs real glyph metrics)".into());
        }
        self.pages.push(std::mem::take(&mut self.cur));

        let mut w = ObjWriter::new();
        w.add(String::new()); // 1: catalog (filled below)
        w.add(String::new()); // 2: pages tree (filled below)

        // page number footers (ops computed first, then appended)
        let total = self.pages.len();
        let mut footer_ops = Vec::with_capacity(total);
        if self.page_numbers {
            for i in 0..total {
                let label = format!("Page {} of {}", i + 1, total);
                let tw = self.advance_of(&label, 8.5);
                let (cid, _) = self.font.encode(&label, 8.5);
                footer_ops.push(format!(
                    "BT 0.45 0.45 0.45 rg /F0 8.5 Tf {x} {y} Td {cid} Tj ET\n",
                    x = num((self.page_w - tw) / 2.0),
                    y = num(self.margin / 2.0),
                ));
            }
        }
        for (i, ops) in self.pages.iter_mut().enumerate() {
            if self.page_numbers {
                ops.push_str(&footer_ops[i]);
            }
        }

        // content streams + page objects (content id = page id - 1)
        let mut page_ids = Vec::new();
        for ops in &self.pages {
            let content_id = w.add_stream(String::new(), String::new(), ops.as_bytes().to_vec());
            let id = w.add(format!(
                "/Type /Page /Parent 2 0 R /MediaBox [0 0 {pw} {ph}] /Contents {content_id} 0 R",
                pw = num(self.page_w),
                ph = num(self.page_h)
            ));
            page_ids.push(id);
        }

        // font: Type0 + CIDFontType2 + descriptor + FontFile2 + ToUnicode
        let mut font_res = String::new();
        {
            let used: Vec<u32> = self.font.widths.keys().copied().collect();
            let mut warr = String::new();
            for gid in &used {
                warr.push_str(&format!("{} [{} ] ", gid, self.font.widths[gid]));
            }
            // ToUnicode CMap: gid → UTF-16BE (makes pdf_text work on PDFs
            // this engine produced, and text-copying in viewers).
            let mut cmap = String::from(
                "/CIDInit /ProcSet findresource begin\n12 dict begin\nbegincmap\n/CMapName /RcDocs-ToUnicode def\n/CMapType 2 def\n1 begincodespacerange\n<0000> <FFFF>\nendcodespacerange\n",
            );
            let entries: Vec<(u32, char)> = self.font.used.iter().map(|(g, c)| (*g, *c)).collect();
            for chunk in entries.chunks(100) {
                cmap.push_str(&format!("{} beginbfchar\n", chunk.len()));
                for (gid, ch) in chunk {
                    let mut buf = [0u16; 2];
                    let n = 2 - (*ch).encode_utf16(&mut buf).len();
                    let mut u = String::new();
                    for unit in &buf[..n] {
                        u.push_str(&format!("{unit:04X}"));
                    }
                    cmap.push_str(&format!("<{gid:04X}> <{u}>\n"));
                }
                cmap.push_str("endbfchar\n");
            }
            cmap.push_str("endcmap\nCMapName currentdict /CMap defineresource pop\nend\nend\n");
            let tou_id = w.add_stream(String::new(), String::new(), cmap.into_bytes());
            let data = self.font.data.clone().unwrap_or_default();
            let file_id = w.add_stream("/Type /FontFile2".into(), format!("/Length1 {}", data.len()), data);
            let desc_id = w.add(format!(
                "/Type /FontDescriptor /FontName /RcDocsSans /Flags 32 /FontBBox [-1000 -300 2000 1200] /ItalicAngle 0 /Ascent 1000 /Descent -300 /CapHeight 700 /StemV 80 /FontFile2 {file_id} 0 R"
            ));
            let cid_id = w.add(format!(
                "/Type /Font /Subtype /CIDFontType2 /BaseFont /RcDocsSans /CIDSystemInfo << /Registry (Adobe) /Ordering (Identity) /Supplement 0 >> /FontDescriptor {desc_id} 0 R /DW 1000 /W [ {warr} ] /CIDToGIDMap /Identity"
            ));
            let type0_id = w.add(format!(
                "/Type /Font /Subtype /Type0 /BaseFont /RcDocsSans /Encoding /Identity-H /DescendantFonts [ {cid_id} 0 R ] /ToUnicode {tou_id} 0 R"
            ));
            font_res.push_str(&format!("/F0 {type0_id} 0 R "));
        }

        // images
        let mut img_res = String::new();
        for (idx, img) in self.images.iter().enumerate() {
            let (dict, data) = match &img.kind {
                ImgKind::Jpeg { data, px_w, px_h } => (
                    format!("/Type /XObject /Subtype /Image /Width {px_w} /Height {px_h} /ColorSpace /DeviceRGB /BitsPerComponent 8 /Filter /DCTDecode"),
                    data.clone(),
                ),
                ImgKind::Rgb { data, px_w, px_h } => (
                    format!("/Type /XObject /Subtype /Image /Width {px_w} /Height {px_h} /ColorSpace /DeviceRGB /BitsPerComponent 8"),
                    data.clone(),
                ),
            };
            let id = w.add_stream(dict, String::new(), data);
            img_res.push_str(&format!("/Im{idx} {id} 0 R "));
        }

        // page resources
        for pid in &page_ids {
            let obj = &mut w.objs[*pid - 1];
            let content_id = pid - 1;
            obj.dict = format!(
                "/Type /Page /Parent 2 0 R /MediaBox [0 0 {pw} {ph}] /Resources << /Font << {font_res}>> {imgs} >> /Contents {content_id} 0 R",
                pw = num(self.page_w),
                ph = num(self.page_h),
                imgs = if img_res.is_empty() { String::new() } else { format!("/XObject << {img_res}>>") },
            );
        }

        // info (must be the LAST object — serialize() computes its id)
        w.add(format!(
            "/Title {} /Author {} /Subject {} /Producer (RomaCode documents engine {}) /Creator (RomaCode)",
            pdf_text_string(&self.meta.title),
            pdf_text_string(&self.meta.author),
            pdf_text_string(&self.meta.subject),
            crate::VERSION,
        ));

        let kids: String = page_ids.iter().map(|id| format!("{id} 0 R ")).collect();
        w.objs[1].dict = format!("/Type /Pages /Kids [ {kids}] /Count {}", page_ids.len());
        w.objs[0].dict = "/Type /Catalog /Pages 2 0 R".into();
        Ok(w.serialize())
    }
}

pub(crate) fn jpeg_dims(data: &[u8]) -> Option<(u32, u32)> {
    if data.len() < 4 || data[0..2] != [0xFF, 0xD8] {
        return None;
    }
    let mut i = 2;
    while i + 9 <= data.len() {
        if data[i] != 0xFF {
            i += 1;
            continue;
        }
        let marker = data[i + 1];
        if (0xC0..=0xCF).contains(&marker) && ![0xC4, 0xC8, 0xCC].contains(&marker) {
            let h = u16::from_be_bytes([data[i + 5], data[i + 6]]) as u32;
            let w = u16::from_be_bytes([data[i + 7], data[i + 8]]) as u32;
            return Some((w, h));
        }
        let seglen = u16::from_be_bytes([data[i + 2], data[i + 3]]) as usize;
        i += 2 + seglen.max(2);
    }
    None
}

fn pdf_date() -> String {
    let secs = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0);
    let (y, m, d) = civil_from_days((secs / 86400) as i64);
    format!("{y:04}{m:02}{d:02}000000")
}

/// Build a PDF from the JSON block model used by the `pdf_create` tool.
pub fn blocks_from_json(v: &Value) -> Result<Vec<Block>, String> {
    let arr = v.as_array().ok_or("blocks must be an array")?;
    let mut out = Vec::new();
    for b in arr {
        let ty = b["type"].as_str().unwrap_or("");
        let g = |k: &str| b[k].as_str().unwrap_or("").to_string();
        match ty {
            "h1" | "h2" | "h3" | "h4" => out.push(Block::Heading { level: ty[1..].parse().unwrap_or(1), text: g("text") }),
            "p" | "para" | "paragraph" | "text" => out.push(Block::Para {
                text: g("text"),
                align: match b["align"].as_str().unwrap_or("justify") {
                    "left" => Align::Left,
                    "center" => Align::Center,
                    "right" => Align::Right,
                    _ => Align::Justify,
                },
                size_pt: b["size_pt"].as_f64(),
                color: b["color"].as_str().map(String::from),
                line_spacing: b["line_spacing"].as_f64().unwrap_or(1.25),
            }),
            "bullets" => out.push(Block::Bullets { items: str_list(&b["items"]) }),
            "numbered" => out.push(Block::Numbered { items: str_list(&b["items"]) }),
            "table" => out.push(Block::Table {
                header: str_list(&b["header"]),
                rows: b["rows"].as_array().map(|a| a.iter().map(|r| str_list(r)).collect()).unwrap_or_default(),
                widths: b["widths"].as_array().map(|a| a.iter().filter_map(|x| x.as_f64()).collect()),
            }),
            "image" => out.push(Block::Image { path: g("path"), width_pt: b["width_pt"].as_f64(), caption: b["caption"].as_str().map(String::from) }),
            "pagebreak" => out.push(Block::PageBreak),
            "spacer" => out.push(Block::Spacer { pt: b["pt"].as_f64().unwrap_or(12.0) }),
            "rule" => out.push(Block::Rule { color: b["color"].as_str().map(String::from) }),
            "cover" => out.push(Block::Cover { title: g("title"), subtitle: g("subtitle"), author: g("author"), date: g("date"), accent: g("accent") }),
            other => return Err(format!("unknown pdf block type {other:?}")),
        }
    }
    Ok(out)
}

fn str_list(v: &Value) -> Vec<String> {
    v.as_array().map(|a| a.iter().filter_map(|x| x.as_str().map(String::from)).collect()).unwrap_or_default()
}

/// One-shot build used by the tool.
pub fn build_document(
    meta: &Meta,
    blocks: &[Block],
    page: PageSize,
    margin: f64,
    page_numbers: bool,
    accent: Rgba,
) -> Result<Vec<u8>, String> {
    let mut b = Builder::new(page, margin, Meta { title: meta.title.clone(), author: meta.author.clone(), subject: meta.subject.clone() }, page_numbers, accent);
    for blk in blocks {
        b.add(blk.clone())?;
    }
    b.finish()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn minimal_pdf_builds() {
        if default_family().is_none() {
            return; // no fonts in this container — writer errors, tested elsewhere
        }
        let meta = Meta { title: "T".into(), author: "A".into(), subject: "".into() };
        let blocks = vec![
            Block::Cover { title: "Report".into(), subtitle: "Sub".into(), author: "Ada".into(), date: "2024-01-01".into(), accent: "#1F77B4".into() },
            Block::Heading { level: 1, text: "Section".into() },
            Block::Para { text: "Some paragraph text that wraps across lines when narrow enough to be interesting.".into(), align: Align::Justify, size_pt: None, color: None, line_spacing: 1.25 },
            Block::Table { header: vec!["A".into(), "B".into()], rows: vec![vec!["x".into(), "y".into()]], widths: None },
            Block::PageBreak,
            Block::Bullets { items: vec!["one".into(), "two".into()] },
        ];
        let bytes = build_document(&meta, &blocks, PageSize::A4, 56.0, true, Rgba::new(31, 119, 180)).unwrap();
        assert!(bytes.starts_with(b"%PDF-1.7"));
        assert!(bytes.windows(5).any(|w| w == b"%%EOF"));
        let pages = bytes.windows("/Type /Page ".len()).filter(|w| w == b"/Type /Page ").count();
        assert_eq!(pages, 3);
        let _ = pdf_date();
    }
}
