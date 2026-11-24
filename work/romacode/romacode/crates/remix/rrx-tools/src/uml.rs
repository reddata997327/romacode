//! UML Generator: mermaid class diagrams (and PlantUML) from the solc AST.

use serde_json::Value;

pub fn mermaid(output: &Value) -> String {
    let mut out = String::from("classDiagram\n");
    let mut relations = String::new();
    if let Some(sources) = output.get("sources").and_then(|s| s.as_object()) {
        for (_file, sv) in sources {
            let Some(ast) = sv.get("ast") else { continue };
            walk_contracts(ast, &mut out, &mut relations);
        }
    }
    out.push_str(&relations);
    out
}

fn walk_contracts(node: &Value, out: &mut String, relations: &mut String) {
    if let Value::Object(map) = node {
        if map.get("nodeType").and_then(|t| t.as_str()) == Some("ContractDefinition") {
            let name = map.get("name").and_then(|n| n.as_str()).unwrap_or("?");
            let kind = map.get("contractKind").and_then(|k| k.as_str()).unwrap_or("contract");
            let abstract_ = map.get("abstract").and_then(|a| a.as_bool()).unwrap_or(false);
            let stereo = if abstract_ {
                format!("«abstract {kind}»")
            } else {
                format!("«{kind}»")
            };
            out.push_str(&format!("  class {name} {{\n    {}\n", stereo.replace(' ', "_")));
            if let Some(nodes) = map.get("nodes").and_then(|n| n.as_array()) {
                for child in nodes {
                    let cty = child.get("nodeType").and_then(|t| t.as_str()).unwrap_or("");
                    match cty {
                        "VariableDeclaration" => {
                            let vn = child.get("name").and_then(|n| n.as_str()).unwrap_or("");
                            let vis = child
                                .get("visibility")
                                .and_then(|v| v.as_str())
                                .and_then(short_vis)
                                .unwrap_or("+");
                            let ty = type_name(child.get("typeName"));
                            out.push_str(&format!("    {vis}{vn} : {ty}\n"));
                        }
                        "FunctionDefinition" => {
                            let fname = child.get("name").and_then(|n| n.as_str()).unwrap_or("constructor");
                            let vis = child
                                .get("visibility")
                                .and_then(|v| v.as_str())
                                .and_then(short_vis)
                                .unwrap_or("+");
                            let sm = child.get("stateMutability").and_then(|m| m.as_str()).unwrap_or("");
                            let params = child
                                .get("parameters").and_then(|p| p.get("parameters"))
                                .and_then(|p| p.as_array())
                                .map(|ps| {
                                    ps.iter()
                                        .map(|p| type_name(p.get("typeName")).replace(' ', "_"))
                                        .collect::<Vec<_>>()
                                        .join(",")
                                })
                                .unwrap_or_default();
                            let ret = child
                                .get("returnParameters").and_then(|p| p.get("parameters"))
                                .and_then(|p| p.as_array())
                                .map(|ps| {
                                    ps.iter()
                                        .map(|p| type_name(p.get("typeName")).replace(' ', "_"))
                                        .collect::<Vec<_>>()
                                        .join(",")
                                })
                                .unwrap_or_default();
                            let mut sig = format!("{vis}{fname}({params})");
                            if !sm.is_empty() && sm != "nonpayable" {
                                sig.push_str(&format!("_{sm}"));
                            }
                            if !ret.is_empty() {
                                sig.push_str(&format!("_{ret}"));
                            }
                            out.push_str(&format!("    {sig}\n"));
                        }
                        "EventDefinition" => {
                            let en = child.get("name").and_then(|n| n.as_str()).unwrap_or("");
                            out.push_str(&format!("    «event»_{en}\n"));
                        }
                        _ => {}
                    }
                }
            }
            out.push_str("  }\n");

            for base in map.get("baseContracts").and_then(|b| b.as_array()).unwrap_or(&vec![]) {
                let base_name = base
                    .get("baseName").and_then(|b| b.get("name"))
                    .and_then(|n| n.as_str())
                    .unwrap_or("?");
                relations.push_str(&format!("  {base_name} <|-- {name}\n"));
            }
            return;
        }
        for v in map.values() {
            walk_contracts(v, out, relations);
        }
    } else if let Value::Array(arr) = node {
        for v in arr {
            walk_contracts(v, out, relations);
        }
    }
}

fn short_vis(v: &str) -> Option<&'static str> {
    match v {
        "public" => Some("+"),
        "external" => Some("+"),
        "internal" => Some("#"),
        "private" => Some("-"),
        _ => None,
    }
}

fn type_name(v: Option<&Value>) -> String {
    match v {
        Some(Value::Object(t)) => {
            let ty = t.get("nodeType").and_then(|n| n.as_str()).unwrap_or("");
            match ty {
                "ElementaryTypeName" => t.get("name").and_then(|n| n.as_str()).unwrap_or("?").to_string(),
                "UserDefinedTypeName" => t
                    .get("pathNode").and_then(|b| b.get("name"))
                    .or_else(|| t.get("name"))
                    .and_then(|n| n.as_str())
                    .unwrap_or("?")
                    .to_string(),
                "Mapping" => {
                    let k = type_name(t.get("keyType"));
                    let v = type_name(t.get("valueType"));
                    format!("mapping({k}=>{v})")
                }
                "ArrayTypeName" => {
                    let b = type_name(t.get("baseType"));
                    format!("{b}[]")
                }
                _ => "?".to_string(),
            }
        }
        _ => "?".to_string(),
    }
}
