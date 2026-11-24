//! dsd-live — custom-built live tracing tool (no frameworks).
//!
//! STEP 1  hand-rolled argument + upstream-spec parsing
//! STEP 2  hand-rolled HTTP head parsing (no httparse)
//! STEP 3  custom TLS upstream client (skip-verify — tracing tool)
//! STEP 4  timeline recorder: console lines + JSONL file (no serde)
//! STEP 5  relay engine ported verbatim from the proven rc-dsd tap
//!         (exact / chunked / until-eof / upgrade tunnel, keep-alive loop)
//! STEP 6  hand-rolled minimal PostgreSQL wire client (no tokio-postgres)
//!         + hand-rolled MD5 (RFC 1321) for the md5 auth exchange
//! STEP 7  main dispatch:  `dsd-live` = tap mode,  `dsd-live pg` = engine mode
//!
//! tap mode:
//!   dsd-live --listen 127.0.0.1:8807 --upstream https://host:443 [--out tl.jsonl]
//! pg mode (talks DIRECTLY to a real local database engine):
//!   dsd-live pg --addr 127.0.0.1:5432 --user u --pass p --db d --sql "SELECT ..."

use std::collections::HashMap;
use std::fs::{File, OpenOptions};
use std::io::Write as IoWrite;
use std::sync::{Arc, Mutex};
use std::time::Instant;

use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};

// ─────────────────────────────────────────────────────────────────────────────
// STEP 1 — arguments (hand-rolled)
// ─────────────────────────────────────────────────────────────────────────────

struct Args {
    flags: HashMap<String, String>,
    positional: Vec<String>,
}

impl Args {
    fn parse() -> Result<Args, String> {
        let mut flags = HashMap::new();
        let mut positional = Vec::new();
        let mut it = std::env::args().skip(1);
        while let Some(a) = it.next() {
            if let Some(rest) = a.strip_prefix("--") {
                let (k, v) = match rest.split_once('=') {
                    Some((k, v)) => (k.to_string(), v.to_string()),
                    None => (
                        rest.to_string(),
                        it.next().ok_or_else(|| format!("--{rest} needs a value"))?,
                    ),
                };
                flags.insert(k, v);
            } else {
                positional.push(a);
            }
        }
        Ok(Args { flags, positional })
    }
    fn get(&self, k: &str) -> Option<&str> {
        self.flags.get(k).map(|s| s.as_str())
    }
}

/// "host:port", "http://host:port", "https://host:port" → (host, port, tls)
fn parse_upstream(s: &str) -> Result<(String, u16, bool), String> {
    let s = s.trim();
    let (tls, rest) = match s.split_once("://") {
        Some(("https", r)) => (true, r),
        Some(("http", r)) => (false, r),
        Some((x, _)) => {
            return Err(format!("scheme must be http:// or https://, got \"{x}://\""))
        }
        None => (false, s),
    };
    let (h, p) = rest
        .split_once(':')
        .ok_or_else(|| format!("upstream needs host:port, got \"{s}\""))?;
    let port: u16 = p
        .trim_end_matches('/')
        .parse()
        .map_err(|_| format!("bad port in \"{s}\""))?;
    Ok((h.to_string(), port, tls))
}

// ─────────────────────────────────────────────────────────────────────────────
// STEP 2 — HTTP head parsing (hand-rolled)
// ─────────────────────────────────────────────────────────────────────────────

const HEAD_MAX: usize = 64 * 1024;

/// Read until the CRLFCRLF terminator; returns head bytes + any body bytes
/// already read past the head (must be forwarded, never dropped).
async fn read_head<R: AsyncRead + Unpin>(
    r: &mut R,
    buf: &mut [u8],
) -> Result<(Vec<u8>, Vec<u8>), ()> {
    let mut total = 0usize;
    loop {
        if total >= buf.len() {
            return Err(()); // head larger than HEAD_MAX
        }
        let n = r.read(&mut buf[total..]).await.map_err(|_| ())?;
        if n == 0 {
            return Err(()); // EOF before a complete head
        }
        total += n;
        if let Some(pos) = find_terminator(&buf[..total]) {
            let head_len = pos + 4;
            let head = buf[..head_len].to_vec();
            let leftover = buf[head_len..total].to_vec();
            return Ok((head, leftover));
        }
    }
}

fn find_terminator(b: &[u8]) -> Option<usize> {
    b.windows(4).position(|w| w == b"\r\n\r\n")
}

fn header_value(head: &str, name: &str) -> Option<String> {
    head.lines().skip(1).find_map(|l| {
        let (k, v) = l.split_once(':')?;
        if k.trim().eq_ignore_ascii_case(name) {
            Some(v.trim().to_string())
        } else {
            None
        }
    })
}

fn parse_request(head: &[u8]) -> (String, String, String, u64, bool) {
    let text = String::from_utf8_lossy(head);
    let mut it = text.lines();
    let first = it.next().unwrap_or("");
    let mut parts = first.split_whitespace();
    let method = parts.next().unwrap_or("?").to_string();
    let path = parts.next().unwrap_or("?").to_string();
    let host = header_value(&text, "host").unwrap_or_default();
    let len = header_value(&text, "content-length")
        .and_then(|v| v.parse().ok())
        .unwrap_or(0);
    let chunked = header_value(&text, "transfer-encoding")
        .map(|v| v.to_ascii_lowercase().contains("chunked"))
        .unwrap_or(false);
    (method, path, host, len, chunked)
}

fn parse_response(head: &[u8]) -> (u16, Option<u64>, bool, bool) {
    let text = String::from_utf8_lossy(head);
    let first = text.lines().next().unwrap_or("");
    let status = first
        .split_whitespace()
        .nth(1)
        .and_then(|s| s.parse().ok())
        .unwrap_or(0);
    let len = header_value(&text, "content-length")
        .and_then(|v| v.trim().parse().ok());
    let chunked = header_value(&text, "transfer-encoding")
        .map(|v| v.to_ascii_lowercase().contains("chunked"))
        .unwrap_or(false);
    let upgrade = header_value(&text, "upgrade").is_some() && status == 101;
    (status, len, chunked, upgrade)
}

// ─────────────────────────────────────────────────────────────────────────────
// STEP 3 — custom TLS upstream client (skip-verify: it is a tracing tool;
//          certificate trust is the real client's job, not ours)
// ─────────────────────────────────────────────────────────────────────────────

fn tls_connector() -> tokio_rustls::TlsConnector {
    let provider = Arc::new(rustls::crypto::ring::default_provider());
    let cfg = rustls::ClientConfig::builder_with_provider(provider)
        .with_safe_default_protocol_versions()
        .expect("rustls provider")
        .dangerous()
        .with_custom_certificate_verifier(Arc::new(NoVerify))
        .with_no_client_auth();
    tokio_rustls::TlsConnector::from(Arc::new(cfg))
}

#[derive(Debug)]
struct NoVerify;
impl rustls::client::danger::ServerCertVerifier for NoVerify {
    fn verify_server_cert(
        &self,
        _end_entity: &rustls::pki_types::CertificateDer<'_>,
        _intermediates: &[rustls::pki_types::CertificateDer<'_>],
        _server_name: &rustls::pki_types::ServerName<'_>,
        _ocsp: &[u8],
        _now: rustls::pki_types::UnixTime,
    ) -> Result<rustls::client::danger::ServerCertVerified, rustls::Error> {
        Ok(rustls::client::danger::ServerCertVerified::assertion())
    }
    fn verify_tls12_signature(
        &self,
        _message: &[u8],
        _cert: &rustls::pki_types::CertificateDer<'_>,
        _dss: &rustls::DigitallySignedStruct,
    ) -> Result<rustls::client::danger::HandshakeSignatureValid, rustls::Error> {
        Ok(rustls::client::danger::HandshakeSignatureValid::assertion())
    }
    fn verify_tls13_signature(
        &self,
        _message: &[u8],
        _cert: &rustls::pki_types::CertificateDer<'_>,
        _dss: &rustls::DigitallySignedStruct,
    ) -> Result<rustls::client::danger::HandshakeSignatureValid, rustls::Error> {
        Ok(rustls::client::danger::HandshakeSignatureValid::assertion())
    }
    fn supported_verify_schemes(&self) -> Vec<rustls::SignatureScheme> {
        vec![
            rustls::SignatureScheme::RSA_PKCS1_SHA256,
            rustls::SignatureScheme::RSA_PKCS1_SHA384,
            rustls::SignatureScheme::RSA_PKCS1_SHA512,
            rustls::SignatureScheme::ECDSA_NISTP256_SHA256,
            rustls::SignatureScheme::ECDSA_NISTP384_SHA384,
            rustls::SignatureScheme::ED25519,
            rustls::SignatureScheme::RSA_PSS_SHA256,
            rustls::SignatureScheme::RSA_PSS_SHA384,
            rustls::SignatureScheme::RSA_PSS_SHA512,
        ]
    }
}

/// Plain TCP or TLS upstream; boxed halves so one routine serves both.
async fn connect_upstream(
    upstream_addr: &str,
    use_tls: bool,
    tls_host: &str,
) -> Result<
    (
        Box<dyn AsyncRead + Unpin + Send>,
        Box<dyn AsyncWrite + Unpin + Send>,
    ),
    String,
> {
    let tcp = TcpStream::connect(upstream_addr)
        .await
        .map_err(|e| format!("upstream {upstream_addr} connect failed: {e}"))?;
    if use_tls {
        let connector = tls_connector();
        let domain = rustls::pki_types::ServerName::try_from(tls_host.to_string())
            .map_err(|e| format!("bad tls host \"{tls_host}\": {e}"))?;
        let stream = connector
            .connect(domain, tcp)
            .await
            .map_err(|e| format!("tls handshake with {upstream_addr}: {e}"))?;
        let (r, w) = tokio::io::split(stream);
        Ok((Box::new(r), Box::new(w)))
    } else {
        let (r, w) = tcp.into_split();
        Ok((Box::new(r), Box::new(w)))
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// STEP 4 — timeline recorder: console + JSONL (no serde)
// ─────────────────────────────────────────────────────────────────────────────

struct Tl {
    t0: Instant,
    out: Option<Mutex<File>>,
    dump: Option<Mutex<File>>,
}

impl Tl {
    fn new(path: Option<&str>, dump: Option<&str>) -> Result<Tl, String> {
        let f = match path {
            Some(p) => Some(
                OpenOptions::new()
                    .create(true)
                    .append(true)
                    .open(p)
                    .map_err(|e| format!("cannot open {p}: {e}"))?,
            ),
            None => None,
        };
        let d = match dump {
            Some(p) => Some(
                OpenOptions::new()
                    .create(true)
                    .append(true)
                    .open(p)
                    .map_err(|e| format!("cannot open {p}: {e}"))?,
            ),
            None => None,
        };
        Ok(Tl {
            t0: Instant::now(),
            out: f.map(Mutex::new),
            dump: d.map(Mutex::new),
        })
    }
    fn dump_bytes(&self, tag: &str, b: &[u8]) {
        if let Some(m) = &self.dump {
            if let Ok(mut f) = m.lock() {
                let _ = writeln!(f, "--- {tag} {} bytes ---", b.len());
                for chunk in b.chunks(32) {
                    let hexs: String = chunk
                        .iter()
                        .map(|x| format!("{x:02x}"))
                        .collect::<Vec<_>>()
                        .join(" ");
                    let ascii: String = chunk
                        .iter()
                        .map(|&x| if (0x20..0x7f).contains(&x) { x as char } else { '.' })
                        .collect();
                    let _ = writeln!(f, "{hexs:<96}  {ascii}");
                }
            }
        }
    }
    fn log(&self, ev: &str, fields: &[(&str, String)]) {
        let t = self.t0.elapsed().as_secs_f64() * 1000.0;
        let mut line = format!("[{ev:<5}] t=+{:9.1}ms", t);
        let mut json = format!("{{\"ev\":\"{ev}\",\"t_ms\":{t:.1}");
        for (k, v) in fields {
            line.push_str(&format!(" {k}={}", v.replace(' ', "_")));
            json.push_str(&format!(",\"{k}\":{}", jstr(v)));
        }
        json.push('}');
        println!("{line}");
        if let Some(m) = &self.out {
            if let Ok(mut f) = m.lock() {
                let _ = writeln!(f, "{json}");
            }
        }
    }
}

/// Hand-rolled JSON string escaping.
fn jstr(s: &str) -> String {
    let mut o = String::from("\"");
    for c in s.chars() {
        match c {
            '"' => o.push_str("\\\""),
            '\\' => o.push_str("\\\\"),
            '\n' => o.push_str("\\n"),
            '\r' => o.push_str("\\r"),
            '\t' => o.push_str("\\t"),
            c if (c as u32) < 0x20 => o.push_str(&format!("\\u{:04x}", c as u32)),
            c => o.push(c),
        }
    }
    o.push('"');
    o
}

// ─────────────────────────────────────────────────────────────────────────────
// STEP 5 — relay engine (verbatim port from the proven rc-dsd tap)
// ─────────────────────────────────────────────────────────────────────────────

/// Relay exactly `n` bytes; returns bytes actually relayed.
async fn relay_exact<R, W>(r: &mut R, w: &mut W, n: u64, buf: &mut [u8]) -> Result<u64, ()>
where
    R: AsyncRead + Unpin + ?Sized,
    W: AsyncWrite + Unpin + ?Sized,
{
    let mut left = n;
    while left > 0 {
        let want = left.min(buf.len() as u64) as usize;
        let got = r.read(&mut buf[..want]).await.map_err(|_| ())?;
        if got == 0 {
            return Err(());
        }
        w.write_all(&buf[..got]).await.map_err(|_| ())?;
        left -= got as u64;
    }
    Ok(n)
}

/// Relay until EOF (HTTP/1.0 close-delimited bodies).
async fn relay_until_eof<R, W>(r: &mut R, w: &mut W, buf: &mut [u8]) -> u64
where
    R: AsyncRead + Unpin + ?Sized,
    W: AsyncWrite + Unpin + ?Sized,
{
    let mut total = 0u64;
    loop {
        match r.read(buf).await {
            Ok(0) | Err(_) => break,
            Ok(n) => {
                if w.write_all(&buf[..n]).await.is_err() {
                    break;
                }
                total += n as u64;
            }
        }
    }
    total
}

/// Relay a chunked body verbatim: EVERY framing byte (size lines, CRLFs,
/// trailers) is forwarded exactly as received so the downstream parser stays
/// in sync; sizes are parsed locally only to know when to stop.
async fn relay_chunked<R, W>(r: &mut R, w: &mut W, buf: &mut [u8]) -> Result<(), ()>
where
    R: AsyncRead + Unpin + ?Sized,
    W: AsyncWrite + Unpin + ?Sized,
{
    loop {
        // read the size line verbatim (bytes incl. CRLF)
        let mut raw = Vec::new();
        loop {
            let mut byte = [0u8; 1];
            let n = r.read(&mut byte).await.map_err(|_| ())?;
            if n == 0 {
                return Err(());
            }
            raw.push(byte[0]);
            if byte[0] == b'\n' {
                break;
            }
            if raw.len() > 64 {
                return Err(());
            }
        }
        let text = String::from_utf8_lossy(&raw);
        let size =
            usize::from_str_radix(text.trim().split(';').next().unwrap_or("0").trim(), 16)
                .map_err(|_| ())?;
        w.write_all(&raw).await.map_err(|_| ())?;
        if size == 0 {
            // trailer lines, forwarded verbatim, until the empty line
            loop {
                let mut tr = Vec::new();
                loop {
                    let mut byte = [0u8; 1];
                    let n = r.read(&mut byte).await.map_err(|_| ())?;
                    if n == 0 {
                        return Ok(()); // some servers close right after 0\r\n
                    }
                    tr.push(byte[0]);
                    if byte[0] == b'\n' {
                        break;
                    }
                    if tr.len() > 1024 {
                        return Err(());
                    }
                }
                w.write_all(&tr).await.map_err(|_| ())?;
                if tr == b"\r\n" || tr == b"\n" {
                    return Ok(()); // empty line ends the trailer block
                }
            }
        }
        relay_exact(r, w, size as u64, buf).await?;
        let mut crlf = [0u8; 2];
        r.read_exact(&mut crlf).await.map_err(|_| ())?;
        w.write_all(&crlf).await.map_err(|_| ())?;
    }
}

/// Opaque bidirectional tunnel (websocket upgrade).
async fn tunnel(
    cr: &mut (dyn AsyncRead + Unpin + Send),
    uw: &mut (dyn AsyncWrite + Unpin + Send),
    ur: &mut (dyn AsyncRead + Unpin + Send),
    cw: &mut (dyn AsyncWrite + Unpin + Send),
) {
    let _ = tokio::select! {
        r = tokio::io::copy(&mut *cr, &mut *uw) => r,
        r = tokio::io::copy(&mut *ur, &mut *cw) => r,
    };
}

/// One proxied connection: sequential request/response pairs (keep-alive).
async fn handle_conn(
    mut client: TcpStream,
    upstream_addr: String,
    use_tls: bool,
    tls_host: String,
    tl: Arc<Tl>,
) {
    let peer = client
        .peer_addr()
        .map(|a| a.to_string())
        .unwrap_or_default();
    let (mut ur, mut uw) = match connect_upstream(&upstream_addr, use_tls, &tls_host).await {
        Ok(h) => h,
        Err(e) => {
            tl.log(
                "NOTE",
                &[("stage", "upstream-connect".into()), ("error", e)],
            );
            return;
        }
    };
    let (mut cr, mut cw) = client.split();
    let mut buf = vec![0u8; HEAD_MAX];
    let mut seq = 0u64;

    loop {
        // ── request head ──
        let (head, leftover) = match read_head(&mut cr, &mut buf).await {
            Ok(h) => h,
            Err(_) => break,
        };
        seq += 1;
        tl.dump_bytes("request-head", &head);
        tl.dump_bytes("request-leftover", &leftover);
        let (method, path, host, req_len, req_chunked) = parse_request(&head);
        let started = Instant::now();
        let mut req_bytes = (head.len() + leftover.len()) as u64;
        tl.log(
            "ENTRY",
            &[
                ("n", seq.to_string()),
                ("peer", peer.clone()),
                ("m", method.clone()),
                ("path", path.clone()),
                ("host", host.clone()),
                ("clen", req_len.to_string()),
            ],
        );

        let up_ok = uw.write_all(&head).await.is_ok()
            && (leftover.is_empty() || uw.write_all(&leftover).await.is_ok());
        if !up_ok {
            tl.log("EXIT", &[("n", seq.to_string()), ("status", "0".into()), ("note", "upstream write failed".into())]);
            break;
        }
        let mut req_failed = false;
        if req_chunked {
            if relay_chunked(&mut cr, &mut uw, &mut buf).await.is_err() {
                req_failed = true;
            }
        } else if req_len > leftover.len() as u64 {
            let still = req_len - leftover.len() as u64;
            let got = relay_exact(&mut cr, &mut uw, still, &mut buf)
                .await
                .unwrap_or(0);
            req_bytes += got;
            req_failed = got < still;
        }
        if req_failed {
            tl.log("EXIT", &[("n", seq.to_string()), ("status", "0".into()), ("note", "client request truncated".into())]);
            break;
        }

        // ── response head ──
        let (rhead, rleftover) = match read_head(&mut ur, &mut buf).await {
            Ok(h) => h,
            Err(_) => {
                tl.log("EXIT", &[("n", seq.to_string()), ("status", "0".into()), ("ms", format!("{:.1}", started.elapsed().as_secs_f64() * 1000.0)), ("note", "no response (upstream closed)".into())]);
                break;
            }
        };
        tl.dump_bytes("response-head", &rhead);
        tl.dump_bytes("response-leftover", &rleftover);
        let (status, resp_len, resp_chunked, upgrade) = parse_response(&rhead);
        let mut resp_bytes = (rhead.len() + rleftover.len()) as u64;
        let server = header_value(&String::from_utf8_lossy(&rhead), "server")
            .unwrap_or_else(|| "-".into());
        let down_ok = cw.write_all(&rhead).await.is_ok()
            && (rleftover.is_empty() || cw.write_all(&rleftover).await.is_ok());
        if !down_ok {
            tl.log("EXIT", &[("n", seq.to_string()), ("status", status.to_string()), ("note", "client write failed".into())]);
            break;
        }
        tl.log(
            "EXIT",
            &[
                ("n", seq.to_string()),
                ("status", status.to_string()),
                ("ms", format!("{:.1}", started.elapsed().as_secs_f64() * 1000.0)),
                ("rlen", resp_len.map(|v| v.to_string()).unwrap_or_else(|| if resp_chunked { "chunked".into() } else { "eof".into() })),
                ("server", server.clone()),
                ("reqB", req_bytes.to_string()),
                ("respB", resp_bytes.to_string()),
            ],
        );

        if upgrade {
            tunnel(&mut cr, &mut uw, &mut ur, &mut cw).await;
            break;
        }
        let mut body_failed = false;
        if resp_chunked {
            if relay_chunked(&mut ur, &mut cw, &mut buf).await.is_err() {
                body_failed = true;
            }
        } else if let Some(n) = resp_len {
            if n > rleftover.len() as u64 {
                let still = n - rleftover.len() as u64;
                let got = relay_exact(&mut ur, &mut cw, still, &mut buf)
                    .await
                    .unwrap_or(0);
                resp_bytes += got;
                body_failed = got < still;
            }
        } else {
            let got = relay_until_eof(&mut ur, &mut cw, &mut buf).await;
            resp_bytes += got;
        }
        if body_failed {
            tl.log("NOTE", &[("n", seq.to_string()), ("note", "response truncated".into())]);
            break;
        }
    }
    let _ = client.shutdown().await;
}

async fn tap_main(args: Args) {
    let listen = args.get("listen").unwrap_or("127.0.0.1:8807").to_string();
    let upstream = args.get("upstream").expect("--upstream host:port required").to_string();
    let (host, port, tls) = parse_upstream(&upstream).expect("bad --upstream");
    let tl = Arc::new(Tl::new(args.get("out"), args.get("dump")).expect("timeline file"));
    let scheme = if tls { "https" } else { "http" };
    tl.log(
        "TAP",
        &[
            ("listen", listen.clone()),
            ("upstream", format!("{scheme}://{host}:{port}")),
            ("tls", tls.to_string()),
        ],
    );
    let listener = TcpListener::bind(&listen).await.expect("bind tap");
    let upstream_addr = format!("{host}:{port}");
    loop {
        match listener.accept().await {
            Ok((client, _)) => {
                let up = upstream_addr.clone();
                let h = host.clone();
                let tl2 = tl.clone();
                tokio::spawn(handle_conn(client, up, tls, h, tl2));
            }
            Err(_) => {
                tokio::time::sleep(std::time::Duration::from_millis(50)).await;
            }
        }
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// STEP 6 — hand-rolled PostgreSQL wire client + hand-rolled MD5 (RFC 1321)
// ─────────────────────────────────────────────────────────────────────────────

/// Pure-Rust MD5. K[i] = trunc(|sin(i+1)| · 2³²), S = standard shift table.
fn md5_hex(data: &[u8]) -> String {
    const S: [u32; 64] = [
        7, 12, 17, 22, 7, 12, 17, 22, 7, 12, 17, 22, 7, 12, 17, 22, 5, 9, 14, 20, 5, 9, 14, 20,
        5, 9, 14, 20, 5, 9, 14, 20, 4, 11, 16, 23, 4, 11, 16, 23, 4, 11, 16, 23, 4, 11, 16, 23,
        6, 10, 15, 21, 6, 10, 15, 21, 6, 10, 15, 21, 6, 10, 15, 21,
    ];
    let mut k = [0u32; 64];
    for (i, kv) in k.iter_mut().enumerate() {
        *kv = (((i as f64) + 1.0).sin().abs() * 4294967296.0).floor() as u32;
    }
    let mut msg = data.to_vec();
    let bitlen = (data.len() as u64).wrapping_mul(8);
    msg.push(0x80);
    while msg.len() % 64 != 56 {
        msg.push(0);
    }
    msg.extend_from_slice(&bitlen.to_le_bytes());
    let (mut a0, mut b0, mut c0, mut d0) =
        (0x67452301u32, 0xefcdab89u32, 0x98badcfEu32, 0x10325476u32);
    for chunk in msg.chunks_exact(64) {
        let mut m = [0u32; 16];
        for j in 0..16 {
            m[j] = u32::from_le_bytes([
                chunk[4 * j],
                chunk[4 * j + 1],
                chunk[4 * j + 2],
                chunk[4 * j + 3],
            ]);
        }
        let (mut a, mut b, mut c, mut d) = (a0, b0, c0, d0);
        for i in 0..64 {
            let (f, g) = match i / 16 {
                0 => ((b & c) | (!b & d), i),
                1 => ((d & b) | (!d & c), (5 * i + 1) % 16),
                2 => (b ^ c ^ d, (3 * i + 5) % 16),
                _ => (c ^ (b | !d), (7 * i) % 16),
            };
            let tmp = d;
            d = c;
            c = b;
            b = b.wrapping_add(
                a.wrapping_add(f)
                    .wrapping_add(k[i])
                    .wrapping_add(m[g])
                    .rotate_left(S[i]),
            );
            a = tmp;
        }
        a0 = a0.wrapping_add(a);
        b0 = b0.wrapping_add(b);
        c0 = c0.wrapping_add(c);
        d0 = d0.wrapping_add(d);
    }
    let mut out = Vec::with_capacity(16);
    out.extend_from_slice(&a0.to_le_bytes());
    out.extend_from_slice(&b0.to_le_bytes());
    out.extend_from_slice(&c0.to_le_bytes());
    out.extend_from_slice(&d0.to_le_bytes());
    out.iter().map(|b| format!("{b:02x}")).collect()
}

struct Cur<'a> {
    b: &'a [u8],
    p: usize,
}

impl<'a> Cur<'a> {
    fn i16(&mut self) -> Result<i16, String> {
        if self.p + 2 > self.b.len() {
            return Err("row too short".into());
        }
        let v = i16::from_be_bytes([self.b[self.p], self.b[self.p + 1]]);
        self.p += 2;
        Ok(v)
    }
    fn i32(&mut self) -> Result<i32, String> {
        if self.p + 4 > self.b.len() {
            return Err("row too short".into());
        }
        let v = i32::from_be_bytes([
            self.b[self.p],
            self.b[self.p + 1],
            self.b[self.p + 2],
            self.b[self.p + 3],
        ]);
        self.p += 4;
        Ok(v)
    }
    fn cstr(&mut self) -> Result<String, String> {
        let start = self.p;
        while self.p < self.b.len() && self.b[self.p] != 0 {
            self.p += 1;
        }
        if self.p >= self.b.len() {
            return Err("unterminated string".into());
        }
        let s = String::from_utf8_lossy(&self.b[start..self.p]).to_string();
        self.p += 1;
        Ok(s)
    }
    fn skip(&mut self, n: usize) -> Result<(), String> {
        self.p += n;
        if self.p > self.b.len() {
            return Err("row too short".into());
        }
        Ok(())
    }
    fn bytes(&mut self, n: usize) -> Result<Vec<u8>, String> {
        if self.p + n > self.b.len() {
            return Err("row too short".into());
        }
        let v = self.b[self.p..self.p + n].to_vec();
        self.p += n;
        Ok(v)
    }
}

/// Human text from an ErrorResponse body (fields: code byte + cstring, …).
fn pg_error_text(body: &[u8]) -> String {
    let mut cur = Cur { b: body, p: 0 };
    let mut parts: Vec<String> = Vec::new();
    while cur.p < body.len() {
        let code = body[cur.p];
        cur.p += 1;
        if code == 0 {
            break;
        }
        if let Ok(v) = cur.cstr() {
            match code {
                b'S' => parts.push(format!("severity={v}")),
                b'C' => parts.push(format!("code={v}")),
                b'M' => parts.push(format!("msg={v}")),
                b'V' => parts.push(format!("detail={v}")),
                _ => {}
            }
        }
    }
    if parts.is_empty() {
        "(no error fields)".into()
    } else {
        parts.join(", ")
    }
}

/// Minimal synchronous PostgreSQL client: startup → auth (ok/cleartext/md5)
/// → simple Query → RowDescription/DataRow/CommandComplete/ReadyForQuery.
fn pg_run(
    addr: &str,
    user: &str,
    pass: &str,
    db: &str,
    sql: &str,
    tl: &Tl,
) -> Result<usize, String> {
    use std::io::{Read, Write};
    use std::net::TcpStream as StdTcp;
    let t0 = Instant::now();
    let mut s = StdTcp::connect(addr).map_err(|e| format!("connect {addr}: {e}"))?;
    s.set_nodelay(true).ok();

    // startup: len(u32) + 196608 + "user\0u\0database\0d\0\0"
    let mut body: Vec<u8> = Vec::new();
    body.extend_from_slice(&196608u32.to_be_bytes());
    for (k, v) in [("user", user), ("database", db)] {
        body.extend_from_slice(k.as_bytes());
        body.push(0);
        body.extend_from_slice(v.as_bytes());
        body.push(0);
    }
    body.push(0);
    let mut msg: Vec<u8> = Vec::new();
    msg.extend_from_slice(&((body.len() as u32 + 4).to_be_bytes()));
    msg.extend_from_slice(&body);
    s.write_all(&msg).map_err(|e| format!("startup write: {e}"))?;

    let mut read_msg = |s: &mut StdTcp| -> Result<(u8, Vec<u8>), String> {
        let mut hb = [0u8; 5];
        s.read_exact(&mut hb).map_err(|e| format!("read: {e}"))?;
        let len = u32::from_be_bytes([hb[1], hb[2], hb[3], hb[4]]) as usize;
        if len < 4 {
            return Err("bad message length".into());
        }
        let mut body = vec![0u8; len - 4];
        s.read_exact(&mut body).map_err(|e| format!("read: {e}"))?;
        Ok((hb[0], body))
    };

    // auth handshake
    loop {
        let (t, body) = read_msg(&mut s)?;
        match t {
            b'R' => {
                if body.len() < 4 {
                    return Err("bad auth message".into());
                }
                let code = u32::from_be_bytes([body[0], body[1], body[2], body[3]]);
                match code {
                    0 => {
                        tl.log("DB", &[("stage", "auth-ok".into()), ("ms", format!("{:.1}", t0.elapsed().as_secs_f64() * 1000.0))]);
                        // drain the rest of startup completion (S/K/N) until
                        // ReadyForQuery 'Z' — otherwise the query loop below
                        // breaks on that early 'Z' before seeing results
                        loop {
                            let (t2, _) = read_msg(&mut s)?;
                            if t2 == b'Z' {
                                break;
                            }
                        }
                        break;
                    }
                    3 => {
                        // cleartext password
                        let mut m = vec![b'p'];
                        m.extend_from_slice(&((pass.len() as u32 + 5).to_be_bytes()));
                        m.extend_from_slice(pass.as_bytes());
                        m.push(0);
                        s.write_all(&m).map_err(|e| format!("auth write: {e}"))?;
                    }
                    5 => {
                        // md5: md5(md5(password+username) + salt)
                        if body.len() < 8 {
                            return Err("bad md5 salt".into());
                        }
                        let salt = &body[4..8];
                        let inner = md5_hex(format!("{pass}{user}").as_bytes());
                        let mut combined = inner.into_bytes();
                        combined.extend_from_slice(salt);
                        let resp = format!("md5{}", md5_hex(&combined));
                        let mut m = vec![b'p'];
                        m.extend_from_slice(&((resp.len() as u32 + 5).to_be_bytes()));
                        m.extend_from_slice(resp.as_bytes());
                        m.push(0);
                        s.write_all(&m).map_err(|e| format!("auth write: {e}"))?;
                    }
                    10 => {
                        return Err(
                            "server wants SCRAM-SHA-256 — configure the local pg host auth as md5 or trust for this demo client".into(),
                        )
                    }
                    c => return Err(format!("unsupported auth method {c}")),
                }
            }
            b'E' => return Err(format!("server error: {}", pg_error_text(&body))),
            b'S' | b'K' | b'N' => {}
            other => return Err(format!("unexpected message type {other} during auth")),
        }
    }

    // simple query
    let mut q = vec![b'Q'];
    q.extend_from_slice(&((sql.len() as u32 + 5).to_be_bytes()));
    q.extend_from_slice(sql.as_bytes());
    q.push(0);
    s.write_all(&q).map_err(|e| format!("query write: {e}"))?;

    let mut cols: Vec<String> = Vec::new();
    let mut rows = 0usize;
    let mut tag = String::new();
    loop {
        let (t, body) = read_msg(&mut s)?;
        match t {
            b'T' => {
                // RowDescription
                let mut cur = Cur { b: &body, p: 0 };
                let n = cur.i16()?;
                cols.clear();
                for _ in 0..n {
                    cols.push(cur.cstr()?);
                    cur.skip(18)?;
                }
            }
            b'D' => {
                // DataRow
                let mut cur = Cur { b: &body, p: 0 };
                let n = cur.i16()?;
                let mut cells: Vec<String> = Vec::with_capacity(n as usize);
                for _ in 0..n {
                    let len = cur.i32()?;
                    if len < 0 {
                        cells.push("NULL".into());
                    } else {
                        let v = cur.bytes(len as usize)?;
                        cells.push(String::from_utf8_lossy(&v).to_string());
                    }
                }
                rows += 1;
                let rendered: Vec<String> = cols
                    .iter()
                    .zip(cells.iter())
                    .map(|(c, v)| format!("{c}={v}"))
                    .collect();
                tl.log("DB", &[("row", rows.to_string()), ("data", rendered.join(" | "))]);
            }
            b'C' => {
                let mut cur = Cur { b: &body, p: 0 };
                tag = cur.cstr()?;
            }
            b'E' => return Err(format!("query error: {}", pg_error_text(&body))),
            b'Z' => break,
            b'1' | b'2' | b'n' | b'N' | b'S' | b'K' | b't' => {}
            other => return Err(format!("unexpected message type {other} in query")),
        }
    }
    let ms = t0.elapsed().as_secs_f64() * 1000.0;
    let is_select = tag.starts_with("SELECT") || rows > 0;
    let nrows: usize = if tag.starts_with("INSERT") || tag.starts_with("UPDATE") || tag.starts_with("DELETE") {
        tag.split_whitespace().last().and_then(|w| w.parse().ok()).unwrap_or(rows)
    } else if is_select {
        rows
    } else {
        usize::MAX // DDL completed — not a row-count decision
    };
    let verdict = if nrows == usize::MAX {
        format!("COMMAND OK — {tag}")
    } else if nrows > 0 {
        format!("DATABASE SAID YES — {nrows} row(s) matched")
    } else {
        "DATABASE SAID NO — 0 rows matched".to_string()
    };
    tl.log(
        "VERDICT",
        &[
            ("tag", tag.clone()),
            ("ms", format!("{ms:.1}")),
            ("result", verdict),
        ],
    );
    Ok(if nrows == usize::MAX { 0 } else { nrows })
}

fn pg_main(args: Args) {
    let addr = args.get("addr").unwrap_or("127.0.0.1:5432").to_string();
    let user = args.get("user").unwrap_or("postgres").to_string();
    let pass = args.get("pass").unwrap_or("").to_string();
    let db = args.get("db").unwrap_or("postgres").to_string();
    let sql = args
        .get("sql")
        .map(|s| s.to_string())
        .unwrap_or_else(|| args.positional.join(" "));
    if sql.is_empty() {
        eprintln!("pg mode needs --sql \"...\"");
        std::process::exit(2);
    }
    let tl = Tl::new(args.get("out"), None).expect("timeline file");
    tl.log(
        "PG",
        &[
            ("addr", addr.clone()),
            ("db", db.clone()),
            ("user", user.clone()),
            ("client", "hand-rolled wire client".into()),
        ],
    );
    match pg_run(&addr, &user, &pass, &db, &sql, &tl) {
        Ok(n) => {
            let _ = n;
        }
        Err(e) => {
            tl.log("FAIL", &[("error", e)]);
            std::process::exit(1);
        }
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// STEP 7 — main dispatch (with md5 self-test)
// ─────────────────────────────────────────────────────────────────────────────

fn main() {
    // self-test the hand-rolled MD5 against the three RFC 1321 vectors
    assert_eq!(md5_hex(b""), "d41d8cd98f00b204e9800998ecf8427e");
    assert_eq!(md5_hex(b"abc"), "900150983cd24fb0d6963f7d28e17f72");
    assert_eq!(md5_hex(b"message digest"), "f96b697d7cb7938d525a2f31aaf161d0");

    let args = Args::parse().unwrap_or_else(|e| {
        eprintln!("args error: {e}");
        std::process::exit(2);
    });
    if args.positional.first().map(|s| s.as_str()) == Some("pg") {
        pg_main(args);
    } else {
        let rt = tokio::runtime::Builder::new_multi_thread()
            .enable_all()
            .build()
            .expect("tokio runtime");
        rt.block_on(tap_main(args));
    }
}
