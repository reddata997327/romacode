//! Builds the full list of transcript rows for a session at a given width.
use super::draw::{truncate, Row};
use super::{markdown, theme::*};
use crate::app::ClientState;
use crate::session::*;
use romacode_shared::protocol::Msg;
use ratatui::style::{Modifier, Style};
use unicode_width::UnicodeWidthStr;

pub fn fmt_tokens(n: u64) -> String {
    if n >= 1_000_000 {
        format!("{:.1}M", n as f64 / 1e6)
    } else if n >= 1000 {
        format!("{:.1}K", n as f64 / 1e3)
    } else {
        n.to_string()
    }
}

pub fn cost(s: &UiSession, cfg: &ClientState) -> f64 {
    // Usage state stores high-water marks, not a cumulative billing total.
    (s.tokens_in as f64 * cfg.price_in_per_mtok + s.tokens_out as f64 * cfg.price_out_per_mtok)
        / 1e6
}

pub fn ensure(s: &mut UiSession, cfg: &ClientState, width: u16) {
    if let Some((w, rev, _, _)) = &s.cache {
        if *w == width && *rev == s.rev {
            return;
        }
    }
    let (rows, tools) = build(s, cfg, width as usize);
    s.cache = Some((width, s.rev, rows, tools));
}

/// Collapse whitespace/newlines so a tool argument or result stays on one row.
fn flat(s: &str) -> String {
    s.split_whitespace().collect::<Vec<_>>().join(" ")
}

fn bar_row(spans: Vec<(String, Style)>, width: usize) -> Row {
    let mut sp = vec![("▎".to_string(), Style::new().fg(ORANGE))];
    sp.extend(spans);
    Row {
        spans: sp,
        bg: Some((PANEL, 1, width as u16)),
    }
}

fn build(s: &UiSession, cfg: &ClientState, width: usize) -> (Vec<Row>, crate::session::ToolMap) {
    let mut rows: Vec<Row> = Vec::new();
    let mut tools: crate::session::ToolMap = Vec::new();
    if s.messages.is_empty() {
        return (rows, tools);
    }
    // ── title block ──
    let pct = if cfg.context_window > 0 {
        s.tokens_total() * 100 / cfg.context_window
    } else {
        0
    };
    let right = format!(
        "{} peak ({}%)  ·  ~${:.2} peak estimate ",
        fmt_tokens(s.tokens_total()),
        pct,
        cost(s, cfg)
    );
    let title = format!(
        "  # {}",
        truncate(&s.title, width.saturating_sub(right.width() + 6))
    );
    let gap = width.saturating_sub(1 + title.width() + right.width());
    rows.push(bar_row(
        vec![
            (title, Style::new().fg(TEXT).add_modifier(Modifier::BOLD)),
            (" ".repeat(gap), Style::new()),
            (right, dim()),
        ],
        width,
    ));
    rows.push(Row::blank());

    for (mi, m) in s.messages.iter().enumerate() {
        match m {
            Msg::User { text: t } => {
                let inner = width.saturating_sub(5);
                for l in super::draw::wrap_plain(t, inner) {
                    rows.push(bar_row(
                        vec![(format!("  {l}"), Style::new().fg(TEXT))],
                        width,
                    ));
                }
                rows.push(Row::blank());
            }
            Msg::Assistant {
                blocks,
                model,
                mode,
                secs,
            } => {
                let mut head = vec![
                    ("◆ ".to_string(), Style::new().fg(ORANGE)),
                    (mode.label().to_string(), accent_bold()),
                    (format!("   {model}"), dim()),
                ];
                if let Some(sec) = secs {
                    head.push((format!("  ·  {sec:.1}s"), dim()));
                }
                rows.push(Row::from(head));
                rows.push(Row::blank());
                let mut prev_tool = false;
                for (bi, b) in blocks.iter().enumerate() {
                    match b {
                        Block::Text { text: t } => {
                            if prev_tool {
                                rows.push(Row::blank());
                            }
                            markdown::render(t.trim_end_matches('\n'), width, 0, &mut rows);
                            rows.push(Row::blank());
                            prev_tool = false;
                        }
                        Block::Tool {
                            name,
                            arg,
                            status,
                            extra,
                            ..
                        } => {
                            tools.push((rows.len(), mi, bi));
                            let (icon, ic) = match status {
                                ToolStatus::Ok => ("✓", GREEN),
                                ToolStatus::Running => ("◌", ORANGE),
                                ToolStatus::Failed => ("✗", RED),
                            };
                            let arg_line = flat(arg);
                            let arg_txt = truncate(
                                &arg_line,
                                width.saturating_sub(name.width() + 16),
                            );
                            let used = 4 + name.width() + 2 + arg_txt.width();
                            let mut sp = vec![
                                // collapsed-row affordance: every tool row
                                // opens the full details view on click / ctrl+o
                                ("▸".to_string(), dimmer()),
                                (" ".to_string(), Style::new()),
                                (format!("{icon} "), Style::new().fg(ic)),
                                (
                                    name.clone(),
                                    Style::new().fg(DIM).add_modifier(Modifier::BOLD),
                                ),
                                (format!("  {arg_txt}"), dimmer()),
                            ];
                            if let Some(e) = extra {
                                // one line, and only as much as still fits
                                let room = width.saturating_sub(used + 3);
                                if room >= 6 {
                                    sp.push((
                                        format!("  {}", truncate(&flat(e), room)),
                                        Style::new().fg(GREEN),
                                    ));
                                }
                            }
                            rows.push(Row::from(sp));
                            prev_tool = true;
                        }
                    }
                }
                if prev_tool {
                    rows.push(Row::blank());
                }
            }
            Msg::Notice { text: t } => {
                for l in super::draw::wrap_plain(t, width.saturating_sub(2)) {
                    rows.push(Row::from(vec![(format!("· {l}"), dim())]));
                }
                rows.push(Row::blank());
            }
            Msg::Error { text: t } => {
                for (i, l) in super::draw::wrap_plain(t, width.saturating_sub(4))
                    .into_iter()
                    .enumerate()
                {
                    let lead = if i == 0 { "✗ " } else { "  " };
                    rows.push(Row::from(vec![(
                        format!("{lead}{l}"),
                        Style::new().fg(RED),
                    )]));
                }
                rows.push(Row::blank());
            }
        }
    }
    // trim trailing blank rows
    while rows.last().map_or(false, |r| r.spans.is_empty()) {
        rows.pop();
    }
    (rows, tools)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn transcript_does_not_advertise_unsupported_sharing() {
        let cfg = ClientState::default();
        let mut session = UiSession::new(1);
        session.messages.push(Msg::User {
            text: "Keep the existing conversation visible".into(),
        });

        let (rows, _) = build(&session, &cfg, 80);
        let rendered = rows
            .iter()
            .flat_map(|row| row.spans.iter().map(|(text, _)| text.as_str()))
            .collect::<String>();

        assert!(rendered.contains("Keep the existing conversation visible"));
        assert!(!rendered.contains("/share"));
        assert!(!rendered.contains("shareable link"));
    }
}
