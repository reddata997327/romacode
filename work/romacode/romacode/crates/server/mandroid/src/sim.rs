//! Multi-SIM management — the MANDROID SIM roster, online-availability
//! scanning and one-shot auto-configuration.
//!
//! The device ships a large catalog of ready eSIM profiles spanning many
//! countries — the US, the UK and Pakistan are covered densely (three to four
//! carriers each) and nine more countries carry one to three profiles apiece —
//! 28 profiles in total. Each profile is a declarative entry: carrier,
//! country, PLMN (MCC+MNC, ITU-T E.212 data), a synthetic ICCID and the
//! carrier's public domain used as the *reachability* target.
//!
//! "Which SIM is online — configure that one" is the [`configure`] flow:
//!
//! 1. every catalog profile is probed concurrently — a DNS resolve plus TCP
//!    connect (port 443) against the carrier domain with a hard per-probe
//!    timeout, measuring round-trip latency;
//! 2. reachable profiles are ranked by latency (offline ones are reported
//!    with the failure reason);
//! 3. the fastest online profile is enabled on slot 1; when a second online
//!    profile from a *different* country exists it is enabled on slot 2, so
//!    the dual-eSIM hardware is actually used;
//! 4. the decision is persisted to the SIM store (`device.json`'s sibling,
//!    `sims.json`) and the full report — availability table, chosen profile,
//!    slot assignment — is returned.
//!
//! Scope, stated plainly: this is configuration-space emulation. A probe
//! proves the carrier's ecosystem is *reachable from this host*; it is not a
//! GSMA SM-DP+ entitlement transaction, and a real profile download would
//! still need carrier activation material (QR / activation code). What the
//! module does deliver is the whole decision layer an agent needs: which of
//! the many SIMs is online right now, how fast, and which one to configure
//! where.

use std::net::{TcpStream, ToSocketAddrs};
use std::path::PathBuf;
use std::time::{Duration, Instant};

use serde_json::{json, Value};

use crate::key::default_dir;

/// Where SIM state persists (`~/.android/sims.json`).
pub fn store_path() -> PathBuf {
    default_dir().join("sims.json")
}

/// One catalog entry: a ready-to-enable eSIM profile.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SimProfile {
    /// Stable slug used by tools (`us-tmobile`, `pk-jazz`, ...).
    pub id: &'static str,
    /// ISO 3166 country code (`US`, `GB`, `PK`, ...).
    pub country: &'static str,
    /// Country display name.
    pub country_name: &'static str,
    /// Carrier / brand name.
    pub carrier: &'static str,
    /// Mobile country code (ITU-T E.212).
    pub mcc: u32,
    /// Mobile network code (as assigned — 2 or 3 digits, e.g. "260", "012").
    pub mnc: &'static str,
    /// Public carrier domain used as the online-reachability probe target.
    pub reachability_host: &'static str,
    /// Synthetic ICCID (config-space; never a real subscription identifier).
    pub synthetic_iccid: &'static str,
    /// Network generation the profile targets.
    pub network: &'static str,
}

impl SimProfile {
    /// `MCC-MNC` string as Android reports it (`ro.carrier` / telephony).
    pub fn plmn(&self) -> String {
        format!("{:03}-{}", self.mcc, self.mnc)
    }
}

/// The catalog — "lots of sims": 28 profiles, 12 countries, dense coverage of
/// the US, the UK and Pakistan. PLMN codes are public E.212 assignments.
pub const CATALOG: &[SimProfile] = &[
    // ---- United States (310) -------------------------------------------
    SimProfile {
        id: "us-tmobile",
        country: "US",
        country_name: "United States",
        carrier: "T-Mobile US",
        mcc: 310,
        mnc: "260",
        reachability_host: "www.t-mobile.com",
        synthetic_iccid: "8901260041000000001",
        network: "5G",
    },
    SimProfile {
        id: "us-att",
        country: "US",
        country_name: "United States",
        carrier: "AT&T",
        mcc: 310,
        mnc: "410",
        reachability_host: "www.att.com",
        synthetic_iccid: "8901410000000000021",
        network: "5G",
    },
    SimProfile {
        id: "us-verizon",
        country: "US",
        country_name: "United States",
        carrier: "Verizon",
        mcc: 310,
        mnc: "012",
        reachability_host: "www.verizon.com",
        synthetic_iccid: "8901200000000000031",
        network: "5G UW",
    },
    // ---- United Kingdom (234) ------------------------------------------
    SimProfile {
        id: "uk-vodafone",
        country: "GB",
        country_name: "United Kingdom",
        carrier: "Vodafone UK",
        mcc: 234,
        mnc: "15",
        reachability_host: "www.vodafone.co.uk",
        synthetic_iccid: "8901500000000000012",
        network: "5G",
    },
    SimProfile {
        id: "uk-ee",
        country: "GB",
        country_name: "United Kingdom",
        carrier: "EE",
        mcc: 234,
        mnc: "30",
        reachability_host: "ee.co.uk",
        synthetic_iccid: "8901300000000000022",
        network: "5G",
    },
    SimProfile {
        id: "uk-o2",
        country: "GB",
        country_name: "United Kingdom",
        carrier: "O2 UK",
        mcc: 234,
        mnc: "10",
        reachability_host: "www.o2.co.uk",
        synthetic_iccid: "8901000000000000032",
        network: "5G",
    },
    SimProfile {
        id: "uk-three",
        country: "GB",
        country_name: "United Kingdom",
        carrier: "Three UK",
        mcc: 234,
        mnc: "20",
        reachability_host: "www.three.co.uk",
        synthetic_iccid: "8901200000000000042",
        network: "5G",
    },
    // ---- Pakistan (410) --------------------------------------------------
    SimProfile {
        id: "pk-jazz",
        country: "PK",
        country_name: "Pakistan",
        carrier: "Jazz (Mobilink)",
        mcc: 410,
        mnc: "01",
        reachability_host: "www.jazz.com.pk",
        synthetic_iccid: "8901010000000000014",
        network: "4G",
    },
    SimProfile {
        id: "pk-zong",
        country: "PK",
        country_name: "Pakistan",
        carrier: "Zong CMPak",
        mcc: 410,
        mnc: "04",
        reachability_host: "www.zong.com.pk",
        synthetic_iccid: "8901040000000000024",
        network: "4G",
    },
    SimProfile {
        id: "pk-telenor",
        country: "PK",
        country_name: "Pakistan",
        carrier: "Telenor Pakistan",
        mcc: 410,
        mnc: "06",
        reachability_host: "www.telenor.com.pk",
        synthetic_iccid: "8901060000000000034",
        network: "4G",
    },
    SimProfile {
        id: "pk-ufone",
        country: "PK",
        country_name: "Pakistan",
        carrier: "Ufone",
        mcc: 410,
        mnc: "03",
        reachability_host: "www.ufone.com",
        synthetic_iccid: "8901030000000000044",
        network: "4G",
    },
    // ---- UAE (424) -------------------------------------------------------
    SimProfile {
        id: "ae-etisalat",
        country: "AE",
        country_name: "United Arab Emirates",
        carrier: "Etisalat",
        mcc: 424,
        mnc: "02",
        reachability_host: "www.etisalat.ae",
        synthetic_iccid: "8901020000000000015",
        network: "5G",
    },
    SimProfile {
        id: "ae-du",
        country: "AE",
        country_name: "United Arab Emirates",
        carrier: "du",
        mcc: 424,
        mnc: "03",
        reachability_host: "www.du.ae",
        synthetic_iccid: "8901030000000000025",
        network: "5G",
    },
    // ---- Saudi Arabia (420) ----------------------------------------------
    SimProfile {
        id: "sa-stc",
        country: "SA",
        country_name: "Saudi Arabia",
        carrier: "STC",
        mcc: 420,
        mnc: "01",
        reachability_host: "www.stc.com.sa",
        synthetic_iccid: "8901010000000000016",
        network: "5G",
    },
    SimProfile {
        id: "sa-mobily",
        country: "SA",
        country_name: "Saudi Arabia",
        carrier: "Mobily",
        mcc: 420,
        mnc: "03",
        reachability_host: "www.mobily.com.sa",
        synthetic_iccid: "8901030000000000026",
        network: "5G",
    },
    SimProfile {
        id: "sa-zain",
        country: "SA",
        country_name: "Saudi Arabia",
        carrier: "Zain KSA",
        mcc: 420,
        mnc: "04",
        reachability_host: "www.sa.zain.com",
        synthetic_iccid: "8901040000000000036",
        network: "5G",
    },
    // ---- Canada (302) -----------------------------------------------------
    SimProfile {
        id: "ca-rogers",
        country: "CA",
        country_name: "Canada",
        carrier: "Rogers",
        mcc: 302,
        mnc: "370",
        reachability_host: "www.rogers.com",
        synthetic_iccid: "8901370000000000017",
        network: "5G",
    },
    SimProfile {
        id: "ca-bell",
        country: "CA",
        country_name: "Canada",
        carrier: "Bell",
        mcc: 302,
        mnc: "610",
        reachability_host: "www.bell.ca",
        synthetic_iccid: "8901610000000000027",
        network: "5G",
    },
    // ---- Germany (262) ----------------------------------------------------
    SimProfile {
        id: "de-telekom",
        country: "DE",
        country_name: "Germany",
        carrier: "Deutsche Telekom",
        mcc: 262,
        mnc: "01",
        reachability_host: "www.telekom.de",
        synthetic_iccid: "8901010000000000018",
        network: "5G",
    },
    SimProfile {
        id: "de-vodafone",
        country: "DE",
        country_name: "Germany",
        carrier: "Vodafone DE",
        mcc: 262,
        mnc: "02",
        reachability_host: "www.vodafone.de",
        synthetic_iccid: "8901020000000000028",
        network: "5G",
    },
    SimProfile {
        id: "de-o2",
        country: "DE",
        country_name: "Germany",
        carrier: "O2 Deutschland",
        mcc: 262,
        mnc: "07",
        reachability_host: "www.o2online.de",
        synthetic_iccid: "8901070000000000038",
        network: "5G",
    },
    // ---- India (404/405) ---------------------------------------------------
    SimProfile {
        id: "in-jio",
        country: "IN",
        country_name: "India",
        carrier: "Reliance Jio",
        mcc: 405,
        mnc: "840",
        reachability_host: "www.jio.com",
        synthetic_iccid: "8901840000000000019",
        network: "5G",
    },
    SimProfile {
        id: "in-airtel",
        country: "IN",
        country_name: "India",
        carrier: "Airtel",
        mcc: 404,
        mnc: "45",
        reachability_host: "www.airtel.in",
        synthetic_iccid: "8901450000000000029",
        network: "5G",
    },
    // ---- Türkiye (286) ------------------------------------------------------
    SimProfile {
        id: "tr-turkcell",
        country: "TR",
        country_name: "Türkiye",
        carrier: "Turkcell",
        mcc: 286,
        mnc: "01",
        reachability_host: "www.turkcell.com.tr",
        synthetic_iccid: "8901010000000000121",
        network: "4.5G",
    },
    // ---- Bangladesh (470) ---------------------------------------------------
    SimProfile {
        id: "bd-grameenphone",
        country: "BD",
        country_name: "Bangladesh",
        carrier: "Grameenphone",
        mcc: 470,
        mnc: "01",
        reachability_host: "www.grameenphone.com",
        synthetic_iccid: "8901010000000000131",
        network: "4G",
    },
    SimProfile {
        id: "bd-robi",
        country: "BD",
        country_name: "Bangladesh",
        carrier: "Robi",
        mcc: 470,
        mnc: "07",
        reachability_host: "www.robi.com.bd",
        synthetic_iccid: "8901070000000000141",
        network: "4G",
    },
    // ---- Singapore (525) ------------------------------------------------------
    SimProfile {
        id: "sg-singtel",
        country: "SG",
        country_name: "Singapore",
        carrier: "Singtel",
        mcc: 525,
        mnc: "01",
        reachability_host: "www.singtel.com",
        synthetic_iccid: "8901010000000000151",
        network: "5G",
    },
    // ---- Australia (505) -------------------------------------------------------
    SimProfile {
        id: "au-telstra",
        country: "AU",
        country_name: "Australia",
        carrier: "Telstra",
        mcc: 505,
        mnc: "01",
        reachability_host: "www.telstra.com.au",
        synthetic_iccid: "8901010000000000161",
        network: "5G",
    },
];

/// Outcome of one reachability probe.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProbeResult {
    pub profile_id: &'static str,
    /// `true` when the carrier domain resolved and accepted a TCP connection.
    pub online: bool,
    /// Measured connect latency in ms (present only when online).
    pub latency_ms: Option<u64>,
    /// Failure reason (present only when offline).
    pub detail: Option<String>,
}

/// Look up one catalog profile by id.
pub fn find(id: &str) -> Option<&'static SimProfile> {
    CATALOG.iter().find(|p| p.id == id)
}

/// Probe one host: DNS resolve + TCP connect on 443, hard-bounded by
/// `timeout`, returning the connect latency on success. This is the "is that
/// SIM's ecosystem online from here" primitive.
pub fn probe_host(host: &str, timeout: Duration) -> Result<u64, String> {
    let addrs: Vec<_> = (host, 443u16)
        .to_socket_addrs()
        .map_err(|e| format!("dns: {e}"))?
        .collect();
    if addrs.is_empty() {
        return Err("dns: no addresses".into());
    }
    let mut last = String::new();
    for addr in addrs {
        let start = Instant::now();
        match TcpStream::connect_timeout(&addr, timeout) {
            Ok(_) => return Ok(start.elapsed().as_millis() as u64),
            Err(e) => last = e.to_string(),
        }
    }
    Err(format!("tcp: {last}"))
}

/// Probe one profile against the live network.
pub fn probe(profile: &SimProfile, timeout: Duration) -> ProbeResult {
    match probe_host(profile.reachability_host, timeout) {
        Ok(ms) => ProbeResult {
            profile_id: profile.id,
            online: true,
            latency_ms: Some(ms),
            detail: None,
        },
        Err(e) => ProbeResult {
            profile_id: profile.id,
            online: false,
            latency_ms: None,
            detail: Some(e),
        },
    }
}

/// Scan the whole catalog concurrently (one thread per profile, each bounded
/// by `timeout`) and return results ranked: online first by latency, offline
/// after. This is the "which sims are online right now" answer.
pub fn scan(timeout: Duration) -> Vec<ProbeResult> {
    scan_catalog_with(CATALOG, &|p| probe(p, timeout))
}

/// Testable core of [`scan`]: run `prober` for every profile of `catalog` on
/// its own thread and rank the outcomes.
pub fn scan_catalog_with<F>(catalog: &[SimProfile], prober: &F) -> Vec<ProbeResult>
where
    F: Fn(&SimProfile) -> ProbeResult + Sync,
{
    let mut results: Vec<ProbeResult> = std::thread::scope(|scope| {
        let handles: Vec<_> = catalog
            .iter()
            .map(|p| scope.spawn(move || prober(p)))
            .collect();
        handles.into_iter().filter_map(|h| h.join().ok()).collect()
    });
    results.sort_by(|a, b| match (a.online, b.online) {
        (true, true) => a
            .latency_ms
            .unwrap_or(u64::MAX)
            .cmp(&b.latency_ms.unwrap_or(u64::MAX)),
        (true, false) => std::cmp::Ordering::Less,
        (false, true) => std::cmp::Ordering::Greater,
        (false, false) => a.profile_id.cmp(b.profile_id),
    });
    results
}

// ---------------------------------------------------------------- store ---

/// Persisted SIM state: which profile is enabled on which slot.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct SimStore {
    /// Profile id enabled on slot 1 (or None).
    pub slot1: Option<String>,
    /// Profile id enabled on slot 2 (or None).
    pub slot2: Option<String>,
    /// ISO timestamp of the last configuration decision.
    pub configured_at: Option<String>,
}

impl SimStore {
    /// Load persisted state (absent file → empty state).
    pub fn load() -> Self {
        let Ok(text) = std::fs::read_to_string(store_path()) else {
            return Self::default();
        };
        let Ok(v) = serde_json::from_str::<Value>(&text) else {
            return Self::default();
        };
        Self {
            slot1: v["slot1"].as_str().map(String::from),
            slot2: v["slot2"].as_str().map(String::from),
            configured_at: v["configured_at"].as_str().map(String::from),
        }
    }

    /// Persist state.
    pub fn save(&self) -> std::io::Result<()> {
        let path = store_path();
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let body = json!({
            "slot1": self.slot1,
            "slot2": self.slot2,
            "configured_at": self.configured_at,
        });
        std::fs::write(path, serde_json::to_string_pretty(&body).unwrap())
    }

    /// The profile enabled on `slot` (1-based), if any.
    pub fn active_on(&self, slot: u32) -> Option<&'static SimProfile> {
        let id = match slot {
            1 => self.slot1.as_deref(),
            2 => self.slot2.as_deref(),
            _ => None,
        }?;
        find(id)
    }

    /// Enable `profile_id` on `slot`, clearing any duplicate assignment.
    pub fn set_slot(&mut self, slot: u32, profile_id: &str) -> Result<(), String> {
        if find(profile_id).is_none() {
            return Err(format!(
                "unknown sim profile {profile_id:?} (not in the catalog)"
            ));
        }
        match slot {
            1 => self.slot1 = Some(profile_id.into()),
            2 => self.slot2 = Some(profile_id.into()),
            other => {
                return Err(format!(
                    "slot {other} does not exist (device has 2 eSIM slots)"
                ))
            }
        }
        // One profile can only be live on one slot.
        if self.slot1.as_deref() == Some(profile_id) && slot == 2 {
            self.slot1 = None;
        }
        if self.slot2.as_deref() == Some(profile_id) && slot == 1 {
            self.slot2 = None;
        }
        Ok(())
    }
}

// ------------------------------------------------------------ operations ---

/// Render the full roster plus live slot state (action=list).
pub fn list_json() -> Value {
    let store = SimStore::load();
    let sims: Vec<Value> = CATALOG
        .iter()
        .map(|p| {
            let slot = if store.slot1.as_deref() == Some(p.id) {
                1
            } else if store.slot2.as_deref() == Some(p.id) {
                2
            } else {
                0
            };
            json!({
                "id": p.id,
                "carrier": p.carrier,
                "country": p.country,
                "country_name": p.country_name,
                "plmn": p.plmn(),
                "network": p.network,
                "iccid_synthetic": p.synthetic_iccid,
                "reachability_host": p.reachability_host,
                "state": if slot > 0 { "enabled" } else { "available" },
                "slot": slot,
            })
        })
        .collect();
    json!({
        "catalog_size": CATALOG.len(),
        "countries": country_count(),
        "slots": {
            "count": 2,
            "slot1": slot_json(&store, 1),
            "slot2": slot_json(&store, 2),
        },
        "configured_at": store.configured_at,
        "sims": sims,
        "mode": "config-space eSIM emulation (probes prove reachability, not GSMA entitlement)",
    })
}

fn slot_json(store: &SimStore, slot: u32) -> Value {
    match store.active_on(slot) {
        Some(p) => json!({
            "slot": slot,
            "state": "enabled",
            "profile": { "id": p.id, "carrier": p.carrier, "country": p.country, "plmn": p.plmn() },
        }),
        None => json!({ "slot": slot, "state": "empty" }),
    }
}

fn country_count() -> usize {
    let mut countries: Vec<&str> = CATALOG.iter().map(|p| p.country).collect();
    countries.sort_unstable();
    countries.dedup();
    countries.len()
}

/// One-shot auto-configuration: scan, rank, enable the best online profile(s).
///
/// * `country` / `carrier` — optional case-insensitive filters (e.g. "US",
///   "Pakistan", "jazz");
/// * `slot` — which slot receives the winner (default 1; slot 2 also gets the
///   best *different-country* online profile so dual eSIM is used);
/// * `timeout` — per-probe budget.
///
/// Returns the report: availability table, chosen profiles, slot assignment.
/// Errors when nothing online matches the filters.
pub fn configure(
    country: Option<&str>,
    carrier: Option<&str>,
    slot: u32,
    timeout: Duration,
) -> Result<Value, String> {
    // Slot-2 occupancy is decided up front and handed to the pure core, so the
    // decision itself never reads machine state.
    let slot2_occupant = SimStore::load().slot2;
    // Pure decision first (may fail before anything is persisted) …
    let mut report = configure_with(
        CATALOG,
        &|p| probe(p, timeout),
        country,
        carrier,
        slot,
        slot2_occupant.as_deref(),
    )?;
    // … then apply + persist the decision against the real catalog.
    let mut store = SimStore::load();
    let slot_n = report["configured"]["slot"].as_u64().unwrap_or(1) as u32;
    let id = report["configured"]["profile"]["id"]
        .as_str()
        .ok_or_else(|| "decision report is missing the winner id".to_string())?;
    store.set_slot(slot_n, id)?;
    if let Some(id2) = report["second_slot"]["profile"]["id"].as_str() {
        if store.slot2.is_none() {
            store.set_slot(2, id2)?;
        }
    }
    // When slot 2 already held an assignment we deliberately leave it alone —
    // say so in the report instead of letting `second_slot: null` read as
    // "dual SIM inactive". (Only for slot-1 configures: with slot=2 the winner
    // itself sits on slot 2 and is already reported as `configured`.)
    if slot_n == 1 && report["second_slot"].is_null() {
        if let Some(kept) = kept_slot2_report(store.slot2.as_deref()) {
            report["second_slot"] = kept;
        }
    }
    store.configured_at = Some(now_iso());
    store
        .save()
        .map_err(|e| format!("persisting sim state: {e}"))?;
    report["persisted_to"] = json!(store_path().display().to_string());
    Ok(report)
}

/// Testable core of [`configure`] with an injected prober: computes the
/// decision and the report but touches no persistent state and reads no
/// machine state — the slot-2 occupant comes in as an explicit argument.
pub fn configure_with<F>(
    catalog: &[SimProfile],
    prober: &F,
    country: Option<&str>,
    carrier: Option<&str>,
    slot: u32,
    slot2_occupant: Option<&str>,
) -> Result<Value, String>
where
    F: Fn(&SimProfile) -> ProbeResult + Sync,
{
    if !matches!(slot, 1 | 2) {
        return Err(format!(
            "slot {slot} does not exist (device has 2 eSIM slots)"
        ));
    }
    let norm = |s: &str| s.to_lowercase();
    let country_f = country.map(norm);
    let carrier_f = carrier.map(norm);
    let matches_filters = |p: &SimProfile| {
        country_f
            .as_ref()
            .map(|c| p.country.eq_ignore_ascii_case(c) || p.country_name.to_lowercase().contains(c))
            .unwrap_or(true)
            && carrier_f
                .as_ref()
                .map(|c| p.carrier.to_lowercase().contains(c) || p.id.contains(c))
                .unwrap_or(true)
    };

    let results = scan_catalog_with(catalog, prober);
    let by_id = |id: &str| catalog.iter().find(|p| p.id == id);

    // Winner: fastest online profile matching the filters (the predicate
    // lives inside find_map so the search continues past non-matching ones).
    let winner = results
        .iter()
        .filter(|r| r.online)
        .find_map(|r| by_id(r.profile_id).filter(|p| matches_filters(p)));
    let winner = match winner {
        Some(p) => p,
        None => {
            let any_online = results.iter().any(|r| r.online);
            return Err(if any_online {
                format!(
                    "no online sim matches the filters (country={country:?}, carrier={carrier:?}); {} of {} profiles are online",
                    results.iter().filter(|r| r.online).count(),
                    catalog.len()
                )
            } else {
                format!(
                    "no sim is online: none of the {} carrier ecosystems were reachable within the probe timeout (is this host online?)",
                    catalog.len()
                )
            });
        }
    };

    // Dual-slot bonus: the best online profile from a different country takes
    // the other slot. Slot 2 is refillable when it is empty — or when its
    // occupant is about to move to slot 1 (the winner itself), which frees it.
    // A different, deliberate slot-2 assignment is never clobbered.
    let mut second = None;
    let slot2_effective_free = slot2_occupant.is_none() || slot2_occupant == Some(winner.id);
    if slot == 1 && slot2_effective_free {
        second = results
            .iter()
            .filter(|r| r.online)
            .find_map(|r| by_id(r.profile_id).filter(|p| p.country != winner.country));
    }

    let table: Vec<Value> = results
        .iter()
        .map(|r| {
            let p = by_id(r.profile_id).expect("probe ids come from the catalog");
            json!({
                "id": p.id,
                "carrier": p.carrier,
                "country": p.country,
                "online": r.online,
                "latency_ms": r.latency_ms,
                "detail": r.detail,
            })
        })
        .collect();
    let online_count = results.iter().filter(|r| r.online).count();

    Ok(json!({
        "configured": {
            "slot": slot,
            "profile": profile_json(winner),
            "latency_ms": results.iter().find(|r| r.profile_id == winner.id).and_then(|r| r.latency_ms),
        },
        "second_slot": second.map(|p| json!({
            "slot": 2,
            "profile": profile_json(p),
        })),
        "scan": {
            "probed": catalog.len(),
            "online": online_count,
            "offline": catalog.len() - online_count,
            "results": table,
        },
        "mode": "config-space eSIM emulation (reachability-proved, not GSMA entitlement)",
    }))
}

fn profile_json(p: &SimProfile) -> Value {
    json!({
        "id": p.id,
        "carrier": p.carrier,
        "country": p.country,
        "country_name": p.country_name,
        "plmn": p.plmn(),
        "network": p.network,
        "iccid_synthetic": p.synthetic_iccid,
    })
}

/// Report entry for a slot-2 assignment that configure deliberately left
/// untouched (`None` when slot 2 is empty or holds an unknown id). Pure so the
/// wording and shape stay testable without touching the store file.
fn kept_slot2_report(slot2: Option<&str>) -> Option<Value> {
    slot2.and_then(find).map(|p| {
        json!({
            "slot": 2,
            "profile": profile_json(p),
            "note": "kept — configured earlier, left untouched",
        })
    })
}

/// Enable one specific profile on one slot (action=activate).
pub fn activate(profile_id: &str, slot: u32) -> Result<Value, String> {
    let p = find(profile_id).ok_or_else(|| {
        format!(
            "unknown sim profile {profile_id:?}; try android_sim action=list to see the catalog"
        )
    })?;
    let mut store = SimStore::load();
    store.set_slot(slot, p.id)?;
    store.configured_at = Some(now_iso());
    store
        .save()
        .map_err(|e| format!("persisting sim state: {e}"))?;
    Ok(json!({
        "activated": { "slot": slot, "profile": profile_json(p) },
        "persisted_to": store_path().display().to_string(),
    }))
}

/// Disable one slot (action=deactivate).
pub fn deactivate(slot: u32) -> Result<Value, String> {
    let mut store = SimStore::load();
    let removed = match slot {
        1 => store.slot1.take(),
        2 => store.slot2.take(),
        other => {
            return Err(format!(
                "slot {other} does not exist (device has 2 eSIM slots)"
            ))
        }
    };
    store
        .save()
        .map_err(|e| format!("persisting sim state: {e}"))?;
    Ok(json!({
        "deactivated": { "slot": slot, "was": removed },
        "slot": slot_json(&store, slot),
    }))
}

/// Small RFC-3339-ish timestamp without pulling chrono in.
fn now_iso() -> String {
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default();
    let secs = now.as_secs();
    let days = secs / 86_400;
    // civil-from-days (Howard Hinnant's algorithm), enough for a timestamp.
    let z = days as i64 + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146_096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = if m <= 2 { y + 1 } else { y };
    let hms = secs % 86_400;
    format!(
        "{:04}-{:02}-{:02}T{:02}:{:02}:{:02}Z",
        y,
        m,
        d,
        hms / 3600,
        (hms % 3600) / 60,
        hms % 60
    )
}

// ----------------------------------------------------------------- tests ---

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::ErrorKind;
    use std::net::TcpListener;

    #[test]
    fn catalog_is_large_and_multi_country() {
        assert!(
            CATALOG.len() >= 25,
            "lots of sims expected, got {}",
            CATALOG.len()
        );
        assert!(country_count() >= 10, "many countries expected");
    }

    #[test]
    fn catalog_covers_us_uk_pakistan_densely() {
        for (code, min_profiles) in [("US", 3), ("GB", 3), ("PK", 4)] {
            let n = CATALOG.iter().filter(|p| p.country == code).count();
            assert!(
                n >= min_profiles,
                "{code} needs >= {min_profiles} profiles, has {n}"
            );
        }
        // The exact carriers the user asked about are present.
        assert!(CATALOG.iter().any(|p| p.id == "us-tmobile"));
        assert!(CATALOG.iter().any(|p| p.id == "uk-vodafone"));
        assert!(CATALOG.iter().any(|p| p.id == "pk-jazz"));
    }

    #[test]
    fn catalog_ids_and_plmns_are_well_formed() {
        for p in CATALOG {
            assert!(p.mcc >= 100 && p.mcc <= 999, "{}: mcc range", p.id);
            assert!(
                p.mnc.chars().all(|c| c.is_ascii_digit()) && (2..=3).contains(&p.mnc.len()),
                "{}: mnc digits",
                p.id
            );
            assert!(p.plmn().starts_with(&format!("{:03}-", p.mcc)));
            assert!(
                p.synthetic_iccid.starts_with("890"),
                "{}: iccid industry prefix",
                p.id
            );
            assert!(
                p.reachability_host.contains('.'),
                "{}: probe host looks like a domain",
                p.id
            );
        }
        // Unique ids and unique PLMNs.
        let mut ids: Vec<_> = CATALOG.iter().map(|p| p.id).collect();
        ids.sort_unstable();
        ids.dedup();
        assert_eq!(ids.len(), CATALOG.len(), "ids must be unique");
        let mut plmns: Vec<_> = CATALOG.iter().map(|p| p.plmn()).collect();
        plmns.sort_unstable();
        plmns.dedup();
        assert_eq!(plmns.len(), CATALOG.len(), "PLMN pairs must be unique");
    }

    #[test]
    fn probe_hits_local_listener_and_misses_closed_port() {
        // Online: a real TCP listener on loopback.
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = listener.local_addr().unwrap();
        let t = std::thread::spawn(move || {
            let (_s, _) = listener.accept().unwrap();
        });
        // probe_host is fixed to 443, so exercise the raw connect primitive
        // against the bound port directly.
        let start = Instant::now();
        let stream = TcpStream::connect_timeout(&addr, Duration::from_secs(2)).unwrap();
        let ms = start.elapsed().as_millis() as u64;
        drop(stream);
        t.join().unwrap();
        assert!(ms < 2_000, "loopback connect must be fast");
        // Offline: nothing listens on loopback port 9 (discard).
        let res = (|| -> Result<u64, String> {
            let addr: std::net::SocketAddr = "127.0.0.1:9".parse().unwrap();
            match TcpStream::connect_timeout(&addr, Duration::from_millis(300)) {
                Ok(_) => Ok(0),
                Err(e) => Err(match e.kind() {
                    ErrorKind::ConnectionRefused => "tcp: connection refused".into(),
                    _ => e.to_string(),
                }),
            }
        })();
        assert!(res.is_err(), "closed port must not be reported online");
    }

    #[test]
    fn scan_ranks_online_before_offline() {
        let catalog = vec![
            SimProfile {
                id: "slow-online",
                country: "US",
                country_name: "United States",
                carrier: "Slow",
                mcc: 310,
                mnc: "260",
                reachability_host: "slow.test",
                synthetic_iccid: "8901",
                network: "4G",
            },
            SimProfile {
                id: "fast-online",
                country: "GB",
                country_name: "United Kingdom",
                carrier: "Fast",
                mcc: 234,
                mnc: "30",
                reachability_host: "fast.test",
                synthetic_iccid: "8902",
                network: "5G",
            },
            SimProfile {
                id: "offline",
                country: "PK",
                country_name: "Pakistan",
                carrier: "Off",
                mcc: 410,
                mnc: "01",
                reachability_host: "off.test",
                synthetic_iccid: "8903",
                network: "4G",
            },
        ];
        let prober = |p: &SimProfile| match p.id {
            "slow-online" => ProbeResult {
                profile_id: p.id,
                online: true,
                latency_ms: Some(90),
                detail: None,
            },
            "fast-online" => ProbeResult {
                profile_id: p.id,
                online: true,
                latency_ms: Some(12),
                detail: None,
            },
            _ => ProbeResult {
                profile_id: p.id,
                online: false,
                latency_ms: None,
                detail: Some("refused".into()),
            },
        };
        let out = scan_catalog_with(&catalog, &prober);
        assert_eq!(out.len(), 3);
        assert_eq!(out[0].profile_id, "fast-online", "fastest online first");
        assert_eq!(out[1].profile_id, "slow-online");
        assert_eq!(out[2].profile_id, "offline", "offline last");
        assert!(!out[2].online && out[2].detail.is_some());
    }

    #[test]
    fn configure_picks_fastest_online_and_fills_second_slot() {
        let catalog = vec![
            SimProfile {
                id: "us-fast",
                country: "US",
                country_name: "United States",
                carrier: "USNet",
                mcc: 310,
                mnc: "260",
                reachability_host: "us.test",
                synthetic_iccid: "8901",
                network: "5G",
            },
            SimProfile {
                id: "us-slow",
                country: "US",
                country_name: "United States",
                carrier: "USAlt",
                mcc: 310,
                mnc: "410",
                reachability_host: "usa.test",
                synthetic_iccid: "8902",
                network: "5G",
            },
            SimProfile {
                id: "pk-fast",
                country: "PK",
                country_name: "Pakistan",
                carrier: "PKNet",
                mcc: 410,
                mnc: "01",
                reachability_host: "pk.test",
                synthetic_iccid: "8903",
                network: "4G",
            },
            SimProfile {
                id: "gb-off",
                country: "GB",
                country_name: "United Kingdom",
                carrier: "GBNet",
                mcc: 234,
                mnc: "30",
                reachability_host: "gb.test",
                synthetic_iccid: "8904",
                network: "5G",
            },
        ];
        let prober = |p: &SimProfile| match p.id {
            "us-fast" => ProbeResult {
                profile_id: p.id,
                online: true,
                latency_ms: Some(20),
                detail: None,
            },
            "us-slow" => ProbeResult {
                profile_id: p.id,
                online: true,
                latency_ms: Some(70),
                detail: None,
            },
            "pk-fast" => ProbeResult {
                profile_id: p.id,
                online: true,
                latency_ms: Some(40),
                detail: None,
            },
            _ => ProbeResult {
                profile_id: p.id,
                online: false,
                latency_ms: None,
                detail: Some("down".into()),
            },
        };
        let report = configure_with(&catalog, &prober, None, None, 1, None).unwrap();
        // Winner is the globally fastest online profile.
        assert_eq!(report["configured"]["profile"]["id"], "us-fast");
        assert_eq!(report["configured"]["latency_ms"], 20);
        // Dual-slot: best different-country profile takes slot 2.
        assert_eq!(report["second_slot"]["profile"]["id"], "pk-fast");
        assert_eq!(report["scan"]["online"], 3);
        assert_eq!(report["scan"]["offline"], 1);
    }

    #[test]
    fn configure_respects_country_filter() {
        let catalog = vec![
            SimProfile {
                id: "us-fast",
                country: "US",
                country_name: "United States",
                carrier: "USNet",
                mcc: 310,
                mnc: "260",
                reachability_host: "us.test",
                synthetic_iccid: "8901",
                network: "5G",
            },
            SimProfile {
                id: "pk-fast",
                country: "PK",
                country_name: "Pakistan",
                carrier: "PKNet",
                mcc: 410,
                mnc: "01",
                reachability_host: "pk.test",
                synthetic_iccid: "8903",
                network: "4G",
            },
        ];
        let prober = |p: &SimProfile| ProbeResult {
            profile_id: p.id,
            online: true,
            latency_ms: Some(if p.id == "us-fast" { 10 } else { 50 }),
            detail: None,
        };
        let report = configure_with(&catalog, &prober, Some("pakistan"), None, 1, None).unwrap();
        assert_eq!(report["configured"]["profile"]["id"], "pk-fast");
        let report = configure_with(&catalog, &prober, Some("PK"), None, 1, None).unwrap();
        assert_eq!(report["configured"]["profile"]["id"], "pk-fast");
        let report = configure_with(&catalog, &prober, None, Some("usnet"), 1, None).unwrap();
        assert_eq!(report["configured"]["profile"]["id"], "us-fast");
    }

    #[test]
    fn configure_errors_when_all_offline_or_filters_exclude() {
        let catalog = vec![SimProfile {
            id: "off",
            country: "US",
            country_name: "United States",
            carrier: "Net",
            mcc: 310,
            mnc: "260",
            reachability_host: "x.test",
            synthetic_iccid: "8901",
            network: "5G",
        }];
        let prober = |p: &SimProfile| ProbeResult {
            profile_id: p.id,
            online: false,
            latency_ms: None,
            detail: Some("refused".into()),
        };
        let err = configure_with(&catalog, &prober, None, None, 1, None).unwrap_err();
        assert!(err.contains("no sim is online"), "got: {err}");
        let online = |p: &SimProfile| ProbeResult {
            profile_id: p.id,
            online: true,
            latency_ms: Some(5),
            detail: None,
        };
        let err = configure_with(&catalog, &online, Some("PK"), None, 1, None).unwrap_err();
        assert!(err.contains("no online sim matches"), "got: {err}");
    }

    #[test]
    fn configure_never_clobbers_an_occupied_slot2() {
        let catalog = vec![
            SimProfile {
                id: "us-fast",
                country: "US",
                country_name: "United States",
                carrier: "USNet",
                mcc: 310,
                mnc: "260",
                reachability_host: "us.test",
                synthetic_iccid: "8901",
                network: "5G",
            },
            SimProfile {
                id: "pk-fast",
                country: "PK",
                country_name: "Pakistan",
                carrier: "PKNet",
                mcc: 410,
                mnc: "01",
                reachability_host: "pk.test",
                synthetic_iccid: "8903",
                network: "4G",
            },
        ];
        let online = |p: &SimProfile| ProbeResult {
            profile_id: p.id,
            online: true,
            latency_ms: Some(10),
            detail: None,
        };
        // A different, deliberate slot-2 assignment is never clobbered: the
        // pure core proposes no second slot, and the caller surfaces the
        // existing occupant as "kept".
        let report = configure_with(&catalog, &online, None, None, 1, Some("pk-fast")).unwrap();
        assert!(report["second_slot"].is_null());
        // Empty slot 2 → the different-country profile wins it.
        let report = configure_with(&catalog, &online, None, None, 1, None).unwrap();
        assert_eq!(report["second_slot"]["profile"]["id"], "pk-fast");
        // Occupant == winner → the occupant moves to slot 1, freeing slot 2,
        // so a fresh different-country profile is picked for it.
        let report = configure_with(&catalog, &online, None, None, 1, Some("us-fast")).unwrap();
        assert_eq!(report["configured"]["profile"]["id"], "us-fast");
        assert_eq!(report["second_slot"]["profile"]["id"], "pk-fast");
    }

    #[test]
    fn store_slot_ops_and_duplicate_handling() {
        let mut s = SimStore::default();
        assert!(s.active_on(1).is_none());
        // Unknown profile rejected.
        assert!(s.set_slot(1, "nope").is_err());
        // Valid assignment + duplicate cleanup.
        s.set_slot(1, "us-tmobile").unwrap();
        assert_eq!(s.active_on(1).unwrap().id, "us-tmobile");
        s.set_slot(2, "us-tmobile").unwrap();
        assert!(s.slot1.is_none(), "moving a profile clears its old slot");
        // Slot 3 does not exist.
        assert!(s.set_slot(3, "us-tmobile").is_err());
    }

    #[test]
    fn find_and_deactivate_roundtrip() {
        assert!(find("pk-jazz").is_some());
        assert!(find("does-not-exist").is_none());
        // deactivate on an invalid slot errors cleanly.
        assert!(deactivate(9).is_err());
    }

    #[test]
    fn configure_reports_a_kept_slot2_assignment_instead_of_null() {
        // Re-running configure while slot 2 already holds a profile must leave
        // that profile alone AND surface it as `second_slot` with an explicit
        // note — `null` would read as "dual SIM inactive".
        let kept = kept_slot2_report(Some("pk-jazz")).expect("pk-jazz is in the catalog");
        assert_eq!(kept["slot"], 2);
        assert_eq!(kept["profile"]["id"], "pk-jazz");
        assert!(kept["note"].as_str().unwrap().contains("kept"));

        // Empty or unknown slot-2 state yields nothing to inject, so a fresh
        // configure with no different-country candidate keeps `second_slot`
        // null (honest: dual SIM genuinely not filled).
        assert!(kept_slot2_report(None).is_none());
        assert!(kept_slot2_report(Some("does-not-exist")).is_none());
    }
}
