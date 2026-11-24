//! Provider catalog — thin compatibility shim over the new
//! `romacode-providers` crate.
//!
//! **History**: this module used to be a static `&'static [Known]` slice of
//! hardcoded provider entries (anthropic, openai, groq, …) plus the `Known`
//! struct and `ApiKind` enum. That was the "previous structure" the user asked
//! to remove.
//!
//! **Now**: the source of truth is `romacode_providers::catalog::Catalog`
//! (dynamic, network-fetched with a 5-minute TTL, bundled fallback). This
//! module keeps the legacy `ApiKind` / `Known` / `KNOWN` / `find` API surface
//! so the ~15 consumer files (server config/agent/cli/api/tools, client-tui
//! settings/app/tree/palette/main) keep compiling without a flag-day rewrite
//! of every struct-field access — but the data now flows from the new crate.
//!
//! `KNOWN` is materialized once (via `OnceLock` + `Box::leak`) from the
//! bundled catalog, so `find` still returns `&'static Known`. The static
//! `KNOWN` slice is therefore only the bundled fallback; live catalog
//! updates flow through `romacode_providers::catalog::cached()` directly
//! (used by `provider_engine` and the TUI fallback).

use romacode_providers::catalog::Catalog;
use romacode_providers::{Protocol, ProviderId};
use serde::{Deserialize, Serialize};
use std::sync::OnceLock;

/// Wire protocol a provider speaks. Mirrors `romacode_providers::Protocol`
/// but keeps the legacy `Anthropic` / `OpenAi` names so existing `match` arms
/// compile unchanged.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ApiKind {
    /// Anthropic Messages API (`/v1/messages`, `x-api-key`).
    #[default]
    Anthropic,
    /// OpenAI-compatible Chat Completions (`/chat/completions`, Bearer key):
    /// OpenAI, OpenRouter, Gemini, Groq, DeepSeek, Ollama, LM Studio, vLLM, ...
    #[serde(alias = "openai-compatible", alias = "openai_compatible")]
    OpenAi,
}

impl ApiKind {
    /// Lowercase wire name used in the protocol (`kind` strings).
    pub fn as_str(self) -> &'static str {
        match self {
            ApiKind::Anthropic => "anthropic",
            ApiKind::OpenAi => "openai",
        }
    }
    /// Parse a wire `kind` string.
    pub fn from_str(s: &str) -> ApiKind {
        match s {
            "anthropic" => ApiKind::Anthropic,
            _ => ApiKind::OpenAi,
        }
    }
    /// Map to the new crate's `Protocol`.
    pub fn to_protocol(self) -> Protocol {
        match self {
            ApiKind::Anthropic => Protocol::AnthropicMessages,
            ApiKind::OpenAi => Protocol::OpenAiChat,
        }
    }
    /// Map from the new crate's `Protocol`.
    pub fn from_protocol(p: Protocol) -> Self {
        match p {
            Protocol::AnthropicMessages => ApiKind::Anthropic,
            Protocol::OpenAiChat | Protocol::OpenAiResponses => ApiKind::OpenAi,
        }
    }
}

/// One known provider row (display + resolution metadata; no secrets).
///
/// This is the legacy compatibility shape. It is materialized from the new
/// `romacode_providers::ProviderInfo` (bundled fallback) so the data is the
/// same source of truth the rest of the app uses. The richer fields
/// (capabilities, costs, variants, limits) live on the new `ProviderInfo` /
/// `ModelInfo` types; `Known` projects down to just what the legacy call sites
/// need.
#[derive(Debug, Clone)]
pub struct Known {
    pub id: String,
    pub name: String,
    pub kind: ApiKind,
    pub base_url: String,
    /// Conventional API-key environment variable ("" = none needed).
    pub key_env: String,
    /// Local servers (Ollama, LM Studio) work without a key.
    pub needs_key: bool,
    pub default_model: String,
    pub models: Vec<String>,
}

/// The materialized legacy catalog. Built once from the new crate's bundled
/// catalog (so it is available with no network) and leaked to satisfy the
/// `&'static` lifetime that `find` returns.
static KNOWN_CELLS: OnceLock<Vec<Known>> = OnceLock::new();

fn materialize() -> &'static [Known] {
    KNOWN_CELLS.get_or_init(|| {
        let cat = Catalog::bundled();
        let mut out: Vec<Known> = Vec::new();
        for (pid, p) in &cat.providers {
            // Derive the adapter to learn base_url + auth scheme + protocol.
            let adapter = romacode_providers::adapter_for(pid.as_str());
            let (base_url, key_env, needs_key, kind) = match adapter {
                Some(a) => {
                    let base = a.default_base_url().to_string();
                    let (env, needs) = match a.auth() {
                        romacode_providers::AuthScheme::ApiKey { env_var, .. } => (env_var, true),
                        romacode_providers::AuthScheme::Bearer { env_var } => (env_var, true),
                        romacode_providers::AuthScheme::None => (String::new(), false),
                    };
                    (base, env, needs, ApiKind::from_protocol(a.protocol()))
                }
                None => match romacode_providers::catalog::bundled_meta(pid.as_str()) {
                    // Generated providers have no hand-written
                    // adapter but do carry a base URL, key env and protocol
                    // in the bundled snapshot. Use them so every catalog
                    // provider is selectable and resolvable.
                    Some(m) => (
                        m.base_url.to_string(),
                        m.key_env.to_string(),
                        m.needs_key,
                        if m.anthropic { ApiKind::Anthropic } else { ApiKind::OpenAi },
                    ),
                    None => (String::new(), String::new(), false, ApiKind::OpenAi),
                },
            };
            let models: Vec<String> = p.models.keys().map(|m| m.as_str().to_string()).collect();
            let default_model = p.default_model
                .as_ref()
                .map(|m| m.as_str().to_string())
                .or_else(|| models.first().cloned())
                .unwrap_or_default();
            out.push(Known {
                id: pid.as_str().to_string(),
                name: if p.name.is_empty() { pid.as_str().to_string() } else { p.name.clone() },
                kind,
                base_url,
                key_env,
                needs_key,
                default_model,
                models,
            });
        }
        // Keep the 14 core providers at the top of the picker, in their
        // original order, followed by the rest of the catalog by id. This
        // preserves the familiar list for existing users while exposing the
        // full set below it.
        out.sort_by(|a, b| {
            fn rank(id: &str) -> usize {
                romacode_providers::catalog::BUILTIN_SNAPSHOT
                    .iter()
                    .position(|e| e.id.eq_ignore_ascii_case(id))
                    .unwrap_or(usize::MAX)
            }
            let (ra, rb) = (rank(&a.id), rank(&b.id));
            ra.cmp(&rb).then_with(|| a.id.cmp(&b.id))
        });
        out
    })
}

/// The legacy `KNOWN` slice, now materialized from the new crate. Returns a
/// `&'static` reference so existing call sites (`for k in KNOWN`, `KNOWN[0]`)
/// keep working.
pub fn known() -> &'static [Known] { materialize() }

/// Back-compat alias. Older call sites wrote `catalog::KNOWN` (a const slice).
/// That can no longer be a `const` (it is lazily materialized), so this is a
/// function; call sites are migrated to `known()`. A `KNOWN` const of a
/// pointer is not possible without `lazy_static`/`once_cell` as a static, so
/// the migration is: replace `catalog::KNOWN` → `catalog::known()` and
/// `catalog::find(id)` stays as-is.
pub fn find(id: &str) -> Option<&'static Known> {
    let id = id.trim().to_ascii_lowercase();
    let cells = materialize();
    // SAFETY: `materialize()` returns a &'static [Known] via OnceLock, so
    // indexing into it and reborrowing as &'static is sound.
    cells.iter().find(|k| k.id == id)
        .map(|k| unsafe { std::mem::transmute::<&Known, &'static Known>(k) })
}

/// Number of known providers (back-compat for `KNOWN.len()`).
pub fn known_len() -> usize { materialize().len() }

/// Indexed access (back-compat for `KNOWN[i]`).
pub fn known_at(i: usize) -> Option<&'static Known> {
    let cells = materialize();
    cells.get(i)
        .map(|k| unsafe { std::mem::transmute::<&Known, &'static Known>(k) })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn known_is_populated() {
        let k = known();
        // The full provider catalog is bundled, not just the core.
        assert!(k.len() >= 300, "bundled catalog should have >= 300 providers, got {}", k.len());
        for id in ["anthropic", "openai", "zai", "azure", "vercel", "huggingface", "nvidia", "coarena", "logfare"] {
            assert!(k.iter().any(|p| p.id == id), "{id} must be in the catalog");
        }
    }

    #[test]
    fn generated_provider_has_resolvable_metadata() {
        let v = find("vercel").expect("vercel");
        assert!(v.base_url.starts_with("https://"), "generated provider must have a base URL");
        assert!(!v.key_env.is_empty(), "generated provider must have a key env var");
        assert!(v.needs_key);
        assert!(!v.models.is_empty());
        assert!(!v.default_model.is_empty());
    }

    #[test]
    fn find_is_case_insensitive() {
        assert!(find("anthropic").is_some());
        assert!(find("ANTHROPIC").is_some());
        assert!(find("  openai  ").is_some());
        assert!(find("nonexistent").is_none());
    }

    #[test]
    fn anthropic_entry_shape() {
        let a = find("anthropic").expect("anthropic");
        assert_eq!(a.kind, ApiKind::Anthropic);
        assert_eq!(a.base_url, "https://api.anthropic.com");
        assert_eq!(a.key_env, "ANTHROPIC_API_KEY");
        assert!(a.needs_key);
        assert!(!a.models.is_empty());
        assert!(!a.default_model.is_empty());
    }

    #[test]
    fn ollama_needs_no_key() {
        let o = find("ollama").expect("ollama");
        assert!(!o.needs_key);
        assert!(o.key_env.is_empty());
    }

    #[test]
    fn apikind_roundtrips() {
        assert_eq!(ApiKind::from_str("anthropic"), ApiKind::Anthropic);
        assert_eq!(ApiKind::from_str("openai"), ApiKind::OpenAi);
        assert_eq!(ApiKind::from_str("garbage"), ApiKind::OpenAi);
        assert_eq!(ApiKind::Anthropic.as_str(), "anthropic");
        assert_eq!(ApiKind::OpenAi.as_str(), "openai");
    }

    #[test]
    fn apikind_maps_to_protocol() {
        assert_eq!(ApiKind::Anthropic.to_protocol(), Protocol::AnthropicMessages);
        assert_eq!(ApiKind::OpenAi.to_protocol(), Protocol::OpenAiChat);
        assert_eq!(ApiKind::from_protocol(Protocol::AnthropicMessages), ApiKind::Anthropic);
        assert_eq!(ApiKind::from_protocol(Protocol::OpenAiChat), ApiKind::OpenAi);
    }

    #[test]
    fn known_at_indexes() {
        assert!(known_at(0).is_some());
        assert!(known_at(known_len().wrapping_sub(1)).is_some());
        assert!(known_at(known_len() + 100).is_none());
    }
}
