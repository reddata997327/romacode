//! A tiny HTTP request/response parser used only for the WebSocket
//! handshake. It assumes the underlying TCP stream is in clear-text
//! (chisel-rs does not implement TLS, since TLS from scratch is unsafe).

use std::collections::HashMap;

pub struct Request {
    pub method: String,
    pub path: String,
    pub headers: HashMap<String, String>,
    pub body: Vec<u8>,
}

pub struct Response {
    pub status: u16,
    pub reason: String,
    pub headers: HashMap<String, String>,
    pub body: Vec<u8>,
}

/// Read an HTTP message from a stream until the headers' terminating
/// blank line, returning the raw bytes of the header block (so the
/// caller can inspect how many bytes were consumed).
pub fn read_request<R: std::io::Read>(r: &mut R) -> std::io::Result<Request> {
    let mut buf = Vec::new();
    let mut tmp = [0u8; 1];
    loop {
        let n = r.read(&mut tmp)?;
        if n == 0 {
            return Err(std::io::Error::new(
                std::io::ErrorKind::ConnectionAborted,
                "EOF before end of HTTP headers",
            ));
        }
        buf.push(tmp[0]);
        if buf.len() >= 4 && &buf[buf.len() - 4..] == b"\r\n\r\n" {
            break;
        }
        if buf.len() > 65536 {
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                "HTTP headers too large",
            ));
        }
    }
    let text = String::from_utf8_lossy(&buf);
    let mut lines = text.split("\r\n");
    let start = lines.next().ok_or_else(|| {
        std::io::Error::new(std::io::ErrorKind::InvalidData, "empty HTTP request")
    })?;
    let mut parts = start.splitn(3, ' ');
    let method = parts.next().unwrap_or("").to_string();
    let path = parts.next().unwrap_or("").to_string();

    let mut headers = HashMap::new();
    for line in lines {
        if line.is_empty() {
            break;
        }
        if let Some(idx) = line.find(':') {
            let k = line[..idx].trim().to_ascii_lowercase();
            let v = line[idx + 1..].trim().to_string();
            headers.insert(k, v);
        }
    }
    Ok(Request {
        method,
        path,
        headers,
        body: Vec::new(),
    })
}

impl Response {
    pub fn serialize(&self) -> Vec<u8> {
        let mut out = Vec::new();
        out.extend_from_slice(format!("HTTP/1.1 {} {}\r\n", self.status, self.reason).as_bytes());
        for (k, v) in &self.headers {
            out.extend_from_slice(format!("{}: {}\r\n", k, v).as_bytes());
        }
        out.extend_from_slice(b"\r\n");
        out.extend_from_slice(&self.body);
        out
    }
}
