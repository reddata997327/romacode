//! # rc-lifecycle — L3: the full tool lifecycle
//!
//! The [`Lifecycle`] manager ties the other layers together:
//!
//! 1. **discover** — walk every registered [`ToolSource`](rc_sources::ToolSource),
//!    collect the tools each produces.
//! 2. **register** — insert them into the [`Registry`].
//! 3. **validate** — check every tool's schema for duplicates / empty names.
//! 4. **load** — sources load eagerly (tools are ready-to-register on
//!    `discover`).
//! 5. **execute** — run a tool by name through the [`Executor`], timing the
//!    call and recording metrics.
//! 6. **monitor** — per-tool call/success/failure counts + latency, via
//!    [`MetricsStore`].
//! 7. **version** — a version report of every registered tool.
//! 8. **retire** — remove a tool from the registry.
//!
//! [`Lifecycle::permissive`] is a one-liner for examples/tests.

pub mod lifecycle;
pub mod monitor;

pub use lifecycle::Lifecycle;
pub use monitor::{MetricsStore, ToolMetrics};
