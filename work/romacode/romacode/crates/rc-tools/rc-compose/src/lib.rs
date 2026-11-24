//! # rc-compose — L4 composition: new tools from old tools
//!
//! Five composition primitives, each producing a registered [`Tool`]:
//!
//! | primitive | what it does |
//! |-----------|--------------|
//! | [`pipeline`] | chain tools — each step's output feeds the next |
//! | [`parallel`] | fan out N tools concurrently, join as an array |
//! | [`branch`] | conditionally route to one of N tools (first match wins) |
//! | [`iter`] | loop a tool, feeding output back, until a condition or max |
//! | [`dag`] | run a dependency graph of tool calls in topological order |
//!
//! Because every composition is itself a `Tool`, compositions **nest** — a
//! pipeline step can be a branch, a DAG node can be a parallel, and so on.
//! That nesting is what makes the framework *multiplicatively* unlimited:
//! finite primitives compose into an unbounded space of tools.
//!
//! Template references (`{{prev.x}}`, `{{input.y}}`, `{{nodes.name.z}}`) are
//! resolved by [`templating`], shared by all five primitives.

pub mod branch;
pub mod dag;
pub mod iter;
pub mod parallel;
pub mod pipeline;
pub mod templating;

pub use branch::{BranchArm, BranchTool};
pub use dag::{DagNode, DagTool};
pub use iter::{LoopTool, Until};
pub use parallel::{ParallelStep, ParallelTool};
pub use pipeline::{pipe, PipelineStep, PipelineTool};
pub use templating::{lookup, resolve, resolve_ctx};
