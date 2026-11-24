//! Thin tool wrapper over the documents engine. Definitions and execution
//! live in the engine bridge (`crate::documents_bridge`); this module exists
//! so every agent tool has a uniform home under `tools/`.
pub use crate::documents_bridge::definitions;
pub use crate::documents_bridge::execute;
pub use crate::documents_bridge::owns;
