//! Server-owned sessions: the transcript, the running turn, token counters —
//! and the plain-text `history()` the model sees. The data types themselves
//! live in `romacode-shared` (the wire contract); this module owns the live
//! instances and applies the same event stream that clients receive, so a
//! reconnecting screen can resync from `snapshot()` at any time.

use romacode_shared::events::Event;
use romacode_shared::protocol::{
    Block, FileChange, McpServer, Msg, SessionSnapshot, SessionSummary, Todo, ToolStatus,
};
/// The wire `Mode` is the session `Mode` — one type, both worlds.
pub use romacode_shared::protocol::Mode;
use std::sync::{
    atomic::{AtomicBool, Ordering},
    Arc, Mutex,
};
use std::time::Instant;

/// One in-flight agent turn.
pub struct Run {
    pub cancel: Arc<AtomicBool>,
    pub started: Instant,
    pub model: String,
    /// Steering messages (ctrl+s) the agent has not read yet. The agent loop
    /// drains this between model rounds; anything left when the turn ends is
    /// replayed as a follow-up turn by the hub.
    pub steer: Arc<Mutex<Vec<String>>>,
}

impl Run {
    pub fn interrupted(&self) -> bool {
        self.cancel.load(Ordering::Relaxed)
    }
}

pub struct Session {
    pub id: usize,
    pub title: String,
    pub titled: bool,
    pub messages: Vec<Msg>,
    pub mode: Mode,
    pub run: Option<Run>,
    pub tokens_in: u64,
    pub tokens_out: u64,
    pub turns: u32,
    pub todos: Vec<Todo>,
    pub mcp: Vec<McpServer>,
    pub files: Vec<FileChange>,
    pub lsp: Vec<(String, String)>,
}

impl Session {
    pub fn new(id: usize) -> Self {
        Self {
            id,
            title: format!("Session {id}"),
            titled: false,
            messages: Vec::new(),
            mode: Mode::Build,
            run: None,
            tokens_in: 0,
            tokens_out: 0,
            turns: 0,
            todos: Vec::new(),
            mcp: Vec::new(),
            files: Vec::new(),
            lsp: Vec::new(),
        }
    }

    /// Rebuild a live session from a persisted snapshot. A run is never
    /// restored as "running": the process that owned the cancel flag is gone.
    pub fn from_snapshot(s: &SessionSnapshot) -> Self {
        Self {
            id: s.id,
            title: s.title.clone(),
            titled: !s.title.starts_with("Session "),
            messages: s.messages.clone(),
            mode: s.mode,
            run: None,
            tokens_in: s.tokens_in,
            tokens_out: s.tokens_out,
            turns: s.turns,
            todos: s.todos.clone(),
            mcp: s.mcp.clone(),
            files: s.files.clone(),
            lsp: s.lsp.clone(),
        }
    }

    pub fn is_running(&self) -> bool {
        self.run.is_some()
    }

    pub fn tokens_total(&self) -> u64 {
        self.tokens_in + self.tokens_out
    }

    pub fn summary(&self) -> SessionSummary {
        SessionSummary {
            id: self.id,
            title: self.title.clone(),
            mode: self.mode,
            running: self.is_running(),
        }
    }

    /// Full serialisable state for a (re)connecting client. `pending_approvals`
    /// is filled in by the api layer (`Hub::snapshot`), which owns the live
    /// approval registry.
    pub fn snapshot(&self) -> SessionSnapshot {
        SessionSnapshot {
            id: self.id,
            title: self.title.clone(),
            messages: self.messages.clone(),
            mode: self.mode,
            running: self.is_running(),
            tokens_in: self.tokens_in,
            tokens_out: self.tokens_out,
            turns: self.turns,
            todos: self.todos.clone(),
            mcp: self.mcp.clone(),
            files: self.files.clone(),
            lsp: self.lsp.clone(),
            pending_approvals: Vec::new(),
        }
    }

    fn auto_title(&mut self, text: &str) {
        if self.titled {
            return;
        }
        let t: String = text.lines().next().unwrap_or("").chars().take(30).collect();
        if !t.trim().is_empty() {
            self.title = t.trim().to_string();
            self.titled = true;
        }
    }

    pub fn push_user(&mut self, text: String) {
        self.auto_title(&text);
        self.messages.push(Msg::User { text });
    }

    pub fn begin_assistant(&mut self, model: &str) {
        let mode = self.mode;
        self.messages.push(Msg::Assistant {
            blocks: Vec::new(),
            model: model.to_string(),
            mode,
            secs: None,
        });
    }

    /// Reopen an assistant block after a Notice/Error message. Without this,
    /// any streamed text after a notice (compaction, retry, recovery — all
    /// added for durability) was silently dropped from the canonical and
    /// persisted transcript, even though the screen kept showing it.
    fn ensure_assistant_tail(&mut self) {
        if matches!(self.messages.last(), Some(Msg::Assistant { .. })) {
            return;
        }
        let model = self
            .messages
            .iter()
            .rev()
            .find_map(|m| match m {
                Msg::Assistant { model, .. } => Some(model.clone()),
                _ => None,
            })
            .unwrap_or_default();
        let mode = self.mode;
        self.messages.push(Msg::Assistant {
            blocks: Vec::new(),
            model,
            mode,
            secs: None,
        });
    }

    fn blocks_mut(&mut self) -> Option<&mut Vec<Block>> {
        self.ensure_assistant_tail();
        match self.messages.last_mut() {
            Some(Msg::Assistant { blocks, .. }) => Some(blocks),
            _ => None,
        }
    }

    fn append_text(&mut self, s: &str) {
        if let Some(blocks) = self.blocks_mut() {
            match blocks.last_mut() {
                Some(Block::Text { text: t }) => t.push_str(s),
                _ => blocks.push(Block::Text {
                    text: s.to_string(),
                }),
            }
        }
    }

    fn tool_start(&mut self, id: String, name: String, arg: String) {
        if let Some(blocks) = self.blocks_mut() {
            blocks.push(Block::Tool {
                id,
                name,
                arg,
                status: ToolStatus::Running,
                extra: None,
                output: None,
            });
        }
    }

    fn tool_end(&mut self, tid: &str, ok: bool, extra: Option<String>, output: Option<String>) {
        if let Some(blocks) = self.blocks_mut() {
            for b in blocks.iter_mut() {
                if let Block::Tool {
                    id,
                    status,
                    extra: ex,
                    output: out,
                    ..
                } = b
                {
                    if id == tid {
                        *status = if ok {
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
        // Track touched files for the sidebar (successful edits only).
        if ok {
            if let Some(blocks) = self.blocks_mut() {
                let edits: Vec<(String, String)> = blocks
                    .iter()
                    .filter_map(|b| match b {
                        Block::Tool { id, name, arg, .. }
                            if id == tid && name.eq_ignore_ascii_case("edit") =>
                        {
                            serde_json::from_str::<serde_json::Value>(arg)
                                .ok()
                                .and_then(|v| {
                                    v["path"].as_str().map(|p| (p.to_string(), arg.clone()))
                                })
                        }
                        _ => None,
                    })
                    .collect();
                for (path, arg) in edits {
                    let (added, removed) = edit_delta(&arg);
                    match self.files.iter_mut().find(|f| f.path == path) {
                        Some(f) => {
                            f.added += added;
                            f.removed += removed;
                        }
                        None => self.files.push(FileChange {
                            path,
                            added,
                            removed,
                        }),
                    }
                }
            }
        }
    }

    /// A user message that arrived mid-run: record it, then open a fresh
    /// assistant block below it so the rest of the stream lands under it.
    pub fn inject_user(&mut self, text: String) {
        let (model, mode) = self
            .messages
            .iter()
            .rev()
            .find_map(|m| match m {
                Msg::Assistant { model, mode, .. } => Some((model.clone(), *mode)),
                _ => None,
            })
            .unwrap_or_else(|| (String::new(), self.mode));
        self.messages.push(Msg::User { text });
        self.messages.push(Msg::Assistant {
            blocks: Vec::new(),
            model,
            mode,
            secs: None,
        });
    }

    pub fn finish_turn(&mut self, secs: f32) {
        if self.run.take().is_some() {
            if let Some(Msg::Assistant { secs: s, .. }) = self.messages.last_mut() {
                *s = Some(secs);
            }
            self.turns += 1;
        }
    }

    /// Apply one streamed event to the owned transcript — the same events the
    /// clients receive, so server state and screens never drift apart.
    /// Returns true when something changed.
    pub fn apply(&mut self, ev: &Event) -> bool {
        match ev {
            Event::Text { delta, .. } => {
                self.append_text(delta);
                true
            }
            Event::ToolStart { id, name, arg, .. } => {
                self.tool_start(id.clone(), name.clone(), arg.clone());
                true
            }
            Event::ToolEnd {
                id,
                ok,
                extra,
                output,
                ..
            } => {
                self.tool_end(id, *ok, extra.clone(), output.clone());
                true
            }
            Event::Usage { input, output, .. } => {
                // The engine emits exactly one final usage event per provider
                // request (every tool round is its own billed request), so
                // summing gives the real cumulative session spend. The old
                // `.max()` kept only the largest single request.
                self.tokens_in += input;
                self.tokens_out += output;
                true
            }
            Event::ApprovalRequest { effect, .. } => {
                self.messages.push(Msg::Notice {
                    text: format!(
                        "APPROVAL REQUIRED — exact pending effect:\n{effect}\nPress y to allow once, n to deny. No action runs until allowed."
                    ),
                });
                true
            }
            Event::UserInjected { text, .. } => {
                self.inject_user(text.clone());
                true
            }
            Event::Notice { text, .. } => {
                // `interrupt` both applies and broadcasts its notice, and the
                // broadcast is applied again by the hub tap: the transcript
                // (and the saved session) showed "Interrupted" twice.
                if text == "Interrupted"
                    && matches!(self.messages.last(), Some(Msg::Notice { text: t }) if t == "Interrupted")
                {
                    return false;
                }
                self.messages.push(Msg::Notice { text: text.clone() });
                true
            }
            Event::Error { message, .. } => {
                self.messages.push(Msg::Error {
                    text: message.clone(),
                });
                true
            }
            Event::Done { secs, .. } => {
                self.finish_turn(*secs);
                true
            }
            Event::Snapshot { .. }
            | Event::Sessions { .. }
            | Event::Frame { .. }
            | Event::Device { .. } => false,
        }
    }

    /// Plain-text conversation history for the model.
    ///
    /// In addition to the user/assistant text blocks, this surfaces a short
    /// summary of every finished tool call so the model retains tool context
    /// across turns (which tool ran, what it returned) without flooding the
    /// prompt with full tool output — the first 200 chars of each result,
    /// truncated char-safely via `char_indices().nth(200)`.
    ///
    /// The tool summaries are FOLDED INTO the assistant turn that made the
    /// calls (never emitted as a synthetic `tool` role): the Anthropic
    /// Messages API only accepts `user`/`assistant` roles, and strict
    /// OpenAI-compatible gateways reject `role:"tool"` entries that don't
    /// carry a matching `tool_call_id` from the same turn. A plain-text
    /// history with only user/assistant roles is valid everywhere.
    pub fn history(&self) -> Vec<(&'static str, String)> {
        const TOOL_SUMMARY_CHARS: usize = 200;
        let mut out = Vec::new();
        for m in &self.messages {
            match m {
                Msg::User { text } => out.push(("user", text.clone())),
                Msg::Assistant { blocks, .. } => {
                    let t: String = blocks
                        .iter()
                        .filter_map(|b| {
                            if let Block::Text { text: t } = b {
                                Some(t.as_str())
                            } else {
                                None
                            }
                        })
                        .collect::<Vec<_>>()
                        .join("\n");
                    // Emit a compact summary of each finished tool block so
                    // the model sees what it ran and what came back.
                    // Truncate char-safely (avoid splitting a multi-byte
                    // UTF-8 sequence) by taking up to the 200th char boundary.
                    let mut tool_lines: Vec<String> = Vec::new();
                    for b in blocks {
                        if let Block::Tool {
                            name, status, extra, ..
                        } = b
                        {
                            let output = extra.clone().unwrap_or_default();
                            let summary: String = output
                                .char_indices()
                                .take(TOOL_SUMMARY_CHARS)
                                .map(|(_, c)| c)
                                .collect();
                            let tag = match status {
                                ToolStatus::Running => "running",
                                ToolStatus::Ok => "ok",
                                ToolStatus::Failed => "failed",
                            };
                            let line = if summary.is_empty() {
                                format!("[tool:{name} {tag}]")
                            } else {
                                format!("[tool:{name} {tag}] {summary}")
                            };
                            tool_lines.push(line);
                        }
                    }
                    let text = t.trim().is_empty();
                    match (text, tool_lines.is_empty()) {
                        // No text and no tool info — nothing to add.
                        (true, true) => {}
                        // Text only — as before.
                        (false, true) => out.push(("assistant", t)),
                        // Tool info only — the assistant turn is the tool
                        // summary (still role "assistant").
                        (true, false) => {
                            out.push(("assistant", tool_lines.join("\n")));
                        }
                        // Both — append the summaries after the text.
                        (false, false) => {
                            out.push(("assistant", format!("{t}\n{}", tool_lines.join("\n"))));
                        }
                    }
                }
                _ => {}
            }
        }
        out
    }
}

/// Cheap +/- delta for the sidebar from an Edit tool's argument (old/new
/// line counts) — best effort, never load-bearing.
fn edit_delta(arg_json: &str) -> (u32, u32) {
    let (added, removed) = serde_json::from_str::<serde_json::Value>(arg_json).ok().map_or((0, 0), |v| {
        let lines = |s: &str| s.lines().count() as u32;
        let old = v["old_string"].as_str().unwrap_or("");
        let new = v["new_string"].as_str().unwrap_or("");
        if old.is_empty() {
            (lines(new), 0)
        } else {
            (lines(new).min(lines(old) + 10), lines(old))
        }
    });
    (added, removed)
}

/// Context handed to a spawned agent run.
pub struct RunContext {
    pub session_id: usize,
    pub history: Vec<(&'static str, String)>,
    pub mode: Mode,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn text_after_a_notice_is_not_dropped_from_the_transcript() {
        let mut s = Session::new(1);
        s.begin_assistant("model-x");
        s.apply(&Event::Text { session_id: 1, delta: "before".into() });
        // Durability notices are emitted mid-turn on compaction / retries.
        s.apply(&Event::Notice { session_id: Some(1), text: "context compacted".into() });
        s.apply(&Event::Text { session_id: 1, delta: "after".into() });

        let all = format!("{:?}", s.messages);
        assert!(all.contains("before"), "{all}");
        assert!(all.contains("after"), "{all}");
        assert!(all.contains("context compacted"), "{all}");
        // The post-notice text must live in a reopened assistant block.
        let assistants = s
            .messages
            .iter()
            .filter(|m| matches!(m, Msg::Assistant { .. }))
            .count();
        assert_eq!(assistants, 2, "{all}");
    }
}

#[cfg(test)]
mod steering_tests {
    use super::*;

    #[test]
    fn injected_user_message_is_recorded_and_opens_a_fresh_block() {
        let mut s = Session::new(1);
        s.push_user("task".into());
        s.begin_assistant("m");
        s.apply(&Event::Text {
            session_id: 1,
            delta: "step one".into(),
        });
        s.apply(&Event::UserInjected {
            session_id: 1,
            text: "also do this".into(),
        });
        s.apply(&Event::Text {
            session_id: 1,
            delta: "step two".into(),
        });
        assert_eq!(s.messages.len(), 4);
        assert!(matches!(&s.messages[2], Msg::User { text } if text == "also do this"));
        // the model sees the steering message in the history of the next turn
        let h = s.history();
        assert!(h.iter().any(|(r, t)| *r == "user" && t == "also do this"));
    }

    #[test]
    fn interrupted_is_recorded_once_even_when_applied_twice() {
        let mut s = Session::new(1);
        let ev = Event::Notice {
            session_id: Some(1),
            text: "Interrupted".into(),
        };
        // `interrupt` applies it directly and the hub tap applies the broadcast
        assert!(s.apply(&ev));
        assert!(!s.apply(&ev));
        assert_eq!(s.messages.len(), 1);
    }
}
