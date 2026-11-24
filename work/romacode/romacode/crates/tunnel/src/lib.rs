//! Pure-Rust, std-only reimplementation of the chisel TCP tunnel.
//!
//! Architecture (mirrors jpillora/chisel, but with WebSocket instead of SSH
//! as the multiplexed transport, since SSH-from-scratch would be unsafe
//! and not wire-compatible anyway):
//!
//! ```text
//!   Client side                                 Server side
//!   ----------                                  ----------
//!   chisel client <server> <remote>...          chisel server --port 8080 --auth token
//!
//!   TCP listener  --.        .-->  WS TCP conn  --.        .-->  Dial target
//!   (forward)       \       /      (HTTP upgrade  \       /      (remote-host:remote-port)
//!                    \     /       + token auth)  \     /
//!                     \   /                        \   /
//!                      mux                          mux
//!                  (frame = msg)
//!
//!   Reverse: server listens, dials-back through client.
//! ```
//!
//! Wire protocol (binary frames inside WS):
//!   - byte 0:        message type
//!   - bytes 1..9:    session id (u64 big-endian)
//!   - rest:          payload (length depends on type)
//!
//! Message types:
//!   0x01 Open   payload = u8 addr_type + u16 port (BE) + variable addr
//!                addr_type: 0=ipv4(4B), 1=ipv6(16B), 2=hostname(len-prefixed u8)
//!   0x02 Data   payload = raw bytes
//!   0x03 Close  payload = empty
//!   0x04 Ping   payload = arbitrary (echoed back)
//!   0x05 Pong   payload = arbitrary
//!   0x06 Hello payload = JSON-ish (we use a minimal hand-rolled text format)
//!                       {"auth":"<token>","version":"0.1","reverse":bool}
//!
//! The transport is chisel-shaped (same remote spec, same forward/reverse
//! model, same single-binary client+server design) but NOT wire-compatible
//! with the original Go chisel, which uses crypto/ssh.

#![allow(clippy::needless_range_loop)]

pub mod base64;
pub mod hex;
pub mod http;
pub mod log;
pub mod mux;
pub mod remote;
pub mod sha1;
pub mod ws;

pub mod client;
pub mod server;

/// DB-transport wrapper: a `Tunnel` runs chisel in-process on a background
/// thread and exposes a local `host:port` the db tools connect to.
pub mod transport;
pub use transport::Tunnel;

pub const VERSION: &str = "0.1.0";
