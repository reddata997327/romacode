//! `contrib` — Django-style contrib packages.
//!
//! * [`ai`] — OpenAI-compatible LLM client + tool-calling agent loop
//!   (feature-gated: enable `features = ["ai"]`).

#[cfg(feature = "ai")]
pub mod ai;

#[cfg(feature = "ai")]
pub use ai::{Agent, AiClient, AiError, ChatMessage, Role, Tool, ToolCall};
