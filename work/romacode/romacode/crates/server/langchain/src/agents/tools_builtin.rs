//! Built-in tools shipped with the crate: calculator, file I/O, directory
//! listing and an opt-in shell tool for CLI agents.

use async_trait::async_trait;
use serde_json::{json, Value};
use std::path::Path;
use std::sync::Arc;
use std::time::Duration;

use crate::agents::tools::{tool_fn, Tool, ToolFn};
use crate::errors::{LangchainError, Result};

/// Recursive-descent calculator: `+ - * / ^ %`, parentheses, unary minus,
/// and `sqrt`, `abs`, `min`, `max`, `floor`, `ceil`, `log`, `exp` functions.
///
/// Parsing is hand-rolled (no dependencies) and evaluation is f64-safe:
/// division by zero and NaN results are reported as errors.
pub fn calculate(expression: &str) -> Result<f64> {
    let tokens = lex(expression)?;
    let mut parser = Parser { tokens, pos: 0 };
    let value = parser.parse_expr()?;
    if parser.pos != parser.tokens.len() {
        return Err(LangchainError::InvalidInput(format!(
            "unexpected token at position {}: `{:?}`",
            parser.pos, parser.tokens[parser.pos]
        )));
    }
    if value.is_nan() || value.is_infinite() {
        return Err(LangchainError::InvalidInput("expression evaluates to NaN/∞".into()));
    }
    Ok(value)
}

#[derive(Debug, Clone, PartialEq)]
enum Tok {
    Num(f64),
    Ident(String),
    Op(char),
    LParen,
    RParen,
    Comma,
}

fn lex(s: &str) -> Result<Vec<Tok>> {
    let mut out = Vec::new();
    let mut chars = s.chars().peekable();
    while let Some(&c) = chars.peek() {
        match c {
            ' ' | '\t' | '\n' | '\r' => {
                chars.next();
            }
            '0'..='9' | '.' => {
                let mut num = String::new();
                while let Some(&c2) = chars.peek() {
                    if c2.is_ascii_digit() || c2 == '.' || c2 == 'e' || c2 == 'E' || ((c2 == '+' || c2 == '-') && num.ends_with(['e', 'E'])) {
                        num.push(c2);
                        chars.next();
                    } else {
                        break;
                    }
                }
                let v: f64 = num
                    .parse()
                    .map_err(|_| LangchainError::InvalidInput(format!("bad number `{num}`")))?;
                out.push(Tok::Num(v));
            }
            'a'..='z' | 'A'..='Z' | '_' => {
                let mut id = String::new();
                while let Some(&c2) = chars.peek() {
                    if c2.is_ascii_alphanumeric() || c2 == '_' {
                        id.push(c2);
                        chars.next();
                    } else {
                        break;
                    }
                }
                out.push(Tok::Ident(id));
            }
            '+' | '-' | '*' | '/' | '^' | '%' => {
                out.push(Tok::Op(c));
                chars.next();
            }
            '(' => {
                out.push(Tok::LParen);
                chars.next();
            }
            ')' => {
                out.push(Tok::RParen);
                chars.next();
            }
            ',' => {
                out.push(Tok::Comma);
                chars.next();
            }
            other => {
                return Err(LangchainError::InvalidInput(format!("unexpected char `{other}`")));
            }
        }
    }
    Ok(out)
}

struct Parser {
    tokens: Vec<Tok>,
    pos: usize,
}

impl Parser {
    fn peek(&self) -> Option<&Tok> {
        self.tokens.get(self.pos)
    }

    fn next(&mut self) -> Option<Tok> {
        let t = self.tokens.get(self.pos).cloned();
        if t.is_some() {
            self.pos += 1;
        }
        t
    }

    // expr := term (('+'|'-') term)*
    fn parse_expr(&mut self) -> Result<f64> {
        let mut left = self.parse_term()?;
        while let Some(Tok::Op(op @ ('+' | '-'))) = self.peek() {
            let op = *op;
            self.pos += 1;
            let right = self.parse_term()?;
            left = if op == '+' { left + right } else { left - right };
        }
        Ok(left)
    }

    // term := factor (('*'|'/'|'%') factor)*
    fn parse_term(&mut self) -> Result<f64> {
        let mut left = self.parse_factor()?;
        while let Some(Tok::Op(op @ ('*' | '/' | '%'))) = self.peek() {
            let op = *op;
            self.pos += 1;
            let right = self.parse_factor()?;
            left = match op {
                '*' => left * right,
                '/' => {
                    if right == 0.0 {
                        return Err(LangchainError::InvalidInput("division by zero".into()));
                    }
                    left / right
                }
                '%' => {
                    if right == 0.0 {
                        return Err(LangchainError::InvalidInput("modulo by zero".into()));
                    }
                    left % right
                }
                _ => unreachable!(),
            };
        }
        Ok(left)
    }

    // factor := unary ('^' factor)?   (right associative)
    fn parse_factor(&mut self) -> Result<f64> {
        let base = self.parse_unary()?;
        if let Some(Tok::Op('^')) = self.peek() {
            self.pos += 1;
            let exp = self.parse_factor()?;
            return Ok(base.powf(exp));
        }
        Ok(base)
    }

    // unary := '-' unary | primary
    fn parse_unary(&mut self) -> Result<f64> {
        if let Some(Tok::Op('-')) = self.peek() {
            self.pos += 1;
            return Ok(-self.parse_unary()?);
        }
        if let Some(Tok::Op('+')) = self.peek() {
            self.pos += 1;
            return self.parse_unary();
        }
        self.parse_primary()
    }

    // primary := number | ident '(' args ')' | '(' expr ')'
    fn parse_primary(&mut self) -> Result<f64> {
        match self.next() {
            Some(Tok::Num(v)) => Ok(v),
            Some(Tok::Ident(name)) => {
                if self.next() != Some(Tok::LParen) {
                    return Err(LangchainError::InvalidInput(format!("expected `(` after `{name}`")));
                }
                let mut args = vec![self.parse_expr()?];
                while self.peek() == Some(&Tok::Comma) {
                    self.pos += 1;
                    args.push(self.parse_expr()?);
                }
                if self.next() != Some(Tok::RParen) {
                    return Err(LangchainError::InvalidInput(format!("expected `)` after `{name}(...`")));
                }
                apply_fn(&name, &args)
            }
            Some(Tok::LParen) => {
                let v = self.parse_expr()?;
                if self.next() != Some(Tok::RParen) {
                    return Err(LangchainError::InvalidInput("expected `)`".into()));
                }
                Ok(v)
            }
            other => Err(LangchainError::InvalidInput(format!("unexpected token {other:?}"))),
        }
    }
}

fn apply_fn(name: &str, args: &[f64]) -> Result<f64> {
    let take1 = |a: &[f64]| -> Result<f64> {
        if a.len() != 1 {
            return Err(LangchainError::InvalidInput(format!("{name} takes 1 argument")));
        }
        Ok(a[0])
    };
    match name {
        "sqrt" => {
            let v = take1(args)?;
            if v < 0.0 {
                return Err(LangchainError::InvalidInput("sqrt of negative number".into()));
            }
            Ok(v.sqrt())
        }
        "abs" => Ok(take1(args)?.abs()),
        "floor" => Ok(take1(args)?.floor()),
        "ceil" => Ok(take1(args)?.ceil()),
        "log" => Ok(take1(args)?.ln()),
        "exp" => Ok(take1(args)?.exp()),
        "min" => {
            if args.is_empty() {
                return Err(LangchainError::InvalidInput("min needs at least 1 argument".into()));
            }
            Ok(args.iter().cloned().fold(f64::INFINITY, f64::min))
        }
        "max" => {
            if args.is_empty() {
                return Err(LangchainError::InvalidInput("max needs at least 1 argument".into()));
            }
            Ok(args.iter().cloned().fold(f64::NEG_INFINITY, f64::max))
        }
        other => Err(LangchainError::InvalidInput(format!("unknown function `{other}`"))),
    }
}

/// `calculator` tool — evaluates a math expression.
pub fn calculator_tool() -> Tool {
    tool_fn(
        "calculator",
        "Evaluate a math expression. Supports + - * / % ^ parentheses and sqrt, abs, min, max, floor, ceil, log, exp.",
        json!({
            "type": "object",
            "properties": {
                "expression": {"type": "string", "description": "e.g. (2+3)*4"}
            },
            "required": ["expression"]
        }),
        |args| async move {
            let expr = args["expression"]
                .as_str()
                .ok_or_else(|| LangchainError::ToolExecution("calculator".into(), "`expression` must be a string".into()))?;
            let v = calculate(expr)?;
            // Render integers without a trailing .0
            if v.fract() == 0.0 && v.abs() < 1e15 {
                Ok(format!("{}", v as i64))
            } else {
                Ok(format!("{v}"))
            }
        },
    )
}

/// `read_file` tool — reads a UTF-8 text file from disk.
pub fn read_file_tool() -> Tool {
    tool_fn(
        "read_file",
        "Read a text file at the given path and return its contents.",
        json!({
            "type": "object",
            "properties": {"path": {"type": "string"}},
            "required": ["path"]
        }),
        |args| async move {
            let path = arg_str(&args, "path")?;
            match tokio::fs::read_to_string(&path).await {
                Ok(content) => {
                    let lines = content.lines().count();
                    Ok(format!("{} ({} lines)", content, lines))
                }
                Err(e) => Err(LangchainError::ToolExecution("read_file".into(), format!("{path}: {e}"))),
            }
        },
    )
}

/// `write_file` tool — writes text to a file (creating parents).
pub fn write_file_tool() -> Tool {
    tool_fn(
        "write_file",
        "Write text content to a file at the given path. Overwrites existing files.",
        json!({
            "type": "object",
            "properties": {
                "path": {"type": "string"},
                "content": {"type": "string"}
            },
            "required": ["path", "content"]
        }),
        |args| async move {
            let path = arg_str(&args, "path")?;
            let content = arg_str(&args, "content")?;
            if let Some(parent) = Path::new(&path).parent() {
                let _ = tokio::fs::create_dir_all(parent).await;
            }
            tokio::fs::write(&path, &content)
                .await
                .map_err(|e| LangchainError::ToolExecution("write_file".into(), format!("{path}: {e}")))?;
            Ok(format!("wrote {} bytes to {path}", content.len()))
        },
    )
}

/// `list_dir` tool — lists a directory's entries with sizes.
pub fn list_dir_tool() -> Tool {
    tool_fn(
        "list_dir",
        "List the files and directories at the given path.",
        json!({
            "type": "object",
            "properties": {"path": {"type": "string", "description": "defaults to `.` if omitted"}},
            "required": []
        }),
        |args| async move {
            let path = args["path"].as_str().unwrap_or(".").to_string();
            let mut entries: Vec<String> = Vec::new();
            let mut rd = tokio::fs::read_dir(&path)
                .await
                .map_err(|e| LangchainError::ToolExecution("list_dir".into(), format!("{path}: {e}")))?;
            while let Some(entry) = rd
                .next_entry()
                .await
                .map_err(|e| LangchainError::ToolExecution("list_dir".into(), e.to_string()))?
            {
                let name = entry.file_name().to_string_lossy().to_string();
                if let Ok(md) = entry.metadata().await {
                    if md.is_dir() {
                        entries.push(format!("{name}/ (dir)"));
                    } else {
                        entries.push(format!("{name} ({} bytes)", md.len()));
                    }
                } else {
                    entries.push(name);
                }
            }
            entries.sort();
            Ok(entries.join("\n"))
        },
    )
}

/// `run_shell` tool — executes a command with a timeout. Opt-in for CLI agents
/// because it can run arbitrary commands on the host.
pub struct ShellTool {
    pub timeout: Duration,
    pub allowed: bool,
}

#[async_trait]
impl ToolFn for ShellTool {
    async fn call(&self, args: Value) -> Result<String> {
        if !self.allowed {
            return Err(LangchainError::ToolExecution(
                "run_shell".into(),
                "shell tool disabled — start the CLI with --allow-shell".into(),
            ));
        }
        let cmd = args["command"]
            .as_str()
            .ok_or_else(|| LangchainError::ToolExecution("run_shell".into(), "`command` must be a string".into()))?
            .to_string();
        let output = tokio::process::Command::new("sh")
            .arg("-c")
            .arg(&cmd)
            // The `timeout` below cancels the FUTURE, not the process: make
            // sure dropping the child handle kills the shell instead of
            // leaking a running process on the host.
            .kill_on_drop(true)
            .output();
        let out = tokio::time::timeout(self.timeout, output)
            .await
            .map_err(|_| LangchainError::ToolExecution("run_shell".into(), "command timed out".into()))?
            .map_err(|e| LangchainError::ToolExecution("run_shell".into(), e.to_string()))?;
        let stdout = String::from_utf8_lossy(&out.stdout);
        let stderr = String::from_utf8_lossy(&out.stderr);
        Ok(format!(
            "exit={}\nstdout:\n{}\nstderr:\n{}",
            out.status.code().unwrap_or(-1),
            stdout,
            stderr
        ))
    }
}

/// Build the opt-in shell tool.
pub fn shell_tool(allowed: bool, timeout: Duration) -> Tool {
    let shell = Arc::new(ShellTool { timeout, allowed });
    Tool::new(
        crate::messages::ToolSpec {
            name: "run_shell".into(),
            description: "Run a shell command (`sh -c`) and return exit code, stdout and stderr. Use for searches, builds, git and any terminal task."
                .into(),
            parameters: json!({
                "type": "object",
                "properties": {"command": {"type": "string"}},
                "required": ["command"]
            }),
        },
        shell,
    )
}

fn arg_str(args: &Value, key: &str) -> Result<String> {
    args[key]
        .as_str()
        .map(|s| s.to_string())
        .ok_or_else(|| LangchainError::InvalidInput(format!("`{key}` must be a string")))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn arithmetic() {
        assert!((calculate("2+3*4").unwrap() - 14.0).abs() < 1e-9);
        assert!((calculate("(2+3)*4").unwrap() - 20.0).abs() < 1e-9);
        assert!((calculate("2^3^2").unwrap() - 512.0).abs() < 1e-9); // right assoc
        assert!(calculate("-5+3").unwrap() < 0.0);
        assert!((calculate("10 % 3").unwrap() - 1.0).abs() < 1e-9);
        assert!((calculate("sqrt(16)").unwrap() - 4.0).abs() < 1e-9);
        assert!((calculate("max(3, 7, 5)").unwrap() - 7.0).abs() < 1e-9);
        assert!((calculate("1e2 + 5").unwrap() - 105.0).abs() < 1e-9);
    }

    #[test]
    fn arithmetic_errors() {
        assert!(calculate("2/0").is_err());
        assert!(calculate("2 +").is_err());
        assert!(calculate("(1+2").is_err());
        assert!(calculate("sqrt(-1)").is_err());
        assert!(calculate("foo(1)").is_err());
        assert!(calculate("1 $ 2").is_err());
    }

    #[tokio::test]
    async fn calculator_tool_renders_ints_cleanly() {
        let t = calculator_tool();
        let out = t.call(json!({"expression": "2+2"})).await.unwrap();
        assert_eq!(out, "4");
        let out = t.call(json!({"expression": "1/2"})).await.unwrap();
        assert_eq!(out, "0.5");
    }

    #[tokio::test]
    async fn shell_tool_disabled_by_default() {
        let t = shell_tool(false, Duration::from_secs(5));
        let e = t.call(json!({"command": "echo hi"})).await.unwrap_err();
        assert!(e.to_string().contains("disabled"));
    }
}
