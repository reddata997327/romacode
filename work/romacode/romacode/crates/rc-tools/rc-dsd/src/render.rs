//! The live-timeline renderer: turns the merged D→S→D event buffer into the
//! neat correlated view romacode reads — every API call as one block
//! (entry → exit) with the database queries observed while it was in flight
//! hung underneath it, then background queries and file-level notes.

use std::time::SystemTime;

use crate::session::{call_for, fmt_clock, one_line, DsdEvent, DsdSession};

const QUERY_MAX: usize = 120;
const PATH_MAX: usize = 60;

fn fmt_bytes(n: u64) -> String {
    if n >= 1_048_576 {
        format!("{:.1}MB", n as f64 / 1_048_576.0)
    } else if n >= 1024 {
        format!("{:.1}KB", n as f64 / 1024.0)
    } else {
        format!("{n}B")
    }
}

fn fmt_ms(ms: f64) -> String {
    if ms >= 1000.0 {
        format!("{:.2}s", ms / 1000.0)
    } else {
        format!("{ms:.1}ms")
    }
}

fn pct95(mut v: Vec<f64>) -> f64 {
    if v.is_empty() {
        return 0.0;
    }
    v.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
    let idx = ((v.len() as f64 * 0.95) as usize).min(v.len() - 1);
    v[idx]
}

/// Observer descriptor for the header (plain strings — no live handles).
pub fn observer_lines(s: &DsdSession) -> Vec<String> {
    if !s.observer_labels.is_empty() {
        return s.observer_labels.clone();
    }
    let mut observers: Vec<String> = Vec::new();
    for t in &s.taps {
        observers.push(format!("tap :{} → {}", t.tap_port, t.upstream));
    }
    for m in &s.dbmons {
        observers.push(format!("{}@{}", m.backend, m.target));
    }
    observers
}

/// Render the timeline (everything in `s.events`) as text.
pub fn timeline(s: &DsdSession) -> String {
    let observers = observer_lines(s);
    core_timeline(&observers, &s.events, s.cap)
}

/// Render only what happened inside a watch window (events since `since`).
pub fn window(s: &DsdSession, since: SystemTime) -> String {
    let observers = observer_lines(s);
    let events: Vec<DsdEvent> = s.events.iter().filter(|e| e.ts() >= since).cloned().collect();
    core_timeline(&observers, &events, s.cap)
}

/// The actual renderer over plain data (no live handles).
fn core_timeline(observers: &[String], events: &[DsdEvent], cap: usize) -> String {
    let mut out = String::new();
    let mut entries = 0usize;
    let mut exits = 0usize;
    let mut latencies: Vec<f64> = Vec::new();
    for ev in events {
        match ev {
            DsdEvent::HttpEntry { .. } => entries += 1,
            DsdEvent::HttpExit { ms, .. } => {
                exits += 1;
                latencies.push(*ms);
            }
            _ => {}
        }
    }
    let avg = if latencies.is_empty() { 0.0 } else { latencies.iter().sum::<f64>() / latencies.len() as f64 };
    let queries: Vec<&DsdEvent> = events.iter().filter(|e| matches!(e, DsdEvent::DbQuery { .. })).collect();
    let mut correlated = 0usize;
    for q in &queries {
        let ts = q.ts();
        if call_for(events, ts).is_some() {
            correlated += 1;
        }
    }

    out.push_str("══ dsd live trace — Domain → Server → Database ══\n");
    if observers.is_empty() {
        out.push_str("observers: none attached\n");
    } else {
        out.push_str(&format!("observers: {}\n", observers.join("  ·  ")));
    }
    out.push_str(&format!(
        "api: {entries} in / {exits} out · avg {} · p95 {}\n",
        fmt_ms(avg),
        fmt_ms(pct95(latencies))
    ));
    out.push_str(&format!(
        "db: {} queries ({} correlated to calls, {} background)\n",
        queries.len(),
        correlated,
        queries.len() - correlated
    ));
    out.push_str(&format!("events captured: {} (buffer capped at {})\n", events.len(), cap));

    // ── per-call blocks ──
    out.push_str("── api calls (entry → exit, database hits underneath) ──\n");
    let mut shown_bg = false;
    let mut last_entry: Option<SystemTime> = None;
    for ev in events {
        match ev {
            DsdEvent::HttpEntry { id, ts, method, path, host, .. } => {
                last_entry = Some(*ts);
                let p = one_line(path, PATH_MAX);
                let hostpart = if host.is_empty() { String::new() } else { format!(" ({host})") };
                out.push_str(&format!("\n{}  #{:<4} {:<4} /{}{}\n", fmt_clock(*ts), id, method, p.trim_start_matches('/'), hostpart));
            }
            DsdEvent::HttpExit { id, ts, status, ms, req_bytes, resp_bytes, note } => {
                let mut line = format!("{}  #{:<4} → {}  {}  ↑ {}  ↓ {}", fmt_clock(*ts), id, status, fmt_ms(*ms), fmt_bytes(*req_bytes), fmt_bytes(*resp_bytes));
                if !note.is_empty() {
                    line.push_str(&format!("  [{note}]"));
                }
                out.push_str(&format!("        {line}\n"));
            }
            DsdEvent::DbQuery { ts, backend, pid, user, db, state, ms_so_far, query } => {
                let owner = call_for(events, *ts);
                let scope = match owner {
                    Some(id) => format!("↳ call #{id}"),
                    None => {
                        shown_bg = true;
                        "↳ [background]".to_string()
                    }
                };
                let dbsource = if db.is_empty() { String::new() } else { format!("@{db}") };
                let who = if user.is_empty() { String::new() } else { format!("{user}") };
                out.push_str(&format!(
                    "    {scope}  {} pid={pid} {who}{dbsource} {} {} {}\n",
                    backend,
                    one_line(state, 12),
                    fmt_ms(*ms_so_far),
                    one_line(query, QUERY_MAX)
                ));
                let _ = last_entry;
            }
            DsdEvent::DbNote { ts, backend, text } => {
                out.push_str(&format!("    · [{}] {} {}\n", backend, one_line(text, 160), fmt_clock(*ts)));
            }
        }
    }
    if !shown_bg {
        let _ = shown_bg;
    }
    // open calls (entry without exit) are visible because their exit never
    // rendered — the header counts already say in/out, so nothing to add.
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::session::{self, DsdEvent};
    use std::time::Duration;
    use std::time::UNIX_EPOCH;

    #[test]
    fn bytes_and_ms_formatting() {
        assert_eq!(fmt_bytes(512), "512B");
        assert_eq!(fmt_bytes(2048), "2.0KB");
        assert_eq!(fmt_ms(3.2), "3.2ms");
        assert_eq!(fmt_ms(1500.0), "1.50s");
    }

    #[test]
    fn timeline_shows_call_with_correlated_query() {
        let t0 = UNIX_EPOCH;
        let t1 = t0 + Duration::from_millis(10);
        let t2 = t0 + Duration::from_millis(20);
        let t3 = t0 + Duration::from_millis(30);
        let mut s = DsdSession::default();
        s.events = vec![
            DsdEvent::HttpEntry { id: 7, ts: t1, method: "GET".into(), path: "/api/users".into(), host: "shop.dev".into(), peer: "c".into(), tap_port: 9000, upstream: "u".into() },
            DsdEvent::DbQuery { ts: t2, backend: "pg".into(), pid: "42".into(), user: "app".into(), db: "shop".into(), state: "active".into(), ms_so_far: 2.0, query: "SELECT * FROM users".into() },
            DsdEvent::HttpExit { id: 7, ts: t3, status: 200, ms: 3.2, req_bytes: 1100, resp_bytes: 4800, note: String::new() },
        ];
        let out = timeline(&s);
        assert!(out.contains("GET"), "entry missing");
        assert!(out.contains("#7"), "call id missing");
        assert!(out.contains("↳ call #7"), "correlation missing: {out}");
        assert!(out.contains("SELECT * FROM users"), "query missing");
        assert!(out.contains("200"), "exit status missing");
        assert!(out.contains("1.1KB"), "byte fmt missing");
    }

    #[test]
    fn timeline_marks_background_queries() {
        let t0 = UNIX_EPOCH;
        let t4 = t0 + Duration::from_millis(40);
        let mut s = DsdSession::default();
        s.events = vec![DsdEvent::DbQuery { ts: t4, backend: "pg".into(), pid: "51".into(), user: String::new(), db: String::new(), state: "active".into(), ms_so_far: 1.0, query: "VACUUM".into() }];
        let out = timeline(&s);
        assert!(out.contains("[background]"));
    }

    #[test]
    fn empty_session_renders_header_only() {
        let s = DsdSession::default();
        let out = timeline(&s);
        assert!(out.contains("observers: none attached"));
        assert!(out.contains("api: 0 in / 0 out"));
    }

    #[test]
    fn session_record_dedupes_id_allocation() {
        session::next_call_id();
        session::next_call_id();
        let a = session::next_call_id();
        let b = session::next_call_id();
        assert_eq!(b, a + 1);
    }
}
