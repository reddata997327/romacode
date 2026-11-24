//! Host-side Android environment bootstrap — the piece the old tree never
//! had. Every other `android_*` tool controls a target that already exists;
//! when nothing exists (no adb, no SDK, no AVD, no emulator) they can only
//! fail, and the agent was left telling the user "cannot power on". This
//! module closes that gap: one `android_setup` tool audits the host
//! environment (`status`), prescribes exact repairs (`doctor`), installs a
//! system image and creates an AVD (`create_avd`), launches the SDK emulator
//! detached (`boot`) and waits for full boot completion (`wait`).
//!
//! Design rules:
//! * Works with no live device and no `android.serial` — it never touches the
//!   bridge or the `adb` wire client; it is pure host orchestration.
//! * Every child process is bounded by a deadline and killed on expiry; the
//!   emulator itself is spawned detached (own process group, null stdio) so
//!   it outlives the tool call.
//! * Cancel-aware: polls `cancel` in every wait loop.
//! * No fabricated facts: anything not measured on this host is reported as
//!   missing/unknown, never guessed.

use serde_json::{json, Value};
use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

/// Default AVD name created by `create_avd` when none is given.
const DEFAULT_AVD_NAME: &str = "romacode";
/// Default system image: a widely mirrored, API-34 google_apis x86_64 build.
const DEFAULT_IMAGE: &str = "system-images;android-34;google_apis;x86_64";
/// Default device profile for `avdmanager create avd -d`.
const DEFAULT_DEVICE_PROFILE: &str = "pixel_5";
const PROBE_TIMEOUT: Duration = Duration::from_secs(20);
const INSTALL_TIMEOUT: Duration = Duration::from_secs(900);
const DEFAULT_BOOT_WAIT: Duration = Duration::from_secs(300);
const MAX_BOOT_WAIT: Duration = Duration::from_secs(900);
const POLL_INTERVAL: Duration = Duration::from_secs(3);

// ---------------------------------------------------------------------------
// Path resolution helpers
// ---------------------------------------------------------------------------

/// Locate `bin` on PATH. On Windows every PATHEXT executable suffix is tried;
/// elsewhere the bare name is tried once.
fn which(bin: &str) -> Option<PathBuf> {
    let path = std::env::var_os("PATH")?;
    let dirs = std::env::split_paths(&path);
    let try_one = |dir: &Path, name: &str| -> Option<PathBuf> {
        let p = dir.join(name);
        p.is_file().then_some(p)
    };
    #[cfg(windows)]
    for dir in dirs {
        if let Some(hit) = try_one(&dir, bin) {
            return Some(hit);
        }
        // PATHEXT defaults in order; a missing PATHEXT still tries .exe.
        let exts = std::env::var("PATHEXT").unwrap_or_else(|_| ".EXE;.BAT".into());
        for ext in exts.split(';').map(|e| e.trim().to_ascii_lowercase()) {
            if ext.is_empty() {
                continue;
            }
            if let Some(hit) = try_one(&dir, &format!("{bin}{ext}")) {
                return Some(hit);
            }
        }
    }
    #[cfg(not(windows))]
    for dir in dirs {
        if let Some(hit) = try_one(&dir, bin) {
            return Some(hit);
        }
    }
    None
}

/// Host SDK root candidates, most specific first. Pure — unit-tested.
fn sdk_root_candidates() -> Vec<PathBuf> {
    let mut out = Vec::new();
    for key in ["ANDROID_HOME", "ANDROID_SDK_ROOT"] {
        if let Some(v) = std::env::var_os(key) {
            let p = PathBuf::from(v);
            if p.is_dir() {
                out.push(p);
            }
        }
    }
    if let Some(home) = dirs::home_dir() {
        #[cfg(target_os = "macos")]
        out.push(home.join("Library").join("Android").join("sdk"));
        #[cfg(not(target_os = "macos"))]
        out.push(home.join("Android").join("Sdk"));
        out.push(home.join("Android").join("sdk"));
        #[cfg(windows)]
        if let Some(local) = std::env::var_os("LOCALAPPDATA") {
            out.push(PathBuf::from(local).join("Android").join("Sdk"));
        }
    }
    out
}

/// The first candidate that looks like an SDK (has platform-tools or
/// cmdline-tools / emulator / system-images children).
fn find_sdk_root() -> Option<PathBuf> {
    let looks_like_sdk = |p: &Path| {
        p.join("platform-tools").is_dir()
            || p.join("cmdline-tools").is_dir()
            || p.join("emulator").is_dir()
            || p.join("system-images").is_dir()
    };
    sdk_root_candidates().into_iter().find(|p| looks_like_sdk(p))
}

/// Everything the bootstrap needs on the host, measured right now.
struct ToolPaths {
    adb: Option<PathBuf>,
    emulator: Option<PathBuf>,
    avdmanager: Option<PathBuf>,
    sdkmanager: Option<PathBuf>,
    sdk_root: Option<PathBuf>,
}

fn tool_paths(cfg: &crate::config::Config) -> ToolPaths {
    let sdk_root = find_sdk_root();
    // adb: the configured android.adb_path wins when it exists; SDK
    // platform-tools next; bare PATH last.
    // Use the same resolver as the actual android_* tools so the audit and
    // the tool execution can never disagree about where adb lives.
    let adb = {
        let resolved = super::adb_cli::resolve_adb(&cfg.android.adb_path);
        if Path::new(&resolved).is_file() {
            Some(PathBuf::from(resolved))
        } else {
            which("adb")
        }
    };
    let in_sdk = |rel: &[&str]| -> Option<PathBuf> {
        sdk_root
            .as_ref()
            .map(|s| rel.iter().fold(s.clone(), |acc, part| acc.join(part)))
            .filter(|p| p.is_file())
    };
    let exe = |name: &str| if cfg!(windows) { format!("{name}.exe") } else { name.to_string() };
    let emulator = in_sdk(&["emulator", &exe("emulator")]).or_else(|| which("emulator"));
    let avdmanager = in_sdk(&["cmdline-tools", "latest", "bin", &exe("avdmanager")])
        .or_else(|| in_sdk(&["tools", "bin", &exe("avdmanager")]))
        .or_else(|| which("avdmanager"));
    let sdkmanager = in_sdk(&["cmdline-tools", "latest", "bin", &exe("sdkmanager")])
        .or_else(|| in_sdk(&["tools", "bin", &exe("sdkmanager")]))
        .or_else(|| which("sdkmanager"));
    ToolPaths {
        adb,
        emulator,
        avdmanager,
        sdkmanager,
        sdk_root,
    }
}

#[allow(dead_code)] // retained for the doctor text / future SDK paths
#[cfg(windows)]
fn adb_binary_name() -> &'static str {
    "adb.exe"
}

#[allow(dead_code)] // retained for the doctor text / future SDK paths
#[cfg(not(windows))]
fn adb_binary_name() -> &'static str {
    "adb"
}

// ---------------------------------------------------------------------------
// Child process helpers (bounded, drained, cancel-aware)
// ---------------------------------------------------------------------------

fn spawn_drain<S: std::io::Read + Send + 'static>(mut s: S) -> std::thread::JoinHandle<Vec<u8>> {
    std::thread::spawn(move || {
        let mut buf = Vec::new();
        let _ = s.read_to_end(&mut buf);
        buf
    })
}

fn wait_deadline(
    child: &mut std::process::Child,
    timeout: Duration,
    what: &str,
) -> Result<std::process::ExitStatus, String> {
    let deadline = Instant::now() + timeout;
    loop {
        match child.try_wait() {
            Ok(Some(st)) => return Ok(st),
            Ok(None) => {
                if Instant::now() >= deadline {
                    let _ = child.kill();
                    let _ = child.wait();
                    return Err(format!("{what} timed out after {}s", timeout.as_secs()));
                }
                std::thread::sleep(Duration::from_millis(25));
            }
            Err(e) => return Err(format!("{what}: {e}")),
        }
    }
}

/// Run one command with a hard deadline, draining both pipes.
fn run_cmd(bin: &Path, args: &[&str], timeout: Duration) -> Result<Output, String> {
    let mut child = Command::new(bin)
        .args(args)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|e| format!("could not start {}: {e}", bin.display()))?;
    let stdout_h = child.stdout.take().map(spawn_drain);
    let stderr_h = child.stderr.take().map(spawn_drain);
    let status = wait_deadline(&mut child, timeout, &bin.display().to_string())?;
    Ok(Output {
        status,
        stdout: stdout_h.map(|h| h.join().unwrap_or_default()).unwrap_or_default(),
        stderr: stderr_h.map(|h| h.join().unwrap_or_default()).unwrap_or_default(),
    })
}

fn output_text(out: &Output) -> String {
    let mut s = String::from_utf8_lossy(&out.stdout).trim().to_owned();
    let err = String::from_utf8_lossy(&out.stderr);
    if !err.trim().is_empty() {
        if !s.is_empty() {
            s.push('\n');
        }
        s.push_str(err.trim());
    }
    if s.is_empty() {
        "no diagnostic output".into()
    } else {
        s.chars().take(4000).collect()
    }
}

// ---------------------------------------------------------------------------
// Measured environment facts
// ---------------------------------------------------------------------------

/// Parse `emulator -list-avds` output (one name per line, junk tolerated).
/// Pure — unit-tested.
fn parse_avd_listing(text: &str) -> Vec<String> {
    text.lines()
        .map(str::trim)
        .filter(|l| !l.is_empty() && !l.contains(':') && !l.contains(' '))
        .map(str::to_owned)
        .collect()
}

/// Parse `adb devices -l` into (serial, state) pairs. Pure — unit-tested.
fn parse_devices_listing(text: &str) -> Vec<(String, String)> {
    text.lines()
        .skip_while(|l| !l.contains("List of devices attached"))
        .skip(1)
        .filter_map(|line| {
            let mut f = line.split_whitespace();
            let serial = f.next()?;
            let state = f.next()?.to_owned();
            (!serial.is_empty()).then(|| (serial.to_owned(), state))
        })
        .collect()
}

/// Existing AVDs: ask the emulator first (it knows its own search paths),
/// fall back to scanning ~/.android/avd.
fn list_avds(emulator: Option<&Path>) -> Vec<String> {
    if let Some(emu) = emulator {
        if let Ok(out) = run_cmd(emu, &["-list-avds"], PROBE_TIMEOUT) {
            if out.status.success() {
                let mut names = parse_avd_listing(&String::from_utf8_lossy(&out.stdout));
                names.sort();
                return names;
            }
        }
    }
    let mut names = Vec::new();
    if let Some(home) = dirs::home_dir() {
        if let Ok(entries) = std::fs::read_dir(home.join(".android").join("avd")) {
            for e in entries.flatten() {
                let name = e.file_name().to_string_lossy().into_owned();
                for suffix in [".avd", ".ini"] {
                    if let Some(stripped) = name.strip_suffix(suffix) {
                        names.push(stripped.to_owned());
                        break;
                    }
                }
            }
        }
    }
    names.sort();
    names.dedup();
    names
}

/// Installed system images as sdkmanager package ids, e.g.
/// `system-images;android-34;google_apis;x86_64`. Walks
/// `<sdk>/system-images/<api>/<variant>/<arch>` and keeps leaves holding a
/// kernel or system.img.
fn list_system_images(sdk_root: Option<&Path>) -> Vec<String> {
    let Some(root) = sdk_root else { return Vec::new() };
    let mut ids = Vec::new();
    let Ok(apis) = std::fs::read_dir(root.join("system-images")) else {
        return ids;
    };
    for api in apis.flatten() {
        let Ok(variants) = std::fs::read_dir(api.path()) else {
            continue;
        };
        for variant in variants.flatten() {
            let Ok(archs) = std::fs::read_dir(variant.path()) else {
                // Two-level layouts exist for some images; accept a leaf here.
                if variant.path().join("system.img").is_file() {
                    ids.push(format!(
                        "system-images;{};{}",
                        api.file_name().to_string_lossy(),
                        variant.file_name().to_string_lossy()
                    ));
                }
                continue;
            };
            for arch in archs.flatten() {
                let p = arch.path();
                if p.join("system.img").is_file()
                    || p.join("kernel-ranchu").is_file()
                {
                    ids.push(format!(
                        "system-images;{};{};{}",
                        api.file_name().to_string_lossy(),
                        variant.file_name().to_string_lossy(),
                        arch.file_name().to_string_lossy()
                    ));
                }
            }
        }
    }
    ids.sort();
    ids
}

/// Online / offline / unauthorized adb targets right now.
fn device_states(adb: Option<&Path>) -> Vec<(String, String)> {
    let Some(adb) = adb else { return Vec::new() };
    match run_cmd(adb, &["devices", "-l"], PROBE_TIMEOUT) {
        Ok(out) => parse_devices_listing(&String::from_utf8_lossy(&out.stdout)),
        Err(_) => Vec::new(),
    }
}

/// Hardware virtualization verdict for emulator acceleration.
fn kvm_status() -> &'static str {
    #[cfg(target_os = "linux")]
    {
        let kvm = Path::new("/dev/kvm");
        if !kvm.exists() {
            return "missing (/dev/kvm not present — install qemu-kvm / modprobe kvm_intel)";
        }
        match std::fs::OpenOptions::new().read(true).write(true).open(kvm) {
            Ok(_) => "ok (/dev/kvm writable)",
            Err(_) => {
                "not writable (/dev/kvm exists but this user lacks access — add yourself to the kvm group)"
            }
        }
    }
    #[cfg(target_os = "macos")]
    {
        "ok (Hypervisor.framework — no device node needed)"
    }
    #[cfg(windows)]
    {
        "unknown (enable Hyper-V or the Windows Hypervisor Platform feature)"
    }
    #[cfg(not(any(target_os = "linux", target_os = "macos", target_os = "windows")))]
    {
        "unknown on this platform"
    }
}

// ---------------------------------------------------------------------------
// Input validation
// ---------------------------------------------------------------------------

/// AVD names: the emulator accepts [A-Za-z0-9._-]; anything else can smuggle
/// flags or paths. A LEADING dash would parse as an emulator flag
/// (`-no-snapshot` as an AVD name), so it is rejected. Pure — unit-tested.
fn valid_avd_name(name: &str) -> bool {
    !name.is_empty()
        && name.len() <= 128
        && !name.starts_with('-')
        && name
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '.' || c == '_' || c == '-')
}

/// sdkmanager package ids like `system-images;android-34;google_apis;x86_64`.
/// Pure — unit-tested.
fn valid_package_id(id: &str) -> bool {
    !id.is_empty()
        && id.len() <= 256
        && !id.starts_with(';')
        && !id.ends_with(';')
        && !id.contains(";;")
        && id
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == ';' || c == '.' || c == '_' || c == '-')
}

// ---------------------------------------------------------------------------
// Boot polling
// ---------------------------------------------------------------------------

/// Poll until at least one emulator serial is online and
/// `sys.boot_completed == 1`. Returns (serial, waited_seconds).
fn poll_boot(
    adb: &Path,
    timeout: Duration,
    cancel: &AtomicBool,
) -> Result<(String, u64), String> {
    let start = Instant::now();
    let deadline = start + timeout;
    let mut serial_seen: Option<String> = None;
    while Instant::now() < deadline {
        if cancel.load(Ordering::SeqCst) {
            return Err("cancelled while waiting for emulator boot".into());
        }
        for (serial, state) in device_states(Some(adb)) {
            if state != "device" {
                continue;
            }
            if serial_seen.is_none() && serial.starts_with("emulator-") {
                serial_seen = Some(serial.clone());
            }
            let prop = run_cmd(
                adb,
                &["-s", &serial, "shell", "getprop", "sys.boot_completed"],
                PROBE_TIMEOUT,
            );
            if let Ok(out) = prop {
                if String::from_utf8_lossy(&out.stdout).trim() == "1" {
                    return Ok((serial, start.elapsed().as_secs()));
                }
            }
        }
        std::thread::sleep(POLL_INTERVAL);
    }
    match serial_seen {
        Some(serial) => Err(format!(
            "emulator {serial} came online but did not report sys.boot_completed=1 within {}s (first boot can be slow; run action=wait again)",
            timeout.as_secs()
        )),
        None => Err(format!(
            "no emulator appeared within {}s — check `emulator -avd <name>` output on the host",
            timeout.as_secs()
        )),
    }
}

// ---------------------------------------------------------------------------
// Tool entry point
// ---------------------------------------------------------------------------

/// Execute the `android_setup` tool. `action` selects the step; returns JSON.
pub fn execute(
    cfg: &crate::config::Config,
    input: &Value,
    cancel: &AtomicBool,
) -> Result<String, String> {
    let s = |k: &str| {
        input
            .get(k)
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .trim()
            .to_string()
    };
    let action = {
        let a = s("action");
        if a.is_empty() {
            "status".to_string()
        } else {
            a
        }
    };
    match action.as_str() {
        "status" => status(cfg),
        "doctor" => doctor(cfg),
        "create_avd" => create_avd(cfg, &s("name"), &s("image"), &s("device"), cancel),
        "boot" => boot(cfg, &s("name"), cancel),
        "wait" => {
            let adb = tool_paths(cfg).adb.ok_or(
                "adb was not found on this host; android_setup cannot watch for boot without it",
            )?;
            let timeout = boot_timeout(input);
            let (serial, waited) = poll_boot(&adb, timeout, cancel)?;
            Ok(json!({
                "booted": true,
                "serial": serial,
                "waited_secs": waited,
            })
            .to_string())
        }
        other => Err(format!(
            "android_setup: unknown action {other:?} (expected status, doctor, create_avd, boot or wait)"
        )),
    }
}

fn boot_timeout(input: &Value) -> Duration {
    let secs = input
        .get("timeout")
        .and_then(|v| v.as_i64())
        .unwrap_or(DEFAULT_BOOT_WAIT.as_secs() as i64)
        .clamp(30, MAX_BOOT_WAIT.as_secs() as i64) as u64;
    Duration::from_secs(secs)
}

/// Full environment audit. Never fails — gaps are facts, not errors.
fn status(cfg: &crate::config::Config) -> Result<String, String> {
    let t = tool_paths(cfg);
    let avds = list_avds(t.emulator.as_deref());
    let images = list_system_images(t.sdk_root.as_deref());
    let devices = device_states(t.adb.as_deref());
    let online: Vec<&(String, String)> = devices.iter().filter(|(_, st)| st == "device").collect();
    let p = |tool: &Option<PathBuf>| tool.as_ref().map(|x| x.display().to_string());
    Ok(json!({
        "adb": p(&t.adb),
        "sdk_root": p(&t.sdk_root),
        "emulator": p(&t.emulator),
        "avdmanager": p(&t.avdmanager),
        "sdkmanager": p(&t.sdkmanager),
        "avds": avds,
        "system_images": images,
        "devices": devices.iter().map(|(s, st)| json!({"serial": s, "state": st})).collect::<Vec<_>>(),
        "online_targets": online.iter().map(|(s, _)| s.clone()).collect::<Vec<_>>(),
        "acceleration": kvm_status(),
        "ready": online.len() == 1,
        "next": next_step(&t, &avds, &images, &online, false),
    })
    .to_string())
}

/// Status plus the exact host commands that close each gap.
fn doctor(cfg: &crate::config::Config) -> Result<String, String> {
    let t = tool_paths(cfg);
    let avds = list_avds(t.emulator.as_deref());
    let images = list_system_images(t.sdk_root.as_deref());
    let devices = device_states(t.adb.as_deref());
    let online: Vec<&(String, String)> = devices.iter().filter(|(_, st)| st == "device").collect();
    let mut remedies: Vec<Value> = Vec::new();
    if t.adb.is_none() {
        remedies.push(json!({
            "gap": "adb (Android Platform-Tools)",
            "how": [
                "Ubuntu/Debian: sudo apt install google-android-platform-tools-installer (or download Platform-Tools and add to PATH)",
                "Fedora: sudo dnf install android-tools",
                "macOS: brew install --cask android-platform-tools",
                "Windows: choco install adb; or fetch Platform-Tools and add to PATH"
            ]
        }));
    }
    if t.sdkmanager.is_none() || t.avdmanager.is_none() {
        remedies.push(json!({
            "gap": "cmdline-tools (sdkmanager/avdmanager)",
            "how": [
                "Install Android SDK command-line tools, then: sdkmanager --install \"cmdline-tools;latest\" \"emulator\" \"platform-tools\"",
                "Set ANDROID_HOME to the SDK root afterwards"
            ]
        }));
    }
    if t.emulator.is_none() {
        remedies.push(json!({
            "gap": "emulator binary",
            "how": ["sdkmanager --install \"emulator\""]
        }));
    }
    match kvm_status() {
        s if s.starts_with("missing") || s.starts_with("not writable") => {
            remedies.push(json!({
                "gap": "hardware acceleration",
                "how": [
                    format!("Linux: sudo apt install qemu-kvm && sudo adduser $USER kvm (then re-login); current verdict: {s}"),
                    "Emulator can still boot with software acceleration (-accel off) but it is slow"
                ]
            }));
        }
        _ => {}
    }
    if images.is_empty() {
        remedies.push(json!({
            "gap": "system image",
            "how": [
                format!("sdkmanager --install \"{DEFAULT_IMAGE}\""),
                "or run android_setup create_avd, which installs the image first"
            ]
        }));
    }
    if avds.is_empty() {
        remedies.push(json!({
            "gap": "no AVD",
            "how": ["android_setup {\"action\": \"create_avd\"} (installs the image, then creates the AVD)"]
        }));
    }
    if online.is_empty() && !avds.is_empty() && t.emulator.is_some() {
        remedies.push(json!({
            "gap": "no target online",
            "how": ["android_setup {\"action\": \"boot\", \"name\": \"<avd>\"}"]
        }));
    }
    if remedies.is_empty() {
        remedies.push(json!({
            "gap": "none",
            "how": ["environment looks complete; boot or connect a target and continue with android_device"]
        }));
    }
    Ok(json!({
        "adb": p(&t.adb),
        "sdk_root": p(&t.sdk_root),
        "avds": avds,
        "system_images": images,
        "devices": devices.iter().map(|(s, st)| json!({"serial": s, "state": st})).collect::<Vec<_>>(),
        "acceleration": kvm_status(),
        "remedies": remedies,
    })
    .to_string())
}

fn p(tool: &Option<PathBuf>) -> Value {
    match tool {
        Some(x) => json!(x.display().to_string()),
        None => Value::Null,
    }
}

/// The single next recommended step, phrased as a tool call the agent can run
/// verbatim. Pure — unit-tested.
fn next_step(
    t: &ToolPaths,
    avds: &[String],
    images: &[String],
    online: &[&(String, String)],
    _quiet: bool,
) -> String {
    if online.len() == 1 {
        return "target ready — call android_device".into();
    }
    if !online.is_empty() {
        return "multiple targets online — set android.serial in config.json to one exact serial"
            .into();
    }
    if t.adb.is_none() || t.sdkmanager.is_none() || t.avdmanager.is_none() || t.emulator.is_none() {
        return "run android_setup {\"action\": \"doctor\"} for the exact install commands"
            .into();
    }
    if avds.is_empty() {
        if images.is_empty() {
            return "run android_setup {\"action\": \"create_avd\"} — it installs the system image and creates the AVD"
                .into();
        }
        return "run android_setup {\"action\": \"create_avd\"}".into();
    }
    format!(
        "run android_setup {{\"action\": \"boot\", \"name\": \"{}\"}}",
        avds[0]
    )
}

/// Install the system image when missing, then create the AVD.
fn create_avd(
    cfg: &crate::config::Config,
    name_in: &str,
    image_in: &str,
    device_in: &str,
    cancel: &AtomicBool,
) -> Result<String, String> {
    let t = tool_paths(cfg);
    let sdkmanager = t
        .sdkmanager
        .as_ref()
        .ok_or("sdkmanager not found; run android_setup {\"action\": \"doctor\"} for install steps")?;
    let avdmanager = t
        .avdmanager
        .as_ref()
        .ok_or("avdmanager not found; run android_setup {\"action\": \"doctor\"} for install steps")?;
    let sdk_root = t
        .sdk_root
        .clone()
        .ok_or("Android SDK root not found; install cmdline-tools and set ANDROID_HOME")?;

    let name = if name_in.is_empty() {
        DEFAULT_AVD_NAME.to_string()
    } else {
        name_in.to_string()
    };
    if !valid_avd_name(&name) {
        return Err(format!(
            "android_setup: AVD name {name:?} is invalid — use only letters, digits, '.', '_' or '-'"
        ));
    }
    let image = if image_in.is_empty() {
        DEFAULT_IMAGE.to_string()
    } else {
        image_in.to_string()
    };
    if !valid_package_id(&image) {
        return Err(format!(
            "android_setup: package id {image:?} is invalid — expected e.g. {DEFAULT_IMAGE}"
        ));
    }
    let device = if device_in.is_empty() {
        DEFAULT_DEVICE_PROFILE.to_string()
    } else {
        device_in.to_string()
    };
    if !valid_package_id(&device) {
        return Err(format!(
            "android_setup: device profile {device:?} is invalid — use an `avdmanager list device` id like {DEFAULT_DEVICE_PROFILE}"
        ));
    }

    // 1) system image present?
    let images = list_system_images(Some(&sdk_root));
    let mut steps: Vec<String> = Vec::new();
    if !images.iter().any(|id| *id == image) {
        if cancel.load(Ordering::SeqCst) {
            return Err("cancelled before the system image install".into());
        }
        steps.push(format!("sdkmanager --install {image:?}"));
        let out = run_cmd(sdkmanager, &["--install", &image], INSTALL_TIMEOUT)?;
        if !out.status.success() {
            return Err(format!(
                "sdkmanager failed to install {image}: {} (accept licenses with `yes | sdkmanager --licenses`)",
                output_text(&out)
            ));
        }
        let after = list_system_images(Some(&sdk_root));
        if !after.iter().any(|id| *id == image) {
            return Err(format!(
                "sdkmanager exited 0 but {image} is not on disk; re-run android_setup {{\"action\": \"status\"}}"
            ));
        }
        steps.push(format!("installed {image}"));
    } else {
        steps.push(format!("system image {image} already present"));
    }

    if cancel.load(Ordering::SeqCst) {
        return Err("cancelled before avdmanager".into());
    }

    // 2) create the AVD. avdmanager asks "Do you want to create a custom
    //    hardware profile? [no]" on stdin — answer no.
    let mut child = Command::new(avdmanager)
        .args(["create", "avd", "-n", &name, "-k", &image, "-d", &device, "--force"])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .env("ANDROID_HOME", &sdk_root)
        .env("ANDROID_SDK_ROOT", &sdk_root)
        .spawn()
        .map_err(|e| format!("could not start avdmanager: {e}"))?;
    if let Some(mut stdin) = child.stdin.take() {
        use std::io::Write;
        let _ = stdin.write_all(b"no\n");
        drop(stdin);
    }
    let stdout_h = child.stdout.take().map(spawn_drain);
    let stderr_h = child.stderr.take().map(spawn_drain);
    let status = wait_deadline(&mut child, PROBE_TIMEOUT, "avdmanager create avd")?;
    let out = Output {
        status,
        stdout: stdout_h.map(|h| h.join().unwrap_or_default()).unwrap_or_default(),
        stderr: stderr_h.map(|h| h.join().unwrap_or_default()).unwrap_or_default(),
    };
    if !out.status.success() {
        return Err(format!(
            "avdmanager create avd failed: {}",
            output_text(&out)
        ));
    }
    steps.push(format!("created AVD {name} ({image}, {device})"));

    let avds = list_avds(t.emulator.as_deref());
    if !avds.iter().any(|a| *a == name) {
        return Err(format!(
            "avdmanager exited 0 but AVD {name} is not listed; re-run android_setup {{\"action\": \"status\"}}"
        ));
    }
    Ok(json!({
        "created": name,
        "image": image,
        "device_profile": device,
        "steps": steps,
        "avds": avds,
        "next": format!("run android_setup {{\"action\": \"boot\", \"name\": \"{name}\"}}"),
    })
    .to_string())
}

/// Launch the SDK emulator detached, then wait for full boot.
fn boot(
    cfg: &crate::config::Config,
    name: &str,
    cancel: &AtomicBool,
) -> Result<String, String> {
    let t = tool_paths(cfg);
    let emulator = t
        .emulator
        .as_ref()
        .ok_or("emulator binary not found; run android_setup {\"action\": \"doctor\"} for install steps")?;
    let adb = t
        .adb
        .as_ref()
        .ok_or("adb not found; android_setup boot needs Platform-Tools to watch the boot")?;
    let avds = list_avds(Some(emulator));
    let name = if name.is_empty() {
        avds.first().ok_or(
            "no AVD exists yet; run android_setup {\"action\": \"create_avd\"} first (it installs a system image and creates the AVD)",
        )?
        .clone()
    } else {
        if !valid_avd_name(name) {
            return Err(format!("android_setup: AVD name {name:?} is invalid"));
        }
        if !avds.iter().any(|a| a == name) {
            return Err(format!(
                "no AVD named {name}; existing AVDs: {}",
                if avds.is_empty() { "none".to_string() } else { avds.join(", ") }
            ));
        }
        name.to_string()
    };
    // Never boot a second emulator for an AVD that is already up.
    let already = device_states(Some(adb));
    let running: Vec<String> = already
        .iter()
        .filter(|(s, st)| st == "device" && s.starts_with("emulator-"))
        .map(|(s, _)| s.clone())
        .collect();
    if !running.is_empty() {
        return Ok(json!({
            "booted": true,
            "serial": running[0],
            "already_running": true,
            "note": format!("an emulator is already online ({})", running.join(", ")),
        })
        .to_string());
    }

    if cancel.load(Ordering::SeqCst) {
        return Err("cancelled before the emulator launch".into());
    }

    let sdk_root = t.sdk_root.clone();
    let mut cmd = Command::new(emulator);
    cmd.arg("-avd")
        .arg(&name)
        .args(["-no-snapshot-load", "-no-boot-anim", "-gpu", "auto"]);
    if let Some(root) = &sdk_root {
        // The emulator discovers the SDK itself, but keep ANDROID_HOME /
        // ANDROID_SDK_ROOT consistent for the child when we know the root.
        if std::env::var_os("ANDROID_HOME").is_none() {
            cmd.env("ANDROID_HOME", root);
        }
        if std::env::var_os("ANDROID_SDK_ROOT").is_none() {
            cmd.env("ANDROID_SDK_ROOT", root);
        }
    }
    cmd.stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    #[cfg(unix)]
    {
        // Detach from RomaCode's process group: the emulator must survive
        // this tool call and the server process itself.
        use std::os::unix::process::CommandExt;
        cmd.process_group(0);
    }
    let mut child = cmd
        .spawn()
        .map_err(|e| format!("could not start the emulator for AVD {name}: {e}"))?;
    let pid = child.id();
    // Reap the emulator when it eventually exits: dropping the handle
    // without wait left a zombie in RomaCode's process table for as long
    // as the server ran.
    std::thread::Builder::new()
        .name("emulator-reaper".into())
        .spawn(move || {
            let _ = child.wait();
        })
        .ok();

    let timeout = DEFAULT_BOOT_WAIT;
    let (serial, waited) = poll_boot(adb, timeout, cancel)?;
    Ok(json!({
        "booted": true,
        "serial": serial,
        "avd": name,
        "emulator_pid": pid,
        "waited_secs": waited,
        "detached": true,
        "next": "call android_device to read the real device properties",
    })
    .to_string())
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn avd_listing_parse_keeps_only_names() {
        let text = "studio\nPixel_5_API_34\n\nromacode\n";
        assert_eq!(
            parse_avd_listing(text),
            vec!["studio", "Pixel_5_API_34", "romacode"]
        );
        // command noise lines carry spaces or colons and are dropped
        assert_eq!(parse_avd_listing("PANIC: broken\nemulator: INFO\nok\n"), vec!["ok"]);
    }

    #[test]
    fn devices_listing_parse_handles_states() {
        let text = "List of devices attached\nemulator-5554\tdevice product:sdk_gphone64_x86_64 model:sdk_gphone64_x86_64 device:emu64xa transport_id:1\n192.168.1.42:5555\toffline\nABC123\tunauthorized\n";
        let devs = parse_devices_listing(text);
        assert_eq!(devs.len(), 3);
        assert_eq!(devs[0], ("emulator-5554".into(), "device".into()));
        assert_eq!(devs[1], ("192.168.1.42:5555".into(), "offline".into()));
        assert_eq!(devs[2], ("ABC123".into(), "unauthorized".into()));
    }

    #[test]
    fn avd_names_reject_flag_and_path_injections() {
        assert!(valid_avd_name("romacode"));
        assert!(valid_avd_name("Pixel_5-API.34"));
        assert!(!valid_avd_name(""));
        assert!(!valid_avd_name("-no-snapshot")); // leading dash = emulator flag
        assert!(!valid_avd_name("a b"));
        assert!(!valid_avd_name("../escape"));
        assert!(!valid_avd_name("x;y"));
    }

    #[test]
    fn package_ids_accept_sdkmanager_shape_only() {
        assert!(valid_package_id("system-images;android-34;google_apis;x86_64"));
        assert!(valid_package_id("pixel_5"));
        assert!(valid_package_id("platform-tools"));
        assert!(!valid_package_id(""));
        assert!(!valid_package_id(";system-images"));
        assert!(!valid_package_id("system-images;"));
        assert!(!valid_package_id("a;;b"));
        assert!(!valid_package_id("img\nnext"));
        assert!(!valid_package_id("a b"));
    }

    #[test]
    fn next_step_walks_the_ladder() {
        let t_missing = ToolPaths {
            adb: None,
            emulator: None,
            avdmanager: None,
            sdkmanager: None,
            sdk_root: None,
        };
        assert!(next_step(&t_missing, &[], &[], &[], false).contains("doctor"));
        let t_full = ToolPaths {
            adb: Some(PathBuf::from("/usr/bin/adb")),
            emulator: Some(PathBuf::from("/sdk/emulator/emulator")),
            avdmanager: Some(PathBuf::from("/sdk/cmdline-tools/latest/bin/avdmanager")),
            sdkmanager: Some(PathBuf::from("/sdk/cmdline-tools/latest/bin/sdkmanager")),
            sdk_root: Some(PathBuf::from("/sdk")),
        };
        assert!(next_step(&t_full, &[], &[], &[], false).contains("create_avd"));
        let out = next_step(&t_full, &["romacode".into()], &["system-images;android-34;google_apis;x86_64".into()], &[], false);
        assert!(out.contains("boot") && out.contains("romacode"), "{out}");
        let one = [("emulator-5554".to_string(), "device".to_string())];
        let refs: Vec<&(String, String)> = one.iter().collect();
        assert!(next_step(&t_full, &["romacode".into()], &[], &refs, false).contains("android_device"));
    }

    #[test]
    fn sdk_root_candidates_never_panic_without_env() {
        // The function must be callable on any host and return a Vec.
        let _ = sdk_root_candidates();
    }

    #[cfg(unix)]
    #[test]
    fn run_cmd_kills_hung_children() {
        use std::os::unix::fs::PermissionsExt;
        let dir = std::env::temp_dir().join(format!("romacode-setup-test-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let sleepy = dir.join("sleepy");
        std::fs::write(&sleepy, "#!/bin/sh\nsleep 60\n").unwrap();
        std::fs::set_permissions(&sleepy, std::fs::Permissions::from_mode(0o700)).unwrap();
        let started = Instant::now();
        let err = run_cmd(&sleepy, &[], Duration::from_secs(2)).unwrap_err();
        assert!(err.contains("timed out"), "{err}");
        assert!(started.elapsed() < Duration::from_secs(10));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn status_and_doctor_work_on_a_bare_host() {
        // The exact screenshot scenario: no adb, no SDK, no AVD. status and
        // doctor must still return valid JSON facts and remedies — never a
        // panic, never fabricated paths.
        let cfg = crate::config::Config::default();
        let cancel = AtomicBool::new(false);
        let st = execute(&cfg, &json!({}), &cancel).unwrap();
        let v: Value = serde_json::from_str(&st).expect("status must be valid JSON");
        assert_eq!(v["ready"], false, "a bare host has no online target");
        assert!(v["adb"].is_null() || v["next"].as_str().unwrap().contains("doctor") || v["next"].as_str().unwrap().contains("create_avd"));
        assert!(v["next"].as_str().is_some());
        let doc = execute(&cfg, &json!({"action": "doctor"}), &cancel).unwrap();
        let dv: Value = serde_json::from_str(&doc).expect("doctor must be valid JSON");
        let remedies = dv["remedies"].as_array().expect("remedies array");
        assert!(!remedies.is_empty(), "a bare host always has at least one gap");
        // unknown actions refuse with a name, not a panic
        let err = execute(&cfg, &json!({"action": "install_everything_magic"}), &cancel).unwrap_err();
        assert!(err.contains("unknown action"), "{err}");
    }

    #[test]
    fn registry_dispatches_android_setup_without_a_device() {
        // End-to-end through the single dispatch point: the tool must reach
        // setup::execute even when the Android bridge is None — that is the
        // whole point of a bootstrap tool.
        let cfg = crate::config::Config::default();
        let cancel = AtomicBool::new(false);
        let out = crate::tools::execute(&cfg, "android_setup", &json!({"action": "status"}), None, &cancel).unwrap();
        let v: Value = serde_json::from_str(&out).expect("valid JSON through the registry");
        assert_eq!(v["ready"], false);
        // and the definitions list contains it, so both engines see it
        let names: Vec<String> = crate::tools::definitions(&cfg, true)
            .into_iter()
            .filter_map(|d| d["name"].as_str().map(str::to_owned))
            .collect();
        assert!(names.contains(&"android_setup".to_string()), "android_setup missing from registry: {names:?}");
    }
}
