# rustango — Django, rewritten in pure Rust

**rustango** is a batteries-included, Django-style web framework written
entirely in **Rust with zero C/C++ dependencies**. It re-implements the core
Django architecture — settings, URL routing, views (function + class-based),
a model/ORM layer with migrations, the Django Template Language, forms with
validation, sessions, authentication, CSRF, flash messages, middleware, an
auto-generated admin site, i18n catalogs, a cache framework, static files,
and a management CLI — plus a first-class **AI/LLM agent module**
(`contrib.ai`) for building LLM-backed applications.

```
License: BSD-3-Clause (inspired by Django; not affiliated with the Django project)
Version: 6.2.0-dev  (tracks the Django 6.2 API surface conceptually)
Status:  production-shaped dev framework — see the coverage matrix for details
```

---

## Quick start

```bash
# from this directory
cargo run -- runserver            # built-in demo project at http://127.0.0.1:8000
# open http://127.0.0.1:8000/admin/  (create a login: cargo run -- createsuperuser -u root -p secret)

cargo test                        # 198 unit & integration tests
cargo test --features ai          # + LLM client/agent tests (mocked server)

# scaffold a standalone project
cargo run -- startproject mysite
cd mysite && cargo run
```

Or use it as a library:

```toml
[dependencies]
rustango = "6.2"            # + features = ["ai"] for the LLM module
```

```rust
use std::sync::Arc;
use rustango::prelude::*;
use rustango::views::{make_view, render, View};
use rustango::templates::{Context, TplValue};

#[derive(Clone, Debug, rustango::Model)]
#[model(table = "polls_question")]
pub struct Question {
    #[field(auto)]
    pub id: i64,
    #[field(char, max = 200)]
    pub question_text: String,
    #[field(datetime, auto_now_add)]
    pub pub_date: rustango::types::DateTime,
}

fn main() {
    let project = Project::builder()
        .settings(Settings::builder().database("json:db.json").build())
        .add_app(App::new("polls").models::<Question>())
        .urls(vec![rustango::urls::named_path("", make_view(|req| {
            let mut ctx = Context::new();
            ctx.set("title", TplValue::Str("It works!".into()));
            render(req, "index.html", ctx)
        }), "home")])
        .admin(|site| site.register::<Question>(Default::default()))
        .build()
        .expect("project");

    let addr = "127.0.0.1:8000".parse().unwrap();
    tokio::runtime::Builder::new_multi_thread().enable_all().build().unwrap()
        .block_on(rustango::server::serve_with_shutdown(Arc::new(project), addr))
        .unwrap();
}
```

## Declaring models

```rust
#[derive(Clone, Debug, rustango::Model)]
#[model(table = "polls_choice")]
pub struct Choice {
    #[field(auto)]                       // AutoField primary key
    pub id: i64,
    #[field(fk, target = "polls_question")] // ForeignKey
    pub question_id: i64,
    #[field(char, max = 200)]            // CharField(max_length=200)
    pub choice_text: String,
    #[field(int, default = 0)]           // IntegerField(default=0)
    pub votes: i64,
}

// usage
let saved = choice.save(&*backend)?;                       // INSERT/UPDATE
let rows  = Choice::objects(&*backend)
    .filter(Q::contains("choice_text", "yes") | Q::gt("votes", 3))
    .order_by("-votes")
    .take(10)
    .all()?;
let author_names = Post::objects(&*backend)
    .filter(Q::eq("author_id__username", "ada"))           // FK traversal
    .all()?;
```

Field options: `auto`, `char (max = N)`, `text`, `int/float/bool (default = v)`,
`datetime (null, auto_now_add)`, `fk (target = "table")`, plus `unique`.
The Rust type drives row conversion (`i64`, `String`, `f64`, `bool`,
`DateTime`, `Option<DateTime>`).

## AI / LLM agent support (`contrib.ai`)

```rust
use rustango::contrib::ai::{AiClient, Agent, Tool};

let client = AiClient::from_settings();      // settings: ai_api_url / ai_api_key / ai_model
let reply = client.chat_simple("Explain Django's ORM in one paragraph.")?;

let agent = Agent::new(client)
    .system("You are the admin assistant.")
    .tool(Tool::simple("count_users", "Count rows in auth_user", |_args| {
        Ok(serde_json::json!({ "count": 42 }))
    }))
    .max_steps(8);
let answer = agent.run("How many users are there?")?;
```

Works with any OpenAI-compatible endpoint (OpenAI, Ollama, vLLM, LM Studio,
LiteLLM...). TLS is rustls with the `ring` provider — pure Rust, no OpenSSL.
The agent loop handles multi-step tool calling (`tool_calls` → results →
final answer) with a step budget.

## Architecture

```
request ─▶ middleware.process_request (top-down)
        ─▶ url resolver (path converters, named routes)
        ─▶ view (function or class-based)
        ─▶ ORM / templates / forms / messages
        ─▶ middleware.process_response (bottom-up)
```

The framework core is synchronous (WSGI-style); the HTTP edge is tokio+axum
running handlers on blocking worker threads. The test client drives the same
pipeline in-process.

### Concept map (Django → rustango)

| Django | rustango | Notes |
|---|---|---|
| `django.conf.settings` | `settings::Settings` (+ TOML overlay, `settings::global()`) | builder + `settings.toml` |
| `django.http` | `http::{HttpRequest, HttpResponse, QueryDict}` | cookies, headers, JSON/form bodies |
| `django.urls.path/re_path` | `urls::{path, Router, reverse}` | `<int:> <str:> <slug:> <uuid:> <path:>` converters, `include()` = `Router::mount` with namespace |
| `django.views.generic` | `views::{View, TemplateView, ListView, DetailView, RedirectView}` | `as_view()`, pagination via `?page=` |
| `django.middleware` | `middleware::Middleware` + built-ins | Security, Session, Common (APPEND_SLASH), CsrfView, Auth, Messages, Clickjacking, GZip |
| `django.db.models.Model` | `#[derive(Model)] #[model(table=...)]` + `#[field(...)]` | pk autoincrement, unique, defaults, auto_now_add |
| `Model.objects` | `ModelExt::objects(&backend)` | `filter/exclude/order_by/get/first/latest/count/exists/take/skip/update/delete` |
| `Q` objects | `orm::Q` (`&`, `|`, `!`) | incl. `author__username` FK traversal, `__year/month/day`, `__contains/icontains/startswith/gt/...` |
| DB backends | `Backend` trait: `InMemoryBackend`, `JsonFileBackend` | JSON snapshots are atomic (write tmp + rename) |
| migrations | `orm::migrations` + CLI `makemigrations/migrate/showmigrations` | JSON migration files, `_migrations` bookkeeping |
| `django.template` | `templates` module | `{{ var\|filter }}`, `{% for/if/elif/else %}`, `{% block %}`+`{% extends %}`, `{% include %}`, `{% with %}`, `{% csrf_token %}`, `{% url %}`, `{% static %}`, `{% trans %}`, `{% comment %}`, autoescape ON, 24 built-in filters, custom loaders |
| `django.forms` | `forms::{Form, CharField, EmailField, IntegerField, ...}` | `bind()`, `is_valid()`, `cleaned_data`, `errors`, `as_p()/as_table()` |
| `contrib.auth` | `auth` module | `User` model, PBKDF2-SHA256 in Django wire format (`pbkdf2_sha256$iter$salt$base64hash` — verifies Django-generated hashes), `authenticate/login/logout`, `login_required`, `createsuperuser` |
| `contrib.sessions` | `sessions` | server-side `django_session` table, `sessionid` cookie, `flush/cycle_key/clear_expired` |
| `middleware.csrf` | `csrf` | double-submit cookie + form/header token, `{% csrf_token %}` |
| `contrib.messages` | `messages` | session-backed flash messages, pop-on-read |
| `contrib.admin` | `admin::{AdminSite, ModelAdmin}` | auto CRUD (list/search/pagination/add/change/delete), login, embedded templates, `admin:` namespace |
| `contrib.staticfiles` | `staticfiles` | STATICFILES_DIRS serving + traversal protection |
| `core.cache` | `cache::{Cache, LocMemCache}` | TTL, add/touch, `cache_page()` decorator |
| `core.signals` | `signals` | `pre_save/post_save/pre_delete/post_delete`, request signals, panic-safe receivers |
| `utils.translation` | `i18n` | JSON catalogs, `{% trans %}`, language fallback (`de-de` → `de`) |
| `core.management` | `management` + `rustango` CLI | runserver, makemigrations, migrate, showmigrations, createsuperuser, clearsessions, check, startproject, startapp |
| `test.Client` | `testclient::Client` | in-process, cookie jar, `login()`, `force_login()`, `post_csrf()`, redirects |
| — (new) | `contrib::ai` | OpenAI-compatible client + tool-calling agent loop |

## Dependency purity

Zero C/C++ is linked. The stack:

| Purpose | Crate(s) | Notes |
|---|---|---|
| HTTP server edge | `tokio`, `axum`, `hyper` | pure Rust |
| Serialization | `serde`, `serde_json`, `toml` | pure Rust |
| Crypto | `sha2`, `hmac`, `pbkdf2`, `base64` (RustCrypto) | pure Rust |
| TLS (ai feature) | `ureq` + `rustls` + `ring` | pure Rust (not OpenSSL, not aws-lc-rs) |
| Misc | `chrono`, `uuid`, `flate2(rust_backend)`, `hex`, `thiserror` | pure Rust |
| Proc macros | `syn`, `quote` (rustango-macros) | pure Rust |

## Coverage matrix

**Implemented and tested** (see `cargo test` — 198 tests, plus 14
end-to-end integration tests exercising a full blog app: routing, FK
queries, CSRF-protected forms, auth flows, admin CRUD, flash messages,
JSON persistence across restarts):

- Request/response cycle, QueryDict, cookies, headers, file upload limits
- Router with converters, namespacing, `reverse()`, APPEND_SLASH
- ORM: models, FKs, joins via `__`, unique constraints, defaults,
  auto_now_add, QuerySet API, two backends, atomic JSON persistence
- Migrations: autodetection, migration files, apply/fake/show
- Template engine: full DTL subset listed above, inheritance chains,
  model-aware dotted lookup, 24 filters, lenient missing-variable handling
- Forms: 9 field types, validation messages matching Django's wording,
  bound/unbound rendering, error lists
- Auth: Django-compatible password hashes, session-fixation-safe login,
  permissions flags, `login_required`/`staff_required`, createsuperuser
- Sessions: server-side, expiry, rotation, clear_expired
- CSRF: double-submit, safe-method exemptions, header tokens
- Messages: levels, pop-on-read, template integration
- Admin: registration, changelist (search, order, pagination), add/change
  forms auto-generated from model schema, delete confirmation, staff auth
- Middleware: 8 built-ins, ordered request/response phases, error-response
  phase (cookies still set on 404/500)
- Signals, cache (TTL + page cache), i18n, static files, management CLI
- AI contrib: chat, tool calling, agent loop, mocked-server tests

**Partially implemented** (honest deltas vs Django):

- Storage: file-backed JSON engine instead of SQLite/PostgreSQL (the
  `Backend` trait is the extension point); no transactions/connection pools
- Migrations: no data migrations, column drops are recorded but no-op'd on
  the JSON engine, no `AlterField` semantics beyond schema swap
- Templates: no `{% load %}` template-tag libraries, no `{{ block.super }}`,
  custom filters registered via code rather than template libraries
- Forms: no `ModelForm` validation beyond schema-derived fields, no
  file uploads, no formsets
- Auth: no `Group`/`Permission` tables or per-object permissions
  (staff/superuser flags only)
- Admin: no inlines, no per-field `list_editable`, generic (row-based)
  change views
- i18n: gettext-style msgid lookup without plural forms or context

**Not implemented** (documented for completeness):

- GIS (`contrib.gis`), contenttypes/flatpages/redirects/sitemaps/syndication
  contrib apps, humanize
- Async views/ORM (the core is sync by design; the edge is async)
- Raw SQL, multi-database routing, database introspection commands
- Email backends, logging framework, `django.test` assertions runner

## Testing

```bash
cargo test                 # unit + integration (no network needed)
cargo test --features ai   # + ai module (mock OpenAI-compatible server)
```

The integration suite (`tests/integration.rs`) builds a complete blog app
and drives it through the in-process client: published-post filtering,
comment posting with CSRF + validation + flash messages, login/logout
cycles, FK-traversal queries, admin CRUD end-to-end, JSON persistence
across restarts, signal firing, and template inheritance with i18n.

## CLI reference

```text
rustango runserver [ADDR]                    # dev server (default 127.0.0.1:8000)
rustango makemigrations [APP]                # write migration files
rustango migrate [APP]                       # apply migrations
rustango showmigrations                      # [X]/[ ] list
rustango createsuperuser -u NAME -p PASS     # create/reset superuser
rustango clearsessions                       # purge expired sessions
rustango check                               # config summary
rustango startproject NAME [DIR]             # scaffold a project
rustango startapp NAME [DIR]                 # scaffold an app
```
