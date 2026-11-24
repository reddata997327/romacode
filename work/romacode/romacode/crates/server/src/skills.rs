//! Agent skills — Claude-Code-style SKILL.md packages for RomaCode.
//!
//! A skill is a directory containing a `SKILL.md`: an optional `---`
//! frontmatter with `name` / `description`, followed by the instruction body
//! the agent must follow once the skill is activated. Skills are the
//! packaging format for reusable, on-demand expertise; the catalog (name +
//! description per skill) is advertised in the system prompt so the agent
//! knows what exists, while the full body is only loaded when activated —
//! the same progressive-disclosure model Claude Code uses.
//!
//! Three discovery layers, later ones override same-name earlier ones:
//!
//! 1. `builtin` — the library embedded in the binary at compile time
//!    (`crates/server/skills/*/SKILL.md`, referenced with `include_str!`);
//! 2. `user` — `<config_dir>/romacode/skills/*/SKILL.md`, shared by every
//!    project of this machine;
//! 3. `project` — `<project-root>/.romacode/skills/*/SKILL.md`, private to
//!    the repository being worked on.
//!
//! Two activation paths exist and both end in the same place — the skill body
//! in front of the agent:
//!
//! * the `skill` TOOL (agent-initiated): the model calls `skill {"name":
//!   "docx-report"}` and the body comes back as the tool result;
//! * the `/skill <name>` slash command (user-initiated): the brain starts a
//!   turn whose prompt carries the body (see [`crate::api`]).
//!
//! `/skills` lists the catalog from any client.
use serde_json::{json, Value};
use std::path::PathBuf;

/// Hard cap on a single skill body — a run-away SKILL.md must not flood the
/// context window when activated.
const MAX_BODY_CHARS: usize = 32_000;
/// Hard cap on how many skills one discovery layer may contribute.
const MAX_SKILLS_PER_DIR: usize = 256;

/// One parsed skill.
#[derive(Clone, Debug, PartialEq)]
pub struct Skill {
    pub name: String,
    pub description: String,
    /// The instruction body (everything after the frontmatter), trimmed.
    pub body: String,
    /// Where it was found: `builtin`, `user` or `project`.
    pub source: &'static str,
}

impl Skill {
    /// Parse a SKILL.md. `fallback_name` is used when the frontmatter has no
    /// `name` (typically the directory name). Never fails: an unparsable file
    /// degrades to "(no description)" metadata or is skipped when empty.
    pub fn parse(text: &str, fallback_name: &str, source: &'static str) -> Option<Skill> {
        let text = text.trim_start_matches('\u{feff}');
        let (front, body) = if let Some(rest) = text.strip_prefix("---") {
            // frontmatter ends at the next `---` line
            match rest.split_once("\n---") {
                Some((fm, after)) => {
                    // drop the remainder of the closing line (e.g. `---\r`)
                    let after = after.strip_prefix('\n').unwrap_or(after);
                    (Some(fm), after)
                }
                // Unterminated frontmatter: treat the stray `---` as noise and
                // keep EVERYTHING as the body — losing a whole skill body to a
                // missing marker is worse than losing its name keys.
                None => (None, rest),
            }
        } else {
            (None, text)
        };
        let mut name = String::new();
        let mut description = String::new();
        if let Some(fm) = front {
            for line in fm.lines() {
                let line = line.trim();
                if let Some(v) = line.strip_prefix("name:") {
                    name = v.trim().trim_matches('"').trim_matches('\'').to_string();
                } else if let Some(v) = line.strip_prefix("description:") {
                    description = v.trim().trim_matches('"').trim_matches('\'').to_string();
                }
            }
        }
        let name = if name.is_empty() {
            fallback_name.trim().to_string()
        } else {
            name
        };
        // A skill must have a name and something to say.
        if name.is_empty() {
            return None;
        }
        // names are slug-like: lowercase letters, digits, `-` and `_`
        if !name
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
        {
            return None;
        }
        let body: String = body.trim().chars().take(MAX_BODY_CHARS).collect();
        if body.is_empty() {
            return None;
        }
        let description = if description.is_empty() {
            "(no description)".to_string()
        } else {
            description
        };
        Some(Skill {
            name,
            description,
            body,
            source,
        })
    }

    /// The prompt block injected when the skill is activated (slash-command
    /// path). The tool path wraps the body in the tool-result framing instead.
    pub fn activation_prompt(&self) -> String {
        format!(
            "[Skill activated: {}]\n\nFollow the instructions below for the current task.\n\n{}",
            self.name, self.body
        )
    }
}

/// The ordered skill catalog. Order: builtin first, then user, then project
/// (project entries REPLACE same-name earlier ones, so a repo can override a
/// shipped skill without touching the installation).
#[derive(Clone, Debug, Default)]
pub struct SkillSet(pub Vec<Skill>);

impl SkillSet {
    /// The built-in library, embedded at compile time. Keep in sync with the
    /// directories under `crates/server/skills/`.
    pub fn builtin() -> SkillSet {
        const SOURCES: &[(&str, &str)] = &[
            (
                include_str!("../skills/romacode-customization/SKILL.md"),
                "romacode-customization",
            ),
            (
                include_str!("../skills/romacode-architecture-map/SKILL.md"),
                "romacode-architecture-map",
            ),
            (
                include_str!("../skills/romacode-security-posture/SKILL.md"),
                "romacode-security-posture",
            ),
            (
                include_str!("../skills/romacode-tool-recipes/SKILL.md"),
                "romacode-tool-recipes",
            ),
            (
                include_str!("../skills/claude-parity/SKILL.md"),
                "claude-parity",
            ),
            (
                include_str!("../skills/source-manifest/SKILL.md"),
                "source-manifest",
            ),
            (
                include_str!("../skills/docx-report/SKILL.md"),
                "docx-report",
            ),
            (
                include_str!("../skills/documents/SKILL.md"),
                "documents",
            ),
            (
                include_str!("../skills/docx-toolkit/SKILL.md"),
                "docx-toolkit",
            ),
            (
                include_str!("../skills/xlsx-toolkit/SKILL.md"),
                "xlsx-toolkit",
            ),
            (
                include_str!("../skills/pptx-toolkit/SKILL.md"),
                "pptx-toolkit",
            ),
            (
                include_str!("../skills/pdf-toolkit/SKILL.md"),
                "pdf-toolkit",
            ),
            (
                include_str!("../skills/charts-toolkit/SKILL.md"),
                "charts-toolkit",
            ),
            (
                include_str!("../skills/canvas-design/SKILL.md"),
                "canvas-design",
            ),
            (
                include_str!("../skills/figma-toolkit/SKILL.md"),
                "figma-toolkit",
            ),
            (
                include_str!("../skills/unlock/SKILL.md"),
                "unlock",
            ),
            (
                include_str!("../skills/android-setup/SKILL.md"),
                "android-setup",
            ),
            (
                include_str!("../skills/rc-card/SKILL.md"),
                "rc-card",
            ),
            (
                include_str!("../skills/rc-creds/SKILL.md"),
                "rc-creds",
            ),
            (
                include_str!("../skills/rc-exploit/SKILL.md"),
                "rc-exploit",
            ),
            (
                include_str!("../skills/rc-forge/SKILL.md"),
                "rc-forge",
            ),
            (
                include_str!("../skills/rc-kill/SKILL.md"),
                "rc-kill",
            ),
            (
                include_str!("../skills/rc-launder/SKILL.md"),
                "rc-launder",
            ),
            (
                include_str!("../skills/rc-loot/SKILL.md"),
                "rc-loot",
            ),
            (
                include_str!("../skills/rc-opsec/SKILL.md"),
                "rc-opsec",
            ),
            (
                include_str!("../skills/rc-pivot/SKILL.md"),
                "rc-pivot",
            ),
            (
                include_str!("../skills/rc-recon/SKILL.md"),
                "rc-recon",
            ),
            (
                include_str!("../skills/rc-tx/SKILL.md"),
                "rc-tx",
            ),
            (
                include_str!("../skills/remix-ethereum/SKILL.md"),
                "remix-ethereum",
            ),
            (
                include_str!("../skills/remix-contract-dev/SKILL.md"),
                "remix-contract-dev",
            ),
            (
                include_str!("../skills/remix-chain-forensics/SKILL.md"),
                "remix-chain-forensics",
            ),
        ];
        SkillSet(
            SOURCES
                .iter()
                .filter_map(|(text, name)| Skill::parse(text, name, "builtin"))
                .collect(),
        )
    }

    /// Full discovery: builtins + user dir + project dir. Never fails — a
    /// missing or unreadable directory simply contributes nothing.
    pub fn load() -> SkillSet {
        let mut set = Self::builtin();
        if let Some(cfg) = crate::config::Config::path() {
            if let Some(dir) = cfg.parent().map(|d| d.join("skills")) {
                set.merge_layer(read_skill_dir(&dir, "user"));
            }
        }
        if let Ok(cwd) = std::env::current_dir() {
            let dir = cwd.join(".romacode").join("skills");
            set.merge_layer(read_skill_dir(&dir, "project"));
        }
        set
    }

    /// Replace same-name entries (case-insensitive) and append the rest,
    /// keeping catalog order stable: builtin, user, project.
    pub fn merge_layer(&mut self, incoming: Vec<Skill>) {
        for skill in incoming {
            let lower = skill.name.to_ascii_lowercase();
            if let Some(slot) = self
                .0
                .iter_mut()
                .find(|s| s.name.to_ascii_lowercase() == lower)
            {
                *slot = skill;
            } else {
                self.0.push(skill);
            }
        }
    }

    /// Case-insensitive lookup by name.
    pub fn find(&self, name: &str) -> Option<&Skill> {
        let lower = name.trim().to_ascii_lowercase();
        self.0.iter().find(|s| s.name.to_ascii_lowercase() == lower)
    }

    /// Compact catalog for the system prompt: one line per skill.
    pub fn catalog(&self) -> String {
        let mut out = String::new();
        for s in &self.0 {
            out.push_str(&format!("- {} — {}\n", s.name, s.description));
        }
        out
    }
}

/// Read every `*/SKILL.md` one level below `dir`. Unreadable entries are
/// skipped silently — a broken user skill directory must never take the
/// server down.
fn read_skill_dir(dir: &PathBuf, source: &'static str) -> Vec<Skill> {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return Vec::new();
    };
    let mut paths: Vec<PathBuf> = entries
        .flatten()
        .map(|e| e.path())
        .filter(|p| p.is_dir())
        .collect();
    paths.sort();
    let mut out = Vec::new();
    for dir in paths.into_iter().take(MAX_SKILLS_PER_DIR) {
        let fallback = dir
            .file_name()
            .map(|n| n.to_string_lossy().to_string())
            .unwrap_or_default();
        let path = dir.join("SKILL.md");
        if let Ok(text) = std::fs::read_to_string(&path) {
            if let Some(skill) = Skill::parse(&text, &fallback, source) {
                out.push(skill);
            }
        }
    }
    out
}

// ---------------------------------------------------------------------------
// The agent-side `skill` tool (agent-initiated activation).
// ---------------------------------------------------------------------------

/// Tool schema for the `skill` tool. The description embeds the live catalog
/// so the model sees exactly which skills exist each turn.
pub fn definition(catalog: &str) -> Value {
    let desc = format!(
        "Activate a skill: load a package of expert instructions for the current task and \
         follow it. Available skills:\n{}\
         Call this instead of guessing a workflow the skill already encodes. One call \
         loads the full instructions as the tool result.",
        catalog
    );
    json!({
        "name": "skill",
        "description": desc,
        "input_schema": {
            "type": "object",
            "properties": {
                "name": { "type": "string", "description": "Skill name from the list, e.g. 'romacode-customization'" }
            },
            "required": ["name"]
        },
    })
}

/// Execute the `skill` tool: resolve the name and return the instruction body
/// as the tool result. Read-only and side-effect free.
pub fn execute(input: &Value) -> Result<String, String> {
    let name = input["name"].as_str().unwrap_or("").trim();
    if name.is_empty() {
        return Err("missing skill name".into());
    }
    let set = SkillSet::load();
    match set.find(name) {
        Some(skill) => Ok(format!(
            "Skill '{}' activated ({}, {} chars of instructions). Follow these instructions \
             for the rest of the task:\n\n{}",
            skill.name,
            skill.source,
            skill.body.chars().count(),
            skill.body
        )),
        None => {
            let known = set
                .0
                .iter()
                .map(|s| s.name.as_str())
                .collect::<Vec<_>>()
                .join(", ");
            Err(format!(
                "no skill named {name:?}; available: {known}. Run /skills for details."
            ))
        }
    }
}

/// The system-prompt block that advertises the catalog. Returns an empty
/// string when no skills are installed (keeps `system_prompt` clean).
pub fn prompt_section() -> String {
    let set = SkillSet::load();
    if set.0.is_empty() {
        return String::new();
    }
    format!(
        "\n\n## Skills\nExpert instruction packages are available. You can activate one \
         yourself with the `skill` tool (name argument), and the user can activate one with \
         the /skill <name> slash command. When a task matches a skill, activate it before \
         improvising. Catalog:\n{}",
        set.catalog()
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_reads_frontmatter_and_body() {
        let s = Skill::parse(
            "---\nname: my-skill\ndescription: Does a thing\n---\n\nBody line one.\nBody two.\n",
            "unused",
            "user",
        )
        .unwrap();
        assert_eq!(s.name, "my-skill");
        assert_eq!(s.description, "Does a thing");
        assert_eq!(s.body, "Body line one.\nBody two.");
        assert_eq!(s.source, "user");
    }

    #[test]
    fn parse_falls_back_to_directory_name_and_default_description() {
        let s = Skill::parse("# just a body\nstep one", "dir-name", "project").unwrap();
        assert_eq!(s.name, "dir-name");
        assert_eq!(s.description, "(no description)");
        assert_eq!(s.body, "# just a body\nstep one");
    }

    #[test]
    fn parse_rejects_nameless_and_invalid_slugs() {
        // no name anywhere
        assert!(Skill::parse("body only", "   ", "user").is_none());
        // empty body
        assert!(Skill::parse("---\nname: x\ndescription: d\n---\n   ", "x", "user").is_none());
        // slug violations
        assert!(Skill::parse("---\nname: bad name!\ndescription: d\n---\nbody", "x", "user").is_none());
    }

    #[test]
    fn parse_reads_quoted_frontmatter_values() {
        let s = Skill::parse(
            "---\nname: \"quoted\"\ndescription: 'single'\n---\nBody here.\n",
            "unused",
            "builtin",
        )
        .unwrap();
        assert_eq!(s.name, "quoted");
        assert_eq!(s.description, "single");
    }

    #[test]
    fn parse_tolerates_unterminated_frontmatter() {
        // missing closing marker must not lose the body: the stray `---` is
        // treated as noise and the whole text becomes the body
        let s = Skill::parse(
            "---\nname: forgotten\nstep one\nstep two",
            "salvaged",
            "user",
        )
        .unwrap();
        assert_eq!(s.name, "salvaged");
        assert!(s.body.contains("step one"), "{}", s.body);
    }

    #[test]
    fn builtin_library_loads_with_every_skill_parsable() {
        let set = SkillSet::builtin();
        assert!(set.0.len() >= 9, "expected at least 9 builtin skills");
        for s in &set.0 {
            assert!(!s.body.is_empty());
            assert_eq!(s.source, "builtin");
            assert_ne!(s.description, "(no description)");
        }
        let names: Vec<&str> = set.0.iter().map(|s| s.name.as_str()).collect();
        for required in [
            "romacode-customization",
            "romacode-architecture-map",
            "romacode-security-posture",
            "romacode-tool-recipes",
            "claude-parity",
            "source-manifest",
            "docx-report",
            "unlock",
            "android-setup",
            "remix-ethereum",
            "remix-contract-dev",
            "remix-chain-forensics",
        ] {
            assert!(names.contains(&required), "builtin skill {required} missing");
        }
        // the remix skills carry the full rrx surface and the safety gates
        let remix = set.find("remix-ethereum").unwrap();
        assert!(remix.body.contains("remix_status"), "remix skill must teach the doctor");
        assert!(remix.body.contains("allow_chain_writes"), "remix skill must teach the write gate");
        assert!(remix.body.contains("debugtx"), "remix skill must cover the flagship replay");
        let dev = set.find("remix-contract-dev").unwrap();
        assert!(dev.body.contains("workspace init"));
        assert!(dev.body.contains("bytediff"));
        let forensics = set.find("remix-chain-forensics").unwrap();
        assert!(forensics.body.contains("proof"));
        assert!(forensics.body.contains("fakeasset"));
        // the shipped customization skill is the full recipe set
        let custom = set.find("romacode-customization").unwrap();
        assert!(custom.body.contains("Recipe A"), "missing recipes");
        assert!(custom.body.len() > 6_000, "shipped skill was truncated?");
        // the unlock skill is model-agnostic: any downloadable local model,
        // not only the tested Qwen3 recipe
        let unlock = set.find("unlock").unwrap();
        assert!(unlock.body.contains("--model-id"), "unlock must cover other models");
        assert!(unlock.body.contains("Heretic"), "unlock must credit the engine");
        // the android-setup skill encodes the bootstrap ladder: no-device
        // questions are answered by android_setup, never by giving up
        let setup = set.find("android-setup").unwrap();
        assert!(setup.body.contains("create_avd"));
        assert!(setup.body.contains("android_setup"));
    }

    #[test]
    fn project_overrides_replace_builtin_same_name() {
        let mut set = SkillSet::builtin();
        let before = set.0.len();
        set.merge_layer(vec![Skill {
            name: "docx-report".into(),
            description: "project-local override".into(),
            body: "override body".into(),
            source: "project",
        }]);
        assert_eq!(set.0.len(), before, "override must replace, not append");
        let s = set.find("DOCX-REPORT").unwrap();
        assert_eq!(s.body, "override body");
        assert_eq!(s.source, "project");
        set.merge_layer(vec![Skill {
            name: "brand-new".into(),
            description: "d".into(),
            body: "b".into(),
            source: "user",
        }]);
        assert_eq!(set.0.len(), before + 1);
        assert!(set.find("brand-new").is_some());
    }

    #[test]
    fn find_is_case_insensitive_and_catalog_lists_everything() {
        let set = SkillSet::builtin();
        assert!(set.find("  ROMACODE-CUSTOMIZATION ").is_some());
        let cat = set.catalog();
        for s in &set.0 {
            assert!(cat.contains(&format!("- {} — {}", s.name, s.description)));
        }
    }

    #[test]
    fn skill_tool_executes_and_reports_unknown_names() {
        let out = execute(&json!({"name": "docx-report"})).unwrap();
        assert!(out.contains("docx-report"));
        assert!(out.contains("activated"));
        let err = execute(&json!({"name": "does-not-exist"})).unwrap_err();
        assert!(err.contains("no skill named"), "{err}");
        assert!(err.contains("docx-report"), "error should list known skills: {err}");
        assert!(execute(&json!({})).is_err());
    }

    #[test]
    fn prompt_section_advertises_the_catalog() {
        let section = prompt_section();
        assert!(section.contains("## Skills"));
        assert!(section.contains("romacode-customization"));
        assert!(section.contains("skill"));
    }

    #[test]
    fn body_is_capped_at_the_limit() {
        let big = format!("---\nname: big\n---\n{}", "x".repeat(MAX_BODY_CHARS + 500));
        let s = Skill::parse(&big, "big", "user").unwrap();
        assert_eq!(s.body.chars().count(), MAX_BODY_CHARS);
    }
}
