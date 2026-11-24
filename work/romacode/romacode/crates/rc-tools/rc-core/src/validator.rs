//! Input validation: check a JSON object against a tool's [`Schema`] before
//! the tool runs. The validator also fills in defaults for missing optional
//! parameters, so a tool's `execute` always sees a complete input.

use crate::error::{ToolError, ToolResult};
use crate::schema::Schema;
use serde_json::{Map, Value};

/// Validate `input` against `schema`. On success, returns a new JSON object
/// with defaults applied for any missing optional parameters.
pub fn validate(schema: &Schema, input: &Value) -> ToolResult<Value> {
    let obj = input.as_object().ok_or_else(|| {
        ToolError::Validation("input must be a JSON object".to_string())
    })?;

    let mut out = Map::new();

    // Copy through any caller-supplied values first (after type-checking).
    for (k, v) in obj {
        out.insert(k.clone(), v.clone());
    }

    for p in &schema.parameters {
        match out.get(&p.name) {
            // Present — type-check it.
            Some(v) => {
                if let Err(detail) = p.typ.check(v) {
                    return Err(ToolError::BadParam {
                        name: p.name.clone(),
                        expected: p.typ.clone(),
                        detail,
                    });
                }
                // enum constraint, if any
                if !p.enum_values.is_empty()
                    && !p.enum_values.iter().any(|e| e == v)
                {
                    return Err(ToolError::BadParam {
                        name: p.name.clone(),
                        expected: p.typ.clone(),
                        detail: format!(
                            "value {} is not one of the allowed values",
                            v
                        ),
                    });
                }
            }
            // Absent — required? error. Optional? apply default or skip.
            None => {
                if p.required {
                    return Err(ToolError::Validation(format!(
                        "missing required parameter `{}`",
                        p.name
                    )));
                } else if let Some(d) = &p.default {
                    out.insert(p.name.clone(), d.clone());
                }
            }
        }
    }

    Ok(Value::Object(out))
}

/// Public facade matching the docs (`Validator::validate`).
pub struct Validator;
impl Validator {
    pub fn validate(schema: &Schema, input: &Value) -> ToolResult<Value> {
        validate(schema, input)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::schema::{ParamType, Parameter};

    #[test]
    fn fills_defaults_and_type_checks() {
        let s = Schema::new()
            .param(Parameter::required("n", ParamType::Integer))
            .param(
                Parameter::optional("label", ParamType::String)
                    .with_default(serde_json::json!("hi")),
            );
        let v = validate(&s, &serde_json::json!({ "n": 5 })).unwrap();
        assert_eq!(v["n"], 5);
        assert_eq!(v["label"], "hi"); // default applied
    }

    #[test]
    fn rejects_bad_type() {
        let s = Schema::new().param(Parameter::required("n", ParamType::Integer));
        let err = validate(&s, &serde_json::json!({ "n": "five" }));
        assert!(err.is_err());
    }

    #[test]
    fn rejects_missing_required() {
        let s = Schema::new().param(Parameter::required("n", ParamType::Integer));
        let err = validate(&s, &serde_json::json!({}));
        assert!(err.is_err());
    }

    #[test]
    fn enforces_enum() {
        let s = Schema::new().param(
            Parameter::required("mode", ParamType::String)
                .with_enum(vec![serde_json::json!("a"), serde_json::json!("b")]),
        );
        assert!(validate(&s, &serde_json::json!({ "mode": "a" })).is_ok());
        assert!(validate(&s, &serde_json::json!({ "mode": "z" })).is_err());
    }
}
