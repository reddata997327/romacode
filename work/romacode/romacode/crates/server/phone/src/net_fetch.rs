//! Background page loads for the browser. Runs on its own thread so the
//! compositor never blocks on the network; the result lands in the shared
//! event inbox that the compositor drains.

use std::net::ToSocketAddrs;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use crate::state::OsEvent;

type Inbox = Arc<Mutex<std::collections::VecDeque<OsEvent>>>;

/// SSRF guard: reject URLs whose host resolves to a private / loopback /
/// link-local address. The phone runs untrusted browser navigations, so a
/// malicious page could otherwise pull the host into fetching
/// `http://169.254.169.254/...` (cloud metadata) or
/// `http://127.0.0.1:6379/...` (local services).
///
/// Carve-out: `localhost` and `127.0.0.1` are permitted *only* when they
/// appear literally as the URL host, so the phone's own
/// `http://localhost:<port>` test fetches still work.
fn ssrf_blocked(url: &str) -> bool {
    // Parse minimally without pulling in a URL crate: split off scheme, then
    // authority, then strip port and userinfo. This is intentionally strict —
    // any URL we cannot confidently decompose is rejected.
    let Some(rest) = url
        .split("://")
        .nth(1)
        .and_then(|a| a.split(['/', '?', '#']).next())
    else {
        return true;
    };
    // Strip userinfo: `user:pass@host:port` → `host:port`.
    let host_port = rest.rsplit('@').next().unwrap_or(rest);
    // Strip brackets and port from `[ipv6]:port` / `host:port`.
    let (host, _port) = match host_port.rsplit_once(':') {
        Some((h, p)) if !h.starts_with('[') => (h, Some(p)),
        // IPv6 literal `[::1]:port` or `[::1]`.
        _ if host_port.starts_with('[') => {
            let inner = host_port
                .trim_start_matches('[')
                .split(']')
                .next()
                .unwrap_or("");
            (inner, None)
        }
        _ => (host_port, None),
    };
    let host = host.trim();
    if host.is_empty() {
        return true;
    }
    // Explicit-localhost carve-out.
    let is_explicit_loopback =
        host.eq_ignore_ascii_case("localhost") || host == "127.0.0.1" || host == "::1";
    // Resolve and inspect every returned address.
    let addrs: Vec<_> = (host, 0u16).to_socket_addrs().map(|it| it.collect()).unwrap_or_default();
    if addrs.is_empty() {
        // No DNS resolution: refuse. This also blocks raw IPs that fail to
        // parse (shouldn't happen for valid IPs, but fail closed).
        return !is_explicit_loopback;
    }
    for sa in addrs {
        let ip = sa.ip();
        let private = match ip {
            std::net::IpAddr::V4(v4) => {
                v4.is_loopback()
                    || v4.is_private()
                    || v4.is_link_local()
                    || v4.is_unspecified()
                    // 100.64.0.0/10 — CGNAT, often reachable in cloud envs.
                    || (v4.octets()[0] == 100
                        && (v4.octets()[1] & 0xc0) == 0x40)
            }
            std::net::IpAddr::V6(v6) => {
                v6.is_loopback()
                    || v6.is_unspecified()
                    || (v6.segments()[0] & 0xfe00) == 0xfc00 // ULA fc00::/7
                    || (v6.segments()[0] & 0xffc0) == 0xfe80 // link-local fe80::/10
            }
        };
        if private && !is_explicit_loopback {
            return true;
        }
        if private && is_explicit_loopback {
            // localhost resolves to 127.0.0.1 — that's the carve-out.
            continue;
        }
    }
    false
}

/// Kick off a real fetch of `url` (rustls TLS through the engine's native
/// HTTP stack — no curl, no system browser).
pub fn spawn_page_load(inbox: Inbox, data_dir: PathBuf, url: String) {
    std::thread::spawn(move || {
        // SSRF guard before any network touch: refuse private / loopback /
        // link-local destinations (cloud metadata, local services, etc.).
        if ssrf_blocked(&url) {
            let ev = OsEvent::BrowserLoaded {
                url,
                ok: false,
                detail: "blocked by SSRF guard: host resolves to a private/loopback/link-local address".into(),
                body_path: PathBuf::new(),
                status: 0,
            };
            if let Ok(mut q) = inbox.lock() {
                q.push_back(ev);
            }
            return;
        }
        let cache = data_dir.join("cache");
        let _ = std::fs::create_dir_all(&cache);
        let dest = cache.join("page.html");
        let opts = mandroid::net::FetchOptions {
            max_bytes: 4_000_000,
            max_redirects: 5,
            timeout: Duration::from_secs(15),
            headers: vec![(
                "User-Agent".into(),
                "Mozilla/5.0 (RomaCode Phone) RomaCode".into(),
            )],
            expect_content_type: None,
        };
        let ev = match mandroid::net::fetch(&url, &dest, &opts) {
            Ok(res) => {
                let ok = (200..300).contains(&res.status);
                let detail = format!(
                    "HTTP {} · {} bytes · {}",
                    res.status,
                    res.bytes,
                    res.content_type.unwrap_or_default()
                );
                OsEvent::BrowserLoaded {
                    url,
                    ok,
                    detail,
                    body_path: res.path,
                    status: res.status,
                }
            }
            Err(e) => OsEvent::BrowserLoaded {
                url,
                ok: false,
                detail: format!("{e:#}"),
                body_path: PathBuf::new(),
                status: 0,
            },
        };
        if let Ok(mut q) = inbox.lock() {
            q.push_back(ev);
        }
    });
}
