//! User configuration, stored at `<config_dir>/romacode/config.json`.
//!
//! Schema v2 — sectioned, versioned and forward/backward compatible:
//!
//! * every section and every field has a default (`#[serde(default)]`), so a
//!   config file only has to contain what the user wants to change;
//! * the original v1 flat keys (`model`, `api_base`, `max_tokens`, `sidebar`,
//!   `android`, …) are unchanged, so existing config files keep working;
//! * values are validated and clamped on load, and every correction is
//!   reported as a warning instead of silently discarded;
//! * a config file that fails to parse is backed up to `config.json.bak`
//!   (never overwritten with defaults) and defaults are used for the session;
//! * selected settings can be overridden per run with `ROMACODE_*`
//!   environment variables (see [`Config::apply_env`]).
//!
//! See `docs/SETTINGS.md` for the full reference and `config.example.json`
//! for a complete file with every option at its default.
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::path::PathBuf;

/// Current schema version written to `config.json`.
pub const SCHEMA_VERSION: u32 = 2;

// ── enums ────────────────────────────────────────────────────────────────────

/// What the agent is allowed to do on its own for a class of tools.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Permission {
    /// Run without asking.
    Allow,
    /// Prompt before each use.
    #[default]
    Ask,
    /// Never run.
    Deny,
}

/// Agent working mode (the `tab` toggle in the UI).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum AgentMode {
    /// Full tool access — the agent edits and runs things.
    #[default]
    Build,
    /// Read-only planning — no edits, no shell.
    Plan,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ThemeMode {
    /// Follow the terminal (dark unless it reports light).
    #[default]
    Auto,
    Dark,
    Light,
    /// RomaCode bronze (the home-screen palette).
    Bronze,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum DiffStyle {
    #[default]
    Unified,
    Split,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum LogLevel {
    Off,
    Error,
    Warn,
    #[default]
    Info,
    Debug,
    Trace,
}

/// Wire protocol a provider speaks.
/// Which wire protocol a provider speaks. Defined once in the shared
/// protocol crate so the client settings screen and the server config agree.
pub use romacode_shared::catalog::ApiKind;

/// One provider entry in the `providers` map. Use it to override a known
/// provider (same id) or to define a custom one (new id, `base_url` required).
/// Every field is optional; empty means "use the known provider's value".
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(default)]
pub struct ProviderCfg {
    /// Display name.
    pub name: String,
    /// Wire protocol. Unset for a known provider = its own; for a custom one = `openai`.
    pub kind: Option<ApiKind>,
    pub base_url: String,
    /// API key stored in the config file. Prefer `api_key_env`; the file is
    /// written with owner-only permissions when a key is saved from the app.
    pub api_key: String,
    /// Environment variable holding the key (checked after `api_key`).
    pub api_key_env: String,
    /// Model used when the top-level `model` is empty.
    pub default_model: String,
    /// Models offered in pickers (any model id still works).
    pub models: Vec<String>,
    /// Extra headers for this provider (e.g. `HTTP-Referer` for OpenRouter).
    pub headers: BTreeMap<String, String>,
    /// OpenAI-compatible only: ask for token usage in the stream.
    pub stream_usage: bool,
    /// Send tool definitions. Turn off for models without function calling.
    pub tools: bool,
    /// Custom providers that need no key (local servers).
    pub no_key: bool,
}

impl Default for ProviderCfg {
    fn default() -> Self {
        Self {
            name: String::new(),
            kind: None,
            base_url: String::new(),
            api_key: String::new(),
            api_key_env: String::new(),
            default_model: String::new(),
            models: Vec::new(),
            headers: BTreeMap::new(),
            stream_usage: true,
            tools: true,
            no_key: false,
        }
    }
}

/// The active provider with every layer (known defaults, `providers` entry,
/// top-level overrides, environment) applied.
#[derive(Clone, Debug)]
pub struct Resolved {
    pub id: String,
    pub name: String,
    pub kind: ApiKind,
    pub base_url: String,
    pub api_key: Option<String>,
    pub key_env: String,
    pub needs_key: bool,
    pub model: String,
    pub headers: BTreeMap<String, String>,
    pub stream_usage: bool,
    pub tools: bool,
}

// ── sections ─────────────────────────────────────────────────────────────────

/// Sampling and reasoning parameters sent with every request. `None` means
/// "let the API use its default".
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(default)]
pub struct ModelParams {
    pub temperature: Option<f32>,
    pub top_p: Option<f32>,
    pub top_k: Option<u32>,
    pub stop_sequences: Vec<String>,
    /// Extended thinking (models that support it).
    pub thinking: bool,
    /// Token budget for extended thinking; must be below `max_tokens`.
    pub thinking_budget_tokens: u32,
    /// Mark the system prompt for prompt caching.
    pub prompt_cache: bool,
    /// Stream tokens as they arrive.
    pub stream: bool,
    /// Tried in order when the primary model is unavailable or overloaded.
    pub fallback_models: Vec<String>,
}

impl Default for ModelParams {
    fn default() -> Self {
        Self {
            temperature: None,
            top_p: None,
            top_k: None,
            stop_sequences: Vec::new(),
            thinking: false,
            thinking_budget_tokens: 8_000,
            prompt_cache: true,
            stream: true,
            fallback_models: Vec::new(),
        }
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(default)]
pub struct AgentCfg {
    /// Mode a new session starts in.
    pub default_mode: AgentMode,
    /// Maximum model↔tool round trips per user message.
    pub max_tool_rounds: u32,
    /// Run independent tool calls concurrently.
    pub parallel_tools: bool,
    /// Per tool call timeout, seconds (0 = none).
    pub tool_timeout_secs: u64,
    /// Tool output beyond this many bytes is truncated before it reaches the model.
    pub max_tool_output_bytes: usize,
    /// Summarise old turns automatically when the context fills up.
    pub auto_compact: bool,
    /// Context usage (%) at which auto-compaction starts.
    pub compact_threshold_pct: u8,
    /// Give new sessions a short generated title.
    pub auto_title: bool,
    /// Project instruction files read into the system prompt when present.
    pub instruction_files: Vec<String>,
    /// Extra text appended to the system prompt.
    pub system_prompt_append: Option<String>,
    /// Persona file loaded into the system prompt every turn
    /// (`crate::persona`). `None`/null = the default
    /// `<config_dir>/romacode/persona.md`; `""`, `"off"`, `"none"`,
    /// `"disabled"`, `"false"`, `"0"` = off; anything else = that path
    /// (`~/` expanded). Hot-reloads on every edit.
    pub persona_file: Option<String>,
    /// Which engine runs the think-act-observe loop: `native` (in-house SSE
    /// loop, Anthropic + OpenAI-compatible) or `langchain` (the vendored
    /// langchain-rust executor with memory + RAG tools; OpenAI-compatible
    /// providers, falls back to native for Anthropic with a notice).
    pub engine: String,
}

impl Default for AgentCfg {
    fn default() -> Self {
        Self {
            default_mode: AgentMode::Build,
            max_tool_rounds: 200,
            parallel_tools: true,
            tool_timeout_secs: 120,
            max_tool_output_bytes: 64 * 1024,
            auto_compact: true,
            compact_threshold_pct: 85,
            auto_title: true,
            instruction_files: vec!["ROMACODE.md".into(), "AGENTS.md".into(), "CLAUDE.md".into()],
            system_prompt_append: None,
            persona_file: None,
            engine: "native".into(),
        }
    }
}

/// Tool permissions. Rules are `Tool(pattern)` strings such as `bash(git *)`
/// or `edit(src/**)`; `deny` beats `allow`, which beats the class default.
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(default)]
pub struct PermissionsCfg {
    pub read: Permission,
    pub edit: Permission,
    pub bash: Permission,
    pub web: Permission,
    /// Controlling the configured real Android target.
    pub android: Permission,
    /// Active network scanning (`rnmap_scan`). Prompted like shell commands:
    /// a scan probes hosts far beyond the project directory.
    pub net: Permission,
    pub allow: Vec<String>,
    pub ask: Vec<String>,
    pub deny: Vec<String>,
    /// Paths the agent may never modify, even when `edit` is `allow`.
    pub protected_paths: Vec<String>,
    /// Never touch files outside the project directory.
    pub confine_to_project: bool,
}

impl Default for PermissionsCfg {
    fn default() -> Self {
        // Fully open by default: the agent is trusted to run everything on the
        // host it was started on. Every field stays configurable for users who
        // want their own policy, but nothing is restricted out of the box.
        Self {
            read: Permission::Allow,
            edit: Permission::Allow,
            bash: Permission::Allow,
            web: Permission::Allow,
            android: Permission::Allow,
            net: Permission::Allow,
            allow: Vec::new(),
            ask: Vec::new(),
            deny: Vec::new(),
            protected_paths: Vec::new(),
            confine_to_project: false,
        }
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(default)]
pub struct UiCfg {
    pub theme: ThemeMode,
    /// Accent colour override, `#rrggbb`. Empty = theme default.
    pub accent: String,
    pub animations: bool,
    /// Disable the waterfall/sheen animations and slow redraws.
    pub reduce_motion: bool,
    pub mouse: bool,
    pub syntax_highlight: bool,
    pub line_numbers: bool,
    pub diff_style: DiffStyle,
    pub tab_width: u8,
    pub wrap_lines: bool,
    pub compact: bool,
    pub show_timestamps: bool,
    pub show_tokens: bool,
    pub show_cost: bool,
    /// Ring the terminal bell when a long task finishes.
    pub bell_on_done: bool,
    /// Lines kept per session transcript for scrollback.
    pub scrollback_lines: u32,
    /// Maximum frames per second for animated screens.
    pub max_fps: u8,
}

impl Default for UiCfg {
    fn default() -> Self {
        Self {
            theme: ThemeMode::Auto,
            accent: String::new(),
            animations: true,
            reduce_motion: false,
            mouse: true,
            syntax_highlight: true,
            line_numbers: true,
            diff_style: DiffStyle::Unified,
            tab_width: 4,
            wrap_lines: true,
            compact: false,
            show_timestamps: false,
            show_tokens: true,
            show_cost: true,
            bell_on_done: false,
            scrollback_lines: 5_000,
            max_fps: 30,
        }
    }
}

/// Key bindings. Values use the same spelling as the palette (`ctrl+n`,
/// `alt+enter`, `esc`, …). `overrides` maps any palette command id to a key.
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(default)]
pub struct KeysCfg {
    pub send: String,
    pub newline: String,
    pub interrupt: String,
    pub toggle_mode: String,
    pub new_session: String,
    pub close_session: String,
    pub palette: String,
    pub side_panel: String,
    pub android: String,
    pub quit: String,
    /// Modal (vim-style) editing in the input box.
    pub vim_mode: bool,
    pub overrides: BTreeMap<String, String>,
}

impl Default for KeysCfg {
    fn default() -> Self {
        Self {
            send: "enter".into(),
            newline: "alt+enter".into(),
            interrupt: "esc".into(),
            toggle_mode: "tab".into(),
            new_session: "ctrl+n".into(),
            close_session: "ctrl+w".into(),
            palette: "ctrl+p".into(),
            side_panel: "ctrl+b".into(),
            android: "alt+a".into(),
            quit: "ctrl+q".into(),
            vim_mode: false,
            overrides: BTreeMap::new(),
        }
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(default)]
pub struct NetworkCfg {
    /// `http://host:port` or `socks5://host:port`. Empty = use environment.
    pub proxy: String,
    pub connect_timeout_secs: u64,
    /// Whole-request timeout, seconds (0 = none; streams can run long).
    pub request_timeout_secs: u64,
    pub max_retries: u32,
    pub retry_backoff_ms: u64,
    /// PEM bundle for corporate TLS interception. Empty = system roots.
    pub ca_bundle: String,
    pub user_agent: String,
    /// Extra HTTP headers on every API request.
    pub extra_headers: BTreeMap<String, String>,
}

impl Default for NetworkCfg {
    fn default() -> Self {
        Self {
            proxy: String::new(),
            connect_timeout_secs: 15,
            request_timeout_secs: 0,
            max_retries: 2,
            retry_backoff_ms: 800,
            ca_bundle: String::new(),
            user_agent: String::new(),
            extra_headers: BTreeMap::new(),
        }
    }
}

/// The built-in `web_fetch` tool (on by default).
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(default)]
pub struct WebFetchCfg {
    /// Give the agent the `web_fetch` tool.
    pub enabled: bool,
    /// Mode used when the model doesn't pick one: text, markdown, raw, json,
    /// links, headers. (`scrape` needs a per-call selector, so it is not a
    /// valid DEFAULT mode — validation rewrites it to `text`.)
    pub default_mode: String,
    /// Response bytes kept per fetch; the rest is discarded.
    pub max_body_bytes: usize,
    /// Characters returned to the model per fetch (hard cap).
    pub max_output_chars: usize,
    /// Whole-request timeout, seconds.
    pub timeout_secs: u64,
    /// Redirect hops followed (each hop is re-checked).
    pub max_redirects: u32,
    /// User-Agent. Empty = `Mozilla/5.0 (compatible; RomaCode/<version>; +web_fetch)`.
    pub user_agent: String,
    /// Allow loopback / private / link-local targets (localhost, 10.x, 192.168.x, 169.254.x, …).
    /// On by default: the agent may fetch any URL, including localhost.
    pub allow_private_hosts: bool,
    /// If non-empty, only these domains (and subdomains) may be fetched.
    pub allowed_domains: Vec<String>,
    /// These domains (and subdomains) are never fetched.
    pub blocked_domains: Vec<String>,
    /// Enable `render` mode (JavaScript pages via a local Chromium/Chrome). Off by default: the
    /// browser does its own DNS and sub-requests, which the private-address guard cannot pin.
    pub render: bool,
    /// Browser executable for `render`. Empty = search PATH for chromium / google-chrome.
    pub browser: String,
}

impl Default for WebFetchCfg {
    fn default() -> Self {
        Self {
            enabled: true,
            default_mode: "text".into(),
            max_body_bytes: 4 * 1024 * 1024,
            max_output_chars: 20_000,
            timeout_secs: 30,
            max_redirects: 5,
            user_agent: String::new(),
            allow_private_hosts: true,
            allowed_domains: Vec::new(),
            blocked_domains: Vec::new(),
            render: false,
            browser: String::new(),
        }
    }
}

/// The built-in `rnmap_scan` tool (vendored rnmap scanner, on by default).
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(default)]
pub struct RnmapCfg {
    /// Give the agent the `rnmap_scan` tool.
    pub enabled: bool,
    /// Explicit path to an rnmap binary. Empty = the auto-installed vendored
    /// binary (`<data_dir>/romacode/rnmap/bin/rnmap`), then PATH.
    pub binary: String,
    /// Port spec used when the model doesn't pick one (`80`, `1-1000`, `22,80,443`).
    pub default_ports: String,
    /// Timing template 0 (paranoid) … 5 (insane) used when not specified.
    pub default_timing: u8,
    /// Whole-scan timeout, seconds (0 = none; a hanging scan is killed at the cap).
    pub timeout_secs: u64,
    /// Safety cap on targets per scan (IPs after CIDR expansion counts the CIDR once).
    pub max_targets: usize,
    /// Characters of scan report returned to the model (hard cap).
    pub max_output_chars: usize,
    /// Allow scanning public internet hosts. On by default: the agent is fully
    /// open and may scan any target the user asks for.
    pub allow_external_targets: bool,
    /// Allow IDS-evasion options (decoys, packet fragmentation). On by default.
    pub allow_evasion: bool,
    /// Allow the Rhai service-probe scripts (`--script`). They actively probe
    /// discovered services, like the built-in service detection does.
    pub allow_scripts: bool,
}

impl Default for RnmapCfg {
    fn default() -> Self {
        Self {
            enabled: true,
            binary: String::new(),
            default_ports: "1-1000".into(),
            default_timing: 3,
            timeout_secs: 300,
            max_targets: 4096,
            max_output_chars: 20_000,
            allow_external_targets: true,
            allow_evasion: true,
            allow_scripts: true,
        }
    }
}

/// The built-in `remix` tool: RomaCode's native Ethereum toolchain, backed by
/// the vendored **rrx** binary (Remix IDE tools rebuilt in pure Rust — real
/// EVM interpreter, real solc, live mainnet with endpoint failover). The
/// binary ships in the `bin/` directory next to the romacode binary and is
/// discovered automatically; set `binary` for an explicit path.
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(default)]
pub struct RemixCfg {
    /// Give the agent the `remix` + `remix_status` tools.
    pub enabled: bool,
    /// Explicit path to the rrx binary. Empty = exe-adjacent `bin/rrx`
    /// (the shipped layout) → `<data_dir>/romacode/bin/rrx` → PATH → project
    /// `bin/rrx`.
    pub binary: String,
    /// Default JSON-RPC endpoint override for chain-facing commands (passed
    /// as `RRX_RPC`). Empty = rrx's built-in mainnet failover list.
    pub rpc: String,
    /// Whole-command timeout, seconds (0 = none; a hanging command is killed
    /// at the cap). compile/test/fuzz/invariant/coverage are the slow ones.
    pub timeout_secs: u64,
    /// Characters of command output returned to the model (hard cap).
    pub max_output_chars: usize,
    /// Allow the chain-writing commands (`send` / `deploy` / `call` / `script`
    /// runs / demo deploys) to broadcast REAL transactions. Keys are NEVER
    /// stored or typed by RomaCode: rrx reads `RRX_PRIVATE_KEY` from the
    /// environment the server already has; while this flag is off the write
    /// commands are refused with an explanation AND the key env var is
    /// stripped from the child process.
    pub allow_chain_writes: bool,
}

impl Default for RemixCfg {
    fn default() -> Self {
        Self {
            enabled: true,
            binary: String::new(),
            rpc: String::new(),
            timeout_secs: 300,
            max_output_chars: 20_000,
            allow_chain_writes: false,
        }
    }
}

/// The built-in **documents engine** (`rc-docs`): Word, Excel, PowerPoint,
/// PDF, charts, canvas design and the Figma REST client — all pure Rust,
/// builtin native. On by default; tools run in every mode without asking
/// like the rest of the fully-open policy.
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(default)]
pub struct DocumentsCfg {
    /// Give the agent the documents tools (docx_*, xlsx_*, pptx_*, pdf_*,
    /// chart_render, canvas_design, figma_*, documents_status).
    pub enabled: bool,
    /// Characters of tool output returned to the model (hard cap; 0 = no cap).
    pub max_output_chars: usize,
}

impl Default for DocumentsCfg {
    fn default() -> Self {
        Self {
            enabled: true,
            max_output_chars: 40_000,
        }
    }
}

/// The built-in `browser` tool: the agent drives a real browser engine
/// (Servo `servoshell`) as its own component, with full frontend and backend
/// access. Fully open by default — any URL the task calls for may be visited.
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(default)]
pub struct BrowserCfg {
    /// Give the agent the `browser` tool.
    pub enabled: bool,
    /// Path to the Servo engine binary (`servo`/`servoshell`). Empty =
    /// `ROMACODE_SERVO` env → vendored install
    /// (`<data_dir>/romacode/servo/bin/`) → `PATH`.
    pub binary: String,
    /// Interface the engine's WebDriver server listens on.
    pub host: String,
    /// Fixed WebDriver port. `0` = pick a free ephemeral port at launch.
    pub port: u16,
    /// Run the engine without a visible window (recommended; the agent still
    /// gets full frontend access through screenshots + DOM).
    pub headless: bool,
    /// Accept self-signed certificates in the engine session.
    pub accept_insecure_certs: bool,
    /// Launch automatically on the first `browser` action.
    pub auto_start: bool,
    /// Seconds to wait for the engine + WebDriver server to come up.
    pub start_timeout_secs: u64,
    /// Per-WebDriver-call timeout, seconds.
    pub timeout_secs: u64,
    /// Extra CLI arguments passed to the engine verbatim.
    pub extra_args: Vec<String>,
    /// Characters of browser output returned to the model (hard cap).
    pub max_output_chars: usize,
}

impl Default for BrowserCfg {
    fn default() -> Self {
        Self {
            enabled: true,
            binary: String::new(),
            host: "127.0.0.1".into(),
            port: 0,
            headless: true,
            accept_insecure_certs: true,
            auto_start: true,
            start_timeout_secs: 90,
            timeout_secs: 120,
            extra_args: Vec::new(),
            max_output_chars: 200_000,
        }
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(default)]
pub struct SessionsCfg {
    /// Reopen the previous sessions at start-up.
    pub restore_on_start: bool,
    pub autosave: bool,
    pub max_saved: u32,
    /// Where sessions are written. Empty = `<data_dir>/romacode/sessions`.
    pub dir: String,
    /// Input history entries kept.
    pub history_limit: u32,
}

impl Default for SessionsCfg {
    fn default() -> Self {
        Self {
            restore_on_start: true,
            autosave: true,
            max_saved: 100,
            dir: String::new(),
            history_limit: 1_000,
        }
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(default)]
pub struct PrivacyCfg {
    /// Anonymous usage statistics. Off by default; RomaCode sends none today.
    pub telemetry: bool,
    /// Mask API keys and tokens found in tool output before sending it on.
    pub redact_secrets: bool,
    /// Write full transcripts to disk.
    pub save_transcripts: bool,
}

impl Default for PrivacyCfg {
    fn default() -> Self {
        Self {
            telemetry: false,
            redact_secrets: true,
            save_transcripts: true,
        }
    }
}

/// Settings for the host ADB connection to a real Android target. The first four keys are the
/// original v1 settings.
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(default)]
pub struct AndroidCfg {
    /// Use the host Android Debug Bridge to control an actual SDK emulator or device.
    ///
    /// Recognised values:
    /// * `"adb"` (default) — shell out to the host `adb` binary for every operation.
    /// * `"native"` — speak the ADB wire protocol directly to `native_target`
    ///   via `mandroid::adb::AdbDevice::connect`. No `adb` binary required.
    /// * anything else — boot the in-process `phone` simulator and connect the
    ///   native wire client to it (legacy development mode).
    pub backend: String,
    /// Path to Android Platform-Tools `adb`, or `adb` to resolve it from PATH.
    pub adb_path: String,
    /// Exact `adb devices` serial. Empty means require exactly one online target.
    pub serial: String,
    /// When `backend == "native"`, the `host:port` of the real device's or
    /// emulator's adbd to connect to (e.g. `"127.0.0.1:5554"` for an SDK
    /// emulator or `"192.168.1.42:5555"` for a wireless-debugging device).
    /// Empty when `backend != "native"`.
    pub native_target: String,
    /// Connect to the real target automatically when RomaCode starts.
    pub auto_start: bool,
    /// Preferred adb port (steps +2 when busy, like real devices).
    pub port: u16,
    /// Physical display panel: [width, height] px.
    pub display: [u32; 2],
    /// Display density in dpi.
    pub density: u32,
    /// Address the embedded adbd listens on. Keep loopback unless you know why not.
    pub bind: String,
    /// Open the scrcpy-style desktop window on start.
    pub open_window: bool,
    /// Desktop window scale (0.25–2.0).
    pub window_scale: f32,
    /// Always-on-top desktop window.
    pub window_on_top: bool,
    /// System theme: `dark` or `light`.
    pub system_theme: String,
    /// Wallpaper index (0 Midnight Aurora, 1 Emerald Dusk, 2 Rose Nebula).
    pub wallpaper: u32,
    pub boot_animation: bool,
    /// Keep phone state (apps, files, photos) between runs.
    pub persist: bool,
    /// Let the phone's Chrome reach the real network.
    pub network: bool,
    /// Phone refresh rate, frames per second.
    pub fps: u8,
    /// Data directory override. Empty = default.
    pub data_dir: String,
}

impl Default for AndroidCfg {
    fn default() -> Self {
        Self {
            backend: "adb".into(),
            adb_path: "adb".into(),
            serial: String::new(),
            native_target: String::new(),
            auto_start: true,
            port: 5555,
            display: [720, 1560],
            density: 420,
            bind: "127.0.0.1".into(),
            open_window: false,
            window_scale: 0.5,
            window_on_top: false,
            system_theme: "dark".into(),
            wallpaper: 0,
            boot_animation: true,
            persist: true,
            network: true,
            fps: 30,
            data_dir: String::new(),
        }
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(default)]
pub struct AdvancedCfg {
    pub log_level: LogLevel,
    /// Log file path. Empty = no file logging.
    pub log_file: String,
    /// Environment variables set for every shell command the agent runs.
    pub env: BTreeMap<String, String>,
    /// Feature flags for unfinished functionality.
    pub experimental: BTreeMap<String, bool>,
}

impl Default for AdvancedCfg {
    fn default() -> Self {
        Self {
            log_level: LogLevel::Info,
            log_file: String::new(),
            env: BTreeMap::new(),
            experimental: BTreeMap::new(),
        }
    }
}

// ── root ─────────────────────────────────────────────────────────────────────

/// One saved database connection profile. Used by the `db_use` tool to switch
/// the active target between local dbs and remote engines.
///
/// `url_env` holds the *name* of the environment variable that contains the
/// real connection URL (so the URL itself — including its password — never
/// appears in `config.json`). `url` is only used as an optional inline
/// fallback for development.
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(default)]
pub struct DatabaseProfile {
    /// Short id used in `db_use`, `db list`, and the TUI Database Access panel.
    pub id: String,
    /// Engine kind: `local` (the built-in engine), `postgres`, `mysql`,
    /// `mongodb`, `redis`, or `http`.
    pub kind: String,
    /// Environment variable holding the connection URL (preferred).
    pub url_env: String,
    /// Inline URL fallback (avoid in real configs — credentials would land on
    /// disk; `url_env` is always preferred). Masked everywhere it is logged.
    pub url: String,
    /// When `true`, even Full-Access agents run this profile read-only.
    pub readonly: bool,
    /// Optional chisel tunnel target this profile connects through, e.g.
    /// `"ssh://user@host"` or a named tunnel profile id. Empty = connect
    /// directly. See `crates/tunnel`.
    pub tunnel: String,
}

impl Default for DatabaseProfile {
    fn default() -> Self {
        Self {
            id: String::new(),
            kind: "local".into(),
            url_env: String::new(),
            url: String::new(),
            readonly: false,
            tunnel: String::new(),
        }
    }
}

/// The `databases` config section: access tier, saved profiles, default db.
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(default)]
pub struct DatabasesCfg {
    /// Access tier enforced by the SQL guard: `"read-only"` (default),
    /// `"read-write"`, or `"full"`. Set from the TUI Database Access radio.
    pub tier: String,
    /// Allow the agent to reach remote/cloud DBs (Neon, Supabase, Atlas…)
    /// over the network. Mirrors the TUI "Remote Database" toggle. When
    /// `false`, remote profiles are listed but `db_use` of them is refused.
    pub remote_enabled: bool,
    /// Saved connection profiles (local + remote).
    pub profiles: Vec<DatabaseProfile>,
    /// Default local db name used when no `db_use` has run yet.
    pub default: String,
    /// On-disk home for the built-in engine. Empty = `~/.romacode/db`.
    pub home: String,
}

impl Default for DatabasesCfg {
    fn default() -> Self {
        Self {
            tier: "read-only".into(),
            remote_enabled: false,
            profiles: Vec::new(),
            default: String::new(),
            home: String::new(),
        }
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(default)]
pub struct Config {
    /// Schema version; bumped when keys change meaning.
    pub version: u32,

    // ── core (v1 keys, unchanged) ────────────────────────────────────────
    /// Active provider id: a known one (`anthropic`, `openai`, `openrouter`,
    /// `google`, `groq`, `deepseek`, `mistral`, `xai`, `together`, `fireworks`,
    /// `cerebras`, `ollama`, `lmstudio`) or any id defined in `providers`.
    pub provider: String,
    /// Model id. Empty = the active provider's default model.
    pub model: String,
    /// Override: environment variable holding the API key of the active
    /// provider. Empty = the provider's own (`providers.<id>.api_key_env`).
    pub api_key_env: String,
    /// Override: base URL of the active provider. Empty = the provider's own.
    pub api_base: String,
    pub max_tokens: u32,
    pub system_prompt: Option<String>,
    /// Context window used for the sidebar meter.
    pub context_window: u64,
    /// Cost estimate, USD per million tokens (editable; estimate only).
    pub price_in_per_mtok: f64,
    pub price_out_per_mtok: f64,
    /// Share of the width given to chat when a device panel is open (%).
    pub chat_width_pct: u16,
    pub sidebar: bool,

    // ── v2 sections ──────────────────────────────────────────────────────
    /// Known-provider overrides and custom providers, keyed by id.
    pub providers: BTreeMap<String, ProviderCfg>,
    pub model_params: ModelParams,
    pub agent: AgentCfg,
    pub permissions: PermissionsCfg,
    pub ui: UiCfg,
    pub keys: KeysCfg,
    pub network: NetworkCfg,
    pub sessions: SessionsCfg,
    pub privacy: PrivacyCfg,
    /// The built-in `web_fetch` tool.
    pub webfetch: WebFetchCfg,
    /// The built-in `rnmap_scan` tool (vendored rnmap port scanner).
    pub rnmap: RnmapCfg,
    /// The built-in `remix` tool (vendored rrx Ethereum toolchain).
    pub remix: RemixCfg,
    /// The built-in `browser` tool (agent-driven Servo engine).
    pub browser: BrowserCfg,
    /// The built-in documents engine (Word/Excel/PowerPoint/PDF/charts/canvas/Figma).
    pub documents: DocumentsCfg,
    /// Real Android emulator/device connection.
    pub android: AndroidCfg,
    /// Built-in database engine: access tier, remote toggle, saved profiles.
    pub databases: DatabasesCfg,
    pub advanced: AdvancedCfg,
    /// OpenCode-style TUI settings (`/settings`): theme, session, tabs,
    /// diffs, input, alerts, terminal, debug. See `docs/SETTINGS.md`.
    pub tui: romacode_shared::tui_settings::TuiSettings,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            version: SCHEMA_VERSION,
            provider: "anthropic".into(),
            model: String::new(),
            api_key_env: String::new(),
            api_base: String::new(),
            max_tokens: 4096,
            system_prompt: None,
            context_window: 200_000,
            price_in_per_mtok: 3.0,
            price_out_per_mtok: 15.0,
            chat_width_pct: 36,
            sidebar: true,
            providers: BTreeMap::new(),
            model_params: ModelParams::default(),
            agent: AgentCfg::default(),
            permissions: PermissionsCfg::default(),
            ui: UiCfg::default(),
            keys: KeysCfg::default(),
            network: NetworkCfg::default(),
            sessions: SessionsCfg::default(),
            privacy: PrivacyCfg::default(),
            webfetch: WebFetchCfg::default(),
            rnmap: RnmapCfg::default(),
            remix: RemixCfg::default(),
            browser: BrowserCfg::default(),
            documents: DocumentsCfg::default(),
            android: AndroidCfg::default(),
            databases: DatabasesCfg::default(),
            advanced: AdvancedCfg::default(),
            tui: Default::default(),
        }
    }
}

fn is_hex_colour(s: &str) -> bool {
    s.len() == 7 && s.starts_with('#') && s[1..].chars().all(|c| c.is_ascii_hexdigit())
}

impl Config {
    pub fn path() -> Option<PathBuf> {
        dirs::config_dir().map(|d| d.join("romacode").join("config.json"))
    }

    /// Load the config, apply `ROMACODE_*` overrides and validate. Never
    /// fails: problems become defaults plus warnings (see [`Config::load_report`]).
    pub fn load() -> Self {
        Self::load_report().0
    }

    /// Like [`Config::load`] but also returns human-readable warnings
    /// (parse failure, clamped values, unknown enum spellings, …).
    pub fn load_report() -> (Self, Vec<String>) {
        let mut warnings = Vec::new();
        let mut cfg = match Self::path() {
            Some(p) => match std::fs::read_to_string(&p) {
                Ok(text) => match serde_json::from_str::<Config>(&text) {
                    Ok(c) => c,
                    Err(e) => {
                        // keep the user's file; never replace it with defaults
                        let bak = p.with_extension("json.bak");
                        let _ = std::fs::copy(&p, &bak);
                        warnings.push(format!(
                            "{}: {e} — using defaults this session (original kept at {})",
                            p.display(),
                            bak.display()
                        ));
                        Config::default()
                    }
                },
                Err(_) => Config::default(), // no file yet
            },
            None => Config::default(),
        };
        if cfg.version < SCHEMA_VERSION {
            // v1 → v2: every new section is already defaulted by serde
            cfg.version = SCHEMA_VERSION;
        } else if cfg.version > SCHEMA_VERSION {
            warnings.push(format!(
                "config version {} is newer than this build ({}); unknown settings are ignored",
                cfg.version, SCHEMA_VERSION
            ));
        }
        cfg.apply_env(|k| std::env::var(k).ok());
        warnings.extend(cfg.validate());
        (cfg, warnings)
    }

    /// Per-run overrides, highest priority:
    /// `ROMACODE_PROVIDER`, `ROMACODE_MODEL`, `ROMACODE_API_KEY` (read by [`Config::resolve`]), `ROMACODE_API_BASE`,
    /// `ROMACODE_MAX_TOKENS`, `ROMACODE_MODE` (build|plan),
    /// `ROMACODE_THEME` (auto|dark|light|bronze), `ROMACODE_PROXY`,
    /// `ROMACODE_LOG` (off|error|warn|info|debug|trace),
    /// `ROMACODE_NO_ANIMATIONS=1`, `ROMACODE_NO_ANDROID=1`.
    pub fn apply_env(&mut self, get: impl Fn(&str) -> Option<String>) {
        let nonempty = |k: &str| {
            get(k)
                .filter(|v| !v.trim().is_empty())
                .map(|v| v.trim().to_string())
        };
        let truthy = |k: &str| matches!(nonempty(k).as_deref(), Some("1" | "true" | "yes" | "on"));
        if let Some(v) = nonempty("ROMACODE_PROVIDER") {
            self.provider = v;
        }
        if let Some(v) = nonempty("ROMACODE_MODEL") {
            self.model = v;
        }
        if let Some(v) = nonempty("ROMACODE_API_BASE") {
            self.api_base = v;
        }
        if let Some(v) = nonempty("ROMACODE_MAX_TOKENS").and_then(|v| v.parse().ok()) {
            self.max_tokens = v;
        }
        match nonempty("ROMACODE_MODE").as_deref() {
            Some("build") => self.agent.default_mode = AgentMode::Build,
            Some("plan") => self.agent.default_mode = AgentMode::Plan,
            _ => {}
        }
        match nonempty("ROMACODE_THEME").as_deref() {
            Some("auto") => self.ui.theme = ThemeMode::Auto,
            Some("dark") => self.ui.theme = ThemeMode::Dark,
            Some("light") => self.ui.theme = ThemeMode::Light,
            Some("bronze") => self.ui.theme = ThemeMode::Bronze,
            _ => {}
        }
        if let Some(v) = nonempty("ROMACODE_PROXY") {
            self.network.proxy = v;
        }
        match nonempty("ROMACODE_LOG").as_deref() {
            Some("off") => self.advanced.log_level = LogLevel::Off,
            Some("error") => self.advanced.log_level = LogLevel::Error,
            Some("warn") => self.advanced.log_level = LogLevel::Warn,
            Some("info") => self.advanced.log_level = LogLevel::Info,
            Some("debug") => self.advanced.log_level = LogLevel::Debug,
            Some("trace") => self.advanced.log_level = LogLevel::Trace,
            _ => {}
        }
        if truthy("ROMACODE_NO_ANIMATIONS") {
            self.ui.animations = false;
        }
        if truthy("ROMACODE_NO_ANDROID") {
            self.android.auto_start = false;
        }
    }

    /// Set one OpenCode-style TUI setting by dotted path from its wire string
    /// (`"show"`, `"on"`, `"0.7"`), validating against the settings catalog.
    /// Settings that mirror a legacy key (`session.sidebar`, `animations`,
    /// `mouse`) keep that key in step.
    pub fn set_tui_pref(&mut self, path: &str, raw: &str) -> Result<(), String> {
        use romacode_shared::tui_settings as t;
        let d = t::def(path).ok_or_else(|| format!("unknown setting {path}"))?;
        let v = t::parse_wire(d, raw)?;
        self.tui.set(path, &v)?;
        match path {
            "session.sidebar" => self.sidebar = self.tui.session.sidebar == "auto",
            "animations" => self.ui.animations = self.tui.animations,
            "mouse" => self.ui.mouse = self.tui.mouse,
            _ => {}
        }
        Ok(())
    }

    /// Clamp every value into its supported range. Returns one warning per
    /// correction so the UI can surface them.
    pub fn validate(&mut self) -> Vec<String> {
        let mut w = Vec::new();
        macro_rules! clamp {
            ($field:expr, $lo:expr, $hi:expr, $name:expr) => {{
                let v = $field;
                let c = v.clamp($lo, $hi);
                if c != v {
                    w.push(format!("{} = {} is out of range; using {}", $name, v, c));
                    $field = c;
                }
            }};
        }
        if !matches!(self.agent.engine.as_str(), "native" | "langchain") {
            w.push(format!(
                "agent.engine = '{}' is not one of native|langchain; using native",
                self.agent.engine
            ));
            self.agent.engine = "native".into();
        }
        clamp!(self.chat_width_pct, 20, 70, "chat_width_pct");
        clamp!(self.max_tokens, 1, 128_000, "max_tokens");
        clamp!(self.context_window, 1_000, 10_000_000, "context_window");
        clamp!(self.agent.max_tool_rounds, 1, 10_000, "agent.max_tool_rounds");
        clamp!(
            self.agent.compact_threshold_pct,
            30,
            98,
            "agent.compact_threshold_pct"
        );
        clamp!(
            self.agent.max_tool_output_bytes,
            1_024,
            8 * 1024 * 1024,
            "agent.max_tool_output_bytes"
        );
        clamp!(self.ui.tab_width, 1, 16, "ui.tab_width");
        clamp!(self.ui.max_fps, 5, 120, "ui.max_fps");
        clamp!(
            self.ui.scrollback_lines,
            100,
            1_000_000,
            "ui.scrollback_lines"
        );
        clamp!(
            self.network.connect_timeout_secs,
            1,
            300,
            "network.connect_timeout_secs"
        );
        clamp!(self.network.max_retries, 0, 10, "network.max_retries");
        clamp!(
            self.webfetch.max_body_bytes,
            4_096,
            64 * 1024 * 1024,
            "webfetch.max_body_bytes"
        );
        clamp!(
            self.webfetch.max_output_chars,
            1_000,
            500_000,
            "webfetch.max_output_chars"
        );
        clamp!(self.webfetch.timeout_secs, 1, 300, "webfetch.timeout_secs");
        clamp!(self.webfetch.max_redirects, 0, 10, "webfetch.max_redirects");
        clamp!(self.browser.port, 0, 65_535, "browser.port");
        clamp!(
            self.browser.start_timeout_secs,
            5,
            900,
            "browser.start_timeout_secs"
        );
        clamp!(self.browser.timeout_secs, 1, 900, "browser.timeout_secs");
        clamp!(
            self.browser.max_output_chars,
            1_000,
            2_000_000,
            "browser.max_output_chars"
        );
        clamp!(self.rnmap.default_timing, 0, 5, "rnmap.default_timing");
        clamp!(self.rnmap.timeout_secs, 0, 3_600, "rnmap.timeout_secs");
        clamp!(self.rnmap.max_targets, 1, 65_535, "rnmap.max_targets");
        clamp!(
            self.rnmap.max_output_chars,
            1_000,
            500_000,
            "rnmap.max_output_chars"
        );
        if self.rnmap.default_ports.trim().is_empty() {
            w.push("rnmap.default_ports is empty; using 1-1000".into());
            self.rnmap.default_ports = "1-1000".into();
        }
        clamp!(self.remix.timeout_secs, 0, 3_600, "remix.timeout_secs");
        clamp!(
            self.remix.max_output_chars,
            1_000,
            200_000,
            "remix.max_output_chars"
        );
        clamp!(self.android.density, 120, 640, "android.density");
        clamp!(self.android.window_scale, 0.25, 2.0, "android.window_scale");
        clamp!(self.android.fps, 1, 60, "android.fps");
        clamp!(self.android.wallpaper, 0, 2, "android.wallpaper");
        clamp!(self.sessions.max_saved, 1, 10_000, "sessions.max_saved");
        clamp!(
            self.sessions.history_limit,
            10,
            100_000,
            "sessions.history_limit"
        );

        for (i, name) in ["width", "height"].iter().enumerate() {
            let v = self.android.display[i];
            let c = v.clamp(240, 4320);
            if c != v {
                w.push(format!(
                    "android.display {name} = {v} is out of range; using {c}"
                ));
                self.android.display[i] = c;
            }
        }
        if let Some(t) = self.model_params.temperature {
            let c = t.clamp(0.0, 1.0);
            if c != t {
                w.push(format!(
                    "model_params.temperature = {t} is out of range; using {c}"
                ));
                self.model_params.temperature = Some(c);
            }
        }
        if let Some(p) = self.model_params.top_p {
            let c = p.clamp(0.0, 1.0);
            if c != p {
                w.push(format!(
                    "model_params.top_p = {p} is out of range; using {c}"
                ));
                self.model_params.top_p = Some(c);
            }
        }
        if self.model_params.thinking && self.model_params.thinking_budget_tokens >= self.max_tokens
        {
            if self.max_tokens == 0 {
                // A zero max_tokens leaves no room for a budget — the old
                // repair computed (0/2).max(1024).min(0) = 0, i.e. a zero
                // budget that silently disabled thinking anyway.
                w.push("model_params.thinking requires max_tokens > 0; disabling thinking".into());
                self.model_params.thinking = false;
            } else {
                let fixed = (self.max_tokens / 2)
                    .max(1_024)
                    .min(self.max_tokens - 1);
                w.push(format!(
                    "model_params.thinking_budget_tokens ({}) must be below max_tokens ({}); using {fixed}",
                    self.model_params.thinking_budget_tokens, self.max_tokens
                ));
                self.model_params.thinking_budget_tokens = fixed;
            }
        }
        if !self.ui.accent.is_empty() && !is_hex_colour(&self.ui.accent) {
            w.push(format!(
                "ui.accent = {:?} is not #rrggbb; ignoring",
                self.ui.accent
            ));
            self.ui.accent.clear();
        }
        if self.android.backend != "adb" {
            w.push(format!(
                "android.backend = {:?} is unsupported; using the real ADB backend",
                self.android.backend
            ));
            self.android.backend = "adb".into();
        }
        self.android.adb_path = self.android.adb_path.trim().to_string();
        if self.android.adb_path.is_empty() {
            self.android.adb_path = "adb".into();
        }
        if !matches!(self.android.system_theme.as_str(), "dark" | "light") {
            w.push(format!(
                "android.system_theme = {:?} must be dark or light; using dark",
                self.android.system_theme
            ));
            self.android.system_theme = "dark".into();
        }
        if self.android.bind.trim().is_empty() {
            self.android.bind = "127.0.0.1".into();
        } else if !matches!(
            self.android.bind.as_str(),
            "127.0.0.1" | "localhost" | "::1"
        ) {
            w.push(format!(
                "android.bind = {:?} exposes the phone's adb server beyond this machine",
                self.android.bind
            ));
        }
        if self.provider.trim().is_empty() {
            w.push("provider is empty; using anthropic".into());
            self.provider = "anthropic".into();
        }
        if !self.api_base.is_empty()
            && !self.api_base.starts_with("https://")
            && !self.api_base.starts_with("http://")
        {
            w.push(format!(
                "api_base {:?} is not an http(s) URL; ignoring",
                self.api_base
            ));
            self.api_base.clear();
        }
        for (id, p) in &self.providers {
            if !p.base_url.is_empty()
                && !p.base_url.starts_with("https://")
                && !p.base_url.starts_with("http://")
            {
                w.push(format!(
                    "providers.{id}.base_url {:?} is not an http(s) URL",
                    p.base_url
                ));
            }
            if crate::providers::find(id).is_none() && p.base_url.is_empty() {
                w.push(format!("custom provider {id:?} has no base_url"));
            }
        }
        if !matches!(
            self.webfetch
                .default_mode
                .trim()
                .to_ascii_lowercase()
                .as_str(),
            "text" | "markdown" | "raw" | "json" | "links" | "headers"
        ) {
            w.push(format!(
                "webfetch.default_mode = {:?} is not valid; using text",
                self.webfetch.default_mode
            ));
            self.webfetch.default_mode = "text".into();
        }
        // OpenCode-style TUI settings: clamp hand-edited values.
        w.extend(self.tui.sanitize());
        // `tui.session.sidebar` and the legacy `sidebar` flag are one switch;
        // "hide" from either side wins.
        if self.tui.session.sidebar == "hide" {
            self.sidebar = false;
        } else if !self.sidebar {
            self.tui.session.sidebar = "hide".into();
        }
        w
    }

    /// The model that will actually be used: `model`, else the provider default.
    pub fn effective_model(&self) -> String {
        if !self.model.trim().is_empty() {
            return self.model.trim().to_string();
        }
        let id = self.provider.trim().to_ascii_lowercase();
        let from_user = self
            .providers
            .get(&id)
            .map(|p| p.default_model.trim().to_string())
            .filter(|m| !m.is_empty());
        from_user
            .or_else(|| crate::providers::find(&id).map(|k| k.default_model.to_string()))
            .unwrap_or_default()
    }

    /// Merge known defaults ← `providers.<id>` ← top-level overrides ← env.
    /// Errors (unknown id, missing base URL, missing key) are phrased for the
    /// user: they are shown in the chat instead of a canned reply.
    pub fn resolve(&self) -> Result<Resolved, String> {
        self.resolve_with(|k| std::env::var(k).ok())
    }

    /// [`Config::resolve`] with an injectable environment (used by tests).
    pub fn resolve_with(&self, env: impl Fn(&str) -> Option<String>) -> Result<Resolved, String> {
        let id = self.provider.trim().to_ascii_lowercase();
        if id.is_empty() {
            return Err("no provider selected — set `provider` (try `/providers`)".into());
        }
        let known = crate::providers::find(&id);
        let user = self.providers.get(&id).or_else(|| {
            // ids are case-insensitive
            self.providers
                .iter()
                .find(|(k, _)| k.eq_ignore_ascii_case(&id))
                .map(|(_, v)| v)
        });
        let blank = ProviderCfg::default();
        let u = user.unwrap_or(&blank);
        if known.is_none() && user.is_none() {
            return Err(format!(
                "unknown provider {id:?}. Pick a known one (`/providers`) or define it under \"providers\" with a base_url"
            ));
        }
        let pick = |a: &str, b: &str| {
            if !a.trim().is_empty() {
                a.trim().to_string()
            } else {
                b.to_string()
            }
        };
        let base_url = pick(
            &self.api_base,
            &pick(&u.base_url, known.map_or("", |k| k.base_url.as_str())),
        )
        .trim_end_matches('/')
        .to_string();
        if base_url.is_empty() {
            return Err(format!(
                "provider {id:?} has no base_url — set providers.{id}.base_url"
            ));
        }
        let kind = u.kind.or(known.map(|k| k.kind)).unwrap_or(ApiKind::OpenAi);
        let key_env = pick(
            &self.api_key_env,
            &pick(&u.api_key_env, known.map_or("", |k| k.key_env.as_str())),
        );
        let needs_key = if known.is_some() {
            known.map_or(true, |k| k.needs_key)
        } else {
            !u.no_key
        };
        // key precedence: ROMACODE_API_KEY, providers.<id>.api_key, the env var
        let env_get = |name: &str| {
            env(name)
                .map(|v| v.trim().to_string())
                .filter(|v| !v.is_empty())
        };
        let api_key = env_get("ROMACODE_API_KEY")
            .or_else(|| Some(u.api_key.trim().to_string()).filter(|v| !v.is_empty()))
            .or_else(|| {
                if key_env.is_empty() {
                    None
                } else {
                    env_get(&key_env)
                }
            });
        if needs_key && api_key.is_none() {
            let name = pick(&u.name, known.map_or(id.as_str(), |k| k.name.as_str()));
            let hint = if key_env.is_empty() {
                format!("providers.{id}.api_key")
            } else {
                format!("export {key_env}=… (or providers.{id}.api_key)")
            };
            return Err(format!("no API key for {name}. Set it with `{hint}`, `/apikey <key>`, or `romacode set api-key <key>`"));
        }
        let mut model = self.model.trim().to_string();
        if model.is_empty() {
            model = pick(&u.default_model, known.map_or("", |k| k.default_model.as_str()));
        }
        if model.is_empty() {
            return Err(format!(
                "no model for provider {id:?} — set `model` (or providers.{id}.default_model)"
            ));
        }
        Ok(Resolved {
            name: pick(&u.name, known.map_or(id.as_str(), |k| k.name.as_str())),
            id,
            kind,
            base_url,
            api_key,
            key_env,
            needs_key,
            model,
            headers: u.headers.clone(),
            stream_usage: u.stream_usage,
            tools: u.tools,
        })
    }

    /// Models offered for the active provider: user list first, then the
    /// known provider's suggestions, then the current model.
    pub fn model_choices(&self) -> Vec<String> {
        let id = self.provider.trim().to_ascii_lowercase();
        let mut out: Vec<String> = Vec::new();
        let mut push = |m: &str| {
            if !m.is_empty() && !out.iter().any(|x| x == m) {
                out.push(m.to_string());
            }
        };
        if let Some(p) = self.providers.get(&id) {
            p.models.iter().for_each(|m| push(m));
            push(&p.default_model);
        }
        if let Some(k) = crate::providers::find(&id) {
            k.models.iter().for_each(|m| push(m.as_str()));
        }
        push(&self.model);
        out
    }

    /// Remember the models a live `/models` catalog fetch returned for
    /// `provider_id`, so the list shows up in every model option from now on
    /// (Select Model, `/models`, `romacode models`) instead of living only in
    /// the settings screen that asked for it. Fetched ids are MERGED into the
    /// stored list — already-configured ids keep their position and fetched
    /// ones are appended deduplicated — so a fetch can never drop a model the
    /// user added by hand. Unknown provider ids are ignored unless they are a
    /// known provider or already have a config entry: a typo'd id must not
    /// conjure a junk entry. Persistence (save) is the caller's job.
    pub fn remember_models(&mut self, provider_id: &str, fetched: &[String]) {
        let id = provider_id.trim().to_ascii_lowercase();
        if id.is_empty() || fetched.is_empty() {
            return;
        }
        let known_or_existing =
            crate::providers::find(&id).is_some() || self.providers.contains_key(&id);
        if !known_or_existing {
            return;
        }
        const MAX_MODELS: usize = 512;
        let p = self.providers.entry(id).or_default();
        for m in fetched {
            let m = m.trim();
            if !m.is_empty() && !p.models.iter().any(|x| x == m) && p.models.len() < MAX_MODELS {
                p.models.push(m.to_string());
            }
        }
    }

    /// Switch provider; clears the model so the new provider's default is used.
    pub fn set_provider(&mut self, id: &str) -> Result<(), String> {
        let id = id.trim().to_ascii_lowercase();
        if crate::providers::find(&id).is_none() && !self.providers.contains_key(&id) {
            return Err(format!("unknown provider {id:?} — run `/providers` to see the list, or add it under \"providers\""));
        }
        if self.provider != id {
            self.provider = id;
            self.model.clear();
            self.api_base.clear();
            self.api_key_env.clear();
        }
        Ok(())
    }

    /// Store an API key for the active provider (config file, owner-only).
    pub fn set_api_key(&mut self, key: &str) {
        let id = self.provider.trim().to_ascii_lowercase();
        self.providers.entry(id).or_default().api_key = key.trim().to_string();
    }

    /// Set the base URL of the active provider.
    pub fn set_base_url(&mut self, url: &str) -> Result<(), String> {
        let url = url.trim().trim_end_matches('/');
        if !url.is_empty() && !url.starts_with("http://") && !url.starts_with("https://") {
            return Err("base URL must start with http:// or https://".into());
        }
        let id = self.provider.trim().to_ascii_lowercase();
        self.providers.entry(id).or_default().base_url = url.to_string();
        self.api_base.clear();
        Ok(())
    }

    /// Define or update a custom provider in one call.
    pub fn add_custom_provider(
        &mut self,
        id: &str,
        base_url: &str,
        kind: ApiKind,
    ) -> Result<(), String> {
        let id = id.trim().to_ascii_lowercase();
        if id.is_empty()
            || !id
                .chars()
                .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
        {
            return Err("provider id must be letters, digits, - or _".into());
        }
        let url = base_url.trim().trim_end_matches('/');
        if !url.starts_with("http://") && !url.starts_with("https://") {
            return Err("base URL must start with http:// or https://".into());
        }
        let p = self.providers.entry(id).or_default();
        p.base_url = url.to_string();
        p.kind = Some(kind);
        Ok(())
    }

    /// Atomic save: write a temp file next to the config, then rename.
    pub fn save(&self) {
        let Some(p) = Self::path() else { return };
        if let Some(dir) = p.parent() {
            let _ = std::fs::create_dir_all(dir);
        }
        if let Ok(s) = serde_json::to_string_pretty(self) {
            let tmp = p.with_extension("json.tmp");
            // the file may hold API keys: owner read/write only
            let wrote = {
                #[cfg(unix)]
                {
                    use std::io::Write;
                    use std::os::unix::fs::OpenOptionsExt;
                    std::fs::OpenOptions::new()
                        .write(true)
                        .create(true)
                        .truncate(true)
                        .mode(0o600)
                        .open(&tmp)
                        .and_then(|mut f| f.write_all(s.as_bytes()))
                        .is_ok()
                }
                #[cfg(not(unix))]
                {
                    std::fs::write(&tmp, s).is_ok()
                }
            };
            if wrote && std::fs::rename(&tmp, &p).is_err() {
                let _ = std::fs::remove_file(&tmp);
            }
        }
    }

    /// The effective configuration for diagnostics. Never print stored secrets.
    pub fn to_pretty_json(&self) -> String {
        let Ok(mut value) = serde_json::to_value(self) else {
            return String::new();
        };
        if let Some(providers) = value.get_mut("providers").and_then(|v| v.as_object_mut()) {
            for provider in providers.values_mut() {
                if let Some(p) = provider.as_object_mut() {
                    if p.get("api_key")
                        .and_then(|v| v.as_str())
                        .map_or(false, |s| !s.is_empty())
                    {
                        p.insert(
                            "api_key".into(),
                            serde_json::Value::String("[REDACTED]".into()),
                        );
                    }
                    if let Some(headers) = p.get_mut("headers").and_then(|v| v.as_object_mut()) {
                        for header in headers.values_mut() {
                            *header = serde_json::Value::String("[REDACTED]".into());
                        }
                    }
                }
            }
        }
        if let Some(network) = value.get_mut("network").and_then(|v| v.as_object_mut()) {
            if let Some(headers) = network
                .get_mut("extra_headers")
                .and_then(|v| v.as_object_mut())
            {
                for header in headers.values_mut() {
                    *header = serde_json::Value::String("[REDACTED]".into());
                }
            }
            if network
                .get("proxy")
                .and_then(|v| v.as_str())
                .map_or(false, |s| s.contains('@'))
            {
                network.insert(
                    "proxy".into(),
                    serde_json::Value::String("[REDACTED]".into()),
                );
            }
        }
        serde_json::to_string_pretty(&value).unwrap_or_default()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn printed_config_redacts_keys_and_headers() {
        let mut c = Config::default();
        c.set_api_key("private-api-token");
        c.providers
            .get_mut("anthropic")
            .unwrap()
            .headers
            .insert("X-Token".into(), "private-provider-header".into());
        c.network
            .extra_headers
            .insert("Authorization".into(), "private-network-header".into());
        c.network.proxy = "http://user:password@example.invalid:8080".into();
        let printed = c.to_pretty_json();
        assert!(printed.contains("[REDACTED]"));
        for secret in [
            "private-api-token",
            "private-provider-header",
            "private-network-header",
            "password",
        ] {
            assert!(!printed.contains(secret), "printed config leaked {secret}");
        }
        let parsed: Config = serde_json::from_str(&printed).unwrap();
        assert_eq!(parsed.provider, c.provider);
    }

    #[test]
    fn empty_object_gives_defaults() {
        let c: Config = serde_json::from_str("{}").unwrap();
        assert_eq!(c.version, SCHEMA_VERSION);
        assert_eq!(c.provider, "anthropic");
        assert!(c.model_params.stream);
        assert_eq!(c.permissions.edit, Permission::Allow);
    }

    #[test]
    fn v1_config_still_loads() {
        let v1 = r#"{"provider":"anthropic","model":"m","max_tokens":1000,"sidebar":false,
                     "android":{"auto_start":false,"port":5557,"display":[1080,2400],"density":440}}"#;
        let c: Config = serde_json::from_str(v1).unwrap();
        assert_eq!(c.model, "m");
        assert!(!c.sidebar);
        assert_eq!(c.android.port, 5557);
        assert_eq!(c.android.display, [1080, 2400]);
        assert_eq!(c.android.bind, "127.0.0.1"); // new key defaulted
    }

    #[test]
    fn bundled_example_parses_and_is_all_defaults() {
        let example: Config = serde_json::from_str(include_str!("../../../config.example.json")).unwrap();
        let a = serde_json::to_value(&example).unwrap();
        let b = serde_json::to_value(Config::default()).unwrap();
        assert_eq!(a, b);
    }

    #[test]
    fn webfetch_defaults_are_safe_and_on() {
        let c = Config::default();
        assert!(c.webfetch.enabled && c.webfetch.allow_private_hosts && !c.webfetch.render);
        let mut d = Config::default();
        d.webfetch.default_mode = "scrape".into(); // needs a selector, so not a valid default
        d.webfetch.max_output_chars = 5;
        let w = d.validate();
        assert_eq!(d.webfetch.default_mode, "text");
        assert_eq!(d.webfetch.max_output_chars, 1_000);
        assert!(w.len() >= 2, "{w:?}");
    }

    #[test]
    fn default_round_trips() {
        let a = Config::default();
        let b: Config = serde_json::from_str(&a.to_pretty_json()).unwrap();
        assert_eq!(
            serde_json::to_value(&a).unwrap(),
            serde_json::to_value(&b).unwrap()
        );
    }

    #[test]
    fn validate_clamps_and_reports() {
        let mut c = Config::default();
        c.chat_width_pct = 5;
        c.ui.accent = "orange".into();
        c.android.window_scale = 9.0;
        c.model_params.temperature = Some(3.0);
        c.model_params.thinking = true;
        c.model_params.thinking_budget_tokens = 99_999;
        let w = c.validate();
        assert_eq!(c.chat_width_pct, 20);
        assert!(c.ui.accent.is_empty());
        assert_eq!(c.android.window_scale, 2.0);
        assert_eq!(c.model_params.temperature, Some(1.0));
        assert!(c.model_params.thinking_budget_tokens < c.max_tokens);
        assert!(w.len() >= 5, "{w:?}");
        assert!(Config::default().validate().is_empty());
    }

    #[test]
    fn tui_settings_default_validate_clean_and_roundtrip() {
        let mut c = Config::default();
        assert!(c.validate().is_empty());
        let json = serde_json::to_string(&c).unwrap();
        let back: Config = serde_json::from_str(&json).unwrap();
        assert_eq!(back.tui, c.tui);
        // a file with only one tui key keeps every other default
        let partial: Config = serde_json::from_str(r#"{"tui":{"session":{"thinking":"show"}}}"#).unwrap();
        assert_eq!(partial.tui.session.thinking, "show");
        assert_eq!(partial.tui.leader.timeout, 2000);
    }

    #[test]
    fn set_tui_pref_validates_and_syncs_legacy_keys() {
        let mut c = Config::default();
        c.set_tui_pref("session.verbosity", "high").unwrap();
        assert_eq!(c.tui.session.verbosity, "high");
        assert!(c.set_tui_pref("session.verbosity", "ultra").is_err());
        assert!(c.set_tui_pref("leader.timeout", "5").is_err());
        assert!(c.set_tui_pref("bogus.path", "1").is_err());
        c.set_tui_pref("leader.timeout", "4000").unwrap();
        assert_eq!(c.tui.leader.timeout, 4000);

        c.set_tui_pref("session.sidebar", "hide").unwrap();
        assert!(!c.sidebar);
        c.set_tui_pref("session.sidebar", "auto").unwrap();
        assert!(c.sidebar);
        c.set_tui_pref("animations", "off").unwrap();
        assert!(!c.ui.animations);
        c.set_tui_pref("mouse", "off").unwrap();
        assert!(!c.ui.mouse);
    }

    #[test]
    fn validate_clamps_tui_and_hide_wins_for_sidebar() {
        let mut c: Config = serde_json::from_str(
            r#"{"sidebar":false,"tui":{"leader":{"timeout":999999},"tabs":{"mode":"sideways"}}}"#,
        )
        .unwrap();
        let w = c.validate();
        assert_eq!(c.tui.leader.timeout, 10000);
        assert_eq!(c.tui.tabs.mode, "off");
        assert_eq!(c.tui.session.sidebar, "hide"); // legacy sidebar=false wins
        assert!(w.len() >= 2, "{w:?}");

        let mut c: Config =
            serde_json::from_str(r#"{"tui":{"session":{"sidebar":"hide"}}}"#).unwrap();
        c.validate();
        assert!(!c.sidebar);
    }

    #[test]
    fn env_overrides_win() {
        let mut c = Config::default();
        c.apply_env(|k| match k {
            "ROMACODE_MODEL" => Some("other-model".into()),
            "ROMACODE_MODE" => Some("plan".into()),
            "ROMACODE_THEME" => Some("bronze".into()),
            "ROMACODE_NO_ANDROID" => Some("1".into()),
            _ => None,
        });
        assert_eq!(c.model, "other-model");
        assert_eq!(c.agent.default_mode, AgentMode::Plan);
        assert_eq!(c.ui.theme, ThemeMode::Bronze);
        assert!(!c.android.auto_start);
    }

    fn with_key(mut c: Config, key: &str) -> Config {
        c.set_api_key(key);
        c
    }

    #[test]
    fn known_provider_resolves_with_defaults() {
        let c = with_key(Config::default(), "sk-test");
        let r = c.resolve_with(|_| None).unwrap();
        assert_eq!(r.id, "anthropic");
        assert_eq!(r.kind, ApiKind::Anthropic);
        assert_eq!(r.base_url, "https://api.anthropic.com");
        assert_eq!(r.model, "claude-sonnet-5-5");
        assert_eq!(r.api_key.as_deref(), Some("sk-test"));
        assert_eq!(c.model, ""); // empty = provider default
    }

    #[test]
    fn switching_provider_resets_model_and_urls() {
        let mut c = Config::default();
        c.model = "claude-opus-5-5".into();
        c.api_base = "https://proxy.example/anthropic".into();
        c.set_provider("OpenAI").unwrap();
        c.set_api_key("sk-x");
        let r = c.resolve_with(|_| None).unwrap();
        assert_eq!(r.id, "openai");
        assert_eq!(r.kind, ApiKind::OpenAi);
        assert_eq!(r.base_url, "https://api.openai.com/v1");
        assert_eq!(r.model, "gpt-5");
        assert!(c.set_provider("does-not-exist").is_err());
    }

    #[test]
    fn local_providers_need_no_key() {
        let mut c = Config::default();
        c.set_provider("ollama").unwrap();
        let r = c.resolve_with(|_| None).unwrap();
        assert!(!r.needs_key && r.api_key.is_none());
        assert_eq!(r.base_url, "http://localhost:11434/v1");
    }

    #[test]
    fn custom_provider_round_trip() {
        let mut c = Config::default();
        c.add_custom_provider("my-vllm", "http://10.0.0.5:8000/v1/", ApiKind::OpenAi)
            .unwrap();
        c.providers.get_mut("my-vllm").unwrap().no_key = true;
        c.providers.get_mut("my-vllm").unwrap().default_model = "qwen-coder".into();
        c.set_provider("my-vllm").unwrap();
        let r = c.resolve_with(|_| None).unwrap();
        assert_eq!(r.base_url, "http://10.0.0.5:8000/v1");
        assert_eq!(r.model, "qwen-coder");
        assert_eq!(r.kind, ApiKind::OpenAi);
        // missing base_url is reported, not guessed
        let mut d = Config::default();
        d.providers.insert("broken".into(), ProviderCfg::default());
        d.provider = "broken".into();
        assert!(d.resolve_with(|_| None).unwrap_err().contains("base_url"));
        assert!(c
            .add_custom_provider("bad id!", "http://x", ApiKind::OpenAi)
            .is_err());
        assert!(c
            .add_custom_provider("ok", "ftp://x", ApiKind::OpenAi)
            .is_err());
    }

    #[test]
    fn missing_key_gives_actionable_error() {
        let e = Config::default().resolve_with(|_| None).unwrap_err();
        assert!(
            e.contains("API key") && e.contains("ANTHROPIC_API_KEY"),
            "{e}"
        );
    }

    #[test]
    fn overrides_beat_provider_values() {
        let mut c = with_key(Config::default(), "k");
        c.set_base_url("https://gateway.corp/anthropic/").unwrap();
        assert_eq!(
            c.resolve_with(|_| None).unwrap().base_url,
            "https://gateway.corp/anthropic"
        );
        c.api_base = "https://top.level".into();
        assert_eq!(
            c.resolve_with(|_| None).unwrap().base_url,
            "https://top.level"
        );
        assert!(c.set_base_url("gateway.corp").is_err());
    }

    #[test]
    fn model_choices_merge_sources() {
        let mut c = Config::default();
        c.model = "my-finetune".into();
        c.providers.entry("anthropic".into()).or_default().models = vec!["custom-a".into()];
        let m = c.model_choices();
        assert_eq!(m[0], "custom-a");
        assert!(
            m.contains(&"claude-opus-5-5".to_string()) && m.contains(&"my-finetune".to_string())
        );
    }

    /// Regression for "fetched models never show up in the model options":
    /// `remember_models` is what FetchModels persists through, so a live
    /// catalog must land in the provider entry, merge with ids the user
    /// added, feed `model_choices`, and ignore junk provider ids.
    #[test]
    fn remember_models_persists_fetched_catalog_for_future_selection() {
        let mut c = Config::default();
        // known provider without a config entry — the fetch creates it
        c.remember_models("openai", &["gpt-live-a".to_string(), "gpt-live-b".to_string()]);
        let stored = &c.providers["openai"];
        assert_eq!(stored.models, vec!["gpt-live-a", "gpt-live-b"]);
        c.provider = "openai".into();
        assert!(c.model_choices().contains(&"gpt-live-a".to_string()));

        // a second fetch MERGES: user-added ids keep their place, fetched
        // ones are appended deduplicated, nothing is wiped
        c.providers.get_mut("openai").unwrap().models = vec!["my-custom".into(), "gpt-live-a".into()];
        c.remember_models(
            "OpenAI", // case-insensitive, like the provider id everywhere else
            &[
                "gpt-live-a".to_string(),
                "gpt-new".to_string(),
                "  gpt-spaced  ".to_string(),
                String::new(),
            ],
        );
        assert_eq!(
            c.providers["openai"].models,
            vec!["my-custom", "gpt-live-a", "gpt-new", "gpt-spaced"]
        );

        // empty fetch: no-op; unknown provider: no junk entry
        let before = c.providers.len();
        c.remember_models("openai", &[]);
        c.remember_models("typo-provider", &["m".to_string()]);
        assert_eq!(c.providers.len(), before);
        assert!(c.providers.get("typo-provider").is_none());
    }
}
