//! Chisel server.
//!
//! - Listens on a TCP port for WS upgrade requests.
//! - Authenticates the client via the `Auth:` header (token).
//! - Receives Open messages from the client and dials the requested
//!   target (forward remotes).
//! - Optionally listens on local ports for *reverse* remotes; when a
//!   connection arrives, asks the client (over the same WS) to dial
//!   and pipe.

use std::collections::HashMap;
use std::net::{TcpListener, TcpStream};
use std::sync::Arc;

use crate::log;
use crate::mux::{self, Mux};
use crate::remote::Remote;
use crate::ws;

pub struct ServerConfig {
    pub listen: String,
    pub auth: Option<String>,
    pub reverse: bool,
    pub reverse_remotes: Vec<Remote>,
    pub keepalive: u64,
}

/// Is a requested reverse remote allowed by the server policy?
/// - `reverse == false`: nothing is allowed.
/// - empty `reverse_remotes`: everything is allowed (reverse enabled
///   without an explicit allow-list).
/// - otherwise: an entry must match a configured remote on port, and on
///   host too when the policy names a concrete (non-wildcard) host.
fn reverse_allowed(policy: &[Remote], r: &mux::SetupRemote) -> bool {
    policy.iter().any(|p| {
        p.local_port == r.local_port
            && (p.local_host.is_empty()
                || p.local_host == "0.0.0.0"
                || p.local_host == r.local_host)
    })
}

pub fn run(cfg: ServerConfig) -> std::io::Result<()> {
    let listener = TcpListener::bind(&cfg.listen)?;
    log::info("server", &format!("listening on {}", cfg.listen));

    for sock in listener.incoming() {
        let sock = match sock {
            Ok(s) => s,
            Err(e) => {
                log::err("server", &format!("accept: {}", e));
                continue;
            }
        };
        let auth = cfg.auth.clone();
        let reverse = cfg.reverse;
        let reverse_remotes = cfg.reverse_remotes.clone();
        let keepalive = cfg.keepalive;
        std::thread::spawn(move || {
            handle_client(sock, auth, reverse, reverse_remotes, keepalive);
        });
    }
    Ok(())
}

fn handle_client(
    mut sock: TcpStream,
    auth: Option<String>,
    reverse: bool,
    reverse_remotes: Vec<Remote>,
    keepalive: u64,
) {
    let peer = sock.peer_addr().map(|a| a.to_string()).unwrap_or_default();
    let tag = format!("server[{}]", peer);

    // WS handshake
    let custom_headers: HashMap<String, String> = HashMap::new();
    match ws::server_handshake(&mut sock, auth.as_deref(), &custom_headers) {
        Ok(_) => log::info(&tag, "WS upgrade OK"),
        Err(e) => {
            log::err(&tag, &format!("handshake failed: {}", e));
            let body = format!("Unauthorized: {}\n", e);
            let resp = format!(
                "HTTP/1.1 401 Unauthorized\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                body.len(),
                body
            );
            let _ = sock.write_all(resp.as_bytes());
            return;
        }
    }

    let mux = Mux::new(sock, false, tag.clone());

    // Keepalive pinger
    if keepalive > 0 {
        let m = mux.clone();
        let tag_ping = tag.clone();
        std::thread::spawn(move || loop {
            std::thread::sleep(std::time::Duration::from_secs(keepalive));
            if m.is_closed() { break; }
            if let Err(e) = m.send_ping(b"ka") {
                log::debug(&tag_ping, &format!("ping err: {}", e));
                break;
            }
        });
    }

    // Run the mux loop.
    //  - on_open:  client asked us to dial a target (forward remote).
    //             Unless --reverse is disabled, we also allow reverse
    //             Open messages from the client (i.e. the client dials
    //             on our behalf); this is the symmetric Open path and
    //             is always permitted.
    //  - on_setup: client wants us to listen on a list of reverse ports
    //             and forward incoming connections back to it — only for
    //             remotes the server's reverse policy allows, and only
    //             on THIS client's mux.
    let m2 = mux.clone();
    let tag2 = tag.clone();
    // This client's own mux, moved into the on_setup closure so reverse
    // connections are piped through the same session table the read loop
    // dispatches on (no shared "latest client" slot).
    let setup_mux = mux.clone();
    m2.run_loop(
        move |target, id| {
            log::info(&tag2, &format!("session {} -> dial {}:{}", id, target.host, target.port));
            match mux::dial(&target) {
                Ok(s) => Some(s),
                Err(e) => {
                    log::err(&tag2, &format!("dial {}:{} failed: {}", target.host, target.port, e));
                    None
                }
            }
        },
        move |remotes| {
            for r in &remotes {
                if !reverse || !(reverse_remotes.is_empty() || reverse_allowed(&reverse_remotes, r)) {
                    log::err(&tag, &format!(
                        "reverse remote {}:{} not permitted by server policy; rejecting",
                        r.local_host, r.local_port
                    ));
                    // Signal the rejection on the wire: a CLOSE for the
                    // Setup message's session id (0).
                    let _ = setup_mux.send_raw(&mux::encode_msg(mux::MSG_CLOSE, 0, &[]));
                    continue;
                }
                let mux = setup_mux.clone();
                let listen_addr = format!("{}:{}", r.local_host, r.local_port);
                let target = mux::AddrTarget {
                    host: r.remote_host.clone(),
                    port: r.remote_port,
                };
                let tag_listen = tag.clone();
                std::thread::spawn(move || {
                    let listener = match TcpListener::bind(&listen_addr) {
                        Ok(l) => l,
                        Err(e) => {
                            log::err("server", &format!("reverse bind {} failed: {}", listen_addr, e));
                            return;
                        }
                    };
                    log::info(&tag_listen, &format!("reverse listening on {} -> {}:{}", listen_addr, target.host, target.port));
                    for sock in listener.incoming() {
                        let sock = match sock { Ok(s) => s, Err(_) => continue };
                        if mux.is_closed() { break; }
                        let id = mux.alloc_id();
                        if let Err(e) = mux.open_session_dial_later(id, target.clone()) {
                            log::err("server", &format!("reverse open send: {}", e));
                            continue;
                        }
                        let sess = Arc::new(mux::Session {
                            id,
                            local: Some(sock),
                            target: target.clone(),
                            closed: std::sync::atomic::AtomicBool::new(false),
                        });
                        mux.sessions.lock().unwrap().insert(id, sess.clone());
                        let m = mux.clone();
                        let t = tag_listen.clone();
                        std::thread::spawn(move || {
                            m.pipe_local_to_remote(id, sess);
                            let _ = &t;
                        });
                    }
                });
            }
        },
    );
}

use std::io::Write;
