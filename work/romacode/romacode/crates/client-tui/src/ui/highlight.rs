//! Tiny C-family / Rust / Python / shell-ish syntax highlighter.
use super::theme::*;
use ratatui::style::Style;

const KEYWORDS: &[&str] = &[
    "fn", "let", "mut", "for", "in", "match", "return", "if", "else", "async", "await", "use",
    "pub", "struct", "enum", "impl", "trait", "const", "static", "while", "loop", "break",
    "continue", "mod", "type", "where", "move", "as", "ref", "self", "Self", "def", "class",
    "import", "from", "function", "var", "export", "new", "try", "catch", "throw", "true", "false",
    "None", "null", "unsafe", "dyn", "yield", "with", "lambda",
];

pub fn line(src: &str) -> Vec<(String, Style)> {
    let chars: Vec<char> = src.chars().collect();
    let mut out: Vec<(String, Style)> = Vec::new();
    let mut i = 0;
    let push = |out: &mut Vec<(String, Style)>, s: String, st: Style| {
        if s.is_empty() {
            return;
        }
        if let Some(last) = out.last_mut() {
            if last.1 == st {
                last.0.push_str(&s);
                return;
            }
        }
        out.push((s, st));
    };
    let base = Style::new().fg(TEXT);
    while i < chars.len() {
        let c = chars[i];
        // comments
        if (c == '/' && chars.get(i + 1) == Some(&'/'))
            || c == '#' && (i == 0 || chars[..i].iter().all(|x| x.is_whitespace()))
        {
            push(
                &mut out,
                chars[i..].iter().collect(),
                Style::new().fg(DIMMER),
            );
            break;
        }
        // strings
        if c == '"' || c == '\'' && is_string_quote(&chars, i) {
            let q = c;
            let mut j = i + 1;
            while j < chars.len() {
                if chars[j] == '\\' {
                    j += 2;
                    continue;
                }
                if chars[j] == q {
                    j += 1;
                    break;
                }
                j += 1;
            }
            let j = j.min(chars.len());
            push(
                &mut out,
                chars[i..j].iter().collect(),
                Style::new().fg(GREEN),
            );
            i = j;
            continue;
        }
        // numbers
        if c.is_ascii_digit() {
            let mut j = i;
            while j < chars.len()
                && (chars[j].is_ascii_alphanumeric() || chars[j] == '_' || chars[j] == '.')
            {
                if chars[j] == '.' && chars.get(j + 1) == Some(&'.') {
                    break;
                }
                j += 1;
            }
            push(
                &mut out,
                chars[i..j].iter().collect(),
                Style::new().fg(ORANGE),
            );
            i = j;
            continue;
        }
        // identifiers
        if c.is_alphabetic() || c == '_' {
            let mut j = i;
            while j < chars.len() && (chars[j].is_alphanumeric() || chars[j] == '_') {
                j += 1;
            }
            let word: String = chars[i..j].iter().collect();
            let st = if KEYWORDS.contains(&word.as_str()) {
                Style::new().fg(AMBER)
            } else if chars.get(j) == Some(&'!') {
                j += 1;
                Style::new().fg(AMBER)
            } else if word.chars().next().map_or(false, |f| f.is_uppercase()) {
                Style::new().fg(YELLOW)
            } else {
                base
            };
            let word: String = chars[i..j].iter().collect();
            push(&mut out, word, st);
            i = j;
            continue;
        }
        push(&mut out, c.to_string(), base);
        i += 1;
    }
    out
}

/// A `'` starts a string only if a closing quote follows on the same line
/// shortly after (avoids treating Rust lifetimes as strings).
fn is_string_quote(chars: &[char], i: usize) -> bool {
    let rest = &chars[i + 1..];
    match rest.iter().position(|&c| c == '\'') {
        Some(p) => {
            p <= 40
                && !(p >= 1
                    && rest[0].is_alphabetic()
                    && rest
                        .get(1)
                        .map_or(false, |c| c.is_alphanumeric() || *c == '_')
                    && p > 3)
        }
        None => false,
    }
}
