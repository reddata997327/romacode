//! Colors: hex parsing, named CSS-ish palette and the shared chart/canvas
//! palettes (a RomaCode-branded categorical ramp first, then color-blind
//! friendly fallbacks).

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Rgba {
    pub r: u8,
    pub g: u8,
    pub b: u8,
    pub a: u8,
}

impl Rgba {
    pub const fn new(r: u8, g: u8, b: u8) -> Self {
        Self { r, g, b, a: 255 }
    }
    pub const fn rgba(r: u8, g: u8, b: u8, a: u8) -> Self {
        Self { r, g, b, a }
    }
    /// Parse `#rgb`, `#rrggbb`, `#rrggbbaa` or a palette name.
    pub fn parse(s: &str) -> Option<Self> {
        let s = s.trim();
        if let Some(hex) = s.strip_prefix('#') {
            let h = hex.chars().filter(|c| c.is_ascii_hexdigit()).collect::<String>();
            if h.len() != hex.len() {
                return None;
            }
            return match h.len() {
                3 => {
                    let v = u32::from_str_radix(&h, 16).ok()?;
                    Some(Self::new(((v >> 8 & 0xF) as u8) * 17, ((v >> 4 & 0xF) as u8) * 17, (v & 0xF) as u8 * 17))
                }
                6 => {
                    let v = u32::from_str_radix(&h, 16).ok()?;
                    Some(Self::new((v >> 16) as u8, (v >> 8) as u8, v as u8))
                }
                8 => {
                    let v = u64::from_str_radix(&h, 16).ok()?;
                    Some(Self::rgba((v >> 24) as u8, (v >> 16) as u8, (v >> 8) as u8, v as u8))
                }
                _ => None,
            };
        }
        let lower = s.to_ascii_lowercase().replace([' ', '_'], "");
        NAMED.iter().find(|(n, _)| *n == lower).map(|(_, c)| *c)
    }

    /// Parse with a fallback color on failure.
    pub fn parse_or(s: &str, fallback: Self) -> Self {
        Self::parse(s).unwrap_or(fallback)
    }

    /// Blend `self` over `bg` using self's alpha.
    pub fn over(self, bg: Self) -> Self {
        let sa = self.a as u32;
        let ba = bg.a as u32;
        let oa = sa + ba * (255 - sa) / 255;
        if oa == 0 {
            return Self::rgba(0, 0, 0, 0);
        }
        let mix = |s: u32, b: u32| ((s * sa + b * ba * (255 - sa) / 255) / oa).min(255) as u8;
        Self::rgba(mix(self.r as u32, bg.r as u32), mix(self.g as u32, bg.g as u32), mix(self.b as u32, bg.b as u32), oa as u8)
    }

    pub fn to_hex(self) -> String {
        format!("#{:02X}{:02X}{:02X}", self.r, self.g, self.b)
    }

    /// Darken by factor 0..1 (0 = unchanged).
    pub fn darken(self, f: f64) -> Self {
        let k = 1.0 - f.clamp(0.0, 1.0);
        Self::new(
            (self.r as f64 * k) as u8,
            (self.g as f64 * k) as u8,
            (self.b as f64 * k) as u8,
        )
    }
}

impl From<(u8, u8, u8)> for Rgba {
    fn from(t: (u8, u8, u8)) -> Self {
        Self::new(t.0, t.1, t.2)
    }
}

/// Named colors (a practical subset of CSS).
pub const NAMED: &[(&str, Rgba)] = &[
    ("black", Rgba::new(0, 0, 0)),
    ("white", Rgba::new(255, 255, 255)),
    ("red", Rgba::new(220, 53, 69)),
    ("crimson", Rgba::new(220, 20, 60)),
    ("orange", Rgba::new(255, 127, 14)),
    ("darkorange", Rgba::new(255, 140, 0)),
    ("gold", Rgba::new(255, 193, 7)),
    ("yellow", Rgba::new(255, 205, 86)),
    ("olive", Rgba::new(128, 128, 0)),
    ("lime", Rgba::new(50, 205, 50)),
    ("green", Rgba::new(25, 135, 84)),
    ("darkgreen", Rgba::new(0, 100, 0)),
    ("teal", Rgba::new(32, 178, 170)),
    ("cyan", Rgba::new(6, 182, 212)),
    ("skyblue", Rgba::new(135, 206, 235)),
    ("blue", Rgba::new(13, 110, 253)),
    ("navy", Rgba::new(0, 0, 128)),
    ("indigo", Rgba::new(88, 86, 214)),
    ("purple", Rgba::new(111, 66, 193)),
    ("magenta", Rgba::new(214, 51, 132)),
    ("pink", Rgba::new(214, 51, 132)),
    ("brown", Rgba::new(134, 100, 65)),
    ("gray", Rgba::new(108, 117, 125)),
    ("grey", Rgba::new(108, 117, 125)),
    ("darkgray", Rgba::new(52, 58, 64)),
    ("darkgrey", Rgba::new(52, 58, 64)),
    ("lightgray", Rgba::new(233, 236, 239)),
    ("lightgrey", Rgba::new(233, 236, 239)),
    ("silver", Rgba::new(192, 192, 192)),
    ("beige", Rgba::new(245, 245, 220)),
    ("ivory", Rgba::new(255, 255, 240)),
    ("maroon", Rgba::new(128, 0, 0)),
    ("coral", Rgba::new(255, 127, 80)),
    ("tomato", Rgba::new(255, 99, 71)),
    ("steelblue", Rgba::new(70, 130, 180)),
    ("royalblue", Rgba::new(65, 105, 225)),
    ("slategray", Rgba::new(112, 128, 144)),
];

/// Default categorical palette (matplotlib-like, tuned).
pub const PALETTE: &[Rgba] = &[
    Rgba::new(31, 119, 180),
    Rgba::new(255, 127, 14),
    Rgba::new(44, 160, 44),
    Rgba::new(214, 39, 40),
    Rgba::new(148, 103, 189),
    Rgba::new(140, 86, 75),
    Rgba::new(227, 119, 194),
    Rgba::new(127, 127, 127),
    Rgba::new(188, 189, 34),
    Rgba::new(23, 190, 207),
];

pub fn palette(i: usize) -> Rgba {
    PALETTE[i % PALETTE.len()]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parsing() {
        assert_eq!(Rgba::parse("#FF0000"), Some(Rgba::new(255, 0, 0)));
        assert_eq!(Rgba::parse("#f00"), Some(Rgba::new(255, 0, 0)));
        assert_eq!(Rgba::parse("steelblue"), Some(Rgba::new(70, 130, 180)));
        assert_eq!(Rgba::parse("#11223344"), Some(Rgba::rgba(0x11, 0x22, 0x33, 0x44)));
        assert_eq!(Rgba::parse("nope"), None);
    }

    #[test]
    fn alpha_blend() {
        let red = Rgba::rgba(255, 0, 0, 128);
        let bg = Rgba::new(0, 0, 255);
        let out = red.over(bg);
        assert_eq!((out.r, out.b), (128, 127));
    }
}
