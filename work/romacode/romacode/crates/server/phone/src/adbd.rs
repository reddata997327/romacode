//! `adbd` — the phone's ADB daemon. Listens on TCP (the port `adb connect`
//! uses), speaks the transport protocol server-side: `CNXN` handshake with
//! RSA token authentication against an authorised-key store, then
//! `OPEN`/`WRTE`/`OKAY`/`CLSE` channels for `exec:`, `shell:`, `sync:`,
//! `root:` and `unroot:`.
//!
//! Any real ADB client can connect to it — mandroid's native client, Google's
//! `adb`, or anything else that speaks the wire protocol.

use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use anyhow::{anyhow, bail, Context, Result};
use rsa::pkcs1v15::Pkcs1v15Sign;
use rsa::RsaPublicKey;
use sha1::{Digest, Sha1};

use crate::persist;
use crate::shell;
use crate::state::OsState;

pub const A_SYNC: u32 = 0x434e5953;
pub const A_CNXN: u32 = 0x4e584e43;
pub const A_OPEN: u32 = 0x4e45504f;
pub const A_OKAY: u32 = 0x59414b4f;
pub const A_CLSE: u32 = 0x45534c43;
pub const A_WRTE: u32 = 0x45545257;
pub const A_AUTH: u32 = 0x48545541;

pub const A_VERSION: u32 = 0x0100_0001;
pub const MAX_DATA: u32 = 256 * 1024;
const ADB_AUTH_TOKEN: u32 = 1;
const ADB_AUTH_SIGNATURE: u32 = 2;
const ADB_AUTH_RSAPUBLICKEY: u32 = 3;
const SYNC_DATA_MAX: usize = 64 * 1024;

const ID_STAT: u32 = u32::from_le_bytes(*b"STAT");
const ID_DATA: u32 = u32::from_le_bytes(*b"DATA");
const ID_DONE: u32 = u32::from_le_bytes(*b"DONE");
const ID_OKAY: u32 = u32::from_le_bytes(*b"OKAY");
const ID_FAIL: u32 = u32::from_le_bytes(*b"FAIL");
const ID_QUIT: u32 = u32::from_le_bytes(*b"QUIT");

fn adb_log(msg: &str) {
    if std::env::var("ROMACODE_ADBD_LOG")
        .map(|v| v == "1")
        .unwrap_or(false)
    {
        eprintln!("[adbd] {msg}");
    }
}

pub struct Adbd {
    pub state: Arc<Mutex<OsState>>,
    pub stop: Arc<AtomicBool>,
    pub bound_port: Arc<Mutex<Option<u16>>>,
}

impl Adbd {
    /// Bind and serve until `stop` is set. Returns the bound port when the
    /// listener exits.
    pub fn serve(self) -> Result<u16> {
        let state = self.state.clone();
        let stop = self.stop.clone();
        let bound = self.bound_port.clone();

        // port selection: default 5555, step by 2 (the ADB convention).
        // Keep the last bind error: "no free port" used to hide the real
        // cause (permission denied, a stale listener, …).
        let mut listener = None;
        let mut last_bind_err = None::<std::io::Error>;
        for port in (5555..=5615).step_by(2) {
            match TcpListener::bind(("127.0.0.1", port)) {
                Ok(l) => {
                    listener = Some((l, port));
                    break;
                }
                Err(e) => last_bind_err = Some(e),
            }
        }
        let (listener, port) = listener.ok_or_else(|| {
            anyhow!(
                "no free ADB port in 5555..=5615 (last bind error: {})",
                last_bind_err
                    .map(|e| e.to_string())
                    .unwrap_or_else(|| "unknown".into())
            )
        })?;
        *bound.lock().unwrap() = Some(port);
        if let Ok(mut st) = state.lock() {
            // the transport is TCP, but the device reports its physical serial
            // (what `ro.serialno` and the CNXN banner carry)
            st.serial = st.identity.serial.clone();
            st.adb_port = port;
            st.notify("Android", &format!("adbd online on 127.0.0.1:{port}"), None);
        }

        listener.set_nonblocking(true).ok();
        while !stop.load(Ordering::Relaxed) {
            match listener.accept() {
                Ok((stream, _addr)) => {
                    let state = state.clone();
                    let stop = stop.clone();
                    std::thread::spawn(move || {
                        stream.set_nonblocking(false).ok();
                        let _ = stream.set_read_timeout(Some(std::time::Duration::from_secs(120)));
                        let _ = stream.set_write_timeout(Some(std::time::Duration::from_secs(60)));
                        let _ = stream.set_nodelay(true);
                        let _ = Connection::run(stream, state, stop);
                    });
                }
                Err(ref e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                    std::thread::sleep(std::time::Duration::from_millis(80));
                }
                Err(_) => std::thread::sleep(std::time::Duration::from_millis(200)),
            }
        }
        Ok(port)
    }
}

fn command_name(code: u32) -> &'static str {
    match code {
        A_SYNC => "SYNC",
        A_CNXN => "CNXN",
        A_OPEN => "OPEN",
        A_OKAY => "OKAY",
        A_CLSE => "CLSE",
        A_WRTE => "WRTE",
        A_AUTH => "AUTH",
        _ => "????",
    }
}

/// One client connection: handshake, then a channel loop.
struct Connection {
    stream: TcpStream,
    max_data: u32,
    local_next: u32,
    client_local: u32,
    server_remote: u32,
}

#[derive(Debug)]
struct Message {
    command: u32,
    arg0: u32,
    arg1: u32,
    data: Vec<u8>,
}

impl Connection {
    fn run(stream: TcpStream, state: Arc<Mutex<OsState>>, stop: Arc<AtomicBool>) -> Result<()> {
        let mut conn = Self {
            stream,
            max_data: MAX_DATA,
            local_next: 1,
            client_local: 0,
            server_remote: 0,
        };
        conn.handshake(&state)?;
        conn.channel_loop(state, stop)
    }

    fn send(&mut self, command: u32, arg0: u32, arg1: u32, data: &[u8]) -> Result<()> {
        let mut header = [0u8; 24];
        header[0..4].copy_from_slice(&command.to_le_bytes());
        header[4..8].copy_from_slice(&arg0.to_le_bytes());
        header[8..12].copy_from_slice(&arg1.to_le_bytes());
        header[12..16].copy_from_slice(&(data.len() as u32).to_le_bytes());
        header[16..20].copy_from_slice(&checksum(data).to_le_bytes());
        header[20..24].copy_from_slice(&(command ^ 0xffff_ffff).to_le_bytes());
        self.stream.write_all(&header)?;
        if !data.is_empty() {
            self.stream.write_all(data)?;
        }
        self.stream.flush()?;
        Ok(())
    }

    fn recv(&mut self) -> Result<Message> {
        let mut header = [0u8; 24];
        self.stream
            .read_exact(&mut header)
            .context("reading header")?;
        let command = u32::from_le_bytes(header[0..4].try_into().unwrap());
        let arg0 = u32::from_le_bytes(header[4..8].try_into().unwrap());
        let arg1 = u32::from_le_bytes(header[8..12].try_into().unwrap());
        let length = u32::from_le_bytes(header[12..16].try_into().unwrap()) as usize;
        let wire_sum = u32::from_le_bytes(header[16..20].try_into().unwrap());
        let magic = u32::from_le_bytes(header[20..24].try_into().unwrap());
        if magic != command ^ 0xffff_ffff {
            bail!("header magic mismatch on {command:#010x}");
        }
        if length > 4 * 1024 * 1024 {
            bail!("implausible payload length {length}");
        }
        let mut data = vec![0u8; length];
        if length > 0 {
            self.stream.read_exact(&mut data)?;
            if wire_sum != 0 && checksum(&data) != wire_sum {
                bail!("checksum mismatch on {length}-byte payload");
            }
        }
        Ok(Message {
            command,
            arg0,
            arg1,
            data,
        })
    }

    /// CNXN + AUTH: the client opens with its CNXN; we either accept (key
    /// known) or challenge with a 20-byte token and verify the signature
    /// against the owner-managed authorised keys. Network clients can never
    /// add their own keys; use the local pairing/bootstrap path instead.
    fn handshake(&mut self, state: &Arc<Mutex<OsState>>) -> Result<()> {
        // 1. client CNXN
        let first = self.recv()?;
        adb_log(&format!("first message: {}", command_name(first.command)));
        if first.command != A_CNXN {
            bail!("expected CNXN, got {}", command_name(first.command));
        }
        let data_dir = state.lock().unwrap().data_dir.clone();

        // 2. token challenge unless this key is already authorised
        let authorized = persist::authorized_keys(&data_dir);
        if !authorized.is_empty() {
            // optimistic path: challenge once, verify signature against store
            let token = random_token();
            adb_log(&format!(
                "known keys: {} — challenge sent",
                authorized.len()
            ));
            self.send(A_AUTH, ADB_AUTH_TOKEN, 0, &token)?;
            for _ in 0..4 {
                let msg = self.recv()?;
                adb_log(&format!(
                    "auth msg: {} arg0={}",
                    command_name(msg.command),
                    msg.arg0
                ));
                match msg.command {
                    A_AUTH if msg.arg0 == ADB_AUTH_SIGNATURE => {
                        if verify_signature(&authorized, &token, &msg.data) {
                            return self.send_cnxn(state);
                        }
                        self.send(A_AUTH, ADB_AUTH_TOKEN, 0, &random_token())?;
                    }
                    A_AUTH if msg.arg0 == ADB_AUTH_RSAPUBLICKEY => {
                        bail!("ADB key is not authorised; explicit local owner pairing is required")
                    }
                    other => bail!("unexpected {} during auth", command_name(other)),
                }
            }
            bail!("authentication failed: key not authorised")
        }
        // An empty store is not permission to trust whichever remote client
        // happens to connect first. Challenge, then refuse all client keys.
        let token = random_token();
        adb_log("fresh device — challenge sent");
        self.send(A_AUTH, ADB_AUTH_TOKEN, 0, &token)?;
        for _ in 0..4 {
            let msg = self.recv()?;
            adb_log(&format!(
                "first-auth msg: {} arg0={}",
                command_name(msg.command),
                msg.arg0
            ));
            match msg.command {
                A_AUTH if msg.arg0 == ADB_AUTH_SIGNATURE => {
                    bail!("no ADB keys are authorised; explicit local owner pairing is required")
                }
                A_AUTH if msg.arg0 == ADB_AUTH_RSAPUBLICKEY => {
                    bail!("ADB key is not authorised; explicit local owner pairing is required")
                }
                other => bail!("unexpected {} during first auth", command_name(other)),
            }
        }
        bail!("first authentication failed")
    }

    fn send_cnxn(&mut self, state: &Arc<Mutex<OsState>>) -> Result<()> {
        let banner = {
            let st = state.lock().unwrap();
            // the same banner a physical handset sends: serialno, product
            // identity, SDK level and the negotiated features
            format!(
                "device::ro.product.model={model};ro.product.device={device};\
                 ro.build.version.release={rel};ro.build.version.sdk={sdk};\
                 ro.serialno={serial};features=cmd,shell_v2,stat_v2",
                model = st.identity.model,
                device = st.identity.device,
                rel = st.identity.android_release,
                sdk = st.identity.sdk_level,
                serial = st.identity.serial,
            )
        };
        self.send(A_CNXN, A_VERSION, MAX_DATA, banner.as_bytes())
    }

    /// Post-handshake channel loop.
    fn channel_loop(&mut self, state: Arc<Mutex<OsState>>, stop: Arc<AtomicBool>) -> Result<()> {
        loop {
            if stop.load(Ordering::Relaxed) {
                return Ok(());
            }
            let msg = self.recv()?;
            match msg.command {
                A_OPEN => {
                    let service = String::from_utf8_lossy(&msg.data)
                        .trim_end_matches('\0')
                        .to_string();
                    adb_log(&format!("OPEN service: {service}"));
                    self.client_local = msg.arg0;
                    self.server_remote = self.local_next;
                    self.local_next += 1;
                    if service.starts_with("exec:") || service.starts_with("shell:") {
                        let cmd = service
                            .trim_start_matches("exec:")
                            .trim_start_matches("shell:")
                            .to_string();
                        self.send(A_OKAY, self.server_remote, self.client_local, &[])?;
                        // stream the output, close the CHANNEL — the transport
                        // stays up for the client's next OPEN (real adbd)
                        self.run_exec(state.clone(), &cmd).ok();
                    } else if service == "sync:" {
                        self.send(A_OKAY, self.server_remote, self.client_local, &[])?;
                        self.sync_loop(state.clone()).ok();
                    } else if service == "root:" {
                        self.send(A_OKAY, self.server_remote, self.client_local, &[])?;
                        if let Ok(mut st) = state.lock() {
                            st.rooted = true;
                        }
                        self.send(A_CLSE, self.server_remote, self.client_local, &[])?;
                        // the "restart" drops the transport, like a real userdebug device
                        return Ok(());
                    } else if service == "unroot:" {
                        self.send(A_OKAY, self.server_remote, self.client_local, &[])?;
                        if let Ok(mut st) = state.lock() {
                            st.rooted = false;
                        }
                        self.send(A_CLSE, self.server_remote, self.client_local, &[])?;
                        return Ok(());
                    } else if service == "features:" {
                        self.send(A_OKAY, self.server_remote, self.client_local, &[])?;
                        self.write_channel(b"cmd").ok();
                        self.send(A_CLSE, self.server_remote, self.client_local, &[])?;
                    } else {
                        // unknown service: refuse with CLSE (like real adbd)
                        self.send(A_CLSE, self.server_remote, self.client_local, &[])?;
                    }
                }
                A_CLSE | A_OKAY | A_WRTE => {
                    // stray traffic for closed channels: acknowledge politely
                    if msg.command == A_WRTE {
                        self.send(A_OKAY, msg.arg1, msg.arg0, &[])?;
                    }
                }
                A_CNXN => {
                    // re-handshake (client reconnecting on the same socket)
                    let _ = msg;
                }
                other => bail!("unexpected {} in channel loop", command_name(other)),
            }
        }
    }

    /// Run one shell command and stream its output.
    fn run_exec(&mut self, state: Arc<Mutex<OsState>>, cmd: &str) -> Result<()> {
        if cmd.is_empty() {
            return Ok(());
        }
        // scrcpy-style live screen stream: one channel, every committed frame
        // pushed raw until the client hangs up
        if cmd.trim() == "screenrecord --raw" || cmd.trim().starts_with("screenrecord ") {
            return self.run_screenrecord(state);
        }
        // screencap clones the committed frame and encodes outside the lock
        let out = if cmd.trim() == "screencap -p"
            || cmd.trim() == "screencap"
            || cmd.trim().starts_with("screencap ")
        {
            let (fb, (w, h), path) = {
                let st = state.lock().unwrap();
                let (w, h) = st.effective_size();
                let fb = st.fb.clone();
                let path = cmd
                    .split_whitespace()
                    .skip(1)
                    .find(|a| !a.starts_with('-'))
                    .and_then(|p| shell::resolve_path(&st, p));
                (fb, (w, h), path)
            };
            let png = mandroid::screen::encode_png(&fb, w, h).unwrap_or_default();
            if let Some(path) = path {
                std::fs::write(path, &png).ok();
                Vec::new()
            } else {
                png
            }
        } else {
            let mut st = state.lock().unwrap();
            shell::exec(&mut st, cmd)
        };
        self.write_channel(&out)?;
        self.send(A_CLSE, self.server_remote, self.client_local, &[])?;
        Ok(())
    }

    /// `screenrecord --raw` — the live display stream (what scrcpy does over
    /// `adb exec-out` on a physical device). Frames are pushed the moment the
    /// compositor commits them: a 20-byte header (magic `RCF1`, commit
    /// generation u64, width u32, height u32, all little-endian) followed by
    /// `w*h*4` bytes of RGBA8. The channel stays open until the client
    /// disconnects.
    ///
    /// The state lock is taken only to clone the frame — never while waiting
    /// on the client — so the compositor keeps rendering at full rate while
    /// the stream is being watched. On an idle screen a heartbeat re-send
    /// every 2 s keeps the channel demonstrably alive (and the client's read
    /// timeout from ever firing on a healthy device).
    fn run_screenrecord(&mut self, state: Arc<Mutex<OsState>>) -> Result<()> {
        const MAGIC: &[u8; 4] = b"RCF1";
        let mut last_gen = 0u64;
        let mut last_sent = Instant::now() - Duration::from_secs(10);
        loop {
            let mut frame = None;
            {
                let st = state.lock().unwrap();
                let (w, h) = st.effective_size();
                let gen = st.fb_gen;
                if gen != last_gen || last_sent.elapsed() >= Duration::from_secs(2) {
                    frame = Some((gen, w, h, st.fb.clone()));
                }
            }
            if let Some((gen, w, h, rgba)) = frame {
                last_gen = gen;
                last_sent = Instant::now();
                let mut pkt = Vec::with_capacity(20 + rgba.len());
                pkt.extend_from_slice(MAGIC);
                pkt.extend_from_slice(&gen.to_le_bytes());
                pkt.extend_from_slice(&w.to_le_bytes());
                pkt.extend_from_slice(&h.to_le_bytes());
                pkt.extend_from_slice(&rgba);
                // errors here mean the client went away — end the stream
                self.write_channel(&pkt)?;
            } else {
                std::thread::sleep(Duration::from_millis(8));
            }
        }
    }

    /// Stream `data` as WRTE chunks, waiting for the client's OKAY each time.
    fn write_channel(&mut self, data: &[u8]) -> Result<()> {
        for chunk in data.chunks(self.max_data.max(4096) as usize) {
            self.send(A_WRTE, self.server_remote, self.client_local, chunk)?;
            loop {
                let msg = self.recv()?;
                match msg.command {
                    A_OKAY if msg.arg1 == self.server_remote => break,
                    A_CLSE if msg.arg1 == self.server_remote => bail!("client closed channel"),
                    A_WRTE if msg.arg1 == self.server_remote => {
                        self.send(A_OKAY, self.server_remote, msg.arg0, &[])?;
                    }
                    other => bail!("unexpected {} waiting for OKAY", command_name(other)),
                }
            }
        }
        Ok(())
    }

    /// The `sync:` byte-stream service: SEND/RECV/STAT/QUIT over WRTE
    /// payloads, byte-oriented exactly like AOSP's file_sync_service. The
    /// client writes requests back-to-back without waiting, so every read
    /// goes through one buffered stream that drains leftover payload bytes
    /// before pulling the next WRTE.
    fn sync_loop(&mut self, state: Arc<Mutex<OsState>>) -> Result<()> {
        let mut buf: Vec<u8> = Vec::new();
        loop {
            let hdr = self.sync_read_exact(&mut buf, state.clone(), 8)?;
            let id = u32::from_le_bytes(hdr[0..4].try_into().unwrap());
            let len = u32::from_le_bytes(hdr[4..8].try_into().unwrap()) as usize;
            if len > 16 * 1024 * 1024 {
                // a hostile local client can declare a ~4 GB body — refuse
                // it instead of ballooning the buffer
                self.sync_fail("sync request too large")?;
                continue;
            }
            let body = self.sync_read_exact(&mut buf, state.clone(), len)?;
            match id {
                x if x == u32::from_le_bytes(*b"SEND") => {
                    let spec = String::from_utf8_lossy(&body).to_string();
                    let (path, _mode) = spec.rsplit_once(',').unwrap_or((&spec, "0644"));
                    match self.sync_recv_file(&mut buf, state.clone()) {
                        Ok(bytes) => {
                            let full = {
                                let guard = state.lock().unwrap();
                                shell::resolve_path(&guard, path.trim_end_matches('\0'))
                            };
                            let Some(full) = full else {
                                self.sync_fail("unsafe destination path")?;
                                continue;
                            };
                            if let Some(parent) = full.parent() {
                                let _ = std::fs::create_dir_all(parent);
                            }
                            let res = std::fs::write(&full, &bytes)
                                .map_err(|e| format!("write {}: {e}", full.display()));
                            // exactly one OKAY/FAIL status at the end (AOSP contract)
                            match res {
                                Ok(()) => self.send(
                                    A_WRTE,
                                    self.server_remote,
                                    self.client_local,
                                    &ID_OKAY.to_le_bytes(),
                                )?,
                                Err(e) => self.sync_fail(&e)?,
                            }
                        }
                        Err(e) => self.sync_fail(&e)?,
                    }
                }
                x if x == u32::from_le_bytes(*b"RECV") => {
                    let path = String::from_utf8_lossy(&body)
                        .trim_end_matches('\0')
                        .to_string();
                    let full = {
                        let guard = state.lock().unwrap();
                        shell::resolve_path(&guard, &path)
                    };
                    let Some(full) = full else {
                        self.sync_fail("unsafe source path")?;
                        continue;
                    };
                    match std::fs::read(&full) {
                        Ok(bytes) => {
                            for chunk in bytes.chunks(SYNC_DATA_MAX) {
                                let mut pkt = b"DATA".to_vec();
                                pkt.extend((chunk.len() as u32).to_le_bytes());
                                pkt.extend_from_slice(chunk);
                                self.send(A_WRTE, self.server_remote, self.client_local, &pkt)?;
                            }
                            let mut done = b"DONE".to_vec();
                            done.extend(crate::state::now_secs().to_le_bytes());
                            self.send(A_WRTE, self.server_remote, self.client_local, &done)?;
                        }
                        Err(_) => {
                            let msg = format!("pull {}: No such file or directory", path);
                            self.sync_fail(&msg)?;
                        }
                    }
                }
                x if x == ID_STAT
                    || x == u32::from_le_bytes(*b"LST2")
                    || x == u32::from_le_bytes(*b"STA2") =>
                {
                    let path = String::from_utf8_lossy(&body)
                        .trim_end_matches('\0')
                        .to_string();
                    let full = {
                        let guard = state.lock().unwrap();
                        shell::resolve_path(&guard, &path)
                    };
                    let mut reply = b"STAT".to_vec();
                    let metadata = full.and_then(|p| {
                        std::fs::symlink_metadata(p)
                            .ok()
                            .filter(|m| !m.file_type().is_symlink())
                    });
                    match metadata {
                        Some(m) => {
                            let mode: u32 = if m.is_dir() { 0o040755 } else { 0o0100644 };
                            reply.extend(mode.to_le_bytes());
                            reply.extend((m.len() as u32).to_le_bytes());
                            reply.extend(mtime_u32(&m).to_le_bytes());
                        }
                        None => reply.extend([0u8; 12]),
                    }
                    self.send(A_WRTE, self.server_remote, self.client_local, &reply)?;
                }
                x if x == ID_QUIT => {
                    self.send(A_CLSE, self.server_remote, self.client_local, &[])?;
                    return Ok(());
                }
                other => {
                    // unknown request: fail the channel, keep the transport
                    let msg = format!("unexpected sync id {other:#010x}");
                    self.sync_fail(&msg)?;
                    return Ok(());
                }
            }
        }
    }

    fn sync_fail(&mut self, msg: &str) -> Result<()> {
        let mut fail = ID_FAIL.to_le_bytes().to_vec();
        fail.extend((msg.len() as u32).to_le_bytes());
        fail.extend(msg.as_bytes().to_vec());
        self.send(A_WRTE, self.server_remote, self.client_local, &fail)
    }

    /// Pull one WRTE payload (acknowledging it), for the byte-oriented sync
    /// stream.
    fn read_wrte(&mut self, _state: Arc<Mutex<OsState>>) -> Result<Vec<u8>> {
        loop {
            let msg = self.recv()?;
            match msg.command {
                A_WRTE if msg.arg1 == self.server_remote => {
                    self.send(A_OKAY, self.server_remote, msg.arg0, &[])?;
                    return Ok(msg.data);
                }
                A_OKAY if msg.arg1 == self.server_remote => {}
                A_CLSE if msg.arg1 == self.server_remote => bail!("sync channel closed"),
                other => bail!("unexpected {} during sync read", command_name(other)),
            }
        }
    }

    /// Read exactly `n` bytes of the sync byte stream: leftovers first, then
    /// fresh WRTE payloads.
    fn sync_read_exact(
        &mut self,
        buf: &mut Vec<u8>,
        state: Arc<Mutex<OsState>>,
        n: usize,
    ) -> Result<Vec<u8>> {
        while buf.len() < n {
            buf.extend(self.read_wrte(state.clone())?);
        }
        Ok(buf.drain(..n).collect())
    }

    /// Read the file body after a SEND header: DATA chunks then DONE.
    fn sync_recv_file(
        &mut self,
        buf: &mut Vec<u8>,
        state: Arc<Mutex<OsState>>,
    ) -> Result<Vec<u8>, String> {
        let mut out = Vec::new();
        loop {
            let head = self
                .sync_read_exact(buf, state.clone(), 8)
                .map_err(|e| e.to_string())?;
            let id = u32::from_le_bytes(head[0..4].try_into().unwrap());
            let len = u32::from_le_bytes(head[4..8].try_into().unwrap()) as usize;
            let body = self
                .sync_read_exact(buf, state.clone(), len)
                .map_err(|e| e.to_string())?;
            match id {
                x if x == ID_DATA => {
                    out.extend_from_slice(&body);
                    if out.len() > 512 * 1024 * 1024 {
                        return Err("pushed file exceeds the 512 MiB limit".to_string());
                    }
                }
                x if x == ID_DONE => return Ok(out),
                other => return Err(format!("unexpected sync id {other:#010x} in SEND")),
            }
        }
    }
}

fn checksum(data: &[u8]) -> u32 {
    data.iter().fold(0u32, |acc, &b| acc.wrapping_add(b as u32))
}

fn random_token() -> [u8; 20] {
    let mut t = [0u8; 20];
    rand_bytes(&mut t);
    t
}

fn rand_bytes(buf: &mut [u8]) {
    // The ADB auth token must be unpredictable — it proves the host holds a
    // previously authorized key. SipHash via `RandomState` is keyed by a
    // random seed but is NOT a CSPRNG: its diffusion and predictability
    // properties are wrong for secret material. Use `OsRng` directly so the
    // bytes come from the host's CSPRNG (getrandom / RtlGenRandom).
    use rand::RngCore;
    rand::rngs::OsRng.fill_bytes(buf);
}

fn mtime_u32(m: &std::fs::Metadata) -> u32 {
    m.modified()
        .ok()
        .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
        .map(|d| d.as_secs() as u32)
        .unwrap_or(0)
}

/// Parse a 524-byte ADB public-key blob and verify an AUTH signature.
/// Blob layout (little-endian): len(4) n0inv(4) n(256) rr(256) e(4, BE).
fn verify_signature(keys_b64: &[String], token: &[u8], sig: &[u8]) -> bool {
    for b64 in keys_b64 {
        let Ok(blob) = mandroid::crypto::base64_decode(b64) else {
            continue;
        };
        if blob.len() < 524 {
            continue;
        }
        let n_le = &blob[8..264];
        let exp = u32::from_le_bytes([blob[520], blob[521], blob[522], blob[523]]);
        let mut n_be = n_le.to_vec();
        n_be.reverse();
        let n = rsa::BigUint::from_bytes_be(&n_be);
        let e = rsa::BigUint::from(exp);
        let Ok(pub_key) = RsaPublicKey::new(n, e) else {
            continue;
        };
        let digest = sha1::Sha1::digest(token);
        if pub_key
            .verify(Pkcs1v15Sign::new::<Sha1>(), &digest, sig)
            .is_ok()
        {
            return true;
        }
    }
    false
}

#[cfg(test)]
mod auth_security_tests {
    use super::*;
    use std::net::TcpListener;
    use std::time::{SystemTime, UNIX_EPOCH};

    fn send_msg(stream: &mut TcpStream, command: u32, arg0: u32, arg1: u32, data: &[u8]) {
        let mut h = [0u8; 24];
        h[0..4].copy_from_slice(&command.to_le_bytes());
        h[4..8].copy_from_slice(&arg0.to_le_bytes());
        h[8..12].copy_from_slice(&arg1.to_le_bytes());
        h[12..16].copy_from_slice(&(data.len() as u32).to_le_bytes());
        h[16..20].copy_from_slice(&checksum(data).to_le_bytes());
        h[20..24].copy_from_slice(&(command ^ 0xffff_ffff).to_le_bytes());
        stream.write_all(&h).unwrap();
        stream.write_all(data).unwrap();
        stream.flush().unwrap();
    }

    #[test]
    fn empty_store_does_not_self_enroll_network_supplied_key() {
        let dir = std::env::temp_dir().join(format!(
            "phone-adb-auth-{}-{}",
            std::process::id(),
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        let state = Arc::new(Mutex::new(persist::fresh(&dir)));
        let listener = TcpListener::bind(("127.0.0.1", 0)).unwrap();
        let addr = listener.local_addr().unwrap();
        let server_state = state.clone();
        let server = std::thread::spawn(move || {
            let (stream, _) = listener.accept().unwrap();
            let mut conn = Connection {
                stream,
                max_data: MAX_DATA,
                local_next: 1,
                client_local: 0,
                server_remote: 0,
            };
            conn.handshake(&server_state)
        });
        let mut client = TcpStream::connect(addr).unwrap();
        send_msg(&mut client, A_CNXN, A_VERSION, MAX_DATA, b"host::");
        let mut auth = [0u8; 24];
        client.read_exact(&mut auth).unwrap();
        assert_eq!(u32::from_le_bytes(auth[0..4].try_into().unwrap()), A_AUTH);
        let token_len = u32::from_le_bytes(auth[12..16].try_into().unwrap()) as usize;
        let mut token = vec![0; token_len];
        client.read_exact(&mut token).unwrap();
        let key_dir = dir.join("generated-client-key");
        let key = mandroid::key::generate_at(&key_dir).unwrap();
        let mut payload = (key.public_text().unwrap().len() as u32)
            .to_le_bytes()
            .to_vec();
        payload.extend_from_slice(key.public_text().unwrap().as_bytes());
        payload.push(0);
        send_msg(&mut client, A_AUTH, ADB_AUTH_RSAPUBLICKEY, 0, &payload);
        assert!(server.join().unwrap().is_err());
        assert!(!persist::key_is_authorized(
            &dir,
            &key.public_text().unwrap()
        ));
        let _ = std::fs::remove_dir_all(dir);
    }
}
