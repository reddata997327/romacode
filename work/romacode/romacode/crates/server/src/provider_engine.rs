//! Provider engine — the bridge between RomaCode's existing config/agent-loop
//! types and the new OpenCode-structured `romacode-providers` crate.
//!
//! This module is the load-bearing change for long-task resilience. It
//! replaces the old 4-retry no-jitter loop in `agent.rs::run_provider` with
//! the OpenCode retry policy (10 retries, exponential 2s→10s with jitter,
//! Retry-After header honoring, separate 1-hour budget for unreachable hosts,
//! 3-retry cap for timeouts). The policy is implemented in
//! `romacode_providers::client::RetryPolicy`; this module feeds it the
//! failures from the existing curl-backed transport and decides whether to
//! retry or give up — and crucially, it NEVER returns `Err` on a transient
//! failure until the full budget is exhausted.
//!
//! ## What this replaces
//!
//! Before: `agent.rs:343` ran a `loop { request_anthropic|request_openai;
//! if attempt > 4 || !transient { return Err(e) } }` — 4 retries, no jitter,
//! no Retry-After, and `return Err(e)` killed the turn.
//!
//! After: this module runs the same `request_anthropic|request_openai` calls
//! (curl-backed, unchanged transport) but wraps them in
//! [`retry_with_policy`], which uses `romacode_providers::RetryPolicy` to
//! classify each failure and decide retry/give-up per OpenCode's schedule.
//!
//! ## Catalog
//!
//! The provider/model catalog is now dynamic: `catalog()` returns the cached
//! [`romacode_providers::Catalog`] (network-fetched with a 5-minute TTL,
//! bundled fallback). The TUI settings panel and `providers_cli` read this
//! instead of the old static `KNOWN` slice.

use crate::config::{ApiKind, Config, Resolved};
use romacode_providers::{
    adapter_for, catalog, AuthScheme, FailureClass, OpenAiCompatible, ProviderAdapter,
    Protocol, RetryDecision, RetryPolicy,
};
use serde_json::Value;
use std::collections::BTreeMap;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

/// A retry callback the agent loop uses to emit a `RetryScheduled`-style
/// notice to the UI (mirrors OpenCode's `SessionEvent.RetryScheduled`).
pub type RetryNotifier = Box<dyn FnMut(&RetryDecision)>;

/// Run one provider call through the OpenCode retry policy.
///
/// `do_call` is the closure that performs the actual HTTP request (curl-backed
/// in the current transport). It returns `Ok(())` on success or `Err(msg)` on
/// failure; this function classifies the failure and retries per the policy,
/// calling `on_retry` for each scheduled retry.
///
/// Returns `Ok(())` if the call eventually succeeded, or `Err` with the final
/// failure reason if the budget was exhausted.
pub fn retry_with_policy<F>(
    cfg: &Config,
    cancel: &Arc<AtomicBool>,
    mut do_call: F,
    mut on_retry: impl FnMut(&RetryDecision),
) -> Result<(), String>
where
    F: FnMut() -> Result<(), String>,
{
    let allow_retry = cfg.network.max_retries > 0;
    let mut policy = RetryPolicy::new(allow_retry);
    loop {
        if cancel.load(Ordering::SeqCst) {
            return Err("cancelled".into());
        }
        match do_call() {
            Ok(()) => return Ok(()),
            Err(e) => {
                let class = classify(&e);
                let decision = policy.decide(class, None);
                if !decision.retry {
                    return Err(format!("{} ({})", e, decision.reason));
                }
                on_retry(&decision);
                sleep_or_cancel(decision.delay, cancel);
                if cancel.load(Ordering::SeqCst) {
                    return Err("cancelled during retry backoff".into());
                }
            }
        }
    }
}

/// Classify a RomaCode provider error string into the new `FailureClass`.
/// Delegates to `romacode_providers::FailureClass::from_response` so the
/// classification rules stay in one place.
pub fn classify(err: &str) -> FailureClass {
    FailureClass::from_response(0, "", err)
}

/// Sleep, but poll the cancel flag every 100ms so an interrupt is responsive.
fn sleep_or_cancel(d: Duration, cancel: &Arc<AtomicBool>) {
    if d.is_zero() { return; }
    let mut waited = Duration::ZERO;
    let step = Duration::from_millis(100);
    while waited < d {
        if cancel.load(Ordering::SeqCst) { return; }
        let remaining = d - waited;
        let s = step.min(remaining);
        std::thread::sleep(s);
        waited += s;
    }
}

/// Resolve the adapter for a RomaCode `Resolved` provider. Returns a boxed
/// `ProviderAdapter` (built-in for known ids, `OpenAiCompatible` for custom
/// base URLs).
///
/// BUG FIXED: `adapter_for` only hand-implements 16 providers. Every other
/// provider the bundled catalog knows about — ~300 entries in
/// `catalog_snapshot.rs`, including `nvidia`, `azure`, `vercel`,
/// `huggingface`, `cohere` — used to fall through to a bare
/// `OpenAiCompatible` with an EMPTY base URL whenever `r.base_url` wasn't
/// separately populated, and a guessed `<ID>_API_KEY` env var that may not
/// match the provider's real convention. A request built from that adapter
/// would hit `"/chat/completions"` with no host at all. This now consults
/// `romacode_providers::catalog::bundled_meta`, which already carries the
/// correct base URL, key env var, and wire protocol for every bundled
/// provider, before falling back further.
pub fn adapter_for_resolved(r: &Resolved) -> Box<dyn ProviderAdapter> {
    if let Some(a) = adapter_for(&r.id) {
        return a;
    }
    let meta = romacode_providers::catalog::bundled_meta(&r.id);
    let env_var = if !r.key_env.is_empty() {
        r.key_env.clone()
    } else if let Some(m) = &meta {
        if !m.key_env.is_empty() {
            m.key_env.to_string()
        } else {
            format!("{}_API_KEY", r.id.to_uppercase().replace('-', "_"))
        }
    } else {
        format!("{}_API_KEY", r.id.to_uppercase().replace('-', "_"))
    };
    let base_url = if !r.base_url.is_empty() {
        r.base_url.clone()
    } else if let Some(m) = &meta {
        m.base_url.to_string()
    } else {
        String::new()
    };
    if meta.as_ref().map(|m| m.anthropic).unwrap_or(false) {
        return Box::new(AnthropicCompatible {
            id: r.id.clone(),
            name: r.name.clone(),
            base_url,
            env_var,
        });
    }
    Box::new(OpenAiCompatible {
        id: r.id.clone(),
        name: r.name.clone(),
        base_url,
        env_var,
    })
}

/// A generic Anthropic-Messages-compatible adapter for catalog-known
/// providers that speak the Anthropic wire protocol but have no hand-written
/// adapter (mirrors `OpenAiCompatible`, just for the other protocol family).
pub struct AnthropicCompatible {
    pub id: String,
    pub name: String,
    pub base_url: String,
    pub env_var: String,
}
impl ProviderAdapter for AnthropicCompatible {
    fn id(&self) -> &str { &self.id }
    fn name(&self) -> &str { &self.name }
    fn default_base_url(&self) -> &str { &self.base_url }
    fn auth(&self) -> AuthScheme {
        AuthScheme::ApiKey { header: "x-api-key".into(), env_var: self.env_var.clone() }
    }
    fn protocol(&self) -> Protocol { Protocol::AnthropicMessages }
    fn build_request(
        &self,
        base_url: &str,
        api_key: Option<&str>,
        messages: &[Value],
        max_tokens: u32,
        model: &str,
        extra_headers: &BTreeMap<String, String>,
        stream: bool,
    ) -> romacode_providers::ResolvedRequest {
        let mut r = self.default_build_request(base_url, api_key, messages, max_tokens, model, extra_headers, stream);
        r.headers.insert("anthropic-version".into(), "2023-06-01".into());
        r
    }
}

/// Map a RomaCode `ApiKind` to the new `Protocol`. Used so the agent loop's
/// existing `r.kind` dispatch still works while the transport is curl-backed.
pub fn protocol_for_kind(kind: ApiKind) -> Protocol {
    match kind {
        ApiKind::Anthropic => Protocol::AnthropicMessages,
        ApiKind::OpenAi => Protocol::OpenAiChat,
    }
}

/// The active provider + model catalog (cached, TTL-refreshed). Reads through
/// to `romacode_providers::catalog::cached()`.
pub fn catalog() -> romacode_providers::Catalog {
    catalog::cached()
}

/// Force a catalog refresh (after a credential is added).
pub fn refresh_catalog() -> romacode_providers::Catalog {
    catalog::refresh()
}

/// Resolve the effective model id for a config + resolved provider, checking
/// the new dynamic catalog first and falling back to the resolved model.
pub fn effective_model(cfg: &Config, r: &Resolved) -> String {
    let sel_model = if r.model.is_empty() { cfg.model.clone() } else { r.model.clone() };
    if !sel_model.is_empty() {
        return sel_model;
    }
    // Fall back to the catalog's first model for this provider.
    let c = catalog();
    if let Some(p) = c.provider(&r.id) {
        if let Some((mid, _)) = p.models.iter().next() {
            return mid.as_str().to_string();
        }
    }
    String::new()
}

/// Build the headers for a provider request from `Resolved` + config.
/// Mirrors what `request_anthropic`/`request_openai` build inline today.
pub fn request_headers(r: &Resolved) -> BTreeMap<String, String> {
    let mut h = BTreeMap::new();
    let adapter = adapter_for_resolved(r);
    match adapter.auth() {
        AuthScheme::ApiKey { header, .. } => {
            if let Some(k) = &r.api_key { h.insert(header, k.clone()); }
        }
        AuthScheme::Bearer { .. } => {
            if let Some(k) = &r.api_key { h.insert("Authorization".into(), format!("Bearer {k}")); }
        }
        AuthScheme::None => {}
    }
    if r.kind == ApiKind::Anthropic {
        h.insert("anthropic-version".into(), "2023-06-01".into());
    }
    for (k, v) in &r.headers { h.insert(k.clone(), v.clone()); }
    h
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::Config;

    #[test]
    fn classify_transient() {
        assert_eq!(classify("http 502 bad gateway"), FailureClass::Transient);
        assert_eq!(classify("rate limit exceeded"), FailureClass::Transient);
        assert_eq!(classify("temporarily overloaded"), FailureClass::Transient);
    }

    #[test]
    fn classify_permanent() {
        assert_eq!(classify("401 unauthorized"), FailureClass::Permanent);
        assert_eq!(classify("unknown model"), FailureClass::Permanent);
    }

    #[test]
    fn classify_unreachable() {
        assert_eq!(classify("Name or service not known"), FailureClass::Unreachable);
        assert_eq!(classify("ENOTFOUND api.x.com"), FailureClass::Unreachable);
    }

    #[test]
    fn classify_timeout() {
        assert_eq!(classify("operation timed out"), FailureClass::Timeout);
    }

    #[test]
    fn retry_policy_runs_ten_times_then_gives_up() {
        let cfg = Config::default();
        let cancel = Arc::new(AtomicBool::new(false));
        let mut attempts = 0u32;
        let result = retry_with_policy(&cfg, &cancel, || {
            attempts += 1;
            Err("http 502 bad gateway".into())
        }, |_| {});
        assert!(result.is_err());
        // 1 initial + 10 retries = 11 calls total
        assert_eq!(attempts, 11);
    }

    #[test]
    fn retry_succeeds_on_third_attempt() {
        let cfg = Config::default();
        let cancel = Arc::new(AtomicBool::new(false));
        let mut attempts = 0u32;
        let result = retry_with_policy(&cfg, &cancel, || {
            attempts += 1;
            if attempts < 3 { Err("http 503".into()) } else { Ok(()) }
        }, |_| {});
        assert!(result.is_ok(), "expected Ok, got Err: {:?}", result.err());
        assert_eq!(attempts, 3);
    }

    #[test]
    fn retry_does_not_retry_permanent() {
        let cfg = Config::default();
        let cancel = Arc::new(AtomicBool::new(false));
        let mut attempts = 0u32;
        let result = retry_with_policy(&cfg, &cancel, || {
            attempts += 1;
            Err("401 unauthorized".into())
        }, |_| {});
        assert!(result.is_err());
        assert_eq!(attempts, 1);
    }

    #[test]
    fn retry_respects_cancel() {
        let cfg = Config::default();
        let cancel = Arc::new(AtomicBool::new(false));
        let result = retry_with_policy(&cfg, &cancel, || {
            cancel.store(true, Ordering::SeqCst);
            Err("http 502".into())
        }, |_| {});
        assert!(result.is_err());
        assert!(result.unwrap_err().contains("cancel"));
    }

    #[test]
    fn retry_disabled_when_max_retries_zero() {
        let mut cfg = Config::default();
        cfg.network.max_retries = 0;
        let cancel = Arc::new(AtomicBool::new(false));
        let mut attempts = 0u32;
        let _ = retry_with_policy(&cfg, &cancel, || {
            attempts += 1;
            Err("http 502".into())
        }, |_| {});
        assert_eq!(attempts, 1);
    }

    #[test]
    fn protocol_for_kind_maps() {
        assert_eq!(protocol_for_kind(ApiKind::Anthropic), Protocol::AnthropicMessages);
        assert_eq!(protocol_for_kind(ApiKind::OpenAi), Protocol::OpenAiChat);
    }

    #[test]
    fn catalog_returns_providers() {
        let c = catalog();
        assert!(c.provider_ids().len() >= 10);
    }

    /// Regression test: a catalog-known provider with no hand-written
    /// adapter (nvidia) must resolve to its real base URL and key env var,
    /// not an empty-base-URL guess.
    #[test]
    fn adapter_for_resolved_falls_back_to_bundled_meta_for_nvidia() {
        let r = Resolved {
            id: "nvidia".into(),
            name: "NVIDIA".into(),
            kind: ApiKind::OpenAi,
            base_url: "".into(),
            api_key: Some("test-key".into()),
            key_env: "".into(),
            needs_key: true,
            model: "".into(),
            headers: BTreeMap::new(),
            stream_usage: false,
            tools: true,
        };
        let a = adapter_for_resolved(&r);
        assert!(!a.default_base_url().is_empty(), "nvidia must not resolve to an empty base URL");
        assert!(a.default_base_url().contains("nvidia"), "got {:?}", a.default_base_url());
    }

    #[test]
    fn adapter_for_resolved_respects_explicit_config_override() {
        let r = Resolved {
            id: "nvidia".into(),
            name: "NVIDIA".into(),
            kind: ApiKind::OpenAi,
            base_url: "https://my-gateway.internal/v1".into(),
            api_key: Some("test-key".into()),
            key_env: "MY_NVIDIA_KEY".into(),
            needs_key: true,
            model: "".into(),
            headers: BTreeMap::new(),
            stream_usage: false,
            tools: true,
        };
        let a = adapter_for_resolved(&r);
        assert_eq!(a.default_base_url(), "https://my-gateway.internal/v1");
    }
}
