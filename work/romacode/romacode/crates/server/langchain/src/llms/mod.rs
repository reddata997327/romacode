//! Model abstractions — the Rust mirror of `langchain_core.language_models`
//! (`BaseChatModel`) plus a deterministic `FakeChatModel` for tests.

pub mod openai;

use async_trait::async_trait;
use futures::stream::{once, BoxStream};

use crate::errors::{LangchainError, Result};
use crate::messages::{Message, ToolSpec};

/// The chat-model interface every provider implements.
///
/// Mirrors `BaseChatModel` / `BaseChatModelWithToolCalling`:
/// - [`BaseChatModel::generate`] — plain completion.
/// - [`BaseChatModel::generate_with_tools`] — completion with tool definitions bound.
/// - [`BaseChatModel::stream_text`] — token streaming (defaults to one-shot).
#[async_trait]
pub trait BaseChatModel: Send + Sync {
    /// Model identifier reported in callbacks (e.g. `gpt-4o-mini`).
    fn model_name(&self) -> &str;

    /// Complete a conversation without tool bindings.
    async fn generate(&self, messages: &[Message]) -> Result<Message>;

    /// Complete a conversation with tool definitions available to the model.
    async fn generate_with_tools(&self, messages: &[Message], _tools: &[ToolSpec]) -> Result<Message> {
        // Providers without tool support degrade to plain generation.
        self.generate(messages).await
    }

    /// Stream the textual part of a completion as chunks.
    ///
    /// Default implementation: run `generate` and emit the whole text once.
    /// Tool calls are not surfaced through streaming; use `generate_with_tools`
    /// inside agent loops and keep streaming for display paths.
    fn stream_text<'a>(&'a self, messages: &'a [Message]) -> BoxStream<'a, Result<String>> {
        // Clone the messages so the future only borrows `self` with lifetime 'a.
        let msgs: Vec<Message> = messages.to_vec();
        let fut = async move {
            let msg = self.generate(&msgs).await?;
            Ok(msg.text().to_string())
        };
        Box::pin(once(fut))
    }

    /// Token counts reported by the provider, when available.
    fn usage(&self) -> Option<TokenUsage> {
        None
    }
}

/// Token accounting — `LLMResult.token_usage` in Python LangChain.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct TokenUsage {
    pub prompt_tokens: u64,
    pub completion_tokens: u64,
    pub total_tokens: u64,
}

/// A scripted chat model for unit tests and `--demo` mode: returns the given
/// messages in order and errors when the script is exhausted.
///
/// If an item contains tool calls, they are returned verbatim so the agent
/// executor can be tested end-to-end without network access.
pub struct FakeChatModel {
    script: std::sync::Mutex<std::collections::VecDeque<Message>>,
    label: String,
}

impl std::fmt::Debug for FakeChatModel {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("FakeChatModel")
    }
}

impl FakeChatModel {
    /// Queue the messages to return, one per `generate` call.
    pub fn new(script: Vec<Message>) -> Self {
        Self {
            script: std::sync::Mutex::new(script.into()),
            label: "fake".into(),
        }
    }

    /// Rename the model (shows up in callbacks).
    pub fn with_label(mut self, label: impl Into<String>) -> Self {
        self.label = label.into();
        self
    }
}

#[async_trait]
impl BaseChatModel for FakeChatModel {
    fn model_name(&self) -> &str {
        &self.label
    }

    async fn generate(&self, _messages: &[Message]) -> Result<Message> {
        self.script
            .lock()
            .map_err(|_| LangchainError::Other("fake model poisoned".into()))?
            .pop_front()
            .ok_or_else(|| LangchainError::Other("fake model script exhausted".into()))
    }

    async fn generate_with_tools(&self, _messages: &[Message], _tools: &[ToolSpec]) -> Result<Message> {
        self.generate(_messages).await
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn fake_model_returns_script_in_order() {
        let m = FakeChatModel::new(vec![Message::ai("one"), Message::ai("two")]);
        let m1 = m.generate(&[]).await.unwrap();
        let m2 = m.generate(&[]).await.unwrap();
        assert_eq!(m1.text(), "one");
        assert_eq!(m2.text(), "two");
        assert!(m.generate(&[]).await.is_err());
    }

    #[tokio::test]
    async fn default_stream_emits_whole_text() {
        use futures::StreamExt;
        let m = FakeChatModel::new(vec![Message::ai("hello world")]);
        let chunks: Vec<_> = m.stream_text(&[]).collect().await;
        assert_eq!(chunks[0].as_ref().unwrap(), "hello world");
    }
}
