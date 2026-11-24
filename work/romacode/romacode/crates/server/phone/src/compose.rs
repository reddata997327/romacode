//! The compositor — turns [`OsState`] into pixels and routes touches.
//!
//! One thread ticks this at ~30 fps: drain the input queue, advance
//! animations/clock/battery, recompose the framebuffer if anything changed,
//! and commit it (`fb` + `fb_gen`). Everything else (adbd connections, the
//! screenshot path) reads the committed frame.

use std::time::SystemTime;

use crate::gfx::{in_rect, mix, Canvas};
use crate::state::*;
use crate::theme::*;
use crate::theme::{ACCENT, BG, DIM, DIMMER, GREEN, RED, SURFACE, SURFACE_HI, TEXT};

pub const STATUS_H: i32 = 48;
pub const NAV_H: i32 = 64;
pub const KB_ROWS: i32 = 4;
pub const KB_H: i32 = 56 * KB_ROWS + 16;

pub fn now() -> SystemTime {
    SystemTime::now()
}

/// Advance the device by one frame: input, events, clock/battery, re-render.
pub fn tick(st: &mut OsState) {
    // battery simulation: 1% / 90 s discharge, or +2% / 30 s on charger
    if st.last_battery_tick.elapsed().unwrap_or_default().as_secs() >= 30 {
        let secs = st.last_battery_tick.elapsed().unwrap_or_default().as_secs();
        st.last_battery_tick = now();
        if st.charging {
            st.battery = (st.battery + (secs / 15) as u8).min(100);
        } else {
            st.battery = st.battery.saturating_sub((secs / 45) as u8);
        }
        st.dirty = true;
        st.persist_dirty = true;
    }

    // the radio model advances every 2 s: RSRP drifts, bars move, Wi-Fi
    // fluctuates — the status bar shows a live modem, not a static icon
    if st.last_radio_tick.elapsed().unwrap_or_default().as_millis() >= 2000 {
        st.last_radio_tick = now();
        let t = now_ms();
        for (i, slot) in [1u32, 2].iter().enumerate() {
            let snap = mandroid::identity::sample_radio(
                *slot,
                &st.esim,
                st.airplane_mode,
                st.mobile_data,
                t,
            );
            if st.radio[i] != snap {
                st.radio[i] = snap;
                st.dirty = true;
            }
        }
        let wifi_on = st.settings.get("wifi").map(String::as_str) == Some("on");
        let wifi = mandroid::identity::sample_wifi(wifi_on, t);
        if st.wifi != wifi {
            st.wifi = wifi;
            st.dirty = true;
        }
    }

    // the boot chain: bootloader → (OS flash when an installer is armed) →
    // kernel/init (dmesg plays) → boot animation → home. Each stage does the
    // real work (the flash writes the image; the kernel stage fills dmesg).
    match st.boot.clone() {
        BootPhase::PowerOn { t0 } => {
            if t0.elapsed().unwrap_or_default().as_millis() >= 800 {
                st.boot = if st.installer.is_some() {
                    BootPhase::Flash
                } else {
                    BootPhase::Kernel { t0: now(), line: 0 }
                };
            }
            st.dirty = true;
        }
        BootPhase::Flash => {
            // one installer chunk per tick: the progress screen is the real
            // flash work, not an animation over nothing
            let mut finished = false;
            if let Some(inst) = st.installer.as_mut() {
                finished = !inst.step();
            }
            st.dirty = true;
            if finished {
                if let Some(inst) = st.installer.take() {
                    for l in inst.log() {
                        st.log_line(format!("I/installer( 1): {l}"));
                    }
                    st.log_line(format!(
                        "I/installer( 1): OS image flashed: {}",
                        crate::os::version_line(&st.data_dir)
                    ));
                }
                st.boot = BootPhase::Kernel { t0: now(), line: 0 };
                st.persist_dirty = true;
            }
        }
        BootPhase::Kernel { t0, line } => {
            // play the kernel/init log into dmesg one line per tick
            let script = crate::os::boot_script(&st.identity);
            if let Some(l) = script.get(line) {
                push_dmesg(st, l.clone());
                st.log_line(format!(
                    "I/init( 1): {}",
                    l.trim_start_matches(|c: char| !c.is_alphabetic())
                        .to_string()
                ));
                st.boot = BootPhase::Kernel { t0, line: line + 1 };
            } else {
                st.boot = BootPhase::Splash { t0: now() };
            }
            st.dirty = true;
        }
        BootPhase::Splash { t0 } => {
            if t0.elapsed().unwrap_or_default().as_millis() >= 1500 {
                st.boot = BootPhase::Home;
                st.booted_at = now();
                st.logs
                    .push_back("I/roma-os( 1): boot completed (sys.boot_completed=1)".to_string());
                st.persist_dirty = true;
            }
            st.dirty = true;
        }
        _ => {}
    }

    // toast banner expiry
    if let Some((_, _, at)) = &st.banner {
        if at.elapsed().unwrap_or_default().as_millis() > 2600 {
            st.banner = None;
            st.dirty = true;
        }
    }

    // process events from background threads (browser loads, toasts, OS
    // install / reboot requests from Settings, recovery and adb shell)
    let pending: Vec<OsEvent> = st
        .inbox
        .lock()
        .map(|mut q| q.drain(..).collect())
        .unwrap_or_default();
    for ev in pending {
        match ev {
            OsEvent::Toast(t) => {
                st.notify("System", &t, None);
            }
            OsEvent::OsInstall => {
                // Settings → System update, recovery, or `adb shell os install`:
                // the handset reboots through the bootloader and re-flashes
                st.installer = Some(crate::os::Installer::new(&st.data_dir, &st.identity));
                st.boot = BootPhase::PowerOn { t0: now() };
                st.current = None;
                st.recents_screen_open = false;
                st.recovery_open = false;
                st.screen_off = false;
                st.log_line(
                    "I/installer( 1): OS install requested — rebooting to bootloader".to_string(),
                );
                st.dirty = true;
            }
            OsEvent::Reboot => reboot_now(st),
            OsEvent::RebootRecovery => {
                st.recovery_open = true;
                st.recovery_sel = 0;
                st.recovery_confirm = false;
                st.current = None;
                st.recents_screen_open = false;
                st.boot = BootPhase::Recovery;
                st.logs
                    .push_back("I/roma-os( 1): booting into recovery".to_string());
                st.dirty = true;
            }
            OsEvent::BrowserLoaded {
                url,
                ok,
                detail,
                body_path,
                status,
            } => {
                let ui = &mut st.app.browser;
                ui.loading = false;
                if ok {
                    let html = std::fs::read_to_string(&body_path).unwrap_or_default();
                    ui.lines = crate::apps::html_to_lines(&html, &url);
                    ui.status = status;
                    ui.error = None;
                    ui.scroll = 0;
                    // back/forward reloads keep their slot; a fresh navigation
                    // drops any forward entries and appends after the current one
                    let is_history_load = ui
                        .history_idx
                        .and_then(|i| ui.history.get(i))
                        .map_or(false, |h| *h == url);
                    if !is_history_load {
                        let keep = ui.history_idx.map_or(0, |i| i + 1).min(ui.history.len());
                        ui.history.truncate(keep);
                        ui.history.push(url.clone());
                        ui.history_idx = Some(ui.history.len() - 1);
                    }
                    ui.url_bar = url.clone();
                    ui.url_bar_focus = false;
                    ui.kb.visible = false;
                } else {
                    ui.error = Some(detail);
                    ui.lines.clear();
                }
                st.dirty = true;
            }
        }
    }

    // input
    while let Some(ev) = st.inq.pop_front() {
        st.dirty = true;
        match ev {
            InputEvent::Key(KeyCode::Power) => {
                st.screen_off = !st.screen_off;
            }
            _ if st.screen_off => {
                if matches!(
                    ev,
                    InputEvent::Touch(TouchEvent {
                        action: TouchAction::Up,
                        ..
                    })
                ) || matches!(
                    ev,
                    InputEvent::Key(KeyCode::Back) | InputEvent::Key(KeyCode::Home)
                ) {
                    st.screen_off = false; // any touch or Back/Home wakes
                }
            }
            // a real handset's input pipeline only comes up once the system
            // has booted (sys.boot_completed=1) — touches/keys during the
            // flash or kernel stage are dropped, not mis-routed into apps
            InputEvent::Key(KeyCode::Back)
                if matches!(st.boot, BootPhase::Recovery) || st.recovery_open =>
            {
                // Back leaves the recovery menu (like rebooting the system)
                st.recovery_open = false;
                if matches!(st.boot, BootPhase::Recovery) {
                    st.boot = BootPhase::Home;
                }
            }
            InputEvent::Touch(t) if matches!(st.boot, BootPhase::Recovery) || st.recovery_open => {
                recovery_tap(st, t);
            }
            _ if !st.boot.boot_completed() => {
                // still booting: input is not wired up yet — dropped like on
                // a real device
            }
            InputEvent::Key(k) => handle_key(st, k),
            InputEvent::Text(t) => handle_text(st, &t.text),
            InputEvent::Touch(t) => handle_touch(st, t),
        }
    }

    // titanium backup runs one package per tick while a batch is active
    if st.app.titanium.progress.is_some() {
        crate::apps::titanium_step(st);
        st.dirty = true;
    }

    // clock app + stopwatch invalidate every second
    if st.current == Some(AppId::Clock) || st.app.clock.started.is_some() {
        st.dirty = true;
    }

    if st.dirty {
        let (w, h) = st.effective_size();
        let mut canvas = Canvas::new(w, h);
        draw(&mut canvas, st);
        st.fb = canvas.px;
        st.fb_gen += 1;
        st.dirty = false;
    }

    // periodic persistence
    if st.persist_dirty && st.last_save.elapsed().unwrap_or_default().as_secs() >= 3 {
        if crate::persist::save(st).is_ok() {
            st.persist_dirty = false;
        }
        st.last_save = now();
    }
}

// ── screen composition ───────────────────────────────────────────────────────

fn draw(c: &mut Canvas, st: &mut OsState) {
    match &st.boot {
        BootPhase::PowerOn { .. } => draw_bootloader(c),
        BootPhase::Flash => draw_flash(c, st),
        BootPhase::Kernel { .. } => draw_kernel(c, st),
        BootPhase::Splash { t0 } => {
            let p = (t0.elapsed().unwrap_or_default().as_millis() as f32 / 1500.0).min(1.0);
            draw_boot(c, p);
        }
        BootPhase::Recovery => draw_recovery(c, st),
        BootPhase::Home => {
            if st.screen_off {
                draw_screen_off(c);
                return;
            }
            draw_wallpaper(c, 0, 0, c.h as i32);
            if let Some(app) = st.current.clone() {
                if st.recents_screen_open {
                    draw_recents(c, st);
                } else {
                    draw_running_app(c, st, &app);
                }
            } else if st.recents_screen_open {
                draw_recents(c, st);
            } else {
                draw_launcher(c, st);
            }
            draw_status(c, st);
            draw_nav(c, st);
            draw_banner(c, st);
            if st.recovery_open {
                draw_recovery(c, st);
            }
        }
    }
}

/// Bootloader: the vendor wordmark, the unlock state, and what happens next.
fn draw_bootloader(c: &mut Canvas) {
    c.fill([0, 0, 0]);
    let cx = c.w as i32 / 2;
    let cy = c.h as i32 / 2 - 120;
    // soft halo behind the wordmark
    c.glow(cx, cy + 60, 240, [64, 84, 150], 70);
    c.text_center(cx, cy, "G", 14, TEXT);
    c.text_center(cx, cy + 130, "caiman bootloader", 3, DIM);
    c.text_center(
        cx,
        cy + 180,
        "bootloader unlocked — verified boot state: orange",
        2,
        YELLOW,
    );
    c.text_center(
        cx,
        cy + 220,
        "flashing os image / booting kernel…",
        2,
        DIMMER,
    );
    // the classic orange unlocked-bootloader underline
    c.fill_rect(cx - 160, cy + 260, 320, 4, ORANGE);
}

/// OS flash progress — the installer's real work (partition table, file
/// extraction, checksum verification, manifest stamp).
fn draw_flash(c: &mut Canvas, st: &mut OsState) {
    c.fill([4, 6, 10]);
    let (w, h) = (c.w as i32, c.h as i32);
    let done = st.installer.as_ref().map(|i| i.progress()).unwrap_or((
        1,
        1,
        "Install complete".to_string(),
    ));
    let (files_done, files_total, label) = done;
    let p = (files_done as f32 / files_total.max(1) as f32).min(1.0);
    c.text_center(w / 2, h / 2 - 190, "Installing OS update", 4, TEXT);
    c.text_center(w / 2, h / 2 - 140, &label, 3, ACCENT);
    // progress bar
    let bw = 480;
    let bx = w / 2 - bw / 2;
    let by = h / 2 - 90;
    c.round_rect(bx, by, bw, 14, 7, [30, 36, 46]);
    c.round_rect(bx, by, (bw as f32 * p).round() as i32, 14, 7, GREEN);
    c.text_center(
        w / 2,
        by + 46,
        &format!("{files_done} / {files_total} files written"),
        3,
        DIM,
    );
    // partition map (what a factory flash touches)
    let y0 = h / 2 + 20;
    for (i, part) in crate::os::PARTITIONS.iter().enumerate() {
        let y = y0 + i as i32 * 40;
        let col = if part.readonly { DIM } else { GREEN };
        c.text(
            60,
            y,
            &format!(
                "{:<11} {:<12} {:>7} MiB  {}",
                part.name,
                part.mount,
                part.size_kib / 1024,
                if part.readonly { "ro" } else { "rw" }
            ),
            2,
            col,
        );
    }
}

/// Kernel/init stage: the dmesg feed scrolling on a black panel.
fn draw_kernel(c: &mut Canvas, st: &mut OsState) {
    c.fill([0, 0, 0]);
    let lines: Vec<&String> = st.dmesg.iter().rev().take(26).collect::<Vec<_>>();
    let mut y = c.h as i32 - 60;
    for l in lines {
        c.text(16, y, l, 1, [120, 220, 140]);
        y -= 34;
        if y < 40 {
            break;
        }
    }
    if y >= 40 {
        c.text(16, 20, "ANDROID!", 2, DIM);
    }
}

fn draw_boot(c: &mut Canvas, p: f32) {
    c.fill([6, 8, 12]);
    let cx = c.w as i32 / 2;
    let cy = c.h as i32 / 2 - 60;
    // ambient halo: the wordmark breathes out of the dark
    c.glow(cx, cy + 40, 300, [46, 62, 120], 90);
    c.glow(cx, cy + 40, 140, [70, 96, 180], 60);
    // wordmark
    c.text_center(cx, cy, "romacode", 8, TEXT);
    c.text_center(cx, cy + 80, "P H O N E", 3, DIM);
    // progress bar: gradient fill over a dark track, glass track outline
    let bw = 320;
    let bx = cx - bw / 2;
    let by = cy + 200;
    c.round_rect(bx, by, bw, 10, 5, [24, 30, 42]);
    let fill = (bw as f32 * p).round() as i32;
    if fill > 8 {
        c.round_rect_gradient_v(bx, by, fill, 10, 5, ACCENT_HI, ACCENT_LO);
        // moving sheen on the leading edge
        let edge = bx + fill - 4;
        c.fill_rect(edge, by + 1, 3, 8, ACCENT_HI);
    }
    c.round_rect_outline(bx, by, bw, 10, 5, [58, 68, 88]);
    // three easing dots below — breathe in sequence
    for i in 0..3 {
        let phase = (p * 6.0 - i as f32 * 0.4).clamp(0.0, 1.0);
        let a = (phase * 255.0) as u8;
        let dcol = mix(ACCENT_LO, BG, 1.0 - a as f32 / 255.0);
        let dx = cx - 40 + i * 40;
        c.glow(dx, by + 60, 14, ACCENT_LO, (a / 3).max(24));
        c.circle(dx, by + 60, 8, dcol);
    }
}

fn draw_screen_off(c: &mut Canvas) {
    c.fill([0, 0, 0]);
}

// ── recovery ─────────────────────────────────────────────────────────────────

/// The recovery menu — the real recovery text: reboot, apply OS update
/// (re-flash the image), wipe data, power off.
pub const RECOVERY_ROWS: [&str; 4] = [
    "Reboot system now",
    "Apply OS update (flash image)",
    "Wipe data / factory reset",
    "Power off",
];

fn draw_recovery(c: &mut Canvas, st: &mut OsState) {
    let (w, h) = st.effective_size();
    let wi = w as i32;
    // dim the system underneath (when shown as a Home overlay)
    c.fill_rect(0, 0, wi, h as i32, [0, 0, 0]);
    c.text(24, 28, "Android recovery — caiman", 3, ACCENT);
    c.text(
        24,
        66,
        "e:Unable to open LUN file (expected in emu)",
        1,
        DIMMER,
    );
    c.fill_rect(0, 96, wi, 2, DIM);
    let y0 = 130;
    let rh = 92;
    for (i, label) in RECOVERY_ROWS.iter().enumerate() {
        let y = y0 + i as i32 * rh;
        let sel = st.recovery_sel == i;
        if sel {
            c.fill_rect(0, y, wi, rh, [24, 34, 26]);
        }
        c.text(48, y + 26, if sel { "→ " } else { "  " }, 3, GREEN);
        c.text(100, y + 26, label, 3, if sel { TEXT } else { DIM });
    }
    // wipe confirmation row (real recovery: extra confirmation step)
    let cy = y0 + RECOVERY_ROWS.len() as i32 * rh;
    if st.recovery_confirm {
        c.text(
            48,
            cy + 8,
            "! wipe data: tap 'Wipe data / factory reset' again to confirm",
            2,
            YELLOW,
        );
    }
    c.text(
        24,
        cy + 70,
        "Back — leave recovery · select a row by tapping it",
        2,
        DIMMER,
    );
    let ver = crate::os::read_manifest(&st.data_dir)
        .map(|m| format!("installed: Android {} ({})", m.version, m.build_id))
        .unwrap_or_else(|| "installed: no OS image".to_string());
    c.text(24, h as i32 - 60, &ver, 2, DIM);
}

/// Tap inside the recovery menu: pick a row, run its action.
fn recovery_tap(st: &mut OsState, t: TouchEvent) {
    if t.action != TouchAction::Up {
        return;
    }
    let (_, h) = st.effective_size();
    let y0 = 130;
    let rh = 92;
    let idx = ((t.y as i32 - y0) / rh).max(0) as usize;
    if (t.y as i32) >= (h as i32 - 70) && (t.x as i32) < 200 {
        // the "Back" hint strip at the bottom
        st.recovery_open = false;
        if matches!(st.boot, BootPhase::Recovery) {
            st.boot = BootPhase::Home;
        }
        st.dirty = true;
        return;
    }
    if idx >= RECOVERY_ROWS.len() {
        return;
    }
    st.recovery_sel = idx;
    match idx {
        0 => {
            // reboot system now: back through the full boot chain
            st.recovery_open = false;
            st.boot = BootPhase::Recovery;
            reboot_now(st);
        }
        1 => {
            // apply OS update: re-flash the image through the bootloader
            st.recovery_open = false;
            st.installer = Some(crate::os::Installer::new(&st.data_dir, &st.identity));
            st.boot = BootPhase::PowerOn { t0: now() };
            st.logs
                .push_back("I/recovery( 1): applying OS update (factory image)".to_string());
        }
        2 => {
            if st.recovery_confirm {
                wipe_data(st);
            } else {
                st.recovery_confirm = true;
            }
        }
        _ => {
            // power off: screen goes dark; any touch wakes it
            st.recovery_open = false;
            st.boot = BootPhase::Home;
            st.screen_off = true;
            st.logs
                .push_back("I/roma-os( 1): power off requested from recovery".to_string());
        }
    }
    st.dirty = true;
}

/// Factory reset: /data is wiped (user content, grants, the /data side of the
/// image skeleton); the system partitions stay, then the image re-flashes the
/// userdata skeleton — exactly what a factory reset + reflash does.
fn wipe_data(st: &mut OsState) {
    let dir = st.data_dir.clone();
    let _ = std::fs::remove_file(crate::persist::state_path(&dir));
    for sub in ["sdcard", "photos", "cache", "termux", "adb_keys"] {
        let _ = std::fs::remove_dir_all(dir.join(sub));
    }
    if let Some(data_host) = crate::os::host_path(&dir, "/data") {
        let _ = std::fs::remove_dir_all(&data_host);
    }
    st.sms.clear();
    st.calls.clear();
    st.photos.clear();
    st.notifications.clear();
    st.logs
        .push_back("I/recovery( 1): data wipe complete — restoring the factory image".to_string());
    st.installer = Some(crate::os::Installer::new(&dir, &st.identity));
    st.boot = BootPhase::PowerOn { t0: now() };
    st.current = None;
    st.recents_screen_open = false;
    st.recovery_open = false;
    st.recovery_confirm = false;
}

// ── boot helpers ─────────────────────────────────────────────────────────────

/// Reboot through the real chain (bootloader → kernel → animation → home).
pub fn reboot_now(st: &mut OsState) {
    st.boot = BootPhase::PowerOn { t0: now() };
    st.current = None;
    st.recents_screen_open = false;
    st.screen_off = false;
    st.recovery_open = false;
    st.dmesg.clear();
    st.reboots += 1;
    st.log_line(format!(
        "I/roma-os( 1): reboot #{} — bootloader handshake",
        st.reboots
    ));
    st.dirty = true;
}

/// Append to the kernel ring buffer (dmesg), capped like a real ring buffer.
pub fn push_dmesg(st: &mut OsState, line: String) {
    if st.dmesg.len() >= 1024 {
        st.dmesg.pop_front();
    }
    st.dmesg.push_back(line);
}

/// Status bar: time, notification dot, wifi, cellular, battery.
/// Rendered as a translucent scrim over the wallpaper — the premium look —
/// fading to nothing at its lower edge.
fn draw_status(c: &mut Canvas, st: &mut OsState) {
    c.fill_rect_alpha(0, 0, c.w as i32, STATUS_H, STATUS_BG, 165);
    c.gradient_v_alpha(0, STATUS_H / 2, STATUS_H, c.w as i32, STATUS_BG, 60, 0);
    let (_, _h) = st.effective_size();
    let top = (STATUS_H / 2 - 8) as i32;
    // time (HH:MM, device clock = host clock)
    let time = time_hhmm();
    c.text(28, top, &time, 3, TEXT);
    if !st.notifications.is_empty() {
        let nx = c.w as i32 / 2 - 6;
        c.glow(nx, STATUS_H / 2, 14, ACCENT, 110);
        c.circle(nx, STATUS_H / 2, 5, ACCENT);
        c.circle(nx, STATUS_H / 2, 2, ACCENT_HI);
    }
    // right cluster
    let mut x = c.w as i32 - 28;
    // battery: rounded shell + rounded fill + charge glow
    let bw = 36;
    let by = top + 1;
    c.round_rect_outline(x - bw, by, bw, 16, 4, DIM);
    let fill = ((bw - 6) as u32 * st.battery as u32 / 100) as i32;
    let bcol = if st.charging {
        GREEN
    } else if st.battery <= 15 {
        RED
    } else {
        TEXT
    };
    if st.charging {
        c.glow(x - bw + 2 + fill, by + 8, 12, GREEN, 60);
    }
    if fill > 0 {
        c.round_rect(x - bw + 3, by + 3, fill, 10, 3, bcol);
    }
    c.fill_rect(x - 3, by + 5, 3, 6, DIM);
    x -= bw + 12;
    let pct = format!("{}%", st.battery);
    x -= Canvas::text_w(&pct, 2);
    c.text(x + 8, top + 1, &pct, 2, DIM);
    // wifi arcs (level follows the live RSSI)
    if st.settings.get("wifi").map(String::as_str) == Some("on") {
        x -= 22;
        let arcs = st.wifi.1.clamp(1, 3) as i32;
        for (i, w) in [10, 16, 22].iter().enumerate() {
            let row = 3 - i as i32;
            let col = if row <= arcs { TEXT } else { DIMMER };
            let seg = w / 3;
            for k in 0..row {
                let yy = top + 14 - k * 5;
                let half = (w / 2 - k * 3).max(1);
                let cx0 = x - 12;
                c.hline(cx0 - half, yy, seg, col);
                c.hline(cx0 + half - seg, yy, seg, col);
            }
        }
    }
    // cellular: airplane glyph when powered off, otherwise live bars per slot
    // with the default data slot's network tag (5G/LTE)
    let data = &st.radio[(st.esim.data_slot - 1) as usize];
    if st.airplane_mode {
        draw_airplane(c, x - 20, top);
    } else {
        // network tag of the default data slot, next to its bars
        let tag = if data.registered {
            data.network_display.clone()
        } else {
            "No service".to_string()
        };
        x -= Canvas::text_w(&tag, 2) + 6;
        c.text(x, top + 4, &tag, 2, if data.registered { DIM } else { RED });
        // the non-data slot first (leftmost), then the data slot's bars
        for slot in [1u32, 2].iter().rev() {
            let r = &st.radio[(*slot - 1) as usize];
            let bars = if r.registered { r.bars } else { 0 };
            x -= 34;
            draw_signal_bars(c, x, top, bars, r.registered, *slot == st.esim.data_slot);
            if *slot == 2 && r.registered {
                // small carrier initial between the two bar groups
                let initial = r
                    .operator_alpha
                    .chars()
                    .next()
                    .map(|c| c.to_string())
                    .unwrap_or_default();
                x -= Canvas::text_w(&initial, 2) + 4;
                c.text(x, top + 4, &initial, 2, DIMMER);
            }
        }
    }
}

/// One signal-bar group (Android style: 4 ascending bars, dim when the
/// radio has no service; the data slot renders brighter).
fn draw_signal_bars(c: &mut Canvas, x0: i32, top: i32, bars: u32, registered: bool, is_data: bool) {
    let lit = if is_data { TEXT } else { DIM };
    for i in 0..4u32 {
        let bh = 6 + i * 4;
        let col = if registered && i < bars { lit } else { DIMMER };
        c.fill_rect(x0 + i as i32 * 7, top + 16 - bh as i32, 5, bh as i32, col);
    }
}

/// A small airplane glyph (fuselage + swept wings + tail).
fn draw_airplane(c: &mut Canvas, cx: i32, top: i32) {
    c.fill_rect(cx - 2, top + 1, 4, 15, TEXT); // fuselage
    c.fill_rect(cx - 11, top + 6, 22, 3, TEXT); // wings
    c.fill_rect(cx - 7, top + 12, 14, 2, TEXT); // rear wing
    c.fill_rect(cx - 1, top + 15, 2, 3, TEXT); // tail fin
}

/// Navigation bar: back, home, recents — glass strip, crisp glyphs.
fn draw_nav(c: &mut Canvas, st: &mut OsState) {
    let (_, h) = st.effective_size();
    let y0 = h as i32 - NAV_H;
    // glass strip: translucent fill + soft top fade + hairline edge light
    c.fill_rect_alpha(0, y0, c.w as i32, NAV_H, NAV_BG, 150);
    c.gradient_v_alpha(0, y0 - 14, y0, c.w as i32, NAV_BG, 0, 110);
    c.fill_rect_alpha(0, y0, c.w as i32, 1, [255, 255, 255], 70);
    let cy = y0 + NAV_H / 2;
    let cx = c.w as i32;
    // back triangle — layered strokes give it a subtle anti-aliased feel
    let bx = cx / 6;
    for i in (0..12).rev() {
        let alpha = if i < 4 { 200 + i * 12 } else { 130 };
        c.fill_rect_alpha(
            bx + 12 - i,
            cy - i,
            i * 2 + 2,
            1,
            TEXT,
            alpha.min(255) as u8,
        );
    }
    // home ring — thin double ring reads crisper than a fat one
    c.ring(cx / 2, cy, 14, mix(TEXT, NAV_BG, 0.55));
    c.ring(cx / 2, cy, 13, TEXT);
    // recents square — 1 px outline now that the primitive is crisp
    let rx = cx * 5 / 6;
    c.round_rect_outline(rx - 12, cy - 12, 24, 24, 4, TEXT);
}

/// Home launcher: clock widget, glass search pill, app grid, glass dock, dots.
fn draw_launcher(c: &mut Canvas, st: &OsState) {
    let (w, h) = st.effective_size();
    let wi = w as i32;
    let top = STATUS_H;
    let bottom = h as i32 - NAV_H;
    // clock widget: halo glow behind large, thin-feeling numerals
    c.glow(wi / 2, top + 70, 220, [90, 110, 200], 46);
    c.text_center(wi / 2, top + 40, &time_hhmm(), 9, TEXT);
    c.text_center(wi / 2, top + 130, &date_str(), 3, DIM);
    // search pill: floating dark glass with a drop shadow and top sheen
    let pw = wi - 96;
    let py = top + 190;
    c.shadow(48, py, pw, 64, 32, 55);
    c.round_rect_alpha(48, py, pw, 64, 32, [10, 14, 22], 150);
    c.round_rect_alpha(50, py + 2, pw - 4, 26, 24, [255, 255, 255], 22);
    c.round_rect_outline(48, py, pw, 64, 32, mix(SURFACE_HI, [255, 255, 255], 0.25));
    // search glyph: accent ring + handle
    c.glow(48 + 34, py + 32, 16, ACCENT, 60);
    c.circle(48 + 34, py + 32, 12, ACCENT);
    c.circle(
        48 + 34,
        py + 32,
        8,
        mix([10, 14, 22], [255, 255, 255], 0.16),
    );
    c.line(48 + 42, py + 40, 48 + 50, py + 48, ACCENT_HI, 4);
    c.text(48 + 60, py + 22, "Search", 3, DIM);
    // pages: page 0 = built-in launchables, page 1 = user-installed
    let page = st.home_page.min(1);
    let apps: Vec<AppId> = builtin_launchables()
        .into_iter()
        .chain(
            st.packages
                .iter()
                .filter(|p| !p.system)
                .map(|p| AppId::User(p.package.clone())),
        )
        .collect();
    let per_page = 12usize;
    let pages = apps.len().div_ceil(per_page).max(1);
    let page = page.min(pages - 1);
    let slice: Vec<AppId> = apps
        .iter()
        .skip(page * per_page)
        .take(per_page)
        .cloned()
        .collect();
    let cols = 4usize;
    let cell_w = wi / cols as i32;
    let grid_y = py + 110;
    for (i, app) in slice.iter().enumerate() {
        let cx = (i % cols) as i32 * cell_w + cell_w / 2;
        let cy = grid_y + (i / cols) as i32 * 170;
        draw_app_icon(c, app, cx - 44, cy, 88);
        // ellipsize like a real launcher (cell fits ~11 chars at this size)
        let label = {
            let l = app.label();
            if l.chars().count() > 11 {
                format!("{}…", l.chars().take(10).collect::<String>())
            } else {
                l
            }
        };
        // label with a soft contact shadow for legibility over the wallpaper
        c.text_center(cx + 1, cy + 105, &label, 2, [0, 0, 0]);
        c.text_center(cx, cy + 104, &label, 2, TEXT);
    }
    // dock: floating glass tray with sheen and hairline edge
    let dock_apps = [
        AppId::Dialer,
        AppId::Messages,
        AppId::Browser,
        AppId::Settings,
    ];
    let dy = bottom - 150;
    c.shadow(40, dy - 12, wi - 80, 128, 36, 70);
    c.round_rect_alpha(40, dy - 12, wi - 80, 128, 36, [10, 14, 22], 130);
    c.round_rect_alpha(44, dy - 10, wi - 88, 40, 30, [255, 255, 255], 18);
    c.round_rect_outline(
        40,
        dy - 12,
        wi - 80,
        128,
        36,
        mix(SURFACE_HI, [255, 255, 255], 0.22),
    );
    let dw = (wi - 80) / 4;
    for (i, app) in dock_apps.iter().enumerate() {
        let i = i as i32;
        draw_app_icon(c, app, 40 + i * dw + dw / 2 - 40, dy, 80);
    }
    // page dots: the active page is a pill, the rest are quiet dots
    let dots_y = dy - 34;
    for i in 0..pages as i32 {
        let dx = wi / 2 - (pages as i32 * 24) / 2 + i * 24;
        if i as usize == page {
            c.round_rect(dx - 10, dots_y - 5, 24, 10, 5, ACCENT);
        } else {
            c.circle(dx + 2, dots_y, 5, DIMMER);
        }
    }
}

pub fn builtin_launchables() -> Vec<AppId> {
    vec![
        AppId::Settings,
        AppId::Calculator,
        AppId::Clock,
        AppId::Files,
        AppId::Messages,
        AppId::Dialer,
        AppId::Browser,
        AppId::Photos,
        AppId::Termux,
        // the root stack — installed apps, on the launcher like any other
        AppId::Magisk,
        AppId::RootExplorer,
        AppId::TitaniumBackup,
    ]
}

fn draw_running_app(c: &mut Canvas, st: &mut OsState, app: &AppId) {
    // app surface over the wallpaper
    c.fill_rect(0, STATUS_H, c.w as i32, c.h as i32 - STATUS_H - NAV_H, BG);
    crate::apps::render_app(c, st, app);
}

/// Recents screen: horizontal cards for the running/recent apps.
pub fn draw_recents(c: &mut Canvas, st: &mut OsState) {
    let (w, h) = st.effective_size();
    let wi = w as i32;
    c.fill_rect_alpha(
        0,
        STATUS_H,
        wi,
        h as i32 - STATUS_H - NAV_H,
        [8, 10, 14],
        235,
    );
    c.text_center(wi / 2, STATUS_H + 40, "Recent apps", 3, TEXT);
    if st.recents.is_empty() {
        c.text_center(wi / 2, h as i32 / 2, "No recent items", 3, DIM);
        return;
    }
    // card strip
    let card_w = (wi / 3).min(300);
    let card_h = (card_w as f32 * 1.9) as i32;
    let y = STATUS_H + 120;
    for (i, app) in st.recents.iter().enumerate().take(5) {
        let x = wi / 2 - card_w / 2 + (i as i32 - 2) * (card_w + 24);
        if x + card_w < 0 || x > wi {
            continue;
        }
        // side cards recede: dimmed and desaturated by distance from centre
        let dist = (i as i32 - 2).abs();
        let dim_t = (dist as f32 * 0.22).min(0.55);
        let is_active = dist == 0;
        c.shadow(x, y, card_w, card_h, 26, if is_active { 85 } else { 40 });
        c.round_rect(x, y, card_w, card_h, 26, SURFACE);
        // inner preview area: a quiet gradient with the app icon
        c.round_rect_gradient_v(
            x + 12,
            y + 12,
            card_w - 24,
            card_h - 130,
            18,
            mix(BG, [255, 255, 255], 0.05),
            mix(BG, [0, 0, 0], 0.25),
        );
        draw_app_icon(c, app, x + card_w / 2 - 40, y + 40, 80);
        c.text_center(x + card_w / 2, y + 150, &app.label(), 2, TEXT);
        // active card gets the accent hairline
        if is_active {
            c.round_rect_outline(x, y, card_w, card_h, 26, ACCENT);
        } else {
            c.round_rect_outline(
                x,
                y,
                card_w,
                card_h,
                26,
                mix(SURFACE_HI, [255, 255, 255], 0.15),
            );
        }
        // recede overlay: translucent black veil, flat — not over the icon glyph
        if dim_t > 0.0 {
            let a = (dim_t * 255.0) as u8;
            c.round_rect_alpha(x, y, card_w, card_h, 26, [0, 0, 0], a);
        }
    }
}

/// Notification banner at the top of the screen — floating glass card.
fn draw_banner(c: &mut Canvas, st: &mut OsState) {
    if let Some((title, body, at)) = &st.banner {
        if at.elapsed().unwrap_or_default().as_millis() < 2600 {
            let w = c.w as i32;
            let bx = 16;
            let by = STATUS_H + 8;
            let bw = w - 32;
            c.shadow(bx, by, bw, 96, 24, 80);
            c.round_rect_alpha(bx, by, bw, 96, 24, [14, 18, 26], 225);
            c.round_rect_alpha(bx + 3, by + 2, bw - 6, 30, 20, [255, 255, 255], 16);
            c.round_rect_outline(bx, by, bw, 96, 24, mix(SURFACE_HI, [255, 255, 255], 0.2));
            // accent bar — rounded, glowing
            c.glow(bx + 4, by + 48, 26, ACCENT, 70);
            c.round_rect(bx, by + 12, 6, 72, 3, ACCENT);
            c.text(bx + 28, by + 24, title, 3, TEXT);
            c.text_fit(bx + 28, by + 58, body, 2, bw - 60, DIM);
        }
    }
}

fn time_hhmm() -> String {
    let secs = crate::state::now_secs() as u64;
    let (_, hh, mm) = hms(secs);
    format!("{hh:02}:{mm:02}")
}

fn hms(secs: u64) -> (u64, u64, u64) {
    let ss = secs % 60;
    let mm = (secs / 60) % 60;
    let hh = (secs / 3600) % 24;
    (ss, hh, mm)
}

fn date_str() -> String {
    // days since epoch → YYYY-MM-DD (proleptic Gregorian, no deps)
    let days = crate::state::now_secs() as u64 / 86400;
    let (y, m, d) = civil_from_days(days as i64);
    format!("{y:04}-{m:02}-{d:02}")
}

pub fn civil_from_days(z: i64) -> (i64, u32, u32) {
    let z = z + 719_468;
    let era = if z >= 0 { z } else { z - 146_096 } / 146_097;
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146_096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = (doy - (153 * mp + 2) / 5 + 1) as u32;
    let m = if mp < 10 { mp + 3 } else { mp - 9 } as u32;
    (if m <= 2 { y + 1 } else { y }, m, d)
}

// ── touch routing ────────────────────────────────────────────────────────────

fn handle_touch(st: &mut OsState, t: TouchEvent) {
    let (w, h) = st.effective_size();

    match t.action {
        TouchAction::Down => {
            st.drag = Some((t.x as i32, t.y as i32));
        }
        TouchAction::Move => {}
        TouchAction::Up => {
            let Some((sx, sy)) = st.drag.take() else {
                return;
            };
            let dx = t.x as i32 - sx;
            let dy = t.y as i32 - sy;
            let x = t.x as i32;
            let y = t.y as i32;

            // nav bar zone (press or release inside it)
            if sy >= h as i32 - NAV_H || y >= h as i32 - NAV_H {
                nav_tap(st, x, y, h as i32);
                return;
            }
            // fling gestures
            if dx.abs() > 140 && dx.abs() > dy.abs() {
                if st.current.is_none() {
                    if dx < 0 && st.home_page == 0 {
                        st.home_page = 1;
                    } else if dx > 0 && st.home_page > 0 {
                        st.home_page = 0;
                    }
                    st.dirty = true;
                    return;
                }
                if st.current == Some(AppId::Browser) {
                    crate::apps::browser_nav(st, if dx > 0 { -1 } else { 1 });
                    st.dirty = true;
                    return;
                }
            }
            // everything else is a tap at the release point
            tap(st, x, y, w as i32, h as i32);
        }
    }
}

/// A tap at device coordinates (recents screen, home, or the open app).
fn tap(st: &mut OsState, x: i32, y: i32, w: i32, h: i32) {
    if y >= h - NAV_H {
        nav_tap(st, x, y, h);
        return;
    }
    if st.recents_screen_open {
        // tap a card
        let card_w = (w / 3).min(300);
        let card_h = (card_w as f32 * 1.9) as i32;
        let ry = STATUS_H + 120;
        for (i, app) in st.recents.iter().enumerate().take(5) {
            let cx = w / 2 - card_w / 2 + (i as i32 - 2) * (card_w + 24);
            if in_rect(x, y, cx, ry, card_w, card_h) {
                let app = app.clone();
                st.recents_screen_open = false;
                st.launch_app(app);
                return;
            }
        }
        st.recents_screen_open = false;
        st.dirty = true;
        return;
    }
    if st.current.is_none() {
        home_tap(st, x, y, w, h);
    } else {
        let app = st.current.clone().unwrap_or(AppId::Settings);
        crate::apps::touch_app(st, &app, x, y);
    }
}

fn nav_tap(st: &mut OsState, x: i32, y: i32, h: i32) {
    let (w, _) = st.effective_size();
    let nav_y = h - NAV_H;
    if y < nav_y {
        return;
    }
    let cy = nav_y + NAV_H / 2;
    let _ = cy;
    let cx = w as i32;
    if x < cx / 3 {
        // back
        if st.recents_screen_open {
            st.recents_screen_open = false;
            st.dirty = true;
            return;
        }
        if let Some(app) = &st.current {
            let app = app.clone();
            if !crate::apps::go_back(st, &app) {
                st.current = None; // close app → home
                st.dirty = true;
            }
        }
    } else if x < cx * 2 / 3 {
        // home
        st.recents_screen_open = false;
        st.current = None;
        st.home_page = 0;
    } else {
        // recents
        st.recents_screen_open = !st.recents_screen_open;
    }
    st.dirty = true;
}

fn home_tap(st: &mut OsState, x: i32, y: i32, w: i32, h: i32) {
    // recents screen?
    if st.recents_screen_open {
        // tap a card
        let card_w = (w / 3).min(300);
        let card_h = (card_w as f32 * 1.9) as i32;
        let ry = STATUS_H + 120;
        for (i, app) in st.recents.iter().enumerate().take(5) {
            let cx = w / 2 - card_w / 2 + (i as i32 - 2) * (card_w + 24);
            if in_rect(x, y, cx, ry, card_w, card_h) {
                let app = app.clone();
                st.recents_screen_open = false;
                st.launch_app(app);
                return;
            }
        }
        st.recents_screen_open = false;
        st.dirty = true;
        return;
    }
    let top = STATUS_H;
    let bottom = h - NAV_H;
    // search pill → browser
    let py = top + 190;
    if in_rect(x, y, 48, py, w - 96, 64) {
        st.launch_app(AppId::Browser);
        return;
    }
    // dock
    let dock_apps = [
        AppId::Dialer,
        AppId::Messages,
        AppId::Browser,
        AppId::Settings,
    ];
    let dy = bottom - 150;
    if y >= dy - 12 && y < dy + 116 {
        let dw = (w - 80) / 4;
        for (i, app) in dock_apps.iter().enumerate() {
            if in_rect(x, y, 40 + i as i32 * dw, dy, dw, 116) {
                let app = app.clone();
                st.launch_app(app);
                return;
            }
        }
    }
    // grid icons
    let page = st.home_page.min(1);
    let apps: Vec<AppId> = builtin_launchables()
        .into_iter()
        .chain(
            st.packages
                .iter()
                .filter(|p| !p.system)
                .map(|p| AppId::User(p.package.clone())),
        )
        .collect();
    let per_page = 12usize;
    let pages = apps.len().div_ceil(per_page).max(1);
    let page = page.min(pages - 1);
    let slice: Vec<AppId> = apps
        .iter()
        .skip(page * per_page)
        .take(per_page)
        .cloned()
        .collect();
    let cols = 4usize;
    let cell_w = w / cols as i32;
    let grid_y = py + 110;
    for (i, app) in slice.iter().enumerate() {
        let cx = (i % cols) as i32 * cell_w + cell_w / 2;
        let cy = grid_y + (i / cols) as i32 * 170;
        if in_rect(x, y, cx - 48, cy - 8, 96, 150) {
            let app = app.clone();
            st.launch_app(app);
            return;
        }
    }
}

fn handle_key(st: &mut OsState, k: KeyCode) {
    match k {
        KeyCode::Home => {
            st.current = None;
            st.recents_screen_open = false;
            st.screen_off = false;
        }
        KeyCode::Back => {
            if let Some(app) = &st.current {
                let app = app.clone();
                if !crate::apps::go_back(st, &app) {
                    st.current = None;
                }
            }
        }
        KeyCode::Enter => {
            if let Some(app) = st.current.clone() {
                crate::apps::key_enter(st, &app);
            }
        }
        KeyCode::Del => {
            if let Some(app) = st.current.clone() {
                crate::apps::key_del(st, &app);
            }
        }
        KeyCode::VolumeUp | KeyCode::VolumeDown | KeyCode::Power => {}
    }
    st.dirty = true;
}

fn handle_text(st: &mut OsState, text: &str) {
    if let Some(app) = &st.current {
        let app = app.clone();
        crate::apps::text_input(st, &app, text);
    }
    st.dirty = true;
}
