//! Thin tool wrapper over the webfetch engine. Schemas and execution live in
//! the engine (`crate::webfetch`); this module exists so every agent tool has
//! a uniform home under `tools/`.

pub use crate::webfetch::definition;
pub use crate::webfetch::execute;
