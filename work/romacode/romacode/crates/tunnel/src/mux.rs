//! Multiplexer over a WebSocket connection.
//!
//! Each WebSocket connection carries multiple independent TCP sessions.
//! A session is a stream of bytes flowing both ways inside a single
//! shared transport.
//!
//! Wire format per WS binary frame (big-endian):
//!   [u8 type][u64 session-id][u16 be payload-len][payload-bytes]
//! The 2-byte payload length caps a single frame at 65535 bytes —
//! large writes are split, just like SSH channel data.
//!
//! Message types:
//!   0x01 Open  payload = addr-port-info (see below)
//!   0x02 Data  payload = raw stream bytes
//!   0x03 Close no payload
//!   0x04 Ping  payload = opaque (echoed in Pong)
//!   0x05 Pong  payload = opaque
//!
//! Open payload format:
//!   [u8 addr_type][addr bytes][u16 be port]
//!   addr_type 0=ipv4 (4B), 1=ipv6 (16B), 2=hostname (u8 len + N bytes)
//! The Open message is sent by whichever side initiates a session:
//!   - For forward remotes, the *client* sends Open when it accepts a
//!     local TCP connection, asking the server to dial.
//!   - For reverse remotes, the *server* sends Open when it accepts a
//!     TCP connection, asking the client to dial.

use std::collections::HashMap;
use std::io::{Read, Write};
use std::net::{TcpStream, ToSocketAddrs};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use crate::log;
use crate::ws::{Frame, OpCode};

pub const MSG_OPEN: u8 = 0x01;
pub const MSG_DATA: u8 = 0x02;
pub const MSG_CLOSE: u8 = 0x03;
pub const MSG_PING: u8 = 0x04;
pub const MSG_PONG: u8 = 0x05;
pub const MSG_SETUP: u8 = 0x06;

const MAX_PAYLOAD: usize = 32 * 1024;

#[derive(Debug, Clone)]
pub struct AddrTarget {
    pub host: String,
    pub port: u16,
}

impl AddrTarget {
    pub fn encode(&self) -> Vec<u8> {
        // Try ipv4
        if let Ok(b) = self.host.parse::<std::net::Ipv4Addr>() {
            let mut v = Vec::with_capacity(4 + 2);
            v.push(0u8);
            v.extend_from_slice(&b.octets());
            v.extend_from_slice(&self.port.to_be_bytes());
            return v;
        }
        if let Ok(b) = self.host.parse::<std::net::Ipv6Addr>() {
            let mut v = Vec::with_capacity(16 + 2);
            v.push(1u8);
            v.extend_from_slice(&b.octets());
            v.extend_from_slice(&self.port.to_be_bytes());
            return v;
        }
        // hostname
        let bytes = self.host.as_bytes();
        let mut v = Vec::with_capacity(1 + 1 + bytes.len() + 2);
        v.push(2u8);
        v.push(bytes.len() as u8);
        v.extend_from_slice(bytes);
        v.extend_from_slice(&self.port.to_be_bytes());
        v
    }

    pub fn decode(buf: &[u8]) -> Result<AddrTarget, String> {
        if buf.is_empty() {
            return Err("empty Open payload".into());
        }
        let t = buf[0];
        match t {
            0 => {
                if buf.len() != 1 + 4 + 2 {
                    return Err("bad ipv4 Open".into());
                }
                let mut o = [0u8; 4];
                o.copy_from_slice(&buf[1..5]);
                let port = u16::from_be_bytes([buf[5], buf[6]]);
                Ok(AddrTarget {
                    host: std::net::Ipv4Addr::from(o).to_string(),
                    port,
                })
            }
            1 => {
                if buf.len() != 1 + 16 + 2 {
                    return Err("bad ipv6 Open".into());
                }
                let mut o = [0u8; 16];
                o.copy_from_slice(&buf[1..17]);
                let port = u16::from_be_bytes([buf[17], buf[18]]);
                Ok(AddrTarget {
                    host: std::net::Ipv6Addr::from(o).to_string(),
                    port,
                })
            }
            2 => {
                if buf.len() < 3 {
                    return Err("bad hostname Open".into());
                }
                let l = buf[1] as usize;
                if buf.len() != 1 + 1 + l + 2 {
                    return Err("hostname Open length mismatch".into());
                }
                let host = String::from_utf8_lossy(&buf[2..2 + l]).to_string();
                let port = u16::from_be_bytes([buf[2 + l], buf[2 + l + 1]]);
                Ok(AddrTarget { host, port })
            }
            _ => Err(format!("unknown addr_type {}", t)),
        }
    }
}

pub struct Mux {
    pub stream: Arc<Mutex<TcpStream>>,
    pub sessions: Arc<Mutex<HashMap<u64, Arc<Session>>>>,
    pub next_id: AtomicU64,
    pub closed: AtomicBool,
    pub is_client: bool, // masking direction
    pub tag: String,
}

pub struct Session {
    pub id: u64,
    pub local: Option<TcpStream>,
    pub target: AddrTarget,
    pub closed: AtomicBool,
}

/// A reverse-remote descriptor exchanged in the Setup message.
/// proto is `0`=tcp, `1`=udp.
#[derive(Debug, Clone)]
pub struct SetupRemote {
    pub local_host: String,
    pub local_port: u16,
    pub remote_host: String,
    pub remote_port: u16,
    pub proto: u8,
}

impl Mux {
    pub fn new(stream: TcpStream, is_client: bool, tag: String) -> Arc<Mux> {
        let _ = stream.set_nodelay(true);
        Arc::new(Mux {
            stream: Arc::new(Mutex::new(stream)),
            sessions: Arc::new(Mutex::new(HashMap::new())),
            // Direction-aware ID space: client-allocated IDs are even,
            // server-allocated IDs are odd. The peer uses the opposite
            // parity, so a reverse Open from the peer can never collide
            // with a locally allocated session in `sessions`.
            next_id: AtomicU64::new(if is_client { 2 } else { 1 }),
            closed: AtomicBool::new(false),
            is_client,
            tag,
        })
    }

    /// Allocate a session ID with this side's parity (client even,
    /// server odd). `open_session` and the reverse listeners both go
    /// through here so local allocation never produces a peer parity ID.
    pub fn alloc_id(self: &Arc<Self>) -> u64 {
        let raw = self.next_id.fetch_add(2, Ordering::SeqCst);
        let want_odd = !self.is_client;
        if (raw % 2 == 1) != want_odd {
            raw + 1
        } else {
            raw
        }
    }

    /// Send an Open message and spawn the session pipe.
    pub fn open_session(self: &Arc<Self>, target: AddrTarget, local: TcpStream) -> u64 {
        let id = self.alloc_id();
        let sess = Arc::new(Session {
            id,
            local: Some(local),
            target: target.clone(),
            closed: AtomicBool::new(false),
        });
        self.sessions.lock().unwrap().insert(id, sess.clone());

        let open = encode_msg(MSG_OPEN, id, &target.encode());
        if let Err(e) = self.send_raw(&open) {
            log::err(&self.tag, &format!("open send: {}", e));
            self.close_session(id);
            return id;
        }

        let mux = self.clone();
        let tag = self.tag.clone();
        std::thread::spawn(move || {
            mux.pipe_local_to_remote(id, sess);
            let _ = &tag;
        });
        id
    }

    /// Spawn an Open for a session that has no local stream yet — used
    /// by the *server* when it gets a TCP connection for a reverse
    /// remote and needs to ask the client to dial.
    pub fn open_session_dial_later(
        self: &Arc<Self>,
        id: u64,
        target: AddrTarget,
    ) -> Result<(), String> {
        let open = encode_msg(MSG_OPEN, id, &target.encode());
        self.send_raw(&open)?;
        let sess = Arc::new(Session {
            id,
            local: None,
            target: target.clone(),
            closed: AtomicBool::new(false),
        });
        self.sessions.lock().unwrap().insert(id, sess);
        Ok(())
    }

    pub fn pipe_local_to_remote(self: Arc<Self>, id: u64, sess: Arc<Session>) {
        let mut buf = [0u8; MAX_PAYLOAD];
        loop {
            if sess.closed.load(Ordering::SeqCst) || self.closed.load(Ordering::SeqCst) {
                break;
            }
            // Read::read is implemented for &TcpStream, so we can borrow
            // the local stream out of the session and read it directly.
            let local_ref = match sess.local.as_ref() {
                Some(s) => s,
                None => break,
            };
            let n = match (&*local_ref).read(&mut buf) {
                Ok(0) => break,
                Ok(n) => n,
                Err(e) if e.kind() == std::io::ErrorKind::Interrupted => continue,
                Err(_) => break,
            };
            let data = encode_msg(MSG_DATA, id, &buf[..n]);
            if let Err(e) = self.send_raw(&data) {
                log::debug(&self.tag, &format!("session {} data send err: {}", id, e));
                break;
            }
        }
        // Tell remote we're done.
        let _ = self.send_raw(&encode_msg(MSG_CLOSE, id, &[]));
        self.close_session(id);
    }

    pub fn send_raw(self: &Arc<Self>, bytes: &[u8]) -> Result<(), String> {
        let mut s = self.stream.lock().unwrap();
        let frame = Frame {
            fin: true,
            op: OpCode::Binary,
            payload: bytes.to_vec(),
        };
        Frame::write(&mut *s, &frame, self.is_client)
    }

    pub fn send_setup(self: &Arc<Self>, remotes: &[SetupRemote]) -> Result<(), String> {
        let mut payload = Vec::new();
        for r in remotes {
            let mut v = Vec::with_capacity(1 + r.local_host.len() + 2 + 2 + 1 + r.remote_host.len() + 2);
            // local_host as hostname-encoded, then local_port, remote_host, remote_port, proto
            let lh = r.local_host.as_bytes();
            v.push(2u8);
            v.push(lh.len() as u8);
            v.extend_from_slice(lh);
            v.extend_from_slice(&r.local_port.to_be_bytes());
            let rh = r.remote_host.as_bytes();
            v.push(2u8);
            v.push(rh.len() as u8);
            v.extend_from_slice(rh);
            v.extend_from_slice(&r.remote_port.to_be_bytes());
            v.push(r.proto);
            payload.extend_from_slice(&v);
        }
        let msg = encode_msg(MSG_SETUP, 0, &payload);
        self.send_raw(&msg)
    }

    /// Decode a Setup payload back into a list of remote descriptors.
    pub fn decode_setup(buf: &[u8]) -> Result<Vec<SetupRemote>, String> {
        let mut out = Vec::new();
        let mut i = 0;
        while i < buf.len() {
            if buf[i] != 2 {
                return Err(format!("bad local addr_type {} in Setup", buf[i]));
            }
            i += 1;
            if i >= buf.len() { return Err("truncated Setup (local len)".into()); }
            let lh_len = buf[i] as usize;
            i += 1;
            if i + lh_len > buf.len() { return Err("truncated Setup (local)".into()); }
            let local_host = String::from_utf8_lossy(&buf[i..i + lh_len]).to_string();
            i += lh_len;
            if i + 2 > buf.len() { return Err("truncated Setup (local port)".into()); }
            let local_port = u16::from_be_bytes([buf[i], buf[i + 1]]);
            i += 2;
            if i >= buf.len() || buf[i] != 2 {
                return Err("bad remote addr_type in Setup".into());
            }
            i += 1;
            if i >= buf.len() { return Err("truncated Setup (remote len)".into()); }
            let rh_len = buf[i] as usize;
            i += 1;
            if i + rh_len > buf.len() { return Err("truncated Setup (remote)".into()); }
            let remote_host = String::from_utf8_lossy(&buf[i..i + rh_len]).to_string();
            i += rh_len;
            if i + 2 > buf.len() { return Err("truncated Setup (remote port)".into()); }
            let remote_port = u16::from_be_bytes([buf[i], buf[i + 1]]);
            i += 2;
            if i >= buf.len() { return Err("truncated Setup (proto)".into()); }
            let proto = buf[i];
            i += 1;
            out.push(SetupRemote {
                local_host,
                local_port,
                remote_host,
                remote_port,
                proto,
            });
        }
        Ok(out)
    }

    pub fn send_ping(self: &Arc<Self>, payload: &[u8]) -> Result<(), String> {
        let mut s = self.stream.lock().unwrap();
        let frame = Frame {
            fin: true,
            op: OpCode::Ping,
            payload: payload.to_vec(),
        };
        Frame::write(&mut *s, &frame, self.is_client)
    }

    pub fn close_session(&self, id: u64) {
        let mut map = self.sessions.lock().unwrap();
        if let Some(s) = map.remove(&id) {
            s.closed.store(true, Ordering::SeqCst);
        }
    }

    pub fn is_closed(&self) -> bool {
        self.closed.load(Ordering::SeqCst)
    }

    pub fn shutdown(&self) {
        self.closed.store(true, Ordering::SeqCst);
        let s = self.stream.lock().unwrap();
        let _ = s.shutdown(std::net::Shutdown::Both);
    }

    /// Main read loop: dispatch WS frames to sessions.
    /// On receipt of an Open message, calls `on_open(target, id)`.
    /// On receipt of a Setup message (reverse remotes), calls
    /// `on_setup(remotes)`. The returned Option<TcpStream> is the local
    /// connection to use for piping data back to the remote (None if the
    /// dial failed).
    pub fn run_loop<F, G>(self: Arc<Self>, mut on_open: F, mut on_setup: G)
    where
        F: FnMut(AddrTarget, u64) -> Option<TcpStream>,
        G: FnMut(Vec<SetupRemote>),
    {
        let mut stream_clone = match self.stream.lock() {
            Ok(g) => g.try_clone().expect("clone"),
            Err(_) => {
                // Mutex poisoned — transport state is unrecoverable.
                self.closed.store(true, Ordering::SeqCst);
                return;
            }
        };
        let tag = self.tag.clone();
        loop {
            if self.closed.load(Ordering::SeqCst) {
                break;
            }
            let frame = match Frame::read(&mut stream_clone) {
                Ok(f) => f,
                Err(e) => {
                    if !self.closed.load(Ordering::SeqCst) {
                        log::info(&tag, &format!("read loop end: {}", e));
                    }
                    break;
                }
            };
            match frame.op {
                OpCode::Binary => {
                    if frame.payload.is_empty() {
                        continue;
                    }
                    let mtype = frame.payload[0];
                    if frame.payload.len() < 9 {
                        log::err(&tag, "frame too short");
                        continue;
                    }
                    let id = u64::from_be_bytes(frame.payload[1..9].try_into().unwrap());
                    let plen = u16::from_be_bytes([
                        frame.payload[9],
                        frame.payload[10],
                    ]) as usize;
                    if frame.payload.len() != 11 + plen {
                        log::err(&tag, &format!("frame length mismatch ({} != {})", frame.payload.len(), 11+plen));
                        continue;
                    }
                    let payload = &frame.payload[11..11 + plen];
                    match mtype {
                        MSG_OPEN => {
                            let target = match AddrTarget::decode(payload) {
                                Ok(t) => t,
                                Err(e) => {
                                    log::err(&tag, &format!("Open decode: {}", e));
                                    let _ = self.send_raw(&encode_msg(MSG_CLOSE, id, &[]));
                                    continue;
                                }
                            };
                            log::debug(&tag, &format!("Open id={} -> {}:{}", id, target.host, target.port));
                            match on_open(target.clone(), id) {
                                Some(local) => {
                                    let sess = Arc::new(Session {
                                        id,
                                        local: Some(local),
                                        target,
                                        closed: AtomicBool::new(false),
                                    });
                                    self.sessions.lock().unwrap().insert(id, sess.clone());
                                    let m = self.clone();
                                    std::thread::spawn(move || {
                                        m.pipe_local_to_remote(id, sess);
                                    });
                                }
                                None => {
                                    log::info(&tag, &format!("Open id={} rejected (dial failed)", id));
                                    let _ = self.send_raw(&encode_msg(MSG_CLOSE, id, &[]));
                                }
                            }
                        }
                        MSG_DATA => {
                            // Look the session up under the lock, clone the
                            // local stream, then write *outside* the lock: a
                            // slow TCP consumer must not stall the whole
                            // multiplexer (this is the single reader thread).
                            let writer = {
                                let map = self.sessions.lock().unwrap();
                                match map.get(&id) {
                                    Some(sess) => {
                                        Some(sess.local.as_ref().and_then(|l| l.try_clone().ok()))
                                    }
                                    None => None,
                                }
                            };
                            match writer {
                                Some(Some(mut local)) => {
                                    if let Err(e) = local.write_all(payload) {
                                        log::debug(&tag, &format!("session {} write err: {}", id, e));
                                        // Re-lock only to drop the dead session.
                                        self.close_session(id);
                                    }
                                }
                                Some(None) => { /* known session, local not dialed yet: drop */ }
                                None => {
                                    // Unknown session; tell peer to close.
                                    let _ = self.send_raw(&encode_msg(MSG_CLOSE, id, &[]));
                                }
                            }
                        }
                        MSG_CLOSE => {
                            self.close_session(id);
                        }
                        MSG_PING => {
                            let _ = self.send_raw(&encode_msg(MSG_PONG, id, payload));
                        }
                        MSG_PONG => { /* keepalive ack */ }
                        MSG_SETUP => {
                            match Self::decode_setup(payload) {
                                Ok(remotes) => {
                                    log::info(&tag, &format!("Setup: {} reverse remote(s)", remotes.len()));
                                    on_setup(remotes);
                                }
                                Err(e) => {
                                    log::err(&tag, &format!("Setup decode: {}", e));
                                }
                            }
                        }
                        _ => log::err(&tag, &format!("unknown msg type {}", mtype)),
                    }
                }
                OpCode::Ping => {
                    let pong = Frame {
                        fin: true,
                        op: OpCode::Pong,
                        payload: frame.payload.clone(),
                    };
                    let mut s = self.stream.lock().unwrap();
                    let _ = Frame::write(&mut *s, &pong, self.is_client);
                }
                OpCode::Close => {
                    log::info(&tag, "WS Close received");
                    break;
                }
                OpCode::Pong => {}
                _ => {}
            }
        }
        self.shutdown();
        // mark all sessions closed
        let map = self.sessions.lock().unwrap();
        for (_, s) in map.iter() {
            s.closed.store(true, Ordering::SeqCst);
        }
    }
}

/// Encode a tunnel message: [type][id][len][payload]
pub fn encode_msg(mtype: u8, id: u64, payload: &[u8]) -> Vec<u8> {
    let mut v = Vec::with_capacity(11 + payload.len());
    v.push(mtype);
    v.extend_from_slice(&id.to_be_bytes());
    let len = payload.len();
    if len > u16::MAX as usize {
        // truncate would be a bug; just send 65535 chunk
        v.extend_from_slice(&u16::MAX.to_be_bytes());
        v.extend_from_slice(&payload[..u16::MAX as usize]);
    } else {
        v.extend_from_slice(&(len as u16).to_be_bytes());
        v.extend_from_slice(payload);
    }
    v
}

/// Convenience: dial a host:port with a connect timeout.
pub fn dial(target: &AddrTarget) -> std::io::Result<TcpStream> {
    let addr = format!("{}:{}", target.host, target.port);
    let mut last_err = std::io::Error::new(std::io::ErrorKind::Other, "no addresses");
    for sa in addr.to_socket_addrs()? {
        match TcpStream::connect_timeout(&sa, Duration::from_secs(10)) {
            Ok(s) => {
                let _ = s.set_nodelay(true);
                return Ok(s);
            }
            Err(e) => last_err = e,
        }
    }
    Err(last_err)
}
