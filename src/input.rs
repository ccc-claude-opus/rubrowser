//! Input events and the host interface that platforms implement.

use crate::abi::modifier;

/// Modifier keys held during an event.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Modifiers {
    pub shift: bool,
    pub ctrl: bool,
    pub alt: bool,
    pub meta: bool,
}

impl Modifiers {
    /// Ctrl or Cmd: the "command" modifier for shortcuts on every platform.
    pub fn command(&self) -> bool {
        self.ctrl || self.meta
    }

    /// The modifiers as `abi::modifier` bit flags.
    pub fn bits(&self) -> i32 {
        let mut b = 0;
        if self.shift {
            b |= modifier::SHIFT;
        }
        if self.ctrl {
            b |= modifier::CTRL;
        }
        if self.alt {
            b |= modifier::ALT;
        }
        if self.meta {
            b |= modifier::META;
        }
        b
    }

    pub fn from_bits(b: i32) -> Self {
        Modifiers {
            shift: b & modifier::SHIFT != 0,
            ctrl: b & modifier::CTRL != 0,
            alt: b & modifier::ALT != 0,
            meta: b & modifier::META != 0,
        }
    }
}

/// An input event from the platform. Positions are logical pixels relative
/// to the window's top-left corner; key codes are `abi::key` values.
#[derive(Clone, Debug, PartialEq)]
pub enum Event {
    MouseMove {
        x: i32,
        y: i32,
    },
    MouseDown {
        x: i32,
        y: i32,
        button: i32,
    },
    MouseUp {
        x: i32,
        y: i32,
        button: i32,
    },
    /// The pointer left the window.
    MouseLeave,
    /// Positive `dy` scrolls down.
    Wheel {
        dx: i32,
        dy: i32,
    },
    KeyDown {
        key: i32,
        mods: Modifiers,
    },
    KeyUp {
        key: i32,
        mods: Modifiers,
    },
    /// Typed text (already composed, without control characters).
    Text(String),
}

/// Mouse cursor shapes the browser asks for.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Cursor {
    #[default]
    Default,
    Pointer,
    Text,
}

/// What the browser needs from the platform it runs on.
/// Method and body of an HTTP request made by a page script.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct HttpRequest {
    pub method: String,
    pub body: Vec<u8>,
    pub content_type: String,
}

pub trait Host {
    /// Starts loading `url`; the platform later calls
    /// [`Browser::fetch_done`](crate::browser::Browser::fetch_done) with `id`.
    fn fetch(&mut self, id: u32, url: &str);
    /// Like [`Host::fetch`], with a method and body (requests from page
    /// scripts). Platforms that only support GET may ignore them.
    fn fetch_with(&mut self, id: u32, url: &str, _request: &HttpRequest) {
        self.fetch(id, url);
    }
    fn clipboard_get(&mut self) -> Option<String>;
    fn clipboard_set(&mut self, text: &str);
    fn set_cursor(&mut self, cursor: Cursor);
    fn set_title(&mut self, title: &str);
    /// A console message from a page script or the browser.
    fn log(&mut self, _message: &str) {}
    /// Wall-clock time in milliseconds since the Unix epoch, if known.
    fn wall_clock(&mut self) -> Option<f64> {
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn modifier_bits_round_trip() {
        let m = Modifiers {
            shift: true,
            meta: true,
            ..Default::default()
        };
        assert_eq!(Modifiers::from_bits(m.bits()), m);
        assert!(m.command());
    }
}
