//! Source 3 of 8 — **LLM-generated**. Describe a tool in natural language; the
//! source asks an LLM (via a configurable command, default `z-ai`) to emit a
//! declarative tool spec as JSON, parses it, and builds a [`ShellTool`]. This
//! is the "literally any tool anyone can describe" mechanism.
//!
//! The contract with the LLM command: the rendered prompt is written to the
//! command's stdin; the command's stdout must contain a JSON object shaped
//! like a declarative tool:
//!
//! ```json
//! {"name":"file.line_count","description":"Count lines in a file",
//!  "command":"wc -l < {{path}}","params":[{"name":"path","type":"string","required":true}]}
//! ```
//!
//! The first `{ … }` block in the output is extracted, so leading/trailing
//! prose is tolerated. Any LLM CLI that reads a prompt from stdin and writes
//! text to stdout works (`z-ai`, `llm`, `og`, a curl-to-an-API wrapper, …).

use crate::declarative::DeclarativeTool;
use crate::source::{SourceError, ToolSource};
use crate::tools::ShellTool;
use async_trait::async_trait;
use rc_core::schema::Schema;
use rc_core::tool::{arc, ToolArc, ToolSourceKind};
use tokio::io::AsyncWriteExt;

const DEFAULT_PROMPT: &str = "\
You are a tool generator. Output ONE JSON object (no prose, no markdown fences) describing a single shell-backed tool.
Schema:
{\"name\":\"<dotted.name>\",\"description\":\"<one line>\",\"command\":\"<shell cmd with {{param}} placeholders>\",\"params\":[{\"name\":\"<p>\",\"type\":\"string|integer|number|boolean\",\"required\":true|false,\"description\":\"<one line>\"}]}
Rules:
- name is dot-namespaced (e.g. file.line_count).
- command uses {{param}} placeholders for every param.
- prefer simple, widely-available commands.
Description of the tool to generate:\n";

pub struct LlmGenSource {
    /// The command to run, e.g. ["z-ai", "function", "-n", "chat"].
    command: Vec<String>,
    /// Prompt template; `{{description}}` is substituted with each tool's
    /// description.
    prompt_template: String,
    /// The tool descriptions to generate.
    descriptions: Vec<String>,
    origin: String,
}

impl LlmGenSource {
    /// Default generator using `z-ai`.
    pub fn new() -> Self {
        Self {
            command: vec!["z-ai".into(), "function".into(), "-n".into(), "chat".into()],
            prompt_template: DEFAULT_PROMPT.to_string(),
            descriptions: Vec::new(),
            origin: "llm_gen".into(),
        }
    }

    /// Use a custom command (e.g. ["llm", "-m", "gpt-4o"]).
    pub fn with_command(mut self, command: Vec<String>) -> Self {
        self.command = command;
        self
    }

    pub fn with_prompt_template(mut self, tmpl: impl Into<String>) -> Self {
        self.prompt_template = tmpl.into();
        self
    }

    /// Queue a tool to generate from a natural-language description.
    pub fn add(&mut self, description: impl Into<String>) -> &mut Self {
        self.descriptions.push(description.into());
        self
    }

    /// Generate one tool from a description by calling the LLM command.
    pub async fn generate(&self, description: &str) -> Result<ToolArc, SourceError> {
        let prompt = self
            .prompt_template
            .replace("{{description}}", description);
        let rendered = if !prompt.contains("{{description}}") && !description.is_empty() {
            format!("{prompt}{description}")
        } else {
            prompt
        };

        // Spawn the command, write the prompt to stdin, capture stdout.
        let mut cmd = if self.command.is_empty() {
            return Err(SourceError::other(
                "llm_gen",
                "no command configured",
            ));
        } else {
            let mut c = tokio::process::Command::new(&self.command[0]);
            c.args(&self.command[1..]);
            // Kill the generator if this future is cancelled/timed out.
            c.kill_on_drop(true);
            c.stdin(std::process::Stdio::piped())
                .stdout(std::process::Stdio::piped())
                .stderr(std::process::Stdio::piped());
            c
        };
        let mut child = cmd.spawn().map_err(|e| SourceError::Io {
            src: "llm_gen".into(),
            inner: e,
        })?;
        // Write the prompt from a separate task: writing stdin to completion
        // BEFORE reading stdout deadlocks against a generator that fills its
        // stdout pipe first. The spawned task lets `wait_with_output` read
        // concurrently with the write.
        let stdin_task = match child.stdin.take() {
            Some(mut stdin) => {
                let prompt = rendered.clone();
                Some(tokio::spawn(async move {
                    stdin.write_all(prompt.as_bytes()).await
                }))
            }
            None => None,
        };
        // A wedged generator must not hang this future forever: cap the wait
        // at 120 s. On expiry the future (owning the child) is dropped and
        // `kill_on_drop` terminates the generator.
        let wait = async {
            let out = child.wait_with_output().await;
            if let Some(task) = stdin_task {
                let _ = task.await;
            }
            out
        };
        let output = match tokio::time::timeout(std::time::Duration::from_secs(120), wait).await
        {
            Ok(out) => out.map_err(|e| SourceError::Io {
                src: "llm_gen".into(),
                inner: e,
            })?,
            Err(_) => {
                return Err(SourceError::other(
                    "llm_gen",
                    "generator timed out after 120s",
                ));
            }
        };
        if !output.status.success() {
            return Err(SourceError::other(
                "llm_gen",
                format!(
                    "command {:?} exited with {:?}; stderr: {}",
                    self.command,
                    output.status.code(),
                    String::from_utf8_lossy(&output.stderr)
                ),
            ));
        }
        let stdout = String::from_utf8_lossy(&output.stdout);
        let spec_json = extract_first_json_object(&stdout).ok_or_else(|| {
            SourceError::other(
                "llm_gen",
                format!("no JSON object found in command output: {}", stdout.chars().take(200).collect::<String>()),
            )
        })?;
        let dt: DeclarativeTool = serde_json::from_str(&spec_json).map_err(|e| {
            SourceError::Json {
                src: "llm_gen".into(),
                inner: e,
            }
        })?;
        Ok(tool_from_declarative(dt, ToolSourceKind::LlmGenerated))
    }
}

impl Default for LlmGenSource {
    fn default() -> Self {
        Self::new()
    }
}

/// Build a `ShellTool` from a parsed declarative tool, tagged with a source kind.
pub(crate) fn tool_from_declarative(dt: DeclarativeTool, kind: ToolSourceKind) -> ToolArc {
    let mut schema = Schema::new();
    for p in &dt.params {
        schema = schema.param(rc_core::schema::Parameter {
            name: p.name.clone(),
            description: p.description.clone(),
            typ: crate::declarative::parse_param_type(&p.typ),
            required: p.required,
            default: p.default.clone(),
            enum_values: Vec::new(),
        });
    }
    arc(ShellTool::new(
        dt.name,
        dt.description,
        schema,
        dt.command,
        kind,
    ))
}

/// Extract the first balanced `{ … }` object from a string (tolerates leading
/// prose and code fences).
fn extract_first_json_object(s: &str) -> Option<String> {
    let start = s.find('{')?;
    let bytes = s.as_bytes();
    let mut depth = 0i32;
    let mut in_str = false;
    let mut esc = false;
    for (i, &b) in bytes.iter().enumerate().skip(start) {
        if in_str {
            if esc {
                esc = false;
            } else if b == b'\\' {
                esc = true;
            } else if b == b'"' {
                in_str = false;
            }
        } else if b == b'"' {
            in_str = true;
        } else if b == b'{' {
            depth += 1;
        } else if b == b'}' {
            depth -= 1;
            if depth == 0 {
                return Some(s[start..=i].to_string());
            }
        }
    }
    None
}

#[async_trait]
impl ToolSource for LlmGenSource {
    fn kind(&self) -> ToolSourceKind {
        ToolSourceKind::LlmGenerated
    }
    fn id(&self) -> &str {
        &self.origin
    }
    async fn tools(&self) -> Result<Vec<ToolArc>, SourceError> {
        let mut out = Vec::new();
        for d in &self.descriptions {
            out.push(self.generate(d).await?);
        }
        Ok(out)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn extract_json_object_handles_prose() {
        let s = "Sure! Here is the tool:\n{\"name\":\"x.y\",\"command\":\"echo\"}\nLet me know.";
        let j = extract_first_json_object(s).unwrap();
        assert!(j.starts_with('{'));
        assert!(j.ends_with('}'));
        let v: serde_json::Value = serde_json::from_str(&j).unwrap();
        assert_eq!(v["name"], "x.y");
    }

    #[test]
    fn extract_json_object_handles_nested() {
        let s = r#"{"a":{"b":1},"c":2}"#;
        let j = extract_first_json_object(s).unwrap();
        assert_eq!(j, s);
    }

    #[tokio::test]
    async fn generate_with_cat_as_mock_llm() {
        // `cat` echoes stdin back; feed it a JSON spec as the "prompt".
        let spec = r#"{"name":"file.wc","description":"count words","command":"wc -w < {{path}}","params":[{"name":"path","type":"string","required":true}]}"#;
        let mut src = LlmGenSource::new().with_command(vec!["cat".into()]);
        // Override the prompt template so the spec passes through verbatim.
        src.prompt_template = spec.to_string();
        let tool = src.generate("ignored — cat echoes the template").await.unwrap();
        assert_eq!(tool.name(), "file.wc");
        assert_eq!(tool.source(), ToolSourceKind::LlmGenerated);
        assert_eq!(tool.schema().parameters.len(), 1);
    }
}
