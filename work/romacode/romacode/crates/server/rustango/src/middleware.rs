//! Middleware — `django.middleware`.
//!
//! The stack runs `process_request` top-down, then the view, then
//! `process_response` bottom-up — WSGI-style, exactly like Django's
//! `MIDDLEWARE` setting order.

use crate::errors::HttpError;
use crate::http::{HttpRequest, HttpResponse};
use crate::views::ViewFn;
use std::sync::Arc;

/// What a request-phase middleware can do: continue down the chain or
/// short-circuit with a response (like returning a response from
/// `process_request`).
#[derive(Debug)]
pub enum Next {
    Continue,
    ShortCircuit(HttpResponse),
}

/// A middleware component.
pub trait Middleware: Send + Sync {
    fn process_request(&self, _req: &mut HttpRequest) -> Next {
        Next::Continue
    }

    /// Return Ok(()) to proceed to the view; Err to abort (CSRF etc.).
    fn process_view(&self, _req: &HttpRequest) -> Result<(), HttpError> {
        Ok(())
    }

    fn process_response(&self, _req: &mut HttpRequest, _res: &mut HttpResponse) {}
}

/// The assembled chain: middlewares + the terminal view.
pub struct MiddlewareStack {
    pub middlewares: Vec<Arc<dyn Middleware>>,
}

impl MiddlewareStack {
    pub fn new(middlewares: Vec<Arc<dyn Middleware>>) -> Self {
        Self { middlewares }
    }

    /// Run the full pipeline for one request.
    pub fn dispatch(
        &self,
        req: &mut HttpRequest,
        view: &ViewFn,
    ) -> Result<HttpResponse, HttpError> {
        // process_request top-down; short-circuits skip the view.
        for (i, mw) in self.middlewares.iter().enumerate() {
            if let Next::ShortCircuit(res) = mw.process_request(req) {
                let mut res = res;
                // Django semantics: the response phase only runs for the
                // middlewares up to and including the one that produced the
                // response (the ones below never saw the request).
                self.run_response_phase(req, &mut res, Some(&self.middlewares[..=i]));
                return Ok(res);
            }
        }

        // process_view phase (CSRF etc.)
        for mw in &self.middlewares {
            mw.process_view(req)?;
        }

        let mut result = view(req);

        if let Ok(res) = &mut result {
            self.run_response_phase(req, res, None);
        }
        result
    }

    /// Run only the response phase (used for error responses so cookies
    /// like CSRF/session still get set — Django does the same).
    pub fn run_response(&self, req: &mut HttpRequest, res: &mut HttpResponse) {
        self.run_response_phase(req, res, None);
    }

    fn run_response_phase(
        &self,
        req: &mut HttpRequest,
        res: &mut HttpResponse,
        up_to: Option<&[Arc<dyn Middleware>]>,
    ) {
        let list: &[Arc<dyn Middleware>] = up_to.unwrap_or(&self.middlewares);
        for mw in list.iter().rev() {
            mw.process_response(req, res);
        }
    }
}

// ---------------------------------------------------------------------------
// Built-in middlewares
// ---------------------------------------------------------------------------

/// `SecurityMiddleware` — sets hardened security headers.
pub struct SecurityMiddleware;

impl Middleware for SecurityMiddleware {
    fn process_response(&self, _req: &mut HttpRequest, res: &mut HttpResponse) {
        res.headers
            .set("X-Content-Type-Options", "nosniff");
        res.headers.set("Referrer-Policy", "same-origin");
        res.headers.set(
            "Strict-Transport-Security",
            "max-age=31536000; includeSubDomains",
        );
    }
}

/// `ClickjackingMiddleware` — X-Frame-Options (default DENY, like Django).
pub struct ClickjackingMiddleware {
    pub value: String,
}

impl Middleware for ClickjackingMiddleware {
    fn process_response(&self, _req: &mut HttpRequest, res: &mut HttpResponse) {
        if !res.headers.get("X-Frame-Options").is_some() {
            res.headers.set("X-Frame-Options", self.value.clone());
        }
    }
}

/// `GZipMiddleware` — compress large text responses (miniz_oxide, pure Rust).
pub struct GZipMiddleware {
    pub min_length: usize,
}

impl Middleware for GZipMiddleware {
    fn process_response(&self, req: &mut HttpRequest, res: &mut HttpResponse) {
        let accepts = req
            .headers
            .get("accept-encoding")
            .map(|v| v.to_lowercase().contains("gzip"))
            .unwrap_or(false);
        if !accepts {
            return;
        }
        if res.content_length() < self.min_length {
            return;
        }
        let compressible = res
            .headers
            .get("Content-Type")
            .map(|t| {
                t.starts_with("text/")
                    || t.contains("json")
                    || t.contains("javascript")
                    || t.contains("xml")
            })
            .unwrap_or(false);
        if !compressible {
            return;
        }
        let mut encoder = flate2::write::GzEncoder::new(
            Vec::with_capacity(res.content_length() / 2),
            flate2::Compression::default(),
        );
        if std::io::Write::write_all(&mut encoder, res.body()).is_ok() {
            if let Ok(compressed) = encoder.finish() {
                res.set_body(compressed);
                res.headers.set("Content-Encoding", "gzip");
                res.headers.remove("Content-Length");
            }
        }
    }
}

/// `CommonMiddleware` — APPEND_SLASH: redirect `/missing` to `/missing/`
/// when the latter resolves, and set Content-Length.
pub struct CommonMiddleware {
    pub router: Arc<crate::urls::Router>,
    pub append_slash: bool,
}

impl Middleware for CommonMiddleware {
    fn process_request(&self, req: &mut HttpRequest) -> Next {
        if self.append_slash
            && !req.path.ends_with('/')
            && req.method.is_safe()
        {
            let with_slash = format!("{}/", req.path);
            if self.router.matches(&with_slash) {
                // Preserve the query string (`/search?q=rust` must redirect
                // to `/search/?q=rust`, not lose `?q=rust` — the old code
                // dropped it, and browsers cached the broken target).
                // Django issues a permanent (301) redirect for APPEND_SLASH;
                // with the query preserved the redirect is correct, so the
                // permanent status is safe to keep.
                let target = match req.full_path.split_once('?') {
                    Some((_, qs)) => format!("{with_slash}?{qs}"),
                    None => with_slash,
                };
                return Next::ShortCircuit(HttpResponse::redirect_permanent(target));
            }
        }
        Next::Continue
    }

    fn process_response(&self, _req: &mut HttpRequest, res: &mut HttpResponse) {
        res.headers
            .set("Content-Length", res.content_length().to_string());
    }
}

/// `SessionMiddleware` — loads `request.session` from the cookie and
/// persists it after the view.
pub struct SessionMiddleware {
    pub cookie_name: String,
    pub cookie_age: i64,
    pub backend: Arc<dyn crate::orm::Backend>,
}

impl Middleware for SessionMiddleware {
    fn process_request(&self, req: &mut HttpRequest) -> Next {
        let key = req.cookie(&self.cookie_name).map(|s| s.to_string());
        req.session = Some(crate::sessions::Session::load(
            self.backend.clone(),
            key.as_deref(),
            self.cookie_age,
        ));
        Next::Continue
    }

    fn process_response(&self, req: &mut HttpRequest, res: &mut HttpResponse) {
        crate::messages::persist(req);
        // Django only writes the session when it was modified during the
        // request — never for untouched anonymous sessions.
        if let Some(session) = req.session.as_mut() {
            if session.modified {
                session.save();
            }
        }
        crate::auth::apply_session_cookie(req, res, &self.cookie_name, self.cookie_age);
    }
}

/// `AuthMiddleware` — resolves `request.user` from the session.
pub struct AuthMiddleware;

impl Middleware for AuthMiddleware {
    fn process_request(&self, req: &mut HttpRequest) -> Next {
        crate::auth::get_user(req);
        Next::Continue
    }
}

/// `MessagesMiddleware` — loads flash messages into the request and
/// persists new ones after the view.
pub struct MessagesMiddleware;

impl Middleware for MessagesMiddleware {
    fn process_request(&self, req: &mut HttpRequest) -> Next {
        crate::messages::load(req);
        Next::Continue
    }

    fn process_response(&self, _req: &mut HttpRequest, _res: &mut HttpResponse) {
        // messages::add_message already persists to the session on write
        // (pop-on-read semantics), so nothing to do here.
    }
}

/// Build the default middleware stack, mirroring Django's default order:
/// Security -> Session -> Common -> Csrf -> Auth -> Messages ->
/// Clickjacking -> GZip.
pub fn default_stack(
    settings: &crate::settings::Settings,
    backend: Arc<dyn crate::orm::Backend>,
    router: Arc<crate::urls::Router>,
) -> Vec<Arc<dyn Middleware>> {
    let mut stack: Vec<Arc<dyn Middleware>> = Vec::new();
    if settings.use_security {
        stack.push(Arc::new(SecurityMiddleware));
    }
    stack.push(Arc::new(SessionMiddleware {
        cookie_name: settings.session_cookie_name.clone(),
        cookie_age: settings.session_cookie_age,
        backend: backend.clone(),
    }));
    stack.push(Arc::new(CommonMiddleware {
        router: router.clone(),
        append_slash: true,
    }));
    if settings.csrf_enabled {
        stack.push(Arc::new(crate::csrf::CsrfMiddleware {
            cookie_name: settings.csrf_cookie_name.clone(),
        }));
    }
    stack.push(Arc::new(AuthMiddleware));
    stack.push(Arc::new(MessagesMiddleware));
    if settings.use_clickjacking {
        stack.push(Arc::new(ClickjackingMiddleware {
            value: settings.x_frame_options.clone(),
        }));
    }
    if settings.use_gzip {
        stack.push(Arc::new(GZipMiddleware {
            min_length: settings.gzip_min_length,
        }));
    }
    stack
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::http::Method;
    use crate::orm::InMemoryBackend;
    use crate::views::{make_view, ViewFn};
    use std::sync::Arc as StdArc;

    fn ok_view() -> ViewFn {
        make_view(|req| {
            Ok(HttpResponse::html(format!("hello {}", req.user.username())))
        })
    }

    #[test]
    fn stack_request_and_response_order() {
        struct Add(String);
        impl Middleware for Add {
            fn process_request(&self, req: &mut HttpRequest) -> Next {
                let mut m = req.params.clone();
                m.insert("trace".into(), self.0.clone());
                req.params = m;
                Next::Continue
            }
            fn process_response(&self, _req: &mut HttpRequest, res: &mut HttpResponse) {
                let body = res.body_string();
                res.set_body(format!("{}<{}", body, self.0));
            }
        }

        let stack = MiddlewareStack::new(vec![
            StdArc::new(Add("A".into())),
            StdArc::new(Add("B".into())),
        ]);
        let mut req = HttpRequest::new(Method::GET, "/");
        let res = stack.dispatch(&mut req, &ok_view()).unwrap();
        // request phase top-down (B overwrites A), response phase bottom-up
        assert!(res.body_string().contains("<B<A"));
    }

    #[test]
    fn short_circuit_skips_view() {
        struct Block;
        impl Middleware for Block {
            fn process_request(&self, _req: &mut HttpRequest) -> Next {
                Next::ShortCircuit(HttpResponse::text("blocked"))
            }
        }
        let stack = MiddlewareStack::new(vec![StdArc::new(Block)]);
        let mut req = HttpRequest::new(Method::GET, "/");
        let res = stack.dispatch(&mut req, &ok_view()).unwrap();
        assert_eq!(res.body_string(), "blocked");
    }

    #[test]
    fn process_view_can_abort() {
        struct Deny;
        impl Middleware for Deny {
            fn process_view(&self, _req: &HttpRequest) -> Result<(), HttpError> {
                Err(HttpError::PermissionDenied("no".into()))
            }
        }
        let stack = MiddlewareStack::new(vec![StdArc::new(Deny)]);
        let mut req = HttpRequest::new(Method::GET, "/");
        let err = stack.dispatch(&mut req, &ok_view()).unwrap_err();
        assert_eq!(err.status(), 403);
    }

    #[test]
    fn append_slash_redirect() {
        use crate::urls::{path, Router};
        let view = make_view(|_req| Ok(HttpResponse::text("")));
        let router = StdArc::new(Router::new(vec![path("polls/", view)]));
        let mw = CommonMiddleware { router, append_slash: true };
        let mut req = HttpRequest::new(Method::GET, "/polls");
        match mw.process_request(&mut req) {
            Next::ShortCircuit(res) => {
                // Django semantics: permanent (301) redirect, now with the
                // query string preserved so the cached target is correct.
                assert_eq!(res.status, 301);
                assert_eq!(res.headers.get("Location"), Some("/polls/"));
            }
            _ => panic!("expected redirect"),
        }
    }

    #[test]
    fn append_slash_preserves_query_string() {
        use crate::urls::{path, Router};
        // Regression: `/search?q=rust` used to redirect to bare `/search/`,
        // dropping the query string; browsers then cached the broken target.
        let view = make_view(|_req| Ok(HttpResponse::text("")));
        let router = StdArc::new(Router::new(vec![path("search/", view)]));
        let mw = CommonMiddleware { router, append_slash: true };
        let mut req = HttpRequest::new(Method::GET, "/search?q=rust");
        match mw.process_request(&mut req) {
            Next::ShortCircuit(res) => {
                assert_eq!(res.status, 301);
                assert_eq!(res.headers.get("Location"), Some("/search/?q=rust"));
            }
            _ => panic!("expected redirect"),
        }
        // No query string -> no `?` appended.
        let mut req = HttpRequest::new(Method::GET, "/search");
        match mw.process_request(&mut req) {
            Next::ShortCircuit(res) => {
                assert_eq!(res.headers.get("Location"), Some("/search/"));
            }
            _ => panic!("expected redirect"),
        }
    }

    #[test]
    fn gzip_compresses_large_text() {
        let mw = GZipMiddleware { min_length: 100 };
        let mut req = HttpRequest::new(Method::GET, "/");
        req.headers.set("Accept-Encoding", "gzip, deflate");
        let mut res = HttpResponse::html("x".repeat(1000));
        mw.process_response(&mut req, &mut res);
        assert_eq!(res.headers.get("Content-Encoding"), Some("gzip"));
        assert!(res.content_length() < 1000);
    }

    #[test]
    fn security_headers() {
        let mw = SecurityMiddleware;
        let mut req = HttpRequest::new(Method::GET, "/");
        let mut res = HttpResponse::text("x");
        mw.process_response(&mut req, &mut res);
        assert_eq!(res.headers.get("X-Content-Type-Options"), Some("nosniff"));
        let cj = ClickjackingMiddleware { value: "DENY".into() };
        cj.process_response(&mut req, &mut res);
        assert_eq!(res.headers.get("X-Frame-Options"), Some("DENY"));
    }

    #[test]
    fn session_middleware_load_and_persist() {
        let backend: StdArc<dyn crate::orm::Backend> = StdArc::new(InMemoryBackend::new());
        let mw = SessionMiddleware {
            cookie_name: "sessionid".into(),
            cookie_age: 3600,
            backend: backend.clone(),
        };
        let mut req = HttpRequest::new(Method::GET, "/");
        assert!(matches!(mw.process_request(&mut req), Next::Continue));
        assert!(req.session.is_some());
        // write something and persist via response phase
        req.session.as_mut().unwrap().set("k", serde_json::json!(1));
        let mut res = HttpResponse::text("ok");
        mw.process_response(&mut req, &mut res);
        assert!(res.cookie_value("sessionid").is_some());
        // new request with cookie sees data
        let key = res.cookie_value("sessionid").unwrap();
        let mut req2 = HttpRequest::new(Method::GET, "/");
        req2.cookies.insert("sessionid".into(), key);
        mw.process_request(&mut req2);
        assert_eq!(req2.session.as_ref().unwrap().get_int("k"), Some(1));
    }

    #[test]
    fn untouched_session_is_not_persisted() {
        // Django does not create a session row (or send a cookie) for a
        // request that never touches request.session.
        let backend: StdArc<dyn crate::orm::Backend> = StdArc::new(InMemoryBackend::new());
        let mw = SessionMiddleware {
            cookie_name: "sessionid".into(),
            cookie_age: 3600,
            backend: backend.clone(),
        };
        let mut req = HttpRequest::new(Method::GET, "/");
        mw.process_request(&mut req);
        let mut res = HttpResponse::text("ok");
        mw.process_response(&mut req, &mut res);
        assert!(res.cookie_value("sessionid").is_none());
        assert_eq!(backend.count("django_session", &crate::orm::Criteria::any()).unwrap(), 0);
    }
}
