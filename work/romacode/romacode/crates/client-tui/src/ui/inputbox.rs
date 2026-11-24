//! The orange chatbox shared by the home and chat screens.
use super::draw::*;
use super::theme::*;
use crate::app::{Action, Hit};
use crate::app::ClientState;
use crate::input::Input;
use crate::session::Mode;
use ratatui::{
    buffer::Buffer,
    layout::Rect,
    style::{Modifier, Style},
};
use unicode_width::UnicodeWidthStr;

const MAX_TEXT_LINES: usize = 6;

pub fn height(input: &Input, width: u16) -> u16 {
    let (lines, _) = input.layout(width.saturating_sub(6) as usize);
    lines.len().clamp(1, MAX_TEXT_LINES) as u16 + 1 + 2
}

pub struct Ctx<'a> {
    pub input: &'a Input,
    pub mode: Mode,
    pub cfg: &'a ClientState,
    pub t: f32,
    pub running: bool,
    pub placeholder: &'a str,
    pub inset_bg: (u8, u8, u8),
    pub hover_mode: bool,
}

pub fn draw(buf: &mut Buffer, r: Rect, cx: &Ctx, hits: &mut Vec<Hit>) {
    if r.width < 12 || r.height < 4 {
        return;
    }
    let clip = r;
    // inset background with a warm glow toward the lower right
    let (br, bgc, bb) = cx.inset_bg;
    let base = ratatui::style::Color::Rgb(br, bgc, bb);
    let glow = ratatui::style::Color::Rgb(64, 32, 16);
    for y in r.y + 1..r.bottom() - 1 {
        for x in r.x + 1..r.right() - 1 {
            let u = (x - r.x) as f32 / r.width as f32;
            let v = (y - r.y) as f32 / r.height as f32;
            let k = (u * u) * 0.6 * (0.35 + 0.65 * v);
            let c = buf.get_mut(x, y);
            c.set_symbol(" ");
            c.set_bg(mix(base, glow, k));
        }
    }
    let pulse = if cx.running {
        ((cx.t * 3.0).sin() * 0.5 + 0.5) * 0.45
    } else {
        0.0
    };
    rounded_box(buf, r, Style::new().fg(mix(ORANGE, DIMMER, pulse)));

    let text_x = r.x + 3;
    let text_w = r.width.saturating_sub(6);
    let (lines, (crow, ccol)) = cx.input.layout(text_w as usize);
    let rows_avail = (r.height - 3) as usize;
    let first = if lines.len() > rows_avail {
        (crow + 1)
            .saturating_sub(rows_avail)
            .min(lines.len() - rows_avail)
    } else {
        0
    };
    let blink_on = ((cx.t * 1.9) as u32) % 2 == 0 || cx.input.is_empty();

    if cx.input.is_empty() {
        put(buf, text_x, r.y + 1, "▌", Style::new().fg(ORANGE), clip);
        let ph = if cx.running {
            "Working… esc to interrupt"
        } else {
            cx.placeholder
        };
        put(buf, text_x + 2, r.y + 1, ph, Style::new().fg(DIM), clip);
    } else {
        for (i, line) in lines.iter().skip(first).take(rows_avail).enumerate() {
            put(
                buf,
                text_x,
                r.y + 1 + i as u16,
                line,
                Style::new().fg(TEXT).add_modifier(Modifier::BOLD),
                clip,
            );
        }
        let row = crow.saturating_sub(first);
        if blink_on && row < rows_avail {
            let cxp = text_x + ccol as u16;
            if cxp < r.right() - 1 {
                let cell = buf.get_mut(cxp, r.y + 1 + row as u16);
                let sym = if cell.symbol() == " " || cell.symbol().is_empty() {
                    " ".to_string()
                } else {
                    cell.symbol().to_string()
                };
                cell.set_symbol(&sym);
                cell.set_style(Style::new().fg(BG).bg(ORANGE));
            }
        }
    }

    // mode / model / provider line
    let y = r.bottom() - 2;
    let label = cx.mode.label();
    let hov = if cx.hover_mode {
        Modifier::UNDERLINED
    } else {
        Modifier::empty()
    };
    let mut x = put(buf, text_x, y, label, accent_bold().add_modifier(hov), clip);
    hits.push(Hit {
        rect: Rect::new(text_x, y, label.width() as u16, 1),
        action: Action::ToggleMode,
        tip: Some(("Switch mode", "tab")),
    });
    x = put(buf, x + 2, y, &cx.cfg.effective_model(), bold(), clip);
    put(buf, x + 2, y, &cx.cfg.provider, dim(), clip);
}
