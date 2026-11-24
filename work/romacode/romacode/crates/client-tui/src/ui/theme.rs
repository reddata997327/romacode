//! Colour palette, sampled from the RomaCode reference screens.
use ratatui::style::{Color, Modifier, Style};

pub const BG: Color = Color::Rgb(22, 16, 13);
pub const PANEL: Color = Color::Rgb(40, 28, 22);
pub const PANEL_HI: Color = Color::Rgb(52, 37, 29);
pub const HOVER: Color = Color::Rgb(46, 33, 27);
pub const BORDER: Color = Color::Rgb(62, 47, 38);
pub const ORANGE: Color = Color::Rgb(232, 120, 48);
pub const AMBER: Color = Color::Rgb(240, 170, 70);
pub const TEXT: Color = Color::Rgb(244, 232, 218);
pub const DIM: Color = Color::Rgb(150, 130, 114);
pub const DIMMER: Color = Color::Rgb(100, 84, 72);
pub const GREEN: Color = Color::Rgb(142, 204, 112);
pub const RED: Color = Color::Rgb(232, 104, 92);
pub const YELLOW: Color = Color::Rgb(232, 192, 104);
pub const DIFF_ADD: Color = Color::Rgb(34, 64, 36);
pub const DIFF_DEL: Color = Color::Rgb(70, 30, 30);
pub const WATER_HI: Color = Color::Rgb(176, 226, 244);
pub const WATER_LO: Color = Color::Rgb(66, 128, 190);
/// The near-black "space" behind the working box and the queued-query rows.
pub const DARKSPACE: Color = Color::Rgb(9, 8, 17);
pub const STAR: Color = Color::Rgb(255, 236, 190);

pub fn text() -> Style {
    Style::new().fg(TEXT)
}
pub fn bold() -> Style {
    Style::new().fg(TEXT).add_modifier(Modifier::BOLD)
}
pub fn dim() -> Style {
    Style::new().fg(DIM)
}
pub fn dimmer() -> Style {
    Style::new().fg(DIMMER)
}
pub fn accent_bold() -> Style {
    Style::new().fg(ORANGE).add_modifier(Modifier::BOLD)
}

pub fn mix(a: Color, b: Color, t: f32) -> Color {
    let (ar, ag, ab) = rgb(a);
    let (br, bg, bb) = rgb(b);
    let t = t.clamp(0.0, 1.0);
    Color::Rgb(
        (ar as f32 + (br as f32 - ar as f32) * t) as u8,
        (ag as f32 + (bg as f32 - ag as f32) * t) as u8,
        (ab as f32 + (bb as f32 - ab as f32) * t) as u8,
    )
}

pub fn rgb(c: Color) -> (u8, u8, u8) {
    match c {
        Color::Rgb(r, g, b) => (r, g, b),
        _ => (22, 16, 13),
    }
}
