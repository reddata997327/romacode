//! Template resolution for composition tools. A step's input template is a
//! JSON value whose string leaves may reference:
//!
//! * `{{prev}}` — the previous step's full output
//! * `{{input}}` — the composition's original input
//! * `{{prev.a.b}}` / `{{input.a}}` — a dot-path into either
//!
//! Whole-string templates are replaced by the referenced value (any type);
//! templates embedded in larger strings are string-substituted (result stays
//! a string). Objects and arrays are recursed.

use rc_core::tool::ToolSourceKind;
use serde_json::Value;

/// Walk a dot-path (`a.b.0.c`) into a JSON value. Numbers index arrays.
pub fn lookup(root: &Value, path: &str) -> Value {
    let mut cur = root;
    for seg in path.split('.') {
        if seg.is_empty() {
            continue;
        }
        cur = match cur {
            Value::Object(map) => map.get(seg).unwrap_or(&Value::Null),
            Value::Array(arr) => seg
                .parse::<usize>()
                .ok()
                .and_then(|i| arr.get(i))
                .unwrap_or(&Value::Null),
            _ => &Value::Null,
        };
    }
    cur.clone()
}

/// Resolve a template value against `prev` (previous step output) and `input`
/// (the composition's original input).
pub fn resolve(template: &Value, prev: &Value, input: &Value) -> Value {
    let ctx = serde_json::json!({ "prev": prev, "input": input });
    resolve_ctx(template, &ctx)
}

/// General resolver: any `{{dot.path}}` is looked up against `ctx`. This is
/// what the DAG uses, with `ctx = {"input": …, "nodes": {name: output, …}}`.
pub fn resolve_ctx(template: &Value, ctx: &Value) -> Value {
    match template {
        Value::String(s) => resolve_string_ctx(s, ctx),
        Value::Object(map) => {
            let mut out = serde_json::Map::new();
            for (k, v) in map {
                out.insert(k.clone(), resolve_ctx(v, ctx));
            }
            Value::Object(out)
        }
        Value::Array(arr) => Value::Array(
            arr.iter().map(|v| resolve_ctx(v, ctx)).collect(),
        ),
        other => other.clone(),
    }
}

/// Whole-string `{{path}}` → the looked-up value (any type); embedded
/// templates → string substitution.
fn resolve_string_ctx(s: &str, ctx: &Value) -> Value {
    if let Some(inner) = s.strip_prefix("{{").and_then(|x| x.strip_suffix("}}")) {
        if !inner.contains('{') && !inner.contains(' ') {
            return lookup(ctx, inner.trim());
        }
    }
    if s.contains("{{") {
        return Value::String(substitute_all(s, ctx));
    }
    Value::String(s.to_string())
}

/// Replace every `{{path}}` in `s` with the stringified value at `ctx.path`.
/// Walks the string by whole UTF-8 characters — the old byte-at-a-time loop
/// rendered every non-ASCII character outside a `{{…}}` span as mojibake.
fn substitute_all(s: &str, ctx: &Value) -> String {
    let bytes = s.as_bytes();
    let mut out = String::with_capacity(s.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'{' && i + 1 < bytes.len() && bytes[i + 1] == b'{' {
            if let Some(end) = s[i + 2..].find("}}") {
                let path = &s[i + 2..i + 2 + end];
                let val = lookup(ctx, path.trim());
                match &val {
                    Value::String(t) => out.push_str(t),
                    Value::Null => {}
                    other => out.push_str(&other.to_string()),
                }
                i = i + 2 + end + 2;
                continue;
            }
        }
        // Push the whole character (it may be multi-byte), not the raw byte.
        let ch = s[i..].chars().next().unwrap_or('\u{FFFD}');
        out.push(ch);
        i += ch.len_utf8();
    }
    out
}

/// Source-kind tag for a composition variant.
pub fn compose_kind(variant: &'static str) -> ToolSourceKind {
    ToolSourceKind::Compose(variant)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn whole_string_templates() {
        let prev = serde_json::json!({ "sum": 7 });
        let input = serde_json::json!({ "x": 5 });
        assert_eq!(
            resolve(&serde_json::json!("{{prev}}"), &prev, &input),
            serde_json::json!({ "sum": 7 })
        );
        assert_eq!(
            resolve(&serde_json::json!("{{prev.sum}}"), &prev, &input),
            serde_json::json!(7)
        );
        assert_eq!(
            resolve(&serde_json::json!("{{input.x}}"), &prev, &input),
            serde_json::json!(5)
        );
    }

    #[test]
    fn embedded_templates() {
        let prev = serde_json::json!({ "name": "ada" });
        let input = serde_json::json!({ "greeting": "hi" });
        let t = serde_json::json!("{{input.greeting}}, {{prev.name}}!");
        assert_eq!(
            resolve(&t, &prev, &input),
            serde_json::json!("hi, ada!")
        );
    }

    #[test]
    fn recurses_objects_and_arrays() {
        let prev = serde_json::json!(42);
        let input = serde_json::json!({ "a": 1 });
        let t = serde_json::json!({ "from_prev": "{{prev}}", "list": ["{{input.a}}", "lit"] });
        let r = resolve(&t, &prev, &input);
        assert_eq!(r["from_prev"], 42);
        assert_eq!(r["list"][0], 1);
        assert_eq!(r["list"][1], "lit");
    }

    #[test]
    fn array_index_lookup() {
        let prev = serde_json::json!([10, 20, 30]);
        assert_eq!(lookup(&prev, "1"), serde_json::json!(20));
        assert_eq!(lookup(&prev, "5"), serde_json::Value::Null);
    }

    #[test]
    fn non_ascii_text_survives_substitution() {
        // The old byte-at-a-time loop turned é/ü/日 into mojibake outside the
        // template spans; the fixed loop pushes whole characters.
        let prev = serde_json::json!({ "name": "ada" });
        let input = serde_json::json!({});
        let t = serde_json::json!("héllo {{prev.name}} — ünïcode 日本語!");
        assert_eq!(
            resolve(&t, &prev, &input),
            serde_json::json!("héllo ada — ünïcode 日本語!")
        );
        // Missing lookup right after a multi-byte char still renders cleanly.
        let t = serde_json::json!("héllo {{missing}}!");
        assert_eq!(resolve(&t, &prev, &input), serde_json::json!("héllo !"));
        // Template with a value containing non-ASCII text.
        let prev = serde_json::json!("Grüße");
        let t = serde_json::json!("{{prev}}, wörld");
        assert_eq!(resolve(&t, &prev, &input), serde_json::json!("Grüße, wörld"));
    }
}
