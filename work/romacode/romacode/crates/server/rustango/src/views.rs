//! Views — `django.views`: function helpers and class-based views.
//!
//! ```ignore
//! use std::sync::Arc;
//! use rustango::views::{ViewFn, TemplateView, ListView, DetailView};
//!
//! // function view
//! fn hello(req: &mut HttpRequest) -> Result<HttpResponse, HttpError> {
//!     Ok(HttpResponse::text("hello"))
//! }
//!
//! // class-based view wired into the router
//! let list = Arc::new(ListView::<Question>::new("polls/list.html"))
//!     .as_view();
//! ```

use std::collections::HashMap;
use std::marker::PhantomData;
use std::sync::Arc;

use crate::errors::HttpError;
use crate::http::{HttpRequest, HttpResponse, Method};
use crate::orm::{Backend, Model, ModelExt};
use crate::templates::{Context, TplValue};

/// The function view type: `fn(&mut HttpRequest) -> Result<HttpResponse, HttpError>`.
pub type ViewFn = Arc<dyn Fn(&mut HttpRequest) -> Result<HttpResponse, HttpError> + Send + Sync>;

/// Wrap a plain function into a `ViewFn`.
pub fn make_view<F>(f: F) -> ViewFn
where
    F: Fn(&mut HttpRequest) -> Result<HttpResponse, HttpError> + Send + Sync + 'static,
{
    Arc::new(f)
}

// ---------------------------------------------------------------------------
// render() helper
// ---------------------------------------------------------------------------

/// `django.shortcuts.render` — render a template with the request bound.
pub fn render(
    req: &mut HttpRequest,
    template_name: &str,
    context: Context,
) -> Result<HttpResponse, HttpError> {
    let engine = req
        .engine
        .clone()
        .ok_or_else(|| HttpError::ServerError("no template engine attached to request".into()))?;
    let mut ctx = context;
    // Context processors: request/user/messages are injected like Django's
    // auth.context_processors / request context processor.
    inject_globals(req, &mut ctx);
    let html = engine
        .render(template_name, &mut ctx)
        .map_err(|e| HttpError::ServerError(format!("template error: {e}")))?;
    Ok(HttpResponse::html(html))
}

/// Inject the standard context processors: `request`, `user`, `messages`.
pub fn inject_globals(req: &HttpRequest, ctx: &mut Context) {
    let mut request_map = HashMap::new();
    request_map.insert("path".to_string(), TplValue::Str(req.path.clone()));
    request_map.insert("full_path".to_string(), TplValue::Str(req.full_path.clone()));
    request_map.insert("method".to_string(), TplValue::Str(req.method.as_str().to_string()));
    ctx.set("request", TplValue::Map(Arc::new(request_map)));

    ctx.set(
        "user",
        TplValue::Map(Arc::new(HashMap::from([
            ("username".to_string(), TplValue::Str(req.user.username().to_string())),
            ("is_authenticated".to_string(), TplValue::Bool(req.user.is_authenticated())),
            ("is_staff".to_string(), TplValue::Bool(req.user.is_staff())),
            ("is_superuser".to_string(), TplValue::Bool(req.user.is_superuser())),
        ]))),
    );

    let msgs: Vec<TplValue> = req
        .messages
        .iter()
        .map(|m| {
            TplValue::Map(Arc::new(HashMap::from([
                ("message".to_string(), TplValue::Str(m.text.clone())),
                ("level_tag".to_string(), TplValue::Str(m.level.tag().to_string())),
            ])))
        })
        .collect();
    ctx.set("messages", TplValue::List(msgs));

    if let Some(token) = &req.csrf_token {
        ctx.set("csrf_token", TplValue::Str(token.clone()));
    }
}

/// `django.shortcuts.redirect` — 302 to a URL or named route.
pub fn redirect(req: &HttpRequest, to: &str) -> HttpResponse {
    let _ = req;
    HttpResponse::redirect(to)
}

/// `get_object_or_404` for a model by pk.
pub fn get_object_or_404<T: Model>(backend: &dyn Backend, pk: i64) -> Result<T, HttpError> {
    T::get(backend, pk).map_err(|_| HttpError::NotFound(format!("{} pk={}", T::table(), pk)))
}

// ---------------------------------------------------------------------------
// Class-based views
// ---------------------------------------------------------------------------

/// The `View` trait — dispatch by HTTP method, 405 for unsupported ones.
pub trait View: Send + Sync + 'static {
    fn get(&self, _req: &mut HttpRequest) -> Result<HttpResponse, HttpError> {
        Err(method_not_allowed("GET"))
    }
    fn post(&self, _req: &mut HttpRequest) -> Result<HttpResponse, HttpError> {
        Err(method_not_allowed("POST"))
    }
    fn put(&self, _req: &mut HttpRequest) -> Result<HttpResponse, HttpError> {
        Err(method_not_allowed("PUT"))
    }
    fn delete(&self, _req: &mut HttpRequest) -> Result<HttpResponse, HttpError> {
        Err(method_not_allowed("DELETE"))
    }
    /// Hooks between method dispatch and response (like `dispatch()`).
    fn dispatch(&self, req: &mut HttpRequest) -> Result<HttpResponse, HttpError> {
        match req.method {
            Method::GET | Method::HEAD => self.get(req),
            Method::POST => self.post(req),
            Method::PUT => self.put(req),
            Method::DELETE => self.delete(req),
            _ => Err(method_not_allowed(req.method.as_str())),
        }
    }

    /// Turn the view into a `ViewFn` — `Arc::new(view).as_view()`.
    fn as_view(self: Arc<Self>) -> ViewFn {
        Arc::new(move |req: &mut HttpRequest| self.dispatch(req))
    }
}

fn method_not_allowed(method: &str) -> HttpError {
    HttpError::MethodNotAllowed(format!("{method} not allowed on this view"))
}

/// Hook executed before the view body (like `get_queryset` overrides).
pub type ContextHook = Box<dyn Fn(&HttpRequest, &mut Context) + Send + Sync>;

/// `TemplateView` — render a named template with (optionally extended) context.
pub struct TemplateView {
    pub template_name: String,
    pub extra: Option<ContextHook>,
}

impl TemplateView {
    pub fn new(template_name: impl Into<String>) -> Self {
        Self { template_name: template_name.into(), extra: None }
    }

    pub fn with_extra<F>(mut self, f: F) -> Self
    where
        F: Fn(&HttpRequest, &mut Context) + Send + Sync + 'static,
    {
        self.extra = Some(Box::new(f));
        self
    }
}

impl View for TemplateView {
    fn get(&self, req: &mut HttpRequest) -> Result<HttpResponse, HttpError> {
        let mut ctx = Context::new();
        if let Some(hook) = &self.extra {
            hook(req, &mut ctx);
        }
        render(req, &self.template_name, ctx)
    }
}

/// `ListView<T>` — render a template with `object_list` from the ORM.
pub struct ListView<T: Model> {
    pub template_name: String,
    pub context_object_name: String,
    pub paginate_by: Option<usize>,
    pub ordering: Option<String>,
    _pd: PhantomData<T>,
}

impl<T: Model> ListView<T> {
    pub fn new(template_name: impl Into<String>) -> Self {
        Self {
            template_name: template_name.into(),
            context_object_name: "object_list".into(),
            paginate_by: None,
            ordering: None,
            _pd: PhantomData,
        }
    }

    pub fn context_object_name(mut self, name: impl Into<String>) -> Self {
        self.context_object_name = name.into();
        self
    }

    /// Items per page — enables `?page=N` pagination.
    pub fn paginate_by(mut self, n: usize) -> Self {
        self.paginate_by = Some(n);
        self
    }

    pub fn order_by(mut self, field: impl Into<String>) -> Self {
        self.ordering = Some(field.into());
        self
    }
}

impl<T: Model + 'static> View for ListView<T> {
    fn get(&self, req: &mut HttpRequest) -> Result<HttpResponse, HttpError> {
        let backend = req
            .backend
            .clone()
            .ok_or_else(|| HttpError::ServerError("no backend attached to request".into()))?;
        let mut qs = T::objects(&*backend);
        if let Some(ord) = &self.ordering {
            qs = qs.order_by(ord);
        }
        let mut ctx = Context::new();

        let total = qs.count().map_err(HttpError::from)?;
        match self.paginate_by {
            None => {
                let items: Vec<TplValue> = qs
                    .all()
                    .map_err(HttpError::from)?
                    .into_iter()
                    .map(|o| TplValue::Model(Arc::new(o)))
                    .collect();
                ctx.set(self.context_object_name.clone(), TplValue::List(items));
                ctx.set("paginator", TplValue::Null);
                ctx.set("page_obj", TplValue::Null);
            }
            Some(per_page) => {
                let page: usize = req
                    .query
                    .get("page")
                    .and_then(|p| p.parse().ok())
                    .unwrap_or(1)
                    .max(1);
                let pages = total.div_ceil(per_page).max(1);
                let page = page.min(pages);
                let items: Vec<TplValue> = qs
                    .skip((page - 1) * per_page)
                    .take(per_page)
                    .all()
                    .map_err(HttpError::from)?
                    .into_iter()
                    .map(|o| TplValue::Model(Arc::new(o)))
                    .collect();
                ctx.set(self.context_object_name.clone(), TplValue::List(items));
                let mut page_obj = HashMap::new();
                page_obj.insert("number".to_string(), TplValue::Int(page as i64));
                page_obj.insert("has_next".to_string(), TplValue::Bool(page < pages));
                page_obj.insert("has_previous".to_string(), TplValue::Bool(page > 1));
                page_obj.insert("has_other_pages".to_string(), TplValue::Bool(pages > 1));
                page_obj.insert("total_pages".to_string(), TplValue::Int(pages as i64));
                page_obj.insert("total_count".to_string(), TplValue::Int(total as i64));
                ctx.set("page_obj", TplValue::Map(Arc::new(page_obj)));
            }
        }
        render(req, &self.template_name, ctx)
    }
}

/// `DetailView<T>` — fetch one object by pk path param and render it.
pub struct DetailView<T: Model> {
    pub template_name: String,
    pub context_object_name: String,
    pub param_name: String,
    _pd: PhantomData<T>,
}

impl<T: Model> DetailView<T> {
    pub fn new(template_name: impl Into<String>) -> Self {
        Self {
            template_name: template_name.into(),
            context_object_name: "object".into(),
            param_name: "pk".into(),
            _pd: PhantomData,
        }
    }

    pub fn param_name(mut self, name: impl Into<String>) -> Self {
        self.param_name = name.into();
        self
    }
}

impl<T: Model + 'static> View for DetailView<T> {
    fn get(&self, req: &mut HttpRequest) -> Result<HttpResponse, HttpError> {
        let backend = req
            .backend
            .clone()
            .ok_or_else(|| HttpError::ServerError("no backend attached".into()))?;
        let pk = req.int_param(&self.param_name)?;
        let object = get_object_or_404::<T>(&*backend, pk)?;
        let mut ctx = Context::new();
        ctx.set(self.context_object_name.clone(), TplValue::Model(Arc::new(object)));
        render(req, &self.template_name, ctx)
    }
}

/// `RedirectView` — permanent or temporary redirect to a fixed target
/// (may contain `%s`, substituted with the first path param).
pub struct RedirectView {
    pub url: String,
    pub permanent: bool,
}

impl RedirectView {
    pub fn new(url: impl Into<String>) -> Self {
        Self { url: url.into(), permanent: false }
    }
    pub fn permanent(mut self) -> Self {
        self.permanent = true;
        self
    }
}

impl View for RedirectView {
    fn get(&self, req: &mut HttpRequest) -> Result<HttpResponse, HttpError> {
        let target = if self.url.contains("%s") {
            let arg = req.params.values().next().cloned().unwrap_or_default();
            self.url.replacen("%s", &arg, 1)
        } else {
            self.url.clone()
        };
        Ok(if self.permanent {
            HttpResponse::redirect_permanent(target)
        } else {
            HttpResponse::redirect(target)
        })
    }
}

/// Simple JSON API view helper: return any serializable value with status.
pub fn json_response<T: serde::Serialize>(value: &T, status: u16) -> HttpResponse {
    HttpResponse::json_status(status, &serde_json::to_value(value).unwrap_or_default())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::orm::InMemoryBackend;
    use std::sync::Arc as StdArc;

    #[derive(Clone, Debug, crate::Model)]
    #[model(table = "vw_note")]
    pub struct Note {
        #[field(auto)]
        pub id: i64,
        #[field(char, max = 80)]
        pub text: String,
    }

    fn base_req(path: &str) -> HttpRequest {
        let mut req = HttpRequest::new(Method::GET, path);
        let backend: StdArc<dyn Backend> = StdArc::new(InMemoryBackend::new());
        req.backend = Some(backend);
        req
    }

    fn seed(backend: &dyn crate::orm::Backend, n: usize) {
        backend.create_table(Note::table(), Note::columns().to_vec()).unwrap();
        for i in 0..n {
            let note = Note { id: 0, text: format!("note {i}") };
            note.save(backend).unwrap();
        }
    }

    #[test]
    fn redirect_view() {
        let v = StdArc::new(RedirectView::new("/next/")).as_view();
        let mut req = base_req("/old/");
        let res = v(&mut req).unwrap();
        assert_eq!(res.status, 302);
        assert_eq!(res.headers.get("Location"), Some("/next/"));

        let v2 = StdArc::new(RedirectView::new("/gone/").permanent()).as_view();
        let mut req2 = base_req("/old/");
        assert_eq!(v2(&mut req2).unwrap().status, 301);
    }

    #[test]
    fn method_not_allowed() {
        struct GetOnly;
        impl View for GetOnly {}
        let v = StdArc::new(GetOnly).as_view();
        let mut req = base_req("/");
        req.method = Method::POST;
        let err = v(&mut req).unwrap_err();
        assert_eq!(err.status(), 405);
    }

    #[test]
    fn list_view_pagination() {
        let mut req = base_req("/notes/?page=2");
        seed(req.backend.as_ref().unwrap().as_ref(), 15);
        let mut loader = crate::templates::InMemoryLoader::new();
        loader.add(
            "notes.html",
            "{% for o in object_list %}{{ o.text }};{% endfor %}p{{ page_obj.number }}/{{ page_obj.total_pages }}",
        );
        req.engine = Some(StdArc::new(crate::templates::Engine::new(
            vec![Box::new(loader)],
            true,
        )));

        let view = StdArc::new(
            ListView::<Note>::new("notes.html").paginate_by(10),
        )
        .as_view();
        let res = v_dispatch(&view, &mut req);
        let body = res.body_string();
        assert!(body.starts_with("note 10;"));
        assert!(body.ends_with("p2/2"));
    }

    fn v_dispatch(view: &ViewFn, req: &mut HttpRequest) -> HttpResponse {
        view(req).unwrap()
    }

    #[test]
    fn detail_view_404() {
        let mut req = base_req("/notes/99/");
        seed(req.backend.as_ref().unwrap().as_ref(), 2);
        req.params.insert("pk".into(), "99".into());
        let mut loader = crate::templates::InMemoryLoader::new();
        loader.add("note.html", "{{ object.text }}");
        req.engine = Some(StdArc::new(crate::templates::Engine::new(
            vec![Box::new(loader)],
            true,
        )));
        let view = StdArc::new(DetailView::<Note>::new("note.html")).as_view();
        let err = view(&mut req).unwrap_err();
        assert_eq!(err.status(), 404);

        // existing object
        req.params.insert("pk".into(), "1".into());
        let res = view(&mut req).unwrap();
        assert!(res.body_string().contains("note 0"));
    }

    #[test]
    fn template_view_with_extra() {
        let mut req = base_req("/about/");
        let mut loader = crate::templates::InMemoryLoader::new();
        loader.add("about.html", "site={{ site }} user={{ user.username }}");
        req.engine = Some(StdArc::new(crate::templates::Engine::new(
            vec![Box::new(loader)],
            true,
        )));
        let view = StdArc::new(
            TemplateView::new("about.html").with_extra(|_req, ctx| {
                ctx.set("site", TplValue::Str("rustango".into()));
            }),
        )
        .as_view();
        let res = view(&mut req).unwrap();
        assert_eq!(res.body_string(), "site=rustango user=");
    }

    #[test]
    fn inject_globals_context_processors() {
        let mut req = base_req("/x/");
        req.csrf_token = Some("tok".into());
        let mut ctx = Context::new();
        inject_globals(&req, &mut ctx);
        assert_eq!(
            ctx.resolve("request.path").unwrap().as_str(),
            "/x/"
        );
        assert_eq!(ctx.resolve("user.is_authenticated").unwrap().as_str(), "False");
        assert!(ctx.resolve("csrf_token").is_some());
    }

    #[test]
    fn get_object_or_404_helper() {
        let req = base_req("/");
        let backend = req.backend.as_ref().unwrap().clone();
        seed(backend.as_ref(), 1);
        let note = get_object_or_404::<Note>(&*backend, 1).unwrap();
        assert_eq!(note.text, "note 0");
        assert!(get_object_or_404::<Note>(&*backend, 7).is_err());
    }
}
