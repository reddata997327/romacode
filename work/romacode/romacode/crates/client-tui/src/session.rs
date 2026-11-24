//! The screen's mirror of a server-owned session: the transcript blocks,
//! token counters, todo/file sidebars (all arriving as wire snapshots and
//! events) plus the strictly-local bits — the input box, scroll position and
//! the render cache.
//!
//! `apply` consumes the same `Event` stream the server uses to update its
//! own `session::Session`, so brain and screen can never drift apart.

use crate::device::Frame;
use crate::input::Input;
use crate::ui::draw::Row;
use romacode_shared::events::Event;
use std::time::{Duration, Instant};

// The ui/ layer imports these names from `crate::session` — keep the
// namespace stable while the data itself moved to the shared protocol crate.
pub use romacode_shared::protocol::{
    Block, DeviceStatus, FileChange, McpServer, Mode, Msg, SessionSnapshot, Todo, TodoState,
    ToolStatus,
};

/// What the model is doing right now — drives the star dot in the working box.
#[derive(Clone, PartialEq, Eq, Debug)]
pub enum Live {
    /// Request sent, nothing back yet.
    Thinking,
    /// Answer text is arriving.
    Streaming,
    /// A tool call is running.
    Tool(String),
    /// Between steps (tool finished, next round not started).
    Waiting,
}

/// Queries typed while the agent works are parked here until sent.
pub const QUEUE_CAP: usize = 20;

/// Transcript row → (message index, block index) for every tool row.
pub type ToolMap = Vec<(usize, usize, usize)>;

pub struct UiSession {
    pub id: usize,
    pub title: String,
    pub messages: Vec<Msg>,
    pub mode: Mode,
    /// Live turn timer, started when the client sends a message and cleared
    /// on the matching `Done` event.
    pub started: Option<Instant>,
    pub input: Input,
    /// Rows scrolled up from the bottom (0 = follow the stream).
    pub scroll_back: usize,
    pub tokens_in: u64,
    pub tokens_out: u64,
    pub turns: u32,
    pub todos: Vec<Todo>,
    pub mcp: Vec<McpServer>,
    pub files: Vec<FileChange>,
    pub lsp: Vec<(String, String)>,
    /// Bumped whenever the transcript changes (cache invalidation).
    pub rev: u64,
    /// Rendered rows cache: (width, revision, rows, tool map).
    /// The tool map lists every TOOL row as (row index, message index,
    /// block index) so mouse clicks can open the details view.
    pub cache: Option<(u16, u64, Vec<Row>, ToolMap)>,
    /// Queries typed during a task, waiting for ctrl+s (send to the agent).
    pub queue: Vec<String>,
    pub queue_sel: usize,
    /// When the last answer-text delta arrived (star dot twinkles on it).
    pub last_delta: Option<Instant>,
}

impl UiSession {
    pub fn new(id: usize) -> Self {
        Self {
            id,
            title: format!("Session {id}"),
            messages: Vec::new(),
            mode: Mode::Build,
            started: None,
            input: Input::default(),
            scroll_back: 0,
            tokens_in: 0,
            tokens_out: 0,
            turns: 0,
            todos: Vec::new(),
            mcp: Vec::new(),
            files: Vec::new(),
            lsp: Vec::new(),
            rev: 0,
            cache: None,
            queue: Vec::new(),
            queue_sel: 0,
            last_delta: None,
        }
    }

    #[allow(dead_code)] // convenience for tests / future reconnect paths
    pub fn from_snapshot(snap: SessionSnapshot) -> Self {
        let mut s = Self::new(snap.id);
        s.ingest_snapshot(snap);
        s
    }

    /// Replace server-owned state with a snapshot; keep local input/scroll.
    pub fn ingest_snapshot(&mut self, snap: SessionSnapshot) {
        self.id = snap.id;
        self.title = snap.title;
        self.messages = snap.messages;
        self.mode = snap.mode;
        self.started = if snap.running {
            self.started.or_else(|| Some(Instant::now()))
        } else {
            None
        };
        self.tokens_in = snap.tokens_in;
        self.tokens_out = snap.tokens_out;
        self.turns = snap.turns;
        self.todos = snap.todos;
        self.mcp = snap.mcp;
        self.files = snap.files;
        self.lsp = snap.lsp;
        self.touch();
    }

    pub fn touch(&mut self) {
        self.rev += 1;
    }

    pub fn is_running(&self) -> bool {
        self.started.is_some()
    }

    /// Real-time model state for the working box, read off the transcript tail.
    pub fn live(&self) -> Live {
        if let Some(Msg::Assistant { blocks, .. }) = self.messages.last() {
            if let Some(name) = blocks.iter().rev().find_map(|b| match b {
                Block::Tool {
                    name,
                    status: ToolStatus::Running,
                    ..
                } => Some(name.clone()),
                _ => None,
            }) {
                return Live::Tool(name);
            }
            if self
                .last_delta
                .map_or(false, |t| t.elapsed() < Duration::from_millis(700))
            {
                return Live::Streaming;
            }
            if blocks.is_empty() {
                return Live::Thinking;
            }
            return Live::Waiting;
        }
        Live::Thinking
    }

    pub fn tokens_total(&self) -> u64 {
        self.tokens_in + self.tokens_out
    }

    fn blocks_mut(&mut self) -> Option<&mut Vec<Block>> {
        match self.messages.last_mut() {
            Some(Msg::Assistant { blocks, .. }) => Some(blocks),
            _ => None,
        }
    }

    /// Ensure the transcript tail is an assistant block, opening one when the
    /// snapshot landed mid-turn with User/Notice as the last message. Without
    /// this fallback, every streamed Text/ToolStart/ToolEnd delta was dropped
    /// until the next snapshot arrived.
    fn ensure_assistant_tail(&mut self) {
        if self.blocks_mut().is_none() {
            let mode = self.mode;
            self.messages.push(Msg::Assistant {
                blocks: Vec::new(),
                model: String::new(),
                mode,
                secs: None,
            });
        }
    }

    /// Optimistic local echo: the user prompt + an empty assistant block.
    /// The next snapshot from the server replaces all of this canonically.
    pub fn begin_turn(&mut self, text: &str, model: &str) {
        self.title = if self.turns == 0 {
            let t: String = text.lines().next().unwrap_or("").chars().take(30).collect();
            if t.trim().is_empty() {
                self.title.clone()
            } else {
                t.trim().to_string()
            }
        } else {
            self.title.clone()
        };
        self.messages.push(Msg::User {
            text: text.to_string(),
        });
        let mode = self.mode;
        self.messages.push(Msg::Assistant {
            blocks: Vec::new(),
            model: model.to_string(),
            mode,
            secs: None,
        });
        self.started = Some(Instant::now());
        self.scroll_back = 0;
        self.touch();
    }

    /// Apply one streamed event; returns true when something changed.
    pub fn apply(&mut self, ev: &Event) -> bool {
        match ev {
            Event::Text { delta, .. } => {
                self.last_delta = Some(Instant::now());
                self.ensure_assistant_tail();
                if let Some(blocks) = self.blocks_mut() {
                    match blocks.last_mut() {
                        Some(Block::Text { text: t }) => t.push_str(delta),
                        _ => blocks.push(Block::Text {
                            text: delta.to_string(),
                        }),
                    }
                }
                self.touch();
                true
            }
            Event::ToolStart { id, name, arg, .. } => {
                self.ensure_assistant_tail();
                if let Some(blocks) = self.blocks_mut() {
                    blocks.push(Block::Tool {
                        id: id.clone(),
                        name: name.clone(),
                        arg: arg.clone(),
                        status: ToolStatus::Running,
                        extra: None,
                        output: None,
                    });
                }
                self.touch();
                true
            }
            Event::ToolEnd {
                id,
                ok,
                extra,
                output,
                ..
            } => {
                if let Some(blocks) = self.blocks_mut() {
                    for b in blocks.iter_mut() {
                        if let Block::Tool {
                            id: bid,
                            status,
                            extra: ex,
                            output: out,
                            ..
                        } = b
                        {
                            if bid == id {
                                *status = if *ok {
                                    ToolStatus::Ok
                                } else {
                                    ToolStatus::Failed
                                };
                                *ex = extra.clone();
                                *out = output.clone();
                            }
                        }
                    }
                }
                self.touch();
                true
            }
            Event::Usage { input, output, .. } => {
                self.tokens_in = self.tokens_in.max(*input);
                self.tokens_out = self.tokens_out.max(*output);
                self.touch();
                true
            }
            Event::ApprovalRequest { effect, .. } => {
                self.messages.push(Msg::Notice {
                    text: format!(
                        "APPROVAL REQUIRED — exact pending effect:\n{effect}\nPress y to allow once, n to deny. No action runs until allowed."
                    ),
                });
                self.touch();
                true
            }
            Event::UserInjected { text, .. } => {
                // A message sent mid-run: it lands in the transcript now and
                // the rest of the stream continues in a fresh block below it.
                let (model, mode) = self
                    .messages
                    .iter()
                    .rev()
                    .find_map(|m| match m {
                        Msg::Assistant { model, mode, .. } => Some((model.clone(), *mode)),
                        _ => None,
                    })
                    .unwrap_or_else(|| (String::new(), self.mode));
                self.messages.push(Msg::User { text: text.clone() });
                self.messages.push(Msg::Assistant {
                    blocks: Vec::new(),
                    model,
                    mode,
                    secs: None,
                });
                self.started = self.started.or_else(|| Some(Instant::now()));
                self.scroll_back = 0;
                self.touch();
                true
            }
            Event::Notice { text, .. } => {
                // Esc pushes a local "Interrupted" AND the brain broadcasts
                // one: the transcript used to show the line twice.
                if text == "Interrupted"
                    && matches!(self.messages.last(), Some(Msg::Notice { text: t }) if t == "Interrupted")
                {
                    return false;
                }
                self.messages.push(Msg::Notice { text: text.clone() });
                self.touch();
                true
            }
            Event::Error { message, .. } => {
                self.messages.push(Msg::Error {
                    text: message.clone(),
                });
                self.touch();
                true
            }
            Event::Done { secs, .. } => {
                if let Some(started) = self.started.take() {
                    let elapsed = started.elapsed().as_secs_f32();
                    let secs = if *secs > 0.01 { *secs } else { elapsed };
                    if let Some(Msg::Assistant { secs: s, .. }) = self.messages.last_mut() {
                        *s = Some(secs);
                    }
                    self.turns += 1;
                }
                self.touch();
                true
            }
            Event::Snapshot { session } => {
                self.ingest_snapshot(session.clone());
                true
            }
            Event::Sessions { .. } | Event::Frame { .. } | Event::Device { .. } => false,
        }
    }
}

/// App-level device mirror: server status + the latest frame from `Frame`
/// events (also published into the shared `FrameHub` for the desktop window).
#[derive(Default)]
pub struct DeviceView {
    pub status: Option<DeviceStatus>,
    pub frame: Option<Frame>,
    pub streaming: bool,
}

impl DeviceView {
    /// True when the device reports a usable online state.
    #[allow(dead_code)] // used by the fullscreen panel once key pages exist
    pub fn online(&self) -> bool {
        self.status
            .as_ref()
            .map(|s| s.phase == "online")
            .unwrap_or(false)
    }

    pub fn error(&self) -> Option<String> {
        self.status.as_ref().and_then(|s| {
            s.phase
                .strip_prefix("error:")
                .map(|e| e.to_string())
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn snap() -> SessionSnapshot {
        SessionSnapshot {
            id: 1,
            title: "t".into(),
            messages: vec![Msg::User { text: "hi".into() }],
            mode: Mode::Build,
            running: false,
            tokens_in: 0,
            tokens_out: 0,
            turns: 0,
            todos: vec![],
            mcp: vec![],
            files: vec![],
            lsp: vec![],
            pending_approvals: vec![],
        }
    }

    #[test]
    fn snapshot_replaces_transcript_keeps_local() {
        let mut s = UiSession::new(1);
        s.input.insert('x');
        s.scroll_back = 4;
        let mut snap = snap();
        snap.messages.push(Msg::Notice { text: "n".into() });
        s.ingest_snapshot(snap);
        assert_eq!(s.messages.len(), 2);
        assert_eq!(s.scroll_back, 4);
        assert!(!s.input.is_empty());
    }

    #[test]
    fn events_build_transcript() {
        let mut s = UiSession::new(1);
        s.begin_turn("do it", "m");
        s.apply(&Event::Text {
            session_id: 1,
            delta: "hello".into(),
        });
        s.apply(&Event::ToolStart {
            session_id: 1,
            id: "1".into(),
            name: "Read".into(),
            arg: "{}".into(),
        });
        s.apply(&Event::ToolEnd {
            session_id: 1,
            id: "1".into(),
            ok: true,
            extra: Some("ok".into()),
            output: Some("full command output".into()),
        });
        s.apply(&Event::Done {
            session_id: 1,
            secs: 1.0,
        });
        assert!(!s.is_running());
        assert_eq!(s.turns, 1);
        match &s.messages[1] {
            Msg::Assistant { blocks, secs, .. } => {
                assert_eq!(blocks.len(), 2);
                assert!(secs.is_some());
            }
            other => panic!("wrong msg {other:?}"),
        }
    }

    #[test]
    fn device_view_phase_strings() {
        let mut v = DeviceView::default();
        v.status = Some(DeviceStatus {
            phase: "error:no device".into(),
            ..Default::default()
        });
        assert!(!v.online());
        assert_eq!(v.error().as_deref(), Some("no device"));
        v.status.as_mut().unwrap().phase = "online".into();
        assert!(v.online());
        assert_eq!(v.error(), None);
    }

    #[test]
    fn user_injected_event_lands_in_chat_and_opens_a_fresh_block() {
        let mut s = UiSession::new(1);
        s.begin_turn("first task", "m");
        s.apply(&Event::Text {
            session_id: 1,
            delta: "working on it".into(),
        });
        s.apply(&Event::UserInjected {
            session_id: 1,
            text: "also fix the footer".into(),
        });
        s.apply(&Event::Text {
            session_id: 1,
            delta: "ok, adding that".into(),
        });
        assert!(s.is_running());
        // user, assistant, user(steer), assistant — the new text sits BELOW it
        assert_eq!(s.messages.len(), 4);
        assert!(matches!(&s.messages[2], Msg::User { text } if text == "also fix the footer"));
        match &s.messages[3] {
            Msg::Assistant { blocks, model, .. } => {
                assert_eq!(model, "m");
                assert!(matches!(&blocks[0], Block::Text { text } if text == "ok, adding that"));
            }
            other => panic!("wrong msg {other:?}"),
        }
    }

    #[test]
    fn interrupted_notice_is_not_duplicated() {
        let mut s = UiSession::new(1);
        s.messages.push(Msg::Notice {
            text: "Interrupted".into(),
        });
        let changed = s.apply(&Event::Notice {
            session_id: Some(1),
            text: "Interrupted".into(),
        });
        assert!(!changed);
        assert_eq!(s.messages.len(), 1);
        // other notices are never swallowed
        s.apply(&Event::Notice {
            session_id: Some(1),
            text: "something else".into(),
        });
        assert_eq!(s.messages.len(), 2);
    }

    #[test]
    fn live_state_follows_the_response() {
        let mut s = UiSession::new(1);
        s.begin_turn("go", "m");
        assert_eq!(s.live(), Live::Thinking);
        s.apply(&Event::Text {
            session_id: 1,
            delta: "hi".into(),
        });
        assert_eq!(s.live(), Live::Streaming);
        s.apply(&Event::ToolStart {
            session_id: 1,
            id: "1".into(),
            name: "Read".into(),
            arg: "{}".into(),
        });
        assert_eq!(s.live(), Live::Tool("Read".into()));
        s.apply(&Event::ToolEnd {
            session_id: 1,
            id: "1".into(),
            ok: true,
            extra: None,
            output: Some("ls -la result".into()),
        });
        s.last_delta = None;
        assert_eq!(s.live(), Live::Waiting);
    }
}
