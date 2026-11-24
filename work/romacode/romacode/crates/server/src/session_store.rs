//! Durable session storage.
//!
//! `sessions.autosave` / `restore_on_start` used to be dead config keys: a
//! process restart lost every transcript. This module writes each finished
//! turn to a small JSON file and loads them back at boot. It is deliberately
//! simple — one file per session, atomic rename, no external crate — and it
//! stores what the TUI already receives in `SessionSnapshot`, so a restored
//! session renders exactly like a live one.

use crate::config::Config;
use crate::session::Session;
use romacode_shared::protocol::SessionSnapshot;
use std::path::PathBuf;

/// Where session files live. `sessions.dir` overrides the default
/// (`~/.local/share/romacode/sessions` on Linux).
pub fn storage_dir(cfg: &Config) -> PathBuf {
    let configured = cfg.sessions.dir.trim();
    if !configured.is_empty() {
        let expanded = if let Some(rest) = configured.strip_prefix("~/") {
            dirs::home_dir()
                .map(|h| h.join(rest))
                .unwrap_or_else(|| PathBuf::from(configured))
        } else {
            PathBuf::from(configured)
        };
        return expanded;
    }
    let base = dirs::data_local_dir().unwrap_or_else(|| PathBuf::from("."));
    base.join("romacode").join("sessions")
}

fn session_file(cfg: &Config, id: usize) -> PathBuf {
    storage_dir(cfg).join(format!("{id}.json"))
}

/// Persist one session. Errors are intentionally swallowed by callers — an
/// unwritable disk must never break a running agent turn.
pub fn save(cfg: &Config, session: &Session) -> Result<(), String> {
    if !cfg.sessions.autosave {
        return Ok(());
    }
    let dir = storage_dir(cfg);
    std::fs::create_dir_all(&dir).map_err(|e| format!("creating session store: {e}"))?;
    let path = session_file(cfg, session.id);
    let tmp = path.with_extension("tmp");
    let json = serde_json::to_string_pretty(&session.snapshot())
        .map_err(|e| format!("encoding session {}: {e}", session.id))?;
    std::fs::write(&tmp, json).map_err(|e| format!("writing session {}: {e}", session.id))?;
    std::fs::rename(&tmp, &path).map_err(|e| format!("committing session {}: {e}", session.id))?;
    Ok(())
}

/// Load saved sessions, newest id first, capped at `sessions.max_saved`.
pub fn load_all(cfg: &Config) -> Vec<SessionSnapshot> {
    if !cfg.sessions.restore_on_start {
        return Vec::new();
    }
    let dir = storage_dir(cfg);
    let Ok(entries) = std::fs::read_dir(&dir) else {
        return Vec::new();
    };
    let mut out: Vec<(usize, SessionSnapshot)> = Vec::new();
    for entry in entries.flatten() {
        let path = entry.path();
        if path.extension().and_then(|e| e.to_str()) != Some("json") {
            continue;
        }
        let Ok(text) = std::fs::read_to_string(&path) else {
            continue;
        };
        if let Ok(snap) = serde_json::from_str::<SessionSnapshot>(&text) {
            out.push((snap.id, snap));
        }
    }
    out.sort_by_key(|(id, _)| std::cmp::Reverse(*id));
    let cap = cfg.sessions.max_saved.max(1) as usize;
    out.truncate(cap);
    out.into_iter().map(|(_, s)| s).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn save_and_load_roundtrip() {
        let mut cfg = Config::default();
        cfg.sessions.dir = std::env::temp_dir()
            .join(format!("romacode-store-test-{}", std::process::id()))
            .to_string_lossy()
            .into_owned();
        let _ = std::fs::remove_dir_all(storage_dir(&cfg));

        let mut s = Session::new(7);
        s.push_user("remember this".into());
        s.begin_assistant("m");
        s.finish_turn(1.0);
        save(&cfg, &s).expect("save");

        let loaded = load_all(&cfg);
        assert_eq!(loaded.len(), 1);
        assert_eq!(loaded[0].id, 7);
        assert!(loaded[0].messages.iter().any(|m| format!("{m:?}").contains("remember this")));
        let _ = std::fs::remove_dir_all(storage_dir(&cfg));
    }
}
