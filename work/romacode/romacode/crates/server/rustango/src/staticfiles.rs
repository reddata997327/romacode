//! Static files — `django.contrib.staticfiles`: serve from
//! `STATICFILES_DIRS` under `STATIC_URL` with path-traversal protection.

use std::path::PathBuf;

use crate::errors::HttpError;
use crate::http::{HttpRequest, HttpResponse};

/// Resolve a static asset path across the configured dirs.
pub fn find_asset(dirs: &[PathBuf], rel: &str) -> Option<(Vec<u8>, String)> {
    // Reject traversal attempts.
    if rel.contains("..") || rel.starts_with('/') || rel.contains('\\') {
        return None;
    }
    for dir in dirs {
        let path = dir.join(rel);
        if let Ok(meta) = std::fs::metadata(&path) {
            if meta.is_file() {
                if let Ok(bytes) = std::fs::read(&path) {
                    return Some((bytes, content_type(rel)));
                }
            }
        }
    }
    None
}

/// Content-Type by extension (the common web set).
pub fn content_type(name: &str) -> String {
    let ext = name.rsplit('.').next().unwrap_or("").to_lowercase();
    match ext.as_str() {
        "css" => "text/css; charset=utf-8",
        "js" | "mjs" => "application/javascript; charset=utf-8",
        "json" => "application/json",
        "html" => "text/html; charset=utf-8",
        "txt" | "md" => "text/plain; charset=utf-8",
        "png" => "image/png",
        "jpg" | "jpeg" => "image/jpeg",
        "gif" => "image/gif",
        "svg" => "image/svg+xml",
        "ico" => "image/x-icon",
        "woff" => "font/woff",
        "woff2" => "font/woff2",
        "ttf" => "font/ttf",
        "pdf" => "application/pdf",
        "webp" => "image/webp",
        "mp4" => "video/mp4",
        "wasm" => "application/wasm",
        _ => "application/octet-stream",
    }
    .to_string()
}

/// Serve `/static/<path>` — install this as a route on
/// `format!("{}<path:asset>", settings.static_url)`.
pub fn serve(req: &mut HttpRequest) -> Result<HttpResponse, HttpError> {
    let dirs = crate::settings::global().staticfiles_dirs.clone();
    serve_with_dirs(req, &dirs)
}

/// Serve from explicit dirs (the settings-driven entry point calls this).
pub fn serve_with_dirs(
    req: &mut HttpRequest,
    dirs: &[PathBuf],
) -> Result<HttpResponse, HttpError> {
    if !req.method.is_safe() {
        return Err(HttpError::MethodNotAllowed("static files are read-only".into()));
    }
    let rel = req
        .param("asset")
        .ok_or_else(|| HttpError::NotFound("missing asset path".into()))?;
    match find_asset(dirs, rel) {
        Some((bytes, ct)) => {
            let mut res = HttpResponse::with_body(200, &ct, bytes);
            res.headers.set("Cache-Control", "public, max-age=3600");
            Ok(res)
        }
        None => Err(HttpError::NotFound(format!("static asset `{rel}`"))),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn content_types() {
        assert_eq!(content_type("a.css"), "text/css; charset=utf-8");
        assert_eq!(content_type("app.JS"), "application/javascript; charset=utf-8");
        assert_eq!(content_type("logo.SVG"), "image/svg+xml");
        assert_eq!(content_type("unknown.bin"), "application/octet-stream");
    }

    #[test]
    fn traversal_rejected() {
        let dirs = vec![PathBuf::from("/tmp/rustango-static-test")];
        assert!(find_asset(&dirs, "../etc/passwd").is_none());
        assert!(find_asset(&dirs, "/etc/passwd").is_none());
        assert!(find_asset(&dirs, "a\\..\\b").is_none());
    }

    #[test]
    fn finds_file_in_dir() {
        let dir = std::env::temp_dir().join(format!("rustango-static-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("style.css"), b"body{}").unwrap();
        let dirs = vec![dir.clone()];
        let (bytes, ct) = find_asset(&dirs, "style.css").unwrap();
        assert_eq!(bytes, b"body{}");
        assert!(ct.starts_with("text/css"));
        assert!(find_asset(&dirs, "missing.css").is_none());
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn serve_route_integration() {
        let dir = std::env::temp_dir().join(format!("rustango-static-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("app.js"), b"console.log(1)").unwrap();
        let dirs = vec![dir.clone()];
        let mut req = crate::http::HttpRequest::new(crate::http::Method::GET, "/static/app.js");
        req.params.insert("asset".into(), "app.js".into());
        let res = serve_with_dirs(&mut req, &dirs).unwrap();
        assert_eq!(res.status, 200);
        assert_eq!(res.body_string(), "console.log(1)");

        let mut missing = crate::http::HttpRequest::new(crate::http::Method::GET, "/static/x.js");
        missing.params.insert("asset".into(), "x.js".into());
        assert_eq!(serve_with_dirs(&mut missing, &dirs).unwrap_err().status(), 404);
        std::fs::remove_dir_all(&dir).ok();
    }
}
