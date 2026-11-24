//! Chisel client.
//!
//! - Connects to the server URL (ws://host:port, with optional ?token=).
//! - For each *forward* remote, listens on local_host:local_port; on
//!   accept, sends Open to the server which dials remote_host:remote_port.
//! - For *reverse* remotes, registers them with the server so it knows
//!   to dial back to this client when a TCP conn arrives on the server.
//! - Handles Open messages from the server (reverse case) by dialing
//!   the requested target locally and piping.

use std::collections::HashMap;
use std::net::{TcpListener, TcpStream};
use std::time::Duration;

use crate::log;
use crate::mux::{self, AddrTarget, Mux};
use crate::remote::Remote;
use crate::ws;

pub struct ClientConfig {
    pub server: String,           // host:port
    pub auth: Option<String>,
    pub headers: HashMap<String, String>,
    pub remotes: Vec<Remote>,
    pub keepalive: u64,
    pub max_retry: u64,
}

pub fn run(cfg: ClientConfig) -> std::io::Result<()> {
    // max_retry = number of reconnects after the first failure; 0 means
    // no retry (the embedded server transport relies on that). Any
    // nonzero value must stop counting once the budget is used up.
    let mut attempts: u64 = 0;
    loop {
        match connect_once(&cfg) {
            Ok(()) => {
                log::info("client", "connection ended normally");
                break;
            }
            Err(e) => {
                log::err("client", &format!("connect_once: {}", e));
                if cfg.max_retry == 0 || attempts >= cfg.max_retry {
                    return Err(e);
                }
                attempts += 1;
                let backoff = std::cmp::min(30, 1 + (attempts % 30));
                log::info("client", &format!("retrying in {}s", backoff));
                std::thread::sleep(Duration::from_secs(backoff));
            }
        }
    }
    Ok(())
}

fn connect_once(cfg: &ClientConfig) -> std::io::Result<()> {
    let keepalive = cfg.keepalive;
    let mut sock = TcpStream::connect(&cfg.server)?;
    let _ = sock.set_nodelay(true);
    let (host, port) = split_host_port(&cfg.server);
    ws::client_handshake(&mut sock, host, port, cfg.auth.as_deref(), &cfg.headers)
        .map_err(|e| std::io::Error::new(std::io::ErrorKind::Other, e))?;
    log::info("client", &format!("connected to {}", cfg.server));

    let mux = Mux::new(sock, true, "client".into());

    // Send Setup message for any reverse remotes we want the server to
    // listen on (and forward back to us via Open).
    let rev_remotes: Vec<mux::SetupRemote> = cfg
        .remotes
        .iter()
        .filter(|r| r.reverse && r.proto == crate::remote::Proto::Tcp)
        .map(|r| mux::SetupRemote {
            local_host: r.local_host.clone(),
            local_port: r.local_port,
            remote_host: r.remote_host.clone(),
            remote_port: r.remote_port,
            proto: 0,
        })
        .collect();
    if !rev_remotes.is_empty() {
        if let Err(e) = mux.send_setup(&rev_remotes) {
            log::err("client", &format!("Setup send failed: {}", e));
        }
    }

    // Set up forward remote listeners.
    for r in &cfg.remotes {
        if r.reverse {
            // Reverse: server will initiate Open messages to us.
            // (We just need to make sure the server knows it's allowed to —
            // that's enforced by the server config, not by the client.)
            continue;
        }
        let mux = mux.clone();
        let r = r.clone();
        let listen_addr = format!("{}:{}", r.local_host, r.local_port);
        std::thread::spawn(move || {
            let listener = match TcpListener::bind(&listen_addr) {
                Ok(l) => l,
                Err(e) => {
                    log::err("client", &format!("forward bind {} failed: {}", listen_addr, e));
                    return;
                }
            };
            log::info("client", &format!("forward listening on {} -> {}:{}", listen_addr, r.remote_host, r.remote_port));
            for sock in listener.incoming() {
                let sock = match sock { Ok(s) => s, Err(_) => continue };
                if mux.is_closed() { break; }
                let target = AddrTarget {
                    host: r.remote_host.clone(),
                    port: r.remote_port,
                };
                mux.open_session(target, sock);
            }
        });
    }

    // Keepalive pinger.
    if keepalive > 0 {
        let m = mux.clone();
        let ka = keepalive;
        std::thread::spawn(move || loop {
            std::thread::sleep(Duration::from_secs(ka));
            if m.is_closed() { break; }
            if let Err(e) = m.send_ping(b"ka") {
                log::debug("client", &format!("ping err: {}", e));
                break;
            }
        });
    }

    // Run the mux loop. On Open from the server (reverse remote), the
    // client dials the requested target. The client never receives a
    // Setup message (it sends one), so on_setup is a no-op.
    let m = mux.clone();
    m.run_loop(
        move |target, id| {
            log::info("client", &format!("reverse session {} -> dial {}:{}", id, target.host, target.port));
            match mux::dial(&target) {
                Ok(s) => Some(s),
                Err(e) => {
                    log::err("client", &format!("dial {}:{} failed: {}", target.host, target.port, e));
                    None
                }
            }
        },
        |_| {},
    );

    Ok(())
}

fn split_host_port(s: &str) -> (&str, u16) {
    // handle [ipv6]:port
    if let Some(rest) = s.strip_prefix('[') {
        if let Some(idx) = rest.find(']') {
            let host = &rest[..idx];
            let port: u16 = rest[idx + 2..].parse().unwrap_or(0);
            return (host, port);
        }
    }
    let (h, p) = s.rsplit_once(':').unwrap_or((s, "0"));
    let port: u16 = p.parse().unwrap_or(0);
    (h, port)
}
