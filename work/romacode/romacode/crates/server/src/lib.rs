//! RomaCode server — the brain.
//!
//! Everything that thinks lives here: the agent loop (native SSE loop plus
//! the vendored **langchain-rust** engine), sessions and their history, the
//! security gate every tool call must pass, and the real-device / coding /
//! web tools. Clients (the TUI screen, scripts, tests) talk to it over HTTP
//! + WebSocket using the `romacode-shared` protocol.

pub mod agent;
pub mod android;
pub mod api;
pub mod browser;
pub mod cli_tools;
pub mod config;
pub mod db_cli;
pub mod documents_bridge;
pub mod github;
pub mod github_engineering;
pub mod langchain_bridge;
pub mod persona;
pub mod providers;
pub mod provider_engine;
pub mod providers_cli;
pub mod remix;
pub mod rnmap;
pub mod rustango_bridge;
pub mod security;
pub mod session;
pub mod session_store;
pub mod skills;
pub mod tools;
pub mod tunnel_cli;
pub mod webfetch;

pub use security::Security;

/// Protocol version this server speaks (must match `romacode-shared`).
pub const PROTOCOL_VERSION: u32 = romacode_shared::protocol::Welcome::PROTOCOL_VERSION;
