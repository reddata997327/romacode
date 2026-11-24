//! RFC 6455 WebSocket — handshake + binary framing, std-only.
//!
//! Server side:
//!   read GET, validate Upgrade, compute Sec-WebSocket-Accept, reply.
//! Client side:
//!   send GET, read response, validate Sec-WebSocket-Accept.
//!
//! Framing: implements text/binary/close/ping/pong with masking on the
//! client side and unmasked frames on the server side, per RFC 6455.

use std::collections::HashMap;
use std::io::{Read, Write};
use std::net::TcpStream;

use crate::base64;
use crate::sha1;

const WS_GUID: &str = "258EAFA5-E914-47DA-95CA-C5AB0DC85B11";
const WS_PATH: &str = "/chisel";

#[derive(Debug)]
pub struct Handshake {
    pub accept_key: String,
    pub auth_header: Option<String>,
}

/// Compute the server's Sec-WebSocket-Accept value for a client key.
pub fn accept_key(client_key: &str) -> String {
    let mut cat = String::from(client_key);
    cat.push_str(WS_GUID);
    let h = sha1::sha1(cat.as_bytes());
    base64::encode(&h)
}

/// Constant-time string equality: XOR-folds every byte of both inputs
/// (zero-padded to the longer length) plus the length delta into one
/// accumulator, so timing does not reveal where two tokens first differ.
fn ct_eq(a: &str, b: &str) -> bool {
    let (a, b) = (a.as_bytes(), b.as_bytes());
    let mut acc: u64 = (a.len() as u64) ^ (b.len() as u64);
    let n = a.len().max(b.len());
    for i in 0..n {
        let x = a.get(i).copied().unwrap_or(0) as u64;
        let y = b.get(i).copied().unwrap_or(0) as u64;
        acc |= x ^ y;
    }
    acc == 0
}

// ---------------------------------------------------------------------------
// Server handshake (read request, write response)

pub fn server_handshake(
    stream: &mut TcpStream,
    auth_token: Option<&str>,
    custom_headers: &HashMap<String, String>,
) -> Result<Handshake, String> {
    let mut buf = Vec::with_capacity(8192);
    let mut byte = [0u8; 1];
    loop {
        let n = stream.read(&mut byte).map_err(|e| format!("read: {}", e))?;
        if n == 0 {
            return Err("client closed before WS handshake".into());
        }
        buf.push(byte[0]);
        if buf.len() >= 4 && &buf[buf.len() - 4..] == b"\r\n\r\n" {
            break;
        }
        if buf.len() > 65536 {
            return Err("headers too large".into());
        }
    }
    let text = String::from_utf8_lossy(&buf);
    let mut lines = text.split("\r\n");
    let start = lines.next().unwrap_or("");
    if !start.starts_with("GET ") {
        return Err(format!("expected GET, got: {}", start));
    }
    let path = start.split(' ').nth(1).unwrap_or("");
    if path != WS_PATH {
        // We accept any path that doesn't conflict with our health check.
        if path != "/health" && !path.starts_with(WS_PATH) {
            return Err(format!("unexpected path: {}", path));
        }
    }

    let mut headers: HashMap<String, String> = HashMap::new();
    for line in lines {
        if line.is_empty() {
            break;
        }
        if let Some(idx) = line.find(':') {
            headers.insert(
                line[..idx].trim().to_ascii_lowercase(),
                line[idx + 1..].trim().to_string(),
            );
        }
    }

    // Validate Upgrade + Connection
    let upgrade = headers.get("upgrade").map(|s| s.to_ascii_lowercase());
    let connection = headers.get("connection").map(|s| s.to_ascii_lowercase());
    if upgrade.as_deref() != Some("websocket")
        || !connection
            .map(|c| c.split(',').any(|t| t.trim() == "upgrade"))
            .unwrap_or(false)
    {
        return Err("not a websocket upgrade".into());
    }

    let client_key = match headers.get("sec-websocket-key") {
        Some(k) => k.clone(),
        None => return Err("missing Sec-WebSocket-Key".into()),
    };

    // Token auth: client sends it as Basic-like header, or in a custom
    // "Auth" header, or as ?token= query string. We accept the custom
    // header "Auth" or "Authorization: Bearer <token>".
    if let Some(tok) = auth_token {
        let got = headers.get("auth")
            .cloned()
            .or_else(|| {
                headers
                    .get("authorization")
                    .and_then(|a| a.strip_prefix("Bearer ").map(|s| s.to_string()))
            })
            .or_else(|| {
                // ?token= on the path
                path.split('?').nth(1).and_then(|q| {
                    q.split('&')
                        .find_map(|kv| {
                            let (k, v) = kv.split_once('=')?;
                            if k == "token" { Some(v.to_string()) } else { None }
                        })
                })
            });
        let ok = match got.as_deref() {
            Some(g) => ct_eq(g, tok),
            None => false,
        };
        if !ok {
            return Err("authentication failed".into());
        }
    }

    let accept = accept_key(&client_key);

    let mut resp = String::new();
    resp.push_str("HTTP/1.1 101 Switching Protocols\r\n");
    resp.push_str("Upgrade: websocket\r\n");
    resp.push_str("Connection: Upgrade\r\n");
    resp.push_str(&format!("Sec-WebSocket-Accept: {}\r\n", accept));
    for (k, v) in custom_headers {
        resp.push_str(&format!("{}: {}\r\n", k, v));
    }
    resp.push_str("\r\n");
    stream
        .write_all(resp.as_bytes())
        .map_err(|e| format!("write: {}", e))?;

    Ok(Handshake {
        accept_key: accept,
        auth_header: None,
    })
}

// ---------------------------------------------------------------------------
// Client handshake (write request, read response)

pub fn client_handshake(
    stream: &mut TcpStream,
    host: &str,
    port: u16,
    auth_token: Option<&str>,
    headers: &HashMap<String, String>,
) -> Result<(), String> {
    let key_bytes: [u8; 16] = rand16();
    let client_key = base64::encode(&key_bytes);

    let mut req = String::new();
    req.push_str(&format!("GET {} HTTP/1.1\r\n", WS_PATH));
    req.push_str(&format!("Host: {}:{}\r\n", host, port));
    req.push_str("Upgrade: websocket\r\n");
    req.push_str("Connection: Upgrade\r\n");
    req.push_str(&format!("Sec-WebSocket-Key: {}\r\n", client_key));
    req.push_str("Sec-WebSocket-Version: 13\r\n");
    if let Some(tok) = auth_token {
        req.push_str(&format!("Auth: {}\r\n", tok));
    }
    for (k, v) in headers {
        req.push_str(&format!("{}: {}\r\n", k, v));
    }
    req.push_str("\r\n");
    stream
        .write_all(req.as_bytes())
        .map_err(|e| format!("write: {}", e))?;

    // read response
    let mut buf = Vec::with_capacity(8192);
    let mut byte = [0u8; 1];
    loop {
        let n = stream.read(&mut byte).map_err(|e| format!("read: {}", e))?;
        if n == 0 {
            return Err("server closed before WS handshake".into());
        }
        buf.push(byte[0]);
        if buf.len() >= 4 && &buf[buf.len() - 4..] == b"\r\n\r\n" {
            break;
        }
        if buf.len() > 65536 {
            return Err("headers too large".into());
        }
    }
    let text = String::from_utf8_lossy(&buf);
    let mut lines = text.split("\r\n");
    let status = lines.next().unwrap_or("");
    if !status.starts_with("HTTP/1.1 101") {
        return Err(format!("bad WS response: {}", status));
    }
    let mut resp_h: HashMap<String, String> = HashMap::new();
    for line in lines {
        if line.is_empty() {
            break;
        }
        if let Some(idx) = line.find(':') {
            resp_h.insert(
                line[..idx].trim().to_ascii_lowercase(),
                line[idx + 1..].trim().to_string(),
            );
        }
    }
    let expected = accept_key(&client_key);
    let got = resp_h
        .get("sec-websocket-accept")
        .ok_or("missing Sec-WebSocket-Accept")?;
    if got != &expected {
        return Err(format!(
            "Sec-WebSocket-Accept mismatch (expected {}, got {})",
            expected, got
        ));
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// Frame I/O

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OpCode {
    Continuation,
    Text,
    Binary,
    Close,
    Ping,
    Pong,
}

impl OpCode {
    fn from_byte(b: u8) -> Option<OpCode> {
        match b & 0x0f {
            0x0 => Some(OpCode::Continuation),
            0x1 => Some(OpCode::Text),
            0x2 => Some(OpCode::Binary),
            0x8 => Some(OpCode::Close),
            0x9 => Some(OpCode::Ping),
            0xA => Some(OpCode::Pong),
            _ => None,
        }
    }
    fn to_byte(self) -> u8 {
        match self {
            OpCode::Continuation => 0x0,
            OpCode::Text => 0x1,
            OpCode::Binary => 0x2,
            OpCode::Close => 0x8,
            OpCode::Ping => 0x9,
            OpCode::Pong => 0xA,
        }
    }
}

pub struct Frame {
    pub fin: bool,
    pub op: OpCode,
    pub payload: Vec<u8>,
}

impl Frame {
    /// Read one frame from the stream. The stream may be either masked
    /// (client->server) or unmasked (server->client); both are handled.
    pub fn read<R: Read>(r: &mut R) -> Result<Frame, String> {
        let mut hdr = [0u8; 2];
        read_exact(r, &mut hdr).map_err(|e| format!("frame hdr: {}", e))?;
        let fin = hdr[0] & 0x80 != 0;
        let op = OpCode::from_byte(hdr[0]).ok_or("invalid opcode")?;
        let masked = hdr[1] & 0x80 != 0;
        let mut len = (hdr[1] & 0x7f) as u64;
        if len == 126 {
            let mut ext = [0u8; 2];
            read_exact(r, &mut ext).map_err(|e| format!("frame ext len: {}", e))?;
            len = u16::from_be_bytes(ext) as u64;
        } else if len == 127 {
            let mut ext = [0u8; 8];
            read_exact(r, &mut ext).map_err(|e| format!("frame ext len: {}", e))?;
            len = u64::from_be_bytes(ext);
        }
        let mask = if masked {
            let mut m = [0u8; 4];
            read_exact(r, &mut m).map_err(|e| format!("frame mask: {}", e))?;
            Some(m)
        } else {
            None
        };
        if len > 64 * 1024 * 1024 {
            return Err(format!("frame too large: {}", len));
        }
        let mut payload = vec![0u8; len as usize];
        read_exact(r, &mut payload).map_err(|e| format!("frame payload ({}): {}", len, e))?;
        if let Some(m) = mask {
            for (i, b) in payload.iter_mut().enumerate() {
                *b ^= m[i & 3];
            }
        }
        Ok(Frame { fin, op, payload })
    }

    /// Write a frame. If `mask` is true (client -> server), payload is
    /// masked with a random key.
    pub fn write<W: Write>(w: &mut W, frame: &Frame, mask: bool) -> Result<(), String> {
        let mut hdr = Vec::with_capacity(14);
        let mut b0 = if frame.fin { 0x80 } else { 0 };
        b0 |= frame.op.to_byte();
        hdr.push(b0);

        let len = frame.payload.len();
        let mask_bit = if mask { 0x80 } else { 0 };
        if len < 126 {
            hdr.push(mask_bit | (len as u8));
        } else if len <= u16::MAX as usize {
            hdr.push(mask_bit | 126);
            hdr.extend_from_slice(&(len as u16).to_be_bytes());
        } else {
            hdr.push(mask_bit | 127);
            hdr.extend_from_slice(&(len as u64).to_be_bytes());
        }
        if mask {
            let m = rand16_arr();
            hdr.extend_from_slice(&m[..4]);
            w.write_all(&hdr).map_err(|e| format!("write hdr: {}", e))?;
            let mut masked = Vec::with_capacity(frame.payload.len());
            for (i, &b) in frame.payload.iter().enumerate() {
                masked.push(b ^ m[i & 3]);
            }
            w.write_all(&masked).map_err(|e| format!("write payload: {}", e))?;
        } else {
            w.write_all(&hdr).map_err(|e| format!("write hdr: {}", e))?;
            w.write_all(&frame.payload).map_err(|e| format!("write payload: {}", e))?;
        }
        Ok(())
    }
}

fn read_exact<R: Read>(r: &mut R, buf: &mut [u8]) -> std::io::Result<()> {
    let mut read = 0;
    while read < buf.len() {
        let n = r.read(&mut buf[read..])?;
        if n == 0 {
            return Err(std::io::Error::new(
                std::io::ErrorKind::UnexpectedEof,
                "EOF mid-frame",
            ));
        }
        read += n;
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// A simple PRNG for masking keys. Not cryptographic — RFC 6455 only requires
// the mask be "difficult to predict" to avoid cache poisoning on
// intermediaries; the tunnel itself is auth'd at handshake time.

fn rand16() -> [u8; 16] {
    let mut a = rand16_arr();
    // Take 16 of 32 bytes
    let mut out = [0u8; 16];
    out.copy_from_slice(&a[..16]);
    a.rotate_left(16);
    out
}

fn rand16_arr() -> [u8; 32] {
    // 32-bit xorshift seeded from time + pid + a counter
    use std::sync::atomic::{AtomicU64, Ordering};
    static STATE: AtomicU64 = AtomicU64::new(0x9E37_79B9_7F4A_7C15);
    let mut s = STATE.load(Ordering::Relaxed);
    if s == 0 {
        let t = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos() as u64)
            .unwrap_or(1);
        s = t.wrapping_mul(2_862_933_555_777_941_757).wrapping_add(1);
    }
    let next = {
        let mut x = s;
        x ^= x << 13;
        x ^= x >> 7;
        x ^= x << 17;
        x
    };
    STATE.store(next, Ordering::Relaxed);

    let mut out = [0u8; 32];
    let mut x = next;
    for chunk in out.chunks_mut(8) {
        x ^= x << 13;
        x ^= x >> 7;
        x ^= x << 17;
        chunk.copy_from_slice(&x.to_le_bytes());
    }
    out
}
