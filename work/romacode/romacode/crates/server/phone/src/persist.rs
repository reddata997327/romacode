//! Persistence — `state.json`, the sdcard directory and the authorised ADB
//! key store. Everything the phone remembers lives in one data directory:
//!
//! ```text
//! <data_dir>/
//!   state.json      packages, messages, calls, gallery, settings, battery
//!   adb_keys/       base64 public-key blobs of authorised adb clients
//!   sdcard/         the emulated /sdcard, backed by real files
//!   photos/         the gallery (real PNGs)
//!   cache/          browser downloads and staged APKs
//! ```

use std::collections::{BTreeMap, VecDeque};
use std::path::{Component, Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::SystemTime;

use anyhow::{anyhow, Context, Result};

use crate::state::*;

#[derive(serde::Serialize, serde::Deserialize, Default)]
struct Persisted {
    user_packages: Vec<PersistPackage>,
    sms: Vec<SmsThread>,
    calls: Vec<CallEntry>,
    photos: Vec<PhotoEntry>,
    settings: BTreeMap<String, String>,
    battery: Option<u8>,
    charging: Option<bool>,
    /// The dual-eSIM registry (registered profiles, enabled state, data slot).
    esim: Option<mandroid::identity::EsimRegistry>,
    airplane_mode: Option<bool>,
    mobile_data: Option<bool>,
}

#[derive(serde::Serialize, serde::Deserialize)]
struct PersistPackage {
    package: String,
    label: String,
    version: String,
}

pub fn data_dir() -> PathBuf {
    if let Ok(d) = std::env::var("ROMACODE_ANDROID_DIR") {
        return PathBuf::from(d);
    }
    if let Some(home) = std::env::var_os("HOME") {
        return PathBuf::from(home).join(".romacode").join("android");
    }
    PathBuf::from(".romacode-android")
}

/// Resolve a relative path beneath `root` without accepting traversal or any
/// existing symlink in the path. Canonicalizing the root makes a configured
/// root symlink an explicit trust boundary; descendants may not redirect it.
/// Missing final components are allowed for safe creation by callers.
pub fn confined_path(root: &Path, relative: &Path) -> Result<PathBuf> {
    if relative
        .components()
        .any(|c| !matches!(c, Component::Normal(_)))
    {
        return Err(anyhow!("path must contain only normal relative components"));
    }
    let canonical_root = std::fs::canonicalize(root)
        .with_context(|| format!("data root unavailable: {}", root.display()))?;
    let mut current = canonical_root.clone();
    let mut components = relative.components().peekable();
    while let Some(Component::Normal(name)) = components.next() {
        current.push(name);
        match std::fs::symlink_metadata(&current) {
            Ok(meta) => {
                if meta.file_type().is_symlink() {
                    return Err(anyhow!(
                        "symlink path component refused: {}",
                        current.display()
                    ));
                }
                let resolved = std::fs::canonicalize(&current)
                    .with_context(|| format!("cannot safely resolve {}", current.display()))?;
                if !resolved.starts_with(&canonical_root) {
                    return Err(anyhow!("path escapes data root: {}", current.display()));
                }
                current = resolved;
            }
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                for rest in components {
                    if let Component::Normal(rest) = rest {
                        current.push(rest);
                    }
                }
                break;
            }
            Err(e) => return Err(e).with_context(|| format!("checking {}", current.display())),
        }
    }
    if !current.starts_with(&canonical_root) {
        return Err(anyhow!("path escapes data root"));
    }
    Ok(current)
}

fn sub(dir: &Path, name: &str) -> PathBuf {
    // These names are fixed by the runtime, never caller-provided. Validate
    // existing entries before creating them; on failure return an inert path
    // under the root and leave the unsafe entry untouched.
    let path = match confined_path(dir, Path::new(name)) {
        Ok(p) => p,
        Err(_) => dir.join("\0romacode-invalid-storage").join(name),
    };
    let _ = std::fs::create_dir_all(&path);
    path
}

pub fn sdcard_dir(dir: &Path) -> PathBuf {
    sub(dir, "sdcard")
}

pub fn photos_dir(dir: &Path) -> PathBuf {
    sub(dir, "photos")
}

pub fn cache_dir(dir: &Path) -> PathBuf {
    sub(dir, "cache")
}

pub fn adb_keys_dir(dir: &Path) -> PathBuf {
    sub(dir, "adb_keys")
}

pub fn state_path(dir: &Path) -> PathBuf {
    confined_path(dir, Path::new("state.json"))
        .unwrap_or_else(|_| dir.join("\0romacode-invalid-state"))
}

/// Build the fresh-device state. The package roster mirrors what a new phone
/// with GMS ships (the engine's device profile); launchable entries are the
/// apps this device actually implements, and everything else is a registered
/// system package exactly as `pm list packages` would show it.
pub fn fresh(dir: &Path) -> OsState {
    let launchable = [
        (AppId::Settings, true),
        (AppId::Calculator, true),
        (AppId::Clock, true),
        (AppId::Messages, true),
        (AppId::Dialer, true),
        (AppId::Browser, true),
        (AppId::Photos, true),
        (AppId::Files, true),
        (AppId::Termux, true),
        (AppId::Magisk, true),
        (AppId::RootExplorer, true),
        (AppId::TitaniumBackup, true),
    ];
    let system_only = [
        ("com.android.vending", "Google Play Store"),
        ("com.google.android.gms", "Google Play services"),
        ("com.google.android.gsf", "Google Services Framework"),
        ("com.google.android.setupwizard", "Setup Wizard"),
        ("com.google.android.gm", "Gmail"),
        ("com.google.android.apps.maps", "Maps"),
        ("com.google.android.youtube", "YouTube"),
        ("com.google.android.apps.docs", "Drive"),
        ("com.google.android.googlequicksearchbox", "Google"),
        ("com.google.android.apps.tachyon", "Meet"),
        ("com.google.android.inputmethod.latin", "Gboard"),
        ("com.google.android.tts", "Speech Services"),
        ("com.google.android.webview", "Android System WebView"),
        ("com.google.android.configupdater", "Config Updater"),
        ("com.android.systemui", "System UI"),
    ];
    let mut packages: Vec<Package> = Vec::new();
    for (app, _) in launchable {
        packages.push(Package {
            package: app.package(),
            label: app.label(),
            system: true,
            app: Some(app),
            version: env!("CARGO_PKG_VERSION").to_string(),
        });
    }
    for (pkg, label) in system_only {
        packages.push(Package {
            package: pkg.to_string(),
            label: label.to_string(),
            system: true,
            app: None,
            version: "14.0".into(),
        });
    }
    packages.sort_by(|a, b| a.package.cmp(&b.package));

    let _ = std::fs::create_dir_all(dir);
    let sd = sdcard_dir(dir);
    let _ = std::fs::create_dir_all(sd.join("Download"));
    let _ = std::fs::create_dir_all(sd.join("Pictures"));
    let _ = std::fs::create_dir_all(sd.join("DCIM"));
    let _ = std::fs::create_dir_all(sd.join("Documents"));
    let tx = dir.join("termux");
    let _ = std::fs::create_dir_all(tx.join("usr").join("bin"));
    let _ = std::fs::create_dir_all(tx.join("usr").join("tmp"));
    let _ = std::fs::create_dir_all(tx.join("home"));
    let welcome = b"Welcome to Termux on the phone.\n\nThis shell runs the device's own command set.\nAutomation drives it through the RUN_COMMAND service:\n  adb shell am startservice -n com.termux/.app.RunCommandService \\\n    -a com.termux.RUN_COMMAND --es com.termux.RUN_COMMAND_ARGUMENTS '<cmd>'\n";
    if let Ok(p) = confined_path(dir, Path::new("termux/home/welcome.txt")) {
        let _ = std::fs::write(p, welcome);
    }
    if let Ok(p) = confined_path(dir, Path::new("sdcard/Documents/readme.txt")) {
        let _ = std::fs::write(
            p,
            b"This is the phone's shared storage (/sdcard).\nFiles pushed over adb land here.\n",
        );
    }

    let identity = mandroid::identity::DeviceIdentity::factory();
    let esim = mandroid::identity::EsimRegistry::factory();
    let now = now_ms();
    let model = identity.model;
    let device = identity.device;
    let android_release = identity.android_release;
    let fingerprint = identity.fingerprint;
    let sdk_level = identity.sdk_level;
    let ram_mib = identity.ram_mib;
    let rom_gib = identity.rom_gib;
    let cpu_cores = identity.cpu_cores;
    let mut st = OsState {
        data_dir: dir.to_path_buf(),
        serial: identity.serial.clone(),
        adb_port: 0,
        booted_at: SystemTime::now(),
        boot: BootPhase::PowerOn {
            t0: SystemTime::now(),
        },
        screen_off: false,
        display: (PANEL_W, PANEL_H),
        density: PANEL_DPI,
        size_override: None,
        density_override: None,
        packages,
        sms: Vec::new(),
        calls: Vec::new(),
        photos: Vec::new(),
        settings: BTreeMap::from([
            ("wifi".into(), "on".into()),
            ("wifi_ssid".into(), mandroid::identity::WIFI_SSID.into()),
            ("bluetooth".into(), "off".into()),
            ("brightness".into(), "72".into()),
            ("su.auto_grant".into(), "1".into()),
            ("su.grant.com.android.shell".into(), now.to_string()),
            ("su.grant.com.termux".into(), now.to_string()),
            ("su.grant.com.topjohnwu.magisk".into(), now.to_string()),
        ]),
        battery: 68,
        charging: false,
        rooted: true,
        identity,
        airplane_mode: false,
        mobile_data: true,
        esim,
        radio: [
            mandroid::identity::RadioSnapshot::off(1),
            mandroid::identity::RadioSnapshot::off(2),
        ],
        wifi: (0, 0),
        last_radio_tick: SystemTime::now(),
        model: model.into(),
        device: device.into(),
        android_release: android_release.into(),
        sdk_level,
        ram_mib,
        rom_gib,
        cpu_cores,
        cpu_arch: "aarch64".into(),
        fingerprint: fingerprint.into(),
        current: None,
        recents: Vec::new(),
        home_page: 0,
        drag: None,
        recents_screen_open: false,
        app: AppUi::default(),
        shell_cwd: None,
        inq: VecDeque::new(),
        inbox: Arc::new(Mutex::new(VecDeque::new())),
        notifications: Vec::new(),
        banner: None,
        dirty: true,
        persist_dirty: false,
        fb: Vec::new(),
        fb_gen: 0,
        last_battery_tick: SystemTime::now(),
        last_save: SystemTime::now(),
        dmesg: VecDeque::new(),
        logs: VecDeque::new(),
        recovery_open: false,
        recovery_sel: 0,
        recovery_confirm: false,
        installer: None,
        reboots: 0,
    };
    st.radio = [st.radio_now(1), st.radio_now(2)];
    st.wifi = mandroid::identity::sample_wifi(true, now);
    st
}

/// Load `state.json` on top of the fresh defaults (package roster always
/// rebuilt from code; user content restored from disk).
pub fn load(dir: &Path) -> Result<OsState> {
    std::fs::create_dir_all(dir)
        .with_context(|| format!("creating data root {}", dir.display()))?;
    let mut st = fresh(dir);
    let path = confined_path(dir, Path::new("state.json"))?;
    if !path.exists() {
        return Ok(st);
    }
    let text =
        std::fs::read_to_string(&path).with_context(|| format!("reading {}", path.display()))?;
    let p: Persisted =
        serde_json::from_str(&text).with_context(|| format!("parsing {}", path.display()))?;
    for up in p.user_packages {
        st.packages.push(Package {
            package: up.package.clone(),
            label: up.label,
            system: false,
            app: Some(AppId::User(up.package)),
            version: up.version,
        });
        st.packages.sort_by(|a, b| a.package.cmp(&b.package));
    }
    st.sms = p.sms;
    st.calls = p.calls;
    st.photos = p
        .photos
        .into_iter()
        .filter(|photo| {
            let mut parts = Path::new(&photo.file).components();
            matches!(parts.next(), Some(Component::Normal(_))) && parts.next().is_none()
        })
        .collect();
    for (k, v) in p.settings {
        st.settings.insert(k, v);
    }
    if let Some(b) = p.battery {
        st.battery = b.min(100);
    }
    st.charging = p.charging.unwrap_or(false);
    if let Some(esim) = p.esim {
        st.esim = esim;
    }
    st.airplane_mode = p.airplane_mode.unwrap_or(false);
    st.mobile_data = p.mobile_data.unwrap_or(true);
    st.radio = [st.radio_now(1), st.radio_now(2)];
    st.wifi = mandroid::identity::sample_wifi(
        st.settings.get("wifi").map(String::as_str) == Some("on"),
        now_ms(),
    );
    Ok(st)
}

/// Snapshot the parts of the state that persist across boots.
pub fn save(st: &OsState) -> Result<()> {
    let p = Persisted {
        user_packages: st
            .packages
            .iter()
            .filter(|pkg| !pkg.system)
            .map(|pkg| PersistPackage {
                package: pkg.package.clone(),
                label: pkg.label.clone(),
                version: pkg.version.clone(),
            })
            .collect(),
        sms: st.sms.clone(),
        calls: st.calls.clone(),
        photos: st.photos.clone(),
        settings: st.settings.clone(),
        battery: Some(st.battery),
        charging: Some(st.charging),
        esim: Some(st.esim.clone()),
        airplane_mode: Some(st.airplane_mode),
        mobile_data: Some(st.mobile_data),
    };
    let path = confined_path(&st.data_dir, Path::new("state.json"))?;
    let tmp = confined_path(&st.data_dir, Path::new("state.json.tmp"))?;
    let text = serde_json::to_string_pretty(&p)?;
    std::fs::write(&tmp, text).with_context(|| format!("writing {}", tmp.display()))?;
    std::fs::rename(&tmp, &path).with_context(|| format!("committing {}", path.display()))?;
    Ok(())
}

/// Read only regular, non-symlink key files from the owner-managed directory.
pub fn authorized_keys(dir: &Path) -> Vec<String> {
    let Ok(d) = confined_path(dir, Path::new("adb_keys")) else {
        return Vec::new();
    };
    let mut out = Vec::new();
    if let Ok(rd) = std::fs::read_dir(d) {
        for e in rd.flatten() {
            let path = e.path();
            let Ok(meta) = std::fs::symlink_metadata(&path) else {
                continue;
            };
            if meta.file_type().is_symlink() || !meta.is_file() {
                continue;
            }
            if let Ok(t) = std::fs::read_to_string(path) {
                let t = t.trim().to_string();
                if !t.is_empty() {
                    out.push(t);
                }
            }
        }
    }
    out
}

/// Explicit local-owner bootstrap API. The ADB network handshake deliberately
/// never calls this function for a key supplied by a client.
pub fn authorize_key(dir: &Path, base64_blob: &str) -> Result<()> {
    std::fs::create_dir_all(dir)
        .with_context(|| format!("creating data root {}", dir.display()))?;
    let d = confined_path(dir, Path::new("adb_keys"))?;
    std::fs::create_dir_all(&d).with_context(|| format!("creating {}", d.display()))?;
    // Name the file after the key itself, so local re-authorisation is idempotent.
    let name = format!("key_{}.pub", hash_text(base64_blob));
    let path = confined_path(dir, Path::new("adb_keys").join(name).as_path())?;
    std::fs::write(path, base64_blob)?;
    Ok(())
}

pub fn key_is_authorized(dir: &Path, base64_blob: &str) -> bool {
    authorized_keys(dir).iter().any(|k| k == base64_blob)
}

fn hash_text(s: &str) -> String {
    let mut h: u64 = 0xcbf29ce484222325;
    for b in s.bytes() {
        h ^= b as u64;
        h = h.wrapping_mul(0x100000001b3);
    }
    format!("{h:016x}")
}

/// Register a real PNG into the gallery (screenshot or browser image).
pub fn add_photo(_dir: &Path, file: &str, w: u32, h: u32) -> PhotoEntry {
    PhotoEntry {
        file: file.to_string(),
        w,
        h,
        taken_at: now_ms(),
    }
}

#[cfg(test)]
mod security_tests {
    use super::*;
    use std::time::{SystemTime, UNIX_EPOCH};

    fn temp() -> PathBuf {
        let p = std::env::temp_dir().join(format!(
            "phone-persist-security-{}-{}",
            std::process::id(),
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(&p).unwrap();
        p
    }

    #[test]
    fn confined_path_rejects_parent_and_symlink_escape() {
        let root = temp();
        let outside = root.with_extension("outside");
        std::fs::create_dir_all(&outside).unwrap();
        std::fs::write(outside.join("secret"), b"secret").unwrap();
        assert!(confined_path(&root, Path::new("../outside/secret")).is_err());
        #[cfg(unix)]
        {
            std::os::unix::fs::symlink(&outside, root.join("link")).unwrap();
            assert!(confined_path(&root, Path::new("link/secret")).is_err());
        }
        let _ = std::fs::remove_dir_all(root);
        let _ = std::fs::remove_dir_all(outside);
    }

    #[test]
    fn only_explicit_local_bootstrap_authorizes_keys_and_symlinks_are_ignored() {
        let root = temp();
        assert!(!key_is_authorized(&root, "owner-key"));
        authorize_key(&root, "owner-key").unwrap();
        assert!(key_is_authorized(&root, "owner-key"));
        #[cfg(unix)]
        {
            let outside = root.with_extension("keys");
            std::fs::write(&outside, "remote-key").unwrap();
            std::os::unix::fs::symlink(&outside, root.join("adb_keys/remote.pub")).unwrap();
            assert!(!key_is_authorized(&root, "remote-key"));
            let _ = std::fs::remove_file(outside);
        }
        let _ = std::fs::remove_dir_all(root);
    }
}
