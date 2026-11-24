//! System font discovery and text layout for the rasterizer (charts/canvas)
//! and the PDF writer.
//!
//! Discovery scans the well-known Linux font roots first (Noto CJK, DejaVu,
//! Liberation, WenQuanYi, LXGW), then anything `fc-list` reports. An explicit
//! override wins: `RC_DOCS_FONT`, `RC_DOCS_FONT_BOLD`, `RC_DOCS_FONT_MONO`.
//! All parsing/rasterization is ab_glyph (pure Rust).

use ab_glyph::{Font as _, ScaleFont as _};
use std::path::{Path, PathBuf};
use std::sync::OnceLock;

/// A discovered font family: regular + bold file paths.
#[derive(Clone, Debug)]
pub struct FontPaths {
    pub regular: PathBuf,
    pub bold: Option<PathBuf>,
}

static REGULAR: OnceLock<Option<FontPaths>> = OnceLock::new();
static MONO: OnceLock<Option<FontPaths>> = OnceLock::new();

fn env_first(var: &str) -> Option<PathBuf> {
    std::env::var(var).ok().map(PathBuf::from).filter(|p| p.is_file())
}

/// Candidate files, best first. `(root, patterns)`.
fn candidates() -> Vec<(PathBuf, Vec<&'static str>)> {
    let roots = [
        "/usr/share/fonts/truetype/chinese",
        "/usr/share/fonts/truetype/noto-serif-sc",
        "/usr/share/fonts/truetype/english",
        "/usr/share/fonts/truetype/wqy",
        "/usr/share/fonts/truetype/lxgw-wenkai",
        "/usr/share/fonts/opentype/noto",
        "/usr/share/fonts/truetype/noto",
        "/usr/share/fonts/truetype/dejavu",
        "/usr/share/fonts/truetype/liberation",
        "/usr/share/fonts/truetype/freefont",
        "/usr/share/fonts/TTF",
        "/usr/share/fonts",
    ];
    let sans = [
        "NotoSansSC-Regular.ttf",
        "NotoSansCJK-Regular.ttc",
        "NotoSansSC-Regular.otf",
        "NotoSans-Regular.ttf",
        "LXGWWenKai-Regular.ttf",
        "wqy-microhei.ttc",
        "DejaVuSans.ttf",
        "LiberationSans-Regular.ttf",
        "FreeSans.ttf",
        "Arial.ttf",
    ];
    let sans_bold = [
        "NotoSansSC-Bold.ttf",
        "NotoSansCJK-Bold.ttc",
        "NotoSans-Bold.ttf",
        "LXGWWenKai-Bold.ttf",
        "DejaVuSans-Bold.ttf",
        "LiberationSans-Bold.ttf",
        "FreeSansBold.ttf",
        "Arial Bold.ttf",
    ];
    let serif = ["NotoSerifSC-Regular.otf", "NotoSerifSC-Regular.ttf", "NotoSerif-Regular.ttf", "DejaVuSerif.ttf", "LiberationSerif-Regular.ttf", "FreeSerif.ttf"];
    let serif_bold = ["NotoSerifSC-Bold.otf", "NotoSerifSC-Bold.ttf", "NotoSerif-Bold.ttf", "DejaVuSerif-Bold.ttf", "LiberationSerif-Bold.ttf", "FreeSerifBold.ttf"];
    let mono = ["DejaVuSansMono.ttf", "LiberationMono-Regular.ttf", "FreeMono.ttf", "NotoMono-Regular.ttf"];
    let mono_bold = ["DejaVuSansMono-Bold.ttf", "LiberationMono-Bold.ttf", "FreeMonoBold.ttf"];
    let mut out = Vec::new();
    for root in roots {
        let p = PathBuf::from(root);
        if !p.is_dir() {
            continue;
        }
        out.push((p.clone(), sans.to_vec()));
        out.push((p.clone(), sans_bold.to_vec()));
        out.push((p.clone(), serif.to_vec()));
        out.push((p.clone(), serif_bold.to_vec()));
        out.push((p.clone(), mono.to_vec()));
        out.push((p, mono_bold.to_vec()));
    }
    out
}

fn pair(kind_sans: bool) -> Option<FontPaths> {
    // Single scan pass that keeps ordering: find the first regular match and
    // the first bold match in priority order.
    let mut regular = None;
    let mut bold = None;
    let sans: Vec<&str> = if kind_sans {
        vec!["NotoSansSC-Regular.ttf", "NotoSansCJK-Regular.ttc", "NotoSansSC-Regular.otf", "NotoSans-Regular.ttf", "LXGWWenKai-Regular.ttf", "wqy-microhei.ttc", "DejaVuSans.ttf", "LiberationSans-Regular.ttf", "FreeSans.ttf"]
    } else {
        vec!["NotoSerifSC-Regular.otf", "NotoSerifSC-Regular.ttf", "NotoSerif-Regular.ttf", "DejaVuSerif.ttf", "LiberationSerif-Regular.ttf", "FreeSerif.ttf"]
    };
    let sans_bold: Vec<&str> = if kind_sans {
        vec!["NotoSansSC-Bold.ttf", "NotoSansCJK-Bold.ttc", "NotoSans-Bold.ttf", "LXGWWenKai-Bold.ttf", "DejaVuSans-Bold.ttf", "LiberationSans-Bold.ttf", "FreeSansBold.ttf"]
    } else {
        vec!["NotoSerifSC-Bold.otf", "NotoSerifSC-Bold.ttf", "NotoSerif-Bold.ttf", "DejaVuSerif-Bold.ttf", "LiberationSerif-Bold.ttf", "FreeSerifBold.ttf"]
    };
    for (root, pats) in candidates() {
        for pat in pats {
            let p = root.join(pat);
            if !p.is_file() {
                continue;
            }
            if regular.is_none() && sans.contains(&pat) {
                regular = Some(p);
            } else if bold.is_none() && sans_bold.contains(&pat) {
                bold = Some(p);
            }
        }
        if regular.is_some() {
            break;
        }
    }
    // Bold-only fallback (rare but possible).
    if regular.is_none() {
        regular = bold.take();
    }
    regular.map(|regular| FontPaths { regular, bold })
}

/// The default (sans) family; cached.
pub fn default_family() -> Option<&'static FontPaths> {
    REGULAR
        .get_or_init(|| {
            env_first("RC_DOCS_FONT").map(|regular| FontPaths { regular, bold: env_first("RC_DOCS_FONT_BOLD") }).or_else(|| pair(true))
        })
        .as_ref()
}

/// The monospace family; cached.
pub fn mono_family() -> Option<&'static FontPaths> {
    MONO
        .get_or_init(|| {
            env_first("RC_DOCS_MONO_FONT").map(|regular| FontPaths { regular, bold: None }).or_else(|| {
                for (root, pats) in candidates() {
                    for pat in pats {
                        if pat.ends_with("Mono.ttf") && root.join(pat).is_file() {
                            return Some(FontPaths { regular: root.join(pat), bold: None });
                        }
                    }
                }
                None
            })
        })
        .as_ref()
}

// ── ab_glyph wrappers ───────────────────────────────────────────────────────

/// A loaded font face ready for metrics + rasterization.
pub struct Face {
    pub font: ab_glyph::FontRef<'static>,
    /// Keep the font bytes alive (ab_glyph borrows them).
    pub(crate) _data: &'static [u8],
}

impl Face {
    pub fn load(path: &Path) -> Result<Self, String> {
        let data = std::fs::read(path).map_err(|e| format!("font {}: {e}", path.display()))?;
        let leaked: &'static [u8] = Box::leak(data.into_boxed_slice());
        let font = ab_glyph::FontRef::try_from_slice(leaked)
            .map_err(|e| format!("font {}: {e}", path.display()))?;
        Ok(Self { font, _data: leaked })
    }

    /// Advance width of a char at the given pixel size.
    pub fn advance(&self, ch: char, px: f32) -> f32 {
        let scaled = self.font.as_scaled(px);
        let gid = scaled.glyph_id(ch);
        scaled.h_advance(gid).max(0.0)
    }

    pub fn ascent(&self, px: f32) -> f32 {
        self.font.as_scaled(px).ascent()
    }

    pub fn descent(&self, px: f32) -> f32 {
        self.font.as_scaled(px).descent()
    }

    /// Render a char to an alpha bitmap (origin = top-left of the ink box).
    pub fn rasterize(&self, ch: char, px: f32) -> GlyphBitmap {
        let gid = self.font.as_scaled(px).glyph_id(ch);
        let g = ab_glyph::Glyph { id: gid, scale: px.into(), position: ab_glyph::point(0.0, 0.0) };
        if let Some(outlined) = self.font.outline_glyph(g) {
            let bounds = outlined.px_bounds();
            let (w, h) = (bounds.width().ceil() as usize, bounds.height().ceil() as usize);
            let mut buf = vec![0u8; w * h];
            outlined.draw(|x, y, c| {
                let (x, y) = (x as usize, y as usize);
                if x < w && y < h {
                    let idx = y * w + x;
                    // ab_glyph gives coverage; combine as max coverage.
                    buf[idx] = buf[idx].max((c * 255.0) as u8);
                }
            });
            GlyphBitmap { w, h, buf, left: bounds.min.x, top: bounds.min.y }
        } else {
            GlyphBitmap { w: 0, h: 0, buf: Vec::new(), left: 0.0, top: 0.0 }
        }
    }
}

/// Rasterized glyph: coverage buffer + pen-relative ink offsets.
pub struct GlyphBitmap {
    pub w: usize,
    pub h: usize,
    pub buf: Vec<u8>,
    /// x offset relative to the pen position.
    pub left: f32,
    /// y of the bitmap top relative to the text baseline.
    pub top: f32,
}

/// Simple text measurement used by wrapping code.
pub struct TextMeter<'a> {
    pub face: &'a Face,
}

impl<'a> TextMeter<'a> {
    pub fn new(face: &'a Face) -> Self {
        Self { face }
    }

    pub fn width(&self, text: &str, px: f32) -> f32 {
        text.chars().map(|c| self.face.advance(c, px)).sum()
    }

    /// Greedy word wrap; returns lines (words kept intact, long words broken).
    pub fn wrap(&self, text: &str, px: f32, max_width: f32) -> Vec<String> {
        let mut lines = Vec::new();
        for para in text.split('\n') {
            if para.is_empty() {
                lines.push(String::new());
                continue;
            }
            let mut cur = String::new();
            let mut cur_w = 0.0f32;
            let space_w = self.face.advance(' ', px);
            for word in para.split(' ') {
                let w = self.width(word, px);
                if cur.is_empty() {
                    // A single huge word gets hard-broken per char.
                    if w > max_width {
                        for c in word.chars() {
                            let cw = self.face.advance(c, px);
                            if cur_w + cw > max_width && !cur.is_empty() {
                                lines.push(std::mem::take(&mut cur));
                                cur_w = 0.0;
                            }
                            cur.push(c);
                            cur_w += cw;
                        }
                        // keep remainder in cur
                    } else {
                        cur.push_str(word);
                        cur_w = w;
                    }
                } else if cur_w + space_w + w <= max_width {
                    cur.push(' ');
                    cur.push_str(word);
                    cur_w += space_w + w;
                } else {
                    lines.push(std::mem::take(&mut cur));
                    cur_w = 0.0;
                    if w > max_width {
                        for c in word.chars() {
                            let cw = self.face.advance(c, px);
                            if cur_w + cw > max_width && !cur.is_empty() {
                                lines.push(std::mem::take(&mut cur));
                                cur_w = 0.0;
                            }
                            cur.push(c);
                            cur_w += cw;
                        }
                    } else {
                        cur.push_str(word);
                        cur_w = w;
                    }
                }
            }
            lines.push(cur);
        }
        lines
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn some_font_is_found_or_none_gracefully() {
        // In a bare container there may be no fonts; both outcomes are valid,
        // but the discovery must not panic.
        let _ = default_family();
        let _ = mono_family();
    }

    #[test]
    fn wrap_breaks_long_text() {
        if let Some(fp) = default_family() {
            let f = Face::load(&fp.regular).unwrap();
            let m = TextMeter::new(&f);
            let lines = m.wrap("hello world this is a fairly long sentence about wrapping", 16.0, 120.0);
            assert!(lines.len() > 1);
            for l in &lines {
                assert!(m.width(l, 16.0) <= 125.0);
            }
        }
    }
}
