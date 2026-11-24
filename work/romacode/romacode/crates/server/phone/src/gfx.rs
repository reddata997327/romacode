//! Framebuffer drawing — a small integer raster library over an RGBA buffer.
//!
//! Everything the phone renders goes through [`Canvas`]: flat fills, rounded
//! rects, circles, lines and bitmap-font text (the classic 8x8 glyph set,
//! scaled). No image assets anywhere; every icon is drawn from shapes.

use font8x8::UnicodeFonts;

/// Maximum supported canvas side. Anything larger is almost certainly a
/// misconfiguration (e.g. a typo in `--display 10000x10000`) and is rejected
/// up front to avoid allocating gigabytes of pixel memory or tripping a u32
/// multiplication overflow on 32-bit targets.
const MAX_SIDE: u32 = 4096;

/// Reject obviously-wrong canvas dimensions before we allocate. This is a
/// developer-visible config error (typically a typo on `--display WxH`), not
/// untrusted input — panicking with a clear message is the right behaviour
/// because silently allocating ~67 GB (`4096*4096*4 * 4`) would OOM the host.
fn assert_valid_dims(w: u32, h: u32) {
    if w == 0 || h == 0 || w > MAX_SIDE || h > MAX_SIDE {
        panic!(
            "phone gfx: invalid canvas dims {w}x{h} (max {MAX_SIDE}x{MAX_SIDE}, min 1x1)"
        );
    }
}

pub type Rgb = [u8; 3];

pub struct Canvas {
    pub w: u32,
    pub h: u32,
    /// RGBA8, row-major, alpha always 255 (screencap encodes RGBA like a
    /// real device's screencap does).
    pub px: Vec<u8>,
}

impl Canvas {
    pub fn new(w: u32, h: u32) -> Self {
        assert_valid_dims(w, h);
        Self {
            w,
            h,
            px: vec![0; (w as usize) * (h as usize) * 4],
        }
    }

    pub fn resize(&mut self, w: u32, h: u32) {
        if self.w != w || self.h != h {
            assert_valid_dims(w, h);
            self.w = w;
            self.h = h;
            self.px = vec![0; (w as usize) * (h as usize) * 4];
        }
    }

    #[inline]
    fn idx(&self, x: u32, y: u32) -> usize {
        ((y * self.w + x) * 4) as usize
    }

    #[inline]
    pub fn set(&mut self, x: u32, y: u32, c: Rgb) {
        if x >= self.w || y >= self.h {
            return;
        }
        let i = self.idx(x, y);
        self.px[i] = c[0];
        self.px[i + 1] = c[1];
        self.px[i + 2] = c[2];
        self.px[i + 3] = 255;
    }

    pub fn fill(&mut self, c: Rgb) {
        for y in 0..self.h {
            for x in 0..self.w {
                self.set(x, y, c);
            }
        }
    }

    pub fn fill_rect(&mut self, x: i32, y: i32, w: i32, h: i32, c: Rgb) {
        // Clamp in i32 BEFORE casting: a negative (x+w) or (y+h) cast to u32
        // would wrap to ~4 billion and iterate forever (bug seen on the
        // wallpaper's off-screen circle tops).
        let x0 = x.max(0) as u32;
        let y0 = y.max(0) as u32;
        let x1 = (x + w).max(0).min(self.w as i32) as u32;
        let y1 = (y + h).max(0).min(self.h as i32) as u32;
        for yy in y0..y1 {
            for xx in x0..x1 {
                self.set(xx, yy, c);
            }
        }
    }

    /// Vertical gradient between two colours across `y0..y1`.
    pub fn gradient_v(&mut self, x: i32, y0: i32, y1: i32, w: i32, top: Rgb, bottom: Rgb) {
        let span = (y1 - y0).max(1) as f32;
        for yy in y0.max(0)..y1.min(self.h as i32) {
            let t = (yy - y0) as f32 / span;
            let c = mix(top, bottom, t);
            self.fill_rect(x, yy, w, 1, c);
        }
    }

    pub fn hline(&mut self, x: i32, y: i32, w: i32, c: Rgb) {
        self.fill_rect(x, y, w, 1, c);
    }

    pub fn vline(&mut self, x: i32, y: i32, h: i32, c: Rgb) {
        self.fill_rect(x, y, 1, h, c);
    }

    /// Filled rounded rectangle with a vertical gradient across its shape —
    /// every row is clipped to the rounded silhouette while the colour slides
    /// from `top` to `bottom`. The premium icon-tile primitive.
    pub fn round_rect_gradient_v(
        &mut self,
        x: i32,
        y: i32,
        w: i32,
        h: i32,
        r: i32,
        top: Rgb,
        bottom: Rgb,
    ) {
        if w <= 0 || h <= 0 {
            return;
        }
        let r = r.min(w / 2).min(h / 2).max(0);
        let span = (h - 1).max(1) as f32;
        for row in 0..h {
            // row span inset: inside the top/bottom corner arcs the row narrows
            let (x0, x1) = if row < r || row >= h - r {
                let dy = if row < r { r - row } else { row - (h - r) + 1 };
                // chord of the circle at this row offset: dx = r - sqrt(r²-dy²)
                let dy = dy.min(r);
                let dx = r - ((r * r - dy * dy) as f32).sqrt().round() as i32;
                (x + dx, x + w - dx)
            } else {
                (x, x + w)
            };
            let t = row as f32 / span;
            let c = mix(top, bottom, t);
            self.fill_rect(x0, y + row, x1 - x0, 1, c);
        }
    }

    /// Filled rounded rectangle.
    pub fn round_rect(&mut self, x: i32, y: i32, w: i32, h: i32, r: i32, c: Rgb) {
        if w <= 0 || h <= 0 {
            return;
        }
        let r = r.min(w / 2).min(h / 2).max(0);
        // centre slab + two side slabs + quarter circles at the corners
        self.fill_rect(x, y + r, w, h - 2 * r, c);
        self.fill_rect(x + r, y, w - 2 * r, r, c);
        self.fill_rect(x + r, y + h - r, w - 2 * r, r, c);
        for (cx, cy) in [
            (x + r, y + r),
            (x + w - r - 1, y + r),
            (x + r, y + h - r - 1),
            (x + w - r - 1, y + h - r - 1),
        ] {
            self.quarter_circle(cx, cy, r, c);
        }
    }

    fn quarter_circle(&mut self, cx: i32, cy: i32, r: i32, c: Rgb) {
        for dy in -r..=r {
            let dx = ((r * r - dy * dy) as f32).sqrt().round() as i32;
            if dy <= 0 {
                // top half: fill from cx-dx..cx+dx at row cy+dy
                self.fill_rect(cx - dx, cy + dy, dx * 2, 1, c);
            } else {
                self.fill_rect(cx - dx, cy + dy, dx * 2, 1, c);
            }
        }
    }

    /// Outline of a rounded rectangle (1 px stroke).
    pub fn round_rect_outline(&mut self, x: i32, y: i32, w: i32, h: i32, r: i32, c: Rgb) {
        let r = r.min(w / 2).min(h / 2).max(1);
        // top and bottom edges, straight segments between the corner arcs
        let x0 = x + r;
        let x1 = x + w - r;
        let y0 = y + r;
        let y1 = y + h - r;
        self.hline(x0, y, x1 - x0, c);
        self.hline(x0, y + h - 1, x1 - x0, c);
        self.vline(x, y0, y1 - y0, c);
        self.vline(x + w - 1, y0, y1 - y0, c);
        // corner arcs: walk each quarter circle and plot the two edge points
        for dy in 0..=r {
            let dx = ((r * r - dy * dy) as f32).sqrt().round() as i32;
            // top-left / top-right
            self.set((x + r - dx).max(0) as u32, (y + r - dy).max(0) as u32, c);
            self.set(
                (x + w - r + dx - 1).max(0) as u32,
                (y + r - dy).max(0) as u32,
                c,
            );
            // bottom-left / bottom-right
            self.set(
                (x + r - dx).max(0) as u32,
                (y + h - r + dy - 1).max(0) as u32,
                c,
            );
            self.set(
                (x + w - r + dx - 1).max(0) as u32,
                (y + h - r + dy - 1).max(0) as u32,
                c,
            );
        }
    }

    pub fn rect_outline(&mut self, x: i32, y: i32, w: i32, h: i32, c: Rgb) {
        self.hline(x, y, w, c);
        self.hline(x, y + h - 1, w, c);
        self.vline(x, y, h, c);
        self.vline(x + w - 1, y, h, c);
    }

    // ── premium layer: alpha blending, shadows, glows, gradients ────────────

    /// Read the pixel currently at (x, y).
    pub fn get(&self, x: u32, y: u32) -> Rgb {
        if x >= self.w || y >= self.h {
            return [0, 0, 0];
        }
        let i = self.idx(x, y);
        [self.px[i], self.px[i + 1], self.px[i + 2]]
    }

    /// Alpha-blend `c` over the existing pixel (`alpha` 0..=255).
    pub fn set_alpha(&mut self, x: u32, y: u32, c: Rgb, alpha: u8) {
        if x >= self.w || y >= self.h || alpha == 0 {
            return;
        }
        if alpha == 255 {
            self.set(x, y, c);
            return;
        }
        let base = self.get(x, y);
        let a = alpha as u32;
        let blend = |b: u8, o: u8| ((b as u32 * (255 - a) + o as u32 * a) / 255) as u8;
        let out = [
            blend(base[0], c[0]),
            blend(base[1], c[1]),
            blend(base[2], c[2]),
        ];
        let i = self.idx(x, y);
        self.px[i] = out[0];
        self.px[i + 1] = out[1];
        self.px[i + 2] = out[2];
        self.px[i + 3] = 255;
    }

    /// Alpha-blended flat rect (glass panels, scrims).
    pub fn fill_rect_alpha(&mut self, x: i32, y: i32, w: i32, h: i32, c: Rgb, alpha: u8) {
        let x0 = x.max(0) as u32;
        let y0 = y.max(0) as u32;
        let x1 = (x + w).max(0).min(self.w as i32) as u32;
        let y1 = (y + h).max(0).min(self.h as i32) as u32;
        for yy in y0..y1 {
            for xx in x0..x1 {
                self.set_alpha(xx, yy, c, alpha);
            }
        }
    }

    /// Alpha-blended rounded rect — the workhorse of the glass look.
    pub fn round_rect_alpha(&mut self, x: i32, y: i32, w: i32, h: i32, r: i32, c: Rgb, alpha: u8) {
        if w <= 0 || h <= 0 || alpha == 0 {
            return;
        }
        let r = r.min(w / 2).min(h / 2).max(0);
        self.fill_rect_alpha(x, y + r, w, h - 2 * r, c, alpha);
        self.fill_rect_alpha(x + r, y, w - 2 * r, r, c, alpha);
        self.fill_rect_alpha(x + r, y + h - r, w - 2 * r, r, c, alpha);
        for (cx, cy) in [
            (x + r, y + r),
            (x + w - r - 1, y + r),
            (x + r, y + h - r - 1),
            (x + w - r - 1, y + h - r - 1),
        ] {
            self.quarter_circle_alpha(cx, cy, r, c, alpha);
        }
    }

    fn quarter_circle_alpha(&mut self, cx: i32, cy: i32, r: i32, c: Rgb, alpha: u8) {
        for dy in -r..=r {
            let dx = ((r * r - dy * dy) as f32).sqrt().round() as i32;
            let yy = cy + dy;
            self.fill_rect_alpha(cx - dx, yy, dx * 2, 1, c, alpha);
        }
    }

    /// Soft drop shadow under a rounded rect: concentric translucent rings,
    /// strongest at the edge, fading outward. Draw BEFORE the panel itself.
    pub fn shadow(&mut self, x: i32, y: i32, w: i32, h: i32, r: i32, strength: u8) {
        let layers = 6;
        for l in (0..layers).rev() {
            let spread = l * 3;
            // falloff: outermost ~10% of strength → edge ~55%
            let t = 1.0 - l as f32 / layers as f32;
            let a = (strength as f32 * (0.10 + 0.45 * t)) as u8;
            self.round_rect_alpha(
                x - spread,
                y - spread,
                w + spread * 2,
                h + spread * 2,
                r + spread,
                [0, 0, 0],
                a,
            );
        }
    }

    /// Radial glow: a soft orb whose brightness falls off smoothly from the
    /// centre (`alpha` = centre intensity, 0..=255). This is what makes the
    /// wallpaper orbs luminous instead of flat circles.
    pub fn glow(&mut self, cx: i32, cy: i32, r: i32, c: Rgb, alpha: u8) {
        if r <= 0 || alpha == 0 {
            return;
        }
        let x0 = (cx - r).max(0);
        let x1 = (cx + r).min(self.w as i32 - 1);
        let y0 = (cy - r).max(0);
        let y1 = (cy + r).min(self.h as i32 - 1);
        let rr = (r * r) as f32;
        for py in y0..=y1 {
            let dy = py - cy;
            for px in x0..=x1 {
                let dx = px - cx;
                let d2 = (dx * dx + dy * dy) as f32;
                if d2 >= rr {
                    continue;
                }
                // quadratic falloff (smooth, no hard edge)
                let t = 1.0 - d2 / rr;
                let a = (alpha as f32 * t * t) as u8;
                if a > 0 {
                    self.set_alpha(px as u32, py as u32, c, a);
                }
            }
        }
    }

    /// Horizontal gradient across `x0..x0+w` (progress bars, sheens).
    pub fn gradient_h(&mut self, x0: i32, y: i32, w: i32, h: i32, left: Rgb, right: Rgb) {
        let span = w.max(1) as f32;
        for i in 0..w {
            let t = i as f32 / span;
            let c = mix(left, right, t);
            self.fill_rect(x0 + i, y, 1, h, c);
        }
    }

    /// Vertical gradient with alpha start → alpha end (scrims, status bar tint).
    pub fn gradient_v_alpha(&mut self, x: i32, y0: i32, y1: i32, w: i32, c: Rgb, a0: u8, a1: u8) {
        let span = (y1 - y0).max(1) as f32;
        for yy in y0.max(0)..y1.min(self.h as i32) {
            let t = (yy - y0) as f32 / span;
            let a = (a0 as f32 * (1.0 - t) + a1 as f32 * t).round() as u8;
            self.fill_rect_alpha(x, yy, w, 1, c, a);
        }
    }

    /// Darken the frame edges (photographic vignette) for depth.
    pub fn vignette(&mut self, strength: u8) {
        let w = self.w as i32;
        let h = self.h as i32;
        let cx = w as f32 / 2.0;
        let cy = h as f32 / 2.0;
        let max_d = (cx * cx + cy * cy).sqrt();
        for y in 0..h {
            for x in 0..w {
                let dx = x as f32 - cx;
                let dy = y as f32 - cy;
                let t = ((dx * dx + dy * dy).sqrt() / max_d).clamp(0.0, 1.0);
                // only the outer 30% gets darkened, smoothly
                let edge = ((t - 0.7) / 0.3).clamp(0.0, 1.0);
                let a = (strength as f32 * edge) as u8;
                if a > 0 {
                    self.set_alpha(x as u32, y as u32, [0, 0, 0], a);
                }
            }
        }
    }

    pub fn circle(&mut self, cx: i32, cy: i32, r: i32, c: Rgb) {
        for dy in -r..=r {
            let dx = ((r * r - dy * dy) as f32).sqrt().round() as i32;
            self.fill_rect(cx - dx, cy + dy, dx * 2, 1, c);
        }
    }

    pub fn ring(&mut self, cx: i32, cy: i32, r: i32, c: Rgb) {
        for dy in -r..=r {
            let dx = ((r * r - dy * dy) as f32).sqrt().round() as i32;
            self.set((cx - dx).max(0) as u32, (cy + dy).max(0) as u32, c);
            self.set((cx + dx - 1).max(0) as u32, (cy + dy).max(0) as u32, c);
        }
        for dx in -r..=r {
            let dy = ((r * r - dx * dx) as f32).sqrt().round() as i32;
            self.set((cx + dx).max(0) as u32, (cy - dy).max(0) as u32, c);
            self.set((cx + dx).max(0) as u32, (cy + dy - 1).max(0) as u32, c);
        }
    }

    pub fn line(&mut self, x0: i32, y0: i32, x1: i32, y1: i32, c: Rgb, width: i32) {
        let dx = (x1 - x0).abs();
        let dy = (y1 - y0).abs();
        let sx = if x0 < x1 { 1 } else { -1 };
        let sy = if y0 < y1 { 1 } else { -1 };
        let mut err = dx - dy;
        let (mut x, mut y) = (x0, y0);
        let hw = (width / 2).max(0);
        loop {
            self.fill_rect(x - hw, y - hw, width.max(1), width.max(1), c);
            if x == x1 && y == y1 {
                break;
            }
            let e2 = 2 * err;
            if e2 > -dy {
                err -= dy;
                x += sx;
            }
            if e2 < dx {
                err += dx;
                y += sy;
            }
        }
    }

    /// Draw one character with the 8x8 bitmap font scaled to `px` pixels,
    /// smoothed: the glyph is resampled bilinearly and the edge is pushed
    /// through a smoothstep, so strokes stay crisp but diagonals and curves
    /// lose the chunky "scaled pixel" staircase. Returns the advance width
    /// (always `8 * px`, so layout is unchanged).
    pub fn char_at(&mut self, x: i32, y: i32, ch: char, px: u32, c: Rgb) -> i32 {
        let s = px.max(1) as i32;
        let adv = 8 * s;
        let Some(glyph) = glyph_for(ch) else {
            return adv;
        };
        if glyph.iter().all(|&r| r == 0) {
            return adv;
        }
        let sf = s as f32;
        for oy in 0..adv {
            let py = y + oy;
            if py < 0 {
                continue;
            }
            let v = (oy as f32 + 0.5) / sf - 0.5;
            for ox in 0..adv {
                let pxx = x + ox;
                if pxx < 0 {
                    continue;
                }
                let u = (ox as f32 + 0.5) / sf - 0.5;
                let cov = glyph_coverage(&glyph, u, v);
                if cov > 0.0 {
                    self.set_alpha(pxx as u32, py as u32, c, (cov * 255.0 + 0.5) as u8);
                }
            }
        }
        adv
    }

    /// Draw a string; returns the advance width in pixels.
    pub fn text(&mut self, x: i32, y: i32, s: &str, px: u32, c: Rgb) -> i32 {
        let mut cx = x;
        for ch in s.chars() {
            cx += self.char_at(cx, y, ch, px, c);
        }
        cx - x
    }

    /// Width of a string at the given pixel size. The bitmap glyphs advance
    /// 8 scaled pixels per character (see `char_at`), so width = chars * 8 * px.
    pub fn text_w(s: &str, px: u32) -> i32 {
        (s.chars().count() as i32) * (px as i32 * 8)
    }

    /// Draw `s` centered horizontally at `xc`.
    pub fn text_center(&mut self, xc: i32, y: i32, s: &str, px: u32, c: Rgb) -> i32 {
        let w = Self::text_w(s, px);
        self.text(xc - w / 2, y, s, px, c)
    }

    /// Truncate with an ellipsis to fit `max_w` pixels, then draw.
    pub fn text_fit(&mut self, x: i32, y: i32, s: &str, px: u32, max_w: i32, c: Rgb) {
        let adv = px as i32 * 8;
        let mut out = String::new();
        let mut w = 0;
        let chars: Vec<char> = s.chars().collect();
        for (i, ch) in chars.iter().enumerate() {
            if w + adv + adv > max_w && i + 1 < chars.len() {
                out.push('…');
                break;
            }
            out.push(*ch);
            w += adv;
        }
        self.text(x, y, &out, px, c);
    }

    /// Blit a small RGBA patch (icons from PNG data use this via decode).
    pub fn blit(&mut self, x: i32, y: i32, w: u32, h: u32, rgba: &[u8]) {
        for yy in 0..h {
            for xx in 0..w {
                let i = ((yy * w + xx) * 4) as usize;
                if i + 3 < rgba.len() && rgba[i + 3] > 128 {
                    self.set(
                        (x + xx as i32).max(0) as u32,
                        (y + yy as i32).max(0) as u32,
                        [rgba[i], rgba[i + 1], rgba[i + 2]],
                    );
                }
            }
        }
    }
}
/// Look up an 8x8 glyph: ASCII, then Latin-1, plus hand-drawn bitmaps for the
/// typographic characters the UI uses (the stock tables have no `…` or `•`)
/// and plain fallbacks for curly quotes and dashes.
fn glyph_for(ch: char) -> Option<[u8; 8]> {
    match ch {
        '\u{2026}' => return Some([0, 0, 0, 0, 0, 0, 0x49, 0x49]), // …
        '\u{2022}' => return Some([0, 0, 0x18, 0x3C, 0x3C, 0x18, 0, 0]), // •
        '\u{00B7}' => return Some([0, 0, 0, 0x18, 0x18, 0, 0, 0]), // ·
        '\u{2018}' | '\u{2019}' => return glyph_for('\''),
        '\u{201C}' | '\u{201D}' => return glyph_for('"'),
        '\u{2013}' | '\u{2014}' | '\u{2212}' => return glyph_for('-'),
        '\u{00A0}' => return Some([0; 8]),
        '\u{00D7}' => return glyph_for('x'),
        _ => {}
    }
    font8x8::BASIC_FONTS
        .get(ch)
        .or_else(|| font8x8::LATIN_FONTS.get(ch))
        .or(Some([0; 8]))
}

/// Anti-aliased coverage (0..=1) of an 8x8 bitmap at fractional glyph
/// coordinates `(u, v)` (texel centres sit on integers).
fn glyph_coverage(g: &[u8; 8], u: f32, v: f32) -> f32 {
    let bit = |x: i32, y: i32| -> f32 {
        if (0..8).contains(&x) && (0..8).contains(&y) {
            ((g[y as usize] >> x) & 1) as f32
        } else {
            0.0
        }
    };
    let (x0, y0) = (u.floor(), v.floor());
    let (fx, fy) = (u - x0, v - y0);
    let (xi, yi) = (x0 as i32, y0 as i32);
    let top = bit(xi, yi) * (1.0 - fx) + bit(xi + 1, yi) * fx;
    let bot = bit(xi, yi + 1) * (1.0 - fx) + bit(xi + 1, yi + 1) * fx;
    let val = top * (1.0 - fy) + bot * fy;
    // edge shaping: slightly below 0.5 keeps strokes a touch bolder/clearer
    let t = ((val - 0.28) / (0.62 - 0.28)).clamp(0.0, 1.0);
    t * t * (3.0 - 2.0 * t)
}

pub fn mix(a: Rgb, b: Rgb, t: f32) -> Rgb {
    let t = t.clamp(0.0, 1.0);
    [
        (a[0] as f32 * (1.0 - t) + b[0] as f32 * t).round() as u8,
        (a[1] as f32 * (1.0 - t) + b[1] as f32 * t).round() as u8,
        (a[2] as f32 * (1.0 - t) + b[2] as f32 * t).round() as u8,
    ]
}

/// Chebyshev distance for hit-testing helpers.
pub fn in_circle(px: i32, py: i32, cx: i32, cy: i32, r: i32) -> bool {
    let dx = px - cx;
    let dy = py - cy;
    dx * dx + dy * dy <= r * r
}

pub fn in_rect(px: i32, py: i32, x: i32, y: i32, w: i32, h: i32) -> bool {
    px >= x && px < x + w && py >= y && py < y + h
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn wallpaper_draw_terminates_quickly() {
        let mut c = Canvas::new(720, 1560);
        let t0 = std::time::Instant::now();
        crate::theme::draw_wallpaper(&mut c, 0, 0, 1560);
        eprintln!("wallpaper took {:?}", t0.elapsed());
        assert!(t0.elapsed().as_secs() < 5);
    }

    #[test]
    fn launcher_screen_draw_terminates() {
        let mut c = Canvas::new(720, 1560);
        let dir = std::env::temp_dir().join(format!("phone-gfx-test-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let mut st = crate::persist::fresh(&dir);
        st.boot = crate::state::BootPhase::Home;
        st.dirty = true;
        let t0 = std::time::Instant::now();
        // draw one full home frame via the public compositor draw path
        crate::compose::tick(&mut st);
        eprintln!("home frame took {:?}, fb_gen={}", t0.elapsed(), st.fb_gen);
        assert!(st.fb_gen >= 1);
        assert!(t0.elapsed().as_secs() < 10);
        let _ = std::fs::remove_dir_all(&dir);
    }

    // ── premium primitives ───────────────────────────────────────────────────

    #[test]
    fn alpha_blend_is_exact_and_monotonic() {
        let mut c = Canvas::new(8, 8);
        c.fill([0, 0, 0]);
        c.set_alpha(0, 0, [255, 255, 255], 0);
        assert_eq!(c.get(0, 0), [0, 0, 0], "alpha 0 must not change the pixel");
        c.set_alpha(1, 0, [255, 255, 255], 255);
        assert_eq!(c.get(1, 0), [255, 255, 255], "alpha 255 must fully replace");
        c.set_alpha(2, 0, [255, 255, 255], 128);
        assert_eq!(c.get(2, 0), [128, 128, 128], "half blend over black");
        // monotonic ramp
        let mut prev = 0u8;
        for a in 0..=255u8 {
            c.set_alpha(3, 0, [255, 0, 0], a);
            let v = c.get(3, 0)[0];
            assert!(v >= prev, "red channel must be non-decreasing in alpha");
            prev = v;
        }
    }

    #[test]
    fn shadow_darkens_without_leaving_artifacts() {
        let mut c = Canvas::new(120, 120);
        c.fill([200, 200, 200]);
        c.shadow(20, 20, 80, 80, 12, 90);
        // just inside the shadow ring the pixel must be darker than the base
        let edge = c.get(16, 60);
        assert!(edge[0] < 200, "shadow must darken the surround");
        // far corner must be untouched
        assert_eq!(c.get(119, 0), [200, 200, 200]);
    }

    #[test]
    fn glow_falls_off_smoothly_to_zero() {
        let mut c = Canvas::new(100, 100);
        c.fill([0, 0, 0]);
        c.glow(50, 50, 30, [255, 255, 255], 255);
        let centre = c.get(50, 50)[0];
        let mid = c.get(50 + 15, 50)[0];
        let rim = c.get(50 + 29, 50)[0];
        assert!(centre > mid && mid > rim, "brightness must fall outward");
        assert!(rim < 40, "rim must be near-black, not a hard edge");
        assert_eq!(
            c.get(50 + 31, 50),
            [0, 0, 0],
            "outside the radius untouched"
        );
    }

    #[test]
    fn outline_is_one_pixel_thick() {
        let mut c = Canvas::new(60, 60);
        c.fill([0, 0, 0]);
        c.round_rect_outline(10, 10, 40, 40, 10, [255, 255, 255]);
        // top edge is lit, and exactly one pixel tall
        assert_eq!(c.get(30, 10), [255, 255, 255]);
        assert_eq!(
            c.get(30, 11),
            [0, 0, 0],
            "no second row — crisp 1 px stroke"
        );
        assert_eq!(c.get(30, 49), [255, 255, 255]);
        assert_eq!(c.get(30, 48), [0, 0, 0]);
    }

    #[test]
    fn gradients_and_vignette_terminate() {
        let mut c = Canvas::new(200, 200);
        c.fill([10, 10, 10]);
        c.gradient_h(0, 100, 200, 4, [255, 0, 0], [0, 0, 255]);
        c.gradient_v_alpha(0, 0, 48, 200, [0, 0, 0], 180, 0);
        c.vignette(120);
        // vignette keeps the centre intact and darkens the corner
        assert!(c.get(100, 100)[0] >= 10, "centre not vignetted");
        let corner = c.get(199, 199);
        assert!(corner[0] < 10 + 1, "corner darker than base");
    }
}
