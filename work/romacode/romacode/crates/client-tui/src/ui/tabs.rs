//! Session tab bar (top, two rows): hover effects, close button, "+" button.
use super::draw::*;
use super::theme::*;
use crate::app::{Action, App, Device, Hit};
use ratatui::{
    buffer::Buffer,
    layout::Rect,
    style::{Modifier, Style},
};
use unicode_width::UnicodeWidthStr;

pub const HEIGHT: u16 = 2;

pub fn draw(app: &mut App, buf: &mut Buffer, area: Rect) {
    let clip = area;
    fill(buf, area, BG);
    hline(
        buf,
        area.x,
        area.y + 1,
        area.width,
        "─",
        Style::new().fg(BORDER),
        clip,
    );

    // right cluster, laid out right → left
    let mut rx = area.right().saturating_sub(2);
    let icons: [(&str, Action, Device, &str, &str); 1] = [(
        " ▣ ",
        Action::ToggleAndroid,
        Device::Android,
        "Connect to Android emulator/device via ADB",
        "alt+a",
    )];
    for (label, action, dev, tip, key) in icons {
        let w = label.width() as u16;
        if rx < area.x + w {
            break;
        }
        rx -= w;
        let active = app.device == dev;
        let hov = app.hover == Some(action);
        let r = Rect::new(rx, area.y, w, 1);
        if active || hov {
            tint(buf, r, if active { PANEL_HI } else { HOVER });
        }
        let fg = if active {
            ORANGE
        } else if hov {
            TEXT
        } else {
            DIM
        };
        put(
            buf,
            rx,
            area.y,
            label,
            Style::new().fg(fg).add_modifier(Modifier::BOLD),
            clip,
        );
        if active {
            hline(buf, rx, area.y + 1, w, "━", Style::new().fg(ORANGE), clip);
        }
        app.hits.push(Hit {
            rect: Rect::new(rx, area.y, w, 2),
            action,
            tip: Some((tip, key)),
        });
        rx = rx.saturating_sub(1);
    }
    let panel_lbl = "ctrl+b panel";
    let pw = panel_lbl.width() as u16;
    if rx >= area.x + pw + 2 {
        rx -= pw;
        let hov = app.hover == Some(Action::TogglePanel);
        put(
            buf,
            rx,
            area.y,
            panel_lbl,
            Style::new().fg(if hov { TEXT } else { DIM }),
            clip,
        );
        app.hits.push(Hit {
            rect: Rect::new(rx, area.y, pw, 1),
            action: Action::TogglePanel,
            tip: Some(("Toggle side panel", "ctrl+b")),
        });
    }
    let limit = rx.saturating_sub(2);

    // tabs
    let n = app.sessions.len();
    let plus_w = 3u16;
    let avail = limit.saturating_sub(area.x + 2 + plus_w + 1) as usize;
    let max_title = ((avail / n.max(1)).saturating_sub(7)).clamp(5, 28);
    let t = app.t();
    let mut x = area.x + 2;
    for i in 0..n {
        let (title, running) = {
            let s = &app.sessions[i];
            (truncate(&s.title, max_title), s.is_running())
        };
        let active = i == app.active;
        let w = (title.width() + 7) as u16;
        if x + w > limit {
            break;
        }
        let tab = Rect::new(x, area.y, w, 1);
        let close = Rect::new(x + w - 3, area.y, 3, 1);
        let hov_tab = app.hover == Some(Action::SelectTab(i));
        let hov_close = app.hover == Some(Action::CloseTab(i));
        if active {
            tint(buf, tab, PANEL_HI);
        } else if hov_tab || hov_close {
            tint(buf, tab, HOVER);
        }
        let dot_c = if running && (t * 4.0) as u32 % 2 == 0 {
            AMBER
        } else if active || running {
            ORANGE
        } else {
            DIMMER
        };
        put(buf, x + 1, area.y, "●", Style::new().fg(dot_c), clip);
        let tstyle = if active {
            Style::new().fg(TEXT).add_modifier(Modifier::BOLD)
        } else if hov_tab || hov_close {
            Style::new().fg(TEXT)
        } else {
            Style::new().fg(DIM)
        };
        put(buf, x + 3, area.y, &title, tstyle, clip);
        if hov_tab || hov_close {
            if hov_close {
                tint(buf, close, BORDER);
            }
            put(
                buf,
                close.x + 1,
                area.y,
                "×",
                Style::new().fg(if hov_close { TEXT } else { DIM }),
                clip,
            );
        }
        if active {
            hline(buf, x, area.y + 1, w, "━", Style::new().fg(ORANGE), clip);
        } else if hov_tab || hov_close {
            hline(buf, x, area.y + 1, w, "─", Style::new().fg(DIMMER), clip);
        }
        app.hits.push(Hit {
            rect: tab,
            action: Action::SelectTab(i),
            tip: None,
        });
        app.hits.push(Hit {
            rect: close,
            action: Action::CloseTab(i),
            tip: Some(("Close session", "ctrl+w")),
        });
        x += w + 1;
    }

    // "+" button
    if x + plus_w <= limit + 1 {
        let r = Rect::new(x, area.y, plus_w, 1);
        let hov = app.hover == Some(Action::NewTab);
        if hov {
            tint(buf, r, PANEL_HI);
            put(buf, x - 1, area.y, "▕", Style::new().fg(ORANGE), clip);
            put(buf, x + plus_w, area.y, "▏", Style::new().fg(ORANGE), clip);
        }
        put(
            buf,
            x + 1,
            area.y,
            "+",
            Style::new()
                .fg(if hov { ORANGE } else { DIM })
                .add_modifier(Modifier::BOLD),
            clip,
        );
        app.hits.push(Hit {
            rect: r,
            action: Action::NewTab,
            tip: Some(("New session", "ctrl+n")),
        });
    }
}
