//! OpenAI-compatible chat client — `ChatOpenAI`.
//!
//! Works with any server exposing `/chat/completions`: OpenAI, Azure OpenAI,
//! LM Studio, vLLM, Ollama's OpenAI compatibility layer, llama.cpp server, etc.
//!
//! ```ignore
//! let model = ChatOpenAI::builder()
//!     .model("gpt-4o-mini")
//!     .api_key(std::env::var("OPENAI_API_KEY").unwrap())
//!     .temperature(0.2)
//!     .build()?;
//! ```

use async_trait::async_trait;
use futures::future::FutureExt;
use futures::stream::{self, BoxStream, StreamExt};
use reqwest::header::{HeaderValue, AUTHORIZATION, CONTENT_TYPE};
use serde_json::{json, Value};
use std::sync::Mutex;
use std::time::Duration;

use crate::errors::{LangchainError, Result};
use crate::llms::{BaseChatModel, TokenUsage};
use crate::messages::{Message, ToolCall, ToolSpec};

/// Chat completion model for every OpenAI-compatible endpoint.
pub struct ChatOpenAI {
    model: String,
    api_key: String,
    base_url: String,
    temperature: f64,
    max_tokens: Option<u32>,
    timeout: Duration,
    client: reqwest::Client,
    usage: Mutex<TokenUsage>,
}

impl std::fmt::Debug for ChatOpenAI {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ChatOpenAI")
            .field("model", &self.model)
            .field("base_url", &self.base_url)
            .field("temperature", &self.temperature)
            .finish_non_exhaustive()
    }
}

impl ChatOpenAI {
    /// Builder entry point.
    pub fn builder() -> ChatOpenAIBuilder {
        ChatOpenAIBuilder::default()
    }

    /// Serialise messages + tools into the request body.
    fn build_body(messages: &[Message], tools: &[ToolSpec]) -> Value {
        let mut body = json!({
            "messages": messages.iter().map(|m| m.to_openai_json()).collect::<Vec<_>>(),
        });
        if !tools.is_empty() {
            body["tools"] = Value::Array(tools.iter().map(|t| t.to_openai_json()).collect());
        }
        body
    }

    fn inject_options(&self, body: &mut Value) {
        body["model"] = json!(self.model);
        body["temperature"] = json!(self.temperature);
        if let Some(mt) = self.max_tokens {
            body["max_tokens"] = json!(mt);
        }
    }

    /// POST and read the full JSON response, mapping provider errors to
    /// [`LangchainError::LlmRequest`]. Transient provider failures
    /// (overloaded gateways, 429/5xx) are retried with backoff instead of
    /// killing the whole agent turn.
    async fn post(&self, body: &Value) -> Result<Value> {
        const MAX_ATTEMPTS: u32 = 4;
        for attempt in 1..=MAX_ATTEMPTS {
            match self.post_once(body).await {
                Ok(v) => return Ok(v),
                Err(e) => {
                    let transient = match &e {
                        LangchainError::LlmRequest(t) => is_transient_text(t),
                        _ => false,
                    };
                    if !transient || attempt == MAX_ATTEMPTS {
                        return Err(e);
                    }
                    let wait = std::time::Duration::from_secs(2u64.pow(attempt - 1) * 2);
                    tokio::time::sleep(wait).await;
                }
            }
        }
        unreachable!("retry loop always returns")
    }

    async fn post_once(&self, body: &Value) -> Result<Value> {
        let url = format!("{}/chat/completions", self.base_url.trim_end_matches('/'));
        let resp = self
            .client
            .post(&url)
            .header(CONTENT_TYPE, HeaderValue::from_static("application/json"))
            .header(AUTHORIZATION, format!("Bearer {}", self.api_key))
            .timeout(self.timeout)
            .json(body)
            .send()
            .await?;

        let status = resp.status();
        let text = resp.text().await?;
        if !status.is_success() {
            return Err(LangchainError::LlmRequest(format!("HTTP {status}: {}", truncate(&text, 400))));
        }
        serde_json::from_str::<Value>(&text).map_err(|e| {
            LangchainError::LlmParse(format!("non-JSON response: {e}: {}", truncate(&text, 200)))
        })
    }

    /// Extract the assistant `Message` (with tool calls) from a completion payload.
    fn parse_choice(payload: &Value) -> Result<Message> {
        let choice = payload
            .get("choices")
            .and_then(|c| c.get(0))
            .ok_or_else(|| LangchainError::LlmParse("response missing `choices[0]`".into()))?;

        let content = choice
            .get("message")
            .and_then(|m| m.get("content"))
            .and_then(|c| c.as_str())
            .map(|s| s.to_string());

        let mut tool_calls = Vec::new();
        if let Some(tcs) = choice
            .get("message")
            .and_then(|m| m.get("tool_calls"))
            .and_then(|t| t.as_array())
        {
            for tc in tcs {
                // Several OpenAI-compatible gateways (NVIDIA NIM, minimax,
                // Coarena, …) omit `id` on tool calls. Echoing an empty id back
                // makes them reject the next request with "tool_call_id must
                // be a non-empty string" — synthesize a stable id instead so
                // the assistant tool_call and its tool result always match.
                let id = match tc.get("id").and_then(|v| v.as_str()) {
                    Some(s) if !s.trim().is_empty() => s.to_string(),
                    _ => next_local_call_id(),
                };
                let name = tc
                    .pointer("/function/name")
                    .and_then(|v| v.as_str())
                    .unwrap_or("")
                    .to_string();
                let raw_args = tc
                    .pointer("/function/arguments")
                    .and_then(|v| v.as_str())
                    .unwrap_or("{}");
                let args: Value = serde_json::from_str(raw_args)
                    .unwrap_or_else(|_| json!({ "_raw_arguments": raw_args }));
                tool_calls.push(ToolCall { id, name, args });
            }
        }

        Ok(Message::AI { content, tool_calls })
    }

    fn record_usage(&self, payload: &Value) {
        if let Some(u) = payload.get("usage") {
            let pt = u.get("prompt_tokens").and_then(|v| v.as_u64()).unwrap_or(0);
            let ct = u.get("completion_tokens").and_then(|v| v.as_u64()).unwrap_or(0);
            let tt = u.get("total_tokens").and_then(|v| v.as_u64()).unwrap_or(0);
            if let Ok(mut guard) = self.usage.lock() {
                guard.prompt_tokens += pt;
                guard.completion_tokens += ct;
                guard.total_tokens += tt;
            }
        }
    }

    /// Open the SSE stream and return a `'static` stream of raw byte chunks.
    ///
    /// No total request timeout is applied here: reqwest's per-request
    /// `.timeout()` runs until the response BODY is fully read, which killed
    /// healthy long streams mid-generation. Connection establishment is
    /// still bounded by the client's `connect_timeout`.
    async fn open_sse(
        &self,
        messages: &[Message],
        tools: &[ToolSpec],
    ) -> Result<BoxStream<'static, Result<Vec<u8>>>> {
        let url = format!("{}/chat/completions", self.base_url.trim_end_matches('/'));
        let mut body = Self::build_body(messages, tools);
        self.inject_options(&mut body);
        body["stream"] = json!(true);

        let resp = self
            .client
            .post(&url)
            .header(CONTENT_TYPE, HeaderValue::from_static("application/json"))
            .header(AUTHORIZATION, format!("Bearer {}", self.api_key))
            // Intentionally no `.timeout(self.timeout)` — see doc comment.
            .json(&body)
            .send()
            .await?;

        let status = resp.status();
        if !status.is_success() {
            let text = resp.text().await.unwrap_or_default();
            return Err(LangchainError::LlmRequest(format!("HTTP {status}: {}", truncate(&text, 400))));
        }

        Ok(Box::pin(resp.bytes_stream().map(|chunk| {
            chunk.map(|b| b.to_vec()).map_err(LangchainError::from)
        })))
    }

    /// Shared SSE driver: buffer raw bytes across TCP chunks, split complete
    /// events on `\n\n` boundaries, decode each event once and parse the
    /// `data:` payloads. Terminates the whole stream on `data: [DONE]`.
    fn sse_to_deltas(raw: BoxStream<'static, Result<Vec<u8>>>) -> BoxStream<'static, Result<String>> {
        stream::unfold(
            (raw, SseBuffer::default()),
            |(mut raw, mut st)| async move {
                loop {
                    if st.done {
                        return None;
                    }
                    // Yield deltas from complete buffered events first.
                    if let Some(event) = st.take_event() {
                        let delta = parse_sse_event(&event, &mut st);
                        if st.done && delta.is_none() {
                            return None;
                        }
                        if let Some(delta) = delta {
                            return Some((Ok(delta), (raw, st)));
                        }
                        continue;
                    }
                    // Need more bytes from the wire.
                    match raw.next().await {
                        Some(Ok(bytes)) => st.buf.extend_from_slice(&bytes),
                        Some(Err(e)) => return Some((Err(e), (raw, st))),
                        None => {
                            // Stream ended: flush any trailing unterminated event.
                            if st.buf.is_empty() {
                                return None;
                            }
                            let rest = std::mem::take(&mut st.buf);
                            let event = String::from_utf8_lossy(&rest).into_owned();
                            let delta = parse_sse_event(&event, &mut st);
                            match delta {
                                Some(d) if !st.done => return Some((Ok(d), (raw, st))),
                                _ => return None,
                            }
                        }
                    }
                }
            },
        )
        .boxed()
    }
}

/// Byte buffer that only hands out COMPLETE SSE events: an event ends at an
/// empty line (`\n\n`, or `\r\n\r\n`/`\n\r\n` for CRLF servers). Decoding at
/// event level keeps multi-byte UTF-8 sequences that straddle TCP chunk
/// boundaries intact.
#[derive(Default)]
struct SseBuffer {
    buf: Vec<u8>,
    done: bool,
}

impl SseBuffer {
    /// Extract the next complete event from the buffer, if one has fully
    /// arrived. Incomplete events stay buffered for the next chunk.
    fn take_event(&mut self) -> Option<String> {
        let mut i = 0;
        while i < self.buf.len() {
            if self.buf[i] == b'\n' {
                let boundary = if self.buf.get(i + 1) == Some(&b'\n') {
                    2
                } else if self.buf.get(i + 1) == Some(&b'\r') && self.buf.get(i + 2) == Some(&b'\n') {
                    3
                } else {
                    0
                };
                if boundary > 0 {
                    let event = self.buf.drain(..i).collect::<Vec<u8>>();
                    self.buf.drain(..boundary);
                    return Some(String::from_utf8_lossy(&event).into_owned());
                }
            }
            i += 1;
        }
        None
    }
}

/// Parse one SSE event into its `data:` payload(s); returns the next text
/// delta, if any. Empty lines and `:`-prefixed commentary (keep-alives) are
/// skipped silently; a non-empty unparseable payload is logged and skipped.
fn parse_sse_event(event: &str, st: &mut SseBuffer) -> Option<String> {
    let mut data_lines: Vec<String> = Vec::new();
    for line in event.lines() {
        let line = line.trim_end_matches('\r');
        if line.is_empty() || line.starts_with(':') {
            continue;
        }
        if let Some(data) = line.strip_prefix("data:") {
            // Strip the field name, then leading space — handles both the
            // `data: {...}` and the spaceless `data:{...}` SSE forms.
            let data = data.trim_start();
            if data.is_empty() {
                continue;
            }
            if data == "[DONE]" {
                st.done = true;
                continue;
            }
            data_lines.push(data.to_string());
        }
        // Other fields (`event:`, `id:`, `retry:`) carry no deltas.
    }
    if data_lines.is_empty() {
        return None;
    }
    let payload = data_lines.join("\n");
    match serde_json::from_str::<Value>(&payload) {
        Ok(v) => v
            .pointer("/choices/0/delta/content")
            .and_then(|c| c.as_str())
            .filter(|t| !t.is_empty())
            .map(|t| t.to_string()),
        Err(e) => {
            // Do not swallow JSON failures invisibly.
            eprintln!(
                "langchain: skipping unparseable SSE data payload ({e}): {}",
                truncate(&payload, 200)
            );
            None
        }
    }
}

#[async_trait]
impl BaseChatModel for ChatOpenAI {
    fn model_name(&self) -> &str {
        &self.model
    }

    async fn generate(&self, messages: &[Message]) -> Result<Message> {
        let mut body = Self::build_body(messages, &[]);
        self.inject_options(&mut body);
        let payload = self.post(&body).await?;
        self.record_usage(&payload);
        Self::parse_choice(&payload)
    }

    async fn generate_with_tools(&self, messages: &[Message], tools: &[ToolSpec]) -> Result<Message> {
        let mut body = Self::build_body(messages, tools);
        self.inject_options(&mut body);
        let payload = self.post(&body).await?;
        self.record_usage(&payload);
        Self::parse_choice(&payload)
    }

    /// Stream assistant text deltas. Terminates on the SSE `data: [DONE]` sentinel.
    fn stream_text<'a>(&'a self, messages: &'a [Message]) -> BoxStream<'a, Result<String>> {
        let this: &'a ChatOpenAI = self;
        let msgs: Vec<Message> = messages.to_vec();
        // Future: open the HTTP stream, then flatten it into the item stream.
        // Errors surface as the first stream item.
        let fut = async move {
            match this.open_sse(&msgs, &[]).await {
                Ok(raw) => ChatOpenAI::sse_to_deltas(raw),
                Err(e) => stream::iter(vec![Err(e)]).boxed(),
            }
        };
        fut.flatten_stream().boxed()
    }

    fn usage(&self) -> Option<TokenUsage> {
        self.usage.lock().ok().map(|g| *g)
    }
}

fn truncate(s: &str, n: usize) -> String {
    if s.chars().count() <= n {
        s.to_string()
    } else {
        format!("{}…", s.chars().take(n).collect::<String>())
    }
}

/// Builder for [`ChatOpenAI`].
#[derive(Debug, Default)]
pub struct ChatOpenAIBuilder {
    model: Option<String>,
    api_key: Option<String>,
    base_url: Option<String>,
    temperature: Option<f64>,
    max_tokens: Option<u32>,
    timeout_secs: Option<u64>,
}

impl ChatOpenAIBuilder {
    /// Model id, e.g. `gpt-4o-mini`.
    pub fn model(mut self, model: impl Into<String>) -> Self {
        self.model = Some(model.into());
        self
    }

    /// API key (`Authorization: Bearer ...`).
    pub fn api_key(mut self, key: impl Into<String>) -> Self {
        self.api_key = Some(key.into());
        self
    }

    /// Base URL, default `https://api.openai.com/v1`.
    pub fn base_url(mut self, url: impl Into<String>) -> Self {
        self.base_url = Some(url.into());
        self
    }

    /// Sampling temperature (default 0.7).
    pub fn temperature(mut self, t: f64) -> Self {
        self.temperature = Some(t);
        self
    }

    /// Max completion tokens.
    pub fn max_tokens(mut self, n: u32) -> Self {
        self.max_tokens = Some(n);
        self
    }

    /// Request timeout in seconds (default 120).
    pub fn timeout_secs(mut self, s: u64) -> Self {
        self.timeout_secs = Some(s);
        self
    }

    /// Validate and construct.
    pub fn build(self) -> Result<ChatOpenAI> {
        let model = self
            .model
            .ok_or_else(|| LangchainError::InvalidInput("model is required".into()))?;
        let api_key = self
            .api_key
            .ok_or_else(|| LangchainError::InvalidInput("api_key is required".into()))?;
        let base_url = self
            .base_url
            .unwrap_or_else(|| "https://api.openai.com/v1".to_string());
        let client = reqwest::Client::builder()
            // Bound connection establishment + TLS handshake; body reads are
            // NOT limited (long-lived SSE streams) and non-streaming calls
            // carry their own per-request `.timeout(self.timeout)`.
            .connect_timeout(Duration::from_secs(30))
            .build()
            .map_err(|e| LangchainError::LlmRequest(e.to_string()))?;
        Ok(ChatOpenAI {
            model,
            api_key,
            base_url,
            temperature: self.temperature.unwrap_or(0.7),
            max_tokens: self.max_tokens,
            timeout: Duration::from_secs(self.timeout_secs.unwrap_or(120)),
            client,
            usage: Mutex::new(TokenUsage::default()),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_choice_extracts_tool_calls() {
        let payload = json!({
            "choices": [{
                "message": {
                    "role": "assistant",
                    "content": null,
                    "tool_calls": [{
                        "id": "call_1",
                        "type": "function",
                        "function": {"name": "calc", "arguments": "{\"x\":2}"}
                    }]
                }
            }],
            "usage": {"prompt_tokens": 10, "completion_tokens": 5, "total_tokens": 15}
        });
        let m = ChatOpenAI::parse_choice(&payload).unwrap();
        match m {
            Message::AI { content, tool_calls } => {
                assert_eq!(content, None);
                assert_eq!(tool_calls.len(), 1);
                assert_eq!(tool_calls[0].name, "calc");
                assert_eq!(tool_calls[0].args["x"], 2);
            }
            other => panic!("wrong variant: {other:?}"),
        }
    }

    #[test]
    fn parse_choice_synthesizes_missing_tool_call_id() {
        // NVIDIA NIM / minimax / Coarena-style gateways stream tool calls
        // without an `id`. The parser must synthesize a non-empty one —
        // echoing "" back gets the request rejected ("tool_call_id must be
        // a non-empty string").
        let payload = json!({
            "choices": [{
                "message": {
                    "role": "assistant",
                    "content": null,
                    "tool_calls": [{
                        "type": "function",
                        "function": {"name": "calc", "arguments": "{}"}
                    }]
                }
            }]
        });
        let m = ChatOpenAI::parse_choice(&payload).unwrap();
        match m {
            Message::AI { tool_calls, .. } => {
                assert_eq!(tool_calls.len(), 1);
                assert!(!tool_calls[0].id.trim().is_empty(), "id must not be empty");
                assert!(tool_calls[0].id.starts_with("call_romacode_"));
            }
            other => panic!("wrong variant: {other:?}"),
        }
    }

    #[test]
    fn parse_choice_plain_text() {
        let payload = json!({"choices": [{"message": {"role": "assistant", "content": "hi"}}]});
        let m = ChatOpenAI::parse_choice(&payload).unwrap();
        assert_eq!(m.text(), "hi");
    }

    #[test]
    fn parse_choice_missing_choices_errors() {
        assert!(ChatOpenAI::parse_choice(&json!({"error": "x"})).is_err());
    }

    #[test]
    fn builder_requires_model_and_key() {
        assert!(ChatOpenAI::builder().build().is_err());
        assert!(ChatOpenAI::builder().model("m").build().is_err());
        assert!(ChatOpenAI::builder().model("m").api_key("k").build().is_ok());
    }

    fn raw_stream(chunks: Vec<Vec<u8>>) -> BoxStream<'static, Result<Vec<u8>>> {
        stream::iter(chunks.into_iter().map(Ok)).boxed()
    }

    #[tokio::test]
    async fn sse_event_split_across_chunks_is_parsed_exactly_once() {
        // Regression (H3): each TCP chunk used to be decoded and parsed
        // independently, so a `data: {...}` event straddling a chunk
        // boundary failed to parse in BOTH halves and was silently dropped.
        let full = "data: {\"choices\":[{\"delta\":{\"content\":\"hi\"}}]}\n\n";
        let bytes = full.as_bytes();
        let split = bytes.len() / 2; // mid-JSON
        let deltas: Vec<String> = ChatOpenAI::sse_to_deltas(raw_stream(vec![
            bytes[..split].to_vec(),
            bytes[split..].to_vec(),
        ]))
        .map(|r| r.unwrap())
        .collect()
        .await;
        assert_eq!(deltas, vec!["hi".to_string()], "event must be received exactly once");
    }

    #[tokio::test]
    async fn sse_multibyte_utf8_split_across_chunks() {
        // `é` is two UTF-8 bytes; split the byte stream INSIDE the character.
        let event = "data: {\"choices\":[{\"delta\":{\"content\":\"héllo wörld\"}}]}\n\n";
        let bytes = event.as_bytes();
        let mid = event.find('é').unwrap() + 1;
        let deltas: Vec<String> = ChatOpenAI::sse_to_deltas(raw_stream(vec![
            bytes[..mid].to_vec(),
            bytes[mid..].to_vec(),
        ]))
        .map(|r| r.unwrap())
        .collect()
        .await;
        assert_eq!(deltas, vec!["héllo wörld".to_string()]);
    }

    #[tokio::test]
    async fn sse_done_terminates_the_outer_stream() {
        // Regression: `break` on [DONE] only exited the inner line loop, so
        // the stream kept polling the wire after completion.
        let ev = "data: {\"choices\":[{\"delta\":{\"content\":\"a\"}}]}\n\n";
        let done = "data: [DONE]\n\n";
        let never = "data: {\"choices\":[{\"delta\":{\"content\":\"NEVER\"}}]}\n\n";
        let deltas: Vec<String> = ChatOpenAI::sse_to_deltas(raw_stream(vec![
            format!("{ev}{done}").into_bytes(),
            never.as_bytes().to_vec(),
        ]))
        .map(|r| r.unwrap())
        .collect()
        .await;
        assert_eq!(deltas, vec!["a".to_string()]);
    }

    #[tokio::test]
    async fn sse_spaceless_data_prefix_and_commentary() {
        // Regression (M38c): `strip_prefix("data: ")` missed the spaceless
        // `data:{...}` form; keep-alive commentary lines must be skipped.
        let raw = raw_stream(vec![
            b": keep-alive\n\ndata:{\"choices\":[{\"delta\":{\"content\":\"x\"}}]}\n\ndata: \n\n".to_vec(),
        ]);
        let deltas: Vec<String> = ChatOpenAI::sse_to_deltas(raw).map(|r| r.unwrap()).collect().await;
        assert_eq!(deltas, vec!["x".to_string()]);
    }

    #[tokio::test]
    async fn sse_unparseable_payload_is_logged_and_skipped() {
        let raw = raw_stream(vec![
            b"data: {not-json}\n\ndata: {\"choices\":[{\"delta\":{\"content\":\"ok\"}}]}\n\n".to_vec(),
        ]);
        let deltas: Vec<String> = ChatOpenAI::sse_to_deltas(raw).map(|r| r.unwrap()).collect().await;
        assert_eq!(deltas, vec!["ok".to_string()], "bad payload skipped, good one parsed");
    }

    #[tokio::test]
    async fn sse_crlf_event_boundaries() {
        let raw = raw_stream(vec![
            b"data: {\"choices\":[{\"delta\":{\"content\":\"c\"}}]}\r\n\r\n".to_vec(),
        ]);
        let deltas: Vec<String> = ChatOpenAI::sse_to_deltas(raw).map(|r| r.unwrap()).collect().await;
        assert_eq!(deltas, vec!["c".to_string()]);
    }

    #[test]
    fn transient_detection_is_precise() {
        // Regression (M38b): bare numeric substrings ("500") matched
        // permanent 400 responses whose body mentioned e.g. max_tokens: 500.
        assert!(!is_transient_text(
            "HTTP 400 Bad Request: {\"error\":{\"message\":\"max_tokens must be at most 500\"}}"
        ));
        assert!(!is_transient_text("HTTP 401 Unauthorized: invalid api key"));
        assert!(!is_transient_text("HTTP 404 Not Found: model not found"));
        // Exact transient statuses still match at the status position.
        assert!(is_transient_text("HTTP 429 Too Many Requests: slow down"));
        assert!(is_transient_text("HTTP 500 Internal Server Error"));
        assert!(is_transient_text("HTTP 503: overloaded"));
        // Structured error types/phrases from the body.
        assert!(is_transient_text("HTTP 429: {\"error\":{\"type\":\"rate_limit_exceeded\"}}"));
        assert!(is_transient_text("connection reset by peer"));
        assert!(is_transient_text("operation timed out"));
    }
}

/// Process-unique fallback id for tool calls that arrive without one.
/// Both the assistant `tool_calls[].id` and the matching `tool` message's
/// `tool_call_id` get the same value, so gateways that dropped the id still
/// see a consistent request/response pair.
pub fn next_local_call_id() -> String {
    use std::sync::atomic::{AtomicU64, Ordering};
    static N: AtomicU64 = AtomicU64::new(0);
    format!("call_romacode_{}", N.fetch_add(1, Ordering::Relaxed))
}

/// Shared transient-failure detector for provider error text (overloaded
/// gateways, rate limits, 429/5xx). Used by the retry path in `ChatOpenAI`.
///
/// Detection is deliberately precise: exact HTTP status codes read from the
/// `HTTP <code>` prefix the client formats, plus structured error types and
/// established transient phrases. Bare numeric substrings ("500") must NOT
/// match — a permanent 400 whose body mentions e.g. `max_tokens: 500` used
/// to be retried as if it were transient.
fn is_transient_text(t: &str) -> bool {
    let l = t.to_lowercase();
    if let Some(rest) = l.strip_prefix("http ") {
        // Errors are formatted as `HTTP {status}: {body}`; match the status
        // position exactly instead of anywhere in the text.
        let code = rest.get(0..3).unwrap_or("");
        if matches!(code, "429" | "500" | "502" | "503" | "504") {
            return true;
        }
    }
    [
        "rate_limit",
        "rate limit",
        "overloaded",
        "overload",
        "over capacity",
        "too many requests",
        "service unavailable",
        "bad gateway",
        "gateway timeout",
        "internal server error",
        "server_error",
        "temporarily",
        "try again",
        "timed out",
        "timeout",
        "connection reset",
    ]
    .iter()
    .any(|k| l.contains(k))
}
