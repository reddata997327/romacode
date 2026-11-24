//! Wire types for offense events/requests. All serde-default for compat.

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct LootQuery { pub selector: String, pub limit: Option<u32> }

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct LootSummary { pub counts: Vec<(String, i64)> }

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct ChainState { pub id: i64, pub node: String, pub state: String }

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct Evidence { pub artifact_id: i64, pub sha256: String }

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct DryRun { pub tool: String, pub skipped: String }
