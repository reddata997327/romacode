//! Home screen: bronze background, ROMACODE wordmark, chatbox.
use super::draw::*;
use super::theme::*;
use super::{bg, inputbox, logo};
use crate::app::{Action, App, Hit};
use ratatui::{
    buffer::Buffer,
    layout::Rect,
    style::{Modifier, Style},
};
use unicode_width::UnicodeWidthStr;

pub fn draw(app: &mut App, buf: &mut Buffer, area: Rect) {
    let t = app.t();
    bg::paint(buf, area, t);
    let clip = area;

    let compact = area.height < 20 || area.width < 56;
    let logo_cols = (area.width as u32 * 55 / 100).clamp(40, 78) as u16;
    let logo_cols = logo_cols.min(area.width.saturating_sub(4));
    let (_, lh) = logo::size_for(logo_cols);

    let box_w = (area.width as u32 * 60 / 100).clamp(44, 88) as u16;
    let box_w = box_w.min(area.width.saturating_sub(4));
    let s = &app.sessions[app.active];
    let box_h = inputbox::height(&s.input, box_w);

    let logo_h = if compact { 0 } else { lh };
    let gap = if compact { 0 } else { 3 };
    let total = logo_h + gap + box_h + 2;
    let top = area.y + area.height.saturating_sub(total) / 2;
    let top = top.saturating_sub(area.height / 14);

    if !compact {
        logo::draw(buf, Rect::new(area.x, top, area.width, lh), logo_cols, t);
    }
    let by = top + logo_h + gap;
    let bx = area.x + (area.width.saturating_sub(box_w)) / 2;
    let rect = Rect::new(bx, by, box_w, box_h);
    let hover_mode = app.hover == Some(Action::ToggleMode);
    inputbox::draw(
        buf,
        rect,
        &inputbox::Ctx {
            input: &s.input,
            mode: s.mode,
            cfg: &app.cfg,
            t,
            running: s.is_running(),
            placeholder: "Ask anything…  \"fix the failing test in src/client.rs\"",
            inset_bg: (24, 17, 11),
            hover_mode,
        },
        &mut app.hits,
    );

    // key hints under the box
    let hints: [(&str, &str); 3] = [
        ("tab", "switch mode"),
        ("ctrl+p", "commands"),
        ("ctrl+n", "new session"),
    ];
    let total_w: usize = hints
        .iter()
        .map(|(k, l)| k.width() + 1 + l.width())
        .sum::<usize>()
        + 3 * (hints.len() - 1);
    let mut x = area.x + (area.width.saturating_sub(total_w as u16)) / 2;
    let hy = by + box_h + 1;
    for (i, (k, l)) in hints.iter().enumerate() {
        x = put(
            buf,
            x,
            hy,
            k,
            Style::new().fg(TEXT).add_modifier(Modifier::BOLD),
            clip,
        );
        x = put(buf, x + 1, hy, l, Style::new().fg(DIM), clip);
        if i + 1 < hints.len() {
            x += 3;
        }
    }

    // bottom status line
    let fy = area.bottom().saturating_sub(1);
    put(buf, area.x + 2, fy, &app.cwd, Style::new().fg(DIM), clip);
    let v = concat!("v", env!("CARGO_PKG_VERSION"));
    put(
        buf,
        area.right().saturating_sub(v.width() as u16 + 2),
        fy,
        v,
        Style::new().fg(DIM),
        clip,
    );
    let _ = Hit {
        rect: area,
        action: Action::Refresh,
        tip: None,
    };
}
