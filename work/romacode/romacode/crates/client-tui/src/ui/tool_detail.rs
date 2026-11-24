//! Click-to-inspect tool details: a modal over the chat that shows the FULL
//! input (pretty-printed) and the FULL captured output of one tool call,
//! scrollable with ↑/↓ / PgUp/PgDn / mouse wheel, closed with esc / q or a
//! click outside the box. Opened by clicking a `▸` tool row in the
//! transcript or with ctrl+o on the most recent call.
use super::draw::{truncate, wrap_plain};
use super::theme::*;
use crate::app::App;
use crate::session::{Block, ToolStatus};
use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::style::{Modifier, Style};
use unicode_width::UnicodeWidthStr;

/// One styled content line of the detail view.
type Line = (String, Style);

/// Build the content lines for one tool block: header info, INPUT (pretty
/// JSON, or `$ command` for shell calls) and OUTPUT (full, wrapped).
fn content(name: &str, status: &ToolStatus, arg: &str, output: &Option<String>) -> Vec<Line> {
    let mut lines: Vec<Line> = Vec::new();
    let (icon, ic) = match status {
        ToolStatus::Ok => ("✓ succeeded", GREEN),
        ToolStatus::Running => ("◌ running", ORANGE),
        ToolStatus::Failed => ("✗ failed", RED),
    };
    lines.push((
        format!("{icon}  {name}"),
        Style::new().fg(ic).add_modifier(Modifier::BOLD),
    ));
    lines.push((String::new(), Style::new()));

    lines.push(("─ INPUT ".repeat(6), dimmer()));
    // Neat per-tool presentation: shell commands shown as `$ …`, everything
    // else pretty-printed JSON when it parses.
    if name.eq_ignore_ascii_case("bash") {
        let cmd = serde_json::from_str::<serde_json::Value>(arg)
            .ok()
            .and_then(|v| v.get("command").and_then(|c| c.as_str()).map(String::from))
            .unwrap_or_else(|| arg.to_string());
        for l in cmd.lines() {
            lines.push((format!("  $ {l}"), Style::new().fg(TEXT)));
        }
    } else if let Ok(v) = serde_json::from_str::<serde_json::Value>(arg) {
        if let Ok(pretty) = serde_json::to_string_pretty(&v) {
            for l in pretty.lines() {
                let (key_style, body_style) = if l.trim_start().starts_with('"') {
                    (Style::new().fg(ORANGE), Style::new().fg(TEXT))
                } else {
                    (Style::new().fg(TEXT), Style::new().fg(TEXT))
                };
                let _ = key_style;
                lines.push((format!("  {l}"), body_style));
            }
        }
    } else {
        for l in wrap_plain(arg, 10_000) {
            lines.push((format!("  {l}"), Style::new().fg(TEXT)));
        }
    }
    lines.push((String::new(), Style::new()));

    lines.push(("─ OUTPUT ".repeat(6), dimmer()));
    match output {
        Some(out) if out.trim().is_empty() => {
            lines.push(("  (no output)".into(), dimmer()));
        }
        Some(out) => {
            for l in out.lines() {
                if l.is_empty() {
                    lines.push((String::new(), Style::new()));
                } else {
                    // Long lines keep their shape: horizontal scrolling is
                    // worse than wrapping in a chat transcript viewer.
                    for w in wrap_plain(l, 200) {
                        lines.push((format!("  {w}"), Style::new().fg(TEXT)));
                    }
                }
            }
        }
        None => {
            lines.push(("  (no output captured)".into(), dimmer()));
        }
    }
    lines
}

/// Draw the modal (call only when `app.tool_detail` is set).
pub fn draw(app: &App, buf: &mut Buffer, area: Rect) {
    let Some((mi, bi)) = app.tool_detail else { return };
    let Some(s) = app.sessions.get(app.active) else { return };
    let block = match s.messages.get(mi) {
        Some(crate::session::Msg::Assistant { blocks, .. }) => blocks.get(bi),
        _ => None,
    };
    let Some(Block::Tool {
        name,
        status,
        arg,
        output,
        ..
    }) = block
    else {
        return;
    };

    // backdrop: darken everything behind the modal
    for y in area.y..area.bottom() {
        for x in area.x..area.right() {
            let c = buf.get_mut(x, y);
            c.set_char(' ');
            c.set_style(Style::new().bg(ratatui::style::Color::Rgb(12, 9, 7)));
        }
    }

    let w = (area.width.saturating_sub(8)).min(110).max(40);
    let h = (area.height.saturating_sub(6)).max(10);
    let bx = area.x + (area.width - w) / 2;
    let by = area.y + (area.height - h) / 2;
    let box_area = Rect::new(bx, by, w, h);

    // box + border
    for y in by..by + h {
        for x in bx..bx + w {
            let c = buf.get_mut(x, y);
            c.set_char(' ').set_bg(super::theme::PANEL);
        }
    }
    let border = Style::new().fg(ORANGE);
    for x in bx..bx + w {
        buf.get_mut(x, by).set_char('─').set_style(border);
        buf.get_mut(x, by + h - 1).set_char('─').set_style(border);
    }
    for y in by..by + h {
        buf.get_mut(bx, y).set_char('│').set_style(border);
        buf.get_mut(bx + w - 1, y).set_char('│').set_style(border);
    }
    buf.get_mut(bx, by).set_char('╭').set_style(border);
    buf.get_mut(bx + w - 1, by).set_char('╮').set_style(border);
    buf.get_mut(bx, by + h - 1).set_char('╰').set_style(border);
    buf.get_mut(bx + w - 1, by + h - 1)
        .set_char('╯')
        .set_style(border);

    // title
    let title = format!(" Tool details — {name} ");
    super::draw::put(
        buf,
        bx + 2,
        by,
        &truncate(&title, (w as usize).saturating_sub(4)),
        Style::new()
            .fg(TEXT)
            .add_modifier(Modifier::BOLD)
            .bg(PANEL),
        box_area,
    );

    // content (scrollable)
    let inner = Rect::new(bx + 2, by + 1, w.saturating_sub(4), h.saturating_sub(3));
    let lines = content(name, status, arg, output);
    let inner_h = inner.height as usize;
    let max_scroll = lines.len().saturating_sub(inner_h);
    let scroll = app.tool_scroll.min(max_scroll);
    for (vy, line) in lines.iter().skip(scroll).take(inner_h).enumerate() {
        super::draw::put(buf, inner.x, inner.y + vy as u16, &line.0, line.1, inner);
    }

    // footer hints + scroll position
    let pos = if lines.len() > inner_h {
        format!(" {}/{} ", scroll + 1, max_scroll + 1)
    } else {
        String::new()
    };
    let hints = " ↑/↓ scroll · esc/q close ";
    let hy = by + h - 1;
    super::draw::put(buf, bx + 2, hy, hints, dimmer().bg(PANEL), box_area);
    if !pos.is_empty() {
        let px = bx + w - 2 - pos.width() as u16;
        super::draw::put(
            buf,
            px,
            hy,
            &pos,
            Style::new().fg(TEXT).bg(PANEL_HI).add_modifier(Modifier::BOLD),
            box_area,
        );
    }
}

/// Total scrollable lines (for clamping from the key handler).
pub fn lines_for(app: &App) -> usize {
    let Some((mi, bi)) = app.tool_detail else { return 0 };
    let Some(s) = app.sessions.get(app.active) else { return 0 };
    let block = match s.messages.get(mi) {
        Some(crate::session::Msg::Assistant { blocks, .. }) => blocks.get(bi),
        _ => None,
    };
    match block {
        Some(Block::Tool {
            name,
            status,
            arg,
            output,
            ..
        }) => content(name, status, arg, output).len(),
        _ => 0,
    }
}
