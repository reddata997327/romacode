//! The agent-facing framework surface: one JSON tool definition per
//! capability plus a single dispatch function. The brain
//! (`crates/server/src/documents_bridge.rs`) advertises these through the
//! tool registry, the security gate classifies them, and the rc-tools
//! framework can wrap them as a `ToolSource`.

use serde_json::{json, Value};
use std::path::Path;

/// Every tool definition (name, description, input schema).
pub fn definitions() -> Vec<Value> {
    let def = |name: &str, desc: &str, props: Value, req: &[&str]| {
        json!({
            "name": name,
            "description": desc,
            "input_schema": { "type": "object", "properties": props, "required": req },
        })
    };
    vec![
        def("docx_create",
            "Create a Word .docx document (built-in pure-Rust documents engine). Blocks: h1-h4, paragraphs (inline **bold** / *italic* / __underline__ / `code`), bullets, numbered lists, grid tables with a styled header, images (png/jpg/gif/bmp), page breaks, quote, code, a Word TOC field, and a footer with page numbers.",
            json!({
                "path": { "type": "string", "description": "Output .docx path (relative to the project or absolute)" },
                "title": { "type": "string", "description": "Document metadata title" },
                "author": { "type": "string", "description": "Document metadata author" },
                "page_numbers": { "type": "boolean", "description": "Add a centered page-number footer (default true)" },
                "blocks": { "type": "array", "description": "Content blocks, e.g. [{\"type\":\"h1\",\"text\":\"...\"},{\"type\":\"p\",\"text\":\"...\"},{\"type\":\"bullets\",\"items\":[\"...\"]},{\"type\":\"table\",\"header\":[...],\"rows\":[[...]]}]" }
            }),
            &["path", "blocks"]),
        def("docx_read",
            "Read a Word .docx: every paragraph with its style, every table grid, plus counts.",
            json!({
                "path": { "type": "string", "description": "The .docx file to read" }
            }),
            &["path"]),
        def("xlsx_create",
            "Create an Excel .xlsx workbook (built-in pure-Rust documents engine): multi-sheet, typed cells (text/number/bool/formula/date via \"date:YYYY-MM-DD\"), bold header row with fill, column widths, freeze top row, autofilter, merged cells.",
            json!({
                "path": { "type": "string", "description": "Output .xlsx path" },
                "sheets": { "type": "array", "description": "[{ \"name\": \"Sheet1\", \"rows\": [[cell,...],...], \"cells\": {\"B2\": \"=A1*2\"}, \"widths\": {\"A\": 24}, \"freeze_top\": true, \"autofilter\": \"A1:C9\", \"merges\": [\"A1:C1\"] }]" }
            }),
            &["path", "sheets"]),
        def("xlsx_read",
            "Read an Excel .xlsx: every sheet with cell values, detected formulas and dates.",
            json!({
                "path": { "type": "string", "description": "The .xlsx file to read" },
                "max_chars": { "type": "integer", "description": "Output cap in characters (default 40000)" }
            }),
            &["path"]),
        def("xlsx_edit",
            "Edit an existing .xlsx in place: only the target sheet's XML is rewritten, every other part (styles, charts, images) is preserved byte-identical. Delete a cell by passing null.",
            json!({
                "path": { "type": "string", "description": "The .xlsx file to edit (overwritten)" },
                "sheet": { "type": "string", "description": "Sheet name (default: first sheet)" },
                "cells": { "type": "object", "description": "{\"B2\": 42, \"C3\": \"text\", \"D4\": \"=SUM(A1:A9)\", \"E5\": null}" }
            }),
            &["path", "cells"]),
        def("xlsx_convert",
            "Convert CSV ↔ xlsx. action=csv_to_xlsx writes a workbook from CSV text (numbers auto-typed, first row as header); action=xlsx_to_csv dumps the first (or named) sheet as CSV.",
            json!({
                "action": { "type": "string", "enum": ["csv_to_xlsx", "xlsx_to_csv"] },
                "csv": { "type": "string", "description": "csv_to_xlsx: CSV text (delimiter option below)" },
                "path": { "type": "string", "description": "xlsx_to_csv: the .xlsx to dump; csv_to_xlsx: the output path" },
                "out": { "type": "string", "description": "csv_to_xlsx: output .xlsx path (alias of path)" },
                "sheet": { "type": "string", "description": "xlsx_to_csv: sheet name (default first)" },
                "delimiter": { "type": "string", "description": "CSV delimiter character (default ',')" }
            }),
            &["action"]),
        def("pptx_create",
            "Create a PowerPoint .pptx deck (built-in pure-Rust documents engine): themed 16:9 slides — title, section, bullets, two_content, image, blank — with optional speaker notes and configurable accent colors.",
            json!({
                "path": { "type": "string", "description": "Output .pptx path" },
                "title": { "type": "string", "description": "Deck metadata title" },
                "author": { "type": "string", "description": "Deck metadata author" },
                "accent1": { "type": "string", "description": "Theme accent hex, e.g. #1F77B4 (accent1..accent6 accepted)" },
                "slides": { "type": "array", "description": "[{\"type\":\"title\",\"title\":...,\"subtitle\":...},{\"type\":\"bullets\",\"title\":...,\"items\":[\"...\",{\"text\":\"sub\",\"level\":1}],\"notes\":\"speaker notes\"},{\"type\":\"two_content\",\"title\":...,\"left\":[...],\"right\":[...]},{\"type\":\"image\",\"title\":...,\"path\":\"...\",\"caption\":...},{\"type\":\"section\",\"title\":...}]" }
            }),
            &["path", "slides"]),
        def("pptx_read",
            "Read a PowerPoint .pptx: slide count, per-slide text and speaker notes.",
            json!({
                "path": { "type": "string", "description": "The .pptx file to read" }
            }),
            &["path"]),
        def("pdf_create",
            "Create a PDF report (built-in pure-Rust documents engine, embedded TrueType fonts — CJK capable): cover page, headings, wrapped/justified paragraphs, tables with striped rows and styled headers, images (png/jpeg), bullets, numbered lists, rules, spacers, page numbers.",
            json!({
                "path": { "type": "string", "description": "Output .pdf path" },
                "page_size": { "type": "string", "enum": ["a4", "letter"], "description": "Default a4" },
                "margin": { "type": "number", "description": "Page margin in pt (default 56)" },
                "page_numbers": { "type": "boolean", "description": "Footer \"Page N of M\" (default true)" },
                "accent": { "type": "string", "description": "Accent color for headings/tables, e.g. #1F77B4" },
                "title": { "type": "string", "description": "Metadata title" },
                "author": { "type": "string", "description": "Metadata author" },
                "blocks": { "type": "array", "description": "[{\"type\":\"cover\",\"title\":...,\"subtitle\":...,\"author\":...,\"date\":...,\"accent\":\"#...\"},{\"type\":\"h1\",\"text\":...},{\"type\":\"p\",\"text\":...,\"align\":\"justify\"},{\"type\":\"table\",\"header\":[...],\"rows\":[[...]],\"widths\":[2,1]},{\"type\":\"image\",\"path\":...,\"width_pt\":300,\"caption\":...},{\"type\":\"bullets\",\"items\":[...]},{\"type\":\"rule\"},{\"type\":\"spacer\",\"pt\":12},{\"type\":\"pagebreak\"}]" }
            }),
            &["path", "blocks"]),
        def("pdf_read",
            "Read a PDF: page count, page sizes, metadata (title/author/producer) and best-effort text extraction (FlateDecode streams, ToUnicode CMaps, UTF-16 strings). Encrypted or image-only PDFs are reported honestly.",
            json!({
                "path": { "type": "string", "description": "The .pdf to read" },
                "max_chars": { "type": "integer", "description": "Text cap in characters (default 20000)" }
            }),
            &["path"]),
        def("pdf_merge",
            "Merge multiple PDFs into one, preserving every page's resources (fonts, images) via object-graph rewriting.",
            json!({
                "paths": { "type": "array", "description": "Input .pdf files in order" },
                "out": { "type": "string", "description": "Output .pdf path" }
            }),
            &["paths", "out"]),
        def("pdf_split",
            "Extract a page range (1-based, inclusive) into a new PDF.",
            json!({
                "path": { "type": "string", "description": "Input .pdf" },
                "from": { "type": "integer", "description": "First page (1-based)" },
                "to": { "type": "integer", "description": "Last page (inclusive; default = from)" },
                "out": { "type": "string", "description": "Output .pdf path" }
            }),
            &["path", "from", "out"]),
        def("chart_render",
            "Render a data chart to PNG (+ optional SVG twin) with the built-in pure-Rust rasterizer: line, area, bar, hbar, stacked_bar, pie, donut, scatter, histogram, heatmap, radar, waterfall, boxplot. CJK-capable text via system fonts.",
            json!({
                "type": { "type": "string", "description": "Chart type (see description)" },
                "title": { "type": "string", "description": "Chart title" },
                "x_label": { "type": "string", "description": "X axis label" },
                "y_label": { "type": "string", "description": "Y axis label" },
                "labels": { "type": "array", "description": "Category labels" },
                "series": { "type": "array", "description": "[{\"name\":\"Revenue\",\"values\":[1,2,3]}] or [{\"name\":\"p\",\"points\":[[x,y],...]}] for scatter" },
                "stacked": { "type": "boolean", "description": "stacked_bar: stack the series" },
                "width": { "type": "integer", "description": "Pixel width (default 800)" },
                "height": { "type": "integer", "description": "Pixel height (default 500)" },
                "legend": { "type": "boolean", "description": "Show legend (default true)" },
                "bins": { "type": "integer", "description": "histogram: number of bins (default 20)" },
                "palette": { "type": "array", "description": "Custom colors, e.g. [\"#1F77B4\", \"#FF7F0E\"]" },
                "out_png": { "type": "string", "description": "Output .png path" },
                "out_svg": { "type": "string", "description": "Optional .svg twin path" }
            }),
            &["type", "out_png"]),
        def("canvas_design",
            "Design a poster, card, banner or social graphic from a JSON layout (canvas-design engine): gradient/solid backgrounds, rounded rects, circles, lines, polygons, wrapped text, pills/badges, images; presets square/story/poster/a4/a2/card/slide; PNG output plus optional PDF export.",
            json!({
                "preset": { "type": "string", "enum": ["square", "story", "poster", "a4", "a2", "card", "slide"], "description": "Canvas preset (or set width/height)" },
                "width": { "type": "integer", "description": "Canvas width in px" },
                "height": { "type": "integer", "description": "Canvas height in px" },
                "background": { "type": "object", "description": "{\"type\":\"gradient\",\"from\":\"#0F172A\",\"to\":\"#3B82F6\",\"direction\":\"vertical\"} or {\"type\":\"solid\",\"color\":\"#FFF\"}" },
                "elements": { "type": "array", "description": "[{\"kind\":\"rect\",\"x\":\"6%\",\"y\":\"8%\",\"w\":\"88%\",\"h\":\"84%\",\"fill\":\"#FFF\",\"radius\":24},{\"kind\":\"text\",\"x\":\"10%\",\"y\":\"26%\",\"text\":...,\"size\":64,\"weight\":\"bold\",\"max_width\":\"80%\"},{\"kind\":\"pill\",...},{\"kind\":\"image\",\"path\":...},{\"kind\":\"circle\",\"cx\":...,\"cy\":...,\"r\":...},{\"kind\":\"line\"|\"polygon\"}]" },
                "out_png": { "type": "string", "description": "Output .png path" },
                "out_pdf": { "type": "string", "description": "Optional .pdf export path" }
            }),
            &["out_png"]),
        def("figma_file",
            "Fetch a Figma file structure (built-in Figma REST client): name, thumbnail, node tree trimmed to `depth` levels. Needs FIGMA_TOKEN (env) or token input.",
            json!({
                "key": { "type": "string", "description": "Figma file key (from the file URL)" },
                "depth": { "type": "integer", "description": "Tree depth 1-10 (default 4)" },
                "token": { "type": "string", "description": "Optional explicit token (else FIGMA_TOKEN env)" }
            }),
            &["key"]),
        def("figma_node",
            "Fetch specific Figma node subtrees by ids (with fills, text content, sizes).",
            json!({
                "key": { "type": "string", "description": "Figma file key" },
                "ids": { "type": "string", "description": "Comma-separated node ids, e.g. 12:34,12:35" },
                "depth": { "type": "integer", "description": "Tree depth (default 6)" },
                "token": { "type": "string", "description": "Optional token override" }
            }),
            &["key", "ids"]),
        def("figma_images",
            "Render Figma nodes to PNG/SVG and download them into `dir` (built-in Figma REST client).",
            json!({
                "key": { "type": "string", "description": "Figma file key" },
                "ids": { "type": "string", "description": "Comma-separated node ids to render" },
                "format": { "type": "string", "enum": ["png", "svg"], "description": "Render format (default png)" },
                "scale": { "type": "number", "description": "PNG scale 0.01-4 (default 2)" },
                "dir": { "type": "string", "description": "Output directory (default figma/)" },
                "token": { "type": "string", "description": "Optional token override" }
            }),
            &["key", "ids"]),
        def("figma_comments",
            "List comments on a Figma file, or post one with `message`.",
            json!({
                "key": { "type": "string", "description": "Figma file key" },
                "message": { "type": "string", "description": "Post this message as a new comment" },
                "token": { "type": "string", "description": "Optional token override" }
            }),
            &["key"]),
        def("figma_me",
            "Verify the Figma token: returns the authenticated user (name, email, handle).",
            json!({
                "token": { "type": "string", "description": "Optional token override (else FIGMA_TOKEN env)" }
            }),
            &[]),
        def("documents_status",
            "Diagnostics for the built-in documents engine: version, tool list, discovered fonts (which file text rendering will use), Figma token presence.",
            json!({}),
            &[]),
    ]
}

/// Dispatch one tool call. `base` is the project root: relative output paths
/// are resolved against it.
pub fn execute(cfg_name: &str, input: &Value, base: &Path) -> Result<Value, String> {
    match cfg_name {
        "docx_create" => {
            let meta = crate::docx::Meta {
                title: input["title"].as_str().map(String::from),
                author: input["author"].as_str().map(String::from),
                subject: input["subject"].as_str().map(String::from),
            };
            let blocks = crate::docx::blocks_from_json(&input["blocks"])?;
            let bytes = crate::docx::build(&meta, &blocks, input["page_numbers"].as_bool().unwrap_or(true))?;
            let path = resolve(base, input["path"].as_str().ok_or("needs `path`")?);
            write_file(&path, &bytes)?;
            Ok(json!({ "path": path, "bytes": bytes.len(), "blocks": blocks.len() }))
        }
        "docx_read" => {
            let path = resolve(base, input["path"].as_str().ok_or("needs `path`")?);
            let content = crate::docx::read_path(&path)?;
            Ok(crate::docx::content_to_json(&content))
        }
        "xlsx_create" => {
            let sheets = crate::xlsx::sheets_from_json(&input["sheets"])?;
            let bytes = crate::xlsx::build(&sheets);
            let path = resolve(base, input["path"].as_str().ok_or("needs `path`")?);
            write_file(&path, &bytes)?;
            Ok(json!({ "path": path, "bytes": bytes.len(), "sheets": sheets.len() }))
        }
        "xlsx_read" => {
            let path = resolve(base, input["path"].as_str().ok_or("needs `path`")?);
            let content = crate::xlsx::read_path(&path)?;
            Ok(crate::xlsx::content_to_json(&content, input["max_chars"].as_u64().unwrap_or(40_000) as usize))
        }
        "xlsx_edit" => {
            let path = resolve(base, input["path"].as_str().ok_or("needs `path`")?);
            let data = std::fs::read(&path).map_err(|e| format!("xlsx {path}: {e}"))?;
            let op = crate::xlsx::patch_from_json(input)?;
            let patched = crate::xlsx::patch(&data, &op)?;
            write_file(&path, &patched)?;
            Ok(json!({ "path": path, "bytes": patched.len(), "cells_set": op.set.len() }))
        }
        "xlsx_convert" => {
            let action = input["action"].as_str().unwrap_or("");
            match action {
                "csv_to_xlsx" => {
                    let csv = input["csv"].as_str().ok_or("csv_to_xlsx needs `csv` text")?;
                    let delim = input["delimiter"].as_str().and_then(|s| s.chars().next()).unwrap_or(',');
                    let out = resolve(base, input["out"].as_str().or_else(|| input["path"].as_str()).ok_or("csv_to_xlsx needs `out`")?);
                    let sheet = input["sheet"].as_str().unwrap_or("Sheet1");
                    let bytes = crate::xlsx::csv_to_xlsx(csv, sheet, delim);
                    write_file(&out, &bytes)?;
                    Ok(json!({ "out": out, "bytes": bytes.len() }))
                }
                "xlsx_to_csv" => {
                    let path = resolve(base, input["path"].as_str().ok_or("xlsx_to_csv needs `path`")?);
                    let content = crate::xlsx::read_path(&path)?;
                    let delim = input["delimiter"].as_str().and_then(|s| s.chars().next()).unwrap_or(',');
                    let rows: Vec<Vec<String>> = content
                        .sheets
                        .iter()
                        .find(|(name, _)| input["sheet"].as_str().map(|s| name == s).unwrap_or(true))
                        .map(|(_, rows)| {
                            grid(rows).iter().map(|r| r.to_vec()).collect()
                        })
                        .ok_or("sheet not found")?;
                    let csv = crate::xlsx::rows_to_csv_delim(&rows, delim);
                    Ok(json!({ "path": path, "csv": csv }))
                }
                other => Err(format!("unknown action {other:?} — use csv_to_xlsx or xlsx_to_csv")),
            }
        }
        "pptx_create" => {
            let deck = crate::pptx::deck_from_json(input)?;
            let bytes = crate::pptx::build(&deck)?;
            let path = resolve(base, input["path"].as_str().ok_or("needs `path`")?);
            write_file(&path, &bytes)?;
            Ok(json!({ "path": path, "bytes": bytes.len(), "slides": deck.slides.len() }))
        }
        "pptx_read" => {
            let path = resolve(base, input["path"].as_str().ok_or("needs `path`")?);
            let content = crate::pptx::read_path(&path)?;
            Ok(crate::pptx::content_to_json(&content))
        }
        "pdf_create" => {
            let meta = crate::pdfgen::Meta {
                title: input["title"].as_str().unwrap_or("").to_string(),
                author: input["author"].as_str().unwrap_or("").to_string(),
                subject: input["subject"].as_str().unwrap_or("").to_string(),
            };
            let blocks = crate::pdfgen::blocks_from_json(&input["blocks"])?;
            let page = match input["page_size"].as_str() {
                Some(s) => crate::pdfgen::PageSize::parse(s).ok_or_else(|| format!("bad page_size {s:?} (a4|letter)"))?,
                None => crate::pdfgen::PageSize::A4,
            };
            let accent = crate::color::Rgba::parse_or(input["accent"].as_str().unwrap_or("#1F77B4"), crate::color::Rgba::new(31, 119, 180));
            let bytes = crate::pdfgen::build_document(&meta, &blocks, page, input["margin"].as_f64().unwrap_or(56.0), input["page_numbers"].as_bool().unwrap_or(true), accent)?;
            let path = resolve(base, input["path"].as_str().ok_or("needs `path`")?);
            write_file(&path, &bytes)?;
            Ok(json!({ "path": path, "bytes": bytes.len(), "blocks": blocks.len() }))
        }
        "pdf_read" => {
            let path = resolve(base, input["path"].as_str().ok_or("needs `path`")?);
            let data = std::fs::read(&path).map_err(|e| format!("pdf {path}: {e}"))?;
            let mut info = crate::pdfread::read(&data)?;
            let cap = input["max_chars"].as_u64().unwrap_or(20_000) as usize;
            if info.text.chars().count() > cap {
                info.text = info.text.chars().take(cap).collect();
                info.extraction_note.push_str(" (text truncated)");
            }
            Ok(json!({
                "path": path,
                "page_count": info.page_count,
                "page_sizes": info.page_sizes.iter().map(|(w, h)| json!({"w": w, "h": h})).collect::<Vec<_>>(),
                "title": info.title,
                "author": info.author,
                "producer": info.producer,
                "encrypted": info.encrypted,
                "text": info.text,
                "extraction": info.extraction_note,
            }))
        }
        "pdf_merge" => {
            let paths: Vec<Vec<u8>> = input["paths"]
                .as_array()
                .ok_or("needs `paths` (array of pdf files)")?
                .iter()
                .map(|p| std::fs::read(resolve(base, p.as_str().unwrap_or(""))))
                .collect::<Result<Vec<_>, _>>()
                .map_err(|e| format!("read: {e}"))?;
            let merged = crate::pdfread::merge(&paths)?;
            let out = resolve(base, input["out"].as_str().ok_or("needs `out`")?);
            write_file(&out, &merged)?;
            Ok(json!({ "out": out, "bytes": merged.len(), "inputs": paths.len() }))
        }
        "pdf_split" => {
            let path = resolve(base, input["path"].as_str().ok_or("needs `path`")?);
            let data = std::fs::read(&path).map_err(|e| format!("pdf {path}: {e}"))?;
            let from = input["from"].as_u64().unwrap_or(1) as usize;
            let to = input["to"].as_u64().unwrap_or(from as u64) as usize;
            let part = crate::pdfread::split(&data, from, to)?;
            let out = resolve(base, input["out"].as_str().ok_or("needs `out`")?);
            write_file(&out, &part)?;
            Ok(json!({ "out": out, "bytes": part.len(), "pages": format!("{from}-{to}") }))
        }
        "chart_render" => crate::charts::execute(input, base),
        "canvas_design" => crate::canvas::execute(input, base),
        "figma_file" => crate::figma::file(input),
        "figma_node" => crate::figma::node(input),
        "figma_images" => crate::figma::images(input, base),
        "figma_comments" => crate::figma::comments(input),
        "figma_me" => crate::figma::me(input),
        "documents_status" => {
            let fonts = crate::fonts::default_family().map(|fp| {
                json!({
                    "regular": fp.regular.to_string_lossy(),
                    "bold": fp.bold.as_ref().map(|b| b.to_string_lossy()),
                })
            });
            let tools: Vec<String> = definitions().iter().filter_map(|d| d["name"].as_str().map(String::from)).collect();
            Ok(json!({
                "engine": format!("rc-docs {}", crate::VERSION),
                "tools": tools,
                "font": fonts,
                "figma_token": std::env::var("FIGMA_TOKEN").is_ok(),
                "note": "pure-Rust builtin: no Python, no LibreOffice, no external binaries",
            }))
        }
        other => Err(format!("unknown documents tool {other:?}")),
    }
}

/// Does this name belong to the documents engine? (dispatch helper)
pub fn owns(name: &str) -> bool {
    definitions().iter().any(|d| d["name"] == name)
}

fn write_file(path: &str, bytes: &[u8]) -> Result<(), String> {
    if let Some(dir) = Path::new(path).parent() {
        let _ = std::fs::create_dir_all(dir);
    }
    std::fs::write(path, bytes).map_err(|e| format!("write {path}: {e}"))
}

pub(crate) fn resolve(base: &Path, p: &str) -> String {
    let path = Path::new(p);
    if path.is_absolute() {
        p.to_string()
    } else {
        base.join(path).to_string_lossy().into_owned()
    }
}

/// Rows of cell values (text) from parsed xlsx cells.
fn grid(rows: &[Vec<crate::xlsx::CellValue>]) -> Vec<Vec<String>> {
    let mut out = Vec::new();
    for row in rows {
        let mut r = Vec::new();
        for cell in row {
            r.push(cell.text.clone());
        }
        out.push(r);
    }
    // pad rows to equal length
    let width = out.iter().map(|r| r.len()).max().unwrap_or(0);
    for r in out.iter_mut() {
        while r.len() < width {
            r.push(String::new());
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_tool_definition_is_valid_and_dispatchable_name() {
        let mut seen = std::collections::HashSet::new();
        for d in definitions() {
            let name = d["name"].as_str().unwrap().to_string();
            assert!(seen.insert(name.clone()), "duplicate {name}");
            assert!(!d["description"].as_str().unwrap_or("").is_empty());
            assert_eq!(d["input_schema"]["type"], "object");
            assert!(owns(&name));
        }
        assert!(!owns("bash"));
    }

    #[test]
    fn docx_tool_round_trip() {
        let dir = std::env::temp_dir().join("rcdocs_tools_test");
        let _ = std::fs::create_dir_all(&dir);
        let out = dir.join("t.docx").to_string_lossy().into_owned();
        let input = json!({
            "path": out,
            "title": "T",
            "blocks": [
                {"type": "h1", "text": "Head"},
                {"type": "p", "text": "Body **bold** text."},
                {"type": "bullets", "items": ["a", "b"]}
            ]
        });
        let r = execute("docx_create", &input, Path::new("/")).unwrap();
        assert!(r["bytes"].as_u64().unwrap() > 500);
        let read = execute("docx_read", &json!({"path": out}), Path::new("/")).unwrap();
        // heading + paragraph + two bullet items
        assert_eq!(read["paragraph_count"].as_u64().unwrap(), 4);
    }

    #[test]
    fn status_lists_tools() {
        let r = execute("documents_status", &json!({}), Path::new("/")).unwrap();
        assert!(r["tools"].as_array().unwrap().len() >= 17);
    }
}
