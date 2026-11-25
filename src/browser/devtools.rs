//! Developer tools: a panel docked below the page with three tools.
//!
//! - **Elements**: the page's DOM as a tree, the selected element's
//!   attributes, box model, matching style rules and computed styles; the
//!   element under the pointer (or selected) is outlined on the page with
//!   its margin, border, padding and content boxes; attributes and text can
//!   be edited; "inspect" mode picks an element by clicking the page.
//! - **Console**: what the page's scripts log (`console.*`, uncaught
//!   errors, failed requests) and a line that runs JavaScript in the page.
//! - **Network**: every request of the page with its status, type, size,
//!   time, headers, request body and response.
//! - **Sources**: the page's documents, scripts and style sheets (external
//!   and inline) with line numbers.
//! - **Application**: `localStorage`, `sessionStorage`, cookies (all kept
//!   by the JavaScript runtime) and `blob:` URLs, with edits.
//! - **Performance**: what each script call cost (instructions and time)
//!   and how long frames took to update and paint.
//!
//! The panel is part of the chrome document (`chrome.html`): this module
//! rewrites its contents from the browser's state, so it keeps working
//! when the page's scripts do not. Console and network logs are kept per
//! tab, from the start of each navigation.

use std::collections::{HashSet, VecDeque};

use super::{Browser, JS_MIME, Request, ResponseInfo, Target, escape, js_string};
use crate::dom::{Dom, NodeData, NodeId};
use crate::input::{Host, HttpRequest};

/// Most console messages kept per tab (the oldest go first).
const MAX_CONSOLE: usize = 2000;
/// Most requests kept per tab.
const MAX_NETWORK: usize = 1000;
/// Most rows drawn in the DOM tree.
const MAX_ROWS: usize = 3000;
/// Bytes of request and response bodies kept for previews.
const MAX_BODY: usize = 256 * 1024;
/// Height of a DOM tree row (`.dt-row` in chrome.html).
const ROW_HEIGHT: f32 = 18.0;
/// A changing DOM redraws the tree at most this often (milliseconds).
const TREE_REFRESH_MS: f64 = 300.0;

/// Message marker of console results (see `__rb_devtools_eval` in
/// prelude.js).
const RESULT: &str = "\u{1}devtools-result ";
const RESULT_ERROR: &str = "\u{1}devtools-error ";
/// Message marker of a storage snapshot (`__rb_devtools_storage`).
const STORAGE: &str = "\u{1}devtools-storage ";
/// Bytes of a script, style sheet or document kept for Sources.
const MAX_SOURCE: usize = 4 << 20;
/// Lines shown of one source.
const MAX_LINES: usize = 5000;
/// Script calls and frames kept for Performance.
const MAX_CALLS: usize = 500;
#[cfg_attr(target_arch = "wasm32", allow(dead_code))]
const MAX_FRAMES: usize = 120;

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Panel {
    #[default]
    Elements,
    Console,
    Network,
    Sources,
    Application,
    Performance,
}

impl Panel {
    fn parse(s: &str) -> Option<Panel> {
        match s {
            "elements" => Some(Panel::Elements),
            "console" => Some(Panel::Console),
            "network" => Some(Panel::Network),
            "sources" => Some(Panel::Sources),
            "application" => Some(Panel::Application),
            "performance" => Some(Panel::Performance),
            _ => None,
        }
    }

    fn name(self) -> &'static str {
        match self {
            Panel::Elements => "elements",
            Panel::Console => "console",
            Panel::Network => "network",
            Panel::Sources => "sources",
            Panel::Application => "application",
            Panel::Performance => "performance",
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Level {
    Log,
    Warn,
    Error,
    /// Code typed in the console.
    Command,
    /// Its result.
    Result,
}

/// Which console messages are shown.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Filter {
    #[default]
    All,
    Warn,
    Error,
}

pub struct ConsoleEntry {
    pub level: Level,
    pub text: String,
    /// The same message repeated this many times in a row.
    pub count: u32,
}

/// One request in the Network panel.
pub struct NetEntry {
    pub id: u32,
    pub url: String,
    pub method: String,
    /// `document`, `stylesheet`, `script`, `image`, `font`, `fetch`,
    /// `download`.
    pub kind: &'static str,
    pub start: f64,
    pub end: Option<f64>,
    pub status: Option<u16>,
    pub size: usize,
    pub error: Option<String>,
    pub request_type: String,
    /// The first [`MAX_BODY`] bytes of the request body.
    pub request_body: Vec<u8>,
    pub headers: Vec<(String, String)>,
    /// The first [`MAX_BODY`] bytes of the response.
    pub body: Vec<u8>,
}

/// A loaded script, style sheet or document, for Sources.
pub struct SourceFile {
    /// The request that loaded it.
    pub id: u32,
    pub url: String,
    pub kind: &'static str,
    pub text: String,
}

/// One call into a page script, for Performance.
pub struct CallRecord {
    /// Its slot among the tab's scripts.
    pub script: usize,
    pub stats: crate::script::CallStats,
}

/// The developer tools' state of one tab.
#[derive(Default)]
pub struct TabDev {
    pub console: Vec<ConsoleEntry>,
    pub network: Vec<NetEntry>,
    pub sources: Vec<SourceFile>,
    pub calls: VecDeque<CallRecord>,
    /// The last storage snapshot: (area, key, value), area `local`,
    /// `session` or `cookie`; `None` until taken.
    pub storage: Option<Vec<(String, String, String)>>,
    /// `element.style` declarations turned off: (element, name, value).
    pub disabled_inline: Vec<(NodeId, String, String)>,
    /// Expanded elements of the DOM tree.
    pub expanded: HashSet<NodeId>,
    pub selected: Option<NodeId>,
    /// The tab's document (`Tab::page`) the node ids belong to.
    pub page: u64,
}

/// The developer tools' window state.
pub struct DevTools {
    pub open: bool,
    pub panel: Panel,
    /// Height in logical pixels.
    pub height: f32,
    /// Picking an element by clicking the page.
    pub inspecting: bool,
    /// The element outlined on the page (under the pointer in the tree or
    /// in the page while inspecting).
    pub hover: Option<NodeId>,
    /// The request shown in detail.
    pub request: Option<u32>,
    pub filter: Filter,
    /// The panel's contents need rebuilding.
    pub dirty: bool,
    /// (tab, document, DOM version) the panel was built for, and when.
    built: (u64, u64, u64),
    built_at: f64,
    /// A drag of the top edge: (pointer y, height) at its start.
    drag: Option<(f32, f32)>,
    /// Scroll the console to its end, the tree to the selection.
    stick: bool,
    reveal: bool,
    /// Console input history, and the entry being edited.
    history: Vec<String>,
    history_at: usize,
    /// The source shown in Sources (`net:ID` or `node:ID`).
    pub source: Option<String>,
    /// (update, paint) milliseconds of recent frames.
    pub frames: VecDeque<(f64, f64)>,
    /// Take a storage snapshot at the next chance.
    want_storage: bool,
    /// `hover` was set by a remote debugger (`Overlay.highlightNode`): drawn
    /// even with the panel closed.
    pub remote_highlight: bool,
}

impl Default for DevTools {
    fn default() -> Self {
        DevTools {
            open: false,
            panel: Panel::Elements,
            height: 300.0,
            inspecting: false,
            hover: None,
            request: None,
            filter: Filter::All,
            dirty: true,
            built: (0, 0, 0),
            built_at: f64::NEG_INFINITY,
            drag: None,
            stick: false,
            reveal: false,
            history: Vec::new(),
            history_at: 0,
            source: None,
            frames: VecDeque::new(),
            want_storage: false,
            remote_highlight: false,
        }
    }
}

/// Elements whose children the tree shows at first.
fn default_expanded(dom: &Dom) -> HashSet<NodeId> {
    [dom.html(), dom.body()].into_iter().flatten().collect()
}

impl Browser {
    // ----- opening and closing ---------------------------------------------

    /// Shows or hides the developer tools (F12, Cmd+Opt+I, Ctrl+Shift+I).
    pub fn toggle_devtools(&mut self) {
        if self.devtools.open {
            self.close_devtools();
        } else {
            let panel = self.devtools.panel.name();
            self.open_devtools(panel);
        }
    }

    /// Shows the developer tools at `panel` (`elements`, `console`,
    /// `network`; others keep the current one).
    pub fn open_devtools(&mut self, panel: &str) {
        let d = &mut self.devtools;
        d.open = true;
        if let Some(p) = Panel::parse(panel) {
            d.panel = p;
        }
        d.dirty = true;
        d.stick = true;
        d.reveal = true;
        self.dirty = true;
    }

    pub fn close_devtools(&mut self) {
        let d = &mut self.devtools;
        d.open = false;
        d.inspecting = false;
        d.hover = None;
        d.drag = None;
        if self.keyboard == Target::Chrome {
            self.keyboard = Target::Page;
            self.set_focus(Target::Chrome, None);
        }
        self.dirty = true;
    }

    pub fn devtools_open(&self) -> bool {
        self.devtools.open
    }

    /// Selects element (or text node) `node` of the page in the Elements
    /// panel, opening it and expanding the tree down to the node.
    pub fn inspect(&mut self, node: NodeId) {
        self.tab_dev(self.active);
        let tab = &mut self.tabs[self.active];
        if tab.doc.dom.get(node).is_none() {
            return;
        }
        let mut up = tab.doc.dom.parent(node);
        while let Some(p) = up {
            tab.dev.expanded.insert(p);
            up = tab.doc.dom.parent(p);
        }
        tab.dev.selected = Some(node);
        self.devtools.inspecting = false;
        self.open_devtools("elements");
    }

    /// Selects the first element matching CSS selector `selector`; returns
    /// whether there is one.
    pub fn inspect_selector(&mut self, selector: &str) -> bool {
        let doc = &self.tabs[self.active].doc;
        let found = crate::css::query(
            &doc.dom,
            doc.dom.descendants(Dom::ROOT),
            selector,
            &doc.state(),
        )
        .and_then(|v| v.first().copied());
        match found {
            Some(n) => {
                self.inspect(n);
                true
            }
            None => false,
        }
    }

    /// Tab `i`'s developer tools state, reset when it shows a new document.
    fn tab_dev(&mut self, i: usize) -> &mut TabDev {
        let tab = &mut self.tabs[i];
        if tab.dev.page != tab.page {
            tab.dev.page = tab.page;
            tab.dev.selected = None;
            tab.dev.expanded = default_expanded(&tab.doc.dom);
            tab.dev.storage = None;
            tab.dev.disabled_inline.clear();
        }
        &mut tab.dev
    }

    // ----- console -----------------------------------------------------------

    /// A message from tab `i`'s scripts (or about them): to the platform's
    /// log and the tab's console.
    pub(super) fn console_message(&mut self, i: usize, msg: &str, host: &mut dyn Host) {
        if let Some(r) = msg.strip_prefix(super::cdp::CDP_RESULT) {
            return self.cdp_eval_result(r);
        }
        if let Some(snapshot) = msg.strip_prefix(STORAGE) {
            let dec = |s: &str| String::from_utf8_lossy(&crate::net::url::percent_decode(s)).into_owned();
            let rows = snapshot
                .lines()
                .filter_map(|l| {
                    let mut f = l.split('\t');
                    Some((f.next()?.to_string(), dec(f.next()?), dec(f.next()?)))
                })
                .collect();
            self.tabs[i].dev.storage = Some(rows);
            self.devtools.dirty = true;
            return;
        }
        let (level, text) = if let Some(r) = msg.strip_prefix(RESULT) {
            (Level::Result, r)
        } else if let Some(r) = msg.strip_prefix(RESULT_ERROR) {
            (Level::Error, r)
        } else {
            host.log(msg);
            if let Some(r) = msg.strip_prefix("warning: ") {
                (Level::Warn, r)
            } else if let Some(r) = msg.strip_prefix("error: ") {
                (Level::Error, r)
            } else if msg.starts_with("Uncaught ") || msg.starts_with("script error") {
                (Level::Error, msg)
            } else {
                (Level::Log, msg)
            }
        };
        self.cdp_console(i, level, text, host);
        self.console_push(i, level, text);
    }

    fn console_push(&mut self, i: usize, level: Level, text: &str) {
        let console = &mut self.tabs[i].dev.console;
        match console.last_mut() {
            Some(last) if last.level == level && last.text == text && level != Level::Command => {
                last.count += 1;
            }
            _ => console.push(ConsoleEntry {
                level,
                text: text.to_string(),
                count: 1,
            }),
        }
        if console.len() > MAX_CONSOLE {
            console.drain(..console.len() - MAX_CONSOLE);
        }
        if i == self.active {
            self.devtools.dirty = true;
            self.devtools.stick = true;
        }
    }

    /// Runs `src` typed in the console in the active page.
    pub fn devtools_eval(&mut self, src: &str, host: &mut dyn Host) {
        let src = src.trim();
        if src.is_empty() {
            return;
        }
        let d = &mut self.devtools;
        if d.history.last().map(String::as_str) != Some(src) {
            d.history.push(src.to_string());
        }
        d.history_at = d.history.len();
        let i = self.active;
        self.console_push(i, Level::Command, src);
        let web = !self.tabs[i].doc.url.starts_with("about:");
        if web && !self.javascript {
            return self.console_push(i, Level::Error, "JavaScript is turned off for web pages");
        }
        let code = format!("__rb_devtools_eval({})", js_string(src));
        self.run_source(i, JS_MIME, &code, "devtools console", host);
    }

    /// Up / Down in the console input: earlier or later input.
    fn console_history(&mut self, up: bool) {
        let Some(input) = self.chrome_el("dt-input") else {
            return;
        };
        let d = &mut self.devtools;
        if up && d.history_at > 0 {
            d.history_at -= 1;
        } else if !up && d.history_at < d.history.len() {
            d.history_at += 1;
        } else {
            return;
        }
        let text = d.history.get(d.history_at).cloned().unwrap_or_default();
        self.chrome.set_input_value(input, &text);
        self.dirty = true;
    }

    // ----- network -----------------------------------------------------------

    /// Records request `id` (already in `self.requests`) as started.
    pub(super) fn net_begin(
        &mut self,
        id: u32,
        url: &str,
        request: Option<&HttpRequest>,
        host: &mut dyn Host,
    ) {
        let Some(req) = self.requests.get(&id) else {
            return;
        };
        let kind = match req {
            Request::Page { .. } | Request::Frame { .. } => "document",
            Request::Stylesheet { .. } => "stylesheet",
            Request::Script { .. } | Request::Module { .. } => "script",
            Request::Image { .. } => "image",
            Request::Font { .. } => "font",
            Request::Http { .. } => "fetch",
            Request::Download { .. } => "download",
        };
        let navigation = matches!(req, Request::Page { .. });
        let Some(i) = self.tab_index(req.tab()) else {
            return;
        };
        let method = request
            .map(|r| r.method.as_str())
            .filter(|m| !m.is_empty())
            .unwrap_or("GET")
            .to_string();
        let now = host.wall_clock().unwrap_or(self.now);
        self.cdp_request(id, url, &method, kind, request.map(|r| &r.body[..]), host);
        let dev = &mut self.tabs[i].dev;
        if navigation {
            // A new page: its log starts here (as in other browsers).
            dev.network.clear();
            dev.console.clear();
            dev.sources.clear();
            dev.calls.clear();
            self.devtools.source = None;
            self.devtools.request = None;
        }
        dev.network.push(NetEntry {
            id,
            url: url.to_string(),
            method,
            kind,
            start: now,
            end: None,
            status: None,
            size: 0,
            error: None,
            request_type: request.map(|r| r.content_type.clone()).unwrap_or_default(),
            request_body: request
                .map(|r| r.body[..r.body.len().min(MAX_BODY)].to_vec())
                .unwrap_or_default(),
            headers: Vec::new(),
            body: Vec::new(),
        });
        if dev.network.len() > MAX_NETWORK {
            dev.network.remove(0);
        }
        if i == self.active {
            self.devtools.dirty = true;
        }
    }

    /// Records the end of request `id` (before it leaves `self.requests`).
    pub(super) fn net_end(
        &mut self,
        id: u32,
        result: &Result<Vec<u8>, String>,
        info: Option<&ResponseInfo>,
        host: &mut dyn Host,
    ) {
        let Some(i) = self.requests.get(&id).and_then(|r| self.tab_index(r.tab())) else {
            return;
        };
        // Requests end between frames: the wall clock, not the frame time.
        let now = host.wall_clock().unwrap_or(self.now);
        let Some(e) = self.tabs[i].dev.network.iter_mut().rev().find(|e| e.id == id) else {
            return;
        };
        e.end = Some(now);
        if let Ok(body) = result
            && matches!(e.kind, "script" | "stylesheet" | "document")
        {
            let text = String::from_utf8_lossy(&body[..body.len().min(MAX_SOURCE)]).into_owned();
            let (url, kind) = (e.url.clone(), e.kind);
            self.tabs[i].dev.sources.push(SourceFile { id, url, kind, text });
        }
        let Some(e) = self.tabs[i].dev.network.iter_mut().rev().find(|e| e.id == id) else {
            return;
        };
        match result {
            Ok(body) => {
                e.status = Some(info.map_or(200, |r| r.status));
                e.size = body.len();
                e.body = body[..body.len().min(MAX_BODY)].to_vec();
                if let Some(info) = info {
                    e.headers = info.headers.clone();
                }
            }
            Err(err) => e.error = Some(err.clone()),
        }
        let (url, kind) = (e.url.clone(), e.kind);
        let status = info.map_or(200, |r| r.status);
        let headers = info.map(|r| r.headers.clone()).unwrap_or_default();
        let ended = match result {
            Ok(body) => Ok((status, &headers[..], body.len())),
            Err(err) => Err(err.as_str()),
        };
        self.cdp_response(id, &url, kind, ended, host);
        if i == self.active {
            self.devtools.dirty = true;
        }
    }

    // ----- application, performance -------------------------------------------

    /// Runs `code` (storage edits) in the page, then takes a new snapshot.
    fn storage_script(&mut self, code: &str, host: &mut dyn Host) {
        let i = self.active;
        if self.tabs[i].runtimes.contains_key(JS_MIME) {
            self.run_source(i, JS_MIME, code, "devtools", host);
        }
        self.devtools.want_storage = true;
        self.devtools.dirty = true;
        self.dirty = true;
    }

    /// Work that needs the host: a storage snapshot asked for.
    pub(super) fn devtools_tick(&mut self, host: &mut dyn Host) {
        if !self.devtools.open {
            return;
        }
        self.tab_dev(self.active);
        let d = &self.devtools;
        let want = d.open
            && d.panel == Panel::Application
            && (d.want_storage || self.tabs[self.active].dev.storage.is_none());
        if !want {
            return;
        }
        self.devtools.want_storage = false;
        let i = self.active;
        // Storage lives in the JavaScript runtime: none, nothing stored.
        if self.tabs[i].runtimes.contains_key(JS_MIME) {
            self.run_source(i, JS_MIME, "__rb_devtools_storage()", "devtools", host);
        }
        let dev = &mut self.tabs[i].dev;
        if dev.storage.is_none() {
            dev.storage = Some(Vec::new());
        }
        self.devtools.dirty = true;
    }

    /// Records the cost of the call script `si` of tab `i` just made.
    pub(super) fn record_call(&mut self, i: usize, si: usize) {
        let Some(Some(script)) = self.tabs[i].scripts.get_mut(si) else {
            return;
        };
        let Some(stats) = script.take_call() else {
            return;
        };
        // The panel's own snapshots are not the page's work.
        if stats.label == "devtools" {
            return;
        }
        let calls = &mut self.tabs[i].dev.calls;
        calls.push_back(CallRecord { script: si, stats });
        if calls.len() > MAX_CALLS {
            calls.pop_front();
        }
        if i == self.active && self.devtools.panel == Panel::Performance {
            self.devtools.dirty = true;
        }
    }

    /// Records how long a frame took to update (style, layout, scripts'
    /// timers) and to paint.
    #[cfg_attr(target_arch = "wasm32", allow(dead_code))]
    pub(super) fn record_frame(&mut self, update_ms: f64, paint_ms: f64) {
        let f = &mut self.devtools.frames;
        f.push_back((update_ms, paint_ms));
        if f.len() > MAX_FRAMES {
            f.pop_front();
        }
        // Redrawing the panel makes another frame: only on request.
    }

    /// Turns `element.style` declaration `k` of the selected element off
    /// (`k` < the declarations it has) or a turned-off one back on.
    fn toggle_inline(&mut self, k: usize) {
        let Some(n) = self.tabs[self.active].dev.selected else {
            return;
        };
        let tab = &mut self.tabs[self.active];
        let style = tab.doc.dom.attr(n, "style").unwrap_or("").to_string();
        let decls = declarations(&style);
        let new_style = if let Some((name, value, important)) = decls.get(k) {
            let value = if *important { format!("{value} !important") } else { value.clone() };
            tab.dev.disabled_inline.push((n, name.clone(), value));
            crate::script::set_style_property(&style, name, "")
        } else {
            let off: Vec<usize> = (0..tab.dev.disabled_inline.len())
                .filter(|&j| tab.dev.disabled_inline[j].0 == n)
                .collect();
            let Some(&j) = off.get(k - decls.len()) else {
                return;
            };
            let (_, name, value) = tab.dev.disabled_inline.remove(j);
            crate::script::set_style_property(&style, &name, &value)
        };
        if let Some(e) = tab.doc.dom_mut().element_mut(n) {
            e.set_attr("style", &new_style);
        }
        tab.doc.dom_changed();
    }

    // ----- commands and input --------------------------------------------------

    /// A click on a `data-dt` control of the panel.
    pub(super) fn devtools_command(&mut self, cmd: &str, host: &mut dyn Host) {
        let (verb, arg) = cmd.split_once(':').unwrap_or((cmd, ""));
        let id = arg.parse::<usize>().ok();
        self.tab_dev(self.active);
        let d = &mut self.devtools;
        match verb {
            "panel" => {
                if let Some(p) = Panel::parse(arg) {
                    d.panel = p;
                    d.stick = true;
                    d.reveal = true;
                    d.want_storage |= p == Panel::Application;
                }
            }
            "close" => return self.close_devtools(),
            "inspect" => {
                d.inspecting = !d.inspecting;
                d.hover = None;
            }
            "clear" => {
                let dev = &mut self.tabs[self.active].dev;
                match d.panel {
                    Panel::Console => dev.console.clear(),
                    Panel::Network => {
                        dev.network.clear();
                        d.request = None;
                    }
                    Panel::Performance => {
                        dev.calls.clear();
                        d.frames.clear();
                    }
                    Panel::Application => {
                        let code = "localStorage.clear(); sessionStorage.clear(); \
                                    for (const c of document.cookie.split('; ')) __rb_devtools_cookie_delete(c.split('=')[0]);";
                        return self.storage_script(code, host);
                    }
                    Panel::Elements | Panel::Sources => {}
                }
            }
            "filter" => {
                d.filter = match arg {
                    "warn" => Filter::Warn,
                    "error" => Filter::Error,
                    _ => Filter::All,
                };
                d.stick = true;
            }
            "select" => {
                if let Some(n) = id {
                    self.tabs[self.active].dev.selected = Some(n);
                }
            }
            "toggle" => {
                if let Some(n) = id {
                    let ex = &mut self.tabs[self.active].dev.expanded;
                    if !ex.remove(&n) {
                        ex.insert(n);
                    }
                }
            }
            "req" => {
                let r = arg.parse::<u32>().ok();
                d.request = if d.request == r { None } else { r };
            }
            "src" => d.source = Some(arg.to_string()),
            "refresh" => d.want_storage = true,
            "del" => {
                // del:AREA:KEY (the key percent-encoded)
                let (area, key) = arg.split_once(':').unwrap_or((arg, ""));
                let key = js_string(&String::from_utf8_lossy(&crate::net::url::percent_decode(key)));
                let code = match area {
                    "local" => format!("localStorage.removeItem({key})"),
                    "session" => format!("sessionStorage.removeItem({key})"),
                    _ => format!("__rb_devtools_cookie_delete({key})"),
                };
                return self.storage_script(&code, host);
            }
            "decl" => {
                // decl:BLOCK:INDEX, a declaration of a style rule
                let (block, index) = arg.split_once(':').unwrap_or((arg, ""));
                if let (Ok(b), Ok(k)) = (block.parse(), index.parse()) {
                    self.tabs[self.active].doc.toggle_declaration(b, k);
                }
            }
            "inline" => {
                if let Some(k) = id {
                    self.toggle_inline(k);
                }
            }
            _ => {}
        }
        self.devtools.dirty = true;
        self.dirty = true;
    }

    /// Enter in one of the panel's inputs; returns whether `n` is one.
    pub(super) fn devtools_submit(&mut self, n: NodeId, host: &mut dyn Host) -> bool {
        if Some(n) == self.chrome_el("dt-input") {
            let src = self.chrome.input_value(n);
            self.chrome.set_input_value(n, "");
            self.devtools_eval(&src, host);
            return true;
        }
        if Some(n) == self.chrome_el("dt-app-edit") {
            let text = self.chrome.input_value(n);
            self.chrome.set_input_value(n, "");
            // `key=value` sets localStorage; `session key=value` sessionStorage.
            let (area, rest) = match text.trim().strip_prefix("session ") {
                Some(r) => ("sessionStorage", r),
                None => ("localStorage", text.trim()),
            };
            if let Some((k, v)) = rest.split_once('=') {
                let code = format!("{area}.setItem({}, {})", js_string(k.trim()), js_string(v.trim()));
                self.storage_script(&code, host);
            }
            return true;
        }
        if Some(n) == self.chrome_el("dt-edit") {
            let text = self.chrome.input_value(n);
            self.chrome.set_input_value(n, "");
            self.devtools_edit(&text);
            return true;
        }
        false
    }

    /// Applies an edit typed under the selected node: `name=value` sets an
    /// attribute, `-name` removes one; a text node takes the text.
    pub fn devtools_edit(&mut self, text: &str) {
        let Some(n) = self.tab_dev(self.active).selected else {
            return;
        };
        let doc = &mut self.tabs[self.active].doc;
        let colon = text.find(':');
        let equals = text.find('=');
        if doc.dom.text(n).is_some() {
            doc.dom_mut().set_text(n, text);
        } else if let Some(c) = colon.filter(|&c| equals.is_none_or(|e| c < e)) {
            // `prop: value` sets an `element.style` property.
            let (prop, value) = (text[..c].trim().to_ascii_lowercase(), text[c + 1..].trim());
            let value = value.trim_end_matches(';').trim();
            let style = doc.dom.attr(n, "style").unwrap_or("").to_string();
            let style = crate::script::set_style_property(&style, &prop, value);
            if let Some(e) = doc.dom_mut().element_mut(n) {
                e.set_attr("style", &style);
            }
        } else if let Some(name) = text.trim().strip_prefix('-') {
            if let Some(e) = doc.dom_mut().element_mut(n) {
                e.remove_attr(name.trim());
            }
        } else if let Some((name, value)) = text.split_once('=') {
            let value = value.trim();
            let value = value
                .strip_prefix('"')
                .and_then(|v| v.strip_suffix('"'))
                .unwrap_or(value);
            let name = name.trim().to_ascii_lowercase();
            if !name.is_empty()
                && let Some(e) = doc.dom_mut().element_mut(n)
            {
                e.set_attr(&name, value);
            }
        } else {
            return;
        }
        doc.dom_changed();
        self.devtools.dirty = true;
        self.dirty = true;
    }

    /// Keys for the panel's inputs (console history); returns whether the
    /// key was used.
    pub(super) fn devtools_key(&mut self, code: i32) -> bool {
        use crate::abi::key;
        let console = self.keyboard == Target::Chrome
            && self.chrome.state().focus.is_some()
            && self.chrome.state().focus == self.chrome_el("dt-input");
        if console && (code == key::UP || code == key::DOWN) {
            self.console_history(code == key::UP);
            return true;
        }
        if code == key::ESCAPE && self.devtools.inspecting {
            self.devtools.inspecting = false;
            self.devtools.hover = None;
            self.devtools.dirty = true;
            return true;
        }
        false
    }

    /// Pointer moves: resizing the panel, inspecting, outlining the
    /// element of the tree row under the pointer. Returns whether the move
    /// is used up.
    pub(super) fn devtools_pointer_move(&mut self, x: f32, y: f32) -> bool {
        if !self.devtools.open {
            return false;
        }
        if let Some((y0, h0)) = self.devtools.drag {
            let (_, wh) = self.logical_size();
            self.devtools.height = (h0 + y0 - y).clamp(80.0, (wh - 160.0).max(80.0));
            self.dirty = true;
            return true;
        }
        let (t, lx, ly) = self.target_at(x, y);
        let hover = if self.devtools.inspecting && t == Target::Page {
            self.tabs[self.active].doc.hit_test(lx, ly).element
        } else if t == Target::Chrome {
            self.tree_node_at(x, y)
        } else {
            None
        };
        if hover != self.devtools.hover {
            self.devtools.hover = hover;
            self.dirty = true;
        }
        self.devtools.inspecting && t == Target::Page
    }

    /// The page node of the DOM tree row at window point `(x, y)`.
    fn tree_node_at(&self, x: f32, y: f32) -> Option<NodeId> {
        let dom = &self.chrome.dom;
        let mut n = self.chrome.hit_test(x, y).element;
        while let Some(e) = n {
            if let Some(id) = dom.attr(e, "data-dt").and_then(|a| a.strip_prefix("select:")) {
                return id.parse().ok();
            }
            n = dom.parent(e);
        }
        None
    }

    /// A press: picks the element while inspecting, or starts resizing.
    /// Returns whether the press is used up.
    pub(super) fn devtools_pointer_down(&mut self, x: f32, y: f32) -> bool {
        if !self.devtools.open {
            return false;
        }
        let (t, lx, ly) = self.target_at(x, y);
        if self.devtools.inspecting && t == Target::Page {
            if let Some(e) = self.tabs[self.active].doc.hit_test(lx, ly).element {
                self.inspect(e);
            }
            self.devtools.hover = None;
            return true;
        }
        let on_edge = t == Target::Chrome
            && self.chrome.hit_test(lx, ly).element == self.chrome_el("dt-resize");
        if on_edge {
            self.devtools.drag = Some((y, self.devtools.height));
            return true;
        }
        false
    }

    /// The end of a press; returns whether it ended a resize.
    pub(super) fn devtools_pointer_up(&mut self) -> bool {
        self.devtools.drag.take().is_some()
    }

    // ----- drawing ---------------------------------------------------------------

    /// Updates the panel in the chrome document (before its layout).
    pub(super) fn sync_devtools(&mut self) {
        let Some(root) = self.chrome_el("devtools") else {
            return;
        };
        let d = &self.devtools;
        let (open, panel) = (d.open, d.panel);
        let class = panel.name();
        let style = format!("height:{}px", d.height.round());
        let dom = &self.chrome.dom;
        let shown = dom.attr(root, "hidden").is_none();
        if (shown != open
            || dom.attr(root, "class") != Some(class)
            || dom.attr(root, "style") != Some(style.as_str()))
            && let Some(e) = self.chrome.dom_mut().element_mut(root)
        {
            if open {
                e.remove_attr("hidden");
            } else {
                e.set_attr("hidden", "");
            }
            e.set_attr("class", class);
            e.set_attr("style", &style);
        }
        if !open {
            return;
        }
        self.tab_dev(self.active);
        let tab = &self.tabs[self.active];
        let key = (tab.id, tab.page, tab.doc.dom.version());
        let d = &self.devtools;
        let same_doc = (d.built.0, d.built.1) == (key.0, key.1);
        let tree_stale = panel == Panel::Elements
            && d.built.2 != key.2
            && (!same_doc || self.now - d.built_at >= TREE_REFRESH_MS);
        // Performance follows new frames about once a second.
        let perf_stale = panel == Panel::Performance && self.now - d.built_at >= 1000.0;
        if !d.dirty && same_doc && !tree_stale && !perf_stale {
            return;
        }
        self.devtools.dirty = false;
        self.devtools.built = key;
        self.devtools.built_at = self.now;
        self.sync_toolbar();
        match panel {
            Panel::Elements => self.build_elements(),
            Panel::Console => self.build_console(),
            Panel::Network => self.build_network(),
            Panel::Sources => self.build_sources(),
            Panel::Application => self.build_application(),
            Panel::Performance => self.build_performance(),
        }
    }

    /// The page's sources: (key, label, kind, text). Loaded files first,
    /// then inline scripts and style sheets.
    fn source_list(&self) -> Vec<(String, String, &'static str, String)> {
        let tab = &self.tabs[self.active];
        let mut out: Vec<(String, String, &'static str, String)> = tab
            .dev
            .sources
            .iter()
            .map(|f| (format!("net:{}", f.id), short_name(&f.url), f.kind, f.text.clone()))
            .collect();
        let dom = &tab.doc.dom;
        let (mut scripts, mut styles) = (0, 0);
        for n in dom.descendants(Dom::ROOT) {
            let (kind, count) = match dom.tag(n) {
                Some("script") if dom.attr(n, "src").is_none() => ("script", &mut scripts),
                Some("style") => ("stylesheet", &mut styles),
                _ => continue,
            };
            *count += 1;
            let label = format!("(inline {} {})", if kind == "script" { "script" } else { "style" }, count);
            out.push((format!("node:{n}"), label, kind, dom.text_content(n)));
        }
        out
    }

    fn build_sources(&mut self) {
        let list = self.source_list();
        let current = self
            .devtools
            .source
            .clone()
            .filter(|k| list.iter().any(|s| &s.0 == k))
            .or_else(|| list.first().map(|s| s.0.clone()));
        let mut files = String::new();
        for (key, label, kind, _) in &list {
            files.push_str(&format!(
                "<div class=\"dt-file{}\" data-dt=\"src:{key}\"><span class=\"dt-kind {kind}\">{}</span> {}</div>",
                if Some(key) == current.as_ref() { " sel" } else { "" },
                match *kind {
                    "script" => "JS",
                    "stylesheet" => "CSS",
                    _ => "HTML",
                },
                escape(label)
            ));
        }
        if list.is_empty() {
            files.push_str("<div class=\"dt-h\">No sources</div>");
        }
        let code = match list.iter().find(|s| Some(&s.0) == current.as_ref()) {
            Some((_, _, _, text)) => {
                let lines: Vec<&str> = text.lines().collect();
                let shown = lines.len().min(MAX_LINES);
                let numbers: String = (1..=shown).map(|i| format!("{i}\n")).collect();
                let mut body = lines[..shown].join("\n");
                if lines.len() > shown {
                    body.push_str(&format!("\n… {} more lines", lines.len() - shown));
                }
                format!(
                    "<pre class=\"dt-ln\">{numbers}</pre><pre class=\"dt-code-text\">{}</pre>",
                    escape(&body)
                )
            }
            None => String::new(),
        };
        self.set_html("dt-files", &files);
        self.set_html("dt-code", &code);
    }

    fn build_application(&mut self) {
        let tab = &self.tabs[self.active];
        let enc = |s: &str| {
            s.bytes()
                .map(|b| match b {
                    b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' => (b as char).to_string(),
                    b => format!("%{b:02X}"),
                })
                .collect::<String>()
        };
        let mut html = String::from(
            "<div class=\"dt-app-bar\"><button data-dt=\"refresh\">Refresh</button></div>",
        );
        let rows = tab.dev.storage.clone().unwrap_or_default();
        for (area, title) in [("local", "Local storage"), ("session", "Session storage"), ("cookie", "Cookies")] {
            html.push_str(&format!("<div class=\"dt-h\">{title}</div>"));
            let mut any = false;
            for (a, k, v) in rows.iter().filter(|r| r.0 == area) {
                any = true;
                html.push_str(&format!(
                    "<div class=\"dt-kvrow\"><span class=\"dt-x\" data-dt=\"del:{a}:{}\" title=\"Delete\">✕</span>\
                     <span class=\"dt-an\">{}</span><span class=\"dt-v\">{}</span></div>",
                    enc(k),
                    escape(k),
                    escape(&one_line(v, 300))
                ));
            }
            if !any {
                html.push_str("<div class=\"dt-kv dt-src\">(empty)</div>");
            }
        }
        html.push_str("<div class=\"dt-h\">Blob URLs</div>");
        let mut blobs: Vec<_> = tab.doc.blobs.iter().collect();
        blobs.sort_by(|a, b| a.0.cmp(b.0));
        for (url, f) in &blobs {
            html.push_str(&format!(
                "<div class=\"dt-kvrow\"><span class=\"dt-an\">{}</span><span class=\"dt-v\">{} {}</span></div>",
                escape(url),
                escape(if f.mime.is_empty() { "(no type)" } else { &f.mime }),
                size_text(f.bytes.len())
            ));
        }
        if blobs.is_empty() {
            html.push_str("<div class=\"dt-kv dt-src\">(none)</div>");
        }
        self.set_html("dt-store", &html);
    }

    fn build_performance(&mut self) {
        let tab = &self.tabs[self.active];
        let frames = &self.devtools.frames;
        let mut html = String::new();
        // Frames: one bar per frame (update below, paint on top).
        let (n, mut sum_u, mut sum_p, mut max) = (frames.len(), 0.0, 0.0, 0.0f64);
        for &(u, p) in frames {
            sum_u += u;
            sum_p += p;
            max = max.max(u + p);
        }
        html.push_str(&format!(
            "<div class=\"dt-h\">Frames (last {n})</div><div class=\"dt-kv\">update {} ms average, paint {} ms average, slowest {} ms</div>",
            round(if n > 0 { sum_u / n as f64 } else { 0.0 }),
            round(if n > 0 { sum_p / n as f64 } else { 0.0 }),
            round(max)
        ));
        let scale = if max > 0.0 { 60.0 / max.max(16.7) } else { 0.0 };
        html.push_str("<div class=\"dt-bars\">");
        for &(u, p) in frames {
            html.push_str(&format!(
                "<span class=\"dt-bar\"><span class=\"p\" style=\"height:{}px\"></span><span class=\"u\" style=\"height:{}px\"></span></span>",
                (p * scale).round(),
                (u * scale).round()
            ));
        }
        html.push_str("</div>");
        // Script calls: totals by kind, then the slowest and the latest.
        let calls = &tab.dev.calls;
        let fuel: u64 = calls.iter().map(|c| c.stats.fuel).sum();
        let ms: f64 = calls.iter().map(|c| c.stats.ms).sum();
        html.push_str(&format!(
            "<div class=\"dt-h\">Script calls ({})</div><div class=\"dt-kv\">{} ms, {} instructions</div>",
            calls.len(),
            round(ms),
            count_text(fuel)
        ));
        let mut by_kind: Vec<(String, usize, f64, u64)> = Vec::new();
        for c in calls {
            let kind = c.stats.label.split(' ').next().unwrap_or("").to_string();
            let kind = if matches!(kind.as_str(), "event" | "timer" | "fetch" | "websocket" | "start") {
                kind
            } else {
                "script".into()
            };
            match by_kind.iter_mut().find(|k| k.0 == kind) {
                Some(k) => {
                    k.1 += 1;
                    k.2 += c.stats.ms;
                    k.3 += c.stats.fuel;
                }
                None => by_kind.push((kind, 1, c.stats.ms, c.stats.fuel)),
            }
        }
        by_kind.sort_by(|a, b| b.2.total_cmp(&a.2));
        let row = |cols: [String; 4], head: bool| {
            format!(
                "<div class=\"dt-req{}\"><span class=\"name\">{}</span><span class=\"ty\">{}</span><span class=\"tm\">{}</span><span class=\"sz\">{}</span></div>",
                if head { " head" } else { "" },
                escape(&cols[0]),
                cols[1],
                cols[2],
                cols[3]
            )
        };
        html.push_str(&row(["Kind".into(), "Calls".into(), "Time".into(), "Instr.".into()], true));
        for (kind, count, ms, fuel) in &by_kind {
            html.push_str(&row([kind.clone(), count.to_string(), format!("{} ms", round(*ms)), count_text(*fuel)], false));
        }
        let mut slow: Vec<&CallRecord> = calls.iter().collect();
        slow.sort_by(|a, b| b.stats.ms.total_cmp(&a.stats.ms));
        html.push_str("<div class=\"dt-h\">Slowest calls</div>");
        html.push_str(&row(["Call".into(), "Script".into(), "Time".into(), "Instr.".into()], true));
        for c in slow.iter().take(15) {
            html.push_str(&row(
                [c.stats.label.clone(), format!("#{}", c.script), format!("{} ms", round(c.stats.ms)), count_text(c.stats.fuel)],
                false,
            ));
        }
        self.set_html("dt-perf", &html);
    }

    /// Marks the active tab, filter and inspect button.
    fn sync_toolbar(&mut self) {
        let d = &self.devtools;
        let filter = match d.filter {
            Filter::All => "all",
            Filter::Warn => "warn",
            Filter::Error => "error",
        };
        let on: Vec<(&str, bool)> = vec![
            ("dt-tab-elements", d.panel == Panel::Elements),
            ("dt-tab-console", d.panel == Panel::Console),
            ("dt-tab-network", d.panel == Panel::Network),
            ("dt-tab-sources", d.panel == Panel::Sources),
            ("dt-tab-application", d.panel == Panel::Application),
            ("dt-tab-performance", d.panel == Panel::Performance),
            ("dt-inspect", d.inspecting),
            ("dt-f-all", filter == "all"),
            ("dt-f-warn", filter == "warn"),
            ("dt-f-error", filter == "error"),
        ];
        for (id, active) in on {
            if let Some(n) = self.chrome_el(id)
                && let Some(e) = self.chrome.dom_mut().element_mut(n)
            {
                e.set_attr("class", if active { "on" } else { "" });
            }
        }
    }

    fn set_html(&mut self, id: &str, html: &str) {
        if let Some(n) = self.chrome_el(id) {
            self.chrome.dom_mut().set_inner_html(n, html);
        }
    }

    fn build_elements(&mut self) {
        let tab = &self.tabs[self.active];
        let (dom, dev) = (&tab.doc.dom, &tab.dev);
        let mut html = String::new();
        let mut rows = 0;
        let mut selected_row = None;
        for &c in dom.children(Dom::ROOT) {
            tree_rows(dom, dev, c, 0, &mut html, &mut rows, &mut selected_row);
        }
        if rows >= MAX_ROWS {
            html.push_str("<div class=\"dt-row dt-comment\">…</div>");
        }
        self.set_html("dt-tree", &html);
        let side = self.side_html();
        self.set_html("dt-side-content", &side);
        // Keep the selection in view after inspecting.
        if std::mem::take(&mut self.devtools.reveal)
            && let (Some(row), Some(tree)) = (selected_row, self.chrome_el("dt-tree"))
        {
            let y = row as f32 * ROW_HEIGHT;
            let view = (self.devtools.height - 60.0).max(ROW_HEIGHT);
            let top = self.chrome.element_scroll(tree);
            if y < top || y + ROW_HEIGHT > top + view {
                self.chrome.scroll_element_to(tree, y - view / 3.0);
            }
        }
    }

    /// The side pane: the selected node's attributes, box model, styles.
    fn side_html(&mut self) -> String {
        let Some(n) = self.tabs[self.active].dev.selected else {
            return "<div class=\"dt-h\">Select an element</div>".into();
        };
        let doc = &mut self.tabs[self.active].doc;
        if doc.dom.get(n).is_none() {
            return String::new();
        }
        if let Some(t) = doc.dom.text(n) {
            return format!(
                "<div class=\"dt-h\">#text</div><div class=\"dt-kv\">{}</div>",
                escape(t)
            );
        }
        let Some(el) = doc.dom.element(n).cloned() else {
            return String::new();
        };
        let mut out = format!("<div class=\"dt-h\">{}</div>", escape(&node_label(&el)));
        out.push_str("<div class=\"dt-h\">Attributes</div><div class=\"dt-kv\">");
        if el.attrs.is_empty() {
            out.push_str("(none)");
        }
        for (k, v) in &el.attrs {
            out.push_str(&format!(
                "<span class=\"dt-an\">{}</span>=<span class=\"dt-av\">\"{}\"</span>\n",
                escape(k),
                escape(v)
            ));
        }
        out.push_str("</div>");
        // Box model (as laid out).
        let px = |doc: &mut crate::document::Document, p: &str| {
            doc.computed_value(n, p)
                .map(|v| v.trim_end_matches("px").to_string())
                .unwrap_or_default()
        };
        let sides = |doc: &mut crate::document::Document, f: &dyn Fn(&str) -> String| {
            ["top", "right", "bottom", "left"]
                .iter()
                .map(|s| px(doc, &f(s)))
                .collect::<Vec<_>>()
                .join(" ")
        };
        if let Some((_, content)) = doc.box_rects(n) {
            let margin = sides(doc, &|s| format!("margin-{s}"));
            let border = sides(doc, &|s| format!("border-{s}-width"));
            let padding = sides(doc, &|s| format!("padding-{s}"));
            out.push_str(&format!(
                "<div class=\"dt-h\">Box model</div><div class=\"dt-boxm\">margin {margin}\
                 <div class=\"b\">border {border}<div class=\"p\">padding {padding}\
                 <div class=\"c\">{} × {}</div></div></div></div>",
                round(content.w),
                round(content.h)
            ));
        } else {
            out.push_str("<div class=\"dt-h\">Box model</div><div class=\"dt-kv\">(not rendered)</div>");
        }
        // Styles: the inline style, then the matching rules.
        out.push_str("<div class=\"dt-h\">Styles</div>");
        // Each declaration has a box that turns it off and on.
        let mut inline: Vec<DeclRow> = declarations(el.attr("style").unwrap_or(""))
            .into_iter()
            .map(|(name, value, important)| DeclRow { name, value, important, on: true, toggle: None })
            .collect();
        let off = self.tabs[self.active].dev.disabled_inline.iter().filter(|(e, ..)| *e == n);
        inline.extend(off.map(|(_, name, value)| DeclRow {
            name: name.clone(),
            value: value.clone(),
            important: false,
            on: false,
            toggle: None,
        }));
        for (k, row) in inline.iter_mut().enumerate() {
            row.toggle = Some(format!("inline:{k}"));
        }
        let doc = &mut self.tabs[self.active].doc;
        if !inline.is_empty() {
            out.push_str(&rule_html("element.style", "", &inline));
        }
        for r in doc.matched_rules(n) {
            let decls: Vec<DeclRow> = r
                .decls
                .iter()
                .enumerate()
                .map(|(k, d)| DeclRow {
                    name: d.name.clone(),
                    value: d.value.clone(),
                    important: d.important,
                    on: !doc.declaration_disabled(r.decls_id, k),
                    toggle: (!r.user_agent).then(|| format!("decl:{}:{k}", r.decls_id)),
                })
                .collect();
            let source = if r.user_agent { "user agent" } else { "" };
            out.push_str(&rule_html(&r.selector, source, &decls));
        }
        // Computed values of the common properties.
        out.push_str("<div class=\"dt-h\">Computed</div><div class=\"dt-kv\">");
        for p in COMPUTED {
            if let Some(v) = doc.computed_value(n, p).filter(|v| !v.is_empty()) {
                out.push_str(&format!(
                    "<span class=\"dt-an\">{p}</span>: {}\n",
                    escape(&v)
                ));
            }
        }
        out.push_str("</div>");
        out
    }

    fn build_console(&mut self) {
        let filter = self.devtools.filter;
        let mut html = String::new();
        for e in &self.tabs[self.active].dev.console {
            let shown = match filter {
                Filter::All => true,
                Filter::Warn => e.level == Level::Warn,
                Filter::Error => e.level == Level::Error,
            };
            if !shown {
                continue;
            }
            let (class, prefix) = match e.level {
                Level::Log => ("", ""),
                Level::Warn => ("warn", ""),
                Level::Error => ("error", ""),
                Level::Command => ("cmd", "› "),
                Level::Result => ("result", "← "),
            };
            let count = if e.count > 1 {
                format!("<span class=\"dt-count\">{}</span>", e.count)
            } else {
                String::new()
            };
            html.push_str(&format!(
                "<div class=\"dt-msg {class}\">{count}{prefix}{}</div>",
                escape(&e.text)
            ));
        }
        self.set_html("dt-log", &html);
        if std::mem::take(&mut self.devtools.stick)
            && let Some(log) = self.chrome_el("dt-log")
        {
            self.chrome.scroll_element_to(log, f32::MAX / 4.0);
        }
    }

    fn build_network(&mut self) {
        let selected = self.devtools.request;
        let entries = &self.tabs[self.active].dev.network;
        let mut html = String::from(
            "<div class=\"dt-req head\"><span class=\"name\">Name</span><span class=\"st\">Status</span>\
             <span class=\"ty\">Type</span><span class=\"sz\">Size</span><span class=\"tm\">Time</span></div>",
        );
        for e in entries {
            let status = match (&e.error, e.status) {
                (Some(_), _) => "(failed)".to_string(),
                (None, Some(s)) => s.to_string(),
                (None, None) => "…".to_string(),
            };
            let failed = e.error.is_some() || e.status.is_some_and(|s| s >= 400);
            let class = match (Some(e.id) == selected, failed) {
                (true, _) => " sel",
                (false, true) => " fail",
                _ => "",
            };
            let time = e
                .end
                .map_or(String::new(), |end| format!("{} ms", round(end - e.start)));
            let size = if e.end.is_some() && e.error.is_none() {
                size_text(e.size)
            } else {
                String::new()
            };
            html.push_str(&format!(
                "<div class=\"dt-req{class}\" data-dt=\"req:{}\"><span class=\"name\">{}</span>\
                 <span class=\"st\">{status}</span><span class=\"ty\">{}</span>\
                 <span class=\"sz\">{size}</span><span class=\"tm\">{time}</span></div>",
                e.id,
                escape(&short_name(&e.url)),
                e.kind
            ));
        }
        let detail = selected
            .and_then(|id| entries.iter().find(|e| e.id == id))
            .map(detail_html);
        self.set_html("dt-requests", &html);
        let detail_shown = detail.is_some();
        self.set_html("dt-detail", &detail.unwrap_or_default());
        if let Some(n) = self.chrome_el("dt-detail")
            && let Some(e) = self.chrome.dom_mut().element_mut(n)
        {
            e.set_attr("class", if detail_shown { "show" } else { "" });
        }
    }

    /// Outlines the inspected element on the page: its margin, border,
    /// padding and content boxes in the usual colors.
    pub(super) fn paint_devtools_overlay(&mut self) {
        if !self.devtools.open && !self.devtools.remote_highlight {
            return;
        }
        let tab = &self.tabs[self.active];
        let node = self.devtools.hover.or(tab.dev.selected.filter(|_| {
            self.devtools.panel == Panel::Elements && !self.devtools.inspecting
        }));
        let Some(n) = node.filter(|&n| tab.doc.dom.element(n).is_some()) else {
            return;
        };
        let doc = &tab.doc;
        let (Some((border, content)), Some(st)) = (doc.box_rects(n), doc.style(n)) else {
            return;
        };
        let base = doc
            .dom
            .parent(n)
            .and_then(|p| doc.box_rects(p))
            .map_or(border.w, |(_, c)| c.w);
        let bw = |i: usize| {
            if st.border_style[i] == crate::css::style::LineStyle::None {
                0.0
            } else {
                st.border_width[i]
            }
        };
        let m = |i: usize| st.margin[i].px_or_zero(base);
        // [top, right, bottom, left] insets
        let grow = |r: crate::layout::RectF, e: [f32; 4]| crate::layout::RectF {
            x: r.x - e[3],
            y: r.y - e[0],
            w: r.w + e[1] + e[3],
            h: r.h + e[0] + e[2],
        };
        let padding_box = grow(border, [-bw(0), -bw(1), -bw(2), -bw(3)]);
        let margin_box = grow(border, [m(0), m(1), m(2), m(3)]);
        let vp = self.viewport_rect();
        let s = self.scale;
        let phys = |r: crate::layout::RectF| {
            (
                ((vp.x + r.x) * s).round() as i32,
                ((vp.y + r.y) * s).round() as i32,
                (r.w * s).round() as i32,
                (r.h * s).round() as i32,
            )
        };
        let clip = crate::gfx::Rect::new(
            (vp.x * s) as i32,
            (vp.y * s) as i32,
            (vp.w * s).ceil() as i32,
            (vp.h * s).ceil() as i32,
        );
        let canvas = &mut self.canvas;
        let saved = canvas.clip();
        canvas.set_clip(clip.intersect(&saved));
        // Each ring: the outer box minus the inner one.
        let mut ring = |outer: crate::layout::RectF, inner: crate::layout::RectF, color: u32| {
            let (ox, oy, ow, oh) = phys(outer);
            let (ix, iy, iw, ih) = phys(inner);
            let alpha = 140;
            canvas.fill_rect_alpha(ox, oy, ow, iy - oy, color, alpha);
            canvas.fill_rect_alpha(ox, iy + ih, ow, oy + oh - iy - ih, color, alpha);
            canvas.fill_rect_alpha(ox, iy, ix - ox, ih, color, alpha);
            canvas.fill_rect_alpha(ix + iw, iy, ox + ow - ix - iw, ih, color, alpha);
        };
        ring(margin_box, border, 0xf6b26b);
        ring(border, padding_box, 0xffe599);
        ring(padding_box, content, 0x93c47d);
        let (x, y, w, h) = phys(content);
        canvas.fill_rect_alpha(x, y, w, h, 0x6fa8dc, 140);
        canvas.set_clip(saved);
    }
}

/// Computed properties listed for the selected element.
pub(super) const COMPUTED: &[&str] = &[
    "display",
    "position",
    "top",
    "left",
    "width",
    "height",
    "box-sizing",
    "margin-top",
    "margin-right",
    "margin-bottom",
    "margin-left",
    "padding-top",
    "padding-right",
    "padding-bottom",
    "padding-left",
    "color",
    "background-color",
    "font-family",
    "font-size",
    "font-weight",
    "line-height",
    "text-align",
    "flex-direction",
    "justify-content",
    "align-items",
    "gap",
    "overflow",
    "opacity",
    "z-index",
    "visibility",
];

fn round(v: impl Into<f64>) -> String {
    let v: f64 = v.into();
    if (v - v.round()).abs() < 0.05 {
        format!("{}", v.round() as i64)
    } else {
        format!("{v:.1}")
    }
}

/// A large count shortened: 12.3M.
fn count_text(n: u64) -> String {
    match n {
        0..1_000 => n.to_string(),
        1_000..1_000_000 => format!("{:.1}k", n as f64 / 1e3),
        1_000_000..1_000_000_000 => format!("{:.1}M", n as f64 / 1e6),
        _ => format!("{:.2}G", n as f64 / 1e9),
    }
}

fn size_text(n: usize) -> String {
    if n < 1024 {
        format!("{n} B")
    } else if n < 1024 * 1024 {
        format!("{:.1} kB", n as f64 / 1024.0)
    } else {
        format!("{:.1} MB", n as f64 / 1048576.0)
    }
}

/// `tag#id.class` of an element.
fn node_label(el: &crate::dom::Element) -> String {
    let mut s = el.name.clone();
    if let Some(id) = el.id() {
        s.push('#');
        s.push_str(id);
    }
    for c in el.classes() {
        s.push('.');
        s.push_str(c);
    }
    s
}

/// A request's name in the list: the last path segment and the query.
fn short_name(url: &str) -> String {
    if url.starts_with("data:") || url.starts_with("blob:") {
        return url.chars().take(60).collect();
    }
    let rest = url.split_once("://").map_or(url, |(_, r)| r);
    let path = rest.split('#').next().unwrap_or(rest);
    let (path, query) = path.split_once('?').map_or((path, ""), |(p, q)| (p, q));
    let last = path.trim_end_matches('/').rsplit('/').next().unwrap_or(path);
    let name = if query.is_empty() {
        last.to_string()
    } else {
        format!("{last}?{query}")
    };
    if name.is_empty() { url.to_string() } else { name }
}

/// Declarations of a `style` attribute.
fn declarations(style: &str) -> Vec<(String, String, bool)> {
    crate::css::syntax::parse_declarations(style, None)
        .into_iter()
        .map(|d| (d.name, d.value, d.important))
        .collect()
}

/// A declaration in the Styles pane.
struct DeclRow {
    name: String,
    value: String,
    important: bool,
    /// Turned on (not crossed out).
    on: bool,
    /// The `data-dt` command of its box (none: it cannot be turned off).
    toggle: Option<String>,
}

fn rule_html(selector: &str, source: &str, decls: &[DeclRow]) -> String {
    let mut out = format!(
        "<div class=\"dt-rule dt-kv\"><span class=\"dt-sel\">{}</span> <span class=\"dt-src\">{}</span>{{\n",
        escape(selector),
        if source.is_empty() { String::new() } else { format!("({source}) ") }
    );
    for d in decls {
        let check = match &d.toggle {
            Some(cmd) => format!(
                "<span class=\"dt-chk\" data-dt=\"{cmd}\">{}</span>",
                if d.on { "☑" } else { "☐" }
            ),
            None => " ".into(),
        };
        out.push_str(&format!(
            "{check} <span class=\"{}\"><span class=\"dt-an\">{}</span>: {}{};</span>\n",
            if d.on { "" } else { "dt-off" },
            escape(&d.name),
            escape(&d.value),
            if d.important { " !important" } else { "" }
        ));
    }
    out.push_str("}</div>");
    out
}

/// Whether a body of `mime` (or that looks like text) is shown as text.
fn textual(mime: &str, body: &[u8]) -> bool {
    let m = mime.to_ascii_lowercase();
    if m.starts_with("text/")
        || ["json", "xml", "javascript", "ecmascript", "css", "x-www-form-urlencoded", "multipart/form-data"]
            .iter()
            .any(|t| m.contains(t))
    {
        return true;
    }
    m.is_empty() && !body.contains(&0) && std::str::from_utf8(body).is_ok()
}

/// A body as text for the panel (binary bytes as a note).
fn body_preview(mime: &str, body: &[u8], size: usize) -> String {
    if body.is_empty() {
        return "(empty)".into();
    }
    if !textual(mime, body) && !mime.to_ascii_lowercase().starts_with("multipart/") {
        return format!("({} of {})", if mime.is_empty() { "binary data" } else { mime }, size_text(size));
    }
    let text = String::from_utf8_lossy(body);
    let mut shown: String = text.chars().take(20_000).collect();
    if shown.len() < text.len() || body.len() < size {
        shown.push_str("\n…");
    }
    shown
}

/// The detail pane of a request.
fn detail_html(e: &NetEntry) -> String {
    let header = |name: &str| {
        e.headers
            .iter()
            .find(|(n, _)| n.eq_ignore_ascii_case(name))
            .map_or("", |(_, v)| v.as_str())
    };
    let status = match (&e.error, e.status) {
        (Some(err), _) => format!("failed: {err}"),
        (None, Some(s)) => s.to_string(),
        (None, None) => "pending".into(),
    };
    let mut out = format!(
        "<div class=\"dt-h\">General</div><div class=\"dt-kv\">URL: {}\nMethod: {}\nStatus: {}\nType: {}\n</div>",
        escape(&e.url),
        escape(&e.method),
        escape(&status),
        e.kind
    );
    if !e.headers.is_empty() {
        out.push_str("<div class=\"dt-h\">Response headers</div><div class=\"dt-kv\">");
        for (n, v) in &e.headers {
            out.push_str(&format!("<span class=\"dt-an\">{}</span>: {}\n", escape(n), escape(v)));
        }
        out.push_str("</div>");
    }
    if !e.request_body.is_empty() || !e.request_type.is_empty() {
        out.push_str(&format!(
            "<div class=\"dt-h\">Request payload</div><div class=\"dt-kv\">Content-Type: {}</div><div class=\"dt-body\">{}</div>",
            escape(&e.request_type),
            escape(&body_preview(&e.request_type, &e.request_body, e.request_body.len()))
        ));
    }
    if e.end.is_some() && e.error.is_none() {
        out.push_str(&format!(
            "<div class=\"dt-h\">Response</div><div class=\"dt-body\">{}</div>",
            escape(&body_preview(header("content-type"), &e.body, e.size))
        ));
    }
    out
}

/// Whether node `n` gets a row in the tree (whitespace-only text does not).
fn shown(dom: &Dom, n: NodeId) -> bool {
    match &dom.node(n).data {
        NodeData::Text(t) => !t.trim().is_empty(),
        _ => true,
    }
}

/// Text collapsed to one line of at most `max` characters.
fn one_line(t: &str, max: usize) -> String {
    let s = t.split_whitespace().collect::<Vec<_>>().join(" ");
    if s.chars().count() > max {
        format!("{}…", s.chars().take(max).collect::<String>())
    } else {
        s
    }
}

/// Rows of node `n` (and its subtree, when expanded) at `depth`.
fn tree_rows(
    dom: &Dom,
    dev: &TabDev,
    n: NodeId,
    depth: usize,
    out: &mut String,
    rows: &mut usize,
    selected_row: &mut Option<usize>,
) {
    if *rows >= MAX_ROWS || !shown(dom, n) {
        return;
    }
    let sel = dev.selected == Some(n);
    if sel {
        *selected_row = Some(*rows);
    }
    *rows += 1;
    let open_row = |out: &mut String, sel: bool| {
        out.push_str(&format!(
            "<div class=\"dt-row{}\" data-dt=\"select:{n}\" style=\"padding-left:{}px\">",
            if sel { " sel" } else { "" },
            4 + depth * 14
        ));
    };
    open_row(out, sel);
    match &dom.node(n).data {
        NodeData::Text(t) => out.push_str(&format!(
            "<span class=\"dt-tw\"> </span>\"<span class=\"dt-text\">{}</span>\"</div>",
            escape(&one_line(t, 200))
        )),
        NodeData::Comment(c) => out.push_str(&format!(
            "<span class=\"dt-tw\"> </span><span class=\"dt-comment\">&lt;!--{}--&gt;</span></div>",
            escape(&one_line(c, 200))
        )),
        NodeData::Document => out.push_str("#document</div>"),
        NodeData::Element(e) => {
            let kids: Vec<NodeId> = dom
                .children(n)
                .iter()
                .copied()
                .filter(|&c| shown(dom, c))
                .collect();
            // A lone short text child is shown on the element's line.
            let inline = match kids[..] {
                [k] => dom.text(k).map(|t| one_line(t, 80)).filter(|t| t.len() <= 80),
                _ => None,
            };
            let expandable = !kids.is_empty() && inline.is_none();
            let open = expandable && dev.expanded.contains(&n);
            let twisty = match (expandable, open) {
                (false, _) => "<span class=\"dt-tw\"> </span>".to_string(),
                (true, o) => format!(
                    "<span class=\"dt-tw\" data-dt=\"toggle:{n}\">{}</span>",
                    if o { "▾" } else { "▸" }
                ),
            };
            out.push_str(&twisty);
            out.push_str(&format!("<span class=\"dt-tag\">&lt;{}</span>", escape(&e.name)));
            for (k, v) in &e.attrs {
                out.push_str(&format!(
                    " <span class=\"dt-an\">{}</span>=<span class=\"dt-av\">\"{}\"</span>",
                    escape(k),
                    escape(&one_line(v, 100))
                ));
            }
            out.push_str("<span class=\"dt-tag\">&gt;</span>");
            let close = format!("<span class=\"dt-tag\">&lt;/{}&gt;</span>", escape(&e.name));
            let void = crate::dom::VOID.contains(&e.name.as_str());
            if let Some(t) = inline {
                out.push_str(&format!("<span class=\"dt-text\">{}</span>{close}", escape(&t)));
            } else if expandable && !open {
                out.push_str(&format!("…{close}"));
            } else if !expandable && !void {
                out.push_str(&close);
            }
            out.push_str("</div>");
            if open {
                for &k in &kids {
                    tree_rows(dom, dev, k, depth + 1, out, rows, selected_row);
                }
                if *rows < MAX_ROWS {
                    *rows += 1;
                    out.push_str(&format!(
                        "<div class=\"dt-row\" data-dt=\"select:{n}\" style=\"padding-left:{}px\"><span class=\"dt-tw\"> </span>{close}</div>",
                        4 + depth * 14
                    ));
                }
            }
        }
    }
}
