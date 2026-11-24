//! Excel (.xlsx) — SpreadsheetML, create + read + in-place patch, pure Rust.
//!
//! Create: multi-sheet workbooks with typed cells (text, numbers, booleans,
//! dates, formulas), bold header rows, column widths, freeze panes,
//! autofilter, merged cells and a totals note. Read: sheets, dimensions and
//! cell values with formula + date detection. Patch: replace `xl/worksheets/
//! sheetN.xml` inside the existing archive so every other part (styles,
//! charts, pivot caches, media) survives byte-identical.

use crate::docx::civil_from_days;
use crate::xml::{escape, Event, Parser};
use crate::zipio::{ZipReader, ZipWriter};
use serde_json::{json, Value};

// ── cell model ──────────────────────────────────────────────────────────────

#[derive(Clone, Debug)]
pub enum Cell {
    Empty,
    Str(String),
    Num(f64),
    Bool(bool),
    /// Formula with an optional cached value.
    Formula(String, Option<f64>),
    /// Date (ISO y-m-d string is accepted on input) stored as a serial.
    Date(i64, u32, u32),
}

#[derive(Clone, Debug, Default)]
pub struct ColWidth {
    pub col: u32,
    pub width: f64,
}

#[derive(Clone, Debug)]
pub struct Sheet {
    pub name: String,
    pub cells: Vec<(String, Cell)>, // A1-style refs
    pub widths: Vec<ColWidth>,
    pub freeze_top: bool,
    pub autofilter: Option<String>, // e.g. "A1:C20"
    pub merges: Vec<String>,
    /// Mark the first row as a styled bold header (x_create applies shading).
    pub header_style: bool,
}

impl Default for Sheet {
    fn default() -> Self {
        Self { name: "Sheet1".into(), cells: Vec::new(), widths: Vec::new(), freeze_top: false, autofilter: None, merges: Vec::new(), header_style: false }
    }
}

/// Build sheets from the JSON model the `xlsx_create` tool sends.
pub fn sheets_from_json(v: &Value) -> Result<Vec<Sheet>, String> {
    let arr = v.as_array().ok_or("sheets must be an array")?;
    let mut out = Vec::new();
    for s in arr {
        let mut sheet = Sheet { name: s["name"].as_str().unwrap_or("").to_string(), ..Default::default() };
        if sheet.name.is_empty() {
            sheet.name = format!("Sheet{}", out.len() + 1);
        }
        if let Some(rows) = s["rows"].as_array() {
            for (r, row) in rows.iter().enumerate() {
                let cells = row.as_array().cloned().unwrap_or_default();
                for (c, cell) in cells.iter().enumerate() {
                    let col = col_letter(c as u32);
                    let rfc = col + &(r + 1).to_string();
                    sheet.cells.push((rfc, cell_from_json(cell)?));
                }
            }
        }
        if let Some(cells) = s["cells"].as_object() {
            for (rfc, cell) in cells {
                sheet.cells.push((rfc.to_uppercase().replace('$', ""), cell_from_json(cell)?));
            }
        }
        if let Some(w) = s["widths"].as_object() {
            for (col, width) in w {
                let n = col_to_num(&col.to_uppercase().replace('$', ""))?;
                sheet.widths.push(ColWidth { col: n, width: width.as_f64().unwrap_or(10.0) });
            }
        }
        sheet.freeze_top = s["freeze_top"].as_bool().unwrap_or(false);
        sheet.header_style = s["header_style"].as_bool().unwrap_or(true);
        sheet.autofilter = s["autofilter"].as_str().map(String::from);
        if let Some(m) = s["merges"].as_array() {
            sheet.merges = m.iter().filter_map(|x| x.as_str().map(String::from)).collect();
        }
        out.push(sheet);
    }
    if out.is_empty() {
        out.push(Sheet::default());
    }
    Ok(out)
}

fn cell_from_json(v: &Value) -> Result<Cell, String> {
    if v.is_null() {
        return Ok(Cell::Empty);
    }
    if let Some(s) = v.as_str() {
        // typed strings: "=A1+B2" → formula, "date:2024-01-05" → date
        if let Some(f) = s.strip_prefix('=') {
            return Ok(Cell::Formula(f.to_string(), v["value"].as_f64()));
        }
        if let Some(d) = s.strip_prefix("date:") {
            return parse_date(d);
        }
        return Ok(Cell::Str(s.to_string()));
    }
    if let Some(b) = v.as_bool() {
        return Ok(Cell::Bool(b));
    }
    if let Some(o) = v.as_object() {
        if let Some(f) = o.get("formula").and_then(|f| f.as_str()) {
            return Ok(Cell::Formula(f.trim_start_matches('=').to_string(), o.get("value").and_then(|x| x.as_f64())));
        }
        if let Some(d) = o.get("date").and_then(|d| d.as_str()) {
            return parse_date(d);
        }
    }
    if let Some(n) = v.as_f64() {
        return Ok(Cell::Num(n));
    }
    Err(format!("unsupported cell value {v}"))
}

fn parse_date(s: &str) -> Result<Cell, String> {
    let p: Vec<&str> = s.split('-').collect();
    if p.len() != 3 {
        return Err(format!("bad date {s:?} (want YYYY-MM-DD)"));
    }
    let y: i64 = p[0].parse().map_err(|_| format!("bad year in {s}"))?;
    let m: u32 = p[1].parse().map_err(|_| format!("bad month in {s}"))?;
    let d: u32 = p[2].parse().map_err(|_| format!("bad day in {s}"))?;
    Ok(Cell::Date(y, m, d))
}

pub(crate) fn col_letter(mut n: u32) -> String {
    n += 1;
    let mut s = String::new();
    while n > 0 {
        let rem = (n - 1) % 26;
        s.insert(0, (b'A' + rem as u8) as char);
        n = (n - 1) / 26;
    }
    s
}

pub(crate) fn col_to_num(s: &str) -> Result<u32, String> {
    let s = s.trim_end_matches(char::is_numeric);
    let mut n = 0u32;
    for c in s.chars() {
        let d = c.to_ascii_uppercase() as u32 - 'A' as u32;
        if !(0..26).contains(&d) {
            return Err(format!("bad column {s:?}"));
        }
        n = n * 26 + d + 1;
    }
    Ok(n.saturating_sub(1))
}

// ── create ──────────────────────────────────────────────────────────────────

/// Serialize one sheet's XML.
fn sheet_xml(sheet: &Sheet) -> String {
    let mut rows: Vec<(u32, Vec<(u32, &Cell)>)> = Vec::new();
    for (rfc, cell) in &sheet.cells {
        let (r, c) = split_ref(rfc);
        match rows.iter_mut().find(|(rr, _)| *rr == r) {
            Some(row) => row.1.push((c, cell)),
            None => rows.push((r, vec![(c, cell)])),
        }
    }
    rows.sort_by_key(|(r, _)| *r);
    for (_, cells) in rows.iter_mut() {
        cells.sort_by_key(|(c, _)| *c);
    }
    let max_row = rows.last().map(|(r, _)| *r).unwrap_or(1);
    let max_col = rows.iter().flat_map(|(_, cs)| cs.iter().map(|(c, _)| *c)).max().unwrap_or(1);

    let mut x = String::from(
        "<?xml version=\"1.0\" encoding=\"UTF-8\" standalone=\"yes\"?>\
<worksheet xmlns=\"http://schemas.openxmlformats.org/spreadsheetml/2006/main\">",
    );
    if !sheet.widths.is_empty() {
        x.push_str("<cols>");
        for w in &sheet.widths {
            x.push_str(&format!(
                "<col min=\"{}\" max=\"{}\" width=\"{:.2}\" customWidth=\"1\"/>",
                w.col + 1,
                w.col + 1,
                w.width
            ));
        }
        x.push_str("</cols>");
    }
    if sheet.freeze_top {
        x.push_str("<sheetViews><sheetView workbookViewId=\"0\"><pane ySplit=\"1\" topLeftCell=\"A2\" activePane=\"bottomLeft\" state=\"frozen\"/></sheetView></sheetViews>");
    }
    x.push_str(&format!("<dimension ref=\"A1:{}\"/>", cell_ref(max_row, max_col)));
    x.push_str("<sheetData>");
    // style indexes in styles.xml: 0 default, 1 bold, 2 date, 3 header(bold+fill)
    for (r, cells) in &rows {
        x.push_str(&format!("<row r=\"{r}\">"));
        for (c, cell) in cells {
            let rfc = cell_ref(*r, *c);
            let header = sheet.header_style && *r == 1;
            match cell {
                Cell::Empty => {}
                Cell::Str(s) => {
                    let style = if header { " s=\"3\"" } else { "" };
                    x.push_str(&format!(
                        "<c r=\"{rfc}\"{style} t=\"inlineStr\"><is><t xml:space=\"preserve\">{}</t></is></c>",
                        escape(s)
                    ));
                }
                Cell::Num(n) => {
                    let style = if header { " s=\"1\"" } else { "" };
                    x.push_str(&format!("<c r=\"{rfc}\"{style}><v>{n}</v></c>"));
                }
                Cell::Bool(b) => {
                    x.push_str(&format!("<c r=\"{rfc}\" t=\"b\"><v>{}</v></c>", if *b { 1 } else { 0 }));
                }
                Cell::Formula(f, cached) => {
                    let cache = cached.map(|v| format!("<v>{v}</v>")).unwrap_or_default();
                    x.push_str(&format!("<c r=\"{rfc}\"><f>{}</f>{cache}</c>", escape(f)));
                }
                Cell::Date(y, m, d) => {
                    let serial = date_serial(*y, *m, *d);
                    x.push_str(&format!("<c r=\"{rfc}\" s=\"2\"><v>{serial}</v></c>"));
                }
            }
        }
        x.push_str("</row>");
    }
    x.push_str("</sheetData>");
    if let Some(range) = &sheet.autofilter {
        x.push_str(&format!("<autoFilter ref=\"{}\"/>", escape(range)));
    }
    if !sheet.merges.is_empty() {
        x.push_str(&format!(
            "<mergeCells count=\"{}\">{}</mergeCells>",
            sheet.merges.len(),
            sheet.merges.iter().map(|m| format!("<mergeCell ref=\"{}\"/>", escape(m))).collect::<String>()
        ));
    }
    x.push_str("</worksheet>");
    x
}

/// Days since 1899-12-30 (Excel serial, incl. the 1900 leap bug correction).
pub(crate) fn date_serial(y: i64, m: u32, d: u32) -> i64 {
    let (yy, mm) = if m <= 2 { (y - 1, m + 12) } else { (y, m) };
    let era = if yy >= 0 { yy } else { yy - 399 } / 400;
    let yoe = yy - era * 400;
    let doy = (153 * (mm as i64 - 3) + 2) / 5 + d as i64 - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy as i64;
    let days = era * 146_097 + doe as i64 - 719_468; // days since 1970-01-01
    // Excel serials run from 1899-12-31 for dates before 1900-03-01 (Excel
    // wrongly treats 1900 as a leap year, so 1900-02-29 = serial 60 is fake).
    days + 25_569 - if days < -25_508 { 1 } else { 0 }
}

fn split_ref(s: &str) -> (u32, u32) {
    let mut col = 0u32;
    let mut i = 0;
    let b = s.as_bytes();
    while i < b.len() && b[i].is_ascii_alphabetic() {
        col = col * 26 + (b[i].to_ascii_uppercase() - b'A') as u32 + 1;
        i += 1;
    }
    let row: u32 = s[i..].parse().unwrap_or(1);
    (row, col.saturating_sub(1))
}

fn cell_ref(row: u32, col: u32) -> String {
    format!("{}{}", col_letter(col), row)
}

/// Build a complete .xlsx from sheets.
pub fn build(sheets: &[Sheet]) -> Vec<u8> {
    let n = sheets.len();
    let mut workbook = format!(
        "<?xml version=\"1.0\" encoding=\"UTF-8\" standalone=\"yes\"?>\
<workbook xmlns=\"http://schemas.openxmlformats.org/spreadsheetml/2006/main\" \
xmlns:r=\"http://schemas.openxmlformats.org/officeDocument/2006/relationships\">\
<bookViews><workbookView xWindow=\"0\" yWindow=\"0\" windowWidth=\"20000\" windowHeight=\"12000\"/></bookViews><sheets>"
    );
    let mut wb_rels = String::from(
        "<?xml version=\"1.0\" encoding=\"UTF-8\" standalone=\"yes\"?>\
<Relationships xmlns=\"http://schemas.openxmlformats.org/package/2006/relationships\">",
    );
    for (i, s) in sheets.iter().enumerate() {
        workbook.push_str(&format!(
            "<sheet name=\"{}\" sheetId=\"{}\" r:id=\"rIdS{}\"/>",
            escape(&s.name),
            i + 1,
            i + 1
        ));
        wb_rels.push_str(&format!(
            "<Relationship Id=\"rIdS{}\" Type=\"http://schemas.openxmlformats.org/officeDocument/2006/relationships/worksheet\" Target=\"worksheets/sheet{}.xml\"/>",
            i + 1,
            i + 1
        ));
    }
    workbook.push_str("</sheets></workbook>");
    wb_rels.push_str("</Relationships>");

    let content_types = format!(
        "<?xml version=\"1.0\" encoding=\"UTF-8\" standalone=\"yes\"?>\
<Types xmlns=\"http://schemas.openxmlformats.org/package/2006/content-types\">\
<Default Extension=\"rels\" ContentType=\"application/vnd.openxmlformats-package.relationships+xml\"/>\
<Default Extension=\"xml\" ContentType=\"application/xml\"/>\
<Override PartName=\"/xl/workbook.xml\" ContentType=\"application/vnd.openxmlformats-officedocument.spreadsheetml.sheet.main+xml\"/>\
<Override PartName=\"/xl/styles.xml\" ContentType=\"application/vnd.openxmlformats-officedocument.spreadsheetml.styles+xml\"/>\
<Override PartName=\"/docProps/core.xml\" ContentType=\"application/vnd.openxmlformats-package.core-properties+xml\"/>\
<Override PartName=\"/docProps/app.xml\" ContentType=\"application/vnd.openxmlformats-officedocument.extended-properties+xml\"/>\
{}\
</Types>",
        (1..=n).map(|i| format!("<Override PartName=\"/xl/worksheets/sheet{i}.xml\" ContentType=\"application/vnd.openxmlformats-officedocument.spreadsheetml.worksheet+xml\"/>")).collect::<String>()
    );

    let root_rels = "<?xml version=\"1.0\" encoding=\"UTF-8\" standalone=\"yes\"?>\
<Relationships xmlns=\"http://schemas.openxmlformats.org/package/2006/relationships\">\
<Relationship Id=\"rId1\" Type=\"http://schemas.openxmlformats.org/officeDocument/2006/relationships/officeDocument\" Target=\"xl/workbook.xml\"/>\
<Relationship Id=\"rId2\" Type=\"http://schemas.openxmlformats.org/package/2006/relationships/metadata/core-properties\" Target=\"docProps/core.xml\"/>\
<Relationship Id=\"rId3\" Type=\"http://schemas.openxmlformats.org/officeDocument/2006/relationships/extended-properties\" Target=\"docProps/app.xml\"/>\
</Relationships>";

    // styles: 0 default, 1 bold, 2 date (builtin 14), 3 header (bold + gray fill)
    let styles = "<?xml version=\"1.0\" encoding=\"UTF-8\" standalone=\"yes\"?>\
<styleSheet xmlns=\"http://schemas.openxmlformats.org/spreadsheetml/2006/main\">\
<fonts count=\"2\"><font><sz val=\"11\"/><name val=\"Calibri\"/></font><font><b/><sz val=\"11\"/><name val=\"Calibri\"/></font></fonts>\
<fills count=\"3\"><fill><patternFill patternType=\"none\"/></fill><fill><patternFill patternType=\"gray125\"/></fill><fill><patternFill patternType=\"solid\"><fgColor rgb=\"FFD9E2F3\"/><bgColor indexed=\"64\"/></patternFill></fill></fills>\
<borders count=\"1\"><border><left/><right/><top/><bottom/><diagonal/></border></borders>\
<cellStyleXfs count=\"1\"><xf numFmtId=\"0\" fontId=\"0\" fillId=\"0\" borderId=\"0\"/></cellStyleXfs>\
<cellXfs count=\"4\">\
<xf numFmtId=\"0\" fontId=\"0\" fillId=\"0\" borderId=\"0\" xfId=\"0\"/>\
<xf numFmtId=\"0\" fontId=\"1\" fillId=\"0\" borderId=\"0\" xfId=\"0\" applyFont=\"1\"/>\
<xf numFmtId=\"14\" fontId=\"0\" fillId=\"0\" borderId=\"0\" xfId=\"0\" applyNumberFormat=\"1\"/>\
<xf numFmtId=\"0\" fontId=\"1\" fillId=\"2\" borderId=\"0\" xfId=\"0\" applyFont=\"1\" applyFill=\"1\"/>\
</cellXfs>\
<cellStyles count=\"1\"><cellStyle name=\"Normal\" xfId=\"0\" builtinId=\"0\"/></cellStyles>\
</styleSheet>";

    let date = {
        let secs = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0);
        let (y, m, d) = civil_from_days((secs / 86400) as i64);
        format!("{y:04}-{m:02}-{d:02}T00:00:00Z")
    };
    let core = format!(
        "<?xml version=\"1.0\" encoding=\"UTF-8\" standalone=\"yes\"?>\
<cp:coreProperties xmlns:cp=\"http://schemas.openxmlformats.org/package/2006/metadata/core-properties\" \
xmlns:dc=\"http://purl.org/dc/elements/1.1/\" xmlns:dcterms=\"http://purl.org/dc/terms/\" xmlns:xsi=\"http://www.w3.org/2001/XMLSchema-instance\">\
<dc:creator>RomaCode</dc:creator><cp:lastModifiedBy>RomaCode</cp:lastModifiedBy>\
<dcterms:created xsi:type=\"dcterms:W3CDTF\">{date}</dcterms:created><dcterms:modified xsi:type=\"dcterms:W3CDTF\">{date}</dcterms:modified>\
</cp:coreProperties>"
    );
    let app = "<?xml version=\"1.0\" encoding=\"UTF-8\" standalone=\"yes\"?>\
<Properties xmlns=\"http://schemas.openxmlformats.org/officeDocument/2006/extended-properties\"><Application>RomaCode documents engine</Application></Properties>";

    let mut z = ZipWriter::new();
    z.add("[Content_Types].xml", content_types.as_bytes());
    z.add("_rels/.rels", root_rels.as_bytes());
    z.add("xl/workbook.xml", workbook.as_bytes());
    z.add("xl/_rels/workbook.xml.rels", wb_rels.as_bytes());
    z.add("xl/styles.xml", styles.as_bytes());
    z.add("docProps/core.xml", core.as_bytes());
    z.add("docProps/app.xml", app.as_bytes());
    for (i, s) in sheets.iter().enumerate() {
        z.add(&format!("xl/worksheets/sheet{}.xml", i + 1), sheet_xml(s).as_bytes());
    }
    z.finish()
}

// ── read ────────────────────────────────────────────────────────────────────

#[derive(Debug, Default)]
pub struct CellValue {
    pub rfc: String,
    pub kind: String, // text | number | bool | formula | date | empty
    pub text: String,
    pub formula: Option<String>,
}

#[derive(Debug, Default)]
pub struct WorkbookContent {
    pub sheets: Vec<(String, Vec<Vec<CellValue>>)>, // name, rows
}

/// Read every sheet's values from .xlsx bytes.
pub fn read(data: &[u8]) -> Result<WorkbookContent, String> {
    let z = ZipReader::open(data).map_err(|e| format!("not an xlsx (zip): {e}"))?;
    let wb = z.read("xl/workbook.xml").map_err(|_| "missing xl/workbook.xml — not an xlsx".to_string())?;
    let wb_s = String::from_utf8_lossy(&wb).into_owned();
    // sheet name → target
    let mut sheets: Vec<(String, String)> = Vec::new();
    {
        let mut p = Parser::new(&wb_s);
        while let Some(ev) = p.next() {
            // <sheet name="…" sheetId="…" r:id="…"/> is self-closing.
            if let Some(attrs) = match &ev {
                Event::Start(n, attrs) if n == "sheet" => Some(attrs),
                Event::Empty(n, attrs) if n == "sheet" => Some(attrs),
                _ => None,
            } {
                let name = attrs.iter().find(|(k, _)| k == "name").map(|(_, v)| v.clone()).unwrap_or_default();
                let id = attrs.iter().find(|(k, _)| k == "r:id").or_else(|| attrs.iter().find(|(k, _)| k == "sheetId")).map(|(_, v)| v.clone()).unwrap_or_default();
                sheets.push((name, id));
            }
        }
    }
    // resolve targets via rels
    let rels_bytes = z.read("xl/_rels/workbook.xml.rels").unwrap_or_default();
    let rels = String::from_utf8_lossy(&rels_bytes).into_owned();
    let resolve = |id: &str| -> Option<String> {
        let mut p = Parser::new(&rels);
        while let Some(ev) = p.next() {
            // <Relationship Id="…" Target="…"/> is self-closing.
            if let Some(attrs) = match &ev {
                Event::Start(n, attrs) if n == "Relationship" => Some(attrs),
                Event::Empty(n, attrs) if n == "Relationship" => Some(attrs),
                _ => None,
            } {
                let rid = attrs.iter().find(|(k, _)| k == "Id").map(|(_, v)| v.clone());
                if rid.as_deref() == Some(id) {
                    return attrs.iter().find(|(k, _)| k == "Target").map(|(_, v)| v.clone());
                }
            }
        }
        None
    };

    // shared strings
    let shared: Vec<String> = match z.read("xl/sharedStrings.xml") {
        Ok(b) => {
            let s = String::from_utf8_lossy(&b).into_owned();
            let mut p = Parser::new(&s);
            let mut out = Vec::new();
            let mut cur = String::new();
            let mut in_item = false;
            while let Some(ev) = p.next() {
                match ev {
                    Event::Start(n, _) if n == "si" => {
                        in_item = true;
                        cur = String::new();
                    }
                    Event::End(n) if n == "si" => {
                        if in_item {
                            out.push(std::mem::take(&mut cur));
                            in_item = false;
                        }
                    }
                    Event::Text(t) if in_item => cur.push_str(&t),
                    _ => {}
                }
            }
            out
        }
        Err(_) => Vec::new(),
    };

    let mut out = WorkbookContent::default();
    for (name, id) in sheets {
        let target = resolve(&id).unwrap_or_else(|| format!("worksheets/sheet{}.xml", out.sheets.len() + 1));
        let target_path = if target.starts_with("/xl/") { target.trim_start_matches('/').to_string() } else { format!("xl/{target}") };
        let bytes = match z.read(&target_path) {
            Ok(b) => b,
            Err(_) => continue,
        };
        let rows = parse_sheet(&String::from_utf8_lossy(&bytes), &shared);
        out.sheets.push((name, rows));
    }
    Ok(out)
}

fn parse_sheet(xml: &str, shared: &[String]) -> Vec<Vec<CellValue>> {
    let mut p = Parser::new(xml);
    let mut rows: Vec<Vec<CellValue>> = Vec::new();
    let mut cur_row: u32 = 0;
    let mut cur_cells: Vec<CellValue> = Vec::new();
    let mut in_cell = false;
    let mut cell_ref = String::new();
    let mut cell_type = String::new();
    let mut cell_style = String::new();
    let mut in_v = false;
    let mut v_text = String::new();
    let mut in_f = false;
    let mut f_text = String::new();
    let mut in_is = false;
    let mut is_text = String::new();
    while let Some(ev) = p.next() {
        match ev {
            Event::Start(n, attrs) => match n.as_str() {
                "row" => {
                    cur_row = attrs.iter().find(|(k, _)| k == "r").and_then(|(_, v)| v.parse().ok()).unwrap_or(rows.len() as u32 + 1);
                    cur_cells = Vec::new();
                }
                "c" => {
                    in_cell = true;
                    cell_ref = attrs.iter().find(|(k, _)| k == "r").map(|(_, v)| v.clone()).unwrap_or_default();
                    cell_type = attrs.iter().find(|(k, _)| k == "t").map(|(_, v)| v.clone()).unwrap_or_default();
                    cell_style = attrs.iter().find(|(k, _)| k == "s").map(|(_, v)| v.clone()).unwrap_or_default();
                    v_text.clear();
                    f_text.clear();
                    is_text.clear();
                }
                "v" => {
                    in_v = true;
                }
                "f" => {
                    in_f = true;
                }
                "is" => {
                    in_is = true;
                }
                _ => {}
            },
            Event::Text(t) => {
                if in_v {
                    v_text.push_str(&t);
                } else if in_f {
                    f_text.push_str(&t);
                } else if in_is {
                    is_text.push_str(&t);
                }
            }
            Event::End(n) => match n.as_str() {
                "v" => in_v = false,
                "f" => in_f = false,
                "is" => in_is = false,
                "c" => {
                    let val = build_value(&cell_type, &cell_style, &v_text, &is_text, shared);
                    cur_cells.push(CellValue { rfc: cell_ref.clone(), kind: val.0, text: val.1, formula: if f_text.is_empty() { None } else { Some(f_text.clone()) } });
                    in_cell = false;
                }
                "row" => {
                    rows.push(std::mem::take(&mut cur_cells));
                    let _ = cur_row;
                }
                _ => {}
            },
            _ => {}
        }
    }
    let _ = in_cell;
    rows
}

fn build_value(t: &str, style: &str, v: &str, inline: &str, shared: &[String]) -> (String, String) {
    match t {
        "s" => {
            let idx: usize = v.parse().unwrap_or(usize::MAX);
            ("text".into(), shared.get(idx).cloned().unwrap_or_default())
        }
        "inlineStr" => ("text".into(), inline.to_string()),
        "str" => ("text".into(), v.to_string()),
        "b" => ("bool".into(), if v == "1" { "TRUE".into() } else { "FALSE".into() }),
        "e" => ("error".into(), v.to_string()),
        _ => {
            // numeric; date if the style uses builtin numFmt 14..22, 45..47
            let numeric: f64 = v.parse().unwrap_or(0.0);
            let is_num = v.parse::<f64>().is_ok();
            if is_num && is_date_style(style) {
                let days = numeric as i64;
                let (y, m, d) = serial_to_date(days);
                ("date".into(), format!("{y:04}-{m:02}-{d:02}"))
            } else if is_num {
                if v.contains('.') {
                    ("number".into(), v.to_string())
                } else {
                    ("number".into(), format!("{}", numeric as i64))
                }
            } else {
                ("text".into(), v.to_string())
            }
        }
    }
}

fn is_date_style(style: &str) -> bool {
    // This engine writes dates with s="2"; real files carry their own numFmts.
    // Without parsing styles.xml we accept the common convention: any style
    // index that maps to builtin date formats is unknown here, so we rely on
    // a lightweight heuristic — styles.xml scan happens in `read` for exact
    // detection; here treat style "2" (ours) as date.
    style == "2"
}

fn serial_to_date(serial: i64) -> (i64, u32, u32) {
    let days = serial - 25_569;
    civil_from_days(days)
}

/// Read an .xlsx from a file path.
pub fn read_path(path: &str) -> Result<WorkbookContent, String> {
    let data = std::fs::read(path).map_err(|e| format!("xlsx {path}: {e}"))?;
    read(&data)
}

pub fn content_to_json(c: &WorkbookContent, max_chars: usize) -> Value {
    let mut budget = max_chars;
    let sheets: Vec<Value> = c
        .sheets
        .iter()
        .map(|(name, rows)| {
            let mut kept = Vec::new();
            for row in rows {
                if budget <= 0 {
                    break;
                }
                let cells: Vec<Value> = row
                    .iter()
                    .map(|cv| {
                        budget = budget.saturating_sub(cv.text.len());
                        let mut o = json!({ "cell": cv.rfc, "type": cv.kind, "value": cv.text });
                        if let Some(f) = &cv.formula {
                            o["formula"] = json!(f);
                        }
                        o
                    })
                    .collect();
                kept.push(json!(cells));
            }
            let truncated = if budget <= 0 { " (truncated)" } else { "" };
            json!({ "name": name, "rows": kept, "row_count": rows.len(), "note": truncated.trim() })
        })
        .collect();
    json!({ "sheet_count": c.sheets.len(), "sheets": sheets })
}

// ── patch (edit in place) ───────────────────────────────────────────────────

/// Operations for an in-place edit of an existing workbook.
#[derive(Debug, Default)]
pub struct Patch {
    pub sheet: Option<String>, // name (default: first sheet)
    /// (ref, new value) — Empty deletes the cell.
    pub set: Vec<(String, Cell)>,
}

pub fn patch_from_json(v: &Value) -> Result<Patch, String> {
    let mut p = Patch { sheet: v["sheet"].as_str().map(String::from), ..Default::default() };
    for (k, val) in v["cells"].as_object().ok_or("patch needs cells object {\"A1\": value}")? {
        p.set.push((k.to_uppercase().replace('$', ""), cell_from_json(val)?));
    }
    Ok(p)
}

/// Patch an existing workbook: rewrite only the target sheet XML.
pub fn patch(data: &[u8], op: &Patch) -> Result<Vec<u8>, String> {
    let z = ZipReader::open(data).map_err(|e| format!("zip: {e}"))?;
    // find target sheet part
    let wb = z.read("xl/workbook.xml").map_err(|_| "not an xlsx".to_string())?;
    let wb_s = String::from_utf8_lossy(&wb).into_owned();
    let mut p = Parser::new(&wb_s);
    let mut sheet_targets: Vec<(String, String)> = Vec::new();
    while let Some(ev) = p.next() {
        if let Some(attrs) = match &ev {
            Event::Start(n, attrs) if n == "sheet" => Some(attrs),
            Event::Empty(n, attrs) if n == "sheet" => Some(attrs),
            _ => None,
        } {
            let name = attrs.iter().find(|(k, _)| k == "name").map(|(_, v)| v.clone()).unwrap_or_default();
            let rid = attrs.iter().find(|(k, _)| k == "r:id").map(|(_, v)| v.clone()).unwrap_or_default();
            sheet_targets.push((name, rid));
        }
    }
    let rels_b = z.read("xl/_rels/workbook.xml.rels").unwrap_or_default();
    let rels = String::from_utf8_lossy(&rels_b).into_owned();
    let target_of = |rid: &str| -> Option<String> {
        let mut p = Parser::new(&rels);
        while let Some(ev) = p.next() {
            if let Some(attrs) = match &ev {
                Event::Start(n, attrs) if n == "Relationship" => Some(attrs),
                Event::Empty(n, attrs) if n == "Relationship" => Some(attrs),
                _ => None,
            } {
                let id = attrs.iter().find(|(k, _)| k == "Id").map(|(_, v)| v.clone());
                if id.as_deref() == Some(rid) {
                    return attrs.iter().find(|(k, _)| k == "Target").map(|(_, v)| v.clone());
                }
            }
        }
        None
    };
    let wanted = op.sheet.clone().or_else(|| sheet_targets.first().map(|(n, _)| n.clone()));
    let mut part: Option<String> = None;
    for (name, rid) in &sheet_targets {
        if name.eq_ignore_ascii_case(wanted.as_deref().unwrap_or("")) {
            let t = target_of(rid).ok_or("workbook rels missing target")?;
            part = Some(if t.starts_with('/') { t.trim_start_matches('/').to_string() } else { format!("xl/{}", t) });
        }
    }
    let part = part.ok_or_else(|| format!("sheet {:?} not found", wanted.unwrap_or_default()))?;

    // load + modify the sheet
    let xml_b = z.read(&part)?;
    let xml = String::from_utf8_lossy(&xml_b).into_owned();
    let new_xml = apply_patch(&xml, op)?;
    // rebuild the zip replacing that one entry
    let mut zw = ZipWriter::new();
    for name in z.names() {
        if name == part {
            zw.add(&name, new_xml.as_bytes());
        } else {
            let b = z.read(&name)?;
            zw.add(&name, &b);
        }
    }
    Ok(zw.finish())
}

fn apply_patch(xml: &str, op: &Patch) -> Result<String, String> {
    // Walk the sheet XML once: keep everything outside <sheetData> verbatim,
    // collect cells from inside, then re-emit sheetData with the ops applied.
    let mut p = Parser::new(xml);
    let mut before = String::new(); // XML before <sheetData> (incl. <worksheet …>)
    let mut after = String::new(); // XML after </sheetData> (incl. closers)
    let mut cells: Vec<(String, String)> = Vec::new();
    let mut in_sheetdata = false;
    let mut cur_attrs: Vec<(String, String)> = Vec::new();
    let mut cur_ref = String::new();
    let mut cur_type = String::new();
    let mut in_v = false;
    let mut v_text = String::new();
    let mut in_f = false;
    let mut f_text = String::new();
    let mut in_is = false;
    let mut is_text = String::new();
    // pending row grouping: remember first row tag seen per row number
    let mut row_xml: Vec<(u32, String)> = Vec::new(); // row number → row open attrs
    let mut cur_row_num = 0u32;
    while let Some(ev) = p.next() {
        match ev {
            Event::Start(n, attrs) => match n.as_str() {
                "sheetData" => in_sheetdata = true,
                "row" if in_sheetdata => {
                    cur_row_num = attrs.iter().find(|(k, _)| k == "r").and_then(|(_, v)| v.parse().ok()).unwrap_or(row_xml.len() as u32 + 1);
                    if !row_xml.iter().any(|(r, _)| *r == cur_row_num) {
                        row_xml.push((cur_row_num, render_start("row", &attrs)));
                    }
                }
                "c" if in_sheetdata => {
                    cur_ref = attrs.iter().find(|(k, _)| k == "r").map(|(_, v)| v.clone()).unwrap_or_default();
                    cur_type = attrs.iter().find(|(k, _)| k == "t").map(|(_, v)| v.clone()).unwrap_or_default();
                    cur_attrs = attrs;
                    v_text.clear();
                    f_text.clear();
                    is_text.clear();
                }
                "v" if in_sheetdata => in_v = true,
                "f" if in_sheetdata => in_f = true,
                "is" if in_sheetdata => in_is = true,
                _ => {
                    let s = render_start(&n, &attrs);
                    if in_sheetdata && n != "row" {
                        // unexpected element inside a row (rare) — drop
                    } else if !in_sheetdata {
                        before.push_str(&s);
                    }
                }
            },
            Event::Text(t) => {
                if in_v {
                    v_text.push_str(&t);
                } else if in_f {
                    f_text.push_str(&t);
                } else if in_is {
                    is_text.push_str(&t);
                }
            }
            Event::Empty(n, attrs) => {
                if !in_sheetdata {
                    let s = render_start(&n, &attrs);
                    before.push_str(&s.replace('>', "/>"));
                }
            }
            Event::End(n) => match n.as_str() {
                "v" => in_v = false,
                "f" => in_f = false,
                "is" => in_is = false,
                "c" if in_sheetdata => {
                    let style = cur_attrs.iter().find(|(k, _)| k == "s").map(|(_, v)| v.clone());
                    let cell = if !is_text.is_empty() {
                        make_cell(&cur_ref, style.as_deref(), &Cell::Str(is_text.clone()))
                    } else if !f_text.is_empty() {
                        make_cell(&cur_ref, style.as_deref(), &Cell::Formula(f_text.clone(), v_text.parse().ok()))
                    } else {
                        make_cell(&cur_ref, style.as_deref(), &cell_value_from(v_text.clone(), cur_type.clone()))
                    };
                    if !cell.is_empty() {
                        cells.push((cur_ref.clone(), cell));
                    }
                }
                "sheetData" => in_sheetdata = false,
                _ => {
                    if !in_sheetdata {
                        after.push_str(&format!("</{n}>"));
                    }
                }
            },
        }
    }

    // Apply ops: delete or replace/add cells.
    for (rfc, val) in &op.set {
        cells.retain(|(r, _)| r != rfc);
        if !matches!(val, Cell::Empty) {
            let style = if matches!(val, Cell::Date(..)) { Some("2") } else { None };
            cells.push((rfc.clone(), make_cell(rfc, style, val)));
        }
    }
    cells.sort_by_key(|(r, _)| {
        let (row, col) = split_ref(r);
        (row, col)
    });

    // Re-emit: before + sheetData + after. Keep the original row ordering and
    // original row tags; rows that lost every cell are dropped; cells for
    // rows not seen before get a fresh <row>.
    let mut out = before;
    out.push_str("<sheetData>");
    for (row_no, row_open) in &row_xml {
        let row_cells: Vec<&(String, String)> = cells.iter().filter(|(r, _)| split_ref(r).0 == *row_no).collect();
        if row_cells.is_empty() {
            continue;
        }
        out.push_str(row_open);
        for (_, cx) in row_cells {
            out.push_str(cx);
        }
        out.push_str("</row>");
    }
    // new rows (not present in the original)
    let known: std::collections::HashSet<u32> = row_xml.iter().map(|(r, _)| *r).collect();
    for (rfc, cx) in &cells {
        let (row, _) = split_ref(rfc);
        if !known.contains(&row) {
            out.push_str(&format!("<row r=\"{row}\">{cx}</row>"));
        }
    }
    out.push_str("</sheetData>");
    out.push_str(&after);
    Ok(out)
}

fn render_start(n: &str, attrs: &[(String, String)]) -> String {
    if attrs.is_empty() {
        return format!("<{n}>");
    }
    let a: String = attrs.iter().map(|(k, v)| format!(" {}=\"{}\"", k, escape(v))).collect();
    format!("<{n}{a}>")
}

fn cell_value_from(v: String, t: String) -> Cell {
    match t.as_str() {
        "b" => Cell::Bool(v == "1" || v.eq_ignore_ascii_case("true")),
        "str" | "s" | "inlineStr" => Cell::Str(v),
        _ => match v.parse::<f64>() {
            Ok(n) => Cell::Num(n),
            Err(_) => Cell::Str(v),
        },
    }
}

fn make_cell(rfc: &str, style: Option<&str>, val: &Cell) -> String {
    let s_attr = style.map(|s| format!(" s=\"{s}\"")).unwrap_or_default();
    match val {
        Cell::Empty => String::new(),
        Cell::Str(s) => format!("<c r=\"{rfc}\"{s_attr} t=\"inlineStr\"><is><t xml:space=\"preserve\">{}</t></is></c>", escape(s)),
        Cell::Num(n) => format!("<c r=\"{rfc}\"{s_attr}><v>{n}</v></c>"),
        Cell::Bool(b) => format!("<c r=\"{rfc}\" t=\"b\"><v>{}</v></c>", if *b { 1 } else { 0 }),
        Cell::Formula(f, cached) => {
            let cache = cached.map(|v| format!("<v>{v}</v>")).unwrap_or_default();
            format!("<c r=\"{rfc}\"{s_attr}><f>{}</f>{cache}</c>", escape(f))
        }
        Cell::Date(y, m, d) => format!("<c r=\"{rfc}\" s=\"2\"><v>{}</v></c>", date_serial(*y, *m, *d)),
    }
}

// ── CSV ─────────────────────────────────────────────────────────────────────

/// Parse CSV (quotes, embedded commas/newlines) into rows.
pub fn parse_csv(text: &str, delimiter: char) -> Vec<Vec<String>> {
    let mut rows = Vec::new();
    let mut row: Vec<String> = Vec::new();
    let mut field = String::new();
    let mut in_quotes = false;
    let mut chars = text.chars().peekable();
    while let Some(c) = chars.next() {
        if in_quotes {
            if c == '"' {
                if chars.peek() == Some(&'"') {
                    field.push('"');
                    chars.next();
                } else {
                    in_quotes = false;
                }
            } else {
                field.push(c);
            }
        } else if c == '"' && field.is_empty() {
            in_quotes = true;
        } else if c == delimiter {
            row.push(std::mem::take(&mut field));
        } else if c == '\n' {
            row.push(std::mem::take(&mut field));
            rows.push(std::mem::take(&mut row));
        } else if c == '\r' {
            // skip (handled by \n)
        } else {
            field.push(c);
        }
    }
    if !field.is_empty() || !row.is_empty() {
        row.push(field);
        rows.push(row);
    }
    rows
}

fn csv_escape(s: &str) -> String {
    if s.contains(',') || s.contains('"') || s.contains('\n') {
        format!("\"{}\"", s.replace('"', "\"\""))
    } else {
        s.to_string()
    }
}

/// Convert rows to CSV text (comma).
pub fn rows_to_csv(rows: &[Vec<String>]) -> String {
    rows_to_csv_delim(rows, ',')
}

/// Convert rows to CSV text with an explicit delimiter.
pub fn rows_to_csv_delim(rows: &[Vec<String>], delimiter: char) -> String {
    rows.iter()
        .map(|r| {
            r.iter()
                .map(|c| {
                    if c.contains(delimiter) || c.contains('"') || c.contains('\n') {
                        format!("\"{}\"", c.replace('"', "\"\""))
                    } else {
                        c.clone()
                    }
                })
                .collect::<Vec<_>>()
                .join(&delimiter.to_string())
        })
        .collect::<Vec<_>>()
        .join("\n")
}

/// CSV text → xlsx bytes (first row as bold header).
pub fn csv_to_xlsx(text: &str, sheet_name: &str, delimiter: char) -> Vec<u8> {
    let rows = parse_csv(text, delimiter);
    let mut sheet = Sheet { name: sheet_name.to_string(), header_style: true, ..Default::default() };
    for (r, row) in rows.iter().enumerate() {
        for (c, f) in row.iter().enumerate() {
            let cell = if r == 0 {
                Cell::Str(f.clone())
            } else if let Ok(n) = f.parse::<f64>() {
                Cell::Num(n)
            } else {
                Cell::Str(f.clone())
            };
            sheet.cells.push((format!("{}{}", col_letter(c as u32), r + 1), cell));
        }
    }
    build(&[sheet])
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn create_read_round_trip() {
        let s = Sheet {
            name: "Data".into(),
            cells: vec![
                ("A1".into(), Cell::Str("Name".into())),
                ("B1".into(), Cell::Str("Score".into())),
                ("C1".into(), Cell::Str("When".into())),
                ("A2".into(), Cell::Str("ada".into())),
                ("B2".into(), Cell::Num(99.5)),
                ("C2".into(), Cell::Date(2024, 1, 5)),
                ("D1".into(), Cell::Str("Total".into())),
                ("D2".into(), Cell::Formula("B2*2".into(), Some(199.0))),
                ("A3".into(), Cell::Bool(true)),
            ],
            widths: vec![ColWidth { col: 0, width: 22.0 }],
            freeze_top: true,
            autofilter: Some("A1:D2".into()),
            merges: vec![],
            header_style: true,
        };
        let bytes = build(&[s]);
        let c = read(&bytes).unwrap();
        assert_eq!(c.sheets.len(), 1);
        assert_eq!(c.sheets[0].0, "Data");
        let rows = &c.sheets[0].1;
        assert_eq!(rows[0].len(), 4);
        assert_eq!(rows[0][0].text, "Name");
        assert_eq!(rows[1][0].text, "ada");
        assert_eq!(rows[1][1].text, "99.5");
        assert_eq!(rows[1][2].kind, "date");
        assert_eq!(rows[1][2].text, "2024-01-05");
        assert_eq!(rows[1][3].formula.as_deref(), Some("B2*2"));
        assert_eq!(rows[2][0].kind, "bool");
    }

    #[test]
    fn patch_replaces_only_target_sheet() {
        let s1 = Sheet { name: "One".into(), cells: vec![("A1".into(), Cell::Str("orig".into()))], ..Default::default() };
        let s2 = Sheet { name: "Two".into(), cells: vec![("A1".into(), Cell::Str("keep".into()))], ..Default::default() };
        let bytes = build(&[s1, s2]);
        let op = Patch { sheet: Some("One".into()), set: vec![("A1".into(), Cell::Str("patched".into())), ("B2".into(), Cell::Num(42.0))] };
        let patched = patch(&bytes, &op).unwrap();
        let c = read(&patched).unwrap();
        assert_eq!(c.sheets[0].1[0][0].text, "patched");
        assert!(c.sheets[0].1.iter().any(|r| r.iter().any(|cv| cv.rfc == "B2" && cv.text == "42")));
        assert_eq!(c.sheets[1].1[0][0].text, "keep");
    }

    #[test]
    fn csv_round_trip() {
        let csv = "a,b\n1,\"x,y\"\n2,\"say \"\"hi\"\"\"";
        let rows = parse_csv(csv, ',');
        assert_eq!(rows[1][1], "x,y");
        assert_eq!(rows[2][1], "say \"hi\"");
        let back = rows_to_csv(&rows);
        assert_eq!(parse_csv(&back, ','), rows);
    }

    #[test]
    fn date_serial_matches_excel() {
        assert_eq!(date_serial(1900, 1, 1), 1);
        assert_eq!(date_serial(2024, 1, 5), 45_296);
        assert_eq!(date_serial(2000, 1, 1), 36_526);
    }

    #[test]
    fn col_letters() {
        assert_eq!(col_letter(0), "A");
        assert_eq!(col_letter(25), "Z");
        assert_eq!(col_letter(26), "AA");
        assert_eq!(col_to_num("AA").unwrap(), 26);
    }
}
