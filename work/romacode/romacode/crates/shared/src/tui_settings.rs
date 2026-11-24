//! TUI settings — the OpenCode `/settings` option set, reproduced exactly.
//!
//! Two things live here, shared by the server (which persists them) and the
//! client TUI (which shows and edits them):
//!
//! * [`TuiSettings`] — the typed config tree (`theme`, `leader`, `scroll`,
//!   `attention`, `diffs`, `terminal`, `prompt`, `session`, `tabs`, `mini`,
//!   `debug`, `cursor`, plus the top-level `animations` / `highlights` /
//!   `mouse` / `experimental` keys). Every field has OpenCode's default.
//! * [`CATALOG`] — the rows of the settings dialog, in OpenCode's order and
//!   categories (Appearance, Session, Tabs, Diffs, Input, Alerts, Terminal,
//!   Debug), with the same titles, values, labels, steps and ranges.
//!
//! Paths are dotted (`session.thinking`). [`TuiSettings::get`] /
//! [`TuiSettings::set`] address any setting by path and validate against the
//! catalog, so the wire only ever needs `(path, value)` pairs.

use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::BTreeMap;

// ── typed config tree (defaults = OpenCode's) ───────────────────────────────

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct ThemeCfg {
    /// Theme name (see [`THEME_NAMES`]).
    pub name: String,
    /// `system` | `dark` | `light`.
    pub mode: String,
}
impl Default for ThemeCfg {
    fn default() -> Self {
        Self { name: "opencode".into(), mode: "system".into() }
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct LeaderCfg {
    /// Leader key wait time, ms.
    pub timeout: u32,
}
impl Default for LeaderCfg {
    fn default() -> Self {
        Self { timeout: 2000 }
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct ScrollCfg {
    /// Distance scrolled per input tick.
    pub speed: f64,
    /// Accelerate scrolling from repeated input.
    pub acceleration: bool,
}
impl Default for ScrollCfg {
    fn default() -> Self {
        Self { speed: 3.0, acceleration: false }
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct AttentionCfg {
    pub notifications: bool,
    pub sound: bool,
    /// 0–1.
    pub volume: f64,
    pub sound_pack: String,
    /// Sound file overrides by event (`default`, `question`, `permission`,
    /// `error`, `done`, `subagent_done`).
    pub sounds: BTreeMap<String, String>,
}
impl Default for AttentionCfg {
    fn default() -> Self {
        Self {
            notifications: false,
            sound: false,
            volume: 0.4,
            sound_pack: "opencode.default".into(),
            sounds: BTreeMap::new(),
        }
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct DiffsCfg {
    /// Initial diff source (`branch`, `turn`, …).
    pub source: String,
    /// `word` | `none`.
    pub wrap: String,
    /// Show the diff file tree.
    pub tree: bool,
    /// Show only the selected file patch.
    pub single: bool,
    /// Open the diff side panel full screen (remembers the last toggle).
    pub fullscreen: Option<bool>,
    /// `auto` | `split` | `unified`.
    pub view: String,
}
impl Default for DiffsCfg {
    fn default() -> Self {
        Self {
            source: "branch".into(),
            wrap: "word".into(),
            tree: true,
            single: false,
            fullscreen: None,
            view: "auto".into(),
        }
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct TerminalCfg {
    /// Update the terminal window title.
    pub title: bool,
    /// `manual` | `select` — `manual` on Windows, `select` elsewhere.
    pub copy: String,
}
impl Default for TerminalCfg {
    fn default() -> Self {
        Self {
            title: true,
            copy: if cfg!(windows) { "manual" } else { "select" }.into(),
        }
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct PromptCfg {
    /// Include the active editor file/selection as prompt context.
    pub editor: bool,
    /// `compact` | `full` — how large pastes are displayed.
    pub paste: String,
    /// Show image attachment previews above the prompt input.
    pub image_preview: bool,
}
impl Default for PromptCfg {
    fn default() -> Self {
        Self { editor: true, paste: "compact".into(), image_preview: false }
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct SessionCfg {
    /// `auto` | `hide`.
    pub sidebar: String,
    pub scrollbar: bool,
    /// `bottom` | `keep`.
    pub submit_scroll: String,
    /// `show` | `hide`.
    pub thinking: String,
    /// `auto` | `none`.
    pub grouping: String,
    /// `low` | `medium` | `high`.
    pub verbosity: String,
    pub image_preview: bool,
    /// Show output tokens per second while the model streams.
    pub tps: bool,
    /// `source` | `rendered`.
    pub markdown: String,
    /// `launch` | `inherit`.
    pub new_location: String,
    /// `prompt` | `autoaccept`.
    pub permissions: String,
}
impl Default for SessionCfg {
    fn default() -> Self {
        Self {
            sidebar: "auto".into(),
            scrollbar: false,
            submit_scroll: "bottom".into(),
            thinking: "hide".into(),
            grouping: "auto".into(),
            verbosity: "low".into(),
            image_preview: false,
            tps: true,
            markdown: "rendered".into(),
            new_location: "launch".into(),
            // Fully open default: the agent runs everything without asking.
            // "prompt" remains an explicit opt-in (Ctrl+P → PERMISSIONS).
            permissions: "autoaccept".into(),
        }
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct TabsCfg {
    /// `auto` | `on` | `off`.
    pub mode: String,
    /// `global` | `cwd`.
    pub scope: String,
    /// `horizontal` | `vertical`.
    pub layout: String,
    /// `status` | `numbers`.
    pub indicators: String,
}
impl Default for TabsCfg {
    fn default() -> Self {
        Self {
            mode: "auto".into(),
            scope: "cwd".into(),
            layout: "horizontal".into(),
            indicators: "status".into(),
        }
    }
}

/// `opencode mini` options. `None` = the mini default for that option.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct MiniCfg {
    pub thinking: Option<String>,
    pub tools: Option<String>,
    pub shell_output: Option<String>,
    pub turn_summary: Option<String>,
    pub footer: Option<String>,
    pub splash: Option<String>,
    pub work_spinner: Option<String>,
    pub mono: Option<bool>,
    pub replay: Option<bool>,
    pub replay_limit: Option<u32>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct DebugCfg {
    pub devtools: bool,
    pub timing: bool,
    /// `false` | `true` | `"verbose"`.
    pub turn_tokens: Value,
}
impl Default for DebugCfg {
    fn default() -> Self {
        Self { devtools: false, timing: false, turn_tokens: Value::Bool(false) }
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct CursorCfg {
    /// `block` | `underline` | `line` | `default`.
    pub style: String,
    pub blinking: bool,
}
impl Default for CursorCfg {
    fn default() -> Self {
        Self { style: "block".into(), blinking: true }
    }
}

/// The whole TUI settings tree.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct TuiSettings {
    pub theme: ThemeCfg,
    /// Custom key bindings (command id → binding).
    pub keybinds: BTreeMap<String, String>,
    /// Plugin enablement directives / external package declarations.
    pub plugins: Vec<Value>,
    pub leader: LeaderCfg,
    pub scroll: ScrollCfg,
    pub attention: AttentionCfg,
    pub diffs: DiffsCfg,
    pub terminal: TerminalCfg,
    pub prompt: PromptCfg,
    pub session: SessionCfg,
    pub tabs: TabsCfg,
    pub mini: MiniCfg,
    pub debug: DebugCfg,
    pub experimental: BTreeMap<String, bool>,
    /// Enable interface animations.
    pub animations: bool,
    /// Show release highlights once after updates.
    pub highlights: bool,
    /// Enable terminal mouse capture.
    pub mouse: bool,
    pub cursor: CursorCfg,
}

impl Default for TuiSettings {
    fn default() -> Self {
        Self {
            theme: ThemeCfg::default(),
            keybinds: BTreeMap::new(),
            plugins: Vec::new(),
            leader: LeaderCfg::default(),
            scroll: ScrollCfg::default(),
            attention: AttentionCfg::default(),
            diffs: DiffsCfg::default(),
            terminal: TerminalCfg::default(),
            prompt: PromptCfg::default(),
            session: SessionCfg::default(),
            tabs: TabsCfg::default(),
            mini: MiniCfg::default(),
            debug: DebugCfg::default(),
            experimental: BTreeMap::new(),
            animations: true,
            highlights: true,
            mouse: true,
            cursor: CursorCfg::default(),
        }
    }
}

// ── settings dialog catalog ─────────────────────────────────────────────────

/// How a number is shown in the dialog.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum NumFmt {
    /// `3.00` (`toFixed(2)`).
    Fixed2,
    /// `2000 ms`.
    Ms,
    /// `40%` (`round(value * 100)`).
    Percent,
}

/// What kind of control a row is.
#[derive(Clone, Copy, Debug)]
pub enum Kind {
    /// Cycles through `values`; `labels` are what the dialog shows.
    Choice { values: &'static [&'static str], labels: &'static [&'static str] },
    /// `off` / `on`.
    Toggle,
    /// Stepped number; ← → change it by `step`, clamped to `min..=max`.
    Number { step: f64, min: f64, max: f64, fmt: NumFmt, integer: bool },
    /// Opens a picker (the theme list).
    Picker,
}

/// One row of the settings dialog.
#[derive(Clone, Copy, Debug)]
pub struct Def {
    pub category: &'static str,
    pub title: &'static str,
    pub path: &'static str,
    pub kind: Kind,
    pub keywords: &'static [&'static str],
}

const fn choice(
    category: &'static str,
    title: &'static str,
    path: &'static str,
    values: &'static [&'static str],
    labels: &'static [&'static str],
    keywords: &'static [&'static str],
) -> Def {
    Def { category, title, path, kind: Kind::Choice { values, labels }, keywords }
}
const fn toggle(
    category: &'static str,
    title: &'static str,
    path: &'static str,
    keywords: &'static [&'static str],
) -> Def {
    Def { category, title, path, kind: Kind::Toggle, keywords }
}

/// Category order, as the dialog shows it.
pub const CATEGORIES: &[&str] =
    &["Appearance", "Session", "Tabs", "Diffs", "Input", "Alerts", "Terminal", "Debug"];

/// Every row of `/settings`, in display order.
pub const CATALOG: &[Def] = &[
    // Appearance
    Def { category: "Appearance", title: "Theme", path: "theme.name", kind: Kind::Picker, keywords: &["theme", "colors", "palette"] },
    choice("Appearance", "Color mode", "theme.mode", &["system", "dark", "light"], &["system", "dark", "light"], &["theme", "dark", "light", "mode"]),
    toggle("Appearance", "Animations", "animations", &["animation", "motion"]),
    // Session
    choice("Session", "Sidebar", "session.sidebar", &["hide", "auto"], &["hide", "auto"], &["sidebar", "panel"]),
    toggle("Session", "Scrollbar", "session.scrollbar", &["scrollbar", "scroll"]),
    choice("Session", "Scroll on send", "session.submit_scroll", &["bottom", "keep"], &["jump to latest", "keep position"], &["scroll", "submit", "send"]),
    choice("Session", "Thinking", "session.thinking", &["hide", "show"], &["hide", "show"], &["thinking", "reasoning"]),
    choice("Session", "Markdown", "session.markdown", &["source", "rendered"], &["source", "rendered"], &["markdown", "render"]),
    choice("Session", "Tool grouping", "session.grouping", &["none", "auto"], &["none", "auto"], &["tools", "grouping", "exploration"]),
    choice("Session", "Verbosity", "session.verbosity", &["low", "medium", "high"], &["low", "medium", "high"], &["verbosity", "detail"]),
    toggle("Session", "Transcript images", "session.image_preview", &["images", "preview", "transcript"]),
    toggle("Session", "TPS", "session.tps", &["tps", "tokens per second", "speed"]),
    choice("Session", "New session location", "session.new_location", &["launch", "inherit"], &["launch directory", "active session"], &["location", "directory", "cwd", "new session"]),
    choice("Session", "Permissions", "session.permissions", &["prompt", "autoaccept"], &["prompt", "auto accept"], &["permissions", "approve", "auto accept"]),
    // Tabs
    choice("Tabs", "Mode", "tabs.mode", &["off", "on", "auto"], &["off", "on", "auto"], &["tabs"]),
    choice("Tabs", "Scope", "tabs.scope", &["cwd", "global"], &["current directory", "global"], &["tabs", "scope"]),
    choice("Tabs", "Layout", "tabs.layout", &["horizontal", "vertical"], &["horizontal", "vertical"], &["tabs", "layout"]),
    choice("Tabs", "Indicators", "tabs.indicators", &["status", "numbers"], &["status icons", "always show numbers"], &["tabs", "indicators", "numbers"]),
    // Diffs
    choice("Diffs", "Layout", "diffs.view", &["auto", "split", "unified"], &["auto", "split", "unified"], &["diff", "layout", "split", "unified"]),
    choice("Diffs", "Wrapping", "diffs.wrap", &["none", "word"], &["none", "word"], &["diff", "wrap"]),
    toggle("Diffs", "File tree", "diffs.tree", &["diff", "tree", "files"]),
    toggle("Diffs", "Single patch", "diffs.single", &["diff", "single", "patch"]),
    // Input
    Def { category: "Input", title: "Scroll speed", path: "scroll.speed", kind: Kind::Number { step: 0.25, min: 0.25, max: 10.0, fmt: NumFmt::Fixed2, integer: false }, keywords: &["scroll", "speed"] },
    toggle("Input", "Acceleration", "scroll.acceleration", &["scroll", "acceleration"]),
    toggle("Input", "Mouse", "mouse", &["mouse", "capture"]),
    toggle("Input", "Editor context", "prompt.editor", &["editor", "context", "file"]),
    choice("Input", "Large pastes", "prompt.paste", &["compact", "full"], &["compact", "full"], &["paste", "large"]),
    toggle("Input", "Image previews", "prompt.image_preview", &["image", "preview", "attachment"]),
    Def { category: "Input", title: "Leader timeout", path: "leader.timeout", kind: Kind::Number { step: 250.0, min: 250.0, max: 10000.0, fmt: NumFmt::Ms, integer: true }, keywords: &["leader", "timeout", "key"] },
    // Alerts
    toggle("Alerts", "Notifications", "attention.notifications", &["notifications", "alerts"]),
    toggle("Alerts", "Sounds", "attention.sound", &["sound", "alerts"]),
    Def { category: "Alerts", title: "Volume", path: "attention.volume", kind: Kind::Number { step: 0.1, min: 0.0, max: 1.0, fmt: NumFmt::Percent, integer: false }, keywords: &["volume", "sound"] },
    toggle("Alerts", "Release highlights", "highlights", &["release", "highlights", "whats new"]),
    // Terminal
    toggle("Terminal", "Window title", "terminal.title", &["terminal", "title"]),
    choice("Terminal", "Copy behavior", "terminal.copy", &["manual", "select"], &["manual", "select"], &["copy", "selection", "clipboard"]),
    // Debug
    toggle("Debug", "Developer tools", "debug.devtools", &["debug", "devtools", "developer"]),
];

/// The built-in theme registry (32 entries) plus the dynamic ones.
pub const THEME_NAMES: &[&str] = &[
    "opencode", "system", "aura", "ayu", "catppuccin", "catppuccin-frappe",
    "catppuccin-macchiato", "cobalt2", "cursor", "dracula", "everforest", "flexoki",
    "github", "gruvbox", "kanagawa", "material", "matrix", "mercury", "monokai",
    "nightowl", "nord", "one-dark", "osaka-jade", "orng", "lucent-orng", "palenight",
    "rosepine", "solarized", "synthwave84", "tokyonight", "vesper", "vercel", "zenburn",
    "carbonfox",
];

/// `MiniWorkSpinner` values.
pub const MINI_WORK_SPINNERS: &[&str] = &[
    "block-soft-slide", "block-soft-sweep", "block-low-comet", "block-low-duet",
    "block-shuttle", "block-bridge", "block-squeeze", "small-toggle", "square-toggle",
    "grow-shrink", "quadrant-orbit", "crosshatch", "density-wave", "seed",
];

/// Look a catalog row up by path.
pub fn def(path: &str) -> Option<&'static Def> {
    CATALOG.iter().find(|d| d.path == path)
}

/// Rows matching a search query (title, category, path or keywords), in order.
pub fn search(query: &str) -> Vec<&'static Def> {
    let q = query.trim().to_lowercase();
    CATALOG
        .iter()
        .filter(|d| {
            q.is_empty()
                || d.title.to_lowercase().contains(&q)
                || d.category.to_lowercase().contains(&q)
                || d.path.to_lowercase().contains(&q)
                || d.keywords.iter().any(|k| k.to_lowercase().contains(&q))
        })
        .collect()
}

fn round_to(v: f64, step: f64) -> f64 {
    // Remove float drift (0.1 steps etc.).
    let inv = 1.0 / step;
    ((v * inv).round() / inv * 1e6).round() / 1e6
}

impl Def {
    /// Text the dialog shows for a raw value.
    pub fn display(&self, v: &Value) -> String {
        match self.kind {
            Kind::Choice { values, labels } => {
                let s = v.as_str().unwrap_or("");
                values
                    .iter()
                    .position(|x| *x == s)
                    .map(|i| labels[i].to_string())
                    .unwrap_or_else(|| s.to_string())
            }
            Kind::Toggle => if v.as_bool().unwrap_or(false) { "on" } else { "off" }.into(),
            Kind::Number { fmt, .. } => {
                let n = v.as_f64().unwrap_or(0.0);
                match fmt {
                    NumFmt::Fixed2 => format!("{n:.2}"),
                    NumFmt::Ms => format!("{} ms", n.round() as i64),
                    NumFmt::Percent => format!("{}%", (n * 100.0).round() as i64),
                }
            }
            Kind::Picker => v.as_str().unwrap_or("").to_string(),
        }
    }

    /// The value after stepping `dir` (+1 = right/next, -1 = left/previous)
    /// from `cur`. Choices and toggles wrap; numbers clamp.
    pub fn step(&self, cur: &Value, dir: i32) -> Value {
        match self.kind {
            Kind::Choice { values, .. } => {
                let s = cur.as_str().unwrap_or("");
                let i = values.iter().position(|x| *x == s).unwrap_or(0) as i32;
                let n = values.len() as i32;
                Value::String(values[(((i + dir) % n) + n) as usize % n as usize].into())
            }
            Kind::Toggle => Value::Bool(!cur.as_bool().unwrap_or(false)),
            Kind::Number { step, min, max, integer, .. } => {
                let n = cur.as_f64().unwrap_or(min);
                let v = round_to((n + step * dir as f64).clamp(min, max), step.min(0.01));
                if integer {
                    Value::from(v.round() as i64)
                } else {
                    Value::from(v)
                }
            }
            Kind::Picker => {
                let s = cur.as_str().unwrap_or("");
                let i = THEME_NAMES.iter().position(|x| *x == s).unwrap_or(0) as i32;
                let n = THEME_NAMES.len() as i32;
                Value::String(THEME_NAMES[(((i + dir) % n) + n) as usize % n as usize].into())
            }
        }
    }

    /// Validate (and normalise) a raw value for this row.
    pub fn validate(&self, v: &Value) -> Result<Value, String> {
        match self.kind {
            Kind::Choice { values, .. } => {
                let s = v.as_str().ok_or_else(|| format!("{} expects text", self.path))?;
                if values.contains(&s) {
                    Ok(Value::String(s.into()))
                } else {
                    Err(format!("{} must be one of: {}", self.path, values.join(", ")))
                }
            }
            Kind::Toggle => v
                .as_bool()
                .map(Value::Bool)
                .ok_or_else(|| format!("{} expects true or false", self.path)),
            Kind::Number { min, max, integer, .. } => {
                let n = v.as_f64().ok_or_else(|| format!("{} expects a number", self.path))?;
                if n < min || n > max {
                    return Err(format!("{} must be between {min} and {max}", self.path));
                }
                Ok(if integer { Value::from(n.round() as i64) } else { Value::from(n) })
            }
            Kind::Picker => {
                let s = v.as_str().ok_or_else(|| format!("{} expects text", self.path))?;
                if THEME_NAMES.contains(&s) {
                    Ok(Value::String(s.into()))
                } else {
                    Err(format!("unknown theme {s}"))
                }
            }
        }
    }
}

/// Parse a wire string (`"true"`, `"0.4"`, `"dark"`) into the JSON value the
/// row expects.
pub fn parse_wire(def: &Def, raw: &str) -> Result<Value, String> {
    let raw = raw.trim();
    match def.kind {
        Kind::Toggle => match raw.to_lowercase().as_str() {
            "true" | "1" | "on" | "yes" => Ok(Value::Bool(true)),
            "false" | "0" | "off" | "no" => Ok(Value::Bool(false)),
            _ => Err(format!("{} expects on/off", def.path)),
        },
        Kind::Number { .. } => raw
            .parse::<f64>()
            .map(Value::from)
            .map_err(|_| format!("{} expects a number", def.path)),
        _ => Ok(Value::String(raw.into())),
    }
}

impl TuiSettings {
    /// Current raw value at a dotted path.
    pub fn get(&self, path: &str) -> Option<Value> {
        let mut v = serde_json::to_value(self).ok()?;
        for part in path.split('.') {
            v = v.get(part)?.clone();
        }
        Some(v)
    }

    /// Set a catalog setting by dotted path. The value is validated against
    /// the catalog row; unknown paths are rejected.
    pub fn set(&mut self, path: &str, value: &Value) -> Result<(), String> {
        let d = def(path).ok_or_else(|| format!("unknown setting {path}"))?;
        let value = d.validate(value)?;
        let mut root = serde_json::to_value(&*self).map_err(|e| e.to_string())?;
        let parts: Vec<&str> = path.split('.').collect();
        let mut cur = &mut root;
        for p in &parts[..parts.len() - 1] {
            cur = cur
                .get_mut(*p)
                .ok_or_else(|| format!("unknown setting {path}"))?;
        }
        cur[parts[parts.len() - 1]] = value;
        *self = serde_json::from_value(root).map_err(|e| e.to_string())?;
        Ok(())
    }

    /// Dialog text for a path (`"jump to latest"`, `"2000 ms"`, `"on"`).
    pub fn display(&self, path: &str) -> String {
        match (def(path), self.get(path)) {
            (Some(d), Some(v)) => d.display(&v),
            _ => String::new(),
        }
    }

    /// Step a setting left/right and return the new value.
    pub fn step(&mut self, path: &str, dir: i32) -> Result<Value, String> {
        let d = def(path).ok_or_else(|| format!("unknown setting {path}"))?;
        let cur = self.get(path).ok_or_else(|| format!("unknown setting {path}"))?;
        let next = d.step(&cur, dir);
        self.set(path, &next)?;
        Ok(next)
    }

    /// Clamp every catalog value into its range after loading a hand-edited
    /// file. Returns one warning per correction.
    pub fn sanitize(&mut self) -> Vec<String> {
        let mut warnings = Vec::new();
        for d in CATALOG {
            let Some(cur) = self.get(d.path) else { continue };
            if d.validate(&cur).is_ok() {
                continue;
            }
            let fixed = match d.kind {
                Kind::Number { min, max, integer, .. } => {
                    let n = cur.as_f64().unwrap_or(min).clamp(min, max);
                    if integer { Value::from(n.round() as i64) } else { Value::from(n) }
                }
                Kind::Choice { values, .. } => Value::String(values[0].into()),
                Kind::Picker => Value::String("opencode".into()),
                // Normalize invalid Toggle values to the bool the readers
                // fall back to — `continue` used to leave the broken value
                // persisted while every reader silently read `false`.
                Kind::Toggle => Value::Bool(false),
            };
            if self.set(d.path, &fixed).is_ok() {
                warnings.push(format!("tui.{} out of range; using {}", d.path, d.display(&fixed)));
            }
        }
        warnings
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn catalog_paths_exist_in_the_tree_and_defaults_validate() {
        let t = TuiSettings::default();
        for d in CATALOG {
            let v = t.get(d.path).unwrap_or_else(|| panic!("missing {}", d.path));
            d.validate(&v).unwrap_or_else(|e| panic!("{e}"));
        }
    }

    #[test]
    fn catalog_has_every_opencode_setting_in_order() {
        assert_eq!(CATALOG.len(), 36);
        let cats: Vec<&str> = {
            let mut c: Vec<&str> = Vec::new();
            for d in CATALOG {
                if c.last() != Some(&d.category) {
                    c.push(d.category);
                }
            }
            c
        };
        assert_eq!(cats, CATEGORIES);
        assert_eq!(THEME_NAMES.len(), 34);
        assert_eq!(MINI_WORK_SPINNERS.len(), 14);
    }

    #[test]
    fn opencode_defaults() {
        let t = TuiSettings::default();
        assert_eq!(t.theme.name, "opencode");
        assert_eq!(t.theme.mode, "system");
        assert_eq!(t.leader.timeout, 2000);
        assert_eq!(t.scroll.speed, 3.0);
        assert!(!t.scroll.acceleration);
        assert_eq!(t.attention.volume, 0.4);
        assert_eq!(t.attention.sound_pack, "opencode.default");
        assert_eq!(t.diffs.source, "branch");
        assert_eq!(t.diffs.view, "auto");
        assert!(t.diffs.tree && !t.diffs.single);
        assert!(t.prompt.editor && !t.prompt.image_preview);
        assert_eq!(t.prompt.paste, "compact");
        assert_eq!(t.session.sidebar, "auto");
        assert_eq!(t.session.thinking, "hide");
        assert_eq!(t.session.verbosity, "low");
        assert!(t.session.tps);
        // shipped default is fully open (the agent runs everything)
        assert_eq!(t.session.permissions, "autoaccept");
        assert_eq!(t.tabs.mode, "auto");
        assert_eq!(t.tabs.scope, "cwd");
        assert!(t.animations && t.highlights && t.mouse);
        assert_eq!(t.cursor.style, "block");
        assert!(t.cursor.blinking);
    }

    #[test]
    fn choice_cycles_and_wraps_both_ways() {
        let mut t = TuiSettings::default();
        assert_eq!(t.step("session.verbosity", 1).unwrap(), "medium");
        assert_eq!(t.step("session.verbosity", 1).unwrap(), "high");
        assert_eq!(t.step("session.verbosity", 1).unwrap(), "low");
        assert_eq!(t.step("session.verbosity", -1).unwrap(), "high");
        assert_eq!(t.display("session.submit_scroll"), "jump to latest");
        t.step("session.submit_scroll", 1).unwrap();
        assert_eq!(t.display("session.submit_scroll"), "keep position");
    }

    #[test]
    fn numbers_step_clamp_and_format() {
        let mut t = TuiSettings::default();
        assert_eq!(t.display("scroll.speed"), "3.00");
        t.step("scroll.speed", 1).unwrap();
        assert_eq!(t.display("scroll.speed"), "3.25");
        for _ in 0..100 {
            t.step("scroll.speed", 1).unwrap();
        }
        assert_eq!(t.display("scroll.speed"), "10.00");
        for _ in 0..100 {
            t.step("scroll.speed", -1).unwrap();
        }
        assert_eq!(t.display("scroll.speed"), "0.25");

        assert_eq!(t.display("leader.timeout"), "2000 ms");
        t.step("leader.timeout", 1).unwrap();
        assert_eq!(t.leader.timeout, 2250);
        for _ in 0..100 {
            t.step("leader.timeout", -1).unwrap();
        }
        assert_eq!(t.leader.timeout, 250);

        assert_eq!(t.display("attention.volume"), "40%");
        t.step("attention.volume", 1).unwrap();
        assert_eq!(t.display("attention.volume"), "50%");
        for _ in 0..20 {
            t.step("attention.volume", -1).unwrap();
        }
        assert_eq!(t.display("attention.volume"), "0%");
    }

    #[test]
    fn toggles_and_picker() {
        let mut t = TuiSettings::default();
        assert_eq!(t.display("animations"), "on");
        t.step("animations", 1).unwrap();
        assert!(!t.animations);
        assert_eq!(t.step("theme.name", 1).unwrap(), "system");
        assert_eq!(t.step("theme.name", -1).unwrap(), "opencode");
        assert_eq!(t.step("theme.name", -1).unwrap(), "carbonfox");
    }

    #[test]
    fn set_validates_and_rejects_unknown() {
        let mut t = TuiSettings::default();
        assert!(t.set("session.thinking", &Value::from("show")).is_ok());
        assert_eq!(t.session.thinking, "show");
        assert!(t.set("session.thinking", &Value::from("loud")).is_err());
        assert!(t.set("leader.timeout", &Value::from(100)).is_err());
        assert!(t.set("nope.nothing", &Value::from(1)).is_err());
        assert!(t.set("theme.name", &Value::from("not-a-theme")).is_err());
        assert_eq!(t.session.thinking, "show");
    }

    #[test]
    fn wire_parsing() {
        let d = def("mouse").unwrap();
        assert_eq!(parse_wire(d, "off").unwrap(), Value::Bool(false));
        let d = def("attention.volume").unwrap();
        assert_eq!(parse_wire(d, "0.7").unwrap(), Value::from(0.7));
        assert!(parse_wire(d, "loud").is_err());
    }

    #[test]
    fn sanitize_clamps_hand_edited_values() {
        let mut t: TuiSettings = serde_json::from_str(
            r#"{"leader":{"timeout":99999},"session":{"verbosity":"ultra"},"scroll":{"speed":50}}"#,
        )
        .unwrap();
        let w = t.sanitize();
        assert_eq!(w.len(), 3);
        assert_eq!(t.leader.timeout, 10000);
        assert_eq!(t.session.verbosity, "low");
        assert_eq!(t.scroll.speed, 10.0);
    }

    #[test]
    fn json_roundtrip_and_partial_file() {
        let t: TuiSettings = serde_json::from_str(r#"{"session":{"thinking":"show"}}"#).unwrap();
        assert_eq!(t.session.thinking, "show");
        assert_eq!(t.session.sidebar, "auto"); // untouched default
        let back: TuiSettings = serde_json::from_value(serde_json::to_value(&t).unwrap()).unwrap();
        assert_eq!(back, t);
    }

    #[test]
    fn search_matches_title_category_and_keywords() {
        assert_eq!(search("").len(), CATALOG.len());
        assert!(search("tabs").iter().all(|d| d.category == "Tabs" || d.keywords.contains(&"tabs")));
        assert!(search("leader").iter().any(|d| d.path == "leader.timeout"));
        assert!(search("zzzz").is_empty());
    }
}
