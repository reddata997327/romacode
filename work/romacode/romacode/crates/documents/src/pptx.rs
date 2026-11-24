//! PowerPoint (.pptx) — PresentationML, create + read, pure Rust.
//!
//! Create: a complete themed deck (master, layouts, theme, notes) with slide
//! kinds `title`, `section`, `bullets`, `two_content`, `image` and `blank`,
//! optional speaker notes per slide and configurable accent colors. 16:9 by
//! default. Read: per-slide text, notes and slide count.

use crate::docx::civil_from_days;
use crate::xml::{escape, Event, Parser};
use crate::zipio::{ZipReader, ZipWriter};
use serde_json::{json, Value};

const EMU_W: u32 = 12_192_000; // 16:9
const EMU_H: u32 = 6_858_000;

#[derive(Clone, Debug)]
pub enum Slide {
    Title { title: String, subtitle: String },
    Section { title: String },
    Bullets { title: String, items: Vec<(String, u8)> },
    TwoContent { title: String, left: Vec<String>, right: Vec<String> },
    Image { title: String, path: String, caption: Option<String> },
    Blank { text: String },
}

#[derive(Clone, Debug, Default)]
pub struct Deck {
    pub title: Option<String>,
    pub author: Option<String>,
    /// accent1..6 as hex (without '#'); empty = built-in palette.
    pub accents: [String; 6],
    pub slides: Vec<(Slide, Option<String>)>, // slide + notes
}

pub fn deck_from_json(v: &Value) -> Result<Deck, String> {
    let mut d = Deck { title: v["title"].as_str().map(String::from), author: v["author"].as_str().map(String::from), ..Default::default() };
    for (i, key) in ["accent1", "accent2", "accent3", "accent4", "accent5", "accent6"].iter().enumerate() {
        d.accents[i] = v[key].as_str().unwrap_or("").trim_start_matches('#').to_string();
    }
    let arr = v["slides"].as_array().ok_or("slides must be an array")?;
    for s in arr {
        let ty = s["type"].as_str().unwrap_or("bullets");
        let g = |k: &str| s[k].as_str().unwrap_or("").to_string();
        let notes = s["notes"].as_str().map(String::from);
        let slide = match ty {
            "title" => Slide::Title { title: g("title"), subtitle: g("subtitle") },
            "section" | "section_header" => Slide::Section { title: g("title") },
            "bullets" | "content" | "list" => Slide::Bullets {
                title: g("title"),
                items: s["items"].as_array().map(|a| a.iter().map(|it| match it { Value::String(t) => (t.clone(), 0u8), Value::Object(_) => (it["text"].as_str().unwrap_or("").to_string(), it["level"].as_u64().unwrap_or(0).min(3) as u8), _ => (String::new(), 0) }).collect()).unwrap_or_default(),
            },
            "two_content" | "two-content" => Slide::TwoContent {
                title: g("title"),
                left: str_list(&s["left"]),
                right: str_list(&s["right"]),
            },
            "image" => Slide::Image { title: g("title"), path: g("path"), caption: s["caption"].as_str().map(String::from) },
            "blank" => Slide::Blank { text: g("text") },
            other => return Err(format!("unknown slide type {other:?}")),
        };
        d.slides.push((slide, notes));
    }
    if d.slides.is_empty() {
        return Err("a deck needs at least one slide".into());
    }
    Ok(d)
}

fn str_list(v: &Value) -> Vec<String> {
    v.as_array().map(|a| a.iter().filter_map(|x| x.as_str().map(String::from)).collect()).unwrap_or_default()
}

// ── create ──────────────────────────────────────────────────────────────────

struct Media {
    id: usize,
    rel_id: String,
    name: String,
    data: Vec<u8>,
    ext: String,
}

pub fn build(deck: &Deck) -> Result<Vec<u8>, String> {
    let mut media: Vec<Media> = Vec::new();
    let n = deck.slides.len();
    let default_accents = ["4472C4", "ED7D31", "A5A5A5", "FFC000", "5B9BD5", "70AD47"];
    let accents: Vec<String> = (0..6).map(|i| if deck.accents[i].is_empty() { default_accents[i].to_string() } else { deck.accents[i].clone() }).collect();

    let mut slides_xml: Vec<String> = Vec::new();
    let mut slides_rels: Vec<String> = Vec::new();
    let mut notes_files: Vec<usize> = Vec::new(); // slide numbers that carry notes
    let mut notes_xml: Vec<(usize, String)> = Vec::new(); // (slide number, xml)
    for (i, (slide, notes)) in deck.slides.iter().enumerate() {
        let has_notes = notes.is_some();
        let img_rel = match slide {
            Slide::Image { path, .. } => {
                let data = std::fs::read(path).map_err(|e| format!("image {path}: {e}"))?;
                let ext = path.rsplit('.').next().unwrap_or("png").to_ascii_lowercase();
                let ext = if ext == "jpg" { "jpeg".into() } else { ext };
                let (iw, ih) = image_extent(&data);
                let m = Media { id: media.len() + 1, rel_id: format!("rIdImg{}", media.len() + 2), name: format!("image{}.{}", media.len() + 1, ext), data, ext };
                let r = m.rel_id.clone();
                let name = m.name.clone();
                media.push(m);
                Some((r, name, (iw, ih)))
            }
            _ => None,
        };
        slides_xml.push(slide_xml(slide, i + 1, &accents, img_rel.as_ref().map(|(r, _, sz)| (r.clone(), *sz))));
        slides_rels.push(slide_rels_xml(has_notes, img_rel.as_ref().map(|(r, name, _)| (r.clone(), name.clone())), i + 1));
        if let Some(text) = notes {
            notes_files.push(i + 1);
            notes_xml.push((i + 1, notes_xml_for(i + 1, text)));
        }
    }

    // [Content_Types].xml
    let mut ct = String::from(
        "<?xml version=\"1.0\" encoding=\"UTF-8\" standalone=\"yes\"?>\
<Types xmlns=\"http://schemas.openxmlformats.org/package/2006/content-types\">\
<Default Extension=\"rels\" ContentType=\"application/vnd.openxmlformats-package.relationships+xml\"/>\
<Default Extension=\"xml\" ContentType=\"application/xml\"/>\
<Default Extension=\"png\" ContentType=\"image/png\"/><Default Extension=\"jpeg\" ContentType=\"image/jpeg\"/>\
<Default Extension=\"jpg\" ContentType=\"image/jpeg\"/><Default Extension=\"gif\" ContentType=\"image/gif\"/>\
<Override PartName=\"/ppt/presentation.xml\" ContentType=\"application/vnd.openxmlformats-officedocument.presentationml.presentation.main+xml\"/>\
<Override PartName=\"/ppt/slideMasters/slideMaster1.xml\" ContentType=\"application/vnd.openxmlformats-officedocument.presentationml.slideMaster+xml\"/>\
<Override PartName=\"/ppt/theme/theme1.xml\" ContentType=\"application/vnd.openxmlformats-officedocument.theme+xml\"/>",
    );
    for i in 1..=2 {
        ct.push_str(&format!("<Override PartName=\"/ppt/slideLayouts/slideLayout{i}.xml\" ContentType=\"application/vnd.openxmlformats-officedocument.presentationml.slideLayout+xml\"/>"));
    }
    for i in 1..=n {
        ct.push_str(&format!("<Override PartName=\"/ppt/slides/slide{i}.xml\" ContentType=\"application/vnd.openxmlformats-officedocument.presentationml.slide+xml\"/>"));
    }
    for no in &notes_files {
        ct.push_str(&format!("<Override PartName=\"/ppt/notesSlides/notesSlide{no}.xml\" ContentType=\"application/vnd.openxmlformats-officedocument.presentationml.notesSlide+xml\"/>"));
    }
    ct.push_str("<Override PartName=\"/docProps/core.xml\" ContentType=\"application/vnd.openxmlformats-package.core-properties+xml\"/>\
<Override PartName=\"/docProps/app.xml\" ContentType=\"application/vnd.openxmlformats-officedocument.extended-properties+xml\"/>\
</Types>");

    let root_rels = "<?xml version=\"1.0\" encoding=\"UTF-8\" standalone=\"yes\"?>\
<Relationships xmlns=\"http://schemas.openxmlformats.org/package/2006/relationships\">\
<Relationship Id=\"rId1\" Type=\"http://schemas.openxmlformats.org/officeDocument/2006/relationships/officeDocument\" Target=\"ppt/presentation.xml\"/>\
<Relationship Id=\"rId2\" Type=\"http://schemas.openxmlformats.org/package/2006/relationships/metadata/core-properties\" Target=\"docProps/core.xml\"/>\
<Relationship Id=\"rId3\" Type=\"http://schemas.openxmlformats.org/officeDocument/2006/relationships/extended-properties\" Target=\"docProps/app.xml\"/>\
</Relationships>";

    // presentation.xml + rels
    let mut sld_ids = String::new();
    for i in 1..=n {
        sld_ids.push_str(&format!("<p:sldId id=\"{}\" r:id=\"rIdS{}\"/>", 256 + i, i));
    }
    let presentation = format!(
        "<?xml version=\"1.0\" encoding=\"UTF-8\" standalone=\"yes\"?>\
<p:presentation xmlns:p=\"http://schemas.openxmlformats.org/presentationml/2006/main\" xmlns:r=\"http://schemas.openxmlformats.org/officeDocument/2006/relationships\">\
<p:sldMasterIdLst><p:sldMasterId id=\"2147483648\" r:id=\"rIdMaster\"/></p:sldMasterIdLst>\
<p:sldIdLst>{sld_ids}</p:sldIdLst>\
<p:sldSz cx=\"{EMU_W}\" cy=\"{EMU_H}\"/><p:notesSz cx=\"6858000\" cy=\"9144000\"/></p:presentation>"
    );
    let mut pres_rels = String::from(
        "<?xml version=\"1.0\" encoding=\"UTF-8\" standalone=\"yes\"?>\
<Relationships xmlns=\"http://schemas.openxmlformats.org/package/2006/relationships\">\
<Relationship Id=\"rIdMaster\" Type=\"http://schemas.openxmlformats.org/officeDocument/2006/relationships/slideMaster\" Target=\"slideMasters/slideMaster1.xml\"/>\
<Relationship Id=\"rIdTheme\" Type=\"http://schemas.openxmlformats.org/officeDocument/2006/relationships/theme\" Target=\"theme/theme1.xml\"/>",
    );
    for i in 1..=n {
        pres_rels.push_str(&format!(
            "<Relationship Id=\"rIdS{}\" Type=\"http://schemas.openxmlformats.org/officeDocument/2006/relationships/slide\" Target=\"slides/slide{i}.xml\"/>",
            i
        ));
    }
    pres_rels.push_str("</Relationships>");

    // master + layouts + theme
    let master_rels = "<?xml version=\"1.0\" encoding=\"UTF-8\" standalone=\"yes\"?>\
<Relationships xmlns=\"http://schemas.openxmlformats.org/package/2006/relationships\">\
<Relationship Id=\"rId1\" Type=\"http://schemas.openxmlformats.org/officeDocument/2006/relationships/slideLayout\" Target=\"../slideLayouts/slideLayout1.xml\"/>\
<Relationship Id=\"rId2\" Type=\"http://schemas.openxmlformats.org/officeDocument/2006/relationships/slideLayout\" Target=\"../slideLayouts/slideLayout2.xml\"/>\
<Relationship Id=\"rId3\" Type=\"http://schemas.openxmlformats.org/officeDocument/2006/relationships/theme\" Target=\"../theme/theme1.xml\"/>\
</Relationships>";
    let master = format!(
        "<?xml version=\"1.0\" encoding=\"UTF-8\" standalone=\"yes\"?>\
<p:sldMaster xmlns:p=\"http://schemas.openxmlformats.org/presentationml/2006/main\" xmlns:a=\"http://schemas.openxmlformats.org/drawingml/2006/main\" xmlns:r=\"http://schemas.openxmlformats.org/officeDocument/2006/relationships\">\
<p:cSld><p:spTree><p:nvGrpSpPr><p:cNvPr id=\"1\" name=\"\"/><p:cNvGrpSpPr/><p:nvPr/></p:nvGrpSpPr><p:grpSpPr/></p:spTree></p:cSld>\
<p:clrMap bg1=\"lt1\" tx1=\"dk1\" bg2=\"lt2\" tx2=\"dk2\" accent1=\"accent1\" accent2=\"accent2\" accent3=\"accent3\" accent4=\"accent4\" accent5=\"accent5\" accent6=\"accent6\" hlink=\"hlink\" folHlink=\"folHlink\"/>\
<p:sldLayoutIdLst><p:sldLayoutId id=\"2147483649\" r:id=\"rId1\"/><p:sldLayoutId id=\"2147483650\" r:id=\"rId2\"/></p:sldLayoutIdLst>\
</p:sldMaster>"
    );
    let layout_rels = |i: u32| format!(
        "<?xml version=\"1.0\" encoding=\"UTF-8\" standalone=\"yes\"?>\
<Relationships xmlns=\"http://schemas.openxmlformats.org/package/2006/relationships\">\
<Relationship Id=\"rId1\" Type=\"http://schemas.openxmlformats.org/officeDocument/2006/relationships/slideMaster\" Target=\"../slideMasters/slideMaster1.xml\"/></Relationships>"
    );
    let layout = |i: u32, name: &str, kind: &str| {
        let body = if kind == "title" {
            "<p:sp><p:nvSpPr><p:cNvPr id=\"2\" name=\"Title\"/><p:cNvSpPr><a:spLocks noGrp=\"1\"/></p:cNvSpPr><p:nvPr><p:ph type=\"title\"/></p:nvPr></p:nvSpPr><p:spPr><a:xfrm><a:off x=\"1371600\" y=\"2286000\"/><a:ext cx=\"9448800\" cy=\"2057400\"/></a:xfrm></p:spPr><p:txBody><a:bodyPr anchor=\"ctr\"/><a:lstStyle/><a:p><a:endParaRPr lang=\"en-US\"/></a:p></p:txBody></p:sp>"
        } else {
            "<p:sp><p:nvSpPr><p:cNvPr id=\"2\" name=\"Title\"/><p:cNvSpPr><a:spLocks noGrp=\"1\"/></p:cNvSpPr><p:nvPr><p:ph type=\"title\"/></p:nvPr></p:nvSpPr><p:spPr><a:xfrm><a:off x=\"838200\" y=\"365125\"/><a:ext cx=\"10515600\" cy=\"1325563\"/></a:xfrm></p:spPr><p:txBody><a:bodyPr/><a:lstStyle/><a:p><a:endParaRPr lang=\"en-US\"/></a:p></p:txBody></p:sp>\
<p:sp><p:nvSpPr><p:cNvPr id=\"3\" name=\"Body\"/><p:cNvSpPr><a:spLocks noGrp=\"1\"/></p:cNvSpPr><p:nvPr><p:ph type=\"body\" idx=\"1\"/></p:nvPr></p:nvSpPr><p:spPr><a:xfrm><a:off x=\"838200\" y=\"1825625\"/><a:ext cx=\"10515600\" cy=\"4351338\"/></a:xfrm></p:spPr><p:txBody><a:bodyPr/><a:lstStyle/><a:p><a:endParaRPr lang=\"en-US\"/></a:p></p:txBody></p:sp>"
        };
        format!(
            "<?xml version=\"1.0\" encoding=\"UTF-8\" standalone=\"yes\"?>\
<p:sldLayout xmlns:p=\"http://schemas.openxmlformats.org/presentationml/2006/main\" xmlns:a=\"http://schemas.openxmlformats.org/drawingml/2006/main\" xmlns:r=\"http://schemas.openxmlformats.org/officeDocument/2006/relationships\">\
<p:cSld name=\"{name}\"><p:spTree><p:nvGrpSpPr><p:cNvPr id=\"1\" name=\"\"/><p:cNvGrpSpPr/><p:nvPr/></p:nvGrpSpPr><p:grpSpPr/>{body}</p:spTree></p:cSld>\
<p:clrMapOvr><a:masterClrMapping/></p:clrMapOvr></p:sldLayout>"
        )
    };
    let theme = theme_xml(&accents, deck.title.as_deref().unwrap_or("RomaCode Theme"));

    // docProps
    let date = {
        let secs = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0);
        let (y, m, d) = civil_from_days((secs / 86400) as i64);
        format!("{y:04}-{m:02}-{d:02}T00:00:00Z")
    };
    let core = format!(
        "<?xml version=\"1.0\" encoding=\"UTF-8\" standalone=\"yes\"?>\
<cp:coreProperties xmlns:cp=\"http://schemas.openxmlformats.org/package/2006/metadata/core-properties\" xmlns:dc=\"http://purl.org/dc/elements/1.1/\" xmlns:dcterms=\"http://purl.org/dc/terms/\" xmlns:xsi=\"http://www.w3.org/2001/XMLSchema-instance\">\
<dc:title>{}</dc:title><dc:creator>{}</dc:creator><cp:lastModifiedBy>{}</cp:lastModifiedBy>\
<dcterms:created xsi:type=\"dcterms:W3CDTF\">{}</dcterms:created><dcterms:modified xsi:type=\"dcterms:W3CDTF\">{}</dcterms:modified></cp:coreProperties>",
        escape(deck.title.as_deref().unwrap_or("")),
        escape(deck.author.as_deref().unwrap_or("RomaCode")),
        escape(deck.author.as_deref().unwrap_or("RomaCode")),
        date,
        date
    );
    let app = format!(
        "<?xml version=\"1.0\" encoding=\"UTF-8\" standalone=\"yes\"?>\
<Properties xmlns=\"http://schemas.openxmlformats.org/officeDocument/2006/extended-properties\"><Application>RomaCode documents engine</Application><Slides>{n}</Slides><TitlesOfParts><vt:vector size=\"{}\" baseType=\"lpstr\"><vt:lpstr>Office Theme</vt:lpstr>{}</vt:vector></TitlesOfParts></Properties>",
        1 + n,
        (1..=n).map(|i| format!("<vt:lpstr>Slide {}</vt:lpstr>", i)).collect::<String>()
    );

    // assemble
    let mut z = ZipWriter::new();
    z.add("[Content_Types].xml", ct.as_bytes());
    z.add("_rels/.rels", root_rels.as_bytes());
    z.add("docProps/core.xml", core.as_bytes());
    z.add("docProps/app.xml", app.as_bytes());
    z.add("ppt/presentation.xml", presentation.as_bytes());
    z.add("ppt/_rels/presentation.xml.rels", pres_rels.as_bytes());
    z.add("ppt/slideMasters/slideMaster1.xml", master.as_bytes());
    z.add("ppt/slideMasters/_rels/slideMaster1.xml.rels", master_rels.as_bytes());
    for i in 1..=2u32 {
        z.add(&format!("ppt/slideLayouts/slideLayout{i}.xml"), layout(i, if i == 1 { "Title Slide" } else { "Title and Content" }, if i == 1 { "title" } else { "content" }).as_bytes());
        z.add(&format!("ppt/slideLayouts/_rels/slideLayout{i}.xml.rels"), layout_rels(i).as_bytes());
    }
    z.add("ppt/theme/theme1.xml", theme.as_bytes());
    for (i, sx) in slides_xml.iter().enumerate() {
        z.add(&format!("ppt/slides/slide{}.xml", i + 1), sx.as_bytes());
        z.add(&format!("ppt/slides/_rels/slide{}.xml.rels", i + 1), slides_rels[i].as_bytes());
    }
    for (no, nx) in &notes_xml {
        z.add(&format!("ppt/notesSlides/notesSlide{no}.xml"), nx.as_bytes());
        z.add(&format!("ppt/notesSlides/_rels/notesSlide{no}.xml.rels", no = no), notes_rels_xml(*no).as_bytes());
    }
    for m in &media {
        z.add(&format!("ppt/media/{}", m.name), &m.data);
    }
    Ok(z.finish())
}

fn image_extent(data: &[u8]) -> (u32, u32) {
    if data.len() > 24 && data[0..8] == [0x89, b'P', b'N', b'G', 0x0d, 0x0a, 0x1a, 0x0a] {
        let w = u32::from_be_bytes([data[16], data[17], data[18], data[19]]);
        let h = u32::from_be_bytes([data[20], data[21], data[22], data[23]]);
        return (w, h);
    }
    if data.len() > 4 && data[0..2] == [0xFF, 0xD8] {
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
                return (w, h);
            }
            let seglen = u16::from_be_bytes([data[i + 2], data[i + 3]]) as usize;
            i += 2 + seglen.max(2);
        }
    }
    (1600, 900)
}

// ── slide XML builders ──────────────────────────────────────────────────────

fn txbody(paras: &[(String, u8, bool, u32, String)], anchor: &str, wrap: bool) -> String {
    // paras: (text, level, bold, size_pt_x100, color)
    let body = paras
        .iter()
        .map(|(text, lvl, bold, sz, color)| {
            let mut rpr = String::new();
            if *bold {
                rpr.push_str("<a:b/>");
            }
            if !color.is_empty() {
                rpr.push_str(&format!("<a:solidFill><a:srgbClr val=\"{color}\"/></a:solidFill>"));
            }
            if *sz > 0 {
                rpr.push_str(&format!("<a:sz val=\"{}\"/>", sz));
            }
            let ppr = if *lvl > 0 { format!("<a:pPr lvl=\"{lvl}\"/>") } else { String::new() };
            if text.is_empty() {
                format!("<a:p>{ppr}<a:endParaRPr lang=\"en-US\"/></a:p>")
            } else {
                format!(
                    "<a:p>{ppr}<a:r><a:rPr lang=\"en-US\" dirty=\"0\">{rpr}</a:rPr><a:t>{}</a:t></a:r></a:p>",
                    escape(text)
                )
            }
        })
        .collect::<String>();
    format!(
        "<p:txBody><a:bodyPr wrap=\"{}\" anchor=\"{anchor}\"><a:spAutoFit/></a:bodyPr><a:lstStyle/>{body}</p:txBody>",
        if wrap { "square" } else { "none" }
    )
}

fn textbox(id: usize, name: &str, x: u32, y: u32, cx: u32, cy: u32, paras: &[(String, u8, bool, u32, String)], anchor: &str) -> String {
    format!(
        "<p:sp><p:nvSpPr><p:cNvPr id=\"{id}\" name=\"{name}\"/><p:cNvSpPr txBox=\"1\"/><p:nvPr/></p:nvSpPr>\
<p:spPr><a:xfrm><a:off x=\"{x}\" y=\"{y}\"/><a:ext cx=\"{cx}\" cy=\"{cy}\"/></a:xfrm><a:prstGeom prst=\"rect\"><a:avLst/></a:prstGeom></p:spPr>{}\
</p:sp>",
        txbody(paras, anchor, true)
    )
}

fn slide_xml(slide: &Slide, idx: usize, accents: &[String], img_rel: Option<(String, (u32, u32))>) -> String {
    let mut shapes = String::new();
    let accent = &accents[0];
    let dark = "1F2937";
    match slide {
        Slide::Title { title, subtitle } => {
            shapes.push_str(&textbox(2, "Title", 1_371_600, 2_743_200, 9_448_800, 1_600_200,
                &[(title.clone(), 0, true, 4400, dark.to_string())], "ctr"));
            shapes.push_str(&textbox(3, "Subtitle", 1_371_600, 4_400_000, 9_448_800, 1_000_000,
                &[(subtitle.clone(), 0, false, 2000, "6B7280".into())], "t"));
            // accent bar
            shapes.push_str(&format!(
                "<p:sp><p:nvSpPr><p:cNvPr id=\"4\" name=\"AccentBar\"/><p:cNvSpPr/><p:nvPr/></p:nvSpPr><p:spPr><a:xfrm><a:off x=\"1371600\" y=\"2600000\"/><a:ext cx=\"2200000\" cy=\"76200\"/></a:xfrm><a:prstGeom prst=\"rect\"><a:avLst/></a:prstGeom><a:solidFill><a:srgbClr val=\"{accent}\"/></a:solidFill><a:ln><a:noFill/></a:ln></p:spPr><p:txBody><a:bodyPr/><a:lstStyle/><a:p><a:endParaRPr lang=\"en-US\"/></a:p></p:txBody></p:sp>"
            ));
        }
        Slide::Section { title } => {
            shapes.push_str(&textbox(2, "Section", 914_400, 2_743_200, 10_363_200, 1_600_200,
                &[(title.clone(), 0, true, 4000, accents[0].clone())], "ctr"));
        }
        Slide::Bullets { title, items } => {
            shapes.push_str(&textbox(2, "Title", 838_200, 365_125, 10_515_600, 1_100_000,
                &[(title.clone(), 0, true, 2800, dark.to_string())], "t"));
            let paras: Vec<(String, u8, bool, u32, String)> = items
                .iter()
                .map(|(t, l)| (t.clone(), *l, *l == 0, if *l == 0 { 1800 } else { 1600 }, if *l == 0 { dark.to_string() } else { "4B5563".into() }))
                .collect();
            shapes.push_str(&textbox(3, "Content", 838_200, 1_825_625, 10_515_600, 4_351_338, &paras, "t"));
        }
        Slide::TwoContent { title, left, right } => {
            shapes.push_str(&textbox(2, "Title", 838_200, 365_125, 10_515_600, 1_100_000,
                &[(title.clone(), 0, true, 2800, dark.to_string())], "t"));
            let l: Vec<_> = left.iter().map(|t| (t.clone(), 0, false, 1600u32, dark.to_string())).collect();
            let r: Vec<_> = right.iter().map(|t| (t.clone(), 0, false, 1600u32, dark.to_string())).collect();
            shapes.push_str(&textbox(3, "Left", 838_200, 1_825_625, 5_029_200, 4_351_338, &l, "t"));
            shapes.push_str(&textbox(4, "Right", 6_356_675, 1_825_625, 5_029_200, 4_351_338, &r, "t"));
        }
        Slide::Image { title, path, caption } => {
            let _ = path;
            shapes.push_str(&textbox(2, "Title", 838_200, 365_125, 10_515_600, 1_100_000,
                &[(title.clone(), 0, true, 2800, dark.to_string())], "t"));
            if let Some((rid, (iw, ih))) = img_rel {
                // fit the real aspect ratio inside a 9.4M x 4.2M EMU frame
                let box_w = 9_448_800f64;
                let box_h = 4_200_000f64;
                let (w, h) = if iw > 0 && ih > 0 {
                    let s = (box_w / iw as f64).min(box_h / ih as f64);
                    (iw as f64 * s, ih as f64 * s)
                } else {
                    (box_w, box_h)
                };
                let px = (1_371_600f64 + (box_w - w) / 2.0) as u32;
                let py = (1_600_000f64 + (box_h - h) / 2.0) as u32;
                shapes.push_str(&format!(
                    "<p:pic><p:nvPicPr><p:cNvPr id=\"5\" name=\"Picture\"/><p:cNvPicPr><a:picLocks noChangeAspect=\"1\"/></p:cNvPicPr><p:nvPr/></p:nvPicPr>\
<p:blipFill><a:blip r:embed=\"{rid}\"/><a:stretch><a:fillRect/></a:stretch></p:blipFill>\
<p:spPr><a:xfrm><a:off x=\"{px}\" y=\"{py}\"/><a:ext cx=\"{w}\" cy=\"{h}\"/></a:xfrm><a:prstGeom prst=\"rect\"><a:avLst/></a:prstGeom></p:spPr></p:pic>"
                ));
            }
            if let Some(cap) = caption {
                shapes.push_str(&textbox(6, "Caption", 1_371_600, 5_950_000, 9_448_800, 500_000,
                    &[(cap.clone(), 0, false, 1200, "6B7280".into())], "t"));
            }
        }
        Slide::Blank { text } => {
            if !text.is_empty() {
                shapes.push_str(&textbox(2, "Text", 838_200, 1_825_625, 10_515_600, 4_351_338,
                    &[(text.clone(), 0, false, 1800, dark.to_string())], "t"));
            }
        }
    }
    format!(
        "<?xml version=\"1.0\" encoding=\"UTF-8\" standalone=\"yes\"?>\
<p:sld xmlns:p=\"http://schemas.openxmlformats.org/presentationml/2006/main\" xmlns:a=\"http://schemas.openxmlformats.org/drawingml/2006/main\" xmlns:r=\"http://schemas.openxmlformats.org/officeDocument/2006/relationships\">\
<p:cSld><p:spTree><p:nvGrpSpPr><p:cNvPr id=\"1\" name=\"Slide {idx}\"/><p:cNvGrpSpPr/><p:nvPr/></p:nvGrpSpPr><p:grpSpPr/>{shapes}</p:spTree></p:cSld>\
<p:clrMapOvr><a:masterClrMapping/></p:clrMapOvr></p:sld>"
    )
}

fn slide_rels_xml(has_notes: bool, img_rel: Option<(String, String)>, slide_no: usize) -> String {
    let mut x = format!(
        "<?xml version=\"1.0\" encoding=\"UTF-8\" standalone=\"yes\"?>\
<Relationships xmlns=\"http://schemas.openxmlformats.org/package/2006/relationships\">\
<Relationship Id=\"rId1\" Type=\"http://schemas.openxmlformats.org/officeDocument/2006/relationships/slideLayout\" Target=\"../slideLayouts/slideLayout{}.xml\"/>",
        if slide_no == 1 { 1 } else { 2 }
    );
    if has_notes {
        x.push_str(&format!(
            "<Relationship Id=\"rIdNotes{}\" Type=\"http://schemas.openxmlformats.org/officeDocument/2006/relationships/notesSlide\" Target=\"../notesSlides/notesSlide{}.xml\"/>",
            slide_no, slide_no
        ));
    }
    if let Some((rid, media_name)) = img_rel {
        x.push_str(&format!(
            "<Relationship Id=\"{rid}\" Type=\"http://schemas.openxmlformats.org/officeDocument/2006/relationships/image\" Target=\"../media/{media_name}\"/>"
        ));
    }
    x.push_str("</Relationships>");
    x
}

fn notes_rels_xml(n: usize) -> String {
    format!(
        "<?xml version=\"1.0\" encoding=\"UTF-8\" standalone=\"yes\"?>\
<Relationships xmlns=\"http://schemas.openxmlformats.org/package/2006/relationships\">\
<Relationship Id=\"rId1\" Type=\"http://schemas.openxmlformats.org/officeDocument/2006/relationships/slide\" Target=\"../slides/slide{n}.xml\"/>\
</Relationships>"
    )
}

fn notes_xml_for(n: usize, text: &str) -> String {
    format!(
        "<?xml version=\"1.0\" encoding=\"UTF-8\" standalone=\"yes\"?>\
<p:notes xmlns:p=\"http://schemas.openxmlformats.org/presentationml/2006/main\" xmlns:a=\"http://schemas.openxmlformats.org/drawingml/2006/main\" xmlns:r=\"http://schemas.openxmlformats.org/officeDocument/2006/relationships\">\
<p:cSld><p:spTree><p:nvGrpSpPr><p:cNvPr id=\"1\" name=\"\"/><p:cNvGrpSpPr/><p:nvPr/></p:nvGrpSpPr><p:grpSpPr/>\
<p:sp><p:nvSpPr><p:cNvPr id=\"2\" name=\"Notes Placeholder\"/><p:cNvSpPr><a:spLocks noGrp=\"1\"/></p:cNvSpPr><p:nvPr><p:ph type=\"body\" idx=\"1\"/></p:nvPr></p:nvSpPr>\
<p:spPr/><p:txBody><a:bodyPr/><a:lstStyle/><a:p><a:r><a:rPr lang=\"en-US\" dirty=\"0\"/><a:t>{}</a:t></a:r></a:p></p:txBody></p:sp>\
</p:spTree></p:cSld></p:notes>",
        escape(text)
    )
}

fn theme_xml(accents: &[String], name: &str) -> String {
    format!(
        "<?xml version=\"1.0\" encoding=\"UTF-8\" standalone=\"yes\"?>\
<a:theme xmlns:a=\"http://schemas.openxmlformats.org/drawingml/2006/main\" name=\"{}\">\
<a:themeElements><a:clrScheme name=\"RomaCode\"><a:dk1><a:srgbClr val=\"1F2937\"/></a:dk1><a:lt1><a:srgbClr val=\"FFFFFF\"/></a:lt1>\
<a:dk2><a:srgbClr val=\"111827\"/></a:dk2><a:lt2><a:srgbClr val=\"F3F4F6\"/></a:lt2>\
<a:accent1><a:srgbClr val=\"{a1}\"/></a:accent1><a:accent2><a:srgbClr val=\"{a2}\"/></a:accent2>\
<a:accent3><a:srgbClr val=\"{a3}\"/></a:accent3><a:accent4><a:srgbClr val=\"{a4}\"/></a:accent4>\
<a:accent5><a:srgbClr val=\"{a5}\"/></a:accent5><a:accent6><a:srgbClr val=\"{a6}\"/></a:accent6>\
<a:hlink><a:srgbClr val=\"2563EB\"/></a:hlink><a:folHlink><a:srgbClr val=\"7C3AED\"/></a:folHlink></a:clrScheme>\
<a:fontScheme name=\"RomaCode\"><a:majorFont><a:latin typeface=\"Calibri Light\"/><a:ea typeface=\"\"/><a:cs typeface=\"\"/></a:majorFont>\
<a:minorFont><a:latin typeface=\"Calibri\"/><a:ea typeface=\"\"/><a:cs typeface=\"\"/></a:minorFont></a:fontScheme>\
<a:fmtScheme name=\"RomaCode\"><a:fillStyleLst><a:solidFill><a:schemeClr val=\"phClr\"/></a:solidFill><a:solidFill><a:schemeClr val=\"phClr\"/></a:solidFill><a:solidFill><a:schemeClr val=\"phClr\"/></a:solidFill></a:fillStyleLst>\
<a:lnStyleLst><a:ln><a:solidFill><a:schemeClr val=\"phClr\"/></a:solidFill></a:ln><a:ln><a:solidFill><a:schemeClr val=\"phClr\"/></a:solidFill></a:ln><a:ln><a:solidFill><a:schemeClr val=\"phClr\"/></a:solidFill></a:ln></a:lnStyleLst>\
<a:effectStyleLst><a:effectStyle><a:effectLst/></a:effectStyle><a:effectStyle><a:effectLst/></a:effectStyle><a:effectStyle><a:effectLst/></a:effectStyle></a:effectStyleLst>\
<a:bgFillStyleLst><a:solidFill><a:schemeClr val=\"phClr\"/></a:solidFill><a:solidFill><a:schemeClr val=\"phClr\"/></a:solidFill><a:solidFill><a:schemeClr val=\"phClr\"/></a:solidFill></a:bgFillStyleLst>\
</a:fmtScheme></a:themeElements></a:theme>",
        escape(name),
        a1 = accents[0], a2 = accents[1], a3 = accents[2], a4 = accents[3], a5 = accents[4], a6 = accents[5]
    )
}

// ── read ────────────────────────────────────────────────────────────────────

#[derive(Debug, Default)]
pub struct PptxContent {
    pub slides: Vec<(Vec<String>, Option<String>)>, // texts, notes
}

pub fn read(data: &[u8]) -> Result<PptxContent, String> {
    let z = ZipReader::open(data).map_err(|e| format!("not a pptx (zip): {e}"))?;
    let mut names = z.names();
    names.sort();
    let mut out = PptxContent::default();
    // presentation.xml gives slide order; fall back to filename order
    let slide_files: Vec<String> = if let Ok(pres) = z.read("ppt/presentation.xml") {
        let pres = String::from_utf8_lossy(&pres).into_owned();
        let rels = String::from_utf8_lossy(&z.read("ppt/_rels/presentation.xml.rels").unwrap_or_default()).into_owned();
        let mut p = Parser::new(&pres);
        let mut ids: Vec<String> = Vec::new();
        while let Some(ev) = p.next() {
            if let Event::Start(n, attrs) = ev {
                if n == "sldId" {
                    if let Some(rid) = attrs.iter().find(|(k, _)| k == "r:id").map(|(_, v)| v.clone()) {
                        ids.push(rid);
                    }
                }
            }
        }
        let resolve = |rid: &str| -> Option<String> {
            let mut p = Parser::new(&rels);
            while let Some(ev) = p.next() {
                if let Event::Start(n, attrs) = ev {
                    if n == "Relationship" {
                        let id = attrs.iter().find(|(k, _)| k == "Id").map(|(_, v)| v.clone())?;
                        if id == rid {
                            return attrs.iter().find(|(k, _)| k == "Target").map(|(_, v)| v.clone());
                        }
                    }
                }
            }
            None
        };
        let mut files = Vec::new();
        for rid in &ids {
            if let Some(t) = resolve(rid) {
                let t = t.trim_start_matches('/').to_string();
                files.push(if t.starts_with("ppt/") { t } else { format!("ppt/{t}") });
            }
        }
        if files.is_empty() {
            names.iter().filter(|n| n.starts_with("ppt/slides/slide") && n.ends_with(".xml") && !n.contains("_rels")).cloned().collect()
        } else {
            files
        }
    } else {
        return Err("missing ppt/presentation.xml — not a pptx".into());
    };
    for f in &slide_files {
        let bytes = match z.read(f) {
            Ok(b) => b,
            Err(_) => continue,
        };
        let xml = String::from_utf8_lossy(&bytes).into_owned();
        let mut p = Parser::new(&xml);
        let mut texts: Vec<String> = Vec::new();
        let mut cur = String::new();
        while let Some(ev) = p.next() {
            match ev {
                Event::Start(n, _) if n == "a:t" => cur = String::new(),
                Event::Text(t) => cur.push_str(&t),
                Event::End(n) if n == "a:t" => {
                    if !cur.trim().is_empty() {
                        texts.push(std::mem::take(&mut cur));
                    }
                }
                _ => {}
            }
        }
        // notes for this slide (match by filename number)
        let num: String = f.rsplit('/').next().unwrap_or("").trim_start_matches("slide").trim_end_matches(".xml").to_string();
        let notes = z
            .read(&format!("ppt/notesSlides/notesSlide{}.xml", num))
            .ok()
            .map(|b| {
                let xml = String::from_utf8_lossy(&b).into_owned();
                let mut p = Parser::new(&xml);
                let mut t = String::new();
                let mut cur = String::new();
                while let Some(ev) = p.next() {
                    match ev {
                        Event::Start(n2, _) if n2 == "a:t" => cur = String::new(),
                        Event::Text(t2) => cur.push_str(&t2),
                        Event::End(n2) if n2 == "a:t" => {
                            t.push_str(&cur);
                            cur.clear();
                        }
                        _ => {}
                    }
                }
                if t.trim().is_empty() { None } else { Some(t) }
            })
            .flatten();
        out.slides.push((texts, notes));
    }
    Ok(out)
}

pub fn read_path(path: &str) -> Result<PptxContent, String> {
    let data = std::fs::read(path).map_err(|e| format!("pptx {path}: {e}"))?;
    read(&data)
}

pub fn content_to_json(c: &PptxContent) -> Value {
    json!({
        "slide_count": c.slides.len(),
        "slides": c.slides.iter().enumerate().map(|(i, (texts, notes))| json!({
            "slide": i + 1,
            "texts": texts,
            "notes": notes,
        })).collect::<Vec<_>>(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn deck_round_trip() {
        let deck = Deck {
            title: Some("Q4 Review".into()),
            author: Some("Ada".into()),
            accents: Default::default(),
            slides: vec![
                (Slide::Title { title: "Q4 Review".into(), subtitle: "Finance team".into() }, None),
                (Slide::Bullets { title: "Highlights".into(), items: vec![("Revenue up 12%".into(), 0), ("Churn down".into(), 0)] }, Some("Remember the chart".into())),
                (Slide::Section { title: "Deep dive".into() }, None),
                (Slide::TwoContent { title: "Compare".into(), left: vec!["Plan A".into()], right: vec!["Plan B".into()] }, None),
                (Slide::Blank { text: "custom".into() }, None),
            ],
        };
        let bytes = build(&deck).unwrap();
        let c = read(&bytes).unwrap();
        assert_eq!(c.slides.len(), 5);
        assert!(c.slides[0].0.contains(&"Q4 Review".to_string()));
        assert!(c.slides[1].0.contains(&"Revenue up 12%".to_string()));
        assert_eq!(c.slides[1].1.as_deref(), Some("Remember the chart"));
        let z = ZipReader::open(&bytes).unwrap();
        assert!(z.read("ppt/theme/theme1.xml").is_ok());
        assert!(z.read("ppt/slideMasters/slideMaster1.xml").is_ok());
    }

    #[test]
    fn json_model() {
        let d = deck_from_json(&json!({
            "title": "T",
            "slides": [
                {"type": "title", "title": "Hello", "subtitle": "World"},
                {"type": "bullets", "title": "List", "items": ["a", {"text": "sub", "level": 1}]}
            ]
        })).unwrap();
        assert_eq!(d.slides.len(), 2);
        assert!(matches!(&d.slides[1].0, Slide::Bullets { items, .. } if items.len() == 2));
        assert!(deck_from_json(&json!({"slides": []})).is_err());
    }
}
