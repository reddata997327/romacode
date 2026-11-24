//! Flash messages — `django.contrib.messages`.
//!
//! Messages are stored in the session between requests, loaded into
//! `request.messages` by MessagesMiddleware, and consumed on read
//! (pop-on-read semantics, like Django).

use serde_json::json;

use crate::http::HttpRequest;

/// Message severity levels with the same tags as Django's default
/// `messages.constants`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Level {
    Debug,
    Info,
    Success,
    Warning,
    Error,
}

impl Level {
    pub fn tag(&self) -> &'static str {
        match self {
            Level::Debug => "debug",
            Level::Info => "info",
            Level::Success => "success",
            Level::Warning => "warning",
            Level::Error => "error",
        }
    }

    pub fn from_tag(tag: &str) -> Option<Level> {
        match tag {
            "debug" => Some(Level::Debug),
            "info" => Some(Level::Info),
            "success" => Some(Level::Success),
            "warning" => Some(Level::Warning),
            "error" => Some(Level::Error),
            _ => None,
        }
    }
}

/// One flash message.
#[derive(Debug, Clone)]
pub struct Message {
    pub level: Level,
    pub text: String,
}

impl Message {
    pub fn new(level: Level, text: impl Into<String>) -> Self {
        Self { level, text: text.into() }
    }

    /// The CSS class Django's default template context produces
    /// (`message.level_tag` + `message.message`).
    pub fn css_class(&self) -> String {
        self.level.tag().to_string()
    }
}

const MESSAGES_KEY: &str = "_messages";

/// `messages.add_message(request, messages.INFO, "...")`
pub fn add_message(req: &mut HttpRequest, level: Level, text: impl Into<String>) {
    req.messages.push(Message::new(level, text));
    persist(req);
}

/// Convenience shorthands matching Django's `messages.info(request, ..)`.
pub fn info(req: &mut HttpRequest, text: impl Into<String>) { add_message(req, Level::Info, text) }
pub fn success(req: &mut HttpRequest, text: impl Into<String>) { add_message(req, Level::Success, text) }
pub fn warning(req: &mut HttpRequest, text: impl Into<String>) { add_message(req, Level::Warning, text) }
pub fn error(req: &mut HttpRequest, text: impl Into<String>) { add_message(req, Level::Error, text) }

/// Called by MessagesMiddleware at request start: load from session.
pub fn load(req: &mut HttpRequest) {
    if let Some(session) = &mut req.session {
        let stored = session
            .get(MESSAGES_KEY)
            .and_then(|v| v.as_array().cloned())
            .unwrap_or_default();
        let msgs: Vec<Message> = stored
            .iter()
            .filter_map(|item| {
                let level = item.get("level").and_then(|l| l.as_str()).and_then(Level::from_tag)?;
                let text = item.get("text").and_then(|t| t.as_str())?.to_string();
                Some(Message::new(level, text))
            })
            .collect();
        req.messages = msgs;
        // Pop-on-read: clear stored copy immediately (Django semantics).
        if !stored.is_empty() {
            session.remove(MESSAGES_KEY);
        }
    }
}

/// Called by MessagesMiddleware after the view: persist newly added ones.
pub fn persist(req: &mut HttpRequest) {
    if let Some(session) = &mut req.session {
        let arr: Vec<serde_json::Value> = req
            .messages
            .iter()
            .map(|m| json!({"level": m.level.tag(), "text": m.text}))
            .collect();
        if arr.is_empty() {
            // Only mark the session modified when a stored message list is
            // actually being dropped — never touch untouched sessions.
            if session.contains(MESSAGES_KEY) {
                session.remove(MESSAGES_KEY);
            }
        } else {
            session.set(MESSAGES_KEY, json!(arr));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::orm::InMemoryBackend;
    use std::sync::Arc;

    fn req_with_session() -> HttpRequest {
        let mut req = HttpRequest::new(crate::http::Method::GET, "/");
        let backend = Arc::new(InMemoryBackend::new());
        req.session = Some(crate::sessions::Session::load(backend, None, 3600));
        req
    }

    #[test]
    fn levels_and_tags() {
        assert_eq!(Level::Success.tag(), "success");
        assert_eq!(Level::from_tag("warning"), Some(Level::Warning));
        assert_eq!(Level::from_tag("bogus"), None);
    }

    #[test]
    fn add_and_load_roundtrip() {
        let mut req = req_with_session();
        success(&mut req, "Profile saved");
        error(&mut req, "Oops");
        persist(&mut req);

        // simulate a new request with same session
        let mut req2 = req_with_session();
        let key = req.session.as_ref().unwrap().key.clone();
        let backend = Arc::new(InMemoryBackend::new()); // replaced below
        let _ = backend;
        req2.session = req.session; // move session across "requests"
        load(&mut req2);
        assert_eq!(req2.messages.len(), 2);
        assert_eq!(req2.messages[0].level, Level::Success);
        assert_eq!(req2.messages[0].text, "Profile saved");
        assert_eq!(req2.messages[1].text, "Oops");
        let _ = key;
    }

    #[test]
    fn pop_on_read() {
        let mut req = req_with_session();
        info(&mut req, "note");
        persist(&mut req);
        // first read delivers the message...
        let mut req2 = req_with_session();
        req2.session = req.session;
        load(&mut req2);
        assert_eq!(req2.messages.len(), 1);
        assert_eq!(req2.messages[0].text, "note");
        // ...and the second read finds nothing (pop-on-read)
        let mut req3 = req_with_session();
        req3.session = req2.session;
        load(&mut req3);
        assert_eq!(req3.messages.len(), 0);
    }
}
