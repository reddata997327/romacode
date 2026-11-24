//! Client-side device view: the latest frame from the server and the key
//! names the phone panel / desktop window send back. All pixels live in the
//! brain; the screen only mirrors and forwards input.

use std::sync::{Arc, Mutex};

/// One committed device frame (RGBA), PNG-decoded from a wire `Frame` event.
/// The pixel buffer is shared by reference count — cloning a frame never
/// copies megabytes of pixels.
#[derive(Clone)]
pub struct Frame {
    pub gen: u64,
    pub w: u32,
    pub h: u32,
    pub rgba: Arc<Vec<u8>>,
}

/// Latest-frame hub shared between the net thread (writer) and the render
/// paths (phone panel, pixel renderer, desktop window).
#[derive(Clone, Default)]
pub struct FrameHub {
    frame: Arc<Mutex<Option<Frame>>>,
}

impl FrameHub {
    pub fn new() -> Self {
        Self::default()
    }

    /// Publish the newest frame (from a wire `Frame` event).
    pub fn set(&self, f: Frame) {
        *self.frame.lock().unwrap_or_else(|e| e.into_inner()) = Some(f);
    }

    /// Latest frame if newer than `seen_gen`.
    pub fn take_frame(&self, seen_gen: u64) -> Option<Frame> {
        let f = self
            .frame
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .clone()?;
        if f.gen != seen_gen {
            Some(f)
        } else {
            None
        }
    }
}

/// Phone hardware keys the screen can send. Mapped server-side to real
/// Android keyevents (power/volume are wired for the desktop window's menus).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[allow(dead_code)]
pub enum PhoneKey {
    Home,
    Back,
    Enter,
    Del,
    Power,
    VolumeUp,
    VolumeDown,
}

impl PhoneKey {
    /// Wire name for `AndroidAction::Key { key }`.
    pub fn as_str(self) -> &'static str {
        match self {
            PhoneKey::Home => "home",
            PhoneKey::Back => "back",
            PhoneKey::Enter => "enter",
            PhoneKey::Del => "del",
            PhoneKey::Power => "power",
            PhoneKey::VolumeUp => "volume_up",
            PhoneKey::VolumeDown => "volume_down",
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hub_dedups_by_gen() {
        let hub = FrameHub::new();
        assert!(hub.take_frame(0).is_none());
        hub.set(Frame {
            gen: 1,
            w: 2,
            h: 3,
            rgba: Arc::new(vec![0; 24]),
        });
        assert_eq!(hub.take_frame(0).map(|f| f.gen), Some(1));
        assert!(hub.take_frame(1).is_none());
    }

    #[test]
    fn key_names_are_wire_names() {
        assert_eq!(PhoneKey::Home.as_str(), "home");
        assert_eq!(PhoneKey::VolumeDown.as_str(), "volume_down");
    }
}
