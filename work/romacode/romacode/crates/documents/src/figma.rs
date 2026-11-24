//! Figma REST API client — pure Rust over ureq (rustls). Files, nodes,
//! rendered images, comments and team components. The token comes from the
//! `FIGMA_TOKEN` environment variable or the tool input.

use serde_json::{json, Value};
use std::io::Read;

const BASE: &str = "https://api.figma.com/v1";

fn token(input: &Value) -> Result<String, String> {
    if let Some(t) = input["token"].as_str() {
        return Ok(t.to_string());
    }
    std::env::var("FIGMA_TOKEN").map_err(|_| {
        "no Figma token: set the FIGMA_TOKEN environment variable for the server (or pass token in the input)".into()
    })
}

fn get(tok: &str, path: &str) -> Result<Value, String> {
    let url = format!("{BASE}{path}");
    let resp = ureq::get(&url)
        .set("X-Figma-Token", tok)
        .timeout(std::time::Duration::from_secs(45))
        .call()
        .map_err(|e| format!("figma GET {path}: {e}"))?;
    let status = resp.status();
    let body = resp.into_string().map_err(|e| format!("figma read: {e}"))?;
    let v: Value = serde_json::from_str(&body).map_err(|e| format!("figma json: {e} ({body})"))?;
    if status != 200 {
        let msg = v["err"].as_str().unwrap_or("request failed");
        return Err(format!("figma API {status}: {msg}"));
    }
    Ok(v)
}

fn post(tok: &str, path: &str, body: &Value) -> Result<Value, String> {
    let url = format!("{BASE}{path}");
    let resp = ureq::post(&url)
        .set("X-Figma-Token", tok)
        .timeout(std::time::Duration::from_secs(45))
        .send_json(body.clone())
        .map_err(|e| format!("figma POST {path}: {e}"))?;
    let status = resp.status();
    let text = resp.into_string().map_err(|e| format!("figma read: {e}"))?;
    let v: Value = serde_json::from_str(&text).unwrap_or(json!({"raw": text}));
    if status != 200 {
        let msg = v["err"].as_str().unwrap_or("request failed");
        return Err(format!("figma API {status}: {msg}"));
    }
    Ok(v)
}

/// Trim a Figma document tree to a manageable depth/size for the model.
fn trim_tree(node: &Value, depth: usize, max_depth: usize, stats: &mut (usize, usize)) -> Value {
    // stats: (kept nodes, total scanned)
    stats.1 += 1;
    let mut out = json!({
        "id": node["id"],
        "name": node["name"],
        "type": node["type"],
    });
    if depth == 0 {
        for k in ["width", "height", "backgroundColor", "absoluteBoundingBox"] {
            if node.get(k).is_some() {
                out[k] = node[k].clone();
            }
        }
    }
    if node.get("fills").is_some() && depth <= 2 {
        out["fills"] = node["fills"].clone();
    }
    if node.get("characters").is_some() {
        out["characters"] = node["characters"].clone();
    }
    if let Some(children) = node["children"].as_array() {
        if depth < max_depth {
            let kept: Vec<Value> = children
                .iter()
                .take(40)
                .map(|c| trim_tree(c, depth + 1, max_depth, stats))
                .collect();
            out["children"] = Value::Array(kept);
        } else {
            out["child_count"] = json!(children.len());
            stats.0 += children.len();
        }
    } else {
        stats.0 += 1;
    }
    out
}

/// `figma_file`: file structure (trimmed) for a file key.
pub fn file(input: &Value) -> Result<Value, String> {
    let tok = token(input)?;
    let key = input["key"].as_str().ok_or("needs `key` (the Figma file key from the URL)")?;
    let depth = input["depth"].as_u64().unwrap_or(4).min(10) as usize;
    let v = get(&tok, &format!("/files/{key}?depth={depth}"))?;
    let mut stats = (0usize, 0usize);
    let trimmed = trim_tree(&v["document"], 0, depth, &mut stats);
    Ok(json!({
        "name": v["name"],
        "lastModified": v["lastModified"],
        "version": v["version"],
        "thumbnailUrl": v["thumbnailUrl"],
        "document": trimmed,
        "nodes_scanned": stats.1,
        "nodes_listed": stats.0,
    }))
}

/// `figma_node`: one node subtree by ids.
pub fn node(input: &Value) -> Result<Value, String> {
    let tok = token(input)?;
    let key = input["key"].as_str().ok_or("needs `key`")?;
    let ids = input["ids"].as_str().ok_or("needs `ids` (comma-separated node ids)")?;
    let v = get(&tok, &format!("/files/{key}/nodes?ids={ids}"))?;
    let mut stats = (0usize, 0usize);
    let mut out = json!({});
    if let Some(nodes) = v["nodes"].as_object() {
        for (id, entry) in nodes {
            out[id] = json!({
                "document": trim_tree(&entry["document"], 0, input["depth"].as_u64().unwrap_or(6).min(10) as usize, &mut stats),
            });
        }
    }
    Ok(out)
}

/// `figma_images`: render nodes to PNG/SVG URLs and download them.
pub fn images(input: &Value, base: &std::path::Path) -> Result<Value, String> {
    let tok = token(input)?;
    let key = input["key"].as_str().ok_or("needs `key`")?;
    let ids = input["ids"].as_str().ok_or("needs `ids` (comma-separated node ids)")?;
    let format = input["format"].as_str().unwrap_or("png");
    let scale = input["scale"].as_f64().unwrap_or(2.0).clamp(0.01, 4.0);
    let v = get(&tok, &format!("/images/{key}?ids={ids}&format={format}&scale={scale}"))?;
    let mut saved = Vec::new();
    if let Some(imgs) = v["images"].as_object() {
        let out_dir = crate::charts::resolve(base, input["dir"].as_str().unwrap_or("figma"));
        std::fs::create_dir_all(&out_dir).map_err(|e| format!("mkdir {out_dir}: {e}"))?;
        for (i, (node_id, url)) in imgs.iter().enumerate() {
            let Some(url) = url.as_str() else { continue };
            let ext = if format == "svg" { "svg" } else { "png" };
            let path = std::path::Path::new(&out_dir).join(format!("node{}.{ext}", node_id.replace([':', '-', ';'], "_")));
            let resp = ureq::get(url).timeout(std::time::Duration::from_secs(60)).call().map_err(|e| format!("download {url}: {e}"))?;
            let mut bytes = Vec::new();
            resp.into_reader().take(64 * 1024 * 1024).read_to_end(&mut bytes).map_err(|e| format!("read {url}: {e}"))?;
            std::fs::write(&path, &bytes).map_err(|e| format!("write {}: {e}", path.display()))?;
            saved.push(json!({ "node": node_id, "path": path.to_string_lossy(), "bytes": bytes.len() }));
            let _ = i;
        }
    }
    Ok(json!({ "format": format, "scale": scale, "images": saved }))
}

/// `figma_comments`: list (or post, with `message`) comments on a file.
pub fn comments(input: &Value) -> Result<Value, String> {
    let tok = token(input)?;
    let key = input["key"].as_str().ok_or("needs `key`")?;
    if let Some(message) = input["message"].as_str() {
        let v = post(&tok, &format!("/files/{key}/comments"), &json!({ "message": message }))?;
        return Ok(json!({ "posted": true, "id": v["id"], "created_at": v["created_at"] }));
    }
    let v = get(&tok, &format!("/files/{key}/comments"))?;
    let count = v.as_array().map(|a| a.len()).unwrap_or(0);
    Ok(json!({ "comments": v, "count": count }))
}

/// `figma_components`: team components (needs `team_id`).
pub fn components(input: &Value) -> Result<Value, String> {
    let tok = token(input)?;
    let team = input["team_id"].as_str().ok_or("needs `team_id`")?;
    let v = get(&tok, &format!("/teams/{team}/components"))?;
    let count = v["meta"]["components"].as_array().map(|a| a.len()).unwrap_or(0);
    Ok(json!({ "team_id": team, "component_count": count, "components": v["meta"]["components"] }))
}

/// `figma_me`: token verification.
pub fn me(input: &Value) -> Result<Value, String> {
    let tok = token(input)?;
    get(&tok, "/me")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn trim_tree_limits_depth() {
        let big = json!({
            "id": "0:1", "name": "Page", "type": "FRAME",
            "children": [
                {"id": "1:1", "name": "Rect", "type": "RECTANGLE", "characters": "hello",
                 "children": [{"id": "2:1", "name": "Deep", "type": "VECTOR", "children": [{"id": "3:1", "name": "Deeper", "type": "VECTOR"}]}]}
            ]
        });
        let mut stats = (0, 0);
        let t = trim_tree(&big, 0, 1, &mut stats);
        assert_eq!(t["children"][0]["characters"], "hello");
        // depth 1 → the deep child's children are collapsed
        assert!(t["children"][0]["children"][0].get("children").is_none());
    }
}
