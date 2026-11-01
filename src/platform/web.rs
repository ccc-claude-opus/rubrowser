//! Canvas version: plain `extern "C"` exports driven by `web/glue.js`.
//!
//! No wasm-bindgen. JS calls `rb_web_init` once, `rb_web_resize` on resize,
//! and `rb_web_frame` every animation frame, which returns a pointer to
//! `width * height` RGBA pixels in wasm memory ready for `putImageData`.

use std::cell::RefCell;

use crate::browser::Browser;

struct Web {
    browser: Browser,
    rgba: Vec<u8>,
}

thread_local! {
    static WEB: RefCell<Option<Web>> = const { RefCell::new(None) };
}

#[unsafe(no_mangle)]
pub extern "C" fn rb_web_init(width: u32, height: u32, scale: f32) {
    let web = Web {
        browser: Browser::new(width as usize, height as usize, scale),
        rgba: Vec::new(),
    };
    WEB.with(|w| *w.borrow_mut() = Some(web));
}

#[unsafe(no_mangle)]
pub extern "C" fn rb_web_resize(width: u32, height: u32, scale: f32) {
    WEB.with(|w| {
        if let Some(web) = w.borrow_mut().as_mut() {
            web.browser.resize(width as usize, height as usize, scale);
        }
    });
}

#[unsafe(no_mangle)]
pub extern "C" fn rb_web_frame() -> *const u8 {
    WEB.with(|w| {
        let mut w = w.borrow_mut();
        let Some(web) = w.as_mut() else {
            return std::ptr::null();
        };
        let pixels = web.browser.render().pixels();
        web.rgba.clear();
        web.rgba.reserve(pixels.len() * 4);
        for &p in pixels {
            web.rgba
                .extend_from_slice(&[(p >> 16) as u8, (p >> 8) as u8, p as u8, 0xff]);
        }
        web.rgba.as_ptr()
    })
}
