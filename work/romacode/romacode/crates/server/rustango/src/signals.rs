//! Signals — `django.core.signals` receiver/dispatcher.
//!
//! Signals are global named events with `table`/`pk` payloads for model
//! lifecycle signals and a free-form note for request signals:
//!
//! ```ignore
//! use rustango::signals;
//!
//! signals::connect("post_save", |payload| {
//!     if payload.table == "polls_question" {
//!         println!("question {} saved", payload.pk);
//!     }
//! });
//! ```

use std::sync::{Mutex, OnceLock};

/// Payload carried by every signal emission.
#[derive(Debug, Clone)]
pub struct SignalPayload {
    /// Signal name: `pre_save`, `post_save`, `pre_delete`, `post_delete`,
    /// `request_started`, `request_finished`, `got_request_exception`.
    pub name: &'static str,
    /// Model table the signal relates to ("" for request signals).
    pub table: String,
    /// Model primary key (0 when not applicable).
    pub pk: i64,
    /// Extra note (e.g. the request path for request signals).
    pub note: String,
}

type Receiver = Box<dyn Fn(&SignalPayload) + Send + Sync>;

struct SignalRegistry {
    receivers: Vec<Receiver>,
}

static REGISTRY: OnceLock<Mutex<SignalRegistry>> = OnceLock::new();

fn registry() -> &'static Mutex<SignalRegistry> {
    REGISTRY.get_or_init(|| Mutex::new(SignalRegistry { receivers: Vec::new() }))
}

/// Register a receiver for a signal name — `signal.connect(receiver)`.
pub fn connect<F>(name: &'static str, f: F)
where
    F: Fn(&SignalPayload) + Send + Sync + 'static,
{
    let mut reg = registry().lock().unwrap();
    let filter_name = name;
    reg.receivers.push(Box::new(move |payload| {
        if payload.name == filter_name {
            f(payload);
        }
    }));
}

/// Register a receiver that fires for every signal (like connecting to
/// several `django.db.models.signals` at once).
pub fn connect_all<F>(f: F)
where
    F: Fn(&SignalPayload) + Send + Sync + 'static,
{
    registry().lock().unwrap().receivers.push(Box::new(f));
}

/// Number of registered receivers (tests/metrics).
pub fn receiver_count() -> usize {
    registry().lock().unwrap().receivers.len()
}

/// Fire a signal — `Model.save()` sends `pre_save`/`post_save`, the request
/// cycle sends `request_started`/`request_finished`. Receiver panics are
/// caught and reported to `tracing`-style stderr, never propagated into the
/// request (matching Django's `send_robust` spirit).
pub fn send(name: &'static str, table: &str, pk: i64, note: &str) {
    let payload = SignalPayload {
        name,
        table: table.to_string(),
        pk,
        note: note.to_string(),
    };
    let reg = registry().lock().unwrap();
    for rx in &reg.receivers {
        let _ = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| rx(&payload)));
    }
}

/// Model-lifecycle shortcuts used by the ORM.
pub fn emit(name: &'static str, table: &str, pk: i64) {
    send(name, table, pk, "");
}

pub fn emit_request(note: &str, name: &'static str) {
    send(name, "", 0, note);
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::Arc;

    #[test]
    fn connect_and_send() {
        let hits = Arc::new(AtomicUsize::new(0));
        let h2 = hits.clone();
        connect("post_save", move |p| {
            if p.table == "test_a" {
                h2.fetch_add(1, Ordering::SeqCst);
            }
        });
        emit("post_save", "test_a", 1);
        emit("post_save", "test_b", 1);
        emit("pre_save", "test_a", 1);
        assert_eq!(hits.load(Ordering::SeqCst), 1);
    }

    #[test]
    fn connect_all_fires_for_everything() {
        let hits = Arc::new(AtomicUsize::new(0));
        let h2 = hits.clone();
        connect_all(move |_| {
            h2.fetch_add(1, Ordering::SeqCst);
        });
        emit("pre_save", "x", 0);
        emit_request("/path/", "request_started");
        assert!(hits.load(Ordering::SeqCst) >= 2);
    }

    #[test]
    fn panicking_receivers_do_not_break_dispatch() {
        connect("got_request_exception", |_| panic!("boom"));
        // must not panic
        emit_request("err", "got_request_exception");
    }
}
