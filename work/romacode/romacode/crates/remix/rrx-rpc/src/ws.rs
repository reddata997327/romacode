//! Real WebSocket (RFC 6455) client for eth_subscribe — live chain data over wss://.
//!
//! TLS is bridged through the system `openssl s_client` process (no external
//! crates), plain TCP for ws://. Frames: client→server masked, fragmentation,
//! ping/pong handled. All data is REAL live chain data from real endpoints.

use std::io::{Read, Write};
use std::net::TcpStream;
use std::process::{Child, ChildStdin, ChildStdout, Command, Stdio};
use std::sync::mpsc;
use std::time::{Duration, Instant};

// ---------- SHA-1 (handshake only) ----------

fn sha1(data: &[u8]) -> [u8; 20] {
    let mut h: [u32; 5] = [0x67452301, 0xEFCDAB89, 0x98BADCFE, 0x10325476, 0xC3D2E1F0];
    let ml = (data.len() as u64) * 8;
    let mut msg = data.to_vec();
    msg.push(0x80);
    while msg.len() % 64 != 56 {
        msg.push(0);
    }
    msg.extend_from_slice(&ml.to_be_bytes());
    for chunk in msg.chunks(64) {
        let mut w = [0u32; 80];
        for i in 0..16 {
            w[i] = u32::from_be_bytes([chunk[i * 4], chunk[i * 4 + 1], chunk[i * 4 + 2], chunk[i * 4 + 3]]);
        }
        for i in 16..80 {
            w[i] = (w[i - 3] ^ w[i - 8] ^ w[i - 14] ^ w[i - 16]).rotate_left(1);
        }
        let (mut a, mut b, mut c, mut d, mut e) = (h[0], h[1], h[2], h[3], h[4]);
        for (i, &wi) in w.iter().enumerate() {
            let (f, k) = match i {
                0..=19 => ((b & c) | ((!b) & d), 0x5A827999u32),
                20..=39 => (b ^ c ^ d, 0x6ED9EBA1),
                40..=59 => ((b & c) | (b & d) | (c & d), 0x8F1BBCDC),
                _ => (b ^ c ^ d, 0xCA62C1D6),
            };
            let tmp = a
                .rotate_left(5)
                .wrapping_add(f)
                .wrapping_add(e)
                .wrapping_add(k)
                .wrapping_add(wi);
            e = d;
            d = c;
            c = b.rotate_left(30);
            b = a;
            a = tmp;
        }
        h[0] = h[0].wrapping_add(a);
        h[1] = h[1].wrapping_add(b);
        h[2] = h[2].wrapping_add(c);
        h[3] = h[3].wrapping_add(d);
        h[4] = h[4].wrapping_add(e);
    }
    let mut out = [0u8; 20];
    for i in 0..5 {
        out[i * 4..i * 4 + 4].copy_from_slice(&h[i].to_be_bytes());
    }
    out
}

const B64: &[u8] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";

fn base64(data: &[u8]) -> String {
    let mut out = String::new();
    for chunk in data.chunks(3) {
        let b = [
            chunk[0],
            chunk.get(1).copied().unwrap_or(0),
            chunk.get(2).copied().unwrap_or(0),
        ];
        let n = ((b[0] as u32) << 16) | ((b[1] as u32) << 8) | b[2] as u32;
        out.push(B64[(n >> 18) as usize & 63] as char);
        out.push(B64[(n >> 12) as usize & 63] as char);
        out.push(if chunk.len() > 1 { B64[(n >> 6) as usize & 63] as char } else { '=' });
        out.push(if chunk.len() > 2 { B64[n as usize & 63] as char } else { '=' });
    }
    out
}

// ---------- transport ----------

/// Write half of the connection.
enum WriteSink {
    Tcp(TcpStream),
    Pipe(ChildStdin),
}

impl WriteSink {
    fn write_all(&mut self, buf: &[u8]) -> std::io::Result<()> {
        match self {
            WriteSink::Tcp(s) => s.write_all(buf),
            WriteSink::Pipe(p) => p.write_all(buf),
        }
    }
}

/// Read half of the connection.
enum ReadSink {
    Tcp(TcpStream),
    Pipe(ChildStdout),
}

impl ReadSink {
    fn read_exact(&mut self, buf: &mut [u8]) -> std::io::Result<()> {
        match self {
            ReadSink::Tcp(s) => s.read_exact(buf),
            ReadSink::Pipe(p) => p.read_exact(buf),
        }
    }
}

/// Parse wss://host[:port]/path or ws://...
fn parse_url(url: &str) -> Result<(bool, String, u16, String), String> {
    let (tls, rest) = if let Some(r) = url.strip_prefix("wss://") {
        (true, r)
    } else if let Some(r) = url.strip_prefix("ws://") {
        (false, r)
    } else {
        return Err("url must start with ws:// or wss://".into());
    };
    let (hostport, path) = match rest.find('/') {
        Some(i) => (&rest[..i], &rest[i..]),
        None => (rest, "/"),
    };
    let (host, port) = match hostport.rsplit_once(':') {
        Some((h, p)) => (h.to_string(), p.parse::<u16>().map_err(|_| "bad port")?),
        None => (hostport.to_string(), if tls { 443 } else { 80 }),
    };
    Ok((tls, host, port, path.to_string()))
}

fn connect_transport(url: &str) -> Result<(WriteSink, ReadSink, Option<Child>), String> {
    let (tls, host, port, _path) = parse_url(url)?;
    if tls {
        let mut child = Command::new("openssl")
            .args(["s_client", "-quiet", "-verify_quiet", "-no_ign_eof"])
            .arg("-connect")
            .arg(format!("{host}:{port}"))
            .arg("-servername")
            .arg(&host)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()
            .map_err(|e| format!("openssl s_client spawn failed: {e}"))?;
        let stdin = child.stdin.take().ok_or("no stdin")?;
        let stdout = child.stdout.take().ok_or("no stdout")?;
        Ok((WriteSink::Pipe(stdin), ReadSink::Pipe(stdout), Some(child)))
    } else {
        let stream = TcpStream::connect((host.as_str(), port)).map_err(|e| e.to_string())?;
        let read_half = stream.try_clone().map_err(|e| e.to_string())?;
        Ok((WriteSink::Tcp(stream), ReadSink::Tcp(read_half), None))
    }
}

// ---------- frames ----------

fn encode_frame(opcode: u8, payload: &[u8], mask: bool) -> Vec<u8> {
    let mut out = vec![0x80 | opcode]; // FIN + opcode
    let len = payload.len();
    let mask_bit = if mask { 0x80 } else { 0x00 };
    if len < 126 {
        out.push(mask_bit | len as u8);
    } else if len <= 0xffff {
        out.push(mask_bit | 126);
        out.extend_from_slice(&(len as u16).to_be_bytes());
    } else {
        out.push(mask_bit | 127);
        out.extend_from_slice(&(len as u64).to_be_bytes());
    }
    if mask {
        let key: [u8; 4] = rand_bytes();
        out.extend_from_slice(&key);
        for (i, b) in payload.iter().enumerate() {
            out.push(b ^ key[i % 4]);
        }
    } else {
        out.extend_from_slice(payload);
    }
    out
}

fn rand_bytes() -> [u8; 4] {
    // cheap entropy: pid + time (frames only need unpredictability for intermediaries)
    let t = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default();
    let mut seed = t.as_nanos() as u64 ^ (std::process::id() as u64) << 32;
    let mut out = [0u8; 4];
    for b in out.iter_mut() {
        seed = seed.wrapping_mul(6364136223846793005).wrapping_add(1442695040888963407);
        *b = (seed >> 33) as u8;
    }
    out
}

pub enum WsMessage {
    Text(String),
    Binary(Vec<u8>),
    Ping(Vec<u8>),
    Pong(Vec<u8>),
    Close(Option<String>),
}

/// Read one COMPLETE message, assembling fragments (FIN + continuation
/// opcodes; some providers emit continuation chunks as binary frames).
fn read_message(t: &mut ReadSink) -> Result<WsMessage, String> {
    let mut acc: Option<(u8, Vec<u8>)> = None;
    loop {
        let mut hdr = [0u8; 2];
        t.read_exact(&mut hdr).map_err(|e| format!("ws read: {e}"))?;
        let fin = hdr[0] & 0x80 != 0;
        let opcode = hdr[0] & 0x0f;
        let masked = hdr[1] & 0x80 != 0;
        let mut len = (hdr[1] & 0x7f) as usize;
        if len == 126 {
            let mut b = [0u8; 2];
            t.read_exact(&mut b).map_err(|e| e.to_string())?;
            len = u16::from_be_bytes(b) as usize;
        } else if len == 127 {
            let mut b = [0u8; 8];
            t.read_exact(&mut b).map_err(|e| e.to_string())?;
            len = u64::from_be_bytes(b) as usize;
        }
        if len > 64 * 1024 * 1024 {
            return Err("ws frame too large".into());
        }
        let mask_key = if masked {
            let mut k = [0u8; 4];
            t.read_exact(&mut k).map_err(|e| e.to_string())?;
            Some(k)
        } else {
            None
        };
        let mut payload = vec![0u8; len];
        if len > 0 {
            t.read_exact(&mut payload).map_err(|e| e.to_string())?;
        }
        if let Some(k) = mask_key {
            for (i, b) in payload.iter_mut().enumerate() {
                *b ^= k[i % 4];
            }
        }
        match &mut acc {
            None => {
                if opcode == 0x8 {
                    return Ok(WsMessage::Close(if payload.len() >= 2 {
                        Some(String::from_utf8_lossy(&payload[2..]).into_owned())
                    } else {
                        None
                    }));
                }
                if opcode == 0x9 {
                    return Ok(WsMessage::Ping(payload));
                }
                if opcode == 0xA {
                    return Ok(WsMessage::Pong(payload));
                }
                if fin && opcode != 0x0 {
                    return Ok(match opcode {
                        0x1 => WsMessage::Text(String::from_utf8_lossy(&payload).into_owned()),
                        _ => WsMessage::Binary(payload),
                    });
                }
                acc = Some((if opcode == 0x0 { 0x1 } else { opcode }, payload));
            }
            Some((op0, buf)) => {
                buf.extend_from_slice(&payload);
                if fin {
                    let op0 = *op0;
                    let buf = std::mem::take(buf);
                    acc = None;
                    return Ok(match op0 {
                        0x1 => WsMessage::Text(String::from_utf8_lossy(&buf).into_owned()),
                        _ => WsMessage::Binary(buf),
                    });
                }
            }
        }
    }
}

#[allow(dead_code)]
fn read_frame(t: &mut ReadSink) -> Result<WsMessage, String> {
    let mut hdr = [0u8; 2];
    t.read_exact(&mut hdr).map_err(|e| format!("ws read: {e}"))?;
    let fin = hdr[0] & 0x80 != 0;
    let opcode = hdr[0] & 0x0f;
    let masked = hdr[1] & 0x80 != 0;
    let mut len = (hdr[1] & 0x7f) as usize;
    if len == 126 {
        let mut b = [0u8; 2];
        t.read_exact(&mut b).map_err(|e| e.to_string())?;
        len = u16::from_be_bytes(b) as usize;
    } else if len == 127 {
        let mut b = [0u8; 8];
        t.read_exact(&mut b).map_err(|e| e.to_string())?;
        len = u64::from_be_bytes(b) as usize;
    }
    if len > 64 * 1024 * 1024 {
        return Err("ws frame too large".into());
    }
    let mask_key = if masked {
        let mut k = [0u8; 4];
        t.read_exact(&mut k).map_err(|e| e.to_string())?;
        Some(k)
    } else {
        None
    };
    let mut payload = vec![0u8; len];
    if len > 0 {
        t.read_exact(&mut payload).map_err(|e| e.to_string())?;
    }
    if let Some(k) = mask_key {
        for (i, b) in payload.iter_mut().enumerate() {
            *b ^= k[i % 4];
        }
    }
    let _ = fin;
    Ok(match opcode {
        0x1 => WsMessage::Text(String::from_utf8_lossy(&payload).into_owned()),
        0x2 => WsMessage::Binary(payload),
        0x9 => WsMessage::Ping(payload),
        0xA => WsMessage::Pong(payload),
        0x8 => WsMessage::Close(if payload.len() >= 2 {
            Some(String::from_utf8_lossy(&payload[2..]).into_owned())
        } else {
            None
        }),
        _ => WsMessage::Binary(payload), // ignore unknown (incl. continuation for our use)
    })
}

/// A live websocket connection with a background reader thread.
pub struct WsConn {
    writer: std::sync::Arc<std::sync::Mutex<WriteSink>>,
    rx: mpsc::Receiver<WsMessage>,
    _child: Option<Child>,
}

impl WsConn {
    /// Connect + perform the RFC 6455 handshake.
    pub fn connect(url: &str) -> Result<WsConn, String> {
        let (tls, host, port, path) = parse_url(url)?;
        let (mut writer, mut reader, child) = connect_transport(url)?;
        // handshake
        let mut key_raw = [0u8; 16];
        key_raw[..4].copy_from_slice(&rand_bytes());
        let t0 = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap_or_default();
        key_raw[4..].copy_from_slice(&t0.as_nanos().to_le_bytes()[..12]);
        let ws_key = base64(&key_raw);
        let req = format!(
            "GET {path} HTTP/1.1\r\nHost: {host}:{port}\r\nUpgrade: websocket\r\nConnection: Upgrade\r\nSec-WebSocket-Key: {ws_key}\r\nSec-WebSocket-Version: 13\r\nUser-Agent: rrx/0.6\r\n\r\n"
        );
        writer.write_all(req.as_bytes()).map_err(|e| e.to_string())?;
        // read response headers
        let mut buf = Vec::new();
        let mut byte = [0u8; 1];
        loop {
            reader.read_exact(&mut byte).map_err(|e| format!("handshake read: {e}"))?;
            buf.push(byte[0]);
            if buf.ends_with(b"\r\n\r\n") {
                break;
            }
            if buf.len() > 16 * 1024 {
                return Err("handshake response too large".into());
            }
        }
        let headers = String::from_utf8_lossy(&buf);
        if !headers.contains("101") {
            return Err(format!("handshake failed: {}", headers.lines().next().unwrap_or("")));
        }
        // verify Sec-WebSocket-Accept
        let expect = base64(&sha1(format!("{ws_key}258EAFA5-E914-47DA-95CA-C5AB0DC85B11").as_bytes()));
        if !headers.to_lowercase().contains(&expect.to_lowercase()) {
            return Err("Sec-WebSocket-Accept mismatch".into());
        }
        let (tx_msg, rx) = mpsc::channel::<WsMessage>();
        // reader thread owns the read half; pings answered through the writer mutex
        let writer_for_reader = std::sync::Arc::new(std::sync::Mutex::new(writer));
        {
            let writer_for_reader = writer_for_reader.clone();
            std::thread::spawn(move || {
                loop {
                    match read_message(&mut reader) {
                        Ok(WsMessage::Ping(p)) => {
                            if let Ok(mut w) = writer_for_reader.lock() {
                                let _ = w.write_all(&encode_frame(0xA, &p, true));
                            }
                            let _ = tx_msg.send(WsMessage::Ping(p));
                        }
                        Ok(m) => {
                            if matches!(m, WsMessage::Close(_)) {
                                if let Ok(mut w) = writer_for_reader.lock() {
                                    let _ = w.write_all(&encode_frame(0x8, &[], true));
                                }
                                let _ = tx_msg.send(m);
                                break;
                            }
                            if tx_msg.send(m).is_err() {
                                break;
                            }
                        }
                        Err(_) => break,
                    }
                }
            });
        }
        Ok(WsConn { writer: writer_for_reader, rx, _child: child })
    }

    /// Send a text message.
    pub fn send_text(&self, text: &str) -> Result<(), String> {
        let mut w = self.writer.lock().map_err(|_| "writer poisoned")?;
        w.write_all(&encode_frame(0x1, text.as_bytes(), true))
            .map_err(|e| format!("ws write: {e}"))
    }

    /// Receive with timeout; handles pings internally.
    pub fn recv_timeout(&self, timeout: Duration) -> Option<WsMessage> {
        self.rx.recv_timeout(timeout).ok()
    }
}

// ---------- eth_subscribe ----------

/// Default public wss endpoints per chain (publicnode).
pub fn default_wss(chain: &str) -> &'static str {
    match chain {
        "base" => "wss://base-rpc.publicnode.com/websocket",
        "arbitrum" => "wss://arbitrum-one-rpc.publicnode.com/websocket",
        "optimism" => "wss://optimism-rpc.publicnode.com/websocket",
        "polygon" => "wss://polygon-bor-rpc.publicnode.com/websocket",
        "bsc" => "wss://bsc-rpc.publicnode.com/websocket",
        _ => "wss://ethereum-rpc.publicnode.com/websocket",
    }
}

/// Subscribe and collect live notifications.
/// Returns (subscription_id, events) — events are the `params` objects as JSON strings.
pub fn subscribe(
    url: &str,
    method: &str,
    params: serde_json::Value,
    max_events: usize,
    timeout_secs: u64,
) -> Result<(String, Vec<String>), String> {
    let conn = WsConn::connect(url)?;
    let req = serde_json::json!({
        "jsonrpc": "2.0",
        "id": 1,
        "method": "eth_subscribe",
        "params": if params.is_null() { serde_json::json!([method]) } else { serde_json::json!([method, params]) }
    });
    conn.send_text(&req.to_string())?;
    let deadline = Instant::now() + Duration::from_secs(timeout_secs);
    let mut sub_id = String::new();
    let mut events = vec![];
    let mut pending = String::new();
    while Instant::now() < deadline {
        let remain = deadline.saturating_duration_since(Instant::now());
        match conn.recv_timeout(remain.min(Duration::from_millis(500))) {
            Some(WsMessage::Text(t)) => {
                // coalesce fragmented JSON by attempting parse
                pending.push_str(&t);
                let v: serde_json::Result<serde_json::Value> = serde_json::from_str(&pending);
                match v {
                    Ok(v) => {
                        pending.clear();
                        if let Some(id) = v.get("id").and_then(|i| i.as_u64()) {
                            if id == 1 {
                                if let Some(err) = v.get("error") {
                                    return Err(format!("eth_subscribe error: {err}"));
                                }
                                sub_id = v
                                    .get("result")
                                    .and_then(|r| r.as_str())
                                    .unwrap_or("")
                                    .to_string();
                                continue;
                            }
                        }
                        if v.get("method").and_then(|m| m.as_str()) == Some("eth_subscription") {
                            if let Some(params) = v.get("params") {
                                let sid = params.get("subscription").and_then(|s| s.as_str()).unwrap_or("");
                                if sid == sub_id {
                                    if let Some(result) = params.get("result") {
                                        events.push(result.to_string());
                                        if events.len() >= max_events {
                                            return Ok((sub_id, events));
                                        }
                                    }
                                }
                            }
                        }
                    }
                    Err(_) => {
                        // wait for more fragments; if it can't ever parse, timeout handles it
                        if pending.len() > 1024 * 1024 {
                            return Err("unparseable stream".into());
                        }
                    }
                }
            }
            Some(WsMessage::Ping(_)) => continue,
            Some(WsMessage::Close(reason)) => {
                return Err(format!("server closed: {:?}", reason));
            }
            None => continue,
            _ => continue,
        }
    }
    if sub_id.is_empty() {
        return Err("no subscription confirmation within timeout".into());
    }
    Ok((sub_id, events))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sha1_known_vectors() {
        assert_eq!(hex::encode(sha1(b"abc")), "a9993e364706816aba3e25717850c26c9cd0d89d");
        assert_eq!(hex::encode(sha1(b"")), "da39a3ee5e6b4b0d3255bfef95601890afd80709");
        assert_eq!(
            hex::encode(sha1(b"The quick brown fox jumps over the lazy dog")),
            "2fd4e1c67a2d28fced849ee1bb76e7391b93eb12"
        );
    }

    #[test]
    fn base64_known() {
        assert_eq!(base64(b""), "");
        assert_eq!(base64(b"f"), "Zg==");
        assert_eq!(base64(b"fo"), "Zm8=");
        assert_eq!(base64(b"foo"), "Zm9v");
        assert_eq!(base64(b"foobar"), "Zm9vYmFy");
    }

    #[test]
    fn frame_roundtrip() {
        // encode with mask then decode manually
        let payload = b"hello world";
        let frame = encode_frame(0x1, payload, true);
        assert_eq!(frame[0], 0x81);
        assert_eq!(frame[1] & 0x80, 0x80);
        assert_eq!((frame[1] & 0x7f) as usize, payload.len());
        let key = &frame[2..6];
        let masked = &frame[6..];
        let unmasked: Vec<u8> = masked.iter().enumerate().map(|(i, b)| b ^ key[i % 4]).collect();
        assert_eq!(&unmasked, payload);
        // unmasked encode
        let f2 = encode_frame(0x1, payload, false);
        assert_eq!(&f2[2..], payload);
    }

    #[test]
    fn url_parse() {
        let (tls, host, port, path) = parse_url("wss://ethereum-rpc.publicnode.com/websocket").unwrap();
        assert!(tls);
        assert_eq!(host, "ethereum-rpc.publicnode.com");
        assert_eq!(port, 443);
        assert_eq!(path, "/websocket");
        let (tls2, _h, p2, _p) = parse_url("ws://localhost:8546").unwrap();
        assert!(!tls2);
        assert_eq!(p2, 8546);
    }
}
