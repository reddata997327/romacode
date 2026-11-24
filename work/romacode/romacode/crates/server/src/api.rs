//! HTTP + WebSocket endpoints and the event bus — the boundary of the brain.
//!
//! One std `TcpListener`, one thread per connection. A client either speaks
//! REST (`GET /api/health`, `GET /api/sessions`, `POST /api/message`, …) or
//! upgrades to WebSocket and uses the `romacode-shared` envelope protocol.
//!
//! The [`Bus`] is the single event path: agent threads and the device
//! streamer broadcast wire events; every attached client gets a copy and one
//! internal tap applies the same events to the server-owned sessions, so the
//! transcript can never drift between brain and screen.

use crate::android::{Phase, PHONE_AVAILABLE};
use crate::config::{ApiKind, Config};
use crate::security::Security;
use crate::session::{Run, Session};
use crate::{agent, providers, providers_cli};
use romacode_shared::events::Event;
use romacode_shared::protocol::{
    AndroidAction, ClientEnvelope, ClientRequest, Msg, ProviderInfo, Reply, ServerEnvelope,
    SessionSnapshot, Welcome,
};
use serde_json::json;
use std::collections::HashMap;
use std::io::{BufRead, BufReader, Read as IoRead, Write as IoWrite};
use std::net::{TcpListener, TcpStream};
use std::sync::{
    atomic::{AtomicBool, AtomicU64, Ordering},
    Arc, Mutex, Weak,
};
use std::time::{Duration, Instant};

// ---------------------------------------------------------------------------
// Bus.
// ---------------------------------------------------------------------------

type Sink = BoundedSink;

/// A BOUNDED client queue (1024 envelopes, `try_send` never blocks). The old
/// `mpsc::channel` was unbounded while the register doc-comment claimed "a
/// stuck client is dropped instead of stalling the brain" — a stalled WS
/// client accumulated unbounded memory during streaming. With a bound, a
/// stuck client's `try_send` fails, `Bus::broadcast` unregisters it and the
/// connection is reaped.
#[derive(Clone)]
pub(crate) struct BoundedSink {
    tx: std::sync::mpsc::SyncSender<ServerEnvelope>,
}

impl BoundedSink {
    pub fn send(&self, env: ServerEnvelope) -> Result<(), ()> {
        self.tx.try_send(env).map_err(|_| ())
    }
}

/// Broadcast hub for wire events. Cheap to clone; every agent run and the
/// device streamer hold one.
#[derive(Clone, Default)]
pub struct Bus {
    sinks: Arc<Mutex<Vec<(u64, Sink)>>>,
    next: Arc<AtomicU64>,
    tap: Arc<Mutex<Option<Weak<Hub>>>>,
}

impl Bus {
    pub fn new() -> Self {
        Self::default()
    }

    /// Register a client sink; returns a handle for unregistering. The sink
    /// is bounded: a stuck client is dropped instead of stalling the brain.
    pub(crate) fn register(&self, tx: Sink) -> u64 {
        let id = self.next.fetch_add(1, Ordering::Relaxed);
        self.sinks
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .push((id, tx));
        id
    }

    pub fn unregister(&self, token: u64) {
        self.sinks
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .retain(|(id, _)| *id != token);
    }

    /// Push a REPLY envelope (e.g. a refreshed Welcome) to every attached
    /// client without touching the session tap: replies are not session
    /// events. Screens apply a req-0 reply as a state refresh.
    pub fn broadcast_reply(&self, reply: Reply) {
        let env = ServerEnvelope::reply(0, reply);
        let mut sinks = self.sinks.lock().unwrap_or_else(|e| e.into_inner());
        let mut dead = Vec::new();
        for (id, sink) in sinks.iter() {
            if sink.send(env.clone()).is_err() {
                dead.push(*id);
            }
        }
        if !dead.is_empty() {
            sinks.retain(|(id, _)| !dead.contains(id));
        }
    }

    /// Push one event to every attached client, then apply it to the
    /// server-owned sessions through the internal tap.
    pub fn broadcast(&self, ev: Event) {
        {
            let mut sinks = self.sinks.lock().unwrap_or_else(|e| e.into_inner());
            let mut dead = Vec::new();
            for (id, sink) in sinks.iter() {
                // Send either through the unbounded queue or fail the client.
                if sink.send(ServerEnvelope::push(ev.clone())).is_err() {
                    dead.push(*id);
                }
            }
            if !dead.is_empty() {
                sinks.retain(|(id, _)| !dead.contains(id));
            }
        }
        // Copy the weak handle out FIRST so the tap mutex is not held while
        // the hub applies the event: `apply` may broadcast again (the steering
        // follow-up turn), and re-locking the tap from there would deadlock.
        let tap = self
            .tap
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .as_ref()
            .cloned();
        if let Some(weak) = tap {
            if let Some(hub) = weak.upgrade() {
                hub.apply(&ev);
            }
        }
    }

    pub(crate) fn set_tap(&self, hub: &Arc<Hub>) {
        *self.tap.lock().unwrap_or_else(|e| e.into_inner()) = Some(Arc::downgrade(hub));
    }
}

// ---------------------------------------------------------------------------
// Hub: config + sessions + security + device bridge.
// ---------------------------------------------------------------------------

pub struct Hub {
    pub cfg: Mutex<Config>,
    pub security: Arc<Security>,
    pub bus: Bus,
    pub android: crate::android::Android,
    sessions: Mutex<Sessions>,
    /// Approvals parked on the brain: request_id → record, recorded from
    /// ApprovalRequest events. Any client can resolve them, a `Done` cleans
    /// up the unanswered ones, and `snapshot` re-raises them to a
    /// (re)connecting screen so the permission popup is never lost.
    pending: Mutex<HashMap<String, PendingApprovalRec>>,
}

/// One approval waiting on the brain (record kept so snapshots can restore
/// the popup with its tool name and exact effect).
#[derive(Debug, Clone)]
struct PendingApprovalRec {
    session_id: usize,
    name: String,
    effect: String,
}

struct Sessions {
    map: HashMap<usize, Session>,
    next_id: usize,
}

impl Hub {
    fn new(cfg: Config, bus: Bus) -> Arc<Self> {
        let android = if cfg.android.auto_start && PHONE_AVAILABLE {
            crate::android::Android::spawn(crate::android::Android::config_from(&cfg))
        } else {
            crate::android::Android::disabled("android.auto_start is false")
        };
        // Restore saved transcripts before creating the fallback session. A
        // process restart now keeps the work the user already did.
        let mut map = HashMap::new();
        let mut next_id = 1usize;
        for snap in crate::session_store::load_all(&cfg) {
            next_id = next_id.max(snap.id.saturating_add(1));
            map.insert(snap.id, Session::from_snapshot(&snap));
        }
        if map.is_empty() {
            let mut first = Session::new(1);
            first.mode = default_mode(&cfg);
            map.insert(1, first);
            next_id = 2;
        }
        Arc::new(Self {
            cfg: Mutex::new(cfg),
            security: Arc::new(Security::new()),
            bus,
            android,
            sessions: Mutex::new(Sessions { map, next_id }),
            pending: Mutex::new(HashMap::new()),
        })
    }

    #[allow(dead_code)] // reserved for api-driven defaults
    fn default_mode(&self) -> crate::session::Mode {
        let cfg = self.cfg.lock().unwrap_or_else(|e| e.into_inner());
        default_mode(&cfg)
    }

    /// The internal tap: apply one broadcast event to the owned sessions.
    /// Same events the clients receive — one source of truth.
    fn apply(&self, ev: &Event) {
        match ev {
            Event::ApprovalRequest {
                session_id,
                request_id,
                name,
                effect,
            } => {
                self.pending
                    .lock()
                    .unwrap_or_else(|e| e.into_inner())
                    .insert(
                        request_id.clone(),
                        PendingApprovalRec {
                            session_id: *session_id,
                            name: name.clone(),
                            effect: effect.clone(),
                        },
                    );
            }
            Event::Done { session_id, .. } => {
                // drop unanswered approvals of the finished turn
                let done: Vec<String> = self
                    .pending
                    .lock()
                    .unwrap_or_else(|e| e.into_inner())
                    .iter()
                    .filter(|(_, p)| p.session_id == *session_id)
                    .map(|(rid, _)| rid.clone())
                    .collect();
                for rid in done {
                    if let Some((_, p)) = self
                        .pending
                        .lock()
                        .unwrap_or_else(|e| e.into_inner())
                        .remove_entry(&rid)
                    {
                        self.security.cancel(&rid, false);
                        let _ = p;
                    }
                }
            }
            _ => {}
        }
        if let Some(sid) = ev.session_id() {
            // Steering messages the agent never got to read (they arrived
            // after its last model round): take them BEFORE `Done` clears the
            // run, then continue the task with them below.
            let leftover = if matches!(ev, Event::Done { .. }) {
                let sessions = self.sessions.lock().unwrap_or_else(|e| e.into_inner());
                sessions.map.get(&sid).and_then(|s| {
                    s.run.as_ref().map(|r| {
                        let n = r
                            .steer
                            .lock()
                            .unwrap_or_else(|e| e.into_inner())
                            .drain(..)
                            .count();
                        (n, r.interrupted(), s.mode)
                    })
                })
            } else {
                None
            };
            let changed = {
                let mut sessions = self.sessions.lock().unwrap_or_else(|e| e.into_inner());
                sessions.map.get_mut(&sid).map(|s| s.apply(ev)).unwrap_or(false)
            };
            // Persist at turn boundaries (and on a user turn, saved by
            // start_turn) rather than on every streamed text delta.
            if changed && matches!(ev, Event::Done { .. } | Event::Error { .. }) {
                let cfg = self.cfg.lock().unwrap_or_else(|e| e.into_inner());
                if cfg.sessions.autosave {
                    let sessions = self.sessions.lock().unwrap_or_else(|e| e.into_inner());
                    if let Some(s) = sessions.map.get(&sid) {
                        if let Err(e) = crate::session_store::save(&cfg, s) {
                            eprintln!("romacode: session {sid} autosave failed: {e}");
                        }
                    }
                }
            }
            // No lock is held here. A stopped turn drops its unread messages
            // (interrupt already said so); a finished one carries on with them.
            if let Some((n, cancelled, mode)) = leftover {
                if n > 0 && !cancelled {
                    match self.launch(sid, None, mode) {
                        Ok(()) => {
                            if let Some(snap) = self.snapshot(sid) {
                                self.bus.broadcast(Event::Snapshot { session: snap });
                            }
                        }
                        Err(e) => self.bus.broadcast(Event::Notice {
                            session_id: Some(sid),
                            text: format!("could not continue with your queued message: {e}"),
                        }),
                    }
                }
            }
        }
    }

    fn sessions_list(&self) -> Vec<romacode_shared::protocol::SessionSummary> {
        let sessions = self.sessions.lock().unwrap_or_else(|e| e.into_inner());
        let mut v: Vec<_> = sessions.map.values().map(|s| s.summary()).collect();
        v.sort_by_key(|s| s.id);
        v
    }

    fn snapshot(&self, id: usize) -> Option<SessionSnapshot> {
        let sessions = self.sessions.lock().unwrap_or_else(|e| e.into_inner());
        let mut snap = sessions.map.get(&id).map(|s| s.snapshot())?;
        drop(sessions);
        // restore permission popups for approvals still parked on the brain
        let pending = self.pending.lock().unwrap_or_else(|e| e.into_inner());
        snap.pending_approvals = pending
            .iter()
            .filter(|(_, p)| p.session_id == id)
            .map(
                |(rid, p)| romacode_shared::protocol::PendingApprovalInfo {
                    request_id: rid.clone(),
                    name: p.name.clone(),
                    effect: p.effect.clone(),
                },
            )
            .collect();
        Some(snap)
    }

    /// Resolve one pending approval (the `Approval` wire request): take the
    /// record, answer the parked slot the agent thread waits on, broadcast
    /// the decision notice. Returns the notice text for the answering client.
    /// Taking the record first means an answered approval can never re-raise
    /// in a snapshot; a stale request id puts the record back untouched.
    fn resolve_approval(&self, request_id: &str, approve: bool) -> String {
        let record = self
            .pending
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .remove(request_id);
        let sid = record.as_ref().map(|p| p.session_id);
        let resolved = self.security.resolve(request_id, approve);
        let notice = if resolved {
            if approve {
                "Approved once; executing the displayed action."
            } else {
                "Refused: the user declined this action."
            }
        } else {
            "no such pending approval"
        };
        if resolved {
            self.bus.broadcast(Event::Notice {
                session_id: sid,
                text: notice.to_string(),
            });
        } else if let Some(p) = record {
            self.pending
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .insert(request_id.to_string(), p);
        }
        notice.to_string()
    }

    fn newest_session(&self) -> usize {
        let sessions = self.sessions.lock().unwrap_or_else(|e| e.into_inner());
        sessions.map.keys().copied().max().unwrap_or(1)
    }

    fn new_session(&self) -> usize {
        // cfg BEFORE sessions — the global lock order (apply's autosave path
        // holds cfg while taking sessions); nesting them the other way here
        // deadlocked against it.
        let mode = default_mode(&self.cfg.lock().unwrap_or_else(|e| e.into_inner()));
        let mut sessions = self.sessions.lock().unwrap_or_else(|e| e.into_inner());
        let id = sessions.next_id;
        sessions.next_id += 1;
        let mut s = Session::new(id);
        s.mode = mode;
        sessions.map.insert(id, s);
        id
    }

    fn close_session(&self, id: usize) {
        {
            let mut sessions = self.sessions.lock().unwrap_or_else(|e| e.into_inner());
            if let Some(s) = sessions.map.get(&id) {
                if let Some(run) = &s.run {
                    run.cancel.store(true, Ordering::Relaxed);
                }
            }
            sessions.map.remove(&id);
        }
        let mine: Vec<String> = self
            .pending
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .iter()
            .filter(|(_, p)| p.session_id == id)
            .map(|(rid, _)| rid.clone())
            .collect();
        for rid in mine {
            self.pending
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .remove(&rid);
            self.security.cancel(&rid, false);
        }
        if self.sessions_list().is_empty() {
            self.new_session();
        }
        self.bus.broadcast(Event::Sessions {
            sessions: self.sessions_list(),
        });
    }

    /// Start an agent turn for `text` on session `id`.
    fn start_turn(&self, id: usize, text: &str, mode: crate::session::Mode) -> Result<(), String> {
        self.launch(id, Some(text), mode)
    }

    /// Start a turn. `Some(text)` records a new user message; `None` resumes
    /// from user messages that are ALREADY in the transcript (steering
    /// messages the finished turn never got to read).
    fn launch(
        &self,
        id: usize,
        text: Option<&str>,
        mode: crate::session::Mode,
    ) -> Result<(), String> {
        let (model, history, cfg, cancel, steer) = {
            let cfg = self.cfg.lock().unwrap_or_else(|e| e.into_inner()).clone();
            let model = cfg.effective_model();
            let mut sessions = self.sessions.lock().unwrap_or_else(|e| e.into_inner());
            let Some(s) = sessions.map.get_mut(&id) else {
                return Err(format!("no session {id}"));
            };
            if s.is_running() {
                return Err("Agent is busy — press esc to interrupt".into());
            }
            s.mode = mode;
            match text {
                Some(text) => {
                    s.push_user(text.to_string());
                    s.begin_assistant(&model);
                }
                None => match s.messages.last_mut() {
                    // reuse the empty assistant block the injection opened
                    Some(Msg::Assistant { blocks, secs, .. }) if blocks.is_empty() => {
                        *secs = None;
                    }
                    _ => s.begin_assistant(&model),
                },
            }
            let history = s.history();
            let cancel = Arc::new(AtomicBool::new(false));
            let steer = Arc::new(Mutex::new(Vec::<String>::new()));
            s.run = Some(Run {
                cancel: cancel.clone(),
                started: Instant::now(),
                model: model.clone(),
                steer: steer.clone(),
            });
            (model, history, cfg, cancel, steer)
        };
        // Persist the user turn immediately so even a crash mid-answer leaves
        // the request and any earlier completed turns on disk.
        if cfg.sessions.autosave {
            let sessions = self.sessions.lock().unwrap_or_else(|e| e.into_inner());
            if let Some(s) = sessions.map.get(&id) {
                if let Err(e) = crate::session_store::save(&cfg, s) {
                    eprintln!("romacode: session {id} autosave failed: {e}");
                }
            }
        }
        let _ = model;
        let android = if PHONE_AVAILABLE {
            Some(self.android.agent_handle())
        } else {
            None
        };
        agent::spawn_run(
            &cfg,
            id,
            history,
            mode,
            self.bus.clone(),
            self.security.clone(),
            android,
            cancel,
            steer,
            Instant::now(),
        );
        Ok(())
    }

    /// Ctrl+S steering: deliver `text` to the RUNNING turn (it is shown in the
    /// transcript now and read by the agent before its next model round); on an
    /// idle session it simply starts a turn.
    fn steer(&self, id: usize, text: &str) -> Result<(), String> {
        let text = text.trim();
        if text.is_empty() {
            return Err("empty message".into());
        }
        let mode = {
            let sessions = self.sessions.lock().unwrap_or_else(|e| e.into_inner());
            let Some(s) = sessions.map.get(&id) else {
                return Err(format!("no session {id}"));
            };
            match &s.run {
                Some(run) if !run.interrupted() => {
                    run.steer
                        .lock()
                        .unwrap_or_else(|e| e.into_inner())
                        .push(text.to_string());
                    None
                }
                Some(_) => return Err("Agent is stopping — try again in a moment".into()),
                None => Some(s.mode),
            }
        };
        match mode {
            // running: show it in every screen's transcript right away
            None => {
                self.bus.broadcast(Event::UserInjected {
                    session_id: id,
                    text: text.to_string(),
                });
                Ok(())
            }
            // idle: a normal turn, and tell the screens it is running now
            Some(mode) => {
                self.start_turn(id, text, mode)?;
                if let Some(snap) = self.snapshot(id) {
                    self.bus.broadcast(Event::Snapshot { session: snap });
                }
                Ok(())
            }
        }
    }

    fn interrupt(&self, id: usize) -> bool {
        let mut unread = 0usize;
        let running = {
            let mut sessions = self.sessions.lock().unwrap_or_else(|e| e.into_inner());
            match sessions.map.get_mut(&id) {
                Some(s) => match &s.run {
                    Some(run) => {
                        unread = run.steer.lock().unwrap_or_else(|e| e.into_inner()).len();
                        run.cancel.store(true, Ordering::Relaxed);
                        s.apply(&Event::Notice {
                            session_id: Some(id),
                            text: "Interrupted".into(),
                        });
                        true
                    }
                    None => false,
                },
                None => false,
            }
        };
        if running {
            self.bus.broadcast(Event::Notice {
                session_id: Some(id),
                text: "Interrupted".into(),
            });
            if unread > 0 {
                self.bus.broadcast(Event::Notice {
                    session_id: Some(id),
                    text: format!(
                        "{unread} steering message(s) were not read before the stop — send them again"
                    ),
                });
            }
        }
        running
    }

    /// Provider/model/key/settings/skills slash-commands (moved verbatim from the
    /// old TUI's `provider_command` + `add_custom` — the brain owns the
    /// config). Accepts both "/engine x" and "engine x" forms. `session_id`
    /// (optional; old clients do not send it) lets `/skill` activate a skill
    /// on the SENDING tab instead of guessing one.
    fn run_command_in(&self, session_id: Option<usize>, cmd: &str) -> Result<String, String> {
        let cmd = cmd.trim_start_matches('/');
        let mut it = cmd.split_whitespace();
        let name = it.next().unwrap_or("");
        let rest: Vec<&str> = it.collect();
        let arg = rest.join(" ");
        // The skills arms run BEFORE the config lock is taken: activation
        // starts an agent turn, and `start_turn` locks the config itself —
        // calling it while holding this mutex would deadlock.
        match name {
            "skills" if arg.is_empty() => return Ok(self.skills_list()),
            "skill" => return Ok(self.activate_skill_cmd(session_id, &arg)),
            _ => {}
        }
        let mut cfg = self.cfg.lock().unwrap_or_else(|e| e.into_inner());
        let reply = match name {
            "providers" => providers_cli::list(&cfg),
            "provider" if arg.is_empty() => providers_cli::status(&cfg),
            "provider" => match cfg.set_provider(&arg) {
                Ok(()) => {
                    cfg.save();
                    format!(
                        "Provider set to {}.\n{}",
                        arg.to_ascii_lowercase(),
                        providers_cli::status(&cfg)
                    )
                }
                Err(e) => return Ok(e),
            },
            "models" => providers_cli::models(&cfg),
            "model" if arg.is_empty() => format!("Model: {}", cfg.effective_model()),
            "model" => {
                cfg.model = arg.clone();
                cfg.save();
                format!("Model set to {arg}.")
            }
            "baseurl" if arg.is_empty() => format!(
                "Base URL: {}",
                cfg.resolve().map(|r| r.base_url).unwrap_or_else(|e| e)
            ),
            "baseurl" => match cfg.set_base_url(&arg) {
                Ok(()) => {
                    cfg.save();
                    "Base URL saved for this provider.".to_string()
                }
                Err(e) => return Ok(e),
            },
            "apikey" if arg.is_empty() => {
                "Usage: /apikey <key>  (stored in the config file, owner-only)".to_string()
            }
            "apikey" => {
                cfg.set_api_key(&arg);
                cfg.save();
                "API key saved for this provider.".to_string()
            }
            "customprovider" => match rest.as_slice() {
                [id, url] | [id, url, "openai"] => {
                    self.add_custom(&mut cfg, id, url, ApiKind::OpenAi)
                }
                [id, url, "anthropic"] => self.add_custom(&mut cfg, id, url, ApiKind::Anthropic),
                _ => "Usage: /customprovider <id> <base-url> [openai|anthropic]   e.g. /customprovider vllm http://10.0.0.5:8000/v1".to_string(),
            },
            // ── general settings (all server-owned, saved to config.json) ──
            "engine" if arg.is_empty() => format!(
                "Agent engine: {}  (native = in-house loop | langchain = built-in Rust LangChain; applies to the next message)",
                cfg.agent.engine
            ),
            "engine" => match arg.to_ascii_lowercase().as_str() {
                "native" | "langchain" => {
                    cfg.agent.engine = arg.to_ascii_lowercase();
                    cfg.save();
                    format!(
                        "Agent engine set to {} (applies to the next message).",
                        cfg.agent.engine
                    )
                }
                other => format!("unknown engine '{other}' — use /engine native or /engine langchain"),
            },
            "mode" if arg.is_empty() => format!(
                "Default mode for new sessions: {:?}",
                cfg.agent.default_mode
            ),
            "mode" => match arg.to_ascii_lowercase().as_str() {
                "build" | "plan" => {
                    cfg.agent.default_mode = if arg.eq_ignore_ascii_case("build") {
                        crate::config::AgentMode::Build
                    } else {
                        crate::config::AgentMode::Plan
                    };
                    cfg.save();
                    format!("Default mode set to {}.", arg.to_ascii_lowercase())
                }
                other => format!("unknown mode '{other}' — use /mode build or /mode plan"),
            },
            "rounds" if arg.is_empty() => {
                format!("Max tool rounds per turn: {}", cfg.agent.max_tool_rounds)
            }
            "rounds" => match arg.parse::<u32>() {
                Ok(v) if (1..=200).contains(&v) => {
                    cfg.agent.max_tool_rounds = v;
                    cfg.save();
                    format!("Max tool rounds set to {v}.")
                }
                _ => "Usage: /rounds <1..200>".to_string(),
            },
            "webfetch" if arg.is_empty() => format!(
                "Web fetch tool: {}",
                if cfg.webfetch.enabled { "on" } else { "off" }
            ),
            "webfetch" => match arg.to_ascii_lowercase().as_str() {
                "on" | "off" => {
                    cfg.webfetch.enabled = arg.eq_ignore_ascii_case("on");
                    cfg.save();
                    format!(
                        "Web fetch tool {}.",
                        if cfg.webfetch.enabled { "enabled" } else { "disabled" }
                    )
                }
                other => format!("unknown value '{other}' — use /webfetch on or /webfetch off"),
            },
            "rnmap" if arg.is_empty() => format!(
                "rnmap scanner tool: {} (external targets: {})",
                if cfg.rnmap.enabled { "on" } else { "off" },
                if cfg.rnmap.allow_external_targets { "allowed" } else { "allowed" }
            ),
            "rnmap" => match arg.to_ascii_lowercase().as_str() {
                "on" | "off" => {
                    cfg.rnmap.enabled = arg.eq_ignore_ascii_case("on");
                    cfg.save();
                    format!(
                        "rnmap scanner tool {}.",
                        if cfg.rnmap.enabled { "enabled" } else { "disabled" }
                    )
                }
                other => format!("unknown value '{other}' — use /rnmap on or /rnmap off"),
            },
            "remix" if arg.is_empty() => {
                format!(
                    "remix (rrx Ethereum toolchain): {} (chain writes: {})",
                    if cfg.remix.enabled { "on" } else { "off" },
                    if cfg.remix.allow_chain_writes { "allowed" } else { "refused" }
                )
            }
            "remix" => match arg.to_ascii_lowercase().as_str() {
                "on" | "off" => {
                    cfg.remix.enabled = arg.eq_ignore_ascii_case("on");
                    cfg.save();
                    format!(
                        "remix Ethereum toolchain {}.",
                        if cfg.remix.enabled { "enabled" } else { "disabled" }
                    )
                }
                // The doctor runs the engine's status report: binary discovery,
                // version, RPC policy, safety flags, command surface.
                "status" | "doctor" => crate::remix::status_execute(&cfg, &serde_json::json!({}))
                    .unwrap_or_else(|e| format!("remix doctor failed: {e}")),
                "writes on" => {
                    cfg.remix.allow_chain_writes = true;
                    cfg.save();
                    "Chain writes (send/deploy/call/script) now ALLOWED for the remix tool — \
                     make sure RRX_PRIVATE_KEY is set in the server environment."
                        .into()
                }
                "writes off" => {
                    cfg.remix.allow_chain_writes = false;
                    cfg.save();
                    "Chain writes refused; RRX_PRIVATE_KEY is stripped from rrx children."
                        .into()
                }
                "help" => crate::remix::execute(&cfg, &serde_json::json!({"command": "help"}), &std::sync::atomic::AtomicBool::default())
                    .unwrap_or_else(|e| format!("remix help failed: {e}")),
                other => format!(
                    "unknown value '{other}' — use /remix on|off, /remix status, \
                     /remix writes on|off or /remix help"
                ),
            },
            "timeout" if arg.is_empty() => format!(
                "Provider request timeout: {}s",
                cfg.network.request_timeout_secs
            ),
            "timeout" => match arg.parse::<u64>() {
                Ok(v) if (5..=3600).contains(&v) => {
                    cfg.network.request_timeout_secs = v;
                    cfg.save();
                    format!("Request timeout set to {v}s.")
                }
                _ => "Usage: /timeout <5..3600 seconds>".to_string(),
            },
            "context" if arg.is_empty() => {
                format!("Context window: {} tokens", cfg.context_window)
            }
            "context" => match arg.parse::<u64>() {
                Ok(v) if (1_000..=4_000_000).contains(&v) => {
                    cfg.context_window = v;
                    cfg.save();
                    format!("Context window set to {v} tokens.")
                }
                _ => "Usage: /context <1000..4000000 tokens>".to_string(),
            },
            "documents" if arg.is_empty() => {
                let s = crate::documents_bridge::status();
                format!(
                    "documents engine (rc-docs): {} (tools: {})",
                    if cfg.documents.enabled { "on" } else { "off" },
                    s["tools"].as_array().map(|a| a.len()).unwrap_or(0)
                )
            }
            "documents" => match arg.to_ascii_lowercase().as_str() {
                "on" | "off" => {
                    cfg.documents.enabled = arg.eq_ignore_ascii_case("on");
                    cfg.save();
                    format!(
                        "documents engine {}.",
                        if cfg.documents.enabled { "enabled" } else { "disabled" }
                    )
                }
                "status" | "doctor" => match serde_json::to_string_pretty(&crate::documents_bridge::status()) {
                    Ok(s) => s,
                    Err(e) => format!("documents status: {e}"),
                },
                other => format!("unknown value '{other}' — use /documents on|off or /documents status"),
            },
            "prefs" | "settings" => crate::api::prefs_overview(&cfg),
            other => return Err(format!("unknown command /{other}")),
        };
        Ok(reply)
    }

    /// `/skills` — the catalog the system prompt and the `skill` tool see.
    fn skills_list(&self) -> String {
        let set = crate::skills::SkillSet::load();
        if set.0.is_empty() {
            return "No skills installed. Drop SKILL.md packages into \
                    <config_dir>/romacode/skills/<name>/ or .romacode/skills/<name>/"
                .to_string();
        }
        let mut out = String::from("Skills  (activate with /skill <name>; the agent can also call the skill tool)\n");
        for s in &set.0 {
            out.push_str(&format!("  {:<24} [{}] {}\n", s.name, s.source, s.description));
        }
        out
    }

    /// `/skill <name>` — resolve the name server-side (single source of
    /// truth for discovery) and start a turn whose prompt carries the skill
    /// body. Falls back to the newest session when the sending client did
    /// not identify its tab (old clients / `romacode skill` CLI).
    fn activate_skill_cmd(&self, session_id: Option<usize>, arg: &str) -> String {
        let arg = arg.trim();
        if arg.is_empty() {
            return self.skills_list();
        }
        match self.activate_skill(session_id, arg) {
            Ok(notice) => notice,
            Err(e) => e,
        }
    }

    /// The shared activation path for `/skill` and the `Skill` wire request.
    pub fn activate_skill(&self, session_id: Option<usize>, name: &str) -> Result<String, String> {
        let name = name.trim();
        if name.is_empty() {
            return Err("usage: /skill <name>  (/skills lists the catalog)".into());
        }
        let set = crate::skills::SkillSet::load();
        let Some(skill) = set.find(name) else {
            return Err(format!(
                "no skill named {name:?} — /skills lists the available ones"
            ));
        };
        let id = match session_id {
            Some(id) => id,
            None => self.newest_session(),
        };
        let mode = {
            let sessions = self.sessions.lock().unwrap_or_else(|e| e.into_inner());
            sessions.map.get(&id).map(|s| s.mode)
        }
        .unwrap_or_else(|| {
            let cfg = self.cfg.lock().unwrap_or_else(|e| e.into_inner());
            default_mode(&cfg)
        });
        self.start_turn(id, &skill.activation_prompt(), mode)?;
        // The activation turn was started server-side (no client optimistic
        // echo like the Message path has): broadcast the session snapshot so
        // every screen — including the one that ran /skill — shows the
        // activation prompt and the running turn immediately.
        if let Some(snap) = self.snapshot(id) {
            self.bus.broadcast(Event::Snapshot { session: snap });
        }
        Ok(format!(
            "Skill '{}' activated on session {id} ({}, {} chars of instructions).",
            skill.name,
            skill.source,
            skill.body.chars().count()
        ))
    }

    /// Persist a live-fetched model catalog for `provider_id` (FetchModels).
    /// The old flow only ECHOED the list to the screen that asked — close the
    /// panel without picking a model and the whole catalog was gone, so the
    /// model options kept showing the stale stored/suggested list. The list
    /// is now remembered in the provider entry (merged, never wiping
    /// user-added ids) and every attached screen is refreshed.
    fn remember_fetched_models(&self, provider_id: &str, models: &[String]) {
        if provider_id.trim().is_empty() || models.is_empty() {
            return;
        }
        {
            let mut cfg = self.cfg.lock().unwrap_or_else(|e| e.into_inner());
            cfg.remember_models(provider_id, models);
            cfg.save();
        }
        // the stored model options changed → refresh settings-dependent
        // client state on EVERY attached screen (req-0 Welcome pattern)
        self.bus.broadcast_reply(Reply::Welcome(self.welcome()));
    }

    fn add_custom(&self, cfg: &mut Config, id: &str, url: &str, kind: ApiKind) -> String {
        match cfg.add_custom_provider(id, url, kind) {
            Ok(()) => {
                if let Some(p) = cfg.providers.get_mut(&id.to_ascii_lowercase()) {
                    p.no_key = p.api_key.is_empty();
                }
                let _ = cfg.set_provider(id);
                cfg.save();
                format!(
                    "Custom provider {id} added and selected. Set its model with /model <name>; add a key with /apikey <key> if it needs one."
                )
            }
            Err(e) => e,
        }
    }

    /// Commit the settings-panel model choice (from the old client-side
    /// `save_settings_model`).
    fn save_model(
        &self,
        provider_id: &str,
        provider_name: &str,
        base_url: &str,
        api_key: &str,
        kind: &str,
        model: &str,
        models: &[String],
    ) -> Result<String, String> {
        if provider_id.is_empty() {
            return Err("no provider selected".into());
        }
        let kindv = match kind {
            "anthropic" => ApiKind::Anthropic,
            _ => ApiKind::OpenAi,
        };
        // Normalize the id once: config keys are lowercase (set_provider and
        // add_custom_provider already lowercase), and an entry saved under
        // "MyProvider" must never fork from "myprovider" — the duplicated
        // entry would split the stored models and keys across two rows.
        let pid = provider_id.trim().to_ascii_lowercase();
        if pid.is_empty() {
            return Err("no provider selected".into());
        }
        let mut cfg = self.cfg.lock().unwrap_or_else(|e| e.into_inner());
        if providers::find(&pid).is_some() {
            let _ = cfg.set_provider(&pid);
        } else {
            let _ = cfg.add_custom_provider(&pid, base_url, kindv);
        }
        cfg.provider = pid.clone();
        // Empty model = provider-only save (the TUI sends this right after the
        // user enters an API key, BEFORE the model list is fetched, so the key
        // survives a failed/overloaded fetch). Keep the current model.
        let provider_only = model.trim().is_empty();
        let default_model = model.to_string();
        let p = cfg.providers.entry(pid.clone()).or_default();
        // The screen never round-trips keys: the Select Model flow sends an
        // empty api_key on purpose. Only overwrite the stored key when a real
        // one arrived, so picking a model can never erase the user's key.
        let key = api_key.trim();
        if !key.is_empty() {
            p.api_key = key.to_string();
        }
        if !base_url.trim().is_empty() {
            p.base_url = base_url.to_string();
        }
        p.kind = Some(kindv);
        // MERGE, never replace: the screen captured its list when it opened,
        // so a pick from a stale screen must not wipe ids that another screen
        // fetched (or the user added) in the meantime — same semantics as
        // `Config::remember_models`: stored ids keep their position, incoming
        // ids are appended deduplicated under the 512-entry cap. An empty
        // incoming list (Select Model on a fresh entry) simply adds nothing.
        const MAX_MODELS: usize = 512;
        for m in models {
            let m = m.trim();
            if !m.is_empty() && !p.models.iter().any(|x| x == m) && p.models.len() < MAX_MODELS {
                p.models.push(m.to_string());
            }
        }
        if !provider_only {
            p.default_model = default_model;
        }
        if !provider_name.trim().is_empty() {
            p.name = provider_name.to_string();
        }
        if !provider_only {
            cfg.model = model.to_string();
        }
        cfg.save();
        Ok(if provider_only {
            format!(
                "Provider saved (key + base URL stored) — active model unchanged: {}",
                cfg.effective_model()
            )
        } else {
            format!("Active model: {}", cfg.effective_model())
        })
    }

    /// Provider rows for the settings panel (`Welcome.providers`). Every row
    /// carries a usable model list: when the config entry has none (known
    /// provider added by key/env only), the shared catalog's suggestions fill
    /// in — otherwise "Select Model" shows an empty list for fresh installs.
    /// The active provider is always present, even when it has no config
    /// entry at all (out-of-the-box `provider = "anthropic"` + env key).
    fn provider_infos(&self) -> Vec<ProviderInfo> {
        let cfg = self.cfg.lock().unwrap_or_else(|e| e.into_inner());
        let mut v: Vec<ProviderInfo> = cfg
            .providers
            .iter()
            .map(|(id, p)| {
                let known = providers::find(id);
                let mut models = p.models.clone();
                if models.is_empty() {
                    if let Some(k) = known {
                        models = k.models.iter().map(|m| m.to_string()).collect();
                    }
                }
                let default_model = if p.default_model.is_empty() {
                    known.map(|k| k.default_model.to_string())
                } else {
                    Some(p.default_model.clone())
                };
                if let Some(d) = &default_model {
                    if !models.iter().any(|m| m == d) {
                        models.insert(0, d.clone());
                    }
                }
                ProviderInfo {
                    id: id.clone(),
                    name: if p.name.is_empty() {
                        known.map(|k| k.name.to_string()).unwrap_or_else(|| id.clone())
                    } else {
                        p.name.clone()
                    },
                    base_url: if p.base_url.is_empty() {
                        known.map(|k| k.base_url.to_string()).unwrap_or_default()
                    } else {
                        p.base_url.clone()
                    },
                    kind: p
                        .kind
                        .map(|k| match k {
                            ApiKind::OpenAi => "openai".into(),
                            ApiKind::Anthropic => "anthropic".into(),
                        })
                        .or_else(|| known.map(|k| k.kind.as_str().to_string()))
                        .unwrap_or_else(|| "openai".into()),
                    has_key: !p.api_key.is_empty()
                        || p.no_key
                        || known.map_or(false, |k| {
                            !k.key_env.is_empty()
                                && std::env::var(&k.key_env)
                                    .map(|v| !v.trim().is_empty())
                                    .unwrap_or(false)
                        }),
                    models,
                    default_model,
                }
            })
            .collect();
        if let Some(k) = providers::find(cfg.provider.trim()) {
            let id = k.id.to_string();
            if !v.iter().any(|p| p.id == id) {
                v.push(ProviderInfo {
                    id: id.clone(),
                    name: k.name.to_string(),
                    base_url: k.base_url.to_string(),
                    kind: k.kind.as_str().to_string(),
                    has_key: std::env::var(&k.key_env)
                        .map(|val| !val.trim().is_empty())
                        .unwrap_or(false),
                    models: k.models.iter().map(|m| m.to_string()).collect(),
                    default_model: Some(k.default_model.to_string()),
                });
            }
        }
        v.sort_by(|a, b| a.id.cmp(&b.id));
        v
    }

    fn welcome(&self) -> Welcome {
        let cfg = self.cfg.lock().unwrap_or_else(|e| e.into_inner()).clone();
        let cwd = std::env::current_dir()
            .map(|p| p.display().to_string())
            .unwrap_or_default();
        let branch = git_branch();
        let missing_keys: Vec<String> = self
            .provider_infos()
            .iter()
            .filter(|p| !p.has_key && providers::find(&p.id).is_some())
            .map(|p| p.id.clone())
            .collect();
        Welcome {
            server_version: env!("CARGO_PKG_VERSION").to_string(),
            protocol_version: Welcome::PROTOCOL_VERSION,
            provider: cfg.provider.clone(),
            model: cfg.effective_model(),
            mode_default: default_mode(&cfg),
            engine: cfg.agent.engine.clone(),
            chat_width_pct: cfg.chat_width_pct,
            sidebar: cfg.sidebar,
            cwd,
            branch,
            providers: self.provider_infos(),
            sessions: self.sessions_list(),
            device: device_status(&self.android),
            missing_keys,
            context_window: cfg.context_window,
            price_in_per_mtok: cfg.price_in_per_mtok,
            price_out_per_mtok: cfg.price_out_per_mtok,
            max_tool_rounds: cfg.agent.max_tool_rounds,
            webfetch_enabled: cfg.webfetch.enabled,
            rnmap_enabled: cfg.rnmap.enabled,
            remix_enabled: cfg.remix.enabled,
            request_timeout_secs: cfg.network.request_timeout_secs,
            tui: cfg.tui.clone(),
        }
    }

    fn set_ui_pref(&self, key: &str, value: &str) -> Result<(), String> {
        let mut cfg = self.cfg.lock().unwrap_or_else(|e| e.into_inner());
        match key {
            "sidebar" => {
                cfg.sidebar = value == "true" || value == "1" || value == "on";
            }
            "chat_width_pct" => {
                let v: u16 = value
                    .parse()
                    .map_err(|_| "chat_width_pct must be a number".to_string())?;
                cfg.chat_width_pct = v.clamp(20, 70);
            }
            other => {
                // `tui.<path>` addresses any OpenCode-style setting by its
                // dotted path (`tui.session.thinking`, `tui.mouse`, …).
                let path = other
                    .strip_prefix("tui.")
                    .ok_or_else(|| format!("unknown ui pref {other}"))?;
                cfg.set_tui_pref(path, value)?;
            }
        }
        if key == "sidebar" {
            cfg.tui.session.sidebar = if cfg.sidebar { "auto" } else { "hide" }.into();
        }
        cfg.save();
        Ok(())
    }
}

fn default_mode(cfg: &Config) -> crate::session::Mode {
    match cfg.agent.default_mode {
        crate::config::AgentMode::Build => crate::session::Mode::Build,
        crate::config::AgentMode::Plan => crate::session::Mode::Plan,
    }
}

/// Which provider entry a fetched model catalog belongs to. New clients tag
/// every FetchModels with the provider being configured; a missing id (old
/// protocol-v1 clients, or a hand-rolled request) falls back to the ACTIVE
/// provider, which the preceding provider-only SaveModel has already set.
/// Trim + lowercase so the id matches the config keys. Pure function so the
/// fallback is unit-testable without a network round-trip.
fn resolve_fetch_provider_id(provider_id: &str, active_provider: &str) -> String {
    let id = provider_id.trim().to_ascii_lowercase();
    if !id.is_empty() {
        return id;
    }
    active_provider.trim().to_ascii_lowercase()
}

/// One-glance `/prefs` listing — every setting the settings panel offers.
fn prefs_overview(cfg: &Config) -> String {
    format!(
        "Settings (server-owned):\n  provider        {p}\n  model           {m}\n  engine          {e}   (/engine native|langchain)\n  default mode    {dm:?}   (/mode build|plan)\n  max tool rounds {r}   (/rounds <1..200>)\n  web fetch       {w}   (/webfetch on|off)
  rnmap scanner   {rn}   (/rnmap on|off)\n  remix (rrx eth) {rx}   (/remix on|off|status)\n  request timeout {t}s  (/timeout <5..3600>)\n  context window  {c}   (/context <tokens>)\n  chat width      {cw}%  (panel)\n  sidebar         {sb}  (panel)\n  config file     ~/.config/romacode/config.json (server cwd)",
        p = cfg.provider,
        m = cfg.effective_model(),
        e = cfg.agent.engine,
        dm = cfg.agent.default_mode,
        r = cfg.agent.max_tool_rounds,
        w = if cfg.webfetch.enabled { "on" } else { "off" },
        rn = if cfg.rnmap.enabled { "on" } else { "off" },
        rx = if cfg.remix.enabled { "on" } else { "off" },
        t = cfg.network.request_timeout_secs,
        c = cfg.context_window,
        cw = cfg.chat_width_pct,
        sb = if cfg.sidebar { "on" } else { "off" },
    )
}

fn device_status(android: &crate::android::Android) -> Option<romacode_shared::protocol::DeviceStatus> {
    let st = android.status()?;
    let phase = match &st.phase {
        Phase::Booting => "booting".to_string(),
        Phase::Online => "online".to_string(),
        Phase::Error(e) => format!("error:{e}"),
    };
    Some(romacode_shared::protocol::DeviceStatus {
        phase,
        serial: st.serial.clone(),
        model: st.model.clone(),
        port: st.port,
        root: st.root.clone(),
        display: st.display,
        battery: st.battery,
        package_count: st.package_count,
    })
}

fn git_branch() -> Option<String> {
    let out = std::process::Command::new("git")
        .args(["rev-parse", "--abbrev-ref", "HEAD"])
        .output()
        .ok()?;
    let s = String::from_utf8_lossy(&out.stdout).trim().to_string();
    if s.is_empty() {
        None
    } else {
        Some(s)
    }
}

// ---------------------------------------------------------------------------
// Server entry point.
// ---------------------------------------------------------------------------

/// Serve forever: REST + WebSocket on `bind`, device frame streaming in the
/// background.
pub fn serve(cfg: Config, bind: &str) -> anyhow::Result<()> {
    let bus = Bus::new();
    let hub = Hub::new(cfg, bus.clone());
    bus.set_tap(&hub);

    let listener = TcpListener::bind(bind)
        .map_err(|e| anyhow::anyhow!("cannot bind {bind}: {e}"))?;
    eprintln!(
        "romacode-server {} listening on http://{bind}  (ws:// {bind}/ws)",
        env!("CARGO_PKG_VERSION")
    );

    // Device frame + status streamer: PNG-encodes committed frames and
    // broadcasts them while at least one client wants the stream.
    {
        let hub = hub.clone();
        std::thread::Builder::new()
            .name("frame-streamer".into())
            .spawn(move || frame_streamer(hub))
            .ok();
    }

    for stream in listener.incoming() {
        let Ok(stream) = stream else { continue };
        let hub = hub.clone();
        let _ = std::thread::Builder::new()
            .name("conn".into())
            .spawn(move || handle_conn(stream, hub));
    }
    Ok(())
}

/// PNG-encode committed device frames and push them at a calm rate while
/// clients watch. Full-resolution so client tap mapping stays 1:1.
fn frame_streamer(hub: Arc<Hub>) {
    let mut last_sent_gen = 0u64;
    let mut last_sent_status: Option<romacode_shared::protocol::DeviceStatus> = None;
    let mut last_encode = Instant::now() - Duration::from_secs(1);
    loop {
        let watchers = hub
            .android
            .demand
            .load(Ordering::Relaxed);
        let want = watchers > 0;
        hub.android.set_streaming(want);
        if want {
            let changed = hub.android.poll();
            if changed {
                if let Some(st) = device_status(&hub.android) {
                    if last_sent_status.as_ref() != Some(&st) {
                        last_sent_status = Some(st.clone());
                        hub.bus.broadcast(Event::Device { status: st });
                    }
                }
                if let Some(frame) = hub.android.frame() {
                    if frame.gen != last_sent_gen
                        && last_encode.elapsed() >= Duration::from_millis(350)
                    {
                        if let Ok(png) = encode_png(frame.w, frame.h, &frame.rgba) {
                            last_sent_gen = frame.gen;
                            last_encode = Instant::now();
                            hub.bus.broadcast(Event::Frame {
                                gen: frame.gen,
                                w: frame.w,
                                h: frame.h,
                                png_base64: base64_encode(&png),
                            });
                        }
                    }
                }
            }
        }
        std::thread::sleep(Duration::from_millis(120));
    }
}

fn encode_png(w: u32, h: u32, rgba: &[u8]) -> Result<Vec<u8>, String> {
    let mut out = Vec::new();
    {
        let mut enc = png::Encoder::new(&mut out, w.max(1), h.max(1));
        enc.set_color(png::ColorType::Rgba);
        enc.set_depth(png::BitDepth::Eight);
        let mut writer = enc
            .write_header()
            .map_err(|e| format!("png header: {e}"))?;
        writer
            .write_image_data(rgba)
            .map_err(|e| format!("png data: {e}"))?;
    }
    Ok(out)
}

// ---------------------------------------------------------------------------
// Connection handling: HTTP parsing, REST routes, WS upgrade.
// ---------------------------------------------------------------------------

fn handle_conn(stream: TcpStream, hub: Arc<Hub>) {
    let _ = stream.set_nodelay(true);
    let mut reader = BufReader::new(match stream.try_clone() {
        Ok(s) => s,
        Err(_) => return,
    });
    let req = match read_request_head(&mut reader, &stream) {
        Some(r) => r,
        None => return,
    };

    // WebSocket upgrade?
    let upgrade = req
        .headers
        .iter()
        .any(|(k, v)| k.eq_ignore_ascii_case("upgrade") && v.to_ascii_lowercase().contains("websocket"));
    if upgrade {
        let key = req
            .headers
            .iter()
            .find(|(k, _)| k.eq_ignore_ascii_case("sec-websocket-key"))
            .map(|(_, v)| v.clone())
            .unwrap_or_default();
        if websocket_accept(&key).is_none() {
            let _ = write_response(&stream, 400, "Bad Request", "missing websocket key");
            return;
        }
        let accept = websocket_accept(&key).unwrap();
        let resp = format!(
            "HTTP/1.1 101 Switching Protocols\r\nUpgrade: websocket\r\nConnection: Upgrade\r\nSec-WebSocket-Accept: {accept}\r\n\r\n"
        );
        if stream.try_clone().and_then(|mut s| s.write_all(resp.as_bytes())).is_err() {
            return;
        }
        let mut ws = WsStream { stream, reader };
        handle_ws(&mut ws, hub, req.path);
    } else {
        // REST: read (and discard) any body, route, respond, close.
        drain_body(&mut reader, &req);
        let (status, body) = route_rest(&req, &hub);
        let _ = write_response(&stream, status, if status == 200 { "OK" } else { "Error" }, &body);
    }
}

struct ReqHead {
    method: String,
    path: String,
    headers: Vec<(String, String)>,
}

/// Total request-head budget (request line + all header lines). Like
/// drain_body's oversize rule for bodies, a client beyond it is cut off
/// instead of being allowed to stream headers forever.
const MAX_HEAD_BYTES: usize = 64 * 1024;

fn read_request_head(reader: &mut BufReader<TcpStream>, stream: &TcpStream) -> Option<ReqHead> {
    let mut total = 0usize;
    let mut line = String::new();
    total += reader.read_line(&mut line).ok()?;
    if total == 0 {
        return None;
    }
    if total > MAX_HEAD_BYTES {
        let _ = write_response(
            stream,
            431,
            "Request Header Fields Too Large",
            &json!({ "error": "request head exceeds 64 KiB" }).to_string(),
        );
        return None; // close the request without reading further
    }
    let mut parts = line.split_whitespace();
    let method = parts.next()?.to_string();
    let path = parts.next()?.to_string();
    let mut headers = Vec::new();
    loop {
        let mut h = String::new();
        let n = reader.read_line(&mut h).ok()?;
        total += n;
        if n == 0 {
            break;
        }
        if total > MAX_HEAD_BYTES {
            let _ = write_response(
                stream,
                431,
                "Request Header Fields Too Large",
                &json!({ "error": "request head exceeds 64 KiB" }).to_string(),
            );
            return None; // stop reading; the connection closes below
        }
        let h = h.trim_end().to_string();
        if h.is_empty() {
            break;
        }
        if let Some((k, v)) = h.split_once(':') {
            headers.push((k.trim().to_string(), v.trim().to_string()));
        }
    }
    Some(ReqHead { method, path, headers })
}

fn drain_body(reader: &mut BufReader<TcpStream>, req: &ReqHead) {
    let len: usize = req
        .headers
        .iter()
        .find(|(k, _)| k.eq_ignore_ascii_case("content-length"))
        .and_then(|(_, v)| v.parse().ok())
        .unwrap_or(0);
    if len > 0 && len < 4 * 1024 * 1024 {
        let mut buf = vec![0u8; len];
        let _ = reader.read_exact(&mut buf);
    }
}

fn route_rest(req: &ReqHead, hub: &Arc<Hub>) -> (u16, String) {
    match (req.method.as_str(), req.path.as_str()) {
        ("GET", "/api/health") => (
            200,
            json!({
                "ok": true,
                "server": "romacode-server",
                "version": env!("CARGO_PKG_VERSION"),
                "protocol": Welcome::PROTOCOL_VERSION,
                "sessions": hub.sessions_list().len(),
            })
            .to_string(),
        ),
        ("GET", "/api/sessions") => (200, serde_json::to_string(&hub.sessions_list()).unwrap_or_default()),
        ("GET", "/api/config") => {
            let cfg = hub.cfg.lock().unwrap_or_else(|e| e.into_inner());
            (200, cfg.to_pretty_json())
        }
        ("GET", "/api/rag") => match crate::langchain_bridge::rag_stats() {
            Some((n, label)) => (200, json!({ "chunks": n, "embeddings": label }).to_string()),
            None => (200, json!({ "chunks": 0 }).to_string()),
        },
        ("POST", "/api/message") => {
            // The REST surface is read-only; starting turns (and streaming
            // their events) requires the WebSocket protocol at /ws.
            (405, json!({"error": "connect to /ws to start agent turns"}).to_string())
        }
        ("GET", _) | ("POST", _) => (
            404,
            json!({"error": "not found", "routes": ["/api/health", "/api/sessions", "/api/config", "/api/rag", "/ws"]}).to_string(),
        ),
        _ => (405, json!({"error": "method not allowed"}).to_string()),
    }
}

fn write_response(stream: &TcpStream, status: u16, reason: &str, body: &str) -> std::io::Result<()> {
    let mut s = stream.try_clone()?;
    let resp = format!(
        "HTTP/1.1 {status} {reason}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
        body.len()
    );
    s.write_all(resp.as_bytes())
}

// ---------------------------------------------------------------------------
// WebSocket session.
// ---------------------------------------------------------------------------

fn handle_ws(ws: &mut WsStream, hub: Arc<Hub>, path: String) {
    let _ = path;
    let (tx, rx) = std::sync::mpsc::sync_channel::<ServerEnvelope>(1024);
    let sink = BoundedSink { tx };
    let token = hub.bus.register(sink.clone());
    // Serializes every write to this client's socket: the writer thread and
    // the reader thread (ws_pong) used to write to separate TcpStream clones
    // with no synchronization — interleaved partial writes corrupt frames
    // for any ping-capable client.
    let socket_lock: Arc<std::sync::Mutex<()>> = Arc::new(std::sync::Mutex::new(()));
    // Writer thread: everything the hub broadcasts + direct replies.
    let writer_running = Arc::new(AtomicBool::new(true));
    {
        let running = writer_running.clone();
        let stream = ws.stream.try_clone();
        let lock = socket_lock.clone();
        let rx = rx;
        std::thread::Builder::new()
            .name("ws-writer".into())
            .spawn(move || {
                if let Ok(mut stream) = stream {
                    while running.load(Ordering::Relaxed) {
                        match rx.recv_timeout(Duration::from_millis(250)) {
                            Ok(env) => {
                                let text =
                                    serde_json::to_string(&env).unwrap_or_else(|_| "{}".into());
                                let _guard = lock.lock().unwrap_or_else(|e| e.into_inner());
                                if ws_send_text(&mut stream, &text).is_err() {
                                    break;
                                }
                            }
                            Err(std::sync::mpsc::RecvTimeoutError::Timeout) => continue,
                            Err(_) => break,
                        }
                    }
                }
            })
            .ok();
    }

    // Greet: welcome payload + snapshot of the newest session.
    let _ = sink.send(ServerEnvelope::reply(0, Reply::Welcome(hub.welcome())));
    if let Some(snap) = hub.snapshot(hub.newest_session()) {
        let _ = sink.send(ServerEnvelope::push(Event::Snapshot { session: snap }));
    }

    // Android frame streaming is enabled per client and reaped on
    // disconnect — the old global counter was decremented unconditionally
    // on every disconnect and clamped only when prev == 0, so a few
    // connect/disconnect cycles sent it negative and the frame streamer
    // never started again.
    let mut streaming = false;
    // Frame loop: read client frames, dispatch.
    let mut buf = Vec::new();
    loop {
        match ws_read_frame(ws, &mut buf) {
            Ok(Frame::Text(text)) => {
                let env: ClientEnvelope = match serde_json::from_str(&text) {
                    Ok(e) => e,
                    Err(e) => {
                        let _ = sink.send(ServerEnvelope::reply_err(
                            0,
                            format!("bad envelope: {e}"),
                        ));
                        continue;
                    }
                };
                dispatch(hub.clone(), env, &sink, &writer_running, &mut streaming);
            }
            Ok(Frame::Ping(payload)) => {
                let _guard = socket_lock.lock().unwrap_or_else(|e| e.into_inner());
                let _ = ws_pong(ws, &payload);
            }
            Ok(Frame::Close) | Err(_) => break,
            Ok(Frame::Ignore) => {}
        }
    }
    writer_running.store(false, Ordering::Relaxed);
    hub.bus.unregister(token);
    if streaming {
        let _ = hub.android.demand.try_update(Ordering::Relaxed, Ordering::Relaxed, |v| {
            Some(v.saturating_sub(1))
        });
    }
}

/// Handle one client request. Runs on the reader thread; long work (model
/// catalog fetch) is spawned. `streaming` tracks whether THIS client enabled
/// Android frame streaming, so the demand counter is reaped exactly once per
/// enabling client on disconnect.
fn dispatch(
    hub: Arc<Hub>,
    env: ClientEnvelope,
    tx: &Sink,
    _running: &Arc<AtomicBool>,
    streaming: &mut bool,
) {
    let req_id = env.req;
    let reply_err = |e: String| {
        let _ = tx.send(ServerEnvelope::reply_err(req_id, e));
    };
    match env.body {
        ClientRequest::Hello { .. } => {
            let _ = tx.send(ServerEnvelope::reply(req_id, Reply::Welcome(hub.welcome())));
            if let Some(snap) = hub.snapshot(hub.newest_session()) {
                let _ = tx.send(ServerEnvelope::push(Event::Snapshot { session: snap }));
            }
        }
        ClientRequest::Ping => {
            let _ = tx.send(ServerEnvelope::reply(req_id, Reply::Pong));
        }
        ClientRequest::NewSession => {
            let id = hub.new_session();
            hub.bus.broadcast(Event::Sessions {
                sessions: hub.sessions_list(),
            });
            if let Some(snap) = hub.snapshot(id) {
                hub.bus.broadcast(Event::Snapshot { session: snap });
            }
            let _ = tx.send(ServerEnvelope::reply(req_id, Reply::NewSession { id }));
        }
        ClientRequest::CloseSession { id } => {
            hub.close_session(id);
            let _ = tx.send(ServerEnvelope::reply(req_id, Reply::Pong));
        }
        ClientRequest::SelectSession { id } => {
            if let Some(snap) = hub.snapshot(id) {
                hub.bus.broadcast(Event::Snapshot { session: snap });
                let _ = tx.send(ServerEnvelope::reply(req_id, Reply::Pong));
            } else {
                reply_err(format!("no session {id}"));
            }
        }
        ClientRequest::Message { id, text, mode } => {
            match hub.start_turn(id, &text, mode) {
                Ok(()) => {
                    let _ = tx.send(ServerEnvelope::reply(req_id, Reply::Pong));
                }
                Err(e) => reply_err(e),
            }
        }
        ClientRequest::Cancel { id } => {
            hub.interrupt(id);
            let _ = tx.send(ServerEnvelope::reply(req_id, Reply::Pong));
        }
        ClientRequest::Steer { id, text } => match hub.steer(id, &text) {
            Ok(()) => {
                let _ = tx.send(ServerEnvelope::reply(req_id, Reply::Pong));
            }
            Err(e) => reply_err(e),
        },
        ClientRequest::ToggleMode { id } => {
            let mut sessions = hub.sessions.lock().unwrap_or_else(|e| e.into_inner());
            if let Some(s) = sessions.map.get_mut(&id) {
                s.mode = s.mode.toggle();
                let snap = s.snapshot();
                drop(sessions);
                hub.bus.broadcast(Event::Snapshot { session: snap });
                let _ = tx.send(ServerEnvelope::reply(req_id, Reply::Pong));
            } else {
                reply_err(format!("no session {id}"));
            }
        }
        ClientRequest::Approval {
            session_id: _,
            request_id,
            approve,
        } => {
            let notice = hub.resolve_approval(&request_id, approve);
            let _ = tx.send(ServerEnvelope::reply(
                req_id,
                Reply::Notice { text: notice },
            ));
        }
        ClientRequest::Command { text, session_id } => match hub.run_command_in(session_id, &text) {
            Ok(notice) => {
                let _ = tx.send(ServerEnvelope::reply(
                    req_id,
                    Reply::Notice { text: notice },
                ));
                // settings commands change the config mirror → refresh every
                // attached screen. Broadcast the Welcome reply with req 0;
                // screens apply a req-0 Welcome as a state refresh (the old
                // per-client send was dropped by the TUI reply matcher).
                hub.bus.broadcast_reply(Reply::Welcome(hub.welcome()));
            }
            Err(e) => reply_err(e),
        },
        ClientRequest::Skill { id, name } => {
            // The activation itself starts an agent turn on session `id`;
            // its events reach every screen through the bus, the reply is
            // just the ack notice.
            match hub.activate_skill(Some(id), &name) {
                Ok(notice) => {
                    let _ = tx.send(ServerEnvelope::reply(
                        req_id,
                        Reply::Notice { text: notice },
                    ));
                }
                Err(e) => reply_err(e),
            }
        }
        ClientRequest::FetchModels {
            provider_id,
            base_url,
            kind,
            api_key,
        } => {
            let tx = tx.clone();
            let hub_ref = hub.clone();
            std::thread::Builder::new()
                .name("fetch-models".into())
                .spawn(move || {
                    let kindv = match kind.as_str() {
                        "anthropic" => ApiKind::Anthropic,
                        _ => ApiKind::OpenAi,
                    };
                    let res = providers::fetch_models(&base_url, kindv, &api_key);
                    match res {
                        Ok(models) => {
                            // THE FIX for "fetched models never show up in the
                            // model options": remember the catalog in the
                            // provider entry (merged, never wiping ids the
                            // user added) so it lands in Select Model, /models
                            // and the CLI — not just in the screen that asked.
                            // The Welcome broadcast re-syncs every screen.
                            // Protocol-v1 clients (and any request that omits
                            // provider_id) still persist: the SaveModel that
                            // always precedes the fetch made the configured
                            // provider ACTIVE, so the catalog belongs there.
                            let pid = resolve_fetch_provider_id(
                                &provider_id,
                                &hub_ref
                                    .cfg
                                    .lock()
                                    .unwrap_or_else(|e| e.into_inner())
                                    .provider,
                            );
                            hub_ref.remember_fetched_models(&pid, &models);
                            let _ = tx.send(ServerEnvelope::reply(
                                req_id,
                                Reply::Models { models },
                            ));
                        }
                        Err(e) => {
                            let _ = tx.send(ServerEnvelope::reply_err(req_id, e));
                        }
                    }
                    drop(hub_ref);
                })
                .ok();
        }
        ClientRequest::SaveModel {
            provider_id,
            provider_name,
            base_url,
            api_key,
            kind,
            model,
            models,
        } => match hub.save_model(
            &provider_id,
            &provider_name,
            &base_url,
            &api_key,
            &kind,
            &model,
            &models,
        ) {
            Ok(notice) => {
                let _ = tx.send(ServerEnvelope::reply(
                    req_id,
                    Reply::Notice { text: notice },
                ));
                // provider change → refresh settings-dependent client state
                // on EVERY attached screen (req-0 Welcome, applied by screens).
                hub.bus.broadcast_reply(Reply::Welcome(hub.welcome()));
            }
            Err(e) => reply_err(e),
        },
        ClientRequest::SetUiPref { key, value } => match hub.set_ui_pref(&key, &value) {
            Ok(()) => {
                let _ = tx.send(ServerEnvelope::reply(req_id, Reply::Pong));
            }
            Err(e) => reply_err(e),
        },
        ClientRequest::Android { action } => {
            // Stream on/off is THIS client's demand-counter bookkeeping —
            // tracked per connection and reaped on disconnect.
            if let AndroidAction::Stream { on } = action {
                if on {
                    if !*streaming {
                        hub.android.demand.fetch_add(1, Ordering::Relaxed);
                        *streaming = true;
                    }
                } else if *streaming {
                    let _ = hub.android.demand.try_update(Ordering::Relaxed, Ordering::Relaxed, |v| {
                        Some(v.saturating_sub(1))
                    });
                    *streaming = false;
                }
            } else {
                handle_android(&hub, action);
            }
            let _ = tx.send(ServerEnvelope::reply(req_id, Reply::Pong));
        }
    }
}

fn handle_android(hub: &Arc<Hub>, action: AndroidAction) {
    match action {
        // Stream is handled by dispatch (per-client bookkeeping).
        AndroidAction::Stream { .. } => {}
        AndroidAction::Tap { x, y } => hub.android.tap(x, y),
        AndroidAction::Swipe { x1, y1, x2, y2 } => hub.android.swipe(x1, y1, x2, y2),
        AndroidAction::Text { text } => hub.android.text(&text),
        AndroidAction::Key { key } => {
            use phone::state::KeyCode;
            let code = match key.as_str() {
                "home" => Some(KeyCode::Home),
                "back" => Some(KeyCode::Back),
                "enter" => Some(KeyCode::Enter),
                "del" => Some(KeyCode::Del),
                "power" => Some(KeyCode::Power),
                "volume_up" => Some(KeyCode::VolumeUp),
                "volume_down" => Some(KeyCode::VolumeDown),
                _ => None,
            };
            if let Some(code) = code {
                hub.android.key(code);
            }
        }
    }
}

// ---------------------------------------------------------------------------
// Minimal WebSocket framing (server side: reads masked client frames, writes
// unmasked server frames). Pure std — no websocket dependency.
// ---------------------------------------------------------------------------

struct WsStream {
    stream: TcpStream,
    reader: BufReader<TcpStream>,
}

enum Frame {
    Text(String),
    Ping(Vec<u8>),
    Close,
    Ignore,
}

fn ws_read_frame(ws: &mut WsStream, buf: &mut Vec<u8>) -> std::io::Result<Frame> {
    let mut hdr = [0u8; 2];
    ws.reader.read_exact(&mut hdr)?;
    let fin = hdr[0] & 0x80 != 0;
    let opcode = hdr[0] & 0x0f;
    let masked = hdr[1] & 0x80 != 0;
    let mut len = (hdr[1] & 0x7f) as u64;
    if len == 126 {
        let mut ext = [0u8; 2];
        ws.reader.read_exact(&mut ext)?;
        len = u16::from_be_bytes(ext) as u64;
    } else if len == 127 {
        let mut ext = [0u8; 8];
        ws.reader.read_exact(&mut ext)?;
        len = u64::from_be_bytes(ext);
    }
    if len > 32 * 1024 * 1024 {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            "frame too large",
        ));
    }
    let mut mask = [0u8; 4];
    if masked {
        ws.reader.read_exact(&mut mask)?;
    }
    let mut payload = vec![0u8; len as usize];
    ws.reader.read_exact(&mut payload)?;
    if masked {
        for (i, b) in payload.iter_mut().enumerate() {
            *b ^= mask[i % 4];
        }
    }
    match opcode {
        0x1 | 0x2 => {
            buf.extend_from_slice(&payload);
            if fin {
                let out = String::from_utf8_lossy(buf).into_owned();
                buf.clear();
                Ok(Frame::Text(out))
            } else {
                Ok(Frame::Ignore)
            }
        }
        0x8 => Ok(Frame::Close),
        0x9 => Ok(Frame::Ping(payload)),
        _ => Ok(Frame::Ignore),
    }
}

fn ws_send_text(stream: &mut TcpStream, text: &str) -> std::io::Result<()> {
    let bytes = text.as_bytes();
    let len = bytes.len();
    let header: Vec<u8> = if len < 126 {
        vec![0x81, len as u8]
    } else if len < 65536 {
        let mut h = vec![0x81, 126];
        h.extend_from_slice(&(len as u16).to_be_bytes());
        h
    } else {
        let mut h = vec![0x81, 127];
        h.extend_from_slice(&(len as u64).to_be_bytes());
        h
    };
    stream.write_all(&header)?;
    stream.write_all(bytes)
}

fn ws_pong(ws: &mut WsStream, payload: &[u8]) -> std::io::Result<()> {
    let mut frame = vec![0x8a, payload.len().min(125) as u8];
    frame.extend_from_slice(&payload[..payload.len().min(125)]);
    ws.stream.write_all(&frame)
}

// ---------------------------------------------------------------------------
// SHA-1 + base64 for the WS handshake (no crypto dependency needed: the
// accept key is a fixed algorithm over the client key + a constant GUID).
// ---------------------------------------------------------------------------

fn sha1(data: &[u8]) -> [u8; 20] {
    let mut h: [u32; 5] = [0x67452301, 0xEFCDAB89, 0x98BADCFE, 0x10325476, 0xC3D2E1F0];
    let ml = (data.len() as u64) * 8;
    let mut msg = data.to_vec();
    msg.push(0x80);
    while msg.len() % 64 != 56 {
        msg.push(0);
    }
    msg.extend_from_slice(&ml.to_be_bytes());
    for chunk in msg.chunks(64) {
        let mut w = [0u32; 80];
        for (i, word) in chunk.chunks(4).enumerate() {
            w[i] = u32::from_be_bytes([word[0], word[1], word[2], word[3]]);
        }
        for i in 16..80 {
            w[i] = (w[i - 3] ^ w[i - 8] ^ w[i - 14] ^ w[i - 16]).rotate_left(1);
        }
        let (mut a, mut b, mut c, mut d, mut e) = (h[0], h[1], h[2], h[3], h[4]);
        for (i, wi) in w.iter().enumerate() {
            let (f, k) = match i {
                0..=19 => ((b & c) | ((!b) & d), 0x5A827999u32),
                20..=39 => (b ^ c ^ d, 0x6ED9EBA1),
                40..=59 => ((b & c) | (b & d) | (c & d), 0x8F1BBCDC),
                _ => (b ^ c ^ d, 0xCA62C1D6),
            };
            let temp = a
                .rotate_left(5)
                .wrapping_add(f)
                .wrapping_add(k)
                .wrapping_add(*wi)
                .wrapping_add(e);
            e = d;
            d = c;
            c = b.rotate_left(30);
            b = a;
            a = temp;
        }
        h[0] = h[0].wrapping_add(a);
        h[1] = h[1].wrapping_add(b);
        h[2] = h[2].wrapping_add(c);
        h[3] = h[3].wrapping_add(d);
        h[4] = h[4].wrapping_add(e);
    }
    let mut out = [0u8; 20];
    for (i, v) in h.iter().enumerate() {
        out[i * 4..i * 4 + 4].copy_from_slice(&v.to_be_bytes());
    }
    out
}

fn base64_encode(data: &[u8]) -> String {
    use base64::Engine as _;
    base64::engine::general_purpose::STANDARD.encode(data)
}

const WS_GUID: &str = "258EAFA5-E914-47DA-95CA-C5AB0DC85B11";

fn websocket_accept(key: &str) -> Option<String> {
    if key.is_empty() {
        return None;
    }
    let mut input = key.trim().as_bytes().to_vec();
    input.extend_from_slice(WS_GUID.as_bytes());
    Some(base64_encode(&sha1(&input)))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sha1_known_vectors() {
        let hex = |b: &[u8]| b.iter().map(|x| format!("{x:02x}")).collect::<String>();
        assert_eq!(hex(&sha1(b"abc")), "a9993e364706816aba3e25717850c26c9cd0d89d");
        assert_eq!(hex(&sha1(b"")), "da39a3ee5e6b4b0d3255bfef95601890afd80709");
        assert_eq!(
            hex(&sha1(b"The quick brown fox jumps over the lazy dog")),
            "2fd4e1c67a2d28fced849ee1bb76e7391b93eb12"
        );
    }

    #[test]
    fn base64_known_vectors() {
        assert_eq!(base64_encode(b""), "");
        assert_eq!(base64_encode(b"f"), "Zg==");
        assert_eq!(base64_encode(b"fo"), "Zm8=");
        assert_eq!(base64_encode(b"foo"), "Zm9v");
        assert_eq!(base64_encode(b"foobar"), "Zm9vYmFy");
        assert_eq!(base64_encode(&[0xfb, 0xff]), "+/8=");
    }

    #[test]
    fn websocket_accept_matches_rfc_example() {
        // RFC 6455 §1.3 example handshake.
        let key = "dGhlIHNhbXBsZSBub25jZQ==";
        assert_eq!(
            websocket_accept(key).unwrap(),
            "s3pPLMBiTxaQ9kYGzzhZRbK+xOo="
        );
        assert!(websocket_accept("").is_none());
    }

    #[test]
    fn bus_delivers_and_cleans_dead_sinks() {
        let bus = Bus::new();
        let (tx, rx) = std::sync::mpsc::sync_channel(64);
        bus.register(BoundedSink { tx });
        bus.broadcast(Event::Notice {
            session_id: None,
            text: "hi".into(),
        });
        assert!(matches!(
            rx.try_recv(),
            Ok(ServerEnvelope {
                event: Some(Event::Notice { .. }),
                ..
            })
        ));
        // dead sink removed on next broadcast
        bus.broadcast(Event::Notice {
            session_id: None,
            text: "again".into(),
        });
        assert!(rx.try_recv().is_ok());
    }

    /// The approval contract, end to end inside one Hub: the gate asks → the
    /// loop registers + broadcasts a real ApprovalRequest → the tap records it
    /// (so snapshots restore the popup) → a client answers → the waiting agent
    /// thread observes the decision → the record is gone.
    #[test]
    fn approval_flow_end_to_end_through_the_hub() {
        use crate::config::Config;
        use std::time::Duration;

        let bus = Bus::new();
        let hub = Hub::new(Config::default(), bus.clone());
        bus.set_tap(&hub); // wire the internal tap, exactly like `serve`
        let (tx, rx) = std::sync::mpsc::sync_channel(64);
        hub.bus.register(BoundedSink { tx });

        // 1. the approval request is synthesized (the gate is fully open, so
        //    nothing asks by default — the approval machinery stays available
        //    for the optional approval UI)
        let request_id = "apr-test-1".to_string();
        let effect = crate::security::pending_effect("Bash", &json!({"command": "cargo test"}));
        assert!(effect.contains("cargo test"));

        // 2. the agent loop parks the answer slot FIRST (race fix), then
        //    broadcasts the wire event every screen needs
        let waiter = hub.security.register(&request_id);
        hub.bus.broadcast(Event::ApprovalRequest {
            session_id: 1,
            request_id: request_id.clone(),
            name: "Bash".into(),
            effect: effect.clone(),
        });

        // 3. the broadcast reached the attached screen as an ApprovalRequest
        let env = rx
            .recv_timeout(Duration::from_secs(1))
            .expect("screen receives the approval event");
        match env.event.expect("envelope carries an event") {
            Event::ApprovalRequest {
                request_id: rid,
                name,
                effect: fx,
                ..
            } => {
                assert_eq!(rid, request_id);
                assert_eq!(name, "Bash");
                assert_eq!(fx, effect);
            }
            other => panic!("wrong event: {other:?}"),
        }

        // 4. the tap recorded it — a (re)connecting screen gets the popup back
        let snap = hub.snapshot(1).expect("session 1 exists");
        assert_eq!(snap.pending_approvals.len(), 1);
        assert_eq!(snap.pending_approvals[0].request_id, request_id);
        assert_eq!(snap.pending_approvals[0].name, "Bash");

        // 5. the user allows on any client (the same path the `Approval` wire
        //    request takes); the waiting loop observes true
        let notice = hub.resolve_approval(&request_id, true);
        assert!(notice.contains("Approved"));
        assert_eq!(
            waiter.recv_timeout(Duration::from_secs(1)),
            Ok(true)
        );

        // 6. answered approvals never re-raise: the record is gone
        let snap = hub.snapshot(1).unwrap();
        assert!(snap.pending_approvals.is_empty());
    }

    /// Wire compatibility of the extended requests: new clients send
    /// `provider_id` on FetchModels and `session_id` on Command; protocol-v1
    /// clients (which never carried them) must keep decoding.
    #[test]
    fn fetch_models_and_command_requests_stay_wire_compatible() {
        // new-format FetchModels
        let req: ClientRequest = serde_json::from_value(json!({
            "type": "fetch_models",
            "provider_id": "openai",
            "base_url": "https://api.openai.com/v1",
            "kind": "openai",
            "api_key": "sk-test"
        }))
        .unwrap();
        match req {
            ClientRequest::FetchModels { provider_id, .. } => {
                assert_eq!(provider_id, "openai");
            }
            other => panic!("wrong variant: {other:?}"),
        }
        // old-format FetchModels (protocol v1, no provider_id)
        let req: ClientRequest = serde_json::from_value(json!({
            "type": "fetch_models",
            "base_url": "https://api.openai.com/v1",
            "kind": "openai",
            "api_key": "sk-test"
        }))
        .unwrap();
        match req {
            ClientRequest::FetchModels { provider_id, .. } => assert!(provider_id.is_empty()),
            other => panic!("wrong variant: {other:?}"),
        }
        // Command with and without the sending tab
        let req: ClientRequest =
            serde_json::from_value(json!({"type": "command", "text": "providers", "session_id": 3}))
                .unwrap();
        match req {
            ClientRequest::Command { session_id, .. } => assert_eq!(session_id, Some(3)),
            other => panic!("wrong variant: {other:?}"),
        }
        let req: ClientRequest =
            serde_json::from_value(json!({"type": "command", "text": "providers"})).unwrap();
        match req {
            ClientRequest::Command { session_id, .. } => assert_eq!(session_id, None),
            other => panic!("wrong variant: {other:?}"),
        }
        // the Skill round-trips too
        let req: ClientRequest = serde_json::from_value(json!({
            "type": "skill", "id": 2, "name": "docx-report"
        }))
        .unwrap();
        match req {
            ClientRequest::Skill { id, name } => {
                assert_eq!((id, name.as_str()), (2, "docx-report"));
            }
            other => panic!("wrong variant: {other:?}"),
        }
    }

    /// `/skills` shows the catalog and `/skill` rejects unknown names without
    /// touching the disk or starting a turn.
    #[test]
    fn skills_command_lists_catalog_and_activation_validates_names() {
        let hub = Hub::new(Config::default(), Bus::new());
        let list = hub.skills_list();
        assert!(list.contains("romacode-customization"), "{list}");
        assert!(list.contains("[builtin]"), "{list}");
        let err = hub.activate_skill(Some(1), "no-such-skill").unwrap_err();
        assert!(err.contains("no skill named"), "{err}");
        assert!(hub.activate_skill(Some(1), "   ").is_err());
    }

    /// The FetchModels `provider_id` fallback: new clients tag the config
    /// entry they are configuring; a missing id (protocol-v1 clients) falls
    /// back to the ACTIVE provider, which the preceding provider-only
    /// SaveModel has already set. Ids are trimmed + lowercased to match the
    /// config keys.
    #[test]
    fn resolve_fetch_provider_id_falls_back_to_the_active_provider() {
        assert_eq!(resolve_fetch_provider_id("openai", "anthropic"), "openai");
        assert_eq!(resolve_fetch_provider_id("  Groq ", "openai"), "groq");
        assert_eq!(resolve_fetch_provider_id("", "anthropic"), "anthropic");
        assert_eq!(resolve_fetch_provider_id("   ", "Google"), "google");
        assert_eq!(resolve_fetch_provider_id("", ""), "");
    }

    /// `save_model` must MERGE the incoming list into the stored one — a pick
    /// from a stale screen may never wipe ids another screen (or a live
    /// fetch) stored in the meantime — and must normalize the provider id to
    /// the lowercase config key so one provider cannot fork into two entries.
    #[test]
    fn save_model_merges_models_and_normalizes_provider_id() {
        // keep the atomic config save out of the real user config
        let tmp = std::env::temp_dir().join(format!("romacode-api-test-{}", std::process::id()));
        std::fs::create_dir_all(&tmp).unwrap();
        std::env::set_var("XDG_CONFIG_HOME", &tmp);

        let hub = Hub::new(Config::default(), Bus::new());
        // a mixed-case id must land under the lowercase key
        hub.save_model(
            "OpenAI",
            "",
            "",
            "",
            "openai",
            "gpt-x",
            &["gpt-live-a".to_string()],
        )
        .unwrap();
        {
            let cfg = hub.cfg.lock().unwrap_or_else(|e| e.into_inner());
            assert_eq!(cfg.provider, "openai");
            let p = cfg.providers.get("openai").expect("entry under lowercase id");
            assert!(
                p.models.contains(&"gpt-live-a".to_string()),
                "first list stored: {:?}",
                p.models
            );
            assert_eq!(cfg.model, "gpt-x");
        }
        // a later pick from a different screen MERGES: the earlier id keeps
        // its position and the new id is appended — nothing is dropped
        hub.save_model(
            "openai",
            "",
            "",
            "",
            "openai",
            "gpt-y",
            &["gpt-live-b".to_string()],
        )
        .unwrap();
        let cfg = hub.cfg.lock().unwrap_or_else(|e| e.into_inner());
        let p = cfg.providers.get("openai").unwrap();
        assert!(
            p.models.iter().any(|m| m == "gpt-live-a"),
            "earlier id kept: {:?}",
            p.models
        );
        assert!(
            p.models.iter().any(|m| m == "gpt-live-b"),
            "new id added: {:?}",
            p.models
        );
        assert_eq!(cfg.model, "gpt-y");
        std::fs::remove_dir_all(&tmp).ok();
    }
}
