//! Sandbox — the resource limits the executor enforces around every tool
//! call: a wall-clock timeout, an output-size cap, and network/filesystem
//! allow-lists that tools *should* consult before doing I/O.
//!
//! The shipped default is **fully open** (network allowed, whole filesystem,
//! like the server gate). [`Sandbox::strict`] remains available as an opt-in
//! lockdown for embedders who want one.

use serde::{Deserialize, Serialize};

/// Resource limits for one execution.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Sandbox {
    /// Hard wall-clock budget, in milliseconds. `0` = no limit.
    #[serde(default = "default_timeout_ms")]
    pub timeout_ms: u64,

    /// Maximum bytes of output the tool may return. `0` = no limit. Tools
    /// that produce large outputs should truncate and note the truncation.
    #[serde(default = "default_max_output")]
    pub max_output_bytes: usize,

    /// Whether outbound network is permitted in this sandbox.
    #[serde(default = "default_allow_net")]
    pub allow_net: bool,

    /// Filesystem roots the tool may read/write. Defaults to the whole
    /// filesystem (`/`); an explicitly empty list disables FS access for
    /// tools that consult this list.
    #[serde(default = "default_fs_paths")]
    pub allow_fs_paths: Vec<String>,

    /// Maximum bytes the tool may read from a single file.
    #[serde(default = "default_max_read")]
    pub max_read_bytes: usize,
}

fn default_timeout_ms() -> u64 {
    30_000
}
fn default_max_output() -> usize {
    1_000_000
}
fn default_allow_net() -> bool {
    true
}
fn default_max_read() -> usize {
    10_000_000
}

/// The whole filesystem. `check_path` treats a root of `/` as "everything".
fn default_fs_paths() -> Vec<String> {
    vec!["/".to_string()]
}

impl Default for Sandbox {
    fn default() -> Self {
        Self {
            timeout_ms: default_timeout_ms(),
            max_output_bytes: default_max_output(),
            allow_net: default_allow_net(),
            allow_fs_paths: default_fs_paths(),
            max_read_bytes: default_max_read(),
        }
    }
}

impl Sandbox {
    /// A locked-down sandbox: no net, no fs, short timeout. Good default for
    /// untrusted/composed tools.
    pub fn strict() -> Self {
        Self {
            timeout_ms: 5_000,
            max_output_bytes: 100_000,
            allow_net: false,
            allow_fs_paths: Vec::new(),
            max_read_bytes: 100_000,
        }
    }

    /// Check that a network operation is permitted.
    pub fn check_net(&self) -> Result<(), crate::error::ToolError> {
        if self.allow_net {
            Ok(())
        } else {
            Err(crate::error::ToolError::Sandbox(
                "network access is disabled in this sandbox".to_string(),
            ))
        }
    }

    /// Check that a path is under one of the allowed FS roots.
    ///
    /// Containment is proven, not guessed: the path is resolved via
    /// `canonicalize` when it exists (catching symlink escapes), and for
    /// not-yet-existing paths it is normalized *lexically* (`.` dropped,
    /// `..` resolved) before a component-wise prefix check against the
    /// canonical root. Anything that cannot be proven inside the sandbox is
    /// rejected — the function fails closed.
    pub fn check_path(&self, path: &str) -> Result<(), crate::error::ToolError> {
        if self.allow_fs_paths.is_empty() {
            return Err(crate::error::ToolError::Sandbox(
                "filesystem access is disabled in this sandbox".to_string(),
            ));
        }
        path_inside_roots(path, &self.allow_fs_paths)
    }

    /// Enforce the output-size cap. Truncates a returned string and appends a
    /// marker; returns an error if the value serializes above the limit and
    /// cannot be sensibly truncated.
    pub fn clamp_output(&self, value: &mut serde_json::Value) -> Result<(), crate::error::ToolError> {
        if self.max_output_bytes == 0 {
            return Ok(());
        }
        let s = serde_json::to_string(value).unwrap_or_default();
        if s.len() <= self.max_output_bytes {
            return Ok(());
        }
        // Try to truncate a string value in place.
        if let Some(str_val) = value.as_str() {
            let max = self.max_output_bytes.saturating_sub(40);
            if max > 0 && str_val.len() > max {
                let head: String = str_val.chars().take(max).collect();
                *value = serde_json::Value::String(format!(
                    "{}… [truncated: output exceeded {} bytes]",
                    head, self.max_output_bytes
                ));
                return Ok(());
            }
        }
        Err(crate::error::ToolError::OutputTooLarge {
            limit: self.max_output_bytes,
        })
    }
}

/// Split a path into its non-empty components (no `.`/`..` resolution).
fn components_of(p: &str) -> Vec<String> {
    p.split('/').filter(|c| !c.is_empty() && *c != ".").map(String::from).collect()
}

/// Normalize a path lexically: split on `/`, drop `.` and empty components,
/// and resolve `..` by popping. Fails closed if the path climbs above its
/// root (`/..`, `a/../..`) — such a path can never be proven to stay inside
/// a sandbox root.
fn lexical_components(path: &str) -> Result<Vec<String>, crate::error::ToolError> {
    let mut comps: Vec<String> = Vec::new();
    for part in path.split('/') {
        match part {
            "" | "." => {}
            ".." => {
                if comps.pop().is_none() {
                    return Err(crate::error::ToolError::Sandbox(format!(
                        "path `{path}` escapes above its root — not provably inside the sandbox"
                    )));
                }
            }
            other => comps.push(other.to_string()),
        }
    }
    Ok(comps)
}

/// Containment check shared by [`Sandbox::check_path`] and
/// [`ToolContext::check_path`](crate::tool::ToolContext::check_path):
/// `path` must be provably inside one of the canonical `roots`.
///
/// For paths that exist, `canonicalize` resolves symlinks first (so a
/// symlink inside an allowed root pointing outside is caught). For
/// not-yet-existing paths, lexical normalization is used instead — the
/// insecure fallback of a raw string prefix check is gone. Fails closed:
/// anything not provably inside is an error.
pub fn path_inside_roots(path: &str, roots: &[String]) -> Result<(), crate::error::ToolError> {
    let path_comps = match std::path::Path::new(path).canonicalize() {
        Ok(p) => components_of(&p.to_string_lossy()),
        // Not-yet-existing (or unreadable) path: normalize lexically and
        // fail closed on any `..` that climbs above the root.
        Err(_) => lexical_components(path)?,
    };
    for root in roots {
        let root_str = std::path::Path::new(root)
            .canonicalize()
            .map(|p| p.to_string_lossy().to_string())
            .unwrap_or_else(|_| root.clone());
        // A root of "/" (zero components) means the whole filesystem.
        let root_comps = if root_str == "/" {
            Vec::new()
        } else {
            lexical_components(&root_str)?
        };
        if path_comps.len() >= root_comps.len()
            && path_comps[..root_comps.len()] == root_comps[..]
        {
            return Ok(());
        }
    }
    Err(crate::error::ToolError::Sandbox(format!(
        "path `{path}` is outside the allowed filesystem roots"
    )))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn strict_blocks_net_and_fs() {
        let s = Sandbox::strict();
        assert!(s.check_net().is_err());
        assert!(s.check_path("/etc/passwd").is_err());
    }

    /// The shipped default sandbox is fully open: network allowed and the
    /// whole filesystem reachable. Empty FS roots remain available to
    /// embedders who construct one explicitly.
    #[test]
    fn default_sandbox_is_fully_open() {
        let s = Sandbox::default();
        assert!(s.allow_net);
        assert!(s.check_net().is_ok());
        assert!(s.check_path("/etc/passwd").is_ok());
        assert!(s.check_path("/home/z/project/src/main.rs").is_ok());
        // An explicitly empty list still means "no FS access".
        let locked = Sandbox { allow_fs_paths: Vec::new(), ..Sandbox::default() };
        assert!(locked.check_path("/tmp/x").is_err());
    }

    #[test]
    fn clamp_truncates_strings() {
        let s = Sandbox {
            max_output_bytes: 60,
            ..Sandbox::strict()
        };
        let mut v = serde_json::json!("x".repeat(500));
        assert!(s.clamp_output(&mut v).is_ok());
        let truncated = v.as_str().unwrap();
        assert!(truncated.contains("[truncated"));
    }

    /// A sandbox whose FS root is a fresh temp dir.
    fn rooted_sandbox() -> (Sandbox, std::path::PathBuf, std::path::PathBuf) {
        let base = std::env::temp_dir().join(format!(
            "rc-sbx-{}-{:?}",
            std::process::id(),
            std::thread::current().id()
        ));
        let root = base.join("allowed");
        std::fs::create_dir_all(&root).unwrap();
        let s = Sandbox {
            allow_fs_paths: vec![root.to_string_lossy().to_string()],
            ..Sandbox::default()
        };
        (s, base, root)
    }

    #[test]
    fn rejects_dotdot_escapes() {
        let (s, base, root) = rooted_sandbox();
        // The M21 attack: a relative path that escapes through `..` — with
        // the old raw-prefix fallback this passed for not-yet-existing paths.
        assert!(s.check_path("allowed/../../etc/cron.d/x").is_err());
        // Absolute escape through more `..` than there are components.
        let esc = root.join("sub/../../../etc/cron.d/x");
        assert!(s.check_path(esc.to_str().unwrap()).is_err());
        // `/..` climbs above the filesystem root.
        assert!(s.check_path("/..").is_err());
        // Plain outside path still rejected.
        assert!(s.check_path("/etc/passwd").is_err());
        std::fs::remove_dir_all(&base).ok();
    }

    #[test]
    fn not_yet_existing_path_inside_root_is_allowed() {
        let (s, base, root) = rooted_sandbox();
        // A file that does not exist yet, but provably inside the root.
        let inside = root.join("newdir/deep/file.txt");
        assert!(s.check_path(inside.to_str().unwrap()).is_ok());
        // `..` that stays inside resolves fine.
        let inside2 = root.join("a/../b/c.txt");
        assert!(s.check_path(inside2.to_str().unwrap()).is_ok());
        // Existing file inside the root still passes via canonicalize.
        let f = root.join("real.txt");
        std::fs::write(&f, "hi").unwrap();
        assert!(s.check_path(f.to_str().unwrap()).is_ok());
        std::fs::remove_dir_all(&base).ok();
    }

    #[cfg(unix)]
    #[test]
    fn symlink_escape_is_rejected() {
        let (s, base, root) = rooted_sandbox();
        let outside = base.join("outside");
        std::fs::create_dir_all(&outside).unwrap();
        let link = root.join("leak");
        std::os::unix::fs::symlink(&outside, &link).unwrap();
        // Existing path via the symlink canonicalizes outside → rejected.
        assert!(s.check_path(link.to_str().unwrap()).is_err());
        std::fs::remove_dir_all(&base).ok();
    }
}
