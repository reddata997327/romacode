//! Generates the sample deliverable pack exercising every documents tool.
use rc_docs::tools::execute;
use serde_json::json;
use std::path::Path;

fn main() {
    let base = Path::new("/home/z/my-project/work/samples");
    std::fs::create_dir_all(base).unwrap();
    let run = |name: &str, input: serde_json::Value| {
        match execute(name, &input, base) {
            Ok(v) => println!("{name}: ok {}", serde_json::to_string(&v).unwrap_or_default()),
            Err(e) => println!("{name}: ERROR {e}"),
        }
    };

    // 5. Chart
    run("chart_render", json!({
        "type": "bar", "title": "Revenue by quarter", "labels": ["Q1", "Q2", "Q3", "Q4"],
        "series": [{"name": "2023", "values": [0.9, 1.0, 1.2, 1.25]}, {"name": "2024", "values": [1.1, 1.3, 1.4, 1.6]}],
        "y_label": "M USD", "legend": true, "width": 760, "height": 460,
        "out_png": "revenue.png", "out_svg": "revenue.svg"
    }));

    // 1. Word report
    run("docx_create", json!({
        "path": "RomaCode_Documents_Report.docx", "title": "RomaCode Documents Engine",
        "author": "RomaCode", "page_numbers": true,
        "blocks": [
            {"type": "h1", "text": "1. Overview"},
            {"type": "p", "text": "The **RomaCode documents engine** (`rc-docs`) is a vendored pure-Rust engine shipped inside the brain. It writes and reads Word, Excel, PowerPoint and PDF files, renders data charts, designs posters and talks to the Figma REST API — with **no Python**, no LibreOffice and no external binaries. Every tool passes the same security gate as the rest of the agent."},
            {"type": "bullets", "items": ["Word: headings, lists, shaded tables, images, TOC field", "Excel: typed cells, formulas, freeze panes, autofilter", "PowerPoint: themed 16:9 decks with speaker notes", "PDF: embedded TrueType fonts (CJK capable), merge and split"]},
            {"type": "h2", "text": "2. Quarterly numbers"},
            {"type": "table", "header": ["Metric", "Q3", "Q4", "Δ"], "rows": [["Revenue", "1.2M", "1.4M", "+17%"], ["Active users", "8,400", "9,900", "+18%"], ["Churn", "2.1%", "1.7%", "-19%"]], "widths": [2, 1, 1, 1]},
            {"type": "image", "path": "/home/z/my-project/work/samples/revenue.png", "width_px": 430, "caption": "Figure 1 — Revenue by quarter (rendered by chart_render)"},
            {"type": "quote", "text": "Everything here was produced by builtin native tools — one binary, zero dependencies."},
            {"type": "code", "text": "romacode-server serve   # the brain\nromacode               # the screen (TUI)"}
        ]
    }));

    // 2. Excel workbook
    run("xlsx_create", json!({
        "path": "RomaCode_Sales.xlsx", "sheets": [
            {"name": "Sales", "rows": [["Region", "Units", "Unit price", "When"],
                                        ["North", 120, 9.5, "date:2024-01-05"],
                                        ["South", 80, 9.5, "date:2024-01-12"],
                                        ["East", 150, 8.75, "date:2024-02-02"]],
             "cells": {"E1": "Total", "E2": "=B2*C2", "E3": "=B3*C3", "E4": "=B4*C4"},
             "widths": {"A": 12, "D": 14}, "freeze_top": true, "autofilter": "A1:D4"},
            {"name": "Summary", "cells": {"A1": "Grand total", "B1": "=SUM(Sales!E2:E4)"}}
        ]
    }));

    // 3. PowerPoint deck
    run("pptx_create", json!({
        "path": "RomaCode_Deck.pptx", "title": "RomaCode Documents", "author": "RomaCode",
        "accent1": "#1F3864",
        "slides": [
            {"type": "title", "title": "Documents, natively", "subtitle": "rc-docs 1.0 — pure Rust"},
            {"type": "bullets", "title": "What ships in the binary", "items": ["Word · Excel · PowerPoint · PDF", "13 chart types → PNG + SVG", "Canvas design engine", "Figma REST client"], "notes": "Mention the security gate: every tool passes authorize()."},
            {"type": "two_content", "title": "Write vs Read", "left": ["docx_create", "xlsx_create", "pptx_create", "pdf_create"], "right": ["docx_read", "xlsx_read", "pptx_read", "pdf_read"]},
            {"type": "image", "title": "Chart slide", "path": "/home/z/my-project/work/samples/revenue.png", "caption": "chart_render → PNG"},
            {"type": "section", "title": "Try it: /documents status"}
        ]
    }));

    // 4. PDF report
    run("pdf_create", json!({
        "path": "RomaCode_Overview.pdf", "page_size": "a4", "accent": "#1F3864",
        "title": "RomaCode Documents Engine", "author": "RomaCode",
        "blocks": [
            {"type": "cover", "title": "RomaCode Documents Engine", "subtitle": "Word · Excel · PowerPoint · PDF · Charts · Canvas · Figma", "author": "RomaCode", "date": "2026-11-24", "accent": "#1F3864"},
            {"type": "h1", "text": "1. Built-in native"},
            {"type": "p", "text": "The documents engine is vendored inside the RomaCode workspace as first-party crates and compiled into the brain. Tool definitions are advertised through the normal registry, so both agent engines see them, and the same security gate classifies every call. Reads are read-class; writes are edit-class with the target path."},
            {"type": "h2", "text": "2. Numbers"},
            {"type": "table", "header": ["Format", "Write", "Read", "Notes"], "rows": [["docx", "yes", "yes", "TOC field, tables, images"], ["xlsx", "yes", "yes", "in-place patch, CSV"], ["pptx", "yes", "yes", "notes, theming"], ["pdf", "yes", "yes", "CJK fonts, merge/split"]], "widths": [1, 1, 1, 3]},
            {"type": "image", "path": "/home/z/my-project/work/samples/revenue.png", "width_pt": 300, "caption": "Figure 1 — chart_render output"},
            {"type": "pagebreak"},
            {"type": "h1", "text": "3. Getting started"},
            {"type": "numbered", "items": ["/documents status — doctor", "Ask for a report — the agent picks the right tool", "Read back and verify — every skill ends with verification"]},
            {"type": "rule"}
        ]
    }));

    // 6. Canvas poster
    run("canvas_design", json!({
        "preset": "square", "out_png": "poster.png",
        "background": {"type": "gradient", "from": "#0F172A", "to": "#1F3864", "direction": "vertical"},
        "elements": [
            {"kind": "rect", "x": "6%", "y": "8%", "w": "88%", "h": "84%", "fill": "#FFFFFF", "radius": 28, "opacity": 0.96},
            {"kind": "pill", "x": "10%", "y": "13%", "text": "ROMACODE", "size": 22, "bg": "#1F3864"},
            {"kind": "text", "x": "10%", "y": "26%", "text": "Documents, natively", "size": 72, "weight": "bold", "max_width": "80%"},
            {"kind": "text", "x": "10%", "y": "40%", "text": "Word, Excel, PowerPoint, PDF, charts, canvas and Figma — rebuilt in pure Rust and shipped inside the binary.", "size": 30, "color": "#475569", "max_width": "80%"},
            {"kind": "line", "x": "10%", "y": "86%", "w": "80%", "color": "#1F3864", "width": 4},
            {"kind": "text", "x": "10%", "y": "91%", "text": "rc-docs 1.0", "size": 24, "color": "#64748B"},
            {"kind": "circle", "cx": "88%", "cy": "90%", "r": 26, "fill": "#F59E0B"}
        ]
    }));

    // 7. xlsx round-trip: edit + convert
    run("xlsx_edit", json!({"path": "RomaCode_Sales.xlsx", "sheet": "Sales", "cells": {"B5": 60, "C5": 9.9, "A5": "West"}}));
    run("xlsx_convert", json!({"action": "csv_to_xlsx", "csv": "city,pop\nBerlin,3.6M\nTokyo,14M", "out": "from_csv.xlsx"}));

    // 8. PDF split demo
    run("pdf_split", json!({"path": "RomaCode_Overview.pdf", "from": 3, "to": 3, "out": "Overview_p3.pdf"}));

    run("documents_status", json!({}));
    println!("samples done");
}
