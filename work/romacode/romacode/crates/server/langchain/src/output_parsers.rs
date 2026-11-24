//! Output parsers — the Rust mirror of `langchain_core.output_parsers`.
//!
//! Parsers are plain `Runnable`s taking the raw model text, so they compose
//! with `.pipe(...)` exactly like any other step.

use async_trait::async_trait;
use serde_json::Value;

use crate::errors::{LangchainError, Result};
use crate::runnables::Runnable;

/// Returns the model text untouched — `StrOutputParser`.
#[derive(Debug, Default, Clone, Copy)]
pub struct StrOutputParser;

#[async_trait]
impl Runnable<String, String> for StrOutputParser {
    async fn invoke(&self, input: String) -> Result<String> {
        Ok(input)
    }
}

/// Parses the first JSON object/array found in the text — `JsonOutputParser`.
///
/// Tolerates markdown fences and surrounding prose: if the whole string is not
/// valid JSON, the first balanced `{...}` / `[...]` block is extracted.
#[derive(Debug, Default, Clone, Copy)]
pub struct JsonOutputParser;

#[async_trait]
impl Runnable<String, Value> for JsonOutputParser {
    async fn invoke(&self, input: String) -> Result<Value> {
        let trimmed = input.trim();
        // Strip markdown code fences like ```json ... ```
        let cleaned = if trimmed.starts_with("```") {
            let inner = trimmed.trim_start_matches("```json").trim_start_matches("```");
            inner.trim_end_matches("```").trim()
        } else {
            trimmed
        };

        if let Ok(v) = serde_json::from_str::<Value>(cleaned) {
            return Ok(v);
        }

        // Fall back: extract the first balanced JSON block.
        for (open, close) in [('{', '}'), ('[', ']')] {
            if let Some(v) = extract_balanced(cleaned, open, close) {
                if let Ok(v) = serde_json::from_str::<Value>(&v) {
                    return Ok(v);
                }
            }
        }
        Err(LangchainError::LlmParse(format!("no valid JSON found in: {}", truncate(&input, 120))))
    }
}

fn extract_balanced(s: &str, open: char, close: char) -> Option<String> {
    let bytes: Vec<char> = s.chars().collect();
    for (i, &c) in bytes.iter().enumerate() {
        if c == open {
            let mut depth = 0usize;
            let mut in_string = false;
            let mut escape = false;
            for j in i..bytes.len() {
                let c = bytes[j];
                if escape {
                    escape = false;
                } else if in_string {
                    if c == '\\' {
                        escape = true;
                    } else if c == '"' {
                        in_string = false;
                    }
                } else if c == '"' {
                    in_string = true;
                } else if c == open {
                    depth += 1;
                } else if c == close {
                    depth -= 1;
                    if depth == 0 {
                        return Some(bytes[i..=j].iter().collect());
                    }
                }
            }
        }
    }
    None
}

fn truncate(s: &str, n: usize) -> String {
    if s.chars().count() <= n {
        s.to_string()
    } else {
        format!("{}…", s.chars().take(n).collect::<String>())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn str_parser_identity() {
        let p = StrOutputParser;
        assert_eq!(p.invoke("hi".into()).await.unwrap(), "hi");
    }

    #[tokio::test]
    async fn json_parser_plain() {
        let p = JsonOutputParser;
        assert_eq!(p.invoke(r#"{"a": 1}"#.into()).await.unwrap(), serde_json::json!({"a": 1}));
    }

    #[tokio::test]
    async fn json_parser_fenced_and_noisy() {
        let p = JsonOutputParser;
        let text = "Here you go:\n```json\n{\"name\": \"rust\", \"n\": 2}\n```\nHope that helps";
        assert_eq!(
            p.invoke(text.into()).await.unwrap(),
            serde_json::json!({"name": "rust", "n": 2})
        );
    }

    #[tokio::test]
    async fn json_parser_embedded_object() {
        let p = JsonOutputParser;
        let text = r#"prefix {"ok": true, "s": "a\"b}c"} suffix"#;
        assert_eq!(
            p.invoke(text.into()).await.unwrap(),
            serde_json::json!({"ok": true, "s": "a\"b}c"})
        );
    }

    #[tokio::test]
    async fn json_parser_failure() {
        let p = JsonOutputParser;
        assert!(p.invoke("no json here".into()).await.is_err());
    }
}
