//! Human-readable provider/model reporting and the `romacode providers|models|set|add-provider`
//! command line. The same text is used by the in-app slash commands
//! (`/providers`, `/provider`, `/models`, …).
use crate::config::{ApiKind, Config};
use crate::providers;

fn kind_name(k: ApiKind) -> &'static str {
    match k {
        ApiKind::Anthropic => "anthropic",
        ApiKind::OpenAi => "openai-compatible",
    }
}

/// Readiness of provider `id` as if it were selected (top-level overrides of
/// the active provider are not applied to the others).
fn readiness(cfg: &Config, id: &str) -> &'static str {
    let mut c = cfg.clone();
    if c.provider.to_ascii_lowercase() != id {
        c.provider = id.to_string();
        c.model.clear();
        c.api_base.clear();
        c.api_key_env.clear();
    }
    match c.resolve() {
        Ok(r) if r.needs_key => "key set",
        Ok(_) => "no key needed",
        Err(e) if e.contains("API key") => "needs key",
        Err(_) => "incomplete",
    }
}

/// All known and custom providers, the active one marked with `*`.
pub fn list(cfg: &Config) -> String {
    let active = cfg.provider.to_ascii_lowercase();
    let mut out = String::from("Providers  (* = active)\n");
    let mut row = |id: &str, name: &str, kind: ApiKind, base: &str, tag: &str| {
        let mark = if id == active { '*' } else { ' ' };
        out.push_str(&format!(
            "{mark} {id:<11} {name:<18} {:<18} {base}   [{tag}]\n",
            kind_name(kind)
        ));
    };
    for k in providers::known() {
        let base = cfg
            .providers
            .get(k.id.as_str())
            .map(|p| p.base_url.as_str())
            .filter(|b| !b.is_empty())
            .unwrap_or(k.base_url.as_str());
        row(
            k.id.as_str(),
            k.name.as_str(),
            cfg.providers
                .get(k.id.as_str())
                .and_then(|p| p.kind)
                .unwrap_or(k.kind),
            base,
            readiness(cfg, k.id.as_str()),
        );
    }
    for (id, p) in &cfg.providers {
        if providers::find(id).is_none() {
            let name = if p.name.is_empty() {
                "(custom)"
            } else {
                p.name.as_str()
            };
            row(
                id,
                name,
                p.kind.unwrap_or(ApiKind::OpenAi),
                &p.base_url,
                readiness(cfg, id),
            );
        }
    }
    out.push_str(
        "\nSwitch: /provider <id>   Model: /model <name>   Key: /apikey <key>   URL: /baseurl <url>\n\
         Custom: /customprovider <id> <base-url> [openai|anthropic]",
    );
    out
}

/// The active provider, model, endpoint and key state, plus the selected
/// model's official running rules when a profile exists.
pub fn status(cfg: &Config) -> String {
    match cfg.resolve() {
        Ok(r) => {
            let profile = romacode_shared::modeldata::find(&r.id, &r.model)
                .map(|p| format!("\nModel rules: {} ({})", romacode_shared::modeldata::summary(p), p.id))
                .unwrap_or_default();
            format!(
                "Provider: {} ({})\nModel:    {}\nBase URL: {}\nAPI key:  {}{}",
                r.name,
                kind_name(r.kind),
                r.model,
                r.base_url,
                if !r.needs_key { "not needed" } else { "set" },
                profile
            )
        }
        Err(e) => format!(
            "Provider: {}\nModel:    {}\nNot ready: {e}",
            cfg.provider,
            match cfg.effective_model() {
                m if m.is_empty() => "(none)".to_string(),
                m => m,
            }
        ),
    }
}

/// Suggested models for the active provider, annotated with each model's
/// official limits from the embedded modeldata library (context, output
/// budget, headline features) when a profile exists.
pub fn models(cfg: &Config) -> String {
    let cur = cfg.effective_model();
    let mut out = format!(
        "Models for {}  (* = current; any model id the provider accepts works)\n",
        cfg.provider
    );
    for m in cfg.model_choices() {
        let note = romacode_shared::modeldata::find(&cfg.provider, &m)
            .map(|p| romacode_shared::modeldata::summary(p))
            .unwrap_or_default();
        if note.is_empty() {
            out.push_str(&format!("{} {m}\n", if m == cur { '*' } else { ' ' }));
        } else {
            out.push_str(&format!("{} {m} — {note}\n", if m == cur { '*' } else { ' ' }));
        }
    }
    out.push_str("\nSet with: /model <name>   Per-model rules: romacode modelinfo <provider> <model>");
    out
}

/// The full official profile for (provider, model) — the "own file setup"
/// every selected model follows: limits, param ranges, wire rules, endpoints
/// and the official source documents.
pub fn modelinfo(provider: &str, model: &str) -> String {
    let p = romacode_shared::modeldata::profile_for(provider, model);
    let mut out = format!(
        "Model profile: {} ({})\nProvider: {}   Family: {}   Status: {}\nContext window: {}   Max output: {}\nFeatures: {}\n",
        p.display_name,
        p.id,
        p.provider,
        p.family,
        p.status,
        if p.context_window > 0 { p.context_window.to_string() } else { "unknown".into() },
        if p.max_output_tokens > 0 { p.max_output_tokens.to_string() } else { "unknown".into() },
        romacode_shared::modeldata::summary(p),
    );
    if let Some(t) = &p.params.temperature {
        out.push_str(&format!(
            "Temperature: {}\n",
            if t.supported {
                format!("supported, range {}..={}", t.min, t.max)
            } else {
                "NOT supported — omit from requests".into()
            }
        ));
    }
    if let Some(tp) = &p.params.top_p {
        out.push_str(&format!(
            "top_p: {}\n",
            if tp.supported {
                format!("supported, range {}..={}", tp.min, tp.max)
            } else {
                "NOT supported — omit from requests".into()
            }
        ));
    }
    if p.params.reasoning_effort.as_ref().map(|e| e.supported).unwrap_or(false) {
        out.push_str("Reasoning effort: supported\n");
    }
    out.push_str(&format!(
        "Limit parameter: {}   stop: {}{}\n",
        if p.wire.max_tokens_param.is_empty() { "auto (provider id rule)" } else { &p.wire.max_tokens_param },
        if p.wire.stop_supported { "accepted" } else { "rejected" },
        if p.wire.temperature_or_top_p_exclusive { "   temperature OR top_p only" } else { "" },
    ));
    for note in &p.wire_notes {
        out.push_str(&format!("Note: {note}\n"));
    }
    if let Some(base) = p.endpoints.get("base_url").and_then(|v| v.as_str()) {
        out.push_str(&format!("Base URL: {base}\n"));
    }
    if let Some(docs) = &p.docs {
        out.push_str(&format!("Docs: {docs}\n"));
    }
    if !p.unverified_fields.is_empty() {
        out.push_str(&format!(
            "Unverified fields (not from official docs): {}\n",
            p.unverified_fields.join(", ")
        ));
    }
    out
}

/// Handle `romacode providers | models | provider <id> | model <name> |
/// modelinfo [provider] <model> | set <key> <value> |
/// add-provider <id> <url> [kind]`. Returns `None` when `args` is not one of
/// these, otherwise the process exit code.
pub fn run_cli(args: &[String]) -> Option<i32> {
    let first = args.first()?.as_str();
    if !matches!(
        first,
        "providers" | "models" | "provider" | "model" | "modelinfo" | "set" | "add-provider"
            | "catalog" | "refresh"
    ) {
        return None;
    }
    let mut cfg = Config::load();
    let say = |s: String| println!("{s}");
    let fail = |s: String| -> i32 {
        eprintln!("error: {s}");
        2
    };
    match (first, &args[1..]) {
        ("providers", []) => say(list(&cfg)),
        ("models", []) => say(models(&cfg)),
        ("catalog", []) => say(catalog_text()),
        ("refresh", []) => {
            let c = crate::provider_engine::refresh_catalog();
            say(format!(
                "catalog refreshed ({} providers, source={:?})",
                c.providers.len(),
                c.source
            ))
        }
        ("provider", []) => say(status(&cfg)),
        ("provider", [id]) => match cfg.set_provider(id) {
            Ok(()) => {
                cfg.save();
                say(status(&cfg));
            }
            Err(e) => return Some(fail(e)),
        },
        ("model", []) => say(format!("{}", cfg.effective_model())),
        ("modelinfo", [model]) => {
            // resolve against the ACTIVE provider
            say(modelinfo(&cfg.provider, model));
        }
        ("modelinfo", [provider, model]) => say(modelinfo(provider, model)),
        ("model", [name]) => {
            cfg.model = name.clone();
            cfg.save();
            say(format!("model = {name}"));
        }
        ("set", [key, value]) => {
            let value = if value == "-" {
                // read the secret from stdin so it stays out of shell history
                let mut s = String::new();
                let _ = std::io::stdin().read_line(&mut s);
                s.trim().to_string()
            } else {
                value.clone()
            };
            match key.as_str() {
                "provider" => {
                    if let Err(e) = cfg.set_provider(&value) {
                        return Some(fail(e));
                    }
                }
                "model" => cfg.model = value.clone(),
                "base-url" | "base_url" => {
                    if let Err(e) = cfg.set_base_url(&value) {
                        return Some(fail(e));
                    }
                }
                "api-key" | "api_key" => cfg.set_api_key(&value),
                "api-key-env" | "api_key_env" => cfg.providers.entry(cfg.provider.to_ascii_lowercase()).or_default().api_key_env = value.clone(),
                other => return Some(fail(format!("unknown setting {other:?}; use provider, model, base-url, api-key or api-key-env"))),
            }
            cfg.save();
            // never echo secrets
            say(format!("{key} saved\n{}", status(&cfg)));
        }
        ("add-provider", [id, url]) | ("add-provider", [id, url, _]) => {
            let kind = match args.get(3).map(String::as_str) {
                None | Some("openai") => ApiKind::OpenAi,
                Some("anthropic") => ApiKind::Anthropic,
                Some(o) => return Some(fail(format!("kind must be openai or anthropic, not {o:?}"))),
            };
            if let Err(e) = cfg.add_custom_provider(id, url, kind) {
                return Some(fail(e));
            }
            cfg.save();
            say(format!("custom provider {id} added — select it with: romacode provider {id}"));
        }
        _ => {
            return Some(fail(
                "usage: romacode providers | models | catalog | refresh | provider [id] | model [name] | set <provider|model|base-url|api-key|api-key-env> <value|-> | add-provider <id> <base-url> [openai|anthropic]".into(),
            ))
        }
    }
    Some(0)
}

/// Print the dynamic provider+model catalog (OpenCode-structured). Source is
/// network-fetched with a 5-minute TTL, falling back to a bundled snapshot.
/// `romacode catalog` shows every provider and its models; `romacode refresh`
/// forces a network refresh.
pub fn catalog_text() -> String {
    let c = crate::provider_engine::catalog();
    let mut out = format!(
        "RomaCode provider catalog  (source: {:?}, {} providers)\n\n",
        c.source,
        c.providers.len()
    );
    for (pid, p) in &c.providers {
        out.push_str(&format!("  {}  ({})  [{} models]\n", pid, p.name, p.models.len()));
        for m in p.models.values() {
            out.push_str(&format!(
                "      {}  ctx={}  out={}  tools={}  status={:?}\n",
                m.model_id,
                m.limit.context,
                m.limit.output,
                m.capabilities.tools,
                m.status,
            ));
        }
    }
    out
}
