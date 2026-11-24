//! Minimal pull-style XML parser + escape helpers for OOXML.
//!
//! Handles what Word/Excel/PowerPoint writers actually emit: nested elements
//! with attributes, text, CDATA, comments, declarations and self-closing
//! tags. Namespace prefixes are carried through verbatim (`w:p`, `a:t`, …)
//! — callers match on prefixed names exactly like the OOXML specs' examples.

#[derive(Debug, Clone, PartialEq)]
pub enum Event {
    /// `<w:p w:rsidR="00A">` — name, attributes (name → value).
    Start(String, Vec<(String, String)>),
    /// `<w:br/>`
    Empty(String, Vec<(String, String)>),
    /// `</w:p>`
    End(String),
    /// Character data (already unescaped; CDATA merged).
    Text(String),
}

pub struct Parser<'a> {
    s: &'a [u8],
    pos: usize,
}

impl<'a> Parser<'a> {
    pub fn new(s: &'a str) -> Self {
        Self { s: s.as_bytes(), pos: 0 }
    }

    /// Next meaningful event; `None` at end of document.
    pub fn next(&mut self) -> Option<Event> {
        loop {
            if self.pos >= self.s.len() {
                return None;
            }
            if self.s[self.pos] == b'<' {
                // markup
                let rest = &self.s[self.pos..];
                if rest.starts_with(b"<!--") {
                    match find(self.s, self.pos + 4, b"-->") {
                        Some(p) => {
                            self.pos = p + 3;
                            continue;
                        }
                        None => return None,
                    }
                }
                if rest.starts_with(b"<![CDATA[") {
                    let end = find(self.s, self.pos + 9, b"]]>")?;
                    let text = String::from_utf8_lossy(&self.s[self.pos + 9..end]).into_owned();
                    self.pos = end + 3;
                    return Some(Event::Text(text));
                }
                if rest.starts_with(b"<?") || rest.starts_with(b"<!") {
                    match find(self.s, self.pos + 1, b">") {
                        Some(p) => {
                            self.pos = p + 1;
                            continue;
                        }
                        None => return None,
                    }
                }
                // element
                let end = find(self.s, self.pos + 1, b">")?;
                let inner = &self.s[self.pos + 1..end];
                self.pos = end + 1;
                let inner_s = String::from_utf8_lossy(inner);
                if inner_s.starts_with('/') {
                    return Some(Event::End(inner_s[1..].trim().to_string()));
                }
                let self_closing = inner_s.ends_with('/');
                let body = if self_closing { &inner_s[..inner_s.len() - 1] } else { &inner_s };
                let (name, attrs) = parse_tag(body);
                return Some(if self_closing {
                    Event::Empty(name, attrs)
                } else {
                    Event::Start(name, attrs)
                });
            }
            // text run until the next '<'
            let start = self.pos;
            while self.pos < self.s.len() && self.s[self.pos] != b'<' {
                self.pos += 1;
            }
            let text = String::from_utf8_lossy(&self.s[start..self.pos]).into_owned();
            let unescaped = unescape(&text);
            if !unescaped.is_empty() {
                return Some(Event::Text(unescaped));
            }
        }
    }
}

fn find(s: &[u8], from: usize, pat: &[u8]) -> Option<usize> {
    let mut i = from;
    while i + pat.len() <= s.len() {
        if &s[i..i + pat.len()] == pat {
            return Some(i);
        }
        i += 1;
    }
    None
}

fn parse_tag(body: &str) -> (String, Vec<(String, String)>) {
    let mut name = String::new();
    let mut has_ws = false;
    for c in body.chars() {
        if c.is_whitespace() {
            has_ws = true;
            break;
        }
        name.push(c);
    }
    if !has_ws {
        return (name, Vec::new());
    }
    let mut attrs = Vec::new();
    let rest = &body[name.len()..];
    let b = rest.as_bytes();
    let mut i = 0;
    while i < b.len() {
        while i < b.len() && (b[i] as char).is_whitespace() {
            i += 1;
        }
        // attr name
        let ns = i;
        while i < b.len() && b[i] != b'=' && !(b[i] as char).is_whitespace() {
            i += 1;
        }
        if ns == i {
            break;
        }
        let key = rest[ns..i].to_string();
        while i < b.len() && (b[i] as char).is_whitespace() {
            i += 1;
        }
        if i < b.len() && b[i] == b'=' {
            i += 1;
            while i < b.len() && (b[i] as char).is_whitespace() {
                i += 1;
            }
            let quote = if i < b.len() && (b[i] == b'"' || b[i] == b'\'') { b[i] } else { 0 };
            if quote != 0 {
                i += 1;
                let vs = i;
                while i < b.len() && b[i] != quote {
                    i += 1;
                }
                attrs.push((key, unescape(&rest[vs..i.min(rest.len())])));
                i += 1;
            } else {
                let vs = i;
                while i < b.len() && !(b[i] as char).is_whitespace() {
                    i += 1;
                }
                attrs.push((key, unescape(&rest[vs..i])));
            }
        } else {
            attrs.push((key, String::new()));
        }
    }
    (name, attrs)
}

pub fn escape(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
        match c {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '"' => out.push_str("&quot;"),
            '\'' => out.push_str("&#39;"),
            _ => out.push(c),
        }
    }
    out
}

pub fn unescape(s: &str) -> String {
    if !s.contains('&') {
        return s.to_string();
    }
    let mut out = String::with_capacity(s.len());
    let b = s.as_bytes();
    let mut i = 0;
    while i < b.len() {
        if b[i] == b'&' {
            if let Some(semi) = s[i..].find(';') {
                let ent = &s[i + 1..i + semi];
                match ent {
                    "amp" => out.push('&'),
                    "lt" => out.push('<'),
                    "gt" => out.push('>'),
                    "quot" => out.push('"'),
                    "apos" => out.push('\''),
                    e if e.starts_with("#x") || e.starts_with("#X") => {
                        if let Ok(v) = u32::from_str_radix(&e[2..], 16) {
                            if let Some(c) = char::from_u32(v) {
                                out.push(c);
                            }
                        }
                    }
                    e if e.starts_with('#') => {
                        if let Ok(v) = e[1..].parse::<u32>() {
                            if let Some(c) = char::from_u32(v) {
                                out.push(c);
                            }
                        }
                    }
                    _ => out.push_str(&s[i..i + semi + 1]),
                }
                i += semi + 1;
                continue;
            }
        }
        // byte-safe char advance
        let ch_len = utf8_len(b[i]);
        let chunk = &s[i..(i + ch_len).min(s.len())];
        out.push_str(chunk);
        i += ch_len;
    }
    out
}

fn utf8_len(b: u8) -> usize {
    if b < 0x80 {
        1
    } else if b >> 5 == 0b110 {
        2
    } else if b >> 4 == 0b1110 {
        3
    } else if b >> 3 == 0b11110 {
        4
    } else {
        1
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_ooxml_snippet() {
        let xml = r#"<?xml version="1.0"?><w:document xmlns:w="ns"><w:p><w:r><w:t>hi &amp; bye</w:t></w:r></w:p><w:br/></w:document>"#;
        let mut p = Parser::new(xml);
        assert!(matches!(p.next(), Some(Event::Start(n, _)) if n == "w:document"));
        assert!(matches!(p.next(), Some(Event::Start(n, _)) if n == "w:p"));
        assert!(matches!(p.next(), Some(Event::Start(n, _)) if n == "w:r"));
        assert!(matches!(p.next(), Some(Event::Start(n, _)) if n == "w:t"));
        assert_eq!(p.next(), Some(Event::Text("hi & bye".into())));
        assert!(matches!(p.next(), Some(Event::End(n)) if n == "w:t"));
        assert!(matches!(p.next(), Some(Event::End(n)) if n == "w:r"));
        assert!(matches!(p.next(), Some(Event::End(n)) if n == "w:p"));
        assert!(matches!(p.next(), Some(Event::Empty(n, _)) if n == "w:br"));
        assert!(matches!(p.next(), Some(Event::End(n)) if n == "w:document"));
        assert_eq!(p.next(), None);
    }

    #[test]
    fn attributes_and_cdata() {
        let mut p = Parser::new(r##"<a x="1&lt;2" y='q'><![CDATA[<raw>]]>tail</a>"##);
        let e = p.next().unwrap();
        if let Event::Start(_, attrs) = e {
            assert_eq!(attrs[0], ("x".into(), "1<2".into()));
            assert_eq!(attrs[1], ("y".into(), "q".into()));
        } else {
            panic!("want start");
        }
        assert_eq!(p.next().unwrap(), Event::Text("<raw>".into()));
        assert_eq!(p.next().unwrap(), Event::Text("tail".into()));
    }
}
