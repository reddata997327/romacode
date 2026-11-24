//! Unified error type for the whole crate, mirroring `LangChainException` in Python.

use thiserror::Error;

/// The error type returned by every fallible operation in `langchain-rust`.
#[derive(Debug, Error)]
pub enum LangchainError {
    /// A `{placeholder}` in a prompt template has no matching variable.
    #[error("missing template variable: {0}")]
    MissingVariable(String),

    /// The template contains unbalanced braces.
    #[error("invalid template: {0}")]
    InvalidTemplate(String),

    /// The configured model or provider rejected the request (HTTP or transport level).
    #[error("llm request failed: {0}")]
    LlmRequest(String),

    /// The provider answered with something this crate could not parse.
    #[error("failed to parse llm response: {0}")]
    LlmParse(String),

    /// A tool invocation failed.
    #[error("tool `{0}` failed: {1}")]
    ToolExecution(String, String),

    /// The agent executor exhausted `max_iterations` without producing a final answer.
    #[error("agent stopped after {0} iterations without a final answer")]
    AgentLoopLimit(usize),

    /// A tool requested by the model is not registered on the executor.
    #[error("unknown tool: {0}")]
    UnknownTool(String),

    /// Generic invalid input (e.g. malformed calculator expression).
    #[error("invalid input: {0}")]
    InvalidInput(String),

    /// The embedding dimension does not match the store dimension.
    #[error("dimension mismatch: expected {expected}, got {got}")]
    DimensionMismatch { expected: usize, got: usize },

    /// Anything else.
    #[error("{0}")]
    Other(String),
}

/// Convenience alias used across the crate.
pub type Result<T> = std::result::Result<T, LangchainError>;

impl From<serde_json::Error> for LangchainError {
    fn from(e: serde_json::Error) -> Self {
        LangchainError::LlmParse(e.to_string())
    }
}

impl From<reqwest::Error> for LangchainError {
    fn from(e: reqwest::Error) -> Self {
        LangchainError::LlmRequest(e.to_string())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn display_is_human_readable() {
        let e = LangchainError::MissingVariable("topic".into());
        assert_eq!(e.to_string(), "missing template variable: topic");
    }
}
