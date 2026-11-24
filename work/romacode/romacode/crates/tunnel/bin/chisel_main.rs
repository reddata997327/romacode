//! Chisel-rs — entry point.
//!
//! Usage:
//!   chisel server --port 8080 [--auth TOKEN] [--reverse]
//!   chisel client <host:port> [remote...] [--auth TOKEN] [--header K:V]
//!
//! Remotes (chisel syntax):
//!   3000
//!   example.com:3000
//!   3000:google.com:80
//!   192.168.0.5:3000:google.com:80
//!   R:2222:localhost:22
//!   R:0.0.0.0:8080:internal:80/tcp
//!
//! This binary is a single executable serving both as client and server,
//! mirroring the original chisel UX.

use std::collections::HashMap;
use std::process::ExitCode;

use romacode_tunnel::client::ClientConfig;
use romacode_tunnel::log;
use romacode_tunnel::remote::Remote;
use romacode_tunnel::server::ServerConfig;

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();

    // Global --verbose / --quiet flag may appear before the subcommand.
    let (verbose, args) = strip_flag(&args, "--verbose", "-v");
    let (quiet, args) = strip_flag(&args, "--quiet", "-q");
    let (show_ver, args) = strip_flag(&args, "--version", "-V");
    let (show_help, args) = strip_flag(&args, "--help", "-h");

    if show_ver {
        println!("chisel-rs {}", romacode_tunnel::VERSION);
        return ExitCode::SUCCESS;
    }
    if args.is_empty() || show_help {
        print_help();
        return if args.is_empty() { ExitCode::FAILURE } else { ExitCode::SUCCESS };
    }

    let level = if verbose { 3 } else if quiet { 1 } else { 2 };
    log::set_level(level);

    let sub = args[0].as_str();
    let rest: Vec<String> = args[1..].to_vec();

    match sub {
        "server" => match run_server(rest) {
            Ok(()) => ExitCode::SUCCESS,
            Err(e) => {
                eprintln!("server error: {}", e);
                ExitCode::FAILURE
            }
        },
        "client" => match run_client(rest) {
            Ok(()) => ExitCode::SUCCESS,
            Err(e) => {
                eprintln!("client error: {}", e);
                ExitCode::FAILURE
            }
        },
        other => {
            eprintln!("unknown subcommand: {}\n", other);
            print_help();
            ExitCode::FAILURE
        }
    }
}

fn strip_flag(args: &[String], long: &str, short: &str) -> (bool, Vec<String>) {
    let mut found = false;
    let mut out = Vec::with_capacity(args.len());
    for a in args {
        if a == long || a == short {
            found = true;
        } else {
            out.push(a.clone());
        }
    }
    (found, out)
}

fn print_help() {
    print!(
        r#"chisel-rs {ver} — a fast TCP tunnel over WebSocket, pure-Rust, std-only.

USAGE:
    chisel server --port <port> [--auth <token>] [--reverse]
    chisel client <server-host:port> <remote>... [--auth <token>]

EXAMPLES:
    # Run a server on port 8080 with auth token "secret"
    chisel server --port 8080 --auth secret

    # Forward local 3000 to remote google.com:80 through the server
    chisel client 127.0.0.1:8080 3000:google.com:80 --auth secret

    # Reverse: server listens on 2222, client forwards to its local :22
    chisel server --port 8080 --auth secret --reverse
    chisel client 127.0.0.1:8080 R:2222:localhost:22 --auth secret

REMOTE SPEC:
    <local-host>:<local-port>:<remote-host>:<remote-port>/<proto>
    local-host defaults to 0.0.0.0; local-port defaults to remote-port;
    remote-host defaults to 127.0.0.1; proto defaults to tcp.
    Prefix "R:" for a reverse remote (server listens, client dials).

NOTE: this is a chisel-shaped tunnel (same remote syntax, forward +
reverse model, single-binary client+server). It is NOT wire-compatible
with jpillora/chisel (which uses crypto/ssh). Transport is plaintext
WebSocket + token auth — use behind TLS or in a trusted network.

FLAGS:
    -h, --help       show this help
    -V, --version    print version
    -v, --verbose    debug logging
    -q, --quiet      errors only
"#,
        ver = romacode_tunnel::VERSION
    );
}

fn run_server(args: Vec<String>) -> std::io::Result<()> {
    let mut port: u16 = 0;
    let mut host = String::from("0.0.0.0");
    let mut auth: Option<String> = None;
    let mut reverse = false;
    let mut keepalive: u64 = 25;

    let mut i = 0;
    while i < args.len() {
        match args[i].as_str() {
            "--port" | "-p" => {
                i += 1;
                port = args
                    .get(i)
                    .and_then(|s| s.parse().ok())
                    .ok_or_else(|| std::io::Error::new(std::io::ErrorKind::InvalidInput, "--port needs a value"))?;
            }
            "--host" => {
                i += 1;
                host = args
                    .get(i)
                    .cloned()
                    .ok_or_else(|| std::io::Error::new(std::io::ErrorKind::InvalidInput, "--host needs a value"))?;
            }
            "--auth" => {
                i += 1;
                auth = args.get(i).cloned();
            }
            "--reverse" => reverse = true,
            "--keepalive" => {
                i += 1;
                keepalive = args
                    .get(i)
                    .and_then(|s| s.parse().ok())
                    .unwrap_or(25);
            }
            "--header" => {
                // server ignores custom headers
                i += 1;
            }
            other => {
                return Err(std::io::Error::new(
                    std::io::ErrorKind::InvalidInput,
                    format!("server: unknown flag {}", other),
                ));
            }
        }
        i += 1;
    }

    if port == 0 {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            "--port is required",
        ));
    }

    // For --reverse without explicit remotes, do nothing special —
    // the client will send R: remotes and the server will accept them
    // (server allows reverse because --reverse was set).
    let rev_remotes = Vec::new();

    let cfg = ServerConfig {
        listen: format!("{}:{}", host, port),
        auth,
        reverse,
        reverse_remotes: rev_remotes,
        keepalive,
    };
    romacode_tunnel::server::run(cfg)
}

fn run_client(args: Vec<String>) -> std::io::Result<()> {
    if args.is_empty() {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            "client needs a <server> argument",
        ));
    }
    let mut server: Option<String> = None;
    let mut auth: Option<String> = None;
    let mut headers: HashMap<String, String> = HashMap::new();
    let mut keepalive: u64 = 25;
    let mut max_retry: u64 = 1;
    let mut remotes: Vec<Remote> = Vec::new();

    let mut i = 0;
    while i < args.len() {
        let a = &args[i];
        if a == "--auth" || a == "-A" {
            i += 1;
            auth = args.get(i).cloned();
        } else if a == "--header" || a == "-H" {
            i += 1;
            if let Some(h) = args.get(i) {
                if let Some((k, v)) = h.split_once(':') {
                    headers.insert(k.trim().to_string(), v.trim().to_string());
                }
            }
        } else if a == "--keepalive" {
            i += 1;
            keepalive = args.get(i).and_then(|s| s.parse().ok()).unwrap_or(25);
        } else if a == "--max-retry" {
            i += 1;
            max_retry = args.get(i).and_then(|s| s.parse().ok()).unwrap_or(1);
        } else if a.starts_with("--") {
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidInput,
                format!("client: unknown flag {}", a),
            ));
        } else if server.is_none() {
            server = Some(a.clone());
        } else {
            match Remote::parse(a) {
                Ok(r) => remotes.push(r),
                Err(e) => {
                    return Err(std::io::Error::new(
                        std::io::ErrorKind::InvalidInput,
                        format!("bad remote '{}': {}", a, e),
                    ));
                }
            }
        }
        i += 1;
    }

    let server = server.ok_or_else(|| {
        std::io::Error::new(std::io::ErrorKind::InvalidInput, "no <server> provided")
    })?;

    if remotes.is_empty() {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            "no remotes provided",
        ));
    }

    // Pretty-print the plan
    for r in &remotes {
        let kind = if r.reverse { "reverse" } else { "forward" };
        log::info(
            "client",
            &format!("{} remote {} ({} -> {}:{})", kind, r.summary(), r.local_host, r.remote_host, r.remote_port),
        );
    }

    let cfg = ClientConfig {
        server,
        auth,
        headers,
        remotes,
        keepalive,
        max_retry,
    };
    romacode_tunnel::client::run(cfg)
}
