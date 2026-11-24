//! `romacode` — the screen. Connects to `romacode-server` (the brain) and
//! draws the sessions: transcript, tabs, sidebars, the live Android panel and
//! the scrcpy-style desktop window. No agent logic, no config file, no tools:
//! everything is a request on the wire and an event back.

mod app;
mod autostart;
mod device;
mod display;
mod input;
mod net;
mod session;
mod settings;
mod terminal;
mod tools;
mod tree;
mod ui;

use anyhow::Result;
use app::App;
use crossterm::{
    event::{
        self, DisableBracketedPaste, DisableMouseCapture, EnableBracketedPaste, EnableMouseCapture,
        Event,
    },
    execute,
    terminal::{disable_raw_mode, enable_raw_mode, EnterAlternateScreen, LeaveAlternateScreen},
};
use net::Net;
use ratatui::{backend::CrosstermBackend, Terminal};
use romacode_shared::protocol::{Reply, Welcome};
use std::io;
use std::sync::Arc;
use std::time::Duration;

fn setup() -> Result<Terminal<CrosstermBackend<io::Stdout>>> {
    enable_raw_mode()?;
    // Roll back everything already applied when a later step fails — the old
    // code could bail out with the terminal left in raw mode (no echo, no
    // line discipline) after the process returned to the shell.
    let run = || -> Result<Terminal<CrosstermBackend<io::Stdout>>> {
        let mut out = io::stdout();
        execute!(
            out,
            EnterAlternateScreen,
            EnableMouseCapture,
            EnableBracketedPaste
        )?;
        let mut term = Terminal::new(CrosstermBackend::new(out))?;
        term.hide_cursor()?;
        term.clear()?;
        Ok(term)
    };
    match run() {
        Ok(t) => Ok(t),
        Err(e) => {
            restore();
            Err(e)
        }
    }
}

fn restore() {
    let _ = disable_raw_mode();
    let _ = execute!(
        io::stdout(),
        DisableBracketedPaste,
        DisableMouseCapture,
        LeaveAlternateScreen
    );
}

fn main() -> Result<()> {
    let args: Vec<String> = std::env::args().skip(1).collect();
    if args.first().map(String::as_str) == Some("--snapshot") {
        return snapshot(&args[1..]);
    }
    // `romacode tools|doctor|github|db|fetch|…` run on the brain binary, so a
    // single command is the whole CLI (scriptable: exit codes + --json).
    if let Some(first) = args.first().map(String::as_str) {
        if autostart::UTILITY_COMMANDS.contains(&first) {
            std::process::exit(autostart::run_utility(&args));
        }
    }
    if matches!(args.first().map(String::as_str), Some("--version" | "-V")) {
        println!("romacode {}", env!("CARGO_PKG_VERSION"));
        return Ok(());
    }
    if matches!(args.first().map(String::as_str), Some("--help" | "-h")) {
        println!(
            "romacode {}\n\nUSAGE: romacode [--server 127.0.0.1:7712] [--no-autostart]\n\nThe screen half of RomaCode. The brain (romacode-server) is auto-started in the\nbackground on first launch — you no longer need to run `romacode-server serve` in\na separate terminal. To drive the brain yourself, pass --no-autostart.\n\nKEYS\n  enter send (queues the query while the agent works) · alt+enter newline · tab Build/Plan · esc interrupt\n  ctrl+s send now — the agent reads it at its next step (or the selected queued query)\n  alt+e edit / alt+d delete queued query · alt+↑/↓ select it\n  ctrl+n new session · ctrl+w close · alt+←/→ switch · alt+1..9 jump\n  ctrl+b side panel · alt+a Android device · alt+[ / ] chat width\n  ctrl+p settings — one page (appearance, permissions, session, tabs & diffs,\n       input, alerts, AI & server, advanced) · / opens command menu · pgup/pgdn scroll · ctrl+q quit\n\nCOMMANDS\n  / opens the slash menu (every settings tree leaf, filtered)\n  /providers · /models · /provider [id] · /model [name] · /apikey <key> · /baseurl <url>\n  /customprovider <id> <base-url> [openai|anthropic]   (run in the brain)\n  /engine [native|langchain] · /mode [build|plan] · /rounds <1..200>\n  /webfetch [on|off] · /rnmap [on|off] · /timeout <5..3600> · /context <tokens> · /prefs\n\nUTILITY COMMANDS  (run on the brain; scriptable, exit codes, --json where noted)\n  romacode tools [list|describe <n>|search <w>|run <n> '<json>'] [--json]   everything the agent can call\n  romacode doctor [--json]       health check: config, provider, key, engine, tools, git/adb\n  romacode github <cmd> · git <cmd> · db <cmd> · fetch <url> · providers · models · tunnel …\n\nOPTIONS\n  --server <addr>     brain address (default 127.0.0.1:7712)\n  --no-autostart       do not spawn romacode-server when the brain is unreachable;\n                      require it to already be running (legacy two-terminal mode)\n  --snapshot <home|chat|new|android|phone|palette|tree|models|slash|root|providers|apikey|fetching|perm> [w] [h]\n                      headless render for development\n  --version · --help\n\nENGINE  the brain runs agent.engine = native | langchain (built-in LangChain in Rust)\n\nAUTO-START  when the brain at --server is not reachable, the screen spawns\n            `romacode-server serve --bind <addr> --cwd <current dir>` as a\n            detached daemon (log: ~/.local/share/romacode/server.log,\n            pid:  ~/.local/share/romacode/server.pid). The brain keeps running\n            after the screen exits. To stop it:  pkill -f romacode-server",
            env!("CARGO_PKG_VERSION")
        );
        return Ok(());
    }

    let mut bind = net::DEFAULT_BIND.to_string();
    let mut autostart_enabled = true;
    // Capture the launch cwd so the auto-started brain runs against the same
    // project the user invoked the screen from. (Taken before any --cwd flag
    // in `serve` could chdir the brain — the brain's cwd comes from its own
    // --cwd argument, but we want the screen's *original* cwd here.)
    let launch_cwd = std::env::current_dir()
        .ok()
        .and_then(|p| p.to_str().map(|s| s.to_string()));
    let mut it = args.iter();
    while let Some(a) = it.next() {
        match a.as_str() {
            "--server" | "-s" => {
                if let Some(v) = it.next() {
                    bind = v.clone();
                }
            }
            "--no-autostart" => autostart_enabled = false,
            other => anyhow::bail!("unknown argument {other} (try --help)"),
        }
    }

    let default_hook = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        // Only the MAIN thread owns the terminal. A panic on a background
        // thread (reader/writer/request workers) used to run `restore()` and
        // stomp the live TUI: raw mode off, alt-screen closed — while the UI
        // kept drawing into a restored terminal.
        if std::thread::current().name() == Some("main") {
            restore();
        }
        default_hook(info);
    }));

    let net = connect_with_retry(&bind, autostart_enabled, launch_cwd.as_deref())?;
    // Handshake BEFORE the terminal is claimed, so version problems print as
    // plain text. A stale brain (an older romacode-server still running as
    // the persistent daemon — it keeps running after the screen exits) used
    // to be silently reused: its Welcome carried no saved providers/models,
    // so Select Model appeared empty even with a fresh screen binary. Now
    // the screen notices version drift and RESTARTS the daemon so both
    // halves match.
    let mut welcome = handshake(&net)?;
    if welcome.protocol_version != Welcome::PROTOCOL_VERSION {
        anyhow::bail!(
            "protocol mismatch: server v{}, screen v{} — reinstall both binaries together",
            welcome.protocol_version,
            Welcome::PROTOCOL_VERSION
        );
    }
    if welcome.server_version != env!("CARGO_PKG_VERSION") {
        if !autostart_enabled {
            anyhow::bail!(
                "brain v{} != screen v{} at {bind} — restart the brain:\n  \
                 pkill -f romacode-server && romacode-server serve",
                welcome.server_version,
                env!("CARGO_PKG_VERSION")
            );
        }
        eprintln!(
            "romacode: brain v{} is stale (screen v{}) — restarting it so saved \
             providers/models match",
            welcome.server_version,
            env!("CARGO_PKG_VERSION")
        );
        autostart::restart_server(&bind, launch_cwd.as_deref()).map_err(|e| {
            anyhow::anyhow!(
                "restarting the stale brain failed: {e}\n\
                 Stop the old one manually:  pkill -f romacode-server"
            )
        })?;
        let mut net2 = None;
        let mut last_err = String::new();
        for _ in 0..3 {
            match Net::connect(&bind) {
                Ok(n) => {
                    net2 = Some(n);
                    break;
                }
                Err(e) => last_err = e,
            }
            std::thread::sleep(Duration::from_millis(400));
        }
        let net = Arc::new(net2.ok_or_else(|| {
            anyhow::anyhow!("{last_err}\nrestarted brain did not come back on {bind}")
        })?);
        welcome = handshake(&net)?;
        if welcome.protocol_version != Welcome::PROTOCOL_VERSION {
            anyhow::bail!(
                "protocol mismatch after restart: server v{}, screen v{} — reinstall both binaries together",
                welcome.protocol_version,
                Welcome::PROTOCOL_VERSION
            );
        }
        if welcome.server_version != env!("CARGO_PKG_VERSION") {
            anyhow::bail!(
                "brain at {bind} is still v{} after a restart — a different \
                 romacode-server binary is probably on PATH; stop it with \
                 pkill -f romacode-server and rebuild",
                welcome.server_version
            );
        }
        eprintln!("romacode: brain restarted at v{}", welcome.server_version);
    }
    let mut term = setup()?;
    let res = run(&mut term, net, &bind, &welcome);
    restore();
    term.show_cursor()?;
    res
}

/// `hello` handshake: everything the screen needs comes out of this reply.
fn handshake(net: &Net) -> Result<Welcome> {
    match net.hello(env!("CARGO_PKG_VERSION")) {
        Ok(Reply::Welcome(w)) => Ok(w),
        Ok(_) => anyhow::bail!("unexpected handshake reply from romacode-server"),
        Err(e) => anyhow::bail!("handshake with romacode-server failed: {e}"),
    }
}

fn connect_with_retry(
    bind: &str,
    autostart_enabled: bool,
    launch_cwd: Option<&str>,
) -> Result<Arc<Net>> {
    let mut last_err = String::new();
    for _ in 0..3 {
        match Net::connect(bind) {
            Ok(n) => return Ok(Arc::new(n)),
            Err(e) => last_err = e,
        }
        std::thread::sleep(Duration::from_millis(400));
    }
    // No brain answering. Auto-start it as a detached daemon and retry the
    // connection for a comfortable window; the brain's startup is dominated
    // by config load + provider catalog, both local.
    if autostart_enabled {
        match autostart::ensure_server_running(bind, launch_cwd) {
            Ok(()) => {
                eprintln!(
                    "romacode: auto-started romacode-server in the background \
                     (log: {}, pid: {})",
                    autostart::server_log_path().display(),
                    autostart::server_pid_path().display()
                );
            }
            Err(e) => {
                anyhow::bail!(
                    "{last_err}\nAuto-starting romacode-server failed: {e}\n\
                     Start the brain manually:  romacode-server serve"
                );
            }
        }
        let deadline = std::time::Instant::now() + Duration::from_secs(15);
        loop {
            match Net::connect(bind) {
                Ok(n) => return Ok(Arc::new(n)),
                Err(e) => last_err = e,
            }
            if std::time::Instant::now() >= deadline {
                break;
            }
            std::thread::sleep(Duration::from_millis(300));
        }
        anyhow::bail!(
            "{last_err}\nAuto-started romacode-server did not become reachable \
             on {bind} within 15s — check the log at {}",
            autostart::server_log_path().display()
        );
    }
    anyhow::bail!(
        "{last_err}\nStart the brain first:  romacode-server serve  \
         (in your project directory), or drop --no-autostart to let the screen \
         spawn it for you"
    )
}

fn run(
    term: &mut Terminal<CrosstermBackend<io::Stdout>>,
    net: Arc<Net>,
    bind: &str,
    welcome: &Welcome,
) -> Result<()> {
    let mut app = App::new(net.clone(), welcome, bind);
    // The "Mouse" setting is honoured live: capture is on by default, off when
    // the user turns it off, and it flips the moment the setting changes.
    let mut mouse_on = true;
    while !app.quit {
        if app.cfg.tui.mouse != mouse_on {
            mouse_on = app.cfg.tui.mouse;
            let mut out = io::stdout();
            let _ = if mouse_on {
                execute!(out, EnableMouseCapture)
            } else {
                execute!(out, DisableMouseCapture)
            };
        }
        if app.dirty || app.animating() {
            term.draw(|f| ui::draw(f, &mut app))?;
            app.dirty = false;
        }
        // 30 fps event loop: the live Android stream lands in shared state
        // asynchronously, so the UI polls it every tick regardless of
        // animation state — that keeps the device panel truly live. With the
        // scrcpy-style window open the tick halves to 60 fps so the desktop
        // window pumps input + frames between terminal events.
        let tick = if app.phone_window.is_some() {
            Duration::from_millis(16)
        } else {
            Duration::from_millis(33)
        };
        if event::poll(tick)? {
            // drain everything queued so typing/paste stays snappy
            loop {
                let ev: Event = event::read()?;
                app.on_event(ev);
                if !event::poll(Duration::from_millis(0))? {
                    break;
                }
            }
        }
        if app.poll() {
            app.dirty = true;
        }
    }
    Ok(())
}

/// Headless render for development: prints the screen as JSON cells.
/// usage: romacode --snapshot <home|chat|new|android|phone|palette|tree|slash|root|providers|apikey|fetching|perm> <w> <h>
fn snapshot(args: &[String]) -> Result<()> {
    use ratatui::backend::TestBackend;
    use romacode_shared::protocol::{
        Block, McpServer, Mode, Msg, SessionSnapshot, Todo, TodoState, ToolStatus,
    };
    let which = args.first().map(String::as_str).unwrap_or("home");
    let w: u16 = args.get(1).and_then(|v| v.parse().ok()).unwrap_or(160);
    let h: u16 = args.get(2).and_then(|v| v.parse().ok()).unwrap_or(45);

    // a dummy in-process protocol server so App can hold a Net
    let welcome = Welcome {
        server_version: env!("CARGO_PKG_VERSION").to_string(),
        protocol_version: Welcome::PROTOCOL_VERSION,
        provider: "anthropic".into(),
        model: "model-name".into(),
        mode_default: Mode::Build,
        engine: "native".into(),
        chat_width_pct: 36,
        sidebar: true,
        cwd: "~/projects/romacode".into(),
        branch: Some("main".into()),
        providers: Vec::new(),
        sessions: Vec::new(),
        device: None,
        missing_keys: Vec::new(),
        context_window: 200_000,
        price_in_per_mtok: 3.0,
        price_out_per_mtok: 15.0,
        max_tool_rounds: 25,
        webfetch_enabled: true,
        rnmap_enabled: true,
        remix_enabled: true,
        request_timeout_secs: 120,
        tui: Default::default(),
    };
    let bind = net::spawn_dummy_server(serde_json::to_string(&welcome)?)
        .map_err(|e| anyhow::anyhow!(e))?;
    let net = Net::connect(&bind).map_err(|e| anyhow::anyhow!(e))?;
    let mut app = App::new(Arc::new(net), &welcome, &bind);
    if which != "home" {
        app.screen = app::Screen::Chat;
        let snap = SessionSnapshot {
            id: 1,
            title: "Retry logic for HTTP client".into(),
            messages: vec![
                Msg::User {
                    text: "Add retry with exponential backoff to the HTTP client and keep the per-request timeout at 10s.".into(),
                },
                Msg::Assistant {
                    blocks: vec![
                        Block::Text {
                            text: "I'll wrap the request in a retry loop with exponential backoff, starting at 200 ms and doubling each attempt.\nThe 10s timeout stays on every individual try.".into(),
                        },
                        Block::Tool {
                            id: "1".into(),
                            name: "Read".into(),
                            arg: "src/client.rs".into(),
                            status: ToolStatus::Ok,
                            extra: None,
                            output: None,
                        },
                        Block::Tool {
                            id: "2".into(),
                            name: "Edit".into(),
                            arg: "src/client.rs".into(),
                            status: ToolStatus::Ok,
                            extra: Some("+11 −1".into()),
                            output: None,
                        },
                        Block::Text {
                            text: "```diff src/client.rs\n@@ -40,1 +40,12 @@\n-    client.get(url).timeout(TIMEOUT).send().await\n+async fn get_with_retry(client: &Client, url: &str) -> Result<Response> {\n+    let mut delay = Duration::from_millis(200);\n+    for attempt in 1..=MAX_RETRIES {\n+        match client.get(url).timeout(TIMEOUT).send().await {\n+            Ok(res) if res.status().is_success() => return Ok(res),\n+            _ if attempt < MAX_RETRIES => { sleep(delay).await; delay *= 2; }\n+            other => return other.map_err(Into::into),\n+        }\n+    }\n+    unreachable!()\n+}\n```\nEach failed attempt now waits 200 ms, 400 ms, 800 ms before retrying, up to `MAX_RETRIES`. Want me to run cargo test?".into(),
                        },
                    ],
                    model: "model-name".into(),
                    mode: Mode::Build,
                    secs: Some(12.4),
                },
            ],
            mode: Mode::Build,
            running: false,
            tokens_in: 38_000,
            tokens_out: 4_100,
            turns: 4,
            todos: vec![
                Todo { text: "Read src/client.rs".into(), state: TodoState::Done },
                Todo { text: "Add backoff retry loop".into(), state: TodoState::Done },
                Todo { text: "Run cargo test".into(), state: TodoState::Active },
                Todo { text: "Update README".into(), state: TodoState::Open },
            ],
            mcp: vec![
                McpServer { name: "filesystem".into(), tools: 11, on: true },
                McpServer { name: "github".into(), tools: 24, on: true },
                McpServer { name: "postgres".into(), tools: 0, on: false },
            ],
            files: vec![romacode_shared::protocol::FileChange {
                path: "src/client.rs".into(),
                added: 11,
                removed: 1,
            }],
            lsp: vec![("rust-analyzer".into(), "ready".into())],
            pending_approvals: Vec::new(),
        };
        app.sessions[0].ingest_snapshot(snap);
        app.new_session_for_snapshot(true);
        if which == "palette" {
            app.palette = Some(app::Palette::default());
        }
        if which == "tree" {
            // render the flat Settings page at the top
            app.tree = Some(crate::tree::Tree::root());
        }
        if which == "models" {
            // the Select Model picker exactly as the Settings page opens it
            let mut st = crate::settings::Settings::root();
            st.screen = crate::settings::Screen::SelectModel;
            st.sel = 0;
            for m in ["deepseek-chat", "deepseek-reasoner", "claude-sonnet-5-5", "gpt-5"] {
                st.models.push(m.into());
                st.model_providers.push("deepseek".into());
            }
            app.settings = Some(st);
        }
        if which == "slash" {
            // render the `/` slash menu with a partial query so the filter shows
            let mut m = crate::tree::SlashMenu::open();
            m.query = "prov".into();
            m.refresh();
            app.slash = Some(m);
        }
        if which == "root" {
            app.settings = Some(crate::settings::Settings::root());
        }
        if which == "providers" {
            let mut st = crate::settings::Settings::root();
            st.screen = crate::settings::Screen::Providers;
            st.sel = 0;
            app.settings = Some(st);
        }
        if which == "apikey" {
            // API-key screen with a fetch error, so the error line is visible
            let k = romacode_shared::catalog::known_at(0).expect("at least one known provider");
            let mut st = crate::settings::Settings::root();
            st.begin_known(k);
            st.input = "sk-demo".into();
            st.cursor = st.input.chars().count();
            st.error = Some(
                "provider host api.example.com resolves to a blocked address".into(),
            );
            app.settings = Some(st);
        }
        if which == "fetching" {
            // API-key screen mid-fetch: the loading spinner must be visible
            let k = romacode_shared::catalog::known_at(0).expect("at least one known provider");
            let mut st = crate::settings::Settings::root();
            st.begin_known(k);
            st.input = "sk-demo".into();
            st.cursor = st.input.chars().count();
            st.loading = true;
            app.settings = Some(st);
        }
        if which == "perm" {
            // the brain asked for a y/n on a shell command: render the popup
            app.screen = app::Screen::Chat;
            app.sessions[0].mode = Mode::Build;
            app.pending_approval.push(app::PendingApproval {
                id: "apr-demo".into(),
                session_id: 1,
                name: "Bash".into(),
                effect: "Run host shell command in project directory:\ncargo test --release".into(),
            });
        }
    }
    let mut term = Terminal::new(TestBackend::new(w, h))?;
    term.draw(|f| ui::draw(f, &mut app))?;
    let buf = term.backend().buffer().clone();
    let mut cells = Vec::new();
    for y in 0..h {
        let mut row = Vec::new();
        for x in 0..w {
            let c = buf.get(x, y);
            let col = |c: ratatui::style::Color| match c {
                ratatui::style::Color::Rgb(r, g, b) => [r as i32, g as i32, b as i32],
                _ => [-1, -1, -1],
            };
            row.push(serde_json::json!([
                c.symbol(),
                col(c.fg),
                col(c.bg),
                c.modifier.contains(ratatui::style::Modifier::BOLD)
            ]));
        }
        cells.push(row);
    }
    println!("{}", serde_json::to_string(&cells)?);
    Ok(())
}
