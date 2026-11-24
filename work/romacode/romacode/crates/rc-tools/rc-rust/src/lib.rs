//! # rc-rust — the Rust language pack + framework entry point
//!
//! `rc-rust` does two things:
//!
//! 1. **A Rust language pack** — eight `rust.*` developer-workflow tools
//!    ([`tools`]): `cargo check / build / test / fmt / clippy / doc`,
//!    `rust.toolchain`, and `rust.fmt_file`. Each is a first-class
//!    [`rc_core::Tool`] behind the framework's sandbox + permission gate, and
//!    each returns the same `{exit_code, stdout, stderr}` shape the `cli` and
//!    `declarative` sources produce — so a `rust.*` step composes identically
//!    to any other tool.
//!
//! 2. **A framework entry point** — re-exports the whole rc-* surface (core,
//!    builtin, sources, compose, lifecycle) in one place, so `use rc_rust::*`
//!    gives a caller the full builder *plus* the Rust tools. The
//!    [`RustSource`] adapter makes the eight tools discoverable through the
//!    standard [`ToolSource`](rc_sources::ToolSource) contract.
//!
//! ## Quick start
//!
//! ```no_run
//! # use rc_rust as rc;
//! # async fn run() -> Result<(), Box<dyn std::error::Error>> {
//! let mut life = rc::Lifecycle::permissive();
//! life.add_source(rc::RustSource);                  // the 8 rust.* tools
//! life.add_source(rc::BuiltinSource);               // the 38-tool catalog
//! let n = life.discover().await?;                   // registers 8 + 38 tools
//! assert_eq!(n, rc::COUNT + rc::BUILTIN_COUNT);
//! // run `cargo check` on the current directory (caller-approved):
//! let out = life.execute("rust.cargo_check", &rc::serde_json::json!({}), true).await?;
//! println!("exit={} stderr={}", out["exit_code"], out["stderr"]);
//! # Ok(()) }
//! ```
//!
//! Because the `rust.*` tools spawn subprocesses and touch the filesystem,
//! the default permission policy leaves them at `Ask` (read-only classes like
//! `math.*` are `Allow`). Pass `approved = true` once the caller has
//! confirmed.

pub mod source;
pub mod tools;

// ── Framework re-exports: one `use rc_rust::*` gives the whole builder. ──────
pub use rc_core::{
    arc, Catalog, Executor, Manifest, ParamType, Parameter, Permission, PermissionPolicy,
    PermissionRule, Registry, Sandbox, Schema, Tool, ToolArc, ToolContext, ToolError, ToolInfo,
    ToolResult, ToolSourceKind, Validator, VERSION,
};
pub use rc_builtin::{all_tools as builtin_all_tools, CATEGORIES as BUILTIN_CATEGORIES, COUNT as BUILTIN_COUNT};
pub use rc_sources::{
    builtin::BuiltinSource, HttpTool, ShellTool, SourceError, SourceRegistry, ToolSource,
    builtin_only,
};
// Re-export the JSON crates so callers (and doctests) get them from one place.
pub use serde_json;
pub use serde;
pub use rc_compose::{
    BranchArm, BranchTool, DagNode, DagTool, LoopTool, ParallelStep, ParallelTool, PipelineStep,
    PipelineTool, pipe,
};
pub use rc_lifecycle::{Lifecycle, MetricsStore, ToolMetrics};

// ── This crate's own surface. ────────────────────────────────────────────────
pub use source::RustSource;
pub use tools::{all, CATEGORY, COUNT};

/// Every tool this crate contributes — the eight `rust.*` tools — ready to
/// register in a [`Registry`].
pub fn all_tools() -> Vec<ToolArc> {
    tools::all()
}

/// A convenience: build a [`Lifecycle`](rc_lifecycle::Lifecycle) pre-loaded
/// with the Rust language pack **and** the 38-tool builtin catalog, with a
/// permissive policy. Add more sources (declarative, openapi, …) on the
/// returned lifecycle before calling `discover`.
pub fn rust_lifecycle() -> Lifecycle {
    let mut life = Lifecycle::permissive();
    life.add_source(RustSource);
    life.add_source(rc_sources::builtin::BuiltinSource);
    life
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn all_tools_exposes_eight() {
        assert_eq!(all_tools().len(), COUNT);
    }

    #[tokio::test]
    async fn rust_lifecycle_discovers_rust_and_builtin() {
        let life = rust_lifecycle();
        let n = life.discover().await.expect("discover");
        // 8 rust tools + the 38 builtin tools.
        assert_eq!(n, COUNT + BUILTIN_COUNT);
        assert!(life.registry.contains("rust.cargo_check"));
        assert!(life.registry.contains("math.add"));
    }

    #[test]
    fn reexports_are_reachable() {
        // sanity: the re-exported symbols compile into the public surface.
        let _policy = PermissionPolicy::default();
        let _sandbox = Sandbox::default();
        let _reg = Registry::new();
        let _: Schema = Schema::new().param(Parameter::required("x", ParamType::Integer));
    }
}
