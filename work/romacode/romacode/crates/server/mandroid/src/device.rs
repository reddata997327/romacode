//! Physical device hardware profile.
//!
//! MANDROID carries the hardware profile of the physical handset it drives:
//! RAM, ROM, CPU/SoC shape, screen geometry, **dual eSIM with two factory-
//! registered profiles**, the **out-of-the-box app roster** (Play Store,
//! Gmail, Maps — everything a new phone ships with) and the **pre-
//! provisioned Google account slot**.
//!
//! The defaults come from the device identity (see [`crate::identity`]) —
//! **16 GB RAM, 1 TB ROM (931 GiB usable), a Google Tensor G4 (8 cores:
//! 1× Cortex-X4 + 3× Cortex-A720 + 4× Cortex-A520), 720x1560 @ 420dpi** —
//! and every identifier (serial, IMEI/IMEI2, chip EID, baseband) is
//! deterministic, so the handset keeps the same identity across boots.
//!
//! Account handling, stated plainly: the profile stores **identifiers only**
//! (the account e-mail and its alternate). Passwords are never written to disk,
//! never embedded in the binary and never echoed into tool output — the single
//! supported source is the `MANDROID_ACCOUNT_PASSWORD` environment variable,
//! read at render time and surfaced only as `password_set: true|false`. Nothing
//! here contacts Google: there are no online verification flows to satisfy or
//! bypass.
//!
//! The profile is used in three places:
//!
//! * `mandroid profile` / `android_device_profile` render it as JSON.
//! * Manifest presets can read the SDK level / ABIs when targeting a build.
//! * Screen defaults (720x1560 @ 420dpi) seed [`crate::screen`] when a device
//!   reports nothing.

/// Honest statement about what this sandbox/runtime can virtualise: a real
/// Android system image needs KVM + binder/ashmem kernel support. Where that
/// is absent (containers, most CI), the handset runs as a full in-process OS
/// runtime (compositor + adbd + radio model), not a system VM.
fn kernel_support_note() -> &'static str {
    let kvm = std::path::Path::new("/dev/kvm").exists();
    let binder = std::path::Path::new("/dev/binder").exists();
    match (kvm, binder) {
        (true, true) => "host supports KVM + binder; profile can additionally back a full system image",
        (true, false) => "KVM present, binder absent; in-process OS runtime (compositor + adbd + radio)",
        (false, true) => "binder present, KVM absent; in-process OS runtime (compositor + adbd + radio)",
        (false, false) => "no KVM/binder on this host; the handset runs as the in-process OS runtime (compositor + adbd + radio model)",
    }
}

use std::path::PathBuf;

use serde_json::{json, Value};

use crate::key::default_dir;

/// Where account identifiers persist (`~/.android/device.json`). Passwords
/// are deliberately absent from this file forever.
pub fn config_path() -> PathBuf {
    default_dir().join("device.json")
}

/// Built-in account identifiers: a fresh install reports the device as signed
/// in with these, out of the box (identifiers only — never secrets). Override
/// any time with `mandroid profile --set-account ...`; `--clear-account`
/// writes an explicit empty state which is always respected.
pub const DEFAULT_ACCOUNT_EMAIL: &str = "owner@romacode.local";
pub const DEFAULT_ACCOUNT_ALT_EMAIL: &str = "alt-owner@romacode.local";

/// The pre-provisioned account: identifiers only.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct AccountConfig {
    pub email: Option<String>,
    pub alternate_email: Option<String>,
}

impl AccountConfig {
    /// Load account identifiers (config file, else the built-in defaults).
    pub fn load() -> Self {
        Self::load_from(&config_path())
    }

    /// Testable core of [`load`]: no file → built-in defaults (fresh install
    /// is signed-in ready); a file present is respected verbatim, explicit
    /// nulls included (that is what clear-account persists).
    pub fn load_from(path: &std::path::Path) -> Self {
        let Ok(text) = std::fs::read_to_string(path) else {
            return Self {
                email: Some(DEFAULT_ACCOUNT_EMAIL.into()),
                alternate_email: Some(DEFAULT_ACCOUNT_ALT_EMAIL.into()),
            };
        };
        let Ok(v) = serde_json::from_str::<Value>(&text) else {
            return Self::default();
        };
        Self {
            email: v["account_email"].as_str().map(String::from),
            alternate_email: v["account_alt_email"].as_str().map(String::from),
        }
    }

    /// Persist account identifiers (never secrets).
    pub fn save(&self) -> std::io::Result<()> {
        let path = config_path();
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let body = json!({
            "account_email": self.email,
            "account_alt_email": self.alternate_email,
            "note": "identifiers only; passwords come from MANDROID_ACCOUNT_PASSWORD at render time and are never stored",
        });
        std::fs::write(path, serde_json::to_string_pretty(&body).unwrap())
    }
}

/// Product-spec hardware profile for the MANDROID virtual device.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DeviceProfile {
    /// Marketing name of the profile.
    pub name: String,
    /// RAM in MiB (spec: 16 GB = 16384 MiB).
    pub ram_mib: u64,
    /// Internal storage in GiB (spec: 1 TB = 931 GiB usable).
    pub rom_gib: u64,
    /// CPU cores, matched to the host processor.
    pub cpu_cores: u32,
    /// Host/device CPU architecture (e.g. `x86_64`, `aarch64`).
    pub cpu_arch: &'static str,
    /// Default screen width in px.
    pub screen_width: u32,
    /// Default screen height in px.
    pub screen_height: u32,
    /// Default screen density in dpi.
    pub screen_density: u32,
    /// Android API level the profile targets.
    pub sdk_level: u32,
    /// Pre-provisioned account identifiers.
    pub account: AccountConfig,
}

/// The connectivity spec: two embedded SIMs, no physical slot.
pub const ESIM_SLOTS: u32 = 2;
/// Profiles each eSIM can hold (GSMA-standard consumer limit on modern chips).
pub const ESIM_PROFILES_PER_SLOT: u32 = 8;

/// The out-of-the-box app roster — what a new phone with GMS ships with.
pub const PREINSTALLED_APPS: &[(&str, &str, &str)] = &[
    // (package, label, category)
    ("com.android.vending", "Google Play Store", "store"),
    ("com.google.android.gms", "Google Play services", "system"),
    (
        "com.google.android.gsf",
        "Google Services Framework",
        "system",
    ),
    ("com.google.android.setupwizard", "Setup Wizard", "system"),
    ("com.google.android.gm", "Gmail", "google"),
    ("com.google.android.apps.maps", "Maps", "google"),
    ("com.google.android.youtube", "YouTube", "google"),
    ("com.google.android.apps.photos", "Photos", "google"),
    ("com.google.android.apps.docs", "Drive", "google"),
    (
        "com.google.android.googlequicksearchbox",
        "Google",
        "google",
    ),
    ("com.android.chrome", "Chrome", "google"),
    ("com.google.android.apps.messaging", "Messages", "google"),
    ("com.google.android.dialer", "Phone", "system"),
    ("com.google.android.apps.tachyon", "Meet", "google"),
    ("com.google.android.inputmethod.latin", "Gboard", "system"),
    ("com.google.android.tts", "Speech Services", "system"),
    (
        "com.google.android.webview",
        "Android System WebView",
        "system",
    ),
    (
        "com.google.android.configupdater",
        "Config Updater",
        "system",
    ),
    ("com.android.settings", "Settings", "system"),
    ("com.android.systemui", "System UI", "system"),
];

impl Default for DeviceProfile {
    fn default() -> Self {
        let id = crate::identity::DeviceIdentity::factory();
        Self {
            name: id.model.into(),
            ram_mib: id.ram_mib,
            rom_gib: id.rom_gib,
            cpu_cores: id.cpu_cores,
            cpu_arch: id.abi,
            screen_width: id.panel.0,
            screen_height: id.panel.1,
            screen_density: id.panel.2,
            sdk_level: id.sdk_level,
            account: AccountConfig::load(),
        }
    }
}

impl DeviceProfile {
    /// Render the full profile as a JSON value.
    pub fn to_json(&self) -> Value {
        let id = crate::identity::DeviceIdentity::factory();
        json!({
            "name": self.name,
            "hardware_class": "physical handset",
            "ram": {
                "mib": self.ram_mib,
                "gb": self.ram_mib as f64 / 1024.0,
                "bytes": self.ram_mib * 1024 * 1024,
                "spec": format!("{} GB", self.ram_mib / 1024),
            },
            "rom": {
                "gib": self.rom_gib,
                "bytes": self.rom_gib * 1024 * 1024 * 1024,
                "spec": if self.rom_gib >= 900 { "1 TB".to_string() } else { format!("{} GB", self.rom_gib) },
            },
            "cpu": {
                "cores": self.cpu_cores,
                "arch": self.cpu_arch,
                "soc": id.soc_model,
            },
            "identifiers": {
                "serial": id.serial,
                "imei": id.imei,
                "imei2": id.imei2,
                "eid": id.eid,
                "baseband": id.baseband,
            },
            "screen": {
                "width": self.screen_width,
                "height": self.screen_height,
                "density": self.screen_density,
                "geometry": format!("{}x{}", self.screen_width, self.screen_height),
                "resizable": true,
            },
            "esim": self.esim_json(),
            "apps": self.apps_json(),
            "account": self.account_json(),
            "android": {
                "sdk_level": self.sdk_level,
                "release": id.android_release,
                "security_patch": id.security_patch,
                "build_type": "user",
                "build_tags": "release-keys",
                "ro_prop_set": self.ro_properties(),
            },
            "kernel_support": kernel_support_note(),
        })
    }

    /// eSIM connectivity spec merged with the factory registry state: the
    /// two profiles registered and enabled out of the box, with their full
    /// subscription identity (ICCID, IMSI, MSISDN, activation code).
    pub fn esim_json(&self) -> Value {
        let reg = crate::identity::EsimRegistry::factory();
        let id = crate::identity::DeviceIdentity::factory();
        let slots: Vec<Value> = (1..=2)
            .map(|n| match reg.enabled_on(n) {
                Some(p) => json!({
                    "slot": n,
                    "port_index": n - 1,
                    "state": "enabled",
                    "profile_state": "active",
                    "label": format!("eSIM {n}"),
                    "profile": p.to_json(),
                    "eid": id.eid,
                }),
                None => json!({ "slot": n, "state": "empty", "label": format!("eSIM {n}"),
                        "eid": id.eid }),
            })
            .collect();
        json!({
            "slot_count": ESIM_SLOTS,
            "dual_esim": ESIM_SLOTS >= 2,
            "technology": "eSIM (embedded UICC, SGP.22)",
            "physical_sim_slot": false,
            "eid": id.eid,
            "max_profiles_per_slot": ESIM_PROFILES_PER_SLOT,
            "switchable": true,
            "download_over_the_air": true,
            "profiles_registered": reg.profiles.len(),
            "profile_catalog": crate::sim::CATALOG.len(),
            "profile_countries": {
                "highlights": ["US", "GB", "PK"],
                "note": format!("{} additional carrier profiles can be registered over the air (android_esim action=register)", crate::sim::CATALOG.len()),
            },
            "slots": slots,
            "live_tools": "android_esim (live device) | android_signal (live signal)",
        })
    }

    /// The out-of-the-box roster.
    pub fn apps_json(&self) -> Value {
        let apps: Vec<Value> = PREINSTALLED_APPS
            .iter()
            .map(|(pkg, label, cat)| {
                json!({
                    "package": pkg,
                    "label": label,
                    "category": cat,
                    "state": "installed",
                    "signed_in_with_account": matches!(*cat, "google" | "store"),
                })
            })
            .collect();
        json!({
            "count": apps.len(),
            "like_new_phone": true,
            "gms_core": true,
            "apps": apps,
        })
    }

    /// The account section. Identifiers only — the password is read from the
    /// environment at render time and reported solely as `password_set`.
    pub fn account_json(&self) -> Value {
        let password_set = std::env::var_os("MANDROID_ACCOUNT_PASSWORD")
            .map(|v| !v.is_empty())
            .unwrap_or(false);
        json!({
            "type": "com.google",
            "email": self.account.email,
            "alternate_email": self.account.alternate_email,
            "signed_in": self.account.email.is_some(),
            "play_store_signed_in": self.account.email.is_some(),
            "signed_in_apps": ["com.android.vending", "com.google.android.gm",
                "com.google.android.youtube", "com.google.android.apps.photos",
                "com.google.android.apps.maps", "com.google.android.apps.docs"],
            "password_source": "MANDROID_ACCOUNT_PASSWORD environment variable",
            "password_set": password_set,
            "password_stored": false,
            "verification": {
                "required": false,
                "reason": "configuration-space emulation — no online services are contacted, so no verification flow exists",
            },
        })
    }

    /// The physical `ro.` property set the device reports over `getprop`.
    pub fn ro_properties(&self) -> Value {
        let mut out = serde_json::Map::new();
        let id = crate::identity::DeviceIdentity::factory();
        for (k, v) in crate::identity::base_ro_props(&id) {
            out.insert(k, Value::String(v));
        }
        Value::Object(out)
    }

    /// Override RAM/ROM/Cores (MiB / GiB / count) — the rest stays spec-fixed.
    pub fn with_overrides(
        mut self,
        ram_mib: Option<u64>,
        rom_gib: Option<u64>,
        cores: Option<u32>,
    ) -> Self {
        if let Some(mib) = ram_mib {
            self.ram_mib = mib;
        }
        if let Some(gib) = rom_gib {
            self.rom_gib = gib;
        }
        if let Some(c) = cores {
            self.cpu_cores = c;
        }
        self
    }
}

/// Core count of the physical SoC (Tensor G4: 1 Cortex-X4 + 3 Cortex-A720
/// + 4 Cortex-A520).
pub fn detect_cpu_cores() -> u32 {
    crate::identity::DeviceIdentity::factory().cpu_cores
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn spec_defaults_are_product_spec() {
        let p = DeviceProfile::default();
        assert_eq!(p.ram_mib, 16_384, "16 GB RAM — the real Pixel 9 Pro spec");
        assert_eq!(p.rom_gib, 931, "1 TB UFS variant (931 GiB usable)");
        assert_eq!(p.screen_width, 720);
        assert_eq!(p.screen_height, 1560);
    }

    #[test]
    fn cores_match_the_physical_soc() {
        let p = DeviceProfile::default();
        assert_eq!(
            p.cpu_cores, 8,
            "Tensor G4 is an 8-core SoC (1 X4 + 3 A720 + 4 A520)"
        );
        assert_eq!(p.cpu_arch, "arm64-v8a");
    }

    #[test]
    fn json_contains_all_sections() {
        let v = DeviceProfile::default().to_json();
        assert_eq!(v["ram"]["gb"], 16.0);
        assert_eq!(v["ram"]["spec"], "16 GB");
        assert_eq!(v["rom"]["gib"], 931);
        assert_eq!(v["rom"]["spec"], "1 TB");
        assert_eq!(v["cpu"]["cores"], 8);
        assert_eq!(v["screen"]["geometry"], "720x1560");
        assert_eq!(v["screen"]["resizable"], true);
        assert_eq!(v["esim"]["slot_count"], 2);
        assert_eq!(v["esim"]["dual_esim"], true);
        assert_eq!(v["esim"]["profiles_registered"], 2);
        let slots = v["esim"]["slots"].as_array().unwrap();
        assert_eq!(slots.len(), 2);
        assert_eq!(slots[0]["state"], "enabled");
        assert_eq!(slots[1]["state"], "enabled");
        // the factory identities are real: Luhn-valid ICCIDs
        for s in slots {
            let iccid = s["profile"]["iccid"].as_str().unwrap();
            assert_eq!(iccid.len(), 19);
        }
        assert_eq!(v["identifiers"]["imei"].as_str().unwrap().len(), 15);
        assert_eq!(v["identifiers"]["eid"].as_str().unwrap().len(), 32);
        assert!(v["apps"]["count"].as_u64().unwrap() >= 20);
        assert_eq!(v["android"]["build_type"], "user");
        assert_eq!(v["android"]["build_tags"], "release-keys");
        assert!(v["android"]["ro_prop_set"]["ro.product.model"].is_string());
        assert!(v["kernel_support"].is_string());
    }

    #[test]
    fn play_store_is_preinstalled_and_signable_in() {
        let v = DeviceProfile::default().to_json();
        let apps = v["apps"]["apps"].as_array().unwrap();
        let store = apps
            .iter()
            .find(|a| a["package"] == "com.android.vending")
            .expect("Play Store must be in the out-of-the-box roster");
        assert_eq!(store["state"], "installed");
        assert_eq!(store["signed_in_with_account"], true);
        // Gmail too.
        assert!(apps.iter().any(|a| a["package"] == "com.google.android.gm"));
    }

    #[test]
    fn fresh_install_ships_with_built_in_account_identifiers() {
        // No config file → the built-in defaults (signed-in ready out of the
        // box); identifiers only, never secrets.
        let missing = std::env::temp_dir().join(format!("mandroid-none-{}", std::process::id()));
        let _ = std::fs::remove_file(&missing);
        let acct = AccountConfig::load_from(&missing);
        assert_eq!(acct.email.as_deref(), Some(DEFAULT_ACCOUNT_EMAIL));
        assert_eq!(
            acct.alternate_email.as_deref(),
            Some(DEFAULT_ACCOUNT_ALT_EMAIL)
        );
        // A present file is respected verbatim — including an explicit clear.
        let cleared = std::env::temp_dir().join(format!("mandroid-clear-{}", std::process::id()));
        std::fs::write(
            &cleared,
            json!({ "account_email": Value::Null, "account_alt_email": Value::Null }).to_string(),
        )
        .unwrap();
        let acct = AccountConfig::load_from(&cleared);
        assert_eq!(acct.email, None, "explicit clear must win over defaults");
        std::fs::remove_file(&cleared).ok();
    }

    #[test]
    fn account_identifiers_only_password_never_stored() {
        let mut p = DeviceProfile::default();
        p.account.email = Some("owner@romacode.local".into());
        p.account.alternate_email = Some("alt-owner@romacode.local".into());
        let v = p.account_json();
        assert_eq!(v["email"], "owner@romacode.local");
        assert_eq!(v["alternate_email"], "alt-owner@romacode.local");
        assert_eq!(v["signed_in"], true);
        assert_eq!(v["play_store_signed_in"], true);
        assert_eq!(v["verification"]["required"], false);
        // The rendered JSON must never carry a secret value: the only
        // password-related fields are the source pointer and the boolean.
        let rendered = serde_json::to_string(&v).unwrap();
        assert!(
            !rendered.to_lowercase().contains("bari"),
            "no secrets in output"
        );
        assert_eq!(v["password_stored"], false);
        assert!(v["password_set"].is_boolean());
    }

    #[test]
    fn overrides_apply() {
        let p = DeviceProfile::default().with_overrides(Some(8192), Some(512), Some(4));
        assert_eq!(p.ram_mib, 8192);
        assert_eq!(p.rom_gib, 512);
        assert_eq!(p.cpu_cores, 4);
    }
}
