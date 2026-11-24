//! Application state, actions and event handling. The screen is a mirror:
//! sessions and the device live in the brain (`romacode-server`); every
//! mutation is a request on `crate::net::Net` and every change arrives back
//! as a wire `Event` applied to the local [`UiSession`] mirrors.
use crate::device::{Frame, FrameHub, PhoneKey};
use crate::net::{Net, UiEvent};
use crate::session::{DeviceView, UiSession};
use crossterm::event::{
    Event, KeyCode, KeyEvent, KeyEventKind, KeyModifiers, MouseButton, MouseEvent, MouseEventKind,
};
use ratatui::layout::Rect;
use romacode_shared::events::Event as WireEvent;
use romacode_shared::protocol::{
    ClientRequest, Mode, Msg, ProviderInfo, Reply, Welcome, AndroidAction,
};
use std::sync::mpsc::Receiver;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Screen {
    Home,
    Chat,
    Terminal,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Device {
    None,
    Android,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Action {
    SelectTab(usize),
    CloseTab(usize),
    NewTab,
    TogglePanel,
    ToggleAndroid,
    CloseDevice,
    PhoneFullscreen,
    ToggleMode,
    /// Queued-query box: pick a row / send it now / pull it back to edit / drop it.
    QueueSelect(usize),
    QueueSend(usize),
    QueueEdit(usize),
    QueueDelete(usize),
    PaletteRun(usize),
    Refresh,
    /// Open the click-to-inspect details view for a tool call
    /// (message index, block index) — fired by clicking a `▸` tool row.
    OpenTool(usize, usize),
    /// No-op click target: panel backgrounds use it to swallow stray clicks.
    /// The command palette used to swallow with `Refresh` — which is NOT a
    /// no-op (it fires an Android Home keypress) — so clicking any palette
    /// row silently pressed Home on the phone and left the palette open.
    Swallow,
}

pub struct Hit {
    pub rect: Rect,
    pub action: Action,
    /// Tooltip: (label, shortcut).
    pub tip: Option<(&'static str, &'static str)>,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Cmd {
    NewSession,
    CloseSession,
    NextSession,
    PrevSession,
    TogglePanel,
    ToggleAndroid,
    ToggleTerminal,
    WidenChat,
    NarrowChat,
    ToggleMode,
    ClearSession,
    Quit,
}


pub const COMMANDS: &[(Cmd, &str, &str)] = &[
    (Cmd::NewSession, "New session", "ctrl+n"),
    (Cmd::CloseSession, "Close session", "ctrl+w"),
    (Cmd::NextSession, "Next session", "alt+→"),
    (Cmd::PrevSession, "Previous session", "alt+←"),
    (Cmd::TogglePanel, "Toggle side panel", "ctrl+b"),
    (Cmd::ToggleAndroid, "Toggle Android window", "alt+a"),
    (Cmd::ToggleTerminal, "Open native terminal", "alt+t"),
    (Cmd::WidenChat, "Widen chat", "alt+]"),
    (Cmd::NarrowChat, "Narrow chat", "alt+["),
    (Cmd::ToggleMode, "Switch Build / Plan", "tab"),
    (Cmd::ClearSession, "Clear session", "/clear"),
    (Cmd::Quit, "Quit", "ctrl+q"),
];

#[derive(Default)]
pub struct Palette {
    pub query: String,
    pub sel: usize,
}

impl Palette {
    pub fn items(&self) -> Vec<usize> {
        let q = self.query.to_lowercase();
        COMMANDS
            .iter()
            .enumerate()
            .filter(|(_, (_, label, _))| q.is_empty() || label.to_lowercase().contains(&q))
            .map(|(i, _)| i)
            .collect()
    }
}

/// An approval prompt from the brain: answered by a wire request, not a
/// channel. `id` is the server's `request_id`; `name`/`effect` feed the
/// allow popup (tool + the exact pending effect).
pub struct PendingApproval {
    pub id: String,
    pub session_id: usize,
    pub name: String,
    pub effect: String,
}

/// Results of requests answered on background threads; applied in `poll`.
enum Bg {
    /// Reply to a `/provider...` slash command; carries the command text as
    /// sent, so the input is only cleared when it still holds just that
    /// command (text typed during the round trip survives).
    Command(Result<Reply, String>, String),
    /// The turn-start request answered (err → roll back the optimistic echo).
    MessageSent { id: usize, err: Option<String> },
    /// A ctrl+s steering request answered; on failure the text goes back to
    /// the queue (the agent is still running, so `started` must stay set).
    SteerSent { id: usize, text: String, err: Option<String> },
    /// `hello` refresh (config mirror re-sync).
    Hello(Result<Reply, String>),
    /// SaveModel reply from the settings screen.
    Saved(Result<Reply, String>),
}

/// One mouse-clickable button of the permission popup, recorded by the draw
/// pass each frame (same pattern as `settings_hits`).
#[derive(Clone, Copy, Debug)]
pub struct PermHit {
    pub x: u16,
    pub y: u16,
    pub w: u16,
    pub h: u16,
    /// true = the Allow button, false = the Deny button.
    pub allow: bool,
}

impl PermHit {
    pub fn contains(&self, x: u16, y: u16) -> bool {
        x >= self.x && x < self.x.saturating_add(self.w) && y >= self.y && y < self.y.saturating_add(self.h)
    }
}

/// Everything the screen knows about the brain's config, refreshed from
/// `Welcome` replies. The config file itself never leaves the server.
#[derive(Clone, Debug, Default)]
pub struct ClientState {
    pub provider: String,
    pub model: String,
    pub engine: String,
    pub chat_width_pct: u16,
    pub sidebar: bool,
    pub context_window: u64,
    pub price_in_per_mtok: f64,
    pub price_out_per_mtok: f64,
    pub default_mode: Mode,
    /// Mirror of brain-owned fields the Ctrl+P Settings panel used to surface.
    /// Kept for protocol parity (Welcome still carries them) and future use.
    #[allow(dead_code)]
    pub max_tool_rounds: u32,
    #[allow(dead_code)]
    pub webfetch_enabled: bool,
    pub rnmap_enabled: bool,
    /// The built-in remix (rrx Ethereum toolchain) tool is offered — mirrored
    /// for the settings panel and the /remix command UX.
    pub remix_enabled: bool,
    #[allow(dead_code)]
    pub request_timeout_secs: u64,
    /// OpenCode-style TUI settings tree, mirrored from the brain. Still used
    /// by the TUI's own theme/scroll/animation behaviours (no longer edited
    /// from the Ctrl+P panel after the Settings section was removed).
    #[allow(dead_code)]
    pub tui: romacode_shared::tui_settings::TuiSettings,
    pub providers: Vec<ProviderInfo>,
    #[allow(dead_code)] // surfaced in the settings panel + future toasts
    pub missing_keys: Vec<String>,
    #[allow(dead_code)]
    pub server_version: String,
    pub cwd: String,
    #[allow(dead_code)]
    pub branch: Option<String>,
}

impl ClientState {
    pub fn from_welcome(w: &Welcome) -> Self {
        Self {
            provider: w.provider.clone(),
            model: w.model.clone(),
            engine: w.engine.clone(),
            chat_width_pct: w.chat_width_pct,
            sidebar: w.sidebar,
            context_window: w.context_window,
            price_in_per_mtok: w.price_in_per_mtok,
            price_out_per_mtok: w.price_out_per_mtok,
            default_mode: w.mode_default,
            max_tool_rounds: w.max_tool_rounds,
            webfetch_enabled: w.webfetch_enabled,
            rnmap_enabled: w.rnmap_enabled,
            remix_enabled: w.remix_enabled,
            request_timeout_secs: w.request_timeout_secs,
            tui: w.tui.clone(),
            providers: w.providers.clone(),
            missing_keys: w.missing_keys.clone(),
            server_version: w.server_version.clone(),
            cwd: w.cwd.clone(),
            branch: w.branch.clone(),
        }
    }
    pub fn effective_model(&self) -> String {
        self.model.clone()
    }
}

pub struct App {
    /// Server-owned config, mirrored for rendering.
    pub cfg: ClientState,
    /// The live connection to the brain.
    pub net: Arc<Net>,
    /// Where the brain lives — the reconnect loop dials this.
    bind: String,
    /// Next reconnect attempt instant (Some = connection lost, retrying).
    reconnect_at: Option<Instant>,
    /// Results of backgrounded requests, drained by `poll`. The UI thread
    /// never blocks on a request any more (the old blocking `Net::request`
    /// froze the whole TUI for up to 15 s per call while the brain was slow).
    bg: Arc<Mutex<Vec<Bg>>>,
    pub screen: Screen,
    /// One persistent in-process Roxterm VT session, created on first open.
    pub terminal: Option<crate::terminal::RoxtermTerminal>,
    terminal_return: Screen,
    /// Local mirrors of the server sessions.
    pub sessions: Vec<UiSession>,
    pub active: usize,
    pub device: Device,
    pub panel: bool,
    /// Device mirror: status + latest frame streamed from the brain.
    pub android: DeviceView,
    /// Frame hub shared with the desktop window.
    pub frame_hub: FrameHub,
    /// Whether we asked the server to stream frames (synced in `poll`).
    streaming_wanted: bool,
    /// The scrcpy-style desktop window, while it is open.
    pub phone_window: Option<crate::display::PhoneWindow>,
    /// Fullscreen phone view open?
    pub android_full: bool,
    /// Where the live phone screen was last painted (for mouse mapping).
    pub phone_rect: Option<Rect>,
    /// Mouse press position inside the phone screen (drag → swipe).
    pub phone_press: Option<(u16, u16)>,
    pub hits: Vec<Hit>,
    pub hover: Option<Action>,
    pub mouse: (u16, u16),
    pub start: Instant,
    pub quit: bool,
    pub palette: Option<Palette>,
    pub settings: Option<crate::settings::Settings>,
    pub settings_fetch: Option<Receiver<crate::settings::FetchResult>>,
    /// The new hierarchical Settings tree (Ctrl+P). When `Some`, the overlay
    /// is the tree overview / group / leaf; when the user descends into a
    /// provider or tools flow, control is handed to `settings` (the legacy
    /// struct) and `tree` is cleared until next open.
    pub tree: Option<crate::tree::Tree>,
    /// The `/` slash menu in chat. When `Some`, a popup lists every tree leaf
    /// filtered by what the user typed after the slash.
    pub slash: Option<crate::tree::SlashMenu>,
    pub toast: Option<(String, Instant)>,
    pub dirty: bool,
    pub branch: Option<String>,
    pub cwd: String,
    pub transcript_rect: Rect,
    /// Click-to-inspect tool details: (message index, block index) of the
    /// open tool call, plus its scroll offset. None = closed.
    pub tool_detail: Option<(usize, usize)>,
    pub tool_scroll: usize,
    pub pending_approval: Vec<PendingApproval>,
    /// Mouse hit-test buttons of the permission popup; rebuilt every draw.
    pub perm_hits: Vec<PermHit>,
    last_ctrl_c: Option<Instant>,
    /// The integrated RomaCode tools builder (38 builtin tools across 8
    /// categories), surfaced in the Ctrl+P "Tools" settings section.
    pub tools: crate::tools::Tools,
    /// Mouse hit-test rows recorded by `draw_settings` each frame; read by
    /// `on_mouse` to turn clicks/hover into selection + activation. Cleared
    /// and repopulated on every draw so the geometry never goes stale.
    pub settings_hits: Vec<crate::settings::HitRow>,
    /// Mouse hit-test rows for the tree overlay + slash menu (same pattern).
    pub tree_hits: Vec<crate::tree::TreeHit>,
}

impl App {
    /// Build the screen from the handshake payload. The brain already
    /// created session 1; mirror it.
    pub fn new(net: Arc<Net>, welcome: &Welcome, bind: &str) -> Self {
        let cfg = ClientState::from_welcome(welcome);
        let panel = cfg.sidebar;
        let mut app = Self {
            cfg,
            net,
            bind: bind.to_string(),
            reconnect_at: None,
            bg: Arc::new(Mutex::new(Vec::new())),
            screen: Screen::Home,
            terminal: None,
            terminal_return: Screen::Home,
            sessions: vec![UiSession::new(1)],
            active: 0,
            device: Device::None,
            panel,
            android: DeviceView {
                status: welcome.device.clone(),
                frame: None,
                streaming: false,
            },
            frame_hub: FrameHub::new(),
            streaming_wanted: false,
            phone_window: None,
            android_full: false,
            phone_rect: None,
            phone_press: None,
            hits: Vec::new(),
            hover: None,
            mouse: (0, 0),
            start: Instant::now(),
            quit: false,
            palette: None,
            settings: None,
            settings_fetch: None,
            tree: None,
            slash: None,
            toast: None,
            dirty: true,
            branch: welcome.branch.clone(),
            cwd: welcome.cwd.clone(),
            transcript_rect: Rect::default(),
            tool_detail: None,
            tool_scroll: 0,
            pending_approval: Vec::new(),
            perm_hits: Vec::new(),
            last_ctrl_c: None,
            tools: crate::tools::Tools::new(),
            settings_hits: Vec::new(),
            tree_hits: Vec::new(),
        };
        app.sessions[0].mode = app.cfg.default_mode;
        app.sessions[0].mcp = Vec::new();
        app
    }

    pub fn t(&self) -> f32 {
        self.start.elapsed().as_secs_f32()
    }
    pub fn session(&self) -> &UiSession {
        &self.sessions[self.active]
    }
    pub fn session_mut(&mut self) -> &mut UiSession {
        &mut self.sessions[self.active]
    }

    fn session_by_id_mut(&mut self, id: usize) -> Option<&mut UiSession> {
        self.sessions.iter_mut().find(|s| s.id == id)
    }

    /// Refresh the config mirror from a fresh `Welcome` payload.
    pub fn apply_welcome(&mut self, w: &Welcome) {
        self.cfg = ClientState::from_welcome(w);
        self.branch = w.branch.clone();
        self.cwd = w.cwd.clone();
        if let Some(d) = &w.device {
            self.android.status = Some(d.clone());
        }
        self.dirty = true;
    }
    pub fn animating(&self) -> bool {
        self.screen == Screen::Home
            || self.screen == Screen::Terminal
            || self.android_full
            || self.device == Device::Android
            || self.sessions.iter().any(|s| s.is_running())
            || self.toast.is_some()
            // while a permission popup is up the brain is paused on the
            // user's y/n — keep ticking so its toast/hint stays alive
            || !self.pending_approval.is_empty()
            // While a model fetch is in flight (after the user enters an API
            // key and presses Enter), keep the draw loop running every tick so
            // the loading spinner renders and the fetch reply is drained as
            // soon as it lands — otherwise the UI looks frozen for up to 45s.
            || self.settings.as_ref().map_or(false, |st| st.loading)
            || self.settings_fetch.is_some()
    }

    /// Apply one wire event to the mirrors. Returns true when redraw needed.
    pub fn apply_event(&mut self, ev: &WireEvent) -> bool {
        match ev {
            WireEvent::Frame { gen, w, h, png_base64 } => {
                if let Ok(bytes) = png_decode(png_base64) {
                    self.frame_hub.set(Frame {
                        gen: *gen,
                        w: *w,
                        h: *h,
                        rgba: Arc::new(bytes),
                    });
                    self.android.frame = self.frame_hub.take_frame(0);
                    return self.android_full || self.device == Device::Android;
                }
                false
            }
            WireEvent::Device { status } => {
                self.android.status = Some(status.clone());
                true
            }
            WireEvent::Sessions { sessions: list } => {
                // sync tab set with the server
                for summary in list {
                    if !self.sessions.iter().any(|s| s.id == summary.id) {
                        let mut s = UiSession::new(summary.id);
                        s.title = summary.title.clone();
                        s.mode = summary.mode;
                        self.sessions.push(s);
                    }
                }
                self.sessions.retain(|s| list.iter().any(|l| l.id == s.id));
                if self.sessions.is_empty() {
                    self.sessions.push(UiSession::new(1));
                }
                if self.active >= self.sessions.len() {
                    self.active = self.sessions.len() - 1;
                }
                self.dirty = true;
                true
            }
            WireEvent::ApprovalRequest { session_id, request_id, name, effect } => {
                // Dedup on the REQUEST: a duplicate delivery used to run the
                // session mirror again — pushing the "APPROVAL REQUIRED"
                // transcript notice twice (the popup itself was deduped).
                if !self.pending_approval.iter().any(|p| &p.id == request_id) {
                    // Transcript notice via the session mirror…
                    for s in self.sessions.iter_mut() {
                        if s.id == *session_id {
                            s.apply(ev);
                        }
                    }
                    // …and queue the permission popup (deduped by request id).
                    self.pending_approval.push(PendingApproval {
                        id: request_id.clone(),
                        session_id: *session_id,
                        name: name.clone(),
                        effect: effect.clone(),
                    });
                }
                self.dirty = true;
                true
            }
            WireEvent::Snapshot { session } => {
                // the canonical mirror refresh (select / reconnect / create)
                let sid = session.id;
                let mut changed = false;
                for s in self.sessions.iter_mut() {
                    if s.id == sid {
                        changed |= s.apply(ev);
                    }
                }
                // Re-raise permission popups for approvals still parked on the
                // brain: a (re)connecting screen must not silently watch a
                // turn that is waiting on the user's y/n. Replace this
                // session's queued entries with the snapshot's list (entries
                // for other sessions are kept).
                self.pending_approval.retain(|p| p.session_id != sid);
                for a in &session.pending_approvals {
                    self.pending_approval.push(PendingApproval {
                        id: a.request_id.clone(),
                        session_id: sid,
                        name: a.name.clone(),
                        effect: a.effect.clone(),
                    });
                }
                self.dirty = true;
                changed
            }
            _ => {
                let sid = ev.session_id();
                let mut changed = false;
                for s in self.sessions.iter_mut() {
                    if Some(s.id) == sid {
                        changed |= s.apply(ev);
                    }
                }
                changed
            }
        }
    }
    pub fn toast(&mut self, msg: impl Into<String>) {
        self.toast = Some((msg.into(), Instant::now()));
        self.dirty = true;
    }

    // ── sessions ──
    pub fn new_session(&mut self) {
        // optimistic tab; the server's Sessions+Snapshot events canonicalise
        let id = self.sessions.iter().map(|s| s.id).max().unwrap_or(1) + 1;
        let mut session = UiSession::new(id);
        session.mode = self.cfg.default_mode;
        self.sessions.push(session);
        self.active = self.sessions.len() - 1;
        self.screen = Screen::Chat;
        self.dirty = true;
        self.net.fire(ClientRequest::NewSession);
    }
    pub fn close_session(&mut self, i: usize) {
        if i >= self.sessions.len() {
            return;
        }
        let session_id = self.sessions[i].id;
        self.net.fire(ClientRequest::CloseSession { id: session_id });
        for n in (0..self.pending_approval.len()).rev() {
            if self.pending_approval[n].session_id == session_id {
                let pending = self.pending_approval.remove(n);
                // deny so the brain does not wait on it
                self.net.fire(ClientRequest::Approval {
                    session_id,
                    request_id: pending.id,
                    approve: false,
                });
            }
        }
        if self.sessions.len() == 1 {
            // Last tab: reset to a fresh home session with a NEW optimistic id.
            // Reusing the closed session's id made the first send target a
            // session the brain had already deleted, and the next Sessions
            // broadcast yanked the phantom tab.
            let fresh = self.sessions.iter().map(|s| s.id).max().unwrap_or(1) + 1;
            self.sessions[0] = UiSession::new(fresh);
            self.sessions[0].mode = self.cfg.default_mode;
            self.screen = Screen::Home;
            self.net.fire(ClientRequest::NewSession);
        } else {
            self.sessions.remove(i);
            if self.active >= self.sessions.len() {
                self.active = self.sessions.len() - 1;
            } else if self.active > i {
                self.active -= 1;
            }
        }
        self.dirty = true;
    }
    pub fn select(&mut self, i: usize) {
        if i < self.sessions.len() {
            self.active = i;
            self.screen = if self.session().messages.is_empty() && self.sessions.len() == 1 {
                Screen::Home
            } else {
                Screen::Chat
            };
            // resync this tab from the brain (also while it streams)
            let id = self.session().id;
            self.net.fire(ClientRequest::SelectSession { id });
            self.dirty = true;
        }
    }

    // ── sending / agent ──
    pub fn send(&mut self) {
        if self.session().is_running() {
            // The agent is working: park the query in the queue box (ctrl+s
            // sends it to the agent without interrupting the task).
            let typed = self.session().input.text();
            let t = typed.trim();
            if t.is_empty() {
                self.toast("Agent is working — enter queues a query, ctrl+s sends it now");
            } else if t.starts_with('/') {
                self.toast("Commands run when the agent is idle — esc stops the task");
            } else if self.session().queue.len() >= crate::session::QUEUE_CAP {
                self.toast("Queue is full — send or delete a queued query first");
            } else {
                let text = self.session_mut().input.submit();
                let s = self.session_mut();
                s.queue.push(text.trim().to_string());
                s.queue_sel = s.queue.len() - 1;
                s.touch();
                self.toast("Queued — ctrl+s sends it to the agent now");
                self.dirty = true;
            }
            return;
        }
        let text = self.session().input.text();
        let trimmed = text.trim();
        if trimmed.is_empty() {
            return;
        }
        if let Some(cmd) = trimmed.strip_prefix('/') {
            // provider/model/key/skill commands run in the brain, which owns
            // the config file; the reply is the notice to show
            let provider_cmd = matches!(
                cmd.split_whitespace().next().unwrap_or(""),
                "providers" | "provider" | "models" | "model" | "baseurl" | "apikey"
                    | "customprovider" | "engine" | "mode" | "rounds" | "webfetch"
                    | "rnmap" | "remix" | "documents"
                    | "timeout" | "context" | "prefs" | "settings" | "skills" | "skill"
            );
            if provider_cmd {
                // Backgrounded: the UI thread must never block on the brain
                // (the old inline request froze the whole TUI for up to 15 s).
                // The typed command stays in the input until the reply — if
                // the request fails, nothing the user typed is lost.
                // `session_id` tags the sending tab: /skill activates on THE
                // tab that ran it instead of the brain guessing.
                let cmd = cmd.to_string();
                let sid = self.session().id;
                let sent = cmd.clone();
                self.spawn_request(
                    ClientRequest::Command { text: cmd.clone(), session_id: Some(sid) },
                    req_timeout(),
                    move |res| Bg::Command(res, sent),
                );
                return;
            }
            let handled = match cmd.split_whitespace().next().unwrap_or("") {
                "new" => Some(Cmd::NewSession),
                "clear" => Some(Cmd::ClearSession),
                "panel" => Some(Cmd::TogglePanel),
                "quit" | "exit" => Some(Cmd::Quit),
                _ => None,
            };
            if let Some(c) = handled {
                self.session_mut().input.clear();
                self.run_cmd(c);
                return;
            }
        }
        let text = self.session_mut().input.submit();
        let model = self.cfg.effective_model();
        let mode = self.session().mode;
        let id = self.session().id;
        // optimistic echo; the server's Snapshot event canonicalises
        self.session_mut().begin_turn(&text, &model);
        self.screen = Screen::Chat;
        self.dirty = true;
        // Backgrounded: the old blocking request froze the TUI for the whole
        // req_timeout while the brain was busy starting the turn.
        self.spawn_request(
            ClientRequest::Message { id, text, mode },
            req_timeout(),
            move |res| Bg::MessageSent {
                id,
                err: res.err(),
            },
        );
    }

    // ── queued queries (typed while the agent works) ──

    fn queue_select(&mut self, i: usize) {
        let s = self.session_mut();
        if !s.queue.is_empty() {
            s.queue_sel = i.min(s.queue.len() - 1);
        }
        self.dirty = true;
    }

    fn queue_move(&mut self, delta: i32) {
        let s = self.session_mut();
        if s.queue.is_empty() {
            return;
        }
        let n = s.queue.len() as i32;
        s.queue_sel = (s.queue_sel as i32 + delta).rem_euclid(n) as usize;
        self.dirty = true;
    }

    /// Remove the selected queued query and return it.
    fn queue_take_selected(&mut self) -> Option<String> {
        let s = self.session_mut();
        if s.queue.is_empty() {
            return None;
        }
        let i = s.queue_sel.min(s.queue.len() - 1);
        let text = s.queue.remove(i);
        s.queue_sel = s.queue_sel.min(s.queue.len().saturating_sub(1));
        s.touch();
        self.dirty = true;
        Some(text)
    }

    fn queue_delete_selected(&mut self) {
        if self.queue_take_selected().is_some() {
            self.toast("Removed from the queue");
        }
    }

    /// Pull the selected query back into the chatbox to edit it; enter queues
    /// it again, ctrl+s sends it.
    fn queue_edit_selected(&mut self) {
        if self.session().queue.is_empty() {
            return;
        }
        if !self.session().input.is_empty() {
            self.toast("Chatbox has text — send or clear it first (ctrl+c), then edit");
            return;
        }
        if let Some(text) = self.queue_take_selected() {
            self.session_mut().input.set(&text);
        }
    }

    /// Send the selected queued query to the agent (the ➤ icon).
    fn queue_send_selected(&mut self) {
        if let Some(text) = self.queue_take_selected() {
            self.send_steer(text);
        }
    }

    /// ctrl+s: the text typed in the chatbox goes to the agent NOW; with an
    /// empty chatbox it sends the selected queued query instead.
    fn steer_now(&mut self) {
        let typed = self.session().input.text();
        let t = typed.trim();
        if !t.is_empty() {
            if t.starts_with('/') {
                self.toast("Commands can't be sent to the agent — they run when it is idle");
                return;
            }
            let text = self.session_mut().input.submit().trim().to_string();
            self.send_steer(text);
        } else if self.session().queue.is_empty() {
            self.toast("Nothing to send — type a query first");
        } else {
            self.queue_send_selected();
        }
    }

    /// Hand `text` to the agent. Mid-task it is steering: the brain puts it in
    /// the chat at once and the agent reads it before its next step. On an
    /// idle session it is just a normal turn.
    fn send_steer(&mut self, text: String) {
        let id = self.session().id;
        if self.session().is_running() {
            self.toast("Sent — the agent reads it at its next step");
            let keep = text.clone();
            self.spawn_request(
                ClientRequest::Steer { id, text },
                req_timeout(),
                move |res| Bg::SteerSent {
                    id,
                    text: keep,
                    err: res.err(),
                },
            );
        } else {
            let model = self.cfg.effective_model();
            let mode = self.session().mode;
            self.session_mut().begin_turn(&text, &model);
            self.screen = Screen::Chat;
            self.dirty = true;
            self.spawn_request(
                ClientRequest::Message { id, text, mode },
                req_timeout(),
                move |res| Bg::MessageSent {
                    id,
                    err: res.err(),
                },
            );
        }
    }

    /// Re-fetch the config mirror (provider switch, ui prefs, ...).
    fn refresh_state(&mut self) {
        // Backgrounded hello: applies on the next poll tick.
        self.spawn_request(
            ClientRequest::Hello {
                version: env!("CARGO_PKG_VERSION").to_string(),
            },
            Duration::from_secs(10),
            Bg::Hello,
        );
    }

    /// Run a request on a worker thread; the reply lands in the `bg` queue
    /// and is applied by `poll`. The UI thread never blocks.
    fn spawn_request(
        &self,
        body: ClientRequest,
        timeout: Duration,
        map: impl FnOnce(Result<Reply, String>) -> Bg + Send + 'static,
    ) {
        let net = self.net.clone();
        let bg = self.bg.clone();
        std::thread::Builder::new()
            .name("tui-request".into())
            .spawn(move || {
                let res = net.request(body, timeout);
                let item = map(res);
                bg.lock()
                    .unwrap_or_else(|e| e.into_inner())
                    .push(item);
            })
            .ok();
    }

    fn resolve_approval(&mut self, allow: bool) {
        if !self.pending_approval.is_empty() {
            let pending = self.pending_approval.remove(0);
            self.net.fire(ClientRequest::Approval {
                session_id: pending.session_id,
                request_id: pending.id.clone(),
                approve: allow,
            });
            // The decision notice arrives back from the brain as a broadcast
            // Notice event (also applied to the server-side transcript), so
            // no local echo here — pushing one would draw the line twice.
            self.dirty = true;
        }
    }

    pub fn interrupt(&mut self) -> bool {
        let session_id = self.session().id;
        for i in (0..self.pending_approval.len()).rev() {
            if self.pending_approval[i].session_id == session_id {
                let pending = self.pending_approval.remove(i);
                self.net.fire(ClientRequest::Approval {
                    session_id,
                    request_id: pending.id,
                    approve: false,
                });
            }
        }
        let running = self.session().is_running();
        self.net.fire(ClientRequest::Cancel { id: session_id });
        if running {
            if let Some(s) = self.session_by_id_mut(session_id) {
                s.messages.push(Msg::Notice {
                    text: "Interrupted".into(),
                });
                s.started = None;
                s.touch();
            }
            self.dirty = true;
            return true;
        }
        false
    }

    /// Drain wire events, sync the device stream, pump the desktop window.
    /// Returns true if anything changed.
    pub fn poll(&mut self) -> bool {
        let mut changed = false;
        if self
            .terminal
            .as_mut()
            .map_or(false, |terminal| terminal.poll())
        {
            changed = true;
        }
        // the scrcpy-style desktop window pumps between terminal ticks:
        // input goes over the wire to the device, the latest frame is blitted
        if self.phone_window.is_some() {
            let alive = self
                .phone_window
                .as_mut()
                .map(|w| w.pump())
                .unwrap_or(false);
            if !alive {
                self.phone_window = None;
                self.toast("Phone window closed");
                self.dirty = true;
            }
        }
        // Reconnect loop (H8): the old code set `Net.closed` and never read it
        // — after a brain restart, streamed events stopped forever and every
        // request burned its full timeout with the screen just looking broken.
        if self.net.closed.load(std::sync::atomic::Ordering::Relaxed) {
            if self.reconnect_at.is_none() {
                self.reconnect_at = Some(Instant::now());
                self.toast("connection to the brain was lost — reconnecting…");
                self.dirty = true;
                changed = true;
            }
            if let Some(at) = self.reconnect_at {
                if Instant::now() >= at {
                    match Net::connect(&self.bind) {
                        Ok(new) => {
                            self.net = Arc::new(new);
                            self.reconnect_at = None;
                            // Re-sync: the new connection knows nothing about
                            // frame streaming — flip the flag so the block
                            // below re-fires the Stream request.
                            self.streaming_wanted = !self.streaming_wanted;
                            self.toast("reconnected to romacode-server");
                            self.refresh_state();
                            // Re-sync EVERY known session, not just the newest:
                            // `SelectSession` (the per-session snapshot request
                            // a tab switch uses) re-subscribes each session, so
                            // sessions that changed during the outage converge
                            // and a turn that finished during the outage no
                            // longer leaves its spinner stuck. The ACTIVE tab
                            // is selected last so the server's current session
                            // ends up matching the UI.
                            let active = self.sessions[self.active].id;
                            let mut ids: Vec<usize> =
                                self.sessions.iter().map(|s| s.id).collect();
                            ids.sort_by_key(|id| *id == active);
                            for sid in ids {
                                self.net.fire(ClientRequest::SelectSession { id: sid });
                            }
                        }
                        Err(_) => {
                            // backoff: 2s, then 4s, capped at 5s
                            let next = 5_u64.min(at.elapsed().as_secs() * 2 + 2);
                            self.reconnect_at = Some(Instant::now() + Duration::from_secs(next));
                        }
                    }
                    changed = true;
                }
            }
        }
        // the phone streams frames while its screen is watched — in the TUI
        // (fullscreen/panel) or by the scrcpy-style desktop window
        let visible = self.android_full || self.device == Device::Android;
        let want = visible || self.phone_window.is_some();
        if want != self.streaming_wanted {
            self.streaming_wanted = want;
            self.net.fire(ClientRequest::Android {
                action: AndroidAction::Stream { on: want },
            });
        }
        self.android.streaming = want;

        // pump wire events (bounded per tick so typing stays snappy)
        let mut batch = Vec::new();
        // Recover from a poisoned lock instead of dropping ALL wire events
        // forever (every other mutex here already uses into_inner recovery).
        let events = self.net.events.lock().unwrap_or_else(|e| e.into_inner());
        for _ in 0..512 {
            match events.try_recv() {
                Ok(ev) => batch.push(ev),
                Err(std::sync::mpsc::TryRecvError::Empty) => break,
                Err(std::sync::mpsc::TryRecvError::Disconnected) => break,
            }
        }
        drop(events);
        for ev in batch {
            match ev {
                UiEvent::Wire(e) => {
                    if self.apply_event(&e) {
                        changed = true;
                    }
                }
                UiEvent::Welcome(w) => {
                    // req-0 Welcome refresh (settings/provider changed —
                    // previously silently dropped by the reply matcher).
                    self.apply_welcome(&w);
                    changed = true;
                }
                UiEvent::Malformed(text) => {
                    self.toast(text);
                    changed = true;
                }
            }
        }
        // apply backgrounded request replies
        let drained: Vec<Bg> = self
            .bg
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .drain(..)
            .collect();
        for item in drained {
            match item {
                Bg::Command(res, sent) => match res {
                    Ok(Reply::Notice { text: notice }) => {
                        let s = self.session_mut();
                        // Only clear the input when it still holds exactly the
                        // command that was sent — text typed during the round
                        // trip must not be wiped.
                        if s.input.text().trim() == sent.trim() {
                            s.input.clear();
                        }
                        s.messages.push(Msg::Notice { text: notice });
                        s.scroll_back = 0;
                        s.touch();
                        self.screen = Screen::Chat;
                        self.refresh_state();
                        changed = true;
                    }
                    Ok(_) => {}
                    Err(e) => self.toast(format!("/command failed: {e}")),
                },
                Bg::MessageSent { id, err } => {
                    if let Some(e) = err {
                        if let Some(s) = self.session_by_id_mut(id) {
                            s.started = None;
                            s.messages.push(Msg::Error { text: e.clone() });
                            s.touch();
                        }
                        self.toast(e);
                        changed = true;
                    }
                }
                Bg::SteerSent { id, text, err } => {
                    if let Some(e) = err {
                        if let Some(s) = self.session_by_id_mut(id) {
                            if s.queue.len() < crate::session::QUEUE_CAP {
                                s.queue.push(text);
                                s.queue_sel = s.queue.len() - 1;
                            }
                            s.touch();
                        }
                        self.toast(format!("not sent: {e}"));
                        changed = true;
                    }
                }
                Bg::Hello(res) => {
                    if let Ok(Reply::Welcome(w)) = res {
                        self.apply_welcome(&w);
                        changed = true;
                    }
                }
                Bg::Saved(res) => match res {
                    Ok(Reply::Notice { text: notice }) => {
                        self.settings = None;
                        self.toast(notice);
                        self.refresh_state();
                        changed = true;
                    }
                    Ok(_) => self.toast("Model saved."),
                    Err(e) => {
                        // Keep the screen open and show WHY — a silent failure
                        // looked exactly like "models are not saving".
                        if let Some(st) = self.settings.as_mut() {
                            st.error = Some(format!("model not saved: {e}"));
                        }
                        changed = true;
                    }
                },
            }
        }
        // approval prompts belong to sessions that still exist. Pruning them
        // merely because the session is not LOCALLY "running" deleted
        // approvals that were restored from a snapshot (or raised while the
        // brain had running=false) — the agent then waited on a y/n the user
        // could no longer see. An answered prompt is removed by
        // `resolve_approval`; a closed session removes its prompts here.
        self.pending_approval.retain(|p| {
            self.sessions.iter().any(|s| s.id == p.session_id)
        });
        if let Some(rx) = self.settings_fetch.take() {
            match rx.try_recv() {
                Ok(result) => {
                    if let Some(st) = self.settings.as_mut() {
                        st.loading = false;
                        match result {
                            Ok(models) => st.begin_models(models),
                            // The fetch failed, but the key is already saved
                            // and the user still needs model options: a KNOWN
                            // provider falls back to its catalog suggestions
                            // (each pick persists like a fetched model would)
                            // instead of an empty picker; a custom provider
                            // shows the error alone.
                            Err(e) => st.begin_models_fallback(&e),
                        }
                    }
                    changed = true;
                }
                Err(std::sync::mpsc::TryRecvError::Empty) => self.settings_fetch = Some(rx),
                Err(std::sync::mpsc::TryRecvError::Disconnected) => {
                    if let Some(st) = self.settings.as_mut() {
                        st.loading = false;
                        st.error = Some("model fetch stopped unexpectedly; retry".into());
                    }
                    changed = true;
                }
            }
        }
        if let Some((_, at)) = &self.toast {
            if at.elapsed() > Duration::from_millis(2600) {
                self.toast = None;
                changed = true;
            }
        }
        changed
    }

    // ── commands ──
    pub fn run_cmd(&mut self, c: Cmd) {
        match c {
            Cmd::NewSession => self.new_session(),
            Cmd::CloseSession => self.close_session(self.active),
            Cmd::NextSession => self.select((self.active + 1) % self.sessions.len()),
            Cmd::PrevSession => {
                self.select((self.active + self.sessions.len() - 1) % self.sessions.len())
            }
            Cmd::TogglePanel => {
                self.panel = !self.panel;
                self.cfg.sidebar = self.panel;
                self.net.fire(ClientRequest::SetUiPref {
                    key: "sidebar".into(),
                    value: if self.panel { "true".into() } else { "false".into() },
                });
            }
            Cmd::ToggleAndroid => self.toggle_phone_window(),
            Cmd::ToggleTerminal => self.toggle_terminal(),
            Cmd::WidenChat => self.resize_chat(4),
            Cmd::NarrowChat => self.resize_chat(-4),
            Cmd::ToggleMode => {
                let s = self.session_mut();
                s.mode = s.mode.toggle();
                let id = s.id;
                let mode = s.mode;
                self.net.fire(ClientRequest::ToggleMode { id });
                let _ = mode;
            }
            Cmd::ClearSession => {
                let id = self.session().id;
                let mode = self.session().mode;
                *self.session_mut() = UiSession::new(id);
                self.session_mut().mode = mode;
                if self.sessions.len() == 1 {
                    self.screen = Screen::Home;
                }
            }
            Cmd::Quit => self.quit = true,
        }
        self.dirty = true;
    }

    /// The android icon / alt+a: detach the phone display from the terminal.
    /// A scrcpy-style desktop window opens — a real OS window owned by this
    /// process (no browser, no HTTP, no external binary) showing the live
    /// screen with scrcpy input parity: tap/swipe, right = BACK, middle =
    /// HOME, wheel = scroll, typing drives the device. Toggles: again closes
    /// the window and stops the stream. Without a desktop display server
    /// (headless / bare console) the old inline panel takes over unchanged.
    fn toggle_phone_window(&mut self) {
        if self.phone_window.is_some() {
            self.phone_window = None; // drops the OS window
            self.device = Device::None;
            self.android_full = false;
            self.toast("Phone window closed — alt+a reopens");
        } else {
            let title = match self.android.status.as_ref() {
                Some(st) if st.phase == "online" => {
                    format!("RomaCode · {} — {}", st.model, st.serial)
                }
                _ => "RomaCode · Android — waiting for the brain's device…".to_string(),
            };
            match crate::display::PhoneWindow::open(
                self.frame_hub.clone(),
                self.net.clone(),
                title,
            ) {
                Ok(w) => {
                    self.phone_window = Some(w);
                    self.toast("Phone attached — scrcpy-style window opened");
                }
                Err(_) => {
                    // headless or no display server: same view stays in the terminal
                    self.device = Device::Android;
                    if self.screen == Screen::Home {
                        self.screen = Screen::Chat;
                    }
                    self.toast("No desktop display — inline panel");
                }
            }
        }
        self.dirty = true;
    }

    fn toggle_terminal(&mut self) {
        if self.screen == Screen::Terminal {
            self.screen = self.terminal_return;
            self.dirty = true;
            return;
        }
        if self
            .terminal
            .as_ref()
            .map_or(false, |t| t.exit_code().is_some())
        {
            self.terminal = None;
        }
        if self.terminal.is_none() {
            let cwd = std::env::current_dir().unwrap_or_default();
            match crate::terminal::RoxtermTerminal::spawn(80, 22, &cwd) {
                Ok(terminal) => self.terminal = Some(terminal),
                Err(error) => {
                    self.toast(format!("Could not start native terminal: {error}"));
                    return;
                }
            }
        }
        self.terminal_return = self.screen;
        self.screen = Screen::Terminal;
        self.android_full = false;
        self.palette = None;
        self.dirty = true;
    }

    fn resize_chat(&mut self, delta: i16) {
        let v = (self.cfg.chat_width_pct as i16 + delta).clamp(20, 70) as u16;
        self.cfg.chat_width_pct = v;
        self.net.fire(ClientRequest::SetUiPref {
            key: "chat_width_pct".into(),
            value: v.to_string(),
        });
        self.toast(format!("Chat width {v}%"));
    }

    fn run_action(&mut self, a: Action) {
        match a {
            Action::SelectTab(i) => self.select(i),
            Action::CloseTab(i) => self.close_session(i),
            Action::NewTab => self.new_session(),
            Action::TogglePanel => self.run_cmd(Cmd::TogglePanel),
            Action::ToggleAndroid => self.run_cmd(Cmd::ToggleAndroid),
            Action::CloseDevice => {
                self.device = Device::None;
                self.android_full = false;
            }
            Action::PhoneFullscreen => {
                self.android_full = !self.android_full;
            }
            Action::ToggleMode => self.run_cmd(Cmd::ToggleMode),
            Action::QueueSelect(i) => self.queue_select(i),
            Action::QueueSend(i) => {
                self.queue_select(i);
                self.queue_send_selected();
            }
            Action::QueueEdit(i) => {
                self.queue_select(i);
                self.queue_edit_selected();
            }
            Action::QueueDelete(i) => {
                self.queue_select(i);
                self.queue_delete_selected();
            }
            Action::Refresh => {
                // home screen via the phone's nav
                self.net.fire(ClientRequest::Android {
                    action: AndroidAction::Key {
                        key: PhoneKey::Home.as_str().to_string(),
                    },
                });
            }
            Action::OpenTool(mi, bi) => {
                self.tool_detail = Some((mi, bi));
                self.tool_scroll = 0;
                self.dirty = true;
            }
            Action::Swallow => {
                // Click landed on a panel background — intentionally nothing.
            }
            Action::PaletteRun(i) => {
                self.palette = None;
                if let Some((c, _, _)) = COMMANDS.get(i) {
                    self.run_cmd(*c);
                }
            }
        }
        self.dirty = true;
    }

    // ── events ──
    pub fn on_event(&mut self, ev: Event) {
        self.dirty = true;
        match ev {
            Event::Key(k) if k.kind != KeyEventKind::Release => self.on_key(k),
            Event::Mouse(m) => self.on_mouse(m),
            Event::Paste(s) => {
                if self.screen == Screen::Terminal {
                    if let Some(terminal) = self.terminal.as_mut() {
                        if let Err(error) = terminal.send_paste(&s) {
                            self.toast(error);
                        }
                    }
                } else if let Some(st) = self.settings.as_mut() {
                    // paste goes to the focused settings field — an API key
                    // pasted in the provider screens must never land in chat
                    match st.screen {
                        crate::settings::Screen::Root => st.query_paste(&s),
                        crate::settings::Screen::Tools => st.tools_query_paste(&s),
                        crate::settings::Screen::Providers => st.provider_query_paste(&s),
                        crate::settings::Screen::Models | crate::settings::Screen::SelectModel => {
                            st.model_query_paste(&s)
                        }
                        _ => st.input_paste(&s),
                    }
                } else if let Some(tree) = self.tree.as_mut() {
                    // the Ctrl+P tree is a modal: pasted text filters it —
                    // it used to land in the hidden chat input underneath
                    for c in s.chars().filter(|c| !c.is_control()) {
                        tree.query.push(c);
                    }
                    tree.sel = 0;
                    tree.top = 0;
                } else if let Some(sl) = self.slash.as_mut() {
                    // the slash menu is a modal too
                    for c in s.chars().filter(|c| !c.is_control()) {
                        sl.query.push(c);
                    }
                    sl.sel = 0;
                    sl.top = 0;
                } else if let Some(p) = self.palette.as_mut() {
                    for c in s.chars().filter(|c| !c.is_control()) {
                        p.query.push(c);
                    }
                    p.sel = 0;
                } else if self.android_full {
                    let text: String = s.chars().filter(|c| !c.is_control()).collect();
                    if !text.is_empty() {
                        self.device_text(&text);
                    }
                } else {
                    self.session_mut().input.insert_str(&s);
                }
            }
            Event::Resize(width, height) => {
                if self.screen == Screen::Terminal {
                    if let Some(terminal) = self.terminal.as_mut() {
                        let _ = terminal.resize(width, height.saturating_sub(2));
                    }
                }
            }
            _ => {}
        }
    }

    fn on_key(&mut self, k: KeyEvent) {
        let ctrl = k.modifiers.contains(KeyModifiers::CONTROL);
        let alt = k.modifiers.contains(KeyModifiers::ALT);
        let shift = k.modifiers.contains(KeyModifiers::SHIFT);

        if self.screen == Screen::Terminal {
            // A pending permission popup is modal even here: y / n / esc
            // answer it and ctrl+c interrupts the turn; alt+t still leaves
            // the terminal. Every other key belongs to the child shell.
            if !self.pending_approval.is_empty() {
                if alt && matches!(k.code, KeyCode::Char('t' | 'T')) {
                    self.toggle_terminal();
                } else if ctrl && matches!(k.code, KeyCode::Char('c')) {
                    self.interrupt();
                } else {
                    match k.code {
                        KeyCode::Char('y' | 'Y') => self.resolve_approval(true),
                        KeyCode::Char('n' | 'N') | KeyCode::Esc => self.resolve_approval(false),
                        _ => {
                            if let Some(terminal) = self.terminal.as_mut() {
                                if let Err(error) = terminal.send_key(k) {
                                    self.toast(error);
                                }
                            }
                        }
                    }
                }
                return;
            }
            if alt && matches!(k.code, KeyCode::Char('t' | 'T')) {
                self.toggle_terminal();
            } else if shift && k.code == KeyCode::PageUp {
                if let Some(terminal) = self.terminal.as_mut() {
                    let page = terminal.screen.rows().saturating_sub(2) as isize;
                    terminal.scroll_scrollback(page);
                }
            } else if shift && k.code == KeyCode::PageDown {
                if let Some(terminal) = self.terminal.as_mut() {
                    let page = terminal.screen.rows().saturating_sub(2) as isize;
                    terminal.scroll_scrollback(-page);
                }
            } else if let Some(terminal) = self.terminal.as_mut() {
                if let Err(error) = terminal.send_key(k) {
                    self.toast(error);
                }
            }
            return;
        }

        // Normalise the legacy control encodings of Enter / Backspace so the
        // chat input behaves the same in every terminal: Ctrl+M is Enter,
        // Ctrl+H is Backspace, and raw \r / \n / DEL bytes are their keys.
        // (The native-terminal screen above is deliberately excluded — there
        // every byte belongs to the child shell.)
        let mut k = k;
        if ctrl && matches!(k.code, KeyCode::Char('m')) {
            k.modifiers.remove(KeyModifiers::CONTROL);
            k.code = KeyCode::Enter;
        } else if ctrl && matches!(k.code, KeyCode::Char('h')) {
            k.code = KeyCode::Backspace; // keeps ctrl → kill-word, like ctrl+backspace
        } else if matches!(k.code, KeyCode::Char('\r') | KeyCode::Char('\n')) {
            k.code = KeyCode::Enter;
        } else if matches!(k.code, KeyCode::Char('\u{7f}') | KeyCode::Char('\u{8}')) {
            k.code = KeyCode::Backspace;
        }
        let ctrl = k.modifiers.contains(KeyModifiers::CONTROL);

        if !self.pending_approval.is_empty() {
            if self.pending_approval[0].session_id != self.session().id {
                if let Some(i) = self
                    .sessions
                    .iter()
                    .position(|s| s.id == self.pending_approval[0].session_id)
                {
                    self.select(i);
                    self.toast(
                        "Review the exact pending effect in this session, then press y or n",
                    );
                } else {
                    self.resolve_approval(false);
                }
                return;
            }
            if ctrl && matches!(k.code, KeyCode::Char('c')) {
                self.interrupt();
                return;
            }
            match k.code {
                KeyCode::Char('y' | 'Y') => self.resolve_approval(true),
                KeyCode::Char('n' | 'N') | KeyCode::Esc => self.resolve_approval(false),
                _ => self.toast("Approval pending: press y to allow once or n to deny"),
            }
            return;
        }
        if self.slash.is_some() {
            return self.on_slash_key(k);
        }
        if self.tree.is_some() {
            return self.on_tree_key(k);
        }
        if self.settings.is_some() {
            return self.on_settings_key(k);
        }
        if self.palette.is_some() {
            return self.on_palette_key(k);
        }
        // Tool-details modal (click on a ▸ tool row, or ctrl+o): scroll the
        // full input/output, esc / q / enter closes. Modal like the palette.
        if self.tool_detail.is_some() {
            let lines = super::ui::tool_detail::lines_for(self);
            let page = self
                .transcript_rect
                .height
                .saturating_sub(4)
                .max(1) as usize;
            let max_scroll = lines.saturating_sub(page);
            match k.code {
                KeyCode::Esc | KeyCode::Enter | KeyCode::Char('q' | 'Q') => {
                    self.tool_detail = None;
                }
                KeyCode::Up | KeyCode::Char('k' | 'K') => {
                    self.tool_scroll = self.tool_scroll.saturating_sub(1);
                }
                KeyCode::Down | KeyCode::Char('j' | 'J') => {
                    self.tool_scroll = (self.tool_scroll + 1).min(max_scroll);
                }
                KeyCode::PageUp => {
                    self.tool_scroll = self.tool_scroll.saturating_sub(page);
                }
                KeyCode::PageDown | KeyCode::Char(' ') => {
                    self.tool_scroll = (self.tool_scroll + page).min(max_scroll);
                }
                KeyCode::Home => self.tool_scroll = 0,
                KeyCode::End => self.tool_scroll = max_scroll,
                _ => {}
            }
            self.dirty = true;
            return;
        }
        // ctrl+o anywhere in chat: inspect the most recent tool call.
        if ctrl && matches!(k.code, KeyCode::Char('o' | 'O')) {
            self.open_latest_tool();
            return;
        }
        // fullscreen phone: typing drives the device (over the wire)
        if self.android_full && !ctrl && !alt {
            match k.code {
                KeyCode::Esc => {
                    self.android_full = false;
                    self.dirty = true;
                    return;
                }
                KeyCode::Enter => {
                    self.device_key(PhoneKey::Enter);
                    return;
                }
                KeyCode::Backspace => {
                    self.device_key(PhoneKey::Del);
                    return;
                }
                KeyCode::Char(c) => {
                    self.device_text(&c.to_string());
                    return;
                }
                KeyCode::Left | KeyCode::Up | KeyCode::Right | KeyCode::Down => {
                    // nav inside the phone via swipe-like keys (no-op fallback)
                    return;
                }
                _ => {}
            }
        }
        match (k.code, ctrl, alt) {
            (KeyCode::Char('c'), true, _) => {
                if self.interrupt() {
                    return;
                }
                if !self.session().input.is_empty() {
                    self.session_mut().input.clear();
                    return;
                }
                if self
                    .last_ctrl_c
                    .map_or(false, |t| t.elapsed() < Duration::from_millis(1500))
                {
                    self.quit = true;
                } else {
                    self.last_ctrl_c = Some(Instant::now());
                    self.toast("Press ctrl+c again to quit");
                }
            }
            (KeyCode::Char('q'), true, _) => self.quit = true,
            (KeyCode::Char('n'), true, _) => self.new_session(),
            (KeyCode::Char('w'), true, _) => self.close_session(self.active),
            (KeyCode::Char('b'), true, _) => self.run_cmd(Cmd::TogglePanel),
            // Ctrl+P opens the hierarchical Settings tree (Permission at top,
            // then the seven groups: General, AI & Server, Input, Editor,
            // Workspace, System, Advanced). Ctrl+Shift+P is the same overlay.
            (KeyCode::Char('p'), true, _) => {
                if self.tree.is_some() {
                    self.tree = None;
                    self.tree_hits.clear();
                } else if self.settings.is_some() {
                    // legacy provider/tools flow still open: close it too
                    self.settings = None;
                    self.settings_fetch = None;
                    self.settings_hits.clear();
                } else {
                    self.palette = None;
                    self.tree = Some(crate::tree::Tree::root());
                    self.tree_hits.clear();
                }
            }
            (KeyCode::Char('a'), _, true) => self.run_cmd(Cmd::ToggleAndroid),
            (KeyCode::Char('t' | 'T'), _, true) => self.run_cmd(Cmd::ToggleTerminal),
            (KeyCode::Char(']'), _, true) => self.run_cmd(Cmd::WidenChat),
            (KeyCode::Char('['), _, true) => self.run_cmd(Cmd::NarrowChat),
            (KeyCode::Right, _, true) => self.run_cmd(Cmd::NextSession),
            (KeyCode::Left, _, true) => self.run_cmd(Cmd::PrevSession),
            (KeyCode::Char(c), _, true) if c.is_ascii_digit() && c != '0' => {
                self.select(c as usize - '1' as usize)
            }
            (KeyCode::Esc, _, _) => {
                if self.android_full {
                    self.android_full = false;
                } else if !self.interrupt() && self.device != Device::None {
                    self.device = Device::None;
                } else if self.phone_window.is_some() {
                    self.toggle_phone_window();
                }
            }
            // queued queries: ctrl+s sends, alt+e edits, alt+d deletes,
            // alt+↑/↓ moves the selection (plain ↑/↓ stay input history)
            (KeyCode::Char('s'), true, _) => self.steer_now(),
            (KeyCode::Char('e'), false, true) => self.queue_edit_selected(),
            (KeyCode::Char('d'), false, true) => self.queue_delete_selected(),
            (KeyCode::Up, _, true) => self.queue_move(-1),
            (KeyCode::Down, _, true) => self.queue_move(1),
            (KeyCode::Tab, _, _) => self.run_cmd(Cmd::ToggleMode),
            (KeyCode::BackTab, _, _) => self.run_cmd(Cmd::ToggleMode),
            (KeyCode::PageUp, _, _) => self.scroll(self.page()),
            (KeyCode::PageDown, _, _) => self.scroll(-self.page()),
            (KeyCode::End, true, _) => self.session_mut().scroll_back = 0,
            (KeyCode::Enter, _, _) => {
                if shift || alt {
                    self.session_mut().input.newline();
                } else {
                    self.send();
                }
            }
            (KeyCode::Char('j'), true, _) => self.session_mut().input.newline(),
            (KeyCode::Char('u'), true, _) => self.session_mut().input.kill_to_start(),
            (KeyCode::Char('k'), true, _) => self.session_mut().input.kill_to_end(),
            (KeyCode::Char('a'), true, _) => self.session_mut().input.home(),
            (KeyCode::Char('e'), true, _) => self.session_mut().input.end(),
            (KeyCode::Backspace, true, _) | (KeyCode::Backspace, _, true) => {
                self.session_mut().input.kill_word()
            }
            (KeyCode::Left, true, _) => self.session_mut().input.word_left(),
            (KeyCode::Right, true, _) => self.session_mut().input.word_right(),
            (KeyCode::Backspace, _, _) => self.session_mut().input.backspace(),
            (KeyCode::Delete, _, _) => self.session_mut().input.delete(),
            (KeyCode::Left, _, _) => self.session_mut().input.left(),
            (KeyCode::Right, _, _) => self.session_mut().input.right(),
            (KeyCode::Home, _, _) => self.session_mut().input.home(),
            (KeyCode::End, _, _) => self.session_mut().input.end(),
            (KeyCode::Up, _, _) => self.session_mut().input.up(),
            (KeyCode::Down, _, _) => self.session_mut().input.down(),
            (KeyCode::Char(c), false, false) => {
                // `/` typed at the start of an empty input opens the slash
                // menu (the settings tree as a `/`-prefixed command popup).
                // Otherwise the character is inserted into the chat input.
                if c == '/' && self.session().input.is_empty() {
                    self.slash = Some(crate::tree::SlashMenu::open());
                    self.tree_hits.clear();
                    self.dirty = true;
                } else {
                    self.session_mut().input.insert(c);
                }
            }
            (KeyCode::Char(c), false, true) if shift => self.session_mut().input.insert(c),
            _ => {}
        }
    }

    fn page(&self) -> i32 {
        (self.transcript_rect.height as i32 - 2).max(3)
    }
    fn scroll(&mut self, rows: i32) {
        let s = self.session_mut();
        s.scroll_back = (s.scroll_back as i32 + rows).max(0) as usize;
    }

    fn start_model_fetch(&mut self) {
        let Some(st) = self.settings.as_mut() else {
            return;
        };
        if st.base_url.trim().is_empty() {
            st.error = Some("base URL is required".into());
            return;
        }
        st.loading = true;
        st.error = None;
        let base = st.base_url.clone();
        let kind = st.kind.as_str().to_string();
        let key = st.api_key.clone();
        let key_fetch = key.clone();
        let net = self.net.clone();
        // Persist the provider entry (key + base URL + protocol) BEFORE the
        // model fetch: when the provider is overloaded (NVIDIA NIM, minimax,
        // …) the fetch fails, and previously that threw away the key the user
        // just typed — the provider had to be re-entered from scratch and
        // nothing ever appeared under "models". The entry now survives.
        let (pid, pname) = (st.provider_id.clone(), st.provider_name.clone());
        let pid_fetch = pid.clone();
        let kindv = kind.clone();
        // One worker thread does BOTH steps (persist the provider entry, then
        // fetch the catalog) and delivers the outcome through the channel the
        // settings screen already drains. The old code ran the SaveModel step
        // INLINE on the UI thread — up to req_timeout frozen per call.
        let (tx, rx) = std::sync::mpsc::channel();
        std::thread::spawn(move || {
            let saved = net.request(
                ClientRequest::SaveModel {
                    provider_id: pid,
                    provider_name: pname,
                    base_url: base.clone(),
                    api_key: key,
                    kind: kindv,
                    model: String::new(), // provider-only save; keeps the model
                    models: Vec::new(),
                },
                req_timeout(),
            );
            let res = match saved {
                Err(e) => Err(format!("could not save provider: {e}")),
                Ok(_) => {
                    // live catalog lookup happens in the brain; `provider_id`
                    // tags the entry so the brain REMEMBERS the fetched list
                    // in that provider's model options (previously the list
                    // was echoed once and lost when the panel closed)
                    match net.request(
                        ClientRequest::FetchModels {
                            provider_id: pid_fetch,
                            base_url: base,
                            kind,
                            api_key: key_fetch,
                        },
                        Duration::from_secs(45),
                    ) {
                        Ok(Reply::Models { models }) => Ok(models),
                        Ok(_) => Err("unexpected reply while fetching models".into()),
                        Err(e) => Err(e),
                    }
                }
            };
            let _ = tx.send(res);
        });
        self.settings_fetch = Some(rx);
    }
    fn save_settings_model(&mut self, model: String) {
        let Some(st) = self.settings.as_ref() else {
            return;
        };
        // Never silently drop a model pick: when the screen has no provider
        // id (e.g. a row picked straight off the palette), fall back to the
        // active provider so the choice still lands in the config.
        let id = if st.provider_id.is_empty() {
            self.cfg.provider.clone()
        } else {
            st.provider_id.clone()
        };
        if id.trim().is_empty() {
            if let Some(st) = self.settings.as_mut() {
                st.error = Some("no provider selected — pick a provider first".into());
            }
            return;
        }
        let name = st.provider_name.clone();
        let base = st.base_url.clone();
        let key = st.api_key.clone();
        let kind = st.kind.as_str().to_string();
        let models = st.models.clone();
        // Backgrounded: the old blocking request froze the TUI for req_timeout.
        // The screen stays open with `loading` semantics until the reply lands.
        self.spawn_request(
            ClientRequest::SaveModel {
                provider_id: id,
                provider_name: name,
                base_url: base,
                api_key: key,
                kind,
                model,
                models,
            },
            req_timeout(),
            Bg::Saved,
        );
        self.dirty = true;
    }

    /// Resolve `(model, provider)` for the selected row on the Models /
    /// SelectModel screen, WITHOUT touching the network — so the Enter handler
    /// and the unit tests share one source of truth.
    ///
    /// `model` is the selected entry of `st.models`. `provider` is the matching
    /// entry of `st.model_providers` (SelectModel, one provider per row); on
    /// the Models screen `model_providers` is empty, so we fall back to the
    /// screen's own `provider_id` (set earlier by `begin_known` / `begin_custom`).
    /// That fallback is the fix for "Enter on a fetched model does nothing".
    pub(crate) fn models_screen_selection(
        &self,
    ) -> Option<(Option<String>, Option<String>)> {
        let st = self.settings.as_ref()?;
        // `sel` is an index into the FILTERED list; map it back to the original
        // model index before touching the parallel provider vector.
        let mi = st.selected_model_index()?;
        let model = st.models.get(mi).cloned();
        let provider = st
            .model_providers
            .get(mi)
            .cloned()
            .or_else(|| {
                (!st.provider_id.is_empty()).then(|| st.provider_id.clone())
            });
        Some((model, provider))
    }

    fn on_settings_key(&mut self, mut k: KeyEvent) {
        use crate::settings::{RootAction, Screen};
        // A few terminals / keyboard protocols deliver Enter and Backspace as
        // plain control characters instead of the dedicated key codes; match
        // on the exact bytes so Enter and Backspace work everywhere.
        if k.modifiers.contains(KeyModifiers::CONTROL)
            && matches!(k.code, KeyCode::Char('m'))
        {
            k.code = KeyCode::Enter;
        } else if matches!(k.code, KeyCode::Char('\r') | KeyCode::Char('\n')) {
            k.code = KeyCode::Enter;
        } else if matches!(k.code, KeyCode::Char('\u{7f}') | KeyCode::Char('\u{8}')) {
            k.code = KeyCode::Backspace;
        }
        let ctrl = k.modifiers.contains(KeyModifiers::CONTROL);
        // Global hotkeys keep working while the settings overlay is open, so
        // the user is never trapped: Ctrl+Q quits, Ctrl+N opens a fresh
        // session, Ctrl+W closes the current one, Ctrl+C interrupts the agent
        // (and closes the overlay if there is nothing to interrupt).
        if ctrl && matches!(k.code, KeyCode::Char('q')) {
            self.quit = true;
            return;
        }
        if ctrl && matches!(k.code, KeyCode::Char('n')) {
            self.settings = None;
            self.settings_fetch = None;
            self.new_session();
            return;
        }
        if ctrl && matches!(k.code, KeyCode::Char('w')) {
            self.settings = None;
            self.settings_fetch = None;
            self.close_session(self.active);
            return;
        }
        if ctrl && matches!(k.code, KeyCode::Char('c')) {
            if self.interrupt() {
                return;
            }
            self.settings = None;
            self.settings_fetch = None;
            self.toast("Settings closed");
            return;
        }
        // Ctrl+P toggles the panel from inside too (parity with opening it).
        if ctrl && matches!(k.code, KeyCode::Char('p')) {
            self.settings = None;
            self.settings_fetch = None;
            return;
        }

        let screen = match self.settings.as_ref() {
            Some(st) => st.screen,
            None => return,
        };
        if self.settings.as_ref().map_or(false, |st| st.loading) {
            if matches!(k.code, KeyCode::Esc) {
                if let Some(st) = self.settings.as_mut() {
                    st.loading = false;
                }
                self.settings_fetch = None;
            }
            return;
        }
        match k.code {
            KeyCode::Esc => match screen {
                Screen::Root | Screen::Providers | Screen::Models | Screen::SelectModel => {
                    self.settings = None;
                    self.settings_fetch = None;
                }
                Screen::KnownKey => {
                    // the API-key screen is always reached from Providers
                    if let Some(st) = self.settings.as_mut() {
                        st.screen = Screen::Providers;
                        st.input.clear();
                        st.cursor = 0;
                        st.error = None;
                    }
                }
                Screen::CustomName => {
                    // first step of the flow — back to the provider picker
                    if let Some(st) = self.settings.as_mut() {
                        st.screen = Screen::Providers;
                        st.sel = 0;
                        st.input.clear();
                        st.cursor = 0;
                        st.error = None;
                    }
                }
                Screen::CustomBase | Screen::CustomKey => {
                    // the custom form is reached from the Settings page's
                    // Providers row — Esc leaves Settings entirely (the old
                    // Root list no longer exists as a user-facing screen)
                    self.settings = None;
                    self.settings_fetch = None;
                }
                Screen::Tools => {
                    self.settings = None;
                    self.settings_fetch = None;
                }
                Screen::ToolsInput => {
                    if let Some(st) = self.settings.as_mut() {
                        st.screen = Screen::Tools;
                        st.input.clear();
                        st.cursor = 0;
                        st.error = None;
                        st.tools_result = None;
                    }
                }
            },
            KeyCode::Up | KeyCode::Down => {
                // Tool names are needed to compute the Tools list length; pull
                // them once from the (disjoint) tools bridge before borrowing
                // settings mutably. Movement NEVER wraps: at the first row Up
                // stays put, at the last row Down stays put.
                let tool_names: Vec<String> =
                    self.tools.list().iter().map(|t| t.name.clone()).collect();
                if let Some(st) = self.settings.as_mut() {
                    let max = st.visible_len(&tool_names).saturating_sub(1);
                    match (k.code, st.screen) {
                        (KeyCode::Up, Screen::Tools) => {
                            st.tools_sel = st.tools_sel.saturating_sub(1)
                        }
                        (KeyCode::Up, _) => st.sel = st.sel.saturating_sub(1).min(max),
                        (KeyCode::Down, Screen::Tools) => {
                            st.tools_sel = st.tools_sel.saturating_add(1).min(max)
                        }
                        (KeyCode::Down, _) => st.sel = st.sel.saturating_add(1).min(max),
                        _ => {}
                    }
                }
            }
            // Cursor movement for the input screens (API key, base URL):
            // ←→ Home End move the cursor inside the field.
            KeyCode::Left | KeyCode::Right | KeyCode::Home | KeyCode::End | KeyCode::Delete
                if !matches!(screen, Screen::Root | Screen::Tools) =>
            {
                if let Some(st) = self.settings.as_mut() {
                    match k.code {
                        KeyCode::Left => st.input_left(),
                        KeyCode::Right => st.input_right(),
                        KeyCode::Home => st.input_home(),
                        KeyCode::End => st.input_end(),
                        _ => st.input_delete(),
                    }
                    st.error = None;
                }
            }
            // Accept the three forms terminals send for Backspace:
            //   KeyCode::Backspace (crossterm normalizes this)
            //   Char('\u{7f}')   (DEL — xterm default)
            //   Char('\u{8}')    (Ctrl+H — some SSH clients)
            KeyCode::Backspace | KeyCode::Char('\u{7f}') | KeyCode::Char('\u{8}') => {
                if let Some(st) = self.settings.as_mut() {
                    match st.screen {
                        Screen::Root => {
                            st.query.pop();
                            st.sel = 0;
                        }
                        Screen::Tools => {
                            st.tools_query.pop();
                            st.tools_sel = 0;
                        }
                        Screen::Providers => {
                            st.provider_query.pop();
                            st.sel = 0;
                        }
                        Screen::Models | Screen::SelectModel => {
                            st.model_query.pop();
                            st.sel = 0;
                        }
                        _ => {
                            st.input_backspace();
                        }
                    }
                    st.error = None;
                }
            }
            KeyCode::Char(c) if !ctrl => {
                if let Some(st) = self.settings.as_mut() {
                    match st.screen {
                        Screen::Root => {
                            st.query.push(c);
                            st.sel = 0;
                        }
                        Screen::Tools => {
                            st.tools_query.push(c);
                            st.tools_sel = 0;
                        }
                        Screen::Providers => {
                            st.provider_query.push(c);
                            st.sel = 0;
                        }
                        Screen::Models | Screen::SelectModel => {
                            st.model_query.push(c);
                            st.sel = 0;
                        }
                        _ => {
                            st.input_char(c);
                        }
                    }
                    st.error = None;
                }
            }
            KeyCode::Enter => match screen {
                Screen::Root => {
                    // Resolve the selected entry in the filtered list.
                    let action = self.settings.as_ref().and_then(|st| {
                        let items = st.root_items();
                        let sel = st.sel.min(items.len().saturating_sub(1));
                        items.get(sel).map(|(_, e)| e.action)
                    });
                    match action {
                        None => {}
                        Some(RootAction::NewSession) => {
                            self.settings = None;
                            self.new_session();
                        }
                        Some(RootAction::CloseSession) => {
                            self.settings = None;
                            self.close_session(self.active);
                        }
                        Some(RootAction::NextSession) => {
                            self.settings = None;
                            self.run_cmd(Cmd::NextSession);
                        }
                        Some(RootAction::PrevSession) => {
                            self.settings = None;
                            self.run_cmd(Cmd::PrevSession);
                        }
                        Some(RootAction::TogglePanel) => {
                            self.settings = None;
                            self.run_cmd(Cmd::TogglePanel);
                        }
                        Some(RootAction::ToggleAndroid) => {
                            self.settings = None;
                            self.run_cmd(Cmd::ToggleAndroid);
                        }
                        Some(RootAction::ToggleTerminal) => {
                            self.settings = None;
                            self.run_cmd(Cmd::ToggleTerminal);
                        }
                        Some(RootAction::WidenChat) => {
                            self.settings = None;
                            self.run_cmd(Cmd::WidenChat);
                        }
                        Some(RootAction::NarrowChat) => {
                            self.settings = None;
                            self.run_cmd(Cmd::NarrowChat);
                        }
                        Some(RootAction::ToggleMode) => {
                            self.settings = None;
                            self.run_cmd(Cmd::ToggleMode);
                        }
                        Some(RootAction::ClearSession) => {
                            self.settings = None;
                            self.run_cmd(Cmd::ClearSession);
                        }
                        Some(RootAction::Quit) => {
                            self.quit = true;
                        }
                        Some(RootAction::AddKnownProvider) => {
                            if let Some(st) = self.settings.as_mut() {
                                st.screen = Screen::Providers;
                                st.sel = 0;
                                st.query.clear();
                                st.provider_query.clear();
                            }
                        }
                        Some(RootAction::AddCustomProvider) => {
                            if let Some(st) = self.settings.as_mut() {
                                st.begin_custom();
                            }
                        }
                        Some(RootAction::SelectModel) => {
                            self.open_select_models();
                        }
                        Some(RootAction::OpenConfigFile) => {
                            // the config lives in the brain; point at it
                            self.toast(format!(
                                "Config lives on the server: ~/.config/romacode/config.json (server cwd: {})",
                                self.cfg.cwd
                            ));
                        }
                        Some(RootAction::PrintEffectiveConfig) => {
                            self.toast(format!(
                                "Provider: {} · Model: {} · engine: {}",
                                self.cfg.provider,
                                self.cfg.effective_model(),
                                self.cfg.engine
                            ));
                        }
                        Some(RootAction::OpenTools) => {
                            if let Some(st) = self.settings.as_mut() {
                                st.begin_tools();
                            }
                        }
                    }
                }
                Screen::Providers => {
                    let (is_custom, picked) = self.settings.as_ref().map_or((false, None), |st| {
                        (st.sel >= st.provider_items().len(), st.selected_provider())
                    });
                    if is_custom {
                        if let Some(st) = self.settings.as_mut() {
                            st.begin_custom();
                        }
                    } else if let Some(k) = picked {
                        if let Some(st) = self.settings.as_mut() {
                            st.begin_known(k);
                        }
                    }
                }
                Screen::CustomName => {
                    let name = self
                        .settings
                        .as_ref()
                        .map(|st| st.input.trim().to_string())
                        .unwrap_or_default();
                    if name.is_empty() {
                        if let Some(st) = self.settings.as_mut() {
                            st.error = Some("Provider name is required".into());
                        }
                    } else {
                        let id = crate::settings::Settings::custom_id_from_name(&name);
                        if let Some(st) = self.settings.as_mut() {
                            st.custom_name = name.clone();
                            st.provider_id = id;
                            st.provider_name = name;
                            st.screen = Screen::CustomBase;
                            st.input.clear();
                            st.cursor = 0;
                            st.error = None;
                        }
                    }
                }
                Screen::KnownKey => {
                    let (input, needs) = self
                        .settings
                        .as_ref()
                        .map(|st| (st.input.clone(), st.needs_key))
                        .unwrap_or_default();
                    if needs && input.trim().is_empty() {
                        if let Some(st) = self.settings.as_mut() {
                            st.error = Some("API key is required".into());
                        }
                    } else {
                        if let Some(st) = self.settings.as_mut() {
                            st.api_key = input;
                        }
                        self.start_model_fetch();
                    }
                }
                Screen::CustomBase => {
                    let input = self
                        .settings
                        .as_ref()
                        .map_or(String::new(), |st| st.input.clone());
                    let url = input.trim().trim_end_matches('/').to_string();
                    if !url.starts_with("http://") && !url.starts_with("https://") {
                        if let Some(st) = self.settings.as_mut() {
                            st.error = Some("Base URL must start with http:// or https://".into());
                        }
                    } else if let Some(st) = self.settings.as_mut() {
                        st.base_url = url;
                        st.input.clear();
                        st.screen = Screen::CustomKey;
                        st.error = None;
                    }
                }
                Screen::CustomKey => {
                    let input = self
                        .settings
                        .as_ref()
                        .map_or(String::new(), |st| st.input.clone());
                    if let Some(st) = self.settings.as_mut() {
                        st.api_key = input;
                        if st.provider_id.is_empty() {
                            st.provider_id = format!("custom-{}", self.cfg.providers.len() + 1);
                        }
                    }
                    self.start_model_fetch();
                }
                Screen::Models | Screen::SelectModel => {
                    // SelectModel lists models across many providers, so each
                    // row carries its own provider id. The Models screen is
                    // reached right after fetching models for ONE provider, so
                    // the provider identity lives in the screen's own fields
                    // (set by begin_known / begin_custom) — fall back to those
                    // when model_providers is empty. Without this fallback the
                    // Enter-on-model on the Models screen did nothing.
                    let (model, provider) =
                        self.models_screen_selection().unwrap_or((None, None));
                    if let Some(model) = model {
                        // For the Models screen, the screen fields are already
                        // the right provider — keep them. For SelectModel, look
                        // up the row's provider so we save against the right
                        // one — config entry first, then the shared catalog
                        // (a known provider with no config entry yet must not
                        // fall back to a wrong protocol or base URL).
                        if let Some(provider) = provider {
                            if let Some(st) = self.settings.as_mut() {
                                if screen == Screen::SelectModel {
                                    use romacode_shared::catalog as cat;
                                    let info = self.cfg.providers.iter().find(|p| p.id == provider);
                                    let known = cat::find(&provider);
                                    st.provider_id = provider.clone();
                                    st.provider_name = info
                                        .map(|p| p.name.clone())
                                        .filter(|n| !n.is_empty())
                                        .or_else(|| known.map(|k| k.name.to_string()))
                                        .unwrap_or_else(|| provider.clone());
                                    st.base_url = info
                                        .map(|p| p.base_url.clone())
                                        .filter(|b| !b.is_empty())
                                        .or_else(|| known.map(|k| k.base_url.to_string()))
                                        .unwrap_or_default();
                                    st.kind = cat::ApiKind::from_str(
                                        info.map(|p| p.kind.as_str())
                                            .filter(|s| !s.is_empty())
                                            .or_else(|| known.map(|k| k.kind.as_str()))
                                            .unwrap_or("openai"),
                                    );
                                    // keys never round-trip through the screen;
                                    // the brain keeps whatever it stored
                                    st.api_key.clear();
                                }
                            }
                        }
                        self.save_settings_model(model);
                    }
                }
                Screen::Tools => {
                    // Open the JSON-input prompt for the selected tool.
                    let names: Vec<String> =
                        self.tools.list().iter().map(|t| t.name.clone()).collect();
                    let pick = self.settings.as_ref().and_then(|st| {
                        let filt = st.tools_filter(&names);
                        filt.get(st.tools_sel.min(filt.len().saturating_sub(1)))
                            .map(|&fi| names.get(fi).cloned().unwrap_or_default())
                    });
                    if let Some(name) = pick {
                        if let Some(st) = self.settings.as_mut() {
                            st.begin_tools_input(name);
                        }
                    }
                }
                Screen::ToolsInput => {
                    // Parse the typed JSON, run the tool, show the result.
                    let (name, raw) = self
                        .settings
                        .as_ref()
                        .map(|st| (st.tools_selected.clone().unwrap_or_default(), st.input.clone()))
                        .unwrap_or_default();
                    if !name.is_empty() {
                        match serde_json::from_str::<serde_json::Value>(&raw) {
                            Ok(v) => {
                                let res = self.tools.run(&name, &v);
                                if let Some(st) = self.settings.as_mut() {
                                    st.tools_result = Some(match res {
                                        Ok(out) => (
                                            true,
                                            serde_json::to_string_pretty(&out)
                                                .unwrap_or_else(|_| out.to_string()),
                                        ),
                                        Err(e) => (false, e),
                                    });
                                    st.error = None;
                                }
                            }
                            Err(e) => {
                                if let Some(st) = self.settings.as_mut() {
                                    st.error = Some(format!("Invalid JSON: {e}"));
                                    st.tools_result = None;
                                }
                            }
                        }
                    }
                }
            },
            _ => {}
        }
        self.dirty = true;
    }

    fn open_select_models(&mut self) {
        use romacode_shared::catalog;
        // Rows are (model, provider id), consumed by `models_screen_selection`.
        // Config entries first (a fetched list is the most accurate), then the
        // active provider's catalog suggestions, then the effective model —
        // so the picker is never empty on a fresh install (out-of-the-box
        // `provider = "anthropic"`, no `providers` map entry yet).
        let mut models: Vec<String> = Vec::new();
        let mut providers: Vec<String> = Vec::new();
        let mut push = |m: &str, pid: &str| {
            if m.is_empty() {
                return;
            }
            // Keep the same model id once PER PROVIDER. The old code deduped
            // on the id alone, so "gpt-5" under a second provider vanished —
            // the picker could not show where a model actually comes from.
            if models
                .iter()
                .zip(providers.iter())
                .any(|(x, p)| x == m && p == pid)
            {
                return;
            }
            models.push(m.to_string());
            providers.push(pid.to_string());
        };
        for p in &self.cfg.providers {
            for m in &p.models {
                push(m, &p.id);
            }
            push(p.default_model.as_deref().unwrap_or(""), &p.id);
        }
        let active = self.cfg.provider.trim().to_ascii_lowercase();
        if let Some(k) = catalog::find(&active) {
            for m in &k.models {
                push(m.as_str(), k.id.as_str());
            }
        }
        push(&self.cfg.effective_model(), &active);
        // THE fix: the screen must exist even when this was called from the
        // Settings page or the `/` menu (no settings overlay open yet). The
        // old code wrote the computed list into `self.settings.as_mut()` —
        // which was None on that path — and then replaced it with an empty
        // Settings, so "Select Model" showed nothing at all.
        let st = self.settings.get_or_insert_with(crate::settings::Settings::root);
        st.screen = crate::settings::Screen::SelectModel;
        st.sel = 0;
        st.model_query.clear();
        st.models = models;
        st.model_providers = providers;
        st.error = if st.models.is_empty() {
            Some("No configured models yet — open Settings › Providers to add one.".into())
        } else {
            None
        };
        self.dirty = true;
    }
    fn on_palette_key(&mut self, k: KeyEvent) {
        let Some(p) = self.palette.as_mut() else {
            return;
        };
        match k.code {
            KeyCode::Esc => self.palette = None,
            KeyCode::Up => p.sel = p.sel.saturating_sub(1),
            KeyCode::Down => {
                // Cap at the last filtered row: the draw pass re-clamps, but
                // an uncapped counter desynced the highlight from the value.
                let last = p.items().len().saturating_sub(1);
                p.sel = (p.sel + 1).min(last);
            }
            KeyCode::Backspace => {
                p.query.pop();
                p.sel = 0;
            }
            KeyCode::Char('c') | KeyCode::Char('p')
                if k.modifiers.contains(KeyModifiers::CONTROL) =>
            {
                self.palette = None
            }
            KeyCode::Char(c) if !k.modifiers.contains(KeyModifiers::CONTROL) => {
                p.query.push(c);
                p.sel = 0;
            }
            KeyCode::Enter => {
                let items = p.items();
                if let Some(&idx) = items.get(p.sel.min(items.len().saturating_sub(1))) {
                    self.run_action(Action::PaletteRun(idx));
                } else {
                    self.palette = None;
                }
            }
            _ => {}
        }
    }

    // ── flat Settings page (Ctrl+P) ────────────────────────────────────────
    fn on_tree_key(&mut self, k: KeyEvent) {
        if self.tree.is_none() {
            return;
        }
        match k.code {
            KeyCode::Esc => {
                let has_query = self.tree.as_ref().map(|t| !t.query.is_empty()).unwrap_or(false);
                if has_query {
                    if let Some(t) = self.tree.as_mut() {
                        t.query.clear();
                        t.sel = 0;
                        t.top = 0;
                    }
                } else {
                    self.tree = None;
                    self.tree_hits.clear();
                }
            }
            KeyCode::Up => {
                if let Some(t) = self.tree.as_mut() {
                    t.up();
                }
            }
            KeyCode::Down => {
                if let Some(t) = self.tree.as_mut() {
                    t.down();
                }
            }
            KeyCode::Left => self.tree_step(-1),
            KeyCode::Right | KeyCode::Enter => self.tree_activate(),
            KeyCode::Backspace => {
                if let Some(t) = self.tree.as_mut() {
                    t.query.pop();
                    t.sel = 0;
                    t.top = 0;
                }
            }
            KeyCode::Char('c') | KeyCode::Char('p') if k.modifiers.contains(KeyModifiers::CONTROL) => {
                self.tree = None;
                self.tree_hits.clear();
            }
            KeyCode::Char(c) if !k.modifiers.contains(KeyModifiers::CONTROL) => {
                if c.is_ascii() {
                    if let Some(t) = self.tree.as_mut() {
                        t.query.push(c);
                        t.sel = 0;
                        t.top = 0;
                    }
                }
            }
            _ => {}
        }
        self.dirty = true;
    }

    /// Enter / → on the selected row: a setting steps forward, a preset is
    /// pinned, a hand-off row runs its flow.
    fn tree_activate(&mut self) {
        self.tree_step(1);
    }

    /// One activation step on the flat Settings page. `dir` 1 = Enter/→
    /// (open / step forward / pin), -1 = ← (step a setting back only —
    /// ← never opens a flow or switches the permission preset by accident).
    fn tree_step(&mut self, dir: i32) {
        let leaf: Option<crate::tree::Leaf> = self
            .tree
            .as_ref()
            .and_then(|t| t.selected())
            .cloned();
        match leaf {
            Some(crate::tree::Leaf::Setting { path }) => self.step_tui(path, dir),
            Some(crate::tree::Leaf::Preset { path, value }) if dir > 0 => self.set_tui(path, value),
            Some(crate::tree::Leaf::Dest(dest)) if dir > 0 => self.tree_to_dest(dest),
            _ => {}
        }
    }

    /// Step a catalog setting one value and persist it as `tui.<path>`.
    fn step_tui(&mut self, path: &'static str, dir: i32) {
        use romacode_shared::tui_settings as ts;
        let Some(def) = ts::def(path) else {
            self.toast(format!("unknown setting {path}"));
            return;
        };
        let next = match self.cfg.tui.step(path, dir) {
            Ok(v) => v,
            Err(e) => {
                self.toast(e);
                return;
            }
        };
        let wire = match &next {
            serde_json::Value::String(sv) => sv.clone(),
            other => other.to_string(),
        };
        self.net.fire(ClientRequest::SetUiPref {
            key: format!("tui.{path}"),
            value: wire,
        });
        self.apply_tui(path);
        self.toast(format!("{}: {}", def.title, self.cfg.tui.display(path)));
        self.dirty = true;
    }

    /// Pin a catalog setting to one value and persist it.
    fn set_tui(&mut self, path: &'static str, value: &str) {
        use romacode_shared::tui_settings as ts;
        let Some(def) = ts::def(path) else {
            self.toast(format!("unknown setting {path}"));
            return;
        };
        let parsed = ts::parse_wire(def, value).and_then(|v| self.cfg.tui.set(path, &v));
        match parsed {
            Ok(()) => {
                self.net.fire(ClientRequest::SetUiPref {
                    key: format!("tui.{path}"),
                    value: value.to_string(),
                });
                self.apply_tui(path);
                self.toast(format!("{}: {}", def.title, self.cfg.tui.display(path)));
                self.dirty = true;
            }
            Err(e) => self.toast(e),
        }
    }

    /// Settings that change something in the screen half itself.
    fn apply_tui(&mut self, path: &str) {
        if path == "session.sidebar" {
            self.panel = self.cfg.tui.session.sidebar == "auto";
        }
    }

    /// Hand control from the Settings page to one of the flow screens. The
    /// page is closed first; the same-titled Settings panel takes over for
    /// provider/model/tools flows, or a toast is shown for info-only rows.
    fn tree_to_dest(&mut self, dest: crate::tree::Dest) {
        use crate::settings::{Screen, Settings};
        use crate::tree::Dest;
        match dest {
            // Select Model: open_select_models fills the screen with the
            // saved + catalog model list and CREATES the screen when no
            // settings overlay is open yet (the tree/slash path — the old
            // code wrote the list into a None Option and showed an empty
            // picker).
            Dest::SelectModel => {
                self.tree = None;
                self.open_select_models();
            }
            // The Providers screen: every catalog provider (known + the
            // custom form) reachable with one Enter from the Settings page.
            Dest::OpenProviders => {
                self.tree = None;
                let mut st = Settings::root();
                st.screen = Screen::Providers;
                st.sel = 0;
                st.error = None;
                self.settings = Some(st);
            }
            Dest::OpenTools => {
                self.tree = None;
                let mut st = Settings::root();
                st.begin_tools();
                self.settings = Some(st);
            }
            Dest::OpenConfigFile => {
                self.tree = None;
                self.toast(format!(
                    "Config lives on the server: ~/.config/romacode/config.json (server cwd: {})",
                    self.cfg.cwd
                ));
            }
            Dest::PrintEffectiveConfig => {
                self.tree = None;
                self.toast(format!(
                    "Provider: {} · Model: {} · engine: {}",
                    self.cfg.provider,
                    self.cfg.effective_model(),
                    self.cfg.engine
                ));
            }
            Dest::ResetSettings => {
                self.tree = None;
                self.toast("Reset Settings: confirm with /reset in chat (coming)");
            }
        }
    }

    // ── `/` slash menu (chat) ────────────────────────────────────────────────
    /// ctrl+o: open the details view for the most recent tool call of the
    /// active session (walk the transcript backwards).
    fn open_latest_tool(&mut self) {
        let s = &self.sessions[self.active];
        for (mi, m) in s.messages.iter().enumerate().rev() {
            if let crate::session::Msg::Assistant { blocks, .. } = m {
                for (bi, b) in blocks.iter().enumerate().rev() {
                    if matches!(b, crate::session::Block::Tool { .. }) {
                        self.tool_detail = Some((mi, bi));
                        self.tool_scroll = 0;
                        self.dirty = true;
                        return;
                    }
                }
            }
        }
        self.toast("No tool calls in this session yet");
    }

    fn on_slash_key(&mut self, k: KeyEvent) {
        let Some(m) = self.slash.as_mut() else { return; };
        match k.code {
            KeyCode::Esc => {
                self.slash = None;
                self.tree_hits.clear();
            }
            KeyCode::Up => m.up(),
            KeyCode::Down => m.down(),
            KeyCode::Backspace => {
                if m.query.is_empty() {
                    self.slash = None;
                    self.tree_hits.clear();
                } else {
                    m.query.pop();
                    m.refresh();
                }
            }
            KeyCode::Enter => {
                let leaf = m.selected().map(|f| (f.path.clone(), f.leaf.clone()));
                if let Some((path, leaf)) = leaf {
                    self.slash = None;
                    self.tree_hits.clear();
                    let _ = path;
                    match leaf {
                        crate::tree::Leaf::Dest(dest) => self.tree_to_dest(dest),
                        crate::tree::Leaf::Preset { path: p, value } => self.set_tui(p, value),
                        crate::tree::Leaf::Setting { path: p } => self.step_tui(p, 1),
                    }
                }
            }
            KeyCode::Char('c') if k.modifiers.contains(KeyModifiers::CONTROL) => {
                self.slash = None;
                self.tree_hits.clear();
            }
            KeyCode::Char(c) if !k.modifiers.contains(KeyModifiers::CONTROL) => {
                m.query.push(c);
                m.refresh();
            }
            _ => {}
        }
        self.dirty = true;
    }

    fn hit_at(&self, x: u16, y: u16) -> Option<&Hit> {
        self.hits
            .iter()
            .rev()
            .find(|h| x >= h.rect.x && x < h.rect.right() && y >= h.rect.y && y < h.rect.bottom())
    }

    /// Mouse dispatch while the settings tree overlay is open. Same pattern as
    /// `on_settings_mouse`: hover moves the selection, left click selects +
    /// activates (replays Enter), scroll moves up/down.
    fn on_tree_mouse(&mut self, m: MouseEvent) {
        let (x, y) = (m.column, m.row);
        let hit = self.tree_hits.iter().find(|h| h.contains(x, y)).copied();
        match m.kind {
            MouseEventKind::Moved => {
                if let Some(h) = hit {
                    if let crate::tree::TreeHitKind::TreeRow(i) = h.kind {
                        if let Some(t) = self.tree.as_mut() {
                            t.sel = i;
                        }
                    }
                }
            }
            MouseEventKind::Down(MouseButton::Left) => match hit.map(|h| h.kind) {
                Some(crate::tree::TreeHitKind::TreeRow(i)) => {
                    if let Some(t) = self.tree.as_mut() {
                        t.sel = i;
                    }
                    self.tree_activate();
                }
                _ => {}
            },
            MouseEventKind::ScrollUp => {
                if let Some(t) = self.tree.as_mut() {
                    t.up();
                }
            }
            MouseEventKind::ScrollDown => {
                if let Some(t) = self.tree.as_mut() {
                    t.down();
                }
            }
            _ => {}
        }
        self.dirty = true;
    }

    /// Mouse dispatch while the `/` slash menu is open. Hover selects the row
    /// under the cursor; left click selects + runs it (replays Enter).
    fn on_slash_mouse(&mut self, m: MouseEvent) {
        let (x, y) = (m.column, m.row);
        let hit = self.tree_hits.iter().find(|h| h.contains(x, y)).copied();
        match m.kind {
            MouseEventKind::Moved => {
                if let Some(h) = hit {
                    if let crate::tree::TreeHitKind::SlashRow(i) = h.kind {
                        if let Some(m2) = self.slash.as_mut() {
                            m2.sel = i;
                        }
                    }
                }
            }
            MouseEventKind::Down(MouseButton::Left) => {
                if let Some(h) = hit {
                    if let crate::tree::TreeHitKind::SlashRow(i) = h.kind {
                        if let Some(m2) = self.slash.as_mut() {
                            m2.sel = i;
                        }
                        self.on_slash_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::empty()));
                    }
                }
            }
            MouseEventKind::ScrollUp => {
                if let Some(m2) = self.slash.as_mut() {
                    m2.up();
                }
            }
            MouseEventKind::ScrollDown => {
                if let Some(m2) = self.slash.as_mut() {
                    m2.down();
                }
            }
            _ => {}
        }
        self.dirty = true;
    }

    /// Mouse dispatch while the settings overlay is open. Hover follows the
    /// cursor (moves the selection to the row under the pointer); a left click
    /// selects the row and then activates it by replaying an `Enter` keypress
    /// through [`on_settings_key`] (so click and keyboard do exactly the same
    /// thing); scroll moves the selection up/down.
    fn on_settings_mouse(&mut self, m: MouseEvent) {
        let (x, y) = (m.column, m.row);
        let hit = self.settings_hits.iter().find(|h| h.contains(x, y)).copied();
        match m.kind {
            MouseEventKind::Moved => {
                if let Some(h) = hit {
                    self.set_settings_sel_from_hit(h);
                }
            }
            MouseEventKind::Down(MouseButton::Left) => {
                if let Some(h) = hit {
                    self.set_settings_sel_from_hit(h);
                    // Replay Enter to activate the now-selected row.
                    self.on_settings_key(KeyEvent::new(
                        KeyCode::Enter,
                        KeyModifiers::empty(),
                    ));
                }
            }
            MouseEventKind::ScrollUp => self.settings_scroll(-1),
            MouseEventKind::ScrollDown => self.settings_scroll(1),
            _ => {}
        }
    }

    /// Map a hit-test row to the selection index of the current screen.
    fn set_settings_sel_from_hit(&mut self, h: crate::settings::HitRow) {
        use crate::settings::RowKind;
        if let Some(st) = self.settings.as_mut() {
            match h.kind {
                RowKind::RootItem(vis) => st.sel = vis,
                RowKind::Provider(idx) | RowKind::Model(idx) => st.sel = idx,
                RowKind::Tool(vis) => st.tools_sel = vis,
            }
        }
    }

    /// Scroll the settings list by `dir` rows (−1 up, +1 down).
    /// Selection is clamped on BOTH ends for every screen — the old
    /// `usize::MAX` cap let the wheel push the counter far past the list, so
    /// the next Up arrow appeared to jump to the end.
    fn settings_scroll(&mut self, dir: i32) {
        let tool_names: Vec<String> = self
            .tools
            .list()
            .iter()
            .map(|t| t.name.clone())
            .collect();
        if let Some(st) = self.settings.as_mut() {
            let last = st.visible_len(&tool_names).saturating_sub(1);
            if st.screen == crate::settings::Screen::Tools {
                st.tools_sel = if dir > 0 {
                    st.tools_sel.saturating_add(1).min(last)
                } else {
                    st.tools_sel.saturating_sub(1)
                };
            } else {
                st.sel = if dir > 0 {
                    st.sel.saturating_add(1).min(last)
                } else {
                    st.sel.saturating_sub(1).min(last)
                };
            }
        }
    }

    fn on_mouse(&mut self, m: MouseEvent) {
        // The permission popup is topmost and modal: while an approval is
        // pending, its Allow / Deny buttons take every click and nothing
        // leaks into the screen beneath (same contract as the overlays below).
        if !self.pending_approval.is_empty() {
            if matches!(m.kind, MouseEventKind::Down(MouseButton::Left)) {
                let hit = self
                    .perm_hits
                    .iter()
                    .find(|h| h.contains(m.column, m.row))
                    .map(|h| h.allow);
                if let Some(allow) = hit {
                    self.resolve_approval(allow);
                } else {
                    self.toast("Approval pending: press y to allow once or n to deny");
                }
            }
            return;
        }
        // The slash menu and the settings tree have full mouse support too:
        // click selects + activates, hover follows the cursor. Route every
        // event there while either is open so nothing leaks into the screen
        // beneath the darkened backdrop.
        if self.slash.is_some() {
            self.on_slash_mouse(m);
            return;
        }
        if self.tree.is_some() {
            self.on_tree_mouse(m);
            return;
        }
        if self.settings.is_some() {
            self.on_settings_mouse(m);
            return;
        }
        // Tool-details modal: wheel scrolls, click outside the box closes.
        if self.tool_detail.is_some() {
            match m.kind {
                MouseEventKind::ScrollUp => {
                    self.tool_scroll = self.tool_scroll.saturating_sub(3);
                }
                MouseEventKind::ScrollDown => {
                    let lines = super::ui::tool_detail::lines_for(self);
                    let page = self.transcript_rect.height.saturating_sub(4).max(1) as usize;
                    let max = lines.saturating_sub(page);
                    self.tool_scroll = (self.tool_scroll + 3).min(max);
                }
                MouseEventKind::Down(MouseButton::Left) => {
                    // A click anywhere outside the detail box closes it; the
                    // box occupies the centered modal area of the chat column.
                    self.tool_detail = None;
                }
                _ => {}
            }
            self.dirty = true;
            return;
        }
        if self.screen == Screen::Terminal {
            if let Some(terminal) = self.terminal.as_mut() {
                match m.kind {
                    MouseEventKind::ScrollUp => terminal.scroll_scrollback(3),
                    MouseEventKind::ScrollDown => terminal.scroll_scrollback(-3),
                    _ => {}
                }
            }
            return;
        }
        self.mouse = (m.column, m.row);
        let hit = self.hit_at(m.column, m.row).map(|h| h.action);
        match m.kind {
            MouseEventKind::Moved => self.hover = hit,
            MouseEventKind::Drag(_) => {
                if self.phone_press.is_none() && self.phone_rect.map_or(false, |r| self.inside(r)) {
                    self.phone_press = Some((m.column, m.row));
                }
            }
            MouseEventKind::Down(MouseButton::Left) => {
                if let Some(a) = hit {
                    self.run_action(a);
                    return;
                }
                if self.palette.is_some() {
                    self.palette = None;
                    return;
                }
                if self.phone_rect.map_or(false, |r| self.inside(r)) {
                    if !self.android_full {
                        // clicking the panel's live screen opens fullscreen
                        self.android_full = true;
                    } else {
                        // Tap vs swipe is decided on RELEASE (see Up below).
                        // The old fullscreen path tapped on press AND swiped
                        // on release, so a jittery drag fired tap-then-swipe.
                        self.phone_press = Some((m.column, m.row));
                    }
                }
            }
            MouseEventKind::Up(MouseButton::Left) => {
                if let Some((px, py)) = self.phone_press.take() {
                    let dx = m.column as i32 - px as i32;
                    let dy = m.row as i32 - py as i32;
                    if self.phone_rect.map_or(false, |r| self.inside(r)) {
                        if dx.abs() + dy.abs() > 2 {
                            self.phone_swipe(px, py, m.column, m.row);
                        } else {
                            // a plain tap on the device screen
                            self.phone_tap_at(px, py);
                        }
                    }
                }
            }
            MouseEventKind::ScrollUp => {
                if self.android_full {
                    // Frame-derived swipe so a 1080×2400 device scrolls the
                    // whole display. Falls back to the legacy 720×1560
                    // geometry before the first frame arrives.
                    let (w, h) = self
                        .android
                        .frame
                        .as_ref()
                        .map(|f| (f.w, f.h))
                        .unwrap_or((720, 1560));
                    let mid_x = w / 2;
                    let bottom_y = (h * 3) / 4;
                    let top_y = h / 4;
                    self.device_swipe(mid_x, bottom_y, mid_x, top_y);
                } else {
                    let n = self.wheel_rows();
                    self.scroll(n);
                }
            }
            MouseEventKind::ScrollDown => {
                if self.android_full {
                    let (w, h) = self
                        .android
                        .frame
                        .as_ref()
                        .map(|f| (f.w, f.h))
                        .unwrap_or((720, 1560));
                    let mid_x = w / 2;
                    let bottom_y = (h * 3) / 4;
                    let top_y = h / 4;
                    self.device_swipe(mid_x, top_y, mid_x, bottom_y);
                } else {
                    let n = self.wheel_rows();
                    self.scroll(-n);
                }
            }
            _ => {}
        }
    }

    /// Wheel distance in rows, from the Scroll speed setting (3 by default).
    fn wheel_rows(&self) -> i32 {
        (self.cfg.tui.scroll.speed.round() as i32).max(1)
    }

    fn inside(&self, r: Rect) -> bool {
        let (x, y) = self.mouse;
        x >= r.x && x < r.right() && y >= r.y && y < r.bottom()
    }

    fn phone_tap_at(&mut self, col: u16, row: u16) {
        if let (Some(r), Some(f)) = (&self.phone_rect, &self.android.frame.clone()) {
            let (dx, dy) = crate::ui::pixels::to_device(*r, &f, col, row);
            self.device_tap(dx, dy);
        }
    }

    fn phone_swipe(&mut self, c1: u16, r1: u16, c2: u16, r2: u16) {
        if let (Some(rect), Some(f)) = (&self.phone_rect, &self.android.frame.clone()) {
            let (x1, y1) = crate::ui::pixels::to_device(*rect, &f, c1, r1);
            let (x2, y2) = crate::ui::pixels::to_device(*rect, &f, c2, r2);
            self.device_swipe(x1, y1, x2, y2);
        }
    }

    // ── device input over the wire ──
    fn device_tap(&mut self, x: u32, y: u32) {
        self.net.fire(ClientRequest::Android {
            action: AndroidAction::Tap { x, y },
        });
    }
    fn device_swipe(&mut self, x1: u32, y1: u32, x2: u32, y2: u32) {
        self.net.fire(ClientRequest::Android {
            action: AndroidAction::Swipe { x1, y1, x2, y2 },
        });
    }
    fn device_text(&mut self, text: &str) {
        self.net.fire(ClientRequest::Android {
            action: AndroidAction::Text {
                text: text.to_string(),
            },
        });
    }
    fn device_key(&mut self, key: PhoneKey) {
        self.net.fire(ClientRequest::Android {
            action: AndroidAction::Key {
                key: key.as_str().to_string(),
            },
        });
    }
}

/// Shared request timeout for interactive TUI requests.
pub fn req_timeout() -> Duration {
    Duration::from_secs(15)
}

fn png_decode(b64: &str) -> Result<Vec<u8>, String> {
    use base64::Engine as _;
    let bytes = base64::engine::general_purpose::STANDARD
        .decode(b64)
        .map_err(|e| e.to_string())?;
    let mut decoder = png::Decoder::new(std::io::Cursor::new(bytes));
    decoder.set_transformations(png::Transformations::EXPAND);
    let mut reader = decoder.read_info().map_err(|e| e.to_string())?;
    let mut out = vec![0u8; reader.output_buffer_size()];
    let info = reader.next_frame(&mut out).map_err(|e| e.to_string())?;
    out.truncate(info.buffer_size());
    // expand to RGBA8 for the pixel renderer
    match info.color_type {
        png::ColorType::Rgba => Ok(out),
        png::ColorType::Rgb => {
            let mut rgba = Vec::with_capacity(out.len() / 3 * 4);
            for px in out.chunks_exact(3) {
                rgba.extend_from_slice(px);
                rgba.push(255);
            }
            Ok(rgba)
        }
        _ => Err("unsupported device frame color type".into()),
    }
}

impl App {
    /// Dev helper for snapshots: add sibling tabs so the tab bar looks real.
    pub fn new_session_for_snapshot(&mut self, with_tabs: bool) {
        if !with_tabs {
            return;
        }
        let first = self.active;
        for title in ["Fix flaky auth test", "Docs cleanup"] {
            let mut s = UiSession::new(self.sessions.iter().map(|s| s.id).max().unwrap_or(1) + 1);
            s.title = title.into();
            self.sessions.push(s);
        }
        self.active = first;
    }
}

impl Drop for App {
    fn drop(&mut self) {
        // tell the brain to stop streaming frames for this screen
        if self.streaming_wanted {
            self.net.fire(ClientRequest::Android {
                action: AndroidAction::Stream { on: false },
            });
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crossterm::event::{Event as CtEvent, KeyEventKind};

    /// Build an App over the in-process dummy server (same trick the
    /// `--snapshot` dev renderer uses). No brain binary needed.
    fn test_app() -> App {
        let welcome = romacode_shared::protocol::Welcome {
            server_version: "test".into(),
            protocol_version: romacode_shared::protocol::Welcome::PROTOCOL_VERSION,
            provider: "anthropic".into(),
            model: "model-name".into(),
            mode_default: Mode::Build,
            engine: "native".into(),
            chat_width_pct: 36,
            sidebar: true,
            cwd: "/tmp/romacode-test".into(),
            branch: None,
            providers: Vec::new(),
            sessions: Vec::new(),
            device: None,
            missing_keys: Vec::new(),
            context_window: 200_000,
            price_in_per_mtok: 3.0,
            price_out_per_mtok: 15.0,
            max_tool_rounds: 25,
            webfetch_enabled: true,
            rnmap_enabled: true,
            remix_enabled: true,
            request_timeout_secs: 120,
            tui: Default::default(),
        };
        let bind = crate::net::spawn_dummy_server(serde_json::to_string(&welcome).unwrap())
            .expect("dummy server");
        let net = crate::net::Net::connect(&bind).expect("connect");
        App::new(std::sync::Arc::new(net), &welcome, &bind)
    }

    fn key(code: KeyCode, mods: KeyModifiers) -> KeyEvent {
        KeyEvent {
            code,
            modifiers: mods,
            kind: KeyEventKind::Press,
            state: crossterm::event::KeyEventState::NONE,
        }
    }

    /// THE regression: pasting an API key while the settings API-key screen is
    /// focused used to land in the chat (query) input instead of the field.
    #[test]
    fn paste_goes_to_the_focused_settings_field_not_the_chat() {
        let mut app = test_app();

        // 1. open the custom-provider flow from the root panel
        app.settings = Some(crate::settings::Settings::root());
        let st = app.settings.as_mut().unwrap();
        st.begin_custom(); // CustomName — first enter the provider name
        app.on_event(CtEvent::Paste("My Gateway".into()));
        app.on_event(CtEvent::Key(key(KeyCode::Enter, KeyModifiers::NONE)));
        assert_eq!(
            app.settings.as_ref().unwrap().screen,
            crate::settings::Screen::CustomBase
        );
        app.on_event(CtEvent::Paste("https://api.together.xyz/v1".into()));
        let st = app.settings.as_ref().unwrap();
        assert_eq!(st.input, "https://api.together.xyz/v1");
        assert!(app.session().input.is_empty(), "paste leaked into chat input");

        // Enter advances to the API-key screen
        app.on_event(CtEvent::Key(key(KeyCode::Enter, KeyModifiers::NONE)));
        let st = app.settings.as_ref().unwrap();
        assert_eq!(st.screen, crate::settings::Screen::CustomKey);

        // 2. paste the key while the API-key screen is focused
        app.on_event(CtEvent::Paste("sk-test-1234567890\r\n".into()));
        let st = app.settings.as_ref().unwrap();
        assert_eq!(st.input, "sk-test-1234567890", "CR/LF must be stripped");
        assert!(app.session().input.is_empty(), "API key leaked into chat input");

        // 3. type + edit inside the field: backspace removes the last char
        app.on_event(CtEvent::Key(key(KeyCode::Char('X'), KeyModifiers::NONE)));
        app.on_event(CtEvent::Key(key(KeyCode::Backspace, KeyModifiers::NONE)));
        let st = app.settings.as_ref().unwrap();
        assert_eq!(st.input, "sk-test-1234567890");
        assert!(app.session().input.is_empty());
    }

    /// Enter on the Root list runs the selected entry (provider flow opens,
    /// nothing is sent to the chat).
    #[test]
    fn enter_on_root_list_opens_the_selected_flow() {
        let mut app = test_app();
        app.settings = Some(crate::settings::Settings::root());
        // filter down to the custom provider entry, then Enter
        for c in "add custom".chars() {
            app.on_event(CtEvent::Key(key(KeyCode::Char(c), KeyModifiers::NONE)));
        }
        app.on_event(CtEvent::Key(key(KeyCode::Enter, KeyModifiers::NONE)));
        let st = app.settings.as_ref().unwrap();
        assert_eq!(st.screen, crate::settings::Screen::CustomName);
        assert!(app.session().input.is_empty());
    }

    /// With no overlay open, paste still lands in the chat input (its normal
    /// home) — and Enter sends, Backspace edits.
    #[test]
    fn paste_and_keys_reach_chat_without_an_overlay() {
        let mut app = test_app();
        app.on_event(CtEvent::Paste("hello ".into()));
        app.on_event(CtEvent::Key(key(KeyCode::Char('w'), KeyModifiers::NONE)));
        assert_eq!(app.session().input.text(), "hello w");
        app.on_event(CtEvent::Key(key(KeyCode::Backspace, KeyModifiers::NONE)));
        assert_eq!(app.session().input.text(), "hello ");
    }

    /// Ctrl+M behaves as Enter and Ctrl+H as Backspace in the chat input
    /// (legacy control encodings some terminals send).
    #[test]
    fn ctrl_m_and_ctrl_h_are_enter_and_backspace() {
        let mut app = test_app();
        app.on_event(CtEvent::Key(key(KeyCode::Char('a'), KeyModifiers::NONE)));
        app.on_event(CtEvent::Key(key(
            KeyCode::Char('h'),
            KeyModifiers::CONTROL,
        )));
        assert!(app.session().input.is_empty());
        app.on_event(CtEvent::Key(key(KeyCode::Char('h'), KeyModifiers::CONTROL)));
        assert!(app.session().input.is_empty());
    }

    /// REGRESSION: on the Models screen (the one shown right after fetching
    /// models for a single provider), pressing Enter on a selected model used
    /// to do nothing — the handler required a per-row provider id from
    /// `model_providers`, which `begin_models` never populates. The fix falls
    /// back to the screen's own `provider_id`. This test asserts the fallback
    /// resolves a provider, so the Enter handler will reach `save_settings_model`.
    #[test]
    fn models_screen_enter_resolves_provider_from_screen_fields() {
        use crate::settings::Screen;
        use romacode_shared::catalog;

        let mut app = test_app();
        // open the Known-provider key screen, then jump straight to the Models
        // screen as `begin_models` would after a successful fetch.
        let known = catalog::known_at(0).expect("at least one known provider"); // anthropic
        app.settings = Some(crate::settings::Settings::root());
        let st = app.settings.as_mut().unwrap();
        st.begin_known(known);
        // simulate the user typing a key then the fetch landing models
        st.input = "sk-test".into();
        st.api_key = "sk-test".into();
        st.begin_models(vec!["claude-3-5-sonnet".into(), "claude-3-opus".into()]);
        assert_eq!(st.screen, Screen::Models);
        assert!(st.model_providers.is_empty(), "precondition: no per-row providers");

        // selection: first model, provider must come from st.provider_id
        let (model, provider) = app.models_screen_selection().expect("settings open");
        assert_eq!(model.as_deref(), Some("claude-3-5-sonnet"));
        assert_eq!(provider.as_deref(), Some("anthropic"),
            "fallback must resolve the provider set by begin_known; \
             without the fix this is None and Enter does nothing");

        // move the cursor to the second model and re-check
        let st = app.settings.as_mut().unwrap();
        st.sel = 1;
        let (model, provider) = app.models_screen_selection().expect("settings open");
        assert_eq!(model.as_deref(), Some("claude-3-opus"));
        assert_eq!(provider.as_deref(), Some("anthropic"));
    }

    /// THE approval regression: the brain broadcast an `ApprovalRequest`
    /// event, but the TUI only printed a transcript notice — the popup queue
    /// stayed empty, y/n went into the chat input and the agent waited
    /// forever. The event must queue the popup (deduped by request id) and
    /// y / n must answer it over the wire.
    #[test]
    fn approval_request_queues_popup_and_y_n_answers() {
        let mut app = test_app();
        let sid = app.session().id;
        // a turn is live on the session (the approval belongs to a running turn)
        app.session_mut().begin_turn("run it", "model");

        let ev = romacode_shared::events::Event::ApprovalRequest {
            session_id: sid,
            request_id: "apr-42".into(),
            name: "Bash".into(),
            effect: "Run host shell command in project directory:\ncargo test".into(),
        };
        assert!(app.apply_event(&ev), "approval event must trigger a redraw");
        assert_eq!(app.pending_approval.len(), 1, "popup must be queued");
        assert_eq!(app.pending_approval[0].id, "apr-42");
        assert_eq!(app.pending_approval[0].name, "Bash");
        assert!(app.session().messages.iter().any(|m| matches!(
            m,
            Msg::Notice { text } if text.contains("cargo test")
        )), "transcript must show the exact effect");

        // duplicate delivery (reconnect replay) must not double-queue
        assert!(app.apply_event(&ev));
        assert_eq!(app.pending_approval.len(), 1, "deduped by request id");

        // while the popup is up the keys are modal: "y" must NOT type into
        // the chat input — it allows the action
        app.on_event(CtEvent::Key(key(KeyCode::Char('y'), KeyModifiers::NONE)));
        assert!(app.pending_approval.is_empty(), "y resolves the prompt");
        assert!(app.session().input.is_empty(), "y must never land in the input");

        // a second request: "n" denies, esc denies, unknown keys toast
        let ev2 = romacode_shared::events::Event::ApprovalRequest {
            session_id: sid,
            request_id: "apr-43".into(),
            name: "Bash".into(),
            effect: "Run host shell command in project directory:\nls".into(),
        };
        app.apply_event(&ev2);
        app.on_event(CtEvent::Key(key(KeyCode::Char('n'), KeyModifiers::NONE)));
        assert!(app.pending_approval.is_empty());
    }

    /// Select Model must offer options on a fresh install: the catalog's
    /// suggestions for the active provider plus the effective model, even
    /// when the config has no `providers` entries at all. REGRESSION: the
    /// old code wrote the computed list into `self.settings.as_mut()` —
    /// None when opened from the Settings page or the `/` menu — and then
    /// replaced it with an empty Settings, so the picker showed nothing.
    /// The screen must be CREATED here, with the list already inside.
    #[test]
    fn select_model_lists_catalog_models_for_the_active_provider() {
        use crate::settings::Screen;
        let mut app = test_app(); // provider = "anthropic", providers = []
        assert!(app.settings.is_none(), "precondition: no settings overlay open");
        app.open_select_models();
        let st = app.settings.as_ref().expect("picker screen must be created");
        assert_eq!(st.screen, Screen::SelectModel);
        assert!(st.error.is_none(), "picker must not be empty: {:?}", st.error);
        assert!(
            st.models.contains(&"claude-sonnet-5-5".to_string()),
            "catalog suggestions must be listed: {:?}",
            st.models
        );
        assert_eq!(st.models.len(), st.model_providers.len());
        assert!(st
            .model_providers
            .iter()
            .all(|p| p == "anthropic"));
    }

    /// The Settings page's Models row (Dest::SelectModel) opens a POPULATED
    /// picker: the exact path from the user's screenshot that used to show
    /// an empty "Select Model" panel.
    #[test]
    fn settings_page_models_row_opens_a_populated_picker() {
        use crate::tree::{Dest, Tree};
        let mut app = test_app();
        app.tree = Some(Tree::root());
        app.tree_to_dest(Dest::SelectModel);
        assert!(app.tree.is_none(), "the page closes when the picker opens");
        let st = app.settings.as_ref().expect("picker must exist");
        assert!(st.screen == crate::settings::Screen::SelectModel);
        assert!(!st.models.is_empty(), "picker must list models, got none");
        assert_eq!(st.models.len(), st.model_providers.len());
    }

    /// The Providers row opens the provider picker (31 catalog providers),
    /// not a dead end.
    #[test]
    fn settings_page_providers_row_opens_the_provider_picker() {
        use crate::settings::Screen;
        use crate::tree::{Dest, Tree};
        let mut app = test_app();
        app.tree = Some(Tree::root());
        app.tree_to_dest(Dest::OpenProviders);
        let st = app.settings.as_ref().expect("providers screen must exist");
        assert_eq!(st.screen, Screen::Providers);
        assert_eq!(st.sel, 0);
    }

    /// A (re)connecting screen re-raises permission popups for approvals
    /// still parked on the brain (they arrive in the SessionSnapshot), and a
    /// snapshot without them clears stale ones for that session.
    #[test]
    fn snapshot_restores_pending_permission_popups() {
        use romacode_shared::protocol::{PendingApprovalInfo, SessionSnapshot};
        let mut app = test_app();
        let sid = app.session().id;
        app.session_mut().begin_turn("run it", "model");

        let snap = SessionSnapshot {
            id: sid,
            title: "t".into(),
            messages: vec![],
            mode: Mode::Build,
            running: true,
            tokens_in: 0,
            tokens_out: 0,
            turns: 1,
            todos: vec![],
            mcp: vec![],
            files: vec![],
            lsp: vec![],
            pending_approvals: vec![PendingApprovalInfo {
                request_id: "apr-99".into(),
                name: "Bash".into(),
                effect: "Run host shell command in project directory:\ncargo build".into(),
            }],
        };
        assert!(app.apply_event(&romacode_shared::events::Event::Snapshot { session: snap }));
        assert_eq!(app.pending_approval.len(), 1);
        assert_eq!(app.pending_approval[0].id, "apr-99");
        assert_eq!(app.pending_approval[0].name, "Bash");

        // the answered/stale case: a snapshot with an empty list clears them
        let snap = SessionSnapshot {
            id: sid,
            title: "t".into(),
            messages: vec![],
            mode: Mode::Build,
            running: true,
            tokens_in: 0,
            tokens_out: 0,
            turns: 1,
            todos: vec![],
            mcp: vec![],
            files: vec![],
            lsp: vec![],
            pending_approvals: vec![],
        };
        app.apply_event(&romacode_shared::events::Event::Snapshot { session: snap });
        assert!(app.pending_approval.is_empty());
    }
    #[test]
    fn settings_arrows_stop_at_both_ends() {
        use crate::settings::{Screen, Settings};
        let mut app = test_app();
        let mut st = Settings::root();
        st.screen = Screen::Providers;
        app.settings = Some(st);

        // Up at the top must stay at 0 (never jump to the bottom).
        for _ in 0..3 {
            app.on_settings_key(key(KeyCode::Up, KeyModifiers::empty()));
        }
        assert_eq!(app.settings.as_ref().unwrap().sel, 0);

        // Down repeatedly must stop on the last filtered row.
        let last = app
            .settings
            .as_ref()
            .unwrap()
            .provider_total()
            .saturating_sub(1);
        for _ in 0..(last + 5) {
            app.on_settings_key(key(KeyCode::Down, KeyModifiers::empty()));
        }
        assert_eq!(app.settings.as_ref().unwrap().sel, last);
    }

    #[test]
    fn provider_search_pick_resolves_the_filtered_row() {
        use crate::settings::{Screen, Settings};
        let mut app = test_app();
        let mut st = Settings::root();
        st.screen = Screen::Providers;
        for c in "openrouter".chars() {
            st.provider_query.push(c);
        }
        app.settings = Some(st);
        app.on_settings_key(key(KeyCode::Enter, KeyModifiers::empty()));
        let st = app.settings.as_ref().expect("settings still open");
        assert_eq!(st.screen, Screen::KnownKey);
        assert_eq!(st.provider_id, "openrouter");
    }


    fn type_text(app: &mut App, text: &str) {
        for c in text.chars() {
            app.on_event(CtEvent::Key(key(KeyCode::Char(c), KeyModifiers::NONE)));
        }
    }

    /// While the agent works, enter parks the query in the queue box instead
    /// of refusing it; alt+e pulls it back to edit, alt+d drops it, and
    /// ctrl+s hands the text to the agent (the chatbox empties).
    #[test]
    fn queue_box_enter_queues_edit_delete_and_ctrl_s_sends() {
        let mut app = test_app();
        app.screen = Screen::Chat;
        app.session_mut().started = Some(Instant::now());

        type_text(&mut app, "fix the login redirect");
        app.on_event(CtEvent::Key(key(KeyCode::Enter, KeyModifiers::NONE)));
        assert_eq!(app.session().queue, vec!["fix the login redirect".to_string()]);
        assert!(app.session().input.is_empty());

        // edit: the query returns to the chatbox, the queue empties
        app.on_event(CtEvent::Key(key(KeyCode::Char('e'), KeyModifiers::ALT)));
        assert!(app.session().queue.is_empty());
        assert_eq!(app.session().input.text(), "fix the login redirect");

        // queue again, then delete it
        app.on_event(CtEvent::Key(key(KeyCode::Enter, KeyModifiers::NONE)));
        assert_eq!(app.session().queue.len(), 1);
        app.on_event(CtEvent::Key(key(KeyCode::Char('d'), KeyModifiers::ALT)));
        assert!(app.session().queue.is_empty());

        // ctrl+s with text typed: it leaves the chatbox for the agent and the
        // task keeps running (no turn restart on the client)
        type_text(&mut app, "and rename the helper");
        app.on_event(CtEvent::Key(key(KeyCode::Char('s'), KeyModifiers::CONTROL)));
        assert!(app.session().input.is_empty());
        assert!(app.session().queue.is_empty());
        assert!(app.session().is_running());

        // ctrl+s with an empty chatbox sends the SELECTED queued query
        type_text(&mut app, "first");
        app.on_event(CtEvent::Key(key(KeyCode::Enter, KeyModifiers::NONE)));
        type_text(&mut app, "second");
        app.on_event(CtEvent::Key(key(KeyCode::Enter, KeyModifiers::NONE)));
        assert_eq!(app.session().queue, vec!["first".to_string(), "second".to_string()]);
        app.on_event(CtEvent::Key(key(KeyCode::Up, KeyModifiers::ALT)));
        assert_eq!(app.session().queue_sel, 0);
        app.on_event(CtEvent::Key(key(KeyCode::Char('s'), KeyModifiers::CONTROL)));
        assert_eq!(app.session().queue, vec!["second".to_string()]);
    }

    /// The chat screen draws the queue box (rule, rows, ➤ ✎ ✕ icons wired to
    /// click actions) and the small working box while the agent runs.
    #[test]
    fn chat_draws_queue_box_icons_and_working_box() {
        use ratatui::{buffer::Buffer, layout::Rect};
        let mut app = test_app();
        app.screen = Screen::Chat;
        app.session_mut().begin_turn("do the thing", "m");
        app.session_mut().queue = vec!["first queued".into(), "second\nqueued".into()];
        let area = Rect::new(0, 0, 100, 30);
        let mut buf = Buffer::empty(area);
        app.hits.clear();
        crate::ui::chat::draw(&mut app, &mut buf, area);

        let mut text = String::new();
        for y in 0..area.height {
            for x in 0..area.width {
                text.push_str(buf.get(x, y).symbol());
            }
            text.push('\n');
        }
        assert!(text.contains("queued · 2"), "queue header missing:\n{text}");
        assert!(text.contains("first queued"));
        assert!(text.contains("second ⏎ queued"), "newlines must flatten to one row");
        for icon in ["➤", "✎", "✕"] {
            assert!(text.contains(icon), "icon {icon} missing");
        }
        assert!(text.contains("thinking"), "live state label missing");
        assert!(text.contains("▄"), "waterfall crest missing");
        for want in [Action::QueueSend(0), Action::QueueEdit(1), Action::QueueDelete(1)] {
            assert!(
                app.hits.iter().any(|h| h.action == want),
                "no click target for {want:?}"
            );
        }
    }

    /// A 12-row terminal (the minimum) used to underflow the settings panel
    /// geometry; the slash menu and toast spill past the buffer there too.
    #[test]
    fn overlays_survive_the_smallest_terminal() {
        use ratatui::{buffer::Buffer, layout::Rect};
        let mut app = test_app();
        app.settings = Some(crate::settings::Settings::root());
        let area = Rect::new(0, 0, 40, 12);
        let mut buf = Buffer::empty(area);
        crate::ui::palette::draw_settings(&mut app, &mut buf, area);
        app.settings = None;
        app.toast("a toast message that is far wider than a forty column terminal");
        crate::ui::palette::draw_toast(&app, &mut buf, area);
        app.slash = Some(crate::tree::SlashMenu::open());
        crate::ui::palette::draw_slash(&mut app, &mut buf, area);
    }
}
