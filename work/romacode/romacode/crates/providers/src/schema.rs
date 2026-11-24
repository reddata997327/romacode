//! Schema types — a pure-Rust port of OpenCode's `packages/schema` provider/
//! model/config/credential type hierarchy.
//!
//! Every type here mirrors the OpenCode TypeScript interface of the same name
//! (see `packages/schema/src/{provider,model,config,credential}.ts`). Branded
//! TS strings (`Provider.ID`, `Model.ID`, …) become Rust newtypes over
//! `String`; `Schema.StructWithRest` (fixed struct + arbitrary rest map)
//! becomes a struct with a `#[serde(flatten)] extra: HashMap<String, Value>`
//! tail; tagged unions become `#[serde(tag = "type")]` enums.
//!
//! These types are the single source of truth for what a provider and a model
//! look like across RomaCode. The server config, the TUI settings panel, the
//! agent loop, and the CLI all read and write these — nothing else.

use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::BTreeMap;

// ── Branded-id newtypes ────────────────────────────────────────────────────

macro_rules! branded_id {
    ($name:ident, $doc:literal) => {
        /// $doc
        #[derive(Clone, Debug, Default, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
        #[serde(transparent)]
        pub struct $name(pub String);
        impl $name {
            pub fn new(s: impl Into<String>) -> Self { Self(s.into()) }
            pub fn as_str(&self) -> &str { &self.0 }
            pub fn is_empty(&self) -> bool { self.0.is_empty() }
        }
        impl std::fmt::Display for $name {
            fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
                f.write_str(&self.0)
            }
        }
        impl std::str::FromStr for $name {
            type Err = std::convert::Infallible;
            fn from_str(s: &str) -> Result<Self, Self::Err> { Ok(Self(s.to_string())) }
        }
        impl From<&str> for $name { fn from(s: &str) -> Self { Self(s.to_string()) } }
        impl From<String> for $name { fn from(s: String) -> Self { Self(s) } }
    };
}

branded_id!(ProviderId, "Provider id (OpenCode `Provider.ID`).");
branded_id!(ModelId, "Model id (OpenCode `Model.ID`).");
branded_id!(VariantId, "Model variant id (OpenCode `Model.VariantID`).");
branded_id!(Family, "Model family (OpenCode `Model.Family`).");
branded_id!(IntegrationId, "Integration id (OpenCode `Integration.ID`).");
branded_id!(IntegrationMethodId, "Integration method id.");
branded_id!(CredentialId, "Credential id (`cred_<asc>`).");

/// USD cost per million tokens. OpenCode brands this `Money.USDPerMillionTokens`
/// (a finite non-NaN number). We validate finiteness on construction.
#[derive(Clone, Copy, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(transparent)]
pub struct UsdPerMillionTokens(pub f64);

impl UsdPerMillionTokens {
    pub fn new(v: f64) -> Self { Self(if v.is_finite() { v } else { 0.0 }) }
    pub fn get(self) -> f64 { self.0 }
}

// ── Enums shared across provider + model ──────────────────────────────────

/// How a provider is activated. Mirrors OpenCode `Provider.Activation`.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Activation {
    /// Discovered automatically (env var present, integration connected, …).
    #[default]
    Auto,
    /// Forced on by config even with no credential.
    Enabled,
    /// Forced off by config.
    Disabled,
}

/// Wire transport. Mirrors OpenCode `Provider.Transport`.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Transport {
    #[default]
    Http,
    Websocket,
}

/// Body encoding. Mirrors OpenCode `Provider.Encoding`.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Encoding {
    Zstd,
    Gzip,
    #[default]
    None,
}

/// Compaction strategy. Mirrors OpenCode `Provider.Compaction` tagged union.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "lowercase")]
pub enum Compaction {
    Summary,
    Native,
}

/// Cache TTL pattern (`/^\d+(ms|s|m|h|d)$/`, e.g. `"5m"`, `"1h"`).
pub type CacheTtl = String;

/// Cache warming config.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct CacheWarming {
    pub interval: Option<String>,
    pub duration: Option<String>,
}

/// Cache config. Mirrors OpenCode `Provider.Cache`.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Cache {
    pub ttl: Option<CacheTtl>,
    /// `true` is the simple form; an object is the detailed form.
    /// Serialized as either a bool or the object depending on contents.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub warming: Option<WarmingFlag>,
}

/// A warming flag that may be a plain bool or the detailed object.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(untagged)]
pub enum WarmingFlag {
    Bool(bool),
    Detailed(CacheWarming),
}

/// TLS config. Mirrors OpenCode `Provider.Tls`.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Tls {
    pub cert: String,
    pub key: String,
    pub passphrase: Option<String>,
}

/// A timeout that may be a number of milliseconds or `false` (disabled).
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
#[serde(untagged)]
pub enum Timeout {
    Ms(u64),
    Disabled(bool),
}
impl Default for Timeout {
    fn default() -> Self { Timeout::Disabled(false) }
}

// ── Provider.Settings ──────────────────────────────────────────────────────

/// OpenCode `Provider.Settings`: named timeout/compaction/cache/transport
/// fields plus an arbitrary provider-specific rest map.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct ProviderSettings {
    pub timeout: Option<Timeout>,
    pub header_timeout: Option<Timeout>,
    pub chunk_timeout: Option<Timeout>,
    pub compaction: Option<Compaction>,
    pub cache: Option<Cache>,
    pub transport: Option<Transport>,
    pub encoding: Option<Encoding>,
    pub tls: Option<Tls>,
    /// Provider-specific options (the `StructWithRest` tail).
    #[serde(flatten)]
    pub extra: BTreeMap<String, Value>,
}

// ── Compatibility (provider-level + model-level) ───────────────────────────

/// Which field carries reasoning output.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct ReasoningField(pub String);

/// Which field sets the max output tokens.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MaxTokensField {
    #[default]
    MaxCompletionTokens,
    MaxTokens,
}

/// How media is attached to a request.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum MediaInput {
    Assets,
    State,
    #[default]
    Root,
}

/// Cache wire format.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum CacheFormat {
    #[default]
    Anthropic,
    Alibaba,
}

/// Model-level compatibility (superset of `ProviderCompatibility`: adds
/// `supports_thinking_block_binding` and `supports_effort_updates`).
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Compatibility {
    pub reasoning_field: Option<String>,
    pub require_reasoning: Option<bool>,
    pub max_tokens_field: Option<MaxTokensField>,
    pub require_finish_reason: Option<bool>,
    pub require_assistant_after_tool: Option<bool>,
    pub supports_prompt_cache_key: Option<bool>,
    pub stream_usage: Option<bool>,
    pub cache_format: Option<CacheFormat>,
    pub supports_thinking_block_binding: Option<bool>,
    pub supports_effort_updates: Option<bool>,
    pub media_input: Option<MediaInput>,
}

/// Provider-level compatibility = `Compatibility` minus the two thinking/effort
/// flags (those are model-only).
pub type ProviderCompatibility = Compatibility;

// ── Provider.Info ──────────────────────────────────────────────────────────

/// OpenCode `Provider.Info`: the user-facing catalog entry.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct ProviderInfo {
    pub id: ProviderId,
    pub canonical: Option<ProviderId>,
    pub integration_id: Option<IntegrationId>,
    pub name: String,
    pub activation: Activation,
    pub package: String,
    pub compatibility: Option<ProviderCompatibility>,
    pub settings: Option<ProviderSettings>,
    pub headers: Option<BTreeMap<String, String>>,
    pub body: Option<BTreeMap<String, Value>>,
    /// The preferred default model for this provider (used when the config
    /// does not pin one). Set by the bundled catalog; not in OpenCode's
    /// schema but required because `models` is a sorted map (loses insertion
    /// order).
    pub default_model: Option<ModelId>,
    /// Models offered by this provider (filled by the catalog, not by config).
    #[serde(skip)]
    pub models: BTreeMap<ModelId, ModelInfo>,
}

/// The request envelope built from settings + headers + body.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct ProviderRequest {
    pub settings: ProviderSettings,
    pub headers: BTreeMap<String, String>,
    pub body: BTreeMap<String, Value>,
}

// ── Model.Info + nested types ──────────────────────────────────────────────

/// What a model can do. Mirrors OpenCode `Model.Capabilities`.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Capabilities {
    pub tools: bool,
    pub freeform: Vec<String>,
    pub input: Vec<String>,
    pub output: Vec<String>,
    pub temperature: Option<bool>,
}

impl Capabilities {
    /// OpenCode `Model.Capabilities.default()`.
    pub fn default_capabilities() -> Self {
        Self {
            tools: true,
            freeform: Vec::new(),
            input: vec!["text".into(), "image".into()],
            output: vec!["text".into()],
            temperature: None,
        }
    }
}

/// Per-tier cost. Mirrors OpenCode `Model.Cost`.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Cost {
    pub tier: Option<CostTier>,
    pub input: UsdPerMillionTokens,
    pub output: UsdPerMillionTokens,
    pub cache: CostCache,
}

/// A cost tier (context-window-size bucket).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct CostTier {
    #[serde(rename = "type")]
    pub kind: CostTierKind,
    pub size: u64,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum CostTierKind { Context }

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct CostCache {
    pub read: UsdPerMillionTokens,
    pub write: UsdPerMillionTokens,
}

/// A model variant (e.g. reasoning on/off). Mirrors OpenCode `Model.Variant`.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Variant {
    pub id: VariantId,
    pub settings: Option<ProviderSettings>,
    pub headers: Option<BTreeMap<String, String>>,
    pub body: Option<BTreeMap<String, Value>>,
}

/// Model token limits.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct ModelLimit {
    pub context: u64,
    pub input: Option<u64>,
    pub output: u64,
}
impl Default for ModelLimit {
    fn default() -> Self {
        Self { context: 200_000, input: None, output: 32_000 }
    }
}

/// Model lifecycle status.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ModelStatus {
    Alpha,
    Beta,
    Deprecated,
    #[default]
    Active,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ModelType {
    #[default]
    Chat,
    Decision,
    Image,
    Video,
}

/// OpenCode `Model.Info`: the full model descriptor.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct ModelInfo {
    pub id: ModelId,
    pub model_id: ModelId,
    pub provider_id: ProviderId,
    pub canonical: Option<ProviderId>,
    pub r#type: Option<ModelType>,
    pub family: Option<Family>,
    pub name: String,
    pub compatibility: Option<Compatibility>,
    pub package: Option<String>,
    pub settings: Option<ProviderSettings>,
    pub headers: Option<BTreeMap<String, String>>,
    pub body: Option<BTreeMap<String, Value>>,
    pub capabilities: Capabilities,
    pub variants: Vec<Variant>,
    pub time: ModelTime,
    pub cost: Vec<Cost>,
    pub status: ModelStatus,
    pub enabled: bool,
    pub limit: ModelLimit,
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct ModelTime {
    pub released: u64,
}

impl ModelInfo {
    /// OpenCode `Model.Info.default(providerID, id)`.
    pub fn default_for(provider: ProviderId, id: ModelId) -> Self {
        Self {
            id: id.clone(),
            model_id: id,
            provider_id: provider,
            name: String::new(),
            capabilities: Capabilities::default_capabilities(),
            variants: Vec::new(),
            time: ModelTime { released: 0 },
            cost: Vec::new(),
            status: ModelStatus::Active,
            enabled: true,
            limit: ModelLimit::default(),
            ..Default::default()
        }
    }
}

// ── Config: ConfigProvider.Info + ConfigModel.Selection + ModelConfig ───────

/// A provider override entry in `config.providers`.
/// Mirrors OpenCode `ConfigProvider.Info`.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct ConfigProvider {
    pub canonical: Option<ProviderId>,
    pub name: Option<String>,
    pub env: Vec<String>,
    pub package: Option<String>,
    pub compatibility: Option<ProviderCompatibility>,
    pub settings: Option<ProviderSettings>,
    pub headers: Option<BTreeMap<String, String>>,
    pub body: Option<BTreeMap<String, Value>>,
    pub models: Option<BTreeMap<ModelId, ModelConfig>>,
}

/// Per-model config in `config.providers.<id>.models`.
/// Mirrors OpenCode `ModelConfig`.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct ModelConfig {
    pub model_id: Option<ModelId>,
    pub r#type: Option<ModelType>,
    pub family: Option<Family>,
    pub name: Option<String>,
    pub release_date: Option<String>,
    pub compatibility: Option<Compatibility>,
    pub package: Option<String>,
    pub settings: Option<ProviderSettings>,
    pub headers: Option<BTreeMap<String, String>>,
    pub body: Option<BTreeMap<String, Value>>,
    pub capabilities: Option<ConfigModelCapabilities>,
    pub variants: Option<Vec<Variant>>,
    pub cost: Option<ConfigModelCost>,
    pub disabled: Option<bool>,
    pub limit: Option<ConfigModelLimit>,
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct ConfigModelCapabilities {
    pub tools: Option<bool>,
    pub freeform: Option<Vec<String>>,
    pub input: Option<Vec<String>>,
    pub output: Option<Vec<String>>,
    pub temperature: Option<bool>,
    pub reasoning: Option<Reasoning>,
}

/// Mirrors OpenCode `ConfigModel.Reasoning`.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "supported")]
pub enum Reasoning {
    #[serde(rename = "false")]
    NotSupported,
    #[serde(rename = "true")]
    Supported {
        toggle: Option<bool>,
        effort: Option<Vec<String>>,
        budget: Option<ReasoningBudget>,
    },
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct ReasoningBudget {
    pub min: Option<u64>,
    pub max: Option<u64>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(untagged)]
pub enum ConfigModelCost {
    One(Cost),
    Many(Vec<Cost>),
}

impl Default for ConfigModelCost {
    fn default() -> Self { Self::Many(Vec::new()) }
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct ConfigModelLimit {
    pub context: Option<u64>,
    pub input: Option<u64>,
    pub output: Option<u64>,
}

/// The selected model. Mirrors OpenCode `ConfigModel.Selection`.
/// Accepts either `"provider/model[#variant]"` or the explicit object form.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct ModelSelection {
    pub provider_id: ProviderId,
    pub model: ModelId,
    pub variant: Option<VariantId>,
}

impl ModelSelection {
    /// Parse `"provider/model[#variant]"` (OpenCode short form).
    pub fn parse(s: &str) -> Option<Self> {
        let s = s.trim();
        if s.is_empty() { return None; }
        let (provider_part, rest) = s.split_once('/')?;
        // provider must not contain '#' or '/'
        if provider_part.contains('#') || provider_part.contains('/') || provider_part.is_empty() {
            return None;
        }
        // model is everything up to the first '#' (variant), and must not
        // contain a further '/' (catches "a/b/c").
        let (model_part, variant) = match rest.split_once('#') {
            Some((m, v)) => (m, if v.is_empty() { None } else { Some(VariantId::new(v)) }),
            None => (rest, None),
        };
        if model_part.is_empty() || model_part.contains('/') { return None; }
        Some(Self {
            provider_id: ProviderId::new(provider_part),
            model: ModelId::new(model_part),
            variant,
        })
    }

    /// Serialize back to `"provider/model[#variant]"`.
    pub fn to_short(&self) -> String {
        match &self.variant {
            Some(v) => format!("{}/{}#{}", self.provider_id, self.model, v),
            None => format!("{}/{}", self.provider_id, self.model),
        }
    }
}

// ── Credential.Value (tagged union: OAuth | Key | External) ─────────────────

/// Mirrors OpenCode `Credential.Value`.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "lowercase")]
pub enum CredentialValue {
    Oauth(CredentialOauth),
    Key(CredentialKey),
    External(CredentialExternal),
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct CredentialOauth {
    pub method_id: IntegrationMethodId,
    pub refresh: String,
    pub access: String,
    pub expires: u64,
    pub metadata: Option<BTreeMap<String, Value>>,
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct CredentialKey {
    pub key: String,
    pub metadata: Option<BTreeMap<String, Value>>,
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct CredentialExternal {
    pub method_id: IntegrationMethodId,
    pub metadata: Option<BTreeMap<String, Value>>,
}

/// One stored credential entry.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct CredentialEntry {
    pub id: CredentialId,
    pub integration_id: IntegrationId,
    pub label: String,
    pub active: bool,
    pub value: Option<CredentialValue>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn model_selection_roundtrips_short_form() {
        let s = ModelSelection::parse("anthropic/claude-sonnet-5-5").unwrap();
        assert_eq!(s.provider_id.as_str(), "anthropic");
        assert_eq!(s.model.as_str(), "claude-sonnet-5-5");
        assert!(s.variant.is_none());
        assert_eq!(s.to_short(), "anthropic/claude-sonnet-5-5");
    }

    #[test]
    fn model_selection_parses_variant() {
        let s = ModelSelection::parse("openai/gpt-5#reasoning").unwrap();
        assert_eq!(s.model.as_str(), "gpt-5");
        assert_eq!(s.variant.as_ref().unwrap().as_str(), "reasoning");
        assert_eq!(s.to_short(), "openai/gpt-5#reasoning");
    }

    #[test]
    fn model_selection_rejects_malformed() {
        assert!(ModelSelection::parse("").is_none());
        assert!(ModelSelection::parse("noprovider").is_none());
        assert!(ModelSelection::parse("/nomodel").is_none());
        assert!(ModelSelection::parse("a/b/c").is_none()); // slash in model
    }

    #[test]
    fn provider_info_roundtrips_json() {
        let p = ProviderInfo {
            id: ProviderId::new("anthropic"),
            name: "Anthropic".into(),
            package: "anthropic".into(),
            ..Default::default()
        };
        let j = serde_json::to_string(&p).unwrap();
        let back: ProviderInfo = serde_json::from_str(&j).unwrap();
        assert_eq!(back.id.as_str(), "anthropic");
    }

    #[test]
    fn model_info_default_for() {
        let m = ModelInfo::default_for(ProviderId::new("openai"), ModelId::new("gpt-5"));
        assert_eq!(m.limit.context, 200_000);
        assert_eq!(m.limit.output, 32_000);
        assert!(m.enabled);
        assert_eq!(m.status, ModelStatus::Active);
    }

    #[test]
    fn credential_value_key_tag() {
        let v = CredentialValue::Key(CredentialKey { key: "sk-xxx".into(), metadata: None });
        let j = serde_json::to_string(&v).unwrap();
        assert!(j.contains(r#""type":"key""#));
        let back: CredentialValue = serde_json::from_str(&j).unwrap();
        match back {
            CredentialValue::Key(k) => assert_eq!(k.key, "sk-xxx"),
            _ => panic!("wrong variant"),
        }
    }

    #[test]
    fn compaction_tagged_union() {
        let c = Compaction::Summary;
        let j = serde_json::to_string(&c).unwrap();
        assert_eq!(j, r#"{"type":"summary"}"#);
    }

    #[test]
    fn provider_settings_extra_tail() {
        let s: ProviderSettings = serde_json::from_str(
            r#"{"timeout":30000,"anthropic_beta":["output-128k-2025-02-19"]}"#
        ).unwrap();
        assert_eq!(s.timeout, Some(Timeout::Ms(30000)));
        assert!(s.extra.contains_key("anthropic_beta"));
    }

    #[test]
    fn reasoning_enum_tags() {
        let r = Reasoning::NotSupported;
        let j = serde_json::to_string(&r).unwrap();
        assert_eq!(j, r#"{"supported":"false"}"#);
    }

    #[test]
    fn cost_usd_finite() {
        assert_eq!(UsdPerMillionTokens::new(f64::NAN).get(), 0.0);
        assert_eq!(UsdPerMillionTokens::new(3.0).get(), 3.0);
    }
}
