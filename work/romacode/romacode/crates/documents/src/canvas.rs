//! Canvas design engine — posters, flyers, social cards and other static
//! art from a JSON layout spec, rendered to PNG (raster) and optionally
//! exported to PDF. Gradients, shapes, wrapped text, images, pills and
//! badges. The pure-Rust replacement for HTML-canvas poster tooling.

use crate::charts::measure_px;
use crate::color::Rgba;
use crate::fonts::{default_family, Face, TextMeter};
use crate::pdfgen::{Block, Meta, PageSize};
use crate::raster::{encode_png, Surface};
use serde_json::{json, Value};

/// Preset page sizes for common design surfaces.
fn preset_size(name: &str) -> Option<(u32, u32)> {
    match name.to_ascii_lowercase().replace(['-', '_'], "").as_str() {
        "square" | "instagram" => Some((1080, 1080)),
        "story" => Some((1080, 1920)),
        "poster" | "a3" => Some((1587, 2245)), // A3 @96dpi portrait
        "a4" => Some((1123, 1587)),
        "a2" => Some((2245, 3171)),
        "card" | "banner" => Some((1200, 630)),
        "slide" => Some((1920, 1080)),
        "twitter" | "x" => Some((1600, 900)),
        "youtube" | "thumbnail" => Some((1280, 720)),
        _ => None,
    }
}

#[derive(Clone, Debug)]
enum El {
    Rect { x: f64, y: f64, w: f64, h: f64, fill: Option<String>, stroke: Option<String>, stroke_w: f64, radius: f64, opacity: f64 },
    Circle { cx: f64, cy: f64, r: f64, fill: Option<String>, stroke: Option<String>, opacity: f64 },
    Line { x0: f64, y0: f64, x1: f64, y1: f64, color: String, width: f64 },
    Polygon { pts: Vec<(f64, f64)>, fill: String, opacity: f64 },
    Text { x: f64, y: f64, text: String, size: f64, color: String, weight: String, align: String, max_width: Option<f64>, line_gap: f64, opacity: f64 },
    Image { path: String, x: f64, y: f64, w: f64, h: Option<f64>, radius: f64, opacity: f64 },
    Pill { x: f64, y: f64, text: String, size: f64, bg: String, color: String, pad: f64 },
}

fn parse_el(v: &Value) -> Result<El, String> {
    // Coordinates accept numbers (px) or strings ("50%") resolved against
    // the page by the caller via resolve_pct below.
    let coord = |key: &str| -> f64 { v[key].as_f64().unwrap_or(0.0) };
    let opt = |key: &str| -> Option<String> {
        v[key].as_str().map(String::from)
    };
    let num_opt = |key: &str| -> Option<f64> { v[key].as_f64() };
    let kind = v["kind"].as_str().unwrap_or("rect");
    match kind {
        "rect" | "box" => Ok(El::Rect {
            x: coord("x"), y: coord("y"), w: coord("w"), h: coord("h"),
            fill: opt("fill"), stroke: opt("stroke"), stroke_w: num_opt("stroke_w").unwrap_or(2.0),
            radius: num_opt("radius").unwrap_or(0.0), opacity: num_opt("opacity").unwrap_or(1.0),
        }),
        "circle" | "ellipse" | "dot" => Ok(El::Circle {
            cx: if v.get("cx").is_some() { coord("cx") } else { coord("x") },
            cy: if v.get("cy").is_some() { coord("cy") } else { coord("y") },
            r: num_opt("r").or_else(|| num_opt("radius")).unwrap_or(40.0),
            fill: opt("fill"), stroke: opt("stroke"), opacity: num_opt("opacity").unwrap_or(1.0),
        }),
        "line" | "rule" => Ok(El::Line {
            x0: coord("x"), y0: coord("y"), x1: num_opt("x1").unwrap_or(coord("x") + coord("w")), y1: num_opt("y1").unwrap_or(coord("y")),
            color: opt("color").unwrap_or_else(|| "#000000".into()), width: num_opt("width").unwrap_or(2.0),
        }),
        "polygon" | "triangle" => {
            let pts = v["points"]
                .as_array()
                .ok_or("polygon needs points [[x,y],...]")?
                .iter()
                .filter_map(|p| {
                    let pa = p.as_array()?;
                    Some((pa.first()?.as_f64()?, pa.get(1)?.as_f64()?))
                })
                .collect();
            Ok(El::Polygon { pts, fill: opt("fill").unwrap_or_else(|| "#3366CC".into()), opacity: num_opt("opacity").unwrap_or(1.0) })
        }
        "text" | "heading" | "title" => Ok(El::Text {
            x: coord("x"), y: coord("y"), text: v["text"].as_str().unwrap_or("").to_string(),
            size: num_opt("size").unwrap_or(if kind == "text" { 36.0 } else { 56.0 }),
            color: opt("color").unwrap_or_else(|| "#1F2937".into()),
            weight: opt("weight").unwrap_or_else(|| if kind == "text" { "normal".into() } else { "bold".into() }),
            align: opt("align").unwrap_or_else(|| "left".into()),
            max_width: num_opt("max_width").or_else(|| num_opt("w")),
            line_gap: num_opt("line_gap").unwrap_or(1.25),
            opacity: num_opt("opacity").unwrap_or(1.0),
        }),
        "image" | "photo" | "img" => Ok(El::Image {
            path: v["path"].as_str().ok_or("image needs path")?.to_string(),
            x: coord("x"), y: coord("y"),
            w: num_opt("w").unwrap_or(400.0), h: num_opt("h"),
            radius: num_opt("radius").unwrap_or(0.0), opacity: num_opt("opacity").unwrap_or(1.0),
        }),
        "pill" | "badge" | "tag" => Ok(El::Pill {
            x: coord("x"), y: coord("y"), text: v["text"].as_str().unwrap_or("").to_string(),
            size: num_opt("size").unwrap_or(20.0), bg: opt("bg").unwrap_or_else(|| "#1F77B4".into()),
            color: opt("color").unwrap_or_else(|| "#FFFFFF".into()), pad: num_opt("pad").unwrap_or(18.0),
        }),
        other => Err(format!("unknown element kind {other:?} (rect, circle, line, polygon, text, image, pill)")),
    }
}

/// Parse "50%" strings against the page dimension.
fn resolve_pct(base: f64, v: &Value, key: &str) -> f64 {
    if let Some(s) = v[key].as_str() {
        if let Some(p) = s.strip_suffix('%') {
            return base * p.trim().parse::<f64>().unwrap_or(0.0) / 100.0;
        }
        return s.parse().unwrap_or(0.0);
    }
    v[key].as_f64().unwrap_or(0.0)
}

/// Render a design: returns (png_bytes, pdf_bytes?)
pub fn render(input: &Value) -> Result<(Vec<u8>, Option<Vec<u8>>), String> {
    let preset = input["preset"].as_str().map(String::from);
    let w = input["width"].as_f64().unwrap_or_else(|| {
        preset.as_deref().and_then(preset_size).map(|s| s.0 as f64).unwrap_or(1080.0)
    });
    let h = input["height"].as_f64().unwrap_or_else(|| {
        preset.as_deref().and_then(preset_size).map(|s| s.1 as f64).unwrap_or(1350.0)
    });
    let w = w.clamp(50.0, 8000.0) as u32;
    let h = h.clamp(50.0, 8000.0) as u32;
    let wf = w as f64;
    let hf = h as f64;

    let face = default_family().and_then(|fp| Face::load(&fp.regular).ok());
    let mut s = Surface::new(w, h, Rgba::new(255, 255, 255));

    // background
    let bg = &input["background"];
    if bg.is_object() {
        let kind = bg["type"].as_str().unwrap_or("solid");
        match kind {
            "gradient" => {
                let from = Rgba::parse_or(bg["from"].as_str().unwrap_or("#1F77B4"), Rgba::new(31, 119, 180));
                let to = Rgba::parse_or(bg["to"].as_str().unwrap_or("#5B9BD5"), Rgba::new(91, 155, 213));
                let vertical = bg["direction"].as_str().map(|d| d != "horizontal").unwrap_or(true);
                s.gradient(0, 0, w as i32, h as i32, from, to, vertical);
            }
            _ => {
                let c = Rgba::parse_or(bg["color"].as_str().unwrap_or("#FFFFFF"), Rgba::new(255, 255, 255));
                s.fill(c);
            }
        }
    } else if let Some(c) = bg.as_str() {
        s.fill(Rgba::parse_or(c, Rgba::new(255, 255, 255)));
    }

    // elements
    let els = input["elements"].as_array().ok_or("design needs elements: []")?;
    for el in els {
        // resolve percentages per element by patching the view
        let e = resolve_el(&el, wf, hf)?;
        draw_el(&mut s, &e, face.as_ref(), wf, hf)?;
    }

    let png = encode_png(&s)?;

    // optional PDF export: the raster becomes a full-bleed image page
    let pdf = if input.get("out_pdf").is_some() {
        Some(render_pdf_page(&png, w, h)?)
    } else {
        None
    };

    Ok((png, pdf))
}

fn resolve_el(el: &Value, wf: f64, hf: f64) -> Result<Value, String> {
    // Rewrite numeric fields that carry percent strings into absolute px.
    let mut v = el.clone();
    for key in ["x", "y", "w", "h", "x1", "y1", "cx", "cy", "r", "max_width"] {
        if let Some(sv) = v[key].as_str() {
            if sv.ends_with('%') {
                let base = match key {
                    "y" | "y1" | "cy" | "h" => hf,
                    _ => wf,
                };
                v[key] = json!(resolve_pct(base, &v, key));
            } else {
                v[key] = json!(sv.parse::<f64>().unwrap_or(0.0));
            }
        } else if v[key].is_number() {
            // keep
        }
    }
    Ok(v)
}

fn draw_el(s: &mut Surface, el: &Value, face: Option<&Face>, wf: f64, hf: f64) -> Result<(), String> {
    let e = parse_el(el)?;
    let _ = (wf, hf);
    match e {
        El::Rect { x, y, w, h, fill, stroke, stroke_w, radius, opacity } => {
            let f = fill.map(|c| Rgba::parse_or(&c, Rgba::new(31, 119, 180)).with_a((255.0 * opacity) as u8));
            let st = stroke.map(|c| Rgba::parse_or(&c, Rgba::new(30, 30, 30)));
            if let Some(f) = f {
                if radius > 0.0 {
                    s.round_rect_fill(x as i32, y as i32, w.max(1.0) as i32, h.max(1.0) as i32, radius as i32, f);
                } else {
                    s.rect_fill(x as i32, y as i32, w.max(1.0) as i32, h.max(1.0) as i32, f);
                }
            }
            if let Some(st) = st {
                s.rect_outline(x as i32, y as i32, w.max(1.0) as i32, h.max(1.0) as i32, st, stroke_w.max(1.0) as i32);
            }
        }
        El::Circle { cx, cy, r, fill, stroke, opacity } => {
            let f = fill.map(|c| Rgba::parse_or(&c, Rgba::new(31, 119, 180)).with_a((255.0 * opacity) as u8));
            let st = stroke.map(|c| Rgba::parse_or(&c, Rgba::new(30, 30, 30)));
            if let Some(f) = f {
                s.circle_fill(cx as i32, cy as i32, r as i32, f);
            }
            if let Some(st) = st {
                s.circle_outline(cx as i32, cy as i32, r as i32, st, stroke_w_of(el, 2.0) as i32);
            }
        }
        El::Line { x0, y0, x1, y1, color, width } => {
            let c = Rgba::parse_or(&color, Rgba::new(30, 30, 30));
            s.line(x0, y0, x1, y1, c, width.max(1.0));
        }
        El::Polygon { pts, fill, opacity } => {
            let c = Rgba::parse_or(&fill, Rgba::new(51, 102, 204)).with_a((255.0 * opacity) as u8);
            s.polygon_fill(&pts, c);
        }
        El::Text { x, y, text, size, color, weight, align, max_width, line_gap, opacity } => {
            let Some(face) = face else { return Ok(()) };
            let c = Rgba::parse_or(&color, Rgba::new(31, 41, 55)).with_a((255.0 * opacity) as u8);
            let meter = TextMeter::new(face);
            let lines = match max_width {
                Some(mw) if mw > 0.0 => meter.wrap(&text, size as f32, mw as f32),
                _ => text.split('\n').map(String::from).collect(),
            };
            let ascent = face.ascent(size as f32) as f64;
            let lh = (ascent + face.descent(size as f32).abs() as f64) * line_gap.max(1.0);
            for (i, line) in lines.iter().enumerate() {
                let lw = measure_px(Some(face), line, size);
                let tx = match align.as_str() {
                    "center" | "middle" => x - lw / 2.0,
                    "right" | "end" => x - lw,
                    _ => x,
                };
                s.text(face, tx, y + i as f64 * lh, line, size as f32, c);
            }
        }
        El::Image { path, x, y, w, h, radius, opacity } => {
            let data = std::fs::read(&path).map_err(|e| format!("image {path}: {e}"))?;
            let surf = crate::raster::decode_png(&data)
                .map_err(|_| format!("canvas images must be PNG (got {path}): convert JPEG with the chart/image tools first"))?;
            let scale = w / surf.w.max(1) as f64;
            let dh = h.unwrap_or(surf.h as f64 * scale);
            // nearest-neighbor scale into a temp surface
            let dw_px = w.round().max(1.0) as u32;
            let dh_px = dh.round().max(1.0) as u32;
            let mut scaled = Surface::new(dw_px, dh_px, Rgba::new(255, 255, 255));
            for yy in 0..dh_px {
                for xx in 0..dw_px {
                    let sx = (xx as f64 / dw_px as f64 * surf.w as f64) as u32;
                    let sy = (yy as f64 / dh_px as f64 * surf.h as f64) as u32;
                    let idx = ((sy * surf.w + sx) * 4) as usize;
                    let c = Rgba::rgba(surf.px[idx], surf.px[idx + 1], surf.px[idx + 2], ((surf.px[idx + 3] as f64) * opacity) as u8);
                    scaled.set(xx as i32, yy as i32, c);
                }
            }
            blit(s, &scaled, x as i32, y as i32, radius as i32);
        }
        El::Pill { x, y, text, size, bg, color, pad } => {
            let Some(face) = face else { return Ok(()) };
            let bg_c = Rgba::parse_or(&bg, Rgba::new(31, 119, 180));
            let tx_c = Rgba::parse_or(&color, Rgba::new(255, 255, 255));
            let tw = measure_px(Some(face), &text, size);
            let pw = tw + pad * 2.0;
            let ph = size * 1.7;
            s.round_rect_fill(x as i32, y as i32, pw as i32, ph as i32, (ph / 2.0) as i32, bg_c);
            s.text(face, x + pad, y + ph / 2.0 + size * 0.35, &text, size as f32, tx_c);
        }
    }
    Ok(())
}

fn stroke_w_of(el: &Value, d: f64) -> f64 {
    el["stroke_w"].as_f64().unwrap_or(d)
}

impl Rgba {
    fn with_a(self, a: u8) -> Self {
        Self::rgba(self.r, self.g, self.b, a)
    }
}

fn blit(dst: &mut Surface, src: &Surface, x: i32, y: i32, radius: i32) {
    for yy in 0..src.h as i32 {
        for xx in 0..src.w as i32 {
            // rounded-corner mask
            if radius > 0 {
                let r = radius.min(src.w as i32 / 2).min(src.h as i32 / 2);
                let in_corner_x = xx < r || xx >= src.w as i32 - r;
                let in_corner_y = yy < r || yy >= src.h as i32 - r;
                if in_corner_x && in_corner_y {
                    let cx = if xx < r { r } else { src.w as i32 - r - 1 };
                    let cy = if yy < r { r } else { src.h as i32 - r - 1 };
                    let dx = (xx - cx) as f64;
                    let dy = (yy - cy) as f64;
                    if dx * dx + dy * dy > (r * r) as f64 {
                        continue;
                    }
                }
            }
            let idx = ((yy * src.w as i32 + xx) * 4) as usize;
            let c = Rgba::rgba(src.px[idx], src.px[idx + 1], src.px[idx + 2], src.px[idx + 3]);
            dst.blend(x + xx, y + yy, c);
        }
    }
}

/// Export a rendered design as a single-page PDF embedding the PNG full-bleed.
fn render_pdf_page(png: &[u8], w: u32, h: u32) -> Result<Vec<u8>, String> {
    let surf = crate::raster::decode_png(png)?;
    let mut rgb = Vec::with_capacity((surf.w * surf.h * 3) as usize);
    for px in surf.px.chunks_exact(4) {
        let a = px[3] as u32;
        let mix = |fg: u8, bg: u8| ((fg as u32 * a + bg as u32 * (255 - a)) / 255) as u8;
        rgb.extend_from_slice(&[mix(px[0], 255), mix(px[1], 255), mix(px[2], 255)]);
    }
    // page size in points: px * 0.75 (96 dpi → 72 dpi)
    let pw = w as f64 * 0.75;
    let ph = h as f64 * 0.75;
    let img = crate::pdfgen::Block::Image { path: String::new(), width_pt: Some(pw), caption: None };
    let _ = img; // we need a custom path: build via Builder with a temp image file? Simpler: dedicated minimal serializer below.
    let _ = rgb;
    let _ = (pw, ph);
    // Reuse the pdfgen builder by writing the PNG to a temp file — simplest
    // correct path that keeps one PDF implementation.
    let tmp = std::env::temp_dir().join(format!("rcdocs-canvas-{}.png", std::process::id()));
    std::fs::write(&tmp, png).map_err(|e| format!("tmp png: {e}"))?;
    let mut b = crate::pdfgen::Builder::new(
        PageSize::Custom(pw, ph),
        0.0,
        Meta { title: "Canvas design".into(), author: "RomaCode".into(), subject: String::new() },
        false,
        Rgba::new(0, 0, 0),
    );
    b.add(Block::Image { path: tmp.to_string_lossy().into_owned(), width_pt: Some(pw), caption: None })?;
    let bytes = b.finish()?;
    let _ = std::fs::remove_file(&tmp);
    Ok(bytes)
}

/// Tool entry for `canvas_design`.
pub fn execute(input: &Value, base: &std::path::Path) -> Result<Value, String> {
    let (png, pdf) = render(input)?;
    let out_png = crate::charts::resolve(base, input["out_png"].as_str().unwrap_or("design.png"));
    if let Some(dir) = std::path::Path::new(&out_png).parent() {
        let _ = std::fs::create_dir_all(dir);
    }
    std::fs::write(&out_png, &png).map_err(|e| format!("write {out_png}: {e}"))?;
    let mut result = json!({
        "out_png": out_png,
        "png_bytes": png.len(),
        "width": input["width"].as_f64().or_else(|| input["preset"].as_str().and_then(preset_size).map(|s| s.0 as f64)).unwrap_or(1080.0),
        "elements": input["elements"].as_array().map(|a| a.len()).unwrap_or(0),
    });
    if let Some(pdf) = pdf {
        let out_pdf = crate::charts::resolve(base, input["out_pdf"].as_str().unwrap_or("design.pdf"));
        std::fs::write(&out_pdf, &pdf).map_err(|e| format!("write {out_pdf}: {e}"))?;
        result["out_pdf"] = json!(out_pdf);
        result["pdf_bytes"] = json!(pdf.len());
    }
    Ok(result)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn poster_renders() {
        let dir = std::env::temp_dir().join("rcdocs_canvas_test");
        let _ = std::fs::create_dir_all(&dir);
        let design = json!({
            "preset": "square",
            "background": {"type": "gradient", "from": "#0F172A", "to": "#1F77B4", "direction": "vertical"},
            "elements": [
                {"kind": "rect", "x": "6%", "y": "8%", "w": "88%", "h": "84%", "fill": "#FFFFFF", "radius": 24, "opacity": 0.95},
                {"kind": "pill", "x": "10%", "y": "14%", "text": "ROMACODE", "size": 22, "bg": "#1F77B4"},
                {"kind": "text", "x": "10%", "y": "26%", "text": "Documents engine", "size": 64, "weight": "bold", "max_width": "80%"},
                {"kind": "text", "x": "10%", "y": "42%", "text": "Word, Excel, PowerPoint, PDF, charts and Figma — rebuilt in pure Rust as builtin native tools.", "size": 30, "color": "#475569", "max_width": "80%"},
                {"kind": "line", "x": "10%", "y": "88%", "w": "80%", "color": "#1F77B4", "width": 4},
                {"kind": "circle", "cx": "88%", "cy": "90%", "r": 30, "fill": "#F59E0B"}
            ],
            "out_png": dir.join("poster.png").to_string_lossy().into_owned(),
        });
        let (png, _) = render(&design).unwrap();
        assert!(png.len() > 10_000, "poster too small: {}", png.len());
        // non-uniform background actually drew
        let surf = crate::raster::decode_png(&png).unwrap();
        let c1 = &surf.px[0..4];
        let mid = ((surf.h / 2 * surf.w) * 4) as usize;
        assert_ne!(c1[0], surf.px[mid]);
    }
}
