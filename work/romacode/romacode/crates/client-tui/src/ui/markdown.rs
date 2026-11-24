//! Markdown-lite renderer for assistant text → wrapped transcript rows.
//! Supports paragraphs (line breaks kept), `inline code`, **bold**, bullets,
//! headings and fenced code / diff blocks with line numbers.
use super::draw::{pad_to, truncate, Row};
use super::{highlight, theme::*};
use ratatui::style::{Modifier, Style};
use unicode_width::UnicodeWidthStr;

fn body() -> Style {
    Style::new().fg(TEXT).add_modifier(Modifier::BOLD)
}

pub fn render(text: &str, width: usize, indent: usize, out: &mut Vec<Row>) {
    let width = width.max(8);
    let lines: Vec<&str> = text.split('\n').collect();
    let mut i = 0;
    let mut last_blank = true;
    while i < lines.len() {
        let l = lines[i];
        if let Some(info) = l.trim_start().strip_prefix("```") {
            let info = info.trim().to_string();
            let mut code = Vec::new();
            i += 1;
            while i < lines.len() && !lines[i].trim_start().starts_with("```") {
                code.push(lines[i]);
                i += 1;
            }
            i += 1; // closing fence (or end of stream)
            if !last_blank {
                out.push(Row::blank());
            }
            code_block(&info, &code, width, indent, out);
            last_blank = false;
            continue;
        }
        if l.trim().is_empty() {
            if !last_blank {
                out.push(Row::blank());
                last_blank = true;
            }
            i += 1;
            continue;
        }
        last_blank = false;
        let (prefix, content, hang) =
            if let Some(r) = l.strip_prefix("- ").or_else(|| l.strip_prefix("* ")) {
                ("• ", r, 2)
            } else if l.starts_with('#') {
                ("", l.trim_start_matches('#').trim_start(), 0)
            } else {
                ("", l, 0)
            };
        let heading = l.starts_with('#');
        let words = inline(content, heading);
        wrap_styled(
            prefix,
            &words,
            width.saturating_sub(indent),
            hang,
            indent,
            out,
        );
        i += 1;
    }
}

/// Split inline markup into styled words (spaces preserved as separate tokens).
fn inline(s: &str, heading: bool) -> Vec<(String, Style)> {
    let mut toks: Vec<(String, Style)> = Vec::new();
    let base = if heading {
        Style::new().fg(AMBER).add_modifier(Modifier::BOLD)
    } else {
        body()
    };
    let chars: Vec<char> = s.chars().collect();
    let mut i = 0;
    let mut cur = String::new();
    let mut bold = false;
    let flush = |toks: &mut Vec<(String, Style)>, cur: &mut String, st: Style| {
        if !cur.is_empty() {
            toks.push((std::mem::take(cur), st));
        }
    };
    while i < chars.len() {
        if chars[i] == '`' {
            if let Some(end) = chars[i + 1..].iter().position(|&c| c == '`') {
                let st = if bold {
                    base.fg(ratatui::style::Color::Rgb(255, 255, 255))
                } else {
                    base
                };
                flush(&mut toks, &mut cur, st);
                let code: String = chars[i + 1..i + 1 + end].iter().collect();
                toks.push((code, Style::new().fg(AMBER).add_modifier(Modifier::BOLD)));
                i += end + 2;
                continue;
            }
        }
        if chars[i] == '*' && chars.get(i + 1) == Some(&'*') {
            let st = if bold {
                base.fg(ratatui::style::Color::Rgb(255, 255, 255))
            } else {
                base
            };
            flush(&mut toks, &mut cur, st);
            bold = !bold;
            i += 2;
            continue;
        }
        cur.push(chars[i]);
        i += 1;
    }
    let st = if bold {
        base.fg(ratatui::style::Color::Rgb(255, 255, 255))
    } else {
        base
    };
    flush(&mut toks, &mut cur, st);
    toks
}

fn wrap_styled(
    prefix: &str,
    toks: &[(String, Style)],
    width: usize,
    hang: usize,
    indent: usize,
    out: &mut Vec<Row>,
) {
    // explode tokens into words, keeping style
    let mut words: Vec<(String, Style)> = Vec::new();
    for (t, st) in toks {
        let mut cur = String::new();
        for ch in t.chars() {
            if ch == ' ' {
                if !cur.is_empty() {
                    words.push((std::mem::take(&mut cur), *st));
                }
                words.push((" ".into(), *st));
            } else {
                cur.push(ch);
            }
        }
        if !cur.is_empty() {
            words.push((cur, *st));
        }
    }
    let pad = " ".repeat(indent);
    let mut row: Vec<(String, Style)> = vec![(format!("{pad}{prefix}"), body())];
    let mut w = prefix.width();
    let first_w = w;
    let mut line_has = false;
    for (word, st) in words {
        let ww = word.width();
        if word == " " {
            if line_has {
                row.push((word, st));
                w += 1;
            }
            continue;
        }
        if line_has && w + ww > width {
            // drop trailing space
            if let Some(last) = row.last() {
                if last.0 == " " {
                    row.pop();
                }
            }
            out.push(Row::from(std::mem::take(&mut row)));
            row = vec![(format!("{pad}{}", " ".repeat(hang)), body())];
            w = hang;
            line_has = false;
        }
        // hard split very long tokens
        if ww > width.saturating_sub(w) && !line_has {
            let mut part = String::new();
            let mut pw = 0;
            for ch in word.chars() {
                let cw = unicode_width::UnicodeWidthChar::width(ch).unwrap_or(0);
                if w + pw + cw > width {
                    row.push((std::mem::take(&mut part), st));
                    out.push(Row::from(std::mem::take(&mut row)));
                    row = vec![(format!("{pad}{}", " ".repeat(hang)), body())];
                    w = hang;
                    pw = 0;
                }
                part.push(ch);
                pw += cw;
            }
            row.push((part, st));
            w += pw;
            line_has = true;
            continue;
        }
        row.push((word, st));
        w += ww;
        line_has = true;
    }
    let _ = first_w;
    if let Some(last) = row.last() {
        if last.0 == " " {
            row.pop();
        }
    }
    out.push(Row::from(row));
}

fn code_block(info: &str, code: &[&str], width: usize, indent: usize, out: &mut Vec<Row>) {
    let mut parts = info.split_whitespace();
    let lang = parts.next().unwrap_or("");
    let file = parts.next().unwrap_or("");
    let title = if !file.is_empty() { file } else { lang };
    let is_diff = lang == "diff" || lang == "patch";
    let box_w = width.saturating_sub(indent).max(12);
    let inner = box_w - 2;
    let bs = Style::new().fg(BORDER);
    let pad = " ".repeat(indent);
    let mk = |spans: Vec<(String, Style)>, bg: Option<(ratatui::style::Color, u16, u16)>| {
        let mut sp = vec![(pad.clone(), Style::new())];
        sp.extend(spans);
        Row { spans: sp, bg }
    };
    out.push(mk(vec![(format!("╭{}╮", "─".repeat(inner)), bs)], None));
    let head = if title.is_empty() {
        "code".to_string()
    } else {
        truncate(title, inner.saturating_sub(2))
    };
    out.push(mk(
        vec![
            ("│".into(), bs),
            (pad_to(&format!(" {head}"), inner), Style::new().fg(DIM)),
            ("│".into(), bs),
        ],
        None,
    ));

    // line-number bookkeeping
    let mut new_ln: i64 = 1;
    let digits = (code.len() + 1).to_string().len().max(2);
    for (n, raw) in code.iter().enumerate() {
        let (sign, text, bg, ln): (&str, &str, Option<ratatui::style::Color>, Option<i64>) =
            if is_diff {
                if let Some(h) = raw.strip_prefix("@@") {
                    // @@ -a,b +c,d @@
                    if let Some(p) = h.split_whitespace().find(|t| t.starts_with('+')) {
                        new_ln = p[1..]
                            .split(',')
                            .next()
                            .and_then(|v| v.parse().ok())
                            .unwrap_or(1);
                    }
                    (" ", raw, None, None)
                } else if raw.starts_with("+++")
                    || raw.starts_with("---")
                    || raw.starts_with("diff ")
                    || raw.starts_with("index ")
                {
                    continue;
                } else if let Some(t) = raw.strip_prefix('+') {
                    let l = new_ln;
                    new_ln += 1;
                    ("+", t, Some(DIFF_ADD), Some(l))
                } else if let Some(t) = raw.strip_prefix('-') {
                    ("-", t, Some(DIFF_DEL), Some(new_ln))
                } else {
                    let l = new_ln;
                    new_ln += 1;
                    (" ", raw.strip_prefix(' ').unwrap_or(raw), None, Some(l))
                }
            } else {
                (" ", raw, None, Some(n as i64 + 1))
            };
        let gutter = match ln {
            Some(l) => format!(" {:>w$} ", l, w = digits),
            None => " ".repeat(digits + 2),
        };
        let sign_style = match sign {
            "+" => Style::new().fg(GREEN).add_modifier(Modifier::BOLD),
            "-" => Style::new().fg(RED).add_modifier(Modifier::BOLD),
            _ => Style::new().fg(DIMMER),
        };
        let avail = inner.saturating_sub(gutter.width() + 3);
        let t = text.replace('\t', "    ");
        let t = truncate(&t, avail);
        let used = gutter.width() + 3 + t.width();
        let mut spans = vec![
            ("│".to_string(), bs),
            (gutter, Style::new().fg(DIMMER)),
            (format!(" {sign} "), sign_style),
        ];
        if ln.is_none() {
            spans.push((t.clone(), Style::new().fg(DIMMER)));
        } else {
            spans.extend(highlight::line(&t));
        }
        spans.push((" ".repeat(inner.saturating_sub(used)), Style::new()));
        spans.push(("│".to_string(), bs));
        out.push(mk(
            spans,
            bg.map(|c| (c, (indent + 1) as u16, (indent + 1 + inner) as u16)),
        ));
    }
    out.push(mk(vec![(format!("╰{}╯", "─".repeat(inner)), bs)], None));
}
