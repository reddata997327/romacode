//! Template engine — the `django.template` equivalent.
//!
//! Supports the Django Template Language core:
//!
//! * `{{ variable.path|filter:arg }}` — dotted lookup, filters
//! * `{% for x in items %}...{% empty %}...{% endfor %}` — with
//!   `forloop.counter`, `.counter0`, `.revcounter`, `.first`, `.last`
//! * `{% if %}/{% elif %}/{% else %}` with `== != < > <= >= in not in and or not`
//! * `{% block name %}` + `{% extends "base.html" %}` inheritance
//! * `{% include "x.html" %}`, `{% with a=1 b=2 %}`
//! * `{% csrf_token %}`, `{% url 'name' args %}`, `{% static 'path' %}`,
//!   `{% trans "key" %}`, `{% comment %}...{% endcomment %}`
//!
//! Auto-escaping is ON by default (values rendered from `Str`), exactly like
//! Django; use the `|safe` filter to opt out.

pub mod filters;
pub mod parse;

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;

use chrono::{DateTime, Utc};

pub use parse::{Node, Template, ValueExpr};

// ---------------------------------------------------------------------------
// Template values
// ---------------------------------------------------------------------------

/// A value flowing through templates. Models are wrapped so dotted access
/// resolves their fields lazily.
#[derive(Clone)]
pub enum TplValue {
    Null,
    Str(String),
    /// Pre-escaped / trusted content (`|safe`).
    Safe(String),
    Int(i64),
    Float(f64),
    Bool(bool),
    DateTime(DateTime<Utc>),
    List(Vec<TplValue>),
    Map(Arc<HashMap<String, TplValue>>),
    Model(Arc<dyn ModelLike>),
}

impl std::fmt::Debug for TplValue {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            TplValue::Null => write!(f, "Null"),
            TplValue::Str(s) => write!(f, "Str({s:?})"),
            TplValue::Safe(s) => write!(f, "Safe({s:?})"),
            TplValue::Int(i) => write!(f, "Int({i})"),
            TplValue::Float(x) => write!(f, "Float({x})"),
            TplValue::Bool(b) => write!(f, "Bool({b})"),
            TplValue::DateTime(d) => write!(f, "DateTime({d})"),
            TplValue::List(items) => write!(f, "List({items:?})"),
            TplValue::Map(m) => write!(f, "Map({m:?})"),
            TplValue::Model(_) => write!(f, "Model(..)"),
        }
    }
}

impl PartialEq for TplValue {
    fn eq(&self, other: &Self) -> bool {
        match (self, other) {
            (TplValue::Null, TplValue::Null) => true,
            (TplValue::Str(a), TplValue::Str(b)) => a == b,
            (TplValue::Safe(a), TplValue::Safe(b)) => a == b,
            (TplValue::Int(a), TplValue::Int(b)) => a == b,
            (TplValue::Float(a), TplValue::Float(b)) => a == b,
            (TplValue::Bool(a), TplValue::Bool(b)) => a == b,
            (TplValue::DateTime(a), TplValue::DateTime(b)) => a == b,
            (TplValue::List(a), TplValue::List(b)) => a == b,
            (TplValue::Map(a), TplValue::Map(b)) => Arc::ptr_eq(a, b) || **a == **b,
            (TplValue::Model(a), TplValue::Model(b)) => Arc::ptr_eq(a, b),
            _ => false,
        }
    }
}

/// Anything that can expose named fields to templates.
pub trait ModelLike: Send + Sync {
    fn tpl_field(&self, name: &str) -> Option<TplValue>;
}

impl<T: crate::orm::Model + 'static> ModelLike for T {
    fn tpl_field(&self, name: &str) -> Option<TplValue> {
        let cols = T::columns();
        let row = self.to_row();
        cols.iter()
            .zip(row)
            .find(|(c, _)| c.name == name)
            .map(|(_, v)| value_to_tpl(&v))
    }
}

/// Convert an ORM [`crate::orm::Value`] into a template value.
pub fn value_to_tpl(v: &crate::orm::Value) -> TplValue {
    match v {
        crate::orm::Value::Null => TplValue::Null,
        crate::orm::Value::Int(i) => TplValue::Int(*i),
        crate::orm::Value::Float(f) => TplValue::Float(*f),
        crate::orm::Value::Text(s) => TplValue::Str(s.clone()),
        crate::orm::Value::Bool(b) => TplValue::Bool(*b),
        crate::orm::Value::DateTime(d) => TplValue::DateTime(*d),
    }
}

impl TplValue {
    /// Django truthiness.
    pub fn is_truthy(&self) -> bool {
        match self {
            TplValue::Null => false,
            TplValue::Str(s) => !s.is_empty(),
            TplValue::Safe(s) => !s.is_empty(),
            TplValue::Int(i) => *i != 0,
            TplValue::Float(f) => *f != 0.0,
            TplValue::Bool(b) => *b,
            TplValue::DateTime(_) => true,
            TplValue::List(l) => !l.is_empty(),
            TplValue::Map(m) => !m.is_empty(),
            TplValue::Model(_) => true,
        }
    }

    pub fn as_str(&self) -> String {
        match self {
            TplValue::Null => String::new(),
            TplValue::Str(s) => s.clone(),
            TplValue::Safe(s) => s.clone(),
            TplValue::Int(i) => i.to_string(),
            TplValue::Float(f) => {
                if f.fract() == 0.0 {
                    format!("{f:.1}")
                } else {
                    f.to_string()
                }
            }
            TplValue::Bool(b) => (if *b { "True" } else { "False" }).to_string(),
            TplValue::DateTime(d) => d.format("%Y-%m-%d %H:%M:%S").to_string(),
            TplValue::List(items) => {
                let parts: Vec<String> = items.iter().map(|v| v.as_str()).collect();
                format!("[{}]", parts.join(", "))
            }
            TplValue::Map(_) => "{...}".to_string(),
            TplValue::Model(_) => "(model)".to_string(),
        }
    }

    pub fn as_int(&self) -> Option<i64> {
        match self {
            TplValue::Int(i) => Some(*i),
            TplValue::Float(f) => Some(*f as i64),
            TplValue::Str(s) => s.trim().parse().ok(),
            _ => None,
        }
    }
}

/// HTML escape (Django's `escape` filter).
pub fn escape_html(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for ch in s.chars() {
        match ch {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '"' => out.push_str("&quot;"),
            '\'' => out.push_str("&#39;"),
            _ => out.push(ch),
        }
    }
    out
}

/// From a JSON value (used for tests, forms and the AI contrib).
pub fn json_to_tpl(v: &serde_json::Value) -> TplValue {
    match v {
        serde_json::Value::Null => TplValue::Null,
        serde_json::Value::Bool(b) => TplValue::Bool(*b),
        serde_json::Value::Number(n) => {
            if let Some(i) = n.as_i64() {
                TplValue::Int(i)
            } else {
                TplValue::Float(n.as_f64().unwrap_or(0.0))
            }
        }
        serde_json::Value::String(s) => TplValue::Str(s.clone()),
        serde_json::Value::Array(items) => {
            TplValue::List(items.iter().map(json_to_tpl).collect())
        }
        serde_json::Value::Object(map) => {
            let mut m = HashMap::new();
            for (k, v) in map {
                m.insert(k.clone(), json_to_tpl(v));
            }
            TplValue::Map(Arc::new(m))
        }
    }
}

// ---------------------------------------------------------------------------
// Context
// ---------------------------------------------------------------------------

/// Rendering context — a stack of scopes (`{% with %}`, `{% for %}` push).
#[derive(Default, Clone)]
pub struct Context {
    stack: Vec<HashMap<String, TplValue>>,
}

impl Context {
    pub fn new() -> Context { Self::default() }

    pub fn from(map: HashMap<String, TplValue>) -> Context {
        Context { stack: vec![map] }
    }

    pub fn from_json(json: &serde_json::Value) -> Context {
        match json {
            serde_json::Value::Object(map) => {
                let mut m = HashMap::new();
                for (k, v) in map {
                    m.insert(k.clone(), json_to_tpl(v));
                }
                Context::from(m)
            }
            _ => Context::new(),
        }
    }

    pub fn set(&mut self, key: impl Into<String>, value: TplValue) {
        if self.stack.is_empty() {
            self.stack.push(HashMap::new());
        }
        self.stack.last_mut().unwrap().insert(key.into(), value);
    }

    pub fn get(&self, key: &str) -> Option<&TplValue> {
        for scope in self.stack.iter().rev() {
            if let Some(v) = scope.get(key) {
                return Some(v);
            }
        }
        None
    }

    pub fn push_scope(&mut self) {
        self.stack.push(HashMap::new());
    }

    pub fn pop_scope(&mut self) {
        self.stack.pop();
    }

    /// Dotted lookup across scopes: `a.b.c` / `items.0`.
    pub fn resolve(&self, path: &str) -> Option<TplValue> {
        let mut parts = path.split('.');
        let first = parts.next()?;
        let mut current = self.get(first)?.clone();
        for part in parts {
            current = match &current {
                TplValue::Map(m) => m.get(part).cloned()?,
                TplValue::List(items) => {
                    let idx: usize = part.parse().ok()?;
                    items.get(idx).cloned()?
                }
                TplValue::Model(m) => m.tpl_field(part)?,
                _ => return None,
            };
        }
        Some(current)
    }
}

// ---------------------------------------------------------------------------
// Loaders
// ---------------------------------------------------------------------------

/// Template source loader — the `django.template.loader` protocol.
pub trait TemplateLoader: Send + Sync {
    fn load(&self, name: &str) -> Option<String>;
}

/// Loads templates from a list of directories (DIRS + APP_DIRS behavior).
pub struct FileSystemLoader {
    pub dirs: Vec<PathBuf>,
}

impl FileSystemLoader {
    pub fn new(dirs: Vec<PathBuf>) -> Self { Self { dirs } }
}

impl TemplateLoader for FileSystemLoader {
    fn load(&self, name: &str) -> Option<String> {
        // Path-traversal guard (the documented loader contract): a template
        // name — possibly user-controlled via `{% include user_value %}` —
        // must stay inside the loader dirs. Reject absolute paths,
        // backslashes and any `..` path component outright, then verify the
        // canonicalized path still resolves under the target dir.
        let suspicious = name.is_empty()
            || name.starts_with('/')
            || name.starts_with('\\')
            || name.contains('\\')
            || name.split('/').any(|part| part == "..");
        if suspicious {
            return None;
        }
        for dir in &self.dirs {
            let path = dir.join(name);
            let (Ok(resolved), Ok(root)) = (path.canonicalize(), dir.canonicalize()) else {
                continue;
            };
            if !resolved.starts_with(&root) {
                continue;
            }
            if let Ok(text) = std::fs::read_to_string(&resolved) {
                return Some(text);
            }
        }
        None
    }
}

/// In-memory loader — used for embedded admin templates and tests.
#[derive(Default)]
pub struct InMemoryLoader {
    pub templates: HashMap<String, String>,
}

impl InMemoryLoader {
    pub fn new() -> Self { Self::default() }

    pub fn add(&mut self, name: impl Into<String>, source: impl Into<String>) {
        self.templates.insert(name.into(), source.into());
    }
}

impl TemplateLoader for InMemoryLoader {
    fn load(&self, name: &str) -> Option<String> {
        self.templates.get(name).cloned()
    }
}

// ---------------------------------------------------------------------------
// Engine
// ---------------------------------------------------------------------------

/// The template engine: loaders + compiled-template cache.
pub struct Engine {
    loaders: Vec<Box<dyn TemplateLoader>>,
    cache: std::sync::Mutex<HashMap<String, Arc<Template>>>,
    /// When false (production), templates are cached after first compile.
    pub debug: bool,
}

impl Default for Engine {
    fn default() -> Self {
        Self {
            loaders: vec![Box::new(InMemoryLoader::new())],
            cache: std::sync::Mutex::new(HashMap::new()),
            debug: true,
        }
    }
}

impl Engine {
    pub fn new(loaders: Vec<Box<dyn TemplateLoader>>, debug: bool) -> Engine {
        Engine { loaders, cache: std::sync::Mutex::new(HashMap::new()), debug }
    }

    pub fn add_loader(&mut self, loader: Box<dyn TemplateLoader>) {
        self.loaders.push(loader);
    }

    /// Fetch the source of a template (first loader that has it).
    pub fn source(&self, name: &str) -> Option<String> {
        for loader in &self.loaders {
            if let Some(src) = loader.load(name) {
                return Some(src);
            }
        }
        None
    }

    /// Compile (and cache) a template by name.
    pub fn compile(&self, name: &str) -> Result<Arc<Template>, String> {
        if !self.debug {
            if let Some(t) = self.cache.lock().unwrap().get(name) {
                return Ok(t.clone());
            }
        }
        let src = self
            .source(name)
            .ok_or_else(|| format!("template `{name}` not found"))?;
        let template = Template::parse(&src)
            .map_err(|e| format!("template `{name}`: {e}"))?;
        let arc = Arc::new(template);
        self.cache.lock().unwrap().insert(name.to_string(), arc.clone());
        Ok(arc)
    }

    /// Render a template by name with an initial context.
    pub fn render(&self, name: &str, ctx: &mut Context) -> Result<String, String> {
        let template = self.compile(name)?;
        template.render_with_engine(self, ctx)
    }

    /// Render a source string directly (no loader).
    pub fn render_source(&self, src: &str, ctx: &mut Context) -> Result<String, String> {
        let template =
            Template::parse(src).map_err(|e| format!("template: {e}"))?;
        template.render_with_engine(self, ctx)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn engine_with(srcs: &[(&str, &str)]) -> Engine {
        let mut loader = InMemoryLoader::new();
        for (n, s) in srcs {
            loader.add(*n, *s);
        }
        Engine::new(vec![Box::new(loader)], true)
    }

    #[test]
    fn variables_and_filters() {
        let e = engine_with(&[("t.html", "Hello {{ name|upper }}! {{ n }} {{ f }}")]);
        let mut ctx = Context::new();
        ctx.set("name", TplValue::Str("world".into()));
        ctx.set("n", TplValue::Int(42));
        ctx.set("f", TplValue::Float(2.5));
        let out = e.render("t.html", &mut ctx).unwrap();
        assert_eq!(out, "Hello WORLD! 42 2.5");
    }

    #[test]
    fn autoescape_and_safe() {
        let e = engine_with(&[("t.html", "{{ raw }}|{{ raw|safe }}")]);
        let mut ctx = Context::new();
        ctx.set("raw", TplValue::Str("<b>x</b>".into()));
        let out = e.render("t.html", &mut ctx).unwrap();
        assert_eq!(out, "&lt;b&gt;x&lt;/b&gt;|<b>x</b>");
    }

    #[test]
    fn dotted_and_index_lookup() {
        let e = engine_with(&[("t.html", "{{ user.name }} {{ items.1 }}")]);
        let mut user = HashMap::new();
        user.insert("name".to_string(), TplValue::Str("ada".into()));
        let mut ctx = Context::new();
        ctx.set("user", TplValue::Map(Arc::new(user)));
        ctx.set("items", TplValue::List(vec![
            TplValue::Int(10),
            TplValue::Int(20),
        ]));
        assert_eq!(e.render("t.html", &mut ctx).unwrap(), "ada 20");
    }

    #[test]
    fn missing_vars_render_empty() {
        let e = engine_with(&[("t.html", "[{{ nope }}][{{ nope.deep }}]")]);
        assert_eq!(e.render("t.html", &mut Context::new()).unwrap(), "[][]");
    }

    #[test]
    fn for_loop_with_empty() {
        let e = engine_with(&[(
            "t.html",
            "{% for x in items %}{{ forloop.counter }}:{{ x }};{% empty %}NONE{% endfor %}",
        )]);
        let mut ctx = Context::new();
        ctx.set("items", TplValue::List(vec![TplValue::Str("a".into()), TplValue::Int(2)]));
        let out = e.render("t.html", &mut ctx).unwrap();
        assert_eq!(out, "1:a;2:2;");

        ctx.set("items", TplValue::List(vec![]));
        assert_eq!(e.render("t.html", &mut ctx).unwrap(), "NONE");
    }

    #[test]
    fn forloop_metadata() {
        let e = engine_with(&[(
            "t.html",
            "{% for x in items %}{{ forloop.counter0 }}/{{ forloop.revcounter }}/{{ forloop.first }}{{ forloop.last }};{% endfor %}",
        )]);
        let mut ctx = Context::new();
        ctx.set("items", TplValue::List(vec![
            TplValue::Str("a".into()),
            TplValue::Str("b".into()),
            TplValue::Str("c".into()),
        ]));
        let out = e.render("t.html", &mut ctx).unwrap();
        assert_eq!(out, "0/3/TrueFalse;1/2/FalseFalse;2/1/FalseTrue;");
    }

    #[test]
    fn if_elif_else() {
        let e = engine_with(&[(
            "t.html",
            "{% if n > 10 %}big{% elif n > 5 %}mid{% else %}small{% endif %}",
        )]);
        let mut ctx = Context::new();
        ctx.set("n", TplValue::Int(20));
        assert_eq!(e.render("t.html", &mut ctx).unwrap(), "big");
        ctx.set("n", TplValue::Int(7));
        assert_eq!(e.render("t.html", &mut ctx).unwrap(), "mid");
        ctx.set("n", TplValue::Int(1));
        assert_eq!(e.render("t.html", &mut ctx).unwrap(), "small");
    }

    #[test]
    fn if_conditions() {
        let e = engine_with(&[(
            "t.html",
            "{% if a == 1 and b == 'x' %}1{% endif %}{% if flag %}f{% endif %}"
                .to_string()
                .as_str(),
        )]);
        let mut ctx = Context::new();
        ctx.set("a", TplValue::Int(1));
        ctx.set("b", TplValue::Str("x".into()));
        ctx.set("flag", TplValue::Bool(false));
        assert_eq!(e.render("t.html", &mut ctx).unwrap(), "1");
    }

    #[test]
    fn parenthesized_conditions() {
        // Regression: `(a == 1) and (b == 2)` used to have its inner parens
        // stripped as if they wrapped the whole condition, mangling the
        // token stream and failing at render time.
        let e = engine_with(&[(
            "t.html",
            "{% if (a == 1) and (b == 2) %}both{% else %}nope{% endif %}",
        )]);
        let mut ctx = Context::new();
        ctx.set("a", TplValue::Int(1));
        ctx.set("b", TplValue::Int(2));
        assert_eq!(e.render("t.html", &mut ctx).unwrap(), "both");
        ctx.set("b", TplValue::Int(3));
        assert_eq!(e.render("t.html", &mut ctx).unwrap(), "nope");

        // A truly wrapping paren pair is still stripped.
        let e2 = engine_with(&[("w.html", "{% if (a == 1) %}yes{% endif %}")]);
        assert_eq!(e2.render("w.html", &mut ctx).unwrap(), "yes");
    }

    #[test]
    fn in_operator() {
        let e = engine_with(&[(
            "t.html",
            "{% if 'rust' in tags %}yes{% endif %}{% if 'go' not in tags %}-no-go{% endif %}",
        )]);
        let mut ctx = Context::new();
        ctx.set("tags", TplValue::List(vec![
            TplValue::Str("rust".into()),
            TplValue::Str("django".into()),
        ]));
        assert_eq!(e.render("t.html", &mut ctx).unwrap(), "yes-no-go");
    }

    #[test]
    fn extends_and_blocks() {
        let e = engine_with(&[
            ("base.html", "<title>{% block title %}Default{% endblock %}</title><body>{% block body %}empty{% endblock %}</body>"),
            ("child.html", "{% extends \"base.html\" %}{% block title %}My Page{% endblock %}{% block body %}Hello{% endblock %}"),
        ]);
        let out = e.render("child.html", &mut Context::new()).unwrap();
        assert_eq!(out, "<title>My Page</title><body>Hello</body>");
    }

    #[test]
    fn include() {
        let e = engine_with(&[
            ("part.html", "[{{ x }}]"),
            ("t.html", "A{% include \"part.html\" %}B"),
        ]);
        let mut ctx = Context::new();
        ctx.set("x", TplValue::Int(7));
        assert_eq!(e.render("t.html", &mut ctx).unwrap(), "A[7]B");
    }

    #[test]
    fn filesystem_loader_rejects_path_traversal() {
        // Regression: `{% include user_value %}` with `../../../../etc/passwd`
        // used to read arbitrary files into the rendered page.
        let base = std::env::temp_dir().join(format!(
            "rustango_tpl_{}_{:?}",
            std::process::id(),
            std::thread::current().id()
        ));
        std::fs::create_dir_all(&base).unwrap();
        std::fs::write(base.join("ok.html"), "OK").unwrap();
        let loader = FileSystemLoader::new(vec![base.clone()]);
        // Normal lookup still works.
        assert_eq!(loader.load("ok.html"), Some("OK".into()));
        // Traversal shapes are rejected before any filesystem access.
        assert_eq!(loader.load("../evil.html"), None);
        assert_eq!(loader.load("a/../../evil.html"), None);
        assert_eq!(loader.load("/etc/passwd"), None);
        assert_eq!(loader.load("..\\evil.html"), None);
        assert_eq!(loader.load(""), None);
        std::fs::remove_dir_all(&base).ok();
    }

    #[test]
    fn with_tag() {
        let e = engine_with(&[(
            "t.html",
            "{% with greeting = 'Hi' %}{{ greeting }}{% endwith %}",
        )]);
        assert_eq!(e.render("t.html", &mut Context::new()).unwrap(), "Hi");
    }

    #[test]
    fn comment_tag() {
        let e = engine_with(&[("t.html", "a{% comment %}hidden{% endcomment %}b")]);
        assert_eq!(e.render("t.html", &mut Context::new()).unwrap(), "ab");
    }

    #[test]
    fn more_filters() {
        let e = engine_with(&[(
            "t.html",
            "{{ s|truncatechars:6 }}|{{ list|length }}|{{ list|join:', ' }}|{{ missing|default:'dflt' }}|{{ n|add:3 }}|{{ b|yesno:'ja,nein' }}",
        )]);
        let mut ctx = Context::new();
        ctx.set("s", TplValue::Str("abcdefgh".into()));
        ctx.set("list", TplValue::List(vec![
            TplValue::Str("a".into()),
            TplValue::Str("b".into()),
        ]));
        ctx.set("n", TplValue::Int(4));
        ctx.set("b", TplValue::Bool(true));
        let out = e.render("t.html", &mut ctx).unwrap();
        assert_eq!(out, "abc...|2|a, b|dflt|7|ja");
    }

    #[test]
    fn url_tag() {
        let _lock = crate::urls::registry_test_lock();
        let e = engine_with(&[("t.html", "{% url 'about' %} {% url 'post' '2026' 'hi' %}")]);
        let view: crate::views::ViewFn = std::sync::Arc::new(|_r| Ok(crate::http::HttpResponse::text("")));
        // Keep this router in sync with urls::tests::reverse_flow — the
        // registry is process-global (whichever test runs first wins).
        let router = crate::urls::Router::new(vec![
            crate::urls::named_path("about/", view.clone(), "about"),
            crate::urls::named_path("blog/<int:year>/<slug:slug>/", view.clone(), "post"),
            crate::urls::named_path("search/", view.clone(), "search"),
        ]);
        crate::urls::set_route_registry_for_tests(router);
        assert_eq!(e.render("t.html", &mut Context::new()).unwrap(), "/about/ /blog/2026/hi/");
    }

    #[test]
    fn url_tag_escapes_reversed_url() {
        let _lock = crate::urls::registry_test_lock();
        // Regression: reversed URLs were pushed into the output raw while
        // every `{{ var }}` is auto-escaped — a `<str:slug>` value like
        // `x"><img src=x onerror=alert(1)>` broke out of the href attribute.
        // (The payload must not contain `/`: the Str converter rejects
        // slashes, but attribute breakout needs no slash at all.)
        let e = engine_with(&[("t.html", "<a href=\"{% url 'esc_post' slug %}\">x</a>")]);
        let view: crate::views::ViewFn = std::sync::Arc::new(|_r| Ok(crate::http::HttpResponse::text("")));
        let router = crate::urls::Router::new(vec![
            crate::urls::named_path("p/<str:slug>/", view.clone(), "esc_post"),
        ]);
        crate::urls::set_route_registry_for_tests(router);
        let mut ctx = Context::new();
        ctx.set("slug", TplValue::Str("x\"><img src=x onerror=alert(1)>".into()));
        let out = e.render("t.html", &mut ctx).unwrap();
        assert!(!out.contains("<img"), "unescaped url in output: {out}");
        assert!(out.contains("&quot;&gt;&lt;img"), "escaped url expected, got: {out}");
    }

    #[test]
    fn csrf_token_tag() {
        let e = engine_with(&[("t.html", "{% csrf_token %}")]);
        let mut ctx = Context::new();
        ctx.set("csrf_token", TplValue::Str("tok123".into()));
        let out = e.render("t.html", &mut ctx).unwrap();
        assert!(out.contains("name=\"csrfmiddlewaretoken\""));
        assert!(out.contains("value=\"tok123\""));
    }

    #[test]
    fn date_filter() {
        let e = engine_with(&[("t.html", "{{ d|date:'Y-m-d' }}")]);
        let mut ctx = Context::new();
        ctx.set("d", TplValue::DateTime(Utc::now()));
        let out = e.render("t.html", &mut ctx).unwrap();
        // year-month-day shape
        assert!(out.matches('-').count() == 2);
        assert!(out.starts_with('2'));
    }

    #[test]
    fn model_in_template() {
        use crate::orm::InMemoryBackend;
        #[derive(Clone, Debug, crate::Model)]
        #[model(table = "tpl_model")]
        pub struct M {
            #[field(auto)]
            pub id: i64,
            #[field(char, max = 20)]
            pub name: String,
        }
        use crate::orm::{Backend as _, Model as _};
        let backend = InMemoryBackend::new();
        backend.create_table(M::table(), M::columns().to_vec()).unwrap();
        use crate::orm::ModelExt as _;
        let m = M { id: 0, name: "widget".into() };
        let saved = m.save(&backend).unwrap();

        let e = engine_with(&[("t.html", "{{ obj.name }} #{{ obj.id }}")]);
        let mut ctx = Context::new();
        ctx.set("obj", TplValue::Model(Arc::new(saved)));
        assert_eq!(e.render("t.html", &mut ctx).unwrap(), "widget #1");
    }

    #[test]
    fn errors_are_reported() {
        let e = engine_with(&[("bad.html", "{% for %}")]);
        assert!(e.render("bad.html", &mut Context::new()).is_err());
        let e2 = engine_with(&[("missing2.html", "x")]);
        assert!(e2.render("does-not-exist.html", &mut Context::new()).is_err());
    }
}
