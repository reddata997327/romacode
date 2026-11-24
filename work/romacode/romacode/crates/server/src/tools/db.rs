//! Database tools — six agent-facing tools backed by the built-in native
//! `romacode-db` engine. Every tool here is dispatched through
//! `tools::execute`; the SQL safety gate (`romacode_db::SqlGuard`) sits in
//! front of `db_query` / `db_execute` and decides whether a statement runs,
//! needs an approval card, or is refused.
//!
//! The agent sees six tools:
//!   * `db_list`     — list local databases (and any saved remote profiles).
//!   * `db_use`      — select the active database the next query targets.
//!   * `db_schema`   — introspect a table's key structure (PK/FK/unique/index).
//!   * `db_query`    — run a Read-Only SQL statement (SELECT/WITH/EXPLAIN).
//!   * `db_execute`  — run a Write or Destructive statement; flagged by the
//!                     guard, refused under Read-Only, requires an approval
//!                     card under Read-Write.
//!   * `db_explain`  — EXPLAIN a statement (always read-only).
//!
//! Underneath, the engine exposes the full dbengine tool surface (db_insert,
//! db_get, db_scan, db_keys, db_graph, db_topo, db_remote_query, db_extract,
//! ...) — those are reachable via `db_execute` passing a JSON-RPC-style
//! command, OR through future thin wrapper tools. The six here are the ones
//! the user spec asked for.

use crate::config::Config;
use romacode_db::{DbEngine, guard::{AccessTier, SqlGuard, Verdict}};
use serde_json::{json, Value};
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Mutex, OnceLock};

/// Shared, lazily-initialised engine. One per process. Locking is per-tool
/// call; the engine itself is single-threaded internally (the vendored
/// dbengine was designed for an MCP server's serial request loop).
static ENGINE: OnceLock<Mutex<DbEngine>> = OnceLock::new();

/// Active database (last `db_use`). Empty string = engine default.
static ACTIVE_DB: Mutex<String> = Mutex::new(String::new());

/// Initialise the engine against a configured home directory. Called once at
/// server boot from `main.rs` (after config load). Safe to call more than
/// once — the first call wins.
pub fn init(home: PathBuf) -> Result<(), String> {
    if ENGINE.get().is_some() {
        return Ok(());
    }
    let e = DbEngine::open(&home).map_err(|e| format!("db home {home:?}: {e}"))?;
    let _ = ENGINE.set(Mutex::new(e));
    Ok(())
}

/// The configured-or-default home. Lazily opens an ephemeral home if the
/// server never called `init` (keeps the tools usable in tests).
fn with_engine<F, R>(f: F) -> R
where
    F: FnOnce(&mut DbEngine) -> R,
{
    let m = ENGINE.get_or_init(|| Mutex::new(DbEngine::ephemeral().expect("db ephemeral home")));
    let mut guard = m.lock().unwrap_or_else(|e| e.into_inner());
    f(&mut guard)
}

/// Read the currently-selected access tier from config. Defaults to
/// Read-Only when the databases section is absent.
fn tier_from_cfg(cfg: &Config) -> AccessTier {
    match cfg.databases.tier.as_str() {
        "full" => AccessTier::Full,
        "read-write" | "readwrite" | "rw" => AccessTier::ReadWrite,
        _ => AccessTier::ReadOnly,
    }
}

/// Pull the active db name, falling back to the request's `db` arg or the
/// config default. The engine requires a database on most calls.
fn active_db(cfg: &Config, input: &Value) -> Option<String> {
    if let Some(s) = input.get("db").and_then(|v| v.as_str()) {
        if !s.is_empty() { return Some(s.to_string()); }
    }
    let a = ACTIVE_DB.lock().unwrap_or_else(|e| e.into_inner());
    if !a.is_empty() { return Some(a.clone()); }
    if !cfg.databases.default.is_empty() { return Some(cfg.databases.default.clone()); }
    None
}

// ---- tool definitions (advertised to the model) ----

pub fn definitions() -> Vec<Value> {
    let t = |name: &str, desc: &str, props: Value, req: &[&str]| {
        json!({
            "name": name,
            "description": desc,
            "input_schema": { "type": "object", "properties": props, "required": req },
        })
    };
    vec![
        t("db_list",
          "List all local databases in the RomaCode engine, plus any saved remote connection profiles. Use this FIRST to see what is available before db_use.",
          json!({}),
          &[]),
        t("db_use",
          "Select the active database that subsequent db_query / db_execute / db_schema calls target. Pass either a local db name or a saved remote profile id (see db_list).",
          json!({
            "db": { "type": "string", "description": "local db name OR saved remote profile id" }
          }),
          &["db"]),
        t("db_schema",
          "Introspect a table's full key structure — primary key, foreign keys, unique keys and indexes. Works on local databases and (when db_use selected a remote profile) remote schemas via pg_catalog / INFORMATION_SCHEMA.",
          json!({
            "db": { "type": "string", "description": "database or profile id; omit to use the active db" },
            "table": { "type": "string", "description": "table to introspect (omit to list all tables and their keys)" }
          }),
          &[]),
        t("db_query",
          "Run a READ-ONLY SQL statement (SELECT, WITH, EXPLAIN, SHOW, DESCRIBE). Anything that mutates is refused — use db_execute for writes. Read-Only tier allows SELECT/WITH/EXPLAIN only.",
          json!({
            "sql": { "type": "string", "description": "the read-only SQL statement" },
            "db": { "type": "string", "description": "database or profile id; omit to use the active db" }
          }),
          &["sql"]),
        t("db_execute",
          "Run a WRITE or DESTRUCTIVE SQL statement (INSERT / UPDATE / DELETE / CREATE / DROP / TRUNCATE / ALTER). Guarded: DELETE/UPDATE without WHERE and DROP/TRUNCATE/ALTER are flagged DESTRUCTIVE (red warning). Under Read-Only tier this is refused. Under Read-Write tier the agent must wait for a Security approval card before the statement runs.",
          json!({
            "sql": { "type": "string", "description": "the write or DDL statement" },
            "db": { "type": "string", "description": "database or profile id; omit to use the active db" }
          }),
          &["sql"]),
        t("db_explain",
          "EXPLAIN a query plan. Always read-only regardless of access tier. Use this to understand how the engine (or a remote DB) will execute a statement before running it.",
          json!({
            "sql": { "type": "string", "description": "the statement to explain" },
            "db": { "type": "string", "description": "database or profile id; omit to use the active db" }
          }),
          &["sql"]),
    ]
}

// ---- dispatch ----

/// Execute a db_* tool by name. `cancel` is honoured for the potentially-slow
/// remote-query paths.
pub fn execute(cfg: &Config, name: &str, input: &Value, cancel: &AtomicBool) -> Result<String, String> {
    if cancel.load(Ordering::SeqCst) {
        return Err("db tool cancelled".into());
    }
    match name {
        "db_list" => list(cfg),
        "db_use" => use_db(input),
        "db_schema" => schema(cfg, input),
        "db_query" => query(cfg, input),
        "db_execute" => execute_sql(cfg, input),
        "db_explain" => explain(cfg, input),
        other => Err(format!("unknown db tool '{other}'")),
    }
}

fn list(cfg: &Config) -> Result<String, String> {
    let res = with_engine(|e| e.call("db_status", &json!({})))?;
    let mut out = serde_json::to_string_pretty(&res).map_err(|e| e.to_string())?;
    // Surface remote profiles from config too, masked.
    if !cfg.databases.profiles.is_empty() {
        out.push_str("\n\nRemote profiles (from config):\n");
        for p in &cfg.databases.profiles {
            out.push_str(&format!("  - {} [{}] {}\n", p.id, p.kind, mask_url(&p.url)));
        }
    }
    Ok(out)
}

fn use_db(input: &Value) -> Result<String, String> {
    let db = input.get("db").and_then(|v| v.as_str())
        .ok_or("db_use requires 'db'")?;
    let mut a = ACTIVE_DB.lock().unwrap_or_else(|e| e.into_inner());
    *a = db.to_string();
    Ok(format!("active database set to: {db}"))
}

fn schema(cfg: &Config, input: &Value) -> Result<String, String> {
    let db = active_db(cfg, input).ok_or("no active db; call db_use first or pass 'db'")?;
    if let Some(table) = input.get("table").and_then(|v| v.as_str()) {
        let r = with_engine(|e| e.call("db_schema", &json!({"db": db, "table": table})))?;
        Ok(serde_json::to_string_pretty(&r).map_err(|e| e.to_string())?)
    } else {
        // List every table's key map.
        let r = with_engine(|e| e.call("db_keys", &json!({"db": db})))?;
        Ok(serde_json::to_string_pretty(&r).map_err(|e| e.to_string())?)
    }
}

fn query(cfg: &Config, input: &Value) -> Result<String, String> {
    let sql = input.get("sql").and_then(|v| v.as_str())
        .ok_or("db_query requires 'sql'")?;
    let tier = tier_from_cfg(cfg);
    let guard = SqlGuard::new(tier);
    let d = guard.classify(sql);
    // db_query is hard-restricted to read-only even in Full mode (the agent
    // must use db_execute for writes). Anything not ReadOnly is refused here.
    if d.verdict != Verdict::ReadOnly {
        return Err(format!(
            "db_query is read-only; this statement is classified '{verdict}'. Use db_execute for write/destructive statements. ({reason})",
            verdict = d.verdict, reason = d.reason
        ));
    }
    if d.refused {
        return Err(d.reason);
    }
    let db = active_db(cfg, input).ok_or("no active db; call db_use first or pass 'db'")?;
    let r = with_engine(|e| e.call("db_sql", &json!({"db": db, "sql": sql})))?;
    Ok(serde_json::to_string_pretty(&r).map_err(|e| e.to_string())?)
}

fn execute_sql(cfg: &Config, input: &Value) -> Result<String, String> {
    let sql = input.get("sql").and_then(|v| v.as_str())
        .ok_or("db_execute requires 'sql'")?;
    let tier = tier_from_cfg(cfg);
    let guard = SqlGuard::new(tier);
    let d = guard.classify(sql);
    if d.refused {
        return Err(format!("refused: {}", d.reason));
    }
    let db = active_db(cfg, input).ok_or("no active db; call db_use first or pass 'db'")?;
    // The agent loop is responsible for awaiting the Security approval card
    // when needs_approval is set. When the call reaches this executor the
    // approval has already been granted — but we still emit a red warning
    // marker in the output for destructive statements so the result is
    // auditable.
    let prefix = if d.verdict.is_destructive() {
        "⚠ DESTRUCTIVE statement executed: "
    } else if matches!(d.verdict, Verdict::Write) {
        "write statement executed: "
    } else {
        ""
    };
    // dbengine's db_sql is read-only. For writes we go through db_execute's
    // own dispatch: create_table/drop_table/insert/update/delete are first-class
    // engine tools. We route by verb so the engine applies real mutations.
    let verb = sql.split_whitespace().next().unwrap_or("").to_uppercase();
    let res = match verb.as_str() {
        "CREATE" => with_engine(|e| create_table_from_sql(e, &db, sql)),
        "DROP" => with_engine(|e| drop_from_sql(e, &db, sql)),
        "INSERT" => with_engine(|e| insert_from_sql(e, &db, sql)),
        "UPDATE" => with_engine(|e| update_from_sql(e, &db, sql)),
        "DELETE" => with_engine(|e| delete_from_sql(e, &db, sql)),
        _ => with_engine(|e| e.call("db_sql", &json!({"db": db, "sql": sql}))),
    }?;
    let body = serde_json::to_string_pretty(&res).map_err(|e| e.to_string())?;
    Ok(format!("{prefix}{body}"))
}

fn explain(cfg: &Config, input: &Value) -> Result<String, String> {
    let sql = input.get("sql").and_then(|v| v.as_str())
        .ok_or("db_explain requires 'sql'")?;
    let db = active_db(cfg, input).ok_or("no active db; call db_use first or pass 'db'")?;
    // The engine's db_sql understands EXPLAIN natively. Always read-only.
    let r = with_engine(|e| e.call("db_sql", &json!({"db": db, "sql": format!("EXPLAIN {sql}")})))?;
    Ok(serde_json::to_string_pretty(&r).map_err(|e| e.to_string())?)
}

/// Mask the credentials in a connection URL for logging/display.
/// `postgres://user:pass@host/db` → `postgres://***@host/db`.
pub fn mask_url(url: &str) -> String {
    // Find scheme separator, then userinfo, mask everything between "//" and "@".
    if let (Some(scheme_end), Some(at)) = (url.find("://"), url.rfind('@')) {
        let host_part = &url[at + 1..];
        let scheme = &url[..scheme_end + 3];
        format!("{scheme}***@{host_part}")
    } else {
        url.to_string()
    }
}

// ---- CREATE TABLE bridge: raw SQL → engine's db_create_table tool ----

/// Best-effort: route a raw `CREATE TABLE` SQL through the engine's
/// `db_create_table` tool by parsing columns out of the statement. The
/// engine's own mini-SQL does not speak DDL, so we bridge here.
fn create_table_from_sql(e: &mut DbEngine, db: &str, sql: &str) -> Result<Value, String> {
    // Very small parser: CREATE TABLE <name> ( col dtype, ... )
    // Locate case-insensitively on an ASCII-folded copy: unlike
    // `to_lowercase`, `to_ascii_lowercase` never changes byte length, so the
    // offset stays valid (and on a char boundary) in the original `sql`.
    let idx = sql
        .to_ascii_lowercase()
        .find("create table")
        .map(|i| i + 12)
        .unwrap_or(0);
    let rest = sql[idx..].trim();
    let paren = rest.find('(').ok_or("CREATE TABLE needs '('")?;
    let name_part = rest[..paren].trim().trim_matches('`').trim_matches('"');
    let cols_part = rest[paren + 1..].trim_end_matches(')').trim();
    let mut columns = Vec::new();
    for c in cols_part.split(',') {
        let c = c.trim();
        if c.is_empty() { continue; }
        let lc = c.to_lowercase();
        if lc.starts_with("primary") || lc.starts_with("foreign") || lc.starts_with("unique")
            || lc.starts_with("constraint") || lc.starts_with("key") || lc.starts_with("index") {
            continue;
        }
        let mut parts = c.split_whitespace();
        let cname = parts.next().unwrap_or("").trim_matches('`').trim_matches('"');
        let dtype = parts.next().unwrap_or("text").to_lowercase();
        if cname.is_empty() { continue; }
        let dtype = match dtype.as_str() {
            "int" | "integer" | "bigint" | "smallint" => "integer",
            "real" | "float" | "double" | "numeric" | "decimal" => "real",
            "bool" | "boolean" => "boolean",
            "json" | "jsonb" => "json",
            _ => "text",
        };
        columns.push(json!({"name": cname, "dtype": dtype}));
    }
    e.call("db_create_table", &json!({
        "db": db, "table": name_part, "columns": columns
    }))
}

// ---- raw write SQL → engine tools (DROP / INSERT / UPDATE / DELETE) ----

/// Byte offset of an ASCII case-insensitive, word-bounded `kw` in `s`,
/// outside quotes and parentheses. Keeps every slice on the ORIGINAL text
/// (word bounding stops matches inside identifiers like "offsets").
fn find_keyword(s: &str, kw: &str) -> Option<usize> {
    let b = s.as_bytes();
    let kb = kw.as_bytes();
    let word = |c: u8| c.is_ascii_alphanumeric() || c == b'_';
    let (mut depth, mut quote) = (0i32, None);
    let mut i = 0;
    while i < b.len() {
        let c = b[i];
        if let Some(q) = quote {
            if c == q {
                if b.get(i + 1) == Some(&q) {
                    i += 1; // escaped '' inside the literal
                } else {
                    quote = None;
                }
            }
        } else if c == b'\'' || c == b'"' {
            quote = Some(c);
        } else if c == b'(' {
            depth += 1;
        } else if c == b')' {
            depth = (depth - 1).max(0);
        } else if depth == 0
            && i + kb.len() <= b.len()
            && b[i..i + kb.len()].eq_ignore_ascii_case(kb)
            && (i == 0 || !word(b[i - 1]))
            && (i + kb.len() == b.len() || !word(b[i + kb.len()]))
        {
            return Some(i);
        }
        i += 1;
    }
    None
}

/// Split `s` on `kw` occurrences (same rules as find_keyword).
fn split_keyword(s: &str, kw: &str) -> Vec<String> {
    let mut parts = Vec::new();
    let mut rest = s;
    while let Some(i) = find_keyword(rest, kw) {
        parts.push(rest[..i].to_string());
        rest = &rest[i + kw.len()..];
    }
    parts.push(rest.to_string());
    parts
}

/// Split `s` on `sep` at paren-depth 0 and outside quotes.
fn split_top_level(s: &str, sep: char) -> Vec<String> {
    let mut out = Vec::new();
    let mut cur = String::new();
    let (mut depth, mut quote) = (0i32, None);
    for c in s.chars() {
        if let Some(q) = quote {
            cur.push(c);
            if c == q {
                quote = None;
            }
        } else {
            match c {
                '\'' | '"' => {
                    quote = Some(c);
                    cur.push(c);
                }
                '(' => {
                    depth += 1;
                    cur.push(c);
                }
                ')' => {
                    depth = (depth - 1).max(0);
                    cur.push(c);
                }
                _ if c == sep && depth == 0 => out.push(std::mem::take(&mut cur)),
                _ => cur.push(c),
            }
        }
    }
    out.push(cur);
    out
}

/// First SQL comparison operator in `s` outside quotes, longest first.
/// Returns (byte offset, canonical op) — `<>` is normalised to `!=`.
fn find_op(s: &str) -> Option<(usize, &'static str)> {
    let ops = [
        ("!=", "!="),
        ("<>", "!="),
        (">=", ">="),
        ("<=", "<="),
        ("=", "="),
        (">", ">"),
        ("<", "<"),
    ];
    let mut quote: Option<char> = None;
    for (i, c) in s.char_indices() {
        if let Some(q) = quote {
            if c == q {
                quote = None;
            }
        } else if c == '\'' || c == '"' {
            quote = Some(c);
        } else if let Some((_, canon)) = ops.iter().find(|(sym, _)| s[i..].starts_with(sym)) {
            return Some((i, *canon));
        }
    }
    None
}

/// One SQL literal: 'string' / "string" / NULL / TRUE / FALSE / number,
/// otherwise passed through as a string.
fn sql_value(s: &str) -> Value {
    let t = s.trim().trim_end_matches(';').trim();
    if t.len() >= 2
        && ((t.starts_with('\'') && t.ends_with('\''))
            || (t.starts_with('"') && t.ends_with('"')))
    {
        return json!(t[1..t.len() - 1].replace("''", "'"));
    }
    match t.to_ascii_lowercase().as_str() {
        "null" => Value::Null,
        "true" => json!(true),
        "false" => json!(false),
        _ => match t.parse::<i64>() {
            Ok(i) => json!(i),
            Err(_) => t
                .parse::<f64>()
                .map(|f| json!(f))
                .unwrap_or_else(|_| json!(t)),
        },
    }
}

/// Strip backticks/quotes from an identifier.
fn unquote_ident(s: &str) -> String {
    s.trim_matches('`').trim_matches('"').trim().to_string()
}

/// One WHERE condition → the engine's `[col, op, value]` filter triple.
/// Unsupported syntax is an explicit error, never a silent mis-read.
fn parse_condition(cond: &str) -> Result<Value, String> {
    let c = cond.trim().trim_end_matches(';').trim();
    let err = || {
        format!(
            "unsupported WHERE condition {c:?}: the engine accepts col =/!=/</<=/>/>=/LIKE value, \
             IS [NOT] NULL, joined by AND"
        )
    };
    for (kw, op) in [("is not null", "not_null"), ("is null", "is_null")] {
        if let Some(i) = find_keyword(c, kw) {
            let (col, tail) = (c[..i].trim(), c[i + kw.len()..].trim());
            if col.is_empty() || !tail.is_empty() {
                return Err(err());
            }
            return Ok(json!([unquote_ident(col), op, Value::Null]));
        }
    }
    if let Some(i) = find_keyword(c, "like") {
        let (col, val) = (c[..i].trim(), c[i + "like".len()..].trim());
        if col.is_empty() || val.is_empty() {
            return Err(err());
        }
        return Ok(json!([unquote_ident(col), "like", sql_value(val)]));
    }
    if let Some((i, op)) = find_op(c) {
        let (col, val) = (c[..i].trim(), c[i + op.len()..].trim());
        if col.is_empty() || val.is_empty() {
            return Err(err());
        }
        return Ok(json!([unquote_ident(col), op, sql_value(val)]));
    }
    Err(err())
}

/// Route `DROP TABLE [IF EXISTS] tbl` to the engine's db_drop_table tool.
fn drop_from_sql(e: &mut DbEngine, db: &str, sql: &str) -> Result<Value, String> {
    const ERR: &str = "only DROP TABLE [IF EXISTS] <name> can be routed to the engine's db_drop_table tool";
    let table_at = find_keyword(sql, "table").ok_or(ERR)? + "table".len();
    let mut name = sql[table_at..].trim().trim_end_matches(';').trim();
    if let Some(i) = find_keyword(name, "if exists") {
        name = name[i + "if exists".len()..].trim();
    }
    if name.is_empty() || name.contains(char::is_whitespace) {
        return Err(ERR.into());
    }
    e.call("db_drop_table", &json!({"db": db, "table": unquote_ident(name)}))
}

/// Route `INSERT INTO tbl (cols) VALUES (...), ...` to db_insert. A column
/// list is required — the engine's rows are name-keyed objects.
fn insert_from_sql(e: &mut DbEngine, db: &str, sql: &str) -> Result<Value, String> {
    const ERR: &str = "could not parse INSERT: expected 'INSERT INTO tbl (cols) VALUES (...), (...)' \
                       (the engine's db_insert takes name-keyed rows, so a column list is required)";
    let into = find_keyword(sql, "into").ok_or(ERR)?;
    let values_at = find_keyword(sql, "values").ok_or(ERR)?;
    if values_at < into {
        return Err(ERR.into());
    }
    let head = sql[into + "into".len()..values_at].trim();
    let (a, b) = match (head.find('('), head.rfind(')')) {
        (Some(a), Some(b)) if b > a => (a, b),
        _ => return Err(ERR.into()),
    };
    let table = unquote_ident(&head[..a]);
    if table.is_empty() {
        return Err(ERR.into());
    }
    let cols: Vec<String> = split_top_level(&head[a + 1..b], ',')
        .iter()
        .map(|c| unquote_ident(c))
        .collect();
    if cols.iter().any(|c| c.is_empty()) {
        return Err(ERR.into());
    }
    let lists = sql[values_at + "values".len()..]
        .trim()
        .trim_end_matches(';')
        .trim();
    let mut rows = Vec::new();
    for tuple in split_top_level(lists, ',') {
        let t = tuple.trim();
        if t.is_empty() {
            continue;
        }
        if !(t.starts_with('(') && t.ends_with(')')) {
            return Err("could not parse INSERT VALUES: expected parenthesised tuples".into());
        }
        let vals = split_top_level(&t[1..t.len() - 1], ',');
        if vals.len() != cols.len() {
            return Err(format!(
                "INSERT column count ({}) does not match value count ({})",
                cols.len(),
                vals.len()
            ));
        }
        let mut row = serde_json::Map::new();
        for (c, v) in cols.iter().zip(vals.iter()) {
            row.insert(c.clone(), sql_value(v));
        }
        rows.push(Value::Object(row));
    }
    if rows.is_empty() {
        return Err("INSERT parsed but no VALUES tuples found".into());
    }
    e.call("db_insert", &json!({"db": db, "table": table, "rows": rows}))
}

/// Route `UPDATE tbl SET col = value, ... [WHERE ...]` to db_update.
fn update_from_sql(e: &mut DbEngine, db: &str, sql: &str) -> Result<Value, String> {
    const ERR: &str = "could not parse UPDATE: expected 'UPDATE tbl SET col = value, ... [WHERE cond AND ...]'";
    let upd = find_keyword(sql, "update").ok_or(ERR)?;
    let set_at = find_keyword(sql, "set").ok_or(ERR)?;
    if set_at < upd {
        return Err(ERR.into());
    }
    let where_at = find_keyword(sql, "where");
    let table = unquote_ident(sql[upd + "update".len()..set_at].trim());
    if table.is_empty() {
        return Err(ERR.into());
    }
    let set_end = where_at.unwrap_or(sql.len());
    let mut set = serde_json::Map::new();
    for assign in split_top_level(&sql[set_at + "set".len()..set_end], ',') {
        let (col, val) = assign.split_once('=').ok_or(ERR)?;
        if col.trim().is_empty() || val.trim().is_empty() {
            return Err(ERR.into());
        }
        set.insert(unquote_ident(col), sql_value(val));
    }
    if set.is_empty() {
        return Err("UPDATE parsed but the SET clause is empty".into());
    }
    let mut args = json!({"db": db, "table": table, "set": Value::Object(set)});
    if let Some(w) = where_at {
        let mut filters = Vec::new();
        for cond in split_keyword(&sql[w + "where".len()..], "and") {
            filters.push(parse_condition(&cond)?);
        }
        if !filters.is_empty() {
            args["where"] = json!(filters);
        }
    }
    e.call("db_update", &args)
}

/// Route `DELETE FROM tbl [WHERE ...]` to db_delete.
fn delete_from_sql(e: &mut DbEngine, db: &str, sql: &str) -> Result<Value, String> {
    const ERR: &str = "could not parse DELETE: expected 'DELETE FROM tbl [WHERE cond AND ...]'";
    let from = find_keyword(sql, "from").ok_or(ERR)?;
    let where_at = find_keyword(sql, "where");
    let table_end = where_at.unwrap_or(sql.len());
    let table = unquote_ident(
        sql[from + "from".len()..table_end]
            .trim()
            .trim_end_matches(';')
            .trim(),
    );
    if table.is_empty() {
        return Err(ERR.into());
    }
    let mut args = json!({"db": db, "table": table});
    if let Some(w) = where_at {
        let mut filters = Vec::new();
        for cond in split_keyword(&sql[w + "where".len()..], "and") {
            filters.push(parse_condition(&cond)?);
        }
        if !filters.is_empty() {
            args["where"] = json!(filters);
        }
    }
    e.call("db_delete", &args)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::Config;

    fn cfg_readonly() -> Config {
        let mut c = Config::default();
        c.databases.tier = "read-only".into();
        c
    }

    #[test]
    fn mask_url_hides_password() {
        assert_eq!(mask_url("postgres://alice:s3cr3t@db.host:5432/app"), "postgres://***@db.host:5432/app");
        assert_eq!(mask_url("mysql://u:p@localhost/x"), "mysql://***@localhost/x");
        assert_eq!(mask_url("no-creds-here"), "no-creds-here");
    }

    #[test]
    fn list_returns_status() {
        // ephemeral engine; just ensure no error and output is non-empty.
        let out = list(&cfg_readonly()).unwrap();
        assert!(!out.is_empty());
    }

    #[test]
    fn query_refuses_drop_in_dbquery() {
        let cfg = cfg_readonly();
        let r = query(&cfg, &json!({"sql":"DROP TABLE x"}));
        assert!(r.is_err(), "expected error, got {r:?}");
        let msg = r.unwrap_err();
        assert!(msg.contains("read-only") || msg.contains("db_query"),
            "expected read-only/db_query refusal, got: {msg}");
    }

    #[test]
    fn execute_refuses_drop_in_readonly() {
        let cfg = cfg_readonly();
        let r = execute(&cfg, "db_execute", &json!({"sql":"DROP TABLE x"}), &AtomicBool::new(false));
        assert!(r.is_err(), "expected error, got {r:?}");
        let msg = r.unwrap_err();
        assert!(msg.contains("refused") || msg.contains("Read-Only") || msg.contains("destructive"),
            "expected refusal, got: {msg}");
    }

    #[test]
    fn tier_from_cfg_parses() {
        let mut c = Config::default();
        c.databases.tier = "full".into();
        assert_eq!(tier_from_cfg(&c), AccessTier::Full);
        c.databases.tier = "read-write".into();
        assert_eq!(tier_from_cfg(&c), AccessTier::ReadWrite);
        c.databases.tier = "read-only".into();
        assert_eq!(tier_from_cfg(&c), AccessTier::ReadOnly);
    }

    #[test]
    fn use_then_active_resolves() {
        use_db(&json!({"db":"mydb"})).unwrap();
        let cfg = cfg_readonly();
        assert_eq!(active_db(&cfg, &json!({})), Some("mydb".into()));
    }

    #[test]
    fn definition_count() {
        assert_eq!(definitions().len(), 6);
    }
}
