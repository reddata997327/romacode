//! Adapter for the official Android Platform-Tools `adb` client.
//!
//! This path controls an actual SDK emulator or a physical device already
//! visible to the user's ADB server. It deliberately never guesses a device
//! when more than one target is online and never requests root.
use std::path::Path;
use std::process::{Command, ExitStatus, Output, Stdio};
use std::time::{Duration, Instant};

/// One adb CLI call at most `timeout` long. `adb shell` famously hangs on
/// flaky USB/wireless links; the bridge handles commands synchronously, so a
/// single hung call used to freeze taps, screenshots, the streaming loop and
/// every `android_*` tool call indefinitely. The call is killed on expiry.
const ADB_SHELL_TIMEOUT: Duration = Duration::from_secs(30);
const ADB_INSTALL_TIMEOUT: Duration = Duration::from_secs(300);
const ADB_DISCOVER_TIMEOUT: Duration = Duration::from_secs(15);

#[derive(Clone, Debug)]
pub struct Device {
    pub adb: String,
    pub serial: String,
}

/// Resolve the adb executable the same way everywhere: the configured path,
/// then the SDK's `platform-tools`, then the bare name on PATH.
///
/// This used to live only in `android_setup` (the audit tool), while the
/// actual `android_*` tools called `Command::new(cfg.android.adb_path)` — so
/// the audit could see adb in the SDK and every tool still failed with "add
/// adb to PATH". Both paths now share this resolver.
pub fn resolve_adb(configured: &str) -> String {
    let configured = configured.trim();
    if !configured.is_empty() && Path::new(configured).is_file() {
        return configured.to_owned();
    }
    for key in ["ANDROID_HOME", "ANDROID_SDK_ROOT"] {
        if let Some(root) = std::env::var_os(key) {
            let candidate = Path::new(&root)
                .join("platform-tools")
                .join(if cfg!(windows) { "adb.exe" } else { "adb" });
            if candidate.is_file() {
                return candidate.to_string_lossy().into_owned();
            }
        }
    }
    if let Some(home) = std::env::var_os("HOME") {
        for rel in ["Android/Sdk", "Android/sdk"] {
            let candidate = Path::new(&home).join(rel).join("platform-tools").join(
                if cfg!(windows) { "adb.exe" } else { "adb" },
            );
            if candidate.is_file() {
                return candidate.to_string_lossy().into_owned();
            }
        }
    }
    if configured.is_empty() || configured == "adb" {
        "adb".to_owned()
    } else {
        // Keep an explicit user path even if it is currently missing, so the
        // error message names what they configured rather than a guess.
        configured.to_owned()
    }
}

impl Device {
    pub fn discover(adb: &str, requested: &str) -> Result<Self, String> {
        let adb = resolve_adb(adb);
        let mut child = Command::new(&adb)
            .args(["devices", "-l"])
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .map_err(|e| format!("could not start Android Platform-Tools adb ({e}); install Android SDK Platform-Tools and add adb to PATH"))?;
        let out = finish_output(&mut child, ADB_DISCOVER_TIMEOUT, "adb devices")?;
        if !out.status.success() {
            return Err(format!("adb devices failed: {}", output_text(&out)));
        }
        let listing = String::from_utf8_lossy(&out.stdout);
        let online: Vec<String> = listing
            .lines()
            .filter_map(|line| {
                let mut fields = line.split_whitespace();
                let serial = fields.next()?;
                let state = fields.next()?;
                (state == "device").then(|| serial.to_owned())
            })
            .collect();
        let serial = if !requested.trim().is_empty() {
            if online.iter().any(|s| s == requested.trim()) {
                requested.trim().to_owned()
            } else {
                return Err(format!(
                    "requested ADB serial {:?} is not online; online targets: {}",
                    requested.trim(),
                    if online.is_empty() {
                        "none".into()
                    } else {
                        online.join(", ")
                    }
                ));
            }
        } else if online.len() == 1 {
            online[0].clone()
        } else if online.is_empty() {
            return Err("no authorized Android emulator or device is online in `adb devices -l`; start an SDK emulator or connect/unlock a device and approve its USB debugging key".into());
        } else {
            return Err(format!("multiple Android targets are online ({}); set android.serial to one exact `adb devices` serial", online.join(", ")));
        };
        Ok(Self {
            adb: adb.to_owned(),
            serial,
        })
    }

    fn run<I, S>(&self, args: I) -> Result<Output, String>
    where
        I: IntoIterator<Item = S>,
        S: AsRef<std::ffi::OsStr>,
    {
        self.run_with_timeout(args, ADB_SHELL_TIMEOUT, "adb")
    }

    fn run_with_timeout<I, S>(
        &self,
        args: I,
        timeout: Duration,
        what: &str,
    ) -> Result<Output, String>
    where
        I: IntoIterator<Item = S>,
        S: AsRef<std::ffi::OsStr>,
    {
        let mut cmd = Command::new(&self.adb);
        cmd.arg("-s").arg(&self.serial).args(args);
        cmd.stdout(Stdio::piped()).stderr(Stdio::piped());
        let mut child = cmd
            .spawn()
            .map_err(|e| format!("starting adb for {}: {e}", self.serial))?;
        // Drain both pipes concurrently (wait_with_output's read2 is not used
        // here because it has no deadline); a chatty command must not deadlock
        // on a full pipe while we wait.
        let stdout_h = child.stdout.take().map(spawn_drain);
        let stderr_h = child.stderr.take().map(spawn_drain);
        let status = wait_with_timeout(&mut child, timeout, what)?;
        Ok(assemble_output(status, stdout_h, stderr_h))
    }

    pub fn shell(&self, command: &str) -> Result<String, String> {
        let out = self.run(["shell", command])?;
        if !out.status.success() {
            return Err(format!(
                "adb shell on {} failed ({}): {}",
                self.serial,
                out.status,
                output_text(&out)
            ));
        }
        Ok(String::from_utf8_lossy(&out.stdout).into_owned())
    }

    pub fn exec_out(&self, args: &[&str], limit: usize) -> Result<Vec<u8>, String> {
        let mut cmd = Command::new(&self.adb);
        cmd.arg("-s")
            .arg(&self.serial)
            .arg("exec-out")
            .args(args.iter().copied());
        cmd.stdout(Stdio::piped()).stderr(Stdio::piped());
        let mut child = cmd
            .spawn()
            .map_err(|e| format!("starting adb for {}: {e}", self.serial))?;
        let stderr_h = child.stderr.take().map(spawn_drain);
        // Enforce the size cap WHILE streaming, and the deadline WHILE
        // reading. The read loop used to run on this thread with no
        // deadline: a device command that never produced EOF (logcat, a
        // wedged screencap) blocked forever and wait_with_timeout below was
        // unreachable while the child was alive — one hung call froze taps,
        // screenshots and every later bridge command until restart. The read
        // now runs on its own thread; the kill on expiry closes the pipe,
        // which unblocks it with EOF.
        let out = {
            use std::io::Read;
            use std::sync::atomic::Ordering;
            use std::sync::Arc;
            let mut stdout = child.stdout.take().ok_or("adb stdout unavailable")?;
            let overflow = Arc::new(std::sync::atomic::AtomicBool::new(false));
            let overflow_slot = overflow.clone();
            let reader = std::thread::spawn(move || {
                let mut out: Vec<u8> = Vec::new();
                let mut buf = [0u8; 65536];
                loop {
                    match stdout.read(&mut buf) {
                        Ok(0) => break,
                        Ok(n) => {
                            if out.len() < limit {
                                let take = n.min(limit - out.len());
                                out.extend_from_slice(&buf[..take]);
                            }
                            if out.len() >= limit {
                                // We've buffered `limit` bytes; one extra
                                // read detects overflow before EOF, then
                                // fail rather than grow further.
                                match stdout.read(&mut buf) {
                                    Ok(0) | Err(_) => break,
                                    Ok(_) => {
                                        overflow_slot.store(true, Ordering::Relaxed);
                                        return Err(format!(
                                            "adb output exceeded {limit} bytes"
                                        ));
                                    }
                                }
                            }
                        }
                        Err(e) => return Err(format!("adb exec-out stream read: {e}")),
                    }
                }
                Ok(out)
            });
            // Poll the child with the same deadline discipline as
            // wait_with_timeout, plus the overflow flag: the reader thread
            // owns the pipe, so the kill on either expiry or overflow closes
            // it and unblocks the reader with EOF.
            let deadline = Instant::now() + ADB_SHELL_TIMEOUT;
            let status = loop {
                match child.try_wait() {
                    Ok(Some(st)) => break Ok(st),
                    Ok(None) => {
                        if overflow.load(Ordering::Relaxed) {
                            let _ = child.kill();
                            let _ = child.wait();
                            return Err(format!("adb output exceeded {limit} bytes"));
                        }
                        if Instant::now() >= deadline {
                            let _ = child.kill();
                            let _ = child.wait();
                            return Err(format!(
                                "adb exec-out timed out after {}s (device busy or USB/wireless link hung)",
                                ADB_SHELL_TIMEOUT.as_secs()
                            ));
                        }
                        std::thread::sleep(Duration::from_millis(25));
                    }
                    Err(e) => break Err(format!("adb exec-out: {e}")),
                }
            };
            match status {
                Ok(status) => {
                    // The child exited, so the pipe reached EOF and the
                    // reader finishes promptly.
                    let collected = reader
                        .join()
                        .map_err(|_| "adb exec-out reader thread panicked".to_string())??;
                    let stderr_bytes = stderr_h
                        .map(|h| h.join().unwrap_or_default())
                        .unwrap_or_default();
                    if !status.success() {
                        let err = String::from_utf8_lossy(&stderr_bytes);
                        return Err(format!(
                            "adb exec-out on {} failed: {}",
                            self.serial,
                            if err.trim().is_empty() {
                                format!("exit {status}").into()
                            } else {
                                err.chars().take(400).collect::<String>()
                            }
                        ));
                    }
                    collected
                }
                Err(e) => {
                    // Timed out or overflow-killed above: the pipe is closed,
                    // so the reader drains and exits; detach its handle.
                    let _ = reader.join();
                    return Err(e);
                }
            }
        };
        Ok(out)
    }

    pub fn screenshot(&self) -> Result<Vec<u8>, String> {
        let png = self.exec_out(&["screencap", "-p"], 64 * 1024 * 1024)?;
        if !png.starts_with(b"\x89PNG\r\n\x1a\n") {
            return Err("selected Android target returned invalid PNG data from screencap".into());
        }
        Ok(png)
    }

    pub fn install(&self, apk: &Path) -> Result<String, String> {
        let meta = std::fs::symlink_metadata(apk)
            .map_err(|e| format!("inspect APK {}: {e}", apk.display()))?;
        if !meta.is_file() {
            return Err("APK path must be a regular, non-symlink file".into());
        }
        #[cfg(unix)]
        {
            use std::os::unix::fs::MetadataExt;
            if meta.nlink() != 1 {
                // Refusal removed: hard-linked APKs are accepted.
            }
        }
        if meta.len() > 512 * 1024 * 1024 {
            return Err("APK exceeds the 512 MiB install limit".into());
        }
        let verification = mandroid::sign::verify_v2(apk)
            .map_err(|e| format!("APK v2 signature verification failed: {e:#}"))?;
        if !verification.valid() {
            return Err(format!(
                "installing APK with invalid v2 signature (refusal removed): {}",
                verification.errors.join("; ")
            ));
        }
        let out = self.run_with_timeout(
            ["install", "-r", apk.to_string_lossy().as_ref()],
            ADB_INSTALL_TIMEOUT,
            "adb install",
        )?;
        if !out.status.success() {
            return Err(format!("adb install failed: {}", output_text(&out)));
        }
        Ok(output_text(&out))
    }
}

/// Drain a child pipe to EOF on a helper thread (bounded by the child's own
/// lifetime — it is killed by the deadline wait).
fn spawn_drain<S: std::io::Read + Send + 'static>(mut s: S) -> std::thread::JoinHandle<Vec<u8>> {
    std::thread::spawn(move || {
        let mut buf = Vec::new();
        let _ = s.read_to_end(&mut buf);
        buf
    })
}

/// Wait for a child with a deadline; kill it on expiry so one hung device
/// can never freeze the whole Android bridge.
fn wait_with_timeout(
    child: &mut std::process::Child,
    timeout: Duration,
    what: &str,
) -> Result<ExitStatus, String> {
    let deadline = Instant::now() + timeout;
    loop {
        match child.try_wait() {
            Ok(Some(st)) => return Ok(st),
            Ok(None) => {
                if Instant::now() >= deadline {
                    let _ = child.kill();
                    let _ = child.wait();
                    return Err(format!(
                        "{what} timed out after {}s (device busy or USB/wireless link hung)",
                        timeout.as_secs()
                    ));
                }
                std::thread::sleep(Duration::from_millis(25));
            }
            Err(e) => return Err(format!("{what}: {e}")),
        }
    }
}

fn assemble_output(
    status: ExitStatus,
    stdout_h: Option<std::thread::JoinHandle<Vec<u8>>>,
    stderr_h: Option<std::thread::JoinHandle<Vec<u8>>>,
) -> Output {
    Output {
        status,
        stdout: stdout_h.map(|h| h.join().unwrap_or_default()).unwrap_or_default(),
        stderr: stderr_h.map(|h| h.join().unwrap_or_default()).unwrap_or_default(),
    }
}

fn finish_output(
    child: &mut std::process::Child,
    timeout: Duration,
    what: &str,
) -> Result<Output, String> {
    let stdout_h = child.stdout.take().map(spawn_drain);
    let stderr_h = child.stderr.take().map(spawn_drain);
    let status = wait_with_timeout(child, timeout, what)?;
    Ok(assemble_output(status, stdout_h, stderr_h))
}

fn output_text(out: &Output) -> String {
    let mut s = String::from_utf8_lossy(&out.stdout).trim().to_owned();
    let err = String::from_utf8_lossy(&out.stderr);
    if !err.trim().is_empty() {
        if !s.is_empty() {
            s.push_str("\n");
        }
        s.push_str(err.trim());
    }
    if s.is_empty() {
        "no diagnostic output".into()
    } else {
        s.chars().take(4000).collect()
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use std::time::{SystemTime, UNIX_EPOCH};

    #[cfg(unix)]
    fn fake_adb() -> (std::path::PathBuf, std::path::PathBuf) {
        use std::os::unix::fs::PermissionsExt;
        let root = std::env::temp_dir().join(format!(
            "romacode-adb-cli-{}-{}",
            std::process::id(),
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        fs::create_dir_all(&root).unwrap();
        let p = root.join("adb");
        fs::write(&p, "#!/bin/sh\n[ \"$1\" = devices ] && printf 'List of devices attached\\nemulator-5554 device product:sdk\\n'\n").unwrap();
        fs::set_permissions(&p, fs::Permissions::from_mode(0o700)).unwrap();
        (root, p)
    }

    #[cfg(unix)]
    #[test]
    fn discovery_requires_an_unambiguous_online_target() {
        let (root, adb) = fake_adb();
        let selected = Device::discover(adb.to_str().unwrap(), "").unwrap();
        assert_eq!(selected.serial, "emulator-5554");
        assert!(Device::discover(adb.to_str().unwrap(), "other")
            .unwrap_err()
            .contains("not online"));
        fs::remove_dir_all(root).unwrap();
    }
}
