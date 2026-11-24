//! `romacode-server tunnel …` — headless command line for the built-in
//! TCP tunnel (vendored chisel-rs). Same `run_cli -> Option<i32>` pattern
//! as `db` / `github` / `fetch`.
//!
//! Subcommands:
//!   `tunnel server [--bind 127.0.0.1:8443] [--auth TOKEN] [--reverse]`
//!                  start a chisel server (forwards a remote RomaCode's
//!                  traffic to local DBs)
//!   `tunnel client <server> <remote>... [--auth TOKEN]`
//!                  connect to a chisel server and open forwards
//!   `tunnel forward <profile_id>`
//!                  look up a db profile's `tunnel` field and open a forward
//!                  so the agent can reach that remote DB locally
//!
//! `tunnel forward` is the bridge between `config.databases.profiles[].tunnel`
//! and the db tools: it opens the forward, prints the local `host:port`, and
//! exits (the forward runs in a foreground thread the operator can Ctrl-C).

use crate::config::Config;
use romacode_tunnel::Tunnel;

/// Entry point. Returns `Some(exit_code)` when handled, `None` otherwise.
pub fn run_cli(args: &[String]) -> Option<i32> {
    if args.first()?.as_str() != "tunnel" {
        return None;
    }
    let sub = args.get(1).map(String::as_str).unwrap_or("help");
    let rest = &args[2.min(args.len())..];
    let code = match sub {
        "server" | "listen" => cmd_server(rest),
        "client" | "connect" => cmd_client(rest),
        "forward" | "fwd" => cmd_forward(rest),
        "help" | "-h" | "--help" => { print_help(); 0 }
        other => { eprintln!("tunnel: unknown subcommand '{other}' (try: tunnel help)"); 2 }
    };
    Some(code)
}

fn cmd_server(rest: &[String]) -> i32 {
    let mut bind = "127.0.0.1:8443".to_string();
    let mut auth: Option<String> = None;
    let mut reverse = false;
    let mut it = rest.iter();
    while let Some(a) = it.next() {
        match a.as_str() {
            "--bind" | "-b" => { if let Some(v) = it.next() { bind = v.clone(); } }
            "--auth" => { if let Some(v) = it.next() { auth = Some(v.clone()); } }
            "--reverse" => { reverse = true; }
            other => { eprintln!("tunnel server: unknown flag {other}"); return 2; }
        }
    }
    eprintln!("tunnel server: listening on {bind} (reverse={reverse}, auth={})",
              if auth.is_some() { "yes" } else { "no" });
    // blocking — runs until Ctrl-C or the listener errors
    match Tunnel::serve(&bind, auth, reverse) {
        Ok(t) => {
            // the server thread runs forever; block this thread on a park
            while !t.is_stopped() {
                std::thread::sleep(std::time::Duration::from_millis(500));
            }
            eprintln!("tunnel server: stopped");
            0
        }
        Err(e) => { eprintln!("tunnel server failed: {e}"); 1 }
    }
}

fn cmd_client(rest: &[String]) -> i32 {
    if rest.is_empty() {
        eprintln!("usage: tunnel client <server> <remote>... [--auth TOKEN]");
        return 2;
    }
    let server = &rest[0];
    let mut auth: Option<String> = None;
    let mut remotes = Vec::new();
    let mut it = rest.iter().skip(1);
    while let Some(a) = it.next() {
        if a == "--auth" { if let Some(v) = it.next() { auth = Some(v.clone()); } continue; }
        remotes.push(a.clone());
    }
    if remotes.is_empty() {
        eprintln!("tunnel client: need at least one remote spec (e.g. 5432:db.host:5432)");
        return 2;
    }
    // open all forwards; keep them alive in this foreground thread
    let mut tunnels = Vec::new();
    for r in &remotes {
        match Tunnel::forward(server, r, auth.clone()) {
            Ok(t) => {
                eprintln!("tunnel client: forwarding {} -> {}", r, t.local_addr());
                tunnels.push(t);
            }
            Err(e) => { eprintln!("tunnel client: {r} failed: {e}"); return 1; }
        }
    }
    // block until all stop
    while tunnels.iter().any(|t| !t.is_stopped()) {
        std::thread::sleep(std::time::Duration::from_millis(500));
    }
    0
}

fn cmd_forward(rest: &[String]) -> i32 {
    if rest.is_empty() {
        eprintln!("usage: tunnel forward <profile_id>");
        return 2;
    }
    let pid = &rest[0];
    let cfg = Config::load();
    let profile = cfg.databases.profiles.iter().find(|p| p.id == *pid).cloned();
    let Some(profile) = profile else {
        eprintln!("tunnel forward: no db profile '{pid}'");
        return 1;
    };
    if profile.tunnel.is_empty() {
        eprintln!("tunnel forward: profile '{pid}' has no tunnel target configured (set databases.profiles[].tunnel)");
        return 1;
    }
    // The tunnel field is a chisel remote spec, optionally with a server
    // prefix separated by '@'. Convention:
    //   "server.host:port|5432:db.internal:5432"
    let (server, remote) = match profile.tunnel.split_once('|') {
        Some((s, r)) => (s.trim().to_string(), r.trim().to_string()),
        None => {
            eprintln!("tunnel forward: profile '{pid}' tunnel value must be '<server>|<remote-spec>'");
            return 1;
        }
    };
    // resolve the auth token from the profile's url_env if present (shared
    // secret convention: the env var may hold either a full URL or just the
    // token; we use it verbatim as the chisel auth token when set)
    let auth = if profile.url_env.is_empty() { None }
               else { std::env::var(&profile.url_env).ok() };
    match Tunnel::forward(&server, &remote, auth) {
        Ok(t) => {
            eprintln!("tunnel forward: {pid} -> {} (via {server})", t.local_addr());
            eprintln!("  the agent can now db_use a profile pointing at {} and run queries", t.local_addr());
            // block until the tunnel ends
            while !t.is_stopped() {
                std::thread::sleep(std::time::Duration::from_millis(500));
            }
            0
        }
        Err(e) => { eprintln!("tunnel forward failed: {e}"); 1 }
    }
}

fn print_help() {
    println!("romacode-server tunnel — built-in TCP tunnel (chisel-rs)\n");
    println!("USAGE:");
    println!("  romacode-server tunnel server [--bind 127.0.0.1:8443] [--auth TOKEN] [--reverse]");
    println!("      start a chisel server. Other RomaCode instances connect to it");
    println!("      to reach THIS machine's local databases.\n");
    println!("  romacode-server tunnel client <server> <remote>... [--auth TOKEN]");
    println!("      connect to a chisel server and open forwards.");
    println!("      remote spec examples: 5432 | db.host:5432 | 5432:db.host:5432 | R:2222:localhost:22\n");
    println!("  romacode-server tunnel forward <profile_id>");
    println!("      open the tunnel declared by config.databases.profiles[<id>].tunnel");
    println!("      (format: '<chisel-server>|<remote-spec>'); blocks until Ctrl-C.\n");
    println!("AUTH: shared secret between chisel client and server; pass via --auth or");
    println!("      the profile's url_env environment variable (for `tunnel forward`).");
}
