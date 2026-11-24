//! Workspace management: folders of Solidity sources (File Explorer's higher level).

use std::path::{Path, PathBuf};

pub struct Workspace {
    pub root: PathBuf,
}

impl Workspace {
    pub fn at(root: &Path) -> Workspace {
        Workspace { root: root.to_path_buf() }
    }

    pub fn create_from_template(root: &Path, template: &str) -> Result<PathBuf, String> {
        for (rel, content) in crate::templates::template_files(template)? {
            let path = root.join(&rel);
            if let Some(parent) = path.parent() {
                std::fs::create_dir_all(parent).map_err(|e| e.to_string())?;
            }
            std::fs::write(&path, content).map_err(|e| e.to_string())?;
        }
        Ok(root.to_path_buf())
    }

    pub fn exists(&self) -> bool {
        self.root.exists()
    }

    /// All .sol files in the workspace (relative paths, sorted).
    pub fn sol_files(&self) -> Vec<String> {
        let mut out = vec![];
        for entry in walkdir::WalkDir::new(&self.root)
            .into_iter()
            .filter_map(|e| e.ok())
        {
            let p = entry.path();
            if p.is_file() && p.extension().map(|e| e == "sol").unwrap_or(false) {
                let rel = p.strip_prefix(&self.root).unwrap_or(p).to_string_lossy().to_string();
                if !rel.contains("node_modules") {
                    out.push(rel.replace('\\', "/"));
                }
            }
        }
        out.sort();
        out
    }

    /// Load all sol sources into a standard-json style map.
    pub fn load_sources(&self) -> Result<std::collections::BTreeMap<String, String>, String> {
        let mut map = std::collections::BTreeMap::new();
        for rel in self.sol_files() {
            let content = std::fs::read_to_string(self.root.join(&rel)).map_err(|e| e.to_string())?;
            map.insert(rel, content);
        }
        Ok(map)
    }

    pub fn artifacts_dir(&self) -> PathBuf {
        self.root.join("artifacts")
    }
}
