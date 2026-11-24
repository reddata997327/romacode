//! Smoke test over the real tool surface: the dsd tools as registered in
//! `all_tools()` — start a tap against a fake upstream, watch the timeline,
//! stop the session — all through `Tool::execute` with JSON inputs.

use rc_core::tool::{Tool, ToolContext, ToolArc};
use rc_builtin::all_tools;
use serde_json::json;
use std::time::Duration;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};

fn find(names: &[ToolArc], name: &str) -> ToolArc {
    names.iter().find(|t| t.name() == name).cloned().unwrap_or_else(|| panic!("tool {name} not registered"))
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn dsd_tools_work_end_to_end_via_registry() {
    // fake upstream web server
    let l = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let upstream_port = l.local_addr().unwrap().port();
    tokio::spawn(async move {
        loop {
            let Ok((mut s, _)) = l.accept().await else { break };
            tokio::spawn(async move {
                let mut buf = [0u8; 4096];
                let _ = s.read(&mut buf).await;
                let body = "{\"ok\":true}";
                let resp = format!(
                    "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                    body.len(),
                    body
                );
                let _ = s.write_all(resp.as_bytes()).await;
            });
        }
    });

    let tools = all_tools();
    assert_eq!(tools.len(), 42, "builtin count includes the 4 dsd tools");
    let ctx = ToolContext::default();

    // dsd.watch with nothing attached → helpful message, no panic
    let watch = find(&tools, "dsd.watch");
    let out = watch.execute(&json!({"duration_ms": 100}), &ctx).await.unwrap();
    assert!(out.as_str().unwrap().contains("nothing is attached"));

    // dsd.proxy_start → binds an ephemeral tap
    let proxy = find(&tools, "dsd.proxy_start");
    let out = proxy
        .execute(&json!({"listen_port": 0, "upstream": format!("127.0.0.1:{upstream_port}")}), &ctx)
        .await
        .unwrap();
    let text = out.as_str().unwrap();
    assert!(text.contains("dsd tap live"), "{text}");

    // find the bound tap port from the session observers
    let tap_port = rc_dsd::session::session().lock().unwrap().taps[0].tap_port;

    // real request through the tap
    let mut c = TcpStream::connect(("127.0.0.1", tap_port)).await.unwrap();
    c.write_all(b"GET /api/widgets HTTP/1.1\r\nHost: shop.dev\r\nContent-Length: 0\r\nConnection: close\r\n\r\n").await.unwrap();
    let mut resp = Vec::new();
    let _ = tokio::time::timeout(Duration::from_secs(5), c.read_to_end(&mut resp)).await;

    // dsd.watch → timeline with the call, correlated
    tokio::time::sleep(Duration::from_millis(300)).await;
    let out = watch.execute(&json!({"duration_ms": 200}), &ctx).await.unwrap();
    let text = out.as_str().unwrap();
    assert!(text.contains("GET"), "timeline shows method: {text}");
    assert!(text.contains("/api/widgets"), "timeline shows path: {text}");
    assert!(text.contains("200"), "timeline shows exit status: {text}");
    assert!(text.contains("tap :"), "observer header present: {text}");

    // dsd.stop → releases everything
    let stop = find(&tools, "dsd.stop");
    let out = stop.execute(&json!({}), &ctx).await.unwrap();
    assert!(out.as_str().unwrap().contains("stopped"));
    assert!(rc_dsd::session::session().lock().unwrap().taps.is_empty());
}
