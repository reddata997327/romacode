//! `web.*` tools — HTTP fetch + URL encoding. `fetch` consults
//! `ctx.check_net()` so the sandbox's net policy is enforced at the tool
//! level. URL encode/decode are pure functions.

use rc_core::schema::{ParamType, Parameter, Schema};
use rc_core::tool::{ToolContext, ToolSourceKind, ToolArc};
use rc_core::{arc, Tool, ToolResult};
use async_trait::async_trait;
use serde_json::Value;

use crate::need_param;

pub struct Fetch;
#[async_trait]
impl Tool for Fetch {
    fn name(&self) -> &str { "web.fetch" }
    fn description(&self) -> &str {
        "HTTP GET a URL, return the response body as a string. Enforces the sandbox net policy."
    }
    fn schema(&self) -> Schema {
        Schema::new()
            .param(Parameter::required("url", ParamType::String))
            .param(Parameter::optional("max_bytes", ParamType::Integer).with_default(serde_json::json!(200_000)))
    }
    fn source(&self) -> ToolSourceKind { ToolSourceKind::Builtin }
    async fn execute(&self, input: &Value, ctx: &ToolContext) -> ToolResult<Value> {
        ctx.check_net()?;
        let url = need_param(input, "url", ParamType::String, Value::as_str)?;
        let max_bytes = input["max_bytes"].as_i64().unwrap_or(200_000).max(0) as usize;
        let mk_err = |e: String| rc_core::error::ToolError::ToolFailed {
            tool: "web.fetch".into(),
            inner: e,
        };
        let client = reqwest::Client::builder()
            .timeout(std::time::Duration::from_secs(15))
            .build()
            .map_err(|e| mk_err(e.to_string()))?;
        let resp = client
            .get(url)
            .send()
            .await
            .map_err(|e| mk_err(e.to_string()))?;
        let status = resp.status().as_u16();
        // Stream the body with a running byte counter — a huge response is
        // truncated at `max_bytes` while reading, never read fully into RAM
        // before the cap is applied.
        let mut body_bytes: Vec<u8> = Vec::with_capacity(8_192);
        let mut truncated = false;
        let mut resp = resp;
        while let Some(chunk) = resp.chunk().await.map_err(|e| mk_err(e.to_string()))? {
            if body_bytes.len() + chunk.len() > max_bytes {
                let take = max_bytes.saturating_sub(body_bytes.len());
                body_bytes.extend_from_slice(&chunk[..take]);
                truncated = true;
                break;
            }
            body_bytes.extend_from_slice(&chunk);
        }
        let body = String::from_utf8_lossy(&body_bytes).to_string();
        Ok(serde_json::json!({
            "status": status,
            "body": body,
            "truncated": truncated,
        }))
    }
}

pub struct UrlEncode;
#[async_trait]
impl Tool for UrlEncode {
    fn name(&self) -> &str { "web.url_encode" }
    fn description(&self) -> &str { "Percent-encode a string for use in a URL query/component." }
    fn schema(&self) -> Schema {
        Schema::new().param(Parameter::required("s", ParamType::String))
    }
    fn source(&self) -> ToolSourceKind { ToolSourceKind::Builtin }
    async fn execute(&self, input: &Value, _ctx: &ToolContext) -> ToolResult<Value> {
        let s = need_param(input, "s", ParamType::String, Value::as_str)?;
        Ok(serde_json::json!(url_encode(s)))
    }
}

pub struct UrlDecode;
#[async_trait]
impl Tool for UrlDecode {
    fn name(&self) -> &str { "web.url_decode" }
    fn description(&self) -> &str { "Percent-decode a string." }
    fn schema(&self) -> Schema {
        Schema::new().param(Parameter::required("s", ParamType::String))
    }
    fn source(&self) -> ToolSourceKind { ToolSourceKind::Builtin }
    async fn execute(&self, input: &Value, _ctx: &ToolContext) -> ToolResult<Value> {
        let s = need_param(input, "s", ParamType::String, Value::as_str)?;
        let dec = url_decode(s).map_err(rc_core::error::ToolError::Other)?;
        Ok(serde_json::json!(dec))
    }
}

/// Percent-encode per RFC 3986 (unreserved set kept).
fn url_encode(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for &b in s.as_bytes() {
        match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                out.push(b as char);
            }
            _ => {
                out.push('%');
                out.push_str(&format!("{:02X}", b));
            }
        }
    }
    out
}

fn url_decode(s: &str) -> Result<String, String> {
    let bytes = s.as_bytes();
    let mut out: Vec<u8> = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%' {
            if i + 2 >= bytes.len() {
                return Err("incomplete percent-encoding".into());
            }
            let hex = std::str::from_utf8(&bytes[i + 1..i + 3])
                .map_err(|e| e.to_string())?;
            let byte = u8::from_str_radix(hex, 16)
                .map_err(|e| e.to_string())?;
            out.push(byte);
            i += 3;
        } else if bytes[i] == b'+' {
            out.push(b' ');
            i += 1;
        } else {
            out.push(bytes[i]);
            i += 1;
        }
    }
    String::from_utf8(out).map_err(|e| e.to_string())
}

pub fn all() -> Vec<ToolArc> {
    vec![arc(Fetch), arc(UrlEncode), arc(UrlDecode)]
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn url_roundtrip() {
        let s = "hello world & foo=bar";
        let enc = url_encode(s);
        assert_eq!(enc, "hello%20world%20%26%20foo%3Dbar");
        assert_eq!(url_decode(&enc).unwrap(), s);
    }
    #[test]
    fn url_plus_decode() {
        assert_eq!(url_decode("a+b").unwrap(), "a b");
    }
}
