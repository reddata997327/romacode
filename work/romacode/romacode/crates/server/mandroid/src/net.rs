//! Native HTTP(S) fetch — no `curl`, no `reqwest`, no external process.
//!
//! TLS is rustls with the webpki root store, matching the rest of this stack.
//! Used to acquire an APK from a URL the way an agent would be told to.

use std::io::{Read, Write};
use std::net::TcpStream;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use anyhow::{anyhow, bail, Context, Result};
use sha2::{Digest, Sha256};

/// Refuse anything larger than this by default — a runaway download on a
/// metered or hostile link is not a useful outcome.
pub const DEFAULT_MAX_BYTES: u64 = 512 * 1024 * 1024;

#[derive(Debug, Clone)]
pub struct FetchOptions {
    pub max_bytes: u64,
    pub max_redirects: usize,
    pub timeout: Duration,
    /// Extra request headers, e.g. an Authorization or User-Agent override.
    pub headers: Vec<(String, String)>,
    /// Only accept this content type family (substring match, case-insensitive).
    pub expect_content_type: Option<String>,
}

impl Default for FetchOptions {
    fn default() -> Self {
        Self {
            max_bytes: DEFAULT_MAX_BYTES,
            max_redirects: 5,
            timeout: Duration::from_secs(120),
            headers: vec![(
                "User-Agent".into(),
                format!(
                    "mandroid/{} (native android engine)",
                    env!("CARGO_PKG_VERSION")
                ),
            )],
            expect_content_type: None,
        }
    }
}

/// What a completed download produced. Never contains file contents.
#[derive(Debug, Clone)]
pub struct FetchResult {
    pub final_url: String,
    pub status: u16,
    pub content_type: Option<String>,
    pub content_length: Option<u64>,
    pub bytes: u64,
    pub sha256: String,
    pub path: PathBuf,
}

#[derive(Debug)]
struct ParsedUrl {
    https: bool,
    host: String,
    port: u16,
    path: String,
}

fn parse_url(url: &str) -> Result<ParsedUrl> {
    let (https, rest) = if let Some(r) = url.strip_prefix("https://") {
        (true, r)
    } else if let Some(r) = url.strip_prefix("http://") {
        (false, r)
    } else {
        bail!("unsupported URL scheme (expected http:// or https://): {url}");
    };
    // The authority ends at the first '/', '?' or '#'.
    let (authority, path) = match rest.find(['/', '?', '#']) {
        Some(i) if rest[i..].starts_with('/') => (&rest[..i], rest[i..].to_string()),
        Some(i) if rest[i..].starts_with('?') => (&rest[..i], format!("/{}", &rest[i..])),
        Some(i) => (&rest[..i], "/".to_string()), // bare fragment
        None => (rest, "/".to_string()),
    };
    if authority.is_empty() {
        bail!("URL has no host: {url}");
    }
    let (host, port) = match authority.rsplit_once(':') {
        Some((h, p)) if !h.is_empty() && p.chars().all(|c| c.is_ascii_digit()) => {
            (h.to_string(), p.parse::<u16>().context("bad port")?)
        }
        _ => (authority.to_string(), if https { 443 } else { 80 }),
    };
    Ok(ParsedUrl {
        https,
        host,
        port,
        path,
    })
}

/// A `Read + Write` stream that is either plain TCP or TLS over TCP.
enum Stream {
    Plain(TcpStream),
    Tls(Box<rustls::StreamOwned<rustls::ClientConnection, TcpStream>>),
}

impl Read for Stream {
    fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
        match self {
            Stream::Plain(s) => s.read(buf),
            Stream::Tls(s) => s.read(buf),
        }
    }
}

impl Write for Stream {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        match self {
            Stream::Plain(s) => s.write(buf),
            Stream::Tls(s) => s.write(buf),
        }
    }
    fn flush(&mut self) -> std::io::Result<()> {
        match self {
            Stream::Plain(s) => s.flush(),
            Stream::Tls(s) => s.flush(),
        }
    }
}

fn tls_config() -> Arc<rustls::ClientConfig> {
    let roots: rustls::RootCertStore = webpki_roots::TLS_SERVER_ROOTS.iter().cloned().collect();
    Arc::new(
        rustls::ClientConfig::builder()
            .with_root_certificates(roots)
            .with_no_client_auth(),
    )
}

fn connect(u: &ParsedUrl, timeout: Duration) -> Result<Stream> {
    let addr = format!("{}:{}", u.host, u.port);
    let tcp = TcpStream::connect(&addr).with_context(|| format!("connecting to {addr}"))?;
    tcp.set_read_timeout(Some(timeout))?;
    tcp.set_write_timeout(Some(timeout))?;
    tcp.set_nodelay(true).ok();
    if !u.https {
        return Ok(Stream::Plain(tcp));
    }
    let name = rustls_pki_types::ServerName::try_from(u.host.clone())
        .map_err(|_| anyhow!("invalid TLS server name: {}", u.host))?;
    let conn = rustls::ClientConnection::new(tls_config(), name).context("starting TLS session")?;
    Ok(Stream::Tls(Box::new(rustls::StreamOwned::new(conn, tcp))))
}

/// One HTTP/1.1 GET. Header names are echoed in the response struct only as far
/// as we need them; the body is streamed to the caller.
struct HttpResponse {
    status: u16,
    headers: Vec<(String, String)>,
}

impl HttpResponse {
    fn header(&self, name: &str) -> Option<&str> {
        self.headers
            .iter()
            .find(|(k, _)| k.eq_ignore_ascii_case(name))
            .map(|(_, v)| v.as_str())
    }
}

fn send_request(
    stream: &mut Stream,
    u: &ParsedUrl,
    headers: &[(String, String)],
) -> Result<HttpResponse> {
    let mut req = format!(
        "GET {} HTTP/1.1\r\nHost: {}\r\nAccept: */*\r\nAccept-Encoding: identity\r\nConnection: close\r\n",
        u.path, u.host
    );
    for (k, v) in headers {
        req.push_str(&format!("{k}: {v}\r\n"));
    }
    req.push_str("\r\n");
    stream.write_all(req.as_bytes())?;
    stream.flush()?;

    // Read headers, byte at a time up to the blank line (keeps back-pressure
    // sane and avoids swallowing body bytes).
    let mut head = Vec::with_capacity(1024);
    let mut b = [0u8; 1];
    loop {
        let n = stream.read(&mut b)?;
        if n == 0 {
            bail!("connection closed before HTTP headers were complete");
        }
        head.push(b[0]);
        if head.len() >= 4 && &head[head.len() - 4..] == b"\r\n\r\n" {
            break;
        }
        if head.len() > 64 * 1024 {
            bail!("HTTP response headers exceeded 64 KiB");
        }
    }
    let text = String::from_utf8_lossy(&head).to_string();
    let mut lines = text.split("\r\n");
    let status_line = lines.next().unwrap_or("");
    let status = status_line
        .split_whitespace()
        .nth(1)
        .and_then(|s| s.parse::<u16>().ok())
        .ok_or_else(|| anyhow!("malformed HTTP status line: {status_line:?}"))?;
    let mut headers_out = Vec::new();
    for line in lines {
        if line.is_empty() {
            break;
        }
        if let Some((k, v)) = line.split_once(':') {
            headers_out.push((k.trim().to_string(), v.trim().to_string()));
        }
    }
    Ok(HttpResponse {
        status,
        headers: headers_out,
    })
}

/// Body reader that yields at most the fixed content-length (or everything up
/// to EOF when the server closes, which is what `Connection: close` gives us).
struct Body<'a> {
    stream: &'a mut Stream,
    remaining: Option<u64>,
    max_bytes: u64,
    read: u64,
}

impl<'a> Body<'a> {
    fn next_chunk(&mut self) -> Result<Option<Vec<u8>>> {
        let mut buf = vec![0u8; 64 * 1024];
        let want = match self.remaining {
            Some(r) if r == 0 => return Ok(None),
            Some(r) => buf.len().min(r as usize),
            None => buf.len(),
        };
        if want == 0 {
            return Ok(None);
        }
        buf.truncate(want);
        let n = self.stream.read(&mut buf)?;
        if n == 0 {
            return Ok(None);
        }
        buf.truncate(n);
        self.read += n as u64;
        if self.read > self.max_bytes {
            bail!("download exceeded the {} byte limit", self.max_bytes);
        }
        if let Some(r) = self.remaining.as_mut() {
            *r -= n as u64;
        }
        Ok(Some(buf))
    }
}

/// Fetch a URL to a file, following redirects. Returns the digest so the caller
/// can pin/verify what actually arrived.
pub fn fetch(url: &str, dest: &Path, opts: &FetchOptions) -> Result<FetchResult> {
    let mut current = url.to_string();
    for hop in 0..=opts.max_redirects {
        let parsed = parse_url(&current)?;
        let mut stream = connect(&parsed, opts.timeout)?;
        let resp = send_request(&mut stream, &parsed, &opts.headers)?;

        if (300..400).contains(&resp.status) {
            let location = resp
                .header("location")
                .ok_or_else(|| anyhow!("HTTP {} with no Location header", resp.status))?;
            if hop == opts.max_redirects {
                bail!("too many redirects (> {})", opts.max_redirects);
            }
            current = if location.starts_with("http://") || location.starts_with("https://") {
                // Absolute URL — use it verbatim. parse_url on the next loop
                // iteration will reject anything malformed.
                location.to_string()
            } else if location.starts_with("//") {
                // Protocol-relative: `//host/path` → `scheme://host/path`.
                if location.contains('\0') || location.chars().any(|c| c.is_control()) {
                    bail!("relative redirect contains a NUL or control character: {location:?}");
                }
                let scheme = if parsed.https { "https" } else { "http" };
                format!("{scheme}:{location}")
            } else if let Some(stripped) = location.strip_prefix('/') {
                // Server-absolute: `/path` → `scheme://host:port/path`.
                if location.contains('\0') || location.chars().any(|c| c.is_control()) {
                    bail!("relative redirect contains a NUL or control character: {location:?}");
                }
                let scheme = if parsed.https { "https" } else { "http" };
                format!("{scheme}://{}:{}/{stripped}", parsed.host, parsed.port)
            } else {
                // Path-relative: resolve against the directory of the current
                // request URL (RFC 3986 §5.2), honouring `..` and ignoring `.`.
                if location.contains('\0') || location.chars().any(|c| c.is_control()) {
                    bail!("relative redirect contains a NUL or control character: {location:?}");
                }
                let scheme = if parsed.https { "https" } else { "http" };
                // Take everything up to and including the last '/' as the base
                // directory (so `/api/v1/models` becomes `/api/v1/`).
                let base_dir = match parsed.path.rfind('/') {
                    Some(i) => &parsed.path[..=i],
                    None => "/",
                };
                let mut segs: Vec<&str> = base_dir
                    .split('/')
                    .filter(|s| !s.is_empty())
                    .collect();
                for part in location.split('/') {
                    match part {
                        ".." => {
                            segs.pop();
                        }
                        "." | "" => {} // ignore single-dot and empty segments
                        other => segs.push(other),
                    }
                }
                let joined = segs.join("/");
                format!("{scheme}://{}:{}/{joined}", parsed.host, parsed.port)
            };
            continue;
        }
        if resp.status != 200 {
            bail!("HTTP {} fetching {current}", resp.status);
        }

        let content_type = resp.header("content-type").map(|s| s.to_string());
        if let Some(want) = &opts.expect_content_type {
            let ct = content_type
                .clone()
                .unwrap_or_default()
                .to_ascii_lowercase();
            if !ct.contains(&want.to_ascii_lowercase()) {
                bail!("unexpected content type {ct:?} (wanted something like {want:?})");
            }
        }
        let declared = resp
            .header("content-length")
            .and_then(|s| s.parse::<u64>().ok());
        if let Some(len) = declared {
            if len > opts.max_bytes {
                bail!(
                    "server advertises {len} bytes, over the {} byte limit",
                    opts.max_bytes
                );
            }
        }

        if let Some(parent) = dest.parent() {
            std::fs::create_dir_all(parent)
                .with_context(|| format!("creating {}", parent.display()))?;
        }
        let tmp = dest.with_extension("part");
        let mut file =
            std::fs::File::create(&tmp).with_context(|| format!("creating {}", tmp.display()))?;
        let mut hasher = Sha256::new();
        let mut total: u64 = 0;
        let mut body = Body {
            stream: &mut stream,
            remaining: declared,
            max_bytes: opts.max_bytes,
            read: 0,
        };
        while let Some(chunk) = body.next_chunk()? {
            hasher.update(&chunk);
            file.write_all(&chunk)?;
            total += chunk.len() as u64;
        }
        file.flush()?;
        drop(file);
        if let Some(expect) = declared {
            if total != expect {
                let _ = std::fs::remove_file(&tmp);
                bail!("short read: got {total} of {expect} bytes");
            }
        }
        std::fs::rename(&tmp, dest).with_context(|| format!("finalising {}", dest.display()))?;
        return Ok(FetchResult {
            final_url: current,
            status: resp.status,
            content_type,
            content_length: declared,
            bytes: total,
            sha256: hex::encode(hasher.finalize()),
            path: dest.to_path_buf(),
        });
    }
    bail!("internal error: redirect loop exhausted without resolution");
}

pub fn sha256_file(path: &Path) -> Result<String> {
    let mut f = std::fs::File::open(path).with_context(|| format!("opening {}", path.display()))?;
    let mut hasher = Sha256::new();
    let mut buf = vec![0u8; 256 * 1024];
    loop {
        let n = f.read(&mut buf)?;
        if n == 0 {
            break;
        }
        hasher.update(&buf[..n]);
    }
    Ok(hex::encode(hasher.finalize()))
}

/// Cheap sanity check that what we downloaded is the thing we asked for: a ZIP
/// archive (every APK is one) and, when we can see the manifest without
/// decoding, not obviously HTML (a captive portal or error page).
pub fn looks_like_apk(path: &Path) -> Result<bool> {
    let mut f = std::fs::File::open(path)?;
    let mut magic = [0u8; 4];
    if f.read_exact(&mut magic).is_err() {
        return Ok(false);
    }
    Ok(&magic == b"PK\x03\x04")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_urls() {
        // fixture uses the real F-Droid host so the same string is a live,
        // fetchable link (see phone/examples/linkcheck.rs)
        let u = parse_url("https://f-droid.org/F-Droid.apk").unwrap();
        assert!(u.https);
        assert_eq!(u.host, "f-droid.org");
        assert_eq!(u.port, 443);
        assert_eq!(u.path, "/F-Droid.apk");

        let u = parse_url("http://127.0.0.1:8080/a/b?c=d").unwrap();
        assert!(!u.https);
        assert_eq!(u.port, 8080);
        assert_eq!(u.path, "/a/b?c=d");

        let u = parse_url("https://example.com").unwrap();
        assert_eq!(u.path, "/");

        // query directly after the host (no slash) must not leak into the host
        let u = parse_url("https://example.com?x=1").unwrap();
        assert_eq!(u.host, "example.com");
        assert_eq!(u.path, "/?x=1");

        assert!(parse_url("ftp://x/y").is_err());
        assert!(parse_url("https://").is_err());
    }

    #[test]
    fn rejects_bad_scheme_with_a_clear_message() {
        let e = parse_url("file:///etc/passwd").unwrap_err().to_string();
        assert!(e.contains("unsupported URL scheme"));
    }

    #[test]
    fn local_fetch_roundtrip_and_digest() {
        // A tiny plain-HTTP server so the fetch path is exercised without TLS or
        // the network. Proves redirect handling, length accounting and hashing.
        use std::io::Read as _;
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        let body = b"PK\x03\x04fake apk payload".to_vec();
        let body_for_thread = body.clone();
        let handle = std::thread::spawn(move || {
            for _ in 0..2 {
                let (mut sock, _) = listener.accept().unwrap();
                let mut req = [0u8; 2048];
                let _ = sock.read(&mut req);
                let text = String::from_utf8_lossy(&req);
                // First request redirects, second serves the payload.
                let resp = if text.contains("GET /redirect") {
                    "HTTP/1.1 302 Found\r\nLocation: /real.apk\r\nContent-Length: 0\r\nConnection: close\r\n\r\n".to_string()
                } else {
                    format!(
                        "HTTP/1.1 200 OK\r\nContent-Type: application/vnd.android.package-archive\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                        body_for_thread.len()
                    )
                };
                use std::io::Write as _;
                sock.write_all(resp.as_bytes()).unwrap();
                if !text.contains("GET /redirect") {
                    sock.write_all(&body_for_thread).unwrap();
                }
                sock.flush().ok();
            }
        });

        let dir = std::env::temp_dir().join(format!("mandroid-net-test-{port}"));
        std::fs::create_dir_all(&dir).unwrap();
        let dest = dir.join("out.apk");
        let opts = FetchOptions {
            expect_content_type: Some("android.package-archive".into()),
            ..Default::default()
        };
        let r = fetch(&format!("http://127.0.0.1:{port}/redirect"), &dest, &opts).unwrap();
        assert_eq!(r.bytes, body.len() as u64);
        assert_eq!(r.final_url, format!("http://127.0.0.1:{port}/real.apk"));
        assert_eq!(
            r.sha256,
            hex::encode(Sha256::digest(&body)),
            "digest must cover the bytes actually written"
        );
        assert_eq!(std::fs::read(&dest).unwrap(), body);
        assert!(looks_like_apk(&dest).unwrap());
        handle.join().unwrap();
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn refuses_wrong_content_type() {
        use std::io::Read as _;
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        std::thread::spawn(move || {
            let (mut sock, _) = listener.accept().unwrap();
            let mut req = [0u8; 2048];
            let _ = sock.read(&mut req);
            use std::io::Write as _;
            sock.write_all(b"HTTP/1.1 200 OK\r\nContent-Type: text/html\r\nContent-Length: 5\r\nConnection: close\r\n\r\nhello")
                .unwrap();
        });
        let dir = std::env::temp_dir().join(format!("mandroid-net-test-ct-{port}"));
        std::fs::create_dir_all(&dir).unwrap();
        let err = fetch(
            &format!("http://127.0.0.1:{port}/x"),
            &dir.join("x.apk"),
            &FetchOptions {
                expect_content_type: Some("android.package-archive".into()),
                ..Default::default()
            },
        )
        .unwrap_err()
        .to_string();
        assert!(err.contains("content type"), "{err}");
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn enforces_the_size_cap() {
        use std::io::Read as _;
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        std::thread::spawn(move || {
            let (mut sock, _) = listener.accept().unwrap();
            let mut req = [0u8; 2048];
            let _ = sock.read(&mut req);
            use std::io::Write as _;
            sock.write_all(
                b"HTTP/1.1 200 OK\r\nContent-Length: 100000\r\nConnection: close\r\n\r\n",
            )
            .unwrap();
            let junk = vec![b'x'; 100_000];
            sock.write_all(&junk).ok();
        });
        let dir = std::env::temp_dir().join(format!("mandroid-net-test-cap-{port}"));
        std::fs::create_dir_all(&dir).unwrap();
        let err = fetch(
            &format!("http://127.0.0.1:{port}/big"),
            &dir.join("big.apk"),
            &FetchOptions {
                max_bytes: 1024,
                ..Default::default()
            },
        )
        .unwrap_err()
        .to_string();
        assert!(err.contains("limit"), "{err}");
        std::fs::remove_dir_all(&dir).ok();
    }
}
