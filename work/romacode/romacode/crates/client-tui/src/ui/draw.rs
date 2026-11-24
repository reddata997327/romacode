//! Low-level drawing helpers that write straight into the ratatui buffer.
use ratatui::{
    buffer::Buffer,
    layout::Rect,
    style::{Color, Style},
};
use unicode_width::{UnicodeWidthChar, UnicodeWidthStr};

/// A pre-styled, pre-wrapped transcript row.
#[derive(Clone, Default)]
pub struct Row {
    pub spans: Vec<(String, Style)>,
    /// Optional background: (colour, from column, to column exclusive).
    pub bg: Option<(Color, u16, u16)>,
}

impl Row {
    pub fn blank() -> Self {
        Self::default()
    }
    pub fn from(spans: Vec<(String, Style)>) -> Self {
        Self { spans, bg: None }
    }
}

/// Write `s` at (x, y) clipped to `clip`. Returns the x after the last cell.
pub fn put(buf: &mut Buffer, x: u16, y: u16, s: &str, style: Style, clip: Rect) -> u16 {
    // Popups are sized for a comfortable terminal; on a small one their rect
    // spills past the buffer, and writing there panics. Clip to the buffer too.
    let clip = clip.intersection(buf.area);
    if y < clip.y || y >= clip.bottom() || x >= clip.right() {
        return x;
    }
    let max = (clip.right() - x) as usize;
    let (nx, _) = buf.set_stringn(x, y, s, max, style);
    nx
}

pub fn put_spans(buf: &mut Buffer, x: u16, y: u16, spans: &[(String, Style)], clip: Rect) -> u16 {
    let mut cx = x;
    for (s, st) in spans {
        cx = put(buf, cx, y, s, *st, clip);
    }
    cx
}

/// Fill a rect with spaces and a background colour.
pub fn fill(buf: &mut Buffer, r: Rect, bg: Color) {
    let r = r.intersection(buf.area);
    for y in r.y..r.bottom() {
        for x in r.x..r.right() {
            let c = buf.get_mut(x, y);
            c.set_symbol(" ");
            c.set_bg(bg);
        }
    }
}

/// Paint only the background colour, keeping any glyphs.
pub fn tint(buf: &mut Buffer, r: Rect, bg: Color) {
    let r = r.intersection(buf.area);
    for y in r.y..r.bottom() {
        for x in r.x..r.right() {
            buf.get_mut(x, y).set_bg(bg);
        }
    }
}

pub fn hline(buf: &mut Buffer, x: u16, y: u16, w: u16, ch: &str, style: Style, clip: Rect) {
    for i in 0..w {
        put(buf, x + i, y, ch, style, clip);
    }
}

/// Rounded border box.
pub fn rounded_box(buf: &mut Buffer, r: Rect, style: Style) {
    if r.width < 2 || r.height < 2 {
        return;
    }
    let clip = buf.area;
    let (x0, y0, x1, y1) = (r.x, r.y, r.right() - 1, r.bottom() - 1);
    put(buf, x0, y0, "╭", style, clip);
    put(buf, x1, y0, "╮", style, clip);
    put(buf, x0, y1, "╰", style, clip);
    put(buf, x1, y1, "╯", style, clip);
    for x in x0 + 1..x1 {
        put(buf, x, y0, "─", style, clip);
        put(buf, x, y1, "─", style, clip);
    }
    for y in y0 + 1..y1 {
        put(buf, x0, y, "│", style, clip);
        put(buf, x1, y, "│", style, clip);
    }
}

pub fn pad_to(s: &str, w: usize) -> String {
    let cur = s.width();
    if cur >= w {
        s.to_string()
    } else {
        format!("{}{}", s, " ".repeat(w - cur))
    }
}

/// Truncate to `w` display cells, adding an ellipsis if cut.
pub fn truncate(s: &str, w: usize) -> String {
    if s.width() <= w {
        return s.to_string();
    }
    let mut out = String::new();
    let mut cur = 0;
    for ch in s.chars() {
        let cw = ch.width().unwrap_or(0);
        if cur + cw + 1 > w {
            break;
        }
        out.push(ch);
        cur += cw;
    }
    out.push('…');
    out
}

/// Greedy word-wrap of plain text to `w` cells.
pub fn wrap_plain(s: &str, w: usize) -> Vec<String> {
    let w = w.max(1);
    let mut lines = Vec::new();
    for src in s.split('\n') {
        let mut line = String::new();
        let mut lw = 0;
        for word in src.split(' ') {
            let ww = word.width();
            if lw > 0 && lw + 1 + ww > w {
                lines.push(std::mem::take(&mut line));
                lw = 0;
            }
            if lw > 0 {
                line.push(' ');
                lw += 1;
            }
            // hard-split very long words
            if ww > w {
                for ch in word.chars() {
                    let cw = ch.width().unwrap_or(0);
                    if lw + cw > w {
                        lines.push(std::mem::take(&mut line));
                        lw = 0;
                    }
                    line.push(ch);
                    lw += cw;
                }
            } else {
                line.push_str(word);
                lw += ww;
            }
        }
        lines.push(line);
    }
    lines
}
