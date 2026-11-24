//! Live database monitors for the D→S→D timeline.
//!
//! Attaches to a database and polls what is ACTUALLY running right now:
//!   postgres — pg_stat_activity (state, wait, ms running, query text)
//!   mysql    — information_schema.processlist (command, time, state, info)
//!   sqlite   — file change detection (mtime/size of db + wal/-journal)
//!
//! Polling runs on a plain std::thread because the connection uses the
//! vendored std-only wire-protocol client (romacode-db), which is blocking —
//! and that is fine: one lightweight query per poll interval.
//!
//! Honest limitation (recorded as a DbNote when sqlite attaches): sqlite has
//! no activity table an outside process can read, so the monitor reports
//! file-level changes, not SQL text. For SQL-level tracing on sqlite the app
//! would need in-process hooks.

use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use serde_json::{json, Value};

use crate::session::{self, DbMonHandle, DsdEvent};

/// Poll interval bounds (ms) — too fast hammering pg_stat_activity is
/// counterproductive, too slow misses short queries.
const POLL_MIN: u64 = 100;
const POLL_MAX: u64 = 5_000;

fn clamp_poll(ms: u64) -> u64 {
    ms.clamp(POLL_MIN, POLL_MAX)
}

/// Parse `postgres://user:pass@host:port/db` / `mysql://…` into a conn object
/// the db engine accepts, or build it from explicit fields.
pub fn conn_object(backend: &str, dsn: &str, explicit: &Value) -> Result<Value, String> {
    let mut host = explicit["host"].as_str().unwrap_or("127.0.0.1").to_string();
    let mut port = explicit["port"].as_u64().unwrap_or(match backend {
        "postgres" => 5432,
        _ => 3306,
    });
    let mut user = explicit["user"].as_str().unwrap_or("").to_string();
    let mut password = explicit["password"].as_str().unwrap_or("").to_string();
    let mut database = explicit["database"].as_str().unwrap_or("").to_string();

    if !dsn.is_empty() {
        let rest = dsn
            .split("://")
            .nth(1)
            .ok_or_else(|| format!("bad dsn (want {backend}://user:pass@host:port/db): {dsn}"))?;
        let (auth, hostpart) = rest.split_once('@').ok_or("dsn missing user:pass@host part")?;
        let (u, p) = auth.split_once(':').unwrap_or((auth, ""));
        user = u.to_string();
        password = p.to_string();
        let (h, dbpart) = hostpart.split_once('/').ok_or("dsn missing /database part")?;
        database = dbpart.trim_end_matches('/').to_string();
        if let Some((hh, pp)) = h.rsplit_once(':') {
            if pp.chars().all(|c| c.is_ascii_digit()) && !pp.is_empty() {
                host = hh.to_string();
                port = pp.parse().map_err(|_| "bad port in dsn")?;
            } else {
                host = h.to_string();
            }
        } else {
            host = h.to_string();
        }
    }
    if user.is_empty() {
        return Err("no database user given (dsn or \"user\" field)".into());
    }
    Ok(json!({
        "kind": if backend == "postgres" { "postgres" } else { "mysql" },
        "host": host,
        "port": port as i64,
        "user": user,
        "password": password,
        "database": database,
    }))
}

fn engine_call(conn: &Value, sql: &str) -> Result<Value, String> {
    let mut eng = romacode_db::DbEngine::ephemeral().map_err(|e| format!("db engine: {e}"))?;
    eng.call("db_remote_query", &json!({ "conn": conn, "sql": sql }))
}

fn rows_of(res: &Value) -> Vec<Value> {
    res["data"].as_array().cloned().unwrap_or_default()
}

/// Attach a postgres or mysql monitor. Probes once (validates credentials
/// and connectivity) before spawning the poller.
pub fn attach_sql(
    backend: &str,
    conn: Value,
    poll_ms: u64,
    label: &str,
) -> Result<DbMonHandle, String> {
    let poll = clamp_poll(poll_ms);
    let probe_sql = if backend == "postgres" {
        "SELECT 1"
    } else {
        "SELECT 1"
    };
    engine_call(&conn, probe_sql).map_err(|e| format!("{backend} probe failed: {e}"))?;

    let stop = Arc::new(AtomicBool::new(false));
    let stop2 = stop.clone();
    let backend_s = backend.to_string();
    let label_s = label.to_string();
    let thread = std::thread::Builder::new()
        .name(format!("dsd-dbmon-{backend}"))
        .spawn(move || poll_loop(&backend_s, conn, poll, stop2, &label_s))
        .map_err(|e| format!("spawn poller: {e}"))?;
    Ok(DbMonHandle {
        backend: backend.to_string(),
        target: label.to_string(),
        stop,
        thread: Some(thread),
    })
}

fn poll_loop(backend: &str, conn: Value, poll_ms: u64, stop: Arc<AtomicBool>, label: &str) {
    let sql = if backend == "postgres" {
        "SELECT pid, usename, datname, state, wait_event_type, \
         EXTRACT(EPOCH FROM (now() - query_start)) * 1000 AS ms, query \
         FROM pg_stat_activity \
         WHERE state <> 'idle' AND pid <> pg_backend_pid()"
    } else {
        "SELECT id, user, IFNULL(db,'') AS db, command, time, IFNULL(state,'') AS state, \
         IFNULL(info,'') AS info \
         FROM information_schema.processlist \
         WHERE command <> 'Sleep' AND id <> CONNECTION_ID()"
    };
    // A fresh engine per poll keeps reconnect logic trivial: the vendored
    // client reconnects on each call, surviving restarts of the database.
    while !stop.load(Ordering::Relaxed) {
        match engine_call(&conn, sql) {
            Ok(res) => {
                for row in rows_of(&res) {
                    record_sql_row(backend, &row);
                }
            }
            Err(e) => {
                session::record(DsdEvent::DbNote {
                    ts: SystemTime::now(),
                    backend: backend.to_string(),
                    text: format!("poll failed ({label}): {e}"),
                });
            }
        }
        // sleep in small slices so stop is responsive
        let mut waited = 0u64;
        while waited < poll_ms && !stop.load(Ordering::Relaxed) {
            std::thread::sleep(Duration::from_millis(50));
            waited += 50;
        }
    }
}

fn s(v: &Value) -> String {
    match v {
        Value::Null => String::new(),
        Value::String(t) => t.clone(),
        other => other.to_string(),
    }
}

fn record_sql_row(backend: &str, row: &Value) {
    let ts = SystemTime::now();
    let query = if backend == "postgres" {
        row["query"].as_str().unwrap_or("")
    } else {
        row["info"].as_str().unwrap_or("")
    };
    // Skip our own monitor query if it ever shows up.
    if query.contains("pg_stat_activity") || query.contains("information_schema.processlist") {
        return;
    }
    let ms = row["ms"].as_f64().unwrap_or(0.0);
    let ms = if backend == "postgres" {
        ms
    } else {
        row["time"].as_f64().unwrap_or(0.0) * 1000.0
    };
    session::record(DsdEvent::DbQuery {
        ts,
        backend: backend.to_string(),
        pid: s(&row[if backend == "postgres" { "pid" } else { "id" }]),
        user: s(&row[if backend == "postgres" { "usename" } else { "user" }]),
        db: s(&row["db"]),
        state: if backend == "postgres" {
            s(&row["state"])
        } else {
            s(&row["command"])
        },
        ms_so_far: ms,
        query: query.to_string(),
    });
}

/// Attach a sqlite file-change monitor (db file + its wal/journal siblings).
pub fn attach_sqlite(path: &str, poll_ms: u64) -> Result<DbMonHandle, String> {
    let p = PathBuf::from(path);
    let meta = std::fs::metadata(&p).map_err(|e| format!("sqlite file {path}: {e}"))?;
    if !meta.is_file() {
        return Err(format!("{path} is not a file"));
    }
    let poll = clamp_poll(poll_ms);
    let stop = Arc::new(AtomicBool::new(false));
    let stop2 = stop.clone();
    let path2 = p.clone();
    let thread = std::thread::Builder::new()
        .name("dsd-dbmon-sqlite".into())
        .spawn(move || {
            // watched files: the db file plus its journal/wal siblings
            let mut files: Vec<(PathBuf, (u128, u64))> = vec![(path2.clone(), stat_of(&path2))];
            for ext in ["-wal", "-journal"] {
                let mut p2 = path2.clone();
                let name = p2.file_name().unwrap_or_default().to_string_lossy().to_string();
                p2.set_file_name(format!("{name}{ext}"));
                let st = stat_of(&p2);
                files.push((p2, st));
            }

            session::record(DsdEvent::DbNote {
                ts: SystemTime::now(),
                backend: "sqlite".into(),
                text: format!(
                    "watching {} ({} bytes) — file-level changes only; sqlite has no \
                     cross-process activity table for SQL text",
                    path2.display(),
                    files[0].1 .1
                ),
            });
            while !stop2.load(Ordering::Relaxed) {
                std::thread::sleep(Duration::from_millis(poll));
                for (path, last) in files.iter_mut() {
                    let now = stat_of(path);
                    if now != *last {
                        let delta = now.1 as i64 - last.1 as i64;
                        let size = if delta >= 0 {
                            format!("+{delta}")
                        } else {
                            format!("{delta}")
                        };
                        session::record(DsdEvent::DbNote {
                            ts: SystemTime::now(),
                            backend: "sqlite".into(),
                            text: format!("{} changed: {} bytes ({})", path.display(), size, fmt_epoch_ms(now.0)),
                        });
                        *last = now;
                    }
                }
            }
        })
        .map_err(|e| format!("spawn sqlite watcher: {e}"))?;
    Ok(DbMonHandle {
        backend: "sqlite".into(),
        target: path.to_string(),
        stop,
        thread: Some(thread),
    })
}

fn stat_of(p: &PathBuf) -> (u128, u64) {
    match std::fs::metadata(p) {
        Ok(m) => (
            m.modified().ok().and_then(|t| t.duration_since(UNIX_EPOCH).ok()).map(|d| d.as_millis()).unwrap_or(0),
            m.len(),
        ),
        Err(_) => (0, 0),
    }
}

fn fmt_epoch_ms(ms: u128) -> String {
    let st = UNIX_EPOCH + Duration::from_millis(ms as u64);
    session::fmt_clock(st)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn conn_object_from_explicit_fields() {
        let c = conn_object("postgres", "", &json!({"host": "db.local", "user": "app", "database": "shop"})).unwrap();
        assert_eq!(c["host"], "db.local");
        assert_eq!(c["port"], 5432);
        assert_eq!(c["database"], "shop");
    }

    #[test]
    fn conn_object_from_dsn() {
        let c = conn_object("mysql", "mysql://app:secret@10.0.0.5:3307/shop", &json!({})).unwrap();
        assert_eq!(c["host"], "10.0.0.5");
        assert_eq!(c["port"], 3307);
        assert_eq!(c["user"], "app");
        assert_eq!(c["password"], "secret");
        assert_eq!(c["database"], "shop");
        assert_eq!(c["kind"], "mysql");
    }

    #[test]
    fn dsn_without_user_is_rejected() {
        let err = conn_object("postgres", "postgres://@h/db", &json!({})).unwrap_err();
        assert!(err.contains("no database user"));
    }

    #[test]
    fn poll_interval_is_clamped() {
        assert_eq!(clamp_poll(1), POLL_MIN);
        assert_eq!(clamp_poll(999_999), POLL_MAX);
    }
}
