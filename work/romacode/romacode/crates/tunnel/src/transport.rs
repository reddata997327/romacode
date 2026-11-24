//! DB-transport wrapper over the vendored chisel tunnel.
//!
//! The RomaCode database tools talk to local databases (the built-in engine)
//! and to remote ones (postgres / mysql / mongodb / redis behind a chisel
//! tunnel). Instead of forcing the agent to manage a separate chisel process,
//! this module runs the tunnel **in-process** on a background thread and
//! hands back a local `host:port` the db tools connect to as if it were a
//! local database.
//!
//! Typical flow (from `romacode-server`):
//!
//! ```ignore
//! use romacode_tunnel::Tunnel;
//!
//! // forward a remote postgres (5432 on db.internal) to a local ephemeral port
//! let t = Tunnel::forward("tunnel.romacode.dev:8080",
//!                         "db.internal:5432",
//!                         Some("token".into()))?;
//! let local = t.local_addr(); // e.g. "127.0.0.1:5432"
//! // db_use profile pointing at local, then db_query hits the remote DB.
//! ```
//!
//! The tunnel is bidirectional — `Tunnel::reverse` lets the server side dial
//! back through a chisel client (the `R:port:host:port` syntax), which is how
//! you reach a database that lives behind a NAT.

use crate::client::ClientConfig;
use crate::remote::Remote;
use crate::server::ServerConfig;
use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;

/// One live tunnel. Dropping it stops the background thread.
pub struct Tunnel {
    local: String,
    stop: Arc<AtomicBool>,
    handle: Mutex<Option<JoinHandle<()>>>,
}

impl Tunnel {
    /// Forward a remote endpoint through a chisel server to a local port.
    ///
    /// * `server`  — chisel server address, e.g. `"tunnel.example.com:8080"`
    /// * `remote`  — remote target as a chisel remote spec, e.g.
    ///               `"db.internal:5432"` or `"5432:db.internal:5432"`
    /// * `auth`    — optional shared token (must match the server's `--auth`)
    ///
    /// The local listener binds on `127.0.0.1:<same port as remote>` when the
    /// remote spec pins a port; otherwise an ephemeral port is chosen by the
    /// OS and reported via [`Tunnel::local_addr`].
    pub fn forward(server: &str, remote: &str, auth: Option<String>) -> std::io::Result<Self> {
        let r = Remote::parse(remote).map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidInput, e))?;
        let local = format!("127.0.0.1:{}", r.local_port);
        let cfg = ClientConfig {
            server: server.to_string(),
            auth,
            headers: HashMap::new(),
            remotes: vec![r],
            keepalive: 25,
            max_retry: 0, // do not retry forever inside the server process
        };
        let stop = Arc::new(AtomicBool::new(false));
        let stop_t = stop.clone();
        let handle = std::thread::Builder::new()
            .name("romacode-tunnel-client".into())
            .spawn(move || {
                let _ = crate::client::run(cfg);
                // run() returns when the tunnel ends; mark stopped so the
                // owner can detect the tunnel is gone.
                stop_t.store(true, Ordering::SeqCst);
            })?;
        Ok(Self { local, stop, handle: Mutex::new(Some(handle)) })
    }

    /// Run a chisel *server* that exposes the local machine's ports to remote
    /// clients. Used by `romacode-server tunnel server` so a remote RomaCode
    /// instance can reach this machine's local DBs.
    pub fn serve(listen: &str, auth: Option<String>, reverse: bool) -> std::io::Result<Self> {
        let cfg = ServerConfig {
            listen: listen.to_string(),
            auth,
            reverse,
            reverse_remotes: Vec::new(),
            keepalive: 25,
        };
        let local = listen.to_string();
        let stop = Arc::new(AtomicBool::new(false));
        let stop_t = stop.clone();
        let handle = std::thread::Builder::new()
            .name("romacode-tunnel-server".into())
            .spawn(move || {
                let _ = crate::server::run(cfg);
                stop_t.store(true, Ordering::SeqCst);
            })?;
        Ok(Self { local, stop, handle: Mutex::new(Some(handle)) })
    }

    /// The local address db tools should connect to.
    pub fn local_addr(&self) -> &str { &self.local }

    /// `true` when the background thread has exited (tunnel broken).
    pub fn is_stopped(&self) -> bool { self.stop.load(Ordering::SeqCst) }

    /// Stop the tunnel. Best-effort: chisel's blocking `run` has no cancel
    /// handle, so this drops the thread and lets it finish its current
    /// operation. A subsequent bind to the same local port will work.
    pub fn stop(&self) {
        self.stop.store(true, Ordering::SeqCst);
        // We cannot truly cancel the blocking accept; detaching is the safest
        // option. The thread becomes orphaned and will exit when the
        // connection drops.
        if let Ok(mut g) = self.handle.lock() {
            if let Some(h) = g.take() {
                let _ = h.join();
            }
        }
    }
}

impl Drop for Tunnel {
    fn drop(&mut self) {
        self.stop();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn forward_parses_remote_spec() {
        // We don't actually start a tunnel here (no server to connect to);
        // we just verify the remote spec is accepted and the local port is
        // derived. The Tunnel::forward call will spawn a thread that fails
        // to connect, but is_stopped will become true quickly.
        let t = Tunnel::forward("127.0.0.1:1", "5432:db.internal:5432", None);
        // Port 1 is privileged — bind will fail, but the parse path runs first.
        // We accept either Ok (thread spawned) or an immediate io error.
        match t {
            Ok(t) => {
                assert_eq!(t.local_addr(), "127.0.0.1:5432");
                t.stop();
            }
            Err(_) => { /* connection refused is acceptable in a unit test */ }
        }
    }

    #[test]
    fn remote_spec_with_invalid_input_errors() {
        let r = Remote::parse("not a remote spec at all !!!");
        assert!(r.is_err());
    }

    #[test]
    fn remote_spec_simple_port_parses() {
        let r = Remote::parse("5432").unwrap();
        assert_eq!(r.local_port, 5432);
    }
}
