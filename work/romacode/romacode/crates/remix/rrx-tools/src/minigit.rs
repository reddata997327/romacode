//! Mini Git (remix-git equivalent): content-addressed snapshots with keccak hashing.
//! Supports init/add/commit/status/log — enough to version a workspace headlessly.

use std::path::{Path, PathBuf};

use rrx_util::keccak256;

pub struct MiniGit {
    pub dir: PathBuf, // <workspace>/.rrxgit
}

impl MiniGit {
    pub fn at(workspace: &Path) -> MiniGit {
        MiniGit { dir: workspace.join(".rrxgit") }
    }

    pub fn init(&self) -> Result<(), String> {
        std::fs::create_dir_all(self.dir.join("objects")).map_err(|e| e.to_string())?;
        std::fs::create_dir_all(self.dir.join("refs")).map_err(|e| e.to_string())?;
        Ok(())
    }

    pub fn is_repo(&self) -> bool {
        self.dir.exists()
    }

    fn read_json(&self, rel: &Path) -> Option<serde_json::Value> {
        std::fs::read_to_string(self.dir.join(rel))
            .ok()
            .and_then(|s| serde_json::from_str(&s).ok())
    }

    fn write_json(&self, rel: &Path, v: &serde_json::Value) -> Result<(), String> {
        if let Some(p) = self.dir.join(rel).parent() {
            std::fs::create_dir_all(p).map_err(|e| e.to_string())?;
        }
        std::fs::write(self.dir.join(rel), serde_json::to_string_pretty(v).map_err(|e| e.to_string())?)
            .map_err(|e| e.to_string())
    }

    /// Stage all workspace files (excluding .rrxgit / .rrx / artifacts) — like `git add .`.
    pub fn add_all(&self, workspace: &Path) -> Result<usize, String> {
        let mut index: std::collections::BTreeMap<String, String> = Default::default();
        for entry in walkdir::WalkDir::new(workspace)
            .into_iter()
            .filter_map(|e| e.ok())
            .filter(|e| e.file_type().is_file())
        {
            let p = entry.path();
            let rel = p.strip_prefix(workspace).unwrap_or(p).to_string_lossy().replace('\\', "/");
            if rel.starts_with(".rrxgit/") || rel.starts_with(".rrx/") || rel.starts_with("artifacts/") {
                continue;
            }
            let content = std::fs::read(p).map_err(|e| e.to_string())?;
            let hash = rrx_util::to_hex(&keccak256(&content));
            std::fs::write(self.dir.join("objects").join(&hash), &content).map_err(|e| e.to_string())?;
            index.insert(rel, hash);
        }
        let n = index.len();
        self.write_json(Path::new("index"), &serde_json::to_value(&index).map_err(|e| e.to_string())?)?;
        Ok(n)
    }

    /// Commit the staged index. Returns the commit hash.
    pub fn commit(&self, message: &str) -> Result<String, String> {
        let idx_val = self.read_json(Path::new("index")).ok_or("nothing staged — run add first")?;
        let index: std::collections::BTreeMap<String, String> =
            serde_json::from_value(idx_val).map_err(|e| e.to_string())?;
        let parent = self.head().unwrap_or_default();
        let time = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_secs())
            .unwrap_or(0);
        let commit = serde_json::json!({
            "message": message,
            "time": time,
            "parent": parent,
            "tree": index,
        });
        let body = serde_json::to_string(&commit).map_err(|e| e.to_string())?;
        let hash = rrx_util::to_hex(&keccak256(body.as_bytes()));
        std::fs::write(self.dir.join("objects").join(&hash), body).map_err(|e| e.to_string())?;
        self.write_json(Path::new("refs/head"), &serde_json::Value::String(hash.clone()))?;
        Ok(hash)
    }

    pub fn head(&self) -> Option<String> {
        self.read_json(Path::new("refs/head")).and_then(|v| v.as_str().map(|s| s.to_string()))
    }

    /// Changed / new / deleted files vs HEAD.
    pub fn status(&self, workspace: &Path) -> Result<(Vec<String>, Vec<String>, Vec<String>), String> {
        let head_tree: std::collections::BTreeMap<String, String> = self
            .head()
            .and_then(|h| {
                let body: serde_json::Value =
                    serde_json::from_str(&std::fs::read_to_string(self.dir.join("objects").join(&h)).ok()?).ok()?;
                let tree = body.get("tree")?;
                serde_json::from_value(tree.clone()).ok()
            })
            .unwrap_or_default();


        let mut modified = vec![];
        let mut added = vec![];
        let mut deleted: Vec<String> = head_tree.keys().cloned().collect();
        for entry in walkdir::WalkDir::new(workspace)
            .into_iter()
            .filter_map(|e| e.ok())
            .filter(|e| e.file_type().is_file())
        {
            let p = entry.path();
            let rel = p.strip_prefix(workspace).unwrap_or(p).to_string_lossy().replace('\\', "/");
            if rel.starts_with(".rrxgit/") || rel.starts_with(".rrx/") || rel.starts_with("artifacts/") {
                continue;
            }
            deleted.retain(|d| d != &rel);
            let content = std::fs::read(p).map_err(|e| e.to_string())?;
            let hash = rrx_util::to_hex(&keccak256(&content));
            match head_tree.get(&rel) {
                Some(h) if h == &hash => {}
                Some(_) => modified.push(rel),
                None => added.push(rel),
            }
        }
        Ok((modified, added, deleted))
    }

    /// One line per commit (newest first).
    pub fn log(&self) -> Result<Vec<String>, String> {
        let mut out = vec![];
        let mut cur = self.head();
        let mut guard = 0;
        while let Some(h) = cur {
            guard += 1;
            if guard > 1000 {
                break;
            }
            let body: serde_json::Value = serde_json::from_str(
                &std::fs::read_to_string(self.dir.join("objects").join(&h)).map_err(|e| e.to_string())?,
            )
            .map_err(|e| e.to_string())?;
            let msg = body["message"].as_str().unwrap_or("?");
            let time = body["time"].as_u64().unwrap_or(0);
            out.push(format!("{h} {} — {}", time, msg));
            cur = body["parent"].as_str().map(|s| s.to_string()).filter(|s| !s.is_empty());
        }
        Ok(out)
    }
}
