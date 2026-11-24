//! End-to-end tests of the OpenAI protocol layer against a local mock server
//! (wiremock). No network access required — these prove the agent loop works
//! with the real `ChatOpenAI` client, not just the fake model.

use async_trait::async_trait;
use langchain_rust::agents::{calculator_tool, AgentExecutor};
use langchain_rust::llms::openai::ChatOpenAI;
use langchain_rust::llms::BaseChatModel;
use langchain_rust::embeddings::Embeddings;
use langchain_rust::vectorstores::InMemoryVectorStore;
use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

fn completion_json(content: &str) -> ResponseTemplate {
    ResponseTemplate::new(200).set_body_json(serde_json::json!({
        "id": "chatcmpl-1",
        "object": "chat.completion",
        "choices": [{
            "index": 0,
            "message": {"role": "assistant", "content": content},
            "finish_reason": "stop"
        }],
        "usage": {"prompt_tokens": 9, "completion_tokens": 4, "total_tokens": 13}
    }))
}

fn tool_call_json(id: &str, name: &str, args: &str) -> ResponseTemplate {
    let body = serde_json::json!({
        "choices": [{
            "index": 0,
            "message": {
                "role": "assistant",
                "content": null,
                "tool_calls": [{
                    "id": id,
                    "type": "function",
                    "function": {"name": name, "arguments": args}
                }]
            }
        }]
    });
    ResponseTemplate::new(200).set_body_json(body)
}

fn make_model(server: &MockServer) -> ChatOpenAI {
    ChatOpenAI::builder()
        .model("mock-gpt")
        .api_key("test-key")
        .base_url(server.uri())
        .build()
        .expect("model")
}

#[tokio::test]
async fn chat_openai_plain_completion() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/chat/completions"))
        .respond_with(completion_json("Rust is memory safe."))
        .mount(&server)
        .await;

    let model = make_model(&server);
    let messages = vec![langchain_rust::messages::Message::human("why rust?")];
    let answer = model.generate(&messages).await.unwrap();

    assert_eq!(answer.text(), "Rust is memory safe.");
    let usage = model.usage().unwrap();
    assert_eq!(usage.total_tokens, 13);

    // The client must have sent bearer auth + model id.
    let reqs = server.received_requests().await.unwrap();
    assert_eq!(reqs.len(), 1);
    let auth = reqs[0]
        .headers
        .iter()
        .find(|(k, _)| k.as_str() == "authorization")
        .map(|(_, v)| String::from_utf8_lossy(v.as_bytes()).to_string());
    assert_eq!(auth.as_deref(), Some("Bearer test-key"));
    let body: serde_json::Value = serde_json::from_slice(&reqs[0].body).unwrap();
    assert_eq!(body["model"], "mock-gpt");
}

#[tokio::test]
async fn agent_loop_with_real_openai_client() {
    let server = MockServer::start().await;

    // Turn 1: model requests the calculator with 21*2.
    Mock::given(method("POST"))
        .and(path("/chat/completions"))
        .respond_with(tool_call_json("call_x", "calculator", "{\"expression\":\"21*2\"}"))
        .up_to_n_times(1)
        .mount(&server)
        .await;
    // Turn 2: final answer.
    Mock::given(method("POST"))
        .and(path("/chat/completions"))
        .respond_with(completion_json("The answer is 42."))
        .mount(&server)
        .await;

    let model = make_model(&server);
    let mut agent = AgentExecutor::builder(std::sync::Arc::new(model))
        .tool(calculator_tool())
        .max_iterations(4)
        .build();

    let answer = agent.invoke("what is 21*2?").await.unwrap();
    assert_eq!(answer, "The answer is 42.");

    let reqs = server.received_requests().await.unwrap();
    assert_eq!(reqs.len(), 2);
    // First request carries the tool schema, second carries the tool result.
    let b1 = String::from_utf8_lossy(&reqs[0].body).to_string();
    let b2 = String::from_utf8_lossy(&reqs[1].body).to_string();
    assert!(b1.contains("\"calculator\""), "first request must bind tools");
    assert!(b2.contains("tool_call_id"), "second request must include the tool result");
    assert!(b2.contains("call_x"));
}

#[tokio::test]
async fn sse_streaming_is_parsed() {
    let server = MockServer::start().await;
    let sse_body = concat!(
        "data: {\"choices\":[{\"delta\":{\"content\":\"Hel\"}}]}\n\n",
        "data: {\"choices\":[{\"delta\":{\"content\":\"lo\"}}]}\n\n",
        "data: {\"choices\":[{\"delta\":{}}]}\n\n",
        "data: [DONE]\n\n"
    );
    Mock::given(method("POST"))
        .and(path("/chat/completions"))
        .respond_with(
            ResponseTemplate::new(200)
                .insert_header("content-type", "text/event-stream")
                .set_body_string(sse_body),
        )
        .mount(&server)
        .await;

    let model = make_model(&server);
    let messages = vec![langchain_rust::messages::Message::human("hi")];
    use futures::StreamExt;
    let mut stream = model.stream_text(&messages);
    let mut out = String::new();
    while let Some(chunk) = stream.next().await {
        out.push_str(&chunk.unwrap());
    }
    assert_eq!(out, "Hello");
}

#[tokio::test]
async fn embeddings_roundtrip_into_store() {
    // Minimal deterministic embedding server: one 4-dim unit vector PER
    // input text, exactly like a real provider (a mock that returned fewer
    // embeddings than inputs encoded the old silent-drop behaviour).
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/embeddings"))
        .respond_with(|req: &wiremock::Request| {
            let n = req
                .body_json::<serde_json::Value>()
                .ok()
                .and_then(|v| v.get("input").and_then(|i| i.as_array()).map(Vec::len))
                .unwrap_or(1)
                .max(1);
            let data: Vec<serde_json::Value> = (0..n)
                .map(|i| serde_json::json!({"index": i, "embedding": [1.0, 0.0, 0.0, 0.0]}))
                .collect();
            ResponseTemplate::new(200).set_body_json(serde_json::json!({ "data": data }))
        })
        .mount(&server)
        .await;

    struct OneHotEmbedder {
        url: String,
    }
    #[async_trait]
    impl Embeddings for OneHotEmbedder {
        async fn embed_query(&self, text: &str) -> langchain_rust::errors::Result<Vec<f32>> {
            self.embed_documents(&[text.to_string()])
                .await
                .and_then(|mut v| {
                    v.pop()
                        .ok_or_else(|| langchain_rust::errors::LangchainError::LlmParse("empty".into()))
                })
        }
        async fn embed_documents(
            &self,
            texts: &[String],
        ) -> langchain_rust::errors::Result<Vec<Vec<f32>>> {
            let emb = langchain_rust::embeddings::openai::OpenAIEmbeddings::builder()
                .model("mock-embed")
                .api_key("k")
                .base_url(&self.url)
                .build()?;
            emb.embed_documents(texts).await
        }
    }

    let embedder = OneHotEmbedder { url: server.uri() };
    let store = InMemoryVectorStore::new();
    let texts: Vec<String> = vec!["alpha".into(), "beta".into()];
    assert_eq!(store.add_texts(&texts, &embedder).await.unwrap(), 2);
    let hits = store.similarity_search("alpha", 1, &embedder).await.unwrap();
    assert_eq!(hits.len(), 1);
    assert_eq!(hits[0].0.page_content, "alpha");
    assert!((hits[0].1 - 1.0).abs() < 1e-6);
}

#[tokio::test]
async fn provider_error_maps_to_langchain_error() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/chat/completions"))
        .respond_with(ResponseTemplate::new(401).set_body_string(r#"{"error":"bad key"}"#))
        .mount(&server)
        .await;
    let model = make_model(&server);
    let err = model.generate(&[]).await.unwrap_err();
    assert!(err.to_string().contains("401"), "got: {err}");
}
