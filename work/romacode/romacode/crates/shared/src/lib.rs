//! RomaCode shared contract.
//!
//! This crate is the single source of truth for everything that crosses the
//! wire between the two halves of RomaCode:
//!
//! * the **server** (`crates/server`) — the brain: agent loop, sessions,
//!   tools, security gate, engines;
//! * the **client TUI** (`crates/client-tui`) — the screen: keyboard, mouse
//!   and pixels.
//!
//! `protocol` defines the request/response messages; `events` defines the
//! stream of things the server pushes to attached clients. Both sides depend
//! only on this crate, so the wire format can never drift apart.

pub mod offense;
pub mod catalog;
pub mod events;
pub mod modeldata;
pub mod protocol;
pub mod tui_settings;

pub use events::Event;
pub use protocol::{ClientEnvelope, ClientRequest, Reply, ServerEnvelope, Welcome};
