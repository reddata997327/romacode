//! End-to-end tap test: a fake upstream web server, a real request through
//! the tap, and assertions that ENTRY (method/path/host) and EXIT (status,
//! bytes) landed in the DSD timeline.

use std::time::Duration;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};

use rc_dsd::session::{self, DsdEvent};
use rc_dsd::tap;

/// One-shot upstream: reads the request head, replies 200 + tiny JSON body.
async fn fake_upstream() -> u16 {
    let l = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = l.local_addr().unwrap().port();
    tokio::spawn(async move {
        loop {
            let Ok((mut s, _)) = l.accept().await else { break };
            tokio::spawn(async move {
                let mut buf = [0u8; 4096];
                let mut got = 0usize;
                // read head
                loop {
                    let n = s.read(&mut buf[got..]).await.unwrap_or(0);
                    if n == 0 {
                        return;
                    }
                    got += n;
                    if buf[..got].windows(4).any(|w| w == b"\r\n\r\n") {
                        break;
                    }
                }
                let head = String::from_utf8_lossy(&buf[..got]).to_string();
                // consume a request body if content-length says so
                let clen: usize = head
                    .lines()
                    .find_map(|l| {
                        let (k, v) = l.split_once(':')?;
                        if k.trim().eq_ignore_ascii_case("content-length") {
                            v.trim().parse().ok()
                        } else {
                            None
                        }
                    })
                    .unwrap_or(0);
                let head_end = buf[..got].windows(4).position(|w| w == b"\r\n\r\n").unwrap() + 4;
                let mut body_read = got - head_end;
                while body_read < clen {
                    let n = s.read(&mut buf[got..]).await.unwrap_or(0);
                    if n == 0 {
                        break;
                    }
                    got += n;
                    body_read += n;
                }
                let body = if head.starts_with("POST") { "seen-body" } else { "{\"ok\":true}" };
                let resp = format!("HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}", body.len(), body);
                let _ = s.write_all(resp.as_bytes()).await;
            });
        }
    });
    port
}

async fn tap_events_for(tap_port: u16) -> Vec<DsdEvent> {
    // wait for the tap to flush its events, then snapshot and filter
    for _ in 0..60 {
        tokio::time::sleep(Duration::from_millis(50)).await;
        let events = session::session().lock().unwrap().events.clone();
        let ids: Vec<u64> = events
            .iter()
            .filter_map(|e| match e {
                DsdEvent::HttpEntry { id, tap_port: p, .. } if *p == tap_port => Some(*id),
                _ => None,
            })
            .collect();
        let have_exit = ids.iter().any(|id| {
            events.iter().any(|e| matches!(e, DsdEvent::HttpExit { id: x, .. } if x == id))
        });
        if have_exit {
            return events
                .into_iter()
                .filter(|e| match e {
                    DsdEvent::HttpEntry { tap_port: p, .. } => *p == tap_port,
                    DsdEvent::HttpExit { id, .. } => ids.contains(id),
                    _ => false,
                })
                .collect();
        }
    }
    Vec::new()
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn tap_records_entry_and_exit() {
    let up = fake_upstream().await;
    let handle = tap::start(0, "127.0.0.1", up, false).await.unwrap();
    let tport = handle.tap_port;

    let mut c = TcpStream::connect(("127.0.0.1", tport)).await.unwrap();
    c.write_all(b"GET /api/users HTTP/1.1\r\nHost: shop.dev\r\nContent-Length: 0\r\nConnection: close\r\n\r\n").await.unwrap();
    let mut resp = Vec::new();
    let _ = tokio::time::timeout(Duration::from_secs(5), c.read_to_end(&mut resp)).await;
    let text = String::from_utf8_lossy(&resp).to_string();
    assert!(text.contains("200 OK"), "client saw: {text:?}");
    assert!(text.contains("{\"ok\":true}"), "body relayed: {text:?}");

    let events = tap_events_for(tport).await;
    let entry = events.iter().find_map(|e| match e {
        DsdEvent::HttpEntry { method, path, host, .. } => Some((method.clone(), path.clone(), host.clone())),
        _ => None,
    });
    assert_eq!(
        entry,
        Some(("GET".to_string(), "/api/users".to_string(), "shop.dev".to_string())),
        "entry parsed: {events:?}"
    );
    let exit = events.iter().find_map(|e| match e {
        DsdEvent::HttpExit { status, resp_bytes, .. } => Some((*status, *resp_bytes)),
        _ => None,
    });
    assert_eq!(exit.map(|(s, _)| s), Some(200));
    assert!(exit.map(|(_, b)| b > 0).unwrap_or(false), "exit bytes counted");
    let _ = up;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn tap_relay_post_body_and_count_bytes() {
    let up = fake_upstream().await;
    let handle = tap::start(0, "127.0.0.1", up, false).await.unwrap();
    let tport = handle.tap_port;

    let mut c = TcpStream::connect(("127.0.0.1", tport)).await.unwrap();
    c.write_all(b"POST /api/orders HTTP/1.1\r\nHost: shop.dev\r\nContent-Length: 5\r\nConnection: close\r\n\r\nhello").await.unwrap();
    let mut resp = Vec::new();
    let _ = tokio::time::timeout(Duration::from_secs(5), c.read_to_end(&mut resp)).await;
    let text = String::from_utf8_lossy(&resp).to_string();
    assert!(text.contains("seen-body"), "upstream got the body through the tap: {text:?}");

    let events = tap_events_for(tport).await;
    let exit = events.iter().find_map(|e| match e {
        DsdEvent::HttpExit { status, req_bytes, .. } => Some((*status, *req_bytes)),
        _ => None,
    });
    assert_eq!(exit.map(|(s, _)| s), Some(200));
    // head (73 bytes) + body (5) must all be counted
    let req = exit.map(|(_, b)| b).unwrap_or(0);
    assert!(req >= 78, "request bytes include head+body: {req}");
}
