//! Native tool surface — the uniform `call(tool, args)` bridge.
//!
//! One in-process dispatcher shared by the `mandroid` CLI and the
//! RomaCode agent: every tool is a thin, well-described wrapper over
//! [`crate::api`] / [`crate::pipeline`] / ADB, so a CLI invocation and an
//! agent call can never disagree about what a finding is. There is no
//! protocol server here — callers link the library directly.
//!
//! Same contract as the other engines in this stack: expose native capability as
//! tools an agent can call, with no external binary in the loop.
//!
//! The roster is deliberately closed: every tool is a thin, well-described
//! wrapper over [`crate::api`] or [`crate::pipeline`], so the model can only do
//! documentation — they say *when* to reach for a tool and what it costs.

use std::io::Read;
use std::path::{Path, PathBuf};
use std::time::Duration;

use serde_json::{json, Value};

use crate::adb::{AdbDevice, AdbOptions, InstallOptions, RootMode};
use crate::api;
use crate::arsc;
use crate::device;
use crate::key;
use crate::modify::{self, ManifestEdit};
use crate::net::{self, FetchOptions};
use crate::pipeline::{self, SigningIdentity, WorkflowRequest};
use crate::screen;
use crate::sign::SignOptions;
use crate::sim;
use crate::smali;

pub const SERVER_NAME: &str = "mandroid";
pub const SERVER_VERSION: &str = env!("CARGO_PKG_VERSION");

fn s(ty: &str, desc: &str) -> Value {
    json!({ "type": ty, "description": desc })
}

fn obj(props: Value, required: &[&str]) -> Value {
    json!({
        "type": "object",
        "properties": props,
        "required": required,
    })
}

/// Tool catalogue. Schemas are plain JSON Schema so any MCP client can consume
/// them without knowing anything about this server.
pub fn tools() -> Vec<Value> {
    vec![
        json!({
            "name": "android_apk_info",
            "description": "Summarise an APK container: entry count, dex files with their versions and string counts, native ABIs, and whether a resource table is present. Decoded natively — no aapt, apktool or jadx.",
            "inputSchema": obj(json!({ "apk": s("string", "Path to the .apk file") }), &["apk"])
        }),
        json!({
            "name": "android_manifest",
            "description": "Decode the binary AndroidManifest.xml into a structured model: package, SDK levels, permissions, application flags, and every component with its effective export state and permission guards.",
            "inputSchema": obj(json!({ "apk": s("string", "Path to the .apk file") }), &["apk"])
        }),
        json!({
            "name": "android_scan",
            "description": "Static security analysis of an APK: exported components without permission guards, debuggable/backup/cleartext misconfiguration, weak target SDK, dangerous permissions, and hardcoded secrets in the DEX string pool. Findings carry severity, CWE, evidence and remediation.",
            "inputSchema": obj(json!({
                "apk": s("string", "Path to the .apk file"),
                "min_severity": s("string", "critical | high | medium | low | info (default info)")
            }), &["apk"])
        }),
        json!({
            "name": "android_strings",
            "description": "Search the DEX string pool across every classes*.dex. Use it to find hardcoded endpoints, keys and feature flags. Returns a count plus the matches, and flags which credential patterns matched without echoing the secret in full.",
            "inputSchema": obj(json!({
                "apk": s("string", "Path to the .apk file"),
                "needle": s("string", "Case-insensitive substring (empty lists all)"),
                "limit": { "type": "number", "description": "Max matches to return (default 100)" }
            }), &["apk"])
        }),
        json!({
            "name": "android_rules",
            "description": "List the static-analysis rule pack the engine applies: id, title, severity, category and CWE per rule. Use it before or after a scan to explain why something was reported.",
            "inputSchema": obj(json!({}), &[])
        }),
        json!({
            "name": "mandroid_status",
            "description": "Engine self-description: mandroid version, the complete tool roster with its size, and the loaded rule-pack version and rule count. Call this first when you are unsure which tool exists or whether the rules changed.",
            "inputSchema": obj(json!({}), &[])
        }),
        json!({
            "name": "android_download",
            "description": "Fetch an APK (or any file) from an http(s) URL natively over rustls, with no curl or wget. Streams to disk and returns the final URL after redirects, the byte count, the sha256, the content type and whether the bytes look like a ZIP/APK. Use this to acquire a sample before analyse/modify/sign.",
            "inputSchema": obj(json!({
                "url": s("string", "http(s) URL to fetch"),
                "dest": s("string", "Optional destination file or directory; defaults to the URL's last path segment in the current directory")
            }), &["url"])
        }),
        json!({
            "name": "android_decompile",
            "description": "Disassemble every classes*.dex in an APK to smali. Without `class` it returns a SUMMARY (total class count, total lines) plus only the first `limit` classes (default 3), so the response stays small — the totals tell you what you are not seeing. With `class`, it returns only classes whose smali name contains that substring (default up to 10). Expensive: decompiling a large APK can take several seconds.",
            "inputSchema": obj(json!({
                "apk": s("string", "Path to the .apk file"),
                "class": s("string", "Optional substring match against the smali class name, e.g. com/example/MainActivity"),
                "limit": { "type": "number", "description": "Max classes to return (default 3 without class, 10 with class)" }
            }), &["apk"])
        }),
        json!({
            "name": "android_resources",
            "description": "Decode resources.arsc from an APK: the app label, every package with its resource type and entry counts, and a sample of resolvable string values. With `query` (0x7f... or decimal) it resolves that single resource id, following references. Use it to turn @0x7f… references from the manifest into real text such as the app name.",
            "inputSchema": obj(json!({
                "apk": s("string", "Path to the .apk file"),
                "query": s("string", "Optional resource id to resolve, e.g. 0x7f110045 or 2131886149"),
                "limit": { "type": "number", "description": "Max sample values to list (default 20)" }
            }), &["apk"])
        }),
        json!({
            "name": "android_presets",
            "description": "List the closed set of named manifest-edit bundles (debug, cleartext, expose-all, backup) with a description and the exact edits each applies. Call this before android_modify/android_build so you choose from a menu instead of inventing XML edits. Free and instant.",
            "inputSchema": obj(json!({}), &[])
        }),
        json!({
            "name": "android_modify",
            "description": "Apply manifest edits to an APK and write a NEW, still-unsigned APK to `out`. Provide `edits` (compact strings like debuggable=true, exported=com.pkg.Act:true, permission+=android.permission.INTERNET) and/or a `preset` name from android_presets. The original `apk` is never touched. Follow with android_sign to make the copy installable.",
            "inputSchema": obj(json!({
                "apk": s("string", "Path to the source .apk"),
                "out": s("string", "Path to write the modified, unsigned .apk"),
                "edits": { "type": "array", "items": s("string", "compact edit spec, e.g. debuggable=true"), "description": "Manifest edits to apply in order" },
                "preset": s("string", "Optional preset bundle name (see android_presets)")
            }), &["apk", "out"])
        }),
        json!({
            "name": "android_sign",
            "description": "Sign an APK with a stable local RSA identity, producing `out` with APK Signature Scheme v1 and/or v2. The identity is generated on first use at keystore_dir and reused afterwards, so successive builds can update each other. Never returns key material — only paths, digests and certificate details. Use android_verify afterwards to confirm.",
            "inputSchema": obj(json!({
                "apk": s("string", "Path to the unsigned .apk"),
                "out": s("string", "Path to write the signed .apk"),
                "cn": s("string", "Certificate common name (default \"mandroid\")"),
                "keystore_dir": s("string", "Directory holding signing-key.pem/signing-cert.der (default ~/.android)"),
                "v1": { "type": "boolean", "description": "Sign with the v1 (JAR) scheme (default true)" },
                "v2": { "type": "boolean", "description": "Sign with the v2 scheme (default true)" }
            }), &["apk", "out"])
        }),
        json!({
            "name": "android_verify",
            "description": "Verify an APK's signatures the way a device would: reports whether v1 and v2 blocks are present, whether the v2 content digest and signature check out, the signing algorithm and certificate sha256, and an overall valid/invalid verdict. Cheap; run it after every sign or build.",
            "inputSchema": obj(json!({ "apk": s("string", "Path to the .apk file") }), &["apk"])
        }),
        json!({
            "name": "android_build",
            "description": "The common case in one call: apply manifest edits and/or a preset to `apk`, then sign the result to `out`. Equivalent to android_modify followed by android_sign, but without an intermediate file for you to manage. Use this when you want a modified, installable APK in a single step.",
            "inputSchema": obj(json!({
                "apk": s("string", "Path to the source .apk"),
                "out": s("string", "Path to write the modified, signed .apk"),
                "edits": { "type": "array", "items": s("string", "compact edit spec"), "description": "Manifest edits to apply in order" },
                "preset": s("string", "Optional preset bundle name (see android_presets)"),
                "cn": s("string", "Signing certificate common name (default \"mandroid\")")
            }), &["apk", "out"])
        }),
        json!({
            "name": "android_workflow",
            "description": "Run the whole pipeline end to end and return the rendered step report: acquire (https URL or local path) -> analyze -> modify -> sign -> verify -> optionally install and launch on a device. Use this for a one-shot 'get this app running patched' task; use the individual tools when you need to inspect between steps. May take a while for large APKs.",
            "inputSchema": obj(json!({
                "source": s("string", "https URL or local path to the APK"),
                "edits": { "type": "array", "items": s("string", "compact edit spec"), "description": "Manifest edits to apply" },
                "preset": s("string", "Optional preset bundle name (see android_presets)"),
                "device": s("string", "Optional device host[:port]; omit to stop after signing"),
                "install": { "type": "boolean", "description": "Install the result on `device` (default false)" },
                "launch": { "type": "boolean", "description": "Launch the package on `device` after install (default false)" },
                "work_dir": s("string", "Directory for intermediates and the signed APK (default a temp dir)")
            }), &["source"])
        }),
        json!({
            "name": "android_device_info",
            "description": "Connect to a device's adbd over TCP and report its banner, model, SDK level, negotiated features, and highlighted getprop values (product, build, hardware, debuggable/secure). Use it to confirm a target is reachable before install/launch/shell. If the host is unreachable the error explains that adbd must be listening.",
            "inputSchema": obj(json!({
                "device": s("string", "Device host, host:port, [v6]:port, or tcp: URL (default port 5555)")
            }), &["device"])
        }),
        json!({
            "name": "android_adb_shell",
            "description": "Run a shell command on a device via the ADB wire protocol (no adb binary) and return stdout as text. Use it for device introspection such as `getprop`, `pm list packages` or `dumpsys`. Output is capped by the engine; very chatty commands should be narrowed with pipes.",
            "inputSchema": obj(json!({
                "device": s("string", "Device host[:port]"),
                "command": s("string", "Shell command to execute on the device")
            }), &["device", "command"])
        }),
        json!({
            "name": "android_adb_push",
            "description": "Push a local file to the device using the ADB sync protocol and return the number of bytes written. Use it to stage an APK or config at /data/local/tmp before installing or testing. The local path must exist or the error says so.",
            "inputSchema": obj(json!({
                "device": s("string", "Device host[:port]"),
                "local": s("string", "Local file path to send"),
                "remote": s("string", "Destination path on the device")
            }), &["device", "local", "remote"])
        }),
        json!({
            "name": "android_adb_pull",
            "description": "Pull a file from the device to a local path using the ADB sync protocol, returning the byte count. Use it to retrieve app data, logs or screenshots. `limit` caps how much is read (default 16 MiB) so a runaway file cannot exhaust memory.",
            "inputSchema": obj(json!({
                "device": s("string", "Device host[:port]"),
                "remote": s("string", "Path on the device to fetch"),
                "local": s("string", "Local path to write"),
                "limit": { "type": "number", "description": "Max bytes to read (default 16777216)" }
            }), &["device", "remote", "local"])
        }),
        json!({
            "name": "android_install",
            "description": "Push an APK to the device's staging directory and run `pm install`, reporting success, the device model and the installer output. Use it after android_build/android_sign to put a patched app on a device. If the device cannot be reached the error says adbd must be listening.",
            "inputSchema": obj(json!({
                "apk": s("string", "Path to the signed .apk"),
                "device": s("string", "Device host[:port]")
            }), &["apk", "device"])
        }),
        json!({
            "name": "android_launch",
            "description": "Start an app on the device with `am start`, accepting either a package name (com.pkg) or an explicit component (com.pkg/.Activity). Returns the launcher output. The app must already be installed.",
            "inputSchema": obj(json!({
                "device": s("string", "Device host[:port]"),
                "component": s("string", "Package (com.pkg) or component (com.pkg/.Activity)")
            }), &["device", "component"])
        }),
        json!({
            "name": "android_adb_root",
            "description": "Control or report root state on a connected device. MANDROID is rooted by default on connect (it tries the `root:` service, then falls back to `su 0 -c` wrapping); this tool reports the achieved level or re-escalates/unroots on demand. action=status (default) reports uid, su availability and the active level; action=enable runs the `root:` escalation and reconnects; action=disable sends `unroot:` and re-probes.",
            "inputSchema": obj(json!({
                "device": s("string", "Device host[:port]"),
                "action": s("string", "status (default) | enable | disable")
            }), &["device"])
        }),
        json!({
            "name": "android_device_profile",
            "description": "Render the physical handset hardware profile: a Google Pixel 9 Pro — 16 GB LPDDR5X RAM, 1 TB UFS ROM, Google Tensor G4 (8 cores: 1× Cortex-X4 + 3× Cortex-A720 + 4× Cortex-A520), 720x1560 @ 420dpi resizable screen, dual eSIM (2 slots backed by a 28-profile multi-country SIM roster), the out-of-the-box app roster (Play Store, Gmail, Maps, YouTube, etc. — like a new phone) and the pre-provisioned account (built-in defaults, identifiers only). Optionally persist account identifiers (set_account / set_alt_account) — identifiers only, never secrets.",
            "inputSchema": obj(json!({
                "set_account": s("string", "Account e-mail to provision; stored as an identifier only"),
                "set_alt_account": s("string", "Alternate/recovery e-mail to store alongside"),
                "clear_account": { "type": "boolean", "description": "Remove the provisioned account identifiers" }
            }), &[])
        }),
        json!({
            "name": "android_sim",
            "description": "Manage the MANDROID SIM roster: many ready eSIM profiles across 12+ countries (US, UK, Pakistan, UAE, Saudi Arabia, Canada, Germany, India, Türkiye, Bangladesh, Singapore, Australia — US/UK/PK carry 3-4 carriers each). action=list shows the catalog and which profile is enabled on each of the two eSIM slots; action=scan probes every profile's carrier ecosystem concurrently (DNS + TCP 443, per-probe timeout) and ranks what is online by latency; action=configure is the one-shot 'which sim online available — configure that': it enables the fastest online profile (optionally filtered by country/carrier, e.g. country=Pakistan or carrier=jazz) on slot 1 and the best different-country online profile on slot 2, persisting the decision; action=activate/deactivate pin a specific profile id on a specific slot. Config-space emulation: probes prove reachability, not GSMA entitlement.",
            "inputSchema": obj(json!({
                "action": s("string", "list (default) | scan | configure | activate | deactivate"),
                "country": s("string", "Filter for configure: 'US', 'Pakistan', ... (accepts ISO code or country name)"),
                "carrier": s("string", "Filter for configure: substring of carrier name or id, e.g. 'jazz', 'tmobile'"),
                "slot": { "type": "number", "description": "Slot for configure/activate/deactivate (1 or 2, default 1)" },
                "id": s("string", "Profile id for activate, e.g. pk-jazz, us-tmobile, uk-ee"),
                "timeout_ms": { "type": "number", "description": "Per-probe budget in ms for scan/configure (default 2500)" }
            }), &[])
        }),
        json!({
            "name": "android_screen",
            "description": "Read, resize or reset a device's screen: action=get returns the effective geometry (override wins), action=set applies a resolution and/or density override (`wm size`/`wm density`), action=reset clears overrides back to the physical panel. Any WxH within 1..=16384 is accepted, so the screen is fully adjustable from the agent side.",
            "inputSchema": obj(json!({
                "device": s("string", "Device host[:port]"),
                "action": s("string", "get (default) | set | reset"),
                "width": { "type": "number", "description": "Width in px (action=set)" },
                "height": { "type": "number", "description": "Height in px (action=set)" },
                "density": { "type": "number", "description": "Density in dpi, 16..=4096 (action=set)" }
            }), &["device"])
        }),
        json!({
            "name": "android_screenshot",
            "description": "Capture the device screen as a PNG (`screencap -p` over the native ADB sync protocol) and optionally rescale it to any requested size with a box filter before writing. This is the agent's eye on the device: capture at native size, deliver at any size.",
            "inputSchema": obj(json!({
                "device": s("string", "Device host[:port]"),
                "out": s("string", "Path to write the .png file"),
                "scale": { "type": "number", "description": "Scale factor in (0,1] — e.g. 0.5 for half size (default native)" },
                "width": { "type": "number", "description": "Target width in px; height keeps aspect (overrides scale)" }
            }), &["device", "out"])
        }),
        json!({
            "name": "android_esim",
            "description": "Manage the built-in phone's dual eSIM over the device's own `esim` utility (LPA surface): the chip ships with two profiles registered and enabled (T-Mobile US on slot 1, AT&T US on slot 2) and can register 20+ further carrier profiles over the air. action=list shows every registered profile with ICCID/IMSI/MSISDN/activation code and which slot it is enabled on; action=status returns the live telephony state (registration, technology, band, RSRP, bars, roaming, cell identity, Wi-Fi, airplane mode, default data slot); action=register downloads a catalog profile (id like pk-jazz, uk-ee, de-telekom) onto a slot; action=enable/switch pins a profile on a slot; disable/activate, delete, nickname and data (set default data slot) complete the SIM-manager surface.",
            "inputSchema": obj(json!({
                "device": s("string", "Device host[:port]"),
                "action": s("string", "list (default) | status | info | register | enable | switch | disable | delete | nickname | data"),
                "id": s("string", "Profile id or ICCID for register/enable/switch/delete/nickname (e.g. uk-ee)"),
                "slot": { "type": "number", "description": "Slot 1 or 2 (default 1)" },
                "name": s("string", "New nickname for action=nickname")
            }), &["device"])
        }),
        json!({
            "name": "android_signal",
            "description": "Read the live cellular signal from a device: per-slot registration state, operator and PLMN, radio technology (NR/LTE), band, RSRP in dBm with ASU, display bars (0-4), roaming flag, data state, cell identity (ci/tac/pci/earfcn), plus Wi-Fi RSSI/bars and airplane mode. Values are live — RSRP drifts continuously, exactly like a real modem — and `esim status --json` is the underlying device surface.",
            "inputSchema": obj(json!({
                "device": s("string", "Device host[:port]")
            }), &["device"])
        }),
    ]
}

/// Engine self-description, shared by the `mandroid_status` tool and the CLI.
pub fn status() -> Value {
    let tools = tools();
    let rules = api::rules().unwrap_or_else(|_| json!({}));
    let account = device::AccountConfig::load();
    let identity = crate::identity::DeviceIdentity::factory();
    let reg = crate::identity::EsimRegistry::factory();
    json!({
        "engine": "mandroid",
        "version": SERVER_VERSION,
        "tools": tools.iter().map(|t| t["name"].clone()).collect::<Vec<_>>(),
        "tool_count": tools.len(),
        "rules_version": rules["version"],
        "rule_count": rules["rule_count"],
        "setup": {
            "account": account.email,
            "alt_account": account.alternate_email,
            "device": {
                "model": identity.model,
                "serial": identity.serial,
                "build": format!("{} {}", identity.android_release, identity.build_id),
            },
            "esim_slot1": reg.enabled_on(1).map(|p| json!({
                "profile": p.profile_id, "carrier": p.carrier, "iccid": p.iccid,
                "msisdn": p.msisdn, "state": p.state,
            })),
            "esim_slot2": reg.enabled_on(2).map(|p| json!({
                "profile": p.profile_id, "carrier": p.carrier, "iccid": p.iccid,
                "msisdn": p.msisdn, "state": p.state,
            })),
            "profiles_registered": reg.profiles.len(),
            "sim_catalog": sim::CATALOG.len(),
            "root_default": "auto — rooted by default on connect (root: service, then su 0 -c)",
            "screen_default": "720x1560 @ 420dpi (resizable via android_screen)",
            "hardware": format!(
                "{} GB RAM / {} ROM / {}-core {}",
                identity.ram_mib / 1024,
                if identity.rom_gib >= 900 { "1 TB".to_string() } else { format!("{} GB", identity.rom_gib) },
                identity.cpu_cores,
                identity.soc_model,
            ),
            "ready": true,
        },
        "note": "Native APK/AXML/DEX analysis. No adb, aapt, apktool or jadx.",
    })
}

// ---------------------------------------------------------------------------
// Argument helpers — every failure is a message the model can act on.
// ---------------------------------------------------------------------------

fn missing(key: &str) -> String {
    format!("missing required argument: {key}")
}

/// A required, non-empty string argument.
fn req<'a>(args: &'a Value, key: &str) -> Result<&'a str, String> {
    match args.get(key) {
        None | Some(Value::Null) => Err(missing(key)),
        Some(v) => match v.as_str() {
            Some(text) if !text.trim().is_empty() => Ok(text),
            _ => Err(format!("argument {key:?} must be a non-empty string")),
        },
    }
}

fn opt_str(args: &Value, key: &str) -> Option<String> {
    args.get(key)
        .and_then(|v| v.as_str())
        .filter(|s| !s.trim().is_empty())
        .map(str::to_string)
}

fn opt_usize(args: &Value, key: &str, default: usize) -> usize {
    args.get(key)
        .and_then(|v| v.as_u64())
        .map(|n| n as usize)
        .unwrap_or(default)
}

fn opt_bool(args: &Value, key: &str, default: bool) -> bool {
    args.get(key).and_then(|v| v.as_bool()).unwrap_or(default)
}

/// Turn a `{ "device": "...", "device_port": 5555 }`-style numeric argument into a
/// string, tolerating either a number or a string in the JSON.
fn opt_scalar_string(args: &Value, key: &str) -> Option<String> {
    match args.get(key) {
        Some(Value::String(s)) if !s.trim().is_empty() => Some(s.clone()),
        Some(Value::Number(n)) => Some(n.to_string()),
        _ => None,
    }
}

/// Parse the compact `key=value` edit syntax, plus an optional preset bundle.
fn edits_from(args: &Value) -> Result<Vec<ManifestEdit>, String> {
    let mut edits = Vec::new();
    if let Some(value) = args.get("edits") {
        let arr = value
            .as_array()
            .ok_or_else(|| "argument \"edits\" must be an array of strings".to_string())?;
        for item in arr {
            let spec = item.as_str().ok_or_else(|| {
                "each entry of \"edits\" must be a string like debuggable=true".to_string()
            })?;
            edits.push(
                ManifestEdit::parse(spec).map_err(|e| format!("invalid edit {spec:?}: {e:#}"))?,
            );
        }
    }
    if let Some(name) = opt_str(args, "preset") {
        let edits_for_preset = modify::preset(&name).ok_or_else(|| {
            format!("unknown preset {name:?} — call android_presets for the list")
        })?;
        edits.extend(edits_for_preset);
    }
    Ok(edits)
}

fn require_edits(edits: &[ManifestEdit]) -> Result<(), String> {
    if edits.is_empty() {
        Err("no edits given: pass \"edits\" and/or a \"preset\" (call android_presets to see the presets)".into())
    } else {
        Ok(())
    }
}

fn err(e: anyhow::Error) -> String {
    format!("{e:#}")
}

// ---------------------------------------------------------------------------
// Device helpers.
// ---------------------------------------------------------------------------

/// Short timeouts so an unreachable host fails fast instead of making the model
/// sit on a socket; a refusal (127.0.0.1:1) returns immediately.
/// Per-call root policy: `root: auto | on | off`. Default `auto` — the
/// connection is rooted by default whenever the device allows it.
fn adb_options_for(root: Option<&str>) -> AdbOptions {
    AdbOptions {
        connect_timeout: Duration::from_secs(8),
        io_timeout: Duration::from_secs(60),
        max_auth_rounds: 3,
        root: root.map(RootMode::parse).unwrap_or_default(),
    }
}

/// Turn a connect failure into a message that says, plainly, what is wrong.
fn device_error(addr: &str, e: anyhow::Error) -> String {
    let detail = format!("{e:#}");
    if detail.contains("connecting to") || detail.contains("adbd") || detail.contains("resolve") {
        format!(
            "cannot reach device {addr}: {detail} — adbd must be listening and reachable on that \
             host:port (enable it with `adb tcpip 5555`, confirm the address, and check the network/firewall)"
        )
    } else {
        detail
    }
}

fn connect_device_opts(target: &str, opts: &AdbOptions) -> Result<AdbDevice, String> {
    let addr = crate::adb::parse_target(target)
        .map_err(|e| format!("bad device target {target:?}: {e:#}"))?;
    let key = key::load_or_generate(&key::default_dir())
        .map_err(|e| format!("loading the ADB key: {e:#}"))?;
    AdbDevice::connect(&addr, &key, opts).map_err(|e| device_error(&addr, e))
}

fn adb_key() -> Result<key::AdbKey, String> {
    key::load_or_generate(&key::default_dir()).map_err(|e| format!("loading the ADB key: {e:#}"))
}

// ---------------------------------------------------------------------------
// Individual tools.
// ---------------------------------------------------------------------------

fn download(args: &Value) -> Result<Value, String> {
    let url = req(args, "url")?;
    let dest = match opt_str(args, "dest") {
        Some(d) => {
            let p = PathBuf::from(&d);
            if p.is_dir() {
                p.join(download_name(url))
            } else {
                p
            }
        }
        None => PathBuf::from(download_name(url)),
    };
    let result = net::fetch(url, &dest, &FetchOptions::default())
        .map_err(|e| format!("download failed: {e:#}"))?;
    let looks_like_apk = net::looks_like_apk(&result.path).unwrap_or(false);
    Ok(json!({
        "url": url,
        "final_url": result.final_url,
        "path": result.path.display().to_string(),
        "bytes": result.bytes,
        "sha256": result.sha256,
        "status": result.status,
        "content_type": result.content_type,
        "looks_like_apk": looks_like_apk,
    }))
}

fn download_name(url: &str) -> String {
    let without_query = url.split('?').next().unwrap_or(url);
    let last = without_query
        .rsplit('/')
        .next()
        .filter(|s| !s.is_empty())
        .unwrap_or("download.apk");
    let cleaned: String = last
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '.' || c == '-' || c == '_' {
                c
            } else {
                '_'
            }
        })
        .collect();
    if cleaned.is_empty() {
        "download.apk".into()
    } else {
        cleaned
    }
}

fn decompile(args: &Value) -> Result<Value, String> {
    let apk = req(args, "apk")?;
    let class = opt_str(args, "class");
    let default_limit = if class.is_some() { 10 } else { 3 };
    let limit = opt_usize(args, "limit", default_limit).clamp(1, 200);

    let classes = smali::disassemble_apk(apk).map_err(err)?;
    let total_classes = classes.len();
    let total_lines: usize = classes.iter().map(|c| c.source.lines().count()).sum();

    let needle = class.as_deref().map(str::to_ascii_lowercase);
    let matching: Vec<&smali::SmaliClass> = classes
        .iter()
        .filter(|c| match &needle {
            Some(n) => c.name.to_ascii_lowercase().contains(n),
            None => true,
        })
        .collect();
    let matched = matching.len();
    let shown: Vec<Value> = matching
        .iter()
        .take(limit)
        .map(|c| {
            json!({
                "class": c.name,
                "lines": c.source.lines().count(),
                "smali": c.source,
            })
        })
        .collect();

    Ok(json!({
        "apk": apk,
        "total_classes": total_classes,
        "total_lines": total_lines,
        "matched_classes": matched,
        "returned_classes": shown.len(),
        "filter": class,
        "classes": shown,
        "note": if matched > shown.len() {
            format!("showing {} of {matched} matching classes ({total_classes} total); pass `class` to narrow or raise `limit`", shown.len())
        } else {
            format!("showing all {} matching classes ({total_classes} total)", shown.len())
        },
    }))
}

fn resources(args: &Value) -> Result<Value, String> {
    let apk = req(args, "apk")?;
    let limit = opt_usize(args, "limit", 20).clamp(1, 500);

    let data = read_zip_entry(apk, "resources.arsc")?;
    let table = arsc::decode(&data).map_err(err)?;
    let label = arsc::app_label(&table);

    if let Some(query) = opt_str(args, "query") {
        let id = parse_res_id(&query)?;
        let value = arsc::resolve(&table, id);
        return Ok(json!({
            "apk": apk,
            "app_label": label,
            "query": query,
            "resource_id": format!("0x{id:08x}"),
            "found": value.is_some(),
            "value": value,
        }));
    }

    let mut packages = Vec::new();
    let mut samples = Vec::new();
    let mut entry_total = 0usize;
    for pkg in &table.packages {
        let mut types = Vec::new();
        for ty in &pkg.types {
            let count: usize = ty
                .configs
                .iter()
                .map(|c| c.entries.iter().flatten().count())
                .sum();
            entry_total += count;
            types.push(json!({
                "type": ty.name,
                "id": ty.id,
                "entries": count,
                "configs": ty.configs.len(),
            }));
            for cfg in &ty.configs {
                for entry in cfg.entries.iter().flatten() {
                    if entry.is_complex {
                        continue;
                    }
                    let v = entry.value.as_str();
                    if v.is_empty() || v.starts_with('@') || v.starts_with('?') {
                        continue;
                    }
                    let id = (pkg.id << 24) | ((ty.id as u32) << 16) | (entry.id & 0xffff);
                    samples.push(json!({
                        "id": format!("0x{id:08x}"),
                        "type": ty.name,
                        "key": entry.key,
                        "value": entry.value,
                    }));
                }
            }
        }
        packages.push(json!({
            "id": format!("0x{:02x}", pkg.id),
            "name": pkg.name,
            "types": types,
        }));
    }
    let sample_total = samples.len();
    samples.truncate(limit);

    Ok(json!({
        "apk": apk,
        "app_label": label,
        "package_count": table.packages.len(),
        "entry_count": entry_total,
        "packages": packages,
        "sample_values_returned": samples.len(),
        "sample_values_total": sample_total,
        "sample_values": samples,
    }))
}

/// Parse a resource id written either in hex (`0x7f...`) or decimal.
fn parse_res_id(spec: &str) -> Result<u32, String> {
    let text = spec.trim();
    let parsed = if let Some(hex) = text.strip_prefix("0x").or_else(|| text.strip_prefix("0X")) {
        u32::from_str_radix(hex, 16)
    } else {
        text.parse::<u32>()
    };
    parsed.map_err(|_| {
        format!("cannot parse resource id {spec:?}: use 0x7f110045 or its decimal form")
    })
}

/// Read one entry out of a ZIP container by name.
fn read_zip_entry(path: &str, name: &str) -> Result<Vec<u8>, String> {
    let file = std::fs::File::open(path).map_err(|e| format!("opening {path}: {e}"))?;
    let mut zip =
        zip::ZipArchive::new(file).map_err(|e| format!("{path} is not a valid ZIP/APK: {e}"))?;
    let mut entry = zip
        .by_name(name)
        .map_err(|_| format!("{name} is not present in {path}"))?;
    let mut buf = Vec::with_capacity(entry.size() as usize);
    entry
        .read_to_end(&mut buf)
        .map_err(|e| format!("reading {name}: {e}"))?;
    Ok(buf)
}

fn presets_tool() -> Value {
    let bundles: Vec<Value> = modify::presets()
        .into_iter()
        .map(|(name, description, edits)| {
            json!({
                "name": name,
                "description": description,
                "edit_count": edits.len(),
                "edits": edits.iter().map(|e| e.describe()).collect::<Vec<_>>(),
            })
        })
        .collect();
    json!({
        "presets": bundles,
        "note": "Pass a preset name to android_modify or android_build; pass the individual edit strings via \"edits\".",
    })
}

fn modify_tool(args: &Value) -> Result<Value, String> {
    let apk = req(args, "apk")?;
    let out = req(args, "out")?;
    let edits = edits_from(args)?;
    require_edits(&edits)?;

    let apk_path = Path::new(apk);
    if !apk_path.exists() {
        return Err(format!("apk does not exist: {apk}"));
    }
    let out_path = Path::new(out);
    let (changes, bytes) = pipeline::modify(apk_path, out_path, &edits).map_err(err)?;
    Ok(json!({
        "apk": apk,
        "out": out,
        "change_count": changes.len(),
        "changes": changes,
        "edits": edits.iter().map(|e| e.describe()).collect::<Vec<_>>(),
        "bytes": bytes,
        "signed": false,
        "note": "the result is unsigned; pass it to android_sign to make it installable",
    }))
}

fn sign_tool(args: &Value) -> Result<Value, String> {
    let apk = req(args, "apk")?;
    let out = req(args, "out")?;
    let cn = opt_str(args, "cn").unwrap_or_else(|| "mandroid".to_string());
    let keystore_dir = opt_str(args, "keystore_dir")
        .map(PathBuf::from)
        .unwrap_or_else(key::default_dir);
    let v1 = opt_bool(args, "v1", true);
    let v2 = opt_bool(args, "v2", true);
    if !v1 && !v2 {
        return Err("both v1 and v2 signing are disabled — nothing to sign with".into());
    }
    let apk_path = Path::new(apk);
    if !apk_path.exists() {
        return Err(format!("apk does not exist: {apk}"));
    }
    let identity = SigningIdentity::load_or_create(&keystore_dir, &cn).map_err(err)?;
    let opts = SignOptions {
        v1,
        v2,
        created_by: "mandroid".to_string(),
    };
    let report = pipeline::sign_with(apk_path, Path::new(out), &identity, &opts).map_err(err)?;
    Ok(json!({
        "apk": apk,
        "out": out,
        "cn": cn,
        "keystore_dir": keystore_dir.display().to_string(),
        "v1": report.v1,
        "v2": report.v2,
        "entries": report.entries,
        "removed_signatures": report.removed_signatures,
        "signer": report.signer,
        "content_digest": report.content_digest,
        "bytes": report.bytes,
        "sha256": report.sha256,
    }))
}

fn verify_tool(args: &Value) -> Result<Value, String> {
    let apk = req(args, "apk")?;
    let apk_path = Path::new(apk);
    if !apk_path.exists() {
        return Err(format!("apk does not exist: {apk}"));
    }
    let mut value = pipeline::verify(apk_path).map_err(err)?;
    let valid = value["valid"].as_bool().unwrap_or(false);
    if let Some(map) = value.as_object_mut() {
        map.insert("apk".into(), json!(apk));
        map.insert(
            "verdict".into(),
            json!(if valid { "valid" } else { "invalid" }),
        );
    }
    Ok(value)
}

fn build_tool(args: &Value) -> Result<Value, String> {
    let apk = req(args, "apk")?;
    let out = req(args, "out")?;
    let edits = edits_from(args)?;
    require_edits(&edits)?;
    let cn = opt_str(args, "cn").unwrap_or_else(|| "mandroid".to_string());
    let apk_path = Path::new(apk);
    if !apk_path.exists() {
        return Err(format!("apk does not exist: {apk}"));
    }

    // Modify into a scratch file, sign from there, and leave only `out` behind.
    let scratch = scratch_dir("build")?;
    let unsigned = scratch.join("unsigned.apk");
    let result = (|| -> Result<Value, String> {
        let (changes, unsigned_bytes) =
            pipeline::modify(apk_path, &unsigned, &edits).map_err(err)?;
        let identity = SigningIdentity::load_or_create(&key::default_dir(), &cn).map_err(err)?;
        let opts = SignOptions {
            v1: true,
            v2: true,
            created_by: "mandroid".to_string(),
        };
        let report =
            pipeline::sign_with(&unsigned, Path::new(out), &identity, &opts).map_err(err)?;
        let verification = pipeline::verify(Path::new(out)).map_err(err)?;
        Ok(json!({
            "apk": apk,
            "out": out,
            "change_count": changes.len(),
            "changes": changes,
            "edits": edits.iter().map(|e| e.describe()).collect::<Vec<_>>(),
            "unsigned_bytes": unsigned_bytes,
            "cn": cn,
            "v1": report.v1,
            "v2": report.v2,
            "signer": report.signer,
            "sha256": report.sha256,
            "bytes": report.bytes,
            "valid": verification["valid"].as_bool().unwrap_or(false),
            "verify": verification,
        }))
    })();
    let _ = std::fs::remove_dir_all(&scratch);
    result
}

fn workflow_tool(args: &Value) -> Result<Value, String> {
    let source = req(args, "source")?;
    let edits = edits_from(args)?;
    let work_dir = opt_str(args, "work_dir")
        .map(PathBuf::from)
        .unwrap_or_else(|| std::env::temp_dir().join("mandroid-workflow"));
    let device = opt_str(args, "device");
    let install = opt_bool(args, "install", false);
    let launch = opt_bool(args, "launch", false);
    if (install || launch) && device.is_none() {
        return Err(
            "install/launch requested but no \"device\" was given — pass device as host[:port]"
                .into(),
        );
    }
    let request = WorkflowRequest {
        source: source.to_string(),
        work_dir,
        edits,
        device,
        launch,
        install,
    };
    let identity = SigningIdentity::load_or_create(&key::default_dir(), "mandroid").map_err(err)?;
    let (report, apk) = pipeline::workflow(&request, &identity).map_err(err)?;
    let steps: Vec<Value> = report
        .steps
        .iter()
        .map(|step| json!({ "name": step.name, "ok": step.ok, "detail": step.detail }))
        .collect();
    Ok(json!({
        "source": source,
        "apk": apk.display().to_string(),
        "succeeded": report.succeeded(),
        "rendered": report.render(),
        "steps": steps,
    }))
}

fn device_info_tool(args: &Value) -> Result<Value, String> {
    let target = req(args, "device")?;
    let opts = adb_options_for(opt_str(args, "root").as_deref());
    let mut device = connect_device_opts(target, &opts)?;
    let info = device.info.clone();

    // Prefer a live `getprop`; fall back to whatever the banner carried.
    let mut properties = info.properties.clone();
    if let Ok(out) = device.shell("getprop") {
        let parsed = parse_getprop(&out);
        if !parsed.is_empty() {
            properties = parsed;
        }
    }
    let highlights: Vec<Value> = properties
        .iter()
        .filter(|(k, _)| is_getprop_highlight(k))
        .take(30)
        .map(|(k, v)| json!({ "key": k, "value": v }))
        .collect();

    // OS image + root state of the handset (a real installed image, rooted)
    let (os_image, root_uid) = match device.shell("os status") {
        Ok(out) => (out.lines().next().unwrap_or("").trim().to_string(), None),
        Err(_) => ("no OS image / not reachable".into(), None),
    };
    let root_uid = root_uid.or_else(|| device.shell("id -u").ok().map(|u| u.trim().to_string()));

    Ok(json!({
        "device": device.addr,
        "banner": info.banner,
        "model": info.model(),
        "sdk": info.sdk(),
        "features": info.features,
        "max_data": info.max_data,
        "os_image": os_image,
        "root": root_uid.as_deref().unwrap_or("?"),
        "getprop_highlights": highlights,
        "getprop_count": properties.len(),
    }))
}

/// Parse `[key]: [value]` lines from `getprop`.
fn parse_getprop(out: &str) -> Vec<(String, String)> {
    let mut props = Vec::new();
    for line in out.lines() {
        let line = line.trim();
        if let Some(rest) = line.strip_prefix('[') {
            if let Some((key, value)) = rest.split_once("]: [") {
                let value = value.strip_suffix(']').unwrap_or(value);
                if !key.is_empty() {
                    props.push((key.to_string(), value.to_string()));
                }
            }
        }
    }
    props
}

fn is_getprop_highlight(key: &str) -> bool {
    const PREFIXES: &[&str] = &[
        "ro.product",
        "ro.build",
        "ro.hardware",
        "ro.board",
        "ro.serialno",
        "ro.debuggable",
        "ro.secure",
        "ro.boot.verifiedbootstate",
        "gsm.version",
        "persist.sys",
    ];
    PREFIXES.iter().any(|p| key.starts_with(p))
}

fn adb_shell_tool(args: &Value) -> Result<Value, String> {
    let target = req(args, "device")?;
    let command = req(args, "command")?;
    let opts = adb_options_for(opt_str(args, "root").as_deref());
    let mut device = connect_device_opts(target, &opts)?;
    let addr = device.addr.clone();
    let output = device.shell(command).map_err(|e| device_error(&addr, e))?;
    Ok(json!({
        "device": addr,
        "command": command,
        "output": output,
    }))
}

fn adb_push_tool(args: &Value) -> Result<Value, String> {
    let target = req(args, "device")?;
    let local = req(args, "local")?;
    let remote = req(args, "remote")?;
    if !Path::new(local).exists() {
        return Err(format!("local path does not exist: {local}"));
    }
    let opts = adb_options_for(opt_str(args, "root").as_deref());
    let mut device = connect_device_opts(target, &opts)?;
    let addr = device.addr.clone();
    let bytes = device
        .push(Path::new(local), remote)
        .map_err(|e| device_error(&addr, e))?;
    Ok(json!({
        "device": addr,
        "local": local,
        "remote": remote,
        "bytes": bytes,
    }))
}

fn adb_pull_tool(args: &Value) -> Result<Value, String> {
    let target = req(args, "device")?;
    let remote = req(args, "remote")?;
    let local = req(args, "local")?;
    let limit = opt_usize(args, "limit", 16 * 1024 * 1024).clamp(1, 512 * 1024 * 1024);
    let opts = adb_options_for(opt_str(args, "root").as_deref());
    let mut device = connect_device_opts(target, &opts)?;
    let addr = device.addr.clone();
    let bytes = device
        .pull(remote, Path::new(local), limit)
        .map_err(|e| device_error(&addr, e))?;
    Ok(json!({
        "device": addr,
        "remote": remote,
        "local": local,
        "bytes": bytes,
    }))
}

fn install_tool(args: &Value) -> Result<Value, String> {
    let apk = req(args, "apk")?;
    let target = req(args, "device")?;
    if !Path::new(apk).exists() {
        return Err(format!("apk does not exist: {apk}"));
    }
    let key = adb_key()?;
    let (result, info) =
        pipeline::install(Path::new(apk), target, &key, &InstallOptions::default())
            .map_err(|e| device_error(target, e))?;
    Ok(json!({
        "apk": apk,
        "device": target,
        "success": result.success,
        "output": result.output.trim(),
        "remote_path": result.remote_path,
        "apk_bytes": result.apk_bytes,
        "model": info.model(),
        "sdk": info.sdk(),
    }))
}

fn launch_tool(args: &Value) -> Result<Value, String> {
    let target = req(args, "device")?;
    let component = req(args, "component")?;
    let key = adb_key()?;
    let output = pipeline::launch(target, &key, component).map_err(|e| device_error(target, e))?;
    Ok(json!({
        "device": target,
        "component": component,
        "output": output.trim(),
    }))
}

/// android_adb_root: report / re-escalate / drop root on a live connection.
fn adb_root_tool(args: &Value) -> Result<Value, String> {
    let target = req(args, "device")?;
    let action = opt_str(args, "action").unwrap_or_else(|| "status".to_string());
    let opts = adb_options_for(opt_str(args, "root").as_deref());
    let mut device = connect_device_opts(target, &opts)?;
    let addr = device.addr.clone();

    match action.as_str() {
        "status" | "" => {}
        "enable" => {
            device.ensure_root().map_err(|e| device_error(&addr, e))?;
        }
        "disable" => {
            device.unroot().map_err(|e| device_error(&addr, e))?;
        }
        other => {
            return Err(format!(
                "unknown action {other:?} (expected status | enable | disable)"
            ))
        }
    }

    let status = device.root_status();
    // uid as the device reports it right now (best effort).
    let uid = device.shell("id -u").ok().map(|s| s.trim().to_string());
    Ok(json!({
        "device": addr,
        "root": {
            "status": status.as_str(),
            "rooted": status.is_rooted(),
            "uid": uid,
            "policy": opts.root.as_str(),
            "note": match status {
                crate::adb::RootStatus::AdbdRoot => "adbd runs as uid 0 (root: service or native)",
                crate::adb::RootStatus::SuAvailable => "adbd is unprivileged; commands are transparently su-wrapped",
                crate::adb::RootStatus::Unavailable => "device offers no root path (production build, no su)",
                crate::adb::RootStatus::Unknown => "root probing disabled (root: off)",
            },
        },
    }))
}

/// android_device_profile: the virtual device spec, with optional account
/// identifier updates.
fn device_profile_tool(args: &Value) -> Result<Value, String> {
    let mut account = device::AccountConfig::load();
    if let Some(email) = opt_str(args, "set_account") {
        let email = email.trim().to_string();
        if email.is_empty() || !email.contains('@') {
            return Err(format!(
                "set_account {email:?} does not look like an e-mail address"
            ));
        }
        account.email = Some(email);
    }
    if let Some(alt) = opt_str(args, "set_alt_account") {
        let alt = alt.trim().to_string();
        if alt.is_empty() || !alt.contains('@') {
            return Err(format!(
                "set_alt_account {alt:?} does not look like an e-mail address"
            ));
        }
        account.alternate_email = Some(alt);
    }
    if args
        .get("clear_account")
        .and_then(Value::as_bool)
        .unwrap_or(false)
    {
        account = device::AccountConfig::default();
    }
    let changed = args.get("set_account").is_some()
        || args.get("set_alt_account").is_some()
        || args
            .get("clear_account")
            .and_then(Value::as_bool)
            .unwrap_or(false);
    if changed {
        account
            .save()
            .map_err(|e| format!("persisting account identifiers: {e}"))?;
    }
    let mut profile = device::DeviceProfile::default();
    profile.account = account;
    Ok(profile.to_json())
}

/// android_sim: the multi-country SIM roster — list / scan / configure /
/// activate / deactivate. See [`crate::sim`] for the semantics.
fn sim_tool(args: &Value) -> Result<Value, String> {
    let action = opt_str(args, "action").unwrap_or_else(|| "list".to_string());
    let timeout = Duration::from_millis(
        args.get("timeout_ms")
            .and_then(Value::as_u64)
            .unwrap_or(2500)
            .max(100),
    );
    match action.as_str() {
        "list" | "" => Ok(sim::list_json()),
        "scan" => {
            let results = sim::scan(timeout);
            let online = results.iter().filter(|r| r.online).count();
            let table: Vec<Value> = results
                .iter()
                .map(|r| {
                    let p = sim::find(r.profile_id).expect("probe ids come from the catalog");
                    json!({
                        "id": p.id, "carrier": p.carrier, "country": p.country,
                        "country_name": p.country_name, "plmn": p.plmn(), "network": p.network,
                        "online": r.online, "latency_ms": r.latency_ms, "detail": r.detail,
                    })
                })
                .collect();
            Ok(json!({
                "probed": sim::CATALOG.len(),
                "online": online,
                "offline": sim::CATALOG.len() - online,
                "fastest_online": results.first().filter(|r| r.online).map(|r| r.profile_id),
                "results": table,
                "timeout_ms": timeout.as_millis() as u64,
                "next": "android_sim action=configure to enable the best online profile",
            }))
        }
        "configure" => {
            let country: Option<String> = opt_str(args, "country");
            let carrier: Option<String> = opt_str(args, "carrier");
            let slot = args.get("slot").and_then(Value::as_u64).unwrap_or(1) as u32;
            sim::configure(country.as_deref(), carrier.as_deref(), slot, timeout)
        }
        "activate" => {
            let id = opt_str(args, "id").ok_or_else(|| {
                "action=activate needs the profile id (android_sim action=list)".to_string()
            })?;
            let slot = args.get("slot").and_then(Value::as_u64).unwrap_or(1) as u32;
            sim::activate(&id, slot)
        }
        "deactivate" => {
            let slot = args.get("slot").and_then(Value::as_u64).unwrap_or(1) as u32;
            sim::deactivate(slot)
        }
        other => Err(format!(
            "unknown action {other:?} (expected list | scan | configure | activate | deactivate)"
        )),
    }
}

/// android_screen: get / set / reset the screen geometry.
fn screen_tool(args: &Value) -> Result<Value, String> {
    let target = req(args, "device")?;
    let action = opt_str(args, "action").unwrap_or_else(|| "get".to_string());
    let opts = adb_options_for(opt_str(args, "root").as_deref());
    let mut device = connect_device_opts(target, &opts)?;

    let geometry = match action.as_str() {
        "get" | "" => screen::get_geometry(&mut device).map_err(err)?,
        "set" => {
            let width = args.get("width").and_then(Value::as_u64).map(|w| w as u32);
            let height = args.get("height").and_then(Value::as_u64).map(|h| h as u32);
            let density = args
                .get("density")
                .and_then(Value::as_u64)
                .map(|d| d as u32);
            if width.is_none() && height.is_none() && density.is_none() {
                return Err("action=set needs at least one of: width, height, density".to_string());
            }
            let mut g = screen::get_geometry(&mut device).map_err(err)?;
            if let (Some(w), Some(h)) = (width, height) {
                g = screen::set_size(&mut device, w, h).map_err(err)?;
            } else if width.is_some() || height.is_some() {
                // Half a size is meaningless: keep the other dimension.
                let w = width.unwrap_or(g.width);
                let h = height.unwrap_or(g.height);
                g = screen::set_size(&mut device, w, h).map_err(err)?;
            }
            if let Some(d) = density {
                g = screen::set_density(&mut device, d).map_err(err)?;
            }
            g
        }
        "reset" => screen::reset(&mut device).map_err(err)?,
        other => {
            return Err(format!(
                "unknown action {other:?} (expected get | set | reset)"
            ))
        }
    };

    Ok(json!({
        "device": device.addr.clone(),
        "action": action,
        "geometry": format!("{}x{}", geometry.width, geometry.height),
        "width": geometry.width,
        "height": geometry.height,
        "density": geometry.density,
        "resizable": true,
        "root": {
            "status": device.root_status().as_str(),
            "rooted": device.root_status().is_rooted(),
            "note": "screen overrides apply as the shell user; root is not required for wm size/density",
        },
    }))
}

/// android_screenshot: capture (and optionally rescale) to a file.
fn screenshot_tool(args: &Value) -> Result<Value, String> {
    let target = req(args, "device")?;
    let out = req(args, "out")?;
    let scale = args.get("scale").and_then(Value::as_f64).map(|s| s as f32);
    let width = args.get("width").and_then(Value::as_u64).map(|w| w as u32);
    let opts = adb_options_for(opt_str(args, "root").as_deref());
    let mut device = connect_device_opts(target, &opts)?;
    let src = screen::capture_to_file(&mut device, out, scale, width).map_err(err)?;
    let written = std::fs::metadata(out)
        .map(|m| m.len())
        .map_err(|e| format!("reading back {out}: {e}"))?;
    Ok(json!({
        "device": device.addr.clone(),
        "out": out,
        "source_geometry": format!("{}x{}", src.0, src.1),
        "bytes": written,
        "png_dimensions": screen::png_dimensions(&std::fs::read(out).unwrap_or_default()),
        "root": {
            "status": device.root_status().as_str(),
            "rooted": device.root_status().is_rooted(),
        },
    }))
}

/// android_esim: the live dual-eSIM manager on a device. Everything runs
/// through the device's own `esim` utility over adb shell — the same surface
/// a rooted handset exposes — so list/status are live modem state and
/// register/switch persist on the device.
fn esim_tool(args: &Value) -> Result<Value, String> {
    let target = req(args, "device")?;
    let action = opt_str(args, "action").unwrap_or_else(|| "list".to_string());
    let opts = adb_options_for(opt_str(args, "root").as_deref());
    let mut device = connect_device_opts(target, &opts)?;
    let addr = device.addr.clone();

    let slot = args.get("slot").and_then(Value::as_u64).unwrap_or(1);
    let shell_quote = |v: &str| format!("'{}'", v.replace('\'', r"'\''"));
    let cmd = match action.as_str() {
        "list" | "" => "esim list --json".to_string(),
        "status" => "esim status --json".to_string(),
        "info" => match opt_str(args, "id") {
            Some(id) => format!("esim info {}", shell_quote(&id)),
            None => format!("esim info --slot {slot}"),
        },
        "register" => {
            let id = opt_str(args, "id").ok_or(
                "action=register needs the carrier profile id (see android_sim action=list, e.g. pk-jazz, uk-ee, de-telekom)",
            )?;
            format!("esim register {} --slot {slot}", shell_quote(&id))
        }
        "enable" | "switch" => {
            let id = opt_str(args, "id")
                .ok_or("action=enable/switch needs the profile id or ICCID (android_esim action=list)")?;
            format!("esim switch {} --slot {slot}", shell_quote(&id))
        }
        "disable" => format!("esim disable --slot {slot}"),
        "delete" => {
            let id = opt_str(args, "id")
                .ok_or("action=delete needs the profile id or ICCID (android_esim action=list)")?;
            format!("esim delete {}", shell_quote(&id))
        }
        "nickname" => {
            let id = opt_str(args, "id")
                .ok_or("action=nickname needs the profile id or ICCID")?;
            let name = opt_str(args, "name")
                .ok_or("action=nickname needs the new name")?;
            format!("esim nickname {} {}", shell_quote(&id), shell_quote(&name))
        }
        "data" => format!("esim data --slot {slot}"),
        other => {
            return Err(format!(
                "unknown action {other:?} (expected list | status | info | register | enable | switch | disable | delete | nickname | data)"
            ))
        }
    };
    let out = device.shell(&cmd).map_err(|e| device_error(&addr, e))?;
    let json_out = cmd.contains("--json");
    if json_out {
        let parsed: Value = serde_json::from_str(out.trim())
            .map_err(|e| format!("device esim output was not JSON ({e}): {out}"))?;
        return Ok(json!({ "device": addr, "action": action, "result": parsed }));
    }
    Ok(json!({ "device": addr, "action": action, "result": out.trim() }))
}

/// android_signal: the live per-slot radio state (registration, technology,
/// band, RSRP/bars, roaming, data state, cell identity, Wi-Fi).
fn signal_tool(args: &Value) -> Result<Value, String> {
    let target = req(args, "device")?;
    let opts = adb_options_for(opt_str(args, "root").as_deref());
    let mut device = connect_device_opts(target, &opts)?;
    let addr = device.addr.clone();
    let out = device
        .shell("esim status --json")
        .map_err(|e| device_error(&addr, e))?;
    let parsed: Value = serde_json::from_str(out.trim())
        .map_err(|e| format!("device esim status output was not JSON ({e}): {out}"))?;
    let mut v = parsed;
    if let Some(obj) = v.as_object_mut() {
        obj.insert("device".into(), json!(addr));
    }
    Ok(v)
}

/// A unique scratch directory under the system temp dir.
fn scratch_dir(tag: &str) -> Result<PathBuf, String> {
    let unique = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    let dir = std::env::temp_dir().join(format!("mandroid-{tag}-{}-{unique}", std::process::id()));
    std::fs::create_dir_all(&dir)
        .map_err(|e| format!("creating scratch dir {}: {e}", dir.display()))?;
    Ok(dir)
}

/// Dispatch one tools/call. Errors are returned as tool results with isError so
/// the model sees the reason instead of a transport failure.
pub fn call(name: &str, args: &Value) -> Result<Value, String> {
    let value = match name {
        // -- existing static analysis --------------------------------------
        "android_apk_info" => api::info(req(args, "apk")?).map_err(err)?,
        "android_manifest" => api::manifest(req(args, "apk")?).map_err(err)?,
        "android_scan" => {
            let apk = req(args, "apk")?;
            let min = opt_str(args, "min_severity").unwrap_or_else(|| "info".to_string());
            api::scan(apk, &min).map_err(err)?
        }
        "android_strings" => {
            let apk = req(args, "apk")?;
            let needle = opt_scalar_string(args, "needle").unwrap_or_default();
            let limit = opt_usize(args, "limit", 100);
            api::strings(apk, &needle, limit).map_err(err)?
        }
        "android_rules" => api::rules().map_err(err)?,
        "mandroid_status" => status(),
        // -- acquisition and structure -------------------------------------
        "android_download" => download(args)?,
        "android_decompile" => decompile(args)?,
        "android_resources" => resources(args)?,
        "android_presets" => presets_tool(),
        // -- modify / sign / verify / build / workflow ---------------------
        "android_modify" => modify_tool(args)?,
        "android_sign" => sign_tool(args)?,
        "android_verify" => verify_tool(args)?,
        "android_build" => build_tool(args)?,
        "android_workflow" => workflow_tool(args)?,
        // -- device --------------------------------------------------------
        "android_device_info" => device_info_tool(args)?,
        "android_adb_shell" => adb_shell_tool(args)?,
        "android_adb_push" => adb_push_tool(args)?,
        "android_adb_pull" => adb_pull_tool(args)?,
        "android_install" => install_tool(args)?,
        "android_launch" => launch_tool(args)?,
        "android_adb_root" => adb_root_tool(args)?,
        "android_device_profile" => device_profile_tool(args)?,
        "android_sim" => sim_tool(args)?,
        "android_screen" => screen_tool(args)?,
        "android_screenshot" => screenshot_tool(args)?,
        "android_esim" => esim_tool(args)?,
        "android_signal" => signal_tool(args)?,
        other => return Err(format!("unknown tool: {other}")),
    };
    Ok(value)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The full contract: 25 `android_*` tools plus `mandroid_status`.
    const EXPECTED: &[&str] = &[
        "android_apk_info",
        "android_manifest",
        "android_scan",
        "android_strings",
        "android_rules",
        "mandroid_status",
        "android_download",
        "android_decompile",
        "android_resources",
        "android_presets",
        "android_modify",
        "android_sign",
        "android_verify",
        "android_build",
        "android_workflow",
        "android_device_info",
        "android_adb_shell",
        "android_adb_push",
        "android_adb_pull",
        "android_install",
        "android_launch",
        "android_adb_root",
        "android_device_profile",
        "android_sim",
        "android_screen",
        "android_screenshot",
        "android_esim",
        "android_signal",
    ];

    fn real_apk() -> PathBuf {
        PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("testdata/test-app.apk")
    }

    fn temp_dir(tag: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("mandroid-tools-{tag}-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn call_ok(name: &str, args: Value) -> Value {
        call(name, &args).unwrap_or_else(|e| panic!("{name} failed: {e}"))
    }

    /// The roster is a contract: every expected tool is advertised with a real
    /// description and an object schema, and no unnamed extra appears.
    #[test]
    fn tools_list_advertises_the_full_roster() {
        let tools = tools();
        let names: Vec<&str> = tools.iter().map(|t| t["name"].as_str().unwrap()).collect();

        assert_eq!(
            names.len(),
            EXPECTED.len(),
            "unexpected tool count: {names:?}"
        );
        for expected in EXPECTED {
            assert!(
                names.contains(expected),
                "missing tool {expected}; got {names:?}"
            );
        }
        for t in tools {
            let n = t["name"].as_str().unwrap();
            assert!(
                n.starts_with("android_") || n.starts_with("mandroid_"),
                "{n} is not namespaced"
            );
            let desc = t["description"].as_str().unwrap_or("");
            assert!(desc.len() > 40, "{n} description is too thin: {desc:?}");
            assert_eq!(
                t["inputSchema"]["type"], "object",
                "{n} needs an object inputSchema"
            );
        }
    }

    /// Presets are real edit bundles, not placeholders.
    #[test]
    fn presets_tool_returns_the_real_presets() {
        let v = call_ok("android_presets", json!({}));
        let presets = v["presets"].as_array().unwrap();
        let names: Vec<&str> = presets
            .iter()
            .map(|p| p["name"].as_str().unwrap())
            .collect();
        for expected in ["debug", "cleartext", "expose-all", "backup"] {
            assert!(
                names.contains(&expected),
                "missing preset {expected}: {names:?}"
            );
        }
        for p in presets {
            assert!(!p["description"].as_str().unwrap().is_empty());
            assert!(p["edits"].as_array().unwrap().len() >= 1);
        }
    }

    #[test]
    fn status_reports_setup_sections() {
        let v = call_ok("mandroid_status", json!({}));
        assert!(v["version"].as_str().is_some());
        assert!(v["tools"].as_array().is_some());
        assert!(v["setup"].is_object());
    }

    #[test]
    fn manifest_and_info_round_trip_a_real_apk() {
        let apk = real_apk();
        let info = call_ok(
            "android_apk_info",
            json!({ "apk": apk.display().to_string() }),
        );
        assert_eq!(info["package"].as_str().unwrap(), "org.fdroid.fdroid");
        let m = call_ok(
            "android_manifest",
            json!({ "apk": apk.display().to_string() }),
        );
        assert_eq!(m["package"].as_str().unwrap(), "org.fdroid.fdroid");
    }

    #[test]
    fn unknown_tool_is_an_error() {
        assert!(call("android_nope", &json!({})).is_err());
    }

    #[test]
    fn missing_required_arg_is_an_error() {
        assert!(call("android_scan", &json!({})).is_err());
    }
}
