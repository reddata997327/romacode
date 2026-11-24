//! Unified Ctrl+P settings panel — one overlay for commands + provider config
//! + the integrated tools builder.
//!
//! `Ctrl+P` opens it; the root list is grouped into clear sections (Commands,
//! Providers, Tools, Config).
//!
//! Every clickable row is mouse-hit-testable: `draw_settings` records a
//! [`HitRow`] per row each frame into `App::settings_hits`; `on_mouse` turns a
//! click into select+activate and a hover into selection, so the panel works
//! fully with keyboard **and** mouse. Input fields (API key, base URL, tool
//! JSON) support full cursor editing (←→ Home End Delete) and bracketed paste
//! is routed to the focused field.

use romacode_shared::catalog::{self, ApiKind, Known};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Screen {
    /// Unified root: every command + every provider action in one searchable list.
    Root,
    /// Pick a known provider from `KNOWN`.
    Providers,
    /// Enter an API key for a chosen known provider.
    KnownKey,
    /// Enter a display name for a custom provider (first step of the flow).
    CustomName,
    /// Enter a base URL for a custom provider.
    CustomBase,
    /// Enter an API key for a custom provider.
    CustomKey,
    /// Live-fetched model list (after a successful key + fetch).
    Models,
    /// Pick a model already saved under an existing provider.
    SelectModel,
    /// Browse the integrated tools builder catalog (38 tools, 8 categories).
    Tools,
    /// Type a JSON input for a selected tool, then run it.
    ToolsInput,
}

/// What a root-list entry does when activated.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RootAction {
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
    AddKnownProvider,
    AddCustomProvider,
    SelectModel,
    OpenConfigFile,
    PrintEffectiveConfig,
    /// Open the integrated tools builder catalog.
    OpenTools,
}

/// One entry in the unified root list.
#[derive(Clone, Copy, Debug)]
pub struct Entry {
    pub label: &'static str,
    pub category: &'static str,
    pub action: RootAction,
    pub key_hint: &'static str,
}

/// Every entry shown in the unified Ctrl+P panel, in display order.
/// Categories are emitted in the order they first appear.
pub const ENTRIES: &[Entry] = &[
    // Commands
    Entry { label: "New session", category: "Commands", action: RootAction::NewSession, key_hint: "ctrl+n" },
    Entry { label: "Close session", category: "Commands", action: RootAction::CloseSession, key_hint: "ctrl+w" },
    Entry { label: "Next session", category: "Commands", action: RootAction::NextSession, key_hint: "alt+→" },
    Entry { label: "Previous session", category: "Commands", action: RootAction::PrevSession, key_hint: "alt+←" },
    Entry { label: "Toggle side panel", category: "Commands", action: RootAction::TogglePanel, key_hint: "ctrl+b" },
    Entry { label: "Toggle Android window", category: "Commands", action: RootAction::ToggleAndroid, key_hint: "alt+a" },
    Entry { label: "Open native terminal", category: "Commands", action: RootAction::ToggleTerminal, key_hint: "alt+t" },
    Entry { label: "Widen chat", category: "Commands", action: RootAction::WidenChat, key_hint: "alt+]" },
    Entry { label: "Narrow chat", category: "Commands", action: RootAction::NarrowChat, key_hint: "alt+[" },
    Entry { label: "Switch Build / Plan", category: "Commands", action: RootAction::ToggleMode, key_hint: "tab" },
    Entry { label: "Clear session", category: "Commands", action: RootAction::ClearSession, key_hint: "/clear" },
    Entry { label: "Quit RomaCode", category: "Commands", action: RootAction::Quit, key_hint: "ctrl+q" },
    // Providers
    Entry { label: "Add Provider", category: "Providers", action: RootAction::AddKnownProvider, key_hint: "" },
    Entry { label: "Add Custom Provider", category: "Providers", action: RootAction::AddCustomProvider, key_hint: "" },
    Entry { label: "Select Model", category: "Providers", action: RootAction::SelectModel, key_hint: "" },
    // Tools — the integrated tools builder (38 builtin tools, 8 categories).
    Entry { label: "Tools — browse & run (38 builtin)", category: "Tools", action: RootAction::OpenTools, key_hint: "" },
    // Config
    Entry { label: "Open config file", category: "Config", action: RootAction::OpenConfigFile, key_hint: "" },
    Entry { label: "Print effective config (toast)", category: "Config", action: RootAction::PrintEffectiveConfig, key_hint: "" },
];

/// What kind of thing a mouse hit-test row points at — drives `on_mouse`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RowKind {
    /// A row in the root command list (index into the filtered `root_items`).
    RootItem(usize),
    /// A provider row (index into `KNOWN`).
    Provider(usize),
    /// A model row (index into `st.models`).
    Model(usize),
    /// A tool row (index into the filtered `tools_items`).
    Tool(usize),
}

/// One mouse-hit-testable row, recorded by `draw_settings` each frame.
/// Geometry is stored as plain `u16` quartets so this module stays free of the
/// ratatui dependency; `palette.rs` builds these from `Rect`s.
#[derive(Clone, Copy, Debug)]
pub struct HitRow {
    pub x: u16,
    pub y: u16,
    pub w: u16,
    pub h: u16,
    pub kind: RowKind,
}

impl HitRow {
    pub fn contains(&self, x: u16, y: u16) -> bool {
        x >= self.x && x < self.x.saturating_add(self.w) && y >= self.y && y < self.y.saturating_add(self.h)
    }
}

pub struct Settings {
    pub screen: Screen,
    pub sel: usize,
    /// Search query on the Root screen.
    pub query: String,
    /// Search query on the Providers screen (filters by provider name or id).
    pub provider_query: String,
    /// Search query on the Models / SelectModel screens (filters by model id
    /// or its provider id).
    pub model_query: String,
    /// Text input on KnownKey / CustomBase / CustomKey / ToolsInput.
    pub input: String,
    /// Cursor position inside `input` (chars). Input screens support full
    /// cursor editing: ←→ Home End Delete, and Backspace at the cursor.
    pub cursor: usize,
    pub provider_id: String,
    pub provider_name: String,
    /// Display name typed in the first step of the custom-provider flow.
    pub custom_name: String,
    pub kind: ApiKind,
    pub needs_key: bool,
    pub base_url: String,
    pub api_key: String,
    pub models: Vec<String>,
    pub model_providers: Vec<String>,
    pub error: Option<String>,
    pub loading: bool,
    /// Tools: search query.
    pub tools_query: String,
    /// Tools: selected row in the filtered list.
    pub tools_sel: usize,
    /// Tools: the name of the tool a ToolsInput screen is editing.
    pub tools_selected: Option<String>,
    /// Tools: the last run result (ok flag + pretty json or error string).
    pub tools_result: Option<(bool, String)>,
}

impl Settings {
    pub fn root() -> Self {
        Self {
            screen: Screen::Root,
            needs_key: false,
            sel: 0,
            query: String::new(),
            provider_query: String::new(),
            model_query: String::new(),
            input: String::new(),
            cursor: 0,
            provider_id: String::new(),
            provider_name: String::new(),
            custom_name: String::new(),
            kind: ApiKind::OpenAi,
            base_url: String::new(),
            api_key: String::new(),
            models: Vec::new(),
            model_providers: Vec::new(),
            error: None,
            loading: false,
            tools_query: String::new(),
            tools_sel: 0,
            tools_selected: None,
            tools_result: None,
        }
    }

    /// Filtered + ordered list of `(original_index, &Entry)` matching the query.
    /// Empty query returns every entry in declared order.
    pub fn root_items(&self) -> Vec<(usize, &'static Entry)> {
        let q = self.query.to_lowercase();
        ENTRIES
            .iter()
            .enumerate()
            .filter(|(_, e)| {
                q.is_empty()
                    || e.label.to_lowercase().contains(&q)
                    || e.category.to_lowercase().contains(&q)
                    || e.key_hint.to_lowercase().contains(&q)
            })
            .map(|(i, e)| (i, e))
            .collect()
    }

    /// Filtered tool list (names) for the Tools screen, matching the query by
    /// name or category. The actual tool entries come from `App::tools`; this
    /// returns the indices into the full sorted list that match, so the draw
    /// and key handlers stay in sync.
    pub fn tools_filter(&self, all: &[String]) -> Vec<usize> {
        let q = self.tools_query.to_lowercase();
        all.iter()
            .enumerate()
            .filter(|(_, n)| q.is_empty() || n.to_lowercase().contains(&q))
            .map(|(i, _)| i)
            .collect()
    }

    /// Providers matching `provider_query`, as `(original_index, provider)`.
    /// `sel` on the Providers screen is an index into this filtered list.
    pub fn provider_items(&self) -> Vec<(usize, &'static Known)> {
        let q = self.provider_query.trim().to_lowercase();
        catalog::known()
            .iter()
            .enumerate()
            .filter(|(_, k)| {
                q.is_empty()
                    || k.name.to_lowercase().contains(&q)
                    || k.id.to_lowercase().contains(&q)
            })
            .collect()
    }

    /// Models matching `model_query`, as original indices into `self.models`.
    /// Matching the provider id too means "anthropic" finds that provider's
    /// models even when the id itself does not contain the word.
    pub fn model_items(&self) -> Vec<usize> {
        let q = self.model_query.trim().to_lowercase();
        self.models
            .iter()
            .enumerate()
            .filter(|(i, m)| {
                q.is_empty()
                    || m.to_lowercase().contains(&q)
                    || self
                        .model_providers
                        .get(*i)
                        .map_or(false, |p| p.to_lowercase().contains(&q))
            })
            .map(|(i, _)| i)
            .collect()
    }

    /// Providers plus the trailing "Custom provider…" row.
    pub fn provider_total(&self) -> usize {
        self.provider_items().len() + 1
    }

    /// The provider under the cursor on the Providers screen. `None` on the
    /// trailing custom-provider row.
    pub fn selected_provider(&self) -> Option<&'static Known> {
        let items = self.provider_items();
        if self.sel >= items.len() {
            return None;
        }
        let (idx, _) = items.get(self.sel.min(items.len().saturating_sub(1)))?;
        catalog::known_at(*idx)
    }

    /// The original model index under the cursor on a Models screen.
    pub fn selected_model_index(&self) -> Option<usize> {
        let items = self.model_items();
        items.get(self.sel.min(items.len().saturating_sub(1))).copied()
    }

    /// Number of selectable rows on the current screen, after filtering.
    pub fn visible_len(&self, tool_names: &[String]) -> usize {
        match self.screen {
            Screen::Root => self.root_items().len(),
            Screen::Providers => self.provider_total(),
            Screen::Models | Screen::SelectModel => self.model_items().len(),
            Screen::Tools => self.tools_filter(tool_names).len(),
            _ => 0,
        }
    }

    /// Keep the cursor inside the visible list. Call after every key / wheel
    /// event and on entry so a stale index can never point past the end (the
    /// old code only clamped on Down, which made Up from a stale index jump).
    pub fn clamp_sel(&mut self, tool_names: &[String]) {
        let max = self.visible_len(tool_names).saturating_sub(1);
        self.sel = self.sel.min(max);
    }

    #[allow(dead_code)] // panel API parity with the palette
    pub fn field(&self) -> &str {
        &self.input
    }
    #[allow(dead_code)]
    pub fn field_mut(&mut self) -> &mut String {
        &mut self.input
    }
    pub fn begin_known(&mut self, k: &Known) {
        self.screen = Screen::KnownKey;
        self.sel = 0;
        self.input.clear();
        self.cursor = 0;
        self.error = None;
        self.provider_id = k.id.clone();
        self.provider_name = k.name.clone();
        self.kind = k.kind;
        self.needs_key = k.needs_key;
        self.base_url = k.base_url.clone();
        self.api_key.clear();
    }
    /// The custom-provider flow now starts with a NAME, then base URL, then
    /// API key, then the model fetch. The last row of the Providers picker
    /// opens this flow.
    pub fn begin_custom(&mut self) {
        self.screen = Screen::CustomName;
        self.sel = 0;
        self.input.clear();
        self.cursor = 0;
        self.error = None;
        self.provider_id.clear();
        self.provider_name.clear();
        self.custom_name.clear();
        self.kind = ApiKind::OpenAi;
        self.needs_key = false;
        self.base_url.clear();
        self.api_key.clear();
    }

    /// Slug used as the config id for a custom provider name.
    pub fn custom_id_from_name(name: &str) -> String {
        let mut id = String::new();
        let mut dash = false;
        for c in name.trim().to_lowercase().chars() {
            if c.is_ascii_alphanumeric() {
                if dash && !id.is_empty() { id.push('-'); }
                id.push(c);
                dash = false;
            } else {
                dash = true;
            }
        }
        if id.is_empty() { "custom".to_string() } else { id }
    }
    pub fn begin_models(&mut self, models: Vec<String>) {
        self.screen = Screen::Models;
        self.sel = 0;
        self.models = models;
        // This is a single-provider list; a previous SelectModel visit may
        // have left per-row provider ids behind.
        self.model_providers.clear();
        self.model_query.clear();
        self.error = None;
        self.loading = false;
    }

    /// A failed live fetch must not leave an empty model picker: the key was
    /// already saved, so the built-in catalog still gives the user real model
    /// options to pick from (a pick persists through SaveModel like any other),
    /// with the fetch error surfaced alongside so nothing fails silently.
    ///
    /// The model list comes from the bundled in-memory catalog
    /// (`romacode_providers::catalog::bundled()`). The network-fetched
    /// `cached()` variant must never run here: it blocks the UI thread on
    /// expiry and uses a no-TLS client against HTTPS, so it always failed.
    pub fn begin_models_fallback(&mut self, reason: &str) {
        // 1. Bundled catalog only — in-memory, no network path.
        let mut suggestions: Vec<String> =
            romacode_providers::catalog::Catalog::bundled()
                .models(self.provider_id.trim())
                .iter()
                .map(|m| m.model_id.as_str().to_string())
                .collect();
        // 2. Fall back to the legacy static catalog if the dynamic one had
        // nothing for this provider (keeps every previously-known provider
        // working even when the network catalog is unreachable).
        if suggestions.is_empty() {
            suggestions = catalog::find(self.provider_id.trim())
                .map(|k| k.models.iter().map(|m| m.to_string()).collect())
                .unwrap_or_default();
        }
        if suggestions.is_empty() {
            self.loading = false;
            self.error = Some(reason.to_string());
            return;
        }
        self.begin_models(suggestions);
        self.error = Some(format!(
            "{reason} — showing the built-in catalog for {}; pick one or retry the fetch.",
            self.provider_name
        ));
    }
    pub fn begin_tools(&mut self) {
        self.screen = Screen::Tools;
        self.sel = 0;
        self.tools_sel = 0;
        self.tools_query.clear();
        self.tools_selected = None;
        self.tools_result = None;
        self.error = None;
    }
    pub fn begin_tools_input(&mut self, name: String) {
        self.screen = Screen::ToolsInput;
        self.input.clear();
        self.cursor = 0;
        self.error = None;
        self.tools_selected = Some(name);
        self.tools_result = None;
        // A sane default JSON object so Enter works immediately for no-param tools.
        self.input = "{}".to_string();
        self.cursor = 1;
    }

    // ── input editing (cursor-based) ────────────────────────────────────────

    fn clamp_cursor(&mut self) {
        self.cursor = self.cursor.min(self.input.chars().count());
    }
    pub fn input_char(&mut self, c: char) {
        let idx = self.char_index(self.cursor);
        self.input.insert(idx, c);
        self.cursor += 1;
    }
    pub fn input_backspace(&mut self) {
        self.clamp_cursor();
        if self.cursor > 0 {
            let idx = self.char_index(self.cursor - 1);
            self.input.remove(idx);
            self.cursor -= 1;
        }
    }
    pub fn input_delete(&mut self) {
        self.clamp_cursor();
        if self.cursor < self.input.chars().count() {
            let idx = self.char_index(self.cursor);
            self.input.remove(idx);
        }
    }
    pub fn input_left(&mut self) {
        self.clamp_cursor();
        self.cursor = self.cursor.saturating_sub(1);
    }
    pub fn input_right(&mut self) {
        self.clamp_cursor();
        self.cursor = (self.cursor + 1).min(self.input.chars().count());
    }
    pub fn input_home(&mut self) {
        self.cursor = 0;
    }
    pub fn input_end(&mut self) {
        self.cursor = self.input.chars().count();
    }
    /// Paste into the input: control characters (incl. CR/LF from terminals)
    /// are stripped so a pasted key or URL can never break the field.
    pub fn input_paste(&mut self, s: &str) {
        for c in s.chars().filter(|c| !c.is_control()) {
            self.input_char(c);
        }
    }
    /// Paste into the Root search query (same control-char filter).
    pub fn query_paste(&mut self, s: &str) {
        for c in s.chars().filter(|c| !c.is_control()) {
            self.query.push(c);
        }
        self.sel = 0;
    }
    /// Paste into the Tools search query.
    pub fn tools_query_paste(&mut self, s: &str) {
        for c in s.chars().filter(|c| !c.is_control()) {
            self.tools_query.push(c);
        }
        self.tools_sel = 0;
    }
    /// Paste into the Providers search query.
    pub fn provider_query_paste(&mut self, s: &str) {
        for c in s.chars().filter(|c| !c.is_control()) {
            self.provider_query.push(c);
        }
        self.sel = 0;
    }
    /// Paste into the Models search query.
    pub fn model_query_paste(&mut self, s: &str) {
        for c in s.chars().filter(|c| !c.is_control()) {
            self.model_query.push(c);
        }
        self.sel = 0;
    }
    /// Visible form of the input: secrets masked, cursor rendered at position.
    pub fn input_display(&self, mask: bool) -> String {
        let cur = self.cursor.min(self.input.chars().count());
        let chars: Vec<char> = self.input.chars().collect();
        let (before, after) = (
            chars[..cur].iter().collect::<String>(),
            chars[cur..].iter().collect::<String>(),
        );
        if mask {
            format!(
                "{}▌{}",
                "•".repeat(before.chars().count()),
                "•".repeat(after.chars().count())
            )
        } else {
            format!("{before}▌{after}")
        }
    }
    fn char_index(&self, char_pos: usize) -> usize {
        self.input
            .char_indices()
            .nth(char_pos)
            .map(|(i, _)| i)
            .unwrap_or(self.input.len())
    }
}

pub type FetchResult = Result<Vec<String>, String>;

#[cfg(test)]
mod tests {
    use super::*;

    /// A failed live fetch must not leave an empty model picker: a KNOWN
    /// provider falls back to its catalog (now the dynamic OpenCode-structured
    /// catalog, with the legacy static catalog as a last-resort fallback), with
    /// the fetch error surfaced alongside. A custom provider has no suggestions
    /// and shows the error alone.
    #[test]
    fn fetch_failure_falls_back_to_catalog_suggestions_for_known_providers() {
        let known = catalog::find("groq").expect("groq is in the shared catalog");
        let mut st = Settings::root();
        st.begin_known(known);
        st.begin_models_fallback("provider returned an empty model list");
        assert_eq!(st.screen, Screen::Models);
        // The new dynamic catalog is consulted first; if it has groq models,
        // those are shown. Either way the picker must not be empty for a known
        // provider, and the error must be surfaced.
        assert!(!st.models.is_empty(), "picker must not be empty for a known provider");
        let err = st.error.as_deref().unwrap_or("");
        assert!(err.contains("built-in catalog"), "error should mention the catalog: {err}");
        assert!(err.contains("provider returned an empty model list"), "{err}");
        assert!(!st.loading);

        // a custom provider has no suggestions: the error alone is shown
        let mut st = Settings::root();
        st.begin_custom();
        st.provider_id = "custom-1".into();
        st.begin_models_fallback("connection refused");
        assert!(st.models.is_empty());
        assert_eq!(st.error.as_deref(), Some("connection refused"));
        assert!(!st.loading);
    }

    #[test]
    fn cursor_editing_edits_the_middle_of_the_field() {
        let mut st = Settings::root();
        st.begin_custom();
        for c in "abc".chars() {
            st.input_char(c);
        }
        assert_eq!(st.cursor, 3);
        st.input_left();
        st.input_left();
        assert_eq!(st.cursor, 1);
        st.input_delete(); // removes 'b' at the cursor
        assert_eq!(st.input, "ac");
        st.input_char('B');
        assert_eq!(st.input, "aBc");
        assert_eq!(st.cursor, 2);
        st.input_home();
        assert_eq!(st.cursor, 0);
        st.input_end();
        st.input_backspace();
        assert_eq!(st.input, "aB");

        // fix a base URL: https://wrong.io/v1 -> https://right.io/v1
        let mut st = Settings::root();
        st.begin_custom();
        for c in "https://wrong.io/v1".chars() {
            st.input_char(c);
        }
        st.input_home();
        for _ in 0..8 {
            st.input_right();
        }
        assert_eq!(st.cursor, 8); // just before 'w'
        for _ in 0..5 {
            st.input_delete(); // drop "wrong"
        }
        assert_eq!(st.input, "https://.io/v1");
        for c in "right".chars() {
            st.input_char(c);
        }
        assert_eq!(st.input, "https://right.io/v1");
    }

    #[test]
    fn paste_strips_control_characters() {
        let mut st = Settings::root();
        st.begin_known(&Known {
            id: "openai".into(),
            name: "OpenAI".into(),
            kind: ApiKind::OpenAi,
            base_url: "https://api.openai.com/v1".into(),
            key_env: "OPENAI_API_KEY".into(),
            needs_key: true,
            default_model: "gpt-4o".into(),
            models: vec![],
        });
        // terminals often terminate a bracketed paste with CRLF
        st.input_paste("sk-abc123\r\n");
        assert_eq!(st.input, "sk-abc123");
        st.query_paste("prov\tider");
        assert_eq!(st.query, "provider");
    }

    #[test]
    fn masked_display_hides_secrets_but_keeps_cursor() {
        let mut st = Settings::root();
        st.begin_custom();
        for c in "abcd".chars() {
            st.input_char(c);
        }
        st.input_left();
        let shown = st.input_display(true);
        assert!(shown.starts_with("•••▌•"));
        let plain = st.input_display(false);
        assert!(plain.starts_with("abc▌d"));
    }

    /// Regression: "Add Custom Provider" must have exactly one entry point.
    #[test]
    fn custom_provider_has_exactly_one_entry_point() {
        let customs = ENTRIES.iter().filter(|e| e.action == RootAction::AddCustomProvider).count();
        let knowns = ENTRIES.iter().filter(|e| e.action == RootAction::AddKnownProvider).count();
        assert_eq!(customs, 1);
        assert_eq!(knowns, 1);
        assert_eq!(ENTRIES.iter().filter(|e| e.label.contains("Custom")).count(), 1);
    }

    /// The Tools entry exists exactly once and routes to OpenTools.
    #[test]
    fn tools_has_exactly_one_entry() {
        assert_eq!(
            ENTRIES.iter().filter(|e| e.action == RootAction::OpenTools).count(),
            1
        );
    }

    /// The three encodings terminals use for Backspace all edit the field.
    #[test]
    fn backspace_variants_edit_the_input() {
        let mut st = Settings::root();
        st.begin_custom();
        for c in "abc".chars() {
            st.input_char(c);
        }
        st.input.pop();
        assert_eq!(st.input, "ab");
        st.input_char('c');
        assert_eq!(st.input, "abc");
        st.input_backspace();
        assert_eq!(st.input, "ab");
        st.input_backspace();
        st.input_backspace();
        assert_eq!(st.input, "");
        st.input_backspace(); // no underflow on an empty field
        assert_eq!(st.input, "");
        assert_eq!(st.cursor, 0);
    }

    #[test]
    fn hit_row_contains_is_inclusive_origin_exclusive_far_edge() {
        let h = HitRow { x: 10, y: 5, w: 20, h: 1, kind: RowKind::Tool(0) };
        assert!(h.contains(10, 5));
        assert!(h.contains(29, 5));
        assert!(!h.contains(30, 5)); // far edge exclusive
        assert!(!h.contains(10, 6)); // height 1 → next row excluded
    }

    #[test]
    fn tools_filter_matches_name_substring() {
        let st = Settings::root();
        let all = vec!["math.add".into(), "math.sqrt".into(), "text.upper".into()];
        let idx = st.tools_filter(&all);
        assert_eq!(idx.len(), 3);
        let mut st = st;
        st.tools_query = "math".into();
        let idx = st.tools_filter(&all);
        assert_eq!(idx, vec![0, 1]);
    }

    /// Regression guard: the Settings category was fully removed from the Ctrl+P
    /// panel. No entry's category is "Settings" — the OpenCode-style settings
    /// dialog, the server-prefs editor, and the engine switcher are gone.
    #[test]
    fn no_settings_entries_remain_in_the_root_list() {
        assert!(ENTRIES.iter().all(|e| e.category != "Settings"));
        // Only the four surviving categories are present, in this order.
        let cats: Vec<&str> = {
            let mut c: Vec<&str> = Vec::new();
            for e in ENTRIES {
                if c.last() != Some(&e.category) {
                    c.push(e.category);
                }
            }
            c
        };
        assert_eq!(cats, vec!["Commands", "Providers", "Tools", "Config"]);
    }
    #[test]
    fn provider_search_filters_by_name_or_id_and_maps_back() {
        let mut st = Settings::root();
        st.screen = Screen::Providers;
        st.provider_query = "openrouter".into();
        let items = st.provider_items();
        assert!(!items.is_empty());
        assert!(items.iter().all(|(_, k)| {
            k.id.to_lowercase().contains("openrouter")
                || k.name.to_lowercase().contains("openrouter")
        }));
        st.sel = 0;
        let picked = st.selected_provider().expect("filtered pick");
        assert_eq!(picked.id, "openrouter");

        st.provider_query = "no-such-provider-xyz".into();
        assert!(st.provider_items().is_empty());
        assert!(st.selected_provider().is_none());
        st.clamp_sel(&[]);
        assert_eq!(st.sel, 0, "cursor must never point past an empty list");
    }

    #[test]
    fn model_search_filters_by_model_and_provider() {
        let mut st = Settings::root();
        st.screen = Screen::SelectModel;
        st.models = vec!["alpha".into(), "beta".into(), "gamma".into()];
        st.model_providers = vec!["openai".into(), "anthropic".into(), "openai".into()];

        st.model_query = "openai".into();
        assert_eq!(st.model_items(), vec![0, 2]);
        st.sel = 1;
        assert_eq!(st.selected_model_index(), Some(2));

        st.model_query = "beta".into();
        assert_eq!(st.model_items(), vec![1]);
        st.clamp_sel(&[]);
        assert_eq!(st.sel, 0);
        assert_eq!(st.selected_model_index(), Some(1));
    }

    #[test]
    fn visible_len_and_clamp_never_let_the_cursor_wrap() {
        let mut st = Settings::root();
        st.screen = Screen::Providers;
        st.sel = usize::MAX;
        st.clamp_sel(&[]);
        let last = st.provider_total() - 1; // includes the trailing custom row
        assert_eq!(st.sel, last, "stale index clamps to the last row, not past it");
        st.sel = 0;
        st.clamp_sel(&[]);
        assert_eq!(st.sel, 0);
    }

}
