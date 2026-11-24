//! `romacode-providers` — the OpenCode-structured provider/model subsystem,
//! rewritten in pure Rust.
//!
//! This crate replaces RomaCode's old `ProviderCfg` / `Resolved` / static
//! `KNOWN` catalog / curl-subprocess provider calls. It is a direct port of
//! OpenCode's `packages/schema` (provider/model/config/credential types) +
//! `packages/ai` (adapter trait + retry client) + `packages/core/models-dev`
//! (dynamic catalog), rewritten in Rust.
//!
//! # Modules
//!
//! * [`schema`] — `ProviderInfo`, `ModelInfo`, `ConfigProvider`,
//!   `ModelSelection`, `CredentialValue` (the type hierarchy).
//! * [`adapter`] — `ProviderAdapter` trait + built-in adapters
//!   (anthropic/openai/openrouter/google/groq/deepseek/mistral/xai/zai/
//!   ollama/lmstudio/cerebras/together/fireworks) + a generic
//!   `OpenAiCompatible` for custom providers.
//! * [`client`] — native HTTP client + the OpenCode retry policy
//!   (`RetryPolicy`, `FailureClass`, `request_with_retry`).
//! * [`catalog`] — dynamic provider+model catalog with a bundled fallback
//!   snapshot.
//!
//! # Why this exists
//!
//! RomaCode's old provider system was a fixed `&[Known]` slice and a curl
//! subprocess that died after 4 retries. OpenCode's is a dynamic catalog, a
//! real HTTP client, and a 10-retry layered policy that keeps the turn alive.
//! This crate ports the OpenCode structure to Rust so RomaCode gets the same
//! resilience, while keeping the zero-extra-deps philosophy (the HTTP client
//! is std-only; adapters are zero-sized structs).

pub mod adapter;
pub mod catalog;
/// Generated snapshot of the full provider catalog.
pub mod catalog_snapshot;
pub mod client;
pub mod schema;

pub use adapter::{adapter_for, AuthScheme, OpenAiCompatible, Protocol, ProviderAdapter, ResolvedRequest};
pub use catalog::{cached, refresh, Catalog, CatalogSource, DEFAULT_CATALOG_URL, TTL_SECS};
pub use client::{
    FailureClass, HttpClient, HttpOutcome, RetryDecision, RetryExhausted, RetryPolicy, SseStream,
    request_with_retry,
};
pub use schema::{
    Activation, Cache, Capabilities, Compatibility, Compaction, ConfigProvider, ConfigModelLimit,
    ConfigModelCapabilities, ConfigModelCost, Cost, CostTier, CostTierKind, CostCache,
    CredentialEntry, CredentialExternal, CredentialKey, CredentialOauth, CredentialValue,
    Encoding, Family, IntegrationId, IntegrationMethodId, MediaInput, MaxTokensField, ModelConfig,
    ModelId, ModelInfo, ModelLimit, ModelSelection, ModelStatus, ModelTime, ModelType, ProviderId,
    ProviderInfo, ProviderRequest, ProviderSettings, ProviderCompatibility, Reasoning,
    ReasoningBudget, Tls, Timeout, Transport, UsdPerMillionTokens, Variant, VariantId, WarmingFlag,
    CacheWarming, CacheTtl,
};

/// Crate version, matching the workspace.
pub const VERSION: &str = env!("CARGO_PKG_VERSION");
