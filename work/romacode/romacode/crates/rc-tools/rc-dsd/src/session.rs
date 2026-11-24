//! DSD session state: the merged Domain→Server→Database event timeline.
//!
//! Every observer (HTTP taps, database pollers) pushes [`DsdEvent`]s into one
//! ring buffer guarded by a global mutex. `dsd.watch` renders the tail of that
//! timeline as a correlated live view: each database query observed while an
//! API call was in flight is attached to that call (entry→exit window), so
//! romacode sees the full journey of a request — network entry, server work,
//! database hits, network exit — in one place.

use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Mutex, OnceLock};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

/// One observed moment in the Domain→Server→Database journey.
#[derive(Debug, Clone)]
pub enum DsdEvent {
    /// Request reached the tap (domain entry).
    HttpEntry {
        id: u64,
        ts: SystemTime,
        method: String,
        path: String,
        host: String,
        peer: String,
        tap_port: u16,
        upstream: String,
    },
    /// Response left the upstream back through the tap (exit).
    HttpExit {
        id: u64,
        ts: SystemTime,
        status: u16,
        ms: f64,
        req_bytes: u64,
        resp_bytes: u64,
        note: String,
    },
    /// A query observed running on an attached database.
    DbQuery {
        ts: SystemTime,
        backend: String,
        pid: String,
        user: String,
        db: String,
        state: String,
        ms_so_far: f64,
        /// Set at render time: the in-flight API call this query belongs to.
        query: String,
    },
    /// A non-query observation (sqlite file change, poller notices).
    DbNote { ts: SystemTime, backend: String, text: String },
}

impl DsdEvent {
    pub fn ts(&self) -> SystemTime {
        match self {
            DsdEvent::HttpEntry { ts, .. }
            | DsdEvent::HttpExit { ts, .. }
            | DsdEvent::DbQuery { ts, .. }
            | DsdEvent::DbNote { ts, .. } => *ts,
        }
    }
}

/// A running reverse-proxy tap (domain entry point).
pub struct TapHandle {
    pub tap_port: u16,
    pub upstream: String,
    pub stop: std::sync::Arc<AtomicBool>,
    pub task: tokio::task::JoinHandle<()>,
}

/// A running database poller thread.
pub struct DbMonHandle {
    pub backend: String,
    pub target: String,
    pub stop: std::sync::Arc<AtomicBool>,
    pub thread: Option<std::thread::JoinHandle<()>>,
}

/// The live DSD session: taps, monitors, and the event timeline.
pub struct DsdSession {
    pub taps: Vec<TapHandle>,
    pub dbmons: Vec<DbMonHandle>,
    /// Human-readable observer descriptors (survives watch snapshots,
    /// which drop the live handles).
    pub observer_labels: Vec<String>,
    pub events: Vec<DsdEvent>,
    pub next_call_id: AtomicU64,
    /// Ring-buffer cap so a chatty stack cannot exhaust memory.
    pub cap: usize,
}

impl Default for DsdSession {
    fn default() -> Self {
        Self {
            taps: Vec::new(),
            dbmons: Vec::new(),
            observer_labels: Vec::new(),
            events: Vec::new(),
            next_call_id: AtomicU64::new(1),
            cap: 5_000,
        }
    }
}

static SESSION: OnceLock<Mutex<DsdSession>> = OnceLock::new();

pub fn session() -> &'static Mutex<DsdSession> {
    SESSION.get_or_init(|| Mutex::new(DsdSession::default()))
}

pub fn next_call_id() -> u64 {
    session().lock().unwrap().next_call_id.fetch_add(1, Ordering::Relaxed)
}

/// Record one event (from any observer thread/task).
pub fn record(ev: DsdEvent) {
    let mut s = session().lock().unwrap();
    if s.events.len() >= s.cap {
        let drop_n = s.cap / 5;
        s.events.drain(0..drop_n); // drop the oldest fifth, amortised
    }
    s.events.push(ev);
}

/// Allocate an id and record an entry in one step (tap side).
pub fn record_entry(method: &str, path: &str, host: &str, peer: &str, tap_port: u16, upstream: &str) -> u64 {
    let id = next_call_id();
    record(DsdEvent::HttpEntry {
        id,
        ts: SystemTime::now(),
        method: method.to_string(),
        path: path.to_string(),
        host: host.to_string(),
        peer: peer.to_string(),
        tap_port,
        upstream: upstream.to_string(),
    });
    id
}

pub fn now_ms() -> u128 {
    SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_millis()).unwrap_or(0)
}

pub fn fmt_clock(ts: SystemTime) -> String {
    let ms = ts.duration_since(UNIX_EPOCH).map(|d| d.as_millis()).unwrap_or(0);
    let secs_total = ms / 1000;
    let millis = ms % 1000;
    let secs = secs_total % 60;
    let mins = (secs_total / 60) % 60;
    let hours = (secs_total / 3600) % 24;
    format!("{hours:02}:{mins:02}:{secs:02}.{millis:03}")
}

/// Truncate a query for one-line display.
pub fn one_line(s: &str, max: usize) -> String {
    let flat: String = s.split_whitespace().collect::<Vec<_>>().join(" ");
    if flat.chars().count() <= max {
        flat
    } else {
        flat.chars().take(max).collect::<String>() + "…"
    }
}

/// Correlate a query timestamp to the in-flight API call whose entry→exit
/// window covers it. Linear over recent events — the buffer is capped and
/// this runs once per rendered row.
pub fn call_for(events: &[DsdEvent], at: SystemTime) -> Option<u64> {
    let mut open: Option<u64> = None;
    for ev in events {
        match ev {
            DsdEvent::HttpEntry { id, ts, .. } => {
                if *ts <= at {
                    open = Some(*id);
                }
            }
            DsdEvent::HttpExit { id, ts, .. } => {
                if let Some(o) = open {
                    if o == *id && *ts >= at {
                        return Some(*id);
                    }
                    if *ts > at {
                        open = None;
                    }
                }
            }
            _ => {}
        }
    }
    None // still in flight (no exit yet) or background
}

/// Stop every tap and monitor and return a one-line summary.
pub fn stop_all() -> String {
    let mut s = session().lock().unwrap();
    let mut stopped = 0usize;
    for t in &s.taps {
        t.stop.store(true, Ordering::Relaxed);
        t.task.abort();
        stopped += 1;
    }
    for m in &s.dbmons {
        m.stop.store(true, Ordering::Relaxed);
        stopped += 1;
    }
    let n = s.events.len();
    s.taps.clear();
    s.dbmons.clear();
    s.observer_labels.clear();
    s.events.clear();
    format!("stopped {stopped} observer(s); {n} event(s) in the last session")
}

/// Wait for `duration_ms`, then return the events observed in that window.
pub fn collect_window(duration_ms: u64) -> Vec<DsdEvent> {
    let deadline = std::time::Instant::now() + Duration::from_millis(duration_ms.min(60_000));
    while std::time::Instant::now() < deadline {
        std::thread::sleep(Duration::from_millis(100));
    }
    session().lock().unwrap().events.clone()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    fn entry(id: u64, ts: SystemTime) -> DsdEvent {
        DsdEvent::HttpEntry {
            id,
            ts,
            method: "GET".into(),
            path: "/".into(),
            host: "d".into(),
            peer: "p".into(),
            tap_port: 9000,
            upstream: "u".into(),
        }
    }
    fn exit(id: u64, ts: SystemTime) -> DsdEvent {
        DsdEvent::HttpExit { id, ts, status: 200, ms: 1.0, req_bytes: 0, resp_bytes: 0, note: String::new() }
    }
    fn query(ts: SystemTime) -> DsdEvent {
        DsdEvent::DbQuery {
            ts,
            backend: "pg".into(),
            pid: "1".into(),
            user: "u".into(),
            db: "d".into(),
            state: "active".into(),
            ms_so_far: 1.0,
            query: "SELECT 1".into(),
        }
    }

    #[test]
    fn query_inside_entry_exit_window_correlates() {
        let t0 = UNIX_EPOCH;
        let t1 = t0 + Duration::from_millis(10);
        let t2 = t0 + Duration::from_millis(20);
        let t3 = t0 + Duration::from_millis(30);
        let events = vec![entry(7, t1), query(t2), exit(7, t3)];
        assert_eq!(call_for(&events, t2), Some(7));
    }

    #[test]
    fn query_outside_any_window_is_background() {
        let t0 = UNIX_EPOCH;
        let t1 = t0 + Duration::from_millis(10);
        let t2 = t0 + Duration::from_millis(20);
        let t4 = t0 + Duration::from_millis(40);
        let events = vec![entry(7, t1), exit(7, t2), query(t4)];
        assert_eq!(call_for(&events, t4), None);
    }

    #[test]
    fn clock_format_is_hhmmss_mmm() {
        let ts = UNIX_EPOCH + Duration::from_millis(3_723_004);
        assert_eq!(fmt_clock(ts), "01:02:03.004");
    }

    #[test]
    fn one_line_flattens_and_truncates() {
        assert_eq!(one_line("SELECT  a\n  FROM  t", 100), "SELECT a FROM t");
        assert_eq!(one_line("abcdefgh", 4), "abcd…");
    }
}
