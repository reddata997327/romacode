//! Migrations — `makemigrations` / `migrate`.
//!
//! Migration files are JSON documents next to the project (default
//! `migrations/`), recording schema operations. Applied migrations are
//! tracked in the `_migrations` table.

use std::fs;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use super::backend::Backend;
use super::{ColumnDesc, FieldKind, OrmResult};

/// One schema operation.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(tag = "op")]
pub enum MigrationOp {
    CreateTable { table: String, columns: Vec<ColumnDesc> },
    AddColumn { table: String, column: ColumnDesc },
    DropColumn { table: String, name: String },
    DropTable { table: String },
}

impl MigrationOp {
    pub fn describe(&self) -> String {
        match self {
            MigrationOp::CreateTable { table, columns } => {
                format!("create table `{}` ({} columns)", table, columns.len())
            }
            MigrationOp::AddColumn { table, column } => {
                format!("add column `{}` to `{}` ({})", column.name, table, column.type_name())
            }
            MigrationOp::DropColumn { table, name } => format!("drop column `{name}` from `{table}`"),
            MigrationOp::DropTable { table } => format!("drop table `{table}`"),
        }
    }
}

/// A named migration file for one app.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Migration {
    pub app: String,
    pub name: String,
    pub ops: Vec<MigrationOp>,
}

impl Migration {
    pub fn new(app: &str, name: &str, ops: Vec<MigrationOp>) -> Self {
        Self { app: app.to_string(), name: name.to_string(), ops }
    }

    pub fn file_name(&self) -> String {
        format!("{}_{}.json", self.app, self.name)
    }

    pub fn write(&self, dir: &Path) -> OrmResult<PathBuf> {
        fs::create_dir_all(dir)?;
        let path = dir.join(self.file_name());
        let text = serde_json::to_string_pretty(self)?;
        fs::write(&path, text)?;
        Ok(path)
    }

    pub fn read(dir: &Path, file: &str) -> OrmResult<Migration> {
        let text = fs::read_to_string(dir.join(file))?;
        serde_json::from_str(&text)
            .map_err(|e| crate::errors::OrmError::Migration(format!("bad migration file {file}: {e}")))
    }

    /// List all migration files for an app, sorted by name.
    pub fn list(dir: &Path, app: &str) -> Vec<String> {
        let mut files: Vec<String> = fs::read_dir(dir)
            .map(|rd| {
                rd.filter_map(|e| e.ok())
                    .filter_map(|e| e.file_name().into_string().ok())
                    .filter(|f| f.starts_with(&format!("{app}_")) && f.ends_with(".json"))
                    .collect()
            })
            .unwrap_or_default();
        files.sort();
        files
    }

    /// Apply unapplied ops + record. Idempotent.
    pub fn apply(&self, backend: &dyn Backend) -> OrmResult<usize> {
        backend.ensure_migration_meta()?;
        let applied = backend.applied_migrations()?;
        let key = (self.app.clone(), self.name.clone());
        if applied.contains(&key) {
            return Ok(0);
        }
        let mut n = 0;
        for op in &self.ops {
            match op {
                MigrationOp::CreateTable { table, columns } => {
                    backend.create_table(table, columns.clone())?;
                    n += 1;
                }
                MigrationOp::AddColumn { table, column } => {
                    backend.add_column(table, column.clone())?;
                    n += 1;
                }
                MigrationOp::DropColumn { table, name } => {
                    let _ = name;
                    // Column drops are recorded but no-op'd on file stores;
                    // the JSON engine keeps column layouts append-only.
                    let _ = table;
                }
                MigrationOp::DropTable { table } => {
                    backend.drop_table(table)?;
                    n += 1;
                }
            }
        }
        backend.record_migration(&self.app, &self.name)?;
        Ok(n)
    }
}

/// Diff the registry's declared schema against the backend and produce the
/// migration ops needed — Django's `autodetector`.
pub fn detect_changes(backend: &dyn Backend, app: &str, metas: &[super::ModelMeta]) -> Vec<MigrationOp> {
    let mut ops = Vec::new();
    for meta in metas {
        let existing_cols = backend.columns(&meta.table);
        if existing_cols.is_empty() && !backend.tables().contains(&meta.table) {
            ops.push(MigrationOp::CreateTable {
                table: meta.table.clone(),
                columns: meta.columns.clone(),
            });
            continue;
        }
        for col in &meta.columns {
            if !existing_cols.iter().any(|c| c.name == col.name) {
                ops.push(MigrationOp::AddColumn { table: meta.table.clone(), column: col.clone() });
            }
        }
    }
    let _ = app;
    ops
}

/// Build (and optionally write) the next migration for an app.
pub fn makemigrations(
    backend: &dyn Backend,
    dir: &Path,
    app: &str,
    metas: &[super::ModelMeta],
    dry_run: bool,
) -> OrmResult<Option<Migration>> {
    let ops = detect_changes(backend, app, metas);
    if ops.is_empty() {
        return Ok(None);
    }
    let existing = Migration::list(dir, app);
    let next_num = existing.len() + 1;
    let name = if existing.is_empty() {
        "0001_initial".to_string()
    } else {
        format!("{:04}_auto", next_num)
    };
    let m = Migration::new(app, &name, ops);
    if !dry_run {
        m.write(dir)?;
    }
    Ok(Some(m))
}

/// Apply all unapplied migrations for every app found in `dir`.
pub fn migrate(backend: &dyn Backend, dir: &Path, app: Option<&str>) -> OrmResult<Vec<String>> {
    backend.ensure_migration_meta()?;
    let mut applied = Vec::new();
    let files: Vec<String> = fs::read_dir(dir)
        .map(|rd| {
            rd.filter_map(|e| e.ok())
                .filter_map(|e| e.file_name().into_string().ok())
                .filter(|f| f.ends_with(".json"))
                .collect::<Vec<_>>()
        })
        .unwrap_or_default();
    let mut files = files;
    files.sort();

    for file in files {
        let m = Migration::read(dir, &file)?;
        if let Some(want) = app {
            if m.app != want {
                continue;
            }
        }
        let n = m.apply(backend)?;
        if n > 0 || !backend.applied_migrations()?.contains(&(m.app.clone(), m.name.clone())) {
            backend.record_migration(&m.app, &m.name)?;
        }
        applied.push(format!("{}/{} ({} ops)", m.app, m.name, m.ops.len()));
    }
    Ok(applied)
}

/// Fake a migration (mark applied without running) — `migrate --fake`.
pub fn fake_migration(backend: &dyn Backend, app: &str, name: &str) -> OrmResult<()> {
    backend.ensure_migration_meta()?;
    backend.record_migration(app, name)
}

/// Human-readable list: applied vs pending — `showmigrations`.
pub fn show_migrations(backend: &dyn Backend, dir: &Path) -> OrmResult<String> {
    backend.ensure_migration_meta()?;
    let applied = backend.applied_migrations()?;
    let mut out = String::new();
    let files: Vec<String> = fs::read_dir(dir)
        .map(|rd| {
            rd.filter_map(|e| e.ok())
                .filter_map(|e| e.file_name().into_string().ok())
                .filter(|f| f.ends_with(".json"))
                .collect()
        })
        .unwrap_or_default();
    let mut files = files;
    files.sort();
    for f in files {
        let m = Migration::read(dir, &f)?;
        let done = applied.contains(&(m.app.clone(), m.name.clone()));
        out.push_str(&format!("[{}] {}/{}\n", if done { "X" } else { " " }, m.app, m.name));
    }
    Ok(out)
}

/// Whether a column kind is migration-significant (admin introspection).
pub fn is_relation(kind: &FieldKind) -> bool {
    matches!(kind, FieldKind::ForeignKey { .. })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::orm::{InMemoryBackend, Model, ModelExt};
    use std::sync::Arc;

    #[derive(Clone, Debug, crate::Model)]
#[model(table = "mig_post")]
    pub struct Post {
        #[field(auto)]
        pub id: i64,
        #[field(char, max = 80)]
        pub title: String,
    }

    fn meta() -> super::super::ModelMeta {
        super::super::ModelMeta::of::<Post>()
    }

    #[test]
    fn detect_create_table() {
        let b = InMemoryBackend::new();
        let ops = detect_changes(&b, "blog", &[meta()]);
        assert_eq!(ops.len(), 1);
        assert!(matches!(&ops[0], MigrationOp::CreateTable { table, .. } if table == "mig_post"));
    }

    #[test]
    fn detect_add_column() {
        let b = InMemoryBackend::new();
        b.create_table("mig_post", Post::columns().to_vec()).unwrap();
        let mut richer = meta();
        richer.columns.push(ColumnDesc::char("slug", 100));
        let ops = detect_changes(&b, "blog", &[richer]);
        assert_eq!(ops.len(), 1);
        assert!(matches!(&ops[0], MigrationOp::AddColumn { column, .. } if column.name == "slug"));
    }

    #[test]
    fn makemigrate_and_migrate_flow() {
        let dir = std::env::temp_dir().join(format!("rustango-mig-{}", uuid::Uuid::new_v4()));
        let b = Arc::new(InMemoryBackend::new());

        // fresh backend -> initial migration detected
        let m = makemigrations(&*b, &dir, "blog", &[meta()], false)
            .unwrap()
            .expect("initial migration expected");
        assert_eq!(m.name, "0001_initial");
        assert!(dir.join("blog_0001_initial.json").exists());

        // apply
        let applied = migrate(&*b, &dir, None).unwrap();
        assert_eq!(applied.len(), 1);
        assert!(b.tables().contains(&"mig_post".to_string()));
        assert!(b.applied_migrations().unwrap().contains(&("blog".into(), "0001_initial".into())));

        // re-migrate: idempotent
        let again = migrate(&*b, &dir, None).unwrap();
        assert_eq!(again.len(), 1); // seen but not re-applied

        // no further changes when schema matches
        let nochange = makemigrations(&*b, &dir, "blog", &[meta()], false).unwrap();
        assert!(nochange.is_none());

        // add a column -> auto migration
        let mut richer = meta();
        richer.columns.push(ColumnDesc::char("slug", 100));
        let m2 = makemigrations(&*b, &dir, "blog", &[richer], false).unwrap().unwrap();
        assert!(m2.name.starts_with("0002"));
        migrate(&*b, &dir, None).unwrap();
        let cols = b.columns("mig_post");
        assert!(cols.iter().any(|c| c.name == "slug"));

        let report = show_migrations(&*b, &dir).unwrap();
        assert!(report.contains("[X] blog/0001_initial"));
        assert!(report.contains("[X] blog/0002_auto"));

        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn migration_file_roundtrip() {
        let m = Migration::new("blog", "0003_x", vec![
            MigrationOp::DropTable { table: "old".into() },
        ]);
        let dir = std::env::temp_dir().join(format!("rustango-mig2-{}", uuid::Uuid::new_v4()));
        let p = m.write(&dir).unwrap();
        let back = Migration::read(&dir, &m.file_name()).unwrap();
        assert_eq!(back, m);
        std::fs::remove_file(p).ok();
    }

    #[test]
    fn fake_migration_marks_applied() {
        let b = InMemoryBackend::new();
        b.ensure_migration_meta().unwrap();
        fake_migration(&b, "blog", "0001_initial").unwrap();
        assert!(b.applied_migrations().unwrap().contains(&("blog".into(), "0001_initial".into())));
    }

    #[test]
    fn apply_creates_and_inserts_work() {
        let dir = std::env::temp_dir().join(format!("rustango-mig3-{}", uuid::Uuid::new_v4()));
        let b = InMemoryBackend::new();
        let m = Migration::new("blog", "0001_initial", vec![MigrationOp::CreateTable {
            table: "mig_post".into(),
            columns: Post::columns().to_vec(),
        }]);
        m.write(&dir).unwrap();
        migrate(&b, &dir, None).unwrap();
        let p = Post { id: 0, title: "hello".into() };
        let saved = p.save(&b).unwrap();
        assert_eq!(saved.pk(), 1);
        std::fs::remove_dir_all(&dir).ok();
    }
}
