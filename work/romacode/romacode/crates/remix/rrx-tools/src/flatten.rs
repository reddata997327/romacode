//! Contract Flattener: resolve imports recursively and emit one self-contained file.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

pub fn flatten(root: &Path, entry: &str) -> Result<String, String> {
    let mut remappings = load_remappings(root);
    let mut resolved: BTreeMap<String, String> = BTreeMap::new();
    let mut order: Vec<String> = vec![];
    let mut stack: Vec<String> = vec![];

    process_file(root, entry, &mut remappings, &mut resolved, &mut order, &mut stack)?;

    let mut out = String::new();
    // single SPDX header
    out.push_str("// SPDX-License-Identifier: MIT\n");
    let mut pragma_done = false;
    for f in &order {
        let content = &resolved[f];
        for line in content.lines() {
            let t = line.trim();
            if t.starts_with("// SPDX-License-Identifier") {
                continue;
            }
            if t.starts_with("pragma solidity") {
                if !pragma_done {
                    out.push_str(line);
                    out.push('\n');
                    pragma_done = true;
                }
                continue;
            }
            if t.starts_with("import ") {
                continue;
            }
            out.push_str(line);
            out.push('\n');
        }
        out.push('\n');
    }
    Ok(out)
}

fn load_remappings(root: &Path) -> Vec<(String, String)> {
    let mut out = vec![];
    let f = root.join("remappings.txt");
    if let Ok(content) = std::fs::read_to_string(f) {
        for line in content.lines() {
            let line = line.trim();
            if let Some((k, v)) = line.split_once('=') {
                out.push((k.trim().to_string(), v.trim().trim_end_matches('/').to_string()));
            }
        }
    }
    out
}

fn resolve_import(
    root: &Path,
    importer_dir: &Path,
    import: &str,
    remappings: &[(String, String)],
) -> Result<PathBuf, String> {
    // remappings first
    for (prefix, target) in remappings {
        if let Some(rest) = import.strip_prefix(prefix.as_str()) {
            return Ok(root.join(target).join(rest));
        }
    }
    let direct = importer_dir.join(import);
    if direct.exists() {
        return Ok(direct);
    }
    let from_root = root.join(import);
    if from_root.exists() {
        return Ok(from_root);
    }
    let node_modules = root.join("node_modules").join(import);
    if node_modules.exists() {
        return Ok(node_modules);
    }
    Err(format!("cannot resolve import '{import}' (from {})", importer_dir.display()))
}

#[allow(clippy::too_many_arguments)]
fn process_file(
    root: &Path,
    rel: &str,
    remappings: &mut Vec<(String, String)>,
    resolved: &mut BTreeMap<String, String>,
    order: &mut Vec<String>,
    stack: &mut Vec<String>,
) -> Result<(), String> {
    let path = if rel.starts_with('/') { PathBuf::from(rel) } else { root.join(rel) };
    let canonical = path
        .canonicalize()
        .map_err(|e| format!("cannot open {rel}: {e}"))?
        .to_string_lossy()
        .to_string();
    if resolved.contains_key(&canonical) {
        return Ok(());
    }
    if stack.contains(&canonical) {
        return Ok(()); // cycle guard
    }
    stack.push(canonical.clone());

    let content = std::fs::read_to_string(&path).map_err(|e| e.to_string())?;
    let importer_dir = path.parent().unwrap_or(root).to_path_buf();

    for line in content.lines() {
        let t = line.trim();
        if let Some(rest) = t.strip_prefix("import ") {
            // import "path";  |  import {A} from "path";  |  import * as X from "path";
            let quote_start = rest.find('"').ok_or("malformed import")?;
            let quote_end = rest[quote_start + 1..].find('"').ok_or("malformed import")? + quote_start + 1;
            let import = &rest[quote_start + 1..quote_end];
            let target = resolve_import(root, &importer_dir, import, remappings)?;
            let rel_target = target
                .strip_prefix(root)
                .map(|p| p.to_string_lossy().to_string())
                .unwrap_or_else(|_| target.to_string_lossy().to_string());
            process_file(root, &rel_target, remappings, resolved, order, stack)?;
        }
    }

    resolved.insert(canonical.clone(), content);
    order.push(canonical);
    stack.pop();
    Ok(())
}
