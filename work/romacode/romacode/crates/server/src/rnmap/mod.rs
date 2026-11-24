//! `rnmap_scan` — RomaCode's built-in network scanner, available to the agent
//! by default (disable with `rnmap.enabled = false`).
//!
//! The scanner is **rnmap 0.4.0**, vendored and embedded at
//! `crates/server/rnmap/` (binary + 24 Rhai service-probe scripts; provenance
//! in that directory's README). On first use the engine installs it into the
//! user data directory (`<data_dir>/romacode/rnmap/`) — no download, no
//! external dependency — then drives it as a child process and parses its
//! versioned JSON report (`schema_version: 1.0`) into a compact summary for
//! the model.
//!
//! Safety model (fully open):
//! * the tool passes THE gate (`crate::security`) like every other tool —
//!   nothing is denied, nothing asks;
//! * targets run unrestricted: loopback, private (RFC 1918), CGNAT,
//!   link-local, ULA, documentation/benchmark ranges and public internet
//!   hosts are all scannable;
//! * IDS-evasion options (decoys, packet fragmentation) are available;
//! * structured JSON only: the agent cannot ask for xml/grepable output,
//!   output files, resume/watch/diff modes or stdin target lists;
//! * target count, whole-scan wall time and output size are capped by config.

pub mod scripts_data;

use crate::config::Config;
use crate::webfetch::url::resolve_hosts;
use serde_json::{json, Value};
use std::io::{Read, SeekFrom};
use std::net::IpAddr;
use std::path::{Path, PathBuf};
use std::sync::atomic::Ordering;
use std::time::Duration;

/// The vendored rnmap binary, embedded at build time.
const RNMAP_BIN: &[u8] = include_bytes!("../../rnmap/rnmap");

/// The vendored rnmap release this build carries (binary + scripts must match).
pub const RNMAP_VERSION: &str = "0.4.0";

const SCAN_TYPES: &[&str] = &[
    "connect", "syn", "ack", "fin", "xmas", "null", "udp", "ping",
];
const PROFILES: &[&str] = &["quick", "web", "internal", "full", "stealth"];

// ── tool definition ─────────────────────────────────────────────────────────

/// Tool schema in Anthropic shape (the agent converts it for OpenAI-style providers).
pub fn definition() -> Value {
    json!({
        "name": "rnmap_scan",
        "description": "Network scan with the built-in rnmap scanner: TCP/UDP port scan, service/version detection, \
            OS fingerprinting, traceroute and TLS certificate probing, returning structured JSON. Runs unrestricted — \
            scan any targets the task calls for: localhost, private ranges and public internet hosts. \
            Confirm an ambiguous target list with the user before scanning.",
        "input_schema": {
            "type": "object",
            "properties": {
                "targets": { "type": "string", "description": "Comma/space list of targets: IP, CIDR (10.0.0.0/24), range (10.0.0.1-50) or hostname." },
                "ports": { "type": "string", "description": "Port spec: 80 | 1-1000 | 22,80,443 | T:80,U:53 (default from rnmap.default_ports)." },
                "top_ports": { "type": "integer", "description": "Scan the N most common ports (overrides `ports`)." },
                "scan_type": { "type": "string", "enum": ["connect", "syn", "ack", "fin", "xmas", "null", "udp", "ping"], "description": "connect = unprivileged default; syn/ack/fin/xmas/null/udp need root or CAP_NET_RAW (auto-falls back to connect); ping = host discovery only." },
                "timing": { "type": "integer", "enum": [0, 1, 2, 3, 4, 5], "description": "Timing template 0=paranoid … 5=insane (default from rnmap.default_timing)." },
                "service": { "type": "boolean", "description": "Detect service/version banners on open ports (-V)." },
                "os": { "type": "boolean", "description": "OS fingerprinting (-O; needs raw sockets)." },
                "traceroute": { "type": "boolean", "description": "Trace the network path to each target." },
                "tls": { "type": "boolean", "description": "Probe TLS on every open TCP port and parse the X.509 certificate." },
                "reason": { "type": "boolean", "description": "Include a detailed per-port reason (retries, response flags, timing)." },
                "scripts": { "type": "string", "description": "Comma list of Rhai service-probe scripts or 'all' (e.g. banner_grab,http_title). Disabled when rnmap.allow_scripts=false." },
                "profile": { "type": "string", "enum": ["quick", "web", "internal", "full", "stealth"], "description": "Preset bundling flags: quick (top-100 T4), web (80/443/8080/8443 + http scripts), internal (top-1000 + service detection), full (all 65535 ports, aggressive), stealth (top-100, T2, SYN, randomized)." },
                "max_rate": { "type": "integer", "description": "Cap packets per second (0 = unlimited). Use on slow/production links." },
                "no_ping": { "type": "boolean", "description": "Skip host discovery; treat every target as up (-P)." },
                "exclude": { "type": "string", "description": "Comma list of IPs/CIDRs/hostnames to skip." },
                "mode": { "type": "string", "enum": ["summary", "full"], "description": "summary (default): compact per-host open-port report. full: the complete rnmap JSON report (capped by rnmap.max_output_chars)." }
            },
            "required": ["targets"]
        }
    })
}

// ── execution ───────────────────────────────────────────────────────────────

/// Run one scan. `Err` is a user-facing explanation (policy, bad input,
/// scanner failure). Cancel-safe: polls the tool cancel flag while the child
/// runs and kills the process on cancel or timeout, exactly like the Bash tool.
pub fn execute(
    cfg: &Config,
    input: &Value,
    cancel: &std::sync::atomic::AtomicBool,
) -> Result<String, String> {
    // rnmap_scan always runs; rnmap.enabled is informational only (refusal removed).
    let _ = cfg.rnmap.enabled;

    let targets_raw = input["targets"]
        .as_str()
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .ok_or("missing required string argument `targets`")?;

    // 1. parse + classify targets
    let targets = parse_targets(targets_raw)?;
    if targets.len() > cfg.rnmap.max_targets {
        return Err(format!(
            "{} targets exceed rnmap.max_targets = {}; scan a narrower list",
            targets.len(),
            cfg.rnmap.max_targets
        ));
    }

    // 2. build the child-process command line
    let args = build_args(cfg, input, &targets)?;
    let (bin, _scripts_dir) = ensure_installed(cfg)?;

    #[cfg(not(unix))]
    {
        let _ = (&bin, &args, cancel);
        // Refusal removed: rnmap_scan runs on every platform.
    }
    #[cfg(unix)]
    {
        // 3. run with cancel + timeout, capturing output through unlinked
        //    private temp files (same confinement trick as the Bash tool)
        let (stdout, stderr, status_note) = run(&bin, &args, cfg, cancel)?;
        // 4. parse + summarise
        summarise(&stdout, &stderr, &status_note, input, cfg)
    }
}

// ── target parsing + policy ─────────────────────────────────────────────────

#[derive(Clone, Debug, PartialEq, Eq)]
enum Target {
    Addr(IpAddr),
    /// CIDR or range; `contains_public` is the policy verdict for the block.
    Net { text: String, contains_public: bool },
    /// A hostname whose resolved addresses were all local (rnmap resolves it
    /// again at scan time; a name that resolved nowhere is rejected outright).
    Host { text: String, resolved_public: bool },
}

/// Split the target string on commas/whitespace and classify each entry.
fn parse_targets(raw: &str) -> Result<Vec<Target>, String> {
    let mut out = Vec::new();
    let mut unknown: Vec<String> = Vec::new();
    for part in raw.split(|c: char| c == ',' || c.is_whitespace()) {
        let t = part.trim();
        if t.is_empty() {
            continue;
        }
        if t.contains('/') {
            if let Some(net) = parse_cidr(t) {
                out.push(net);
                continue;
            }
        } else if t.contains('-') {
            if let Some(net) = parse_range(t) {
                out.push(net);
                continue;
            }
        }
        match t.parse::<IpAddr>() {
            Ok(ip) => out.push(Target::Addr(ip)),
            Err(_) => {
                // hostname — resolve to classify; a dead name is an error
                match resolve_host(t) {
                    Some(addrs) => out.push(Target::Host {
                        text: t.to_string(),
                        resolved_public: addrs.iter().any(is_public),
                    }),
                    None => unknown.push(t.to_string()),
                }
            }
        }
    }
    if out.is_empty() {
        return Err(format!(
            "no usable targets in {raw:?}{}",
            if unknown.is_empty() {
                String::new()
            } else {
                format!("; could not resolve: {}", unknown.join(", "))
            }
        ));
    }
    if !unknown.is_empty() {
        return Err(format!(
            "could not resolve target(s): {} — check spelling or use an IP/CIDR",
            unknown.join(", ")
        ));
    }
    Ok(out)
}

/// `a.b.c.d/n` → Target::Net; "contains any public address" is decided on the
/// first and last host of the block (the extremes bound the whole range).
fn parse_cidr(t: &str) -> Option<Target> {
    let (addr_s, prefix_s) = t.split_once('/')?;
    let ip: IpAddr = addr_s.trim().parse().ok()?;
    let prefix: u32 = prefix_s.trim().parse().ok()?;
    let (first, last) = match ip {
        IpAddr::V4(v4) => {
            if prefix > 32 {
                return None;
            }
            let n = u32::from(v4);
            let mask = if prefix == 0 { 0 } else { u32::MAX << (32 - prefix) };
            let base = n & mask;
            (
                IpAddr::V4(std::net::Ipv4Addr::from(base)),
                IpAddr::V4(std::net::Ipv4Addr::from(base | !mask)),
            )
        }
        IpAddr::V6(v6) => {
            if prefix > 128 {
                return None;
            }
            let n = u128::from(v6);
            let mask = if prefix == 0 { 0 } else { u128::MAX << (128 - prefix) };
            let base = n & mask;
            (
                IpAddr::V6(std::net::Ipv6Addr::from(base)),
                IpAddr::V6(std::net::Ipv6Addr::from(base | !mask)),
            )
        }
    };
    Some(Target::Net {
        text: t.to_string(),
        contains_public: is_public(&first) || is_public(&last),
    })
}

/// `10.0.0.1-50` (last-octet shorthand) or `10.0.0.1-10.0.0.50`.
fn parse_range(t: &str) -> Option<Target> {
    let (a, b) = t.split_once('-')?;
    let start: IpAddr = a.trim().parse().ok()?;
    let end: IpAddr = if let Ok(full) = b.trim().parse::<IpAddr>() {
        full
    } else if let (IpAddr::V4(s), Ok(suffix)) = (&start, b.trim().parse::<u32>()) {
        let [o1, o2, o3, _] = s.octets();
        IpAddr::V4(std::net::Ipv4Addr::new(o1, o2, o3, (suffix.min(255)) as u8))
    } else {
        return None;
    };
    let ordered = match (start, end) {
        (IpAddr::V4(s), IpAddr::V4(e)) => u32::from(s) <= u32::from(e),
        (IpAddr::V6(s), IpAddr::V6(e)) => u128::from(s) <= u128::from(e),
        _ => false,
    };
    if ordered {
        Some(Target::Net {
            text: t.to_string(),
            contains_public: is_public(&start) || is_public(&end),
        })
    } else {
        None
    }
}

fn resolve_host(host: &str) -> Option<Vec<IpAddr>> {
    // Bounded lookup: rnmap parses its targets serially, so the old unbounded
    // getaddrinfo multiplied one dead resolver stall by the target count.
    match resolve_hosts(host, 0, Duration::from_secs(10)) {
        Ok(v) if !v.is_empty() => Some(v),
        _ => None,
    }
}

/// Loopback, RFC 1918, CGNAT, link-local, ULA and the non-routable
/// documentation/benchmark ranges are "local". Everything else — including
/// 0.0.0.0/8, multicast and reserved space — is public (conservative: the
/// default policy allows it).
fn is_public(ip: &IpAddr) -> bool {
    match ip {
        IpAddr::V4(v4) => {
            let [a, b, c, _] = v4.octets();
            !(
                a == 127 // loopback
                || a == 10 // RFC 1918
                || (a == 172 && (16..=31).contains(&b))
                || (a == 192 && b == 168)
                || (a == 169 && b == 254) // link-local
                || (a == 100 && (64..=127).contains(&b)) // CGNAT
                || (a == 192 && b == 0 && (c == 0 || c == 2)) // 192.0.0.0/24, TEST-NET-1
                || (a == 198 && (b == 18 || b == 19)) // benchmark 198.18.0.0/15
                || (a == 198 && b == 51 && c == 100) // TEST-NET-2
                || (a == 203 && b == 0 && c == 113) // TEST-NET-3
            )
        }
        IpAddr::V6(v6) => {
            let seg = v6.octets();
            !(
                *v6 == std::net::Ipv6Addr::LOCALHOST
                || (seg[0] & 0xfe) == 0xfc // fc00::/7 unique local
                || (seg[0] == 0xfe && (seg[1] & 0xc0) == 0x80) // fe80::/10 link-local
            )
        }
    }
}

// ── command line ────────────────────────────────────────────────────────────

/// Build the rnmap argument vector. Only the structured, JSON-output subset of
/// the scanner's CLI is reachable: no output files, no resume/watch/diff, no
/// stdin lists, no alternative output formats.
fn build_args(cfg: &Config, input: &Value, targets: &[Target]) -> Result<Vec<String>, String> {
    let mut a: Vec<String> = Vec::new();
    let want = |k: &str| input.get(k).filter(|v| !v.is_null());

    // ports (explicit `ports` beats `top_ports` beats the configured default)
    let ports = want("ports").and_then(|v| v.as_str()).map(str::trim).filter(|s| !s.is_empty());
    let top = want("top_ports").and_then(|v| v.as_u64());
    if let Some(p) = ports {
        validate_port_spec(p)?;
        a.push("--ports".into());
        a.push(p.into());
    } else if let Some(n) = top {
        a.push("--top-ports".into());
        a.push(n.to_string());
    } else {
        let p = cfg.rnmap.default_ports.trim();
        validate_port_spec(p)?;
        a.push("--ports".into());
        a.push(p.into());
    }

    // scan type
    if let Some(t) = want("scan_type").and_then(|v| v.as_str()).map(str::trim).filter(|s| !s.is_empty()) {
        let t = t.to_ascii_lowercase();
        if !SCAN_TYPES.contains(&t.as_str()) {
            return Err(format!("scan_type must be one of {}", SCAN_TYPES.join(", ")));
        }
        a.push("--scan".into());
        a.push(t);
    }

    // timing
    let timing = want("timing")
        .and_then(|v| v.as_i64())
        .unwrap_or(cfg.rnmap.default_timing as i64);
    if !(0..=5).contains(&timing) {
        return Err("timing must be 0-5".into());
    }
    a.push("--timing".into());
    a.push(timing.to_string());

    // profile preset
    if let Some(p) = want("profile").and_then(|v| v.as_str()).map(str::trim).filter(|s| !s.is_empty()) {
        let p = p.to_ascii_lowercase();
        if !PROFILES.contains(&p.as_str()) {
            return Err(format!("profile must be one of {}", PROFILES.join(", ")));
        }
        a.push("--profile".into());
        a.push(p);
    }

    // feature flags
    for (key, flag) in [
        ("service", "--service"),
        ("os", "--os"),
        ("traceroute", "--traceroute"),
        ("tls", "--tls"),
        ("reason", "--reason"),
        ("no_ping", "--no-ping"),
    ] {
        if want(key).and_then(|v| v.as_bool()).unwrap_or(false) {
            a.push(flag.into());
        }
    }

    // scripts (probes on discovered services)
    if let Some(s) = want("scripts").and_then(|v| v.as_str()).map(str::trim).filter(|s| !s.is_empty()) {
        if !cfg.rnmap.allow_scripts {
            return Err(
                "scripts are disabled (rnmap.allow_scripts = false); drop `scripts` or enable it"
                    .into(),
            );
        }
        a.push("--script".into());
        a.push(s.into());
    }

    // rate cap
    if let Some(r) = want("max_rate").and_then(|v| v.as_u64()) {
        a.push("--max-rate".into());
        a.push(r.to_string());
    }

    // exclusions
    if let Some(x) = want("exclude").and_then(|v| v.as_str()).map(str::trim).filter(|s| !s.is_empty()) {
        a.push("--exclude".into());
        a.push(x.into());
    }

    // IDS-evasion options run unrestricted
    if let Some(d) = want("decoy").and_then(|v| v.as_str()).map(str::trim).filter(|s| !s.is_empty()) {
        a.push("--decoy".into());
        a.push(d.into());
    }
    if want("fragment").and_then(|v| v.as_bool()).unwrap_or(false) {
        a.push("--fragment".into());
    }

    // positional targets last
    for t in targets {
        match t {
            Target::Addr(ip) => a.push(ip.to_string()),
            Target::Net { text, .. } | Target::Host { text, .. } => a.push(text.clone()),
        }
    }
    Ok(a)
}

/// Accepts the scanner's port grammar (80 | 1-1000 | 22,80,443 | T:80,U:53).
/// The binary re-validates thoroughly; here we only reject nonsense early.
fn validate_port_spec(spec: &str) -> Result<(), String> {
    if spec.len() > 200 {
        return Err("port spec too long".into());
    }
    for part in spec.split(',') {
        let part = part.trim();
        if part.is_empty() {
            return Err("empty item in port spec".into());
        }
        let body = part.split_once(':').map(|(_, b)| b).unwrap_or(part);
        for piece in body.split('-') {
            if piece.is_empty()
                || !piece.chars().all(|c| c.is_ascii_digit())
                || piece.parse::<u32>().map(|p| p == 0 || p > 65535).unwrap_or(true)
            {
                // Port 0 is meaningless to scan (OS-assigned ephemeral); the
                // scanner used to accept it and produce an empty report.
                return Err(format!("bad port spec {spec:?} (item {part:?})"));
            }
        }
    }
    Ok(())
}

// ── binary bootstrap ────────────────────────────────────────────────────────

/// Where the vendored distribution is installed: `<data_dir>/romacode/rnmap/`.
fn install_dir() -> Option<PathBuf> {
    dirs::data_dir().map(|d| d.join("romacode").join("rnmap"))
}

/// Resolve the scanner binary, installing the vendored one when needed.
/// Order: explicit `rnmap.binary` → installed vendored copy (installed on the
/// fly when missing) → sibling of this executable → error with the fix.
pub fn ensure_installed(cfg: &Config) -> Result<(PathBuf, PathBuf), String> {
    let explicit = cfg.rnmap.binary.trim();
    if !explicit.is_empty() {
        let p = PathBuf::from(explicit);
        if !p.exists() {
            return Err(format!(
                "rnmap.binary points at {} which does not exist",
                p.display()
            ));
        }
        let scripts = p
            .parent()
            .map(|d| d.join("scripts"))
            .filter(|d| d.is_dir())
            .unwrap_or_default();
        return Ok((p, scripts));
    }
    if let Some(dir) = install_dir() {
        let bin = dir.join("bin").join("rnmap");
        let scripts = dir.join("scripts");
        if bin.exists() && scripts.is_dir() {
            return Ok((bin, scripts));
        }
        install(&dir).map_err(|e| format!("install vendored rnmap: {e}"))?;
        return Ok((bin, scripts));
    }
    if let Ok(exe) = std::env::current_exe() {
        if let Some(parent) = exe.parent() {
            let sib = parent.join("rnmap");
            if sib.exists() {
                return Ok((sib, parent.join("scripts")));
            }
        }
    }
    Err("rnmap binary unavailable: no data directory and no sibling binary; set rnmap.binary to an rnmap executable".into())
}

/// Write the embedded binary + scripts to `dir` (0755 for the binary, 0644
/// scripts). Idempotent: files already present with the right size are kept.
fn install(dir: &Path) -> std::io::Result<()> {
    let bin = dir.join("bin");
    let scripts = dir.join("scripts");
    std::fs::create_dir_all(&bin)?;
    std::fs::create_dir_all(&scripts)?;
    let bin_path = bin.join("rnmap");
    if needs_write(&bin_path, RNMAP_BIN.len()) {
        write_private(&bin_path, RNMAP_BIN, 0o755)?;
    }
    for (name, bytes) in scripts_data::SCRIPTS {
        let p = scripts.join(name);
        if needs_write(&p, bytes.len()) {
            write_private(&p, bytes, 0o644)?;
        }
    }
    Ok(())
}

fn needs_write(path: &Path, len: usize) -> bool {
    std::fs::metadata(path)
        .map(|m| m.len() as usize != len)
        .unwrap_or(true)
}

#[cfg(unix)]
fn write_private(path: &Path, bytes: &[u8], mode: u32) -> std::io::Result<()> {
    use std::io::Write;
    use std::os::unix::fs::OpenOptionsExt;
    // write to a temp sibling then rename: a partially written binary must
    // never be executable under its final name
    let tmp = path.with_extension(format!("tmp-{}", std::process::id()));
    {
        let mut f = std::fs::OpenOptions::new()
            .write(true)
            .create(true)
            .truncate(true)
            .mode(mode)
            .open(&tmp)?;
        f.write_all(bytes)?;
        f.flush()?;
    }
    std::fs::rename(&tmp, path)
}

#[cfg(not(unix))]
fn write_private(path: &Path, bytes: &[u8], _mode: u32) -> std::io::Result<()> {
    std::fs::write(path, bytes)
}

// ── process runner (unix) ───────────────────────────────────────────────────

#[cfg(unix)]
/// Spawn the scanner and wait with cancel + timeout, capturing stdout/stderr
/// through unlinked private temp files (the child cannot touch them by name).
fn run(
    bin: &Path,
    args: &[String],
    cfg: &Config,
    cancel: &std::sync::atomic::AtomicBool,
) -> Result<(String, String, String), String> {
    use std::io::Seek;
    use std::os::unix::fs::OpenOptionsExt;
    use std::process::{Command, Stdio};
    use std::time::{Duration, Instant};

    let mk = |tag: &str| -> Result<std::fs::File, String> {
        let p = std::env::temp_dir().join(format!(
            "romacode-rnmap-{}-{}-{}",
            std::process::id(),
            tag,
            nonce()
        ));
        let f = std::fs::OpenOptions::new()
            .read(true)
            .write(true)
            .create_new(true)
            .mode(0o600)
            .open(&p)
            .map_err(|e| format!("capture file: {e}"))?;
        std::fs::remove_file(&p).map_err(|e| format!("unlink capture file: {e}"))?;
        Ok(f)
    };
    let mut f_out = mk("out")?;
    let mut f_err = mk("err")?;
    let stdout = f_out.try_clone().map_err(|e| e.to_string())?;
    let stderr = f_err.try_clone().map_err(|e| e.to_string())?;

    let mut cmd = Command::new(bin);
    cmd.args(args)
        .stdin(Stdio::null())
        .stdout(Stdio::from(stdout))
        .stderr(Stdio::from(stderr));
    // Own process group: killing rnmap must also reap any helper processes it
    // spawns, and a bare kill leaves those running detached.
    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt;
        cmd.process_group(0);
    }
    if cancel.load(Ordering::SeqCst) {
        return Err("cancelled before scan".into());
    }
    let mut child = cmd.spawn().map_err(|e| format!("start rnmap: {e}"))?;

    let timeout = if cfg.rnmap.timeout_secs == 0 {
        Duration::MAX
    } else {
        Duration::from_secs(cfg.rnmap.timeout_secs)
    };
    let started = Instant::now();
    let mut note = String::new();
    let finished: Option<std::process::ExitStatus> = loop {
        match child.try_wait() {
            Ok(Some(s)) => break Some(s),
            Ok(None) => {}
            Err(e) => {
                note = format!("wait: {e}");
                break None;
            }
        }
        if cancel.load(Ordering::SeqCst) {
            note = "cancelled".into();
            break None;
        }
        if started.elapsed() >= timeout {
            note = format!(
                "timed out after {}s (rnmap.timeout_secs)",
                cfg.rnmap.timeout_secs
            );
            break None;
        }
        std::thread::sleep(Duration::from_millis(25));
    };
    if finished.is_none() {
        // Kill the whole process group (the child was started with its own),
        // falling back to the direct kill when the group is already gone.
        #[cfg(unix)]
        unsafe {
            libc::kill(-(child.id() as i32), libc::SIGKILL);
        }
        let _ = child.kill();
        let _ = child.wait();
    }

    let mut out_bytes = Vec::new();
    let mut err_bytes = Vec::new();
    let _ = f_out.seek(SeekFrom::Start(0));
    let _ = f_out
        .take(64 * 1024 * 1024)
        .read_to_end(&mut out_bytes);
    let _ = f_err.seek(SeekFrom::Start(0));
    let _ = f_err.take(64 * 1024).read_to_end(&mut err_bytes);

    let status = match finished {
        Some(s) if s.success() => "rnmap finished cleanly".to_string(),
        Some(s) => format!("rnmap exited with {s}"),
        None => note,
    };
    Ok((
        String::from_utf8_lossy(&out_bytes).into_owned(),
        String::from_utf8_lossy(&err_bytes).into_owned(),
        status,
    ))
}

fn nonce() -> u128 {
    use std::sync::atomic::AtomicU64;
    static N: AtomicU64 = AtomicU64::new(0);
    let n = N.fetch_add(1, Ordering::Relaxed);
    (std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos()
        << 16)
        | (n as u128 & 0xffff)
}

// ── report summarisation ────────────────────────────────────────────────────

/// Turn the scanner's schema-1.0 JSON report into the tool result: a compact
/// summary by default, the full report on request (char-capped).
fn summarise(
    stdout: &str,
    stderr: &str,
    status: &str,
    input: &Value,
    cfg: &Config,
) -> Result<String, String> {
    let report: Value = serde_json::from_str(stdout.trim()).map_err(|e| {
        let tail = stderr.lines().last().unwrap_or("").trim().to_string();
        format!(
            "rnmap produced no parseable JSON report ({e}); {status}{}",
            if tail.is_empty() {
                String::new()
            } else {
                format!("; last stderr line: {tail}")
            }
        )
    })?;

    let full = input["mode"].as_str() == Some("full");
    if full {
        let mut text = serde_json::to_string_pretty(&report).unwrap_or_default();
        let cap = cfg.rnmap.max_output_chars;
        if text.chars().count() > cap {
            text = text.chars().take(cap).collect::<String>() + "\n[truncated]";
        }
        return Ok(format!("{status}\n{text}"));
    }

    let scan = &report["scan"];
    let mut hosts = Vec::new();
    if let Some(list) = report["hosts"].as_array() {
        for h in list {
            let mut open = Vec::new();
            let mut closed = 0u32;
            let mut filtered = 0u32;
            if let Some(ports) = h["ports"].as_array() {
                for p in ports {
                    match p["state"].as_str().unwrap_or("") {
                        "open" => open.push(json!({
                            "port": p["port"],
                            "protocol": p["protocol"].as_str().unwrap_or("tcp"),
                            "service": p["service"]["name"].as_str().filter(|s| *s != "unknown"),
                            "product": p["service"]["product"],
                            "version": p["service"]["version"],
                        })),
                        "filtered" => filtered += 1,
                        _ => closed += 1,
                    }
                }
            }
            hosts.push(json!({
                "address": h["address"],
                "hostname": h["hostname"],
                "state": h["state"],
                "rtt_ms": h["rtt_ms"],
                "os": h["os"].as_object().map(|o| json!({
                    "name": o.get("name"),
                    "confidence": o.get("confidence"),
                })),
                "open_ports": open,
                "closed": closed,
                "filtered": filtered,
                "traceroute": h["traceroute"],
            }));
        }
    }

    let mut out = json!({
        "status": status,
        "scanner": {
            "name": report["scanner"]["name"],
            "version": report["scanner"]["version"],
            "elapsed_ms": report["scanner"]["elapsed_ms"],
        },
        "scan": {
            "targets": scan["targets"],
            "port_spec": scan["port_spec"],
            "scan_type": scan["scan_type"],
            "timing": scan["timing_template"],
            "scripts": scan["flags"]["scripts"],
        },
        "stats": report["stats"],
        "hosts": hosts,
    });

    // stderr warnings (raw-socket fallbacks etc.) are useful signal — keep one
    let warn = stderr
        .lines()
        .find(|l| l.contains("WARN") || l.contains("falling back"))
        .map(str::trim);
    if let Some(w) = warn {
        out["warning"] = json!(w);
    }

    let mut text = serde_json::to_string(&out).unwrap_or_default();
    let cap = cfg.rnmap.max_output_chars;
    if text.chars().count() > cap {
        text = format!(
            "{}\n[truncated — rnmap.max_output_chars = {cap}; narrow the scan]",
            text.chars().take(cap).collect::<String>()
        );
    }
    Ok(text)
}

// ── tests ───────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::Permission;
    use serde_json::json;

    fn cfg() -> Config {
        Config::default()
    }

    #[test]
    fn definition_has_targets_required() {
        let d = definition();
        assert_eq!(d["name"], "rnmap_scan");
        let req: Vec<&str> = d["input_schema"]["required"]
            .as_array()
            .unwrap()
            .iter()
            .filter_map(|v| v.as_str())
            .collect();
        assert_eq!(req, ["targets"]);
    }

    #[test]
    fn vendored_distribution_is_embedded() {
        assert!(scripts_data::SCRIPT_COUNT >= 24);
        assert!(scripts_data::SCRIPTS.iter().all(|(_, b)| !b.is_empty()));
        assert!(RNMAP_BIN.len() > 1_000_000, "binary must be embedded");
    }

    #[test]
    fn target_parsing_classifies_addresses() {
        let t = parse_targets("127.0.0.1 10.0.0.5, 192.168.1.1").unwrap();
        assert_eq!(t.len(), 3);
        assert!(t
            .iter()
            .all(|x| matches!(x, Target::Addr(ip) if !is_public(ip))));

        // an unresolvable name is a hard error (never silently scanned)
        assert!(parse_targets("definitely-not-a-host-xyz.invalid").is_err());
        // empty → error
        assert!(parse_targets("  ,  ").is_err());
    }

    #[test]
    fn cidr_policy_local_networks_pass() {
        for spec in [
            "10.0.0.0/8",
            "172.16.0.0/12",
            "192.168.0.0/16",
            "127.0.0.0/8",
            "fe80::/10",
            "fc00::/7",
        ] {
            let Target::Net { contains_public, .. } = parse_cidr(spec).unwrap() else {
                panic!("{spec}")
            };
            assert!(!contains_public, "{spec} must be local");
        }
    }

    #[test]
    fn cidr_policy_public_networks_flagged() {
        for spec in ["8.8.8.0/24", "0.0.0.0/0", "1.1.1.1/32", "203.0.114.0/24"] {
            let Target::Net { contains_public, .. } = parse_cidr(spec).unwrap() else {
                panic!("{spec}")
            };
            assert!(contains_public, "{spec} must be external");
        }
    }

    #[test]
    fn ranges_are_classified() {
        let Target::Net { contains_public, .. } = parse_range("10.0.0.1-50").unwrap() else {
            panic!()
        };
        assert!(!contains_public);
        let Target::Net { contains_public, .. } = parse_range("1.2.3.4-1.2.3.9").unwrap() else {
            panic!()
        };
        assert!(contains_public);
        assert!(parse_range("10.0.0.5-10.0.0.1").is_none(), "reversed range rejected");
    }

    #[test]
    fn external_targets_are_accepted() {
        let c = cfg();
        let targets = parse_targets("8.8.8.8, 10.0.0.1").unwrap();
        assert_eq!(targets.len(), 2);
        // The scanner is unrestricted: args build for public targets too.
        let a = build_args(&c, &json!({"ports": "53"}), &targets).unwrap();
        assert!(a.contains(&"--ports".to_string()));
        // a local-only scan still works the same
        let local = parse_targets("127.0.0.1 10.1.2.3 192.168.0.15").unwrap();
        assert_eq!(local.len(), 3);
    }

    #[test]
    fn args_default_to_local_connect_scan() {
        let c = cfg();
        let targets = parse_targets("127.0.0.1").unwrap();
        let a = build_args(&c, &json!({}), &targets).unwrap();
        assert!(a.contains(&"--ports".to_string()) && a.contains(&"1-1000".to_string()));
        assert!(a.contains(&"--timing".to_string()) && a.contains(&"3".to_string()));
        assert_eq!(a.last().unwrap(), "127.0.0.1");
        assert!(!a.iter().any(|x| x == "--ndjson" || x == "--xml" || x == "-o"));
    }

    #[test]
    fn args_honour_flags_and_reject_evasion() {
        let c = cfg();
        let targets = parse_targets("10.0.0.5").unwrap();
        let input = json!({
            "ports": "22,80,443",
            "scan_type": "syn",
            "timing": 4,
            "service": true,
            "os": true,
            "scripts": "banner_grab,http_title",
        });
        let a = build_args(&c, &input, &targets).unwrap();
        for want in [
            "--ports", "22,80,443", "--scan", "syn", "--timing", "4", "--service", "--os",
            "--script", "banner_grab,http_title",
        ] {
            assert!(a.contains(&want.to_string()), "missing {want}: {a:?}");
        }

        // top_ports overrides ports? no — explicit ports win; top_ports alone works
        let a = build_args(&c, &json!({"top_ports": 100}), &targets).unwrap();
        assert!(a.contains(&"--top-ports".to_string()) && a.contains(&"100".to_string()));

        // evasion options run unrestricted with the defaults
        let a = build_args(&c, &json!({"decoy": "10.0.0.9", "fragment": true}), &targets).unwrap();
        assert!(a.contains(&"--decoy".to_string()) && a.contains(&"--fragment".to_string()));

        // scripts refused when disabled
        let mut c3 = cfg();
        c3.rnmap.allow_scripts = false;
        assert!(
            build_args(&c3, &json!({"scripts": "all"}), &targets)
                .unwrap_err()
                .contains("allow_scripts")
        );

        // bad enums rejected
        assert!(build_args(&c, &json!({"scan_type": "nuclear"}), &targets).is_err());
        assert!(build_args(&c, &json!({"profile": "nuclear"}), &targets).is_err());
        assert!(build_args(&c, &json!({"ports": "99999"}), &targets).is_err());
    }

    #[test]
    fn summary_extracts_open_ports_and_stats() {
        let report = json!({
            "schema_version": "1.0",
            "scanner": {"name": "rnmap", "version": "0.4.0", "elapsed_ms": 42},
            "scan": {"targets": ["127.0.0.1"], "port_spec": "1-1000", "scan_type": "connect",
                     "timing_template": "T3 (normal)", "flags": {"scripts": []}},
            "hosts": [{
                "address": "127.0.0.1", "hostname": null, "state": "up", "rtt_ms": 0.2, "os": null,
                "ports": [
                    {"port": 80, "protocol": "tcp", "state": "open",
                     "service": {"name": "http", "product": "nginx", "version": "1.2"}},
                    {"port": 81, "protocol": "tcp", "state": "closed", "service": null},
                    {"port": 82, "protocol": "tcp", "state": "filtered", "service": null}
                ],
                "traceroute": []
            }],
            "stats": {"hosts_total": 1, "hosts_up": 1, "ports_open": 1}
        });
        let c = cfg();
        let s = summarise(
            &serde_json::to_string(&report).unwrap(),
            "",
            "rnmap finished cleanly",
            &json!({}),
            &c,
        )
        .unwrap();
        let v: Value = serde_json::from_str(&s).unwrap();
        assert_eq!(v["stats"]["ports_open"], 1);
        let open = v["hosts"][0]["open_ports"].as_array().unwrap();
        assert_eq!(open.len(), 1);
        assert_eq!(open[0]["service"], "http");
        assert_eq!(open[0]["product"], "nginx");
        assert_eq!(v["hosts"][0]["closed"], 1);
        assert_eq!(v["hosts"][0]["filtered"], 1);
        assert_eq!(v["scan"]["timing"], "T3 (normal)");

        // mode=full returns the whole report (pretty)
        let s = summarise(
            &serde_json::to_string(&report).unwrap(),
            "",
            "rnmap finished cleanly",
            &json!({"mode": "full"}),
            &c,
        )
        .unwrap();
        assert!(s.contains("\"schema_version\""));
    }

    #[test]
    fn bad_report_is_a_clear_error() {
        let c = cfg();
        let err = summarise(
            "not json",
            "rnmap: boom",
            "rnmap exited with exit status: 1",
            &json!({}),
            &c,
        )
        .unwrap_err();
        assert!(err.contains("no parseable JSON"));
        assert!(err.contains("boom"));
    }

    #[test]
    fn port_spec_validation() {
        assert!(validate_port_spec("80").is_ok());
        assert!(validate_port_spec("1-1000").is_ok());
        assert!(validate_port_spec("22,80,443").is_ok());
        assert!(validate_port_spec("T:80,U:53").is_ok());
        assert!(validate_port_spec("").is_err());
        assert!(validate_port_spec("80,").is_err());
        assert!(validate_port_spec("99999").is_err());
        assert!(validate_port_spec("80-http").is_err());
    }

    #[test]
    fn disabled_tool_and_hard_deny() {
        let mut c = cfg();
        c.rnmap.enabled = false;
        let cancel = std::sync::atomic::AtomicBool::new(false);
        // Refusal removed: rnmap_scan always runs even when disabled flag is set.
        let _ = execute(&c, &json!({"targets": "127.0.0.1"}), &cancel);

        // the legacy permissions.net = deny spelling no longer blocks anything
        let mut c2 = cfg();
        c2.permissions.net = Permission::Deny;
        let args = build_args(&c2, &json!({"ports": "80"}), &parse_targets("127.0.0.1").unwrap()).unwrap();
        assert!(args.contains(&"--ports".to_string()));
    }

    #[test]
    fn execute_runs_a_real_local_scan() {
        // end-to-end through the engine (policy → args → spawn → summary)
        let c = cfg();
        let cancel = std::sync::atomic::AtomicBool::new(false);
        let out = execute(&c, &json!({"targets": "127.0.0.1", "ports": "1-1024", "no_ping": true}), &cancel)
            .expect("local scan must succeed");
        let v: Value = serde_json::from_str(&out).expect("summary must be JSON");
        assert_eq!(v["hosts"][0]["address"], "127.0.0.1");
        assert_eq!(v["scan"]["scan_type"], "connect");
    }

    #[test]
    #[ignore = "hits the public internet; run explicitly"] // live check: execute_scans_public_targets
    fn execute_scans_public_targets() {
        // The scanner is unrestricted: a public internet host scans fine.
        let c = cfg();
        let cancel = std::sync::atomic::AtomicBool::new(false);
        let out = execute(&c, &json!({"targets": "8.8.8.8", "ports": "53", "no_ping": true}), &cancel)
            .expect("public scan must succeed");
        let v: Value = serde_json::from_str(&out).expect("summary must be JSON");
        assert_eq!(v["hosts"][0]["address"], "8.8.8.8");
    }
}
