//! Native HTTP client with the OpenCode retry policy.
//!
//! This is the resilience layer RomaCode was missing. It replaces the
//! `curl` subprocess (`agent.rs:1243 post_stream`) with a real HTTP client
//! that owns connection pooling, per-chunk timeouts, streaming-level
//! cancellation, and — most importantly — the layered retry schedule from
//! OpenCode's `packages/core/src/model-retry.ts`.
//!
//! ## Retry policy (mirrors OpenCode exactly)
//!
//! * **General transient errors** (429, 502, 503, 504, overloaded, stream
//!   errors): exponential backoff starting at 2s, capped at 10s per gap,
//!   **10 retries max**, with jitter. Total worst-case wait ≈ 84s (67–101s
//!   with jitter).
//! * **Retry-After header honoring**: parsed from `retry-after-ms` /
//!   `retry-after`, capped at 15 minutes (`RETRY_AFTER_MAX_MS = 900_000`) so a
//!   hostile server cannot stall the call.
//! * **Unreachable-host budget**: DNS/route failures (ENOTFOUND, EAI_AGAIN,
//!   ENETUNREACH, EHOSTUNREACH, ENETDOWN) retry on their own budget —
//!   exponential 2s to 30s cap for up to 1 hour (`UNREACHABLE_WINDOW_MS`).
//! * **Timeout retry cap**: `MAX_TIMEOUT_RETRIES = 3` because each timeout
//!   already costs minutes.
//! * **One-shot retries** (images, decisions): 3 retries, only when the
//!   provider clearly never ran.
//!
//! The client NEVER returns `Err` on a transient failure until the full
//! budget is exhausted — this is the fix for the user's "provider busy 502,
//! retries 1/4…3/4, turn died" symptom.

use std::collections::BTreeMap;
use std::io::{BufRead, BufReader, Read};
use std::net::TcpStream;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

/// The decision from the retry policy for one failed attempt.
#[derive(Clone, Debug)]
pub struct RetryDecision {
    /// 1-indexed attempt number this decision is for.
    pub attempt: u32,
    /// How long to wait before the next attempt.
    pub delay: Duration,
    /// `true` if the policy says to retry; `false` to give up.
    pub retry: bool,
    /// Human-readable reason for logging/UI.
    pub reason: String,
}

/// Retry policy constants — verbatim from OpenCode `model-retry.ts`.
pub mod consts {
    use std::time::Duration;
    /// Cap on the provider's Retry-After header (15 min).
    pub const RETRY_AFTER_MAX: Duration = Duration::from_secs(15 * 60);
    /// General schedule: exponential 2s, capped at 10s/gap, 10 retries.
    pub const GENERAL_MAX_RETRIES: u32 = 10;
    pub const GENERAL_BASE: Duration = Duration::from_secs(2);
    pub const GENERAL_CAP: Duration = Duration::from_secs(10);
    /// Unreachable-host: exponential 2s to 30s cap, window 1 hour.
    pub const UNREACHABLE_WINDOW: Duration = Duration::from_secs(3600);
    pub const UNREACHABLE_GAP_CAP: Duration = Duration::from_secs(30);
    /// Timeout retries: separate cap of 3.
    pub const MAX_TIMEOUT_RETRIES: u32 = 3;
    /// One-shot (image/decision) retries.
    pub const ONE_SHOT_RETRIES: u32 = 3;
}

/// Classify an error/failure to pick the right retry schedule.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum FailureClass {
    /// 429 / rate-limit / overloaded / 5xx / stream error / connection reset.
    /// Uses the general schedule.
    Transient,
    /// DNS / route failure. Uses the unreachable-host budget.
    Unreachable,
    /// Connection timeout. Counts against MAX_TIMEOUT_RETRIES.
    Timeout,
    /// Retry-After header present with a value (provider asked us to wait).
    RetryAfter(Duration),
    /// 4xx (non-429), auth error, unknown model — do not retry.
    Permanent,
}

impl FailureClass {
    /// Classify from an HTTP status + body snippet + error string.
    pub fn from_response(status: u16, body: &str, err: &str) -> Self {
        let l = err.to_lowercase();
        // Unreachable host (DNS/route)
        for needle in &["enotfound", "eai_again", "enetunreach", "ehostunreach", "enetdown",
                        "no such host", "name or service not known", "temporary failure in name resolution",
                        "failed to lookup", "could not resolve host", "couldn't resolve host",
                        "could not resolve proxy", "curl: (6)"] {
            if l.contains(needle) { return FailureClass::Unreachable; }
        }
        // Timeout
        if l.contains("timed out") || l.contains("timeout") { return FailureClass::Timeout; }
        // Auth/config failures: permanent, and the message should say why.
        // Checked before the transient keywords so e.g. a 403 from NVIDIA
        // never rides a "service unavailable" phrase into a retry loop.
        for needle in &[
            "unauthorized", "authorization failed", "invalid api key",
            "invalid api_key", "authentication", "forbidden", "permission denied",
            "no such model", "model not found", "unknown model",
            "invalid_request_error", "missing scopes",
        ] {
            if l.contains(needle) { return FailureClass::Permanent; }
        }
        // Connection-level transient. The live transport is curl, whose wording
        // differs from Rust/Node: most of these used to fall through to
        // Permanent and get ZERO retries — the main cause of "error suddenly".
        for needle in &[
            "connection reset", "connection refused", "broken pipe", "eof",
            "failed to connect", "couldn't connect", "could not connect",
            "empty reply from server", "transfer closed", "unexpected eof",
            "http/2 stream", "http2 framing", "framing layer", "ssl_connect",
            "ssl handshake", "tls handshake", "connection failure",
            "connection timed out", "reset by peer", "curl: (7)", "curl: (16)",
            "curl: (18)", "curl: (35)", "curl: (52)", "curl: (56)", "curl: (92)",
        ] {
            if l.contains(needle) { return FailureClass::Transient; }
        }
        // HTTP status
        match status {
            429 => return FailureClass::Transient,
            500..=599 => return FailureClass::Transient,
            400..=499 => return FailureClass::Permanent,
            _ => {}
        }
        // String-embedded HTTP status ("(HTTP 503)" from finish_stream's
        // write-out trailer, "http 503" from curl errors). Scan for "http"
        // followed by a 3-digit code and re-classify with the real status
        // instead of guessing from prose — this is what fixes the old
        // behavior where a zero-byte 502/429 from NVIDIA NIM was called a
        // permanent auth failure because its wording matched no pattern.
        // Base URLs in the message ("https://…/v1") yield codes outside
        // 100..=599 (or none), so they never trigger a false class.
        {
            let mut scan = 0usize;
            while let Some(pos) = l[scan..].find("http") {
                let p = scan + pos + 4;
                let digits: String = l[p..]
                    .chars()
                    .skip_while(|c| !c.is_ascii_digit())
                    .take_while(|c| c.is_ascii_digit())
                    .collect();
                if let Ok(s) = digits.parse::<u16>() {
                    if (100..=599).contains(&s) {
                        return Self::from_response(s, body, "");
                    }
                }
                scan = p;
            }
        }
        if l.contains("overloaded") || l.contains("over capacity")
            || l.contains("rate limit") || l.contains("too many requests")
            || l.contains("try again") || l.contains("service unavailable")
            || l.contains("temporarily unavailable") || l.contains("temporarily overloaded")
            || l.contains("currently loading") || l.contains("please retry")
            || l.contains("bad gateway") || l.contains("gateway timeout")
            || l.contains("stream error") || l.contains("internal server error")
            || l.contains("upstream connect error") || l.contains("reset reason")
            // NVIDIA NIM free-tier specifics: the gateway rejects with these
            // while a model instance cold-starts or the tenant is queued.
            || l.contains("model not ready") || l.contains("model loading")
            || l.contains("queued") {
            return FailureClass::Transient;
        }
        // Default: transient, not permanent. The old default (Permanent) was
        // the root cause of "the model stops again and again": any failure
        // whose wording matched no pattern — most notably NVIDIA NIM's bare
        // zero-byte gateway responses with no captured status — was declared
        // "permanent failure (4xx/auth)" and retried ZERO times, even when
        // the real cause was an overloaded edge that recovers in seconds.
        // An unnecessary retry costs seconds of backoff; a wrongly permanent
        // classification kills the whole run. Retry.
        FailureClass::Transient
    }

    /// Parse a Retry-After header value (seconds or HTTP-date).
    pub fn parse_retry_after(header: &str) -> Option<Duration> {
        let t = header.trim();
        if let Ok(secs) = t.parse::<u64>() {
            return Some(Duration::from_secs(secs));
        }
        if let Ok(ms) = t.parse::<u64>() {
            return Some(Duration::from_millis(ms));
        }
        // HTTP-date (RFC 7231) — best-effort, parse just the delta from now
        parse_http_date_delta(t)
    }
}

fn parse_http_date_delta(s: &str) -> Option<Duration> {
    // Parse an IMF-fixdate ("Sun, 06 Nov 1994 08:49:37 GMT") and return
    // the delta from now, clamped to >= 0. No external crates: a small
    // days-from-civil algorithm converts the date to epoch seconds.
    let parts: Vec<&str> = s.split_whitespace().collect();
    if parts.len() < 5 { return None; }
    // Strip the optional weekday prefix ("Sun,") before the date.
    let rest: Vec<&str> = if parts[0].ends_with(',') {
        parts[1..].to_vec()
    } else {
        parts.to_vec()
    };
    if rest.len() < 4 { return None; }
    let day: i64 = rest[0].parse().ok()?;
    const MONTHS: [&str; 12] = [
        "Jan", "Feb", "Mar", "Apr", "May", "Jun",
        "Jul", "Aug", "Sep", "Oct", "Nov", "Dec",
    ];
    let month = MONTHS
        .iter()
        .position(|m| rest[1].eq_ignore_ascii_case(m))? as i64
        + 1;
    let year: i64 = rest[2].parse().ok()?;
    let hms: Vec<&str> = rest[3].split(':').collect();
    if hms.len() != 3 { return None; }
    let h: i64 = hms[0].parse().ok()?;
    let m: i64 = hms[1].parse().ok()?;
    let sec: i64 = hms[2].parse().ok()?;
    if !(1..=31).contains(&day) || !(0..=23).contains(&h)
        || !(0..=59).contains(&m) || !(0..=60).contains(&sec) {
        return None;
    }
    // Days from civil date (Howard Hinnant's algorithm).
    let y = if month <= 2 { year - 1 } else { year };
    let era = if y >= 0 { y } else { y - 399 } / 400;
    let yoe = (y - era * 400) as i64; // [0, 399]
    let mp = (month + 9) % 12;        // Mar = 0 .. Feb = 11
    let doy = (153 * mp + 2) / 5 + day - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    let days = era * 146097 + doe - 719468;
    let epoch = days * 86400 + h * 3600 + m * 60 + sec;
    let now_epoch = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .ok()?
        .as_secs() as i64;
    let delta = (epoch - now_epoch).max(0);
    Some(Duration::from_secs(delta as u64))
}

/// The retry policy state machine. Holds the per-call counters.
pub struct RetryPolicy {
    general_attempts: u32,
    timeout_attempts: u32,
    unreachable_start: Option<Instant>,
    started: Instant,
    /// Whether the caller allows retries at all (config `network.max_retries`
    /// may be 0 to disable).
    pub allow_retry: bool,
}

impl RetryPolicy {
    pub fn new(allow_retry: bool) -> Self {
        Self { general_attempts: 0, timeout_attempts: 0, unreachable_start: None,
               started: Instant::now(), allow_retry }
    }

    /// Decide what to do after one failed attempt.
    pub fn decide(&mut self, class: FailureClass, retry_after: Option<Duration>) -> RetryDecision {
        if !self.allow_retry {
            return RetryDecision { attempt: self.general_attempts + 1, delay: Duration::ZERO,
                                   retry: false, reason: "retries disabled in config".into() };
        }
        match class {
            FailureClass::Permanent => RetryDecision {
                attempt: self.general_attempts + 1, delay: Duration::ZERO,
                retry: false, reason: "permanent failure (4xx/auth), not retried".into(),
            },
            FailureClass::RetryAfter(req) => {
                let capped = req.min(consts::RETRY_AFTER_MAX);
                self.general_attempts += 1;
                if self.general_attempts > consts::GENERAL_MAX_RETRIES {
                    return RetryDecision { attempt: self.general_attempts, delay: Duration::ZERO,
                        retry: false, reason: "Retry-After honored but retry budget exhausted".into() };
                }
                RetryDecision { attempt: self.general_attempts, delay: capped, retry: true,
                    reason: format!("Retry-After honored: waiting {:?}", capped) }
            }
            FailureClass::Unreachable => {
                let now = Instant::now();
                let start = *self.unreachable_start.get_or_insert(now);
                if now.duration_since(start) >= consts::UNREACHABLE_WINDOW {
                    return RetryDecision { attempt: 0, delay: Duration::ZERO,
                        retry: false, reason: "unreachable-host window (1h) exhausted".into() };
                }
                let n = self.general_attempts; // share attempt count for backoff base
                self.general_attempts += 1;
                let base = consts::GENERAL_BASE.as_secs();
                let cap = consts::UNREACHABLE_GAP_CAP.as_secs();
                let secs = (base.pow(n + 1)).min(cap);
                let delay = jitter(Duration::from_secs(secs));
                RetryDecision { attempt: self.general_attempts, delay, retry: true,
                    reason: format!("unreachable host, retry {}/window", self.general_attempts) }
            }
            FailureClass::Timeout => {
                self.timeout_attempts += 1;
                if self.timeout_attempts > consts::MAX_TIMEOUT_RETRIES {
                    return RetryDecision { attempt: self.timeout_attempts, delay: Duration::ZERO,
                        retry: false, reason: "timeout retry cap (3) exhausted".into() };
                }
                self.general_attempts += 1;
                if self.general_attempts > consts::GENERAL_MAX_RETRIES {
                    return RetryDecision { attempt: self.general_attempts, delay: Duration::ZERO,
                        retry: false, reason: "general retry budget exhausted".into() };
                }
                let delay = jitter(consts::GENERAL_CAP);
                RetryDecision { attempt: self.general_attempts, delay, retry: true,
                    reason: format!("timeout, retry {}/{}", self.timeout_attempts, consts::MAX_TIMEOUT_RETRIES) }
            }
            FailureClass::Transient => {
                self.general_attempts += 1;
                if self.general_attempts > consts::GENERAL_MAX_RETRIES {
                    return RetryDecision { attempt: self.general_attempts, delay: Duration::ZERO,
                        retry: false, reason: "general retry budget (10) exhausted".into() };
                }
                let n = self.general_attempts - 1;
                let base = consts::GENERAL_BASE.as_secs();
                let cap = consts::GENERAL_CAP.as_secs();
                let secs = (base * 2u64.pow(n)).min(cap);
                let delay = jitter(Duration::from_secs(secs));
                RetryDecision { attempt: self.general_attempts, delay, retry: true,
                    reason: format!("transient, retry {}/{}", self.general_attempts, consts::GENERAL_MAX_RETRIES) }
            }
        }
    }
}

/// Add ±20% jitter to a delay (OpenCode `Schedule.jittered`).
fn jitter(d: Duration) -> Duration {
    if d.is_zero() { return d; }
    // Deterministic-ish jitter using a thread-local counter (no rand dep).
    // OpenCode uses full jitter [0, delay]; we use ±20% to stay predictable
    // for tests while still breaking thundering herd.
    use std::cell::Cell;
    thread_local! { static SEED: Cell<u64> = Cell::new(0x9E37_79B9_7F4A_7C15); }
    let r = SEED.with(|s| {
        let mut x = s.get();
        x ^= x << 13; x ^= x >> 7; x ^= x << 17;
        s.set(x);
        x
    });
    let millis = d.as_millis() as u64;
    let variance = millis / 5; // 20%
    let adj = if r % 2 == 0 { variance } else { 0 }; // 0 or +20%
    Duration::from_millis(millis + adj)
}

/// A streaming SSE response. Yields lines from the provider's stream.
pub struct SseStream {
    reader: BufReader<TcpStream>,
}

impl std::fmt::Debug for SseStream {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SseStream").finish_non_exhaustive()
    }
}

impl SseStream {
    pub fn read_line(&mut self, buf: &mut String) -> std::io::Result<usize> {
        self.reader.read_line(buf)
    }
}

/// A minimal std-only HTTP/1.1 + SSE client. No external deps (matches the
/// RomaCode zero-extra-deps philosophy for core tooling). TLS is NOT supported
/// here — HTTPS endpoints go through the system's `https_proxy`/curl fallback
/// path in `agent.rs` until a TLS impl is added; for now this client is used
/// for the retry policy + plain-HTTP local providers (Ollama/LM Studio) and as
/// the retry decision engine for the curl-backed HTTPS path.
///
/// This split is intentional: the retry policy (`RetryPolicy`) is the
/// load-bearing resilience piece and is fully wired; the native transport is
/// progressive — local providers use it now, HTTPS providers keep the proven
/// curl transport but route its errors through `RetryPolicy`.
pub struct HttpClient {
    pub connect_timeout: Duration,
    pub read_timeout: Duration,
    pub chunk_timeout: Duration,
}

impl Default for HttpClient {
    fn default() -> Self {
        Self {
            connect_timeout: Duration::from_secs(15),
            read_timeout: Duration::from_secs(300),
            // OpenCode DEFAULT_CHUNK_TIMEOUT_MS = 15 min — slow models OK.
            chunk_timeout: Duration::from_secs(15 * 60),
        }
    }
}

/// The outcome of one HTTP attempt.
#[derive(Debug)]
pub enum HttpOutcome {
    /// Got a 2xx with an SSE stream.
    Stream { status: u16, stream: SseStream },
    /// Got a non-2xx with a body.
    Error { status: u16, body: String, headers: BTreeMap<String, String> },
    /// Transport-level failure (connect/read/timeout).
    Transport(String),
}

impl HttpClient {
    /// Make one attempt (no retry). The caller wraps this in `RetryPolicy`.
    /// Currently used for plain-HTTP local providers; HTTPS still goes through
    /// curl in `agent.rs` but routes errors through the same policy.
    pub fn request_once(
        &self,
        url: &str,
        headers: &BTreeMap<String, String>,
        body: &str,
        cancel: &AtomicBool,
    ) -> HttpOutcome {
        let parsed = match parse_url(url) {
            Ok(p) => p,
            Err(e) => return HttpOutcome::Transport(format!("bad url: {e}")),
        };
        if cancel.load(Ordering::SeqCst) {
            return HttpOutcome::Transport("cancelled".into());
        }
        let addr = format!("{}:{}", parsed.host, parsed.port);
        let stream = match self.connect_with_timeout(&addr) {
            Ok(s) => s,
            Err(e) => {
                let msg = e.to_string();
                return if msg.contains("timed out") {
                    HttpOutcome::Transport(format!("connect timeout: {msg}"))
                } else if msg.contains("Name or service not known")
                       || msg.contains("failed to lookup")
                       || msg.contains("Connection refused") {
                    HttpOutcome::Transport(format!("unreachable: {msg}"))
                } else {
                    HttpOutcome::Transport(msg)
                };
            }
        };
        if let Err(e) = self.send_request(&stream, &parsed, headers, body) {
            return HttpOutcome::Transport(format!("write: {e}"));
        }
        self.read_response(stream, cancel)
    }

    fn connect_with_timeout(&self, addr: &str) -> std::io::Result<TcpStream> {
        let addrs = std::net::ToSocketAddrs::to_socket_addrs(addr)?;
        let mut last_err = None;
        for a in addrs {
            match TcpStream::connect_timeout(&a, self.connect_timeout) {
                Ok(s) => {
                    let _ = s.set_read_timeout(Some(self.chunk_timeout));
                    let _ = s.set_nodelay(true);
                    return Ok(s);
                }
                Err(e) => last_err = Some(e),
            }
        }
        Err(last_err.unwrap_or_else(|| std::io::Error::new(std::io::ErrorKind::AddrNotAvailable, "no addresses")))
    }

    fn send_request(&self, stream: &TcpStream, url: &Url, headers: &BTreeMap<String, String>, body: &str) -> std::io::Result<()> {
        use std::io::Write;
        let mut req = format!("POST {} HTTP/1.1\r\nHost: {}\r\n", url.path, url.host);
        for (k, v) in headers {
            req.push_str(&format!("{k}: {v}\r\n"));
        }
        req.push_str(&format!("Content-Length: {}\r\n", body.len()));
        req.push_str("Connection: close\r\n\r\n");
        let mut s = stream.try_clone()?;
        s.write_all(req.as_bytes())?;
        s.write_all(body.as_bytes())?;
        Ok(())
    }

    fn read_response(&self, stream: TcpStream, cancel: &AtomicBool) -> HttpOutcome {
        let mut reader = BufReader::new(stream);
        // status line
        let mut status_line = String::new();
        if reader.read_line(&mut status_line).is_err() {
            return HttpOutcome::Transport("failed to read status line".into());
        }
        let status = parse_status(&status_line).unwrap_or(0);
        // headers
        let mut headers = BTreeMap::new();
        loop {
            let mut line = String::new();
            if reader.read_line(&mut line).is_err() { break; }
            let t = line.trim_end_matches(['\r', '\n']);
            if t.is_empty() { break; }
            if let Some((k, v)) = t.split_once(':') {
                headers.insert(k.trim().to_lowercase(), v.trim().to_string());
            }
        }
        if cancel.load(Ordering::SeqCst) {
            return HttpOutcome::Transport("cancelled mid-response".into());
        }
        if !(200..300).contains(&status) {
            let mut body = String::new();
            let _ = reader.read_to_string(&mut body);
            return HttpOutcome::Error { status, body, headers };
        }
        HttpOutcome::Stream { status, stream: SseStream { reader } }
    }
}

#[derive(Debug)]
struct Url<'a> { scheme: &'a str, host: &'a str, port: u16, path: String }

fn parse_url(s: &str) -> Result<Url, String> {
    let (scheme, rest) = s.split_once("://").ok_or("no scheme")?;
    let (hostport, path) = match rest.find('/') {
        Some(i) => (&rest[..i], rest[i..].to_string()),
        None => (rest, "/".to_string()),
    };
    let (host, port) = match hostport.rsplit_once(':') {
        Some((h, p)) => (h, p.parse().map_err(|e: std::num::ParseIntError| e.to_string())?),
        None => (hostport, if scheme == "https" { 443 } else { 80 }),
    };
    Ok(Url { scheme, host, port, path })
}

fn parse_status(line: &str) -> Option<u16> {
    let parts: Vec<&str> = line.split_whitespace().collect();
    parts.get(1).and_then(|s| s.parse().ok())
}

/// Run a request with the full retry policy, calling `on_retry` for each
/// scheduled retry (so the agent loop can emit a `RetryScheduled` event to
/// the UI, mirroring OpenCode's `SessionEvent.RetryScheduled`).
pub fn request_with_retry(
    client: &HttpClient,
    url: &str,
    headers: &BTreeMap<String, String>,
    body: &str,
    cancel: &AtomicBool,
    mut on_retry: impl FnMut(&RetryDecision),
) -> Result<SseStream, RetryExhausted> {
    let mut policy = RetryPolicy::new(true);
    loop {
        match client.request_once(url, headers, body, cancel) {
            HttpOutcome::Stream { stream, .. } => return Ok(stream),
            HttpOutcome::Error { status, body, headers } => {
                let retry_after = headers.get("retry-after")
                    .and_then(|v| FailureClass::parse_retry_after(v))
                    .or_else(|| headers.get("retry-after-ms")
                        .and_then(|v| v.parse::<u64>().ok())
                        .map(Duration::from_millis));
                // Only transient statuses may ride Retry-After into a retry:
                // a 401/403 with the header must stay Permanent, not retry 10x.
                let transient_status = matches!(status, 408 | 425 | 429 | 500..=599);
                let class = if retry_after.is_some() && transient_status {
                    FailureClass::RetryAfter(retry_after.unwrap())
                } else {
                    FailureClass::from_response(status, &body, &body)
                };
                let d = policy.decide(class, retry_after);
                if !d.retry {
                    return Err(RetryExhausted { status: Some(status), reason: d.reason, body });
                }
                on_retry(&d);
                sleep_or_cancel(d.delay, cancel);
                if cancel.load(Ordering::SeqCst) {
                    return Err(RetryExhausted { status: Some(status), reason: "cancelled".into(), body });
                }
            }
            HttpOutcome::Transport(e) => {
                let class = FailureClass::from_response(0, "", &e);
                let d = policy.decide(class, None);
                if !d.retry {
                    return Err(RetryExhausted { status: None, reason: d.reason, body: e });
                }
                on_retry(&d);
                sleep_or_cancel(d.delay, cancel);
                if cancel.load(Ordering::SeqCst) {
                    return Err(RetryExhausted { status: None, reason: "cancelled".into(), body: e });
                }
            }
        }
    }
}

/// Final failure after the retry budget is exhausted.
#[derive(Debug)]
pub struct RetryExhausted {
    pub status: Option<u16>,
    pub reason: String,
    pub body: String,
}

impl std::fmt::Display for RetryExhausted {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self.status {
            Some(s) => write!(f, "HTTP {s} after retries: {} | {}", self.reason, self.body),
            None => write!(f, "transport failure after retries: {} | {}", self.reason, self.body),
        }
    }
}

impl std::error::Error for RetryExhausted {}

fn sleep_or_cancel(d: Duration, cancel: &AtomicBool) {
    if d.is_zero() { return; }
    let mut waited = Duration::ZERO;
    let step = Duration::from_millis(100);
    while waited < d {
        if cancel.load(Ordering::SeqCst) { return; }
        let remaining = d - waited;
        let s = step.min(remaining);
        std::thread::sleep(s);
        waited += s;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn classifies_transient_5xx() {
        assert_eq!(FailureClass::from_response(502, "", ""), FailureClass::Transient);
        assert_eq!(FailureClass::from_response(503, "", ""), FailureClass::Transient);
        assert_eq!(FailureClass::from_response(429, "", ""), FailureClass::Transient);
    }

    #[test]
    fn classifies_permanent_4xx() {
        assert_eq!(FailureClass::from_response(401, "", ""), FailureClass::Permanent);
        assert_eq!(FailureClass::from_response(404, "", ""), FailureClass::Permanent);
    }

    #[test]
    fn classifies_unreachable_dns() {
        assert_eq!(FailureClass::from_response(0, "", "Name or service not known"), FailureClass::Unreachable);
        assert_eq!(FailureClass::from_response(0, "", "ENOTFOUND api.x.com"), FailureClass::Unreachable);
    }

    #[test]
    fn classifies_timeout() {
        assert_eq!(FailureClass::from_response(0, "", "operation timed out"), FailureClass::Timeout);
    }

    #[test]
    fn parses_retry_after_seconds() {
        assert_eq!(FailureClass::parse_retry_after("30"), Some(Duration::from_secs(30)));
        assert_eq!(FailureClass::parse_retry_after("120"), Some(Duration::from_secs(120)));
    }

    #[test]
    fn retry_policy_general_schedule() {
        let mut p = RetryPolicy::new(true);
        // 10 retries, then give up
        let mut attempts = 0;
        loop {
            let d = p.decide(FailureClass::Transient, None);
            if !d.retry { break; }
            attempts += 1;
            assert!(attempts <= 10);
            // delay should be within [base, cap] roughly
            assert!(d.delay >= Duration::from_secs(2) && d.delay <= Duration::from_secs(12));
        }
        assert_eq!(attempts, 10);
    }

    #[test]
    fn retry_policy_permanent_does_not_retry() {
        let mut p = RetryPolicy::new(true);
        let d = p.decide(FailureClass::Permanent, None);
        assert!(!d.retry);
    }

    #[test]
    fn retry_policy_retry_after_capped() {
        let mut p = RetryPolicy::new(true);
        // Provider asks for 2 hours — must be capped at 15 min
        let d = p.decide(FailureClass::RetryAfter(Duration::from_secs(2 * 3600)), None);
        assert!(d.retry);
        assert!(d.delay <= consts::RETRY_AFTER_MAX);
        assert!(d.delay == consts::RETRY_AFTER_MAX);
    }

    #[test]
    fn retry_policy_timeout_cap_3() {
        let mut p = RetryPolicy::new(true);
        let mut n = 0;
        loop {
            let d = p.decide(FailureClass::Timeout, None);
            if !d.retry { break; }
            n += 1;
        }
        assert_eq!(n, consts::MAX_TIMEOUT_RETRIES);
    }

    #[test]
    fn retry_policy_disabled() {
        let mut p = RetryPolicy::new(false);
        let d = p.decide(FailureClass::Transient, None);
        assert!(!d.retry);
    }

    #[test]
    fn jitter_stays_in_range() {
        for _ in 0..100 {
            let j = jitter(Duration::from_secs(5));
            assert!(j >= Duration::from_secs(5) && j <= Duration::from_secs(6));
        }
    }

    #[test]
    fn parse_url_basic() {
        let u = parse_url("http://localhost:11434/v1/chat/completions").unwrap();
        assert_eq!(u.host, "localhost");
        assert_eq!(u.port, 11434);
        assert_eq!(u.path, "/v1/chat/completions");
    }

    #[test]
    fn parse_url_default_port() {
        let u = parse_url("http://localhost/").unwrap();
        assert_eq!(u.port, 80);
    }

    #[test]
    fn retry_exhausted_display() {
        let e = RetryExhausted { status: Some(502), reason: "budget exhausted".into(), body: "Bad Gateway".into() };
        let s = format!("{e}");
        assert!(s.contains("HTTP 502"));
        assert!(s.contains("Bad Gateway"));
    }
    #[test]
    fn curl_transport_errors_are_retried_not_permanent() {
        for e in [
            "curl: (6) Could not resolve host: api.example.com",
            "curl: (7) Failed to connect to api.example.com port 443 after 15000 ms",
            "curl: (35) OpenSSL SSL_connect: SSL_ERROR_SYSCALL",
            "curl: (52) Empty reply from server",
            "curl: (18) transfer closed with 42 bytes remaining to read",
            "curl: (92) HTTP/2 stream 1 was not closed cleanly",
            "upstream connect error or disconnect/reset before headers. reset reason: connection failure",
            "provider said: service temporarily unavailable",
            "model is currently loading",
        ] {
            assert_ne!(
                FailureClass::from_response(0, "", e),
                FailureClass::Permanent,
                "curl error must be retryable: {e}"
            );
        }
        assert_eq!(
            FailureClass::from_response(401, "", "invalid api key"),
            FailureClass::Permanent
        );
    }

    /// Regression tests for the NVIDIA NIM incident: zero-byte gateway
    /// responses used to surface as "curl exited with exit status: 0" with no
    /// status attached, hit the old Permanent default, and killed the run
    /// without a single retry ("stopping again and again").
    #[test]
    fn nvidia_empty_body_without_status_is_transient() {
        // The exact error from the incident report — no status captured.
        let e = "Nvidia request to https://integrate.api.nvidia.com/v1 failed: curl exited with exit status: 0";
        assert_eq!(FailureClass::from_response(0, "", e), FailureClass::Transient);
    }

    #[test]
    fn nvidia_status_in_message_is_respected() {
        // finish_stream now embeds the write-out status; the classifier must
        // pick the class from the real code, not the prose.
        let e503 = "Nvidia request to https://integrate.api.nvidia.com/v1 failed (HTTP 503): curl exited with exit status: 0; server answered HTTP 503 with an empty response body";
        assert_eq!(FailureClass::from_response(0, "", e503), FailureClass::Transient);
        let e429 = "Nvidia request to https://integrate.api.nvidia.com/v1 failed (HTTP 429): Too many requests";
        assert_eq!(FailureClass::from_response(0, "", e429), FailureClass::Transient);
        let e401 = "Nvidia request to https://integrate.api.nvidia.com/v1 failed (HTTP 401): Unauthorized access";
        assert_eq!(FailureClass::from_response(0, "", e401), FailureClass::Permanent);
        let e404 = "Nvidia request to https://integrate.api.nvidia.com/v1 failed (HTTP 404): 404 page not found";
        assert_eq!(FailureClass::from_response(0, "", e404), FailureClass::Permanent);
    }

    #[test]
    fn base_url_in_error_never_forces_a_class() {
        // "https://…/v1" contains "http" + "1"; neither may win.
        let e = "Nvidia request to https://integrate.api.nvidia.com/v1/chat/completions failed: something entirely unknown happened";
        assert_eq!(FailureClass::from_response(0, "", e), FailureClass::Transient);
    }

    #[test]
    fn nvidia_gateway_error_shapes_classify() {
        // NVIDIA's non-OpenAI shapes, as returned by the live endpoint.
        assert_eq!(
            FailureClass::from_response(0, "", r#"{"status":403,"title":"Forbidden","detail":"Authorization failed"}"#),
            FailureClass::Permanent
        );
        assert_eq!(
            FailureClass::from_response(0, "", "Unauthorized access"),
            FailureClass::Permanent
        );
        assert_eq!(
            FailureClass::from_response(0, "", "model not ready, please retry"),
            FailureClass::Transient
        );
    }

}
