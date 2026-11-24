//! Fullscreen view of the selected real Android emulator or device.

use super::draw::*;
use super::pixels;
use super::theme::*;
use crate::app::App;
use ratatui::{
    buffer::Buffer,
    layout::Rect,
    style::{Color, Modifier, Style},
};
use unicode_width::UnicodeWidthStr;

pub const FOOTER_H: u16 = 1;

pub fn draw(app: &mut App, buf: &mut Buffer, area: Rect) {
    // The fullscreen phone paints over the ENTIRE screen; without clearing,
    // stale chat-screen hit rects (tab bar, the invisible "×" close button,
    // mode labels) stayed live underneath — clicking where the "×" used to
    // be closed a session, and tooltips for hidden elements still rendered.
    app.hits.clear();
    fill(buf, area, Color::Rgb(6, 8, 12));
    if area.height < 8 || area.width < 20 {
        return;
    }
    // the phone (chassis + live screen) fills the terminal minus a header/footer strip
    let body = Rect::new(area.x, area.y + 1, area.width, area.height - 2 - FOOTER_H);
    let phase = app.android.status.as_ref().map(|st| st.phase.clone());
    let err = app.android.error();
    let waiting = match (&phase, &err) {
        (Some(p), _) if p == "online" => ("Android device", "waiting for display frame"),
        (_, Some(error)) => ("Android target unavailable", error.as_str()),
        _ => (
            "Waiting for Android target…",
            "started the brain? connect with Android Platform-Tools ADB",
        ),
    };
    let display = app
        .android
        .status
        .as_ref()
        .map(|s| s.display)
        .unwrap_or((0, 0));
    let screen = pixels::paint_phone(buf, body, app.android.frame.as_ref(), display, waiting);
    app.phone_rect = Some(screen);

    // Header: selected target and values reported by Android.
    let line = if let Some(st) = &app.android.status {
        match st.phase.as_str() {
            "online" => format!(
                "Android device · {} · {} · battery {}%",
                st.model, st.serial, st.battery
            ),
            p if p.starts_with("error:") => {
                format!("Android target error · {}", &p["error:".len()..])
            }
            _ => "Waiting for Android target".into(),
        }
    } else {
        "Waiting for Android target".into()
    };
    put(
        buf,
        area.x + 2,
        area.y,
        &truncate(&line, area.width.saturating_sub(24) as usize),
        dim(),
        area,
    );
    let hint = "click tap · drag swipe · type text · enter ⏎ · esc close";
    let hx = area.right().saturating_sub(hint.width() as u16 + 2);
    put(buf, hx, area.y, hint, dimmer(), area);

    // footer
    let fy = area.bottom() - 1;
    put(
        buf,
        area.x + 2,
        fy,
        "Android device — fullscreen",
        dim(),
        area,
    );
    put(
        buf,
        area.right() - 12,
        fy,
        "esc  exit",
        Style::new()
            .fg(TEXT)
            .bg(PANEL_HI)
            .add_modifier(Modifier::BOLD),
        area,
    );
}
