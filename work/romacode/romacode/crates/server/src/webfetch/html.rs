//! Tiny forgiving HTML parser + extractors for `web_fetch` (std only).
//!
//! Ported in spirit from webfetch-tui's modes: readable text (Read), Markdown
//! (Md), CSS selection (Scrape) and link listing — without any dependency, so
//! it builds anywhere RomaCode builds. It is not a spec-complete HTML5 parser;
//! it is bounded (node and depth caps) and never panics on malformed input.
//! All truncation is by `char`, never by byte index (the UTF-8 panic class the
//! webfetch-tui 1.0.0 patch fixed).
use super::url::{join, Url};

const MAX_NODES: usize = 200_000;
const MAX_DEPTH: usize = 256;

#[derive(Debug)]
pub struct Node {
    pub name: String,
    pub attrs: Vec<(String, String)>,
    pub children: Vec<Child>,
    pub parent: Option<usize>,
}

#[derive(Debug)]
pub enum Child {
    Node(usize),
    Text(String),
}

#[derive(Debug)]
pub struct Dom {
    /// `nodes[0]` is the synthetic `#root`.
    pub nodes: Vec<Node>,
}

const VOID: &[&str] = &[
    "area", "base", "br", "col", "embed", "hr", "img", "input", "link", "meta", "param", "source",
    "track", "wbr",
];
const BLOCK_STARTS_P: &[&str] = &[
    "address",
    "article",
    "aside",
    "blockquote",
    "div",
    "dl",
    "fieldset",
    "figure",
    "footer",
    "form",
    "h1",
    "h2",
    "h3",
    "h4",
    "h5",
    "h6",
    "header",
    "hr",
    "main",
    "nav",
    "ol",
    "p",
    "pre",
    "section",
    "table",
    "ul",
];

fn is_void(n: &str) -> bool {
    VOID.contains(&n)
}

/// Truncate to at most `n` chars, appending `…` when something was cut.
pub fn truncate_chars(s: &str, n: usize) -> String {
    match s.char_indices().nth(n) {
        Some((i, _)) => format!("{}…", &s[..i]),
        None => s.to_string(),
    }
}

pub fn decode_entities(s: &str) -> String {
    if !s.contains('&') {
        return s.to_string();
    }
    let mut out = String::with_capacity(s.len());
    let mut rest = s;
    while let Some(i) = rest.find('&') {
        out.push_str(&rest[..i]);
        rest = &rest[i..];
        // an entity is at most ~10 chars; look for ';' within a short window
        let window_end = rest.char_indices().nth(12).map_or(rest.len(), |(i, _)| i);
        match rest[..window_end].find(';') {
            Some(semi) => {
                let name = &rest[1..semi];
                match entity(name) {
                    Some(ch) => {
                        out.push_str(&ch);
                        rest = &rest[semi + 1..];
                    }
                    None => {
                        out.push('&');
                        rest = &rest[1..];
                    }
                }
            }
            None => {
                out.push('&');
                rest = &rest[1..];
            }
        }
    }
    out.push_str(rest);
    out
}

fn entity(name: &str) -> Option<String> {
    if let Some(num) = name.strip_prefix('#') {
        let code = match num.strip_prefix(['x', 'X']) {
            Some(h) => u32::from_str_radix(h, 16).ok()?,
            None => num.parse::<u32>().ok()?,
        };
        return char::from_u32(code)
            .filter(|c| *c != '\0')
            .map(|c| c.to_string());
    }
    let s = match name {
        "amp" => "&",
        "lt" => "<",
        "gt" => ">",
        "quot" => "\"",
        "apos" => "'",
        "nbsp" => " ",
        "ndash" => "–",
        "mdash" => "—",
        "hellip" => "…",
        "lsquo" => "‘",
        "rsquo" => "’",
        "ldquo" => "“",
        "rdquo" => "”",
        "laquo" => "«",
        "raquo" => "»",
        "copy" => "©",
        "reg" => "®",
        "trade" => "™",
        "middot" => "·",
        "bull" => "•",
        "times" => "×",
        "euro" => "€",
        "pound" => "£",
        "yen" => "¥",
        "deg" => "°",
        "larr" => "←",
        "rarr" => "→",
        _ => return None,
    };
    Some(s.to_string())
}

impl Dom {
    pub fn parse(html: &str) -> Dom {
        let mut dom = Dom {
            nodes: vec![Node {
                name: "#root".into(),
                attrs: vec![],
                children: vec![],
                parent: None,
            }],
        };
        let mut stack: Vec<usize> = vec![0];
        let b = html.as_bytes();
        let mut i = 0;
        let mut text_start = 0;

        macro_rules! flush_text {
            ($end:expr) => {{
                if $end > text_start {
                    let t = &html[text_start..$end];
                    if !t.is_empty() {
                        let top = *stack.last().unwrap();
                        dom.nodes[top]
                            .children
                            .push(Child::Text(decode_entities(t)));
                    }
                }
            }};
        }

        while i < b.len() {
            if b[i] != b'<' {
                i += 1;
                continue;
            }
            let rest = &html[i..];
            if rest.starts_with("<!--") {
                flush_text!(i);
                i = match rest.find("-->") {
                    Some(e) => i + e + 3,
                    None => b.len(),
                };
                text_start = i;
                continue;
            }
            if rest.starts_with("<!") || rest.starts_with("<?") {
                flush_text!(i);
                i = match rest.find('>') {
                    Some(e) => i + e + 1,
                    None => b.len(),
                };
                text_start = i;
                continue;
            }
            if let Some(after) = rest.strip_prefix("</") {
                if after.starts_with(|c: char| c.is_ascii_alphabetic()) {
                    flush_text!(i);
                    let end = rest.find('>').map_or(b.len(), |e| i + e + 1);
                    let name: String = after
                        .chars()
                        .take_while(|c| !c.is_whitespace() && *c != '>' && *c != '/')
                        .collect::<String>()
                        .to_ascii_lowercase();
                    if let Some(pos) = stack.iter().rposition(|&n| dom.nodes[n].name == name) {
                        if pos > 0 {
                            stack.truncate(pos);
                        }
                    }
                    i = end;
                    text_start = i;
                    continue;
                }
            }
            if rest[1..].starts_with(|c: char| c.is_ascii_alphabetic()) {
                flush_text!(i);
                let (name, attrs, self_closing, end) = parse_open_tag(html, i);
                i = end;
                text_start = i;
                // implied end tags
                implied_close(&dom, &mut stack, &name);
                let top = *stack.last().unwrap();
                if dom.nodes.len() < MAX_NODES {
                    let idx = dom.nodes.len();
                    dom.nodes.push(Node {
                        name: name.clone(),
                        attrs,
                        children: vec![],
                        parent: Some(top),
                    });
                    dom.nodes[top].children.push(Child::Node(idx));
                    let raw_text =
                        matches!(name.as_str(), "script" | "style" | "textarea" | "title");
                    if raw_text && !self_closing {
                        // contents are not markup: find the matching close tag.
                        // Case-insensitive search WITHOUT lowercasing the rest
                        // of the document — every script/style/title/textarea
                        // open tag used to allocate a lowercased copy of the
                        // entire remaining page (~500 MB of churn on a 1 MB
                        // page with scattered scripts).
                        let close = format!("</{name}").into_bytes();
                        let close = match find_ignore_case(&b[i..], &close) {
                            Some(p) => {
                                let gt = b[i + p..]
                                    .iter()
                                    .position(|&c| c == b'>')
                                    .map_or(b.len() - i, |g| p + g + 1);
                                (i + p, i + gt)
                            }
                            None => (b.len(), b.len()),
                        };
                        let (body_end, resume) = close;
                        if name == "title" || name == "textarea" {
                            let t = &html[i..body_end];
                            if !t.is_empty() {
                                dom.nodes[idx]
                                    .children
                                    .push(Child::Text(decode_entities(t)));
                            }
                        }
                        i = resume;
                        text_start = i;
                    } else if !is_void(&name) && !self_closing && stack.len() < MAX_DEPTH {
                        stack.push(idx);
                    }
                }
                continue;
            }
            i += 1; // a lone '<' is just text
        }
        flush_text!(b.len());
        dom
    }

    pub fn attr(&self, n: usize, name: &str) -> Option<&str> {
        self.nodes[n]
            .attrs
            .iter()
            .find(|(k, _)| k == name)
            .map(|(_, v)| v.as_str())
    }

    /// Whitespace-collapsed text of a subtree (scripts/styles excluded by construction).
    pub fn text(&self, n: usize) -> String {
        let mut raw = String::new();
        self.collect_text(n, &mut raw, 0);
        collapse_ws(&raw)
    }

    fn collect_text(&self, n: usize, out: &mut String, depth: usize) {
        if depth > MAX_DEPTH + 8 {
            return;
        }
        for c in &self.nodes[n].children {
            match c {
                Child::Text(t) => {
                    out.push_str(t);
                    out.push(' ');
                }
                Child::Node(i) => {
                    if matches!(
                        self.nodes[*i].name.as_str(),
                        "script" | "style" | "noscript" | "template"
                    ) {
                        continue;
                    }
                    self.collect_text(*i, out, depth + 1);
                }
            }
        }
    }

    pub fn find_first(&self, name: &str) -> Option<usize> {
        (1..self.nodes.len()).find(|&i| self.nodes[i].name == name)
    }

    pub fn title(&self) -> Option<String> {
        self.find_first("title")
            .map(|t| self.text(t))
            .filter(|t| !t.is_empty())
    }
}

fn collapse_ws(s: &str) -> String {
    s.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// Case-insensitive substring search over ASCII (HTML tag names are ASCII).
/// Allocates nothing — the lowercased-remainder scan this replaces used to
/// allocate a full copy of the document tail for every raw-text open tag.
fn find_ignore_case(hay: &[u8], needle: &[u8]) -> Option<usize> {
    if needle.is_empty() || hay.len() < needle.len() {
        return None;
    }
    (0..=hay.len() - needle.len()).find(|&i| hay[i..i + needle.len()].eq_ignore_ascii_case(needle))
}

/// Parse `<name attr=...>` starting at `start` (which points at `<`).
/// Returns (lowercased name, attrs, self_closing, index just past `>`).
fn parse_open_tag(html: &str, start: usize) -> (String, Vec<(String, String)>, bool, usize) {
    let b = html.as_bytes();
    let mut i = start + 1;
    let name_start = i;
    while i < b.len() && !b[i].is_ascii_whitespace() && b[i] != b'>' && b[i] != b'/' {
        i += 1;
    }
    let name = html[name_start..i].to_ascii_lowercase();
    let mut attrs = Vec::new();
    let mut self_closing = false;
    loop {
        while i < b.len() && (b[i].is_ascii_whitespace() || b[i] == b'/') {
            if b[i] == b'/' {
                self_closing = true;
            }
            i += 1;
        }
        if i >= b.len() {
            break;
        }
        if b[i] == b'>' {
            i += 1;
            break;
        }
        self_closing = false;
        let ks = i;
        while i < b.len()
            && !b[i].is_ascii_whitespace()
            && b[i] != b'='
            && b[i] != b'>'
            && b[i] != b'/'
        {
            i += 1;
        }
        let key = html[ks..i].to_ascii_lowercase();
        while i < b.len() && b[i].is_ascii_whitespace() {
            i += 1;
        }
        let mut value = String::new();
        if i < b.len() && b[i] == b'=' {
            i += 1;
            while i < b.len() && b[i].is_ascii_whitespace() {
                i += 1;
            }
            if i < b.len() && (b[i] == b'"' || b[i] == b'\'') {
                let q = b[i];
                i += 1;
                let vs = i;
                while i < b.len() && b[i] != q {
                    i += 1;
                }
                value = decode_entities(&html[vs..i]);
                i = (i + 1).min(b.len());
            } else {
                let vs = i;
                while i < b.len() && !b[i].is_ascii_whitespace() && b[i] != b'>' {
                    i += 1;
                }
                value = decode_entities(&html[vs..i]);
            }
        }
        if !key.is_empty() && attrs.len() < 64 {
            attrs.push((key, value));
        }
    }
    (name, attrs, self_closing, i)
}

fn implied_close(dom: &Dom, stack: &mut Vec<usize>, opening: &str) {
    let top_name = |s: &Vec<usize>| dom.nodes[*s.last().unwrap()].name.clone();
    if BLOCK_STARTS_P.contains(&opening) && top_name(stack) == "p" {
        stack.pop();
    }
    match opening {
        "li" => {
            // close an open <li> in the current list only
            if let Some(pos) = stack
                .iter()
                .rposition(|&n| matches!(dom.nodes[n].name.as_str(), "li" | "ul" | "ol"))
            {
                if dom.nodes[stack[pos]].name == "li" {
                    stack.truncate(pos);
                }
            }
        }
        "td" | "th" => {
            if let Some(pos) = stack
                .iter()
                .rposition(|&n| matches!(dom.nodes[n].name.as_str(), "td" | "th" | "tr" | "table"))
            {
                if matches!(dom.nodes[stack[pos]].name.as_str(), "td" | "th") {
                    stack.truncate(pos);
                }
            }
        }
        "tr" => {
            if let Some(pos) = stack
                .iter()
                .rposition(|&n| matches!(dom.nodes[n].name.as_str(), "tr" | "table"))
            {
                if dom.nodes[stack[pos]].name == "tr" {
                    stack.truncate(pos);
                }
            }
        }
        "option" => {
            if top_name(stack) == "option" {
                stack.pop();
            }
        }
        _ => {}
    }
}

// ── readable text / markdown ─────────────────────────────────────────────────

const SKIP: &[&str] = &[
    "head", "script", "style", "noscript", "template", "svg", "canvas", "iframe", "object",
    "embed", "nav", "footer", "aside", "select", "button", "dialog",
];
const BLOCK: &[&str] = &[
    "address",
    "article",
    "aside",
    "blockquote",
    "details",
    "div",
    "dl",
    "dt",
    "dd",
    "fieldset",
    "figcaption",
    "figure",
    "footer",
    "form",
    "h1",
    "h2",
    "h3",
    "h4",
    "h5",
    "h6",
    "header",
    "main",
    "nav",
    "ol",
    "p",
    "section",
    "summary",
    "table",
    "ul",
];

struct Renderer<'a> {
    dom: &'a Dom,
    base: &'a Url,
    md: bool,
    out: String,
    pending_space: bool,
    list: Vec<Option<usize>>, // None = bullet, Some(n) = next ordered number
}

/// Choose the main content root: `<main>`, else a lone `<article>`, else `<body>`.
fn content_root(dom: &Dom) -> usize {
    if let Some(m) = dom.find_first("main") {
        return m;
    }
    let articles: Vec<usize> = (1..dom.nodes.len())
        .filter(|&i| dom.nodes[i].name == "article")
        .collect();
    if articles.len() == 1 {
        return articles[0];
    }
    dom.find_first("body").unwrap_or(0)
}

/// Render the page's main content as plain text (`md = false`) or Markdown.
pub fn render(dom: &Dom, base: &Url, md: bool) -> String {
    let root = content_root(dom);
    let mut r = Renderer {
        dom,
        base,
        md,
        out: String::new(),
        pending_space: false,
        list: vec![],
    };
    // the chosen root itself is never skipped, only its boilerplate descendants
    r.children(root, 0);
    tidy(&r.out)
}

fn tidy(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut blank = 0;
    for line in s.lines() {
        let l = line.trim_end();
        if l.is_empty() {
            blank += 1;
            if blank <= 1 && !out.is_empty() {
                out.push('\n');
            }
        } else {
            blank = 0;
            out.push_str(l);
            out.push('\n');
        }
    }
    out.trim().to_string()
}

impl Renderer<'_> {
    fn ensure_newline(&mut self) {
        self.pending_space = false;
        if !self.out.is_empty() && !self.out.ends_with('\n') {
            self.out.push('\n');
        }
    }

    fn ensure_blank(&mut self) {
        self.ensure_newline();
        if !self.out.is_empty() && !self.out.ends_with("\n\n") {
            self.out.push('\n');
        }
    }

    fn text(&mut self, t: &str) {
        for ch in t.chars() {
            if ch.is_whitespace() {
                self.pending_space = true;
            } else {
                if self.pending_space && !self.out.is_empty() && !self.out.ends_with('\n') {
                    self.out.push(' ');
                }
                self.pending_space = false;
                self.out.push(ch);
            }
        }
    }

    fn children(&mut self, n: usize, depth: usize) {
        if depth > MAX_DEPTH + 8 {
            return;
        }
        let kids: Vec<&Child> = self.dom.nodes[n].children.iter().collect();
        for c in kids {
            match c {
                Child::Text(t) => self.text(t),
                Child::Node(i) => self.node(*i, depth + 1),
            }
        }
    }

    fn wrap_inline(&mut self, n: usize, depth: usize, mark: &str) {
        let start = self.out.len();
        self.children(n, depth);
        let inner = self.out.split_off(start);
        if inner.trim().is_empty() {
            self.out.push_str(&inner);
        } else {
            let lead = inner.len() - inner.trim_start().len();
            self.out.push_str(&inner[..lead]);
            self.out.push_str(mark);
            self.out.push_str(inner.trim());
            self.out.push_str(mark);
        }
    }

    fn node(&mut self, n: usize, depth: usize) {
        let name = self.dom.nodes[n].name.clone();
        if SKIP.contains(&name.as_str()) {
            return;
        }
        match name.as_str() {
            "br" => {
                self.pending_space = false;
                self.out.push('\n');
            }
            "hr" => {
                self.ensure_blank();
                self.out.push_str(if self.md { "---" } else { "----" });
                self.ensure_blank();
            }
            "h1" | "h2" | "h3" | "h4" | "h5" | "h6" => {
                self.ensure_blank();
                if self.md {
                    let level = name.as_bytes()[1] - b'0';
                    self.out.push_str(&"#".repeat(level as usize));
                    self.out.push(' ');
                }
                self.children(n, depth);
                self.ensure_blank();
            }
            "pre" => {
                self.ensure_blank();
                let mut code = String::new();
                self.dom.collect_pre(n, &mut code, 0);
                if self.md {
                    self.out.push_str("```\n");
                    self.out.push_str(code.trim_matches('\n'));
                    self.out.push_str("\n```");
                } else {
                    self.out.push_str(code.trim_matches('\n'));
                }
                self.ensure_blank();
            }
            "ul" | "ol" => {
                self.ensure_newline();
                self.list.push(if name == "ol" { Some(1) } else { None });
                self.children(n, depth);
                self.list.pop();
                self.ensure_newline();
                if self.list.is_empty() {
                    self.ensure_blank();
                }
            }
            "li" => {
                self.ensure_newline();
                let indent = "  ".repeat(self.list.len().saturating_sub(1));
                let marker = match self.list.last_mut() {
                    Some(Some(k)) => {
                        let m = format!("{k}. ");
                        *k += 1;
                        m
                    }
                    _ => "- ".to_string(),
                };
                self.out.push_str(&indent);
                self.out.push_str(&marker);
                self.children(n, depth);
                self.ensure_newline();
            }
            "blockquote" => {
                self.ensure_blank();
                let start = self.out.len();
                self.children(n, depth);
                self.ensure_blank();
                let inner = self.out.split_off(start);
                for line in inner.trim_end().lines() {
                    self.out.push_str(if self.md { "> " } else { "  " });
                    self.out.push_str(line);
                    self.out.push('\n');
                }
                self.ensure_blank();
            }
            "table" => {
                self.ensure_blank();
                self.children(n, depth);
                self.ensure_blank();
            }
            "tr" => {
                self.ensure_newline();
                let cells: Vec<usize> = self.dom.nodes[n]
                    .children
                    .iter()
                    .filter_map(|c| match c {
                        Child::Node(i)
                            if matches!(self.dom.nodes[*i].name.as_str(), "td" | "th") =>
                        {
                            Some(*i)
                        }
                        _ => None,
                    })
                    .collect();
                if cells.is_empty() {
                    self.children(n, depth);
                } else {
                    for (k, cell) in cells.into_iter().enumerate() {
                        if k > 0 {
                            self.out.push_str(" | ");
                        }
                        self.pending_space = false;
                        self.children(cell, depth + 1);
                    }
                }
                self.ensure_newline();
            }
            "a" => {
                let href = self
                    .dom
                    .attr(n, "href")
                    .and_then(|h| join(self.base, h))
                    .map(|u| u.to_string());
                match (self.md, href) {
                    (true, Some(h)) => {
                        let start = self.out.len();
                        self.children(n, depth);
                        let inner = self.out.split_off(start);
                        let label = inner.trim();
                        if label.is_empty() {
                            self.out.push_str(&inner);
                        } else {
                            let lead = inner.len() - inner.trim_start().len();
                            self.out.push_str(&inner[..lead]);
                            self.out.push_str(&format!("[{label}]({h})"));
                        }
                    }
                    _ => self.children(n, depth),
                }
            }
            "img" => {
                let alt = self.dom.attr(n, "alt").map(collapse_ws).unwrap_or_default();
                if self.md && !alt.is_empty() {
                    if let Some(src) = self.dom.attr(n, "src").and_then(|s| join(self.base, s)) {
                        self.pending_space = true;
                        self.text(&format!("![{alt}]({src})"));
                    }
                } else if !alt.is_empty() {
                    self.text(&format!("[{alt}]"));
                }
            }
            "strong" | "b" if self.md => self.wrap_inline(n, depth, "**"),
            "em" | "i" if self.md => self.wrap_inline(n, depth, "*"),
            "code" if self.md => self.wrap_inline(n, depth, "`"),
            _ if BLOCK.contains(&name.as_str()) => {
                self.ensure_blank();
                self.children(n, depth);
                self.ensure_blank();
            }
            _ => self.children(n, depth),
        }
    }
}

impl Dom {
    /// Raw text for `<pre>`: whitespace preserved, `<br>` as newline.
    fn collect_pre(&self, n: usize, out: &mut String, depth: usize) {
        if depth > MAX_DEPTH + 8 {
            return;
        }
        for c in &self.nodes[n].children {
            match c {
                Child::Text(t) => out.push_str(t),
                Child::Node(i) => {
                    if self.nodes[*i].name == "br" {
                        out.push('\n');
                    } else if !matches!(self.nodes[*i].name.as_str(), "script" | "style") {
                        self.collect_pre(*i, out, depth + 1);
                    }
                }
            }
        }
    }
}

// ── links ────────────────────────────────────────────────────────────────────

/// All fetchable links in document order, de-duplicated by URL: `(text, url)`.
pub fn links(dom: &Dom, base: &Url) -> Vec<(String, String)> {
    let mut seen = std::collections::HashSet::new();
    let mut out = Vec::new();
    for i in 1..dom.nodes.len() {
        if dom.nodes[i].name != "a" {
            continue;
        }
        let Some(url) = dom.attr(i, "href").and_then(|h| join(base, h)) else {
            continue;
        };
        let url = url.to_string();
        if seen.insert(url.clone()) {
            let mut text = dom.text(i);
            if text.is_empty() {
                text = dom
                    .attr(i, "title")
                    .or_else(|| dom.attr(i, "aria-label"))
                    .unwrap_or("")
                    .to_string();
            }
            out.push((truncate_chars(&text, 80), url));
        }
    }
    out
}

// ── CSS selectors (subset) ───────────────────────────────────────────────────
//
// Supported: `tag`, `*`, `#id`, `.class`, `[attr]`, `[attr=v]`, `[attr*=v]`,
// `[attr^=v]`, `[attr$=v]`, `[attr~=v]`, compounds (`a.btn#x[href]`), the
// descendant (` `) and child (`>`) combinators, and `,` lists.
// Pseudo-classes / `+` / `~` are rejected with a clear error.

#[derive(Debug, Clone, Copy, PartialEq)]
enum Comb {
    Descendant,
    Child,
}

#[derive(Debug, Default)]
struct Compound {
    tag: Option<String>,
    id: Option<String>,
    classes: Vec<String>,
    attrs: Vec<(String, Option<(char, String)>)>,
}

#[derive(Debug)]
pub struct Selector {
    /// Each part carries its combinator relative to the previous part.
    parts: Vec<(Comb, Compound)>,
}

fn ident(chars: &[char], i: &mut usize) -> String {
    let s = *i;
    while *i < chars.len() && (chars[*i].is_alphanumeric() || chars[*i] == '-' || chars[*i] == '_')
    {
        *i += 1;
    }
    chars[s..*i].iter().collect()
}

pub fn parse_selector(src: &str) -> Result<Vec<Selector>, String> {
    if src.trim().is_empty() {
        return Err("empty selector".into());
    }
    if src.chars().count() > 500 {
        return Err("selector too long".into());
    }
    let chars: Vec<char> = src.chars().collect();
    let mut i = 0;
    let mut list = Vec::new();
    let mut parts: Vec<(Comb, Compound)> = Vec::new();
    let mut comb = Comb::Descendant;
    loop {
        while i < chars.len() && chars[i].is_whitespace() {
            i += 1;
            if !parts.is_empty() && comb == Comb::Descendant {
                // whitespace = descendant unless a combinator follows
            }
        }
        if i >= chars.len() {
            break;
        }
        match chars[i] {
            ',' => {
                i += 1;
                if parts.is_empty() {
                    return Err("empty selector before ','".into());
                }
                list.push(Selector {
                    parts: std::mem::take(&mut parts),
                });
                comb = Comb::Descendant;
                continue;
            }
            '>' => {
                i += 1;
                if parts.is_empty() {
                    return Err("selector cannot start with '>'".into());
                }
                comb = Comb::Child;
                continue;
            }
            '+' | '~' => return Err(format!("the '{}' combinator is not supported", chars[i])),
            _ => {}
        }
        let mut c = Compound::default();
        let mut any = false;
        if chars[i] == '*' {
            i += 1;
            any = true;
        } else if chars[i].is_alphanumeric() {
            c.tag = Some(ident(&chars, &mut i).to_ascii_lowercase());
            any = true;
        }
        loop {
            if i >= chars.len() {
                break;
            }
            match chars[i] {
                '#' => {
                    i += 1;
                    let id = ident(&chars, &mut i);
                    if id.is_empty() {
                        return Err("expected an id after '#'".into());
                    }
                    c.id = Some(id);
                    any = true;
                }
                '.' => {
                    i += 1;
                    let cl = ident(&chars, &mut i);
                    if cl.is_empty() {
                        return Err("expected a class after '.'".into());
                    }
                    c.classes.push(cl);
                    any = true;
                }
                '[' => {
                    i += 1;
                    while i < chars.len() && chars[i].is_whitespace() {
                        i += 1;
                    }
                    let name = ident(&chars, &mut i).to_ascii_lowercase();
                    if name.is_empty() {
                        return Err("expected an attribute name after '['".into());
                    }
                    while i < chars.len() && chars[i].is_whitespace() {
                        i += 1;
                    }
                    let mut test = None;
                    if i < chars.len() && chars[i] != ']' {
                        let op = if chars[i] == '=' {
                            i += 1;
                            '='
                        } else if matches!(chars[i], '*' | '^' | '$' | '~')
                            && chars.get(i + 1) == Some(&'=')
                        {
                            let o = chars[i];
                            i += 2;
                            o
                        } else {
                            return Err(format!(
                                "unsupported attribute operator near {:?}",
                                chars[i]
                            ));
                        };
                        while i < chars.len() && chars[i].is_whitespace() {
                            i += 1;
                        }
                        let val: String = if i < chars.len()
                            && (chars[i] == '"' || chars[i] == '\'')
                        {
                            let q = chars[i];
                            i += 1;
                            let s = i;
                            while i < chars.len() && chars[i] != q {
                                i += 1;
                            }
                            if i >= chars.len() {
                                return Err("unterminated quote in selector".into());
                            }
                            let v = chars[s..i].iter().collect();
                            i += 1;
                            v
                        } else {
                            let s = i;
                            while i < chars.len() && chars[i] != ']' && !chars[i].is_whitespace() {
                                i += 1;
                            }
                            chars[s..i].iter().collect()
                        };
                        test = Some((op, val));
                        while i < chars.len() && chars[i].is_whitespace() {
                            i += 1;
                        }
                    }
                    if i >= chars.len() || chars[i] != ']' {
                        return Err("missing ']' in selector".into());
                    }
                    i += 1;
                    c.attrs.push((name, test));
                    any = true;
                }
                ':' => return Err("pseudo-classes (':…') are not supported".into()),
                _ => break,
            }
        }
        if !any {
            return Err(format!("unexpected {:?} in selector", chars[i]));
        }
        parts.push((comb, c));
        comb = Comb::Descendant;
        if parts.len() > 16 {
            return Err("selector has too many parts".into());
        }
    }
    if parts.is_empty() {
        return Err("selector is incomplete".into());
    }
    if comb == Comb::Child {
        return Err("selector ends with '>'".into());
    }
    list.push(Selector { parts });
    Ok(list)
}

fn compound_matches(dom: &Dom, n: usize, c: &Compound) -> bool {
    let node = &dom.nodes[n];
    if node.name.starts_with('#') {
        return false;
    }
    if let Some(t) = &c.tag {
        if &node.name != t {
            return false;
        }
    }
    if let Some(id) = &c.id {
        if dom.attr(n, "id") != Some(id.as_str()) {
            return false;
        }
    }
    if !c.classes.is_empty() {
        let have = dom.attr(n, "class").unwrap_or("");
        if !c
            .classes
            .iter()
            .all(|w| have.split_whitespace().any(|h| h == w))
        {
            return false;
        }
    }
    for (name, test) in &c.attrs {
        let Some(v) = dom.attr(n, name) else {
            return false;
        };
        if let Some((op, want)) = test {
            let ok = match op {
                '=' => v == want,
                '*' => v.contains(want.as_str()),
                '^' => v.starts_with(want.as_str()),
                '$' => v.ends_with(want.as_str()),
                '~' => v.split_whitespace().any(|w| w == want),
                _ => false,
            };
            if !ok {
                return false;
            }
        }
    }
    true
}

fn matches(dom: &Dom, n: usize, parts: &[(Comb, Compound)]) -> bool {
    let Some(((comb, last), rest)) = parts.split_last().map(|(l, r)| ((l.0, &l.1), r)) else {
        return true;
    };
    if !compound_matches(dom, n, last) {
        return false;
    }
    if rest.is_empty() {
        return true;
    }
    match comb {
        Comb::Child => dom.nodes[n].parent.map_or(false, |p| matches(dom, p, rest)),
        Comb::Descendant => {
            let mut cur = dom.nodes[n].parent;
            while let Some(p) = cur {
                if matches(dom, p, rest) {
                    return true;
                }
                cur = dom.nodes[p].parent;
            }
            false
        }
    }
}

/// Matching element indices in document order.
pub fn select(dom: &Dom, sels: &[Selector]) -> Vec<usize> {
    // node indices are allocated in document order, so a linear scan is ordered
    (1..dom.nodes.len())
        .filter(|&n| sels.iter().any(|s| matches(dom, n, &s.parts)))
        .collect()
}

/// One-line description of an element: `<a id="x" class="y" href="…">`.
pub fn describe(dom: &Dom, n: usize) -> String {
    let node = &dom.nodes[n];
    let mut s = format!("<{}", node.name);
    for key in ["id", "class", "href", "src", "name", "type"] {
        if let Some(v) = dom.attr(n, key) {
            s.push_str(&format!(" {key}=\"{}\"", truncate_chars(v, 60)));
        }
    }
    s.push('>');
    s
}

#[cfg(test)]
mod tests {
    use super::*;

    fn base() -> Url {
        Url::parse("https://example.com/docs/page.html").unwrap()
    }

    const PAGE: &str = r##"<!DOCTYPE html><html><head><title>Hello &amp; welcome</title>
<style>body{color:red}</style><script>var x = "<b>not markup</b>";</script></head>
<body><nav><a href="/nav">Navigation</a></nav>
<main><h1>Main  Title</h1>
<p>First <b>bold</b> and <em>soft</em> text with <a href="../other?a=1&amp;b=2">a link</a>.
<p>Second paragraph &mdash; with entity &#169; &#x41;.
<ul><li>one<li>two <a href="#frag">frag</a><li>three</ul>
<ol><li>alpha</li><li>beta</li></ol>
<pre><code>fn main() {
    println!("hi");
}</code></pre>
<table><tr><th>Name<th>Age<tr><td>Ann<td>30</table>
<blockquote>quoted</blockquote>
<img src="/img/a.png" alt="A picture">
<a class="btn big" id="go" href="https://other.org/x">Go</a>
</main><footer>footer junk</footer></body></html>"##;

    #[test]
    fn markdown_rendering() {
        let dom = Dom::parse(PAGE);
        assert_eq!(dom.title().as_deref(), Some("Hello & welcome"));
        let md = render(&dom, &base(), true);
        assert!(md.starts_with("# Main Title"), "{md}");
        assert!(
            md.contains(
                "First **bold** and *soft* text with [a link](https://example.com/other?a=1&b=2)."
            ),
            "{md}"
        );
        assert!(md.contains("Second paragraph — with entity © A."), "{md}");
        assert!(md.contains("- one\n- two frag\n- three"), "{md}");
        assert!(md.contains("1. alpha\n2. beta"), "{md}");
        assert!(
            md.contains("```\nfn main() {\n    println!(\"hi\");\n}\n```"),
            "{md}"
        );
        assert!(md.contains("Name | Age\nAnn | 30"), "{md}");
        assert!(md.contains("> quoted"), "{md}");
        assert!(
            md.contains("![A picture](https://example.com/img/a.png)"),
            "{md}"
        );
        assert!(
            !md.contains("footer junk") && !md.contains("Navigation") && !md.contains("not markup"),
            "{md}"
        );
    }

    #[test]
    fn text_rendering_has_no_markup() {
        let dom = Dom::parse(PAGE);
        let t = render(&dom, &base(), false);
        assert!(t.starts_with("Main Title"), "{t}");
        assert!(t.contains("First bold and soft text with a link."), "{t}");
        assert!(
            !t.contains("**") && !t.contains("](") && !t.contains("```"),
            "{t}"
        );
    }

    #[test]
    fn links_are_resolved_and_deduped() {
        let dom = Dom::parse(PAGE);
        let l = links(&dom, &base());
        let urls: Vec<&str> = l.iter().map(|(_, u)| u.as_str()).collect();
        assert_eq!(
            urls,
            [
                "https://example.com/nav",
                "https://example.com/other?a=1&b=2",
                "https://other.org/x"
            ]
        );
        assert_eq!(l[1].0, "a link");
    }

    #[test]
    fn selectors() {
        let dom = Dom::parse(PAGE);
        let q = |s: &str| select(&dom, &parse_selector(s).unwrap()).len();
        assert_eq!(q("a"), 4);
        assert_eq!(q("a.btn"), 1);
        assert_eq!(q("a.big.btn#go"), 1);
        assert_eq!(q("a[href^='https://']"), 1);
        assert_eq!(q("a[href*=other]"), 2);
        assert_eq!(q("main > p"), 2);
        assert_eq!(q("main a"), 3);
        assert_eq!(q("ul li"), 3);
        assert_eq!(q("ul > li"), 3);
        assert_eq!(q("body > li"), 0);
        assert_eq!(q("h1, blockquote"), 2);
        assert_eq!(q("[alt]"), 1);
        assert_eq!(q("nosuch"), 0);
        let m = select(&dom, &parse_selector("#go").unwrap());
        assert_eq!(
            describe(&dom, m[0]),
            "<a id=\"go\" class=\"btn big\" href=\"https://other.org/x\">"
        );
        for bad in [
            "", "a:hover", "a + b", "> a", "a >", "[x", "a[", "a,", ".", "#",
        ] {
            assert!(parse_selector(bad).is_err(), "should reject {bad:?}");
        }
    }

    #[test]
    fn utf8_safe_truncation() {
        // the byte-slice panic class fixed by the webfetch-tui 1.0.0 patch
        let s = "日本語のテキスト😀".repeat(40);
        let t = truncate_chars(&s, 61);
        assert_eq!(t.chars().count(), 62); // 61 chars + ellipsis
        assert_eq!(truncate_chars("short", 61), "short");
        assert_eq!(truncate_chars("", 0), "");
    }

    #[test]
    fn malformed_html_never_panics() {
        let nasty = [
            "<",
            "<<<<",
            "<a",
            "<a href",
            "<a href=",
            "<a href='x",
            "<div",
            "</",
            "</>",
            "<!--",
            "<!-- x",
            "<script>",
            "<script>alert(1)",
            "<title>x",
            "&",
            "&#",
            "&#xZZ;",
            "&#99999999999;",
            "&amp",
            "<p>\u{0}</p>",
            "<a/b/c>",
            "<img src=x alt=\"é",
            "<a href=\"é\u{1f600}\">日本</a",
        ];
        for h in nasty {
            let dom = Dom::parse(h);
            let _ = render(&dom, &base(), true);
            let _ = render(&dom, &base(), false);
            let _ = links(&dom, &base());
        }
        // pathological nesting is capped instead of overflowing the stack
        let deep = "<div>".repeat(50_000) + "x";
        let dom = Dom::parse(&deep);
        assert!(render(&dom, &base(), true).contains('x'));
        let wide = "<p>a".repeat(20_000);
        let _ = render(&Dom::parse(&wide), &base(), false);
    }

    #[test]
    fn entities() {
        assert_eq!(
            decode_entities("a &lt; b &amp;&amp; c &gt; d &nbsp;e"),
            "a < b && c > d  e"
        );
        assert_eq!(decode_entities("&unknown; &#65;&#x42; &"), "&unknown; AB &");
        assert_eq!(decode_entities("日本&amp;語"), "日本&語");
    }
}
