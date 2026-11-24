//! # rc-builtin — the built-in tool catalog
//!
//! Eight categories of representative tools, every one a `rc_core::Tool`:
//!
//! | category | tools | default permission |
//! |----------|-------|--------------------|
//! | `math`   | add, subtract, multiply, divide, sqrt, factorial, gcd | allow |
//! | `text`   | upper, lower, length, reverse, trim, replace, split, join, contains | allow |
//! | `data`   | json_path, json_pretty, json_minify, sort_array, dedupe | allow |
//! | `code`   | base64_encode, base64_decode, parse_json, eval_expr | allow |
//! | `time`   | now, iso_now, sleep | allow |
//! | `file`   | read, write, list, exists | ask (FS) |
//! | `web`    | fetch, url_encode, url_decode | ask (net) |
//! | `system` | env, hostname, cwd | deny (host info) |
//!
//! [`all_tools`] returns every built-in as a `Vec<ToolArc>`, ready to register
//! in a [`Registry`](rc_core::Registry). This is the "source 1: built-in"
//! mechanism — the others live in `rc-sources`.

pub mod code;
pub mod data;
pub mod file;
pub mod math;
pub mod system;
pub mod text;
pub mod time;
pub mod web;

use rc_core::error::{ToolError, ToolResult};
use rc_core::schema::ParamType;
use rc_core::tool::ToolArc;
use serde_json::Value;

/// Pull a required parameter out of (schema-validated) tool input, returning
/// a proper [`ToolError`] instead of panicking when it is missing or of an
/// unexpected shape. Handlers must convert through this helper rather than
/// calling `.unwrap()` on `as_i64()`/`as_str()`/…: the validator accepts the
/// full u64 range for `Integer`, so e.g. `18446744073709551615` passes
/// validation but `as_i64()` returns `None`.
pub(crate) fn need_param<'a, T, F>(
    input: &'a Value,
    name: &str,
    expected: ParamType,
    f: F,
) -> ToolResult<T>
where
    F: FnOnce(&'a Value) -> Option<T>,
{
    f(&input[name]).ok_or_else(|| {
        let detail = format!("missing or not a {} (got {})", expected, input[name]);
        ToolError::BadParam {
            name: name.to_string(),
            expected,
            detail,
        }
    })
}

/// Every built-in tool, across all 8 categories, ready to register.
pub fn all_tools() -> Vec<ToolArc> {
    let mut v = Vec::new();
    v.extend(math::all());
    v.extend(text::all());
    v.extend(data::all());
    v.extend(code::all());
    v.extend(file::all());
    v.extend(web::all());
    v.extend(system::all());
    v.extend(time::all());
    // dsd.* — Domain→Server→Database live api tracing (added as a 9th
    // category; COUNT below stays the sum of the 8 legacy categories).
    v.extend(rc_dsd::tools::all());
    v
}

/// The number of built-in tools (for `rc sources builtin`).
pub const COUNT: usize = 7 + 9 + 5 + 4 + 4 + 3 + 3 + 3 + 4; // 42 (incl. dsd)

/// The category list (for `rc sources builtin`).
pub const CATEGORIES: &[&str] = &[
    "math", "text", "data", "code", "file", "web", "system", "time", "dsd",
];

#[cfg(test)]
mod tests {
    use super::*;
    use rc_core::Registry;

    #[test]
    fn all_tools_register_uniquely() {
        let tools = all_tools();
        assert_eq!(tools.len(), COUNT);
        // No duplicate names.
        let mut names: Vec<String> = tools.iter().map(|t| t.name().to_string()).collect();
        names.sort();
        let mut deduped = names.clone();
        deduped.dedup();
        assert_eq!(names.len(), deduped.len(), "duplicate tool names: {names:?}");
        // They all register.
        let r = Registry::new();
        r.register_all(tools);
        assert_eq!(r.len(), COUNT);
    }

    #[test]
    fn categories_cover_all_tools() {
        let tools = all_tools();
        for t in &tools {
            let cat = t.name().split('.').next().unwrap_or("");
            assert!(
                CATEGORIES.contains(&cat),
                "tool {} has category {} not in CATEGORIES",
                t.name(),
                cat
            );
        }
    }
}
