//! Built-in registry of known model providers.
//!
//! Every entry is real and verified: which wire protocol it speaks, its
//! default base URL, which environment variable conventionally holds its API
//! key, and a few suggested models (any model id the provider accepts can be
//! used — the lists only feed pickers and `romacode providers`). Anything not
//! listed here is a *custom provider*, defined in the config's `providers`
//! map.
//!
//! `fetch_models` performs a live catalog lookup against the provider's
//! `/models` endpoint. The HTTP request is executed via curl with the URL,
//! headers, and API key all passed through stdin (`curl --config -`) so the
//! key never appears in the process argument list (which other local users
//! can read via `ps`/`/proc/<pid>/cmdline`). The URL is run through the
//! `webfetch::url` SSRF filter before curl is invoked.
pub use romacode_shared::catalog::Known;
pub use romacode_shared::catalog::{find, known, known_at, known_len};
use crate::config::ApiKind;
use crate::webfetch::url::{ip_is_blocked, Url};
use serde_json::Value;
use std::io::{Read, Write};
use std::net::ToSocketAddrs;
use std::process::{Command, Stdio};




/// Escape a value for a curl config-file `key = "value"` line. Backslash and
/// double-quote are escaped; CR/LF are stripped to prevent header injection
/// through the config file.
fn cfg_quote(s: &str) -> String {
    s.replace('\\', "\\\\")
        .replace('"', "\\\"")
        .replace(['\n', '\r'], "")
}

/// Reject API keys containing characters that could inject extra curl config
/// directives or HTTP headers. Newlines and NULs are the dangerous ones; the
/// earlier `cfg_quote` strips CR/LF anyway, but rejecting up-front produces a
/// clearer error than a silently-corrupted request.
fn validate_api_key(key: &str) -> Result<(), String> {
    if key.contains('\n') || key.contains('\r') || key.contains('\0') {
        return Err("API key contains a newline or NUL — refusing to send".into());
    }
    if key.len() > 4096 {
        return Err("API key is implausibly long (>4096 chars)".into());
    }
    Ok(())
}

/// Resolve the URL host through `webfetch::url` and reject private / loopback
/// / link-local / cloud-metadata endpoints so a misconfigured `base_url` can
/// never leak the API key to an internal service.
fn ssrf_check(raw_url: &str) -> Result<(), String> {
    let parsed = Url::parse(raw_url)?;
    // Resolve the host to its IPs and block any private-range hits.
    // For `localhost` and `127.0.0.1` (Ollama/LM Studio) the resolver returns
    // a loopback address, which is allowed for local servers but blocked for
    // remote URLs. We allow loopback only when the URL host is exactly
    // `localhost` or a literal loopback IP.
    if !is_loopback_host(&parsed) {
        let target = format!("{}:{}", parsed.host, parsed.port);
        for addr in target.to_socket_addrs().into_iter().flatten() {
            if ip_is_blocked(addr.ip()) {
                return Err(format!(
                    "provider host {} resolves to a blocked private/loopback address ({}) — refusing to fetch",
                    parsed.host, addr
                ));
            }
        }
    }
    Ok(())
}

/// True when the URL host is literally `localhost`, `127.0.0.1` or `::1` —
/// the only hosts allowed to speak plain HTTP (Ollama, LM Studio, vLLM on
/// the same machine). Every other host must use HTTPS.
fn is_loopback_host(parsed: &Url) -> bool {
    matches!(parsed.host.as_str(), "localhost" | "127.0.0.1" | "::1")
}

/// Fetch the provider's current model catalog. The registry selects the wire
/// adapter; all OpenAI-compatible providers use `/models`, Anthropic uses
/// `/v1/models`. The request is performed by curl reading its config from
/// stdin so the URL, headers, and API key never appear in the process
/// argument list (which other local users could read via `ps`).
pub fn fetch_models(base_url: &str, kind: ApiKind, api_key: &str) -> Result<Vec<String>, String> {
    let base = base_url.trim_end_matches('/');
    if base.is_empty() {
        return Err("provider base URL is empty".into());
    }
    let url = match kind {
        ApiKind::Anthropic => format!("{base}/v1/models"),
        ApiKind::OpenAi => format!("{base}/models"),
    };
    ssrf_check(&url)?;
    let key_trimmed = api_key.trim();
    if !key_trimmed.is_empty() {
        validate_api_key(key_trimmed)?;
    }
    // Build the curl config file. Redirects stay off (`no-location`, the
    // explicit negated form — writing `location = "false"` would make curl
    // try to *read a file named false* and abort every request) so the
    // Authorization header can never follow a 302 to another host. HTTPS is
    // enforced for every remote host; loopback servers (Ollama, LM Studio,
    // vLLM on the same machine) may speak plain HTTP — that is the whole
    // point of the localhost exception.
    let parsed_for_proto = Url::parse(&url)?;
    let proto = if is_loopback_host(&parsed_for_proto) {
        "https,http"
    } else {
        "=https"
    };
    let mut cfg = format!(
        "url = \"{}\"\nrequest = \"GET\"\nuser-agent = \"romacode/0.6\"\nconnect-timeout = 15\nmax-time = 20\nproto = \"{}\"\nno-location\nheader = \"Accept: application/json\"\n",
        cfg_quote(&url),
        proto
    );
    if !key_trimmed.is_empty() {
        let header = match kind {
            // Anthropic pins an API version on every call — without this
            // header /v1/models answers 400 and the settings panel never
            // shows a model list for Anthropic.
            ApiKind::Anthropic => format!(
                "header = \"x-api-key: {}\"\nheader = \"anthropic-version: 2023-06-01\"\n",
                cfg_quote(key_trimmed)
            ),
            ApiKind::OpenAi => format!("header = \"Authorization: Bearer {}\"\n", cfg_quote(key_trimmed)),
        };
        cfg.push_str(&header);
    }
    let mut child = Command::new("curl")
        .args(["-q", "-sS", "--fail-with-body", "--config", "-"])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|e| format!("could not run curl: {e}"))?;
    {
        let mut stdin = child
            .stdin
            .take()
            .ok_or("curl stdin unavailable")?;
        stdin
            .write_all(cfg.as_bytes())
            .map_err(|e| format!("curl config: {e}"))?;
    }
    // Drain stdout and stderr CONCURRENTLY. The old code read stderr to EOF
    // first and stdout second — a catalog larger than the 64 KB pipe buffer
    // (OpenRouter ships hundreds of models) blocked curl on the stdout write
    // while this thread waited for stderr EOF: the classic single-threaded
    // pipe deadlock, released only by curl's own 20 s max-time, after which
    // the body came back truncated and the model list failed. stdout is
    // drained first on the current thread (the big stream); stderr goes to a
    // helper thread that can never block it.
    let stderr_handle = child
        .stderr
        .take()
        .map(|mut s| std::thread::spawn(move || {
            let mut err = String::new();
            let _ = s.read_to_string(&mut err);
            err
        }));
    let mut out = Vec::new();
    if let Some(mut stdout) = child.stdout.take() {
        let _ = stdout.read_to_end(&mut out);
    }
    let err = stderr_handle
        .map(|h| h.join().unwrap_or_default())
        .unwrap_or_default();
    let status = child
        .wait()
        .map_err(|e| format!("curl did not finish: {e}"))?;
    if !status.success() {
        // Sanitise stderr: strip any fragment that looks like a leaked
        // Authorization value, and cap length so a chatty proxy can't flood
        // the TUI.
        let msg = sanitise_curl_error(&err);
        return Err(if msg.is_empty() {
            "provider rejected the API key or URL".into()
        } else {
            format!("provider request failed: {msg}")
        });
    }
    let value: Value = serde_json::from_slice(&out)
        .map_err(|e| format!("invalid provider response: {e}"))?;
    let models = value
        .get("data")
        .and_then(Value::as_array)
        .or_else(|| value.get("models").and_then(Value::as_array))
        .map(|items| {
            items
                .iter()
                .filter_map(|m| {
                    m.get("id")
                        .or_else(|| m.get("name"))
                        .and_then(Value::as_str)
                        .map(str::to_string)
                })
                .collect::<Vec<_>>()
        })
        .unwrap_or_default();
    if models.is_empty() {
        return Err("provider returned an empty model list".into());
    }
    Ok(models)
}

/// Strip Authorization/x-api-key values from a curl error string and cap it.
fn sanitise_curl_error(raw: &str) -> String {
    let mut clean = String::new();
    for line in raw.lines().take(8) {
        let lower = line.to_ascii_lowercase();
        if lower.contains("authorization:") || lower.contains("x-api-key:") {
            // Keep only the header name, drop the value.
            if let Some(idx) = line.find(':') {
                clean.push_str(&line[..idx]);
                clean.push_str(": <redacted>");
            }
        } else {
            clean.push_str(line);
        }
        clean.push('\n');
    }
    let clean = clean.trim().to_string();
    if clean.len() > 400 {
        // Cut at a CHAR boundary: curl stderr can be any encoding; slicing a
        // multi-byte UTF-8 char in half panicked the whole fetch.
        let cut = clean
            .char_indices()
            .nth(400)
            .map(|(i, _)| i)
            .unwrap_or(clean.len());
        format!("{}…", &clean[..cut])
    } else {
        clean
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn registry_is_well_formed() {
        let mut seen = std::collections::HashSet::new();
        let mut ids = Vec::new();
        for k in known() {
            assert!(seen.insert(k.id.as_str()), "duplicate provider id {}", k.id);
            ids.push(k.id.clone());
            assert!(
                k.base_url.starts_with("http://") || k.base_url.starts_with("https://"),
                "{} base_url must be http(s)://",
                k.id
            );
            assert!(
                !k.base_url.ends_with('/'),
                "{} base url must not end with /",
                k.id
            );
            assert!(
                k.models.contains(&k.default_model),
                "{} default model {:?} not in models list {:?}",
                k.id,
                k.default_model,
                k.models
            );
            assert_eq!(k.needs_key, !k.key_env.is_empty(), "{}", k.id);
            assert!(
                !k.base_url.contains(".example"),
                "{} points at a fake .example domain: {}",
                k.id,
                k.base_url
            );
        }
        // Sanity: the bundled catalog contains the core real providers.
        for required in [
            "anthropic",
            "openai",
            "openrouter",
            "google",
            "groq",
            "deepseek",
            "mistral",
            "xai",
            "zai",
            "together",
            "fireworks",
            "cerebras",
            "ollama",
            "lmstudio",
        ] {
            assert!(
                seen.contains(required),
                "required real provider {:?} missing from catalog (have: {:?})",
                required,
                ids
            );
        }
        // At least 14 bundled providers (the live network catalog is richer).
        assert!(
            known_len() >= 14,
            "expected at least 14 bundled providers, got {}",
            known_len()
        );
        assert_eq!(find("OpenAI").map(|k| k.id.clone()), Some("openai".to_string()));
        assert_eq!(find("Anthropic").map(|k| k.id.clone()), Some("anthropic".to_string()));
        assert!(find("nope").is_none());
    }

    #[test]
    fn validate_api_key_rejects_crlf_and_nul() {
        assert!(validate_api_key("sk-clean-key-123").is_ok());
        assert!(validate_api_key("sk-leak\nX-Evil: 1").is_err());
        assert!(validate_api_key("sk-leak\r\nX-Evil: 1").is_err());
        assert!(validate_api_key("sk-nul\0injected").is_err());
        assert!(validate_api_key(&"x".repeat(4097)).is_err());
    }

    #[test]
    fn ssrf_check_blocks_private_addresses() {
        // Public hostname — should pass (DNS may or may not resolve in CI,
        // but `to_socket_addrs` returning empty is treated as no match).
        let _ = ssrf_check("https://api.openai.com/v1/models");
        // Loopback URL — allowed only when host is literally localhost/127.0.0.1.
        assert!(ssrf_check("http://localhost:11434/v1/models").is_ok());
        assert!(ssrf_check("http://127.0.0.1:11434/v1/models").is_ok());
        // Numeric-form private IP used as the host — blocked.
        assert!(ssrf_check("http://10.0.0.1/v1/models").is_err());
        assert!(ssrf_check("http://169.254.169.254/latest/meta-data/").is_err());
        assert!(ssrf_check("http://192.168.1.5/v1/models").is_err());
    }

    /// Regression: Ollama / LM Studio live on plain `http://localhost` and the
    /// old curl config pinned `proto = "=https"`, so their model list could
    /// never be fetched and the settings screen looked frozen. A loopback
    /// server must be reachable over HTTP; HTTPS-only stays for remote hosts.
    #[test]
    fn fetch_models_works_over_http_on_localhost() {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = listener.local_addr().unwrap();
        let server = std::thread::spawn(move || {
            use std::io::{Read as _, Write as _};
            let (mut sock, _) = listener.accept().unwrap();
            let mut buf = [0u8; 2048];
            let n = sock.read(&mut buf).unwrap_or(0);
            let req = String::from_utf8_lossy(&buf[..n]);
            // curl config decides proto before we ever see the request; the
            // request itself proves plain HTTP was allowed for loopback.
            assert!(req.starts_with("GET /v1/models"), "got: {req}");
            let body = r#"{"data":[{"id":"qwen3:8b"},{"id":"llama3.1:70b"}]}"#;
            let resp = format!(
                "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                body.len(),
                body
            );
            sock.write_all(resp.as_bytes()).unwrap();
        });
        let url = format!("http://{addr}/v1");
        let models = fetch_models(&url, ApiKind::OpenAi, "").expect("localhost http fetch");
        server.join().unwrap();
        assert_eq!(models, vec!["qwen3:8b", "llama3.1:70b"]);
    }

    #[test]
    fn sanitise_curl_error_redacts_authorization() {
        let raw = "curl: (6) Could not resolve host: api.foo.example\nAuthorization: Bearer sk-secret-key\n* Connection refused";
        let out = sanitise_curl_error(raw);
        assert!(!out.contains("sk-secret-key"), "key leaked: {out}");
        assert!(out.contains("<redacted>"), "expected <redacted>: {out}");
    }
}
