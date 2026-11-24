//! Right-hand session panel: usage and populated integration/task/file state.
use super::draw::*;
use super::theme::*;
use super::transcript::{cost, fmt_tokens};
use crate::app::App;
use crate::session::*;
use ratatui::{
    buffer::Buffer,
    layout::Rect,
    style::{Modifier, Style},
};
use unicode_width::UnicodeWidthStr;

pub const WIDTH: u16 = 34;

fn header(buf: &mut Buffer, x: u16, y: u16, w: u16, title: &str, right: &str, clip: Rect) {
    put(buf, x, y, "▾", Style::new().fg(ORANGE), clip);
    put(buf, x + 2, y, title, bold(), clip);
    let rw = right.width() as u16;
    put(buf, x + w.saturating_sub(rw), y, right, dim(), clip);
}

fn divider(buf: &mut Buffer, x: u16, y: u16, w: u16, clip: Rect) {
    hline(buf, x, y, w, "─", Style::new().fg(BORDER), clip);
}

fn row_kv(
    buf: &mut Buffer,
    x: u16,
    y: u16,
    w: u16,
    left: &[(String, Style)],
    right: &[(String, Style)],
    clip: Rect,
) {
    put_spans(buf, x, y, left, clip);
    let rw: usize = right.iter().map(|(s, _)| s.width()).sum();
    put_spans(buf, x + w.saturating_sub(rw as u16), y, right, clip);
}

pub fn draw(app: &App, buf: &mut Buffer, area: Rect) {
    let clip = area;
    fill(buf, area, BG);
    for y in area.y..area.bottom() {
        put(buf, area.x, y, "│", Style::new().fg(BORDER), clip);
    }
    let x = area.x + 2;
    let w = area.width.saturating_sub(4);
    let s = app.session();
    let mut y = area.y + 1;

    // The session stores token high-water marks, not a cumulative context or
    // billing total, so label the meter and configured-rate estimate clearly.
    let total = s.tokens_total();
    let win = app.cfg.context_window.max(1);
    let pct = (total * 100 / win).min(100);
    header(
        buf,
        x,
        y,
        w,
        "Reported Usage",
        &format!("{pct}% peak"),
        clip,
    );
    y += 1;
    put_spans(
        buf,
        x,
        y,
        &[
            (fmt_tokens(total), bold()),
            (format!(" peak / {} context tokens", fmt_tokens(win)), dim()),
        ],
        clip,
    );
    y += 1;
    let filled = ((total as f64 / win as f64) * w as f64)
        .ceil()
        .min(w as f64) as u16;
    let filled = if total > 0 { filled.max(1) } else { 0 };
    for i in 0..w {
        let (c, st) = if i < filled {
            ("━", Style::new().fg(ORANGE))
        } else {
            ("━", Style::new().fg(BORDER))
        };
        put(buf, x + i, y, c, st, clip);
    }
    y += 1;
    put(
        buf,
        x,
        y,
        &format!(
            "~${:.2} peak estimate  ·  {} turns",
            cost(s, &app.cfg),
            s.turns
        ),
        dim(),
        clip,
    );
    y += 2;
    divider(buf, x, y, w, clip);
    y += 2;

    // These sections are rendered only when the session contains actual
    // integration/task/file/server state. Empty model fields are not evidence
    // that the corresponding integration exists or is connected.
    if !s.mcp.is_empty() {
        let on = s.mcp.iter().filter(|m| m.on).count();
        header(
            buf,
            x,
            y,
            w,
            "MCP",
            &format!("{on}/{} on", s.mcp.len()),
            clip,
        );
        y += 1;
        for m in &s.mcp {
            let (dot, c, st) = if m.on {
                ("●", GREEN, text())
            } else {
                ("○", DIMMER, dimmer())
            };
            let right = if m.on {
                format!("{} tools", m.tools)
            } else {
                "off".into()
            };
            row_kv(
                buf,
                x,
                y,
                w,
                &[
                    (format!("{dot} "), Style::new().fg(c)),
                    (m.name.clone(), st),
                ],
                &[(right, if m.on { dim() } else { dimmer() })],
                clip,
            );
            y += 1;
        }
        y += 1;
        divider(buf, x, y, w, clip);
        y += 2;
    }

    if !s.todos.is_empty() {
        let done = s
            .todos
            .iter()
            .filter(|t| t.state == TodoState::Done)
            .count();
        header(
            buf,
            x,
            y,
            w,
            "Todo",
            &format!("{done}/{}", s.todos.len()),
            clip,
        );
        y += 1;
        for t in &s.todos {
            let (icon, ist, tst) = match t.state {
                TodoState::Done => ("✓", Style::new().fg(DIM), dim()),
                TodoState::Active => ("◐", Style::new().fg(ORANGE), bold()),
                TodoState::Open => ("○", Style::new().fg(DIMMER), dim()),
            };
            put(buf, x, y, icon, ist, clip);
            put(
                buf,
                x + 2,
                y,
                &truncate(&t.text, w.saturating_sub(2) as usize),
                tst,
                clip,
            );
            y += 1;
        }
        y += 1;
        divider(buf, x, y, w, clip);
        y += 2;
    }

    if !s.files.is_empty() {
        header(
            buf,
            x,
            y,
            w,
            "Modified Files",
            &s.files.len().to_string(),
            clip,
        );
        y += 1;
        for f in &s.files {
            let mut right = Vec::new();
            if f.added > 0 {
                right.push((format!("+{}", f.added), Style::new().fg(GREEN)));
            }
            if f.removed > 0 {
                right.push((format!(" −{}", f.removed), Style::new().fg(RED)));
            }
            row_kv(
                buf,
                x,
                y,
                w,
                &[(truncate(&f.path, w.saturating_sub(10) as usize), text())],
                &right,
                clip,
            );
            y += 1;
        }
        y += 1;
        divider(buf, x, y, w, clip);
        y += 2;
    }

    if !s.lsp.is_empty() {
        header(buf, x, y, w, "LSP", "", clip);
        y += 1;
        for (name, state) in &s.lsp {
            row_kv(
                buf,
                x,
                y,
                w,
                &[
                    ("● ".into(), Style::new().fg(GREEN)),
                    (name.clone(), text()),
                ],
                &[(state.clone(), dim())],
                clip,
            );
            y += 1;
        }
    }

    // footer: branch + version
    let fy = area.bottom().saturating_sub(2);
    divider(buf, x, fy.saturating_sub(1), w, clip);
    if let Some(b) = &app.branch {
        put_spans(
            buf,
            x,
            fy,
            &[
                ("● ".into(), Style::new().fg(AMBER)),
                (
                    truncate(b, 16),
                    Style::new().fg(AMBER).add_modifier(Modifier::BOLD),
                ),
            ],
            clip,
        );
    }
    let v = concat!("v", env!("CARGO_PKG_VERSION"));
    put(buf, x + w - v.width() as u16, fy, v, dim(), clip);
}
