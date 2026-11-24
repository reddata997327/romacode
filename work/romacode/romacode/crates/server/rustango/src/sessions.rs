//! Server-side sessions — `django.contrib.sessions` with database backend.
//!
//! The session data lives in the `django_session` table (same name as
//! Django's own default table) as a JSON blob, keyed by a 32-char session
//! key stored in the `sessionid` cookie.

use std::collections::HashMap;
use std::sync::Arc;

use chrono::{DateTime, Duration, Utc};
use serde_json::Value as JsonValue;

use crate::orm::{Backend, Criteria, Op, Value};

pub const SESSION_TABLE: &str = "django_session";
const SESSION_KEY_LEN: usize = 32;

/// A user session, attached to `request.session` by SessionMiddleware.
pub struct Session {
    pub key: String,
    data: HashMap<String, JsonValue>,
    pub modified: bool,
    pub expiry: DateTime<Utc>,
    pub cookie_age: i64,
    backend: Arc<dyn Backend>,
    pub flushed: bool,
    pub new: bool,
    /// Set when this session was persisted during the request — the cookie
    /// must be re-sent (key rotation / creation).
    pub written: bool,
}

impl Session {
    /// Load an existing session by cookie key, or create a fresh one.
    pub fn load(backend: Arc<dyn Backend>, key: Option<&str>, cookie_age: i64) -> Session {
        // Creating the table flushes the whole JSON file — skip it when the
        // table already exists (in-memory check) so a request neither
        // rewrites the DB nor races a concurrent flush.
        if !backend.tables().iter().any(|t| t == SESSION_TABLE) {
            let _ = backend.create_table(SESSION_TABLE, session_columns());
        }
        if let Some(key) = key {
            if key.len() >= 8 && key.len() <= 64 {
                let crit = Criteria::eq("session_key", Value::Text(key.to_string()));
                if let Ok(rows) = backend.select(SESSION_TABLE, &crit, &[], None, None) {
                    if let Some(row) = rows.first() {
                        let data = row
                            .get(1)
                            .and_then(|v| v.as_text())
                            .and_then(|s| serde_json::from_str::<HashMap<String, JsonValue>>(s).ok())
                            .unwrap_or_default();
                        let expiry = row
                            .get(2)
                            .and_then(|v| v.as_datetime())
                            .unwrap_or_else(|| Utc::now() + Duration::seconds(cookie_age));
                        // Expired sessions are treated as absent (Django's
                        // SessionMiddleware drops them at load time).
                        if Utc::now() < expiry {
                            return Session {
                                key: key.to_string(),
                                data,
                                modified: false,
                                expiry,
                                cookie_age,
                                backend,
                                flushed: false,
                                new: false,
                                written: false,
                            };
                        }
                    }
                }
            }
        }
        Self::fresh(backend, cookie_age)
    }

    fn fresh(backend: Arc<dyn Backend>, cookie_age: i64) -> Session {
        Session {
            key: new_session_key(),
            data: HashMap::new(),
            modified: false,
            expiry: Utc::now() + Duration::seconds(cookie_age),
            cookie_age,
            backend,
            flushed: false,
            new: true,
            written: false,
        }
    }

    pub fn is_expired(&self) -> bool { Utc::now() >= self.expiry }

    /// `request.session["k"] = v`
    pub fn set(&mut self, key: impl Into<String>, value: JsonValue) {
        self.data.insert(key.into(), value);
        self.modified = true;
    }

    pub fn get(&self, key: &str) -> Option<&JsonValue> { self.data.get(key) }

    /// `request.session.get("k", default)`
    pub fn get_or(&self, key: &str, default: &str) -> String {
        self.data
            .get(key)
            .and_then(|v| v.as_str())
            .unwrap_or(default)
            .to_string()
    }

    pub fn get_int(&self, key: &str) -> Option<i64> {
        self.data.get(key).and_then(|v| v.as_i64())
    }

    pub fn remove(&mut self, key: &str) -> Option<JsonValue> {
        self.modified = true;
        self.data.remove(key)
    }

    pub fn contains(&self, key: &str) -> bool { self.data.contains_key(key) }

    pub fn keys(&self) -> Vec<String> { self.data.keys().cloned().collect() }

    /// `session.clear()` — keeps the same key, empties data.
    pub fn clear(&mut self) {
        self.data.clear();
        self.modified = true;
    }

    /// `session.flush()` — delete the old session and start a new key.
    pub fn flush(&mut self) {
        self.data.clear();
        self.flushed = true;
        self.modified = true;
        if !self.new {
            let _ = self.backend.delete(
                SESSION_TABLE,
                &Criteria::eq("session_key", Value::Text(self.key.clone())),
            );
        }
        self.key = new_session_key();
        self.expiry = Utc::now() + Duration::seconds(self.cookie_age);
        self.new = true;
    }

    /// `session.cycle_key()` — keep data, rotate the key (used on login to
    /// prevent session fixation, exactly like Django).
    pub fn cycle_key(&mut self) {
        let data = self.data.clone();
        self.flush();
        self.data = data;
    }

    /// Persist to the backend if modified. Called by SessionMiddleware.
    pub fn save(&mut self) {
        if !self.modified && !self.new {
            return;
        }
        self.written = true;
        if self.flushed {
            // flush() already rotated the key; write the (possibly empty)
            // new session row.
            self.flushed = false;
        }
        let json = serde_json::to_string(&self.data).unwrap_or_default();
        let row = vec![
            Value::Text(self.key.clone()),
            Value::Text(json.clone()),
            Value::DateTime(self.expiry),
        ];
        let crit = Criteria::eq("session_key", Value::Text(self.key.clone()));
        match self.backend.update(
            SESSION_TABLE,
            &crit,
            &[(1usize, Value::Text(json.clone())), (2usize, Value::DateTime(self.expiry))],
        ) {
            Ok(0) => {
                let _ = self.backend.insert(SESSION_TABLE, row);
            }
            Ok(_) => {}
            Err(_) => {
                let _ = self.backend.insert(SESSION_TABLE, row);
            }
        }
        self.modified = false;
        self.new = false;
    }

    /// Delete this session entirely (logout).
    pub fn delete(&mut self) {
        let _ = self.backend.delete(
            SESSION_TABLE,
            &Criteria::eq("session_key", Value::Text(self.key.clone())),
        );
        self.data.clear();
        self.modified = true;
        self.new = true;
        self.key = new_session_key();
    }

    /// Purge expired sessions (like `clearsessions` management command).
    pub fn clear_expired(backend: &dyn Backend) -> usize {
        backend
            .delete(SESSION_TABLE, &Criteria::cond("expire_date", Op::Lt, Value::DateTime(Utc::now())))
            .unwrap_or(0)
    }
}

/// Random 32-char lowercase session key, exactly like Django's format.
pub fn new_session_key() -> String {
    // Generate directly from the RNG bytes formatted as hex: 16 random
    // bytes -> 32 uniform hex digits (128 bits). The old `pair % 16` scheme
    // mapped ASCII hex chars through a modulo, skewing the digit
    // distribution (~83 bits effective entropy).
    let uuid = uuid::Uuid::new_v4();
    let bytes = uuid.as_bytes();
    let mut s = String::with_capacity(SESSION_KEY_LEN);
    for b in bytes {
        if s.len() >= SESSION_KEY_LEN {
            break;
        }
        s.push_str(&format!("{b:02x}"));
    }
    s
}

/// Column layout of `django_session` for schema registration.
pub fn session_columns() -> Vec<crate::orm::ColumnDesc> {
    vec![
        crate::orm::ColumnDesc::char("session_key", 40).unique(),
        crate::orm::ColumnDesc::text("session_data"),
        crate::orm::ColumnDesc::datetime("expire_date", false),
    ]
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::orm::InMemoryBackend;

    #[test]
    fn session_set_get_persist() {
        let backend = Arc::new(InMemoryBackend::new());
        let mut s = Session::load(backend.clone(), None, 3600);
        assert!(s.new);
        s.set("user_id", JsonValue::from(7i64));
        s.set("theme", JsonValue::from("dark"));
        s.save();

        let key = s.key.clone();
        let reloaded = Session::load(backend, Some(&key), 3600);
        assert!(!reloaded.new);
        assert_eq!(reloaded.get_int("user_id"), Some(7));
        assert_eq!(reloaded.get_or("theme", "light"), "dark");
    }

    #[test]
    fn session_flush_rotates_key_and_deletes_old() {
        let backend = Arc::new(InMemoryBackend::new());
        let mut s = Session::load(backend.clone(), None, 3600);
        s.set("x", JsonValue::from(1));
        s.save();
        let old_key = s.key.clone();
        s.flush();
        assert_ne!(s.key, old_key);
        // old row gone
        let crit = Criteria::eq("session_key", Value::Text(old_key));
        assert_eq!(backend.count(SESSION_TABLE, &crit).unwrap(), 0);
    }

    #[test]
    fn session_cycle_key_keeps_data() {
        let backend = Arc::new(InMemoryBackend::new());
        let mut s = Session::load(backend, None, 3600);
        s.set("cart", JsonValue::from("apples"));
        let old_key = s.key.clone();
        s.cycle_key();
        assert_ne!(s.key, old_key);
        assert_eq!(s.get_or("cart", "empty"), "apples");
    }

    #[test]
    fn session_delete() {
        let backend = Arc::new(InMemoryBackend::new());
        let mut s = Session::load(backend.clone(), None, 3600);
        s.set("a", JsonValue::from(1));
        s.save();
        let key = s.key.clone();
        s.delete();
        let crit = Criteria::eq("session_key", Value::Text(key));
        assert_eq!(backend.count(SESSION_TABLE, &crit).unwrap(), 0);
    }

    #[test]
    fn unknown_key_gets_fresh_session() {
        let backend = Arc::new(InMemoryBackend::new());
        let s = Session::load(backend, Some("doesnotexist123456"), 3600);
        assert!(s.new);
        assert!(s.data.is_empty());
    }

    #[test]
    fn session_key_format() {
        let k = new_session_key();
        assert_eq!(k.len(), 32);
        assert!(k.chars().all(|c| c.is_ascii_hexdigit()));
        // Keys must be lowercase hex (Django's format).
        assert!(k.chars().all(|c| !c.is_ascii_uppercase()));
    }

    #[test]
    fn session_key_hex_digits_are_uniform() {
        // Regression: `pair % 16` mapped ASCII hex chars through a modulo,
        // skewing the digit distribution (~83 bits effective entropy).
        // Generating straight from RNG bytes must produce every hex digit
        // across a batch of keys, and no two keys may collide.
        let keys: Vec<String> = (0..200).map(|_| new_session_key()).collect();
        let mut seen = std::collections::HashSet::new();
        let mut digits = [0usize; 16];
        for k in &keys {
            assert!(seen.insert(k.clone()), "duplicate session key");
            for c in k.chars() {
                let idx = c.to_digit(16).unwrap() as usize;
                digits[idx] += 1;
            }
        }
        assert!(
            digits.iter().all(|d| *d > 0),
            "every hex digit must occur; counts: {digits:?}"
        );
    }

    #[test]
    fn expired_session_is_rejected() {
        let backend = Arc::new(InMemoryBackend::new());
        let mut s = Session::load(backend.clone(), None, 3600);
        s.set("k", JsonValue::from(1));
        s.save();
        let key = s.key.clone();
        // Force the stored expiry into the past.
        backend
            .update(
                SESSION_TABLE,
                &Criteria::eq("session_key", Value::Text(key.clone())),
                &[(2, Value::DateTime(Utc::now() - Duration::seconds(10)))],
            )
            .unwrap();
        let reloaded = Session::load(backend, Some(&key), 3600);
        assert!(reloaded.new, "expired session must be treated as absent");
        assert!(reloaded.get("k").is_none());
    }
}
