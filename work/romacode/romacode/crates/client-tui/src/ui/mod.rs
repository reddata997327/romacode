pub mod bg;
pub mod chat;
pub mod draw;
pub mod highlight;
pub mod home;
pub mod inputbox;
pub mod logo;
pub mod markdown;
pub mod palette;
pub mod panels;
pub mod phone;
pub mod pixels;
pub mod queue;
pub mod sidebar;
pub mod tabs;
pub mod terminal;
pub mod theme;
pub mod tool_detail;
pub mod transcript;
pub mod waterfall;

use crate::app::{App, Screen};
use ratatui::Frame;

pub fn draw(f: &mut Frame, app: &mut App) {
    let area = f.size();
    app.hits.clear();
    let buf = f.buffer_mut();
    if area.width < 40 || area.height < 12 {
        draw::fill(buf, area, theme::BG);
        draw::put(
            buf,
            area.x,
            area.y,
            "Terminal too small",
            theme::dim(),
            area,
        );
        return;
    }
    match app.screen {
        Screen::Home => home::draw(app, buf, area),
        Screen::Chat => chat::draw(app, buf, area),
        Screen::Terminal => terminal::draw(app, buf, area),
    }
    if app.android_full {
        phone::draw(app, buf, area);
    }
    if app.slash.is_some() {
        palette::draw_slash(app, buf, area);
    } else if app.tree.is_some() {
        palette::draw_tree(app, buf, area);
    } else if app.settings.is_some() {
        palette::draw_settings(app, buf, area);
    } else if app.palette.is_some() {
        palette::draw_palette(app, buf, area);
    } else {
        palette::draw_tooltip(app, buf, area);
    }
    // The permission popup is topmost (below only the transient toast): the
    // brain is waiting on the user's y/n, so nothing may hide it.
    if !app.pending_approval.is_empty() {
        palette::draw_permission(app, buf, area);
    }
    palette::draw_toast(app, buf, area);
}
