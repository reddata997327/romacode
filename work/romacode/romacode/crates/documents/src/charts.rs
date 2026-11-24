//! Data charts — pure Rust rendering to PNG (+ SVG twin) through the
//! in-house rasterizer. Types: line, area, bar, hbar, stacked_bar, pie,
//! donut, scatter, histogram, heatmap, radar, waterfall, boxplot. Text is
//! rendered with system fonts (CJK capable); when no font exists the chart
//! still renders and the tool result notes it.

use crate::color::{palette, Rgba};
use crate::fonts::{default_family, Face, TextMeter};
use crate::raster::{encode_png, Surface};
use serde_json::{json, Value};

fn num(f: f64) -> String {
    if (f - f.round()).abs() < 1e-9 {
        format!("{}", f.round() as i64)
    } else {
        let s = format!("{f:.2}");
        s.trim_end_matches('0').trim_end_matches('.').to_string()
    }
}

/// A drawing target that renders to a raster surface and (optionally) an
/// SVG twin simultaneously.
struct Svg {
    parts: Vec<String>,
    w: u32,
    h: u32,
}

impl Svg {
    fn text(&mut self, x: f64, y: f64, s: &str, size: f64, color: Rgba, anchor: &str, weight: &str) {
        self.parts.push(format!(
            r#"<text x="{}" y="{}" font-size="{}" fill="{}" text-anchor="{}" font-weight="{}">{}</text>"#,
            num(x),
            num(y),
            size,
            color.to_hex(),
            anchor,
            weight,
            crate::xml::escape(s)
        ));
    }
    fn line(&mut self, x0: f64, y0: f64, x1: f64, y1: f64, color: Rgba, width: f64, dash: bool) {
        self.parts.push(format!(
            r#"<line x1="{}" y1="{}" x2="{}" y2="{}" stroke="{}" stroke-width="{}"{} />"#,
            num(x0),
            num(y0),
            num(x1),
            num(y1),
            color.to_hex(),
            width,
            if dash { r#" stroke-dasharray="4 3""# } else { "" }
        ));
    }
    fn rect(&mut self, x: f64, y: f64, w: f64, h: f64, fill: Rgba, stroke: Option<Rgba>, radius: f64) {
        self.parts.push(format!(
            r#"<rect x="{}" y="{}" width="{}" height="{}" fill="{}" rx="{}" {} />"#,
            num(x),
            num(y),
            num(w),
            num(h),
            fill.to_hex(),
            radius,
            match stroke {
                Some(s) => format!(r#"stroke="{}" stroke-width="1""#, s.to_hex()),
                None => String::new(),
            }
        ));
    }
    fn circle(&mut self, cx: f64, cy: f64, r: f64, fill: Rgba, stroke: Option<Rgba>) {
        self.parts.push(format!(
            r#"<circle cx="{}" cy="{}" r="{}" fill="{}" {} />"#,
            num(cx),
            num(cy),
            num(r),
            fill.to_hex(),
            match stroke {
                Some(s) => format!(r#"stroke="{}" stroke-width="1""#, s.to_hex()),
                None => String::new(),
            }
        ));
    }
    fn polygon(&mut self, pts: &[(f64, f64)], fill: Rgba, stroke: Option<Rgba>, fill_opacity: f64) {
        let ps: Vec<String> = pts.iter().map(|(x, y)| format!("{},{}", num(*x), num(*y))).collect();
        self.parts.push(format!(
            r#"<polygon points="{}" fill="{}" fill-opacity="{}" {} />"#,
            ps.join(" "),
            fill.to_hex(),
            fill_opacity,
            match stroke {
                Some(s) => format!(r#"stroke="{}" stroke-width="2""#, s.to_hex()),
                None => String::new(),
            }
        ));
    }
    fn path(&mut self, d: &str, color: Rgba, width: f64) {
        self.parts.push(format!(r#"<path d="{}" fill="none" stroke="{}" stroke-width="{}" />"#, d, color.to_hex(), width));
    }
    fn finish(self) -> String {
        format!(
            r##"<?xml version="1.0" encoding="UTF-8"?>
<svg xmlns="http://www.w3.org/2000/svg" width="{}" height="{}" viewBox="0 0 {} {}"><rect width="100%" height="100%" fill="#FFFFFF"/>{}</svg>"##,
            self.w,
            self.h,
            self.w,
            self.h,
            self.parts.join("\n")
        )
    }
}

struct Target<'a> {
    s: Surface,
    svg: Svg,
    face: Option<&'a Face>,
}

impl<'a> Target<'a> {
    fn new(w: u32, h: u32, face: Option<&'a Face>) -> Self {
        Self { s: Surface::new(w, h, Rgba::new(255, 255, 255)), svg: Svg { parts: Vec::new(), w, h }, face }
    }
    fn text(&mut self, x: f64, y: f64, s: &str, px: f64, color: Rgba, anchor: &str, weight: &str) {
        let draw_w = match anchor {
            "middle" => -measure_px(self.face, s, px) / 2.0,
            "end" => -measure_px(self.face, s, px),
            _ => 0.0,
        };
        if let Some(face) = self.face {
            let c = if weight == "bold" { Rgba::new(color.r, color.g, color.b) } else { color };
            self.s.text(face, x + draw_w, y, s, px as f32, c);
        }
        self.svg.text(x, y, s, px, color, anchor, weight);
    }
    fn line(&mut self, x0: f64, y0: f64, x1: f64, y1: f64, color: Rgba, width: f64, dash: bool) {
        self.s.line(x0, y0, x1, y1, if dash { Rgba::rgba(color.r, color.g, color.b, 140) } else { color }, width.max(1.0));
        self.svg.line(x0, y0, x1, y1, color, width, dash);
    }
    fn rect(&mut self, x: f64, y: f64, w: f64, h: f64, fill: Rgba, stroke: Option<Rgba>) {
        self.s.rect_fill(x.round() as i32, y.round() as i32, w.round() as i32, h.round() as i32, fill);
        if let Some(st) = stroke {
            self.s.rect_outline(x.round() as i32, y.round() as i32, w.round() as i32, h.round() as i32, st, 1);
        }
        self.svg.rect(x, y, w, h, fill, stroke, 0.0);
    }
    fn circle(&mut self, cx: f64, cy: f64, r: f64, fill: Rgba, stroke: Option<Rgba>) {
        self.s.circle_fill(cx.round() as i32, cy.round() as i32, r.round() as i32, fill);
        if let Some(st) = stroke {
            self.s.circle_outline(cx.round() as i32, cy.round() as i32, r.round() as i32, st, 1);
        }
        self.svg.circle(cx, cy, r, fill, stroke);
    }
    fn polygon(&mut self, pts: &[(f64, f64)], fill: Rgba, stroke: Option<Rgba>, opacity: f64) {
        let o = fill.with_alpha((255.0 * opacity.clamp(0.0, 1.0)) as u8);
        self.s.polygon_fill(pts, o);
        if let Some(st) = stroke {
            for i in 0..pts.len() {
                let a = pts[i];
                let b = pts[(i + 1) % pts.len()];
                self.s.line(a.0, a.1, b.0, b.1, st, 1.5);
            }
        }
        self.svg.polygon(pts, fill, stroke, opacity);
    }
    fn polyline(&mut self, pts: &[(f64, f64)], color: Rgba, width: f64) {
        for w in pts.windows(2) {
            self.s.line(w[0].0, w[0].1, w[1].0, w[1].1, color, width.max(1.0));
        }
        if pts.len() == 1 {
            self.circle(pts[0].0, pts[0].1, width, color, None);
        }
        let d: Vec<String> = pts
            .iter()
            .enumerate()
            .map(|(i, (x, y))| format!("{}{} {}", if i == 0 { "M" } else { "L" }, num(*x), num(*y)))
            .collect();
        self.svg.path(&d.join(" "), color, width);
    }
}

impl Rgba {
    fn with_alpha(self, a: u8) -> Self {
        Self::rgba(self.r, self.g, self.b, a)
    }
}

pub(crate) fn measure_px(face: Option<&Face>, s: &str, px: f64) -> f64 {
    match face {
        Some(f) => s.chars().map(|c| f.advance(c, px as f32) as f64).sum(),
        None => s.chars().count() as f64 * px * 0.55,
    }
}

// ── chart model ─────────────────────────────────────────────────────────────

pub struct ChartSpec {
    pub kind: String,
    pub title: Option<String>,
    pub x_label: Option<String>,
    pub y_label: Option<String>,
    pub labels: Vec<String>,
    pub series: Vec<(String, Vec<f64>, Vec<(f64, f64)>)>, // name, values, points
    pub stacked: bool,
    pub width: u32,
    pub height: u32,
    pub legend: bool,
    pub bins: usize,
    pub out_png: String,
    pub out_svg: Option<String>,
    pub palette: Option<Vec<String>>,
}

pub fn spec_from_json(v: &Value) -> Result<ChartSpec, String> {
    let kind = v["type"].as_str().unwrap_or("line").to_ascii_lowercase();
    let mut series = Vec::new();
    if let Some(arr) = v["series"].as_array() {
        for s in arr {
            let name = s["name"].as_str().unwrap_or("").to_string();
            let values = s["values"].as_array().map(|a| a.iter().filter_map(|x| x.as_f64()).collect()).unwrap_or_default();
            let points = s["points"]
                .as_array()
                .map(|a| {
                    a.iter()
                        .filter_map(|p| {
                            let pa = p.as_array()?;
                            Some((pa.first()?.as_f64()?, pa.get(1)?.as_f64()?))
                        })
                        .collect()
                })
                .unwrap_or_default();
            series.push((name, values, points));
        }
    }
    // single-series convenience: {"values": [...]}
    if series.is_empty() {
        if let Some(vals) = v["values"].as_array() {
            series.push((String::new(), vals.iter().filter_map(|x| x.as_f64()).collect(), Vec::new()));
        } else if kind == "scatter" {
            if let Some(pts) = v["points"].as_array() {
                let points = pts
                    .iter()
                    .filter_map(|p| {
                        let pa = p.as_array()?;
                        Some((pa.first()?.as_f64()?, pa.get(1)?.as_f64()?))
                    })
                    .collect();
                series.push((String::new(), Vec::new(), points));
            }
        }
    }
    if series.is_empty() {
        return Err("chart needs `series` (with values or points)".into());
    }
    Ok(ChartSpec {
        kind,
        title: v["title"].as_str().map(String::from),
        x_label: v["x_label"].as_str().map(String::from),
        y_label: v["y_label"].as_str().map(String::from),
        labels: v["labels"].as_array().map(|a| a.iter().filter_map(|x| x.as_str().map(String::from)).collect()).unwrap_or_default(),
        series,
        stacked: v["stacked"].as_bool().unwrap_or(false),
        width: v["width"].as_u64().unwrap_or(800).clamp(200, 4000) as u32,
        height: v["height"].as_u64().unwrap_or(500).clamp(150, 4000) as u32,
        legend: v["legend"].as_bool().unwrap_or(true),
        bins: v["bins"].as_u64().unwrap_or(20).clamp(2, 100) as usize,
        out_png: v["out_png"].as_str().or_else(|| v["out"].as_str()).unwrap_or("chart.png").to_string(),
        out_svg: v["out_svg"].as_str().map(String::from),
        palette: v["palette"].as_array().map(|a| a.iter().filter_map(|x| x.as_str().map(String::from)).collect()),
    })
}

fn color_for(spec: &ChartSpec, i: usize) -> Rgba {
    if let Some(p) = &spec.palette {
        if let Some(c) = p.get(i) {
            if let Some(c) = Rgba::parse(c) {
                return c;
            }
        }
    }
    palette(i)
}

// ── main render ─────────────────────────────────────────────────────────────

/// Render the chart to PNG bytes (and SVG if requested in the spec).
pub fn render(spec: &ChartSpec) -> Result<(Vec<u8>, Option<String>, Option<String>), String> {
    let face = default_family().and_then(|fp| Face::load(&fp.regular).ok());
    let mut t = Target::new(spec.width, spec.height, face.as_ref());
    let w = spec.width as f64;
    let h = spec.height as f64;
    let title_h = if spec.title.is_some() { 44.0 } else { 8.0 };
    let legend_h = if spec.legend && spec.series.len() > 1 && !matches!(spec.kind.as_str(), "pie" | "donut") { 26.0 } else { 0.0 };

    // plot area
    let margin_l = 64.0 + measure_px(face.as_ref(), spec.y_label.as_deref().unwrap_or(""), 12.0).max(0.0).min(60.0);
    let margin_b = 54.0 + legend_h;
    let margin_r = 24.0;
    let (px0, py0, px1, py1) = (margin_l, title_h + 10.0, w - margin_r, h - margin_b);
    let pw = (px1 - px0).max(40.0);
    let ph = (py1 - py0).max(40.0);

    let mut note = String::new();
    if face.is_none() {
        note = "no system font found — text skipped (set RC_DOCS_FONT)".to_string();
    }

    // title + subtitle
    if let Some(title) = &spec.title {
        t.text(w / 2.0, 26.0, title, 17.0, Rgba::new(20, 24, 33), "middle", "bold");
    }

    match spec.kind.as_str() {
        "line" => line_chart(&mut t, spec, face.as_ref(), px0, py0, pw, ph),
        "area" => area_chart(&mut t, spec, face.as_ref(), px0, py0, pw, ph),
        "bar" | "stacked_bar" => bar_chart(&mut t, spec, face.as_ref(), px0, py0, pw, ph, false),
        "hbar" => hbar_chart(&mut t, spec, face.as_ref(), px0, py0, pw, ph),
        "pie" | "donut" => pie_chart(&mut t, spec, face.as_ref(), w, h, title_h),
        "scatter" => scatter_chart(&mut t, spec, face.as_ref(), px0, py0, pw, ph),
        "histogram" => histogram_chart(&mut t, spec, face.as_ref(), px0, py0, pw, ph),
        "heatmap" => heatmap_chart(&mut t, spec, face.as_ref(), px0, py0, pw, ph),
        "radar" => radar_chart(&mut t, spec, w / 2.0, (py0 + py1) / 2.0, pw.min(ph) / 2.0 - 30.0),
        "waterfall" => waterfall_chart(&mut t, spec, face.as_ref(), px0, py0, pw, ph),
        "boxplot" => boxplot_chart(&mut t, spec, face.as_ref(), px0, py0, pw, ph),
        other => return Err(format!("unknown chart type {other:?} (line, area, bar, hbar, stacked_bar, pie, donut, scatter, histogram, heatmap, radar, waterfall, boxplot)")),
    }

    // axes labels for cartesian charts
    if matches!(spec.kind.as_str(), "line" | "area" | "bar" | "stacked_bar" | "hbar" | "scatter" | "histogram" | "waterfall" | "boxplot") {
        if let Some(xl) = &spec.x_label {
            t.text(px0 + pw / 2.0, py1 + 30.0 + legend_h, xl, 12.0, Rgba::new(80, 86, 98), "middle", "");
        }
        if let Some(yl) = &spec.y_label {
            let f = face.as_ref();
            let tw = measure_px(f, yl, 12.0);
            let _ = tw;
            // rotated text is not supported by the rasterizer; draw at top-left
            t.text(14.0, py0 + 2.0, yl, 12.0, Rgba::new(80, 86, 98), "start", "");
        }
    }

    let png = encode_png(&t.s)?;
    let svg_out = if spec.out_svg.is_some() { Some(t.svg.finish()) } else { None };
    Ok((png, svg_out, if note.is_empty() { None } else { Some(note.to_string()) }))
}

// ── axis helpers ────────────────────────────────────────────────────────────

struct Axis {
    min: f64,
    max: f64,
    ticks: Vec<f64>,
}

fn nice_axis(min: f64, max: f64, want: usize) -> Axis {
    if (max - min).abs() < 1e-12 {
        let m = if min == 0.0 { 1.0 } else { min.abs() };
        return Axis { min: min - m * 0.1, max: max + m * 0.1, ticks: vec![min, max] };
    }
    let span = max - min;
    let step0 = span / want.max(1) as f64;
    let mag = 10f64.powf(step0.abs().log10().floor());
    let step = [1.0, 2.0, 2.5, 5.0, 10.0]
        .iter()
        .map(|m| m * mag)
        .find(|s| span / s <= want.max(1) as f64)
        .unwrap_or(mag * 10.0);
    let lo = (min / step).floor() * step;
    let hi = (max / step).ceil() * step;
    let mut ticks = Vec::new();
    let mut v = lo;
    while v <= hi + step * 0.001 {
        ticks.push(v);
        v += step;
    }
    Axis { min: lo, max: hi, ticks }
}

fn fmt_num(v: f64) -> String {
    if (v - v.round()).abs() < 1e-9 {
        format!("{}", v.round() as i64)
    } else if v.abs() >= 1_000_000.0 {
        format!("{:.1}M", v / 1_000_000.0)
    } else if v.abs() >= 1_000.0 {
        format!("{:.1}k", v / 1_000.0)
    } else {
        let s = format!("{v:.2}");
        s.trim_end_matches('0').trim_end_matches('.').to_string()
    }
}

fn y_axis(t: &mut Target, ax: &Axis, _face: Option<&Face>, px0: f64, pw: f64, py1: f64, ph: f64) {
    for tv in &ax.ticks {
        let y = py1 - ((tv - ax.min) / (ax.max - ax.min)) * ph;
        t.line(px0, y, px0 + pw, y, Rgba::new(229, 233, 240), 1.0, false);
        t.text(px0 - 8.0, y + 4.0, &fmt_num(*tv), 11.0, Rgba::new(110, 116, 128), "end", "");
    }
}

// ── chart implementations ───────────────────────────────────────────────────

fn x_scale(spec: &ChartSpec, n: usize) -> Vec<(f64, String)> {
    // category positions; falls back to index labels
    let count = n.max(1);
    (0..count)
        .map(|i| {
            let label = spec.labels.get(i).cloned().unwrap_or_else(|| (i + 1).to_string());
            (i as f64, label)
        })
        .collect()
}

fn line_chart(t: &mut Target, spec: &ChartSpec, face: Option<&Face>, px0: f64, py0: f64, pw: f64, ph: f64) {
    let n = spec.series.iter().map(|(_, v, _)| v.len()).max().unwrap_or(1).max(1);
    let mut all: Vec<f64> = spec.series.iter().flat_map(|(_, v, _)| v.iter().copied()).collect();
    if all.is_empty() {
        all.push(0.0);
    }
    let ax = nice_axis(all.iter().cloned().fold(f64::MIN, f64::max).min(0.0), all.iter().cloned().fold(f64::MIN, f64::max), 5);
    y_axis(t, &ax, face, px0, pw, py0 + ph, ph);
    let py1 = py0 + ph;
    // x labels
    let cats = x_scale(spec, n);
    for (i, (_, label)) in cats.iter().enumerate() {
        let x = px0 + if n == 1 { pw / 2.0 } else { i as f64 / (n - 1).max(1) as f64 * pw };
        let short: String = label.chars().take(10).collect();
        t.text(x, py1 + 16.0, &short, 10.5, Rgba::new(110, 116, 128), "middle", "");
    }
    for (si, (name, vals, _)) in spec.series.iter().enumerate() {
        let c = color_for(spec, si);
        let pts: Vec<(f64, f64)> = vals
            .iter()
            .enumerate()
            .map(|(i, v)| {
                let x = px0 + if vals.len() == 1 { pw / 2.0 } else { i as f64 / (vals.len() - 1).max(1) as f64 * pw };
                let y = py1 - ((v - ax.min) / (ax.max - ax.min)) * ph;
                (x, y)
            })
            .collect();
        t.polyline(&pts, c, 2.5);
        for (x, y) in &pts {
            t.circle(*x, *y, 3.0, c, Some(Rgba::new(255, 255, 255)));
        }
        if spec.legend && !name.is_empty() {
            draw_legend_entry(t, spec, si, name, c, py0 + ph + 34.0, face);
        }
    }
}

fn area_chart(t: &mut Target, spec: &ChartSpec, face: Option<&Face>, px0: f64, py0: f64, pw: f64, ph: f64) {
    let n = spec.series.iter().map(|(_, v, _)| v.len()).max().unwrap_or(1).max(1);
    let mut all: Vec<f64> = spec.series.iter().flat_map(|(_, v, _)| v.iter().copied()).collect();
    if all.is_empty() {
        all.push(0.0);
    }
    let ax = nice_axis(0.0, all.iter().cloned().fold(f64::MIN, f64::max), 5);
    y_axis(t, &ax, face, px0, pw, py0 + ph, ph);
    let py1 = py0 + ph;
    let cats = x_scale(spec, n);
    for (i, (_, label)) in cats.iter().enumerate() {
        let x = px0 + if n == 1 { pw / 2.0 } else { i as f64 / (n - 1).max(1) as f64 * pw };
        let short: String = label.chars().take(10).collect();
        t.text(x, py1 + 16.0, &short, 10.5, Rgba::new(110, 116, 128), "middle", "");
    }
    for (si, (name, vals, _)) in spec.series.iter().enumerate() {
        let c = color_for(spec, si);
        let mut pts: Vec<(f64, f64)> = vals
            .iter()
            .enumerate()
            .map(|(i, v)| {
                let x = px0 + if vals.len() == 1 { pw / 2.0 } else { i as f64 / (vals.len() - 1).max(1) as f64 * pw };
                let y = py1 - ((v - ax.min) / (ax.max - ax.min)) * ph;
                (x, y)
            })
            .collect();
        pts.push((px0 + pw, py1));
        pts.push((px0, py1));
        t.polygon(&pts, c, None, 0.35);
        let top: Vec<(f64, f64)> = pts[..vals.len().max(1)].to_vec();
        t.polyline(&top, c, 2.0);
        if spec.legend && !name.is_empty() {
            draw_legend_entry(t, spec, si, name, c, py0 + ph + 34.0, face);
        }
    }
}

fn bar_chart(t: &mut Target, spec: &ChartSpec, face: Option<&Face>, px0: f64, py0: f64, pw: f64, ph: f64, _horizontal: bool) {
    let ns = spec.series.len();
    let n = spec.series.iter().map(|(_, v, _)| v.len()).max().unwrap_or(1);
    let mut all: Vec<f64> = if spec.stacked {
        (0..n)
            .map(|i| spec.series.iter().map(|(_, v, _)| v.get(i).copied().unwrap_or(0.0)).sum())
            .collect()
    } else {
        spec.series.iter().flat_map(|(_, v, _)| v.iter().copied()).collect()
    };
    if all.is_empty() {
        all.push(0.0);
    }
    let ax = nice_axis(0.0, all.iter().cloned().fold(f64::MIN, f64::max), 5);
    y_axis(t, &ax, face, px0, pw, py0 + ph, ph);
    let py1 = py0 + ph;
    let cats = x_scale(spec, n);
    let slot = pw / n.max(1) as f64;
    for (i, (_, label)) in cats.iter().enumerate() {
        let x = px0 + slot * i as f64 + slot / 2.0;
        let short: String = label.chars().take(12).collect();
        t.text(x, py1 + 16.0, &short, 10.5, Rgba::new(110, 116, 128), "middle", "");
        if spec.stacked {
            let mut acc = 0.0;
            for (si, (_, vals, _)) in spec.series.iter().enumerate() {
                let v = vals.get(i).copied().unwrap_or(0.0);
                let h0 = ((acc - ax.min) / (ax.max - ax.min)) * ph;
                let h1 = ((acc + v - ax.min) / (ax.max - ax.min)) * ph;
                let c = color_for(spec, si);
                t.rect(px0 + slot * i as f64 + slot * 0.18, py1 - h1, slot * 0.64, (h1 - h0).max(0.5), c, None);
                acc += v;
            }
        } else {
            let sub = slot * 0.8 / ns.max(1) as f64;
            for (si, (_, vals, _)) in spec.series.iter().enumerate() {
                let v = vals.get(i).copied().unwrap_or(0.0);
                let h = ((v - ax.min) / (ax.max - ax.min)) * ph;
                let c = color_for(spec, si);
                t.rect(
                    px0 + slot * i as f64 + slot * 0.1 + sub * si as f64,
                    py1 - h,
                    sub * 0.9,
                    h.max(0.5),
                    c,
                    None,
                );
            }
        }
    }
    if spec.legend {
        for (si, (name, _, _)) in spec.series.iter().enumerate() {
            if !name.is_empty() {
                draw_legend_entry(t, spec, si, name, color_for(spec, si), py0 + ph + 34.0, face);
            }
        }
    }
}

fn hbar_chart(t: &mut Target, spec: &ChartSpec, face: Option<&Face>, px0: f64, py0: f64, pw: f64, ph: f64) {
    // horizontal bars: categories on y, values on x; single series typical
    let vals: Vec<f64> = spec.series.first().map(|(_, v, _)| v.clone()).unwrap_or_default();
    let n = vals.len().max(spec.labels.len()).max(1);
    let ax = nice_axis(0.0, vals.iter().cloned().fold(f64::MIN, f64::max), 5);
    let px1 = px0 + pw;
    // x gridlines
    for tv in &ax.ticks {
        let x = px0 + ((tv - ax.min) / (ax.max - ax.min)) * pw;
        t.line(x, py0, x, py0 + ph, Rgba::new(229, 233, 240), 1.0, false);
        t.text(x, py0 + ph + 16.0, &fmt_num(*tv), 10.5, Rgba::new(110, 116, 128), "middle", "");
    }
    let slot = ph / n.max(1) as f64;
    for i in 0..n {
        let v = vals.get(i).copied().unwrap_or(0.0);
        let w = ((v - ax.min) / (ax.max - ax.min)) * pw;
        let y = py0 + slot * i as f64 + slot * 0.15;
        let c = color_for(spec, i);
        t.rect(px0, y, w.max(1.0), slot * 0.7, c, None);
        let label = spec.labels.get(i).cloned().unwrap_or_else(|| format!("cat {}", i + 1));
        let short: String = label.chars().take(14).collect();
        t.text(px0 - 8.0, y + slot * 0.5, &short, 10.5, Rgba::new(60, 66, 78), "end", "");
        if v != 0.0 {
            t.text(px0 + w.max(1.0) + 6.0, y + slot * 0.5, &fmt_num(v), 10.0, Rgba::new(110, 116, 128), "start", "");
        }
    }
    let _ = face;
    let _ = px1;
}

fn pie_chart(t: &mut Target, spec: &ChartSpec, face: Option<&Face>, w: f64, h: f64, title_h: f64) {
    let vals: Vec<f64> = spec.series.first().map(|(_, v, _)| v.clone()).unwrap_or_default();
    let total: f64 = vals.iter().sum();
    if total <= 0.0 {
        return;
    }
    let labels = &spec.labels;
    let cx = w * (if spec.legend { 0.42 } else { 0.5 });
    let cy = title_h + (h - title_h) * (if spec.legend { 0.52 } else { 0.5 });
    let r = (h - title_h).min(w) * (if spec.legend { 0.32 } else { 0.36 });
    let donut = spec.kind == "donut";
    let mut a0 = -90.0f64;
    for (i, v) in vals.iter().enumerate() {
        let frac = v / total;
        let a1 = a0 + frac * 360.0;
        let c = color_for(spec, i);
        if donut {
            let (ir, or) = (r * 0.58, r);
            let pts_outer = arc_pts(cx, cy, or, a0, a1, 24);
            let pts_inner = arc_pts(cx, cy, ir, a1, a0, 24);
            let mut pts = pts_outer;
            pts.extend(pts_inner);
            t.polygon(&pts, c, Some(Rgba::new(255, 255, 255)), 1.0);
        } else {
            let pts = arc_pts(cx, cy, r, a0, a1, 40);
            let mut full = vec![(cx, cy)];
            full.extend(pts);
            t.polygon(&full, c, Some(Rgba::new(255, 255, 255)), 1.0);
        }
        // percent label at mid-angle
        let mid = (a0 + a1) / 2.0;
        let lx = cx + (r * (if donut { 0.79 } else { 0.68 })) * mid.to_radians().cos();
        let ly = cy + (r * (if donut { 0.79 } else { 0.68 })) * mid.to_radians().sin();
        if frac > 0.04 {
            t.text(lx, ly, &format!("{:.0}%", frac * 100.0), 10.5, Rgba::new(255, 255, 255), "middle", "bold");
        }
        a0 = a1;
    }
    // legend on the right
    if spec.legend {
        let lx0 = w * 0.72;
        let lh = 22.0;
        let ly0 = title_h + (h - title_h) / 2.0 - vals.len() as f64 * lh / 2.0;
        for (i, v) in vals.iter().enumerate() {
            let y = ly0 + i as f64 * lh;
            let c = color_for(spec, i);
            t.rect(lx0, y - 9.0, 12.0, 12.0, c, None);
            let label = labels.get(i).cloned().unwrap_or_else(|| format!("item {}", i + 1));
            let short: String = label.chars().take(16).collect();
            t.text(lx0 + 20.0, y + 2.0, &short, 11.5, Rgba::new(40, 46, 58), "start", "");
            t.text(lx0 + 20.0 + 130.0, y + 2.0, &fmt_num(*v), 11.0, Rgba::new(110, 116, 128), "start", "");
        }
    }
    let _ = face;
}

fn arc_pts(cx: f64, cy: f64, r: f64, a0: f64, a1: f64, steps: usize) -> Vec<(f64, f64)> {
    let n = steps.max(2);
    (0..=n)
        .map(|i| {
            let a = (a0 + (a1 - a0) * i as f64 / n as f64).to_radians();
            (cx + r * a.cos(), cy + r * a.sin())
        })
        .collect()
}

fn scatter_chart(t: &mut Target, spec: &ChartSpec, face: Option<&Face>, px0: f64, py0: f64, pw: f64, ph: f64) {
    let mut xs: Vec<f64> = Vec::new();
    let mut ys: Vec<f64> = Vec::new();
    for (_, _, pts) in &spec.series {
        for (x, y) in pts {
            xs.push(*x);
            ys.push(*y);
        }
    }
    if xs.is_empty() {
        return;
    }
    let ax = nice_axis(xs.iter().cloned().fold(f64::MAX, f64::min), xs.iter().cloned().fold(f64::MIN, f64::max), 6);
    let ay = nice_axis(ys.iter().cloned().fold(f64::MAX, f64::min), ys.iter().cloned().fold(f64::MIN, f64::max), 5);
    let py1 = py0 + ph;
    for tv in &ax.ticks {
        let x = px0 + ((tv - ax.min) / (ax.max - ax.min)) * pw;
        t.line(x, py0, x, py1, Rgba::new(229, 233, 240), 1.0, false);
        t.text(x, py1 + 16.0, &fmt_num(*tv), 10.5, Rgba::new(110, 116, 128), "middle", "");
    }
    for tv in &ay.ticks {
        let y = py1 - ((tv - ay.min) / (ay.max - ay.min)) * ph;
        t.line(px0, y, px0 + pw, y, Rgba::new(229, 233, 240), 1.0, false);
        t.text(px0 - 8.0, y + 4.0, &fmt_num(*tv), 11.0, Rgba::new(110, 116, 128), "end", "");
    }
    for (si, (name, _, pts)) in spec.series.iter().enumerate() {
        let c = color_for(spec, si);
        for (x, y) in pts {
            let px = px0 + ((x - ax.min) / (ax.max - ax.min)) * pw;
            let py = py1 - ((y - ay.min) / (ay.max - ay.min)) * ph;
            t.circle(px, py, 4.0, c.with_alpha(200), None);
        }
        if spec.legend && !name.is_empty() {
            draw_legend_entry(t, spec, si, name, c, py1 + 34.0, face);
        }
    }
}

fn histogram_chart(t: &mut Target, spec: &ChartSpec, face: Option<&Face>, px0: f64, py0: f64, pw: f64, ph: f64) {
    let vals: Vec<f64> = spec.series.first().map(|(_, v, _)| v.clone()).unwrap_or_default();
    if vals.is_empty() {
        return;
    }
    let lo = vals.iter().cloned().fold(f64::MIN, f64::max).min(vals.iter().cloned().fold(f64::MAX, f64::min));
    let hi = vals.iter().cloned().fold(f64::MIN, f64::max);
    let bins = spec.bins;
    let bw = (hi - lo).max(1e-9) / bins as f64;
    let mut counts = vec![0usize; bins];
    for v in &vals {
        let b = (((v - lo) / bw) as usize).min(bins - 1);
        counts[b] += 1;
    }
    let ax = nice_axis(0.0, counts.iter().map(|c| *c as f64).fold(f64::MIN, f64::max), 5);
    y_axis(t, &ax, face, px0, pw, py0 + ph, ph);
    let py1 = py0 + ph;
    let slot = pw / bins as f64;
    for (i, c) in counts.iter().enumerate() {
        let h = ((*c as f64 - ax.min) / (ax.max - ax.min)) * ph;
        t.rect(px0 + slot * i as f64 + 1.0, py1 - h, slot - 2.0, h.max(0.5), color_for(spec, 0), None);
    }
    // x ticks: range edges
    let step = bins.max(1) / 5.max(1);
    for i in (0..bins).step_by(step.max(1)) {
        let x = px0 + slot * i as f64;
        t.text(x + slot * 0.5, py1 + 16.0, &fmt_num(lo + bw * i as f64), 10.0, Rgba::new(110, 116, 128), "middle", "");
    }
}

fn heatmap_chart(t: &mut Target, spec: &ChartSpec, face: Option<&Face>, px0: f64, py0: f64, pw: f64, ph: f64) {
    // series = rows; values = columns
    let rows = spec.series.len().max(1);
    let cols = spec.series.iter().map(|(_, v, _)| v.len()).max().unwrap_or(1).max(1);
    let all: Vec<f64> = spec.series.iter().flat_map(|(_, v, _)| v.iter().copied()).collect();
    let lo = all.iter().cloned().fold(f64::MAX, f64::min);
    let hi = all.iter().cloned().fold(f64::MIN, f64::max);
    let cw = pw / cols as f64;
    let ch = ph / rows as f64;
    for (ri, (_, vals, _)) in spec.series.iter().enumerate() {
        for (ci, v) in vals.iter().enumerate() {
            let f = if (hi - lo).abs() < 1e-12 { 0.5 } else { (v - lo) / (hi - lo) };
            // blue→white→red scale
            let c = if f < 0.5 {
                let k = f * 2.0;
                Rgba::new((255.0 - 195.0 * k) as u8, (240.0 - 190.0 * k) as u8, (250.0 - 100.0 * k) as u8)
            } else {
                let k = (f - 0.5) * 2.0;
                Rgba::new((60.0 + 195.0 * k) as u8, (50.0 + 190.0 * k) as u8, (150.0 - 100.0 * k) as u8)
            };
            let x = px0 + cw * ci as f64;
            let y = py0 + ch * ri as f64;
            t.rect(x + 1.0, y + 1.0, cw - 2.0, ch - 2.0, c, None);
            if cw > 34.0 && ch > 20.0 {
                t.text(x + cw / 2.0, y + ch / 2.0 + 4.0, &fmt_num(*v), 10.0, Rgba::new(30, 34, 42), "middle", "");
            }
        }
        let rlabel = if spec.series[ri].0.is_empty() { format!("row {}", ri + 1) } else { spec.series[ri].0.clone() };
        let short: String = rlabel.chars().take(10).collect();
        t.text(px0 - 8.0, py0 + ch * ri as f64 + ch / 2.0 + 4.0, &short, 10.5, Rgba::new(60, 66, 78), "end", "");
    }
    for (ci, label) in spec.labels.iter().take(cols).enumerate() {
        let short: String = label.chars().take(8).collect();
        t.text(px0 + cw * ci as f64 + cw / 2.0, py0 + ph + 16.0, &short, 10.0, Rgba::new(110, 116, 128), "middle", "");
    }
    let _ = face;
}

fn radar_chart(t: &mut Target, spec: &ChartSpec, cx: f64, cy: f64, r: f64) {
    let n = spec.labels.len().max(3);
    let max = spec
        .series
        .iter()
        .flat_map(|(_, v, _)| v.iter().copied())
        .fold(f64::MIN, f64::max)
        .max(1e-9);
    // spokes + rings
    for k in 1..=4u32 {
        let rr = r * k as f64 / 4.0;
        let ring: Vec<(f64, f64)> = (0..n)
            .map(|i| {
                let a = (i as f64 / n as f64) * std::f64::consts::TAU - std::f64::consts::FRAC_PI_2;
                (cx + rr * a.cos(), cy + rr * a.sin())
            })
            .collect();
        t.polygon(&ring, Rgba::new(235, 238, 244), None, 1.0);
    }
    for i in 0..n {
        let a = (i as f64 / n as f64) * std::f64::consts::TAU - std::f64::consts::FRAC_PI_2;
        t.line(cx, cy, cx + r * a.cos(), cy + r * a.sin(), Rgba::new(220, 224, 232), 1.0, false);
        let label = spec.labels.get(i).cloned().unwrap_or_default();
        let lx = cx + (r + 14.0) * a.cos();
        let ly = cy + (r + 14.0) * a.sin();
        let short: String = label.chars().take(12).collect();
        let anchor = if (a.cos()).abs() < 0.3 { "middle" } else if a.cos() > 0.0 { "start" } else { "end" };
        t.text(lx, ly + 4.0, &short, 10.5, Rgba::new(60, 66, 78), anchor, "");
    }
    for (si, (name, vals, _)) in spec.series.iter().enumerate() {
        let c = color_for(spec, si);
        let pts: Vec<(f64, f64)> = (0..n)
            .map(|i| {
                let v = vals.get(i).copied().unwrap_or(0.0) / max;
                let a = (i as f64 / n as f64) * std::f64::consts::TAU - std::f64::consts::FRAC_PI_2;
                (cx + r * v * a.cos(), cy + r * v * a.sin())
            })
            .collect();
        t.polygon(&pts, c, Some(c), 0.25);
        if spec.legend && !name.is_empty() {
            draw_legend_entry(t, spec, si, name, c, cy + r + 40.0, None);
        }
    }
}

fn waterfall_chart(t: &mut Target, spec: &ChartSpec, face: Option<&Face>, px0: f64, py0: f64, pw: f64, ph: f64) {
    // classic waterfall: values are deltas; running total drawn as floating bars
    let vals: Vec<f64> = spec.series.first().map(|(_, v, _)| v.clone()).unwrap_or_default();
    if vals.is_empty() {
        return;
    }
    let mut running = vec![0.0f64];
    for v in &vals[..vals.len() - 1] {
        running.push(running.last().unwrap() + v);
    }
    let mut lo = 0.0f64;
    let mut hi = 0.0f64;
    for (i, v) in vals.iter().enumerate() {
        let a = running[i];
        let b = a + v;
        lo = lo.min(a).min(b);
        hi = hi.max(a).max(b);
    }
    let ax = nice_axis(lo.min(0.0), hi.max(0.0), 5);
    y_axis(t, &ax, face, px0, pw, py0 + ph, ph);
    let py1 = py0 + ph;
    let n = vals.len();
    let slot = pw / n.max(1) as f64;
    for (i, (_, label)) in x_scale(spec, n).iter().enumerate() {
        let v = vals[i];
        let a = running[i];
        let b = a + v;
        let ya = py1 - ((a.max(b) - ax.min) / (ax.max - ax.min)) * ph;
        let yb = py1 - ((a.min(b) - ax.min) / (ax.max - ax.min)) * ph;
        let c = if v >= 0.0 { Rgba::new(44, 160, 44) } else { Rgba::new(214, 39, 40) };
        let fill = if i == n - 1 { Rgba::new(31, 119, 180) } else { c };
        t.rect(px0 + slot * i as f64 + slot * 0.15, ya, slot * 0.7, (yb - ya).max(1.0), fill, None);
        let short: String = label.chars().take(10).collect();
        t.text(px0 + slot * i as f64 + slot / 2.0, py1 + 16.0, &short, 10.0, Rgba::new(110, 116, 128), "middle", "");
        if i + 1 < n {
            let lvl = py1 - ((b - ax.min) / (ax.max - ax.min)) * ph;
            t.line(px0 + slot * i as f64 + slot * 0.85, lvl, px0 + slot * (i + 1) as f64 + slot * 0.15, lvl, Rgba::new(160, 165, 175), 1.0, true);
        }
    }
}

fn boxplot_chart(t: &mut Target, spec: &ChartSpec, face: Option<&Face>, px0: f64, py0: f64, pw: f64, ph: f64) {
    // each series = one box (values distribution)
    let mut all: Vec<f64> = Vec::new();
    let mut stats: Vec<(f64, f64, f64, f64, f64)> = Vec::new(); // min, q1, med, q3, max
    for (_, vals, _) in &spec.series {
        let mut v = vals.clone();
        v.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
        let q = |p: f64| -> f64 {
            if v.is_empty() {
                0.0
            } else {
                let idx = p * (v.len() - 1) as f64;
                let i = idx.floor() as usize;
                let fr = idx - i as f64;
                v[i] * (1.0 - fr) + v[(i + 1).min(v.len() - 1)] * fr
            }
        };
        let (min, q1, med, q3, max) = (v[0], q(0.25), q(0.5), q(0.75), v[v.len() - 1]);
        all.extend([min, max]);
        stats.push((min, q1, med, q3, max));
    }
    if all.is_empty() {
        return;
    }
    let ax = nice_axis(all.iter().cloned().fold(f64::MAX, f64::min), all.iter().cloned().fold(f64::MIN, f64::max), 5);
    y_axis(t, &ax, face, px0, pw, py0 + ph, ph);
    let py1 = py0 + ph;
    let n = stats.len();
    let slot = pw / n.max(1) as f64;
    for (i, (min, q1, med, q3, max)) in stats.iter().enumerate() {
        let x = px0 + slot * i as f64 + slot / 2.0;
        let y = |v: f64| py1 - ((v - ax.min) / (ax.max - ax.min)) * ph;
        let c = color_for(spec, i);
        // whiskers
        t.line(x, y(*max), x, y(*q3), Rgba::new(90, 96, 108), 1.5, false);
        t.line(x, y(*q1), x, y(*min), Rgba::new(90, 96, 108), 1.5, false);
        // caps
        t.line(x - slot * 0.15, y(*max), x + slot * 0.15, y(*max), Rgba::new(90, 96, 108), 1.5, false);
        t.line(x - slot * 0.15, y(*min), x + slot * 0.15, y(*min), Rgba::new(90, 96, 108), 1.5, false);
        // box
        t.rect(x - slot * 0.25, y(*q3), slot * 0.5, (y(*q1) - y(*q3)).max(1.0), c.with_alpha(120), Some(c));
        // median
        t.line(x - slot * 0.25, y(*med), x + slot * 0.25, y(*med), Rgba::new(20, 24, 33), 2.0, false);
        let label = spec.labels.get(i).cloned().unwrap_or_else(|| spec.series[i].0.clone()).chars().take(10).collect::<String>();
        t.text(x, py1 + 16.0, &label, 10.0, Rgba::new(110, 116, 128), "middle", "");
    }
}

fn draw_legend_entry(t: &mut Target, spec: &ChartSpec, si: usize, name: &str, c: Rgba, y: f64, face: Option<&Face>) {
    let _ = spec;
    let _ = face;
    // lay entries left→right across the width, evenly
    let n_series = spec.series.len().max(1);
    let w = spec.width as f64;
    let per = w / n_series.max(3).min(6) as f64;
    let idx = si % n_series.max(1);
    let x = 24.0 + per * (idx % 6) as f64;
    let row = idx / 6;
    let y = y + row as f64 * 20.0;
    t.rect(x, y - 9.0, 12.0, 12.0, c, None);
    let short: String = name.chars().take(18).collect();
    t.text(x + 18.0, y + 2.0, &short, 11.0, Rgba::new(60, 66, 78), "start", "");
}

/// Tool entry: render per spec, write files, return the JSON result.
pub fn execute(input: &Value, base: &std::path::Path) -> Result<Value, String> {
    let spec = spec_from_json(input)?;
    let (png, svg, note) = render(&spec)?;
    let out_png = resolve(base, &spec.out_png);
    if let Some(dir) = std::path::Path::new(&out_png).parent() {
        let _ = std::fs::create_dir_all(dir);
    }
    std::fs::write(&out_png, &png).map_err(|e| format!("write {}: {e}", out_png))?;
    let mut result = json!({
        "out_png": out_png,
        "png_bytes": png.len(),
        "width": spec.width,
        "height": spec.height,
        "type": spec.kind,
        "series": spec.series.len(),
    });
    if let Some(svg) = svg {
        let out_svg = resolve(base, spec.out_svg.as_deref().unwrap_or("chart.svg"));
        std::fs::write(&out_svg, &svg).map_err(|e| format!("write {out_svg}: {e}"))?;
        result["out_svg"] = json!(out_svg);
    }
    if let Some(n) = note {
        result["note"] = json!(n);
    }
    Ok(result)
}

pub(crate) fn resolve(base: &std::path::Path, p: &str) -> String {
    let path = std::path::Path::new(p);
    if path.is_absolute() {
        p.to_string()
    } else {
        base.join(path).to_string_lossy().into_owned()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_type_renders() {
        let dir = std::env::temp_dir().join("rcdocs_charts_test");
        let _ = std::fs::create_dir_all(&dir);
        for kind in ["line", "area", "bar", "hbar", "stacked_bar", "pie", "donut", "scatter", "histogram", "heatmap", "radar", "waterfall", "boxplot"] {
            let spec = ChartSpec {
                kind: kind.into(),
                title: Some(format!("{kind} demo")),
                x_label: Some("X".into()),
                y_label: Some("Y".into()),
                labels: (0..6).map(|i| format!("c{i}")).collect(),
                series: vec![
                    ("a".into(), vec![3.0, 7.0, 2.0, 9.0, 4.0, 6.0], Vec::new()),
                    ("b".into(), vec![2.0, 4.0, 6.0, 3.0, 8.0, 5.0], Vec::new()),
                ],
                stacked: false,
                width: 640,
                height: 420,
                legend: true,
                bins: 12,
                out_png: dir.join(format!("{kind}.png")).to_string_lossy().into_owned(),
                out_svg: Some(dir.join(format!("{kind}.svg")).to_string_lossy().into_owned()),
                palette: None,
            };
            let (png, svg, _) = render(&spec).unwrap();
            assert!(png.len() > 500, "{kind} png too small");
            assert!(svg.unwrap().starts_with("<?xml"));
        }
        // scatter points
        let spec = ChartSpec {
            kind: "scatter".into(),
            title: None,
            x_label: None,
            y_label: None,
            labels: vec![],
            series: vec![("p".into(), vec![], vec![(1.0, 2.0), (2.0, 4.0), (3.0, 3.0), (5.0, 8.0)])],
            stacked: false,
            width: 400,
            height: 300,
            legend: false,
            bins: 10,
            out_png: dir.join("scatter.png").to_string_lossy().into_owned(),
            out_svg: None,
            palette: None,
        };
        let (png, _, _) = render(&spec).unwrap();
        assert!(png.len() > 500);
    }

    #[test]
    fn nice_axis_picks_clean_steps() {
        let ax = nice_axis(0.0, 47.0, 5);
        assert_eq!(ax.ticks.last().copied().unwrap_or(0.0), 50.0);
        let ax = nice_axis(0.0, 100.0, 5);
        assert_eq!(ax.ticks.len(), 6);
    }
}
