//! Resizable, adjustable screen: geometry read/override/reset and PNG
//! capture with box-filter rescaling.
//!
//! Two halves:
//!
//! * **Geometry** — `wm size` / `wm density` over the native ADB shell.
//!   Android reports `Physical` and `Override` lines; the effective geometry
//!   is the Override when present. `set_size` / `set_density` apply overrides
//!   (the agent's resize lever), `reset` clears them back to the panel.
//! * **Capture** — `screencap -p` (PNG straight off the device), then an
//!   optional rescale: decode → normalize to RGBA8 → box-filter (area
//!   average) → re-encode. The agent can therefore capture at native size and
//!   deliver at any size, which is exactly what a model wants.

use anyhow::{bail, Context, Result};

use crate::adb::AdbDevice;

/// Effective screen geometry (px + density).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Geometry {
    pub width: u32,
    pub height: u32,
    pub density: u32,
}

/// Sane bounds so a typo cannot send `wm size` into a 0x0 or 64K screen.
pub const MIN_SIDE_PX: u32 = 1;
pub const MAX_SIDE_PX: u32 = 16384;
pub const MIN_DENSITY: u32 = 16;
pub const MAX_DENSITY: u32 = 4096;

/// Read the effective geometry. `Override` wins over `Physical`, matching what
/// Android itself renders with. Missing density falls back to the device
/// profile default (440).
pub fn get_geometry(device: &mut AdbDevice) -> Result<Geometry> {
    let size = device.shell("wm size")?;
    let density = device.shell("wm density")?;
    let (width, height) =
        parse_wm_size(&size).context("`wm size` did not report a WxH geometry")?;
    let d = parse_wm_density(&density).unwrap_or(440);
    Ok(Geometry {
        width,
        height,
        density: d,
    })
}

/// Apply a resolution override (`wm size WxH`). Returns the effective geometry.
pub fn set_size(device: &mut AdbDevice, width: u32, height: u32) -> Result<Geometry> {
    for (label, v) in [("width", width), ("height", height)] {
        if !(MIN_SIDE_PX..=MAX_SIDE_PX).contains(&v) {
            bail!("{label} {v} out of range ({MIN_SIDE_PX}..={MAX_SIDE_PX})");
        }
    }
    device.shell(&format!("wm size {width}x{height}"))?;
    get_geometry(device)
}

/// Apply a density override (`wm density DPI`). Returns the effective geometry.
pub fn set_density(device: &mut AdbDevice, density: u32) -> Result<Geometry> {
    if !(MIN_DENSITY..=MAX_DENSITY).contains(&density) {
        bail!("density {density} out of range ({MIN_DENSITY}..={MAX_DENSITY})");
    }
    device.shell(&format!("wm density {density}"))?;
    get_geometry(device)
}

/// Clear all overrides — back to the physical panel.
pub fn reset(device: &mut AdbDevice) -> Result<Geometry> {
    device.shell("wm size reset")?;
    device.shell("wm density reset")?;
    get_geometry(device)
}

/// Parse `wm size` output. Both lines are accepted; `Override` wins.
/// Format: `Physical size: 1080x2400` (optionally with `Override size: ...`).
pub fn parse_wm_size(out: &str) -> Option<(u32, u32)> {
    let mut physical = None;
    let mut override_ = None;
    for line in out.lines() {
        let line = line.trim();
        let (label, rest) = if let Some(r) = line.strip_prefix("Override size:") {
            ("o", r.trim())
        } else if let Some(r) = line.strip_prefix("Physical size:") {
            ("p", r.trim())
        } else {
            continue;
        };
        // Some builds append a rotation marker like `1080x2400:rot0`; the
        // digits we want are the leading WxH token.
        // Some builds append a rotation marker like `1080x2400:rot0` with no
        // whitespace; cut at the first ':' as well.
        let token = rest
            .split_whitespace()
            .next()
            .unwrap_or("")
            .split(':')
            .next()
            .unwrap_or("");
        let mut it = token.split('x');
        let (w, h) = (it.next()?, it.next()?);
        let parsed = (w.parse::<u32>().ok()?, h.parse::<u32>().ok()?);
        if label == "o" {
            override_ = Some(parsed);
        } else {
            physical = Some(parsed);
        }
    }
    override_.or(physical)
}

/// Parse `wm density` output; `Override` wins. Returns raw dpi.
pub fn parse_wm_density(out: &str) -> Option<u32> {
    let mut best: Option<u32> = None;
    for line in out.lines() {
        let line = line.trim();
        let val = if let Some(r) = line.strip_prefix("Override density:") {
            r.trim()
        } else if let Some(r) = line.strip_prefix("Physical density:") {
            r.trim()
        } else {
            continue;
        };
        let token = val.split_whitespace().next().unwrap_or("");
        if let Ok(d) = token.parse::<u32>() {
            let is_override = line.starts_with("Override");
            if is_override {
                return Some(d);
            }
            best = Some(d);
        }
    }
    best
}

/// Capture the screen and optionally rescale: `scale` multiplies (must be in
/// (0, 1] — growing beyond the panel adds no information), `target_width`
/// forces an exact width with the height keeping aspect and wins over scale.
/// Returns the (possibly resized) PNG bytes and the captured source size.
pub fn capture(
    device: &mut AdbDevice,
    scale: Option<f32>,
    target_width: Option<u32>,
) -> Result<(Vec<u8>, (u32, u32))> {
    let png = device.screencap()?;
    let src = png_dimensions(&png).context("screencap output is not a PNG")?;
    if let Some(w) = target_width {
        if w == 0 || w > MAX_SIDE_PX {
            bail!("target width {w} out of range (1..={MAX_SIDE_PX})");
        }
        if w == src.0 {
            return Ok((png, src));
        }
        let h = ((src.1 as u64 * w as u64) / src.0 as u64).max(1) as u32;
        let out = reshape_bytes(&png, w, h)?;
        return Ok((out, src));
    }
    if let Some(s) = scale {
        if !(0.0..=1.0).contains(&s) || s == 0.0 {
            bail!("scale {s} out of range (0,1]");
        }
        if (s - 1.0).abs() < f32::EPSILON {
            return Ok((png, src));
        }
        let w = ((src.0 as f32 * s).round() as u32).clamp(1, MAX_SIDE_PX);
        let h = ((src.1 as f32 * s).round() as u32).clamp(1, MAX_SIDE_PX);
        let out = reshape_bytes(&png, w, h)?;
        return Ok((out, src));
    }
    Ok((png, src))
}

/// [`capture`] straight to a file.
pub fn capture_to_file(
    device: &mut AdbDevice,
    path: &str,
    scale: Option<f32>,
    target_width: Option<u32>,
) -> Result<(u32, u32)> {
    let (png, src) = capture(device, scale, target_width)?;
    std::fs::write(path, &png).with_context(|| format!("writing {path}"))?;
    Ok(src)
}

/// PNG signature + IHDR width/height sniff (big-endian at fixed offsets) —
/// enough to know a screencap's size without a full decode.
pub fn png_dimensions(png: &[u8]) -> Option<(u32, u32)> {
    const SIG: [u8; 8] = [0x89, b'P', b'N', b'G', 0x0d, 0x0a, 0x1a, 0x0a];
    if png.len() < 24 || png[..8] != SIG || &png[12..16] != b"IHDR" {
        return None;
    }
    let w = u32::from_be_bytes([png[16], png[17], png[18], png[19]]);
    let h = u32::from_be_bytes([png[20], png[21], png[22], png[23]]);
    Some((w, h))
}

/// Decode a PNG, resample to `width x height` with a box filter, re-encode.
pub fn resize_png(png: &[u8], width: u32, height: u32) -> Result<Vec<u8>> {
    if width == 0 || height == 0 || width > MAX_SIDE_PX || height > MAX_SIDE_PX {
        bail!("target {width}x{height} out of range");
    }
    reshape_bytes(png, width, height)
}

/// Device-less core of the rescale pipeline.
fn reshape_bytes(png: &[u8], width: u32, height: u32) -> Result<Vec<u8>> {
    let decoder = png::Decoder::new(std::io::Cursor::new(png));
    let mut reader = decoder.read_info().context("decoding PNG")?;
    let mut buf = vec![0u8; reader.output_buffer_size()];
    let info = reader.next_frame(&mut buf).context("reading PNG frame")?;
    let (sw, sh) = (info.width, info.height);
    let src = &buf[..info.buffer_size()];

    // Normalize any input layout to RGBA8.
    let rgba: Vec<u8> = match info.color_type {
        png::ColorType::Rgba => src.to_vec(),
        png::ColorType::Rgb => src
            .chunks_exact(3)
            .flat_map(|c| [c[0], c[1], c[2], 0xff])
            .collect(),
        png::ColorType::Grayscale => src.iter().flat_map(|g| [*g, *g, *g, 0xff]).collect(),
        png::ColorType::GrayscaleAlpha => src
            .chunks_exact(2)
            .flat_map(|g| [g[0], g[0], g[0], g[1]])
            .collect(),
        other => bail!("unsupported PNG colour type {other:?}"),
    };

    let out_rgba = if (width, height) == (sw, sh) {
        rgba
    } else {
        box_filter(&rgba, sw, sh, width, height)
    };

    encode_png(&out_rgba, width, height)
}

/// Area-average (box) resample over RGBA8: each destination pixel averages
/// every source pixel that overlaps it. Sharp on downscale (the capture
/// deliver path), acceptable on the rare upscale, and simple enough to be
/// obviously correct.
pub fn box_filter(src: &[u8], sw: u32, sh: u32, dw: u32, dh: u32) -> Vec<u8> {
    let mut out = vec![0u8; (dw * dh * 4) as usize];
    let (sx, sy) = (sw as f64 / dw as f64, sh as f64 / dh as f64);
    for dy in 0..dh {
        let y0 = (dy as f64 * sy).floor() as u32;
        let y1 = (((dy + 1) as f64 * sy).ceil() as u32).clamp(y0 + 1, sh);
        for dx in 0..dw {
            let x0 = (dx as f64 * sx).floor() as u32;
            let x1 = (((dx + 1) as f64 * sx).ceil() as u32).clamp(x0 + 1, sw);
            let (mut r, mut g, mut b, mut a) = (0u64, 0u64, 0u64, 0u64);
            for y in y0..y1 {
                for x in x0..x1 {
                    let i = ((y * sw + x) * 4) as usize;
                    r += src[i] as u64;
                    g += src[i + 1] as u64;
                    b += src[i + 2] as u64;
                    a += src[i + 3] as u64;
                }
            }
            let n = ((y1 - y0) * (x1 - x0)) as u64;
            let o = ((dy * dw + dx) * 4) as usize;
            out[o] = (r / n) as u8;
            out[o + 1] = (g / n) as u8;
            out[o + 2] = (b / n) as u8;
            out[o + 3] = (a / n) as u8;
        }
    }
    out
}

/// Encode RGBA8 as PNG.
pub fn encode_png(rgba: &[u8], width: u32, height: u32) -> Result<Vec<u8>> {
    let mut out = Vec::new();
    let mut enc = png::Encoder::new(&mut out, width, height);
    enc.set_color(png::ColorType::Rgba);
    enc.set_depth(png::BitDepth::Eight);
    let mut writer = enc.write_header().context("writing PNG header")?;
    writer
        .write_image_data(rgba)
        .context("writing PNG pixels")?;
    drop(writer);
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A tiny synthetic PNG (a red left half, blue right half) built with the
    /// crate's own encoder.
    fn sample_png(w: u32, h: u32) -> Vec<u8> {
        let mut px = Vec::with_capacity((w * h * 4) as usize);
        for _y in 0..h {
            for x in 0..w {
                if x < w / 2 {
                    px.extend_from_slice(&[0xff, 0x00, 0x00, 0xff]);
                } else {
                    px.extend_from_slice(&[0x00, 0x00, 0xff, 0xff]);
                }
            }
        }
        encode_png(&px, w, h).unwrap()
    }

    #[test]
    fn wm_size_parsing_physical_and_override() {
        let (w, h) = parse_wm_size("Physical size: 1080x2400\n").unwrap();
        assert_eq!((w, h), (1080, 2400));
        // Override wins.
        let (w, h) = parse_wm_size("Physical size: 1080x2400\nOverride size: 720x1600\n").unwrap();
        assert_eq!((w, h), (720, 1600));
        assert_eq!(parse_wm_size("nope"), None);
        // Rotation suffix tolerated.
        let (w, h) = parse_wm_size("Physical size: 1080x2400:rot0\n").unwrap();
        assert_eq!((w, h), (1080, 2400));
    }

    #[test]
    fn wm_density_parsing() {
        assert_eq!(parse_wm_density("Physical density: 440"), Some(440));
        assert_eq!(
            parse_wm_density("Physical density: 440\nOverride density: 420"),
            Some(420)
        );
        assert_eq!(parse_wm_density("-"), None);
    }

    #[test]
    fn png_dimension_sniffing() {
        let png = sample_png(8, 6);
        assert_eq!(png_dimensions(&png), Some((8, 6)));
        assert_eq!(png_dimensions(b"not a png"), None);
        assert_eq!(png_dimensions(&[]), None);
    }

    #[test]
    fn box_filter_downscale_averages() {
        // 4x1 with [red, blue, red, blue] → 2x1: each dest pixel averages
        // two source pixels → a 50/50 blend. (Integer scaling of [red,red,
        // blue,blue] would map without blending — wrong test expectation.)
        let src = vec![
            0xff, 0, 0, 0xff, 0, 0, 0xff, 0xff, 0xff, 0, 0, 0xff, 0, 0, 0xff, 0xff,
        ];
        let out = box_filter(&src, 4, 1, 2, 1);
        assert_eq!(&out[0..4], &[0x7f, 0, 0x7f, 0xff]); // (255+0)/2 = 127 integer
        assert_eq!(&out[4..8], &[0x7f, 0, 0x7f, 0xff]);
    }

    #[test]
    fn resize_round_trip_keeps_size_and_layout() {
        let png = sample_png(16, 8);
        let out = resize_png(&png, 8, 4).unwrap();
        assert_eq!(png_dimensions(&out), Some((8, 4)));
        // Identity resize returns the same geometry.
        let same = resize_png(&png, 16, 8).unwrap();
        assert_eq!(png_dimensions(&same), Some((16, 8)));
    }

    #[test]
    fn range_checks_reject_nonsense() {
        let png = sample_png(4, 4);
        assert!(resize_png(&png, 0, 4).is_err());
        assert!(resize_png(&png, 4, 99999).is_err());
    }
}
