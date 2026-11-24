//! `contrib.ai` — LLM/agent support for rustango apps.
//!
//! An OpenAI-compatible chat client (works with OpenAI, Anthropic-style
//! gateways, Ollama, vLLM, LM Studio — anything exposing
//! `POST {base}/chat/completions`) plus an agent loop with Rust tool
//! calling. The HTTP client is `ureq` + rustls(`ring`) — 100% Rust, no C.
//!
//! ```ignore
//! use rustango::contrib::ai::{AiClient, Agent, Tool};
//!
//! let client = AiClient::from_settings();
//! // single completion
//! let reply = client.chat_simple("Summarize Django in one sentence.")?;
//!
//! // agent loop with tools
//! let agent = Agent::new(client)
//!     .system("You are a helpful admin assistant.")
//!     .tool(Tool::new("count_users", "Count users in the DB", |args| {
//!         Ok(serde_json::json!({ "count": 42 }))
//!     }));
//! let answer = agent.run("How many users do we have?")?;
//! ```
//!
//! Enable with `features = ["ai"]` in your Cargo.toml.

use serde::{Deserialize, Serialize};
use std::sync::Arc;

/// Chat message roles.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Role {
    System,
    User,
    Assistant,
    Tool,
}

/// One chat message.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ChatMessage {
    pub role: Role,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub content: Option<String>,
    /// OpenAI-compatible tool call requests (assistant messages).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tool_calls: Option<Vec<ToolCall>>,
    /// Tool results (role = tool).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tool_call_id: Option<String>,
}

impl ChatMessage {
    pub fn system(text: impl Into<String>) -> Self {
        Self { role: Role::System, content: Some(text.into()), tool_calls: None, tool_call_id: None }
    }
    pub fn user(text: impl Into<String>) -> Self {
        Self { role: Role::User, content: Some(text.into()), tool_calls: None, tool_call_id: None }
    }
    pub fn assistant(text: impl Into<String>) -> Self {
        Self { role: Role::Assistant, content: Some(text.into()), tool_calls: None, tool_call_id: None }
    }
    pub fn tool_result(call_id: impl Into<String>, text: impl Into<String>) -> Self {
        Self {
            role: Role::Tool,
            content: Some(text.into()),
            tool_calls: None,
            tool_call_id: Some(call_id.into()),
        }
    }
}

/// A tool call requested by the model.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ToolCall {
    pub id: String,
    #[serde(rename = "type")]
    pub kind: String,
    pub function: ToolCallFn,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ToolCallFn {
    pub name: String,
    /// JSON-encoded arguments object.
    pub arguments: String,
}

/// Tool definition sent to the model (JSON-schema subset).
#[derive(Debug, Clone, Serialize)]
pub struct ToolSpec {
    #[serde(rename = "type")]
    pub kind: String,
    pub function: ToolSpecFn,
}

#[derive(Debug, Clone, Serialize)]
pub struct ToolSpecFn {
    pub name: String,
    pub description: String,
    pub parameters: serde_json::Value,
}

/// The Rust side of a tool call: takes JSON args, returns JSON (or an error).
pub type ToolHandler = dyn Fn(&serde_json::Value) -> Result<serde_json::Value, String> + Send + Sync;

/// A callable tool with its spec — the Rust side of function calling.
pub struct Tool {
    pub spec: ToolSpec,
    handler: Arc<ToolHandler>,
}

impl Tool {
    pub fn new<F>(
        name: impl Into<String>,
        description: impl Into<String>,
        parameters: serde_json::Value,
        handler: F,
    ) -> Self
    where
        F: Fn(&serde_json::Value) -> Result<serde_json::Value, String> + Send + Sync + 'static,
    {
        Tool {
            spec: ToolSpec {
                kind: "function".into(),
                function: ToolSpecFn {
                    name: name.into(),
                    description: description.into(),
                    parameters,
                },
            },
            handler: Arc::new(handler),
        }
    }

    /// Tool without a JSON schema (parameters default to a free object).
    pub fn simple<F>(name: impl Into<String>, description: impl Into<String>, handler: F) -> Self
    where
        F: Fn(&serde_json::Value) -> Result<serde_json::Value, String> + Send + Sync + 'static,
    {
        Self::new(
            name,
            description,
            serde_json::json!({
                "type": "object",
                "properties": {},
                "additionalProperties": true
            }),
            handler,
        )
    }

    pub fn invoke(&self, args: &serde_json::Value) -> Result<serde_json::Value, String> {
        (self.handler)(args)
    }
}

// ---------------------------------------------------------------------------
// Client
// ---------------------------------------------------------------------------

/// OpenAI-compatible chat-completions client (blocking, rustls TLS).
#[derive(Clone)]
pub struct AiClient {
    pub base_url: String,
    pub api_key: String,
    pub model: String,
    pub timeout_secs: u64,
}

impl AiClient {
    /// Build from global settings (`ai_api_url`, `ai_api_key`, `ai_model`).
    pub fn from_settings() -> Self {
        let s = crate::settings::global();
        Self {
            base_url: s.ai_api_url.clone(),
            api_key: s.ai_api_key.clone(),
            model: s.ai_model.clone(),
            timeout_secs: 120,
        }
    }

    pub fn new(base_url: impl Into<String>, api_key: impl Into<String>, model: impl Into<String>) -> Self {
        Self {
            base_url: base_url.into(),
            api_key: api_key.into(),
            model: model.into(),
            timeout_secs: 120,
        }
    }

    fn endpoint(&self) -> String {
        let base = self.base_url.trim_end_matches('/');
        format!("{base}/chat/completions")
    }

    /// Full chat completion: send messages (+ optional tools), get the
    /// assistant's reply message.
    pub fn chat(
        &self,
        messages: &[ChatMessage],
        tools: Option<&[Tool]>,
    ) -> Result<ChatMessage, AiError> {
        #[derive(Serialize)]
        struct Body<'a> {
            model: &'a str,
            messages: &'a [ChatMessage],
            #[serde(skip_serializing_if = "Option::is_none")]
            tools: Option<Vec<ToolSpec>>,
        }

        let body = Body {
            model: &self.model,
            messages,
            tools: tools.map(|ts| ts.iter().map(|t| t.spec.clone()).collect()),
        };

        let agent: ureq::Agent = ureq::Agent::config_builder()
            .timeout_global(Some(std::time::Duration::from_secs(self.timeout_secs)))
            .build()
            .into();
        let mut req = agent
            .post(self.endpoint())
            .header("Authorization", &format!("Bearer {}", self.api_key));
        let json_body = serde_json::to_string(&body)
            .map_err(|e| AiError::Encoding(e.to_string()))?;
        req = req.header("Content-Type", "application/json");
        let mut res = req
            .send(json_body.as_bytes())
            .map_err(|e| AiError::Http(format!("{e}")))?;
        let text = res
            .body_mut()
            .read_to_string()
            .map_err(|e| AiError::Http(format!("read: {e}")))?;

        #[derive(Deserialize)]
        struct Choices {
            message: ChatMessage,
        }
        #[derive(Deserialize)]
        struct ApiResponse {
            choices: Vec<Choices>,
        }
        let parsed: ApiResponse = serde_json::from_str(&text)
            .map_err(|e| AiError::Decoding(format!("{e}; body: {}", truncate(&text, 300))))?;
        parsed
            .choices
            .into_iter()
            .next()
            .map(|c| c.message)
            .ok_or_else(|| AiError::Decoding("no choices in response".into()))
    }

    /// Convenience: one user message, return the text content.
    pub fn chat_simple(&self, prompt: &str) -> Result<String, AiError> {
        let reply = self.chat(&[ChatMessage::user(prompt)], None)?;
        Ok(reply.content.unwrap_or_default())
    }
}

fn truncate(s: &str, n: usize) -> String {
    if s.len() > n {
        // Cut at a CHAR boundary: this runs on provider error bodies, and
        // slicing a multi-byte UTF-8 char in half at byte n panicked the
        // request exactly when a remote server sent non-ASCII text.
        let cut = s
            .char_indices()
            .nth(n)
            .map(|(i, _)| i)
            .unwrap_or(s.len());
        format!("{}...", &s[..cut])
    } else {
        s.to_string()
    }
}

#[derive(Debug, thiserror::Error)]
pub enum AiError {
    #[error("http error: {0}")]
    Http(String),
    #[error("encoding error: {0}")]
    Encoding(String),
    #[error("response decode error: {0}")]
    Decoding(String),
}

// ---------------------------------------------------------------------------
// Agent loop
// ---------------------------------------------------------------------------

/// A tool-calling agent: loop `chat -> tool calls -> results` until the
/// model answers with plain text or `max_steps` is reached.
pub struct Agent {
    client: AiClient,
    system: Option<String>,
    tools: Vec<Tool>,
    max_steps: usize,
}

impl Agent {
    pub fn new(client: AiClient) -> Self {
        Self { client, system: None, tools: Vec::new(), max_steps: 8 }
    }

    pub fn system(mut self, prompt: impl Into<String>) -> Self {
        self.system = Some(prompt.into());
        self
    }

    pub fn tool(mut self, tool: Tool) -> Self {
        self.tools.push(tool);
        self
    }

    pub fn max_steps(mut self, n: usize) -> Self {
        self.max_steps = n.max(1);
        self
    }

    /// Run the agent loop for one user prompt; returns the final text.
    pub fn run(&self, prompt: &str) -> Result<String, AiError> {
        let mut history: Vec<ChatMessage> = Vec::new();
        if let Some(sys) = &self.system {
            history.push(ChatMessage::system(sys.clone()));
        }
        history.push(ChatMessage::user(prompt));

        for _ in 0..self.max_steps {
            let reply = self.client.chat(&history, Some(&self.tools))?;

            if let Some(calls) = &reply.tool_calls {
                if !calls.is_empty() {
                    history.push(ChatMessage {
                        role: Role::Assistant,
                        content: reply.content.clone(),
                        tool_calls: Some(calls.clone()),
                        tool_call_id: None,
                    });
                    for call in calls {
                        let result = match self.tools.iter().find(|t| t.spec.function.name == call.function.name) {
                            Some(tool) => {
                                let parsed: serde_json::Value = serde_json::from_str(&call.function.arguments)
                                    .unwrap_or(serde_json::json!({}));
                                match tool.invoke(&parsed) {
                                    Ok(v) => serde_json::to_string(&v)
                                        .unwrap_or_else(|_| "null".into()),
                                    Err(e) => format!("tool error: {e}"),
                                }
                            }
                            None => format!("unknown tool: {}", call.function.name),
                        };
                        history.push(ChatMessage::tool_result(&call.id, result));
                    }
                    continue;
                }
            }

            return Ok(reply.content.unwrap_or_default());
        }
        Err(AiError::Decoding("agent exceeded max_steps".into()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::{Read, Write};
    use std::net::TcpListener;

    /// Spawn a tiny mock OpenAI-compatible server on a random port:
    /// first call returns a tool call, second call returns plain text.
    fn mock_server(script: Vec<String>) -> (String, std::thread::JoinHandle<()>) {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = listener.local_addr().unwrap();
        let handle = std::thread::spawn(move || {
            for want in script {
                let (mut stream, _) = listener.accept().unwrap();
                // Read until the end of request headers+body (or short read).
                let mut buf = Vec::new();
                let mut chunk = [0u8; 8192];
                loop {
                    match stream.read(&mut chunk) {
                        Ok(0) => break,
                        Ok(n) => {
                            buf.extend_from_slice(&chunk[..n]);
                            // stop once we've seen headers and (if declared)
                            // enough body bytes
                            let text = String::from_utf8_lossy(&buf);
                            if let Some(pos) = text.find("\r\n\r\n") {
                                let declared = text
                                    .to_lowercase()
                                    .split("content-length:")
                                    .nth(1)
                                    .and_then(|v| {
                                        v.trim()
                                            .split(|c: char| c == '\r' || !c.is_ascii_digit())
                                            .next()
                                            .and_then(|x| x.trim().parse::<usize>().ok())
                                    })
                                    .unwrap_or(0);
                                let body_read = buf.len() - (pos + 4);
                                if body_read >= declared || declared == 0 {
                                    break;
                                }
                            }
                        }
                        Err(_) => break,
                    }
                }
                let body = format!(
                    "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                    want.len(),
                    want
                );
                stream.write_all(body.as_bytes()).unwrap();
                let _ = stream.flush();
                let _ = stream.shutdown(std::net::Shutdown::Both);
            }
        });
        (format!("http://{addr}"), handle)
    }

    fn json_response(v: serde_json::Value) -> String {
        serde_json::json!({ "choices": [ { "message": v } ] }).to_string()
    }

    #[test]
    fn chat_simple_against_mock() {
        let (url, handle) = mock_server(vec![json_response(serde_json::json!({
            "role": "assistant",
            "content": "Hello from the mock!"
        }))]);
        let client = AiClient::new(url, "test-key", "mock-model");
        let reply = client.chat_simple("hi").unwrap();
        assert_eq!(reply, "Hello from the mock!");
        handle.join().unwrap();
    }

    #[test]
    fn agent_tool_loop() {
        let tool_call = serde_json::json!({
            "role": "assistant",
            "content": null,
            "tool_calls": [ {
                "id": "call_1",
                "type": "function",
                "function": {
                    "name": "get_count",
                    "arguments": "{\"table\": \"users\"}"
                }
            } ]
        });
        let (url, handle) = mock_server(vec![
            json_response(tool_call),
            json_response(serde_json::json!({
                "role": "assistant",
                "content": "There are 42 users."
            })),
        ]);
        let client = AiClient::new(url, "k", "m");
        let agent = Agent::new(client)
            .system("You are a data assistant.")
            .tool(Tool::simple("get_count", "Count rows in a table", |args| {
                let table = args.get("table").and_then(|t| t.as_str()).unwrap_or("?").to_string();
                Ok(serde_json::json!({ "table": table, "count": 42 }))
            }));
        let answer = agent.run("how many users?").unwrap();
        assert_eq!(answer, "There are 42 users.");
        handle.join().unwrap();
    }

    #[test]
    fn unknown_tool_reports_error() {
        let tool_call = serde_json::json!({
            "role": "assistant",
            "content": null,
            "tool_calls": [ {
                "id": "call_x",
                "type": "function",
                "function": { "name": "does_not_exist", "arguments": "{}" }
            } ]
        });
        let (url, handle) = mock_server(vec![
            json_response(tool_call),
            json_response(serde_json::json!({
                "role": "assistant",
                "content": "Sorry, that tool is missing."
            })),
        ]);
        let client = AiClient::new(url, "k", "m");
        let agent = Agent::new(client);
        let answer = agent.run("boom").unwrap();
        assert!(answer.contains("missing"));
        handle.join().unwrap();
    }

    #[test]
    fn message_serde_roundtrip() {
        let m = ChatMessage::user("hello");
        let j = serde_json::to_string(&m).unwrap();
        assert!(j.contains("\"role\":\"user\""));
        let tr = ChatMessage::tool_result("id1", "result text");
        let jt = serde_json::to_string(&tr).unwrap();
        assert!(jt.contains("\"tool_call_id\":\"id1\""));
    }

    #[test]
    fn max_steps_guard() {
        let tool_call = serde_json::json!({
            "role": "assistant",
            "content": null,
            "tool_calls": [ {
                "id": "c",
                "type": "function",
                "function": { "name": "noop", "arguments": "{}" }
            } ]
        });
        let (url, handle) = mock_server(vec![json_response(tool_call); 3]);
        let mut client = AiClient::new(url, "k", "m");
        client.timeout_secs = 10;
        let agent = Agent::new(client)
            .tool(Tool::simple("noop", "no-op", |_| Ok(serde_json::json!("ok"))))
            .max_steps(3);
        assert!(agent.run("loop forever").is_err());
        handle.join().unwrap();
    }
}
