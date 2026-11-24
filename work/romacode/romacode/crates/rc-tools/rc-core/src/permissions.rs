//! Permissions — the gate the executor consults before running any tool.
//!
//! This mirrors the RomaCode server's model, and like the server gate it is
//! **fully open by default**: the default policy carries no rules and every
//! tool call is `Allow` — nothing is denied, nothing asks. The rule machinery
//! (deny → ask → allow → default evaluation order) is kept so an embedder who
//! *wants* restrictions can still opt in by building a non-default
//! [`PermissionPolicy`]; the executor itself never prompts a human — `Ask` is
//! surfaced as a decision the caller must handle before calling `run` with
//! `approved = true`.

use crate::tool::Tool;
use serde::{Deserialize, Serialize};

/// The three possible decisions for a tool call.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum Permission {
    Allow,
    Ask,
    Deny,
}

/// A single rule: a glob pattern matched against the tool name, bound to a
/// decision. `math.*` matches `math.add`; `*` matches everything.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PermissionRule {
    pub pattern: String,
    pub decision: Permission,
}

impl PermissionRule {
    pub fn new(pattern: impl Into<String>, decision: Permission) -> Self {
        Self {
            pattern: pattern.into(),
            decision,
        }
    }
    /// Glob match: `*` matches any run of chars; `?` matches one char.
    pub fn matches(&self, name: &str) -> bool {
        glob_match(&self.pattern, name)
    }
}

fn glob_match(pat: &str, s: &str) -> bool {
    let p: Vec<char> = pat.chars().collect();
    let t: Vec<char> = s.chars().collect();
    glob_inner(&p, 0, &t, 0)
}

fn glob_inner(p: &[char], mut pi: usize, t: &[char], mut ti: usize) -> bool {
    while pi < p.len() {
        match p[pi] {
            '*' => {
                // try to match the rest of the pattern at every position
                if pi == p.len() - 1 {
                    return true; // trailing * eats everything
                }
                for skip in ti..=t.len() {
                    if glob_inner(p, pi + 1, t, skip) {
                        return true;
                    }
                }
                return false;
            }
            '?' => {
                if ti >= t.len() {
                    return false;
                }
                pi += 1;
                ti += 1;
            }
            c => {
                if ti >= t.len() || t[ti] != c {
                    return false;
                }
                pi += 1;
                ti += 1;
            }
        }
    }
    ti == t.len()
}

/// The full permission policy. Evaluation order:
/// 1. explicit `deny` rules (first match wins)
/// 2. explicit `ask` rules
/// 3. explicit `allow` rules
/// 4. the `default` (per-class, falling back to a global default)
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PermissionPolicy {
    #[serde(default)]
    pub deny: Vec<PermissionRule>,
    #[serde(default)]
    pub ask: Vec<PermissionRule>,
    #[serde(default)]
    pub allow: Vec<PermissionRule>,
    /// The global default when no rule matches.
    #[serde(default = "default_default")]
    pub default: Permission,
}

fn default_default() -> Permission {
    Permission::Allow
}

impl Default for PermissionPolicy {
    fn default() -> Self {
        // Fully open, matching the server-side gate: no rules, every tool
        // allowed. Restrictive policies are opt-in (see the builder helpers
        // below); they are never the shipped default.
        Self {
            deny: vec![],
            ask: vec![],
            allow: vec![],
            default: Permission::Allow,
        }
    }
}

impl PermissionPolicy {
    /// Decide the permission for a given tool.
    pub fn decide(&self, tool: &dyn Tool) -> Permission {
        let name = tool.name();
        // deny first
        for r in &self.deny {
            if r.matches(name) {
                return r.decision;
            }
        }
        for r in &self.ask {
            if r.matches(name) {
                return r.decision;
            }
        }
        for r in &self.allow {
            if r.matches(name) {
                return r.decision;
            }
        }
        self.default
    }

    /// Builder helpers.
    pub fn deny(mut self, pattern: impl Into<String>) -> Self {
        self.deny.push(PermissionRule::new(pattern, Permission::Deny));
        self
    }
    pub fn ask(mut self, pattern: impl Into<String>) -> Self {
        self.ask.push(PermissionRule::new(pattern, Permission::Ask));
        self
    }
    pub fn allow(mut self, pattern: impl Into<String>) -> Self {
        self.allow.push(PermissionRule::new(pattern, Permission::Allow));
        self
    }
    pub fn with_default(mut self, d: Permission) -> Self {
        self.default = d;
        self
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn glob() {
        assert!(glob_match("math.*", "math.add"));
        assert!(glob_match("math.*", "math.deep.nested"));
        assert!(!glob_match("math.*", "text.upper"));
        assert!(glob_match("*", "anything.at.all"));
        assert!(glob_match("a?c", "abc"));
        assert!(!glob_match("a?c", "ac"));
    }

    /// The shipped default is fully open: no rules, everything allowed —
    /// including the system.* tools that older defaults hard-denied.
    #[test]
    fn default_policy_is_fully_open() {
        let p = PermissionPolicy::default();
        struct T(&'static str);
        #[async_trait::async_trait]
        impl crate::tool::Tool for T {
            fn name(&self) -> &str { self.0 }
            fn description(&self) -> &str { "" }
            fn schema(&self) -> crate::schema::Schema { crate::schema::Schema::new() }
            fn source(&self) -> crate::tool::ToolSourceKind { crate::tool::ToolSourceKind::Builtin }
            async fn execute(&self, _: &serde_json::Value, _: &crate::tool::ToolContext) -> crate::error::ToolResult<serde_json::Value> { Ok(serde_json::json!({})) }
        }
        for name in ["math.add", "text.echo", "system.shell", "fs.write", "net.fetch", "anything.at.all"] {
            assert_eq!(p.decide(&T(name)), Permission::Allow, "{name} must be allowed by the default policy");
        }
    }

    #[test]
    fn deny_wins_over_allow() {
        // Rule-precedence test with an explicit opt-in policy (the shipped
        // default carries no rules at all).
        let p = PermissionPolicy::default()
            .allow("dangerous.*")
            .deny("dangerous.rm")
            .with_default(Permission::Ask);
        // We need a tool to decide against; build a tiny stub.
        struct T(&'static str);
        #[async_trait::async_trait]
        impl crate::tool::Tool for T {
            fn name(&self) -> &str { self.0 }
            fn description(&self) -> &str { "" }
            fn schema(&self) -> crate::schema::Schema { crate::schema::Schema::new() }
            fn source(&self) -> crate::tool::ToolSourceKind { crate::tool::ToolSourceKind::Builtin }
            async fn execute(&self, _: &serde_json::Value, _: &crate::tool::ToolContext) -> crate::error::ToolResult<serde_json::Value> { Ok(serde_json::json!({})) }
        }
        assert_eq!(p.decide(&T("dangerous.rm")), Permission::Deny); // deny rule wins
        assert_eq!(p.decide(&T("dangerous.other")), Permission::Allow); // allow rule
        assert_eq!(p.decide(&T("math.add")), Permission::Ask); // no rule → explicit Ask default
        assert_eq!(p.decide(&T("web.fetch")), Permission::Ask);
    }
}
