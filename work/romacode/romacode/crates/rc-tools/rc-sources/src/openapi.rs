//! Source 4 of 8 — **OpenAPI import**. Point at an OpenAPI 3 (Swagger) JSON
//! spec (a URL or a file), get one [`HttpTool`] per `path × method`. Every
//! REST API in the world becomes a set of RomaCode tools.

use crate::source::{SourceError, ToolSource};
use crate::tools::HttpTool;
use async_trait::async_trait;
use rc_core::schema::{ParamType, Parameter, Schema};
use rc_core::tool::{arc, ToolArc, ToolSourceKind};
use serde::Deserialize;
use serde_json::Value;

#[derive(Debug, Deserialize)]
struct OpenApiSpec {
    #[serde(default)]
    servers: Vec<Server>,
    #[serde(default)]
    paths: serde_json::Map<String, Value>,
}
#[derive(Debug, Deserialize)]
struct Server {
    url: String,
}
#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct Operation {
    #[serde(default)]
    operation_id: Option<String>,
    #[serde(default)]
    summary: Option<String>,
    #[serde(default)]
    parameters: Vec<RefOr<ParameterDef>>,
    #[serde(default)]
    request_body: Option<RequestBody>,
}
#[derive(Debug, Deserialize)]
struct ParameterDef {
    name: String,
    #[serde(default)]
    #[allow(dead_code)]
    #[serde(rename = "in")]
    location: Option<String>,
    #[serde(default)]
    required: bool,
    #[serde(default)]
    description: Option<String>,
    #[serde(default)]
    schema: Option<SchemaRef>,
}
#[derive(Debug, Deserialize)]
struct SchemaRef {
    #[serde(rename = "type")]
    #[serde(default)]
    typ: Option<String>,
}
#[derive(Debug, Deserialize)]
struct RequestBody {
    #[serde(default)]
    description: Option<String>,
    #[serde(default)]
    required: bool,
}
#[derive(Debug, Deserialize)]
#[serde(untagged)]
enum RefOr<T> {
    // `$ref` resolution is a TODO; for now we skip ref'd parameters/operations.
    Ref {
        #[serde(rename = "$ref")]
        #[allow(dead_code)]
        reference: String,
    },
    Inline(T),
}

const METHODS: &[&str] = &["get", "post", "put", "patch", "delete", "head"];

fn oapi_type(s: &str) -> ParamType {
    match s {
        "integer" => ParamType::Integer,
        "number" => ParamType::Number,
        "boolean" => ParamType::Boolean,
        "object" => ParamType::Object,
        "array" => ParamType::Array(Box::new(ParamType::Any)),
        _ => ParamType::String,
    }
}

/// Parse an OpenAPI 3 JSON string into a list of HTTP tools.
pub fn parse_spec(json: &str) -> Result<Vec<ToolArc>, SourceError> {
    let spec: OpenApiSpec = serde_json::from_str(json).map_err(|e| SourceError::Json {
        src: "openapi".into(),
        inner: e,
    })?;
    let base = spec
        .servers
        .first()
        .map(|s| s.url.trim_end_matches('/').to_string())
        .unwrap_or_default();
    let mut out = Vec::new();
    for (path, item) in &spec.paths {
        let Some(ops) = item.as_object() else { continue };
        for &method in METHODS {
            let Some(op_val) = ops.get(method) else { continue };
            let op: Operation = serde_json::from_value(op_val.clone()).map_err(|e| {
                SourceError::Json {
                    src: "openapi".into(),
                    inner: e,
                }
            })?;
            let name = op
                .operation_id
                .clone()
                .unwrap_or_else(|| slug(&format!("{}_{}", method, path)));
            let mut schema = Schema::new();
            // path + query params
            for p in &op.parameters {
                if let RefOr::Inline(def) = p {
                    let typ = def
                        .schema
                        .as_ref()
                        .and_then(|s| s.typ.as_deref())
                        .map(oapi_type)
                        .unwrap_or(ParamType::String);
                    schema = schema.param(
                        Parameter {
                            name: def.name.clone(),
                            description: def.description.clone().unwrap_or_default(),
                            typ,
                            required: def.required,
                            default: None,
                            enum_values: Vec::new(),
                        },
                    );
                }
            }
            // request body → a single "body" object param
            if let Some(rb) = &op.request_body {
                schema = schema.param(Parameter {
                    name: "body".into(),
                    description: rb.description.clone().unwrap_or_default(),
                    typ: ParamType::Any,
                    required: rb.required,
                    default: None,
                    enum_values: Vec::new(),
                });
            }
            // url: base + path, with {param} → {{param}}
            let mut url = if base.is_empty() {
                path.clone()
            } else {
                format!("{base}{path}")
            };
            // OpenAPI path params use {id}; our template uses {{id}}.
            for p in &op.parameters {
                if let RefOr::Inline(def) = p {
                    url = url.replace(&format!("{{{}}}", def.name), &format!("{{{{{}}}}}", def.name));
                }
            }
            let mut tool = HttpTool::new(
                name,
                op.summary.clone().unwrap_or_else(|| format!("{method} {path}")),
                schema,
                method.to_uppercase(),
                url,
                ToolSourceKind::OpenApi,
            );
            if op.request_body.is_some() {
                tool.body = Some("{{body}}".to_string());
            }
            out.push(arc(tool));
        }
    }
    Ok(out)
}

fn slug(s: &str) -> String {
    s.chars()
        .map(|c| if c.is_alphanumeric() { c } else { '.' })
        .collect::<String>()
        .trim_matches('.')
        .to_string()
}

pub struct OpenApiSource {
    json: String,
    origin: String,
}

impl OpenApiSource {
    pub fn from_str(json: impl Into<String>, origin: impl Into<String>) -> Self {
        Self {
            json: json.into(),
            origin: origin.into(),
        }
    }
    pub async fn from_file(path: &str) -> Result<Self, SourceError> {
        let json = tokio::fs::read_to_string(path).await.map_err(|e| {
            SourceError::Io {
                src: "openapi".into(),
                inner: e,
            }
        })?;
        Ok(Self::from_str(json, path))
    }
    /// Fetch a spec from a URL.
    pub async fn from_url(url: &str) -> Result<Self, SourceError> {
        let resp = reqwest::get(url).await.map_err(|e| SourceError::Http {
            src: "openapi".into(),
            inner: e.to_string(),
        })?;
        let json = resp.text().await.map_err(|e| SourceError::Http {
            src: "openapi".into(),
            inner: e.to_string(),
        })?;
        Ok(Self::from_str(json, url))
    }
}

#[async_trait]
impl ToolSource for OpenApiSource {
    fn kind(&self) -> ToolSourceKind {
        ToolSourceKind::OpenApi
    }
    fn id(&self) -> &str {
        &self.origin
    }
    async fn tools(&self) -> Result<Vec<ToolArc>, SourceError> {
        parse_spec(&self.json)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use rc_core::Registry;

    #[test]
    fn parse_petstore_like_spec() {
        let json = r#"{
          "openapi": "3.0.0",
          "servers": [{"url": "https://api.example.com/v1"}],
          "paths": {
            "/pets/{petId}": {
              "get": {
                "operationId": "getPetById",
                "summary": "Find a pet by ID",
                "parameters": [{"name":"petId","in":"path","required":true,"schema":{"type":"integer"},"description":"pet id"}]
              },
              "delete": {"operationId": "deletePet"}
            },
            "/pets": {
              "post": {
                "operationId": "addPet",
                "summary": "Add a pet",
                "requestBody": {"required": true, "description": "the pet body"}
              }
            }
          }
        }"#;
        let tools = parse_spec(json).unwrap();
        assert_eq!(tools.len(), 3);
        let r = Registry::new();
        r.register_all(tools);
        assert!(r.contains("getPetById"));
        assert!(r.contains("deletePet"));
        assert!(r.contains("addPet"));
        let get = r.get("getPetById").unwrap();
        // url should have {{petId}} template
        // (HttpTool stores url privately; check schema instead)
        assert_eq!(get.schema().parameters.len(), 1);
        assert_eq!(get.schema().parameters[0].name, "petId");
        assert!(get.schema().parameters[0].required);
        let post = r.get("addPet").unwrap();
        assert_eq!(post.schema().parameters.len(), 1);
        assert_eq!(post.schema().parameters[0].name, "body");
    }
}
