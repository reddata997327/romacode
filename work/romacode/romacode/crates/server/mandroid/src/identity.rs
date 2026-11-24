//! Physical device identity — the handset the runtime presents.
//!
//! One deterministic, factory-level identity for the built-in phone: a real
//! production-handset hardware profile (brand/model/device codename, a
//! `user` build signed with `release-keys`, baseband, serial, IMEI/IMEI2,
//! chip EID), a **dual-eSIM registry that ships with two registered profiles
//! already enabled** (ICCID/IMSI/MSISDN/activation code per profile), and a
//! **live radio model** that produces the per-slot signal state a real modem
//! would report (RSRP, bars, technology, band, cell identity, roaming,
//! registration state — varying smoothly over time).
//!
//! Everything here is a pure function of (seed, catalog profile, time) — no
//! global mutable state — so the phone compositor, `getprop`, `dumpsys`,
//! the `esim` utility and the agent tools all compute identical values from
//! the same inputs. There is no second representation anywhere.

use serde_json::{json, Value};

use crate::sim;

// ── the physical identity ────────────────────────────────────────────────────

/// Seed for every generated identifier. A fixed seed means the handset keeps
/// the same serial/IMEI/EID across boots and across machines — it is one
/// physical device, not a re-rolled one.
const IDENTITY_SEED: &str = "romacode-phone-caiman-factory-v1";

// Build timestamp of the CP4A.261105.040.A1 factory image (2026-11-05).

/// The production handset this device presents as: a **Google Pixel 9 Pro
/// (GEC77, codename `caiman`)** — a real premium flagship on sale, the first
/// Pixel line that ships **dual eSIM in hardware** ("Dual eSIM — 2 active
/// eSIMs; stores 8 or more" per Google's own spec sheet for US models), with
/// 16 GB LPDDR5X RAM, a 1 TB UFS variant, the Google Tensor G4 SoC (1×
/// Cortex-X4 3.1 GHz + 3× Cortex-A720 2.6 GHz + 4× Cortex-A520 1.92 GHz,
/// Mali-G715 MC7 GPU) and a 4700 mAh battery. It runs the real November 2026
/// Android 17 build: fingerprint
/// `google/caiman/caiman:17/CP4A.261105.040.A1/16581829:user/release-keys`
/// (security patch 2026-11-05). RAM/ROM/SoC/Android version all match the
/// real product; the bootloader is unlocked on this rooted handset and no
/// emulator marker anywhere (`emulator`, `generic`, `sdk`, `goldfish`,
/// `ranchu` never appear).
#[derive(Clone)]
pub struct DeviceIdentity {
    /// 12-character alphanumeric serial number (Pixel-style; what
    /// `ro.serialno` and the CNXN banner report).
    pub serial: String,
    /// Slot-1 IMEI (15 digits, Luhn-valid).
    pub imei: String,
    /// Slot-2 IMEI — a dual-eSIM handset has one IMEI per modem slot.
    pub imei2: String,
    /// eUICC chip EID (32 digits, SGP.22).
    pub eid: String,
    /// Baseband/radio version string (Exynos 5300 modem, Pixel format).
    pub baseband: String,
    /// Kernel version (`uname -r`, GKI android15-6.1 on Tensor G4).
    pub kernel: &'static str,
    /// Model (Pixels report the marketing name as `ro.product.model`).
    pub model: &'static str,
    /// Marketing name (`ro.product.marketname`, shown in Settings).
    pub marketname: &'static str,
    pub brand: &'static str,
    pub device: &'static str,
    pub product: &'static str,
    pub manufacturer: &'static str,
    pub hardware: &'static str,
    pub soc_manufacturer: &'static str,
    pub soc_model: &'static str,
    pub cpu_cores: u32,
    pub abi: &'static str,
    pub abi_list: &'static str,
    pub ram_mib: u64,
    pub rom_gib: u64,
    /// Panel: (width, height, dpi) — matches the phone crate's panel.
    pub panel: (u32, u32, u32),
    pub android_release: &'static str,
    pub sdk_level: u32,
    pub build_id: &'static str,
    pub build_incremental: &'static str,
    pub security_patch: &'static str,
    pub build_date_utc: i64,
    pub build_date: &'static str,
    pub fingerprint: &'static str,
    /// `true`: the bootloader is unlocked on this rooted handset.
    pub bootloader_unlocked: bool,
}

impl DeviceIdentity {
    /// The factory identity — deterministic, identical on every call.
    pub fn factory() -> Self {
        let h0 = fnv64(IDENTITY_SEED);
        Self {
            serial: serial_from(h0),
            imei: imei_from(h0 ^ 0x9E3779B97F4A7C15, TAC),
            imei2: imei_from(h0 ^ 0xC2B2AE3D27D4EB4F, TAC),
            eid: {
                // 32-digit SGP.22 EID with a Luhn-valid check digit
                let base = format!("89049032{}", digits(h0 ^ 0x165667B19E3779F9, 23));
                format!("{base}{}", luhn_check(&base))
            },
            baseband: "g5300-261023-261017_B-260930_B-260918_B-260905_B-260825_B-260812_B".into(),
            kernel: "6.1.99-android15-11-g3c8d92e1a4b6",
            model: "Pixel 9 Pro",
            marketname: "Pixel 9 Pro",
            brand: "google",
            device: "caiman",
            product: "caiman",
            manufacturer: "Google",
            hardware: "caiman",
            soc_manufacturer: "Google",
            soc_model: "Tensor G4",
            cpu_cores: 8, // 1 Cortex-X4 3.1 GHz + 3 Cortex-A720 2.6 GHz + 4 Cortex-A520 1.92 GHz
            abi: "arm64-v8a",
            abi_list: "arm64-v8a,armeabi-v7a",
            ram_mib: 16384, // 16 GB LPDDR5X — the real Pixel 9 Pro spec
            rom_gib: 931,   // 1 TB UFS variant (931 GiB usable)
            panel: (720, 1560, 420),
            android_release: "17",
            sdk_level: 37,
            build_id: "CP4A.261105.040.A1",
            build_incremental: "16581829",
            security_patch: "2026-11-05",
            build_date_utc: 1_793_909_497,
            build_date: "Thu Nov  5 20:11:37 UTC 2026",
            fingerprint: "google/caiman/caiman:17/CP4A.261105.040.A1/16581829:user/release-keys",
            bootloader_unlocked: true,
        }
    }

    /// The `ro.build.description` line — the Pixel shape, and exactly the
    /// string the real caiman CP4A.261105.040.A1 build reports:
    /// `caiman-user 17 CP4A.261105.040.A1 16581829 release-keys`.
    pub fn build_description(&self) -> String {
        format!(
            "{}-user {} {} {} release-keys",
            self.product, self.android_release, self.build_id, self.build_incremental
        )
    }
}

/// The physical device's base `ro.` property set — the build/product/
/// hardware identity a real handset reports. The phone's `getprop` table
/// extends this with live telephony and display state.
pub fn base_ro_props(id: &DeviceIdentity) -> Vec<(String, String)> {
    let fp = id.fingerprint;
    let mut v: Vec<(String, String)> = vec![
        // product identity
        ("ro.product.model".into(), id.model.into()),
        ("ro.product.brand".into(), id.brand.into()),
        ("ro.product.device".into(), id.device.into()),
        ("ro.product.name".into(), id.product.into()),
        ("ro.product.manufacturer".into(), id.manufacturer.into()),
        ("ro.product.marketname".into(), id.marketname.into()),
        ("ro.product.system.model".into(), id.model.into()),
        ("ro.product.vendor.model".into(), id.model.into()),
        ("ro.product.product.model".into(), id.model.into()),
        ("ro.product.system.device".into(), id.device.into()),
        ("ro.product.vendor.device".into(), id.device.into()),
        ("ro.build.product".into(), id.device.into()),
        ("ro.build.flavor".into(), format!("{}-user", id.product)),
        ("ro.build.characteristics".into(), "default".into()),
        // build identity — a physical `user` build with `release-keys`
        ("ro.build.id".into(), id.build_id.into()),
        ("ro.build.display.id".into(), id.build_id.into()),
        ("ro.build.version.release".into(), id.android_release.into()),
        (
            "ro.build.version.release_or_codename".into(),
            id.android_release.into(),
        ),
        ("ro.build.version.sdk".into(), id.sdk_level.to_string()),
        ("ro.build.version.codename".into(), "REL".into()),
        ("ro.build.version.all_codenames".into(), "REL".into()),
        (
            "ro.build.version.incremental".into(),
            id.build_incremental.into(),
        ),
        (
            "ro.build.version.security_patch".into(),
            id.security_patch.into(),
        ),
        (
            "ro.build.version.min_supported_target_sdk".into(),
            "24".into(),
        ),
        ("ro.build.type".into(), "user".into()),
        ("ro.build.tags".into(), "release-keys".into()),
        ("ro.build.user".into(), "android-build".into()),
        ("ro.build.host".into(), "abfarm-release-rbe-64-7300".into()),
        ("ro.build.date".into(), id.build_date.into()),
        ("ro.build.date.utc".into(), id.build_date_utc.to_string()),
        ("ro.build.description".into(), id.build_description()),
        ("ro.build.fingerprint".into(), fp.into()),
        ("ro.bootimage.build.fingerprint".into(), fp.into()),
        ("ro.vendor.build.fingerprint".into(), fp.into()),
        // first API level of the Pixel 9 Pro (launched on Android 14)
        ("ro.product.first_api_level".into(), "34".into()),
        // hardware
        ("ro.hardware".into(), id.hardware.into()),
        ("ro.board.platform".into(), "zumapro".into()),
        ("ro.soc.manufacturer".into(), id.soc_manufacturer.into()),
        ("ro.soc.model".into(), id.soc_model.into()),
        ("ro.product.cpu.abi".into(), id.abi.into()),
        ("ro.product.cpu.abilist".into(), id.abi_list.into()),
        ("ro.product.cpu.abilist32".into(), "armeabi-v7a".into()),
        ("ro.product.cpu.abilist64".into(), "arm64-v8a".into()),
        ("ro.zygote".into(), "zygote64_32".into()),
        ("ro.baseband".into(), id.baseband.clone()),
        ("ro.boot.baseband".into(), id.baseband.clone()),
        ("ro.kernel.version".into(), id.kernel.into()),
        // serial + bootloader (unlocked on this rooted handset)
        ("ro.serialno".into(), id.serial.clone()),
        ("ro.boot.serialno".into(), id.serial.clone()),
        // Pixel SKU identity (GEC77 = the US Pixel 9 Pro model)
        ("ro.boot.hardware.sku".into(), "GEC77".into()),
        ("ro.boot.product.hardware.sku".into(), "GEC77".into()),
        ("ro.bootmode".into(), "normal".into()),
        (
            "ro.boot.flash.locked".into(),
            if id.bootloader_unlocked { "0" } else { "1" }.into(),
        ),
        (
            "ro.boot.verifiedbootstate".into(),
            if id.bootloader_unlocked {
                "orange"
            } else {
                "green"
            }
            .into(),
        ),
        (
            "ro.boot.vbmeta.device_state".into(),
            if id.bootloader_unlocked {
                "unlocked"
            } else {
                "locked"
            }
            .into(),
        ),
        // security posture of a rooted user build
        ("ro.secure".into(), "1".into()),
        ("ro.debuggable".into(), "0".into()),
        ("ro.adb.secure".into(), "1".into()),
        ("ro.crypto.state".into(), "encrypted".into()),
        ("ro.crypto.type".into(), "file".into()),
        (
            "ro.build.expect.bootloader".into(),
            if id.bootloader_unlocked { "0" } else { "1" }.into(),
        ),
        // Pixel platform specifics — the 16 GB handset's Dalvik heap sizing
        ("dalvik.vm.heapstartsize".into(), "16m".into()),
        ("dalvik.vm.heapgrowthlimit".into(), "256m".into()),
        ("dalvik.vm.heapsize".into(), "512m".into()),
        ("dalvik.vm.heapminfree".into(), "2m".into()),
        ("dalvik.vm.heapmaxfree".into(), "8m".into()),
        // radio — dual eSIM handset
        ("ro.telephony.esim_slots".into(), "2".into()),
        ("gsm.version.baseband".into(), id.baseband.clone()),
        // misc boot state
        ("sys.boot_completed".into(), "1".into()),
        ("init.svc.adbd".into(), "running".into()),
        ("persist.sys.timezone".into(), "UTC".into()),
        (
            "ro.runtime.firstboot".into(),
            (id.build_date_utc as i64 * 1000).to_string(),
        ),
    ];
    v.sort_by(|a, b| a.0.cmp(&b.0));
    v
}

/// IMEI Type Allocation Code (a format-plausible 8-digit TAC).
const TAC: u64 = 35_689_207;

/// FNV-1a 64-bit — enough to derive stable identifiers; not a security
/// primitive.
pub fn fnv64(seed: &str) -> u64 {
    let mut h: u64 = 0xcbf29ce484222325;
    for b in seed.bytes() {
        h ^= b as u64;
        h = h.wrapping_mul(0x100000001b3);
    }
    h
}

/// splitmix64 — deterministic, well-distributed digit/char stream.
fn next_u64(state: &mut u64) -> u64 {
    *state = state.wrapping_add(0x9E3779B97F4A7C15);
    let mut z = *state;
    z = (z ^ (z >> 30)).wrapping_mul(0xBF58476D1CE4E5B9);
    z = (z ^ (z >> 27)).wrapping_mul(0x94D049BB133111EB);
    z ^ (z >> 31)
}

/// `n` decimal digits from a seed.
fn digits(mut seed: u64, n: usize) -> String {
    let mut out = String::with_capacity(n);
    for _ in 0..n {
        out.push(char::from(b'0' + (next_u64(&mut seed) % 10) as u8));
    }
    out
}

/// 12-character alphanumeric serial (Pixel-style length, no zeros/O or
/// 1/I ambiguity — like real handset serials).
fn serial_from(seed: u64) -> String {
    const ALPHABET: &[u8] = b"ABCDEFGHJKLMNPQRSTUVWXYZ23456789";
    let mut s = seed;
    let mut out = String::with_capacity(12);
    for _ in 0..12 {
        out.push(ALPHABET[(next_u64(&mut s) % ALPHABET.len() as u64) as usize] as char);
    }
    out
}

/// Luhn check digit for a digit string (mod 10, doubling from the right for
/// the position being added).
fn luhn_check(digits_no_check: &str) -> char {
    let mut sum = 0u32;
    let mut dbl = true; // the check digit position itself is not doubled;
                        // walking right-to-left from the last existing digit, the first is
                        // doubled (parity of the full-length IMEI/ICCID).
    for ch in digits_no_check.chars().rev() {
        let mut d = (ch as u8 - b'0') as u32;
        if dbl {
            d *= 2;
            if d > 9 {
                d -= 9;
            }
        }
        sum += d;
        dbl = !dbl;
    }
    char::from(b'0' + ((10 - sum % 10) % 10) as u8)
}

/// A 15-digit Luhn-valid IMEI: TAC(8) + serial(6) + check(1).
pub fn imei_from(seed: u64, tac: u64) -> String {
    let base = format!("{tac:08}{}", digits(seed, 6));
    format!("{base}{}", luhn_check(&base))
}

/// A 19-digit Luhn-valid ICCID: `89` + country dialing code (1-3 digits) +
/// 3-digit issuer + account digits + check digit — the account length adapts
/// so every profile yields exactly 19 digits.
pub fn iccid_from(seed: u64, country: &str, mnc: &str) -> String {
    let issuer = format!("{:0>3}", mnc); // MNC left-padded to 3 digits
    let dialing = dialing_code(country);
    let prefix_len = 2 + dialing.len() + issuer.len();
    let account_len = 19 - 1 - prefix_len; // 1 digit for the Luhn check
    let base = format!("89{dialing}{issuer}{}", digits(seed, account_len));
    format!("{base}{}", luhn_check(&base))
}

/// ITU-T E.164 country dialing codes for the SIM catalog countries.
fn dialing_code(country: &str) -> &'static str {
    match country {
        "US" | "CA" => "1",
        "GB" => "44",
        "PK" => "92",
        "AE" => "971",
        "SA" => "966",
        "DE" => "49",
        "IN" => "91",
        "TR" => "90",
        "BD" => "880",
        "SG" => "65",
        "AU" => "61",
        _ => "1",
    }
}

// ── eSIM profiles ────────────────────────────────────────────────────────────

/// One registered eSIM profile on the chip — the full subscription identity a
/// real LPA reports after a profile download (ICCID, IMSI, MSISDN, operator,
/// activation code, state).
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct EsimProfile {
    /// Catalog id this profile came from (`us-tmobile`, `uk-ee`, ...).
    pub profile_id: String,
    /// 19-digit Luhn-valid ICCID.
    pub iccid: String,
    /// 15-digit IMSI (MCC+MNC+MSIN, ITU-T E.212).
    pub imsi: String,
    /// Line number, E.164 (fictional-number ranges only).
    pub msisdn: String,
    /// User-visible label (Settings > SIM manager).
    pub nickname: String,
    pub carrier: String,
    pub country: String,
    pub country_name: String,
    pub mcc: u32,
    pub mnc: String,
    pub network: String,
    /// GSMA activation code the profile was downloaded with.
    pub activation: String,
    /// "operational" (a normal carrier profile).
    pub profile_class: String,
    /// "active" once installed and enabled.
    pub state: String,
    pub enabled: bool,
    /// eUICC slot (1 or 2).
    pub slot: u32,
    /// SGP.22 port index (0 for slot 1, 1 for slot 2).
    pub port_index: u32,
    pub registered_at: i64,
}

impl EsimProfile {
    /// Derive the full subscription identity for a catalog profile —
    /// deterministic per profile id.
    pub fn derive(profile: &'static sim::SimProfile, now_ms: i64) -> Self {
        let seed = fnv64(&format!("esim-{}", profile.id));
        let iccid = iccid_from(seed, profile.country, profile.mnc);
        let mnc_padded = format!("{:0>3}", profile.mnc);
        let msin_len = 15 - 3 - mnc_padded.len();
        let imsi = format!(
            "{:03}{}{}",
            profile.mcc,
            mnc_padded,
            digits(seed ^ 0xA24BAED4963EE407, msin_len)
        );
        let msisdn = msisdn_from(seed, profile.country);
        let smdp = profile
            .reachability_host
            .trim_start_matches("www.")
            .to_string();
        let activation = format!(
            "LPA:1$smdp.{smdp}$RK1-{}",
            digits(seed ^ 0xD1B54A32D192ED03, 8)
        );
        Self {
            profile_id: profile.id.to_string(),
            iccid,
            imsi,
            msisdn,
            nickname: profile.carrier.to_string(),
            carrier: profile.carrier.to_string(),
            country: profile.country.to_string(),
            country_name: profile.country_name.to_string(),
            mcc: profile.mcc,
            mnc: profile.mnc.to_string(),
            network: profile.network.to_string(),
            activation,
            profile_class: "operational".into(),
            state: "active".into(),
            enabled: false,
            slot: 0,
            port_index: 0,
            registered_at: now_ms,
        }
    }

    /// MCC-MNC string as Android reports it.
    pub fn plmn(&self) -> String {
        format!("{:03}-{}", self.mcc, self.mnc)
    }

    pub fn to_json(&self) -> Value {
        json!({
            "profile_id": self.profile_id,
            "iccid": self.iccid,
            "imsi": self.imsi,
            "msisdn": self.msisdn,
            "nickname": self.nickname,
            "carrier": self.carrier,
            "country": self.country,
            "country_name": self.country_name,
            "plmn": self.plmn(),
            "network": self.network,
            "activation_code": self.activation,
            "profile_class": self.profile_class,
            "state": self.state,
            "enabled": self.enabled,
            "slot": self.slot,
            "port_index": self.port_index,
            "registered_at": self.registered_at,
        })
    }
}

/// The line number. Fictional-number ranges only: US `555-01xx` (the
/// reserved drama range), UK Ofcom `07700 900xxx`, and a synthetic block for
/// the remaining countries — never a real subscriber's number.
fn msisdn_from(seed: u64, country: &str) -> String {
    let mut s = seed ^ 0x27BB3EE62621D4C6;
    let n3 = (next_u64(&mut s) % 1000) as u32;
    let n2 = (next_u64(&mut s) % 100) as u32;
    match country {
        "US" | "CA" => format!("+1 {} 555 01{:02}", 200 + n3 % 600, n2),
        "GB" => format!("+44 7700 900{:03}", n3 % 1000),
        "PK" => format!(
            "+92 3{:02} 5550 {:03}",
            n3 % 100,
            n2 * 10 + (n3 % 10) as u32
        ),
        "AE" => format!("+971 50 555 01{:02}", n2),
        "SA" => format!("+966 50 555 01{:02}", n2),
        "DE" => format!("+49 151 5550 {:04}", n3),
        "IN" => format!("+91 95550 {:05}", n3 * 100 + n2 as u32),
        "TR" => format!("+90 555 0{:03} {:02}", n3 % 1000, n2),
        "BD" => format!("+880 17 5550 {:04}", n3),
        "SG" => format!("+65 8555 01{:02}", n2),
        "AU" => format!("+61 4 555 01{:02}", n2),
        _ => format!("+1 {} 555 01{:02}", 200 + n3 % 600, n2),
    }
}

/// The dual-eSIM registry — the chip's profile store.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct EsimRegistry {
    pub profiles: Vec<EsimProfile>,
    /// Slot used for mobile data (1 or 2).
    pub data_slot: u32,
}

impl EsimRegistry {
    /// The factory registry: **two profiles already registered and enabled**
    /// — a primary US carrier on slot 1 and a second US carrier on slot 2,
    /// the standard out-of-the-box dual-eSIM state. Both are `active` and
    /// loaded, so the modem registers on both networks from first boot.
    pub fn factory() -> Self {
        let now = 0_i64; // deterministic; runtime stamps overwrite on register
        let mut p1 =
            EsimProfile::derive(sim::find("us-tmobile").expect("us-tmobile in catalog"), now);
        let mut p2 = EsimProfile::derive(sim::find("us-att").expect("us-att in catalog"), now);
        p1.slot = 1;
        p1.port_index = 0;
        p1.enabled = true;
        p2.slot = 2;
        p2.port_index = 1;
        p2.enabled = true;
        Self {
            profiles: vec![p1, p2],
            data_slot: 1,
        }
    }

    /// The profile enabled on `slot`, if any.
    pub fn enabled_on(&self, slot: u32) -> Option<&EsimProfile> {
        self.profiles.iter().find(|p| p.enabled && p.slot == slot)
    }

    fn enabled_on_mut(&mut self, slot: u32) -> Option<&mut EsimProfile> {
        self.profiles
            .iter_mut()
            .find(|p| p.enabled && p.slot == slot)
    }

    /// Enable a profile on a slot (disabling whatever was there). The match
    /// key is the ICCID or the catalog profile id.
    pub fn enable(&mut self, key: &str, slot: u32, now_ms: i64) -> Result<&EsimProfile, String> {
        if !(1..=2).contains(&slot) {
            return Err(format!(
                "slot {slot} does not exist (device has 2 eSIM slots)"
            ));
        }
        let idx = self
            .profiles
            .iter()
            .position(|p| p.iccid == key || p.profile_id == key)
            .ok_or_else(|| format!("profile {key:?} is not registered on the chip (esim list)"))?;
        // free the target slot (the moved profile frees its old slot by moving)
        if let Some(prev) = self.enabled_on_mut(slot) {
            prev.enabled = false;
            prev.state = "disabled".into();
        }
        let p = &mut self.profiles[idx];
        p.enabled = true;
        p.state = "active".into();
        p.slot = slot;
        p.port_index = slot - 1;
        p.registered_at = if p.registered_at == 0 {
            now_ms
        } else {
            p.registered_at
        };
        Ok(&self.profiles[idx])
    }

    /// Disable whatever is enabled on `slot`.
    pub fn disable(&mut self, slot: u32) -> Result<(), String> {
        match self.enabled_on_mut(slot) {
            Some(p) => {
                p.enabled = false;
                p.state = "disabled".into();
                Ok(())
            }
            None => Err(format!("no profile is enabled on slot {slot}")),
        }
    }

    /// Register (download) a catalog profile onto the chip and enable it on
    /// `slot`. The subscription identity is derived deterministically from
    /// the profile id, so re-registering the same profile is idempotent.
    pub fn register(
        &mut self,
        profile_id: &str,
        slot: u32,
        now_ms: i64,
    ) -> Result<&EsimProfile, String> {
        let Some(cat) = sim::find(profile_id) else {
            return Err(format!(
                "unknown carrier profile {profile_id:?} (android_sim action=list for the catalog)"
            ));
        };
        if !(1..=2).contains(&slot) {
            return Err(format!(
                "slot {slot} does not exist (device has 2 eSIM slots)"
            ));
        }
        if self.profiles.len() >= 8 {
            return Err(
                "eUICC is full (8 profiles - the GSMA consumer limit); esim delete first"
                    .to_string(),
            );
        }
        // idempotent: an already-registered profile is just (re-)enabled
        if let Some(p) = self.profiles.iter().find(|p| p.profile_id == profile_id) {
            let key = p.iccid.clone();
            self.enable(&key, slot, now_ms)?;
            return Ok(self.enabled_on(slot).expect("just enabled"));
        }
        let mut p = EsimProfile::derive(cat, now_ms);
        p.enabled = true;
        p.slot = slot;
        p.port_index = slot - 1;
        if let Some(prev) = self.enabled_on_mut(slot) {
            prev.enabled = false;
            prev.state = "disabled".into();
        }
        self.profiles.push(p);
        Ok(self.profiles.last().expect("just pushed"))
    }

    /// Delete a profile by ICCID (or catalog id).
    pub fn delete(&mut self, key: &str) -> Result<EsimProfile, String> {
        let idx = self
            .profiles
            .iter()
            .position(|p| p.iccid == key || p.profile_id == key)
            .ok_or_else(|| format!("profile {key:?} is not registered on the chip"))?;
        let p = self.profiles.remove(idx);
        if self.data_slot == p.slot && !self.profiles.iter().any(|q| q.enabled && q.slot == p.slot)
        {
            self.data_slot = self
                .profiles
                .iter()
                .find(|q| q.enabled)
                .map(|q| q.slot)
                .unwrap_or(1);
        }
        Ok(p)
    }

    /// Rename a profile.
    pub fn nickname(&mut self, key: &str, name: &str) -> Result<(), String> {
        let p = self
            .profiles
            .iter_mut()
            .find(|p| p.iccid == key || p.profile_id == key)
            .ok_or_else(|| format!("profile {key:?} is not registered on the chip"))?;
        p.nickname = name.to_string();
        Ok(())
    }

    /// Set the default mobile-data slot.
    pub fn set_data_slot(&mut self, slot: u32) -> Result<(), String> {
        if !(1..=2).contains(&slot) {
            return Err(format!(
                "slot {slot} does not exist (device has 2 eSIM slots)"
            ));
        }
        if self.enabled_on(slot).is_none() {
            return Err(format!(
                "no profile is enabled on slot {slot}; enable one first"
            ));
        }
        self.data_slot = slot;
        Ok(())
    }
}

// ── the radio / signal model ─────────────────────────────────────────────────

/// What the modem reports for one slot at one instant.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RadioSnapshot {
    pub slot: u32,
    /// Registered on a network (home or roaming).
    pub registered: bool,
    /// IN_SERVICE | OUT_OF_SERVICE | POWER_OFF | EMERGENCY_ONLY
    pub service_state: &'static str,
    pub roaming: bool,
    /// 0..=4 display bars.
    pub bars: u32,
    /// Reference signal received power, dBm (LTE/NR).
    pub rsrp: i32,
    /// RSRP in ASU units (rsrp + 140).
    pub rsrp_asu: i32,
    /// Legacy dBm signal indicator.
    pub dbm: i32,
    /// Radio access technology: "NR" | "LTE".
    pub technology: &'static str,
    /// Human-facing network tag ("5G", "5G UW", "LTE", ...).
    pub network_display: String,
    /// Band ("n41", "n77", "B4", ...).
    pub band: String,
    pub earfcn: u32,
    pub pci: u32,
    pub tac: u32,
    pub ci: u32,
    pub operator_alpha: String,
    /// MCC+MNC, no separator.
    pub operator_numeric: String,
    /// CONNECTED | DISCONNECTED
    pub data_state: &'static str,
}

impl RadioSnapshot {
    /// The off state — radio powered down (airplane mode).
    pub fn off(slot: u32) -> Self {
        Self {
            slot,
            registered: false,
            service_state: "POWER_OFF",
            roaming: false,
            bars: 0,
            rsrp: 0,
            rsrp_asu: 0,
            dbm: 0,
            technology: "NONE",
            network_display: "NONE".into(),
            band: String::new(),
            earfcn: 0,
            pci: 0,
            tac: 0,
            ci: 0,
            operator_alpha: String::new(),
            operator_numeric: String::new(),
            data_state: "DISCONNECTED",
        }
    }
}

/// Per-carrier radio geography — band, channel, cell identifiers and the
/// signal envelope. Unknown carriers get a deterministic band derived from
/// their catalog entry.
fn radio_profile(profile: &EsimProfile) -> (&'static str, String, u32, i32, u32, u32, u32) {
    // (technology, band, earfcn, base_rsrp, pci, tac, ci)
    match profile.profile_id.as_str() {
        // T-Mobile US: n41 (2.5 GHz) mid-band, typically strong
        "us-tmobile" => ("NR", "n41".into(), 509980, -84, 168, 30801, 44_910_725),
        // AT&T US: n77 (3.7 GHz) C-band, slightly weaker than mid-band
        "us-att" => ("NR", "n77".into(), 646336, -92, 421, 180252, 12_044_231),
        "us-verizon" => ("NR", "n77".into(), 648856, -90, 262, 52412, 33_180_402),
        // UK: LTE low/mid bands
        "uk-vodafone" => ("LTE", "B7".into(), 3150, -91, 287, 23015, 27_216_441),
        "uk-ee" => ("LTE", "B3".into(), 1650, -89, 301, 24061, 8_344_102),
        "uk-o2" => ("LTE", "B20".into(), 6300, -95, 113, 21043, 19_844_320),
        "uk-three" => ("LTE", "B1".into(), 300, -93, 394, 22156, 15_304_820),
        // Pakistan: LTE
        "pk-jazz" => ("LTE", "B3".into(), 1650, -88, 82, 50211, 4_881_042),
        "pk-zong" => ("LTE", "B3".into(), 1700, -94, 151, 50214, 7_112_044),
        "pk-telenor" => ("LTE", "B1".into(), 350, -96, 222, 50216, 9_921_032),
        "pk-ufone" => ("LTE", "B1".into(), 400, -97, 407, 50212, 11_441_067),
        "ae-etisalat" => ("NR", "n78".into(), 631536, -87, 351, 42402, 21_504_311),
        "sa-stc" => ("NR", "n78".into(), 633216, -90, 448, 42003, 18_112_903),
        "in-jio" => ("LTE", "B3".into(), 1475, -89, 128, 40418, 31_204_562),
        "de-telekom" => ("NR", "n78".into(), 638816, -86, 502, 26201, 5_401_238),
        _ => {
            let h = fnv64(&format!("radio-{}", profile.profile_id));
            let nr = profile.network.starts_with('5');
            let band = if nr {
                [
                    ("n78".to_string(), 633216u32),
                    ("n1".into(), 428500u32),
                    ("n3".into(), 464620u32),
                ][(h % 3) as usize]
                    .clone()
            } else {
                [
                    ("B3".to_string(), 1650u32),
                    ("B7".into(), 3150u32),
                    ("B1".into(), 300u32),
                ][(h % 3) as usize]
                    .clone()
            };
            (
                if nr { "NR" } else { "LTE" },
                band.0,
                band.1,
                -(84 + (h % 14) as i32),
                ((h >> 8) % 503) as u32,
                ((h >> 20) % 99_999) as u32,
                ((h >> 32) % 99_999_999) as u32,
            )
        }
    }
}

/// Signal bars from RSRP (the 4-bar display mapping Android's signal strength
/// service uses for LTE/NR RSRP).
pub fn bars_for_rsrp(rsrp: i32) -> u32 {
    match rsrp {
        r if r >= -88 => 4,
        r if r >= -96 => 3,
        r if r >= -105 => 2,
        r if r >= -114 => 1,
        _ => 0,
    }
}

/// Sample one slot's radio at `now_ms`. Pure: the compositor, `getprop`,
/// `dumpsys` and the tools see identical values for identical inputs.
pub fn sample_radio(
    slot: u32,
    registry: &EsimRegistry,
    airplane: bool,
    mobile_data: bool,
    now_ms: i64,
) -> RadioSnapshot {
    if airplane {
        return RadioSnapshot::off(slot);
    }
    let Some(profile) = registry.enabled_on(slot) else {
        let mut s = RadioSnapshot::off(slot);
        s.service_state = "OUT_OF_SERVICE";
        return s;
    };
    let (tech, band, earfcn, base, pci, tac, ci) = radio_profile(profile);
    // smooth, time-varying signal envelope: three slow waves + fast jitter,
    // ±7.5 dB around the carrier's base level
    let t = (now_ms % 3_600_000) as f64 / 1000.0;
    let phase = (fnv64(&profile.profile_id) % 628) as f64 / 100.0;
    let wave = 4.5 * (t / 23.0 + phase).sin()
        + 2.5 * (t / 61.0 + phase * 1.7).sin()
        + 1.2 * (t / 9.5 + phase * 2.3).sin();
    let rsrp = (base as f64 + wave).round() as i32;
    let registered = true;
    let roaming = profile.country != "US";
    let data_ok = mobile_data && registry.data_slot == slot;
    RadioSnapshot {
        slot,
        registered,
        service_state: "IN_SERVICE",
        roaming,
        bars: bars_for_rsrp(rsrp),
        rsrp,
        rsrp_asu: rsrp + 140,
        dbm: rsrp,
        technology: tech,
        network_display: match profile.network.as_str() {
            "5G UW" => "5G".into(),
            "5G" => "5G".into(),
            "4.5G" => "LTE".into(),
            "4G" => "LTE".into(),
            other => other.into(),
        },
        band,
        earfcn,
        pci,
        tac,
        ci,
        operator_alpha: profile.carrier.clone(),
        operator_numeric: format!("{:03}{}", profile.mcc, profile.mnc),
        data_state: if data_ok { "CONNECTED" } else { "DISCONNECTED" },
    }
}

/// Wi-Fi link state at `now_ms` — (rssi_dbm, bars 0..=4).
pub fn sample_wifi(on: bool, now_ms: i64) -> (i32, u32) {
    if !on {
        return (0, 0);
    }
    let t = (now_ms % 600_000) as f64 / 1000.0;
    let rssi = (-52.0 - 8.0 * (t / 17.0).sin() - 4.0 * (t / 47.0 + 1.3).sin()).round() as i32;
    let bars = match rssi {
        r if r >= -55 => 4,
        r if r >= -65 => 3,
        r if r >= -75 => 2,
        _ => 1,
    };
    (rssi, bars)
}

/// Default Wi-Fi network the handset is attached to.
pub const WIFI_SSID: &str = "RomaNet-5G";

#[cfg(test)]
mod tests {
    use super::*;

    fn luhn_ok(digits: &str) -> bool {
        let check = digits.chars().last().unwrap() as u32 - '0' as u32;
        let body = &digits[..digits.len() - 1];
        luhn_check(body) as u32 - '0' as u32 == check
    }

    #[test]
    fn identity_is_deterministic() {
        let a = DeviceIdentity::factory();
        let b = DeviceIdentity::factory();
        assert_eq!(a.serial, b.serial, "same handset, same serial");
        assert_eq!(a.imei, b.imei);
        assert_ne!(a.imei, a.imei2, "two modem slots have distinct IMEIs");
    }

    #[test]
    fn identifiers_are_format_valid() {
        let id = DeviceIdentity::factory();
        assert_eq!(id.serial.len(), 12, "12-char serial");
        assert!(id.serial.chars().all(|c| c.is_ascii_alphanumeric()));
        for imei in [&id.imei, &id.imei2] {
            assert_eq!(imei.len(), 15, "IMEI is 15 digits: {imei}");
            assert!(imei.chars().all(|c| c.is_ascii_digit()));
            assert!(luhn_ok(imei), "IMEI passes Luhn: {imei}");
        }
        assert_eq!(id.eid.len(), 32, "EID is 32 digits");
        assert!(id.eid.starts_with("8904"));
        assert!(luhn_ok(&id.eid), "EID passes Luhn (SGP.22)");
        assert!(
            id.fingerprint.ends_with(":user/release-keys"),
            "physical fingerprint: {}",
            id.fingerprint
        );
        // no emulator markers anywhere in the presented identity
        let rendered = format!(
            "{} {} {} {} {} {}",
            id.model, id.brand, id.device, id.product, id.manufacturer, id.fingerprint
        )
        .to_lowercase();
        for marker in [
            "emulator", "generic", "goldfish", "ranchu", "sdk_", "romacode",
        ] {
            assert!(
                !rendered.contains(marker),
                "emulator marker {marker:?} leaked"
            );
        }
    }

    #[test]
    fn factory_registry_ships_two_registered_esims() {
        let reg = EsimRegistry::factory();
        assert_eq!(reg.profiles.len(), 2);
        let s1 = reg.enabled_on(1).expect("slot 1 has a profile");
        let s2 = reg.enabled_on(2).expect("slot 2 has a profile");
        for p in [s1, s2] {
            assert_eq!(p.state, "active");
            assert!(p.enabled);
            assert_eq!(p.iccid.len(), 19);
            assert!(luhn_ok(&p.iccid), "ICCID passes Luhn: {}", p.iccid);
            assert_eq!(p.imsi.len(), 15);
            assert!(p.imsi.chars().all(|c| c.is_ascii_digit()));
            assert!(p.msisdn.starts_with('+'), "line number is E.164");
            assert!(p.activation.starts_with("LPA:1$"), "GSMA activation code");
        }
        assert_ne!(s1.iccid, s2.iccid);
        assert!(s1.msisdn.contains("555"), "US fictional number range");
    }

    #[test]
    fn register_enable_switch_round_trip() {
        let mut reg = EsimRegistry::factory();
        let now = 1_726_547_182_000_i64;
        // register a third catalog profile onto slot 1
        let p = reg.register("uk-ee", 1, now).expect("uk-ee registers");
        assert!(p.enabled);
        assert_eq!(p.slot, 1);
        assert_eq!(p.iccid.len(), 19);
        assert!(luhn_ok(&p.iccid));
        // the previous slot-1 profile is now disabled, not lost
        assert_eq!(reg.profiles.len(), 3);
        assert!(reg
            .profiles
            .iter()
            .any(|q| q.profile_id == "us-tmobile" && !q.enabled));
        // switching back by ICCID is idempotent-friendly
        let key = reg
            .profiles
            .iter()
            .find(|q| q.profile_id == "us-tmobile")
            .unwrap()
            .iccid
            .clone();
        reg.enable(&key, 1, now).expect("switch back");
        assert_eq!(reg.enabled_on(1).unwrap().profile_id, "us-tmobile");
        assert_eq!(reg.enabled_on(2).unwrap().profile_id, "us-att");
        // re-registering the same profile is idempotent
        reg.register("us-tmobile", 1, now).expect("idempotent");
        assert_eq!(reg.profiles.len(), 3, "no duplicate profile");
        // delete works and never orphans a data slot
        let ee = reg
            .profiles
            .iter()
            .find(|q| q.profile_id == "uk-ee")
            .unwrap()
            .iccid
            .clone();
        reg.delete(&ee).expect("delete");
        assert_eq!(reg.profiles.len(), 2);
        // unknown profile is a clean error
        assert!(reg.register("no-such-carrier", 1, now).is_err());
        assert!(reg.set_data_slot(3).is_err(), "only 2 slots exist");
    }

    #[test]
    fn radio_samples_are_live_and_bounded() {
        let reg = EsimRegistry::factory();
        let t0 = 1_726_547_182_000_i64;
        let s = sample_radio(1, &reg, false, true, t0);
        assert!(s.registered);
        assert_eq!(s.service_state, "IN_SERVICE");
        assert!(!s.roaming, "home carrier");
        assert_eq!(s.operator_numeric, "310260");
        assert_eq!(
            s.data_state, "CONNECTED",
            "mobile data on for the data slot"
        );
        assert!(
            (-125..=-60).contains(&s.rsrp),
            "RSRP in a physical range: {}",
            s.rsrp
        );
        assert_eq!(s.rsrp_asu, s.rsrp + 140);
        assert_eq!(s.bars, bars_for_rsrp(s.rsrp));
        assert!(
            (1..=4).contains(&s.bars),
            "factory carriers are received: {}",
            s.bars
        );

        // the signal moves over time (a live radio, not a constant)
        let mut last_rsrp = s.rsrp;
        let mut changed = false;
        for ms in (0..40_000).step_by(1_000) {
            let r = sample_radio(1, &reg, false, true, t0 + ms).rsrp;
            if r != last_rsrp {
                changed = true;
            }
            assert!((-125..=-60).contains(&r));
            last_rsrp = r;
        }
        assert!(changed, "RSRP fluctuates over 40 s");

        // slot 2: registered too, but not the data slot
        let s2 = sample_radio(2, &reg, false, true, t0);
        assert!(s2.registered);
        assert_eq!(s2.data_state, "DISCONNECTED");
        assert_eq!(s2.operator_numeric, "310410");

        // airplane mode powers the radio off
        for slot in 1..=2 {
            let air = sample_radio(slot, &reg, true, true, t0);
            assert!(!air.registered);
            assert_eq!(air.service_state, "POWER_OFF");
            assert_eq!(air.bars, 0);
        }
        // mobile data off disconnects data without touching registration
        let nodata = sample_radio(1, &reg, false, false, t0);
        assert!(nodata.registered);
        assert_eq!(nodata.data_state, "DISCONNECTED");
    }

    #[test]
    fn roaming_follows_the_profile_country() {
        let mut reg = EsimRegistry::factory();
        reg.register("pk-jazz", 2, 1_726_547_182_000)
            .expect("register pk-jazz on slot 2");
        let s = sample_radio(2, &reg, false, true, 1_726_547_182_000);
        assert!(s.roaming, "a Pakistani profile registered in the US roams");
        assert_eq!(s.operator_numeric, "41001");
    }

    #[test]
    fn base_props_look_physical() {
        let id = DeviceIdentity::factory();
        let props = base_ro_props(&id);
        let get = |k: &str| props.iter().find(|(pk, _)| pk == k).map(|(_, v)| v.clone());
        assert_eq!(get("ro.build.type").as_deref(), Some("user"));
        assert_eq!(get("ro.build.tags").as_deref(), Some("release-keys"));
        assert_eq!(get("ro.product.model").as_deref(), Some(id.model));
        assert_eq!(get("ro.boot.verifiedbootstate").as_deref(), Some("orange"));
        assert!(get("ro.build.version.security_patch")
            .unwrap()
            .starts_with("20"));
        // keys are unique and sorted
        let mut keys: Vec<_> = props.iter().map(|(k, _)| k.clone()).collect();
        let n = keys.len();
        keys.sort();
        keys.dedup();
        assert_eq!(keys.len(), n, "duplicate prop key");
    }

    #[test]
    fn pixel_9_pro_identity_is_coherent() {
        let id = DeviceIdentity::factory();
        assert_eq!(id.model, "Pixel 9 Pro");
        assert_eq!(id.device, "caiman");
        assert_eq!(id.brand, "google");
        assert_eq!(id.manufacturer, "Google");
        assert_eq!(id.soc_model, "Tensor G4");
        assert_eq!(id.cpu_cores, 8);
        // RAM/ROM must match the real device: 16 GB LPDDR5X, 1 TB UFS variant
        assert_eq!(id.ram_mib, 16384, "16 GB RAM — the real Pixel 9 Pro spec");
        assert_eq!(id.rom_gib, 931, "1 TB UFS variant (931 GiB usable)");
        assert_eq!(id.android_release, "17");
        assert_eq!(id.sdk_level, 37);
        assert_eq!(
            id.fingerprint,
            "google/caiman/caiman:17/CP4A.261105.040.A1/16581829:user/release-keys"
        );
        assert_eq!(
            id.build_description(),
            "caiman-user 17 CP4A.261105.040.A1 16581829 release-keys"
        );
        let props = base_ro_props(&id);
        let get = |k: &str| props.iter().find(|(pk, _)| pk == k).map(|(_, v)| v.clone());
        assert_eq!(get("ro.board.platform").as_deref(), Some("zumapro"));
        assert_eq!(get("ro.boot.hardware.sku").as_deref(), Some("GEC77"));
        assert_eq!(get("ro.build.characteristics").as_deref(), Some("default"));
        assert_eq!(
            get("ro.build.version.security_patch").as_deref(),
            Some("2026-11-05")
        );
        assert_eq!(get("dalvik.vm.heapsize").as_deref(), Some("512m"));
        // Samsung platform markers must not leak onto a Pixel identity
        for key in [
            "ro.config.knox",
            "ro.config.tima",
            "ro.csc.sales_code",
            "ro.boot.em.status",
        ] {
            assert!(get(key).is_none(), "{key} must not exist on a Pixel");
        }
    }

    #[test]
    fn wifi_signal_lives_between_strong_and_fair() {
        let (r0, b0) = sample_wifi(true, 0);
        assert!((-80..=-45).contains(&r0));
        assert!((1..=4).contains(&b0));
        let (_, off) = sample_wifi(false, 0);
        assert_eq!(off, 0);
    }
}
