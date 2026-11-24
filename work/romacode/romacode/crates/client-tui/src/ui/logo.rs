//! ROMACODE wordmark rendered natively with half-block cells.
//! Glyph geometry comes from the original `romacode.rs` generator; the colour
//! ramp is re-tuned (cream → amber → bronze) to sit on the bronze background.
use ratatui::{
    buffer::Buffer,
    layout::Rect,
    style::{Color, Style},
};
use std::sync::OnceLock;

const W: usize = 797;
const GH: usize = 114;
const ORIGINS: [i32; 8] = [12, 108, 205, 300, 397, 492, 589, 684];
const LETTERS: [char; 8] = ['R', 'O', 'M', 'A', 'C', 'O', 'D', 'E'];

type Rect4 = (i32, i32, i32, i32);

fn rects(c: char) -> Vec<Rect4> {
    match c {
        'R' => vec![
            (0, 0, 24, 114),
            (0, 0, 68, 24),
            (60, 14, 84, 54),
            (0, 46, 68, 70),
        ],
        'O' => vec![
            (0, 0, 84, 24),
            (0, 0, 24, 114),
            (60, 0, 84, 114),
            (0, 92, 84, 114),
        ],
        'M' => vec![
            (0, 0, 39, 39),
            (45, 0, 84, 39),
            (39, 14, 45, 39),
            (0, 0, 24, 114),
            (60, 0, 84, 114),
            (30, 39, 54, 84),
        ],
        'A' => vec![
            (0, 0, 84, 24),
            (0, 0, 24, 114),
            (61, 0, 84, 114),
            (0, 46, 84, 68),
        ],
        'C' => vec![(0, 0, 24, 114), (0, 0, 83, 24), (0, 92, 83, 114)],
        'D' => vec![
            (0, 0, 24, 114),
            (0, 0, 68, 24),
            (0, 92, 68, 114),
            (60, 16, 82, 97),
        ],
        'E' => vec![
            (0, 0, 24, 114),
            (0, 0, 82, 24),
            (0, 46, 68, 68),
            (0, 91, 82, 114),
        ],
        _ => vec![],
    }
}

/// Summed-area table of the glyph mask, (W+1) x (GH+1).
fn sat() -> &'static Vec<u32> {
    static SAT: OnceLock<Vec<u32>> = OnceLock::new();
    SAT.get_or_init(|| {
        let mut mask = vec![0u8; W * GH];
        for (i, &ch) in LETTERS.iter().enumerate() {
            for (x0, y0, x1, y1) in rects(ch) {
                for y in y0..y1 {
                    for x in x0..x1 {
                        let (px, py) = (ORIGINS[i] + x, y);
                        if px >= 0 && (px as usize) < W && py >= 0 && (py as usize) < GH {
                            mask[py as usize * W + px as usize] = 1;
                        }
                    }
                }
            }
        }
        // R's diagonal leg (parallelogram), as in the original generator
        for y in 70..GH {
            let t = (y as f32 - 70.0) / 44.0;
            let (l, r) = (42.0 + 18.0 * t, 65.0 + 18.0 * t);
            for x in l.floor() as i32..r.ceil() as i32 {
                let px = ORIGINS[0] + x;
                if px >= 0 && (px as usize) < W {
                    mask[y * W + px as usize] = 1;
                }
            }
        }
        let mut s = vec![0u32; (W + 1) * (GH + 1)];
        for y in 0..GH {
            let mut row = 0u32;
            for x in 0..W {
                row += mask[y * W + x] as u32;
                s[(y + 1) * (W + 1) + x + 1] = s[y * (W + 1) + x + 1] + row;
            }
        }
        s
    })
}

fn coverage(x0: f32, y0: f32, x1: f32, y1: f32) -> f32 {
    let s = sat();
    let cl = |v: f32, m: usize| (v.round().max(0.0) as usize).min(m);
    let (ax, bx) = (cl(x0, W), cl(x1, W));
    let (ay, by) = (cl(y0, GH), cl(y1, GH));
    if bx <= ax || by <= ay {
        return 0.0;
    }
    let g = |x: usize, y: usize| s[y * (W + 1) + x] as f32;
    let sum = g(bx, by) - g(ax, by) - g(bx, ay) + g(ax, ay);
    sum / ((bx - ax) as f32 * (by - ay) as f32)
}

fn hash(x: i32, y: i32) -> f32 {
    let mut h = (x as u32)
        .wrapping_mul(374761393)
        .wrapping_add((y as u32).wrapping_mul(668265263));
    h = (h ^ (h >> 13)).wrapping_mul(1274126177);
    h ^= h >> 16;
    (h & 0xFFFF) as f32 / 65535.0
}

/// Warm ramp: cream highlight → amber → bronze, plus a faint left→right drift.
fn ramp(yf: f32, xf: f32, sweep: f32) -> [f32; 3] {
    const STOPS: [(f32, [f32; 3]); 4] = [
        (0.0, [252.0, 232.0, 200.0]),
        (0.38, [246.0, 178.0, 96.0]),
        (0.74, [226.0, 118.0, 52.0]),
        (1.0, [168.0, 78.0, 38.0]),
    ];
    let y = yf.clamp(0.0, 1.0);
    let mut c = STOPS[3].1;
    for i in 0..3 {
        let (a, ca) = STOPS[i];
        let (b, cb) = STOPS[i + 1];
        if y <= b {
            let t = ((y - a) / (b - a)).clamp(0.0, 1.0);
            for k in 0..3 {
                c[k] = ca[k] + (cb[k] - ca[k]) * t;
            }
            break;
        }
    }
    // horizontal drift toward amber on the right
    c[1] += (xf - 0.5) * 10.0;
    c[2] -= (xf - 0.5) * 8.0;
    // slow sheen sweeping across the letters
    let d = (xf - sweep).abs();
    let glow = (1.0 - d * 7.0).max(0.0);
    for k in 0..3 {
        c[k] += glow * 38.0;
    }
    c
}

/// Size in cells (cols, rows) the logo occupies when drawn `cols` wide.
pub fn size_for(cols: u16) -> (u16, u16) {
    let s = W as f32 / cols.max(1) as f32;
    let half_rows = (GH as f32 / s).ceil();
    (cols, ((half_rows / 2.0).ceil() as u16).max(1))
}

/// Draw the wordmark centred in `area`, `cols` cells wide. `t` is seconds.
pub fn draw(buf: &mut Buffer, area: Rect, cols: u16, t: f32) {
    let cols = cols.min(area.width).max(8);
    let (cw, ch) = size_for(cols);
    let ox = area.x + (area.width.saturating_sub(cw)) / 2;
    let oy = area.y + (area.height.saturating_sub(ch)) / 2;
    let s = W as f32 / cw as f32; // source px per cell column == per half-row
    let sweep = ((t * 0.16) % 1.6) - 0.3;
    for cy in 0..ch {
        for cx in 0..cw {
            let (x, y) = (ox + cx, oy + cy);
            if x >= buf.area.right() || y >= buf.area.bottom() {
                continue;
            }
            let sx0 = cx as f32 * s;
            let sx1 = sx0 + s;
            let top = coverage(sx0, (cy * 2) as f32 * s, sx1, (cy * 2 + 1) as f32 * s);
            let bot = coverage(sx0, (cy * 2 + 1) as f32 * s, sx1, (cy * 2 + 2) as f32 * s);
            if top < 0.02 && bot < 0.02 {
                continue;
            }
            let cell = buf.get_mut(x, y);
            let bg = match cell.bg {
                Color::Rgb(r, g, b) => [r as f32, g as f32, b as f32],
                _ => [30.0, 22.0, 14.0],
            };
            let xf = sx0 / W as f32;
            let grain = (hash(x as i32, y as i32) - 0.5) * 14.0;
            let paint = |cov: f32, row: f32| -> [f32; 3] {
                let yf = (row * s) / GH as f32;
                let mut c = ramp(yf, xf, sweep);
                for k in 0..3 {
                    c[k] = (c[k] + grain).clamp(0.0, 255.0);
                }
                let mut o = [0f32; 3];
                for k in 0..3 {
                    o[k] = bg[k] * (1.0 - cov) + c[k] * cov;
                }
                o
            };
            let tc = paint(top, (cy * 2) as f32 + 0.5);
            let bc = paint(bot, (cy * 2 + 1) as f32 + 0.5);
            let rgb = |c: [f32; 3]| Color::Rgb(c[0] as u8, c[1] as u8, c[2] as u8);
            cell.set_symbol("▀");
            cell.set_style(Style::new().fg(rgb(tc)).bg(rgb(bc)));
        }
    }
}
