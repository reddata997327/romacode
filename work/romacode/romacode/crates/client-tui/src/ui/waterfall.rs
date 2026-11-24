//! The "working" box: a small dark-space panel with a waterfall (water spills
//! over a dam crest, falls, and the pool at the base flows inward) and, in the
//! sky above it, a star-like dot that behaves like the model's live response —
//! slow pulse while it thinks, fast twinkle while text streams, a spinning
//! star while a tool runs.
use super::draw::{fill, put};
use super::theme::*;
use crate::session::Live;
use ratatui::{buffer::Buffer, layout::Rect, style::{Color, Style}};

/// Box height: sky (star) + dam crest + falling water + pool.
pub const HEIGHT: u16 = 4;
/// Box width — deliberately small; the label sits to its right.
pub const BOX_W: u16 = 14;

fn hash(i: u32) -> f32 {
    let mut h = i.wrapping_mul(2654435761);
    h ^= h >> 15;
    h = h.wrapping_mul(2246822519);
    h ^= h >> 13;
    (h & 0xFFFF) as f32 / 65535.0
}

/// The star dot for a live state: glyph + colour at time `t` (seconds).
pub fn star(live: &Live, t: f32) -> (&'static str, Color) {
    match live {
        Live::Thinking => {
            const F: [&str; 4] = ["·", "✧", "✦", "✧"];
            let i = ((t * 1.6) as usize) % F.len();
            let pulse = (t * 3.2).sin() * 0.5 + 0.5;
            (F[i], mix(DIMMER, WATER_HI, pulse))
        }
        Live::Streaming => {
            const F: [&str; 6] = ["✦", "✶", "✷", "✸", "✷", "✶"];
            let i = ((t * 11.0) as usize) % F.len();
            let flick = (t * 23.0).sin() * 0.5 + 0.5;
            (F[i], mix(AMBER, STAR, flick))
        }
        Live::Tool(_) => {
            const F: [&str; 4] = ["✢", "✣", "✤", "✥"];
            let i = ((t * 7.0) as usize) % F.len();
            (F[i], ORANGE)
        }
        Live::Waiting => {
            let pulse = (t * 2.0).sin() * 0.5 + 0.5;
            ("·", mix(DIMMER, DIM, pulse))
        }
    }
}

/// Short state word for the label next to the box.
pub fn state_word(live: &Live) -> String {
    match live {
        Live::Thinking => "thinking".into(),
        Live::Streaming => "writing".into(),
        Live::Tool(n) => format!("running {n}"),
        Live::Waiting => "working".into(),
    }
}

/// Draw the box with its top-left corner at (x, y). Everything is clipped to
/// `clip`; callers draw it into a scratch buffer so it can scroll partially.
pub fn draw_box(buf: &mut Buffer, x: u16, y: u16, t: f32, live: &Live, clip: Rect) {
    let w = BOX_W;
    fill(buf, Rect::new(x, y, w, HEIGHT), DARKSPACE);

    // static star field with a slow twinkle, drawn first so water covers it
    for r in 0..HEIGHT {
        for i in 0..w {
            let h = hash((i as u32 + 1) * 131 + (r as u32 + 1) * 977);
            if h < 0.09 {
                let tw = ((t * 0.9 + h * 60.0).sin() * 0.5 + 0.5) * 0.6 + 0.15;
                let ch = if h < 0.03 { "∙" } else { "·" };
                put(buf, x + i, y + r, ch, Style::new().fg(mix(DARKSPACE, WATER_HI, tw * 0.55)), clip);
            }
        }
    }

    // the live star dot, centred in the sky row (row 0)
    let (glyph, col) = star(live, t);
    let cx = x + w / 2;
    // a faint halo while text streams
    if matches!(live, Live::Streaming) {
        let halo = Style::new().fg(mix(DARKSPACE, AMBER, 0.45));
        put(buf, cx - 2, y, "·", halo, clip);
        put(buf, cx + 2, y, "·", halo, clip);
    }
    put(buf, cx, y, glyph, Style::new().fg(col), clip);

    // water: inner width leaves a dark margin either side
    let iw = w - 2;
    let wx = x + 1;
    let center = (iw as f32 - 1.0) / 2.0;
    // row 1 — dam crest with an overflowing lip
    for i in 0..iw {
        let wob = ((i as f32 * 0.9 + t * 3.0).sin() * 0.5 + 0.5) * 0.35;
        put(buf, wx + i, y + 1, "▄", Style::new().fg(mix(ORANGE, AMBER, wob)), clip);
    }
    // row 2 — falling water, streaks move downward
    for i in 0..iw {
        let seed = hash(i as u32 * 7 + 3);
        let speed = 5.0 + seed * 4.0;
        let phase = (t * speed + seed * 20.0).rem_euclid(4.0);
        let (ch, k) = match phase {
            p if p < 0.8 => ("█", 1.0),
            p if p < 1.6 => ("▓", 0.8),
            p if p < 2.4 => ("▒", 0.55),
            p if p < 3.2 => ("░", 0.35),
            _ => (" ", 0.0),
        };
        if k > 0.0 {
            put(buf, wx + i, y + 2, ch, Style::new().fg(mix(WATER_HI, WATER_LO, (1.0 - k) * 0.6)), clip);
        }
    }
    // row 3 — pool: ripples travel from both edges toward the centre
    for i in 0..iw {
        let d = (i as f32 - center).abs();
        let wave = (d * 0.8 + t * 4.2).sin() * 0.5 + 0.5;
        let reach = 1.0 - (d / (center + 1.0)) * 0.55;
        let v = wave * reach;
        let ch = match v {
            v if v > 0.75 => "▄",
            v if v > 0.5 => "▃",
            v if v > 0.28 => "▂",
            _ => "▁",
        };
        put(buf, wx + i, y + 3, ch, Style::new().fg(mix(WATER_LO, WATER_HI, v)), clip);
    }
}
