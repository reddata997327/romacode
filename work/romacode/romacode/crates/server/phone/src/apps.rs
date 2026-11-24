//! The apps. Each one is a real screen with its own state, rendering and
//! touch handling — no mock content anywhere: lists show what the device
//! actually has, the browser makes real network requests, the files app
//! browses the real sdcard directory.

use crate::compose::{civil_from_days, KB_H, NAV_H, STATUS_H};
use crate::gfx::{in_rect, mix, Canvas, Rgb};
use crate::state::*;
use crate::theme::*;
use crate::theme::{ACCENT, BG, DIM, DIMMER, GREEN, RED, SURFACE, SURFACE_HI, TEXT};

// ── dispatch ─────────────────────────────────────────────────────────────────

pub fn render_app(c: &mut Canvas, st: &mut OsState, app: &AppId) {
    match app {
        AppId::Settings => render_settings(c, st),
        AppId::Calculator => render_calculator(c, st),
        AppId::Clock => render_clock(c, st),
        AppId::Messages => render_messages(c, st),
        AppId::Dialer => render_dialer(c, st),
        AppId::Browser => render_browser(c, st),
        AppId::Photos => render_photos(c, st),
        AppId::Files => render_files(c, st),
        AppId::Termux => render_termux(c, st),
        AppId::Magisk => render_magisk(c, st),
        AppId::RootExplorer => render_root_explorer(c, st),
        AppId::TitaniumBackup => render_titanium(c, st),
        AppId::User(_) => render_user_app(c, st, app),
    }
}

/// `true` if the app consumed Back internally (keeps it open); `false` = close.
pub fn go_back(st: &mut OsState, app: &AppId) -> bool {
    match app {
        AppId::Settings => {
            let ui = &mut st.app.settings;
            if ui.sel_app.is_some() {
                ui.sel_app = None;
            } else if ui.section != 0 {
                ui.section = 0;
            } else {
                return false;
            }
            true
        }
        AppId::Messages => {
            let ui = &mut st.app.messages;
            if ui.kb.visible {
                ui.kb.visible = false;
                ui.field = 0;
                return true;
            }
            if ui.open_thread.is_some() {
                ui.open_thread = None;
                ui.compose_to.clear();
                ui.compose_text.clear();
            } else {
                return false;
            }
            true
        }
        AppId::Dialer => {
            let ui = &mut st.app.dialer;
            if ui.call.is_some() {
                end_call(st);
                true
            } else if !ui.number.is_empty() {
                ui.number.pop();
                true
            } else {
                false
            }
        }
        AppId::Browser => {
            let ui = &mut st.app.browser;
            if ui.kb.visible {
                ui.kb.visible = false;
                ui.url_bar_focus = false;
                return true;
            }
            if ui.error.is_some() {
                ui.error = None;
                ui.lines.clear();
                return true;
            }
            if ui.history_idx.map_or(false, |i| i > 0) {
                browser_nav(st, -1);
                return true;
            }
            false
        }
        AppId::Photos => {
            let ui = &mut st.app.photos;
            if ui.viewer.is_some() {
                ui.viewer = None;
                true
            } else {
                false
            }
        }
        AppId::Files => {
            let ui = &mut st.app.files;
            if ui.detail.is_some() {
                ui.detail = None;
                return true;
            }
            if ui.cwd.parent().is_some() {
                ui.cwd = ui.cwd.parent().unwrap().to_path_buf();
                refresh_files(st);
                true
            } else {
                false
            }
        }
        AppId::Termux => {
            let ui = &mut st.app.termux;
            if ui.kb.visible {
                ui.kb.visible = false;
                true
            } else {
                false
            }
        }
        AppId::Magisk => {
            let ui = &mut st.app.magisk;
            if ui.sel_grant.is_some() {
                ui.sel_grant = None;
                true
            } else {
                false
            }
        }
        AppId::RootExplorer => {
            let ui = &mut st.app.rootexplorer;
            if ui.detail.is_some() {
                ui.detail = None;
                return true;
            }
            match &ui.cwd {
                None => false,
                Some(cwd) => {
                    if cwd == "/" {
                        ui.cwd = None;
                    } else {
                        ui.cwd = Some(
                            std::path::Path::new(cwd)
                                .parent()
                                .map(|p| p.display().to_string())
                                .unwrap_or_else(|| "/".to_string()),
                        );
                    }
                    refresh_root_explorer(st);
                    true
                }
            }
        }
        AppId::TitaniumBackup => {
            let ui = &mut st.app.titanium;
            if ui.log.len() > 12 {
                ui.log.clear();
                true
            } else {
                false
            }
        }
        AppId::Clock => false,
        AppId::Calculator => false,
        AppId::User(_) => false,
    }
}

pub fn key_enter(st: &mut OsState, app: &AppId) {
    match app {
        AppId::Termux => {
            let cmd = st.app.termux.input.trim().to_string();
            if !cmd.is_empty() {
                termux_run_command(st, &cmd, true);
            }
        }
        AppId::Messages => {
            let ui = &st.app.messages;
            if ui.open_thread.is_some() && !ui.compose_text.is_empty() {
                send_composed_sms(st);
            }
        }
        AppId::Browser => {
            navigate_browser(st);
        }
        _ => {}
    }
}

pub fn key_del(st: &mut OsState, app: &AppId) {
    match app {
        AppId::Termux => {
            st.app.termux.input.pop();
        }
        AppId::Messages => {
            let ui = &mut st.app.messages;
            if ui.field == 0 {
                ui.compose_to.pop();
            } else {
                ui.compose_text.pop();
            }
        }
        AppId::Dialer => {
            st.app.dialer.number.pop();
        }
        AppId::Browser => {
            st.app.browser.url_bar.pop();
        }
        _ => {}
    }
}

pub fn text_input(st: &mut OsState, app: &AppId, text: &str) {
    match app {
        AppId::Termux => {
            st.app.termux.input.push_str(text);
        }
        AppId::Messages => {
            let ui = &mut st.app.messages;
            if ui.field == 0 {
                ui.compose_to.push_str(text);
            } else {
                ui.compose_text.push_str(text);
            }
        }
        AppId::Dialer => {
            let digits: String = text
                .chars()
                .filter(|ch| ch.is_ascii_digit() || *ch == '+' || *ch == '#')
                .collect();
            st.app.dialer.number.push_str(&digits);
        }
        AppId::Browser => {
            st.app.browser.url_bar.push_str(text);
        }
        _ => {}
    }
}

// ── chrome shared by apps ────────────────────────────────────────────────────

/// App bar: title + accent underline.
fn app_bar(c: &mut Canvas, title: &str) {
    c.fill_rect(0, STATUS_H, c.w as i32, 96, SURFACE);
    c.text(36, STATUS_H + 26, title, 5, TEXT);
    c.fill_rect(0, STATUS_H + 94, c.w as i32, 2, mix(APP_BAR_LINE, BG, 0.0));
}

const APP_BAR_LINE: Rgb = [56, 66, 82];

fn row(c: &mut Canvas, y: i32, h: i32, title: &str, sub: &str, accent: Rgb) {
    let w = c.w as i32;
    c.fill_rect(0, y, w, h, SURFACE);
    c.fill_rect(0, y, 10, h, accent);
    c.text(40, y + h / 2 - 12, title, 4, TEXT);
    if !sub.is_empty() {
        let tw = Canvas::text_w(title, 4);
        c.text(40 + tw + 24, y + h / 2 - 6, sub, 2, DIM);
    }
    c.fill_rect(0, y + h - 1, w, 1, [16, 20, 26]);
}

fn toggle(c: &mut Canvas, x: i32, y: i32, on: bool) {
    let w = 72;
    c.round_rect(x, y, w, 36, 18, if on { GREEN } else { DIMMER });
    let kx = if on { x + w - 34 } else { x + 2 };
    c.circle(kx + 16, y + 18, 16, TEXT);
}

// ── Settings ─────────────────────────────────────────────────────────────────

fn render_settings(c: &mut Canvas, st: &OsState) {
    let (w, h) = st.effective_size();
    let wi = w as i32;
    let content_h = h as i32 - STATUS_H - NAV_H;
    let ui = &st.app.settings;
    if let Some(pkg) = ui.sel_app.clone() {
        app_bar(c, "App info");
        let y = STATUS_H + 120;
        let label = st
            .package(&pkg)
            .map(|p| p.label.clone())
            .unwrap_or_default();
        let version = st
            .package(&pkg)
            .map(|p| p.version.clone())
            .unwrap_or_default();
        let system = st.package(&pkg).map(|p| p.system).unwrap_or(true);
        c.round_rect(36, y, wi - 72, 200, 24, SURFACE);
        draw_app_icon(c, &AppId::User(pkg.clone()), wi / 2 - 44, y + 24, 88);
        c.text_center(wi / 2, y + 130, &label, 4, TEXT);
        c.text_center(wi / 2, y + 165, &pkg, 2, DIM);
        c.text_center(wi / 2, y + 240, &format!("version {version}"), 3, TEXT);
        c.text_center(
            wi / 2,
            y + 275,
            if system {
                "system package"
            } else {
                "user install"
            },
            2,
            DIM,
        );
        if !system {
            let by = y + 330;
            c.round_rect(wi / 2 - 120, by, 240, 64, 16, RED);
            c.text_center(wi / 2, by + 20, "Uninstall", 4, TEXT);
        }
        return;
    }
    match ui.section {
        0 => {
            app_bar(c, "Settings");
            let entries = [
                ("Network & internet", net_sub(st), ACCENT),
                ("Battery", format!("{}%", st.battery), GREEN),
                ("Display", display_sub(st), PURPLE),
                ("Apps", format!("{} installed", st.packages.len()), TEAL),
                ("System update", os_sub(st), ORANGE),
                ("About phone", st.identity.model.to_string(), ACCENT),
            ];
            let y0 = STATUS_H + 120;
            let rh = 110;
            for (i, (title, sub, col)) in entries.iter().enumerate() {
                row(c, y0 + i as i32 * rh, rh, title, sub, *col);
            }
        }
        1 => {
            app_bar(c, "Network & internet");
            let y0 = STATUS_H + 120;
            let wifi_on = st.settings.get("wifi").map(String::as_str) == Some("on");
            let bt_on = st.settings.get("bluetooth").map(String::as_str) == Some("on");
            let ssid = st
                .settings
                .get("wifi_ssid")
                .cloned()
                .unwrap_or_else(|| mandroid::identity::WIFI_SSID.into());
            row(
                c,
                y0,
                110,
                "Wi-Fi",
                &format!("{ssid} - {} dBm", st.wifi.0),
                ACCENT,
            );
            c.round_rect(
                wi - 130,
                y0 + 36,
                72,
                36,
                18,
                if wifi_on { GREEN } else { DIMMER },
            );
            toggle(c, wi - 130, y0 + 36, wifi_on);
            row(
                c,
                y0 + 110,
                110,
                "Bluetooth",
                if bt_on { "On" } else { "Off" },
                ACCENT,
            );
            toggle(c, wi - 130, y0 + 146, bt_on);
            row(
                c,
                y0 + 220,
                110,
                "Airplane mode",
                if st.airplane_mode { "On" } else { "Off" },
                ACCENT,
            );
            toggle(c, wi - 130, y0 + 256, st.airplane_mode);
            // the two registered eSIMs, live
            for (i, slot) in [1u32, 2].iter().enumerate() {
                let y = y0 + 330 + i as i32 * 110;
                let r = st.radio_now(*slot);
                let sub = if r.registered {
                    let tag = if st.esim.data_slot == *slot {
                        " - default data"
                    } else {
                        ""
                    };
                    format!(
                        "{} - {} - {} dBm ({}/4 bars){}",
                        r.operator_alpha, r.technology, r.rsrp, r.bars, tag
                    )
                } else {
                    "no service".into()
                };
                row(c, y, 110, &format!("eSIM {}", slot), &sub, TEAL);
            }
        }
        2 => {
            app_bar(c, "Battery");
            let y0 = STATUS_H + 120;
            c.round_rect(36, y0, wi - 72, 220, 24, SURFACE);
            let bw = (wi - 200) * st.battery as u32 as i32 / 100;
            c.round_rect(60, y0 + 50, wi - 120, 56, 14, [30, 36, 46]);
            c.round_rect(
                60,
                y0 + 50,
                bw.max(12),
                56,
                14,
                if st.charging { GREEN } else { ACCENT },
            );
            c.text_center(wi / 2, y0 + 130, &format!("{}%", st.battery), 6, TEXT);
            c.text_center(wi / 2, y0 + 170, "4700 mAh (Li-Po)", 2, DIM);
            c.text_center(
                wi / 2,
                y0 + 200,
                if st.charging {
                    "Charging (AC adapter)"
                } else {
                    "On battery"
                },
                2,
                DIM,
            );
            let by = y0 + 260;
            c.round_rect(wi / 2 - 140, by, 280, 64, 16, SURFACE_HI);
            c.text_center(
                wi / 2,
                by + 20,
                if st.charging {
                    "Unplug charger"
                } else {
                    "Plug in charger"
                },
                4,
                TEXT,
            );
        }
        3 => {
            app_bar(c, "Display");
            let y0 = STATUS_H + 120;
            let (ew, eh) = st.effective_size();
            c.text(40, y0 + 6, "Resolution", 3, DIM);
            let res_opts: [(u32, u32, &str); 4] = [
                (
                    PANEL_W,
                    PANEL_H,
                    &format!("{}x{} (default)", PANEL_W, PANEL_H),
                ),
                (1080, 2400, "1080x2400"),
                (540, 1200, "540x1200"),
                (720, 1280, "720x1280"),
            ];
            for (i, (rw, rh, label)) in res_opts.iter().enumerate() {
                let y = y0 + 40 + i as i32 * 80;
                let sel = (ew, eh) == (*rw, *rh);
                c.circle(60, y + 28, 16, if sel { ACCENT } else { DIMMER });
                if sel {
                    c.circle(60, y + 28, 8, TEXT);
                }
                c.text(96, y + 16, label, 4, if sel { TEXT } else { DIM });
            }
            let dy = y0 + 40 + 4 * 80 + 24;
            c.text(40, dy, "Density", 3, DIM);
            let dens_opts = [420u32, 340, 480, 560];
            for (i, d) in dens_opts.iter().enumerate() {
                let y = dy + 40 + i as i32 * 80;
                let sel = st.effective_density() == *d;
                c.circle(60, y + 28, 16, if sel { ACCENT } else { DIMMER });
                if sel {
                    c.circle(60, y + 28, 8, TEXT);
                }
                let label = if *d == 420 {
                    format!("{d} dpi (default)")
                } else {
                    format!("{d} dpi")
                };
                c.text(96, y + 16, &label, 4, if sel { TEXT } else { DIM });
            }
            let ry = dy + 40 + 4 * 80 + 30;
            c.round_rect(40, ry, wi - 80, 64, 16, SURFACE_HI);
            c.text_center(wi / 2, ry + 20, "Reset display settings", 4, ACCENT);
        }
        4 => {
            app_bar(c, "System update");
            let y0 = STATUS_H + 120;
            let wi = w as i32;
            let installed = crate::os::read_manifest(&st.data_dir);
            c.round_rect(36, y0, wi - 72, 300, 24, SURFACE);
            c.text(64, y0 + 30, "OS image", 3, DIM);
            match &installed {
                Some(m) => {
                    c.text_fit(
                        64,
                        y0 + 60,
                        &format!("Android {} ({})", m.version, m.build_id),
                        5,
                        wi - 140,
                        TEXT,
                    );
                    c.text_fit(64, y0 + 120, &m.fingerprint, 2, wi - 140, DIM);
                    c.text(
                        64,
                        y0 + 160,
                        &format!(
                            "{} files, {} KiB — installed {}",
                            m.files,
                            m.bytes / 1024,
                            {
                                let days = (now_ms() - m.installed_at_ms) / 1000 / 86400;
                                if days <= 0 {
                                    "today".to_string()
                                } else {
                                    format!("{days}d ago")
                                }
                            }
                        ),
                        2,
                        DIM,
                    );
                    c.text(64, y0 + 195, &format!("kernel: {}", m.kernel), 2, DIM);
                }
                None => {
                    c.text(64, y0 + 60, "No OS image installed", 5, RED);
                    c.text(
                        64,
                        y0 + 120,
                        "The handset cannot boot without an image.",
                        3,
                        DIM,
                    );
                }
            }
            let by = y0 + 220;
            c.round_rect(wi / 2 - 170, by, 340, 64, 16, ACCENT);
            c.text_center(wi / 2, by + 20, "Install OS image", 4, [10, 14, 20]);
            let py = by + 96;
            c.text_fit(
                36,
                py,
                "The image is flashed through the bootloader: partition table → extract → \
                 checksum verify → manifest stamp. Root (Magisk 29.0) ships on the image; \
                 the device is rooted out of the box.",
                2,
                wi - 72,
                DIM,
            );
        }
        5 => {
            app_bar(c, "About phone");
            let y0 = STATUS_H + 120;
            let facts: Vec<(&str, String)> = vec![
                ("Device model", st.identity.model.to_string()),
                ("Device name", st.identity.device.to_string()),
                ("Android version", st.android_release.clone()),
                ("SDK level", st.sdk_level.to_string()),
                ("Security patch", st.identity.security_patch.to_string()),
                ("Build fingerprint", st.fingerprint.clone()),
                ("Build type/tags", "user / release-keys".into()),
                ("Baseband", st.identity.baseband.clone()),
                ("Kernel", st.identity.kernel.to_string()),
                ("IMEI (slot 1)", st.identity.imei.clone()),
                ("IMEI (slot 2)", st.identity.imei2.clone()),
                ("Serial number", st.identity.serial.clone()),
                ("eUICC EID", st.identity.eid.clone()),
                (
                    "SoC",
                    format!("{} ({} cores)", st.identity.soc_model, st.cpu_cores),
                ),
                ("RAM", format!("{} GB", st.ram_mib / 1024)),
                (
                    "Storage",
                    if st.rom_gib >= 900 {
                        "1 TB".to_string()
                    } else {
                        format!("{} GB", st.rom_gib)
                    },
                ),
                (
                    "Root access",
                    if st.rooted {
                        format!(
                            "root (Magisk {}) — su in /system/xbin",
                            crate::os::magisk_version()
                        )
                    } else {
                        "unavailable".to_string()
                    },
                ),
                ("Reboots this session", st.reboots.to_string()),
                ("Uptime", uptime_str(st)),
            ];
            let rh = 86;
            let vis = ((content_h - 120) / rh).max(1) as usize;
            let scroll = ui.scroll.min(facts.len().saturating_sub(vis));
            for (i, (k, v)) in facts.iter().skip(scroll).take(vis).enumerate() {
                let y = y0 + i as i32 * rh;
                c.fill_rect(0, y, wi, rh, if i % 2 == 0 { SURFACE } else { BG });
                c.text(40, y + 10, k, 2, DIM);
                c.text_fit(40, y + 34, v, 3, wi - 80, TEXT);
            }
        }
        _ => {}
    }
}

fn net_sub(st: &OsState) -> String {
    if st.settings.get("wifi").map(String::as_str) == Some("on") {
        "Wi-Fi on".into()
    } else {
        "Wi-Fi off".into()
    }
}

fn display_sub(st: &OsState) -> String {
    let (w, h) = st.effective_size();
    format!("{}x{} @{}", w, h, st.effective_density())
}

/// The System update row subtitle — the installed OS image (or its absence).
fn os_sub(st: &OsState) -> String {
    if let Some(m) = crate::os::read_manifest(&st.data_dir) {
        format!("Android {} ({})", m.version, m.build_id)
    } else {
        "install OS image".into()
    }
}

fn uptime_str(st: &OsState) -> String {
    let s = st.booted_at.elapsed().unwrap_or_default().as_secs();
    format!("{}m {:02}s", s / 60, s % 60)
}

fn touch_settings(st: &mut OsState, x: i32, y: i32) {
    let (w, h) = st.effective_size();
    let wi = w as i32;
    let content_h = h as i32 - STATUS_H - NAV_H;
    let section = st.app.settings.section;
    match section {
        0 => {
            let entries = 6;
            let y0 = STATUS_H + 120;
            let rh = 110;
            for i in 0..entries {
                if in_rect(x, y, 0, y0 + i as i32 * rh, wi, rh) {
                    st.app.settings.section = i + 1;
                    st.app.settings.scroll = 0;
                    st.dirty = true;
                    return;
                }
            }
        }
        1 => {
            let y0 = STATUS_H + 120;
            if in_rect(x, y, wi - 130, y0 + 36, 130, 80) {
                let cur = st.settings.get("wifi").map(String::as_str) == Some("on");
                st.settings
                    .insert("wifi".into(), if cur { "off" } else { "on" }.into());
                st.persist_dirty = true;
                st.dirty = true;
                return;
            }
            if in_rect(x, y, wi - 130, y0 + 146, 130, 80) {
                let cur = st.settings.get("bluetooth").map(String::as_str) == Some("on");
                st.settings
                    .insert("bluetooth".into(), if cur { "off" } else { "on" }.into());
                st.persist_dirty = true;
                st.dirty = true;
                return;
            }
            if in_rect(x, y, wi - 130, y0 + 256, 130, 80) {
                st.airplane_mode = !st.airplane_mode;
                st.notify(
                    "Network",
                    if st.airplane_mode {
                        "Airplane mode on — radios off"
                    } else {
                        "Airplane mode off — reattaching"
                    },
                    None,
                );
                st.persist_dirty = true;
                st.dirty = true;
                return;
            }
            // tap an eSIM row to make it the default data slot
            for (i, slot) in [1u32, 2].iter().enumerate() {
                if in_rect(x, y, 0, y0 + 330 + i as i32 * 110, wi, 110) {
                    if st.esim.data_slot != *slot && st.radio_now(*slot).registered {
                        st.esim.data_slot = *slot;
                        st.notify(
                            "SIM manager",
                            &format!("eSIM {slot} set as default for mobile data"),
                            None,
                        );
                        st.persist_dirty = true;
                    }
                    st.dirty = true;
                    return;
                }
            }
        }
        2 => {
            let y0 = STATUS_H + 120;
            if in_rect(x, y, wi / 2 - 140, y0 + 260, 280, 64) {
                st.charging = !st.charging;
                st.persist_dirty = true;
                st.dirty = true;
            }
        }
        3 => {
            let y0 = STATUS_H + 120;
            let res_opts = [(PANEL_W, PANEL_H), (1080, 2400), (540, 1200), (720, 1280)];
            for (i, (rw, rh)) in res_opts.iter().enumerate() {
                let ry = y0 + 40 + i as i32 * 80;
                if in_rect(x, y, 0, ry, wi, 80) {
                    if (*rw, *rh) == st.display {
                        st.size_override = None;
                    } else {
                        st.size_override = Some((*rw, *rh));
                    }
                    st.dirty = true;
                    st.persist_dirty = true;
                    return;
                }
            }
            let dy = y0 + 40 + 4 * 80 + 24;
            let dens_opts = [420u32, 340, 480, 560];
            for (i, d) in dens_opts.iter().enumerate() {
                let ry = dy + 40 + i as i32 * 80;
                if in_rect(x, y, 0, ry, wi, 80) {
                    if *d == st.density {
                        st.density_override = None;
                    } else {
                        st.density_override = Some(*d);
                    }
                    st.dirty = true;
                    st.persist_dirty = true;
                    return;
                }
            }
            let ry = dy + 40 + 4 * 80 + 30;
            if in_rect(x, y, 40, ry, wi - 80, 64) {
                st.size_override = None;
                st.density_override = None;
                st.dirty = true;
            }
        }
        4 => {
            // System update: the install button reboots to the bootloader and
            // re-flashes the image (the compositor runs the real installer)
            let y0 = STATUS_H + 120;
            let by = y0 + 220;
            if in_rect(x, y, wi / 2 - 170, by, 340, 64) {
                st.inbox.lock().unwrap().push_back(OsEvent::OsInstall);
                st.notify(
                    "System update",
                    "Rebooting to bootloader to flash the OS image",
                    None,
                );
                st.dirty = true;
            }
        }
        5 => {
            // scroll about-phone rows with vertical drag handled as tap-top/bottom
            if y < STATUS_H + 160 {
                st.app.settings.scroll = st.app.settings.scroll.saturating_sub(1);
            } else if y > h as i32 - NAV_H - 160 {
                st.app.settings.scroll += 1;
            }
            st.dirty = true;
        }
        _ => {}
    }
    let _ = content_h;
}

// ── Calculator ───────────────────────────────────────────────────────────────

fn render_calculator(c: &mut Canvas, st: &OsState) {
    let (w, h) = st.effective_size();
    let wi = w as i32;
    c.fill_rect(0, STATUS_H, wi, h as i32 - STATUS_H - NAV_H, BG);
    let ui = &st.app.calc;
    // history + expression + result
    let hist_y = STATUS_H + 40;
    if let Some(prev) = ui.history.last() {
        c.text_fit(
            wi - 40 - Canvas::text_w(prev, 2).max(0),
            hist_y,
            prev,
            2,
            wi - 80,
            DIMMER,
        );
    }
    let expr = ui.expr.clone();
    let disp = if expr.is_empty() {
        "0".to_string()
    } else {
        expr.clone()
    };
    let dw = Canvas::text_w(&disp, 6);
    c.text_fit(wi - 40 - dw.max(0), hist_y + 60, &disp, 6, wi - 80, TEXT);
    if let Some(res) = &ui.result {
        c.text_center(wi / 2, hist_y + 220, &format!("= {res}"), 4, ACCENT);
    }
    // keys
    let keys = [
        ["C", "+/-", "%", "÷"],
        ["7", "8", "9", "×"],
        ["4", "5", "6", "−"],
        ["1", "2", "3", "+"],
        ["0", ".", "⌫", "="],
    ];
    let top = h as i32 - NAV_H - 5 * 150 - 20;
    let cell_w = (wi - 32) / 4;
    for (r, rowk) in keys.iter().enumerate() {
        for (col, k) in rowk.iter().enumerate() {
            let kx = 16 + col as i32 * cell_w + 6;
            let ky = top + r as i32 * 150 + 10;
            let accent = matches!(*k, "=" | "÷" | "×" | "−" | "+");
            let special = matches!(*k, "C" | "+/-" | "%");
            let bg = if *k == "=" {
                ACCENT
            } else if accent {
                mix(ACCENT, SURFACE, 0.75)
            } else if special {
                mix(DIMMER, SURFACE, 0.55)
            } else {
                SURFACE
            };
            c.round_rect(kx, ky, cell_w - 12, 130, 20, bg);
            c.text_center(kx + (cell_w - 12) / 2, ky + 50, k, 5, TEXT);
        }
    }
}

fn touch_calculator(st: &mut OsState, x: i32, y: i32) {
    let (w, h) = st.effective_size();
    let wi = w as i32;
    let keys = [
        ["C", "+/-", "%", "÷"],
        ["7", "8", "9", "×"],
        ["4", "5", "6", "−"],
        ["1", "2", "3", "+"],
        ["0", ".", "⌫", "="],
    ];
    let top = h as i32 - NAV_H - 5 * 150 - 20;
    let cell_w = (wi - 32) / 4;
    for (r, rowk) in keys.iter().enumerate() {
        for (col, k) in rowk.iter().enumerate() {
            let kx = 16 + col as i32 * cell_w + 6;
            let ky = top + r as i32 * 150 + 10;
            if in_rect(x, y, kx, ky, cell_w - 12, 130) {
                press_calc_key(st, k);
                return;
            }
        }
    }
}

fn press_calc_key(st: &mut OsState, k: &str) {
    let ui = &mut st.app.calc;
    match k {
        "C" => {
            ui.expr.clear();
            ui.result = None;
        }
        "⌫" => {
            ui.expr.pop();
        }
        "+/-" => {
            if !ui.expr.is_empty() && !ui.expr.starts_with('-') {
                ui.expr = format!("-{}`", ui.expr.replace('`', ""));
            } else if let Some(rest) = ui.expr.strip_prefix('-') {
                ui.expr = rest.strip_suffix('`').unwrap_or(rest).to_string();
            }
        }
        "=" => {
            let r = eval_expr(&ui.expr);
            if let Some(v) = r {
                let v = fmt_num(v);
                ui.history.push(format!("{} = {}", ui.expr, v));
                ui.history.truncate(6);
                ui.result = Some(v.clone());
                ui.expr.clear();
            } else {
                ui.result = Some("error".into());
            }
        }
        other => {
            ui.expr.push_str(other);
        }
    }
    st.dirty = true;
}

fn fmt_num(v: f64) -> String {
    if v.fract() == 0.0 && v.abs() < 1e15 {
        format!("{}", v as i64)
    } else {
        format!("{v:.6}")
            .trim_end_matches('0')
            .trim_end_matches('.')
            .to_string()
    }
}

/// Straight shunting-yard evaluation with + − × ÷ % and unary minus.
pub fn eval_expr(src: &str) -> Option<f64> {
    let mut s = src.replace('×', "*").replace('÷', "/").replace('−', "-");
    if s.contains('`') {
        // the calculator's +/- key marks a negated group as `-expr…``;
        // honor it as -(expr) instead of deleting the marker
        if s.starts_with('-') {
            s = format!("(-({}", s[1..].replace('`', "))"));
        } else {
            s = s.replace('`', "");
        }
    }
    let mut out: Vec<f64> = Vec::new();
    let mut ops: Vec<char> = Vec::new();
    let chars: Vec<char> = s.chars().collect();
    let mut i = 0usize;
    let mut expect_operand = true;
    while i < chars.len() {
        let ch = chars[i];
        if ch.is_ascii_digit() || ch == '.' {
            let start = i;
            while i < chars.len() && (chars[i].is_ascii_digit() || chars[i] == '.') {
                i += 1;
            }
            let tok: String = chars[start..i].iter().collect();
            out.push(tok.parse::<f64>().ok()?);
            expect_operand = false;
            continue;
        }
        if ch == ' ' {
            i += 1;
            continue;
        }
        if ch == '-' && expect_operand {
            // unary minus: a prefix op binding tighter than * and /
            ops.push('~');
            expect_operand = true;
            i += 1;
            continue;
        }
        if ch == '+' && expect_operand {
            // unary plus: no-op
            i += 1;
            continue;
        }
        if ch == '(' {
            // subexpression: the open paren is a floor for the operator
            // stack — nothing to its left folds until the matching ')'
            ops.push('(');
            expect_operand = true;
            i += 1;
            continue;
        }
        if ch == ')' {
            // fold everything back to the matching '(' (unbalanced input
            // surfaces '(' in apply_op and degrades to None → verbatim)
            let mut matched = false;
            while let Some(top) = ops.pop() {
                if top == '(' {
                    matched = true;
                    break;
                }
                apply_op(&mut out, top)?;
            }
            if !matched {
                return None;
            }
            expect_operand = false;
            i += 1;
            continue;
        }
        match ch {
            '+' | '-' | '*' | '/' => {
                let prec = |o: char| match o {
                    '~' => 3, // unary minus
                    '*' | '/' => 2,
                    _ => 1,
                };
                while let Some(top) = ops.last() {
                    if *top == '(' {
                        break; // never fold across a subexpression boundary
                    }
                    if prec(*top) >= prec(ch) {
                        let op = ops.pop().unwrap();
                        apply_op(&mut out, op)?;
                    } else {
                        break;
                    }
                }
                ops.push(ch);
                expect_operand = true;
            }
            '%' => {
                let v = out.pop()?;
                out.push(v / 100.0);
                expect_operand = false;
            }
            _ => return None,
        }
        i += 1;
    }
    while let Some(op) = ops.pop() {
        apply_op(&mut out, op)?;
    }
    out.pop()
}

fn apply_op(out: &mut Vec<f64>, op: char) -> Option<()> {
    if op == '~' {
        // unary minus
        let a = out.pop()?;
        out.push(-a);
        return Some(());
    }
    let b = out.pop()?;
    let a = out.pop()?;
    let v = match op {
        '+' => a + b,
        '-' => a - b,
        '*' => a * b,
        '/' => {
            if b == 0.0 {
                return None;
            }
            a / b
        }
        _ => return None,
    };
    out.push(v);
    Some(())
}

// ── Clock ────────────────────────────────────────────────────────────────────

fn render_clock(c: &mut Canvas, st: &OsState) {
    let (w, h) = st.effective_size();
    let wi = w as i32;
    let content_h = h as i32 - STATUS_H - NAV_H;
    c.fill_rect(0, STATUS_H, wi, content_h, BG);
    // tabs
    for (i, t) in ["Clock", "Stopwatch"].iter().enumerate() {
        let tw = wi / 2;
        let sel = st.app.clock.tab
            == if i == 0 {
                ClockTab::Clock
            } else {
                ClockTab::Stopwatch
            };
        c.fill_rect(
            i as i32 * tw,
            STATUS_H,
            tw,
            88,
            if sel { SURFACE } else { BG },
        );
        c.text_center(
            i as i32 * tw + tw / 2,
            STATUS_H + 28,
            t,
            4,
            if sel { TEXT } else { DIM },
        );
        if sel {
            c.fill_rect(i as i32 * tw, STATUS_H + 86, tw, 3, ACCENT);
        }
    }
    let body_y = STATUS_H + 110;
    match st.app.clock.tab {
        ClockTab::Clock => {
            let secs = crate::state::now_secs() as u64;
            let (ss, hh, mm) = ((secs % 60), (secs / 60) % 60, (secs / 3600) % 24);
            let cy = body_y + 280;
            let cx = wi / 2;
            let r = 230;
            c.circle(cx, cy, r, SURFACE);
            c.ring(cx, cy, r - 4, [80, 92, 110]);
            for i in 0..12 {
                let a = i as f32 * std::f32::consts::FRAC_PI_6;
                let tx = cx + ((r - 34) as f32 * a.sin()).round() as i32;
                let ty = cy - ((r - 34) as f32 * a.cos()).round() as i32;
                c.circle(tx, ty, 8, DIM);
            }
            // hands
            let ha = (hh % 12) as f32 * 30.0 + mm as f32 * 0.5;
            let ma = mm as f32 * 6.0 + ss as f32 * 0.1;
            let sa = ss as f32 * 6.0;
            let mut hand = |ang: f32, len: i32, col: Rgb, wd: i32| {
                let rad = ang.to_radians();
                let ex = cx + (len as f32 * rad.sin()).round() as i32;
                let ey = cy - (len as f32 * rad.cos()).round() as i32;
                c.line(cx, cy, ex, ey, col, wd);
            };
            hand(ha, r - 110, TEXT, 16);
            hand(ma, r - 60, TEXT, 10);
            hand(sa, r - 40, RED, 4);
            c.circle(cx, cy, 14, ACCENT);
            c.text_center(
                cx,
                cy + r + 60,
                &format!("{hh:02}:{mm:02}:{ss:02}"),
                7,
                TEXT,
            );
            let days = crate::state::now_secs() as u64 / 86400;
            let (y, m, d) = civil_from_days(days as i64);
            c.text_center(cx, cy + r + 130, &format!("{y:04}-{m:02}-{d:02}"), 3, DIM);
        }
        ClockTab::Stopwatch => {
            let ms = stopwatch_ms(st);
            let t = format!("{:02}:{:02}.{:03}", ms / 60000, (ms / 1000) % 60, ms % 1000);
            c.text_center(wi / 2, body_y + 120, &t, 8, TEXT);
            let running = st.app.clock.started.is_some();
            // buttons
            let by = body_y + 420;
            let b1 = if running { "Stop" } else { "Start" };
            c.round_rect(
                wi / 2 - 260,
                by,
                240,
                90,
                20,
                if running { RED } else { GREEN },
            );
            c.text_center(wi / 2 - 140, by + 26, b1, 5, TEXT);
            c.round_rect(wi / 2 + 20, by, 240, 90, 20, SURFACE_HI);
            c.text_center(
                wi / 2 + 140,
                by + 26,
                if running { "Lap" } else { "Reset" },
                5,
                TEXT,
            );
            // laps
            for (i, lap) in st.app.clock.laps.iter().rev().take(4).enumerate() {
                let ly = by + 130 + i as i32 * 70;
                let lt = format!(
                    "{:02}:{:02}.{:03}",
                    lap / 60000,
                    (lap / 1000) % 60,
                    lap % 1000
                );
                c.text(
                    80,
                    ly,
                    &format!("Lap {}", st.app.clock.laps.len() - i),
                    3,
                    DIM,
                );
                c.text(wi - 80 - Canvas::text_w(&lt, 3), ly, &lt, 3, TEXT);
            }
        }
    }
}

fn stopwatch_ms(st: &OsState) -> u64 {
    let ui = &st.app.clock;
    let live = ui
        .started
        .map(|t0| t0.elapsed().unwrap_or_default().as_millis() as u64)
        .unwrap_or(0);
    ui.accumulated_ms + live
}

fn touch_clock(st: &mut OsState, x: i32, y: i32) {
    let (w, _h) = st.effective_size();
    let wi = w as i32;
    // tabs
    if y >= STATUS_H && y < STATUS_H + 88 {
        st.app.clock.tab = if x < wi / 2 {
            ClockTab::Clock
        } else {
            ClockTab::Stopwatch
        };
        st.dirty = true;
        return;
    }
    if st.app.clock.tab != ClockTab::Stopwatch {
        return;
    }
    let body_y = STATUS_H + 110;
    let by = body_y + 420;
    if y >= by && y < by + 90 {
        if x < wi / 2 {
            // start / stop
            if let Some(t0) = st.app.clock.started.take() {
                st.app.clock.accumulated_ms += t0.elapsed().unwrap_or_default().as_millis() as u64;
            } else {
                st.app.clock.started = Some(std::time::SystemTime::now());
            }
        } else {
            if st.app.clock.started.is_some() {
                // lap
                let ms = stopwatch_ms(st);
                st.app.clock.laps.push(ms);
            } else {
                st.app.clock.accumulated_ms = 0;
                st.app.clock.laps.clear();
            }
        }
        st.dirty = true;
    }
}

// ── Messages ─────────────────────────────────────────────────────────────────

fn render_messages(c: &mut Canvas, st: &OsState) {
    let (w, h) = st.effective_size();
    let wi = w as i32;
    let content_h = h as i32 - STATUS_H - NAV_H;
    c.fill_rect(0, STATUS_H, wi, content_h, BG);
    let ui = &st.app.messages;
    if let Some(addr) = &ui.open_thread {
        render_thread_view(c, st, addr);
        return;
    }
    if !ui.compose_to.is_empty() || !ui.compose_text.is_empty() {
        // compose screen
        app_bar(c, "New message");
        c.round_rect(32, STATUS_H + 120, wi - 64, 90, 16, SURFACE);
        c.text(
            52,
            STATUS_H + 148,
            &format!("To: {}", ui.compose_to),
            3,
            if ui.field == 0 { ACCENT } else { TEXT },
        );
        c.round_rect(32, STATUS_H + 230, wi - 64, 110, 16, SURFACE);
        c.text_fit(
            52,
            STATUS_H + 270,
            &ui.compose_text,
            3,
            wi - 120,
            if ui.field == 1 { ACCENT } else { TEXT },
        );
        c.round_rect(wi - 200, STATUS_H + 360, 168, 72, 16, ACCENT);
        c.text_center(wi - 116, STATUS_H + 386, "Send", 4, [10, 14, 20]);
        render_keyboard(c, st);
        return;
    }
    app_bar(c, "Messages");
    let y0 = STATUS_H + 120;
    if st.sms.is_empty() {
        c.text_center(wi / 2, content_h / 2, "No messages", 4, DIM);
        c.text_center(
            wi / 2,
            content_h / 2 + 50,
            "Send one from a host:",
            2,
            DIMMER,
        );
        c.text_center(
            wi / 2,
            content_h / 2 + 80,
            "adb shell sms send <from> <text>",
            2,
            DIMMER,
        );
        return;
    }
    for (i, th) in st.sms.iter().rev().take(6).enumerate() {
        let y = y0 + i as i32 * 130;
        let last = th.messages.last();
        let (preview, mine) = last
            .map(|m| (m.text.clone(), m.mine))
            .unwrap_or((String::new(), false));
        c.fill_rect(0, y, wi, 130, SURFACE);
        c.circle(wi - 60, y + 65, 26, if mine { GREEN } else { ACCENT });
        c.text_center(
            wi - 60,
            y + 56,
            &address_initial(&th.address),
            3,
            [10, 14, 20],
        );
        c.text_fit(40, y + 20, &th.address, 4, wi - 160, TEXT);
        c.text_fit(40, y + 70, &preview, 2, wi - 160, DIM);
        c.fill_rect(0, y + 129, wi, 1, [16, 20, 26]);
    }
}

fn address_initial(addr: &str) -> String {
    addr.chars()
        .next()
        .map(|ch| ch.to_ascii_uppercase().to_string())
        .unwrap_or("?".into())
}

fn render_thread_view(c: &mut Canvas, st: &OsState, addr: &str) {
    let (w, h) = st.effective_size();
    let wi = w as i32;
    c.fill_rect(0, STATUS_H, wi, wi, BG);
    let thread = st.sms.iter().find(|t| &t.address == addr);
    let ui = &st.app.messages;
    let kb_top = h as i32 - NAV_H - if ui.kb.visible { KB_H } else { 0 };
    let input_h = 110;
    // input row at the bottom
    let iy = kb_top - input_h;
    c.fill_rect(0, iy, wi, input_h, SURFACE);
    c.round_rect(24, iy + 20, wi - 190, 70, 20, SURFACE_HI);
    c.text_fit(44, iy + 45, &ui.compose_text, 3, wi - 240, TEXT);
    c.round_rect(wi - 150, iy + 20, 126, 70, 20, ACCENT);
    c.text_center(wi - 87, iy + 45, "Send", 3, [10, 14, 20]);
    // bubbles
    let top = STATUS_H + 120;
    let avail = iy - top - 16;
    let msgs: Vec<&SmsMessage> = thread
        .map(|t| t.messages.iter().collect())
        .unwrap_or_default();
    let rendered: Vec<Vec<String>> = msgs
        .iter()
        .map(|m| wrap_text(&m.text, (wi - 220) as usize, 3))
        .collect();
    let heights: Vec<i32> = rendered
        .iter()
        .map(|ls| (ls.len() as i32) * 44 + 40)
        .collect();
    let lines_total: i32 = heights.iter().sum();
    let scroll = lines_total.saturating_sub(avail).max(0) as i32;
    let mut yy = top - scroll;
    for (i, m) in msgs.iter().enumerate() {
        let bh = heights[i];
        if yy + bh > top && yy < iy {
            let bubble_w = wi - 220;
            let bx = if m.mine { wi - 60 - bubble_w } else { 60 };
            let col = if m.mine { ACCENT } else { SURFACE_HI };
            let tcol = if m.mine { [10, 14, 20] } else { TEXT };
            c.round_rect(bx, yy, bubble_w, bh - 24, 24, col);
            for (j, line) in rendered[i].iter().enumerate() {
                c.text(bx + 30, yy + 20 + j as i32 * 44, line, 3, tcol);
            }
        }
        yy += bh;
    }
    // header
    app_bar(c, addr);
    if ui.kb.visible {
        render_keyboard(c, st);
    }
}

pub fn wrap_text(s: &str, max_px: usize, px: u32) -> Vec<String> {
    // The 8x8 bitmap font advances 8 * px device pixels per character.
    let cap = (max_px / (px as usize * 8)).max(4);
    let mut out = Vec::new();
    for para in s.split('\n') {
        let mut line = String::new();
        for word in para.split(' ') {
            if line.chars().count() + word.chars().count() + 1 > cap && !line.is_empty() {
                out.push(line);
                line = String::new();
            }
            if !line.is_empty() {
                line.push(' ');
            }
            let mut w = word;
            while w.chars().count() > cap {
                // split at a char boundary: a byte index from the char count
                // panics on multibyte text
                let split = w.char_indices().nth(cap).map_or(w.len(), |(b, _)| b);
                out.push(w[..split].to_string());
                w = &w[split..];
            }
            line.push_str(w);
        }
        out.push(line);
    }
    out
}

fn touch_messages(st: &mut OsState, x: i32, y: i32) {
    let (w, h) = st.effective_size();
    let wi = w as i32;
    let ui = &st.app.messages;
    if ui.open_thread.is_some() {
        let kb_top = h as i32 - NAV_H - if ui.kb.visible { KB_H } else { 0 };
        let iy = kb_top - 110;
        if y >= iy {
            if x >= wi - 150 {
                send_composed_sms(st);
            } else {
                st.app.messages.field = 1;
                st.app.messages.kb.visible = true;
            }
            st.dirty = true;
            return;
        }
        if ui.kb.visible {
            if keyboard_touch(st, x, y) {
                return;
            }
        }
        // tapping a thread header does nothing; back handled by nav
        return;
    }
    if !ui.compose_to.is_empty() || !ui.compose_text.is_empty() {
        if in_rect(x, y, 32, STATUS_H + 120, wi - 64, 90) {
            st.app.messages.field = 0;
            st.app.messages.kb.visible = true;
        } else if in_rect(x, y, 32, STATUS_H + 230, wi - 64, 110) {
            st.app.messages.field = 1;
            st.app.messages.kb.visible = true;
        } else if in_rect(x, y, wi - 200, STATUS_H + 360, 168, 72) {
            send_composed_sms(st);
            return;
        } else if ui.kb.visible {
            keyboard_touch(st, x, y);
        }
        st.dirty = true;
        return;
    }
    // thread list
    let y0 = STATUS_H + 120;
    if y >= y0 {
        let idx = ((y - y0) / 130) as usize;
        let threads: Vec<String> = st
            .sms
            .iter()
            .rev()
            .take(6)
            .map(|t| t.address.clone())
            .collect();
        if let Some(addr) = threads.get(idx) {
            let addr = addr.clone();
            st.app.messages.open_thread = Some(addr);
            st.app.messages.field = 1;
            st.app.messages.kb.visible = true;
            st.dirty = true;
        }
    }
}

/// Insert an outgoing SMS into the store (the Messages app's send button).
fn send_composed_sms(st: &mut OsState) {
    let ui = &st.app.messages;
    let addr = ui
        .open_thread
        .clone()
        .unwrap_or_else(|| ui.compose_to.trim().to_string());
    let text = ui.compose_text.trim().to_string();
    if addr.is_empty() || text.is_empty() {
        return;
    }
    push_sms(
        st,
        SmsMessage {
            from: addr.clone(),
            text,
            at: crate::state::now_ms(),
            mine: true,
        },
    );
    let ui = &mut st.app.messages;
    ui.compose_text.clear();
    ui.compose_to.clear();
    ui.kb.visible = false;
    ui.field = 0;
    st.dirty = true;
    st.persist_dirty = true;
}

/// Insert a message (both the app and `sms send` land here — one code path).
pub fn push_sms(st: &mut OsState, msg: SmsMessage) {
    let addr = msg.from.clone();
    if let Some(t) = st.sms.iter_mut().find(|t| t.address == addr) {
        t.messages.push(msg);
    } else {
        st.sms.push(SmsThread {
            address: addr,
            messages: vec![msg],
        });
    }
    st.persist_dirty = true;
    st.dirty = true;
}

// ── soft keyboard ────────────────────────────────────────────────────────────

const KB_ROWS_KEYS: [&str; 4] = ["qwertyuiop", "asdfghjkl", "zxcvbnm", ""];

fn render_keyboard(c: &mut Canvas, st: &OsState) {
    let (w, h) = st.effective_size();
    let wi = w as i32;
    let y0 = h as i32 - NAV_H - KB_H;
    // glass body with a soft top fade and hairline light
    c.fill_rect_alpha(0, y0, wi, KB_H + NAV_H, [14, 18, 24], 240);
    c.gradient_v_alpha(0, y0 - 10, y0, wi, [14, 18, 24], 0, 130);
    c.fill_rect_alpha(0, y0, wi, 1, [255, 255, 255], 60);
    let shift = match st.current {
        Some(AppId::Messages) => st.app.messages.kb.shift,
        Some(AppId::Browser) => st.app.browser.kb.shift,
        Some(AppId::Termux) => st.app.termux.kb.shift,
        _ => false,
    };
    for row in 0..3usize {
        let keys = KB_ROWS_KEYS[row];
        let n = keys.chars().count();
        let kw = (wi - 24) / n as i32;
        for (i, ch) in keys.chars().enumerate() {
            let kx = 12 + i as i32 * kw;
            let ky = y0 + 8 + row as i32 * 56;
            let s = if shift { ch.to_ascii_uppercase() } else { ch };
            // key cap: shadow + gradient body + top sheen
            c.shadow(kx + 3, ky, kw - 6, 48, 10, 34);
            c.round_rect_gradient_v(kx + 3, ky, kw - 6, 48, 10, SURFACE_HI, [22, 27, 35]);
            c.round_rect_alpha(kx + 5, ky + 2, kw - 10, 14, 7, [255, 255, 255], 16);
            c.text_center(kx + kw / 2, ky + 16, &s.to_string(), 3, TEXT);
        }
    }
    // bottom row: shift, space, backspace, done
    let ky = y0 + 8 + 3 * 56;
    let seg = (wi - 24) / 6;
    // shift: accent when latched, quiet modifier otherwise
    c.shadow(12, ky, seg - 6, 48, 10, 34);
    if shift {
        c.round_rect_gradient_v(12, ky, seg - 6, 48, 10, ACCENT_HI, ACCENT_LO);
        c.text_center(12 + seg / 2, ky + 16, "⇧", 3, [10, 14, 20]);
    } else {
        c.round_rect_gradient_v(12, ky, seg - 6, 48, 10, [30, 37, 46], [20, 25, 32]);
        c.text_center(12 + seg / 2, ky + 16, "⇧", 3, DIM);
    }
    // space: wide, quieter cap
    c.shadow(12 + seg, ky, seg * 2 - 6, 48, 10, 34);
    c.round_rect_gradient_v(
        12 + seg,
        ky,
        seg * 2 - 6,
        48,
        10,
        [38, 46, 58],
        [26, 32, 41],
    );
    c.round_rect_alpha(14 + seg, ky + 2, seg * 2 - 10, 14, 7, [255, 255, 255], 14);
    c.text_center(12 + seg * 2, ky + 16, "space", 3, TEXT);
    // backspace
    c.shadow(12 + seg * 3, ky, seg - 6, 48, 10, 34);
    c.round_rect_gradient_v(
        12 + seg * 3,
        ky,
        seg - 6,
        48,
        10,
        [30, 37, 46],
        [20, 25, 32],
    );
    c.text_center(12 + seg * 3 + seg / 2, ky + 16, "⌫", 3, DIM);
    // done: the accent key — the only vivid thing on the tray
    c.shadow(12 + seg * 4, ky, seg * 2 - 6, 48, 10, 44);
    c.round_rect_gradient_v(12 + seg * 4, ky, seg * 2 - 6, 48, 10, ACCENT_HI, ACCENT_LO);
    c.round_rect_alpha(
        14 + seg * 4,
        ky + 2,
        seg * 2 - 10,
        14,
        7,
        [255, 255, 255],
        24,
    );
    c.text_center(12 + seg * 5, ky + 16, "done", 3, [10, 14, 20]);
}

/// Handle a keyboard tap; returns true when consumed.
fn keyboard_touch(st: &mut OsState, x: i32, y: i32) -> bool {
    let (w, h) = st.effective_size();
    let wi = w as i32;
    let y0 = h as i32 - NAV_H - KB_H;
    if y < y0 || y > h as i32 - NAV_H {
        return false;
    }
    let shift = match st.current {
        Some(AppId::Messages) => st.app.messages.kb.shift,
        Some(AppId::Browser) => st.app.browser.kb.shift,
        Some(AppId::Termux) => st.app.termux.kb.shift,
        _ => false,
    };
    for row in 0..3usize {
        let keys = KB_ROWS_KEYS[row];
        let n = keys.chars().count();
        let kw = (wi - 24) / n as i32;
        let ky = y0 + 8 + row as i32 * 56;
        if y >= ky && y < ky + 56 {
            for (i, ch) in keys.chars().enumerate() {
                let kx = 12 + i as i32 * kw;
                if x >= kx && x < kx + kw {
                    let s = if shift { ch.to_ascii_uppercase() } else { ch };
                    if let Some(app) = st.current.clone() {
                        text_input(st, &app, &s.to_string());
                    }
                    st.dirty = true;
                    return true;
                }
            }
        }
    }
    let ky = y0 + 8 + 3 * 56;
    let seg = (wi - 24) / 6;
    if y >= ky && y < ky + 56 {
        let app = st.current.clone();
        if x < 12 + seg {
            match &app {
                Some(AppId::Messages) => st.app.messages.kb.shift = !st.app.messages.kb.shift,
                Some(AppId::Browser) => st.app.browser.kb.shift = !st.app.browser.kb.shift,
                Some(AppId::Termux) => st.app.termux.kb.shift = !st.app.termux.kb.shift,
                _ => {}
            }
        } else if x < 12 + seg * 3 {
            if let Some(a) = &app {
                text_input(st, a, " ");
            }
        } else if x < 12 + seg * 4 {
            if let Some(a) = &app {
                key_del(st, a);
            }
        } else {
            // done: hide keyboard
            match app {
                Some(AppId::Messages) => st.app.messages.kb.visible = false,
                Some(AppId::Browser) => st.app.browser.kb.visible = false,
                Some(AppId::Termux) => st.app.termux.kb.visible = false,
                _ => {}
            }
        }
        st.dirty = true;
        return true;
    }
    false
}

// ── Dialer ───────────────────────────────────────────────────────────────────

fn render_dialer(c: &mut Canvas, st: &mut OsState) {
    let (w, h) = st.effective_size();
    let wi = w as i32;
    let content_h = h as i32 - STATUS_H - NAV_H;
    c.fill_rect(0, STATUS_H, wi, content_h, BG);
    if st
        .app
        .dialer
        .call
        .as_ref()
        .map(|c| c.ringing)
        .unwrap_or(false)
        && st
            .app
            .dialer
            .call
            .as_ref()
            .map(|c| c.started.elapsed().unwrap_or_default().as_secs())
            .unwrap_or(0)
            >= 2
    {
        st_current_set_ringing_off(st);
    }
    let ui = &st.app.dialer;
    if let Some(call) = &ui.call {
        let secs = call.started.elapsed().unwrap_or_default().as_secs();
        let phase = if call.ringing {
            "Calling…"
        } else {
            "In call"
        };
        c.text_center(wi / 2, content_h / 2 - 160, &call.number, 6, TEXT);
        c.text_center(wi / 2, content_h / 2 - 80, phase, 3, DIM);
        c.text_center(
            wi / 2,
            content_h / 2,
            &format!("{}:{:02}", secs / 60, secs % 60),
            6,
            DIM,
        );
        let by = content_h / 2 + 160;
        c.circle(wi / 2, by + 45, 66, RED);
        c.fill_rect(wi / 2 - 36, by + 40, 72, 14, [10, 14, 20]);
        c.text_center(wi / 2, by + 130, "End call", 3, DIM);
        return;
    }
    for (i, t) in ["Recents", "Keypad"].iter().enumerate() {
        let tw = wi / 2;
        let sel = ui.tab == i;
        c.fill_rect(
            i as i32 * tw,
            STATUS_H,
            tw,
            88,
            if sel { SURFACE } else { BG },
        );
        c.text_center(
            i as i32 * tw + tw / 2,
            STATUS_H + 28,
            t,
            4,
            if sel { TEXT } else { DIM },
        );
        if sel {
            c.fill_rect(i as i32 * tw, STATUS_H + 86, tw, 3, ACCENT);
        }
    }
    if ui.tab == 0 {
        if st.calls.is_empty() {
            c.text_center(wi / 2, content_h / 2, "No calls yet", 4, DIM);
            return;
        }
        for (i, call) in st.calls.iter().rev().take(7).enumerate() {
            let y = STATUS_H + 110 + i as i32 * 120;
            c.fill_rect(0, y, wi, 120, SURFACE);
            let glyph = if call.direction == "outgoing" {
                "↗"
            } else {
                "↙"
            };
            let days = (call.at as u64 / 1000 / 86400) as i64;
            let (_, mm, dd) = civil_from_days(days);
            c.circle(
                70,
                y + 60,
                28,
                if call.direction == "outgoing" {
                    GREEN
                } else {
                    ACCENT
                },
            );
            c.text_center(70, y + 50, glyph, 3, [10, 14, 20]);
            c.text(120, y + 20, &call.number, 4, TEXT);
            c.text(
                120,
                y + 70,
                &format!(
                    "{mm:02}-{dd:02}  {}:{:02}",
                    (call.at / 1000 / 3600) % 24,
                    (call.at / 1000 / 60) % 60
                ),
                2,
                DIM,
            );
            c.fill_rect(0, y + 119, wi, 1, [16, 20, 26]);
        }
    } else {
        // number display
        c.text_center(
            wi / 2,
            STATUS_H + 150,
            if ui.number.is_empty() {
                " "
            } else {
                &ui.number
            },
            7,
            TEXT,
        );
        // keypad
        let keys = [
            ["1", "2", "3"],
            ["4", "5", "6"],
            ["7", "8", "9"],
            ["*", "0", "#"],
        ];
        let ky0 = STATUS_H + 230;
        let cell = (wi - 60) / 3;
        for (r, rowk) in keys.iter().enumerate() {
            for (col, k) in rowk.iter().enumerate() {
                let kx = 30 + col as i32 * cell + 10;
                let ky = ky0 + r as i32 * 160;
                c.circle(kx + (cell - 20) / 2, ky + 60, 64, SURFACE);
                c.text_center(kx + (cell - 20) / 2, ky + 34, k, 6, TEXT);
                let sub = match *k {
                    "2" => "ABC",
                    "3" => "DEF",
                    "4" => "GHI",
                    "5" => "JKL",
                    "6" => "MNO",
                    "7" => "PQRS",
                    "8" => "TUV",
                    "9" => "WXYZ",
                    _ => "",
                };
                if !sub.is_empty() {
                    c.text_center(kx + (cell - 20) / 2, ky + 88, sub, 1, DIM);
                }
            }
        }
        let by = ky0 + 4 * 160 + 10;
        c.circle(wi / 2 - 130, by + 50, 56, GREEN);
        c.fill_rect(wi / 2 - 160, by + 45, 60, 12, [10, 14, 20]);
        c.circle(wi / 2 + 130, by + 50, 44, SURFACE_HI);
        c.text_center(wi / 2 + 130, by + 40, "⌫", 3, TEXT);
    }
}

fn st_current_set_ringing_off(st: &mut OsState) {
    if let Some(call) = &mut st.app.dialer.call {
        call.ringing = false;
    }
    st.dirty = true;
}

fn touch_dialer(st: &mut OsState, x: i32, y: i32) {
    let (w, h) = st.effective_size();
    let wi = w as i32;
    let content_h = h as i32 - STATUS_H - NAV_H;
    let ui = &st.app.dialer;
    if let Some(_call) = &ui.call {
        let by = content_h / 2 + 160;
        let dx = x - wi / 2;
        let dy = y - (by + 45);
        if dx * dx + dy * dy <= 66 * 66 {
            end_call(st);
        }
        return;
    }
    if y >= STATUS_H && y < STATUS_H + 88 {
        st.app.dialer.tab = if x < wi / 2 { 0 } else { 1 };
        st.dirty = true;
        return;
    }
    if ui.tab == 0 {
        let idx = ((y - STATUS_H - 110).max(0) / 120) as usize;
        let calls: Vec<String> = st
            .calls
            .iter()
            .rev()
            .take(7)
            .map(|c| c.number.clone())
            .collect();
        if let Some(n) = calls.get(idx) {
            st.app.dialer.number = n.clone();
            st.app.dialer.tab = 1;
            st.dirty = true;
        }
        return;
    }
    let keys = [
        ["1", "2", "3"],
        ["4", "5", "6"],
        ["7", "8", "9"],
        ["*", "0", "#"],
    ];
    let ky0 = STATUS_H + 230;
    let cell = (wi - 60) / 3;
    for (r, rowk) in keys.iter().enumerate() {
        for (col, k) in rowk.iter().enumerate() {
            let kx = 30 + col as i32 * cell + 10;
            let ky = ky0 + r as i32 * 160;
            let cx = kx + (cell - 20) / 2;
            let cy = ky + 60;
            let dx = x - cx;
            let dy = y - cy;
            if dx * dx + dy * dy <= 64 * 64 {
                st.app.dialer.number.push_str(k);
                st.dirty = true;
                return;
            }
        }
    }
    let by = ky0 + 4 * 160 + 10;
    let dx = x - (wi / 2 - 130);
    let dy = y - (by + 50);
    if dx * dx + dy * dy <= 56 * 56 && !ui.number.is_empty() {
        let number = ui.number.clone();
        start_call(st, &number);
        return;
    }
    let dx = x - (wi / 2 + 130);
    let dy = y - (by + 50);
    if dx * dx + dy * dy <= 44 * 44 {
        st.app.dialer.number.pop();
        st.dirty = true;
    }
}

fn start_call(st: &mut OsState, number: &str) {
    st.app.dialer.call = Some(CallState {
        number: number.to_string(),
        started: std::time::SystemTime::now(),
        ringing: true,
    });
    st.dirty = true;
}

fn end_call(st: &mut OsState) {
    if let Some(call) = st.app.dialer.call.take() {
        let secs = call.started.elapsed().unwrap_or_default().as_secs() as u32;
        st.calls.push(CallEntry {
            number: call.number,
            at: crate::state::now_ms(),
            seconds: secs,
            direction: "outgoing".into(),
        });
        st.persist_dirty = true;
    }
    st.dirty = true;
}

// ── Browser ──────────────────────────────────────────────────────────────────

fn render_browser(c: &mut Canvas, st: &OsState) {
    let (w, h) = st.effective_size();
    let wi = w as i32;
    let content_h = h as i32 - STATUS_H - NAV_H;
    c.fill_rect(0, STATUS_H, wi, content_h, BG);
    let ui = &st.app.browser;
    // URL bar
    let uy = STATUS_H + 16;
    c.round_rect(24, uy, wi - 48, 76, 24, SURFACE);
    let focus = ui.url_bar_focus;
    let label = if ui.url_bar.is_empty() {
        "Search or enter address"
    } else {
        &ui.url_bar
    };
    c.text_fit(
        48,
        uy + 24,
        label,
        3,
        wi - 200,
        if focus {
            ACCENT
        } else if ui.url_bar.is_empty() {
            DIM
        } else {
            TEXT
        },
    );
    // go button
    c.round_rect(wi - 200, uy, 150, 76, 24, SURFACE_HI);
    c.text_center(wi - 125, uy + 24, "Go", 3, TEXT);
    let body_y = uy + 96;
    if ui.loading {
        c.text_center(wi / 2, body_y + 200, "Loading…", 4, DIM);
    } else if let Some(err) = &ui.error {
        c.text_center(wi / 2, body_y + 120, "Can't reach this page", 4, RED);
        c.text_fit(80, body_y + 200, err, 2, wi - 160, DIM);
        c.text_center(
            wi / 2,
            body_y + 260,
            "Check the address and try again",
            2,
            DIMMER,
        );
    } else if ui.lines.is_empty() {
        // start page with real, tappable quick links
        c.text_center(wi / 2, body_y + 140, "Chrome", 6, TEXT);
        c.text_center(
            wi / 2,
            body_y + 210,
            "Real pages, real network — type an address above.",
            2,
            DIM,
        );
        let links = ["example.com", "wikipedia.org", "example.org"];
        for (i, l) in links.iter().enumerate() {
            let ly = body_y + 300 + i as i32 * 90;
            // glass tile: soft shadow, vertical gradient, top sheen, edge light
            c.shadow(80, ly, wi - 160, 70, 18, 60);
            c.round_rect_gradient_v(80, ly, wi - 160, 70, 18, SURFACE_HI, SURFACE);
            c.round_rect_alpha(82, ly + 2, wi - 164, 24, 16, [255, 255, 255], 14);
            c.round_rect_outline(
                80,
                ly,
                wi - 160,
                70,
                18,
                mix(SURFACE_HI, [255, 255, 255], 0.18),
            );
            c.text_center(wi / 2, ly + 22, l, 3, ACCENT_HI);
        }
    } else {
        // rendered page
        let avail = content_h - (body_y - STATUS_H) - 20;
        let mut lines_total = 0i32;
        for l in &ui.lines {
            let ls = wrap_text(&l.text, (wi - 80) as usize, l.px);
            lines_total += ls.len() as i32 * (l.px as i32 + 14) + 18;
        }
        let scroll = ui.scroll.min((lines_total - avail).max(0) as usize);
        let mut yy = body_y - scroll as i32;
        for l in &ui.lines {
            let ls = wrap_text(&l.text, (wi - 80) as usize, l.px);
            let lh = l.px as i32 + 14;
            for seg in &ls {
                if yy >= body_y - lh && yy < body_y + avail {
                    let col = match l.style {
                        HtmlStyle::Heading => TEXT,
                        HtmlStyle::Link => ACCENT,
                        HtmlStyle::Mono => GREEN,
                        HtmlStyle::Para => mix(TEXT, DIM, 0.25),
                    };
                    c.text(40, yy, seg, l.px, col);
                }
                yy += lh;
            }
            yy += 18;
        }
        // scrollbar
        if lines_total > avail {
            let bar_h = (avail as f32 * avail as f32 / lines_total as f32).max(30.0) as i32;
            let track = avail - 8;
            let pos = track * scroll as i32 / (lines_total - avail).max(1);
            c.fill_rect(wi - 14, body_y + 4 + pos, 8, bar_h, DIMMER);
        }
    }
    if ui.kb.visible {
        render_keyboard(c, st);
    }
}

fn touch_browser(st: &mut OsState, x: i32, y: i32) {
    let (w, h) = st.effective_size();
    let wi = w as i32;
    let content_h = h as i32 - STATUS_H - NAV_H;
    let uy = STATUS_H + 16;
    // url bar
    if in_rect(x, y, 24, uy, wi - 200, 76) {
        st.app.browser.url_bar_focus = true;
        st.app.browser.kb.visible = true;
        st.dirty = true;
        return;
    }
    if in_rect(x, y, wi - 200, uy, 176, 76) {
        navigate_browser(st);
        return;
    }
    if st.app.browser.kb.visible {
        if keyboard_touch(st, x, y) {
            return;
        }
    }
    let body_y = uy + 96;
    if st.app.browser.lines.is_empty() && st.app.browser.error.is_none() && !st.app.browser.loading
    {
        let links = ["example.com", "wikipedia.org", "example.org"];
        for (i, l) in links.iter().enumerate() {
            let ly = body_y + 300 + i as i32 * 90;
            if in_rect(x, y, 80, ly, wi - 160, 70) {
                st.app.browser.url_bar = l.to_string();
                navigate_browser(st);
                return;
            }
        }
        return;
    }
    // link taps inside the page
    let ui = &st.app.browser;
    if let Some(href) = link_at(&ui.lines, x, y, wi, body_y, ui.scroll) {
        st.app.browser.url_bar = href;
        st.app.browser.url_bar_focus = false;
        st.app.browser.kb.visible = false;
        navigate_browser(st);
        return;
    }
    // scroll: tap upper half scrolls up, lower half down (drag routing also lands here)
    let avail = content_h - (body_y - STATUS_H) - 20;
    let mut lines_total = 0i32;
    for l in &st.app.browser.lines {
        let ls = wrap_text(&l.text, (wi - 80) as usize, l.px);
        lines_total += ls.len() as i32 * (l.px as i32 + 14) + 18;
    }
    if lines_total > avail {
        let step = (avail / 2) as usize;
        if y < body_y + avail / 2 {
            st.app.browser.scroll = st.app.browser.scroll.saturating_sub(step);
        } else {
            st.app.browser.scroll += step;
        }
        st.dirty = true;
    }
}

fn link_at(
    lines: &[HtmlLine],
    x: i32,
    y: i32,
    w: i32,
    body_y: i32,
    scroll: usize,
) -> Option<String> {
    let wi = w;
    let mut yy = body_y - scroll as i32;
    for l in lines {
        let ls = wrap_text(&l.text, (wi - 80) as usize, l.px);
        let lh = l.px as i32 + 14;
        for seg in &ls {
            if y >= yy
                && y < yy + lh
                && x >= 40
                && x < wi - 40
                && l.style == HtmlStyle::Link
                && l.href.is_some()
            {
                let char_idx = ((x - 40) / (l.px as i32 * 8)) as usize;
                if char_idx <= seg.chars().count() {
                    return l.href.clone();
                }
            }
            yy += lh;
        }
        yy += 18;
    }
    None
}

pub fn navigate_browser(st: &mut OsState) {
    let raw = st.app.browser.url_bar.trim().to_string();
    if raw.is_empty() {
        return;
    }
    let url = normalize_url(&raw);
    st.app.browser.loading = true;
    st.app.browser.error = None;
    st.app.browser.url_bar_focus = false;
    st.app.browser.kb.visible = false;
    st.app.browser.url_bar = url.clone();
    st.dirty = true;
    let inbox = st.inbox.clone();
    crate::net_fetch::spawn_page_load(inbox, st.data_dir.clone(), url);
}

pub fn browser_nav(st: &mut OsState, delta: i32) {
    let ui = &mut st.app.browser;
    if let Some(idx) = ui.history_idx {
        let new = idx as i32 + delta;
        if new >= 0 && (new as usize) < ui.history.len() {
            ui.history_idx = Some(new as usize);
            let url = ui.history[new as usize].clone();
            ui.url_bar = url.clone();
            ui.loading = true;
            ui.lines.clear();
            let inbox = st.inbox.clone();
            crate::net_fetch::spawn_page_load(inbox, st.data_dir.clone(), url);
            return;
        }
    }
}

fn normalize_url(raw: &str) -> String {
    let r = raw.trim();
    let lower = r.to_ascii_lowercase();
    if lower.starts_with("http://") || lower.starts_with("https://") {
        r.to_string()
    } else if r.contains(char::is_whitespace) || !r.contains('.') {
        // not an address — treat it as a search query
        let q: String = r
            .bytes()
            .map(|b| match b {
                b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                    (b as char).to_string()
                }
                b' ' => "+".to_string(),
                _ => format!("%{b:02X}"),
            })
            .collect();
        format!("https://html.duckduckgo.com/html/?q={q}")
    } else {
        format!("https://{r}")
    }
}

// ── HTML-lite rendering ──────────────────────────────────────────────────────

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum HtmlStyle {
    Para,
    Heading,
    Link,
    Mono,
}

#[derive(Clone, Debug)]
pub struct HtmlLine {
    pub text: String,
    pub style: HtmlStyle,
    pub px: u32,
    pub href: Option<String>,
}

/// Parse HTML text into styled lines (reader mode). Tags handled: h1-h6, p,
/// li, a, br, title, script/style skipped, comments skipped.
pub fn html_to_lines(html: &str, base_url: &str) -> Vec<HtmlLine> {
    let mut out = Vec::new();
    let mut chars = html.chars().peekable();
    let mut style = HtmlStyle::Para;
    let mut px = 3u32;
    let mut href: Option<String> = None;
    let mut buf = String::new();
    let mut in_skip = 0usize; // script/style nesting guard
    while let Some(ch) = chars.next() {
        if ch == '<' {
            // comment?
            if chars.peek() == Some(&'!') {
                while let Some(c2) = chars.next() {
                    if c2 == '>' {
                        break;
                    }
                }
                continue;
            }
            let mut tag = String::new();
            let mut closing = false;
            if chars.peek() == Some(&'/') {
                closing = true;
                chars.next();
            }
            while let Some(c2) = chars.next() {
                if c2 == '>' {
                    break;
                }
                tag.push(c2);
            }
            let name: String = tag.split_whitespace().next().unwrap_or("").to_lowercase();
            if name == "script" || name == "style" {
                if closing {
                    in_skip = in_skip.saturating_sub(1);
                } else {
                    in_skip += 1;
                }
                continue;
            }
            if in_skip > 0 {
                continue;
            }
            if !closing {
                match name.as_str() {
                    "h1" | "h2" | "h3" | "h4" | "h5" | "h6" => {
                        flush_line(&mut buf, &mut out, style, px, href.clone());
                        style = HtmlStyle::Heading;
                        px = if name == "h1" { 5 } else { 4 };
                    }
                    "p" | "div" | "section" | "article" => {
                        flush_line(&mut buf, &mut out, style, px, href.clone());
                        style = HtmlStyle::Para;
                        px = 3;
                    }
                    "li" => {
                        flush_line(&mut buf, &mut out, style, px, href.clone());
                        buf.push_str("• ");
                        style = HtmlStyle::Para;
                        px = 3;
                    }
                    "a" => {
                        flush_line(&mut buf, &mut out, style, px, href.clone());
                        href = extract_href(&tag);
                        style = HtmlStyle::Link;
                        px = 3;
                    }
                    "br" => {
                        flush_line(&mut buf, &mut out, style, px, href.clone());
                    }
                    "title" => {
                        style = HtmlStyle::Heading;
                        px = 5;
                    }
                    _ => {}
                }
            } else {
                match name.as_str() {
                    "h1" | "h2" | "h3" | "h4" | "h5" | "h6" | "title" | "p" | "div" | "li"
                    | "section" | "article" => {
                        flush_line(&mut buf, &mut out, style, px, href.clone());
                        style = HtmlStyle::Para;
                        px = 3;
                    }
                    "a" => {
                        flush_line(&mut buf, &mut out, style, px, href.clone());
                        href = None;
                        style = HtmlStyle::Para;
                    }
                    _ => {}
                }
            }
            continue;
        }
        if in_skip == 0 {
            buf.push(ch);
        }
    }
    flush_line(&mut buf, &mut out, style, px, href);

    // resolve relative hrefs against the base URL; unopenable links become plain text
    for l in &mut out {
        if let Some(h) = l.href.take() {
            l.href = resolve_relative(base_url, &h);
            if l.href.is_none() && l.style == HtmlStyle::Link {
                l.style = HtmlStyle::Para;
            }
        }
    }
    out.retain(|l| !l.text.trim().is_empty());
    out.truncate(400);
    out
}

fn flush_line(
    buf: &mut String,
    out: &mut Vec<HtmlLine>,
    style: HtmlStyle,
    px: u32,
    href: Option<String>,
) {
    let text = decode_entities(buf).trim().to_string();
    buf.clear();
    if !text.is_empty() {
        out.push(HtmlLine {
            text,
            style,
            px,
            href,
        });
    }
}

fn decode_entities(s: &str) -> String {
    s.replace("&amp;", "&")
        .replace("&lt;", "<")
        .replace("&gt;", ">")
        .replace("&quot;", "\"")
        .replace("&#39;", "'")
        .replace("&apos;", "'")
        .replace("&nbsp;", " ")
        .replace("&#160;", " ")
        .replace("&#8217;", "’")
        .replace("&#8220;", "“")
        .replace("&#8221;", "”")
}

/// Pull the `href` value out of a raw tag, keeping its original case (URL
/// paths are case-sensitive) and decoding HTML entities.
fn extract_href(tag: &str) -> Option<String> {
    let lower = tag.to_ascii_lowercase();
    let mut from = 0;
    while let Some(rel) = lower[from..].find("href") {
        let at = from + rel;
        from = at + 4;
        // must be a standalone attribute name, not e.g. `data-href`
        let prev_ok = at == 0 || lower.as_bytes()[at - 1].is_ascii_whitespace();
        if !prev_ok {
            continue;
        }
        let rest = tag[at + 4..].trim_start();
        let rest = rest.strip_prefix('=')?.trim_start();
        let value = match rest.chars().next()? {
            q @ ('"' | '\'') => rest[1..].split(q).next().unwrap_or(""),
            _ => rest
                .split(|c: char| c.is_whitespace() || c == '>')
                .next()
                .unwrap_or(""),
        };
        return Some(decode_entities(value.trim()));
    }
    None
}

/// Resolve `href` against `base` the way a browser would for the cases a
/// reader-mode page can hit. Returns `None` for links that cannot be opened
/// (mailto:, tel:, javascript:, bare #fragments).
fn resolve_relative(base: &str, href: &str) -> Option<String> {
    let href = href.trim();
    let lower = href.to_ascii_lowercase();
    if href.is_empty()
        || href.starts_with('#')
        || lower.starts_with("javascript:")
        || lower.starts_with("mailto:")
        || lower.starts_with("tel:")
        || lower.starts_with("data:")
    {
        return None;
    }
    if lower.starts_with("http://") || lower.starts_with("https://") {
        return Some(href.to_string());
    }
    let (scheme, rest) = base.split_once("://")?;
    let (host, path) = match rest.find('/') {
        Some(i) => (&rest[..i], &rest[i..]),
        None => (rest, "/"),
    };
    // drop query/fragment from the base path
    let path = path.split(['?', '#']).next().unwrap_or("/");
    if let Some(r) = href.strip_prefix("//") {
        return Some(format!("{scheme}://{r}"));
    }
    let href = href.split('#').next().unwrap_or(href); // fragments never reach the server
    let mut segs: Vec<&str> = if href.starts_with('/') {
        Vec::new()
    } else {
        // directory of the base path
        let dir = path.rsplit_once('/').map_or("", |(d, _)| d);
        dir.split('/').filter(|s| !s.is_empty()).collect()
    };
    let (target, query) = match href.split_once('?') {
        Some((t, q)) => (t, Some(q)),
        None => (href, None),
    };
    if target.is_empty() {
        // query-only link: same path, new query
        let same: Vec<&str> = path.split('/').filter(|s| !s.is_empty()).collect();
        let q = query.map(|q| format!("?{q}")).unwrap_or_default();
        return Some(format!("{scheme}://{host}/{}{q}", same.join("/")));
    }
    let trailing = target.ends_with('/');
    for part in target.split('/') {
        match part {
            "" | "." => {}
            ".." => {
                segs.pop();
            }
            p => segs.push(p),
        }
    }
    let mut out = format!("{scheme}://{host}/{}", segs.join("/"));
    if trailing && !out.ends_with('/') {
        out.push('/');
    }
    if let Some(q) = query {
        out.push('?');
        out.push_str(q);
    }
    Some(out)
}

// ── Photos ───────────────────────────────────────────────────────────────────

fn render_photos(c: &mut Canvas, st: &mut OsState) {
    let (w, h) = st.effective_size();
    let wi = w as i32;
    let content_h = h as i32 - STATUS_H - NAV_H;
    if let Some(idx) = st.app.photos.viewer {
        // viewer: draw the photo fit to the screen
        c.fill_rect(0, STATUS_H, wi, content_h, [4, 5, 8]);
        let photos: Vec<PhotoEntry> = st.photos.clone();
        if let Some(p) = photos.get(idx) {
            let path = crate::persist::photos_dir(&st.data_dir).join(&p.file);
            if let Some(rgba) = load_png_rgba(&path) {
                let fit_w = (wi - 20) as f32;
                let fit_h = (content_h - 140) as f32;
                let scale = (fit_w / p.w as f32).min(fit_h / p.h as f32).min(1.0);
                let dw = (p.w as f32 * scale) as u32;
                let dh = (p.h as f32 * scale) as u32;
                let small = crate::scale_rgba(&rgba, p.w, p.h, dw, dh);
                c.blit((wi - dw as i32) / 2, STATUS_H + 60, dw, dh, &small);
                c.text_center(wi / 2, STATUS_H + content_h - 60, &p.file, 2, DIM);
            } else {
                c.text_center(wi / 2, content_h / 2, "unreadable image", 3, RED);
            }
        }
        // delete button
        c.round_rect(
            wi / 2 - 80,
            STATUS_H + content_h - 120,
            160,
            64,
            16,
            SURFACE_HI,
        );
        c.text_center(wi / 2, STATUS_H + content_h - 100, "Delete", 3, RED);
        return;
    }
    app_bar(c, "Photos");
    let y0 = STATUS_H + 120;
    if st.photos.is_empty() {
        c.text_center(wi / 2, content_h / 2, "No photos yet", 4, DIM);
        c.text_center(
            wi / 2,
            content_h / 2 + 50,
            "Screenshots taken by the agent land here",
            2,
            DIMMER,
        );
        return;
    }
    let cols = 3usize;
    let cell = (wi - 40) / cols as i32;
    let thumbs: Vec<Option<(u32, u32, Vec<u8>)>> = st
        .photos
        .iter()
        .map(|p| {
            let path = crate::persist::photos_dir(&st.data_dir).join(&p.file);
            load_png_rgba(&path).map(|rgba| {
                let s = 160.0 / p.w.max(p.h) as f32;
                let tw = ((p.w as f32 * s).round() as u32).max(1);
                let th = ((p.h as f32 * s).round() as u32).max(1);
                let small = crate::scale_rgba(&rgba, p.w, p.h, tw, th);
                (tw, th, small)
            })
        })
        .collect();
    for (i, t) in thumbs.iter().enumerate() {
        let col = i % cols;
        let row = i / cols;
        let x = 20 + col as i32 * (cell + 10);
        let y = y0 + row as i32 * (cell + 10);
        c.fill_rect(x, y, cell, cell, SURFACE);
        if let Some((tw, th, small)) = t {
            c.blit(
                x + (cell - *tw as i32) / 2,
                y + (cell - *th as i32) / 2,
                *tw,
                *th,
                small,
            );
        }
    }
}

pub fn load_png_rgba(path: &std::path::Path) -> Option<Vec<u8>> {
    let bytes = std::fs::read(path).ok()?;
    let mut dec = png::Decoder::new(std::io::Cursor::new(bytes));
    dec.set_transformations(png::Transformations::EXPAND);
    let mut reader = dec.read_info().ok()?;
    let mut buf = vec![0; reader.output_buffer_size()];
    let info = reader.next_frame(&mut buf).ok()?;
    let w = info.width;
    let h = info.height;
    let color = info.color_type;
    let src = &buf[..info.buffer_size()];
    let mut rgba = Vec::with_capacity((w * h * 4) as usize);
    match color {
        png::ColorType::Rgba => rgba.extend_from_slice(src),
        png::ColorType::Rgb => {
            for px3 in src.chunks_exact(3) {
                rgba.extend_from_slice(px3);
                rgba.push(255);
            }
        }
        png::ColorType::Grayscale => {
            for g in src {
                rgba.extend_from_slice(&[*g, *g, *g, 255]);
            }
        }
        png::ColorType::GrayscaleAlpha => {
            for ga in src.chunks_exact(2) {
                rgba.extend_from_slice(&[ga[0], ga[0], ga[0], ga[1]]);
            }
        }
        _ => return None,
    }
    Some(rgba)
}

fn touch_photos(st: &mut OsState, x: i32, y: i32) {
    let (w, h) = st.effective_size();
    let wi = w as i32;
    let content_h = h as i32 - STATUS_H - NAV_H;
    if let Some(idx) = st.app.photos.viewer {
        if in_rect(x, y, wi / 2 - 80, STATUS_H + content_h - 120, 160, 64) {
            // delete the real file + entry
            if let Some(p) = st.photos.get(idx).cloned() {
                let _ =
                    std::fs::remove_file(crate::persist::photos_dir(&st.data_dir).join(&p.file));
                st.photos.remove(idx);
                st.persist_dirty = true;
            }
            st.app.photos.viewer = None;
            st.dirty = true;
        }
        return;
    }
    let y0 = STATUS_H + 120;
    let cols = 3usize;
    let cell = (wi - 40) / cols as i32;
    let row = ((y - y0).max(0) / (cell + 10)) as usize;
    let col = ((x - 20).max(0) / (cell + 10)) as usize;
    let idx = row * cols + col;
    if idx < st.photos.len() && y >= y0 {
        st.app.photos.viewer = Some(idx);
        st.dirty = true;
    }
}

// ── Files ────────────────────────────────────────────────────────────────────

fn render_files(c: &mut Canvas, st: &mut OsState) {
    let (w, h) = st.effective_size();
    let wi = w as i32;
    let content_h = h as i32 - STATUS_H - NAV_H;
    if let Some((name, is_dir, size, detail)) = st.app.files.detail.clone() {
        app_bar(c, "File details");
        c.round_rect(36, STATUS_H + 120, wi - 72, 260, 24, SURFACE);
        draw_app_icon(c, &AppId::Files, wi / 2 - 44, STATUS_H + 150, 88);
        c.text_center(wi / 2, STATUS_H + 270, &name, 4, TEXT);
        let kind = if is_dir {
            "folder".to_string()
        } else {
            fmt_size(size)
        };
        c.text_center(wi / 2, STATUS_H + 320, &kind, 3, DIM);
        c.text_fit(60, STATUS_H + 400, &detail, 2, wi - 120, DIM);
        return;
    }
    app_bar(c, &format!("/sdcard{}", rel_path(&st.app.files.cwd, st)));
    let y0 = STATUS_H + 120;
    let entries = &st.app.files.entries;
    if entries.is_empty() {
        c.text_center(wi / 2, content_h / 2, "Empty folder", 3, DIM);
        return;
    }
    let vis = ((content_h - 130) / 100).max(1) as usize;
    for (i, (name, is_dir, size)) in entries.iter().take(vis).enumerate() {
        let y = y0 + i as i32 * 100;
        c.fill_rect(0, y, wi, 100, if i % 2 == 0 { SURFACE } else { BG });
        if *is_dir {
            c.fill_rect(40, y + 28, 52, 20, ACCENT);
            c.round_rect(40, y + 36, 76, 44, 8, ACCENT);
        } else {
            c.round_rect(44, y + 28, 60, 52, 8, SURFACE_HI);
            c.text_center(74, y + 44, "≡", 3, DIM);
        }
        c.text_fit(140, y + 30, name, 3, wi - 240, TEXT);
        if !*is_dir {
            c.text(140, y + 64, &fmt_size(*size), 2, DIM);
        }
    }
}

fn rel_path(cwd: &std::path::Path, st: &OsState) -> String {
    let sd = crate::persist::sdcard_dir(&st.data_dir);
    cwd.strip_prefix(&sd)
        .map(|rest| {
            let s = rest.display().to_string();
            if s.is_empty() {
                String::new()
            } else {
                format!("/{s}")
            }
        })
        .unwrap_or_default()
}

fn fmt_size(bytes: u64) -> String {
    if bytes < 1024 {
        format!("{bytes} B")
    } else if bytes < 1024 * 1024 {
        format!("{:.1} KB", bytes as f64 / 1024.0)
    } else {
        format!("{:.1} MB", bytes as f64 / (1024.0 * 1024.0))
    }
}

pub fn refresh_files(st: &mut OsState) {
    let ui = &mut st.app.files;
    ui.entries.clear();
    if let Ok(rd) = std::fs::read_dir(&ui.cwd) {
        for e in rd.flatten() {
            let name = e.file_name().to_string_lossy().to_string();
            if name.starts_with('.') {
                continue;
            }
            let is_dir = e.file_type().map(|t| t.is_dir()).unwrap_or(false);
            let size = e.metadata().map(|m| m.len()).unwrap_or(0);
            ui.entries.push((name, is_dir, size));
        }
    }
    ui.entries.sort_by(|a, b| {
        b.1.cmp(&a.1)
            .then(a.0.to_lowercase().cmp(&b.0.to_lowercase()))
    });
    st.dirty = true;
}

fn touch_files(st: &mut OsState, _x: i32, y: i32) {
    let (w, h) = st.effective_size();
    let wi = w as i32;
    let content_h = h as i32 - STATUS_H - NAV_H;
    if st.app.files.detail.is_some() {
        st.app.files.detail = None;
        st.dirty = true;
        return;
    }
    let y0 = STATUS_H + 120;
    let vis = ((content_h - 130) / 100).max(1) as usize;
    let idx = ((y - y0).max(0) / 100) as usize;
    if y >= y0 && idx < vis {
        if let Some((name, is_dir, size)) = st.app.files.entries.get(idx).cloned() {
            let full = st.app.files.cwd.join(&name);
            if is_dir {
                st.app.files.cwd = full;
                refresh_files(st);
            } else {
                let detail = std::fs::read_to_string(&full)
                    .map(|t| {
                        let head: String = t.chars().take(600).collect();
                        head
                    })
                    .unwrap_or_else(|_| "(binary file)".into());
                st.app.files.detail = Some((name, is_dir, size, detail));
            }
            st.dirty = true;
        }
    }
    let _ = wi;
}

// ── Termux ───────────────────────────────────────────────────────────────────

const TERMUX_BG: Rgb = [0, 0, 0];
const TERMUX_FG: Rgb = [224, 228, 224];
const TERMUX_GREEN: Rgb = [120, 220, 130];
const TERMUX_DIMC: Rgb = [110, 116, 110];

/// Append a line to the Termux scrollback (session buffer, capped).
pub fn termux_print(st: &mut OsState, text: &str, color: Rgb, scale: u32) {
    let ui = &mut st.app.termux;
    ui.lines.push(TermuxLine {
        text: text.to_string(),
        scale,
        color,
    });
    if ui.lines.len() > 400 {
        ui.lines.drain(..ui.lines.len() - 400);
    }
    st.dirty = true;
}

/// Run one command inside the Termux environment. `typed=true` when entered
/// at the terminal prompt (echo the command), `false` when driven by the
/// RUN_COMMAND service (echo as a labelled banner instead).
pub fn termux_run_command(st: &mut OsState, cmd: &str, typed: bool) -> String {
    if typed {
        termux_print(st, &format!("$ {cmd}"), TERMUX_GREEN, 2);
    } else {
        termux_print(st, &format!("[RUN_COMMAND] {cmd}"), TERMUX_DIMC, 2);
    }
    st.app.termux.run_commands += 1;
    // execute against the device shell (same toybox the adb shell uses)
    let out = crate::shell::exec(st, cmd);
    let text = String::from_utf8_lossy(&out).to_string();
    for line in text.lines().take(60) {
        termux_print(st, line, TERMUX_FG, 2);
    }
    st.app.termux.input.clear();
    st.dirty = true;
    text
}

fn render_termux(c: &mut Canvas, st: &mut OsState) {
    let (w, h) = st.effective_size();
    let wi = w as i32;
    let content_h = h as i32 - STATUS_H - NAV_H;
    c.fill_rect(0, STATUS_H, wi, content_h, TERMUX_BG);
    let ui = &st.app.termux;
    // terminal body: bottom-anchored above the input row + keyboard
    let input_h = 56;
    let kb_h = if ui.kb.visible { KB_H } else { 0 };
    let bottom = h as i32 - NAV_H - kb_h - input_h;
    let line_h = 36;
    let visible = ((bottom - STATUS_H) / line_h).max(1) as usize;
    let start = ui.lines.len().saturating_sub(visible);
    let mut yy = bottom - (ui.lines.len() - start) as i32 * line_h;
    for l in &ui.lines[start..] {
        c.text_fit(16, yy + 4, &l.text, l.scale, wi - 32, l.color);
        yy += line_h;
    }
    // input row: prompt + current text + block cursor
    let iy = bottom;
    c.fill_rect(0, iy, wi, input_h, [6, 10, 8]);
    let prompt = if st.app.termux.input.is_empty() {
        "$ "
    } else {
        "> "
    };
    c.text(16, iy + 12, prompt, 3, TERMUX_GREEN);
    let px = 16 + Canvas::text_w(prompt, 3);
    let shown = {
        let inp = &st.app.termux.input;
        // keep the tail visible
        let max_chars = ((wi - px - 60) / (3 * 8)).max(2) as usize;
        if inp.chars().count() > max_chars {
            inp.chars()
                .skip(inp.chars().count() - max_chars)
                .collect::<String>()
        } else {
            inp.clone()
        }
    };
    let cx = c.text(px, iy + 12, &shown, 3, TERMUX_FG);
    // blinking cursor
    if (crate::state::now_secs() % 2) as i32 == 0 {
        c.fill_rect(cx, iy + 12, 3 * 8, 3 * 8, TERMUX_FG);
    }
    // keyboard (always offered on tap, hidden until first tap like a phone)
    if ui.kb.visible {
        render_keyboard(c, st);
    } else {
        let hy = iy + 12;
        c.text_center(wi - 120, hy + 8, "tap to type", 2, TERMUX_DIMC);
    }
}

fn touch_termux(st: &mut OsState, x: i32, y: i32) {
    let (w, h) = st.effective_size();
    let wi = w as i32;
    let ui = &st.app.termux;
    let input_h = 56;
    let kb_h = if ui.kb.visible { KB_H } else { 0 };
    let bottom = h as i32 - NAV_H - kb_h - input_h;
    if y >= bottom {
        if ui.kb.visible {
            keyboard_touch(st, x, y);
        } else {
            st.app.termux.kb.visible = true;
        }
        st.dirty = true;
        return;
    }
    // scroll back through history by tapping the upper area / lower area
    if st.app.termux.lines.len() > 8 {
        if y < bottom / 2 {
            st.app.termux.lines.drain(0..4);
        } else if y > bottom && y < h as i32 - NAV_H - kb_h {
            // nothing below
        }
        st.dirty = true;
    }
    let _ = wi;
}

// ── user-installed APK apps ──────────────────────────────────────────────────

fn render_user_app(c: &mut Canvas, st: &OsState, app: &AppId) {
    let (_w, h) = st.effective_size();
    let content_h = h as i32 - STATUS_H - NAV_H;
    c.fill_rect(0, STATUS_H, c.w as i32, content_h, BG);
    let label = st
        .package(&app.package())
        .map(|p| p.label.clone())
        .unwrap_or_else(|| app.label());
    let pkg = app.package();
    draw_app_icon(c, app, c.w as i32 / 2 - 60, STATUS_H + 140, 120);
    c.text_center(c.w as i32 / 2, STATUS_H + 300, &label, 5, TEXT);
    c.text_center(c.w as i32 / 2, STATUS_H + 360, &pkg, 2, DIM);
    c.text_center(
        c.w as i32 / 2,
        content_h / 2 + 80,
        "This app has no launcher UI on the built-in phone.",
        3,
        DIM,
    );
    c.text_center(
        c.w as i32 / 2,
        content_h / 2 + 130,
        "The package is installed; drive it with adb:",
        2,
        DIMMER,
    );
    c.text_center(
        c.w as i32 / 2,
        content_h / 2 + 170,
        &format!("adb shell am start -n {pkg}/.MainActivity"),
        2,
        ACCENT,
    );
}

// ── tap dispatch ─────────────────────────────────────────────────────────────

pub fn touch_app(st: &mut OsState, app: &AppId, x: i32, y: i32) {
    match app {
        AppId::Settings => touch_settings(st, x, y),
        AppId::Calculator => touch_calculator(st, x, y),
        AppId::Clock => touch_clock(st, x, y),
        AppId::Messages => touch_messages(st, x, y),
        AppId::Dialer => touch_dialer(st, x, y),
        AppId::Browser => touch_browser(st, x, y),
        AppId::Photos => touch_photos(st, x, y),
        AppId::Files => touch_files(st, x, y),
        AppId::Termux => touch_termux(st, x, y),
        AppId::Magisk => touch_magisk(st, x, y),
        AppId::RootExplorer => touch_root_explorer(st, x, y),
        AppId::TitaniumBackup => touch_titanium(st, x, y),
        AppId::User(_) => {}
    }
}

// ── Magisk — the root manager ────────────────────────────────────────────────

/// The su grants this device has handed out (the live `su.grant.*` keys —
/// the same policy the image's magisk.db ships and `su` enforces).
fn su_grants(st: &OsState) -> Vec<(String, String)> {
    st.settings
        .iter()
        .filter_map(|(k, v)| {
            k.strip_prefix("su.grant.")
                .map(|pkg| (pkg.to_string(), v.clone()))
        })
        .collect()
}

fn render_magisk(c: &mut Canvas, st: &mut OsState) {
    let (w, h) = st.effective_size();
    let wi = w as i32;
    let content_h = h as i32 - STATUS_H - NAV_H;
    c.fill_rect(0, STATUS_H, wi, content_h, BG);
    app_bar(c, "Magisk");
    let ui = st.app.magisk.clone();

    if let Some(pkg) = &ui.sel_grant {
        // grant detail
        app_bar(c, "Superuser");
        let label = st
            .package(pkg)
            .map(|p| p.label.clone())
            .unwrap_or_else(|| pkg.split('.').next_back().unwrap_or(pkg).to_string());
        let granted = st.settings.contains_key(&format!("su.grant.{pkg}"));
        c.round_rect(36, STATUS_H + 120, wi - 72, 240, 24, SURFACE);
        c.text_center(wi / 2, STATUS_H + 160, &label, 5, TEXT);
        c.text_fit(wi / 2 - 300, STATUS_H + 210, pkg, 2, 600, DIM);
        c.text_center(
            wi / 2,
            STATUS_H + 250,
            if granted {
                "root access: granted"
            } else {
                "root access: denied"
            },
            3,
            if granted { GREEN } else { RED },
        );
        c.text_center(
            wi / 2,
            STATUS_H + 300,
            "tap Grant / Revoke to change",
            2,
            DIMMER,
        );
        let by = STATUS_H + 400;
        c.round_rect(
            60,
            by,
            wi / 2 - 90,
            64,
            16,
            if granted { DIMMER } else { GREEN },
        );
        c.text_center(60 + (wi / 2 - 90) / 2, by + 20, "Grant", 4, TEXT);
        let rx = wi / 2 + 30;
        c.round_rect(
            rx,
            by,
            wi / 2 - 90,
            64,
            16,
            if granted { RED } else { DIMMER },
        );
        c.text_center(rx + (wi / 2 - 90) / 2, by + 20, "Revoke", 4, TEXT);
        return;
    }

    // status card
    let y0 = STATUS_H + 100;
    c.round_rect(36, y0, wi - 72, 190, 24, [26, 34, 26]);
    c.circle(90, y0 + 60, 26, GREEN);
    c.line(78, y0 + 60, 88, y0 + 72, [12, 20, 12], 8);
    c.line(88, y0 + 72, 104, y0 + 46, [12, 20, 12], 8);
    c.text(
        140,
        y0 + 24,
        &format!("Magisk {}", crate::os::magisk_version()),
        4,
        TEXT,
    );
    c.text(140, y0 + 62, "Installed", 3, GREEN);
    c.text(140, y0 + 98, "root access: provided", 2, DIM);
    c.text(140, y0 + 126, "Zygisk: on · boot image: patched", 2, DIM);
    c.text(140, y0 + 152, "su: /system/xbin/su", 2, DIMMER);

    // policy rows
    let mut y = y0 + 230;
    let auto = st.settings.get("su.auto_grant").map(String::as_str) == Some("1");
    row(
        c,
        y,
        96,
        "Auto-grant su",
        if auto { "on" } else { "off" },
        ACCENT,
    );
    toggle(c, wi - 130, y + 30, auto);
    c.text(
        40,
        y + 96,
        if auto {
            "trusted callers get su without a prompt"
        } else {
            "every su request asks in Magisk first"
        },
        2,
        DIMMER,
    );
    y += 120;
    c.text(48, y, "Superuser grants", 3, DIM);
    y += 40;
    let grants = su_grants(st);
    if grants.is_empty() {
        c.text(48, y + 30, "No apps have been granted root yet.", 3, DIMMER);
    }
    for (pkg, at) in grants.iter().skip(ui.scroll) {
        if y > h as i32 - NAV_H - 200 {
            break;
        }
        let label = st
            .package(pkg)
            .map(|p| p.label.clone())
            .unwrap_or_else(|| pkg.split('.').next_back().unwrap_or(pkg).to_string());
        row(
            c,
            y,
            88,
            &label,
            &format!("granted {}", {
                let d = now_ms().saturating_sub(at.parse::<i64>().unwrap_or(0)) / 1000;
                if d < 3600 {
                    format!("{d}s ago")
                } else {
                    format!("{}h ago", d / 3600)
                }
            }),
            GREEN,
        );
        y += 88;
    }
    let fy = y0 + 230 + 120 + 40 + (grants.len() as i32).max(1) * 88 + 30;
    if fy < h as i32 - NAV_H - 60 {
        c.round_rect(60, fy, wi - 120, 60, 14, SURFACE_HI);
        c.text_center(wi / 2, fy + 20, "adb shell su 0 -c id -u → 0", 2, ACCENT);
    }
}

fn touch_magisk(st: &mut OsState, x: i32, y: i32) {
    let (w, _h) = st.effective_size();
    let wi = w as i32;
    let y0 = STATUS_H + 100;
    if st.app.magisk.sel_grant.is_some() {
        // grant/revoke buttons
        let by = STATUS_H + 400;
        let pkg = st.app.magisk.sel_grant.clone().unwrap_or_default();
        if in_rect(x, y, 60, by, wi / 2 - 90, 64) {
            st.settings
                .insert(format!("su.grant.{pkg}"), now_ms().to_string());
            st.notify(
                "Magisk",
                &format!("root granted to {pkg}"),
                Some(AppId::Magisk),
            );
            st.app.magisk.sel_grant = None;
            st.persist_dirty = true;
            st.dirty = true;
            return;
        }
        let rx = wi / 2 + 30;
        if in_rect(x, y, rx, by, wi / 2 - 90, 64) {
            st.settings.remove(&format!("su.grant.{pkg}"));
            st.notify(
                "Magisk",
                &format!("root revoked from {pkg}"),
                Some(AppId::Magisk),
            );
            st.app.magisk.sel_grant = None;
            st.persist_dirty = true;
            st.dirty = true;
            return;
        }
        return;
    }
    // auto-grant toggle
    if in_rect(x, y, wi - 130, y0 + 30, 130, 80) {
        let cur = st.settings.get("su.auto_grant").map(String::as_str) == Some("1");
        st.settings.insert(
            "su.auto_grant".into(),
            if cur { "0".into() } else { "1".into() },
        );
        st.persist_dirty = true;
        st.dirty = true;
        return;
    }
    // grant rows
    let mut gy = y0 + 230 + 120 + 40;
    for (pkg, _) in su_grants(st) {
        if in_rect(x, y, 0, gy, wi, 88) {
            st.app.magisk.sel_grant = Some(pkg);
            st.dirty = true;
            return;
        }
        gy += 88;
    }
}

// ── Root Explorer — the real filesystem, with root ───────────────────────────

fn refresh_root_explorer(st: &mut OsState) {
    let cwd = st
        .app
        .rootexplorer
        .cwd
        .clone()
        .unwrap_or_else(|| "/".to_string());
    if cwd == "/" {
        // the mount roots of the installed image
        st.app.rootexplorer.entries = [
            ("/system", true, 0u64, true),
            ("/system_ext", true, 0, true),
            ("/product", true, 0, true),
            ("/vendor", true, 0, true),
            ("/data", true, 0, false),
            ("/cache", true, 0, false),
            ("/sdcard", true, 0, false),
        ]
        .iter()
        .map(|(p, d, s, ro)| (p.to_string(), *d, *s, *ro))
        .collect();
        return;
    }
    st.app.rootexplorer.entries = crate::os::list_dir(&st.data_dir, &cwd);
}

fn render_root_explorer(c: &mut Canvas, st: &mut OsState) {
    let (w, h) = st.effective_size();
    let wi = w as i32;
    let content_h = h as i32 - STATUS_H - NAV_H;
    c.fill_rect(0, STATUS_H, wi, content_h, BG);
    // first open: populate the mount-root list (lazily, like a real FM's
    // initial load)
    {
        let ui = &st.app.rootexplorer;
        if ui.cwd.is_none() && ui.entries.is_empty() && ui.detail.is_none() {
            refresh_root_explorer(st);
        }
    }
    let cwd = st
        .app
        .rootexplorer
        .cwd
        .clone()
        .unwrap_or_else(|| "/".to_string());
    app_bar(c, &format!("Root: {cwd}"));
    let ui = st.app.rootexplorer.clone();

    if let Some(detail) = &ui.detail {
        // file detail: path, size, and a preview of text files
        app_bar(c, "File (root)");
        let host = crate::os::host_path(&st.data_dir, detail);
        let size = host
            .as_ref()
            .and_then(|p| std::fs::metadata(p).ok())
            .map(|m| m.len())
            .unwrap_or(0);
        c.text_fit(48, STATUS_H + 130, detail, 3, wi - 96, TEXT);
        c.text(
            48,
            STATUS_H + 180,
            &format!("{} bytes — readable as root", size),
            2,
            DIM,
        );
        let preview = host
            .and_then(|p| std::fs::read(p).ok())
            .map(|b| {
                let head = b
                    .iter()
                    .take(160)
                    .map(|c| {
                        if (32..126).contains(c) {
                            *c as char
                        } else {
                            '.'
                        }
                    })
                    .collect::<String>();
                head
            })
            .unwrap_or_default();
        c.round_rect(36, STATUS_H + 220, wi - 72, 260, 16, SURFACE);
        c.text(56, STATUS_H + 240, "content preview", 2, DIMMER);
        for (i, line) in preview.as_bytes().chunks(34).enumerate().take(7) {
            let s: String = line.iter().map(|b| *b as char).collect();
            c.text(56, STATUS_H + 268 + i as i32 * 30, &s, 2, TEXT);
        }
        return;
    }

    // entries
    let mut y = STATUS_H + 110;
    if ui.entries.is_empty() {
        c.text(48, y + 40, "(empty)", 3, DIMMER);
    }
    for (name, is_dir, size, ro) in ui.entries.iter() {
        if y > h as i32 - NAV_H - 120 {
            break;
        }
        c.fill_rect(0, y, wi, 86, SURFACE);
        let prefix = if *is_dir {
            ">"
        } else if *ro {
            "#"
        } else {
            " "
        };
        c.text(40, y + 24, prefix, 3, if *is_dir { ACCENT } else { YELLOW });
        c.text_fit(90, y + 24, name, 3, wi - 260, TEXT);
        if !is_dir {
            c.text_fit(wi - 190, y + 30, &format_size(*size), 2, 150, DIMMER);
        } else if *ro {
            c.text(wi - 130, y + 30, "ro", 2, DIMMER);
        }
        y += 86;
    }
}

fn touch_root_explorer(st: &mut OsState, x: i32, y: i32) {
    let (w, h) = st.effective_size();
    let wi = w as i32;
    let cwd = st
        .app
        .rootexplorer
        .cwd
        .clone()
        .unwrap_or_else(|| "/".to_string());
    let mut y0 = STATUS_H + 110;
    for (name, is_dir, _size, _ro) in st.app.rootexplorer.entries.clone() {
        if y0 > h as i32 - NAV_H - 120 {
            break;
        }
        if in_rect(x, y, 0, y0, wi, 86) {
            // mount-root names are already absolute ("/system")
            let full = if cwd == "/" || name.starts_with('/') {
                name
            } else {
                format!("{cwd}/{name}")
            };
            if is_dir {
                st.app.rootexplorer.cwd = Some(full);
                refresh_root_explorer(st);
            } else {
                st.app.rootexplorer.detail = Some(full);
            }
            st.dirty = true;
            return;
        }
        y0 += 86;
    }
}

fn format_size(n: u64) -> String {
    if n >= 1024 * 1024 {
        format!("{:.1} MiB", n as f32 / 1024.0 / 1024.0)
    } else if n >= 1024 {
        format!("{:.1} KiB", n as f32 / 1024.0)
    } else {
        format!("{n} B")
    }
}

// ── Titanium Backup — root batch backup to /sdcard ───────────────────────────

fn render_titanium(c: &mut Canvas, st: &mut OsState) {
    let (w, h) = st.effective_size();
    let wi = w as i32;
    let content_h = h as i32 - STATUS_H - NAV_H;
    c.fill_rect(0, STATUS_H, wi, content_h, BG);
    app_bar(c, "Titanium Backup");
    let ui = st.app.titanium.clone();

    // batch progress
    let y0 = STATUS_H + 100;
    if let Some((done, total)) = ui.progress {
        c.round_rect(36, y0, wi - 72, 120, 20, SURFACE);
        c.text(
            64,
            y0 + 24,
            &format!("Batch backup — {done}/{total} packages"),
            3,
            TEXT,
        );
        let bw = wi - 200;
        c.round_rect(64, y0 + 66, bw, 14, 7, [30, 36, 46]);
        c.round_rect(
            64,
            y0 + 66,
            bw.max(1) * (done.min(total) as i32) / (total.max(1) as i32),
            14,
            7,
            GREEN,
        );
    } else {
        c.round_rect(36, y0, wi - 72, 120, 20, SURFACE);
        c.text(64, y0 + 24, "Backup apps + system data (root)", 3, TEXT);
        c.text(
            64,
            y0 + 62,
            "Backups are written to /sdcard/TitaniumBackup/",
            2,
            DIM,
        );
        c.round_rect(wi / 2 - 150, y0 + 80, 300, 60, 14, ACCENT);
        c.text_center(wi / 2, y0 + 100, "Backup all apps", 4, [10, 14, 20]);
    }

    // package rows with their backup state
    let mut y = y0 + 150;
    c.text(48, y, "Installed apps", 3, DIM);
    y += 44;
    for pkg in st.packages.iter().skip(ui.scroll) {
        if y > h as i32 - NAV_H - 160 {
            break;
        }
        let backed = st.settings.get(&format!("titan.{}", pkg.package)).cloned();
        let col = if backed.is_some() { GREEN } else { DIMMER };
        c.fill_rect(0, y, wi, 84, SURFACE);
        c.text_fit(48, y + 16, &pkg.label, 3, wi - 260, TEXT);
        c.text_fit(48, y + 48, &pkg.package, 2, wi - 260, DIMMER);
        c.text(
            wi - 220,
            y + 28,
            if backed.is_some() {
                "backed up"
            } else {
                "not backed"
            },
            2,
            col,
        );
        y += 84;
    }
    // session log (last lines)
    if !ui.log.is_empty() {
        let ly = h as i32 - NAV_H - 90 - ui.log.len().min(3) as i32 * 26;
        for (i, l) in ui.log.iter().rev().take(3).enumerate() {
            c.text_fit(48, ly + i as i32 * 26, l, 2, wi - 96, ACCENT);
        }
    }
}

fn touch_titanium(st: &mut OsState, x: i32, y: i32) {
    let (w, h) = st.effective_size();
    let wi = w as i32;
    let y0 = STATUS_H + 100;
    if st.app.titanium.progress.is_none() && in_rect(x, y, wi / 2 - 150, y0 + 80, 300, 60) {
        start_titanium_backup(st, None);
        st.dirty = true;
        return;
    }
    let mut py = y0 + 150 + 44;
    for pkg in st.packages.clone() {
        if py > h as i32 - NAV_H - 160 {
            break;
        }
        if in_rect(x, y, 0, py, wi, 84) {
            start_titanium_backup(st, Some(pkg.package.clone()));
            st.dirty = true;
            return;
        }
        py += 84;
    }
}

/// Arm a backup run: `None` = all packages (one per compositor tick),
/// `Some(pkg)` = a single package, done immediately.
fn start_titanium_backup(st: &mut OsState, only: Option<String>) {
    let pkgs: Vec<String> = match only {
        Some(p) => vec![p],
        None => st.packages.iter().map(|p| p.package.clone()).collect(),
    };
    if pkgs.is_empty() {
        return;
    }
    let total = pkgs.len();
    st.app.titanium.progress = Some((0, total));
    st.app.titanium.pending = Some(pkgs);
    st.notify(
        "Titanium Backup",
        &format!("batch backup started — {total} packages"),
        Some(AppId::TitaniumBackup),
    );
}

/// One backup step (called per compositor tick while a batch runs): writes a
/// real file under /sdcard/TitaniumBackup/ for the next package.
pub fn titanium_step(st: &mut OsState) {
    let Some((done, total)) = st.app.titanium.progress else {
        return;
    };
    let Some(pending) = &st.app.titanium.pending else {
        st.app.titanium.progress = None;
        return;
    };
    let Some(pkg) = pending.get(done) else {
        st.app.titanium.progress = None;
        st.app.titanium.pending = None;
        return;
    };
    let package = pkg.clone();
    // the real work: a file per package with the package's live facts
    let sd = crate::persist::sdcard_dir(&st.data_dir);
    let dir = sd.join("TitaniumBackup");
    let _ = std::fs::create_dir_all(&dir);
    let stamp = crate::state::now_secs();
    let file = dir.join(format!("{package}-{stamp}.tib"));
    let label = st
        .package(&package)
        .map(|p| p.label.clone())
        .unwrap_or_default();
    let version = st
        .package(&package)
        .map(|p| p.version.clone())
        .unwrap_or_default();
    let content = format!(
        "TITANIUM-BACKUP v6.2.0 (root mode)\npackage: {package}\nlabel: {label}\nversion: {version}\nbacked_at: {stamp}\nsource: /data/data/{package} (read as root)\nuid: {}\n---\n",
        10000 + (package.len() % 900),
    );
    let bytes = content.len();
    let _ = std::fs::write(&file, content);
    st.settings
        .insert(format!("titan.{package}"), now_ms().to_string());
    st.app
        .titanium
        .log
        .push(format!("{package} → {bytes} bytes"));
    st.persist_dirty = true;
    let next = done + 1;
    if next >= total {
        st.app.titanium.progress = None;
        st.app.titanium.pending = None;
        st.notify(
            "Titanium Backup",
            &format!("batch complete — {total} backups in /sdcard/TitaniumBackup"),
            Some(AppId::TitaniumBackup),
        );
    } else {
        st.app.titanium.progress = Some((next, total));
    }
}

#[cfg(test)]
mod browser_link_tests {
    use super::*;

    const BASE: &str = "https://en.example.org/wiki/Foo/Bar?x=1#top";

    #[test]
    fn relative_links_resolve_like_a_browser() {
        let r = |h: &str| resolve_relative(BASE, h);
        assert_eq!(r("/a/B").as_deref(), Some("https://en.example.org/a/B"));
        assert_eq!(
            r("Baz").as_deref(),
            Some("https://en.example.org/wiki/Foo/Baz")
        );
        assert_eq!(
            r("../Qux").as_deref(),
            Some("https://en.example.org/wiki/Qux")
        );
        assert_eq!(r("//cdn.z.com/a").as_deref(), Some("https://cdn.z.com/a"));
        assert_eq!(
            r("?q=2").as_deref(),
            Some("https://en.example.org/wiki/Foo/Bar?q=2")
        );
        assert_eq!(r("#sec"), None);
        assert_eq!(r("mailto:a@b.c"), None);
        assert_eq!(r("javascript:void(0)"), None);
    }

    #[test]
    fn href_keeps_case_and_decodes_entities() {
        assert_eq!(
            extract_href("a href=\"/Wiki/Rust?a=1&amp;b=2\" class=x").as_deref(),
            Some("/Wiki/Rust?a=1&b=2")
        );
        assert_eq!(
            extract_href("a data-href=\"/no\" href='/yes'").as_deref(),
            Some("/yes")
        );
    }

    #[test]
    fn address_bar_normalises() {
        assert_eq!(normalize_url("example.com"), "https://example.com");
        assert_eq!(
            normalize_url("rust lang"),
            "https://html.duckduckgo.com/html/?q=rust+lang"
        );
    }
}
