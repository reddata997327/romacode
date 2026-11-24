//! Tool-calling agent loop, fully offline with a scripted model.
//!
//! The `FakeChatModel` returns a calculator tool call on the first turn and a
//! final answer on the second — exactly what a real OpenAI-compatible server
//! would do. Run: `cargo run --example agent_demo`

use std::sync::Arc;

use langchain_rust::agents::{calculator_tool, AgentExecutor};
use langchain_rust::errors::Result;
use langchain_rust::llms::{BaseChatModel, FakeChatModel};
use langchain_rust::memory::ConversationBufferMemory;
use langchain_rust::messages::{Message, ToolCall};

#[tokio::main]
async fn main() -> Result<()> {
    // Script: turn 1 -> tool call, turn 2 -> final answer.
    let model: Arc<dyn BaseChatModel> = Arc::new(FakeChatModel::new(vec![
        Message::AI {
            content: None,
            tool_calls: vec![ToolCall {
                id: "call_1".into(),
                name: "calculator".into(),
                args: serde_json::json!({ "expression": "(199+1)*3" }),
            }],
        },
        Message::ai("The bill comes to 600."),
    ]));

    let mut agent = AgentExecutor::builder(model)
        .tool(calculator_tool())
        .memory(Box::new(ConversationBufferMemory::new()))
        .system_prompt("You are a billing assistant. Use the calculator for any math.")
        .max_iterations(6)
        .verbose(true) // traces go to stderr, like Python's verbose=True
        .build();

    let answer = agent.invoke("We sold 3 units at 199 each — total?").await?;
    println!("\nfinal answer: {answer}");

    // Memory now holds the user/assistant turns of this exchange.
    println!("memory entries: {}", agent.memory().unwrap().load().len());
    Ok(())
}
