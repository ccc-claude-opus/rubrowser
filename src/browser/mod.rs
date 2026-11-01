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
use crate::script::{EventInfo, Limits, Script};
use crate::text::Fonts;
use crate::ui::textfield::Action;

const CHROME_HTML: &str = include_str!("chrome.html");
const HOME_HTML: &str = include_str!("pages/home.html");
const CARET_BLINK_MS: f64 = 530.0;
const WHEEL_LINE: f32 = 40.0;
const DOUBLE_CLICK_MS: f64 = 400.0;

/// Pages and files built into the browser, served as `about:NAME`.
fn builtin(name: &str) -> Option<&'static [u8]> {
    // about:form?x=1 is the page about:form (the query is for its scripts).
    let name = name.split(['?', '#']).next().unwrap_or(name);
    Some(match name {
        "home" => HOME_HTML.as_bytes(),
        "blank" => b"",
        "counter" => include_bytes!("pages/counter.html"),
        "counter.wasm" => include_bytes!("pages/counter.wasm"),
        "todo" => include_bytes!("pages/todo.html"),
        "todo.wasm" => include_bytes!("pages/todo.wasm"),
        "stopwatch" => include_bytes!("pages/stopwatch.html"),
        "stopwatch.wasm" => include_bytes!("pages/stopwatch.wasm"),
        "events" => include_bytes!("pages/events.html"),
        "events.wasm" => include_bytes!("pages/events.wasm"),
        "js-todo" => include_bytes!("pages/js-todo.html"),
        "calc" => include_bytes!("pages/calc.html"),
        "form" => include_bytes!("pages/form.html"),
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
    Page {
        tab: u64,
        serial: u64,
        url: String,
    },
    Stylesheet {
        tab: u64,
        serial: u64,
        /// The `<link>`, or `None` for an `@import`.
        link: Option<NodeId>,
        url: String,
    },
    Script {
        tab: u64,
        serial: u64,
        slot: usize,
    },
    Image {
        tab: u64,
        serial: u64,
        node: Option<NodeId>,
        src: String,
    },
}

/// Languages a `<script>` can be written in. Every one runs as wasm: Rust
/// (and other compiled languages) directly, JavaScript inside `js.wasm`.
#[derive(Clone, Debug, PartialEq, Eq)]
enum Lang {
    /// A compiled wasm module, run directly.
    Wasm,
    /// Source code (`<script>`) for the language runtime of this MIME type.
    Source(String),
    /// A page-provided language runtime (`<link rel=rubrowser-runtime>`)
    /// for this MIME type.
    Runtime(String),
}

/// The MIME type all JavaScript `<script>` types are normalized to.
const JS_MIME: &str = "text/javascript";

/// Language runtimes built into the browser, by MIME type.
fn builtin_runtime(mime: &str) -> Option<&'static [u8]> {
    match mime {
        JS_MIME => Some(JS_RUNTIME),
        _ => None,
    }
}

/// The script MIME type of a `<script type>` value (`None` for wasm).
fn script_mime(ty: &str) -> Option<String> {
    let ty = ty
        .split(';')
        .next()
        .unwrap_or("")
        .trim()
        .to_ascii_lowercase();
    match ty.as_str() {
        "application/wasm" | "wasm" => None,
        ""
        | "module"
        | "text/javascript"
        | "application/javascript"
        | "application/ecmascript"
        | "text/ecmascript"
        | "application/x-javascript" => Some(JS_MIME.to_string()),
        other => Some(other.to_string()),
    }
}

/// A `<script>` waiting to run (scripts run in document order).
struct PendingScript {
    lang: Lang,
    /// URL or `inline script` (for messages).
    name: String,
    body: Option<Result<Vec<u8>, String>>,
}

/// The JavaScript runtime module (boa compiled to wasm).
const JS_RUNTIME: &[u8] = include_bytes!("runtimes/js.wasm");

/// Extra fields of a dispatched event.
#[derive(Clone, Copy, Debug, Default)]
struct EventInit {
    key: i32,
    mods: i32,
    x: f32,
    y: f32,
    button: i32,
}

impl EventInit {
    fn key(key: i32, mods: i32) -> Self {
        EventInit {
            key,
            mods,
            ..Default::default()
        }
    }

    fn pointer(x: f32, y: f32, button: i32) -> Self {
        EventInit {
            x,
            y,
            button,
            ..Default::default()
        }
    }
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
    /// Scripts not yet run, in document order.
    pending: std::collections::VecDeque<PendingScript>,
    /// Index in `scripts` of the page's runtime for each script MIME type.
    runtimes: HashMap<String, usize>,
    /// Number of `<script>`s found when the page loaded.
    script_count: usize,
    /// Whether `DOMContentLoaded` / `load` have fired.
    loaded: bool,
}

impl Tab {
    /// Number of `<script>`s queued since the page loaded (run or not).
    fn scripts_seen(&self) -> usize {
        self.script_count
    }

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
    /// Whether web pages may run scripts (built-in pages always can).
    javascript: bool,
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
    /// Last page click (element, time), to detect double clicks.
    last_click: Option<(NodeId, f64)>,
}

impl Browser {
    /// `width` and `height` are physical pixels; `scale` is physical pixels
    /// per logical pixel (the display's DPI factor).
    pub fn new(width: usize, height: usize, scale: f32) -> Self {
        Self::with_fonts(width, height, scale, Fonts::new())
    }

    /// Like [`Browser::new`], with the given fonts (e.g.
    /// [`Fonts::with_system_fonts`]).
    pub fn with_fonts(width: usize, height: usize, scale: f32, fonts: Fonts) -> Self {
        let mut b = Browser {
            fonts,
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
            last_click: None,
            javascript: true,
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
            pending: Default::default(),
            runtimes: HashMap::new(),
            script_count: 0,
            loaded: false,
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
        if !self.same_document(self.active) {
            self.load(self.active, host);
        }
        self.keyboard = Target::Page;
        self.set_focus(Target::Chrome, None);
    }

    fn go(&mut self, delta: isize, host: &mut dyn Host) {
        let tab = &mut self.tabs[self.active];
        let next = tab.index as isize + delta;
        if next >= 0 && (next as usize) < tab.history.len() {
            tab.index = next as usize;
            if !self.same_document(self.active) {
                self.load(self.active, host);
            }
        }
    }

    /// If tab `i`'s current history entry is its loaded document with
    /// another fragment, scrolls to that fragment and returns true.
    fn same_document(&mut self, i: usize) -> bool {
        let tab = &mut self.tabs[i];
        let target = tab.history[tab.index].clone();
        let strip = |u: &str| u.split('#').next().unwrap_or("").to_string();
        if tab.loading || target.starts_with("about:") || strip(&target) != strip(&tab.doc.url) {
            return false;
        }
        if !target.contains('#') && !tab.doc.url.contains('#') {
            return false; // same URL without fragments: a reload
        }
        let fragment = target.split_once('#').map_or("", |(_, f)| f);
        tab.doc.scroll_to_fragment(fragment);
        tab.doc.url = target;
        self.dirty = true;
        true
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
        } else if url.starts_with("data:") {
            let result = url::decode_data_url(url).ok_or_else(|| "bad data: URL".to_string());
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
        tab.pending.clear();
        tab.runtimes.clear();
        let (id, serial) = (tab.id, tab.serial);
        let links = tab.doc.stylesheet_links();
        let base = tab.doc.base_url();
        let url = base.as_str();
        let web = !url.starts_with("about:");
        let mut scripts = if web && !self.javascript {
            Vec::new()
        } else {
            page_scripts(&tab.doc)
        };
        // Inline handlers (onclick="…") are JavaScript too; register them
        // first so scripts can rely on them.
        if let Some(code) = inline_handlers(&tab.doc).filter(|_| self.javascript || !web) {
            let at = scripts
                .iter()
                .take_while(|(l, _, _)| matches!(l, Lang::Runtime(_)))
                .count();
            scripts.insert(at, (Lang::Source(JS_MIME.to_string()), None, code));
        }
        tab.script_count = scripts.len();
        tab.loaded = false;
        for (link, href) in links {
            let target = url::resolve(url, &href);
            let req = Request::Stylesheet {
                tab: id,
                serial,
                link: Some(link),
                url: target.clone(),
            };
            self.fetch(req, &target, host);
        }
        // Scripts are fetched in parallel but run in document order.
        for (slot, (lang, src, inline)) in scripts.into_iter().enumerate() {
            let tab = &mut self.tabs[i];
            match src {
                Some(src) => {
                    let target = url::resolve(url, &src);
                    tab.pending.push_back(PendingScript {
                        lang,
                        name: target.clone(),
                        body: None,
                    });
                    let req = Request::Script {
                        tab: id,
                        serial,
                        slot,
                    };
                    self.fetch(req, &target, host);
                }
                None => tab.pending.push_back(PendingScript {
                    lang,
                    name: format!("{url} (inline script)"),
                    body: Some(Ok(inline.into_bytes())),
                }),
            }
        }
        self.run_scripts(i, host);
        self.dirty = true;
    }

    /// Runs pending scripts of tab `i` from the front while they are ready,
    /// then fires the load events once all have run.
    fn run_scripts(&mut self, i: usize, host: &mut dyn Host) {
        loop {
            let tab = &mut self.tabs[i];
            if tab.pending.is_empty() {
                if !tab.loaded {
                    tab.loaded = true;
                    self.fire_load(i, host);
                }
                return;
            }
            if !tab.pending.front().is_some_and(|p| p.body.is_some()) {
                return;
            }
            let p = tab.pending.pop_front().unwrap();
            // Later slots shift down as the front is consumed.
            let body = match p.body.unwrap() {
                Ok(b) => b,
                Err(e) => {
                    host.log(&format!("{}: {e}", p.name));
                    continue;
                }
            };
            match p.lang {
                Lang::Wasm => self.start_wasm(i, &body, &p.name, host),
                Lang::Runtime(mime) => self.start_runtime(i, &mime, &body, &p.name, host),
                Lang::Source(mime) => {
                    let source = String::from_utf8_lossy(&body).into_owned();
                    self.run_source(i, &mime, &source, &p.name, host);
                }
            }
            if i >= self.tabs.len() {
                return;
            }
        }
    }

    /// `DOMContentLoaded` at the document, then `load` at the body.
    fn fire_load(&mut self, i: usize, host: &mut dyn Host) {
        if i != self.active || self.tabs[i].scripts.is_empty() {
            return;
        }
        self.dispatch(
            crate::dom::Dom::ROOT,
            "domcontentloaded",
            EventInit::default(),
            host,
        );
        let body = self.tabs[self.active].doc.dom.body();
        self.dispatch(
            body.unwrap_or(crate::dom::Dom::ROOT),
            "load",
            EventInit::default(),
            host,
        );
    }

    fn wall_offset(&self, host: &mut dyn Host) -> f64 {
        host.wall_clock().map_or(0.0, |w| w - self.now)
    }

    fn start_wasm(&mut self, i: usize, bytes: &[u8], name: &str, host: &mut dyn Host) {
        let (now, offset) = (self.now, self.wall_offset(host));
        let t = &mut self.tabs[i];
        match Script::load_with(bytes, &mut t.doc, now, offset, Limits::SCRIPT) {
            Ok(script) => {
                t.scripts.push(Some(script));
                let si = t.scripts.len() - 1;
                self.after_script(i, si, host);
            }
            Err(e) => host.log(&format!("{name}: {e}")),
        }
        self.dirty = true;
    }

    /// Loads a language runtime module for `mime` scripts in tab `i`.
    fn start_runtime(
        &mut self,
        i: usize,
        mime: &str,
        bytes: &[u8],
        name: &str,
        host: &mut dyn Host,
    ) {
        let (now, offset) = (self.now, self.wall_offset(host));
        let t = &mut self.tabs[i];
        match Script::load_with(bytes, &mut t.doc, now, offset, Limits::RUNTIME) {
            Ok(rt) if rt.is_runtime() => {
                t.scripts.push(Some(rt));
                let si = t.scripts.len() - 1;
                t.runtimes.insert(mime.to_string(), si);
                self.after_script(i, si, host);
            }
            Ok(_) => host.log(&format!(
                "{name}: not a language runtime (needs rb_alloc and rb_eval)"
            )),
            Err(e) => host.log(&format!("{name}: {e}")),
        }
    }

    /// Runs `mime` source code in the page's runtime for that language,
    /// starting a built-in runtime if needed.
    fn run_source(&mut self, i: usize, mime: &str, source: &str, name: &str, host: &mut dyn Host) {
        if !self.tabs[i].runtimes.contains_key(mime) {
            match builtin_runtime(mime) {
                Some(wasm) => self.start_runtime(i, mime, wasm, &format!("{mime} runtime"), host),
                None => {
                    // Data blocks (JSON, templates) are not scripts at all.
                    let data = mime.contains("json")
                        || matches!(
                            mime,
                            "text/template" | "text/html" | "text/plain" | "text/x-template"
                        );
                    if !data {
                        host.log(&format!("{name}: no runtime for scripts of type {mime}"));
                    }
                    return;
                }
            }
        }
        let now = self.now;
        let Some(&si) = self.tabs[i].runtimes.get(mime) else {
            return;
        };
        let t = &mut self.tabs[i];
        let Some(Some(rt)) = t.scripts.get_mut(si) else {
            return;
        };
        match rt.eval(&mut t.doc, source, name, now) {
            Ok(()) => {
                self.after_script(i, si, host);
            }
            Err(e) => self.script_failed(i, si, &e, host),
        }
        self.dirty = true;
    }

    /// Enables or disables scripts on web pages (`--no-js`).
    pub fn set_javascript(&mut self, on: bool) {
        self.javascript = on;
    }

    /// Debug dump of the active page's layout (see
    /// [`Document::dump_layout`]).
    pub fn dump_layout(&self, selector: &str, depth: usize) -> String {
        self.tabs[self.active].doc.dump_layout(selector, depth)
    }

    /// Called by the platform when fetch `id` was redirected to `url`
    /// (before [`Browser::fetch_done`]), so the page uses its real address.
    pub fn fetch_redirected(&mut self, id: u32, final_url: &str) {
        if let Some(Request::Page { tab, url, .. }) = self.requests.get_mut(&id) {
            let tab = *tab;
            // HTTP never sends the fragment: keep the requested one.
            let mut final_url = final_url.to_string();
            if let Some((_, f)) = url.split_once('#')
                && !final_url.contains('#')
            {
                final_url = format!("{final_url}#{f}");
            }
            let final_url = final_url.as_str();
            *url = final_url.to_string();
            if let Some(i) = self.tab_index(tab) {
                let t = &mut self.tabs[i];
                t.history[t.index] = final_url.to_string();
            }
        }
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
            Request::Stylesheet {
                tab,
                serial,
                link,
                url,
            } => {
                let Some(i) = self
                    .tab_index(tab)
                    .filter(|&i| self.tabs[i].serial == serial)
                else {
                    return;
                };
                if let Ok(bytes) = result {
                    let css = String::from_utf8_lossy(&bytes).into_owned();
                    match link {
                        Some(link) => self.tabs[i].doc.add_stylesheet(link, css, &url),
                        None => self.tabs[i].doc.add_import(&url, css),
                    }
                    self.dirty = true;
                }
            }
            Request::Image {
                tab,
                serial,
                node,
                src,
            } => {
                if let Some(i) = self
                    .tab_index(tab)
                    .filter(|&i| self.tabs[i].serial == serial)
                {
                    self.tabs[i].doc.image_loaded(node, &src, result);
                    self.dirty = true;
                }
            }
            Request::Script { tab, serial, slot } => {
                let Some(i) = self
                    .tab_index(tab)
                    .filter(|&i| self.tabs[i].serial == serial)
                else {
                    return;
                };
                // Slots count from the start of the page's script list;
                // already-run scripts have left the queue.
                let total = self.tabs[i].scripts_seen();
                let tab = &mut self.tabs[i];
                let index = slot.checked_sub(total - tab.pending.len());
                if let Some(p) = index.and_then(|k| tab.pending.get_mut(k)) {
                    p.body = Some(result);
                }
                self.run_scripts(i, host);
            }
        }
    }

    // ----- scripts -----------------------------------------------------------

    /// Acts on what script `si` of tab `i` asked for; returns whether it
    /// prevented the default action.
    fn after_script(&mut self, i: usize, si: usize, host: &mut dyn Host) -> (bool, bool) {
        let now = self.now;
        let Some(Some(script)) = self.tabs[i].scripts.get_mut(si) else {
            return (false, false);
        };
        let out = script.take_output();
        for msg in &out.logs {
            host.log(msg);
        }
        let tab = &mut self.tabs[i];
        tab.timers
            .retain(|t| !(t.script == si && out.cleared.contains(&t.id)));
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
        } else if out.history != 0 && i == self.active {
            self.go(out.history as isize, host);
        } else if let Some(form) = out.submit.filter(|_| i == self.active) {
            // form.submit() skips the submit event, as in browsers.
            let doc = &self.tabs[i].doc;
            let data = doc.form_data(form, None);
            let action = doc
                .dom
                .attr(form, "action")
                .unwrap_or("")
                .trim()
                .to_string();
            let base = if action.is_empty() {
                doc.url.clone()
            } else {
                url::resolve(&doc.base_url(), &action)
            };
            let base = base.split(['?', '#']).next().unwrap_or("").to_string();
            let target = format!("{base}?{}", crate::document::urlencode(&data));
            self.navigate(&target, host);
        }
        (out.prevent_default, out.stop_propagation)
    }

    /// Reports a script failure and stops that script.
    fn script_failed(&mut self, i: usize, si: usize, err: &str, host: &mut dyn Host) {
        host.log(&format!("script error in {}: {err}", self.tabs[i].doc.url));
        self.tabs[i].scripts[si] = None;
        self.tabs[i].timers.retain(|t| t.script != si);
    }

    /// Dispatches event `kind` at `node` of the active page, bubbling up to
    /// the document. `init` supplies key and pointer fields. Returns whether
    /// a listener prevented the default action.
    fn dispatch(&mut self, node: NodeId, kind: &str, init: EventInit, host: &mut dyn Host) -> bool {
        let i = self.active;
        let mut event = EventInfo {
            kind: kind.to_string(),
            target: node,
            current: node,
            key_code: init.key,
            key: crate::abi::key_name(init.key, init.mods & crate::abi::modifier::SHIFT != 0),
            mods: init.mods,
            x: init.x as f64,
            y: init.y as f64,
            button: init.button,
        };
        let mut prevented = false;
        let mut n = Some(node);
        while let Some(id) = n {
            let calls: Vec<(usize, i32)> = self.tabs[i]
                .scripts
                .iter()
                .enumerate()
                .flat_map(|(si, s)| {
                    s.iter()
                        .flat_map(|s| s.listeners())
                        .filter(|l| l.node == id && l.event == kind)
                        .map(move |l| (si, l.id))
                        .collect::<Vec<_>>()
                })
                .collect();
            event.current = id;
            let mut stopped = false;
            for (si, lid) in calls {
                let now = self.now;
                let t = &mut self.tabs[i];
                let Some(Some(script)) = t.scripts.get_mut(si) else {
                    continue;
                };
                match script.dispatch(&mut t.doc, lid, &event, now) {
                    Ok(()) => {
                        let (p, s) = self.after_script(i, si, host);
                        prevented |= p;
                        stopped |= s;
                    }
                    Err(e) => self.script_failed(i, si, &e, host),
                }
                if self.active != i || self.tabs.len() <= i {
                    return prevented; // the script navigated away
                }
            }
            if stopped {
                break;
            }
            n = self.tabs[i].doc.dom.parent(id);
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
                        self.dispatch(n, "input", EventInit::default(), host);
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
            let (lx, ly) = if target == Target::Page {
                (x - vp.x, y - vp.y)
            } else {
                (x, y)
            };
            let fonts = &self.fonts;
            match target {
                Target::Chrome => self.chrome.input_pointer(n, lx, ly, true, false, fonts),
                Target::Page => self.tabs[self.active]
                    .doc
                    .input_pointer(n, lx, ly, true, false, fonts),
            }
            self.dirty = true;
            return;
        }
        let (t, lx, ly) = self.target_at(x, y);
        if t == Target::Page {
            let doc = &mut self.tabs[self.active].doc;
            let hover = doc.popup_hit(lx, ly, &self.fonts).flatten();
            if doc.popup_hover != hover {
                doc.popup_hover = hover;
                self.dirty = true;
            }
        }
        let hit = self.doc(t).hit_test(lx, ly);
        let other = if t == Target::Chrome {
            Target::Page
        } else {
            Target::Chrome
        };
        let old_page_hover = self.doc(Target::Page).state().hover;
        self.set_state(other, |s| s.hover = None);
        self.set_state(t, |s| s.hover = hit.element);
        let new_page_hover = self.doc(Target::Page).state().hover;
        if old_page_hover != new_page_hover {
            let at = EventInit::pointer(lx, ly, 0);
            if let Some(o) = old_page_hover {
                self.dispatch(o, "mouseout", at, host);
            }
            if let Some(n) = new_page_hover {
                self.dispatch(n, "mouseover", at, host);
            }
        }
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
            .map(|href| url::resolve(&doc.base_url(), &href));
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

    /// The pointer position in page coordinates, for page events.
    fn page_pointer(&self) -> EventInit {
        let vp = self.viewport_rect();
        EventInit::pointer(self.pointer.0 - vp.x, self.pointer.1 - vp.y, 0)
    }

    /// Moves page focus to `node`, firing `blur` and `focus`.
    fn focus_page(&mut self, node: Option<NodeId>, host: &mut dyn Host) {
        let old = self.doc(Target::Page).state().focus;
        self.set_focus(Target::Page, node);
        if old != node {
            if let Some(o) = old {
                self.dispatch(o, "blur", EventInit::default(), host);
            }
            if let Some(n) = node {
                self.dispatch(n, "focus", EventInit::default(), host);
            }
        }
    }

    fn mouse_down(&mut self, x: f32, y: f32, host: &mut dyn Host) {
        let (t, lx, ly) = self.target_at(x, y);
        // An open <select> list takes the click first.
        if self.tabs[self.active].doc.forms.open_select.is_some() {
            let hit = (t == Target::Page)
                .then(|| self.tabs[self.active].doc.popup_hit(lx, ly, &self.fonts))
                .flatten();
            match hit {
                Some(Some(i)) => {
                    self.choose_option(i, host);
                    return;
                }
                Some(None) => return,
                None => {
                    self.tabs[self.active].doc.forms.open_select = None;
                    self.dirty = true;
                }
            }
        }
        let hit = self.doc(t).hit_test(lx, ly);
        if t == Target::Page
            && let Some(e) = hit.element
        {
            self.dispatch(e, "mousedown", EventInit::pointer(lx, ly, 0), host);
        }
        self.set_state(t, |s| s.active = hit.element);
        let focus = hit.element.and_then(|e| self.doc(t).focusable_ancestor(e));
        let was_focused = self.doc(t).state().focus;
        self.keyboard = t;
        let other = if t == Target::Chrome {
            Target::Page
        } else {
            Target::Chrome
        };
        if other == Target::Page {
            self.focus_page(None, host);
        } else {
            self.set_focus(other, None);
        }
        if t == Target::Page {
            self.focus_page(focus, host);
        } else {
            self.set_focus(t, focus);
        }
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
                    Target::Chrome => self.chrome.input_pointer(f, lx, ly, false, shift, fonts),
                    Target::Page => self.tabs[self.active]
                        .doc
                        .input_pointer(f, lx, ly, false, shift, fonts),
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
        let hit = self.doc(t).hit_test(lx, ly);
        if t == Target::Page
            && let Some(e) = hit.element
        {
            self.dispatch(e, "mouseup", EventInit::pointer(lx, ly, 0), host);
        }
        if t != press.target || press.input.is_some() {
            return;
        }
        // A click: released on the pressed element or inside it.
        if let (Some(p), Some(h)) = (press.element, hit.element)
            && self.doc(t).dom.is_inclusive_ancestor(p, h)
        {
            let double = t == Target::Page
                && self
                    .last_click
                    .is_some_and(|(n, at)| n == h && self.now - at < DOUBLE_CLICK_MS);
            self.last_click = (t == Target::Page).then_some((h, self.now));
            self.click(t, h, host);
            if double && self.tabs[self.active].doc.dom.get(h).is_some() {
                self.dispatch(h, "dblclick", EventInit::pointer(lx, ly, 0), host);
                self.last_click = None;
            }
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
                if self.dispatch(node, "click", self.page_pointer(), host) {
                    return;
                }
                self.activate(node, host);
            }
        }
    }

    /// The default action of a click on page element `node`: follow a
    /// link, toggle a checkbox, open a select, submit a form, …
    fn activate(&mut self, node: NodeId, host: &mut dyn Host) {
        use crate::document::{Control, control, label_target};
        let doc = &self.tabs[self.active].doc;
        // The innermost control or label around the clicked node.
        let mut n = Some(node);
        while let Some(id) = n {
            if doc.dom.attr(id, "disabled").is_some() && control(&doc.dom, id).is_some() {
                return;
            }
            if let Some(kind) = control(&doc.dom, id) {
                return self.activate_control(id, kind, host);
            }
            if doc.dom.tag(id) == Some("label") {
                if let Some(target) = label_target(&doc.dom, id) {
                    let kind = control(&doc.dom, target);
                    if kind.is_some_and(|k| k.is_text()) || kind == Some(Control::Select) {
                        self.focus_page(Some(target), host);
                        self.dirty = true;
                        return;
                    }
                    // Clicking a label clicks its control.
                    if !self.dispatch(target, "click", self.page_pointer(), host) {
                        self.activate(target, host);
                    }
                }
                return;
            }
            if doc.dom.tag(id) == Some("summary")
                && let Some(details) = doc
                    .dom
                    .parent(id)
                    .filter(|&p| doc.dom.tag(p) == Some("details"))
            {
                let open = doc.dom.attr(details, "open").is_some();
                let doc = &mut self.tabs[self.active].doc;
                if let Some(e) = doc.dom_mut().element_mut(details) {
                    if open {
                        e.remove_attr("open");
                    } else {
                        e.set_attr("open", "");
                    }
                }
                self.dirty = true;
                self.dispatch(details, "toggle", EventInit::default(), host);
                return;
            }
            if let Some(href) = doc.link(id) {
                let target = url::resolve(&doc.base_url(), &href);
                return self.navigate(&target, host);
            }
            n = doc.dom.parent(id);
        }
    }

    fn activate_control(
        &mut self,
        node: NodeId,
        kind: crate::document::Control,
        host: &mut dyn Host,
    ) {
        use crate::document::{Control, form_of};
        let doc = &mut self.tabs[self.active].doc;
        match kind {
            Control::Checkbox => {
                let on = !doc.forms.checked(&doc.dom, node);
                doc.forms.set_checked(&doc.dom, node, on);
            }
            Control::Radio => {
                if doc.forms.checked(&doc.dom, node) {
                    return;
                }
                doc.forms.set_checked(&doc.dom, node, true);
            }
            Control::Select => {
                let open = doc.forms.open_select == Some(node);
                doc.forms.open_select = (!open).then_some(node);
                doc.popup_hover = None;
                self.dirty = true;
                return;
            }
            Control::Submit => {
                if let Some(form) = form_of(&doc.dom, node) {
                    self.submit_form(form, Some(node), host);
                }
                return;
            }
            Control::Reset => {
                if let Some(form) = form_of(&doc.dom, node) {
                    doc.reset_form(form);
                    self.dirty = true;
                }
                return;
            }
            _ => return,
        }
        doc.dom_changed();
        self.dirty = true;
        self.dispatch(node, "input", EventInit::default(), host);
        self.dispatch(node, "change", EventInit::default(), host);
    }

    /// Picks option `i` of the open `<select>`.
    fn choose_option(&mut self, i: usize, host: &mut dyn Host) {
        let doc = &mut self.tabs[self.active].doc;
        let Some(select) = doc.forms.open_select.take() else {
            return;
        };
        let changed = doc.forms.selected_index(&doc.dom, select) != Some(i);
        doc.forms.set_selected(select, i);
        doc.dom_changed();
        self.dirty = true;
        self.focus_page(Some(select), host);
        if changed {
            self.dispatch(select, "input", EventInit::default(), host);
            self.dispatch(select, "change", EventInit::default(), host);
        }
    }

    /// Submits `form` with a GET request (after its `submit` event).
    fn submit_form(&mut self, form: NodeId, submitter: Option<NodeId>, host: &mut dyn Host) {
        if self.dispatch(form, "submit", EventInit::default(), host) {
            return;
        }
        let doc = &self.tabs[self.active].doc;
        let data = doc.form_data(form, submitter);
        let action = doc
            .dom
            .attr(form, "action")
            .unwrap_or("")
            .trim()
            .to_string();
        let method = doc
            .dom
            .attr(form, "method")
            .unwrap_or("get")
            .to_ascii_lowercase();
        if method == "post" {
            host.log("form method=post is not supported; submitting with GET");
        }
        let base = if action.is_empty() {
            doc.url.clone()
        } else {
            url::resolve(&doc.base_url(), &action)
        };
        let base = base.split(['?', '#']).next().unwrap_or("").to_string();
        let target = format!("{base}?{}", crate::document::urlencode(&data));
        self.navigate(&target, host);
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
            if t == Target::Page {
                self.focus_page(next, host);
            } else {
                self.set_focus(t, next);
            }
            if let Some(n) = next.filter(|&n| self.doc(t).is_text_input(n)) {
                self.doc_mut(t).input(n).select_all();
            }
            return;
        }
        if t == Target::Page {
            let target = focus
                .or_else(|| self.doc(t).dom.body())
                .unwrap_or(crate::dom::Dom::ROOT);
            if self.dispatch(target, "keydown", EventInit::key(code, mods.bits()), host) {
                return;
            }
        }
        if t == Target::Page
            && let Some(n) = focus
            && crate::document::control(&self.doc(t).dom, n)
                == Some(crate::document::Control::Select)
            && self.select_key(n, code, host)
        {
            return;
        }
        if let Some(n) = focus {
            if self.doc(t).is_text_input(n) {
                let before = self.doc(t).input_value(n);
                let action = self.doc_mut(t).input(n).key(code, mods, host);
                if t == Target::Page && self.doc(t).input_value(n) != before {
                    self.dispatch(n, "input", EventInit::default(), host);
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

    /// Keyboard on a focused `<select>`; returns whether the key was used.
    fn select_key(&mut self, select: NodeId, code: i32, host: &mut dyn Host) -> bool {
        let doc = &mut self.tabs[self.active].doc;
        let count = crate::document::options(&doc.dom, select).len();
        let current = doc.forms.selected_index(&doc.dom, select).unwrap_or(0);
        let open = doc.forms.open_select == Some(select);
        let step = match code {
            key::UP => -1,
            key::DOWN => 1,
            key::ENTER | key::SPACE => {
                if open {
                    let i = doc.popup_hover.unwrap_or(current);
                    doc.forms.open_select = Some(select);
                    self.choose_option(i, host);
                } else {
                    doc.forms.open_select = Some(select);
                    doc.popup_hover = Some(current);
                }
                self.dirty = true;
                return true;
            }
            key::ESCAPE if open => {
                doc.forms.open_select = None;
                self.dirty = true;
                return true;
            }
            _ => return false,
        };
        if count == 0 {
            return true;
        }
        if open {
            let h = doc.popup_hover.unwrap_or(current) as isize + step;
            doc.popup_hover = Some(h.clamp(0, count as isize - 1) as usize);
            self.dirty = true;
        } else {
            let next = (current as isize + step).clamp(0, count as isize - 1) as usize;
            if next != current {
                doc.forms.open_select = Some(select);
                self.choose_option(next, host);
            }
        }
        true
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
                self.dispatch(n, "change", EventInit::key(key::ENTER, 0), host);
                // Enter in a text field submits its form.
                let doc = &self.tabs[self.active].doc;
                if doc.dom.tag(n) == Some("input")
                    && let Some(form) = crate::document::form_of(&doc.dom, n)
                {
                    self.submit_form(form, None, host);
                }
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
        self.request_images(host);
        let doc = &mut self.tabs[self.active].doc;
        doc.set_viewport(vp.w, vp.h);
        doc.update(&self.fonts);
    }

    /// Starts loading images of the active page that have a new `src`
    /// (from the HTML or set later by scripts).
    fn request_images(&mut self, host: &mut dyn Host) {
        let tab = &mut self.tabs[self.active];
        let (id, serial) = (tab.id, tab.serial);
        let base = tab.doc.base_url();
        for url in tab.doc.import_requests() {
            let req = Request::Stylesheet {
                tab: id,
                serial,
                link: None,
                url: url.clone(),
            };
            self.fetch(req, &url, host);
        }
        let tab = &mut self.tabs[self.active];
        for (node, src) in tab.doc.image_requests() {
            let req = Request::Image {
                tab: id,
                serial,
                node,
                src: src.clone(),
            };
            self.fetch(req, &url::resolve(&base, &src), host);
        }
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

/// Every script the browser can run, in document order: language
/// runtimes declared with `<link rel="rubrowser-runtime" type href>` first,
/// then `<script>`s. Each entry is (language, `src`, inline text).
fn page_scripts(doc: &Document) -> Vec<(Lang, Option<String>, String)> {
    let dom = &doc.dom;
    let all = dom.descendants(crate::dom::Dom::ROOT);
    let runtimes = all.iter().filter_map(|&n| {
        let rel = dom.attr(n, "rel")?;
        if dom.tag(n) != Some("link") || !rel.split_whitespace().any(|r| r == "rubrowser-runtime") {
            return None;
        }
        let mime = script_mime(dom.attr(n, "type")?)?;
        Some((
            Lang::Runtime(mime),
            Some(dom.attr(n, "href")?.to_string()),
            String::new(),
        ))
    });
    let scripts = all.iter().filter_map(|&n| {
        if dom.tag(n) != Some("script") {
            return None;
        }
        let src = dom.attr(n, "src").map(str::to_string);
        let ty = dom.attr(n, "type").unwrap_or("");
        let wasm_src = src.as_deref().is_some_and(|s| s.ends_with(".wasm"));
        let lang = match script_mime(ty) {
            Some(m) if m == JS_MIME && wasm_src => Lang::Wasm,
            Some(m) => Lang::Source(m),
            None => Lang::Wasm,
        };
        let inline = dom.text_content(n);
        if src.is_none() && (lang == Lang::Wasm || inline.trim().is_empty()) {
            return None;
        }
        Some((lang, src, inline))
    });
    runtimes.chain(scripts).collect()
}

/// JavaScript registering every `on…="code"` attribute, if there are any.
fn inline_handlers(doc: &Document) -> Option<String> {
    let dom = &doc.dom;
    let mut js = String::new();
    for n in dom.descendants(crate::dom::Dom::ROOT) {
        let Some(e) = dom.element(n) else { continue };
        for (name, code) in &e.attrs {
            if let Some(event) = name.strip_prefix("on").filter(|e| !e.is_empty()) {
                js.push_str(&format!(
                    "__rb_inline({n}, {}, {});\n",
                    js_string(event),
                    js_string(code)
                ));
            }
        }
    }
    (!js.is_empty()).then_some(js)
}

/// A JavaScript string literal for `s`.
fn js_string(s: &str) -> String {
    let mut out = String::from("\"");
    for c in s.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\u{2028}' => out.push_str("\\u2028"),
            '\u{2029}' => out.push_str("\\u2029"),
            c if (c as u32) < 0x20 => out.push_str(&format!("\\u{:04x}", c as u32)),
            c => out.push(c),
        }
    }
    out.push('"');
    out
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
    fn fragment_links_scroll_without_reloading() {
        let (mut b, mut h) = browser();
        b.navigate("https://w.org/wiki/A", &mut h);
        let id = h.fetched.last().unwrap().0;
        let page = br##"<body style="margin:0"><a id=go href="#sec" style="display:block">go</a>
            <div style="height:3000px"></div><h2 id=sec>Section</h2>
            <div style="height:3000px"></div></body>"##;
        b.fetch_done(id, Ok(page.to_vec()), &mut h);
        b.render(0.0, &mut h);
        let fetches = h.fetched.len();
        click_page(&mut b, &mut h, "go");
        assert_eq!(h.fetched.len(), fetches, "no reload");
        assert_eq!(b.tabs[b.active].url(), "https://w.org/wiki/A#sec");
        let y = b.tabs[b.active].doc.scroll_y();
        assert!(y > 2900.0 && y < 3100.0, "scrolled to #sec: {y}");
        b.go(-1, &mut h);
        b.render(0.0, &mut h);
        assert_eq!(h.fetched.len(), fetches, "back stays in the document");
        assert_eq!(b.tabs[b.active].doc.scroll_y(), 0.0);
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
    fn dom_events_page() {
        let (mut b, mut h) = browser();
        b.navigate("about:events", &mut h);
        b.render(0.0, &mut h);
        let log = |b: &Browser| b.tabs[b.active].doc.dom.text_content(page_el(b, "log"));
        assert!(log(&b).contains("ready at about:events"), "{}", log(&b));

        // Hover a card: mouseover adds a class, mouseout removes it.
        let card = b.tabs[b.active].doc.dom.find_tag(Dom::ROOT, "b").unwrap();
        let card = b.tabs[b.active].doc.dom.parent(card).unwrap();
        let r = b.tabs[b.active].doc.content_box(card).unwrap();
        let vp = b.viewport_rect();
        let (x, y) = ((vp.x + r.x + 2.0) as i32, (vp.y + r.y + 2.0) as i32);
        b.event(Event::MouseMove { x, y }, 0.0, &mut h);
        assert!(
            b.tabs[b.active]
                .doc
                .dom
                .element(card)
                .unwrap()
                .has_class("hot")
        );
        b.event(
            Event::MouseMove {
                x: 1,
                y: (vp.y + vp.h - 2.0) as i32,
            },
            0.0,
            &mut h,
        );
        assert!(
            !b.tabs[b.active]
                .doc
                .dom
                .element(card)
                .unwrap()
                .has_class("hot")
        );

        // stopPropagation keeps the inner click from the outer listener.
        click_page(&mut b, &mut h, "inner");
        assert!(log(&b).contains("inner click (stopped)"));
        assert!(!log(&b).contains("outer got click"));
        // Click the outer box's own text, left of the inner span.
        let outer = page_el(&b, "outer");
        let r = b.tabs[b.active].doc.content_box(outer).unwrap();
        click_at(
            &mut b,
            &mut h,
            RectF {
                x: vp.x + r.x,
                y: vp.y + r.y,
                w: 4.0,
                h: r.h,
            },
        );
        assert!(
            log(&b).contains("outer got click from <div>"),
            "{}",
            log(&b)
        );

        // innerHTML builds a list.
        click_page(&mut b, &mut h, "fill");
        assert!(log(&b).contains("3 items, first = one"), "{}", log(&b));

        // Double click.
        click_page(&mut b, &mut h, "dbl");
        click_page(&mut b, &mut h, "dbl");
        assert!(log(&b).contains("double click"));

        // Input events and keydown on the document.
        click_page(&mut b, &mut h, "name");
        type_text(&mut b, &mut h, "Ann");
        let hello = b.tabs[b.active].doc.dom.text_content(page_el(&b, "hello"));
        assert_eq!(hello, "Hello, Ann!");
        let body = page_el(&b, "log");
        let r = b.tabs[b.active].doc.content_box(body).unwrap();
        click_at(
            &mut b,
            &mut h,
            RectF {
                x: r.x + vp.x,
                y: r.y + vp.y,
                ..r
            },
        );
        press(&mut b, &mut h, 'Q' as i32, Modifiers::default());
        assert!(log(&b).contains("key \"q\""), "{}", log(&b));

        // The interval clears itself after five ticks.
        for t in 1..=10 {
            b.render(t as f64 * 210.0, &mut h);
        }
        let ticks = b.tabs[b.active].doc.dom.text_content(page_el(&b, "ticks"));
        assert_eq!(ticks, "5");
        assert!(b.tabs[b.active].timers.is_empty());
    }

    #[derive(Default)]
    struct LogHost {
        inner: TestHost,
        logs: Vec<String>,
    }

    impl Host for LogHost {
        fn fetch(&mut self, id: u32, url: &str) {
            self.inner.fetch(id, url)
        }
        fn clipboard_get(&mut self) -> Option<String> {
            None
        }
        fn clipboard_set(&mut self, _: &str) {}
        fn set_cursor(&mut self, _: Cursor) {}
        fn set_title(&mut self, t: &str) {
            self.inner.set_title(t)
        }
        fn log(&mut self, m: &str) {
            self.logs.push(m.to_string());
        }
        fn wall_clock(&mut self) -> Option<f64> {
            Some(1_700_000_000_000.0)
        }
    }

    #[test]
    fn embedded_js_runtime_is_up_to_date() {
        // js.wasm embeds prelude.js verbatim; if this fails, run
        // scripts/build.sh to rebuild the runtime.
        let prelude = include_bytes!("../lang/prelude.js");
        assert!(
            JS_RUNTIME.windows(prelude.len()).any(|w| w == prelude),
            "src/browser/runtimes/js.wasm is stale: run scripts/build.sh"
        );
    }

    #[test]
    fn javascript_runs_in_the_wasm_runtime() {
        let mut h = LogHost::default();
        let mut b = Browser::new(1000, 700, 1.0);
        b.render(0.0, &mut h);
        b.navigate("https://js.test/index.html", &mut h);
        let id = h.inner.fetched.last().unwrap().0;
        let page = r#"<title>JS</title>
            <p id=out>old</p><button id=btn>Go</button><ul id=list></ul>
            <script>
              const out = document.getElementById('out');
              out.textContent = 'from js ' + [1, 2, 3].map(x => x * 2).join(',');
              out.classList.add('done');
              out.style.color = 'red';
              console.log('sum', 2 + 3, { a: 1 });
              let clicks = 0;
              document.querySelector('#btn').addEventListener('click', (e) => {
                clicks++;
                document.title = 'clicked ' + clicks + ' ' + e.target.tagName;
              });
              setTimeout(() => console.log('timer', Date.now() > 1600000000000), 50);
            </script>
            <script src="more.js"></script>
            <script>console.log('third runs after more.js', typeof fromMore);</script>"#;
        b.fetch_done(id, Ok(page.as_bytes().to_vec()), &mut h);
        // more.js arrives later; the third script must wait for it.
        let more = h.inner.fetched.last().unwrap();
        assert_eq!(more.1, "https://js.test/more.js");
        assert!(!h.logs.iter().any(|l| l.starts_with("third")));
        let more_id = more.0;
        let more_src = "var fromMore = 1; document.getElementById('list').innerHTML = '<li>a</li><li>b</li>'; nope();";
        b.fetch_done(more_id, Ok(more_src.as_bytes().to_vec()), &mut h);
        b.render(0.0, &mut h);

        let doc = &b.tabs[b.active].doc;
        let out = doc.dom.element_by_id("out").unwrap();
        assert_eq!(doc.dom.text_content(out), "from js 2,4,6");
        assert_eq!(doc.dom.attr(out, "class"), Some("done"));
        assert_eq!(doc.style(out).unwrap().color.rgb(), 0xff0000);
        let list = doc.dom.element_by_id("list").unwrap();
        assert_eq!(doc.dom.children(list).len(), 2);
        assert!(
            h.logs.contains(&"sum 5 {\"a\":1}".to_string()),
            "{:?}",
            h.logs
        );
        assert!(
            h.logs
                .iter()
                .any(|l| l.contains("nope") && l.contains("Uncaught")),
            "{:?}",
            h.logs
        );
        assert!(
            h.logs
                .contains(&"third runs after more.js number".to_string()),
            "{:?}",
            h.logs
        );

        click_page_h(&mut b, &mut h, "btn");
        click_page_h(&mut b, &mut h, "btn");
        assert_eq!(
            b.tabs[b.active].doc.title().as_deref(),
            Some("clicked 2 BUTTON")
        );

        b.render(100.0, &mut h);
        assert!(h.logs.contains(&"timer true".to_string()), "{:?}", h.logs);
    }

    #[test]
    fn javascript_calculator_with_inline_handlers() {
        let mut h = LogHost::default();
        let mut b = Browser::new(1000, 700, 1.0);
        b.navigate("about:calc", &mut h);
        b.render(0.0, &mut h);
        let screen = |b: &Browser| b.tabs[b.active].doc.dom.text_content(page_el(b, "screen"));
        assert_eq!(screen(&b), "0", "body onload ran: {:?}", h.logs);
        // Click buttons by their label.
        let mut click_label = |b: &mut Browser, label: &str| {
            let doc = &b.tabs[b.active].doc;
            let btn = doc
                .dom
                .descendants(Dom::ROOT)
                .into_iter()
                .find(|&n| doc.dom.tag(n) == Some("button") && doc.dom.text_content(n) == label)
                .unwrap();
            let r = doc.content_box(btn).unwrap();
            let vp = b.viewport_rect();
            let (x, y) = (
                (vp.x + r.x + r.w / 2.0) as i32,
                (vp.y + r.y + r.h / 2.0) as i32,
            );
            b.event(Event::MouseDown { x, y, button: 0 }, 0.0, &mut h);
            b.event(Event::MouseUp { x, y, button: 0 }, 0.0, &mut h);
        };
        for k in ["1", "2", "+", "3", "×", "4"] {
            click_label(&mut b, k);
        }
        assert_eq!(screen(&b), "12+3*4");
        click_label(&mut b, "=");
        assert_eq!(screen(&b), "24");
        // Keyboard: the document's keydown listener sees symbol keys.
        let mut h2 = LogHost::default();
        for k in ['-', '4'] {
            let code = if k.is_ascii_digit() {
                k as i32
            } else {
                key::CHAR_BASE + k as i32
            };
            b.event(
                Event::KeyDown {
                    key: code,
                    mods: Modifiers::default(),
                },
                0.0,
                &mut h2,
            );
        }
        b.event(
            Event::KeyDown {
                key: key::ENTER,
                mods: Modifiers::default(),
            },
            0.0,
            &mut h2,
        );
        assert_eq!(screen(&b), "20");
        assert!(
            h.logs.is_empty() && h2.logs.is_empty(),
            "{:?} {:?}",
            h.logs,
            h2.logs
        );
    }

    #[test]
    fn javascript_todo_page() {
        let mut h = LogHost::default();
        let mut b = Browser::new(1000, 700, 1.0);
        b.navigate("about:js-todo", &mut h);
        b.render(0.0, &mut h);
        assert!(
            h.logs.contains(&"JS todo ready with 3 items".to_string()),
            "{:?}",
            h.logs
        );
        let items = |b: &Browser| {
            let doc = &b.tabs[b.active].doc;
            doc.dom.children(page_el(b, "list")).len()
        };
        assert_eq!(items(&b), 3);
        click_page_h(&mut b, &mut h, "new");
        b.event(Event::Text("Ship <it>".into()), 0.0, &mut h);
        b.event(
            Event::KeyDown {
                key: key::ENTER,
                mods: Modifiers::default(),
            },
            0.0,
            &mut h,
        );
        b.render(0.0, &mut h);
        assert_eq!(items(&b), 4);
        let list = b.tabs[b.active].doc.dom.text_content(page_el(&b, "list"));
        assert!(list.contains("Ship <it>"), "escaped text: {list}");
        // Delegated click on the first item's text marks it done.
        let doc = &b.tabs[b.active].doc;
        let first = doc.dom.children(page_el(&b, "list"))[0];
        let span = doc.dom.children(first)[0];
        let r = doc.content_box(span).unwrap();
        let vp = b.viewport_rect();
        click_at(
            &mut b,
            &mut h.inner,
            RectF {
                x: vp.x + r.x,
                y: vp.y + r.y,
                ..r
            },
        );
        let left = b.tabs[b.active].doc.dom.text_content(page_el(&b, "left"));
        assert_eq!(left, "3 of 4 left");
        assert!(
            !h.logs.iter().any(|l| l.contains("Uncaught")),
            "{:?}",
            h.logs
        );
    }

    #[test]
    fn forms_controls_and_submission() {
        let mut h = LogHost::default();
        let mut b = Browser::new(1000, 800, 1.0);
        b.navigate("about:form", &mut h);
        b.render(0.0, &mut h);
        let el = |b: &Browser, id: &str| page_el(b, id);
        fn doc(b: &Browser) -> &Document {
            &b.tabs[b.active].doc
        }

        // Submitting without a name is cancelled by the page's script.
        click_page_h(&mut b, &mut h, "send");
        assert_eq!(b.tabs[b.active].url(), "about:form");
        assert!(
            h.logs.contains(&"name is required".to_string()),
            "{:?}",
            h.logs
        );

        click_page_h(&mut b, &mut h, "name");
        b.event(Event::Text("Ada".into()), 0.0, &mut h);
        // The select: open with a click, pick "Team" from the list.
        click_page_h(&mut b, &mut h, "plan");
        assert!(doc(&b).forms.open_select.is_some());
        let (popup, items) = doc(&b).select_popup(&b.fonts).unwrap();
        assert_eq!(items, vec!["Free", "Pro", "Team"]);
        let vp = b.viewport_rect();
        let row = popup.h / 3.0;
        let (x, y) = (
            (vp.x + popup.x + 10.0) as i32,
            (vp.y + popup.y + row * 2.5) as i32,
        );
        b.event(Event::MouseDown { x, y, button: 0 }, 0.0, &mut h);
        b.event(Event::MouseUp { x, y, button: 0 }, 0.0, &mut h);
        assert_eq!(doc(&b).input_value(el(&b, "plan")), "team");
        assert!(doc(&b).forms.open_select.is_none());
        // Keyboard on the focused select.
        b.event(
            Event::KeyDown {
                key: key::UP,
                mods: Modifiers::default(),
            },
            0.0,
            &mut h,
        );
        assert_eq!(doc(&b).input_value(el(&b, "plan")), "pro");

        // The checkbox via its label text, a radio button, the textarea.
        let label = doc(&b).dom.parent(el(&b, "news")).unwrap();
        let r = doc(&b).content_box(label).unwrap();
        click_at(
            &mut b,
            &mut h.inner,
            RectF {
                x: vp.x + r.x + r.w - 10.0,
                y: vp.y + r.y,
                w: 4.0,
                h: r.h,
            },
        );
        assert!(doc(&b).forms.checked(&doc(&b).dom, el(&b, "news")));
        let large = doc(&b)
            .dom
            .descendants(Dom::ROOT)
            .into_iter()
            .find(|&n| doc(&b).dom.attr(n, "value") == Some("l"))
            .unwrap();
        let r = doc(&b).content_box(large).unwrap();
        click_at(
            &mut b,
            &mut h.inner,
            RectF {
                x: vp.x + r.x,
                y: vp.y + r.y,
                ..r
            },
        );
        click_page_h(&mut b, &mut h, "notes");
        b.event(Event::Text("line one".into()), 0.0, &mut h);
        b.event(
            Event::KeyDown {
                key: key::ENTER,
                mods: Modifiers::default(),
            },
            0.0,
            &mut h,
        );
        b.event(Event::Text("two".into()), 0.0, &mut h);
        assert_eq!(doc(&b).input_value(el(&b, "notes")), "line one\ntwo");

        click_page_h(&mut b, &mut h, "send");
        b.render(0.0, &mut h);
        let url = b.tabs[b.active].url().to_string();
        assert_eq!(
            url,
            "about:form?name=Ada&pw=&plan=pro&size=l&notes=line+one%0Atwo&news=yes"
        );
        // The page reads location.search and shows the values.
        let shown = doc(&b).dom.text_content(el(&b, "data"));
        assert!(
            shown.contains("namenAda") || shown.contains("nameAda"),
            "{shown}"
        );
        assert!(shown.contains("line one\ntwo"), "{shown}");
    }

    fn click_page_h(b: &mut Browser, h: &mut LogHost, id: &str) {
        let n = page_el(b, id);
        let r = b.tabs[b.active].doc.content_box(n).unwrap();
        let vp = b.viewport_rect();
        let (x, y) = (
            (vp.x + r.x + r.w / 2.0) as i32,
            (vp.y + r.y + r.h / 2.0) as i32,
        );
        b.event(Event::MouseDown { x, y, button: 0 }, 0.0, h);
        b.event(Event::MouseUp { x, y, button: 0 }, 0.0, h);
        b.render(0.0, h);
    }

    fn png_data_url(w: u32, h: u32, rgba: [u8; 4]) -> String {
        let img = image::RgbaImage::from_pixel(w, h, image::Rgba(rgba));
        let mut bytes = Vec::new();
        image::DynamicImage::ImageRgba8(img)
            .write_to(
                &mut std::io::Cursor::new(&mut bytes),
                image::ImageFormat::Png,
            )
            .unwrap();
        const T: &[u8] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
        let mut b64 = String::new();
        for chunk in bytes.chunks(3) {
            let n = chunk.iter().fold(0u32, |a, &b| a << 8 | b as u32) << (8 * (3 - chunk.len()));
            for i in 0..4 {
                if i <= chunk.len() {
                    b64.push(T[(n >> (18 - 6 * i) & 63) as usize] as char);
                } else {
                    b64.push('=');
                }
            }
        }
        format!("data:image/png;base64,{b64}")
    }

    #[test]
    fn images_load_size_and_paint() {
        let (mut b, mut h) = browser();
        b.navigate("https://img.test/", &mut h);
        let id = h.fetched.last().unwrap().0;
        let red = png_data_url(4, 2, [255, 0, 0, 255]);
        let page = format!(
            r#"<body style="margin:0">
            <img id=a src="{red}" width=40><br>
            <img id=b src="{red}" style="height:10px">
            <img id=c src="missing.png" alt="Broken image">
            <img id=d src="slow.png"></body>"#
        );
        b.fetch_done(id, Ok(page.into_bytes()), &mut h);
        b.render(0.0, &mut h);
        b.render(0.0, &mut h);
        let fetched: Vec<_> = h.fetched.iter().map(|(_, u)| u.clone()).collect();
        assert!(fetched.contains(&"https://img.test/missing.png".to_string()));
        let missing = h
            .fetched
            .iter()
            .find(|(_, u)| u.ends_with("missing.png"))
            .unwrap()
            .0;
        b.fetch_done(missing, Err("404".into()), &mut h);
        b.render(0.0, &mut h);

        let doc = &b.tabs[b.active].doc;
        let rect = |id: &str| doc.content_box(doc.dom.element_by_id(id).unwrap()).unwrap();
        let (a, bb, c, d) = (rect("a"), rect("b"), rect("c"), rect("d"));
        assert_eq!(
            (a.w, a.h),
            (40.0, 20.0),
            "width attribute keeps the aspect ratio"
        );
        assert_eq!(
            (bb.w, bb.h),
            (20.0, 10.0),
            "CSS height keeps the aspect ratio"
        );
        assert!(c.w > 60.0 && c.h > 10.0, "alt text box: {c:?}");
        assert_eq!((d.w, d.h), (0.0, 0.0), "still loading");

        // The red image is painted.
        let vp = b.viewport_rect();
        let canvas = b.render(1.0, &mut h);
        let (px, py) = ((vp.x + a.x + 5.0) as usize, (vp.y + a.y + 5.0) as usize);
        assert_eq!(canvas.pixels()[py * canvas.width() + px], 0xff0000);
    }

    #[test]
    fn wheel_scrolls_the_page() {
        let (mut b, mut h) = browser();
        b.navigate("https://tall.test/", &mut h);
        let id = h.fetched.last().unwrap().0;
        let page = "<div style='height:3000px'>tall</div>";
        b.fetch_done(id, Ok(page.as_bytes().to_vec()), &mut h);
        b.render(0.0, &mut h);
        let vp = b.viewport_rect();
        b.event(
            Event::MouseMove {
                x: 300,
                y: (vp.y + 200.0) as i32,
            },
            0.0,
            &mut h,
        );
        b.event(Event::Wheel { dx: 0, dy: 500 }, 0.0, &mut h);
        assert_eq!(b.tabs[b.active].doc.scroll_y(), 500.0);
    }

    #[test]
    fn css_and_html_extras() {
        let (mut b, mut h) = browser();
        b.navigate("https://x.test/dir/page.html", &mut h);
        let id = h.fetched.last().unwrap().0;
        let blue = png_data_url(2, 2, [0, 0, 255, 255]);
        let page = format!(
            r#"<head><base href="https://cdn.test/assets/"></head>
            <body style="margin:0">
            <p id=up style="text-transform:uppercase;margin:0">hello</p>
            <p style="margin:0">x<sup id=sup>2</sup></p>
            <details id=d><summary id=s>More</summary><p id=hidden>secret</p></details>
            <div id=fixed style="position:fixed;top:0;right:0;width:30px;height:30px;background:#00ff00;z-index:5"></div>
            <div style="position:relative;height:60px">
              <div id=low style="position:absolute;left:0;top:0;width:50px;height:50px;z-index:1;background:#ff0000"></div>
              <div id=high style="position:absolute;left:10px;top:10px;width:50px;height:50px;z-index:2;background:#0000ff"></div>
            </div>
            <div id=faded style="opacity:0.5;background:#000000;width:20px;height:20px"></div>
            <div id=dash style="border-top:4px dashed #ff0000;width:100px;height:4px"></div>
            <div id=bg style="width:40px;height:40px;background:url('{blue}') repeat"></div>
            <div style="width:300px"><div id=fl style="float:right;width:50px;height:20px"></div></div>
            <a id=link href="img.png">rel</a>
            <div style="height:2000px"></div></body>"#
        );
        b.fetch_done(id, Ok(page.into_bytes()), &mut h);
        b.render(0.0, &mut h);
        b.render(0.0, &mut h);
        b.render(0.0, &mut h);
        let vp = b.viewport_rect();
        fn doc(b: &Browser) -> &Document {
            &b.tabs[b.active].doc
        }
        let rect = |b: &Browser, id: &str| doc(b).box_rects(page_el(b, id)).unwrap().0;

        // text-transform
        let up = page_el(&b, "up");
        assert!(doc(&b).dom.text_content(up) == "hello");
        // <sup> sits above the baseline of its line.
        // <details> hides its content until opened.
        assert!(doc(&b).box_rects(page_el(&b, "hidden")).is_none());
        click_page(&mut b, &mut h, "s");
        assert!(doc(&b).dom.attr(page_el(&b, "d"), "open").is_some());
        assert!(doc(&b).box_rects(page_el(&b, "hidden")).is_some());
        // z-index: the higher box wins hit-testing where they overlap.
        let high = rect(&b, "high");
        assert_eq!(
            doc(&b).hit_test(high.x + 5.0, high.y + 5.0).element,
            Some(page_el(&b, "high"))
        );
        // Float right.
        let fl = rect(&b, "fl");
        assert_eq!(fl.x + fl.w, 300.0, "{fl:?}");
        // <base href> resolves links.
        let link = page_el(&b, "link");
        let resolved = url::resolve(&doc(&b).base_url(), doc(&b).dom.attr(link, "href").unwrap());
        assert_eq!(resolved, "https://cdn.test/assets/img.png");

        // Painting: opacity, dashes, background image, fixed box.
        let px = |b: &mut Browser, h: &mut TestHost, x: f32, y: f32| {
            let c = b.render(1.0, h);
            c.pixels()[(vp.y + y) as usize * c.width() + (vp.x + x) as usize]
        };
        let f = rect(&b, "faded");
        assert_eq!(px(&mut b, &mut h, f.x + 5.0, f.y + 5.0), 0x7f7f7f);
        let d = rect(&b, "dash");
        let dashes: Vec<u32> = (0..30)
            .map(|i| px(&mut b, &mut h, d.x + i as f32, d.y + 1.0))
            .collect();
        assert!(
            dashes.contains(&0xff0000) && dashes.contains(&0xffffff),
            "dashed: {dashes:x?}"
        );
        let g = rect(&b, "bg");
        assert_eq!(
            px(&mut b, &mut h, g.x + 30.0, g.y + 30.0),
            0x0000ff,
            "tiled background"
        );
        // Scroll: the fixed box stays at the top right.
        let (mx, my) = ((vp.x + 100.0) as i32, (vp.y + 100.0) as i32);
        b.event(Event::MouseMove { x: mx, y: my }, 0.0, &mut h);
        b.event(Event::Wheel { dx: 0, dy: 300 }, 0.0, &mut h);
        assert_eq!(px(&mut b, &mut h, vp.w - 10.0, 10.0), 0x00ff00);
        let hit = doc(&b).hit_test(vp.w - 10.0, 10.0).element;
        assert_eq!(hit, Some(page_el(&b, "fixed")));
    }

    /// A minimal language runtime: its "interpreter" writes the script
    /// source into #out. (Tests accept WebAssembly text.)
    const SHOUT_RUNTIME: &str = r##"(module
        (import "rb" "query" (func $query (param i32 i32) (result i32)))
        (import "rb" "set_text" (func $set_text (param i32 i32 i32)))
        (memory (export "memory") 1)
        (data (i32.const 0) "#out")
        (global $next (mut i32) (i32.const 1024))
        (func (export "rb_alloc") (param $len i32) (result i32)
          (local $p i32)
          (local.set $p (global.get $next))
          (global.set $next (i32.add (global.get $next) (local.get $len)))
          (local.get $p))
        (func (export "rb_eval") (param $src i32) (param $len i32) (param i32 i32)
          (call $set_text (call $query (i32.const 0) (i32.const 4))
                          (local.get $src) (local.get $len))))"##;

    #[test]
    fn page_provided_language_runtime() {
        let mut h = LogHost::default();
        let mut b = Browser::new(800, 600, 1.0);
        b.render(0.0, &mut h);
        b.navigate("https://lang.test/", &mut h);
        let id = h.inner.fetched.last().unwrap().0;
        let page = r#"<link rel="rubrowser-runtime" type="text/x-shout" href="shout.wasm">
            <p id=out>before</p>
            <script type="text/x-shout">hello from a new language</script>
            <script type="text/x-unknown">ignored</script>"#;
        b.fetch_done(id, Ok(page.as_bytes().to_vec()), &mut h);
        let (rid, url) = h.inner.fetched.last().unwrap().clone();
        assert_eq!(url, "https://lang.test/shout.wasm");
        b.fetch_done(rid, Ok(SHOUT_RUNTIME.as_bytes().to_vec()), &mut h);
        b.render(0.0, &mut h);
        let doc = &b.tabs[b.active].doc;
        assert_eq!(
            doc.dom.text_content(page_el(&b, "out")),
            "hello from a new language"
        );
        assert!(
            h.logs
                .iter()
                .any(|l| l.contains("no runtime for scripts of type text/x-unknown")),
            "{:?}",
            h.logs
        );
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
