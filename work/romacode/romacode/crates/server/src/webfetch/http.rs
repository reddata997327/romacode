//! HTTP plumbing for `web_fetch` (std only): drives `curl` (already a RomaCode
//! requirement), parses the response and decodes the body.
//!
//! * the URL and every option go over curl's stdin as a config, so nothing
//!   appears in the process list;
//! * `-q` ignores any `~/.curlrc` so behaviour is the same everywhere;
//! * redirects are NOT followed by curl — the caller follows them one hop at a
//!   time so each hop can be policy- and SSRF-checked;
//! * the body is read through a hard byte cap and curl is killed beyond it.
use std::io::{Read, Write};
use std::process::{Command, Stdio};

pub struct CurlParams<'a> {
    pub url: &'a str,
    pub user_agent: &'a str,
    pub connect_timeout: u64,
    pub max_time: u64,
    /// Maximum body bytes kept; the rest is discarded and `truncated` is set.
    pub max_bytes: usize,
    /// `host:port:ip` pin so curl connects to the address that was vetted.
    pub pin: Option<&'a str>,
    pub proxy: &'a str,
    pub ca_bundle: &'a str,
    /// `user:pass` basic auth from URL userinfo, if the URL carried any.
    pub auth: Option<&'a str>,
}

#[derive(Debug)]
pub struct Response {
    pub status: u16,
    pub reason: String,
    pub headers: Vec<(String, String)>,
    pub body: Vec<u8>,
    pub truncated: bool,
}

impl Response {
    pub fn header(&self, name: &str) -> Option<&str> {
        self.headers
            .iter()
            .find(|(k, _)| k.eq_ignore_ascii_case(name))
            .map(|(_, v)| v.as_str())
    }

    /// Media type without parameters, lowercased (`text/html`).
    pub fn mime(&self) -> String {
        self.header("content-type")
            .map(|v| {
                v.split(';')
                    .next()
                    .unwrap_or("")
                    .trim()
                    .to_ascii_lowercase()
            })
            .unwrap_or_default()
    }

    pub fn is_redirect(&self) -> bool {
        matches!(self.status, 301 | 302 | 303 | 307 | 308) && self.header("location").is_some()
    }
}

fn q(v: &str) -> String {
    v.replace('\\', "\\\\")
        .replace('"', "\\\"")
        .replace(['\n', '\r'], "")
}

pub fn curl_config(p: &CurlParams) -> String {
    let mut c = format!(
        "url = \"{}\"\nrequest = \"GET\"\nuser-agent = \"{}\"\nconnect-timeout = {}\nmax-time = {}\n\
         proto = \"=http,https\"\ncompressed\n\
         header = \"Accept: text/html,application/xhtml+xml,application/json;q=0.9,text/plain;q=0.8,*/*;q=0.5\"\n\
         header = \"Accept-Language: en-US,en;q=0.8\"\n",
        q(p.url),
        q(p.user_agent),
        p.connect_timeout.max(1),
        p.max_time.max(1),
    );
    if let Some(pin) = p.pin {
        c.push_str(&format!("resolve = \"{}\"\n", q(pin)));
    }
    if let Some(auth) = p.auth {
        c.push_str(&format!("user = \"{}\"\n", q(auth)));
    }
    if !p.proxy.is_empty() {
        c.push_str(&format!("proxy = \"{}\"\n", q(p.proxy)));
    }
    if !p.ca_bundle.is_empty() {
        c.push_str(&format!("cacert = \"{}\"\n", q(p.ca_bundle)));
    }
    c
}

/// Split a raw `curl -i` stream into status, headers and body, skipping
/// interim (1xx) and proxy-CONNECT header blocks.
pub fn split_response(
    mut buf: &[u8],
) -> Result<(u16, String, Vec<(String, String)>, &[u8]), String> {
    loop {
        let end = buf
            .windows(4)
            .position(|w| w == b"\r\n\r\n")
            .ok_or("malformed response: no end of headers")?;
        let head = String::from_utf8_lossy(&buf[..end]).into_owned();
        let rest = &buf[end + 4..];
        let mut lines = head.split("\r\n");
        let status_line = lines.next().unwrap_or("");
        let mut parts = status_line.split_whitespace();
        let proto = parts.next().unwrap_or("");
        if !proto.starts_with("HTTP/") {
            return Err(format!("malformed response status line: {status_line:?}"));
        }
        let status: u16 = parts
            .next()
            .and_then(|s| s.parse().ok())
            .ok_or("malformed response status code")?;
        let reason = parts.collect::<Vec<_>>().join(" ");
        let connect_ack = reason
            .to_ascii_lowercase()
            .starts_with("connection established");
        if ((100..200).contains(&status) && status != 101) || connect_ack {
            buf = rest;
            continue;
        }
        let headers = lines
            .filter_map(|l| l.split_once(':'))
            .map(|(k, v)| (k.trim().to_string(), v.trim().to_string()))
            .collect();
        return Ok((status, reason, headers, rest));
    }
}

pub fn curl_get(p: &CurlParams) -> Result<Response, String> {
    let mut child = Command::new("curl")
        .args([
            "-q",
            "-sS",
            "-i",
            "--suppress-connect-headers",
            "--config",
            "-",
        ])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|e| format!("could not run curl ({e}) — install curl to use web_fetch"))?;
    {
        let mut stdin = child.stdin.take().ok_or("curl stdin unavailable")?;
        stdin
            .write_all(curl_config(p).as_bytes())
            .map_err(|e| format!("curl: {e}"))?;
    }
    let stdout = child.stdout.take().ok_or("curl stdout unavailable")?;
    let cap = p.max_bytes + 64 * 1024; // room for headers
    let mut buf = Vec::new();
    stdout
        .take(cap as u64 + 1)
        .read_to_end(&mut buf)
        .map_err(|e| format!("read error: {e}"))?;
    let over = buf.len() > cap;
    if over {
        buf.truncate(cap);
        let _ = child.kill();
    }
    let status = child.wait().map_err(|e| format!("curl: {e}"))?;
    if buf.is_empty() {
        let mut err = String::new();
        if let Some(mut s) = child.stderr.take() {
            let _ = s.read_to_string(&mut err);
        }
        let first = err.lines().next().unwrap_or("").trim();
        return Err(if first.is_empty() {
            format!("curl exited with {status}")
        } else {
            first.to_string()
        });
    }
    let (code, reason, headers, body) = split_response(&buf)?;
    let mut truncated = over;
    let mut body = body.to_vec();
    if body.len() > p.max_bytes {
        body.truncate(p.max_bytes);
        truncated = true;
    }
    Ok(Response {
        status: code,
        reason,
        headers,
        body,
        truncated,
    })
}

// ── body helpers ─────────────────────────────────────────────────────────────

/// Decode to text: UTF-8 (lossy) unless the Content-Type says Latin-1/Windows-1252.
pub fn decode_body(body: &[u8], content_type: &str) -> String {
    let ct = content_type.to_ascii_lowercase();
    let latin1 = ct.contains("charset=iso-8859-1")
        || ct.contains("charset=windows-1252")
        || ct.contains("charset=latin1");
    if latin1 {
        body.iter().map(|&b| b as char).collect()
    } else {
        String::from_utf8_lossy(body).into_owned()
    }
}

pub fn is_binary(mime: &str, body: &[u8]) -> bool {
    if mime.starts_with("image/")
        || mime.starts_with("audio/")
        || mime.starts_with("video/")
        || mime.starts_with("font/")
    {
        return !mime.contains("svg");
    }
    if matches!(
        mime,
        "application/pdf"
            | "application/zip"
            | "application/gzip"
            | "application/octet-stream"
            | "application/x-tar"
            | "application/wasm"
            | "application/x-7z-compressed"
            | "application/vnd.microsoft.portable-executable"
    ) {
        return true;
    }
    if mime.starts_with("text/")
        || mime.contains("json")
        || mime.contains("xml")
        || mime.contains("javascript")
    {
        return false;
    }
    body.iter().take(2048).any(|&b| b == 0)
}

pub fn looks_html(mime: &str, text: &str) -> bool {
    if mime.contains("html") {
        return true;
    }
    let head: String = text
        .trim_start()
        .chars()
        .take(64)
        .collect::<String>()
        .to_ascii_lowercase();
    head.starts_with("<!doctype html") || head.starts_with("<html")
}

/// First executable named in `names` found on `PATH`.
pub fn which(names: &[&str]) -> Option<std::path::PathBuf> {
    let path = std::env::var_os("PATH")?;
    for dir in std::env::split_paths(&path) {
        for n in names {
            let p = dir.join(n);
            if p.is_file() {
                return Some(p);
            }
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::net::TcpListener;
    use std::thread;

    /// Serve `n` connections on loopback with `handler(request_line) -> raw response`.
    fn serve(n: usize, handler: fn(&str) -> Vec<u8>) -> u16 {
        let l = TcpListener::bind("127.0.0.1:0").unwrap();
        let port = l.local_addr().unwrap().port();
        thread::spawn(move || {
            for _ in 0..n {
                let Ok((mut s, _)) = l.accept() else { return };
                let mut buf = [0u8; 4096];
                let k = s.read(&mut buf).unwrap_or(0);
                let req = String::from_utf8_lossy(&buf[..k]).into_owned();
                let first = req.lines().next().unwrap_or("").to_string();
                let _ = s.write_all(&handler(&first));
            }
        });
        port
    }

    fn params(url: &str, max_bytes: usize) -> CurlParams<'_> {
        std::env::set_var("NO_PROXY", "*");
        std::env::set_var("no_proxy", "*");
        CurlParams {
            url,
            user_agent: "romacode-test",
            connect_timeout: 5,
            max_time: 10,
            max_bytes,
            pin: None,
            proxy: "",
            ca_bundle: "",
            auth: None,
        }
    }

    #[test]
    fn fetches_headers_and_body() {
        let port = serve(1, |req| {
            assert!(req.starts_with("GET /hello HTTP/1.1"), "{req}");
            b"HTTP/1.1 200 OK\r\nContent-Type: text/html; charset=utf-8\r\nX-Test: yes\r\nContent-Length: 5\r\nConnection: close\r\n\r\nhello".to_vec()
        });
        let url = format!("http://127.0.0.1:{port}/hello");
        let r = curl_get(&params(&url, 1000)).unwrap();
        assert_eq!((r.status, r.reason.as_str()), (200, "OK"));
        assert_eq!(r.header("x-test"), Some("yes"));
        assert_eq!(r.mime(), "text/html");
        assert_eq!(r.body, b"hello");
        assert!(!r.truncated && !r.is_redirect());
    }

    #[test]
    fn reports_redirects_without_following() {
        let port = serve(1, |_| {
            b"HTTP/1.1 302 Found\r\nLocation: /next\r\nContent-Length: 0\r\nConnection: close\r\n\r\n".to_vec()
        });
        let url = format!("http://127.0.0.1:{port}/");
        let r = curl_get(&params(&url, 1000)).unwrap();
        assert!(r.is_redirect(), "{r:?}");
        assert_eq!(r.header("Location"), Some("/next"));
    }

    #[test]
    fn caps_large_bodies() {
        let port = serve(1, |_| {
            let mut v = b"HTTP/1.1 200 OK\r\nContent-Type: text/plain\r\nConnection: close\r\n\r\n"
                .to_vec();
            v.extend(std::iter::repeat(b'x').take(500_000));
            v
        });
        let url = format!("http://127.0.0.1:{port}/big");
        let r = curl_get(&params(&url, 1000)).unwrap();
        assert!(r.truncated);
        assert_eq!(r.body.len(), 1000);
    }

    #[test]
    fn error_statuses_are_returned_not_raised() {
        let port = serve(1, |_| {
            b"HTTP/1.1 404 Not Found\r\nContent-Length: 4\r\nConnection: close\r\n\r\nnope".to_vec()
        });
        let url = format!("http://127.0.0.1:{port}/missing");
        let r = curl_get(&params(&url, 1000)).unwrap();
        assert_eq!(
            (r.status, r.reason.as_str(), r.body.as_slice()),
            (404, "Not Found", &b"nope"[..])
        );
    }

    #[test]
    fn connection_refused_is_a_clean_error() {
        let l = TcpListener::bind("127.0.0.1:0").unwrap();
        let port = l.local_addr().unwrap().port();
        drop(l);
        let url = format!("http://127.0.0.1:{port}/");
        let e = curl_get(&params(&url, 1000)).unwrap_err();
        assert!(
            e.to_lowercase().contains("connect") || e.contains("7"),
            "{e}"
        );
    }

    #[test]
    fn only_http_schemes_are_allowed_by_curl() {
        let e = curl_get(&params("file:///etc/passwd", 1000)).unwrap_err();
        assert!(!e.is_empty());
    }

    #[test]
    fn config_cannot_be_injected_through_values() {
        let p = CurlParams {
            url: "http://x/\"\nproxy = \"evil",
            user_agent: "ua\"\nheader = \"X: y",
            connect_timeout: 1,
            max_time: 1,
            max_bytes: 1,
            pin: None,
            proxy: "",
            ca_bundle: "",
            auth: Some("u:}\"\nproxy = \"evil"),
        };
        let c = curl_config(&p);
        assert!(
            !c.contains("\nproxy = \"evil") && !c.contains("\nheader = \"X: y"),
            "{c}"
        );
    }

    #[test]
    fn splits_interim_and_connect_blocks() {
        let raw = b"HTTP/1.1 200 Connection established\r\n\r\nHTTP/1.1 100 Continue\r\n\r\nHTTP/2 200 \r\ncontent-type: text/plain\r\n\r\nbody";
        let (s, _, h, b) = split_response(raw).unwrap();
        assert_eq!((s, b), (200, &b"body"[..]));
        assert_eq!(h[0].0, "content-type");
        assert!(split_response(b"garbage").is_err());
        assert!(split_response(b"NOTHTTP 200\r\n\r\n").is_err());
    }

    #[test]
    fn body_helpers() {
        assert_eq!(
            decode_body(&[0x63, 0x61, 0x66, 0xe9], "text/html; charset=ISO-8859-1"),
            "café"
        );
        assert_eq!(decode_body("café".as_bytes(), "text/html"), "café");
        assert_eq!(decode_body(&[0xff, b'a'], "text/plain"), "\u{fffd}a");
        assert!(is_binary("image/png", b"") && is_binary("application/pdf", b"%PDF"));
        assert!(
            !is_binary("image/svg+xml", b"<svg/>")
                && !is_binary("text/plain", b"a\0b")
                && !is_binary("application/json", b"{}")
        );
        assert!(is_binary("", b"ab\0cd"));
        assert!(
            looks_html("text/html", "")
                && looks_html("", "  <!DOCTYPE html><p>")
                && !looks_html("text/plain", "hello")
        );
    }
}
