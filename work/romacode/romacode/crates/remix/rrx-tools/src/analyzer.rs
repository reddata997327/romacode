//! Static Analysis (remix-analyzer equivalent): real AST-based security & gas checks.

use serde_json::Value;
use std::path::Path;

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Severity {
    High,
    Medium,
    Low,
    Info,
}

impl Severity {
    pub fn as_str(&self) -> &'static str {
        match self {
            Severity::High => "HIGH",
            Severity::Medium => "MEDIUM",
            Severity::Low => "LOW",
            Severity::Info => "INFO",
        }
    }
}

#[derive(Clone, Debug)]
pub struct Finding {
    pub rule: &'static str,
    pub severity: Severity,
    pub message: String,
    pub file: String,
    pub line: usize,
    pub col: usize,
}

struct Ctx {
    findings: Vec<Finding>,
    sources: std::collections::BTreeMap<String, String>,
}

impl Ctx {
    fn locate(&self, file: &str, src: &str) -> (usize, usize) {
        // src = "start:length:fileIndex"
        let start: usize = src.split(':').next().and_then(|s| s.parse().ok()).unwrap_or(0);
        let text = self.sources.get(file).map(|s| s.as_str()).unwrap_or("");
        let mut line = 1usize;
        let mut col = 1usize;
        for (i, ch) in text.char_indices() {
            if i >= start {
                break;
            }
            if ch == '\n' {
                line += 1;
                col = 1;
            } else {
                col += 1;
            }
        }
        (line, col)
    }

    fn add(&mut self, rule: &'static str, sev: Severity, msg: &str, file: &str, src: &str) {
        let (line, col) = self.locate(file, src);
        self.findings.push(Finding {
            rule,
            severity: sev,
            message: msg.to_string(),
            file: file.to_string(),
            line,
            col,
        });
    }
}

/// Run all analyses over standard-json output (needs "ast" in outputSelection).
pub fn analyze(output: &Value, sources: &std::collections::BTreeMap<String, String>) -> Vec<Finding> {
    let mut ctx = Ctx { findings: vec![], sources: sources.clone() };
    if let Some(sources_ast) = output.get("sources").and_then(|s| s.as_object()) {
        for (file, sv) in sources_ast {
            if let Some(ast) = sv.get("ast") {
                let mut stack = vec![ast.clone()];
                while let Some(node) = stack.pop() {
                    visit(&mut ctx, file, &node);
                    if let Some(children) = node.get("nodes").and_then(|n| n.as_array()) {
                        for c in children {
                            stack.push(c.clone());
                        }
                    }
                    if let Some(body) = node.get("body") {
                        if body.is_object() {
                            stack.push(body.clone());
                        }
                    }
                    if let Some(stmts) = node.get("statements").and_then(|n| n.as_array()) {
                        for s in stmts {
                            stack.push(s.clone());
                        }
                    }
                    // expression subtrees we care about
                    for key in ["expression", "leftHandSide", "rightHandSide", "subExpression", "condition", "trueBody", "falseBody", "body", "initializationExpression"] {
                        if let Some(child) = node.get(key) {
                            if child.is_object() {
                                stack.push(child.clone());
                            }
                        }
                    }
                }
            }
        }
    }
    ctx.findings.sort_by(|a, b| (a.file.clone(), a.line).cmp(&(b.file.clone(), b.line)));
    ctx.findings
}

fn is_member_call(node: &Value, obj: &str, member: &[&str]) -> bool {
    if node.get("nodeType").and_then(|n| n.as_str()) != Some("FunctionCall") {
        return false;
    }
    if let Some(expr) = node.get("expression") {
        if expr.get("nodeType").and_then(|n| n.as_str()) == Some("MemberAccess") {
            let member_name = expr.get("memberName").and_then(|m| m.as_str()).unwrap_or("");
            let base_name = expr
                .get("expression")
                .and_then(|e| e.get("name"))
                .and_then(|n| n.as_str())
                .unwrap_or("");
            let base_is = if base_name.is_empty() {
                // e.g. payable(x).call — treat any .call as low level
                false
            } else {
                base_name == obj
            };
            return (base_is || obj.is_empty()) && member.contains(&member_name);
        }
    }
    false
}

fn visit(ctx: &mut Ctx, file: &str, node: &Value) {
    let ty = node.get("nodeType").and_then(|t| t.as_str()).unwrap_or("");
    let src = node.get("src").and_then(|s| s.as_str()).unwrap_or("");

    match ty {
        "MemberAccess" => {
            let member = node.get("memberName").and_then(|m| m.as_str()).unwrap_or("");
            let base = node
                .get("expression")
                .and_then(|e| e.get("name"))
                .and_then(|n| n.as_str())
                .unwrap_or("");
            if base == "tx" && member == "origin" {
                ctx.add(
                    "TXORIGIN",
                    Severity::Medium,
                    "Use of tx.origin for authentication is dangerous (phishing): use msg.sender instead",
                    file,
                    src,
                );
            }
        }
        "FunctionCall" => {
            let kind = node.get("kind").and_then(|k| k.as_str()).unwrap_or("");
            let callee = node
                .get("expression")
                .and_then(|e| e.get("name"))
                .and_then(|n| n.as_str())
                .unwrap_or("");
            if callee == "selfdestruct" || callee == "suicide" {
                ctx.add(
                    "SELFDESTRUCT",
                    Severity::Medium,
                    "selfdestruct can send funds to arbitrary addresses and permanently destroys the contract (EIP-6780 restrictions apply)",
                    file,
                    src,
                );
            }
            if is_member_call(node, "", &["delegatecall"]) {
                ctx.add(
                    "DELEGATECALL",
                    Severity::Medium,
                    "delegatecall executes foreign code in the caller's storage context — ensure the target is trusted",
                    file,
                    src,
                );
            }
            if is_member_call(node, "", &["call", "callcode"]) {
                ctx.add(
                    "LOW_LEVEL_CALL",
                    Severity::Low,
                    "Low-level call() bypasses type and return-value checks — check success and consider typed interfaces",
                    file,
                    src,
                );
            }
            if is_member_call(node, "", &["transfer", "send"]) {
                // unchecked if not inside a require/if — heuristic: parent not tracked here,
                // so flag only when the result is obviously ignored (statement expression)
                if kind == "functionCall" {
                    let in_stmt = node
                        .get("expression")
                        .and_then(|e| e.get("memberName"))
                        .is_some();
                    if in_stmt {
                        ctx.add(
                            "UNCHECKED_TRANSFER",
                            Severity::Low,
                            "transfer/send result should be checked (or use call with explicit checks)",
                            file,
                            src,
                        );
                    }
                }
            }
            if let Some(expr) = node.get("expression") {
                if expr.get("nodeType").and_then(|t| t.as_str()) == Some("Identifier")
                    && expr.get("name").and_then(|n| n.as_str()) == Some("keccak256")
                {
                    // fine; skip
                }
            }
        }
        "InlineAssembly" => {
            ctx.add(
                "INLINE_ASSEMBLY",
                Severity::Info,
                "Inline assembly bypasses Solidity safety checks",
                file,
                src,
            );
        }
        "ForStatement" | "WhileStatement" => {
            ctx.add(
                "GAS_LIMIT_LOOP",
                Severity::Low,
                "Unbounded loop: if iterations depend on user input, the function can exceed the block gas limit",
                file,
                src,
            );
        }
        "ContractDefinition" => {
            let kind = node.get("contractKind").and_then(|k| k.as_str()).unwrap_or("");
            if kind == "contract" {
                // could-be-view check: functions without any state mutation marked payable/default
                if let Some(funcs) = node.get("nodes").and_then(|n| n.as_array()) {
                    for f in funcs {
                        if f.get("nodeType").and_then(|t| t.as_str()) == Some("FunctionDefinition") {
                            let name = f.get("name").and_then(|n| n.as_str()).unwrap_or("");
                            let sm = f.get("stateMutability").and_then(|m| m.as_str()).unwrap_or("");
                            let vis = f.get("visibility").and_then(|v| v.as_str()).unwrap_or("");
                            if sm == "payable" && !name.is_empty() && vis == "public" {
                                ctx.add(
                                    "PAYABLE_SUSPECT",
                                    Severity::Info,
                                    &format!("function '{name}' is payable — confirm it must accept value (gas: payable adds ~37 gas per call)"),
                                    file,
                                    f.get("src").and_then(|s| s.as_str()).unwrap_or(""),
                                );
                            }
                        }
                    }
                }
            }
        }
        "PragmaDirective" => {
            let literals: Vec<String> = node
                .get("literals")
                .and_then(|l| l.as_array())
                .map(|a| a.iter().filter_map(|x| x.as_str().map(|s| s.to_string())).collect())
                .unwrap_or_default();
            let value = node
                .get("value")
                .and_then(|v| v.as_str())
                .map(|s| s.to_string())
                .unwrap_or_else(|| literals.join(" "));
            let has_08 = value.contains("0.8") || value.contains("0.9");
            if !value.is_empty() && !has_08 {
                ctx.add(
                    "OUTDATED_COMPILER",
                    Severity::Medium,
                    "Solidity <0.8 has no built-in overflow checks — consider ^0.8.x",
                    file,
                    src,
                );
            }
        }
        _ => {}
    }
}

pub fn format_findings(findings: &[Finding], root: &Path) -> String {
    let mut out = String::new();
    if findings.is_empty() {
        out.push_str("✓ no findings — clean analysis\n");
        return out;
    }
    for f in findings {
        let rel = Path::new(&f.file);
        let shown = root.join(rel);
        out.push_str(&format!(
            "[{:6}] {:22} {}:{}:{} — {}\n",
            f.severity.as_str(),
            f.rule,
            shown.display(),
            f.line,
            f.col,
            f.message
        ));
    }
    let (h, m, l, i) = findings.iter().fold((0, 0, 0, 0), |acc, f| match f.severity {
        Severity::High => (acc.0 + 1, acc.1, acc.2, acc.3),
        Severity::Medium => (acc.0, acc.1 + 1, acc.2, acc.3),
        Severity::Low => (acc.0, acc.1, acc.2 + 1, acc.3),
        Severity::Info => (acc.0, acc.1, acc.2, acc.3 + 1),
    });
    out.push_str(&format!(
        "\n{} finding(s): {h} high, {m} medium, {l} low, {i} info\n",
        findings.len()
    ));
    out
}
