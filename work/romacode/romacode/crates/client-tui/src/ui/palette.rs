//! Command palette overlay, tooltip and toast.
use super::draw::*;
use super::theme::*;
use crate::app::{Action, App, Hit, COMMANDS};
use ratatui::{
    buffer::Buffer,
    layout::Rect,
    style::{Color, Modifier, Style},
};
use unicode_width::UnicodeWidthStr;

fn darken(buf: &mut Buffer, area: Rect) {
    for y in area.y..area.bottom() {
        for x in area.x..area.right() {
            let c = buf.get_mut(x, y);
            let f = match c.fg {
                Color::Rgb(..) => mix(c.fg, Color::Rgb(0, 0, 0), 0.55),
                o => o,
            };
            let b = match c.bg {
                Color::Rgb(..) => mix(c.bg, Color::Rgb(0, 0, 0), 0.55),
                o => o,
            };
            c.set_fg(f);
            c.set_bg(b);
        }
    }
}

pub fn draw_palette(app: &mut App, buf: &mut Buffer, area: Rect) {
    let Some(p) = &app.palette else { return };
    darken(buf, area);
    let items = p.items();
    let w = 58u16.min(area.width.saturating_sub(4));
    let list_h = items.len().clamp(1, 12) as u16;
    let h = list_h + 4;
    let r = Rect::new(
        area.x + (area.width - w) / 2,
        area.y + (area.height.saturating_sub(h)) / 4 + 1,
        w,
        h,
    );
    fill(buf, r, Color::Rgb(30, 21, 17));
    rounded_box(buf, r, Style::new().fg(ORANGE));
    let clip = r;
    put(buf, r.x + 2, r.y + 1, "Commands", bold(), clip);
    put(buf, r.right() - 6, r.y + 1, "esc", dim(), clip);
    let q = format!("› {}", p.query);
    let qx = put(buf, r.x + 2, r.y + 2, &q, Style::new().fg(TEXT), clip);
    put(buf, qx, r.y + 2, "▌", Style::new().fg(ORANGE), clip);
    let sel = p.sel.min(items.len().saturating_sub(1));
    let mut hits = Vec::new();
    for (row, &idx) in items.iter().take(12).enumerate() {
        let (_, label, key) = COMMANDS[idx];
        let y = r.y + 3 + row as u16;
        let line = Rect::new(r.x + 1, y, r.width - 2, 1);
        if row == sel {
            tint(buf, line, PANEL_HI);
            put(buf, line.x, y, "▎", Style::new().fg(ORANGE), clip);
        }
        put(
            buf,
            r.x + 3,
            y,
            label,
            if row == sel { bold() } else { text() },
            clip,
        );
        put(buf, r.right() - 2 - key.width() as u16, y, key, dim(), clip);
        hits.push(Hit {
            rect: line,
            action: Action::PaletteRun(idx),
            tip: None,
        });
    }
    if items.is_empty() {
        put(
            buf,
            r.x + 3,
            r.y + 3,
            "No matching commands",
            dimmer(),
            clip,
        );
    }
    hits.push(Hit {
        // No-op swallow (NOT Action::Refresh, which fires an Android Home
        // keypress): clicks inside the panel must not trigger anything.
        rect: r,
        action: Action::Swallow,
        tip: None,
    }); // swallow clicks inside
        // palette hits must win over everything beneath
    app.hits.clear();
    app.hits.extend(hits);
}

pub fn draw_tooltip(app: &App, buf: &mut Buffer, area: Rect) {
    let Some(a) = app.hover else { return };
    let Some(h) = app
        .hits
        .iter()
        .rev()
        .find(|h| h.action == a && h.tip.is_some())
    else {
        return;
    };
    let (label, key) = h.tip.unwrap();
    let inner_w = label.width() + if key.is_empty() { 0 } else { 2 + key.width() };
    let w = inner_w as u16 + 4;
    let mut x = h.rect.x.saturating_sub(1);
    if x + w > area.right() {
        x = area.right().saturating_sub(w);
    }
    let y = h.rect.bottom();
    if y + 3 > area.bottom() {
        return;
    }
    let r = Rect::new(x, y, w, 3);
    fill(buf, r, Color::Rgb(26, 18, 15));
    rounded_box(buf, r, Style::new().fg(BORDER));
    let nx = put(
        buf,
        r.x + 2,
        r.y + 1,
        label,
        Style::new().fg(TEXT).add_modifier(Modifier::BOLD),
        r,
    );
    if !key.is_empty() {
        put(buf, nx + 2, r.y + 1, key, dim(), r);
    }
}

pub fn draw_toast(app: &App, buf: &mut Buffer, area: Rect) {
    let Some((msg, _)) = &app.toast else { return };
    let w = (msg.width() as u16 + 4).min(area.width.saturating_sub(2)).max(8);
    let r = Rect::new(area.right().saturating_sub(w + 2), area.y + 3, w, 3);
    if r.bottom() > area.bottom() {
        return;
    }
    let shown = truncate_to_width(msg, (w - 4) as usize);
    fill(buf, r, Color::Rgb(34, 24, 19));
    rounded_box(buf, r, Style::new().fg(ORANGE));
    put(buf, r.x + 2, r.y + 1, &shown, Style::new().fg(TEXT), r);
}

pub fn draw_settings(app: &mut App, buf: &mut Buffer, area: Rect) {
    use crate::settings::Screen;
    let Some(st) = &app.settings else { return };
    // Reset the mouse hit-test rows for this frame; each screen repopulates
    // them as it draws its clickable rows.
    app.settings_hits.clear();
    darken(buf, area);
    let w = 76u16.min(area.width.saturating_sub(4));
    let h = area.height.saturating_sub(6).clamp(16, 32).min(area.height);
    let r = Rect::new(
        area.x + area.width.saturating_sub(w) / 2,
        area.y + area.height.saturating_sub(h) / 2,
        w,
        h,
    );
    fill(buf, r, Color::Rgb(30, 21, 17));
    rounded_box(buf, r, Style::new().fg(ORANGE));
    put(buf, r.x + 2, r.y + 1, "Settings", bold(), r);
    put(
        buf,
        r.right().saturating_sub(19),
        r.y + 1,
        "↑↓ navigate",
        dim(),
        r,
    );
    put(buf, r.right().saturating_sub(6), r.y + 1, "esc", dim(), r);
    let inner = Rect::new(
        r.x + 2,
        r.y + 3,
        r.width.saturating_sub(4),
        r.height.saturating_sub(4),
    );
    match st.screen {
        Screen::Root => {
            put(
                buf,
                inner.x,
                inner.y,
                "Commands · Providers · Config — type to filter, Enter to run.",
                dim(),
                inner,
            );
            let qline = format!("› {}", st.query);
            let qx = put(buf, inner.x, inner.y + 2, &qline, Style::new().fg(TEXT), inner);
            put(buf, qx, inner.y + 2, "▌", Style::new().fg(ORANGE), inner);
            let items = st.root_items();
            let sel = st.sel.min(items.len().saturating_sub(1));
            let max_rows = inner.height.saturating_sub(5) as usize;
            // Keep the selection visible when scrolling.
            let start = sel.saturating_sub(max_rows.saturating_sub(1));
            let mut y = inner.y + 4;
            let mut last_cat: Option<&str> = None;
            let mut drawn = 0usize;
            for (visible_row, (orig_idx, e)) in items.iter().enumerate() {
                if visible_row < start {
                    continue;
                }
                if drawn >= max_rows {
                    break;
                }
                if last_cat != Some(e.category) {
                    if drawn > 0 {
                        y = y.saturating_add(1);
                        if y >= inner.bottom().saturating_sub(1) {
                            break;
                        }
                    }
                    put(buf, inner.x, y, e.category, Style::new().fg(ORANGE), inner);
                    y = y.saturating_add(1);
                    last_cat = Some(e.category);
                    if y >= inner.bottom().saturating_sub(1) {
                        break;
                    }
                }
                let line = Rect::new(inner.x, y, inner.width, 1);
                app.settings_hits.push(crate::settings::HitRow {
                    x: line.x,
                    y: line.y,
                    w: line.width,
                    h: line.height,
                    kind: crate::settings::RowKind::RootItem(visible_row),
                });
                if visible_row == sel {
                    tint(buf, line, PANEL_HI);
                    put(buf, line.x, line.y, "▎", Style::new().fg(ORANGE), inner);
                }
                let _ = orig_idx; // kept for potential hit-testing
                put(
                    buf,
                    line.x + 2,
                    line.y,
                    e.label,
                    if visible_row == sel { bold() } else { text() },
                    inner,
                );
                if !e.key_hint.is_empty() {
                    let hint_w = e.key_hint.width() as u16 + 2;
                    put(
                        buf,
                        inner.right().saturating_sub(hint_w),
                        y,
                        e.key_hint,
                        dim(),
                        inner,
                    );
                }
                y = y.saturating_add(1);
                drawn += 1;
            }
            if items.is_empty() {
                put(
                    buf,
                    inner.x,
                    inner.y + 5,
                    "No matching entries",
                    dimmer(),
                    inner,
                );
            }
            put(
                buf,
                inner.x,
                inner.bottom().saturating_sub(1),
                &format!(
                    "{} items · type to filter · enter to run · esc to close",
                    items.len()
                ),
                dim(),
                inner,
            );
        }
        Screen::Providers => {
            put(buf, inner.x, inner.y, "Choose a provider", bold(), inner);
            let items = st.provider_items();
            let total = romacode_shared::catalog::known_len();
            // Search row — typing filters by display name or provider id.
            put(
                buf,
                inner.x,
                inner.y + 1,
                &format!("› {}▌", st.provider_query),
                Style::new().fg(TEXT),
                inner,
            );
            let start = st.sel.saturating_sub(5);
            let visible = inner.height.saturating_sub(5) as usize;
            for (row, (vis, (_, k))) in items
                .iter()
                .enumerate()
                .skip(start)
                .take(visible)
                .enumerate()
            {
                let line = Rect::new(inner.x, inner.y + 3 + row as u16, inner.width, 1);
                // Hit rows carry the VISIBLE index, not the original catalog
                // index, so a click selects the same row the cursor is on.
                app.settings_hits.push(crate::settings::HitRow {
                    x: line.x,
                    y: line.y,
                    w: line.width,
                    h: line.height,
                    kind: crate::settings::RowKind::Provider(vis),
                });
                if vis == st.sel {
                    tint(buf, line, PANEL_HI);
                    put(buf, line.x, line.y, "▎", Style::new().fg(ORANGE), inner);
                }
                put(
                    buf,
                    line.x + 2,
                    line.y,
                    &format!("{}  ·  {}", k.name, k.id),
                    if vis == st.sel { bold() } else { text() },
                    inner,
                );
            }
            // The custom-provider row is ALWAYS the last row, after the
            // filtered list — name → base URL → API key → fetch models.
            let custom_vis = items.len();
            let custom_row = custom_vis.saturating_sub(start);
            if custom_row < visible {
                let line = Rect::new(inner.x, inner.y + 3 + custom_row as u16, inner.width, 1);
                app.settings_hits.push(crate::settings::HitRow {
                    x: line.x,
                    y: line.y,
                    w: line.width,
                    h: line.height,
                    kind: crate::settings::RowKind::Provider(custom_vis),
                });
                if custom_vis == st.sel {
                    tint(buf, line, PANEL_HI);
                    put(buf, line.x, line.y, "▎", Style::new().fg(ORANGE), inner);
                }
                put(
                    buf,
                    line.x + 2,
                    line.y,
                    "＋ Custom provider…",
                    if custom_vis == st.sel { bold() } else { text() },
                    inner,
                );
            }
            if items.is_empty() {
                put(buf, inner.x, inner.y + 3, "No matching providers", dim(), inner);
            }
            if let Some(error) = &st.error {
                put(
                    buf,
                    inner.x,
                    inner.bottom().saturating_sub(2),
                    &format!("Error: {error}"),
                    Style::new().fg(Color::Rgb(255, 120, 100)),
                    inner,
                );
            }
            put(
                buf,
                inner.x,
                inner.bottom().saturating_sub(1),
                &format!(
                    "{} of {} · type to filter · enter to choose · esc to close",
                    items.len(),
                    total
                ),
                dim(),
                inner,
            );
        }
        Screen::KnownKey | Screen::CustomName | Screen::CustomBase | Screen::CustomKey => {
            let title = match st.screen {
                Screen::KnownKey => format!("Add API Key · {}", st.provider_name),
                Screen::CustomName => "Add Custom Provider · Name (1/3)".into(),
                Screen::CustomBase => "Add Custom Provider · Base URL (2/3)".into(),
                _ => "Add Custom Provider · API Key (3/3)".into(),
            };
            put(buf, inner.x, inner.y, &title, bold(), inner);
            let prompt = match st.screen {
                Screen::KnownKey | Screen::CustomKey => "API key",
                Screen::CustomBase => "Base URL",
                Screen::CustomName => "Provider name",
                _ => "",
            };
            put(
                buf,
                inner.x,
                inner.y + 2,
                &format!("{prompt}:"),
                dim(),
                inner,
            );
            let mask = matches!(st.screen, Screen::KnownKey | Screen::CustomKey);
            let value = st.input_display(mask);
            put(
                buf,
                inner.x + 2,
                inner.y + 3,
                &format!("› {value}"),
                Style::new().fg(TEXT),
                inner,
            );
            // The screen must always say what it is doing: a model fetch
            // freezes the keys on purpose, so show the spinner — and any
            // failure has to be readable here, or the screen looks dead.
            if st.loading {
                put(
                    buf,
                    inner.x,
                    inner.y + 5,
                    "Fetching models… (esc to cancel)",
                    Style::new().fg(ORANGE),
                    inner,
                );
            } else if let Some(error) = &st.error {
                put(
                    buf,
                    inner.x,
                    inner.y + 5,
                    &format!("Error: {error}"),
                    Style::new().fg(Color::Rgb(255, 120, 100)),
                    inner,
                );
                put(
                    buf,
                    inner.x,
                    inner.y + 7,
                    "Fix the input and press Enter to retry.",
                    dim(),
                    inner,
                );
            }
            put(
                buf,
                inner.x,
                inner.bottom().saturating_sub(2),
                "Enter to fetch models · Esc to go back",
                dim(),
                inner,
            );
        }
        Screen::Models | Screen::SelectModel => {
            let title = if st.screen == Screen::Models {
                format!("Models from {}", st.provider_name)
            } else {
                "Select Model".into()
            };
            put(buf, inner.x, inner.y, &title, bold(), inner);
            if st.loading {
                put(
                    buf,
                    inner.x,
                    inner.y + 2,
                    "Fetching models…",
                    Style::new().fg(ORANGE),
                    inner,
                );
            }
            if let Some(error) = &st.error {
                put(
                    buf,
                    inner.x,
                    inner.y + 2,
                    &format!("Error: {error}"),
                    Style::new().fg(Color::Rgb(255, 120, 100)),
                    inner,
                );
                put(
                    buf,
                    inner.x,
                    inner.y + 4,
                    "Fix the input and press Enter to retry.",
                    dim(),
                    inner,
                );
            }
            // Search row — typing filters by model id or provider id.
            put(
                buf,
                inner.x,
                inner.y + 1,
                &format!("› {}▌", st.model_query),
                Style::new().fg(TEXT),
                inner,
            );
            let items = st.model_items();
            let start = st.sel.saturating_sub(5);
            let visible = inner.height.saturating_sub(6) as usize;
            for (row, (vis, idx)) in items
                .iter()
                .enumerate()
                .skip(start)
                .take(visible)
                .enumerate()
            {
                let line = Rect::new(inner.x, inner.y + 3 + row as u16, inner.width, 1);
                app.settings_hits.push(crate::settings::HitRow {
                    x: line.x,
                    y: line.y,
                    w: line.width,
                    h: line.height,
                    kind: crate::settings::RowKind::Model(vis),
                });
                if vis == st.sel {
                    tint(buf, line, PANEL_HI);
                    put(buf, line.x, line.y, "▎", Style::new().fg(ORANGE), inner);
                }
                let model = st.models.get(*idx).map(String::as_str).unwrap_or("");
                // Every row shows its own provider. On SelectModel the list is
                // cross-provider, so this is the only way to tell which entry
                // a model belongs to; the same model id may appear twice.
                let pid = st.model_providers.get(*idx).cloned().unwrap_or_default();
                let provider_label = if pid.is_empty() {
                    st.provider_name.clone()
                } else {
                    romacode_shared::catalog::find(&pid)
                        .map(|k| k.name.clone())
                        .unwrap_or_else(|| pid.clone())
                };
                let budget = inner.width.saturating_sub(6) as usize;
                let label = truncate_to_width(model, budget);
                put(
                    buf,
                    line.x + 2,
                    line.y,
                    &label,
                    if vis == st.sel { bold() } else { text() },
                    inner,
                );
                if !provider_label.is_empty() {
                    let pl = truncate_to_width(&provider_label, (inner.width / 3) as usize);
                    let pw = pl.width() as u16;
                    put(
                        buf,
                        inner.right().saturating_sub(pw + 2),
                        line.y,
                        &pl,
                        dim(),
                        inner,
                    );
                }
            }
            if items.is_empty() {
                put(buf, inner.x, inner.y + 3, "No matching models", dim(), inner);
            }
            // The selected model's official running rules (embedded shared
            // modeldata — same source the server clamps requests with). One
            // dim line under the list; the raw id string is never touched, so
            // selection keeps saving exact model ids.
            if !st.loading {
                if let Some(idx) = st.selected_model_index() {
                    let model = st.models.get(idx);
                    let provider = st
                        .model_providers
                        .get(idx)
                        .cloned()
                        .unwrap_or_else(|| st.provider_id.clone());
                    if let (Some(m), false) = (model, provider.is_empty()) {
                        if let Some(p) = romacode_shared::modeldata::find(&provider, m) {
                            let line = Rect::new(inner.x, inner.bottom().saturating_sub(2), inner.width, 1);
                            put(
                                buf,
                                line.x,
                                line.y,
                                &format!(
                                    "{} — {} ({}{})",
                                    p.display_name,
                                    romacode_shared::modeldata::summary(p),
                                    p.status,
                                    if p.unverified_fields.is_empty() {
                                        ""
                                    } else {
                                        ", some fields unverified"
                                    }
                                ),
                                dim(),
                                inner,
                            );
                        }
                    }
                }
            }
            put(
                buf,
                inner.x,
                inner.bottom().saturating_sub(1),
                &format!(
                    "{} of {} models · type to filter · enter to activate · esc to close",
                    items.len(),
                    st.models.len()
                ),
                dim(),
                inner,
            );
        }
        Screen::Tools => {
            let tools = app.tools.list();
            let names: Vec<String> = tools.iter().map(|t| t.name.clone()).collect();
            let filt = st.tools_filter(&names);
            let cats = app.tools.categories().len();
            put(
                buf,
                inner.x,
                inner.y,
                &format!("Tools — integrated builder ({} tools, {} categories)", tools.len(), cats),
                bold(),
                inner,
            );
            let qline = format!("› {}", st.tools_query);
            let qx = put(buf, inner.x, inner.y + 2, &qline, Style::new().fg(TEXT), inner);
            put(buf, qx, inner.y + 2, "▌", Style::new().fg(ORANGE), inner);
            let sel = st.tools_sel.min(filt.len().saturating_sub(1));
            let max_rows = inner.height.saturating_sub(5) as usize;
            let start = sel.saturating_sub(max_rows.saturating_sub(1));
            let mut y = inner.y + 4;
            let mut last_cat: Option<String> = None;
            let mut drawn = 0usize;
            for (vis, &fi) in filt.iter().enumerate() {
                if vis < start {
                    continue;
                }
                if drawn >= max_rows {
                    break;
                }
                let t = &tools[fi];
                if last_cat.as_deref() != Some(t.category.as_str()) {
                    if drawn > 0 {
                        y = y.saturating_add(1);
                        if y >= inner.bottom().saturating_sub(1) {
                            break;
                        }
                    }
                    put(buf, inner.x, y, t.category.as_str(), Style::new().fg(ORANGE), inner);
                    y = y.saturating_add(1);
                    last_cat = Some(t.category.clone());
                    if y >= inner.bottom().saturating_sub(1) {
                        break;
                    }
                }
                let line = Rect::new(inner.x, y, inner.width, 1);
                app.settings_hits.push(crate::settings::HitRow {
                    x: line.x,
                    y: line.y,
                    w: line.width,
                    h: line.height,
                    kind: crate::settings::RowKind::Tool(vis),
                });
                if vis == sel {
                    tint(buf, line, PANEL_HI);
                    put(buf, line.x, line.y, "▎", Style::new().fg(ORANGE), inner);
                }
                put(
                    buf,
                    line.x + 2,
                    line.y,
                    t.name.as_str(),
                    if vis == sel { bold() } else { text() },
                    inner,
                );
                let namew = t.name.width() as u16;
                let desc_w = (inner.width as usize).saturating_sub(namew as usize + 4 + 2);
                let desc: String = t.description.chars().take(desc_w).collect();
                if !desc.is_empty() {
                    put(buf, line.x + 2 + namew + 2, line.y, desc.as_str(), dim(), inner);
                }
                y = y.saturating_add(1);
                drawn += 1;
            }
            if filt.is_empty() {
                put(buf, inner.x, inner.y + 5, "No matching tools", dimmer(), inner);
            }
            put(
                buf,
                inner.x,
                inner.bottom().saturating_sub(1),
                &format!("{} tools · Enter to run · Esc back · type to filter · click", filt.len()),
                dim(),
                inner,
            );
        }
        Screen::ToolsInput => {
            let name = st.tools_selected.clone().unwrap_or_default();
            put(buf, inner.x, inner.y, &format!("Run tool · {name}"), bold(), inner);
            if let Some(schema) = app.tools.schema_json(&name) {
                let one: String = schema
                    .lines()
                    .next()
                    .unwrap_or("")
                    .chars()
                    .take(inner.width.saturating_sub(2) as usize)
                    .collect();
                put(buf, inner.x, inner.y + 2, &format!("schema: {one}"), dim(), inner);
            }
            put(buf, inner.x, inner.y + 4, "Input JSON:", dim(), inner);
            let value = st.input_display(false);
            put(
                buf,
                inner.x + 2,
                inner.y + 5,
                &format!("› {value}"),
                Style::new().fg(TEXT),
                inner,
            );
            if let Some((ok, text)) = &st.tools_result {
                let col = if *ok {
                    Color::Rgb(120, 220, 140)
                } else {
                    Color::Rgb(255, 120, 100)
                };
                let label = if *ok { "result" } else { "error" };
                let mut y = inner.y + 7;
                put(buf, inner.x, y, &format!("{label}:"), Style::new().fg(col), inner);
                y = y.saturating_add(1);
                let cap = inner.height.saturating_sub(10) as usize;
                for line in text.lines().take(cap.max(1)) {
                    let trunc: String = line
                        .chars()
                        .take(inner.width.saturating_sub(2) as usize)
                        .collect();
                    put(buf, inner.x + 2, y, &trunc, Style::new().fg(col), inner);
                    y = y.saturating_add(1);
                }
            } else if let Some(err) = &st.error {
                put(
                    buf,
                    inner.x,
                    inner.y + 7,
                    &format!("Error: {err}"),
                    Style::new().fg(Color::Rgb(255, 120, 100)),
                    inner,
                );
            }
            put(
                buf,
                inner.x,
                inner.bottom().saturating_sub(2),
                "Enter to run · Esc back",
                dim(),
                inner,
            );
        }
    }
}

// ── flat Settings page (Ctrl+P) ─────────────────────────────────────────────

/// True when a radio-style Preset row is the currently persisted value.
/// Kept pure so the "mark exactly one option" rule is directly testable:
/// Manual (`prompt`) and Auto accept (`autoaccept`) share one path, so only
/// the row whose pinned value equals the live value may show the mark.
fn preset_is_active(tui: &romacode_shared::tui_settings::TuiSettings, path: &str, value: &str) -> bool {
    tui.get(path).and_then(|v| v.as_str().map(|s| s == value)).unwrap_or(false)
}

/// Draw the ONE-page Settings overlay: every section is a main heading with
/// its subsetting options directly below it — no drilling, no breadcrumb, no
/// sub-screens for plain settings. Rows that open a flow (Models, Providers,
/// Tools, Config file) hand off to the same-titled Settings panel.
pub fn draw_tree(app: &mut App, buf: &mut Buffer, area: Rect) {
    if app.tree.is_none() {
        return;
    }
    // The panel is sized to its content but never taller than the terminal:
    // the page scrolls inside the viewport with ▲▼ in the header.
    let rows_total = app.tree.as_ref().map(|t| t.rows().len()).unwrap_or(0) as u16;
    let max_h = area.height.saturating_sub(2).min(34).max(8);
    let h = (rows_total + 6).min(max_h);
    let w = 78u16.min(area.width.saturating_sub(4));
    let r = Rect::new(
        area.x + (area.width - w) / 2,
        area.y + (area.height.saturating_sub(h)) / 2,
        w,
        h,
    );
    fill(buf, r, Color::Rgb(30, 21, 17));
    rounded_box(buf, r, Style::new().fg(ORANGE));
    let clip = r;
    app.tree_hits.clear();
    let Some(t) = app.tree.as_mut() else { return; };

    // Header: one title, always the same — this is the only Settings screen.
    put(buf, r.x + 2, r.y + 1, "Settings", bold(), clip);
    // Current permission mode, always visible at the top right.
    let auto = app.cfg.tui.session.permissions == "autoaccept";
    let perm = format!("perm: {}", if auto { "auto accept" } else { "manual" });
    put(
        buf,
        r.right().saturating_sub(perm.width() as u16 + 2),
        r.y + 1,
        &perm,
        if auto { Style::new().fg(GREEN) } else { Style::new().fg(ORANGE) },
        clip,
    );

    // Filter row (typing filters leaves by label or section).
    let mut row_y = r.y + 3;
    let q = format!("› {}", t.query);
    let qx = put(buf, r.x + 2, row_y, &q, Style::new().fg(TEXT), clip);
    put(buf, qx, row_y, "▌", Style::new().fg(ORANGE), clip);
    row_y += 2;

    // Snapshot the page state (visual rows, visible leaves, cursor, viewport)
    // while the tree borrow is alive, then release it so the draw loop can
    // touch `app.cfg` / `app.tree_hits` freely.
    let avail = r.bottom().saturating_sub(row_y + 2) as usize; // rows that fit
    let (rows, vis, sel, top, vis_len) = {
        t.clamp_viewport(avail);
        let top = t.top;
        (t.rows(), t.visible_leaves(), t.sel, top, t.visible_leaves().len())
    };

    let inner_right = r.right().saturating_sub(1);
    let label_max = (r.width as usize).saturating_sub(4).max(8);

    // The page itself: headings + leaf rows in one scrollable list.
    let shown = rows.len().min(avail);
    for vi in top..(top + shown).min(rows.len()) {
        let y = row_y + (vi - top) as u16;
        match rows[vi] {
            crate::tree::VRow::Heading(sec) => {
                // A main heading with a rule — the section name, then its
                // subsetting options right below.
                let line_txt = format!("  {sec} ");
                let fill = (r.width as usize).saturating_sub(line_txt.width() + 4);
                let heading = format!("{line_txt}{}", "─".repeat(fill));
                put(buf, r.x + 1, y, &truncate_to_width(&heading, (r.width - 2) as usize), Style::new().fg(ORANGE).add_modifier(Modifier::BOLD), clip);
            }
            crate::tree::VRow::Leaf(pos) => {
                let Some((_, n, _)) = vis.get(pos) else { continue; };
                let line = Rect::new(r.x + 1, y, r.width.saturating_sub(2), 1);
                let is_sel = pos == sel;
                if is_sel {
                    tint(buf, line, PANEL_HI);
                    put(buf, line.x, y, "▎", Style::new().fg(ORANGE), clip);
                }
                let crate::tree::NodeKind::Leaf { owner, leaf, .. } = &n.kind else { continue };
                // A Preset row is a radio choice (Manual / Auto accept):
                // exactly the active option gets the filled mark. Every other
                // row keeps the owner badge (● brain / ○ screen).
                let mut preset_active = false;
                let b: &str = match leaf {
                    crate::tree::Leaf::Preset { path, value } => {
                        preset_active = preset_is_active(&app.cfg.tui, path, value);
                        if preset_active { "●" } else { "○" }
                    }
                    _ => if *owner == crate::tree::Owner::Brain { "●" } else { "○" },
                };
                // Settings show their live value; presets show the active
                // mark; flow rows show what they hand off to.
                let value = match leaf {
                    crate::tree::Leaf::Setting { path } => app.cfg.tui.display(path),
                    crate::tree::Leaf::Preset { .. } => {
                        if preset_active { "✓ active".to_string() } else { String::new() }
                    }
                    crate::tree::Leaf::Dest(d) => match d {
                        crate::tree::Dest::SelectModel => {
                            let m = app.cfg.effective_model();
                            if m.is_empty() {
                                "(none — pick one) ›".to_string()
                            } else {
                                format!("{m} ›")
                            }
                        }
                        crate::tree::Dest::OpenProviders => {
                            format!("{} known + custom ›", romacode_shared::catalog::known_len())
                        }
                        crate::tree::Dest::OpenTools => "42 builtin (incl. dsd trace) + MCP ›".to_string(),
                        crate::tree::Dest::OpenConfigFile => "~/.config/romacode ›".to_string(),
                        crate::tree::Dest::PrintEffectiveConfig => "view ›".to_string(),
                        crate::tree::Dest::ResetSettings => "restore ›".to_string(),
                    },
                };
                let label_full = format!("{b}  {}", n.label);
                // A long value (model id, path) may take at most half the row;
                // before, it was drawn straight over the label on narrow panes.
                let value = truncate_to_width(&value, (label_max / 2).max(8));
                // Truncate the label so the value always fits on the right.
                let hint_w = value.width();
                let budget = if hint_w + 2 >= label_max {
                    label_max
                } else {
                    label_max.saturating_sub(hint_w + 2)
                };
                let label = truncate_to_width(&label_full, budget);
                put(buf, line.x + 2, y, &label, if is_sel { bold() } else { text() }, clip);
                if !value.is_empty() {
                    put(buf, inner_right.saturating_sub(hint_w as u16), y, &value, dim(), clip);
                }
                app.tree_hits.push(crate::tree::TreeHit {
                    x: line.x,
                    y: line.y,
                    w: line.width,
                    h: line.height,
                    kind: crate::tree::TreeHitKind::TreeRow(pos),
                });
            }
        }
    }
    if vis_len == 0 {
        put(buf, r.x + 3, row_y, "No matching settings", dimmer(), clip);
    }

    // Scroll indicator + footer.
    let more_above = top > 0;
    let more_below = top + shown < rows.len();
    let scroll = match (more_above, more_below) {
        (true, true) => "▲▼",
        (true, false) => "▲",
        (false, true) => "▼",
        (false, false) => "",
    };
    if !scroll.is_empty() {
        put(buf, r.right().saturating_sub(4), r.y + 1, scroll, Style::new().fg(ORANGE), clip);
    }
    put(
        buf,
        r.x + 2,
        r.bottom().saturating_sub(1),
        // The selected row's hint leads the footer: a live one-line
        // description of what the cursor is on ("Theme — color palette").
        &{
            let sel_hint = vis
                .get(sel.min(vis.len().saturating_sub(1)))
                .and_then(|(_, n, _)| match &n.kind {
                    crate::tree::NodeKind::Leaf { hint, .. } => Some(*hint),
                    _ => None,
                })
                .filter(|h| !h.is_empty());
            let base = "↑↓ move · ←→ change · Enter open · type to filter · esc close";
            let footer = match sel_hint {
                Some(h) => format!("{h}  ·  ↑↓ move · ←→ change · Enter open · esc close"),
                None => base.to_string(),
            };
            truncate_to_width(&footer, (r.width.saturating_sub(4)) as usize)
        },
        dim(),
        clip,
    );
    // The panel background swallows stray clicks without activating row 0.
    app.tree_hits.push(crate::tree::TreeHit {
        x: r.x,
        y: r.y,
        w: r.width,
        h: r.height,
        kind: crate::tree::TreeHitKind::Panel,
    });
    app.hits.clear();
}

/// Truncate `s` to fit within `max` display columns, appending `…` if cut.
fn truncate_to_width(s: &str, max: usize) -> String {
    use unicode_width::UnicodeWidthStr;
    if max == 0 {
        return String::new();
    }
    let mut out = String::new();
    let mut w = 0usize;
    for ch in s.chars() {
        let cw = unicode_width::UnicodeWidthChar::width(ch).unwrap_or(0);
        if w + cw > max.saturating_sub(1) {
            break;
        }
        out.push(ch);
        w += cw;
    }
    let full_w = UnicodeWidthStr::width(s);
    if full_w > max {
        out.push('…');
    }
    out
}

// ── `/` slash menu (chat) ───────────────────────────────────────────────────

/// Draw the `/` slash menu popup over the chat input: a filtered list of
/// every tree leaf. The user types after the slash to filter; Enter runs
/// the selected leaf (toggle, choice, or destination screen).
pub fn draw_slash(app: &mut App, buf: &mut Buffer, area: Rect) {
    let Some(m) = app.slash.as_mut() else { return; };
    let w = 60u16.min(area.width.saturating_sub(4));
    let max_rows = 10usize;
    let avail = m.items.len().min(max_rows);
    let list_h = avail.max(1) as u16;
    let h = list_h + 5;
    // anchor above the bottom of the screen (where the chat input lives)
    let y = area.bottom().saturating_sub(h + 2).max(area.y + 4);
    let r = Rect::new(area.x + (area.width - w) / 2, y, w, h);
    fill(buf, r, Color::Rgb(30, 21, 17));
    rounded_box(buf, r, Style::new().fg(ORANGE));
    let clip = r;
    put(buf, r.x + 2, r.y + 1, "Command Menu", bold(), clip);
    let q = format!("/{}", m.query);
    let qx = put(buf, r.x + 2, r.y + 2, &q, Style::new().fg(TEXT), clip);
    put(buf, qx, r.y + 2, "▌", Style::new().fg(ORANGE), clip);

    app.tree_hits.clear();
    let sel = m.sel.min(m.items.len().saturating_sub(1));
    m.clamp_viewport(max_rows);
    let top = m.top;
    let label_max = (r.width as usize).saturating_sub(4).max(8);

    let shown = m.items.len().min(max_rows);
    for vi in 0..shown {
        let i = top + vi;
        let Some(f) = m.items.get(i) else { break; };
        let y = r.y + 3 + vi as u16;
        let line = Rect::new(r.x + 1, y, r.width.saturating_sub(2), 1);
        let is_sel = i == sel;
        if is_sel {
            tint(buf, line, PANEL_HI);
            put(buf, line.x, y, "▎", Style::new().fg(ORANGE), clip);
        }
        let badge = if f.owner == crate::tree::Owner::Brain { "●" } else { "○" };
        let label_full = format!("{badge} {}", f.label);
        let label = truncate_to_width(&label_full, label_max);
        put(buf, line.x + 2, y, &label, if is_sel { bold() } else { text() }, clip);
        app.tree_hits.push(crate::tree::TreeHit {
            x: line.x,
            y: line.y,
            w: line.width,
            h: line.height,
            kind: crate::tree::TreeHitKind::SlashRow(i),
        });
    }
    if m.items.is_empty() {
        put(buf, r.x + 3, r.y + 3, "No matching commands", dimmer(), clip);
    }
    // scroll indicator
    let more_above = top > 0;
    let more_below = top + shown < m.items.len();
    if more_above || more_below {
        let s = match (more_above, more_below) {
            (true, true) => "▲▼",
            (true, false) => "▲",
            (false, true) => "▼",
            _ => "",
        };
        put(buf, r.right().saturating_sub(4), r.y + 1, s, Style::new().fg(ORANGE), clip);
    }
    put(
        buf,
        r.x + 2,
        r.bottom().saturating_sub(1),
        "↑↓ nav · enter run · esc close",
        dim(),
        clip,
    );
    app.tree_hits.push(crate::tree::TreeHit {
        x: r.x,
        y: r.y,
        w: r.width,
        h: r.height,
        kind: crate::tree::TreeHitKind::Panel,
    });
    app.hits.clear();
}

// ── permission popup (approval requests) ────────────────────────────────────

/// Modal allow/deny popup for a pending tool approval. The brain raises it
/// with an `ApprovalRequest` event (request id + exact pending effect); the
/// user answers with y / n / esc or by clicking the buttons, and the answer
/// goes back over the wire as `ClientRequest::Approval`. Drawn last so it
/// sits above every other overlay.
pub fn draw_permission(app: &mut App, buf: &mut Buffer, area: Rect) {
    use crate::app::PermHit;
    let Some(p) = app.pending_approval.first() else {
        app.perm_hits.clear();
        return;
    };
    app.perm_hits.clear();
    darken(buf, area);

    let w = 68u16.min(area.width.saturating_sub(4)).max(26);
    let inner_w = w.saturating_sub(6) as usize;
    let effect_lines = wrap_plain(&p.effect, inner_w);
    // keep the popup on screen: title + tool + effect + buttons + hint + borders
    let max_effect = (area.height.saturating_sub(10)).max(1) as usize;
    let shown = effect_lines.len().min(max_effect);
    let h = (8 + shown as u16).min(area.height.saturating_sub(2));
    let r = Rect::new(
        area.x + (area.width.saturating_sub(w)) / 2,
        area.y + (area.height.saturating_sub(h)) / 2,
        w,
        h,
    );
    fill(buf, r, Color::Rgb(36, 25, 19));
    rounded_box(buf, r, Style::new().fg(ORANGE));
    let clip = r;
    let inner = Rect::new(
        r.x + 2,
        r.y + 1,
        r.width.saturating_sub(4),
        r.height.saturating_sub(2),
    );

    put(
        buf,
        inner.x,
        inner.y,
        "PERMISSION REQUIRED",
        Style::new().fg(YELLOW).add_modifier(Modifier::BOLD),
        clip,
    );
    put(buf, inner.right().saturating_sub(3), inner.y, "esc", dim(), clip);

    let tool_line = format!("{} wants to run (session {}):", p.name, p.session_id);
    put(
        buf,
        inner.x,
        inner.y + 2,
        &truncate_to_width(&tool_line, inner.width as usize),
        accent_bold(),
        clip,
    );

    let mut y = inner.y + 3;
    for line in effect_lines.iter().take(shown) {
        put(buf, inner.x, y, line, text(), clip);
        y += 1;
    }
    if effect_lines.len() > shown {
        let more = format!(
            "… +{} more lines (full effect is in the transcript)",
            effect_lines.len() - shown
        );
        put(
            buf,
            inner.x,
            y,
            &truncate_to_width(&more, inner.width as usize),
            dimmer(),
            clip,
        );
    }

    // Buttons — recorded as hit rows so a click answers the approval.
    let by = r.bottom().saturating_sub(3);
    let allow_label = " [Y] Allow once ";
    let deny_label = " [N] Deny ";
    let aw = allow_label.width() as u16;
    let dw = deny_label.width() as u16;
    let ax = r.x + 3;
    let dx = ax + aw + 3;
    if dx + dw < r.right().saturating_sub(1) {
        let arect = Rect::new(ax, by, aw, 1);
        let drect = Rect::new(dx, by, dw, 1);
        app.perm_hits.push(PermHit {
            x: arect.x,
            y: arect.y,
            w: arect.width,
            h: arect.height,
            allow: true,
        });
        app.perm_hits.push(PermHit {
            x: drect.x,
            y: drect.y,
            w: drect.width,
            h: drect.height,
            allow: false,
        });
        fill(buf, arect, DIFF_ADD);
        put(
            buf,
            ax,
            by,
            allow_label,
            Style::new().fg(GREEN).add_modifier(Modifier::BOLD),
            arect,
        );
        fill(buf, drect, DIFF_DEL);
        put(
            buf,
            dx,
            by,
            deny_label,
            Style::new().fg(RED).add_modifier(Modifier::BOLD),
            drect,
        );
    }
    put(
        buf,
        inner.x,
        r.bottom().saturating_sub(2),
        "y allow once · n / esc deny · ctrl+c interrupt the turn",
        dim(),
        clip,
    );
}

#[cfg(test)]
mod tests {
    use super::preset_is_active;
    use romacode_shared::tui_settings::TuiSettings;

    #[test]
    fn permission_mark_follows_the_live_value() {
        let mut tui = TuiSettings::default();
        assert_eq!(tui.session.permissions, "autoaccept");
        assert!(!preset_is_active(&tui, "session.permissions", "prompt"));
        assert!(preset_is_active(&tui, "session.permissions", "autoaccept"));

        tui.set("session.permissions", &serde_json::json!("prompt")).unwrap();
        assert!(preset_is_active(&tui, "session.permissions", "prompt"));
        assert!(!preset_is_active(&tui, "session.permissions", "autoaccept"));
    }
}
