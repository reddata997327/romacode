//! `rustango-macros` — procedural macros for the rustango framework.
//!
//! The `#[model(...)]` attribute turns a plain Rust struct into a Django
//! model. Field kinds and options are declared with `#[field(...)]`
//! attributes; the Rust type drives storage conversion:
//!
//! ```ignore
//! #[rustango::model(table = "polls_question")]
//! pub struct Question {
//!     #[field(auto)]
//!     pub id: i64,
//!     #[field(char, max = 200)]
//!     pub question_text: String,
//!     #[field(datetime, auto_now_add)]
//!     pub pub_date: DateTime,
//! }
//! ```
//!
//! `#[field]` options:
//! * kind: `auto`, `char`, `text`, `int`, `float`, `bool`, `datetime`, `fk`
//! * `max = N` (char), `unique`, `null`, `default = expr`,
//!   `auto_now_add` (datetime), `target = "table"` (fk)
//!
//! Untagged fields infer their column kind from the Rust type
//! (`i64`/`String`/`f64`/`bool`/`DateTime`/`Option<DateTime>`).

use proc_macro::TokenStream;
use quote::quote;
use syn::parse_macro_input;
use syn::spanned::Spanned;
use syn::{ItemStruct, Token};
use syn::parse::Parse as _ParseTrait;

/// One `#[field(...)]` argument: `max = 150`, `unique`, `default = false`.
struct FieldArg {
    key: syn::Ident,
    value: Option<syn::Expr>,
}

impl syn::parse::Parse for FieldArg {
    fn parse(input: syn::parse::ParseStream) -> syn::Result<Self> {
        let key = input.parse::<syn::Ident>()?;
        if input.peek(Token![=]) {
            input.parse::<Token![=]>()?;
            let value = input.parse::<syn::Expr>()?;
            Ok(FieldArg { key, value: Some(value) })
        } else {
            Ok(FieldArg { key, value: None })
        }
    }
}

/// Parsed `#[field(...)]` content.
struct FieldOpts {
    kind: String,
    max: usize,
    unique: bool,
    null: bool,
    auto_now_add: bool,
    default: Option<syn::Expr>,
    target: Option<String>,
}

impl FieldOpts {
    fn parse_args(args: Vec<FieldArg>) -> Result<FieldOpts, syn::Error> {
        let mut o = FieldOpts {
            kind: String::new(),
            max: 0,
            unique: false,
            null: false,
            auto_now_add: false,
            default: None,
            target: None,
        };
        for a in args {
            match a.key.to_string().as_str() {
                "auto" => o.kind = "auto".into(),
                "char" | "varchar" => o.kind = "char".into(),
                "text" => o.kind = "text".into(),
                "int" | "integer" => o.kind = "int".into(),
                "float" => o.kind = "float".into(),
                "bool" | "boolean" => o.kind = "bool".into(),
                "datetime" => o.kind = "datetime".into(),
                "fk" | "foreign_key" => o.kind = "fk".into(),
                "max" => {
                    if let Some(syn::Expr::Lit(lit)) = &a.value {
                        if let syn::Lit::Int(li) = &lit.lit {
                            o.max = li.base10_parse().unwrap_or(0);
                        }
                    }
                }
                "unique" => o.unique = true,
                "null" => o.null = true,
                "auto_now_add" => o.auto_now_add = true,
                "default" => o.default = a.value.clone(),
                "target" => {
                    if let Some(syn::Expr::Lit(lit)) = &a.value {
                        if let syn::Lit::Str(s) = &lit.lit {
                            o.target = Some(s.value());
                        }
                    }
                }
                other => {
                    return Err(syn::Error::new(
                        a.key.span(),
                        format!("unknown #[field] option `{other}`"),
                    ))
                }
            }
        }
        Ok(o)
    }
}

/// Rust type tag used to drive row conversion.
#[derive(Clone, Copy)]
enum RustTy {
    Int,
    Float,
    Text,
    Bool,
    DateTime,
    OptInt,
    OptFloat,
    OptText,
    OptBool,
    OptDateTime,
}

impl RustTy {
    fn is_optional(self) -> bool {
        matches!(
            self,
            RustTy::OptInt | RustTy::OptFloat | RustTy::OptText | RustTy::OptBool | RustTy::OptDateTime
        )
    }
}

/// Final path-segment ident of the inner type of an `Option<...>` path,
/// e.g. `Option<DateTime>` -> `"DateTime"`, `Option<i64>` -> `"i64"`.
fn option_inner_ident(ty: &syn::Type) -> Option<String> {
    if let syn::Type::Path(p) = ty {
        if let Some(seg) = p.path.segments.first() {
            if seg.ident == "Option" {
                if let syn::PathArguments::AngleBracketed(args) = &seg.arguments {
                    if let Some(syn::GenericArgument::Type(inner)) = args.args.first() {
                        if let syn::Type::Path(ip) = inner {
                            if let Some(last) = ip.path.segments.last() {
                                return Some(last.ident.to_string());
                            }
                        }
                    }
                }
            }
        }
    }
    None
}

fn classify(ty: &syn::Type) -> RustTy {
    if let syn::Type::Path(p) = ty {
        let segs: Vec<&syn::PathSegment> = p.path.segments.iter().collect();
        if let Some(first) = segs.first() {
            if first.ident == "Option" {
                // Only inspect the INNER type: every `Option<T>` used to be
                // classified as `Option<DateTime>`, making `Option<i64>` /
                // `Option<String>` fields generate type errors buried in
                // the proc-macro expansion.
                let inner = option_inner_ident(ty).unwrap_or_default();
                return match inner.as_str() {
                    "DateTime" => RustTy::OptDateTime,
                    "i64" | "i32" => RustTy::OptInt,
                    "f64" | "f32" => RustTy::OptFloat,
                    "String" => RustTy::OptText,
                    "bool" => RustTy::OptBool,
                    _ => RustTy::OptText,
                };
            }
            match first.ident.to_string().as_str() {
                "i64" | "i32" => return RustTy::Int,
                "f64" | "f32" => return RustTy::Float,
                "String" => return RustTy::Text,
                "bool" => return RustTy::Bool,
                "DateTime" => return RustTy::DateTime,
                _ => {}
            }
        }
    }
    RustTy::Text
}

fn column_desc(
    name: &str,
    o: &FieldOpts,
    ty: RustTy,
) -> Result<proc_macro2::TokenStream, syn::Error> {
    let name_lit = name;
    let mut base = match o.kind.as_str() {
        "auto" => quote!(::rustango::orm::ColumnDesc::auto(#name_lit)),
        "char" => {
            if o.max == 0 {
                return Err(syn::Error::new(name.span(), "`char` fields need `max = N`"));
            }
            let max = o.max;
            quote!(::rustango::orm::ColumnDesc::char(#name_lit, #max))
        }
        "text" => quote!(::rustango::orm::ColumnDesc::text(#name_lit)),
        "int" => quote!(::rustango::orm::ColumnDesc::integer(#name_lit)),
        "float" => quote!(::rustango::orm::ColumnDesc::float(#name_lit)),
        "bool" => quote!(::rustango::orm::ColumnDesc::boolean(#name_lit, false)),
        "datetime" => {
            if o.auto_now_add {
                quote!(::rustango::orm::ColumnDesc::datetime_auto_now_add(#name_lit))
            } else {
                let null = o.null || matches!(ty, RustTy::OptDateTime);
                quote!(::rustango::orm::ColumnDesc::datetime(#name_lit, #null))
            }
        }
        "fk" => {
            let target = match &o.target {
                Some(t) => t.clone(),
                None => {
                    return Err(syn::Error::new(
                        name.span(),
                        "`fk` fields need `target = \"table_name\"`",
                    ))
                }
            };
            quote!(::rustango::orm::ColumnDesc::foreign_key(#name_lit, #target))
        }
        other => {
            return Err(syn::Error::new(name.span(), format!("unknown field kind `{other}`")))
        }
    };

    if o.unique {
        base = quote!(#base.unique());
    }
    // Optional field kinds are nullable columns, whatever the kind.
    if o.null || ty.is_optional() {
        base = quote!(#base.nullable());
    }
    if let Some(d) = &o.default {
        base = quote!({
            let mut c = #base;
            c.default = Some(::rustango::orm::Value::from(#d));
            c
        });
    }
    Ok(base)
}

fn from_expr(ty: RustTy) -> proc_macro2::TokenStream {
    let v = quote!(it.next().unwrap_or(&::rustango::orm::Value::Null));
    match ty {
        RustTy::Int => quote!(::rustango::orm::model::coerce_int(#v)),
        RustTy::Float => quote!(::rustango::orm::model::coerce_float(#v)),
        RustTy::Text => quote!(::rustango::orm::model::coerce_text(#v)),
        RustTy::Bool => quote!(::rustango::orm::model::coerce_bool(#v)),
        RustTy::DateTime => quote!(::rustango::orm::model::coerce_datetime(#v)),
        RustTy::OptInt => quote!(::rustango::orm::model::coerce_int_opt(#v)),
        RustTy::OptFloat => quote!(::rustango::orm::model::coerce_float_opt(#v)),
        RustTy::OptText => quote!(::rustango::orm::model::coerce_text_opt(#v)),
        RustTy::OptBool => quote!(::rustango::orm::model::coerce_bool_opt(#v)),
        RustTy::OptDateTime => quote!(::rustango::orm::model::coerce_datetime_opt(#v)),
    }
}

fn to_expr(name: &proc_macro2::Ident, ty: RustTy) -> proc_macro2::TokenStream {
    match ty {
        RustTy::Int => quote!(::rustango::orm::Value::Int(self.#name)),
        RustTy::Float => quote!(::rustango::orm::Value::Float(self.#name)),
        RustTy::Text => quote!(::rustango::orm::Value::Text(self.#name.clone())),
        RustTy::Bool => quote!(::rustango::orm::Value::Bool(self.#name)),
        RustTy::DateTime => quote!(::rustango::orm::Value::DateTime(self.#name)),
        RustTy::OptInt => quote!(self
            .#name
            .map(::rustango::orm::Value::Int)
            .unwrap_or(::rustango::orm::Value::Null)),
        RustTy::OptFloat => quote!(self
            .#name
            .map(::rustango::orm::Value::Float)
            .unwrap_or(::rustango::orm::Value::Null)),
        RustTy::OptText => quote!(self
            .#name
            .clone()
            .map(::rustango::orm::Value::Text)
            .unwrap_or(::rustango::orm::Value::Null)),
        RustTy::OptBool => quote!(self
            .#name
            .map(::rustango::orm::Value::Bool)
            .unwrap_or(::rustango::orm::Value::Null)),
        RustTy::OptDateTime => quote!(self
            .#name
            .map(::rustango::orm::Value::DateTime)
            .unwrap_or(::rustango::orm::Value::Null)),
    }
}

/// Extract `#[field(...)]` from a field, if present.
fn field_opts(f: &syn::Field) -> Result<Option<FieldOpts>, syn::Error> {
    let mut result = None;
    for attr in &f.attrs {
        if attr.path().is_ident("field") {
            let args: syn::punctuated::Punctuated<FieldArg, Token![,]> =
                attr.parse_args_with(|input: syn::parse::ParseStream| {
                    input.parse_terminated(FieldArg::parse, Token![,])
                })?;
            result = Some(FieldOpts::parse_args(args.into_iter().collect())?);
        }
    }
    Ok(result)
}

/// The `Model` derive macro (helper attributes: `#[model(...)]` on the
/// struct, `#[field(...)]` on fields).
#[proc_macro_derive(Model, attributes(model, field))]
pub fn model(input: TokenStream) -> TokenStream {
    let item_struct = parse_macro_input!(input as ItemStruct);
    let struct_name = &item_struct.ident;

    // Read `#[model(table = "name")]` from the struct attributes.
    let mut table_override: Option<String> = None;
    for attr in &item_struct.attrs {
        if attr.path().is_ident("model") {
            let args: syn::punctuated::Punctuated<FieldArg, Token![,]> =
                match attr.parse_args_with(|input: syn::parse::ParseStream| {
                    input.parse_terminated(FieldArg::parse, Token![,])
                }) {
                    Ok(a) => a,
                    Err(e) => return e.to_compile_error().into(),
                };
            for a in &args {
                if a.key == "table" {
                    if let Some(syn::Expr::Lit(lit)) = &a.value {
                        if let syn::Lit::Str(s) = &lit.lit {
                            table_override = Some(s.value());
                        }
                    }
                }
            }
        }
    }

    let table = table_override.unwrap_or_else(|| snake_case(&struct_name.to_string()));

    let mut col_exprs = Vec::new();
    let mut from_exprs = Vec::new();
    let mut to_exprs = Vec::new();
    let mut pk_field: Option<proc_macro2::Ident> = None;

    let fields = match &item_struct.fields {
        syn::Fields::Named(named) => &named.named,
        _ => {
            return syn::Error::new(
                item_struct.ident.span(),
                "only structs with named fields are supported",
            )
            .to_compile_error()
            .into()
        }
    };

    for f in fields {
        let fname = match &f.ident {
            Some(i) => i.clone(),
            None => continue,
        };
        let ty = classify(&f.ty);
        let opts = match field_opts(f) {
            Ok(o) => o,
            Err(e) => return e.to_compile_error().into(),
        };

        let eff_opts = match opts {
            Some(o) => o,
            None => FieldOpts {
                // Untagged: infer kind from the Rust type.
                kind: match ty {
                    RustTy::Int | RustTy::OptInt => "int",
                    RustTy::Float | RustTy::OptFloat => "float",
                    RustTy::Text | RustTy::OptText => "text",
                    RustTy::Bool | RustTy::OptBool => "bool",
                    RustTy::DateTime | RustTy::OptDateTime => "datetime",
                }
                .into(),
                max: 0,
                unique: false,
                null: ty.is_optional(),
                auto_now_add: false,
                default: None,
                target: None,
            },
        };

        if eff_opts.kind == "auto" {
            pk_field = Some(fname.clone());
        }
        match column_desc(&fname.to_string(), &eff_opts, ty) {
            Ok(c) => col_exprs.push(c),
            Err(e) => return e.to_compile_error().into(),
        }
        let fe = from_expr(ty);
        from_exprs.push(quote!(#fname: #fe,));
        let te = to_expr(&fname, ty);
        to_exprs.push(quote!(#te,));
    }

    let pk_ident = match pk_field {
        Some(p) => p,
        None => {
            return syn::Error::new(
                item_struct.ident.span(),
                "model needs one `#[field(auto)]` primary key field",
            )
            .to_compile_error()
            .into()
        }
    };

    // NOTE: derive macros keep the original struct in place (helper
    // attributes are stripped automatically), so we only emit impls here.
    let out_name = struct_name;

    let expanded = quote! {
        impl #out_name {
            /// Primary key value.
            pub fn pk(&self) -> i64 { self.#pk_ident }
            /// Set the primary key (used after INSERT).
            pub fn set_pk(&mut self, id: i64) { self.#pk_ident = id; }
        }

        impl ::rustango::orm::Model for #out_name {
            fn table() -> &'static str { #table }

            fn columns() -> &'static [::rustango::orm::ColumnDesc] {
                use std::sync::OnceLock;
                static COLS: OnceLock<Vec<::rustango::orm::ColumnDesc>> = OnceLock::new();
                COLS.get_or_init(|| vec![
                    #(#col_exprs),*
                ])
            }

            fn from_row(row: &[::rustango::orm::Value]) -> Self {
                #[allow(unused_mut)]
                let mut it = row.iter();
                #out_name {
                    #(#from_exprs)*
                }
            }

            fn to_row(&self) -> Vec<::rustango::orm::Value> {
                vec![
                    #(#to_exprs)*
                ]
            }

            fn pk(&self) -> i64 { self.#pk_ident }

            fn set_pk(&mut self, id: i64) { self.#pk_ident = id; }
        }
    };

    if std::env::var("RUSTANGO_MACRO_DEBUG").is_ok() {
        eprintln!("=== #[model] expansion for {} ===\n{}", struct_name, expanded);
    }
    expanded.into()
}

fn snake_case(name: &str) -> String {
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
