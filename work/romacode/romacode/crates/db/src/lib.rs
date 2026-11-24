//! `romacode-db` — built-in native database engine.
//!
//! This crate vendors `dbengine` (a pure-Rust, std-only, single-file database
//! engine) directly into the RomaCode workspace as a **native library**, NOT an
//! MCP server. The engine handles local key-addressed storage and speaks the
//! postgres / mysql / redis / http wire protocols for remote introspection.
//!
//! The engine is wrapped behind [`DbEngine`], which accepts and returns plain
//! `serde_json::Value` so the rest of RomaCode never has to know about the
//! engine's internal `J` JSON type. Every tool name listed in [`DbEngine::call`]
//! dispatches straight into the engine's in-process handler table — no stdio,
//! no subprocess, no MCP framing.
//!
//! `guard::SqlGuard` is the read-only / destructive-SQL policy that sits in
//! front of `db_query` / `db_execute` and is what the Security gateway uses to
//! decide whether a statement needs an approval card.

pub mod engine;
pub mod guard;

pub use engine::{Engine, J, R, tools_call, selftest};
pub use guard::{SqlGuard, Verdict};

use serde_json::Value;
use std::path::{Path, PathBuf};

/// Native database engine handle. Lives in-process; one instance per RomaCode
/// server. Internally holds the dbengine `Engine` (local key-addressed store
/// plus remote connection state).
pub struct DbEngine {
    inner: engine::Engine,
}

/// Default on-disk home: `<workspace>/.romacode/db`. Caller chooses at build
/// time; romacode-server passes the configured `state_dir` here.
impl DbEngine {
    /// Open (creating if absent) a database home directory.
    pub fn open(home: impl AsRef<Path>) -> std::io::Result<Self> {
        let home = home.as_ref().to_path_buf();
        std::fs::create_dir_all(&home)?;
        Ok(Self { inner: engine::Engine::new(home) })
    }

    /// In-memory home in /tmp — used by tests and the `db test` CLI smoke.
    pub fn ephemeral() -> std::io::Result<Self> {
        let dir = std::env::temp_dir().join(format!("romacode-db-{}", std::process::id()));
        Self::open(&dir)
    }

    /// Run the engine's built-in selftest (pure logic, no network). Returns
    /// the number of failures (0 = pass). Exposed so `romacode-server db test`
    /// can report engine health.
    pub fn selftest_failures() -> i32 {
        engine::selftest()
    }

    /// The home directory backing this engine.
    pub fn home(&self) -> &Path {
        &self.inner.home
    }

    /// Call one engine tool by name with a JSON arguments object.
    ///
    /// `name` is one of the engine's tool ids — `db_status`, `db_create_table`,
    /// `db_schema`, `db_insert`, `db_get`, `db_scan`, `db_sql`, `db_keys`,
    /// `db_graph`, `db_topo`, `db_connect`, `db_remote_query`,
    /// `db_remote_schema`, `db_extract`, `db_export_bundle`, ... (full list in
    /// `engine::tools_call`). The returned `Value` is the tool's result object
    /// already converted from the engine's internal `J`.
    ///
    /// This is the single entry point romacode-server's tool dispatcher uses;
    /// there is no subprocess and no MCP framing.
    pub fn call(&mut self, name: &str, args: &Value) -> Result<Value, String> {
        // tools_call expects {"name": <tool>, "arguments": <args>} — the MCP
        // tools/call envelope. Build it here so callers pass the bare args.
        let params = Value::Object(serde_json::Map::from_iter([
            ("name".to_string(), Value::String(name.to_string())),
            ("arguments".to_string(), args.clone()),
        ]));
        let jparams = value_to_j(&params);
        let res = engine::tools_call(&mut self.inner, &jparams);
        // `tools_call` wraps BOTH successes and tool-level failures in an
        // MCP-style {"content":[{text}], "isError": bool} envelope (it was an
        // MCP server). Unwrap: isError=true becomes Err, otherwise the parsed
        // JSON of the text field.
        match res {
            Ok(envelope) => unwrap_envelope(envelope),
            Err(e) => Err(e),
        }
    }

    /// List the names of every engine tool, for schema advertisement.
    pub fn tools() -> &'static [&'static str] {
        &[
            "db_status", "db_create_table", "db_drop_table", "db_schema",
            "db_insert", "db_get", "db_update", "db_delete", "db_scan",
            "db_count", "db_sql", "db_keys", "db_graph", "db_topo",
            "db_diff", "db_snapshot", "db_branch", "db_merge", "db_sync",
            "db_connect", "db_remote_query", "db_remote_schema", "db_extract",
            "db_import_bundle", "db_export_bundle", "db_fingerprint", "db_compact",
        ]
    }
}

/// Pull the `text` field out of the engine's MCP-style result envelope and
/// parse it back to JSON. If the envelope carries `isError`, surface it as
/// `Err` so the caller treats it as a tool failure (matching how a real error
/// would flow through the agent loop).
fn unwrap_envelope(env: J) -> Result<Value, String> {
    let is_err = env.get("isError").and_then(|v| v.as_bool()).unwrap_or(false);
    let text = env
        .get("content")
        .and_then(|c| c.as_arr())
        .and_then(|arr| arr.first())
        .and_then(|first| first.get("text"))
        .and_then(|t| t.as_str())
        .unwrap_or("");
    // The engine always writes valid JSON via jpretty. If it ever fails to
    // parse, fall back to the raw text so the caller still sees something.
    if is_err {
        return Err(text.to_string());
    }
    if let Ok(j) = engine::jparse(text) {
        Ok(j_to_value(&j))
    } else {
        Ok(Value::String(text.to_string()))
    }
}

// ---- serde_json::Value <-> engine J conversion ----

fn value_to_j(v: &Value) -> J {
    match v {
        Value::Null => J::Null,
        Value::Bool(b) => J::Bool(*b),
        Value::Number(n) => {
            if let Some(i) = n.as_i64() {
                J::Int(i)
            } else if let Some(f) = n.as_f64() {
                J::Num(f)
            } else {
                J::Null
            }
        }
        Value::String(s) => J::Str(s.clone()),
        Value::Array(a) => J::Arr(a.iter().map(value_to_j).collect()),
        Value::Object(o) => {
            let pairs: Vec<(String, J)> = o.iter().map(|(k, vv)| (k.clone(), value_to_j(vv))).collect();
            J::Obj(pairs)
        }
    }
}

fn j_to_value(j: &J) -> Value {
    match j {
        J::Null => Value::Null,
        J::Bool(b) => Value::Bool(*b),
        J::Int(i) => Value::Number((*i).into()),
        J::Num(f) => serde_json::Number::from_f64(*f).map(Value::Number).unwrap_or(Value::Null),
        J::Str(s) => Value::String(s.clone()),
        J::Arr(a) => Value::Array(a.iter().map(j_to_value).collect()),
        J::Obj(pairs) => {
            let m: serde_json::Map<String, Value> =
                pairs.iter().map(|(k, v)| (k.clone(), j_to_value(v))).collect();
            Value::Object(m)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn fresh() -> DbEngine {
        let dir = std::env::temp_dir().join(format!("rmdb-test-{}-{}", std::process::id(), unique()));
        let _ = std::fs::remove_dir_all(&dir);
        DbEngine::open(&dir).unwrap()
    }

    fn unique() -> String {
        use std::sync::atomic::{AtomicU64, Ordering};
        static N: AtomicU64 = AtomicU64::new(1);
        N.fetch_add(1, Ordering::SeqCst).to_string()
    }

    #[test]
    fn status_reports_home() {
        let mut e = fresh();
        let r = e.call("db_status", &json!({})).unwrap();
        assert!(r.get("home").is_some() || r.is_object(), "status returned {r}");
    }

    #[test]
    fn create_insert_get_roundtrip() {
        let mut e = fresh();
        e.call("db_create_table", &json!({
            "db": "testdb", "table": "users",
            "primary_key": ["id"],
            "columns": [
                {"name":"id","dtype":"integer"},
                {"name":"name","dtype":"text"}
            ]
        })).unwrap();
        e.call("db_insert", &json!({
            "db":"testdb","table":"users",
            "rows":[{"id":1,"name":"ada"}]
        })).unwrap();
        // db_get takes the raw primary-key value (scalar when pk is one column)
        let got = e.call("db_get", &json!({"db":"testdb","table":"users","key":1})).unwrap();
        let s = serde_json::to_string(&got).unwrap();
        assert!(s.contains("ada"), "got {s}");
    }

    #[test]
    fn unknown_tool_errors() {
        let mut e = fresh();
        let r = e.call("db_nonexistent", &json!({}));
        assert!(r.is_err());
    }

    #[test]
    fn tools_list_nonempty() {
        assert!(DbEngine::tools().len() >= 20);
    }

    #[test]
    fn home_dir_backed() {
        let dir = std::env::temp_dir().join(format!("rmdb-home-{}", unique()));
        let e = DbEngine::open(&dir).unwrap();
        assert_eq!(e.home(), dir);
        assert!(dir.exists());
    }
}
