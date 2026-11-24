//! HTTP primitives — the `django.http` equivalent.
//!
//! [`HttpRequest`] mirrors `django.http.HttpRequest` (method, path, GET/POST
//! [`QueryDict`], COOKIES, headers, plus framework-injected `user`, `session`
//! and resolver match data). [`HttpResponse`] mirrors `HttpResponse` with the
//! usual shortcut constructors (`html`, `json`, `redirect`, ...).

use std::collections::HashMap;

use serde_json::Value as JsonValue;

use crate::errors::HttpError;

// ---------------------------------------------------------------------------
// Headers & cookies
// ---------------------------------------------------------------------------

/// Case-insensitive header map.
#[derive(Debug, Clone, Default)]
pub struct Headers {
    items: Vec<(String, String)>,
}

impl Headers {
    pub fn new() -> Self { Self::default() }

    pub fn set(&mut self, name: impl Into<String>, value: impl Into<String>) {
        let name = name.into();
        let value = value.into();
        if let Some(existing) = self.items.iter_mut().find(|(n, _)| n.eq_ignore_ascii_case(&name)) {
            existing.1 = value;
        } else {
            self.items.push((name, value));
        }
    }

    pub fn add(&mut self, name: impl Into<String>, value: impl Into<String>) {
        self.items.push((name.into(), value.into()));
    }

    pub fn get(&self, name: &str) -> Option<&str> {
        self.items
            .iter()
            .find(|(n, _)| n.eq_ignore_ascii_case(name))
            .map(|(_, v)| v.as_str())
    }

    /// All values for a header, in order (headers may repeat,
    /// e.g. multiple `Cookie:` lines).
    pub fn get_all(&self, name: &str) -> Vec<&str> {
        self.items
            .iter()
            .filter(|(n, _)| n.eq_ignore_ascii_case(name))
            .map(|(_, v)| v.as_str())
            .collect()
    }

    pub fn remove(&mut self, name: &str) {
        self.items.retain(|(n, _)| !n.eq_ignore_ascii_case(name));
    }

    pub fn iter(&self) -> impl Iterator<Item = (&str, &str)> {
        self.items.iter().map(|(n, v)| (n.as_str(), v.as_str()))
    }
}

/// Parse a `Cookie:` header value into a map.
pub fn parse_cookie_header(header: &str) -> HashMap<String, String> {
    let mut out = HashMap::new();
    for part in header.split(';') {
        let part = part.trim();
        if part.is_empty() {
            continue;
        }
        if let Some((k, v)) = part.split_once('=') {
            out.insert(urldecode(k.trim()), urldecode(v.trim()));
        }
    }
    out
}

// ---------------------------------------------------------------------------
// QueryDict — the multi-value GET/POST bag
// ---------------------------------------------------------------------------

/// Multi-value string map, like `django.http.QueryDict`.
#[derive(Debug, Clone, Default)]
pub struct QueryDict {
    data: HashMap<String, Vec<String>>,
}

impl QueryDict {
    pub fn new() -> Self { Self::default() }

    /// Parse `a=1&b=2&b=3` form/query-string encoding.
    pub fn parse(encoded: &str) -> Self {
        let mut qd = Self::new();
        for pair in encoded.split('&') {
            if pair.is_empty() {
                continue;
            }
            let (k, v) = pair.split_once('=').unwrap_or((pair, ""));
            qd.add(urldecode(k), urldecode(v.replace('+', " ").as_str()));
        }
        qd
    }

    pub fn add(&mut self, key: impl Into<String>, value: impl Into<String>) {
        self.data.entry(key.into()).or_default().push(value.into());
    }

    /// Last value, like `QueryDict.__getitem__` / `.get()`.
    pub fn get(&self, key: &str) -> Option<&str> {
        self.data.get(key).and_then(|v| v.last()).map(|s| s.as_str())
    }

    /// First value (like `QueryDict.__getitem__` which returns the last;
    /// we expose both).
    pub fn get_first(&self, key: &str) -> Option<&str> {
        self.data.get(key).and_then(|v| v.first()).map(|s| s.as_str())
    }

    /// All values for a key, like `QueryDict.getlist()`.
    pub fn getlist(&self, key: &str) -> Vec<&str> {
        self.data
            .get(key)
            .map(|v| v.iter().map(|s| s.as_str()).collect())
            .unwrap_or_default()
    }

    pub fn set(&mut self, key: impl Into<String>, value: impl Into<String>) {
        self.data.insert(key.into(), vec![value.into()]);
    }

    pub fn contains(&self, key: &str) -> bool { self.data.contains_key(key) }

    pub fn keys(&self) -> impl Iterator<Item = &str> { self.data.keys().map(|k| k.as_str()) }

    pub fn is_empty(&self) -> bool { self.data.is_empty() }

    pub fn len(&self) -> usize { self.data.len() }

    /// Encode back to form-urlencoded.
    pub fn encode(&self) -> String {
        let mut out = String::new();
        for (k, vs) in &self.data {
            for v in vs {
                if !out.is_empty() {
                    out.push('&');
                }
                out.push_str(&urlencode(k));
                out.push('=');
                out.push_str(&urlencode(v));
            }
        }
        out
    }
}

/// Percent-decode a URL component.
pub fn urldecode(s: &str) -> String {
    let bytes = s.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        match bytes[i] {
            b'%' if i + 2 < bytes.len() => {
                let hex_str = std::str::from_utf8(&bytes[i + 1..i + 3]).unwrap_or("");
                if let Ok(b) = u8::from_str_radix(hex_str, 16) {
                    out.push(b);
                    i += 3;
                } else {
                    out.push(bytes[i]);
                    i += 1;
                }
            }
            b'+' => {
                out.push(b' ');
                i += 1;
            }
            b => {
                out.push(b);
                i += 1;
            }
        }
    }
    String::from_utf8_lossy(&out).into_owned()
}

/// Percent-encode a URL component (RFC 3986 unreserved set kept).
pub fn urlencode(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for b in s.bytes() {
        match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'.' | b'_' | b'~' => {
                out.push(b as char)
            }
            _ => out.push_str(&format!("%{b:02X}")),
        }
    }
    out
}

// ---------------------------------------------------------------------------
// Request
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Method { GET, POST, PUT, PATCH, DELETE, HEAD, OPTIONS, TRACE }

impl Method {
    pub fn as_str(&self) -> &'static str {
        match self {
            Method::GET => "GET",
            Method::POST => "POST",
            Method::PUT => "PUT",
            Method::PATCH => "PATCH",
            Method::DELETE => "DELETE",
            Method::HEAD => "HEAD",
            Method::OPTIONS => "OPTIONS",
            Method::TRACE => "TRACE",
        }
    }

    pub fn parse(s: &str) -> Option<Method> {
        match s.to_ascii_uppercase().as_str() {
            "GET" => Some(Method::GET),
            "POST" => Some(Method::POST),
            "PUT" => Some(Method::PUT),
            "PATCH" => Some(Method::PATCH),
            "DELETE" => Some(Method::DELETE),
            "HEAD" => Some(Method::HEAD),
            "OPTIONS" => Some(Method::OPTIONS),
            "TRACE" => Some(Method::TRACE),
            _ => None,
        }
    }

    /// Django's "safe methods" for CSRF: GET, HEAD, OPTIONS, TRACE.
    pub fn is_safe(&self) -> bool {
        matches!(self, Method::GET | Method::HEAD | Method::OPTIONS | Method::TRACE)
    }
}

impl std::fmt::Display for Method {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

/// Authenticated-or-anonymous user attached by the auth middleware.
/// Full `User` model lives in [`crate::auth`].
#[derive(Debug, Clone, Default)]
pub enum AuthUser {
    #[default]
    Anonymous,
    Authenticated {
        id: i64,
        username: String,
        is_staff: bool,
        is_superuser: bool,
    },
}

impl AuthUser {
    pub fn is_authenticated(&self) -> bool {
        matches!(self, AuthUser::Authenticated { .. })
    }
    pub fn is_anonymous(&self) -> bool { !self.is_authenticated() }
    pub fn id(&self) -> Option<i64> {
        match self {
            AuthUser::Anonymous => None,
            AuthUser::Authenticated { id, .. } => Some(*id),
        }
    }
    pub fn username(&self) -> &str {
        match self {
            AuthUser::Anonymous => "",
            AuthUser::Authenticated { username, .. } => username,
        }
    }
    pub fn is_staff(&self) -> bool {
        matches!(self, AuthUser::Authenticated { is_staff: true, .. })
    }
    pub fn is_superuser(&self) -> bool {
        matches!(self, AuthUser::Authenticated { is_superuser: true, .. })
    }
}

/// The request object. Framework middleware populates `user`, `session`,
/// `params` (path kwargs) and `csrf_token` before the view runs.
pub struct HttpRequest {
    pub method: Method,
    /// Path portion, e.g. `/polls/3/vote/`.
    pub path: String,
    /// Full path incl. query string (like `request.get_full_path()`).
    pub full_path: String,
    pub query: QueryDict,
    pub post: QueryDict,
    pub headers: Headers,
    pub cookies: HashMap<String, String>,
    pub body: Vec<u8>,
    /// Path kwargs resolved by the router (`<int:pk>` -> `pk`).
    pub params: HashMap<String, String>,
    /// Name of the matched route (like `resolver_match.url_name`).
    pub url_name: Option<String>,
    /// Set by SessionMiddleware.
    pub session: Option<crate::sessions::Session>,
    /// Set by AuthMiddleware.
    pub user: AuthUser,
    /// Set by CsrfMiddleware; used by `{% csrf_token %}`.
    pub csrf_token: Option<String>,
    /// Flash messages loaded by MessagesMiddleware.
    pub messages: Vec<crate::messages::Message>,
    /// Remote address string.
    pub remote_addr: String,
    /// Template engine (set by the dispatcher), for `views::render`.
    pub engine: Option<std::sync::Arc<crate::templates::Engine>>,
    /// Database backend (set by the dispatcher), for ORM access in views.
    pub backend: Option<std::sync::Arc<dyn crate::orm::Backend>>,
}

impl HttpRequest {
    pub fn new(method: Method, path: impl Into<String>) -> Self {
        let path = path.into();
        let (p, q) = path.split_once('?').unwrap_or((&path, ""));
        Self {
            method,
            path: p.to_string(),
            full_path: if q.is_empty() { p.to_string() } else { format!("{p}?{q}") },
            query: QueryDict::parse(q),
            post: QueryDict::new(),
            headers: Headers::new(),
            cookies: HashMap::new(),
            body: Vec::new(),
            params: HashMap::new(),
            url_name: None,
            session: None,
            user: AuthUser::Anonymous,
            csrf_token: None,
            messages: Vec::new(),
            remote_addr: "127.0.0.1".into(),
            engine: None,
            backend: None,
        }
    }

    /// `request.GET` in Django.
    #[allow(non_snake_case)]
    pub fn GET(&self) -> &QueryDict { &self.query }

    /// `request.POST` in Django (form-encoded bodies are parsed eagerly).
    #[allow(non_snake_case)]
    pub fn POST(&self) -> &QueryDict { &self.post }

    pub fn header(&self, name: &str) -> Option<&str> { self.headers.get(name) }

    pub fn cookie(&self, name: &str) -> Option<&str> {
        self.cookies.get(name).map(|s| s.as_str())
    }

    /// Path parameter captured by the router.
    pub fn param(&self, name: &str) -> Option<&str> { self.params.get(name).map(|s| s.as_str()) }

    /// Path parameter parsed as i64 (`<int:pk>`).
    pub fn int_param(&self, name: &str) -> Result<i64, HttpError> {
        self.param(name)
            .and_then(|s| s.parse().ok())
            .ok_or_else(|| HttpError::NotFound(format!("invalid `{name}` path parameter")))
    }

    /// True when the request was made over HTTPS (X-Forwarded-Proto aware).
    pub fn is_secure(&self) -> bool {
        self.headers.get("x-forwarded-proto").map(|p| p == "https").unwrap_or(false)
    }

    /// `request.is_ajax()`-style helper: X-Requested-With: XMLHttpRequest.
    pub fn is_ajax(&self) -> bool {
        self.headers.get("x-requested-with").map(|v| v.eq_ignore_ascii_case("XMLHttpRequest")).unwrap_or(false)
    }

    pub fn get_full_path(&self) -> &str { &self.full_path }

    /// Build an absolute URI for this request.
    pub fn build_absolute_uri(&self, location: &str) -> String {
        if location.starts_with("http://") || location.starts_with("https://") {
            return location.to_string();
        }
        let host = self.headers.get("host").unwrap_or("localhost");
        let scheme = if self.is_secure() { "https" } else { "http" };
        if location.starts_with('/') {
            format!("{scheme}://{host}{location}")
        } else {
            format!("{scheme}://{host}/{location}")
        }
    }
}

impl std::fmt::Debug for HttpRequest {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("HttpRequest")
            .field("method", &self.method)
            .field("path", &self.path)
            .field("user", &self.user.username())
            .finish()
    }
}

// ---------------------------------------------------------------------------
// Response
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Default)]
pub struct SetCookie {
    pub name: String,
    pub value: String,
    pub max_age: Option<i64>,
    pub path: String,
    pub http_only: bool,
    pub secure: bool,
    pub same_site: Option<String>,
}

impl SetCookie {
    pub fn new(name: impl Into<String>, value: impl Into<String>) -> Self {
        Self {
            name: name.into(),
            value: value.into(),
            max_age: None,
            path: "/".into(),
            http_only: false,
            secure: false,
            same_site: None,
        }
    }

    pub fn max_age(mut self, secs: i64) -> Self { self.max_age = Some(secs); self }
    pub fn http_only(mut self) -> Self { self.http_only = true; self }
    pub fn secure(mut self) -> Self { self.secure = true; self }
    pub fn same_site(mut self, v: impl Into<String>) -> Self { self.same_site = Some(v.into()); self }

    pub fn expired(name: impl Into<String>) -> Self {
        Self { name: name.into(), value: String::new(), max_age: Some(0), ..Self::new("", "") }
    }

    pub fn to_header(&self) -> String {
        let mut s = format!("{}={}", urlencode(&self.name), urlencode(&self.value));
        s.push_str(&format!("; Path={}", self.path));
        if let Some(ma) = self.max_age {
            s.push_str(&format!("; Max-Age={ma}"));
        }
        if self.http_only {
            s.push_str("; HttpOnly");
        }
        if self.secure {
            s.push_str("; Secure");
        }
        if let Some(ss) = &self.same_site {
            s.push_str(&format!("; SameSite={ss}"));
        }
        s
    }
}

/// The response object — status, headers, cookies and a byte body.
#[derive(Debug, Clone)]
pub struct HttpResponse {
    pub status: u16,
    pub headers: Headers,
    pub cookies: Vec<SetCookie>,
    body: Vec<u8>,
}

impl HttpResponse {
    pub fn new(status: u16) -> Self {
        Self { status, headers: Headers::new(), cookies: Vec::new(), body: Vec::new() }
    }

    pub fn with_body(status: u16, content_type: &str, body: impl Into<Vec<u8>>) -> Self {
        let mut r = Self::new(status);
        r.headers.set("Content-Type", content_type);
        r.body = body.into();
        r
    }

    /// 200 text/html — like `HttpResponse("<h1>..</h1>")`.
    pub fn html(body: impl Into<Vec<u8>>) -> Self {
        Self::with_body(200, "text/html; charset=utf-8", body)
    }

    /// 200 text/plain.
    pub fn text(body: impl Into<String>) -> Self {
        Self::with_body(200, "text/plain; charset=utf-8", body.into().into_bytes())
    }

    /// 200 application/json — like `JsonResponse`.
    pub fn json(value: &JsonValue) -> Self {
        Self::with_body(200, "application/json", serde_json::to_string(value).unwrap_or_default())
    }

    /// Any status + JSON body.
    pub fn json_status(status: u16, value: &JsonValue) -> Self {
        Self::with_body(status, "application/json", serde_json::to_string(value).unwrap_or_default())
    }

    /// 302 redirect — like `HttpResponseRedirect`. Relative or absolute.
    pub fn redirect(to: impl Into<String>) -> Self {
        let mut r = Self::new(302);
        r.headers.set("Location", to.into());
        r
    }

    /// 301 permanent redirect.
    pub fn redirect_permanent(to: impl Into<String>) -> Self {
        let mut r = Self::new(301);
        r.headers.set("Location", to.into());
        r
    }

    pub fn status(status: u16) -> Self { Self::new(status) }

    /// HttpResponseNotAllowed equivalent.
    pub fn not_allowed(allowed: &[Method]) -> Self {
        let list: Vec<&str> = allowed.iter().map(|m| m.as_str()).collect();
        let mut r = Self::with_body(405, "text/plain; charset=utf-8", format!("Method not allowed. Allowed: {}", list.join(", ")));
        r.headers.set("Allow", list.join(", "));
        r
    }

    pub fn body(&self) -> &[u8] { &self.body }

    pub fn body_string(&self) -> String {
        String::from_utf8_lossy(&self.body).into_owned()
    }

    pub fn set_body(&mut self, body: impl Into<Vec<u8>>) {
        self.body = body.into();
    }

    pub fn content_length(&self) -> usize { self.body.len() }

    pub fn set_cookie(&mut self, cookie: SetCookie) {
        self.cookies.push(cookie);
    }

    pub fn delete_cookie(&mut self, name: impl Into<String>, path: &str) {
        let mut c = SetCookie::expired(name);
        c.path = path.to_string();
        self.cookies.push(c);
    }

    /// Extract a single Set-Cookie value for a name (test helper).
    pub fn cookie_value(&self, name: &str) -> Option<String> {
        self.cookies
            .iter()
            .find(|c| c.name == name)
            .map(|c| c.value.clone())
    }
}

impl Default for HttpResponse {
    fn default() -> Self { Self::new(200) }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn querydict_roundtrip() {
        let qd = QueryDict::parse("a=1&b=hello+world&b=second&name=a%20b");
        assert_eq!(qd.get("a"), Some("1"));
        assert_eq!(qd.get("b"), Some("second"));
        assert_eq!(qd.getlist("b"), vec!["hello world", "second"]);
        assert_eq!(qd.get("name"), Some("a b"));
        let encoded = qd.encode();
        let reparsed = QueryDict::parse(&encoded);
        assert_eq!(reparsed.getlist("b").len(), 2);
    }

    #[test]
    fn querydict_add_get() {
        let mut qd = QueryDict::new();
        qd.add("tag", "rust");
        qd.add("tag", "django");
        qd.set("only", "one");
        assert_eq!(qd.getlist("tag"), vec!["rust", "django"]);
        assert!(qd.contains("only"));
        assert!(!qd.contains("nope"));
    }

    #[test]
    fn header_case_insensitive() {
        let mut h = Headers::new();
        h.set("Content-Type", "text/html");
        assert_eq!(h.get("content-type"), Some("text/html"));
        assert_eq!(h.get("CONTENT-TYPE"), Some("text/html"));
        h.set("content-type", "application/json");
        assert_eq!(h.get("Content-Type"), Some("application/json"));
        assert_eq!(h.iter().count(), 1);
    }

    #[test]
    fn cookies() {
        let c = SetCookie::new("sessionid", "abc123").max_age(3600).http_only().same_site("Lax");
        let header = c.to_header();
        assert!(header.starts_with("sessionid=abc123"));
        assert!(header.contains("Max-Age=3600"));
        assert!(header.contains("HttpOnly"));
        assert!(header.contains("SameSite=Lax"));

        let expired = SetCookie::expired("csrftoken");
        assert!(expired.to_header().contains("Max-Age=0"));
    }

    #[test]
    fn multiple_cookie_headers_are_joined() {
        let mut h = Headers::new();
        h.add("Cookie", "a=1");
        h.add("Cookie", "b=2");
        assert_eq!(h.get_all("cookie"), vec!["a=1", "b=2"]);
    }

    #[test]
    fn request_basics() {
        let mut req = HttpRequest::new(Method::GET, "/polls/3/?page=2&tag=a&tag=b");
        assert_eq!(req.path, "/polls/3/");
        assert_eq!(req.query.get("page"), Some("2"));
        assert_eq!(req.query.getlist("tag"), vec!["a", "b"]);
        assert_eq!(req.method, Method::GET);

        req.params.insert("pk".into(), "3".into());
        assert_eq!(req.int_param("pk").unwrap(), 3);
        assert!(req.int_param("missing").is_err());

        let full = req.build_absolute_uri("/next/");
        assert_eq!(full, "http://localhost/next/");
    }

    #[test]
    fn methods() {
        assert!(Method::GET.is_safe());
        assert!(!Method::POST.is_safe());
        assert_eq!(Method::parse("post"), Some(Method::POST));
        assert_eq!(Method::parse("bogus"), None);
    }

    #[test]
    fn response_helpers() {
        let r = HttpResponse::html("<h1>hi</h1>");
        assert_eq!(r.status, 200);
        assert_eq!(r.headers.get("Content-Type"), Some("text/html; charset=utf-8"));

        let r = HttpResponse::json(&serde_json::json!({"ok": true}));
        assert!(r.body_string().contains("\"ok\":true"));

        let r = HttpResponse::redirect("/next/");
        assert_eq!(r.status, 302);
        assert_eq!(r.headers.get("Location"), Some("/next/"));

        let r = HttpResponse::not_allowed(&[Method::POST]);
        assert_eq!(r.status, 405);
        assert_eq!(r.headers.get("Allow"), Some("POST"));
    }

    #[test]
    fn cookie_header_parse() {
        let jar = parse_cookie_header("sessionid=abc; csrftoken=x%20y; empty=");
        assert_eq!(jar.get("sessionid").map(String::as_str), Some("abc"));
        assert_eq!(jar.get("csrftoken").map(String::as_str), Some("x y"));
        assert_eq!(jar.get("empty").map(String::as_str), Some(""));
    }
}
