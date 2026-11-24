//! Device state — the "hardware" and "OS" of the built-in phone.
//!
//! One [`OsState`] behind a mutex is the single source of truth. The
//! compositor thread renders it, the `adbd` connection threads mutate it
//! (shell commands, input injection), and persistence snapshots it. There is
//! no second representation anywhere: a screenshot, `getprop`, the Files app
//! and the Settings screen all read the same fields.

use std::collections::{BTreeMap, VecDeque};
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::SystemTime;

/// Physical display panel of the built-in phone (20:9, like a modern handset).
pub const PANEL_W: u32 = 720;
pub const PANEL_H: u32 = 1560;
pub const PANEL_DPI: u32 = 420;

// ── apps ─────────────────────────────────────────────────────────────────────

/// Every launchable app. `User` apps arrive through a real `pm install` of a
/// real APK — the engine decodes the manifest for the package and label.
#[derive(Clone, PartialEq, Eq, Debug)]
pub enum AppId {
    Settings,
    Calculator,
    Clock,
    Messages,
    Dialer,
    Browser,
    Photos,
    Files,
    Termux,
    /// Magisk — the root manager (su grants, Zygisk, modules). Shipped on
    /// the image: the device is rooted out of the box.
    Magisk,
    /// Root Explorer — file manager over the real OS partitions.
    RootExplorer,
    /// Titanium Backup — batch app backup to /sdcard (needs root).
    TitaniumBackup,
    User(String),
}

impl AppId {
    /// Package name, as `pm list packages` reports it.
    pub fn package(&self) -> String {
        match self {
            Self::Settings => "com.android.settings",
            Self::Calculator => "com.android.calculator2",
            Self::Clock => "com.android.deskclock",
            Self::Messages => "com.google.android.apps.messaging",
            Self::Dialer => "com.google.android.dialer",
            Self::Browser => "com.android.chrome",
            Self::Photos => "com.google.android.apps.photos",
            Self::Files => "com.android.documentsui",
            Self::Termux => "com.termux",
            Self::Magisk => "com.topjohnwu.magisk",
            Self::RootExplorer => "com.speedsoftware.rootexplorer",
            Self::TitaniumBackup => "com.keramidas.TitaniumBackup",
            Self::User(p) => return p.clone(),
        }
        .to_string()
    }

    pub fn label(&self) -> String {
        match self {
            Self::Settings => "Settings",
            Self::Calculator => "Calculator",
            Self::Clock => "Clock",
            Self::Messages => "Messages",
            Self::Dialer => "Phone",
            Self::Browser => "Chrome",
            Self::Photos => "Photos",
            Self::Files => "Files",
            Self::Termux => "Termux",
            Self::Magisk => "Magisk",
            Self::RootExplorer => "Root Explorer",
            Self::TitaniumBackup => "Titanium Backup",
            Self::User(p) => p.split('.').next_back().unwrap_or(p),
        }
        .to_string()
    }

    /// Launcher activity component, as `am start -n` spells it.
    pub fn activity(&self) -> String {
        let pkg = self.package();
        let act = match self {
            Self::Settings => ".Settings",
            Self::Calculator => ".Calculator",
            Self::Clock => ".DeskClock",
            Self::Messages => ".ConversationListActivity",
            Self::Dialer => ".DialtactsActivity",
            Self::Browser => ".Main",
            Self::Photos => ".MainActivity",
            Self::Files => ".DocumentsActivity",
            Self::Termux => ".app.TermuxActivity",
            Self::Magisk => ".ui.MainActivity",
            Self::RootExplorer => ".activities.ExplorerActivity",
            Self::TitaniumBackup => ".BackupActivity",
            Self::User(_) => ".MainActivity",
        };
        format!("{pkg}/{pkg}{act}")
    }
}

/// A package in the device's package database.
#[derive(Clone, Debug)]
pub struct Package {
    pub package: String,
    pub label: String,
    pub system: bool,
    /// Which launchable app backs this package, if any.
    pub app: Option<AppId>,
    pub version: String,
}

// ── messaging / telephony ────────────────────────────────────────────────────

#[derive(Clone, Debug, serde::Serialize, serde::Deserialize)]
pub struct SmsMessage {
    pub from: String,
    pub text: String,
    pub at: i64,
    /// `true` when sent from the device (compose), `false` when received
    /// (`sms send` through adbd — the telephony console path).
    pub mine: bool,
}

#[derive(Clone, Debug, serde::Serialize, serde::Deserialize)]
pub struct SmsThread {
    pub address: String,
    pub messages: Vec<SmsMessage>,
}

#[derive(Clone, Debug, serde::Serialize, serde::Deserialize)]
pub struct CallEntry {
    pub number: String,
    pub at: i64,
    pub seconds: u32,
    /// incoming | outgoing
    pub direction: String,
}

/// A photo in the gallery. Always a real PNG file on disk under `photos/`.
#[derive(Clone, Debug, serde::Serialize, serde::Deserialize)]
pub struct PhotoEntry {
    pub file: String,
    pub w: u32,
    pub h: u32,
    pub taken_at: i64,
}

// ── input ────────────────────────────────────────────────────────────────────

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TouchAction {
    Down,
    Move,
    Up,
}

/// One pointer sample in effective-display pixel coordinates.
#[derive(Clone, Copy, Debug)]
pub struct TouchEvent {
    pub action: TouchAction,
    pub x: u32,
    pub y: u32,
}

/// Key events accepted by `input keyevent` (the codes the stack uses).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum KeyCode {
    Back,
    Home,
    Enter,
    Del,
    Power,
    VolumeUp,
    VolumeDown,
}

impl KeyCode {
    /// Android keyevent number → code. Unknown codes are ignored (like a
    /// device with no mapping).
    pub fn from_number(n: i64) -> Option<Self> {
        Some(match n {
            3 => Self::Home,
            4 => Self::Back,
            24 => Self::VolumeUp,
            25 => Self::VolumeDown,
            26 => Self::Power,
            66 => Self::Enter,
            67 => Self::Del,
            _ => return None,
        })
    }
}

/// Text typed through `input text` — delivered to the focused field.
#[derive(Clone, Debug)]
pub struct TextEvent {
    pub text: String,
}

#[derive(Clone, Debug)]
pub enum InputEvent {
    Touch(TouchEvent),
    Key(KeyCode),
    Text(TextEvent),
}

/// Events produced by background work and consumed on the compositor tick.
#[derive(Clone, Debug)]
pub enum OsEvent {
    /// A page finished downloading for the browser.
    BrowserLoaded {
        url: String,
        ok: bool,
        detail: String,
        body_path: PathBuf,
        status: u16,
    },
    /// Notification banner (e.g. an incoming SMS).
    Toast(String),
    /// (Re-)flash the OS image: Settings → System update, recovery, or
    /// `adb shell os install`. Progress is drawn from the installer itself.
    OsInstall,
    /// Reboot the handset through the real boot chain (bootloader → kernel →
    /// init → boot animation → home).
    Reboot,
    /// Boot into recovery (the OTA/OS menu).
    RebootRecovery,
}

// ── per-app UI state ─────────────────────────────────────────────────────────

#[derive(Default)]
pub struct CalcUi {
    pub expr: String,
    pub result: Option<String>,
    pub history: Vec<String>,
}

#[derive(Clone, Copy, PartialEq, Eq, Default)]
pub enum ClockTab {
    #[default]
    Clock,
    Stopwatch,
}

#[derive(Default)]
pub struct ClockUi {
    pub tab: ClockTab,
    /// Stopwatch: `Some(start instant)` while running.
    pub started: Option<SystemTime>,
    pub accumulated_ms: u64,
    pub laps: Vec<u64>,
}

#[derive(Default)]
pub struct MessagesUi {
    pub open_thread: Option<String>,
    pub compose_to: String,
    pub compose_text: String,
    pub field: usize,
    pub kb: SoftKeyboard,
}

#[derive(Default)]
pub struct DialerUi {
    pub tab: usize, // 0 recents, 1 keypad
    pub number: String,
    pub call: Option<CallState>,
}

pub struct CallState {
    pub number: String,
    pub started: SystemTime,
    pub ringing: bool,
}

#[derive(Default)]
pub struct BrowserUi {
    pub url_bar: String,
    pub url_bar_focus: bool,
    pub loading: bool,
    pub lines: Vec<crate::apps::HtmlLine>,
    pub scroll: usize,
    pub history: Vec<String>,
    pub history_idx: Option<usize>,
    pub error: Option<String>,
    pub status: u16,
    pub kb: SoftKeyboard,
}

#[derive(Default)]
pub struct PhotosUi {
    pub viewer: Option<usize>,
}

#[derive(Default)]
pub struct FilesUi {
    pub cwd: PathBuf,
    pub entries: Vec<(String, bool, u64)>, // name, is_dir, size
    pub selected: Option<usize>,
    pub detail: Option<(String, bool, u64, String)>,
}

/// One rendered line of the Termux terminal.
#[derive(Clone, Debug)]
pub struct TermuxLine {
    pub text: String,
    pub scale: u32,
    pub color: [u8; 3],
}

#[derive(Default)]
pub struct TermuxUi {
    pub lines: Vec<TermuxLine>,
    pub input: String,
    pub kb: SoftKeyboard,
    /// Commands executed through the RUN_COMMAND service this session.
    pub run_commands: usize,
    /// Session working directory (starts at $HOME).
    pub cwd: Option<PathBuf>,
}

#[derive(Default)]
pub struct SettingsUi {
    pub section: usize, // 0 root list
    pub scroll: usize,
    pub apps_scroll: usize,
    pub sel_app: Option<String>,
}

/// Magisk — root manager UI. The grant list is the live `su.grant.*` keys of
/// the settings table, so the app, `su` and `pm` all see the same policy.
#[derive(Default, Clone)]
pub struct MagiskUi {
    pub scroll: usize,
    /// (package, grant time) of the row the user tapped, shown as detail.
    pub sel_grant: Option<String>,
}

/// Root Explorer — browses the real installed OS image on disk (system,
/// vendor, product, /data, /cache) with root.
#[derive(Default, Clone)]
pub struct RootExplorerUi {
    /// Device path (e.g. `/system/etc`) of the current directory.
    pub cwd: Option<String>,
    /// (name, is_dir, size, readonly-mount) entries of the cwd.
    pub entries: Vec<(String, bool, u64, bool)>,
    /// Selected file detail: (path, size, sha-line).
    pub detail: Option<String>,
}

/// Titanium Backup — batch backup of installed packages to /sdcard.
#[derive(Default, Clone)]
pub struct TitaniumUi {
    pub scroll: usize,
    /// Backup run in progress: (done, total) — one package per tick.
    pub progress: Option<(usize, usize)>,
    /// The package queue of the running batch (real file per package).
    pub pending: Option<Vec<String>>,
    /// Log lines of completed backups this session.
    pub log: Vec<String>,
}

/// A minimal on-screen keyboard (the device's input method). Rows of keys,
/// shift, space, backspace and enter — enough to type anywhere on the phone.
#[derive(Default)]
pub struct SoftKeyboard {
    pub visible: bool,
    pub shift: bool,
}

#[derive(Default)]
pub struct AppUi {
    pub calc: CalcUi,
    pub termux: TermuxUi,
    pub clock: ClockUi,
    pub messages: MessagesUi,
    pub dialer: DialerUi,
    pub browser: BrowserUi,
    pub photos: PhotosUi,
    pub files: FilesUi,
    pub settings: SettingsUi,
    pub magisk: MagiskUi,
    pub rootexplorer: RootExplorerUi,
    pub titanium: TitaniumUi,
}

// ── boot / misc ──────────────────────────────────────────────────────────────

/// The real boot chain. A handset goes bootloader → (OS flash when the image
/// is missing) → kernel/init → boot animation → home; each stage renders its
/// own screen and advances in the compositor tick. Recovery is its own boot
/// target (the OTA/OS menu). An OS re-flash runs between the bootloader and
/// the kernel, exactly like a factory image being applied.
#[derive(Clone, Debug)]
pub enum BootPhase {
    /// Bootloader: the vendor wordmark on black, then the kernel takes over.
    PowerOn {
        t0: SystemTime,
    },
    /// OS flash in progress (bootloader stage, factory image being applied).
    /// The installer lives in [`OsState::installer`] and drives the screen.
    Flash,
    /// Kernel + init: dmesg lines advance; ends after the boot script.
    Kernel {
        t0: SystemTime,
        line: usize,
    },
    /// Boot animation (the classic Android boot splash), `progress` in 0..1.
    Splash {
        t0: SystemTime,
    },
    /// Recovery menu (reboot / apply OS update / wipe data / power off).
    Recovery,
    Home,
}

impl BootPhase {
    /// `true` once the system has finished booting (sys.boot_completed=1).
    /// Recovery counts: the OS is up and running, just showing its menu.
    pub fn boot_completed(&self) -> bool {
        matches!(self, BootPhase::Home | BootPhase::Recovery)
    }
}

#[derive(Clone, Debug)]
pub struct Notif {
    pub title: String,
    pub body: String,
    pub at: i64,
    pub app: Option<AppId>,
}

// ── the state itself ─────────────────────────────────────────────────────────

pub struct OsState {
    pub data_dir: PathBuf,
    pub serial: String,
    /// The TCP port adbd is listening on (0 until bound).
    pub adb_port: u16,
    pub booted_at: SystemTime,
    pub boot: BootPhase,
    pub screen_off: bool,
    pub display: (u32, u32),
    pub density: u32,
    pub size_override: Option<(u32, u32)>,
    pub density_override: Option<u32>,
    pub packages: Vec<Package>,
    pub sms: Vec<SmsThread>,
    pub calls: Vec<CallEntry>,
    pub photos: Vec<PhotoEntry>,
    pub settings: BTreeMap<String, String>,
    pub battery: u8,
    pub charging: bool,
    pub rooted: bool,
    /// The physical handset identity (serial, IMEIs, EID, build fingerprint).
    /// Deterministic — the same device on every boot.
    pub identity: mandroid::identity::DeviceIdentity,
    /// The dual-eSIM profile registry (2 profiles registered out of the box).
    pub esim: mandroid::identity::EsimRegistry,
    pub airplane_mode: bool,
    pub mobile_data: bool,
    /// Live radio snapshots per slot, advanced by the compositor.
    pub radio: [mandroid::identity::RadioSnapshot; 2],
    /// Live Wi-Fi link state: (rssi dBm, bars).
    pub wifi: (i32, u32),
    pub last_radio_tick: SystemTime,
    pub model: String,
    pub device: String,
    pub android_release: String,
    pub sdk_level: u32,
    pub ram_mib: u64,
    pub rom_gib: u64,
    pub cpu_cores: u32,
    pub cpu_arch: String,
    pub fingerprint: String,
    pub current: Option<AppId>,
    pub recents: Vec<AppId>,
    pub home_page: usize,
    /// Active drag gesture origin (device px), if a pointer is down.
    pub drag: Option<(i32, i32)>,
    /// Recents (task switcher) screen open.
    pub recents_screen_open: bool,
    pub app: AppUi,
    /// Shell working directory override (Termux session context when set).
    pub shell_cwd: Option<PathBuf>,
    pub inq: VecDeque<InputEvent>,
    /// Shared event inbox — background threads (browser loads, other
    /// processes) push here; the compositor drains each tick.
    pub inbox: Arc<Mutex<VecDeque<OsEvent>>>,
    pub notifications: Vec<Notif>,
    /// The notification banner currently on screen: (title, body, shown-at).
    pub banner: Option<(String, String, SystemTime)>,
    pub dirty: bool,
    pub persist_dirty: bool,
    /// Composed frame (RGBA, `effective_size()`), advanced by the compositor.
    pub fb: Vec<u8>,
    pub fb_gen: u64,
    pub last_battery_tick: SystemTime,
    pub last_save: SystemTime,
    /// Kernel ring buffer (`dmesg`) — filled by the boot chain, read by the
    /// `dmesg` shell command.
    pub dmesg: VecDeque<String>,
    /// Logcat-style system log entries (boot events, install steps).
    pub logs: VecDeque<String>,
    /// Recovery is open: the OS menu (reboot / install / wipe) overlays all.
    pub recovery_open: bool,
    pub recovery_sel: usize,
    /// Wipe-data confirmation armed (recovery shows the confirm row).
    pub recovery_confirm: bool,
    /// Active OS installer — a flash in progress, drives the progress screen.
    pub installer: Option<crate::os::Installer>,
    /// Completed boot cycles this session (reboots).
    pub reboots: u32,
}

impl OsState {
    pub fn effective_size(&self) -> (u32, u32) {
        self.size_override.unwrap_or(self.display)
    }

    pub fn effective_density(&self) -> u32 {
        self.density_override.unwrap_or(self.density)
    }

    /// Sample the radio for `slot` right now (the same pure function the
    /// tools see — no second representation).
    pub fn radio_now(&self, slot: u32) -> mandroid::identity::RadioSnapshot {
        mandroid::identity::sample_radio(
            slot,
            &self.esim,
            self.airplane_mode,
            self.mobile_data,
            now_ms(),
        )
    }

    /// The default mobile-data slot's live radio.
    pub fn data_radio_now(&self) -> mandroid::identity::RadioSnapshot {
        self.radio_now(self.esim.data_slot)
    }

    /// Append a logcat-style line to `logs` and cap the deque at 1024 entries
    /// (newest kept, oldest evicted) so a long-running phone doesn't grow its
    /// resident set unboundedly. Every call site that builds a log line should
    /// route through here instead of `logs.push_back(...)` directly.
    pub fn log_line(&mut self, line: String) {
        self.logs.push_back(line);
        const MAX_LOGS: usize = 1024;
        if self.logs.len() > MAX_LOGS {
            let drop = self.logs.len() - MAX_LOGS;
            self.logs.drain(0..drop);
        }
    }

    pub fn package(&self, pkg: &str) -> Option<&Package> {
        self.packages.iter().find(|p| p.package == pkg)
    }

    /// Push a notification and mark the screen dirty. The list is capped at
    /// 64 entries (newest kept, oldest evicted) so a runaway notification
    /// flood can't grow the phone's resident set unboundedly.
    pub fn notify(&mut self, title: &str, body: &str, app: Option<AppId>) {
        self.notifications.push(Notif {
            title: title.to_string(),
            body: body.to_string(),
            at: now_ms(),
            app,
        });
        const MAX_NOTIFS: usize = 64;
        if self.notifications.len() > MAX_NOTIFS {
            let drop = self.notifications.len() - MAX_NOTIFS;
            self.notifications.drain(0..drop);
        }
        self.dirty = true;
    }

    /// Register an app launch from the shell side (`am start`, `monkey`).
    /// While the system is still booting (flash/kernel/splash) the activity
    /// manager is not up yet: the launch is refused like on a real handset.
    pub fn launch_app(&mut self, app: AppId) -> bool {
        if !self.boot.boot_completed() {
            return false;
        }
        self.current = Some(app.clone());
        self.boot = BootPhase::Home;
        self.screen_off = false;
        if self.recents.first() != Some(&app) {
            self.recents.insert(0, app);
            self.recents.truncate(8);
        }
        self.dirty = true;
        true
    }
}

pub fn now_ms() -> i64 {
    SystemTime::now()
        .duration_since(SystemTime::UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0)
}

pub fn now_secs() -> u32 {
    SystemTime::now()
        .duration_since(SystemTime::UNIX_EPOCH)
        .map(|d| d.as_secs() as u32)
        .unwrap_or(0)
}
