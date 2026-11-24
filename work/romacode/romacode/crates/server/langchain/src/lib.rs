//! # langchain-rust
//!
//! LangChain rebuilt in pure Rust — the agent-core kit:
//! LCEL runnables, prompt templates, chat models (OpenAI-compatible),
//! tool-calling agents with memory, and basic RAG (text splitter,
//! embeddings, in-memory vector store).
//!
//! ## Module map (Python LangChain → this crate)
//!
//! | Python | Rust |
//! |---|---|
//! | `langchain_core.runnables` | [`runnables`] |
//! | `langchain_core.prompts` | [`prompts`] |
//! | `langchain_core.messages` | [`messages`] |
//! | `langchain_core.language_models` | [`llms`] |
//! | `langchain_core.output_parsers` | [`output_parsers`] |
//! | `langchain_core.callbacks` | [`callbacks`] |
//! | `langchain.agents` | [`agents`] |
//! | `langchain.memory` | [`memory`] |
//! | `langchain.text_splitter` | [`text_splitter`] |
//! | `langchain_core.embeddings` | [`embeddings`] |
//! | `langchain_core.vectorstores` | [`vectorstores`] |
//! | `langchain.chains` | [`agents::LLMChain`] |
//!
//! ## Quickstart
//!
//! ```no_run
//! use std::collections::HashMap;
//! use std::sync::Arc;
//! use langchain_rust::prelude::*;
//!
//! # async fn demo() -> langchain_rust::errors::Result<()> {
//! let model: Arc<dyn BaseChatModel> = Arc::new(
//!     ChatOpenAI::builder()
//!         .model("gpt-4o-mini")
//!         .api_key(std::env::var("OPENAI_API_KEY").unwrap())
//!         .build()?,
//! );
//!
//! let prompt = ChatPromptTemplate::from_system_and_human("You are {role}.", "{question}");
//! let mut vars = HashMap::new();
//! vars.insert("role".into(), PromptValue::from("a Rust wizard"));
//! vars.insert("question".into(), PromptValue::from("Why is Rust memory-safe?"));
//! let messages = prompt.format(&vars)?;
//! let answer = model.generate(&messages).await?;
//! println!("{}", answer.text());
//! # Ok(())
//! # }
//! ```

pub mod agents;
pub mod callbacks;
pub mod cli;
pub mod embeddings;
pub mod errors;
pub mod llms;
pub mod memory;
pub mod messages;
pub mod output_parsers;
pub mod prompts;
pub mod runnables;
pub mod text_splitter;
pub mod vectorstores;

/// Everything you usually need, in one import: `use langchain_rust::prelude::*;`.
pub mod prelude {
    pub use crate::agents::tools::{Tool, ToolFn};
    pub use crate::agents::{
        calculator_tool, list_dir_tool, read_file_tool, shell_tool, write_file_tool, AgentExecutor, LLMChain, Tool as AgentTool,
    };
    pub use crate::callbacks::{CallbackHandler, CallbackManager, VerboseHandler};
    pub use crate::embeddings::openai::OpenAIEmbeddings;
    pub use crate::embeddings::{cosine_similarity, Embeddings};
    pub use crate::errors::{LangchainError, Result};
    pub use crate::llms::openai::ChatOpenAI;
    pub use crate::llms::{BaseChatModel, FakeChatModel, TokenUsage};
    pub use crate::memory::{ConversationBufferMemory, Memory, WindowBufferMemory};
    pub use crate::messages::{Message, ToolCall, ToolSpec};
    pub use crate::output_parsers::{JsonOutputParser, StrOutputParser};
    pub use crate::prompts::{ChatPromptTemplate, ChatSlot, PromptTemplate, PromptValue};
    pub use crate::runnables::{
        lambda, Runnable, RunnableExt, RunnableLambda, RunnableMap, RunnablePassthrough, RunnableSequence,
    };
    pub use crate::text_splitter::{Document, RecursiveCharacterTextSplitter};
    pub use crate::vectorstores::{InMemoryVectorStore, Retriever};
}

pub use errors::{LangchainError, Result};
