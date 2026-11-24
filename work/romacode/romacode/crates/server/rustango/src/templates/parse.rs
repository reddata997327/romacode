//! Template lexer, parser and renderer for the Django Template Language.

use std::collections::HashMap;
use std::sync::Arc;

use super::{Context, Engine, TplValue};

// ---------------------------------------------------------------------------
// Expressions
// ---------------------------------------------------------------------------

/// A value expression: literals, variables, filter chains.
#[derive(Debug, Clone)]
pub enum ValueExpr {
    /// Variable path (`user.name`, `items.0`).
    Var(String),
    Str(String),
    Int(i64),
    Float(f64),
    Bool(bool),
    Filtered {
        expr: Box<ValueExpr>,
        filter: String,
        arg: Option<Box<ValueExpr>>,
    },
}

impl ValueExpr {
    pub fn eval(&self, ctx: &Context) -> TplValue {
        match self {
            ValueExpr::Var(path) => ctx.resolve(path).unwrap_or(TplValue::Null),
            ValueExpr::Str(s) => TplValue::Str(s.clone()),
            ValueExpr::Int(i) => TplValue::Int(*i),
            ValueExpr::Float(f) => TplValue::Float(*f),
            ValueExpr::Bool(b) => TplValue::Bool(*b),
            ValueExpr::Filtered { expr, filter, arg } => {
                let base = expr.eval(ctx);
                let arg_v = arg.as_ref().map(|a| a.eval(ctx));
                match super::filters::apply_filter(filter, base, arg_v, ctx) {
                    Ok(v) => v,
                    Err(_) => TplValue::Str(format!("(bad filter {filter})")),
                }
            }
        }
    }
}

/// Comparison operators for `{% if %}`.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Cmp { Eq, Ne, Lt, Lte, Gt, Gte }

impl Cmp {
    fn apply(&self, left: &TplValue, right: &TplValue) -> bool {
        match self {
            Cmp::Eq => tpl_eq(left, right),
            Cmp::Ne => !tpl_eq(left, right),
            _ => {
                let (lf, rf) = (numeric(left), numeric(right));
                if let (Some(a), Some(b)) = (lf, rf) {
                    return match self {
                        Cmp::Lt => a < b,
                        Cmp::Lte => a <= b,
                        Cmp::Gt => a > b,
                        Cmp::Gte => a >= b,
                        _ => false,
                    };
                }
                let (ls, rs) = (left.as_str(), right.as_str());
                match self {
                    Cmp::Lt => ls < rs,
                    Cmp::Lte => ls <= rs,
                    Cmp::Gt => ls > rs,
                    Cmp::Gte => ls >= rs,
                    _ => false,
                }
            }
        }
    }
}

fn numeric(v: &TplValue) -> Option<f64> {
    match v {
        TplValue::Int(i) => Some(*i as f64),
        TplValue::Float(f) => Some(*f),
        TplValue::Str(s) => s.trim().parse().ok(),
        TplValue::DateTime(d) => Some(d.timestamp() as f64),
        _ => None,
    }
}

fn tpl_eq(a: &TplValue, b: &TplValue) -> bool {
    match (a, b) {
        (TplValue::Str(x), TplValue::Str(y))
        | (TplValue::Safe(x), TplValue::Str(y))
        | (TplValue::Str(x), TplValue::Safe(y))
        | (TplValue::Safe(x), TplValue::Safe(y)) => x == y,
        _ => {
            if let (Some(x), Some(y)) = (numeric(a), numeric(b)) {
                return x == y;
            }
            a.as_str() == b.as_str()
        }
    }
}

/// `{% if %}` conditions.
#[derive(Debug, Clone)]
pub enum CondExpr {
    Or(Box<CondExpr>, Box<CondExpr>),
    And(Box<CondExpr>, Box<CondExpr>),
    Not(Box<CondExpr>),
    Compare { left: ValueExpr, op: Cmp, right: ValueExpr },
    In { left: ValueExpr, negated: bool, right: ValueExpr },
    Truthy(ValueExpr),
}

impl CondExpr {
    pub fn eval(&self, ctx: &Context) -> bool {
        match self {
            CondExpr::Or(a, b) => a.eval(ctx) || b.eval(ctx),
            CondExpr::And(a, b) => a.eval(ctx) && b.eval(ctx),
            CondExpr::Not(a) => !a.eval(ctx),
            CondExpr::Compare { left, op, right } => {
                op.apply(&left.eval(ctx), &right.eval(ctx))
            }
            CondExpr::In { left, negated, right } => {
                let lv = left.eval(ctx);
                let rv = right.eval(ctx);
                let found = match rv {
                    TplValue::List(items) => items.iter().any(|x| tpl_eq(x, &lv)),
                    TplValue::Map(m) => match &lv {
                        TplValue::Str(s) => m.contains_key(s),
                        _ => false,
                    },
                    TplValue::Str(hay) => hay.contains(&lv.as_str()),
                    _ => false,
                };
                if *negated { !found } else { found }
            }
            CondExpr::Truthy(v) => v.eval(ctx).is_truthy(),
        }
    }
}

// ---------------------------------------------------------------------------
// Nodes
// ---------------------------------------------------------------------------

/// A parsed template node.
#[derive(Debug, Clone)]
pub enum Node {
    Text(String),
    /// `{{ expr }}` — autoescape flag controls escaping of plain strings.
    Var(ValueExpr, bool),
    For {
        vars: Vec<String>,
        iter: ValueExpr,
        body: Vec<Node>,
        empty: Vec<Node>,
    },
    If {
        branches: Vec<(CondExpr, Vec<Node>)>,
        else_body: Vec<Node>,
    },
    Block {
        name: String,
        body: Vec<Node>,
    },
    Include {
        name: ValueExpr,
    },
    With {
        assigns: Vec<(String, ValueExpr)>,
        body: Vec<Node>,
    },
    CsrfToken,
    Url {
        name: String,
        args: Vec<ValueExpr>,
    },
    Static {
        path: ValueExpr,
    },
    Trans {
        key: ValueExpr,
    },
    Autoescape {
        enabled: bool,
        body: Vec<Node>,
    },
}

/// A compiled template.
#[derive(Debug, Clone, Default)]
pub struct Template {
    pub nodes: Vec<Node>,
    pub extends: Option<String>,
    /// All blocks by name (for `{% extends %}` inheritance).
    pub blocks: HashMap<String, Vec<Node>>,
}

impl Template {
    /// Parse template source into a node tree.
    pub fn parse(src: &str) -> Result<Template, String> {
        let chunks = lex(src)?;
        let mut parser = Parser::new(chunks);
        let nodes = parser.parse_until(&[])?;
        let mut template = Template {
            extends: parser.extends.clone(),
            blocks: collect_blocks(&nodes),
            nodes,
        };
        if parser.extends.is_some() {
            // Top-level content of an extending template is its blocks only.
            template.nodes.retain(|n| matches!(n, Node::Block { .. }));
        }
        Ok(template)
    }

    /// Render with the engine (handles `{% extends %}` inheritance).
    pub fn render_with_engine(&self, engine: &Engine, ctx: &mut Context) -> Result<String, String> {
        match &self.extends {
            Some(parent_name) => {
                let parent = engine.compile(parent_name)?;
                let mut blocks = self.blocks.clone();
                // Chain: parent may itself extend; merge its blocks under ours.
                for (name, nodes) in &parent.blocks {
                    blocks.entry(name.clone()).or_insert_with(|| nodes.clone());
                }
                parent.render_nodes_with_overrides(engine, ctx, &blocks)
            }
            None => self.render_nodes_with_overrides(engine, ctx, &self.blocks),
        }
    }

    pub(crate) fn render_nodes_with_overrides(
        &self,
        engine: &Engine,
        ctx: &mut Context,
        overrides: &HashMap<String, Vec<Node>>,
    ) -> Result<String, String> {
        let mut out = String::new();
        render_nodes(&self.nodes, engine, ctx, overrides, true, &mut out)?;
        Ok(out)
    }
}

fn collect_blocks(nodes: &[Node]) -> HashMap<String, Vec<Node>> {
    let mut map = HashMap::new();
    fn walk(nodes: &[Node], map: &mut HashMap<String, Vec<Node>>) {
        for n in nodes {
            match n {
                Node::Block { name, body } => {
                    map.insert(name.clone(), body.clone());
                    walk(body, map);
                }
                Node::For { body, empty, .. } => {
                    walk(body, map);
                    walk(empty, map);
                }
                Node::If { branches, else_body } => {
                    for (_, b) in branches {
                        walk(b, map);
                    }
                    walk(else_body, map);
                }
                Node::With { body, .. } => walk(body, map),
                Node::Autoescape { body, .. } => walk(body, map),
                _ => {}
            }
        }
    }
    walk(nodes, &mut map);
    map
}

// ---------------------------------------------------------------------------
// Lexer
// ---------------------------------------------------------------------------

#[derive(Debug, Clone)]
enum Chunk {
    Text(String),
    Var(String),
    Tag(String),
}

fn lex(src: &str) -> Result<Vec<Chunk>, String> {
    let mut chunks = Vec::new();
    let rest = src;
    let mut text_start = 0usize;
    let bytes = rest.as_bytes();
    let mut i = 0;
    let mut text = String::new();
    while i < bytes.len() {
        if bytes[i] == b'{' && i + 1 < bytes.len() {
            match bytes[i + 1] {
                b'{' => {
                    // {{ ... }}
                    if let Some(end_rel) = rest[i + 2..].find("}}") {
                        let end = i + 2 + end_rel;
                        if i > text_start {
                            text.push_str(&rest[text_start..i]);
                        }
                        if !text.is_empty() {
                            chunks.push(Chunk::Text(std::mem::take(&mut text)));
                        }
                        chunks.push(Chunk::Var(rest[i + 2..end].trim().to_string()));
                        i = end + 2;
                        text_start = i;
                        continue;
                    } else {
                        return Err("unclosed `{{`".into());
                    }
                }
                b'%' => {
                    // {% ... %}
                    if let Some(end_rel) = rest[i + 2..].find("%}") {
                        let end = i + 2 + end_rel;
                        if i > text_start {
                            text.push_str(&rest[text_start..i]);
                        }
                        if !text.is_empty() {
                            chunks.push(Chunk::Text(std::mem::take(&mut text)));
                        }
                        chunks.push(Chunk::Tag(rest[i + 2..end].trim().to_string()));
                        i = end + 2;
                        text_start = i;
                        continue;
                    } else {
                        return Err("unclosed `{%`".into());
                    }
                }
                b'#' => {
                    // {# comment #}
                    if let Some(end_rel) = rest[i + 2..].find("#}") {
                        let end = i + 2 + end_rel;
                        if i > text_start {
                            text.push_str(&rest[text_start..i]);
                        }
                        if !text.is_empty() {
                            chunks.push(Chunk::Text(std::mem::take(&mut text)));
                        }
                        i = end + 2;
                        text_start = i;
                        continue;
                    } else {
                        return Err("unclosed `{#`".into());
                    }
                }
                _ => {}
            }
        }
        i += 1;
    }
    let _ = text_start;
    if text_start < rest.len() {
        text.push_str(&rest[text_start..]);
    }
    if !text.is_empty() {
        chunks.push(Chunk::Text(text));
    }
    Ok(chunks)
}

// ---------------------------------------------------------------------------
// Tag token scanner
// ---------------------------------------------------------------------------

/// Split tag content into tokens, respecting quotes: `url 'x' 'a b' 42`.
#[derive(Debug, Clone, PartialEq)]
enum Tok {
    Word(String),
    Str(String),
    Num(String),
    Punct(char),
}

fn tokenize(content: &str) -> Vec<Tok> {
    let mut toks = Vec::new();
    let chars: Vec<char> = content.chars().collect();
    let mut i = 0;
    while i < chars.len() {
        let c = chars[i];
        if c.is_whitespace() {
            i += 1;
        } else if c == '\'' || c == '"' {
            let quote = c;
            let mut s = String::new();
            i += 1;
            while i < chars.len() && chars[i] != quote {
                if chars[i] == '\\' && i + 1 < chars.len() {
                    i += 1;
                }
                s.push(chars[i]);
                i += 1;
            }
            i += 1; // closing quote
            toks.push(Tok::Str(s));
        } else if c.is_ascii_digit()
            || (c == '-' && i + 1 < chars.len() && chars[i + 1].is_ascii_digit())
        {
            let mut s = String::new();
            if c == '-' {
                s.push('-');
                i += 1;
            }
            while i < chars.len() && (chars[i].is_ascii_digit() || chars[i] == '.') {
                s.push(chars[i]);
                i += 1;
            }
            toks.push(Tok::Num(s));
        } else if c.is_ascii_alphanumeric() || c == '_' || c == '.' {
            let mut s = String::new();
            while i < chars.len()
                && (chars[i].is_ascii_alphanumeric() || chars[i] == '_' || chars[i] == '.')
            {
                s.push(chars[i]);
                i += 1;
            }
            toks.push(Tok::Word(s));
        } else {
            toks.push(Tok::Punct(c));
            i += 1;
        }
    }
    toks
}

// ---------------------------------------------------------------------------
// Parser
// ---------------------------------------------------------------------------

struct Parser {
    chunks: Vec<Chunk>,
    pos: usize,
    extends: Option<String>,
}

impl Parser {
    fn new(chunks: Vec<Chunk>) -> Parser {
        Parser { chunks, pos: 0, extends: None }
    }

    fn parse_until(&mut self, stops: &[&str]) -> Result<Vec<Node>, String> {
        let mut nodes = Vec::new();
        while self.pos < self.chunks.len() {
            let chunk = self.chunks[self.pos].clone();
            self.pos += 1;
            match chunk {
                Chunk::Text(t) => nodes.push(Node::Text(t)),
                Chunk::Var(content) => {
                    let expr = parse_value_expr(&content)?;
                    nodes.push(Node::Var(expr, true));
                }
                Chunk::Tag(content) => {
                    let toks = tokenize(&content);
                    let first = toks
                        .first()
                        .cloned()
                        .ok_or_else(|| "empty tag".to_string())?;
                    let kw = match &first {
                        Tok::Word(w) => w.clone(),
                        _ => return Err(format!("unexpected tag `{content}`")),
                    };
                    if stops.contains(&kw.as_str()) {
                        // Push back and stop.
                        self.pos -= 1;
                        return Ok(nodes);
                    }
                    match kw.as_str() {
                        "extends" => {
                            self.extends = Some(expect_string(&toks, 1, "extends")?);
                        }
                        "block" => {
                            let name = match toks.get(1) {
                                Some(Tok::Word(w)) => w.clone(),
                                _ => return Err("block needs a name".into()),
                            };
                            let body = self.parse_until(&["endblock"])?;
                            self.pos += 1; // consume endblock
                            nodes.push(Node::Block { name, body });
                        }
                        "for" => {
                            // for x[, y] in expr
                            let mut vars = Vec::new();
                            let mut j = 1;
                            while j < toks.len() {
                                match &toks[j] {
                                    Tok::Word(w) if w != "in" => {
                                        vars.push(w.clone());
                                        j += 1;
                                        if matches!(toks.get(j), Some(Tok::Punct(','))) {
                                            j += 1;
                                        }
                                    }
                                    Tok::Word(w) if w == "in" => {
                                        j += 1;
                                        break;
                                    }
                                    _ => return Err("bad for syntax".into()),
                                }
                            }
                            let rest: Vec<Tok> = toks[j..].to_vec();
                            let iter = parse_value_expr_from_toks(&rest)?;
                            let body = self.parse_until(&["empty", "endfor"])?;
                            let mut empty = Vec::new();
                            if self.stops_at("empty") {
                                self.pos += 1;
                                empty = self.parse_until(&["endfor"])?;
                            }
                            self.pos += 1; // consume endfor
                            nodes.push(Node::For { vars, iter, body, empty });
                        }
                        "if" => {
                            let mut branches = Vec::new();
                            let cond = parse_cond_from_toks(&toks[1..])?;
                            let body = self.parse_until(&["elif", "else", "endif"])?;
                            branches.push((cond, body));
                            loop {
                                let cur = self.current_tag_kw();
                                match cur.as_deref() {
                                    Some("elif") => {
                                        let toks2 =
                                            tokenize(&self.current_tag_content().unwrap_or_default());
                                        let cond = parse_cond_from_toks(&toks2[1..])?;
                                        self.pos += 1;
                                        let body = self.parse_until(&["elif", "else", "endif"])?;
                                        branches.push((cond, body));
                                    }
                                    Some("else") => {
                                        self.pos += 1;
                                        let body = self.parse_until(&["endif"])?;
                                        self.pos += 1;
                                        nodes.push(Node::If { branches, else_body: body });
                                        break;
                                    }
                                    Some("endif") => {
                                        self.pos += 1;
                                        nodes.push(Node::If { branches, else_body: vec![] });
                                        break;
                                    }
                                    _ => return Err("if without endif".into()),
                                }
                            }
                        }
                        "include" => {
                            let expr = parse_value_expr_from_toks(&toks[1..])?;
                            nodes.push(Node::Include { name: expr });
                        }
                        "with" => {
                            // with a = 1, b = 2
                            let mut assigns = Vec::new();
                            let mut pending_key: Option<String> = None;
                            for tok in &toks[1..] {
                                match tok {
                                    Tok::Word(w) if pending_key.is_none() => {
                                        pending_key = Some(w.clone());
                                    }
                                    Tok::Punct('=') => {}
                                    Tok::Punct(',') => {}
                                    _ => {
                                        let key = pending_key
                                            .take()
                                            .ok_or("with: expected `key = value`")?;
                                        // value may be a multi-token expr: gather rest? keep single-token exprs
                                        let single = vec![tok.clone()];
                                        let expr = parse_value_expr_from_toks(&single)?;
                                        assigns.push((key, expr));
                                    }
                                }
                            }
                            let body = self.parse_until(&["endwith"])?;
                            self.pos += 1;
                            nodes.push(Node::With { assigns, body });
                        }
                        "csrf_token" => nodes.push(Node::CsrfToken),
                        "url" => {
                            let name = expect_string(&toks, 1, "url")?;
                            let mut args = Vec::new();
                            let mut j = 2;
                            while j < toks.len() {
                                args.push(parse_value_expr_from_toks(&[toks[j].clone()])?);
                                j += 1;
                            }
                            nodes.push(Node::Url { name, args });
                        }
                        "static" => {
                            let expr = parse_value_expr_from_toks(&toks[1..])?;
                            nodes.push(Node::Static { path: expr });
                        }
                        "trans" => {
                            let expr = parse_value_expr_from_toks(&toks[1..])?;
                            nodes.push(Node::Trans { key: expr });
                        }
                        "comment" => {
                            self.parse_until(&["endcomment"])?;
                            self.pos += 1;
                        }
                        "autoescape" => {
                            let enabled = match toks.get(1) {
                                Some(Tok::Word(w)) if w == "on" => true,
                                Some(Tok::Word(w)) if w == "off" => false,
                                _ => return Err("autoescape needs on|off".into()),
                            };
                            let body = self.parse_until(&["endautoescape"])?;
                            self.pos += 1;
                            nodes.push(Node::Autoescape { enabled, body });
                        }
                        other => {
                            return Err(format!("unknown tag `{other}`"));
                        }
                    }
                }
            }
        }
        if stops.is_empty() {
            Ok(nodes)
        } else {
            Err(format!("unclosed tag: expected one of {stops:?}"))
        }
    }

    fn stops_at(&self, kw: &str) -> bool {
        self.current_tag_kw().as_deref() == Some(kw)
    }

    fn current_tag_kw(&self) -> Option<String> {
        match self.chunks.get(self.pos) {
            Some(Chunk::Tag(content)) => {
                let toks = tokenize(content);
                match toks.first() {
                    Some(Tok::Word(w)) => Some(w.clone()),
                    _ => None,
                }
            }
            _ => None,
        }
    }

    fn current_tag_content(&self) -> Option<String> {
        match self.chunks.get(self.pos) {
            Some(Chunk::Tag(content)) => Some(content.clone()),
            _ => None,
        }
    }
}

fn expect_string(toks: &[Tok], idx: usize, ctx: &str) -> Result<String, String> {
    match toks.get(idx) {
        Some(Tok::Str(s)) => Ok(s.clone()),
        _ => Err(format!("{ctx} expects a quoted string")),
    }
}

// ---------------------------------------------------------------------------
// Value expression parsing (with filters)
// ---------------------------------------------------------------------------

/// Parse `var.path|filter:arg|filter2` from a string.
pub fn parse_value_expr(content: &str) -> Result<ValueExpr, String> {
    let toks = tokenize(content);
    parse_value_expr_from_toks(&toks)
}

fn parse_value_expr_from_toks(toks: &[Tok]) -> Result<ValueExpr, String> {
    // Split on '|' at top level; the first chunk is the base expr, the rest
    // are filters (with optional `:arg`).
    let mut groups: Vec<Vec<Tok>> = vec![Vec::new()];
    for tok in toks {
        if let Tok::Punct('|') = tok {
            groups.push(Vec::new());
        } else {
            groups.last_mut().unwrap().push(tok.clone());
        }
    }
    let base_group = groups
        .first()
        .ok_or_else(|| "empty expression".to_string())?;
    let mut expr = parse_base_expr(base_group)?;
    for filter_group in groups.into_iter().skip(1) {
        // filter[:arg]
        let filter_name = match filter_group.first() {
            Some(Tok::Word(w)) => w.clone(),
            _ => return Err("filter needs a name".into()),
        };
        let arg = if filter_group.len() > 2 {
            if matches!(filter_group.get(1), Some(Tok::Punct(':'))) {
                Some(Box::new(parse_base_expr(&filter_group[2..])?))
            } else {
                None
            }
        } else {
            None
        };
        expr = ValueExpr::Filtered { expr: Box::new(expr), filter: filter_name, arg };
    }
    Ok(expr)
}

fn parse_base_expr(toks: &[Tok]) -> Result<ValueExpr, String> {
    if toks.is_empty() {
        return Err("empty expression".into());
    }
    match &toks[0] {
        Tok::Str(s) => Ok(ValueExpr::Str(s.clone())),
        Tok::Num(n) => {
            if n.contains('.') {
                n.parse::<f64>()
                    .map(ValueExpr::Float)
                    .map_err(|e| e.to_string())
            } else {
                n.parse::<i64>()
                    .map(ValueExpr::Int)
                    .map_err(|e| e.to_string())
            }
        }
        Tok::Word(w) => match w.as_str() {
            "true" | "True" => Ok(ValueExpr::Bool(true)),
            "false" | "False" => Ok(ValueExpr::Bool(false)),
            "None" | "none" => Ok(ValueExpr::Var("None".into())),
            _ => Ok(ValueExpr::Var(w.clone())),
        },
        _ => Err(format!("unexpected token in expression: {:?}", toks[0])),
    }
}

// ---------------------------------------------------------------------------
// Condition parsing: or < and < not < comparison
// ---------------------------------------------------------------------------

fn parse_cond_from_toks(toks: &[Tok]) -> Result<CondExpr, String> {
    let toks = strip_parens(toks);
    // Split on 'or' (lowest precedence).
    let mut or_parts: Vec<Vec<Tok>> = vec![Vec::new()];
    for tok in &toks {
        if matches!(tok, Tok::Word(w) if w == "or") {
            or_parts.push(Vec::new());
        } else {
            or_parts.last_mut().unwrap().push(tok.clone());
        }
    }
    if or_parts.len() > 1 {
        let mut expr = parse_cond_from_toks(&or_parts[0])?;
        for part in or_parts.into_iter().skip(1) {
            let rhs = parse_cond_from_toks(&part)?;
            expr = CondExpr::Or(Box::new(expr), Box::new(rhs));
        }
        return Ok(expr);
    }

    // Split on 'and'.
    let mut and_parts: Vec<Vec<Tok>> = vec![Vec::new()];
    for tok in &toks {
        if matches!(tok, Tok::Word(w) if w == "and") {
            and_parts.push(Vec::new());
        } else {
            and_parts.last_mut().unwrap().push(tok.clone());
        }
    }
    if and_parts.len() > 1 {
        let mut expr = parse_cond_from_toks(&and_parts[0])?;
        for part in and_parts.into_iter().skip(1) {
            let rhs = parse_cond_from_toks(&part)?;
            expr = CondExpr::And(Box::new(expr), Box::new(rhs));
        }
        return Ok(expr);
    }

    // not ...
    if matches!(toks.first(), Some(Tok::Word(w)) if w == "not") {
        let inner = parse_cond_from_toks(&toks[1..])?;
        return Ok(CondExpr::Not(Box::new(inner)));
    }

    // not in / in / comparisons / truthy
    for (i, tok) in toks.iter().enumerate() {
        if let Tok::Word(w) = tok {
            if w == "in" || w == "not" {
                // left in right | left not in right
                let (left, negated) = if w == "not" {
                    if !matches!(toks.get(i + 1), Some(Tok::Word(n)) if n == "in") {
                        return Err("`not` must be followed by `in`".into());
                    }
                    (&toks[..i], true)
                } else {
                    (&toks[..i], false)
                };
                let skip = if negated { i + 2 } else { i + 1 };
                let left_expr = parse_value_expr_from_toks(left)?;
                let right_expr = parse_value_expr_from_toks(&toks[skip..])?;
                return Ok(CondExpr::In { left: left_expr, negated, right: right_expr });
            }
        }
        if let Tok::Word(w) = tok {
            if let Some(op) = match w.as_str() {
                "==" => Some(Cmp::Eq),
                "!=" => Some(Cmp::Ne),
                "<" => Some(Cmp::Lt),
                "<=" => Some(Cmp::Lte),
                ">" => Some(Cmp::Gt),
                ">=" => Some(Cmp::Gte),
                _ => None,
            } {
                let left = parse_value_expr_from_toks(&toks[..i])?;
                let right = parse_value_expr_from_toks(&toks[i + 1..])?;
                return Ok(CondExpr::Compare { left, op, right });
            }
        }
        if let Tok::Punct(p) = tok {
            let op = match p {
                '=' if matches!(toks.get(i + 1), Some(Tok::Punct('='))) => Some(Cmp::Eq),
                '!' if matches!(toks.get(i + 1), Some(Tok::Punct('='))) => Some(Cmp::Ne),
                '<' if matches!(toks.get(i + 1), Some(Tok::Punct('='))) => Some(Cmp::Lte),
                '>' if matches!(toks.get(i + 1), Some(Tok::Punct('='))) => Some(Cmp::Gte),
                '<' => Some(Cmp::Lt),
                '>' => Some(Cmp::Gt),
                _ => None,
            };
            if let Some(op) = op {
                let skip = if matches!(p, '=' | '!' | '<' | '>')
                    && matches!(toks.get(i + 1), Some(Tok::Punct('=')))
                {
                    i + 2
                } else {
                    i + 1
                };
                // `==` / `!=` come as two Punct tokens; `<` `>` as one
                let left = parse_value_expr_from_toks(&toks[..i])?;
                let right = parse_value_expr_from_toks(&toks[skip..])?;
                return Ok(CondExpr::Compare { left, op, right });
            }
        }
    }

    // Plain truthiness.
    let expr = parse_value_expr_from_toks(&toks)?;
    Ok(CondExpr::Truthy(expr))
}

fn strip_parens(toks: &[Tok]) -> Vec<Tok> {
    // Remove a single wrapping paren pair only if it SPANS everything: the
    // first token must be `(` and its matching close paren (tracked with a
    // depth counter) must be the final token. `(a == 1) and (b == 2)` has
    // first=`(` and last=`)` too, but the matching close of the first paren
    // is not the last token — stripping there mangled the token stream.
    if toks.len() >= 3 {
        if matches!(toks.first(), Some(Tok::Punct('('))) {
            let mut depth: usize = 0;
            for (i, tok) in toks.iter().enumerate() {
                match tok {
                    Tok::Punct('(') => depth += 1,
                    Tok::Punct(')') => {
                        depth = depth.saturating_sub(1);
                        if depth == 0 {
                            if i == toks.len() - 1 {
                                return toks[1..i].to_vec();
                            }
                            // Matching close is not the final token.
                            break;
                        }
                    }
                    _ => {}
                }
            }
        }
    }
    toks.to_vec()
}

// ---------------------------------------------------------------------------
// Rendering
// ---------------------------------------------------------------------------

pub(crate) fn render_nodes(
    nodes: &[Node],
    engine: &Engine,
    ctx: &mut Context,
    overrides: &HashMap<String, Vec<Node>>,
    autoescape: bool,
    out: &mut String,
) -> Result<(), String> {
    for node in nodes {
        match node {
            Node::Text(t) => out.push_str(t),
            Node::Var(expr, _default_escape) => {
                let v = expr.eval(ctx);
                out.push_str(&render_value(&v, autoescape));
            }
            Node::For { vars, iter, body, empty } => {
                let iterable = iter.eval(ctx);
                let items: Vec<TplValue> = match iterable {
                    TplValue::List(items) => items,
                    TplValue::Map(m) => m
                        .iter()
                        .map(|(k, v)| {
                            TplValue::List(vec![TplValue::Str(k.clone()), v.clone()])
                        })
                        .collect(),
                    _ => vec![],
                };
                if items.is_empty() {
                    render_nodes(empty, engine, ctx, overrides, autoescape, out)?;
                } else {
                    let len = items.len();
                    for (idx, item) in items.into_iter().enumerate() {
                        ctx.push_scope();
                        if vars.len() == 1 {
                            ctx.set(vars[0].clone(), item);
                        } else {
                            // unpack pairs
                            if let TplValue::List(pair) = item {
                                for (vi, var) in vars.iter().enumerate() {
                                    let v = pair.get(vi).cloned().unwrap_or(TplValue::Null);
                                    ctx.set(var.clone(), v);
                                }
                            }
                        }
                        let mut forloop = HashMap::new();
                        forloop.insert("counter".to_string(), TplValue::Int(idx as i64 + 1));
                        forloop.insert("counter0".to_string(), TplValue::Int(idx as i64));
                        forloop.insert(
                            "revcounter".to_string(),
                            TplValue::Int((len - idx) as i64),
                        );
                        forloop.insert("first".to_string(), TplValue::Bool(idx == 0));
                        forloop.insert("last".to_string(), TplValue::Bool(idx + 1 == len));
                        ctx.set(
                            "forloop",
                            TplValue::Map(Arc::new(forloop)),
                        );
                        render_nodes(body, engine, ctx, overrides, autoescape, out)?;
                        ctx.pop_scope();
                    }
                }
            }
            Node::If { branches, else_body } => {
                let mut done = false;
                for (cond, body) in branches {
                    if cond.eval(ctx) {
                        render_nodes(body, engine, ctx, overrides, autoescape, out)?;
                        done = true;
                        break;
                    }
                }
                if !done {
                    render_nodes(else_body, engine, ctx, overrides, autoescape, out)?;
                }
            }
            Node::Block { name, body } => {
                let chosen = overrides.get(name).unwrap_or(body);
                render_nodes(chosen, engine, ctx, overrides, autoescape, out)?;
            }
            Node::Include { name } => {
                thread_local! {
                    /// Current `{% include %}` render depth on this thread.
                    static INCLUDE_DEPTH: std::cell::Cell<usize> = std::cell::Cell::new(0);
                }
                // Unbounded `{% include %}` recursion stack-overflows on a
                // cyclic include — an abort `catch_unwind` cannot catch. Cap
                // the depth instead and surface a render error.
                const MAX_INCLUDE_DEPTH: usize = 32;
                let depth = INCLUDE_DEPTH.with(|d| d.get());
                if depth >= MAX_INCLUDE_DEPTH {
                    return Err("template include depth exceeded (possible cycle)".into());
                }
                INCLUDE_DEPTH.with(|d| d.set(depth + 1));
                let result = (|| -> Result<(), String> {
                    let tpl_name = name.eval(ctx).as_str();
                    let child = engine.compile(&tpl_name)?;
                    let rendered = child.render_with_engine(engine, ctx)?;
                    out.push_str(&rendered);
                    Ok(())
                })();
                INCLUDE_DEPTH.with(|d| d.set(depth));
                result?;
            }
            Node::With { assigns, body } => {
                ctx.push_scope();
                for (key, expr) in assigns {
                    ctx.set(key.clone(), expr.eval(ctx));
                }
                render_nodes(body, engine, ctx, overrides, autoescape, out)?;
                ctx.pop_scope();
            }
            Node::CsrfToken => {
                let token = ctx
                    .resolve("csrf_token")
                    .map(|v| v.as_str())
                    .unwrap_or_default();
                if !token.is_empty() {
                    out.push_str(&format!(
                        "<input type=\"hidden\" name=\"csrfmiddlewaretoken\" value=\"{}\">",
                        escape(&token)
                    ));
                }
            }
            Node::Url { name, args } => {
                let args: Vec<String> = args.iter().map(|a| a.eval(ctx).as_str()).collect();
                let refs: Vec<&str> = args.iter().map(|s| s.as_str()).collect();
                match crate::urls::reverse(name, &refs) {
                    // Reversed URLs land in HTML (href attributes) and a
                    // `Converter::Str` slug may itself carry markup
                    // (`x"><script>...`), so the rendered URL must be
                    // HTML-escaped exactly like an auto-escaped `{{ var }}`.
                    Ok(url) => out.push_str(&escape(&url)),
                    Err(_) => out.push('#'),
                }
            }
            Node::Static { path } => {
                let p = path.eval(ctx).as_str();
                let base = ctx
                    .resolve("STATIC_URL")
                    .map(|v| v.as_str())
                    .unwrap_or_else(|| crate::settings::global().static_url.clone());
                if !base.ends_with('/') {
                    out.push_str(&format!("{base}/{}", escape(&p)));
                } else {
                    out.push_str(&format!("{base}{}", escape(&p)));
                }
            }
            Node::Trans { key } => {
                let k = key.eval(ctx).as_str();
                let lang = ctx
                    .resolve("_lang")
                    .map(|v| v.as_str())
                    .unwrap_or_else(|| crate::settings::global().language_code.clone());
                out.push_str(&crate::i18n::translate(&lang, &k));
            }
            Node::Autoescape { enabled, body } => {
                render_nodes(body, engine, ctx, overrides, *enabled, out)?;
            }
        }
    }
    Ok(())
}

fn render_value(v: &TplValue, autoescape: bool) -> String {
    match v {
        TplValue::Safe(s) => s.clone(),
        TplValue::Str(s) => {
            if autoescape {
                escape(s)
            } else {
                s.clone()
            }
        }
        other => other.as_str(),
    }
}

pub(crate) fn escape(s: &str) -> String {
    super::escape_html(s)
}
