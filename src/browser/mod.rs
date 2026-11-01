//! The browser: an HTML/CSS chrome document (tabs, toolbar, address bar)
//! with the active tab's page drawn inside its `#viewport` element.
//!
//! The chrome's behavior lives here in Rust: clicks on elements with a
//! `data-action` attribute run browser commands, and the tab strip and
//! address bar are kept in sync by editing the chrome DOM.

use std::collections::HashMap;

use crate::abi::key;
use crate::css::ElementState;
use crate::document::Document;
use crate::dom::{Element, NodeId};
use crate::gfx::Canvas;
use crate::input::{Cursor, Event, Host, Modifiers};
use crate::layout::RectF;
use crate::net::url;
use crate::script::Script;
use crate::text::Fonts;
use crate::ui::textfield::Action;

const CHROME_HTML: &str = include_str!("chrome.html");
const HOME_HTML: &str = include_str!("pages/home.html");
const CARET_BLINK_MS: f64 = 530.0;
const WHEEL_LINE: f32 = 40.0;

/// Pages and files built into the browser, served as `about:NAME`.
fn builtin(name: &str) -> Option<&'static [u8]> {
    Some(match name {
        "home" => HOME_HTML.as_bytes(),
        "blank" => b"",
        "counter" => include_bytes!("pages/counter.html"),
        "counter.wasm" => include_bytes!("pages/counter.wasm"),
        "todo" => include_bytes!("pages/todo.html"),
        "todo.wasm" => include_bytes!("pages/todo.wasm"),
        "stopwatch" => include_bytes!("pages/stopwatch.html"),
        "stopwatch.wasm" => include_bytes!("pages/stopwatch.wasm"),
        _ => return None,
    })
}

/// Which document an event goes to.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Target {
    Chrome,
    Page,
}

/// A mouse press in progress.
#[derive(Clone, Copy, Debug)]
struct Press {
    target: Target,
    element: Option<NodeId>,
    /// Text input being drag-selected.
    input: Option<NodeId>,
}

/// What an outstanding fetch is for.
#[derive(Clone, Debug)]
enum Request {
    Page { tab: u64, serial: u64, url: String },
    Stylesheet { tab: u64, serial: u64, link: NodeId },
    Script { tab: u64, serial: u64, url: String },
}

/// A pending `set_timeout` of a script.
#[derive(Clone, Copy, Debug)]
struct Timer {
    due: f64,
    script: usize,
    id: i32,
}

struct Tab {
    id: u64,
    history: Vec<String>,
    index: usize,
    doc: Document,
    /// Increases with every navigation, to drop stale fetch results.
    serial: u64,
    loading: bool,
    /// The page's wasm scripts (`None` once a script has crashed).
    scripts: Vec<Option<Script>>,
    timers: Vec<Timer>,
}

impl Tab {
    fn url(&self) -> &str {
        &self.history[self.index]
    }

    fn title(&self) -> String {
        if self.loading {
            return "Loading…".into();
        }
        self.doc.title().unwrap_or_else(|| url::title(self.url()))
    }
}

/// Platform-independent browser state. Platforms feed it input events and
/// the window size, and put the rendered canvas on screen.
pub struct Browser {
    fonts: Fonts,
    canvas: Canvas,
    scale: f32,
    chrome: Document,
    tabs: Vec<Tab>,
    active: usize,
    next_id: u32,
    next_tab: u64,
    requests: HashMap<u32, Request>,
    keyboard: Target,
    pointer: (f32, f32),
    press: Option<Press>,
    mods: Modifiers,
    /// Time the caret last became visible (it blinks from there).
    caret_epoch: f64,
    now: f64,
    dirty: bool,
    /// Tab strip as last written to the chrome DOM (title, active).
    shown_tabs: Vec<(String, bool)>,
    /// Built-in resources fetched, delivered on the next update.
    local: Vec<(u32, Result<Vec<u8>, String>)>,
}

impl Browser {
    /// `width` and `height` are physical pixels; `scale` is physical pixels
    /// per logical pixel (the display's DPI factor).
    pub fn new(width: usize, height: usize, scale: f32) -> Self {
        let mut b = Browser {
            fonts: Fonts::new(),
            canvas: Canvas::new(width, height),
            scale: sanitize_scale(scale),
            chrome: Document::from_html(CHROME_HTML, "about:chrome"),
            tabs: Vec::new(),
            active: 0,
            next_id: 1,
            next_tab: 1,
            requests: HashMap::new(),
            keyboard: Target::Page,
            pointer: (-1.0, -1.0),
            press: None,
            mods: Modifiers::default(),
            caret_epoch: 0.0,
            now: 0.0,
            dirty: true,
            shown_tabs: Vec::new(),
            local: Vec::new(),
        };
        b.push_tab("about:home");
        b.tabs[0].doc = Document::from_html(HOME_HTML, "about:home");
        b
    }

    pub fn resize(&mut self, width: usize, height: usize, scale: f32) {
        self.canvas.resize(width, height);
        self.scale = sanitize_scale(scale);
        self.dirty = true;
    }

    fn logical_size(&self) -> (f32, f32) {
        (
            self.canvas.width() as f32 / self.scale,
            self.canvas.height() as f32 / self.scale,
        )
    }

    // ----- tabs and navigation -------------------------------------------

    fn push_tab(&mut self, url: &str) {
        let id = self.next_tab;
        self.next_tab += 1;
        self.tabs.push(Tab {
            id,
            history: vec![url.to_string()],
            index: 0,
            doc: Document::from_html("", url),
            serial: 0,
            loading: false,
            scripts: Vec::new(),
            timers: Vec::new(),
        });
        self.active = self.tabs.len() - 1;
    }

    fn tab_index(&self, id: u64) -> Option<usize> {
        self.tabs.iter().position(|t| t.id == id)
    }

    /// Opens `url` in a new tab and makes it active.
    pub fn new_tab(&mut self, url: &str, host: &mut dyn Host) {
        self.push_tab(url);
        self.load(self.active, host);
        if url == "about:home" || url == "about:blank" {
            self.focus_address(true);
        }
        self.dirty = true;
    }

    fn close_tab(&mut self, i: usize, host: &mut dyn Host) {
        if i >= self.tabs.len() {
            return;
        }
        self.tabs.remove(i);
        if self.tabs.is_empty() {
            self.new_tab("about:home", host);
            return;
        }
        if self.active > i || self.active >= self.tabs.len() {
            self.active = self.active.saturating_sub(1);
        }
        self.dirty = true;
    }

    /// Navigates the active tab to `url` (already resolved), adding it to
    /// the history.
    pub fn navigate(&mut self, url: &str, host: &mut dyn Host) {
        if url.is_empty() {
            return;
        }
        let tab = &mut self.tabs[self.active];
        tab.history.truncate(tab.index + 1);
        tab.history.push(url.to_string());
        tab.index += 1;
        self.load(self.active, host);
        self.keyboard = Target::Page;
        self.set_focus(Target::Chrome, None);
    }

    fn go(&mut self, delta: isize, host: &mut dyn Host) {
        let tab = &mut self.tabs[self.active];
        let next = tab.index as isize + delta;
        if next >= 0 && (next as usize) < tab.history.len() {
            tab.index = next as usize;
            self.load(self.active, host);
        }
    }

    /// (Re)loads the current history entry of tab `i`.
    fn load(&mut self, i: usize, host: &mut dyn Host) {
        let tab = &mut self.tabs[i];
        tab.serial += 1;
        let url = tab.url().to_string();
        self.dirty = true;
        if let Some(name) = url.strip_prefix("about:") {
            let html = match builtin(name) {
                Some(bytes) => as_html(&url, bytes),
                None => error_page(&url, "This about: page does not exist."),
            };
            self.set_page(i, &html, &url, host);
            return;
        }
        tab.loading = true;
        let req = Request::Page {
            tab: tab.id,
            serial: tab.serial,
            url: url.clone(),
        };
        self.fetch(req, &url, host);
    }

    /// Starts loading `url` for `req`; built-in `about:` files complete on
    /// the next update.
    fn fetch(&mut self, req: Request, url: &str, host: &mut dyn Host) {
        let id = self.next_id;
        self.next_id += 1;
        self.requests.insert(id, req);
        if let Some(name) = url.strip_prefix("about:") {
            let result = builtin(name)
                .map(<[u8]>::to_vec)
                .ok_or_else(|| format!("{url} does not exist"));
            self.local.push((id, result));
        } else {
            host.fetch(id, url);
        }
    }

    fn set_page(&mut self, i: usize, html: &str, url: &str, host: &mut dyn Host) {
        let tab = &mut self.tabs[i];
        tab.loading = false;
        tab.doc = Document::from_html(html, url);
        tab.scripts.clear();
        tab.timers.clear();
        let (id, serial) = (tab.id, tab.serial);
        let links = tab.doc.stylesheet_links();
        let scripts = wasm_scripts(&tab.doc);
        for (link, href) in links {
            let req = Request::Stylesheet {
                tab: id,
                serial,
                link,
            };
            self.fetch(req, &url::resolve(url, &href), host);
        }
        // Scripts run in document order: they are fetched in order and
        // each is started when it arrives (in order for local files).
        for src in scripts {
            let target = url::resolve(url, &src);
            let req = Request::Script {
                tab: id,
                serial,
                url: target.clone(),
            };
            self.fetch(req, &target, host);
        }
        self.dirty = true;
    }

    /// Called by the platform when fetch `id` completes.
    pub fn fetch_done(&mut self, id: u32, result: Result<Vec<u8>, String>, host: &mut dyn Host) {
        let Some(req) = self.requests.remove(&id) else {
            return;
        };
        match req {
            Request::Page { tab, serial, url } => {
                let Some(i) = self
                    .tab_index(tab)
                    .filter(|&i| self.tabs[i].serial == serial)
                else {
                    return;
                };
                let html = match result {
                    Ok(bytes) => as_html(&url, &bytes),
                    Err(e) => error_page(&url, &e),
                };
                self.set_page(i, &html, &url, host);
            }
            Request::Stylesheet { tab, serial, link } => {
                let Some(i) = self
                    .tab_index(tab)
                    .filter(|&i| self.tabs[i].serial == serial)
                else {
                    return;
                };
                if let Ok(bytes) = result {
                    let css = String::from_utf8_lossy(&bytes).into_owned();
                    self.tabs[i].doc.add_stylesheet(link, css);
                    self.dirty = true;
                }
            }
            Request::Script { tab, serial, url } => {
                let Some(i) = self
                    .tab_index(tab)
                    .filter(|&i| self.tabs[i].serial == serial)
                else {
                    return;
                };
                let bytes = match result {
                    Ok(b) => b,
                    Err(e) => return host.log(&format!("{url}: {e}")),
                };
                let now = self.now;
                let t = &mut self.tabs[i];
                match Script::load(&bytes, &mut t.doc, now) {
                    Ok(script) => {
                        t.scripts.push(Some(script));
                        let si = t.scripts.len() - 1;
                        self.after_script(i, si, host);
                    }
                    Err(e) => host.log(&format!("{url}: {e}")),
                }
                self.dirty = true;
            }
        }
    }

    // ----- scripts -----------------------------------------------------------

    /// Acts on what script `si` of tab `i` asked for; returns whether it
    /// prevented the default action.
    fn after_script(&mut self, i: usize, si: usize, host: &mut dyn Host) -> bool {
        let now = self.now;
        let Some(Some(script)) = self.tabs[i].scripts.get_mut(si) else {
            return false;
        };
        let out = script.take_output();
        for msg in &out.logs {
            host.log(msg);
        }
        let tab = &mut self.tabs[i];
        for (delay, id) in out.timers {
            tab.timers.push(Timer {
                due: now + delay,
                script: si,
                id,
            });
        }
        tab.doc.dom_changed();
        self.dirty = true;
        if let Some(target) = out.navigate
            && i == self.active
        {
            let target = url::resolve(&self.tabs[i].doc.url, &target);
            self.navigate(&target, host);
        }
        out.prevent_default
    }

    /// Reports a script failure and stops that script.
    fn script_failed(&mut self, i: usize, si: usize, err: &str, host: &mut dyn Host) {
        host.log(&format!("script error in {}: {err}", self.tabs[i].doc.url));
        self.tabs[i].scripts[si] = None;
        self.tabs[i].timers.retain(|t| t.script != si);
    }

    /// Dispatches `event` at `node` of the active page, bubbling up to the
    /// root. Returns whether a listener prevented the default action.
    fn dispatch(
        &mut self,
        node: NodeId,
        event: &str,
        key: i32,
        mods: i32,
        host: &mut dyn Host,
    ) -> bool {
        let i = self.active;
        let tab = &self.tabs[i];
        let mut calls = Vec::new();
        let mut n = Some(node);
        while let Some(id) = n {
            for (si, s) in tab.scripts.iter().enumerate() {
                for l in s.iter().flat_map(|s| s.listeners()) {
                    if l.node == id && l.event == event {
                        calls.push((si, l.id));
                    }
                }
            }
            n = tab.doc.dom.parent(id);
        }
        let mut prevented = false;
        for (si, lid) in calls {
            let now = self.now;
            let t = &mut self.tabs[i];
            let Some(Some(script)) = t.scripts.get_mut(si) else {
                continue;
            };
            match script.dispatch(&mut t.doc, lid, node, key, mods, now) {
                Ok(()) => prevented |= self.after_script(i, si, host),
                Err(e) => self.script_failed(i, si, &e, host),
            }
            if self.active != i || self.tabs.len() <= i {
                break; // the script navigated away or closed the tab
            }
        }
        prevented
    }

    /// Fires timers that are due in every tab.
    fn run_timers(&mut self, host: &mut dyn Host) {
        let now = self.now;
        for i in 0..self.tabs.len() {
            while let Some(k) = self.tabs[i].timers.iter().position(|t| t.due <= now) {
                let timer = self.tabs[i].timers.remove(k);
                let t = &mut self.tabs[i];
                let Some(Some(script)) = t.scripts.get_mut(timer.script) else {
                    continue;
                };
                match script.timer(&mut t.doc, timer.id, now) {
                    Ok(()) => {
                        self.after_script(i, timer.script, host);
                    }
                    Err(e) => self.script_failed(i, timer.script, &e, host),
                }
                if i >= self.tabs.len() {
                    return;
                }
            }
        }
    }

    fn next_timer(&self) -> Option<f64> {
        self.tabs
            .iter()
            .flat_map(|t| t.timers.iter().map(|t| t.due))
            .reduce(f64::min)
    }

    // ----- chrome document -------------------------------------------------

    fn chrome_el(&self, id: &str) -> Option<NodeId> {
        self.chrome.dom.element_by_id(id)
    }

    /// Rebuilds the tab strip and updates toolbar state from browser state.
    fn sync_chrome(&mut self, host: &mut dyn Host) {
        let tabs: Vec<(String, bool)> = self
            .tabs
            .iter()
            .enumerate()
            .map(|(i, t)| (t.title(), i == self.active))
            .collect();
        let (can_back, can_forward) = {
            let t = &self.tabs[self.active];
            (t.index > 0, t.index + 1 < t.history.len())
        };
        let address = self.chrome_el("address");
        let url = self.tabs[self.active].url().to_string();
        let title = tabs[self.active].0.clone();
        // Rebuild the strip only when it changed, so element ids stay
        // stable while the user is clicking.
        if let Some(strip) = self.chrome_el("tabs").filter(|_| tabs != self.shown_tabs) {
            self.shown_tabs = tabs.clone();
            let dom = self.chrome.dom_mut();
            for c in dom.children(strip).to_vec() {
                dom.detach(c);
            }
            for (i, (title, active)) in tabs.iter().enumerate() {
                let mut tab = Element::new("div");
                tab.set_attr("class", if *active { "tab active" } else { "tab" });
                tab.set_attr("data-tab", &i.to_string());
                let tab = dom.create_element(tab);
                let mut label = Element::new("span");
                label.set_attr("class", "title");
                let label = dom.create_element(label);
                dom.append_text(label, title);
                let mut close = Element::new("span");
                close.set_attr("class", "close");
                close.set_attr("data-action", "close-tab");
                close.set_attr("data-tab", &i.to_string());
                let close = dom.create_element(close);
                dom.append_text(close, "\u{2715}");
                dom.append(tab, label);
                dom.append(tab, close);
                dom.append(strip, tab);
            }
        }
        for (id, enabled) in [("back", can_back), ("forward", can_forward)] {
            let class = if enabled { "" } else { "disabled" };
            if let Some(n) = self.chrome_el(id)
                && self.chrome.dom.attr(n, "class").unwrap_or("") != class
                && let Some(e) = self.chrome.dom_mut().element_mut(n)
            {
                e.set_attr("class", class);
            }
        }
        if let Some(a) = address
            && self.chrome.state().focus != Some(a)
            && self.chrome.input_value(a) != url
        {
            self.chrome.set_input_value(a, &url);
        }
        host.set_title(&format!("{title} - rubrowser"));
    }

    fn focus_address(&mut self, select_all: bool) {
        if let Some(a) = self.chrome_el("address") {
            let url = self.tabs[self.active].url().to_string();
            self.chrome.set_input_value(a, &url);
            self.keyboard = Target::Chrome;
            self.set_focus(Target::Chrome, Some(a));
            if select_all {
                self.chrome.input(a).select_all();
            }
        }
    }

    // ----- input -----------------------------------------------------------

    fn doc(&self, t: Target) -> &Document {
        match t {
            Target::Chrome => &self.chrome,
            Target::Page => &self.tabs[self.active].doc,
        }
    }

    fn doc_mut(&mut self, t: Target) -> &mut Document {
        match t {
            Target::Chrome => &mut self.chrome,
            Target::Page => &mut self.tabs[self.active].doc,
        }
    }

    /// The page area in window coordinates.
    fn viewport_rect(&self) -> RectF {
        self.chrome_el("viewport")
            .and_then(|v| self.chrome.content_box(v))
            .unwrap_or_default()
    }

    /// Which document is under `(x, y)` and the point in its coordinates.
    fn target_at(&self, x: f32, y: f32) -> (Target, f32, f32) {
        let vp = self.viewport_rect();
        if vp.contains(x, y) {
            (Target::Page, x - vp.x, y - vp.y)
        } else {
            (Target::Chrome, x, y)
        }
    }

    fn set_state(&mut self, t: Target, f: impl FnOnce(&mut ElementState)) {
        let doc = self.doc_mut(t);
        let mut s = doc.state();
        f(&mut s);
        if doc.set_state(s) {
            self.dirty = true;
        }
    }

    fn set_focus(&mut self, t: Target, node: Option<NodeId>) {
        self.set_state(t, |s| s.focus = node);
        self.caret_epoch = self.now;
    }

    fn focused(&self) -> Option<(Target, NodeId)> {
        let t = self.keyboard;
        self.doc(t).state().focus.map(|n| (t, n))
    }

    /// Handles an input event. `now` is a timestamp in milliseconds.
    pub fn event(&mut self, e: Event, now: f64, host: &mut dyn Host) {
        self.now = now;
        self.update_layout(host);
        match e {
            Event::MouseMove { x, y } => self.mouse_move(x as f32, y as f32, host),
            Event::MouseLeave => {
                self.pointer = (-1.0, -1.0);
                for t in [Target::Chrome, Target::Page] {
                    self.set_state(t, |s| s.hover = None);
                }
                self.set_status(None);
            }
            Event::MouseDown { x, y, button: 0 } => self.mouse_down(x as f32, y as f32, host),
            Event::MouseUp { x, y, button: 0 } => self.mouse_up(x as f32, y as f32, host),
            Event::MouseDown { .. } | Event::MouseUp { .. } => {}
            Event::Wheel { dy, .. } => {
                let (t, lx, ly) = self.target_at(self.pointer.0, self.pointer.1);
                if self.doc_mut(t).scroll_by(lx, ly, dy as f32) {
                    self.dirty = true;
                    // Content moved under the pointer: refresh hover.
                    let (px, py) = self.pointer;
                    self.mouse_move(px, py, host);
                }
            }
            Event::KeyDown { key, mods } => {
                self.mods = mods;
                self.key_down(key, mods, host);
            }
            Event::KeyUp { mods, .. } => self.mods = mods,
            Event::Text(text) => {
                if let Some((t, n)) = self.focused()
                    && self.doc(t).is_text_input(n)
                {
                    self.doc_mut(t).input(n).insert(&text);
                    self.caret_epoch = now;
                    self.dirty = true;
                    if t == Target::Page {
                        self.dispatch(n, "input", 0, 0, host);
                    }
                }
            }
        }
    }

    fn mouse_move(&mut self, x: f32, y: f32, host: &mut dyn Host) {
        self.pointer = (x, y);
        if let Some(Press {
            target,
            input: Some(n),
            ..
        }) = self.press
        {
            let vp = self.viewport_rect();
            let lx = if target == Target::Page { x - vp.x } else { x };
            let fonts = &self.fonts;
            match target {
                Target::Chrome => self.chrome.input_pointer(n, lx, true, false, fonts),
                Target::Page => self.tabs[self.active]
                    .doc
                    .input_pointer(n, lx, true, false, fonts),
            }
            self.dirty = true;
            return;
        }
        let (t, lx, ly) = self.target_at(x, y);
        let hit = self.doc(t).hit_test(lx, ly);
        let other = if t == Target::Chrome {
            Target::Page
        } else {
            Target::Chrome
        };
        self.set_state(other, |s| s.hover = None);
        self.set_state(t, |s| s.hover = hit.element);
        let doc = self.doc(t);
        let mut cursor = doc.cursor(hit.element);
        if hit.element.is_some_and(|e| doc.is_text_input(e)) {
            cursor = Cursor::Text;
        }
        if t == Target::Chrome
            && cursor == Cursor::Text
            && !hit.element.is_some_and(|e| doc.is_text_input(e))
        {
            cursor = Cursor::Default;
        }
        host.set_cursor(cursor);
        let link = (t == Target::Page)
            .then(|| hit.element.and_then(|e| doc.link(e)))
            .flatten()
            .map(|href| url::resolve(&doc.url, &href));
        self.set_status(link);
    }

    fn set_status(&mut self, text: Option<String>) {
        let Some(s) = self.chrome_el("status") else {
            return;
        };
        let class = if text.is_some() { "show" } else { "" };
        let text = text.unwrap_or_default();
        let dom = &self.chrome.dom;
        if dom.attr(s, "class").unwrap_or("") == class && dom.text_content(s) == text {
            return;
        }
        let dom = self.chrome.dom_mut();
        if let Some(e) = dom.element_mut(s) {
            e.set_attr("class", class);
        }
        dom.set_text_content(s, &text);
        self.dirty = true;
    }

    fn mouse_down(&mut self, x: f32, y: f32, _host: &mut dyn Host) {
        let (t, lx, ly) = self.target_at(x, y);
        let hit = self.doc(t).hit_test(lx, ly);
        self.set_state(t, |s| s.active = hit.element);
        let focus = hit.element.and_then(|e| self.doc(t).focusable_ancestor(e));
        let was_focused = self.doc(t).state().focus;
        self.keyboard = t;
        let other = if t == Target::Chrome {
            Target::Page
        } else {
            Target::Chrome
        };
        self.set_focus(other, None);
        self.set_focus(t, focus);
        let mut input = None;
        if let Some(f) = focus.filter(|&f| self.doc(t).is_text_input(f)) {
            let is_address = t == Target::Chrome && Some(f) == self.chrome_el("address");
            if is_address && was_focused != Some(f) {
                // First click into the address bar selects the whole URL.
                self.chrome.input(f).select_all();
            } else {
                let shift = self.mods.shift;
                let fonts = &self.fonts;
                match t {
                    Target::Chrome => self.chrome.input_pointer(f, lx, false, shift, fonts),
                    Target::Page => self.tabs[self.active]
                        .doc
                        .input_pointer(f, lx, false, shift, fonts),
                }
                input = Some(f);
            }
        }
        self.press = Some(Press {
            target: t,
            element: hit.element,
            input,
        });
        self.dirty = true;
    }

    fn mouse_up(&mut self, x: f32, y: f32, host: &mut dyn Host) {
        let Some(press) = self.press.take() else {
            return;
        };
        self.set_state(press.target, |s| s.active = None);
        let (t, lx, ly) = self.target_at(x, y);
        if t != press.target || press.input.is_some() {
            return;
        }
        let hit = self.doc(t).hit_test(lx, ly);
        // A click: released on the pressed element or inside it.
        if let (Some(p), Some(h)) = (press.element, hit.element)
            && self.doc(t).dom.is_inclusive_ancestor(p, h)
        {
            self.click(t, h, host);
        }
    }

    /// Activates element `node` in document `t` (mouse click, Enter, Space).
    fn click(&mut self, t: Target, node: NodeId, host: &mut dyn Host) {
        match t {
            Target::Chrome => {
                let dom = &self.chrome.dom;
                let mut n = Some(node);
                while let Some(id) = n {
                    let tab = dom
                        .attr(id, "data-tab")
                        .and_then(|v| v.parse::<usize>().ok());
                    match (dom.attr(id, "data-action"), tab) {
                        (Some(action), tab) => {
                            let action = action.to_string();
                            return self.command(&action, tab, host);
                        }
                        (None, Some(i)) => {
                            if i < self.tabs.len() {
                                self.active = i;
                                self.keyboard = Target::Page;
                                self.dirty = true;
                            }
                            return;
                        }
                        _ => n = dom.parent(id),
                    }
                }
            }
            Target::Page => {
                if self.dispatch(node, "click", 0, 0, host) {
                    return;
                }
                let doc = &self.tabs[self.active].doc;
                if let Some(href) = doc.link(node) {
                    let target = url::resolve(&doc.url, &href);
                    self.navigate(&target, host);
                }
            }
        }
    }

    fn command(&mut self, action: &str, tab: Option<usize>, host: &mut dyn Host) {
        match action {
            "new-tab" => self.new_tab("about:home", host),
            "close-tab" => self.close_tab(tab.unwrap_or(self.active), host),
            "back" => self.go(-1, host),
            "forward" => self.go(1, host),
            "reload" => self.load(self.active, host),
            _ => {}
        }
    }

    fn key_down(&mut self, code: i32, mods: Modifiers, host: &mut dyn Host) {
        self.caret_epoch = self.now;
        self.dirty = true;
        // Browser shortcuts.
        let cmd = mods.command();
        match code {
            c if cmd && c == 'L' as i32 => return self.focus_address(true),
            c if cmd && c == 'T' as i32 => return self.new_tab("about:home", host),
            c if cmd && c == 'W' as i32 => return self.close_tab(self.active, host),
            c if cmd && c == 'R' as i32 => return self.load(self.active, host),
            key::F5 => return self.load(self.active, host),
            key::LEFT_BRACKET if cmd => return self.go(-1, host),
            key::RIGHT_BRACKET if cmd => return self.go(1, host),
            key::LEFT if mods.alt && self.focused_text_input().is_none() => {
                return self.go(-1, host);
            }
            key::RIGHT if mods.alt && self.focused_text_input().is_none() => {
                return self.go(1, host);
            }
            key::TAB if cmd || mods.ctrl => {
                let n = self.tabs.len();
                self.active = if mods.shift {
                    (self.active + n - 1) % n
                } else {
                    (self.active + 1) % n
                };
                return;
            }
            _ => {}
        }
        let t = self.keyboard;
        let focus = self.doc(t).state().focus;
        if code == key::TAB {
            let next = self.doc(t).next_focusable(focus, mods.shift);
            self.set_focus(t, next);
            if let Some(n) = next.filter(|&n| self.doc(t).is_text_input(n)) {
                self.doc_mut(t).input(n).select_all();
            }
            return;
        }
        if t == Target::Page {
            let target = focus
                .or_else(|| self.doc(t).dom.body())
                .unwrap_or(crate::dom::Dom::ROOT);
            if self.dispatch(target, "keydown", code, mods.bits(), host) {
                return;
            }
        }
        if let Some(n) = focus {
            if self.doc(t).is_text_input(n) {
                let before = self.doc(t).input_value(n);
                let action = self.doc_mut(t).input(n).key(code, mods, host);
                if t == Target::Page && self.doc(t).input_value(n) != before {
                    self.dispatch(n, "input", 0, 0, host);
                }
                self.input_action(t, n, action, host);
                return;
            }
            if code == key::ENTER || code == key::SPACE {
                return self.click(t, n, host);
            }
        }
        if t == Target::Page {
            self.scroll_key(code, mods);
        }
    }

    fn focused_text_input(&self) -> Option<NodeId> {
        self.focused()
            .filter(|&(t, n)| self.doc(t).is_text_input(n))
            .map(|(_, n)| n)
    }

    fn input_action(&mut self, t: Target, n: NodeId, action: Action, host: &mut dyn Host) {
        let is_address = t == Target::Chrome && Some(n) == self.chrome_el("address");
        match action {
            Action::Submit if is_address => {
                let typed = self.chrome.input_value(n);
                let target = url::from_input(&typed);
                self.navigate(&target, host);
            }
            Action::Submit if t == Target::Page => {
                self.dispatch(n, "change", key::ENTER, 0, host);
            }
            Action::Cancel if is_address => {
                let current = self.tabs[self.active].url().to_string();
                self.chrome.set_input_value(n, &current);
                self.keyboard = Target::Page;
                self.set_focus(Target::Chrome, None);
            }
            _ => {}
        }
    }

    fn scroll_key(&mut self, code: i32, mods: Modifiers) {
        let page = self.viewport_rect().h * 0.9;
        let dy = match code {
            key::DOWN => WHEEL_LINE,
            key::UP => -WHEEL_LINE,
            key::PAGE_DOWN => page,
            key::SPACE if mods.shift => -page,
            key::SPACE => page,
            key::PAGE_UP => -page,
            key::HOME => -f32::MAX / 4.0,
            key::END => f32::MAX / 4.0,
            _ => return,
        };
        // Scroll the viewport (a point outside any inner scroller).
        let doc = &mut self.tabs[self.active].doc;
        if doc.scroll_by(-1.0, -1.0, dy) || doc.scroll_by(0.0, 0.0, dy) {
            self.dirty = true;
        }
    }

    // ----- rendering -------------------------------------------------------

    fn caret_on(&self) -> bool {
        (((self.now - self.caret_epoch) / CARET_BLINK_MS) as i64) % 2 == 0
    }

    /// Restyles and lays out the chrome and the active page if needed.
    fn update_layout(&mut self, host: &mut dyn Host) {
        // Deliver built-in files (they may queue more, e.g. scripts).
        while !self.local.is_empty() {
            for (id, result) in std::mem::take(&mut self.local) {
                self.fetch_done(id, result, host);
            }
        }
        self.run_timers(host);
        if self.dirty {
            self.sync_chrome(host);
        }
        let (w, h) = self.logical_size();
        self.chrome.set_viewport(w, h);
        self.chrome.update(&self.fonts);
        let vp = self.viewport_rect();
        let doc = &mut self.tabs[self.active].doc;
        doc.set_viewport(vp.w, vp.h);
        doc.update(&self.fonts);
    }

    /// Whether a new frame should be drawn at time `now`.
    pub fn needs_redraw(&self, now: f64) -> bool {
        self.dirty
            || !self.local.is_empty()
            || self.next_timer().is_some_and(|t| t <= now)
            || self.chrome.needs_update()
            || self.tabs[self.active].doc.needs_update()
            || (self.focused_text_input().is_some() && self.caret_on() != self.caret_at(now))
    }

    fn caret_at(&self, now: f64) -> bool {
        (((now - self.caret_epoch) / CARET_BLINK_MS) as i64) % 2 == 0
    }

    /// When the platform should wake up for the next frame without input
    /// (caret blink), in milliseconds since the same epoch as `now`.
    pub fn next_wakeup(&self, now: f64) -> Option<f64> {
        if self.needs_redraw(now) {
            return Some(now);
        }
        let caret = self.focused_text_input().map(|_| {
            let phase = (now - self.caret_epoch) / CARET_BLINK_MS;
            self.caret_epoch + (phase.floor() + 1.0) * CARET_BLINK_MS
        });
        match (caret, self.next_timer()) {
            (Some(a), Some(b)) => Some(a.min(b)),
            (a, b) => a.or(b),
        }
    }

    /// Draws the whole browser and returns the result.
    pub fn render(&mut self, now: f64, host: &mut dyn Host) -> &Canvas {
        self.now = now;
        self.update_layout(host);
        let caret = self.caret_on();
        let focus_chrome = self.keyboard == Target::Chrome;
        self.chrome.caret_on = caret && focus_chrome;
        self.tabs[self.active].doc.caret_on = caret && !focus_chrome;
        self.chrome.scroll_inputs(&self.fonts);
        self.tabs[self.active].doc.scroll_inputs(&self.fonts);

        let vp = self.viewport_rect();
        let scale = self.scale;
        self.canvas.reset_clip();
        self.chrome
            .paint(&mut self.canvas, (0.0, 0.0), scale, &self.fonts);
        self.tabs[self.active]
            .doc
            .paint(&mut self.canvas, (vp.x, vp.y), scale, &self.fonts);
        // The status bubble is part of the chrome but sits above the page.
        if let Some(s) = self.chrome_el("status").and_then(|s| {
            let st = self.chrome.style(s)?;
            (st.display != crate::css::style::Display::None).then_some(s)
        }) && let Some(r) = self.chrome.content_box(s)
        {
            let pad = 12.0;
            let rect = crate::gfx::Rect::new(
                ((r.x - pad) * scale) as i32,
                ((r.y - pad) * scale) as i32,
                ((r.w + 2.0 * pad) * scale) as i32 + 1,
                ((r.h + 2.0 * pad) * scale) as i32 + 1,
            );
            self.canvas.set_clip(rect);
            self.chrome
                .paint(&mut self.canvas, (0.0, 0.0), scale, &self.fonts);
            self.canvas.reset_clip();
        }
        self.dirty = false;
        &self.canvas
    }
}

/// `src` of every `<script>` that is WebAssembly (JavaScript is not run).
fn wasm_scripts(doc: &Document) -> Vec<String> {
    let dom = &doc.dom;
    dom.descendants(crate::dom::Dom::ROOT)
        .into_iter()
        .filter(|&n| dom.tag(n) == Some("script"))
        .filter_map(|n| {
            let src = dom.attr(n, "src")?;
            let ty = dom.attr(n, "type").unwrap_or("").to_ascii_lowercase();
            (ty == "application/wasm" || ty == "wasm" || src.ends_with(".wasm"))
                .then(|| src.to_string())
        })
        .collect()
}

fn sanitize_scale(scale: f32) -> f32 {
    if scale.is_finite() && scale > 0.0 {
        scale
    } else {
        1.0
    }
}

fn escape(s: &str) -> String {
    s.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
}

fn error_page(url: &str, message: &str) -> String {
    format!(
        r#"<!doctype html><html><head><title>Error</title><style>
        body {{ font-family: sans-serif; margin: 0; padding: 48px; color: #202124; }}
        h1 {{ font-size: 24px; margin: 0 0 12px 0; }}
        .url {{ font-family: monospace; color: #5f6368; }}
        .msg {{ margin-top: 16px; padding: 12px 16px; background: #fce8e6; color: #a50e0e;
               border-radius: 6px; }}
        </style></head><body><h1>This page couldn&#39;t be loaded</h1>
        <div class="url">{}</div><div class="msg">{}</div></body></html>"#,
        escape(url),
        escape(message)
    )
}

/// Turns fetched bytes into HTML: plain text files are wrapped in `<pre>`.
fn as_html(url: &str, bytes: &[u8]) -> String {
    let text = String::from_utf8_lossy(bytes);
    let path = url
        .split(['?', '#'])
        .next()
        .unwrap_or(url)
        .to_ascii_lowercase();
    let plain = [
        ".txt", ".md", ".rs", ".toml", ".css", ".json", ".js", ".log",
    ];
    if plain.iter().any(|e| path.ends_with(e)) {
        format!("<pre style='white-space: pre-wrap'>{}</pre>", escape(&text))
    } else if path.ends_with(".wasm") || bytes.contains(&0) {
        error_page(url, "This is not an HTML document.")
    } else {
        text.into_owned()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::dom::Dom;

    #[derive(Default)]
    struct TestHost {
        fetched: Vec<(u32, String)>,
        title: String,
        clipboard: Option<String>,
    }

    impl Host for TestHost {
        fn fetch(&mut self, id: u32, url: &str) {
            self.fetched.push((id, url.to_string()));
        }
        fn clipboard_get(&mut self) -> Option<String> {
            self.clipboard.clone()
        }
        fn clipboard_set(&mut self, text: &str) {
            self.clipboard = Some(text.into());
        }
        fn set_cursor(&mut self, _: Cursor) {}
        fn set_title(&mut self, title: &str) {
            self.title = title.into();
        }
    }

    fn browser() -> (Browser, TestHost) {
        let mut h = TestHost::default();
        let mut b = Browser::new(1000, 700, 1.0);
        b.render(0.0, &mut h);
        (b, h)
    }

    fn type_text(b: &mut Browser, h: &mut TestHost, s: &str) {
        b.event(Event::Text(s.into()), 0.0, h);
    }

    fn press(b: &mut Browser, h: &mut TestHost, k: i32, mods: Modifiers) {
        b.event(Event::KeyDown { key: k, mods }, 0.0, h);
    }

    fn click_at(b: &mut Browser, h: &mut TestHost, r: RectF) {
        let (x, y) = ((r.x + r.w / 2.0) as i32, (r.y + r.h / 2.0) as i32);
        b.event(Event::MouseDown { x, y, button: 0 }, 0.0, h);
        b.event(Event::MouseUp { x, y, button: 0 }, 0.0, h);
        b.render(0.0, h);
    }

    fn chrome_rect(b: &Browser, id: &str) -> RectF {
        b.chrome.content_box(b.chrome_el(id).unwrap()).unwrap()
    }

    const CMD: Modifiers = Modifiers {
        shift: false,
        ctrl: false,
        alt: false,
        meta: true,
    };

    #[test]
    fn starts_on_home_with_chrome_laid_out() {
        let (b, h) = browser();
        assert_eq!(h.title, "rubrowser - rubrowser");
        let vp = b.viewport_rect();
        assert!(vp.y > 50.0 && vp.h > 500.0 && vp.w == 1000.0);
        let address = chrome_rect(&b, "address");
        assert!(address.w > 500.0, "address bar takes the remaining width");
        assert_eq!(
            b.chrome.input_value(b.chrome_el("address").unwrap()),
            "about:home"
        );
    }

    #[test]
    fn typing_an_address_fetches_it() {
        let (mut b, mut h) = browser();
        press(&mut b, &mut h, 'L' as i32, CMD);
        type_text(&mut b, &mut h, "example.com/index.html");
        press(&mut b, &mut h, key::ENTER, Modifiers::default());
        assert_eq!(
            h.fetched.last().unwrap().1,
            "https://example.com/index.html"
        );
        let id = h.fetched.last().unwrap().0;
        let page = b"<html><head><title>Example</title><link rel=stylesheet href=s.css></head><body><a href=next.html style=display:block>next</a></body></html>";
        b.fetch_done(id, Ok(page.to_vec()), &mut h);
        b.render(0.0, &mut h);
        assert_eq!(h.title, "Example - rubrowser");
        assert_eq!(h.fetched.last().unwrap().1, "https://example.com/s.css");
        // Click the link in the page.
        let doc = &b.tabs[b.active].doc;
        let a = doc.dom.find_tag(Dom::ROOT, "a").unwrap();
        let r = doc.content_box(a).unwrap();
        let vp = b.viewport_rect();
        click_at(
            &mut b,
            &mut h,
            RectF {
                x: r.x + vp.x,
                y: r.y + vp.y,
                ..r
            },
        );
        assert_eq!(h.fetched.last().unwrap().1, "https://example.com/next.html");
        // Back button returns to the first page.
        let back = chrome_rect(&b, "back");
        click_at(&mut b, &mut h, back);
        assert_eq!(b.tabs[b.active].url(), "https://example.com/index.html");
    }

    #[test]
    fn fetch_errors_show_an_error_page() {
        let (mut b, mut h) = browser();
        b.navigate("https://nowhere.invalid/", &mut h);
        let id = h.fetched.last().unwrap().0;
        b.fetch_done(id, Err("connection refused".into()), &mut h);
        b.render(0.0, &mut h);
        let text = b.tabs[b.active].doc.dom.text_content(Dom::ROOT);
        assert!(text.contains("connection refused"));
    }

    #[test]
    fn tabs_open_switch_and_close() {
        let (mut b, mut h) = browser();
        let plus = chrome_rect(&b, "new-tab");
        click_at(&mut b, &mut h, plus);
        assert_eq!(b.tabs.len(), 2);
        assert_eq!(b.active, 1);
        let strip = b.chrome_el("tabs").unwrap();
        let first = b.chrome.dom.children(strip)[0];
        let r = b.chrome.content_box(first).unwrap();
        click_at(&mut b, &mut h, r);
        assert_eq!(b.active, 0);
        press(&mut b, &mut h, 'W' as i32, CMD);
        b.render(0.0, &mut h);
        assert_eq!(b.tabs.len(), 1);
        assert_eq!(b.chrome.dom.children(strip).len(), 1);
    }

    #[test]
    fn new_tab_shows_its_own_address() {
        let (mut b, mut h) = browser();
        b.navigate("about:nothing", &mut h);
        b.render(0.0, &mut h);
        press(&mut b, &mut h, 'T' as i32, CMD);
        b.render(0.0, &mut h);
        let a = b.chrome_el("address").unwrap();
        assert_eq!(b.chrome.input_value(a), "about:home");
    }

    #[test]
    fn escape_restores_the_address() {
        let (mut b, mut h) = browser();
        press(&mut b, &mut h, 'L' as i32, CMD);
        type_text(&mut b, &mut h, "garbage");
        let a = b.chrome_el("address").unwrap();
        assert_eq!(b.chrome.input_value(a), "garbage");
        press(&mut b, &mut h, key::ESCAPE, Modifiers::default());
        assert_eq!(b.chrome.input_value(a), "about:home");
    }

    fn page_el(b: &Browser, id: &str) -> NodeId {
        b.tabs[b.active].doc.dom.element_by_id(id).unwrap()
    }

    /// Clicks the center of page element `id`.
    fn click_page(b: &mut Browser, h: &mut TestHost, id: &str) {
        let n = page_el(b, id);
        let r = b.tabs[b.active].doc.content_box(n).unwrap();
        let vp = b.viewport_rect();
        click_at(
            b,
            h,
            RectF {
                x: r.x + vp.x,
                y: r.y + vp.y,
                ..r
            },
        );
    }

    #[test]
    fn wasm_counter_page() {
        let (mut b, mut h) = browser();
        b.navigate("about:counter", &mut h);
        b.render(0.0, &mut h);
        assert_eq!(b.tabs[b.active].scripts.len(), 1, "counter.wasm loaded");
        let text = |b: &Browser| b.tabs[b.active].doc.dom.text_content(page_el(b, "count"));
        click_page(&mut b, &mut h, "inc");
        click_page(&mut b, &mut h, "inc");
        assert_eq!(text(&b), "2");
        click_page(&mut b, &mut h, "dec");
        click_page(&mut b, &mut h, "dec");
        click_page(&mut b, &mut h, "dec");
        assert_eq!(text(&b), "-1");
        let count = page_el(&b, "count");
        let style = b.tabs[b.active].doc.style(count).unwrap();
        assert_eq!(
            style.color.rgb(),
            0xc5221f,
            "inline style set by the script"
        );
    }

    #[test]
    fn wasm_todo_page() {
        let (mut b, mut h) = browser();
        b.navigate("about:todo", &mut h);
        b.render(0.0, &mut h);
        let items = |b: &Browser| {
            let doc = &b.tabs[b.active].doc;
            doc.dom.children(page_el(b, "list")).len()
        };
        assert_eq!(items(&b), 3, "initial items added by the script");
        click_page(&mut b, &mut h, "new");
        type_text(&mut b, &mut h, "Write docs");
        press(&mut b, &mut h, key::ENTER, Modifiers::default());
        b.render(0.0, &mut h);
        assert_eq!(items(&b), 4);
        let doc = &b.tabs[b.active].doc;
        assert!(
            doc.dom
                .text_content(page_el(&b, "list"))
                .contains("Write docs")
        );
        assert_eq!(doc.input_value(page_el(&b, "new")), "", "input cleared");
        assert!(doc.dom.text_content(page_el(&b, "left")).contains("4 of 4"));
    }

    #[test]
    fn wasm_timers_drive_the_stopwatch() {
        let (mut b, mut h) = browser();
        b.navigate("about:stopwatch", &mut h);
        b.render(0.0, &mut h);
        click_page(&mut b, &mut h, "toggle");
        let next = b.next_wakeup(0.0).unwrap();
        assert!(next > 0.0 && next <= 100.0);
        b.render(1250.0, &mut h);
        b.render(1300.0, &mut h);
        let time = b.tabs[b.active].doc.dom.text_content(page_el(&b, "time"));
        assert!(time.starts_with("00:01."), "{time}");
    }

    #[test]
    fn redraw_scheduling() {
        let (mut b, mut h) = browser();
        assert!(!b.needs_redraw(0.0));
        assert_eq!(b.next_wakeup(0.0), None);
        press(&mut b, &mut h, 'L' as i32, CMD);
        b.render(0.0, &mut h);
        assert_eq!(b.next_wakeup(0.0), Some(CARET_BLINK_MS), "caret blinks");
        assert!(b.needs_redraw(CARET_BLINK_MS + 1.0));
    }
}
