//! The in-house 2D rasterizer: RGBA surface, shapes, gradients and text.
//! Charts ([`crate::charts`]) and the design engine ([`crate::canvas`]) both
//! draw through this, then encode PNG via the `png` crate. All math is
//! integer/f32 — no external image stack.

use crate::color::Rgba;
use crate::fonts::{Face, TextMeter};

/// An RGBA8 drawing surface.
pub struct Surface {
    pub w: u32,
    pub h: u32,
    pub px: Vec<u8>, // RGBA rows, row-major
}

impl Surface {
    pub fn new(w: u32, h: u32, bg: Rgba) -> Self {
        let n = (w as usize) * (h as usize);
        let mut px = Vec::with_capacity(n * 4);
        for _ in 0..n {
            px.extend_from_slice(&[bg.r, bg.g, bg.b, bg.a]);
        }
        Self { w, h, px }
    }

    #[inline]
    pub fn blend(&mut self, x: i32, y: i32, c: Rgba) {
        if x < 0 || y < 0 || x >= self.w as i32 || y >= self.h as i32 || c.a == 0 {
            return;
        }
        let idx = ((y as u32 * self.w + x as u32) * 4) as usize;
        let bg = Rgba::rgba(self.px[idx], self.px[idx + 1], self.px[idx + 2], self.px[idx + 3]);
        let out = c.over(bg);
        self.px[idx..idx + 4].copy_from_slice(&[out.r, out.g, out.b, out.a]);
    }

    #[inline]
    pub fn set(&mut self, x: i32, y: i32, c: Rgba) {
        if x < 0 || y < 0 || x >= self.w as i32 || y >= self.h as i32 {
            return;
        }
        let idx = ((y as u32 * self.w + x as u32) * 4) as usize;
        self.px[idx..idx + 4].copy_from_slice(&[c.r, c.g, c.b, c.a]);
    }

    pub fn fill(&mut self, c: Rgba) {
        for px in self.px.chunks_exact_mut(4) {
            px.copy_from_slice(&[c.r, c.g, c.b, c.a]);
        }
    }

    // ── primitives ──────────────────────────────────────────────────────────

    pub fn rect_fill(&mut self, x0: i32, y0: i32, w: i32, h: i32, c: Rgba) {
        for y in y0..y0 + h {
            for x in x0..x0 + w {
                self.blend(x, y, c);
            }
        }
    }

    pub fn rect_outline(&mut self, x0: i32, y0: i32, w: i32, h: i32, c: Rgba, t: i32) {
        let t = t.max(1);
        self.rect_fill(x0, y0, w, t, c);
        self.rect_fill(x0, y0 + h - t, w, t, c);
        self.rect_fill(x0, y0, t, h, c);
        self.rect_fill(x0 + w - t, y0, t, h, c);
    }

    /// Rounded-corner filled rect (radius clamped to half the side).
    pub fn round_rect_fill(&mut self, x0: i32, y0: i32, w: i32, h: i32, r: i32, c: Rgba) {
        let r = r.max(0).min(w.min(h) / 2);
        if r == 0 {
            return self.rect_fill(x0, y0, w, h, c);
        }
        self.rect_fill(x0 + r, y0, w - 2 * r, h, c);
        self.rect_fill(x0, y0 + r, r, h - 2 * r, c);
        self.rect_fill(x0 + w - r, y0 + r, r, h - 2 * r, c);
        self.quarter(x0 + r, y0 + r, r, c, 2);
        self.quarter(x0 + w - r - 1, y0 + r, r, c, 1);
        self.quarter(x0 + r, y0 + h - r - 1, r, c, 3);
        self.quarter(x0 + w - r - 1, y0 + h - r - 1, r, c, 0);
    }

    /// One quadrant of a filled circle. `q`: 0=bottom-right, 1=bottom-left,
    /// 2=top-left, 3=top-right (the corner the quarter bends away from).
    fn quarter(&mut self, cx: i32, cy: i32, r: i32, c: Rgba, q: u8) {
        for y in 0..=r {
            for x in 0..=r {
                let dx = (x) as f64;
                let dy = (y) as f64;
                if dx * dx + dy * dy <= (r as f64 + 0.5) * (r as f64 + 0.5) {
                    let (px, py) = match q {
                        0 => (cx + x, cy + y),
                        1 => (cx - x, cy + y),
                        2 => (cx - x, cy - y),
                        _ => (cx + x, cy - y),
                    };
                    self.blend(px, py, c);
                }
            }
        }
    }

    pub fn circle_fill(&mut self, cx: i32, cy: i32, r: i32, c: Rgba) {
        let r = r.max(0);
        for y in -r..=r {
            for x in -r..=r {
                if (x * x + y * y) as f64 <= (r as f64 + 0.5) * (r as f64 + 0.5) {
                    self.blend(cx + x, cy + y, c);
                }
            }
        }
    }

    pub fn circle_outline(&mut self, cx: i32, cy: i32, r: i32, c: Rgba, t: i32) {
        let r = r.max(1);
        let t = t.max(1) as f64;
        let rf = r as f64;
        for y in -(r as i32 + 1)..=(r as i32 + 1) {
            for x in -(r as i32 + 1)..=(r as i32 + 1) {
                let d = ((x * x + y * y) as f64).sqrt();
                if d <= rf + 0.5 && d >= rf - t {
                    self.blend(cx + x, cy + y, c);
                }
            }
        }
    }

    /// Thick line (distance-based, works for any angle).
    pub fn line(&mut self, x0: f64, y0: f64, x1: f64, y1: f64, c: Rgba, width: f64) {
        let steps = ((x1 - x0).abs().max((y1 - y0).abs()).ceil() as i32).max(1) * 2;
        let half = (width / 2.0).max(0.5);
        for i in 0..=steps {
            let t = i as f64 / steps as f64;
            let x = x0 + (x1 - x0) * t;
            let y = y0 + (y1 - y0) * t;
            for dy in -(half.ceil() as i32)..=(half.ceil() as i32) {
                for dx in -(half.ceil() as i32)..=(half.ceil() as i32) {
                    let d = ((dx * dx + dy * dy) as f64).sqrt();
                    if d <= half + 0.35 {
                        self.blend(x as i32 + dx, y as i32 + dy, c);
                    }
                }
            }
        }
    }

    /// Filled polygon (scanline, even-odd rule).
    pub fn polygon_fill(&mut self, pts: &[(f64, f64)], c: Rgba) {
        if pts.len() < 3 {
            return;
        }
        let min_y = pts.iter().map(|p| p.1).fold(f64::MAX, f64::min).floor() as i32;
        let max_y = pts.iter().map(|p| p.1).fold(f64::MIN, f64::max).ceil() as i32;
        for y in min_y..=max_y {
            let yc = y as f64 + 0.5;
            let mut xs: Vec<f64> = Vec::new();
            for i in 0..pts.len() {
                let a = pts[i];
                let b = pts[(i + 1) % pts.len()];
                if (a.1 <= yc && b.1 > yc) || (b.1 <= yc && a.1 > yc) {
                    let t = (yc - a.1) / (b.1 - a.1);
                    xs.push(a.0 + (b.0 - a.0) * t);
                }
            }
            xs.sort_by(|p, q| p.partial_cmp(q).unwrap_or(std::cmp::Ordering::Equal));
            for pair in xs.chunks(2) {
                if pair.len() == 2 {
                    for x in pair[0].round() as i32..=pair[1].round() as i32 {
                        self.blend(x, y, c);
                    }
                }
            }
        }
    }

    /// Vertical or linear two-stop gradient.
    pub fn gradient(&mut self, x0: i32, y0: i32, w: i32, h: i32, from: Rgba, to: Rgba, vertical: bool) {
        for y in y0..y0 + h {
            for x in x0..x0 + w {
                let t = if vertical {
                    (y - y0) as f64 / h.max(1) as f64
                } else {
                    (x - x0) as f64 / w.max(1) as f64
                };
                let t = t.clamp(0.0, 1.0);
                let mix = |a: u8, b: u8| (a as f64 + (b as f64 - a as f64) * t) as u8;
                self.set(x, y, Rgba::rgba(mix(from.r, to.r), mix(from.g, to.g), mix(from.b, to.b), mix(from.a, to.a)));
            }
        }
    }

    // ── text ────────────────────────────────────────────────────────────────

    /// Draw one line of text with the top edge at (x, y). `color` alpha
    /// applied per-glyph coverage. Returns the advance width used.
    pub fn text(&mut self, face: &Face, x: f64, y: f64, s: &str, px: f32, color: Rgba) -> f64 {
        let mut pen = x as f32;
        let baseline = y as f32 + face.ascent(px);
        for ch in s.chars() {
            if ch == '\n' {
                break;
            }
            let bmp = face.rasterize(ch, px);
            let bx = (pen + bmp.left).round() as i32;
            let by = (baseline + bmp.top).round() as i32;
            for gy in 0..bmp.h {
                for gx in 0..bmp.w {
                    let cov = bmp.buf[gy * bmp.w + gx] as u32;
                    if cov == 0 {
                        continue;
                    }
                    self.blend(bx + gx as i32, by + gy as i32, Rgba::rgba(color.r, color.g, color.b, ((color.a as u32 * cov) / 255) as u8));
                }
            }
            pen += face.advance(ch, px);
        }
        (pen - x as f32) as f64
    }

    /// Measure a single line.
    pub fn measure(face: &Face, s: &str, px: f32) -> f64 {
        s.chars().map(|c| face.advance(c, px)).sum::<f32>() as f64
    }

    pub fn meter(face: &Face) -> TextMeter<'_> {
        TextMeter::new(face)
    }
}

/// Encode an RGBA surface to PNG bytes.
pub fn encode_png(s: &Surface) -> Result<Vec<u8>, String> {
    let mut buf = Vec::new();
    {
        let mut enc = png::Encoder::new(&mut buf, s.w, s.h);
        enc.set_color(png::ColorType::Rgba);
        enc.set_depth(png::BitDepth::Eight);
        let mut writer = enc.write_header().map_err(|e| format!("png header: {e}"))?;
        writer.write_image_data(&s.px).map_err(|e| format!("png data: {e}"))?;
    }
    Ok(buf)
}

/// Decode a PNG file to an RGBA surface (used when embedding into PDFs).
pub fn decode_png(data: &[u8]) -> Result<Surface, String> {
    let dec = png::Decoder::new(std::io::Cursor::new(data));
    let mut reader = dec.read_info().map_err(|e| format!("png open: {e}"))?;
    let mut buf = vec![0; reader.output_buffer_size()];
    let info = reader.next_frame(&mut buf).map_err(|e| format!("png frame: {e}"))?;
    let (w, h) = (info.width, info.height);
    let src = &buf[..info.buffer_size()];
    let mut px = Vec::with_capacity((w * h * 4) as usize);
    match info.color_type {
        png::ColorType::Rgba => px.extend_from_slice(src),
        png::ColorType::Rgb => {
            for chunk in src.chunks_exact(3) {
                px.extend_from_slice(&[chunk[0], chunk[1], chunk[2], 255]);
            }
        }
        png::ColorType::Grayscale => {
            for g in src {
                px.extend_from_slice(&[*g, *g, *g, 255]);
            }
        }
        png::ColorType::GrayscaleAlpha => {
            for chunk in src.chunks_exact(2) {
                px.extend_from_slice(&[chunk[0], chunk[0], chunk[0], chunk[1]]);
            }
        }
        png::ColorType::Indexed => return Err("png: indexed color unsupported — convert to RGBA".into()),
    }
    Ok(Surface { w, h, px })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn png_round_trip() {
        let mut s = Surface::new(16, 16, Rgba::new(10, 20, 30));
        s.rect_fill(2, 2, 8, 8, Rgba::new(255, 0, 0));
        let bytes = encode_png(&s).unwrap();
        let back = decode_png(&bytes).unwrap();
        assert_eq!(back.w, 16);
        assert_eq!(back.px[0..4], [10, 20, 30, 255]);
        assert_eq!(back.px[(3 * 16 * 4 + 3 * 4)..][0], 255);
    }

    #[test]
    fn shapes_draw() {
        let mut s = Surface::new(64, 64, Rgba::new(255, 255, 255));
        s.circle_fill(32, 32, 10, Rgba::new(0, 0, 255));
        s.line(0.0, 0.0, 63.0, 63.0, Rgba::new(0, 0, 0), 2.0);
        s.polygon_fill(&[(10.0, 50.0), (30.0, 20.0), (50.0, 50.0)], Rgba::new(0, 255, 0));
        // something non-white got drawn
        let nonwhite = s.px.chunks_exact(4).filter(|p| p[0] != 255 || p[1] != 255 || p[2] != 255).count();
        assert!(nonwhite > 100);
    }
}
