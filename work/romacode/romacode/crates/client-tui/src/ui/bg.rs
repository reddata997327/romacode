//! Bronze "sun morning noon" background, sampled from the reference image.
use ratatui::{buffer::Buffer, layout::Rect, style::Color};

// 9x9 grid sampled from sun-morning-bronze-noon.png (row-major, top→bottom).
const GRID: [[u32; 9]; 9] = [
    [
        0x2c2012, 0x2b1f11, 0x2e2214, 0x2a1e10, 0x2c2012, 0x2a1e10, 0x2c2113, 0x2b1f11, 0x291e10,
    ],
    [
        0x2f2212, 0x302314, 0x2e2214, 0x291d0f, 0x2c2012, 0x2b2012, 0x2b1f12, 0x2b2012, 0x2a1e11,
    ],
    [
        0x352615, 0x332514, 0x322415, 0x2d2112, 0x2a1e10, 0x2d2113, 0x2c2012, 0x2c2113, 0x2b1f11,
    ],
    [
        0x392815, 0x362614, 0x352615, 0x332514, 0x2f2212, 0x2e2214, 0x2c2012, 0x2c2113, 0x291d0f,
    ],
    [
        0x3d2b16, 0x3b2a16, 0x3a2916, 0x372815, 0x342614, 0x302212, 0x2f2213, 0x2b1f10, 0x2a1e11,
    ],
    [
        0x3b2a15, 0x3f2c16, 0x3f2d18, 0x3d2c18, 0x3a2916, 0x342513, 0x332514, 0x312313, 0x2d2011,
    ],
    [
        0x362614, 0x3a2916, 0x3d2b16, 0x3f2d17, 0x3d2b17, 0x3b2a17, 0x372714, 0x362615, 0x342615,
    ],
    [
        0x302313, 0x312312, 0x362714, 0x3c2b17, 0x3d2b16, 0x402d18, 0x3b2915, 0x3c2b18, 0x382815,
    ],
    [
        0x291e11, 0x2a1e0f, 0x312414, 0x332413, 0x372815, 0x3b2a16, 0x3f2d18, 0x3e2c16, 0x3e2d18,
    ],
];

fn unpack(c: u32) -> [f32; 3] {
    [
        ((c >> 16) & 0xff) as f32,
        ((c >> 8) & 0xff) as f32,
        (c & 0xff) as f32,
    ]
}

/// Colour of the background at normalised coordinates (u, v ∈ 0..1).
pub fn sample(u: f32, v: f32) -> Color {
    let fx = (u.clamp(0.0, 1.0)) * 8.0;
    let fy = (v.clamp(0.0, 1.0)) * 8.0;
    let (x0, y0) = (fx.floor() as usize, fy.floor() as usize);
    let (x1, y1) = ((x0 + 1).min(8), (y0 + 1).min(8));
    let (tx, ty) = (fx - x0 as f32, fy - y0 as f32);
    let mut out = [0f32; 3];
    let (a, b, c, d) = (
        unpack(GRID[y0][x0]),
        unpack(GRID[y0][x1]),
        unpack(GRID[y1][x0]),
        unpack(GRID[y1][x1]),
    );
    for k in 0..3 {
        let top = a[k] * (1.0 - tx) + b[k] * tx;
        let bot = c[k] * (1.0 - tx) + d[k] * tx;
        out[k] = top * (1.0 - ty) + bot * ty;
    }
    // gentle vignette so edges settle into the UI background
    let dx = u - 0.5;
    let dy = v - 0.5;
    let vig = 1.0 - (dx * dx + dy * dy).min(0.5) * 0.35;
    Color::Rgb(
        (out[0] * vig) as u8,
        (out[1] * vig) as u8,
        (out[2] * vig) as u8,
    )
}

/// Paint the whole area with the gradient. `t` (seconds) drives a very slow
/// breathing of the warm glow near the bottom.
pub fn paint(buf: &mut Buffer, area: Rect, t: f32) {
    let w = area.width.max(1) as f32;
    let h = area.height.max(1) as f32;
    let breath = 1.0 + (t * 0.35).sin() * 0.03;
    for y in area.y..area.bottom() {
        for x in area.x..area.right() {
            let u = (x - area.x) as f32 / w;
            let v = (y - area.y) as f32 / h;
            let c = sample(u, v);
            let c = match c {
                Color::Rgb(r, g, b) => {
                    let k = 1.0 + (v - 0.4).max(0.0) * (breath - 1.0) * 6.0;
                    Color::Rgb(
                        ((r as f32) * k).min(255.0) as u8,
                        ((g as f32) * k).min(255.0) as u8,
                        ((b as f32) * k).min(255.0) as u8,
                    )
                }
                o => o,
            };
            let cell = buf.get_mut(x, y);
            cell.set_symbol(" ");
            cell.set_bg(c);
        }
    }
}
