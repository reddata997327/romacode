//! Admin — `django.contrib.admin`: automatic CRUD for registered models.
//!
//! ```ignore
//! use rustango::admin::{AdminSite, ModelAdmin};
//!
//! let project = Project::builder()
//!     // ...
//!     .admin(|site| {
//!         site.register::<Question>(ModelAdmin {
//!             list_display: vec!["id".into(), "question_text".into()],
//!             search_fields: vec!["question_text".into()],
//!             ..Default::default()
//!         });
//!     })
//!     .build();
//! ```
//!
//! Mounted at `settings.admin_url` (default `admin/`) with namespace
//! `admin:` — routes `admin:index`, `admin:login`, `admin:logout`,
//! `admin:list`, `admin:add`, `admin:change`, `admin:delete`.

use std::collections::HashMap;
use std::sync::Arc;

use crate::errors::HttpError;
use crate::forms::{Field as FormField, Form};
use crate::http::{HttpRequest, HttpResponse};
use crate::orm::{Backend, Criteria, FieldKind, Model, Value};
use crate::templates::{Context, TplValue};
use crate::urls::{named_path, Router};
use crate::views::{render, ViewFn};

/// Per-model admin options — `ModelAdmin`.
#[derive(Clone, Default)]
pub struct ModelAdmin {
    /// Columns shown on the changelist (defaults to first 4 fields).
    pub list_display: Vec<String>,
    /// Char/text columns searchable via `?q=`.
    pub search_fields: Vec<String>,
    /// Default ordering, `-` prefix for desc (default `-id`).
    pub ordering: String,
    pub list_per_page: usize,
}

/// One registered model inside the admin site.
#[derive(Clone)]
pub struct AdminEntry {
    pub table: String,
    pub meta: crate::orm::ModelMeta,
    pub admin: ModelAdmin,
}

/// The admin site.
pub struct AdminSite {
    pub site_title: String,
    pub site_header: String,
    pub entries: Vec<AdminEntry>,
    /// URL prefix the site is mounted under (no slashes), e.g. `admin`.
    /// Defaults to `admin`; `ProjectBuilder::build` syncs it with
    /// `settings.admin_url`.
    pub url_prefix: String,
}

impl AdminSite {
    pub fn new(site_title: impl Into<String>, site_header: impl Into<String>) -> AdminSite {
        AdminSite {
            site_title: site_title.into(),
            site_header: site_header.into(),
            entries: Vec::new(),
            url_prefix: "admin".into(),
        }
    }

    /// Absolute URL inside the admin site (`"login/"` -> `/admin/login/`).
    fn url(&self, path: &str) -> String {
        if self.url_prefix.is_empty() {
            format!("/{path}")
        } else {
            format!("/{}/{}", self.url_prefix, path)
        }
    }

    /// `admin.site.register(Question, QuestionAdmin)`.
    pub fn register<T: Model>(&mut self, admin: ModelAdmin) {
        let meta = crate::orm::ModelMeta::of::<T>();
        let table = meta.table.clone();
        self.entries.push(AdminEntry { table, meta, admin });
    }

    fn entry(&self, table: &str) -> Option<&AdminEntry> {
        self.entries.iter().find(|e| e.table == table)
    }

    /// Build the admin Router (mounted under settings.admin_url).
    pub fn urls(&self) -> Router {
        let site = Arc::new(AdminSite {
            site_title: self.site_title.clone(),
            site_header: self.site_header.clone(),
            entries: self.entries.clone(),
            url_prefix: self.url_prefix.clone(),
        });

        let index: ViewFn = {
            let site = site.clone();
            Arc::new(move |req: &mut HttpRequest| site.view_index(req))
        };
        let login: ViewFn = {
            let site = site.clone();
            Arc::new(move |req: &mut HttpRequest| site.view_login(req))
        };
        let logout: ViewFn = {
            let site = site.clone();
            Arc::new(move |req: &mut HttpRequest| site.view_logout(req))
        };
        let list: ViewFn = {
            let site = site.clone();
            Arc::new(move |req: &mut HttpRequest| site.view_list(req))
        };
        let add: ViewFn = {
            let site = site.clone();
            Arc::new(move |req: &mut HttpRequest| site.view_add_or_change(req, None))
        };
        let change: ViewFn = {
            let site = site.clone();
            Arc::new(move |req: &mut HttpRequest| {
                let pk = req.int_param("pk")?;
                site.view_add_or_change(req, Some(pk))
            })
        };
        let delete: ViewFn = {
            let site = site.clone();
            Arc::new(move |req: &mut HttpRequest| {
                let pk = req.int_param("pk")?;
                site.view_delete(req, pk)
            })
        };

        Router::new(vec![
            named_path("", index, "index"),
            named_path("login/", login, "login"),
            named_path("logout/", logout, "logout"),
            named_path("<str:table>/", list, "list"),
            named_path("<str:table>/add/", add, "add"),
            named_path("<str:table>/<int:pk>/change/", change, "change"),
            named_path("<str:table>/<int:pk>/delete/", delete, "delete"),
        ])
    }

    // -- shared context ------------------------------------------------------

    fn base_ctx(&self, req: &HttpRequest) -> Context {
        let mut ctx = Context::new();
        ctx.set("site_title", TplValue::Str(self.site_title.clone()));
        ctx.set("site_header", TplValue::Str(self.site_header.clone()));
        // Templates build admin links from this (no hardcoded `/admin/`).
        let prefix = if self.url_prefix.is_empty() {
            String::new()
        } else {
            format!("/{}", self.url_prefix)
        };
        ctx.set("admin_prefix", TplValue::Str(prefix));
        let mut user = HashMap::new();
        user.insert("username".to_string(), TplValue::Str(req.user.username().to_string()));
        user.insert("is_authenticated".to_string(), TplValue::Bool(req.user.is_authenticated()));
        user.insert("is_staff".to_string(), TplValue::Bool(req.user.is_staff()));
        ctx.set("user", TplValue::Map(Arc::new(user)));
        ctx
    }

    fn require_staff(&self, req: &HttpRequest) -> Result<(), HttpResponse> {
        if !req.user.is_authenticated() || (!req.user.is_staff() && !req.user.is_superuser()) {
            return Err(HttpResponse::redirect(self.url("login/")));
        }
        Ok(())
    }

    // -- views -----------------------------------------------------------------

    fn view_login(&self, req: &mut HttpRequest) -> Result<HttpResponse, HttpError> {
        if req.method == crate::http::Method::POST {
            let username = req.post.get("username").unwrap_or("").to_string();
            let password = req.post.get("password").unwrap_or("").to_string();
            let backend = req
                .backend
                .clone()
                .ok_or_else(|| HttpError::ServerError("no backend".into()))?;
            let user = crate::auth::authenticate(&*backend, &username, &password);
            return match user {
                Some(u) if u.is_staff || u.is_superuser => {
                    crate::auth::login(req, &u);
                    Ok(HttpResponse::redirect(self.url("")))
                }
                _ => {
                    let mut ctx = self.base_ctx(req);
                    ctx.set(
                        "error",
                        TplValue::Str("Invalid credentials or not a staff user.".into()),
                    );
                    render(req, "admin/login.html", ctx)
                }
            };
        }
        if req.user.is_staff() || req.user.is_superuser() {
            return Ok(HttpResponse::redirect(self.url("")));
        }
        let ctx = self.base_ctx(req);
        render(req, "admin/login.html", ctx)
    }

    fn view_logout(&self, req: &mut HttpRequest) -> Result<HttpResponse, HttpError> {
        crate::auth::logout(req);
        Ok(HttpResponse::redirect(self.url("login/")))
    }

    fn view_index(&self, req: &mut HttpRequest) -> Result<HttpResponse, HttpError> {
        if let Err(res) = self.require_staff(req) {
            return Ok(res);
        }
        let backend = req
            .backend
            .clone()
            .ok_or_else(|| HttpError::ServerError("no backend".into()))?;
        let mut models = Vec::new();
        for entry in &self.entries {
            let count = backend
                .count(&entry.table, &Criteria::any())
                .unwrap_or(0);
            models.push(TplValue::Map(Arc::new(HashMap::from([
                ("name".to_string(), TplValue::Str(entry.table.clone())),
                ("count".to_string(), TplValue::Int(count as i64)),
            ]))));
        }
        let mut ctx = self.base_ctx(req);
        ctx.set("models", TplValue::List(models));
        render(req, "admin/index.html", ctx)
    }

    fn display_columns(entry: &AdminEntry) -> Vec<String> {
        if entry.admin.list_display.is_empty() {
            entry
                .meta
                .columns
                .iter()
                .take(4)
                .map(|c| c.name.clone())
                .collect()
        } else {
            entry.admin.list_display.clone()
        }
    }

    fn view_list(&self, req: &mut HttpRequest) -> Result<HttpResponse, HttpError> {
        if let Err(res) = self.require_staff(req) {
            return Ok(res);
        }
        let table = req.param("table").unwrap_or("").to_string();
        let entry = self
            .entry(&table)
            .ok_or_else(|| HttpError::NotFound(format!("model `{table}` not registered")))?
            .clone();
        let backend = req
            .backend
            .clone()
            .ok_or_else(|| HttpError::ServerError("no backend".into()))?;

        // search: OR of icontains over searchable text columns
        let mut crit = Criteria::any();
        let q = req.query.get("q").unwrap_or("").to_string();
        if !q.is_empty() {
            let mut branches = Vec::new();
            for field in &entry.admin.search_fields {
                if let Some(col) = entry.meta.column(field) {
                    if matches!(col.kind, FieldKind::Char | FieldKind::Text) {
                        branches.push(Criteria::icontains(field.clone(), q.clone()));
                    }
                }
            }
            if !branches.is_empty() {
                crit = Criteria::Or(branches);
            }
        }

        let ordering = if entry.admin.ordering.is_empty() {
            "-id".to_string()
        } else {
            entry.admin.ordering.clone()
        };
        let desc = ordering.starts_with('-');
        let order_field = ordering.trim_start_matches('-').to_string();
        let order_spec = vec![(order_field, desc)];

        let per_page = if entry.admin.list_per_page == 0 { 25 } else { entry.admin.list_per_page };
        let total = backend.count(&entry.table, &crit).map_err(HttpError::from)?;
        let pages = total.div_ceil(per_page).max(1);
        let page: usize = req
            .query
            .get("page")
            .and_then(|p| p.parse().ok())
            .unwrap_or(1)
            .max(1)
            .min(pages);

        let cols = entry.meta.columns.clone();
        let mut rows = backend
            .select(&entry.table, &crit, &order_spec, Some(per_page), Some((page - 1) * per_page))
            .map_err(HttpError::from)?;

        let display = Self::display_columns(&entry);
        let objects: Vec<TplValue> = rows
            .drain(..)
            .map(|row| {
                let mut m = HashMap::new();
                for (c, v) in cols.iter().zip(&row) {
                    if display.contains(&c.name) || c.name == "id" {
                        m.insert(c.name.clone(), TplValue::Str(v.display()));
                    }
                }
                m.insert("pk".to_string(), TplValue::Str(display_pk(&cols, &row)));
                TplValue::Map(Arc::new(m))
            })
            .collect();

        let mut ctx = self.base_ctx(req);
        ctx.set("model_name", TplValue::Str(table.clone()));
        ctx.set("columns", TplValue::List(
            display.iter().map(|c| TplValue::Str(c.clone())).collect(),
        ));
        ctx.set("objects", TplValue::List(objects));
        ctx.set("total", TplValue::Int(total as i64));
        ctx.set("page", TplValue::Int(page as i64));
        ctx.set("pages", TplValue::Int(pages as i64));
        ctx.set("q", TplValue::Str(q));
        render(req, "admin/change_list.html", ctx)
    }

    /// Build the auto-form from model columns; when `initial` values are
    /// provided (change view GET), they are bound as display values.
    fn build_form(
        entry: &AdminEntry,
        initial: Option<&HashMap<String, String>>,
        posted: Option<&crate::http::QueryDict>,
    ) -> Form {
        let mut fields: Vec<Box<dyn FormField>> = Vec::new();
        for col in &entry.meta.columns {
            if col.primary_key || col.kind == FieldKind::Auto {
                continue;
            }
            if col.kind == FieldKind::DateTimeAutoNowAdd {
                continue; // backend fills it
            }
            let optional = col.null || col.default.is_some();
            let f: Box<dyn FormField> = match &col.kind {
                FieldKind::Char => {
                    let mut cf = crate::forms::CharField::new(col.name.clone())
                        .with_label(col.name.clone());
                    if col.max_length > 0 {
                        cf = cf.max_length(col.max_length);
                    }
                    if !optional {
                        cf = cf.with_required(true);
                    } else {
                        cf = cf.optional();
                    }
                    Box::new(cf)
                }
                FieldKind::Text => {
                    let mut tf = crate::forms::TextField(col.name.clone());
                    tf = tf.with_label(col.name.clone());
                    if !optional {
                        tf = tf.with_required(true);
                    } else {
                        tf = tf.optional();
                    }
                    Box::new(tf)
                }
                FieldKind::Integer | FieldKind::ForeignKey { .. } => {
                    let mut inf = crate::forms::IntegerField::new(col.name.clone())
                        .label(col.name.clone());
                    if optional {
                        inf = inf.optional();
                    }
                    Box::new(inf)
                }
                FieldKind::Float => {
                    let mut ff = crate::forms::FloatField::new(col.name.clone())
                        .label(col.name.clone());
                    if optional {
                        ff = ff.optional();
                    }
                    Box::new(ff)
                }
                FieldKind::Bool => Box::new(crate::forms::BooleanField::new(col.name.clone())),
                FieldKind::DateTime => {
                    Box::new(AdminDateTimeField::new(col.name.clone(), optional))
                }
                _ => Box::new(crate::forms::HiddenField::new(col.name.clone())),
            };
            fields.push(f);
        }
        let form = Form::new(format!("admin-{}", entry.table), fields);
        match (posted, initial) {
            (Some(d), _) => form.bind(d),
            (None, Some(init)) => {
                let mut data = crate::http::QueryDict::new();
                for (k, v) in init {
                    data.set(k.clone(), v.clone());
                }
                form.bind(&data)
            }
            _ => form,
        }
    }

    fn row_initial_values(cols: &[crate::orm::ColumnDesc], row: &[Value]) -> HashMap<String, String> {
        let mut out = HashMap::new();
        for (c, v) in cols.iter().zip(row) {
            if c.primary_key || c.kind == FieldKind::Auto || c.kind == FieldKind::DateTimeAutoNowAdd {
                continue;
            }
            out.insert(c.name.clone(), v.display());
        }
        out
    }

    fn view_add_or_change(
        &self,
        req: &mut HttpRequest,
        pk: Option<i64>,
    ) -> Result<HttpResponse, HttpError> {
        if let Err(res) = self.require_staff(req) {
            return Ok(res);
        }
        let table = req.param("table").unwrap_or("").to_string();
        let entry = self
            .entry(&table)
            .ok_or_else(|| HttpError::NotFound(format!("model `{table}` not registered")))?
            .clone();
        let backend = req
            .backend
            .clone()
            .ok_or_else(|| HttpError::ServerError("no backend".into()))?;

        let is_post = req.method == crate::http::Method::POST;

        // Existing row (for change view prefill).
        let existing: Option<(Vec<Value>, HashMap<String, String>)> = match pk {
            Some(pk) => {
                let crit = Criteria::eq("id", Value::Int(pk));
                let rows = backend
                    .select(&entry.table, &crit, &[], None, None)
                    .map_err(HttpError::from)?;
                rows.into_iter().next().map(|row| {
                    let init = Self::row_initial_values(&entry.meta.columns, &row);
                    (row, init)
                })
            }
            None => None,
        };

        if pk.is_some() && existing.is_none() {
            return Err(HttpError::NotFound(format!(
                "{} pk={}",
                entry.table,
                pk.unwrap_or(0)
            )));
        }

        let mut form = Self::build_form(
            &entry,
            if is_post { None } else { existing.as_ref().map(|(_, init)| init) },
            if is_post { Some(&req.post) } else { None },
        );

        if is_post && form.is_valid() {
            let mut values: HashMap<String, Value> = HashMap::new();
            for col in &entry.meta.columns {
                if col.primary_key || col.kind == FieldKind::Auto || col.kind == FieldKind::DateTimeAutoNowAdd {
                    continue;
                }
                let v = form.cleaned(&col.name).cloned().unwrap_or(Value::Null);
                values.insert(col.name.clone(), v);
            }
            match pk {
                Some(pk) => {
                    update_row(&*backend, &entry.meta.columns, &entry.table, pk, &values)
                        .map_err(HttpError::from)?;
                    crate::messages::success(req, "Changed successfully.");
                    return Ok(HttpResponse::redirect(self.url(&format!("{table}/"))));
                }
                None => {
                    let new_pk =
                        insert_row(&*backend, &entry.table, &entry.meta.columns, &values)
                            .map_err(HttpError::from)?;
                    crate::messages::success(req, "Added successfully.");
                    return Ok(HttpResponse::redirect(
                        self.url(&format!("{table}/{new_pk}/change/")),
                    ));
                }
            }
        }

        let mut ctx = self.base_ctx(req);
        ctx.set("model_name", TplValue::Str(table.clone()));
        ctx.set("is_add", TplValue::Bool(pk.is_none()));
        if let Some(pk) = pk {
            ctx.set("pk", TplValue::Int(pk));
        }
        ctx.set("form_html", TplValue::Safe(form.as_table()));
        render(req, "admin/change_form.html", ctx)
    }

    fn view_delete(&self, req: &mut HttpRequest, pk: i64) -> Result<HttpResponse, HttpError> {
        if let Err(res) = self.require_staff(req) {
            return Ok(res);
        }
        let table = req.param("table").unwrap_or("").to_string();
        let entry = self
            .entry(&table)
            .ok_or_else(|| HttpError::NotFound(format!("model `{table}` not registered")))?;
        let backend = req
            .backend
            .clone()
            .ok_or_else(|| HttpError::ServerError("no backend".into()))?;

        if req.method == crate::http::Method::POST {
            let crit = Criteria::eq("id", Value::Int(pk));
            let n = backend.delete(&entry.table, &crit).map_err(HttpError::from)?;
            crate::messages::success(req, format!("Deleted {n} item(s)."));
            return Ok(HttpResponse::redirect(self.url(&format!("{table}/"))));
        }

        let crit = Criteria::eq("id", Value::Int(pk));
        let rows = backend
            .select(&entry.table, &crit, &[], None, None)
            .map_err(HttpError::from)?;
        let row = rows
            .into_iter()
            .next()
            .ok_or_else(|| HttpError::NotFound(format!("{} pk={}", entry.table, pk)))?;
        let mut pairs: Vec<TplValue> = Vec::new();
        for (c, v) in entry.meta.columns.iter().zip(&row) {
            pairs.push(TplValue::List(vec![
                TplValue::Str(c.name.clone()),
                TplValue::Str(v.display()),
            ]));
        }
        let mut ctx = self.base_ctx(req);
        ctx.set("model_name", TplValue::Str(table));
        ctx.set("pk", TplValue::Int(pk));
        ctx.set("object_pairs", TplValue::List(pairs));
        render(req, "admin/delete_confirmation.html", ctx)
    }
}

fn display_pk(cols: &[crate::orm::ColumnDesc], row: &[Value]) -> String {
    cols.first()
        .zip(row.first())
        .map(|(_, v)| v.display())
        .unwrap_or_default()
}

fn insert_row(
    backend: &dyn Backend,
    table: &str,
    cols: &[crate::orm::ColumnDesc],
    values: &HashMap<String, Value>,
) -> crate::orm::OrmResult<i64> {
    let row: Vec<Value> = cols
        .iter()
        .map(|c| {
            if c.primary_key {
                Value::Null
            } else {
                values.get(&c.name).cloned().unwrap_or(Value::Null)
            }
        })
        .collect();
    backend.insert(table, row)
}

fn update_row(
    backend: &dyn Backend,
    cols: &[crate::orm::ColumnDesc],
    table: &str,
    pk: i64,
    values: &HashMap<String, Value>,
) -> crate::orm::OrmResult<usize> {
    let mut updates = Vec::new();
    for (i, c) in cols.iter().enumerate() {
        if c.primary_key {
            continue;
        }
        if let Some(v) = values.get(&c.name) {
            updates.push((i, v.clone()));
        }
    }
    let crit = Criteria::eq("id", Value::Int(pk));
    backend.update(table, &crit, &updates)
}

/// DateTime form field: accepts `YYYY-MM-DD HH:MM[:SS]` or ISO-8601.
pub struct AdminDateTimeField {
    name: String,
    optional: bool,
}

impl AdminDateTimeField {
    pub fn new(name: impl Into<String>, optional: bool) -> Self {
        Self { name: name.into(), optional }
    }
}

impl FormField for AdminDateTimeField {
    fn name(&self) -> &str {
        &self.name
    }
    fn label(&self) -> String {
        self.name.clone()
    }
    fn required(&self) -> bool {
        !self.optional
    }

    fn clean(&self, raw: Option<&str>) -> Result<Value, crate::forms::FieldErrors> {
        let raw = raw.map(|s| s.trim()).unwrap_or("");
        if raw.is_empty() {
            if self.required() {
                return Err(vec!["This field is required.".to_string()]);
            }
            return Ok(Value::Null);
        }
        for fmt in [
            "%Y-%m-%d %H:%M:%S",
            "%Y-%m-%d %H:%M",
            "%Y-%m-%dT%H:%M:%S",
            "%Y-%m-%dT%H:%M",
        ] {
            if let Ok(dt) = chrono::NaiveDateTime::parse_from_str(raw, fmt) {
                return Ok(Value::DateTime(dt.and_utc()));
            }
        }
        if let Ok(d) = chrono::NaiveDate::parse_from_str(raw, "%Y-%m-%d") {
            if let Some(dt) = d.and_hms_opt(0, 0, 0) {
                return Ok(Value::DateTime(dt.and_utc()));
            }
        }
        Err(vec!["Enter a valid date/time.".to_string()])
    }

    fn render(&self, bound: Option<&str>, _error: Option<&str>) -> String {
        let initial = bound.unwrap_or("");
        format!(
            "<input type=\"text\" name=\"{}\" id=\"id_{}\" value=\"{}\" class=\"form-control\" placeholder=\"YYYY-MM-DD HH:MM\">",
            self.name,
            self.name,
            crate::templates::escape_html(initial)
        )
    }
}

/// Embedded admin template loader (compiled into the binary).
pub(crate) fn embedded_templates() -> crate::templates::InMemoryLoader {
    let mut l = crate::templates::InMemoryLoader::new();
    l.add("admin/base.html", include_str!("admin/templates/base.html"));
    l.add("admin/login.html", include_str!("admin/templates/login.html"));
    l.add("admin/index.html", include_str!("admin/templates/index.html"));
    l.add("admin/change_list.html", include_str!("admin/templates/change_list.html"));
    l.add("admin/change_form.html", include_str!("admin/templates/change_form.html"));
    l.add(
        "admin/delete_confirmation.html",
        include_str!("admin/templates/delete_confirmation.html"),
    );
    l
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::http::{AuthUser, Method};
    use crate::orm::{InMemoryBackend, ModelExt as _};
    use std::sync::Arc as StdArc;

    #[derive(Clone, Debug, crate::Model)]
    #[model(table = "adm_item")]
    pub struct Item {
        #[field(auto)]
        pub id: i64,
        #[field(char, max = 60)]
        pub name: String,
        #[field(int, default = 0)]
        pub quantity: i64,
    }

    fn site() -> AdminSite {
        let mut s = AdminSite::new("Rustango", "Rustango admin");
        s.register::<Item>(ModelAdmin {
            list_display: vec!["id".into(), "name".into(), "quantity".into()],
            search_fields: vec!["name".into()],
            ..Default::default()
        });
        s
    }

    fn req_with(method: Method, path: &str) -> (HttpRequest, StdArc<dyn Backend>) {
        let backend: StdArc<dyn Backend> = StdArc::new(InMemoryBackend::new());
        backend.create_table(Item::table(), Item::columns().to_vec()).unwrap();
        let mut req = HttpRequest::new(method, path);
        req.backend = Some(backend.clone());
        (req, backend)
    }

    fn engine() -> crate::templates::Engine {
        crate::templates::Engine::new(vec![Box::new(embedded_templates())], true)
    }

    fn staff(mut req: HttpRequest) -> HttpRequest {
        req.user = AuthUser::Authenticated {
            id: 1,
            username: "root".into(),
            is_staff: true,
            is_superuser: true,
        };
        req
    }

    #[test]
    fn admin_routes_registered() {
        let s = site();
        let router = s.urls();
        assert_eq!(router.routes.len(), 7);
        // names get the admin: namespace only after Router::mount
        let mut root = crate::urls::Router::empty();
        root.mount("admin/", "admin", s.urls());
        assert!(root.routes.iter().any(|r| r.name.as_deref() == Some("admin:index")));
        assert!(root.routes.iter().any(|r| r.name.as_deref() == Some("admin:add")));
        assert!(root.routes.iter().any(|r| r.name.as_deref() == Some("admin:login")));
    }

    #[test]
    fn login_flow() {
        let s = site();
        let backend = StdArc::new(InMemoryBackend::new());
        backend.create_table("auth_user", crate::auth::User::columns().to_vec()).unwrap();
        crate::auth::create_superuser(&*backend, "root", "", "pw").unwrap();

        let mut req = HttpRequest::new(Method::POST, "/admin/login/");
        req.backend = Some(backend.clone());
        req.engine = Some(StdArc::new(engine()));
        req.session = Some(crate::sessions::Session::load(backend.clone(), None, 3600));
        req.post.set("username", "root");
        req.post.set("password", "pw");
        let res = s.view_login(&mut req).unwrap();
        assert_eq!(res.status, 302);
        assert_eq!(res.headers.get("Location"), Some("/admin/"));
        assert!(req.user.is_authenticated());
    }

    #[test]
    fn index_requires_staff() {
        let s = site();
        let (mut req, _b) = req_with(Method::GET, "/admin/");
        req.engine = Some(StdArc::new(engine()));
        let res = s.view_index(&mut req).unwrap();
        // anonymous -> redirect to login
        assert_eq!(res.status, 302);
    }

    #[test]
    fn list_add_change_delete_flow() {
        let s = site();
        // One shared backend for the whole flow.
        let backend: StdArc<dyn crate::orm::Backend> = StdArc::new(InMemoryBackend::new());
        backend.create_table(Item::table(), Item::columns().to_vec()).unwrap();
        let mk = |method: Method, path: &str| {
            let mut req = HttpRequest::new(method, path);
            req.backend = Some(backend.clone());
            req.engine = Some(StdArc::new(engine()));
            req
        };

        // seed rows
        for i in 0..30 {
            let item = Item { id: 0, name: format!("item {i}"), quantity: i as i64 };
            item.save(&*backend).unwrap();
        }

        let mut req = staff(mk(Method::GET, "/admin/adm_item/"));
        req.params.insert("table".into(), "adm_item".into());
        let res = s.view_list(&mut req).unwrap();
        let body = res.body_string();
        assert!(body.contains("item 29"));
        assert!(body.contains("30 items"));

        // add form renders
        let mut req_add = staff(mk(Method::GET, "/admin/adm_item/add/"));
        req_add.params.insert("table".into(), "adm_item".into());
        let res = s.view_add_or_change(&mut req_add, None).unwrap();
        assert!(res.body_string().contains("name=\"name\""));

        // POST add
        let mut req_post = staff(mk(Method::POST, "/admin/adm_item/add/"));
        req_post.params.insert("table".into(), "adm_item".into());
        req_post.post.set("name", "created");
        req_post.post.set("quantity", "5");
        let res = s.view_add_or_change(&mut req_post, None).unwrap();
        assert_eq!(res.status, 302);
        let count = backend.count("adm_item", &crate::orm::Criteria::any()).unwrap();
        assert_eq!(count, 31);

        // change prefills
        let mut req_chg = staff(mk(Method::GET, "/admin/adm_item/31/change/"));
        req_chg.params.insert("table".into(), "adm_item".into());
        req_chg.params.insert("pk".into(), "31".into());
        let res = s.view_add_or_change(&mut req_chg, Some(31)).unwrap();
        assert!(res.body_string().contains("value=\"created\""));

        // POST change
        let mut req_chg2 = staff(mk(Method::POST, "/admin/adm_item/31/change/"));
        req_chg2.params.insert("table".into(), "adm_item".into());
        req_chg2.post.set("name", "edited");
        req_chg2.post.set("quantity", "9");
        let res = s.view_add_or_change(&mut req_chg2, Some(31)).unwrap();
        assert_eq!(res.status, 302);
        let item = Item::get(&*backend, 31).unwrap();
        assert_eq!(item.name, "edited");
        assert_eq!(item.quantity, 9);

        // delete
        let mut req_del = staff(mk(Method::POST, "/admin/adm_item/31/delete/"));
        req_del.params.insert("table".into(), "adm_item".into());
        req_del.params.insert("pk".into(), "31".into());
        let res = s.view_delete(&mut req_del, 31).unwrap();
        assert_eq!(res.status, 302);
        assert_eq!(backend.count("adm_item", &crate::orm::Criteria::any()).unwrap(), 30);
    }

    #[test]
    fn search_filter_works() {
        let s = site();
        let (mut req, backend) = req_with(Method::GET, "/admin/adm_item/?q=needle");
        req.engine = Some(StdArc::new(engine()));
        let mut req = staff(req);
        req.params.insert("table".into(), "adm_item".into());
        req.query.set("q", "needle");
        Item { id: 0, name: "haystack".into(), quantity: 1 }.save(&*backend).unwrap();
        Item { id: 0, name: "the needle".into(), quantity: 2 }.save(&*backend).unwrap();
        let res = s.view_list(&mut req).unwrap();
        let body = res.body_string();
        assert!(body.contains("the needle"));
        assert!(!body.contains("haystack"));
    }

    #[test]
    fn datetime_field_parses() {
        let f = AdminDateTimeField::new("when", false);
        let v = f.clean(Some("2026-01-15 10:30")).unwrap();
        assert!(matches!(v, Value::DateTime(_)));
        assert!(f.clean(Some("garbage")).is_err());
        let opt = AdminDateTimeField::new("when2", true);
        assert_eq!(opt.clean(None).unwrap(), Value::Null);
    }
}
