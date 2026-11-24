//! Management commands — `django.core.management` (`manage.py` / `django-admin`).
//!
//! ```text
//! rustango runserver [addr]          start the dev server
//! rustango makemigrations [app]      detect schema changes, write migration files
//! rustango migrate [app]             apply pending migrations
//! rustango showmigrations            list applied/pending migrations
//! rustango createsuperuser -u root -p secret   create or reset a superuser
//! rustango clearsessions             purge expired sessions
//! rustango check                     validate settings + registry
//! rustango startproject NAME [DIR]   scaffold a new project
//! rustango startapp NAME [DIR]       scaffold a new app
//! ```

use std::path::PathBuf;
use std::sync::Arc;

use crate::project::Project;
use crate::settings::Settings;

pub const HELP: &str = r#"rustango — Django-style web framework in pure Rust

USAGE:
    rustango <COMMAND> [ARGS]

COMMANDS:
    runserver [ADDR]              Start the development server (default 127.0.0.1:8000)
    makemigrations [APP]          Write migration files for schema changes
    migrate [APP]                 Apply pending migrations
    showmigrations                Show applied/pending migrations
    createsuperuser -u NAME -p PW Create (or reset) a superuser
    clearsessions                 Delete expired sessions
    check                         Validate configuration
    startproject NAME [DIR]       Scaffold a new project
    startapp NAME [DIR]           Scaffold a new app inside a project
    help                          Show this help
"#;

/// Build the built-in demo project used by `runserver` and DB commands when
/// invoked inside the framework checkout (no user project configured).
/// Set `RUSTANGO_DB=json:<path>` for a persistent demo database and
/// `RUSTANGO_MIGRATIONS_DIR=<path>` to relocate migration files.
pub fn demo_project() -> Project {
    let database = std::env::var("RUSTANGO_DB").unwrap_or_else(|_| "memory".into());
    let migrations_dir =
        std::env::var("RUSTANGO_MIGRATIONS_DIR").map(PathBuf::from).unwrap_or_else(|_| PathBuf::from("migrations"));
    let project = crate::project::Project::builder()
        .settings(
            Settings::builder()
                .database(database)
                .migrations_dir(migrations_dir)
                .build(),
        )
        .add_app(crate::project::App::new("core"))
        .urls(vec![crate::urls::named_path(
            "",
            crate::views::make_view(|_req| {
                let html = format!(
                    "<!DOCTYPE html><html><head><title>rustango</title></head><body>\
                     <h1>It works! (rustango {})</h1>\
                     <p>This is the built-in demo project. Visit <a href=\"/admin/\">/admin/</a>.</p>\
                     </body></html>",
                    crate::VERSION
                );
                Ok(crate::http::HttpResponse::html(html))
            }),
            "home"),
        ])
        .admin(|site| {
            site.register::<crate::auth::User>(crate::admin::ModelAdmin {
                list_display: vec!["id".into(), "username".into(), "email".into(), "is_staff".into()],
                search_fields: vec!["username".into(), "email".into()],
                ..Default::default()
            });
        })
        .build();
    match project {
        Ok(p) => p,
        Err(e) => panic!("demo project failed to build: {e}"),
    }
}

/// Run one management command; returns a process exit code.
pub fn run(args: &[String]) -> i32 {
    let cmd = args.first().map(|s| s.as_str()).unwrap_or("help");
    let rest = &args[1.min(args.len())..];
    match cmd {
        "help" | "--help" | "-h" => {
            println!("{HELP}");
            0
        }
        "version" | "--version" => {
            println!("rustango {}", crate::VERSION);
            0
        }
        "runserver" => {
            let addr: std::net::SocketAddr = rest
                .first()
                .cloned()
                .unwrap_or_else(|| "127.0.0.1:8000".to_string())
                .parse()
                .unwrap_or_else(|_| {
                    eprintln!("invalid address `{}`", rest.first().cloned().unwrap_or_default());
                    std::process::exit(2);
                });
            let project = Arc::new(demo_project());
            let runtime = tokio::runtime::Builder::new_multi_thread()
                .enable_all()
                .build()
                .expect("tokio runtime");
            match runtime.block_on(crate::server::serve_with_shutdown(project, addr)) {
                Ok(()) => 0,
                Err(e) => {
                    eprintln!("server error: {e}");
                    1
                }
            }
        }
        "makemigrations" => {
            let project = demo_project();
            let app = rest.first().cloned().unwrap_or_else(|| "core".to_string());
            let metas: Vec<crate::orm::ModelMeta> =
                crate::orm::registry().read().unwrap().models.clone();
            let dir = project.settings.migrations_dir.clone();
            match crate::orm::migrations::makemigrations(
                &*project.backend,
                &dir,
                &app,
                &metas,
                false,
            ) {
                Ok(Some(m)) => {
                    println!("Wrote {} ({} ops)", dir.join(m.file_name()).display(), m.ops.len());
                    for op in &m.ops {
                        println!("  - {}", op.describe());
                    }
                    0
                }
                Ok(None) => {
                    println!("No changes detected in app `{app}`");
                    0
                }
                Err(e) => {
                    eprintln!("makemigrations failed: {e}");
                    1
                }
            }
        }
        "migrate" => {
            let project = demo_project();
            let dir = project.settings.migrations_dir.clone();
            let app = rest.first().map(|s| s.as_str());
            match crate::orm::migrations::migrate(&*project.backend, &dir, app) {
                Ok(applied) => {
                    if applied.is_empty() {
                        println!("Nothing to apply.");
                    }
                    for line in applied {
                        println!("Applying {line}... OK");
                    }
                    0
                }
                Err(e) => {
                    eprintln!("migrate failed: {e}");
                    1
                }
            }
        }
        "showmigrations" => {
            let project = demo_project();
            match crate::orm::migrations::show_migrations(&*project.backend, &project.settings.migrations_dir) {
                Ok(text) => {
                    print!("{text}");
                    0
                }
                Err(e) => {
                    eprintln!("showmigrations failed: {e}");
                    1
                }
            }
        }
        "createsuperuser" => {
            let project = demo_project();
            let mut username = String::new();
            let mut password = String::new();
            let mut i = 0;
            while i < rest.len() {
                match rest[i].as_str() {
                    "-u" | "--username" => {
                        username = rest.get(i + 1).cloned().unwrap_or_default();
                        i += 2;
                    }
                    "-p" | "--password" => {
                        password = rest.get(i + 1).cloned().unwrap_or_default();
                        i += 2;
                    }
                    _ => i += 1,
                }
            }
            if username.is_empty() || password.is_empty() {
                eprintln!("usage: rustango createsuperuser -u NAME -p PASSWORD");
                return 2;
            }
            match crate::auth::create_superuser(&*project.backend, &username, "", &password) {
                Ok(u) => {
                    println!("Superuser `{}` created (pk {}).", u.username, u.pk());
                    0
                }
                Err(e) => {
                    eprintln!("createsuperuser failed: {e}");
                    1
                }
            }
        }
        "clearsessions" => {
            let project = demo_project();
            let n = crate::sessions::Session::clear_expired(&*project.backend);
            println!("Deleted {n} expired session(s).");
            0
        }
        "check" => {
            let project = demo_project();
            let models = crate::orm::registry().read().unwrap().models.len();
            println!("rustango {} — OK", crate::VERSION);
            println!("  backend:      {}", project.settings.database);
            println!("  debug:        {}", project.settings.debug);
            println!("  admin:        {} (at /{})", project.admin.is_some(), project.settings.admin_url);
            println!("  models:       {models} registered");
            println!("  routes:       {} top-level", project.router.routes.len());
            println!("  middleware:   {}", project.stack.middlewares.len());
            0
        }
        "startproject" => startproject(rest),
        "startapp" => startapp(rest),
        other => {
            eprintln!("Unknown command `{other}`.\n\n{HELP}");
            2
        }
    }
}

fn startproject(rest: &[String]) -> i32 {
    let name = match rest.first() {
        Some(n) if !n.is_empty() => n.clone(),
        _ => {
            eprintln!("usage: rustango startproject NAME [DIR]");
            return 2;
        }
    };
    let dir = PathBuf::from(rest.get(1).cloned().unwrap_or_else(|| name.clone()));
    let root = dir.join(&name);

    if root.exists() {
        eprintln!("`{}` already exists", root.display());
        return 1;
    }
    let _ = std::fs::create_dir_all(root.join("src"));
    let _ = std::fs::create_dir_all(root.join("templates"));

    // Locate the framework checkout so the scaffold can depend on it via a
    // path (rustango is not published to crates.io). Walk up from the
    // running executable: target/{debug,release}/rustango -> ../../Cargo.toml
    let framework_root = find_framework_root();
    let rustango_dep = match &framework_root {
        Some(path) => format!(
            "rustango = {{ path = \"{}\" }}  # or \"6.2\" once published",
            path.display()
        ),
        None => "rustango = \"6.2\"  # set RUSTANGO_SRC or a path dependency until published"
            .to_string(),
    };
    let cargo_toml = format!(
        r#"[package]
name = "{name}"
version = "0.1.0"
edition = "2021"

[dependencies]
{rustango_dep}
tokio = {{ version = "1", features = ["rt-multi-thread", "macros"] }}
"#
    );
    let main_rs = r#"use rustango::prelude::*;
use rustango::views::{make_view, render};
use rustango::templates::{Context, TplValue};

fn index(req: &mut rustango::http::HttpRequest) -> Result<rustango::http::HttpResponse, rustango::errors::HttpError> {
    let mut ctx = Context::new();
    ctx.set("title", TplValue::Str("It works!".into()));
    render(req, "index.html", ctx)
}

fn main() {
    // Settings: builder defaults, overlaid by settings.toml when present.
    let mut settings = Settings::builder()
        .template_dirs(vec![std::path::PathBuf::from("templates")])
        .build();
    if let Ok(text) = std::fs::read_to_string("settings.toml") {
        settings.load_toml(&text).expect("invalid settings.toml");
    }

    let project = Project::builder()
        .settings(settings)
        .urls(vec![rustango::urls::named_path("", make_view(index), "home")])
        .admin(|_site| {
            // _site.register::<MyModel>(Default::default());
        })
        .build()
        .expect("project");

    let addr = std::env::var("RUSTANGO_ADDR")
        .unwrap_or_else(|_| "127.0.0.1:8000".into())
        .parse()
        .expect("addr");
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .expect("runtime");
    runtime.block_on(rustango::server::serve_with_shutdown(std::sync::Arc::new(project), addr))
        .expect("server");
}
"#;
    let index_html = r#"<!DOCTYPE html>
<html><head><title>{{ title }}</title></head>
<body><h1>{{ title }}</h1>
<p>Edit <code>templates/index.html</code> and <code>src/main.rs</code> to get started.</p>
</body></html>
"#;
    let settings_toml = r#"debug = true
database = "json:db.json"
language_code = "en-us"
"#;

    let w = |p: &std::path::Path, c: &str| {
        std::fs::write(p, c).map_err(|e| e.to_string())
    };
    let results = vec![
        w(&root.join("Cargo.toml"), &cargo_toml),
        w(&root.join("src/main.rs"), main_rs),
        w(&root.join("templates/index.html"), index_html),
        w(&root.join("settings.toml"), settings_toml),
    ];
    for r in results {
        if let Err(e) = r {
            eprintln!("startproject failed: {e}");
            return 1;
        }
    }
    println!("Project scaffolded at {}:", root.display());
    println!("  cd {}", root.display());
    println!("  cargo run        # starts http://127.0.0.1:8000");
    0
}

fn find_framework_root() -> Option<PathBuf> {
    if let Ok(env_path) = std::env::var("RUSTANGO_SRC") {
        let p = PathBuf::from(env_path);
        if p.join("Cargo.toml").exists() {
            return Some(p);
        }
    }
    let exe = std::env::current_exe().ok()?;
    let mut dir = exe.parent()?.to_path_buf();
    for _ in 0..4 {
        dir = dir.parent()?.to_path_buf();
        let cargo = dir.join("Cargo.toml");
        if let Ok(text) = std::fs::read_to_string(&cargo) {
            if text.contains("name = \"rustango\"") {
                return Some(dir);
            }
        }
    }
    None
}

fn startapp(rest: &[String]) -> i32 {
    let name = match rest.first() {
        Some(n) if !n.is_empty() && n.chars().all(|c| c.is_ascii_lowercase() || c == '_') => n.clone(),
        _ => {
            eprintln!("usage: rustango startapp NAME  (lowercase name)");
            return 2;
        }
    };
    let dir = PathBuf::from(rest.get(1).cloned().unwrap_or_else(|| name.clone()));
    let app_dir = dir.join(&name);
    if app_dir.exists() {
        eprintln!("`{}` already exists", app_dir.display());
        return 1;
    }
    let _ = std::fs::create_dir_all(app_dir.join("templates"));
    let models_rs = format!(
        r#"use rustango::orm::{{Model, ModelExt, QuerySet}};
use rustango::types::DateTime;

// Declare models with the derive macro:
// #[derive(Clone, Debug, rustango::Model)]
// #[model(table = "{name}_thing")]
// pub struct Thing {{
//     #[field(auto)]
//     pub id: i64,
//     #[field(char, max = 100)]
//     pub name: String,
//     #[field(datetime, auto_now_add)]
//     pub created: DateTime,
// }}
"#
    );
    let views_rs = r#"use rustango::errors::HttpError;
use rustango::http::HttpResponse;
use rustango::http::HttpRequest;
use rustango::views::render;
use rustango::templates::Context;
"#;
    let w = |p: &std::path::Path, c: &str| std::fs::write(p, c).map_err(|e| e.to_string());
    let results = vec![
        w(&app_dir.join("models.rs"), &models_rs),
        w(&app_dir.join("views.rs"), views_rs),
        w(&app_dir.join("templates/.gitkeep"), ""),
    ];
    for r in results {
        if let Err(e) = r {
            eprintln!("startapp failed: {e}");
            return 1;
        }
    }
    println!("App `{name}` scaffolded at {}", app_dir.display());
    println!("  Wire it up in your project builder: .urls(vec![...]) and .add_app(...)");
    0
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn help_and_version() {
        assert_eq!(run(&["help".into()]), 0);
        assert_eq!(run(&["version".into()]), 0);
    }

    #[test]
    fn check_runs() {
        assert_eq!(run(&["check".into()]), 0);
    }

    #[test]
    fn unknown_command() {
        assert_eq!(run(&["bogus".into()]), 2);
    }

    #[test]
    fn makemigrate_and_migrate_cli() {
        // Keep the CLI hermetic: migration files go to a temp dir.
        let dir = std::env::temp_dir().join(format!(
            "rustango-cli-mig-{}",
            uuid::Uuid::new_v4()
        ));
        // SAFETY: this test is the only reader/writer of this variable.
        std::env::set_var("RUSTANGO_MIGRATIONS_DIR", &dir);
        assert_eq!(run(&["makemigrations".into(), "core".into()]), 0);
        assert_eq!(run(&["migrate".into()]), 0);
        assert_eq!(run(&["showmigrations".into()]), 0);
        std::env::remove_var("RUSTANGO_MIGRATIONS_DIR");
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn startproject_scaffold() {
        let tmp = std::env::temp_dir().join(format!("rustango-sp-{}", uuid::Uuid::new_v4()));
        let code = run(&[
            "startproject".into(),
            "mysite".into(),
            tmp.to_string_lossy().to_string(),
        ]);
        assert_eq!(code, 0);
        assert!(tmp.join("mysite/Cargo.toml").exists());
        assert!(tmp.join("mysite/src/main.rs").exists());
        assert!(tmp.join("mysite/templates/index.html").exists());
        let main_rs = std::fs::read_to_string(tmp.join("mysite/src/main.rs")).unwrap();
        assert!(main_rs.contains("Project::builder()"));
        std::fs::remove_dir_all(&tmp).ok();
    }

    #[test]
    fn startapp_scaffold() {
        let tmp = std::env::temp_dir().join(format!("rustango-sa-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&tmp).unwrap();
        let code = run(&["startapp".into(), "polls".into(), tmp.to_string_lossy().to_string()]);
        assert_eq!(code, 0);
        assert!(tmp.join("polls/models.rs").exists());
        assert!(tmp.join("polls/views.rs").exists());
        std::fs::remove_dir_all(&tmp).ok();
    }
}
