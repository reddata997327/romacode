//! Provider adapters — the Rust analogue of OpenCode's `ProviderPackage`.
//!
//! Each adapter is a struct implementing [`ProviderAdapter`]. It declares its
//! id, default base URL, auth scheme (header name + env var), and the wire
//! protocol kind (Anthropic Messages vs OpenAI Chat/Responses). The adapter
//! does NOT make HTTP calls itself — it produces a [`ResolvedRequest`] that the
//! HTTP client ([`crate::client::HttpClient`]) executes.
//!
//! This mirrors OpenCode's separation: the adapter knows the shape of the
//! request (headers, body framing, auth); the client knows how to send it and
//! how to retry. RomaCode's old `request_anthropic` / `request_openai`
//! functions in `agent.rs` conflated both; this crate splits them cleanly.

use crate::schema::*;
use serde_json::Value;
use std::collections::BTreeMap;

/// The wire protocol a provider speaks. RomaCode's agent loop dispatches on
/// this to pick the SSE parser.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Protocol {
    /// Anthropic Messages API (`/v1/messages`, SSE, `x-api-key` auth).
    #[default]
    AnthropicMessages,
    /// OpenAI Chat Completions (`/v1/chat/completions`, SSE, Bearer auth).
    OpenAiChat,
    /// OpenAI Responses API (`/v1/responses`, SSE, Bearer auth).
    OpenAiResponses,
}

impl Protocol {
    pub fn as_str(self) -> &'static str {
        match self {
            Protocol::AnthropicMessages => "anthropic-messages",
            Protocol::OpenAiChat => "openai-chat",
            Protocol::OpenAiResponses => "openai-responses",
        }
    }
}

/// How a provider authenticates. Mirrors OpenCode `AuthOptions`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum AuthScheme {
    /// `x-api-key: <key>` (Anthropic). Falls back to Bearer if `auth_token` set.
    ApiKey { header: String, env_var: String },
    /// `Authorization: Bearer <key>` (OpenAI-style).
    Bearer { env_var: String },
    /// No auth header (local servers like Ollama).
    None,
}

/// The fully-resolved request the HTTP client will send: URL, headers, body.
#[derive(Clone, Debug)]
pub struct ResolvedRequest {
    pub url: String,
    pub headers: BTreeMap<String, String>,
    pub body: Value,
    pub protocol: Protocol,
}

/// One provider adapter. Implementors are zero-sized (state lives in config);
/// the trait methods read config + the active credential and produce the
/// request shape.
pub trait ProviderAdapter: Send + Sync {
    /// The provider id this adapter implements (e.g. `"anthropic"`).
    fn id(&self) -> &str;

    /// Human-readable name.
    fn name(&self) -> &str;

    /// Default base URL (no trailing slash). Overridable via config.
    fn default_base_url(&self) -> &str;

    /// Auth scheme (env var name + header).
    fn auth(&self) -> AuthScheme;

    /// Wire protocol.
    fn protocol(&self) -> Protocol;

    /// Build the request URL for one model call.
    fn endpoint(&self, base_url: &str) -> String {
        match self.protocol() {
            Protocol::AnthropicMessages => format!("{}/v1/messages", base_url.trim_end_matches('/')),
            Protocol::OpenAiChat => format!("{}/chat/completions", base_url.trim_end_matches('/')),
            Protocol::OpenAiResponses => format!("{}/responses", base_url.trim_end_matches('/')),
        }
    }

    /// Build the auth header(s) from the resolved credential.
    fn auth_headers(&self, api_key: Option<&str>) -> BTreeMap<String, String> {
        let mut h = BTreeMap::new();
        match (self.auth(), api_key) {
            (AuthScheme::ApiKey { header, .. }, Some(k)) => { h.insert(header.clone(), k.to_string()); }
            (AuthScheme::ApiKey { .. }, None) => {} // some adapters allow no key (ollama)
            (AuthScheme::Bearer { .. }, Some(k)) => { h.insert("Authorization".into(), format!("Bearer {k}")); }
            (AuthScheme::Bearer { .. }, None) => {}
            (AuthScheme::None, _) => {}
        }
        h
    }

    /// Build the full request from messages + params. Adapters may override to
    /// add provider-specific body fields (e.g. Anthropic `anthropic-version`).
    fn build_request(
        &self,
        base_url: &str,
        api_key: Option<&str>,
        messages: &[Value],
        max_tokens: u32,
        model: &str,
        extra_headers: &BTreeMap<String, String>,
        stream: bool,
    ) -> ResolvedRequest {
        self.default_build_request(base_url, api_key, messages, max_tokens, model, extra_headers, stream)
    }

    /// The non-trait default implementation. Adapters that override
    /// [`build_request`](ProviderAdapter::build_request) call this to get the
    /// base request, then mutate it — avoiding the infinite recursion that a
    /// naive `self.build_request()` call would cause.
    fn default_build_request(
        &self,
        base_url: &str,
        api_key: Option<&str>,
        messages: &[Value],
        max_tokens: u32,
        model: &str,
        extra_headers: &BTreeMap<String, String>,
        stream: bool,
    ) -> ResolvedRequest {
        let url = self.endpoint(base_url);
        let mut headers = self.auth_headers(api_key);
        for (k, v) in extra_headers { headers.insert(k.clone(), v.clone()); }
        headers.insert("content-type".into(), "application/json".into());
        let body = match self.protocol() {
            Protocol::AnthropicMessages => serde_json::json!({
                "model": model,
                "max_tokens": max_tokens,
                "messages": messages,
                "stream": stream,
            }),
            Protocol::OpenAiChat | Protocol::OpenAiResponses => serde_json::json!({
                "model": model,
                "max_tokens": max_tokens,
                "messages": messages,
                "stream": stream,
            }),
        };
        ResolvedRequest { url, headers, body, protocol: self.protocol() }
    }
}

// ── Concrete adapters ──────────────────────────────────────────────────────

/// Anthropic — `x-api-key` auth, Anthropic Messages protocol.
pub struct Anthropic;
impl ProviderAdapter for Anthropic {
    fn id(&self) -> &str { "anthropic" }
    fn name(&self) -> &str { "Anthropic" }
    fn default_base_url(&self) -> &str { "https://api.anthropic.com" }
    fn auth(&self) -> AuthScheme { AuthScheme::ApiKey { header: "x-api-key".into(), env_var: "ANTHROPIC_API_KEY".into() } }
    fn protocol(&self) -> Protocol { Protocol::AnthropicMessages }
    fn build_request(&self, base_url: &str, api_key: Option<&str>, messages: &[Value], max_tokens: u32, model: &str, extra_headers: &BTreeMap<String, String>, stream: bool) -> ResolvedRequest {
        let mut r = self.default_build_request(base_url, api_key, messages, max_tokens, model, extra_headers, stream);
        r.headers.insert("anthropic-version".into(), "2023-06-01".into());
        r
    }
}

/// OpenAI — Bearer auth, Chat Completions (Responses optional later).
pub struct OpenAi;
impl ProviderAdapter for OpenAi {
    fn id(&self) -> &str { "openai" }
    fn name(&self) -> &str { "OpenAI" }
    fn default_base_url(&self) -> &str { "https://api.openai.com/v1" }
    fn auth(&self) -> AuthScheme { AuthScheme::Bearer { env_var: "OPENAI_API_KEY".into() } }
    fn protocol(&self) -> Protocol { Protocol::OpenAiChat }
}

/// OpenRouter — OpenAI-compatible, Bearer auth.
pub struct OpenRouter;
impl ProviderAdapter for OpenRouter {
    fn id(&self) -> &str { "openrouter" }
    fn name(&self) -> &str { "OpenRouter" }
    fn default_base_url(&self) -> &str { "https://openrouter.ai/api/v1" }
    fn auth(&self) -> AuthScheme { AuthScheme::Bearer { env_var: "OPENROUTER_API_KEY".into() } }
    fn protocol(&self) -> Protocol { Protocol::OpenAiChat }
}

/// Google AI Studio — OpenAI-compatible endpoint, Bearer auth.
pub struct Google;
impl ProviderAdapter for Google {
    fn id(&self) -> &str { "google" }
    fn name(&self) -> &str { "Google AI Studio" }
    fn default_base_url(&self) -> &str { "https://generativelanguage.googleapis.com/v1beta/openai" }
    fn auth(&self) -> AuthScheme { AuthScheme::Bearer { env_var: "GEMINI_API_KEY".into() } }
    fn protocol(&self) -> Protocol { Protocol::OpenAiChat }
}

/// Groq — OpenAI-compatible, Bearer auth.
pub struct Groq;
impl ProviderAdapter for Groq {
    fn id(&self) -> &str { "groq" }
    fn name(&self) -> &str { "Groq" }
    fn default_base_url(&self) -> &str { "https://api.groq.com/openai/v1" }
    fn auth(&self) -> AuthScheme { AuthScheme::Bearer { env_var: "GROQ_API_KEY".into() } }
    fn protocol(&self) -> Protocol { Protocol::OpenAiChat }
}

/// DeepSeek — OpenAI-compatible, Bearer auth.
pub struct DeepSeek;
impl ProviderAdapter for DeepSeek {
    fn id(&self) -> &str { "deepseek" }
    fn name(&self) -> &str { "DeepSeek" }
    fn default_base_url(&self) -> &str { "https://api.deepseek.com/v1" }
    fn auth(&self) -> AuthScheme { AuthScheme::Bearer { env_var: "DEEPSEEK_API_KEY".into() } }
    fn protocol(&self) -> Protocol { Protocol::OpenAiChat }
}

/// Mistral — OpenAI-compatible, Bearer auth.
pub struct Mistral;
impl ProviderAdapter for Mistral {
    fn id(&self) -> &str { "mistral" }
    fn name(&self) -> &str { "Mistral" }
    fn default_base_url(&self) -> &str { "https://api.mistral.ai/v1" }
    fn auth(&self) -> AuthScheme { AuthScheme::Bearer { env_var: "MISTRAL_API_KEY".into() } }
    fn protocol(&self) -> Protocol { Protocol::OpenAiChat }
}

/// xAI — OpenAI-compatible, Bearer auth.
pub struct Xai;
impl ProviderAdapter for Xai {
    fn id(&self) -> &str { "xai" }
    fn name(&self) -> &str { "xAI" }
    fn default_base_url(&self) -> &str { "https://api.x.ai/v1" }
    fn auth(&self) -> AuthScheme { AuthScheme::Bearer { env_var: "XAI_API_KEY".into() } }
    fn protocol(&self) -> Protocol { Protocol::OpenAiChat }
}

/// Z.AI — OpenAI-compatible, Bearer auth.
pub struct Zai;
impl ProviderAdapter for Zai {
    fn id(&self) -> &str { "zai" }
    fn name(&self) -> &str { "Z.AI" }
    fn default_base_url(&self) -> &str { "https://api.z.ai/api/paas/v4" }
    fn auth(&self) -> AuthScheme { AuthScheme::Bearer { env_var: "ZAI_API_KEY".into() } }
    fn protocol(&self) -> Protocol { Protocol::OpenAiChat }
}

/// Ollama (local) — OpenAI-compatible, no auth.
pub struct Ollama;
impl ProviderAdapter for Ollama {
    fn id(&self) -> &str { "ollama" }
    fn name(&self) -> &str { "Ollama (local)" }
    fn default_base_url(&self) -> &str { "http://localhost:11434/v1" }
    fn auth(&self) -> AuthScheme { AuthScheme::None }
    fn protocol(&self) -> Protocol { Protocol::OpenAiChat }
}

/// LM Studio (local) — OpenAI-compatible, no auth.
pub struct LmStudio;
impl ProviderAdapter for LmStudio {
    fn id(&self) -> &str { "lmstudio" }
    fn name(&self) -> &str { "LM Studio (local)" }
    fn default_base_url(&self) -> &str { "http://localhost:1234/v1" }
    fn auth(&self) -> AuthScheme { AuthScheme::None }
    fn protocol(&self) -> Protocol { Protocol::OpenAiChat }
}

/// Cerebras — OpenAI-compatible, Bearer auth.
pub struct Cerebras;
impl ProviderAdapter for Cerebras {
    fn id(&self) -> &str { "cerebras" }
    fn name(&self) -> &str { "Cerebras" }
    fn default_base_url(&self) -> &str { "https://api.cerebras.ai/v1" }
    fn auth(&self) -> AuthScheme { AuthScheme::Bearer { env_var: "CEREBRAS_API_KEY".into() } }
    fn protocol(&self) -> Protocol { Protocol::OpenAiChat }
}

/// Together — OpenAI-compatible, Bearer auth.
pub struct Together;
impl ProviderAdapter for Together {
    fn id(&self) -> &str { "together" }
    fn name(&self) -> &str { "Together" }
    fn default_base_url(&self) -> &str { "https://api.together.xyz/v1" }
    fn auth(&self) -> AuthScheme { AuthScheme::Bearer { env_var: "TOGETHER_API_KEY".into() } }
    fn protocol(&self) -> Protocol { Protocol::OpenAiChat }
}

/// Fireworks — OpenAI-compatible, Bearer auth.
pub struct Fireworks;
impl ProviderAdapter for Fireworks {
    fn id(&self) -> &str { "fireworks" }
    fn name(&self) -> &str { "Fireworks" }
    fn default_base_url(&self) -> &str { "https://api.fireworks.ai/inference/v1" }
    fn auth(&self) -> AuthScheme { AuthScheme::Bearer { env_var: "FIREWORKS_API_KEY".into() } }
    fn protocol(&self) -> Protocol { Protocol::OpenAiChat }
}

/// Coarena gateway — OpenAI-compatible Chat Completions, Bearer auth.
pub struct Coarena;
impl ProviderAdapter for Coarena {
    fn id(&self) -> &str { "coarena" }
    fn name(&self) -> &str { "Coarena" }
    fn default_base_url(&self) -> &str { "https://coarena.ai/api/v1" }
    fn auth(&self) -> AuthScheme { AuthScheme::Bearer { env_var: "COARENA_API_KEY".into() } }
    fn protocol(&self) -> Protocol { Protocol::OpenAiChat }
}

/// Logfare gateway — OpenAI-compatible Chat Completions, Bearer auth.
pub struct Logfare;
impl ProviderAdapter for Logfare {
    fn id(&self) -> &str { "logfare" }
    fn name(&self) -> &str { "Logfare" }
    fn default_base_url(&self) -> &str { "https://logfare.ai/v1" }
    fn auth(&self) -> AuthScheme { AuthScheme::Bearer { env_var: "LOGFARE_API_KEY".into() } }
    fn protocol(&self) -> Protocol { Protocol::OpenAiChat }
}

/// The registry of built-in adapters. Returns the adapter for a provider id,
/// or `None` for custom/unknown ids (which fall back to OpenAI-compatible
/// with a config-supplied base URL).
pub fn adapter_for(id: &str) -> Option<Box<dyn ProviderAdapter>> {
    Some(match id {
        "anthropic" => Box::new(Anthropic),
        "openai" => Box::new(OpenAi),
        "openrouter" => Box::new(OpenRouter),
        "google" => Box::new(Google),
        "groq" => Box::new(Groq),
        "deepseek" => Box::new(DeepSeek),
        "mistral" => Box::new(Mistral),
        "xai" => Box::new(Xai),
        "zai" => Box::new(Zai),
        "ollama" => Box::new(Ollama),
        "lmstudio" => Box::new(LmStudio),
        "cerebras" => Box::new(Cerebras),
        "together" => Box::new(Together),
        "fireworks" => Box::new(Fireworks),
        "coarena" => Box::new(Coarena),
        "logfare" => Box::new(Logfare),
        _ => return None,
    })
}

/// Built-in provider ids (OpenCode `Provider` constants + RomaCode extras).
pub const BUILTIN_IDS: &[&str] = &[
    "anthropic", "openai", "openrouter", "google", "groq", "deepseek",
    "mistral", "xai", "zai", "ollama", "lmstudio", "cerebras", "together", "fireworks",
    "coarena", "logfare",
];

/// A generic OpenAI-compatible adapter for custom providers (config-supplied
/// base URL, Bearer auth via a config-named env var).
pub struct OpenAiCompatible {
    pub id: String,
    pub name: String,
    pub base_url: String,
    pub env_var: String,
}
impl ProviderAdapter for OpenAiCompatible {
    fn id(&self) -> &str { &self.id }
    fn name(&self) -> &str { &self.name }
    fn default_base_url(&self) -> &str { &self.base_url }
    fn auth(&self) -> AuthScheme { AuthScheme::Bearer { env_var: self.env_var.clone() } }
    fn protocol(&self) -> Protocol { Protocol::OpenAiChat }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn adapter_for_known_ids() {
        assert!(adapter_for("anthropic").is_some());
        assert!(adapter_for("openai").is_some());
        assert!(adapter_for("zai").is_some());
        assert!(adapter_for("coarena").is_some());
        assert!(adapter_for("logfare").is_some());
        assert!(adapter_for("nonexistent").is_none());
    }

    #[test]
    fn coarena_and_logfare_use_openai_compatible_endpoints() {
        let c = adapter_for("coarena").expect("coarena");
        assert_eq!(c.default_base_url(), "https://coarena.ai/api/v1");
        assert_eq!(c.protocol(), Protocol::OpenAiChat);
        let l = adapter_for("logfare").expect("logfare");
        assert_eq!(l.default_base_url(), "https://logfare.ai/v1");
        assert_eq!(l.protocol(), Protocol::OpenAiChat);
    }

    #[test]
    fn anthropic_endpoint_and_auth() {
        let a = Anthropic;
        assert_eq!(a.endpoint("https://api.anthropic.com"), "https://api.anthropic.com/v1/messages");
        let h = a.auth_headers(Some("sk-ant-xxx"));
        assert_eq!(h.get("x-api-key").map(|s| s.as_str()), Some("sk-ant-xxx"));
    }

    #[test]
    fn openai_endpoint_and_bearer() {
        let a = OpenAi;
        assert_eq!(a.endpoint("https://api.openai.com/v1"), "https://api.openai.com/v1/chat/completions");
        let h = a.auth_headers(Some("sk-xxx"));
        assert_eq!(h.get("Authorization").map(|s| s.as_str()), Some("Bearer sk-xxx"));
    }

    #[test]
    fn anthropic_build_request_adds_version_header() {
        let a = Anthropic;
        let msgs = vec![serde_json::json!({"role":"user","content":"hi"})];
        let r = a.build_request("https://api.anthropic.com", Some("k"), &msgs, 1024, "claude-x", &BTreeMap::new(), true);
        assert_eq!(r.headers.get("anthropic-version").map(|s| s.as_str()), Some("2023-06-01"));
        assert_eq!(r.headers.get("x-api-key").map(|s| s.as_str()), Some("k"));
        assert_eq!(r.protocol, Protocol::AnthropicMessages);
        assert!(r.body["stream"].as_bool().unwrap());
    }

    #[test]
    fn openai_compatible_custom() {
        let a = OpenAiCompatible { id: "myco".into(), name: "MyCo".into(), base_url: "https://api.my.co/v1".into(), env_var: "MYCO_API_KEY".into() };
        assert_eq!(a.endpoint("https://api.my.co/v1"), "https://api.my.co/v1/chat/completions");
        match a.auth() { AuthScheme::Bearer { env_var } => assert_eq!(env_var, "MYCO_API_KEY"), _ => panic!() }
    }

    #[test]
    fn ollama_no_auth() {
        let a = Ollama;
        assert!(a.auth_headers(None).is_empty());
    }

    #[test]
    fn builtin_ids_contains_zai() {
        assert!(BUILTIN_IDS.contains(&"zai"));
        assert!(BUILTIN_IDS.contains(&"anthropic"));
    }
}
