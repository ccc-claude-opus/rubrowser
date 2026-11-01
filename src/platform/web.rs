//! Canvas version: plain `extern "C"` exports driven by `web/glue.js`.
//!
//! No wasm-bindgen. JS forwards input with the `rb_web_*` calls, asks for a
//! frame with `rb_web_frame` (which returns a pointer to RGBA pixels, or 0
//! when nothing changed), and services requests the browser queues: fetches,
//! cursor, title and clipboard text, exchanged through a shared byte buffer.

use std::cell::RefCell;
use std::collections::VecDeque;

use crate::browser::Browser;
use crate::input::{Cursor, Event, Host, Modifiers};
use crate::net::url;

#[derive(Default)]
struct WebHost {
    fetches: VecDeque<(u32, String)>,
    cursor: Cursor,
    title: String,
    /// Text the browser copied, handed to the next `copy` event.
    clipboard: String,
}

impl Host for WebHost {
    fn fetch(&mut self, id: u32, url: &str) {
        self.fetches.push_back((id, url.to_string()));
    }
    // Pastes arrive as text events from the page's `paste` handler.
    fn clipboard_get(&mut self) -> Option<String> {
        None
    }
    fn clipboard_set(&mut self, text: &str) {
        self.clipboard = text.to_string();
    }
    fn set_cursor(&mut self, cursor: Cursor) {
        self.cursor = cursor;
    }
    fn set_title(&mut self, title: &str) {
        self.title = title.to_string();
    }
}

struct Web {
    browser: Browser,
    host: WebHost,
    rgba: Vec<u8>,
    /// Bytes exchanged with JS (strings out, fetch bodies and text in).
    buf: Vec<u8>,
    now: f64,
}

thread_local! {
    static WEB: RefCell<Option<Web>> = const { RefCell::new(None) };
}

fn with<R: Default>(f: impl FnOnce(&mut Web) -> R) -> R {
    WEB.with(|w| w.borrow_mut().as_mut().map(f).unwrap_or_default())
}

fn send(web: &mut Web, e: Event) {
    web.browser.event(e, web.now, &mut web.host);
}

#[unsafe(no_mangle)]
pub extern "C" fn rb_web_init(width: u32, height: u32, scale: f32) {
    let web = Web {
        browser: Browser::new(width as usize, height as usize, scale),
        host: WebHost::default(),
        rgba: Vec::new(),
        buf: Vec::new(),
        now: 0.0,
    };
    WEB.with(|w| *w.borrow_mut() = Some(web));
}

#[unsafe(no_mangle)]
pub extern "C" fn rb_web_resize(width: u32, height: u32, scale: f32) {
    with(|w| w.browser.resize(width as usize, height as usize, scale));
}

/// Renders if needed; returns a pointer to RGBA pixels, or 0 if unchanged.
#[unsafe(no_mangle)]
pub extern "C" fn rb_web_frame(now: f64) -> usize {
    with(|w| {
        w.now = now;
        if !w.browser.needs_redraw(now) {
            return 0;
        }
        let pixels = w.browser.render(now, &mut w.host).pixels();
        w.rgba.clear();
        w.rgba.reserve(pixels.len() * 4);
        for &p in pixels {
            w.rgba
                .extend_from_slice(&[(p >> 16) as u8, (p >> 8) as u8, p as u8, 0xff]);
        }
        w.rgba.as_ptr() as usize
    })
}

/// `kind`: 0 move, 1 down, 2 up, 3 leave. Coordinates are CSS pixels.
#[unsafe(no_mangle)]
pub extern "C" fn rb_web_mouse(kind: i32, x: i32, y: i32, button: i32) {
    with(|w| {
        let e = match kind {
            0 => Event::MouseMove { x, y },
            1 => Event::MouseDown { x, y, button },
            2 => Event::MouseUp { x, y, button },
            _ => Event::MouseLeave,
        };
        send(w, e);
    });
}

#[unsafe(no_mangle)]
pub extern "C" fn rb_web_wheel(dx: i32, dy: i32) {
    with(|w| send(w, Event::Wheel { dx, dy }));
}

#[unsafe(no_mangle)]
pub extern "C" fn rb_web_key(down: i32, key: i32, mods: i32) {
    with(|w| {
        let mods = Modifiers::from_bits(mods);
        let e = if down != 0 {
            Event::KeyDown { key, mods }
        } else {
            Event::KeyUp { key, mods }
        };
        send(w, e);
    });
}

/// Makes the shared buffer `len` bytes long and returns its address, for
/// JS to write text or a fetch body into.
#[unsafe(no_mangle)]
pub extern "C" fn rb_web_buffer(len: usize) -> usize {
    with(|w| {
        w.buf.clear();
        w.buf.resize(len, 0);
        w.buf.as_ptr() as usize
    })
}

/// Address of the shared buffer (after a call that filled it).
#[unsafe(no_mangle)]
pub extern "C" fn rb_web_buffer_ptr() -> usize {
    with(|w| w.buf.as_ptr() as usize)
}

/// Typed or pasted text, previously written to the shared buffer.
#[unsafe(no_mangle)]
pub extern "C" fn rb_web_text(len: usize) {
    with(|w| {
        let text = String::from_utf8_lossy(&w.buf[..len.min(w.buf.len())]).into_owned();
        send(w, Event::Text(text));
    });
}

/// Opens a URL typed or linked from outside (e.g. the page's `#hash`).
#[unsafe(no_mangle)]
pub extern "C" fn rb_web_open(len: usize) {
    with(|w| {
        let text = String::from_utf8_lossy(&w.buf[..len.min(w.buf.len())]).into_owned();
        w.browser.navigate(&url::from_input(&text), &mut w.host);
    });
}

/// Next queued fetch: returns its id (0 if none) and puts the URL in the
/// shared buffer; its length is returned by `rb_web_buffer_len`.
#[unsafe(no_mangle)]
pub extern "C" fn rb_web_next_fetch() -> u32 {
    with(|w| match w.host.fetches.pop_front() {
        Some((id, url)) => {
            w.buf = url.into_bytes();
            id
        }
        None => 0,
    })
}

#[unsafe(no_mangle)]
pub extern "C" fn rb_web_buffer_len() -> usize {
    with(|w| w.buf.len())
}

/// A fetch finished; the body (or error message if `ok` is 0) is in the
/// shared buffer.
#[unsafe(no_mangle)]
pub extern "C" fn rb_web_fetch_done(id: u32, len: usize, ok: i32) {
    with(|w| {
        let bytes = w.buf[..len.min(w.buf.len())].to_vec();
        let result = if ok != 0 {
            Ok(bytes)
        } else {
            Err(String::from_utf8_lossy(&bytes).into_owned())
        };
        w.browser.fetch_done(id, result, &mut w.host);
    });
}

/// 0 default, 1 pointer, 2 text.
#[unsafe(no_mangle)]
pub extern "C" fn rb_web_cursor() -> i32 {
    with(|w| match w.host.cursor {
        Cursor::Default => 0,
        Cursor::Pointer => 1,
        Cursor::Text => 2,
    })
}

/// Puts the window title in the shared buffer; returns its length.
#[unsafe(no_mangle)]
pub extern "C" fn rb_web_title() -> usize {
    with(|w| {
        w.buf = w.host.title.clone().into_bytes();
        w.buf.len()
    })
}

/// Puts the last copied text in the shared buffer; returns its length.
#[unsafe(no_mangle)]
pub extern "C" fn rb_web_clipboard() -> usize {
    with(|w| {
        w.buf = w.host.clipboard.clone().into_bytes();
        w.buf.len()
    })
}

/// Milliseconds until the browser wants a frame without input, or -1.
#[unsafe(no_mangle)]
pub extern "C" fn rb_web_next_wakeup(now: f64) -> f64 {
    WEB.with(|w| {
        w.borrow()
            .as_ref()
            .and_then(|w| w.browser.next_wakeup(now))
            .map_or(-1.0, |t| (t - now).max(0.0))
    })
}
