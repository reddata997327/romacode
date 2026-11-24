//! roxterm wire protocol.
//!
//! Everything a client (agent or human) and the daemon exchange is a
//! [`Request`] answered by a [`Response`], framed with
//! [`framing::encode_frame`] / [`framing::try_decode_frame`] (length-prefixed
//! postcard). The same types are shared by the native viewer and the
//! runbook executor, so the daemon speaks exactly one protocol.

pub const PROTOCOL_VERSION: u32 = 5;

/// Where the daemon listens: UDS path on Unix, TCP loopback addr on Windows.
#[derive(Debug, Clone)]
pub enum Endpoint {
    #[cfg(unix)]
    Uds(std::path::PathBuf),
    #[cfg(windows)]
    Tcp(String),
}

pub const WINDOWS_DEFAULT_PORT: u16 = 47761;

pub mod client;

/// Process exit codes used by the `roxterm` CLI. Agents branch on these.
pub mod exit {
    pub const OK: i32 = 0;
    pub const FAILED: i32 = 1;
    pub const TIMEOUT: i32 = 2;
    pub const ASSERT_FAILED: i32 = 3;
    pub const USAGE: i32 = 4;
    pub const DAEMON_UNREACHABLE: i32 = 5;
}

/// Cell rendering attributes carried through the grid model.
pub mod flags {
    pub const BOLD: u8 = 1 << 0;
    pub const ITALIC: u8 = 1 << 1;
    pub const UNDERLINE: u8 = 1 << 2;
    pub const REVERSE: u8 = 1 << 3;
    pub const STRIKEOUT: u8 = 1 << 4;
    pub const DIM: u8 = 1 << 5;
    pub const HIDDEN: u8 = 1 << 6;
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub enum WaitKind {
    /// A line containing `needle` appears on screen or in new scrollback.
    Text,
    /// A TCP port accepts connections (probe loop).
    Port,
    /// OSC 133 shell prompt appeared (shell is idle, ready for input).
    Prompt,
    /// No output for the given duration.
    Idle,
    /// The current OSC 133 command produced an exit code.
    Exit,
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct WaitSpec {
    pub kind: WaitKind,
    pub needle: Option<String>,
    pub timeout_ms: u64,
    /// Exit code to match when kind == Exit.
    pub exit_code: Option<i32>,
    /// 0.8.0: sentinel tag for content-addressed exit waits. The `run`
    /// handler tags its sentinel D (`OSC 133;D;<exit>;<tag>`) and waits for
    /// THAT mark specifically, immune to stale prompt-time Ds from shell
    /// startup still in flight when the run started.
    #[serde(default)]
    pub tag: Option<String>,
    /// Scan the last N lines only (Text waits).
    pub tail: Option<u32>,
    /// Also scan scrollback, not just the live grid (Text waits).
    pub scan_scrollback: bool,
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct RunSpec {
    pub command: String,
    pub tag: Option<String>,
    /// Max wall time; on timeout the run reports failure, shell survives.
    pub timeout_ms: Option<u64>,
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct ReadSpec {
    pub diff: bool,
    /// Diff baseline: return rows touched after this seq (0 = everything).
    pub since: u64,
    pub tail: Option<u32>,
    /// Max characters returned; the report notes how much was dropped.
    pub budget: Option<u32>,
    pub include_images: bool,
    /// Substring filter (case-sensitive, server-side).
    pub grep: Option<String>,
    pub scan_scrollback: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub enum TunnelRole {
    /// -L : listen locally, forward to target through the session host.
    Local,
    /// -R : listen remotely, forward back to a local target.
    Remote,
    /// Plain in-daemon TCP relay (no ssh): bind local, stream to target.
    Relay,
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct TunnelSpec {
    pub role: TunnelRole,
    /// Bind address for the listener end, e.g. 127.0.0.1:8080.
    pub bind: String,
    /// Target host:port packets are delivered to.
    pub target: String,
    /// When set, the tunnel rides `ssh -N` to this host (user@host).
    pub via_ssh: Option<String>,
    /// Extra ssh args (-i key, -J jump, ...).
    pub ssh_args: Vec<String>,
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct ImagePlacement {
    pub image_id: u32,
    pub image_number: u32,
    pub col: u32,
    pub row: u32,
    pub width_cells: u32,
    pub height_cells: u32,
    pub pixel_w: u32,
    pub pixel_h: u32,
}

/// One row of the live grid as returned by `read`.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct GridLine {
    pub seq: u64,
    /// Absolute scrollback-relative row index.
    pub index: i64,
    pub text: String,
    /// True when the row carries non-default attributes.
    pub colored: bool,
    /// Per-cell attr flags, same length as `text` cells (chars).
    pub attrs: Vec<u8>,
    pub image: Option<ImagePlacement>,
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct ReadReport {
    pub session: String,
    pub seq: u64,
    pub cursor: (u32, u32),
    pub mode_bits: u32,
    pub lines: Vec<GridLine>,
    /// How many chars were dropped to satisfy the budget.
    pub dropped: u32,
    pub dropped_bytes: u32,
    pub truncated_head: bool,
    pub images: Vec<ImageMeta>,
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct ImageMeta {
    pub image_id: u32,
    pub pixel_w: u32,
    pub pixel_h: u32,
    pub size_bytes: u32,
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct SessionInfo {
    pub id: String,
    pub tag: Option<String>,
    pub shell: String,
    pub cwd: String,
    pub title: String,
    pub created_unix_ms: u64,
    pub pid: u32,
    pub width: u32,
    pub height: u32,
    pub seq: u64,
    pub last_exit: Option<i32>,
    pub alive: bool,
    /// Bumped each time the daemon restarts and replays this journal.
    pub generation: u32,
    pub journal_bytes: u64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub enum SnapshotFormat {
    Svg,
    Text,
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct SnapshotSvg {
    pub svg: String,
    pub width: u32,
    pub height: u32,
    pub sha256: String,
}

// ------------------------------------------------------------ viewer wire

/// Color encoding for viewer wire: 0 = default, 0x4000_0000|n = 256-index,
/// 0x8000_0000|0xRRGGBB = truecolor.
#[inline]
pub fn encode_color(kind: u8, r: u8, g: u8, b: u8) -> u32 {
    match kind {
        1 => 0x4000_0000 | r as u32,
        2 => 0x8000_0000 | ((r as u32) << 16) | ((g as u32) << 8) | b as u32,
        _ => 0,
    }
}

#[derive(Debug, Clone, Copy, serde::Serialize, serde::Deserialize)]
pub struct CellWire {
    pub ch: char,
    pub fg: u32,
    pub bg: u32,
    pub flags: u8,
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct RowWire {
    /// Absolute scrollback-space row index.
    pub index: i64,
    pub touch: u64,
    pub cells: Vec<CellWire>,
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct ImageWire {
    pub image_id: u32,
    pub pixel_w: u32,
    pub pixel_h: u32,
    pub rgba: Vec<u8>,
}

// ------------------------------------------------------- http capture wire
//
// The capture proxy is the "login-once" gateway: agents point every HTTP
// call at a local port, the daemon forwards to the upstream (often through
// a tunnel) and records request + response pairs as receipts.

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct CaptureStartSpec {
    /// Bind address for the proxy, e.g. 127.0.0.1:18080.
    pub listen: String,
    /// Upstream origin, e.g. http://10.0.0.5:8080 (https not proxied).
    pub upstream: String,
    #[serde(default = "default_true")]
    pub record_bodies: bool,
    /// Max bytes of each body kept in a receipt (rest is counted, not stored).
    pub max_body: u32,
    /// 0.7.0 pinned-CA mode: path or PEM body of a CA the upstream origin
    /// cert must chain to. When set, the proxy VERIFIES the upstream
    /// (standard webpki) instead of accepting any certificate.
    #[serde(default)]
    pub pinned_ca: Option<String>,
}

fn default_true() -> bool {
    true
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct CaptureInfo {
    pub capture_id: String,
    pub listen: String,
    pub upstream: String,
    pub entries: u64,
    pub bytes_req: u64,
    pub bytes_resp: u64,
    pub record_bodies: bool,
    pub alive: bool,
    /// Upstream dialed over TLS (https:// origin); CONNECT MITM receipts
    /// additionally carry per-entry `tls` flags.
    #[serde(default)]
    pub tls: bool,
    /// 0.7.0: upstream TLS is verified against a pinned CA (path given at
    /// `capture start --ca`) instead of the accept-any capture stance.
    #[serde(default)]
    pub pinned_ca: bool,
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct CaptureEntry {
    pub id: u64,
    pub capture_id: String,
    pub ts_unix_ms: u64,
    pub method: String,
    pub path: String,
    pub status: u16,
    pub req_headers: Vec<(String, String)>,
    pub resp_headers: Vec<(String, String)>,
    pub req_body_b64: String,
    pub resp_body_b64: String,
    pub duration_ms: u64,
    /// True when this call was seen through TLS termination (CONNECT MITM or
    /// an https upstream), i.e. the receipt holds the decrypted payload.
    #[serde(default)]
    pub tls: bool,
}

/// A first-class HTTP call issued BY the daemon through a capture proxy:
/// every runbook `http:` step rides this, so login responses are recorded.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct HttpRequestSpec {
    /// Capture id; None = the sole live capture.
    pub capture: Option<String>,
    pub method: String,
    pub path: String,
    pub headers: Vec<(String, String)>,
    pub body: Option<String>,
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct HttpReply {
    pub capture_id: String,
    pub entry_id: u64,
    pub status: u16,
    pub headers: Vec<(String, String)>,
    pub body: String,
    pub duration_ms: u64,
}

// ------------------------------------------------------------ db view wire

#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub enum DbDriver {
    Auto,
    Sqlite3,
    Postgres,
    MySQL,
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct DbExecSpec {
    pub dsn: String,
    pub sql: String,
    pub driver: DbDriver,
    pub timeout_ms: u64,
    pub max_rows: u32,
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct DbResult {
    pub driver: DbDriver,
    pub columns: Vec<String>,
    pub rows: Vec<Vec<String>>,
    pub rows_affected: u64,
    pub duration_ms: u64,
    pub truncated: bool,
    pub error: Option<String>,
}

// -------------------------------------------------------- frames (splits)

/// Where a new pane lands relative to the focused one.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub enum FrameDir {
    Left,
    Right,
    Top,
    Bottom,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub enum FrameKind {
    Term,
    Http,
    Db,
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub enum FrameSource {
    Session {
        session: String,
    },
    Capture {
        capture: Option<String>,
    },
    Db {
        dsn: String,
        table: String,
        sql: Option<String>,
        limit: u32,
    },
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct FrameSpec {
    pub dir: FrameDir,
    pub source: FrameSource,
    /// Relative size of the new pane vs the focused one (1.0 = equal split).
    pub weight: f32,
    pub title: Option<String>,
}

/// rect = (x, y, w, h) in workspace grid cells (virtual 240x60 canvas).
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct FrameInfo {
    pub id: String,
    pub workspace: String,
    pub kind: FrameKind,
    pub source: String,
    pub session: Option<String>,
    pub title: String,
    pub rect: (u32, u32, u32, u32),
    pub focused: bool,
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct FrameRenderData {
    pub frame_id: String,
    pub kind: FrameKind,
    pub cols: u32,
    pub rows: u32,
    pub lines: Vec<String>,
    pub cursor: Option<(u32, u32)>,
}

// ---------------------------------------------------------------- requests

/// Screen mode: primary grid vs alternate screen (vim, less, htop).
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub enum SessionMode {
    Normal,
    Alternate,
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub enum Request {
    Hello,
    /// Create a session, inject OSC 133 shell integration.
    Open {
        shell: String,
        tag: Option<String>,
        cwd: Option<String>,
        env: Vec<(String, String)>,
        cols: u32,
        rows: u32,
        keep: bool,
        detached: bool,
    },
    List,
    Close {
        session: String,
    },
    Signal {
        session: String,
        sig: String,
    },
    Resize {
        session: String,
        cols: u32,
        rows: u32,
    },
    /// Write bytes to the PTY (text + newline handling handled by CLI).
    Input {
        session: String,
        bytes: Vec<u8>,
        press_enter: bool,
    },
    /// Special-key input ("enter", "c-c", "tab", ...).
    Key {
        session: String,
        key: String,
    },
    Run {
        session: String,
        spec: RunSpec,
    },
    Wait {
        session: String,
        spec: WaitSpec,
    },
    Read {
        session: String,
        spec: ReadSpec,
    },
    Snapshot {
        session: String,
        format: SnapshotFormat,
        diff_since: Option<u64>,
    },
    /// Viewer-grade incremental grid state (full cell colors).
    RenderState {
        session: String,
        since: u64,
    },
    /// Fetch pixel data for one kitty image.
    Image {
        session: String,
        image_id: u32,
    },
    /// Subscribe: daemon pushes frames (output events) until client disconnects.
    Stream {
        session: String,
        from_seq: Option<u64>,
    },
    TunnelOpen {
        session: Option<String>,
        spec: TunnelSpec,
    },
    TunnelList,
    TunnelClose {
        tunnel_id: String,
    },
    JournalVerify {
        session: String,
    },
    /// Kill daemon; journals persist for replay.
    Shutdown,

    // ---- 0.4.0 capture / db / frames ----
    CaptureStart {
        spec: CaptureStartSpec,
    },
    CaptureStop {
        capture_id: String,
    },
    CaptureList,
    CaptureShow {
        capture_id: Option<String>,
        /// Specific entry id, `last`, or tail-of-log when None.
        entry: Option<u64>,
        tail: Option<u32>,
        include_body: bool,
    },
    /// Poll for a captured call matching path/status (login-once receipt).
    CaptureWait {
        capture_id: Option<String>,
        path_contains: Option<String>,
        status: Option<u16>,
        since_entry: u64,
        timeout_ms: u64,
    },
    /// Daemon issues the HTTP call through the capture proxy (recorded).
    HttpRequest {
        spec: HttpRequestSpec,
    },
    /// Read/write view over a database reached through a tunnel.
    DbExec {
        spec: DbExecSpec,
    },
    DbTables {
        dsn: String,
        driver: DbDriver,
        timeout_ms: u64,
    },
    /// Split the workspace: first split creates the workspace.
    FrameSplit {
        workspace: Option<String>,
        spec: FrameSpec,
    },
    FrameList {
        workspace: Option<String>,
    },
    FrameClose {
        frame_id: String,
    },
    /// Text view of one pane (http/db panes render their source live).
    FrameRender {
        frame_id: String,
    },

    // ---- 0.5.0 https interception / workspaces / capture CA ----
    /// Local CA used to sign MITM leaf certs (install into trust stores).
    CaptureCa,
    /// Create a named workspace (tab). Errors when it already exists.
    WorkspaceNew {
        name: String,
    },
    WorkspaceList,
    /// Switch the daemon-active workspace (None = active is listed first).
    WorkspaceUse {
        name: String,
    },
    /// Close a workspace; its panes die with it. The last workspace stays.
    WorkspaceClose {
        name: String,
    },

    // ---- 0.6.0 websocket frame receipting ----
    /// Daemon opens a WebSocket, exchanges the scripted frames and records
    /// every frame into the capture log (receipting like `http:` steps).
    WsRun {
        spec: WsRunSpec,
    },

    // ---- 0.7.0 long-lived ws connections / pinned CA / stream backfill ----
    /// Open a NAMED, daemon-owned WebSocket that stays connected across RPC
    /// calls and runbook steps. Frames stream into the capture log and a
    /// per-connection receipt buffer that `WsRecv` drains in order.
    WsOpen {
        spec: WsOpenSpec,
    },
    /// Send text frames on a named connection.
    WsSend {
        name: String,
        frames: Vec<String>,
    },
    /// Wait for / drain received frames on a named connection.
    WsRecv {
        name: String,
        expect: Option<String>,
        max_frames: u32,
        timeout_ms: u64,
        from: u32,
    },
    /// Close a named connection politely (close frame + status flip).
    WsClose {
        name: String,
    },
    /// List every daemon-owned WebSocket connection.
    WsConns,

    // ---- 0.8.0 agent-driven subs / bracketed paste / OSC 52 clipboard ----
    /// Write to the PTY as a CLIPBOARD PASTE: when the running app enabled
    /// bracketed-paste mode (DECSET 2004) the daemon wraps the bytes in
    /// `ESC[200~ … ESC[201~` so multiline pastes arrive as one input event
    /// instead of N enter-pressed lines. Without the mode the bytes go
    /// through unchanged.
    Paste {
        session: String,
        bytes: Vec<u8>,
        press_enter: bool,
    },
    /// Read the last OSC 52 clipboard payload the app published into the
    /// grid (`ESC ] 52 ; c ; <base64> ST`). The clipboard is a queryable
    /// cell of the grid database, like the command timeline.
    Clipboard {
        session: String,
    },

    // ---- 1.0.0 offline audit / live subscriptions / provenance ----
    /// Export a journal (session, or the workspace-layout journal with
    /// `workspaces: true`) for OFFLINE AUDIT: the full hash-chained record
    /// list plus the chain verdict in one reply. The export outlives the
    /// daemon — agents archive it, diff it, re-verify it elsewhere.
    JournalExport {
        session: String,
        workspaces: bool,
    },
    /// Turn THIS connection into a one-way push feed for a named ws
    /// connection: every frame the ORIGIN sends lands here as it arrives
    /// (live subscription, no polling). Unknown conn → normal error reply;
    /// after registration the daemon never replies in-band again — every
    /// frame read is a pushed [`Response::WsFrame`], terminated by
    /// [`Response::WsConnClosed`].
    StreamFrames {
        conn: String,
    },
    /// The command-provenance query: the OSC 133 timeline (prompt →
    /// command → exit marks), newest LAST, capped to `last` entries. The
    /// grid database answers "what ran here, what did it cost".
    Timeline {
        session: String,
        last: Option<u32>,
    },
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct ErrorBody {
    pub code: i32,
    pub message: String,
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub enum Response {
    Pong {
        version: u32,
        sessions: u32,
    },
    SessionOpened(SessionInfo),
    Sessions(Vec<SessionInfo>),
    Closed {
        session: String,
    },
    Ok,
    ExitCode {
        session: String,
        code: i32,
    },
    Read(ReadReport),
    Snapshot(SnapshotSvg),
    RenderState(RenderStateWire),
    Image(ImageWire),
    SignalSent,
    TunnelOpened {
        tunnel_id: String,
        bind: String,
    },
    Tunnels(Vec<TunnelInfo>),
    TunnelClosed {
        tunnel_id: String,
    },
    JournalOk {
        session: String,
        records: u64,
        chain_ok: bool,
    },
    /// Stream channel: PTY output chunks with seq numbers.
    Output {
        session: String,
        seq: u64,
        bytes: Vec<u8>,
    },
    /// Stream channel: session resumed after daemon restart + journal replay.
    Resumed {
        session: String,
        generation: u32,
    },
    Error(ErrorBody),

    // ---- 0.4.0 capture / db / frames (appended: stable variant order) ----
    CaptureStarted(CaptureInfo),
    CaptureStopped {
        capture_id: String,
    },
    Captures(Vec<CaptureInfo>),
    CaptureEntries {
        entries: Vec<CaptureEntry>,
        truncated: bool,
    },
    CaptureMatched(CaptureEntry),
    HttpReply(HttpReply),
    DbDone(DbResult),
    Frames(Vec<FrameInfo>),
    FrameClosed {
        frame_id: String,
    },
    FrameData(FrameRenderData),

    // ---- 0.5.0 https interception / workspaces / capture CA ----
    CaptureCaInfo {
        path: String,
        pem: String,
        sha256: String,
    },
    Workspaces(Vec<WorkspaceInfo>),
    WorkspaceClosed {
        name: String,
    },

    // ---- 0.6.0 websocket frame receipting ----
    WsReport(WsRunReport),

    // ---- 0.7.0 long-lived ws connections ----
    WsOpened {
        name: String,
        url: String,
        adopted: bool,
    },
    WsSent {
        name: String,
        frames_sent: u32,
    },
    WsRecvDone(WsRecvReport),
    WsConnClosed {
        name: String,
        frames_received: u32,
    },
    WsConns(Vec<WsConnView>),

    // ---- 0.8.0 bracketed paste / OSC 52 clipboard ----
    /// The last OSC 52 payload decoded (None = nothing published yet).
    ClipboardText {
        session: String,
        text: Option<String>,
        seq: u64,
    },

    // ---- 1.0.0 offline audit / live subscriptions / provenance ----
    /// Journal export: the full hash-chained record list + chain verdict.
    /// `records` is ordered oldest → newest; the last record's hash is the
    /// chain tip an offline verifier checks against.
    JournalData {
        name: String,
        chain_ok: bool,
        records: Vec<JournalRecordWire>,
    },
    /// Push feed ([`Request::StreamFrames`]): one frame the ORIGIN sent on
    /// the followed connection, as it arrived. `index` is the connection's
    /// buffer position — the same index `ws_recv --from` addresses, so a
    /// feed consumer and a poller agree on ordering.
    WsFrame {
        conn: String,
        index: u64,
        dir: String,
        opcode: u16,
        text: String,
        bytes: u32,
    },
    /// Push feed terminal event: the followed connection closed (or errored).
    /// No further frames arrive on the feed after this.
    WsFeedClosed {
        conn: String,
        reason: Option<String>,
    },
    /// Command-provenance query result: the OSC 133 timeline, oldest →
    /// newest, capped to the requested `last` entries.
    Timeline {
        session: String,
        marks: Vec<TimelineMark>,
    },
}

/// One journal record on the wire (1.0.0 offline audit). The payload is
/// pre-serialized JSON: the payload enums live in rox-journal and the wire
/// keeps this crate independent of it; the hashes are hex so an export is
/// greppable by any auditor.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct JournalRecordWire {
    pub seq: u64,
    pub prev_hash: String,
    pub payload_json: String,
    pub hash: String,
}

/// One completed OSC 133 command on the wire (1.0.0 provenance query).
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct TimelineMark {
    pub seq_prompt: u64,
    pub seq_end: u64,
    pub command: Option<String>,
    pub exit: Option<i32>,
    pub started_unix_ms: u64,
    pub ended_unix_ms: u64,
    pub row_start: u32,
    /// Run-sentinel tag (0.8.0) — `run-N` for daemon `run` commands.
    pub tag: Option<String>,
}

// ------------------------------------------------- websocket receipting (0.6.0)

/// One scripted WebSocket exchange. The daemon connects to `url`, sends
/// every `send` frame, then receipts incoming frames until `expect` matches,
/// `max_frames` arrive or `timeout_ms` elapses. Frames land in the capture
/// log (`capture show`) exactly like HTTP calls.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct WsRunSpec {
    /// ws://host:port/path or wss://host:port/path.
    pub url: String,
    /// Extra handshake headers (Authorization, Origin, ...).
    #[serde(default)]
    pub headers: Vec<(String, String)>,
    /// Text frames to send after the handshake, in order.
    #[serde(default)]
    pub send: Vec<String>,
    /// Succeed as soon as a received text frame contains this substring.
    pub expect: Option<String>,
    /// Cap on received frames receipted per run (default 8).
    pub max_frames: u32,
    /// Overall deadline for the whole exchange.
    pub timeout_ms: u64,
    /// wss:// only: accept any origin certificate (the daemon is the trust
    /// boundary for captured traffic; matches the capture proxy stance).
    #[serde(default)]
    pub insecure: bool,
    /// Capture log to record frames into; None = the sole live capture.
    pub capture: Option<String>,
    /// Path or PEM body of a CA the origin cert must chain to (0.7.0
    /// pinned-CA mode). When set, standard webpki verification applies
    /// instead of the accept-any capture-CA config.
    #[serde(default)]
    pub pinned_ca: Option<String>,
}

/// Open a persistent, daemon-owned WebSocket connection (0.7.0). Unlike
/// [`WsRunSpec`] (one scripted exchange per call), the connection survives
/// across RPC calls and runbook steps: `ws_open` → `ws_send` → `ws_recv`…
/// → `ws_close`. Every frame — both directions — is receipted into the
/// capture log while the connection lives.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct WsOpenSpec {
    /// Unique connection name (agents address it from later steps).
    pub name: String,
    /// ws://host:port/path or wss://host:port/path.
    pub url: String,
    /// Extra handshake headers (Authorization, Origin, ...).
    #[serde(default)]
    pub headers: Vec<(String, String)>,
    /// Connect + handshake deadline.
    pub timeout_ms: u64,
    /// wss:// only: accept any origin certificate (daemon trust boundary).
    #[serde(default)]
    pub insecure: bool,
    /// Path or PEM body of a CA the origin cert must chain to. When set,
    /// standard webpki verification replaces the accept-any stance — this
    /// is the pinned-CA mode: no trust in arbitrary self-signed origins.
    #[serde(default)]
    pub pinned_ca: Option<String>,
    /// Capture log to receipt frames into; None = the sole live capture.
    pub capture: Option<String>,
    /// 0.8.0 agent-driven subscriptions: when a daemon-held connection with
    /// this name already exists and is open, ADOPT it instead of dialing —
    /// the buffer, capture receipts and stream continuity all carry over, so
    /// a runbook can reattach mid-run to a subscription the CLI (or another
    /// runbook) opened earlier. Missing → dial fresh; open but dead → dial
    /// fresh and replace. `false` keeps the 0.7.0 duplicate-name error.
    #[serde(default)]
    pub adopt: bool,
}

/// Result of draining a named connection's received frames (0.7.0).
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct WsRecvReport {
    /// The frames returned by THIS call (subset of the connection buffer).
    pub frames: Vec<WsFrameView>,
    /// Index (0-based, recv order on the connection) of the frame that
    /// satisfied `expect`, if one was given.
    pub matched: Option<u32>,
    /// Total frames received on the connection so far.
    pub received_total: u32,
    /// Index the next `ws_recv` should start from (`from` + frames.len()).
    pub next_index: u32,
    pub error: Option<String>,
}

/// One named WebSocket connection as listed by `WsConns` (0.7.0).
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct WsConnView {
    pub name: String,
    pub url: String,
    /// open | closed | error.
    pub status: String,
    pub frames_in: u32,
    pub frames_out: u32,
    pub opened_unix_ms: u64,
    pub error: Option<String>,
}

/// One receipted frame as reported back to the client.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct WsFrameView {
    /// "send" (client→origin) or "recv" (origin→client).
    pub dir: String,
    /// RFC 6455 opcode: 1 text, 2 binary, 8 close, 9 pong.
    pub opcode: u16,
    /// Payload as text (lossy for binary frames).
    pub text: String,
    pub bytes: u32,
    /// Milliseconds since the socket connected.
    pub offset_ms: u64,
}

/// Result of one `WsRun`: every frame + where the receipts landed.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct WsRunReport {
    /// Capture that holds the frame receipts (None = nothing was recording).
    pub capture_id: Option<String>,
    /// Id of the first recorded frame receipt (sequential ids follow).
    pub entry_from: Option<u64>,
    pub frames_sent: u32,
    pub frames_received: u32,
    /// Index (0-based, recv order) of the frame that satisfied `expect`.
    pub matched: Option<u32>,
    pub frames: Vec<WsFrameView>,
    pub duration_ms: u64,
    /// Transport/handshake error, if any.
    pub error: Option<String>,
}

/// One named workspace (tab): a guillotine pane tree over term/http/db
/// sources. Workspaces are independent; the viewer shows them as tabs.
/// Layouts are journaled (0.6.0) so a daemon restart restores every tab.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct WorkspaceInfo {
    pub name: String,
    pub frames: u32,
    pub active: bool,
    pub created_unix_ms: u64,
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct RenderStateWire {
    pub session: String,
    pub seq: u64,
    pub cursor: (u32, u32),
    pub mode_bits: u32,
    pub cols: u32,
    pub rows: u32,
    pub scrollback_len: u32,
    /// Rows with touch seq > requested `since` (viewer merges by index).
    pub rows_changed: Vec<RowWire>,
    /// All live kitty images (RGB pixels). Bounded by 8 MiB per response.
    pub images: Vec<ImageWire>,
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct TunnelInfo {
    pub id: String,
    pub role: TunnelRole,
    pub bind: String,
    pub target: String,
    pub via_ssh: Option<String>,
    pub session: Option<String>,
    pub bytes_up: u64,
    pub bytes_down: u64,
    pub established_ms: u64,
    pub alive: bool,
}

#[derive(Debug, thiserror::Error)]
pub enum WireError {
    #[error("postcard encode/decode failed: {0}")]
    Postcard(#[from] postcard::Error),
    #[error("frame length prefix invalid")]
    BadFrame,
    #[error("frame exceeds {0} bytes")]
    TooLarge(u32),
}

/// Length-prefixed postcard framing over byte streams (UDS, TCP).
pub mod framing {
    use super::WireError;

    pub const MAX_FRAME: u32 = 64 * 1024 * 1024;

    /// `u32 LE length || postcard payload`
    pub fn encode_frame<T: serde::Serialize>(value: &T) -> Result<Vec<u8>, WireError> {
        let payload = postcard::to_allocvec(value)?;
        let len = payload.len() as u32;
        if len > MAX_FRAME {
            return Err(WireError::TooLarge(MAX_FRAME));
        }
        let mut out = Vec::with_capacity(4 + payload.len());
        out.extend_from_slice(&len.to_le_bytes());
        out.extend_from_slice(&payload);
        Ok(out)
    }

    /// Try to decode one frame from `buf`; returns frame size consumed.
    pub fn try_decode_frame<T: serde::de::DeserializeOwned>(
        buf: &[u8],
    ) -> Result<Option<(T, usize)>, WireError> {
        if buf.len() < 4 {
            return Ok(None);
        }
        let len = u32::from_le_bytes([buf[0], buf[1], buf[2], buf[3]]) as usize;
        if len > MAX_FRAME as usize {
            return Err(WireError::BadFrame);
        }
        if buf.len() < 4 + len {
            return Ok(None);
        }
        let value: T = postcard::from_bytes(&buf[4..4 + len])?;
        Ok(Some((value, 4 + len)))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn roundtrip_all_requests() {
        let reqs = vec![
            Request::Hello,
            Request::Open {
                shell: "bash".into(),
                tag: Some("dev".into()),
                cwd: None,
                env: vec![("FOO".into(), "bar".into())],
                cols: 80,
                rows: 24,
                keep: true,
                detached: true,
            },
            Request::Wait {
                session: "s1".into(),
                spec: WaitSpec {
                    kind: WaitKind::Text,
                    needle: Some("ready".into()),
                    timeout_ms: 3000,
                    exit_code: None,
                    tail: Some(20),
                    scan_scrollback: true,
                    tag: Some("run-1".into()),
                },
            },
            Request::TunnelOpen {
                session: None,
                spec: TunnelSpec {
                    role: TunnelRole::Relay,
                    bind: "127.0.0.1:0".into(),
                    target: "127.0.0.1:9".into(),
                    via_ssh: None,
                    ssh_args: vec![],
                },
            },
            Request::CaptureStart {
                spec: CaptureStartSpec {
                    listen: "127.0.0.1:18080".into(),
                    upstream: "http://10.0.0.5:8080".into(),
                    record_bodies: true,
                    max_body: 65536,
                    pinned_ca: None,
                },
            },
            Request::CaptureWait {
                capture_id: None,
                path_contains: Some("/login".into()),
                status: Some(200),
                since_entry: 0,
                timeout_ms: 5000,
            },
            Request::HttpRequest {
                spec: HttpRequestSpec {
                    capture: None,
                    method: "POST".into(),
                    path: "/login".into(),
                    headers: vec![("Content-Type".into(), "application/json".into())],
                    body: Some("{\"user\":\"agent\"}".into()),
                },
            },
            Request::DbExec {
                spec: DbExecSpec {
                    dsn: "postgres://app@127.0.0.1:5432/app".into(),
                    sql: "select 1".into(),
                    driver: DbDriver::Auto,
                    timeout_ms: 10_000,
                    max_rows: 100,
                },
            },
            Request::FrameSplit {
                workspace: None,
                spec: FrameSpec {
                    dir: FrameDir::Right,
                    source: FrameSource::Db {
                        dsn: "sqlite:app.db".into(),
                        table: "users".into(),
                        sql: None,
                        limit: 20,
                    },
                    weight: 1.0,
                    title: Some("db view".into()),
                },
            },
            // 0.5.0 variants.
            Request::CaptureCa,
            Request::WorkspaceNew {
                name: "db-review".into(),
            },
            Request::WorkspaceList,
            Request::WorkspaceUse {
                name: "db-review".into(),
            },
            Request::WorkspaceClose { name: "old".into() },
            // 0.7.0 variants.
            Request::WsOpen {
                spec: WsOpenSpec {
                    name: "events".into(),
                    url: "wss://origin.example/events".into(),
                    headers: vec![("Authorization".into(), "Bearer t".into())],
                    timeout_ms: 5000,
                    insecure: false,
                    pinned_ca: Some("/tmp/origin-ca.pem".into()),
                    capture: Some("c1".into()),
                    adopt: true,
                },
            },
            Request::WsSend {
                name: "events".into(),
                frames: vec!["hi".into(), "there".into()],
            },
            Request::WsRecv {
                name: "events".into(),
                expect: Some("build-ok".into()),
                max_frames: 4,
                timeout_ms: 8000,
                from: 2,
            },
            Request::WsClose {
                name: "events".into(),
            },
            Request::WsConns,
            // 0.8.0 variants.
            Request::Paste {
                session: "s1".into(),
                bytes: vec![1, 2, 3],
                press_enter: false,
            },
            Request::Clipboard {
                session: "s1".into(),
            },
            // 1.0.0 variants.
            Request::JournalExport {
                session: "s1".into(),
                workspaces: true,
            },
            Request::StreamFrames {
                conn: "events".into(),
            },
            Request::Timeline {
                session: "s1".into(),
                last: Some(20),
            },
        ];
        for req in reqs {
            let frame = framing::encode_frame(&req).unwrap();
            let (back, n) = framing::try_decode_frame::<Request>(&frame)
                .unwrap()
                .unwrap();
            assert_eq!(n, frame.len());
            // Re-encode both and compare — postcard output is deterministic here.
            let re = framing::encode_frame(&back).unwrap();
            assert_eq!(re, frame);
        }

        let resp = Response::Read(ReadReport {
            session: "s".into(),
            seq: 7,
            cursor: (0, 0),
            mode_bits: 0,
            lines: vec![GridLine {
                seq: 7,
                index: 0,
                text: "hello".into(),
                colored: false,
                attrs: vec![flags::BOLD; 5],
                image: None,
            }],
            dropped: 0,
            dropped_bytes: 0,
            truncated_head: false,
            images: vec![],
        });
        let frame = framing::encode_frame(&resp).unwrap();
        let (back, _) = framing::try_decode_frame::<Response>(&frame)
            .unwrap()
            .unwrap();
        let re = framing::encode_frame(&back).unwrap();
        assert_eq!(re, frame);

        // New 0.4.0 responses roundtrip too.
        let resps = vec![
            Response::CaptureMatched(CaptureEntry {
                id: 1,
                capture_id: "c1".into(),
                ts_unix_ms: 0,
                method: "POST".into(),
                path: "/login".into(),
                status: 200,
                req_headers: vec![],
                resp_headers: vec![("Set-Cookie".into(), "sid=abc".into())],
                req_body_b64: String::new(),
                resp_body_b64: String::new(),
                duration_ms: 12,
                tls: true,
            }),
            Response::HttpReply(HttpReply {
                capture_id: "c1".into(),
                entry_id: 1,
                status: 200,
                headers: vec![],
                body: "{\"token\":\"t\"}".into(),
                duration_ms: 9,
            }),
            Response::DbDone(DbResult {
                driver: DbDriver::Sqlite3,
                columns: vec!["one".into()],
                rows: vec![vec!["1".into()]],
                rows_affected: 0,
                duration_ms: 3,
                truncated: false,
                error: None,
            }),
            Response::Frames(vec![FrameInfo {
                id: "f1".into(),
                workspace: "main".into(),
                kind: FrameKind::Term,
                source: "session s1".into(),
                session: Some("s1".into()),
                title: "agent".into(),
                rect: (0, 0, 120, 60),
                focused: true,
            }]),
            Response::FrameData(FrameRenderData {
                frame_id: "f2".into(),
                kind: FrameKind::Db,
                cols: 80,
                rows: 20,
                lines: vec!["id | name".into()],
                cursor: None,
            }),
            // 0.5.0 variants.
            Response::CaptureCaInfo {
                path: "/tmp/ca.pem".into(),
                pem: "-----BEGIN".into(),
                sha256: "deadbeef".into(),
            },
            Response::Workspaces(vec![
                WorkspaceInfo {
                    name: "main".into(),
                    frames: 3,
                    active: true,
                    created_unix_ms: 42,
                },
                WorkspaceInfo {
                    name: "db-review".into(),
                    frames: 1,
                    active: false,
                    created_unix_ms: 43,
                },
            ]),
            Response::WorkspaceClosed { name: "old".into() },
            // 0.7.0 variants.
            Response::WsOpened {
                name: "events".into(),
                url: "ws://h/e".into(),
                adopted: true,
            },
            Response::WsSent {
                name: "events".into(),
                frames_sent: 2,
            },
            Response::WsRecvDone(WsRecvReport {
                frames: vec![WsFrameView {
                    dir: "recv".into(),
                    opcode: 1,
                    text: "build-ok".into(),
                    bytes: 8,
                    offset_ms: 42,
                }],
                matched: Some(0),
                received_total: 1,
                next_index: 1,
                error: None,
            }),
            Response::WsConnClosed {
                name: "events".into(),
                frames_received: 7,
            },
            Response::WsConns(vec![WsConnView {
                name: "events".into(),
                url: "wss://origin.example/events".into(),
                status: "open".into(),
                frames_in: 3,
                frames_out: 2,
                opened_unix_ms: 42,
                error: None,
            }]),
            // 0.8.0 variants.
            Response::ClipboardText {
                session: "s1".into(),
                text: Some("copied".into()),
                seq: 9,
            },
            // 1.0.0 variants.
            Response::JournalData {
                name: "s1".into(),
                chain_ok: true,
                records: vec![JournalRecordWire {
                    seq: 1,
                    prev_hash: "aa".into(),
                    payload_json: "{\"kind\":\"header\"}".into(),
                    hash: "bb".into(),
                }],
            },
            Response::WsFrame {
                conn: "events".into(),
                index: 3,
                dir: "recv".into(),
                opcode: 1,
                text: "hello".into(),
                bytes: 5,
            },
            Response::WsFeedClosed {
                conn: "events".into(),
                reason: Some("origin closed (1000)".into()),
            },
            Response::Timeline {
                session: "s1".into(),
                marks: vec![TimelineMark {
                    seq_prompt: 1,
                    seq_end: 9,
                    command: Some("echo hi".into()),
                    exit: Some(0),
                    started_unix_ms: 1,
                    ended_unix_ms: 2,
                    row_start: 0,
                    tag: Some("run-1".into()),
                }],
            },
        ];
        for resp in resps {
            let frame = framing::encode_frame(&resp).unwrap();
            let (back, _) = framing::try_decode_frame::<Response>(&frame)
                .unwrap()
                .unwrap();
            let re = framing::encode_frame(&back).unwrap();
            assert_eq!(re, frame);
        }
    }

    #[test]
    fn partial_frames_wait() {
        let frame = framing::encode_frame(&Request::Hello).unwrap();
        // Byte-by-byte feeding must eventually produce the frame.
        let mut buf: Vec<u8> = vec![];
        let mut got = None;
        for b in &frame {
            buf.push(*b);
            if let Some((_, n)) = framing::try_decode_frame::<Request>(&buf).unwrap() {
                got = Some(n);
                break;
            }
        }
        assert_eq!(got, Some(frame.len()));
    }
}
