//! Chat message primitives — the Rust equivalent of
//! `langchain_core.messages` (`SystemMessage`, `HumanMessage`, `AIMessage`, `ToolMessage`).

use serde::{Deserialize, Serialize};
use serde_json::Value;

/// A tool invocation requested by the model (`AIMessage.tool_calls`).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ToolCall {
    /// OpenAI style call id (e.g. `call_abc123`).
    pub id: String,
    /// Name of the tool the model wants to run.
    pub name: String,
    /// Arguments as a JSON object.
    pub args: Value,
}

/// OpenAI `tools` parameter entry describing a callable function.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ToolSpec {
    pub name: String,
    pub description: String,
    /// JSON Schema for the tool arguments.
    pub parameters: Value,
}

impl ToolSpec {
    /// Build the full OpenAI request body fragment:
    /// `{"type": "function", "function": {"name": ..., "description": ..., "parameters": ...}}`.
    pub fn to_openai_json(&self) -> Value {
        serde_json::json!({
            "type": "function",
            "function": {
                "name": self.name,
                "description": self.description,
                "parameters": self.parameters,
            }
        })
    }
}

/// A single message in a chat conversation.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "role", rename_all = "lowercase")]
pub enum Message {
    /// `system` message — sets behaviour.
    System {
        content: String,
    },
    /// `user` message.
    Human {
        content: String,
    },
    /// `assistant` message; may carry tool invocations.
    AI {
        /// Text content, `None` when the model only requested tools.
        content: Option<String>,
        /// Tool calls requested by the model (empty for plain answers).
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        tool_calls: Vec<ToolCall>,
    },
    /// `tool` message — the result of a tool call, addressed by `tool_call_id`.
    Tool {
        tool_call_id: String,
        name: String,
        content: String,
    },
}

impl Message {
    /// Shorthand constructor for a system message.
    pub fn system(content: impl Into<String>) -> Self {
        Message::System {
            content: content.into(),
        }
    }

    /// Shorthand constructor for a human message.
    pub fn human(content: impl Into<String>) -> Self {
        Message::Human {
            content: content.into(),
        }
    }

    /// Shorthand constructor for a plain (no tools) AI message.
    pub fn ai(content: impl Into<String>) -> Self {
        Message::AI {
            content: Some(content.into()),
            tool_calls: Vec::new(),
        }
    }

    /// Shorthand constructor for a tool result message.
    pub fn tool(tool_call_id: impl Into<String>, name: impl Into<String>, content: impl Into<String>) -> Self {
        Message::Tool {
            tool_call_id: tool_call_id.into(),
            name: name.into(),
            content: content.into(),
        }
    }

    /// The textual content of the message, ignoring tool calls.
    pub fn text(&self) -> &str {
        match self {
            Message::System { content } | Message::Human { content } | Message::Tool { content, .. } => content,
            Message::AI { content, .. } => content.as_deref().unwrap_or(""),
        }
    }

    /// Serialise to the exact JSON shape the OpenAI chat-completions API expects.
    pub fn to_openai_json(&self) -> Value {
        match self {
            Message::System { content } => serde_json::json!({ "role": "system", "content": content }),
            Message::Human { content } => serde_json::json!({ "role": "user", "content": content }),
            Message::AI { content, tool_calls } => {
                let mut obj = serde_json::json!({ "role": "assistant", "content": content });
                if !tool_calls.is_empty() {
                    obj["tool_calls"] = Value::Array(
                        tool_calls
                            .iter()
                            .map(|tc| {
                                serde_json::json!({
                                    "id": tc.id,
                                    "type": "function",
                                    "function": {
                                        "name": tc.name,
                                        "arguments": tc.args.to_string(),
                                    }
                                })
                            })
                            .collect(),
                    );
                }
                obj
            }
            Message::Tool { tool_call_id, name, content } => serde_json::json!({
                "role": "tool",
                "tool_call_id": tool_call_id,
                "name": name,
                "content": content,
            }),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn openai_serialisation_roundtrip() {
        let ai = Message::AI {
            content: None,
            tool_calls: vec![ToolCall {
                id: "call_1".into(),
                name: "calculator".into(),
                args: serde_json::json!({"expression": "2+2"}),
            }],
        };
        let j = ai.to_openai_json();
        assert_eq!(j["role"], "assistant");
        assert_eq!(j["tool_calls"][0]["function"]["name"], "calculator");
        assert_eq!(j["tool_calls"][0]["function"]["arguments"], r#"{"expression":"2+2"}"#);
    }

    #[test]
    fn tool_message_shape() {
        let m = Message::tool("call_1", "calculator", "4");
        let j = m.to_openai_json();
        assert_eq!(j["role"], "tool");
        assert_eq!(j["tool_call_id"], "call_1");
    }

    #[test]
    fn text_helper() {
        assert_eq!(Message::human("hi").text(), "hi");
        let ai = Message::AI { content: None, tool_calls: vec![] };
        assert_eq!(ai.text(), "");
    }
}
