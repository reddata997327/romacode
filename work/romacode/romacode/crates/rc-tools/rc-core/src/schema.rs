//! Tool input schemas — a small, strong-typed subset of JSON Schema.
//!
//! A [`Schema`] is the contract a tool publishes about its input: the
//! parameters it accepts, their types, whether they are required, and their
//! defaults. The [`Validator`](crate::validator::Validator) checks a JSON
//! value against a schema before the tool ever runs.

use serde::{Deserialize, Serialize};

/// The parameter types a schema can express. Arrays carry an inner type so
/// `["a", "b"]` validates as `Array(String)`.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum ParamType {
    String,
    Integer,
    Number,
    Boolean,
    Array(Box<ParamType>),
    Object,
    Any,
}

impl std::fmt::Display for ParamType {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ParamType::String => write!(f, "string"),
            ParamType::Integer => write!(f, "integer"),
            ParamType::Number => write!(f, "number"),
            ParamType::Boolean => write!(f, "boolean"),
            ParamType::Object => write!(f, "object"),
            ParamType::Array(inner) => write!(f, "array<{}>", inner),
            ParamType::Any => write!(f, "any"),
        }
    }
}

impl ParamType {
    /// Check a JSON value against this type. Returns `Ok(())` if it matches.
    pub fn check(&self, v: &serde_json::Value) -> Result<(), String> {
        let ok = match (self, v) {
            (ParamType::Any, _) => true,
            (ParamType::String, serde_json::Value::String(_)) => true,
            (ParamType::Integer, serde_json::Value::Number(n)) => n.is_i64() || n.is_u64(),
            (ParamType::Number, serde_json::Value::Number(_)) => true,
            (ParamType::Boolean, serde_json::Value::Bool(_)) => true,
            (ParamType::Object, serde_json::Value::Object(_)) => true,
            (ParamType::Array(inner), serde_json::Value::Array(items)) => {
                for item in items {
                    inner.check(item).map_err(|e| format!("array element: {e}"))?;
                }
                true
            }
            _ => false,
        };
        if ok {
            Ok(())
        } else {
            Err(format!("expected {}, got {}", self, type_name(v)))
        }
    }
}

fn type_name(v: &serde_json::Value) -> &'static str {
    match v {
        serde_json::Value::Null => "null",
        serde_json::Value::Bool(_) => "boolean",
        serde_json::Value::Number(_) => "number",
        serde_json::Value::String(_) => "string",
        serde_json::Value::Array(_) => "array",
        serde_json::Value::Object(_) => "object",
    }
}

/// One parameter of a tool's input schema.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Parameter {
    pub name: String,
    pub description: String,
    #[serde(rename = "type")]
    pub typ: ParamType,
    #[serde(default)]
    pub required: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub default: Option<serde_json::Value>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub enum_values: Vec<serde_json::Value>,
}

impl Parameter {
    /// Build a required parameter shorthand.
    pub fn required(name: &str, typ: ParamType) -> Self {
        Self {
            name: name.to_string(),
            description: String::new(),
            typ,
            required: true,
            default: None,
            enum_values: Vec::new(),
        }
    }
    /// Build an optional parameter shorthand.
    pub fn optional(name: &str, typ: ParamType) -> Self {
        Self {
            name: name.to_string(),
            description: String::new(),
            typ,
            required: false,
            default: None,
            enum_values: Vec::new(),
        }
    }
    pub fn with_desc(mut self, d: impl Into<String>) -> Self {
        self.description = d.into();
        self
    }
    pub fn with_default(mut self, v: serde_json::Value) -> Self {
        self.default = Some(v);
        self
    }
    pub fn with_enum(mut self, vs: Vec<serde_json::Value>) -> Self {
        self.enum_values = vs;
        self
    }
}

/// The full input schema of a tool: an ordered list of named parameters.
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct Schema {
    #[serde(default)]
    pub parameters: Vec<Parameter>,
}

impl Schema {
    pub fn new() -> Self {
        Self::default()
    }
    pub fn param(mut self, p: Parameter) -> Self {
        self.parameters.push(p);
        self
    }
    /// Render the schema as a JSON-Schema-ish object for OpenAI/Anthropic tool
    /// definitions: `{ "type": "object", "properties": {...}, "required": [...] }`.
    pub fn to_json_schema(&self) -> serde_json::Value {
        let mut props = serde_json::Map::new();
        let mut required = Vec::new();
        for p in &self.parameters {
            let t = match &p.typ {
                ParamType::String => serde_json::json!("string"),
                ParamType::Integer => serde_json::json!("integer"),
                ParamType::Number => serde_json::json!("number"),
                ParamType::Boolean => serde_json::json!("boolean"),
                ParamType::Object => serde_json::json!("object"),
                ParamType::Array(_) => serde_json::json!({ "type": "array" }),
                ParamType::Any => serde_json::json!({}),
            };
            let mut entry = serde_json::Map::new();
            if let serde_json::Value::String(_) = t {
                entry.insert("type".into(), t);
            } else if let serde_json::Value::Object(_) = &t {
                // array case: insert the type object directly
                if let Some(obj) = t.as_object() {
                    for (k, v) in obj {
                        entry.insert(k.clone(), v.clone());
                    }
                }
            }
            if !p.description.is_empty() {
                entry.insert("description".into(), serde_json::Value::String(p.description.clone()));
            }
            if !p.enum_values.is_empty() {
                entry.insert("enum".into(), serde_json::Value::Array(p.enum_values.clone()));
            }
            if let Some(d) = &p.default {
                entry.insert("default".into(), d.clone());
            }
            props.insert(p.name.clone(), serde_json::Value::Object(entry));
            if p.required {
                required.push(serde_json::Value::String(p.name.clone()));
            }
        }
        serde_json::json!({
            "type": "object",
            "properties": serde_json::Value::Object(props),
            "required": serde_json::Value::Array(required),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn type_check_basic() {
        assert!(ParamType::String.check(&serde_json::json!("hi")).is_ok());
        assert!(ParamType::Integer.check(&serde_json::json!(42)).is_ok());
        assert!(ParamType::Number.check(&serde_json::json!(42)).is_ok());
        assert!(ParamType::Number.check(&serde_json::json!(3.14)).is_ok());
        assert!(ParamType::Integer.check(&serde_json::json!(3.14)).is_err());
        assert!(ParamType::Boolean.check(&serde_json::json!(true)).is_ok());
        assert!(ParamType::Array(Box::new(ParamType::String))
            .check(&serde_json::json!(["a", "b"]))
            .is_ok());
        assert!(ParamType::Array(Box::new(ParamType::Integer))
            .check(&serde_json::json!(["a"]))
            .is_err());
    }

    #[test]
    fn json_schema_shape() {
        let s = Schema::new()
            .param(Parameter::required("n", ParamType::Integer).with_desc("count"))
            .param(Parameter::optional("greeting", ParamType::String).with_default(serde_json::json!("hi")));
        let j = s.to_json_schema();
        assert_eq!(j["type"], "object");
        assert_eq!(j["required"][0], "n");
        assert_eq!(j["properties"]["n"]["type"], "integer");
        assert_eq!(j["properties"]["greeting"]["default"], "hi");
    }
}
