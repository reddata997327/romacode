//! `OpenAIEmbeddings` — calls `POST {base_url}/embeddings`.

use async_trait::async_trait;
use reqwest::header::{HeaderValue, AUTHORIZATION, CONTENT_TYPE};
use serde_json::json;

use crate::embeddings::Embeddings;
use crate::errors::{LangchainError, Result};
use std::time::Duration;

/// Embeddings backed by any OpenAI-compatible `/embeddings` endpoint.
pub struct OpenAIEmbeddings {
    model: String,
    api_key: String,
    base_url: String,
    timeout: Duration,
    client: reqwest::Client,
}

impl OpenAIEmbeddings {
    /// Builder entry point.
    pub fn builder() -> OpenAIEmbeddingsBuilder {
        OpenAIEmbeddingsBuilder::default()
    }

    async fn embed(&self, inputs: Vec<String>) -> Result<Vec<Vec<f32>>> {
        let url = format!("{}/embeddings", self.base_url.trim_end_matches('/'));
        let body = json!({ "model": self.model, "input": inputs });
        let resp = self
            .client
            .post(&url)
            .header(CONTENT_TYPE, HeaderValue::from_static("application/json"))
            .header(AUTHORIZATION, format!("Bearer {}", self.api_key))
            .timeout(self.timeout)
            .json(&body)
            .send()
            .await?;

        let status = resp.status();
        let text = resp.text().await?;
        if !status.is_success() {
            return Err(LangchainError::LlmRequest(format!("HTTP {status}: {text}")));
        }
        let payload: serde_json::Value = serde_json::from_str(&text)
            .map_err(|e| LangchainError::LlmParse(format!("embeddings: {e}")))?;
        let arr = payload
            .get("data")
            .and_then(|d| d.as_array())
            .ok_or_else(|| LangchainError::LlmParse("embeddings response missing `data`".into()))?;
        let mut vectors = arr
            .iter()
            .map(|item| {
                item.get("embedding")
                    .and_then(|e| e.as_array())
                    .map(|a| a.iter().filter_map(|x| x.as_f64()).map(|x| x as f32).collect::<Vec<f32>>())
                    .ok_or_else(|| LangchainError::LlmParse("embedding entry missing".into()))
            })
            .collect::<Result<Vec<_>>>()?;
        // Some providers return unordered indices; sort by `index` when present.
        if let Some(first) = arr.first() {
            if first.get("index").is_some() {
                let mut paired: Vec<(usize, Vec<f32>)> = arr
                    .iter()
                    .zip(vectors.drain(..))
                    .map(|(item, v)| (item["index"].as_u64().unwrap_or(0) as usize, v))
                    .collect();
                paired.sort_by_key(|(i, _)| *i);
                vectors = paired.into_iter().map(|(_, v)| v).collect();
            }
        }
        Ok(vectors)
    }
}

#[async_trait]
impl Embeddings for OpenAIEmbeddings {
    async fn embed_query(&self, text: &str) -> Result<Vec<f32>> {
        let mut v = self.embed(vec![text.to_string()]).await?;
        v.pop()
            .ok_or_else(|| LangchainError::LlmParse("empty embeddings response".into()))
    }

    async fn embed_documents(&self, texts: &[String]) -> Result<Vec<Vec<f32>>> {
        self.embed(texts.to_vec()).await
    }
}

/// Builder for [`OpenAIEmbeddings`].
#[derive(Debug, Default)]
pub struct OpenAIEmbeddingsBuilder {
    model: Option<String>,
    api_key: Option<String>,
    base_url: Option<String>,
    timeout_secs: Option<u64>,
}

impl OpenAIEmbeddingsBuilder {
    /// Model id, e.g. `text-embedding-3-small`.
    pub fn model(mut self, model: impl Into<String>) -> Self {
        self.model = Some(model.into());
        self
    }

    /// API key.
    pub fn api_key(mut self, key: impl Into<String>) -> Self {
        self.api_key = Some(key.into());
        self
    }

    /// Base URL, default `https://api.openai.com/v1`.
    pub fn base_url(mut self, url: impl Into<String>) -> Self {
        self.base_url = Some(url.into());
        self
    }

    /// Timeout seconds (default 60).
    pub fn timeout_secs(mut self, s: u64) -> Self {
        self.timeout_secs = Some(s);
        self
    }

    /// Validate and construct.
    pub fn build(self) -> Result<OpenAIEmbeddings> {
        let model = self.model.ok_or_else(|| LangchainError::InvalidInput("embeddings model is required".into()))?;
        let api_key = self.api_key.ok_or_else(|| LangchainError::InvalidInput("api_key is required".into()))?;
        let client = reqwest::Client::builder()
            .build()
            .map_err(|e| LangchainError::LlmRequest(e.to_string()))?;
        Ok(OpenAIEmbeddings {
            model,
            api_key,
            base_url: self.base_url.unwrap_or_else(|| "https://api.openai.com/v1".into()),
            timeout: Duration::from_secs(self.timeout_secs.unwrap_or(60)),
            client,
        })
    }
}
