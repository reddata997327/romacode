//! RAG pipeline, fully offline: split -> embed (deterministic toy embedder)
//! -> vector store -> retriever -> answer.
//!
//! Run: `cargo run --example rag_demo`

use std::sync::Arc;

use async_trait::async_trait;
use langchain_rust::prelude::*;

/// Toy embedder: bag-of-words hashed into 64 dims. Swap in `OpenAIEmbeddings`
/// (builder below, commented) for real retrieval.
struct WordHashEmbedder;

#[async_trait]
impl Embeddings for WordHashEmbedder {
    async fn embed_query(&self, text: &str) -> Result<Vec<f32>> {
        Ok(self.hash(text))
    }

    async fn embed_documents(&self, texts: &[String]) -> Result<Vec<Vec<f32>>> {
        Ok(texts.iter().map(|t| self.hash(t)).collect())
    }
}

impl WordHashEmbedder {
    fn hash(&self, text: &str) -> Vec<f32> {
        let mut v = vec![0f32; 64];
        for w in text.to_lowercase().split(|c: char| !c.is_alphanumeric()) {
            if w.is_empty() {
                continue;
            }
            let h = w.bytes().fold(5381usize, |acc, b| {
                (acc.wrapping_mul(33).wrapping_add(b as usize)) % 64
            });
            v[h] += 1.0;
        }
        v
    }
}

#[tokio::main]
async fn main() -> Result<()> {
    // -- Real embeddings (needs OPENAI_API_KEY) --
    // let embedder: Arc<dyn Embeddings> = Arc::new(
    //     OpenAIEmbeddings::builder()
    //         .model("text-embedding-3-small")
    //         .api_key(std::env::var("OPENAI_API_KEY").unwrap())
    //         .build()?,
    // );

    let embedder: Arc<dyn Embeddings> = Arc::new(WordHashEmbedder);

    // 1. Split a long document on natural boundaries.
    let splitter = RecursiveCharacterTextSplitter::new(220, 40);
    let raw = format!(
        "Rust achieves memory safety without a garbage collector.\n\n\
         The borrow checker enforces ownership rules at compile time.\n\n\
         Ownership means each value has exactly one owner.\n\n\
         Lifetimes describe how long references remain valid.\n\n\
         Zero-cost abstractions let Rust match handwritten C performance.\n\n\
         Pattern matching with match expressions covers all cases exhaustively."
    );
    let chunks = splitter.split(&raw);
    println!("split into {} chunks", chunks.len());

    // 2. Embed + index the chunks.
    let store = InMemoryVectorStore::new();
    let docs: Vec<Document> = chunks
        .iter()
        .enumerate()
        .map(|(i, c)| Document::with_source(c.clone(), format!("rust-notes.md#chunk-{i}")))
        .collect();
    store.add_documents(&docs, embedder.as_ref()).await?;
    println!("indexed {} documents", store.len()?);

    // 3. Retrieve the top-2 chunks for a question.
    let retriever = store.as_retriever(embedder.clone(), 2);
    let question = "How does Rust stay memory safe?";
    let hits = retriever.retrieve_scored(question).await?;
    for (d, score) in &hits {
        println!("  score={score:.3} src={:?} :: {}", d.metadata["source"], d.page_content);
    }

    // 4. Feed retrieved context to the model (offline fake here).
    let context: String = hits
        .iter()
        .map(|(d, _)| d.page_content.clone())
        .collect::<Vec<_>>()
        .join("\n");
    let model: Arc<dyn BaseChatModel> = Arc::new(FakeChatModel::new(vec![Message::ai(format!(
        "Answer (grounded in {len} chunks): the borrow checker enforces ownership.",
        len = hits.len()
    ))]));
    let answer = model
        .generate(&[Message::human(format!("Context:\n{context}\n\nQuestion: {question}"))])
        .await?;
    println!("\nanswer: {}", answer.text());

    Ok(())
}
