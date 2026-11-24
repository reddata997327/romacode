//! Minimal, strict URL handling for the `web_fetch` tool (std only).
//!
//! Strictness is deliberate: the agent fetches URLs chosen by a model that may
//! be steered by untrusted page content, so anything ambiguous is rejected
//! rather than guessed (credentials in the URL, non-http schemes, numeric
//! hosts such as `http://2130706433/`), and every resolved address is checked
//! against private / loopback / link-local ranges (SSRF guard).
use std::fmt;
use std::net::{IpAddr, Ipv4Addr, Ipv6Addr};
use std::time::Duration;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Url {
    pub scheme: String,
    pub host: String,
    pub port: u16,
    /// Path and query, always starting with `/`. Never contains the fragment.
    pub path: String,
    /// `user:pass` userinfo stripped from the authority, if any — passed to
    /// curl as basic auth, never re-embedded in the URL string.
    pub auth: Option<String>,
}

impl Url {
    /// Parse an http(s) URL. A missing scheme means `https://`.
    pub fn parse(raw: &str) -> Result<Url, String> {
        let raw = raw.trim();
        if raw.is_empty() {
            return Err("empty URL".into());
        }
        if raw.chars().any(|c| c.is_control()) {
            return Err("URL contains control characters".into());
        }
        let with_scheme = if raw.contains("://") {
            raw.to_string()
        } else {
            format!("https://{raw}")
        };
        let (scheme, rest) = with_scheme.split_once("://").ok_or("malformed URL")?;
        let scheme = scheme.to_ascii_lowercase();
        if scheme != "http" && scheme != "https" {
            return Err(format!(
                "only http and https URLs are supported, not {scheme}://"
            ));
        }
        let rest = rest.split('#').next().unwrap_or("");
        let end = rest.find(['/', '?']).unwrap_or(rest.len());
        let (authority, tail) = rest.split_at(end);
        // `user:pass@host` is allowed: the userinfo is stripped out and used
        // as basic auth for the fetch instead of being rejected.
        let (userinfo, authority) = match authority.rsplit_once('@') {
            Some((u, a)) if !a.is_empty() => (Some(u.to_string()), a),
            _ => (None, authority),
        };
        let (host, port) = if let Some(inner) = authority.strip_prefix('[') {
            let close = inner.find(']').ok_or("unterminated [ipv6] host")?;
            let host = &inner[..close];
            let after = &inner[close + 1..];
            let port = match after.strip_prefix(':') {
                Some(p) => Some(p),
                None if after.is_empty() => None,
                None => return Err("malformed host".into()),
            };
            if host.parse::<Ipv6Addr>().is_err() {
                return Err(format!("invalid IPv6 address {host:?}"));
            }
            (host.to_string(), port)
        } else {
            match authority.rsplit_once(':') {
                Some((h, p)) => (h.to_string(), Some(p)),
                None => (authority.to_string(), None),
            }
        };
        let host = host.trim_end_matches('.').to_ascii_lowercase();
        if host.is_empty() {
            return Err("URL has no host".into());
        }
        if !host.contains(':')
            && !host
                .chars()
                .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '.' || c == '_')
        {
            return Err(format!("invalid host {host:?}"));
        }
        let default_port = if scheme == "https" { 443 } else { 80 };
        let port = match port {
            None | Some("") => default_port,
            Some(p) => p
                .parse::<u16>()
                .ok()
                .filter(|p| *p != 0)
                .ok_or_else(|| format!("invalid port {p:?}"))?,
        };
        // Numeric-looking hosts (decimal, octal, hex, short forms) are parsed
        // differently by different resolvers — a classic SSRF filter bypass.
        // Real TLDs never start with a digit, so insist on a canonical dotted quad.
        if !host.contains(':') {
            let last = host.rsplit('.').next().unwrap_or("");
            if last.chars().next().map_or(false, |c| c.is_ascii_digit())
                && host.parse::<Ipv4Addr>().is_err()
            {
                return Err(format!("ambiguous numeric host {host:?}; use a normal dotted IPv4 address or a hostname"));
            }
        }
        let mut path = if tail.is_empty() {
            "/".to_string()
        } else if tail.starts_with('?') {
            format!("/{tail}")
        } else {
            tail.to_string()
        };
        path = path.replace(' ', "%20");
        Ok(Url {
            scheme,
            host,
            port,
            path,
            auth: userinfo,
        })
    }

    /// `host` or `[host]` as it appears in a URL authority.
    fn authority(&self) -> String {
        let h = if self.host.contains(':') {
            format!("[{}]", self.host)
        } else {
            self.host.clone()
        };
        let default = if self.scheme == "https" { 443 } else { 80 };
        if self.port == default {
            h
        } else {
            format!("{h}:{}", self.port)
        }
    }

    pub fn is_ip_literal(&self) -> bool {
        self.host.parse::<IpAddr>().is_ok()
    }
}

impl fmt::Display for Url {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}://{}{}", self.scheme, self.authority(), self.path)
    }
}

/// Resolve `href` against `base` like a browser would. `None` for links that
/// cannot be fetched (`#frag`, `mailto:`, `javascript:`, `tel:`, `data:`) or
/// that fail the strict parser.
pub fn join(base: &Url, href: &str) -> Option<Url> {
    let href = href.trim();
    let lower = href.to_ascii_lowercase();
    if href.is_empty()
        || href.starts_with('#')
        || [
            "javascript:",
            "mailto:",
            "tel:",
            "data:",
            "blob:",
            "about:",
            "sms:",
        ]
        .iter()
        .any(|p| lower.starts_with(p))
    {
        return None;
    }
    if lower.starts_with("http://") || lower.starts_with("https://") {
        let mut u = Url::parse(href).ok()?;
        // Inherit the base URL's basic-auth credentials ONLY for the same
        // host. Fetching `https://user:pass@internal-jira/` that redirects to
        // `https://evil.example/` (or a page whose links the model follows)
        // used to send the internal credentials to the external host —
        // browsers strip credentials cross-origin, and so must this join.
        if u.auth.is_none() && u.host == base.host {
            u.auth = base.auth.clone();
        }
        return Some(u);
    }
    if let Some(r) = href.strip_prefix("//") {
        let mut u = Url::parse(&format!("{}://{r}", base.scheme)).ok()?;
        // Same-origin-only credential inheritance (see above).
        if u.auth.is_none() && u.host == base.host {
            u.auth = base.auth.clone();
        }
        return Some(u);
    }
    let href = href.split('#').next().unwrap_or(href);
    let (target, query) = match href.split_once('?') {
        Some((t, q)) => (t, Some(q)),
        None => (href, None),
    };
    let base_path = base.path.split('?').next().unwrap_or("/");
    if target.is_empty() {
        let q = query.map(|q| format!("?{q}")).unwrap_or_default();
        let mut u = Url::parse(&format!(
            "{}://{}{}{}",
            base.scheme,
            base.authority(),
            base_path,
            q
        ))
        .ok()?;
        u.auth = base.auth.clone();
        return Some(u);
    }
    let mut segs: Vec<&str> = if target.starts_with('/') {
        Vec::new()
    } else {
        let dir = base_path.rsplit_once('/').map_or("", |(d, _)| d);
        dir.split('/').filter(|s| !s.is_empty()).collect()
    };
    for part in target.split('/') {
        match part {
            "" | "." => {}
            ".." => {
                segs.pop();
            }
            p => segs.push(p),
        }
    }
    let mut path = format!("/{}", segs.join("/"));
    if target.ends_with('/') && !path.ends_with('/') {
        path.push('/');
    }
    if let Some(q) = query {
        path.push('?');
        path.push_str(q);
    }
    let mut u = Url::parse(&format!("{}://{}{}", base.scheme, base.authority(), path)).ok()?;
    u.auth = base.auth.clone();
    Some(u)
}

/// Resolve `host:port` with a hard deadline.
///
/// `to_socket_addrs` calls getaddrinfo, which has no timeout of its own: one
/// dead resolver per hostname used to stall the calling tool for the OS
/// resolver timeout (and rnmap parsed its targets serially, so a handful of
/// dead names multiplied the stall). The lookup runs on a helper thread; if
/// it has not finished within `timeout`, the caller gets an error and the
/// helper is detached (getaddrinfo cannot be cancelled safely — it completes
/// in the background and the thread exits on its own).
pub fn resolve_hosts(host: &str, port: u16, timeout: Duration) -> Result<Vec<IpAddr>, String> {
    use std::net::ToSocketAddrs;
    let host = host.to_string();
    let label = host.clone();
    let h = std::thread::Builder::new()
        .name("romacode-resolver".into())
        .spawn(move || {
            (host.as_str(), port)
                .to_socket_addrs()
                .map(|it| it.map(|a| a.ip()).collect::<Vec<IpAddr>>())
        })
        .map_err(|e| format!("spawning resolver thread: {e}"))?;
    let deadline = std::time::Instant::now() + timeout;
    loop {
        if h.is_finished() {
            return match h.join() {
                Ok(Ok(addrs)) => Ok(addrs),
                Ok(Err(e)) => Err(format!("could not resolve {label}: {e}")),
                Err(_) => Err(format!("resolver thread panicked for {label}")),
            };
        }
        if std::time::Instant::now() >= deadline {
            std::mem::forget(h); // detach; the lookup finishes in the background
            return Err(format!(
                "could not resolve {label} within {}s (resolver timed out)",
                timeout.as_secs()
            ));
        }
        std::thread::sleep(std::time::Duration::from_millis(20));
    }
}

/// True for every address a public web fetch must never reach: loopback,
/// RFC 1918, link-local (cloud metadata at 169.254.169.254), CGNAT, unique-local,
/// multicast, unspecified, benchmarking and reserved ranges, including IPv4
/// addresses smuggled inside IPv6 (mapped, compatible, NAT64).
pub fn ip_is_blocked(ip: IpAddr) -> bool {
    match ip {
        IpAddr::V4(v4) => v4_blocked(v4),
        IpAddr::V6(v6) => {
            if v6.is_loopback() || v6.is_unspecified() {
                return true;
            }
            let s = v6.segments();
            if s[0] & 0xfe00 == 0xfc00 || s[0] & 0xffc0 == 0xfe80 || s[0] & 0xff00 == 0xff00 {
                return true; // unique-local, link-local, multicast
            }
            if let Some(m) = v6.to_ipv4_mapped() {
                return v4_blocked(m);
            }
            // IPv4-compatible (::a.b.c.d) and NAT64 (64:ff9b::/96): judge the embedded v4
            if s[..6].iter().all(|x| *x == 0)
                || (s[0] == 0x64 && s[1] == 0xff9b && s[2..6].iter().all(|x| *x == 0))
            {
                let v4 =
                    Ipv4Addr::new((s[6] >> 8) as u8, s[6] as u8, (s[7] >> 8) as u8, s[7] as u8);
                return v4_blocked(v4);
            }
            // 6to4 (2002::/16) embeds a v4 in bits 16..48
            if s[0] == 0x2002 {
                let v4 =
                    Ipv4Addr::new((s[1] >> 8) as u8, s[1] as u8, (s[2] >> 8) as u8, s[2] as u8);
                return v4_blocked(v4);
            }
            false
        }
    }
}

fn v4_blocked(ip: Ipv4Addr) -> bool {
    let [a, b, c, _] = ip.octets();
    a == 0
        || a == 10
        || a == 127
        || (a == 100 && (64..=127).contains(&b))
        || (a == 169 && b == 254)
        || (a == 172 && (16..=31).contains(&b))
        || (a == 192 && b == 168)
        || (a == 192 && b == 0 && c == 0)
        || (a == 198 && (b == 18 || b == 19))
        || a >= 224
}

/// Does `host` equal `domain` or end in `.domain`? (`domain` may have a leading dot.)
pub fn host_matches(host: &str, domain: &str) -> bool {
    let d = domain
        .trim()
        .trim_start_matches('.')
        .trim_start_matches("*.")
        .to_ascii_lowercase();
    !d.is_empty() && (host == d || host.ends_with(&format!(".{d}")))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn p(s: &str) -> Url {
        Url::parse(s).unwrap()
    }

    #[test]
    fn parses_and_normalises() {
        assert_eq!(p("Example.COM").to_string(), "https://example.com/");
        assert_eq!(
            p("http://a.b:8080/x y?q=1#frag").to_string(),
            "http://a.b:8080/x%20y?q=1"
        );
        assert_eq!(p("https://a.b?x=1").path, "/?x=1");
        assert_eq!(p("https://a.b:443/").to_string(), "https://a.b/");
        assert_eq!(p("http://[::1]:3000/").host, "::1");
        assert_eq!(p("http://[::1]:3000/").to_string(), "http://[::1]:3000/");
        assert_eq!(p("https://example.com./").host, "example.com");
    }

    #[test]
    fn rejects_risky_urls() {
        for bad in [
            "",
            "ftp://a.b/",
            "file:///etc/passwd",
            "https://a.b:0/",
            "https://a.b:99999/",
            "http://2130706433/",
            "http://0x7f.0.0.1/",
            "http://127.1/",
            "http://017700000001/",
            "https://a b.com/",
            "https://a.b/\r\nHost: x",
            "http://[::1/",
            "https:///path",
        ] {
            assert!(Url::parse(bad).is_err(), "should reject {bad:?}");
        }
        assert!(Url::parse("http://127.0.0.1/").is_ok()); // canonical literals parse; blocking is a separate check
    }

    /// URL userinfo is no longer rejected: it is stripped from the authority
    /// and carried as basic auth.
    #[test]
    fn url_userinfo_becomes_basic_auth() {
        let u = Url::parse("https://user:pw@a.b/seg?q=1").unwrap();
        assert_eq!(u.host, "a.b");
        assert_eq!(u.auth.as_deref(), Some("user:pw"));
        assert_eq!(u.to_string(), "https://a.b/seg?q=1");
        // a bare host still parses to auth = None
        assert_eq!(p("https://a.b/").auth, None);
    }

    #[test]
    fn join_matches_browser_behaviour() {
        let b = p("https://en.example.org/wiki/Foo/Bar?x=1#top");
        let j = |h: &str| join(&b, h).map(|u| u.to_string());
        assert_eq!(j("/a/B").as_deref(), Some("https://en.example.org/a/B"));
        assert_eq!(
            j("Baz").as_deref(),
            Some("https://en.example.org/wiki/Foo/Baz")
        );
        assert_eq!(
            j("../Qux").as_deref(),
            Some("https://en.example.org/wiki/Qux")
        );
        assert_eq!(
            j("./x/").as_deref(),
            Some("https://en.example.org/wiki/Foo/x/")
        );
        assert_eq!(j("//cdn.z.com/a").as_deref(), Some("https://cdn.z.com/a"));
        assert_eq!(
            j("?q=2").as_deref(),
            Some("https://en.example.org/wiki/Foo/Bar?q=2")
        );
        assert_eq!(
            j("p?a=1#f").as_deref(),
            Some("https://en.example.org/wiki/Foo/p?a=1")
        );
        assert_eq!(
            j("https://other.net/x").as_deref(),
            Some("https://other.net/x")
        );
        for none in [
            "#sec",
            "mailto:a@b.c",
            "javascript:void(0)",
            "tel:1",
            "data:text/plain,x",
            "",
        ] {
            assert_eq!(j(none), None, "{none}");
        }
        // too many ../ cannot escape the root
        assert_eq!(
            j("../../../../z").as_deref(),
            Some("https://en.example.org/z")
        );
    }

    #[test]
    fn blocks_internal_addresses() {
        for ip in [
            "127.0.0.1",
            "10.1.2.3",
            "172.16.0.1",
            "172.31.255.255",
            "192.168.1.1",
            "169.254.169.254",
            "100.64.0.1",
            "0.0.0.0",
            "224.0.0.1",
            "255.255.255.255",
            "198.18.0.1",
            "::1",
            "::",
            "fc00::1",
            "fd12:3456::1",
            "fe80::1",
            "ff02::1",
            "::ffff:127.0.0.1",
            "::ffff:10.0.0.1",
            "::127.0.0.1",
            "64:ff9b::7f00:1",
            "2002:7f00:1::1",
        ] {
            assert!(ip_is_blocked(ip.parse().unwrap()), "{ip} should be blocked");
        }
        for ip in [
            "8.8.8.8",
            "1.1.1.1",
            "172.15.0.1",
            "172.32.0.1",
            "93.184.216.34",
            "2606:4700:4700::1111",
            "::ffff:8.8.8.8",
        ] {
            assert!(
                !ip_is_blocked(ip.parse().unwrap()),
                "{ip} should be allowed"
            );
        }
    }

    #[test]
    fn domain_matching() {
        assert!(host_matches("docs.rs", "docs.rs"));
        assert!(host_matches("a.docs.rs", "docs.rs"));
        assert!(host_matches("a.docs.rs", "*.docs.rs"));
        assert!(!host_matches("evildocs.rs", "docs.rs"));
        assert!(!host_matches("docs.rs.evil.com", "docs.rs"));
        assert!(!host_matches("x", ""));
    }
}
