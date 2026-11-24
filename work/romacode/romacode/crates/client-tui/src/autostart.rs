//! Auto-start: when the screen (`romacode`) cannot reach `romacode-server`,
//! it spawns the brain itself as a fully detached background daemon so the
//! user never has to open a second terminal just to run
//! `romacode-server serve`.
//!
//! ## Lifecycle
//!
//! * The screen first tries to connect to `bind` (a few quick retries, see
//!   `main::connect_with_retry`).
//! * If every retry fails, [`ensure_server_running`] locates the
//!   `romacode-server` binary (PATH first, then the directory the `romacode`
//!   screen binary itself lives in), spawns `romacode-server serve --bind
//!   <bind> --cwd <current_dir>` with stdio redirected to a log file, and
//!   `setsid(2)`-detaches it from the controlling terminal.
//! * The screen then polls the port until the brain answers (or a deadline
//!   expires) and connects normally.
//!
//! The brain keeps running after the screen exits, so a subsequent
//! `romacode` invocation just reconnects to the existing instance. If you
//! switch project directories you should stop the old brain first:
//!
//! ```text
//! pkill -f romacode-server        # or: kill $(cat ~/.local/share/romacode/server.pid)
//! ```
//!
//! Override the behaviour with `romacode --no-autostart` when you want to
//! drive the brain's lifecycle yourself (e.g. under systemd or in a
//! container).

use std::path::PathBuf;
use std::process::Command;
use std::time::{Duration, Instant};

/// Where the auto-started server appends its stdout/stderr. Lives under the
/// user's `~/.local/share/romacode/` data dir so it survives across launches
/// and is easy to find when something goes wrong.
pub fn server_log_path() -> PathBuf {
    data_dir().join("romacode").join("server.log")
}

/// PID file for the most recently auto-started brain — handy for `kill
/// $(cat ...)`, monitoring, or hygiene scripts. Not authoritative (the brain
/// may have been started manually), just a best-effort marker.
pub fn server_pid_path() -> PathBuf {
    data_dir().join("romacode").join("server.pid")
}

fn data_dir() -> PathBuf {
    // Respect XDG_DATA_HOME / HOME; fall back to /tmp only if neither is set
    // (extremely rare — containers run as a real user).
    if let Ok(d) = std::env::var("XDG_DATA_HOME") {
        if !d.is_empty() {
            return PathBuf::from(d);
        }
    }
    if let Ok(home) = std::env::var("HOME") {
        if !home.is_empty() {
            return PathBuf::from(home).join(".local").join("share");
        }
    }
    PathBuf::from("/tmp")
}

/// Locate the `romacode-server` binary. Strategy:
/// 1. `PATH` lookup (the common case after `install.sh`).
/// 2. The directory the `romacode` screen binary itself lives in — handles
///    unpack-and-run layouts where both binaries sit side-by-side in
///    `bin/` or a extracted `target/release/` without being on `PATH`.
pub fn locate_server_bin() -> Option<PathBuf> {
    if let Ok(path) = std::env::var("PATH") {
        for dir in path.split(':') {
            if dir.is_empty() {
                continue;
            }
            let cand = PathBuf::from(dir).join("romacode-server");
            if is_executable(&cand) {
                return Some(cand);
            }
        }
    }
    if let Ok(exe) = std::env::current_exe() {
        if let Some(parent) = exe.parent() {
            let cand = parent.join("romacode-server");
            if is_executable(&cand) {
                return Some(cand);
            }
            // Also try a `bin/` sibling — common when the screen is run from
            // a checkout root and the brain is in `./bin/romacode-server`.
            let cand2 = parent.join("bin").join("romacode-server");
            if is_executable(&cand2) {
                return Some(cand2);
            }
        }
    }
    None
}

#[cfg(unix)]
fn is_executable(p: &PathBuf) -> bool {
    use std::os::unix::fs::PermissionsExt;
    match std::fs::metadata(p) {
        Ok(m) => m.is_file() && (m.permissions().mode() & 0o111 != 0),
        Err(_) => false,
    }
}

#[cfg(not(unix))]
fn is_executable(p: &PathBuf) -> bool {
    std::fs::metadata(p).map(|m| m.is_file()).unwrap_or(false)
}

/// Quick TCP probe — `Ok` if anything is accepting on `bind`. Cheaper than
/// the full WebSocket handshake, and good enough for "is the brain up yet".
/// An unparseable bind is a hard error, NOT a silent probe of
/// 127.0.0.1:7712: silently probing the default address could find ANOTHER
/// brain there and then skip spawning the one the user actually asked for.
fn port_open(bind: &str) -> Result<bool, String> {
    let addr: std::net::SocketAddr = bind
        .parse()
        .map_err(|e| format!("invalid --server address {bind:?}: {e}"))?;
    Ok(std::net::TcpStream::connect_timeout(&addr, Duration::from_millis(500)).is_ok())
}

/// Make sure a `romacode-server` is reachable on `bind`. If something is
/// already listening, this is a no-op. Otherwise it spawns the brain as a
/// detached daemon and waits (up to [`SPAWN_DEADLINE`]) for it to bind.
///
/// `cwd_override`, when `Some`, is passed to the brain via `--cwd <dir>` so
/// the auto-started brain operates on the same project the screen was
/// launched from.
pub fn ensure_server_running(bind: &str, cwd_override: Option<&str>) -> Result<(), String> {
    // Race-window check: someone may have started a brain between the
    // screen's connect retries and this call.
    if port_open(bind)? {
        return Ok(());
    }

    let bin = locate_server_bin().ok_or_else(|| {
        "romacode-server not found on PATH or beside the romacode binary — install it (bash scripts/install.sh) or start it manually".to_string()
    })?;

    let log = server_log_path();
    if let Some(parent) = log.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    let log_file = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&log)
        .map_err(|e| format!("open log {}: {e}", log.display()))?;
    let log_file_err = log_file
        .try_clone()
        .map_err(|e| format!("dup log handle: {e}"))?;

    let mut cmd = Command::new(&bin);
    cmd.arg("serve")
        .arg("--bind")
        .arg(bind)
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::from(log_file))
        .stderr(std::process::Stdio::from(log_file_err));
    if let Some(cwd) = cwd_override {
        cmd.arg("--cwd").arg(cwd);
    }

    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt;
        // Detach: new session + process group, so the brain does not receive
        // SIGHUP/SIGINT when the user's terminal or the screen exits. The
        // `setsid` libc symbol is always present on Linux/glibc and musl.
        unsafe {
            cmd.pre_exec(|| {
                extern "C" {
                    fn setsid() -> i32;
                }
                let _ = setsid();
                Ok(())
            });
        }
    }

    let child = cmd
        .spawn()
        .map_err(|e| format!("spawn {} serve: {e}", bin.display()))?;
    let pid = child.id();
    // Reap on a detached thread: outright dropping the `Child` left a ZOMBIE
    // for as long as this screen process ran (the kernel keeps the dead brain
    // in the process table until waited on).
    std::thread::Builder::new()
        .name("autostart-reap".into())
        .spawn(move || {
            let mut child = child;
            let _ = child.wait();
        })
        .ok();

    let pid_path = server_pid_path();
    if let Some(parent) = pid_path.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    let _ = std::fs::write(&pid_path, format!("{pid}\n"));

    // Wait for the brain to finish loading config and bind the port.
    let deadline = Instant::now() + SPAWN_DEADLINE;
    while Instant::now() < deadline {
        if port_open(bind)? {
            // Brief settle: the listener is up but the WS handler may still
            // be arming; one extra short tick avoids a spurious first
            // handshake error.
            std::thread::sleep(Duration::from_millis(150));
            return Ok(());
        }
        std::thread::sleep(Duration::from_millis(150));
    }
    Err(format!(
        "server was spawned (pid {pid}) but did not bind {bind} within {}s — check the log at {}",
        SPAWN_DEADLINE.as_secs(),
        log.display()
    ))
}

/// How long to wait for the auto-started brain to come up. The brain's
/// startup is dominated by config load + provider catalog resolution, both
/// of which are local; 8s is comfortable even on a cold disk.
const SPAWN_DEADLINE: Duration = Duration::from_secs(8);

/// How long `stop_server` waits for the port to go quiet before falling back
/// to `pkill` and, failing that, giving up.
const STOP_DEADLINE: Duration = Duration::from_secs(3);

/// Best-effort stop of a running brain on `bind`. Tries the pid file first,
/// then `pkill -f romacode-server`, and waits for the port to go quiet.
/// Returns `Ok(())` when nothing is listening any more (or nothing was).
pub fn stop_server(bind: &str) -> Result<(), String> {
    if !port_open(bind)? {
        return Ok(()); // nothing running
    }
    // 1) the pid recorded by the last auto-start
    if let Ok(pid) = std::fs::read_to_string(server_pid_path()) {
        let pid = pid.trim().to_string();
        if pid.chars().all(|c| c.is_ascii_digit()) && !pid.is_empty() {
            let _ = Command::new("kill").arg(&pid).output();
        }
    }
    // 2) wait for the port to close
    let deadline = Instant::now() + STOP_DEADLINE;
    while Instant::now() < deadline {
        if !port_open(bind)? {
            return Ok(());
        }
        std::thread::sleep(Duration::from_millis(150));
    }
    // 3) the pid file was stale (brain started manually): pkill by name.
    //    The pattern "romacode-server" cannot match this screen process
    //    (the screen binary is `romacode`), so this never kills ourselves.
    let _ = Command::new("pkill").args(["-f", "romacode-server"]).output();
    let deadline = Instant::now() + STOP_DEADLINE;
    while Instant::now() < deadline {
        if !port_open(bind)? {
            return Ok(());
        }
        std::thread::sleep(Duration::from_millis(150));
    }
    Err(format!("a brain on {bind} refused to stop — kill it manually: pkill -f romacode-server"))
}

/// Stop any brain on `bind` and start a fresh one from the binaries next to
/// (or on the PATH of) this screen. Used when the handshake reveals a
/// version mismatch: a stale daemon would keep serving a Welcome without
/// the saved providers/models, which is exactly how "Select Model" used to
/// come up empty.
pub fn restart_server(bind: &str, cwd_override: Option<&str>) -> Result<(), String> {
    stop_server(bind)?;
    ensure_server_running(bind, cwd_override)
}

/// Subcommands that belong to the brain binary rather than the screen. The
/// screen forwards them (stdio inherited, exit code preserved) so one command,
/// `romacode`, is the whole CLI: `romacode tools`, `romacode doctor`, …
pub const UTILITY_COMMANDS: &[&str] = &[
    "tools", "doctor", "github", "git", "db", "fetch", "providers", "models", "provider", "model",
    "tunnel",
];

/// Run a utility command on the brain binary and return its exit code.
pub fn run_utility(args: &[String]) -> i32 {
    let Some(bin) = locate_server_bin() else {
        eprintln!(
            "romacode: the brain binary (romacode-server) was not found on PATH or beside this binary — install it, then retry"
        );
        return 127;
    };
    match std::process::Command::new(&bin).args(args).status() {
        Ok(s) => s.code().unwrap_or(1),
        Err(e) => {
            eprintln!("romacode: could not run {}: {e}", bin.display());
            126
        }
    }
}
