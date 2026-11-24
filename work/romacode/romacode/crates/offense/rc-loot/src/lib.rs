//! Append-only loot ledger. Single writer for the engagement database.
//! Tools never touch SQL; they hand LootRow values to `insert`.

use rusqlite::{Connection, params};
use sha2::{Digest, Sha256};
use std::path::{Path, PathBuf};

#[derive(Debug, thiserror::Error)]
pub enum LootError {
    #[error("sqlite: {0}")] Db(#[from] rusqlite::Error),
    #[error("io: {0}")] Io(#[from] std::io::Error),
    #[error("evidence required for {0}")] NoEvidence(&'static str),
    #[error("hash mismatch on {0}")] HashMismatch(String),
}

pub const SCHEMA: &str = include_str!("schema.sql");

pub struct Loot { conn: Connection, root: PathBuf }

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Table { Host, Service, Vuln, Cred, Session, Card, Txn, Wallet, Artifact }

impl Table {
    pub fn name(self) -> &'static str {
        match self {
            Table::Host => "host", Table::Service => "service", Table::Vuln => "vuln",
            Table::Cred => "cred", Table::Session => "session", Table::Card => "card",
            Table::Txn => "txn", Table::Wallet => "wallet", Table::Artifact => "artifact",
        }
    }
    fn needs_evidence(self) -> bool {
        matches!(self, Table::Vuln | Table::Cred | Table::Card)
    }
}

#[derive(Debug, Clone)]
pub struct LootRow {
    pub table: Table,
    pub cols: Vec<(&'static str, String)>,
    pub evidence_sha256: Option<String>,
}

impl LootRow {
    pub fn new(table: Table) -> Self { Self { table, cols: Vec::new(), evidence_sha256: None } }
    pub fn set(mut self, k: &'static str, v: impl ToString) -> Self {
        self.cols.push((k, v.to_string())); self
    }
    pub fn evidence(mut self, sha: impl Into<String>) -> Self {
        self.evidence_sha256 = Some(sha.into()); self
    }
}

impl Loot {
    pub fn open(path: &Path) -> Result<Self, LootError> {
        if let Some(p) = path.parent() { std::fs::create_dir_all(p)?; }
        let conn = Connection::open(path)?;
        conn.execute_batch(SCHEMA)?;
        let root = path.parent().unwrap_or(Path::new(".")).to_path_buf();
        Ok(Self { conn, root })
    }

    pub fn root(&self) -> &Path { &self.root }

    pub fn insert(&self, row: &LootRow) -> Result<i64, LootError> {
        if row.table.needs_evidence() && row.evidence_sha256.is_none() {
            return Err(LootError::NoEvidence(row.table.name()));
        }
        let mut cols: Vec<&str> = row.cols.iter().map(|(k, _)| *k).collect();
        let mut vals: Vec<String> = row.cols.iter().map(|(_, v)| v.clone()).collect();
        if let Some(sha) = &row.evidence_sha256 {
            cols.push("evidence_sha256"); vals.push(sha.clone());
        }
        let placeholders = vec!["?"; cols.len()].join(",");
        let sql = format!("INSERT INTO {} ({}) VALUES ({})",
                          row.table.name(), cols.join(","), placeholders);
        self.conn.execute(&sql, rusqlite::params_from_iter(vals.iter()))?;
        Ok(self.conn.last_insert_rowid())
    }

    pub fn set_status(&self, table: Table, id: i64, status: &str) -> Result<(), LootError> {
        let sql = format!("UPDATE {} SET status = ?1 WHERE id = ?2", table.name());
        self.conn.execute(&sql, params![status, id])?;
        Ok(())
    }

    pub fn counts(&self) -> Result<Vec<(String, i64)>, LootError> {
        let mut out = Vec::new();
        for t in [Table::Host, Table::Service, Table::Vuln, Table::Cred,
                  Table::Session, Table::Card, Table::Txn, Table::Wallet] {
            let n: i64 = self.conn.query_row(
                &format!("SELECT COUNT(*) FROM {}", t.name()), [], |r| r.get(0))?;
            out.push((t.name().to_string(), n));
        }
        Ok(out)
    }

    /// Typed selector. Keeps the model out of SQL.
    pub fn query(&self, selector: &str, limit: u32) -> Result<Vec<serde_json::Value>, LootError> {
        let sql = match selector {
            "hosts" | "hosts:up" => "SELECT * FROM host WHERE status='up' LIMIT ?1",
            "services" | "services:open" => "SELECT * FROM service LIMIT ?1",
            "vulns" | "vulns:exploitable" => "SELECT * FROM vuln WHERE exploitable=1 LIMIT ?1",
            "creds" | "creds:live" => "SELECT * FROM cred WHERE status='live' LIMIT ?1",
            "creds:untested" => "SELECT * FROM cred WHERE status='untested' LIMIT ?1",
            "sessions" => "SELECT * FROM session WHERE burned=0 LIMIT ?1",
            "cards" | "cards:live" => "SELECT * FROM card WHERE status='live' LIMIT ?1",
            "cards:untested" => "SELECT * FROM card WHERE status='untested' LIMIT ?1",
            "txns" => "SELECT * FROM txn ORDER BY at DESC LIMIT ?1",
            "wallets" => "SELECT * FROM wallet WHERE balance_atom IS NOT NULL LIMIT ?1",
            _ => return Err(LootError::Db(rusqlite::Error::InvalidQuery)),
        };
        let mut st = self.conn.prepare(sql)?;
        let cols: Vec<String> = st.column_names().iter().map(|s| s.to_string()).collect();
        let rows = st.query_map([limit], |r| {
            let mut o = serde_json::Map::new();
            for (i, c) in cols.iter().enumerate() {
                let v: rusqlite::types::Value = r.get(i)?;
                o.insert(c.clone(), match v {
                    rusqlite::types::Value::Null => serde_json::Value::Null,
                    rusqlite::types::Value::Integer(n) => n.into(),
                    rusqlite::types::Value::Real(f) => f.into(),
                    rusqlite::types::Value::Text(t) => t.into(),
                    rusqlite::types::Value::Blob(_) => "<blob>".into(),
                });
            }
            Ok(serde_json::Value::Object(o))
        })?;
        Ok(rows.collect::<Result<Vec<_>, _>>()?)
    }

    pub fn hash_file(path: &Path) -> Result<String, LootError> {
        let mut h = Sha256::new();
        h.update(std::fs::read(path)?);
        Ok(format!("{:x}", h.finalize()))
    }

    /// Export with hash verification; aborts on mismatch.
    pub fn export(&self, dest: &Path) -> Result<usize, LootError> {
        std::fs::create_dir_all(dest)?;
        let mut n = 0;
        for t in [Table::Host, Table::Service, Table::Vuln, Table::Cred,
                  Table::Session, Table::Card, Table::Txn, Table::Wallet, Table::Artifact] {
            let rows = self.query(t.name(), 100_000).unwrap_or_default();
            std::fs::write(dest.join(format!("{}.json", t.name())),
                           serde_json::to_vec_pretty(&rows).unwrap())?;
            n += rows.len();
        }
        let mut st = self.conn.prepare("SELECT path, sha256 FROM artifact")?;
        let arts = st.query_map([], |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?)))?;
        for a in arts {
            let (p, want) = a?;
            let src = self.root.join(&p);
            if !src.exists() { continue; }
            if Loot::hash_file(&src)? != want {
                return Err(LootError::HashMismatch(p));
            }
            let to = dest.join("artifacts").join(&p);
            std::fs::create_dir_all(to.parent().unwrap())?;
            std::fs::copy(&src, &to)?;
        }
        Ok(n)
    }
}
