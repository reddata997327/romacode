//! Local repository engineering primitives used by the GitHub framework.
//! The module deliberately works through Git and project-native build tools,
//! while keeping orchestration, manifests, and reports in Rust.
use serde_json::{json, Value};
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::{SystemTime, UNIX_EPOCH};

fn run(program: &str, args: &[String], cwd: Option<&Path>) -> Result<String, String> {
    let mut c = Command::new(program);
    c.args(args);
    if let Some(dir) = cwd {
        c.current_dir(dir);
    }
    let out = c
        .output()
        .map_err(|e| format!("could not run {program}: {e}"))?;
    if !out.status.success() {
        return Err(format!(
            "{program} failed ({}): {}",
            out.status,
            String::from_utf8_lossy(&out.stderr).trim()
        ));
    }
    Ok(String::from_utf8_lossy(&out.stdout).into_owned())
}
fn stamp() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

pub fn clone_repo(args: &[String]) -> Result<String, String> {
    let remote = args.get(1).ok_or("missing repository URL or owner/name")?;
    // Scheme allow-list: only https:// is accepted verbatim. The owner/name
    // shorthand is rewritten to https://github.com/<owner>/<name>.git. Any
    // other scheme (file://, ssh://, git@, http://) is rejected — `file://`
    // can read local paths and `ssh://`/`git@` can probe or exfiltrate from
    // internal hosts.
    let source = if remote.starts_with("https://") {
        remote.to_string()
    } else if remote.contains("://") || remote.starts_with("git@") {
        return Err(format!(
            "refusing remote {remote:?}: only https:// URLs and the owner/name shorthand are accepted"
        ));
    } else {
        // owner/name shorthand — validate the characters so a value like
        // `../etc/passwd` cannot traverse.
        if !remote
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '/' | '-' | '_' | '.' | '~'))
        {
            return Err(format!(
                "refusing owner/name shorthand {remote:?}: contains characters outside [A-Za-z0-9/_.~-]"
            ));
        }
        format!("https://github.com/{remote}.git")
    };
    let dest = option(args, "--dest")
        .map(PathBuf::from)
        .unwrap_or_else(|| {
            PathBuf::from(
                remote
                    .rsplit('/')
                    .next()
                    .unwrap_or("repository")
                    .trim_end_matches(".git"),
            )
        });
    // Sandbox --dest: must be a relative path inside the current working
    // directory. Rejects absolute paths and `..` traversal.
    let cwd = std::env::current_dir().map_err(|e| format!("cwd: {e}"))?;
    let dest_abs = cwd.join(&dest);
    let canonical_parent = dest_abs
        .parent()
        .and_then(|p| std::fs::canonicalize(p).ok())
        .ok_or_else(|| format!("--dest parent directory does not exist: {}", dest.display()))?;
    if !canonical_parent.starts_with(&cwd) {
        return Err(format!(
            "--dest {} escapes the current working directory — refusing",
            dest.display()
        ));
    }
    // Validate option values: --depth must be a positive integer; --branch
    // and --commit must not start with `-` (argument-injection guard).
    let mut git: Vec<String> = vec!["clone".into()];
    if let Some(depth) = option(args, "--depth") {
        if !depth.chars().all(|c| c.is_ascii_digit()) || depth.is_empty() {
            return Err(format!("--depth must be a positive integer, got {depth:?}"));
        }
        git.push("--depth".to_string());
        git.push(depth.to_string());
    }
    if let Some(branch) = option(args, "--branch") {
        validate_git_value(&branch, "--branch")?;
        git.push("--branch".to_string());
        git.push(branch.to_string());
    }
    git.push(source.clone());
    git.push(dest.display().to_string());
    run("git", &git, None)?;
    if let Some(revision) = option(args, "--commit") {
        validate_git_value(&revision, "--commit")?;
        run(
            "git",
            &["checkout".to_string(), revision.to_string()],
            Some(&dest),
        )?;
    }
    let manifest = manifest(&dest, &source)?;
    write_manifest(&dest, &manifest)?;
    Ok(serde_json::to_string_pretty(&manifest).map_err(|e| e.to_string())?)
}

/// Reject values that begin with `-` (git argument-injection guard) or
/// contain path separators where git would not expect them. Git branch and
/// commit names accept a wide character set, but a leading `-` makes git
/// treat the value as an option, which is the classic injection class.
fn validate_git_value(value: &str, flag: &str) -> Result<(), String> {
    if value.is_empty() {
        return Err(format!("{flag} value must not be empty"));
    }
    if value.starts_with('-') {
        return Err(format!(
            "{flag} value {value:?} starts with '-' — refusing (argument-injection guard)"
        ));
    }
    if value.contains('\0') || value.chars().any(|c| c.is_control()) {
        return Err(format!(
            "{flag} value {value:?} contains a NUL or control character"
        ));
    }
    Ok(())
}

pub fn worktree(args: &[String]) -> Result<String, String> {
    let repo = PathBuf::from(args.get(1).ok_or("missing repository path")?);
    let branch = args
        .get(2)
        .cloned()
        .unwrap_or_else(|| format!("romacode/task-{}", stamp()));
    validate_git_value(&branch, "worktree branch")?;
    let dest = option(args, "--dest")
        .map(PathBuf::from)
        .unwrap_or_else(|| {
            repo.parent().unwrap_or(Path::new(".")).join(format!(
                "{}-worktree",
                repo.file_name().and_then(|s| s.to_str()).unwrap_or("repo")
            ))
        });
    run(
        "git",
        &[
            "worktree".into(),
            "add".into(),
            "-b".into(),
            branch.clone(),
            dest.display().to_string(),
        ],
        Some(&repo),
    )?;
    let m = manifest(
        &dest,
        &run(
            "git",
            &["remote".into(), "get-url".into(), "origin".into()],
            Some(&repo),
        )
        .unwrap_or_default(),
    )?;
    write_manifest(&dest, &m)?;
    Ok(serde_json::to_string_pretty(&m).map_err(|e| e.to_string())?)
}

pub fn inspect(args: &[String]) -> Result<String, String> {
    let dir = PathBuf::from(args.get(1).ok_or("missing repository path")?);
    let source = run("git", &["status".into(), "--short".into()], Some(&dir)).unwrap_or_default();
    let commit: String = run("git", &["rev-parse".into(), "HEAD".into()], Some(&dir))
        .unwrap_or_default()
        .trim()
        .into();
    let branch: String = run(
        "git",
        &["branch".into(), "--show-current".into()],
        Some(&dir),
    )
    .unwrap_or_default()
    .trim()
    .into();
    let remote: String = run(
        "git",
        &["remote".into(), "get-url".into(), "origin".into()],
        Some(&dir),
    )
    .unwrap_or_default()
    .trim()
    .into();
    let mut files = Vec::new();
    collect_files(&dir, &dir, &mut files, 0);
    let kind = detect_kind(&dir, &files);
    let tests = files
        .iter()
        .filter(|f| f.to_ascii_lowercase().contains("test") || f.contains("spec"))
        .count();
    let result = json!({"path": dir, "remote": remote, "branch": branch, "commit": commit, "kind": kind, "file_count": files.len(), "test_file_count": tests, "dirty": !source.trim().is_empty(), "status": source.lines().collect::<Vec<_>>(), "files": files});
    serde_json::to_string_pretty(&result).map_err(|e| e.to_string())
}

pub fn apply_patch(args: &[String]) -> Result<String, String> {
    let dir = PathBuf::from(args.get(1).ok_or("missing repository path")?);
    let patch = PathBuf::from(args.get(2).ok_or("missing patch file")?);
    let text = std::fs::read_to_string(&patch).map_err(|e| format!("read patch: {e}"))?;
    if text.trim().is_empty() {
        return Err("patch is empty".into());
    }
    let out = Command::new("git")
        .args([
            "apply",
            "--check",
            patch.to_str().ok_or("invalid patch path")?,
        ])
        .current_dir(&dir)
        .output()
        .map_err(|e| e.to_string())?;
    if !out.status.success() {
        return Err(format!(
            "patch check failed: {}",
            String::from_utf8_lossy(&out.stderr).trim()
        ));
    }
    run(
        "git",
        &["apply".into(), patch.display().to_string()],
        Some(&dir),
    )?;
    Ok(json!({"applied": true, "repository": dir, "patch_bytes": text.len(), "status": run("git", &["status".into(), "--short".into()], Some(&dir)).unwrap_or_default()}).to_string())
}

pub fn validate(args: &[String]) -> Result<String, String> {
    let dir = PathBuf::from(args.get(1).ok_or("missing repository path")?);
    let kind = option(args, "--kind")
        .map(str::to_string)
        .unwrap_or_else(|| detect_kind(&dir, &Vec::new()));
    let commands: Vec<Vec<String>> = match kind.as_str() {
        "rust" => vec![
            vec!["cargo".into(), "fmt".into(), "--check".into()],
            vec!["cargo".into(), "check".into(), "--all-targets".into()],
            vec!["cargo".into(), "test".into(), "--workspace".into()],
        ],
        "node" => vec![vec!["npm".into(), "test".into()]],
        "python" => vec![vec!["python3".into(), "-m".into(), "pytest".into()]],
        "go" => vec![vec!["go".into(), "test".into(), "./...".into()]],
        _ => vec![vec!["git".into(), "diff".into(), "--check".into()]],
    };
    let mut checks = Vec::new();
    let mut passed = true;
    for command in commands {
        let program = command[0].clone();
        let args2 = command[1..].to_vec();
        match run(&program, &args2, Some(&dir)) {
            Ok(out) => {
                checks.push(json!({"command": command.join(" "), "passed": true, "output": out}))
            }
            Err(e) => {
                passed = false;
                checks.push(json!({"command": command.join(" "), "passed": false, "error": e}));
                break;
            }
        }
    }
    let report = json!({"repository": dir, "kind": kind, "passed": passed, "checks": checks, "generated_at": stamp()});
    let out = option(args, "--out").unwrap_or("romacode-validation.json");
    std::fs::write(out, serde_json::to_string_pretty(&report).unwrap())
        .map_err(|e| format!("write report: {e}"))?;
    Ok(serde_json::to_string_pretty(&report).unwrap())
}

pub fn diff(args: &[String]) -> Result<String, String> {
    let dir = PathBuf::from(args.get(1).ok_or("missing repository path")?);
    run("git", &["diff".into(), "--stat".into()], Some(&dir)).map_err(|e| e.to_string())
}
pub fn archive(args: &[String]) -> Result<String, String> {
    let dir = PathBuf::from(args.get(1).ok_or("missing repository path")?);
    let out = args.get(2).cloned().unwrap_or_else(|| {
        format!(
            "{}-romacode.zip",
            dir.file_name()
                .and_then(|x| x.to_str())
                .unwrap_or("repository")
        )
    });
    run(
        "git",
        &[
            "archive".into(),
            "--format=zip".into(),
            "-o".into(),
            out.clone(),
            "HEAD".into(),
        ],
        Some(&dir),
    )?;
    Ok(json!({"archive": out, "repository": dir}).to_string())
}

fn manifest(dir: &Path, remote: &str) -> Result<Value, String> {
    Ok(
        json!({"repository": dir, "remote": remote.trim(), "branch": run("git", &["branch".into(), "--show-current".into()], Some(dir)).unwrap_or_default().trim(), "commit": run("git", &["rev-parse".into(), "HEAD".into()], Some(dir)).unwrap_or_default().trim(), "created_at": stamp(), "dirty": !run("git", &["status".into(), "--short".into()], Some(dir)).unwrap_or_default().trim().is_empty()}),
    )
}
fn write_manifest(dir: &Path, value: &Value) -> Result<(), String> {
    std::fs::write(
        dir.join(".romacode-manifest.json"),
        serde_json::to_string_pretty(value).unwrap(),
    )
    .map_err(|e| format!("write manifest: {e}"))
}
fn collect_files(root: &Path, dir: &Path, files: &mut Vec<String>, depth: usize) {
    if depth > 8 {
        return;
    }
    if let Ok(read) = std::fs::read_dir(dir) {
        for item in read.flatten() {
            let p = item.path();
            let name = item.file_name().to_string_lossy().to_string();
            if name == ".git" || name == "target" || name == "node_modules" {
                continue;
            }
            if p.is_dir() {
                collect_files(root, &p, files, depth + 1);
            } else if p.is_file() {
                if let Ok(r) = p.strip_prefix(root) {
                    files.push(r.display().to_string());
                }
            }
        }
    }
}
fn detect_kind(dir: &Path, files: &[String]) -> String {
    if dir.join("Cargo.toml").exists() || files.iter().any(|f| f == "Cargo.toml") {
        "rust"
    } else if dir.join("package.json").exists() {
        "node"
    } else if dir.join("pyproject.toml").exists() || dir.join("setup.py").exists() {
        "python"
    } else if dir.join("go.mod").exists() {
        "go"
    } else if dir.join("pom.xml").exists() || dir.join("build.gradle").exists() {
        "java"
    } else {
        "unknown"
    }
    .into()
}
fn option<'a>(args: &'a [String], key: &str) -> Option<&'a str> {
    args.windows(2).find(|w| w[0] == key).map(|w| w[1].as_str())
}
pub fn handles(s: &str) -> bool {
    matches!(
        s,
        "clone"
            | "download"
            | "worktree"
            | "inspect"
            | "apply-patch"
            | "validate"
            | "diff"
            | "archive"
    )
}
pub fn run_cli(args: &[String]) -> Result<String, String> {
    match args.first().map(String::as_str).unwrap_or("") {
        "clone" | "download" => clone_repo(args),
        "worktree" => worktree(args),
        "inspect" => inspect(args),
        "apply-patch" => apply_patch(args),
        "validate" => validate(args),
        "diff" => diff(args),
        "archive" => archive(args),
        _ => Err("unknown engineering command".into()),
    }
}
