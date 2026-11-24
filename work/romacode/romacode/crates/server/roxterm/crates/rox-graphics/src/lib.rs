//! Kitty graphics protocol support (the `ESC _ G ... ESC \` APC family).
//!
//! The store owns transmitted bitmaps and in-flight chunked transmits.
//! [`GraphicsStore::feed`] consumes one APC payload and emits typed events;
//! the VT screen turns placement events into grid [`ImagePlacement`]s.

use std::collections::HashMap;

use thiserror::Error;

const MAX_APC_BYTES: usize = 4 * 1024 * 1024;
const MAX_PENDING_TRANSMITS: usize = 16;
const MAX_IMAGE_EDGE: u32 = 8192;
const MAX_IMAGE_PIXELS: u64 = 16 * 1024 * 1024;
const MAX_STORED_IMAGES: usize = 64;
const MAX_STORED_RGBA_BYTES: usize = 64 * 1024 * 1024;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PxFormat {
    /// f=0 / f=100: payload is a PNG file.
    Png,
    /// f=24: 3 bytes per pixel.
    Rgb,
    /// f=32: 4 bytes per pixel.
    Rgba,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DeleteScope {
    ByImageId(u32),
    ByImageNumber(u32),
    AllImages,
    AllPlacements,
    /// d=7: images and placements matching the given number.
    ByNumberEverywhere(u32),
}

#[derive(Debug, Clone, PartialEq)]
pub enum KittyEvent {
    ImageStored {
        image_id: u32,
        w: u32,
        h: u32,
    },
    /// Placement resolved against the cursor by the VT screen.
    Placed {
        image_id: u32,
        placement: RawPlacement,
    },
    Deleted(DeleteScope),
    Ignored,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RawPlacement {
    /// Cell offsets: absolute when explicitly set (X/Y), else cursor-relative.
    pub x_cells: Option<u32>,
    pub y_cells: Option<u32>,
    pub columns_cells: Option<u32>,
    pub rows_cells: Option<u32>,
}

#[derive(Debug, Clone)]
pub struct Image {
    pub id: u32,
    pub number: Option<u32>,
    pub w: u32,
    pub h: u32,
    pub rgba: Vec<u8>,
}

#[derive(Debug, Error)]
pub enum GraphicsError {
    #[error("base64 payload invalid")]
    Base64,
    #[error("PNG decode failed: {0}")]
    Png(String),
    #[error("raw pixel data truncated (need {need}, got {got})")]
    Truncated { need: usize, got: usize },
    #[error("image exceeds the embedded terminal resource limit")]
    TooLarge,
}

#[derive(Default)]
struct ChunkBuf {
    control: ControlData,
    payload: Vec<u8>,
}

/// Parsed `key=value` control data of one APC.
#[derive(Debug, Clone, Default)]
struct ControlData {
    action: Option<u8>,
    format: Option<u32>,
    transmission: Option<u8>,
    width: Option<u32>,
    height: Option<u32>,
    more: Option<u8>,
    image_id: Option<u32>,
    image_number: Option<u32>,
    placement_id: u32,
    quiet: Option<u8>,
    columns: Option<u32>,
    rows: Option<u32>,
    x: Option<u32>,
    y: Option<u32>,
    delete: Option<u32>,
}

impl ControlData {
    /// Merge a follow-up chunk's control data over this one (chunked transmits).
    fn merge_from(&mut self, other: &ControlData) {
        if other.action.is_some() {
            self.action = other.action;
        }
        if other.format.is_some() {
            self.format = other.format;
        }
        if other.transmission.is_some() {
            self.transmission = other.transmission;
        }
        if other.width.is_some() {
            self.width = other.width;
        }
        if other.height.is_some() {
            self.height = other.height;
        }
        if other.more.is_some() {
            self.more = other.more;
        }
        if other.image_id.is_some() {
            self.image_id = other.image_id;
        }
        if other.image_number.is_some() {
            self.image_number = other.image_number;
        }
        if other.quiet.is_some() {
            self.quiet = other.quiet;
        }
    }

    fn parse(payload: &[u8]) -> ControlData {
        let mut cd = ControlData::default();
        let head = payload.split(|b| *b == b';').next().unwrap_or(&[]);
        for kv in head.split(|b| *b == b',') {
            let mut it = kv.splitn(2, |b| *b == b'=');
            let k = it.next().unwrap_or(&[]);
            let v = it.next().unwrap_or(&[]);
            let val = || {
                std::str::from_utf8(v)
                    .ok()
                    .and_then(|s| s.trim().parse().ok())
            };
            match k {
                b"a" => cd.action = v.first().copied(),
                b"f" => cd.format = val(),
                b"t" => cd.transmission = v.first().copied(),
                b"s" => cd.width = val(),
                b"v" => cd.height = val(),
                b"m" => {
                    cd.more = std::str::from_utf8(v)
                        .ok()
                        .and_then(|s| s.trim().parse::<u32>().ok())
                        .map(|x| x as u8)
                }
                b"i" => cd.image_id = val(),
                b"I" => cd.image_number = val(),
                b"p" => cd.placement_id = val().unwrap_or(1),
                b"q" => {
                    cd.quiet = std::str::from_utf8(v)
                        .ok()
                        .and_then(|s| s.trim().parse::<u32>().ok())
                        .map(|x| x as u8)
                }
                b"C" => cd.columns = val(),
                b"R" => cd.rows = val(),
                b"X" => cd.x = val(),
                b"Y" => cd.y = val(),
                b"d" => cd.delete = val(),
                _ => {}
            }
        }
        cd
    }
}

#[derive(Default)]
pub struct GraphicsStore {
    images: HashMap<u32, Image>,
    next_id: u32,
    pending: HashMap<u32, ChunkBuf>,
    /// Guess used to convert pixel sizes to cells when C/R are omitted.
    pub cell_px: (u32, u32),
}

impl GraphicsStore {
    pub fn new() -> Self {
        Self {
            cell_px: (8, 16),
            ..Default::default()
        }
    }

    pub fn get(&self, id: u32) -> Option<&Image> {
        self.images.get(&id)
    }

    pub fn iter(&self) -> impl Iterator<Item = &Image> {
        self.images.values()
    }

    pub fn len(&self) -> usize {
        self.images.len()
    }

    pub fn is_empty(&self) -> bool {
        self.images.is_empty()
    }

    /// Feed one APC payload (bytes between `ESC _` and `ESC \`).
    pub fn feed(&mut self, payload: &[u8]) -> Result<KittyEvent, GraphicsError> {
        let cd = ControlData::parse(payload);
        let body = payload.split(|b| *b == b';').nth(1).unwrap_or(&[]);

        match cd.action.unwrap_or(b't') {
            b't' | b'T' => self.handle_transmit(&cd, body),
            b'p' => Ok(self.handle_place(&cd)),
            b'd' => Ok(self.handle_delete(&cd)),
            _ => Ok(KittyEvent::Ignored),
        }
    }

    /// Kitty strips base64 padding; accept padded or unpadded payloads.
    fn decode_b64(body: &[u8]) -> Result<Vec<u8>, GraphicsError> {
        use base64::Engine as _;
        if body.len() > MAX_APC_BYTES {
            return Err(GraphicsError::TooLarge);
        }
        let mut s = String::with_capacity(body.len() + 4);
        for &b in body {
            if !b.is_ascii_whitespace() {
                s.push(b as char);
            }
        }
        while s.len() % 4 != 0 {
            s.push('=');
        }
        base64::engine::general_purpose::STANDARD
            .decode(s.as_bytes())
            .map_err(|_| GraphicsError::Base64)
    }

    fn handle_transmit(
        &mut self,
        cd: &ControlData,
        body: &[u8],
    ) -> Result<KittyEvent, GraphicsError> {
        // Only direct transmission supported; file/temp/shared are ignored.
        if cd.transmission.is_some() && cd.transmission != Some(b'd') {
            return Ok(KittyEvent::Ignored);
        }
        let key = cd.image_id.or(cd.image_number).unwrap_or(0);
        if body.len() > MAX_APC_BYTES
            || (!self.pending.contains_key(&key) && self.pending.len() >= MAX_PENDING_TRANSMITS)
        {
            return Ok(KittyEvent::Ignored);
        }
        let decoded = Self::decode_b64(body)?;

        let too_large = {
            let buf = self.pending.entry(key).or_default();
            if buf.payload.len().saturating_add(decoded.len()) > MAX_APC_BYTES {
                true
            } else {
                // Later chunks may restate keys (e.g. f/s/v only on chunk 1): merge.
                buf.control.merge_from(cd);
                buf.payload.extend_from_slice(&decoded);
                false
            }
        };
        if too_large {
            self.pending.remove(&key);
            return Ok(KittyEvent::Ignored);
        }

        // m=1: more chunks coming — wait for the tail.
        if cd.more == Some(1) {
            return Ok(KittyEvent::Ignored);
        }
        let ChunkBuf {
            control: c,
            payload,
        } = self.pending.remove(&key).unwrap();
        let (w, h, rgba) = decode_pixels(&c, &payload)?;
        let id = c.image_id.unwrap_or_else(|| {
            self.next_id += 1;
            self.next_id
        });
        let replacing_bytes = self.images.get(&id).map_or(0, |im| im.rgba.len());
        let replacing_count = if replacing_bytes > 0 { 1 } else { 0 };
        let retained_count = self.images.len().saturating_sub(replacing_count);
        let retained_bytes = self
            .images
            .values()
            .map(|im| im.rgba.len())
            .sum::<usize>()
            .saturating_sub(replacing_bytes);
        if retained_count >= MAX_STORED_IMAGES
            || retained_bytes.saturating_add(rgba.len()) > MAX_STORED_RGBA_BYTES
        {
            return Ok(KittyEvent::Ignored);
        }
        self.next_id = self.next_id.max(id);
        self.images.insert(
            id,
            Image {
                id,
                number: c.image_number,
                w,
                h,
                rgba,
            },
        );
        Ok(KittyEvent::ImageStored { image_id: id, w, h })
    }

    fn handle_place(&mut self, cd: &ControlData) -> KittyEvent {
        let image_id = match (cd.image_id, cd.image_number) {
            (Some(i), _) => i,
            (None, Some(n)) => match self.images.values().find(|im| im.number == Some(n)) {
                Some(im) => im.id,
                None => return KittyEvent::Ignored,
            },
            (None, None) => self.images.keys().copied().max().unwrap_or(0),
        };
        if !self.images.contains_key(&image_id) {
            return KittyEvent::Ignored;
        }
        KittyEvent::Placed {
            image_id,
            placement: RawPlacement {
                x_cells: cd.x,
                y_cells: cd.y,
                columns_cells: cd.columns,
                rows_cells: cd.rows,
            },
        }
    }

    fn handle_delete(&mut self, cd: &ControlData) -> KittyEvent {
        let Some(d) = cd.delete else {
            return KittyEvent::Ignored;
        };
        let scope = match d {
            1 => match cd.image_id {
                Some(i) => DeleteScope::ByImageId(i),
                None => DeleteScope::AllPlacements,
            },
            2 => match cd.image_number {
                Some(n) => DeleteScope::ByImageNumber(n),
                None => return KittyEvent::Ignored,
            },
            3 | 7 => DeleteScope::AllImages,
            4 => DeleteScope::AllPlacements,
            5 => match cd.image_number {
                Some(n) => DeleteScope::ByNumberEverywhere(n),
                None => DeleteScope::AllPlacements,
            },
            6 => DeleteScope::AllPlacements,
            _ => return KittyEvent::Ignored,
        };
        match scope {
            DeleteScope::ByImageId(i) => {
                self.images.remove(&i);
            }
            DeleteScope::ByImageNumber(n) => {
                let ids: Vec<u32> = self
                    .images
                    .iter()
                    .filter(|(_, im)| im.number == Some(n))
                    .map(|(id, _)| *id)
                    .collect();
                for id in ids {
                    self.images.remove(&id);
                }
            }
            DeleteScope::AllImages => self.images.clear(),
            _ => {}
        }
        KittyEvent::Deleted(scope)
    }

    /// Cell footprint for a placement, honouring explicit C/R else pixel math.
    pub fn cells_for(&self, image_id: u32, rp: &RawPlacement) -> (u32, u32) {
        let (cw, ch) = self.cell_px;
        let dims = match self.images.get(&image_id) {
            Some(im) => (im.w, im.h),
            None => (0, 0),
        };
        let cols = rp
            .columns_cells
            .unwrap_or_else(|| (dims.0 + cw - 1) / cw.max(1));
        let rows = rp
            .rows_cells
            .unwrap_or_else(|| (dims.1 + ch - 1) / ch.max(1));
        (cols.max(1), rows.max(1))
    }
}

fn decode_pixels(cd: &ControlData, data: &[u8]) -> Result<(u32, u32, Vec<u8>), GraphicsError> {
    match cd.format.unwrap_or(0) {
        0 | 100 => {
            if data.len() > MAX_APC_BYTES
                || data.len() < 24
                || data.get(..8) != Some(b"\x89PNG\r\n\x1a\n")
            {
                return Err(GraphicsError::TooLarge);
            }
            let w = u32::from_be_bytes([data[16], data[17], data[18], data[19]]);
            let h = u32::from_be_bytes([data[20], data[21], data[22], data[23]]);
            check_dimensions(w, h)?;
            let img = image::load_from_memory(data)
                .map_err(|e| GraphicsError::Png(e.to_string()))?
                .to_rgba8();
            let (w, h) = (img.width(), img.height());
            Ok((w, h, img.into_raw()))
        }
        24 => {
            let (w, h) = (
                cd.width
                    .ok_or(GraphicsError::Truncated { need: 0, got: 0 })?,
                cd.height
                    .ok_or(GraphicsError::Truncated { need: 0, got: 0 })?,
            );
            check_dimensions(w, h)?;
            let need = (w as usize)
                .checked_mul(h as usize)
                .and_then(|n| n.checked_mul(3))
                .ok_or(GraphicsError::TooLarge)?;
            if data.len() < need {
                return Err(GraphicsError::Truncated {
                    need,
                    got: data.len(),
                });
            }
            let mut rgba = Vec::with_capacity(need * 4 / 3);
            for px in data[..need].chunks_exact(3) {
                rgba.extend_from_slice(&[px[0], px[1], px[2], 255]);
            }
            Ok((w, h, rgba))
        }
        32 => {
            let (w, h) = (
                cd.width
                    .ok_or(GraphicsError::Truncated { need: 0, got: 0 })?,
                cd.height
                    .ok_or(GraphicsError::Truncated { need: 0, got: 0 })?,
            );
            check_dimensions(w, h)?;
            let need = (w as usize)
                .checked_mul(h as usize)
                .and_then(|n| n.checked_mul(4))
                .ok_or(GraphicsError::TooLarge)?;
            if data.len() < need {
                return Err(GraphicsError::Truncated {
                    need,
                    got: data.len(),
                });
            }
            Ok((w, h, data[..need].to_vec()))
        }
        f => Err(GraphicsError::Png(format!(
            "unsupported pixel format f={f}"
        ))),
    }
}

fn check_dimensions(w: u32, h: u32) -> Result<(), GraphicsError> {
    if w == 0
        || h == 0
        || w > MAX_IMAGE_EDGE
        || h > MAX_IMAGE_EDGE
        || (w as u64) * (h as u64) > MAX_IMAGE_PIXELS
    {
        return Err(GraphicsError::TooLarge);
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use base64::Engine as _;

    fn apc(control: &str, payload: &[u8]) -> Vec<u8> {
        // feed() expects the payload AFTER the leading 'G'.
        let mut v = control.as_bytes().to_vec();
        if !payload.is_empty() {
            v.push(b';');
            v.extend_from_slice(payload);
        }
        v
    }

    fn b64(data: &[u8]) -> Vec<u8> {
        base64::engine::general_purpose::STANDARD
            .encode(data)
            .into_bytes()
    }

    #[test]
    fn rejects_oversized_raw_dimensions_before_allocation() {
        let control = ControlData {
            format: Some(32),
            width: Some(8192),
            height: Some(8192),
            ..ControlData::default()
        };
        assert!(matches!(
            decode_pixels(&control, &[]),
            Err(GraphicsError::TooLarge)
        ));
    }

    #[test]
    fn transmit_and_place_raw_rgba() {
        let mut s = GraphicsStore::new();
        // 2x1 red/green pixel, f=32 (payload must be base64).
        let ev = s
            .feed(&apc(
                "f=32,s=2,v=1,i=10",
                &b64(&[255, 0, 0, 255, 0, 255, 0, 255]),
            ))
            .unwrap();
        assert_eq!(
            ev,
            KittyEvent::ImageStored {
                image_id: 10,
                w: 2,
                h: 1
            }
        );

        // Place at cursor with explicit C=2,R=1.
        let ev = s.feed(&apc("a=p,i=10,p=1,C=2,R=1", &[])).unwrap();
        match ev {
            KittyEvent::Placed {
                image_id,
                placement,
            } => {
                assert_eq!(image_id, 10);
                assert_eq!(placement.columns_cells, Some(2));
            }
            other => panic!("{other:?}"),
        }
        assert_eq!(
            s.cells_for(
                10,
                &RawPlacement {
                    x_cells: None,
                    y_cells: None,
                    columns_cells: None,
                    rows_cells: None
                }
            )
            .0,
            1
        );
    }

    #[test]
    fn chunked_transmit() {
        let mut s = GraphicsStore::new();
        let data = vec![7u8; 4 * 4 * 4]; // 4x4 RGBA
        let b = b64(&data);
        let (a, rest) = b.split_at(b.len() / 2);
        let e1 = s.feed(&apc("f=32,s=4,v=4,i=5,m=1", a)).unwrap();
        assert_eq!(e1, KittyEvent::Ignored);
        let e2 = s.feed(&apc("m=0,i=5", rest)).unwrap();
        assert!(matches!(
            e2,
            KittyEvent::ImageStored {
                image_id: 5,
                w: 4,
                h: 4
            }
        ));
    }

    #[test]
    fn png_transmit_and_delete() {
        let mut s = GraphicsStore::new();
        let png = image::DynamicImage::new_rgb8(3, 2);
        let mut buf = std::io::Cursor::new(Vec::new());
        png.write_to(&mut buf, image::ImageFormat::Png).unwrap();
        let b = b64(buf.get_ref());
        let ev = s.feed(&apc("i=3,q=1", &b)).unwrap();
        assert_eq!(
            ev,
            KittyEvent::ImageStored {
                image_id: 3,
                w: 3,
                h: 2
            }
        );

        let ev = s.feed(&apc("a=d,d=1,i=3", &[])).unwrap();
        assert_eq!(ev, KittyEvent::Deleted(DeleteScope::ByImageId(3)));
        assert!(s.is_empty());
    }

    #[test]
    fn place_by_number() {
        let mut s = GraphicsStore::new();
        s.feed(&apc("f=32,s=1,v=1,I=42", &b64(&[1, 2, 3, 4])))
            .unwrap();
        let ev = s.feed(&apc("a=p,I=42", &[])).unwrap();
        assert!(matches!(ev, KittyEvent::Placed { image_id: 1, .. }));
    }
}
