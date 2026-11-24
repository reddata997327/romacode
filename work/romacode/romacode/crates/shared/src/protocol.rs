//! Wire protocol — request/response types shared by the server (brain) and
//! the client TUI (screen).
//!
//! The client owns nothing but pixels: every session, every message, every
//! tool result lives server-side and reaches the screen through
//! [`Snapshot`](crate::events::Event::Snapshot) pushes plus the fine-grained
//! events in [`crate::events`]. Requests are JSON over WebSocket:
//!
//! ```json
//! {"req": 1, "body": {"type": "message", "id": 1, "text": "hi", "mode": "build"}}
//! ```
//! ```json
//! {"req": 1, "reply": {"ok": true, "data": {...}}, "event": null}
//! ```

use serde::{Deserialize, Serialize};

// ---------------------------------------------------------------------------
// Session data model — serialisable, server-owned, mirrored by the client.
// ---------------------------------------------------------------------------

/// Build/Plan switch, mirrored in the UI footer.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Mode {
    Build,
    Plan,
}

impl Mode {
    pub fn label(self) -> &'static str {
        match self {
            Mode::Build => "Build",
            Mode::Plan => "Plan",
        }
    }
    pub fn toggle(self) -> Self {
        match self {
            Mode::Build => Mode::Plan,
            Mode::Plan => Mode::Build,
        }
    }
}

impl Default for Mode {
    fn default() -> Self {
        Mode::Build
    }
}

/// Live state of one tool call block.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ToolStatus {
    Running,
    Ok,
    Failed,
}

/// One block inside an assistant message.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "lowercase")]
pub enum Block {
    Text { text: String },
    Tool {
        id: String,
        name: String,
        arg: String,
        status: ToolStatus,
        extra: Option<String>,
        /// FULL tool output (capped), for the TUI's click-to-inspect details
        /// view. `extra` stays the one-line transcript summary.
        #[serde(default)]
        output: Option<String>,
    },
}

/// One transcript entry.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "lowercase")]
pub enum Msg {
    User {
        text: String,
    },
    Assistant {
        blocks: Vec<Block>,
        model: String,
        mode: Mode,
        secs: Option<f32>,
    },
    Notice {
        text: String,
    },
    Error {
        text: String,
    },
}

/// Todo list entry shown in the side panel.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Todo {
    pub text: String,
    pub state: TodoState,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum TodoState {
    Done,
    Active,
    Open,
}

/// Fake "MCP server" summary row in the sidebar (display only).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct McpServer {
    pub name: String,
    pub tools: u32,
    pub on: bool,
}

/// One file the agent touched this session.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FileChange {
    pub path: String,
    pub added: u32,
    pub removed: u32,
}

/// One approval parked on the brain (waiting for the user's y/n). Sent in
/// `SessionSnapshot` so a (re)connecting screen re-raises the permission
/// popup instead of watching a turn that silently waits on nobody.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PendingApprovalInfo {
    pub request_id: String,
    pub name: String,
    pub effect: String,
}

/// Full state of one session — what the server sends so a (re)connecting
/// client can render immediately without replaying history.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SessionSnapshot {
    pub id: usize,
    pub title: String,
    pub messages: Vec<Msg>,
    pub mode: Mode,
    pub running: bool,
    pub tokens_in: u64,
    pub tokens_out: u64,
    pub turns: u32,
    pub todos: Vec<Todo>,
    pub mcp: Vec<McpServer>,
    pub files: Vec<FileChange>,
    pub lsp: Vec<(String, String)>,
    /// Approvals still waiting on the brain for this session; restored as
    /// permission popups on (re)connect. `default` keeps older payloads
    /// without the field parsing.
    #[serde(default)]
    pub pending_approvals: Vec<PendingApprovalInfo>,
}

/// Compact row for the session list / welcome payload.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SessionSummary {
    pub id: usize,
    pub title: String,
    pub mode: Mode,
    pub running: bool,
}

/// Android device facts for the phone panel header — the serialisable mirror
/// of the bridge `Status`.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct DeviceStatus {
    /// "booting" | "online" | "error:<message>"
    pub phase: String,
    pub serial: String,
    pub model: String,
    pub port: u16,
    pub root: String,
    pub display: (u32, u32),
    pub battery: u8,
    pub package_count: usize,
}

/// One provider row for the settings panel and `/providers`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProviderInfo {
    pub id: String,
    pub name: String,
    pub base_url: String,
    pub kind: String,
    pub has_key: bool,
    pub models: Vec<String>,
    pub default_model: Option<String>,
}

/// What `hello` answers with: everything a fresh screen needs.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Welcome {
    pub server_version: String,
    pub protocol_version: u32,
    pub provider: String,
    pub model: String,
    pub mode_default: Mode,
    pub engine: String,
    pub chat_width_pct: u16,
    pub sidebar: bool,
    pub cwd: String,
    pub branch: Option<String>,
    pub providers: Vec<ProviderInfo>,
    pub sessions: Vec<SessionSummary>,
    pub device: Option<DeviceStatus>,
    /// Provider ids whose key is missing — the settings panel marks them.
    pub missing_keys: Vec<String>,
    /// Context window + prices, so the screen can draw usage bars and costs
    /// without owning the config file.
    pub context_window: u64,
    pub price_in_per_mtok: f64,
    pub price_out_per_mtok: f64,
    /// Agent settings surfaced in the settings panel (all server-owned).
    #[serde(default)]
    pub max_tool_rounds: u32,
    #[serde(default = "default_true")]
    pub webfetch_enabled: bool,
    /// The built-in rnmap scanner tool is offered (server-owned).
    #[serde(default = "default_true")]
    pub rnmap_enabled: bool,
    /// The built-in remix (rrx Ethereum toolchain) tool is offered
    /// (server-owned).
    #[serde(default = "default_true")]
    pub remix_enabled: bool,
    #[serde(default)]
    pub request_timeout_secs: u64,
    /// The OpenCode-style TUI settings (`/settings`), server-owned.
    #[serde(default)]
    pub tui: crate::tui_settings::TuiSettings,
}

fn default_true() -> bool {
    true
}

impl Welcome {
    pub const PROTOCOL_VERSION: u32 = 1;
}

// ---------------------------------------------------------------------------
// Client → server requests.
// ---------------------------------------------------------------------------

/// Everything the screen can ask the brain to do.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ClientRequest {
    /// Handshake; the reply carries the [`Welcome`] payload.
    Hello { version: String },
    /// Create a session (tab). Reply carries the id.
    NewSession,
    /// Close a session tab; a running turn is cancelled first.
    CloseSession { id: usize },
    /// Subscribe to + snapshot a session (switching tabs).
    SelectSession { id: usize },
    /// Send a user prompt; the agent loop starts server-side.
    Message { id: usize, text: String, mode: Mode },
    /// Answer a pending approval prompt.
    Approval {
        session_id: usize,
        request_id: String,
        approve: bool,
    },
    /// Interrupt the running turn (esc / ctrl+c).
    Cancel { id: usize },
    /// Mid-run steering (ctrl+s on a queued query): hand `text` to the
    /// RUNNING turn. The brain shows it in the transcript at once and the
    /// agent reads it before its next model round, so the user can redirect a
    /// task without interrupting it. When the session is idle this simply
    /// starts a normal turn.
    Steer { id: usize, text: String },
    /// Toggle the session mode (Build ⇄ Plan).
    ToggleMode { id: usize },
    /// Provider / model / key slash-commands run server-side; the reply is
    /// the notice text to show. `session_id` (optional for wire
    /// compatibility) tells the brain which tab sent the command — `/skill`
    /// needs it to activate a skill on the right session.
    Command {
        text: String,
        #[serde(default)]
        session_id: Option<usize>,
    },
    /// Live model-catalog lookup for the settings panel. `provider_id` (may
    /// be empty for old clients) tells the brain which config entry the
    /// fetched catalog belongs to, so the list is REMEMBERED in that
    /// provider's model options instead of being echoed once to the asking
    /// screen and lost.
    FetchModels {
        #[serde(default)]
        provider_id: String,
        base_url: String,
        kind: String,
        api_key: String,
    },
    /// Commit the model chosen in the settings panel.
    SaveModel {
        provider_id: String,
        provider_name: String,
        base_url: String,
        api_key: String,
        kind: String,
        model: String,
        models: Vec<String>,
    },
    /// Activate a skill (an instruction package from the skills library) on
    /// session `id`: the brain resolves the name server-side and starts a
    /// turn whose prompt carries the skill body. Reply is a Notice.
    Skill { id: usize, name: String },
    /// Persist UI prefs that live in the server config (sidebar, width…).
    SetUiPref { key: String, value: String },
    /// Android device: stream toggle, taps, swipes, keys, typed text.
    Android { action: AndroidAction },
    /// Liveness probe.
    Ping,
}

/// Device input events forwarded from the phone panel / desktop window.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum AndroidAction {
    /// Start/stop the frame stream for this client.
    Stream { on: bool },
    Tap { x: u32, y: u32 },
    Swipe {
        x1: u32,
        y1: u32,
        x2: u32,
        y2: u32,
    },
    Text { text: String },
    /// "home" | "back" | "enter" | "del" | "power" | "volume_up" | "volume_down"
    Key { key: String },
}

// ---------------------------------------------------------------------------
// Server → client replies (to a specific request) and the envelope.
// ---------------------------------------------------------------------------

/// Reply payload variants. `None` (= plain ack) for mutations.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum Reply {
    Welcome(Welcome),
    NewSession { id: usize },
    Models { models: Vec<String> },
    Notice { text: String },
    Pong,
}

/// A successful reply.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ReplyOk {
    pub data: Reply,
}

/// A failed reply.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ReplyErr {
    pub error: String,
}

/// Envelope for one client message.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ClientEnvelope {
    pub req: u64,
    pub body: ClientRequest,
}

/// Envelope for one server message: either a reply to `req`, a pushed
/// [`crate::events::Event`], or (on error) just `error`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ServerEnvelope {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub req: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reply: Option<serde_json::Value>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub event: Option<crate::events::Event>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

impl ServerEnvelope {
    pub fn reply(req: u64, data: Reply) -> Self {
        Self {
            req: Some(req),
            reply: Some(serde_json::json!({ "ok": true, "data": data })),
            event: None,
            error: None,
        }
    }
    pub fn reply_err(req: u64, error: impl Into<String>) -> Self {
        Self {
            req: Some(req),
            reply: None,
            event: None,
            error: Some(error.into()),
        }
    }
    pub fn push(event: crate::events::Event) -> Self {
        Self {
            req: None,
            reply: None,
            event: Some(event),
            error: None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn roundtrips_request() {
        let e = ClientEnvelope {
            req: 7,
            body: ClientRequest::Message {
                id: 3,
                text: "hello".into(),
                mode: Mode::Plan,
            },
        };
        let v = serde_json::to_string(&e).unwrap();
        let back: ClientEnvelope = serde_json::from_str(&v).unwrap();
        assert_eq!(back.req, 7);
        match back.body {
            ClientRequest::Message { id, text, mode } => {
                assert_eq!(id, 3);
                assert_eq!(text, "hello");
                assert_eq!(mode, Mode::Plan);
            }
            other => panic!("wrong variant: {other:?}"),
        }
    }

    #[test]
    fn roundtrips_snapshot() {
        let s = SessionSnapshot {
            id: 1,
            title: "t".into(),
            messages: vec![
                Msg::User {
                    text: "hi".into(),
                },
                Msg::Assistant {
                    blocks: vec![
                        Block::Text {
                            text: "doing it".into(),
                        },
                        Block::Tool {
                            id: "1".into(),
                            name: "Read".into(),
                            arg: "{}".into(),
                            status: ToolStatus::Ok,
                            output: None,
                            extra: Some("ok".into()),
                        },
                    ],
                    model: "m".into(),
                    mode: Mode::Build,
                    secs: Some(1.5),
                },
            ],
            mode: Mode::Build,
            running: false,
            tokens_in: 1,
            tokens_out: 2,
            turns: 1,
            todos: vec![Todo {
                text: "x".into(),
                state: TodoState::Active,
            }],
            mcp: vec![],
            files: vec![],
            lsp: vec![],
            pending_approvals: vec![PendingApprovalInfo {
                request_id: "apr-1".into(),
                name: "Bash".into(),
                effect: "Run host shell command in project directory:\ncargo test".into(),
            }],
        };
        let v = serde_json::to_string(&s).unwrap();
        let back: SessionSnapshot = serde_json::from_str(&v).unwrap();
        assert_eq!(back.messages.len(), 2);
        assert_eq!(back.pending_approvals.len(), 1);

        // older payloads without the field still parse (serde default)
        let legacy = r#"{"id":1,"title":"t","messages":[],"mode":"build","running":false,"tokens_in":0,"tokens_out":0,"turns":0,"todos":[],"mcp":[],"files":[],"lsp":[]}"#;
        let back: SessionSnapshot = serde_json::from_str(legacy).unwrap();
        assert!(back.pending_approvals.is_empty());
    }

    #[test]
    fn envelope_kinds() {
        let env = ServerEnvelope::reply(3, Reply::Pong);
        assert_eq!(env.req, Some(3));
        assert!(env.reply.is_some());
        let env = ServerEnvelope::push(crate::events::Event::Done {
            session_id: 1,
            secs: 0.5,
        });
        assert!(env.event.is_some());
        let env = ServerEnvelope::reply_err(9, "nope");
        assert_eq!(env.error.as_deref(), Some("nope"));
    }
}
