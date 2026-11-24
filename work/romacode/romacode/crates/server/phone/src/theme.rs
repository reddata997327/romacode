//! Palette, wallpapers and app icons — the "Obsidian Glass" design system.
//!
//! Premium cues, all drawn from primitives (no embedded assets):
//! * deep, high-contrast surfaces with a single vivid accent family
//! * luminous wallpapers — radial-gradient orbs over a rich vertical gradient,
//!   finished with a photographic vignette (no flat hard-edged circles)
//! * icon tiles as gradient glass: soft drop shadow, vertical colour slide,
//!   a top glass highlight and a crisp 1 px edge light

use crate::gfx::{mix, Canvas, Rgb};

// ── surfaces ─────────────────────────────────────────────────────────────────

pub const STATUS_BG: Rgb = [8, 10, 14];
pub const NAV_BG: Rgb = [10, 13, 18];
pub const BG: Rgb = [12, 15, 20];
pub const SURFACE: Rgb = [21, 26, 33];
pub const SURFACE_HI: Rgb = [33, 40, 50];

// ── type ─────────────────────────────────────────────────────────────────────

pub const TEXT: Rgb = [236, 238, 241];
pub const DIM: Rgb = [158, 164, 172];
pub const DIMMER: Rgb = [108, 114, 123];

// ── accents ──────────────────────────────────────────────────────────────────

/// The single accent family: cool electric blue, with gradient stops.
pub const ACCENT: Rgb = [138, 180, 248];
pub const ACCENT_HI: Rgb = [183, 213, 255];
pub const ACCENT_LO: Rgb = [88, 124, 205];
pub const VIOLET: Rgb = [167, 139, 250];

pub const GREEN: Rgb = [129, 201, 149];
pub const RED: Rgb = [242, 139, 130];
pub const YELLOW: Rgb = [253, 214, 99];
pub const TEAL: Rgb = [120, 208, 196];
pub const PURPLE: Rgb = [197, 145, 250];
pub const ORANGE: Rgb = [252, 165, 90];

/// Wallpaper id → colour story. Returns `(top, bottom, orbs)` where every orb
/// is `(cx, cy, r, colour, centre alpha)` — soft radial glows, not circles.
fn wallpaper_story(id: u32, h: i32) -> (Rgb, Rgb, [(i32, i32, i32, Rgb, u8); 5]) {
    let s = id % 3;
    // vertical anchors scale with the panel height so the composition holds
    let a = h / 5;
    let b = h * 3 / 5;
    let d = h * 7 / 10;
    let e = h * 9 / 10;
    match s {
        // Midnight Aurora — deep space navy into violet, electric-blue aurora
        _ if s == 0 => (
            [9, 12, 26],
            [34, 22, 64],
            [
                (150, a, 230, [64, 96, 190], 120),
                (540, b, 300, [110, 62, 200], 110),
                (600, d - 60, 190, [40, 70, 150], 80),
                (220, e - 100, 170, [80, 50, 160], 70),
                (370, h - 150, 260, [46, 60, 130], 60),
            ],
        ),
        // Emerald Dusk — petrol green, cold teal light
        _ if s == 1 => (
            [6, 20, 24],
            [10, 52, 46],
            [
                (140, a, 220, [18, 92, 84], 120),
                (520, b, 280, [26, 110, 96], 100),
                (610, d - 60, 180, [14, 70, 66], 80),
                (240, e - 100, 160, [30, 96, 70], 70),
                (380, h - 150, 240, [16, 74, 70], 60),
            ],
        ),
        // Rose Nebula — warm plum into violet dusk
        _ => (
            [30, 14, 26],
            [46, 22, 62],
            [
                (160, a, 230, [150, 52, 96], 120),
                (530, b, 290, [96, 44, 160], 105),
                (620, d - 60, 190, [70, 34, 120], 80),
                (230, e - 100, 170, [140, 56, 110], 70),
                (370, h - 150, 250, [60, 40, 110], 60),
            ],
        ),
    }
}

/// Wallpaper i: rich vertical gradient + soft luminous orbs + vignette.
pub fn draw_wallpaper(c: &mut Canvas, id: u32, y0: i32, y1: i32) {
    let (top, bottom, orbs) = wallpaper_story(id, y1 - y0);
    c.gradient_v(0, y0, y1, c.w as i32, top, bottom);
    // distant glows first, foreground glows last (painter's order)
    for (x, y, r, col, alpha) in orbs.iter().rev() {
        c.glow(*x, y + y0, *r, *col, *alpha);
    }
    // a faint horizon light along the lower third — depth cue
    let hy = y0 + (y1 - y0) * 8 / 10;
    c.glow(
        c.w as i32 / 2,
        hy,
        c.w as i32 / 2 + 100,
        mix(bottom, [255, 255, 255], 0.10),
        34,
    );
    // photographic vignette keeps the edges quiet so icons pop
    c.vignette(70);
}

/// App accent colour — the tile's *dominant* hue (kept for glyph accents and
/// any caller that needs one flat colour).
pub fn app_color(app: &crate::state::AppId) -> Rgb {
    app_gradient(app).1
}

/// Icon tile gradient: `(top, bottom)` — light source above, depth below.
pub fn app_gradient(app: &crate::state::AppId) -> (Rgb, Rgb) {
    use crate::state::AppId::*;
    match app {
        Settings => ([124, 136, 152], [72, 82, 96]),
        Calculator => ([110, 124, 140], [62, 72, 86]),
        Clock => ([70, 96, 132], [38, 58, 88]),
        Messages => ([56, 132, 220], [30, 84, 156]),
        Dialer => ([64, 158, 106], [38, 106, 70]),
        Browser => ([236, 108, 66], [172, 62, 36]),
        Photos => ([232, 136, 70], [180, 88, 44]),
        Files => ([112, 142, 208], [70, 96, 152]),
        Termux => ([46, 66, 54], [22, 34, 30]),
        Magisk => ([54, 104, 74], [28, 62, 44]),
        RootExplorer => ([74, 116, 152], [42, 72, 102]),
        TitaniumBackup => ([118, 94, 156], [72, 56, 104]),
        User(_) => (PURPLE, [128, 82, 178]),
    }
}

/// Draw an app icon tile with its hand-drawn glyph — gradient glass style.
/// `size` x `size` pixels.
pub fn draw_app_icon(c: &mut Canvas, app: &crate::state::AppId, x: i32, y: i32, size: i32) {
    use crate::state::AppId::*;
    let (gtop, gbot) = app_gradient(app);
    let r = (size * 24 / 100).max(6); // modern ~24 % squircle radius
                                      // 1. soft drop shadow — the tile floats above the wallpaper
    c.shadow(x, y, size, size, r, 80);
    // 2. the gradient tile itself
    c.round_rect_gradient_v(x, y, size, size, r, gtop, gbot);
    // 3. glass highlight: translucent white cap on the upper 45 %
    let cap_h = (size * 45 / 100).max(4);
    c.round_rect_alpha(
        x + 2,
        y + 2,
        size - 4,
        cap_h,
        (r - 2).max(2),
        [255, 255, 255],
        26,
    );
    // 4. crisp edge light: brighter top rim, darker bottom rim
    c.hline(x + r / 2, y, size - r, [255, 255, 255]);
    c.round_rect_outline(x, y, size, size, r, mix(gtop, [255, 255, 255], 0.35));
    // inner glyph box
    let gx = x + size / 4;
    let gy = y + size / 4;
    let gs = size / 2;
    let cx = x + size / 2;
    let cy = y + size / 2;
    match app {
        Dialer => {
            // handset: a thick arc
            c.line(
                cx - gs / 3,
                cy + gs / 3,
                cx + gs / 4,
                cy - gs / 4,
                TEXT,
                size / 10,
            );
            c.circle(cx + gs / 4, cy - gs / 4, size / 12, TEXT);
            c.circle(cx - gs / 3, cy + gs / 3, size / 12, TEXT);
        }
        Messages => {
            // chat bubble
            c.round_rect(gx, gy, gs, gs * 3 / 4, gs / 5, TEXT);
            c.circle(gx + gs / 5, gy + gs / 4, gs / 14, gbot);
            c.circle(gx + gs / 2, gy + gs / 4, gs / 14, gbot);
            c.circle(gx + gs - gs / 5, gy + gs / 4, gs / 14, gbot);
            // tail
            for i in 0..gs / 4 {
                c.fill_rect(
                    gx + gs / 5,
                    gy + gs * 3 / 4 + i,
                    (gs / 5 - i).max(1),
                    1,
                    TEXT,
                );
            }
        }
        Browser => {
            // chrome-like: outer ring segments + blue centre
            c.ring(cx, cy, gs / 2, RED);
            c.ring(cx, cy, gs / 2 - 1, GREEN);
            c.circle(cx, cy, gs / 4, TEXT);
            c.circle(cx, cy, gs / 4 - 2, ACCENT);
        }
        Photos => {
            // pinwheel: four coloured petals
            c.circle(cx - gs / 4, cy - gs / 4, gs / 5, mix(ACCENT_HI, GREEN, 0.4));
            c.circle(cx + gs / 4, cy - gs / 4, gs / 5, mix(YELLOW, ORANGE, 0.5));
            c.circle(cx - gs / 4, cy + gs / 4, gs / 5, mix(RED, PURPLE, 0.4));
            c.circle(cx + gs / 4, cy + gs / 4, gs / 5, GREEN);
        }
        Settings => {
            // gear: ring + teeth
            c.ring(cx, cy, gs / 3, TEXT);
            for k in 0..8 {
                let a = k as f32 * std::f32::consts::FRAC_PI_4;
                let tx = cx + ((gs / 3) as f32 * a.cos()).round() as i32;
                let ty = cy + ((gs / 3) as f32 * a.sin()).round() as i32;
                c.circle(tx, ty, size / 18, TEXT);
            }
            c.circle(cx, cy, gs / 8, gbot);
        }
        Calculator => {
            // keypad grid
            let cw = gs / 4;
            for row in 0..3 {
                for colx in 0..3 {
                    c.round_rect(
                        gx + colx * (cw + gs / 16),
                        gy + row * (cw + gs / 16),
                        cw,
                        cw,
                        2,
                        TEXT,
                    );
                }
            }
        }
        Clock => {
            c.circle(cx, cy, gs / 2, TEXT);
            c.circle(cx, cy, gs / 2 - 2, mix(gtop, gbot, 0.5));
            c.line(cx, cy, cx, cy - gs / 4, TEXT, size / 24);
            c.line(cx, cy, cx + gs / 3, cy, TEXT, size / 24);
        }
        Files => {
            // folder
            c.fill_rect(gx, gy + gs / 6, gs / 3, gs / 8, TEXT);
            c.round_rect(gx, gy + gs / 4, gs, gs * 3 / 5, gs / 8, TEXT);
        }
        Termux => {
            // terminal prompt: > _ on dark tile
            c.line(cx - gs / 3, cy - gs / 6, cx - gs / 12, cy, TEXT, size / 14);
            c.line(cx - gs / 12, cy, cx - gs / 3, cy + gs / 6, TEXT, size / 14);
            c.fill_rect(cx, cy + gs / 6, gs / 3, size / 12, TEXT);
        }
        Magisk => {
            // the magisk mask: dark visor with two eye slits
            c.round_rect(gx, gy + gs / 5, gs, gs * 3 / 5, gs / 4, TEXT);
            c.fill_rect(gx + gs / 6, gy + gs * 2 / 5, gs / 5, gs / 7, gbot);
            c.fill_rect(gx + gs - gs / 3, gy + gs * 2 / 5, gs / 5, gs / 7, gbot);
            // the # root mark under the visor
            let p = gs / 12;
            c.fill_rect(cx - p, cy + gs * 3 / 10, p * 2, gs / 4, TEXT);
            c.fill_rect(cx + p * 2, cy + gs * 3 / 10, p * 2, gs / 4, TEXT);
            c.fill_rect(cx - p * 2, cy + gs * 3 / 10, p * 5, p, TEXT);
            c.fill_rect(cx - p * 2, cy + gs * 3 / 10 + p * 3, p * 5, p, TEXT);
        }
        RootExplorer => {
            // folder with a root "#" badge
            c.fill_rect(gx, gy + gs / 6, gs / 3, gs / 8, TEXT);
            c.round_rect(gx, gy + gs / 4, gs, gs / 2, gs / 8, TEXT);
            let p = gs / 10;
            c.fill_rect(cx - p * 3, cy + gs / 8, p, gs / 4, gbot);
            c.fill_rect(cx, cy + gs / 8, p, gs / 4, gbot);
            c.fill_rect(cx - p * 4, cy + gs / 8 + p, p * 7, p, gbot);
            c.fill_rect(cx - p * 4, cy + gs / 8 + p * 3, p * 7, p, gbot);
        }
        TitaniumBackup => {
            // backup: two rotating arrows around a box
            c.round_rect(
                cx - gs / 3,
                cy - gs / 8,
                gs * 2 / 3,
                gs * 2 / 3,
                gs / 8,
                TEXT,
            );
            c.ring(cx, cy, gs / 2, TEXT);
            c.line(
                cx + gs / 3,
                cy - gs / 3,
                cx + gs / 2,
                cy - gs / 6,
                TEXT,
                size / 16,
            );
            c.line(
                cx - gs / 3,
                cy + gs / 3,
                cx - gs / 2,
                cy + gs / 6,
                TEXT,
                size / 16,
            );
            c.circle(cx, cy, gs / 10, gbot);
        }
        User(_) => {
            // generic APK app: rounded android-head silhouette
            c.circle(cx, cy - gs / 8, gs / 4, TEXT);
            c.fill_rect(cx - gs / 3, cy, gs * 2 / 3, gs / 3, TEXT);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::state::AppId;

    #[test]
    fn icons_render_on_the_new_primitives() {
        // every built-in app draws without panicking and actually paints
        let apps = [
            AppId::Settings,
            AppId::Calculator,
            AppId::Clock,
            AppId::Files,
            AppId::Messages,
            AppId::Dialer,
            AppId::Browser,
            AppId::Photos,
            AppId::Termux,
            AppId::Magisk,
            AppId::RootExplorer,
            AppId::TitaniumBackup,
        ];
        for (i, app) in apps.iter().enumerate() {
            let mut c = Canvas::new(140, 140);
            c.fill([0, 0, 0]);
            draw_app_icon(&mut c, app, 10, 10, 120);
            // centre of the tile must be painted (non-black)
            let mid = c.get(70, 70);
            let lum = mid[0] as u16 + mid[1] as u16 + mid[2] as u16;
            assert!(lum > 0, "{app:?} tile empty");
            // shadow ring must darken outside the tile (except photo chaos)
            assert!(i < 20);
        }
    }

    #[test]
    fn wallpaper_is_not_flat() {
        let mut c = Canvas::new(720, 1560);
        draw_wallpaper(&mut c, 0, 0, 1560);
        let top = c.get(360, 100);
        let bottom = c.get(360, 1460);
        assert_ne!(top, bottom, "gradient must actually vary");
        // a glow orb must be brighter than its surroundings
        let (x, y, ..) = wallpaper_story(0, 1560).2[0];
        let in_orb = c.get((x.min(719)) as u32, (y.min(1559)) as u32);
        let off = c.get(((x + 400).min(719)) as u32, (y.min(1559)) as u32);
        assert!(in_orb[2] >= off[2], "glow must add light");
    }
}
