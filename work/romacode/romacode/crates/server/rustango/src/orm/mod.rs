//! Object-relational mapping — the `django.db.models` equivalent.
//!
//! Models are declared with the [`model!`](model) macro:
//!
//! ```ignore
//! model! {
//!     #[table = "polls_question"]
//!     pub struct Question {
//!         pub id: AutoId,
//!         pub question_text: Char(200),
//!         pub pub_date: DateTimeAuto,
//!     }
//! }
//!
//! let q = Question { id: 0, question_text: "What's new?".into(), pub_date: Utc::now() };
//! let q = q.save(&*backend)?;
//! let rows: Vec<Question> = Question::objects(&*backend)
//!     .filter(Q::contains("question_text", "new"))
//!     .order_by("-pub_date")
//!     .all()?;
//! ```

pub mod backend;
pub mod migrations;
pub mod model;
pub mod query;

pub use backend::{Backend, InMemoryBackend, JsonFileBackend};
pub use migrations::{Migration, MigrationOp};
pub use model::{Model, ModelExt};
pub use query::QuerySet;

// Common error alias, so users can write `orm::OrmError`.
pub use crate::errors::OrmError;

use std::sync::{OnceLock, RwLock};

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

pub type OrmResult<T> = Result<T, crate::errors::OrmError>;
pub type Row = Vec<Value>;

// ---------------------------------------------------------------------------
// Value — the dynamic column value
// ---------------------------------------------------------------------------

/// Dynamic value stored in a column. `Serialize`/`Deserialize` via a tagged
/// JSON representation so the file backend can persist rows verbatim.
#[derive(Debug, Clone, PartialEq)]
pub enum Value {
    Null,
    Int(i64),
    Float(f64),
    Text(String),
    Bool(bool),
    DateTime(DateTime<Utc>),
}

impl Value {
    pub fn as_int(&self) -> Option<i64> {
        match self { Value::Int(i) => Some(*i), _ => None }
    }
    pub fn as_float(&self) -> Option<f64> {
        match self { Value::Float(f) => Some(*f), Value::Int(i) => Some(*i as f64), _ => None }
    }
    pub fn as_text(&self) -> Option<&str> {
        match self { Value::Text(s) => Some(s), _ => None }
    }
    pub fn as_bool(&self) -> Option<bool> {
        match self { Value::Bool(b) => Some(*b), _ => None }
    }
    pub fn as_datetime(&self) -> Option<DateTime<Utc>> {
        match self { Value::DateTime(d) => Some(*d), _ => None }
    }

    /// Coerce to the string form used in comparisons and templates.
    pub fn display(&self) -> String {
        match self {
            Value::Null => String::new(),
            Value::Int(i) => i.to_string(),
            Value::Float(f) => {
                if f.fract() == 0.0 { format!("{f:.1}") } else { f.to_string() }
            }
            Value::Text(s) => s.clone(),
            Value::Bool(b) => (if *b { "True" } else { "False" }).to_string(),
            Value::DateTime(d) => d.format("%Y-%m-%d %H:%M:%S").to_string(),
        }
    }
}

impl From<i64> for Value { fn from(v: i64) -> Self { Value::Int(v) } }
impl From<i32> for Value { fn from(v: i32) -> Self { Value::Int(v as i64) } }
impl From<f64> for Value { fn from(v: f64) -> Self { Value::Float(v) } }
impl From<&str> for Value { fn from(v: &str) -> Self { Value::Text(v.to_string()) } }
impl From<String> for Value { fn from(v: String) -> Self { Value::Text(v) } }
impl From<bool> for Value { fn from(v: bool) -> Self { Value::Bool(v) } }
impl From<DateTime<Utc>> for Value { fn from(v: DateTime<Utc>) -> Self { Value::DateTime(v) } }
impl From<Option<String>> for Value {
    fn from(v: Option<String>) -> Self { v.map(Value::Text).unwrap_or(Value::Null) }
}

impl Serialize for Value {
    fn serialize<S: serde::Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        #[derive(Serialize)]
        #[serde(tag = "t", content = "v")]
        enum Tagged<'a> {
            N,
            I(i64),
            F(&'a f64),
            T(&'a str),
            B(bool),
            D(String),
        }
        match self {
            Value::Null => Tagged::N.serialize(s),
            Value::Int(i) => Tagged::I(*i).serialize(s),
            Value::Float(f) => Tagged::F(f).serialize(s),
            Value::Text(t) => Tagged::T(t).serialize(s),
            Value::Bool(b) => Tagged::B(*b).serialize(s),
            Value::DateTime(d) => Tagged::D(d.to_rfc3339()).serialize(s),
        }
    }
}

impl<'de> Deserialize<'de> for Value {
    fn deserialize<D: serde::Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        #[derive(Deserialize)]
        #[serde(tag = "t", content = "v")]
        enum Tagged {
            N,
            I(i64),
            F(f64),
            T(String),
            B(bool),
            D(String),
        }
        match Tagged::deserialize(d)? {
            Tagged::N => Ok(Value::Null),
            Tagged::I(i) => Ok(Value::Int(i)),
            Tagged::F(f) => Ok(Value::Float(f)),
            Tagged::T(t) => Ok(Value::Text(t)),
            Tagged::B(b) => Ok(Value::Bool(b)),
            Tagged::D(r) => DateTime::parse_from_rfc3339(&r)
                .map(|dt| Value::DateTime(dt.with_timezone(&Utc)))
                .map_err(serde::de::Error::custom),
        }
    }
}

// ---------------------------------------------------------------------------
// Column descriptors
// ---------------------------------------------------------------------------

/// Field kinds, mirroring `django.db.models` field classes.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum FieldKind {
    /// `models.AutoField` — primary key with auto increment.
    Auto,
    /// `models.CharField(max_length=N)`
    Char,
    /// `models.TextField`
    Text,
    /// `models.IntegerField`
    Integer,
    /// `models.FloatField`
    Float,
    /// `models.BooleanField(default=..)`
    Bool,
    /// `models.DateTimeField`
    DateTime,
    /// `models.DateTimeField(auto_now_add=True)`
    DateTimeAutoNowAdd,
    /// `models.ForeignKey(other_model)`
    ForeignKey { target_table: String },
}

/// One model field / table column.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ColumnDesc {
    pub name: String,
    pub kind: FieldKind,
    pub max_length: usize,
    pub null: bool,
    pub unique: bool,
    pub primary_key: bool,
    /// Optional default applied when inserting Null.
    pub default: Option<Value>,
}

impl ColumnDesc {
    fn new(name: &str, kind: FieldKind) -> Self {
        Self {
            name: name.to_string(),
            kind,
            max_length: 0,
            null: false,
            unique: false,
            primary_key: false,
            default: None,
        }
    }

    pub fn auto(name: &str) -> Self {
        let mut c = Self::new(name, FieldKind::Auto);
        c.primary_key = true;
        c
    }
    pub fn char(name: &str, max_length: usize) -> Self {
        let mut c = Self::new(name, FieldKind::Char);
        c.max_length = max_length;
        c
    }
    pub fn text(name: &str) -> Self { Self::new(name, FieldKind::Text) }
    pub fn integer(name: &str) -> Self { Self::new(name, FieldKind::Integer) }
    pub fn float(name: &str) -> Self { Self::new(name, FieldKind::Float) }
    pub fn boolean(name: &str, default: bool) -> Self {
        let mut c = Self::new(name, FieldKind::Bool);
        c.default = Some(Value::Bool(default));
        c
    }
    pub fn datetime(name: &str, null: bool) -> Self {
        let mut c = Self::new(name, FieldKind::DateTime);
        c.null = null;
        c
    }
    pub fn datetime_auto_now_add(name: &str) -> Self { Self::new(name, FieldKind::DateTimeAutoNowAdd) }
    pub fn foreign_key(name: &str, target_table: &str) -> Self {
        Self::new(name, FieldKind::ForeignKey { target_table: target_table.to_string() })
    }

    pub fn unique(mut self) -> Self { self.unique = true; self }
    pub fn nullable(mut self) -> Self { self.null = true; self }

    /// Apply a flag by name — used by the `model!` macro expansion
    /// (`Char(50, unique, null)`).
    pub fn with_flag_pub(self, flag: &str) -> ColumnDesc {
        crate::orm::model::apply_flag(self, flag)
    }

    /// Python-ish repr for error messages and admin.
    pub fn type_name(&self) -> &'static str {
        match self.kind {
            FieldKind::Auto => "AutoField",
            FieldKind::Char => "CharField",
            FieldKind::Text => "TextField",
            FieldKind::Integer => "IntegerField",
            FieldKind::Float => "FloatField",
            FieldKind::Bool => "BooleanField",
            FieldKind::DateTime => "DateTimeField",
            FieldKind::DateTimeAutoNowAdd => "DateTimeField(auto_now_add)",
            FieldKind::ForeignKey { .. } => "ForeignKey",
        }
    }
}

// ---------------------------------------------------------------------------
// Lookup operators — `field__gt`, `field__contains`, ...
// ---------------------------------------------------------------------------

/// Query operators, mirroring Django's lookup suffixes.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum Op {
    Exact,
    IExact,
    Contains,
    IContains,
    StartsWith,
    IStartsWith,
    EndsWith,
    IEndsWith,
    Gt,
    Gte,
    Lt,
    Lte,
    Year,
    Month,
    Day,
}

impl Op {
    pub fn from_lookup(word: &str) -> Option<Op> {
        match word {
            "exact" => Some(Op::Exact),
            "iexact" => Some(Op::IExact),
            "contains" => Some(Op::Contains),
            "icontains" => Some(Op::IContains),
            "startswith" => Some(Op::StartsWith),
            "istartswith" => Some(Op::IStartsWith),
            "endswith" => Some(Op::EndsWith),
            "iendswith" => Some(Op::IEndsWith),
            "gt" => Some(Op::Gt),
            "gte" => Some(Op::Gte),
            "lt" => Some(Op::Lt),
            "lte" => Some(Op::Lte),
            "year" => Some(Op::Year),
            "month" => Some(Op::Month),
            "day" => Some(Op::Day),
            _ => None,
        }
    }
}

// ---------------------------------------------------------------------------
// Criteria (Q objects)
// ---------------------------------------------------------------------------

/// Query criteria — the equivalent of Django's `Q` objects, combinable with
/// `&` (AND), `|` (OR) and `!` (NOT), and nestable.
#[derive(Debug, Clone, PartialEq)]
pub enum Criteria {
    And(Vec<Criteria>),
    Or(Vec<Criteria>),
    Not(Box<Criteria>),
    /// `field__op=value` where `field` may traverse FKs with `__`
    /// (`author_id__username__icontains`).
    Cond { field: String, op: Op, value: Value },
    /// `field__in=[..]`
    In { field: String, values: Vec<Value> },
    /// `field__isnull=True/False`
    IsNull { field: String, negated: bool },
}

/// Alias so call sites read like Django: `Q::eq("published", true)`.
pub type Q = Criteria;

impl Criteria {
    pub fn eq(field: impl Into<String>, value: impl Into<Value>) -> Criteria {
        Criteria::Cond { field: field.into(), op: Op::Exact, value: value.into() }
    }
    pub fn ne(field: impl Into<String>, value: impl Into<Value>) -> Criteria {
        Criteria::Not(Box::new(Criteria::eq(field, value)))
    }
    pub fn cond(field: impl Into<String>, op: Op, value: impl Into<Value>) -> Criteria {
        Criteria::Cond { field: field.into(), op, value: value.into() }
    }
    pub fn contains(field: impl Into<String>, value: impl Into<String>) -> Criteria {
        Criteria::cond(field, Op::Contains, value.into())
    }
    pub fn icontains(field: impl Into<String>, value: impl Into<String>) -> Criteria {
        Criteria::cond(field, Op::IContains, value.into())
    }
    pub fn gt(field: impl Into<String>, value: impl Into<Value>) -> Criteria {
        Criteria::cond(field, Op::Gt, value)
    }
    pub fn gte(field: impl Into<String>, value: impl Into<Value>) -> Criteria {
        Criteria::cond(field, Op::Gte, value)
    }
    pub fn lt(field: impl Into<String>, value: impl Into<Value>) -> Criteria {
        Criteria::cond(field, Op::Lt, value)
    }
    pub fn lte(field: impl Into<String>, value: impl Into<Value>) -> Criteria {
        Criteria::cond(field, Op::Lte, value)
    }
    pub fn in_(field: impl Into<String>, values: Vec<Value>) -> Criteria {
        Criteria::In { field: field.into(), values }
    }
    pub fn isnull(field: impl Into<String>) -> Criteria {
        Criteria::IsNull { field: field.into(), negated: false }
    }
    pub fn not_null(field: impl Into<String>) -> Criteria {
        Criteria::IsNull { field: field.into(), negated: true }
    }
    /// True criteria (matches everything) — `Q()`.
    pub fn any() -> Criteria { Criteria::And(vec![]) }

    pub fn is_any(&self) -> bool {
        matches!(self, Criteria::And(v) if v.is_empty())
    }
}

impl std::ops::BitAnd for Criteria {
    type Output = Criteria;
    fn bitand(self, rhs: Criteria) -> Criteria {
        Criteria::And(vec![self, rhs])
    }
}

impl std::ops::BitOr for Criteria {
    type Output = Criteria;
    fn bitor(self, rhs: Criteria) -> Criteria {
        Criteria::Or(vec![self, rhs])
    }
}

impl std::ops::Not for Criteria {
    type Output = Criteria;
    fn not(self) -> Criteria {
        Criteria::Not(Box::new(self))
    }
}

// ---------------------------------------------------------------------------
// Model registry (for FK traversal, migrations and the admin)
// ---------------------------------------------------------------------------

/// Runtime metadata for one registered model.
#[derive(Debug, Clone)]
pub struct ModelMeta {
    pub table: String,
    pub columns: Vec<ColumnDesc>,
    /// (fk column name, target table)
    pub fks: Vec<(String, String)>,
}

impl ModelMeta {
    pub fn column(&self, name: &str) -> Option<&ColumnDesc> {
        self.columns.iter().find(|c| c.name == name)
    }
}

#[derive(Debug, Default)]
pub struct Registry {
    pub models: Vec<ModelMeta>,
}

impl Registry {
    pub fn add(&mut self, meta: ModelMeta) {
        if let Some(existing) = self.models.iter_mut().find(|m| m.table == meta.table) {
            *existing = meta;
        } else {
            self.models.push(meta);
        }
    }

    pub fn get(&self, table: &str) -> Option<&ModelMeta> {
        self.models.iter().find(|m| m.table == table)
    }
}

static REGISTRY: OnceLock<RwLock<Registry>> = OnceLock::new();

/// The global model registry.
pub fn registry() -> &'static RwLock<Registry> {
    REGISTRY.get_or_init(|| RwLock::new(Registry::default()))
}

/// Register a model's metadata (called by `App::models::<T>()`).
pub fn register_model(meta: ModelMeta) {
    registry().write().unwrap().add(meta);
}

/// snake_case of a CamelCase model name — used for default table names.
pub fn snake_case(name: &str) -> String {
    let mut out = String::with_capacity(name.len() + 4);
    for (i, ch) in name.chars().enumerate() {
        if ch.is_ascii_uppercase() {
            if i > 0 {
                out.push('_');
            }
            out.push(ch.to_ascii_lowercase());
        } else {
            out.push(ch);
        }
    }
    out
}

// ---------------------------------------------------------------------------
// Criteria evaluation (with FK traversal)
// ---------------------------------------------------------------------------

/// Resolve `field__path__op` against the registry.
/// Returns (final table, column name, op-from-path) — the op is `Some`
/// when the path carried a lookup suffix (`pub_date__year`), `None` when
/// the path is just a field (the caller's own op applies).
pub fn resolve_field_path(
    start_table: &str,
    path: &str,
) -> OrmResult<(String, String, Option<Op>)> {
    let segs: Vec<&str> = path.split("__").collect();
    let mut cur_table = start_table.to_string();
    let mut col: Option<String> = None;
    let mut op: Option<Op> = None;

    for (i, seg) in segs.iter().enumerate() {
        let meta = registry().read().unwrap().get(&cur_table).cloned();

        // An explicit lookup suffix after a resolved column.
        if col.is_some() {
            if let Some(o) = Op::from_lookup(seg) {
                op = Some(o);
                break;
            }
        }

        match meta {
            Some(m) => {
                // FK traversal — unless the next segment is an op suffix
                // (`author_id__gt=5` compares the fk column value itself).
                if let Some((fk_col, target)) = m.fks.iter().find(|(c, _)| c == seg) {
                    let next_is_op = segs.get(i + 1).and_then(|n| Op::from_lookup(n));
                    if next_is_op.is_some() {
                        col = Some(fk_col.clone());
                        op = next_is_op;
                        break;
                    }
                    cur_table = target.clone();
                    col = None;
                    continue;
                }
                // Plain column?
                if m.column(seg).is_some() {
                    col = Some(seg.to_string());
                    continue;
                }
                // FK column referenced directly as a value column.
                if m.fks.iter().any(|(c, _)| c == seg) {
                    col = Some(seg.to_string());
                    continue;
                }
                return Err(crate::errors::OrmError::UnknownField(format!(
                    "{path} (at `{seg}`)"
                )));
            }
            None => {
                // No registry entry: treat the segment as a plain column
                // (single-table backends / unregistered models).
                col = Some(seg.to_string());
            }
        }
    }

    let col = col.ok_or_else(|| crate::errors::OrmError::UnknownField(path.to_string()))?;
    Ok((cur_table, col, op))
}

/// Extract the comparison target for a date op.
fn date_part(v: &Value, op: Op) -> Option<i64> {
    let d = v.as_datetime()?;
    match op {
        Op::Year => Some(d.format("%Y").to_string().parse().ok()?),
        Op::Month => Some(d.format("%m").to_string().parse().ok()?),
        Op::Day => Some(d.format("%d").to_string().parse().ok()?),
        _ => None,
    }
}

fn compare(op: Op, left: &Value, right: &Value) -> bool {
    match op {
        Op::Exact => left == right,
        Op::IExact => match (left.as_text(), right.as_text()) {
            // Unicode-consistent folding: `iexact` and `icontains` must agree
            // for non-ASCII text (`eq_ignore_ascii_case` disagreed with
            // `icontains`'s `to_lowercase`).
            (Some(a), Some(b)) => a.to_lowercase() == b.to_lowercase(),
            _ => left == right,
        },
        Op::Contains => contains_like(left, right, false),
        Op::IContains => contains_like(left, right, true),
        Op::StartsWith => text_like(left, right, true, false),
        Op::IStartsWith => text_like(left, right, true, true),
        Op::EndsWith => text_like(left, right, false, false),
        Op::IEndsWith => text_like(left, right, false, true),
        Op::Gt | Op::Gte | Op::Lt | Op::Lte => ordered_cmp(op, left, right),
        Op::Year | Op::Month | Op::Day => match (date_part(left, op), right.as_int()) {
            (Some(a), Some(b)) => a == b,
            _ => false,
        },
    }
}

fn contains_like(left: &Value, right: &Value, insensitive: bool) -> bool {
    let (l, r) = (left.display(), right.display());
    if insensitive {
        l.to_lowercase().contains(&r.to_lowercase())
    } else {
        l.contains(&r)
    }
}

fn text_like(left: &Value, right: &Value, start: bool, insensitive: bool) -> bool {
    let (mut l, mut r) = (left.display(), right.display());
    if insensitive {
        l = l.to_lowercase();
        r = r.to_lowercase();
    }
    if start { l.starts_with(&r) } else { l.ends_with(&r) }
}

fn ordered_cmp(op: Op, left: &Value, right: &Value) -> bool {
    let ord = if let (Some(a), Some(b)) = (left.as_datetime(), right.as_datetime()) {
        Some(a.cmp(&b))
    } else if let (Some(a), Some(b)) = (left.as_float(), right.as_float()) {
        Some(a.partial_cmp(&b).unwrap_or(std::cmp::Ordering::Equal))
    } else {
        left.display().partial_cmp(&right.display())
    };
    match ord {
        Some(std::cmp::Ordering::Greater) => matches!(op, Op::Gt | Op::Gte),
        Some(std::cmp::Ordering::Less) => matches!(op, Op::Lt | Op::Lte),
        Some(std::cmp::Ordering::Equal) => matches!(op, Op::Gte | Op::Lte),
        None => false,
    }
}

/// Evaluate a criteria tree against a row of `table`.
/// FK paths fetch related rows from `backend` as needed.
pub fn eval_criteria(
    backend: &dyn Backend,
    table: &str,
    row: &Row,
    cols: &[ColumnDesc],
    crit: &Criteria,
) -> bool {
    match crit {
        Criteria::And(list) => list.iter().all(|c| eval_criteria(backend, table, row, cols, c)),
        Criteria::Or(list) => list.iter().any(|c| eval_criteria(backend, table, row, cols, c)),
        Criteria::Not(inner) => !eval_criteria(backend, table, row, cols, inner),
        Criteria::Cond { field, op, value } => {
            // Fast path: plain column without `__` traversal.
            if !field.contains("__") {
                if let Some(idx) = cols.iter().position(|c| c.name == *field) {
                    if let Some(v) = row.get(idx) {
                        return compare(*op, v, value);
                    }
                }
                return false;
            }
            match resolve_field_path(table, field) {
                Ok((target_table, col_name, path_op)) => {
                    let effective = path_op.unwrap_or(*op);
                    let row_value =
                        follow_path(backend, table, row, cols, field, &target_table, &col_name);
                    match row_value {
                        Some(v) => compare(effective, &v, value),
                        None => false,
                    }
                }
                Err(_) => false,
            }
        }
        Criteria::In { field, values } => {
            if !field.contains("__") {
                if let Some(idx) = cols.iter().position(|c| c.name == *field) {
                    if let Some(v) = row.get(idx) {
                        return values.contains(v);
                    }
                }
                return false;
            }
            match resolve_field_path(table, field) {
                Ok((target_table, col_name, _)) => {
                    match follow_path(backend, table, row, cols, field, &target_table, &col_name) {
                        Some(v) => values.contains(&v),
                        None => false,
                    }
                }
                Err(_) => false,
            }
        }
        Criteria::IsNull { field, negated } => {
            if !field.contains("__") {
                if let Some(idx) = cols.iter().position(|c| c.name == *field) {
                    let v = row.get(idx).cloned().unwrap_or(Value::Null);
                    return match v {
                        Value::Null => !*negated,
                        _ => *negated,
                    };
                }
                return false;
            }
            match resolve_field_path(table, field) {
                Ok((target_table, col_name, _)) => {
                    match follow_path(backend, table, row, cols, field, &target_table, &col_name) {
                        Some(Value::Null) => !*negated,
                        Some(_) => *negated,
                        None => !*negated,
                    }
                }
                Err(_) => false,
            }
        }
    }
}

/// Walk an FK path: `author_id__username` fetches the related row.
fn follow_path(
    backend: &dyn Backend,
    table: &str,
    row: &Row,
    cols: &[ColumnDesc],
    path: &str,
    target_table: &str,
    col_name: &str,
) -> Option<Value> {
    // If the path only touches the base table, just read the column.
    if target_table == table && col_name != "id" || path.split("__").count() == 1 {
        let idx = cols.iter().position(|c| c.name == col_name)?;
        return row.get(idx).cloned();
    }

    // Walk FK by FK: collect the fk columns along the path.
    let segs: Vec<&str> = path.split("__").collect();
    let mut cur_table = table.to_string();
    let mut cur_row: Row = row.to_vec();
    let mut cur_cols: Vec<ColumnDesc> = cols.to_vec();

    for (i, seg) in segs.iter().enumerate() {
        let last = i + 1 == segs.len();
        let meta = registry().read().unwrap().get(&cur_table).cloned();

        if let Some(meta) = &meta {
            if let Some((fk_col, target)) = meta.fks.iter().find(|(c, _)| c == seg) {
                let idx = cur_cols.iter().position(|c| c.name == *fk_col)?;
                let fk_val = cur_row.get(idx).cloned().unwrap_or(Value::Null);
                if last {
                    // the fk column itself was requested
                    return Some(fk_val);
                }
                let pk = fk_val.as_int()?;
                let crit = Criteria::eq("id", Value::Int(pk));
                let related = backend.select(target, &crit, &[], None, None).ok()?;
                let rel_row = related.into_iter().next()?;
                let rel_cols = backend.columns(target);
                cur_table = target.clone();
                cur_row = rel_row;
                cur_cols = rel_cols;
                continue;
            }
        }

        // Final column on the current row.
        let idx = cur_cols.iter().position(|c| c.name == *seg)?;
        return cur_row.get(idx).cloned();
    }

    let idx = cur_cols.iter().position(|c| c.name == col_name)?;
    cur_row.get(idx).cloned()
}

/// Order rows by `order` spec (`name` asc, `-name` desc), FK-aware.
pub fn order_rows(
    backend: &dyn Backend,
    table: &str,
    rows: &mut [Row],
    cols: &[ColumnDesc],
    order: &[(String, bool)],
) {
    if order.is_empty() {
        return;
    }
    let resolved: Vec<(String, String, bool, Op)> = order
        .iter()
        .filter_map(|(field, desc)| {
            resolve_field_path(table, field)
                .ok()
                .map(|(t, c, op)| (t, c, *desc, op.unwrap_or(Op::Exact)))
        })
        .collect();
    if resolved.is_empty() {
        return;
    }
    let cols_snapshot: Vec<ColumnDesc> = cols.to_vec();
    rows.sort_by(|a, b| {
        for (t, col, desc, op) in &resolved {
            let va = follow_path(backend, table, a, &cols_snapshot, &field_for(t, col), t, col)
                .unwrap_or(Value::Null);
            let vb = follow_path(backend, table, b, &cols_snapshot, &field_for(t, col), t, col)
                .unwrap_or(Value::Null);
            // date ops are not orderable this way; treat as Exact value compare
            let cmp = cmp_values(&va, &vb, *op);
            let cmp = if *desc { cmp.reverse() } else { cmp };
            if cmp != std::cmp::Ordering::Equal {
                return cmp;
            }
        }
        std::cmp::Ordering::Equal
    });
}

fn field_for(_t: &str, col: &str) -> String { col.to_string() }

fn cmp_values(a: &Value, b: &Value, op: Op) -> std::cmp::Ordering {
    if let (Some(x), Some(y)) = (a.as_datetime(), b.as_datetime()) {
        return x.cmp(&y);
    }
    if let (Some(x), Some(y)) = (a.as_float(), b.as_float()) {
        return x.partial_cmp(&y).unwrap_or(std::cmp::Ordering::Equal);
    }
    if matches!(op, Op::Year | Op::Month | Op::Day) {
        if let (Some(x), Some(y)) = (date_part(a, op), date_part(b, op)) {
            return x.cmp(&y);
        }
    }
    a.display().cmp(&b.display())
}

/// Unique-constraint check shared by backends.
pub fn check_unique(
    backend: &dyn Backend,
    table: &str,
    cols: &[ColumnDesc],
    row: &Row,
    exclude_pk: Option<i64>,
) -> OrmResult<()> {
    for (i, col) in cols.iter().enumerate() {
        if !col.unique || col.primary_key {
            continue;
        }
        let val = row.get(i).cloned().unwrap_or(Value::Null);
        if val == Value::Null {
            continue;
        }
        let crit = Criteria::eq(&col.name, val);
        let found = backend.select(table, &crit, &[], None, None)?;
        if let Some(other) = found.first() {
            let other_pk = other.first().cloned().unwrap_or(Value::Null).as_int();
            if other_pk != exclude_pk {
                return Err(crate::errors::OrmError::InvalidValue(
                    col.name.clone(),
                    "unique constraint violated".into(),
                ));
            }
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn value_coercion_and_display() {
        assert_eq!(Value::Int(3).display(), "3");
        assert_eq!(Value::Float(3.0).display(), "3.0");
        assert_eq!(Value::Float(3.5).display(), "3.5");
        assert_eq!(Value::Bool(true).display(), "True");
        assert_eq!(Value::Text("x".into()).as_text(), Some("x"));
        assert_eq!(Value::from(7i64).as_int(), Some(7));
    }

    #[test]
    fn value_serde_roundtrip() {
        let v = Value::DateTime(Utc::now());
        let json = serde_json::to_string(&v).unwrap();
        let back: Value = serde_json::from_str(&json).unwrap();
        assert_eq!(v, back);
        let v = Value::Text("hi".into());
        let back: Value = serde_json::from_str(&serde_json::to_string(&v).unwrap()).unwrap();
        assert_eq!(v, back);
    }

    #[test]
    fn snake_case_names() {
        assert_eq!(snake_case("Question"), "question");
        assert_eq!(snake_case("AuthUser"), "auth_user");
        assert_eq!(snake_case("HTTPError"), "h_t_t_p_error"); // documented quirk
    }

    #[test]
    fn q_operators() {
        let a = Q::eq("a", 1);
        let b = Q::eq("b", 2);
        let both = a & b;
        let either = Q::eq("a", 1) | Q::eq("b", 2);
        let neg = !Q::eq("a", 1);
        assert!(matches!(both, Criteria::And(_)));
        assert!(matches!(either, Criteria::Or(_)));
        assert!(matches!(neg, Criteria::Not(_)));
        assert!(Q::any().is_any());
    }

    #[test]
    fn compare_ops() {
        assert!(compare(Op::Exact, &Value::Int(3), &Value::Int(3)));
        assert!(!compare(Op::Exact, &Value::Int(3), &Value::Int(4)));
        assert!(compare(Op::IExact, &Value::Text("ABC".into()), &Value::Text("abc".into())));
        // Regression (M35): `iexact` used `eq_ignore_ascii_case` while
        // `icontains` folded with `to_lowercase` — the two disagreed for
        // non-ASCII text. They must agree.
        assert!(compare(
            Op::IExact,
            &Value::Text("ÄPFEL".into()),
            &Value::Text("äpfel".into())
        ));
        assert!(compare(
            Op::IContains,
            &Value::Text("ÄPFEL".into()),
            &Value::Text("pfel".into())
        ));
        assert!(compare(Op::Contains, &Value::Text("hello world".into()), &Value::Text("world".into())));
        assert!(compare(Op::IContains, &Value::Text("HELLO".into()), &Value::Text("ell".into())));
        assert!(compare(Op::StartsWith, &Value::Text("rust".into()), &Value::Text("ru".into())));
        assert!(compare(Op::EndsWith, &Value::Text("rust".into()), &Value::Text("st".into())));
        assert!(compare(Op::Gt, &Value::Int(5), &Value::Int(3)));
        assert!(!compare(Op::Gt, &Value::Int(3), &Value::Int(3)));
        assert!(compare(Op::Gte, &Value::Int(3), &Value::Int(3)));
        assert!(compare(Op::Lt, &Value::Text("a".into()), &Value::Text("b".into())));
        let d1 = Value::DateTime(Utc::now());
        assert!(compare(Op::Exact, &d1, &d1));
    }
}
