//! `romacode-server db …` — headless command line for the built-in native
//! database engine. Same pattern as `fetch` / `github` / `providers`: a
//! `run_cli` that returns `Some(code)` when it handled the call, `None` to
//! let the next dispatcher try.
//!
//! Subcommands:
//!   `db`                  show status (alias for `db list`)
//!   `db list`             list local databases + saved remote profiles
//!   `db test`             engine selftest (pure logic, no network)
//!   `db schema <db> [t]`  show a table's key structure, or all tables' keys
//!   `db query  <db> <sql>` run a read-only SELECT against a local db
//!   `db add <id> <kind> <url_env> [--url URL] [--readonly] [--tunnel T]`
//!                         append a profile to config.databases.profiles
//!
//! `db add` writes back to the config file. The others are read-only.

use crate::config::{Config, DatabaseProfile};
use crate::tools::db::{self as dbtools};
use serde_json::json;

/// Entry point. Returns `Some(exit_code)` when this dispatcher handled `args`.
pub fn run_cli(args: &[String]) -> Option<i32> {
    // recognise `db` and `db <sub>`; never grab `--db-style` flags etc.
    let first = args.first()?;
    if first != "db" {
        return None;
    }
    let sub = args.get(1).map(String::as_str).unwrap_or("list");
    let rest = &args[2.min(args.len())..];
    let code = match sub {
        "list" | "ls" | "status" | "" => cmd_list(),
        "test" | "selftest" => cmd_test(),
        "schema" | "keys" => cmd_schema(rest),
        "query" | "select" => cmd_query(rest),
        "add" | "add-profile" => cmd_add(rest),
        "help" | "-h" | "--help" => {
            print_help();
            0
        }
        other => {
            eprintln!("db: unknown subcommand '{other}' (try: db help)");
            2
        }
    };
    Some(code)
}

fn cmd_list() -> i32 {
    let cfg = Config::load();
    match dbtools::execute(&cfg, "db_list", &json!({}), &std::sync::atomic::AtomicBool::new(false)) {
        Ok(out) => { println!("{out}"); 0 }
        Err(e) => { eprintln!("db list failed: {e}"); 1 }
    }
}

fn cmd_test() -> i32 {
    // engine selftest is pure logic; lives in the db crate.
    let fails = romacode_db::DbEngine::selftest_failures();
    if fails == 0 {
        println!("db engine selftest: OK");
        0
    } else {
        eprintln!("db engine selftest: {fails} failure(s)");
        1
    }
}

fn cmd_schema(rest: &[String]) -> i32 {
    if rest.is_empty() {
        eprintln!("usage: db schema <db> [table]");
        return 2;
    }
    let db = &rest[0];
    let table = rest.get(1).map(String::as_str);
    let cfg = Config::load();
    let input = match table {
        Some(t) => json!({"db": db, "table": t}),
        None => json!({"db": db}),
    };
    match dbtools::execute(&cfg, "db_schema", &input, &std::sync::atomic::AtomicBool::new(false)) {
        Ok(out) => { println!("{out}"); 0 }
        Err(e) => { eprintln!("db schema failed: {e}"); 1 }
    }
}

fn cmd_query(rest: &[String]) -> i32 {
    if rest.len() < 2 {
        eprintln!("usage: db query <db> <sql>");
        return 2;
    }
    let db = &rest[0];
    let sql = rest[1..].join(" ");
    let cfg = Config::load();
    let input = json!({"db": db, "sql": sql});
    match dbtools::execute(&cfg, "db_query", &input, &std::sync::atomic::AtomicBool::new(false)) {
        Ok(out) => { println!("{out}"); 0 }
        Err(e) => { eprintln!("db query failed: {e}"); 1 }
    }
}

fn cmd_add(rest: &[String]) -> i32 {
    // db add <id> <kind> <url_env> [--url URL] [--readonly] [--tunnel T]
    if rest.len() < 3 {
        eprintln!("usage: db add <id> <kind> <url_env> [--url URL] [--readonly] [--tunnel T]");
        return 2;
    }
    let id = rest[0].clone();
    let kind = rest[1].clone();
    let url_env = rest[2].clone();
    let mut url = String::new();
    let mut readonly = false;
    let mut tunnel = String::new();
    let mut i = 3;
    while i < rest.len() {
        match rest[i].as_str() {
            "--url" => { if i + 1 < rest.len() { url = rest[i+1].clone(); i += 2; continue; } }
            "--readonly" => { readonly = true; i += 1; continue; }
            "--tunnel" => { if i + 1 < rest.len() { tunnel = rest[i+1].clone(); i += 2; continue; } }
            other => { eprintln!("db add: unknown flag {other}"); return 2; }
        }
    }
    let profile = DatabaseProfile { id, kind, url_env, url, readonly, tunnel };
    let mut cfg = Config::load();
    // replace if id exists, else append
    if let Some(existing) = cfg.databases.profiles.iter_mut().find(|p| p.id == profile.id) {
        *existing = profile.clone();
    } else {
        cfg.databases.profiles.push(profile.clone());
    }
    cfg.save();
    println!("saved profile '{}' [{}] (url_env={})", profile.id, profile.kind, profile.url_env);
    if !profile.tunnel.is_empty() {
        println!("tunnel: {}", profile.tunnel);
    }
    0
}

fn print_help() {
    println!("romacode-server db — built-in native database engine\n");
    println!("USAGE:");
    println!("  romacode-server db                  list databases + profiles (alias for `db list`)");
    println!("  romacode-server db list             same as above");
    println!("  romacode-server db test             run the engine selftest (pure logic, no network)");
    println!("  romacode-server db schema <db> [t]  show a table's key structure, or all tables' keys");
    println!("  romacode-server db query <db> <sql> run a read-only SELECT against a local db");
    println!("  romacode-server db add <id> <kind> <url_env> [--url URL] [--readonly] [--tunnel T]");
    println!("                                      append (or replace) a profile in config.databases");
    println!();
    println!("KIND: local | postgres | mysql | mongodb | redis | http");
    println!("URL_ENV: the environment variable holding the connection URL (preferred over --url)");
    println!("TIER: read-only (default) | read-write | full   (set in config.databases.tier or the TUI)");
}
