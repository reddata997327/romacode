//! HTTP server glue — tokio + axum at the edge, sync framework core on
//! blocking worker threads (WSGI-style thread-per-request).

use std::net::SocketAddr;
use std::sync::Arc;

use axum::extract::Request as AxumRequest;
use axum::response::Response as AxumResponse;
use tokio::net::TcpListener;

use crate::http::{HttpRequest, HttpResponse, Method};
use crate::project::Project;

/// Verbs outside the known set must be rejected with 405. The old
/// `unwrap_or(Method::GET)` downgraded unknown methods (PROPFIND, FOO, …)
/// to GET, which `is_safe()` treats as CSRF-exempt and routes to
/// `View::get()` — an attacker-controlled verb silently became a safe GET.
fn resolve_method(raw: &str) -> Result<Method, HttpResponse> {
    Method::parse(raw).ok_or_else(|| {
        let mut res = HttpResponse::with_body(
            405,
            "text/plain; charset=utf-8",
            format!("Method `{raw}` not allowed"),
        );
        res.headers
            .set("Allow", "GET, HEAD, POST, PUT, PATCH, DELETE, OPTIONS, TRACE");
        res
    })
}

/// Convert an axum request into a rustango `HttpRequest`. Unknown verbs
/// short-circuit into a 405 response instead of becoming `Method::GET`.
async fn to_internal(req: AxumRequest) -> Result<HttpRequest, HttpResponse> {
    let method = resolve_method(req.method().as_str())?;
    let path = req.uri().path().to_string();
    let query = req.uri().query().unwrap_or("").to_string();
    let full = if query.is_empty() {
        path.clone()
    } else {
        format!("{path}?{query}")
    };
    let mut internal = HttpRequest::new(method, full);
    for (k, v) in req.headers() {
        if let Ok(v) = v.to_str() {
            internal.headers.add(k.as_str().to_string(), v.to_string());
        }
    }
    // Join every Cookie header (clients/proxies may send several).
    let cookie_header = internal.headers.get_all("cookie").join("; ");
    if !cookie_header.is_empty() {
        internal.cookies = crate::http::parse_cookie_header(&cookie_header);
    }
    internal.remote_addr = req
        .extensions()
        .get::<std::net::SocketAddr>()
        .map(|a| a.to_string())
        .unwrap_or_else(|| "unknown".into());
    let body = axum::body::to_bytes(req.into_body(), 64 * 1024 * 1024)
        .await
        .unwrap_or_default();
    internal.body = body.to_vec();
    let content_type = internal.headers.get("content-type").unwrap_or("").to_string();
    if content_type.starts_with("application/x-www-form-urlencoded") {
        let text = String::from_utf8_lossy(&internal.body);
        internal.post = crate::http::QueryDict::parse(&text);
    } else if content_type.starts_with("application/json") {
        if let Ok(serde_json::Value::Object(map)) =
            serde_json::from_slice::<serde_json::Value>(&internal.body)
        {
            for (k, val) in map {
                let s = match val {
                    serde_json::Value::String(s) => s,
                    other => other.to_string(),
                };
                internal.post.set(k, s);
            }
        }
    }
    Ok(internal)
}

fn to_axum(res: HttpResponse) -> AxumResponse {
    let mut builder = AxumResponse::builder().status(res.status);
    for (k, v) in res.headers.iter() {
        builder = builder.header(k, v);
    }
    for cookie in &res.cookies {
        builder = builder.header("Set-Cookie", cookie.to_header());
    }
    builder
        .body(axum::body::Body::from(res.body().to_vec()))
        .unwrap_or_else(|_| AxumResponse::new("internal error".into()))
}

/// Serve the project on `addr`.
pub async fn serve(project: Arc<Project>, addr: SocketAddr) -> Result<(), String> {
    let listener = TcpListener::bind(addr)
        .await
        .map_err(|e| format!("bind {addr}: {e}"))?;
    println!(
        "rustango {} starting server at http://{} (DEBUG: {}) — press Ctrl-C to stop",
        crate::VERSION,
        addr,
        project.settings.debug
    );
    let app = axum::Router::new().fallback(move |req: AxumRequest| {
        let project = project.clone();
        async move {
            let internal = match to_internal(req).await {
                Ok(r) => r,
                Err(res) => return to_axum(res),
            };
            let res = tokio::task::spawn_blocking(move || {
                let mut r = internal;
                project.handle_request(&mut r)
            })
            .await
            .unwrap_or_else(|_| HttpResponse::status(500));
            to_axum(res)
        }
    });
    axum::serve(listener, app).await.map_err(|e| e.to_string())
}

/// Bind and run with graceful shutdown on Ctrl-C.
pub async fn serve_with_shutdown(project: Arc<Project>, addr: SocketAddr) -> Result<(), String> {
    let listener = TcpListener::bind(addr)
        .await
        .map_err(|e| format!("bind {addr}: {e}"))?;
    println!(
        "rustango {} serving http://{} (DEBUG: {})",
        crate::VERSION,
        addr,
        project.settings.debug
    );
    let app = axum::Router::new().fallback(move |req: AxumRequest| {
        let project = project.clone();
        async move {
            let internal = match to_internal(req).await {
                Ok(r) => r,
                Err(res) => return to_axum(res),
            };
            let res = tokio::task::spawn_blocking(move || {
                let mut r = internal;
                project.handle_request(&mut r)
            })
            .await
            .unwrap_or_else(|_| HttpResponse::status(500));
            to_axum(res)
        }
    });
    axum::serve(listener, app)
        .with_graceful_shutdown(shutdown_signal())
        .await
        .map_err(|e| e.to_string())
}

async fn shutdown_signal() {
    let _ = tokio::signal::ctrl_c().await;
    println!("\nshutting down...");
}

/// Build an axum Router from a project (for embedding rustango into a
/// larger axum application).
pub fn axum_router(project: Arc<Project>) -> axum::Router {
    axum::Router::new().fallback(move |req: AxumRequest| {
        let project = project.clone();
        async move {
            let internal = match to_internal(req).await {
                Ok(r) => r,
                Err(res) => return to_axum(res),
            };
            let res = tokio::task::spawn_blocking(move || {
                let mut r = internal;
                project.handle_request(&mut r)
            })
            .await
            .unwrap_or_else(|_| HttpResponse::status(500));
            to_axum(res)
        }
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn unknown_verbs_get_405_not_get() {
        // Regression: unknown verbs used to fall back to `Method::GET`, which
        // is_safe() exempts from CSRF and routes to `View::get()`.
        for verb in ["PROPFIND", "FOO", "BREW", "CONNECT"] {
            let err = resolve_method(verb).expect_err("unknown verb must be rejected");
            assert_eq!(err.status, 405, "verb {verb} must map to 405");
            assert!(err.headers.get("Allow").is_some());
        }
        // All known verbs still resolve (case-insensitive like `parse`).
        for (raw, expect) in [
            ("GET", Method::GET),
            ("get", Method::GET),
            ("POST", Method::POST),
            ("put", Method::PUT),
            ("PATCH", Method::PATCH),
            ("DELETE", Method::DELETE),
            ("HEAD", Method::HEAD),
            ("OPTIONS", Method::OPTIONS),
            ("TRACE", Method::TRACE),
        ] {
            assert_eq!(resolve_method(raw).unwrap(), expect, "verb {raw}");
        }
    }
}
