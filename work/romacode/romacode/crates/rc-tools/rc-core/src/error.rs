//! Errors for the RomaCode tools builder core.

use crate::schema::ParamType;
use thiserror::Error;

/// Every tool execution failure flows through `ToolError`.
#[derive(Debug, Error)]
pub enum ToolError {
    #[error("tool not found: {0}")]
    NotFound(String),

    #[error("validation error: {0}")]
    Validation(String),

    #[error("parameter `{name}` (expected {expected:?}) — {detail}")]
    BadParam {
        name: String,
        expected: ParamType,
        detail: String,
    },

    #[error("permission denied for tool `{tool}`: {reason}")]
    PermissionDenied { tool: String, reason: String },

    #[error("sandbox violation: {0}")]
    Sandbox(String),

    #[error("execution timed out after {timeout_ms} ms")]
    Timeout { timeout_ms: u64 },

    #[error("output exceeded sandbox limit ({limit} bytes)")]
    OutputTooLarge { limit: usize },

    #[error("tool `{tool}` returned an error: {inner}")]
    ToolFailed { tool: String, inner: String },

    #[error("i/o error: {0}")]
    Io(#[from] std::io::Error),

    #[error("json error: {0}")]
    Json(#[from] serde_json::Error),

    #[error("{0}")]
    Other(String),
}

/// Shorthand result alias.
pub type ToolResult<T> = Result<T, ToolError>;

impl From<ToolError> for String {
    fn from(e: ToolError) -> String {
        e.to_string()
    }
}
