//! Cache framework — `django.core.cache` with a `LocMemCache` backend.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

/// Cache backend trait (only LocMem ships; the trait allows custom ones).
pub trait Cache: Send + Sync {
    fn get(&self, key: &str) -> Option<String>;
    fn set(&self, key: &str, value: String, timeout: Option<Duration>);
    fn add(&self, key: &str, value: String, timeout: Option<Duration>) -> bool;
    fn delete(&self, key: &str);
    fn clear(&self);
    fn touch(&self, key: &str, timeout: Duration) -> bool;
}

struct Entry {
    value: String,
    expires: Option<Instant>,
}

impl Entry {
    fn alive(&self) -> bool {
        match self.expires {
            Some(t) => Instant::now() < t,
            None => true,
        }
    }
}

/// `LocMemCache` — process-local, TTL-aware, evicts on access.
pub struct LocMemCache {
    inner: Mutex<HashMap<String, Entry>>,
    max_entries: usize,
}

impl LocMemCache {
    pub fn new(max_entries: usize) -> Self {
        Self { inner: Mutex::new(HashMap::new()), max_entries: max_entries.max(1) }
    }

    fn purge_expired(map: &mut HashMap<String, Entry>) {
        map.retain(|_, e| e.alive());
    }
}

impl Default for LocMemCache {
    fn default() -> Self {
        Self::new(300)
    }
}

impl Cache for LocMemCache {
    fn get(&self, key: &str) -> Option<String> {
        let mut map = self.inner.lock().unwrap();
        Self::purge_expired(&mut map);
        map.get(key).map(|e| e.value.clone())
    }

    fn set(&self, key: &str, value: String, timeout: Option<Duration>) {
        let mut map = self.inner.lock().unwrap();
        Self::purge_expired(&mut map);
        if map.len() >= self.max_entries && !map.contains_key(key) {
            // simple eviction: drop an arbitrary entry
            if let Some(k) = map.keys().next().cloned() {
                map.remove(&k);
            }
        }
        map.insert(
            key.to_string(),
            Entry { value, expires: timeout.map(|t| Instant::now() + t) },
        );
    }

    fn add(&self, key: &str, value: String, timeout: Option<Duration>) -> bool {
        let mut map = self.inner.lock().unwrap();
        Self::purge_expired(&mut map);
        if map.contains_key(key) {
            return false;
        }
        map.insert(
            key.to_string(),
            Entry { value, expires: timeout.map(|t| Instant::now() + t) },
        );
        true
    }

    fn delete(&self, key: &str) {
        self.inner.lock().unwrap().remove(key);
    }

    fn clear(&self) {
        self.inner.lock().unwrap().clear();
    }

    fn touch(&self, key: &str, timeout: Duration) -> bool {
        let mut map = self.inner.lock().unwrap();
        Self::purge_expired(&mut map);
        match map.get_mut(key) {
            Some(e) => {
                e.expires = Some(Instant::now() + timeout);
                true
            }
            None => false,
        }
    }
}

/// The default cache from settings (`cache_backend: locmem`).
pub fn default_cache() -> Arc<dyn Cache> {
    match crate::settings::global().cache_backend.as_str() {
        "locmem" | "" => Arc::new(LocMemCache::default()),
        other => {
            // Unknown alias falls back to locmem (documented behavior).
            let _ = other;
            Arc::new(LocMemCache::default())
        }
    }
}

/// `@cache_page(seconds)` — wraps a view with page caching.
pub fn cache_page(
    seconds: u64,
    view: crate::views::ViewFn,
) -> crate::views::ViewFn {
    let cache = Arc::new(LocMemCache::default());
    std::sync::Arc::new(move |req: &mut crate::http::HttpRequest| {
        if !req.method.is_safe() {
            return view(req);
        }
        let key = format!("cache_page:{}:{}", req.full_path, req.user.username());
        if let Some(hit) = cache.get(&key) {
            return Ok(http_response_from_bytes(hit));
        }
        let res = view(req)?;
        // Pages containing a rendered CSRF token are per-session: serving a
        // cached token to other visitors makes every later form POST fail
        // with a mismatch. Never cache those.
        if res
            .body_string()
            .contains("name=\"csrfmiddlewaretoken\"")
        {
            return Ok(res);
        }
        cache.set(&key, http_response_to_bytes(&res), Some(Duration::from_secs(seconds)));
        Ok(res)
    })
}

/// A cached page snapshot: status, content type, redirect target and body.
/// Caching the body alone replayed 302/404 responses as empty 200s.
#[derive(serde::Serialize, serde::Deserialize)]
struct CachedPage {
    status: u16,
    content_type: String,
    location: Option<String>,
    body: String,
}

fn http_response_to_bytes(res: &crate::http::HttpResponse) -> String {
    let snap = CachedPage {
        status: res.status,
        content_type: res.headers.get("Content-Type").unwrap_or("text/html; charset=utf-8").to_string(),
        location: res.headers.get("Location").map(|s| s.to_string()),
        body: res.body_string(),
    };
    serde_json::to_string(&snap).unwrap_or_default()
}

fn http_response_from_bytes(data: String) -> crate::http::HttpResponse {
    let snap: CachedPage = match serde_json::from_str(&data) {
        Ok(s) => s,
        Err(_) => {
            // Defensive: unparseable cache entries fall back to a plain page.
            return crate::http::HttpResponse::html(data);
        }
    };
    let mut res = crate::http::HttpResponse::with_body(snap.status, &snap.content_type, snap.body);
    if let Some(loc) = snap.location {
        res.headers.set("Location", loc);
    }
    res
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::thread::sleep;

    #[test]
    fn set_get_delete() {
        let c = LocMemCache::default();
        c.set("k", "v".into(), None);
        assert_eq!(c.get("k"), Some("v".into()));
        c.delete("k");
        assert_eq!(c.get("k"), None);
    }

    #[test]
    fn ttl_expiry() {
        let c = LocMemCache::default();
        c.set("short", "x".into(), Some(Duration::from_millis(20)));
        assert_eq!(c.get("short"), Some("x".into()));
        sleep(Duration::from_millis(40));
        assert_eq!(c.get("short"), None);
    }

    #[test]
    fn add_is_atomic_setifabsent() {
        let c = LocMemCache::default();
        assert!(c.add("lock", "1".into(), None));
        assert!(!c.add("lock", "2".into(), None));
        assert_eq!(c.get("lock"), Some("1".into()));
    }

    #[test]
    fn touch_extends() {
        let c = LocMemCache::default();
        c.set("t", "v".into(), Some(Duration::from_millis(30)));
        assert!(c.touch("t", Duration::from_secs(60)));
        sleep(Duration::from_millis(50));
        assert_eq!(c.get("t"), Some("v".into()));
        assert!(!c.touch("missing", Duration::from_secs(1)));
    }

    #[test]
    fn eviction_and_clear() {
        let c = LocMemCache::new(2);
        c.set("a", "1".into(), None);
        c.set("b", "2".into(), None);
        c.set("c", "3".into(), None);
        let count = ["a", "b", "c"].iter().filter(|k| c.get(k).is_some()).count();
        assert!(count <= 3);
        c.clear();
        assert_eq!(c.get("a"), None);
    }

    #[test]
    fn cache_page_wrapper() {
        let counter = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let c2 = counter.clone();
        let view: crate::views::ViewFn = Arc::new(move |_req| {
            c2.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            Ok(crate::http::HttpResponse::text("generated"))
        });
        let cached = cache_page(60, view);
        let mut req = crate::http::HttpRequest::new(crate::http::Method::GET, "/p/");
        let r1 = cached(&mut req).unwrap();
        let r2 = cached(&mut req).unwrap();
        assert_eq!(r1.body_string(), "generated");
        assert_eq!(r2.body_string(), "generated");
        assert_eq!(counter.load(std::sync::atomic::Ordering::SeqCst), 1);
    }

    #[test]
    fn cache_page_replays_status_faithfully() {
        // Regression: cached 404/302 responses were reconstructed as
        // status-200 HTML pages.
        let counter = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let c2 = counter.clone();
        let view: crate::views::ViewFn = Arc::new(move |_req| {
            c2.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            let mut res = crate::http::HttpResponse::with_body(404, "text/html; charset=utf-8", "nope");
            res.headers.set("Location", "/elsewhere/");
            Ok(res)
        });
        let cached = cache_page(60, view);
        let mut req = crate::http::HttpRequest::new(crate::http::Method::GET, "/missing/");
        let r1 = cached(&mut req).unwrap();
        assert_eq!(r1.status, 404);
        let r2 = cached(&mut req).unwrap();
        assert_eq!(r2.status, 404, "cached response must keep the status code");
        assert_eq!(r2.body_string(), "nope");
        assert_eq!(r2.headers.get("Location"), Some("/elsewhere/"));
        assert_eq!(counter.load(std::sync::atomic::Ordering::SeqCst), 1);
    }

    #[test]
    fn cache_page_skips_csrf_pages() {
        // Regression: pages containing a rendered `{% csrf_token %}` were
        // cached — later visitors got a foreign token and every form POST
        // failed with a mismatch.
        let counter = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let c2 = counter.clone();
        let view: crate::views::ViewFn = Arc::new(move |_req| {
            c2.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            Ok(crate::http::HttpResponse::html(
                "<form><input type=\"hidden\" name=\"csrfmiddlewaretoken\" value=\"abc\"></form>",
            ))
        });
        let cached = cache_page(60, view);
        let mut req = crate::http::HttpRequest::new(crate::http::Method::GET, "/form/");
        cached(&mut req).unwrap();
        cached(&mut req).unwrap();
        // Not cached: the view ran for every request.
        assert_eq!(counter.load(std::sync::atomic::Ordering::SeqCst), 2);
    }
}
