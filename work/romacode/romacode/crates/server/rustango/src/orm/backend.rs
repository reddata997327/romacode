//! Storage backends. The [`Backend`] trait is the `django.db.backends`
//! equivalent; two pure-Rust implementations ship in the box:
//!
//! * [`InMemoryBackend`] — everything lives in RAM (tests, scratch work).
//! * [`JsonFileBackend`] — the same engine, snapshotted to a JSON file on
//!   every write (durable dev storage, no C SQLite needed).

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, RwLock};

use serde::{Deserialize, Serialize};

use super::{eval_criteria, order_rows, ColumnDesc, Criteria, FieldKind, OrmResult, Row, Value};

/// What a storage backend must provide. All operations are synchronous and
/// thread-safe; criteria evaluation (incl. FK traversal) happens inside the
/// backend so callers work with plain `Value` rows.
pub trait Backend: Send + Sync {
    /// Insert a full row; returns the assigned primary key.
    fn insert(&self, table: &str, row: Row) -> OrmResult<i64>;
    /// Update rows matching `crit`; `updates` are (column_index, new_value).
    /// Returns the number of rows updated.
    fn update(&self, table: &str, crit: &Criteria, updates: &[(usize, Value)]) -> OrmResult<usize>;
    /// Delete rows matching `crit`; returns the number deleted.
    fn delete(&self, table: &str, crit: &Criteria) -> OrmResult<usize>;
    /// Select rows: criteria + order (`(field, desc)`) + limit + offset.
    fn select(
        &self,
        table: &str,
        crit: &Criteria,
        order: &[(String, bool)],
        limit: Option<usize>,
        offset: Option<usize>,
    ) -> OrmResult<Vec<Row>>;
    fn count(&self, table: &str, crit: &Criteria) -> OrmResult<usize>;

    // -- schema (migration support) ---------------------------------------
    fn create_table(&self, table: &str, columns: Vec<ColumnDesc>) -> OrmResult<()>;
    fn add_column(&self, table: &str, column: ColumnDesc) -> OrmResult<()>;
    fn drop_table(&self, table: &str) -> OrmResult<()>;
    fn tables(&self) -> Vec<String>;
    fn columns(&self, table: &str) -> Vec<ColumnDesc>;

    // -- migration bookkeeping --------------------------------------------
    fn ensure_migration_meta(&self) -> OrmResult<()>;
    fn applied_migrations(&self) -> OrmResult<Vec<(String, String)>>;
    fn record_migration(&self, app: &str, name: &str) -> OrmResult<()>;

    /// Optional hook so the JSON backend can flush its snapshot.
    fn flush(&self) -> OrmResult<()> { Ok(()) }
}

// ---------------------------------------------------------------------------
// Shared engine
// ---------------------------------------------------------------------------

#[derive(Debug, Default)]
pub(crate) struct Engine {
    tables: HashMap<String, Table>,
}

#[derive(Debug, Default)]
struct Table {
    columns: Vec<ColumnDesc>,
    rows: Vec<Row>,
    next_pk: i64,
}

impl Engine {
    fn table(&self, name: &str) -> OrmResult<&Table> {
        self.tables
            .get(name)
            .ok_or_else(|| crate::errors::OrmError::Backend(format!("no such table: {name}")))
    }

    fn table_mut(&mut self, name: &str) -> OrmResult<&mut Table> {
        self.tables
            .get_mut(name)
            .ok_or_else(|| crate::errors::OrmError::Backend(format!("no such table: {name}")))
    }

    /// Insert with auto-pk, defaults and unique-constraint enforcement.
    fn insert_row(&mut self, table: &str, mut row: Row) -> OrmResult<i64> {
        let cols = self.table(table)?.columns.clone();

        // 1. Auto primary key.
        let has_auto_pk = cols
            .first()
            .map(|c| c.primary_key && c.kind == FieldKind::Auto)
            .unwrap_or(false);
        if has_auto_pk {
            let needs_id = row
                .first()
                .map(|v| v == &Value::Null || v.as_int() == Some(0) || v.as_int().is_none())
                .unwrap_or(true);
            let t = self.table_mut(table)?;
            if needs_id {
                t.next_pk += 1;
                if row.is_empty() {
                    row.push(Value::Int(t.next_pk));
                } else {
                    row[0] = Value::Int(t.next_pk);
                }
            } else if let Some(id) = row.first().and_then(|v| v.as_int()) {
                if id > t.next_pk {
                    t.next_pk = id;
                }
            }
        }

        // 2. Defaults + auto_now_add.
        for (i, col) in cols.iter().enumerate() {
            if i >= row.len() {
                continue;
            }
            let is_null = row[i] == Value::Null;
            if !is_null {
                continue;
            }
            if let Some(d) = &col.default {
                row[i] = d.clone();
            } else if col.kind == FieldKind::DateTimeAutoNowAdd {
                row[i] = Value::DateTime(chrono::Utc::now());
            }
        }

        // 3. Unique constraints (scan in place, no recursion).
        {
            let t = self.table(table)?;
            for (i, col) in cols.iter().enumerate() {
                if !col.unique || col.primary_key {
                    continue;
                }
                let val = row.get(i).cloned().unwrap_or(Value::Null);
                if val == Value::Null {
                    continue;
                }
                for other in &t.rows {
                    let other_val = other.get(i).cloned().unwrap_or(Value::Null);
                    if other_val == val {
                        let other_pk = other.first().and_then(|v| v.as_int());
                        let my_pk = row.first().and_then(|v| v.as_int());
                        if other_pk != my_pk {
                            return Err(crate::errors::OrmError::InvalidValue(
                                col.name.clone(),
                                "unique constraint violated".into(),
                            ));
                        }
                    }
                }
            }
        }

        // 4. Explicit primary keys must be unique: a caller-supplied pk that
        // already exists would silently create a duplicate row and make
        // update/delete-by-pk hit every copy.
        let pk = row.first().and_then(|v| v.as_int()).unwrap_or(0);
        if pk != 0 {
            let t = self.table(table)?;
            if t.rows
                .iter()
                .any(|r| r.first().and_then(|v| v.as_int()) == Some(pk))
            {
                return Err(crate::errors::OrmError::Backend(format!(
                    "duplicate primary key {pk} in table {table}"
                )));
            }
        }

        let t = self.table_mut(table)?;
        t.rows.push(row);
        Ok(pk)
    }
}

// ---------------------------------------------------------------------------
// InMemoryBackend
// ---------------------------------------------------------------------------

/// Pure in-memory backend. Cheap, fast, non-durable.
pub struct InMemoryBackend {
    engine: RwLock<Engine>,
}

impl InMemoryBackend {
    pub fn new() -> Self {
        Self { engine: RwLock::new(Engine::default()) }
    }

    pub(crate) fn with_engine(e: Engine) -> InMemoryBackend {
        Self { engine: RwLock::new(e) }
    }

    /// All rows of a table (test/debug helper).
    pub fn debug_rows(&self, table: &str) -> Vec<Row> {
        self.select(table, &Criteria::any(), &[], None, None).unwrap_or_default()
    }
}

impl Default for InMemoryBackend {
    fn default() -> Self { Self::new() }
}

/// Row identity used to re-find snapshot rows in phase 2 without re-matching
/// by full value equality: the primary key (first column) when non-Null,
/// otherwise the whole row (pk-less tables keep the previous behavior).
///
/// Matching between the two lock phases by full row equality mutated rows
/// inserted concurrently between the phases (and every value-duplicate row)
/// as collateral damage; identity matching touches exactly the rows that
/// matched the criteria under the lock.
enum MatchId {
    Pk(Value),
    FullRow(Row),
}

impl MatchId {
    fn of(row: &Row) -> Self {
        match row.first() {
            Some(pk) if pk != &Value::Null => MatchId::Pk(pk.clone()),
            _ => MatchId::FullRow(row.clone()),
        }
    }

    fn matches(&self, row: &Row) -> bool {
        match self {
            MatchId::Pk(pk) => row.first() == Some(pk),
            MatchId::FullRow(snapshot) => snapshot == row,
        }
    }
}

impl Backend for InMemoryBackend {
    fn insert(&self, table: &str, row: Row) -> OrmResult<i64> {
        self.engine.write().unwrap().insert_row(table, row)
    }

    fn update(&self, table: &str, crit: &Criteria, updates: &[(usize, Value)]) -> OrmResult<usize> {
        // Phase 1 (read lock): snapshot + evaluate criteria, capturing the
        // identity (primary key / id value) of every matching row.
        // FK-traversal criteria call back into `select()`, which must not
        // happen while the write lock is held (same-thread re-entry would
        // deadlock).
        let matched_ids: Vec<MatchId> = {
            let engine = self.engine.read().unwrap();
            let t = engine.table(table)?;
            let cols = t.columns.clone();
            t.rows
                .iter()
                .filter(|row| eval_criteria(self, table, row, &cols, crit))
                .map(|row| MatchId::of(row))
                .collect()
        };
        if matched_ids.is_empty() {
            return Ok(0);
        }
        // Phase 2 (write lock): apply updates by row identity captured in
        // phase 1 — never by re-matching values. Full-value equality between
        // the two lock phases mutated concurrent identical inserts and every
        // duplicate row as collateral damage.
        let mut engine = self.engine.write().unwrap();
        let t = engine.table_mut(table)?;
        let mut n = 0;
        for row in t.rows.iter_mut() {
            if matched_ids.iter().any(|m| m.matches(row)) {
                for (idx, val) in updates {
                    if *idx < row.len() {
                        row[*idx] = val.clone();
                    }
                }
                n += 1;
            }
        }
        Ok(n)
    }

    fn delete(&self, table: &str, crit: &Criteria) -> OrmResult<usize> {
        // Phase 1 (read lock): resolve matching row identities (see `update`).
        let matched_ids: Vec<MatchId> = {
            let engine = self.engine.read().unwrap();
            let t = engine.table(table)?;
            let cols = t.columns.clone();
            t.rows
                .iter()
                .filter(|row| eval_criteria(self, table, row, &cols, crit))
                .map(|row| MatchId::of(row))
                .collect()
        };
        if matched_ids.is_empty() {
            return Ok(0);
        }
        // Phase 2 (write lock): drop rows whose identity was captured in
        // phase 1.
        let mut engine = self.engine.write().unwrap();
        let t = engine.table_mut(table)?;
        let before = t.rows.len();
        t.rows.retain(|row| !matched_ids.iter().any(|m| m.matches(row)));
        Ok(before - t.rows.len())
    }

    fn select(
        &self,
        table: &str,
        crit: &Criteria,
        order: &[(String, bool)],
        limit: Option<usize>,
        offset: Option<usize>,
    ) -> OrmResult<Vec<Row>> {
        let engine = self.engine.read().unwrap();
        let t = engine.table(table)?;
        let cols = t.columns.clone();
        let mut matched: Vec<Row> = t
            .rows
            .iter()
            .filter(|row| eval_criteria(self, table, row, &cols, crit))
            .cloned()
            .collect();
        drop(engine);
        order_rows(self, table, &mut matched, &cols, order);
        if let Some(off) = offset {
            if off >= matched.len() {
                return Ok(vec![]);
            }
            matched.drain(0..off);
        }
        if let Some(lim) = limit {
            matched.truncate(lim);
        }
        Ok(matched)
    }

    fn count(&self, table: &str, crit: &Criteria) -> OrmResult<usize> {
        let engine = self.engine.read().unwrap();
        let t = engine.table(table)?;
        let cols = t.columns.clone();
        Ok(t.rows
            .iter()
            .filter(|row| eval_criteria(self, table, row, &cols, crit))
            .count())
    }

    fn create_table(&self, table: &str, columns: Vec<ColumnDesc>) -> OrmResult<()> {
        let mut engine = self.engine.write().unwrap();
        if engine.tables.contains_key(table) {
            return Ok(()); // idempotent, like IF NOT EXISTS
        }
        engine.tables.insert(
            table.to_string(),
            Table { columns, rows: vec![], next_pk: 0 },
        );
        Ok(())
    }

    fn add_column(&self, table: &str, column: ColumnDesc) -> OrmResult<()> {
        let mut engine = self.engine.write().unwrap();
        let t = engine.table_mut(table)?;
        if t.columns.iter().any(|c| c.name == column.name) {
            return Ok(());
        }
        let fill = column.default.clone().unwrap_or(Value::Null);
        t.columns.push(column);
        for row in t.rows.iter_mut() {
            row.push(fill.clone());
        }
        Ok(())
    }

    fn drop_table(&self, table: &str) -> OrmResult<()> {
        self.engine.write().unwrap().tables.remove(table);
        Ok(())
    }

    fn tables(&self) -> Vec<String> {
        let mut v: Vec<String> = self.engine.read().unwrap().tables.keys().cloned().collect();
        v.sort();
        v
    }

    fn columns(&self, table: &str) -> Vec<ColumnDesc> {
        self.engine.read().unwrap().tables.get(table).map(|t| t.columns.clone()).unwrap_or_default()
    }

    fn ensure_migration_meta(&self) -> OrmResult<()> {
        self.create_table(
            "_migrations",
            vec![
                ColumnDesc::auto("id"),
                ColumnDesc::char("app", 100),
                ColumnDesc::char("name", 255),
                ColumnDesc::datetime("applied_at", false),
            ],
        )
    }

    fn applied_migrations(&self) -> OrmResult<Vec<(String, String)>> {
        let crit = Criteria::any();
        let spec = vec![(String::from("id"), false)];
        let rows = self.select("_migrations", &crit, &spec, None, None)?;
        Ok(rows
            .into_iter()
            .filter_map(|r| match (r.get(1), r.get(2)) {
                (Some(Value::Text(a)), Some(Value::Text(b))) => Some((a.clone(), b.clone())),
                _ => None,
            })
            .collect())
    }

    fn record_migration(&self, app: &str, name: &str) -> OrmResult<()> {
        self.insert(
            "_migrations",
            vec![
                Value::Null,
                Value::Text(app.to_string()),
                Value::Text(name.to_string()),
                Value::DateTime(chrono::Utc::now()),
            ],
        )?;
        Ok(())
    }
}

// ---------------------------------------------------------------------------
// JsonFileBackend
// ---------------------------------------------------------------------------

#[derive(Serialize, Deserialize, Default)]
struct SnapshotTable {
    columns: Vec<ColumnDesc>,
    rows: Vec<Row>,
}

#[derive(Serialize, Deserialize, Default)]
struct Snapshot {
    tables: std::collections::BTreeMap<String, SnapshotTable>,
}

/// In-memory engine persisted to a JSON file. Every mutating call re-writes
/// the snapshot atomically (write temp file, rename over the target), so a
/// crash can never leave a half-written database behind.
pub struct JsonFileBackend {
    engine: InMemoryBackend,
    path: PathBuf,
}

impl JsonFileBackend {
    /// Open (or create) a database file.
    pub fn open(path: impl AsRef<Path>) -> OrmResult<Self> {
        let path = path.as_ref().to_path_buf();
        let engine = if path.exists() {
            let text = std::fs::read_to_string(&path)?;
            let snap: Snapshot = serde_json::from_str(&text)?;
            let mut e = Engine::default();
            for (name, t) in snap.tables {
                let max: i64 = t
                    .rows
                    .iter()
                    .filter_map(|r| r.first().and_then(|v| v.as_int()))
                    .max()
                    .unwrap_or(0);
                e.tables.insert(name, Table { columns: t.columns, rows: t.rows, next_pk: max });
            }
            InMemoryBackend::with_engine(e)
        } else {
            InMemoryBackend::new()
        };
        let backend = Self { engine, path };
        backend.flush()?;
        Ok(backend)
    }
}

impl std::ops::Deref for JsonFileBackend {
    type Target = InMemoryBackend;
    fn deref(&self) -> &InMemoryBackend { &self.engine }
}

impl Backend for JsonFileBackend {
    fn insert(&self, table: &str, row: Row) -> OrmResult<i64> {
        let pk = self.engine.insert(table, row)?;
        self.flush()?;
        Ok(pk)
    }

    fn update(&self, table: &str, crit: &Criteria, updates: &[(usize, Value)]) -> OrmResult<usize> {
        let n = self.engine.update(table, crit, updates)?;
        if n > 0 {
            self.flush()?;
        }
        Ok(n)
    }

    fn delete(&self, table: &str, crit: &Criteria) -> OrmResult<usize> {
        let n = self.engine.delete(table, crit)?;
        if n > 0 {
            self.flush()?;
        }
        Ok(n)
    }

    fn select(
        &self,
        table: &str,
        crit: &Criteria,
        order: &[(String, bool)],
        limit: Option<usize>,
        offset: Option<usize>,
    ) -> OrmResult<Vec<Row>> {
        self.engine.select(table, crit, order, limit, offset)
    }

    fn count(&self, table: &str, crit: &Criteria) -> OrmResult<usize> {
        self.engine.count(table, crit)
    }

    fn create_table(&self, table: &str, columns: Vec<ColumnDesc>) -> OrmResult<()> {
        self.engine.create_table(table, columns)?;
        // The engine's create_table is idempotent (IF NOT EXISTS): when the
        // table already existed nothing changed, so skip the full-DB rewrite
        // — this used to flush on EVERY request via Session::load.
        if self.engine.tables().iter().any(|t| t == table) {
            return Ok(());
        }
        self.flush()
    }

    fn add_column(&self, table: &str, column: ColumnDesc) -> OrmResult<()> {
        self.engine.add_column(table, column)?;
        self.flush()
    }

    fn drop_table(&self, table: &str) -> OrmResult<()> {
        self.engine.drop_table(table)?;
        self.flush()
    }

    fn tables(&self) -> Vec<String> { self.engine.tables() }

    fn columns(&self, table: &str) -> Vec<ColumnDesc> { self.engine.columns(table) }

    fn ensure_migration_meta(&self) -> OrmResult<()> {
        self.engine.ensure_migration_meta()?;
        self.flush()
    }

    fn applied_migrations(&self) -> OrmResult<Vec<(String, String)>> {
        self.engine.applied_migrations()
    }

    fn record_migration(&self, app: &str, name: &str) -> OrmResult<()> {
        self.engine.record_migration(app, name)?;
        self.flush()
    }

    fn flush(&self) -> OrmResult<()> {
        let mut snap = Snapshot::default();
        for table in self.engine.tables() {
            let cols = self.engine.columns(&table);
            let crit = Criteria::any();
            let rows = self.engine.select(&table, &crit, &[], None, None)?;
            snap.tables.insert(table, SnapshotTable { columns: cols, rows });
        }
        let text = serde_json::to_string(&snap)?;
        if let Some(parent) = self.path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        // Unique temp name: two threads flushing concurrently must not
        // clobber each other's temp file before the atomic rename.
        let tmp = self
            .path
            .with_extension(format!("tmp-{}", uuid::Uuid::new_v4().simple()));
        std::fs::write(&tmp, text)?;
        std::fs::rename(&tmp, &self.path)?;
        Ok(())
    }
}

/// Build a backend from a settings alias: `memory` | `json:<path>`.
pub fn backend_from_alias(alias: &str) -> OrmResult<Arc<dyn Backend>> {
    if alias == "memory" || alias.is_empty() {
        Ok(Arc::new(InMemoryBackend::new()))
    } else if let Some(path) = alias.strip_prefix("json:") {
        Ok(Arc::new(JsonFileBackend::open(path)?))
    } else {
        Err(crate::errors::OrmError::Backend(format!(
            "unknown backend `{alias}` (supported: memory, json:<path>)"
        )))
    }
}

/// Re-export the shared unique checker for backend implementors.
pub use super::check_unique as verify_unique;

#[allow(dead_code)]
fn _assert_send_sync() {
    fn f<T: Send + Sync>() {}
    f::<InMemoryBackend>();
    f::<JsonFileBackend>();
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::orm::Op;

    fn user_cols() -> Vec<ColumnDesc> {
        vec![
            ColumnDesc::auto("id"),
            ColumnDesc::char("username", 150).unique(),
            ColumnDesc::boolean("is_active", true),
        ]
    }

    fn row(id: Option<i64>, name: &str, active: bool) -> Row {
        vec![
            id.map(Value::Int).unwrap_or(Value::Null),
            Value::Text(name.into()),
            Value::Bool(active),
        ]
    }

    #[test]
    fn insert_auto_pk_and_select() {
        let b = InMemoryBackend::new();
        b.create_table("auth_user", user_cols()).unwrap();
        let id = b.insert("auth_user", row(None, "alice", true)).unwrap();
        assert_eq!(id, 1);
        let id2 = b.insert("auth_user", row(None, "bob", false)).unwrap();
        assert_eq!(id2, 2);

        let all = b.select("auth_user", &Criteria::any(), &[], None, None).unwrap();
        assert_eq!(all.len(), 2);

        let crit = Criteria::eq("username", Value::Text("alice".into()));
        let alice = b.select("auth_user", &crit, &[], None, None).unwrap();
        assert_eq!(alice.len(), 1);
        assert_eq!(alice[0][1], Value::Text("alice".into()));
    }

    #[test]
    fn explicit_pk_respected() {
        let b = InMemoryBackend::new();
        b.create_table("auth_user", user_cols()).unwrap();
        let id = b.insert("auth_user", row(Some(41), "zoe", true)).unwrap();
        assert_eq!(id, 41);
        let next = b.insert("auth_user", row(None, "abe", true)).unwrap();
        assert_eq!(next, 42); // counter advanced past explicit pk
    }

    #[test]
    fn update_delete_count() {
        let b = InMemoryBackend::new();
        b.create_table("auth_user", user_cols()).unwrap();
        b.insert("auth_user", row(None, "alice", true)).unwrap();
        b.insert("auth_user", row(None, "bob", true)).unwrap();

        let crit = Criteria::eq("username", Value::Text("bob".into()));
        let n = b.update("auth_user", &crit, &[(2, Value::Bool(false))]).unwrap();
        assert_eq!(n, 1);

        assert_eq!(b.count("auth_user", &Criteria::eq("is_active", Value::Bool(false))).unwrap(), 1);
        let n = b.delete("auth_user", &Criteria::eq("is_active", Value::Bool(false))).unwrap();
        assert_eq!(n, 1);
        assert_eq!(b.count("auth_user", &Criteria::any()).unwrap(), 1);
    }

    #[test]
    fn update_delete_match_rows_by_identity_not_value() {
        // Regression (M31): phase 2 used to re-match rows by full VALUE
        // equality against phase-1 snapshots, so a row inserted concurrently
        // between the two lock phases with identical values was mutated too,
        // and duplicate value-rows all got updated/deleted. Matching must be
        // by the captured unique primary-key/id instead.
        let snapshot = row(Some(1), "same", true);
        let id = MatchId::of(&snapshot);
        // Same identity -> match.
        assert!(id.matches(&row(Some(1), "same", true)));
        // Concurrent identical insert (different pk, same values) -> no match.
        assert!(!id.matches(&row(Some(2), "same", true)));
        // Same identity with mutated values (row being updated) -> still matches.
        assert!(id.matches(&row(Some(1), "changed", false)));
        // A Null-pk row falls back to whole-row identity.
        let null_row = row(None, "anon", true);
        let null_id = MatchId::of(&null_row);
        assert!(matches!(null_id, MatchId::FullRow(_)));
        assert!(null_id.matches(&row(None, "anon", true)));
        assert!(!null_id.matches(&row(None, "other", true)));
    }

    #[test]
    fn update_by_pk_touches_exactly_the_matching_row() {
        let b = InMemoryBackend::new();
        let cols = vec![ColumnDesc::auto("id"), ColumnDesc::char("name", 50)];
        b.create_table("dupes", cols).unwrap();
        let id1 = b.insert("dupes", vec![Value::Null, Value::Text("same".into())]).unwrap();
        let id2 = b.insert("dupes", vec![Value::Null, Value::Text("same".into())]).unwrap();
        assert_ne!(id1, id2);
        let n = b
            .update("dupes", &Criteria::eq("id", Value::Int(id1)), &[(1, Value::Text("changed".into()))])
            .unwrap();
        assert_eq!(n, 1);
        let rows = b.select("dupes", &Criteria::any(), &[], None, None).unwrap();
        let changed = rows.iter().filter(|r| r[1] == Value::Text("changed".into())).count();
        assert_eq!(changed, 1);
        let n = b.delete("dupes", &Criteria::eq("id", Value::Int(id2))).unwrap();
        assert_eq!(n, 1);
        assert_eq!(b.count("dupes", &Criteria::any()).unwrap(), 1);
    }

    #[test]
    fn fk_traversal_criteria_do_not_deadlock_update_delete() {
        // Regression: update()/delete() used to evaluate criteria while the
        // write lock was held; FK-traversal criteria call back into
        // select(), which deadlocked the same thread. Must complete.
        use crate::orm::{register_model, ModelMeta};

        let user_cols = vec![ColumnDesc::auto("id"), ColumnDesc::char("username", 150)];
        let post_cols = vec![
            ColumnDesc::auto("id"),
            ColumnDesc::char("title", 100),
            ColumnDesc::foreign_key("author_id", "fktr_user"),
        ];
        register_model(ModelMeta {
            table: "fktr_user".into(),
            columns: user_cols.clone(),
            fks: vec![],
        });
        register_model(ModelMeta {
            table: "fktr_post".into(),
            columns: post_cols.clone(),
            fks: vec![("author_id".into(), "fktr_user".into())],
        });

        let b = InMemoryBackend::new();
        b.create_table("fktr_user", user_cols).unwrap();
        b.create_table("fktr_post", post_cols).unwrap();
        let uid = b
            .insert("fktr_user", vec![Value::Null, Value::Text("ada".into())])
            .unwrap();
        b.insert(
            "fktr_post",
            vec![Value::Null, Value::Text("one".into()), Value::Int(uid)],
        )
        .unwrap();
        b.insert(
            "fktr_post",
            vec![Value::Null, Value::Text("two".into()), Value::Int(uid + 1)],
        )
        .unwrap();

        let crit = Criteria::eq("author_id__username", Value::Text("ada".into()));
        let n = b
            .update("fktr_post", &crit, &[(1, Value::Text("edited".into()))])
            .unwrap();
        assert_eq!(n, 1);
        let n = b.delete("fktr_post", &crit).unwrap();
        assert_eq!(n, 1);
        assert_eq!(b.count("fktr_post", &Criteria::any()).unwrap(), 1);
    }

    #[test]
    fn unique_constraint_enforced() {
        let b = InMemoryBackend::new();
        b.create_table("auth_user", user_cols()).unwrap();
        b.insert("auth_user", row(None, "alice", true)).unwrap();
        let dup = b.insert("auth_user", row(None, "alice", false));
        assert!(dup.is_err());
    }

    #[test]
    fn order_limit_offset() {
        let b = InMemoryBackend::new();
        b.create_table("auth_user", user_cols()).unwrap();
        for name in ["carol", "alice", "bob"] {
            b.insert("auth_user", row(None, name, true)).unwrap();
        }
        let mut spec = vec![(String::from("username"), false)];
        let asc = b.select("auth_user", &Criteria::any(), &spec, None, None).unwrap();
        assert_eq!(asc[0][1], Value::Text("alice".into()));
        spec[0].1 = true;
        let desc = b.select("auth_user", &Criteria::any(), &spec, None, None).unwrap();
        assert_eq!(desc[0][1], Value::Text("carol".into()));
        let page = b.select("auth_user", &Criteria::any(), &[], Some(2), Some(1)).unwrap();
        assert_eq!(page.len(), 2);
    }

    #[test]
    fn debug_and_probe() {
        let b = InMemoryBackend::new();
        b.create_table("auth_user", user_cols()).unwrap();
        b.insert("auth_user", row(None, "alice", true)).unwrap();
        let single = b.count("auth_user", &Criteria::eq("is_active", Value::Bool(true))).unwrap();
        let contains = b.count("auth_user", &Criteria::contains("username", "l")).unwrap();
        let both = b.count("auth_user", &(Criteria::eq("is_active", Value::Bool(true)) & Criteria::contains("username", "l"))).unwrap();
        assert_eq!(single, 1, "single eq");
        let eq_name = b.count("auth_user", &Criteria::eq("username", Value::Text("alice".into()))).unwrap();
        assert_eq!(eq_name, 1, "eq username");
        let gt = b.count("auth_user", &Criteria::cond("id", Op::Gt, Value::Int(0))).unwrap();
        assert_eq!(gt, 1, "gt id");
        let icontains = b.count("auth_user", &Criteria::icontains("username", "LIC")).unwrap();
        assert_eq!(icontains, 1, "icontains");
        assert_eq!(contains, 1, "contains");
        assert_eq!(both, 1, "AND");
    }

    #[test]
    fn and_or_not_criteria() {
        let b = InMemoryBackend::new();
        b.create_table("auth_user", user_cols()).unwrap();
        b.insert("auth_user", row(None, "alice", true)).unwrap();
        b.insert("auth_user", row(None, "bob", false)).unwrap();
        b.insert("auth_user", row(None, "carol", true)).unwrap();

        let crit =
            Criteria::eq("is_active", Value::Bool(true)) & Criteria::contains("username", "ali");
        assert_eq!(b.count("auth_user", &crit).unwrap(), 1); // alice

        let crit = Criteria::eq("username", Value::Text("bob".into()))
            | Criteria::eq("username", Value::Text("carol".into()));
        assert_eq!(b.count("auth_user", &crit).unwrap(), 2);

        let crit = !Criteria::eq("is_active", Value::Bool(true));
        assert_eq!(b.count("auth_user", &crit).unwrap(), 1);
    }

    #[test]
    fn in_lookup() {
        let b = InMemoryBackend::new();
        b.create_table("auth_user", user_cols()).unwrap();
        for name in ["alice", "bob", "carol"] {
            b.insert("auth_user", row(None, name, true)).unwrap();
        }
        let crit = Criteria::in_(
            "username",
            vec![Value::Text("alice".into()), Value::Text("carol".into())],
        );
        assert_eq!(b.count("auth_user", &crit).unwrap(), 2);
    }

    #[test]
    fn defaults_and_add_column() {
        let b = InMemoryBackend::new();
        b.create_table("auth_user", user_cols()).unwrap();
        b.insert("auth_user", vec![Value::Null, Value::Text("alice".into()), Value::Null]).unwrap();
        // boolean default true was applied
        let rows = b.select("auth_user", &Criteria::any(), &[], None, None).unwrap();
        assert_eq!(rows[0][2], Value::Bool(true));
        assert_eq!(rows[0][0], Value::Int(1));

        b.add_column("auth_user", ColumnDesc::char("email", 100)).unwrap();
        let rows = b.select("auth_user", &Criteria::any(), &[], None, None).unwrap();
        assert_eq!(rows[0].len(), 4);
        assert_eq!(rows[0][3], Value::Null);
    }

    #[test]
    fn json_backend_persistence() {
        let dir = std::env::temp_dir().join(format!("rustango-test-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        let db = dir.join("db.json");

        {
            let b = JsonFileBackend::open(&db).unwrap();
            b.create_table("auth_user", user_cols()).unwrap();
            b.insert("auth_user", row(None, "alice", true)).unwrap();
        }
        // reopen: data must survive
        {
            let b = JsonFileBackend::open(&db).unwrap();
            assert_eq!(b.count("auth_user", &Criteria::any()).unwrap(), 1);
            b.insert("auth_user", row(None, "bob", false)).unwrap();
        }
        {
            let b = JsonFileBackend::open(&db).unwrap();
            assert_eq!(b.count("auth_user", &Criteria::any()).unwrap(), 2);
        }
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn migrations_meta() {
        let b = InMemoryBackend::new();
        b.ensure_migration_meta().unwrap();
        assert!(b.applied_migrations().unwrap().is_empty());
        b.record_migration("polls", "0001_initial").unwrap();
        let applied = b.applied_migrations().unwrap();
        assert_eq!(applied, vec![("polls".into(), "0001_initial".into())]);
    }

    #[test]
    fn field_kinds_roundtrip() {
        let c = ColumnDesc::foreign_key("author_id", "auth_user");
        let json = serde_json::to_string(&c).unwrap();
        let back: ColumnDesc = serde_json::from_str(&json).unwrap();
        assert_eq!(back.kind, FieldKind::ForeignKey { target_table: "auth_user".into() });
    }
}
