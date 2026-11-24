//! The device shell — the commands `adbd` runs when a client opens an
//! `exec:`/`shell:` channel. This is the phone's toybox: getprop, wm, input,
//! screencap, pm, am, dumpsys, svc, settings, sms and a real little
//! file-system shell over the sdcard directory.

use std::path::{Component, Path, PathBuf};

use crate::apps;
use crate::persist;
use crate::state::*;

// ── entry ────────────────────────────────────────────────────────────────────

/// Run one shell command against the device state. Returns raw stdout bytes
/// (PNG for `screencap -p`).
pub fn exec(st: &mut OsState, cmdline: &str) -> Vec<u8> {
    let cmdline = unwrap_su(cmdline.trim());
    // top-level `;` separators run sequentially (real shell semantics)
    let mut acc = Vec::new();
    for seg in split_commands(&cmdline) {
        let seg = seg.trim();
        if seg.is_empty() {
            continue;
        }
        let tokens = tokenize(seg);
        if tokens.is_empty() {
            continue;
        }
        let out = dispatch(st, &tokens, seg);
        acc.extend(out);
        if !acc.ends_with(b"\n") && !acc.is_empty() {
            acc.push(b'\n');
        }
    }
    acc
}

/// Split on `;` that sits outside single/double quotes.
fn split_commands(cmd: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut cur = String::new();
    let mut quote: Option<char> = None;
    for ch in cmd.chars() {
        match quote {
            Some(q) => {
                cur.push(ch);
                if ch == q {
                    quote = None;
                }
            }
            None => {
                if ch == '\'' || ch == '"' {
                    quote = Some(ch);
                    cur.push(ch);
                } else if ch == ';' {
                    out.push(std::mem::take(&mut cur));
                } else {
                    cur.push(ch);
                }
            }
        }
    }
    out.push(cur);
    out
}

/// `su 0 -c 'inner'` → inner (our adbd is already root).
fn unwrap_su(cmd: &str) -> String {
    if let Some(rest) = cmd.strip_prefix("su 0 -c ") {
        let inner = rest.trim().trim_matches('\'');
        return inner.replace(r"\'", "'");
    }
    if cmd == "su" || cmd.starts_with("su ") {
        // bare `su` in a non-interactive shell: run the rest verbatim
        return cmd.strip_prefix("su ").unwrap_or("").trim().to_string();
    }
    cmd.to_string()
}

/// POSIX-ish tokenizer with single/double quotes.
pub fn tokenize(cmd: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut cur = String::new();
    let mut quote: Option<char> = None;
    let mut any = false;
    for ch in cmd.chars() {
        match quote {
            Some(q) if ch == q => {
                quote = None;
                any = true;
            }
            Some(_) => cur.push(ch),
            None if ch == '\'' || ch == '"' => {
                quote = Some(ch);
                any = true;
            }
            None if ch == ' ' || ch == '\t' => {
                if any || !cur.is_empty() {
                    out.push(std::mem::take(&mut cur));
                    any = false;
                }
            }
            None => cur.push(ch),
        }
    }
    if any || !cur.is_empty() {
        out.push(cur);
    }
    out
}

fn dispatch(st: &mut OsState, t: &[String], cmdline: &str) -> Vec<u8> {
    match t[0].as_str() {
        "id" => {
            // the full identity line, or the requested field (-u/-g/-G/-n)
            let (uid, gid, name) = if st.rooted {
                (0u32, 0u32, "root")
            } else {
                (2000, 2000, "shell")
            };
            let want = |flag: &str| t[1..].iter().any(|a| a == flag);
            if want("-u") {
                format!("{uid}").into_bytes()
            } else if want("-g") || want("-G") {
                format!("{gid}").into_bytes()
            } else if want("-n") {
                name.as_bytes().to_vec()
            } else if st.rooted {
                b"uid=0(root) gid=0(root) groups=0(root),1004(input),1015(sdcard_rw),2000(shell)"
                    .to_vec()
            } else {
                b"uid=2000(shell) gid=2000(shell) groups=1004(input),1015(sdcard_rw)".to_vec()
            }
        }
        "getprop" => getprop(st, t),
        "os" => os_cmd(st, t),
        "dmesg" => dmesg(st),
        "ps" => ps(st),
        "mount" => mount_cmd(st),
        "reboot" => reboot_cmd(st, t),
        "esim" => esim_cmd(st, t),
        "wm" => wm(st, t),
        "input" => input_cmd(st, t),
        "screencap" => screencap(st, t),
        "pm" => pm(st, t),
        "am" => am(st, t),
        "monkey" => monkey(st, t),
        "dumpsys" => dumpsys(st, t),
        "service" => service_cmd(st, t),
        "svc" => svc(st, t),
        "settings" => settings_cmd(st, t),
        "sms" => sms_cmd(st, t),
        "getenforce" => b"Enforcing".to_vec(),
        "pkg" => pkg_cmd(st, t),
        "termux-info" => termux_info(st),
        "termux-setup-storage" => termux_setup_storage(st),
        "cd" => {
            if t.len() >= 2 {
                let target = if t[1] == "~" || t[1].starts_with("~/") || t[1] == "$HOME" {
                    resolve_path(st, TERMUX_HOME)
                } else {
                    resolve_path(st, &t[1])
                };
                if target.as_ref().is_some_and(|p| p.is_dir()) {
                    let target = target.unwrap();
                    st.shell_cwd = Some(target.clone());
                    st.app.termux.cwd = Some(target);
                } else {
                    return format!("cd: {}: No such file or directory", t[1]).into_bytes();
                }
            } else if let Some(home) = st.app.termux.cwd.clone() {
                st.shell_cwd = Some(home);
            }
            Vec::new()
        }
        "setprop" => {
            if t.len() >= 3 {
                st.settings.insert(format!("prop.{}", t[1]), t[2].clone());
            }
            Vec::new()
        }
        "uname" => uname(st, t),
        "date" => date_string().into_bytes(),
        "uptime" => {
            let ms = st.booted_at.elapsed().unwrap_or_default().as_millis();
            format!("up time: {ms}ms, deep sleep: 0ms, awake time: {ms}ms").into_bytes()
        }
        "echo" => {
            let joined = t[1..].join(" ");
            let expanded = expand_dollar_paren(&expand_env(&joined, st));
            expanded.into_bytes()
        }
        "true" | "false" | ":" => Vec::new(),
        "logcat" => logcat(st),
        "df" => df(st),
        "free" => free_cmd(st),
        "ls" | "cat" | "mkdir" | "rmdir" | "rm" | "touch" | "cp" | "mv" => fs_cmd(st, t),
        "which" => {
            if t.len() > 1 {
                let bins: &[(&str, &str)] = &[
                    ("su", "/system/xbin/su"),
                    ("magisk", "/system/xbin/magisk"),
                    ("magiskpolicy", "/system/xbin/magiskpolicy"),
                    ("getprop", "/system/bin/getprop"),
                    ("pm", "/system/bin/pm"),
                    ("am", "/system/bin/am"),
                    ("input", "/system/bin/input"),
                    ("screencap", "/system/bin/screencap"),
                    ("wm", "/system/bin/wm"),
                    ("sh", "/system/bin/sh"),
                    ("dmesg", "/system/bin/dmesg"),
                    ("ps", "/system/bin/ps"),
                    ("mount", "/system/bin/mount"),
                    ("os", "/system/bin/os"),
                ];
                if let Some((_, path)) = bins.iter().find(|(n, _)| *n == t[1]) {
                    return path.as_bytes().to_vec();
                }
            }
            Vec::new()
        }
        "sh" | "/system/bin/sh" | "/data/data/com.termux/files/usr/bin/sh" => {
            // `sh -c 'cmd'` or multi-line scripts: run each line
            let rest = cmdline
                .strip_prefix("sh -c ")
                .map(|s| s.trim_matches('\'').trim_matches('"').to_string())
                .unwrap_or_default();
            let mut acc = Vec::new();
            for line in rest.lines().filter(|l| !l.trim().is_empty()) {
                acc.extend(exec_inner_lines(st, line));
            }
            acc
        }
        "cmd" => {
            // `cmd package ...` / `cmd activity ...` aliases used by some stacks
            if t.len() >= 3 && (t[1] == "package" || t[1] == "activity") {
                let head = if t[1] == "package" { "pm" } else { "am" };
                let mut mapped = vec![head.to_string()];
                mapped.extend(t[2..].iter().cloned());
                return dispatch(st, &mapped, cmdline);
            }
            format!(
                "cmd: unknown service {}",
                t.get(1).map(String::as_str).unwrap_or("?")
            )
            .into_bytes()
        }
        "exit" | "quit" => Vec::new(),
        other => format!("/system/bin/sh: {}: not found", other).into_bytes(),
    }
}

fn exec_inner_lines(st: &mut OsState, line: &str) -> Vec<u8> {
    let mut out = exec(st, line);
    if !out.ends_with(b"\n") {
        out.push(b'\n');
    }
    out
}

// ── getprop ──────────────────────────────────────────────────────────────────

/// The live `getprop` table: the physical identity's base `ro.` set plus the
/// live telephony/display state. Values come from the same radio model the
/// compositor renders — getprop and the screen cannot disagree.
pub fn prop_table(st: &OsState) -> Vec<(String, String)> {
    let (w, h) = st.effective_size();
    let id = &st.identity;
    let mut props = mandroid::identity::base_ro_props(id);

    // live per-slot telephony state (slot 1, slot 2)
    let r1 = st.radio_now(1);
    let r2 = st.radio_now(2);
    let radii = [r1, r2];
    let sim_state: Vec<String> = radii
        .iter()
        .map(|r| {
            if r.registered {
                "LOADED".to_string()
            } else {
                "ABSENT".to_string()
            }
        })
        .collect();
    let alpha: Vec<String> = radii.iter().map(|r| r.operator_alpha.clone()).collect();
    let numeric: Vec<String> = radii.iter().map(|r| r.operator_numeric.clone()).collect();
    let iso: Vec<String> = radii
        .iter()
        .map(|r| {
            if r.registered {
                profile_country(st, &r.operator_numeric).to_lowercase()
            } else {
                String::new()
            }
        })
        .collect();
    let net: Vec<String> = radii.iter().map(|r| r.technology.to_string()).collect();
    let data = st.data_radio_now();

    props.push(("gsm.sim.state".into(), sim_state.join(",")));
    props.push(("gsm.operator.alpha".into(), alpha.join(",")));
    props.push(("gsm.operator.numeric".into(), numeric.join(",")));
    props.push(("gsm.operator.iso-country".into(), iso.join(",")));
    props.push((
        "gsm.operator.isroaming".into(),
        if radii.iter().any(|r| r.roaming) {
            "true"
        } else {
            "false"
        }
        .into(),
    ));
    props.push(("gsm.network.type".into(), net.join(",")));
    props.push(("gsm.sim.operator.alpha".into(), alpha.join(",")));
    props.push(("gsm.sim.operator.numeric".into(), numeric.join(",")));
    props.push(("gsm.sim.operator.iso-country".into(), iso.join(",")));
    props.push(("ro.carrier".into(), data.operator_alpha.clone()));
    props.push(("persist.radio.multisim.config".into(), "dsds".into()));
    props.push(("ro.telephony.get-imsi".into(), "1".into()));
    props.push(("ro.vendor.multisim.config".into(), "dsds".into()));

    // live display state
    props.push((
        "ro.sf.lcd_density".into(),
        st.effective_density().to_string(),
    ));
    props.push(("debug.hwui.renderer".into(), "skiagl".into()));
    props.push(("screen.width".into(), w.to_string()));
    props.push(("screen.height".into(), h.to_string()));
    props.push(("service.adb.tcp.port".into(), st.adb_port.to_string()));

    // OS image + root (one source of truth: the installed image + live boot)
    let os_installed = crate::os::read_manifest(&st.data_dir);
    props.push((
        "ro.magisk.version".into(),
        crate::os::magisk_version().into(),
    ));
    props.push(("ro.magisk.vercode".into(), "29003".into()));
    props.push(("magisk.root_access".into(), "1".into()));
    props.push((
        "init.svc.magiskd".into(),
        if os_installed.is_some() {
            "running"
        } else {
            "stopped"
        }
        .into(),
    ));
    props.push((
        "init.svc.zygote".into(),
        if os_installed.is_some() {
            "running"
        } else {
            "stopped"
        }
        .into(),
    ));
    props.push((
        "sys.boot_completed".into(),
        if st.boot.boot_completed() { "1" } else { "0" }.into(),
    ));
    props.push(("sys.reboots.count".into(), st.reboots.to_string()));
    props.push((
        "ro.os.image".into(),
        if os_installed.is_some() {
            "caiman-factory (installed)"
        } else {
            "none"
        }
        .into(),
    ));

    // user-set overrides (setprop) last, so they win a name collision
    for (k, v) in &st.settings {
        if let Some(rest) = k.strip_prefix("prop.") {
            props.push((rest.to_string(), v.clone()));
        }
    }
    props
}

/// Country of the profile whose PLMN numeric matches (for iso-country).
fn profile_country(st: &OsState, operator_numeric: &str) -> &'static str {
    st.esim
        .profiles
        .iter()
        .find(|p| p.enabled && format!("{:03}{}", p.mcc, p.mnc) == operator_numeric)
        .and_then(|p| match p.country.as_str() {
            "US" => Some("US"),
            "GB" => Some("GB"),
            "PK" => Some("PK"),
            "AE" => Some("AE"),
            "SA" => Some("SA"),
            "CA" => Some("CA"),
            "DE" => Some("DE"),
            "IN" => Some("IN"),
            "TR" => Some("TR"),
            "BD" => Some("BD"),
            "SG" => Some("SG"),
            "AU" => Some("AU"),
            _ => None,
        })
        .unwrap_or("US")
}

fn getprop(st: &mut OsState, t: &[String]) -> Vec<u8> {
    let props = prop_table(st);
    // the table is built in layers (static identity → live state → user
    // setprop) and the LAST entry of a key wins — exactly `getprop` semantics;
    // the full listing prints sorted like the real tool
    if t.len() == 1 {
        let mut last: std::collections::BTreeMap<&str, &str> = Default::default();
        for (k, v) in &props {
            last.insert(k, v);
        }
        let mut out = String::new();
        for (k, v) in last {
            out.push_str(&format!("[{k}]: [{v}]\n"));
        }
        out.into_bytes()
    } else {
        let key = &t[1];
        props
            .iter()
            .rev()
            .find(|(k, _)| k == key)
            .map(|(_, v)| v.clone().into_bytes())
            .unwrap_or_default()
    }
}

// ── window manager ───────────────────────────────────────────────────────────

fn wm(st: &mut OsState, t: &[String]) -> Vec<u8> {
    if t.len() < 2 {
        return b"usage: wm [size|density] [WxH|DPI|reset]".to_vec();
    }
    match t[1].as_str() {
        "size" => {
            if t.len() >= 3 {
                if t[2] == "reset" {
                    st.size_override = None;
                } else if let Some((w, h)) = parse_wxh(&t[2]) {
                    st.size_override = Some((w, h));
                } else {
                    return format!("Error: invalid size {}", t[2]).into_bytes();
                }
                st.dirty = true;
                st.persist_dirty = true;
            }
            let (report, _) = wm_size_report(st);
            report
        }
        "density" => {
            if t.len() >= 3 {
                if t[2] == "reset" {
                    st.density_override = None;
                } else if let Ok(d) = t[2].parse::<u32>() {
                    if (16..=4096).contains(&d) {
                        st.density_override = Some(d);
                    } else {
                        return format!("Error: density {} out of range", d).into_bytes();
                    }
                } else {
                    return format!("Error: invalid density {}", t[2]).into_bytes();
                }
                st.dirty = true;
                st.persist_dirty = true;
            }
            let (report, _) = wm_size_report(st);
            let mut out = report;
            out.extend(format!("Physical density: {}\n", st.density).into_bytes());
            if let Some(d) = st.density_override {
                out.extend(format!("Override density: {d}\n").into_bytes());
            }
            out
        }
        "dismiss-keyguard" => {
            st.screen_off = false;
            st.dirty = true;
            Vec::new()
        }
        _ => format!("Error: unknown wm subcommand {}", t[1]).into_bytes(),
    }
}

fn wm_size_report(st: &OsState) -> (Vec<u8>, (u32, u32)) {
    let (ew, eh) = st.effective_size();
    let mut out = format!("Physical size: {}x{}\n", st.display.0, st.display.1).into_bytes();
    if st.size_override.is_some() {
        out.extend(format!("Override size: {ew}x{eh}\n").into_bytes());
    }
    (out, (ew, eh))
}

fn parse_wxh(s: &str) -> Option<(u32, u32)> {
    let (w, h) = s.split_once('x')?;
    let w = w.trim().parse().ok()?;
    let h = h.trim().parse().ok()?;
    Some((w, h))
}

// ── input ────────────────────────────────────────────────────────────────────

fn input_cmd(st: &mut OsState, t: &[String]) -> Vec<u8> {
    if t.len() < 2 {
        return b"usage: input [tap|swipe|text|keyevent] ...".to_vec();
    }
    let nums = |v: &[String], i: usize| v.get(i).and_then(|s| s.parse::<i32>().ok());
    match t[1].as_str() {
        "tap" => {
            let (Some(x), Some(y)) = (nums(t, 2), nums(t, 3)) else {
                return b"Error: tap requires X and Y".to_vec();
            };
            st.inq.push_back(InputEvent::Touch(TouchEvent {
                action: TouchAction::Down,
                x: x.max(0) as u32,
                y: y.max(0) as u32,
            }));
            st.inq.push_back(InputEvent::Touch(TouchEvent {
                action: TouchAction::Up,
                x: x.max(0) as u32,
                y: y.max(0) as u32,
            }));
            Vec::new()
        }
        "swipe" => {
            let (Some(x1), Some(y1), Some(x2), Some(y2)) =
                (nums(t, 2), nums(t, 3), nums(t, 4), nums(t, 5))
            else {
                return b"Error: swipe requires X1 Y1 X2 Y2 [MS]".to_vec();
            };
            st.inq.push_back(InputEvent::Touch(TouchEvent {
                action: TouchAction::Down,
                x: x1.max(0) as u32,
                y: y1.max(0) as u32,
            }));
            st.inq.push_back(InputEvent::Touch(TouchEvent {
                action: TouchAction::Move,
                x: ((x1 + x2) / 2).max(0) as u32,
                y: ((y1 + y2) / 2).max(0) as u32,
            }));
            st.inq.push_back(InputEvent::Touch(TouchEvent {
                action: TouchAction::Up,
                x: x2.max(0) as u32,
                y: y2.max(0) as u32,
            }));
            Vec::new()
        }
        "text" => {
            let text = t[2..].join(" ");
            if text.is_empty() {
                return b"Error: text requires an argument".to_vec();
            }
            st.inq.push_back(InputEvent::Text(TextEvent { text }));
            Vec::new()
        }
        "keyevent" => {
            let Some(code) = t.get(2) else {
                return b"Error: keyevent requires a keycode".to_vec();
            };
            let n: Option<i64> = code.parse().ok().or_else(|| keycode_by_name(code));
            let Some(n) = n else {
                return format!("Error: unknown keycode {code}").into_bytes();
            };
            if let Some(k) = KeyCode::from_number(n) {
                st.inq.push_back(InputEvent::Key(k));
            }
            Vec::new()
        }
        other => format!("Error: unknown input subcommand {other}").into_bytes(),
    }
}

fn keycode_by_name(name: &str) -> Option<i64> {
    Some(match name.to_ascii_uppercase().as_str() {
        "KEYCODE_HOME" => 3,
        "KEYCODE_BACK" => 4,
        "KEYCODE_VOLUME_UP" => 24,
        "KEYCODE_VOLUME_DOWN" => 25,
        "KEYCODE_POWER" => 26,
        "KEYCODE_ENTER" => 66,
        "KEYCODE_DEL" => 67,
        _ => return None,
    })
}

// ── screencap ────────────────────────────────────────────────────────────────

fn screencap(st: &mut OsState, t: &[String]) -> Vec<u8> {
    let png = screencap_png(st);
    if t.len() >= 2 && t[1] != "-p" && !t[1].starts_with('-') {
        // screencap /path — write the file
        let Some(path) = resolve_path(st, &t[1]) else {
            return b"Error: unsafe path".to_vec();
        };
        if std::fs::write(&path, &png).is_err() {
            return b"Error: cannot write file".to_vec();
        }
        return Vec::new();
    }
    if t.len() >= 3 && t[1] == "-p" {
        let Some(path) = resolve_path(st, &t[2]) else {
            return b"Error: unsafe path".to_vec();
        };
        if std::fs::write(&path, &png).is_err() {
            return b"Error: cannot write file".to_vec();
        }
        return Vec::new();
    }
    // stdout (PNG regardless — the client sniffs the signature)
    png
}

/// The committed framebuffer, encoded as PNG. Clone under the lock, encode
/// after: the compositor is only blocked for the copy, never the encode.
pub fn screencap_png(st: &OsState) -> Vec<u8> {
    mandroid::screen::encode_png(&st.fb, st.effective_size().0, st.effective_size().1)
        .unwrap_or_default()
}

// ── package manager ──────────────────────────────────────────────────────────

fn pm(st: &mut OsState, t: &[String]) -> Vec<u8> {
    if t.len() < 2 {
        return b"usage: pm [list|install|uninstall] ...".to_vec();
    }
    match t[1].as_str() {
        "list" => {
            let only = t.get(2).map(String::as_str).unwrap_or("");
            let mut out = String::new();
            for p in &st.packages {
                let show = match only {
                    "-3" => !p.system,
                    "-s" => p.system,
                    _ => true,
                };
                if show {
                    out.push_str(&format!("package:{}\n", p.package));
                }
            }
            out.into_bytes()
        }
        "install" => {
            let flags: Vec<&String> = t[2..].iter().filter(|a| a.starts_with('-')).collect();
            let _ = flags;
            let Some(apk_path) = t[2..].iter().find(|a| !a.starts_with('-')) else {
                return b"Error: pm install requires an APK path".to_vec();
            };
            let Some(local) = resolve_path(st, apk_path) else {
                return b"Error: unsafe path".to_vec();
            };
            install_apk(st, &local)
        }
        "uninstall" => {
            let Some(pkg) = t.get(2) else {
                return b"Error: pm uninstall requires a package".to_vec();
            };
            let existing = st.packages.iter().find(|p| &p.package == pkg).cloned();
            match existing {
                Some(p) if !p.system => {
                    st.packages.retain(|p| &p.package != pkg);
                    persist::save(st).ok();
                    st.dirty = true;
                    b"Success".to_vec()
                }
                Some(_) => b"Failure [DELETE_FAILED_INTERNAL_ERROR]".to_vec(),
                None => format!("Failure [package {} unknown]", pkg).into_bytes(),
            }
        }
        "path" => {
            if let Some(p) = st
                .packages
                .iter()
                .find(|p| p.package == t.get(2).map(String::as_str).unwrap_or(""))
            {
                format!("/data/app/{}.apk", p.package).into_bytes()
            } else {
                Vec::new()
            }
        }
        other => format!("Error: unknown pm subcommand {other}").into_bytes(),
    }
}

/// Install a real APK: decode its manifest with the engine's own parsers and
/// register the package. The launcher picks it up immediately.
fn install_apk(st: &mut OsState, local: &Path) -> Vec<u8> {
    let mut apk = match mandroid::apk::Apk::open(local.display().to_string().as_str()) {
        Ok(a) => a,
        Err(e) => return format!("Failure [INSTALL_PARSE_FAILED: {e}]").into_bytes(),
    };
    let root = match apk.manifest() {
        Ok(r) => r,
        Err(e) => return format!("Failure [INSTALL_PARSE_FAILED: {e}]").into_bytes(),
    };
    let m = mandroid::manifest::Manifest::from_node(&root);
    let pkg = m.package.clone();
    if pkg.is_empty() {
        return b"Failure [INSTALL_PARSE_FAILED_NO_PACKAGE_NAME]".to_vec();
    }
    let app_label = m.application.label.clone().unwrap_or_default();
    let label = if app_label.is_empty() {
        pkg.split('.').next_back().unwrap_or(&pkg).to_string()
    } else {
        app_label
    };
    let version = m.version_name.clone().unwrap_or_else(|| "1.0".into());
    st.packages.retain(|p| p.package != pkg);
    st.packages.push(Package {
        package: pkg.clone(),
        label: label.clone(),
        system: false,
        app: Some(AppId::User(pkg.clone())),
        version,
    });
    st.packages.sort_by(|a, b| a.package.cmp(&b.package));
    persist::save(st).ok();
    st.dirty = true;
    format!("Success\nPackage installed: {pkg} ({label})").into_bytes()
}

// ── activity manager ─────────────────────────────────────────────────────────

fn am(st: &mut OsState, t: &[String]) -> Vec<u8> {
    if t.len() < 2 {
        return b"usage: am [start|force-stop] ...".to_vec();
    }
    match t[1].as_str() {
        "start" => {
            let mut pkg = None;
            let mut i = 2;
            while i < t.len() {
                if t[i] == "-n" && i + 1 < t.len() {
                    pkg = t[i + 1].split('/').next().map(|p| p.to_string());
                    i += 2;
                } else {
                    i += 1;
                }
            }
            let Some(pkg) = pkg else {
                return b"Error: am start requires -n package/activity".to_vec();
            };
            launch_by_package(st, &pkg)
        }
        "startservice" | "start-foreground-service" => am_startservice(st, t),
        "force-stop" => {
            let Some(pkg) = t.get(2) else {
                return b"Error: am force-stop requires a package".to_vec();
            };
            let was_running = st
                .current
                .as_ref()
                .map(|a| a.package() == *pkg)
                .unwrap_or(false);
            if was_running {
                st.current = None;
            }
            st.recents.retain(|a| a.package() != *pkg);
            st.dirty = true;
            Vec::new()
        }
        other => format!("Error: unknown am subcommand {other}").into_bytes(),
    }
}

fn monkey(st: &mut OsState, t: &[String]) -> Vec<u8> {
    let mut pkg = None;
    let mut i = 1;
    while i < t.len() {
        if t[i] == "-p" && i + 1 < t.len() {
            pkg = Some(t[i + 1].clone());
            i += 2;
        } else {
            i += 1;
        }
    }
    let Some(pkg) = pkg else {
        return b"Error: monkey requires -p package".to_vec();
    };
    launch_by_package(st, &pkg)
}

fn launch_by_package(st: &mut OsState, pkg: &str) -> Vec<u8> {
    // while the system is still booting the activity manager is not up yet
    if !st.boot.boot_completed() {
        return b"Error: system not ready (device is booting)".to_vec();
    }
    let Some(p) = st.packages.iter().find(|p| p.package == pkg).cloned() else {
        return format!("Error: Activity not started, unable to resolve Intent — package {pkg} is not installed").into_bytes();
    };
    let Some(app) = p.app else {
        return format!("Error: package {pkg} has no launcher activity").into_bytes();
    };
    st.launch_app(app);
    st.persist_dirty = true;
    format!("Starting: Intent {{ cmp={pkg}/.MainActivity }}").into_bytes()
}

/// `am startservice` — the Termux automation surface. The real Termux app
/// exposes `com.termux.RUN_COMMAND` exactly like this; here it executes the
/// command in the phone's Termux session (scrollback visible on screen) and
/// returns the output over the exec channel.
fn am_startservice(st: &mut OsState, t: &[String]) -> Vec<u8> {
    // -n com.termux/.app.RunCommandService  -a com.termux.RUN_COMMAND
    // --es com.termux.RUN_COMMAND_PATH ... --es com.termux.RUN_COMMAND_ARGUMENTS ...
    let mut component = None;
    let mut action = None;
    let mut path = None;
    let mut args: Vec<String> = Vec::new();
    let mut i = 2;
    while i < t.len() {
        match t[i].as_str() {
            // -n/-a take one argument; --es takes two
            "-n" => {
                component = t.get(i + 1).cloned();
                i += 2;
            }
            "-a" => {
                action = t.get(i + 1).cloned();
                i += 2;
            }
            "--es" => {
                let key = t.get(i + 1).cloned().unwrap_or_default();
                let val = t.get(i + 2).cloned().unwrap_or_default();
                match key.as_str() {
                    "com.termux.RUN_COMMAND_PATH" => path = Some(val),
                    "com.termux.RUN_COMMAND_ARGUMENTS" => args.push(val),
                    _ => {}
                }
                i += 3;
            }
            _ => i += 1,
        }
    }
    let Some(component) = component else {
        return b"Error: am startservice requires -n component".to_vec();
    };
    if !component.starts_with("com.termux") {
        return format!("Error: service {} is not exported", component).into_bytes();
    }
    let Some(action) = action else {
        return b"Error: am startservice requires -a action".to_vec();
    };
    if action != "com.termux.RUN_COMMAND" {
        return format!("Error: unknown action {action}").into_bytes();
    }
    let Some(path) = path else {
        return b"Error: RUN_COMMAND needs com.termux.RUN_COMMAND_PATH".to_vec();
    };
    if path != format!("{TERMUX_PREFIX}/bin/sh") && !path.ends_with("/bin/sh") {
        return format!("Error: RUN_COMMAND_PATH {path} is not executable here").into_bytes();
    }
    // execute inside the Termux session (cwd = $HOME unless changed)
    let Some(home) = resolve_path(st, TERMUX_HOME) else {
        return b"Error: unsafe path".to_vec();
    };
    st.shell_cwd = Some(st.app.termux.cwd.clone().unwrap_or(home));
    let script = args.join(" ");
    let out = if script.is_empty() {
        crate::apps::termux_run_command(st, "sh -c ''", false)
    } else {
        crate::apps::termux_run_command(st, &script, false)
    };
    st.shell_cwd = None;
    let mut resp = format!("Starting service: {component}\n").into_bytes();
    resp.extend(out.into_bytes());
    resp
}

// ── dumpsys / svc / settings / sms ───────────────────────────────────────────

fn dumpsys(st: &mut OsState, t: &[String]) -> Vec<u8> {
    let what = t.get(1).map(String::as_str).unwrap_or("");
    match what {
        "battery" => format!(
            "Current Battery Service state:\n  AC powered: {}\n  USB powered: {}\n  Wireless powered: false\n  status: {}\n  health: 2\n  present: true\n  level: {}\n  scale: 100\n  voltage: 4200\n  temperature: 302\n  technology: Li-poly\n",
            st.charging, st.charging,
            if st.charging { 2 } else { 3 },
            st.battery,
        )
        .into_bytes(),
        "window" => {
            let (w, h) = st.effective_size();
            format!(
                "WINDOW MANAGER DISPLAY CONTENTS (dumpsys window)\n  mDisplayId=0\n  mDisplayWidth={w}\n  mDisplayHeight={h}\n  mScreenOn={}\n",
                !st.screen_off
            )
            .into_bytes()
        }
        "package" => pm(st, &["pm".to_string(), "list".to_string()]),
        "iphonesubinfo" | "isub" => iphonesubinfo_dump(st),
        "telephony.registry" => telephony_registry_dump(st),
        "telephony" | "phone" => phone_dump(st),
        "wifi" => {
            let (rssi, bars) = st.wifi;
            let ssid = st.settings.get("wifi_ssid").cloned().unwrap_or_else(|| mandroid::identity::WIFI_SSID.into());
            format!(
                "Wi-Fi is {}\n  mWifiInfo SSID: {ssid}\n  mWifiState=ENABLED\n  RSSI: {rssi}\n  Link speed: 540Mbps\n  Wi-Fi standard: 6\n  Signal strength: {bars}/4\n",
                if st.settings.get("wifi").map(String::as_str) == Some("on") { "enabled" } else { "disabled" },
            )
            .into_bytes()
        }
        "" => b"usage: dumpsys [battery|window|package|telephony.registry|iphonesubinfo|wifi]".to_vec(),
        other => format!("Unknown service: {other}").into_bytes(),
    }
}

/// `dumpsys telephony.registry` — the per-phone telephony state a real
/// device exposes, driven by the live radio model.
fn telephony_registry_dump(st: &mut OsState) -> Vec<u8> {
    let mut out = String::from("telephony.registry:\n");
    for slot in 1..=2u32 {
        let r = st.radio_now(slot);
        let phone = slot - 1;
        out.push_str(&format!("Phone id={phone}:\n"));
        out.push_str(&format!("  mCallState=0\n"));
        out.push_str(&format!("  mCallIncomingNumber=\n"));
        let reg_state = match r.service_state {
            "IN_SERVICE" => 0,
            "EMERGENCY_ONLY" => 1,
            "OUT_OF_SERVICE" => 2,
            _ => 3, // POWER_OFF
        };
        let net_type = if r.technology == "NR" {
            "NR"
        } else if r.registered {
            "LTE"
        } else {
            "UNKNOWN"
        };
        out.push_str(&format!(
            "  mServiceState={} {} {} {} {} {} {} {} CSS not supported -3 -3 RoamInd=-3 DefRoamInd=-3 EmergOnly={} CurrentState=0 4g 4g ...\n",
            reg_state,
            reg_state,
            if r.registered { if r.roaming { "roaming" } else { "home" } } else { "off" },
            r.operator_alpha,
            r.operator_alpha,
            r.operator_numeric,
            net_type,
            net_type,
            !r.registered,
        ));
        let level_gsm = if r.bars > 0 {
            (r.rsrp_asu as u32).min(31)
        } else {
            99
        };
        out.push_str(&format!(
            "  mSignalStrength=SignalStrength:{{ mCdmaDbm=2147483647 mCdmaEcio=2147483647 mEvdoDbm=2147483647 mEvdoEcio=2147483647 mEvdoSnr=2147483647 mGsmSignalStrength={level_gsm} mGsmBitErrorRate=0 mTdscdmaRscp=2147483647 mLteRsrp={} mLteRssnr=17 mLteCqi=2147483647 mTimingAdvance=2147483647 mLevel={} mIsGsm=true }}\n",
            if r.registered { r.rsrp } else { 2147483647 },
            r.bars,
        ));
        out.push_str(&format!(
            "  mMessageWaiting=false\n  mCallForwarding=false\n"
        ));
        out.push_str(&format!("  mDataActivity=1\n"));
        out.push_str(&format!(
            "  mDataConnectionState={}\n  mDataConnectionPossible={}\n  mDataConnectionReason=dataAttached\n  mDataApnType=default\n  mDataState={}\n",
            if r.data_state == "CONNECTED" { 2 } else { 0 },
            r.data_state == "CONNECTED",
            r.data_state,
        ));
        out.push_str(&format!(
            "  mDataConnectionNetworkType={}\n",
            if r.technology == "NR" { "20" } else { "13" }
        ));
        out.push_str(&format!(
            "  mCellLocation=Bundle[{{ci={}, tac={}, pci={}}}]\n",
            r.ci, r.tac, r.pci
        ));
        out.push_str(&format!("  mIsUserSubActive=true\n"));
    }
    out.push_str(&format!("  mActiveDataSubId={}\n", st.esim.data_slot - 1));
    out.into_bytes()
}

/// `dumpsys isub` / `dumpsys iphonesubinfo` — subscription + device identity.
fn iphonesubinfo_dump(st: &mut OsState) -> Vec<u8> {
    let id = &st.identity;
    let mut out = String::from("Subscription controller:\n");
    for p in st.esim.profiles.iter().filter(|p| p.enabled) {
        out.push_str(&format!(
            " SubscriptionInfo: {{id={}, iccId={}, simSlotIndex={}, carrierId={}, displayName={}, carrierName={}, nameSource=3, iconTint=-16743488, number={}, dataRoaming=0, mcc={}, mnc={}, countryIso={}, isEmbedded=1, cardString={}, accessRules=null}}\n",
            p.slot,
            p.iccid,
            p.slot - 1,
            p.mcc * 10 + p.mnc.parse::<u32>().unwrap_or(0),
            p.nickname,
            p.carrier,
            p.msisdn,
            p.mcc,
            p.mnc.parse::<u32>().unwrap_or(0),
            p.country.to_lowercase(),
            p.iccid,
        ));
    }
    out.push_str(&format!("\nPhone Subscriber Info\n  Phone Type = GSM\n  Device ID = {}\n  Device ID 2 = {}\n  Serial Number = {}\n  eUICC EID = {}\n  Baseband Version = {}\n", id.imei, id.imei2, id.serial, id.eid, id.baseband));
    out.into_bytes()
}

/// `dumpsys phone` — operator summary per slot.
fn phone_dump(st: &mut OsState) -> Vec<u8> {
    let mut out = String::from(
        "PhoneInterfaceManager:
  mPhones:\n",
    );
    for slot in 1..=2u32 {
        let r = st.radio_now(slot);
        let reg_state = if r.registered {
            0
        } else if r.service_state == "POWER_OFF" {
            3
        } else {
            2
        };
        out.push_str(&format!(
            "  Phone[{}] serviceState={} operator={}({}) networkType={} signalLevel={} roaming={}\n",
            slot - 1,
            reg_state,
            r.operator_alpha,
            r.operator_numeric,
            r.technology,
            r.bars,
            r.roaming,
        ));
    }
    out.into_bytes()
}

// ── the eSIM manager (the device's LPA utility) ────────────────────────────

/// `esim` — the handset's eSIM manager, the same surface the agent tools use:
/// list, status, info, enable, disable, switch, register, delete, nickname,
/// data, eid. Machine-readable with --json.
fn esim_cmd(st: &mut OsState, t: &[String]) -> Vec<u8> {
    let sub = t.get(1).cloned().unwrap_or_default();
    let json_mode = t.iter().any(|a| a == "--json");
    let slot_flag = |i: &mut usize, default: u32| -> u32 {
        let mut s = default;
        while *i + 1 < t.len() {
            if t[*i] == "--slot" {
                s = t[*i + 1].parse().unwrap_or(s);
                *i += 2;
            } else {
                *i += 1;
            }
        }
        s
    };
    match sub.as_str() {
        "list" => {
            if json_mode {
                let id = &st.identity;
                let v = serde_json::json!({
                    "eid": id.eid,
                    "slot_count": 2,
                    "profiles_registered": st.esim.profiles.len(),
                    "data_slot": st.esim.data_slot,
                    "profiles": st.esim.profiles.iter().map(|p| p.to_json()).collect::<Vec<_>>(),
                });
                return serde_json::to_string_pretty(&v).unwrap_or_default().into_bytes();
            }
            let mut out = format!(
                "eSIM manager: {} profile(s) on 2 slot(s), EID {}\n",
                st.esim.profiles.len(),
                st.identity.eid
            );
            for p in &st.esim.profiles {
                out.push_str(&format!(
                    "slot {}: {:<8} {}  {:<18} {} ({}, {}) {}\n",
                    p.slot,
                    if p.enabled { "enabled" } else { "disabled" },
                    p.iccid,
                    p.carrier,
                    p.network,
                    p.country,
                    p.state,
                    if st.esim.data_slot == p.slot && p.enabled { "[data]" } else { "" },
                ));
            }
            out.into_bytes()
        }
        "status" => {
            if json_mode {
                let v = telephony_status_json(st);
                return serde_json::to_string_pretty(&v).unwrap_or_default().into_bytes();
            }
            let mut out = String::new();
            out.push_str(&format!("airplane_mode: {}\n", on_off(st.airplane_mode)));
            out.push_str(&format!("mobile_data: {}\n", on_off(st.mobile_data)));
            out.push_str(&format!("default_data_slot: {}\n", st.esim.data_slot));
            let (wrssi, wbars) = st.wifi;
            out.push_str(&format!(
                "wifi: ssid={} rssi={wrssi}dBm bars={wbars}\n",
                st.settings.get("wifi_ssid").cloned().unwrap_or_else(|| mandroid::identity::WIFI_SSID.into()),
            ));
            for s in 1..=2u32 {
                let r = st.radio_now(s);
                out.push_str(&format!(
                    "slot {}: {} operator={} ({}) technology={} band={} rsrp={}dBm asu={} bars={}/4 roaming={} data={} cell(ci={},tac={},pci={},earfcn={})\n",
                    s,
                    r.service_state,
                    r.operator_alpha,
                    r.operator_numeric,
                    r.technology,
                    r.band,
                    r.rsrp,
                    r.rsrp_asu,
                    r.bars,
                    r.roaming,
                    r.data_state,
                    r.ci,
                    r.tac,
                    r.pci,
                    r.earfcn,
                ));
            }
            out.into_bytes()
        }
        "info" => {
            // esim info [--slot N] or esim info <iccid|profile-id>
            let key = t.get(2).map(String::as_str).unwrap_or("");
            let slot = t.iter().position(|a| a == "--slot").map(|i| t[i + 1].parse::<u32>().unwrap_or(1));
            let Some(p) = st.esim.profiles.iter().find(|p| {
                (slot == Some(p.slot)) || (!key.is_empty() && (p.iccid == key || p.profile_id == key))
            }) else {
                return format!("esim: no profile for {}{}", if key.is_empty() { "--slot" } else { key }, String::new()).into_bytes();
            };
            if json_mode {
                let v = serde_json::json!({
                    "eid": st.identity.eid,
                    "profile": p.to_json(),
                });
                return serde_json::to_string_pretty(&v).unwrap_or_default().into_bytes();
            }
            let mut out = format!(
                "eSIM slot {} (port {}) — {}\n  ICCID: {}\n  IMSI: {}\n  MSISDN: {}\n  Carrier: {} ({}, {})\n  PLMN: {}\n  Network: {}\n  State: {}\n  Profile class: {}\n  Activation code: {}\n  Nickname: {}\n  Registered: {}\n  Chip EID: {}\n  Chip: eUICC SGP.22, 8 profile capacity\n",
                p.slot,
                p.port_index,
                if p.enabled { "enabled" } else { "disabled" },
                p.iccid,
                p.imsi,
                p.msisdn,
                p.carrier,
                p.country,
                p.country_name,
                p.plmn(),
                p.network,
                p.state,
                p.profile_class,
                p.activation,
                p.nickname,
                p.registered_at,
                st.identity.eid,
            );
            let r = st.radio_now(p.slot);
            out.push_str(&format!(
                "  Signal: {} {} {} dBm ({}/4 bars)\n",
                r.operator_alpha,
                r.technology,
                r.rsrp,
                r.bars,
            ));
            out.into_bytes()
        }
        "enable" | "switch" => {
            let key = t.get(2).cloned().unwrap_or_default();
            let mut i = 3;
            let slot = slot_flag(&mut i, 1);
            if key.is_empty() {
                return b"usage: esim enable <iccid|profile-id> [--slot N]".to_vec();
            }
            let now = now_ms();
            match st.esim.enable(&key, slot, now) {
                Ok(p) => {
                    st.persist_dirty = true;
                    st.dirty = true;
                    format!("enabled {} ({}) on slot {}\n", p.iccid, p.carrier, slot).into_bytes()
                }
                Err(e) => format!("esim: {e}\n").into_bytes(),
            }
        }
        "disable" => {
            let mut i = 2;
            let slot = slot_flag(&mut i, 1);
            match st.esim.disable(slot) {
                Ok(()) => {
                    st.persist_dirty = true;
                    st.dirty = true;
                    format!("disabled slot {slot}\n").into_bytes()
                }
                Err(e) => format!("esim: {e}\n").into_bytes(),
            }
        }
        "register" => {
            let key = t.get(2).cloned().unwrap_or_default();
            let mut i = 3;
            let slot = slot_flag(&mut i, 1);
            if key.is_empty() {
                return b"usage: esim register <profile-id> [--slot N]  (profile ids: esim catalog / android_sim action=list)".to_vec();
            }
            let now = now_ms();
            match st.esim.register(&key, slot, now) {
                Ok(p) => {
                    st.persist_dirty = true;
                    st.dirty = true;
                    format!(
                        "registered {} ({}) on slot {}\nICCID {}\nMSISDN {}\nactivation {}\n",
                        p.profile_id, p.carrier, slot, p.iccid, p.msisdn, p.activation
                    )
                    .into_bytes()
                }
                Err(e) => format!("esim: {e}\n").into_bytes(),
            }
        }
        "delete" => {
            let key = t.get(2).cloned().unwrap_or_default();
            match st.esim.delete(&key) {
                Ok(p) => {
                    st.persist_dirty = true;
                    st.dirty = true;
                    format!("deleted profile {} ({})\n", p.iccid, p.profile_id).into_bytes()
                }
                Err(e) => format!("esim: {e}\n").into_bytes(),
            }
        }
        "nickname" => {
            let key = t.get(2).cloned().unwrap_or_default();
            let name = t.get(3..).map(|v| v.join(" ")).unwrap_or_default();
            if key.is_empty() || name.is_empty() {
                return b"usage: esim nickname <iccid|profile-id> <name>".to_vec();
            }
            match st.esim.nickname(&key, &name) {
                Ok(()) => {
                    st.persist_dirty = true;
                    st.dirty = true;
                    format!("nickname set: {} -> {name}\n", key).into_bytes()
                }
                Err(e) => format!("esim: {e}\n").into_bytes(),
            }
        }
        "data" => {
            let mut i = 2;
            let slot = slot_flag(&mut i, st.esim.data_slot);
            match st.esim.set_data_slot(slot) {
                Ok(()) => {
                    st.persist_dirty = true;
                    st.dirty = true;
                    format!("default data slot -> {slot}\n").into_bytes()
                }
                Err(e) => format!("esim: {e}\n").into_bytes(),
            }
        }
        "eid" => format!("{}\n", st.identity.eid).into_bytes(),
        "" | "help" => b"usage: esim <list|status|info|enable|disable|switch|register|delete|nickname|data|eid> [--json] [--slot N]".to_vec(),
        other => format!("esim: unknown subcommand {other}\n").into_bytes(),
    }
}

fn on_off(v: bool) -> &'static str {
    if v {
        "on"
    } else {
        "off"
    }
}

/// The live telephony status the agent tools consume (esim status --json).
fn telephony_status_json(st: &OsState) -> serde_json::Value {
    let (wrssi, wbars) = st.wifi;
    let slots: Vec<serde_json::Value> = (1..=2u32)
        .map(|s| {
            let r = st.radio_now(s);
            let profile = st.esim.enabled_on(s);
            serde_json::json!({
                "slot": s,
                "profile": profile.map(|p| p.to_json()),
                "registered": r.registered,
                "service_state": r.service_state,
                "roaming": r.roaming,
                "operator": r.operator_alpha,
                "operator_numeric": r.operator_numeric,
                "technology": r.technology,
                "network": r.network_display,
                "band": r.band,
                "rsrp_dbm": r.rsrp,
                "asu": r.rsrp_asu,
                "bars": r.bars,
                "data_state": r.data_state,
                "cell": { "ci": r.ci, "tac": r.tac, "pci": r.pci, "earfcn": r.earfcn },
            })
        })
        .collect();
    let id = &st.identity;
    serde_json::json!({
        "model": id.model,
        "serial": id.serial,
        "imei": id.imei,
        "imei2": id.imei2,
        "eid": id.eid,
        "airplane_mode": st.airplane_mode,
        "mobile_data": st.mobile_data,
        "data_slot": st.esim.data_slot,
        "wifi": {
            "on": st.settings.get("wifi").map(String::as_str) == Some("on"),
            "ssid": st.settings.get("wifi_ssid").cloned().unwrap_or_else(|| mandroid::identity::WIFI_SSID.into()),
            "rssi_dbm": wrssi,
            "bars": wbars,
        },
        "slots": slots,
    })
}

/// `service call iphonesubinfo <code>` — the binder surface some stacks use
/// to read device identifiers. The reply is a parcel whose ASCII column
/// spells the identifier in UTF-16, exactly like a real handset.
fn service_cmd(st: &mut OsState, t: &[String]) -> Vec<u8> {
    // t = ["service", "call", <service>, <code>, (i32 <slot>)]
    if t.len() < 4 || t[1] != "call" {
        return b"usage: service call <service> <code>".to_vec();
    }
    let service = t[2].as_str();
    let Ok(code) = t[3].parse::<u32>() else {
        return format!("{}: no such transaction", service).into_bytes();
    };
    // an optional `i32 <slot>` tail selects the modem slot for the identifier
    let slot = t
        .iter()
        .position(|a| a == "i32")
        .and_then(|i| t.get(i + 1))
        .and_then(|v| v.parse::<u32>().ok())
        .unwrap_or(1)
        .clamp(1, 2);
    if service != "iphonesubinfo" && service != "isub" {
        return format!("{service}: Service does not exist\n").into_bytes();
    }
    let id = &st.identity;
    let value = match code {
        // getDeviceId / getImeiForSlot family — slot 2 returns imei2
        1 | 3 | 5 | 7 | 11 | 15 | 17 | 20 | 25 | 30 if slot == 2 => id.imei2.clone(),
        1 | 3 | 5 | 7 | 11 | 15 | 17 | 20 | 25 | 30 => id.imei.clone(),
        // getDeviceSvn
        4 => "10".to_string(),
        // getIccSerialNumber family — the slot's ICCID
        2 | 6 | 8 | 13 => st
            .esim
            .enabled_on(slot)
            .map(|p| p.iccid.clone())
            .unwrap_or_default(),
        _ => return format!("{service}: no such transaction {code}\n").into_bytes(),
    };
    let mut out = String::from("Result: Parcel(\n");
    out.push_str(&parcel_dump(&value));
    out.push_str(")\n");
    out.into_bytes()
}

/// A binder parcel dump for a UTF-16 string reply: 4-byte length word, then
/// the characters as 32-bit words, hex columns with the ASCII gutter — the
/// format real `service call` output has.
fn parcel_dump(s: &str) -> String {
    let len = s.chars().count() as u32;
    let mut words: Vec<u32> = vec![len];
    for ch in s.chars() {
        words.push((ch as u32) << 16); // UTF-16 char in the high half
        words.push(0); // (low half padding, like the real layout)
    }
    words.push(0);
    let mut out = String::new();
    for chunk in words.chunks(4) {
        let mut hex_part = String::new();
        let mut ascii_part = String::new();
        for w in chunk {
            let b = w.to_le_bytes();
            hex_part.push_str(&format!(
                "{:02x}{:02x}{:02x}{:02x} ",
                b[3], b[2], b[1], b[0]
            ));
            for byte in b {
                let c = byte as char;
                ascii_part.push(if c.is_ascii_graphic() { c } else { '.' });
            }
        }
        out.push_str(&format!("  {hex_part}'{ascii_part}'\n"));
    }
    out
}

fn uname(st: &mut OsState, t: &[String]) -> Vec<u8> {
    let flag = t.get(1).map(String::as_str).unwrap_or("");
    let full = format!(
        "Linux localhost {} #1 SMP PREEMPT Thu Sep  5 14:39:42 UTC 2024 {} Android\n",
        st.identity.kernel, st.cpu_arch
    );
    match flag {
        "-m" => format!("{}\n", st.cpu_arch).into_bytes(),
        "-r" => format!("{}\n", st.identity.kernel).into_bytes(),
        "-a" => full.into_bytes(),
        "" => b"Linux\n".to_vec(),
        _ => full.into_bytes(),
    }
}

fn svc(st: &mut OsState, t: &[String]) -> Vec<u8> {
    if t.len() < 3 {
        return b"usage: svc [wifi|data] [enable|disable]".to_vec();
    }
    match (t[1].as_str(), t[2].as_str()) {
        ("wifi", "enable") => {
            st.settings.insert("wifi".into(), "on".into());
            st.persist_dirty = true;
            st.dirty = true;
            Vec::new()
        }
        ("wifi", "disable") => {
            st.settings.insert("wifi".into(), "off".into());
            st.persist_dirty = true;
            st.dirty = true;
            Vec::new()
        }
        ("data", "enable") => {
            st.mobile_data = true;
            st.persist_dirty = true;
            st.dirty = true;
            Vec::new()
        }
        ("data", "disable") => {
            st.mobile_data = false;
            st.persist_dirty = true;
            st.dirty = true;
            Vec::new()
        }
        (a, b) => format!("Error: unsupported service {a} {b}").into_bytes(),
    }
}

fn settings_cmd(st: &mut OsState, t: &[String]) -> Vec<u8> {
    // settings get|put NAMESPACE KEY [VALUE]
    if t.len() < 4 {
        return b"usage: settings [get|put] NAMESPACE KEY [VALUE]".to_vec();
    }
    let op = t[1].as_str();
    let full_key = format!("settings.{}.{}", t[2], t[3]);
    match op {
        "get" => {
            // airplane mode is live state, mirrored for the settings provider
            if full_key == "settings.global.airplane_mode_on" {
                return if st.airplane_mode {
                    b"1".to_vec()
                } else {
                    b"0".to_vec()
                };
            }
            st.settings
                .get(&full_key)
                .cloned()
                .unwrap_or_default()
                .into_bytes()
        }
        "put" => {
            let val = t.get(4).cloned().unwrap_or_default();
            if full_key == "settings.global.airplane_mode_on" {
                let on = val == "1";
                if st.airplane_mode != on {
                    st.airplane_mode = on;
                    st.dirty = true;
                    st.persist_dirty = true;
                }
                return Vec::new();
            }
            st.settings.insert(full_key, val);
            st.persist_dirty = true;
            Vec::new()
        }
        other => format!("Error: unknown settings op {other}").into_bytes(),
    }
}

/// `sms send <origin> <text…>` — the telephony console's incoming-SMS path.
fn sms_cmd(st: &mut OsState, t: &[String]) -> Vec<u8> {
    if t.len() < 4 || t[1] != "send" {
        return b"usage: sms send <origin> <text>".to_vec();
    }
    let origin = t[2].clone();
    let text = t[3..].join(" ");
    apps::push_sms(
        st,
        SmsMessage {
            from: origin,
            text,
            at: crate::state::now_ms(),
            mine: false,
        },
    );
    st.notify("Messages", "New message received", Some(AppId::Messages));
    st.persist_dirty = true;
    Vec::new()
}

// ── filesystem over the sdcard ───────────────────────────────────────────────

/// The Termux environment on the device.
pub const TERMUX_PREFIX: &str = "/data/data/com.termux/files/usr";
pub const TERMUX_HOME: &str = "/data/data/com.termux/files/home";

/// Map device paths to the real host directories.
pub fn resolve_path(st: &OsState, p: &str) -> Option<PathBuf> {
    let p = expand_tilde(st, p.trim());
    let data = std::fs::canonicalize(&st.data_dir).ok()?;
    let parse_tail = |tail: &str| -> Option<PathBuf> {
        let mut rel = PathBuf::new();
        for c in Path::new(tail).components() {
            match c {
                Component::Normal(v) => rel.push(v),
                Component::ParentDir
                | Component::CurDir
                | Component::Prefix(_)
                | Component::RootDir => return None,
            }
        }
        Some(rel)
    };
    let under = |base: &str, tail: &str| -> Option<PathBuf> {
        let mut rel = PathBuf::from(base);
        let suffix = parse_tail(tail)?;
        if !suffix.as_os_str().is_empty() {
            rel.push(suffix);
        }
        persist::confined_path(&data, &rel).ok()
    };

    // Match virtual mounts at component boundaries and reject traversal.
    for (device, host) in [
        ("/data/data/com.termux/files/home/storage/shared", "sdcard"),
        (TERMUX_HOME, "termux/home"),
        (TERMUX_PREFIX, "termux/usr"),
        ("/sdcard", "sdcard"),
        ("/data/local/tmp", "cache"),
    ] {
        if p == device {
            return under(host, "");
        }
        if let Some(tail) = p.strip_prefix(device).and_then(|s| s.strip_prefix('/')) {
            return under(host, tail);
        }
    }
    if p.starts_with('/') {
        if let Some(host) = crate::os::host_path(&data, &p) {
            return Some(host);
        }
        return under("cache", p.strip_prefix('/')?);
    }
    let fallback_cwd = persist::sdcard_dir(&data);
    let cwd = st
        .shell_cwd
        .as_ref()
        .or_else(|| {
            if st.app.files.cwd.as_os_str().is_empty() {
                None
            } else {
                Some(&st.app.files.cwd)
            }
        })
        .unwrap_or(&fallback_cwd);
    let absolute_cwd = if cwd.is_absolute() {
        cwd.to_path_buf()
    } else {
        std::env::current_dir().ok()?.join(cwd)
    };
    let cwd_rel = absolute_cwd.strip_prefix(&data).ok()?;
    let checked_cwd = persist::confined_path(&data, cwd_rel).ok()?;
    let cwd_rel = checked_cwd.strip_prefix(&data).ok()?;
    let mut rel = cwd_rel.to_path_buf();
    rel.push(parse_tail(&p)?);
    persist::confined_path(&data, &rel).ok()
}

/// Expand a leading `~` to the Termux home (the shell's $HOME).
fn expand_tilde(st: &OsState, p: &str) -> String {
    if p == "~" {
        return TERMUX_HOME.to_string();
    }
    if let Some(rest) = p.strip_prefix("~/") {
        if st.shell_cwd.is_some() {
            return format!("{TERMUX_HOME}/{rest}");
        }
    }
    p.to_string()
}

fn fs_cmd(st: &mut OsState, t: &[String]) -> Vec<u8> {
    // a token like -x/-la/-rf is a flag, never a path, whatever its length;
    // after `--` every remaining argument is a path
    let mut paths: Vec<String> = Vec::new();
    let mut opts_end = false;
    for a in t[1..].iter() {
        if !opts_end && a == "--" {
            opts_end = true;
        } else if !opts_end
            && a.starts_with('-')
            && a.len() > 1
            && a[1..].chars().all(|c| c.is_ascii_alphanumeric())
        {
            // clustered short flags stay flags for path-detection purposes
        } else {
            paths.push(a.clone());
        }
    }
    // writes on a read-only mount refuse like a real ro filesystem (even as
    // root — system partitions are dm-verity protected until remounted)
    let ro_refused = |p: &str| {
        if crate::os::readonly_path(p) {
            Some(format!("{}: Read-only file system", t[0]))
        } else {
            None
        }
    };
    match t[0].as_str() {
        "ls" => {
            let dir = match paths.last() {
                Some(p) => match resolve_path(st, p) {
                    Some(p) => p,
                    None => return b"ls: unsafe path".to_vec(),
                },
                None => match resolve_path(st, "/sdcard") {
                    Some(p) => p,
                    None => return b"ls: unsafe path".to_vec(),
                },
            };
            match std::fs::read_dir(&dir) {
                Ok(rd) => {
                    let mut names: Vec<String> = rd
                        .flatten()
                        .filter_map(|e| e.file_name().into_string().ok())
                        .filter(|n| !n.starts_with('.'))
                        .map(|n| {
                            let full = dir.join(&n);
                            if std::fs::symlink_metadata(&full)
                                .map(|m| m.is_dir() && !m.file_type().is_symlink())
                                .unwrap_or(false)
                            {
                                format!("{n}/")
                            } else {
                                n
                            }
                        })
                        .collect();
                    names.sort();
                    if names.is_empty() {
                        Vec::new()
                    } else {
                        names.join("\n").into_bytes()
                    }
                }
                Err(_) => format!("ls: {}: No such file or directory", dir.display()).into_bytes(),
            }
        }
        "cat" => {
            let Some(p) = paths.last() else {
                return b"usage: cat <file>".to_vec();
            };
            // /proc is synthesized from the same identity the getprop table
            // reports — one source of truth, no second representation.
            match p.as_str() {
                "/proc/meminfo" => return meminfo(st).into_bytes(),
                "/proc/cpuinfo" => return cpuinfo(st).into_bytes(),
                "/proc/version" => {
                    return format!(
                        "Linux version {} (android-build@abfarm-release-rbe-64-7300) \
                         (Android (12584041) clang version 19.0.1) #1 SMP PREEMPT {}\n",
                        st.identity.kernel, st.identity.build_date,
                    )
                    .into_bytes()
                }
                "/proc/uptime" => {
                    let s = st.booted_at.elapsed().unwrap_or_default().as_secs();
                    return format!("{s} {s}\n").into_bytes();
                }
                _ => {}
            }
            let Some(path) = resolve_path(st, p) else {
                return b"cat: unsafe path".to_vec();
            };
            match std::fs::read(path) {
                Ok(d) => d,
                Err(_) => format!("cat: {}: No such file or directory", p).into_bytes(),
            }
        }
        "mkdir" => {
            let Some(p) = paths.last() else {
                return b"usage: mkdir <dir>".to_vec();
            };
            if let Some(msg) = ro_refused(p) {
                return msg.into_bytes();
            }
            let Some(path) = resolve_path(st, p) else {
                return b"mkdir: unsafe path".to_vec();
            };
            match std::fs::create_dir_all(path) {
                Ok(()) => Vec::new(),
                Err(e) => format!("mkdir: {e}").into_bytes(),
            }
        }
        "rmdir" => {
            let Some(p) = paths.last() else {
                return b"usage: rmdir <dir>".to_vec();
            };
            if let Some(msg) = ro_refused(p) {
                return msg.into_bytes();
            }
            let Some(path) = resolve_path(st, p) else {
                return b"rmdir: unsafe path".to_vec();
            };
            match std::fs::remove_dir(path) {
                Ok(()) => Vec::new(),
                Err(e) => format!("rmdir: {e}").into_bytes(),
            }
        }
        "rm" => {
            let Some(p) = paths.last() else {
                return b"usage: rm [-r] <path>".to_vec();
            };
            if let Some(msg) = ro_refused(p) {
                return msg.into_bytes();
            }
            let Some(full) = resolve_path(st, p) else {
                return b"rm: unsafe path".to_vec();
            };
            let recursive = t[1..].iter().any(|a| a == "-r" || a == "-rf");
            let is_dir = std::fs::symlink_metadata(&full)
                .map(|m| m.is_dir())
                .unwrap_or(false);
            let res = if is_dir && recursive {
                std::fs::remove_dir_all(&full)
            } else if full.is_dir() {
                Err(std::io::Error::new(
                    std::io::ErrorKind::IsADirectory,
                    "is a directory",
                ))
            } else {
                std::fs::remove_file(&full)
            };
            match res {
                Ok(()) => Vec::new(),
                Err(e) => format!("rm: {e}").into_bytes(),
            }
        }
        "touch" => {
            let Some(p) = paths.last() else {
                return b"usage: touch <file>".to_vec();
            };
            if let Some(msg) = ro_refused(p) {
                return msg.into_bytes();
            }
            let Some(path) = resolve_path(st, p) else {
                return b"touch: unsafe path".to_vec();
            };
            match std::fs::OpenOptions::new()
                .create(true)
                .append(true)
                .open(path)
            {
                Ok(_) => Vec::new(),
                Err(e) => format!("touch: {e}").into_bytes(),
            }
        }
        "cp" | "mv" => {
            if paths.len() < 2 {
                return format!("usage: {} <src> <dst>", t[0]).into_bytes();
            }
            if let Some(msg) = ro_refused(&paths[1]).or_else(|| {
                if t[0] == "mv" {
                    ro_refused(&paths[0])
                } else {
                    None
                }
            }) {
                return msg.into_bytes();
            }
            let (Some(src), Some(dst)) = (resolve_path(st, &paths[0]), resolve_path(st, &paths[1]))
            else {
                return b"unsafe path".to_vec();
            };
            let res = if t[0] == "cp" {
                std::fs::copy(&src, &dst).map(|_| ())
            } else {
                std::fs::rename(&src, &dst)
            };
            match res {
                Ok(()) => Vec::new(),
                Err(e) => format!("{}: {e}", t[0]).into_bytes(),
            }
        }
        _ => Vec::new(),
    }
}

fn df(st: &mut OsState) -> Vec<u8> {
    // The 1 TB UFS variant: /data spans the full 931 GiB usable volume and
    // /storage/emulated is the FUSE view over the same partition.
    let total_kib = st.rom_gib * 1024 * 1024;
    let sys_used = total_kib * 7 / 100; // system + app data on a fresh handset
    let mut files: u64 = 0;
    let sd = persist::sdcard_dir(&st.data_dir);
    walk_size(&sd, &mut files);
    let sd_used = sys_used + files / 1024;
    format!(
        "Filesystem            1K-blocks       Used   Available Use% Mounted on\n\
         /dev/block/dm-4  {}  {}  {}   {}% /data\n\
         /dev/fuse        {}  {}  {}   {}% /storage/emulated\n",
        total_kib,
        sys_used,
        total_kib - sys_used,
        sys_used * 100 / total_kib.max(1),
        total_kib,
        sd_used,
        total_kib - sd_used,
        sd_used * 100 / total_kib.max(1),
    )
    .into_bytes()
}

/// Memory accounting derived from the device identity (16 GB LPDDR5X on the
/// Pixel 9 Pro profile) — `cat /proc/meminfo`, `free` and the Settings UI all
/// agree on these numbers. Returns (total, free, available, cached) in kB.
fn mem_numbers(st: &OsState) -> (u64, u64, u64, u64) {
    let physical = st.identity.ram_mib * 1024; // 16777216 kB physical
    let total = physical * 9519 / 10000; // real MemTotal after kernel reservations
    let free = total * 31 / 100;
    let cached = total * 26 / 100;
    let available = free + cached - total / 100;
    (total, free, available, cached)
}

/// `/proc/meminfo` — the 16 GB handset's memory map with zram swap.
fn meminfo(st: &OsState) -> String {
    let physical = st.identity.ram_mib * 1024;
    let (t, f, a, c) = mem_numbers(st);
    let b = t * 2 / 100;
    let swap = physical / 2; // zram sizing on a 16 GB device
    let anon_active = t * 18 / 100;
    let file_active = c / 2;
    let anon_inactive = t / 100;
    let file_inactive = c - file_active;
    let active = anon_active + file_active;
    let inactive = anon_inactive + file_inactive;
    let zram = swap / 2;
    format!(
        "MemTotal:       {:>10} kB\n\
         MemFree:        {:>10} kB\n\
         MemAvailable:   {:>10} kB\n\
         Buffers:        {:>10} kB\n\
         Cached:         {:>10} kB\n\
         SwapCached:     {:>10} kB\n\
         Active:         {:>10} kB\n\
         Inactive:       {:>10} kB\n\
         Active(anon):   {:>10} kB\n\
         Inactive(anon): {:>10} kB\n\
         Active(file):   {:>10} kB\n\
         Inactive(file): {:>10} kB\n\
         Unevictable:    {:>10} kB\n\
         Mlocked:        {:>10} kB\n\
         SwapTotal:      {:>10} kB\n\
         SwapFree:       {:>10} kB\n\
         Zswap:          {:>10} kB\n\
         Zram:           {:>10} kB\n\
         VmallocTotal:   {:>10} kB\n\
         VmallocUsed:    {:>10} kB\n\
         VmallocChunk:   {:>10} kB\n",
        t,
        f,
        a,
        b,
        c,
        0,
        active,
        inactive,
        anon_active,
        anon_inactive,
        file_active,
        file_inactive,
        0,
        0,
        swap,
        swap / 2,
        0,
        zram,
        258997272,
        21304,
        0,
    )
}

/// `/proc/cpuinfo` — the Tensor G4 big.LITTLE topology: CPUs 0-3 Cortex-A520,
/// 4-6 Cortex-A720, 7 Cortex-X4 (ARM parts 0xd80 / 0xd81 / 0xd4e).
fn cpuinfo(st: &OsState) -> String {
    let _ = st;
    let parts = [
        (0xd80u32, "a52"),
        (0xd80, "a52"),
        (0xd80, "a52"),
        (0xd80, "a52"),
        (0xd81, "a72"),
        (0xd81, "a72"),
        (0xd81, "a72"),
        (0xd4e, "x4"),
    ];
    let mut out = String::new();
    for (i, (part, _cluster)) in parts.iter().enumerate() {
        out.push_str(&format!(
            "processor       : {i}\n\
             BogoMIPS        : 38.00\n\
             Features        : fp asimd evtstrm aes pmull sha1 sha2 crc32 atomics fphp asimdhp \
             cpuid asimdrdm jscvt fcma lrcpc dcpip sha3 sm3 sm4 asimddp sha512 sve dotprod rcpc2\n\
             CPU implementer : 0x41\n\
             CPU architecture: 8\n\
             CPU variant     : 0x3\n\
             CPU part        : 0x{part:02x}\n\
             CPU revision    : 0\n\n"
        ));
    }
    out.push_str("Hardware        : Google Tensor G4 (zumapro)\n");
    out
}

/// `free` — toybox-style memory report from the same numbers as meminfo.
fn free_cmd(st: &mut OsState) -> Vec<u8> {
    let (t, f, a, c) = mem_numbers(st);
    let used = t - f - c;
    let cache = f + c;
    format!(
        "               total        used        free      shared  buff/cache   available\n\
         Mem:    {:>10} {:>10} {:>10} {:>10} {:>10} {:>10}\n\
         Swap:   {:>10} {:>10} {:>10}\n",
        t,
        used,
        f,
        0,
        cache,
        a,
        st.identity.ram_mib * 512, // swap total (zram)
        st.identity.ram_mib * 256, // swap used
        st.identity.ram_mib * 256, // swap free
    )
    .into_bytes()
}

fn walk_size(dir: &Path, acc: &mut u64) {
    if let Ok(rd) = std::fs::read_dir(dir) {
        for e in rd.flatten() {
            if let Ok(meta) = e.metadata() {
                if meta.is_dir() {
                    walk_size(&e.path(), acc);
                } else {
                    *acc += meta.len();
                }
            }
        }
    }
}

fn logcat(st: &mut OsState) -> Vec<u8> {
    let mut out = String::new();
    // the system log: boot chain, installer, recovery — what logcat shows
    for l in &st.logs {
        out.push_str(l);
        out.push('\n');
    }
    let up = st.booted_at.elapsed().unwrap_or_default().as_secs();
    out.push_str(&format!(
        "I/roma-os( 1): boot completed in {up}s (compositor + adbd online)\n"
    ));
    for n in &st.notifications {
        out.push_str(&format!(
            "I/Notification( {}): {} — {}\n",
            1200, n.title, n.body
        ));
    }
    if st.screen_off {
        out.push_str("D/Surface( 100): screen off (power key)\n");
    }
    out.into_bytes()
}

// ── the OS: install / verify / status / partitions, dmesg, ps, mount, reboot ─

/// `os install | os verify | os status | os partitions` — the OS image is a
/// real on-disk install; these commands drive and prove it over adb.
fn os_cmd(st: &mut OsState, t: &[String]) -> Vec<u8> {
    match t.get(1).map(String::as_str).unwrap_or("status") {
        "status" => {
            let mut out = String::new();
            match crate::os::read_manifest(&st.data_dir) {
                Some(m) => {
                    out.push_str(&format!(
                        "Android {} ({}) — installed\n",
                        m.version, m.build_id
                    ));
                    out.push_str(&format!("fingerprint: {}\n", m.fingerprint));
                    out.push_str(&format!("kernel: {}\n", m.kernel));
                    out.push_str(&format!("files: {} ({} KiB)\n", m.files, m.bytes / 1024));
                    out.push_str(&format!(
                        "root: Magisk {} — su in /system/xbin/su\n",
                        crate::os::magisk_version()
                    ));
                }
                None => out.push_str("no OS image installed (run `os install`)\n"),
            }
            out.push_str(&format!("boot: {:?}\n", st.boot));
            out.into_bytes()
        }
        "install" => {
            // a real flash, synchronously (same installer the boot chain runs)
            let id = st.identity.clone();
            let dir = st.data_dir.clone();
            match crate::os::install(&dir, &id) {
                Ok(m) => {
                    st.log_line(format!(
                        "I/installer( 1): os install over adb: Android {} ({}), {} files",
                        m.version, m.build_id, m.files
                    ));
                    format!(
                        "OS image flashed: Android {} ({})\n{} files, {} KiB, verified\nroot: Magisk {} (su /system/xbin/su)\n",
                        m.version,
                        m.build_id,
                        m.files,
                        m.bytes / 1024,
                        crate::os::magisk_version()
                    )
                    .into_bytes()
                }
                Err(e) => format!("os install failed: {e}").into_bytes(),
            }
        }
        "verify" => match crate::os::verify(&st.data_dir) {
            Ok(report) => report.into_bytes(),
            Err(e) => format!("os verify failed: {e}").into_bytes(),
        },
        "partitions" => {
            let mut out = String::from(
                "name         mount         dev                fs     size MiB  flags\n",
            );
            for p in crate::os::PARTITIONS {
                out.push_str(&format!(
                    "{:<12} {:<13} {:<18} {:<6} {:>9}  {}\n",
                    p.name,
                    p.mount,
                    p.dev,
                    p.fs,
                    p.size_kib / 1024,
                    if p.readonly { "ro" } else { "rw" }
                ));
            }
            out.into_bytes()
        }
        other => format!("os: unknown subcommand {other} (status|install|verify|partitions)")
            .into_bytes(),
    }
}

/// `dmesg` — the kernel ring buffer filled by the boot chain.
fn dmesg(st: &mut OsState) -> Vec<u8> {
    let mut out = Vec::new();
    for l in &st.dmesg {
        out.extend_from_slice(l.as_bytes());
        out.push(b'\n');
    }
    out
}

/// `ps` — the init service roster with deterministic PIDs (the same list
/// init.rc starts and the boot chain logs).
fn ps(st: &mut OsState) -> Vec<u8> {
    let _ = st;
    let mut out = String::from("USER           PID  PPID  VSZ  RSS WCHAN            ADDR S NAME\n");
    let mut push = |user: &str, pid: u32, ppid: u32, rss: u32, s: &str, name: &str| {
        out.push_str(&format!(
            "{user:<12} {pid:>5} {ppid:>5} {:>4}M {:>4}M        0   0 {s} {name}\n",
            200 + pid * 13 % 400,
            rss
        ));
    };
    push("root", 1, 0, 12, "S", "init");
    for (i, (name, _)) in crate::os::boot_services().iter().enumerate() {
        let user = match *name {
            "system_server" | "zygote" => "system",
            _ => "root",
        };
        push(
            user,
            300 + (i as u32) * 7,
            1,
            4 + (i as u32 * 13) % 90,
            "S",
            name,
        );
    }
    out.into_bytes()
}

/// `mount` — the fstab view of the installed image.
fn mount_cmd(st: &mut OsState) -> Vec<u8> {
    let _ = st;
    let mut out = String::new();
    for p in crate::os::PARTITIONS {
        out.push_str(&format!(
            "{} on {} type {} ({})\n",
            p.dev,
            p.mount,
            p.fs,
            if p.readonly {
                "ro,seclabel,relatime"
            } else {
                "rw,seclabel,nosuid,nodev,noatime"
            }
        ));
    }
    out.push_str("tmpfs on /dev type tmpfs (rw,seclabel,nosuid,relatime,mode=755)\n");
    out.push_str("proc on /proc type proc (rw,relatime)\n");
    out.into_bytes()
}

/// `reboot [recovery|bootloader]` — through the real chain.
fn reboot_cmd(st: &mut OsState, t: &[String]) -> Vec<u8> {
    match t.get(1).map(String::as_str) {
        None | Some("-f") => {
            st.inbox.lock().unwrap().push_back(OsEvent::Reboot);
            Vec::new()
        }
        Some("recovery") => {
            st.inbox.lock().unwrap().push_back(OsEvent::RebootRecovery);
            Vec::new()
        }
        Some("bootloader") => {
            // bootloader target: the chain restarts at the bootloader screen
            st.inbox.lock().unwrap().push_back(OsEvent::Reboot);
            Vec::new()
        }
        Some(other) => format!("reboot: invalid target {other}").into_bytes(),
    }
}

fn pkg_cmd(st: &mut OsState, t: &[String]) -> Vec<u8> {
    match t.get(1).map(String::as_str).unwrap_or("") {
        "list-installed" | "list" => {
            // the termux prefix's own command set (real files on the prefix)
            let Some(bin) = resolve_path(st, &format!("{TERMUX_PREFIX}/bin")) else {
                return b"pkg: unsafe path".to_vec();
            };
            let mut names: Vec<String> = std::fs::read_dir(&bin)
                .map(|rd| {
                    rd.flatten()
                        .filter_map(|e| e.file_name().into_string().ok())
                        .collect()
                })
                .unwrap_or_default();
            names.sort();
            names.join("\n").into_bytes()
        }
        "install" | "upgrade" => {
            // honest: no package repository backend is wired up in this build
            b"pkg: no package repository configured on the built-in phone (offline prefix)".to_vec()
        }
        other => format!("pkg: unknown subcommand {other}").into_bytes(),
    }
}

fn termux_info(st: &mut OsState) -> Vec<u8> {
    let home = resolve_path(st, TERMUX_HOME).unwrap_or_default();
    let bin = resolve_path(st, &format!("{TERMUX_PREFIX}/bin")).unwrap_or_default();
    let cmds = std::fs::read_dir(&bin).map(|rd| rd.count()).unwrap_or(0);
    let runs = st.app.termux.run_commands;
    format!(
        "Termux environment (built-in phone)\n  PREFIX: {TERMUX_PREFIX}\n  HOME:   {TERMUX_HOME}\n  prefix commands: {cmds}\n  RUN_COMMAND executions this session: {runs}\n  storage: /sdcard is accessible (termux-setup-storage is pre-configured)\n  data dir: {}\n",
        home.display()
    )
    .into_bytes()
}

fn termux_setup_storage(st: &mut OsState) -> Vec<u8> {
    // bind /sdcard into the termux home like the real utility does
    let Some(home) = resolve_path(st, TERMUX_HOME) else {
        return b"termux-setup-storage: unsafe path".to_vec();
    };
    let Some(sd) = resolve_path(st, "/sdcard") else {
        return b"termux-setup-storage: unsafe path".to_vec();
    };
    let link = home.join("storage");
    if std::fs::symlink_metadata(&link).is_ok() {
        return b"termux-setup-storage: storage path already exists".to_vec();
    }
    match std::os::unix::fs::symlink(sd, &link) {
        Ok(()) => Vec::new(),
        Err(e) => format!("termux-setup-storage: {e}").into_bytes(),
    }
}

/// Expand `$HOME` / `$PREFIX` (device paths, not host ones).
fn expand_env(s: &str, st: &OsState) -> String {
    s.replace("$HOME", TERMUX_HOME)
        .replace("$PREFIX", TERMUX_PREFIX)
        .replace(
            '~',
            if st.shell_cwd.is_some() {
                TERMUX_HOME
            } else {
                "/"
            },
        )
}

/// Find the byte offset of the `))` that closes a `$((` opened at `start`,
/// honouring nested parentheses (`$((1+(2*3)))`). Returns the offset of the
/// first `)` of the closing pair, relative to `rest`.
fn find_arith_close(rest: &str, start: usize) -> Option<usize> {
    let bytes = rest.as_bytes();
    let mut depth = 0usize; // parens opened inside the expression
    let mut i = start + 3;
    while i < bytes.len() {
        match bytes[i] {
            b'(' => depth += 1,
            b')' => {
                if depth == 0 {
                    return if bytes.get(i + 1) == Some(&b')') {
                        Some(i)
                    } else {
                        None
                    };
                }
                depth -= 1;
            }
            _ => {}
        }
        i += 1;
    }
    None
}

/// Expand `$(( arithmetic ))` with the calculator's own evaluator.
fn expand_dollar_paren(s: &str) -> String {
    let mut out = String::new();
    let mut rest = s;
    while let Some(start) = rest.find("$((") {
        out.push_str(&rest[..start]);
        let Some(end) = find_arith_close(rest, start) else {
            out.push_str(&rest[start..]);
            rest = "";
            break;
        };
        let expr = &rest[start + 3..end];
        match crate::apps::eval_expr(expr) {
            Some(v) => {
                if v.fract() == 0.0 && v.abs() < 1e15 {
                    out.push_str(&format!("{}", v as i64));
                } else {
                    out.push_str(&format!("{v}"));
                }
            }
            // Keep the original text so the failure is visible and lossless.
            None => out.push_str(&rest[start..end + 2]),
        }
        rest = &rest[end + 2..];
    }
    out.push_str(rest);
    out
}

fn date_string() -> String {
    let secs = crate::state::now_secs() as i64;
    let days = secs / 86400;
    let (y, m, d) = crate::compose::civil_from_days(days);
    let rem = secs % 86400;
    format!(
        "{}-{:02}-{:02} {:02}:{:02}:{:02} UTC",
        y,
        m,
        d,
        rem / 3600,
        (rem / 60) % 60,
        rem % 60
    )
}

#[cfg(test)]
mod path_security_tests {
    use super::*;
    use std::time::{SystemTime, UNIX_EPOCH};

    fn temp() -> PathBuf {
        let p = std::env::temp_dir().join(format!(
            "phone-shell-security-{}-{}",
            std::process::id(),
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(&p).unwrap();
        let _ = persist::fresh(&p);
        p
    }

    #[test]
    fn shell_paths_reject_traversal_prefix_confusion_and_symlink_escape() {
        let root = temp();
        let st = persist::fresh(&root);
        let outside = root.with_extension("outside");
        std::fs::create_dir_all(&outside).unwrap();
        std::fs::write(outside.join("secret"), b"private").unwrap();
        assert!(resolve_path(&st, "/sdcard/../../outside/secret").is_none());
        assert!(
            resolve_path(&st, "/sdcardX/secret").is_some_and(|p| p.starts_with(root.join("cache")))
        );
        #[cfg(unix)]
        {
            std::os::unix::fs::symlink(&outside, root.join("sdcard/link")).unwrap();
            assert!(resolve_path(&st, "/sdcard/link/secret").is_none());
        }
        let _ = std::fs::remove_dir_all(root);
        let _ = std::fs::remove_dir_all(outside);
    }
}

#[cfg(test)]
mod shell_write_security_tests {
    use super::*;
    use std::time::{SystemTime, UNIX_EPOCH};

    #[test]
    fn shell_touch_does_not_write_outside_data_root() {
        let root = std::env::temp_dir().join(format!(
            "phone-shell-write-{}-{}",
            std::process::id(),
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(&root).unwrap();
        let mut st = persist::fresh(&root);
        let outside = root.with_extension("outside");
        std::fs::create_dir_all(&outside).unwrap();
        let _ = exec(&mut st, "touch /sdcard/../../outside/owned");
        assert!(!outside.join("owned").exists());
        let _ = std::fs::remove_dir_all(root);
        let _ = std::fs::remove_dir_all(outside);
    }
}

#[cfg(test)]
mod arith_expansion_tests {
    use super::expand_dollar_paren;

    #[test]
    fn simple_expression() {
        assert_eq!(expand_dollar_paren("x=$((1+2))"), "x=3");
    }

    #[test]
    fn nested_parentheses_are_not_truncated() {
        assert_eq!(expand_dollar_paren("$((1+(2*3)))"), "7");
        assert_eq!(expand_dollar_paren("a $(((1+2)*3)) b"), "a 9 b");
    }

    #[test]
    fn unterminated_is_kept_verbatim() {
        assert_eq!(expand_dollar_paren("$((1+2"), "$((1+2");
    }
}
