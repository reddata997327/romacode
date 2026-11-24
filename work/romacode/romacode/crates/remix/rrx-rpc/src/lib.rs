//! rrx-rpc: a real JSON-RPC client for live networks + a real local JSON-RPC node
//! serving our chain over HTTP (wallets/tools can connect to it).

pub mod client;
pub mod ws;
pub mod endpoints;
pub mod server;

pub use client::Provider;
pub use endpoints::{auto_mainnet, probe, probe_all, resolve_endpoint, EndpointStatus, MultiProvider, MAINNET_ENDPOINTS};
