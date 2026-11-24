//! Phone rendering — draws a device chassis (rim, bezel, speaker, camera,
//! side buttons) and the phone's live RGBA framebuffer inside it.
//!
//! Frame → cells: the screen is point-sampled to a 2 × 2 sub-pixel grid per
//! terminal cell, then every cell picks the best two-colour quadrant glyph
//! (`▘ ▝ ▀ ▖ ▌ ▞ ▛ …`). Compared with plain half-blocks this doubles the
//! horizontal resolution, so text and icons stay crisp instead of smearing.
//!
//! Sampling: **one exact device pixel per grid point**, taken at the centre
//! of that point's source footprint with pure integer math. The previous
//! path ran the frame through a box filter first, which averaged ~15×15
//! device pixels per grid point at these sizes and turned the live view into
//! mush; a point sample keeps every edge crisp — a real pixel render, not a
//! smear. When the grid happens to match the frame size the formula
//! degenerates to an exact 1:1 copy.
//!
//! Geometry: a terminal cell is ~1 : 2 (w : h), so a cell is 2 device pixels
//! wide on screen per 1 px tall; the screen rect is sized so the device
//! aspect ratio comes out right (never cropped, never stretched) and the
//! **whole frame** is always visible edge to edge.

use super::draw::*;
use ratatui::{
    buffer::Buffer,
    layout::Rect,
    style::{Color, Modifier, Style},
};
use unicode_width::UnicodeWidthStr;

/// Cell height / cell width of a typical terminal font.
const CELL_ASPECT: f64 = 2.0;
/// Chassis thickness around the screen, in cells (rim + bezel).
pub const INSET_X: u16 = 2;
pub const INSET_Y: u16 = 2;

const BODY: Color = Color::Rgb(12, 13, 16);
const RIM: Color = Color::Rgb(78, 82, 92);
const RIM_HI: Color = Color::Rgb(122, 127, 140);
const SPEAKER: Color = Color::Rgb(38, 40, 46);
const LENS: Color = Color::Rgb(34, 42, 66);
const OFF_SCREEN: Color = Color::Rgb(6, 7, 9);

/// Quadrant glyphs indexed by mask: bit0 = top-left, bit1 = top-right,
/// bit2 = bottom-left, bit3 = bottom-right.
const QUADS: [&str; 16] = [
    " ", "▘", "▝", "▀", "▖", "▌", "▞", "▛", "▗", "▚", "▐", "▜", "▄", "▙", "▟", "█",
];

type Rgb = [u8; 3];

/// Where the chassis and its screen sit inside an area.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Layout {
    pub outer: Rect,
    pub screen: Rect,
}

/// Largest aspect-correct phone that fits `area`, centered.
/// `aspect` is device width / height (e.g. 720 / 1560).
pub fn layout(area: Rect, aspect: f64) -> Option<Layout> {
    let aspect = if aspect.is_finite() && aspect > 0.1 && aspect < 4.0 {
        aspect
    } else {
        720.0 / 1560.0
    };
    if area.width < 2 * INSET_X + 6 || area.height < 2 * INSET_Y + 4 {
        return None;
    }
    let avail_w = area.width - 2 * INSET_X;
    let avail_h = area.height - 2 * INSET_Y;
    // one cell row is CELL_ASPECT cell-widths tall
    let mut sh = avail_h;
    let mut sw = (aspect * CELL_ASPECT * sh as f64).round() as u16;
    if sw > avail_w {
        sw = avail_w;
        sh = ((sw as f64) / (aspect * CELL_ASPECT)).round() as u16;
    }
    let (sw, sh) = (sw.max(4), sh.clamp(2, avail_h));
    let (ow, oh) = (sw + 2 * INSET_X, sh + 2 * INSET_Y);
    let ox = area.x + (area.width - ow) / 2;
    let oy = area.y + (area.height - oh) / 2;
    Some(Layout {
        outer: Rect::new(ox, oy, ow, oh),
        screen: Rect::new(ox + INSET_X, oy + INSET_Y, sw, sh),
    })
}

/// Paint the chassis and the live screen. Returns the screen rect (what
/// mouse taps are mapped against). `frame = None` shows a blank powered-on
/// screen with `waiting` as its caption.
pub fn paint_phone(
    buf: &mut Buffer,
    area: Rect,
    frame: Option<&crate::device::Frame>,
    display: (u32, u32),
    waiting: (&str, &str),
) -> Rect {
    let aspect = match frame {
        Some(f) if f.w > 0 && f.h > 0 => f.w as f64 / f.h as f64,
        _ if display.0 > 0 && display.1 > 0 => display.0 as f64 / display.1 as f64,
        _ => 0.0,
    };
    let Some(lay) = layout(area, aspect) else {
        return area;
    };
    paint_chassis(buf, lay);
    match frame {
        Some(f) if f.w > 0 && f.h > 0 => paint_screen(buf, lay.screen, f),
        _ => paint_waiting(buf, lay.screen, waiting.0, waiting.1),
    }
    lay.screen
}

// ── chassis ─────────────────────────────────────────────────────────────

fn paint_chassis(buf: &mut Buffer, lay: Layout) {
    let o = lay.outer;
    let clip = buf.area;
    fill(buf, o, BODY);

    let rim = Style::new().fg(RIM).bg(BODY);
    let (x0, y0, x1, y1) = (o.x, o.y, o.right() - 1, o.bottom() - 1);
    put(buf, x0, y0, "╭", rim, clip);
    put(buf, x1, y0, "╮", rim, clip);
    put(buf, x0, y1, "╰", rim, clip);
    put(buf, x1, y1, "╯", rim, clip);
    for x in x0 + 1..x1 {
        put(buf, x, y0, "─", rim, clip);
        put(buf, x, y1, "─", rim, clip);
    }
    for y in y0 + 1..y1 {
        put(buf, x0, y, "│", rim, clip);
        put(buf, x1, y, "│", rim, clip);
    }

    // side buttons on the right edge: volume rocker, then power
    let h = o.height as f64;
    let btn = Style::new().fg(RIM_HI).bg(BODY);
    for (from, to) in [(0.17, 0.26), (0.31, 0.38)] {
        let (a, b) = (
            (h * from) as u16,
            ((h * to) as u16).max((h * from) as u16 + 1),
        );
        for y in a.max(2)..b.min(o.height.saturating_sub(2)) {
            put(buf, x1, y0 + y, "┃", btn, clip);
        }
    }

    // earpiece slot + front camera in the top bezel
    let top = y0 + 1;
    let cx = o.x + o.width / 2;
    let slot = Style::new().fg(SPEAKER).bg(BODY);
    put(buf, cx.saturating_sub(3), top, "▬▬▬▬▬▬", slot, clip);
    put(buf, cx + 5, top, "●", Style::new().fg(LENS).bg(BODY), clip);
}

// ── live screen ─────────────────────────────────────────────────────────

fn paint_screen(buf: &mut Buffer, screen: Rect, frame: &crate::device::Frame) {
    let (gw, gh) = (screen.width as u32 * 2, screen.height as u32 * 2);
    if gw == 0 || gh == 0 {
        return;
    }
    let body = [12u8, 13, 16];

    // physical size of one grid pixel (cell = 8 × 16 → grid px = 4 × 8)
    let (pw, ph) = (4.0, 4.0 * CELL_ASPECT);
    let (wmax, hmax) = (gw as f64 * pw, gh as f64 * ph);
    let radius = (wmax * 0.085).min(hmax / 2.0);

    let at = |gx: u32, gy: u32| -> Rgb {
        if outside_corner(gx, gy, pw, ph, wmax, hmax, radius) {
            return body;
        }
        // SHARP: one exact device pixel at the footprint centre — never an
        // average. (`scale_rgba`'s box filter smeared 15×15 px into each
        // grid point and made the live view blurry.)
        let (sx, sy) = src_xy(gx, gy, gw, gh, frame.w, frame.h);
        sample_px(frame, sx, sy)
    };

    for row in 0..screen.height as u32 {
        let y = screen.y as u32 + row;
        if y >= buf.area.bottom() as u32 {
            break;
        }
        for col in 0..screen.width as u32 {
            let x = screen.x as u32 + col;
            if x >= buf.area.right() as u32 {
                break;
            }
            let quad = [
                at(col * 2, row * 2),
                at(col * 2 + 1, row * 2),
                at(col * 2, row * 2 + 1),
                at(col * 2 + 1, row * 2 + 1),
            ];
            let (mask, fg, bg) = best_quadrant(quad);
            let cell = buf.get_mut(x as u16, y as u16);
            cell.set_symbol(QUADS[mask]);
            cell.set_fg(Color::Rgb(fg[0], fg[1], fg[2]));
            cell.set_bg(Color::Rgb(bg[0], bg[1], bg[2]));
        }
    }
}

/// Centre-of-footprint point sample of target pixel `(x, y)` on a
/// `dw × dh` target of a `fw × fh` source. With `fw == dw` (and `fh == dh`)
/// this is the identity map. Exact integer math, no rounding drift.
#[inline]
pub fn src_xy(x: u32, y: u32, dw: u32, dh: u32, fw: u32, fh: u32) -> (u32, u32) {
    let sx = (((2 * x as u64 + 1) * fw as u64) / (2 * dw.max(1) as u64)) as u32;
    let sy = (((2 * y as u64 + 1) * fh as u64) / (2 * dh.max(1) as u64)) as u32;
    (sx.min(fw.saturating_sub(1)), sy.min(fh.saturating_sub(1)))
}

/// Bounds-checked RGBA read.
#[inline]
fn sample_px(frame: &crate::device::Frame, x: u32, y: u32) -> Rgb {
    if x >= frame.w || y >= frame.h {
        return [0, 0, 0];
    }
    let i = ((y as u64 * frame.w as u64 + x as u64) * 4) as usize;
    match frame.rgba.get(i..i + 3) {
        Some(p) => [p[0], p[1], p[2]],
        None => [0, 0, 0],
    }
}

/// True when grid pixel (gx, gy) lies in a rounded display corner's cut-off.
fn outside_corner(gx: u32, gy: u32, pw: f64, ph: f64, w: f64, h: f64, r: f64) -> bool {
    let x = (gx as f64 + 0.5) * pw;
    let y = (gy as f64 + 0.5) * ph;
    let cx = if x < r {
        r
    } else if x > w - r {
        w - r
    } else {
        return false;
    };
    let cy = if y < r {
        r
    } else if y > h - r {
        h - r
    } else {
        return false;
    };
    let (dx, dy) = (x - cx, y - cy);
    dx * dx + dy * dy > r * r
}

/// Choose the two-colour split of a 2 × 2 block with the least error.
/// Returns (glyph mask, foreground, background).
fn best_quadrant(p: [Rgb; 4]) -> (usize, Rgb, Rgb) {
    let mean = |idx: &[usize]| -> Rgb {
        let n = idx.len().max(1) as u32;
        let mut s = [0u32; 3];
        for &i in idx {
            for c in 0..3 {
                s[c] += p[i][c] as u32;
            }
        }
        [(s[0] / n) as u8, (s[1] / n) as u8, (s[2] / n) as u8]
    };
    let err = |idx: &[usize], m: Rgb| -> u32 {
        idx.iter()
            .map(|&i| {
                (0..3)
                    .map(|c| (p[i][c] as i32 - m[c] as i32).pow(2) as u32)
                    .sum::<u32>()
            })
            .sum()
    };

    let all = [0usize, 1, 2, 3];
    let flat = mean(&all);
    let mut best = (0usize, flat, flat);
    let mut best_err = err(&all, flat);
    if best_err == 0 {
        return best;
    }
    // masks 1..=7 cover all seven two-group partitions (bit 3 stays in bg)
    for mask in 1usize..8 {
        let fg_i: Vec<usize> = (0..4).filter(|b| mask & (1 << b) != 0).collect();
        let bg_i: Vec<usize> = (0..4).filter(|b| mask & (1 << b) == 0).collect();
        let (fg, bg) = (mean(&fg_i), mean(&bg_i));
        let e = err(&fg_i, fg) + err(&bg_i, bg);
        if e < best_err {
            best_err = e;
            best = (mask, fg, bg);
        }
    }
    best
}

// ── hit-testing ─────────────────────────────────────────────────────────

/// Map a terminal cell inside `screen` back to device pixel coordinates.
/// The cell's device footprint centre on the same sampling grid
/// [`paint_screen`] used — clicks land exactly where the pixels came from.
pub fn to_device(screen: Rect, frame: &crate::device::Frame, col: u16, row: u16) -> (u32, u32) {
    let gw = (screen.width.max(1) as u32) * 2;
    let gh = (screen.height.max(1) as u32) * 2;
    let gx = ((col.saturating_sub(screen.x)) as u32) * 2 + 1;
    let gy = ((row.saturating_sub(screen.y)) as u32) * 2 + 1;
    let (dx, dy) = src_xy(gx, gy, gw, gh, frame.w, frame.h);
    (
        dx.min(frame.w.saturating_sub(1)),
        dy.min(frame.h.saturating_sub(1)),
    )
}

// ── placeholder ─────────────────────────────────────────────────────────

/// Dark powered-on screen with a two-line caption (before the first frame).
pub fn paint_waiting(buf: &mut Buffer, area: Rect, label: &str, sub: &str) {
    fill(buf, area, OFF_SCREEN);
    let cy = area.y + area.height / 2;
    let caption = |buf: &mut Buffer, y: u16, text: &str, style: Style| {
        let text = truncate(text, area.width as usize);
        let w = text.width() as u16;
        put(
            buf,
            area.x + area.width.saturating_sub(w) / 2,
            y,
            &text,
            style,
            area,
        );
    };
    caption(
        buf,
        cy,
        label,
        Style::new()
            .fg(Color::Rgb(154, 160, 166))
            .add_modifier(Modifier::BOLD),
    );
    caption(buf, cy + 1, sub, Style::new().fg(Color::Rgb(106, 112, 120)));
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;

    const FW: u32 = 720;
    const FH: u32 = 1560;

    /// Left half blue, right half red — a horizontal crop is detectable.
    fn frame() -> crate::device::Frame {
        let mut rgba = vec![0u8; (FW * FH * 4) as usize];
        for y in 0..FH {
            for x in 0..FW {
                let i = ((y * FW + x) * 4) as usize;
                let c = if x < FW / 2 { (0, 0, 255) } else { (255, 0, 0) };
                rgba[i] = c.0;
                rgba[i + 1] = c.1;
                rgba[i + 2] = c.2;
                rgba[i + 3] = 255;
            }
        }
        crate::device::Frame {
            gen: 1,
            w: FW,
            h: FH,
            rgba: Arc::new(rgba),
        }
    }

    /// Top half blue, bottom half red — a vertical crop is detectable.
    fn frame_v() -> crate::device::Frame {
        let mut rgba = vec![0u8; (FW * FH * 4) as usize];
        for y in 0..FH {
            for x in 0..FW {
                let i = ((y * FW + x) * 4) as usize;
                let c = if y < FH / 2 { (0, 0, 255) } else { (255, 0, 0) };
                rgba[i] = c.0;
                rgba[i + 1] = c.1;
                rgba[i + 2] = c.2;
                rgba[i + 3] = 255;
            }
        }
        crate::device::Frame {
            gen: 1,
            w: FW,
            h: FH,
            rgba: Arc::new(rgba),
        }
    }

    fn cell_rgb(c: &ratatui::buffer::Cell) -> (Color, Color) {
        (c.fg, c.bg)
    }

    #[test]
    fn layout_keeps_device_aspect_and_fits() {
        let area = Rect::new(0, 0, 120, 52);
        let l = layout(area, 720.0 / 1560.0).unwrap();
        assert!(l.outer.right() <= area.right() && l.outer.bottom() <= area.bottom());
        let phys = (l.screen.width as f64 * 8.0) / (l.screen.height as f64 * 16.0);
        assert!((phys - 720.0 / 1560.0).abs() < 0.02, "aspect {phys}");
    }

    #[test]
    fn layout_narrow_area_is_width_limited() {
        let l = layout(Rect::new(0, 0, 30, 80), 0.46).unwrap();
        assert!(l.outer.width <= 30);
    }

    #[test]
    fn layout_rejects_tiny_areas() {
        assert!(layout(Rect::new(0, 0, 8, 5), 0.46).is_none());
    }

    #[test]
    fn quadrant_picks_exact_split() {
        let (w, k) = ([255u8, 255, 255], [0u8, 0, 0]);
        // left column white, right column black → ▌ (mask 5) fg white bg black
        let (m, fg, bg) = best_quadrant([w, k, w, k]);
        assert_eq!((m, fg, bg), (5, w, k));
        // flat block stays a flat block
        assert_eq!(best_quadrant([w, w, w, w]).0, 0);
    }

    #[test]
    fn corners_are_cut_but_center_is_not() {
        let (pw, ph, w, h, r) = (4.0, 8.0, 400.0, 800.0, 34.0);
        assert!(outside_corner(0, 0, pw, ph, w, h, r));
        assert!(!outside_corner(50, 100, pw, ph, w, h, r));
    }

    #[test]
    fn sampling_is_exact_1_to_1_when_sizes_match() {
        for (x, y) in [(0u32, 0u32), (5, 9), (99, 199)] {
            let (sx, sy) = src_xy(x, y, 100, 200, 100, 200);
            assert_eq!((sx, sy), (x, y), "identity map must hold");
        }
    }

    #[test]
    fn sampling_is_monotonic_and_reaches_both_ends() {
        // At 7:1 downscale point sampling cannot hit every source column
        // (that is what averaging would be for) — but the mapping must be
        // monotonic and must reach both edges of the frame.
        let (dw, dh) = (47u32, 51u32);
        let mut last_y = 0u32;
        for y in 0..dh {
            let mut last_x = 0u32;
            for x in 0..dw {
                let (sx, sy) = src_xy(x * 2, y * 2, dw * 2, dh * 2, FW, FH);
                assert!(sx >= last_x, "x mapping must be monotonic");
                assert!(sy >= last_y, "y mapping must be monotonic");
                last_x = sx;
                last_y = sy;
            }
        }
        let (x0, _) = src_xy(0, 0, dw * 2, dh * 2, FW, FH);
        let (xn, _) = src_xy(dw * 2 - 1, dh * 2 - 1, dw * 2, dh * 2, FW, FH);
        assert!(
            x0 < FW / 10,
            "first sample must be at the frame's left, got {x0}"
        );
        assert!(
            xn > FW * 9 / 10,
            "last sample must be at the frame's right, got {xn}"
        );
        let (_, y0) = src_xy(0, 0, dw * 2, dh * 2, FW, FH);
        let (_, yn) = src_xy(dw * 2 - 1, dh * 2 - 1, dw * 2, dh * 2, FW, FH);
        assert!(y0 < FH / 10);
        assert!(yn > FH * 9 / 10);
    }

    /// THE half-screen regression: the painted screen must span the frame's
    /// full width and height — left edge blue, right edge red, top blue,
    /// bottom red. The old sampler read only x < dw/2 and cropped the right
    /// half of the device out of the live view.
    #[test]
    fn paint_spans_the_full_frame() {
        let fh = frame();
        let fv = frame_v();
        let area = Rect::new(0, 0, 127, 55);
        let mut buf = Buffer::empty(area);
        let screen = paint_phone(&mut buf, area, Some(&fh), (FW, FH), ("", ""));
        assert!(
            screen.width >= 40,
            "screen must be reasonably wide, got {screen:?}"
        );

        // horizontal: left edge blue, right edge red (inset 1 cell for the
        // rounded display corners; quarter column so we never sit on the
        // frame's centre split)
        let mid = screen.y + screen.height / 2;
        let left = buf.get(screen.x + 1, mid);
        let right = buf.get(screen.x + screen.width - 2, mid);
        assert_eq!(
            cell_rgb(left).1,
            Color::Rgb(0, 0, 255),
            "left edge must show the frame's left"
        );
        assert_eq!(
            cell_rgb(right).1,
            Color::Rgb(255, 0, 0),
            "right edge must show the frame's right"
        );

        // vertical split: top edge blue, bottom edge red
        let mut buf2 = Buffer::empty(area);
        let screen2 = paint_phone(&mut buf2, area, Some(&fv), (FW, FH), ("", ""));
        let cxt = screen2.x + screen2.width / 4;
        let top2 = buf2.get(cxt, screen2.y + 1);
        let bot2 = buf2.get(cxt, screen2.y + screen2.height - 2);
        assert_eq!(
            cell_rgb(top2).1,
            Color::Rgb(0, 0, 255),
            "top edge must show the frame's top"
        );
        assert_eq!(
            cell_rgb(bot2).1,
            Color::Rgb(255, 0, 0),
            "bottom edge must show the frame's bottom"
        );
    }

    /// Blur regression: the source has only two colours (white, black), so
    /// every painted cell must be built from pure white / pure black — the
    /// box filter turned the boundary into grey-on-grey mush; point sampling
    /// cannot invent a grey that is not in the source. The 8-px line spans
    /// several sampling points, so the line itself must also be visible.
    #[test]
    fn sampling_invents_no_grey_and_shows_wide_features() {
        // frame: all white except a black column band x = 400..408
        let mut rgba = vec![255u8; (FW * FH * 4) as usize];
        for y in 0..FH {
            for x in 400..408 {
                let i = ((y * FW + x) * 4) as usize;
                rgba[i] = 0;
                rgba[i + 1] = 0;
                rgba[i + 2] = 0;
            }
        }
        let f = crate::device::Frame {
            gen: 1,
            w: FW,
            h: FH,
            rgba: Arc::new(rgba),
        };
        let area = Rect::new(0, 0, 127, 55);
        let mut buf = Buffer::empty(area);
        let screen = paint_phone(&mut buf, area, Some(&f), (FW, FH), ("", ""));
        let mid = screen.y + screen.height / 2;
        let (white, black) = (Color::Rgb(255, 255, 255), Color::Rgb(0, 0, 0));
        let mut line_visible = false;
        for x in screen.x..screen.x + screen.width {
            let c = buf.get(x, mid);
            let is_w = c.fg == white && c.bg == white;
            let is_b = c.fg == black && c.bg == black;
            let is_split = (c.fg == white && c.bg == black) || (c.fg == black && c.bg == white);
            assert!(
                is_w || is_b || is_split,
                "grey mush cell at x={x}: fg={:?} bg={:?}",
                c.fg,
                c.bg
            );
            if is_b || (is_split && c.fg == black) {
                line_visible = true;
            }
        }
        assert!(
            line_visible,
            "the 8-px line must show up in the painted screen"
        );
    }

    #[test]
    fn to_device_covers_the_whole_frame() {
        let f = frame();
        let screen = Rect::new(10, 10, 40, 40);
        let (x0, _) = to_device(screen, &f, screen.x, screen.y);
        let (x1, _) = to_device(screen, &f, screen.x + screen.width - 1, screen.y);
        assert!(x0 < FW / 4, "left edge maps near the frame's left");
        assert!(x1 > FW * 3 / 4, "right edge maps near the frame's right");
        let (_, y0) = to_device(screen, &f, screen.x, screen.y);
        let (_, y1) = to_device(screen, &f, screen.x, screen.y + screen.height - 1);
        assert!(y0 < FH / 4, "top edge maps near the frame's top");
        assert!(y1 > FH * 3 / 4, "bottom edge maps near the frame's bottom");
    }
}
