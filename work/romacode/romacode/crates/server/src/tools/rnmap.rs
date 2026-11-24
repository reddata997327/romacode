//! Thin tool wrapper over the rnmap engine. Schema and execution live in the
//! engine (`crate::rnmap`); this module exists so every agent tool has a
//! uniform home under `tools/`.

pub use crate::rnmap::definition;
pub use crate::rnmap::execute;
