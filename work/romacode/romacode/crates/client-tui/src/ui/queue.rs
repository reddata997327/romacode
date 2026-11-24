//! Queued queries: what the user typed while the agent was busy.
//!
//! ```text
//! ═════╡ queued · 2 ╞═══════════════  ctrl+s send · alt+e edit · alt+d delete
//! ▌▣ fix the login redirect too                              ➤  ✎  ✕
//!  ▣ then run the tests again                                ➤  ✎  ✕
//! ═════╡══════════════════════════════════════════════════════════════
//! ```
//! `ctrl+s` (or the ➤ icon) hands the selected query to the agent: it appears
//! in the chat at once and the agent reads it before its next step.
use super::draw::*;
use super::theme::*;
use crate::app::{Action, Hit};
use ratatui::{
    buffer::Buffer,
    layout::Rect,
    style::{Modifier, Style},
};
use unicode_width::UnicodeWidthStr;

/// Rows of queued queries shown at once (the rest scroll with the selection).
pub const MAX_VISIBLE: usize = 3;

/// Total height of the box for `n` queued queries (0 when empty).
pub fn height(n: usize) -> u16 {
    if n == 0 {
        0
    } else {
        2 + n.min(MAX_VISIBLE) as u16
    }
}

fn one_line(s: &str) -> String {
    s.replace('\n', " ⏎ ").replace('\t', " ")
}

pub fn draw(
    buf: &mut Buffer,
    r: Rect,
    queue: &[String],
    sel: usize,
    hover: Option<Action>,
    hits: &mut Vec<Hit>,
) {
    if queue.is_empty() || r.width < 24 || r.height < 3 {
        return;
    }
    let clip = r;
    let rule = Style::new().fg(ORANGE);
    let n = queue.len();
    let sel = sel.min(n - 1);
    let vis = ((r.height - 2) as usize).min(MAX_VISIBLE).min(n);
    let start = sel.saturating_sub(vis.saturating_sub(1)).min(n - vis);

    // ── top rule: ═════╡ queued · n ╞═══…  + key hints
    let head = format!("╡ queued · {n} ╞");
    let mut x = hline_str(buf, r.x, r.y, "═════", rule, clip);
    x = put(buf, x, r.y, &head, Style::new().fg(ORANGE).add_modifier(Modifier::BOLD), clip);
    let hint = "ctrl+s send · alt+e edit · alt+d delete";
    let hint_w = hint.width() as u16;
    let show_hint = r.width > x - r.x + hint_w + 4;
    let rule_end = if show_hint { r.right() - hint_w - 2 } else { r.right() };
    while x < rule_end {
        x = put(buf, x, r.y, "═", rule, clip);
    }
    if show_hint {
        put(buf, r.right() - hint_w - 1, r.y, hint, dimmer(), clip);
    }

    // ── rows
    for (vi, i) in (start..start + vis).enumerate() {
        let y = r.y + 1 + vi as u16;
        let row = Rect::new(r.x, y, r.width, 1);
        let is_sel = i == sel;
        let bg = if is_sel { PANEL_HI } else { DARKSPACE };
        fill(buf, row, bg);
        hits.push(Hit {
            rect: row,
            action: Action::QueueSelect(i),
            tip: None,
        });
        put(buf, r.x, y, if is_sel { "▌" } else { " " }, Style::new().fg(ORANGE).bg(bg), clip);
        put(buf, r.x + 1, y, "▣", Style::new().fg(if is_sel { ORANGE } else { DIM }).bg(bg), clip);

        // icons, right-aligned: ➤ send   ✎ edit   ✕ delete
        let icons: [(&str, Action, Color, &'static str, &'static str); 3] = [
            ("➤", Action::QueueSend(i), GREEN, "Send now", "ctrl+s"),
            ("✎", Action::QueueEdit(i), AMBER, "Edit", "alt+e"),
            ("✕", Action::QueueDelete(i), RED, "Delete", "alt+d"),
        ];
        let icons_w = 3 * 3;
        let ix0 = r.right().saturating_sub(icons_w + 1);
        for (k, (g, act, col, tip, key)) in icons.iter().enumerate() {
            let gx = ix0 + k as u16 * 3 + 1;
            let hot = hover == Some(*act);
            let mut st = Style::new().fg(*col).bg(if hot { HOVER } else { bg });
            if hot {
                st = st.add_modifier(Modifier::BOLD | Modifier::UNDERLINED);
            }
            put(buf, gx - 1, y, " ", st, clip);
            put(buf, gx, y, g, st, clip);
            put(buf, gx + 1, y, " ", st, clip);
            hits.push(Hit {
                rect: Rect::new(gx - 1, y, 3, 1),
                action: *act,
                tip: Some((tip, key)),
            });
        }

        // query text between the box icon and the action icons
        let tx = r.x + 3;
        let tw = (ix0.saturating_sub(tx + 1)) as usize;
        let text = truncate(&one_line(&queue[i]), tw);
        let st = if is_sel {
            Style::new().fg(TEXT).bg(bg).add_modifier(Modifier::BOLD)
        } else {
            Style::new().fg(TEXT).bg(bg)
        };
        put(buf, tx, y, &text, st, clip);
    }

    // ── bottom rule (shows how many more are queued off-screen)
    let by = r.y + 1 + vis as u16;
    let more = n - vis;
    let tag = if more > 0 { format!("╡ +{more} more ╞") } else { "╡".to_string() };
    let mut x = hline_str(buf, r.x, by, "═════", rule, clip);
    x = put(buf, x, by, &tag, rule, clip);
    while x < r.right() {
        x = put(buf, x, by, "═", rule, clip);
    }
}

/// Write a short literal string; returns the x after it.
fn hline_str(buf: &mut Buffer, x: u16, y: u16, s: &str, st: Style, clip: Rect) -> u16 {
    put(buf, x, y, s, st, clip)
}

use ratatui::style::Color;
