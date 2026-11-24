//! `time.*` tools — clock helpers. `now` and `iso_now` are pure reads;
//! `sleep` is a test/delay helper (capped by the sandbox timeout).

use rc_core::schema::{ParamType, Parameter, Schema};
use rc_core::tool::{ToolContext, ToolSourceKind, ToolArc};
use rc_core::{arc, Tool, ToolResult};
use async_trait::async_trait;
use serde_json::Value;
use std::time::{SystemTime, UNIX_EPOCH};

pub struct Now;
#[async_trait]
impl Tool for Now {
    fn name(&self) -> &str { "time.now" }
    fn description(&self) -> &str { "Current Unix epoch time in seconds (float, sub-ms precision)." }
    fn schema(&self) -> Schema { Schema::new() }
    fn source(&self) -> ToolSourceKind { ToolSourceKind::Builtin }
    async fn execute(&self, _input: &Value, _ctx: &ToolContext) -> ToolResult<Value> {
        let d = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map_err(|e| rc_core::error::ToolError::Other(e.to_string()))?;
        Ok(serde_json::json!(d.as_secs_f64()))
    }
}

pub struct IsoNow;
#[async_trait]
impl Tool for IsoNow {
    fn name(&self) -> &str { "time.iso_now" }
    fn description(&self) -> &str { "Current UTC time as an ISO-8601 string (seconds precision)." }
    fn schema(&self) -> Schema { Schema::new() }
    fn source(&self) -> ToolSourceKind { ToolSourceKind::Builtin }
    async fn execute(&self, _input: &Value, _ctx: &ToolContext) -> ToolResult<Value> {
        Ok(Value::String(iso_now()))
    }
}

pub struct Sleep;
#[async_trait]
impl Tool for Sleep {
    fn name(&self) -> &str { "time.sleep" }
    fn description(&self) -> &str { "Sleep for `ms` milliseconds (capped by the sandbox timeout). Returns the elapsed ms." }
    fn schema(&self) -> Schema {
        Schema::new().param(Parameter::required("ms", ParamType::Integer))
    }
    fn source(&self) -> ToolSourceKind { ToolSourceKind::Builtin }
    async fn execute(&self, input: &Value, ctx: &ToolContext) -> ToolResult<Value> {
        let ms = input["ms"].as_i64().unwrap_or(0).max(0) as u64;
        let cap = if ctx.timeout_ms == 0 { ms } else { ms.min(ctx.timeout_ms) };
        let start = std::time::Instant::now();
        tokio::time::sleep(std::time::Duration::from_millis(cap)).await;
        Ok(serde_json::json!(start.elapsed().as_millis() as u64))
    }
}

/// ISO-8601 UTC from the system clock, without pulling a datetime crate.
fn iso_now() -> String {
    let d = SystemTime::now().duration_since(UNIX_EPOCH).unwrap_or_default();
    let secs = d.as_secs();
    let days = secs / 86_400;
    let (y, m, day) = civil_from_days(days as i64);
    let s = secs % 86_400;
    let hh = s / 3600;
    let mm = (s % 3600) / 60;
    let ss = s % 60;
    format!("{y:04}-{m:02}-{day:02}T{hh:02}:{mm:02}:{ss:02}Z")
}

/// Howard Hinnant's algorithm: days since 1970-01-01 → (year, month, day).
fn civil_from_days(z: i64) -> (i64, u32, u32) {
    let z = z + 719_468;
    let era = if z >= 0 { z } else { z - 146_096 } / 146_097;
    let doe = (z - era * 146_097) as u64;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let y = yoe as i64 + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = if m <= 2 { y + 1 } else { y };
    (y, m as u32, d as u32)
}

pub fn all() -> Vec<ToolArc> {
    vec![arc(Now), arc(IsoNow), arc(Sleep)]
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn iso_format() {
        let s = iso_now();
        assert!(s.ends_with('Z'));
        assert_eq!(s.len(), 20); // YYYY-MM-DDTHH:MM:SSZ
    }
    #[test]
    fn civil_known() {
        // 1970-01-01 is day 0.
        assert_eq!(civil_from_days(0), (1970, 1, 1));
        // 2024-01-01 is day 19_723.
        assert_eq!(civil_from_days(19_723), (2024, 1, 1));
    }
}
