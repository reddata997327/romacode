//! Live integration test for the `browser` tool against a **real Servo
//! engine**. Skipped unless `ROMACODE_SERVO` points at a servoshell binary
//! that can start headlessly on this machine:
//!
//! ```sh
//! ROMACODE_SERVO=/path/to/servoshell \
//! LD_LIBRARY_PATH=/path/to/engine-libs \
//! cargo test -p romacode-server --test browser_live -- --ignored --nocapture
//! ```
//!
//! The test drives the public tool surface exactly as the agent does —
//! `browser::execute` — against a local HTTP server started in-process,
//! proving the full loop: engine spawn → WebDriver session → navigate to the
//! local server → frontend (DOM + screenshot) → backend (JS-evaluated page
//! state, network log, cookies) → real HTTP round trips to the local server.

use romacode_server::browser;
use romacode_server::config::Config;
use serde_json::json;
use std::io::{Read, Write};
use std::net::TcpListener;
use std::sync::atomic::AtomicBool;
use std::time::Duration;

fn index_html() -> &'static str {
    "<!DOCTYPE html><html><head><title>RomaCode Live Test Page</title>\
     <link rel=\"stylesheet\" href=\"/style.css\"></head><body>\
     <h1 id=\"headline\">RomaCode + Servo live integration</h1>\
     <p id=\"counter\">waiting for JS</p>\
     <script src=\"/app.js\"></script></body></html>"
}

fn app_js() -> &'static str {
    "fetch('/data.json').then(r => r.json()).then(d => { \
        window.ROMACODE_LIVE = { js: true, payload: d, cookie: document.cookie }; \
        document.getElementById('counter').textContent = 'JS ran; payload: ' + d.message; });"
}

/// Minimal threaded HTTP server: index.html + app.js + style.css + data.json,
/// every response carries `Set-Cookie: romacode=live`.
fn spawn_site() -> u16 {
    let l = TcpListener::bind("127.0.0.1:0").expect("bind site port");
    let port = l.local_addr().unwrap().port();
    std::thread::spawn(move || {
        for stream in l.incoming() {
            let mut s = match stream {
                Ok(s) => s,
                Err(_) => continue,
            };
            let mut buf = [0u8; 4096];
            let n = match s.read(&mut buf) {
                Ok(0) | Err(_) => continue,
                Ok(n) => n,
            };
            let req = String::from_utf8_lossy(&buf[..n]).into_owned();
            let Some(path) = req.lines().next().and_then(|l| l.split_whitespace().nth(1)) else {
                continue;
            };
            let (code, ctype, body): (u16, &str, &[u8]) = match path {
                "/" => (200, "text/html; charset=utf-8", index_html().as_bytes()),
                "/app.js" => (200, "application/javascript", app_js().as_bytes()),
                "/style.css" => (200, "text/css", b"h1 { color: purple; }"),
                "/data.json" => (
                    200,
                    "application/json",
                    br#"{"message":"hello from the local backend","n":42}"#,
                ),
                _ => (404, "text/plain", b"not found"),
            };
            let head = format!(
                "HTTP/1.1 {} OK\r\nContent-Type: {ctype}\r\nSet-Cookie: romacode=live; Path=/\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                code,
                body.len()
            );
            let _ = s.write_all(head.as_bytes());
            let _ = s.write_all(body);
        }
    });
    port
}

#[test]
#[ignore = "requires ROMACODE_SERVO pointing at a servoshell binary"]
fn browser_tool_drives_real_servo_end_to_end() {
    let Some(engine) = std::env::var("ROMACODE_SERVO").ok().filter(|p| !p.is_empty()) else {
        panic!("set ROMACODE_SERVO to a servoshell binary to run this test");
    };

    let port = spawn_site();
    let site_url = format!("http://127.0.0.1:{port}/");
    println!("SITE   -> {site_url}");

    let mut cfg = Config::default();
    cfg.browser.binary = engine;
    cfg.browser.host = "127.0.0.1".into();
    cfg.browser.port = 0; // free ephemeral port
    cfg.browser.headless = true;
    cfg.browser.start_timeout_secs = 90;
    cfg.browser.timeout_secs = 120;
    let cancel = AtomicBool::new(false);

    // 1. Engine spawn + WebDriver session (public tool surface).
    let out = browser::execute(&cfg, &json!({"action": "start"}), &cancel).expect("engine start");
    println!("START  -> {out}");
    assert!(out.contains("webdriver"), "start must report the webdriver port");

    // 2. Navigate the real engine to the local server.
    let out = browser::execute(
        &cfg,
        &json!({"action": "navigate", "url": site_url}),
        &cancel,
    )
    .expect("navigate");
    println!("NAV    -> {out}");

    // 3. Frontend: title + url as the engine sees them.
    let title = browser::execute(&cfg, &json!({"action": "title"}), &cancel).expect("title");
    println!("TITLE  -> {title}");
    assert!(title.contains("RomaCode Live Test Page"), "{title}");
    let url = browser::execute(&cfg, &json!({"action": "url"}), &cancel).expect("url");
    println!("URL    -> {url}");
    assert!(url.contains(&format!("127.0.0.1:{port}")), "{url}");

    // 4. Frontend: DOM from the real engine.
    let dom = browser::execute(&cfg, &json!({"action": "dom"}), &cancel).expect("dom");
    println!("DOM    -> {} chars", dom.len());
    assert!(dom.contains("RomaCode + Servo live integration"), "{dom}");

    // 5. Backend: page-level JS state (fetch result + cookie).
    let deadline = std::time::Instant::now() + Duration::from_secs(30);
    let mut live_state = String::new();
    while std::time::Instant::now() < deadline {
        let out = browser::execute(
            &cfg,
            &json!({"action": "eval", "script": "return JSON.stringify(window.ROMACODE_LIVE || null)"}),
            &cancel,
        )
        .expect("eval");
        if out.contains("payload") {
            live_state = out;
            break;
        }
        std::thread::sleep(Duration::from_millis(250));
    }
    println!("EVAL   -> {live_state}");
    assert!(live_state.contains("\"js\":true"), "JS must have executed: {live_state}");
    assert!(
        live_state.contains("hello from the local backend"),
        "page fetch of /data.json must have succeeded: {live_state}"
    );
    assert!(live_state.contains("romacode=live"), "cookie must be visible: {live_state}");

    // 6. Backend: network view from the engine's Performance API.
    let net = browser::execute(&cfg, &json!({"action": "network"}), &cancel).expect("network");
    println!("NET    -> {net}");
    assert!(net.contains("/data.json"), "{net}");
    assert!(net.contains("/app.js"), "{net}");

    // 7. Frontend: screenshot from the real compositor.
    let shot =
        browser::execute(&cfg, &json!({"action": "screenshot"}), &cancel).expect("screenshot");
    println!("SHOT   -> {shot}");
    let path = shot
        .lines()
        .find(|l| l.starts_with("screenshot saved: "))
        .unwrap()
        .trim_start_matches("screenshot saved: ")
        .to_string();
    let bytes = std::fs::read(&path).expect("screenshot file");
    assert_eq!(&bytes[..4], b"\x89PNG", "compositor output must be a PNG");

    // 8. Teardown through the tool.
    let out = browser::execute(&cfg, &json!({"action": "stop"}), &cancel).expect("stop");
    println!("STOP   -> {out}");
    assert!(out.contains("stopped"), "{out}");
}
