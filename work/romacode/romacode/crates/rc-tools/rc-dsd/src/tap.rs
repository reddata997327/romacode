//! HTTP reverse-proxy tap: the Domain→Server entry point observer.
//!
//! Binds a local port and forwards every connection to the upstream web
//! server byte-for-byte while parsing ONLY the HTTP heads. Each request is
//! logged on ENTRY (method, path, host) and again on EXIT (status, latency,
//! byte counts). Because only heads are parsed and everything else is relayed
//! untouched, it works with any HTTP/1.x stack, chunked bodies, keep-alive
//! and websocket upgrades (tunneled opaque after the 101).
//!
//! This is what makes the journey visible: point the domain at the tap port
//! (or call it directly in tests) and every entry/exit appears in the DSD
//! timeline while `dsd.db_attach` watches the database below.

use std::sync::atomic::Ordering;
use std::sync::Arc;
use std::time::Instant;

use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};

use crate::session::{self, DsdEvent, TapHandle};

/// TLS client that accepts ANY certificate — the tap traces arbitrary
/// domains (dev stacks, replayed prod traffic); verification is done by the
/// real client, not by us.
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

/// Connect to the upstream over plain TCP or TLS; returns boxed halves so
/// one connection routine serves both transports.
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

const HEAD_MAX: usize = 64 * 1024;

/// Bind the tap and spawn the accept loop. Returns the handle whose `stop`
/// flag ends the loop (checked between accepts).
pub async fn start(
    listen_port: u16,
    upstream_host: &str,
    upstream_port: u16,
    use_tls: bool,
) -> Result<TapHandle, String> {
    let listener = TcpListener::bind(("0.0.0.0", listen_port))
        .await
        .map_err(|e| format!("cannot bind tap on :{listen_port}: {e}"))?;
    // port 0 = ephemeral (tests); report the real bound port
    let tap_port = listener.local_addr().map(|a| a.port()).unwrap_or(listen_port);
    let scheme = if use_tls { "https" } else { "http" };
    // display form (scheme://host:port) for TapHandle; connect form (host:port)
    // for handle_conn — TcpStream::connect takes a bare host:port
    let upstream = format!("{scheme}://{upstream_host}:{upstream_port}");
    let upstream_addr = format!("{upstream_host}:{upstream_port}");
    let stop = Arc::new(std::sync::atomic::AtomicBool::new(false));
    let stop2 = stop.clone();
    let upstream2 = upstream_addr.clone();
    let host2 = upstream_host.to_string();
    let task = tokio::spawn(async move {
        loop {
            if stop2.load(Ordering::Relaxed) {
                break;
            }
            match listener.accept().await {
                Ok((client, _)) => {
                    let up = upstream2.clone();
                    let host3 = host2.clone();
                    tokio::spawn(handle_conn(client, up, use_tls, host3));
                }
                Err(_) => {
                    tokio::time::sleep(std::time::Duration::from_millis(50)).await;
                }
            }
        }
    });
    Ok(TapHandle { tap_port, upstream, stop, task })
}

/// One proxied connection: sequential request/response pairs (keep-alive).
async fn handle_conn(mut client: TcpStream, upstream_addr: String, use_tls: bool, tls_host: String) {
    let tap_port = client.local_addr().map(|a| a.port()).unwrap_or(0);
    let peer = client.peer_addr().map(|a| a.to_string()).unwrap_or_default();
    let (mut ur, mut uw) = match connect_upstream(&upstream_addr, use_tls, &tls_host).await {
        Ok(h) => h,
        Err(e) => {
            session::record(DsdEvent::DbNote {
                ts: std::time::SystemTime::now(),
                backend: "tap".into(),
                text: e,
            });
            return;
        }
    };
    let (mut cr, mut cw) = client.split();
    let mut buf = vec![0u8; HEAD_MAX];

    loop {
        // ── request head ──
        let (head, leftover) = match read_head(&mut cr, &mut buf).await {
            Ok(h) => h,
            Err(_) => break, // EOF / idle keep-alive closed / garbage
        };
        // Bytes read past the head (small bodies often arrive in one packet)
        // belong to the body and MUST be forwarded, not dropped.
        let (method, path, host, req_len, req_chunked) = parse_request(&head);
        let id = session::record_entry(&method, &path, &host, &peer, tap_port, &upstream_addr);
        let started = Instant::now();
        let mut req_bytes = (head.len() + leftover.len()) as u64;

        let up_ok = uw.write_all(&head).await.is_ok()
            && (leftover.is_empty() || uw.write_all(&leftover).await.is_ok());
        if !up_ok {
            record_exit(id, 0, started, req_bytes, 0, "upstream write failed");
            break;
        }
        // request body: fixed length or chunked (then relaid precisely)
        let mut req_failed = false;
        if req_chunked {
            if relay_chunked(&mut cr, &mut uw, &mut buf).await.is_err() {
                req_failed = true;
            }
        } else if req_len > leftover.len() as u64 {
            let still = req_len - leftover.len() as u64;
            let got = relay_exact(&mut cr, &mut uw, still, &mut buf).await.unwrap_or(0);
            req_bytes += got;
            req_failed = got < still;
        }
        if req_failed {
            record_exit(id, 0, started, req_bytes, 0, "client request truncated");
            break;
        }

        // ── response head ──
        let (rhead, rleftover) = match read_head(&mut ur, &mut buf).await {
            Ok(h) => h,
            Err(_) => {
                record_exit(id, 0, started, req_bytes, 0, "no response (upstream closed)");
                break;
            }
        };
        let (status, resp_len, resp_chunked, upgrade) = parse_response(&rhead);
        let mut resp_bytes = (rhead.len() + rleftover.len()) as u64;
        let down_ok = cw.write_all(&rhead).await.is_ok()
            && (rleftover.is_empty() || cw.write_all(&rleftover).await.is_ok());
        if !down_ok {
            record_exit(id, status, started, req_bytes, resp_bytes, "client write failed");
            break;
        }

        if upgrade {
            // 101 Switching Protocols: tunnel opaque both ways, log exit.
            record_exit(id, status, started, req_bytes, resp_bytes, "tunneled (protocol upgrade)");
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
                let got = relay_exact(&mut ur, &mut cw, still, &mut buf).await.unwrap_or(0);
                resp_bytes += got;
                body_failed = got < still;
            }
        } else {
            // HTTP/1.0-style: body ends when the upstream closes.
            let got = relay_until_eof(&mut ur, &mut cw, &mut buf).await;
            resp_bytes += got;
        }
        record_exit(id, status, started, req_bytes, resp_bytes, if body_failed { "response truncated" } else { "" });
        if body_failed {
            break;
        }
    }
}

fn record_exit(id: u64, status: u16, started: Instant, req_bytes: u64, resp_bytes: u64, note: &str) {
    session::record(DsdEvent::HttpExit {
        id,
        ts: std::time::SystemTime::now(),
        status,
        ms: started.elapsed().as_secs_f64() * 1000.0,
        req_bytes,
        resp_bytes,
        note: note.to_string(),
    });
}

/// Read until the CRLFCRLF head terminator. Returns the head bytes plus any
/// bytes already read BEYOND the head (they belong to the body and must be
/// forwarded to the other side before relaying the rest).
async fn read_head<R: tokio::io::AsyncRead + Unpin>(
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
    head.lines()
        .skip(1)
        .find_map(|l| {
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
    let len = header_value(&text, "content-length").and_then(|v| v.parse().ok()).unwrap_or(0);
    let chunked = header_value(&text, "transfer-encoding")
        .map(|v| v.to_ascii_lowercase().contains("chunked"))
        .unwrap_or(false);
    (method, path, host, len, chunked)
}

type RespHead = (u16, Option<u64>, bool, bool);
fn parse_response(head: &[u8]) -> RespHead {
    let text = String::from_utf8_lossy(head);
    let first = text.lines().next().unwrap_or("");
    // "HTTP/1.1 200 OK"
    let status = first.split_whitespace().nth(1).and_then(|s| s.parse().ok()).unwrap_or(0);
    let len = header_value(&text, "content-length").and_then(|v| v.trim().parse().ok());
    let chunked = header_value(&text, "transfer-encoding")
        .map(|v| v.to_ascii_lowercase().contains("chunked"))
        .unwrap_or(false);
    let upgrade = header_value(&text, "upgrade").is_some() && status == 101;
    (status, len, chunked, upgrade)
}

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

/// Relay until EOF, byte counting only (HTTP/1.0 close-delimited bodies).
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
        // size line, read and forwarded verbatim (bytes incl. CRLF)
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
        let size = usize::from_str_radix(text.trim().split(';').next().unwrap_or("0").trim(), 16)
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
        // trailing CRLF after each chunk
        let mut crlf = [0u8; 2];
        r.read_exact(&mut crlf).await.map_err(|_| ())?;
        w.write_all(&crlf).await.map_err(|_| ())?;
    }
}

/// Opaque bidirectional tunnel (websocket upgrade): client-read → upstream-
/// write and upstream-read → client-write race until either side closes.
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
