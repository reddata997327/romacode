//! Link verification for every URL referenced anywhere in the RomaCode
//! codebase, run through the engine's own native HTTP(S) stack (rustls TLS,
//! no curl, no system browser) — the same path the phone's Chrome app uses.
//!
//! ```text
//! cargo run --release -p phone --example linkcheck
//! ```
//!
//! Each referenced link is fetched and checked for a live HTTP status. XML
//! namespace identifiers (`http://schemas.android.com/...`) are spec-correct
//! as unfetchable: they are names, not endpoints. Loopback URLs
//! (`127.0.0.1:PORT`) are the in-process adbd — verified by the runtime
//! battery (`romacode --verify-android`), not by this program.

use std::time::Duration;

use mandroid::net::{fetch, FetchOptions};

/// Every URL referenced in the sources, with why it exists.
const LINKS: &[(&str, &str)] = &[
    // src/config.rs default `api_base` — the agent's streaming endpoint
    (
        "https://api.anthropic.com",
        "AI agent API base (src/config.rs)",
    ),
    // mandroid/src/dex.rs test fixture string
    ("https://example.com", "dex.rs redaction test fixture"),
    // mandroid/src/net.rs unit-test fixture
    (
        "https://f-droid.org/F-Droid.apk",
        "net.rs parse_url test fixture",
    ),
    // phone/src/apps.rs Chrome start-page quick links and search fallback
    ("https://example.com", "Chrome quick link"),
    ("https://wikipedia.org", "Chrome quick link"),
    ("https://example.org", "Chrome quick link"),
    (
        "https://html.duckduckgo.com/html/?q=romacode",
        "Chrome address-bar search fallback",
    ),
];

fn main() {
    println!("RomaCode link check — native rustls stack, no curl\n");
    let mut pass = 0usize;
    let mut fail = 0usize;
    for (url, why) in LINKS {
        let opts = FetchOptions {
            max_bytes: 256 * 1024,
            max_redirects: 5,
            timeout: Duration::from_secs(20),
            headers: vec![(
                "User-Agent".into(),
                format!("romacode-linkcheck/{}", env!("CARGO_PKG_VERSION")),
            )],
            expect_content_type: None,
        };
        let tmp =
            std::env::temp_dir().join(format!("romacode-linkcheck-{}.bin", std::process::id()));
        // Classify: 200 = live; any HTTP status (including 401/403 on an API
        // root without credentials, or 404 on a fixture path) = the host
        // answered over TLS → link is live. DNS/TLS/transport errors = dead.
        let verdict = match fetch(url, &tmp, &opts) {
            Ok(res) => {
                let _ = std::fs::remove_file(&tmp);
                format!(
                    "PASS  HTTP {} · {} bytes · {}",
                    res.status,
                    res.bytes,
                    res.content_type.unwrap_or_else(|| "-".into())
                )
            }
            Err(e) => {
                let _ = std::fs::remove_file(&tmp);
                let msg = format!("{e:#}");
                // "HTTP <status> fetching …" — the host answered; an API root
                // refusing unauthenticated requests is still a live link
                let answered = msg.starts_with("HTTP ")
                    // real response headers arrived (content-length) before the
                    // checker's byte cap aborted the body download
                    || msg.starts_with("server advertises ");
                if answered {
                    format!("PASS  (host answered: {msg})")
                } else {
                    format!("FAIL  {msg}")
                }
            }
        };
        let ok = verdict.starts_with("PASS");
        pass += ok as usize;
        fail += (!ok) as usize;
        println!("{verdict}\n      {url}\n      ({why})\n");
    }

    // documented non-fetchable identifiers
    println!(
        "SKIP  http://schemas.android.com/apk/res/android\n      XML namespace identifier (axml.rs/axmlenc.rs) — a name, not an endpoint\n"
    );
    println!(
        "SKIP  http://127.0.0.1:<port>\n      in-process adbd loopback — exercised end-to-end by `romacode --verify-android`\n"
    );

    println!("{pass} passed, {fail} failed, 2 skipped (by design)");
    if fail > 0 {
        std::process::exit(1);
    }
}
