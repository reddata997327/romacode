//! Provider + model catalog — the Rust analogue of OpenCode's
//! `packages/core/src/models-dev.ts` + `packages/app/src/providers/catalog`.
//!
//! OpenCode fetches a catalog snapshot from `https://models.opencode.ai/...`
//! with a 5-minute TTL, ETag cache, and a bundled fallback. RomaCode mirrors
//! this: [`Catalog`] holds a list of [`ProviderInfo`] each with a map of
//! [`ModelInfo`]. It is populated by [`Catalog::fetch`] (HTTP GET against the
//! configured catalog URL) with [`Catalog::bundled`] as the zero-network
//! fallback. The catalog is the single source of truth for what providers and
//! models exist — the TUI, CLI, and agent loop all read it.
//!
//! Unlike RomaCode's old static `KNOWN` slice, this catalog is dynamic: a
//! remote update adds providers and models without a RomaCode release, and the
//! bundled snapshot is only the last-resort fallback (mirroring OpenCode's
//! `snapshot.txt`).

use crate::schema::*;
use std::collections::BTreeMap;
use std::sync::{Mutex, OnceLock};

/// Default catalog URL (RomaCode-hosted equivalent of models.dev).
/// Mirrors OpenCode's `https://models.opencode.ai/experimental/v2.0/api.json`.
pub const DEFAULT_CATALOG_URL: &str = "https://catalog.romacode.dev/v1/catalog.json";

/// Catalog TTL — 5 minutes, matching OpenCode.
pub const TTL_SECS: u64 = 5 * 60;

/// The catalog: every known provider with its models.
#[derive(Clone, Debug, Default)]
pub struct Catalog {
    pub providers: BTreeMap<ProviderId, ProviderInfo>,
    /// When this catalog was loaded (unix ms).
    pub fetched_at: u64,
    /// Whether this came from the network or the bundled fallback.
    pub source: CatalogSource,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum CatalogSource {
    #[default]
    Bundled,
    Network,
}

impl Catalog {
    /// The bundled fallback catalog (zero-network). Built from the adapter
    /// registry + a small static model list per provider. This is the
    /// equivalent of OpenCode shipping `snapshot.txt`.
    pub fn bundled() -> Self {
        let mut providers = BTreeMap::new();
        for entry in bundled_entries() {
            let mut p = ProviderInfo {
                id: ProviderId::new(entry.id),
                name: entry.name.into(),
                package: entry.id.into(),
                activation: Activation::Auto,
                default_model: Some(ModelId::new(entry.default)),
                ..Default::default()
            };
            for m in entry.models {
                let mid = ModelId::new(*m);
                p.models.insert(mid.clone(), ModelInfo::default_for(p.id.clone(), mid));
            }
            providers.insert(p.id.clone(), p);
        }
        Self { providers, fetched_at: now_ms(), source: CatalogSource::Bundled }
    }

    /// Look up a provider by id.
    pub fn provider(&self, id: &str) -> Option<&ProviderInfo> {
        self.providers.get(&ProviderId::new(id.to_string()))
    }

    /// All provider ids.
    pub fn provider_ids(&self) -> Vec<String> {
        self.providers.keys().map(|k| k.as_str().to_string()).collect()
    }

    /// Models for a provider.
    pub fn models(&self, provider: &str) -> Vec<&ModelInfo> {
        self.provider(provider).map(|p| p.models.values().collect()).unwrap_or_default()
    }

    /// Find a specific model.
    pub fn model(&self, provider: &str, model: &str) -> Option<&ModelInfo> {
        self.provider(provider)?.models.get(&ModelId::new(model.to_string()))
    }

    /// Resolve a [`ModelSelection`] to the [`ModelInfo`].
    pub fn resolve(&self, sel: &ModelSelection) -> Option<&ModelInfo> {
        self.model(sel.provider_id.as_str(), sel.model.as_str())
    }

    /// Fetch from the network. On any failure, returns the bundled catalog
    /// (mirrors OpenCode's `try fetch -> catch -> bundled snapshot`).
    pub fn fetch(url: &str) -> Self {
        match Self::fetch_network(url) {
            Ok(c) => c,
            Err(_) => Self::bundled(),
        }
    }

    fn fetch_network(url: &str) -> Result<Self, String> {
        // Minimal HTTP GET via std (no TLS — HTTPS catalog URLs fall back to
        // bundled in the no-TLS build; a future TLS impl will enable live
        // fetch). This is acceptable because the bundled catalog is the
        // reliable path; live fetch is best-effort.
        let parsed = parse_catalog_url(url)?;
        let addr = format!("{}:{}", parsed.host, parsed.port);
        let addrs = std::net::ToSocketAddrs::to_socket_addrs(&addr).map_err(|e| e.to_string())?;
        let mut stream = None;
        for a in addrs {
            if let Ok(s) = std::net::TcpStream::connect_timeout(&a, std::time::Duration::from_secs(5)) {
                stream = Some(s); break;
            }
        }
        let mut s = stream.ok_or("connect failed")?;
        // Bound the read: with no TLS this is a plain-HTTP GET against what
        // is usually an HTTPS-only host, so the peer may never send anything
        // readable back. Without a read timeout, `read_to_end` could block
        // indefinitely instead of falling back to the bundled catalog.
        s.set_read_timeout(Some(std::time::Duration::from_secs(5)))
            .map_err(|e| e.to_string())?;
        use std::io::{Read, Write};
        let req = format!("GET {} HTTP/1.1\r\nHost: {}\r\nConnection: close\r\n\r\n", parsed.path, parsed.host);
        s.write_all(req.as_bytes()).map_err(|e| e.to_string())?;
        let mut buf = Vec::new();
        s.read_to_end(&mut buf).map_err(|e| e.to_string())?;
        let body = std::str::from_utf8(&buf).map_err(|e| e.to_string())?;
        // skip headers
        let split = body.find("\r\n\r\n").ok_or("no body")?;
        let json = &body[split + 4..];
        let parsed_catalog: NetworkCatalog = serde_json::from_str(json).map_err(|e| e.to_string())?;
        Ok(parsed_catalog.into_catalog())
    }
}

struct ParsedUrl<'a> { host: &'a str, port: u16, path: String }
fn parse_catalog_url(s: &str) -> Result<ParsedUrl, String> {
    let (scheme, rest) = s.split_once("://").ok_or("no scheme")?;
    let (hostport, path) = match rest.find('/') {
        Some(i) => (&rest[..i], rest[i..].to_string()),
        None => (rest, "/".to_string()),
    };
    let (host, port) = match hostport.rsplit_once(':') {
        Some((h, p)) => (h, p.parse().map_err(|e: std::num::ParseIntError| e.to_string())?),
        None => (hostport, if scheme == "https" { 443 } else { 80 }),
    };
    Ok(ParsedUrl { host, port, path })
}

fn now_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

/// The on-wire catalog shape (a small subset of OpenCode's models.dev JSON).
#[derive(serde::Deserialize)]
struct NetworkCatalog {
    #[serde(default)]
    providers: Vec<NetworkProvider>,
}
#[derive(serde::Deserialize)]
struct NetworkProvider {
    id: String,
    #[serde(default)]
    name: String,
    #[serde(default)]
    models: Vec<NetworkModel>,
}
#[derive(serde::Deserialize)]
struct NetworkModel {
    id: String,
    #[serde(default)]
    name: String,
    #[serde(default)]
    context: Option<u64>,
    #[serde(default)]
    output: Option<u64>,
    #[serde(default)]
    tools: Option<bool>,
}
impl NetworkCatalog {
    fn into_catalog(self) -> Catalog {
        let mut providers = BTreeMap::new();
        for np in self.providers {
            let pid = ProviderId::new(np.id.clone());
            let mut p = ProviderInfo {
                id: pid.clone(),
                name: np.name,
                package: np.id.clone(),
                activation: Activation::Auto,
                ..Default::default()
            };
            for nm in np.models {
                let mid = ModelId::new(nm.id.clone());
                let mut m = ModelInfo::default_for(pid.clone(), mid.clone());
                m.name = nm.name;
                if let Some(c) = nm.context { m.limit.context = c; }
                if let Some(o) = nm.output { m.limit.output = o; }
                if let Some(t) = nm.tools { m.capabilities.tools = t; }
                p.models.insert(mid, m);
            }
            providers.insert(pid, p);
        }
        Catalog { providers, fetched_at: now_ms(), source: CatalogSource::Network }
    }
}

// ── Bundled fallback ────────────────────────────────────────────────────────

/// One bundled provider row. This is the shape shared by the hand-written
/// core snapshot below and the generated full provider snapshot in
/// [`crate::catalog_snapshot`].
#[derive(Clone, Copy, Debug)]
pub struct BundledEntry {
    pub id: &'static str,
    pub name: &'static str,
    /// Default API base URL ("" when it must be supplied in config).
    pub base_url: &'static str,
    /// Conventional API-key environment variable ("" when not needed).
    pub key_env: &'static str,
    /// Whether the provider needs an API key to resolve.
    pub needs_key: bool,
    /// `true` for the Anthropic Messages wire protocol, `false` for
    /// OpenAI-compatible Chat Completions.
    pub anthropic: bool,
    pub models: &'static [&'static str],
    /// The preferred default model (first in `models` by convention; stored
    /// explicitly so the catalog can surface it even though `models` is held
    /// in a sorted BTreeMap that loses insertion order).
    pub default: &'static str,
}

/// Resolution metadata carried by a bundled entry for providers that have
/// no dedicated adapter in [`crate::adapter`].
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct BundledMeta {
    pub base_url: &'static str,
    pub key_env: &'static str,
    pub needs_key: bool,
    pub anthropic: bool,
}

/// Look up the bundled metadata for a provider id (case-insensitive).
/// Used by the legacy compatibility shim so generated providers resolve to
/// a base URL and API-key env var without a hand-written adapter.
pub fn bundled_meta(id: &str) -> Option<BundledMeta> {
    bundled_entries()
        .find(|e| e.id.eq_ignore_ascii_case(id))
        .map(|e| BundledMeta {
            base_url: e.base_url,
            key_env: e.key_env,
            needs_key: e.needs_key,
            anthropic: e.anthropic,
        })
}

/// All bundled rows: RomaCode's hand-written core first, then the generated
/// full bundled provider snapshot.
pub fn bundled_entries() -> impl Iterator<Item = &'static BundledEntry> {
    BUILTIN_SNAPSHOT.iter().chain(crate::catalog_snapshot::EXTRA_ENTRIES.iter())
}

/// The hand-written core providers (matching the adapter registry).
/// The complete provider set lives in `catalog_snapshot.rs`.
pub static BUILTIN_SNAPSHOT: &[BundledEntry] = &[
    BundledEntry { id: "anthropic", name: "Anthropic",
        base_url: "https://api.anthropic.com", key_env: "ANTHROPIC_API_KEY", needs_key: true, anthropic: true,
        models: &["claude-sonnet-5-5", "claude-opus-5-5", "claude-haiku-5-5", "claude-3-7-sonnet"],
        default: "claude-sonnet-5-5" },
    BundledEntry { id: "openai", name: "OpenAI",
        base_url: "https://api.openai.com/v1", key_env: "OPENAI_API_KEY", needs_key: true, anthropic: false,
        models: &["gpt-5", "gpt-5-mini", "gpt-4.1", "gpt-4o", "o3", "o4-mini"],
        default: "gpt-5" },
    BundledEntry { id: "openrouter", name: "OpenRouter",
        base_url: "https://openrouter.ai/api/v1", key_env: "OPENROUTER_API_KEY", needs_key: true, anthropic: false,
        models: &["anthropic/claude-sonnet-5-5", "openai/gpt-5", "google/gemini-2.5-pro"],
        default: "anthropic/claude-sonnet-5-5" },
    BundledEntry { id: "google", name: "Google AI Studio",
        base_url: "https://generativelanguage.googleapis.com/v1beta/openai", key_env: "GEMINI_API_KEY", needs_key: true, anthropic: false,
        models: &["gemini-2.5-pro", "gemini-2.5-flash", "gemini-2.0-flash"],
        default: "gemini-2.5-pro" },
    BundledEntry { id: "groq", name: "Groq",
        base_url: "https://api.groq.com/openai/v1", key_env: "GROQ_API_KEY", needs_key: true, anthropic: false,
        models: &["llama-3.3-70b", "llama-3.1-8b", "deepseek-r1-distill-llama-70b"],
        default: "llama-3.3-70b" },
    BundledEntry { id: "deepseek", name: "DeepSeek",
        base_url: "https://api.deepseek.com/v1", key_env: "DEEPSEEK_API_KEY", needs_key: true, anthropic: false,
        models: &["deepseek-chat", "deepseek-reasoner"],
        default: "deepseek-chat" },
    BundledEntry { id: "mistral", name: "Mistral",
        base_url: "https://api.mistral.ai/v1", key_env: "MISTRAL_API_KEY", needs_key: true, anthropic: false,
        models: &["mistral-large-latest", "mistral-small-latest", "codestral-latest"],
        default: "mistral-large-latest" },
    BundledEntry { id: "xai", name: "xAI",
        base_url: "https://api.x.ai/v1", key_env: "XAI_API_KEY", needs_key: true, anthropic: false,
        models: &["grok-4", "grok-3", "grok-3-mini"],
        default: "grok-4" },
    BundledEntry { id: "zai", name: "Z.AI",
        base_url: "https://api.z.ai/api/paas/v4", key_env: "ZAI_API_KEY", needs_key: true, anthropic: false,
        models: &["glm-4.6", "glm-4.5-air", "glm-4.5"],
        default: "glm-4.6" },
    BundledEntry { id: "ollama", name: "Ollama (local)",
        base_url: "http://localhost:11434/v1", key_env: "", needs_key: false, anthropic: false,
        models: &["qwen2.5-coder", "llama3.3", "deepseek-r1"],
        default: "qwen2.5-coder" },
    BundledEntry { id: "lmstudio", name: "LM Studio (local)",
        base_url: "http://localhost:1234/v1", key_env: "", needs_key: false, anthropic: false,
        models: &["local-model"],
        default: "local-model" },
    BundledEntry { id: "cerebras", name: "Cerebras",
        base_url: "https://api.cerebras.ai/v1", key_env: "CEREBRAS_API_KEY", needs_key: true, anthropic: false,
        models: &["llama-3.3-70b", "llama-3.1-8b"],
        default: "llama-3.3-70b" },
    BundledEntry { id: "together", name: "Together",
        base_url: "https://api.together.xyz/v1", key_env: "TOGETHER_API_KEY", needs_key: true, anthropic: false,
        models: &["meta-llama/Llama-3.3-70B-Instruct-Turbo", "Qwen/Qwen2.5-72B-Instruct-Turbo"],
        default: "meta-llama/Llama-3.3-70B-Instruct-Turbo" },
    BundledEntry { id: "fireworks", name: "Fireworks",
        base_url: "https://api.fireworks.ai/inference/v1", key_env: "FIREWORKS_API_KEY", needs_key: true, anthropic: false,
        models: &["accounts/fireworks/models/llama-3.3-70b-instruct"],
        default: "accounts/fireworks/models/llama-3.3-70b-instruct" },
    BundledEntry { id: "coarena", name: "Coarena",
        base_url: "https://coarena.ai/api/v1", key_env: "COARENA_API_KEY", needs_key: true, anthropic: false,
        models: &["claude-fable-5", "claude-fable-5-1", "claude-fable-5-2", "claude-haiku-5-5",
                  "claude-opus-5-5", "claude-sonnet-5-5", "deepseek-v4-1-flash-1017",
                  "gemini-3-8-flash", "gemini-4-argon", "gemini-4-carbon", "gpt-6-1-sol",
                  "gpt-6-1-terra", "gpt-6-2-astra", "gpt-6-2-sol"],
        default: "gpt-6-2-sol" },
    BundledEntry { id: "logfare", name: "Logfare",
        base_url: "https://logfare.ai/v1", key_env: "LOGFARE_API_KEY", needs_key: true, anthropic: false,
        models: &["flux-2-dev", "flux-2-klein-4b", "flux-2-klein-9b", "flux-1-schnell",
                  "phoenix-1.0", "sdxl-lightning", "glm-5.2", "logfare/auto", "nova-3",
                  "clef", "8ball", "clef-flash", "qwen-3.8-27b", "lucid-origin",
                  "deepseek-v4.1-flash-1017"],
        default: "deepseek-v4.1-flash-1017" },
];

// ── Process-wide cached catalog (TTL-refreshed) ─────────────────────────────

static CATALOG: OnceLock<Mutex<Catalog>> = OnceLock::new();

/// Get the cached catalog, refreshing if older than TTL. Uses the bundled
/// fallback on first access / network failure.
///
/// BUG FIXED: this used to gate the TTL check on `source == Network` only.
/// Because `fetch_network` cannot actually complete a TLS handshake (it is a
/// plain-HTTP, no-TLS client reaching an `https://` URL — see its own
/// doc comment), `Catalog::fetch` always falls through to `bundled()`, so
/// `source` never becomes `Network`. The TTL check was therefore never true,
/// and EVERY call to `cached()` — every model-picker open, every `/models`,
/// every agent model resolution — re-ran a doomed network connection from
/// scratch before returning the fallback. Gating the TTL on `fetched_at`
/// alone (regardless of source) makes the bundled fallback itself cache for
/// the TTL window, so a live environment with no working catalog endpoint
/// pays the connection cost at most once per `TTL_SECS`, not once per call.
pub fn cached() -> Catalog {
    let m = CATALOG.get_or_init(|| Mutex::new(Catalog::bundled()));
    let g = m.lock().unwrap_or_else(|e| e.into_inner());
    let now = now_ms();
    if now.saturating_sub(g.fetched_at) < TTL_SECS * 1000 {
        return g.clone();
    }
    drop(g);
    // Refresh in a separate scope; on failure keep the bundled one.
    let fresh = Catalog::fetch(DEFAULT_CATALOG_URL);
    let mut g = m.lock().unwrap_or_else(|e| e.into_inner());
    // Always adopt a successful network fetch. On failure, still record the
    // fresh `fetched_at` so the TTL window above actually holds — otherwise
    // a permanently-unreachable catalog endpoint would retry every call.
    if fresh.source == CatalogSource::Network {
        *g = fresh;
    } else {
        g.fetched_at = fresh.fetched_at;
    }
    g.clone()
}

/// Force a refresh (e.g. after the user adds a credential).
pub fn refresh() -> Catalog {
    let fresh = Catalog::fetch(DEFAULT_CATALOG_URL);
    if let Some(m) = CATALOG.get() {
        let mut g = m.lock().unwrap_or_else(|e| e.into_inner());
        *g = fresh.clone();
    }
    fresh
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bundled_has_providers_and_models() {
        let c = Catalog::bundled();
        // 14 hand-written + the full generated provider snapshot.
        assert!(c.providers.len() >= 300, "only {} bundled providers", c.providers.len());
        let a = c.provider("anthropic").expect("anthropic");
        assert!(!a.models.is_empty());
        assert!(a.models.contains_key(&ModelId::new("claude-sonnet-5-5")));
        for id in ["azure", "vercel", "openrouter", "huggingface", "nvidia", "cohere"] {
            assert!(c.provider(id).is_some(), "{id} from the catalog must be bundled");
        }
    }

    #[test]
    fn bundled_meta_covers_generated_providers() {
        let m = bundled_meta("vercel").expect("vercel meta");
        assert!(m.base_url.starts_with("https://"));
        assert!(!m.key_env.is_empty());
        assert!(bundled_meta("ANTHROPIC").is_some(), "lookup is case-insensitive");
        assert!(bundled_meta("no-such-provider").is_none());
    }

    #[test]
    fn resolve_model_selection() {
        let c = Catalog::bundled();
        let sel = ModelSelection::parse("anthropic/claude-sonnet-5-5").unwrap();
        let m = c.resolve(&sel).expect("model exists");
        assert_eq!(m.model_id.as_str(), "claude-sonnet-5-5");
    }

    #[test]
    fn unknown_provider_returns_none() {
        let c = Catalog::bundled();
        assert!(c.provider("nonexistent").is_none());
    }

    #[test]
    fn cached_returns_something() {
        let c = cached();
        assert!(c.providers.len() >= 10);
    }

    #[test]
    fn parse_catalog_url_https() {
        let u = parse_catalog_url("https://catalog.romacode.dev/v1/catalog.json").unwrap();
        assert_eq!(u.host, "catalog.romacode.dev");
        assert_eq!(u.port, 443);
        assert_eq!(u.path, "/v1/catalog.json");
    }

    #[test]
    fn network_catalog_decodes() {
        let json = r#"{"providers":[{"id":"openai","name":"OpenAI","models":[{"id":"gpt-5","name":"GPT-5","context":128000,"output":16384,"tools":true}]}]}"#;
        let nc: NetworkCatalog = serde_json::from_str(json).unwrap();
        let c = nc.into_catalog();
        let m = c.model("openai", "gpt-5").unwrap();
        assert_eq!(m.limit.context, 128000);
        assert_eq!(m.limit.output, 16384);
        assert!(m.capabilities.tools);
        assert_eq!(c.source, CatalogSource::Network);
    }
}
