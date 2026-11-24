//! Forms — `django.forms`: field definitions, validation, `cleaned_data`,
//! errors and widget rendering.
//!
//! ```ignore
//! use rustango::forms::{Form, CharField, EmailField, IntegerField};
//!
//! let form = Form::new("contact", vec![
//!     Box::new(CharField::new("name").max_length(100).required()),
//!     Box::new(EmailField::new("email")),
//!     Box::new(IntegerField::new("age").min_value(0).max_value(150)),
//! ]);
//!
//! let bound = form.bind(&req.post);
//! if bound.is_valid() {
//!     let name = bound.cleaned("name");
//! }
//! ```

use std::collections::HashMap;

use crate::http::QueryDict;
use crate::orm::Value;

/// Validation error messages for one field.
pub type FieldErrors = Vec<String>;

/// A form field: validation + widget rendering.
pub trait Field: Send + Sync {
    fn name(&self) -> &str;
    fn label(&self) -> String;
    fn required(&self) -> bool { true }
    /// Validate raw input; returns the cleaned value or error messages.
    fn clean(&self, raw: Option<&str>) -> Result<Value, FieldErrors>;
    /// Render the widget for unbound or bound display.
    fn render(&self, bound: Option<&str>, error: Option<&str>) -> String;
    /// Render a full row: label + widget + errors (as_p style).
    fn render_row(&self, bound: Option<&str>) -> String {
        let label = format!(
            "<label for=\"id_{}\">{}{}</label>",
            self.name(),
            self.label(),
            if self.required() { " <span class=\"required\">*</span>" } else { "" }
        );
        let widget = self.render(bound, None);
        format!("<p>{} {}</p>", label, widget)
    }
}

// ---------------------------------------------------------------------------
// Shared builder state
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Default)]
struct CommonOpts {
    required: bool,
    label: Option<String>,
    initial: Option<String>,
    help_text: Option<String>,
    widget_attrs: Vec<(String, String)>,
}

impl CommonOpts {
    fn label_of(&self, name: &str) -> String {
        self.label
            .clone()
            .unwrap_or_else(|| {
                let mut s = name.replace('_', " ");
                if let Some(first) = s.chars().next() {
                    let up: String = first.to_uppercase().collect::<String>();
                    s = format!("{}{}", up, &s[first.len_utf8()..]);
                }
                s
            })
    }
}

fn input_html(name: &str, type_: &str, value: Option<&str>, extra_attrs: &[(String, String)], css: &str) -> String {
    let value_attr = value
        .map(|v| format!(" value=\"{}\"", crate::templates::escape_html(v)))
        .unwrap_or_default();
    let mut attrs = String::new();
    for (k, v) in extra_attrs {
        attrs.push_str(&format!(" {}=\"{}\"", k, crate::templates::escape_html(v)));
    }
    format!(
        "<input type=\"{type_}\" name=\"{name}\" id=\"id_{name}\"{value_attr} class=\"{css}\"{attrs}>",
    )
}

// ---------------------------------------------------------------------------
// CharField / TextField
// ---------------------------------------------------------------------------

/// `forms.CharField` — text input with max/min length validation.
pub struct CharField {
    name: String,
    opts: CommonOpts,
    max_length: Option<usize>,
    min_length: Option<usize>,
    strip: bool,
    empty_value: Option<String>,
    textarea: bool,
}

impl CharField {
    pub fn new(name: impl Into<String>) -> Self {
        Self {
            name: name.into(),
            opts: CommonOpts { required: true, ..Default::default() },
            max_length: None,
            min_length: None,
            strip: true,
            empty_value: None,
            textarea: false,
        }
    }
    pub fn max_length(mut self, n: usize) -> Self { self.max_length = Some(n); self }
    pub fn min_length(mut self, n: usize) -> Self { self.min_length = Some(n); self }
    /// Do not strip surrounding whitespace.
    pub fn no_strip(mut self) -> Self { self.strip = false; self }
    /// Use a `<textarea>` widget instead of `<input type=text>`.
    pub fn textarea(mut self) -> Self { self.textarea = true; self }
    /// Make the field optional (`required=False`).
    pub fn optional(mut self) -> Self { self.opts.required = false; self }
    pub fn with_required(mut self, v: bool) -> Self { self.opts.required = v; self }
    pub fn with_label(mut self, v: impl Into<String>) -> Self { self.opts.label = Some(v.into()); self }
    pub fn with_initial(mut self, v: impl Into<String>) -> Self { self.opts.initial = Some(v.into()); self }
    pub fn with_help_text(mut self, v: impl Into<String>) -> Self { self.opts.help_text = Some(v.into()); self }
    pub fn with_attr(mut self, k: impl Into<String>, v: impl Into<String>) -> Self {
        self.opts.widget_attrs.push((k.into(), v.into()));
        self
    }

    fn clean_impl(&self, raw: Option<&str>) -> Result<Value, FieldErrors> {
        let mut errors = FieldErrors::new();
        let value = match raw {
            None => String::new(),
            Some(s) => {
                if self.strip { s.trim().to_string() } else { s.to_string() }
            }
        };
        if value.is_empty() {
            if self.required() {
                errors.push("This field is required.".to_string());
                return Err(errors);
            }
            if let Some(ev) = &self.empty_value {
                return Ok(Value::Text(ev.clone()));
            }
            return Ok(Value::Null);
        }
        if let Some(max) = self.max_length {
            if value.chars().count() > max {
                errors.push(format!(
                    "Ensure this value has at most {} characters (it has {}).",
                    max,
                    value.chars().count()
                ));
            }
        }
        if let Some(min) = self.min_length {
            if value.chars().count() < min {
                errors.push(format!(
                    "Ensure this value has at least {} characters (it has {}).",
                    min,
                    value.chars().count()
                ));
            }
        }
        if !errors.is_empty() {
            return Err(errors);
        }
        Ok(Value::Text(value))
    }
}

impl Field for CharField {
    fn name(&self) -> &str { &self.name }
    fn label(&self) -> String { self.opts.label_of(&self.name) }
    fn required(&self) -> bool { self.opts.required }

    fn clean(&self, raw: Option<&str>) -> Result<Value, FieldErrors> {
        self.clean_impl(raw)
    }

    fn render(&self, bound: Option<&str>, error: Option<&str>) -> String {
        let css = if error.is_some() { "form-control is-invalid" } else { "form-control" };
        let value = bound.or(self.opts.initial.as_deref());
        if self.textarea {
            let v = crate::templates::escape_html(value.unwrap_or(""));
            format!(
                "<textarea name=\"{0}\" id=\"id_{0}\" class=\"{1}\" rows=\"4\">{2}</textarea>",
                self.name, css, v
            )
        } else {
            input_html(&self.name, "text", value, &self.opts.widget_attrs, css)
        }
    }
}

/// `forms.CharField(widget=Textarea)` shorthand with no length limits.
/// (Django-style class name; intentionally not snake_case.)
#[allow(non_snake_case)]
pub fn TextField(name: impl Into<String>) -> CharField {
    CharField::new(name).textarea().optional()
}

// ---------------------------------------------------------------------------
// EmailField / SlugField / URLField
// ---------------------------------------------------------------------------

/// `forms.EmailField`.
pub struct EmailField {
    base: CharField,
}

impl EmailField {
    pub fn new(name: impl Into<String>) -> Self {
        Self { base: CharField::new(name) }
    }
    pub fn optional(self) -> Self { Self { base: self.base.optional() } }
    pub fn with_required(self, v: bool) -> Self { Self { base: self.base.with_required(v) } }
    pub fn with_label(self, v: impl Into<String>) -> Self { Self { base: self.base.with_label(v) } }
    pub fn with_initial(self, v: impl Into<String>) -> Self { Self { base: self.base.with_initial(v) } }
    pub fn with_attr(self, k: impl Into<String>, v: impl Into<String>) -> Self {
        Self { base: self.base.with_attr(k, v) }
    }
    pub fn max_length(mut self, n: usize) -> Self { self.base.max_length = Some(n); self }

    fn valid_email(s: &str) -> bool {
        // Pragmatic RFC-lite check: one @, non-empty local + domain with dot.
        let parts: Vec<&str> = s.split('@').collect();
        if parts.len() != 2 {
            return false;
        }
        let (local, domain) = (parts[0], parts[1]);
        !local.is_empty()
            && domain.contains('.')
            && !domain.starts_with('.')
            && !domain.ends_with('.')
            && domain.split('.').all(|p| !p.is_empty())
            && !s.contains(char::is_whitespace)
    }
}

impl Field for EmailField {
    fn name(&self) -> &str { self.base.name() }
    fn label(&self) -> String { self.base.label() }
    fn required(&self) -> bool { self.base.required() }

    fn clean(&self, raw: Option<&str>) -> Result<Value, FieldErrors> {
        match self.base.clean_impl(raw) {
            Ok(Value::Text(v)) => {
                if Self::valid_email(&v) {
                    Ok(Value::Text(v))
                } else {
                    Err(vec!["Enter a valid email address.".to_string()])
                }
            }
            other => other,
        }
    }

    fn render(&self, bound: Option<&str>, error: Option<&str>) -> String {
        let css = if error.is_some() { "form-control is-invalid" } else { "form-control" };
        let value = bound.or(self.base.opts.initial.as_deref());
        input_html(self.base.name(), "email", value, &self.base.opts.widget_attrs, css)
    }
}

/// `forms.SlugField`.
pub struct SlugField {
    base: CharField,
}

impl SlugField {
    pub fn new(name: impl Into<String>) -> Self {
        Self { base: CharField::new(name) }
    }
    pub fn optional(self) -> Self { Self { base: self.base.optional() } }
    pub fn with_required(self, v: bool) -> Self { Self { base: self.base.with_required(v) } }
    pub fn with_label(self, v: impl Into<String>) -> Self { Self { base: self.base.with_label(v) } }
    pub fn with_initial(self, v: impl Into<String>) -> Self { Self { base: self.base.with_initial(v) } }
    pub fn with_attr(self, k: impl Into<String>, v: impl Into<String>) -> Self {
        Self { base: self.base.with_attr(k, v) }
    }
    pub fn max_length(mut self, n: usize) -> Self { self.base.max_length = Some(n); self }
}

impl Field for SlugField {
    fn name(&self) -> &str { self.base.name() }
    fn label(&self) -> String { self.base.label() }
    fn required(&self) -> bool { self.base.required() }

    fn clean(&self, raw: Option<&str>) -> Result<Value, FieldErrors> {
        match self.base.clean_impl(raw) {
            Ok(Value::Text(v)) => {
                let ok = !v.is_empty()
                    && v.chars().all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_');
                if ok {
                    Ok(Value::Text(v))
                } else {
                    Err(vec![
                        "Enter a valid 'slug' consisting of letters, numbers, underscores or hyphens."
                            .to_string(),
                    ])
                }
            }
            other => other,
        }
    }

    fn render(&self, bound: Option<&str>, error: Option<&str>) -> String {
        let css = if error.is_some() { "form-control is-invalid" } else { "form-control" };
        input_html(self.base.name(), "text", bound.or(self.base.opts.initial.as_deref()), &self.base.opts.widget_attrs, css)
    }
}

// ---------------------------------------------------------------------------
// IntegerField / FloatField / BooleanField / DateField
// ---------------------------------------------------------------------------

/// `forms.IntegerField`.
pub struct IntegerField {
    name: String,
    opts: CommonOpts,
    min_value: Option<i64>,
    max_value: Option<i64>,
}

impl IntegerField {
    pub fn new(name: impl Into<String>) -> Self {
        Self { name: name.into(), opts: CommonOpts { required: true, ..Default::default() }, min_value: None, max_value: None }
    }
    pub fn min_value(mut self, v: i64) -> Self { self.min_value = Some(v); self }
    pub fn max_value(mut self, v: i64) -> Self { self.max_value = Some(v); self }
    pub fn optional(mut self) -> Self { self.opts.required = false; self }
    pub fn label(mut self, v: impl Into<String>) -> Self { self.opts.label = Some(v.into()); self }
    pub fn with_initial(mut self, v: impl Into<String>) -> Self { self.opts.initial = Some(v.into()); self }
}

impl Field for IntegerField {
    fn name(&self) -> &str { &self.name }
    fn label(&self) -> String { self.opts.label_of(&self.name) }
    fn required(&self) -> bool { self.opts.required }

    fn clean(&self, raw: Option<&str>) -> Result<Value, FieldErrors> {
        let mut errors = FieldErrors::new();
        let raw = raw.map(|s| s.trim()).unwrap_or("");
        if raw.is_empty() {
            if self.required() {
                return Err(vec!["This field is required.".to_string()]);
            }
            return Ok(Value::Null);
        }
        match raw.parse::<i64>() {
            Ok(n) => {
                if let Some(min) = self.min_value {
                    if n < min {
                        errors.push(format!("Ensure this value is greater than or equal to {min}."));
                    }
                }
                if let Some(max) = self.max_value {
                    if n > max {
                        errors.push(format!("Ensure this value is less than or equal to {max}."));
                    }
                }
                if !errors.is_empty() {
                    Err(errors)
                } else {
                    Ok(Value::Int(n))
                }
            }
            Err(_) => Err(vec!["Enter a whole number.".to_string()]),
        }
    }

    fn render(&self, bound: Option<&str>, error: Option<&str>) -> String {
        let css = if error.is_some() { "form-control is-invalid" } else { "form-control" };
        input_html(&self.name, "number", bound.or(self.opts.initial.as_deref()), &self.opts.widget_attrs, css)
    }
}

/// `forms.FloatField`.
pub struct FloatField {
    name: String,
    opts: CommonOpts,
    min_value: Option<f64>,
    max_value: Option<f64>,
}

impl FloatField {
    pub fn new(name: impl Into<String>) -> Self {
        Self { name: name.into(), opts: CommonOpts { required: true, ..Default::default() }, min_value: None, max_value: None }
    }
    pub fn min_value(mut self, v: f64) -> Self { self.min_value = Some(v); self }
    pub fn max_value(mut self, v: f64) -> Self { self.max_value = Some(v); self }
    pub fn optional(mut self) -> Self { self.opts.required = false; self }
    pub fn label(mut self, v: impl Into<String>) -> Self { self.opts.label = Some(v.into()); self }
    pub fn with_initial(mut self, v: impl Into<String>) -> Self { self.opts.initial = Some(v.into()); self }
}

impl Field for FloatField {
    fn name(&self) -> &str { &self.name }
    fn label(&self) -> String { self.opts.label_of(&self.name) }
    fn required(&self) -> bool { self.opts.required }

    fn clean(&self, raw: Option<&str>) -> Result<Value, FieldErrors> {
        let raw = raw.map(|s| s.trim()).unwrap_or("");
        if raw.is_empty() {
            if self.required() {
                return Err(vec!["This field is required.".to_string()]);
            }
            return Ok(Value::Null);
        }
        match raw.parse::<f64>() {
            Ok(n) => {
                if let Some(min) = self.min_value {
                    if n < min {
                        return Err(vec![format!("Ensure this value is greater than or equal to {min}.")]);
                    }
                }
                if let Some(max) = self.max_value {
                    if n > max {
                        return Err(vec![format!("Ensure this value is less than or equal to {max}.")]);
                    }
                }
                Ok(Value::Float(n))
            }
            Err(_) => Err(vec!["Enter a number.".to_string()]),
        }
    }

    fn render(&self, bound: Option<&str>, error: Option<&str>) -> String {
        let css = if error.is_some() { "form-control is-invalid" } else { "form-control" };
        let mut attrs = self.opts.widget_attrs.clone();
        attrs.push(("step".into(), "any".into()));
        input_html(&self.name, "number", bound.or(self.opts.initial.as_deref()), &attrs, css)
    }
}

/// `forms.BooleanField` — checkbox; missing value means false.
pub struct BooleanField {
    name: String,
    opts: CommonOpts,
}

impl BooleanField {
    pub fn new(name: impl Into<String>) -> Self {
        Self { name: name.into(), opts: CommonOpts { required: false, ..Default::default() } }
    }
}

impl Field for BooleanField {
    fn name(&self) -> &str { &self.name }
    fn label(&self) -> String { self.opts.label_of(&self.name) }
    fn required(&self) -> bool { self.opts.required }

    fn clean(&self, raw: Option<&str>) -> Result<Value, FieldErrors> {
        let on = matches!(raw, Some("on") | Some("true") | Some("True") | Some("1") | Some("yes"));
        if !on && self.required() {
            return Err(vec!["This field is required.".to_string()]);
        }
        Ok(Value::Bool(on))
    }

    fn render(&self, bound: Option<&str>, _error: Option<&str>) -> String {
        let checked = bound.map(|v| v == "on" || v == "true" || v == "1").unwrap_or(false);
        format!(
            "<input type=\"checkbox\" name=\"{}\" id=\"id_{}\"{}>",
            self.name,
            self.name,
            if checked { " checked" } else { "" }
        )
    }
}

/// `forms.DateField` — accepts ISO `YYYY-MM-DD` (plus a few common shapes).
pub struct DateField {
    name: String,
    opts: CommonOpts,
}

impl DateField {
    pub fn new(name: impl Into<String>) -> Self {
        Self { name: name.into(), opts: CommonOpts { required: true, ..Default::default() } }
    }
}

impl Field for DateField {
    fn name(&self) -> &str { &self.name }
    fn label(&self) -> String { self.opts.label_of(&self.name) }
    fn required(&self) -> bool { self.opts.required }

    fn clean(&self, raw: Option<&str>) -> Result<Value, FieldErrors> {
        let raw = raw.map(|s| s.trim()).unwrap_or("");
        if raw.is_empty() {
            if self.required() {
                return Err(vec!["This field is required.".to_string()]);
            }
            return Ok(Value::Null);
        }
        for fmt in ["%Y-%m-%d", "%d/%m/%Y", "%m/%d/%Y"] {
            if let Ok(d) = chrono::NaiveDate::parse_from_str(raw, fmt) {
                let dt = d.and_hms_opt(0, 0, 0).unwrap_or_default();
                return Ok(Value::DateTime(dt.and_utc()));
            }
        }
        Err(vec!["Enter a valid date.".to_string()])
    }

    fn render(&self, bound: Option<&str>, error: Option<&str>) -> String {
        let css = if error.is_some() { "form-control is-invalid" } else { "form-control" };
        input_html(&self.name, "date", bound.or(self.opts.initial.as_deref()), &self.opts.widget_attrs, css)
    }
}

/// `forms.ChoiceField` — select widget with choices validation.
pub struct ChoiceField {
    name: String,
    opts: CommonOpts,
    choices: Vec<(String, String)>,
}

impl ChoiceField {
    pub fn new(name: impl Into<String>, choices: Vec<(&str, &str)>) -> Self {
        Self {
            name: name.into(),
            opts: CommonOpts { required: true, ..Default::default() },
            choices: choices
                .into_iter()
                .map(|(v, l)| (v.to_string(), l.to_string()))
                .collect(),
        }
    }
}

impl Field for ChoiceField {
    fn name(&self) -> &str { &self.name }
    fn label(&self) -> String { self.opts.label_of(&self.name) }
    fn required(&self) -> bool { self.opts.required }

    fn clean(&self, raw: Option<&str>) -> Result<Value, FieldErrors> {
        let raw = raw.unwrap_or("");
        if raw.is_empty() {
            if self.required() {
                return Err(vec!["This field is required.".to_string()]);
            }
            return Ok(Value::Null);
        }
        if self.choices.iter().any(|(v, _)| v == raw) {
            Ok(Value::Text(raw.to_string()))
        } else {
            Err(vec!["Select a valid choice.".to_string()])
        }
    }

    fn render(&self, bound: Option<&str>, _error: Option<&str>) -> String {
        let mut out = format!("<select name=\"{}\" id=\"id_{}\" class=\"form-control\">", self.name, self.name);
        for (value, label) in &self.choices {
            let selected = bound.map(|b| b == value).unwrap_or(false);
            out.push_str(&format!(
                "<option value=\"{}\"{}>{}</option>",
                crate::templates::escape_html(value),
                if selected { " selected" } else { "" },
                crate::templates::escape_html(label)
            ));
        }
        out.push_str("</select>");
        out
    }
}

/// `forms.HiddenInput` plain field (e.g. for tokens).
pub struct HiddenField {
    name: String,
    initial: Option<String>,
    required: bool,
}

impl HiddenField {
    pub fn new(name: impl Into<String>) -> Self {
        Self { name: name.into(), initial: None, required: false }
    }
    pub fn initial(mut self, v: impl Into<String>) -> Self { self.initial = Some(v.into()); self }
}

impl Field for HiddenField {
    fn name(&self) -> &str { &self.name }
    fn label(&self) -> String { String::new() }
    fn required(&self) -> bool { self.required }

    fn clean(&self, raw: Option<&str>) -> Result<Value, FieldErrors> {
        Ok(match raw {
            Some(s) if !s.is_empty() => Value::Text(s.to_string()),
            _ => Value::Null,
        })
    }

    fn render(&self, bound: Option<&str>, _error: Option<&str>) -> String {
        let v = bound.or(self.initial.as_deref());
        input_html(&self.name, "hidden", v, &[], "")
    }
}

// ---------------------------------------------------------------------------
// Form
// ---------------------------------------------------------------------------

/// A bound/unbound form: field set + submitted data + validation state.
pub struct Form {
    pub prefix: String,
    fields: Vec<Box<dyn Field>>,
    data: QueryDict,
    bound: bool,
    cleaned: HashMap<String, Value>,
    errors: HashMap<String, FieldErrors>,
    validated: bool,
    valid: bool,
}

impl Form {
    /// Build an unbound form.
    pub fn new(prefix: impl Into<String>, fields: Vec<Box<dyn Field>>) -> Form {
        Form {
            prefix: prefix.into(),
            fields,
            data: QueryDict::new(),
            bound: false,
            cleaned: HashMap::new(),
            errors: HashMap::new(),
            validated: false,
            valid: false,
        }
    }

    /// Bind to submitted data — `MyForm(request.POST)`.
    pub fn bind(mut self, data: &QueryDict) -> Form {
        self.data = data.clone();
        self.bound = true;
        self
    }

    pub fn fields(&self) -> &[Box<dyn Field>] {
        &self.fields
    }

    pub fn is_bound(&self) -> bool { self.bound }

    /// Validate all fields — `form.is_valid()`.
    pub fn is_valid(&mut self) -> bool {
        if self.validated {
            return self.valid;
        }
        self.valid = true;
        self.errors.clear();
        self.cleaned.clear();
        for field in &self.fields {
            let raw = self.data.get(field.name());
            match field.clean(raw) {
                Ok(value) => {
                    self.cleaned.insert(field.name().to_string(), value);
                }
                Err(errs) => {
                    self.valid = false;
                    self.errors.insert(field.name().to_string(), errs);
                }
            }
        }
        self.validated = true;
        self.valid
    }

    /// `form.cleaned_data["name"]`.
    pub fn cleaned(&self, name: &str) -> Option<&Value> {
        self.cleaned.get(name)
    }

    pub fn cleaned_data(&self) -> &HashMap<String, Value> {
        &self.cleaned
    }

    /// `form.errors` — field name -> messages.
    pub fn errors(&self) -> &HashMap<String, FieldErrors> {
        &self.errors
    }

    /// Non-field error list (empty in this build; kept for API parity).
    pub fn non_field_errors(&self) -> FieldErrors {
        vec![]
    }

    /// Render every field wrapped in `<p>` — `form.as_p()`.
    pub fn as_p(&self) -> String {
        let mut out = String::new();
        for field in &self.fields {
            let bound = if self.bound { self.data.get(field.name()) } else { None };
            let error_html = if let Some(errs) = self.errors.get(field.name()) {
                let items: String = errs
                    .iter()
                    .map(|e| format!("<li>{}</li>", crate::templates::escape_html(e)))
                    .collect();
                format!("<ul class=\"errorlist\">{items}</ul>")
            } else {
                String::new()
            };
            let help = match field_label_help(field.as_ref()) {
                Some(h) => format!(" <span class=\"helptext\">{}</span>", crate::templates::escape_html(&h)),
                None => String::new(),
            };
            let label = format!(
                "<label for=\"id_{0}\">{1}</label>",
                field.name(),
                crate::templates::escape_html(&field.label())
            );
            out.push_str(&format!(
                "<p>{label} {} {help}{error_html}</p>",
                field.render(bound, self.errors.get(field.name()).map(|_| "")),
            ));
        }
        out
    }

    /// Render as a `<table>` — `form.as_table()`.
    pub fn as_table(&self) -> String {
        let mut out = String::new();
        for field in &self.fields {
            let bound = if self.bound { self.data.get(field.name()) } else { None };
            let error_html = if let Some(errs) = self.errors.get(field.name()) {
                let items: String = errs
                    .iter()
                    .map(|e| format!("<li>{}</li>", crate::templates::escape_html(e)))
                    .collect();
                format!("<ul class=\"errorlist\">{items}</ul>")
            } else {
                String::new()
            };
            out.push_str(&format!(
                "<tr><th><label for=\"id_{0}\">{1}</label></th><td>{2}{3}</td></tr>",
                field.name(),
                crate::templates::escape_html(&field.label()),
                field.render(bound, self.errors.get(field.name()).map(|_| "")),
                error_html,
            ));
        }
        out
    }

    /// Opening `<form>` tag (pair with `{% csrf_token %}`).
    pub fn open_tag(&self) -> String {
        "<form method=\"post\" action=\"\">".to_string()
    }

    pub fn close_tag(&self) -> String {
        "</form>".to_string()
    }
}

fn field_label_help(field: &dyn Field) -> Option<String> {
    // help_text is stored per concrete field; kept simple in this build.
    let _ = field;
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample_form() -> Form {
        Form::new(
            "contact",
            vec![
                Box::new(CharField::new("name").max_length(50)),
                Box::new(EmailField::new("email")),
                Box::new(IntegerField::new("age").min_value(1).max_value(120)),
                Box::new(BooleanField::new("subscribe")),
                Box::new(ChoiceField::new("color", vec![("red", "Red"), ("green", "Green")])),
            ],
        )
    }

    #[test]
    fn valid_submission() {
        let mut data = QueryDict::new();
        data.set("name", "Ada Lovelace");
        data.set("email", "ada@example.com");
        data.set("age", "36");
        data.set("color", "red");
        let form = sample_form().bind(&data);
        let mut bound = form;
        assert!(bound.is_valid());
        assert_eq!(bound.cleaned("name"), Some(&Value::Text("Ada Lovelace".into())));
        assert_eq!(bound.cleaned("age"), Some(&Value::Int(36)));
        assert_eq!(bound.cleaned("subscribe"), Some(&Value::Bool(false)));
    }

    #[test]
    fn invalid_email_and_age() {
        let mut data = QueryDict::new();
        data.set("name", "X");
        data.set("email", "not-an-email");
        data.set("age", "500");
        data.set("color", "purple");
        let mut bound = sample_form().bind(&data);
        assert!(!bound.is_valid());
        assert!(bound.errors().contains_key("email"));
        assert!(bound.errors().contains_key("age"));
        assert!(bound.errors().contains_key("color"));
        assert!(bound.errors()["email"][0].contains("valid email"));
        assert!(bound.errors()["age"][0].contains("less than or equal"));
    }

    #[test]
    fn required_fields_missing() {
        let data = QueryDict::new();
        let mut bound = sample_form().bind(&data);
        assert!(!bound.is_valid());
        assert!(bound.errors()["name"][0].contains("required"));
        assert!(bound.errors()["email"][0].contains("required"));
        // is_valid is cached
        assert!(!bound.is_valid());
    }

    #[test]
    fn char_max_length() {
        let mut data = QueryDict::new();
        data.set("name", "x".repeat(51));
        data.set("email", "a@b.co");
        data.set("age", "1");
        data.set("color", "red");
        let mut bound = sample_form().bind(&data);
        assert!(!bound.is_valid());
        assert!(bound.errors()["name"][0].contains("at most 50"));
    }

    #[test]
    fn unbound_form_renders() {
        let form = sample_form();
        let html = form.as_p();
        assert!(html.contains("name=\"name\""));
        assert!(html.contains("name=\"email\""));
        assert!(html.contains("<select"));
        assert!(!html.contains("errorlist"));
    }

    #[test]
    fn bound_form_renders_values_and_errors() {
        let mut data = QueryDict::new();
        data.set("name", "");
        data.set("email", "a@b.co");
        data.set("age", "30");
        data.set("color", "red");
        let mut bound = sample_form().bind(&data);
        bound.is_valid();
        let html = bound.as_p();
        assert!(html.contains("errorlist"));
        assert!(html.contains("value=\"a@b.co\""));
    }

    #[test]
    fn optional_and_textarea() {
        let form = Form::new(
            "bio",
            vec![
                Box::new(TextField("about")),
                Box::new(CharField::new("nick").optional()),
            ],
        );
        let data = QueryDict::new();
        let mut bound = form.bind(&data);
        assert!(bound.is_valid());
        assert_eq!(bound.cleaned("about"), Some(&Value::Null));
        assert_eq!(bound.cleaned("nick"), Some(&Value::Null));
    }

    #[test]
    fn date_field() {
        let form = Form::new("d", vec![Box::new(DateField::new("when"))]);
        let mut data = QueryDict::new();
        data.set("when", "2026-01-15");
        let mut bound = form.bind(&data);
        assert!(bound.is_valid());
        assert!(matches!(bound.cleaned("when"), Some(Value::DateTime(_))));

        let mut bad = QueryDict::new();
        bad.set("when", "not a date");
        let mut b2 = Form::new("d", vec![Box::new(DateField::new("when"))]).bind(&bad);
        assert!(!b2.is_valid());
    }

    #[test]
    fn slug_field() {
        let form = Form::new("s", vec![Box::new(SlugField::new("slug"))]);
        let mut ok = QueryDict::new();
        ok.set("slug", "my-post_1");
        let mut b = form.bind(&ok);
        assert!(b.is_valid());

        let mut bad = QueryDict::new();
        bad.set("slug", "bad slug!");
        let mut b2 = Form::new("s", vec![Box::new(SlugField::new("slug"))]).bind(&bad);
        assert!(!b2.is_valid());
    }

    #[test]
    fn integer_coercion_and_float() {
        let form = Form::new(
            "m",
            vec![Box::new(IntegerField::new("n")), Box::new(FloatField::new("f"))],
        );
        let mut data = QueryDict::new();
        data.set("n", " 42 ");
        data.set("f", "3.25");
        let mut bound = form.bind(&data);
        assert!(bound.is_valid());
        assert_eq!(bound.cleaned("n"), Some(&Value::Int(42)));
        assert_eq!(bound.cleaned("f"), Some(&Value::Float(3.25)));
    }

    #[test]
    fn hidden_field_and_table_render() {
        let form = Form::new(
            "h",
            vec![Box::new(HiddenField::new("token").initial("abc"))],
        );
        let html = form.as_table();
        assert!(html.contains("type=\"hidden\""));
        assert!(html.contains("value=\"abc\""));
    }

    #[test]
    fn as_table_shows_labels_not_errors() {
        // Regression: as_table() used to interpolate the error HTML into the
        // <label> slot instead of the field label.
        let form = Form::new("t", vec![Box::new(IntegerField::new("age"))]);
        let html = form.as_table();
        assert!(html.contains(">Age</label>"), "label missing: {html}");

        let mut data = QueryDict::new();
        data.set("age", "notanumber");
        let mut bound = form.bind(&data);
        bound.is_valid();
        let html = bound.as_table();
        assert!(html.contains(">Age</label>"));
        assert!(html.contains("errorlist"));
    }
}
