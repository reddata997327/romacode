//! Android side panel — renders the selected real target's screen and status.
use super::draw::*;
use super::pixels;
use super::theme::*;
use crate::app::{Action, App, Hit};
use ratatui::{
    buffer::Buffer,
    layout::Rect,
    style::{Color, Style},
};
use unicode_width::UnicodeWidthStr;

fn center(buf: &mut Buffer, area: Rect, y: u16, text: &str, style: Style, clip: Rect) {
    let w = text.width() as u16;
    let x = area.x + area.width.saturating_sub(w) / 2;
    put(buf, x, y, text, style, clip);
}

pub fn draw(app: &mut App, buf: &mut Buffer, area: Rect) {
    app.phone_rect = None;
    if area.width < 30 || area.height < 8 {
        return;
    }
    let clip = area;

    // Header values are queried from the selected Android target (streamed
    // from the brain as Device events).
    let (title, status, ok): (String, String, bool) = match &app.android.status {
        Some(st) if st.phase == "online" => {
            let mut status = format!("{} · {} · {}%", st.model, st.serial, st.battery);
            if app.android.streaming {
                // the frame stream is delivering committed frames
                status.push_str(" · live");
            }
            ("Android device".into(), status, true)
        }
        Some(st) if st.phase.starts_with("error:") => (
            "Android target".into(),
            format!(
                "connection error · {}",
                truncate(st.phase.trim_start_matches("error:"), 32)
            ),
            false,
        ),
        _ => (
            "Android device".into(),
            "waiting for ADB target".into(),
            false,
        ),
    };
    put(buf, area.x + 1, area.y, "▣", Style::new().fg(ORANGE), clip);
    // leave the last 11 columns for the three header buttons
    let budget = area.width.saturating_sub(6 + 11) as usize;
    let mut x = put(
        buf,
        area.x + 3,
        area.y,
        &truncate(&title, budget.min(20)),
        bold(),
        clip,
    );
    x = put(buf, x, area.y, "  ·  ", dim(), clip);
    let left = (area.right().saturating_sub(12)).saturating_sub(x);
    put_spans(
        buf,
        x,
        area.y,
        &[
            (
                "● ".into(),
                Style::new().fg(if ok { GREEN } else { DIMMER }),
            ),
            (
                truncate(&status, left as usize).into(),
                Style::new().fg(if ok { GREEN } else { DIM }),
            ),
        ],
        clip,
    );
    // header buttons
    let mut bx = area.right().saturating_sub(2);
    for (label, action, tip, key) in [
        ("✕", Action::CloseDevice, "Close panel", "esc"),
        ("⛶", Action::PhoneFullscreen, "Fullscreen phone", ""),
        ("↻", Action::Refresh, "Home screen", ""),
    ] {
        bx -= 3;
        let r = Rect::new(bx, area.y, 3, 1);
        let hov = app.hover == Some(action);
        tint(buf, r, if hov { PANEL_HI } else { PANEL });
        put(
            buf,
            bx + 1,
            area.y,
            label,
            Style::new().fg(if hov { TEXT } else { DIM }),
            clip,
        );
        app.hits.push(Hit {
            rect: r,
            action,
            tip: Some((tip, key)),
        });
        bx -= 1;
    }

    // the phone (chassis + live screen), centered in the panel body. The last
    // row is kept free for the hint line.
    let body = Rect::new(area.x, area.y + 1, area.width, area.height - 1);
    fill(buf, body, Color::Rgb(18, 13, 11));
    let hint_h = if body.height > 12 { 1 } else { 0 };
    let stage = Rect::new(body.x, body.y, body.width, body.height - hint_h);

    let online = matches!(
        &app.android.status,
        Some(st) if st.phase == "online"
    );
    let waiting = if online {
        ("Streaming…", "waiting for the first frame")
    } else {
        (
            "Waiting for Android target…",
            "Connect an emulator or device through ADB",
        )
    };
    let display = app
        .android
        .status
        .as_ref()
        .map(|s| s.display)
        .unwrap_or((0, 0));
    let hit = pixels::paint_phone(buf, stage, app.android.frame.as_ref(), display, waiting);

    // clicking the live screen opens fullscreen
    app.hits.push(Hit {
        rect: hit,
        action: Action::PhoneFullscreen,
        tip: Some(("Open fullscreen", "")),
    });
    app.phone_rect = Some(hit);

    if hint_h == 1 {
        center(
            buf,
            body,
            body.bottom() - 1,
            "click to open · mouse taps · esc closes",
            dimmer(),
            clip,
        );
    }
}
