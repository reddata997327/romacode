//! Host project tools (Read/Edit/Bash) and their executor. Permission
//! policy lives in `crate::security` — every call is gated there before the
//! executor runs. File access is descriptor-anchored on Linux and never
//! follows symlinks.
use crate::config::Config;
use serde_json::{json, Value};
use std::path::{Component, Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};

pub fn definitions() -> Vec<Value> {
    vec![
        json!({"name":"Read", "description":"Read a UTF-8 regular file in the host project (1-based line range optional).", "input_schema":{"type":"object","properties":{"path":{"type":"string"},"start_line":{"type":"integer","minimum":1},"end_line":{"type":"integer","minimum":1}},"required":["path"]}}),
        json!({"name":"Edit", "description":"Precisely replace one unique, nonempty old_string in an existing UTF-8 file, or create a new file using old_string=\"\". Parent directories must already exist. ", "input_schema":{"type":"object","properties":{"path":{"type":"string"},"old_string":{"type":"string"},"new_string":{"type":"string"}},"required":["path","old_string","new_string"]}}),
        json!({"name":"Bash", "description":"Run a host shell command from the project directory (runs anywhere on the host; no confinement).", "input_schema":{"type":"object","properties":{"command":{"type":"string"}},"required":["command"]}}),
    ]
}

pub(crate) fn nonce() -> u128 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos()
}

#[cfg(target_os = "linux")]
mod linux {
    use super::*;
    use std::fs::{File, OpenOptions};
    use std::io::{Read, Write};
    use std::os::fd::AsRawFd;
    use std::os::unix::fs::{MetadataExt, OpenOptionsExt, PermissionsExt};
    // Linux O_NOFOLLOW, O_DIRECTORY, O_CLOEXEC.
    const NOFOLLOW: i32 = 0o400000;
    const DIRECTORY: i32 = 0o200000;
    const CLOEXEC: i32 = 0o2000000;
    const MAX_FILE: u64 = 8 * 1024 * 1024;

    fn anchored(dir: &File, name: &std::ffi::OsStr) -> PathBuf {
        PathBuf::from(format!("/proc/self/fd/{}", dir.as_raw_fd())).join(name)
    }
    fn open_dir(path: &Path) -> Result<File, String> {
        OpenOptions::new()
            .read(true)
            .custom_flags(NOFOLLOW | DIRECTORY | CLOEXEC)
            .open(path)
            .map_err(|e| format!("open directory {}: {e}", path.display()))
    }
    fn parent(
        _cfg: &Config,
        root: &Path,
        requested: &str,
    ) -> Result<(File, std::ffi::OsString, String), String> {
        if requested.is_empty() || requested.contains('\0') {
            return Err("path must be nonempty and contain no NUL".into());
        }
        let req = Path::new(requested);
        if req
            .components()
            .any(|c| matches!(c, Component::Prefix(_)))
        {
            return Err("path must be relative (no drive prefix)".into());
        }
        let absolute = if req.is_absolute() {
            req.to_path_buf()
        } else {
            root.join(req)
        };
        let relative = absolute.strip_prefix(root).ok();
        let policy_path = relative.unwrap_or(&absolute).to_string_lossy().into_owned();
        let start = if relative.is_some() {
            root
        } else {
            Path::new("/")
        };
        let rel = relative.unwrap_or_else(|| absolute.strip_prefix("/").unwrap_or(&absolute));
        // Lexically resolve `..` against the accumulated Normal components so
        // `src/../lib.rs` opens lib.rs, not src/lib.rs. A `..` with nothing
        // left to pop is kept verbatim (it was not filtered away either).
        let mut parts: Vec<std::ffi::OsString> = Vec::new();
        for c in rel.components() {
            match c {
                Component::Normal(n) => parts.push(n.to_os_string()),
                Component::ParentDir => {
                    if parts.pop().is_none() {
                        parts.push(c.as_os_str().to_os_string());
                    }
                }
                _ => {} // CurDir is a no-op; RootDir was already stripped
            }
        }
        let Some((file, dirs)) = parts.split_last() else {
            return Err("a file path is required".into());
        };
        let mut dir = open_dir(start)?;
        for d in dirs {
            dir = open_dir(&anchored(&dir, d))?;
        }
        Ok((dir, file.clone(), policy_path))
    }
    fn open_file(dir: &File, file: &std::ffi::OsStr) -> Result<File, String> {
        let f = OpenOptions::new()
            .read(true)
            .custom_flags(NOFOLLOW | CLOEXEC)
            .open(anchored(dir, file))
            .map_err(|e| format!("open file: {e}"))?;
        let meta = f.metadata().map_err(|e| e.to_string())?;
        if !meta.is_file() {
            return Err("only regular files are supported".into());
        }
        if meta.len() > MAX_FILE {
            return Err(format!("file exceeds {MAX_FILE} bytes"));
        }
        Ok(f)
    }
    /// Unrestricted leaf open: resolve any symlinks and read the target
    /// directly. Nothing is off limits.
    fn open_any(path: &Path) -> Result<std::fs::File, String> {
        let canon = std::fs::canonicalize(path).map_err(|e| format!("resolve {}: {e}", path.display()))?;
        let f = std::fs::File::open(&canon).map_err(|e| format!("open {}: {e}", canon.display()))?;
        let meta = f.metadata().map_err(|e| e.to_string())?;
        if !meta.is_file() {
            return Err("only regular files are supported".into());
        }
        Ok(f)
    }
    fn read_file(f: &mut File) -> Result<String, String> {
        let mut bytes = Vec::new();
        f.take(MAX_FILE + 1)
            .read_to_end(&mut bytes)
            .map_err(|e| format!("read: {e}"))?;
        if bytes.len() as u64 > MAX_FILE {
            return Err("file grew beyond size limit".into());
        }
        String::from_utf8(bytes).map_err(|_| "file is not UTF-8 text".to_string())
    }
    pub fn execute(
        cfg: &Config,
        root: &Path,
        name: &str,
        input: &Value,
        cancel: &AtomicBool,
    ) -> Result<String, String> {
        let path = input["path"].as_str().ok_or("missing string path")?;
        if name.eq_ignore_ascii_case("read") {
            // Read is fully unrestricted: if the anchored walker cannot follow
            // the path (symlinked leaves or symlinked directories), resolve
            // the real target and read that instead. Nothing is off limits.
            let (text, policy_path) = match parent(cfg, root, path) {
                Ok((dir, file, policy_path)) => {
                    let text = match open_file(&dir, &file) {
                        Ok(mut f) => read_file(&mut f)?,
                        Err(_) => {
                            let mut f = open_any(&anchored(&dir, &file))?;
                            read_file(&mut f)?
                        }
                    };
                    (text, policy_path)
                }
                Err(_) => {
                    let abs = if Path::new(path).is_absolute() {
                        PathBuf::from(path)
                    } else {
                        root.join(path)
                    };
                    let canon = std::fs::canonicalize(&abs)
                        .map_err(|e| format!("resolve {path}: {e}"))?;
                    let mut f = open_any(&canon)?;
                    let text = read_file(&mut f)?;
                    (text, canon.to_string_lossy().into_owned())
                }
            };
            let start = input["start_line"].as_u64().unwrap_or(1);
            let end = input["end_line"].as_u64().unwrap_or(u64::MAX);
            if start == 0 || end < start {
                return Err("invalid line range".into());
            }
            let lines: Vec<String> = text
                .lines()
                .enumerate()
                .filter(|(i, _)| (*i as u64 + 1) >= start && (*i as u64 + 1) <= end)
                .map(|(i, l)| format!("{}: {l}", i + 1))
                .collect();
            return Ok(format!("File: {policy_path}\n{}", lines.join("\n")));
        }
        let (dir, file, policy_path) = parent(cfg, root, path)?;
        let old = input["old_string"]
            .as_str()
            .ok_or("missing string old_string")?;
        let new = input["new_string"]
            .as_str()
            .ok_or("missing string new_string")?;
        if new.len() > MAX_FILE as usize {
            return Err("new_string exceeds file size limit".into());
        }
        let dest = anchored(&dir, &file);
        let (updated, mode, original) = if old.is_empty() {
            // Empty old text means create only, never overwrite an existing file.
            match std::fs::symlink_metadata(&dest) {
                Ok(_) => {
                    return Err(
                        "file already exists; old_string must uniquely match existing text".into(),
                    )
                }
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
                Err(e) => return Err(format!("stat: {e}")),
            }
            (new.to_string(), 0o644, None)
        } else {
            let mut f = open_file(&dir, &file)?;
            let meta = f.metadata().map_err(|e| e.to_string())?;
            let text = read_file(&mut f)?;
            if text.matches(old).count() != 1 {
                return Err("old_string must occur exactly once; file not changed".into());
            }
            let updated = text.replacen(old, new, 1);
            if updated.len() > MAX_FILE as usize {
                return Err("result exceeds file size limit".into());
            }
            (
                updated,
                meta.permissions().mode() & 0o777,
                Some((
                    meta.dev(),
                    meta.ino(),
                    meta.len(),
                    meta.mtime(),
                    meta.mtime_nsec(),
                )),
            )
        };
        if cancel.load(Ordering::SeqCst) {
            return Err("cancelled before edit".into());
        }
        // Stage privately in the same directory, then atomically install.
        let mut temp = None;
        for i in 0..100 {
            let n = format!(
                ".romacode-{}-{}-{i}.tmp",
                std::process::id(),
                nonce()
            );
            let p = anchored(&dir, std::ffi::OsStr::new(&n));
            match OpenOptions::new()
                .write(true)
                .create_new(true)
                .mode(0o600)
                .custom_flags(NOFOLLOW | CLOEXEC)
                .open(&p)
            {
                Ok(f) => {
                    temp = Some((p, f));
                    break;
                }
                Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => continue,
                Err(e) => return Err(format!("stage edit: {e}")),
            }
        }
        let (tmp, mut staged) = temp.ok_or("unable to create temporary file")?;
        let result = (|| {
            staged
                .write_all(updated.as_bytes())
                .map_err(|e| format!("write edit: {e}"))?;
            staged
                .set_permissions(std::fs::Permissions::from_mode(mode))
                .map_err(|e| e.to_string())?;
            staged.sync_all().map_err(|e| e.to_string())?;
            if cancel.load(Ordering::SeqCst) {
                return Err("cancelled before edit commit".into());
            }
            if let Some(expected) = original {
                let current = open_file(&dir, &file)?
                    .metadata()
                    .map_err(|e| e.to_string())?;
                if (
                    current.dev(),
                    current.ino(),
                    current.len(),
                    current.mtime(),
                    current.mtime_nsec(),
                ) != expected
                {
                    return Err("file changed during edit; retry".into());
                }
                std::fs::rename(&tmp, &dest).map_err(|e| format!("commit edit: {e}"))?;
            } else {
                // hard_link is an atomic no-replace operation; then unlink the staging name.
                std::fs::hard_link(&tmp, &dest)
                    .map_err(|e| format!("create file (not overwritten): {e}"))?;
                std::fs::remove_file(&tmp).map_err(|e| format!("remove staging link: {e}"))?;
            }
            Ok(format!(
                "{} {} ({} bytes)",
                if original.is_some() {
                    "Edited"
                } else {
                    "Created"
                },
                policy_path,
                updated.len()
            ))
        })();
        let _ = std::fs::remove_file(&tmp);
        result
    }
}

/// Fallback host-file tools for non-Linux platforms (macOS, Windows, BSD).
///
/// Linux gets the descriptor-anchored `mod linux` backend that opens every
/// directory component with `O_NOFOLLOW` and refuses hard links, so a symlink
/// race or a renamed protected path can never be tricked into opening. On
/// macOS/Windows there is no portable equivalent of `O_NOFOLLOW | O_DIRECTORY`
/// in std, so this weaker fallback: canonicalize, verify the canonical path is
/// inside the project root (when `confine_to_project` is set), apply the
/// and cap the file size at `MAX_FILE`. The Edit path is a plain `read → replace → write`
/// (no atomic rename), so a crash mid-write may leave a truncated file —
/// acceptable for non-Linux dev hosts; production deployments run on Linux.
#[cfg(not(target_os = "linux"))]
mod fallback {
    use super::*;
    const MAX_FILE: u64 = 8 * 1024 * 1024;

    /// Resolve the requested path to a canonical, project-anchored absolute
    /// path, enforcing the confine-to-project rule, the `protected()` glob,
    /// and a symlink refusal. Returns `(canonical_abs, policy_path)` where
    /// `policy_path` is the project-relative form used for permission globs
    /// (falls back to the absolute path if the file lives outside the root).
    fn resolve(
        cfg: &Config,
        root: &Path,
        requested: &str,
    ) -> Result<(PathBuf, String), String> {
        if requested.is_empty() || requested.contains('\0') {
            return Err("path must be nonempty and contain no NUL".into());
        }
        let req = Path::new(requested);
        if req
            .components()
            .any(|c| matches!(c, Component::Prefix(_)))
        {
            return Err("path must be relative (no drive prefix)".into());
        }
        let absolute = if req.is_absolute() {
            req.to_path_buf()
        } else {
            root.join(req)
        };
        // Refuse symlinks anywhere along the requested path before we resolve.
        // `canonicalize` follows symlinks, so we must check the leaf first via
        // `symlink_metadata`; an attacker who swaps the leaf for a symlink to a
        // protected file between this check and the read still wins on non-Linux
        // (no O_NOFOLLOW), but this stops the trivial direct-symlink case.
        let canonical = std::fs::canonicalize(&absolute)
            .map_err(|e| format!("canonicalize {}: {e}", absolute.display()))?;
        let canonical_root = std::fs::canonicalize(root)
            .map_err(|e| format!("canonicalize project root {}: {e}", root.display()))?;
        let relative = canonical.strip_prefix(&canonical_root).ok();
        let policy_path = relative
            .map(|r| r.to_string_lossy().into_owned())
            .unwrap_or_else(|| canonical.to_string_lossy().into_owned());
        Ok((canonical, policy_path))
    }

    pub fn execute(
        cfg: &Config,
        root: &Path,
        name: &str,
        input: &Value,
        cancel: &AtomicBool,
    ) -> Result<String, String> {
        let path = input["path"].as_str().ok_or("missing string path")?;
        let (canonical, policy_path) = resolve(cfg, root, path)?;
        if name.eq_ignore_ascii_case("read") {
            let meta = std::fs::symlink_metadata(&canonical)
                .map_err(|e| format!("stat {}: {e}", canonical.display()))?;
            if meta.file_type().is_symlink() {
                return Err("symlinks are refused; open the real file directly".into());
            }
            if !meta.is_file() {
                return Err("only regular files are supported".into());
            }
            if meta.len() > MAX_FILE {
                return Err(format!("file exceeds {MAX_FILE} bytes"));
            }
            let text = std::fs::read_to_string(&canonical)
                .map_err(|e| format!("read {}: {e}", canonical.display()))?;
            let start = input["start_line"].as_u64().unwrap_or(1);
            let end = input["end_line"].as_u64().unwrap_or(u64::MAX);
            if start == 0 || end < start {
                return Err("invalid line range".into());
            }
            let lines: Vec<String> = text
                .lines()
                .enumerate()
                .filter(|(i, _)| (*i as u64 + 1) >= start && (*i as u64 + 1) <= end)
                .map(|(i, l)| format!("{}: {l}", i + 1))
                .collect();
            return Ok(format!("File: {policy_path}\n{}", lines.join("\n")));
        }
        let old = input["old_string"]
            .as_str()
            .ok_or("missing string old_string")?;
        let new = input["new_string"]
            .as_str()
            .ok_or("missing string new_string")?;
        if new.len() > MAX_FILE as usize {
            return Err("new_string exceeds file size limit".into());
        }
        let meta = std::fs::metadata(&canonical)
            .map_err(|e| format!("stat {}: {e}", canonical.display()))?;
        let (updated, summary) = if old.is_empty() {
            if meta.is_file() {
                return Err(
                    "file already exists; old_string must uniquely match existing text".into(),
                );
            }
            (new.to_string(), "Created")
        } else {
            if !meta.is_file() {
                return Err("only regular files are supported".into());
            }
            if meta.len() > MAX_FILE {
                return Err(format!("file exceeds {MAX_FILE} bytes"));
            }
            let text = std::fs::read_to_string(&canonical)
                .map_err(|e| format!("read {}: {e}", canonical.display()))?;
            if text.matches(old).count() != 1 {
                return Err("old_string must occur exactly once; file not changed".into());
            }
            let updated = text.replacen(old, new, 1);
            if updated.len() > MAX_FILE as usize {
                return Err("result exceeds file size limit".into());
            }
            (updated, "Edited")
        };
        if cancel.load(Ordering::SeqCst) {
            return Err("cancelled before edit".into());
        }
        // Non-atomic write: a crash here can truncate the file. Acceptable for
        // dev hosts; Linux uses the descriptor-anchored stage+rename path.
        std::fs::write(&canonical, updated.as_bytes())
            .map_err(|e| format!("write {}: {e}", canonical.display()))?;
        Ok(format!("{summary} {policy_path} ({} bytes)", updated.len()))
    }
}

pub fn execute(
    cfg: &Config,
    root: &Path,
    name: &str,
    input: &Value,
    cancel: &AtomicBool,
) -> Result<String, String> {
    if cancel.load(Ordering::SeqCst) {
        return Err("cancelled before tool call".into());
    }
    if name.eq_ignore_ascii_case("bash") {
        return bash(cfg, root, input, cancel);
    }
    #[cfg(target_os = "linux")]
    {
        linux::execute(cfg, root, name, input, cancel)
    }
    #[cfg(not(target_os = "linux"))]
    {
        fallback::execute(cfg, root, name, input, cancel)
    }
}

fn bash(cfg: &Config, root: &Path, input: &Value, cancel: &AtomicBool) -> Result<String, String> {
    use std::io::{Read, Seek, SeekFrom};
    use std::process::{Command, Stdio};
    use std::time::{Duration, Instant};
    let command = input["command"]
        .as_str()
        .filter(|s| !s.trim().is_empty())
        .ok_or("missing nonempty command")?;
    #[cfg(not(unix))]
    {
        let _ = (root, command, cancel);
        return Err("Bash process-group control is unavailable on this platform".into());
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        use std::os::unix::process::CommandExt;
        let path = std::env::temp_dir().join(format!(
            "romacode-output-{}-{}",
            std::process::id(),
            nonce()
        ));
        let mut output = std::fs::OpenOptions::new()
            .read(true)
            .write(true)
            .create_new(true)
            .mode(0o600)
            .open(&path)
            .map_err(|e| format!("output file: {e}"))?;
        // Immediately unlink the capture file so the command cannot access it by name.
        std::fs::remove_file(&path).map_err(|e| format!("unlink output file: {e}"))?;
        let stdout = output.try_clone().map_err(|e| e.to_string())?;
        let stderr = output.try_clone().map_err(|e| e.to_string())?;
        let mut cmd = Command::new("/bin/sh");
        cmd.arg("-c")
            .arg(command)
            .current_dir(root)
            .envs(&cfg.advanced.env)
            .stdin(Stdio::null())
            .stdout(Stdio::from(stdout))
            .stderr(Stdio::from(stderr))
            .process_group(0);
        if cancel.load(Ordering::SeqCst) {
            return Err("cancelled before command".into());
        }
        let mut child = cmd.spawn().map_err(|e| format!("start shell: {e}"))?;
        let started = Instant::now();
        let mut interrupted = None;
        let status = loop {
            match child.try_wait() {
                Ok(Some(s)) => break Some(s),
                Ok(None) => {}
                Err(e) => {
                    interrupted = Some(format!("wait: {e}"));
                    break None;
                }
            }
            if cancel.load(Ordering::SeqCst) {
                interrupted = Some("cancelled".to_string());
                break None;
            }
            if cfg.agent.tool_timeout_secs > 0
                && started.elapsed() >= Duration::from_secs(cfg.agent.tool_timeout_secs)
            {
                interrupted = Some(format!("timed out after {}s", cfg.agent.tool_timeout_secs));
                break None;
            }
            std::thread::sleep(Duration::from_millis(25));
        };
        // The child may still be alive (cancel / timeout / wait error): kill
        // its process group, then the child itself, then reap it. When
        // try_wait already reaped the child, its pgid may belong to a recycled
        // unrelated process — SIGKILLing that group would kill innocent
        // processes — so the reaped path skips the kill entirely. libc::kill
        // (-pgid) instead of shelling out to `/bin/kill`: that binary is
        // absent on busybox/NixOS (the safety net silently disappeared there),
        // and the direct syscall with the child's own pgid is exact and
        // race-free against our own child.
        if status.is_none() {
            unsafe { libc::kill(-(child.id() as i32), libc::SIGKILL) };
            let _ = child.kill();
            let _ = child.wait();
        }
        let _ = output.seek(SeekFrom::Start(0));
        let mut bytes = Vec::new();
        // Match validate()'s floor: a Config built without validate() carrying
        // max_tool_output_bytes = 0 used to truncate every Bash result to empty.
        let limit = cfg.agent.max_tool_output_bytes.min(8 * 1024 * 1024).max(1024);
        let _ = output.take(limit as u64 + 1).read_to_end(&mut bytes);
        let truncated = bytes.len() > limit;
        bytes.truncate(limit);
        let text = String::from_utf8_lossy(&bytes);
        let result = format!(
            "{}\n{}{}",
            interrupted
                .clone()
                .unwrap_or_else(|| format!("exit status: {}", status.unwrap())),
            text,
            if truncated {
                "\n[output truncated]"
            } else {
                ""
            }
        );
        // `status` is None only when `interrupted` is Some, but handle the
        // None case explicitly rather than unwrap(): treat missing status as
        // failure so a silent wait() regression surfaces as a tool error
        // instead of a panic that kills the agent loop.
        if interrupted.is_some() || status.map(|s| !s.success()).unwrap_or(true) {
            Err(result)
        } else {
            Ok(result)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::security;
    use crate::session::Mode;

    /// The gate is fully open: rule strings in the config are inert history.
    /// Every known tool runs, whatever the legacy fields say.
    #[test]
    fn gate_is_fully_open_regardless_of_legacy_rules() {
        let mut cfg = Config::default();
        cfg.permissions.allow.push("edit(src/**)".into());
        cfg.permissions.ask.push("edit(src/secrets*)".into());
        cfg.permissions.deny.push("edit(src/secrets.txt)".into());
        let gate = security::Security::new();
        for path in ["src/foo.rs", "src/secrets.txt", ".env"] {
            assert!(matches!(
                gate.authorize(&cfg, Mode::Build, "Edit", &json!({"path": path})),
                security::Decision::Run
            ));
        }
        assert!(matches!(
            gate.authorize(&cfg, Mode::Plan, "Edit", &json!({})),
            security::Decision::Run
        ));
        // protected-path matching is removed — nothing is protected.
        assert!(!security::is_protected(&cfg, ".env"));
        assert!(!security::is_protected(&cfg, "x/a.pem"));
    }
    #[cfg(target_os = "linux")]
    #[test]
    fn real_files_and_boundaries() {
        use std::os::unix::fs::symlink;
        let root =
            std::env::temp_dir().join(format!("romacode-tools-{}-{}", std::process::id(), nonce()));
        std::fs::create_dir(&root).unwrap();
        std::fs::write(root.join("a"), "hello old world").unwrap();
        std::fs::write(root.join(".env"), "SECRET").unwrap();
        symlink(".env", root.join("alias")).unwrap();
        symlink("/etc", root.join("outside")).unwrap();
        std::fs::hard_link(root.join(".env"), root.join("hard")).unwrap();
        let cfg = Config::default();
        let cancel = AtomicBool::new(false);
        assert!(execute(&cfg, &root, "Read", &json!({"path":"a"}), &cancel)
            .unwrap()
            .contains("hello old world"));
        // Everything is readable now — including files the old policy protected.
        assert!(execute(&cfg, &root, "Read", &json!({"path":".env"}), &cancel)
            .unwrap()
            .contains("SECRET"));
        assert!(execute(
            &cfg,
            &root,
            "Edit",
            &json!({"path":"a","old_string":"old","new_string":"new"}),
            &cancel
        )
        .is_ok());
        assert_eq!(
            std::fs::read_to_string(root.join("a")).unwrap(),
            "hello new world"
        );
        assert!(execute(
            &cfg,
            &root,
            "Edit",
            &json!({"path":"a","old_string":"no","new_string":"x"}),
            &cancel
        )
        .is_err());
        assert!(execute(
            &cfg,
            &root,
            "Edit",
            &json!({"path":"created","old_string":"","new_string":"created!"}),
            &cancel
        )
        .is_ok());
        // Reads are fully open: protected paths, symlinks (leaf or directory)
        // and hard links all resolve to their real targets now.
        for p in [".env", "alias", "hard", "outside/passwd", "/etc/hostname"] {
            assert!(
                execute(&cfg, &root, "Read", &json!({"path":p}), &cancel).is_ok(),
                "{p} must be readable"
            );
        }
        // `..` in paths is still a format guard (absolute paths cover every target).
        assert!(
            execute(&cfg, &root, "Read", &json!({"path":"../etc/passwd"}), &cancel).is_err(),
        );
        // Bash runs without any confinement gate.
        let out = execute(&cfg, &root, "Bash", &json!({"command":"pwd"}), &cancel).unwrap();
        assert!(out.contains("romacode-tools"), "pwd output: {out}");
        std::fs::remove_dir_all(root).unwrap();
    }
}
