# langchain-rust

**LangChain, rebuilt in pure Rust** — the agent-core kit: LCEL runnables, prompt
templates, chat models, tool-calling agents with memory, and basic RAG. Zero
Python, zero bloat, one dependency tree, plus a built-in REPL agent CLI.

```
cargo build            # library + langchain-cli binary
cargo test             # 49 unit + 5 wiremock integration + doctests
cargo run -- chat --demo   # offline agent REPL, no API key needed
```

---

## Module map (Python LangChain → this crate)

| Python LangChain | langchain-rust | Notes |
|---|---|---|
| `langchain_core.runnables.Runnable` | `runnables::Runnable` | async `invoke` / `batch`; `.pipe()` = LCEL `\|` |
| `RunnableLambda` / `RunnablePassthrough` | `runnables::{lambda, RunnablePassthrough}` | closures become runnables |
| `RunnableParallel` | `runnables::RunnableMap` | fan-out one input to many runnables concurrently |
| `langchain_core.prompts.*` | `prompts::{PromptTemplate, ChatPromptTemplate}` | `{var}` f-string syntax, `{{` escapes |
| `MessagesPlaceholder` | `ChatSlot::Placeholder` | splices history into a chat prompt |
| `langchain_core.messages` | `messages::Message` | System/Human/AI/Tool + `ToolCall`, OpenAI JSON serialisation |
| `langchain_core.language_models.BaseChatModel` | `llms::BaseChatModel` | `generate`, `generate_with_tools`, `stream_text` |
| `ChatOpenAI` | `llms::openai::ChatOpenAI` | any `/v1/chat/completions` server (OpenAI, LM Studio, vLLM, llama.cpp) |
| `langchain_core.output_parsers` | `output_parsers::{StrOutputParser, JsonOutputParser}` | fenced/prose-tolerant JSON extraction |
| `langchain_core.callbacks` | `callbacks::{CallbackHandler, VerboseHandler}` | `verbose=True` equivalent |
| `langchain.agents.AgentExecutor` | `agents::AgentExecutor` | OpenAI tool-calling loop, iteration budget |
| `@tool` / `BaseTool` | `agents::tools::{Tool, tool_fn}` | JSON-schema parameters + async closure |
| `langchain.memory` | `memory::{ConversationBufferMemory, WindowBufferMemory}` | |
| `langchain.chains.LLMChain` | `agents::LLMChain` | prompt → model → string, as a `Runnable` |
| `langchain.text_splitter` | `text_splitter::RecursiveCharacterTextSplitter` | chunk_size + overlap |
| `langchain_core.embeddings` | `embeddings::{Embeddings, OpenAIEmbeddings}` | `/v1/embeddings` |
| `langchain_core.vectorstores` | `vectorstores::{InMemoryVectorStore, Retriever}` | cosine top-k, retriever is a `Runnable` |
| `FakeListChatModel` (tests) | `llms::FakeChatModel` | scripted offline model |

## Architecture

```
┌────────────────────────── langchain-cli (REPL agent) ──────────────────────────┐
│  chat (tools + memory)   ask (streamed)   tools (list)                         │
└───────────────▲───────────────────────────────────────────────▲────────────────┘
                │ AgentExecutor                                 │
│ ┌─────────────┴────────────────────────────────────────────────────────────┐  │
│ │ agents       AgentExecutor: plan → tool_calls → run tools → feed back    │  │
│ │ tools        Tool {ToolSpec, async ToolFn} + calculator/file/shell tools │  │
│ │ memory       ConversationBufferMemory / WindowBufferMemory               │  │
│ └──────────────▲───────────────────────────────────────────────────────────┘  │
│                │ Arc<dyn BaseChatModel>                                       │
│ ┌──────────────┴───────────────────────────────────────────────────────────┐  │
│ │ llms         BaseChatModel trait · ChatOpenAI (SSE streaming, tool calls)│  │
│ │ messages     Message::{System,Human,AI,Tool} → OpenAI wire format        │  │
│ └──────────────────────────────────────────────────────────────────────────  │
│ ┌──────────────────────────────────────────────────────────────────────────┐ │
│ │ runnables     Runnable<I,O> · pipe() · RunnableMap · Passthrough         │ │
│ │ prompts       PromptTemplate · ChatPromptTemplate · PromptValue          │ │
│ │ output_parsers / callbacks / errors                                     │ │
│ └──────────────────────────────────────────────────────────────────────────┘ │
│ ┌──────────────────────────────────────────────────────────────────────────┐ │
│ │ RAG: text_splitter → embeddings (OpenAI) → InMemoryVectorStore → Retriever│ │
│ └──────────────────────────────────────────────────────────────────────────┘ │
└───────────────────────────────────────────────────────────────────────────────┘
```

## Quickstart (library)

```rust
use std::collections::HashMap;
use std::sync::Arc;
use langchain_rust::prelude::*;

# async fn demo() -> langchain_rust::errors::Result<()> {
let model: Arc<dyn BaseChatModel> = Arc::new(
    ChatOpenAI::builder()
        .model("gpt-4o-mini")
        .api_key(std::env::var("OPENAI_API_KEY")?)
        .build()?,
);

// LCEL: prompt | model
let prompt = ChatPromptTemplate::from_system_and_human("You are {role}.", "{question}");
let mut vars = HashMap::new();
vars.insert("role".into(), PromptValue::from("a Rust wizard"));
vars.insert("question".into(), PromptValue::from("Why is Rust memory-safe?"));
let messages = prompt.format(&vars)?;
let answer = model.generate(&messages).await?;
println!("{}", answer.text());
# Ok(())
# }
```

### Agent with tools and memory

```rust
use std::sync::Arc;
use langchain_rust::prelude::*;

# async fn demo() -> langchain_rust::errors::Result<()> {
let model: Arc<dyn BaseChatModel> = Arc::new(
    ChatOpenAI::builder().model("gpt-4o-mini")
        .api_key(std::env::var("OPENAI_API_KEY")?).build()?,
);

let mut agent = AgentExecutor::builder(model)
    .tool(calculator_tool())
    .tool(read_file_tool())
    .tool(write_file_tool())
    .memory(Box::new(ConversationBufferMemory::new()))
    .system_prompt("You are a terminal AI agent. Prefer tools over guessing.")
    .max_iterations(12)
    .verbose(true)          // traces to stderr, like verbose=True
    .build();

let answer = agent.invoke("What is (12*35)*1.05? Use the calculator.").await?;
# Ok(())
# }
```

Custom tools are one function each:

```rust
use langchain_rust::prelude::*;
let weather = tool_fn(
    "weather",
    "Current weather for a city",
    serde_json::json!({
        "type": "object",
        "properties": {"city": {"type": "string"}},
        "required": ["city"]
    }),
    |args| async move { Ok(format!("sunny in {}", args["city"].as_str()?)) },
);
```

## The CLI (`langchain-cli`)

```text
langchain-cli chat                # interactive agent REPL (tools + memory)
langchain-cli chat --verbose      # print every LLM call / tool run to stderr
langchain-cli chat --no-tools     # plain chat, no tool binding
langchain-cli chat --allow-shell  # also enable the run_shell tool
langchain-cli chat --demo         # offline scripted agent, no API key
langchain-cli ask "Why Rust?"     # one-shot streamed answer
langchain-cli tools               # list built-in tools
```

| Env var | Meaning | Default |
|---|---|---|
| `OPENAI_API_KEY` | Bearer token (any string works for local servers) | — |
| `OPENAI_BASE_URL` | Any OpenAI-compatible endpoint | `https://api.openai.com/v1` |
| `LANGCHAIN_MODEL` / `OPENAI_MODEL` | model id | `gpt-4o-mini` |

REPL commands: `/help`, `/memory` (dump history), `/clear` (reset memory),
`/exit`. Built-in tools: `calculator`, `read_file`, `write_file`, `list_dir`,
`run_shell` (opt-in via `--allow-shell`).

Point it at a local model with zero setup:

```bash
OPENAI_BASE_URL=http://localhost:11434/v1 OPENAI_API_KEY=ollama \
  langchain-cli chat --model llama3.1
```

## Examples (all runnable from this repo)

| Example | Needs network? | Shows |
|---|---|---|
| `cargo run --example lcel_composition` | no | pipe chains, batch, parallel fan-out, LLMChain, JSON parser |
| `cargo run --example agent_demo` | no | scripted tool-calling agent loop + memory |
| `cargo run --example rag_demo` | no | split → embed → vector store → retriever → grounded answer |
| `cargo run --example streaming` | yes | token-by-token SSE streaming + usage |

## Design notes

- **Everything is a `Runnable`.** Prompts, chains, retrievers and parsers share
  one trait (`invoke` / `batch`), so composition type-checks at compile time —
  LCEL's `prompt | model | parser` becomes `prompt.pipe(model).pipe(parser)`.
- **The agent loop is the OpenAI tool protocol.** The executor binds
  `ToolSpec`s, feeds `tool_calls` results back as `role: "tool"` messages, and
  stops on a plain-text turn. Tool *errors* go back to the model as
  `Error: …` results so it can self-correct; only transport failures abort.
- **Streaming is real SSE parsing**, including the `[DONE]` sentinel and
  per-provider usage accounting where offered.
- **Deliberately not ported** (agent-core scope): document loaders (PDF/HTML),
  few-shot selectors, LangGraph, LangServe, multi-vector stores. The seams for
  all of them are the existing traits (`Embeddings`, `Memory`, `Runnable`,
  `BaseChatModel`).

## Project layout

```text
src/
├── lib.rs                 # module map + prelude
├── errors.rs              # LangchainError (thiserror)
├── messages.rs            # Message / ToolCall / ToolSpec + OpenAI JSON
├── callbacks.rs           # CallbackHandler, CallbackManager, VerboseHandler
├── runnables.rs           # LCEL: Runnable, pipe, RunnableMap, Passthrough
├── prompts.rs             # templates + PromptValue
├── memory.rs              # buffer + window memory
├── output_parsers.rs      # Str / Json parsers
├── llms/
│   ├── mod.rs             # BaseChatModel trait, TokenUsage, FakeChatModel
│   └── openai.rs          # ChatOpenAI: completions, SSE stream, tool calls
├── agents/
│   ├── mod.rs             # AgentExecutor loop + LLMChain
│   ├── tools.rs           # Tool, tool_fn, schema validation
│   └── tools_builtin.rs   # calculator (hand-rolled parser), file/shell tools
├── embeddings/
│   ├── mod.rs             # Embeddings trait + cosine similarity
│   └── openai.rs          # OpenAIEmbeddings
├── vectorstores.rs        # InMemoryVectorStore + Retriever (Runnable)
├── text_splitter.rs       # RecursiveCharacterTextSplitter + Document
├── cli.rs                 # REPL agent CLI implementation
└── main.rs                # binary entry
tests/openai_integration.rs  # wiremock: full agent loop against a mock server
examples/                    # lcel_composition, agent_demo, rag_demo, streaming
```

## Testing

```bash
cargo test            # 49 unit tests + 5 integration tests + doctests
```

The integration suite spins up a local mock OpenAI server (wiremock) and runs
the **real** `ChatOpenAI` client through: plain completion with auth checks,
a full agent tool-calling loop, SSE stream parsing, embeddings indexing, and
HTTP error mapping — no network, no keys.

License: MIT.
