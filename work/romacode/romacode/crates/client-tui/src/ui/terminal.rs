//! Ratatui renderer for Roxterm's VT grid.
use super::draw::{fill, put};
use super::theme;
use crate::app::App;
use ratatui::{
    buffer::Buffer,
    layout::Rect,
    style::{Color, Modifier, Style},
};
use rox_protocol::flags;
use unicode_width::UnicodeWidthChar;

pub fn draw(app: &mut App, buf: &mut Buffer, area: Rect) {
    app.hits.clear();
    fill(buf, area, theme::BG);
    if area.width < 12 || area.height < 4 {
        put(
            buf,
            area.x.saturating_add(1),
            area.y,
            "Terminal area too small",
            theme::dim(),
            area,
        );
        return;
    }

    let header_y = area.y;
    let footer_y = area.bottom() - 1;
    let screen_area = Rect::new(area.x, area.y + 1, area.width, area.height - 2);
    let (rows, cursor, cursor_visible, shell_title, status) =
        if let Some(terminal) = app.terminal.as_mut() {
            if terminal.screen.cols() != screen_area.width as usize
                || terminal.screen.rows() != screen_area.height as usize
            {
                let _ = terminal.resize(screen_area.width, screen_area.height);
            }
            let rows = terminal.visible_rows();
            let cursor = terminal.screen.cursor();
            let visible = terminal.screen.mode_bits & 1 != 0;
            let title = if terminal.screen.title.is_empty() {
                "interactive shell".to_string()
            } else {
                terminal.screen.title.clone()
            };
            let status = terminal.exit_code().map(|code| format!("exited {code}"));
            (rows, cursor, visible, title, status)
        } else {
            (
                Vec::new(),
                (0, 0),
                false,
                "terminal unavailable".to_string(),
                None,
            )
        };

    put(
        buf,
        area.x + 1,
        header_y,
        &format!("ROXTERM  ·  {shell_title}"),
        theme::accent_bold(),
        area,
    );
    if let Some(status) = status {
        let text = format!("{status} · alt+t to return");
        let x = area.right().saturating_sub(text.chars().count() as u16 + 1);
        put(buf, x, header_y, &text, theme::dim(), area);
    }

    for (y, row) in rows.iter().take(screen_area.height as usize).enumerate() {
        let py = screen_area.y + y as u16;
        let mut x = screen_area.x;
        for cell in &row.cells {
            if x >= screen_area.right() {
                break;
            }
            let mut ch = cell.ch;
            if cell.flags & flags::HIDDEN != 0 {
                ch = ' ';
            }
            let width = ch.width().unwrap_or(1).max(1) as u16;
            let mut fg = decode_color(cell.fg, theme::TEXT);
            let mut bg = decode_color(cell.bg, theme::BG);
            let reverse = cell.flags & flags::REVERSE != 0
                || (cursor_visible
                    && cursor.0 as u16 == x.saturating_sub(screen_area.x)
                    && cursor.1 as usize == y);
            if reverse {
                std::mem::swap(&mut fg, &mut bg);
            }
            let mut style = Style::default().fg(fg).bg(bg);
            if cell.flags & flags::BOLD != 0 {
                style = style.add_modifier(Modifier::BOLD);
            }
            if cell.flags & flags::DIM != 0 {
                style = style.add_modifier(Modifier::DIM);
            }
            if cell.flags & flags::ITALIC != 0 {
                style = style.add_modifier(Modifier::ITALIC);
            }
            if cell.flags & flags::UNDERLINE != 0 {
                style = style.add_modifier(Modifier::UNDERLINED);
            }
            if cell.flags & flags::STRIKEOUT != 0 {
                style = style.add_modifier(Modifier::CROSSED_OUT);
            }
            let symbol = ch.to_string();
            buf.get_mut(x, py).set_symbol(&symbol).set_style(style);
            x = x.saturating_add(width);
        }
    }

    fill(
        buf,
        Rect::new(area.x, footer_y, area.width, 1),
        theme::PANEL,
    );
    put(
        buf,
        area.x + 1,
        footer_y,
        "Native PTY · Roxterm VT · shift+PgUp/PgDn or wheel: scrollback · alt+t return · ctrl+c to shell",
        theme::dim(),
        area,
    );
}

fn decode_color(value: u32, default: Color) -> Color {
    match value & 0xc000_0000 {
        0x4000_0000 => Color::Indexed((value & 0xff) as u8),
        0x8000_0000 => Color::Rgb(
            ((value >> 16) & 0xff) as u8,
            ((value >> 8) & 0xff) as u8,
            (value & 0xff) as u8,
        ),
        _ => default,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn decodes_roxterm_wire_colors() {
        assert_eq!(
            decode_color(0x4000_0000 | 203, theme::BG),
            Color::Indexed(203)
        );
        assert_eq!(
            decode_color(0x8000_0000 | 0x12_34_56, theme::BG),
            Color::Rgb(0x12, 0x34, 0x56)
        );
        assert_eq!(decode_color(0, theme::BG), theme::BG);
    }
}
