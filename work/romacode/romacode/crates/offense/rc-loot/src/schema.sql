
PRAGMA journal_mode = WAL;
PRAGMA foreign_keys = ON;

CREATE TABLE IF NOT EXISTS engagement (
  id INTEGER PRIMARY KEY, name TEXT NOT NULL UNIQUE, scope TEXT NOT NULL,
  started_at INTEGER NOT NULL, dry_run INTEGER NOT NULL DEFAULT 1);

CREATE TABLE IF NOT EXISTS host (
  id INTEGER PRIMARY KEY, engagement INTEGER NOT NULL REFERENCES engagement(id),
  addr TEXT NOT NULL, kind TEXT NOT NULL, os_guess TEXT,
  first_seen INTEGER NOT NULL, status TEXT NOT NULL DEFAULT 'up',
  UNIQUE (engagement, addr));

CREATE TABLE IF NOT EXISTS service (
  id INTEGER PRIMARY KEY, host INTEGER NOT NULL REFERENCES host(id),
  port INTEGER NOT NULL, proto TEXT NOT NULL, banner TEXT, product TEXT,
  version TEXT, tls_subject TEXT, fingerprint TEXT, evidence_sha256 TEXT,
  UNIQUE (host, port, proto));

CREATE TABLE IF NOT EXISTS vuln (
  id INTEGER PRIMARY KEY, service INTEGER NOT NULL REFERENCES service(id),
  cve TEXT, title TEXT NOT NULL, cvss REAL, confidence REAL NOT NULL,
  exploitable INTEGER NOT NULL DEFAULT 0, evidence_sha256 TEXT);

CREATE TABLE IF NOT EXISTS cred (
  id INTEGER PRIMARY KEY, engagement INTEGER NOT NULL REFERENCES engagement(id),
  host INTEGER REFERENCES host(id), kind TEXT NOT NULL, username TEXT,
  secret TEXT NOT NULL, source TEXT NOT NULL,
  status TEXT NOT NULL DEFAULT 'untested', tested_at INTEGER,
  evidence_sha256 TEXT);

CREATE TABLE IF NOT EXISTS session (
  id INTEGER PRIMARY KEY, cred INTEGER REFERENCES cred(id),
  host INTEGER NOT NULL REFERENCES host(id), proto TEXT NOT NULL,
  established INTEGER NOT NULL, last_seen INTEGER NOT NULL,
  burned INTEGER NOT NULL DEFAULT 0);

CREATE TABLE IF NOT EXISTS card (
  id INTEGER PRIMARY KEY, engagement INTEGER NOT NULL REFERENCES engagement(id),
  pan TEXT NOT NULL, bin TEXT NOT NULL, brand TEXT, issuer TEXT, country TEXT,
  exp_mm INTEGER NOT NULL, exp_yy INTEGER NOT NULL, cvv TEXT, holder TEXT,
  avs_street TEXT, avs_zip TEXT, status TEXT NOT NULL DEFAULT 'untested',
  gateway TEXT, decline_code TEXT, checked_at INTEGER,
  UNIQUE (engagement, pan));

CREATE TABLE IF NOT EXISTS txn (
  id INTEGER PRIMARY KEY, engagement INTEGER NOT NULL REFERENCES engagement(id),
  card INTEGER REFERENCES card(id), gateway TEXT NOT NULL,
  amount_cents INTEGER NOT NULL, currency TEXT NOT NULL, result TEXT NOT NULL,
  raw TEXT, at INTEGER NOT NULL);

CREATE TABLE IF NOT EXISTS wallet (
  id INTEGER PRIMARY KEY, engagement INTEGER NOT NULL REFERENCES engagement(id),
  chain TEXT NOT NULL, addr TEXT NOT NULL, balance_atom TEXT,
  swept INTEGER NOT NULL DEFAULT 0, UNIQUE (engagement, chain, addr));

CREATE TABLE IF NOT EXISTS artifact (
  id INTEGER PRIMARY KEY, engagement INTEGER NOT NULL REFERENCES engagement(id),
  kind TEXT NOT NULL, path TEXT NOT NULL, sha256 TEXT NOT NULL,
  bytes INTEGER NOT NULL, at INTEGER NOT NULL);

CREATE TABLE IF NOT EXISTS chain (
  id INTEGER PRIMARY KEY, engagement INTEGER NOT NULL REFERENCES engagement(id),
  name TEXT NOT NULL, plan TEXT NOT NULL, state TEXT NOT NULL DEFAULT 'draft',
  cursor TEXT, updated_at INTEGER NOT NULL);

CREATE TABLE IF NOT EXISTS action_log (
  id INTEGER PRIMARY KEY, engagement INTEGER NOT NULL REFERENCES engagement(id),
  tool TEXT NOT NULL, args TEXT NOT NULL, effect TEXT NOT NULL,
  result TEXT NOT NULL, started INTEGER NOT NULL, ended INTEGER);

CREATE INDEX IF NOT EXISTS ix_service_host ON service(host);
CREATE INDEX IF NOT EXISTS ix_vuln_service ON vuln(service);
CREATE INDEX IF NOT EXISTS ix_cred_status  ON cred(engagement, status);
CREATE INDEX IF NOT EXISTS ix_card_status  ON card(engagement, status);
