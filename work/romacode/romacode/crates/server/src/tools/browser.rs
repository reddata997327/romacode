//! Thin tool wrapper over the browser engine. Schemas and execution live in
//! the engine (`crate::browser`); this module exists so every agent tool has
//! a uniform home under `tools/`.

pub use crate::browser::definition;
pub use crate::browser::execute;
