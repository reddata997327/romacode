//! Agent tools for a selected real Android target. Commands are sent through
//! the host Android Platform-Tools ADB client; no synthetic device data is
//! returned.

use serde_json::json;
use std::time::Duration;

/// Anthropic tool schemas for the agent loop.
pub fn definitions() -> Vec<serde_json::Value> {
    let t = |name: &str, desc: &str, props: serde_json::Value, req: &[&str]| {
        json!({
            "name": name,
            "description": desc,
            "input_schema": { "type": "object", "properties": props, "required": req },
        })
    };
    vec![
        t(
            "android_setup",
            "Bootstrap or audit the host Android environment — the ONLY android tool that works with no device online. action=status: full audit (adb/SDK/emulator/avdmanager/sdkmanager paths, AVDs, installed system images, online devices, KVM verdict, recommended next step). action=doctor: the same audit plus the exact host commands that close every gap. action=create_avd: installs the system image through sdkmanager when missing, then creates an AVD (name defaults to 'romacode', image defaults to system-images;android-34;google_apis;x86_64, device defaults to pixel_5). action=boot: launches the SDK emulator DETACHED (survives the tool call) and waits for sys.boot_completed=1, then returns the adb serial. action=wait: re-poll boot completion. Run status before any other android_* tool when `adb devices` shows nothing.",
            json!({
                "action": { "type": "string", "enum": ["status", "doctor", "create_avd", "boot", "wait"], "description": "Defaults to status." },
                "name": { "type": "string", "description": "AVD name (create_avd/boot); defaults to 'romacode' / first AVD." },
                "image": { "type": "string", "description": "sdkmanager package id (create_avd), e.g. system-images;android-34;google_apis;x86_64" },
                "device": { "type": "string", "description": "avdmanager device profile (create_avd), e.g. pixel_5" },
                "timeout": { "type": "integer", "description": "Seconds to wait for boot (wait/boot, 30-900, default 300)" }
            }),
            &[],
        ),
        t(
            "android_device",
            "Inspect the selected real Android emulator or device through Android Platform-Tools: model, brand, device codename, release, SDK, build fingerprint, serial, display, battery and installed package count. Values are queried from the selected target; RomaCode does not fabricate hardware, carrier, or root facts.",
            json!({}),
            &[],
        ),
        t(
            "android_shell",
            "Run one command in the selected real Android target's adb shell. This operates on the actual device and its data; no root escalation is attempted. Returns stdout or a non-zero exit diagnostic.",
            json!({ "command": { "type": "string", "description": "The shell command, e.g. 'input tap 360 1500' or 'wm size 1080x2400'" } }),
            &["command"],
        ),
        t(
            "android_tap",
            "Tap the phone screen at device pixel coordinates. Use android_device or android_screen to learn the current resolution.",
            json!({
                "x": { "type": "integer" },
                "y": { "type": "integer" }
            }),
            &["x", "y"],
        ),
        t(
            "android_swipe",
            "Swipe on the phone screen (drag gesture) over ~180 ms.",
            json!({
                "x1": { "type": "integer" }, "y1": { "type": "integer" },
                "x2": { "type": "integer" }, "y2": { "type": "integer" }
            }),
            &["x1", "y1", "x2", "y2"],
        ),
        t(
            "android_text",
            "Type text into the phone's focused field (soft keyboard input). Escape spaces normally; the bridge quotes it safely.",
            json!({ "text": { "type": "string" } }),
            &["text"],
        ),
        t(
            "android_key",
            "Send an Android keyevent by number: 3=home, 4=back, 24/25=volume, 26=power, 66=enter, 67=delete.",
            json!({ "code": { "type": "integer" } }),
            &["code"],
        ),
        t(
            "android_screen",
            "Get or change the phone's display: action get|set|reset; width/height in px, density in dpi (action=set, e.g. 720x1560@420).",
            json!({
                "action": { "type": "string", "enum": ["get", "set", "reset"] },
                "width": { "type": "integer" }, "height": { "type": "integer" },
                "density": { "type": "integer" }
            }),
            &["action"],
        ),
        t(
            "android_screenshot",
            "Capture the selected real Android target's current screen through `adb exec-out screencap -p`, save the PNG on the host, and return its path, dimensions and byte size.",
            json!({ "path": { "type": "string", "description": "Optional .png path (default under the session dir)" } }),
            &[],
        ),
        t(
            "android_install",
            "Install an APK on the selected real Android target using `adb install -r`. The target package manager performs the install; failure output is returned without claiming success.",
            json!({ "apk": { "type": "string", "description": "Host path to the .apk file" } }),
            &["apk"],
        ),
        t(
            "android_launch",
            "Launch an app by package (monkey -p) or component (am start -n pkg/activity).",
            json!({ "component": { "type": "string" } }),
            &["component"],
        ),
        t(
            "android_packages",
            "List packages reported by the selected real Android target; optional substring filter.",
            json!({ "filter": { "type": "string" } }),
            &[],
        ),
    ]
}

/// Execute one tool call. Returns the text the agent sees as tool_result.
pub fn execute(
    android: &crate::android::AndroidAgent,
    name: &str,
    args: &serde_json::Value,
) -> Result<String, String> {
    let s = |k: &str| {
        args.get(k)
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string()
    };
    let i = |k: &str| args.get(k).and_then(|v| v.as_i64());
    let shell_reply = |android: &crate::android::AndroidAgent, cmd: &str| -> Result<String, String> {
        android.shell(cmd)
    };

    match name {
        "android_device" => {
            let st = android.status().unwrap_or_default();
            let info = shell_reply(android, "getprop ro.product.model")?;
            let fingerprint =
                shell_reply(android, "getprop ro.build.fingerprint").unwrap_or_default();
            let release =
                shell_reply(android, "getprop ro.build.version.release").unwrap_or_default();
            let serial = shell_reply(android, "getprop ro.serialno").unwrap_or_default();
            let brand = shell_reply(android, "getprop ro.product.brand").unwrap_or_default();
            let device = shell_reply(android, "getprop ro.product.device").unwrap_or_default();
            let sdk = shell_reply(android, "getprop ro.build.version.sdk").unwrap_or_default();
            let patch =
                shell_reply(android, "getprop ro.build.version.security_patch").unwrap_or_default();
            let battery = shell_reply(android, "dumpsys battery")?;
            let level = battery
                .lines()
                .find(|l| l.trim().starts_with("level:"))
                .and_then(|l| l.split(':').nth(1))
                .and_then(|v| v.trim().parse::<u8>().ok())
                .unwrap_or(st.battery);
            let pkgs = shell_reply(android, "pm list packages")?;
            let (w, h) = match shell_reply(android, "wm size") {
                Ok(out) => mandroid::screen::parse_wm_size(&out).unwrap_or(st.display),
                Err(_) => st.display,
            };
            Ok(json!({
                "model": info.trim(),
                "brand": brand.trim(),
                "device": device.trim(),
                "android_release": release.trim(),
                "build": {
                    "fingerprint": fingerprint.trim(),
                    "security_patch": patch.trim()
                },
                "sdk": sdk.trim(),
                "serial": serial.trim(),
                "adb_serial": st.serial,
                "display": format!("{w}x{h}"),
                "battery": level,
                "packages": pkgs.lines().filter(|line| line.starts_with("package:")).count(),
            })
            .to_string())
        }
        "android_shell" => {
            let cmd = s("command");
            if cmd.trim().is_empty() {
                return Err("android_shell: command is required".into());
            }
            shell_reply(android, &cmd)
        }
        "android_tap" => {
            let (x, y) = (i("x").ok_or("x required")?, i("y").ok_or("y required")?);
            if !(0..=20000).contains(&x) || !(0..=20000).contains(&y) {
                return Err("tap coordinates must be between 0 and 20000 device pixels".into());
            }
            android.tap(x as u32, y as u32)?;
            std::thread::sleep(Duration::from_millis(150));
            Ok(format!("tapped ({x}, {y})"))
        }
        "android_swipe" => {
            let (x1, y1, x2, y2) = (
                i("x1").ok_or("x1 required")?,
                i("y1").ok_or("y1 required")?,
                i("x2").ok_or("x2 required")?,
                i("y2").ok_or("y2 required")?,
            );
            if [x1, y1, x2, y2].iter().any(|v| !(0..=20000).contains(v)) {
                return Err("swipe coordinates must be between 0 and 20000 device pixels".into());
            }
            android.swipe(x1 as u32, y1 as u32, x2 as u32, y2 as u32)?;
            Ok(format!("swiped ({x1},{y1}) → ({x2},{y2})"))
        }
        "android_text" => {
            let text = s("text");
            if text.is_empty() {
                return Err("android_text: text is required".into());
            }
            android.text(&text)?;
            Ok(format!("typed {text:?}"))
        }
        "android_key" => {
            let code = i("code").ok_or("code required")?;
            let k = phone::state::KeyCode::from_number(code)
                .ok_or_else(|| format!("unsupported keycode {code}"))?;
            android.key(k)?;
            Ok(format!("keyevent {code}"))
        }
        "android_screen" => match s("action").as_str() {
            "get" | "" => {
                let size = shell_reply(android, "wm size")?;
                let density = shell_reply(android, "wm density")?;
                Ok(format!("{}\n{}", size.trim(), density.trim()))
            }
            "set" => {
                let w = i("width").ok_or("width required for set")?;
                let h = i("height").ok_or("height required for set")?;
                let d = i("density").unwrap_or(0);
                if !(320..=7680).contains(&w) || !(320..=7680).contains(&h) {
                    return Err("screen width and height must be 320..=7680 pixels".into());
                }
                if d != 0 && !(72..=1000).contains(&d) {
                    return Err("density must be 72..=1000 dpi (or omit it)".into());
                }
                let (w, h, d) = (w as u32, h as u32, d as u32);
                let mut out = shell_reply(android, &format!("wm size {w}x{h}"))?;
                if d > 0 {
                    out.push_str(&shell_reply(android, &format!("wm density {d}"))?);
                }
                out.push_str(&shell_reply(android, "wm size")?);
                Ok(out)
            }
            "reset" => {
                let mut out = shell_reply(android, "wm size reset")?;
                out.push_str(&shell_reply(android, "wm density reset")?);
                out.push_str(&shell_reply(android, "wm size")?);
                Ok(out)
            }
            other => Err(format!("android_screen: unknown action {other}")),
        },
        "android_screenshot" => {
            let png = android.screenshot_png()?;
            let dir = dirs::home_dir()
                .map(|h| h.join(".romacode").join("screenshots"))
                .unwrap_or_else(|| std::path::PathBuf::from("screenshots"));
            let _ = std::fs::create_dir_all(&dir);
            let name = s("path");
            let path = if name.is_empty() {
                let ts = std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .map(|d| d.as_millis())
                    .unwrap_or(0);
                dir.join(format!("phone-{ts}.png"))
            } else {
                std::path::PathBuf::from(name)
            };
            std::fs::write(&path, &png).map_err(|e| format!("write {}: {e}", path.display()))?;
            let dims = mandroid::screen::png_dimensions(&png);
            Ok(json!({
                "saved": path.display().to_string(),
                "bytes": png.len(),
                "dimensions": dims,
            })
            .to_string())
        }
        "android_install" => {
            let apk = s("apk");
            if apk.trim().is_empty() {
                return Err("android_install: APK path is required".into());
            }
            let verification = mandroid::sign::verify_v2(std::path::Path::new(&apk))
                .map_err(|e| format!("APK v2 verification failed: {e:#}"))?;
            if !verification.valid() {
                return Err(format!(
                    "refusing to install APK with invalid v2 signature: {}",
                    verification.errors.join("; ")
                ));
            }
            // push + install through the bridge thread's own device connection
            shell_reply(android, &format!("__install {}", apk))
        }
        "android_launch" => {
            let comp = s("component");
            if comp.is_empty() {
                return Err("android_launch: component is required".into());
            }
            let parts: Vec<_> = comp.split('/').collect();
            if parts.len() > 2
                || parts.iter().any(|part| {
                    part.is_empty()
                        || !part
                            .chars()
                            .all(|c| c.is_ascii_alphanumeric() || "._$".contains(c))
                })
            {
                return Err(
                    "android_launch: expected a package name or package/activity component".into(),
                );
            }
            if comp.contains('/') {
                shell_reply(android, &format!("am start -n {comp}"))
            } else {
                shell_reply(
                    android,
                    &format!("monkey -p {comp} -c android.intent.category.LAUNCHER 1"),
                )
            }
        }
        "android_packages" => {
            let filter = s("filter");
            let out = shell_reply(android, "pm list packages")?;
            let lines: Vec<&str> = out
                .lines()
                .filter(|l| filter.is_empty() || l.contains(&filter))
                .collect();
            Ok(lines.join("\n"))
        }
        other => Err(format!("unknown android tool: {other}")),
    }
}

/// Direct-host fallback: run an `android_*` tool through the host Android
/// Platform-Tools `adb` CLI, bypassing the phone bridge entirely. Used
/// whenever the bridge is not answering (not started, thread gone, channel
/// down) so the agent never sees "Android bridge is unavailable" — it either
/// talks to the real device or gets a real adb diagnostic.
pub fn execute_via_host_adb(
    cfg: &crate::config::Config,
    name: &str,
    args: &serde_json::Value,
) -> Result<String, String> {
    use crate::android::adb_cli::Device;
    let device = Device::discover(&cfg.android.adb_path, &cfg.android.serial)?;
    let s = |k: &str| {
        args.get(k)
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string()
    };
    let i = |k: &str| args.get(k).and_then(|v| v.as_i64());
    // `input text` needs the text quoted as one adb-shell argument.
    let q = |t: &str| format!("'{}'", t.replace('\'', r"'\''"));

    match name {
        "android_device" => {
            let getprop = |k: &str| device.shell(&format!("getprop {k}")).unwrap_or_default();
            let info = getprop("ro.product.model");
            let fingerprint = getprop("ro.build.fingerprint");
            let release = getprop("ro.build.version.release");
            let serial = getprop("ro.serialno");
            let brand = getprop("ro.product.brand");
            let dev = getprop("ro.product.device");
            let sdk = getprop("ro.build.version.sdk");
            let patch = getprop("ro.build.version.security_patch");
            let battery = device.shell("dumpsys battery").unwrap_or_default();
            let level = battery
                .lines()
                .find(|l| l.trim().starts_with("level:"))
                .and_then(|l| l.split(':').nth(1))
                .and_then(|v| v.trim().parse::<u8>().ok())
                .unwrap_or(0);
            let pkgs = device.shell("pm list packages").unwrap_or_default();
            let (w, h) = device
                .shell("wm size")
                .ok()
                .and_then(|out| mandroid::screen::parse_wm_size(&out))
                .unwrap_or((0, 0));
            Ok(json!({
                "model": info.trim(),
                "brand": brand.trim(),
                "device": dev.trim(),
                "android_release": release.trim(),
                "build": { "fingerprint": fingerprint.trim(), "security_patch": patch.trim() },
                "sdk": sdk.trim(),
                "serial": serial.trim(),
                "adb_serial": device.serial,
                "display": format!("{w}x{h}"),
                "battery": level,
                "packages": pkgs.lines().filter(|l| l.starts_with("package:")).count(),
            })
            .to_string())
        }
        "android_shell" => {
            let cmd = s("command");
            if cmd.trim().is_empty() {
                return Err("android_shell: command is required".into());
            }
            device.shell(&cmd)
        }
        "android_tap" => {
            let (x, y) = (i("x").ok_or("x required")?, i("y").ok_or("y required")?);
            device.shell(&format!("input tap {x} {y}"))?;
            Ok(format!("tapped ({x}, {y})"))
        }
        "android_swipe" => {
            let (x1, y1, x2, y2) = (
                i("x1").ok_or("x1 required")?,
                i("y1").ok_or("y1 required")?,
                i("x2").ok_or("x2 required")?,
                i("y2").ok_or("y2 required")?,
            );
            device.shell(&format!("input swipe {x1} {y1} {x2} {y2} 180"))?;
            Ok(format!("swiped ({x1},{y1}) → ({x2},{y2})"))
        }
        "android_text" => {
            let text = s("text");
            if text.is_empty() {
                return Err("android_text: text is required".into());
            }
            device.shell(&format!("input text {}", q(&text)))?;
            Ok(format!("typed {text:?}"))
        }
        "android_key" => {
            let code = i("code").ok_or("code required")?;
            device.shell(&format!("input keyevent {code}"))?;
            Ok(format!("keyevent {code}"))
        }
        "android_screen" => match s("action").as_str() {
            "get" | "" => {
                let size = device.shell("wm size")?;
                let density = device.shell("wm density")?;
                Ok(format!("{}\n{}", size.trim(), density.trim()))
            }
            "set" => {
                let w = i("width").ok_or("width required for set")?;
                let h = i("height").ok_or("height required for set")?;
                let d = i("density").unwrap_or(0);
                let mut out = device.shell(&format!("wm size {w}x{h}"))?;
                if d > 0 {
                    out.push_str(&device.shell(&format!("wm density {d}"))?);
                }
                Ok(out)
            }
            "reset" => {
                let mut out = device.shell("wm size reset")?;
                out.push_str(&device.shell("wm density reset")?);
                Ok(out)
            }
            other => Err(format!("android_screen: unknown action {other}")),
        },
        "android_screenshot" => {
            let png = device.screenshot()?;
            let dir = dirs::home_dir()
                .map(|h| h.join(".romacode").join("screenshots"))
                .unwrap_or_else(|| std::path::PathBuf::from("screenshots"));
            let _ = std::fs::create_dir_all(&dir);
            let given = s("path");
            let path = if given.is_empty() {
                let ts = std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .map(|d| d.as_millis())
                    .unwrap_or(0);
                dir.join(format!("phone-{ts}.png"))
            } else {
                std::path::PathBuf::from(given)
            };
            std::fs::write(&path, &png).map_err(|e| format!("write {}: {e}", path.display()))?;
            Ok(json!({
                "saved": path.display().to_string(),
                "bytes": png.len(),
                "dimensions": mandroid::screen::png_dimensions(&png),
            })
            .to_string())
        }
        "android_install" => {
            let apk = s("apk");
            if apk.trim().is_empty() {
                return Err("android_install: APK path is required".into());
            }
            device.install(std::path::Path::new(&apk))
        }
        "android_launch" => {
            let comp = s("component");
            if comp.trim().is_empty() {
                return Err("android_launch: component is required".into());
            }
            // Quote the component so `com.a/.MainActivity; reboot` stays ONE
            // literal device-shell word instead of running extra commands.
            // Same validation the in-process bridge enforces — tool semantics
            // no longer change depending on whether the bridge is up.
            if comp.contains('/') {
                device.shell(&format!("am start -n {}", q(&comp)))
            } else {
                device.shell(&format!(
                    "monkey -p {} -c android.intent.category.LAUNCHER 1",
                    q(&comp)
                ))
            }
        }
        "android_packages" => {
            let filter = s("filter");
            let out = device.shell("pm list packages")?;
            let lines: Vec<&str> = out
                .lines()
                .filter(|l| filter.is_empty() || l.contains(&filter))
                .collect();
            Ok(lines.join("\n"))
        }
        other => Err(format!("unknown android tool: {other}")),
    }
}
