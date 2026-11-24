//! URL routing — the `django.urls` equivalent.
//!
//! Patterns use Django's path syntax with converters:
//!
//! ```ignore
//! use rustango::urls::{path, Router};
//!
//! let router = Router::new(vec![
//!     path("", index_view),                       // /
//!     path("polls/", polls_index),                // /polls/
//!     path("polls/<int:pk>/", poll_detail).name("poll_detail"),
//!     path("files/<path:rest>/", serve_file),
//! ]);
//!
//! let url = reverse("poll_detail", &["42"]);      // /polls/42/
//! ```

use std::collections::HashMap;
use std::sync::RwLock;

use crate::views::ViewFn;

/// A function view reference — `def my_view(request): ...`
pub type RouteView = ViewFn;

/// Path converters, mirroring Django's registered converters.
#[derive(Debug, Clone, PartialEq)]
pub enum Converter {
    /// Default: any text without a slash.
    Str,
    /// Integer, parsed to i64.
    Int,
    /// Letters, digits, underscore, hyphen.
    Slug,
    /// UUID.
    Uuid,
    /// Greedy: matches across slashes, including the empty string.
    Path,
}

/// One segment of a path pattern.
#[derive(Debug, Clone)]
pub enum Segment {
    Literal(String),
    Param { name: String, converter: Converter },
}

#[derive(Debug, Clone)]
pub struct Pattern {
    /// Normalized segments (trailing empty segment from a trailing slash
    /// is preserved so `polls/` only matches `/polls/`).
    pub segments: Vec<Segment>,
    pub raw: String,
}

impl Pattern {
    /// Parse `"polls/<int:pk>/"` into segments. A trailing slash is kept
    /// as a trailing empty segment so `polls/` only matches `/polls/`.
    pub fn parse(raw: &str) -> Pattern {
        let raw_stored = raw.to_string();
        let raw = raw.strip_prefix('/').unwrap_or(raw);
        let mut segments = Vec::new();
        for part in raw.split('/') {
            if part.is_empty() {
                segments.push(Segment::Literal(String::new()));
                continue;
            }
            if let Some(inner) = part.strip_prefix('<').and_then(|s| s.strip_suffix('>')) {
                let (kind, name) = inner.split_once(':').unwrap_or(("str", inner));
                let converter = match kind.trim() {
                    "int" => Converter::Int,
                    "slug" => Converter::Slug,
                    "uuid" => Converter::Uuid,
                    "path" => Converter::Path,
                    _ => Converter::Str,
                };
                segments.push(Segment::Param { name: name.trim().to_string(), converter });
            } else {
                segments.push(Segment::Literal(part.to_string()));
            }
        }
        Pattern { segments, raw: raw_stored }
    }

    /// Match a request path; returns captured params. A trailing
    /// `<path:name>` converter greedily matches the rest of the path
    /// (slashes included), honouring a trailing slash on the pattern
    /// (`files/<path:rest>/` only matches paths ending in `/`).
    pub fn matches(&self, path: &str) -> Option<HashMap<String, String>> {
        let path_trimmed = path.trim_start_matches('/');
        let parts: Vec<&str> = path_trimmed.split('/').collect();

        // Greedy <path:...> handling (converter must be the final core
        // segment; a trailing empty literal means the pattern ends with '/').
        let greedy_pos = self.segments.iter().position(|s| {
            matches!(s, Segment::Param { converter: Converter::Path, .. })
        });
        if let Some(pos) = greedy_pos {
            let has_trailing_slash = matches!(
                self.segments.last(),
                Some(Segment::Literal(s)) if s.is_empty() && self.segments.len() > 1
            );
            let core_len = if has_trailing_slash { self.segments.len() - 1 } else { self.segments.len() };
            if pos + 1 == core_len {
                if parts.len() < pos + 1 {
                    return None;
                }
                let value_parts: &[&str] = if has_trailing_slash {
                    // The URL must end with a '/' (empty final part).
                    if parts.last() != Some(&"") {
                        return None;
                    }
                    &parts[pos..parts.len() - 1]
                } else {
                    &parts[pos..]
                };
                if value_parts.is_empty() {
                    return None;
                }
                let mut params = HashMap::new();
                for (i, seg) in self.segments[..pos].iter().enumerate() {
                    match seg {
                        Segment::Literal(lit) => {
                            if lit != parts[i] {
                                return None;
                            }
                        }
                        Segment::Param { name, converter } => {
                            if !valid_param(converter, parts[i]) {
                                return None;
                            }
                            params.insert(name.clone(), parts[i].to_string());
                        }
                    }
                }
                if let Segment::Param { name, .. } = &self.segments[pos] {
                    params.insert(name.clone(), value_parts.join("/"));
                }
                return Some(params);
            }
        }

        if parts.len() != self.segments.len() {
            return None;
        }
        let mut params = HashMap::new();
        for (seg, part) in self.segments.iter().zip(parts.iter()) {
            match seg {
                Segment::Literal(lit) => {
                    if lit != part {
                        return None;
                    }
                }
                Segment::Param { name, converter } => {
                    if !valid_param(converter, part) {
                        return None;
                    }
                    params.insert(name.clone(), (*part).to_string());
                }
            }
        }
        Some(params)
    }

    /// Build a URL by substituting params in order; for `reverse()`.
    pub fn build(&self, args: &[String]) -> Result<String, String> {
        let mut args = args.iter();
        // Drop the trailing empty literal (the trailing-slash marker).
        let core: &[Segment] = match self.segments.last() {
            Some(Segment::Literal(s)) if s.is_empty() && self.segments.len() > 1 => {
                &self.segments[..self.segments.len() - 1]
            }
            _ => &self.segments[..],
        };
        let mut parts: Vec<String> = Vec::new();
        for seg in core {
            match seg {
                Segment::Literal(lit) => parts.push(lit.clone()),
                Segment::Param { converter, .. } => {
                    let val = args.next().ok_or_else(|| "not enough arguments".to_string())?;
                    if !valid_param(converter, val) {
                        return Err(format!("`{val}` does not match {converter:?} converter"));
                    }
                    parts.push(val.clone());
                }
            }
        }
        let mut out = String::from("/");
        out.push_str(&parts.join("/"));
        if self.raw.is_empty() {
            // The root pattern "" is just "/".
            out = "/".into();
        } else if self.raw.ends_with('/') {
            out.push('/');
        }
        Ok(out)
    }

    pub fn param_names(&self) -> Vec<String> {
        self.segments
            .iter()
            .filter_map(|s| match s {
                Segment::Param { name, .. } => Some(name.clone()),
                _ => None,
            })
            .collect()
    }
}

fn valid_param(c: &Converter, val: &str) -> bool {
    match c {
        Converter::Int => val.parse::<i64>().is_ok(),
        Converter::Str => !val.is_empty() && !val.contains('/'),
        Converter::Slug => {
            !val.is_empty()
                && val
                    .chars()
                    .all(|ch| ch.is_ascii_alphanumeric() || ch == '_' || ch == '-')
        }
        Converter::Uuid => uuid::Uuid::parse_str(val).is_ok(),
        Converter::Path => true,
    }
}

/// A single URL route: pattern + view + optional name.
#[derive(Clone)]
pub struct Route {
    pub pattern: Pattern,
    pub view: RouteView,
    pub name: Option<String>,
    /// Skip CSRF verification for this route (like Django's `@csrf_exempt`).
    pub csrf_exempt: bool,
}

impl Route {
    pub fn name(mut self, name: impl Into<String>) -> Route {
        self.name = Some(name.into());
        self
    }

    /// Mark the route exempt from CSRF verification — `@csrf_exempt`.
    pub fn csrf_exempt(mut self) -> Route {
        self.csrf_exempt = true;
        self
    }
}

/// `path("polls/<int:pk>/", detail)` — Django's `django.urls.path`.
pub fn path(pattern: &str, view: RouteView) -> Route {
    Route { pattern: Pattern::parse(pattern), view, name: None, csrf_exempt: false }
}

/// Convenience: `path(...).name("x")` shorthand.
pub fn named_path(pattern: &str, view: RouteView, name: &str) -> Route {
    path(pattern, view).name(name)
}

/// The router — holds all routes, in order, first match wins.
#[derive(Clone, Default)]
pub struct Router {
    pub routes: Vec<Route>,
}

impl Router {
    pub fn new(routes: Vec<Route>) -> Router { Router { routes } }

    pub fn empty() -> Router { Router { routes: vec![] } }

    /// Append another router's routes (like including `urls.py`).
    pub fn merge(&mut self, other: Router) {
        self.routes.extend(other.routes);
    }

    /// Mount another router under a path prefix and a namespace; route
    /// names become `namespace:name` (like `app_name` includes).
    pub fn mount(&mut self, prefix: &str, namespace: &str, other: Router) {
        let pfx = prefix.trim_matches('/');
        for route in other.routes {
            let joined = if route.pattern.raw.is_empty() {
                // Root route under a prefix: keep the trailing slash.
                if pfx.is_empty() { String::new() } else { format!("{pfx}/") }
            } else if pfx.is_empty() {
                route.pattern.raw.clone()
            } else {
                format!("{pfx}/{}", route.pattern.raw)
            };
            let name = route.name.as_ref().map(|n| {
                if namespace.is_empty() { n.clone() } else { format!("{namespace}:{n}") }
            });
            self.routes.push(Route {
                pattern: Pattern::parse(&joined),
                view: route.view,
                name,
                csrf_exempt: route.csrf_exempt,
            });
        }
    }

    /// First pattern match (like `URLResolver.resolve`).
    pub fn match_path(&self, path: &str) -> Option<(Route, HashMap<String, String>)> {
        for route in &self.routes {
            if let Some(params) = route.pattern.matches(path) {
                return Some((route.clone(), params));
            }
        }
        None
    }

    /// Does any route match this path at all (used by APPEND_SLASH logic)?
    pub fn matches(&self, path: &str) -> bool { self.match_path(path).is_some() }
}

/// Reverse a named route with positional arguments — `django.urls.reverse`.
pub fn reverse(name: &str, args: &[&str]) -> Result<String, String> {
    reverse_with(name, &[], args)
}

/// Reverse with extra query parameters appended.
pub fn reverse_with(
    name: &str,
    query: &[(&str, &str)],
    args: &[&str],
) -> Result<String, String> {
    // The registry is injected by the project; tests may set it directly.
    let router = route_registry().ok_or_else(|| "no URL registry configured".to_string())?;
    let route = router
        .routes
        .iter()
        .find(|r| r.name.as_deref() == Some(name))
        .ok_or_else(|| format!("reverse for '{name}' not found"))?;
    let args: Vec<String> = args.iter().map(|s| s.to_string()).collect();
    let mut url = route.pattern.build(&args)?;
    if !query.is_empty() {
        let qs: Vec<String> = query
            .iter()
            .map(|(k, v)| format!("{}={}", crate::http::urlencode(k), crate::http::urlencode(v)))
            .collect();
        url.push('?');
        url.push_str(&qs.join("&"));
    }
    Ok(url)
}

static ROUTE_REGISTRY: RwLock<Option<Router>> = RwLock::new(None);

/// Install the global registry so `reverse()` works everywhere.
/// First call wins (like configuring ROOT_URLCONF); later calls are no-ops.
pub fn set_route_registry(router: Router) -> bool {
    let mut slot = ROUTE_REGISTRY.write().unwrap();
    if slot.is_none() {
        *slot = Some(router);
        true
    } else {
        false
    }
}

/// Test hook: forcibly replace the registry (documented, test-only).
#[doc(hidden)]
pub fn set_route_registry_for_tests(router: Router) {
    *ROUTE_REGISTRY.write().unwrap() = Some(router);
}

/// Serialize tests that install the process-global route registry: the
/// registry is one process-wide slot, so concurrent test threads installing
/// different routers would race each other's `reverse()` lookups.
#[doc(hidden)]
pub fn registry_test_lock() -> std::sync::MutexGuard<'static, ()> {
    static LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());
    LOCK.lock().unwrap_or_else(|e| e.into_inner())
}

/// Cloned accessor for the global registry.
pub fn route_registry() -> Option<Router> {
    ROUTE_REGISTRY.read().unwrap().clone()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::http::{HttpRequest, HttpResponse};
    use std::sync::Arc;
    
    fn dummy_view() -> ViewFn {
        Arc::new(|_req: &mut HttpRequest| Ok(HttpResponse::text("ok")))
    }

    #[test]
    fn pattern_parse_and_match() {
        let p = Pattern::parse("polls/<int:pk>/");
        assert_eq!(p.segments.len(), 3);
        let m = p.matches("/polls/42/").unwrap();
        assert_eq!(m.get("pk").map(String::as_str), Some("42"));
        assert!(p.matches("/polls/abc/").is_none());
        assert!(p.matches("/polls/42").is_none()); // trailing slash required
        assert!(p.matches("/other/1/").is_none());
    }

    #[test]
    fn greedy_path_converter_with_trailing_slash() {
        // Regression: `files/<path:rest>/` used to match nothing at all
        // because the trailing empty literal broke the greedy branch.
        let p = Pattern::parse("files/<path:rest>/");
        let m = p.matches("/files/a/b/c/").unwrap();
        assert_eq!(m.get("rest").map(String::as_str), Some("a/b/c"));
        let m = p.matches("/files/x/").unwrap();
        assert_eq!(m.get("rest").map(String::as_str), Some("x"));
        // no trailing slash -> no match (pattern demands one)
        assert!(p.matches("/files/a/b").is_none());
        // bare prefix -> nothing to capture
        assert!(p.matches("/files/").is_none());
        assert_eq!(p.build(&["a/b".to_string()]).unwrap(), "/files/a/b/");
    }

    #[test]
    fn converters() {
        let p = Pattern::parse("slug/<slug:s>/uuid/<uuid:u>/file/<path:rest>");
        let m = p
            .matches("/slug/my-post/uuid/550e8400-e29b-41d4-a716-446655440000/file/a/b/c")
            .unwrap();
        assert_eq!(m.get("s").map(String::as_str), Some("my-post"));
        assert_eq!(m.get("rest").map(String::as_str), Some("a/b/c"));
        assert!(p.matches("/slug/ok/uuid/550e8400-e29b-41d4-a716-446655440000/file/").is_some());
        assert!(p.matches("/slug/bad!/uuid/550e8400-e29b-41d4-a716-446655440000/file/x").is_none());
    }

    #[test]
    fn root_pattern() {
        let p = Pattern::parse("");
        assert!(p.matches("/").is_some());
        assert!(p.matches("/x").is_none());
    }

    #[test]
    fn router_first_match_wins() {
        let r = Router::new(vec![
            path("polls/special/", dummy_view()).name("special"),
            path("polls/<int:pk>/", dummy_view()).name("detail"),
            path("polls/<str:slug>/", dummy_view()).name("by-slug"),
        ]);
        let (route, params) = r.match_path("/polls/special/").unwrap();
        assert_eq!(route.name.as_deref(), Some("special"));
        assert!(params.is_empty());
        let (route, params) = r.match_path("/polls/7/").unwrap();
        assert_eq!(route.name.as_deref(), Some("detail"));
        assert_eq!(params.get("pk").map(String::as_str), Some("7"));
        let (route, _) = r.match_path("/polls/hello/").unwrap();
        assert_eq!(route.name.as_deref(), Some("by-slug"));
        assert!(r.match_path("/nope/").is_none());
    }

    #[test]
    fn mount_with_namespace() {
        let mut admin = Router::new(vec![
            path("", dummy_view()).name("index"),
            path("login/", dummy_view()).name("login"),
        ]);
        let mut root = Router::empty();
        root.mount("admin/", "admin", admin.clone());
        admin.routes.clear();
        root.merge(Router::empty());
        assert!(root.routes.iter().any(|r| r.name.as_deref() == Some("admin:index")));
        let p = &root.routes[0].pattern;
        assert!(p.matches("/admin/").is_some());
    }

    #[test]
    fn build_urls() {
        let p = Pattern::parse("polls/<int:pk>/vote/");
        let url = p.build(&["42".to_string()]).unwrap();
        assert_eq!(url, "/polls/42/vote/");
        let p2 = Pattern::parse("");
        assert_eq!(p2.build(&[]).unwrap(), "/");
        let bad = Pattern::parse("x/<int:n>/").build(&["zzz".to_string()]);
        assert!(bad.is_err());
    }

    #[test]
    fn reverse_flow() {
        let _lock = crate::urls::registry_test_lock();
        let router = Router::new(vec![
            path("about/", dummy_view()).name("about"),
            path("blog/<int:year>/<slug:slug>/", dummy_view()).name("post"),
            path("search/", dummy_view()).name("search"),
        ]);
        // The registry is process-global; use the forced test setter.
        set_route_registry_for_tests(router);

        assert_eq!(reverse("about", &[]).unwrap(), "/about/");
        assert_eq!(
            reverse("post", &["2026", "hello-world"]).unwrap(),
            "/blog/2026/hello-world/"
        );
        assert_eq!(
            reverse_with("search", &[("q", "rust lang")], &[]).unwrap(),
            "/search/?q=rust%20lang"
        );
        assert!(reverse("missing", &[]).is_err());
    }

    #[test]
    fn path_named_helper() {
        let r = Router::new(vec![named_path("help/", dummy_view(), "help")]);
        assert_eq!(r.routes[0].name.as_deref(), Some("help"));
    }
}
