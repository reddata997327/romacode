//! Detached phone display — a scrcpy-style attach, built natively.
//!
//! When the user clicks the android icon (or presses alt+a), RomaCode no
//! longer squeezes the phone into the terminal: it opens a **real desktop
//! window** owned by this same process — the live compositor framebuffer is
//! blitted straight into a native OS window (minifb: X11/XWayland, macOS
//! AppKit, Windows GDI — compiled in, no browser, no HTTP server, no
//! external binary, no MCP) exactly like attaching scrcpy to a device.
//!
//! scrcpy input parity, wired straight into the same device command queue
//! the TUI mouse path uses:
//!   left mouse       tap (drag beyond slop → swipe)
//!   right mouse      BACK
//!   middle mouse     HOME
//!   mouse wheel      vertical scroll (synthetic swipe)
//!   keyboard         typing + enter/backspace; esc = BACK; home key = HOME
//!
//! The terminal stays fully free for the agent; the live stream only runs
//! while a window is actually watching (`Display::demand`), so a closed
//! window costs nothing. Without a desktop display server (SSH console, CI)
//! the window simply cannot open and the inline terminal panel takes over.

use crate::device::{Frame, FrameHub, PhoneKey};
use crate::net::Net;
use romacode_shared::protocol::{AndroidAction, ClientRequest};
use std::sync::{Arc, Mutex};

/// Letterbox color for the aspect-fit desktop window background.
const LETTERBOX: u32 = 0x101014;
/// Finger slop (px): a press that moves less than this is a tap.
const SWIPE_SLOP: i64 = 10;

/// Collects the characters minifb receives while pumping (typed text) so the
/// next tick can send them to the device — the same Cmd::Text path the
/// fullscreen TUI uses.
struct CharSink(Arc<Mutex<Vec<char>>>);

impl minifb::InputCallback for CharSink {
    fn add_char(&mut self, c: u32) {
        let Some(ch) = char::from_u32(c) else { return };
        if let Ok(mut v) = self.0.lock() {
            if v.len() < 256 {
                v.push(ch);
            }
        }
    }
}

pub struct PhoneWindow {
    win: minifb::Window,
    /// Latest frames streamed from the brain.
    hub: FrameHub,
    /// Device input goes over the wire (taps, swipes, keys, text).
    net: Arc<Net>,
    /// Characters typed into the window since the last pump.
    chars: Arc<Mutex<Vec<char>>>,
    buf: Vec<u32>,
    buf_dims: (usize, usize),
    /// Content rect inside the window buffer: (x, y, w, h) — the aspect-fit
    /// device screen; the rest is letterbox.
    content: (usize, usize, usize, usize),
    /// Device framebuffer size the content rect maps into.
    device: (u32, u32),
    last_gen: u64,
    /// Left-press position in device coords (tap vs swipe decided on release).
    press: Option<(u32, u32)>,
    had_right: bool,
    had_middle: bool,
    last_buttons: (bool, bool, bool),
    frames_blitted: u64,
}

impl PhoneWindow {
    /// Create the desktop window. Fails when there is no display server
    /// (headless console / CI) — the caller falls back to the inline panel.
    pub fn open(hub: FrameHub, net: Arc<Net>, title: String) -> Result<Self, String> {
        // The brain streams device frames; geometry comes from the first one
        // (or the legacy default before it lands).
        let (dw, dh) = hub
            .take_frame(0)
            .map(|f| (f.w.max(1), f.h.max(1)))
            .unwrap_or((720, 1560));
        // half resolution: aspect-exact for the panel (2:1 integer downsample
        // keeps every device pixel crisp) and fits an 800 px-high desktop
        let (ww, wh) = (
            (dw / 2).clamp(160, 720) as usize,
            (dh / 2).clamp(320, 1560) as usize,
        );
        let win = minifb::Window::new(
            &title,
            ww,
            wh,
            minifb::WindowOptions {
                resize: true,
                scale: minifb::Scale::X1,
                ..Default::default()
            },
        )
        .map_err(|e| format!("no desktop display server ({e})"))?;
        let chars = Arc::new(Mutex::new(Vec::new()));
        let mut win = win;
        win.set_input_callback(Box::new(CharSink(chars.clone())));
        Ok(Self {
            win,
            hub,
            net,
            chars,
            buf: vec![LETTERBOX; ww * wh],
            buf_dims: (ww, wh),
            content: (0, 0, ww, wh),
            device: (dw, dh),
            last_gen: 0,
            press: None,
            had_right: false,
            had_middle: false,
            last_buttons: (false, false, false),
            frames_blitted: 0,
        })
    }

    #[allow(dead_code)] // window state introspection for debugging
    pub fn is_open(&self) -> bool {
        self.win.is_open()
    }

    /// How many compositor frames reached the desktop window.
    #[allow(dead_code)] // exposed for e2e window scripts
    pub fn frames_blitted(&self) -> u64 {
        self.frames_blitted
    }

    /// Pump once per TUI tick: input → device commands, latest compositor
    /// frame → desktop window. Returns false when the window was closed by
    /// the user (caller drops it and falls back to the inline panel).
    pub fn pump(&mut self) -> bool {
        if !self.win.is_open() {
            return false;
        }
        // stream demand is owned by App.poll (window open → Stream on)
        self.pump_chars();
        self.pump_keys();
        self.pump_mouse();
        self.pump_frame();
        self.win.is_open()
    }

    /// Typed text → device (the same Cmd::Text the fullscreen TUI sends).
    fn pump_chars(&mut self) {
        let drained: Vec<char> = self
            .chars
            .lock()
            .map(|mut v| std::mem::take(&mut *v))
            .unwrap_or_default();
        let mut text = String::new();
        for c in drained {
            match c {
                '\n' | '\r' => {
                    self.flush_text(&mut text);
                    self.send_key(PhoneKey::Enter);
                }
                '\u{8}' | '\u{7f}' => {
                    self.flush_text(&mut text);
                    self.send_key(PhoneKey::Del);
                }
                c if (c as u32) < 32 => {} // control chars the device can't type
                c => {
                    if text.len() < 128 {
                        text.push(c);
                    }
                }
            }
        }
        self.flush_text(&mut text);
    }

    fn flush_text(&mut self, text: &mut String) {
        if !text.is_empty() {
            let t = std::mem::take(text);
            self.send_text(&t);
        }
    }

    /// scrcpy key parity: esc = BACK, home key = HOME. (enter/backspace are
    /// handled as chars so no platform double-fires.)
    fn pump_keys(&mut self) {
        if self
            .win
            .is_key_pressed(minifb::Key::Escape, minifb::KeyRepeat::No)
        {
            self.send_key(PhoneKey::Back);
        }
        if self
            .win
            .is_key_pressed(minifb::Key::Home, minifb::KeyRepeat::No)
        {
            self.send_key(PhoneKey::Home);
        }
    }

    fn mouse_pos(&self) -> Option<(u32, u32)> {
        let (x, y) = self.win.get_mouse_pos(minifb::MouseMode::Clamp)?;
        self.to_device(x, y)
    }

    /// Window-buffer coords → device coords through the aspect-fit rect.
    fn to_device(&self, wx: f32, wy: f32) -> Option<(u32, u32)> {
        let (ox, oy, cw, ch) = self.content;
        let (fx, fy) = self.device;
        if cw == 0 || ch == 0 {
            return None;
        }
        let x = ((wx - ox as f32) * fx as f32 / cw as f32).round();
        let y = ((wy - oy as f32) * fy as f32 / ch as f32).round();
        if x < 0.0 || y < 0.0 || x >= fx as f32 || y >= fy as f32 {
            None
        } else {
            Some((x as u32, y as u32))
        }
    }

    /// Left button = tap/swipe (decided on release, like the TUI panel path);
    /// right = BACK; middle = HOME; wheel = vertical scroll — scrcpy parity.
    fn pump_mouse(&mut self) {
        let right = self.win.get_mouse_down(minifb::MouseButton::Right);
        let middle = self.win.get_mouse_down(minifb::MouseButton::Middle);
        let left = self.win.get_mouse_down(minifb::MouseButton::Left);
        if std::env::var("ROMACODE_WINDOW_DEBUG").is_ok() {
            let cur = (left, right, middle);
            if cur != self.last_buttons {
                eprintln!("[window] buttons left={left} right={right} middle={middle}");
                self.last_buttons = cur;
            }
        }
        if right && !self.had_right {
            self.send_key(PhoneKey::Back);
        }
        self.had_right = right;
        if middle && !self.had_middle {
            self.send_key(PhoneKey::Home);
        }
        self.had_middle = middle;

        if left && self.press.is_none() {
            self.press = self.mouse_pos();
            if std::env::var("ROMACODE_WINDOW_DEBUG").is_ok() {
                let at = self.press;
                eprintln!("[window] press down at {at:?}");
            }
        } else if !left {
            if let Some(p0) = self.press.take() {
                let p1 = self.mouse_pos().unwrap_or(p0);
                let out = press_outcome(p0, p1);
                if std::env::var("ROMACODE_WINDOW_DEBUG").is_ok() {
                    eprintln!("[window] press up {p0:?} -> {p1:?} (tap/swipe decided)");
                }
                self.send_action(out);
            }
        }

        if let Some((_, dy)) = self.win.get_scroll_wheel() {
            if dy != 0.0 {
                if std::env::var("ROMACODE_WINDOW_DEBUG").is_ok() {
                    eprintln!("[window] wheel dy={dy}");
                }
                // Frame-derived swipe so the gesture covers the middle 50%
                // of the actual device display rather than a hardcoded
                // 720×1560 region. Fall back to a sane default before the
                // first frame arrives so the wheel still works at boot.
                let (fw, fh) = self.device;
                self.send_action(wheel_cmd(dy > 0.0, fw, fh));
            }
        }
    }

    // ── device input over the wire ──

    fn send_action(&self, action: AndroidAction) {
        self.net.fire(ClientRequest::Android { action });
    }

    fn send_key(&self, key: PhoneKey) {
        self.send_action(AndroidAction::Key {
            key: key.as_str().to_string(),
        });
    }

    fn send_text(&self, text: &str) {
        self.send_action(AndroidAction::Text {
            text: text.to_string(),
        });
    }

    /// Blit the newest compositor frame; letterbox while the phone boots.
    fn pump_frame(&mut self) {
        match self.hub.take_frame(self.last_gen) {
            Some(f) => {
                self.last_gen = f.gen;
                if (f.w, f.h) != self.device {
                    self.device = (f.w, f.h);
                }
                let (w, h) = self.win.get_size();
                if (w, h) != self.buf_dims {
                    self.buf_dims = (w, h);
                    self.buf = vec![LETTERBOX; w.max(1) * h.max(1)];
                }
                self.content = blit(&f, &mut self.buf, w, h);
                let _ = self.win.update_with_buffer(&self.buf, w, h);
                self.frames_blitted += 1;
            }
            None => {
                // still pump the OS event loop so the window stays responsive
                self.win.update();
            }
        }
    }
}

/// The same tap/swipe decision the fullscreen TUI and the old browser page
/// made: a move beyond the slop is a swipe, anything shorter is a tap.
fn press_outcome(down: (u32, u32), up: (u32, u32)) -> AndroidAction {
    let dx = (down.0 as i64 - up.0 as i64).abs();
    let dy = (down.1 as i64 - up.1 as i64).abs();
    if dx + dy > SWIPE_SLOP {
        AndroidAction::Swipe {
            x1: down.0,
            y1: down.1,
            x2: up.0,
            y2: up.1,
        }
    } else {
        AndroidAction::Tap { x: up.0, y: up.1 }
    }
}

/// Mouse wheel → a vertical scroll swipe sized to the device frame. The
/// gesture covers the middle 50% of the display (`mid_x`, `h/4` ↔ `3h/4`) so
/// a 1080×2400 device scrolls across the whole height rather than the upper
/// left quadrant. Falls back to a 720×1560 default (the legacy emulator
/// geometry) when the bridge has not yet delivered a frame.
fn wheel_cmd(up: bool, w: u32, h: u32) -> AndroidAction {
    const DEFAULT_W: u32 = 720;
    const DEFAULT_H: u32 = 1560;
    let w = if w == 0 { DEFAULT_W } else { w };
    let h = if h == 0 { DEFAULT_H } else { h };
    let mid_x = w / 2;
    let bottom_y = (h * 3) / 4;
    let top_y = h / 4;
    if up {
        AndroidAction::Swipe {
            x1: mid_x,
            y1: bottom_y,
            x2: mid_x,
            y2: top_y,
        }
    } else {
        AndroidAction::Swipe {
            x1: mid_x,
            y1: top_y,
            x2: mid_x,
            y2: bottom_y,
        }
    }
}

/// Aspect-correct scrcpy-style blit: nearest-neighbour sample of the device
/// frame into the window buffer, centered and letterboxed — never cropped,
/// never stretched. Returns the content rect (x, y, w, h) for input mapping.
fn blit(src: &Frame, dst: &mut [u32], dw: usize, dh: usize) -> (usize, usize, usize, usize) {
    if dw == 0
        || dh == 0
        || src.w == 0
        || src.h == 0
        || src.rgba.len() < (src.w * src.h * 4) as usize
    {
        return (0, 0, 0, 0);
    }
    for p in dst.iter_mut() {
        *p = LETTERBOX;
    }
    let scale = (dw as f64 / src.w as f64).min(dh as f64 / src.h as f64);
    let cw = ((src.w as f64 * scale) as usize).clamp(1, dw);
    let ch = ((src.h as f64 * scale) as usize).clamp(1, dh);
    let ox = (dw - cw) / 2;
    let oy = (dh - ch) / 2;
    for y in 0..ch {
        let sy = y * src.h as usize / ch;
        let row = &src.rgba[sy * src.w as usize * 4..][..src.w as usize * 4];
        let dst_row = &mut dst[(y + oy) * dw + ox..(y + oy) * dw + ox + cw];
        for (x, p) in dst_row.iter_mut().enumerate() {
            let i = x * src.w as usize / cw * 4;
            *p = (row[i] as u32) << 16 | (row[i + 1] as u32) << 8 | row[i + 2] as u32;
        }
    }
    (ox, oy, cw, ch)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;

    fn frame(w: u32, h: u32) -> Frame {
        Frame {
            gen: 1,
            w,
            h,
            rgba: Arc::new(vec![80u8; (w * h * 4) as usize]),
        }
    }

    #[test]
    fn blit_is_aspect_correct_and_letterboxed() {
        // 720×1560 into 360×780: perfect 2:1 fit — content fills the window
        let mut dst = vec![0u32; 360 * 780];
        let rect = blit(&frame(720, 1560), &mut dst, 360, 780);
        assert_eq!(
            rect,
            (0, 0, 360, 780),
            "matching aspect must fill the window"
        );
        assert_eq!(
            dst[0],
            (80 << 16) | (80 << 8) | 80,
            "corner must sample the frame"
        );
        assert_eq!(
            dst[360 * 780 - 1],
            (80 << 16) | (80 << 8) | 80,
            "bottom-right corner"
        );

        // a wider window letterboxes top/bottom, never crops or stretches
        let mut wide = vec![0u32; 500 * 780];
        let rect = blit(&frame(720, 1560), &mut wide, 500, 780);
        assert_eq!(rect.2, 360, "content width follows the device aspect");
        assert_eq!(rect.3, 780);
        assert_eq!(
            (rect.0, rect.1),
            ((500 - 360) / 2, 0),
            "centered horizontally"
        );
        // the letterbox columns keep the backdrop colour
        assert_eq!(wide[0], LETTERBOX, "left gutter is letterbox");
        assert_eq!(wide[rect.0 - 1 + 400 * 500], LETTERBOX, "gutter mid-screen");
        assert_eq!(
            wide[rect.0 + 400 * 500],
            (80 << 16) | (80 << 8) | 80,
            "content starts at rect"
        );

        // a taller window letterboxes left/right
        let mut tall = vec![0u32; 360 * 900];
        let rect = blit(&frame(720, 1560), &mut tall, 360, 900);
        assert_eq!(rect, (0, (900 - 780) / 2, 360, 780), "centered vertically");
        assert_eq!(tall[10], LETTERBOX, "top gutter is letterbox");

        // degenerate sizes never panic (2×2 window with a tall frame → 1×2 content)
        let mut tiny = vec![0u32; 4];
        let rect = blit(&frame(720, 1560), &mut tiny, 2, 2);
        assert_eq!(rect, (0, 0, 1, 2));
        let mut empty = vec![];
        assert_eq!(blit(&frame(0, 0), &mut empty, 0, 0), (0, 0, 0, 0));
    }

    #[test]
    fn press_decides_tap_vs_swipe_like_every_other_path() {
        assert!(matches!(
            press_outcome((360, 1200), (362, 1198)),
            AndroidAction::Tap { x: 362, y: 1198 }
        ));
        assert!(matches!(
            press_outcome((360, 1100), (360, 400)),
            AndroidAction::Swipe {
                x1: 360,
                y1: 1100,
                x2: 360,
                y2: 400,
            }
        ));
        // slop is exclusive: exactly 11 px of travel is a swipe
        assert!(matches!(
            press_outcome((0, 0), (11, 0)),
            AndroidAction::Swipe { .. }
        ));
        assert!(matches!(
            press_outcome((0, 0), (10, 0)),
            AndroidAction::Tap { .. }
        ));
    }

    #[test]
    fn wheel_maps_to_scroll_swipes() {
        // wheel up scrolls content down → swipe from below to above.
        // Coordinates are frame-derived (middle 50% of a 720×1560 default).
        assert!(matches!(
            wheel_cmd(true, 720, 1560),
            AndroidAction::Swipe {
                x1: 360,
                y1: 1170,
                x2: 360,
                y2: 390
            }
        ));
        assert!(matches!(
            wheel_cmd(false, 720, 1560),
            AndroidAction::Swipe {
                x1: 360,
                y1: 390,
                x2: 360,
                y2: 1170
            }
        ));
        // 1080×2400 device scrolls across the whole display, not the upper
        // left quadrant the old hardcoded (360, 1100) coords hit.
        assert!(matches!(
            wheel_cmd(true, 1080, 2400),
            AndroidAction::Swipe {
                x1: 540,
                y1: 1800,
                x2: 540,
                y2: 600
            }
        ));
    }
}
