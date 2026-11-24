//! User-owned persona instructions, loaded into the system prompt.
//!
//! The persona file is the user's standing-instructions channel: a plain
//! markdown file (default `~/.config/romacode/persona.md`) whose content is
//! appended to the agent's system prompt on every turn. It hot-reloads —
//! edit the file and the next message already speaks with the new voice,
//! no restart, no config reload.
//!
//! Design rules:
//! - The file is entirely user-owned. RomaCode only ever WRITES it once,
//!   when it does not exist (the starter template). Edits are preserved.
//! - Loading is best-effort: a missing, empty or unreadable file silently
//!   contributes nothing, so a broken persona can never break the loop.
//! - Reads are capped (`PERSONA_MAX_BYTES`); oversized files are truncated
//!   with an explicit note instead of silently eating the context window.
//! - `agent.persona_file` in config.json switches the path; the spellings
//!   `""`, `off`, `none`, `disabled`, `false`, `0` switch the feature off.

use crate::config::Config;
use std::path::{Path, PathBuf};

/// Persona file read cap. Anything larger is truncated with a note.
pub const PERSONA_MAX_BYTES: usize = 64 * 1024;

/// File name used when [`Config`]'s `agent.persona_file` is unset.
pub const PERSONA_FILE_NAME: &str = "persona.md";

/// Config spellings that switch persona loading off.
const OFF_SPELLINGS: [&str; 6] = ["", "off", "none", "disabled", "false", "0"];

/// The file written on first run, when the default persona file is missing.
/// Deliberately short: tone and priorities only. The base prompt keeps
/// owning tool behavior; anything the user writes here speaks as standing
/// instructions to their own model.
const STARTER_TEMPLATE: &str = r#"<!--
RomaCode persona file — standing instructions appended to the system prompt.

- Hot-reloaded: edit it and the NEXT message already uses the new text.
- This default file lives at ~/.config/romacode/persona.md.
- Point it at another file or switch it off with agent.persona_file in
  ~/.config/romacode/config.json ("off" disables; a path switches files).
- Keep it short: a few sharp lines beat a wall of text. The base prompt
  (tools, engine behavior) stays in effect; this file tunes style and
  priorities only.
-->

# Persona

Answer directly and to the point: lead with the answer or the diff, explain
only what earns its place. No filler, no hedging, no restating the request.

Terminal-first output: fenced code blocks with the file path or language in
the info string, one shell command per line, real paths instead of
placeholders.

When a request is ambiguous, pick the most reasonable interpretation, state
the assumption in one line, and keep moving. Stop and ask only when a wrong
guess would waste real work.

Prefer concrete artifacts over descriptions of artifacts: write the file,
run the command, show the diff.
"#;

/// Default persona path: `<config_dir>/romacode/persona.md`
/// (e.g. `~/.config/romacode/persona.md` on Linux).
pub fn default_path() -> Option<PathBuf> {
    dirs::config_dir().map(|d| d.join("romacode").join(PERSONA_FILE_NAME))
}

/// Expand a leading `~/` (or bare `~`) against `home`. `~user/...` is left
/// untouched — resolving other users' homes is not RomaCode's business.
fn expand_tilde(home: &Path, p: &str) -> PathBuf {
    if p == "~" {
        return home.to_path_buf();
    }
    if let Some(rest) = p.strip_prefix("~/") {
        return home.join(rest);
    }
    PathBuf::from(p)
}

/// Resolve the effective persona path from the config.
///
/// - `None` (key absent / null): the default path — feature on.
/// - an `OFF_SPELLINGS` value: `None` — feature off.
/// - anything else: that path, `~/` expanded, trimmed.
pub fn resolve(cfg: &Config) -> Option<PathBuf> {
    match cfg.agent.persona_file.as_deref().map(str::trim) {
        None => default_path(),
        Some(s) if OFF_SPELLINGS.contains(&s) => None,
        Some(s) => match dirs::home_dir() {
            Some(home) => Some(expand_tilde(&home, s)),
            None => Some(PathBuf::from(s)),
        },
    }
}

/// Read the persona file and return its content, trimmed, ready to append
/// to the system prompt. `None` = no persona this turn (missing file,
/// disabled, empty content, unreadable). Oversized files are truncated at
/// [`PERSONA_MAX_BYTES`] with an explicit note — never a silent cut.
pub fn load(cfg: &Config) -> Option<String> {
    let path = resolve(cfg)?;
    let meta = std::fs::metadata(&path).ok()?;
    if !meta.is_file() {
        return None;
    }
    let (text, truncated) = if meta.len() as usize > PERSONA_MAX_BYTES {
        let raw = std::fs::read(&path).ok()?;
        // The file can shrink between metadata and read; min() keeps the
        // slice in bounds even in that race.
        let end = raw.len().min(PERSONA_MAX_BYTES);
        (String::from_utf8_lossy(&raw[..end]).into_owned(), true)
    } else {
        let raw = std::fs::read(&path).ok()?;
        (String::from_utf8_lossy(&raw).into_owned(), false)
    };
    // Strip a UTF-8 BOM if the file was saved by a Windows editor.
    let text = text.strip_prefix('\u{feff}').unwrap_or(&text);
    let text = text.trim();
    if text.is_empty() {
        return None;
    }
    if truncated {
        Some(format!(
            "{text}\n\n[persona file truncated at {PERSONA_MAX_BYTES} bytes]"
        ))
    } else {
        Some(text.to_string())
    }
}

/// Write the starter template to `path` if nothing is there yet. Returns
/// whether it created the file. Never overwrites: if the user already has
/// a persona there (even an empty one), it stays untouched.
fn write_starter_if_missing(path: &Path) -> std::io::Result<bool> {
    if path.exists() {
        return Ok(false);
    }
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    std::fs::write(path, STARTER_TEMPLATE)?;
    Ok(true)
}

/// First-run convenience, called from the `serve` startup path: if the
/// persona feature points at the DEFAULT path and that file is missing,
/// drop the starter template there so the mechanism is usable immediately.
/// Custom paths are respected, never created or overwritten.
pub fn ensure_starter(cfg: &Config) {
    let on_default = cfg.agent.persona_file.is_none();
    if !on_default {
        return;
    }
    if let Some(path) = resolve(cfg) {
        match write_starter_if_missing(&path) {
            Ok(true) => eprintln!("persona file created: {} (edit it; hot-reloads every turn)", path.display()),
            Ok(false) => {}
            Err(e) => eprintln!("warning: could not create persona file {}: {e}", path.display()),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    fn cfg_with_persona(value: Option<&str>) -> Config {
        let mut cfg = Config::default();
        cfg.agent.persona_file = value.map(str::to_string);
        cfg
    }

    #[test]
    fn resolve_default_uses_config_dir() {
        let p = resolve(&cfg_with_persona(None)).expect("default persona path");
        assert!(p.ends_with("romacode/persona.md"), "got {}", p.display());
    }

    #[test]
    fn resolve_off_spellings_disable() {
        for s in OFF_SPELLINGS {
            assert!(resolve(&cfg_with_persona(Some(s))).is_none(), "'{s}' must disable");
        }
    }

    #[test]
    fn resolve_custom_path_and_whitespace() {
        let cfg = cfg_with_persona(Some(" /tmp/pers.md "));
        assert_eq!(resolve(&cfg).unwrap(), PathBuf::from("/tmp/pers.md"));
    }

    #[test]
    fn expand_tilde_forms() {
        let home = Path::new("/home/u");
        assert_eq!(expand_tilde(home, "~/a.md"), PathBuf::from("/home/u/a.md"));
        assert_eq!(expand_tilde(home, "~"), PathBuf::from("/home/u"));
        assert_eq!(expand_tilde(home, "/abs/x"), PathBuf::from("/abs/x"));
        // ~user stays literal: no passwd lookups.
        assert_eq!(expand_tilde(home, "~other/x"), PathBuf::from("~other/x"));
    }

    #[test]
    fn load_appends_trimmed_content() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("persona.md");
        std::fs::write(&p, "\n\nfirst line\nsecond line\n\n").unwrap();
        let cfg = cfg_with_persona(Some(p.to_str().unwrap()));
        assert_eq!(load(&cfg).unwrap(), "first line\nsecond line");
    }

    #[test]
    fn load_missing_or_empty_is_none() {
        let dir = tempfile::tempdir().unwrap();
        let cfg = cfg_with_persona(Some(dir.path().join("nope.md").to_str().unwrap()));
        assert!(load(&cfg).is_none());

        let empty = dir.path().join("empty.md");
        std::fs::write(&empty, "   \n\t\n").unwrap();
        let cfg = cfg_with_persona(Some(empty.to_str().unwrap()));
        assert!(load(&cfg).is_none());
    }

    #[test]
    fn load_truncates_oversized_files_with_a_note() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("big.md");
        let mut f = std::fs::File::create(&p).unwrap();
        let chunk = "x".repeat(4096);
        for _ in 0..(PERSONA_MAX_BYTES / 4096 + 8) {
            f.write_all(chunk.as_bytes()).unwrap();
        }
        drop(f);
        let cfg = cfg_with_persona(Some(p.to_str().unwrap()));
        let loaded = load(&cfg).unwrap();
        assert!(loaded.contains("[persona file truncated at"), "note missing");
        // Truncated body + note, never the whole 100+ KiB file.
        assert!(loaded.len() < PERSONA_MAX_BYTES + 128);
    }

    #[test]
    fn load_hot_reloads_between_turns() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("persona.md");
        std::fs::write(&p, "v1 persona").unwrap();
        let cfg = cfg_with_persona(Some(p.to_str().unwrap()));
        assert_eq!(load(&cfg).unwrap(), "v1 persona");
        std::fs::write(&p, "v2 persona, sharper").unwrap();
        assert_eq!(load(&cfg).unwrap(), "v2 persona, sharper");
    }

    #[test]
    fn starter_is_created_once_and_never_clobbers() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("romacode").join("persona.md");
        assert!(write_starter_if_missing(&p).unwrap(), "first run creates");
        assert_eq!(std::fs::read_to_string(&p).unwrap(), STARTER_TEMPLATE);
        assert!(!write_starter_if_missing(&p).unwrap(), "second run is a no-op");
        // User edits survive: the file is never rewritten.
        std::fs::write(&p, "my own persona").unwrap();
        assert!(!write_starter_if_missing(&p).unwrap());
        assert_eq!(std::fs::read_to_string(&p).unwrap(), "my own persona");
    }

    #[test]
    fn starter_uses_bom_free_utf8() {
        assert!(!STARTER_TEMPLATE.starts_with('\u{feff}'));
        assert!(STARTER_TEMPLATE.contains("# Persona"));
    }

    #[test]
    fn system_prompt_picks_up_the_persona_and_its_off_switch() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("persona.md");
        std::fs::write(&p, "speak tersely, ship diffs").unwrap();

        let mut cfg = cfg_with_persona(Some(p.to_str().unwrap()));
        let with = crate::agent::system_prompt(&cfg);
        assert!(with.contains("speak tersely, ship diffs"), "persona missing from prompt");

        cfg.agent.persona_file = Some("off".into());
        let without = crate::agent::system_prompt(&cfg);
        assert!(!without.contains("speak tersely"), "off must remove the persona");
    }
}
