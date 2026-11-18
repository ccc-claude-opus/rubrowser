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
        /// Sent again while the key is held (`KeyboardEvent.repeat`).
        repeat: bool,
    },
    KeyUp {
        key: i32,
        mods: Modifiers,
    },
    /// Typed text (already composed, without control characters).
    Text(String),
    /// A finger on a touch screen; `id` stays the same from start to end.
    Touch {
        id: u64,
        x: i32,
        y: i32,
        phase: TouchPhase,
    },
}

/// The stage of a touch.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TouchPhase {
    Start,
    Move,
    End,
    /// The system took the touch away (no tap, no click).
    Cancel,
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
/// Method, body and referrer of an HTTP request (method empty: a plain GET
/// for a page or a resource it uses).
#[derive(Clone, Debug, Default, PartialEq)]
pub struct HttpRequest {
    pub method: String,
    pub body: Vec<u8>,
    pub content_type: String,
    /// The `Referer` to send (empty: none), from the document's URL and the
    /// default referrer policy.
    pub referrer: String,
}

/// What happened on a WebSocket connection (RFC 6455), reported by the
/// platform with [`Browser::ws_event`](crate::browser::Browser::ws_event).
#[derive(Clone, Debug, PartialEq)]
pub enum WsEvent {
    /// The opening handshake succeeded, with the subprotocol the server chose.
    Open(String),
    Text(String),
    Binary(Vec<u8>),
    /// The connection is closed (`clean`: after a closing handshake).
    Close { code: u16, reason: String, clean: bool },
}

pub trait Host {
    /// Opens WebSocket connection `id` to `url` (`ws:` / `wss:`) for a page
    /// of `origin`, offering `protocols`; events come back through
    /// [`Browser::ws_event`](crate::browser::Browser::ws_event). Platforms
    /// without sockets report the connection closed.
    fn ws_open(&mut self, _id: u32, _url: &str, _protocols: &[String], _origin: &str) {}
    /// Sends a message on connection `id` (`text`: a text frame).
    fn ws_send(&mut self, _id: u32, _data: &[u8], _text: bool) {}
    /// Starts the closing handshake of connection `id`.
    fn ws_close(&mut self, _id: u32, _code: u16, _reason: &str) {}

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
    /// Shows the platform's dialog for choosing files to upload
    /// (`<input type=file>`). `accept` is the input's `accept` attribute
    /// (a hint). Returns the chosen files (empty when the user cancelled),
    /// or `None` when there is no dialog to show (no window).
    fn pick_files(
        &mut self,
        _multiple: bool,
        _accept: &str,
    ) -> Option<Vec<crate::document::FileData>> {
        None
    }
    /// Saves a download named `name` (already a safe file name) to the
    /// user's download folder; returns the path it was written to.
    fn save_download(&mut self, _name: &str, _bytes: &[u8]) -> Result<String, String> {
        Err("this platform cannot save downloads".into())
    }
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
