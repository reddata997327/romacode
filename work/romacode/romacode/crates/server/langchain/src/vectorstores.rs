//! Vector stores — the Rust mirror of `langchain_core.vectorstores`
//! (`InMemoryVectorStore` + `as_retriever`).

use async_trait::async_trait;
use std::sync::Arc;
use std::sync::RwLock;

use crate::embeddings::{cosine_similarity, Embeddings};
use crate::errors::{LangchainError, Result};
use crate::runnables::Runnable;
use crate::text_splitter::Document;

struct StoredDoc {
    doc: Document,
    vector: Vec<f32>,
}

/// In-memory similarity-search store. Thread-safe via `RwLock`.
#[derive(Clone)]
pub struct InMemoryVectorStore {
    docs: Arc<RwLock<Vec<StoredDoc>>>,
}

impl Default for InMemoryVectorStore {
    fn default() -> Self {
        Self::new()
    }
}

impl std::fmt::Debug for InMemoryVectorStore {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let n = self.docs.read().map(|d| d.len()).unwrap_or(0);
        write!(f, "InMemoryVectorStore(docs={n})")
    }
}

impl InMemoryVectorStore {
    /// Empty store.
    pub fn new() -> Self {
        Self { docs: Arc::new(RwLock::new(Vec::new())) }
    }

    /// Embed `texts` with `embedder` and add them to the store.
    pub async fn add_texts(&self, texts: &[String], embedder: &dyn Embeddings) -> Result<usize> {
        let vectors = embedder.embed_documents(texts).await?;
        // Fewer embeddings than texts must be an ERROR — silently dropping
        // documents while reporting success hid data loss.
        if vectors.len() != texts.len() {
            return Err(LangchainError::Other(format!(
                "embedder returned {} vectors for {} texts",
                vectors.len(),
                texts.len()
            )));
        }
        // Validate EVERY vector against the first one (not just first() vs
        // first()) BEFORE storing anything, so a ragged batch fails whole.
        let expected = match vectors.first() {
            Some(v) => v.len(),
            None => return Ok(0),
        };
        if let Some(v) = vectors.iter().find(|v| v.len() != expected) {
            return Err(LangchainError::DimensionMismatch { expected, got: v.len() });
        }
        let mut guard = self
            .docs
            .write()
            .map_err(|_| LangchainError::Other("vector store lock poisoned".into()))?;
        if let Some(stored) = guard.first() {
            if stored.vector.len() != expected {
                return Err(LangchainError::DimensionMismatch {
                    expected: stored.vector.len(),
                    got: expected,
                });
            }
        }
        for (text, vector) in texts.iter().cloned().zip(vectors) {
            guard.push(StoredDoc { doc: Document::new(text), vector });
        }
        Ok(texts.len())
    }

    /// Add full documents (keeps metadata).
    pub async fn add_documents(&self, docs: &[Document], embedder: &dyn Embeddings) -> Result<usize> {
        let texts: Vec<String> = docs.iter().map(|d| d.page_content.clone()).collect();
        let vectors = embedder.embed_documents(&texts).await?;
        if vectors.len() != docs.len() {
            return Err(LangchainError::Other(format!(
                "embedder returned {} vectors for {} documents",
                vectors.len(),
                docs.len()
            )));
        }
        let expected = match vectors.first() {
            Some(v) => v.len(),
            None => return Ok(0),
        };
        if let Some(v) = vectors.iter().find(|v| v.len() != expected) {
            return Err(LangchainError::DimensionMismatch { expected, got: v.len() });
        }
        let mut guard = self
            .docs
            .write()
            .map_err(|_| LangchainError::Other("vector store lock poisoned".into()))?;
        if let Some(stored) = guard.first() {
            if stored.vector.len() != expected {
                return Err(LangchainError::DimensionMismatch {
                    expected: stored.vector.len(),
                    got: expected,
                });
            }
        }
        for (doc, vector) in docs.iter().cloned().zip(vectors) {
            guard.push(StoredDoc { doc, vector });
        }
        Ok(docs.len())
    }

    /// Top-`k` documents by cosine similarity to `query_embedding`.
    pub fn similarity_search_by_vector(&self, query_embedding: &[f32], k: usize) -> Result<Vec<(Document, f32)>> {
        let guard = self
            .docs
            .read()
            .map_err(|_| LangchainError::Other("vector store lock poisoned".into()))?;
        let mut scored: Vec<(Document, f32)> = guard
            .iter()
            .map(|s| (s.doc.clone(), cosine_similarity(query_embedding, &s.vector)))
            .collect();
        scored.sort_by(|a, b| b.1.partial_cmp(&a.1).unwrap_or(std::cmp::Ordering::Equal));
        scored.truncate(k);
        Ok(scored)
    }

    /// Embed `query` then return the top-`k` documents — `similarity_search`.
    pub async fn similarity_search(&self, query: &str, k: usize, embedder: &dyn Embeddings) -> Result<Vec<(Document, f32)>> {
        let qv = embedder.embed_query(query).await?;
        self.similarity_search_by_vector(&qv, k)
    }

    /// Number of stored documents.
    pub fn len(&self) -> Result<usize> {
        Ok(self
            .docs
            .read()
            .map_err(|_| LangchainError::Other("vector store lock poisoned".into()))?
            .len())
    }

    /// True when the store holds no documents.
    pub fn is_empty(&self) -> Result<bool> {
        Ok(self.len()? == 0)
    }

    /// Wrap this store into a retriever runnable (`as_retriever`).
    pub fn as_retriever(self, embedder: Arc<dyn Embeddings>, k: usize) -> Retriever {
        Retriever { store: self, embedder, k }
    }
}

/// `VectorStoreRetriever`: `Runnable<String, Vec<Document>>` that embeds the
/// query and returns the top-k documents (scores dropped).
pub struct Retriever {
    store: InMemoryVectorStore,
    embedder: Arc<dyn Embeddings>,
    k: usize,
}

impl std::fmt::Debug for Retriever {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "Retriever(k={})", self.k)
    }
}

impl Retriever {
    /// Return the top-k docs with their similarity scores.
    pub async fn retrieve_scored(&self, query: &str) -> Result<Vec<(Document, f32)>> {
        self.store.similarity_search(query, self.k, self.embedder.as_ref()).await
    }
}

#[async_trait]
impl Runnable<String, Vec<Document>> for Retriever {
    async fn invoke(&self, query: String) -> Result<Vec<Document>> {
        let scored = self.retrieve_scored(&query).await?;
        Ok(scored.into_iter().map(|(d, _)| d).collect())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;

    /// Deterministic embeddings: bag-of-words hashed into 64 dims. Shared words
    /// dominate similarity, which makes retrieval assertions predictable.
    struct HashEmbedder;

    fn hash_word(w: &str) -> usize {
        w.bytes().fold(5381usize, |h, b| (h.wrapping_mul(33).wrapping_add(b as usize)) % 64)
    }

    #[async_trait]
    impl Embeddings for HashEmbedder {
        async fn embed_query(&self, text: &str) -> Result<Vec<f32>> {
            self.embed_documents(&[text.to_string()]).await
                .and_then(|mut v| v.pop().ok_or_else(|| LangchainError::LlmParse("empty".into())))
        }

        async fn embed_documents(&self, texts: &[String]) -> Result<Vec<Vec<f32>>> {
            Ok(texts
                .iter()
                .map(|t| {
                    let mut v = vec![0f32; 64];
                    for w in t.to_lowercase().split(|c: char| !c.is_alphanumeric()) {
                        if w.is_empty() {
                            continue;
                        }
                        v[hash_word(w)] += 1.0;
                    }
                    v
                })
                .collect())
        }
    }

    #[tokio::test]
    async fn add_and_search() {
        let store = InMemoryVectorStore::new();
        let embedder = HashEmbedder;
        let texts: Vec<String> = vec![
            "the quick brown fox jumps".into(),
            "rust is a systems programming language".into(),
            "lazy dogs sleep all day".into(),
            "the fox and the dog are friends".into(),
        ];
        store.add_texts(&texts, &embedder).await.unwrap();
        assert_eq!(store.len().unwrap(), 4);

        let results = store.similarity_search("fox jumps", 2, &embedder).await.unwrap();
        assert_eq!(results.len(), 2);
        // Highest scored doc should mention the fox.
        assert!(results[0].0.page_content.contains("fox"));
        assert!(results[0].1 > results[1].1);
    }

    #[tokio::test]
    async fn retriever_is_runnable() {
        let store = InMemoryVectorStore::new();
        let embedder: Arc<dyn Embeddings> = Arc::new(HashEmbedder);
        store
            .add_texts(&["alpha beta".into(), "gamma delta".into()], embedder.as_ref())
            .await
            .unwrap();
        let retriever = store.as_retriever(embedder, 1);
        let docs = retriever.invoke("alpha".into()).await.unwrap();
        assert_eq!(docs.len(), 1);
        assert!(docs[0].page_content.contains("alpha"));
    }

    #[tokio::test]
    async fn documents_keep_metadata() {
        let store = InMemoryVectorStore::new();
        let embedder = HashEmbedder;
        let mut meta = HashMap::new();
        meta.insert("source".into(), "notes.md".into());
        let docs = vec![Document {
            page_content: "chunked content".into(),
            metadata: meta.clone(),
        }];
        store.add_documents(&docs, &embedder).await.unwrap();
        let hits = store.similarity_search("content", 1, &embedder).await.unwrap();
        assert_eq!(hits[0].0.metadata["source"], "notes.md");
    }

    /// Embedder whose vector count/dims deliberately disagree with the input.
    struct RaggedEmbedder {
        fewer: bool,
        ragged: bool,
    }

    #[async_trait]
    impl Embeddings for RaggedEmbedder {
        async fn embed_query(&self, text: &str) -> Result<Vec<f32>> {
            self.embed_documents(&[text.to_string()]).await
                .and_then(|mut v| v.pop().ok_or_else(|| LangchainError::LlmParse("empty".into())))
        }

        async fn embed_documents(&self, texts: &[String]) -> Result<Vec<Vec<f32>>> {
            let mut out: Vec<Vec<f32>> = texts
                .iter()
                .map(|_| vec![0f32; 64])
                .collect();
            if self.fewer && !out.is_empty() {
                out.pop(); // one embedding short
            }
            if self.ragged {
                if let Some(last) = out.last_mut() {
                    last.push(1.0); // wrong dimension on the last vector
                }
            }
            Ok(out)
        }
    }

    #[tokio::test]
    async fn add_texts_count_mismatch_is_an_error() {
        // Regression (M37): fewer embeddings than texts silently dropped
        // documents while reporting success.
        let store = InMemoryVectorStore::new();
        let embedder = RaggedEmbedder { fewer: true, ragged: false };
        let texts = vec!["a".to_string(), "b".to_string()];
        let err = store.add_texts(&texts, &embedder).await.unwrap_err();
        assert!(err.to_string().contains("1 vectors for 2 texts"), "got: {err}");
        assert_eq!(store.len().unwrap(), 0, "nothing may be stored on mismatch");
    }

    #[tokio::test]
    async fn add_texts_rejects_ragged_vector_dimensions() {
        // Regression (M37): the dimension check compared only first() vs
        // first(); a ragged vector inside the batch slipped through.
        let store = InMemoryVectorStore::new();
        let embedder = RaggedEmbedder { fewer: false, ragged: true };
        let texts = vec!["a".to_string(), "b".to_string()];
        assert!(store.add_texts(&texts, &embedder).await.is_err());
        assert_eq!(store.len().unwrap(), 0, "nothing may be stored on mismatch");

        // And an already-populated store rejects foreign dimensions.
        let good = HashEmbedder;
        store.add_texts(&["seed".to_string()], &good).await.unwrap();
        let embedder2 = RaggedEmbedder { fewer: false, ragged: true };
        assert!(store.add_texts(&["x".to_string(), "y".to_string()], &embedder2).await.is_err());
        assert_eq!(store.len().unwrap(), 1);
    }
}
