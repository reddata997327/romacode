//! Built-in template filters — the `django.template.defaultfilters` set.

use std::collections::HashMap;
use std::sync::OnceLock;

use super::parse::ValueExpr;
use super::{Context, TplValue, escape_html};

type FilterFn = fn(TplValue, Option<TplValue>, &Context) -> Result<TplValue, String>;

/// Registry of built-in filters (static function pointers — zero overhead).
fn registry() -> &'static HashMap<&'static str, FilterFn> {
    static REG: OnceLock<HashMap<&'static str, FilterFn>> = OnceLock::new();
    REG.get_or_init(|| {
        let mut m: HashMap<&'static str, FilterFn> = HashMap::new();
        m.insert("lower", lower);
        m.insert("upper", upper);
        m.insert("capitalize", capitalize);
        m.insert("title", title);
        m.insert("trim", trim);
        m.insert("cut", cut);
        m.insert("slugify", slugify);
        m.insert("escape", escape);
        m.insert("safe", safe);
        m.insert("striptags", striptags);
        m.insert("length", length);
        m.insert("join", join);
        m.insert("default", default);
        m.insert("default_if_none", default_if_none);
        m.insert("truncatechars", truncatechars);
        m.insert("truncatewords", truncatewords);
        m.insert("first", first);
        m.insert("last", last);
        m.insert("add", add);
        m.insert("yesno", yesno);
        m.insert("date", date);
        m.insert("floatformat", floatformat);
        m.insert("linebreaks", linebreaks);
        m.insert("center", center);
        m.insert("lookup", lookup);
        m
    })
}

/// Apply a filter by name; unknown filters return the value unchanged
/// (lenient, like Django raises TemplateSyntaxError — we log-friendly fail).
pub fn apply_filter(
    name: &str,
    value: TplValue,
    arg: Option<TplValue>,
    ctx: &Context,
) -> Result<TplValue, String> {
    match registry().get(name) {
        Some(f) => f(value, arg, ctx),
        None => Err(format!("unknown filter `{name}`")),
    }
}

// -- text filters -------------------------------------------------------------

/// Rebuild a text value while preserving the `Safe` marker. Case/trim
/// transformations cannot introduce markup, so demoting a `Safe` value to
/// `Str` here would cause it to be re-escaped on render.
fn map_text(v: TplValue, f: impl Fn(&str) -> String) -> TplValue {
    match v {
        TplValue::Safe(s) => TplValue::Safe(f(&s)),
        other => TplValue::Str(f(&other.as_str())),
    }
}

fn lower(v: TplValue, _a: Option<TplValue>, _c: &Context) -> Result<TplValue, String> {
    Ok(map_text(v, |s| s.to_lowercase()))
}

fn upper(v: TplValue, _a: Option<TplValue>, _c: &Context) -> Result<TplValue, String> {
    Ok(map_text(v, |s| s.to_uppercase()))
}

fn capitalize(v: TplValue, _a: Option<TplValue>, _c: &Context) -> Result<TplValue, String> {
    Ok(map_text(v, |s| {
        let mut chars = s.chars();
        match chars.next() {
            Some(first) => first.to_uppercase().collect::<String>() + &chars.as_str().to_lowercase(),
            None => s.to_string(),
        }
    }))
}

fn title(v: TplValue, _a: Option<TplValue>, _c: &Context) -> Result<TplValue, String> {
    Ok(map_text(v, |s| {
        s.split_whitespace()
            .map(|word| {
                let mut chars = word.chars();
                match chars.next() {
                    Some(first) => {
                        first.to_uppercase().collect::<String>() + &chars.as_str().to_lowercase()
                    }
                    None => String::new(),
                }
            })
            .collect::<Vec<_>>()
            .join(" ")
    }))
}

fn trim(v: TplValue, _a: Option<TplValue>, _c: &Context) -> Result<TplValue, String> {
    Ok(map_text(v, |s| s.trim().to_string()))
}

fn cut(v: TplValue, a: Option<TplValue>, _c: &Context) -> Result<TplValue, String> {
    let needle = a.map(|x| x.as_str()).unwrap_or_default();
    Ok(TplValue::Str(v.as_str().replace(&needle, "")))
}

fn slugify(v: TplValue, _a: Option<TplValue>, _c: &Context) -> Result<TplValue, String> {
    let s = v.as_str().to_lowercase();
    let mut out = String::with_capacity(s.len());
    let mut last_dash = false;
    for ch in s.chars() {
        if ch.is_ascii_alphanumeric() {
            out.push(ch);
            last_dash = false;
        } else if !last_dash && !out.is_empty() {
            out.push('-');
            last_dash = true;
        }
    }
    while out.ends_with('-') {
        out.pop();
    }
    Ok(TplValue::Str(out))
}

fn escape(v: TplValue, _a: Option<TplValue>, _c: &Context) -> Result<TplValue, String> {
    match v {
        // Already-safe content must not be escaped twice:
        // `{{ x|escape|upper }}` double-escaped before this fix.
        TplValue::Safe(s) => Ok(TplValue::Safe(s)),
        other => Ok(TplValue::Safe(escape_html(&other.as_str()))),
    }
}

fn safe(v: TplValue, _a: Option<TplValue>, _c: &Context) -> Result<TplValue, String> {
    Ok(TplValue::Safe(v.as_str()))
}

fn striptags(v: TplValue, _a: Option<TplValue>, _c: &Context) -> Result<TplValue, String> {
    let s = v.as_str();
    let mut out = String::with_capacity(s.len());
    let mut in_tag = false;
    for ch in s.chars() {
        match ch {
            '<' => in_tag = true,
            '>' => in_tag = false,
            c if !in_tag => out.push(c),
            _ => {}
        }
    }
    Ok(TplValue::Str(out))
}

// -- collection filters --------------------------------------------------------

fn length(v: TplValue, _a: Option<TplValue>, _c: &Context) -> Result<TplValue, String> {
    let n = match v {
        TplValue::List(items) => items.len(),
        TplValue::Map(m) => m.len(),
        TplValue::Str(s) | TplValue::Safe(s) => s.chars().count(),
        TplValue::Null => 0,
        _ => 1,
    };
    Ok(TplValue::Int(n as i64))
}

fn join(v: TplValue, a: Option<TplValue>, _c: &Context) -> Result<TplValue, String> {
    let sep = a.map(|x| x.as_str()).unwrap_or_else(|| ", ".to_string());
    match v {
        TplValue::List(items) => {
            let parts: Vec<String> = items.iter().map(|x| x.as_str()).collect();
            Ok(TplValue::Str(parts.join(&sep)))
        }
        other => Ok(TplValue::Str(other.as_str())),
    }
}

fn first(v: TplValue, _a: Option<TplValue>, _c: &Context) -> Result<TplValue, String> {
    match v {
        TplValue::List(items) => Ok(items.first().cloned().unwrap_or(TplValue::Null)),
        TplValue::Str(s) => Ok(s.chars().next().map(|c| TplValue::Str(c.to_string())).unwrap_or(TplValue::Null)),
        _ => Ok(TplValue::Null),
    }
}

fn last(v: TplValue, _a: Option<TplValue>, _c: &Context) -> Result<TplValue, String> {
    match v {
        TplValue::List(items) => Ok(items.last().cloned().unwrap_or(TplValue::Null)),
        TplValue::Str(s) => Ok(s.chars().last().map(|c| TplValue::Str(c.to_string())).unwrap_or(TplValue::Null)),
        _ => Ok(TplValue::Null),
    }
}

// -- default / fallback ---------------------------------------------------------

fn default(v: TplValue, a: Option<TplValue>, _c: &Context) -> Result<TplValue, String> {
    if v.is_truthy() {
        Ok(v)
    } else {
        Ok(a.unwrap_or(TplValue::Null))
    }
}

fn default_if_none(v: TplValue, a: Option<TplValue>, _c: &Context) -> Result<TplValue, String> {
    match v {
        TplValue::Null => Ok(a.unwrap_or(TplValue::Null)),
        other => Ok(other),
    }
}

// -- truncation -------------------------------------------------------------------

fn truncatechars(v: TplValue, a: Option<TplValue>, _c: &Context) -> Result<TplValue, String> {
    let n = a.and_then(|x| x.as_int()).unwrap_or(30).max(3) as usize;
    let s = v.as_str();
    if s.chars().count() <= n {
        return Ok(TplValue::Str(s));
    }
    let cut: String = s.chars().take(n - 3).collect();
    Ok(TplValue::Str(format!("{cut}...")))
}

fn truncatewords(v: TplValue, a: Option<TplValue>, _c: &Context) -> Result<TplValue, String> {
    let n = a.and_then(|x| x.as_int()).unwrap_or(2).max(1) as usize;
    let s = v.as_str();
    let words: Vec<&str> = s.split_whitespace().collect();
    if words.len() <= n {
        return Ok(TplValue::Str(s));
    }
    Ok(TplValue::Str(format!("{}...", words[..n].join(" "))))
}

// -- math -------------------------------------------------------------------------

fn add(v: TplValue, a: Option<TplValue>, _c: &Context) -> Result<TplValue, String> {
    match a {
        Some(x) => {
            if let (Some(l), Some(r)) = (numeric(&v), numeric(&x)) {
                if let (TplValue::Int(_), TplValue::Int(_)) = (&v, &x) {
                    Ok(TplValue::Int((l + r) as i64))
                } else {
                    Ok(TplValue::Float(l + r))
                }
            } else {
                Ok(TplValue::Str(format!("{}{}", v.as_str(), x.as_str())))
            }
        }
        None => Ok(v),
    }
}

fn numeric(v: &TplValue) -> Option<f64> {
    match v {
        TplValue::Int(i) => Some(*i as f64),
        TplValue::Float(f) => Some(*f),
        TplValue::Str(s) => s.trim().parse().ok(),
        TplValue::Bool(b) => Some(if *b { 1.0 } else { 0.0 }),
        _ => None,
    }
}

fn floatformat(v: TplValue, a: Option<TplValue>, _c: &Context) -> Result<TplValue, String> {
    let places = a.and_then(|x| x.as_int()).unwrap_or(1).max(0) as usize;
    if let Some(f) = numeric(&v) {
        Ok(TplValue::Str(format!("{f:.places$}")))
    } else {
        Ok(TplValue::Str(v.as_str()))
    }
}

// -- misc ---------------------------------------------------------------------------

fn yesno(v: TplValue, a: Option<TplValue>, _c: &Context) -> Result<TplValue, String> {
    // "yes,no,maybe" — maybe optional
    let spec = a.map(|x| x.as_str()).unwrap_or_else(|| "yes,no".to_string());
    let parts: Vec<&str> = spec.split(',').collect();
    let truthy = v.is_truthy();
    let val = if truthy {
        parts.first().copied().unwrap_or("yes")
    } else if let TplValue::Null = v {
        parts.get(2).copied().unwrap_or("no")
    } else {
        parts.get(1).copied().unwrap_or("no")
    };
    Ok(TplValue::Str(val.to_string()))
}

fn date(v: TplValue, a: Option<TplValue>, _c: &Context) -> Result<TplValue, String> {
    match v {
        TplValue::DateTime(d) => {
            let fmt = a.map(|x| x.as_str()).unwrap_or_else(|| "N j, Y, f".to_string());
            Ok(TplValue::Str(django_date_format(&d, &fmt)))
        }
        other => Ok(TplValue::Str(other.as_str())),
    }
}

/// Convert a subset of Django's date format to a string.
/// Supported: Y y m n d j D l H i s M, and literal chars (\'s escaped).
pub fn django_date_format(d: &chrono::DateTime<chrono::Utc>, fmt: &str) -> String {
    let mut out = String::new();
    let chars: Vec<char> = fmt.chars().collect();
    let mut i = 0;
    while i < chars.len() {
        let c = chars[i];
        let piece = match c {
            'Y' => d.format("%Y").to_string(),
            'y' => d.format("%y").to_string(),
            'm' => d.format("%m").to_string(),
            'n' => d.format("%-m").to_string(),
            'd' => d.format("%d").to_string(),
            'j' => d.format("%-d").to_string(),
            'H' => d.format("%H").to_string(),
            'i' => d.format("%M").to_string(),
            's' => d.format("%S").to_string(),
            'M' => d.format("%b").to_string(),
            'D' => d.format("%a").to_string(),
            'l' => d.format("%A").to_string(),
            '\\' if i + 1 < chars.len() => {
                out.push(chars[i + 1]);
                i += 2;
                continue;
            }
            other => {
                out.push(other);
                i += 1;
                continue;
            }
        };
        out.push_str(&piece);
        i += 1;
    }
    out
}

fn linebreaks(v: TplValue, _a: Option<TplValue>, _c: &Context) -> Result<TplValue, String> {
    let s = v.as_str();
    Ok(TplValue::Safe(
        s.split("\n\n")
            .map(|para| format!("<p>{}</p>", escape_html(&para.replace('\n', "<br>"))))
            .collect::<Vec<_>>()
            .join(""),
    ))
}

/// Dynamic key/index lookup: `{{ row|lookup:column }}` (admin changelist).
fn lookup(v: TplValue, a: Option<TplValue>, _c: &Context) -> Result<TplValue, String> {
    let key = a.ok_or_else(|| "lookup needs a key".to_string())?;
    match v {
        TplValue::Map(m) => {
            let k = key.as_str();
            Ok(m.get(&k).cloned().unwrap_or(TplValue::Null))
        }
        TplValue::List(items) => {
            let idx = key.as_int().unwrap_or(0).max(0) as usize;
            Ok(items.get(idx).cloned().unwrap_or(TplValue::Null))
        }
        TplValue::Null => Ok(TplValue::Null),
        other => Ok(TplValue::Str(other.as_str())),
    }
}

fn center(v: TplValue, a: Option<TplValue>, _c: &Context) -> Result<TplValue, String> {
    let width = a.and_then(|x| x.as_int()).unwrap_or(0);
    let s = v.as_str();
    let len = s.chars().count();
    // Non-positive widths (negative used to wrap to usize::MAX and abort on
    // `" ".repeat(huge)`) and widths <= len pad nothing.
    if width <= 0 || (width as usize) <= len {
        return Ok(TplValue::Str(s));
    }
    let width = width as usize;
    let total = width - len; // safe: width > len checked above
    let left = total / 2;
    let right = total - left;
    Ok(TplValue::Str(format!("{}{}{}", " ".repeat(left), s, " ".repeat(right))))
}

/// Re-export for parse.rs' ValueExpr docs.
pub type FilterArg = ValueExpr;

#[cfg(test)]
mod tests {
    use super::*;

    fn ctx() -> Context {
        Context::new()
    }

    #[test]
    fn text_filters() {
        let c = ctx();
        assert_eq!(
            apply_filter("upper", TplValue::Str("abc".into()), None, &c).unwrap().as_str(),
            "ABC"
        );
        assert_eq!(
            apply_filter("capitalize", TplValue::Str("hello WORLD".into()), None, &c)
                .unwrap()
                .as_str(),
            "Hello world"
        );
        assert_eq!(
            apply_filter("title", TplValue::Str("the rust book".into()), None, &c)
                .unwrap()
                .as_str(),
            "The Rust Book"
        );
        assert_eq!(
            apply_filter("slugify", TplValue::Str("Rust & Django!".into()), None, &c)
                .unwrap()
                .as_str(),
            "rust-django"
        );
        assert_eq!(
            apply_filter("cut", TplValue::Str("a-b-c".into()), Some(TplValue::Str("-".into())), &c)
                .unwrap()
                .as_str(),
            "abc"
        );
    }

    #[test]
    fn escape_and_safe() {
        let c = ctx();
        let v = TplValue::Str("<b>".into());
        assert_eq!(
            apply_filter("escape", v.clone(), None, &c).unwrap(),
            TplValue::Safe("&lt;b&gt;".into())
        );
        assert_eq!(
            apply_filter("safe", v, None, &c).unwrap(),
            TplValue::Safe("<b>".into())
        );
    }

    #[test]
    fn collection_filters() {
        let c = ctx();
        let list = TplValue::List(vec![
            TplValue::Str("a".into()),
            TplValue::Str("b".into()),
            TplValue::Str("c".into()),
        ]);
        assert_eq!(
            apply_filter("length", list.clone(), None, &c).unwrap(),
            TplValue::Int(3)
        );
        assert_eq!(
            apply_filter("first", list.clone(), None, &c).unwrap().as_str(),
            "a"
        );
        assert_eq!(
            apply_filter("last", list.clone(), None, &c).unwrap().as_str(),
            "c"
        );
        assert_eq!(
            apply_filter("join", list, Some(TplValue::Str("+".into())), &c).unwrap().as_str(),
            "a+b+c"
        );
    }

    #[test]
    fn math_and_defaults() {
        let c = ctx();
        assert_eq!(
            apply_filter("add", TplValue::Int(4), Some(TplValue::Int(3)), &c).unwrap(),
            TplValue::Int(7)
        );
        assert_eq!(
            apply_filter("add", TplValue::Float(1.5), Some(TplValue::Int(1)), &c)
                .unwrap()
                .as_str(),
            "2.5"
        );
        assert_eq!(
            apply_filter("default", TplValue::Null, Some(TplValue::Str("x".into())), &c)
                .unwrap()
                .as_str(),
            "x"
        );
        assert_eq!(
            apply_filter("default", TplValue::Int(5), Some(TplValue::Str("x".into())), &c)
                .unwrap(),
            TplValue::Int(5)
        );
        assert_eq!(
            apply_filter("yesno", TplValue::Bool(false), Some(TplValue::Str("on,off".into())), &c)
                .unwrap()
                .as_str(),
            "off"
        );
    }

    #[test]
    fn truncation() {
        let c = ctx();
        assert_eq!(
            apply_filter(
                "truncatechars",
                TplValue::Str("abcdefghij".into()),
                Some(TplValue::Int(7)),
                &c
            )
            .unwrap()
            .as_str(),
            "abcd..."
        );
        assert_eq!(
            apply_filter(
                "truncatewords",
                TplValue::Str("one two three four".into()),
                Some(TplValue::Int(2)),
                &c
            )
            .unwrap()
            .as_str(),
            "one two..."
        );
    }

    #[test]
    fn lookup_filter() {
        let c = ctx();
        let mut m = std::collections::HashMap::new();
        m.insert("name".to_string(), TplValue::Str("ada".into()));
        let row = TplValue::Map(std::sync::Arc::new(m));
        assert_eq!(
            apply_filter("lookup", row, Some(TplValue::Str("name".into())), &c)
                .unwrap()
                .as_str(),
            "ada"
        );
        assert_eq!(
            apply_filter("lookup", TplValue::Null, Some(TplValue::Str("x".into())), &c)
                .unwrap(),
            TplValue::Null
        );
    }

    #[test]
    fn unknown_filter_is_error() {
        let c = ctx();
        assert!(apply_filter("nope", TplValue::Int(1), None, &c).is_err());
    }

    #[test]
    fn center_negative_width_does_not_panic() {
        // Regression: a negative width wrapped to usize::MAX, making
        // `" ".repeat(huge)` abort the process.
        let c = ctx();
        let out = apply_filter(
            "center",
            TplValue::Str("ab".into()),
            Some(TplValue::Int(-5)),
            &c,
        )
        .unwrap();
        assert_eq!(out.as_str(), "ab");
        // Sanity: a positive width still pads (odd remainder goes right).
        let out = apply_filter(
            "center",
            TplValue::Str("ab".into()),
            Some(TplValue::Int(5)),
            &c,
        )
        .unwrap();
        assert_eq!(out.as_str(), " ab  ");
    }

    #[test]
    fn safe_marker_survives_case_trim_and_escape() {
        let c = ctx();
        // `escape` on an already-Safe value is a no-op (no double escaping).
        let safe_html = TplValue::Safe("&lt;b&gt;".into());
        assert_eq!(
            apply_filter("escape", safe_html.clone(), None, &c).unwrap(),
            TplValue::Safe("&lt;b&gt;".into())
        );
        // Case/trim filters cannot introduce markup — they must preserve
        // the Safe marker so the value is not re-escaped on render.
        for (name, arg) in [
            ("upper", None),
            ("lower", None),
            ("capitalize", None),
            ("title", None),
            ("trim", None),
        ] {
            let out = apply_filter(name, safe_html.clone(), arg, &c).unwrap();
            assert!(matches!(out, TplValue::Safe(_)), "{name} demoted Safe to Str");
        }
        // And the classic chain no longer double-escapes.
        let out = apply_filter(
            "upper",
            apply_filter("escape", TplValue::Str("<b>x</b>".into()), None, &c).unwrap(),
            None,
            &c,
        )
        .unwrap();
        assert_eq!(out.as_str(), "&LT;B&GT;X&LT;/B&GT;", "must not be double-escaped");
    }
}
