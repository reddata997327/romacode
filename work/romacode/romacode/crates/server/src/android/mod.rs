//! Android device bridge. RomaCode's UI uses the official host `adb` client to
//! select and control an actual SDK emulator or authorized physical device.
//! The old in-process phone implementation remains separate for legacy tests;
//! it is never selected as a fallback by the application.

pub mod adb_cli;
pub mod setup;

use mandroid::adb::{AdbDevice, AdbOptions, ExecStream, InstallOptions, StreamReassembler};
use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{channel, Receiver, Sender};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::{Duration, Instant};

/// Percent-encode every byte that is not a URL-safe unreserved character using
/// the Android `input text` `%XX` convention.
///
/// The resulting string contains only `[A-Za-z0-9._~/-]` and `%` followed by
/// two hex digits — no shell metacharacters (`$`, backtick, `&`, `|`, `;`,
/// `<`, `>`, `"`, `'`, space, `\`). It is therefore safe to pass verbatim to
/// both the native ADB `shell:` service (`dev.shell("input text <encoded>")`)
/// and the external `adb shell input text <encoded>` CLI without any further
/// quoting layer. `input text` itself decodes `%XX` back to the raw byte
/// before injecting the resulting string into the focused IME, so the user
/// sees the original text.
fn shell_safe_text(s: &str) -> String {
    const HEX: &[u8; 16] = b"0123456789ABCDEF";
    let mut out = String::with_capacity(s.len());
    for (idx, &b) in s.as_bytes().iter().enumerate() {
        // `~` and a LEADING `-` are NOT safe: at the start of a device shell
        // word, `~` expands to $HOME and `-` parses as an option flag, so
        // typed text got mangled (or eaten) by mksh. `input text` decodes
        // %XX back to the raw byte, so percent-encoding them is exact.
        let safe = b.is_ascii_alphanumeric()
            || matches!(b, b'-' | b'_' | b'.' | b'/')
            || (b == b'~' && idx > 0);
        if safe && !(idx == 0 && b == b'-') {
            out.push(b as char);
        } else {
            out.push('%');
            out.push(HEX[(b >> 4) as usize] as char);
            out.push(HEX[(b & 0x0f) as usize] as char);
        }
    }
    out
}

/// One committed device frame (RGBA), ready for the pixel renderer.
/// The pixel buffer is shared by reference count — cloning a frame (which
/// happens every poll at streaming rate) never copies 4.5 MB of pixels.
#[derive(Clone)]
pub struct Frame {
    pub gen: u64,
    pub w: u32,
    pub h: u32,
    pub rgba: Arc<Vec<u8>>,
}

/// What the header line shows. Everything here is real device state.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Status {
    pub phase: Phase,
    pub serial: String,
    pub model: String,
    pub port: u16,
    pub root: String,
    pub display: (u32, u32),
    pub battery: u8,
    pub package_count: usize,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Phase {
    #[allow(dead_code)]
    Booting,
    Online,
    Error(String),
}

impl Default for Phase {
    fn default() -> Self {
        Self::Booting
    }
}

/// Commands the UI / agent threads send to the device thread.
pub enum Cmd {
    Tap(u32, u32),
    Swipe(u32, u32, u32, u32),
    Text(String),
    Key(phone::state::KeyCode),
    /// Run a shell command; the reply channel receives stdout or an error.
    Shell(String, Sender<Result<String, String>>),
    /// Raw exec: binary-safe stdout (screencap, file pulls).
    ExecRaw(String, usize, Sender<Result<Vec<u8>, String>>),
    /// Toggle frame streaming (panel open / fullscreen).
    Stream(bool),
}

/// Last device status published by the bridge, readable from the agent prompt
/// builder. The TUI sees `Event::Device`, but the agent never did — it had to
/// re-discover on its own and could report "no device" while the panel showed
/// one online. This shared snapshot closes that gap.
static LAST_STATUS: OnceLock<Mutex<Option<Status>>> = OnceLock::new();

fn status_cell() -> &'static Mutex<Option<Status>> {
    LAST_STATUS.get_or_init(|| Mutex::new(None))
}

fn publish_status(st: &Status) {
    if let Ok(mut cell) = status_cell().lock() {
        *cell = Some(st.clone());
    }
}

/// Latest known device status, or `None` before the bridge has reported.
pub fn last_status() -> Option<Status> {
    status_cell().lock().ok().and_then(|c| c.clone())
}

#[derive(Clone)]
pub(crate) struct Shared {
    status: Arc<Mutex<Option<Status>>>,
    frame: Arc<Mutex<Option<Frame>>>,
    gen: Arc<std::sync::atomic::AtomicU64>,
}

impl Shared {
    pub(crate) fn new() -> Self {
        Self {
            status: Arc::new(Mutex::new(None)),
            frame: Arc::new(Mutex::new(None)),
            gen: Arc::new(std::sync::atomic::AtomicU64::new(0)),
        }
    }

    fn set_status(&self, st: Status) {
        publish_status(&st);
        *self
            .status
            .lock()
            .unwrap_or_else(|e| e.into_inner()) = Some(st);
    }

    pub(crate) fn set_frame(&self, w: u32, h: u32, rgba: Arc<Vec<u8>>) {
        let gen = self.gen.fetch_add(1, std::sync::atomic::Ordering::Relaxed) + 1;
        *self
            .frame
            .lock()
            .unwrap_or_else(|e| e.into_inner()) = Some(Frame { gen, w, h, rgba });
    }

    /// Latest frame if newer than `seen_gen`.
    pub(crate) fn take_frame(&self, seen_gen: u64) -> Option<Frame> {
        let f = self
            .frame
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .clone()?;
        if f.gen != seen_gen {
            Some(f)
        } else {
            None
        }
    }

    /// The current device facts (the detached display's header line).
    #[allow(dead_code)] // used by tests and tooling
    pub(crate) fn current_status(&self) -> Option<Status> {
        self
            .status
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .clone()
    }
}

/// Android tool support is compiled in; the external device connection
/// remains user-configurable and can be disabled.
pub const PHONE_AVAILABLE: bool = true;

pub struct Android {
    pub tx: Sender<Cmd>,
    shared: Shared,
    /// Cached latest facts/frames; interior-mutable so the api `Arc<Hub>`
    /// and the frame streamer can drive the bridge without `&mut`.
    inner: Arc<Mutex<AndroidInner>>,
    pub demand: std::sync::Arc<std::sync::atomic::AtomicUsize>,
}

#[derive(Default)]
struct AndroidInner {
    status: Option<Status>,
    frame: Option<Frame>,
    streaming: bool,
}

/// Narrow, owned handle for background workers. It contains only the command
/// sender and synchronized status snapshot; it never borrows UI-owned state.
#[derive(Clone)]
pub struct AndroidAgent {
    tx: Sender<Cmd>,
    shared: Shared,
}

impl AndroidAgent {
    pub fn status(&self) -> Option<Status> {
        self.shared.status.lock().ok().and_then(|s| s.clone())
    }

    pub fn shell(&self, command: &str) -> Result<String, String> {
        let (reply, rx) = channel();
        self.tx
            .send(Cmd::Shell(command.to_owned(), reply))
            .map_err(|_| "Android bridge is unavailable".to_string())?;
        rx.recv_timeout(Duration::from_secs(60))
            .map_err(|_| "Android command timed out".to_string())?
    }

    pub fn screenshot_png(&self) -> Result<Vec<u8>, String> {
        let (reply, rx) = channel();
        self.tx
            .send(Cmd::ExecRaw("screencap -p".into(), 64 * 1024 * 1024, reply))
            .map_err(|_| "Android bridge is unavailable".to_string())?;
        rx.recv_timeout(Duration::from_secs(60))
            .map_err(|_| "Android screenshot timed out".to_string())?
    }

    pub fn tap(&self, x: u32, y: u32) -> Result<(), String> {
        self.tx
            .send(Cmd::Tap(x, y))
            .map_err(|_| "Android bridge is unavailable".to_string())
    }
    pub fn swipe(&self, x1: u32, y1: u32, x2: u32, y2: u32) -> Result<(), String> {
        self.tx
            .send(Cmd::Swipe(x1, y1, x2, y2))
            .map_err(|_| "Android bridge is unavailable".to_string())
    }
    pub fn text(&self, text: &str) -> Result<(), String> {
        self.tx
            .send(Cmd::Text(text.to_owned()))
            .map_err(|_| "Android bridge is unavailable".to_string())
    }
    pub fn key(&self, key: phone::state::KeyCode) -> Result<(), String> {
        self.tx
            .send(Cmd::Key(key))
            .map_err(|_| "Android bridge is unavailable".to_string())
    }
}

impl Android {
    pub fn agent_handle(&self) -> AndroidAgent {
        AndroidAgent {
            tx: self.tx.clone(),
            shared: self.shared.clone(),
        }
    }
    /// Map user settings into a real Android target selection.
    pub fn config_from(cfg: &crate::config::Config) -> phone::PhoneConfig {
        let a = &cfg.android;
        phone::PhoneConfig {
            external_adb: a.backend == "adb",
            adb_serial: a.serial.clone(),
            adb_path: a.adb_path.clone(),
            port: a.port,
            data_dir: phone::persist::data_dir(),
            display: (a.display[0], a.display[1]),
            density: a.density,
            native_target: a.native_target.clone(),
        }
    }

    /// Start the configured Android bridge. Returns immediately; the UI polls
    /// status and screenshots.
    pub fn spawn(cfg: phone::PhoneConfig) -> Self {
        let shared = Shared::new();
        let (tx, rx) = channel::<Cmd>();
        let shared_thread = Shared {
            status: shared.status.clone(),
            frame: shared.frame.clone(),
            gen: shared.gen.clone(),
        };
        let _ = std::thread::Builder::new()
            .name("android-bridge".into())
            .spawn(move || bridge(cfg, rx, shared_thread));
        // attached screens share this state: same compositor frames, same
        // command queue — one server, no MCP, no wrappers, no browser
        Self {
            tx,
            shared,
            inner: Arc::new(Mutex::new(AndroidInner::default())),
            demand: std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0)),
        }
    }

    /// Construct a non-running bridge when the user disables automatic device
    /// connections. It never falls back to the legacy simulator.
    pub fn disabled(reason: impl Into<String>) -> Self {
        let shared = Shared::new();
        let status = Status {
            phase: Phase::Error(reason.into()),
            model: "Android connection disabled".into(),
            ..Default::default()
        };
        shared.set_status(status.clone());
        let (tx, _rx) = channel::<Cmd>();
        Self {
            tx,
            shared,
            inner: Arc::new(Mutex::new(AndroidInner {
                status: Some(status),
                frame: None,
                streaming: false,
            })),
            demand: std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0)),
        }
    }

    /// Current device facts (clone of the cached status).
    pub fn status(&self) -> Option<Status> {
        self.inner
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .status
            .clone()
    }

    /// Latest committed frame (clone; RGBA pixels are shared by refcount).
    pub fn frame(&self) -> Option<Frame> {
        self.inner
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .frame
            .clone()
    }

    /// Poll the bridge: refresh status and pull a newer frame if any.
    /// Returns true when something changed (the caller marks the UI dirty).
    pub fn poll(&self) -> bool {
        let mut inner = self.inner.lock().unwrap_or_else(|e| e.into_inner());
        let mut changed = false;
        if let Ok(st) = self.shared.status.lock() {
            if *st != inner.status {
                inner.status = st.clone();
                changed = true;
            }
        }
        if inner.streaming {
            let seen = inner.frame.as_ref().map(|f| f.gen).unwrap_or(0);
            if let Some(f) = self.shared.take_frame(seen) {
                inner.frame = Some(f);
                changed = true;
            }
        }
        changed
    }

    pub fn tap(&self, x: u32, y: u32) {
        let _ = self.tx.send(Cmd::Tap(x, y));
    }

    pub fn swipe(&self, x1: u32, y1: u32, x2: u32, y2: u32) {
        let _ = self.tx.send(Cmd::Swipe(x1, y1, x2, y2));
    }

    pub fn text(&self, t: &str) {
        let _ = self.tx.send(Cmd::Text(t.to_string()));
    }

    pub fn key(&self, k: phone::state::KeyCode) {
        let _ = self.tx.send(Cmd::Key(k));
    }

    pub fn set_streaming(&self, on: bool) {
        let mut inner = self.inner.lock().unwrap_or_else(|e| e.into_inner());
        if inner.streaming != on {
            inner.streaming = on;
            let _ = self.tx.send(Cmd::Stream(on));
        }
    }

    /// Run a shell command synchronously (agent tools call this from their
    /// own thread, never from the UI loop).
    pub fn shell(&self, cmd: &str) -> Result<String, String> {
        let (rtx, rrx) = channel();
        self.tx
            .send(Cmd::Shell(cmd.to_string(), rtx))
            .map_err(|_| "android bridge is gone".to_string())?;
        rrx.recv_timeout(Duration::from_secs(60))
            .map_err(|_| "android bridge dropped the reply".to_string())?
    }

    /// Screenshot the device as PNG bytes (binary-safe raw exec).
    pub fn screenshot_png(&self) -> Result<Vec<u8>, String> {
        let (rtx, rrx) = channel();
        self.tx
            .send(Cmd::ExecRaw("screencap -p".into(), 64 * 1024 * 1024, rtx))
            .map_err(|_| "android bridge is gone".to_string())?;
        rrx.recv_timeout(Duration::from_secs(60))
            .map_err(|_| "android bridge dropped the reply".to_string())?
    }
}

/// Native ADB wire-protocol bridge — connects `mandroid::adb::AdbDevice`
/// straight to a real device or emulator at `cfg.native_target` and serves
/// the same `Cmd` channel the simulator bridge serves. No `phone::boot`, no
/// host `adb` binary — this is the path that finally makes the README's
/// "speaks the ADB wire protocol straight to adbd" claim literally true for
/// real external targets.
fn bridge_native_wire(cfg: phone::PhoneConfig, rx: Receiver<Cmd>, shared: Shared) {
    let target = cfg.native_target.clone();
    if target.is_empty() {
        shared.set_status(Status {
            phase: Phase::Error(
                "backend = \"native\" requires android.native_target = \"host:port\"".into(),
            ),
            ..Default::default()
        });
        return;
    }
    shared.set_status(Status {
        phase: Phase::Booting,
        model: "native target".into(),
        port: cfg.port,
        display: cfg.display,
        ..Default::default()
    });

    // Connect to the real device. Same retry shape as the simulator bridge,
    // minus the simulator boot — an SDK emulator's adbd is usually already
    // up by the time RomaCode starts.
    let mut dev = None;
    let mut connect_err = None::<String>;
    for attempt in 0..20u32 {
        match connect(&target) {
            Ok(d) => {
                dev = Some(d);
                break;
            }
            Err(e) => {
                connect_err = Some(format!("{e:#}"));
                if attempt < 19 {
                    std::thread::sleep(Duration::from_millis(300));
                }
            }
        }
    }
    let mut dev = match dev {
        Some(d) => d,
        None => {
            shared.set_status(Status {
                phase: Phase::Error(format!(
                    "native adb connect to {target}: {}",
                    connect_err.clone().unwrap_or_else(|| "no answer".into())
                )),
                ..Default::default()
            });
            // Bounded background retry (30 attempts, ~60 s) — if all fail,
            // surface a terminal error and let the thread die so a later
            // panel toggle can spawn a fresh bridge cleanly.
            let mut connected: Option<_> = None;
            for attempt in 0..30u32 {
                match connect(&target) {
                    Ok(d) => {
                        connected = Some(d);
                        break;
                    }
                    Err(e) => {
                        if attempt == 0 {
                            connect_err = Some(format!("{e:#}"));
                        }
                        let _ = &connect_err;
                        if attempt < 29 {
                            std::thread::sleep(Duration::from_millis(2000));
                        }
                    }
                }
            }
            match connected {
                Some(d) => d,
                None => {
                    shared.set_status(Status {
                        phase: Phase::Error(format!(
                            "native adb connect to {target}: gave up after 30 attempts ({})",
                            connect_err.clone().unwrap_or_else(|| "unknown".into())
                        )),
                        ..Default::default()
                    });
                    return;
                }
            }
        }
    };

    // Online — publish real device facts.
    let mut status = Status {
        phase: Phase::Online,
        serial: cfg.adb_serial.clone(),
        model: dev.info.model(),
        port: cfg.port,
        root: dev.root_status().as_str().to_string(),
        display: (0, 0),
        battery: 0,
        package_count: 0,
    };
    if let Ok(props) = dev.properties() {
        for (k, v) in props {
            match k.as_str() {
                "screen.width" => status.display.0 = v.parse().unwrap_or(0),
                "screen.height" => status.display.1 = v.parse().unwrap_or(0),
                _ => {}
            }
        }
    }
    if let Ok(pkgs) = dev.list_packages(None) {
        status.package_count = pkgs.len();
    }
    if status.display == (0, 0) {
        status.display = cfg.display;
    }
    {
        let out = dev.shell("dumpsys battery").unwrap_or_default();
        if let Some(level) = out
            .lines()
            .find(|l| l.trim().starts_with("level:"))
            .and_then(|l| l.split(':').nth(1))
            .and_then(|v| v.trim().parse::<u8>().ok())
        {
            status.battery = level;
        }
    }
    shared.set_status(status);

    // Serve commands + stream frames — same loop as the simulator bridge, but
    // without any phone.shutdown() to call on the way out (no simulator was
    // started).
    let mut streaming = false;
    let mut streamer: Option<Arc<AtomicBool>> = None;
    let mut stream_alive = Arc::new(AtomicBool::new(false));
    let mut last_frame = Instant::now() - Duration::from_secs(1);
    let frame_interval = Duration::from_millis(120);
    loop {
        match rx.try_recv() {
            Ok(Cmd::Tap(x, y)) => {
                let _ = dev.shell(&format!("input tap {x} {y}"));
            }
            Ok(Cmd::Swipe(x1, y1, x2, y2)) => {
                let _ = dev.shell(&format!("input swipe {x1} {y1} {x2} {y2} 180"));
            }
            Ok(Cmd::Text(t)) => {
                let encoded = shell_safe_text(&t);
                let _ = dev.shell(&format!("input text {encoded}"));
            }
            Ok(Cmd::Key(k)) => {
                let n = match k {
                    phone::state::KeyCode::Home => 3,
                    phone::state::KeyCode::Back => 4,
                    phone::state::KeyCode::Enter => 66,
                    phone::state::KeyCode::Del => 67,
                    phone::state::KeyCode::Power => 26,
                    phone::state::KeyCode::VolumeUp => 24,
                    phone::state::KeyCode::VolumeDown => 25,
                };
                let _ = dev.shell(&format!("input keyevent {n}"));
            }
            Ok(Cmd::ExecRaw(cmd, limit, reply)) => {
                let res = dev.exec_bytes(&cmd, limit).map_err(|e| format!("{e:#}"));
                let _ = reply.send(res);
            }
            Ok(Cmd::Shell(cmd, reply)) => {
                let res = if let Some(apk) = cmd.strip_prefix("__install ") {
                    dev.install(Path::new(apk), &InstallOptions::default())
                        .map(|r| {
                            format!(
                                "install: {}\nsuccess: {}\nstaged: {}\napk bytes: {}",
                                r.output.trim(),
                                r.success,
                                r.remote_path,
                                r.apk_bytes
                            )
                        })
                        .map_err(|e| format!("{e:#}"))
                } else {
                    dev.shell(&cmd).map_err(|e| format!("{e:#}"))
                };
                let _ = reply.send(res);
            }
            Ok(Cmd::Stream(on)) => {
                streaming = on;
                if let Some(f) = streamer.take() {
                    f.store(true, Ordering::Relaxed);
                }
                if on {
                    let stop = Arc::new(AtomicBool::new(false));
                    stream_alive = Arc::new(AtomicBool::new(false));
                    let (sh, tg, st_flag, alive) = (
                        shared.clone(),
                        target.clone(),
                        stop.clone(),
                        stream_alive.clone(),
                    );
                    let _ = std::thread::Builder::new()
                        .name("native-screen-stream".into())
                        .spawn(move || stream_loop(tg, sh, &st_flag, &alive));
                    streamer = Some(stop);
                }
            }
            Err(std::sync::mpsc::TryRecvError::Empty) => {}
            Err(std::sync::mpsc::TryRecvError::Disconnected) => {
                if let Some(f) = streamer.take() {
                    f.store(true, Ordering::Relaxed);
                }
                return;
            }
        }

        // Live frame pull when streaming and the dedicated streamer is not
        // alive (falls back to per-frame screencap, same as bridge()).
        if streaming && !stream_alive.load(Ordering::Relaxed) {
            if last_frame.elapsed() >= frame_interval {
                last_frame = Instant::now();
                if let Ok(png) = dev.screencap() {
                    if let Some((w, h, rgba)) = decode_png_rgba(&png) {
                        shared.set_frame(w, h, Arc::new(rgba));
                    }
                }
            }
        }

        std::thread::sleep(Duration::from_millis(16));
    }
}

/// The device thread: boots the phone, connects the native ADB client, then
/// serves commands + streams frames.
fn bridge(cfg: phone::PhoneConfig, rx: Receiver<Cmd>, shared: Shared) {
    if cfg.external_adb {
        bridge_adb_cli(cfg, rx, shared);
        return;
    }
    // Native wire-protocol branch: skip the in-process simulator and connect
    // straight to a real device's or emulator's adbd at `cfg.native_target`.
    // This is the path that finally makes the README's "speaks the ADB wire
    // protocol straight to adbd" claim literally true for real devices.
    if !cfg.native_target.is_empty() {
        bridge_native_wire(cfg, rx, shared);
        return;
    }
    shared.set_status(Status {
        phase: Phase::Booting,
        model: "Pixel 9 Pro".into(),
        port: cfg.port,
        display: cfg.display,
        ..Default::default()
    });

    // 1. boot the phone runtime in-process
    let phone = match phone::boot(cfg.clone()) {
        Ok(p) => p,
        Err(e) => {
            shared.set_status(Status {
                phase: Phase::Error(format!("boot failed: {e:#}")),
                ..Default::default()
            });
            return;
        }
    };
    let target = format!("127.0.0.1:{}", phone.port);

    // 2. connect the native ADB client (CNXN/AUTH/root — the real device flow)
    let mut dev = None;
    let mut connect_err = None::<String>;
    for attempt in 0..20 {
        match connect(&target) {
            Ok(d) => {
                dev = Some(d);
                break;
            }
            Err(e) => {
                connect_err = Some(format!("{e:#}"));
                if attempt < 19 {
                    std::thread::sleep(Duration::from_millis(300));
                }
            }
        }
    }
    let mut dev = match dev {
        Some(d) => d,
        None => {
            // 20 attempts, ~6 s — give up loudly but keep retrying in the
            // background so a later panel toggle picks the phone up
            shared.set_status(Status {
                phase: Phase::Error(format!(
                    "adb connect to {target}: {}",
                    connect_err.clone().unwrap_or_else(|| "no answer".into())
                )),
                ..Default::default()
            });
            // Bounded retry: at most 30 attempts (~60 s at 2 s each). If all
            // fail, surface a terminal error and let the bridge thread die so
            // a later panel toggle can spawn a fresh bridge cleanly.
            let mut connected: Option<_> = None;
            for attempt in 0..30u32 {
                match connect(&target) {
                    Ok(d) => {
                        connected = Some(d);
                        break;
                    }
                    Err(e) => {
                        if attempt == 0 && connect_err.is_none() {
                            // record the most recent failure for diagnostics
                            connect_err = Some(format!("{e:#}"));
                        }
                        if attempt < 29 {
                            std::thread::sleep(Duration::from_millis(2000));
                        }
                    }
                }
            }
            match connected {
                Some(d) => d,
                None => {
                    shared.set_status(Status {
                        phase: Phase::Error(format!(
                            "adb connect to {target}: gave up after 30 attempts"
                        )),
                        ..Default::default()
                    });
                    phone.shutdown();
                    return;
                }
            }
        }
    };

    // 3. online: publish real device facts
    let mut status = Status {
        phase: Phase::Online,
        serial: phone.serial.clone(),
        model: dev.info.model(),
        port: phone.port,
        root: dev.root_status().as_str().to_string(),
        display: (0, 0),
        battery: 0,
        package_count: 0,
    };
    if let Ok(props) = dev.properties() {
        for (k, v) in props {
            match k.as_str() {
                "screen.width" => status.display.0 = v.parse().unwrap_or(0),
                "screen.height" => status.display.1 = v.parse().unwrap_or(0),
                _ => {}
            }
        }
    }
    if let Ok(pkgs) = dev.list_packages(None) {
        status.package_count = pkgs.len();
    }
    if status.display == (0, 0) {
        status.display = cfg.display;
    }
    {
        let out = dev.shell("dumpsys battery").unwrap_or_default();
        if let Some(level) = out
            .lines()
            .find(|l| l.trim().starts_with("level:"))
            .and_then(|l| l.split(':').nth(1))
            .and_then(|v| v.trim().parse::<u8>().ok())
        {
            status.battery = level;
        }
    }
    shared.set_status(status);

    // 4. serve commands + live stream
    let mut streaming = false;
    let mut streamer: Option<Arc<AtomicBool>> = None; // stop flag while alive
    let mut stream_alive = Arc::new(AtomicBool::new(false));
    let mut last_frame = Instant::now() - Duration::from_secs(1);
    let frame_interval = Duration::from_millis(120);
    loop {
        // commands (non-blocking)
        match rx.try_recv() {
            Ok(Cmd::Tap(x, y)) => {
                let _ = dev.shell(&format!("input tap {x} {y}"));
            }
            Ok(Cmd::Swipe(x1, y1, x2, y2)) => {
                let _ = dev.shell(&format!("input swipe {x1} {y1} {x2} {y2} 180"));
            }
            Ok(Cmd::Text(t)) => {
                let encoded = shell_safe_text(&t);
                let _ = dev.shell(&format!("input text {encoded}"));
            }
            Ok(Cmd::Key(k)) => {
                let n = match k {
                    phone::state::KeyCode::Home => 3,
                    phone::state::KeyCode::Back => 4,
                    phone::state::KeyCode::Enter => 66,
                    phone::state::KeyCode::Del => 67,
                    phone::state::KeyCode::Power => 26,
                    phone::state::KeyCode::VolumeUp => 24,
                    phone::state::KeyCode::VolumeDown => 25,
                };
                let _ = dev.shell(&format!("input keyevent {n}"));
            }
            Ok(Cmd::ExecRaw(cmd, limit, reply)) => {
                let res = dev.exec_bytes(&cmd, limit).map_err(|e| format!("{e:#}"));
                let _ = reply.send(res);
            }
            Ok(Cmd::Shell(cmd, reply)) => {
                // "__install <apk>" is the agent tool's install path: real
                // sync push + manifest decode + pm install on the device
                let res = if let Some(apk) = cmd.strip_prefix("__install ") {
                    dev.install(Path::new(apk), &InstallOptions::default())
                        .map(|r| {
                            format!(
                                "install: {}\nsuccess: {}\nstaged: {}\napk bytes: {}",
                                r.output.trim(),
                                r.success,
                                r.remote_path,
                                r.apk_bytes
                            )
                        })
                        .map_err(|e| format!("{e:#}"))
                } else {
                    dev.shell(&cmd).map_err(|e| format!("{e:#}"))
                };
                let _ = reply.send(res);
            }
            Ok(Cmd::Stream(on)) => {
                streaming = on;
                // stop any previous streamer (it exits on its next wakeup)
                if let Some(f) = streamer.take() {
                    f.store(true, Ordering::Relaxed);
                }
                if on {
                    let stop = Arc::new(AtomicBool::new(false));
                    stream_alive = Arc::new(AtomicBool::new(false));
                    let (sh, tg, st_flag, alive) = (
                        shared.clone(),
                        target.clone(),
                        stop.clone(),
                        stream_alive.clone(),
                    );
                    let _ = std::thread::Builder::new()
                        .name("screen-stream".into())
                        .spawn(move || stream_loop(tg, sh, &st_flag, &alive));
                    streamer = Some(stop);
                }
            }
            Err(std::sync::mpsc::TryRecvError::Empty) => {}
            Err(std::sync::mpsc::TryRecvError::Disconnected) => {
                if let Some(f) = streamer.take() {
                    f.store(true, Ordering::Relaxed);
                }
                phone.shutdown();
                return;
            }
        }

        // fallback frame pull: only while something is watching AND the live
        // `screenrecord` streamer is not delivering (it delivers every
        // committed frame the instant the compositor flushes it)
        if streaming
            && !stream_alive.load(Ordering::Relaxed)
            && last_frame.elapsed() >= frame_interval
        {
            last_frame = Instant::now();
            match dev.screencap() {
                Ok(png) => {
                    if let Some((w, h, rgba)) = decode_png_rgba(&png) {
                        shared.set_frame(w, h, Arc::new(rgba));
                    }
                }
                Err(_) => {
                    // transient failure: reconnect before the next command
                    if let Ok(d) = connect(&target) {
                        dev = d;
                    }
                }
            }
        }
        std::thread::sleep(Duration::from_millis(16));
    }
}

/// Real-device backend using the official Android Platform-Tools client.
/// Unlike the simulator, this never claims a model, root level, or hardware
/// identity until it has read those facts from an explicitly selected target.
fn bridge_adb_cli(cfg: phone::PhoneConfig, rx: Receiver<Cmd>, shared: Shared) {
    // Publish the phase BEFORE the first adb call: discovery + the property
    // probes below each take up to 30 s, and the panel must show Booting
    // during that window instead of sitting silent on the default phase.
    shared.set_status(Status {
        phase: Phase::Booting,
        model: "looking for a device".into(),
        port: cfg.port,
        display: cfg.display,
        ..Default::default()
    });
    let dev = match adb_cli::Device::discover(&cfg.adb_path, &cfg.adb_serial) {
        Ok(d) => d,
        Err(e) => {
            shared.set_status(Status {
                phase: Phase::Error(format!(
                    "{e}; to bring an emulator up in-place run the agent tool \
                     android_setup with {{\"action\": \"boot\"}}"
                )),
                model: "No real Android target".into(),
                ..Default::default()
            });
            return;
        }
    };
    let read = |command: &str| dev.shell(command).unwrap_or_default();
    let model = read("getprop ro.product.model").trim().to_owned();
    let model = if model.is_empty() {
        "Android device".to_owned()
    } else {
        model
    };
    let display = mandroid::screen::parse_wm_size(&read("wm size")).unwrap_or(cfg.display);
    let battery_text = read("dumpsys battery");
    let battery = battery_text
        .lines()
        .find(|l| l.trim().starts_with("level:"))
        .and_then(|l| l.split(':').nth(1))
        .and_then(|v| v.trim().parse::<u8>().ok())
        .unwrap_or(0);
    let packages = read("pm list packages").lines().count();
    let status = Status {
        phase: Phase::Online,
        serial: dev.serial.clone(),
        model,
        port: dev
            .serial
            .rsplit_once(':')
            .and_then(|(_, p)| p.parse().ok())
            .unwrap_or(0),
        root: "not requested".into(),
        display,
        battery,
        package_count: packages,
    };
    shared.set_status(status);
    let mut streaming = false;
    let mut last_frame = Instant::now() - Duration::from_secs(1);
    loop {
        match rx.recv_timeout(Duration::from_millis(20)) {
            Ok(Cmd::Tap(x, y)) => {
                let _ = dev.shell(&format!("input tap {x} {y}"));
            }
            Ok(Cmd::Swipe(x1, y1, x2, y2)) => {
                let _ = dev.shell(&format!("input swipe {x1} {y1} {x2} {y2} 180"));
            }
            Ok(Cmd::Text(text)) => {
                let encoded = shell_safe_text(&text);
                let _ = dev.shell(&format!("input text {encoded}"));
            }
            Ok(Cmd::Key(key)) => {
                let code = match key {
                    phone::state::KeyCode::Home => 3,
                    phone::state::KeyCode::Back => 4,
                    phone::state::KeyCode::Enter => 66,
                    phone::state::KeyCode::Del => 67,
                    phone::state::KeyCode::Power => 26,
                    phone::state::KeyCode::VolumeUp => 24,
                    phone::state::KeyCode::VolumeDown => 25,
                };
                let _ = dev.shell(&format!("input keyevent {code}"));
            }
            Ok(Cmd::ExecRaw(command, limit, reply)) => {
                let result = if command.trim() == "screencap -p" {
                    dev.screenshot()
                } else {
                    dev.exec_out(&["sh", "-c", &command], limit)
                };
                let _ = reply.send(result);
            }
            Ok(Cmd::Shell(command, reply)) => {
                let result = if let Some(path) = command.strip_prefix("__install ") {
                    dev.install(Path::new(path))
                } else {
                    dev.shell(&command)
                };
                let _ = reply.send(result);
            }
            Ok(Cmd::Stream(on)) => streaming = on,
            Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => return,
            Err(std::sync::mpsc::RecvTimeoutError::Timeout) => {}
        }
        if streaming && last_frame.elapsed() >= Duration::from_millis(250) {
            last_frame = Instant::now();
            if let Ok(png) = dev.screenshot() {
                if let Some((w, h, rgba)) = decode_png_rgba(&png) {
                    shared.set_frame(w, h, Arc::new(rgba));
                }
            }
        }
    }
}

/// The live display stream — one dedicated ADB connection running
/// `screenrecord --raw` (scrcpy's architecture): the phone pushes every
/// committed frame the moment the compositor flushes it, raw RGBA, no PNG
/// round-trip. Frames land in `shared` for the UI to paint at its next poll.
///
/// On stream errors the connection is reopened (the phone may have dropped a
/// transport on root flips, etc.). After several consecutive failures the
/// thread gives up and reports dead, and the bridge falls back to polling
/// `screencap` so the panel keeps updating.
fn stream_loop(target: String, shared: Shared, stop: &AtomicBool, alive: &AtomicBool) {
    let mut misses = 0u32;
    'reconnect: loop {
        if stop.load(Ordering::Relaxed) {
            return;
        }
        let mut dev = match connect(&target) {
            Ok(d) => d,
            Err(_) => {
                misses += 1;
                if misses >= 5 {
                    alive.store(false, Ordering::Relaxed);
                    return;
                }
                std::thread::sleep(Duration::from_millis(600));
                continue 'reconnect;
            }
        };
        let s: ExecStream = match dev.open_exec_stream("screenrecord --raw") {
            Ok(s) => s,
            Err(_) => {
                misses += 1;
                if misses >= 5 {
                    alive.store(false, Ordering::Relaxed);
                    return;
                }
                std::thread::sleep(Duration::from_millis(600));
                continue 'reconnect;
            }
        };
        let mut re = StreamReassembler::new();
        loop {
            if stop.load(Ordering::Relaxed) {
                dev.exec_stream_close(&s);
                return;
            }
            match dev.exec_stream_next(&s, &mut re, 256 * 1024 * 1024) {
                Ok(f) => {
                    misses = 0;
                    alive.store(true, Ordering::Relaxed);
                    if std::env::var("ROMACODE_STREAM_DEBUG").is_ok() {
                        eprintln!(
                            "[stream] frame gen={} {}x{} rgba_bytes={}",
                            f.gen,
                            f.w,
                            f.h,
                            f.rgba.len()
                        );
                    }
                    shared.set_frame(f.w, f.h, Arc::new(f.rgba));
                }
                Err(_) => {
                    // transport died — reopen (bounded, like above)
                    misses += 1;
                    if misses >= 5 {
                        alive.store(false, Ordering::Relaxed);
                        return;
                    }
                    continue 'reconnect;
                }
            }
        }
    }
}

fn connect(target: &str) -> anyhow::Result<AdbDevice> {
    let key = mandroid::key::load_or_generate(&mandroid::key::default_dir())
        .map_err(|e| anyhow::anyhow!("loading adb key: {e:#}"))?;
    let mut opts = AdbOptions::default();
    opts.connect_timeout = Duration::from_secs(8);
    opts.io_timeout = Duration::from_secs(30);
    AdbDevice::connect(target, &key, &opts)
}

fn decode_png_rgba(png: &[u8]) -> Option<(u32, u32, Vec<u8>)> {
    let mut dec = png::Decoder::new(std::io::Cursor::new(png));
    dec.set_transformations(png::Transformations::EXPAND);
    let mut reader = dec.read_info().ok()?;
    // Dimensions come from the device (or a hostile image): a bogus header
    // used to allocate `(w * h * 4)` unchecked — u32 math could overflow and
    // a large-but-valid header allocated gigabytes. Cap at 16 MP (any real
    // screenshot is far below that) and do the math in usize.
    let head = reader.info();
    if head.width == 0
        || head.height == 0
        || u64::from(head.width) * u64::from(head.height) > 16_000_000
    {
        return None;
    }
    let mut buf = vec![0; reader.output_buffer_size()];
    let info = reader.next_frame(&mut buf).ok()?;
    let (w, h) = (info.width, info.height);
    let src = &buf[..info.buffer_size()];
    let mut rgba = Vec::with_capacity(w as usize * h as usize * 4);
    match info.color_type {
        png::ColorType::Rgba => rgba.extend_from_slice(src),
        png::ColorType::Rgb => {
            for px3 in src.chunks_exact(3) {
                rgba.extend_from_slice(px3);
                rgba.push(255);
            }
        }
        png::ColorType::Grayscale => {
            for g in src {
                rgba.extend_from_slice(&[*g, *g, *g, 255]);
            }
        }
        png::ColorType::GrayscaleAlpha => {
            for ga in src.chunks_exact(2) {
                rgba.extend_from_slice(&[ga[0], ga[0], ga[0], ga[1]]);
            }
        }
        _ => return None,
    }
    Some((w, h, rgba))
}

/// Run the verification battery against a live bridge (`romacode
/// --verify-android`). Returns (passed, failed, log lines).
pub fn verify(android: &mut Android) -> (usize, usize, Vec<String>) {
    let deadline = Instant::now() + Duration::from_secs(30);
    loop {
        android.poll();
        if let Some(status) = android.status() {
            match &status.phase {
                Phase::Online => break,
                Phase::Error(error) => return (0, 1, vec![format!("FAIL  adb.target — {error}")]),
                Phase::Booting => {}
            }
        }
        if Instant::now() >= deadline {
            return (
                0,
                1,
                vec!["FAIL  adb.target — timed out waiting for an authorized real target".into()],
            );
        }
        std::thread::sleep(Duration::from_millis(100));
    }
    let mut log = Vec::new();
    let mut pass = 0usize;
    let mut fail = 0usize;
    let mut check = |name: &str, ok: bool, detail: String| {
        if ok {
            pass += 1;
            log.push(format!("PASS  {name} — {detail}"));
        } else {
            fail += 1;
            log.push(format!("FAIL  {name} — {detail}"));
        }
    };
    let status = android.status().unwrap_or_default();
    check(
        "adb.target",
        status.phase == Phase::Online && !status.serial.is_empty(),
        format!("serial={} model={}", status.serial, status.model),
    );
    match android.shell("getprop ro.build.version.release") {
        Ok(release) => check(
            "device.release",
            !release.trim().is_empty(),
            format!("Android {}", release.trim()),
        ),
        Err(e) => check("device.release", false, e),
    }
    match android.shell("wm size") {
        Ok(size) => {
            let dims = mandroid::screen::parse_wm_size(&size);
            check("display.size", dims.is_some(), size.trim().to_owned());
        }
        Err(e) => check("display.size", false, e),
    }
    match android.shell("pm list packages") {
        Ok(packages) => {
            let count = packages
                .lines()
                .filter(|line| line.starts_with("package:"))
                .count();
            check("packages.list", count > 0, format!("{count} package(s)"));
        }
        Err(e) => check("packages.list", false, e),
    }
    match android.screenshot_png() {
        Ok(png) => {
            let valid = png.starts_with(b"\x89PNG\r\n\x1a\n");
            check(
                "display.screenshot",
                valid,
                format!("{} bytes; real PNG={valid}", png.len()),
            );
        }
        Err(e) => check("display.screenshot", false, e),
    }
    (pass, fail, log)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn live_device_status_reaches_the_agent_prompt() {
        let mut cfg = crate::config::Config::default();
        cfg.android.auto_start = true;
        publish_status(&Status {
            phase: Phase::Online,
            serial: "emulator-5554".into(),
            model: "Pixel".into(),
            port: 5555,
            root: "adbd".into(),
            display: (720, 1560),
            battery: 80,
            package_count: 42,
        });
        let prompt = crate::agent::system_prompt(&cfg);
        assert!(prompt.contains("Live device state: ONLINE"), "{prompt}");
        assert!(prompt.contains("emulator-5554"), "{prompt}");
        assert!(prompt.contains("Pixel"), "{prompt}");

        publish_status(&Status {
            phase: Phase::Error("no target".into()),
            ..Default::default()
        });
        let prompt = crate::agent::system_prompt(&cfg);
        assert!(prompt.contains("NO TARGET ONLINE"), "{prompt}");
        assert!(prompt.contains("android_setup"), "{prompt}");
    }
}
