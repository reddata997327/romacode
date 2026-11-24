//! Global framework settings — the equivalent of `django.conf.settings`.
//!
//! Settings are built programmatically through [`Settings::builder`] and can
//! be overridden from a `settings.toml` file. Every knob has a default that
//! mirrors Django's own defaults.

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::OnceLock;

/// How passwords are hashed. Mirrors Django's default PBKDF2-SHA256.
#[derive(Debug, Clone)]
pub struct PasswordHasherConfig {
    /// Algorithm tag written into the hash string (`pbkdf2_sha256`), so
    /// hashes are byte-compatible with Django's `make_password` output.
    pub algorithm: &'static str,
    pub iterations: u32,
}

impl Default for PasswordHasherConfig {
    fn default() -> Self {
        Self { algorithm: "pbkdf2_sha256", iterations: 600_000 }
    }
}

/// The full settings object.
#[derive(Debug, Clone)]
pub struct Settings {
    /// DEV mode: pretty 404/500 pages, template caching off.
    pub debug: bool,
    /// Secret key used for signing (kept for parity; sessions are server-side).
    pub secret_key: String,
    /// Allowed hosts — `["*"]` skips the check (dev default).
    pub allowed_hosts: Vec<String>,
    /// Root URL prefix for the admin site.
    pub admin_url: String,
    /// Serve the admin at all?
    pub admin_enabled: bool,
    /// Site title shown in admin.
    pub site_title: String,
    pub site_header: String,

    /// Template search dirs, first match wins (like DIRS + APP_DIRS).
    pub template_dirs: Vec<PathBuf>,
    /// Static files search dirs.
    pub staticfiles_dirs: Vec<PathBuf>,
    /// URL prefix under which static files are served.
    pub static_url: String,

    /// Database backend alias: `memory` or `json:<path>`.
    pub database: String,
    /// Where migration files live.
    pub migrations_dir: PathBuf,

    /// i18n
    pub language_code: String,
    pub languages: Vec<String>,
    pub locale_dirs: Vec<PathBuf>,

    /// CSRF protection on/off.
    pub csrf_enabled: bool,
    /// Cookie names.
    pub session_cookie_name: String,
    pub csrf_cookie_name: String,
    pub language_cookie_name: String,

    /// Middleware toggles for the built-ins.
    pub use_gzip: bool,
    pub use_security: bool,
    pub use_clickjacking: bool,
    pub x_frame_options: String,

    /// Gzip only compresses responses above this many bytes.
    pub gzip_min_length: usize,
    /// Sessions expire after this many seconds (default 2 weeks, like Django).
    pub session_cookie_age: i64,

    /// Cache: `locmem` (the only built-in backend).
    pub cache_backend: String,

    pub password_hasher: PasswordHasherConfig,

    /// contrib.ai configuration (OpenAI-compatible endpoint).
    pub ai_api_url: String,
    pub ai_api_key: String,
    pub ai_model: String,

    /// Extra user-defined settings bag.
    pub extra: HashMap<String, String>,
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            debug: true,
            secret_key: "rustango-insecure-dev-key".into(),
            allowed_hosts: vec!["*".into()],
            admin_url: "admin/".into(),
            admin_enabled: true,
            site_title: "Rustango site admin".into(),
            site_header: "Rustango administration".into(),
            template_dirs: vec![],
            staticfiles_dirs: vec![],
            static_url: "static/".into(),
            database: "memory".into(),
            migrations_dir: PathBuf::from("migrations"),
            language_code: "en-us".into(),
            languages: vec!["en-us".into()],
            locale_dirs: vec![],
            csrf_enabled: true,
            session_cookie_name: "sessionid".into(),
            csrf_cookie_name: "csrftoken".into(),
            language_cookie_name: "rustango_language".into(),
            use_gzip: true,
            use_security: true,
            use_clickjacking: true,
            x_frame_options: "DENY".into(),
            gzip_min_length: 200,
            session_cookie_age: 14 * 24 * 3600,
            cache_backend: "locmem".into(),
            password_hasher: PasswordHasherConfig::default(),
            ai_api_url: "https://api.openai.com/v1".into(),
            ai_api_key: String::new(),
            ai_model: "gpt-4o-mini".into(),
            extra: HashMap::new(),
        }
    }
}

impl Settings {
    pub fn builder() -> SettingsBuilder {
        SettingsBuilder { s: Settings::default() }
    }

    /// Overlay values from a TOML file (missing keys keep current values).
    pub fn load_toml(&mut self, text: &str) -> Result<(), String> {
        let val: toml::Value = toml::from_str(text).map_err(|e| e.to_string())?;
        let map = val.as_table().cloned().unwrap_or_default();
        let g = |k: &str| map.get(k);
        if let Some(toml::Value::Boolean(b)) = g("debug") { self.debug = *b; }
        if let Some(toml::Value::String(s)) = g("secret_key") { self.secret_key = s.clone(); }
        if let Some(toml::Value::String(s)) = g("database") { self.database = s.clone(); }
        if let Some(toml::Value::String(s)) = g("language_code") { self.language_code = s.clone(); }
        if let Some(toml::Value::String(s)) = g("static_url") { self.static_url = s.clone(); }
        if let Some(toml::Value::String(s)) = g("admin_url") { self.admin_url = s.clone(); }
        if let Some(toml::Value::Boolean(b)) = g("admin_enabled") { self.admin_enabled = *b; }
        if let Some(toml::Value::Boolean(b)) = g("csrf_enabled") { self.csrf_enabled = *b; }
        if let Some(toml::Value::String(s)) = g("ai_api_url") { self.ai_api_url = s.clone(); }
        if let Some(toml::Value::String(s)) = g("ai_api_key") { self.ai_api_key = s.clone(); }
        if let Some(toml::Value::String(s)) = g("ai_model") { self.ai_model = s.clone(); }
        if let Some(toml::Value::Array(a)) = g("template_dirs") {
            self.template_dirs = a.iter().filter_map(|v| v.as_str().map(PathBuf::from)).collect();
        }
        if let Some(toml::Value::Array(a)) = g("staticfiles_dirs") {
            self.staticfiles_dirs = a.iter().filter_map(|v| v.as_str().map(PathBuf::from)).collect();
        }
        if let Some(toml::Value::Array(a)) = g("locale_dirs") {
            self.locale_dirs = a.iter().filter_map(|v| v.as_str().map(PathBuf::from)).collect();
        }
        if let Some(toml::Value::Table(t)) = g("extra") {
            for (k, v) in t {
                if let Some(s) = v.as_str() {
                    self.extra.insert(k.clone(), s.to_string());
                }
            }
        }
        Ok(())
    }
}

/// Fluent builder for [`Settings`].
#[derive(Default)]
pub struct SettingsBuilder {
    s: Settings,
}

impl SettingsBuilder {
    pub fn build(self) -> Settings { self.s }

    pub fn debug(mut self, v: bool) -> Self { self.s.debug = v; self }
    pub fn secret_key(mut self, v: impl Into<String>) -> Self { self.s.secret_key = v.into(); self }
    pub fn allowed_hosts(mut self, v: Vec<String>) -> Self { self.s.allowed_hosts = v; self }
    pub fn admin_url(mut self, v: impl Into<String>) -> Self { self.s.admin_url = v.into(); self }
    pub fn admin_enabled(mut self, v: bool) -> Self { self.s.admin_enabled = v; self }
    pub fn template_dirs(mut self, v: Vec<PathBuf>) -> Self { self.s.template_dirs = v; self }
    pub fn staticfiles_dirs(mut self, v: Vec<PathBuf>) -> Self { self.s.staticfiles_dirs = v; self }
    pub fn static_url(mut self, v: impl Into<String>) -> Self { self.s.static_url = v.into(); self }
    pub fn database(mut self, v: impl Into<String>) -> Self { self.s.database = v.into(); self }
    pub fn migrations_dir(mut self, v: PathBuf) -> Self { self.s.migrations_dir = v; self }
    pub fn language_code(mut self, v: impl Into<String>) -> Self { self.s.language_code = v.into(); self }
    pub fn languages(mut self, v: Vec<String>) -> Self { self.s.languages = v; self }
    pub fn locale_dirs(mut self, v: Vec<PathBuf>) -> Self { self.s.locale_dirs = v; self }
    pub fn csrf_enabled(mut self, v: bool) -> Self { self.s.csrf_enabled = v; self }
    pub fn session_cookie_name(mut self, v: impl Into<String>) -> Self { self.s.session_cookie_name = v.into(); self }
    pub fn csrf_cookie_name(mut self, v: impl Into<String>) -> Self { self.s.csrf_cookie_name = v.into(); self }
    pub fn use_gzip(mut self, v: bool) -> Self { self.s.use_gzip = v; self }
    pub fn use_security(mut self, v: bool) -> Self { self.s.use_security = v; self }
    pub fn use_clickjacking(mut self, v: bool) -> Self { self.s.use_clickjacking = v; self }
    pub fn session_cookie_age(mut self, v: i64) -> Self { self.s.session_cookie_age = v; self }
    pub fn password_iterations(mut self, v: u32) -> Self { self.s.password_hasher.iterations = v; self }
    pub fn ai(mut self, url: impl Into<String>, key: impl Into<String>, model: impl Into<String>) -> Self {
        self.s.ai_api_url = url.into();
        self.s.ai_api_key = key.into();
        self.s.ai_model = model.into();
        self
    }
    pub fn extra(mut self, k: impl Into<String>, v: impl Into<String>) -> Self {
        self.s.extra.insert(k.into(), v.into());
        self
    }
}

/// Process-global settings, like `django.conf.settings`.
/// Set once at project startup; falls back to defaults.
static GLOBAL_SETTINGS: OnceLock<Settings> = OnceLock::new();

pub fn set_global(s: Settings) -> bool {
    GLOBAL_SETTINGS.set(s).is_ok()
}

pub fn global() -> &'static Settings {
    GLOBAL_SETTINGS.get_or_init(Settings::default)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn defaults_match_django_spirit() {
        let s = Settings::default();
        assert!(s.debug);
        assert_eq!(s.session_cookie_name, "sessionid");
        assert_eq!(s.csrf_cookie_name, "csrftoken");
        assert_eq!(s.password_hasher.algorithm, "pbkdf2_sha256");
        assert_eq!(s.x_frame_options, "DENY");
        assert_eq!(s.session_cookie_age, 14 * 24 * 3600);
    }

    #[test]
    fn toml_overlay() {
        let mut s = Settings::default();
        s.load_toml(
            r#"
            debug = false
            database = "json:db.json"
            language_code = "de"
            [extra]
            flavor = "rusty"
            "#,
        )
        .unwrap();
        assert!(!s.debug);
        assert_eq!(s.database, "json:db.json");
        assert_eq!(s.language_code, "de");
        assert_eq!(s.extra["flavor"], "rusty");
    }

    #[test]
    fn builder_chain() {
        let s = Settings::builder().debug(false).secret_key("abc").admin_enabled(false).build();
        assert!(!s.debug);
        assert_eq!(s.secret_key, "abc");
        assert!(!s.admin_enabled);
    }
}

