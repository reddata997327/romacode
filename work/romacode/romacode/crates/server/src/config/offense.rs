//! Offense config section.

use serde::{Deserialize, Serialize};
use super::security::Effect;

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct OffenseConfig {
    pub dry_run: bool,
    pub autoaccept: bool,
    pub max_chain_risk: super::security::Risk,
    pub scope: Vec<String>,
    pub deny_effects: Vec<Effect>,
    pub loot_dir: String,
    pub seal_secrets: bool,
    pub evidence: EvidenceConfig,
    pub budget: BudgetConfig,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct EvidenceConfig {
    pub hash_artifacts: bool,
    pub screenshot_on_change: bool,
    pub keep_raw_http: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct BudgetConfig {
    pub max_concurrent_scans: u32,
    pub max_requests_per_sec: u32,
    pub max_hosts_per_engagement: u32,
    pub max_spray_attempts_per_account: u32,
    pub max_card_checks_per_min: u32,
}

impl Default for OffenseConfig {
    fn default() -> Self {
        Self {
            dry_run: true,
            autoaccept: false,
            max_chain_risk: super::security::Risk::High,
            scope: Vec::new(),
            deny_effects: Vec::new(),
            loot_dir: String::new(),
            seal_secrets: true,
            evidence: EvidenceConfig::default(),
            budget: BudgetConfig::default(),
        }
    }
}

impl Default for EvidenceConfig {
    fn default() -> Self {
        Self { hash_artifacts: true, screenshot_on_change: true, keep_raw_http: true }
    }
}

impl Default for BudgetConfig {
    fn default() -> Self {
        Self {
            max_concurrent_scans: 64,
            max_requests_per_sec: 20,
            max_hosts_per_engagement: 4096,
            max_spray_attempts_per_account: 3,
            max_card_checks_per_min: 30,
        }
    }
}
