//! `romacode-server` — the brain. Starts the HTTP/WS server that owns the
//! agent loop (native + langchain engines), sessions, security gate, tools
//! and engines. The TUI (`romacode` binary in crates/client-tui) is just a
//! screen that connects to it.
//!
//! CLI utilities that belong to the brain also live here (they operate on
//! config/providers/webfetch/adb rather than pixels):
//! `--print-config`, `--config-path`, `--verify-android`, `fetch`, `providers`,
//! `github`, `models`, `serve` (default).

use anyhow::Result;
use romacode_server::config::Config;

fn main() -> Result<()> {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let first = args.first().map(String::as_str).unwrap_or("serve");

    // headless agent-server (the default: `romacode-server` alone serves)
    if let Some(code) = romacode_server::webfetch::run_cli(&args) {
        std::process::exit(code);
    }
    if let Some(code) = romacode_server::providers_cli::run_cli(&args) {
        std::process::exit(code);
    }
    if let Some(code) = romacode_server::github::run_cli(&args) {
        std::process::exit(code);
    }
    if let Some(code) = romacode_server::db_cli::run_cli(&args) {
        std::process::exit(code);
    }
    if let Some(code) = romacode_server::tunnel_cli::run_cli(&args) {
        std::process::exit(code);
    }
    if let Some(code) = romacode_server::cli_tools::run_cli(&args) {
        std::process::exit(code);
    }
    match first {
        "--verify-android" => return verify_android(),
        "--print-config" => {
            let (cfg, warnings) = Config::load_report();
            for w in &warnings {
                eprintln!("warning: {w}");
            }
            println!("{}", cfg.to_pretty_json());
            return Ok(());
        }
        "--config-path" => {
            println!(
                "{}",
                Config::path()
                    .map(|p| p.display().to_string())
                    .unwrap_or_default()
            );
            return Ok(());
        }
        "--version" | "-V" => {
            println!("romacode-server {}", env!("CARGO_PKG_VERSION"));
            return Ok(());
        }
        "--help" | "-h" => {
            print_help();
            return Ok(());
        }
        "serve" => {
            let mut bind = "127.0.0.1:7712".to_string();
            let mut it = args.iter().skip(1);
            while let Some(a) = it.next() {
                match a.as_str() {
                    "--bind" | "-b" => {
                        if let Some(v) = it.next() {
                            bind = v.clone();
                        }
                    }
                    "--cwd" => {
                        if let Some(v) = it.next() {
                            std::env::set_current_dir(v)
                                .map_err(|e| anyhow::anyhow!("--cwd {v}: {e}"))?;
                        }
                    }
                    other => anyhow::bail!("unknown option {other} (try --help)"),
                }
            }
            let (cfg, warnings) = Config::load_report();
            for w in &warnings {
                eprintln!("warning: {w}");
            }
            // First run: drop the starter persona template at the default
            // path so the standing-instructions channel works out of the box.
            romacode_server::persona::ensure_starter(&cfg);
            eprintln!("project directory: {}", std::env::current_dir().map(|p| p.display().to_string()).unwrap_or_default());
            // Initialise the built-in native database engine against the
            // configured (or default) home dir before serving. Failures here
            // are non-fatal: the db tools fall back to an ephemeral home.
            let db_home = if cfg.databases.home.is_empty() {
                dirs::home_dir().unwrap_or_else(|| std::path::PathBuf::from("."))
                    .join(".romacode").join("db")
            } else {
                std::path::PathBuf::from(&cfg.databases.home)
            };
            if let Err(e) = romacode_server::tools::db::init(db_home) {
                eprintln!("warning: db engine init failed (db tools will use an ephemeral home): {e}");
            }
            return romacode_server::api::serve(cfg, &bind);
        }
        other => {
            anyhow::bail!("unknown command {other} (try --help)");
        }
    }
}

fn print_help() {
    println!(
        "romacode-server {}\n\nUSAGE:\n  romacode-server serve [--bind 127.0.0.1:7712] [--cwd /path/to/project]\n      start the brain: agent loop (native + langchain engines), sessions,\n      security gate, tools and engines over HTTP + WebSocket.\n\nUTILITY COMMANDS\n  fetch <url> [--mode text|markdown|raw|json|links|scrape|headers] [--selector css]\n  providers · models · provider [id] · model [name]\n  github <search|repo|resource|api> … · git <clone|status|diff|log|branches>\n  db list · db test · db schema <db> [t] · db query <db> <sql> · db add <id> <kind> <url_env>\n  tunnel server|client|forward  open a chisel tunnel so the agent can reach remote DBs\n  tools [list|describe <n>|search <w>|run <n> '<json>'] [--json]   everything the agent can call\n  doctor [--json]      health check: config, provider, key, engine, tools, git/adb, state dir\n\nDIAGNOSTICS\n  --verify-android     run non-destructive checks against the selected real Android target\n  --print-config       show the effective settings (after env overrides)\n  --config-path        print the config file location\n  --version · --help\n\nCONFIG  {}\nAPI KEY  export ANTHROPIC_API_KEY=... (or OPENAI_API_KEY, GROQ_API_KEY, …)\nENGINE   agent.engine = native | langchain   (langchain = built-in LangChain in Rust:\n         agent executor, memory and RAG tools; OpenAI-compatible providers)",
        env!("CARGO_PKG_VERSION"),
        Config::path()
            .map(|p| p.display().to_string())
            .unwrap_or_default()
    );
}

/// Non-destructive checks against the selected real Android target.
fn verify_android() -> Result<()> {
    let mut cfg = Config::load();
    // Running this explicit diagnostic is an intentional connection request.
    cfg.android.auto_start = true;
    println!("ROMACODE ANDROID — real ADB target checks");
    println!("  connecting through Android Platform-Tools; no simulator fallback");
    let mut android =
        romacode_server::android::Android::spawn(romacode_server::android::Android::config_from(&cfg));
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(30);
    loop {
        android.poll();
        match android.status().map(|s| s.phase) {
            Some(romacode_server::android::Phase::Online) => break,
            Some(romacode_server::android::Phase::Error(error)) => {
                anyhow::bail!("real Android target unavailable: {error}")
            }
            _ => {}
        }
        if std::time::Instant::now() >= deadline {
            anyhow::bail!("timed out waiting for the selected authorized Android target");
        }
        std::thread::sleep(std::time::Duration::from_millis(100));
    }
    let (pass, fail, log) = romacode_server::android::verify(&mut android);
    for line in log {
        println!("  {line}");
    }
    println!("\nRESULT: {pass} passed, {fail} failed");
    if fail > 0 {
        anyhow::bail!("real Android target verification failed");
    }
    Ok(())
}
