//! Connection to the brain: a minimal pure-std WebSocket client (RFC 6455)
//! plus the request/reply plumbing over the `romacode-shared` protocol.
//!
//! One TCP connection to the server:
//! * the **reader thread** parses server frames — replies resolve pending
//!   request slots, events flow to the UI through an mpsc channel;
//! * the **writer thread** sends (masked) client frames — client-to-server
//!   frames must be masked per the spec, server frames are not.
//!
//! Requests are synchronous (`Net::request`) with a generous timeout; UI-facing
//! flows background them and drain the result on the next tick so the render
//! loop never freezes on a slow or dead brain.

use romacode_shared::events::Event;
use romacode_shared::protocol::{ClientRequest, Reply, ServerEnvelope, Welcome};
use std::collections::HashMap;
use std::io::{BufRead, BufReader, Read as IoRead, Write as IoWrite};
use std::net::TcpStream;
use std::sync::mpsc::{channel, Receiver, Sender};
use std::sync::{Arc, Mutex};
use std::time::Duration;

pub const DEFAULT_BIND: &str = "127.0.0.1:7712";

/// What the UI loop drains from [`Net::events`]. Besides wire events this
/// carries the req-0 Welcome REFRESH broadcast (settings/provider changes:
/// the server pushes a reply with req 0 — the request matcher can never
/// resolve it, so it is delivered here instead) and protocol drift, which is
/// surfaced instead of being silently swallowed (a dropped reply used to look
/// exactly like a 10-15 s freeze).
pub enum UiEvent {
    Wire(Event),
    Welcome(Welcome),
    Malformed(String),
}

/// Live connection to `romacode-server`.
pub struct Net {
    /// Requests for the writer thread: (seq, json).
    outbox: Sender<(u64, String)>,
    /// Streamed events for the UI loop (behind a mutex so the `Arc<Net>`
    /// can cross into worker threads; the UI drains it every tick).
    pub events: Arc<Mutex<Receiver<UiEvent>>>,
    pending: Arc<Mutex<HashMap<u64, Sender<Result<Reply, String>>>>>,
    next_seq: Arc<Mutex<u64>>,
    /// Bumped by the reader thread when the socket dies; `request` fails
    /// fast on it and `App::poll` drives the reconnect loop.
    pub closed: Arc<std::sync::atomic::AtomicBool>,
}

impl Net {
    /// Connect to `bind` and complete the WS handshake. Returns `Err` with a
    /// human-readable reason (bad address, server not up, no upgrade).
    pub fn connect(bind: &str) -> Result<Net, String> {
        // Bound the connect: an unresponsive address used to hang the caller
        // (the UI thread) for the OS-level SYN timeout (~2 min).
        let addr: std::net::SocketAddr = bind
            .parse()
            .map_err(|_| format!("invalid romacode-server address: {bind}"))?;
        let stream = TcpStream::connect_timeout(&addr, Duration::from_secs(3))
            .map_err(|e| format!("cannot reach romacode-server at {bind}: {e}"))?;
        let _ = stream.set_nodelay(true);

        // --- handshake ---
        let key = ws_key();
        let req = format!(
            "GET /ws HTTP/1.1\r\nHost: {bind}\r\nUpgrade: websocket\r\nConnection: Upgrade\r\nSec-WebSocket-Key: {key}\r\nSec-WebSocket-Version: 13\r\n\r\n"
        );
        let mut w = stream.try_clone().map_err(|e| e.to_string())?;
        w.write_all(req.as_bytes()).map_err(|e| e.to_string())?;
        let mut reader = BufReader::new(stream.try_clone().map_err(|e| e.to_string())?);
        let mut status = String::new();
        reader.read_line(&mut status).map_err(|e| e.to_string())?;
        if !status.contains("101") {
            return Err(format!(
                "romacode-server at {bind} did not upgrade to WebSocket ({})",
                status.trim()
            ));
        }
        loop {
            let mut line = String::new();
            let n = reader.read_line(&mut line).map_err(|e| e.to_string())?;
            if n == 0 || line.trim().is_empty() {
                break;
            }
        }

        let (outbox_tx, outbox_rx) = channel::<(u64, String)>();
        let (event_tx, event_rx) = channel::<UiEvent>();
        let pending: Arc<Mutex<HashMap<u64, Sender<Result<Reply, String>>>>> =
            Arc::new(Mutex::new(HashMap::new()));
        let closed = Arc::new(std::sync::atomic::AtomicBool::new(false));
        // All frame writes (writer thread, reader-thread PONGs, the liveness
        // pinger) go through one mutex: interleaved partial frames corrupt
        // the stream.
        let write_lock = Arc::new(Mutex::new(()));

        // --- writer thread (masked client frames) ---
        {
            let mut stream = stream.try_clone().map_err(|e| e.to_string())?;
            let closed = closed.clone();
            let write_lock = write_lock.clone();
            std::thread::Builder::new()
                .name("ws-client-writer".into())
                .spawn(move || {
                    while let Ok((seq, body)) = outbox_rx.recv() {
                        // Envelope: {"req": seq, "body": <already JSON object>}
                        let text = format!("{{\"req\":{seq},\"body\":{body}}}");
                        let guard = write_lock.lock().unwrap_or_else(|e| e.into_inner());
                        let sent = client_send_text(&mut stream, &text);
                        drop(guard);
                        if sent.is_err() {
                            closed.store(true, std::sync::atomic::Ordering::Relaxed);
                            break;
                        }
                    }
                })
                .ok();
        }

        // --- liveness pinger: a client PING every 30 s. A half-open TCP
        // connection never errors on its own; the first failed PING write
        // flips `closed` so `App::poll` starts the reconnect loop.
        {
            let mut stream = stream.try_clone().map_err(|e| e.to_string())?;
            let closed = closed.clone();
            let write_lock = write_lock.clone();
            std::thread::Builder::new()
                .name("ws-client-pinger".into())
                .spawn(move || {
                    while !closed.load(std::sync::atomic::Ordering::Relaxed) {
                        std::thread::sleep(Duration::from_secs(30));
                        if closed.load(std::sync::atomic::Ordering::Relaxed) {
                            break;
                        }
                        let guard = write_lock.lock().unwrap_or_else(|e| e.into_inner());
                        let sent = client_send_frame(&mut stream, 0x9, b"romacode-ping");
                        drop(guard);
                        if sent.is_err() {
                            closed.store(true, std::sync::atomic::Ordering::Relaxed);
                            break;
                        }
                    }
                })
                .ok();
        }

        // --- reader thread ---
        {
            let pending = pending.clone();
            let closed = closed.clone();
            let write_lock = write_lock.clone();
            let mut stream = stream;
            std::thread::Builder::new()
                .name("ws-client-reader".into())
                .spawn(move || {
                    let mut buf = Vec::new();
                    loop {
                        match client_read_frame(&mut reader, &mut stream, &write_lock, &mut buf) {
                            Ok(None) => continue,
                            Ok(Some(text)) => {
                                let env: ServerEnvelope =
                                    match serde_json::from_str(&text) {
                                        Ok(e) => e,
                                        Err(e) => {
                                            // Undecodable envelope: surface it
                                            // instead of silently swallowing
                                            // (version drift used to degrade
                                            // into mysterious freezes).
                                            let _ = event_tx.send(UiEvent::Malformed(
                                                format!("malformed server envelope: {e}"),
                                            ));
                                            continue;
                                        }
                                    };
                                if let Some(req) = env.req {
                                    match parse_reply(&env) {
                                        Some(answer) => {
                                            let slot = pending
                                                .lock()
                                                .unwrap_or_else(|e| e.into_inner())
                                                .remove(&req);
                                            if let Some(tx) = slot {
                                                let _ = tx.send(answer);
                                                continue;
                                            }
                                            if req == 0 {
                                                // req-0 push: the Welcome
                                                // refresh broadcast.
                                                if let Some(Reply::Welcome(w)) =
                                                    answer.ok()
                                                {
                                                    if event_tx
                                                        .send(UiEvent::Welcome(w))
                                                        .is_err()
                                                    {
                                                        break;
                                                    }
                                                }
                                                continue;
                                            }
                                        }
                                        None => {
                                            // Reply-shaped envelope that could
                                            // not be parsed into a known Reply.
                                            let unresolved = pending
                                                .lock()
                                                .unwrap_or_else(|e| e.into_inner())
                                                .remove(&req)
                                                .map(|tx| {
                                                    let _ = tx.send(Err(
                                                        "malformed reply from romacode-server (screen/brain version mismatch?)"
                                                            .into(),
                                                    ));
                                                });
                                            if unresolved.is_some() {
                                                continue;
                                            }
                                            let _ = event_tx.send(UiEvent::Malformed(
                                                "server sent an unparseable reply (screen/brain version mismatch?)".into(),
                                            ));
                                            continue;
                                        }
                                    }
                                }
                                if let Some(ev) = env.event {
                                    if event_tx.send(UiEvent::Wire(ev)).is_err() {
                                        break;
                                    }
                                }
                            }
                            Err(_) => {
                                closed.store(true, std::sync::atomic::Ordering::Relaxed);
                                // Fail every in-flight request immediately:
                                // without this they each burned their full
                                // timeout after the connection died.
                                let mut map =
                                    pending.lock().unwrap_or_else(|e| e.into_inner());
                                for (_, tx) in map.drain() {
                                    let _ = tx.send(Err(
                                        "connection to romacode-server was lost".into(),
                                    ));
                                }
                                break;
                            }
                        }
                    }
                })
                .ok();
        }

        Ok(Net {
            outbox: outbox_tx,
            events: Arc::new(Mutex::new(event_rx)),
            pending,
            next_seq: Arc::new(Mutex::new(1)),
            closed,
        })
    }

    fn seq(&self) -> u64 {
        let mut n = self.next_seq.lock().unwrap_or_else(|e| e.into_inner());
        *n += 1;
        *n
    }

    /// Send a request and wait for its reply. Fails immediately once the
    /// connection is known-dead (the old code always burned the full timeout
    /// after the socket died — every keypress froze the TUI for 15 s). While
    /// waiting, the closed flag is checked every 250 ms so a mid-request
    /// disconnect also fails fast.
    pub fn request(&self, body: ClientRequest, timeout: Duration) -> Result<Reply, String> {
        if self.closed.load(std::sync::atomic::Ordering::Relaxed) {
            return Err("connection to romacode-server is closed".into());
        }
        let seq = self.seq();
        let (tx, rx) = channel();
        self.pending
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .insert(seq, tx);
        let json = serde_json::to_string(&body).map_err(|e| e.to_string())?;
        if self.outbox.send((seq, json)).is_err() {
            self.pending
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .remove(&seq);
            return Err("connection to romacode-server is closed".into());
        }
        let deadline = std::time::Instant::now() + timeout;
        loop {
            match rx.recv_timeout(Duration::from_millis(250)) {
                Ok(answer) => return answer,
                Err(std::sync::mpsc::RecvTimeoutError::Timeout) => {
                    if self.closed.load(std::sync::atomic::Ordering::Relaxed) {
                        self.pending
                            .lock()
                            .unwrap_or_else(|e| e.into_inner())
                            .remove(&seq);
                        return Err("connection to romacode-server was lost".into());
                    }
                    if std::time::Instant::now() >= deadline {
                        self.pending
                            .lock()
                            .unwrap_or_else(|e| e.into_inner())
                            .remove(&seq);
                        return Err(format!(
                            "no reply from romacode-server within {}s",
                            timeout.as_secs()
                        ));
                    }
                }
                Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => {
                    self.pending
                        .lock()
                        .unwrap_or_else(|e| e.into_inner())
                        .remove(&seq);
                    return Err("connection to romacode-server was lost".into());
                }
            }
        }
    }

    /// Fire-and-forget (device input, pings): no reply expected by the caller.
    pub fn fire(&self, body: ClientRequest) {
        let seq = self.seq();
        if let Ok(json) = serde_json::to_string(&body) {
            let _ = self.outbox.send((seq, json));
        }
    }

    pub fn hello(&self, version: &str) -> Result<Reply, String> {
        self.request(
            ClientRequest::Hello {
                version: version.to_string(),
            },
            Duration::from_secs(10),
        )
    }
}

/// Reply envelopes resolve to `Ok(Reply)`; error envelopes to `Err(message)`.
fn parse_reply(env: &ServerEnvelope) -> Option<Result<Reply, String>> {
    if let Some(err) = &env.error {
        return Some(Err(err.clone()));
    }
    let v = env.reply.as_ref()?;
    if v.get("ok")?.as_bool()? {
        serde_json::from_value::<Reply>(v.get("data")?.clone())
            .ok()
            .map(Ok)
    } else {
        Some(Err(
            v.get("error")
                .and_then(|e| e.as_str())
                .unwrap_or("request failed")
                .to_string(),
        ))
    }
}

// ---------------------------------------------------------------------------
// WS client framing.
// ---------------------------------------------------------------------------

/// 16 random bytes, base64 — the masking key for the handshake. Uses the OS
/// randomness via /dev/urandom with a time+pid fallback.
fn ws_key() -> String {
    let mut bytes = [0u8; 16];
    if let Ok(mut f) = std::fs::File::open("/dev/urandom") {
        use std::io::Read;
        let _ = f.read_exact(&mut bytes);
    } else {
        let t = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_nanos() as u64
            ^ (std::process::id() as u64) << 32;
        bytes
            .iter_mut()
            .enumerate()
            .for_each(|(i, b)| *b = (t >> (i % 8 * 8)) as u8 ^ (i as u8 * 31));
    }
    b64(&bytes)
}

fn b64(data: &[u8]) -> String {
    const T: &[u8] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = String::with_capacity((data.len() + 2) / 3 * 4);
    for chunk in data.chunks(3) {
        let b = [
            chunk[0],
            chunk.get(1).copied().unwrap_or(0),
            chunk.get(2).copied().unwrap_or(0),
        ];
        let n = ((b[0] as u32) << 16) | ((b[1] as u32) << 8) | b[2] as u32;
        out.push(T[(n >> 18) as usize & 63] as char);
        out.push(T[(n >> 12) as usize & 63] as char);
        out.push(if chunk.len() > 1 {
            T[(n >> 6) as usize & 63] as char
        } else {
            '='
        });
        out.push(if chunk.len() > 2 {
            T[n as usize & 63] as char
        } else {
            '='
        });
    }
    out
}

fn client_send_frame(stream: &mut TcpStream, opcode: u8, payload: &[u8]) -> std::io::Result<()> {
    let len = payload.len();
    let mut mask = [0u8; 4];
    if let Ok(mut f) = std::fs::File::open("/dev/urandom") {
        use std::io::Read;
        let _ = f.read_exact(&mut mask);
    } else {
        let t = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_nanos() as u64
            ^ (std::process::id() as u64) << 32;
        mask.iter_mut().enumerate().for_each(|(i, b)| {
            *b = (t >> (i % 8 * 8)) as u8 ^ (i as u8 * 17)
        });
    }
    let mut header: Vec<u8> = vec![opcode];
    if len < 126 {
        header.push(0x80 | len as u8);
    } else if len < 65536 {
        header.push(0x80 | 126);
        header.extend_from_slice(&(len as u16).to_be_bytes());
    } else {
        header.push(0x80 | 127);
        header.extend_from_slice(&(len as u64).to_be_bytes());
    }
    header.extend_from_slice(&mask);
    stream.write_all(&header)?;
    let masked: Vec<u8> = payload
        .iter()
        .enumerate()
        .map(|(i, b)| b ^ mask[i % 4])
        .collect();
    stream.write_all(&masked)
}

fn client_send_text(stream: &mut TcpStream, text: &str) -> std::io::Result<()> {
    client_send_frame(stream, 0x81, text.as_bytes())
}

#[allow(dead_code)] // framing completeness; the reader maps opcodes directly
enum CFrame {
    Text(String),
    Close,
    Ignore,
}

fn client_read_frame(
    reader: &mut BufReader<TcpStream>,
    stream: &mut TcpStream,
    write_lock: &Mutex<()>,
    buf: &mut Vec<u8>,
) -> std::io::Result<Option<String>> {
    let mut hdr = [0u8; 2];
    reader.read_exact(&mut hdr)?;
    let fin = hdr[0] & 0x80 != 0;
    let opcode = hdr[0] & 0x0f;
    let mut len = (hdr[1] & 0x7f) as u64;
    if len == 126 {
        let mut ext = [0u8; 2];
        reader.read_exact(&mut ext)?;
        len = u16::from_be_bytes(ext) as u64;
    } else if len == 127 {
        let mut ext = [0u8; 8];
        reader.read_exact(&mut ext)?;
        len = u64::from_be_bytes(ext);
    }
    if len > 64 * 1024 * 1024 {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            "frame too large",
        ));
    }
    let mut payload = vec![0u8; len as usize];
    reader.read_exact(&mut payload)?;
    match opcode {
        0x0 | 0x1 | 0x2 => {
            // 0x0 = continuation frame: fragment payloads accumulate until the
            // FIN bit. The old code ignored continuations entirely — fragmented
            // messages were lost and the buffer leaked.
            buf.extend_from_slice(&payload);
            if fin {
                let out = String::from_utf8_lossy(buf).into_owned();
                buf.clear();
                Ok(Some(out))
            } else {
                Ok(None)
            }
        }
        0x8 => Err(std::io::Error::new(
            std::io::ErrorKind::ConnectionAborted,
            "server closed",
        )),
        0x9 => {
            // Server PING: RFC 6455 requires a PONG with the same payload —
            // a server may drop a client that never answers. The PONG is
            // written under the shared write mutex so it cannot interleave
            // with the writer thread's frames.
            {
                let guard = write_lock.lock().unwrap_or_else(|e| e.into_inner());
                let _ = client_send_frame(stream, 0x8a, &payload);
                drop(guard);
            }
            Ok(None)
        }
        _ => {
            // PONG and any other unhandled control opcode carry no JSON
            // message — skip them instead of feeding an empty string to the
            // envelope parser (which used to surface a bogus "malformed").
            let _ = CFrame::Ignore;
            Ok(None)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use romacode_shared::protocol::ClientEnvelope;

    #[test]
    fn b64_matches_known_vectors() {
        assert_eq!(b64(b"foobar"), "Zm9vYmFy");
        assert_eq!(b64(b"f"), "Zg==");
    }

    #[test]
    fn ws_key_shape() {
        let k = ws_key();
        assert_eq!(k.len(), 24, "{k}");
        assert!(k.ends_with("=="));
    }

    #[test]
    fn envelope_json_shape() {
        // the writer thread builds {"req":N,"body":<json object>}
        let body = serde_json::to_string(&ClientRequest::Ping).unwrap();
        let text = format!("{{\"req\":7,\"body\":{body}}}");
        let env: ClientEnvelope = serde_json::from_str(&text).unwrap();
        assert_eq!(env.req, 7);
        assert!(matches!(env.body, ClientRequest::Ping));
    }
}

// ---------------------------------------------------------------------------
// In-process dummy server for `--snapshot` (dev headless renders): speaks the
// same protocol on an ephemeral port, answers `hello` with a fixed Welcome
// and nothing else. No brain needed to render UI snapshots.
// ---------------------------------------------------------------------------

pub fn spawn_dummy_server(welcome_json: String) -> Result<String, String> {
    let listener = std::net::TcpListener::bind("127.0.0.1:0")
        .map_err(|e| format!("dummy server: {e}"))?;
    let addr = listener
        .local_addr()
        .map_err(|e| format!("dummy server: {e}"))?
        .to_string();
    std::thread::Builder::new()
        .name("ws-dummy".into())
        .spawn(move || {
            let welcome = welcome_json;
            for stream in listener.incoming().flatten() {
                let _ = stream.set_nodelay(true);
                let mut reader = BufReader::new(match stream.try_clone() {
                    Ok(s) => s,
                    Err(_) => continue,
                });
                let mut line = String::new();
                if reader.read_line(&mut line).is_err() {
                    continue;
                }
                let mut key = String::new();
                loop {
                    let mut h = String::new();
                    if reader.read_line(&mut h).unwrap_or(0) == 0 {
                        break;
                    }
                    if h.trim().is_empty() {
                        break;
                    }
                    if let Some((k, v)) = h.split_once(':') {
                        if k.eq_ignore_ascii_case("sec-websocket-key") {
                            key = v.trim().to_string();
                        }
                    }
                }
                if key.is_empty() {
                    continue;
                }
                // reuse the server crate's handshake math via RFC 6455 example:
                // compute accept = b64(sha1(key + GUID)) — inline SHA-1 (small).
                let accept = sha1_accept(&key);
                let resp = format!(
                    "HTTP/1.1 101 Switching Protocols\r\nUpgrade: websocket\r\nConnection: Upgrade\r\nSec-WebSocket-Accept: {accept}\r\n\r\n"
                );
                if stream
                    .try_clone()
                    .and_then(|mut s| s.write_all(resp.as_bytes()))
                    .is_err()
                {
                    continue;
                }
                let mut stream = stream;
                // Serve frames until the client goes away.
                let write_lock = Mutex::new(());
                loop {
                    match client_read_frame(&mut reader, &mut stream, &write_lock, &mut Vec::new())
                    {
                        Ok(None) => continue,
                        Ok(Some(text)) => {
                            // treat any request as hello; reply with the
                            // request's OWN req id — the client sequences start
                            // at 2, so the old hardcoded req:1 reply never
                            // resolved any pending request.
                            let req: u64 = serde_json::from_str::<serde_json::Value>(&text)
                                .ok()
                                .and_then(|v| v.get("req").and_then(|r| r.as_u64()))
                                .unwrap_or(1);
                            let env = format!(
                                "{{\"req\":{req},\"reply\":{{\"ok\":true,\"data\":{welcome}}}}}"
                            );
                            if client_send_text(&mut stream, &env).is_err() {
                                break;
                            }
                        }
                        Err(_) => break,
                    }
                }
            }
        })
        .ok();
    Ok(addr)
}

/// Minimal SHA-1 for the dummy server handshake (same algorithm as the real
/// server; kept local to avoid a client→server dependency).
fn sha1_accept(key: &str) -> String {
    const GUID: &str = "258EAFA5-E914-47DA-95CA-C5AB0DC85B11";
    let mut input = key.trim().as_bytes().to_vec();
    input.extend_from_slice(GUID.as_bytes());
    let mut h: [u32; 5] = [0x67452301, 0xEFCDAB89, 0x98BADCFE, 0x10325476, 0xC3D2E1F0];
    let ml = (input.len() as u64) * 8;
    let mut msg = input;
    msg.push(0x80);
    while msg.len() % 64 != 56 {
        msg.push(0);
    }
    msg.extend_from_slice(&ml.to_be_bytes());
    for chunk in msg.chunks(64) {
        let mut w = [0u32; 80];
        for (i, word) in chunk.chunks(4).enumerate() {
            w[i] = u32::from_be_bytes([word[0], word[1], word[2], word[3]]);
        }
        for i in 16..80 {
            w[i] = (w[i - 3] ^ w[i - 8] ^ w[i - 14] ^ w[i - 16]).rotate_left(1);
        }
        let (mut a, mut b, mut c, mut d, mut e) = (h[0], h[1], h[2], h[3], h[4]);
        for (i, wi) in w.iter().enumerate() {
            let (f, k) = match i {
                0..=19 => ((b & c) | ((!b) & d), 0x5A827999u32),
                20..=39 => (b ^ c ^ d, 0x6ED9EBA1),
                40..=59 => ((b & c) | (b & d) | (c & d), 0x8F1BBCDC),
                _ => (b ^ c ^ d, 0xCA62C1D6),
            };
            let temp = a
                .rotate_left(5)
                .wrapping_add(f)
                .wrapping_add(k)
                .wrapping_add(*wi)
                .wrapping_add(e);
            e = d;
            d = c;
            c = b.rotate_left(30);
            b = a;
            a = temp;
        }
        h[0] = h[0].wrapping_add(a);
        h[1] = h[1].wrapping_add(b);
        h[2] = h[2].wrapping_add(c);
        h[3] = h[3].wrapping_add(d);
        h[4] = h[4].wrapping_add(e);
    }
    let mut out = Vec::with_capacity(20);
    for v in h.iter() {
        out.extend_from_slice(&v.to_be_bytes());
    }
    b64(&out)
}
