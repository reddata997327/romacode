# rustango — full analysis & debug report

**Input:** `rustango-source.zip` (a Django-style web framework written in pure Rust,
version 6.2.0-dev, ~30 source modules + proc-macro crate + integration tests).

**Result:** all bugs fixed, zero compiler warnings, zero clippy warnings
(`cargo clippy --all-targets --all-features`), **198 tests passing** (default) and
**203 with `--features ai`** (was 191, with 6 of the original tests asserting
wrong behavior). Every fix was verified against a live server (`runserver`) and
through the HTTP admin flow.

---

## 1. Real bugs found and fixed

### BUG-1 — Password hashes were NOT Django-compatible (auth.rs)  [critical]
The README claims `pbkdf2_sha256$iter$salt$hash` in "Django wire format", but
the digest was **hex-encoded**, while Django encodes it **base64**. A hash produced
by Django's `make_password()` could never verify here, and vice versa.
* `make_password` now emits `pbkdf2_sha256$<iter>$<salt 12 alnum>$<base64 digest>`
  — byte-identical to `django.contrib.auth.hashers.PBKDF2PasswordHasher.encode`.
* `check_password` verifies Django-generated hashes.
* The old test asserted the wrong format (`parts[3].len() == 64`) and contained a
  placeholder "Django hash" that tested nothing. Replaced with a **real Django
  test vector** (`pbkdf2_sha256$1000$cd3f9dfb48af6bb6$neYS372AH6...=`), verified
  byte-for-byte.

### BUG-2 — Deadlock in ORM `update`/`delete` with FK-traversal criteria (orm/backend.rs)  [critical]
`InMemoryBackend::update/delete` evaluated `Criteria` while holding the **write**
lock. FK-traversal criteria (`author__username`) call back into `self.select()`,
which tries to take the read lock on the same thread → guaranteed deadlock
(`QuerySet::filter(...fk...).update(...)` hangs forever).
* Restructured into two phases: (1) snapshot + evaluate criteria under the read
  lock, (2) apply mutations under the write lock (row-identity based, so it works
  for any table layout, pk or not). Regression test included.

### BUG-3 — Every anonymous request created a session row + cookie (middleware.rs)  [major]
`SessionMiddleware` saved whenever `modified || new` — a brand-new session is
always `new`, so *every* request from every visitor inserted a `django_session`
row and sent a `sessionid` cookie (session-table bloat, unlike Django's
`SESSION_SAVE_EVERY_REQUEST = False` default). Additionally
`messages::persist` unconditionally removed the `_messages` key, marking
untouched sessions as modified.
* Sessions are now saved only when **modified**; the cookie is only re-sent when
  the session was actually written (Django semantics). Regression test:
  `untouched_session_is_not_persisted`.

### BUG-4 — Expired sessions were still served (sessions.rs)  [major]
`Session::load` read the row but never checked `expire_date` — `is_expired()`
was dead code. Expired sessions stayed valid until `clearsessions` ran.
* `load` now treats expired rows as absent (fresh session), exactly like
  Django's session engine at load time. Regression test included.

### BUG-5 — `files/<path:rest>/` never matched anything (urls.rs)  [major]
The greedy `<path:…>` branch required `pos + 1 == segments.len()`; a trailing
slash adds an empty literal segment, so any `<path:x>/` pattern (trailing slash)
failed to match every URL. Only the no-trailing-slash static route worked.
* Greedy matching now honours the trailing slash: `files/<path:rest>/` matches
  `/files/a/b/c/` capturing `rest = "a/b/c"`, rejects non-slash-terminated paths
  and the bare prefix. Regression test + `reverse()` check included.

### BUG-6 — `Form::as_table()` rendered the error HTML as the label (forms.rs)  [major]
The format string mixed `{0}/{1}` with `{}` and passed only two args, so
`{1}` (label slot) received `error_html`. The stray `let _ = &field.label();`
hinted at the bug. Fixed arg order + regression test
(`as_table_shows_labels_not_errors`).

### BUG-7 — Hardcoded `/admin/` paths vs configurable `admin_url` (admin.rs + templates)  [major]
All admin redirects (`login`, `logout`, `add/change/delete`) and every admin
template link hard-coded `/admin/…`, breaking when `settings.admin_url` is
customized.
* `AdminSite` now carries `url_prefix` (synced from `settings.admin_url` in
  `ProjectBuilder::build`); redirects use it and templates use a new
  `{{ admin_prefix }}` context variable. All six embedded templates updated.

### BUG-8 — `csrf_exempt()` was a no-op (csrf.rs / urls.rs / project.rs)  [major]
The function returned the view unchanged while claiming "the dispatch pipeline
consults this marker" — no marker existed anywhere, so nothing was exempt.
* Routes now have a `csrf_exempt` flag (`Route::csrf_exempt()` /
  `csrf::csrf_exempt(route)`), the dispatcher sets a request flag on match, and
  CSRF verification skips exempt routes. Test included.

### BUG-9 — CLI migration test wrote into the source tree (management.rs)  [moderate]
`demo_project()` ignored the global settings, so the `makemigrations` CLI test
could write `migrations/*.json` into the crate root (order-dependent
pollution), and the `set_global` call in the test was dead.
* `demo_project()` now honours `RUSTANGO_MIGRATIONS_DIR` (like `RUSTANGO_DB`);
  the test runs hermetically against a temp dir.

### BUG-10 — JSON snapshot flush race (orm/backend.rs)  [moderate]
`JsonFileBackend::flush` wrote every snapshot to the same `*.tmp` path; two
concurrent writers could clobber each other before the rename.
* Temp files now carry a unique UUID suffix; the atomic write→rename is kept.

### BUG-11 — Only the first `Cookie:` header was parsed (server.rs)  [moderate]
Clients/proxies may send multiple `Cookie` headers; all but the first were
silently dropped (session/CSRF cookies could vanish).
* `Headers::get_all()` added; the edge joins every `Cookie` header before
  parsing. Test included.

### BUG-12 — `settings.toml` was dead weight in `startproject` (management.rs)  [moderate]
The scaffold wrote a `settings.toml` that `main.rs` never loaded (the README
promises "builder + settings.toml").
* The generated `main.rs` now overlays `settings.toml` onto the builder
  (verified: the scaffold creates `db.json` per `database = "json:db.json"`).
  Also silenced the `unused variable: site` warning in the generated closure.

### BUG-13 — `TRACE` not treated as a safe method (http.rs)  [minor]
Doc comment said "GET, HEAD, OPTIONS, TRACE" but the enum had no `TRACE`.
* `Method::TRACE` added and included in `is_safe()`, matching Django's CSRF
  safe-method set.

### BUG-14 — i18n language activation logic was wrong (i18n.rs)  [minor]
`activate()` accepted any cookie language when the configured list didn't
contain `"en-us"` — an unrelated condition.
* Now: accept the cookie language when configured languages exist-match or the
  list is empty; otherwise fall back to `language_code`.

### BUG-15 — Logout wrote a null auth key before deleting (auth.rs)  [minor]
`logout()` set `_auth_user_id` to null and then deleted the session anyway.
* Redundant write removed (`session.remove` + `delete`).

### BUG-16 — Middleware short-circuit ran the response phase for *all* middlewares (middleware.rs)  [minor]
Django runs `process_response` only for middlewares up to (and including) the
one that short-circuited; here the full stack ran even for components that
never saw the request. Fixed to `middlewares[..=i]`.

### BUG-17 — Doc/macros typos & misleading claims  [minor]
* macros crate: `#odel(...)]` → `#[model(...)]` (4 places).
* `csrf_exempt` docs, README crypto table (`regex` dropped, `hex`/`base64`
  clarified), test-count update (198).

---

## 2. Lint / hygiene fixes (now zero warnings)

* **Compiler & clippy (30 lib + 8 test warnings → 0):** unused imports
  (`hmac::Hmac`, `Session`, `Registry`, `ModelExt`, `OnceLock`, `HttpResponse`,
  `HttpError`, `TestResponse`, …), needless `mut`, dead code (`guard`,
  `db_path_arg`, `sess`, `view!` macro, `Converter::regex`), identity `.map`,
  `iter().any()` → `contains()`, collapsible `if let`, single-pattern `match`,
  identical `if` blocks, `unwrap` after `is_some`, `push_str("#")`, `&Box<dyn …>`
  → `&dyn …`, nested `format!`, useless `format!`, `format!`-in-args,
  non-snake-case names (`HttpResponse_to_bytes` → `http_response_to_bytes`;
  public `TextField` kept Django-style with an explicit allow), derivable
  `Default` impl, private type in public interface (`AdminEntry` now `pub`),
  unused `bool` field, complex type factored (`AdminConfig`, `ToolHandler`).
* **Dependencies:** removed unused `regex` (its only user was dead code);
  `base64` is now genuinely used by the auth fix; removed `hex` (orphaned by the
  base64 fix). `unused_crate_dependencies` clean.
* **Comment-only cleanups:** duplicated `let mut ctx = ctx;` in `render()`,
  dead branch in `Cmp::apply`, placeholder "Django hash" test replaced.

---

## 3. Verification performed

| Check | Result |
|---|---|
| `cargo build --all-targets` | OK, no warnings |
| `cargo clippy --all-targets --all-features` | 0 warnings, 0 errors |
| `cargo test` (default) | **184 unit + 14 integration = 198 passed** |
| `cargo test --features ai` | **189 unit + 14 integration = 203 passed** |
| `runserver` + curl: `/`, 404, security headers | OK |
| Anonymous GET | only `csrftoken` cookie (no session row) — BUG-3 verified |
| `createsuperuser` (json backend) + admin login POST (CSRF + Django-format hash) | 302 → `/admin/` |
| Wrong password | rejected with error page |
| Admin add / search / change / delete over HTTP | all 302 + data correct |
| JSON persistence (`db.json` after restart) | data survives |
| Logout → protected page | redirects to login |
| `startproject` scaffold | builds **and runs** warning-free; `settings.toml` applied |

---

## 4. Notes / intentional non-changes

* The `<path:…>` converter still matches an empty capture when the pattern has
  no trailing slash (pre-existing documented quirk, covered by an existing test);
  Django's converter is `.+` — kept for backwards compatibility.
* Sessions remain server-side with a JSON blob (as designed); the `Backend`
  trait remains the extension point for real databases.
* Storage engine semantics for `DropColumn` remain no-op on the JSON engine
  (documented limitation, unchanged).
