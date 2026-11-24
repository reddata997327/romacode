//! Internationalization (lite) — `django.utils.translation`.
//!
//! Translations live in JSON catalogs (`<lang>.json`, mapping msgid →
//! translated string) inside `LOCALE_PATHS` dirs. Activate a language with
//! the `rustango_language` cookie or the settings default, then use
//! `{% trans "key" %}` or the `t()` helper.

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::RwLock;

static CATALOGS: RwLock<Option<HashMap<String, HashMap<String, String>>>> = RwLock::new(None);

/// Load catalogs from locale dirs: every `<lang>.json` file.
pub fn load_locale_dirs(dirs: &[PathBuf]) -> usize {
    let mut catalogs: HashMap<String, HashMap<String, String>> = HashMap::new();
    for dir in dirs {
        let entries = match std::fs::read_dir(dir) {
            Ok(e) => e,
            Err(_) => continue,
        };
        for entry in entries.filter_map(|e| e.ok()) {
            let path = entry.path();
            if path.extension().and_then(|e| e.to_str()) == Some("json") {
                let lang = path
                    .file_stem()
                    .and_then(|s| s.to_str())
                    .unwrap_or_default()
                    .to_string();
                if let Ok(text) = std::fs::read_to_string(&path) {
                    if let Ok(map) = serde_json::from_str::<HashMap<String, String>>(&text) {
                        let entry = catalogs.entry(lang).or_default();
                        for (k, v) in map {
                            entry.insert(k, v);
                        }
                    }
                }
            }
        }
    }
    let n = catalogs.len();
    *CATALOGS.write().unwrap() = Some(catalogs);
    n
}

/// Install catalogs directly (tests / programmatic setup).
pub fn set_catalogs(catalogs: HashMap<String, HashMap<String, String>>) {
    *CATALOGS.write().unwrap() = Some(catalogs);
}

fn catalogs() -> HashMap<String, HashMap<String, String>> {
    CATALOGS.read().unwrap().clone().unwrap_or_default()
}

/// Translate `msgid` in `lang`; falls back through language prefixes
/// (`de-de` → `de`) then to the msgid itself (gettext behavior).
pub fn translate(lang: &str, msgid: &str) -> String {
    if msgid.is_empty() {
        return String::new();
    }
    let cats = catalogs();
    if let Some(cat) = cats.get(lang) {
        if let Some(t) = cat.get(msgid) {
            return t.clone();
        }
    }
    if let Some((base, _)) = lang.split_once('-') {
        if let Some(cat) = cats.get(base) {
            if let Some(t) = cat.get(msgid) {
                return t.clone();
            }
        }
    }
    msgid.to_string()
}

/// The `t()` helper — translate with optional interpolation:
/// `t("hello {name}", &[("name", "Ada")])`.
pub fn t(lang: &str, msgid: &str, params: &[(&str, &str)]) -> String {
    let mut out = translate(lang, msgid);
    for (k, v) in params {
        out = out.replace(&format!("{{{k}}}"), v);
    }
    out
}

/// Pick the language for a request: cookie > settings default.
pub fn activate(req: &crate::http::HttpRequest) -> String {
    let settings = crate::settings::global();
    if let Some(lang) = req.cookie(&settings.language_cookie_name) {
        if settings.languages.is_empty()
            || settings.languages.iter().any(|l| l.eq_ignore_ascii_case(lang))
        {
            return lang.to_string();
        }
    }
    settings.language_code.clone()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;

    fn demo() {
        let mut cats = HashMap::new();
        let mut de = HashMap::new();
        de.insert("Welcome".to_string(), "Willkommen".to_string());
        de.insert("hello {name}".to_string(), "Hallo {name}".to_string());
        let mut de_de = HashMap::new();
        de_de.insert("Bye".to_string(), "Auf Wiedersehen".to_string());
        cats.insert("de".to_string(), de);
        cats.insert("de-de".to_string(), de_de);
        set_catalogs(cats);
    }

    #[test]
    fn translate_basic() {
        demo();
        assert_eq!(translate("de", "Welcome"), "Willkommen");
        assert_eq!(translate("en", "Welcome"), "Welcome"); // fallback to msgid
        assert_eq!(translate("de", "Unknown key"), "Unknown key");
    }

    #[test]
    fn language_prefix_fallback() {
        demo();
        assert_eq!(translate("de-de", "Bye"), "Auf Wiedersehen");
        // de-at falls back to `de`, which lacks "Bye" -> msgid
        assert_eq!(translate("de-at", "Bye"), "Bye");
        assert_eq!(translate("de-at", "Welcome"), "Willkommen");
        assert_eq!(translate("de-de", "Welcome"), "Willkommen"); // de-de -> de
    }

    #[test]
    fn interpolation() {
        demo();
        assert_eq!(t("de", "hello {name}", &[("name", "Ada")]), "Hallo Ada");
    }

    #[test]
    fn empty_msgid() {
        demo();
        assert_eq!(translate("de", ""), "");
    }
}
