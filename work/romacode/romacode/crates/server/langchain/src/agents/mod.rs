//! Agents — the Rust mirror of `langchain.agents` (`AgentExecutor` +
//! `create_tool_calling_agent`). The loop is the OpenAI tool-calling protocol:
//! the model requests tools, we run them, feed results back, repeat until it
//! answers in plain text or the iteration budget runs out.

pub mod tools;
pub mod tools_builtin;

use async_trait::async_trait;
use std::collections::HashMap;
use std::sync::Arc;

use crate::callbacks::{CallbackManager, VerboseHandler};
use crate::errors::{LangchainError, Result};
use crate::memory::Memory;
use crate::messages::{Message, ToolSpec};
use crate::runnables::Runnable;

pub use tools::Tool;
pub use tools_builtin::{calculator_tool, list_dir_tool, read_file_tool, shell_tool, write_file_tool};

/// The agent executor: plans with the model, runs tools, remembers the chat.
pub struct AgentExecutor {
    model: Arc<dyn crate::llms::BaseChatModel>,
    tools: Vec<Tool>,
    memory: Option<Box<dyn Memory>>,
    system_prompt: Option<String>,
    max_iterations: usize,
    callbacks: CallbackManager,
}

impl std::fmt::Debug for AgentExecutor {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("AgentExecutor")
            .field("model", &self.model.model_name())
            .field("tools", &self.tools.iter().map(|t| t.name()).collect::<Vec<_>>())
            .field("max_iterations", &self.max_iterations)
            .finish_non_exhaustive()
    }
}

impl AgentExecutor {
    /// Builder entry point.
    pub fn builder(model: Arc<dyn crate::llms::BaseChatModel>) -> AgentExecutorBuilder {
        AgentExecutorBuilder {
            model,
            tools: Vec::new(),
            memory: None,
            system_prompt: None,
            max_iterations: 10,
            verbose: false,
            callbacks: None,
        }
    }

    /// The OpenAI `tools` payload describing every registered tool.
    pub fn tool_specs(&self) -> Vec<ToolSpec> {
        self.tools.iter().map(|t| t.spec.clone()).collect()
    }

    fn lookup(&self, name: &str) -> Option<&Tool> {
        self.tools.iter().find(|t| t.spec.name == name)
    }

    /// Run one tool call, returning the string fed back to the model.
    /// Tool failures become `Error: ...` results rather than aborting the loop,
    /// so the model can retry with corrected arguments.
    pub async fn run_tool(&self, name: &str, args: &serde_json::Value) -> String {
        self.callbacks.tool_start(name, args).await;
        let result = match self.lookup(name) {
            None => Err(LangchainError::UnknownTool(name.to_string())),
            Some(tool) => {
                if let Err(e) = tools::validate_args(&tool.spec, args) {
                    Err(e)
                } else {
                    tool.call(args.clone()).await
                }
            }
        };
        let text = match result {
            Ok(s) => s,
            Err(e) => format!("Error: {e}"),
        };
        self.callbacks.tool_end(name, &text).await;
        text
    }

    /// Execute a task: the full plan → tools → answer loop.
    ///
    /// Returns the final assistant text. Tool-call errors are surfaced to the
    /// model as tool results; only transport-level LLM failures bubble up.
    pub async fn invoke(&mut self, input: &str) -> Result<String> {
        let specs = self.tool_specs();
        let mut messages: Vec<Message> = Vec::new();
        if let Some(sys) = &self.system_prompt {
            messages.push(Message::system(sys.clone()));
        }
        if let Some(mem) = &self.memory {
            messages.extend(mem.load());
        }
        messages.push(Message::human(input));

        let model = self.model.model_name().to_string();
        let mut final_answer: Option<String> = None;

        for iteration in 0..self.max_iterations {
            self.callbacks.agent_step(iteration + 1).await;
            self.callbacks.llm_start(&model, &messages).await;
            let response = self.model.generate_with_tools(&messages, &specs).await?;
            self.callbacks.llm_end(&model, response.text()).await;

            let tool_calls = match &response {
                Message::AI { tool_calls, .. } => tool_calls.clone(),
                _ => Vec::new(),
            };
            let has_calls = !tool_calls.is_empty();

            // Belt-and-braces: never let an empty tool_call id reach the wire.
            // The OpenAI client already synthesizes ids at parse time; this
            // second pass repairs anything that slipped through (other
            // model impls, hand-built messages), keeping the assistant
            // message and its tool results paired with the SAME id.
            let mut response = response;
            if let Message::AI { tool_calls: tcs, .. } = &mut response {
                for tc in tcs.iter_mut() {
                    if tc.id.trim().is_empty() {
                        tc.id = crate::llms::openai::next_local_call_id();
                    }
                }
            }
            let tool_calls = match &response {
                Message::AI { tool_calls, .. } => tool_calls.clone(),
                _ => Vec::new(),
            };
            messages.push(response);

            if !has_calls {
                let answer = messages.last().map(|m| m.text().to_string()).unwrap_or_default();
                final_answer = Some(answer);
                break;
            }

            for call in tool_calls {
                let result = self.run_tool(&call.name, &call.args).await;
                messages.push(Message::tool(call.id, call.name, result));
            }
        }

        match final_answer {
            Some(answer) => {
                if let Some(mem) = &mut self.memory {
                    mem.save_turn(input, &answer);
                }
                self.callbacks.agent_finish(&answer).await;
                Ok(answer)
            }
            None => {
                // Iteration budget exhausted. That is not a hard failure:
                // return the last assistant text (or a clear summary) so the
                // caller keeps the work done so far instead of losing the
                // whole turn to an error.
                let last_text = messages
                    .iter()
                    .rev()
                    .find_map(|m| match m {
                        Message::AI { content, .. } => {
                            content.clone().filter(|c| !c.trim().is_empty())
                        }
                        _ => None,
                    })
                    .unwrap_or_default();
                let answer = if last_text.trim().is_empty() {
                    format!(
                        "Stopped after {} tool rounds (budget). Partial progress preserved in the transcript — raise `agent.max_tool_rounds` to go further.",
                        self.max_iterations
                    )
                } else {
                    last_text
                };
                if let Some(mem) = &mut self.memory {
                    mem.save_turn(input, &answer);
                }
                self.callbacks.agent_finish(&answer).await;
                Ok(answer)
            }
        }
    }

    /// Like [`AgentExecutor::invoke`] but additionally streams the final answer
    /// through `on_llm_token` callbacks as it is already known — useful for the
    /// REPL to keep a single code path.
    pub async fn invoke_with_token_feed(&mut self, input: &str) -> Result<String> {
        let answer = self.invoke(input).await?;
        if !self.callbacks.is_empty() {
            for tok in split_feedable(&answer) {
                self.callbacks.llm_token(&tok).await;
            }
        }
        Ok(answer)
    }

    /// Access registered memory (e.g. to dump it in the REPL).
    pub fn memory(&self) -> Option<&dyn Memory> {
        self.memory.as_deref()
    }

    /// Mutable memory access.
    pub fn memory_mut(&mut self) -> Option<&mut Box<dyn Memory>> {
        self.memory.as_mut()
    }

    /// Drop the conversation.
    pub fn clear_memory(&mut self) {
        if let Some(mem) = &mut self.memory {
            mem.clear();
        }
    }
}

fn split_feedable(s: &str) -> Vec<String> {
    // Feed small chunks (word + trailing space) so token callbacks look natural.
    let mut chunks = Vec::new();
    let mut word = String::new();
    for c in s.chars() {
        word.push(c);
        if c == ' ' || c == '\n' {
            chunks.push(std::mem::take(&mut word));
        }
    }
    if !word.is_empty() {
        chunks.push(word);
    }
    chunks
}

/// Builder for [`AgentExecutor`].
pub struct AgentExecutorBuilder {
    model: Arc<dyn crate::llms::BaseChatModel>,
    tools: Vec<Tool>,
    memory: Option<Box<dyn Memory>>,
    system_prompt: Option<String>,
    max_iterations: usize,
    verbose: bool,
    callbacks: Option<CallbackManager>,
}

impl AgentExecutorBuilder {
    /// Register a tool.
    pub fn tool(mut self, tool: Tool) -> Self {
        self.tools.push(tool);
        self
    }

    /// Register several tools at once.
    pub fn tools(mut self, tools: Vec<Tool>) -> Self {
        self.tools.extend(tools);
        self
    }

    /// Attach conversation memory.
    pub fn memory(mut self, memory: Box<dyn Memory>) -> Self {
        self.memory = Some(memory);
        self
    }

    /// System prompt prepended to every run.
    pub fn system_prompt(mut self, prompt: impl Into<String>) -> Self {
        self.system_prompt = Some(prompt.into());
        self
    }

    /// Iteration budget (default 10).
    pub fn max_iterations(mut self, n: usize) -> Self {
        self.max_iterations = n.max(1);
        self
    }

    /// Print traces to stderr (Python's `verbose=True`).
    pub fn verbose(mut self, on: bool) -> Self {
        self.verbose = on;
        self
    }

    /// Install a custom callback manager (overrides `verbose`).
    /// Hosts like RomaCode route executor events to their own transports.
    pub fn callbacks(mut self, manager: CallbackManager) -> Self {
        self.callbacks = Some(manager);
        self
    }

    /// Build the executor.
    pub fn build(self) -> AgentExecutor {
        let callbacks = self.callbacks.unwrap_or_else(|| {
            if self.verbose {
                CallbackManager::new().add(Arc::new(VerboseHandler))
            } else {
                CallbackManager::new()
            }
        });
        AgentExecutor {
            model: self.model,
            tools: self.tools,
            memory: self.memory,
            system_prompt: self.system_prompt,
            max_iterations: self.max_iterations,
            callbacks,
        }
    }
}

// ---------------------------------------------------------------------------
// LLMChain — the classic single-shot chain, also usable through Runnable.
// ---------------------------------------------------------------------------

/// `prompt | model | parser` bundled as one runnable — `LLMChain`.
pub struct LLMChain {
    pub prompt: crate::prompts::ChatPromptTemplate,
    pub model: Arc<dyn crate::llms::BaseChatModel>,
}

impl std::fmt::Debug for LLMChain {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("LLMChain")
    }
}

impl LLMChain {
    /// Create a chain over a chat prompt template.
    pub fn new(prompt: crate::prompts::ChatPromptTemplate, model: Arc<dyn crate::llms::BaseChatModel>) -> Self {
        Self { prompt, model }
    }
}

#[async_trait]
impl Runnable<HashMap<String, crate::prompts::PromptValue>, String> for LLMChain {
    async fn invoke(
        &self,
        vars: HashMap<String, crate::prompts::PromptValue>,
    ) -> Result<String> {
        let messages = self.prompt.format(&vars)?;
        let msg = self.model.generate(&messages).await?;
        Ok(msg.text().to_string())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::llms::FakeChatModel;
    use crate::messages::ToolCall;
    use crate::memory::ConversationBufferMemory;
    use serde_json::json;

    #[tokio::test]
    async fn agent_answers_without_tools() {
        let model = Arc::new(FakeChatModel::new(vec![Message::ai("Hello there!")]));
        let mut agent = AgentExecutor::builder(model).build();
        let answer = agent.invoke("hi").await.unwrap();
        assert_eq!(answer, "Hello there!");
    }

    #[tokio::test]
    async fn agent_runs_tool_then_answers() {
        // Turn 1: model asks for the calculator. Turn 2: model answers.
        let model = Arc::new(FakeChatModel::new(vec![
            Message::AI {
                content: None,
                tool_calls: vec![ToolCall {
                    id: "call_1".into(),
                    name: "calculator".into(),
                    args: json!({"expression": "21*2"}),
                }],
            },
            Message::ai("21*2 is 42."),
        ]));
        let mut agent = AgentExecutor::builder(model)
            .tool(calculator_tool())
            .max_iterations(5)
            .build();
        let answer = agent.invoke("what is 21*2?").await.unwrap();
        assert_eq!(answer, "21*2 is 42.");
    }

    #[tokio::test]
    async fn agent_tool_error_is_fed_back_not_fatal() {
        // Turn 1: model calls an unknown tool; Turn 2: model recovers.
        let model = Arc::new(FakeChatModel::new(vec![
            Message::AI {
                content: None,
                tool_calls: vec![ToolCall {
                    id: "call_1".into(),
                    name: "nope".into(),
                    args: json!({}),
                }],
            },
            Message::ai("That tool does not exist."),
        ]));
        let mut agent = AgentExecutor::builder(model).build();
        let answer = agent.invoke("call nope").await.unwrap();
        assert_eq!(answer, "That tool does not exist.");
    }

    #[tokio::test]
    async fn agent_loop_limit_is_enforced() {
        // Model always requests the same tool: executor must stop — but
        // gracefully, with a clear budget message instead of a hard error
        // that would throw away the work done so far.
        let scripted = Message::AI {
            content: None,
            tool_calls: vec![ToolCall { id: "c".into(), name: "calculator".into(), args: json!({"expression": "1"}) }],
        };
        // 50 scripted turns, executor limit is 3.
        let model = Arc::new(FakeChatModel::new(vec![scripted; 50]));
        let mut agent = AgentExecutor::builder(model)
            .tool(calculator_tool())
            .max_iterations(3)
            .build();
        let answer = agent.invoke("loop forever").await.unwrap();
        assert!(
            answer.contains("Stopped after 3 tool rounds"),
            "budget stop must be reported to the caller: {answer}"
        );
    }

    #[tokio::test]
    async fn memory_records_turns() {
        let model = Arc::new(FakeChatModel::new(vec![Message::ai("answer one"), Message::ai("answer two")]));
        let mut agent = AgentExecutor::builder(model)
            .memory(Box::new(ConversationBufferMemory::new()))
            .build();
        agent.invoke("first").await.unwrap();
        agent.invoke("second").await.unwrap();
        let mem = agent.memory().unwrap();
        assert_eq!(mem.load().len(), 4);
    }

    #[tokio::test]
    async fn llm_chain_formats_and_completes() {
        use crate::prompts::{ChatPromptTemplate, PromptValue};
        let model = Arc::new(FakeChatModel::new(vec![Message::ai("42")]));
        let prompt = ChatPromptTemplate::from_system_and_human("You are terse.", "Q: {q}");
        let chain = LLMChain::new(prompt, model);
        let mut vars = HashMap::new();
        vars.insert("q".to_string(), PromptValue::Text("meaning of life".into()));
        let out = chain.invoke(vars).await.unwrap();
        assert_eq!(out, "42");
    }

    #[tokio::test]
    async fn token_feed_splits_words() {
        let chunks = split_feedable("one two\nthree");
        assert_eq!(chunks, vec!["one ".to_string(), "two\n".to_string(), "three".to_string()]);
    }
}
