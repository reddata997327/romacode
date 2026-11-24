//! # rc-sources — the 8 universal tool-source mechanisms (the "unlimited" part)
//!
//! A tool source is anything that implements [`ToolSource`] — discover and
//! produce ready-to-register [`ToolArc`] handles. Any tool ever conceived fits
//! into at least one of these eight mechanisms, which is what makes the
//! framework "unlimited":
//!
//! | # | source | module | what it wraps |
//! |---|--------|--------|---------------|
//! | 1 | builtin | [`builtin`] | the shipped 38-tool catalog |
//! | 2 | declarative | [`declarative`] | a YAML/JSON spec → shell tool (no code) |
//! | 3 | llm_gen | [`llm_gen`] | natural language → LLM emits a spec → tool |
//! | 4 | openapi | [`openapi`] | an OpenAPI 3 spec → N HTTP tools |
//! | 5 | rest | [`rest`] | one HTTP endpoint → one tool |
//! | 6 | cli | [`cli`] | a binary + arg template → one tool |
//! | 7 | mcp | [`mcp`] | an MCP `tools/list` manifest → tools via a bridge |
//! | 8 | marketplace | [`marketplace`] | a manifest fetched from a URL/file → tools |
//!
//! Sources feed a [`SourceRegistry`], which the lifecycle manager walks to
//! discover and load every tool. Adding a ninth mechanism is one `impl
//! ToolSource` away.

pub mod builtin;
pub mod cli;
pub mod declarative;
pub mod llm_gen;
pub mod marketplace;
pub mod mcp;
pub mod openapi;
pub mod rest;
pub mod source;
pub mod tools;

pub use source::{SourceError, SourceRegistry, ToolSource};
pub use tools::{shell_quote, HttpTool, ShellTool};

/// Convenience: build a [`SourceRegistry`] pre-loaded with just the builtin
/// source. The CLI and lifecycle manager add more sources on demand.
pub fn builtin_only() -> SourceRegistry {
    let mut sr = SourceRegistry::new();
    sr.add(builtin::BuiltinSource);
    sr
}
