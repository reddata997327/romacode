//! rrx-tools: every Remix IDE tool rebuilt headless in pure Rust.
//!
//! Mapping (Remix plugin -> rrx module):
//! - File Explorer  -> file_explorer
//! - Search         -> search
//! - Solidity Compiler -> compile (real solc)
//! - Static Analysis   -> analyzer
//! - Deploy & Run      -> deploy (local chain AND real networks)
//! - Debugger          -> debugger (local) + debugtx (real mainnet tx replay)
//! - Solidity Unit Testing -> unittest (remix-tests conventions)
//! - Gas Fee (gas analytics) -> gasfee
//! - Contract Flattener -> flatten
//! - UML Generator      -> uml
//! - Doc Generator      -> docgen
//! - Script Runner      -> script
//! - Template Explorer  -> templates
//! - Git                -> minigit
//! - Contract Verification -> verify + sources (fetch verified code)
//! - Plugin Manager     -> plugins
//! - Mainchain links    -> netstatus + send + ens + token + logs + proofs + remotehost

pub mod aa;
pub mod analyzer;
pub mod blob;
pub mod builder;
pub mod chains;
pub mod compile;
pub mod coverage;
pub mod debugger;
pub mod debugtx;
pub mod deploy;
pub mod docgen;
pub mod ens;
pub mod fake_variants;
pub mod fakeasset;
pub mod file_explorer;
pub mod flatten;
pub mod fork;
pub mod fuzz;
pub mod gasfee;
pub mod indexer;
pub mod logs;
pub mod minigit;
pub mod netstatus;
pub mod plugins;
pub mod proofs;
pub mod remotehost;
pub mod script;
pub mod search;
pub mod security;
pub mod send;
pub mod sources;
pub mod templates;
pub mod token;
pub mod unittest;
pub mod uml;
pub mod utils;
pub mod verify;
pub mod wildtoken;
pub mod workspace;

pub const VERSION: &str = env!("CARGO_PKG_VERSION");
