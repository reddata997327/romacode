//! Minimal W3C WebDriver HTTP client — dependency-free (std `TcpStream` +
//! `serde_json`). Speaks the protocol surface Servo's WebDriver server
//! (`components/webdriver_server`) implements:
//!
//! `GET /status` · `POST /session` · `DELETE /session/{id}` ·
//! `POST /session/{id}/url` · `GET /session/{id}/url` · `GET /session/{id}/title` ·
//! `POST /session/{id}/execute/sync` · `GET /session/{id}/screenshot` ·
//! `GET /session/{id}/cookies` · `POST /session/{id}/back|forward|refresh`
//!
//! Every response is unwrapped from the W3C envelope `{"value": …}`; HTTP
//! errors are turned into `Err` with the server's `error`/`message`.

use serde_json::Value;
use std::io::Read;
use std::net::TcpStream;
use std::time::Duration;

pub struct WebDriver {
    pub host: String,
    pub port: u16,
}

struct Raw {
    status: u16,
    body: String,
}

impl WebDriver {
    pub fn new(host: String, port: u16) -> Self {
        Self { host, port }
    }

    fn raw(
        &self,
        method: &str,
        path: &str,
        body: Option<&Value>,
        timeout: Duration,
    ) -> Result<Raw, String> {
        let mut stream = TcpStream::connect((self.host.as_str(), self.port))
            .map_err(|e| format!("webdriver connect {}:{}: {e}", self.host, self.port))?;
        stream
            .set_read_timeout(Some(timeout))
            .and_then(|_| stream.set_write_timeout(Some(timeout)))
            .map_err(|e| format!("webdriver timeouts: {e}"))?;

        let payload = body.map(|b| b.to_string()).unwrap_or_default();
        let head = format!(
            "{method} {path} HTTP/1.1\r\nHost: {}:{}\r\nContent-Type: application/json; charset=utf-8\r\nAccept: application/json\r\nUser-Agent: romacode-browser\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
            self.host,
            self.port,
            payload.len()
        );

        use std::io::Write;
        stream
            .write_all(head.as_bytes())
            .and_then(|_| {
                if payload.is_empty() {
                    Ok(())
                } else {
                    stream.write_all(payload.as_bytes())
                }
            })
            .and_then(|_| stream.flush())
            .map_err(|e| format!("webdriver write {}:{}{path}: {e}", self.host, self.port))?;

        let bytes = read_response(&mut stream, timeout)?;

        let text = String::from_utf8_lossy(&bytes).into_owned();
        let (status, body_start) = parse_head(&text)
            .ok_or_else(|| format!("webdriver: malformed HTTP response from {}:{}{path}", self.host, self.port))?;
        let body = text[body_start..].to_string();
        Ok(Raw { status, body })
    }

    /// One WebDriver call: returns the unwrapped `value` on success.
    pub fn call(
        &self,
        method: &str,
        path: &str,
        body: Option<&Value>,
        timeout: Duration,
    ) -> Result<Value, String> {
        let r = self.raw(method, path, body, timeout)?;
        if r.body.trim().is_empty() {
            if r.status < 400 {
                return Ok(Value::Null);
            }
            return Err(format!("webdriver {method} {path} -> HTTP {}", r.status));
        }
        let v: Value = serde_json::from_str(r.body.trim())
            .map_err(|e| format!("webdriver: non-JSON response from {}:{}{path}: {e}", self.host, self.port))?;
        let value = v.get("value").cloned().unwrap_or(Value::Null);
        if r.status >= 400 {
            let err = value["error"].as_str().unwrap_or("webdriver error");
            let msg = value["message"].as_str().unwrap_or("");
            return Err(if msg.is_empty() {
                format!("webdriver {err} (HTTP {})", r.status)
            } else {
                format!("webdriver {err}: {msg}")
            });
        }
        Ok(value)
    }

    pub fn status(&self, timeout: Duration) -> Result<Value, String> {
        self.call("GET", "/status", None, timeout)
    }

    pub fn new_session(
        &self,
        accept_insecure_certs: bool,
        timeout: Duration,
    ) -> Result<String, String> {
        let full = serde_json::json!({
            "capabilities": {
                "alwaysMatch": {
                    "acceptInsecureCerts": accept_insecure_certs,
                    "unhandledPromptBehavior": "accept",
                    "pageLoadStrategy": "normal"
                }
            }
        });
        match self.call("POST", "/session", Some(&full), timeout) {
            Ok(v) => Self::session_id_from(v),
            Err(e) => {
                // Some engines reject individual capability entries (e.g.
                // Servo cannot accept insecure certs) — retry minimally so a
                // hard capability mismatch never blocks the session.
                let minimal = serde_json::json!({ "capabilities": { "alwaysMatch": {} } });
                let v = self.call("POST", "/session", Some(&minimal), timeout)
                    .map_err(|retry| format!("{e}; minimal-capability retry also failed: {retry}"))?;
                Self::session_id_from(v)
            }
        }
    }

    fn session_id_from(v: Value) -> Result<String, String> {
        v["sessionId"]
            .as_str()
            .map(|s| s.to_string())
            .ok_or_else(|| format!("webdriver: session response missing sessionId: {v}"))
    }

    pub fn delete_session(&self, session: &str, timeout: Duration) -> Result<(), String> {
        self.call("DELETE", &session_path(session), None, timeout).map(|_| ())
    }

    pub fn navigate(&self, session: &str, url: &str, timeout: Duration) -> Result<(), String> {
        let body = serde_json::json!({ "url": url });
        self.call(
            "POST",
            &format!("{}/url", session_path(session)),
            Some(&body),
            timeout,
        )
        .map(|_| ())
    }

    pub fn eval(
        &self,
        session: &str,
        script: &str,
        args: &[Value],
        timeout: Duration,
    ) -> Result<Value, String> {
        let body = serde_json::json!({ "script": script, "args": args });
        self.call(
            "POST",
            &format!("{}/execute/sync", session_path(session)),
            Some(&body),
            timeout,
        )
    }

    pub fn screenshot(&self, session: &str, timeout: Duration) -> Result<String, String> {
        let v = self.call(
            "GET",
            &format!("{}/screenshot", session_path(session)),
            None,
            timeout,
        )?;
        v.as_str()
            .map(|s| s.to_string())
            .ok_or_else(|| "webdriver: screenshot did not return base64".into())
    }

    pub fn cookies(&self, session: &str, timeout: Duration) -> Result<Value, String> {
        self.call(
            "GET",
            &format!("{}/cookies", session_path(session)),
            None,
            timeout,
        )
    }

    pub fn simple_post(&self, session: &str, verb: &str, timeout: Duration) -> Result<(), String> {
        self.call(
            "POST",
            &format!("{}/{verb}", session_path(session)),
            None,
            timeout,
        )
        .map(|_| ())
    }
}

fn session_path(session: &str) -> String {
    format!("/session/{session}")
}

/// Read one full HTTP response: header block, then body per
/// `Content-Length`, `Transfer-Encoding: chunked` or EOF.
fn read_response(stream: &mut TcpStream, timeout: Duration) -> Result<Vec<u8>, String> {
    let deadline = std::time::Instant::now() + timeout;
    let mut buf: Vec<u8> = Vec::with_capacity(8 * 1024);
    let mut chunk = [0u8; 16 * 1024];
    loop {
        if std::time::Instant::now() >= deadline {
            return Err("webdriver: timed out reading response".into());
        }
        match stream.read(&mut chunk) {
            Ok(0) => break, // EOF — response complete
            Ok(n) => buf.extend_from_slice(&chunk[..n]),
            Err(e)
                if e.kind() == std::io::ErrorKind::WouldBlock
                    || e.kind() == std::io::ErrorKind::TimedOut =>
            {
                return Err("webdriver: read timed out mid-response".into())
            }
            Err(e) => return Err(format!("webdriver read: {e}")),
        }
        if let Some(body_start) = parse_head_bytes(&buf) {
            if let Some(len) = content_length(&buf, body_start) {
                if buf.len() >= body_start + len {
                    break;
                }
            } else if chunked(&buf, body_start) {
                if chunks_complete(&buf[body_start..]) {
                    break;
                }
                continue; // keep reading until the terminating chunk shows up
            }
        }
        if std::time::Instant::now() >= deadline {
            return Err("webdriver: timed out reading response".into());
        }
    }
    decode_body_bytes(&buf)
}

fn find_head_end(buf: &[u8]) -> Option<usize> {
    buf.windows(4).position(|w| w == b"\r\n\r\n")
}

fn parse_head_bytes(buf: &[u8]) -> Option<usize> {
    find_head_end(buf).map(|p| p + 4)
}

fn header_value(head: &[u8], name: &str) -> Option<String> {
    let text = String::from_utf8_lossy(head);
    for line in text.lines() {
        let Some((k, v)) = line.split_once(':') else { continue };
        if k.trim().eq_ignore_ascii_case(name) {
            return Some(v.trim().to_string());
        }
    }
    None
}

fn content_length(buf: &[u8], body_start: usize) -> Option<usize> {
    header_value(&buf[..body_start], "Content-Length")
        .and_then(|v| v.trim().parse::<usize>().ok())
}

fn chunked(buf: &[u8], body_start: usize) -> bool {
    header_value(&buf[..body_start], "Transfer-Encoding")
        .map(|v| v.to_ascii_lowercase().contains("chunked"))
        .unwrap_or(false)
}

/// Chunked body is complete when the terminating `0`-size chunk (and its
/// trailing CRLF) has been received. Walks the actual chunk chain instead of
/// substring-matching `\r\n0\r\n` — a data chunk legitimately containing that
/// byte sequence used to end the read early (truncated body → JSON parse
/// failure downstream), and a partially-received terminator was discarded.
fn chunks_complete(body: &[u8]) -> bool {
    let mut rest = body;
    loop {
        let Some(line_end) = find_crlf(rest) else { return false };
        let size_str = String::from_utf8_lossy(&rest[..line_end]).into_owned();
        let size_str = size_str.split(';').next().unwrap_or("").trim().to_string();
        let Ok(size) = usize::from_str_radix(&size_str, 16) else { return false };
        if size == 0 {
            // Terminating chunk seen; the trailer (optional) ends with CRLF.
            // We only need to know the stream is done — the decoder below
            // stops at the same place.
            return true;
        }
        let data_start = line_end + 2;
        let need = data_start.saturating_add(size).saturating_add(2);
        if rest.len() < need {
            return false; // chunk data (plus its CRLF) not fully arrived yet
        }
        rest = &rest[need..];
    }
}

/// Return the full response (head + body) with chunked transfer-coding decoded.
fn decode_body_bytes(buf: &[u8]) -> Result<Vec<u8>, String> {
    let body_start = parse_head_bytes(buf)
        .ok_or_else(|| "webdriver: response without complete HTTP head".to_string())?;
    let head = &buf[..body_start];
    let body = &buf[body_start..];
    if chunked(buf, body_start) {
        let mut out = Vec::with_capacity(buf.len());
        out.extend_from_slice(head);
        let mut rest = body;
        loop {
            let Some(line_end) = find_crlf(rest) else { break };
            let size_str = String::from_utf8_lossy(&rest[..line_end]).into_owned();
            let size_str = size_str.split(';').next().unwrap_or("").trim().to_string();
            let Ok(size) = usize::from_str_radix(&size_str, 16) else { break };
            if size == 0 {
                break;
            }
            let data_start = line_end + 2;
            if rest.len() < data_start + size {
                break;
            }
            out.extend_from_slice(&rest[data_start..data_start + size]);
            rest = &rest[(data_start + size + 2).min(rest.len())..];
        }
        Ok(out)
    } else {
        Ok(buf.to_vec())
    }
}

fn find_crlf(buf: &[u8]) -> Option<usize> {
    buf.windows(2).position(|w| w == b"\r\n")
}

fn parse_head(text: &str) -> Option<(u16, usize)> {
    let end = find_head_end(text.as_bytes())? + 4;
    let status_line = text.lines().next()?;
    let status = status_line.split_whitespace().nth(1)?.parse::<u16>().ok()?;
    Some((status, end))
}
