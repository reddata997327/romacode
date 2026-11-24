//! Chat screen: tab bar, transcript, chatbox, sidebar and device panel.
use super::draw::*;
use super::theme::*;
use super::{inputbox, panels, queue, sidebar, tabs, tool_detail, transcript, waterfall};
use crate::app::{Action, App, Device, Hit};
use ratatui::{
    buffer::Buffer,
    layout::Rect,
    style::{Modifier, Style},
};
use unicode_width::UnicodeWidthStr;

pub fn draw(app: &mut App, buf: &mut Buffer, area: Rect) {
    fill(buf, area, BG);
    tabs::draw(
        app,
        buf,
        Rect::new(area.x, area.y, area.width, tabs::HEIGHT),
    );
    let body = Rect::new(
        area.x,
        area.y + tabs::HEIGHT,
        area.width,
        area.height.saturating_sub(tabs::HEIGHT),
    );

    let mut main = body;
    if app.panel && area.width >= 90 {
        let sb = Rect::new(
            body.right() - sidebar::WIDTH,
            body.y,
            sidebar::WIDTH,
            body.height,
        );
        sidebar::draw(app, buf, sb);
        main.width -= sidebar::WIDTH;
    }

    let mut chat_col = main;
    let mut device_rect = None;
    let mut narrow = false;
    if app.device != Device::None && main.width >= 80 {
        let cw = (main.width as u32 * app.cfg.chat_width_pct as u32 / 100) as u16;
        let cw = cw.clamp(34, main.width.saturating_sub(38));
        chat_col = Rect::new(main.x, main.y, cw, main.height);
        device_rect = Some(Rect::new(
            main.x + cw + 1,
            main.y + 1,
            main.width - cw - 2,
            main.height.saturating_sub(2),
        ));
        narrow = true;
    }
    draw_column(app, buf, chat_col, narrow);
    if let Some(r) = device_rect {
        panels::draw(app, buf, r);
    }
    // click-to-inspect modal (topmost over the chat)
    if app.tool_detail.is_some() {
        tool_detail::draw(app, buf, area);
    }
}

fn draw_column(app: &mut App, buf: &mut Buffer, col: Rect, narrow: bool) {
    if col.width < 20 || col.height < 8 {
        return;
    }
    let x = col.x + 2;
    let w = col.width - 4;
    let clip = col;
    let t = app.t();

    // footer (cwd + key hints)
    let fy = col.bottom() - 1;
    let busy = app.sessions[app.active].is_running();
    let hints: [(&str, &str); 3] = if busy {
        [("enter", "queue"), ("ctrl+s", "send now"), ("esc", "stop")]
    } else {
        [("enter", "send"), ("esc", "interrupt"), ("ctrl+p", "commands")]
    };
    let tw: usize = hints
        .iter()
        .map(|(k, l)| k.width() + 1 + l.width())
        .sum::<usize>()
        + 3 * 2;
    // the cwd is cut to the room the hints leave (it used to run right under
    // them, or past the column edge on a long path)
    let room = if !narrow && tw + 8 < w as usize {
        w as usize - tw - 3
    } else {
        w as usize
    };
    let cwd = truncate(&app.cwd, room);
    put(buf, x, fy, &cwd, dim(), clip);
    if !narrow {
        if (tw + cwd.width() + 3) as u16 <= w {
            let mut hx = x + w - tw as u16;
            for (i, (k, l)) in hints.iter().enumerate() {
                hx = put(buf, hx, fy, k, bold(), clip);
                hx = put(buf, hx + 1, fy, l, dim(), clip);
                if i < 2 {
                    hx += 3;
                }
            }
        }
    }

    // chatbox — never so tall that it eats the transcript on a short terminal
    let ih = inputbox::height(&app.sessions[app.active].input, w)
        .min(col.height.saturating_sub(7))
        .max(4);
    let input_y = fy.saturating_sub(1 + ih);

    // queued queries (typed while the agent works) sit right above the chatbox
    let qn = app.sessions[app.active].queue.len();
    let room_q = input_y.saturating_sub(col.y + 6); // keep >= 4 transcript rows
    let qh = if qn > 0 && room_q >= 3 {
        queue::height(qn).min(room_q)
    } else {
        0
    };
    let queue_y = input_y - qh;
    if qh > 0 {
        let (sel, hover) = (app.sessions[app.active].queue_sel, app.hover);
        let q = app.sessions[app.active].queue.clone();
        queue::draw(buf, Rect::new(x, queue_y, w, qh), &q, sel, hover, &mut app.hits);
    }
    {
        let s = &app.sessions[app.active];
        let hover_mode = app.hover == Some(Action::ToggleMode);
        inputbox::draw(
            buf,
            Rect::new(x, input_y, w, ih),
            &inputbox::Ctx {
                input: &s.input,
                mode: s.mode,
                cfg: &app.cfg,
                t,
                running: s.is_running(),
                placeholder: if s.messages.is_empty() {
                    "Ask anything…"
                } else {
                    "Ask a follow-up…"
                },
                inset_bg: (28, 19, 15),
                hover_mode,
            },
            &mut app.hits,
        );
    }

    // transcript
    let tr = Rect::new(x, col.y + 1, w, queue_y.saturating_sub(col.y + 2));
    app.transcript_rect = tr;
    if tr.height == 0 {
        return;
    }
    let empty = app.sessions[app.active].messages.is_empty();
    if empty {
        let msg = "Start a conversation";
        let mx = tr.x + tr.width.saturating_sub(msg.width() as u16) / 2;
        let my = tr.y + tr.height / 2;
        put(buf, mx, my.saturating_sub(1), msg, bold(), tr);
        let sub = "ctrl+p commands  ·  tab switch mode  ·  ctrl+n new session";
        let sx = tr.x + tr.width.saturating_sub(sub.width() as u16) / 2;
        put(buf, sx, my + 1, sub, dimmer(), tr);
        return;
    }

    transcript::ensure(&mut app.sessions[app.active], &app.cfg, w);
    let s = &mut app.sessions[app.active];
    let running = s.started.as_ref().map(|t| t.elapsed().as_secs());
    let live_rows = if running.is_some() {
        1 + waterfall::HEIGHT as usize
    } else {
        0
    };
    let rows_len = s.cache.as_ref().map_or(0, |c| c.2.len());
    let total = rows_len + live_rows;
    let h = tr.height as usize;
    let max_scroll = total.saturating_sub(h);
    s.scroll_back = s.scroll_back.min(max_scroll);
    let sb = s.scroll_back;
    let start = total.saturating_sub(h + sb);

    // rows that are tool calls become click targets for the details view
    let mut tool_hits: Vec<(Rect, usize, usize)> = Vec::new();
    {
        let tool_map: Vec<(usize, usize, usize)> =
            s.cache.as_ref().map(|c| c.3.clone()).unwrap_or_default();
        for (vy, idx) in (start..(start + h).min(total)).enumerate() {
            let y = tr.y + vy as u16;
            if idx < rows_len {
                let row = &s.cache.as_ref().unwrap().2[idx];
                if let Some((c, a, b)) = row.bg {
                    let bx = tr.x + a.min(tr.width);
                    let bw = b.min(tr.width).saturating_sub(a);
                    tint(buf, Rect::new(bx, y, bw, 1), c);
                }
                put_spans(buf, tr.x, y, &row.spans, tr);
                if let Some((_, mi, bi)) = tool_map.iter().find(|(r, _, _)| *r == idx) {
                    tool_hits.push((Rect::new(tr.x, y, tr.width, 1), *mi, *bi));
                }
            }
        }
    }
    if let Some(secs) = running {
        // live area: blank row, then the working box + state label. Drawn
        // into a scratch buffer and copied row by row so it scrolls in and
        // out of view smoothly (the old code popped it in and out whole).
        let live = s.live();
        let lh = waterfall::HEIGHT;
        let area = Rect::new(0, 0, tr.width, lh);
        let mut sc = Buffer::empty(area);
        fill(&mut sc, area, BG);
        waterfall::draw_box(&mut sc, 0, 0, t, &live, area);
        let lx = waterfall::BOX_W + 2;
        let (_, star_col) = waterfall::star(&live, t);
        let state = waterfall::state_word(&live);
        let mut x1 = put(&mut sc, lx, 1, &state, Style::new().fg(star_col).add_modifier(Modifier::BOLD), area);
        x1 = put(&mut sc, x1, 1, "…", Style::new().fg(star_col), area);
        let _ = x1;
        let mode = s.mode.label();
        let mx = put(&mut sc, lx, 2, mode, accent_bold(), area);
        put(&mut sc, mx, 2, &format!("  ·  {secs}s"), dim(), area);
        let hint = if s.queue.is_empty() {
            "enter queues a query · ctrl+s sends it now"
        } else {
            "ctrl+s sends the selected queued query"
        };
        put(&mut sc, lx, 3, hint, dimmer(), area);
        let top = rows_len + 1;
        for r in 0..lh as usize {
            let idx = top + r;
            if idx < start || idx >= start + h {
                continue;
            }
            let y = tr.y + (idx - start) as u16;
            for xx in 0..tr.width {
                *buf.get_mut(tr.x + xx, y) = sc.get(xx, r as u16).clone();
            }
        }
    }
    app.hits.extend(
        tool_hits
            .into_iter()
            .map(|(rect, mi, bi)| Hit {
                rect,
                action: Action::OpenTool(mi, bi),
                tip: None,
            }),
    );
    if sb > 0 {
        let msg = format!(" ↓ {sb} rows below · ctrl+end ");
        let mx = tr.right().saturating_sub(msg.width() as u16);
        put(
            buf,
            mx,
            tr.bottom() - 1,
            &msg,
            Style::new()
                .fg(TEXT)
                .bg(PANEL_HI)
                .add_modifier(Modifier::BOLD),
            tr,
        );
    }
}
