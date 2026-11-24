//! Streamed events — everything the brain pushes to attached screens.
//!
//! These are the server-side equivalents of the old in-process
//! `AgentEvent` channel: text deltas, tool starts/ends, approval prompts,
//! usage, device frames. Each event targets one session (or the whole app
//! when `session_id` is `None`).

use crate::protocol::{DeviceStatus, SessionSummary};
use serde::{Deserialize, Serialize};

/// One pushed server event, wrapped in `ServerEnvelope::push`.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum Event {
    /// Assistant text delta for the current turn.
    Text {
        session_id: usize,
        delta: String,
    },
    /// A tool call started.
    ToolStart {
        session_id: usize,
        id: String,
        name: String,
        arg: String,
    },
    /// A tool call finished.
    ToolEnd {
        session_id: usize,
        id: String,
        ok: bool,
        extra: Option<String>,
        /// Full tool output (capped) — the details view reads this.
        #[serde(default)]
        output: Option<String>,
    },
    /// A user message that arrived WHILE the agent was working (ctrl+s
    /// steering). Screens append it to the transcript and open a fresh
    /// assistant block under it; the agent reads it at its next round.
    UserInjected {
        session_id: usize,
        text: String,
    },
    /// Running token totals for the current turn.
    Usage {
        session_id: usize,
        input: u64,
        output: u64,
    },
    /// A state-changing action waits for y/n. No action runs until answered.
    ApprovalRequest {
        session_id: usize,
        request_id: String,
        name: String,
        effect: String,
    },
    /// Informational notice (interrupted, provider switch, snapshot…).
    Notice {
        session_id: Option<usize>,
        text: String,
    },
    /// An error that is not part of the transcript (transport, config…).
    Error {
        session_id: Option<usize>,
        message: String,
    },
    /// The turn finished; `secs` is the wall time of the turn.
    Done {
        session_id: usize,
        secs: f32,
    },
    /// Full state of one session — on create, select, or reconnect.
    Snapshot {
        session: crate::protocol::SessionSnapshot,
    },
    /// Refresh of the tab list (create/close/external change).
    Sessions {
        sessions: Vec<SessionSummary>,
    },
    /// One committed Android device frame, PNG-encoded, base64-wrapped.
    /// Throttled server-side; `gen` monotonically increases.
    Frame {
        gen: u64,
        w: u32,
        h: u32,
        png_base64: String,
    },
    /// Current device facts (phase, serial, model, battery…).
    Device {
        status: DeviceStatus,
    },
}

impl Event {
    /// Which session this event belongs to, if any.
    pub fn session_id(&self) -> Option<usize> {
        match self {
            Event::Text { session_id, .. }
            | Event::ToolStart { session_id, .. }
            | Event::ToolEnd { session_id, .. }
            | Event::UserInjected { session_id, .. }
            | Event::Usage { session_id, .. }
            | Event::ApprovalRequest { session_id, .. }
            | Event::Done { session_id, .. }
            | Event::Snapshot {
                session: crate::protocol::SessionSnapshot { id: session_id, .. },
            } => Some(*session_id),
            Event::Notice { session_id, .. }
            | Event::Error { session_id, .. } => *session_id,
            Event::Sessions { .. }
            | Event::Frame { .. }
            | Event::Device { .. } => None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn roundtrip_and_routing() {
        let e = Event::Text {
            session_id: 4,
            delta: "hi".into(),
        };
        let v = serde_json::to_string(&e).unwrap();
        let back: Event = serde_json::from_str(&v).unwrap();
        assert_eq!(back.session_id(), Some(4));
        let notice = Event::Notice {
            session_id: None,
            text: "ping".into(),
        };
        assert_eq!(notice.session_id(), None);
        let frame = Event::Frame {
            gen: 1,
            w: 2,
            h: 3,
            png_base64: "aGk=".into(),
        };
        assert_eq!(frame.session_id(), None);
    }
}
