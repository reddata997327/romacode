//! # rc-core — the foundation of the RomaCode tools builder
//!
//! The single contract every tool implements ([`Tool`]), the spine they are
//! stored in ([`Registry`]), the gate they run through ([`Executor`] +
//! [`PermissionPolicy`] + [`Sandbox`]), and the contracts they publish
//! ([`Schema`] + [`Manifest`]). Every other crate in the workspace builds on
//! this one and nothing else.
//!
//! ## The 8 universal tool-source mechanisms
//!
//! `ToolSourceKind` enumerates them: `builtin`, `declarative`, `llm_gen`,
//! `openapi`, `mcp`, `rest`, `graphql`, `cli`, `compose`, `marketplace`. Once
//! loaded, every tool from every source is just a `Tool` — the source is
//! metadata, not a special case.

pub mod error;
pub mod executor;
pub mod manifest;
pub mod permissions;
pub mod registry;
pub mod sandbox;
pub mod schema;
pub mod tool;
pub mod validator;

// Re-export the surface.
pub use error::{ToolError, ToolResult};
pub use executor::Executor;
pub use manifest::{Catalog, Manifest};
pub use permissions::{Permission, PermissionPolicy, PermissionRule};
pub use registry::{Registry, ToolInfo};
pub use sandbox::Sandbox;
pub use schema::{ParamType, Parameter, Schema};
pub use tool::{arc, Tool, ToolArc, ToolContext, ToolSourceKind};
pub use validator::Validator;

/// The framework version.
pub const VERSION: &str = env!("CARGO_PKG_VERSION");
