//! Word (.docx) — OOXML WordprocessingML, create + read, pure Rust.
//!
//! Create: headings, paragraphs (inline `**bold**`/`*italic*`/`__underline__`/
//! `` `code` `` formatting), bullet + numbered lists, grid tables with a
//! styled header row, images (PNG/JPEG/GIF/BMP passthrough), page breaks, a
//! Word TOC field, title/author metadata and a footer with page numbers.
//! Read: paragraphs with styles, table grids, document stats.

use crate::xml::{escape, Parser};
use crate::zipio::{ZipReader, ZipWriter};
use serde_json::{json, Value};

// ── public model ────────────────────────────────────────────────────────────

/// One content block of a document.
#[derive(Clone, Debug)]
pub enum Block {
    Heading { level: u8, text: String },
    Para { text: String, align: Align, size_pt: Option<f32>, font: Option<String>, color: Option<String> },
    Bullets { items: Vec<(String, u8)> },
    Numbered { items: Vec<(String, u8)> },
    Table { header: Vec<String>, rows: Vec<Vec<String>>, widths: Option<Vec<f64>> },
    PageBreak,
    Image { path: String, width_px: Option<f64>, caption: Option<String> },
    Toc,
    Quote { text: String },
    Code { text: String },
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Align {
    Left,
    Center,
    Right,
    Justify,
}

/// Document metadata.
#[derive(Clone, Debug, Default)]
pub struct Meta {
    pub title: Option<String>,
    pub author: Option<String>,
    pub subject: Option<String>,
}

/// Parse the JSON block list used by the `docx_create` tool.
pub fn blocks_from_json(v: &Value) -> Result<Vec<Block>, String> {
    let arr = v.as_array().ok_or("blocks must be an array")?;
    let mut out = Vec::new();
    for b in arr {
        let ty = b["type"].as_str().unwrap_or("");
        let g = |k: &str| b[k].as_str().unwrap_or("").to_string();
        match ty {
            "h1" | "h2" | "h3" | "h4" | "heading" => {
                let level = if ty == "heading" { b["level"].as_u64().unwrap_or(1).min(4) as u8 } else { ty[1..].parse().unwrap_or(1) };
                out.push(Block::Heading { level, text: g("text") });
            }
            "p" | "para" | "paragraph" | "text" => out.push(Block::Para {
                text: g("text"),
                align: match b["align"].as_str().unwrap_or("justify") {
                    "left" => Align::Left,
                    "center" | "centered" => Align::Center,
                    "right" => Align::Right,
                    _ => Align::Justify,
                },
                size_pt: b["size_pt"].as_f64().map(|f| f as f32),
                font: b["font"].as_str().map(String::from),
                color: b["color"].as_str().map(String::from),
            }),
            "bullets" | "bullet" => out.push(Block::Bullets { items: items_from(b)? }),
            "numbered" | "number_list" => out.push(Block::Numbered { items: items_from(b)? }),
            "table" => {
                let header = b["header"].as_array().map(|a| a.iter().filter_map(|x| x.as_str().map(String::from)).collect()).unwrap_or_default();
                let rows = b["rows"]
                    .as_array()
                    .ok_or("table needs rows")?
                    .iter()
                    .map(|r| r.as_array().map(|c| c.iter().filter_map(|x| x.as_str().map(String::from)).collect::<Vec<_>>()).unwrap_or_default())
                    .collect();
                let widths = b["widths"].as_array().map(|a| a.iter().filter_map(|x| x.as_f64()).collect());
                out.push(Block::Table { header, rows, widths });
            }
            "pagebreak" | "page_break" => out.push(Block::PageBreak),
            "image" => out.push(Block::Image { path: g("path"), width_px: b["width_px"].as_f64(), caption: b["caption"].as_str().map(String::from) }),
            "toc" => out.push(Block::Toc),
            "quote" => out.push(Block::Quote { text: g("text") }),
            "code" => out.push(Block::Code { text: g("text") }),
            other => return Err(format!("unknown block type {other:?}")),
        }
    }
    Ok(out)
}

fn items_from(b: &Value) -> Result<Vec<(String, u8)>, String> {
    let arr = b["items"].as_array().ok_or("list needs items")?;
    Ok(arr
        .iter()
        .map(|it| match it {
            Value::String(s) => (s.clone(), 0u8),
            Value::Object(_) => (it["text"].as_str().unwrap_or("").to_string(), it["level"].as_u64().unwrap_or(0).min(3) as u8),
            _ => (String::new(), 0),
        })
        .collect())
}

// ── create ──────────────────────────────────────────────────────────────────

struct Img {
    id: usize,
    rel_id: String,
    name: String,
    ext: String,
    data: Vec<u8>,
    cx_emu: u32,
    cy_emu: u32,
}

/// Build a .docx file from blocks. Returns the finished zip bytes.
pub fn build(meta: &Meta, blocks: &[Block], with_page_numbers: bool) -> Result<Vec<u8>, String> {
    let mut imgs: Vec<Img> = Vec::new();
    let mut body = String::new();
    let mut numbering_used = false;

    for b in blocks {
        match b {
            Block::Heading { level, text } => {
                body.push_str(&para_xml(
                    &format!("Heading{}", level),
                    &inline(text),
                    "",
                    false,
                ));
            }
            Block::Para { text, align, size_pt, font, color } => {
                let jc = match align {
                    Align::Left => "",
                    Align::Center => "center",
                    Align::Right => "right",
                    Align::Justify => "both",
                };
                let mut opts = String::new();
                if let Some(sz) = size_pt {
                    opts.push_str(&format!("<w:sz w:val=\"{}\"/><w:szCs w:val=\"{}\"/>", (sz * 2.0) as u32, (sz * 2.0) as u32));
                }
                if let Some(f) = font {
                    opts.insert_str(0, &format!("<w:rFonts w:ascii=\"{}\" w:hAnsi=\"{}\" w:eastAsia=\"{}\"/>", escape(f), escape(f), escape(f)));
                }
                if let Some(c) = color {
                    opts.push_str(&format!("<w:color w:val=\"{}\"/>", c.trim_start_matches('#')));
                }
                body.push_str(&para_xml_opts("Normal", &inline(text), jc, &opts));
            }
            Block::Bullets { items } => {
                numbering_used = true;
                for (text, level) in items {
                    body.push_str(&list_para("Normal", &inline(text), *level, true));
                }
            }
            Block::Numbered { items } => {
                numbering_used = true;
                for (text, level) in items {
                    body.push_str(&list_para("Normal", &inline(text), *level, false));
                }
            }
            Block::Table { header, rows, widths } => {
                body.push_str(&table_xml(header, rows, widths.as_deref())?);
            }
            Block::PageBreak => {
                body.push_str("<w:p><w:r><w:br w:type=\"page\"/></w:r></w:p>");
            }
            Block::Image { path, width_px, caption } => {
                let rel = load_image(path, width_px.unwrap_or(420.0), &mut imgs)?;
                let _ = &rel;
                let im = imgs.last().ok_or("image load failed")?;
                body.push_str(&image_para(im));
                if let Some(cap) = caption {
                    body.push_str(&para_xml(&"Caption".to_string(), &[Run::plain_text(cap)], "center", false));
                }
            }
            Block::Toc => {
                body.push_str(&format!(
                    "<w:p><w:r><w:fldSimple w:instr=\" TOC \\o &quot;1-3&quot; \\h \\z \\u \"><w:r><w:t>Right-click and choose \u{201c}Update Field\u{201d} to build the table of contents.</w:t></w:r></w:fldSimple></w:r></w:p>"
                ));
            }
            Block::Quote { text } => {
                body.push_str(&format!(
                    "<w:p><w:pPr><w:pStyle w:val=\"Normal\"/><w:ind w:left=\"567\" w:right=\"567\"/><w:pBdr><w:left w:val=\"single\" w:sz=\"18\" w:space=\"8\" w:color=\"4472C4\"/></w:pBdr><w:spacing w:before=\"120\" w:after=\"120\"/></w:pPr><w:r><w:rPr><w:i/><w:color w:val=\"444444\"/></w:rPr><w:t xml:space=\"preserve\">{}</w:t></w:r></w:p>",
                    escape(&text)
                ));
            }
            Block::Code { text } => {
                for line in text.lines() {
                    body.push_str(&format!(
                        "<w:p><w:pPr><w:shd w:val=\"clear\" w:fill=\"F2F2F2\"/><w:ind w:left=\"284\"/></w:pPr><w:r><w:rPr><w:rFonts w:ascii=\"Consolas\" w:hAnsi=\"Consolas\"/><w:sz w:val=\"18\"/></w:rPr><w:t xml:space=\"preserve\">{}</w:t></w:r></w:p>",
                        escape(line)
                    ));
                }
            }
        }
    }

    let sect = if with_page_numbers {
        "<w:sectPr><w:footerReference w:type=\"default\" r:id=\"rIdFooter\"/><w:pgSz w:w=\"11906\" w:h=\"16838\"/><w:pgMar w:top=\"1417\" w:right=\"1417\" w:bottom=\"1417\" w:left=\"1417\" w:header=\"708\" w:footer=\"708\" w:gutter=\"0\"/></w:sectPr>"
    } else {
        "<w:sectPr><w:pgSz w:w=\"11906\" w:h=\"16838\"/><w:pgMar w:top=\"1417\" w:right=\"1417\" w:bottom=\"1417\" w:left=\"1417\" w:header=\"708\" w:footer=\"708\" w:gutter=\"0\"/></w:sectPr>"
    };

    let document = format!(
        "<?xml version=\"1.0\" encoding=\"UTF-8\" standalone=\"yes\"?>\
<w:document xmlns:w=\"http://schemas.openxmlformats.org/wordprocessingml/2006/main\" \
xmlns:r=\"http://schemas.openxmlformats.org/officeDocument/2006/relationships\" \
xmlns:wp=\"http://schemas.openxmlformats.org/drawingml/2006/wordprocessingDrawing\" \
xmlns:a=\"http://schemas.openxmlformats.org/drawingml/2006/main\" \
xmlns:pic=\"http://schemas.openxmlformats.org/drawingml/2006/picture\"><w:body>{body}{sect}</w:body></w:document>"
    );

    // content types
    let mut ct = String::from(
        "<?xml version=\"1.0\" encoding=\"UTF-8\" standalone=\"yes\"?><Types xmlns=\"http://schemas.openxmlformats.org/package/2006/content-types\">\
<Default Extension=\"rels\" ContentType=\"application/vnd.openxmlformats-package.relationships+xml\"/>\
<Default Extension=\"xml\" ContentType=\"application/xml\"/>\
<Default Extension=\"png\" ContentType=\"image/png\"/><Default Extension=\"jpeg\" ContentType=\"image/jpeg\"/>\
<Default Extension=\"jpg\" ContentType=\"image/jpeg\"/><Default Extension=\"gif\" ContentType=\"image/gif\"/>\
<Default Extension=\"bmp\" ContentType=\"image/bmp\"/>\
<Override PartName=\"/word/document.xml\" ContentType=\"application/vnd.openxmlformats-officedocument.wordprocessingml.document.main+xml\"/>\
<Override PartName=\"/word/styles.xml\" ContentType=\"application/vnd.openxmlformats-officedocument.wordprocessingml.styles+xml\"/>\
<Override PartName=\"/docProps/core.xml\" ContentType=\"application/vnd.openxmlformats-package.core-properties+xml\"/>\
<Override PartName=\"/docProps/app.xml\" ContentType=\"application/vnd.openxmlformats-officedocument.extended-properties+xml\"/>",
    );
    if numbering_used {
        ct.push_str("<Override PartName=\"/word/numbering.xml\" ContentType=\"application/vnd.openxmlformats-officedocument.wordprocessingml.numbering+xml\"/>");
    }
    if with_page_numbers {
        ct.push_str("<Override PartName=\"/word/footer1.xml\" ContentType=\"application/vnd.openxmlformats-officedocument.wordprocessingml.footer+xml\"/>");
    }
    ct.push_str("</Types>");

    // package rels
    let rels = "<?xml version=\"1.0\" encoding=\"UTF-8\" standalone=\"yes\"?>\
<Relationships xmlns=\"http://schemas.openxmlformats.org/package/2006/relationships\">\
<Relationship Id=\"rId1\" Type=\"http://schemas.openxmlformats.org/officeDocument/2006/relationships/officeDocument\" Target=\"word/document.xml\"/>\
<Relationship Id=\"rId2\" Type=\"http://schemas.openxmlformats.org/package/2006/relationships/metadata/core-properties\" Target=\"docProps/core.xml\"/>\
<Relationship Id=\"rId3\" Type=\"http://schemas.openxmlformats.org/officeDocument/2006/relationships/extended-properties\" Target=\"docProps/app.xml\"/>\
</Relationships>";

    // document rels: styles, numbering?, footer?, images
    let mut drels = String::from(
        "<?xml version=\"1.0\" encoding=\"UTF-8\" standalone=\"yes\"?>\
<Relationships xmlns=\"http://schemas.openxmlformats.org/package/2006/relationships\">\
<Relationship Id=\"rIdStyles\" Type=\"http://schemas.openxmlformats.org/officeDocument/2006/relationships/styles\" Target=\"styles.xml\"/>",
    );
    if numbering_used {
        drels.push_str("<Relationship Id=\"rIdNum\" Type=\"http://schemas.openxmlformats.org/officeDocument/2006/relationships/numbering\" Target=\"numbering.xml\"/>");
    }
    if with_page_numbers {
        drels.push_str("<Relationship Id=\"rIdFooter\" Type=\"http://schemas.openxmlformats.org/officeDocument/2006/relationships/footer\" Target=\"footer1.xml\"/>");
    }
    for (i, im) in imgs.iter().enumerate() {
        drels.push_str(&format!(
            "<Relationship Id=\"{}\" Type=\"http://schemas.openxmlformats.org/officeDocument/2006/relationships/image\" Target=\"media/{}\"/>",
            im.rel_id, im.name
        ));
        let _ = i;
    }
    drels.push_str("</Relationships>");

    let styles = styles_xml();
    let numbering = if numbering_used { numbering_xml() } else { String::new() };
    let footer = if with_page_numbers {
        format!(
            "<?xml version=\"1.0\" encoding=\"UTF-8\" standalone=\"yes\"?>\
<w:ftr xmlns:w=\"http://schemas.openxmlformats.org/wordprocessingml/2006/main\" xmlns:r=\"http://schemas.openxmlformats.org/officeDocument/2006/relationships\"><w:p><w:pPr><w:jc w:val=\"center\"/></w:pPr><w:r><w:fldSimple w:instr=\" PAGE \"><w:rPr><w:sz w:val=\"18\"/><w:color w:val=\"666666\"/></w:rPr><w:t>1</w:t></w:fldSimple></w:r></w:p></w:ftr>"
        )
    } else {
        String::new()
    };

    let date = current_iso_date();
    let core = format!(
        "<?xml version=\"1.0\" encoding=\"UTF-8\" standalone=\"yes\"?>\
<cp:coreProperties xmlns:cp=\"http://schemas.openxmlformats.org/package/2006/metadata/core-properties\" \
xmlns:dc=\"http://purl.org/dc/elements/1.1/\" xmlns:dcterms=\"http://purl.org/dc/terms/\" \
xmlns:dcmitype=\"http://purl.org/dc/dcmitype/\" xmlns:xsi=\"http://www.w3.org/2001/XMLSchema-instance\">\
<dc:title>{}</dc:title><dc:subject>{}</dc:subject><dc:creator>{}</dc:creator>\
<cp:lastModifiedBy>{}</cp:lastModifiedBy><dcterms:created xsi:type=\"dcterms:W3CDTF\">{}</dcterms:created>\
<dcterms:modified xsi:type=\"dcterms:W3CDTF\">{}</dcterms:modified></cp:coreProperties>",
        escape(meta.title.as_deref().unwrap_or("")),
        escape(meta.subject.as_deref().unwrap_or("")),
        escape(meta.author.as_deref().unwrap_or("RomaCode")),
        escape(meta.author.as_deref().unwrap_or("RomaCode")),
        date,
        date
    );
    let app = "<?xml version=\"1.0\" encoding=\"UTF-8\" standalone=\"yes\"?>\
<Properties xmlns=\"http://schemas.openxmlformats.org/officeDocument/2006/extended-properties\">\
<Application>RomaCode documents engine</Application></Properties>";

    let mut z = ZipWriter::new();
    z.add("[Content_Types].xml", ct.as_bytes());
    z.add("_rels/.rels", rels.as_bytes());
    z.add("docProps/core.xml", core.as_bytes());
    z.add("docProps/app.xml", app.as_bytes());
    z.add("word/document.xml", document.as_bytes());
    z.add("word/_rels/document.xml.rels", drels.as_bytes());
    z.add("word/styles.xml", styles.as_bytes());
    if numbering_used {
        z.add("word/numbering.xml", numbering.as_bytes());
    }
    if with_page_numbers {
        z.add("word/footer1.xml", footer.as_bytes());
    }
    for im in &imgs {
        z.add(&format!("word/media/{}", im.name), &im.data);
    }
    Ok(z.finish())
}

fn current_iso_date() -> String {
    // UTC date in RFC3339 — no chrono: derive from UNIX seconds.
    let secs = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0);
    let days = secs / 86400;
    let (y, m, d) = civil_from_days(days as i64);
    format!("{y:04}-{m:02}-{d:02}T00:00:00Z")
}

/// Howard Hinnant's civil-from-days (proleptic Gregorian).
pub(crate) fn civil_from_days(z: i64) -> (i64, u32, u32) {
    let z = z + 719_468;
    let era = if z >= 0 { z } else { z - 146_096 } / 146_097;
    let doe = (z - era * 146_097) as u64;
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146_096) / 365;
    let y = yoe as i64 + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = (doy - (153 * mp + 2) / 5 + 1) as u32;
    let m = if mp < 10 { mp + 3 } else { mp - 9 } as u32;
    (if m <= 2 { y + 1 } else { y }, m, d)
}

// ── inline formatting ───────────────────────────────────────────────────────

/// A run: text + formatting flags. `**bold**`, `*italic*`, `__underline__`,
/// `` `code` `` and bare text are recognized.
#[derive(Debug)]
struct Run {
    text: String,
    bold: bool,
    italic: bool,
    underline: bool,
    code: bool,
}

impl Run {
    fn text(s: &str) -> Self {
        Self { text: s.to_string(), bold: false, italic: false, underline: false, code: false }
    }
    fn plain_text(s: &str) -> Self {
        Self::text(s)
    }
    fn with(text: String, bold: bool, italic: bool, underline: bool, code: bool) -> Self {
        Self { text, bold, italic, underline, code }
    }
}

/// Split inline markdown-ish markup into runs. Unmatched markers stay literal.
pub(crate) fn inline(s: &str) -> Vec<Run> {
    let mut runs = Vec::new();
    let mut bold = false;
    let mut italic = false;
    let mut underline = false;
    let mut code = false;
    let mut cur = String::new();
    let chars: Vec<char> = s.chars().collect();
    let mut i = 0;
    let flush = |runs: &mut Vec<Run>, cur: &mut String, b: bool, i2: bool, u: bool, c: bool| {
        if !cur.is_empty() {
            runs.push(Run::with(std::mem::take(cur), b, i2, u, c));
        }
    };
    while i < chars.len() {
        let two: String = chars[i..(i + 2).min(chars.len())].iter().collect();
        if two == "**" {
            flush(&mut runs, &mut cur, bold, italic, underline, code);
            bold = !bold;
            i += 2;
            continue;
        }
        if two == "__" {
            flush(&mut runs, &mut cur, bold, italic, underline, code);
            underline = !underline;
            i += 2;
            continue;
        }
        let c = chars[i];
        if c == '*' {
            flush(&mut runs, &mut cur, bold, italic, underline, code);
            italic = !italic;
            i += 1;
            continue;
        }
        if c == '`' {
            flush(&mut runs, &mut cur, bold, italic, underline, code);
            code = !code;
            i += 1;
            continue;
        }
        cur.push(c);
        i += 1;
    }
    flush(&mut runs, &mut cur, bold, italic, underline, code);
    if runs.is_empty() {
        runs.push(Run::text(""));
    }
    runs
}

fn run_xml(r: &Run) -> String {
    let mut rpr = String::new();
    if r.code {
        rpr.push_str("<w:rFonts w:ascii=\"Consolas\" w:hAnsi=\"Consolas\"/><w:shd w:val=\"clear\" w:fill=\"F2F2F2\"/>");
    }
    if r.bold {
        rpr.push_str("<w:b/>");
    }
    if r.italic {
        rpr.push_str("<w:i/>");
    }
    if r.underline {
        rpr.push_str("<w:u w:val=\"single\"/>");
    }
    format!(
        "<w:r>{}<w:t xml:space=\"preserve\">{}</w:t></w:r>",
        if rpr.is_empty() { String::new() } else { format!("<w:rPr>{rpr}</w:rPr>") },
        escape(&r.text)
    )
}

fn para_xml(style: &str, runs: &[Run], jc: &str, _spacing: bool) -> String {
    para_xml_opts(style, runs, jc, "")
}

/// `run_opts` is raw rPr XML injected into every run (font/size/color).
fn para_xml_opts(style: &str, runs: &[Run], jc: &str, run_opts: &str) -> String {
    let mut ppr = format!("<w:pStyle w:val=\"{style}\"/>");
    if !jc.is_empty() {
        ppr.push_str(&format!("<w:jc w:val=\"{jc}\"/>"));
    }
    ppr.push_str("<w:spacing w:after=\"120\"/>");
    let rendered = runs
        .iter()
        .map(|r| {
            let mut rpr = run_opts.to_string();
            if r.code {
                rpr.insert_str(0, "<w:rFonts w:ascii=\"Consolas\" w:hAnsi=\"Consolas\"/><w:shd w:val=\"clear\" w:fill=\"F2F2F2\"/>");
            }
            if r.bold {
                rpr.push_str("<w:b/>");
            }
            if r.italic {
                rpr.push_str("<w:i/>");
            }
            if r.underline {
                rpr.push_str("<w:u w:val=\"single\"/>");
            }
            format!(
                "<w:r>{}<w:t xml:space=\"preserve\">{}</w:t></w:r>",
                if rpr.is_empty() { String::new() } else { format!("<w:rPr>{rpr}</w:rPr>") },
                escape(&r.text)
            )
        })
        .collect::<String>();
    format!("<w:p><w:pPr>{ppr}</w:pPr>{rendered}</w:p>")
}

fn list_para(style: &str, text_runs: &[Run], level: u8, bullet: bool) -> String {
    let num_id = if bullet { 1 } else { 2 };
    format!(
        "<w:p><w:pPr><w:pStyle w:val=\"{style}\"/><w:numPr><w:ilvl w:val=\"{level}\"/><w:numId w:val=\"{num_id}\"/></w:numPr><w:spacing w:after=\"80\"/></w:pPr>{}</w:p>",
        text_runs.iter().map(run_xml).collect::<String>()
    )
}

fn table_xml(header: &[String], rows: &[Vec<String>], widths: Option<&[f64]>) -> Result<String, String> {
    let cols = header.len().max(rows.iter().map(|r| r.len()).max().unwrap_or(0));
    if cols == 0 {
        return Err("table has no columns".into());
    }
    // Relative widths: given, or equal.
    let total = 9360; // dxa inside 11906 - margins
    let w: Vec<u32> = match widths {
        Some(w) if !w.is_empty() => {
            let sum: f64 = w.iter().sum();
            if sum <= 0.0 {
                vec![total / cols as u32; cols]
            } else {
                (0..cols).map(|i| ((w.get(i).copied().unwrap_or(0.0) / sum) * total as f64) as u32).collect()
            }
        }
        _ => vec![total / cols as u32; cols],
    };
    let border =
        "<w:tblBorders><w:top w:val=\"single\" w:sz=\"4\" w:color=\"BFBFBF\"/><w:left w:val=\"single\" w:sz=\"4\" w:color=\"BFBFBF\"/><w:bottom w:val=\"single\" w:sz=\"4\" w:color=\"BFBFBF\"/><w:right w:val=\"single\" w:sz=\"4\" w:color=\"BFBFBF\"/><w:insideH w:val=\"single\" w:sz=\"4\" w:color=\"BFBFBF\"/><w:insideV w:val=\"single\" w:sz=\"4\" w:color=\"BFBFBF\"/></w:tblBorders>";
    let mut x = format!(
        "<w:tbl><w:tblPr><w:tblW w:w=\"5000\" w:type=\"pct\"/>{border}<w:tblCellMar><w:top w:w=\"57\" w:type=\"dxa\"/><w:left w:w=\"113\" w:type=\"dxa\"/><w:bottom w:w=\"57\" w:type=\"dxa\"/><w:right w:w=\"113\" w:type=\"dxa\"/></w:tblCellMar></w:tblPr><w:tblGrid>{}",
        w.iter().map(|cw| format!("<w:gridCol w:w=\"{cw}\"/>")).collect::<String>()
    );
    x.push_str("</w:tblGrid>");
    if !header.is_empty() {
        x.push_str("<w:tr><w:trPr><w:tblHeader/></w:trPr>");
        for (i, h) in header.iter().take(cols).enumerate() {
            x.push_str(&format!(
                "<w:tc><w:tcPr><w:tcW w:w=\"{}\" w:type=\"dxa\"/><w:shd w:val=\"clear\" w:fill=\"4472C4\"/></w:tcPr><w:p><w:pPr><w:b/><w:spacing w:after=\"0\"/></w:pPr><w:r><w:rPr><w:b/><w:color w:val=\"FFFFFF\"/></w:rPr><w:t xml:space=\"preserve\">{}</w:t></w:r></w:p></w:tc>",
                w[i], escape(h)
            ));
        }
        x.push_str("</w:tr>");
    }
    for row in rows {
        x.push_str("<w:tr>");
        for i in 0..cols {
            let cell = row.get(i).map(String::as_str).unwrap_or("");
            x.push_str(&format!(
                "<w:tc><w:tcPr><w:tcW w:w=\"{}\" w:type=\"dxa\"/></w:tcPr><w:p><w:pPr><w:spacing w:after=\"0\"/></w:pPr><w:r><w:t xml:space=\"preserve\">{}</w:t></w:r></w:p></w:tc>",
                w[i],
                escape(cell)
            ));
        }
        x.push_str("</w:tr>");
    }
    x.push_str("</w:tbl><w:p><w:pPr><w:spacing w:after=\"80\"/></w:pPr></w:p>");
    Ok(x)
}

// ── images ──────────────────────────────────────────────────────────────────

fn image_size(path: &str, data: &[u8]) -> Result<(u32, u32), String> {
    // PNG: IHDR; JPEG: SOF scan; GIF: header; BMP: header. EMU = px * 9525.
    if data.len() > 24 && data[0..8] == [0x89, b'P', b'N', b'G', 0x0d, 0x0a, 0x1a, 0x0a] {
        let w = u32::from_be_bytes([data[16], data[17], data[18], data[19]]);
        let h = u32::from_be_bytes([data[20], data[21], data[22], data[23]]);
        return Ok((w, h));
    }
    if data.len() > 4 && data[0..2] == [0xFF, 0xD8] {
        // walk JPEG segments for SOF0..SOF15 (except DHT/DAC)
        let mut i = 2;
        while i + 9 <= data.len() {
            if data[i] != 0xFF {
                i += 1;
                continue;
            }
            let marker = data[i + 1];
            if (0xC0..=0xCF).contains(&marker) && ![0xC4, 0xC8, 0xCC].contains(&marker) {
                let h = u16::from_be_bytes([data[i + 5], data[i + 6]]) as u32;
                let w = u16::from_be_bytes([data[i + 7], data[i + 8]]) as u32;
                return Ok((w, h));
            }
            let seglen = u16::from_be_bytes([data[i + 2], data[i + 3]]) as usize;
            i += 2 + seglen.max(2);
        }
        return Err("jpeg: no SOF marker found".into());
    }
    if data.len() > 10 && data[0..3] == [b'G', b'I', b'F'] {
        let w = u16::from_le_bytes([data[6], data[7]]) as u32;
        let h = u16::from_le_bytes([data[8], data[9]]) as u32;
        return Ok((w, h));
    }
    if data.len() > 26 && data[0..2] == [b'B', b'M'] {
        let w = u32::from_le_bytes([data[18], data[19], data[20], data[21]]);
        let h = u32::from_le_bytes([data[22], data[23], data[24], data[25]]);
        return Ok((w, h));
    }
    let _ = path;
    Err("unsupported image format (use png/jpg/gif/bmp)".into())
}

fn load_image(path: &str, width_px: f64, imgs: &mut Vec<Img>) -> Result<String, String> {
    let data = std::fs::read(path).map_err(|e| format!("image {path}: {e}"))?;
    let ext = path.rsplit('.').next().unwrap_or("png").to_ascii_lowercase();
    let ext = match ext.as_str() {
        "jpeg" => "jpeg".into(),
        "jpg" => "jpeg".into(),
        e => e.into(),
    };
    let (w, h) = image_size(path, &data)?;
    let rel_id = format!("rIdImg{}", imgs.len() + 10);
    let name = format!("image{}.{}", imgs.len() + 1, ext);
    let cx = ((width_px.max(20.0)) * 9525.0) as u32;
    let cy = if w == 0 {
        cx
    } else {
        ((width_px * h as f64 / w as f64).max(20.0) * 9525.0) as u32
    };
    imgs.push(Img { id: imgs.len() + 1, rel_id: rel_id.clone(), name, ext, data, cx_emu: cx, cy_emu: cy });
    Ok(rel_id)
}

fn image_para(img: &Img) -> String {
    let doc_pr = format!("Picture {}", img.rel_id);
    format!(
        "<w:p><w:pPr><w:jc w:val=\"center\"/></w:pPr><w:r><w:drawing><wp:inline distT=\"0\" distB=\"0\" distL=\"0\" distR=\"0\">\
<wp:extent cx=\"{cx}\" cy=\"{cy}\"/><wp:effectExtent l=\"0\" t=\"0\" r=\"0\" b=\"0\"/>\
<wp:docPr id=\"1\" name=\"{dp}\"/><wp:cNvGraphicFramePr><a:graphicFrameLocks noChangeAspect=\"1\"/></wp:cNvGraphicFramePr>\
<a:graphic><a:graphicData uri=\"http://schemas.openxmlformats.org/drawingml/2006/picture\"><pic:pic>\
<pic:nvPicPr><pic:cNvPr id=\"1\" name=\"{dp}\"/><pic:cNvPicPr/></pic:nvPicPr>\
<pic:blipFill><a:blip r:embed=\"{rid}\"/><a:stretch><a:fillRect/></a:stretch></pic:blipFill>\
<pic:spPr><a:xfrm><a:off x=\"0\" y=\"0\"/><a:ext cx=\"{cx}\" cy=\"{cy}\"/></a:xfrm><a:prstGeom prst=\"rect\"><a:avLst/></a:prstGeom></pic:spPr>\
</pic:pic></a:graphicData></a:graphic></wp:inline></w:drawing></w:r></w:p>",
        cx = img.cx_emu,
        cy = img.cy_emu,
        dp = escape(&doc_pr),
        rid = img.rel_id
    )
}

// ── styles / numbering ──────────────────────────────────────────────────────

fn styles_xml() -> String {
    let heading = |id: u8, name: &str, sz: u32, color: &str, before: u32| format!(
        "<w:style w:type=\"paragraph\" w:styleId=\"Heading{id}\"><w:name w:val=\"{name}\"/><w:basedOn w:val=\"Normal\"/><w:next w:val=\"Normal\"/><w:pPr><w:keepNext/><w:spacing w:before=\"{before}\" w:after=\"120\"/></w:pPr><w:rPr><w:b/><w:color w:val=\"{color}\"/><w:sz w:val=\"{sz}\"/><w:szCs w:val=\"{sz}\"/></w:rPr></w:style>"
    );
    format!(
        "<?xml version=\"1.0\" encoding=\"UTF-8\" standalone=\"yes\"?>\
<w:styles xmlns:w=\"http://schemas.openxmlformats.org/wordprocessingml/2006/main\">\
<w:docDefaults><w:rPrDefault><w:rPr><w:rFonts w:ascii=\"Calibri\" w:hAnsi=\"Calibri\" w:eastAsia=\"Calibri\"/><w:sz w:val=\"22\"/><w:szCs w:val=\"22\"/></w:rPr></w:rPrDefault></w:docDefaults>\
<w:style w:type=\"paragraph\" w:default=\"1\" w:styleId=\"Normal\"><w:name w:val=\"Normal\"/><w:pPr><w:spacing w:after=\"120\" w:line=\"276\" w:lineRule=\"auto\"/></w:pPr></w:style>\
{}{}{}{}\
<w:style w:type=\"paragraph\" w:styleId=\"Title\"><w:name w:val=\"Title\"/><w:basedOn w:val=\"Normal\"/><w:pPr><w:jc w:val=\"center\"/><w:spacing w:after=\"240\"/></w:pPr><w:rPr><w:b/><w:sz w:val=\"52\"/><w:color w:val=\"1F3864\"/></w:rPr></w:style>\
<w:style w:type=\"paragraph\" w:styleId=\"Caption\"><w:name w:val=\"caption\"/><w:basedOn w:val=\"Normal\"/><w:rPr><w:i/><w:sz w:val=\"18\"/><w:color w:val=\"595959\"/></w:rPr></w:style>\
<w:style w:type=\"paragraph\" w:styleId=\"ListParagraph\"><w:name w:val=\"List Paragraph\"/><w:basedOn w:val=\"Normal\"/></w:style>\
</w:styles>",
        heading(1, "heading 1", 32, "1F3864", 240),
        heading(2, "heading 2", 26, "2E5395", 200),
        heading(3, "heading 3", 24, "2E5395", 160),
        heading(4, "heading 4", 22, "444444", 140),
    )
}

fn numbering_xml() -> String {
    let level = |ilvl: u8, fmt: &str, text: &str, font: &str, ind: u32| format!(
        "<w:lvl w:ilvl=\"{ilvl}\"><w:start w:val=\"1\"/><w:numFmt w:val=\"{fmt}\"/><w:lvlText w:val=\"{text}\"/><w:lvlJc w:val=\"left\"/>\
<w:pPr><w:ind w:left=\"{ind}\" w:hanging=\"360\"/></w:pPr><w:rPr><w:rFonts w:ascii=\"{font}\" w:hAnsi=\"{font}\" w:hint=\"default\"/></w:rPr></w:lvl>"
    );
    let bullet_levels = (0u8..=2)
        .map(|l| {
            let (txt, font, ind) = match l {
                0 => ("\u{2022}", "Symbol", 720),
                1 => ("o", "Courier New", 1152),
                _ => ("\u{2022}", "Symbol", 1584),
            };
            level(l, "bullet", txt, font, ind)
        })
        .collect::<String>();
    let num_levels = (0u8..=2)
        .map(|l| level(l, "decimal", &format!("%{}.", l + 1), "Calibri", 720 + 432 * l as u32))
        .collect::<String>();
    format!(
        "<?xml version=\"1.0\" encoding=\"UTF-8\" standalone=\"yes\"?>\
<w:numbering xmlns:w=\"http://schemas.openxmlformats.org/wordprocessingml/2006/main\">\
<w:abstractNum w:abstractNumId=\"0\">{bl}</w:abstractNum>\
<w:abstractNum w:abstractNumId=\"1\"><w:multiLevelType w:val=\"hybridMultilevel\"/>{nl}</w:abstractNum>\
<w:num w:numId=\"1\"><w:abstractNumId w:val=\"0\"/></w:num>\
<w:num w:numId=\"2\"><w:abstractNumId w:val=\"1\"/></w:num>\
</w:numbering>",
        bl = bullet_levels,
        nl = num_levels
    )
}

// ── read ────────────────────────────────────────────────────────────────────

/// Parsed content of an existing .docx.
#[derive(Debug, Default)]
pub struct DocxContent {
    pub paragraphs: Vec<(String, String)>, // (style, text)
    pub tables: Vec<Vec<Vec<String>>>,     // tables → rows → cells
}

/// Read paragraphs and tables from .docx bytes.
pub fn read(data: &[u8]) -> Result<DocxContent, String> {
    let z = ZipReader::open(data).map_err(|e| format!("not a docx (zip): {e}"))?;
    let doc = z.read("word/document.xml").map_err(|_| "missing word/document.xml — not a docx".to_string())?;
    let xml = String::from_utf8_lossy(&doc).into_owned();
    let mut p = Parser::new(&xml);
    let mut out = DocxContent::default();
    let mut style_stack: Vec<String> = Vec::new();
    let mut text_stack: Vec<String> = Vec::new();
    // table state
    let mut in_tbl = 0usize;
    let mut cur_row: Vec<String> = Vec::new();
    let mut cur_cell = String::new();
    let mut cur_tbl: Vec<Vec<String>> = Vec::new();
    let mut depth_p = 0usize;
    while let Some(ev) = p.next() {
        // <w:pStyle w:val="…"/> and friends are self-closing: treat Empty
        // exactly like Start (they never carry children we need to close).
        let ev = match ev {
            crate::xml::Event::Empty(name, attrs) => crate::xml::Event::Start(name, attrs),
            other => other,
        };
        match ev {
            crate::xml::Event::Start(name, attrs) => match name.as_str() {
                "w:p" => {
                    depth_p += 1;
                    if depth_p == 1 && in_tbl == 0 {
                        style_stack.push(String::new());
                        text_stack.push(String::new());
                    }
                }
                "w:tbl" => {
                    in_tbl += 1;
                    if in_tbl == 1 {
                        cur_tbl = Vec::new();
                    }
                }
                "w:tr" if in_tbl == 1 => cur_row = Vec::new(),
                "w:tc" if in_tbl == 1 => cur_cell = String::new(),
                "w:pStyle" => {
                    if let Some(v) = attrs.iter().find(|(k, _)| k == "w:val").map(|(_, v)| v.clone()) {
                        if in_tbl == 0 && depth_p == 1 && !style_stack.is_empty() {
                            style_stack.pop();
                            style_stack.push(v);
                        }
                    }
                }
                "w:t" => {
                    // text follows
                    let mut s = String::new();
                    loop {
                        match p.next() {
                            Some(crate::xml::Event::Text(t)) => s.push_str(&t),
                            Some(crate::xml::Event::End(n)) if n == "w:t" => break,
                            Some(crate::xml::Event::End(_)) | None => break,
                            _ => {}
                        }
                    }
                    if in_tbl == 0 && depth_p >= 1 && !text_stack.is_empty() {
                        let idx = text_stack.len() - 1;
                        text_stack[idx].push_str(&s);
                    } else if in_tbl >= 1 {
                        cur_cell.push_str(&s);
                    }
                }
                _ => {}
            },
            crate::xml::Event::End(name) => match name.as_str() {
                "w:p" => {
                    if in_tbl == 0 && depth_p == 1 {
                        let text = text_stack.pop().unwrap_or_default();
                        let style = style_stack.pop().unwrap_or_default();
                        if !text.trim().is_empty() || !style.is_empty() {
                            out.paragraphs.push((style, text));
                        }
                    }
                    depth_p = depth_p.saturating_sub(1);
                }
                "w:tc" if in_tbl == 1 => cur_row.push(cur_cell.clone()),
                "w:tr" if in_tbl == 1 => cur_tbl.push(std::mem::take(&mut cur_row)),
                "w:tbl" => {
                    in_tbl = in_tbl.saturating_sub(1);
                    if in_tbl == 0 && !cur_tbl.is_empty() {
                        out.tables.push(std::mem::take(&mut cur_tbl));
                    }
                }
                _ => {}
            },
            _ => {}
        }
    }
    Ok(out)
}

/// Read a .docx from a file path.
pub fn read_path(path: &str) -> Result<DocxContent, String> {
    let data = std::fs::read(path).map_err(|e| format!("docx {path}: {e}"))?;
    read(&data)
}

/// Render a read result as the JSON the tool returns.
pub fn content_to_json(c: &DocxContent) -> Value {
    json!({
        "paragraph_count": c.paragraphs.len(),
        "table_count": c.tables.len(),
        "paragraphs": c.paragraphs.iter().map(|(s, t)| json!({
            "style": s,
            "text": t,
        })).collect::<Vec<_>>(),
        "tables": c.tables.iter().map(|t| json!(t)).collect::<Vec<_>>(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn create_and_read_round_trip() {
        let meta = Meta { title: Some("Test doc".into()), author: Some("Roma".into()), subject: None };
        let blocks = vec![
            Block::Heading { level: 1, text: "Report".into() },
            Block::Para { text: "Hello **world** and *italic* and `code`.".into(), align: Align::Justify, size_pt: None, font: None, color: None },
            Block::Bullets { items: vec![("one".into(), 0), ("two".into(), 1)] },
            Block::Numbered { items: vec![("first".into(), 0), ("second".into(), 0)] },
            Block::Table { header: vec!["A".into(), "B".into()], rows: vec![vec!["1".into(), "2".into()], vec!["3".into(), "4".into()]], widths: None },
            Block::PageBreak,
            Block::Quote { text: "wisdom".into() },
            Block::Code { text: "fn main() {}".into() },
        ];
        let bytes = build(&meta, &blocks, true).expect("build docx");
        let c = read(&bytes).expect("read docx");
        assert!(c.paragraphs.iter().any(|(s, t)| s == "Heading1" && t == "Report"));
        assert!(c.paragraphs.iter().any(|(_, t)| t.contains("Hello ") && t.contains("world")));
        assert_eq!(c.tables.len(), 1);
        assert_eq!(c.tables[0][0], vec!["A", "B"]);
        assert_eq!(c.tables[0][2], vec!["3", "4"]);
        // valid zip that python-docx-style readers can open: check content types
        let z = ZipReader::open(&bytes).unwrap();
        assert!(z.read("[Content_Types].xml").is_ok());
        assert!(z.read("word/styles.xml").is_ok());
        assert!(z.read("word/numbering.xml").is_ok());
        assert!(z.read("word/footer1.xml").is_ok());
    }

    #[test]
    fn inline_formatting() {
        let runs = inline("a **b** c *d* `e`");
        assert_eq!(runs.len(), 6, "runs: {runs:?}");
        assert!(runs[1].bold);
        assert!(runs[3].italic);
        assert!(runs[5].code);
    }
}
