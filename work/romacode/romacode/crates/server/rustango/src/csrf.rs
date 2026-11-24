//! CSRF protection — `django.middleware.csrf`.
//!
//! Django's double-submit pattern: a random `csrftoken` cookie is paired
//! with a hidden form field (`csrfmiddlewaretoken`) or an `X-CSRFToken`
//! header. Unsafe methods (POST/PUT/DELETE) must present a matching token.
//! Routes can opt out with `Route::csrf_exempt()` (like `@csrf_exempt`).

use crate::http::{HttpRequest, HttpResponse, SetCookie};

/// Request flag set by the router when the matched route is exempt.
pub(crate) const EXEMPT_FLAG: &str = "__csrf_exempt";

/// Generate a fresh CSRF token (64 hex chars, like Django's 32 bytes).
pub fn new_token() -> String {
    format!(
        "{}{}",
        uuid::Uuid::new_v4().simple(),
        uuid::Uuid::new_v4().simple()
    )
    [..64]
    .to_string()
}

/// Session-data key holding the per-session CSRF secret (128-bit random hex,
/// generated on first use and persisted with the session; never sent raw to
/// the client).
const SECRET_KEY: &str = "_csrf_secret";
/// Request-params key holding this request's CSRF secret when no session
/// exists (a fresh ephemeral secret, kept server-side in the request).
const SECRET_FLAG: &str = "__csrf_secret";

/// The server-side secret this request's tokens are derived from.
fn request_secret(req: &mut HttpRequest) -> String {
    if let Some(session) = req.session.as_mut() {
        if session.get(SECRET_KEY).and_then(|v| v.as_str()).is_none() {
            session.set(
                SECRET_KEY,
                serde_json::Value::String(uuid::Uuid::new_v4().simple().to_string()),
            );
        }
        if let Some(secret) = session.get(SECRET_KEY).and_then(|v| v.as_str()) {
            return secret.to_string();
        }
    }
    if let Some(secret) = req.params.get(SECRET_FLAG) {
        return secret.clone();
    }
    let secret = uuid::Uuid::new_v4().simple().to_string();
    req.params.insert(SECRET_FLAG.into(), secret.clone());
    secret
}

/// Expected CSRF token: `sha256(secret || session_key)` hex. It depends on a
/// server-side secret rather than being equal to any stored or sent value,
/// so a cookie-injection primitive alone cannot mint a valid token.
pub fn derive_token(secret: &str, session_key: &str) -> String {
    use sha2::{Digest, Sha256};
    let mut hasher = Sha256::new();
    hasher.update(secret.as_bytes());
    hasher.update(session_key.as_bytes());
    let digest = hasher.finalize();
    digest.iter().map(|b| format!("{b:02x}")).collect()
}

/// Ensure the CSRF secret exists and expose the derived token to the
/// view/template.
pub fn process_request(req: &mut HttpRequest, cookie_name: &str) {
    let secret = request_secret(req);
    let session_key = req
        .session
        .as_ref()
        .map(|s| s.key.clone())
        .unwrap_or_default();
    let _ = cookie_name;
    req.csrf_token = Some(derive_token(&secret, &session_key));
}

/// Verify unsafe requests; returns an error message when rejected.
pub fn process_view(req: &HttpRequest) -> Result<(), String> {
    if req.method.is_safe() {
        return Ok(());
    }
    // Route-level exemption (`Route::csrf_exempt()`), set by the dispatcher.
    if req.params.get(EXEMPT_FLAG).map(|v| v == "1").unwrap_or(false) {
        return Ok(());
    }
    // The expected token is DERIVED from the server-side secret — never read
    // back from the cookie (the old pure double-submit scheme made the
    // cookie the trust anchor, so any cookie-injection primitive bypassed
    // it).
    let secret = req.params.get(SECRET_FLAG).cloned().or_else(|| {
        req.session
            .as_ref()
            .and_then(|s| s.get(SECRET_KEY))
            .and_then(|v| v.as_str().map(|s| s.to_string()))
    });
    let session_key = req
        .session
        .as_ref()
        .map(|s| s.key.clone())
        .unwrap_or_default();
    let expected = match secret {
        Some(secret) => derive_token(&secret, &session_key),
        None => return Err("CSRF cookie not set.".into()),
    };
    let presented = req
        .post
        .get("csrfmiddlewaretoken")
        .map(|s| s.to_string())
        .or_else(|| {
            req.headers
                .get("x-csrftoken")
                .map(|s| s.to_string())
        })
        .unwrap_or_default();
    if presented.is_empty() {
        return Err("CSRF token missing.".into());
    }
    if !tokens_match(&expected, &presented) {
        return Err("CSRF token has incorrect value.".into());
    }
    Ok(())
}

fn tokens_match(a: &str, b: &str) -> bool {
    // Django allows the token with rotated secret prefixes; for our
    // server-side scheme an exact match suffices.
    a.len() == b.len()
        && a.bytes()
            .zip(b.bytes())
            .fold(0u8, |acc, (x, y)| acc | (x ^ y))
            == 0
}

/// Middleware instance for the stack.
pub struct CsrfMiddleware {
    pub cookie_name: String,
}

impl crate::middleware::Middleware for CsrfMiddleware {
    fn process_request(&self, req: &mut HttpRequest) -> crate::middleware::Next {
        process_request(req, &self.cookie_name);
        crate::middleware::Next::Continue
    }

    fn process_view(&self, req: &HttpRequest) -> Result<(), crate::errors::HttpError> {
        match process_view(req) {
            Ok(()) => Ok(()),
            Err(reason) => Err(crate::errors::HttpError::CsrfFailure(reason)),
        }
    }

    fn process_response(
        &self,
        req: &mut HttpRequest,
        res: &mut HttpResponse,
    ) {
        let token = req.csrf_token.clone();
        if let Some(token) = token {
            let present = res.cookies.iter().any(|c| c.name == self.cookie_name);
            if !present {
                res.set_cookie(
                    SetCookie::new(self.cookie_name.clone(), token.clone())
                        .same_site("Lax"),
                );
            }
        }
    }
}

/// Mark a route as exempt from CSRF verification — `@csrf_exempt`.
///
/// ```ignore
/// use rustango::urls::path;
///
/// let route = csrf_exempt(path("webhook/", receiver_view));
/// ```
pub fn csrf_exempt(route: crate::urls::Route) -> crate::urls::Route {
    route.csrf_exempt()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::http::Method;
    use crate::middleware::Middleware as _;

    #[test]
    fn token_format() {
        let t = new_token();
        assert_eq!(t.len(), 64);
        assert!(t.chars().all(|c| c.is_ascii_hexdigit()));
    }

    #[test]
    fn safe_methods_pass() {
        let mut req = HttpRequest::new(Method::GET, "/");
        process_request(&mut req, "csrftoken");
        assert!(process_view(&req).is_ok());
    }

    #[test]
    fn unsafe_without_token_rejected() {
        let mut req = HttpRequest::new(Method::POST, "/");
        process_request(&mut req, "csrftoken");
        let mut data = crate::http::QueryDict::new();
        data.set("x", "1");
        req.post = data;
        assert!(process_view(&req).is_err());
    }

    #[test]
    fn form_token_matches() {
        let mut req = HttpRequest::new(Method::POST, "/");
        process_request(&mut req, "csrftoken");
        let token = req.csrf_token.clone().unwrap();
        let mut data = crate::http::QueryDict::new();
        data.set("csrfmiddlewaretoken", token);
        req.post = data;
        assert!(process_view(&req).is_ok());
    }

    #[test]
    fn header_token_matches() {
        let mut req = HttpRequest::new(Method::POST, "/");
        process_request(&mut req, "csrftoken");
        let token = req.csrf_token.clone().unwrap();
        req.headers.set("X-CSRFToken", token);
        assert!(process_view(&req).is_ok());
    }

    #[test]
    fn wrong_token_rejected() {
        let mut req = HttpRequest::new(Method::POST, "/");
        process_request(&mut req, "csrftoken");
        let mut data = crate::http::QueryDict::new();
        data.set("csrfmiddlewaretoken", "forged");
        req.post = data;
        assert!(process_view(&req).is_err());
    }

    #[test]
    fn exempt_route_is_skipped() {
        let mut req = HttpRequest::new(Method::POST, "/webhook/");
        process_request(&mut req, "csrftoken");
        req.params.insert(EXEMPT_FLAG.into(), "1".into());
        assert!(process_view(&req).is_ok());
    }

    #[test]
    fn middleware_sets_cookie_on_response() {
        let mw = CsrfMiddleware { cookie_name: "csrftoken".into() };
        let mut req = HttpRequest::new(Method::GET, "/");
        mw.process_request(&mut req);
        let mut res = HttpResponse::text("ok");
        mw.process_response(&mut req, &mut res);
        assert_eq!(res.cookie_value("csrftoken"), req.csrf_token);
    }
}
