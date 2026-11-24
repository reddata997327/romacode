//! The `dsd.*` tool surface: Domain → Server → Database live API tracing.
//!
//!   dsd.proxy_start — start a reverse-proxy tap in front of the web server;
//!                     every request through it logs ENTRY (method, path,
//!                     host) and EXIT (status, latency, bytes).
//!   dsd.db_attach   — attach a live database monitor (postgres /
//!                     mysql processlist polling, sqlite file watching).
//!   dsd.watch       — render the correlated live timeline for a window:
//!                     each API call with the DB queries it triggered.
//!   dsd.stop        — stop all observers and report the session summary.
//!
//! Together romacode can watch API calls travel domain → web server →
//! application → database and back, over the network and on local server
//! databases, in one timeline.

use rc_core::schema::{ParamType, Parameter, Schema};
use rc_core::tool::{ToolContext, ToolSourceKind};
use rc_core::{arc, Tool, ToolResult};
use async_trait::async_trait;
use serde_json::{json, Value};

use crate::dbmon;
use crate::render;
use crate::session::{self, DsdSession};
use crate::tap;

fn need_str<'a>(input: &'a Value, key: &str) -> ToolResult<&'a str> {
    input[key]
        .as_str()
        .map(|s| s.trim())
        .filter(|s| !s.is_empty())
        .ok_or_else(|| rc_core::error::ToolError::ToolFailed {
            tool: "dsd".into(),
            inner: format!("missing required string parameter \"{key}\""),
        })
}

fn failed(tool: &str, msg: impl Into<String>) -> rc_core::error::ToolError {
    rc_core::error::ToolError::ToolFailed { tool: tool.into(), inner: msg.into() }
}

/// "host:port", "http://host:port" or "https://host:port" → (host, port, tls).
fn parse_upstream(s: &str) -> Result<(String, u16, bool), String> {
    let s = s.trim();
    let (use_tls, rest) = match s.split_once("://") {
        Some(("https", r)) => (true, r),
        Some(("http", r)) => (false, r),
        Some((_, _)) => return Err(format!("upstream scheme must be http:// or https:// — got \"{s}\"")),
        None => (false, s),
    };
    let (h, p) = rest
        .split_once(':')
        .ok_or_else(|| format!("upstream needs host:port — got \"{s}\""))?;
    let port: u16 = p.trim_end_matches('/').parse().map_err(|_| format!("bad port in \"{s}\""))?;
    Ok((h.to_string(), port, use_tls))
}

// ────────────────────────── dsd.proxy_start ──────────────────────────

pub struct ProxyStart;

#[async_trait]
impl Tool for ProxyStart {
    fn name(&self) -> &str { "dsd.proxy_start" }
    fn description(&self) -> &str {
        "Start a reverse-proxy tap: binds a local port and forwards to the web server, \
         logging every API call's ENTRY (method, path, host) and EXIT (status, latency, bytes) \
         live. Point the domain/site at this port (or call it directly) to trace traffic."
    }
    fn schema(&self) -> Schema {
        Schema::new()
            .param(Parameter::required("listen_port", ParamType::Integer).with_desc("local port the tap binds (the entry point)"))
            .param(Parameter::required("upstream", ParamType::String).with_desc("web server behind it, \"host:port\" or \"http://host:port\""))
    }
    fn source(&self) -> ToolSourceKind { ToolSourceKind::Builtin }
    async fn execute(&self, input: &Value, ctx: &ToolContext) -> ToolResult<Value> {
        ctx.check_net()?;
        // port 0 binds an ephemeral port (useful for probing); the real
        // bound port is reported in the reply
        let port = input["listen_port"].as_u64().filter(|p| *p < 65536).ok_or_else(|| {
            failed(self.name(), "listen_port must be an integer 0..65535 (0 = ephemeral)")
        })? as u16;
        let upstream = need_str(input, "upstream").map_err(|e| failed(self.name(), e.to_string()))?;
        let (host, uport, use_tls) = parse_upstream(upstream).map_err(|e| failed(self.name(), e))?;

        {
            let s = session::session().lock().unwrap();
            if s.taps.iter().any(|t| t.tap_port == port) {
                return Err(failed(self.name(), format!("a tap already runs on :{port} — dsd.stop first or pick another port")));
            }
        }
        let handle = tap::start(port, &host, uport, use_tls)
            .await
            .map_err(|e| failed(self.name(), e))?;
        let upstream_s = handle.upstream.clone();
        let bound = handle.tap_port;
        {
            let mut s = session::session().lock().unwrap();
            s.observer_labels.push(format!("tap :{bound} → {upstream_s}"));
            s.taps.push(handle);
        }
        Ok(json!(format!(
            "dsd tap live: :{bound} → {upstream_s}\n\
             every request through :{bound} now logs entry/exit in the DSD timeline.\n\
             point the domain/site at :{bound} (or curl http://127.0.0.1:{bound}/…) then run dsd.watch.\n\
             next: dsd.db_attach to watch the database, dsd.watch to see the live trace."
        )))
    }
}

// ────────────────────────── dsd.db_attach ──────────────────────────

pub struct DbAttach;

#[async_trait]
impl Tool for DbAttach {
    fn name(&self) -> &str { "dsd.db_attach" }
    fn description(&self) -> &str {
        "Attach a LIVE database monitor to the DSD trace. backend=\"postgres\" or \"mysql\" \
         polls what is actually running (pg_stat_activity / processlist: query, user, db, \
         ms running) via the built-in wire protocol — works for network and local server \
         databases. backend=\"sqlite\" watches the db file (+wal/journal) for changes; \
         sqlite has no cross-process activity table, so SQL text is not visible."
    }
    fn schema(&self) -> Schema {
        Schema::new()
            .param(Parameter::required("backend", ParamType::String).with_desc("postgres | mysql | sqlite"))
            .param(Parameter::optional("dsn", ParamType::String).with_desc("postgres://user:pass@host:port/db (or mysql://…); omit and pass host/user/... fields"))
            .param(Parameter::optional("host", ParamType::String))
            .param(Parameter::optional("port", ParamType::Integer))
            .param(Parameter::optional("user", ParamType::String))
            .param(Parameter::optional("password", ParamType::String))
            .param(Parameter::optional("database", ParamType::String))
            .param(Parameter::optional("path", ParamType::String).with_desc("sqlite: path to the .db/.sqlite file"))
            .param(Parameter::optional("poll_ms", ParamType::Integer).with_desc("poll interval, default 250 (clamped 100..5000)"))
    }
    fn source(&self) -> ToolSourceKind { ToolSourceKind::Builtin }
    async fn execute(&self, input: &Value, ctx: &ToolContext) -> ToolResult<Value> {
        ctx.check_net()?;
        let backend = need_str(input, "backend").map_err(|e| failed(self.name(), e.to_string()))?.to_ascii_lowercase();
        let poll_ms = input["poll_ms"].as_u64().unwrap_or(250);
        let handle = match backend.as_str() {
            "sqlite" => {
                let path = need_str(input, "path").map_err(|e| failed(self.name(), e.to_string()))?;
                dbmon::attach_sqlite(path, poll_ms).map_err(|e| failed(self.name(), e))?
            }
            "postgres" | "postgresql" | "pg" | "mysql" => {
                let norm = if backend.starts_with("postgres") { "postgres" } else { "mysql" };
                let dsn = input["dsn"].as_str().unwrap_or("");
                let conn = dbmon::conn_object(norm, dsn, input).map_err(|e| failed(self.name(), e))?;
                let label = format!(
                    "{}:{}/{}",
                    conn["host"].as_str().unwrap_or("?"),
                    conn["port"],
                    conn["database"].as_str().unwrap_or("?")
                );
                dbmon::attach_sql(norm, conn, poll_ms, &label).map_err(|e| failed(self.name(), e))?
            }
            other => {
                return Err(failed(self.name(), format!("unknown backend \"{other}\" — want postgres | mysql | sqlite")));
            }
        };
        let line = format!("{}@{}", handle.backend, handle.target);
        {
            let mut s = session::session().lock().unwrap();
            s.observer_labels.push(line.clone());
            s.dbmons.push(handle);
        }
        Ok(json!(format!(
            "dsd db monitor live: {line}\n\
             queries observed while an api call is in flight are correlated to that call automatically.\n\
             next: dsd.watch for the live Domain→Server→Database timeline."
        )))
    }
}

// ────────────────────────── dsd.watch ──────────────────────────

pub struct Watch;

#[async_trait]
impl Tool for Watch {
    fn name(&self) -> &str { "dsd.watch" }
    fn description(&self) -> &str {
        "Render the LIVE DSD timeline for a window: api calls entry→exit (status, latency, \
         bytes) with the database queries observed while each call was in flight, plus \
         background queries and file-change notes. Network + local server database views \
         together, correlated. Call repeatedly for fresh windows."
    }
    fn schema(&self) -> Schema {
        Schema::new()
            .param(Parameter::optional("duration_ms", ParamType::Integer).with_desc("how long to watch, default 5000 (max 60000)"))
            .param(Parameter::optional("scope", ParamType::String).with_desc("\"new\" = only events after this call started; \"all\" = whole buffer (default)"))
    }
    fn source(&self) -> ToolSourceKind { ToolSourceKind::Builtin }
    async fn execute(&self, input: &Value, _ctx: &ToolContext) -> ToolResult<Value> {
        let dur_ms = input["duration_ms"].as_u64().unwrap_or(5_000).min(60_000);
        let scope_new = input["scope"].as_str().map(|s| s.eq_ignore_ascii_case("new")).unwrap_or(false);
        let started = std::time::SystemTime::now();
        // async sleep so the agent runtime is not blocked
        tokio::time::sleep(std::time::Duration::from_millis(dur_ms)).await;
        // attachment check happens on the REAL session — the snapshot drops
        // the live handles by design
        let attached = {
            let s = session::session().lock().unwrap();
            !s.taps.is_empty() || !s.dbmons.is_empty()
        };
        if !attached {
            return Ok(json!(format!(
                "dsd: nothing is attached yet.\n\
                 start observers first: dsd.proxy_start {{listen_port, upstream}} in front of the web \
                 server, dsd.db_attach {{backend, dsn|path}} for the database — then dsd.watch."
            )));
        }
        let s = session::session().lock().unwrap().clone_session();
        let out = if scope_new {
            render::window(&s, started)
        } else {
            render::timeline(&s)
        };
        Ok(json!(out))
    }
}

// ────────────────────────── dsd.stop ──────────────────────────

pub struct Stop;

#[async_trait]
impl Tool for Stop {
    fn name(&self) -> &str { "dsd.stop" }
    fn description(&self) -> &str {
        "Stop all dsd observers (taps + database monitors), clear the timeline and return \
         the session summary. The tap port is released."
    }
    fn schema(&self) -> Schema {
        Schema::new()
    }
    fn source(&self) -> ToolSourceKind { ToolSourceKind::Builtin }
    async fn execute(&self, _input: &Value, _ctx: &ToolContext) -> ToolResult<Value> {
        Ok(json!(session::stop_all()))
    }
}

// DsdSession needs a clone for the async watch handoff.
impl DsdSession {
    /// Snapshot without the live handles (events + descriptor lists).
    pub fn clone_session(&self) -> DsdSession {
        DsdSession {
            taps: Vec::new(),
            dbmons: Vec::new(),
            observer_labels: self.observer_labels.clone(),
            events: self.events.clone(),
            next_call_id: std::sync::atomic::AtomicU64::new(self.next_call_id.load(std::sync::atomic::Ordering::Relaxed)),
            cap: self.cap,
        }
    }
}

pub fn all() -> Vec<rc_core::tool::ToolArc> {
    vec![arc(ProxyStart), arc(DbAttach), arc(Watch), arc(Stop)]
}
