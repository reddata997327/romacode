//! Embeddings — the Rust mirror of `langchain_core.embeddings` (`Embeddings`
//! trait + `OpenAIEmbeddings` implementation).

pub mod openai;

use async_trait::async_trait;

use crate::errors::Result;

/// Turns text into a dense vector. Mirrors the Python `Embeddings` interface:
/// `embed_query` for search keys, `embed_documents` for corpus chunks.
#[async_trait]
pub trait Embeddings: Send + Sync {
    /// Embed a single query string.
    async fn embed_query(&self, text: &str) -> Result<Vec<f32>>;

    /// Embed many documents (batched by the implementation).
    async fn embed_documents(&self, texts: &[String]) -> Result<Vec<Vec<f32>>>;
}

/// Cosine similarity between two vectors. Returns 0 for zero-length inputs.
pub fn cosine_similarity(a: &[f32], b: &[f32]) -> f32 {
    if a.len() != b.len() || a.is_empty() {
        return 0.0;
    }
    let dot: f32 = a.iter().zip(b.iter()).map(|(x, y)| x * y).sum();
    let na: f32 = a.iter().map(|x| x * x).sum::<f32>().sqrt();
    let nb: f32 = b.iter().map(|x| x * x).sum::<f32>().sqrt();
    if na == 0.0 || nb == 0.0 {
        return 0.0;
    }
    dot / (na * nb)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cosine_basics() {
        assert!((cosine_similarity(&[1.0, 0.0], &[1.0, 0.0]) - 1.0).abs() < 1e-6);
        assert!((cosine_similarity(&[1.0, 0.0], &[0.0, 1.0])).abs() < 1e-6);
        assert!((cosine_similarity(&[1.0, 0.0], &[-1.0, 0.0]) + 1.0).abs() < 1e-6);
        assert_eq!(cosine_similarity(&[], &[]), 0.0);
        assert_eq!(cosine_similarity(&[1.0], &[1.0, 2.0]), 0.0);
    }
}
