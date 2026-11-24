//! `langchain-cli` — a REPL agent CLI built entirely on the library in this
//! crate. Subcommands:
//!
//! - `chat`   interactive agent shell (tools + memory + verbose traces)
//! - `ask`    one-shot question, streamed token-by-token
//! - `tools`  list built-in tools


use std::sync::Arc;

use anyhow::{Context, Result};
use clap::{Parser, Subcommand};
use futures::StreamExt;

use crate::agents::{
    calculator_tool, list_dir_tool, read_file_tool, shell_tool, write_file_tool, AgentExecutor,
};
use crate::llms::openai::ChatOpenAI;
use crate::llms::BaseChatModel;
use crate::memory::ConversationBufferMemory;
use crate::messages::{Message, ToolCall};

const DEFAULT_MODEL: &str = "gpt-4o-mini";
const DEFAULT_BASE_URL: &str = "https://api.openai.com/v1";

#[derive(Parser)]
#[command(
    name = "langchain-cli",
    version,
    about = "LangChain rebuilt in Rust — REPL agent CLI"
)]
struct Cli {
    #[command(subcommand)]
    command: Commands,
}

#[derive(Subcommand)]
enum Commands {
    /// Interactive agent REPL: type a task, the agent plans, calls tools, answers.
    Chat {
        /// Model id (env LANGCHAIN_MODEL / OPENAI_MODEL, default gpt-4o-mini).
        #[arg(long)]
        model: Option<String>,
        /// OpenAI-compatible base URL (env OPENAI_BASE_URL).
        #[arg(long)]
        base_url: Option<String>,
        /// Run the model without any tools.
        #[arg(long = "no-tools")]
        no_tools: bool,
        /// Print agent traces (LLM calls, tool runs) to stderr.
        #[arg(long)]
        verbose: bool,
        /// Enable the `run_shell` tool (arbitrary commands — use with care).
        #[arg(long)]
        allow_shell: bool,
        /// Offline demo: scripted model, no API key or network needed.
        #[arg(long)]
        demo: bool,
    },
    /// One-shot question, answer streamed to the terminal.
    Ask {
        /// The question in quotes.
        question: String,
        #[arg(long)]
        model: Option<String>,
        #[arg(long)]
        base_url: Option<String>,
        /// Optional system prompt.
        #[arg(long)]
        system: Option<String>,
        /// Disable token streaming (print answer at once).
        #[arg(long)]
        no_stream: bool,
    },
    /// List the built-in tools the agent can use.
    Tools,
}

fn resolve(model_flag: Option<&str>, base_url_flag: Option<&str>) -> (String, String, String) {
    let model = model_flag
        .map(|s| s.to_string())
        .or_else(|| std::env::var("LANGCHAIN_MODEL").ok())
        .or_else(|| std::env::var("OPENAI_MODEL").ok())
        .unwrap_or_else(|| DEFAULT_MODEL.to_string());
    let base_url = base_url_flag
        .map(|s| s.to_string())
        .or_else(|| std::env::var("OPENAI_BASE_URL").ok())
        .unwrap_or_else(|| DEFAULT_BASE_URL.to_string());
    let api_key = std::env::var("OPENAI_API_KEY").unwrap_or_default();
    (model, base_url, api_key)
}

fn build_tools(allow_shell: bool) -> Vec<crate::agents::Tool> {
    let mut tools = vec![calculator_tool(), read_file_tool(), write_file_tool(), list_dir_tool()];
    if allow_shell {
        tools.push(shell_tool(true, std::time::Duration::from_secs(30)));
    }
    tools
}

fn build_model(
    model_flag: Option<&str>,
    base_url_flag: Option<&str>,
) -> Result<Arc<dyn BaseChatModel>> {
    let (model, base_url, api_key) = resolve(model_flag, base_url_flag);
    if api_key.is_empty() {
        anyhow::bail!(
            "OPENAI_API_KEY is not set. Export it (or use `--base-url` with a local server \
             and any dummy key), or run `langchain-cli chat --demo` for an offline demo."
        );
    }
    let m = ChatOpenAI::builder()
        .model(model)
        .api_key(api_key)
        .base_url(base_url)
        .build()
        .context("building ChatOpenAI")?;
    Ok(Arc::new(m))
}

/// Offline scripted agent model: demonstrates the tool-calling loop with zero
/// network. Math-looking input triggers a calculator tool call; the follow-up
/// turn (after the tool result) answers using the tool output.
struct DemoChatModel;

impl DemoChatModel {
    fn new() -> Self {
        Self
    }
}

/// Pull the longest digit-bearing math run out of a sentence so the demo
/// model can hand the calculator something valid.
fn extract_expression(q: &str) -> Option<String> {
    let digit_count = |s: &str| s.chars().filter(|c| c.is_ascii_digit()).count();
    let mut best = String::new();
    let mut cur = String::new();
    for c in q.chars() {
        if c.is_ascii_digit() || "+-*/^().% ".contains(c) {
            cur.push(c);
        } else {
            if digit_count(&cur) > digit_count(&best) {
                best = cur.clone();
            }
            cur.clear();
        }
    }
    if digit_count(&cur) > digit_count(&best) {
        best = cur;
    }
    let t = best.trim().to_string();
    if digit_count(&t) >= 1 && t.chars().any(|c| "+-*/^%".contains(c)) {
        Some(t)
    } else {
        None
    }
}

#[async_trait::async_trait]
impl BaseChatModel for DemoChatModel {
    fn model_name(&self) -> &str {
        "demo-model"
    }

    async fn generate(&self, messages: &[Message]) -> crate::errors::Result<Message> {
        // Inspect the tail of the conversation to decide the next scripted move.
        let last = messages.last();
        if let Some(Message::Tool { name, content, .. }) = last {
            return Ok(Message::ai(format!("The calculator says: {content} (via {name})")));
        }
        let question = messages
            .iter()
            .rev()
            .find_map(|m| match m {
                Message::Human { content } => Some(content.clone()),
                _ => None,
            })
            .unwrap_or_default();

        // Anything that smells mathy goes through the calculator tool.
        let mathy = question
            .chars()
            .filter(|c| c.is_ascii_digit() || "+-*/^().%".contains(*c))
            .count() as f64
            / question.chars().count().max(1) as f64;
        if let Some(expr) = extract_expression(&question)
            .filter(|_| question.contains("calculate") || question.contains("math") || mathy > 0.4)
        {
            return Ok(Message::AI {
                content: None,
                tool_calls: vec![ToolCall {
                    id: "call_demo_1".into(),
                    name: "calculator".into(),
                    args: serde_json::json!({ "expression": expr }),
                }],
            });
        }
        Ok(Message::ai(format!(
            "[demo] I received: “{question}”. I am a scripted model — run without --demo and \
             set OPENAI_API_KEY to talk to a real model."
        )))
    }

    async fn generate_with_tools(
        &self,
        messages: &[Message],
        _tools: &[crate::messages::ToolSpec],
    ) -> crate::errors::Result<Message> {
        self.generate(messages).await
    }
}

async fn run_chat(
    model_flag: Option<&str>,
    base_url_flag: Option<&str>,
    no_tools: bool,
    verbose: bool,
    allow_shell: bool,
    demo: bool,
) -> Result<()> {
    let model: Arc<dyn BaseChatModel> = if demo {
        Arc::new(DemoChatModel::new())
    } else {
        build_model(model_flag, base_url_flag)?
    };

    let mut builder = AgentExecutor::builder(model)
        .system_prompt(
            "You are a helpful AI agent running inside a terminal REPL. \
             Use tools when they help; be concise.",
        )
        .memory(Box::new(ConversationBufferMemory::new()))
        .verbose(verbose)
        .max_iterations(12);

    if !no_tools {
        builder = builder.tools(build_tools(allow_shell));
    }
    let mut agent = builder.build();

    println!("langchain-cli chat — type a task, /help for commands, /exit to quit");
    println!(
        "mode: {} | tools: {} | model: {}",
        if demo { "DEMO (offline)" } else { "live" },
        if no_tools { "off" } else { "on" },
        "auto"
    );

    let stdin = tokio::io::BufReader::new(tokio::io::stdin());
    use tokio::io::AsyncBufReadExt;
    let mut lines = stdin.lines();

    loop {
        print!("\x1b[32myou>\x1b[0m ");
        use std::io::Write as _;
        let _ = std::io::stdout().flush();

        let Some(line) = lines.next_line().await? else {
            println!();
            break;
        };
        let line = line.trim().to_string();
        if line.is_empty() {
            continue;
        }
        match line.as_str() {
            "/exit" | "/quit" | "/q" => {
                println!("bye");
                break;
            }
            "/help" => {
                println!("commands: /help /memory /clear /exit — anything else goes to the agent");
                continue;
            }
            "/clear" => {
                agent.clear_memory();
                println!("memory cleared");
                continue;
            }
            "/memory" => {
                match agent.memory() {
                    Some(mem) => {
                        for m in mem.load() {
                            println!("  {:?}: {}", role_of(&m), m.text());
                        }
                    }
                    None => println!("  (no memory attached)"),
                }
                continue;
            }
            _ => {}
        }

        match agent.invoke(&line).await {
            Ok(answer) => println!("\x1b[36mai>\x1b[0m {answer}"),
            Err(e) => println!("\x1b[31merror>\x1b[0m {e}"),
        }
    }
    Ok(())
}

fn role_of(m: &Message) -> &'static str {
    match m {
        Message::System { .. } => "system",
        Message::Human { .. } => "human",
        Message::AI { .. } => "ai",
        Message::Tool { .. } => "tool",
    }
}

async fn run_ask(
    question: &str,
    model_flag: Option<&str>,
    base_url_flag: Option<&str>,
    system: Option<&str>,
    no_stream: bool,
) -> Result<()> {
    let model = build_model(model_flag, base_url_flag)?;
    let mut messages: Vec<Message> = Vec::new();
    if let Some(sys) = system {
        messages.push(Message::system(sys));
    }
    messages.push(Message::human(question));



    if no_stream {
        let answer = model.generate(&messages).await?;
        println!("{}", answer.text());
    } else {
        let mut stream = model.stream_text(&messages);
        while let Some(chunk) = stream.next().await {
            match chunk {
                Ok(t) => print!("{t}"),
                Err(e) => {
                    println!("\n\x1b[31merror>\x1b[0m {e}");
                    return Ok(());
                }
            }
        }
        println!();
    }
    if let Some(usage) = model.usage() {
        eprintln!(
            "[usage] prompt={} completion={} total={}",
            usage.prompt_tokens, usage.completion_tokens, usage.total_tokens
        );
    }
    Ok(())
}

fn run_tools_list() {
    println!("built-in agent tools:");
    for (name, desc) in [
        ("calculator", "evaluates math expressions (+ - * / % ^, sqrt, abs, min, max…)"),
        ("read_file", "reads a UTF-8 text file"),
        ("write_file", "writes text to a file (creates parents)"),
        ("list_dir", "lists a directory"),
        ("run_shell", "runs `sh -c` commands — only with --allow-shell"),
    ] {
        println!("  {name:<12} {desc}");
    }
}

/// CLI entry point called from `main`.
pub async fn run() -> Result<()> {
    let cli = Cli::parse();
    match cli.command {
        Commands::Chat {
            model,
            base_url,
            no_tools,
            verbose,
            allow_shell,
            demo,
        } => {
            run_chat(
                model.as_deref(),
                base_url.as_deref(),
                no_tools,
                verbose,
                allow_shell,
                demo,
            )
            .await
        }
        Commands::Ask {
            question,
            model,
            base_url,
            system,
            no_stream,
        } => {
            run_ask(
                &question,
                model.as_deref(),
                base_url.as_deref(),
                system.as_deref(),
                no_stream,
            )
            .await
        }
        Commands::Tools => {
            run_tools_list();
            Ok(())
        }
    }
}
