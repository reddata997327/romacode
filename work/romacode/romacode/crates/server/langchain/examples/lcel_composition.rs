//! LCEL composition — pipes, lambdas, parallel map — all offline (no API key).
//!
//! Run: `cargo run --example lcel_composition`

use std::collections::HashMap;
use std::sync::Arc;

use langchain_rust::prelude::*;

#[tokio::main]
async fn main() -> Result<()> {
    // 1. Pipe: lambda -> lambda -> lambda  (Python: r1 | r2 | r3)
    let chain = lambda(|x: i32| async move { Ok(x * 2) })
        .pipe(lambda(|x: i32| async move { Ok(x + 10) }))
        .pipe(lambda(|x: i32| async move { Ok(format!("result = {x}")) }));

    println!("pipe:        {}", chain.invoke(5).await?);
    println!("batch:       {:?}", chain.batch(vec![1, 2, 3]).await?);

    // 2. RunnableMap: fan the same input out to several runnables in parallel.
    let fanout = RunnableMap::<i32, i32>::new()
        .with(lambda(|x: i32| async move { Ok(x * x) }))
        .with(lambda(|x: i32| async move { Ok(-x) }));
    println!("parallel:    {:?}", fanout.invoke(7).await?);

    // 3. Prompt -> FakeChatModel -> parser, wired as an LLMChain.
    let model: Arc<dyn BaseChatModel> =
        Arc::new(FakeChatModel::new(vec![Message::ai("42")]));
    let prompt = ChatPromptTemplate::from_system_and_human("Be terse.", "What is {q}?");

    let mut vars = HashMap::new();
    vars.insert("q".to_string(), PromptValue::from("the answer to everything"));
    let chain = LLMChain::new(prompt, model);
    println!("llm chain:   {}", chain.invoke(vars).await?);

    // 4. JSON output parser extracting fenced JSON from a model reply.
    let parser = JsonOutputParser;
    let parsed = parser
        .invoke("Sure:\n```json\n{\"ok\": true}\n```".to_string())
        .await?;
    println!("parsed json: {parsed}");

    Ok(())
}
