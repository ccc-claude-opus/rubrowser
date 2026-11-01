//! Write rubrowser page scripts in Rust — WebAssembly instead of JavaScript.
//!
//! Depend on rubrowser with only this feature:
//!
//! ```toml
//! [lib]
//! crate-type = ["cdylib"]
//!
//! [dependencies]
//! rubrowser = { version = "0.1", default-features = false, features = ["sdk"] }
//! ```
//!
//! ```ignore
//! use rubrowser::sdk::*;
//!
//! fn init() {
//!     let count = query("#count").unwrap();
//!     let mut n = 0;
//!     query("#inc").unwrap().on("click", move |_| {
//!         n += 1;
//!         count.set_text(&n.to_string());
//!     });
//! }
//!
//! rubrowser::script!(init);
//! ```
//!
//! Build with `cargo build --release --target wasm32-unknown-unknown` and
//! load it from HTML: `<script type="application/wasm" src="app.wasm">`.

use std::cell::RefCell;

pub use crate::abi::{key, modifier};

/// Declares the `rb` imports, with stubs on non-wasm targets so the crate
/// still builds (and documents) everywhere.
macro_rules! imports {
    ($(fn $name:ident($($arg:ident: $ty:ty),*) $(-> $ret:ty)?;)*) => {
        #[cfg(target_arch = "wasm32")]
        mod ffi {
            #[link(wasm_import_module = "rb")]
            unsafe extern "C" {
                $(pub fn $name($($arg: $ty),*) $(-> $ret)?;)*
            }
        }
        #[cfg(not(target_arch = "wasm32"))]
        #[allow(unused_variables, clippy::missing_safety_doc)]
        mod ffi {
            $(pub unsafe fn $name($($arg: $ty),*) $(-> $ret)? {
                unimplemented!("rubrowser scripts only run on wasm32")
            })*
        }
    };
}

imports! {
    fn query(p: *const u8, n: usize) -> i32;
    fn query_all(p: *const u8, n: usize, out: *mut i32, max: i32) -> i32;
    fn parent(h: i32) -> i32;
    fn get_text(h: i32, p: *mut u8, cap: usize) -> i32;
    fn set_text(h: i32, p: *const u8, n: usize);
    fn get_attr(h: i32, np: *const u8, nl: usize, p: *mut u8, cap: usize) -> i32;
    fn set_attr(h: i32, np: *const u8, nl: usize, vp: *const u8, vl: usize);
    fn remove_attr(h: i32, np: *const u8, nl: usize);
    fn class_op(h: i32, op: i32, p: *const u8, n: usize) -> i32;
    fn set_style(h: i32, pp: *const u8, pl: usize, vp: *const u8, vl: usize);
    fn get_value(h: i32, p: *mut u8, cap: usize) -> i32;
    fn set_value(h: i32, p: *const u8, n: usize);
    fn create_element(p: *const u8, n: usize) -> i32;
    fn create_text(p: *const u8, n: usize) -> i32;
    fn insert(parent: i32, child: i32, before: i32) -> i32;
    fn remove(h: i32);
    fn listen(h: i32, p: *const u8, n: usize, id: i32);
    fn prevent_default();
    fn set_timeout(ms: f64, id: i32);
    fn navigate(p: *const u8, n: usize);
    fn log(p: *const u8, n: usize);
    fn now() -> f64;
}

/// Reads a string the browser writes into a buffer, growing it if needed.
fn read_string(mut get: impl FnMut(*mut u8, usize) -> i32) -> Option<String> {
    let mut buf = vec![0u8; 256];
    loop {
        let n = get(buf.as_mut_ptr(), buf.len());
        if n < 0 {
            return None;
        }
        let n = n as usize;
        if n <= buf.len() {
            buf.truncate(n);
            return Some(String::from_utf8_lossy(&buf).into_owned());
        }
        buf.resize(n, 0);
    }
}

/// A DOM element (or text node) of the page.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct Element(i32);

/// The first element matching a CSS selector.
pub fn query(selector: &str) -> Option<Element> {
    let h = unsafe { ffi::query(selector.as_ptr(), selector.len()) };
    (h >= 0).then_some(Element(h))
}

/// All elements matching a CSS selector, in document order.
pub fn query_all(selector: &str) -> Vec<Element> {
    let mut out = vec![0i32; 64];
    loop {
        let n = unsafe {
            ffi::query_all(
                selector.as_ptr(),
                selector.len(),
                out.as_mut_ptr(),
                out.len() as i32,
            )
        } as usize;
        if n <= out.len() {
            out.truncate(n);
            return out.into_iter().map(Element).collect();
        }
        out.resize(n, 0);
    }
}

/// Creates a detached element; insert it with [`Element::append`].
pub fn create(tag: &str) -> Element {
    Element(unsafe { ffi::create_element(tag.as_ptr(), tag.len()) })
}

impl Element {
    /// The raw node handle.
    pub fn handle(self) -> i32 {
        self.0
    }

    pub fn parent(self) -> Option<Element> {
        let h = unsafe { ffi::parent(self.0) };
        (h >= 0).then_some(Element(h))
    }

    /// All text inside the element.
    pub fn text(self) -> String {
        read_string(|p, cap| unsafe { ffi::get_text(self.0, p, cap) }).unwrap_or_default()
    }

    /// Replaces the element's children with `text`.
    pub fn set_text(self, text: &str) {
        unsafe { ffi::set_text(self.0, text.as_ptr(), text.len()) }
    }

    pub fn attr(self, name: &str) -> Option<String> {
        read_string(|p, cap| unsafe { ffi::get_attr(self.0, name.as_ptr(), name.len(), p, cap) })
    }

    pub fn set_attr(self, name: &str, value: &str) {
        unsafe {
            ffi::set_attr(
                self.0,
                name.as_ptr(),
                name.len(),
                value.as_ptr(),
                value.len(),
            )
        }
    }

    pub fn remove_attr(self, name: &str) {
        unsafe { ffi::remove_attr(self.0, name.as_ptr(), name.len()) }
    }

    fn class_op(self, op: i32, class: &str) -> bool {
        unsafe { ffi::class_op(self.0, op, class.as_ptr(), class.len()) != 0 }
    }

    pub fn has_class(self, class: &str) -> bool {
        self.class_op(0, class)
    }

    pub fn add_class(self, class: &str) {
        self.class_op(1, class);
    }

    pub fn remove_class(self, class: &str) {
        self.class_op(2, class);
    }

    /// Adds or removes `class`; returns whether it is now present.
    pub fn toggle_class(self, class: &str) -> bool {
        self.class_op(3, class)
    }

    /// Sets one inline style property (an empty value removes it).
    pub fn set_style(self, property: &str, value: &str) {
        unsafe {
            ffi::set_style(
                self.0,
                property.as_ptr(),
                property.len(),
                value.as_ptr(),
                value.len(),
            )
        }
    }

    /// The current value of an `<input>`.
    pub fn value(self) -> String {
        read_string(|p, cap| unsafe { ffi::get_value(self.0, p, cap) }).unwrap_or_default()
    }

    pub fn set_value(self, value: &str) {
        unsafe { ffi::set_value(self.0, value.as_ptr(), value.len()) }
    }

    /// Appends `child` (moving it if it is already in the document).
    pub fn append(self, child: Element) {
        unsafe { ffi::insert(self.0, child.0, -1) };
    }

    /// Inserts `child` before `before`, which must be a child of `self`.
    pub fn insert_before(self, child: Element, before: Element) {
        unsafe { ffi::insert(self.0, child.0, before.0) };
    }

    /// Appends a text node.
    pub fn append_text(self, text: &str) {
        let t = unsafe { ffi::create_text(text.as_ptr(), text.len()) };
        unsafe { ffi::insert(self.0, t, -1) };
    }

    /// Detaches the element from the document.
    pub fn remove(self) {
        unsafe { ffi::remove(self.0) }
    }

    /// Calls `handler` for `event` on this element or its descendants
    /// (events bubble). Events: `click`, `input`, `change`, `keydown`.
    pub fn on(self, event: &str, handler: impl FnMut(&Event) + 'static) {
        let id = register(Callback::Listener(Box::new(handler)));
        unsafe { ffi::listen(self.0, event.as_ptr(), event.len(), id) }
    }
}

/// An event delivered to a handler.
#[derive(Clone, Copy, Debug)]
pub struct Event {
    /// Where the event happened (may be a descendant of the listener).
    pub target: Element,
    /// Key code for `keydown` (see [`key`]); 0 otherwise.
    pub key: i32,
    /// Modifier bits for `keydown` (see [`modifier`]).
    pub mods: i32,
}

impl Event {
    /// Cancels the browser's default action (e.g. following a link).
    pub fn prevent_default(&self) {
        unsafe { ffi::prevent_default() }
    }
}

/// Runs `f` once after `ms` milliseconds.
pub fn set_timeout(ms: f64, f: impl FnOnce() + 'static) {
    let id = register(Callback::Timeout(Box::new(f)));
    unsafe { ffi::set_timeout(ms, id) }
}

/// Runs `f` every `ms` milliseconds.
pub fn set_interval(ms: f64, f: impl FnMut() + 'static) {
    let id = register(Callback::Interval(ms, Box::new(f)));
    unsafe { ffi::set_timeout(ms, id) }
}

/// Opens `url` (relative to the page) in the current tab.
pub fn navigate(url: &str) {
    unsafe { ffi::navigate(url.as_ptr(), url.len()) }
}

/// Writes a message to the browser's console.
pub fn log(message: &str) {
    unsafe { ffi::log(message.as_ptr(), message.len()) }
}

/// Milliseconds since the browser started.
pub fn now() -> f64 {
    unsafe { ffi::now() }
}

enum Callback {
    Listener(Box<dyn FnMut(&Event)>),
    Timeout(Box<dyn FnOnce()>),
    Interval(f64, Box<dyn FnMut()>),
}

thread_local! {
    static CALLBACKS: RefCell<Vec<Option<Callback>>> = const { RefCell::new(Vec::new()) };
}

fn register(cb: Callback) -> i32 {
    CALLBACKS.with(|c| {
        let mut c = c.borrow_mut();
        c.push(Some(cb));
        (c.len() - 1) as i32
    })
}

/// Takes callback `id` out of the registry (so it may register others).
fn take(id: i32) -> Option<Callback> {
    CALLBACKS.with(|c| c.borrow_mut().get_mut(id as usize)?.take())
}

fn put_back(id: i32, cb: Callback) {
    CALLBACKS.with(|c| {
        if let Some(slot) = c.borrow_mut().get_mut(id as usize) {
            *slot = Some(cb);
        }
    });
}

#[doc(hidden)]
pub fn __event(id: i32, target: i32, key: i32, mods: i32) {
    if let Some(Callback::Listener(mut f)) = take(id) {
        f(&Event {
            target: Element(target),
            key,
            mods,
        });
        put_back(id, Callback::Listener(f));
    }
}

#[doc(hidden)]
pub fn __timer(id: i32) {
    match take(id) {
        Some(Callback::Timeout(f)) => f(),
        Some(Callback::Interval(ms, mut f)) => {
            f();
            put_back(id, Callback::Interval(ms, f));
            unsafe { ffi::set_timeout(ms, id) }
        }
        other => {
            if let Some(cb) = other {
                put_back(id, cb);
            }
        }
    }
}

/// Defines the exports the browser calls: `rb_init` runs `$init` once the
/// page is loaded; events and timers are routed to registered closures.
#[macro_export]
macro_rules! script {
    ($init:path) => {
        #[unsafe(no_mangle)]
        pub extern "C" fn rb_init() {
            $init()
        }

        #[unsafe(no_mangle)]
        pub extern "C" fn rb_event(id: i32, target: i32, key: i32, mods: i32) {
            $crate::sdk::__event(id, target, key, mods)
        }

        #[unsafe(no_mangle)]
        pub extern "C" fn rb_timer(id: i32) {
            $crate::sdk::__timer(id)
        }
    };
}
