//! `mandroid` — native Android engine CLI.

use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use clap::{Parser, Subcommand};
use serde_json::{json, Value};

use mandroid::{api, apk::Apk, axml, key, smali, tools};

#[derive(Parser)]
#[command(
    name = "mandroid",
    version,
    about = "Native Android engine: APK analysis + ADB wire protocol"
)]
struct Cli {
    #[command(subcommand)]
    cmd: Cmd,
}

#[derive(Subcommand)]
enum Cmd {
    /// Decode the binary AndroidManifest.xml (no aapt/apktool).
    Manifest {
        apk: String,
        /// Emit the structured model as JSON instead of the raw tree.
        #[arg(long)]
        json: bool,
    },
    /// Summarise the APK container.
    Info {
        apk: String,
        #[arg(long)]
        json: bool,
    },
    /// Static security analysis of an APK.
    Scan {
        apk: String,
        #[arg(long)]
        json: bool,
        /// Only report this severity or worse.
        #[arg(long, default_value = "info")]
        min_severity: String,
    },
    /// Search the DEX string pool.
    Strings {
        apk: String,
        /// Case-insensitive substring (empty lists all).
        #[arg(default_value = "")]
        needle: String,
        #[arg(long, default_value = "100")]
        limit: usize,
    },
    /// List the static-analysis rule pack.
    Rules {
        #[arg(long)]
        json: bool,
    },
    /// Download an APK over http(s) natively (no curl/wget).
    Download {
        url: String,
        /// Destination file or directory (default: the URL's basename).
        #[arg(long)]
        out: Option<String>,
        #[arg(long)]
        json: bool,
    },
    /// Disassemble an APK's DEX to smali (no apktool).
    Decompile {
        apk: String,
        /// Only classes whose smali name contains this substring.
        #[arg(long)]
        class: Option<String>,
        /// Max classes to print (default 3, or 10 when --class is given).
        #[arg(long)]
        limit: Option<usize>,
        /// Write one .smali file per class into this directory instead of printing.
        #[arg(long)]
        out: Option<String>,
        #[arg(long)]
        json: bool,
    },
    /// Decode resources.arsc: app label, packages, and resolved strings.
    Resources {
        apk: String,
        /// Resolve one resource id, e.g. 0x7f110045 or decimal.
        #[arg(long)]
        query: Option<String>,
        #[arg(long)]
        json: bool,
    },
    /// List the named manifest-edit presets.
    Presets {
        #[arg(long)]
        json: bool,
    },
    /// Apply manifest edits to an APK, writing a new unsigned APK.
    Modify {
        apk: String,
        /// Path to write the modified, unsigned APK.
        #[arg(long)]
        out: String,
        /// Preset bundle name (see `mandroid presets`).
        #[arg(long)]
        preset: Option<String>,
        /// Compact edit spec, e.g. debuggable=true (repeatable).
        #[arg(long = "edit")]
        edit: Vec<String>,
        #[arg(long)]
        json: bool,
    },
    /// Sign an APK with a stable local identity.
    Sign {
        apk: String,
        /// Path to write the signed APK.
        #[arg(long)]
        out: String,
        /// Certificate common name.
        #[arg(long, default_value = "mandroid")]
        cn: String,
        /// Directory holding the signing key/cert.
        #[arg(long)]
        keystore: Option<String>,
        /// Disable the v1 (JAR) signature scheme.
        #[arg(long)]
        no_v1: bool,
        /// Disable the v2 signature scheme.
        #[arg(long)]
        no_v2: bool,
        #[arg(long)]
        json: bool,
    },
    /// Verify an APK's v1/v2 signatures.
    Verify {
        apk: String,
        #[arg(long)]
        json: bool,
    },
    /// Modify then sign in one step (the common case).
    Build {
        apk: String,
        /// Path to write the modified, signed APK.
        #[arg(long)]
        out: String,
        /// Preset bundle name (see `mandroid presets`).
        #[arg(long)]
        preset: Option<String>,
        /// Compact edit spec (repeatable).
        #[arg(long = "edit")]
        edit: Vec<String>,
        #[arg(long)]
        json: bool,
    },
    /// Run the full acquire -> analyze -> modify -> sign -> verify pipeline.
    Workflow {
        /// https URL or local path to the APK.
        source: String,
        /// Directory for intermediates and the signed APK.
        #[arg(long)]
        work_dir: Option<String>,
        #[arg(long)]
        preset: Option<String>,
        #[arg(long = "edit")]
        edit: Vec<String>,
        /// Device host[:port]; omit to stop after signing.
        #[arg(long)]
        device: Option<String>,
        /// Install the result on the device.
        #[arg(long)]
        install: bool,
        /// Launch the package after install.
        #[arg(long)]
        launch: bool,
        #[arg(long)]
        json: bool,
    },
    /// Talk to a device's adbd over TCP (no adb binary).
    Device {
        /// Device host, host:port, [v6]:port or tcp: URL.
        target: String,
        /// Run a shell command.
        #[arg(long)]
        shell: Option<String>,
        /// Push a local file: LOCAL:REMOTE.
        #[arg(long)]
        push: Option<String>,
        /// Pull a remote file: REMOTE:LOCAL.
        #[arg(long)]
        pull: Option<String>,
        /// Install this APK on the device.
        #[arg(long)]
        install: Option<String>,
        /// Launch this package or component (com.pkg or com.pkg/.Activity).
        #[arg(long)]
        launch: Option<String>,
        /// Root policy for this invocation: auto (default, rooted when
        /// possible) | on (require root) | off (plain shell).
        #[arg(long, default_value = "auto")]
        root: String,
        /// Report (status) / re-escalate (enable) or drop (disable) root, then exit.
        #[arg(long)]
        root_action: Option<String>,
        #[arg(long)]
        json: bool,
    },
    /// Show or adjust the virtual device hardware profile (16 GB RAM / 1 TB
    /// ROM, CPU matched to the host, dual eSIM, out-of-the-box apps, account).
    Profile {
        /// Provision the account e-mail (identifier only; never a secret).
        #[arg(long)]
        set_account: Option<String>,
        /// Store an alternate/recovery e-mail alongside the account.
        #[arg(long)]
        set_alt_account: Option<String>,
        /// Remove the provisioned account identifiers.
        #[arg(long)]
        clear_account: bool,
        #[arg(long)]
        json: bool,
    },
    /// Manage the SIM roster: many ready eSIM profiles across 12+ countries.
    Sim {
        /// list (default) | scan | configure | activate | deactivate.
        #[arg(default_value = "list")]
        action: String,
        /// Country filter for configure ('US', 'Pakistan', ...) or profile id
        /// for activate.
        #[arg(long)]
        country: Option<String>,
        /// Carrier filter for configure (e.g. 'jazz').
        #[arg(long)]
        carrier: Option<String>,
        /// Slot for configure/activate/deactivate (1 or 2).
        #[arg(long)]
        slot: Option<u32>,
        /// Profile id for activate (e.g. pk-jazz).
        #[arg(long)]
        id: Option<String>,
        /// Per-probe budget in ms for scan/configure.
        #[arg(long, default_value = "2500")]
        timeout_ms: u64,
        #[arg(long)]
        json: bool,
    },
    /// Show or adjust a device's screen: resolution + density, and capture it.
    Screen {
        /// Device host, host:port, [v6]:port or tcp: URL.
        #[arg(long)]
        target: String,
        /// get (default) | set | reset.
        #[arg(long, default_value = "get")]
        action: String,
        /// Width in px (action=set).
        #[arg(long)]
        width: Option<u32>,
        /// Height in px (action=set).
        #[arg(long)]
        height: Option<u32>,
        /// Density in dpi (action=set).
        #[arg(long)]
        density: Option<u32>,
        /// Capture a screenshot to this .png path.
        #[arg(long)]
        screenshot: Option<String>,
        /// Screenshot scale in (0,1].
        #[arg(long)]
        scale: Option<f32>,
        #[arg(long)]
        json: bool,
    },
    /// Show or generate the ADB public key (the private key is never printed).
    AdbKey {
        /// Key directory (default ~/.android).
        #[arg(long)]
        dir: Option<String>,
        /// Generate a fresh key, overwriting any existing one.
        #[arg(long)]
        generate: bool,
        #[arg(long)]
        json: bool,
    },
    /// Engine status: version, tool roster, rule pack.
    Status,
    /// Invoke one tool by id, JSON args on stdin. Same contract as burp-rs and
    /// roma-327, so the agent bridge is uniform across engines.
    Call {
        /// Tool id, e.g. android_scan.
        tool: String,
    },
}

fn main() -> Result<()> {
    // Unix pipelines close early (`mandroid --help | head`); restore the
    // default SIGPIPE disposition so that is a clean exit, not a panic.
    #[cfg(unix)]
    unsafe {
        libc::signal(libc::SIGPIPE, libc::SIG_DFL);
    }
    let cli = Cli::parse();
    match cli.cmd {
        Cmd::Manifest { apk, json } => {
            if json {
                println!("{}", serde_json::to_string_pretty(&api::manifest(&apk)?)?);
            } else {
                let mut a = Apk::open(&apk)?;
                print_tree(&a.manifest()?, 0);
            }
        }
        Cmd::Info { apk, json } => {
            let v = api::info(&apk)?;
            if json {
                println!("{}", serde_json::to_string_pretty(&v)?);
            } else {
                println!("apk:        {apk}");
                println!("entries:    {}", v["entries"]);
                println!("package:    {}", v["package"]);
                println!(
                    "minSdk:     {}  targetSdk: {}",
                    v["min_sdk"], v["target_sdk"]
                );
                println!("components: {}", v["components"]);
                println!("abis:       {}", v["abis"]);
                let dex: Vec<String> = v["dex"]
                    .as_array()
                    .map(|a| {
                        a.iter()
                            .map(|d| d["file"].as_str().unwrap_or("").to_string())
                            .collect()
                    })
                    .unwrap_or_default();
                println!("dex:        {}", dex.join(", "));
            }
        }
        Cmd::Scan {
            apk,
            json,
            min_severity,
        } => {
            let v = api::scan(&apk, &min_severity)?;
            if json {
                println!("{}", serde_json::to_string_pretty(&v)?);
            } else {
                println!("{}  ({apk})", v["package"].as_str().unwrap_or("?"));
                println!(
                    "  minSdk={} targetSdk={}  components={}",
                    v["min_sdk"],
                    v["target_sdk"],
                    v["components"].as_array().map(|a| a.len()).unwrap_or(0)
                );
                let c = &v["counts"];
                println!(
                    "  {} finding(s): {} critical, {} high, {} medium, {} low\n",
                    c["total"], c["critical"], c["high"], c["medium"], c["low"]
                );
                for f in v["findings"].as_array().cloned().unwrap_or_default() {
                    println!(
                        "  [{}] {} ({})",
                        f["severity"].as_str().unwrap_or("").to_uppercase(),
                        f["title"].as_str().unwrap_or(""),
                        f["id"].as_str().unwrap_or("")
                    );
                    println!("      subject:  {}", f["subject"].as_str().unwrap_or(""));
                    if let Some(cwe) = f["cwe"].as_str() {
                        println!("      cwe:      {cwe}");
                    }
                    println!("      evidence: {}", f["evidence"].as_str().unwrap_or(""));
                }
            }
        }
        Cmd::Strings { apk, needle, limit } => {
            let v = api::strings(&apk, &needle, limit)?;
            println!(
                "{} strings total, {} matched, showing {}",
                v["total_strings"], v["matched"], v["returned"]
            );
            for s in v["strings"].as_array().cloned().unwrap_or_default() {
                println!("  {}", s.as_str().unwrap_or(""));
            }
            for p in v["secret_patterns_hit"]
                .as_array()
                .cloned()
                .unwrap_or_default()
            {
                println!(
                    "  ! secret pattern matched: {} ({})",
                    p["title"], p["severity"]
                );
            }
        }
        Cmd::Rules { json } => {
            let v = api::rules()?;
            if json {
                println!("{}", serde_json::to_string_pretty(&v)?);
            } else {
                println!(
                    "rules v{} — {} rules, {} secret patterns",
                    v["version"], v["rule_count"], v["secret_pattern_count"]
                );
                for r in v["rules"].as_array().cloned().unwrap_or_default() {
                    println!(
                        "  [{:>8}] {} ({})",
                        r["severity"].as_str().unwrap_or(""),
                        r["title"].as_str().unwrap_or(""),
                        r["id"].as_str().unwrap_or("")
                    );
                }
            }
        }
        Cmd::Download { url, out, json } => {
            let mut args = json!({ "url": url });
            if let Some(out) = out {
                args["dest"] = json!(out);
            }
            let v = invoke("android_download", args)?;
            if json {
                print_json(&v)?;
            } else {
                println!("downloaded {}", v["path"].as_str().unwrap_or("?"));
                println!("  final url: {}", v["final_url"].as_str().unwrap_or(""));
                println!("  bytes:     {}", v["bytes"]);
                println!("  sha256:    {}", v["sha256"].as_str().unwrap_or(""));
                println!("  content:   {}", v["content_type"].as_str().unwrap_or("-"));
                println!("  apk/zip:   {}", v["looks_like_apk"]);
            }
        }
        Cmd::Decompile {
            apk,
            class,
            limit,
            out,
            json,
        } => {
            if let Some(dir) = out {
                let classes =
                    smali::disassemble_apk(&apk).with_context(|| format!("disassembling {apk}"))?;
                let needle = class.as_deref().map(str::to_ascii_lowercase);
                let selected: Vec<&smali::SmaliClass> = classes
                    .iter()
                    .filter(|c| match &needle {
                        Some(n) => c.name.to_ascii_lowercase().contains(n),
                        None => true,
                    })
                    .collect();
                std::fs::create_dir_all(&dir).with_context(|| format!("creating {dir}"))?;
                for c in &selected {
                    let file = Path::new(&dir).join(format!("{}.smali", c.name.replace('/', ".")));
                    std::fs::write(&file, &c.source)
                        .with_context(|| format!("writing {}", file.display()))?;
                }
                println!(
                    "{} classes total, wrote {} smali file(s) to {dir}",
                    classes.len(),
                    selected.len()
                );
            } else {
                let mut args = json!({ "apk": apk });
                if let Some(class) = class {
                    args["class"] = json!(class);
                }
                if let Some(limit) = limit {
                    args["limit"] = json!(limit);
                }
                let v = invoke("android_decompile", args)?;
                if json {
                    print_json(&v)?;
                } else {
                    println!(
                        "{} classes, {} lines total; {} match; showing {}",
                        v["total_classes"],
                        v["total_lines"],
                        v["matched_classes"],
                        v["returned_classes"]
                    );
                    for c in v["classes"].as_array().cloned().unwrap_or_default() {
                        println!(
                            "\n// ---- {} ({} lines) ----",
                            c["class"].as_str().unwrap_or(""),
                            c["lines"]
                        );
                        println!("{}", c["smali"].as_str().unwrap_or(""));
                    }
                    if let Some(note) = v["note"].as_str() {
                        println!("\n{note}");
                    }
                }
            }
        }
        Cmd::Resources { apk, query, json } => {
            let mut args = json!({ "apk": apk });
            if let Some(query) = query {
                args["query"] = json!(query);
            }
            let v = invoke("android_resources", args)?;
            if json {
                print_json(&v)?;
            } else if v["query"].is_string() {
                println!(
                    "app label: {}",
                    v["app_label"].as_str().unwrap_or("(unresolved)")
                );
                println!(
                    "{} -> {}",
                    v["resource_id"].as_str().unwrap_or("?"),
                    if v["found"].as_bool().unwrap_or(false) {
                        v["value"].as_str().unwrap_or("").to_string()
                    } else {
                        "(not found)".to_string()
                    }
                );
            } else {
                println!(
                    "app label:    {}",
                    v["app_label"].as_str().unwrap_or("(unresolved)")
                );
                println!("packages:     {}", v["package_count"]);
                println!("entries:      {}", v["entry_count"]);
                for p in v["packages"].as_array().cloned().unwrap_or_default() {
                    println!(
                        "  package {} ({})",
                        p["name"].as_str().unwrap_or(""),
                        p["id"].as_str().unwrap_or("")
                    );
                    for t in p["types"].as_array().cloned().unwrap_or_default() {
                        println!(
                            "    {:<14} #{:<3} {} entries in {} config(s)",
                            t["type"].as_str().unwrap_or(""),
                            t["id"],
                            t["entries"],
                            t["configs"]
                        );
                    }
                }
                let samples = v["sample_values"].as_array().cloned().unwrap_or_default();
                println!(
                    "\n{} of {} resolvable value(s):",
                    v["sample_values_returned"], v["sample_values_total"]
                );
                for sv in samples {
                    println!(
                        "  {} {} {} = {}",
                        sv["id"].as_str().unwrap_or(""),
                        sv["type"].as_str().unwrap_or(""),
                        sv["key"].as_str().unwrap_or(""),
                        sv["value"].as_str().unwrap_or("")
                    );
                }
            }
        }
        Cmd::Presets { json } => {
            let v = invoke("android_presets", json!({}))?;
            if json {
                print_json(&v)?;
            } else {
                for p in v["presets"].as_array().cloned().unwrap_or_default() {
                    println!(
                        "{}  ({} edit{})",
                        p["name"].as_str().unwrap_or(""),
                        p["edit_count"],
                        if p["edit_count"] == 1 { "" } else { "s" }
                    );
                    println!("    {}", p["description"].as_str().unwrap_or(""));
                    for e in p["edits"].as_array().cloned().unwrap_or_default() {
                        println!("      - {}", e.as_str().unwrap_or(""));
                    }
                }
            }
        }
        Cmd::Modify {
            apk,
            out,
            preset,
            edit,
            json,
        } => {
            let v = invoke("android_modify", edit_args(apk, out, preset, edit))?;
            if json {
                print_json(&v)?;
            } else {
                print_changes(&v);
                println!("unsigned APK: {}", v["out"].as_str().unwrap_or(""));
                println!("bytes:        {}", v["bytes"]);
            }
        }
        Cmd::Sign {
            apk,
            out,
            cn,
            keystore,
            no_v1,
            no_v2,
            json,
        } => {
            let mut args = json!({
                "apk": apk,
                "out": out,
                "cn": cn,
                "v1": !no_v1,
                "v2": !no_v2,
            });
            if let Some(keystore) = keystore {
                args["keystore_dir"] = json!(keystore);
            }
            let v = invoke("android_sign", args)?;
            if json {
                print_json(&v)?;
            } else {
                println!("signed {}", v["out"].as_str().unwrap_or(""));
                println!("  signer:    {}", v["signer"].as_str().unwrap_or(""));
                println!("  v1: {}  v2: {}", v["v1"], v["v2"]);
                println!("  keystore:  {}", v["keystore_dir"].as_str().unwrap_or(""));
                println!(
                    "  entries:   {}  (removed {} old signature files)",
                    v["entries"], v["removed_signatures"]
                );
                println!("  bytes:     {}", v["bytes"]);
                println!("  sha256:    {}", v["sha256"].as_str().unwrap_or(""));
            }
        }
        Cmd::Verify { apk, json } => {
            let v = invoke("android_verify", json!({ "apk": apk }))?;
            if json {
                print_json(&v)?;
            } else {
                println!(
                    "{}: {}",
                    v["apk"].as_str().unwrap_or(""),
                    v["verdict"].as_str().unwrap_or("?")
                );
                println!(
                    "  v1: present={} signed_entries={}",
                    v["v1"]["present"], v["v1"]["signed_entries"]
                );
                println!(
                    "  v2: present={} signers={} algorithm={}",
                    v["v2"]["present"], v["v2"]["signers"], v["v2"]["algorithm"]
                );
                println!(
                    "      digest_ok={} signature_ok={}",
                    v["v2"]["digest_ok"], v["v2"]["signature_ok"]
                );
                if let Some(sha) = v["v2"]["certificate_sha256"].as_str() {
                    println!("      cert sha256: {sha}");
                }
                for e in v["v2"]["errors"].as_array().cloned().unwrap_or_default() {
                    println!("      error: {}", e.as_str().unwrap_or(""));
                }
            }
        }
        Cmd::Build {
            apk,
            out,
            preset,
            edit,
            json,
        } => {
            let v = invoke("android_build", edit_args(apk, out, preset, edit))?;
            if json {
                print_json(&v)?;
            } else {
                print_changes(&v);
                println!(
                    "built {}  (valid={})",
                    v["out"].as_str().unwrap_or(""),
                    v["valid"]
                );
                println!("  signer: {}", v["signer"].as_str().unwrap_or(""));
                println!("  bytes:  {}", v["bytes"]);
                println!("  sha256: {}", v["sha256"].as_str().unwrap_or(""));
            }
        }
        Cmd::Workflow {
            source,
            work_dir,
            preset,
            edit,
            device,
            install,
            launch,
            json,
        } => {
            let mut args = json!({ "source": source });
            if let Some(dir) = work_dir {
                args["work_dir"] = json!(dir);
            }
            if let Some(preset) = preset {
                args["preset"] = json!(preset);
            }
            if !edit.is_empty() {
                args["edits"] = json!(edit);
            }
            if let Some(device) = device {
                args["device"] = json!(device);
            }
            if install {
                args["install"] = json!(true);
            }
            if launch {
                args["launch"] = json!(true);
            }
            let v = invoke("android_workflow", args)?;
            if json {
                print_json(&v)?;
            } else {
                print!("{}", v["rendered"].as_str().unwrap_or(""));
                println!("signed APK: {}", v["apk"].as_str().unwrap_or(""));
                if !v["succeeded"].as_bool().unwrap_or(false) {
                    anyhow::bail!("workflow did not complete successfully");
                }
            }
        }
        Cmd::Device {
            target,
            shell,
            push,
            pull,
            install,
            launch,
            root,
            root_action,
            json,
        } => {
            if let Some(action) = root_action {
                let v = invoke(
                    "android_adb_root",
                    json!({ "device": target, "action": action, "root": root }),
                )?;
                if json {
                    print_json(&v)?;
                } else {
                    let r = &v["root"];
                    println!(
                        "root: {} (uid {}) — {}",
                        r["status"].as_str().unwrap_or("?"),
                        r["uid"].as_str().unwrap_or("?"),
                        r["note"].as_str().unwrap_or("")
                    );
                }
                return Ok(());
            }
            device_command(&target, shell, push, pull, install, launch, &root, json)?;
        }
        Cmd::Profile {
            set_account,
            set_alt_account,
            clear_account,
            json,
        } => {
            let mut args = json!({});
            if let Some(a) = &set_account {
                args["set_account"] = json!(a);
            }
            if let Some(a) = &set_alt_account {
                args["set_alt_account"] = json!(a);
            }
            if clear_account {
                args["clear_account"] = json!(true);
            }
            let v = invoke("android_device_profile", args)?;
            if json {
                println!("{}", serde_json::to_string_pretty(&v)?);
            } else {
                let acct = &v["account"];
                println!("device profile: {}", v["name"].as_str().unwrap_or("?"));
                println!(
                    "  ram:     {} ({})",
                    v["ram"]["spec"].as_str().unwrap_or("?"),
                    v["ram"]["gb"],
                );
                println!(
                    "  rom:     {} ({} GiB usable)",
                    v["rom"]["spec"].as_str().unwrap_or("?"),
                    v["rom"]["gib"],
                );
                println!(
                    "  cpu:     {} core(s), {} ({})",
                    v["cpu"]["cores"],
                    v["cpu"]["arch"].as_str().unwrap_or("?"),
                    v["cpu"]["soc"].as_str().unwrap_or("?")
                );
                println!(
                    "  serial:  {}  IMEI {} / {}",
                    v["identifiers"]["serial"], v["identifiers"]["imei"], v["identifiers"]["imei2"]
                );
                println!(
                    "  eid:     {}  baseband {}",
                    v["identifiers"]["eid"], v["identifiers"]["baseband"]
                );
                println!(
                    "  screen:  {} @ {}dpi (resizable)",
                    v["screen"]["geometry"].as_str().unwrap_or("?"),
                    v["screen"]["density"]
                );
                println!(
                    "  esim:    dual eSIM, {} slots, {} profile(s) registered, {} catalog profiles",
                    v["esim"]["slot_count"],
                    v["esim"]["profiles_registered"],
                    v["esim"]["profile_catalog"]
                );
                let s1 = &v["esim"]["slots"][0];
                let s2 = &v["esim"]["slots"][1];
                let slot_line = |s: &Value| {
                    if s["state"] == "enabled" {
                        let p = &s["profile"];
                        format!(
                            "{} ({}, ICCID {}, line {})",
                            p["carrier"].as_str().unwrap_or("?"),
                            p["state"].as_str().unwrap_or("?"),
                            p["iccid"].as_str().unwrap_or("?"),
                            p["msisdn"].as_str().unwrap_or("?")
                        )
                    } else {
                        "(empty)".to_string()
                    }
                };
                println!("  sim 1:   {}", slot_line(s1));
                println!("  sim 2:   {}", slot_line(s2));
                println!(
                    "  apps:    {} preinstalled (like new phone, incl. Play Store)",
                    v["apps"]["count"]
                );
                let email = acct["email"].as_str().unwrap_or("(none)");
                let alt = acct["alternate_email"].as_str().unwrap_or("(none)");
                println!("  account: {email} (alt: {alt}) — identifiers only, password via MANDROID_ACCOUNT_PASSWORD");
            }
        }
        Cmd::Sim {
            action,
            country,
            carrier,
            slot,
            id,
            timeout_ms,
            json,
        } => {
            let mut args = json!({ "action": action, "timeout_ms": timeout_ms });
            if let Some(c) = &country {
                args["country"] = json!(c);
            }
            if let Some(c) = &carrier {
                args["carrier"] = json!(c);
            }
            if let Some(s) = slot {
                args["slot"] = json!(s);
            }
            if let Some(i) = &id {
                args["id"] = json!(i);
            }
            let v = invoke("android_sim", args)?;
            if json {
                println!("{}", serde_json::to_string_pretty(&v)?);
            } else if v["sims"].is_array() {
                // list view
                let s1 = &v["slots"]["slot1"];
                let s2 = &v["slots"]["slot2"];
                let name = |s: &Value| {
                    if s["state"] == "enabled" {
                        format!(
                            "{} ({})",
                            s["profile"]["id"].as_str().unwrap_or("?"),
                            s["profile"]["carrier"].as_str().unwrap_or("")
                        )
                    } else {
                        "(empty)".into()
                    }
                };
                println!(
                    "SIM roster: {} profiles, {} countries (dual eSIM)",
                    v["catalog_size"], v["countries"]
                );
                println!("  slot 1: {}", name(s1));
                println!("  slot 2: {}", name(s2));
                for s in v["sims"].as_array().cloned().unwrap_or_default() {
                    let slot_mark = if s["slot"].as_u64().unwrap_or(0) > 0 {
                        format!(" [slot {}]", s["slot"])
                    } else {
                        String::new()
                    };
                    println!(
                        "  {:<18} {:<18} {:<3} {}{}",
                        s["id"].as_str().unwrap_or(""),
                        s["carrier"].as_str().unwrap_or(""),
                        s["country"].as_str().unwrap_or(""),
                        s["plmn"].as_str().unwrap_or(""),
                        slot_mark
                    );
                }
            } else if v["scan"]["results"].is_array() {
                // configure view
                let c = &v["configured"];
                println!(
                    "configured slot {} -> {} ({}) @ {}ms",
                    c["slot"],
                    c["profile"]["id"].as_str().unwrap_or("?"),
                    c["profile"]["carrier"].as_str().unwrap_or(""),
                    c["latency_ms"]
                );
                if v["second_slot"]["profile"]["id"].is_string() {
                    println!(
                        "configured slot 2 -> {} ({})",
                        v["second_slot"]["profile"]["id"].as_str().unwrap_or(""),
                        v["second_slot"]["profile"]["carrier"]
                            .as_str()
                            .unwrap_or("")
                    );
                }
                println!(
                    "scan: {} online / {} probed",
                    v["scan"]["online"], v["scan"]["probed"]
                );
            } else if v["results"].is_array() {
                // scan view
                println!(
                    "scan: {} online / {} probed (fastest: {})\n",
                    v["online"],
                    v["probed"],
                    v["fastest_online"].as_str().unwrap_or("-")
                );
                for r in v["results"].as_array().cloned().unwrap_or_default() {
                    if r["online"].as_bool().unwrap_or(false) {
                        println!(
                            "  [online  {:>4}ms] {:<18} {:<18} {}",
                            r["latency_ms"],
                            r["id"].as_str().unwrap_or(""),
                            r["carrier"].as_str().unwrap_or(""),
                            r["country"].as_str().unwrap_or("")
                        );
                    } else {
                        println!(
                            "  [offline      ] {:<18} {:<18} {}",
                            r["id"].as_str().unwrap_or(""),
                            r["carrier"].as_str().unwrap_or(""),
                            r["country"].as_str().unwrap_or("")
                        );
                    }
                }
            } else {
                println!("{}", serde_json::to_string_pretty(&v)?);
            }
        }
        Cmd::Screen {
            target,
            action,
            width,
            height,
            density,
            screenshot,
            scale,
            json,
        } => {
            if let Some(out) = screenshot {
                let mut args = json!({ "device": target, "out": out });
                if let Some(s) = scale {
                    args["scale"] = json!(s);
                }
                let v = invoke("android_screenshot", args)?;
                println!("{}", serde_json::to_string_pretty(&v)?);
                return Ok(());
            }
            let mut args = json!({ "device": target, "action": action });
            if let Some(w) = width {
                args["width"] = json!(w);
            }
            if let Some(h) = height {
                args["height"] = json!(h);
            }
            if let Some(d) = density {
                args["density"] = json!(d);
            }
            let v = invoke("android_screen", args)?;
            if json {
                println!("{}", serde_json::to_string_pretty(&v)?);
            } else {
                println!("screen: {:?} @ {}dpi", v["geometry"], v["density"]);
                let r = &v["root"];
                println!(
                    "root: {} ({})",
                    r["status"].as_str().unwrap_or("?"),
                    r["note"].as_str().unwrap_or("")
                );
            }
        }
        Cmd::AdbKey {
            dir,
            generate,
            json,
        } => {
            let dir = dir.map(PathBuf::from).unwrap_or_else(key::default_dir);
            let adb = if generate {
                key::generate_at(&dir)?
            } else {
                key::load_or_generate(&dir)?
            };
            // Public material only — the private key is never rendered.
            let public_text = adb.public_text()?;
            if json {
                print_json(&json!({
                    "private_key": adb.private_path.display().to_string(),
                    "public_key": adb.public_path.display().to_string(),
                    "public_key_text": public_text,
                }))?;
            } else {
                println!("private key: {}", adb.private_path.display());
                println!("public key:  {}", adb.public_path.display());
                println!("public key text (add to the device's adb_keys):");
                println!("{public_text}");
            }
        }
        Cmd::Status => {
            println!("{}", serde_json::to_string_pretty(&tools::status())?);
        }
        Cmd::Call { tool } => {
            let mut input = String::new();
            std::io::Read::read_to_string(&mut std::io::stdin(), &mut input)?;
            let args: Value = if input.trim().is_empty() {
                json!({})
            } else {
                serde_json::from_str(&input)?
            };
            match tools::call(&tool, &args) {
                Ok(v) => println!(
                    "{}",
                    serde_json::to_string(&json!({"ok": true, "result": v}))?
                ),
                Err(e) => {
                    println!(
                        "{}",
                        serde_json::to_string(&json!({"ok": false, "error": e}))?
                    );
                    std::process::exit(1);
                }
            }
        }
    }
    Ok(())
}

/// Call one native tool and surface its error as an `anyhow` error.
fn invoke(tool: &str, args: Value) -> Result<Value> {
    tools::call(tool, &args).map_err(|e| anyhow::anyhow!(e))
}

fn print_json(v: &Value) -> Result<()> {
    println!("{}", serde_json::to_string_pretty(v)?);
    Ok(())
}

/// Build the shared `{apk, out, preset, edits}` argument object.
fn edit_args(apk: String, out: String, preset: Option<String>, edit: Vec<String>) -> Value {
    let mut args = json!({ "apk": apk, "out": out });
    if let Some(preset) = preset {
        args["preset"] = json!(preset);
    }
    if !edit.is_empty() {
        args["edits"] = json!(edit);
    }
    args
}

fn print_changes(v: &Value) {
    let changes = v["changes"].as_array().cloned().unwrap_or_default();
    println!("{} change(s):", changes.len());
    for c in changes {
        println!("  - {}", c.as_str().unwrap_or(""));
    }
}

/// `mandroid device` — info by default, or the requested actions in order.
#[allow(clippy::too_many_arguments)]
fn device_command(
    target: &str,
    shell: Option<String>,
    push: Option<String>,
    pull: Option<String>,
    install: Option<String>,
    launch: Option<String>,
    root: &str,
    json: bool,
) -> Result<()> {
    let mut acted = false;

    if let Some(command) = shell {
        acted = true;
        let v = invoke(
            "android_adb_shell",
            json!({ "device": target, "command": command, "root": root }),
        )?;
        if json {
            print_json(&v)?;
        } else {
            print!("{}", v["output"].as_str().unwrap_or(""));
            if !v["output"].as_str().unwrap_or("").ends_with('\n') {
                println!();
            }
        }
    }
    if let Some(spec) = push {
        acted = true;
        let (local, remote) = split_pair(&spec, "LOCAL:REMOTE")?;
        let v = invoke(
            "android_adb_push",
            json!({ "device": target, "local": local, "remote": remote, "root": root }),
        )?;
        if json {
            print_json(&v)?;
        } else {
            println!("pushed {local} -> {remote} ({} bytes)", v["bytes"]);
        }
    }
    if let Some(spec) = pull {
        acted = true;
        let (remote, local) = split_pair(&spec, "REMOTE:LOCAL")?;
        let v = invoke(
            "android_adb_pull",
            json!({ "device": target, "remote": remote, "local": local, "root": root }),
        )?;
        if json {
            print_json(&v)?;
        } else {
            println!("pulled {remote} -> {local} ({} bytes)", v["bytes"]);
        }
    }
    if let Some(apk) = install {
        acted = true;
        let v = invoke(
            "android_install",
            json!({ "device": target, "apk": apk, "root": root }),
        )?;
        if json {
            print_json(&v)?;
        } else {
            println!(
                "install {}: {} on {} (sdk {})",
                if v["success"].as_bool().unwrap_or(false) {
                    "ok"
                } else {
                    "FAILED"
                },
                v["apk"].as_str().unwrap_or(""),
                v["model"].as_str().unwrap_or("?"),
                v["sdk"]
            );
            let out = v["output"].as_str().unwrap_or("");
            if !out.is_empty() {
                println!("{out}");
            }
        }
    }
    if let Some(component) = launch {
        acted = true;
        let v = invoke(
            "android_launch",
            json!({ "device": target, "component": component, "root": root }),
        )?;
        if json {
            print_json(&v)?;
        } else {
            println!("launched {}", v["component"].as_str().unwrap_or(""));
            let out = v["output"].as_str().unwrap_or("");
            if !out.is_empty() {
                println!("{out}");
            }
        }
    }

    if !acted {
        let v = invoke("android_device_info", json!({ "device": target }))?;
        if json {
            print_json(&v)?;
        } else {
            println!("device:   {}", v["device"].as_str().unwrap_or(""));
            println!("model:    {}", v["model"].as_str().unwrap_or(""));
            println!("sdk:      {}", v["sdk"]);
            println!("banner:   {}", v["banner"].as_str().unwrap_or(""));
            let features: Vec<String> = v["features"]
                .as_array()
                .map(|a| {
                    a.iter()
                        .map(|f| f.as_str().unwrap_or("").to_string())
                        .collect()
                })
                .unwrap_or_default();
            println!("features: {}", features.join(", "));
            println!("getprop ({} properties):", v["getprop_count"]);
            for p in v["getprop_highlights"]
                .as_array()
                .cloned()
                .unwrap_or_default()
            {
                println!(
                    "  {:<34} {}",
                    p["key"].as_str().unwrap_or(""),
                    p["value"].as_str().unwrap_or("")
                );
            }
        }
    }
    Ok(())
}

/// Split `LEFT:RIGHT`, rejecting a missing half.
fn split_pair(spec: &str, what: &str) -> Result<(String, String)> {
    let (a, b) = spec
        .split_once(':')
        .filter(|(a, b)| !a.is_empty() && !b.is_empty())
        .with_context(|| format!("expected {what}, got {spec:?}"))?;
    Ok((a.to_string(), b.to_string()))
}

fn print_tree(n: &axml::Node, depth: usize) {
    let pad = "  ".repeat(depth);
    let attrs: Vec<String> = n
        .attrs
        .iter()
        .map(|a| format!("{}={:?}", a.name, a.value.display()))
        .collect();
    if attrs.is_empty() {
        println!("{pad}<{}>", n.name);
    } else {
        println!("{pad}<{} {}>", n.name, attrs.join(" "));
    }
    for c in &n.children {
        print_tree(c, depth + 1);
    }
}
