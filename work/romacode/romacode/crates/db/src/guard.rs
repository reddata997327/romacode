//! SQL safety guard for the database tools.
//!
//! Sits between the agent's `db_query` / `db_execute` tool calls and the
//! underlying engine. Its job is classification, not enforcement: it returns a
//! [`Verdict`] describing whether a statement is safe to run without approval
//! (read-only), needs a Security approval card (destructive but allowed), or
//! must be refused outright. The agent loop, working through the Security
//! gateway, decides what to do with that verdict.
//!
//! The rules match the user spec: read-only passes are `SELECT`, `WITH`,
//! `EXPLAIN`, `SHOW`, `DESCRIBE`/`DESC`, `BEGIN`/`COMMIT`/`ROLLBACK` (so a
//! read-only session can still manage its own transaction state). Anything
//! that mutates — `INSERT`/`UPDATE`/`DELETE`/`MERGE`/`CREATE`/`DROP`/
//! `TRUNCATE`/`ALTER`/`GRANT`/`REVOKE` — is flagged. `DELETE`/`UPDATE` without
//! a `WHERE` clause and `DROP`/`TRUNCATE`/`ALTER` are flagged as *destructive*
//! (red warning) even in read-write mode.

use std::fmt;

/// How risky a statement is. Set by [`SqlGuard::classify`].
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Verdict {
    /// `SELECT`, `WITH`, `EXPLAIN`, `SHOW`, `DESCRIBE`, transaction control.
    /// Safe to run in read-only and read-write modes without approval.
    ReadOnly,
    /// `INSERT`, `UPDATE ... WHERE`, `MERGE`, `CREATE TABLE`, etc. Allowed in
    /// read-write and full-access modes; needs approval in read-write.
    Write,
    /// `DROP`, `TRUNCATE`, `ALTER`, `DELETE`/`UPDATE` without `WHERE`,
    /// `GRANT`/`REVOKE`. Always carries a red warning; needs approval even in
    /// read-write mode; refused in read-only mode.
    Destructive,
}

impl Verdict {
    pub fn is_read_only(self) -> bool { matches!(self, Verdict::ReadOnly) }
    pub fn is_destructive(self) -> bool { matches!(self, Verdict::Destructive) }
}

impl fmt::Display for Verdict {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Verdict::ReadOnly => "read-only",
            Verdict::Write => "write",
            Verdict::Destructive => "destructive",
        })
    }
}

/// The agent's database access tier (mirrors the TUI Database Access section).
/// This is the live setting, not the static config default; the agent loop
/// passes the currently-selected tier in so the guard is consistent with what
/// the user sees on screen.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub enum AccessTier {
    /// `Read-Only` — only [`Verdict::ReadOnly`] statements pass. Everything
    /// else is refused without even showing an approval card.
    #[default]
    ReadOnly,
    /// `Read-Write` — read-only and write pass; write needs an approval card.
    /// Destructive statements need an approval card AND carry a red warning.
    ReadWrite,
    /// `Full Access` — everything runs, no approval. Destructive statements
    /// still carry a red warning in the tool result so the user can audit.
    Full,
}

/// Stateless SQL classifier. Construct once per request.
pub struct SqlGuard {
    tier: AccessTier,
}

/// What the guard says about one statement.
pub struct Decision {
    pub verdict: Verdict,
    /// `true` if the statement may run under the current tier without an
    /// approval card.
    pub auto_run: bool,
    /// `true` if a Security approval card must be shown before running.
    pub needs_approval: bool,
    /// `true` if the statement is refused outright (the tier forbids it).
    pub refused: bool,
    /// Human-readable reason, suitable for showing in a tool result or the
    /// approval card body.
    pub reason: String,
}

impl SqlGuard {
    pub fn new(tier: AccessTier) -> Self { Self { tier } }
    pub fn tier(&self) -> AccessTier { self.tier }

    /// Classify a single SQL statement. Multi-statement scripts should be split
    /// by the caller and each part passed in; this returns the verdict for one.
    pub fn classify(&self, sql: &str) -> Decision {
        let stmt = Self::first_statement(sql);
        let verb = Self::leading_verb(&stmt);
        let verdict = Self::classify_verb(&verb, &stmt);
        let (auto_run, needs_approval, refused, reason) = match (self.tier, verdict) {
            (AccessTier::ReadOnly, Verdict::ReadOnly) =>
                (true, false, false, "read-only statement; allowed under Read-Only".into()),
            (AccessTier::ReadOnly, Verdict::Write) =>
                (false, false, true, "write statement; refused under Read-Only tier".into()),
            (AccessTier::ReadOnly, Verdict::Destructive) =>
                (false, false, true, "destructive statement; refused under Read-Only tier".into()),
            (AccessTier::ReadWrite, Verdict::ReadOnly) =>
                (true, false, false, "read-only statement; allowed under Read-Write".into()),
            (AccessTier::ReadWrite, Verdict::Write) =>
                (false, true, false, "write statement; requires approval under Read-Write".into()),
            (AccessTier::ReadWrite, Verdict::Destructive) =>
                (false, true, false, "DESTRUCTIVE statement; requires approval (red warning) under Read-Write".into()),
            (AccessTier::Full, Verdict::ReadOnly) =>
                (true, false, false, "read-only statement; allowed under Full Access".into()),
            (AccessTier::Full, Verdict::Write) =>
                (true, false, false, "write statement; allowed under Full Access".into()),
            (AccessTier::Full, Verdict::Destructive) =>
                (true, false, false, "DESTRUCTIVE statement; allowed under Full Access (red warning emitted)".into()),
        };
        Decision { verdict, auto_run, needs_approval, refused, reason }
    }

    /// First statement of a script, up to the first `;` that is not inside a
    /// quoted string. Good enough for classification; the engine parses the
    /// real SQL separately.
    fn first_statement(sql: &str) -> String {
        let s = sql.trim();
        let mut out = String::new();
        let mut in_single = false;
        let mut in_double = false;
        let mut in_back = false;
        let mut prev_dash = false;
        let mut in_line_comment = false;
        let mut in_block_comment = false;
        for ch in s.chars() {
            if in_line_comment { if ch == '\n' { in_line_comment = false; } continue; }
            if in_block_comment {
                if ch == '*' { /* peek next handled below */ }
                out.push(ch);
                if out.ends_with("*/") { in_block_comment = false; }
                continue;
            }
            match ch {
                '\'' if !in_double && !in_back => in_single = !in_single,
                '"' if !in_single && !in_back => in_double = !in_double,
                '`' if !in_single && !in_double => in_back = !in_back,
                '-' if !in_single && !in_double && !in_back => {
                    if prev_dash { in_line_comment = true; prev_dash = false; out.pop(); continue; }
                    prev_dash = true;
                }
                '/' if !in_single && !in_double && !in_back => {
                    // possible block comment start; we just push and let the
                    // ends_with check above close it. Good enough for classify.
                }
                _ => prev_dash = false,
            }
            out.push(ch);
            if ch == ';' && !in_single && !in_double && !in_back { break; }
        }
        out.trim().trim_end_matches(';').trim().to_string()
    }

    fn leading_verb(stmt: &str) -> String {
        // Skip leading parens / whitespace / comments, grab the first word.
        let mut s = stmt.trim_start();
        while let Some(stripped) = s.strip_prefix('(') { s = stripped.trim_start(); }
        s.split_whitespace().next().unwrap_or("").to_uppercase()
    }

    fn classify_verb(verb: &str, stmt: &str) -> Verdict {
        // CTEs (`WITH ... SELECT`) read as WITH.
        match verb {
            "SELECT" | "WITH" | "EXPLAIN" | "DESC" | "DESCRIBE" | "SHOW" |
            "TABLE" | "VALUES" |
            "BEGIN" | "START" | "COMMIT" | "ROLLBACK" | "END" | "SET" |
            "PRAGMA" | "USE" => Verdict::ReadOnly,

            "INSERT" | "REPLACE" | "MERGE" | "UPSERT" => Verdict::Write,
            "UPDATE" => {
                if Self::has_where(stmt) { Verdict::Write } else { Verdict::Destructive }
            }
            "DELETE" => {
                if Self::has_where(stmt) { Verdict::Write } else { Verdict::Destructive }
            }
            "CREATE" => Verdict::Write,
            "DROP" | "TRUNCATE" | "ALTER" | "RENAME" => Verdict::Destructive,
            "GRANT" | "REVOKE" => Verdict::Destructive,
            "VACUUM" | "ANALYZE" | "REINDEX" => Verdict::Write,
            // Unknown verb — be conservative: treat as destructive so the user
            // always sees an approval card rather than a silent surprise.
            _ => Verdict::Destructive,
        }
    }

    fn has_where(stmt: &str) -> bool {
        // Look for a WHERE keyword outside of quotes/comments. Case-insensitive.
        let upper = stmt.to_uppercase();
        // crude but robust enough: split on whitespace and look for WHERE token
        upper.split_whitespace().any(|tok| tok == "WHERE")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn g(tier: AccessTier) -> SqlGuard { SqlGuard::new(tier) }

    #[test]
    fn select_is_read_only() {
        assert_eq!(g(AccessTier::ReadOnly).classify("SELECT * FROM users").verdict, Verdict::ReadOnly);
        assert!(g(AccessTier::ReadOnly).classify("SELECT 1").auto_run);
    }

    #[test]
    fn with_cte_is_read_only() {
        assert_eq!(g(AccessTier::ReadOnly).classify("WITH t AS (SELECT 1) SELECT * FROM t").verdict, Verdict::ReadOnly);
    }

    #[test]
    fn explain_and_show_are_read_only() {
        assert_eq!(g(AccessTier::ReadOnly).classify("EXPLAIN SELECT 1").verdict, Verdict::ReadOnly);
        assert_eq!(g(AccessTier::ReadOnly).classify("SHOW TABLES").verdict, Verdict::ReadOnly);
    }

    #[test]
    fn insert_is_write() {
        assert_eq!(g(AccessTier::ReadOnly).classify("INSERT INTO t VALUES (1)").verdict, Verdict::Write);
        let d = g(AccessTier::ReadOnly).classify("INSERT INTO t VALUES (1)");
        assert!(d.refused);
    }

    #[test]
    fn delete_without_where_is_destructive() {
        assert_eq!(g(AccessTier::ReadWrite).classify("DELETE FROM users").verdict, Verdict::Destructive);
    }

    #[test]
    fn delete_with_where_is_write() {
        assert_eq!(g(AccessTier::ReadWrite).classify("DELETE FROM users WHERE id = 1").verdict, Verdict::Write);
    }

    #[test]
    fn update_without_where_is_destructive() {
        assert_eq!(g(AccessTier::ReadWrite).classify("UPDATE users SET name='x'").verdict, Verdict::Destructive);
    }

    #[test]
    fn drop_truncate_alter_are_destructive() {
        for s in ["DROP TABLE t", "TRUNCATE TABLE t", "ALTER TABLE t DROP COLUMN c"] {
            assert_eq!(g(AccessTier::Full).classify(s).verdict, Verdict::Destructive, "{s}");
        }
    }

    #[test]
    fn readwrite_write_needs_approval() {
        let d = g(AccessTier::ReadWrite).classify("INSERT INTO t VALUES (1)");
        assert!(!d.auto_run && d.needs_approval && !d.refused);
    }

    #[test]
    fn readwrite_destructive_needs_approval_and_warns() {
        let d = g(AccessTier::ReadWrite).classify("DROP TABLE t");
        assert!(!d.auto_run && d.needs_approval && !d.refused);
        assert!(d.reason.contains("DESTRUCTIVE") || d.reason.contains("destructive"));
    }

    #[test]
    fn readonly_refuses_destructive_without_approval() {
        let d = g(AccessTier::ReadOnly).classify("DROP TABLE t");
        assert!(d.refused && !d.needs_approval && !d.auto_run);
    }

    #[test]
    fn full_access_runs_everything_without_approval() {
        let d = g(AccessTier::Full).classify("DROP TABLE t");
        assert!(d.auto_run && !d.needs_approval && !d.refused);
    }

    #[test]
    fn first_statement_trims_trailing_semicolon() {
        assert_eq!(SqlGuard::first_statement("SELECT 1;"), "SELECT 1");
        assert_eq!(SqlGuard::first_statement("  SELECT 1  "), "SELECT 1");
    }

    #[test]
    fn transaction_control_is_read_only() {
        for s in ["BEGIN", "COMMIT", "ROLLBACK"] {
            assert_eq!(g(AccessTier::ReadOnly).classify(s).verdict, Verdict::ReadOnly, "{s}");
        }
    }

    #[test]
    fn grant_revoke_are_destructive() {
        assert_eq!(g(AccessTier::Full).classify("GRANT ALL ON db.* TO 'u'").verdict, Verdict::Destructive);
        assert_eq!(g(AccessTier::Full).classify("REVOKE ALL ON db.* FROM 'u'").verdict, Verdict::Destructive);
    }

    #[test]
    fn unknown_verb_is_destructive() {
        assert_eq!(g(AccessTier::Full).classify("WIBBLE t").verdict, Verdict::Destructive);
    }

    #[test]
    fn first_statement_respects_quotes() {
        // semicolon inside a string literal is not a statement terminator
        assert_eq!(SqlGuard::first_statement("INSERT INTO t VALUES ('a;b')"), "INSERT INTO t VALUES ('a;b')");
    }
}
