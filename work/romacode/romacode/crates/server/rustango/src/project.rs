//! `Project` — the settings module + ROOT_URLCONF + app registry, assembled
//! in code:
//!
//! ```ignore
//! use rustango::prelude::*;
//!
//! let mut project = Project::builder()
//!     .settings(Settings::builder().debug(true).build())
//!     .add_app(App::new("polls").models::<Question>().models::<Choice>())
//!     .urls(vec![path("polls/", polls_index)])
//!     .admin(|site| { site.register::<Question>(); })
//!     .build();
//! let res = project.handle_request(&mut req);
//! ```

use std::sync::Arc;

use crate::errors::HttpError;
use crate::http::{HttpRequest, HttpResponse};
use crate::middleware::{Middleware, MiddlewareStack};
use crate::orm::backend::backend_from_alias;
use crate::orm::{Backend, InMemoryBackend, ModelMeta};
use crate::settings::Settings;
use crate::templates::{Engine, FileSystemLoader};
use crate::urls::Router;

/// Callback that configures the admin site (`.admin(|site| ...)`).
type AdminConfig = Box<dyn FnOnce(&mut crate::admin::AdminSite) + Send>;

/// An app: a name plus the models it declares — `INSTALLED_APPS` + models.py.
pub struct App {
    pub name: String,
    pub models: Vec<ModelMeta>,
}

impl App {
    pub fn new(name: impl Into<String>) -> App {
        App { name: name.into(), models: Vec::new() }
    }

    /// Register a model with this app and the global registry.
    pub fn models<T: crate::orm::Model>(mut self) -> App {
        let meta = ModelMeta::of::<T>();
        crate::orm::register_model(meta.clone());
        self.models.push(meta);
        self
    }
}

/// The assembled project: settings + backend + engine + router + middleware.
pub struct Project {
    pub settings: Settings,
    pub backend: Arc<dyn Backend>,
    pub engine: Arc<Engine>,
    pub router: Arc<Router>,
    pub stack: MiddlewareStack,
    pub admin: Option<Arc<crate::admin::AdminSite>>,
}

pub struct ProjectBuilder {
    settings: Settings,
    backend: Option<Arc<dyn Backend>>,
    apps: Vec<App>,
    routes: Vec<crate::urls::Route>,
    extra_middleware: Vec<Arc<dyn Middleware>>,
    admin_config: Option<AdminConfig>,
    admin_enabled: bool,
    extra_loaders: Vec<Box<dyn crate::templates::TemplateLoader>>,
}

impl Default for ProjectBuilder {
    fn default() -> Self {
        Self::new()
    }
}

impl ProjectBuilder {
    pub fn new() -> ProjectBuilder {
        ProjectBuilder {
            settings: Settings::default(),
            backend: None,
            apps: Vec::new(),
            routes: Vec::new(),
            extra_middleware: Vec::new(),
            admin_config: None,
            admin_enabled: true,
            extra_loaders: Vec::new(),
        }
    }

    pub fn settings(mut self, s: Settings) -> Self {
        self.settings = s;
        self
    }

    pub fn backend(mut self, b: Arc<dyn Backend>) -> Self {
        self.backend = Some(b);
        self
    }

    pub fn add_app(mut self, app: App) -> Self {
        self.apps.push(app);
        self
    }

    pub fn urls(mut self, routes: Vec<crate::urls::Route>) -> Self {
        self.routes.extend(routes);
        self
    }

    /// Append user middleware AFTER the default stack.
    pub fn middleware(mut self, mw: Arc<dyn Middleware>) -> Self {
        self.extra_middleware.push(mw);
        self
    }

    /// Configure the admin site.
    pub fn admin<F>(mut self, f: F) -> Self
    where
        F: FnOnce(&mut crate::admin::AdminSite) + Send + 'static,
    {
        self.admin_config = Some(Box::new(f));
        self.admin_enabled = true;
        self
    }

    /// Additional template loaders (checked after DIRS, before admin ones).
    pub fn template_loader(mut self, loader: Box<dyn crate::templates::TemplateLoader>) -> Self {
        self.extra_loaders.push(loader);
        self
    }

    /// Assemble everything.
    pub fn build(self) -> Result<Project, String> {
        let settings = self.settings;

        let backend: Arc<dyn Backend> = match self.backend {
            Some(b) => b,
            None => backend_from_alias(&settings.database)
                .map_err(|e| format!("backend error: {e}"))?,
        };

        // Built-in framework tables (auth_user, django_session, ...).
        ensure_builtin_schema(&*backend).map_err(|e| e.to_string())?;

        // Ensure every registered model's table exists (dev convenience —
        // production runs `rustango migrate`).
        {
            let reg = crate::orm::registry().read().unwrap().models.clone();
            for meta in &reg {
                backend
                    .create_table(&meta.table, meta.columns.clone())
                    .map_err(|e| e.to_string())?;
            }
        }

        // Admin site.
        let admin = if self.admin_enabled && settings.admin_enabled {
            let mut site = crate::admin::AdminSite::new(
                settings.site_title.clone(),
                settings.site_header.clone(),
            );
            if let Some(f) = self.admin_config {
                f(&mut site);
            }
            // Keep redirects/templates in sync with the configured mount point.
            site.url_prefix = settings.admin_url.trim_matches('/').to_string();
            Some(Arc::new(site))
        } else {
            None
        };

        // Template engine: DIRS first, then extra loaders, then embedded
        // admin templates.
        let mut loaders: Vec<Box<dyn crate::templates::TemplateLoader>> = vec![Box::new(
            FileSystemLoader::new(settings.template_dirs.clone()),
        )];
        for l in self.extra_loaders {
            loaders.push(l);
        }
        loaders.push(Box::new(crate::admin::embedded_templates()));
        let engine = Arc::new(Engine::new(loaders, settings.debug));

        // Router: user urls + admin + static.
        let mut router = Router::new(self.routes);
        if let Some(admin) = &admin {
            let admin_router = admin.urls();
            router.mount(&settings.admin_url, "admin", admin_router);
        }
        if !settings.staticfiles_dirs.is_empty() {
            let static_pattern = format!("{}<path:asset>", settings.static_url);
            router
                .routes
                .push(crate::urls::path(&static_pattern, crate::views::make_view(|req| {
                    crate::staticfiles::serve(req)
                })));
        }
        let router = Arc::new(router);
        crate::urls::set_route_registry((*router).clone());

        // Middleware.
        let mut middlewares = crate::middleware::default_stack(&settings, backend.clone(), router.clone());
        for mw in self.extra_middleware {
            middlewares.push(mw);
        }

        // Global settings snapshot (django.conf.settings equivalent).
        let _ = crate::settings::set_global(settings.clone());

        Ok(Project {
            settings,
            backend,
            engine,
            router,
            stack: MiddlewareStack::new(middlewares),
            admin,
        })
    }
}

fn ensure_builtin_schema(backend: &dyn Backend) -> crate::orm::OrmResult<()> {
    use crate::orm::{ColumnDesc, Model as _};
    backend.create_table("auth_user", crate::auth::User::columns().to_vec())?;
    backend.create_table("django_session", crate::sessions::session_columns())?;
    backend.create_table(
        "_migrations",
        vec![
            ColumnDesc::auto("id"),
            ColumnDesc::char("app", 100),
            ColumnDesc::char("name", 255),
            ColumnDesc::datetime("applied_at", false),
        ],
    )?;
    Ok(())
}

impl Project {
    pub fn builder() -> ProjectBuilder {
        ProjectBuilder::new()
    }

    /// The full request pipeline — what `runserver` and the test client call.
    pub fn handle_request(&self, req: &mut HttpRequest) -> HttpResponse {
        req.engine = Some(self.engine.clone());
        req.backend = Some(self.backend.clone());

        crate::signals::emit_request(&req.full_path, "request_started");
        let view = self.resolve_view(req);
        let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            self.stack.dispatch(req, &view)
        }));

        let response = match result {
            Ok(Ok(res)) => res,
            Ok(Err(err)) => {
                let mut res = self.render_error(req, err);
                // Error responses still pass through the response phase so
                // cookies/headers (CSRF, session, security) are applied.
                self.stack.run_response(req, &mut res);
                res
            }
            Err(panic) => {
                let msg = panic
                    .downcast_ref::<&str>()
                    .map(|s| s.to_string())
                    .or_else(|| panic.downcast_ref::<String>().cloned())
                    .unwrap_or_else(|| "view panicked".to_string());
                let mut res =
                    self.render_error(req, HttpError::ServerError(msg));
                self.stack.run_response(req, &mut res);
                res
            }
        };

        crate::signals::emit_request(&req.full_path, "request_finished");
        response
    }

    fn resolve_view(&self, req: &mut HttpRequest) -> crate::views::ViewFn {
        match self.router.match_path(&req.path) {
            Some((route, params)) => {
                req.params = params;
                req.url_name = route.name.clone();
                if route.csrf_exempt {
                    req.params
                        .insert(crate::csrf::EXEMPT_FLAG.to_string(), "1".to_string());
                }
                route.view
            }
            None => crate::views::make_view(|_req| {
                Err(HttpError::NotFound("no route matched".into()))
            }),
        }
    }

    /// DEBUG pretty errors; terse production ones.
    fn render_error(&self, req: &mut HttpRequest, err: HttpError) -> HttpResponse {
        let status = err.status();
        let _ = req;
        if self.settings.debug {
            let html = format!(
                "<!DOCTYPE html><html><head><title>{status} — rustango</title>\
                 <style>body{{font-family:sans-serif;max-width:720px;margin:40px auto;color:#222}}\
                 h1{{color:#b00}} pre{{background:#f4f4f4;padding:12px;border-radius:6px}}</style></head>\
                 <body><h1>{status}</h1><h2>{kind}</h2><pre>{msg}</pre>\
                 <p><small>rustango {version} — DEBUG page (set debug=false to hide)</small></p>\
                 </body></html>",
                status = status,
                kind = kind_of(&err),
                msg = crate::templates::escape_html(&err.to_string()),
                version = crate::VERSION,
            );
            HttpResponse::with_body(status, "text/html; charset=utf-8", html)
        } else {
            HttpResponse::with_body(
                status,
                "text/plain; charset=utf-8",
                format!("{} {}", status, kind_of(&err)),
            )
        }
    }

    /// Convenience for tests/REPL: a request straight through the pipeline.
    pub fn test_request(&self, mut req: HttpRequest) -> HttpResponse {
        self.handle_request(&mut req)
    }
}

fn kind_of(err: &HttpError) -> &'static str {
    match err {
        HttpError::NotFound(_) => "Not Found",
        HttpError::PermissionDenied(_) => "Permission Denied",
        HttpError::BadRequest(_) => "Bad Request",
        HttpError::MethodNotAllowed(_) => "Method Not Allowed",
        HttpError::CsrfFailure(_) => "CSRF Verification Failed",
        HttpError::ServerError(_) => "Server Error",
    }
}

/// In-memory project helper for tests.
pub fn memory_project() -> Project {
    Project::builder()
        .settings(Settings::builder().build())
        .backend(Arc::new(InMemoryBackend::new()))
        .urls(vec![])
        .build()
        .expect("memory project")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::http::Method;
    use crate::orm::ModelExt as _;
    use crate::templates::InMemoryLoader;
    use crate::urls::{path, named_path};
    use std::sync::Arc as StdArc;

    #[derive(Clone, Debug, crate::Model)]
    #[model(table = "proj_greeting")]
    pub struct Greeting {
        #[field(auto)]
        pub id: i64,
        #[field(char, max = 60)]
        pub text: String,
    }

    #[test]
    fn project_dispatch_full_pipeline() {
        let index = crate::views::make_view(|req| {
            let mut ctx = crate::templates::Context::new();
            ctx.set("path", crate::templates::TplValue::Str(req.path.clone()));
            crate::views::render(req, "index.html", ctx)
        });
        let project = Project::builder()
            .settings(Settings::builder().build())
            .backend(StdArc::new(InMemoryBackend::new()))
            .urls(vec![
                named_path("", index, "home"),
                path("missing/", crate::views::make_view(|_r| Ok(HttpResponse::text("x")))),
            ])
            .template_loader(Box::new({
                let mut l = InMemoryLoader::new();
                l.add("index.html", "PATH={{ request.path }} U={{ user.username }}");
                l
            }))
            .build()
            .unwrap();

        // normal dispatch through middleware + router + template
        let mut req = HttpRequest::new(Method::GET, "/");
        let res = project.handle_request(&mut req);
        assert_eq!(res.status, 200);
        assert!(res.body_string().contains("PATH=/"));

        // 404 handling (debug page)
        let mut req404 = HttpRequest::new(Method::GET, "/nowhere/");
        let res404 = project.handle_request(&mut req404);
        assert_eq!(res404.status, 404);
        assert!(res404.body_string().contains("Not Found"));

        // APPEND_SLASH via CommonMiddleware — Django-style permanent (301)
        // redirect, query string preserved.
        let mut reqslash = HttpRequest::new(Method::GET, "/missing");
        let resslash = project.handle_request(&mut reqslash);
        assert_eq!(resslash.status, 301);
        assert_eq!(resslash.headers.get("Location"), Some("/missing/"));
    }

    #[test]
    fn panicking_view_becomes_500() {
        let boom = crate::views::make_view(|_req| -> Result<HttpResponse, HttpError> {
            panic!("intentional")
        });
        let project = Project::builder()
            .backend(StdArc::new(InMemoryBackend::new()))
            .urls(vec![path("boom/", boom)])
            .build()
            .unwrap();
        let mut req = HttpRequest::new(Method::GET, "/boom/");
        let res = project.handle_request(&mut req);
        assert_eq!(res.status, 500);
        assert!(res.body_string().contains("intentional"));
    }

    #[test]
    fn model_tables_created_on_build() {
        let project = Project::builder()
            .backend(StdArc::new(InMemoryBackend::new()))
            .add_app(App::new("test").models::<Greeting>())
            .build()
            .unwrap();
        assert!(project.backend.tables().contains(&"proj_greeting".to_string()));
        // auth tables exist too
        assert!(project.backend.tables().contains(&"auth_user".to_string()));
        assert!(project.backend.tables().contains(&"django_session".to_string()));
    }

    #[test]
    fn model_save_through_project_backend() {
        let project = Project::builder()
            .backend(StdArc::new(InMemoryBackend::new()))
            .add_app(App::new("test").models::<Greeting>())
            .build()
            .unwrap();
        let g = Greeting { id: 0, text: "hello".into() };
        let saved = g.save(&*project.backend).unwrap();
        assert_eq!(saved.pk(), 1);
    }
}
