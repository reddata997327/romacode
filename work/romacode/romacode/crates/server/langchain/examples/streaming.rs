//! Streaming chat against a real OpenAI-compatible endpoint (needs
//! OPENAI_API_KEY). Prints tokens as they arrive plus the token usage.
//!
//! Run: `OPENAI_API_KEY=sk-... cargo run --example streaming`

use futures::StreamExt;
use langchain_rust::llms::openai::ChatOpenAI;
use langchain_rust::llms::BaseChatModel;
use langchain_rust::messages::Message;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let api_key = match std::env::var("OPENAI_API_KEY") {
        Ok(k) if !k.is_empty() => k,
        _ => anyhow::bail!("set OPENAI_API_KEY first"),
    };
    let base_url = std::env::var("OPENAI_BASE_URL").unwrap_or_default();

    let mut builder = ChatOpenAI::builder().model("gpt-4o-mini").api_key(api_key);
    if !base_url.is_empty() {
        builder = builder.base_url(base_url);
    }
    let model = builder.build()?;

    let messages = vec![
        Message::system("You are a Rust evangelist. Answer in one short paragraph."),
        Message::human("Why should an AI agent CLI be written in Rust?"),
    ];

    let mut stream = model.stream_text(&messages);
    while let Some(chunk) = stream.next().await {
        print!("{}", chunk?);
    }
    println!();

    if let Some(usage) = model.usage() {
        eprintln!("[usage] prompt={} completion={} total={}", usage.prompt_tokens, usage.completion_tokens, usage.total_tokens);
    }
    Ok(())
}
