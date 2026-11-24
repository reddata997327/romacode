//! Tiny stderr logger with timestamps. No external dep.

use std::io::Write;
use std::sync::atomic::{AtomicUsize, Ordering};

static LEVEL: AtomicUsize = AtomicUsize::new(2); // 0=quiet,1=err,2=info,3=debug

pub fn set_level(level: usize) {
    LEVEL.store(level, Ordering::Relaxed);
}

pub fn level() -> usize {
    LEVEL.load(Ordering::Relaxed)
}

fn now() -> String {
    let secs = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    let days = secs / 86400;
    let s = secs % 86400;
    let h = s / 3600;
    let m = (s % 3600) / 60;
    let sec = s % 60;
    // day-zero is 1970-01-01; print only HH:MM:SS plus a relative day counter
    format!("D{}T{:02}:{:02}:{:02}", days, h, m, sec)
}

pub fn log(lvl: usize, tag: &str, msg: &str) {
    if lvl > level() {
        return;
    }
    let prefix = match lvl {
        1 => "ERR",
        3 => "DBG",
        _ => "INF",
    };
    let line = format!("[{}] {} {} {}\n", now(), prefix, tag, msg);
    let _ = std::io::stderr().write_all(line.as_bytes());
}

pub fn info(tag: &str, msg: &str) {
    log(2, tag, msg);
}
pub fn debug(tag: &str, msg: &str) {
    log(3, tag, msg);
}
pub fn err(tag: &str, msg: &str) {
    log(1, tag, msg);
}
