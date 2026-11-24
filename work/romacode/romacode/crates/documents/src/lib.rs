//! RomaCode built-in **documents engine** — Word, Excel, PowerPoint, PDF,
//! charts, canvas design and Figma, rebuilt in pure Rust (builtin native).
//!
//! Same posture as the vendored `db` / `tunnel` engines: no external
//! binaries, no Python, no C beyond what pure-Rust crates provide. Every
//! format is implemented from the file-format specs:
//!
//! * [`docx`] — OOXML WordprocessingML: create (headings, paragraphs with
//!   inline formatting, bullet/numbered lists, tables, images, page breaks,
//!   footers with page numbers) and read (paragraphs + tables + stats).
//! * [`xlsx`] — SpreadsheetML: create multi-sheet workbooks (typed cells,
//!   formulas, dates, column widths, bold headers, freeze panes, autofilter,
//!   merged cells), read cell values, patch cells in-place preserving every
//!   other zip part byte-identical, CSV conversion both ways.
//! * [`pptx`] — PresentationML: create themed decks (title, section, bullets,
//!   two-content, image, blank slides + speaker notes), read slide text.
//! * [`pdfgen`] / [`pdfread`] — PDF 1.7 writer (CJK-capable TTF embedding via
//!   Identity-H, tables, wrapped justified text, images, page numbers,
//!   cover pages) and reader (metadata, page counts, best-effort text
//!   extraction, lossless page merge/split via object-graph rewrite).
//! * [`charts`] — data charts rendered with the in-house rasterizer to PNG
//!   (+ SVG twin): line, area, bar, hbar, stacked, pie, donut, scatter,
//!   histogram, heatmap, radar, waterfall, boxplot.
//! * [`canvas`] — design engine for posters/cards/social art from a JSON
//!   layout spec → PNG (+ PDF export), gradients, shapes, wrapped text,
//!   images, pills, badges.
//! * [`figma`] — Figma REST API client (files, nodes, rendered images,
//!   comments, team components) over rustls.
//!
//! The agent-facing surface lives in [`tools`]: one JSON-schema tool
//! definition per capability plus a single dispatch function, consumed by
//! the brain (`crates/server/src/documents_bridge.rs`) and the rc-tools
//! framework.

pub mod canvas;
pub mod charts;
pub mod color;
pub mod docx;
pub mod figma;
pub mod fonts;
pub mod pdfgen;
pub mod pdfread;
pub mod pptx;
pub mod raster;
pub mod tools;
pub mod xlsx;
pub mod xml;
pub mod zipio;

/// Engine version, surfaced by `documents_status` / skills / doctor output.
pub const VERSION: &str = "1.0.0";
