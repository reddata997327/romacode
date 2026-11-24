//! Capture every boot-chain stage as PNGs from a live first boot.
//! WATCH_OUT_DIR = data dir (fresh), WATCH_OUT_PFX = output prefix.
use phone::{boot, PhoneConfig};
use std::time::{Duration, Instant};

fn main() {
    let data = std::env::var_os("WATCH_OUT_DIR")
        .map(std::path::PathBuf::from)
        .unwrap_or_else(|| std::path::PathBuf::from("/tmp/capture-boot"));
    let pfx = std::env::var("WATCH_OUT_PFX").unwrap_or_else(|_| "/tmp/boot-stage".to_string());
    let _ = std::fs::remove_dir_all(&data);
    let cfg = PhoneConfig {
        port: 5591,
        data_dir: data.clone(),
        display: (720, 1560),
        density: 420,
        ..Default::default()
    };
    let handle = boot(cfg).expect("boot");

    let mut saved: Vec<String> = Vec::new();
    let mut last_phase = String::new();
    let deadline = Instant::now() + Duration::from_secs(30);
    while Instant::now() < deadline {
        let st = handle.state.lock().unwrap();
        let phase: String = match &st.boot {
            phone::state::BootPhase::PowerOn { .. } => "1-bootloader",
            phone::state::BootPhase::Flash => "2-os-flash",
            phone::state::BootPhase::Kernel { .. } => "3-kernel-init",
            phone::state::BootPhase::Splash { .. } => "4-boot-animation",
            phone::state::BootPhase::Recovery => "recovery",
            phone::state::BootPhase::Home => "5-home",
        }
        .to_string();
        let png = phone::shell::screencap_png(&st);
        drop(st);
        // keep overwriting while in the same stage — the file ends up holding
        // the stage's LAST frame (full dmesg, flash nearly done, …)
        let path = format!("{pfx}-{phase}.png");
        let first = phase != last_phase;
        std::fs::write(&path, &png).ok();
        if first {
            saved.push(path);
            eprintln!("tracking {phase}");
            last_phase = phase.clone();
        }
        if phase == "5-home" {
            break;
        }
        std::thread::sleep(Duration::from_millis(60));
    }
    handle.shutdown();
    eprintln!("stages: {}", saved.join(", "));
}
