//! Rustango-native integration — the vendored `rustango` crate (Django-style
//! batteries-included web framework, rewritten in pure Rust) wired into the
//! brain as a first-class engine, exactly like langchain.
//!
//! The framework itself ships inside the brain (`crates/server/rustango`,
//! including its `rustango-macros` proc-macro crate, ORM, template engine,
//! forms, admin, auth, sessions, csrf, i18n, cache, migrations and the
//! `rustango` CLI). This bridge exposes it to the agent loop as tools:
//!
//! * `webapp_scaffold` — writes a complete, runnable rustango project
//!   (Cargo.toml, `src/lib.rs` with a `#[derive(rustango::Model)]` model +
//!   views + urls + project builder, `src/main.rs`, a test-client smoke test,
//!   README) into the user's project. Writes are confined to the project
//!   root; an existing non-empty target directory is never overwritten.
//! * `webapp_check` — structural check of a rustango project (manifest,
//!   entrypoint, routes, models, tests, settings), so the agent can
//!   self-review after editing a generated app.
//!
//! Every call still passes the same security gate as all other tools: the
//! agent loop runs `security.authorize` before dispatch reaches this module.
//!
//! The standalone `rustango` binary (runserver / migrate / startproject /
//! startapp / createsuperuser …) is built by the same workspace and can be
//! installed alongside `romacode-server` and `romacode` (see scripts/install.sh).

use crate::config::Config;
use serde_json::Value;
use std::path::{Path, PathBuf};

/// Version of the vendored framework (surfaced in scaffold output).
pub fn framework_version() -> &'static str {
    rustango::VERSION
}

/// Dispatch a webapp tool call. Same shape as `langchain_bridge::rag_execute`.
pub fn webapp_execute(_cfg: &Config, name: &str, input: &Value) -> Result<String, String> {
    match name {
        "webapp_scaffold" => webapp_scaffold(input),
        "webapp_check" => webapp_check(input),
        other => Err(format!("unknown webapp tool {other}")),
    }
}

// ---------------------------------------------------------------------------
// Path / name helpers (confinement to the project root, no traversal).
// ---------------------------------------------------------------------------

/// Names that would shadow Rust built-ins when used as a lib crate name.
const RESERVED_CRATE_NAMES: &[&str] = &[
    "crate", "self", "super", "std", "core", "alloc", "test", "tests", "main",
];

fn sanitize_project_name(name: &str) -> Result<String, String> {
    let n = name.trim();
    let valid = !n.is_empty()
        && n.len() <= 64
        && n.chars().all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-')
        && !n.starts_with('-');
    if !valid {
        return Err(format!(
            "invalid project name '{n}' — use ASCII letters, digits, '_' or '-' (max 64 chars)"
        ));
    }
    let crate_name = n.replace('-', "_");
    if RESERVED_CRATE_NAMES.contains(&crate_name.as_str()) {
        return Err(format!("'{n}' is a reserved Rust name — pick another"));
    }
    Ok(n.to_string())
}

/// Resolve a project-relative path, refusing absolute paths and `..` escapes.
fn resolve_under_root(root: &Path, rel: &str) -> Result<PathBuf, String> {
    let rel = rel.trim().trim_matches('/');
    if rel.starts_with('/') || rel.split('/').any(|c| c == "..") {
        return Err(format!("'{rel}' escapes the project directory"));
    }
    Ok(if rel.is_empty() {
        root.to_path_buf()
    } else {
        root.join(rel)
    })
}

// ---------------------------------------------------------------------------
// webapp_scaffold
// ---------------------------------------------------------------------------

fn webapp_scaffold(input: &Value) -> Result<String, String> {
    let root = crate::security::root()?;
    webapp_scaffold_at(&root, input)
}

fn webapp_scaffold_at(root: &Path, input: &Value) -> Result<String, String> {
    let name = sanitize_project_name(input["name"].as_str().ok_or("missing string name")?)?;
    let crate_name = name.replace('-', "_");
    let base = resolve_under_root(root, input["path"].as_str().unwrap_or("."))?;
    let dest = base.join(&name);
    if dest.exists()
        && std::fs::read_dir(&dest)
            .map_err(|e| format!("cannot read {}: {e}", dest.display()))?
            .flatten()
            .count()
            > 0
    {
        return Err(format!(
            "{} already exists and is not empty — pick another name or remove it first",
            dest.display()
        ));
    }

    // Dependency line: point at the vendored copy when the caller knows where
    // it lives, otherwise fall back to a version requirement (works once the
    // framework is published or the checkout path is provided).
    let dep_line = match input["rustango_path"].as_str() {
        Some(p) if !p.trim().is_empty() => {
            // This string is only written into the generated Cargo.toml —
            // romacode never resolves it, so relative paths (even ones that
            // climb out of the project, like "../vendor/rustango") are fine.
            // Only quotes and control characters could corrupt the manifest.
            let p = p.trim().replace('\\', "/");
            if p.contains('"') || p.chars().any(|c| c.is_control()) {
                return Err("rustango_path must be a plain path without quotes".into());
            }
            format!("rustango = {{ path = \"{p}\" }}")
        }
        _ => "rustango = \"6.2\"".to_string(),
    };
    let database = match input["database"].as_str() {
        Some("memory") => "memory".to_string(),
        Some(d) if d.starts_with("json:") => {
            let f = d.trim_start_matches("json:");
            if f.contains("..") || f.contains('/') {
                return Err("database json path must be a plain file name".into());
            }
            d.to_string()
        }
        _ => "json:db.json".to_string(),
    };

    let cargo = SCAFFOLD_CARGO
        .replace("%%NAME%%", &name)
        .replace("%%CRATE%%", &crate_name)
        .replace("%%DEP%%", &dep_line);
    let lib = SCAFFOLD_LIB
        .replace("%%NAME%%", &name)
        .replace("%%TITLE%%", &name)
        .replace("%%DB%%", &database);
    let main = SCAFFOLD_MAIN
        .replace("%%NAME%%", &name)
        .replace("%%CRATE%%", &crate_name);
    let test = SCAFFOLD_TEST
        .replace("%%NAME%%", &name)
        .replace("%%CRATE%%", &crate_name);
    let readme = SCAFFOLD_README
        .replace("%%NAME%%", &name)
        .replace("%%DEP%%", &dep_line)
        .replace("%%DB%%", &database);

    write_file(&dest.join("Cargo.toml"), &cargo)?;
    write_file(&dest.join("src/lib.rs"), &lib)?;
    write_file(&dest.join("src/main.rs"), &main)?;
    write_file(&dest.join("tests/smoke.rs"), &test)?;
    write_file(&dest.join("README.md"), &readme)?;
    write_file(&dest.join(".gitignore"), "target/\ndb.json\nmigrations/\n")?;

    Ok(format!(
        "Scaffolded rustango web app (rustango {}) at {} (6 files).\n\
         Run it:  cd {} && cargo run          # serves http://127.0.0.1:8000/\n\
         Test it: cd {} && cargo test\n\
         Grow it: add models in src/lib.rs (#[derive(rustango::Model)]), register routes in the urls vec, or use `rustango startapp` from the CLI.",
        framework_version(),
        dest.display(),
        dest.display(),
        dest.display(),
    ))
}

fn write_file(path: &Path, contents: &str) -> Result<(), String> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)
            .map_err(|e| format!("create {}: {e}", parent.display()))?;
    }
    std::fs::write(path, contents).map_err(|e| format!("write {}: {e}", path.display()))
}

const SCAFFOLD_CARGO: &str = r#"[package]
name = "%%NAME%%"
version = "0.1.0"
edition = "2021"

[lib]
name = "%%CRATE%%"
path = "src/lib.rs"

[[bin]]
name = "%%NAME%%"
path = "src/main.rs"

[dependencies]
%%DEP%%
tokio = { version = "1", features = ["rt-multi-thread", "net", "time", "io-util", "signal"] }
"#;

const SCAFFOLD_LIB: &str = r#"//! %%NAME%% — a rustango (Django-style, pure Rust) web app.
//!
//! Models, views and the project builder live here so both the binary
//! (src/main.rs) and the tests (tests/smoke.rs) share one definition.
//!
//! Run:     cargo run            (serves http://127.0.0.1:8000/)
//! Test:    cargo test
use std::sync::Arc;

use rustango::errors::HttpError;
use rustango::http::{HttpRequest, HttpResponse};
use rustango::orm::ModelExt as _;
use rustango::project::{App, Project};
use rustango::settings::Settings;
use rustango::urls::named_path;
use rustango::views::make_view;

/// A tiny model. `#[derive(rustango::Model)]` registers it with the ORM;
/// fields map to columns (`auto` = autoincrement pk).
#[derive(Clone, Debug, rustango::Model)]
#[model(table = "core_note")]
pub struct Note {
    #[field(auto)]
    pub id: i64,
    #[field(char, max = 200)]
    pub title: String,
    #[field(text)]
    pub body: String,
}

/// Build the app project. `database` is `"json:<file>"` (JsonFileBackend) or
/// `"memory"` for a RAM-only store (used by the tests).
pub fn build_with(database: &str) -> Project {
    Project::builder()
        .settings(
            Settings::builder()
                .database(database)
                .migrations_dir("migrations".into())
                .build(),
        )
        .add_app(App::new("core").models::<Note>())
        .urls(vec![
            named_path("", make_view(home), "home"),
            named_path("notes/<int:pk>/", make_view(note_detail), "note_detail"),
        ])
        .build()
        .expect("project builds")
}

/// The default project used by the binary (%%DB%%).
pub fn build() -> Project {
    build_with("%%DB%%")
}

/// Entry point used by src/main.rs: seed one row through the ORM, then serve.
pub fn run() {
    let project = Arc::new(build());
    let backend = project.backend.clone();
    if Note::objects(&*backend).count().unwrap_or(0) == 0 {
        let note = Note {
            id: 0,
            title: "Hello from rustango".into(),
            body: "This row was saved through the ORM.".into(),
        };
        note.save(&*backend).expect("seed note");
    }

    let addr: std::net::SocketAddr = "127.0.0.1:8000".parse().expect("addr");
    let rt = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .expect("tokio runtime");
    println!("%%NAME%% serving on http://{addr}/  (Ctrl-C to stop)");
    rt.block_on(rustango::server::serve(project, addr))
        .expect("server");
}

fn home(req: &mut HttpRequest) -> Result<HttpResponse, HttpError> {
    let backend = req.backend.clone().expect("backend attached");
    let notes: Vec<Note> = Note::objects(&*backend)
        .order_by("-id")
        .all()
        .unwrap_or_default();
    let mut rows = String::new();
    for n in &notes {
        rows.push_str(&format!(
            "<li><a href=\"/notes/{}/\">{}</a></li>",
            n.id,
            escape(&n.title)
        ));
    }
    let html = format!(
        "<!DOCTYPE html><html><head><title>%%TITLE%%</title></head><body>\
         <h1>%%TITLE%%</h1><p>notes: {}</p><ul>{}</ul>\
         <p>Edit src/lib.rs and rebuild to grow this app.</p>\
         </body></html>",
        notes.len(),
        rows
    );
    Ok(HttpResponse::html(html))
}

fn note_detail(req: &mut HttpRequest) -> Result<HttpResponse, HttpError> {
    let backend = req.backend.clone().expect("backend attached");
    let pk: i64 = req
        .params
        .get("pk")
        .and_then(|v| v.parse().ok())
        .ok_or_else(|| HttpError::BadRequest("bad pk".into()))?;
    let note = Note::get(&*backend, pk)
        .map_err(|_| HttpError::NotFound(format!("note {pk} not found")))?;
    Ok(HttpResponse::html(format!(
        "<!DOCTYPE html><html><body><h1>{}</h1><p>{}</p>\
         <p><a href=\"/\">back</a></p></body></html>",
        escape(&note.title),
        escape(&note.body)
    )))
}

fn escape(s: &str) -> String {
    s.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
}
"#;

const SCAFFOLD_MAIN: &str = r#"//! %%NAME%% — binary entry point. App code lives in src/lib.rs.
fn main() {
    %%CRATE%%::run();
}
"#;

const SCAFFOLD_TEST: &str = r#"//! Smoke tests through the in-process test client (no sockets needed).
use std::sync::Arc;

use rustango::orm::ModelExt as _;
use rustango::testclient::Client;

#[test]
fn home_renders_and_links_notes() {
    let project = Arc::new(%%CRATE%%::build_with("memory"));
    let mut client = Client::new(project);
    let res = client.get("/");
    assert_eq!(res.status, 200, "GET / must render");
    assert!(res.body.contains("%%NAME%%"));
}

#[test]
fn saved_note_is_reachable_through_its_url() {
    let project = Arc::new(%%CRATE%%::build_with("memory"));
    let backend = project.backend.clone();
    let note = %%CRATE%%::Note {
        id: 0,
        title: "First note".into(),
        body: "Body text".into(),
    };
    let saved = note.save(&*backend).expect("save");
    let mut client = Client::new(project);
    let res = client.get(&format!("/notes/{}/", saved.pk()));
    assert_eq!(res.status, 200);
    assert!(res.body.contains("First note"));
}

#[test]
fn unknown_note_is_404() {
    let project = Arc::new(%%CRATE%%::build_with("memory"));
    let mut client = Client::new(project);
    assert_eq!(client.get("/notes/9999/").status, 404);
}
"#;

const SCAFFOLD_README: &str = r#"# %%NAME%%

A Django-style, **pure Rust** web app built on
[rustango](https://docs.djangoproject.com/) — generated by RomaCode's
built-in `webapp_scaffold` tool.

## Run

```sh
cargo run        # serves http://127.0.0.1:8000/
```

## Test

```sh
cargo test       # in-process test-client smoke tests
```

## Dependency

```toml
%%DEP%%
```

`rustango` ships inside RomaCode at `crates/server/rustango` (ORM, template
engine, forms, admin, auth, sessions, CSRF, i18n, cache, migrations + the
`rustango` CLI). Point the path above at that checkout, or use a published
version once available.

## Layout

- `src/lib.rs` — model (`Note`), views, urls, `Project::builder()`
- `src/main.rs` — binary entry point (`%%CRATE%%::run()`)
- `tests/smoke.rs` — request/response round-trip via `rustango::testclient`
- database: `%%DB%%` (JsonFileBackend; pass `"memory"` to `build_with` for RAM)

## Next steps

- Add models: `#[derive(Clone, Debug, rustango::Model)] #[model(table = "...")]`
- Register routes in the `urls(vec![...])` list
- `rustango makemigrations` / `rustango migrate` for schema changes
- `rustango runserver` serves the built-in demo project
"#;

// ---------------------------------------------------------------------------
// webapp_check
// ---------------------------------------------------------------------------

fn webapp_check(input: &Value) -> Result<String, String> {
    let root = crate::security::root()?;
    webapp_check_at(&root, input)
}

fn webapp_check_at(root: &Path, input: &Value) -> Result<String, String> {
    let rel = input["path"].as_str().unwrap_or(".");
    let dir = resolve_under_root(root, rel)?;
    if !dir.is_dir() {
        return Err(format!(
            "{} is not a directory under the project root",
            dir.display()
        ));
    }

    let mut pass = 0usize;
    let mut warn = 0usize;
    let mut miss = 0usize;
    let mut report = String::new();

    {
        let mut check = |status: Check, label: &str, detail: &str| {
            let tag = match status {
                Check::Pass => {
                    pass += 1;
                    "PASS"
                }
                Check::Warn => {
                    warn += 1;
                    "WARN"
                }
                Check::Miss => {
                    miss += 1;
                    "MISS"
                }
            };
            if detail.is_empty() {
                report.push_str(&format!("{tag}  {label}\n"));
            } else {
                report.push_str(&format!("{tag}  {label} — {detail}\n"));
            }
        };

        // 1. manifest
        let cargo = read_opt(&dir.join("Cargo.toml")).unwrap_or_default();
        if cargo.is_empty() {
            check(Check::Miss, "Cargo.toml exists", "no manifest in this directory");
        } else if cargo.contains("rustango") {
            check(Check::Pass, "Cargo.toml declares the rustango dependency", "");
        } else {
            check(
                Check::Miss,
                "Cargo.toml declares the rustango dependency",
                "add `rustango = { path = \"...\" }` or a version requirement",
            );
        }

        // 2. entrypoint + app code (lib.rs holds the builder in scaffolds)
        let main = read_opt(&dir.join("src/main.rs")).unwrap_or_default();
        let lib = read_opt(&dir.join("src/lib.rs")).unwrap_or_default();
        let code = format!("{main}\n{lib}");
        if main.trim().is_empty() && lib.trim().is_empty() {
            check(Check::Miss, "src/main.rs (or src/lib.rs) exists", "no sources found");
        } else if code.contains("Project::builder") || code.contains("Router::new") {
            check(Check::Pass, "project builds a Project/Router", "");
        } else {
            check(
                Check::Warn,
                "project builds a Project/Router",
                "no Project::builder()/Router::new() call found",
            );
        }

        // 3. routes
        if code.contains("urls(vec!") || code.contains("named_path") {
            check(Check::Pass, "routes are registered (urls/named_path)", "");
        } else {
            check(Check::Warn, "routes are registered", "no url patterns found");
        }

        // 4. models
        let derive = code.contains("rustango::Model") || code.contains("Model)]");
        let attr = code.contains("#[model(");
        if derive && attr {
            check(
                Check::Pass,
                "ORM model(s) declared with #[derive(rustango::Model)] + #[model(...)]",
                "",
            );
        } else if derive || attr {
            check(
                Check::Warn,
                "ORM model(s) declared",
                "derive or #[model(...)] attribute missing",
            );
        } else {
            check(
                Check::Warn,
                "ORM model(s) declared",
                "no #[derive(rustango::Model)] found",
            );
        }

        // 5. tests
        let tests = dir.join("tests");
        let has_tests = tests.is_dir()
            && std::fs::read_dir(&tests)
                .map(|rd| {
                    rd.flatten()
                        .any(|e| e.path().extension().map(|x| x == "rs").unwrap_or(false))
                })
                .unwrap_or(false);
        if has_tests {
            check(Check::Pass, "tests/ contains Rust smoke tests", "");
        } else {
            check(
                Check::Warn,
                "tests/ contains Rust smoke tests",
                "add tests/smoke.rs using rustango::testclient",
            );
        }

        // 6. settings
        if code.contains("Settings::builder") {
            check(Check::Pass, "Settings::builder() configures the project", "");
        } else {
            check(
                Check::Warn,
                "Settings::builder() configures the project",
                "no settings call found",
            );
        }
    }

    report.push_str(&format!(
        "\nSummary: {pass} pass, {warn} warn, {miss} miss in {}",
        dir.display()
    ));
    Ok(report)
}

enum Check {
    Pass,
    Warn,
    Miss,
}

fn read_opt(path: &Path) -> Option<String> {
    std::fs::read_to_string(path).ok()
}

// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn tmp_root(tag: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "romacode-rustango-{}-{}",
            tag,
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn scaffold_writes_a_complete_project() {
        let root = tmp_root("scaffold");
        let out = webapp_scaffold_at(
            &root,
            &json!({"name": "mysite", "rustango_path": "../romacode/crates/server/rustango"}),
        )
        .unwrap();
        assert!(out.contains("mysite"), "{out}");

        let cargo = std::fs::read_to_string(root.join("mysite/Cargo.toml")).unwrap();
        assert!(cargo.contains("path = \"../romacode/crates/server/rustango\""), "{cargo}");
        assert!(cargo.contains("edition = \"2021\""));
        assert!(cargo.contains("tokio"));
        assert!(cargo.contains("[lib]"));

        let lib = std::fs::read_to_string(root.join("mysite/src/lib.rs")).unwrap();
        assert!(lib.contains("#[derive(Clone, Debug, rustango::Model)]"));
        assert!(lib.contains("#[model(table = \"core_note\")]"));
        assert!(lib.contains("Project::builder"));
        assert!(lib.contains("urls(vec!"));
        assert!(lib.contains("server::serve"));

        let main = std::fs::read_to_string(root.join("mysite/src/main.rs")).unwrap();
        assert!(main.contains("mysite::run()"), "{main}");

        let test = std::fs::read_to_string(root.join("mysite/tests/smoke.rs")).unwrap();
        assert!(test.contains("Client::new"));

        // refuse to overwrite a non-empty target
        let err = webapp_scaffold_at(&root, &json!({"name": "mysite"})).unwrap_err();
        assert!(err.contains("not empty"), "{err}");
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn scaffold_names_and_paths_are_sanitized() {
        let root = tmp_root("sanitize");
        assert!(webapp_scaffold_at(&root, &json!({"name": "../evil"})).is_err());
        assert!(webapp_scaffold_at(&root, &json!({"name": ""})).is_err());
        assert!(webapp_scaffold_at(&root, &json!({"name": "a/b"})).is_err());
        assert!(webapp_scaffold_at(&root, &json!({"name": "ok", "path": "../x"})).is_err());
        assert!(webapp_scaffold_at(&root, &json!({"name": "ok", "rustango_path": "with\"quote"})).is_err());
        assert!(webapp_scaffold_at(&root, &json!({"name": "ok", "rustango_path": "../vendor/rustango"})).is_ok());
        assert!(webapp_scaffold_at(&root, &json!({"name": "crate"})).is_err());
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn check_reports_missing_pieces_then_passes_after_scaffold() {
        let root = tmp_root("check");
        let dir = root.join("app");
        std::fs::create_dir_all(&dir).unwrap();

        // Empty dir → misses/warns, never panics.
        let report = webapp_check_at(&root, &json!({"path": "app"})).unwrap();
        assert!(report.contains("MISS"), "{report}");

        // Scaffold into it, then every check passes.
        webapp_scaffold_at(&dir, &json!({"name": "inner"})).unwrap();
        let report = webapp_check_at(&dir.join("inner"), &json!({})).unwrap();
        assert!(report.contains("0 miss"), "{report}");
        assert!(report.contains("PASS"), "{report}");
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn scaffold_defaults_use_a_version_dep() {
        let root = tmp_root("defaults");
        webapp_scaffold_at(&root, &json!({"name": "plain"})).unwrap();
        let cargo = std::fs::read_to_string(root.join("plain/Cargo.toml")).unwrap();
        assert!(cargo.contains("rustango = \"6.2\""), "{cargo}");
        let lib = std::fs::read_to_string(root.join("plain/src/lib.rs")).unwrap();
        assert!(lib.contains("json:db.json"), "{lib}");
        let _ = std::fs::remove_dir_all(&root);
    }
}
