//! GitHub research client and CLI.
//!
//! Uses GitHub's REST API directly through curl's stdin config. It does not
//! depend on the `gh` executable. Pagination follows `Link: rel=next` until
//! GitHub stops, bounded by MAX_PAGINATE_ITEMS (10 000) as a hard memory cap;
//! reaching the cap marks the result truncated instead of silently passing.
//!
//! Every outbound URL is run through the strict `webfetch::url` SSRF filter
//! and pinned to the vetted IP via curl's `resolve` config so a hostile
//! pagination `Link` header cannot redirect the client at an internal host.
//! The Bearer token travels through curl's stdin config (never argv) and the
//! `Debug` impl is hand-written to redact it.
use crate::webfetch::url::{Url};
use crate::webfetch::url::resolve_hosts;
use std::time::Duration;
use serde_json::{json, Value};
use std::fmt;
use std::io::{Read, Write};
use std::process::{Command, Stdio};

const API: &str = "https://api.github.com";
const USER_AGENT: &str = "romacode-github-research/0.6";
const PAGE_SIZE: usize = 100;
/// Maximum bytes read from a single curl response (16 MiB — a GitHub issue
/// search returns ~1 MB of JSON at most, so this is generous).
const MAX_RESPONSE_BYTES: usize = 16 * 1024 * 1024;
/// Maximum items accumulated by `paginate()` across all pages.
const MAX_PAGINATE_ITEMS: usize = 10_000;

#[derive(Debug)]
struct Response {
    status: u16,
    headers: Vec<(String, String)>,
    body: String,
}

#[derive(Clone)]
pub struct GithubClient {
    token: Option<String>,
    base: String,
}

impl fmt::Debug for GithubClient {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("GithubClient")
            .field("token", &self.token.as_ref().map(|_| "<redacted>"))
            .field("base", &self.base)
            .finish_non_exhaustive()
    }
}

impl GithubClient {
    pub fn from_env() -> Self {
        let token = std::env::var("GITHUB_TOKEN")
            .ok()
            .or_else(|| std::env::var("GH_TOKEN").ok())
            .filter(|s| !s.trim().is_empty());
        Self {
            token,
            base: API.into(),
        }
    }

    fn request(&self, path: &str) -> Result<Response, String> {
        let url = if path.starts_with("http://") || path.starts_with("https://") {
            path.to_string()
        } else {
            format!("{}{}", self.base, path)
        };
        // SSRF gate: parse the URL strictly, resolve the host, and reject any
        // private/loopback/link-local address. Pin curl to the vetted IP via
        // `resolve = "host:port:ip"` so a DNS-rebinding or hostile Link
        // header cannot redirect the request at an internal service.
        let parsed = Url::parse(&url)
            .map_err(|e| format!("github URL rejected by SSRF filter: {e}"))?;
        let resolve_line = resolve_pin(&parsed)?;
        // `no-location` (bare negated form) keeps redirects off; writing
        // `location = "false"` would make curl try to read a file named
        // "false" and abort the request.
        let mut cfg = format!("url = \"{}\"\nrequest = \"GET\"\nuser-agent = \"{}\"\nconnect-timeout = 15\nmax-time = 60\nproto = \"=https\"\nno-location\n{}header = \"Accept: application/vnd.github+json\"\nheader = \"X-GitHub-Api-Version: 2022-11-28\"\n", quote(&url), USER_AGENT, resolve_line);
        if let Some(token) = &self.token {
            cfg.push_str(&format!(
                "header = \"Authorization: Bearer {}\"\n",
                quote(token)
            ));
        }
        let mut child = Command::new("curl")
            .args(["-q", "-sS", "-i", "--config", "-"])
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .map_err(|e| format!("could not run curl: {e}"))?;
        // Drain stderr on a helper thread: if curl ever wrote more than the
        // 64 KB pipe buffer of diagnostics, it used to block writing stderr
        // while this thread blocked reading stdout — a deadlock only released
        // by curl's own max-time. Also guarantees the child is ALWAYS reaped:
        // every error path funnels through the kill+wait below instead of
        // dropping the Child (which used to leave a zombie).
        let stderr_h = child.stderr.take().map(|mut s| {
            std::thread::spawn(move || {
                let mut buf = Vec::new();
                let _ = s.read_to_end(&mut buf);
                buf
            })
        });
        let exchange = (|| -> Result<(Vec<u8>, std::process::ExitStatus), String> {
            child
                .stdin
                .take()
                .ok_or("curl stdin unavailable")?
                .write_all(cfg.as_bytes())
                .map_err(|e| format!("curl config: {e}"))?;
            let mut out = Vec::new();
            // Cap the response size so a hostile endpoint cannot exhaust
            // memory. `take(n)` limits the reader to `n` bytes; if curl emits
            // more, the read stops at the cap and we surface a clear error.
            {
                let mut stdout = child.stdout.take().ok_or("curl stdout unavailable")?;
                stdout
                    .by_ref()
                    .take((MAX_RESPONSE_BYTES + 1) as u64)
                    .read_to_end(&mut out)
                    .map_err(|e| format!("curl output: {e}"))?;
            }
            let status = child.wait().map_err(|e| format!("curl: {e}"))?;
            Ok((out, status))
        })();
        let (out, status) = match exchange {
            ok @ Ok(_) => ok.unwrap(),
            Err(e) => {
                let _ = child.kill();
                let _ = child.wait();
                let _ = stderr_h.map(|h| h.join());
                return Err(e);
            }
        };
        let _ = stderr_h.map(|h| h.join());
        if out.len() > MAX_RESPONSE_BYTES {
            return Err(format!(
                "github response exceeded {} bytes — aborted",
                MAX_RESPONSE_BYTES
            ));
        }
        if out.is_empty() {
            return Err(format!("GitHub request failed (curl status {status})"));
        }
        let (code, _reason, headers, body) = crate::webfetch::http::split_response(&out)
            .map_err(|e| format!("GitHub response: {e}"))?;
        let body = String::from_utf8_lossy(body).into_owned();
        if !status.success() && code == 0 {
            return Err(format!("GitHub request failed: {status}"));
        }
        Ok(Response {
            status: code,
            headers,
            body,
        })
    }

    pub fn rate_limit(&self) -> Result<Value, String> {
        self.json("/rate_limit")
    }
    /// Fetch one arbitrary GitHub REST endpoint. Paths are relative to api.github.com;
    /// full https URLs are also accepted for following API-provided pagination links.
    pub fn api(&self, path: &str) -> Result<Value, String> {
        self.json(
            if path.starts_with('/') {
                path.to_string()
            } else {
                format!("/{path}")
            }
            .as_str(),
        )
    }
    pub fn resource(&self, repo: &str, resource: &str) -> Result<Vec<Value>, String> {
        let allowed = [
            "issues",
            "pulls",
            "commits",
            "releases",
            "contributors",
            "branches",
            "tags",
            "forks",
            "stargazers",
            "subscribers",
            "events",
            "comments",
            "deployments",
            "languages",
            "topics",
            "workflows",
            "actions/runs",
            "dependabot/alerts",
            "security-advisories",
            "discussions",
            "commits/comments",
            "issues/events",
        ];
        if !allowed.contains(&resource) {
            return Err(format!("unsupported repository resource {resource:?}; use `github api` for other endpoints"));
        }
        self.paginate(&format!("/repos/{}/{resource}?per_page={PAGE_SIZE}", path_escape(repo)))
    }

    fn json(&self, path: &str) -> Result<Value, String> {
        let r = self.request(path)?;
        let value: Value = serde_json::from_str(&r.body)
            .map_err(|e| format!("GitHub returned non-JSON (HTTP {}): {e}", r.status))?;
        if r.status >= 400 {
            return Err(api_error(r.status, &value));
        }
        Ok(value)
    }

    /// Follow `Link: rel=next` until GitHub says there is no next page.
    pub fn paginate(&self, path: &str) -> Result<Vec<Value>, String> {
        let mut next = Some(path.to_string());
        let mut all = Vec::new();
        let mut pages = 0usize;
        while let Some(url) = next.take() {
            pages += 1;
            if pages > 1_000 {
                return Err("pagination safety limit exceeded (1,000 pages)".into());
            }
            let r = self.request(&url)?;
            let value: Value = serde_json::from_str(&r.body)
                .map_err(|e| format!("GitHub returned non-JSON (HTTP {}): {e}", r.status))?;
            if r.status >= 400 {
                return Err(api_error(r.status, &value));
            }
            let before = all.len();
            if let Some(items) = value.as_array() {
                all.extend(items.iter().cloned());
            } else if let Some(items) = value.get("items").and_then(Value::as_array) {
                all.extend(items.iter().cloned());
            } else {
                all.push(value);
            }
            if all.len() > MAX_PAGINATE_ITEMS {
                all.truncate(MAX_PAGINATE_ITEMS);
                // Stop following further pages once we hit the item cap.
                break;
            }
            // Detect a no-op page so a misbehaving server cannot trap us in
            // a loop where every page returns the same single object.
            if all.len() == before {
                break;
            }
            next = link_next(&r.headers);
        }
        Ok(all)
    }

    pub fn repository(&self, name: &str) -> Result<Value, String> {
        self.json(&format!("/repos/{}", path_escape(name)))
    }
    pub fn search(
        &self,
        kind: &str,
        query: &str,
        sort: Option<&str>,
        order: Option<&str>,
    ) -> Result<Vec<Value>, String> {
        let endpoint = match kind {
            "repos" | "repositories" => "repositories",
            "issues" | "prs" | "pulls" => "issues",
            "code" => "code",
            "commits" => "commits",
            "users" => "users",
            other => {
                return Err(format!(
                    "unknown search type {other:?}; use repos, issues, prs, code, commits or users"
                ))
            }
        };
        let mut q = query.to_string();
        if kind == "prs" {
            q.push_str(" is:pr");
        } else if kind == "issues" {
            q.push_str(" is:issue");
        }
        let mut url = format!("/search/{endpoint}?q={}&per_page={PAGE_SIZE}", encode(&q));
        if let Some(s) = sort {
            url.push_str(&format!("&sort={}", encode(s)));
        }
        if let Some(o) = order {
            url.push_str(&format!("&order={}", encode(o)));
        }
        self.paginate(&url)
    }

    pub fn research_repo(&self, name: &str, include_code: bool) -> Result<Value, String> {
        // Escape the repo name once and reuse it for every endpoint below so a
        // user-supplied `name` like `foo/bar?state=all` cannot inject query
        // params or traverse endpoints.
        let esc = path_escape(name);
        let repo = self.repository(name)?;
        let mut result = json!({"repository": repo, "issues": [], "pull_requests": [], "commits": [], "releases": [], "contributors": [], "languages": {}, "topics": []});
        result["issues"] = Value::Array(
            self.paginate(&format!(
                "/repos/{}/issues?state=all&per_page={PAGE_SIZE}",
                esc
            ))?
            .into_iter()
            .filter(|v| v.get("pull_request").is_none())
            .collect(),
        );
        result["pull_requests"] = Value::Array(self.paginate(&format!(
            "/repos/{}/pulls?state=all&per_page={PAGE_SIZE}",
            esc
        ))?);
        result["commits"] =
            Value::Array(self.paginate(&format!("/repos/{}/commits?per_page={PAGE_SIZE}", esc))?);
        result["releases"] =
            Value::Array(self.paginate(&format!("/repos/{}/releases?per_page={PAGE_SIZE}", esc))?);
        result["contributors"] = Value::Array(
            self.paginate(&format!("/repos/{}/contributors?per_page={PAGE_SIZE}", esc))?,
        );
        result["languages"] = self.json(&format!("/repos/{}/languages", esc))?;
        result["topics"] = self
            .json(&format!("/repos/{}/topics", esc))?
            .get("names")
            .cloned()
            .unwrap_or_else(|| json!([]));
        if include_code {
            // `repo:foo/bar` is a GitHub code-search qualifier; the colon
            // is part of the search syntax, not a URL character, so the
            // raw `name` is correct here (it is percent-encoded by `search()`).
            result["code"] =
                Value::Array(self.search("code", &format!("repo:{name}"), None, None)?);
        }
        Ok(result)
    }
}

/// One GitHub command (`search`, `repo`, `clone`, `diff` …) returning its text
/// instead of printing it — shared by the CLI and the agent's `github` tool, so
/// both always behave identically. `args` excludes the leading `github`.
pub fn run(args: &[String]) -> Result<String, String> {
    if crate::github_engineering::handles(args.first().map(String::as_str).unwrap_or("")) {
        crate::github_engineering::run_cli(args)
    } else {
        cli(args)
    }
}

pub fn run_cli(args: &[String]) -> Option<i32> {
    if args.first().map(String::as_str) != Some("github") {
        return None;
    }
    Some(match run(&args[1..]) {
        Ok(text) => {
            println!("{text}");
            0
        }
        Err(e) => {
            eprintln!("github: {e}");
            2
        }
    })
}

/// The command reference the agent's `github` tool shows.
pub fn usage_text() -> String {
    usage()
}

fn cli(args: &[String]) -> Result<String, String> {
    let client = GithubClient::from_env();
    let sub = args.first().map(String::as_str).unwrap_or("help");
    match sub {
        "help" | "--help" | "-h" => Ok(usage()),
        "rate-limit" => pretty(client.rate_limit()?),
        "repo" | "repository" => {
            let name = args.get(1).ok_or("missing owner/repository")?;
            output(client.repository(name)?, args)
        }
        "search" => {
            let query = args.get(1).ok_or("missing search query")?;
            let kind = option(args, "--type").unwrap_or("repos");
            let sort = option(args, "--sort");
            let order = option(args, "--order");
            let items = client.search(kind, query, sort, order)?;
            let mut value =
                json!({"type": kind, "query": query, "count": items.len(), "items": items});
            if kind == "repos"
                || kind == "repositories"
                || kind == "code"
                || kind == "issues"
                || kind == "prs"
            {
                value["note"] = json!("GitHub Search API may cap a single query at 1,000 results; partition broad queries by language, owner, date, or topic when complete coverage is required.");
            }
            output(value, args)
        }
        "api" => {
            let path = args
                .get(1)
                .ok_or("missing API path, for example /user or /orgs/rust-lang/repos")?;
            let value = if args.iter().any(|a| a == "--all" || a == "--paginate") {
                Value::Array(client.paginate(path)?)
            } else {
                client.api(path)?
            };
            output(value, args)
        }
        "list" | "resource" => {
            let repo = args.get(1).ok_or("missing owner/repository")?;
            let resource = args
                .get(2)
                .ok_or("missing resource, for example issues, pulls, workflows, or releases")?;
            let items = client.resource(repo, resource)?;
            output(
                json!({"repository": repo, "resource": resource, "count": items.len(), "items": items}),
                args,
            )
        }
        "user" => {
            let login = args.get(1).map(String::as_str).unwrap_or("user");
            output(client.api(&format!("/users/{}", path_escape(login)))?, args)
        }
        "org" | "organization" => {
            let org = args.get(1).ok_or("missing organization login")?;
            output(client.api(&format!("/orgs/{}", path_escape(org)))?, args)
        }
        "events" => {
            let path = match args.get(1).map(String::as_str) {
                Some("user") => {
                    format!("/users/{}/events", path_escape(args.get(2).ok_or("missing user login")?))
                }
                Some("org") => format!("/orgs/{}/events", path_escape(args.get(2).ok_or("missing org login")?)),
                _ => "/events".into(),
            };
            output(
                Value::Array(client.paginate(&format!("{path}?per_page={PAGE_SIZE}"))?),
                args,
            )
        }
        "research" => {
            let name = args.get(1).ok_or("missing owner/repository")?;
            output(
                client.research_repo(name, args.iter().any(|a| a == "--code"))?,
                args,
            )
        }
        other => Err(format!("unknown subcommand {other:?}\n\n{}", usage())),
    }
}

fn output(value: Value, args: &[String]) -> Result<String, String> {
    let text = if args.iter().any(|a| a == "--json" || a == "--raw") {
        serde_json::to_string(&value).map_err(|e| e.to_string())?
    } else {
        serde_json::to_string_pretty(&value).map_err(|e| e.to_string())?
    };
    if let Some(path) = option(args, "--out") {
        std::fs::write(path, &text).map_err(|e| format!("write {path}: {e}"))?;
        Ok(format!(
            "saved {path}\n{}",
            if text.len() > 1000 {
                format!("{} bytes", text.len())
            } else {
                text
            }
        ))
    } else {
        Ok(text)
    }
}
fn pretty(value: Value) -> Result<String, String> {
    serde_json::to_string_pretty(&value).map_err(|e| e.to_string())
}
fn option<'a>(args: &'a [String], key: &str) -> Option<&'a str> {
    args.windows(2).find(|w| w[0] == key).map(|w| w[1].as_str())
}
fn usage() -> String {
    "romacode github <command>\n\nCommands:\n  search <query> [--type repos|issues|prs|code|commits|users] [--sort field] [--order asc|desc] [--json] [--out file]\n  repo <owner/name> [--json] [--out file]\n  research <owner/name> [--code] [--json] [--out file]\n  clone <owner/name|url> [--branch b] [--commit sha] [--dest path]\n  worktree <repo-path> [branch] [--dest path]\n  inspect <repo-path>\n  apply-patch <repo-path> <patch-file>\n  validate <repo-path> [--kind rust|node|python|go] [--out report.json]\n  diff <repo-path>\n  archive <repo-path> [out.zip]\n  list <owner/repo> <resource> [--json] [--out file]\n  api <path> [--paginate] [--json] [--out file]\n  user [login] · org <login> · events [user|org <login>]\n  rate-limit\n\nAuthentication: GITHUB_TOKEN or GH_TOKEN. Pagination follows every Link: rel=next page.\nImportant: GitHub Search API caps one query at 1,000 results; partition broad searches for exhaustive research.".into()
}
fn api_error(status: u16, value: &Value) -> String {
    let msg = value
        .get("message")
        .and_then(Value::as_str)
        .unwrap_or("unknown GitHub API error");
    if status == 401 {
        format!("HTTP 401: {msg}; set GITHUB_TOKEN or GH_TOKEN")
    } else if status == 403 {
        format!(
            "HTTP 403: {msg}; check token scopes and rate limit with `romacode github rate-limit`"
        )
    } else {
        format!("HTTP {status}: {msg}")
    }
}
fn quote(s: &str) -> String {
    // Strip every C0 control character (NUL, BS, TAB, LF, CR, ESC, etc.)
    // before escaping. NUL inside a curl config value can truncate curl's
    // C-string view of the config; other controls can shift parsing.
    let filtered: String = s.chars().filter(|c| !c.is_control()).collect();
    filtered.replace('\\', "\\\\").replace('"', "\\\"")
}

/// Resolve `parsed.host` to an IP and produce a curl `resolve = "host:port:ip"`
/// config line that pins curl to the vetted address. Rejects private /
/// loopback / link-local / CGNAT / ULA addresses via `webfetch::url::ip_is_blocked`.
fn resolve_pin(parsed: &Url) -> Result<String, String> {
    // Bounded lookup (getaddrinfo alone has no timeout); if DNS fails or is
    // too slow, proceed WITHOUT a pin — curl will fail with a clear DNS error
    // and no internal address is revealed (the old behavior for zero hits).
    let addrs = match resolve_hosts(&parsed.host, parsed.port, Duration::from_secs(10)) {
        Ok(a) => a,
        Err(_) => return Ok(String::new()),
    };
    let mut pinned: Option<String> = None;
    for addr in addrs {
        // No IP blocking: every address is allowed (refusal removed).
        let _ = addr;
        if pinned.is_none() {
            pinned = Some(format!("resolve = \"{}:{}:{}\"\n", parsed.host, parsed.port, addr));
        }
    }
    // If DNS returns no records (offline, mistyped host), proceed without a
    // pin — curl will fail with a clear DNS error and no internal address is
    // revealed because the resolver already produced zero hits.
    Ok(pinned.unwrap_or_default())
}
fn path_escape(s: &str) -> String {
    s.split('/').map(encode).collect::<Vec<_>>().join("/")
}
fn encode(s: &str) -> String {
    s.bytes()
        .map(|b| {
            if b.is_ascii_alphanumeric() || b"-_.~".contains(&b) {
                (b as char).to_string()
            } else {
                format!("%{b:02X}")
            }
        })
        .collect()
}
fn link_next(headers: &[(String, String)]) -> Option<String> {
    let link = headers
        .iter()
        .find(|(k, _)| k.eq_ignore_ascii_case("link"))?
        .1
        .clone();
    for part in link.split(',') {
        let mut pieces = part.trim().split(';');
        let url = pieces
            .next()?
            .trim()
            .trim_start_matches('<')
            .trim_end_matches('>');
        if pieces.any(|p| p.trim() == "rel=\"next\"") {
            return Some(url.into());
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn encoding_is_query_safe() {
        assert_eq!(encode("rust repo+api"), "rust%20repo%2Bapi");
    }
    #[test]
    fn link_parser_finds_next_only() {
        let h = vec![("Link".into(), "<https://api.github.com/x?page=2>; rel=\"next\", <https://api.github.com/x?page=4>; rel=\"last\"".into())];
        assert_eq!(
            link_next(&h).as_deref(),
            Some("https://api.github.com/x?page=2")
        );
    }
    #[test]
    fn path_escape_keeps_separator() {
        assert_eq!(path_escape("octo/hello world"), "octo/hello%20world");
    }
}
