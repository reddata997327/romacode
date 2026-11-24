//! File Explorer (headless): list/read/write/delete/move on the workspace filesystem.

use std::path::Path;

pub fn list(dir: &Path) -> Result<Vec<String>, String> {
    let mut out = vec![];
    let mut entries: Vec<_> = std::fs::read_dir(dir)
        .map_err(|e| e.to_string())?
        .filter_map(|e| e.ok())
        .collect();
    entries.sort_by_key(|e| e.file_name());
    for e in entries {
        let name = e.file_name().to_string_lossy().to_string();
        let meta = e.metadata().map_err(|e| e.to_string())?;
        let mut line = if meta.is_dir() {
            format!("{}/", name)
        } else {
            name.clone()
        };
        if meta.is_file() {
            line.push_str(&format!("  ({} bytes)", meta.len()));
        }
        out.push(line);
    }
    Ok(out)
}

pub fn read(file: &Path) -> Result<String, String> {
    std::fs::read_to_string(file).map_err(|e| e.to_string())
}

pub fn write(file: &Path, content: &str) -> Result<(), String> {
    if let Some(parent) = file.parent() {
        std::fs::create_dir_all(parent).map_err(|e| e.to_string())?;
    }
    std::fs::write(file, content).map_err(|e| e.to_string())
}

pub fn remove(path: &Path) -> Result<(), String> {
    let meta = std::fs::metadata(path).map_err(|e| e.to_string())?;
    if meta.is_dir() {
        std::fs::remove_dir_all(path).map_err(|e| e.to_string())
    } else {
        std::fs::remove_file(path).map_err(|e| e.to_string())
    }
}

pub fn mkdir(path: &Path) -> Result<(), String> {
    std::fs::create_dir_all(path).map_err(|e| e.to_string())
}

pub fn move_path(from: &Path, to: &Path) -> Result<(), String> {
    if let Some(parent) = to.parent() {
        std::fs::create_dir_all(parent).map_err(|e| e.to_string())?;
    }
    std::fs::rename(from, to).map_err(|e| e.to_string())
}

pub fn copy(from: &Path, to: &Path) -> Result<(), String> {
    if let Some(parent) = to.parent() {
        std::fs::create_dir_all(parent).map_err(|e| e.to_string())?;
    }
    std::fs::copy(from, to).map(|_| ()).map_err(|e| e.to_string())
}

/// Recursive tree listing (like the Remix file panel).
pub fn tree(dir: &Path, prefix: &str, out: &mut Vec<String>) -> Result<(), String> {
    let mut entries: Vec<_> = std::fs::read_dir(dir)
        .map_err(|e| e.to_string())?
        .filter_map(|e| e.ok())
        .collect();
    entries.sort_by_key(|e| e.file_name());
    for (i, e) in entries.iter().enumerate() {
        let last = i + 1 == entries.len();
        let name = e.file_name().to_string_lossy().to_string();
        let branch = if last { "└─ " } else { "├─ " };
        let is_dir = e.metadata().map(|m| m.is_dir()).unwrap_or(false);
        out.push(format!("{}{}{}{}", prefix, branch, name, if is_dir { "/" } else { "" }));
        if is_dir {
            let new_prefix = format!("{}{}", prefix, if last { "   " } else { "│  " });
            tree(&e.path(), &new_prefix, out)?;
        }
    }
    Ok(())
}
