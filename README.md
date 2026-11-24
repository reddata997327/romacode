# RomaCode — AI coding agent, rebuilt in pure Rust

This repository is the full-source state of the RomaCode project: a pure-Rust
rebuild of the agent toolchain plus the dsd-tools live API-tracing stack and
the evidence of the live tests that validated them.

## Layout

| Path | What it is |
| --- | --- |
| `work/romacode/` | RomaCode CLI — the pure-Rust agent workspace (crates, agent, agent-skills, assets, data, docs). Native built-in tools follow the same integration pattern as `bash` / `webfetch`. |
| `work/dsd-live/` | dsd-tools: standalone custom Rust tap that traces API calls live (entry/exit per hop) — `Cargo.toml` + `src/`, no frameworks. |
| `work/tools/`, `work/services/`, `work/bridge/`, `work/skillref/`, `work/samples/` | Supporting tools, services, bridge components, skill references and sample documents. |
| `scripts/` | Test and crypto scripts (`dsd-live/`), document-tool generation scripts, and utilities. |
| `skills/` | Skill packages (SKILL.md) covering the document tools and related capabilities. |
| `download/` | Deliverables and test evidence, including `dsd-live-evidence/EVIDENCE_LOG.txt` — the live pkr98.com login test where the database answered **YES** (code 1 + session/JWT) and **NO** (code 1011, wrong password). |

## dsd-tools highlights

- Live visibility of an API call's travel path: entry/exit at the edge, the
  application server (BFF) and the database behind it.
- Custom Rust MITM tap (`work/dsd-live`) with verbatim chunked-relay framing;
  rc-dsd test suite 17/17 passing.
- Reversed the target's app-layer crypto (token-derived AES key) to prove the
  database receives the login and returns yes/no; a local PostgreSQL 17
  engine demo reproduces the decision query end-to-end.

## Notes

- Test credentials, session keys and the static site token are redacted; raw
  captures stay local (see `.gitignore`).
- Build artifacts (`target/`), toolchains and the local PostgreSQL install are
  not tracked.
