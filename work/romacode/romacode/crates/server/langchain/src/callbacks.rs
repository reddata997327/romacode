//! Callback system — the Rust mirror of `langchain_core.callbacks`
//! (`BaseCallbackHandler` + the `verbose=True` handler).

use async_trait::async_trait;
use std::sync::Arc;

use crate::messages::Message;

/// Observer hooks fired at key points of LLM / tool / agent execution.
///
/// Every method defaults to a no-op, so implement only what you need.
/// Handlers are shared behind `Arc` and must be `Send + Sync`.
#[async_trait]
pub trait CallbackHandler: Send + Sync {
    /// Called right before a chat model request.
    async fn on_llm_start(&self, _model: &str, _messages: &[Message]) {}

    /// Called after the chat model returned its message.
    async fn on_llm_end(&self, _model: &str, _output: &str) {}

    /// Called when the model requested a tool execution.
    async fn on_tool_start(&self, _tool: &str, _args: &serde_json::Value) {}

    /// Called with the raw string result of a tool execution.
    async fn on_tool_end(&self, _tool: &str, _result: &str) {}

    /// Called once per agent iteration (planning round).
    async fn on_agent_step(&self, _iteration: usize) {}

    /// Called with the agent's final answer.
    async fn on_agent_finish(&self, _output: &str) {}

    /// Called for every streamed token chunk.
    async fn on_llm_token(&self, _token: &str) {}
}

/// The Python `verbose=True` equivalent: prints every event to stderr.
#[derive(Debug, Default, Clone, Copy)]
pub struct VerboseHandler;

#[async_trait]
impl CallbackHandler for VerboseHandler {
    async fn on_llm_start(&self, model: &str, messages: &[Message]) {
        eprintln!("\x1b[36m[llm:start]\x1b[0m model={model} messages={}", messages.len());
    }

    async fn on_llm_end(&self, model: &str, output: &str) {
        eprintln!("\x1b[36m[llm:end]\x1b[0m model={model} output={:?}" , truncate(output, 120));
    }

    async fn on_tool_start(&self, tool: &str, args: &serde_json::Value) {
        eprintln!("\x1b[33m[tool:start]\x1b[0m {tool} args={args}");
    }

    async fn on_tool_end(&self, tool: &str, result: &str) {
        eprintln!("\x1b[33m[tool:end]\x1b[0m {tool} result={:?}", truncate(result, 160));
    }

    async fn on_agent_step(&self, iteration: usize) {
        eprintln!("\x1b[35m[agent:step]\x1b[0m iteration={iteration}");
    }

    async fn on_agent_finish(&self, output: &str) {
        eprintln!("\x1b[35m[agent:finish]\x1b[0m {:?}", truncate(output, 200));
    }

    async fn on_llm_token(&self, token: &str) {
        eprint!("{token}");
    }
}

fn truncate(s: &str, n: usize) -> String {
    if s.chars().count() <= n {
        s.to_string()
    } else {
        let cut: String = s.chars().take(n).collect();
        format!("{cut}…")
    }
}

/// A set of handlers dispatched together by runnables and the agent executor.
#[derive(Clone, Default)]
pub struct CallbackManager {
    handlers: Vec<Arc<dyn CallbackHandler>>,
}

impl CallbackManager {
    /// Empty manager.
    pub fn new() -> Self {
        Self { handlers: Vec::new() }
    }

    /// Register an additional handler.
    pub fn add(mut self, handler: Arc<dyn CallbackHandler>) -> Self {
        self.handlers.push(handler);
        self
    }

    /// True when no handler is registered (fast path: skip event construction).
    pub fn is_empty(&self) -> bool {
        self.handlers.is_empty()
    }

    /// Fire `on_llm_start` on every handler.
    pub async fn llm_start(&self, model: &str, messages: &[Message]) {
        for h in &self.handlers {
            h.on_llm_start(model, messages).await;
        }
    }

    /// Fire `on_llm_end` on every handler.
    pub async fn llm_end(&self, model: &str, output: &str) {
        for h in &self.handlers {
            h.on_llm_end(model, output).await;
        }
    }

    /// Fire `on_tool_start` on every handler.
    pub async fn tool_start(&self, tool: &str, args: &serde_json::Value) {
        for h in &self.handlers {
            h.on_tool_start(tool, args).await;
        }
    }

    /// Fire `on_tool_end` on every handler.
    pub async fn tool_end(&self, tool: &str, result: &str) {
        for h in &self.handlers {
            h.on_tool_end(tool, result).await;
        }
    }

    /// Fire `on_agent_step` on every handler.
    pub async fn agent_step(&self, iteration: usize) {
        for h in &self.handlers {
            h.on_agent_step(iteration).await;
        }
    }

    /// Fire `on_agent_finish` on every handler.
    pub async fn agent_finish(&self, output: &str) {
        for h in &self.handlers {
            h.on_agent_finish(output).await;
        }
    }

    /// Fire `on_llm_token` on every handler.
    pub async fn llm_token(&self, token: &str) {
        for h in &self.handlers {
            h.on_llm_token(token).await;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn empty_manager_is_fast_path() {
        let mgr = CallbackManager::new();
        assert!(mgr.is_empty());
        assert!(!mgr.clone().add(Arc::new(VerboseHandler)).is_empty());
    }
}
