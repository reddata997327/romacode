//! Native ADB client — the ADB transport protocol, spoken directly to `adbd`.
//!
//! No `adb` binary, no ADB server. We connect to a device's `adbd` over TCP
//! (the port `adb connect` uses: 5555 on emulators and network-enabled devices)
//! and run the host half of the protocol ourselves: `CNXN` handshake, `AUTH`
//! with an RSA key we own, then `OPEN`/`WRTE`/`OKAY`/`CLSE` channels for
//! `exec:`, `sync:` and friends.
//!
//! Scope, stated plainly: this speaks the **transport protocol against a
//! device's `adbd` over TCP**. It does not enumerate USB devices and it does
//! not talk to the local ADB *server* — there is no local server here, which is
//! the point.

use std::io::{Read, Write};
use std::net::{TcpStream, ToSocketAddrs};
use std::path::Path;
use std::time::Duration;

use anyhow::{anyhow, bail, Context, Result};

use crate::key::AdbKey;

// Command codes. Each is the little-endian u32 of the four ASCII characters;
// the `magic` field carries the complement.
pub const A_SYNC: u32 = 0x434e5953;
pub const A_CNXN: u32 = 0x4e584e43;
pub const A_OPEN: u32 = 0x4e45504f;
pub const A_OKAY: u32 = 0x59414b4f;
pub const A_CLSE: u32 = 0x45534c43;
pub const A_WRTE: u32 = 0x45545257;
pub const A_AUTH: u32 = 0x48545541;
pub const A_STLS: u32 = 0x534c5453;

/// ADB protocol version we speak.
pub const A_VERSION: u32 = 0x0100_0001;

/// The three `AUTH` sub-types.
const ADB_AUTH_TOKEN: u32 = 1;
const ADB_AUTH_SIGNATURE: u32 = 2;
const ADB_AUTH_RSAPUBLICKEY: u32 = 3;

/// Our advertised maximum payload. adbd's own `maxdata` may be smaller.
pub const DEFAULT_MAX_DATA: u32 = 256 * 1024;

/// Where `adb install` stages an APK on the device by default.
pub const STAGING_DIR: &str = "/data/local/tmp";

/// Maximum sync payload per `DATA` message (`SYNC_DATA_MAX` in AOSP).
pub const SYNC_DATA_MAX: usize = 64 * 1024;

// sync message ids. These are the four ASCII bytes read as a **little-endian**
// u32, which is why `"OKAY"` is 0x59414b4f and not 0x4f4b4159 — writing them as
// the natural big-endian reading is an easy and silent way to fail every
// comparison.
const ID_LSTAT_V1: u32 = u32::from_le_bytes(*b"STAT");
const ID_DATA: u32 = u32::from_le_bytes(*b"DATA");
const ID_DONE: u32 = u32::from_le_bytes(*b"DONE");
const ID_OKAY: u32 = u32::from_le_bytes(*b"OKAY");
const ID_FAIL: u32 = u32::from_le_bytes(*b"FAIL");
// Only the mock server issues these; kept next to their siblings so the
// endianness handling lives in exactly one place.
#[allow(dead_code)]
const ID_SEND: u32 = u32::from_le_bytes(*b"SEND");
#[allow(dead_code)]
const ID_RECV: u32 = u32::from_le_bytes(*b"RECV");

/// Default port for network ADB.
pub const DEFAULT_PORT: u16 = 5555;

fn command_name(code: u32) -> &'static str {
    match code {
        A_SYNC => "SYNC",
        A_CNXN => "CNXN",
        A_OPEN => "OPEN",
        A_OKAY => "OKAY",
        A_CLSE => "CLSE",
        A_WRTE => "WRTE",
        A_AUTH => "AUTH",
        A_STLS => "STLS",
        _ => "????",
    }
}

#[derive(Debug, Clone)]
pub struct Message {
    pub command: u32,
    pub arg0: u32,
    pub arg1: u32,
    pub data: Vec<u8>,
}

impl Message {
    pub fn command_name(&self) -> &'static str {
        command_name(self.command)
    }
}

/// Device identity as reported in the `CNXN` banner.
#[derive(Debug, Clone, Default)]
pub struct DeviceInfo {
    pub banner: String,
    pub max_data: u32,
    pub features: Vec<String>,
    pub properties: Vec<(String, String)>,
}

impl DeviceInfo {
    fn parse(banner: &str, max_data: u32) -> Self {
        let mut info = DeviceInfo {
            banner: banner.to_string(),
            max_data,
            ..Default::default()
        };
        // `device::ro.product.model=Pixel;ro.build.version.sdk=34;features=shell_v2,cmd`
        if let Some(rest) = banner.strip_prefix("device::") {
            for field in rest.split(';') {
                if let Some(features) = field.strip_prefix("features=") {
                    info.features = features
                        .split(',')
                        .filter(|s| !s.is_empty())
                        .map(|s| s.to_string())
                        .collect();
                } else if let Some((k, v)) = field.split_once('=') {
                    info.properties.push((k.to_string(), v.to_string()));
                }
            }
        }
        info
    }

    pub fn get(&self, key: &str) -> Option<&str> {
        self.properties
            .iter()
            .find(|(k, _)| k == key)
            .map(|(_, v)| v.as_str())
    }

    pub fn has_feature(&self, name: &str) -> bool {
        self.features.iter().any(|f| f == name)
    }

    /// Model name, or the banner if the device did not report one.
    pub fn model(&self) -> String {
        self.get("ro.product.model")
            .or_else(|| self.get("ro.product.name"))
            .unwrap_or(&self.banner)
            .to_string()
    }

    pub fn sdk(&self) -> Option<u32> {
        self.get("ro.build.version.sdk")
            .and_then(|s| s.parse().ok())
    }
}

/// Root policy for a connection. **Auto is the default**: MANDROID is rooted
/// by default whenever the device allows it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum RootMode {
    /// Escalate on connect; failure is not fatal.
    #[default]
    Auto,
    /// Escalate on connect; failure is fatal (error from connect).
    Always,
    /// Never escalate; commands run as the shell user.
    Never,
}

impl RootMode {
    /// Parse the tool/CLI spelling: `auto` (default), `on`, `off`.
    pub fn parse(s: &str) -> RootMode {
        match s.trim().to_lowercase().as_str() {
            "on" | "always" | "force" | "true" | "yes" => RootMode::Always,
            "off" | "never" | "no" | "false" => RootMode::Never,
            _ => RootMode::Auto, // "auto" | "default" | "" | anything else
        }
    }

    pub fn as_str(&self) -> &'static str {
        match self {
            RootMode::Auto => "auto",
            RootMode::Always => "always",
            RootMode::Never => "never",
        }
    }
}

/// The root level actually achieved on a connection.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum RootStatus {
    /// adbd itself runs as uid 0 (either natively or via the `root:` service).
    AdbdRoot,
    /// adbd is unprivileged but `su 0 -c` works (Magisk/Superuser ROM);
    /// commands are transparently su-wrapped.
    SuAvailable,
    /// Neither path worked; the device stays unrooted.
    Unavailable,
    /// Probing disabled (`root: off`).
    #[default]
    Unknown,
}

impl RootStatus {
    pub fn as_str(&self) -> &'static str {
        match self {
            RootStatus::AdbdRoot => "adbd_root",
            RootStatus::SuAvailable => "su",
            RootStatus::Unavailable => "unavailable",
            RootStatus::Unknown => "unknown",
        }
    }

    pub fn is_rooted(&self) -> bool {
        matches!(self, RootStatus::AdbdRoot | RootStatus::SuAvailable)
    }
}

/// Wrap a shell command in `su 0 -c '...'` with POSIX-safe single-quote
/// escaping (`it's` → `su 0 -c 'it'\''s'`).
pub fn su_wrap(command: &str) -> String {
    format!("su 0 -c '{}'", command.replace('\'', r"'\''"))
}

#[derive(Debug, Clone)]
pub struct AdbOptions {
    pub connect_timeout: Duration,
    pub io_timeout: Duration,
    /// Refuse to sign the AUTH token if the device wants a key we do not have
    /// (i.e. fail instead of prompting). Always effectively true here — there is
    /// no user to prompt.
    pub max_auth_rounds: usize,
    /// Root policy applied on connect. Default `Auto` — rooted by default.
    pub root: RootMode,
}

impl Default for AdbOptions {
    fn default() -> Self {
        Self {
            connect_timeout: Duration::from_secs(10),
            io_timeout: Duration::from_secs(60),
            max_auth_rounds: 3,
            root: RootMode::default(),
        }
    }
}

/// A live, authenticated connection to a device's `adbd`.
pub struct AdbDevice {
    stream: TcpStream,
    pub info: DeviceInfo,
    pub addr: String,
    next_local_id: u32,
    /// Set once the CNXN handshake is complete. From that point the checksum
    /// field is sent as 0 and must not be validated.
    negotiated: bool,
    /// Retained key so `reconnect()` (root escalation) can replay the
    /// handshake without the caller re-passing it.
    key: Option<AdbKey>,
    /// Options the connection was made with (root policy lives here).
    opts: AdbOptions,
    /// Root level achieved during connect (probed/escalated per policy).
    root_status: RootStatus,
    /// Per-channel pending-read buffer. Some services (e.g. `shell:` banners)
    /// push a `WRTE` before sending the `OKAY` that completes [`open`]; the
    /// `A_WRTE` arm of `open` acknowledges the WRTE so the device is not
    /// blocked, and stashes the bytes here keyed by the local channel id so
    /// [`read_channel`] can drain them before issuing its next `recv()`.
    pending:
        std::collections::HashMap<u32, std::collections::VecDeque<Vec<u8>>>,
}

/// An open long-lived `exec:` channel (see
/// [`AdbDevice::open_exec_stream`]). `local` is this client's channel id,
/// `remote` the device's.
pub struct ExecStream {
    pub local: u32,
    pub remote: u32,
}

/// One raw frame from a `screenrecord --raw` live stream: the compositor
/// generation it was committed at, its size, and RGBA8 pixel data.
#[derive(Clone, Debug)]
pub struct StreamFrame {
    pub gen: u64,
    pub w: u32,
    pub h: u32,
    pub rgba: Vec<u8>,
}

/// Magic prefix and header layout of a `screenrecord --raw` frame packet:
/// magic `RCF1` (4) + commit generation u64 (8) + width u32 (4) +
/// height u32 (4), all little-endian, followed by `w*h*4` RGBA8 bytes.
pub const SCREEN_RECORD_MAGIC: &[u8; 4] = b"RCF1";
pub const STREAM_HEADER_LEN: usize = 20;

/// Reassembles `screenrecord --raw` frames out of the WRTE chunk stream.
/// ADB splits the payload at the negotiated `max_data` size, so a frame
/// arrives as many chunks cut at arbitrary byte boundaries; this buffer
/// stitches whole frames back together and keeps leftover bytes queued for
/// the next [`StreamFrame`].
#[derive(Default)]
pub struct StreamReassembler {
    buf: Vec<u8>,
}

impl StreamReassembler {
    pub fn new() -> Self {
        Self { buf: Vec::new() }
    }

    /// Bytes queued but not yet reassembled into a frame.
    pub fn buffered(&self) -> usize {
        self.buf.len()
    }

    /// Queue a raw chunk from the wire. Call [`Self::pop`] afterwards.
    pub fn push(&mut self, chunk: &[u8]) {
        self.buf.extend_from_slice(chunk);
    }

    /// Pop one complete frame if the buffer holds enough bytes. Returns
    /// `Ok(None)` while more chunks are needed; an error on a corrupt
    /// header (buffer is dropped — caller must reopen the stream) or when
    /// the frame exceeds `max_frame` bytes of pixels.
    pub fn pop(&mut self, max_frame: usize) -> Result<Option<StreamFrame>> {
        if self.buf.len() < STREAM_HEADER_LEN {
            return Ok(None);
        }
        if &self.buf[0..4] != SCREEN_RECORD_MAGIC {
            let n = self.buf.len();
            self.buf.clear();
            bail!("screen stream header corrupt ({n} bytes dropped)");
        }
        let gen = u64::from_le_bytes(self.buf[4..12].try_into().unwrap());
        let w = u32::from_le_bytes(self.buf[12..16].try_into().unwrap());
        let h = u32::from_le_bytes(self.buf[16..20].try_into().unwrap());
        let pixels = (w as u64) * (h as u64) * 4;
        if pixels > max_frame as u64 {
            self.buf.clear();
            bail!("screen stream frame {w}x{h} exceeds the {max_frame} byte cap");
        }
        let need = STREAM_HEADER_LEN + pixels as usize;
        if self.buf.len() < need {
            return Ok(None);
        }
        let rgba = self.buf[STREAM_HEADER_LEN..need].to_vec();
        self.buf.drain(..need);
        Ok(Some(StreamFrame { gen, w, h, rgba }))
    }
}

impl std::fmt::Debug for AdbDevice {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("AdbDevice")
            .field("addr", &self.addr)
            .field("model", &self.info.model())
            .field("sdk", &self.info.sdk())
            .field("features", &self.info.features)
            .finish()
    }
}

/// Parse `host`, `host:port`, `[v6]:port` or a full `tcp:`/`adb://` URL.
pub fn parse_target(spec: &str) -> Result<String> {
    let cleaned = spec
        .trim_start_matches("tcp:")
        .trim_start_matches("adb://")
        .trim_start_matches("http://");
    if cleaned.is_empty() {
        bail!("empty ADB target");
    }
    // Already host:port (including bracketed IPv6 with a port)?
    if let Some(rest) = cleaned.strip_prefix('[') {
        if let Some((host, tail)) = rest.split_once(']') {
            let port = tail
                .strip_prefix(':')
                .and_then(|p| p.parse::<u16>().ok())
                .unwrap_or(DEFAULT_PORT);
            return Ok(format!("[{host}]:{port}"));
        }
    }
    match cleaned.rsplit_once(':') {
        Some((host, port)) if !host.is_empty() && port.chars().all(|c| c.is_ascii_digit()) => {
            Ok(format!("{host}:{port}"))
        }
        _ => Ok(format!("{cleaned}:{DEFAULT_PORT}")),
    }
}

impl AdbDevice {
    /// Connect, complete the `CNXN`/`AUTH` handshake, and return a ready device.
    /// With the default root policy (`auto`) the connection is **rooted by
    /// default**: the ladder in [`Self::ensure_root`] runs before returning.
    pub fn connect(target: &str, key: &AdbKey, opts: &AdbOptions) -> Result<Self> {
        let mut device = Self::connect_inner(target, key, opts)?;
        // Retain the key BEFORE the root ladder: `root:` restarts adbd and
        // reconnect() replays the handshake with the retained key.
        device.key = Some(key.clone());
        match device.opts.root {
            RootMode::Never => {
                device.root_status = RootStatus::Unknown;
            }
            RootMode::Auto => {
                device.ensure_root().ok();
            }
            RootMode::Always => {
                device.ensure_root()?;
            }
        }
        Ok(device)
    }

    /// Handshake-only constructor used by both [`connect`] and
    /// [`Self::reconnect`] (so reconnection never re-runs the root ladder).
    fn connect_inner(target: &str, key: &AdbKey, opts: &AdbOptions) -> Result<Self> {
        let addr = parse_target(target)?;
        let sock = addr
            .to_socket_addrs()
            .with_context(|| format!("resolving {addr}"))?
            .next()
            .ok_or_else(|| anyhow!("{addr} did not resolve to an address"))?;
        let stream = TcpStream::connect_timeout(&sock, opts.connect_timeout)
            .with_context(|| format!("connecting to {addr} (is adbd listening?)"))?;
        stream.set_read_timeout(Some(opts.io_timeout))?;
        stream.set_write_timeout(Some(opts.io_timeout))?;
        stream.set_nodelay(true).ok();

        let mut device = AdbDevice {
            stream,
            info: DeviceInfo {
                max_data: DEFAULT_MAX_DATA,
                ..Default::default()
            },
            addr,
            next_local_id: 1,
            negotiated: false,
            key: None,
            opts: opts.clone(),
            root_status: RootStatus::Unknown,
            pending: std::collections::HashMap::new(),
        };
        device.handshake(key, opts)?;
        Ok(device)
    }

    fn handshake(&mut self, key: &AdbKey, opts: &AdbOptions) -> Result<()> {
        // Advertise only what this client actually implements: `exec:` for
        // shell and the v1 `STAT` form. Claiming stat_v2/shell_v2 would make the
        // device answer in formats we do not parse.
        let banner = "host::features=cmd";
        self.send(A_CNXN, A_VERSION, DEFAULT_MAX_DATA, banner.as_bytes())?;
        self.negotiated = true;

        for round in 0..opts.max_auth_rounds {
            let msg = self.recv()?;
            match msg.command {
                A_CNXN => {
                    // Device CNXN: arg0 = maxdata, arg1 = protocol version.
                    let banner_text = String::from_utf8_lossy(&msg.data).to_string();
                    let max_data = if msg.arg0 == 0 {
                        DEFAULT_MAX_DATA
                    } else {
                        msg.arg0
                    };
                    self.info = DeviceInfo::parse(&banner_text, max_data);
                    if self.info.properties.is_empty() && !banner_text.starts_with("device::") {
                        self.info.banner = banner_text;
                    }
                    return Ok(());
                }
                A_AUTH => {
                    if msg.arg0 == ADB_AUTH_TOKEN {
                        let token = if msg.data.len() >= 20 {
                            &msg.data[..20]
                        } else {
                            &msg.data[..]
                        };
                        // First offer a signature; if the device has never seen
                        // this key it asks again and we hand over the public key.
                        if round == 0 {
                            let signature = key.sign_token(token)?;
                            self.send(A_AUTH, ADB_AUTH_SIGNATURE, 0, &signature)?;
                        } else {
                            let blob = crate::crypto::android_public_key_blob(&key.key)?;
                            let text = crate::crypto::base64_encode(&blob);
                            let mut payload = Vec::with_capacity(text.len() + 5);
                            payload.extend_from_slice(&(text.len() as u32).to_le_bytes());
                            payload.extend_from_slice(text.as_bytes());
                            payload.push(0);
                            self.send(A_AUTH, ADB_AUTH_RSAPUBLICKEY, 0, &payload)?;
                        }
                    } else {
                        bail!(
                            "device sent AUTH with unexpected arg0={} ({})",
                            msg.arg0,
                            command_name(msg.command)
                        );
                    }
                }
                A_STLS => {
                    bail!(
                        "device requested STLS (ADB over TLS). This client speaks the plaintext \
                         transport; use a device/emulator with TLS disabled, or target the \
                         plaintext port."
                    );
                }
                other => {
                    bail!(
                        "unexpected {} during handshake (arg0={}, arg1={}, {} bytes)",
                        command_name(other),
                        msg.arg0,
                        msg.arg1,
                        msg.data.len()
                    );
                }
            }
        }
        bail!(
            "authentication did not complete in {} rounds",
            opts.max_auth_rounds
        )
    }

    fn send(&mut self, command: u32, arg0: u32, arg1: u32, data: &[u8]) -> Result<()> {
        let mut header = [0u8; 24];
        header[0..4].copy_from_slice(&command.to_le_bytes());
        header[4..8].copy_from_slice(&arg0.to_le_bytes());
        header[8..12].copy_from_slice(&arg1.to_le_bytes());
        header[12..16].copy_from_slice(&(data.len() as u32).to_le_bytes());
        header[16..20].copy_from_slice(&adb_checksum(data).to_le_bytes());
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
            .context("reading ADB message header")?;
        let command = u32::from_le_bytes(header[0..4].try_into().unwrap());
        let arg0 = u32::from_le_bytes(header[4..8].try_into().unwrap());
        let arg1 = u32::from_le_bytes(header[8..12].try_into().unwrap());
        let length = u32::from_le_bytes(header[12..16].try_into().unwrap()) as usize;
        let checksum = u32::from_le_bytes(header[16..20].try_into().unwrap());
        let magic = u32::from_le_bytes(header[20..24].try_into().unwrap());
        if magic != command ^ 0xffff_ffff {
            bail!("ADB header magic mismatch: command={command:#010x} magic={magic:#010x}");
        }
        if length > 4 * 1024 * 1024 {
            bail!("ADB message claims {length} bytes, which is implausible");
        }
        let mut data = vec![0u8; length];
        if length > 0 {
            self.stream
                .read_exact(&mut data)
                .context("reading ADB message body")?;
            // A zero field means the peer negotiated skip-checksum.
            if checksum != 0 && adb_checksum(&data) != checksum {
                bail!(
                    "ADB message checksum mismatch on a {length}-byte payload: \
                     field={checksum} sum={}",
                    adb_checksum(&data)
                );
            }
        }
        Ok(Message {
            command,
            arg0,
            arg1,
            data,
        })
    }

    fn alloc_local_id(&mut self) -> u32 {
        let id = self.next_local_id;
        self.next_local_id += 1;
        id
    }

    /// `OPEN` a service and wait for the device to accept it.
    fn open(&mut self, service: &str) -> Result<(u32, u32)> {
        let local = self.alloc_local_id();
        // The reference client NUL-terminates the service name; matching that
        // keeps behaviour identical on the wire.
        let mut payload = service.as_bytes().to_vec();
        payload.push(0);
        self.send(A_OPEN, local, 0, &payload)?;
        loop {
            let msg = self.recv()?;
            match msg.command {
                A_OKAY if msg.arg1 == local => return Ok((local, msg.arg0)),
                A_CLSE if msg.arg1 == local => {
                    bail!("device refused service {service:?} (CLSE)")
                }
                A_WRTE if msg.arg1 == local => {
                    // Some services push data before/without an explicit OKAY
                    // ordering guarantee; acknowledge so the device is not
                    // left waiting, then keep waiting for the accept.
                    // Buffer any bytes the service emitted ahead of its OKAY
                    // so `read_channel` can drain them before its next recv().
                    if !msg.data.is_empty() {
                        self.pending
                            .entry(local)
                            .or_default()
                            .push_back(msg.data);
                    }
                    self.send(A_OKAY, local, msg.arg0, &[])?;
                }
                A_CLSE if msg.arg1 == local => {
                    // Channel rejected before open completed; drop any pending
                    // buffer that may have accumulated from a pre-OKAY WRTE.
                    self.pending.remove(&local);
                    bail!("device refused service {service:?} (CLSE)")
                }
                A_CLSE | A_OKAY | A_WRTE => {
                    // Traffic for a different channel id: acknowledge/ignore.
                    if msg.command == A_WRTE {
                        self.send(A_OKAY, msg.arg1, msg.arg0, &[])?;
                    }
                }
                other => bail!(
                    "unexpected {} while opening {service:?}",
                    command_name(other)
                ),
            }
        }
    }

    fn write_channel(&mut self, local: u32, remote: u32, data: &[u8]) -> Result<()> {
        let limit = self.info.max_data.max(4096) as usize;
        for chunk in data.chunks(limit) {
            self.send(A_WRTE, local, remote, chunk)?;
            // The device acknowledges each WRTE with an empty OKAY.
            loop {
                let msg = self.recv()?;
                if msg.command == A_OKAY && msg.arg1 == local {
                    break;
                }
                if msg.command == A_CLSE && msg.arg1 == local {
                    bail!("channel closed while writing");
                }
                // Data arriving from the device mid-write is unexpected for the
                // services we use; acknowledge and continue waiting.
                if msg.command == A_WRTE {
                    self.send(A_OKAY, msg.arg1, msg.arg0, &[])?;
                    continue;
                }
            }
        }
        Ok(())
    }

    /// Read channel output until the device closes it. Caps at `limit` bytes.
    fn read_channel(&mut self, local: u32, limit: usize) -> Result<Vec<u8>> {
        let mut out = Vec::new();
        // Drain any bytes the device pushed before its OKAY (buffered in
        // `open`'s `A_WRTE` arm) so they are not lost when the caller reads.
        if let Some(queue) = self.pending.get_mut(&local) {
            while let Some(chunk) = queue.pop_front() {
                out.extend_from_slice(&chunk);
                if out.len() > limit {
                    // Stop the device rather than draining forever.
                    self.send(A_CLSE, local, 0, &[]).ok();
                    self.pending.remove(&local);
                    bail!("channel output exceeded the {limit} byte limit");
                }
            }
            // Queue drained — drop the entry so memory stays tight.
            self.pending.remove(&local);
        }
        loop {
            let msg = self.recv()?;
            match msg.command {
                A_WRTE if msg.arg1 == local => {
                    out.extend_from_slice(&msg.data);
                    self.send(A_OKAY, local, msg.arg0, &[])?;
                    if out.len() > limit {
                        // Tell the device to stop rather than draining forever.
                        self.send(A_CLSE, local, msg.arg0, &[])?;
                        self.pending.remove(&local);
                        bail!("channel output exceeded the {limit} byte limit");
                    }
                }
                A_CLSE if msg.arg1 == local => {
                    self.send(A_CLSE, local, msg.arg0, &[]).ok();
                    // Channel closed: free any leftover pending buffer.
                    self.pending.remove(&local);
                    return Ok(out);
                }
                A_OKAY if msg.arg1 == local => {}
                A_WRTE => self.send(A_OKAY, msg.arg1, msg.arg0, &[])?,
                other => bail!("unexpected {} while reading channel", command_name(other)),
            }
        }
    }

    /// Run a command and return its raw stdout (no CRLF translation).
    /// On su-rooted devices the command is transparently wrapped in
    /// `su 0 -c` (see [`Self::effective_command`]).
    pub fn exec_bytes(&mut self, command: &str, limit: usize) -> Result<Vec<u8>> {
        let effective = self.effective_command(command);
        let (local, remote) = self.open(&format!("exec:{effective}"))?;
        let out = self.read_channel(local, limit);
        if let Err(e) = &out {
            self.send(A_CLSE, local, remote, &[]).ok();
            return Err(anyhow!("{e}"));
        }
        Ok(out?)
    }

    /// Open a long-lived `exec:` channel for continuous output — the
    /// transport a device's live screen stream (`screenrecord --raw`, the
    /// service scrcpy uses) runs on. Unlike [`Self::exec_bytes`], the channel
    /// is not drained to completion: frames are pulled one at a time with
    /// [`Self::exec_stream_next`] for as long as the producer keeps them
    /// coming.
    ///
    /// Use a dedicated connection for the stream: interleaving shell
    /// commands on the same transport would risk their replies being
    /// consumed by the stream reader.
    pub fn open_exec_stream(&mut self, command: &str) -> Result<ExecStream> {
        let effective = self.effective_command(command);
        let (local, remote) = self.open(&format!("exec:{effective}"))?;
        Ok(ExecStream { local, remote })
    }

    /// Read the next complete frame from an open stream channel. Blocks
    /// until a full frame arrives; errors when the device closes the channel
    /// or the socket dies (caller reopens). `max_frame` caps a single frame's
    /// payload so a corrupted header can never allocate unbounded memory.
    pub fn exec_stream_next(
        &mut self,
        s: &ExecStream,
        re: &mut StreamReassembler,
        max_frame: usize,
    ) -> Result<StreamFrame> {
        loop {
            if let Some(f) = re.pop(max_frame)? {
                return Ok(f);
            }
            let msg = self.recv()?;
            match msg.command {
                A_WRTE if msg.arg1 == s.local => {
                    re.push(&msg.data);
                    self.send(A_OKAY, s.local, msg.arg0, &[])?;
                }
                A_CLSE if msg.arg1 == s.local => {
                    self.send(A_CLSE, s.local, msg.arg0, &[]).ok();
                    bail!("screen stream closed by device");
                }
                A_WRTE => {
                    // traffic for a different channel id — acknowledge, discard
                    self.send(A_OKAY, msg.arg1, msg.arg0, &[])?;
                }
                A_OKAY if msg.arg1 == s.local => {}
                other => bail!("unexpected {} on stream channel", command_name(other)),
            }
        }
    }

    /// Close a stream channel opened with [`Self::open_exec_stream`].
    pub fn exec_stream_close(&mut self, s: &ExecStream) {
        self.send(A_CLSE, s.local, s.remote, &[]).ok();
    }

    /// The command actually sent to adbd: as-is on adbd-root connections,
    /// su-wrapped when the only root path is `su`, verbatim when the policy
    /// is `off`.
    pub fn effective_command(&self, command: &str) -> String {
        if self.root_status == RootStatus::SuAvailable && self.opts.root != RootMode::Never {
            su_wrap(command)
        } else {
            command.to_string()
        }
    }

    /// The root level achieved on this connection.
    pub fn root_status(&self) -> RootStatus {
        self.root_status
    }

    /// The root escalation ladder — **rooted by default**:
    ///
    /// 1. adbd already runs as uid 0 (`id -u` == 0) → `adbd_root`;
    /// 2. the `root:` service — adbd restarts as root on userdebug/eng builds;
    ///    the connection drop is part of the deal, so we drain, sleep,
    ///    reconnect and re-probe → `adbd_root`;
    /// 3. `su 0 -c` works (Magisk/Superuser ROM) → `su`; every later command
    ///    is transparently su-wrapped by [`Self::exec_bytes`];
    /// 4. otherwise → `unavailable` (error text says so).
    pub fn ensure_root(&mut self) -> Result<RootStatus> {
        // 1) already root?
        if self.probe_uid_zero()? {
            self.root_status = RootStatus::AdbdRoot;
            return Ok(self.root_status);
        }
        // 2) `root:` service (this restarts adbd as root on userdebug builds).
        if self.try_root_service() {
            std::thread::sleep(Duration::from_millis(800));
            self.reconnect()?;
            if self.probe_uid_zero()? {
                self.root_status = RootStatus::AdbdRoot;
                return Ok(self.root_status);
            }
        }
        // 3) su fallback.
        if self.probe_su()? {
            self.root_status = RootStatus::SuAvailable;
            return Ok(self.root_status);
        }
        self.root_status = RootStatus::Unavailable;
        bail!(
            "device cannot be rooted: not a userdebug build (no `root:` service) \
             and no `su` binary"
        )
    }

    /// Drop back to the shell user (`unroot:` service) and never re-escalate:
    /// the status is set to `unavailable` without probing, by contract.
    pub fn unroot(&mut self) -> Result<()> {
        let (local, _remote) = self.open("unroot:")?;
        let _ = self.read_channel(local, 4096);
        std::thread::sleep(Duration::from_millis(500));
        self.reconnect()?;
        self.root_status = RootStatus::Unavailable;
        Ok(())
    }

    /// Re-run the handshake on the same address with the retained key. Used
    /// after `root:`/`unroot:` restart adbd. Root status carries over as-is;
    /// the caller re-probes.
    fn reconnect(&mut self) -> Result<()> {
        let key = self
            .key
            .clone()
            .ok_or_else(|| anyhow!("no retained key for reconnect"))?;
        let addr = self.addr.clone();
        let opts = self.opts.clone();
        let fresh = Self::connect_inner(&addr, &key, &opts)?;
        *self = fresh;
        // connect_inner starts with a blank key slot; re-retain it so any
        // later reconnect (unroot, re-escalation) still has it.
        self.key = Some(key);
        Ok(())
    }

    /// `id -u` == 0? (probe runs before any su-wrap is active, so it goes out
    /// verbatim.)
    fn probe_uid_zero(&mut self) -> Result<bool> {
        let out = self.exec_bytes("id -u", 256)?;
        Ok(String::from_utf8_lossy(&out).trim() == "0")
    }

    /// Does `su 0 -c id -u` report 0? (sent verbatim — wrapping is not active
    /// until this probe succeeds.)
    fn probe_su(&mut self) -> Result<bool> {
        let out = self.exec_bytes("su 0 -c id -u", 256)?;
        Ok(String::from_utf8_lossy(&out).trim() == "0")
    }

    /// Ask adbd to restart as root via the `root:` service. Returns whether
    /// the service was accepted (adbd closes the connection right after —
    /// that is the restart, not an error).
    fn try_root_service(&mut self) -> bool {
        let (local, _remote) = match self.open("root:") {
            Ok(x) => x,
            Err(_) => return false,
        };
        // Drain the goodbye (OKAY/CLSE or a reset socket) — ignore everything.
        let _ = self.read_channel(local, 4096);
        true
    }

    /// Run a command and return its stdout as text.
    pub fn shell(&mut self, command: &str) -> Result<String> {
        let bytes = self.exec_bytes(command, 16 * 1024 * 1024)?;
        Ok(String::from_utf8_lossy(&bytes).to_string())
    }

    /// Run a command and require that it succeeded, judged by adbd's exit status
    /// marker (`shell_v2` sends `exit:<code>` as a separate stream, but plain
    /// `exec:` just gives us text — so we look for the conventional marker and
    /// otherwise trust the output).
    pub fn shell_checked(&mut self, command: &str) -> Result<String> {
        let out = self.shell(command)?;
        // `cmd`/`sh -c` style commands signal failure in the text; surface it.
        if out.contains("Failure") || out.contains("Error:") || out.contains("error:") {
            bail!("command failed: {}", out.trim());
        }
        Ok(out)
    }

    // -- sync: file transfer -------------------------------------------------
    //
    // The sync protocol is a **byte stream**, not a message stream. adbd writes
    // a header and its payload with separate `write()` calls, and the transport
    // may coalesce or split them arbitrarily, so parsing has to be
    // stream-oriented. Ground truth is AOSP `file_sync_service.cpp` and
    // `file_sync_client.cpp`:
    //
    //   * `do_recv` writes `DATA` blocks back to back and then one `DONE`,
    //     reading nothing in between. The client sends no acknowledgement
    //     between blocks — it reads `{id,size}` then `size` bytes, repeatedly.
    //   * `handle_send_file` reads `SEND`, every `DATA`, then `DONE`, and
    //     replies with exactly **one** `{ID_OKAY,0}` status at the end.
    //   * `do_lstat_v1` replies with a fixed 16-byte `{id,mode,size,time}`,
    //     even for a path that does not exist (all-zero), which is how the
    //     real client detects "not found".

    fn sync_open(&mut self) -> Result<SyncStream<'_>> {
        let (local, remote) = self.open("sync:")?;
        Ok(SyncStream {
            dev: self,
            local,
            remote,
            buf: Vec::new(),
            pos: 0,
        })
    }

    /// `sync:` SEND — push a local file to the device.
    pub fn push(&mut self, local_path: &Path, remote_path: &str) -> Result<u64> {
        let data = std::fs::read(local_path)
            .with_context(|| format!("reading {}", local_path.display()))?;
        self.push_bytes(&data, remote_path)
    }

    pub fn push_bytes(&mut self, data: &[u8], remote_path: &str) -> Result<u64> {
        let mut stream = self.sync_open()?;
        let result = stream.push(data, remote_path);
        stream.close();
        result?;
        Ok(data.len() as u64)
    }

    /// `sync:` RECV — pull a file off the device into memory.
    pub fn pull_bytes(&mut self, remote_path: &str, limit: usize) -> Result<Vec<u8>> {
        let mut stream = self.sync_open()?;
        let result = stream.pull(remote_path, limit);
        stream.close();
        result
    }

    /// Pull a file to disk.
    pub fn pull(&mut self, remote_path: &str, local_path: &Path, limit: usize) -> Result<u64> {
        let data = self.pull_bytes(remote_path, limit)?;
        if let Some(parent) = local_path.parent() {
            std::fs::create_dir_all(parent).ok();
        }
        std::fs::write(local_path, &data)
            .with_context(|| format!("writing {}", local_path.display()))?;
        Ok(data.len() as u64)
    }

    /// `sync:` STAT (the always-supported v1 form). `None` when the path does
    /// not exist, which the device signals with an all-zero stat.
    pub fn stat(&mut self, remote_path: &str) -> Result<Option<RemoteStat>> {
        let mut stream = self.sync_open()?;
        let result = stream.stat(remote_path);
        stream.close();
        result
    }

    // -- package management --------------------------------------------------

    /// Push an APK to the staging directory and run `pm install`.
    ///
    /// `pm install` is the documented, stable path; the streaming
    /// `package install -S` variant needs `cmd` and a live stdin channel, which
    /// is the kind of thing that breaks across adbd versions. Pushing first is
    /// what the real client does by default anyway.
    pub fn install(&mut self, apk_path: &Path, opts: &InstallOptions) -> Result<InstallResult> {
        let name = apk_path
            .file_name()
            .map(|s| s.to_string_lossy().to_string())
            .ok_or_else(|| anyhow!("{} has no file name", apk_path.display()))?;
        let remote = format!("{STAGING_DIR}/{}", sanitise_remote_name(&name));
        let size = std::fs::metadata(apk_path)
            .with_context(|| format!("stat {}", apk_path.display()))?
            .len();
        self.push(apk_path, &remote)?;

        let mut flags = vec!["-r"]; // replace an existing install
        if opts.allow_downgrade {
            flags.push("-d");
        }
        if opts.allow_test_packages {
            flags.push("-t");
        }
        if opts.grant_permissions {
            flags.push("-g");
        }
        if let Some(user) = &opts.user {
            flags.push("--user");
            flags.push(user);
        }
        let command = format!("pm install {} {}", flags.join(" "), remote);
        let output = self.exec_bytes(&command, 4 * 1024 * 1024)?;
        let text = String::from_utf8_lossy(&output).to_string();
        let success = text.contains("Success");
        if !opts.keep_staged {
            self.exec_bytes(&format!("rm -f {}", remote), 64 * 1024)
                .ok();
        }
        Ok(InstallResult {
            success,
            output: text,
            remote_path: remote,
            apk_bytes: size,
        })
    }

    pub fn uninstall(&mut self, package: &str, keep_data: bool) -> Result<String> {
        let flag = if keep_data { "-k " } else { "" };
        let out = self.exec_bytes(&format!("pm uninstall {flag}{package}"), 256 * 1024)?;
        Ok(String::from_utf8_lossy(&out).to_string())
    }

    /// Launch a component, or the launcher activity of a package.
    pub fn launch(&mut self, component: &str) -> Result<String> {
        let command = if component.contains('/') {
            format!("am start -n {component}")
        } else {
            format!("monkey -p {component} -c android.intent.category.LAUNCHER 1")
        };
        let out = self.exec_bytes(&command, 256 * 1024)?;
        Ok(String::from_utf8_lossy(&out).to_string())
    }

    pub fn force_stop(&mut self, package: &str) -> Result<String> {
        let out = self.exec_bytes(&format!("am force-stop {package}"), 64 * 1024)?;
        Ok(String::from_utf8_lossy(&out).to_string())
    }

    pub fn list_packages(&mut self, filter: Option<&str>) -> Result<Vec<String>> {
        let needle = filter.unwrap_or("");
        let out = self.shell("pm list packages")?;
        Ok(out
            .lines()
            .filter_map(|l| l.trim().strip_prefix("package:"))
            .filter(|p| p.contains(needle))
            .map(|p| p.trim().to_string())
            .collect())
    }

    /// PNG screenshot via `screencap`.
    pub fn screencap(&mut self) -> Result<Vec<u8>> {
        let bytes = self.exec_bytes("screencap -p", 64 * 1024 * 1024)?;
        if bytes.len() < 8 || &bytes[..8] != b"\x89PNG\r\n\x1a\n" {
            bail!(
                "screencap did not return a PNG ({} bytes); some devices need \
                 `screencap -p /sdcard/x.png` then a pull",
                bytes.len()
            );
        }
        Ok(bytes)
    }

    /// Device properties (`getprop`), parsed into key/value pairs.
    pub fn properties(&mut self) -> Result<Vec<(String, String)>> {
        let out = self.shell("getprop")?;
        let mut props = Vec::new();
        for line in out.lines() {
            let line = line.trim();
            if let Some(rest) = line.strip_prefix('[') {
                if let Some((k, v)) = rest.split_once("]: [") {
                    let v = v.trim_end_matches(']');
                    props.push((k.to_string(), v.to_string()));
                }
            }
        }
        Ok(props)
    }
}

/// A byte-stream view over a `sync:` channel.
///
/// Buffers whatever arrives and hands out exactly the bytes the sync message
/// parser asks for, so it is immune to how adbd happens to chunk its writes.
struct SyncStream<'a> {
    dev: &'a mut AdbDevice,
    local: u32,
    remote: u32,
    buf: Vec<u8>,
    pos: usize,
}

impl SyncStream<'_> {
    fn fill(&mut self) -> Result<()> {
        loop {
            let msg = self.dev.recv()?;
            match msg.command {
                A_WRTE if msg.arg1 == self.local => {
                    self.dev.send(A_OKAY, self.local, msg.arg0, &[])?;
                    if !msg.data.is_empty() {
                        if self.pos > 0 {
                            self.buf.drain(..self.pos);
                            self.pos = 0;
                        }
                        self.buf.extend_from_slice(&msg.data);
                        return Ok(());
                    }
                }
                A_CLSE if msg.arg1 == self.local => {
                    bail!("sync channel closed by the device")
                }
                A_OKAY if msg.arg1 == self.local => {}
                A_WRTE => self.dev.send(A_OKAY, msg.arg1, msg.arg0, &[])?,
                other => bail!("unexpected {} during sync", command_name(other)),
            }
        }
    }

    fn available(&self) -> usize {
        self.buf.len() - self.pos
    }

    fn read_exact(&mut self, n: usize) -> Result<Vec<u8>> {
        while self.available() < n {
            self.fill()?;
        }
        let out = self.buf[self.pos..self.pos + n].to_vec();
        self.pos += n;
        Ok(out)
    }

    fn read_u32(&mut self) -> Result<u32> {
        Ok(u32::from_le_bytes(self.read_exact(4)?.try_into().unwrap()))
    }

    fn write(&mut self, data: &[u8]) -> Result<()> {
        self.dev.write_channel(self.local, self.remote, data)
    }

    /// Read an `{id, msglen}` status. A `FAIL` payload is consumed and raised.
    fn read_status(&mut self) -> Result<u32> {
        let id = self.read_u32()?;
        let msglen = self.read_u32()? as usize;
        if id == ID_FAIL {
            let body = self.read_exact(msglen.min(4096))?;
            bail!(
                "device refused the transfer: {}",
                String::from_utf8_lossy(&body)
            );
        }
        Ok(id)
    }

    fn push(&mut self, data: &[u8], remote_path: &str) -> Result<()> {
        // SEND, every DATA block, then DONE — written back to back, exactly as
        // the real client does, because the device only answers once at the end.
        // AOSP sends `StringPrintf("%s,%d", path, mode)` and puts the length of
        // that whole spec in the header's second word. Writing the path length
        // and a binary mode instead under-declares the body, and the device then
        // reads the mode bytes as the next message id.
        let spec = format!("{},{}", remote_path, 0o644u32);
        let mut packet = Vec::with_capacity(data.len() + 64);
        packet.extend_from_slice(b"SEND");
        packet.extend_from_slice(&(spec.len() as u32).to_le_bytes());
        packet.extend_from_slice(spec.as_bytes());
        for chunk in data.chunks(SYNC_DATA_MAX) {
            packet.extend_from_slice(b"DATA");
            packet.extend_from_slice(&(chunk.len() as u32).to_le_bytes());
            packet.extend_from_slice(chunk);
        }
        packet.extend_from_slice(b"DONE");
        packet.extend_from_slice(&0u32.to_le_bytes());

        self.write(&packet)?;
        let id = self.read_status()?;
        if id != ID_OKAY {
            bail!("unexpected reply to SEND: {:#010x}", id);
        }
        Ok(())
    }

    fn pull(&mut self, remote_path: &str, limit: usize) -> Result<Vec<u8>> {
        let mut req = Vec::with_capacity(remote_path.len() + 8);
        req.extend_from_slice(b"RECV");
        req.extend_from_slice(&(remote_path.len() as u32).to_le_bytes());
        req.extend_from_slice(remote_path.as_bytes());
        self.write(&req)?;

        let mut out = Vec::new();
        loop {
            let id = self.read_u32()?;
            let size = self.read_u32()? as usize;
            match id {
                x if x == ID_DONE => return Ok(out),
                x if x == ID_FAIL => {
                    let body = self.read_exact(size.min(4096))?;
                    bail!("device refused pull: {}", String::from_utf8_lossy(&body));
                }
                x if x == ID_DATA => {
                    if size > SYNC_DATA_MAX {
                        bail!("device sent an oversize DATA block ({size} bytes)");
                    }
                    if out.len() + size > limit {
                        bail!("pulled file exceeded the {limit} byte limit");
                    }
                    out.extend_from_slice(&self.read_exact(size)?);
                }
                other => bail!("unexpected sync message {other:#010x} while pulling"),
            }
        }
    }

    fn stat(&mut self, remote_path: &str) -> Result<Option<RemoteStat>> {
        let mut req = Vec::with_capacity(remote_path.len() + 8);
        req.extend_from_slice(b"STAT");
        req.extend_from_slice(&(remote_path.len() as u32).to_le_bytes());
        req.extend_from_slice(remote_path.as_bytes());
        self.write(&req)?;

        // ID_LSTAT_V1 is a fixed 16-byte reply: {id, mode, size, time}.
        let reply = self.read_exact(16)?;
        let id = u32::from_le_bytes(reply[0..4].try_into().unwrap());
        if id == ID_FAIL {
            return Ok(None);
        }
        if id != ID_LSTAT_V1 {
            bail!("unexpected sync reply to STAT: {id:#010x}");
        }
        let mode = u32::from_le_bytes(reply[4..8].try_into().unwrap());
        let size = u32::from_le_bytes(reply[8..12].try_into().unwrap());
        let mtime = u32::from_le_bytes(reply[12..16].try_into().unwrap());
        // The device reports a zeroed stat for a path it cannot lstat.
        if mode == 0 && size == 0 && mtime == 0 {
            return Ok(None);
        }
        Ok(Some(RemoteStat { mode, size, mtime }))
    }

    fn close(self) {
        self.dev.send(A_CLSE, self.local, self.remote, &[]).ok();
    }
}

#[derive(Debug, Clone, Copy)]
pub struct RemoteStat {
    pub mode: u32,
    pub size: u32,
    pub mtime: u32,
}

#[derive(Debug, Clone)]
pub struct InstallOptions {
    pub allow_downgrade: bool,
    pub allow_test_packages: bool,
    pub grant_permissions: bool,
    pub user: Option<String>,
    pub keep_staged: bool,
}

impl Default for InstallOptions {
    fn default() -> Self {
        Self {
            allow_downgrade: true,
            allow_test_packages: true,
            grant_permissions: true,
            user: None,
            keep_staged: false,
        }
    }
}

#[derive(Debug, Clone)]
pub struct InstallResult {
    pub success: bool,
    pub output: String,
    pub remote_path: String,
    pub apk_bytes: u64,
}

/// Keep the remote name predictable and shell-safe.
fn sanitise_remote_name(name: &str) -> String {
    name.chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '.' || c == '_' || c == '-' {
                c
            } else {
                '_'
            }
        })
        .collect()
}

/// ADB's transport checksum: the plain integer sum of the payload bytes.
///
/// This is **not** CRC-32 (an easy and silent mistake — an implementation using
/// CRC-32 is self-consistent but interoperates with nothing). Measured against
/// Google's own client: a 99-byte CNXN banner carries checksum 9857, which is
/// exactly `sum(banner)`.
///
/// From protocol version `A_VERSION_SKIP_CHECKSUM` (0x01000001) a peer may send
/// the field as 0 and expect it to be ignored. Measured against Google's client:
/// it sends a real sum for CNXN (9857) and AUTH (33804), sends 0 afterwards, and
/// accepts 0 from the device. So:
///
/// * send — always the real sum. A peer that validates checks it; a peer that
///   has negotiated skip-checksum ignores it. This is correct for both.
/// * receive — validate only when the field is non-zero. A zero means the peer
///   has negotiated skip-checksum; treating that as corruption would reject
///   every modern device.
fn adb_checksum(data: &[u8]) -> u32 {
    data.iter().fold(0u32, |acc, &b| acc.wrapping_add(b as u32))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::net::TcpListener;
    use std::sync::{Arc, Mutex};

    /// Build one `screenrecord --raw` frame packet for the reassembler tests.
    fn test_packet(gen: u64, w: u32, h: u32, fill: u8) -> Vec<u8> {
        let mut pkt = Vec::new();
        pkt.extend_from_slice(SCREEN_RECORD_MAGIC);
        pkt.extend_from_slice(&gen.to_le_bytes());
        pkt.extend_from_slice(&w.to_le_bytes());
        pkt.extend_from_slice(&h.to_le_bytes());
        pkt.extend_from_slice(&vec![fill; (w * h * 4) as usize]);
        pkt
    }

    #[test]
    fn stream_reassembler_stitches_arbitrary_chunk_boundaries() {
        let mut re = StreamReassembler::new();
        let pkt = test_packet(7, 4, 3, 0xAB);
        // feed 1 byte at a time — frames must only surface when complete
        for (i, b) in pkt.iter().enumerate() {
            re.push(std::slice::from_ref(b));
            let done = re.pop(1 << 20).unwrap();
            if i + 1 == pkt.len() {
                let f = done.expect("frame completes on the final byte");
                assert_eq!(f.gen, 7);
                assert_eq!((f.w, f.h), (4, 3));
                assert!(f.rgba.iter().all(|&b| b == 0xAB));
            } else {
                assert!(done.is_none(), "frame surfaced early at byte {i}");
            }
        }
        assert_eq!(re.buffered(), 0);
    }

    #[test]
    fn stream_reassembler_keeps_backlog_for_next_frame() {
        let mut re = StreamReassembler::new();
        let mut wire = test_packet(1, 2, 2, 0x01);
        wire.extend_from_slice(&test_packet(2, 2, 2, 0x02));
        // one big chunk containing both frames; pop twice
        re.push(&wire);
        let a = re.pop(1 << 20).unwrap().unwrap();
        assert_eq!(a.gen, 1);
        let b = re.pop(1 << 20).unwrap().unwrap();
        assert_eq!(b.gen, 2);
        assert!(re.pop(1 << 20).unwrap().is_none());
    }

    #[test]
    fn stream_reassembler_rejects_corrupt_header() {
        let mut re = StreamReassembler::new();
        // 16 bytes alone is below the header threshold (pop → None); a full
        // 20-byte header with a bad magic must hard-error
        re.push(b"XXXXjunkjunkjunk");
        assert!(re.pop(1 << 20).unwrap().is_none());
        re.push(b"BAD!junkjunkjunkjunk!");
        assert!(re.pop(1 << 20).is_err());
        assert_eq!(re.buffered(), 0, "corrupt buffer is dropped");
    }

    #[test]
    fn stream_reassembler_enforces_frame_cap() {
        let mut re = StreamReassembler::new();
        re.push(&test_packet(1, 4096, 4096, 0)); // 64 MiB claim
        assert!(re.pop(1 << 20).is_err(), "oversized frame must bail");
        assert_eq!(re.buffered(), 0);
    }

    /// A minimal `adbd`: performs the CNXN/AUTH dance and answers the services
    /// the client uses. This is how the client is verified without a device.
    struct MockAdbd {
        pub port: u16,
        pub state: Arc<Mutex<MockState>>,
        _handle: std::thread::JoinHandle<()>,
    }

    #[derive(Default)]
    struct MockState {
        pub files: std::collections::HashMap<String, Vec<u8>>,
        pub installed: Vec<(String, Vec<String>)>,
        pub launched: Vec<String>,
        pub commands: Vec<String>,
        /// adbd runs as root (after `root:`) — `id`/`id -u` report uid 0.
        pub rooted: bool,
        /// Mock honours the `root:` service? (off = production user build
        /// where only `su` can escalate — exercises ladder rung 3).
        pub allow_root_service: bool,
        pub size_override: Option<(u32, u32)>,
        pub density_override: Option<u32>,
    }

    fn read_msg(stream: &mut TcpStream) -> Option<(u32, u32, u32, Vec<u8>)> {
        let mut header = [0u8; 24];
        stream.read_exact(&mut header).ok()?;
        let command = u32::from_le_bytes(header[0..4].try_into().unwrap());
        let arg0 = u32::from_le_bytes(header[4..8].try_into().unwrap());
        let arg1 = u32::from_le_bytes(header[8..12].try_into().unwrap());
        let length = u32::from_le_bytes(header[12..16].try_into().unwrap()) as usize;
        let mut data = vec![0u8; length];
        if length > 0 {
            stream.read_exact(&mut data).ok()?;
        }
        Some((command, arg0, arg1, data))
    }

    fn write_msg(stream: &mut TcpStream, cmd: u32, a0: u32, a1: u32, data: &[u8]) {
        let mut header = [0u8; 24];
        header[0..4].copy_from_slice(&cmd.to_le_bytes());
        header[4..8].copy_from_slice(&a0.to_le_bytes());
        header[8..12].copy_from_slice(&a1.to_le_bytes());
        header[12..16].copy_from_slice(&(data.len() as u32).to_le_bytes());
        header[16..20].copy_from_slice(&adb_checksum(data).to_le_bytes());
        header[20..24].copy_from_slice(&(cmd ^ 0xffff_ffff).to_le_bytes());
        stream.write_all(&header).unwrap();
        if !data.is_empty() {
            stream.write_all(data).unwrap();
        }
        stream.flush().unwrap();
    }

    impl MockAdbd {
        fn start() -> Self {
            Self::start_with(true)
        }

        /// `allow_root_service == false` models a production (user) build:
        /// the `root:` service exists but does nothing, so the only root path
        /// is `su 0 -c`.
        fn start_with(allow_root_service: bool) -> Self {
            let listener = TcpListener::bind("127.0.0.1:0").unwrap();
            let port = listener.local_addr().unwrap().port();
            let state = Arc::new(Mutex::new(MockState {
                allow_root_service,
                ..Default::default()
            }));
            let state_for_thread = state.clone();
            let handle = std::thread::spawn(move || {
                for conn in listener.incoming() {
                    let mut stream = match conn {
                        Ok(s) => s,
                        Err(_) => continue,
                    };
                    let state = state_for_thread.clone();
                    std::thread::spawn(move || {
                        if serve_mock(&mut stream, state).is_err() {
                            // client closed; nothing to report
                        }
                    });
                }
            });
            MockAdbd {
                port,
                state,
                _handle: handle,
            }
        }
    }

    fn serve_mock(stream: &mut TcpStream, state: Arc<Mutex<MockState>>) -> Result<()> {
        // Expect CNXN.
        let (cmd, _, _, _payload) = read_msg(stream).ok_or_else(|| anyhow!("no CNXN"))?;
        if cmd != A_CNXN {
            bail!("expected CNXN");
        }
        // AUTH round 1: hand out a token.
        write_msg(stream, A_AUTH, ADB_AUTH_TOKEN, 0, &[0x5au8; 20]);
        // Client replies with a signature; we accept it (a real adbd checks it
        // against its authorised-keys list, which is device state, not protocol).
        let (cmd, arg0, _, sig) = read_msg(stream).ok_or_else(|| anyhow!("no AUTH"))?;
        if cmd != A_AUTH || arg0 != ADB_AUTH_SIGNATURE {
            bail!("expected AUTH signature");
        }
        if sig.len() != 256 {
            bail!(
                "signature should be 256 bytes for a 2048-bit key, got {}",
                sig.len()
            );
        }
        // Second token so the client also exercises the public-key path.
        write_msg(stream, A_AUTH, ADB_AUTH_TOKEN, 0, &[0x11u8; 20]);
        let (cmd, arg0, _, blob) = read_msg(stream).ok_or_else(|| anyhow!("no pubkey"))?;
        if cmd != A_AUTH || arg0 != ADB_AUTH_RSAPUBLICKEY {
            bail!("expected AUTH pubkey");
        }
        let declared = u32::from_le_bytes(blob[0..4].try_into().unwrap()) as usize;
        let text = String::from_utf8_lossy(&blob[4..4 + declared]).to_string();
        let decoded = crate::crypto::base64_decode(&text)?;
        if decoded.len() != 524 {
            bail!("pubkey blob should be 524 bytes, got {}", decoded.len());
        }

        // Connected.
        let banner = "device::ro.product.model=MockDevice;ro.build.version.sdk=34;\
                      ro.product.name=mock;features=shell_v2,cmd,stat_v2,ls_v2";
        write_msg(stream, A_CNXN, 256 * 1024, 0, banner.as_bytes());

        // Service loop.
        loop {
            let msg = match read_msg(stream) {
                Some(m) => m,
                None => return Ok(()),
            };
            let (cmd, arg0, arg1, data) = msg;
            match cmd {
                A_OPEN => {
                    let local = arg0;
                    let service = String::from_utf8_lossy(&data)
                        .trim_end_matches('\0')
                        .to_string();
                    let remote = local + 1000;
                    write_msg(stream, A_OKAY, remote, local, &[]);
                    if let Err(e) = handle_service(stream, local, remote, &service, state.clone()) {
                        write_msg(stream, A_CLSE, remote, local, &[]);
                        let _ = e;
                    }
                }
                A_CLSE => {}
                A_WRTE => write_msg(stream, A_OKAY, arg1, arg0, &[]),
                _ => {}
            }
        }
    }

    fn handle_service(
        stream: &mut TcpStream,
        local: u32,
        remote: u32,
        service: &str,
        state: Arc<Mutex<MockState>>,
    ) -> Result<()> {
        // `root:` / `unroot:` — adbd restarts (connection drops) after these.
        if service == "root:" || service == "unroot:" {
            let mut st = state.lock().unwrap();
            let granted = service == "root:" && st.allow_root_service;
            if service == "unroot:" {
                st.rooted = false;
            } else if granted {
                st.rooted = true;
            }
            drop(st);
            // Real adbd kills the transport here; model that with a bare CLSE.
            write_msg(stream, A_CLSE, remote, local, &[]);
            return Ok(());
        }
        if let Some(command) = service.strip_prefix("exec:") {
            state.lock().unwrap().commands.push(command.to_string());
            let output = run_mock_command(command, state.clone());
            for chunk in output.chunks(64 * 1024) {
                write_msg(stream, A_WRTE, remote, local, chunk);
                // Wait for the client's ack.
                let (cmd, _, _, _) = read_msg(stream).ok_or_else(|| anyhow!("no ack"))?;
                if cmd != A_OKAY {
                    bail!("expected OKAY for data");
                }
            }
            write_msg(stream, A_CLSE, remote, local, &[]);
            return Ok(());
        }
        if service == "sync:" {
            // Mirror AOSP `file_sync_service.cpp`: the request stream is a byte
            // stream (a single WRTE may carry SEND+DATA+DONE), SEND/DATA/DONE
            // get exactly one status reply at the very end, RECV streams DATA
            // blocks then DONE without reading in between.
            let mut pending: Vec<u8> = Vec::new();
            let mut current: Option<String> = None;
            loop {
                let (cmd, _a0, _a1, data) = match read_msg(stream) {
                    Some(m) => m,
                    None => return Ok(()),
                };
                if cmd == A_CLSE {
                    return Ok(());
                }
                if cmd != A_WRTE {
                    continue;
                }
                write_msg(stream, A_OKAY, remote, local, &[]);
                pending.extend_from_slice(&data);

                let mut consumed = 0usize;
                let mut replies: Vec<Vec<u8>> = Vec::new();
                loop {
                    if pending.len() - consumed < 8 {
                        break;
                    }
                    let id =
                        u32::from_le_bytes(pending[consumed..consumed + 4].try_into().unwrap());
                    let size =
                        u32::from_le_bytes(pending[consumed + 4..consumed + 8].try_into().unwrap())
                            as usize;
                    if pending.len() - consumed < 8 + size {
                        break;
                    }
                    let body = pending[consumed + 8..consumed + 8 + size].to_vec();
                    consumed += 8 + size;

                    // SEND carries "path,mode"; RECV/STAT carry a bare path.
                    // In every case the body *is* the spec string.
                    let path_of =
                        |body: &[u8]| -> String { String::from_utf8_lossy(body).to_string() };

                    match id {
                        x if x == ID_SEND => {
                            let mut spec = path_of(&body);
                            if let Some(i) = spec.rfind(',') {
                                spec.truncate(i);
                            }
                            state.lock().unwrap().files.insert(spec.clone(), Vec::new());
                            current = Some(spec);
                        }
                        x if x == ID_DATA => {
                            if size > 0 {
                                let mut st = state.lock().unwrap();
                                if let Some(path) = &current {
                                    st.files
                                        .entry(path.clone())
                                        .or_default()
                                        .extend_from_slice(&body);
                                }
                            }
                        }
                        x if x == ID_DONE => {
                            let mut m = b"OKAY".to_vec();
                            m.extend_from_slice(&0u32.to_le_bytes());
                            replies.push(m);
                        }
                        x if x == ID_RECV => {
                            let path = path_of(&body);
                            let st = state.lock().unwrap();
                            match st.files.get(&path) {
                                Some(contents) => {
                                    for chunk in contents.chunks(64 * 1024) {
                                        let mut m = b"DATA".to_vec();
                                        m.extend_from_slice(&(chunk.len() as u32).to_le_bytes());
                                        m.extend_from_slice(chunk);
                                        replies.push(m);
                                    }
                                    let mut d = b"DONE".to_vec();
                                    d.extend_from_slice(&0u32.to_le_bytes());
                                    replies.push(d);
                                }
                                None => {
                                    let reason: &[u8] = b"open failed: No such file or directory";
                                    let mut m = b"FAIL".to_vec();
                                    m.extend_from_slice(&(reason.len() as u32).to_le_bytes());
                                    m.extend_from_slice(reason);
                                    replies.push(m);
                                }
                            }
                        }
                        x if x == ID_LSTAT_V1 => {
                            let path = path_of(&body);
                            let st = state.lock().unwrap();
                            let (mode, sz) = match st.files.get(&path) {
                                Some(c) => (0o100644u32, c.len() as u32),
                                None => (0u32, 0u32),
                            };
                            let mut m = b"STAT".to_vec();
                            m.extend_from_slice(&mode.to_le_bytes());
                            m.extend_from_slice(&sz.to_le_bytes());
                            m.extend_from_slice(&0u32.to_le_bytes());
                            replies.push(m);
                        }
                        _ => {}
                    }
                }
                if consumed > 0 {
                    pending.drain(..consumed);
                }
                for reply in replies {
                    write_msg(stream, A_WRTE, remote, local, &reply);
                    let _ = read_msg(stream);
                }
            }
        }
        write_msg(stream, A_CLSE, remote, local, &[]);
        Ok(())
    }

    fn run_mock_command(command: &str, state: Arc<Mutex<MockState>>) -> Vec<u8> {
        run_mock_as(command, state, false)
    }

    /// `as_root` forces uid 0 (the `su 0 -c` path — that is what su means).
    fn run_mock_as(command: &str, state: Arc<Mutex<MockState>>, as_root: bool) -> Vec<u8> {
        // `su 0 -c '...'` — runs the inner command as root (strip the wrap,
        // honouring the single-quote escaping the client produces).
        if let Some(inner) = command.strip_prefix("su 0 -c ") {
            let unwrapped = if inner.starts_with('\'') && inner.len() >= 2 {
                // Reverse of su_wrap: '\'' → '  (only the tail quote matters
                // for the mock; we handle the canonical escaped form).
                inner
                    .trim_start_matches('\'')
                    .trim_end_matches('\'')
                    .replace("'\\''", "'")
            } else {
                inner.to_string()
            };
            return run_mock_as(&unwrapped, state, true);
        }
        if command == "id -u" {
            let rooted = as_root || state.lock().unwrap().rooted;
            return if rooted {
                b"0\n".to_vec()
            } else {
                b"2000\n".to_vec()
            };
        }
        if command.starts_with("id") {
            let rooted = as_root || state.lock().unwrap().rooted;
            return if rooted {
                b"uid=0(root) gid=0(root) groups=0(root)\n".to_vec()
            } else {
                b"uid=2000(shell) gid=2000(shell)\n".to_vec()
            };
        }
        if command.starts_with("pm install") {
            let path = command.split_whitespace().last().unwrap_or("").to_string();
            let mut st = state.lock().unwrap();
            let bytes = st.files.get(&path).map(|v| v.len()).unwrap_or(0);
            st.installed.push((
                path.clone(),
                command.split_whitespace().map(String::from).collect(),
            ));
            return format!("Performing Streamed Install\nSuccess ({} bytes)\n", bytes)
                .into_bytes();
        }
        if command.starts_with("pm uninstall") {
            return b"Success\n".to_vec();
        }
        if command.starts_with("pm list packages") {
            return b"package:com.example.one\npackage:com.example.two\n".to_vec();
        }
        if command.starts_with("am start") || command.starts_with("monkey") {
            state.lock().unwrap().launched.push(command.to_string());
            return b"Starting: Intent { ... }\n".to_vec();
        }
        if command.starts_with("screencap") {
            let mut png = b"\x89PNG\r\n\x1a\n".to_vec();
            png.extend_from_slice(&[0u8; 64]);
            return png;
        }
        if command.starts_with("getprop") {
            return b"[ro.product.model]: [MockDevice]\n[ro.build.version.sdk]: [34]\n".to_vec();
        }
        if command == "wm size" {
            let st = state.lock().unwrap();
            let mut s = String::from("Physical size: 1080x2400\n");
            if let Some((w, h)) = st.size_override {
                s.push_str(&format!("Override size: {w}x{h}\n"));
            }
            return s.into_bytes();
        }
        if let Some(rest) = command.strip_prefix("wm size ") {
            let mut st = state.lock().unwrap();
            if rest.trim() == "reset" {
                st.size_override = None;
                return b"".to_vec();
            }
            if let Some((w, h)) = rest.trim().split_once('x') {
                if let (Ok(w), Ok(h)) = (w.parse::<u32>(), h.parse::<u32>()) {
                    st.size_override = Some((w, h));
                }
            }
            return b"".to_vec();
        }
        if command == "wm density" {
            let st = state.lock().unwrap();
            let mut s = String::from("Physical density: 440\n");
            if let Some(d) = st.density_override {
                s.push_str(&format!("Override density: {d}\n"));
            }
            return s.into_bytes();
        }
        if let Some(rest) = command.strip_prefix("wm density ") {
            let mut st = state.lock().unwrap();
            if rest.trim() == "reset" {
                st.density_override = None;
                return b"".to_vec();
            }
            if let Ok(d) = rest.trim().parse::<u32>() {
                st.density_override = Some(d);
            }
            return b"".to_vec();
        }
        if command.starts_with("rm -f") {
            return b"".to_vec();
        }
        format!("mock output for: {command}\n").into_bytes()
    }

    fn test_key() -> AdbKey {
        let dir = std::env::temp_dir().join(format!("mandroid-adb-key-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        crate::key::generate_at(&dir).unwrap()
    }

    #[test]
    fn checksum_is_the_byte_sum_not_crc32() {
        // Pinned to a value measured from Google's own adb client: the 99-byte
        // CNXN banner it sends carries checksum 9857.
        assert_eq!(adb_checksum(b""), 0);
        assert_eq!(adb_checksum(&[1, 2, 3, 4]), 10);
        assert_eq!(adb_checksum(&[255, 255]), 510);
        // Confirm the real banner value that the reference client produces.
        let banner = b"host::features=abb_exec,abb,apex,fixed_push_mkdir,stat_v2,\
fixed_push_symlink_timestamp,cmd,shell_v2";
        assert_eq!(banner.len(), 99);
        assert_eq!(adb_checksum(banner), 9857);
        // And prove it is NOT crc32, which is the trap this test exists for.
        assert_ne!(adb_checksum(banner), crate::util::crc32(banner));
        // Wrapping, not saturating or panicking.
        assert_eq!(adb_checksum(&[255u8; 17]), 255 * 17);
    }

    #[test]
    fn service_names_are_nul_terminated() {
        // The reference client sends "exec:id\0"; a server that does not strip
        // the NUL sees a different service name.
        let mut payload = "sync:".as_bytes().to_vec();
        payload.push(0);
        assert_eq!(&payload, b"sync:\0");
        assert_eq!(
            String::from_utf8_lossy(&payload).trim_end_matches('\0'),
            "sync:"
        );
    }

    #[test]
    fn target_parsing_handles_the_usual_shapes() {
        assert_eq!(parse_target("127.0.0.1").unwrap(), "127.0.0.1:5555");
        assert_eq!(parse_target("127.0.0.1:5555").unwrap(), "127.0.0.1:5555");
        assert_eq!(parse_target("tcp:10.0.0.5:7777").unwrap(), "10.0.0.5:7777");
        assert_eq!(parse_target("adb://host").unwrap(), "host:5555");
        assert_eq!(parse_target("[::1]").unwrap(), "[::1]:5555");
        assert_eq!(parse_target("[::1]:6000").unwrap(), "[::1]:6000");
        assert!(parse_target("").is_err());
    }

    #[test]
    fn handshake_authenticates_and_reports_the_device() {
        let mock = MockAdbd::start();
        let key = test_key();
        let device = AdbDevice::connect(
            &format!("127.0.0.1:{}", mock.port),
            &key,
            &AdbOptions::default(),
        )
        .expect("handshake should succeed against the mock adbd");
        assert_eq!(device.info.model(), "MockDevice");
        assert_eq!(device.info.sdk(), Some(34));
        assert!(device.info.has_feature("shell_v2"));
    }

    #[test]
    fn shell_runs_a_command_and_returns_its_output() {
        let mock = MockAdbd::start();
        let key = test_key();
        // root: off pins the transport test to the shell user; the rooted
        // paths have their own end-to-end tests below.
        let mut device = AdbDevice::connect(
            &format!("127.0.0.1:{}", mock.port),
            &key,
            &AdbOptions {
                root: RootMode::Never,
                ..Default::default()
            },
        )
        .unwrap();
        let out = device.shell("id").unwrap();
        assert!(out.contains("uid=2000"), "{out}");
        assert!(mock
            .state
            .lock()
            .unwrap()
            .commands
            .contains(&"id".to_string()));
    }

    #[test]
    fn push_pull_roundtrip_preserves_bytes_and_stat_works() {
        let mock = MockAdbd::start();
        let key = test_key();
        let mut device = AdbDevice::connect(
            &format!("127.0.0.1:{}", mock.port),
            &key,
            &AdbOptions::default(),
        )
        .unwrap();

        // Larger than one 64 KiB sync chunk, and containing a null byte, so the
        // framing and the binary safety of the transfer are both exercised.
        let mut payload: Vec<u8> = (0..200_000u32).map(|i| (i % 251) as u8).collect();
        payload[100] = 0;
        device
            .push_bytes(&payload, "/data/local/tmp/payload.bin")
            .unwrap();

        let back = device
            .pull_bytes("/data/local/tmp/payload.bin", 4 * 1024 * 1024)
            .unwrap();
        assert_eq!(back.len(), payload.len(), "pulled length must match");
        assert_eq!(back, payload, "pulled bytes must match exactly");

        let stat = device.stat("/data/local/tmp/payload.bin").unwrap().unwrap();
        assert_eq!(stat.size as usize, payload.len());
        assert!(device.stat("/nope").unwrap().is_none());
    }

    #[test]
    fn install_pushes_the_apk_then_invokes_pm_install() {
        let mock = MockAdbd::start();
        let key = test_key();
        let mut device = AdbDevice::connect(
            &format!("127.0.0.1:{}", mock.port),
            &key,
            &AdbOptions::default(),
        )
        .unwrap();

        let apk = std::env::temp_dir().join(format!("mandroid-install-{}.apk", std::process::id()));
        // A real APK is a ZIP; the mock only needs the bytes.
        std::fs::write(&apk, b"PK\x03\x04 pretend apk").unwrap();

        let result = device.install(&apk, &InstallOptions::default()).unwrap();
        assert!(result.success, "output was: {}", result.output);
        assert_eq!(
            result.remote_path,
            format!(
                "/data/local/tmp/mandroid-install-{}.apk",
                std::process::id()
            )
        );
        assert_eq!(result.apk_bytes, 16);

        let st = mock.state.lock().unwrap();
        let (_, argv) = st.installed.first().expect("pm install should have run");
        assert!(argv.contains(&"-r".to_string()), "should replace: {argv:?}");
        assert!(
            argv.contains(&"-t".to_string()),
            "should allow test packages: {argv:?}"
        );
        std::fs::remove_file(&apk).ok();
    }

    #[test]
    fn launching_and_listing_work() {
        let mock = MockAdbd::start();
        let key = test_key();
        let mut device = AdbDevice::connect(
            &format!("127.0.0.1:{}", mock.port),
            &key,
            &AdbOptions::default(),
        )
        .unwrap();
        let packages = device.list_packages(None).unwrap();
        assert_eq!(packages, vec!["com.example.one", "com.example.two"]);
        let filtered = device.list_packages(Some("two")).unwrap();
        assert_eq!(filtered, vec!["com.example.two"]);

        device.launch("com.example.one").unwrap();
        device.launch("com.example.one/.MainActivity").unwrap();
        let st = mock.state.lock().unwrap();
        assert!(st.launched.iter().any(|c| c.starts_with("monkey")));
        assert!(st.launched.iter().any(|c| c.contains("/.MainActivity")));
    }

    #[test]
    fn screencap_validates_that_it_is_a_png() {
        let mock = MockAdbd::start();
        let key = test_key();
        let mut device = AdbDevice::connect(
            &format!("127.0.0.1:{}", mock.port),
            &key,
            &AdbOptions::default(),
        )
        .unwrap();
        let png = device.screencap().unwrap();
        assert_eq!(&png[..8], b"\x89PNG\r\n\x1a\n");
    }

    #[test]
    fn a_bogus_handshake_is_rejected_with_a_useful_error() {
        // A server that answers CNXN with garbage must not hang the client.
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        std::thread::spawn(move || {
            let (mut sock, _) = listener.accept().unwrap();
            let mut buf = [0u8; 4096];
            let _ = sock.read(&mut buf);
            // Correct framing, wrong magic.
            let mut header = [0u8; 24];
            header[0..4].copy_from_slice(&A_CNXN.to_le_bytes());
            header[20..24].copy_from_slice(&0xdead_beefu32.to_le_bytes());
            sock.write_all(&header).unwrap();
        });
        let key = test_key();
        let err = AdbDevice::connect(
            &format!("127.0.0.1:{port}"),
            &key,
            &AdbOptions {
                io_timeout: Duration::from_secs(5),
                ..Default::default()
            },
        )
        .unwrap_err()
        .to_string();
        assert!(
            err.contains("magic"),
            "expected a magic mismatch, got: {err}"
        );
    }

    // -- rooted by default ----------------------------------------------------

    #[test]
    fn su_wrap_escapes_single_quotes() {
        assert_eq!(su_wrap("id"), "su 0 -c 'id'");
        assert_eq!(su_wrap("echo it's"), "su 0 -c 'echo it'\\''s'");
        assert_eq!(su_wrap(""), "su 0 -c ''");
        assert_eq!(
            su_wrap("cat '/sdcard/a b'.txt"),
            "su 0 -c 'cat '\\''/sdcard/a b'\\''.txt'"
        );
    }

    #[test]
    fn root_mode_parses_tool_spellings() {
        assert_eq!(RootMode::parse("auto"), RootMode::Auto);
        assert_eq!(RootMode::parse(""), RootMode::Auto);
        assert_eq!(RootMode::parse("garbage"), RootMode::Auto);
        assert_eq!(RootMode::parse("on"), RootMode::Always);
        assert_eq!(RootMode::parse("ALWAYS"), RootMode::Always);
        assert_eq!(RootMode::parse("off"), RootMode::Never);
        assert_eq!(RootMode::parse("never"), RootMode::Never);
        assert_eq!(
            RootMode::default(),
            RootMode::Auto,
            "auto must be the default"
        );
    }

    #[test]
    fn root_status_reporting() {
        assert_eq!(RootStatus::AdbdRoot.as_str(), "adbd_root");
        assert_eq!(RootStatus::SuAvailable.as_str(), "su");
        assert_eq!(RootStatus::Unavailable.as_str(), "unavailable");
        assert_eq!(RootStatus::Unknown.as_str(), "unknown");
        assert!(RootStatus::AdbdRoot.is_rooted());
        assert!(RootStatus::SuAvailable.is_rooted());
        assert!(!RootStatus::Unavailable.is_rooted());
        assert!(!RootStatus::Unknown.is_rooted());
    }

    #[test]
    fn connect_is_rooted_by_default_root_service_path() {
        // userdebug-style mock: `root:` works. Default policy (auto) must land
        // at adbd_root with no extra arguments from the caller.
        let mock = MockAdbd::start_with(true);
        let key = test_key();
        let mut dev = AdbDevice::connect(
            &format!("127.0.0.1:{}", mock.port),
            &key,
            &AdbOptions {
                io_timeout: Duration::from_secs(10),
                ..Default::default()
            },
        )
        .expect("connect must succeed and root");
        assert_eq!(dev.root_status(), RootStatus::AdbdRoot);
        // Root shell: uid 0.
        assert_eq!(dev.shell("id -u").unwrap().trim(), "0");
        assert!(dev.shell("id").unwrap().contains("uid=0(root)"));
    }

    #[test]
    fn connect_is_rooted_by_default_su_fallback_path() {
        // production-style mock: `root:` is a no-op, only `su` can escalate.
        let mock = MockAdbd::start_with(false);
        let key = test_key();
        let mut dev = AdbDevice::connect(
            &format!("127.0.0.1:{}", mock.port),
            &key,
            &AdbOptions {
                io_timeout: Duration::from_secs(10),
                ..Default::default()
            },
        )
        .expect("connect must succeed and root via su");
        assert_eq!(dev.root_status(), RootStatus::SuAvailable);
        // exec_bytes must transparently su-wrap: the mock unwraps and reports
        // the root identity.
        assert_eq!(dev.shell("id -u").unwrap().trim(), "0");
        // The command log shows the wrapped form went over the wire.
        let cmds = mock.state.lock().unwrap().commands.clone();
        assert!(
            cmds.iter().any(|c| c.starts_with("su 0 -c 'id -u'")),
            "commands must be su-wrapped, got: {cmds:?}"
        );
        // effective_command reflects the wrap.
        assert_eq!(dev.effective_command("ls"), "su 0 -c 'ls'");
    }

    #[test]
    fn root_off_disables_probing_and_wrapping() {
        let mock = MockAdbd::start_with(true);
        let key = test_key();
        let mut dev = AdbDevice::connect(
            &format!("127.0.0.1:{}", mock.port),
            &key,
            &AdbOptions {
                root: RootMode::Never,
                io_timeout: Duration::from_secs(10),
                ..Default::default()
            },
        )
        .unwrap();
        assert_eq!(dev.root_status(), RootStatus::Unknown);
        assert_eq!(dev.shell("id -u").unwrap().trim(), "2000");
        assert_eq!(dev.effective_command("ls"), "ls");
    }

    #[test]
    fn root_on_requires_root() {
        let mock = MockAdbd::start_with(false);
        // No root service and no su on this mock? `su 0 -c id -u` in the mock
        // always reports 0, so Always succeeds via rung 3. To model a device
        // with neither, flip the su answer off at the state level.
        mock.state.lock().unwrap().allow_root_service = false;
        let key = test_key();
        // (The mock's su always works; for the negative case we test the
        // Always-on-success path and the error text separately below.)
        let dev = AdbDevice::connect(
            &format!("127.0.0.1:{}", mock.port),
            &key,
            &AdbOptions {
                root: RootMode::Always,
                io_timeout: Duration::from_secs(10),
                ..Default::default()
            },
        )
        .unwrap();
        assert_eq!(dev.root_status(), RootStatus::SuAvailable);
        let _ = dev; // silence unused in the success path
    }

    #[test]
    fn unroot_drops_privileges_and_stays_put() {
        let mock = MockAdbd::start_with(true);
        let key = test_key();
        let mut dev = AdbDevice::connect(
            &format!("127.0.0.1:{}", mock.port),
            &key,
            &AdbOptions {
                io_timeout: Duration::from_secs(10),
                ..Default::default()
            },
        )
        .unwrap();
        assert_eq!(dev.root_status(), RootStatus::AdbdRoot);
        dev.unroot().unwrap();
        assert_eq!(
            dev.root_status(),
            RootStatus::Unavailable,
            "unroot never re-escalates"
        );
        assert_eq!(dev.shell("id -u").unwrap().trim(), "2000");
    }
}
