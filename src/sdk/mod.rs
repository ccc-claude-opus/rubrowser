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
        pub(crate) mod ffi {
            #[link(wasm_import_module = "rb")]
            unsafe extern "C" {
                $(pub fn $name($($arg: $ty),*) $(-> $ret)?;)*
            }
        }
        #[cfg(not(target_arch = "wasm32"))]
        #[allow(unused_variables, clippy::missing_safety_doc)]
        pub(crate) mod ffi {
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
    fn unlisten(h: i32, p: *const u8, n: usize, id: i32);
    fn prevent_default();
    fn stop_propagation();
    fn set_timeout(ms: f64, id: i32);
    fn clear_timeout(id: i32);
    fn navigate(p: *const u8, n: usize);
    fn log(p: *const u8, n: usize);
    fn now() -> f64;
    fn wall_time() -> f64;
    fn node_type(h: i32) -> i32;
    fn tag_name(h: i32, p: *mut u8, cap: usize) -> i32;
    fn relative(h: i32, which: i32) -> i32;
    fn child_count(h: i32) -> i32;
    fn child_at(h: i32, i: i32) -> i32;
    fn special(which: i32) -> i32;
    fn matches(h: i32, p: *const u8, n: usize) -> i32;
    fn get_html(h: i32, outer: i32, p: *mut u8, cap: usize) -> i32;
    fn set_html(h: i32, p: *const u8, n: usize);
    fn attr_count(h: i32) -> i32;
    fn attr_name_at(h: i32, i: i32, p: *mut u8, cap: usize) -> i32;
    fn event_string(which: i32, p: *mut u8, cap: usize) -> i32;
    fn event_int(which: i32) -> i32;
    fn event_pos(which: i32) -> f64;
    fn location(p: *mut u8, cap: usize) -> i32;
    fn history_go(delta: i32);
}

/// Reads a string the browser writes into a buffer, growing it if needed.
pub(crate) fn read_string(mut get: impl FnMut(*mut u8, usize) -> i32) -> Option<String> {
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

/// A DOM node of the page: usually an element, but also text nodes and
/// the document (see [`Element::node_type`]).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct Element(i32);

/// [`Element::node_type`] values, as in the DOM.
pub mod node_type {
    pub const ELEMENT: i32 = 1;
    pub const TEXT: i32 = 3;
    pub const COMMENT: i32 = 8;
    pub const DOCUMENT: i32 = 9;
}

fn handle(h: i32) -> Option<Element> {
    (h >= 0).then_some(Element(h))
}

/// The document node (listen here for events anywhere on the page).
pub fn document() -> Element {
    Element(unsafe { ffi::special(0) })
}

/// The `<html>` element.
pub fn document_element() -> Option<Element> {
    handle(unsafe { ffi::special(1) })
}

pub fn head() -> Option<Element> {
    handle(unsafe { ffi::special(2) })
}

pub fn body() -> Option<Element> {
    handle(unsafe { ffi::special(3) })
}

/// The page's URL.
pub fn location() -> String {
    read_string(|p, cap| unsafe { ffi::location(p, cap) }).unwrap_or_default()
}

/// Moves through the tab's history (`-1` is back).
pub fn history_go(delta: i32) {
    unsafe { ffi::history_go(delta) }
}

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

    /// The parent element.
    pub fn parent(self) -> Option<Element> {
        handle(unsafe { ffi::parent(self.0) })
    }

    /// The parent node (may be the document).
    pub fn parent_node(self) -> Option<Element> {
        handle(unsafe { ffi::relative(self.0, 0) })
    }

    pub fn first_child(self) -> Option<Element> {
        handle(unsafe { ffi::relative(self.0, 1) })
    }

    pub fn last_child(self) -> Option<Element> {
        handle(unsafe { ffi::relative(self.0, 2) })
    }

    pub fn next_sibling(self) -> Option<Element> {
        handle(unsafe { ffi::relative(self.0, 3) })
    }

    pub fn previous_sibling(self) -> Option<Element> {
        handle(unsafe { ffi::relative(self.0, 4) })
    }

    /// All child nodes (elements, text, comments).
    pub fn child_nodes(self) -> Vec<Element> {
        let n = unsafe { ffi::child_count(self.0) }.max(0);
        (0..n)
            .filter_map(|i| handle(unsafe { ffi::child_at(self.0, i) }))
            .collect()
    }

    /// Child elements only.
    pub fn children(self) -> Vec<Element> {
        self.child_nodes()
            .into_iter()
            .filter(|c| c.node_type() == node_type::ELEMENT)
            .collect()
    }

    /// See [`node_type`]; -1 for an invalid handle.
    pub fn node_type(self) -> i32 {
        unsafe { ffi::node_type(self.0) }
    }

    /// Lowercase tag name (empty for non-elements).
    pub fn tag(self) -> String {
        read_string(|p, cap| unsafe { ffi::tag_name(self.0, p, cap) }).unwrap_or_default()
    }

    /// Whether the element matches a CSS selector.
    pub fn matches(self, selector: &str) -> bool {
        unsafe { ffi::matches(self.0, selector.as_ptr(), selector.len()) != 0 }
    }

    /// The nearest inclusive ancestor matching `selector`.
    pub fn closest(self, selector: &str) -> Option<Element> {
        let mut e = Some(self);
        while let Some(x) = e {
            if x.matches(selector) {
                return Some(x);
            }
            e = x.parent();
        }
        None
    }

    /// The element's contents as HTML.
    pub fn inner_html(self) -> String {
        read_string(|p, cap| unsafe { ffi::get_html(self.0, 0, p, cap) }).unwrap_or_default()
    }

    /// Replaces the element's contents with parsed `html`.
    pub fn set_inner_html(self, html: &str) {
        unsafe { ffi::set_html(self.0, html.as_ptr(), html.len()) }
    }

    /// The element and its contents as HTML.
    pub fn outer_html(self) -> String {
        read_string(|p, cap| unsafe { ffi::get_html(self.0, 1, p, cap) }).unwrap_or_default()
    }

    /// Names of the element's attributes.
    pub fn attr_names(self) -> Vec<String> {
        let n = unsafe { ffi::attr_count(self.0) }.max(0);
        (0..n)
            .filter_map(|i| read_string(|p, cap| unsafe { ffi::attr_name_at(self.0, i, p, cap) }))
            .collect()
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
    /// (events bubble). Returns a handle for [`Element::off`].
    ///
    /// Events: `click`, `dblclick`, `mousedown`, `mouseup`, `mouseover`,
    /// `mouseout`, `input`, `change`, `keydown`, `focus`, `blur`.
    pub fn on(self, event: &str, handler: impl FnMut(&Event) + 'static) -> Listener {
        let id = register(Callback::Listener(Box::new(handler)));
        unsafe { ffi::listen(self.0, event.as_ptr(), event.len(), id) };
        Listener {
            element: self,
            event: event.to_string(),
            id,
        }
    }

    /// Removes a listener added with [`Element::on`].
    pub fn off(self, listener: &Listener) {
        let e = &listener.event;
        unsafe { ffi::unlisten(self.0, e.as_ptr(), e.len(), listener.id) };
        take(listener.id);
    }
}

/// A registered event listener, for [`Element::off`].
#[derive(Clone, Debug)]
pub struct Listener {
    element: Element,
    event: String,
    id: i32,
}

impl Listener {
    /// Removes this listener.
    pub fn remove(&self) {
        self.element.off(self);
    }
}

/// An event delivered to a handler.
#[derive(Clone, Debug)]
pub struct Event {
    /// `click`, `keydown`, …
    pub kind: String,
    /// Where the event happened (may be a descendant of the listener).
    pub target: Element,
    /// The element whose listener is running.
    pub current_target: Element,
    /// DOM-style key name for `keydown`: `"a"`, `"Enter"`, `"ArrowLeft"`…
    pub key: String,
    /// Key code for `keydown` (see [`key`]); 0 otherwise.
    pub key_code: i32,
    /// Modifier bits (see [`modifier`]).
    pub mods: i32,
    /// Pointer position for mouse events, in page coordinates.
    pub x: f64,
    pub y: f64,
    pub button: i32,
}

impl Event {
    fn current() -> Event {
        let s =
            |w| read_string(|p, cap| unsafe { ffi::event_string(w, p, cap) }).unwrap_or_default();
        let i = |w| unsafe { ffi::event_int(w) };
        Event {
            kind: s(0),
            target: Element(i(0)),
            current_target: Element(i(1)),
            key: s(1),
            key_code: i(2),
            mods: i(3),
            x: unsafe { ffi::event_pos(0) },
            y: unsafe { ffi::event_pos(1) },
            button: i(4),
        }
    }

    /// Cancels the browser's default action (e.g. following a link).
    pub fn prevent_default(&self) {
        unsafe { ffi::prevent_default() }
    }

    /// Stops the event from bubbling to ancestors.
    pub fn stop_propagation(&self) {
        unsafe { ffi::stop_propagation() }
    }
}

/// A pending timer, for [`clear_timeout`].
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Timer(i32);

/// Runs `f` once after `ms` milliseconds.
pub fn set_timeout(ms: f64, f: impl FnOnce() + 'static) -> Timer {
    let id = register(Callback::Timeout(Box::new(f)));
    unsafe { ffi::set_timeout(ms, id) };
    Timer(id)
}

/// Runs `f` every `ms` milliseconds.
pub fn set_interval(ms: f64, f: impl FnMut() + 'static) -> Timer {
    let id = register(Callback::Interval(ms, Box::new(f)));
    unsafe { ffi::set_timeout(ms, id) };
    Timer(id)
}

/// Cancels a timeout or interval.
pub fn clear_timeout(timer: Timer) {
    unsafe { ffi::clear_timeout(timer.0) };
    if take(timer.0).is_none() {
        // Cleared from inside its own callback, which is running now.
        CLEARED.with(|c| c.borrow_mut().insert(timer.0));
    }
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

/// Wall-clock time: milliseconds since the Unix epoch.
pub fn wall_time() -> f64 {
    unsafe { ffi::wall_time() }
}

enum Callback {
    Listener(Box<dyn FnMut(&Event)>),
    Timeout(Box<dyn FnOnce()>),
    Interval(f64, Box<dyn FnMut()>),
}

thread_local! {
    static CALLBACKS: RefCell<Vec<Option<Callback>>> = const { RefCell::new(Vec::new()) };
    /// Intervals cleared while their callback was running.
    static CLEARED: RefCell<std::collections::HashSet<i32>> =
        RefCell::new(std::collections::HashSet::new());
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
pub fn __event(id: i32) {
    if let Some(Callback::Listener(mut f)) = take(id) {
        f(&Event::current());
        put_back(id, Callback::Listener(f));
    }
}

#[doc(hidden)]
pub fn __timer(id: i32) {
    match take(id) {
        Some(Callback::Timeout(f)) => f(),
        Some(Callback::Interval(ms, mut f)) => {
            f();
            // The callback may have cleared its own interval.
            let cleared = CLEARED.with(|c| c.borrow_mut().remove(&id));
            if !cleared {
                put_back(id, Callback::Interval(ms, f));
                unsafe { ffi::set_timeout(ms, id) }
            }
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
        pub extern "C" fn rb_event(id: i32) {
            $crate::sdk::__event(id)
        }

        #[unsafe(no_mangle)]
        pub extern "C" fn rb_timer(id: i32) {
            $crate::sdk::__timer(id)
        }
    };
}
