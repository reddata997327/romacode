//! RomaCode's legacy in-process handset simulator. This is not Android and it
//! does not execute Android APKs; the TUI's production target uses host ADB.
//!
//! One process, two halves, no external binaries:
//!
//! * **Simulated UI** — a compositor that renders a mock launcher and apps,
//!   status/nav bars, soft keyboard) into a real RGBA framebuffer, plus the
//!   input pipeline (touch, keys, text), notifications, persistence.
//! * **Legacy protocol stub** — a local ADB-like service retained for tests;
//!   it is not a compatible Android runtime or a security boundary.
//!
//! Standalone test/development use only (`phone serve`).

pub mod adbd;
pub mod apps;
pub mod compose;
pub mod gfx;
pub mod net_fetch;
pub mod os;
pub mod persist;
pub mod shell;
pub mod state;
pub mod theme;

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

pub use state::OsState;

/// Boot configuration.
#[derive(Clone, Debug)]
pub struct PhoneConfig {
    /// Attach RomaCode to the host ADB server rather than booting this simulator.
    pub external_adb: bool,
    /// Exact `adb devices` serial; empty requires a sole online target.
    pub adb_serial: String,
    /// Android Platform-Tools executable path (or `adb` from PATH).
    pub adb_path: String,
    /// Preferred ADB port (5555 default; busy ports step +2 like real
    /// devices). Ignored when 0.
    pub port: u16,
    /// Data directory (state.json, sdcard, photos, adb_keys).
    pub data_dir: std::path::PathBuf,
    /// Display panel: (width, height) in px.
    pub display: (u32, u32),
    /// Display density in dpi.
    pub density: u32,
    /// When non-empty and `external_adb` is false, the bridge speaks the ADB
    /// wire protocol directly to this `host:port` instead of booting the
    /// in-process simulator. Empty = boot the simulator (legacy mode).
    pub native_target: String,
}

impl Default for PhoneConfig {
    fn default() -> Self {
        Self {
            external_adb: false,
            adb_serial: String::new(),
            adb_path: "adb".into(),
            port: 5555,
            data_dir: persist::data_dir(),
            display: (state::PANEL_W, state::PANEL_H),
            density: state::PANEL_DPI,
            native_target: String::new(),
        }
    }
}

/// A live phone. Drop or [`shutdown`] to stop it.
pub struct PhoneHandle {
    pub port: u16,
    pub serial: String,
    pub state: Arc<Mutex<OsState>>,
    stop: Arc<AtomicBool>,
    os_thread: Option<std::thread::JoinHandle<()>>,
    adbd_thread: Option<std::thread::JoinHandle<()>>,
}

impl PhoneHandle {
    /// Stop the compositor and adbd, flush persistence.
    pub fn shutdown(mut self) {
        self.stop.store(true, Ordering::Relaxed);
        if let Some(t) = self.os_thread.take() {
            let _ = t.join();
        }
        if let Some(t) = self.adbd_thread.take() {
            let _ = t.join();
        }
        if let Ok(st) = self.state.lock() {
            let _ = persist::save(&st);
        }
    }

    /// A screenshot of the current frame as PNG bytes.
    pub fn screenshot_png(&self) -> Vec<u8> {
        let st = self.state.lock().unwrap();
        shell::screencap_png(&st)
    }
}

impl Drop for PhoneHandle {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
    }
}

/// Boot the phone: compositor thread + adbd listener. Returns once adbd is
/// actually listening (so callers know the real port).
pub fn boot(cfg: PhoneConfig) -> anyhow::Result<PhoneHandle> {
    std::fs::create_dir_all(&cfg.data_dir)?;
    let mut st = persist::load(&cfg.data_dir)?;
    st.display = cfg.display;
    st.density = cfg.density;
    // the handset goes through the real boot chain: bootloader → (OS flash
    // when no image is on disk yet) → kernel/init → boot animation → home
    st.boot = state::BootPhase::PowerOn {
        t0: std::time::SystemTime::now(),
    };
    st.dirty = true;
    if !os::is_installed(&cfg.data_dir) {
        // factory flash: the image is written to disk (partitioned, real
        // files, checksummed) before the kernel stage plays
        st.installer = Some(os::Installer::new(&cfg.data_dir, &st.identity));
        st.logs
            .push_back("I/boot( 1): no OS image on disk — factory flash scheduled".to_string());
    }
    let state = Arc::new(Mutex::new(st));

    let stop = Arc::new(AtomicBool::new(false));

    // compositor thread
    let (comp_state, comp_stop) = (state.clone(), stop.clone());
    let os_thread = std::thread::Builder::new()
        .name("phone-compositor".into())
        .spawn(move || loop {
            {
                let mut st = comp_state.lock().unwrap();
                compose::tick(&mut st);
            }
            if comp_stop.load(Ordering::Relaxed) {
                // final flush
                let st = comp_state.lock().unwrap();
                let _ = persist::save(&st);
                return;
            }
            std::thread::sleep(Duration::from_millis(33));
        })?;

    // adbd thread
    let adbd_state = state.clone();
    let adbd_stop = stop.clone();
    let bound = Arc::new(Mutex::new(None::<u16>));
    let bound_wait = bound.clone();
    // The old code did `let _ = daemon.serve()`: a bind failure (all ADB
    // ports taken, no permissions) vanished and boot died with the vague
    // "did not bind a port in time". Capture the actual error and surface it.
    let adbd_err = Arc::new(Mutex::new(None::<String>));
    let adbd_err_slot = adbd_err.clone();
    let adbd_thread = std::thread::Builder::new()
        .name("phone-adbd".into())
        .spawn(move || {
            let daemon = adbd::Adbd {
                state: adbd_state,
                stop: adbd_stop,
                bound_port: bound,
            };
            if let Err(e) = daemon.serve() {
                *adbd_err_slot.lock().unwrap() = Some(format!("{e:#}"));
            }
        })?;

    // wait until adbd has bound its port (bounded wait — 10 s; on a loaded
    // host the old 2 s window could expire before the listener came up)
    let mut port = None;
    for _ in 0..500 {
        if let Some(p) = *bound_wait.lock().unwrap() {
            port = Some(p);
            break;
        }
        if let Some(err) = adbd_err.lock().unwrap().clone() {
            anyhow::bail!("phone adbd failed to start: {err}");
        }
        std::thread::sleep(Duration::from_millis(20));
    }
    let Some(port) = port else {
        let cause = adbd_err
            .lock()
            .unwrap()
            .clone()
            .unwrap_or_else(|| "no free ADB port in 5555..=5615".to_string());
        // Do NOT join: the listener may have bound at the exact moment we
        // gave up, and the thread then lives in its accept loop. Detach it
        // (the handle's Drop / stop flag reaps it when the phone is rebuilt).
        anyhow::bail!("adbd did not bind a port in time: {cause}");
    };

    // adopt the physical serial (the transport stays host:port for clients;
    // the device itself reports its physical serial, like a real handset)
    let serial = {
        let mut st = state.lock().unwrap();
        st.serial = st.identity.serial.clone();
        st.serial.clone()
    };

    Ok(PhoneHandle {
        port,
        serial,
        state,
        stop,
        os_thread: Some(os_thread),
        adbd_thread: Some(adbd_thread),
    })
}

/// Box-filter RGBA downscale (the same resampler the engine's screen module
/// uses for screenshots).
pub fn scale_rgba(src: &[u8], sw: u32, sh: u32, dw: u32, dh: u32) -> Vec<u8> {
    mandroid::screen::box_filter(src, sw, sh, dw, dh)
}
