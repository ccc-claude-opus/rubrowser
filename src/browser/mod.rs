//! The browser: an HTML/CSS chrome document (tabs, toolbar, address bar)
//! with the active tab's page drawn inside its `#viewport` element.
//!
//! The chrome's behavior lives here in Rust: clicks on elements with a
//! `data-action` attribute run browser commands, and the tab strip and
//! address bar are kept in sync by editing the chrome DOM.

mod cdp;
mod devtools;
mod osk;
mod touch;

pub use osk::KeyboardMode;

use std::collections::HashMap;
use std::rc::Rc;

use crate::abi::key;
use crate::css::ElementState;
use crate::document::{Document, FrameLoad};
use crate::dom::{Element, NodeId};
use crate::gfx::Canvas;
use crate::input::{Cursor, Event, Host, Modifiers, WsEvent};
use crate::layout::RectF;
use crate::net::url;
use crate::script::{EventInfo, Limits, Script, WsCall};
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
    /// Range input whose thumb is being dragged.
    range: Option<NodeId>,
    /// Page text selection started here (a text point and the page point).
    select: Option<(crate::document::selection::TextPoint, f32, f32)>,
    /// The press became a drag that selected text (it is not a click).
    selected: bool,
}

/// The context menu: what was under the pointer when it opened.
#[derive(Clone, Debug, Default)]
struct MenuCtx {
    link: Option<String>,
    image: Option<String>,
    /// The page element it was opened on (for "Inspect").
    node: Option<NodeId>,
    /// The text field it was opened on.
    field: Option<(Target, NodeId)>,
}

/// A script's cross-origin request (Fetch standard, CORS protocol).
#[derive(Clone, Debug)]
struct Cors {
    /// The page's origin (`Origin`).
    origin: String,
    /// Credentials mode `include`: cookies go both ways, and the server
    /// must allow them.
    credentials: bool,
    /// `mode: "no-cors"`: the script gets an opaque response.
    opaque: bool,
    method: String,
    /// Headers that needed the preflight (lowercase).
    requested: Vec<String>,
    /// While the preflight is out: the request to send once it passes.
    actual: Option<crate::input::HttpRequest>,
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
        /// The `<iframe>` whose document it is for (`None`: the page).
        frame: Option<NodeId>,
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
        frame: Option<NodeId>,
        node: Option<NodeId>,
        src: String,
    },
    Font {
        tab: u64,
        serial: u64,
        frame: Option<NodeId>,
        font: crate::document::FontRequest,
    },
    /// The document of `<iframe>` `node` (`source` as the page requested
    /// it).
    Frame {
        tab: u64,
        serial: u64,
        node: NodeId,
        source: String,
        url: String,
    },
    /// An HTTP request from script `script` (its request `id`) to `url`
    /// (the URL after redirects, once known).
    Http {
        tab: u64,
        serial: u64,
        script: usize,
        id: i32,
        url: String,
        /// For a cross-origin request: the CORS protocol's state.
        cors: Option<Box<Cors>>,
    },
    /// A module imported by module script `slot`.
    Module {
        tab: u64,
        serial: u64,
        slot: usize,
        url: String,
    },
    /// A file to save in the download folder (`<a download>`), named
    /// `name` unless the server names it.
    Download {
        tab: u64,
        url: String,
        name: Option<String>,
    },
}

impl Request {
    fn tab(&self) -> u64 {
        match self {
            Request::Page { tab, .. }
            | Request::Stylesheet { tab, .. }
            | Request::Script { tab, .. }
            | Request::Image { tab, .. }
            | Request::Font { tab, .. }
            | Request::Frame { tab, .. }
            | Request::Http { tab, .. }
            | Request::Module { tab, .. }
            | Request::Download { tab, .. } => *tab,
        }
    }
}

/// The status and headers of an HTTP response (header names lowercase).
#[derive(Clone, Debug, Default, PartialEq)]
pub struct ResponseInfo {
    pub status: u16,
    pub headers: Vec<(String, String)>,
}

impl ResponseInfo {
    pub fn header(&self, name: &str) -> Option<&str> {
        self.headers
            .iter()
            .find(|(n, _)| n.eq_ignore_ascii_case(name))
            .map(|(_, v)| v.as_str())
    }
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
    /// A JavaScript module (`<script type=module>`).
    Module,
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
    /// For a module: its import graph, fetched before it runs (`body` is
    /// set once every module has arrived).
    graph: Option<ModuleGraph>,
    /// Its `<script>` element (`document.currentScript`).
    node: Option<NodeId>,
    /// Whether it has `src` or was inserted by a script: it then gets
    /// `load` / `error`.
    events: bool,
    /// Inserted by a script: async, so not limited by the page's loading
    /// budget (only by the per-call one).
    inserted: bool,
}

/// The modules of one `<script type=module>`.
#[derive(Default)]
struct ModuleGraph {
    root: String,
    /// (URL, source) of every module fetched.
    sources: Vec<(String, String)>,
    seen: std::collections::HashSet<String>,
    /// Fetches still outstanding.
    waiting: usize,
}

impl ModuleGraph {
    fn new(root: &str) -> Self {
        ModuleGraph {
            root: root.to_string(),
            seen: std::iter::once(root.to_string()).collect(),
            waiting: 1,
            ..Default::default()
        }
    }
}

/// The page's `<script type=importmap>` entries.
fn import_map(doc: &Document, base: &str) -> Vec<(String, String)> {
    let dom = &doc.dom;
    dom.descendants(crate::dom::Dom::ROOT)
        .into_iter()
        .filter(|&n| {
            dom.tag(n) == Some("script")
                && dom
                    .attr(n, "type")
                    .is_some_and(|t| t.trim().eq_ignore_ascii_case("importmap"))
        })
        .flat_map(|n| parse_import_map(&dom.text_content(n), base))
        .collect()
}

/// With `RUBROWSER_PROFILE` set: the start time of something to time.
fn profiling_now() -> Option<std::time::Instant> {
    #[cfg(not(target_arch = "wasm32"))]
    if std::env::var_os("RUBROWSER_PROFILE").is_some() {
        return Some(std::time::Instant::now());
    }
    None
}

/// `scheme://host[:port]` of an http(s) URL.
fn origin(u: &str) -> Option<String> {
    let (scheme, rest) = u.split_once("://")?;
    if scheme != "http" && scheme != "https" {
        return None;
    }
    let host = rest.split(['/', '?', '#']).next()?;
    Some(format!("{}://{}", scheme, host.to_ascii_lowercase()))
}

/// Fuel all of a page's language-runtime calls may use while it loads
/// (roughly three seconds); after `load` only the per-call limit applies.
const PAGE_LOAD_FUEL: u64 = 8_000_000_000 * crate::script::SPEED;

/// Most modules fetched for one module script.
const MAX_MODULES: usize = 400;

/// Static import specifiers of an ES module (`import … from "x"`,
/// `import "x"`, `export … from "x"`, `import("x")` with a literal). A small
/// lexer: comments, strings, regex literals and nested template literals
/// are skipped so their contents are not mistaken for imports.
fn module_imports(src: &str) -> Vec<String> {
    let b = src.as_bytes();
    let mut out = Vec::new();
    let mut i = 0;
    // The last word seen outside strings and comments, and whether an
    // `import(` call is open.
    let mut last_word = String::new();
    let mut call = false;
    // The last significant character, to tell a regex literal from `/`.
    let mut prev = b'(';
    // Brace depth, and the depths at which template `${…}` parts began.
    let mut depth = 0usize;
    let mut templates: Vec<usize> = Vec::new();
    // Skips template text from `i` to its end or to `${`; returns true at `${`.
    let skip_template = |i: &mut usize| -> bool {
        while *i < b.len() {
            match b[*i] {
                b'\\' => *i += 2,
                b'`' => {
                    *i += 1;
                    return false;
                }
                b'$' if b.get(*i + 1) == Some(&b'{') => {
                    *i += 2;
                    return true;
                }
                _ => *i += 1,
            }
        }
        false
    };
    while i < b.len() {
        let c = b[i];
        let regex_can_start = b"(,=:[!&|?{};+-*%~^<>".contains(&prev)
            || matches!(
                last_word.as_str(),
                "return" | "typeof" | "case" | "do" | "else" | "in" | "of"
            );
        if c == b'/' && b.get(i + 1) == Some(&b'/') {
            while i < b.len() && b[i] != b'\n' {
                i += 1;
            }
        } else if c == b'/' && b.get(i + 1) == Some(&b'*') {
            i += 2;
            while i + 1 < b.len() && !(b[i] == b'*' && b[i + 1] == b'/') {
                i += 1;
            }
            i += 2;
        } else if c == b'/' && regex_can_start {
            // A regex literal: skip to its closing slash.
            i += 1;
            let mut class = false;
            while i < b.len() && b[i] != b'\n' && (class || b[i] != b'/') {
                match b[i] {
                    b'\\' => i += 1,
                    b'[' => class = true,
                    b']' => class = false,
                    _ => {}
                }
                i += 1;
            }
            i += 1;
            prev = b'a';
            last_word.clear();
        } else if c == b'`' {
            i += 1;
            if skip_template(&mut i) {
                templates.push(depth);
                depth += 1;
                prev = b'{';
            } else {
                prev = b'a';
            }
            last_word.clear();
            call = false;
        } else if c == b'"' || c == b'\'' {
            let start = i + 1;
            i += 1;
            while i < b.len() && b[i] != c && b[i] != b'\n' {
                if b[i] == b'\\' {
                    i += 1;
                }
                i += 1;
            }
            let lit = &src[start.min(src.len())..i.min(src.len())];
            if last_word == "from" || last_word == "import" || call {
                out.push(lit.to_string());
            }
            call = false;
            last_word.clear();
            prev = b'a';
            i += 1;
        } else if c.is_ascii_alphanumeric() || c == b'_' || c == b'$' {
            let start = i;
            while i < b.len() && (b[i].is_ascii_alphanumeric() || b[i] == b'_' || b[i] == b'$') {
                i += 1;
            }
            last_word = src[start..i].to_string();
            call = false;
            prev = b'a';
        } else {
            match c {
                b'{' => depth += 1,
                b'}' => {
                    depth = depth.saturating_sub(1);
                    if templates.last() == Some(&depth) {
                        // The end of a `${…}`: back inside the template.
                        templates.pop();
                        i += 1;
                        if skip_template(&mut i) {
                            templates.push(depth);
                            depth += 1;
                        }
                        prev = b'a';
                        last_word.clear();
                        continue;
                    }
                }
                _ => {}
            }
            if c == b'(' {
                call = last_word == "import";
            } else if !c.is_ascii_whitespace() {
                call = false;
            }
            if !c.is_ascii_whitespace() {
                last_word.clear();
                prev = c;
            }
            i += 1;
        }
    }
    out
}

/// `"imports"` of a `<script type=importmap>` (a flat JSON object of
/// strings), with URLs resolved against `base`.
fn parse_import_map(json: &str, base: &str) -> Vec<(String, String)> {
    let Some(start) = json.find("\"imports\"") else {
        return Vec::new();
    };
    let rest = &json[start + 9..];
    let Some(open) = rest.find('{') else {
        return Vec::new();
    };
    let body = &rest[open + 1..rest[open..].find('}').map_or(rest.len(), |e| open + e)];
    let strings: Vec<&str> = body.split('"').skip(1).step_by(2).collect();
    strings
        .chunks(2)
        .filter(|kv| kv.len() == 2)
        .map(|kv| (kv[0].to_string(), url::resolve(base, kv[1])))
        .collect()
}

/// The JavaScript runtime module (boa compiled to wasm).
const JS_RUNTIME: &[u8] = include_bytes!("runtimes/js.wasm");

/// Time between video frames while media plays (ms).
const MEDIA_FRAME_MS: f64 = 1000.0 / 60.0;

/// Extra fields of a dispatched event.
#[derive(Clone, Debug, Default)]
struct EventInit {
    /// Only at the target (`load` of a script or stylesheet).
    local: bool,
    key: i32,
    mods: i32,
    x: f32,
    y: f32,
    button: i32,
    /// A key held down (auto-repeat).
    repeat: bool,
    /// Wheel deltas in pixels.
    delta: (f32, f32),
    /// Touch data (see [`crate::script::EventInfo::touch`]).
    touch: String,
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

/// The longest a page waits for its render-blocking stylesheets before it
/// is shown anyway (a slow or stuck server should not hide it forever).
const BLOCKING_MS: f64 = 3000.0;

struct Tab {
    id: u64,
    history: Vec<String>,
    /// The page's scroll position when each history entry was left, to
    /// restore it on return (HTML's scroll restoration, mode `auto`).
    scrolls: Vec<f32>,
    index: usize,
    doc: Document,
    /// Increases with every navigation, to drop stale fetch results.
    serial: u64,
    /// Increases with every document shown in the tab.
    page: u64,
    loading: bool,
    /// The page's wasm scripts (`None` once a script has crashed).
    scripts: Vec<Option<Script>>,
    timers: Vec<Timer>,
    /// Scripts not yet run, in document order.
    pending: std::collections::VecDeque<PendingScript>,
    /// Index in `scripts` of the page's runtime for each script MIME type.
    runtimes: HashMap<String, usize>,
    /// Number of script slots given out (the page's, then inserted ones).
    script_count: usize,
    /// Number of slots that have left the queue (run or skipped): pending
    /// script `slot` is at index `slot - consumed`.
    consumed: usize,
    /// The page's scripts are being queued: nothing runs until all are.
    queuing: bool,
    /// Whether `DOMContentLoaded` / `load` have fired.
    loaded: bool,
    /// The page's import map (bare specifier or prefix → URL).
    import_map: Vec<(String, String)>,
    /// Whether the JavaScript runtime has the import map.
    import_map_sent: bool,
    /// `doc.dom.version()` when scripts last finished, to restyle only
    /// after real changes.
    dom_version: u64,
    /// Method and body for the next page load (a form POST).
    post: Option<crate::input::HttpRequest>,
    /// `<script>` and stylesheet `<link>` elements already handled; others
    /// were inserted by scripts.
    known: std::collections::HashSet<NodeId>,
    /// Stylesheet `load` / `error` events waiting for `DOMContentLoaded`
    /// (so inline handlers are registered by then).
    link_events: Vec<(NodeId, &'static str)>,
    /// Scripts inserted by scripts are queued: run them on the next update.
    kick: bool,
    /// The developer tools' console, network log and DOM tree state.
    dev: devtools::TabDev,
    /// `sessionStorage` areas by origin (kept across navigation).
    session_storage: HashMap<String, crate::net::storage::Shared>,
    /// The page's own stylesheets (`<link rel=stylesheet>` in the parsed
    /// document) still loading, and since when: they are render-blocking
    /// (HTML), so the page is not shown unstyled meanwhile.
    blocking: (std::collections::HashSet<NodeId>, f64),
}

impl Tab {
    /// The page's document, or that of its `<iframe>` `frame`.
    fn doc_at(&mut self, frame: Option<NodeId>) -> Option<&mut Document> {
        match frame {
            None => Some(&mut self.doc),
            Some(n) => self.doc.frame_mut(n),
        }
    }

    fn url(&self) -> &str {
        &self.history[self.index]
    }

    /// Remembers where the shown page is scrolled, for its history entry.
    fn save_scroll(&mut self) {
        if !self.loading && self.doc.url == self.history[self.index] {
            self.scrolls[self.index] = self.doc.scroll_y();
        }
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
    /// When playing media last updated (see [`Browser::media_due`]).
    media_drawn: f64,
    /// Whether web pages may run scripts (built-in pages always can).
    javascript: bool,
    /// Shared with page documents, which lay out on demand for scripts.
    fonts: Rc<Fonts>,
    canvas: Canvas,
    scale: f32,
    chrome: Document,
    tabs: Vec<Tab>,
    active: usize,
    next_id: u32,
    /// An app installer (ruwebos's): pages that declare an app package
    /// (`<link rel="ruwebos-app">`) get an Install button opening
    /// `<installer>install?src=<page>`; the menu adds the page to its favorites.
    installer: Option<String>,
    /// Whether the active page declares a package, for (tab, page, DOM version).
    offers_app: ((u64, u64, u64), bool),
    /// Open WebSockets: connection id → (tab id, script slot, the script's id).
    sockets: HashMap<u32, (u64, usize, i32)>,
    next_tab: u64,
    requests: HashMap<u32, Request>,
    keyboard: Target,
    pointer: (f32, f32),
    /// The (tab, document) the pointer's hover was last worked out for.
    hovered: (u64, u64),
    press: Option<Press>,
    /// Fingers on the screen, and the page sliding on after a drag (touch.rs).
    fingers: Vec<touch::Finger>,
    fling: Option<touch::Fling>,
    /// The mouse events being handled come from a tap (no pointer events).
    from_touch: bool,
    /// The last pointer input was a finger (the on-screen keyboard's cue).
    touch_used: bool,
    /// The on-screen keyboard (osk.rs); its keys are being typed; the focused
    /// field must be scrolled into sight (the keyboard just came up).
    osk: osk::Osk,
    osk_typing: bool,
    reveal_focus: bool,
    /// The page's viewport size last laid out, to tell it about a `resize`.
    shown_size: (u64, u64, f32, f32),
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
    /// The developer tools panel (see `devtools.rs`).
    devtools: devtools::DevTools,
    /// Remote debugging connections (Chrome DevTools Protocol).
    cdp: cdp::Cdp,
    /// `localStorage` areas by origin, loaded from the host when first used.
    local_storage: HashMap<String, crate::net::storage::Shared>,
    /// The last page mouse press (point and time), for double-clicking a word.
    last_down: Option<(f32, f32, f64)>,
    /// Where the pointer was when page mouse events last went out.
    last_move: (f32, f32),
    /// The key being handled is an auto-repeat.
    key_repeat: bool,
    /// The open context menu.
    menu: Option<MenuCtx>,
    /// Status and headers of responses not delivered yet (by fetch id).
    responses: HashMap<u32, ResponseInfo>,
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
            media_drawn: 0.0,
            fonts: Rc::new(fonts),
            canvas: Canvas::new(width, height),
            scale: sanitize_scale(scale),
            chrome: Document::from_html(CHROME_HTML, "about:chrome"),
            tabs: Vec::new(),
            active: 0,
            next_id: 1,
            installer: None,
            offers_app: ((0, 0, 0), false),
            sockets: HashMap::new(),
            next_tab: 1,
            requests: HashMap::new(),
            keyboard: Target::Page,
            pointer: (-1.0, -1.0),
            hovered: (0, 0),
            press: None,
            fingers: Vec::new(),
            fling: None,
            from_touch: false,
            touch_used: false,
            osk: Default::default(),
            osk_typing: false,
            reveal_focus: false,
            shown_size: (0, 0, 0.0, 0.0),
            mods: Modifiers::default(),
            caret_epoch: 0.0,
            now: 0.0,
            dirty: true,
            shown_tabs: Vec::new(),
            local: Vec::new(),
            last_click: None,
            devtools: Default::default(),
            cdp: Default::default(),
            local_storage: HashMap::new(),
            last_down: None,
            last_move: (-1.0, -1.0),
            key_repeat: false,
            menu: None,
            javascript: true,
            responses: HashMap::new(),
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
            scrolls: vec![0.0],
            index: 0,
            doc: Document::from_html("", url),
            serial: 0,
            page: 0,
            loading: false,
            scripts: Vec::new(),
            timers: Vec::new(),
            pending: Default::default(),
            import_map: Vec::new(),
            import_map_sent: false,
            dom_version: 0,
            post: None,
            runtimes: HashMap::new(),
            script_count: 0,
            consumed: 0,
            queuing: false,
            loaded: false,
            known: Default::default(),
            link_events: Vec::new(),
            kick: false,
            dev: Default::default(),
            session_storage: HashMap::new(),
            blocking: (std::collections::HashSet::new(), 0.0),
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
        self.close_sockets(self.tabs[i].id, host);
        self.tabs.remove(i);
        if self.tabs.is_empty() {
            self.new_tab("about:home", host);
            return;
        }
        if self.active > i || self.active >= self.tabs.len() {
            self.active = self.active.saturating_sub(1);
        }
        self.tab_switched();
    }

    /// Another tab became the active one: the address bar shows its URL
    /// (dropping what was being typed for the old tab) and keys go to it.
    fn tab_switched(&mut self) {
        if let Some(a) = self.chrome_el("address") {
            let url = self.tabs[self.active].url().to_string();
            if self.chrome.state().focus == Some(a) {
                self.set_focus(Target::Chrome, None);
            }
            self.chrome.set_input_value(a, &url);
        }
        self.keyboard = Target::Page;
        self.dirty = true;
    }

    /// Navigates the active tab to `url` (already resolved), adding it to
    /// the history.
    pub fn navigate(&mut self, url: &str, host: &mut dyn Host) {
        if url.is_empty() {
            return;
        }
        let tab = &mut self.tabs[self.active];
        tab.save_scroll();
        tab.history.truncate(tab.index + 1);
        tab.scrolls.truncate(tab.index + 1);
        tab.history.push(url.to_string());
        tab.scrolls.push(0.0);
        tab.index += 1;
        let before = tab.doc.url.clone();
        if self.same_document(self.active) {
            self.tabs[self.active].post = None;
            self.fragment_changed(&before, false, host);
        } else {
            self.load(self.active, host);
        }
        self.keyboard = Target::Page;
        self.set_focus(Target::Chrome, None);
    }

    fn go(&mut self, delta: isize, host: &mut dyn Host) {
        let tab = &mut self.tabs[self.active];
        let next = tab.index as isize + delta;
        if next >= 0 && (next as usize) < tab.history.len() {
            tab.save_scroll();
            tab.index = next as usize;
            let before = tab.doc.url.clone();
            if self.same_document(self.active) {
                let tab = &mut self.tabs[self.active];
                tab.doc.restore_scroll(tab.scrolls[tab.index]);
                self.fragment_changed(&before, true, host);
            } else {
                self.load(self.active, host);
            }
        }
    }

    /// The active page moved to another fragment of itself (a `#…` link,
    /// `location.hash = …`, back / forward): it hears `popstate` (when going
    /// through history) and `hashchange` if the fragment differs.
    fn fragment_changed(&mut self, before: &str, traversal: bool, host: &mut dyn Host) {
        let after = self.tabs[self.active].doc.url.clone();
        let frag = |u: &str| u.split_once('#').map_or(String::new(), |(_, f)| f.to_string());
        if traversal {
            self.dispatch(crate::dom::Dom::ROOT, "popstate", EventInit::default(), host);
        }
        if frag(before) != frag(&after) {
            self.dispatch(crate::dom::Dom::ROOT, "hashchange", EventInit::default(), host);
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

    /// Reloads the active tab, keeping its scroll position.
    fn reload(&mut self, host: &mut dyn Host) {
        self.tabs[self.active].save_scroll();
        self.load(self.active, host);
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
        let post = tab.post.take();
        let req = Request::Page {
            tab: tab.id,
            serial: tab.serial,
            url: url.clone(),
        };
        match post {
            Some(p) if url.starts_with("http") => {
                let id = self.next_id;
                self.next_id += 1;
                self.requests.insert(id, req);
                self.net_begin(id, &url, Some(&p), host);
                host.fetch_with(id, &url, &p);
            }
            _ => self.fetch(req, &url, host),
        }
    }

    /// Starts loading `url` for `req`; built-in `about:` files complete on
    /// the next update.
    fn fetch(&mut self, req: Request, url: &str, host: &mut dyn Host) {
        let id = self.next_id;
        self.next_id += 1;
        self.requests.insert(id, req);
        self.net_begin(id, url, None, host);
        if let Some(name) = url.strip_prefix("about:") {
            let result = builtin(name)
                .map(<[u8]>::to_vec)
                .ok_or_else(|| format!("{url} does not exist"));
            self.local.push((id, result));
        } else if url.starts_with("data:") {
            let result = url::decode_data_url(url).ok_or_else(|| "bad data: URL".to_string());
            self.local.push((id, result));
        } else if url.starts_with("blob:") {
            let result = self.blob(self.requests[&id].tab(), url);
            if let Ok(f) = &result {
                self.responses.insert(id, blob_response(f));
            }
            self.local
                .push((id, result.map(|f| f.bytes.to_vec()).map_err(String::from)));
        } else {
            // Resources of a page carry its URL as the referrer (pages
            // themselves are opened without one).
            let (referrer, fetch_site) = match self.requests[&id] {
                Request::Page { .. } => (String::new(), String::new()),
                ref r => self
                    .tabs
                    .iter()
                    .find(|t| t.id == r.tab())
                    .map_or_else(Default::default, |t| (url::referrer(&t.doc.url, url), url::fetch_site(&t.doc.url, url))),
            };
            if referrer.is_empty() && fetch_site.is_empty() {
                host.fetch(id, url);
            } else {
                host.fetch_with(id, url, &crate::input::HttpRequest { referrer, fetch_site, ..Default::default() });
            }
        }
    }

    /// The contents of `blob:` URL `url` created in tab `tab` (blob URLs
    /// belong to the document that made them).
    fn blob(&self, tab: u64, url: &str) -> Result<crate::document::FileData, &'static str> {
        let url = url.split('#').next().unwrap_or(url);
        self.tab_index(tab)
            .and_then(|i| self.tabs[i].doc.blobs.get(url).cloned())
            .ok_or("blob: URL not found (revoked, or made by another page)")
    }

    fn set_page(&mut self, i: usize, html: &str, url: &str, host: &mut dyn Host) {
        let vp = self.viewport_rect();
        // The old document's connections go with it.
        self.close_sockets(self.tabs[i].id, host);
        let tab = &mut self.tabs[i];
        tab.loading = false;
        tab.page += 1;
        tab.doc = Document::from_html(html, url);
        if tab.scrolls[tab.index] > 0.0 {
            tab.doc.restore_scroll(tab.scrolls[tab.index]);
        }
        self.attach_storage(i, host);
        let tab = &mut self.tabs[i];
        // Scripts see the viewport (`matchMedia`, `innerWidth`) from the start.
        tab.doc.set_viewport(vp.w, vp.h);
        tab.doc.set_fonts(self.fonts.clone());
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
                .take_while(|(l, ..)| matches!(l, Lang::Runtime(_)))
                .count();
            scripts.insert(at, (Lang::Source(JS_MIME.to_string()), None, code, None));
        }
        // Module scripts, and classic ones with `src` and `defer`, run after
        // the others (HTML: deferred until the document is parsed). Async
        // ones run whenever they are ready, in no set order: here after all
        // of those, the page's own (same origin) before other sites'.
        let dom = &tab.doc.dom;
        let page_origin = origin(url);
        scripts.sort_by_key(|(lang, src, _, node)| {
            let has = |a: &str| node.is_some_and(|n| dom.attr(n, a).is_some());
            let external = src.is_some() || *lang == Lang::Module;
            if external && has("async") {
                let foreign = src
                    .as_deref()
                    .is_some_and(|s| origin(&url::resolve(url, s)) != page_origin);
                2 + foreign as u8
            } else {
                (*lang == Lang::Module || (src.is_some() && has("defer"))) as u8
            }
        });
        tab.script_count = scripts.len();
        tab.consumed = 0;
        tab.queuing = true;
        tab.loaded = false;
        tab.link_events.clear();
        tab.known = tab
            .doc
            .dom
            .descendants(crate::dom::Dom::ROOT)
            .into_iter()
            .filter(|&n| tab.doc.dom.tag(n) == Some("script"))
            .chain(links.iter().map(|&(n, _)| n))
            .collect();
        self.tabs[i].blocking = (links.iter().map(|&(n, _)| n).collect(), self.now);
        for (link, href) in links {
            let target = url::resolve(url, &href);
            let req = Request::Stylesheet {
                tab: id,
                serial,
                frame: None,
                link: Some(link),
                url: target.clone(),
            };
            self.fetch(req, &target, host);
        }
        self.tabs[i].import_map = import_map(&self.tabs[i].doc, url);
        // Scripts are fetched in parallel but run in document order.
        for (slot, (lang, src, inline, node)) in scripts.into_iter().enumerate() {
            let tab = &mut self.tabs[i];
            let module = lang == Lang::Module;
            match src {
                Some(src) => {
                    let target = url::resolve(url, &src);
                    tab.pending.push_back(PendingScript {
                        lang,
                        name: target.clone(),
                        body: None,
                        graph: module.then(|| ModuleGraph::new(&target)),
                        node,
                        events: true,
                        inserted: false,
                    });
                    let req = Request::Script {
                        tab: id,
                        serial,
                        slot,
                    };
                    self.fetch(req, &target, host);
                }
                None if module => {
                    let root = format!("{url}#inline-module-{slot}");
                    tab.pending.push_back(PendingScript {
                        lang,
                        name: format!("{url} (inline module)"),
                        body: None,
                        graph: Some(ModuleGraph::new(&root)),
                        node,
                        events: false,
                        inserted: false,
                    });
                    self.module_loaded(i, slot, &root, Ok(inline.into_bytes()), host);
                }
                None => tab.pending.push_back(PendingScript {
                    lang,
                    name: format!("{url} (inline script)"),
                    body: Some(Ok(inline.into_bytes())),
                    graph: None,
                    node,
                    events: false,
                    inserted: false,
                }),
            }
        }
        self.cdp_navigated(i, host);
        self.tabs[i].queuing = false;
        self.run_scripts(i, host);
        self.dirty = true;
    }

    /// Gives tab `i`'s new document its origin's storage areas.
    fn attach_storage(&mut self, i: usize, host: &mut dyn Host) {
        use crate::net::storage::{self, Area};
        let origin = storage::origin_key(&self.tabs[i].doc.url);
        let local = self
            .local_storage
            .entry(origin.clone())
            .or_insert_with(|| {
                let saved = host.load_storage(&origin).map(|t| Area::decode(&t));
                Rc::new(std::cell::RefCell::new(saved.unwrap_or_default()))
            })
            .clone();
        let tab = &mut self.tabs[i];
        let session = tab.session_storage.entry(origin).or_default().clone();
        tab.doc.local_storage = Some(local);
        tab.doc.session_storage = Some(session);
    }

    /// Saves `localStorage` areas scripts changed.
    fn save_storage(&mut self, host: &mut dyn Host) {
        for (origin, area) in &self.local_storage {
            let mut a = area.borrow_mut();
            if a.dirty {
                a.dirty = false;
                host.save_storage(origin, &a.encode());
            }
        }
    }

    /// Runs pending scripts of tab `i` from the front while they are ready,
    /// then fires the load events once all have run.
    fn run_scripts(&mut self, i: usize, host: &mut dyn Host) {
        if self.tabs[i].queuing {
            return;
        }
        loop {
            let tab = &mut self.tabs[i];
            if tab.pending.is_empty() {
                if !tab.loaded {
                    tab.loaded = true;
                    // After loading, only the per-call limit applies.
                    for rt in tab.scripts.iter_mut().flatten() {
                        rt.set_budget(None);
                    }
                    self.fire_load(i, host);
                    self.cdp_loaded(i, host);
                }
                return;
            }
            // Once the page's scripts used up their loading budget, the rest
            // are skipped so the page shows without waiting for them.
            let exhausted = tab
                .runtimes
                .get(JS_MIME)
                .and_then(|&si| tab.scripts.get(si))
                .and_then(|rt| rt.as_ref()?.budget())
                .is_some_and(|b| b < PAGE_LOAD_FUEL / 100);
            if exhausted
                && matches!(
                    tab.pending.front().map(|p| &p.lang),
                    Some(Lang::Module | Lang::Source(_))
                )
            {
                let skipped = tab.pending.len();
                tab.pending.clear();
                tab.consumed += skipped;
                host.log(&format!(
                    "skipped {skipped} scripts: the page's script time budget is used up"
                ));
                continue;
            }
            if !tab.pending.front().is_some_and(|p| p.body.is_some()) {
                return;
            }
            let p = tab.pending.pop_front().unwrap();
            tab.consumed += 1;
            // Later slots shift down as the front is consumed.
            let body = match p.body.unwrap() {
                Ok(b) => b,
                Err(e) => {
                    host.log(&format!("{}: {e}", p.name));
                    if let Some(node) = p.node.filter(|_| p.events) {
                        self.element_event(i, node, "error", host);
                    }
                    continue;
                }
            };
            // Inserted scripts run outside the loading budget.
            let saved = p.inserted.then(|| self.js_budget(i, None)).flatten();
            match p.lang {
                Lang::Module => self.run_module(i, p.graph.unwrap_or_default(), host),
                Lang::Wasm => self.start_wasm(i, &body, &p.name, host),
                Lang::Runtime(mime) => self.start_runtime(i, &mime, &body, &p.name, host),
                Lang::Source(mime) => {
                    let source = String::from_utf8_lossy(&body).into_owned();
                    let current = p.node.filter(|_| mime == JS_MIME);
                    if let Some(n) = current {
                        let call = format!("__rb_current({n}, {})", p.inserted);
                        self.run_source(i, JS_MIME, &call, "rb", host);
                    }
                    self.run_source(i, &mime, &source, &p.name, host);
                    if current.is_some() && i < self.tabs.len() {
                        self.run_source(i, JS_MIME, "__rb_current(-1)", "rb", host);
                    }
                }
            }
            if i >= self.tabs.len() {
                return;
            }
            if let Some(budget) = saved
                && !self.tabs[i].loaded
            {
                self.js_budget(i, Some(budget));
            }
            if let Some(node) = p.node.filter(|_| p.events) {
                self.element_event(i, node, "load", host);
            }
        }
    }

    /// Module `url` of module script `slot` arrived: records it, fetches
    /// the modules it imports, and marks the script ready once its whole
    /// graph is in.
    fn module_loaded(
        &mut self,
        i: usize,
        slot: usize,
        url: &str,
        result: Result<Vec<u8>, String>,
        host: &mut dyn Host,
    ) {
        let (id, serial) = (self.tabs[i].id, self.tabs[i].serial);
        let tab = &mut self.tabs[i];
        let map = tab.import_map.clone();
        let index = slot.checked_sub(tab.consumed);
        let Some(p) = index.and_then(|k| tab.pending.get_mut(k)) else {
            return;
        };
        let Some(g) = p.graph.as_mut() else { return };
        g.waiting = g.waiting.saturating_sub(1);
        let mut fetches = Vec::new();
        match result {
            Ok(bytes) => {
                let src = String::from_utf8_lossy(&bytes).into_owned();
                for spec in module_imports(&src) {
                    let dep = crate::abi::resolve_module(&spec, url, &map);
                    if g.seen.len() < MAX_MODULES && g.seen.insert(dep.clone()) {
                        g.waiting += 1;
                        fetches.push(dep);
                    }
                }
                g.sources.push((url.to_string(), src));
            }
            Err(e) => host.log(&format!("{url}: {e}")),
        }
        if g.waiting == 0 {
            p.body = Some(Ok(Vec::new()));
        }
        for dep in fetches {
            let req = Request::Module {
                tab: id,
                serial,
                slot,
                url: dep.clone(),
            };
            self.fetch(req, &dep, host);
        }
        self.run_scripts(i, host);
    }

    /// Hands a fetched module graph to the JavaScript runtime and runs its
    /// root (see the module protocol in doc/javascript.md).
    fn run_module(&mut self, i: usize, graph: ModuleGraph, host: &mut dyn Host) {
        let tab = &mut self.tabs[i];
        if !tab.import_map_sent && !tab.import_map.is_empty() {
            tab.import_map_sent = true;
            let text: String = tab
                .import_map
                .iter()
                .map(|(k, v)| format!("{k}\t{v}\n"))
                .collect();
            self.run_source(i, JS_MIME, &text, "rb-import-map", host);
        }
        for (url, src) in &graph.sources {
            self.run_source(i, JS_MIME, src, &format!("rb-define-module:{url}"), host);
        }
        self.run_source(i, JS_MIME, "", &format!("rb-module:{}", graph.root), host);
    }

    /// Sets the loading budget of tab `i`'s JavaScript runtime; returns the
    /// previous one.
    fn js_budget(&mut self, i: usize, budget: Option<u64>) -> Option<u64> {
        let tab = &mut self.tabs[i];
        let rt = tab
            .runtimes
            .get(JS_MIME)
            .and_then(|&si| tab.scripts.get_mut(si))?;
        let rt = rt.as_mut()?;
        let old = rt.budget();
        rt.set_budget(budget);
        old
    }

    /// Fires `load` / `error` at a script or stylesheet element of tab `i`
    /// (not bubbling).
    fn element_event(&mut self, i: usize, node: NodeId, kind: &str, host: &mut dyn Host) {
        if i == self.active && !self.tabs[i].scripts.is_empty() {
            let init = EventInit {
                local: true,
                ..Default::default()
            };
            self.dispatch(node, kind, init, host);
        }
    }

    /// Starts the `<script>` and stylesheet `<link>` elements that scripts
    /// inserted into tab `i`. Scripts are queued after the page's own and
    /// get `load` / `error` once run; `innerHTML` scripts never run.
    fn adopt_inserted(&mut self, i: usize, host: &mut dyn Host) {
        let tab = &mut self.tabs[i];
        let (id, serial) = (tab.id, tab.serial);
        let base = tab.doc.base_url();
        let dom = &tab.doc.dom;
        let mut links = Vec::new();
        let mut scripts = Vec::new();
        for n in dom.descendants(crate::dom::Dom::ROOT) {
            if tab.known.contains(&n) || dom.inert_scripts.contains(&n) {
                continue;
            }
            if let Some(entry) = script_entry(dom, n) {
                // A script without `src` or text yet waits for them.
                tab.known.insert(n);
                scripts.push((n, entry));
            } else if tab.doc.loads_sheet(n)
                && let Some(href) = dom.attr(n, "href")
            {
                tab.known.insert(n);
                links.push((n, url::resolve(&base, href)));
            }
        }
        for (link, target) in links {
            let req = Request::Stylesheet {
                tab: id,
                serial,
                frame: None,
                link: Some(link),
                url: target.clone(),
            };
            self.fetch(req, &target, host);
        }
        for (node, (lang, src, inline)) in scripts {
            if !matches!(lang, Lang::Module | Lang::Source(_)) {
                continue;
            }
            let tab = &mut self.tabs[i];
            let slot = tab.script_count;
            tab.script_count += 1;
            let module = lang == Lang::Module;
            tab.kick = true;
            match src {
                Some(src) => {
                    let target = url::resolve(&base, &src);
                    tab.pending.push_back(PendingScript {
                        lang,
                        name: target.clone(),
                        body: None,
                        graph: module.then(|| ModuleGraph::new(&target)),
                        node: Some(node),
                        events: true,
                        inserted: true,
                    });
                    let req = Request::Script {
                        tab: id,
                        serial,
                        slot,
                    };
                    self.fetch(req, &target, host);
                }
                None if module => {
                    let root = format!("{base}#inserted-module-{slot}");
                    tab.pending.push_back(PendingScript {
                        lang,
                        name: format!("{base} (inserted module)"),
                        body: None,
                        graph: Some(ModuleGraph::new(&root)),
                        node: Some(node),
                        events: false,
                        inserted: true,
                    });
                    self.module_loaded(i, slot, &root, Ok(inline.into_bytes()), host);
                }
                None => tab.pending.push_back(PendingScript {
                    lang,
                    name: format!("{base} (inserted script)"),
                    body: Some(Ok(inline.into_bytes())),
                    graph: None,
                    node: Some(node),
                    events: false,
                    inserted: true,
                }),
            }
        }
    }

    /// `DOMContentLoaded` at the document, then `load` at the body.
    fn fire_load(&mut self, i: usize, host: &mut dyn Host) {
        if i != self.active || self.tabs[i].scripts.is_empty() {
            return;
        }
        for (node, kind) in std::mem::take(&mut self.tabs[i].link_events) {
            self.element_event(i, node, kind, host);
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
            Ok(mut rt) if rt.is_runtime() => {
                if !t.loaded {
                    rt.set_budget(Some(PAGE_LOAD_FUEL));
                }
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

    /// Runs JavaScript `source` in the active page, as a classic script
    /// would (a debugging aid: `js …` in `RUBROWSER_INPUT`).
    pub fn run_script(&mut self, source: &str, host: &mut dyn Host) {
        self.run_source(self.active, "text/javascript", source, "debug script", host);
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
                            "text/template"
                                | "text/html"
                                | "text/plain"
                                | "text/x-template"
                                | "speculationrules"
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
        let started = profiling_now();
        let result = rt.eval(&mut t.doc, source, name, now);
        if let Some(t0) = started {
            eprintln!(
                "[profile] eval {:.0} ms, budget left {:?}: {}",
                t0.elapsed().as_secs_f64() * 1000.0,
                rt.budget(),
                name.chars().take(100).collect::<String>()
            );
        }
        match result {
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

    /// Sets the app installer (see [`Browser::installer`]'s field docs); the URL ends with `/`.
    pub fn set_installer(&mut self, url: Option<String>) {
        self.installer = url.map(|u| if u.ends_with('/') { u } else { format!("{u}/") });
        self.dirty = true;
    }

    /// Whether the active page declares an app package to install.
    fn page_offers_app(&mut self) -> bool {
        let t = &self.tabs[self.active];
        let key = (t.id, t.page, t.doc.dom.version());
        if self.offers_app.0 != key {
            let dom = &t.doc.dom;
            // The installer downloads the package: only for web pages.
            let web = t.url().starts_with("http://") || t.url().starts_with("https://");
            let found = !t.loading
                && web
                && dom.descendants(crate::dom::Dom::ROOT).into_iter().any(|n| {
                    dom.tag(n) == Some("link")
                        && dom.attr(n, "href").is_some()
                        && dom.attr(n, "rel").is_some_and(|r| r.split_ascii_whitespace().any(|w| w.eq_ignore_ascii_case("ruwebos-app")))
                });
            self.offers_app = (key, found);
        }
        self.offers_app.1
    }

    /// Opens `<installer><path>?<query>` in the active tab.
    fn open_installer(&mut self, path: &str, query: &[(&str, &str)], host: &mut dyn Host) {
        let Some(base) = self.installer.clone() else { return };
        let pairs: Vec<(String, String)> = query.iter().map(|(k, v)| (k.to_string(), v.to_string())).collect();
        self.navigate(&format!("{base}{path}?{}", crate::document::urlencode(&pairs)), host);
    }

    /// Debug dump of the active page's layout (see
    /// [`Document::dump_layout`]).
    pub fn dump_layout(&self, selector: &str, depth: usize) -> String {
        self.tabs[self.active].doc.dump_layout(selector, depth)
    }

    /// Called by the platform when fetch `id` was redirected to `url`
    /// (before [`Browser::fetch_done`]), so the page uses its real address.
    pub fn fetch_redirected(&mut self, id: u32, final_url: &str) {
        if let Some(Request::Http { url, .. }) = self.requests.get_mut(&id) {
            *url = final_url.to_string();
            return;
        }
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

    /// Called by the platform with the status and headers of HTTP fetch
    /// `id` (before [`Browser::fetch_done`]).
    pub fn fetch_response(&mut self, id: u32, info: ResponseInfo) {
        if self.requests.contains_key(&id) {
            self.responses.insert(id, info);
        }
    }

    /// Called by the platform when fetch `id` completes.
    pub fn fetch_done(&mut self, id: u32, result: Result<Vec<u8>, String>, host: &mut dyn Host) {
        let info = self.responses.remove(&id);
        self.net_end(id, &result, info.as_ref(), host);
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
                // A response the server marks as an attachment, or one
                // the browser cannot show, is saved instead (HTML:
                // "navigate", handling as a download); the page stays.
                if let (Ok(bytes), Some(info)) = (&result, &info) {
                    let disposition = info.header("content-disposition");
                    let attachment =
                        disposition.is_some_and(|d| crate::net::content_disposition(d).attachment);
                    let shown = crate::net::displayable(info.header("content-type").unwrap_or(""));
                    if attachment || !shown {
                        let name = crate::net::download_name(&url, disposition, None);
                        self.cancel_navigation(i);
                        return self.save(&name, bytes, host);
                    }
                }
                let html = match result {
                    Ok(bytes) => as_html(&url, &bytes),
                    Err(e) => error_page(&url, &e),
                };
                self.set_page(i, &html, &url, host);
            }
            Request::Stylesheet {
                tab,
                serial,
                frame,
                link,
                url,
            } => {
                let Some(i) = self
                    .tab_index(tab)
                    .filter(|&i| self.tabs[i].serial == serial)
                else {
                    return;
                };
                let kind = if result.is_ok() { "load" } else { "error" };
                if let Some(l) = link.filter(|_| frame.is_none()) {
                    self.tabs[i].blocking.0.remove(&l);
                    self.dirty = true;
                }
                if let Ok(bytes) = result
                    && let Some(doc) = self.tabs[i].doc_at(frame)
                {
                    let css = String::from_utf8_lossy(&bytes).into_owned();
                    match link {
                        Some(link) => doc.add_stylesheet(link, css, &url),
                        None => doc.add_import(&url, css),
                    }
                    self.dirty = true;
                }
                // Frames run no scripts: no events for their sheets.
                if let Some(link) = link.filter(|_| frame.is_none()) {
                    if self.tabs[i].loaded {
                        self.element_event(i, link, kind, host);
                    } else {
                        self.tabs[i].link_events.push((link, kind));
                    }
                }
            }
            Request::Image {
                tab,
                serial,
                frame,
                node,
                src,
            } => {
                if let Some(i) = self
                    .tab_index(tab)
                    .filter(|&i| self.tabs[i].serial == serial)
                    && let Some(doc) = self.tabs[i].doc_at(frame)
                {
                    doc.image_loaded(node, &src, result);
                    self.dirty = true;
                    // An <img> of the page hears `load` (decoded) or `error`.
                    if frame.is_none()
                        && let Some(n) = node
                        && self.tabs[i].doc.dom.tag(n) == Some("img")
                    {
                        let kind = match self.tabs[i].doc.images.get(n) {
                            Some(crate::document::ImageState::Ready(s, _)) if *s == src => Some("load"),
                            Some(crate::document::ImageState::Failed(s)) if *s == src => Some("error"),
                            _ => None,
                        };
                        if let Some(kind) = kind {
                            self.element_event(i, n, kind, host);
                        }
                    }
                }
            }
            Request::Frame {
                tab,
                serial,
                node,
                source,
                url,
            } => {
                if let Some(i) = self
                    .tab_index(tab)
                    .filter(|&i| self.tabs[i].serial == serial)
                {
                    let html = match result {
                        Ok(bytes) => as_html(&url, &bytes),
                        Err(e) => error_page(&url, &e),
                    };
                    self.tabs[i].doc.frame_loaded(node, &source, &html, &url);
                    self.frame_loaded(i, node, host);
                }
            }
            Request::Font {
                tab,
                serial,
                frame,
                font,
            } => {
                if std::env::var_os("RUBROWSER_PROFILE").is_some() {
                    eprintln!(
                        "[profile] font {} weight={} italic={} {}: {:?}",
                        font.family,
                        font.weight,
                        font.italic,
                        font.url,
                        result.as_ref().map(Vec::len)
                    );
                }
                if let Some(i) = self
                    .tab_index(tab)
                    .filter(|&i| self.tabs[i].serial == serial)
                    && let Ok(bytes) = result
                    && self.fonts.add_web_font(
                        &font.family,
                        font.weight,
                        font.italic,
                        &font.ranges,
                        &bytes,
                    )
                    && let Some(doc) = self.tabs[i].doc_at(frame)
                {
                    doc.fonts_changed();
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
                let tab = &mut self.tabs[i];
                let index = slot.checked_sub(tab.consumed);
                if let Some(p) = index.and_then(|k| tab.pending.get_mut(k)) {
                    match &p.graph {
                        Some(g) => {
                            let root = g.root.clone();
                            return self.module_loaded(i, slot, &root, result, host);
                        }
                        None => p.body = Some(result),
                    }
                }
                self.run_scripts(i, host);
            }
            Request::Http {
                tab,
                serial,
                script,
                id,
                url,
                cors,
            } => {
                let Some(i) = self
                    .tab_index(tab)
                    .filter(|&i| self.tabs[i].serial == serial)
                else {
                    return;
                };
                let page = self.tabs[i].doc.url.clone();
                // A same-origin request redirected to another origin is
                // checked like a cross-origin one.
                let cors = cors.or_else(|| {
                    (origin(&url).is_some() && origin(&url) != origin(&page)).then(|| {
                        Box::new(Cors {
                            origin: url::origin_header(&page),
                            credentials: true,
                            opaque: false,
                            method: String::new(),
                            requested: Vec::new(),
                            actual: None,
                        })
                    })
                });
                let mut exposed = info.as_ref().map(|r| r.headers.clone()).unwrap_or_default();
                let mut result = result;
                if let Some(c) = &cors {
                    if c.actual.is_some() {
                        // The preflight's answer: send the request, or fail.
                        let status = info.as_ref().map_or(0, |r| r.status);
                        let checked = result.as_ref().map_err(|e| e.clone()).and_then(|_| {
                            crate::net::cors::check_preflight(&c.origin, c.credentials, status, &c.method, &c.requested, &exposed)
                        });
                        if checked.is_ok() {
                            let mut c = c.clone();
                            let request = c.actual.take().unwrap_or_default();
                            let req = Request::Http { tab, serial, script, id, url: url.clone(), cors: Some(c) };
                            self.send_script_request(req, &url, request, host);
                            return;
                        }
                        result = checked.map(|_| Vec::new());
                    } else if c.opaque {
                        // no-cors: neither status, headers nor body.
                        result = result.map(|_| Vec::new());
                        exposed = vec![(":opaque".into(), "1".into())];
                    } else if result.is_ok() {
                        match crate::net::cors::check(&c.origin, c.credentials, &exposed) {
                            Ok(()) => exposed = crate::net::cors::exposed(&exposed, c.credentials),
                            Err(e) => result = Err(e),
                        }
                    }
                    if let Err(e) = &result {
                        let msg = format!(
                            "Access to fetch at '{url}' from origin '{}' has been blocked by CORS policy: {e}",
                            c.origin
                        );
                        result = Err(msg);
                    }
                }
                let opaque = cors.as_ref().is_some_and(|c| c.opaque) && result.is_ok();
                let (status, body) = match result {
                    Ok(_) if opaque => (0, Vec::new()),
                    Ok(b) => (info.as_ref().map_or(200, |r| r.status as i32), b),
                    Err(e) => {
                        self.console_message(i, &format!("error: {e}"), host);
                        (0, Vec::new())
                    }
                };
                // Headers as `name: value` lines, for the script.
                let headers: String = exposed
                    .iter()
                    .map(|(n, v)| format!("{n}: {v}\r\n"))
                    .collect();
                let now = self.now;
                let t = &mut self.tabs[i];
                if let Some(Some(rt)) = t.scripts.get_mut(script) {
                    match rt.http_done(&mut t.doc, id, status, body, &headers, now) {
                        Ok(()) => {
                            self.after_script(i, script, host);
                        }
                        Err(e) => self.script_failed(i, script, &e, host),
                    }
                }
            }
            Request::Module {
                tab,
                serial,
                slot,
                url,
            } => {
                if let Some(i) = self
                    .tab_index(tab)
                    .filter(|&i| self.tabs[i].serial == serial)
                {
                    self.module_loaded(i, slot, &url, result, host);
                }
            }
            Request::Download { url, name, .. } => match result {
                Ok(bytes) => {
                    let disposition = info.as_ref().and_then(|r| r.header("content-disposition"));
                    let name = crate::net::download_name(&url, disposition, name.as_deref());
                    self.save(&name, &bytes, host);
                }
                Err(e) => self.show_download(&format!("Download failed: {e}"), host),
            },
        }
    }

    // ----- downloads ---------------------------------------------------------

    /// Undoes the history entry of tab `i`'s navigation that turned out to
    /// be a download, keeping its current page.
    fn cancel_navigation(&mut self, i: usize) {
        let tab = &mut self.tabs[i];
        tab.loading = false;
        if tab.index > 0 && tab.history[tab.index] != tab.doc.url {
            tab.history.remove(tab.index);
            tab.scrolls.remove(tab.index);
            tab.index -= 1;
        }
        self.dirty = true;
    }

    /// Downloads `url` (already resolved) from page tab `i`: `blob:` and
    /// `data:` URLs at once, others with a request. `name`: the page's
    /// suggested file name (`<a download=…>`).
    fn download(&mut self, i: usize, url: &str, name: Option<&str>, host: &mut dyn Host) {
        let tab = self.tabs[i].id;
        if url.starts_with("blob:") {
            match self.blob(tab, url) {
                Ok(f) => {
                    let name = crate::net::download_name(url, None, name);
                    self.save(&name, &f.bytes, host);
                }
                Err(e) => self.show_download(&format!("Download failed: {e}"), host),
            }
            return;
        }
        if url.starts_with("data:") {
            match url::decode_data_url(url) {
                Some(bytes) => {
                    let name = crate::net::download_name(url, None, name);
                    self.save(&name, &bytes, host);
                }
                None => self.show_download("Download failed: bad data: URL", host),
            }
            return;
        }
        let req = Request::Download {
            tab,
            url: url.to_string(),
            name: name.map(String::from),
        };
        self.fetch(req, url, host);
    }

    /// Saves a download and tells the user where it went.
    fn save(&mut self, name: &str, bytes: &[u8], host: &mut dyn Host) {
        let msg = match host.save_download(name, bytes) {
            Ok(path) => format!("Downloaded {name} to {path}"),
            Err(e) => format!("Could not save {name}: {e}"),
        };
        self.show_download(&msg, host);
    }

    /// Shows `msg` in the download bar at the bottom of the window.
    fn show_download(&mut self, msg: &str, host: &mut dyn Host) {
        host.log(msg);
        let Some(bar) = self.chrome_el("download") else {
            return;
        };
        let text = self.chrome_el("download-text");
        let dom = self.chrome.dom_mut();
        if let Some(e) = dom.element_mut(bar) {
            e.set_attr("class", "show");
        }
        if let Some(t) = text {
            dom.set_text_content(t, msg);
        }
        self.chrome.dom_changed();
        self.dirty = true;
    }

    /// The download bar's text (empty when hidden).
    pub fn download_notice(&self) -> String {
        let shown = self
            .chrome_el("download")
            .is_some_and(|b| self.chrome.dom.attr(b, "class") == Some("show"));
        match self.chrome_el("download-text") {
            Some(t) if shown => self.chrome.dom.text_content(t),
            _ => String::new(),
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
        self.record_call(i, si);
        self.save_storage(host);
        for msg in &out.logs {
            self.console_message(i, msg, host);
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
        let version = tab.doc.dom.version();
        let changed = version != tab.dom_version;
        if changed {
            tab.dom_version = version;
            tab.doc.dom_changed();
        }
        self.dirty = true;
        let page = tab.doc.url.clone();
        for call in out.http {
            let target = url::resolve(&page, &call.url);
            self.script_request(i, si, call, &target, host);
        }
        for call in out.ws {
            self.ws_call(i, si, call, host);
        }
        if changed {
            self.adopt_inserted(i, host);
        }
        if let Some(target) = out.navigate
            && i == self.active
        {
            let target = url::resolve(&self.tabs[i].doc.url, &target);
            self.navigate(&target, host);
        } else if out.history != 0 && i == self.active {
            self.go(out.history as isize, host);
        } else if let Some(form) = out.submit.filter(|_| i == self.active) {
            // form.submit() skips the submit event, as in browsers.
            self.send_form(form, None, host);
        } else if i == self.active {
            for node in out.activate {
                self.script_activate(node, host);
            }
            for (node, on) in out.focus {
                let doc = &self.tabs[i].doc;
                if on && doc.is_focusable(node) {
                    self.keyboard = Target::Page;
                    self.focus_page(Some(node), host);
                } else if !on && doc.state().focus == Some(node) {
                    self.focus_page(None, host);
                }
            }
        }
        (out.prevent_default, out.stop_propagation)
    }

    /// Passes a script's WebSocket request to the platform.
    fn ws_call(&mut self, i: usize, si: usize, call: WsCall, host: &mut dyn Host) {
        let tab = self.tabs[i].id;
        let find = |b: &Browser, sid: i32| b.sockets.iter().find(|(_, v)| **v == (tab, si, sid)).map(|(k, _)| *k);
        match call {
            WsCall::Open { id, url, protocols } => {
                let gid = self.next_id;
                self.next_id += 1;
                self.sockets.insert(gid, (tab, si, id));
                // The server decides from Origin whom to accept (no CORS).
                let page = &self.tabs[i].doc.url;
                let origin = origin(page).unwrap_or_else(|| "null".into());
                host.ws_open(gid, &url, &protocols, &origin);
            }
            WsCall::Send { id, data, text } => {
                if let Some(gid) = find(self, id) {
                    host.ws_send(gid, &data, text);
                }
            }
            WsCall::Close { id, code, reason } => {
                if let Some(gid) = find(self, id) {
                    host.ws_close(gid, code, &reason);
                }
            }
        }
    }

    /// Closes the WebSockets of tab `tab` (its page is going away).
    fn close_sockets(&mut self, tab: u64, host: &mut dyn Host) {
        let ids: Vec<u32> = self.sockets.iter().filter(|(_, v)| v.0 == tab).map(|(k, _)| *k).collect();
        for gid in ids {
            self.sockets.remove(&gid);
            host.ws_close(gid, 1001, "");
        }
    }

    /// Something happened on WebSocket connection `id` (from the platform).
    pub fn ws_event(&mut self, id: u32, event: WsEvent, host: &mut dyn Host) {
        let Some(&(tab, si, sid)) = self.sockets.get(&id) else { return };
        let (kind, data, info) = match event {
            WsEvent::Open(protocol) => (1, Vec::new(), protocol),
            WsEvent::Text(t) => (2, t.into_bytes(), String::new()),
            WsEvent::Binary(b) => (3, b, String::new()),
            WsEvent::Close { code, reason, clean } => {
                self.sockets.remove(&id);
                (4, Vec::new(), format!("{code} {} {reason}", clean as i32))
            }
        };
        let Some(i) = self.tab_index(tab) else { return };
        let now = self.now;
        let t = &mut self.tabs[i];
        if let Some(Some(rt)) = t.scripts.get_mut(si) {
            match rt.ws_event(&mut t.doc, sid, kind, data, &info, now) {
                Ok(()) => {
                    self.after_script(i, si, host);
                }
                Err(e) => self.script_failed(i, si, &e, host),
            }
        }
    }

    /// Starts HTTP request `call` of script `si` of tab `i` to `target`:
    /// same-origin requests directly, cross-origin ones by the CORS
    /// protocol (with a preflight when needed).
    fn script_request(&mut self, i: usize, si: usize, call: crate::script::HttpCall, target: &str, host: &mut dyn Host) {
        use crate::net::cors;
        let (tab, serial) = (self.tabs[i].id, self.tabs[i].serial);
        let page = self.tabs[i].doc.url.clone();
        let same = origin(target).is_some() && origin(target) == origin(&page);
        let method = if call.method.is_empty() { "GET".to_string() } else { call.method.clone() };
        let mut request = crate::input::HttpRequest {
            method: method.clone(),
            body: call.body,
            content_type: call.content_type,
            referrer: url::referrer(&page, target),
            origin: String::new(),
            fetch_site: url::fetch_site(&page, target),
            headers: call.headers,
            omit_credentials: call.credentials == "omit",
        };
        let req = |cors| Request::Http { tab, serial, script: si, id: call.id, url: target.to_string(), cors };
        let fail = |b: &mut Self, msg: String, host: &mut dyn Host| {
            let fid = b.next_id;
            b.next_id += 1;
            b.requests.insert(fid, req(None));
            b.net_begin(fid, target, None, host);
            b.local.push((fid, Err(msg)));
        };
        if same {
            if !matches!(method.as_str(), "GET" | "HEAD") {
                request.origin = url::origin_header(&page);
            }
            return self.send_script_request(req(None), target, request, host);
        }
        if origin(target).is_none() {
            return fail(self, format!("Fetch API cannot load {target}: URL scheme is not supported."), host);
        }
        if call.mode == "same-origin" {
            return fail(self, format!("Fetch API cannot load {target}. Request mode is \"same-origin\" but the URL's origin is not same as the request origin."), host);
        }
        let opaque = call.mode == "no-cors";
        let requested = cors::unsafe_headers(&request.headers, &request.content_type);
        if opaque && (!cors::simple_method(&method) || !requested.is_empty()) {
            return fail(self, format!("Fetch API cannot load {target}: a no-cors request may only use GET, HEAD or POST and CORS-safelisted headers."), host);
        }
        let credentials = call.credentials == "include";
        request.omit_credentials = !credentials;
        request.origin = url::origin_header(&page);
        let mut state = Cors { origin: request.origin.clone(), credentials, opaque, method: method.clone(), requested: requested.clone(), actual: None };
        if !opaque && cors::needs_preflight(&method, &request.headers, &request.content_type) {
            let mut headers = vec![("Access-Control-Request-Method".to_string(), method)];
            if !requested.is_empty() {
                headers.push(("Access-Control-Request-Headers".into(), requested.join(",")));
            }
            let preflight = crate::input::HttpRequest {
                method: "OPTIONS".into(),
                origin: request.origin.clone(),
                referrer: request.referrer.clone(),
                fetch_site: request.fetch_site.clone(),
                headers,
                omit_credentials: true,
                ..Default::default()
            };
            state.actual = Some(request);
            return self.send_script_request(req(Some(Box::new(state))), target, preflight, host);
        }
        self.send_script_request(req(Some(Box::new(state))), target, request, host);
    }

    fn send_script_request(&mut self, req: Request, target: &str, request: crate::input::HttpRequest, host: &mut dyn Host) {
        let fid = self.next_id;
        self.next_id += 1;
        self.requests.insert(fid, req);
        self.net_begin(fid, target, Some(&request), host);
        host.fetch_with(fid, target, &request);
    }

    /// Reports a script failure and stops that script.
    fn script_failed(&mut self, i: usize, si: usize, err: &str, host: &mut dyn Host) {
        // A language runtime survives one of its scripts being stopped for
        // time (what it did before stays done, its timers included); it
        // fails for good if that left it broken.
        let survives = err == crate::script::TOO_LONG
            && self.tabs[i].scripts[si]
                .as_ref()
                .is_some_and(Script::is_runtime);
        if survives {
            self.after_script(i, si, host);
        } else if let Some(Some(script)) = self.tabs[i].scripts.get_mut(si) {
            // What it logged before failing (a panic's message).
            for msg in script.take_output().logs {
                self.console_message(i, &msg, host);
            }
        }
        let msg = format!("script error in {}: {err}", self.tabs[i].doc.url);
        self.console_message(i, &msg, host);
        if survives {
            return;
        }
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
            repeat: init.repeat,
            delta: (init.delta.0 as f64, init.delta.1 as f64),
            touch: init.touch,
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
            if stopped || init.local {
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
        let offer = self.installer.is_some() && self.page_offers_app();
        if let Some(n) = self.chrome_el("install")
            && self.chrome.dom.attr(n, "hidden").is_some() == offer
            && let Some(e) = self.chrome.dom_mut().element_mut(n)
        {
            if offer {
                e.remove_attr("hidden");
            } else {
                e.set_attr("hidden", "");
            }
            self.chrome.dom_changed();
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
        self.sync_osk();
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
            Event::MouseDown { x, y, button: 2 } => self.context_menu(x as f32, y as f32, host),
            Event::MouseUp { x, y, button: 0 } => self.mouse_up(x as f32, y as f32, host),
            Event::MouseDown { .. } | Event::MouseUp { .. } => {}
            Event::Wheel { dx, dy } => {
                self.close_menu();
                let (t, lx, ly) = self.target_at(self.pointer.0, self.pointer.1);
                // The page hears `wheel` first; canceling it (a map zooming)
                // keeps the page from scrolling.
                if t == Target::Page
                    && let Some(e) = self.doc(t).hit_test(lx, ly).element
                {
                    let init = EventInit { delta: (dx as f32, dy as f32), ..EventInit::pointer(lx, ly, 0) };
                    if self.dispatch(e, "wheel", init, host) {
                        return;
                    }
                }
                // A frame under the pointer scrolls first.
                let doc = &mut self.tabs[self.active].doc;
                if t == Target::Page
                    && let Some((n, fx, fy)) = doc.frame_at(lx, ly)
                    && doc
                        .frame_mut(n)
                        .is_some_and(|f| f.scroll_by(fx, fy, dy as f32))
                {
                    self.dirty = true;
                } else if self.doc_mut(t).scroll_by(lx, ly, dy as f32) {
                    self.dirty = true;
                    // Content moved under the pointer: refresh hover.
                    let (px, py) = self.pointer;
                    self.mouse_move(px, py, host);
                }
            }
            Event::KeyDown { key, mods, repeat } => {
                if !self.osk_typing {
                    // a physical keyboard: the on-screen one goes away (auto mode)
                    self.touch_used = false;
                }
                self.mods = mods;
                self.key_repeat = repeat;
                self.key_down(key, mods, host);
                self.key_repeat = false;
            }
            Event::KeyUp { key, mods } => {
                self.mods = mods;
                // the page hears keyup at the focused element (or the body)
                if self.keyboard == Target::Page {
                    let doc = self.doc(Target::Page);
                    let target = doc.state().focus.or_else(|| doc.dom.body()).unwrap_or(crate::dom::Dom::ROOT);
                    self.dispatch(target, "keyup", EventInit::key(key, mods.bits()), host);
                }
            }
            Event::Touch { id, x, y, phase } => self.touch(id, x as f32, y as f32, phase, host),
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
        if self.devtools_pointer_move(x, y) {
            return;
        }
        if self.menu.is_some() {
            // Hover inside the menu (it is part of the chrome, above the page).
            let hover = self.chrome.hit_test(x, y).element;
            self.set_state(Target::Chrome, |s| s.hover = hover);
            return;
        }
        // pointermove and mousemove at the element under the pointer (only
        // for real moves, not for hover refreshes after scrolling).
        if (x, y) != self.last_move {
            self.last_move = (x, y);
            let (t, lx, ly) = self.target_at(x, y);
            if t == Target::Page
                && let Some(e) = self.doc(t).hit_test(lx, ly).element
            {
                let at = EventInit::pointer(lx, ly, 0);
                self.dispatch(e, "pointermove", at.clone(), host);
                self.dispatch(e, "mousemove", at, host);
            }
        }
        // Dragging over page text selects it (after a few pixels: a click
        // that wobbles is still a click), unless the page cancels
        // `selectstart` (a map or a slider that drags things itself).
        if let Some(Press { select: Some((_, sx, sy)), selected, element, .. }) = self.press {
            let vp = self.viewport_rect();
            let (lx, ly) = (x - vp.x, y - vp.y);
            if !selected && ((lx - sx).abs() > 3.0 || (ly - sy).abs() > 3.0) {
                let target = element.unwrap_or(crate::dom::Dom::ROOT);
                let doc = &self.tabs[self.active].doc;
                let no_select = doc.style(target).is_some_and(|s| s.user_select_none);
                if no_select || self.dispatch(target, "selectstart", EventInit::default(), host) {
                    if let Some(p) = self.press.as_mut() {
                        p.select = None;
                    }
                    return;
                }
            }
        }
        if let Some(Press { select: Some((anchor, sx, sy)), selected, .. }) = self.press {
            let vp = self.viewport_rect();
            let (lx, ly) = (x - vp.x, y - vp.y);
            if selected || (lx - sx).abs() > 3.0 || (ly - sy).abs() > 3.0 {
                if let Some(p) = self.press.as_mut() {
                    p.selected = true;
                }
                let doc = &mut self.tabs[self.active].doc;
                let runs = doc.text_runs();
                if let Some(focus) = doc.text_point_at(&runs, lx, ly, &self.fonts) {
                    doc.set_selection(Some(crate::document::selection::Selection { anchor, focus }));
                }
                self.dirty = true;
                return;
            }
        }
        if let Some(Press { range: Some(r), .. }) = self.press {
            let vp = self.viewport_rect();
            return self.slide(r, x - vp.x, host);
        }
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
                self.dispatch(o, "mouseout", at.clone(), host);
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
        self.pointer = (x, y);
        // An open context menu takes the click: an item runs, anywhere else closes it.
        if self.menu.is_some() {
            let action = self.menu_item_at(x, y);
            let ctx = self.menu.take().unwrap_or_default();
            self.show_menu(None);
            if let Some(a) = action {
                self.menu_command(&a, ctx, host);
            }
            return;
        }
        if self.devtools_pointer_down(x, y) {
            return;
        }
        let (t, lx, ly) = self.target_at(x, y);
        // The on-screen keyboard's keys type into the focused field (focus stays).
        if t == Target::Chrome
            && let Some(k) = self.osk_key_at(self.chrome.hit_test(lx, ly).element)
        {
            self.osk_press(&k, host);
            return;
        }
        if !self.from_touch {
            // a mouse user: the keyboard waits for the next touch (auto mode)
            self.touch_used = false;
        }
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
        if t == Target::Page {
            self.tabs[self.active].doc.user_activated = true;
        }
        // pointerdown, then mousedown (Pointer Events §11); a page that
        // cancels them keeps the press to itself (no text selection).
        let mut canceled = false;
        if t == Target::Page
            && let Some(e) = hit.element
        {
            // a tap already sent its pointer events (touch.rs)
            if !self.from_touch {
                canceled = self.dispatch(e, "pointerdown", EventInit::pointer(lx, ly, 0), host);
            }
            if !canceled {
                canceled = self.dispatch(e, "mousedown", EventInit::pointer(lx, ly, 0), host);
            }
        }
        self.last_move = (x, y);
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
        // A range input jumps to the pointer and follows it while pressed.
        let range = hit
            .element
            .filter(|&e| {
                use crate::document::{Control, control};
                t == Target::Page && control(&self.doc(t).dom, e) == Some(Control::Range)
            })
            .filter(|&e| self.doc(t).dom.attr(e, "disabled").is_none());
        if let Some(e) = range {
            self.slide(e, lx, host);
        }
        // Page text: a press starts a selection (shift extends it, a second
        // press on the same spot selects the word).
        let mut select = None;
        let control = hit
            .element
            .is_some_and(|e| crate::document::control(&self.doc(t).dom, e).is_some());
        if t == Target::Page && input.is_none() && range.is_none() && !control && !canceled {
            let double = self
                .last_down
                .is_some_and(|(px, py, at)| (px - lx).abs() < 4.0 && (py - ly).abs() < 4.0 && self.now - at < DOUBLE_CLICK_MS);
            self.last_down = Some((lx, ly, self.now));
            let shift = self.mods.shift;
            let doc = &mut self.tabs[self.active].doc;
            let runs = doc.text_runs();
            let point = doc.text_point_at(&runs, lx, ly, &self.fonts);
            match (point, doc.selection) {
                (Some(p), _) if double => doc.select_word(&runs, p),
                (Some(p), Some(sel)) if shift => {
                    doc.set_selection(Some(crate::document::selection::Selection { focus: p, ..sel }));
                    select = Some((sel.anchor, lx, ly));
                }
                (p, _) => {
                    doc.set_selection(None);
                    select = p.map(|p| (p, lx, ly));
                }
            }
        }
        self.press = Some(Press {
            target: t,
            element: hit.element,
            input,
            range,
            select,
            selected: false,
        });
        self.dirty = true;
    }

    /// Sets range input `node` to the value under page x `lx`; fires
    /// `input` if it changed.
    fn slide(&mut self, node: NodeId, lx: f32, host: &mut dyn Host) {
        let doc = &mut self.tabs[self.active].doc;
        let Some(area) = doc.content_box(node) else {
            return;
        };
        let (lo, hi) = crate::document::range_bounds(&doc.dom, node);
        let thumb = area.h.min(16.0);
        let span = (area.w - thumb).max(1.0);
        let f = ((lx - area.x - thumb / 2.0) / span).clamp(0.0, 1.0) as f64;
        let before = doc.input_value(node);
        doc.set_input_value(node, &(lo + (hi - lo) * f).to_string());
        if doc.input_value(node) != before {
            doc.dom_changed();
            self.dirty = true;
            self.dispatch(node, "input", EventInit::default(), host);
        }
    }

    fn mouse_up(&mut self, x: f32, y: f32, host: &mut dyn Host) {
        self.pointer = (x, y);
        if self.devtools_pointer_up() {
            return;
        }
        let Some(press) = self.press.take() else {
            return;
        };
        self.set_state(press.target, |s| s.active = None);
        let (t, lx, ly) = self.target_at(x, y);
        if let Some(r) = press.range {
            self.slide(r, lx, host);
            self.dispatch(r, "change", EventInit::default(), host);
        }
        let hit = self.doc(t).hit_test(lx, ly);
        if t == Target::Page
            && let Some(e) = hit.element
        {
            if !self.from_touch {
                self.dispatch(e, "pointerup", EventInit::pointer(lx, ly, 0), host);
            }
            self.dispatch(e, "mouseup", EventInit::pointer(lx, ly, 0), host);
        }
        if t != press.target || press.input.is_some() || press.selected {
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
                    if let Some(cmd) = dom.attr(id, "data-dt") {
                        let cmd = cmd.to_string();
                        return self.devtools_command(&cmd, host);
                    }
                    let tab = dom
                        .attr(id, "data-tab")
                        .and_then(|v| v.parse::<usize>().ok());
                    match (dom.attr(id, "data-action"), tab) {
                        (Some(action), tab) => {
                            let action = action.to_string();
                            return self.command(&action, tab, host);
                        }
                        (None, Some(i)) => {
                            if i < self.tabs.len() && i != self.active {
                                self.active = i;
                                self.tab_switched();
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
        if self.tabs[self.active].doc.frame(node).is_some() {
            return self.activate_in_frame(node, host);
        }
        if self.activate_media(node) {
            self.dirty = true;
            return;
        }
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
                // `download`: save instead of navigating (HTML: only for
                // same-origin, blob: and data: URLs; others navigate).
                if let Some(name) = doc.dom.attr(id, "download") {
                    let local = target.starts_with("blob:") || target.starts_with("data:");
                    if local || (origin(&target).is_some() && origin(&target) == origin(&doc.url)) {
                        let name = name.to_string();
                        return self.download(self.active, &target, Some(&name), host);
                    }
                }
                // target=_blank (and named targets) open a new tab.
                let new_tab = doc
                    .dom
                    .attr(id, "target")
                    .is_some_and(|t| !t.is_empty() && !matches!(t, "_self" | "_top" | "_parent"));
                if new_tab {
                    return self.new_tab(&target, host);
                }
                return self.navigate(&target, host);
            }
            n = doc.dom.parent(id);
        }
    }

    /// The activation behavior of `node` after a script's `click()` (or
    /// `showPicker()`): opens a file input's dialog, follows (or
    /// downloads) a link, submits or resets a form. Other elements have
    /// none here.
    fn script_activate(&mut self, node: NodeId, host: &mut dyn Host) {
        use crate::document::{Control, control};
        let doc = &self.tabs[self.active].doc;
        if doc.dom.get(node).is_none() || doc.dom.attr(node, "disabled").is_some() {
            return;
        }
        match control(&doc.dom, node) {
            Some(k @ (Control::File | Control::Submit | Control::Reset)) => {
                self.activate_control(node, k, host)
            }
            None if doc.link(node).is_some() => self.activate(node, host),
            _ => {}
        }
    }

    /// A click on a media element with `controls`: seeks on the progress
    /// track, else plays or pauses. Returns whether it was one.
    fn activate_media(&mut self, node: NodeId) -> bool {
        let vp = self.viewport_rect();
        let doc = &mut self.tabs[self.active].doc;
        if !crate::document::media::is_media(&doc.dom, node)
            || doc.dom.attr(node, "controls").is_none()
        {
            return false;
        }
        let Some(area) = doc.content_box(node) else {
            return false;
        };
        let (x, y) = (self.pointer.0 - vp.x, self.pointer.1 - vp.y);
        let bar_top = area.y + area.h - crate::document::media::CONTROLS_HEIGHT;
        let (x0, x1) = crate::document::media::progress_track(area);
        let duration = doc.media(node).map_or(f64::NAN, |m| m.info().duration);
        if y >= bar_top && x >= x0 - 4.0 && x <= x1 + 4.0 && duration.is_finite() {
            let f = ((x - x0) / (x1 - x0)).clamp(0.0, 1.0) as f64;
            doc.media_command(node, &format!("seek\u{1}{}", f * duration), true);
        } else if y >= bar_top && x >= area.x + area.w - 34.0 {
            let muted = doc.media(node).is_some_and(|m| m.muted);
            doc.media_command(node, &format!("muted\u{1}{}", !muted as u8), true);
        } else {
            doc.toggle_media(node);
        }
        true
    }

    /// A click on `<iframe>` `frame` at the pointer: follows a link in its
    /// document (frames run no scripts, so there is nothing else to do).
    fn activate_in_frame(&mut self, frame: NodeId, host: &mut dyn Host) {
        let vp = self.viewport_rect();
        let (x, y) = (self.pointer.0 - vp.x, self.pointer.1 - vp.y);
        let tab = &self.tabs[self.active];
        let Some((_, fx, fy)) = tab.doc.frame_at(x, y) else {
            return;
        };
        let Some(doc) = tab.doc.frame(frame) else {
            return;
        };
        let mut n = doc.hit_test(fx, fy).element;
        while let Some(id) = n {
            if let Some(href) = doc.link(id) {
                let target = url::resolve(&doc.base_url(), &href);
                let name = doc.dom.attr(id, "target").unwrap_or("");
                return match name {
                    "" | "_self" => {
                        let (tab_id, serial) = (tab.id, tab.serial);
                        self.tabs[self.active].doc.navigate_frame(frame, &target);
                        let req = Request::Frame {
                            tab: tab_id,
                            serial,
                            node: frame,
                            source: target.clone(),
                            url: target.clone(),
                        };
                        self.fetch(req, &target, host);
                        self.dirty = true;
                    }
                    "_top" | "_parent" => self.navigate(&target, host),
                    _ => self.new_tab(&target, host),
                };
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
            Control::File => return self.choose_files(node, host),
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

    /// Shows the file dialog for `<input type=file>` `node`; fires `input`
    /// and `change` once files are chosen, `cancel` if the user cancels.
    /// Without a dialog (no window) the input stays as it is.
    fn choose_files(&mut self, node: NodeId, host: &mut dyn Host) {
        let doc = &self.tabs[self.active].doc;
        let multiple = doc.dom.attr(node, "multiple").is_some();
        let accept = doc.dom.attr(node, "accept").unwrap_or("").to_string();
        let Some(files) = host.pick_files(multiple, &accept) else {
            return;
        };
        if files.is_empty() {
            self.dispatch(node, "cancel", EventInit::default(), host);
            return;
        }
        let files = if multiple {
            files
        } else {
            files.into_iter().take(1).collect()
        };
        let doc = &mut self.tabs[self.active].doc;
        doc.forms.set_files(node, files);
        doc.dom_changed();
        self.dirty = true;
        self.dispatch(node, "input", EventInit::default(), host);
        self.dispatch(node, "change", EventInit::default(), host);
    }

    /// Submits `form` (after its `submit` event).
    fn submit_form(&mut self, form: NodeId, submitter: Option<NodeId>, host: &mut dyn Host) {
        if self.dispatch(form, "submit", EventInit::default(), host) {
            return;
        }
        self.send_form(form, submitter, host);
    }

    /// The HTML standard's form submission algorithm (without the
    /// `submit` event): the entry list encoded as the form's (or the
    /// submitter's) method and enctype say, sent by navigating.
    fn send_form(&mut self, form: NodeId, submitter: Option<NodeId>, host: &mut dyn Host) {
        let doc = &self.tabs[self.active].doc;
        // formaction / formmethod / formenctype on the submit button win.
        let attr = |name: &str| {
            submitter
                .and_then(|s| doc.dom.attr(s, &format!("form{name}")))
                .or_else(|| doc.dom.attr(form, name))
                .unwrap_or("")
                .trim()
                .to_string()
        };
        let (action, method, enctype) = (
            attr("action"),
            attr("method").to_ascii_lowercase(),
            attr("enctype").to_ascii_lowercase(),
        );
        let entries = doc.form_entries(form, submitter);
        let base = if action.is_empty() {
            doc.url.clone()
        } else {
            url::resolve(&doc.base_url(), &action)
        };
        if method == "post" {
            let (body, content_type) = match enctype.as_str() {
                "multipart/form-data" => crate::document::multipart(&entries),
                "text/plain" => (
                    crate::document::text_plain(&crate::document::entry_pairs(&entries))
                        .into_bytes(),
                    "text/plain".to_string(),
                ),
                _ => (
                    crate::document::urlencode(&crate::document::entry_pairs(&entries))
                        .into_bytes(),
                    "application/x-www-form-urlencoded".to_string(),
                ),
            };
            // The body is sent once; going back to this entry reloads it
            // with GET.
            let target = base.split('#').next().unwrap_or("").to_string();
            let doc = &self.tabs[self.active].doc.url;
            let referrer = url::referrer(doc, &target);
            // Origin and Sec-Fetch-Site let the server refuse forms posted
            // from other sites (CSRF)
            let (origin, fetch_site) = (url::origin_header(doc), url::fetch_site(doc, &target));
            self.tabs[self.active].post = Some(crate::input::HttpRequest {
                method: "POST".into(),
                body,
                content_type,
                referrer,
                origin,
                fetch_site,
                ..Default::default()
            });
            return self.navigate(&target, host);
        }
        let data = crate::document::entry_pairs(&entries);
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
            "reload" => self.reload(host),
            "install" => {
                let url = self.tabs[self.active].url().to_string();
                self.open_installer("install", &[("src", &url)], host);
            }
            "close-download" => {
                if let Some(bar) = self.chrome_el("download")
                    && let Some(e) = self.chrome.dom_mut().element_mut(bar)
                {
                    e.set_attr("class", "");
                }
                self.dirty = true;
            }
            _ => {}
        }
    }

    /// Opens the context menu at window point `(x, y)` (right click, or
    /// Ctrl-click on macOS), after the page's `contextmenu` event (a page
    /// that cancels it shows its own menu).
    fn context_menu(&mut self, x: f32, y: f32, host: &mut dyn Host) {
        self.close_menu();
        self.pointer = (x, y);
        let (t, lx, ly) = self.target_at(x, y);
        let hit = self.doc(t).hit_test(lx, ly);
        let mut ctx = MenuCtx::default();
        if t == Target::Page {
            if let Some(e) = hit.element
                && self.dispatch(e, "contextmenu", EventInit::pointer(lx, ly, 2), host)
            {
                return;
            }
            ctx.node = hit.element;
            let doc = &self.tabs[self.active].doc;
            let base = doc.base_url();
            let mut n = hit.element;
            while let Some(e) = n {
                match doc.dom.tag(e) {
                    Some("a") if ctx.link.is_none() => {
                        ctx.link = doc.dom.attr(e, "href").map(|h| crate::net::url::resolve(&base, h));
                    }
                    Some("img") if ctx.image.is_none() => {
                        ctx.image = doc.dom.attr(e, "src").map(|h| crate::net::url::resolve(&base, h));
                    }
                    _ => {}
                }
                n = doc.dom.parent(e);
            }
        }
        if let Some(f) = hit
            .element
            .and_then(|e| self.doc(t).focusable_ancestor(e))
            .filter(|&f| self.doc(t).is_text_input(f))
        {
            ctx.field = Some((t, f));
            // The menu acts on that field: focus it (keeping its selection).
            if self.doc(t).state().focus != Some(f) {
                if t == Target::Page {
                    self.focus_page(Some(f), host);
                } else {
                    self.set_focus(t, Some(f));
                }
            }
            self.keyboard = t;
        }
        let selected = match ctx.field {
            Some((ft, f)) => !self.doc_mut(ft).input(f).selected_text().is_empty(),
            None => self.tabs[self.active].doc.has_selection(),
        };
        // (label, action, enabled, shortcut); "" is a separator
        let mut items: Vec<(&str, &str, bool, &str)> = Vec::new();
        if ctx.link.is_some() {
            items.extend([("開啟連結", "ctx-open-link", true, ""), ("在新分頁開啟連結", "ctx-open-link-tab", true, ""), ("複製連結網址", "ctx-copy-link", true, ""), ("下載連結的檔案", "ctx-save-link", true, ""), ("", "", false, "")]);
        }
        if ctx.image.is_some() {
            items.extend([("在新分頁開啟圖片", "ctx-open-image-tab", true, ""), ("複製圖片網址", "ctx-copy-image", true, ""), ("下載圖片", "ctx-save-image", true, ""), ("", "", false, "")]);
        }
        let mac = cfg!(target_os = "macos");
        let k = |c: &'static str, m: &'static str| if mac { m } else { c };
        if ctx.field.is_some() {
            items.extend([
                ("剪下", "ctx-cut", selected, k("Ctrl+X", "⌘X")),
                ("複製", "ctx-copy", selected, k("Ctrl+C", "⌘C")),
                ("貼上", "ctx-paste", true, k("Ctrl+V", "⌘V")),
                ("全選", "ctx-select-all", true, k("Ctrl+A", "⌘A")),
            ]);
        } else {
            items.extend([("複製", "ctx-copy", selected, k("Ctrl+C", "⌘C")), ("全選", "ctx-select-all", t == Target::Page, k("Ctrl+A", "⌘A"))]);
        }
        let tab = &self.tabs[self.active];
        if self.installer.is_some() {
            let web = tab.url().starts_with("http://") || tab.url().starts_with("https://");
            items.extend([("", "", false, ""), ("加到主畫面", "ctx-favorite", web, "")]);
        }
        items.extend([
            ("", "", false, ""),
            ("上一頁", "back", tab.index > 0, k("Alt+←", "⌘[")),
            ("下一頁", "forward", tab.index + 1 < tab.history.len(), k("Alt+→", "⌘]")),
            ("重新載入", "reload", true, k("Ctrl+R", "⌘R")),
        ]);
        if ctx.node.is_some() {
            items.extend([("", "", false, ""), ("檢查", "ctx-inspect", true, k("Ctrl+Shift+C", "⌥⌘I"))]);
        }
        let esc = |v: &str| v.replace('&', "&amp;").replace('<', "&lt;");
        let mut html = String::new();
        for (label, action, on, keys) in &items {
            if label.is_empty() {
                html.push_str("<div class=\"sep\"></div>");
            } else {
                html.push_str(&format!(
                    "<div class=\"item{}\" data-menu=\"{}\"><span>{}</span><span class=\"key\">{}</span></div>",
                    if *on { "" } else { " off" },
                    if *on { action } else { "" },
                    esc(label),
                    esc(keys)
                ));
            }
        }
        // Inside the window: open up / left when there is no room.
        let (w, h) = self.logical_size();
        let height = items.iter().map(|i| if i.0.is_empty() { 9.0 } else { 26.0 }).sum::<f32>() + 10.0;
        let width = 240.0;
        let left = if x + width > w { (x - width).max(0.0) } else { x };
        let top = if y + height > h { (y - height).max(0.0) } else { y };
        self.menu = Some(ctx);
        self.show_menu(Some((html, left, top)));
    }

    /// Shows the context menu element with `html` at `(left, top)`, or hides it.
    fn show_menu(&mut self, content: Option<(String, f32, f32)>) {
        let Some(m) = self.chrome_el("menu") else { return };
        let dom = self.chrome.dom_mut();
        match content {
            Some((html, left, top)) => {
                dom.set_inner_html(m, &html);
                if let Some(e) = dom.element_mut(m) {
                    e.set_attr("class", "show");
                    e.set_attr("style", &format!("left:{left}px;top:{top}px"));
                }
            }
            None => {
                if let Some(e) = dom.element_mut(m) {
                    e.set_attr("class", "");
                }
            }
        }
        self.chrome.dom_changed();
        self.set_state(Target::Chrome, |s| s.hover = None);
        self.dirty = true;
    }

    fn close_menu(&mut self) {
        if self.menu.take().is_some() {
            self.show_menu(None);
        }
    }

    /// The action of the menu item at window point `(x, y)`.
    fn menu_item_at(&self, x: f32, y: f32) -> Option<String> {
        let dom = &self.chrome.dom;
        let mut n = self.chrome.hit_test(x, y).element;
        while let Some(e) = n {
            if let Some(a) = dom.attr(e, "data-menu") {
                return (!a.is_empty()).then(|| a.to_string());
            }
            n = dom.parent(e);
        }
        None
    }

    fn menu_command(&mut self, action: &str, ctx: MenuCtx, host: &mut dyn Host) {
        let page_selection = |b: &Self| b.tabs[b.active].doc.selected_text();
        match action {
            "ctx-inspect" => {
                if let Some(n) = ctx.node {
                    self.inspect(n);
                }
            }
            "ctx-open-link" => {
                if let Some(u) = ctx.link {
                    self.navigate(&u, host);
                }
            }
            "ctx-open-link-tab" | "ctx-open-image-tab" => {
                if let Some(u) = if action == "ctx-open-link-tab" { ctx.link } else { ctx.image } {
                    let back = self.active;
                    self.new_tab(&u, host);
                    // like other browsers: the new tab opens behind
                    self.active = back;
                    self.dirty = true;
                }
            }
            "ctx-copy-link" => host.clipboard_set(ctx.link.as_deref().unwrap_or("")),
            "ctx-favorite" => {
                let t = &self.tabs[self.active];
                let (url, title) = (t.url().to_string(), t.doc.title().unwrap_or_default());
                self.open_installer("favorites/new", &[("url", &url), ("title", &title)], host);
            }
            "ctx-copy-image" => host.clipboard_set(ctx.image.as_deref().unwrap_or("")),
            "ctx-save-link" | "ctx-save-image" => {
                if let Some(u) = if action == "ctx-save-link" { ctx.link } else { ctx.image } {
                    self.download(self.active, &u, None, host);
                }
            }
            "ctx-copy" | "ctx-cut" => match ctx.field {
                Some((t, f)) => {
                    let text = self.doc_mut(t).input(f).selected_text();
                    host.clipboard_set(&text);
                    if action == "ctx-cut" && !text.is_empty() {
                        self.doc_mut(t).input(f).insert("");
                        self.edited(t, f, host);
                    }
                }
                None => {
                    let text = page_selection(self);
                    if !text.is_empty() {
                        host.clipboard_set(&text);
                    }
                }
            },
            "ctx-paste" => {
                if let (Some((t, f)), Some(text)) = (ctx.field, host.clipboard_get()) {
                    self.doc_mut(t).input(f).insert(&text);
                    self.edited(t, f, host);
                }
            }
            "ctx-select-all" => match ctx.field {
                Some((t, f)) => self.doc_mut(t).input(f).select_all(),
                None => self.tabs[self.active].doc.select_all_text(),
            },
            other => self.command(other, None, host),
        }
        self.dirty = true;
    }

    /// Text field `f` changed from the menu: the page hears `input`.
    fn edited(&mut self, t: Target, f: NodeId, host: &mut dyn Host) {
        self.caret_epoch = self.now;
        if t == Target::Page {
            self.dispatch(f, "input", EventInit::default(), host);
        }
    }

    fn key_down(&mut self, code: i32, mods: Modifiers, host: &mut dyn Host) {
        self.caret_epoch = self.now;
        self.dirty = true;
        if self.menu.is_some() {
            self.close_menu();
            if code == key::ESCAPE {
                return;
            }
        }
        // Browser shortcuts.
        let cmd = mods.command();
        // Copy and select-all of page text (text fields handle their own).
        if self.focused_text_input().is_none() && self.keyboard == Target::Page {
            let doc = &mut self.tabs[self.active].doc;
            match code {
                c if cmd && c == 'C' as i32 && doc.has_selection() => {
                    host.clipboard_set(&doc.selected_text());
                    return;
                }
                c if cmd && c == 'A' as i32 => {
                    doc.select_all_text();
                    return;
                }
                key::ESCAPE if doc.selection.is_some() => doc.set_selection(None),
                _ => {}
            }
        }
        // Developer tools: F12, Cmd+Opt+I / Ctrl+Shift+I; Cmd/Ctrl+Shift+C
        // picks an element.
        let devtools_key = code == key::F12
            || (code == 'I' as i32 && ((mods.meta && mods.alt) || (mods.ctrl && mods.shift)));
        if devtools_key {
            return self.toggle_devtools();
        }
        if code == 'C' as i32 && cmd && mods.shift {
            if !self.devtools.open {
                self.open_devtools("elements");
            }
            return self.devtools_command("inspect", host);
        }
        if self.devtools.open && self.devtools_key(code) {
            return;
        }
        match code {
            c if cmd && c == 'L' as i32 => return self.focus_address(true),
            c if cmd && c == 'T' as i32 => return self.new_tab("about:home", host),
            c if cmd && c == 'W' as i32 => return self.close_tab(self.active, host),
            c if cmd && c == 'R' as i32 => return self.reload(host),
            key::F5 => return self.reload(host),
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
                self.tab_switched();
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
            let init = EventInit { repeat: self.key_repeat, ..EventInit::key(code, mods.bits()) };
            if self.dispatch(target, "keydown", init, host) {
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
        if t == Target::Chrome && matches!(action, Action::Submit) && self.devtools_submit(n, host) {
            return;
        }
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
        self.touch_tick(host);
        for i in 0..self.tabs.len() {
            if std::mem::take(&mut self.tabs[i].kick) {
                self.run_scripts(i, host);
            }
        }
        if self.dirty {
            self.sync_chrome(host);
        }
        self.devtools_tick(host);
        self.sync_devtools();
        let (w, h) = self.logical_size();
        self.chrome.set_viewport(w, h);
        self.chrome.update(&self.fonts);
        let vp = self.viewport_rect();
        // The viewport size first: `srcset` / `sizes` depend on it; then
        // styles, which name background images.
        let doc = &mut self.tabs[self.active].doc;
        doc.set_viewport(vp.w, vp.h);
        doc.set_time(self.now);
        doc.update(&self.fonts);
        if doc.media_playing() {
            self.media_drawn = self.now;
        }
        if std::mem::take(&mut self.reveal_focus)
            && let Some((Target::Page, n)) = self.focused()
        {
            self.tabs[self.active].doc.reveal(n);
            self.dirty = true;
        }
        let i = self.active;
        // The page's viewport changed size (the keyboard, the window): `resize`.
        let size = (self.tabs[i].id, self.tabs[i].page, vp.w, vp.h);
        if size != self.shown_size {
            let same_page = (self.shown_size.0, self.shown_size.1) == (size.0, size.1);
            self.shown_size = size;
            if same_page && self.tabs[i].loaded {
                self.dispatch(crate::dom::Dom::ROOT, "resize", EventInit::default(), host);
            }
        }
        for (node, kind) in self.tabs[i].doc.take_media_events() {
            self.element_event(i, node, kind, host);
        }
        self.request_images(host);
    }

    /// Starts loading images of the active page that have a new `src`
    /// (from the HTML or set later by scripts).
    fn request_images(&mut self, host: &mut dyn Host) {
        self.request_resources(None, host);
        let tab = &mut self.tabs[self.active];
        let (id, serial) = (tab.id, tab.serial);
        for (node, load) in tab.doc.frame_requests() {
            match load {
                FrameLoad::Url(target) => {
                    let req = Request::Frame {
                        tab: id,
                        serial,
                        node,
                        source: target.clone(),
                        url: target.clone(),
                    };
                    self.fetch(req, &target, host);
                }
                FrameLoad::Html(html) => {
                    let tab = &mut self.tabs[self.active];
                    let base = tab.doc.base_url();
                    tab.doc
                        .frame_loaded(node, &format!("srcdoc:{html}"), &html, &base);
                    self.frame_loaded(self.active, node, host);
                }
            }
        }
        let frames: Vec<NodeId> = self.tabs[self.active].doc.frames.keys().copied().collect();
        for n in frames {
            self.request_resources(Some(n), host);
        }
    }

    /// Requests the stylesheets of the newly loaded document of `<iframe>`
    /// `node` in tab `i` (its other resources follow on updates).
    fn frame_loaded(&mut self, i: usize, node: NodeId, host: &mut dyn Host) {
        let tab = &mut self.tabs[i];
        let (id, serial) = (tab.id, tab.serial);
        let Some(doc) = tab.doc.frame_mut(node) else {
            return;
        };
        let base = doc.base_url();
        for (link, href) in doc.stylesheet_links() {
            let target = url::resolve(&base, &href);
            let req = Request::Stylesheet {
                tab: id,
                serial,
                frame: Some(node),
                link: Some(link),
                url: target.clone(),
            };
            self.fetch(req, &target, host);
        }
        self.dirty = true;
    }

    /// Requests `@import`s, fonts and images of the active page's document
    /// or one of its frames.
    fn request_resources(&mut self, frame: Option<NodeId>, host: &mut dyn Host) {
        let tab = &mut self.tabs[self.active];
        let (id, serial) = (tab.id, tab.serial);
        let Some(doc) = tab.doc_at(frame) else { return };
        let base = doc.base_url();
        let imports = doc.import_requests();
        let fonts = doc.font_requests();
        let images = doc.image_requests();
        for url in imports {
            let req = Request::Stylesheet {
                tab: id,
                serial,
                frame,
                link: None,
                url: url.clone(),
            };
            self.fetch(req, &url, host);
        }
        for font in fonts {
            let url = url::resolve(&base, &font.url);
            let req = Request::Font {
                tab: id,
                serial,
                frame,
                font,
            };
            self.fetch(req, &url, host);
        }
        for (node, src) in images {
            let req = Request::Image {
                tab: id,
                serial,
                frame,
                node,
                src: src.clone(),
            };
            self.fetch(req, &url::resolve(&base, &src), host);
        }
    }

    /// Whether the active page has animations, transitions or media
    /// running.
    pub fn animating(&self) -> bool {
        let doc = &self.tabs[self.active].doc;
        doc.animating() || doc.media_playing() || doc.next_image_frame().is_some() || self.fling.is_some()
    }

    /// Whether playing media is due for its next frame.
    fn media_due(&self, now: f64) -> bool {
        now - self.media_drawn >= MEDIA_FRAME_MS && self.tabs[self.active].doc.media_playing()
    }

    /// Whether a new frame should be drawn at time `now`.
    /// When the active page's render-blocking stylesheets stop holding it
    /// back (at most [`BLOCKING_MS`] after it was parsed); `None` if none load.
    fn blocked_until(&self) -> Option<f64> {
        let (sheets, since) = &self.tabs[self.active].blocking;
        (!sheets.is_empty()).then_some(since + BLOCKING_MS)
    }

    pub fn needs_redraw(&self, now: f64) -> bool {
        self.dirty
            || self.blocked_until().is_some_and(|t| t <= now)
            || self.touch_wakeup().is_some_and(|t| t <= now)
            || self.media_due(now)
            || self.tabs[self.active]
                .doc
                .next_image_frame()
                .is_some_and(|t| t <= now)
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
        // Playing media: the next video frame (about 60 per second).
        if self.tabs[self.active].doc.media_playing() {
            return Some(self.media_drawn + MEDIA_FRAME_MS);
        }
        let caret = self.focused_text_input().map(|_| {
            let phase = (now - self.caret_epoch) / CARET_BLINK_MS;
            self.caret_epoch + (phase.floor() + 1.0) * CARET_BLINK_MS
        });
        // The next frame of an animated image.
        let image = self.tabs[self.active].doc.next_image_frame();
        [caret, self.next_timer(), image, self.blocked_until(), self.touch_wakeup()]
            .into_iter()
            .flatten()
            .min_by(f64::total_cmp)
    }

    /// Draws the whole browser and returns the result.
    pub fn render(&mut self, now: f64, host: &mut dyn Host) -> &Canvas {
        self.now = now;
        // Frame timings for the developer tools' Performance panel.
        #[cfg(not(target_arch = "wasm32"))]
        let started = std::time::Instant::now();
        self.update_layout(host);
        #[cfg(not(target_arch = "wasm32"))]
        let updated = std::time::Instant::now();
        // A new document (or tab) under a pointer that has not moved: hover
        // it as if the pointer moved, so the status bubble and :hover
        // follow the page shown.
        let shown = (self.tabs[self.active].id, self.tabs[self.active].page);
        if shown != self.hovered {
            self.hovered = shown;
            if self.pointer.0 >= 0.0 && self.press.is_none() {
                let (x, y) = self.pointer;
                self.mouse_move(x, y, host);
                self.update_layout(host);
            }
        }
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
        // Render-blocking stylesheets still loading: the viewport stays blank.
        if self.blocked_until().is_none_or(|t| t <= now) {
            self.tabs[self.active]
                .doc
                .paint(&mut self.canvas, (vp.x, vp.y), scale, &self.fonts);
        }
        self.paint_devtools_overlay();
        #[cfg(not(target_arch = "wasm32"))]
        {
            let ms = |a: std::time::Instant, b: std::time::Instant| (b - a).as_secs_f64() * 1000.0;
            let done = std::time::Instant::now();
            self.record_frame(ms(started, updated), ms(updated, done));
        }
        // The status bubble and the context menu are part of the chrome but
        // sit above the page.
        // (repainted exactly: around them the chrome would paint its
        // viewport background over the page)
        for (id, pad) in [("status", 1.0), ("menu", 1.0)] {
        if let Some(s) = self.chrome_el(id).and_then(|s| {
            let st = self.chrome.style(s)?;
            (st.display != crate::css::style::Display::None).then_some(s)
        }) && let Some(r) = self.chrome.content_box(s)
        {
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
        }
        self.dirty = false;
        &self.canvas
    }
}

/// Every script the browser can run, in document order: language
/// runtimes declared with `<link rel="rubrowser-runtime" type href>` first,
/// then `<script>`s. Each entry is (language, `src`, inline text).
fn page_scripts(doc: &Document) -> Vec<(Lang, Option<String>, String, Option<NodeId>)> {
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
            None,
        ))
    });
    let scripts = all.iter().filter_map(|&n| {
        let (lang, src, inline) = script_entry(dom, n)?;
        Some((lang, src, inline, Some(n)))
    });
    runtimes.chain(scripts).collect()
}

/// What `<script>` element `n` runs: its language, `src` and inline text
/// (`None` for no script, e.g. JSON data or a `nomodule` fallback).
fn script_entry(dom: &crate::dom::Dom, n: NodeId) -> Option<(Lang, Option<String>, String)> {
    if dom.tag(n) != Some("script") {
        return None;
    }
    let src = dom.attr(n, "src").map(str::to_string);
    let ty = dom
        .attr(n, "type")
        .unwrap_or("")
        .trim()
        .to_ascii_lowercase();
    // `nomodule` scripts are fallbacks for browsers without modules.
    if dom.attr(n, "nomodule").is_some() || ty == "importmap" {
        return None;
    }
    let wasm_src = src.as_deref().is_some_and(|s| s.ends_with(".wasm"));
    let lang = match script_mime(&ty) {
        _ if ty == "module" => Lang::Module,
        Some(m) if m == JS_MIME && wasm_src => Lang::Wasm,
        Some(m) => Lang::Source(m),
        None => Lang::Wasm,
    };
    let inline = dom.text_content(n);
    if src.is_none() && (lang == Lang::Wasm || inline.trim().is_empty()) {
        return None;
    }
    Some((lang, src, inline))
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

/// The response for a `blob:` URL: its type and size.
fn blob_response(f: &crate::document::FileData) -> ResponseInfo {
    ResponseInfo {
        status: 200,
        headers: vec![
            ("content-type".into(), f.mime.clone()),
            ("content-length".into(), f.bytes.len().to_string()),
        ],
    }
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
        /// (url, method, body) of requests with a method.
        requests: Vec<(String, String, String)>,
        title: String,
        clipboard: Option<String>,
        /// (url, Referer) of requests that send one.
        referrers: Vec<(String, String)>,
        /// (url, Origin, Sec-Fetch-Site) of requests with a method.
        origins: Vec<(String, String, String)>,
    }

    impl Host for TestHost {
        fn fetch(&mut self, id: u32, url: &str) {
            self.fetched.push((id, url.to_string()));
        }
        fn fetch_with(&mut self, id: u32, url: &str, r: &crate::input::HttpRequest) {
            if !r.referrer.is_empty() {
                self.referrers.push((url.to_string(), r.referrer.clone()));
            }
            if r.method.is_empty() {
                return self.fetch(id, url);
            }
            self.fetched.push((id, url.to_string()));
            let body = String::from_utf8_lossy(&r.body).into_owned();
            self.requests
                .push((url.to_string(), r.method.clone(), body));
            self.origins.push((url.to_string(), r.origin.clone(), r.fetch_site.clone()));
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
        b.event(Event::KeyDown { key: k, mods, repeat: false }, 0.0, h);
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
    fn module_import_scanning() {
        let src = r#"import a from "./a.js";import*as i from"./b.js";export{x}from'./c.js';
            // import "./no.js"
            const re = /"/g, s = "import './no2.js'"; import("./d.js"); import "./e.js";
            const t = `from "./no3.js" ${f(`"./no5.js"`)} "x"`; x = a / 2; y = "./no4.js";
            import "./f.js";"#;
        assert_eq!(
            module_imports(src),
            ["./a.js", "./b.js", "./c.js", "./d.js", "./e.js", "./f.js"]
        );
    }

    #[test]
    fn post_forms_and_blank_targets() {
        let (mut b, mut h) = browser();
        b.navigate("https://w.org/form", &mut h);
        let id = h.fetched.last().unwrap().0;
        let page = br#"<form method=post action="/save?x=1"><input name=q value="a b">
            <button id=go>Go</button></form>
            <a id=out href="/other" target=_blank style="display:block">out</a>"#;
        b.fetch_done(id, Ok(page.to_vec()), &mut h);
        b.render(0.0, &mut h);
        click_page(&mut b, &mut h, "go");
        assert_eq!(
            h.requests.last().unwrap(),
            &(
                "https://w.org/save?x=1".to_string(),
                "POST".to_string(),
                "q=a+b".to_string()
            )
        );
        let id = h.fetched.last().unwrap().0;
        b.fetch_done(id, Ok(page.to_vec()), &mut h);
        b.render(0.0, &mut h);
        let tabs = b.tabs.len();
        click_page(&mut b, &mut h, "out");
        assert_eq!(b.tabs.len(), tabs + 1, "target=_blank opens a tab");
        assert_eq!(b.tabs[b.active].url(), "https://w.org/other");
    }

    #[test]
    fn form_posts_say_where_they_come_from() {
        let (mut b, mut h) = browser();
        b.navigate("http://evil.localhost:8700/", &mut h);
        let id = h.fetched.last().unwrap().0;
        let page = br#"<form method=post action="http://files.localhost:8700/delete"><input name=p value=x>
            <button id=go>Go</button></form>
            <form method=post action="/own"><button id=own>Own</button></form>"#;
        b.fetch_done(id, Ok(page.to_vec()), &mut h);
        b.render(0.0, &mut h);
        click_page(&mut b, &mut h, "go");
        assert_eq!(
            h.origins.last().unwrap(),
            &("http://files.localhost:8700/delete".to_string(), "http://evil.localhost:8700".to_string(), "cross-site".to_string())
        );
        b.navigate("http://evil.localhost:8700/", &mut h);
        let id = h.fetched.last().unwrap().0;
        b.fetch_done(id, Ok(page.to_vec()), &mut h);
        b.render(0.0, &mut h);
        click_page(&mut b, &mut h, "own");
        assert_eq!(
            h.origins.last().unwrap(),
            &("http://evil.localhost:8700/own".to_string(), "http://evil.localhost:8700".to_string(), "same-origin".to_string())
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
    fn back_returns_to_the_scroll_position() {
        let (mut b, mut h) = browser();
        let page = br##"<body style="margin:0"><div style="height:5000px">tall</div></body>"##;
        b.navigate("https://w.org/a", &mut h);
        b.fetch_done(h.fetched.last().unwrap().0, Ok(page.to_vec()), &mut h);
        b.render(0.0, &mut h);
        assert!(b.tabs[b.active].doc.scroll_by(10.0, 10.0, 1234.0));
        b.navigate("https://w.org/b", &mut h);
        b.fetch_done(h.fetched.last().unwrap().0, Ok(page.to_vec()), &mut h);
        b.render(0.0, &mut h);
        assert_eq!(b.tabs[b.active].doc.scroll_y(), 0.0, "a new page starts at the top");
        b.go(-1, &mut h);
        b.fetch_done(h.fetched.last().unwrap().0, Ok(page.to_vec()), &mut h);
        b.render(0.0, &mut h);
        assert_eq!(b.tabs[b.active].doc.scroll_y(), 1234.0, "back to where it was");
        b.go(1, &mut h);
        b.fetch_done(h.fetched.last().unwrap().0, Ok(page.to_vec()), &mut h);
        b.render(0.0, &mut h);
        assert_eq!(b.tabs[b.active].doc.scroll_y(), 0.0);
    }

    #[test]
    fn ruby_annotations_sit_above_their_bases() {
        let (mut b, mut h) = browser();
        b.navigate("https://w.org/ruby", &mut h);
        let page = r#"<body style="margin:0;font-size:20px">a<ruby><span id=base>漢</span><rp>(</rp><rt id=rt>kan</rt><rp>)</rp></ruby>b<span id=after style=display:inline-block>c</span>"#;
        b.fetch_done(h.fetched.last().unwrap().0, Ok(page.as_bytes().to_vec()), &mut h);
        b.render(0.0, &mut h);
        let doc = &b.tabs[b.active].doc;
        let rect = |id: &str| doc.content_box(page_el(&b, id)).unwrap();
        let (base, rt, after) = (rect("base"), rect("rt"), rect("after"));
        assert!(rt.y + rt.h <= base.y + 0.5, "annotation above: {rt:?} {base:?}");
        assert!((rt.x + rt.w / 2.0 - (base.x + base.w / 2.0)).abs() < 1.0, "centered: {rt:?} {base:?}");
        assert!((base.y + base.h - (after.y + after.h)).abs() < 1.0, "base on the line: {base:?} {after:?}");
    }

    #[test]
    fn pages_declaring_an_app_package_offer_to_install_it() {
        let (mut b, mut h) = browser();
        let hidden = |b: &Browser| b.chrome.dom.attr(b.chrome_el("install").unwrap(), "hidden").is_some();
        let page = r#"<head><link rel="icon" href="i.svg"><link rel="ruwebos-app" href="app.tar.gz"></head><p>app</p>"#;
        b.navigate("https://apps.test/notes/", &mut h);
        b.fetch_done(h.fetched.last().unwrap().0, Ok(page.as_bytes().to_vec()), &mut h);
        b.render(0.0, &mut h);
        assert!(hidden(&b), "no installer: no button");
        b.set_installer(Some("http://installer.localhost:8700".into()));
        b.render(0.0, &mut h);
        assert!(!hidden(&b), "the page declares a package");
        b.command("install", None, &mut h);
        assert_eq!(b.tabs[b.active].url(), "http://installer.localhost:8700/install?src=https%3A%2F%2Fapps.test%2Fnotes%2F");
        b.fetch_done(h.fetched.last().unwrap().0, Ok(b"<p>installer</p>".to_vec()), &mut h);
        b.render(0.0, &mut h);
        assert!(hidden(&b), "the installer page declares nothing");
    }

    #[test]
    fn pages_wait_for_their_stylesheets_before_showing() {
        let (mut b, mut h) = browser();
        b.navigate("https://css.test/", &mut h);
        let page = r#"<link rel=stylesheet href=a.css><p id=p style="font-size:30px">unstyled</p>"#;
        b.fetch_done(h.fetched.last().unwrap().0, Ok(page.as_bytes().to_vec()), &mut h);
        b.render(10.0, &mut h);
        let until = b.blocked_until().expect("a.css is loading");
        assert_eq!(until, BLOCKING_MS, "since the page was parsed");
        assert!(b.next_wakeup(10.0).is_some_and(|t| t <= until), "wakes up to show it anyway");
        let css = h.fetched.iter().find(|(_, u)| u.ends_with("a.css")).unwrap().0;
        b.fetch_done(css, Ok(b"p{color:red}".to_vec()), &mut h);
        assert_eq!(b.blocked_until(), None, "loaded: shown");
    }

    /// A finger: down, moves, up at window points, `step` ms apart.
    fn finger(b: &mut Browser, h: &mut dyn Host, t0: f64, step: f64, path: &[(i32, i32)]) -> f64 {
        let mut t = t0;
        for (i, &(x, y)) in path.iter().enumerate() {
            let phase = match i {
                0 => crate::input::TouchPhase::Start,
                i if i == path.len() - 1 => crate::input::TouchPhase::End,
                _ => crate::input::TouchPhase::Move,
            };
            b.event(Event::Touch { id: 1, x, y, phase }, t, h);
            t += step;
        }
        t
    }

    #[test]
    fn a_tap_is_a_click_with_touch_and_pointer_events() {
        let mut h = LogHost::default();
        let mut b = Browser::new(800, 600, 1.0);
        b.render(0.0, &mut h);
        b.navigate("https://touch.test/", &mut h);
        let id = h.inner.fetched.last().unwrap().0;
        let page = r#"<body style="margin:0"><button id=b style="width:200px;height:80px">tap</button><input id=i style="width:200px"><script>
              const b = document.getElementById('b');
              for (const t of ['pointerdown', 'touchstart', 'pointerup', 'touchend', 'mousedown', 'mouseup', 'click'])
                b.addEventListener(t, (e) => console.log(t + (e.pointerType ? ' ' + e.pointerType : '') + (e.changedTouches ? ' ' + e.changedTouches.length + ' ' + e.touches.length + ' ' + (e.changedTouches[0].target === b) : '')));
            </script>"#;
        b.fetch_done(id, Ok(page.as_bytes().to_vec()), &mut h);
        b.render(0.0, &mut h);
        let vp = b.viewport_rect();
        let (x, y) = ((vp.x + 50.0) as i32, (vp.y + 40.0) as i32);
        finger(&mut b, &mut h, 10.0, 50.0, &[(x, y), (x, y)]);
        let got: Vec<&String> = h.logs.iter().filter(|l| !l.starts_with('[')).collect();
        assert_eq!(
            got,
            ["pointerdown touch", "touchstart 1 1 true", "pointerup touch", "touchend 1 0 true", "mousedown", "mouseup", "click"],
            "{:?}",
            h.logs
        );
        // a tap focuses a text field
        let r = b.tabs[b.active].doc.content_box(page_el(&b, "i")).unwrap();
        let (ix, iy) = ((vp.x + r.x + 10.0) as i32, (vp.y + r.y + r.h / 2.0) as i32);
        finger(&mut b, &mut h, 200.0, 50.0, &[(ix, iy), (ix, iy)]);
        assert_eq!(b.tabs[b.active].doc.state().focus, Some(page_el(&b, "i")));
    }

    #[test]
    fn dragging_scrolls_and_keeps_sliding() {
        let (mut b, mut h) = browser();
        b.navigate("https://touch.test/long", &mut h);
        let page = r#"<body style="margin:0"><div style="height:20000px">long</div><div id=map style="touch-action:none;position:fixed;top:0;left:0;width:100px;height:100px"></div>"#;
        b.fetch_done(h.fetched.last().unwrap().0, Ok(page.as_bytes().to_vec()), &mut h);
        b.render(0.0, &mut h);
        let vp = b.viewport_rect();
        let x = (vp.x + 400.0) as i32;
        let y0 = (vp.y + 400.0) as i32;
        // 300 px up in 150 ms: the page scrolls with the finger
        let path: Vec<(i32, i32)> = (0..=10).map(|i| (x, y0 - i * 30)).collect();
        let t = finger(&mut b, &mut h, 0.0, 15.0, &path);
        let after_drag = b.tabs[b.active].doc.scroll_y();
        assert!((after_drag - 270.0).abs() < 31.0, "followed the finger: {after_drag}");
        assert!(b.animating(), "a fling");
        for i in 1..=60 {
            b.render(t + i as f64 * 16.0, &mut h);
        }
        let after_fling = b.tabs[b.active].doc.scroll_y();
        assert!(after_fling > after_drag + 200.0, "kept sliding: {after_drag} → {after_fling}");
        // touch-action: none keeps the drag to the page
        b.fling = None;
        let before = b.tabs[b.active].doc.scroll_y();
        let mx = (vp.x + 50.0) as i32;
        let my = (vp.y + 80.0) as i32;
        finger(&mut b, &mut h, 2000.0, 15.0, &[(mx, my), (mx, my - 30), (mx, my - 60)]);
        assert_eq!(b.tabs[b.active].doc.scroll_y(), before, "touch-action: none");
    }

    #[test]
    fn canceled_touches_and_long_presses() {
        let mut h = LogHost::default();
        let mut b = Browser::new(800, 600, 1.0);
        b.render(0.0, &mut h);
        b.navigate("https://touch.test/c", &mut h);
        let id = h.inner.fetched.last().unwrap().0;
        let page = r#"<body style="margin:0"><div id=d style="height:3000px">x</div><script>
              const d = document.getElementById('d');
              d.addEventListener('touchstart', (e) => { if (window.own) e.preventDefault(); });
              d.addEventListener('click', () => console.log('click'));
              d.addEventListener('contextmenu', () => console.log('contextmenu'));
            </script>"#;
        b.fetch_done(id, Ok(page.as_bytes().to_vec()), &mut h);
        b.render(0.0, &mut h);
        let vp = b.viewport_rect();
        let (x, y) = ((vp.x + 100.0) as i32, (vp.y + 100.0) as i32);
        // a long press opens the context menu, and is not a click
        b.event(Event::Touch { id: 3, x, y, phase: crate::input::TouchPhase::Start }, 0.0, &mut h);
        assert!(b.next_wakeup(10.0).is_some_and(|t| t <= touch::LONG_PRESS_MS), "wakes for the long press");
        b.render(touch::LONG_PRESS_MS + 1.0, &mut h);
        b.event(Event::Touch { id: 3, x, y, phase: crate::input::TouchPhase::End }, 700.0, &mut h);
        assert!(h.logs.iter().any(|l| l == "contextmenu") && !h.logs.iter().any(|l| l == "click"), "{:?}", h.logs);
        b.close_menu();
        // a page that cancels touchstart gets no click and no scrolling
        b.run_script("window.own = true", &mut h);
        h.logs.clear();
        finger(&mut b, &mut h, 1000.0, 20.0, &[(x, y), (x, y)]);
        finger(&mut b, &mut h, 1100.0, 15.0, &[(x, y), (x, y - 40), (x, y - 80)]);
        assert!(!h.logs.iter().any(|l| l == "click"), "{:?}", h.logs);
        assert_eq!(b.tabs[b.active].doc.scroll_y(), 0.0);
    }

    #[test]
    fn the_on_screen_keyboard_comes_up_for_touched_fields() {
        let mut h = LogHost::default();
        let mut b = Browser::new(800, 600, 1.0);
        b.render(0.0, &mut h);
        b.navigate("https://osk.test/", &mut h);
        let id = h.inner.fetched.last().unwrap().0;
        let page = r#"<body style="margin:0"><input id=t style="width:300px;height:30px" enterkeyhint=send><br><input id=n type=tel style="width:300px;height:30px"><script>
              const t = document.getElementById('t');
              t.addEventListener('keydown', (e) => console.log('keydown ' + e.key));
              t.addEventListener('input', () => console.log('input ' + t.value));
            </script>"#;
        b.fetch_done(id, Ok(page.as_bytes().to_vec()), &mut h);
        b.render(0.0, &mut h);
        let shown = |b: &Browser| b.chrome.dom.attr(b.chrome_el("osk").unwrap(), "hidden").is_none();
        let keys = |b: &Browser| -> Vec<String> {
            let dom = &b.chrome.dom;
            dom.descendants(b.chrome_el("osk").unwrap()).into_iter().filter_map(|n| dom.attr(n, "data-key").map(String::from)).collect()
        };
        let label = |b: &Browser, k: &str| -> String {
            let dom = &b.chrome.dom;
            let n = dom.descendants(b.chrome_el("osk").unwrap()).into_iter().find(|&n| dom.attr(n, "data-key") == Some(k)).unwrap();
            dom.text_content(n)
        };
        let at = |b: &Browser, id: &str| {
            let r = b.tabs[b.active].doc.content_box(page_el(b, id)).unwrap();
            let vp = b.viewport_rect();
            ((vp.x + r.x + 5.0) as i32, (vp.y + r.y + r.h / 2.0) as i32)
        };
        // a mouse click: no keyboard (auto)
        let (x, y) = at(&b, "t");
        b.event(Event::MouseDown { x, y, button: 0 }, 10.0, &mut h);
        b.event(Event::MouseUp { x, y, button: 0 }, 20.0, &mut h);
        b.render(30.0, &mut h);
        assert!(!shown(&b), "a mouse user");
        let full = b.viewport_rect().h;
        // a tap: the keyboard comes up, the page gets shorter
        finger(&mut b, &mut h, 100.0, 30.0, &[(x, y), (x, y)]);
        b.render(200.0, &mut h);
        assert!(shown(&b), "touched");
        assert!(b.viewport_rect().h < full - 150.0, "the page is shorter");
        assert_eq!(label(&b, "enter"), "傳送", "enterkeyhint");
        assert!(keys(&b).contains(&"q".to_string()));
        for k in ["shift", "h", "i", "page:symbols", "!", "backspace"] {
            b.osk_press(k, &mut h);
        }
        b.render(300.0, &mut h);
        assert_eq!(b.tabs[b.active].doc.input_value(page_el(&b, "t")), "Hi");
        assert!(h.logs.iter().any(|l| l == "keydown H") && h.logs.iter().any(|l| l == "input Hi!"), "{:?}", h.logs);
        // a phone field: the dial pad
        let (nx, ny) = at(&b, "n");
        finger(&mut b, &mut h, 400.0, 30.0, &[(nx, ny), (nx, ny)]);
        b.render(500.0, &mut h);
        assert!(keys(&b).contains(&"#".to_string()) && !keys(&b).contains(&"q".to_string()), "{:?}", keys(&b));
        // hidden by the user until the focus moves
        b.osk_press("hide", &mut h);
        b.render(600.0, &mut h);
        assert!(!shown(&b));
        finger(&mut b, &mut h, 700.0, 30.0, &[(x, y), (x, y)]);
        b.render(800.0, &mut h);
        assert!(shown(&b), "another field");
        // a physical key: the keyboard goes away
        b.event(Event::KeyDown { key: 'A' as i32, mods: Modifiers::default(), repeat: false }, 900.0, &mut h);
        b.render(950.0, &mut h);
        assert!(!shown(&b), "a physical keyboard");
        // never / always
        b.set_keyboard_mode(KeyboardMode::Never);
        finger(&mut b, &mut h, 1000.0, 30.0, &[(x, y), (x, y)]);
        b.render(1100.0, &mut h);
        assert!(!shown(&b), "never");
        b.set_keyboard_mode(KeyboardMode::Always);
        b.render(1200.0, &mut h);
        assert!(shown(&b), "always");
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
        fn fetch_with(&mut self, id: u32, url: &str, r: &crate::input::HttpRequest) {
            self.inner.fetch_with(id, url, r)
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
        fn ws_open(&mut self, id: u32, url: &str, protocols: &[String], origin: &str) {
            self.logs.push(format!("ws_open {id} {url} {} {origin}", protocols.join(",")));
        }
        fn ws_send(&mut self, id: u32, data: &[u8], text: bool) {
            self.logs.push(format!("ws_send {id} {} {text}", String::from_utf8_lossy(data)));
        }
        fn ws_close(&mut self, id: u32, code: u16, reason: &str) {
            self.logs.push(format!("ws_close {id} {code} {reason}"));
        }
    }

    #[test]
    fn websockets_go_through_the_host() {
        let mut h = LogHost::default();
        let mut b = Browser::new(1000, 700, 1.0);
        b.render(0.0, &mut h);
        b.navigate("https://ws.test/room", &mut h);
        let id = h.inner.fetched.last().unwrap().0;
        let page = r#"<script>
              const w = new WebSocket('/live?x=1', ['chat']);
              console.log('state ' + w.readyState + ' ' + w.url);
              w.binaryType = 'arraybuffer';
              w.onopen = () => { console.log('open ' + w.protocol); w.send('hi'); w.send(new Uint8Array([1, 2])); };
              w.addEventListener('message', (e) => console.log('message ' + (typeof e.data === 'string' ? e.data : new Uint8Array(e.data).join('+'))));
              w.onclose = (e) => console.log('close ' + e.code + ' ' + e.wasClean + ' ' + e.reason + ' ' + w.readyState);
              try { new WebSocket('ftp://x/'); } catch (e) { console.log('bad ' + e.name); }
              window.w = w;
            </script>"#;
        b.fetch_done(id, Ok(page.as_bytes().to_vec()), &mut h);
        b.render(0.0, &mut h);
        let open = h.logs.iter().find(|l| l.starts_with("ws_open ")).expect("opened").clone();
        assert!(open.ends_with(" wss://ws.test/live?x=1 chat https://ws.test"), "{open}");
        let gid: u32 = open.split(' ').nth(1).unwrap().parse().unwrap();
        b.ws_event(gid, WsEvent::Open("chat".into()), &mut h);
        b.ws_event(gid, WsEvent::Text("hello".into()), &mut h);
        b.ws_event(gid, WsEvent::Binary(vec![7, 8]), &mut h);
        b.ws_event(gid, WsEvent::Close { code: 1000, reason: "done here".into(), clean: true }, &mut h);
        for want in [
            "state 0 wss://ws.test/live?x=1",
            "bad SyntaxError",
            "open chat",
            &format!("ws_send {gid} hi true"),
            &format!("ws_send {gid} \u{1}\u{2} false"),
            "message hello",
            "message 7+8",
            "close 1000 true done here 3",
        ] {
            assert!(h.logs.iter().any(|l| l == want), "{want}: {:?}", h.logs);
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
    fn scripts_and_stylesheets_inserted_by_scripts_load() {
        let mut h = LogHost::default();
        let mut b = Browser::new(1000, 700, 1.0);
        b.render(0.0, &mut h);
        b.navigate("https://js.test/index.html", &mut h);
        let id = h.inner.fetched.last().unwrap().0;
        let page = r#"<p id=p>x</p><script>
              console.log('current ' + document.currentScript.tagName);
              const l = document.createElement('link');
              l.rel = 'stylesheet';
              l.href = 's.css';
              l.onload = () => console.log('css load');
              document.head.appendChild(l);
              const s = document.createElement('script');
              s.src = 'd.js';
              s.addEventListener('load', () => console.log('js load'));
              document.head.appendChild(s);
              document.body.insertAdjacentHTML('beforeend', '<script>console.log("innerHTML ran")<\/script>');
              const t = document.createElement('script');
              t.textContent = 'console.log("inline ran")';
              document.body.appendChild(t);
            </script>"#;
        b.fetch_done(id, Ok(page.as_bytes().to_vec()), &mut h);
        b.render(0.0, &mut h);
        let find = |h: &LogHost, url: &str| h.inner.fetched.iter().find(|f| f.1 == url).unwrap().0;
        let (css, js) = (
            find(&h, "https://js.test/s.css"),
            find(&h, "https://js.test/d.js"),
        );
        b.fetch_done(js, Ok(b"console.log('d ran')".to_vec()), &mut h);
        b.fetch_done(css, Ok(b"#p { color: red }".to_vec()), &mut h);
        b.render(0.0, &mut h);
        for want in [
            "current SCRIPT",
            "inline ran",
            "d ran",
            "js load",
            "css load",
        ] {
            assert!(h.logs.iter().any(|l| l == want), "{want}: {:?}", h.logs);
        }
        assert!(!h.logs.iter().any(|l| l == "innerHTML ran"), "{:?}", h.logs);
        let doc = &b.tabs[b.active].doc;
        let p = doc.dom.element_by_id("p").unwrap();
        assert_eq!(doc.style(p).unwrap().color.rgb(), 0xff0000);
    }

    #[test]
    fn scripts_see_layout_selector_errors_and_html_documents() {
        let mut h = LogHost::default();
        let mut b = Browser::new(1000, 700, 1.0);
        b.render(0.0, &mut h);
        b.navigate("https://js.test/index.html", &mut h);
        let id = h.inner.fetched.last().unwrap().0;
        let page = r#"<div id=box style="width:120px;height:30px"></div><script>
              const box = document.getElementById('box');
              console.log('width ' + box.offsetWidth);
              box.style.width = '50px';
              console.log('after ' + box.getBoundingClientRect().width);
              for (const sel of [':visible', 'div[', '#box:hover']) {
                try { document.querySelector(sel); console.log('ok ' + sel); }
                catch (e) { console.log(e.name + ' ' + sel); }
              }
              const tpl = document.createElement('template');
              tpl.innerHTML = '<li>a</li><li>b</li>';
              const ul = document.createElement('ul');
              ul.append(tpl.content.cloneNode(true), document.importNode(tpl.content, true));
              console.log('template ' + ul.children.length + ' ' + tpl.content.childNodes.length);
              const meta = document.createElement('meta');
              meta.content = 'x';
              console.log('meta ' + meta.content);
              console.log('annex-b ' + 'abcdef'.substr(1, 3) + ' ' + document.createElement('_x').localName);
              console.log('frames ' + (window.frames === window) + ' ' + window.length);
              window.addEventListener('message', (e) => console.log('message ' + e.data.n));
              window.postMessage({ n: 7 }, '*');
              const root = document.createElement('div');
              root.innerHTML = '<p id=a>x<b id=b>y</b></p><i id=c><u id=d>z</u></i><s id=e></s>';
              const tw = document.createTreeWalker(root, NodeFilter.SHOW_ELEMENT, (n) =>
                n.id === 'a' ? NodeFilter.FILTER_SKIP : n.id === 'c' ? NodeFilter.FILTER_REJECT : NodeFilter.FILTER_ACCEPT);
              const seen = [];
              while (tw.nextNode()) seen.push(tw.currentNode.id);
              const back = [];
              while (tw.previousNode()) back.push(tw.currentNode.localName);
              const it = document.createNodeIterator(root, NodeFilter.SHOW_TEXT);
              const texts = [];
              let t;
              while ((t = it.nextNode())) texts.push(t.data);
              console.log('walk ' + seen.join() + ' ' + back.join() + ' ' + texts.join());
              const me = new MouseEvent('click', { clientX: 5, button: 2, ctrlKey: true });
              const ke = new KeyboardEvent('keydown', { key: 'a', code: 'KeyA' });
              console.log('events ' + (me instanceof UIEvent) + ' ' + (me instanceof Event) + ' ' + me.clientX + ' ' + me.button + ' ' + me.getModifierState('Control') + ' ' + ke.code + ' ' + new PointerEvent('pointerdown', { pointerType: 'mouse' }).pointerType + ' ' + typeof WheelEvent.DOM_DELTA_LINE);
              const doc = document.implementation.createHTMLDocument('t');
              doc.body.innerHTML = '<form></form><form></form>';
              console.log('forms ' + doc.body.childNodes.length + ' ' + doc.title);
            </script>"#;
        b.fetch_done(id, Ok(page.as_bytes().to_vec()), &mut h);
        b.render(0.0, &mut h);
        for want in [
            "width 120",
            "after 50",
            "SyntaxError :visible",
            "SyntaxError div[",
            "ok #box:hover",
            "forms 2 t",
            "template 4 2",
            "annex-b bcd _x",
            "frames true 0",
            "message 7",
            "meta x",
            "walk b,e b,div x,y,z",
            "events true true 5 2 true KeyA mouse number",
        ] {
            assert!(h.logs.iter().any(|l| l == want), "{want}: {:?}", h.logs);
        }
    }

    #[test]
    fn script_images_fire_load_and_send_a_referrer() {
        let mut h = LogHost::default();
        let mut b = Browser::new(1000, 700, 1.0);
        b.render(0.0, &mut h);
        b.navigate("http://map.test/page", &mut h);
        let id = h.inner.fetched.last().unwrap().0;
        let page = r#"<script>
              for (const src of ['https://tiles.test/1.png', 'https://tiles.test/broken.png']) {
                const img = document.createElement('img');
                img.onload = () => console.log('load ' + img.naturalWidth + 'x' + img.naturalHeight + ' ' + img.complete);
                img.onerror = () => console.log('error ' + img.naturalWidth + ' ' + img.complete);
                img.src = src;
                document.body.appendChild(img);
              }
              console.log('before ' + document.images[0].complete);
            </script>"#;
        b.fetch_done(id, Ok(page.as_bytes().to_vec()), &mut h);
        b.render(0.0, &mut h);
        let mut png = Vec::new();
        image::RgbaImage::new(3, 2).write_to(&mut std::io::Cursor::new(&mut png), image::ImageFormat::Png).unwrap();
        let find = |h: &LogHost, u: &str| h.inner.fetched.iter().find(|(_, url)| url == u).map(|(id, _)| *id).unwrap();
        let (ok, bad) = (find(&h, "https://tiles.test/1.png"), find(&h, "https://tiles.test/broken.png"));
        b.fetch_done(ok, Ok(png), &mut h);
        b.fetch_done(bad, Ok(b"not an image".to_vec()), &mut h);
        b.render(0.0, &mut h);
        for want in ["before false", "load 3x2 true", "error 0 true"] {
            assert!(h.logs.iter().any(|l| l == want), "{want}: {:?}", h.logs);
        }
        // cross-origin resources get the page's origin as Referer (strict-origin-when-cross-origin)
        assert!(h.inner.referrers.iter().any(|(u, r)| u == "https://tiles.test/1.png" && r == "http://map.test/"), "{:?}", h.inner.referrers);
    }

    #[test]
    fn mouse_and_pointer_events_drag_and_hash_navigation() {
        let mut h = LogHost::default();
        let mut b = Browser::new(1000, 700, 1.0);
        b.render(0.0, &mut h);
        b.navigate("https://drag.test/", &mut h);
        let id = h.inner.fetched.last().unwrap().0;
        let page = r##"<style>body{margin:0} #d{width:300px;height:200px;font-size:20px}</style>
            <div id=d>drag me, not my text</div><a id=cat href="#cat=2">分類</a>
            <script>
              const d = document.getElementById('d');
              const log = (s) => console.log(s);
              d.addEventListener('pointerdown', (e) => log('pointerdown ' + e.pointerType + ' ' + e.isPrimary + ' ' + e.buttons));
              d.addEventListener('mousedown', (e) => {
                log('down ' + e.clientX + ',' + e.clientY + ' ' + e.buttons);
                document.addEventListener('mousemove', (m) => log('move ' + m.clientX + ',' + m.clientY + ' ' + m.buttons));
                document.addEventListener('mouseup', () => log('up'), { once: true });
              });
              window.addEventListener('selectstart', (e) => e.preventDefault());
              window.onhashchange = () => log('hashchange ' + location.hash);
              document.getElementById('cat').addEventListener('click', (e) => { e.preventDefault(); location.hash = e.currentTarget.getAttribute('href'); });
            </script>"##;
        b.fetch_done(id, Ok(page.as_bytes().to_vec()), &mut h);
        b.render(0.0, &mut h);
        let vp = b.viewport_rect();
        let (x, y) = ((vp.x + 50.0) as i32, (vp.y + 20.0) as i32);
        b.event(Event::MouseMove { x, y }, 0.0, &mut h);
        b.event(Event::MouseDown { x, y, button: 0 }, 0.0, &mut h);
        b.event(Event::MouseMove { x: x + 40, y: y + 30 }, 0.0, &mut h);
        b.event(Event::MouseUp { x: x + 40, y: y + 30, button: 0 }, 0.0, &mut h);
        for want in ["pointerdown mouse true 1", "down 50,20 1", "move 90,50 1", "up"] {
            assert!(h.logs.iter().any(|l| l == want), "{want}: {:?}", h.logs);
        }
        assert!(!b.tabs[b.active].doc.has_selection(), "selectstart was canceled");
        // a #fragment change by script: no reload, hashchange
        let fetched = h.inner.fetched.len();
        let (cx, cy) = text_at(&b, "分類", 1);
        b.event(Event::MouseDown { x: cx, y: cy, button: 0 }, 1000.0, &mut h);
        b.event(Event::MouseUp { x: cx, y: cy, button: 0 }, 1000.0, &mut h);
        b.render(1000.0, &mut h);
        assert!(h.logs.iter().any(|l| l == "hashchange #cat=2"), "{:?}", h.logs);
        assert_eq!(b.tabs[b.active].url(), "https://drag.test/#cat=2");
        assert_eq!(h.inner.fetched.len(), fetched, "no reload");
        // back: popstate and hashchange to the empty fragment
        b.go(-1, &mut h);
        assert!(h.logs.iter().any(|l| l == "hashchange "), "{:?}", h.logs);
    }

    #[test]
    fn web_audio_oscillators_play_and_end() {
        let mut h = LogHost::default();
        let mut b = Browser::new(1000, 700, 1.0);
        b.render(0.0, &mut h);
        b.navigate("https://audio.test/", &mut h);
        let id = h.inner.fetched.last().unwrap().0;
        let page = r#"<button id=play style="width:200px;height:50px">play</button><script>
              const log = (s) => console.log(s);
              const ac = new (window.AudioContext || window.webkitAudioContext)();
              log('state ' + ac.state + ' ' + ac.sampleRate);
              document.getElementById('play').onclick = () => {
                ac.resume();
                log('resumed ' + ac.state);
                const t = ac.currentTime;
                const o = ac.createOscillator(), g = ac.createGain();
                o.type = 'triangle'; o.frequency.value = 440;
                g.gain.setValueAtTime(0, t);
                g.gain.linearRampToValueAtTime(0.5, t + 0.1);
                o.connect(g); g.connect(ac.destination);
                o.onended = () => log('ended ' + (ac.currentTime >= t + 0.2));
                o.start(); o.stop(t + 0.2);
                log('type ' + o.type + ' freq ' + o.frequency.value + ' gain ' + (g.gain.value < 0.01));
                try { g.gain.exponentialRampToValueAtTime(0, t + 1); } catch (e) { log(e.name); }
              };
            </script>"#;
        b.fetch_done(id, Ok(page.as_bytes().to_vec()), &mut h);
        b.render(0.0, &mut h);
        let vp = b.viewport_rect();
        let r = b.tabs[b.active].doc.content_box(b.tabs[b.active].doc.dom.element_by_id("play").unwrap()).unwrap();
        let (x, y) = ((vp.x + r.x + 10.0) as i32, (vp.y + r.y + 10.0) as i32);
        b.event(Event::MouseDown { x, y, button: 0 }, 100.0, &mut h);
        b.event(Event::MouseUp { x, y, button: 0 }, 100.0, &mut h);
        for t in [200.0, 400.0, 600.0, 800.0] {
            b.render(t, &mut h);
        }
        for want in ["state suspended 48000", "resumed running", "type triangle freq 440 gain true", "RangeError", "ended true"] {
            assert!(h.logs.iter().any(|l| l == want), "{want}: {:?}", h.logs);
        }
    }

    #[test]
    fn keyup_and_repeat_reach_the_page() {
        let mut h = LogHost::default();
        let mut b = Browser::new(1000, 700, 1.0);
        b.render(0.0, &mut h);
        b.navigate("https://keys.test/", &mut h);
        let id = h.inner.fetched.last().unwrap().0;
        let page = r#"<p>piano</p><script>
              addEventListener('keydown', (e) => console.log('down ' + e.key + ' ' + e.repeat));
              addEventListener('keyup', (e) => console.log('up ' + e.key + ' ' + e.repeat));
            </script>"#;
        b.fetch_done(id, Ok(page.as_bytes().to_vec()), &mut h);
        b.render(0.0, &mut h);
        let none = Modifiers::default();
        b.event(Event::KeyDown { key: 'A' as i32, mods: none, repeat: false }, 0.0, &mut h);
        b.event(Event::KeyDown { key: 'A' as i32, mods: none, repeat: true }, 0.0, &mut h);
        b.event(Event::KeyUp { key: 'A' as i32, mods: none }, 0.0, &mut h);
        let keys: Vec<&String> = h.logs.iter().filter(|l| l.starts_with("down") || l.starts_with("up")).collect();
        assert_eq!(keys, ["down a false", "down a true", "up a false"], "{:?}", h.logs);
    }

    #[test]
    fn intl_formats_dates_numbers_and_lists() {
        let mut h = LogHost::default();
        let mut b = Browser::new(1000, 700, 1.0);
        b.render(0.0, &mut h);
        b.navigate("https://intl.test/", &mut h);
        let id = h.inner.fetched.last().unwrap().0;
        let page = r#"<script>
              const d = new Date(Date.UTC(2026, 10, 3, 14, 5, 9));
              const tz = { timeZone: 'UTC' };
              const f = (o) => new Intl.DateTimeFormat('en-US', { ...tz, ...o }).format(d);
              const log = (s) => console.log(s);
              log('d1 ' + f({}));
              log('d2 ' + f({ month: 'short', day: 'numeric' }));
              log('d3 ' + f({ dateStyle: 'medium' }));
              log('d4 ' + f({ dateStyle: 'full', timeStyle: 'short' }));
              log('d5 ' + f({ hour: 'numeric', minute: '2-digit', hour12: false }));
              log('d6 ' + new Intl.DateTimeFormat('en', { ...tz, month: 'long', year: 'numeric' }).formatToParts(d).map((p) => p.type).join());
              log('d7 ' + d.toLocaleDateString('en-US', { ...tz, weekday: 'short', month: 'short', day: 'numeric' }));
              log('z1 ' + new Intl.DateTimeFormat('en-US', { timeZone: 'Asia/Taipei', hour: 'numeric', minute: '2-digit' }).format(d));
              log('z2 ' + new Intl.DateTimeFormat('en-US', { timeZone: 'America/New_York', dateStyle: 'short', timeStyle: 'short' }).format(d));
              log('z3 ' + new Intl.DateTimeFormat('zh-TW', { timeZone: 'Asia/Taipei', dateStyle: 'medium', timeStyle: 'short' }).format(d));
              log('z4 ' + Intl.DateTimeFormat('zh-TW', { timeZone: 'Asia/Tokyo', weekday: 'long', year: 'numeric', month: 'long', day: 'numeric' }).format(d));
              const zh = (o) => new Intl.DateTimeFormat('zh-TW', { ...tz, ...o }).format(d);
              log('z7 ' + [zh({ dateStyle: 'full', timeStyle: 'short' }), zh({ dateStyle: 'short', timeStyle: 'short' }), zh({ month: 'numeric', day: 'numeric', weekday: 'short' }), zh({ weekday: 'long', hour: 'numeric' }), zh({ year: 'numeric', month: 'numeric', day: 'numeric', weekday: 'long' }), zh({ hour: '2-digit', minute: '2-digit', hourCycle: 'h23' })].join(' | '));
              const sv = (o) => new Intl.DateTimeFormat('sv', { ...tz, ...o }).format(d);
              log('s1 ' + [d.toLocaleDateString('sv', tz), d.toLocaleString('sv-SE', tz), sv({ dateStyle: 'full' }), sv({ dateStyle: 'medium', timeStyle: 'short' }), sv({ month: 'numeric', day: 'numeric' }), sv({ weekday: 'short' }), sv({ year: 'numeric', month: '2-digit' })].join(' | '));
              log('c1 ' + [d.toLocaleDateString('en-CA', tz), d.toLocaleString('en-CA', tz), new Intl.DateTimeFormat('en-CA', { ...tz, dateStyle: 'medium' }).format(d)].join(' | '));
              try { new Intl.DateTimeFormat('en-US', { timeZone: 'Nowhere/Atlantis' }); } catch (e) { log('z5 ' + e.name); }
              log('z6 ' + Intl.supportedValuesOf('timeZone').includes('Europe/London'));
              const n = (v, o) => new Intl.NumberFormat('en-US', o).format(v);
              log('n1 ' + n(1234567.891));
              log('n2 ' + n(0.256, { style: 'percent' }));
              log('n3 ' + n(-1234.5, { style: 'currency', currency: 'USD' }));
              log('n4 ' + n(1530, { notation: 'compact' }) + ' ' + n(2400000, { notation: 'compact', compactDisplay: 'long' }));
              log('n5 ' + n(5, { minimumIntegerDigits: 2 }) + ' ' + n(1.5, { minimumFractionDigits: 2 }) + ' ' + (12345.678).toLocaleString());
              log('n6 ' + [1, 2].map(new Intl.NumberFormat().format).join('|'));
              const r = new Intl.RelativeTimeFormat('en', { numeric: 'auto' });
              log('r ' + r.format(-1, 'day') + ' / ' + r.format(3, 'hours') + ' / ' + r.format(-2, 'month'));
              log('l ' + new Intl.ListFormat('en').format(['a', 'b', 'c']) + ' / ' + new Intl.ListFormat('en', { type: 'disjunction' }).format(['a', 'b']));
              log('p ' + [1, 2, 3, 4, 11].map((v) => new Intl.PluralRules('en', { type: 'ordinal' }).select(v)).join());
              log('c ' + ['b', 'a', 'B', 'á', 'item10', 'item2'].sort(new Intl.Collator('en', { numeric: true }).compare).join());
            </script>"#;
        b.fetch_done(id, Ok(page.as_bytes().to_vec()), &mut h);
        b.render(0.0, &mut h);
        for want in [
            "d1 11/3/2026",
            "d2 Nov 3",
            "d3 Nov 3, 2026",
            "d4 Tuesday, November 3, 2026 at 2:05 PM",
            "d5 14:05",
            "d6 month,literal,year",
            "d7 Tue, Nov 3",
            "z1 10:05 PM",
            "z2 11/3/26, 9:05 AM",
            "z3 2026年11月3日 晚上10:05",
            "z4 2026年11月3日星期二",
            "s1 2026-11-03 | 2026-11-03 14:05:09 | tisdag 3 november 2026 | 3 nov. 2026 14:05 | 3/11 | tis | 2026-11",
            "c1 2026-11-03 | 2026-11-03, 2:05:09 p.m. | Nov 3, 2026",
            "z5 RangeError",
            "z6 true",
            "z7 2026年11月3日 星期二 下午2:05 | 2026/11/3 下午2:05 | 11/3（週二） | 星期二下午2時 | 2026年11月3日 星期二 | 14:05",
            "n1 1,234,567.891",
            "n2 26%",
            "n3 -$1,234.50",
            "n4 1.5K 2.4 million",
            "n5 05 1.50 12,345.678",
            "n6 1|2",
            "r yesterday / in 3 hours / 2 months ago",
            "l a, b, and c / a or b",
            "p one,two,few,other,other",
            "c a,á,b,B,item2,item10",
        ] {
            assert!(h.logs.iter().any(|l| l == want), "{want}: {:?}", h.logs);
        }
    }

    #[test]
    fn range_inputs_follow_the_pointer() {
        let mut h = LogHost::default();
        let mut b = Browser::new(1000, 700, 1.0);
        b.render(0.0, &mut h);
        b.navigate("https://js.test/index.html", &mut h);
        let id = h.inner.fetched.last().unwrap().0;
        let page = r#"<body style="margin:0"><input id=r type=range min=0 max=10 style="margin:0">
            <input id=c type=color><script>
              const r = document.getElementById('r');
              console.log('start ' + r.value + ' ' + document.getElementById('c').value);
              r.addEventListener('input', () => console.log('input ' + r.value));
              r.addEventListener('change', () => console.log('change ' + r.value));
            </script>"#;
        b.fetch_done(id, Ok(page.as_bytes().to_vec()), &mut h);
        b.render(0.0, &mut h);
        let n = page_el(&b, "r");
        let area = b.tabs[b.active].doc.content_box(n).unwrap();
        let vp = b.viewport_rect();
        let y = (vp.y + area.y + area.h / 2.0) as i32;
        let at = |f: f32| (vp.x + area.x + 8.0 + (area.w - 16.0) * f) as i32;
        b.event(
            Event::MouseDown {
                x: at(0.2),
                y,
                button: 0,
            },
            0.0,
            &mut h,
        );
        b.event(Event::MouseMove { x: at(0.9), y }, 0.0, &mut h);
        b.event(
            Event::MouseUp {
                x: at(0.9),
                y,
                button: 0,
            },
            0.0,
            &mut h,
        );
        for want in ["start 5 #000000", "input 2", "input 9", "change 9"] {
            assert!(h.logs.iter().any(|l| l == want), "{want}: {:?}", h.logs);
        }
    }

    #[test]
    fn inline_modules_run_in_order() {
        let mut h = LogHost::default();
        let mut b = Browser::new(1000, 700, 1.0);
        b.render(0.0, &mut h);
        b.navigate("https://js.test/index.html", &mut h);
        let id = h.inner.fetched.last().unwrap().0;
        let page = r#"<script type=module>console.log('mod1')</script>
            <script type=module>console.log('mod2 ' + typeof globalThis.x)</script>
            <script>var x = 1; console.log('classic');
              document.addEventListener('DOMContentLoaded', () => console.log('dcl'));</script>"#;
        b.fetch_done(id, Ok(page.as_bytes().to_vec()), &mut h);
        b.render(0.0, &mut h);
        let order: Vec<&str> = h.logs.iter().map(String::as_str).collect();
        // Module scripts run after the classic one (they are deferred) but
        // before DOMContentLoaded.
        assert_eq!(
            order,
            ["classic", "mod1", "mod2 number", "dcl"],
            "{order:?}"
        );
    }

    #[test]
    fn canvas_2d_draws_into_the_page() {
        let mut h = LogHost::default();
        let mut b = Browser::new(1000, 700, 1.0);
        b.render(0.0, &mut h);
        b.navigate("https://js.test/index.html", &mut h);
        let id = h.inner.fetched.last().unwrap().0;
        let page = r#"<body style="margin:0"><canvas id=c width=100 height=50></canvas><script>
              const x = document.getElementById('c').getContext('2d');
              x.fillStyle = '#ff0000'; x.fillRect(0, 0, 50, 50);
              x.translate(50, 0); x.fillStyle = 'rgb(0, 0, 255)'; x.fillRect(0, 0, 50, 50);
              console.log('px ' + [...x.getImageData(75, 25, 1, 1).data].join(','));
              x.beginPath(); x.arc(25, 25, 10, 0, Math.PI * 2);
              console.log('in ' + x.isPointInPath(75, 25) + ' ' + x.isPointInPath(5, 5));
              console.log('size ' + x.canvas.width + 'x' + x.canvas.height);
              const y = document.createElement('canvas').getContext('2d');
              y.canvas.width = 60; y.canvas.height = 20;
              y.shadowColor = 'rgb(0, 255, 0)'; y.shadowOffsetX = 20;
              y.fillStyle = '#ff0000'; y.fillRect(0, 0, 10, 10);
              const p = (x0, y0) => [...y.getImageData(x0, y0, 1, 1).data].join(',');
              console.log('shadow ' + p(5, 5) + ' ' + p(25, 5));
              y.shadowColor = 'transparent'; y.filter = 'invert(1)';
              y.fillRect(40, 0, 10, 10);
              console.log('filter ' + p(45, 5));
            </script>"#;
        b.fetch_done(id, Ok(page.as_bytes().to_vec()), &mut h);
        let vp = b.viewport_rect();
        let canvas = b.render(0.0, &mut h);
        let at = |x: f32, y: f32| {
            canvas.pixels()[((vp.y + y) as usize) * canvas.width() + (vp.x + x) as usize]
        };
        assert_eq!(at(10.0, 10.0), 0xff0000);
        assert_eq!(at(90.0, 10.0), 0x0000ff);
        for want in [
            "px 0,0,255,255",
            "in true false",
            "size 100x50",
            "shadow 255,0,0,255 0,255,0,255",
            "filter 0,255,255,255",
        ] {
            assert!(h.logs.iter().any(|l| l == want), "{want}: {:?}", h.logs);
        }
    }

    #[test]
    fn media_elements_without_a_player() {
        let mut h = LogHost::default();
        let mut b = Browser::new(1000, 700, 1.0);
        b.render(0.0, &mut h);
        b.navigate("https://js.test/index.html", &mut h);
        let id = h.inner.fetched.last().unwrap().0;
        let page = r#"<video id=v controls src="clip.mp4"></video><audio id=a></audio><script>
              const v = document.getElementById('v');
              console.log('state ' + v.paused + ' ' + isNaN(v.duration) + ' ' + v.currentSrc.endsWith('clip.mp4') + ' ' + typeof v.canPlayType('video/mp4'));
              v.play().catch((e) => console.log('rejected ' + e.name));
              v.muted = true;
              console.log('muted ' + v.muted + ' ' + v.controls + ' ' + v.loop);
            </script>"#;
        b.fetch_done(id, Ok(page.as_bytes().to_vec()), &mut h);
        b.render(0.0, &mut h);
        let doc = &b.tabs[b.active].doc;
        let (v, a) = (page_el(&b, "v"), page_el(&b, "a"));
        // 300 x 150 by default; audio without controls is not shown.
        let r = doc.content_box(v).unwrap();
        assert_eq!((r.w, r.h), (300.0, 150.0));
        assert!(doc.content_box(a).is_none());
        let want: &[&str] = if cfg!(feature = "media") {
            &["muted true true false"]
        } else {
            &[
                "state true true true string",
                "rejected NotSupportedError",
                "muted true true false",
            ]
        };
        for w in want {
            assert!(h.logs.iter().any(|l| l == w), "{w}: {:?}", h.logs);
        }
    }

    #[test]
    fn document_write_inserts_after_the_script() {
        let mut h = LogHost::default();
        let mut b = Browser::new(1000, 700, 1.0);
        b.render(0.0, &mut h);
        b.navigate("https://js.test/index.html", &mut h);
        let id = h.inner.fetched.last().unwrap().0;
        let page = r#"<p id=before>a</p>
            <script>document.write('<ul id=list><li>one'); document.write('</li><li>two</li></ul>');
              document.write('<script>console.log("written script ran")<\/script>');</script>
            <p id=after>b</p>
            <script>setTimeout(() => document.write('<p id=late>no</p>'), 0);</script>"#;
        b.fetch_done(id, Ok(page.as_bytes().to_vec()), &mut h);
        b.render(0.0, &mut h);
        b.render(10.0, &mut h);
        let doc = &b.tabs[b.active].doc;
        let list = doc.dom.element_by_id("list").expect("written list");
        assert_eq!(doc.dom.text_content(list), "onetwo");
        let body = doc.dom.body().unwrap();
        let order: Vec<&str> = doc
            .dom
            .children(body)
            .iter()
            .filter_map(|&c| doc.dom.attr(c, "id"))
            .collect();
        assert_eq!(&order[..3], &["before", "list", "after"]);
        assert!(
            h.logs.iter().any(|l| l == "written script ran"),
            "{:?}",
            h.logs
        );
        assert!(doc.dom.element_by_id("late").is_none());
    }

    #[test]
    fn get_computed_style_and_match_media() {
        let mut h = LogHost::default();
        let mut b = Browser::new(1000, 700, 1.0);
        b.render(0.0, &mut h);
        b.navigate("https://js.test/index.html", &mut h);
        let id = h.inner.fetched.last().unwrap().0;
        let page = r#"<style>
              .box { display: flex; color: #f00; margin: 4px 8px; --gap: 3px }
              @media (max-width: 400px) { .box { display: none } }
              .hidden { display: none }
            </style>
            <div class=box id=box style="padding-left: 2px">x</div>
            <script>
              const box = document.getElementById('box');
              const cs = getComputedStyle(box);
              console.log([cs.display, cs.color, cs.marginLeft, cs.paddingLeft,
                cs.getPropertyValue('--gap'), cs.fontWeight].join('|'));
              box.classList.add('hidden');
              console.log('after ' + getComputedStyle(box).display);
              console.log('media ' + matchMedia('(min-width: 500px)').matches + ' ' +
                matchMedia('(max-width: 400px)').matches);
            </script>"#;
        b.fetch_done(id, Ok(page.as_bytes().to_vec()), &mut h);
        b.render(0.0, &mut h);
        for want in [
            "flex|rgb(255, 0, 0)|8px|2px|3px|400",
            "after none",
            "media true false",
        ] {
            assert!(h.logs.iter().any(|l| l == want), "{want}: {:?}", h.logs);
        }
    }

    #[test]
    fn runtime_survives_a_script_that_runs_too_long() {
        let mut h = LogHost::default();
        let mut b = Browser::new(1000, 700, 1.0);
        b.render(0.0, &mut h);
        b.navigate("https://js.test/index.html", &mut h);
        let id = h.inner.fetched.last().unwrap().0;
        let page = r#"<script>
              // A chain of promise reactions that never ends: stopped, and
              // dropped so later calls do not resume it.
              setTimeout(() => { console.log('before'); (async () => { for (;;) await null; })(); }, 1);
              // A loop that keeps calling into the browser.
              setTimeout(() => { for (;;) document.body.firstChild; }, 2);
              setTimeout(() => console.log('still running', [1, 2].map(x => x * 2).join()), 5);
            </script>"#;
        b.fetch_done(id, Ok(page.as_bytes().to_vec()), &mut h);
        // A smaller per-call limit keeps the test quick.
        let t = &mut b.tabs[b.active];
        let si = t.runtimes[JS_MIME];
        t.scripts[si].as_mut().unwrap().set_call_fuel(300_000_000);
        b.render(10.0, &mut h);
        assert!(
            h.logs.iter().any(|l| l.contains("ran too long")),
            "{:?}",
            h.logs
        );
        assert!(
            h.logs.contains(&"still running 2,4".to_string()),
            "{:?}",
            h.logs
        );
        // What it logged before it was stopped is kept.
        assert!(h.logs.contains(&"before".to_string()), "{:?}", h.logs);
        assert_eq!(
            h.logs.iter().filter(|l| l.contains("ran too long")).count(),
            2,
            "{:?}",
            h.logs
        );
    }

    #[test]
    fn channels_clocks_and_dom_exceptions() {
        let mut h = LogHost::default();
        let mut b = Browser::new(1000, 700, 1.0);
        b.render(0.0, &mut h);
        b.navigate("https://www.chan.test/", &mut h);
        let id = h.inner.fetched.last().unwrap().0;
        let page = r#"<script>
              const log = (s) => console.log(s);
              // Time passes while a script runs.
              const t0 = performance.now(), d0 = Date.now();
              let spins = 0;
              while (performance.now() - t0 < 2) spins++;
              log('clock ' + (spins > 0) + ' ' + (Date.now() >= d0));
              const ch = new MessageChannel();
              ch.port2.onmessage = (e) => log('port ' + e.data.n);
              ch.port1.postMessage({ n: 1 });
              log('sync first');
              const a = new BroadcastChannel('x'), b2 = new BroadcastChannel('x');
              b2.addEventListener('message', (e) => log('broadcast ' + e.data));
              a.onmessage = () => log('sender got its own message');
              a.postMessage('hi');
              const ex = new DOMException('gone', 'NotFoundError');
              log('dom ' + ex.name + ' ' + ex.code + ' ' + ex.message + ' ' + (ex instanceof Error) + ' ' + DOMException.NOT_FOUND_ERR);
              log('domain ' + document.domain);
              const ac = new AbortController();
              ac.signal.addEventListener('abort', () => log('aborted ' + ac.signal.reason.name));
              ac.abort();
              AbortSignal.timeout(5).onabort = function () { log('timeout ' + this.reason.name); };
            </script>"#;
        b.fetch_done(id, Ok(page.as_bytes().to_vec()), &mut h);
        b.render(0.0, &mut h);
        b.render(20.0, &mut h);
        let at = |s: &str| h.logs.iter().position(|l| l == s);
        for want in [
            "clock true true",
            "sync first",
            "port 1",
            "broadcast hi",
            "dom NotFoundError 8 gone true 8",
            "domain www.chan.test",
            "aborted AbortError",
            "timeout TimeoutError",
        ] {
            assert!(at(want).is_some(), "{want}: {:?}", h.logs);
        }
        assert!(at("sync first") < at("port 1"), "messages arrive later");
        assert!(at("sender got its own message").is_none());
    }

    #[test]
    fn cssom_rules_change_the_page() {
        let mut h = LogHost::default();
        let mut b = Browser::new(1000, 700, 1.0);
        b.render(0.0, &mut h);
        b.navigate("https://www.cssom.test/", &mut h);
        let id = h.inner.fetched.last().unwrap().0;
        let page = r#"<style id=s>/* c */ p { margin: 0 } @media print { p { color: blue } }</style>
            <p id=p>x</p><p id=q>y</p>
            <script>
              const log = (s) => console.log(s);
              const s = document.getElementById('s'), sheet = s.sheet;
              const color = (id) => getComputedStyle(document.getElementById(id)).color;
              log('rules ' + sheet.cssRules.length + ' ' + sheet.cssRules[0].cssText + ' ' + sheet.cssRules[1].type + ' ' + sheet.cssRules[1].media.mediaText);
              log('same ' + (s.sheet === sheet) + ' ' + (document.styleSheets[0] === sheet) + ' ' + (sheet.ownerNode === s));
              sheet.insertRule('#p { color: rgb(0, 128, 0) }', sheet.cssRules.length);
              log('inserted ' + color('p') + ' text ' + s.textContent.includes('#p'));
              // An empty @media block, then rules inserted into it.
              sheet.insertRule('@media{}', 0);
              sheet.insertRule('--x { --x: 1 2; }', 0);
              log('custom ' + sheet.cssRules[0].cssText);
              sheet.cssRules[1].insertRule('#q { color: rgb(255, 0, 0) }', 0);
              log('nested ' + color('q'));
              sheet.cssRules[4].style.color = 'rgb(0, 0, 255)';
              log('style ' + color('p') + ' ' + sheet.cssRules[4].style.getPropertyValue('color'));
              sheet.deleteRule(4);
              log('deleted ' + color('p'));
              try { sheet.insertRule('p {}', 99) } catch (e) { log('index ' + e.name) }
              try { sheet.insertRule('a {} b {}') } catch (e) { log('syntax ' + e.name) }
              s.textContent = '#p { color: rgb(1, 2, 3) }';
              log('replaced ' + color('p') + ' ' + color('q') + ' ' + sheet.cssRules.length);
            </script>"#;
        b.fetch_done(id, Ok(page.as_bytes().to_vec()), &mut h);
        b.render(0.0, &mut h);
        for want in [
            "rules 2 p { margin: 0; } 4 print",
            "same true true true",
            "inserted rgb(0, 128, 0) text false",
            "custom --x { --x: 1 2; }",
            "nested rgb(255, 0, 0)",
            "style rgb(0, 0, 255) rgb(0, 0, 255)",
            "deleted rgb(0, 0, 0)",
            "index IndexSizeError",
            "syntax SyntaxError",
            "replaced rgb(1, 2, 3) rgb(0, 0, 0) 1",
        ] {
            assert!(h.logs.iter().any(|l| l == want), "{want}: {:?}", h.logs);
        }
    }

    #[test]
    fn links_expose_the_parts_of_their_url() {
        let mut h = LogHost::default();
        let mut b = Browser::new(1000, 700, 1.0);
        b.render(0.0, &mut h);
        b.navigate("https://www.links.test/dir/page", &mut h);
        let id = h.inner.fetched.last().unwrap().0;
        let page = r#"<script>
              const a = document.createElement('a');
              a.setAttribute('href', '../x/y?q=1#top');
              console.log([a.protocol, a.host, a.hostname, a.port, a.pathname, a.search, a.hash, a.origin].join(' '));
              a.pathname = 'z'; a.hash = 'end'; a.port = '8080';
              console.log(a.getAttribute('href'));
              const none = document.createElement('a');
              console.log('none ' + none.protocol + ' [' + none.pathname + ']');
              const div = document.createElement('div');
              div.hash = 'own';
              console.log('div ' + (div.protocol === undefined) + ' ' + div.hash);
              try { null.x } catch (e) { console.error(e) }
            </script>"#;
        b.fetch_done(id, Ok(page.as_bytes().to_vec()), &mut h);
        b.render(0.0, &mut h);
        for want in [
            "https: www.links.test www.links.test  /x/y ?q=1 #top https://www.links.test",
            "https://www.links.test:8080/z?q=1#end",
            "none : []",
            "div true own",
        ] {
            assert!(h.logs.iter().any(|l| l == want), "{want}: {:?}", h.logs);
        }
        assert!(
            h.logs
                .iter()
                .any(|l| l.starts_with("error: TypeError") && l.contains("\n    at ")),
            "errors are logged with their stack: {:?}",
            h.logs
        );
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
                    repeat: false,
                },
                0.0,
                &mut h2,
            );
        }
        b.event(
            Event::KeyDown {
                key: key::ENTER,
                mods: Modifiers::default(),
                repeat: false,
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
                repeat: false,
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
                repeat: false,
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
                repeat: false,
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

    fn gif_data_url(colors: &[[u8; 4]], delay_ms: u32) -> String {
        use image::codecs::gif::{GifEncoder, Repeat};
        let mut bytes = Vec::new();
        {
            let mut enc = GifEncoder::new(&mut bytes);
            enc.set_repeat(Repeat::Infinite).unwrap();
            for &c in colors {
                let img = image::RgbaImage::from_pixel(4, 4, image::Rgba(c));
                let delay = image::Delay::from_numer_denom_ms(delay_ms, 1);
                enc.encode_frame(image::Frame::from_parts(img, 0, 0, delay))
                    .unwrap();
            }
        }
        format!("data:image/gif;base64,{}", base64(&bytes))
    }

    fn base64(bytes: &[u8]) -> String {
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
        b64
    }

    #[test]
    fn animated_gifs_play() {
        let (mut b, mut h) = browser();
        b.navigate("https://gif.test/", &mut h);
        let id = h.fetched.last().unwrap().0;
        let gif = gif_data_url(&[[255, 0, 0, 255], [0, 0, 255, 255]], 200);
        let page = format!(r#"<body style="margin:0"><img id=a src="{gif}" width=40></body>"#);
        b.fetch_done(id, Ok(page.into_bytes()), &mut h);
        b.render(0.0, &mut h);
        b.render(0.0, &mut h);
        let n = page_el(&b, "a");
        let r = b.tabs[b.active].doc.content_box(n).unwrap();
        let vp = b.viewport_rect();
        let (px, py) = ((vp.x + r.x + 5.0) as usize, (vp.y + r.y + 5.0) as usize);
        let at = |b: &mut Browser, h: &mut TestHost, t: f64| {
            let c = b.render(t, h);
            c.pixels()[py * c.width() + px]
        };
        assert_eq!(at(&mut b, &mut h, 10.0), 0xff0000);
        // The browser asks to wake up when the frame changes.
        assert!(b.animating());
        let next = b.next_wakeup(50.0).unwrap();
        assert!((next - 200.0).abs() < 20.0, "{next}");
        assert_eq!(at(&mut b, &mut h, 250.0), 0x0000ff);
        // It loops.
        assert_eq!(at(&mut b, &mut h, 450.0), 0xff0000);
    }

    #[test]
    fn replaced_sizes_keep_their_ratio() {
        let (mut b, mut h) = browser();
        b.navigate("https://img.test/", &mut h);
        let id = h.fetched.last().unwrap().0;
        let wide = png_data_url(400, 200, [0, 128, 0, 255]);
        // A `viewBox` but no width or height: only a 3:1 ratio.
        let svg = format!(
            "data:image/svg+xml;base64,{}",
            base64(br#"<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 300 100"><rect width="300" height="100" fill="red"/></svg>"#)
        );
        let page = format!(
            r#"<body style="margin:0">
            <div style="width:150px"><img id=a src="{svg}" style="display:block"></div>
            <div style="width:100px"><img id=b src="{wide}" style="max-width:100%"></div>
            <img id=c src="{wide}" style="max-height:50px"></body>"#
        );
        b.fetch_done(id, Ok(page.into_bytes()), &mut h);
        b.render(0.0, &mut h);
        b.render(0.0, &mut h);
        let doc = &b.tabs[b.active].doc;
        let size = |id: &str| {
            let r = doc.content_box(doc.dom.element_by_id(id).unwrap()).unwrap();
            (r.w, r.h)
        };
        assert_eq!(size("a"), (150.0, 50.0), "as wide as its container");
        assert_eq!(size("b"), (100.0, 50.0), "max-width keeps the ratio");
        assert_eq!(size("c"), (100.0, 50.0), "max-height keeps the ratio");
    }

    #[test]
    fn iframes_load_render_and_follow_links() {
        let (mut b, mut h) = browser();
        b.navigate("https://host.test/", &mut h);
        let id = h.fetched.last().unwrap().0;
        let page = r#"<body style="margin:0">
            <iframe id=f src="inner.html" style="width:300px;height:100px;border:0"></iframe>
            <iframe id=s srcdoc="<a id=top target=_top href=https://other.test/>top</a>"></iframe>"#;
        b.fetch_done(id, Ok(page.as_bytes().to_vec()), &mut h);
        b.render(0.0, &mut h);
        let find =
            |h: &TestHost, url: &str| h.fetched.iter().rev().find(|f| f.1 == url).map(|f| f.0);
        let inner = find(&h, "https://host.test/inner.html").expect("frame requested");
        let html = r#"<link rel=stylesheet href=s.css><body style="margin:0">
            <a href="next.html" style="display:block;height:100px">next</a><div style="height:500px"></div>"#;
        b.fetch_done(inner, Ok(html.as_bytes().to_vec()), &mut h);
        b.render(0.0, &mut h);
        assert!(
            find(&h, "https://host.test/s.css").is_some(),
            "frame stylesheet"
        );
        let f = page_el(&b, "f");
        let frame = b.tabs[b.active].doc.frame(f).unwrap();
        assert!(
            frame
                .dom
                .text_content(crate::dom::Dom::ROOT)
                .contains("next")
        );

        // The wheel over the frame scrolls the frame, not the page.
        let vp = b.viewport_rect();
        let r = b.tabs[b.active].doc.content_box(f).unwrap();
        let (x, y) = ((vp.x + r.x + 50.0) as i32, (vp.y + r.y + 50.0) as i32);
        b.event(Event::MouseMove { x, y }, 0.0, &mut h);
        b.event(Event::Wheel { dx: 0, dy: 40 }, 0.0, &mut h);
        assert_eq!(b.tabs[b.active].doc.frame(f).unwrap().scroll_y(), 40.0);
        assert_eq!(b.tabs[b.active].doc.scroll_y(), 0.0);
        b.event(Event::Wheel { dx: 0, dy: -40 }, 0.0, &mut h);

        // A link inside loads into the frame; the page stays.
        b.event(Event::MouseDown { x, y, button: 0 }, 0.0, &mut h);
        b.event(Event::MouseUp { x, y, button: 0 }, 0.0, &mut h);
        b.render(0.0, &mut h);
        assert!(find(&h, "https://host.test/next.html").is_some());
        assert_eq!(b.tabs[b.active].url(), "https://host.test/");

        // target=_top replaces the page.
        let s = page_el(&b, "s");
        let r = b.tabs[b.active].doc.content_box(s).unwrap();
        let (x, y) = ((vp.x + r.x + 12.0) as i32, (vp.y + r.y + 12.0) as i32);
        b.event(Event::MouseDown { x, y, button: 0 }, 0.0, &mut h);
        b.event(Event::MouseUp { x, y, button: 0 }, 0.0, &mut h);
        assert!(find(&h, "https://other.test/").is_some());
    }

    fn load(b: &mut Browser, h: &mut TestHost, url: &str, html: &str) {
        b.navigate(url, h);
        let id = h.fetched.last().unwrap().0;
        b.fetch_done(id, Ok(html.as_bytes().to_vec()), h);
        b.render(0.0, h);
    }

    /// Window point of character `ch` of the text run containing `word`.
    fn text_at(b: &Browser, word: &str, ch: usize) -> (i32, i32) {
        let vp = b.viewport_rect();
        let doc = &b.tabs[b.active].doc;
        let r = doc.text_runs().into_iter().find(|r| r.text.contains(word)).unwrap();
        let st = doc.style(r.key.0).unwrap();
        let ch = r.text[..r.text.find(word).unwrap()].chars().count() + ch;
        let end = r.text.char_indices().nth(ch).map_or(r.text.len(), |(i, _)| i);
        let dx = b.fonts.measure(st.font(), st.font_size, &r.text[..end]);
        ((vp.x + r.rect.x + dx) as i32, (vp.y + r.rect.y + r.rect.h / 2.0) as i32)
    }

    #[test]
    fn dragging_selects_page_text_and_copy_takes_it() {
        let (mut b, mut h) = browser();
        load(&mut b, &mut h, "https://sel.test/", "<style>body{margin:20px;font-size:20px}</style><p>Hello <a href=/x>world</a> here</p><p>Second line</p>");
        let (x0, y0) = text_at(&b, "Hello", 0);
        let (x1, y1) = text_at(&b, "Second", 6);
        b.event(Event::MouseDown { x: x0, y: y0, button: 0 }, 0.0, &mut h);
        b.event(Event::MouseMove { x: x0 + 30, y: y0 }, 0.0, &mut h);
        b.event(Event::MouseMove { x: x1, y: y1 }, 0.0, &mut h);
        b.event(Event::MouseUp { x: x1, y: y1, button: 0 }, 0.0, &mut h);
        b.render(0.0, &mut h);
        assert_eq!(b.tabs[b.active].doc.selected_text(), "Hello world here\nSecond");
        press(&mut b, &mut h, 'C' as i32, CMD);
        assert_eq!(h.clipboard.as_deref(), Some("Hello world here\nSecond"));
        // a drag that ends on a link does not follow it
        assert_eq!(b.tabs[b.active].url(), "https://sel.test/");
        // select all, then a click clears the selection
        press(&mut b, &mut h, 'A' as i32, CMD);
        assert_eq!(b.tabs[b.active].doc.selected_text(), "Hello world here\nSecond line");
        b.event(Event::MouseDown { x: x1, y: y1 + 200, button: 0 }, 1000.0, &mut h);
        b.event(Event::MouseUp { x: x1, y: y1 + 200, button: 0 }, 1000.0, &mut h);
        assert!(!b.tabs[b.active].doc.has_selection());
    }

    #[test]
    fn double_click_selects_a_word() {
        let (mut b, mut h) = browser();
        load(&mut b, &mut h, "https://sel.test/", "<p style='font-size:20px'>alpha beta 金門大學</p>");
        let (x, y) = text_at(&b, "beta", 2);
        for t in [0.0, 100.0] {
            b.event(Event::MouseDown { x, y, button: 0 }, t, &mut h);
            b.event(Event::MouseUp { x, y, button: 0 }, t, &mut h);
        }
        assert_eq!(b.tabs[b.active].doc.selected_text(), "beta");
    }

    #[test]
    fn context_menu_on_a_link_copies_its_address() {
        let (mut b, mut h) = browser();
        load(&mut b, &mut h, "https://menu.test/a/", "<p style='font-size:20px'><a href='../next.html'>next page</a></p><p id=q oncontextmenu='event.preventDefault()'>quiet</p>");
        let (x, y) = text_at(&b, "next", 1);
        b.event(Event::MouseDown { x, y, button: 2 }, 0.0, &mut h);
        b.render(0.0, &mut h);
        let menu = b.chrome_el("menu").unwrap();
        assert_eq!(b.chrome.dom.attr(menu, "class"), Some("show"));
        let text = b.chrome.dom.text_content(menu);
        assert!(text.contains("複製連結網址") && text.contains("重新載入"), "{text}");
        // click the item
        let item = b.chrome.dom.descendants(menu).into_iter().find(|&n| b.chrome.dom.attr(n, "data-menu") == Some("ctx-copy-link")).unwrap();
        let r = b.chrome.content_box(item).unwrap();
        click_at(&mut b, &mut h, r);
        assert_eq!(h.clipboard.as_deref(), Some("https://menu.test/next.html"));
        assert_eq!(b.chrome.dom.attr(menu, "class"), Some(""));
        assert_eq!(b.tabs[b.active].url(), "https://menu.test/a/", "clicking the menu does not click the page");
        // a page that cancels contextmenu gets no menu
        let (x, y) = text_at(&b, "quiet", 1);
        b.event(Event::MouseDown { x, y, button: 2 }, 0.0, &mut h);
        assert!(b.menu.is_none());
        // Escape closes an open menu
        let (x, y) = text_at(&b, "next", 1);
        b.event(Event::MouseDown { x, y, button: 2 }, 0.0, &mut h);
        assert!(b.menu.is_some());
        press(&mut b, &mut h, key::ESCAPE, Modifiers::default());
        assert!(b.menu.is_none());
    }

    #[test]
    fn hover_follows_a_new_page_under_a_still_pointer() {
        let (mut b, mut h) = browser();
        let status = |b: &Browser| {
            let s = b.chrome_el("status").unwrap();
            b.chrome.dom.text_content(s)
        };
        b.navigate("https://one.test/", &mut h);
        let id = h.fetched.last().unwrap().0;
        let page = "<a id=a href='/next' style='display:block;height:300px'>link</a>";
        b.fetch_done(id, Ok(page.as_bytes().to_vec()), &mut h);
        b.render(0.0, &mut h);
        let vp = b.viewport_rect();
        let (x, y) = (300, (vp.y + 100.0) as i32);
        b.event(Event::MouseMove { x, y }, 0.0, &mut h);
        assert_eq!(status(&b), "https://one.test/next");

        // The next page has no link there: the bubble goes away without
        // the pointer moving.
        b.navigate("https://two.test/", &mut h);
        let id = h.fetched.last().unwrap().0;
        b.fetch_done(id, Ok(b"<p>plain</p>".to_vec()), &mut h);
        b.render(0.0, &mut h);
        assert_eq!(status(&b), "");

        // One with a link under the pointer shows it.
        b.navigate("https://three.test/", &mut h);
        let id = h.fetched.last().unwrap().0;
        b.fetch_done(id, Ok(page.as_bytes().to_vec()), &mut h);
        b.render(0.0, &mut h);
        assert_eq!(status(&b), "https://three.test/next");
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

    // ----- files: <input type=file>, Blob/File/FileReader, multipart, downloads

    /// A host with a file dialog that returns `picks`, a download folder
    /// in memory, and a record of requests.
    #[derive(Default)]
    struct FileHost {
        /// (id, url, method, content type, body).
        requests: Vec<(u32, String, String, String, Vec<u8>)>,
        /// What the next file dialog returns.
        picks: Option<Vec<crate::document::FileData>>,
        /// (multiple, accept) of each dialog shown.
        dialogs: Vec<(bool, String)>,
        saved: Vec<(String, Vec<u8>)>,
        logs: Vec<String>,
    }

    impl Host for FileHost {
        fn fetch(&mut self, id: u32, url: &str) {
            self.requests
                .push((id, url.into(), "GET".into(), String::new(), Vec::new()));
        }
        fn fetch_with(&mut self, id: u32, url: &str, r: &crate::input::HttpRequest) {
            let method = if r.method.is_empty() { "GET" } else { &r.method };
            self.requests.push((
                id,
                url.into(),
                method.into(),
                r.content_type.clone(),
                r.body.clone(),
            ));
        }
        fn clipboard_get(&mut self) -> Option<String> {
            None
        }
        fn clipboard_set(&mut self, _: &str) {}
        fn set_cursor(&mut self, _: Cursor) {}
        fn set_title(&mut self, _: &str) {}
        fn log(&mut self, m: &str) {
            self.logs.push(m.to_string());
        }
        fn pick_files(
            &mut self,
            multiple: bool,
            accept: &str,
        ) -> Option<Vec<crate::document::FileData>> {
            self.dialogs.push((multiple, accept.into()));
            self.picks.clone()
        }
        fn save_download(&mut self, name: &str, bytes: &[u8]) -> Result<String, String> {
            self.saved.push((name.into(), bytes.to_vec()));
            Ok(format!("/downloads/{name}"))
        }
    }

    /// A browser showing `html` at `url` (scripts run).
    fn file_page(url: &str, html: &str) -> (Browser, FileHost) {
        let mut h = FileHost::default();
        let mut b = Browser::new(1000, 700, 1.0);
        b.render(0.0, &mut h);
        b.navigate(url, &mut h);
        let id = h.requests.last().unwrap().0;
        b.fetch_done(id, Ok(html.as_bytes().to_vec()), &mut h);
        b.render(0.0, &mut h);
        (b, h)
    }

    fn click_file_host(b: &mut Browser, h: &mut FileHost, id: &str) {
        let n = page_el(b, id);
        let r = b.tabs[b.active].doc.content_box(n).unwrap();
        let vp = b.viewport_rect();
        let (x, y) = ((vp.x + r.x + 4.0) as i32, (vp.y + r.y + r.h / 2.0) as i32);
        b.event(Event::MouseDown { x, y, button: 0 }, 0.0, h);
        b.event(Event::MouseUp { x, y, button: 0 }, 0.0, h);
        b.render(0.0, h);
    }

    /// Splits a multipart body into (headers, content) parts.
    fn multipart_parts(ctype: &str, body: &[u8]) -> Vec<(String, Vec<u8>)> {
        let b = ctype.split("boundary=").nth(1).unwrap();
        let delim = format!("--{b}");
        let text = body.to_vec();
        let mut parts = Vec::new();
        let mut rest = &text[..];
        assert!(rest.starts_with(delim.as_bytes()), "starts with the boundary");
        loop {
            rest = &rest[delim.len()..];
            if rest.starts_with(b"--") {
                break;
            }
            rest = &rest[2..]; // CRLF
            let end = rest
                .windows(delim.len() + 2)
                .position(|w| w == [b"\r\n", delim.as_bytes()].concat())
                .unwrap();
            let part = &rest[..end];
            let split = part.windows(4).position(|w| w == b"\r\n\r\n").unwrap();
            parts.push((
                String::from_utf8_lossy(&part[..split]).into_owned(),
                part[split + 4..].to_vec(),
            ));
            rest = &rest[end + 2..];
        }
        parts
    }

    #[test]
    fn file_input_chooses_files_and_scripts_read_them() {
        let page = r#"<input type=file id=f name=up multiple accept=".txt,image/*">
            <script>
              const f = document.getElementById('f');
              console.log('before', f.files.length, JSON.stringify(f.value));
              f.addEventListener('input', () => console.log('input event'));
              f.addEventListener('change', async () => {
                const files = f.files;
                console.log('same list', files === f.files, files instanceof FileList);
                console.log('files', files.length, files[0].name, files[0].type, files[0].size,
                  files[1].name, files[1].lastModified, f.value);
                console.log('text', await files[0].text());
                const r = new FileReader();
                r.onload = () => console.log('dataurl', r.result);
                r.readAsDataURL(files[1]);
                const buf = await files[1].slice(1, 3).arrayBuffer();
                console.log('slice', [...new Uint8Array(buf)].join(','));
              });
            </script>"#;
        let (mut b, mut h) = file_page("https://files.test/", page);
        h.picks = Some(vec![
            crate::document::FileData::new("notes.txt", "héllo".as_bytes().to_vec(), 1000.0),
            crate::document::FileData::new("dot.png", vec![0x89, 0x50, 0x4e, 0x47], 2000.0),
        ]);
        click_file_host(&mut b, &mut h, "f");
        b.render(10.0, &mut h);
        assert_eq!(h.dialogs, vec![(true, ".txt,image/*".to_string())]);
        let logs = h.logs.join("\n");
        assert!(logs.contains("before 0 \"\""), "{logs}");
        assert!(logs.contains("input event"), "{logs}");
        assert!(logs.contains("same list true true"), "{logs}");
        assert!(
            logs.contains("files 2 notes.txt text/plain 6 dot.png 2000 C:\\fakepath\\notes.txt"),
            "{logs}"
        );
        assert!(logs.contains("text héllo"), "{logs}");
        assert!(logs.contains("dataurl data:image/png;base64,iVBORw=="), "{logs}");
        assert!(logs.contains("slice 80,78"), "{logs}");
        // The input shows how many files were chosen.
        let n = page_el(&b, "f");
        assert_eq!(b.tabs[b.active].doc.forms.files(n).len(), 2);
    }

    #[test]
    fn file_dialog_cancelled_or_missing_leaves_the_input_empty() {
        let page = r#"<input type=file id=f><button id=open>Open</button>
            <script>
              const f = document.getElementById('f');
              f.addEventListener('change', () => console.log('change'));
              f.addEventListener('cancel', () => console.log('cancel'));
              document.getElementById('open').onclick = () => f.click();
            </script>"#;
        // No window (screenshots): nothing happens.
        let (mut b, mut h) = file_page("https://files.test/", page);
        click_file_host(&mut b, &mut h, "f");
        assert_eq!(h.dialogs.len(), 1);
        assert!(!h.logs.iter().any(|l| l == "change" || l == "cancel"));
        // The user cancels: `cancel`.
        h.picks = Some(Vec::new());
        click_file_host(&mut b, &mut h, "f");
        assert!(h.logs.iter().any(|l| l == "cancel"), "{:?}", h.logs);
        // A script's click() opens the dialog too (a styled button).
        h.picks = Some(vec![crate::document::FileData::new(
            "a.csv",
            b"x".to_vec(),
            0.0,
        )]);
        click_file_host(&mut b, &mut h, "open");
        assert_eq!(h.dialogs.len(), 3);
        assert!(h.logs.iter().any(|l| l == "change"), "{:?}", h.logs);
        // Not `multiple`: only one file is kept.
        assert_eq!(h.dialogs[2], (false, String::new()));
    }

    #[test]
    fn multipart_form_submission_sends_files() {
        let page = r#"<form id=form method=post action="/upload" enctype="multipart/form-data">
              <input name=title value="My pic">
              <input type=file name=pic id=pic>
              <input type=file name=none>
              <input type=checkbox name=skip>
              <button id=send>Send</button>
              <button id=plain formenctype="text/plain" formaction="/plain">Plain</button>
            </form>"#;
        let (mut b, mut h) = file_page("https://files.test/form", page);
        h.picks = Some(vec![crate::document::FileData::new(
            "cat \"1\".png",
            vec![1, 2, 13, 10, 3],
            0.0,
        )]);
        click_file_host(&mut b, &mut h, "pic");
        click_file_host(&mut b, &mut h, "send");
        let (_, url, method, ctype, body) = h.requests.last().unwrap().clone();
        assert_eq!((url.as_str(), method.as_str()), ("https://files.test/upload", "POST"));
        assert!(ctype.starts_with("multipart/form-data; boundary="), "{ctype}");
        let parts = multipart_parts(&ctype, &body);
        assert_eq!(parts.len(), 3, "{parts:?}");
        assert_eq!(parts[0].0, "Content-Disposition: form-data; name=\"title\"");
        assert_eq!(parts[0].1, b"My pic");
        assert_eq!(
            parts[1].0,
            "Content-Disposition: form-data; name=\"pic\"; filename=\"cat %221%22.png\"\r\nContent-Type: image/png"
        );
        assert_eq!(parts[1].1, vec![1, 2, 13, 10, 3]);
        assert_eq!(
            parts[2].0,
            "Content-Disposition: form-data; name=\"none\"; filename=\"\"\r\nContent-Type: application/octet-stream"
        );
        assert!(parts[2].1.is_empty());

        // The submitter's formenctype / formaction win.
        let (mut b, mut h) = file_page("https://files.test/form", page);
        click_file_host(&mut b, &mut h, "plain");
        let (_, url, _, ctype, body) = h.requests.last().unwrap().clone();
        assert_eq!(url, "https://files.test/plain");
        assert_eq!(ctype, "text/plain");
        assert_eq!(String::from_utf8(body).unwrap(), "title=My pic\r\npic=\r\nnone=\r\n");
    }

    #[test]
    fn fetch_and_xhr_send_form_data_as_multipart() {
        let page = r#"<form id=form><input name=a value=1><input type=file name=f></form><script>
              const fd = new FormData(document.getElementById('form'));
              fd.append('blob', new Blob(['ab', new Uint8Array([0, 255])], { type: 'application/x-test' }), 'b.bin');
              fd.append('text', 'x\ny');
              console.log('entries', [...fd.keys()].join(','), fd.get('f') instanceof File, fd.get('f').name === '');
              fetch('/api', { method: 'POST', body: fd }).then(async (r) => {
                console.log('status', r.status, r.ok, r.headers.get('content-type'));
                const bytes = new Uint8Array(await r.arrayBuffer());
                console.log('bytes', bytes.join(','));
              });
              const x = new XMLHttpRequest();
              x.open('POST', '/xhr');
              x.responseType = 'blob';
              x.upload.onload = (e) => console.log('upload', e.loaded);
              x.onload = async () => console.log('xhr', x.status, x.response.type, await x.response.text(), x.getResponseHeader('X-Test'));
              x.send(new Blob(['hello'], { type: 'text/plain' }));
            </script>"#;
        let (mut b, mut h) = file_page("https://files.test/", page);
        let logs = h.logs.join("\n");
        assert!(logs.contains("entries a,f,blob,text true true"), "{logs}");
        let api = h.requests.iter().find(|r| r.1.ends_with("/api")).unwrap().clone();
        assert_eq!(api.2, "POST");
        let parts = multipart_parts(&api.3, &api.4);
        assert_eq!(parts.len(), 4, "{parts:?}");
        assert_eq!(parts[0].1, b"1");
        assert!(parts[1].0.contains("filename=\"\""), "{:?}", parts[1]);
        assert_eq!(
            parts[2].0,
            "Content-Disposition: form-data; name=\"blob\"; filename=\"b.bin\"\r\nContent-Type: application/x-test"
        );
        assert_eq!(parts[2].1, vec![b'a', b'b', 0, 255]);
        assert_eq!(parts[3].1, b"x\r\ny");
        let xhr = h.requests.iter().find(|r| r.1.ends_with("/xhr")).unwrap().clone();
        assert_eq!((xhr.3.as_str(), &xhr.4[..]), ("text/plain", &b"hello"[..]));

        let headers = |ct: &str| ResponseInfo {
            status: 201,
            headers: vec![
                ("content-type".into(), ct.into()),
                ("x-test".into(), "yes".into()),
            ],
        };
        b.fetch_response(api.0, headers("application/octet-stream"));
        b.fetch_done(api.0, Ok(vec![0, 1, 254]), &mut h);
        b.fetch_response(xhr.0, headers("text/plain"));
        b.fetch_done(xhr.0, Ok(b"ok".to_vec()), &mut h);
        b.render(10.0, &mut h);
        let logs = h.logs.join("\n");
        assert!(logs.contains("status 201 true application/octet-stream"), "{logs}");
        assert!(logs.contains("bytes 0,1,254"), "{logs}");
        assert!(logs.contains("upload 5"), "{logs}");
        assert!(logs.contains("xhr 201 text/plain ok yes"), "{logs}");
    }

    #[test]
    fn blob_urls_serve_images_fetch_and_downloads() {
        let png = png_data_url(2, 2, [255, 0, 0, 255]);
        let page = format!(
            r#"<img id=img><a id=save download="report.csv" style="display: block">Save</a><script>
              const png = atob('{}'.split(',')[1]);
              const bytes = Uint8Array.from(png, (c) => c.charCodeAt(0));
              const url = URL.createObjectURL(new Blob([bytes], {{ type: 'image/png' }}));
              console.log('url', url.startsWith('blob:https://files.test/'));
              document.getElementById('img').src = url;
              const csv = URL.createObjectURL(new File(['a,b\n1,2\n'], 'x.csv', {{ type: 'text/csv' }}));
              document.getElementById('save').href = csv;
              fetch(csv).then((r) => r.text()).then((t) => console.log('fetched', JSON.stringify(t), 'gone'));
              const gone = URL.createObjectURL(new Blob(['x']));
              URL.revokeObjectURL(gone);
              fetch(gone).catch((e) => console.log('revoked', e.name));
              // A script-made link, as apps export files.
              setTimeout(() => {{
                const a = document.createElement('a');
                a.href = URL.createObjectURL(new Blob(['{{"k":1}}'], {{ type: 'application/json' }}));
                a.download = 'data.json';
                a.click();
              }}, 5);
            </script>"#,
            png
        );
        let (mut b, mut h) = file_page("https://files.test/", &page);
        b.render(1.0, &mut h);
        b.render(20.0, &mut h);
        let logs = h.logs.join("\n");
        assert!(logs.contains("url true"), "{logs}");
        assert!(logs.contains("fetched \"a,b\\n1,2\\n\""), "{logs}");
        assert!(logs.contains("revoked TypeError"), "{logs}");
        // The image loaded from its blob: URL.
        let img = page_el(&b, "img");
        assert!(
            matches!(
                b.tabs[b.active].doc.images.get(img),
                Some(crate::document::ImageState::Ready(..))
            ),
            "image from a blob: URL"
        );
        assert_eq!(h.saved, vec![("data.json".to_string(), b"{\"k\":1}".to_vec())]);
        // Clicking a download link saves its blob under the page's name.
        click_file_host(&mut b, &mut h, "save");
        assert_eq!(h.saved[1], ("report.csv".to_string(), b"a,b\n1,2\n".to_vec()));
        assert_eq!(
            b.download_notice(),
            "Downloaded report.csv to /downloads/report.csv"
        );
        assert_eq!(b.tabs[b.active].url(), "https://files.test/");
    }

    #[test]
    fn attachments_are_saved_not_shown() {
        let page = r#"<style>a { display: block }</style>
            <p id=p>stay</p><a id=get href="/files/report.pdf">Get</a>
            <a id=named href="/export?id=1" download="ignored.txt">Export</a>"#;
        let (mut b, mut h) = file_page("https://files.test/", page);
        // A type the browser cannot show (no Content-Disposition).
        click_file_host(&mut b, &mut h, "get");
        let id = h.requests.last().unwrap().0;
        b.fetch_response(
            id,
            ResponseInfo {
                status: 200,
                headers: vec![("content-type".into(), "application/pdf".into())],
            },
        );
        b.fetch_done(id, Ok(b"%PDF".to_vec()), &mut h);
        b.render(0.0, &mut h);
        assert_eq!(h.saved.last().unwrap().0, "report.pdf");
        assert_eq!(b.tabs[b.active].url(), "https://files.test/");
        assert!(page_el(&b, "p") > 0, "the page stays");
        assert_eq!(b.tabs[b.active].history.len(), 2, "no history entry added");
        // `download` on a same-origin link: the server's name wins.
        click_file_host(&mut b, &mut h, "named");
        let id = h.requests.last().unwrap().0;
        b.fetch_response(
            id,
            ResponseInfo {
                status: 200,
                headers: vec![
                    ("content-type".into(), "text/csv".into()),
                    (
                        "content-disposition".into(),
                        "attachment; filename*=UTF-8''%E5%A0%B1%E8%A1%A8.csv".into(),
                    ),
                ],
            },
        );
        b.fetch_done(id, Ok(b"a,b".to_vec()), &mut h);
        assert_eq!(h.saved.last().unwrap(), &("報表.csv".to_string(), b"a,b".to_vec()));
        // A page served as an attachment is saved too.
        b.navigate("https://files.test/page.html", &mut h);
        let id = h.requests.last().unwrap().0;
        b.fetch_response(
            id,
            ResponseInfo {
                status: 200,
                headers: vec![
                    ("content-type".into(), "text/html".into()),
                    ("content-disposition".into(), "attachment".into()),
                ],
            },
        );
        b.fetch_done(id, Ok(b"<p>x".to_vec()), &mut h);
        assert_eq!(h.saved.last().unwrap().0, "page.html");
        assert_eq!(b.tabs[b.active].url(), "https://files.test/");
        assert!(b.download_notice().contains("/downloads/page.html"));
    }

    // ----- developer tools -------------------------------------------------

    /// The text of chrome element `id`.
    fn chrome_text(b: &Browser, id: &str) -> String {
        b.chrome.dom.text_content(b.chrome_el(id).unwrap())
    }

    /// Clicks the chrome element with `data-dt` = `cmd`.
    fn click_dt(b: &mut Browser, h: &mut TestHost, cmd: &str) {
        b.render(0.0, h);
        let n = b
            .chrome
            .dom
            .descendants(crate::dom::Dom::ROOT)
            .into_iter()
            .find(|&n| b.chrome.dom.attr(n, "data-dt") == Some(cmd))
            .unwrap_or_else(|| panic!("no control {cmd}"));
        let r = b.chrome.box_rects(n).unwrap().0;
        click_at(b, h, r);
        b.render(0.0, h);
    }

    #[test]
    fn devtools_open_with_f12_and_dock_below_the_page() {
        let (mut b, mut h) = browser();
        load(&mut b, &mut h, "https://dt.test/", "<p>x</p>");
        let full = b.viewport_rect();
        press(&mut b, &mut h, key::F12, Modifiers::default());
        b.render(0.0, &mut h);
        assert!(b.devtools_open());
        let vp = b.viewport_rect();
        let panel = chrome_rect(&b, "devtools");
        assert!(vp.h < full.h - 250.0, "the page gets shorter: {vp:?}");
        assert!(panel.y >= vp.y + vp.h - 1.0, "the panel is below the page");
        // Cmd+Opt+I closes it again; Ctrl+Shift+I opens it.
        let mods = Modifiers { meta: true, alt: true, ..Default::default() };
        press(&mut b, &mut h, 'I' as i32, mods);
        b.render(0.0, &mut h);
        assert!(!b.devtools_open());
        assert_eq!(b.viewport_rect().h, full.h);
        let mods = Modifiers { ctrl: true, shift: true, ..Default::default() };
        press(&mut b, &mut h, 'I' as i32, mods);
        assert!(b.devtools_open());
        // Dragging the top edge resizes it.
        b.render(0.0, &mut h);
        let edge = chrome_rect(&b, "dt-resize");
        let (x, y) = ((edge.x + 50.0) as i32, (edge.y + 2.0) as i32);
        b.event(Event::MouseDown { x, y, button: 0 }, 0.0, &mut h);
        b.event(Event::MouseMove { x, y: y - 100 }, 0.0, &mut h);
        b.event(Event::MouseUp { x, y: y - 100, button: 0 }, 0.0, &mut h);
        b.render(0.0, &mut h);
        assert_eq!(b.devtools.height, 400.0);
        click_dt(&mut b, &mut h, "close");
        assert!(!b.devtools_open());
    }

    #[test]
    fn devtools_console_shows_logs_and_runs_code() {
        let (mut b, mut h) = browser();
        load(
            &mut b,
            &mut h,
            "https://dt.test/",
            "<p>x</p><script>console.log('hi', 1); console.log('hi', 1); console.warn('careful'); \
             console.error('bad'); nope();</script>",
        );
        b.open_devtools("console");
        b.render(0.0, &mut h);
        let log = chrome_text(&b, "dt-log");
        assert!(log.contains("2hi 1"), "repeated messages are counted: {log}");
        assert!(log.contains("careful") && log.contains("bad"), "{log}");
        assert!(log.contains("Uncaught ReferenceError"), "{log}");
        let classes: Vec<String> = b
            .chrome
            .dom
            .children(b.chrome_el("dt-log").unwrap())
            .iter()
            .map(|&n| b.chrome.dom.attr(n, "class").unwrap_or("").to_string())
            .collect();
        assert!(classes.contains(&"dt-msg warn".to_string()), "{classes:?}");
        assert!(classes.contains(&"dt-msg error".to_string()), "{classes:?}");
        // Typing code and Enter runs it in the page.
        let r = chrome_rect(&b, "dt-input");
        click_at(&mut b, &mut h, r);
        type_text(&mut b, &mut h, "document.querySelector('p').textContent + 1");
        press(&mut b, &mut h, key::ENTER, Modifiers::default());
        b.render(0.0, &mut h);
        let log = chrome_text(&b, "dt-log");
        assert!(log.contains("› document.querySelector('p').textContent + 1"), "{log}");
        assert!(log.contains("← \"x1\""), "{log}");
        assert_eq!(b.chrome.input_value(b.chrome_el("dt-input").unwrap()), "");
        // The page's state changes and errors are shown.
        b.devtools_eval("document.title = 'changed'; undefinedThing", &mut h);
        b.render(0.0, &mut h);
        assert_eq!(b.tabs[b.active].doc.title().as_deref(), Some("changed"));
        assert!(chrome_text(&b, "dt-log").contains("Uncaught ReferenceError: undefinedThing"));
        // Up recalls earlier input.
        press(&mut b, &mut h, key::UP, Modifiers::default());
        press(&mut b, &mut h, key::UP, Modifiers::default());
        assert_eq!(
            b.chrome.input_value(b.chrome_el("dt-input").unwrap()),
            "document.querySelector('p').textContent + 1"
        );
        // Filters, then clearing.
        click_dt(&mut b, &mut h, "filter:warn");
        assert_eq!(chrome_text(&b, "dt-log"), "careful");
        click_dt(&mut b, &mut h, "filter:all");
        click_dt(&mut b, &mut h, "clear");
        assert_eq!(chrome_text(&b, "dt-log"), "");
        // A new page starts a new log.
        b.devtools_eval("1", &mut h);
        load(&mut b, &mut h, "https://dt.test/next", "<script>console.log('next page')</script>");
        b.render(0.0, &mut h);
        assert_eq!(chrome_text(&b, "dt-log"), "next page");
    }

    #[test]
    fn devtools_elements_tree_styles_and_edits() {
        let (mut b, mut h) = browser();
        load(
            &mut b,
            &mut h,
            "https://dt.test/",
            "<style>.card { padding: 10px; color: rgb(1, 2, 3) } #x .card { margin: 4px }</style>\
             <div id=x><div class=card style='border: 2px solid red'>Card text</div><ul><li>a</li></ul></div>",
        );
        assert!(b.inspect_selector(".card"));
        b.render(0.0, &mut h);
        let card = page_el(&b, "x");
        let card = b.tabs[b.active].doc.dom.children(card)[0];
        assert_eq!(b.tabs[b.active].dev.selected, Some(card));
        let tree = chrome_text(&b, "dt-tree");
        assert!(tree.contains("<div class=\"card\" style=\"border: 2px solid red\">Card text</div>"), "{tree}");
        assert!(tree.contains("<ul>…</ul>"), "collapsed: {tree}");
        let side = chrome_text(&b, "dt-side-content");
        assert!(side.contains("div.card"), "{side}");
        assert!(side.contains("element.style"), "{side}");
        let at = |s: &str| side.find(s).unwrap_or_else(|| panic!("{s} in {side}"));
        assert!(at("#x .card") < at(".card {"), "the more specific rule first: {side}");
        assert!(side.contains("color: rgb(1, 2, 3)"), "computed: {side}");
        assert!(side.contains("padding 10 10 10 10"), "box model: {side}");
        // Expanding a node shows its children.
        let ul = b.tabs[b.active].doc.dom.find_tag(crate::dom::Dom::ROOT, "ul").unwrap();
        click_dt(&mut b, &mut h, &format!("toggle:{ul}"));
        assert!(chrome_text(&b, "dt-tree").contains("<li>a</li>"));
        // Editing attributes and text.
        b.devtools_edit("title=\"hello\"");
        b.devtools_edit("-style");
        let doc = &b.tabs[b.active].doc;
        assert_eq!(doc.dom.attr(card, "title"), Some("hello"));
        assert_eq!(doc.dom.attr(card, "style"), None);
        let text = doc.dom.children(card)[0];
        // (a lone text child shares its element's row: select it directly)
        b.devtools_command(&format!("select:{text}"), &mut h);
        b.render(0.0, &mut h);
        let r = chrome_rect(&b, "dt-edit");
        click_at(&mut b, &mut h, r);
        type_text(&mut b, &mut h, "New text");
        press(&mut b, &mut h, key::ENTER, Modifiers::default());
        assert_eq!(b.tabs[b.active].doc.dom.text_content(card), "New text");
        // The page changed by script: the tree follows (after a moment).
        b.run_script("document.getElementById('x').append(document.createElement('section'))", &mut h);
        b.render(1000.0, &mut h);
        assert!(chrome_text(&b, "dt-tree").contains("<section></section>"));
    }

    #[test]
    fn devtools_inspect_mode_picks_a_page_element() {
        let (mut b, mut h) = browser();
        load(
            &mut b,
            &mut h,
            "https://dt.test/",
            "<p id=a style='height:40px'>first</p><p id=b style='height:40px' onclick=\"document.title='clicked'\">second</p>",
        );
        let mods = Modifiers { ctrl: true, shift: true, ..Default::default() };
        press(&mut b, &mut h, 'C' as i32, mods);
        b.render(0.0, &mut h);
        assert!(b.devtools.inspecting);
        let (x, y) = text_at(&b, "second", 1);
        b.event(Event::MouseMove { x, y }, 0.0, &mut h);
        assert_eq!(b.devtools.hover, Some(page_el(&b, "b")), "outlined under the pointer");
        // The outline is drawn over the page.
        let before = {
            b.devtools.inspecting = false;
            b.devtools.hover = None;
            b.render(0.0, &mut h).pixels().to_vec()
        };
        b.devtools.inspecting = true;
        b.event(Event::MouseMove { x, y }, 0.0, &mut h);
        let after = b.render(0.0, &mut h).pixels().to_vec();
        assert_ne!(before, after, "the overlay changes the page's pixels");
        b.event(Event::MouseDown { x, y, button: 0 }, 0.0, &mut h);
        b.event(Event::MouseUp { x, y, button: 0 }, 0.0, &mut h);
        b.render(0.0, &mut h);
        assert!(!b.devtools.inspecting);
        assert_eq!(b.tabs[b.active].dev.selected, Some(page_el(&b, "b")));
        assert_ne!(b.tabs[b.active].doc.title().as_deref(), Some("clicked"), "the page did not get the click");
        assert_eq!(b.devtools.panel, devtools::Panel::Elements);
        // The context menu's "Inspect" selects the element under it.
        let (x, y) = text_at(&b, "first", 1);
        b.event(Event::MouseDown { x, y, button: 2 }, 0.0, &mut h);
        b.render(0.0, &mut h);
        let menu = b.chrome_el("menu").unwrap();
        let item = b
            .chrome
            .dom
            .descendants(menu)
            .into_iter()
            .find(|&n| b.chrome.dom.attr(n, "data-menu") == Some("ctx-inspect"))
            .unwrap();
        let r = b.chrome.content_box(item).unwrap();
        click_at(&mut b, &mut h, r);
        assert_eq!(b.tabs[b.active].dev.selected, Some(page_el(&b, "a")));
    }

    #[test]
    fn devtools_network_lists_requests() {
        let (mut b, mut h) = browser();
        load(
            &mut b,
            &mut h,
            "https://dt.test/app/",
            "<link rel=stylesheet href=s.css><script>\
             fetch('/api', { method: 'POST', body: JSON.stringify({ q: 1 }), headers: { 'content-type': 'application/json' } });\
             fetch('https://other.test/x');</script>",
        );
        b.render(0.0, &mut h);
        let css = h.fetched.iter().find(|f| f.1.ends_with("s.css")).unwrap().0;
        b.fetch_done(css, Ok(b"p{}".to_vec()), &mut h);
        let api = h.fetched.iter().find(|f| f.1.ends_with("/api")).unwrap().0;
        b.fetch_response(
            api,
            ResponseInfo {
                status: 201,
                headers: vec![("content-type".into(), "application/json".into())],
            },
        );
        b.fetch_done(api, Ok(br#"{"ok":true}"#.to_vec()), &mut h);
        // The cross-origin request is sent; without CORS headers it fails.
        let other = h.fetched.iter().find(|f| f.1 == "https://other.test/x").unwrap().0;
        b.fetch_done(other, Err("HTTP 404".into()), &mut h);
        b.open_devtools("network");
        b.render(0.0, &mut h);
        let list = chrome_text(&b, "dt-requests");
        assert!(list.contains("app200document"), "{list}");
        assert!(list.contains("s.css200stylesheet3 B"), "{list}");
        assert!(list.contains("api201fetch11 B"), "{list}");
        assert!(list.contains("x(failed)fetch"), "cross-origin, blocked: {list}");
        click_dt(&mut b, &mut h, &format!("req:{api}"));
        let detail = chrome_text(&b, "dt-detail");
        assert!(detail.contains("URL: https://dt.test/api"), "{detail}");
        assert!(detail.contains("Method: POST"), "{detail}");
        assert!(detail.contains("content-type: application/json"), "{detail}");
        assert!(detail.contains("{\"q\":1}"), "the request body: {detail}");
        assert!(detail.contains("{\"ok\":true}"), "the response: {detail}");
        click_dt(&mut b, &mut h, "clear");
        assert_eq!(chrome_text(&b, "dt-requests"), "NameStatusTypeSizeTime");
    }

    #[test]
    fn devtools_sources_lists_scripts_and_styles() {
        let (mut b, mut h) = browser();
        load(
            &mut b,
            &mut h,
            "https://dt.test/",
            "<style>p { color: red }</style><script src=app.js></script><script>var inline = 1;</script>",
        );
        let js = h.fetched.iter().find(|f| f.1.ends_with("app.js")).unwrap().0;
        b.fetch_done(js, Ok(b"// app\nvar app = 2;\n".to_vec()), &mut h);
        b.open_devtools("sources");
        b.render(0.0, &mut h);
        let files = chrome_text(&b, "dt-files");
        assert_eq!(files, "HTML dt.testJS app.jsCSS (inline style 1)JS (inline script 1)", "{files}");
        assert!(chrome_text(&b, "dt-code").contains("<style>p { color: red }</style>"));
        click_dt(&mut b, &mut h, &format!("src:net:{js}"));
        let code = chrome_text(&b, "dt-code");
        assert!(code.starts_with("1\n2\n// app\nvar app = 2;"), "line numbers and text: {code:?}");
        let style = b.tabs[b.active].doc.dom.find_tag(crate::dom::Dom::ROOT, "style").unwrap();
        click_dt(&mut b, &mut h, &format!("src:node:{style}"));
        assert!(chrome_text(&b, "dt-code").contains("p { color: red }"));
    }

    #[test]
    fn devtools_application_shows_and_edits_storage() {
        let (mut b, mut h) = browser();
        load(
            &mut b,
            &mut h,
            "https://dt.test/",
            "<script>localStorage.setItem('theme', 'dark'); sessionStorage.setItem('tab', '3'); \
             document.cookie = 'sid=a b'; URL.createObjectURL(new Blob(['hi'], { type: 'text/plain' }));</script>",
        );
        b.open_devtools("application");
        b.render(0.0, &mut h);
        let store = chrome_text(&b, "dt-store");
        for want in ["Local storage✕themedark", "Session storage✕tab3", "Cookies✕sida b", "text/plain 2 B"] {
            assert!(store.contains(want), "{want} in {store}");
        }
        click_dt(&mut b, &mut h, "del:local:theme");
        let r = chrome_rect(&b, "dt-app-edit");
        click_at(&mut b, &mut h, r);
        type_text(&mut b, &mut h, "mode=新");
        press(&mut b, &mut h, key::ENTER, Modifiers::default());
        b.render(0.0, &mut h);
        let store = chrome_text(&b, "dt-store");
        assert!(store.contains("Local storage✕mode新Session"), "{store}");
        click_dt(&mut b, &mut h, "clear");
        let store = chrome_text(&b, "dt-store");
        assert!(store.contains("Local storage(empty)Session storage(empty)Cookies(empty)"), "{store}");
    }

    #[test]
    fn devtools_performance_counts_script_calls() {
        let (mut b, mut h) = browser();
        load(
            &mut b,
            &mut h,
            "https://dt.test/",
            "<button id=go onclick='let x = 0; for (let i = 0; i < 1000; i++) x += i'>go</button>\
             <script>setTimeout(() => {}, 5);</script>",
        );
        b.render(10.0, &mut h);
        click_page(&mut b, &mut h, "go");
        b.open_devtools("performance");
        b.render(20.0, &mut h);
        let perf = chrome_text(&b, "dt-perf");
        assert!(perf.contains("timer1"), "{perf}");
        assert!(perf.contains("event1"), "{perf}");
        assert!(perf.contains("event click"), "the slowest calls: {perf}");
        assert!(perf.contains("Frames (last"), "{perf}");
        let calls = &b.tabs[b.active].dev.calls;
        let click = calls.iter().find(|c| c.stats.label == "event click").unwrap();
        assert!(click.stats.fuel > 1000, "the loop's instructions are counted");
    }

    #[test]
    fn devtools_styles_turn_declarations_off_and_edit_them() {
        let (mut b, mut h) = browser();
        load(
            &mut b,
            &mut h,
            "https://dt.test/",
            "<style>p { color: rgb(255, 0, 0); margin: 0 }</style><p id=p style='font-size: 30px; padding: 2px'>x</p>",
        );
        // Tall enough for the whole Styles pane.
        b.resize(1000, 1600, 1.0);
        b.devtools.height = 1200.0;
        b.inspect_selector("#p");
        b.render(0.0, &mut h);
        let p = page_el(&b, "p");
        let color = |b: &mut Browser| b.tabs[b.active].doc.computed_value(p, "color").unwrap();
        assert_eq!(color(&mut b), "rgb(255, 0, 0)");
        let side = chrome_text(&b, "dt-side-content");
        assert!(side.contains("☑ color: rgb(255, 0, 0);"), "{side}");
        // Turning the rule's color off: the inherited color shows.
        let rule = b.tabs[b.active].doc.matched_rules(p).into_iter().find(|r| &*r.selector == "p").unwrap();
        click_dt(&mut b, &mut h, &format!("decl:{}:0", rule.decls_id));
        assert_eq!(color(&mut b), "rgb(0, 0, 0)");
        assert!(chrome_text(&b, "dt-side-content").contains("☐ color: rgb(255, 0, 0);"));
        click_dt(&mut b, &mut h, &format!("decl:{}:0", rule.decls_id));
        assert_eq!(color(&mut b), "rgb(255, 0, 0)");
        // element.style: off, back on, and a new property.
        click_dt(&mut b, &mut h, "inline:0");
        assert_eq!(b.tabs[b.active].doc.dom.attr(p, "style"), Some("padding: 2px"));
        assert!(chrome_text(&b, "dt-side-content").contains("☐ font-size: 30px;"));
        click_dt(&mut b, &mut h, "inline:1");
        assert_eq!(b.tabs[b.active].doc.dom.attr(p, "style"), Some("padding: 2px; font-size: 30px"));
        b.devtools_edit("background-color: blue;");
        assert_eq!(
            b.tabs[b.active].doc.dom.attr(p, "style"),
            Some("padding: 2px; font-size: 30px; background-color: blue")
        );
    }

    // ----- remote debugging (Chrome DevTools Protocol) ---------------------

    /// Sends `msg` on connection `conn`; returns everything sent back.
    fn cdp(b: &mut Browser, h: &mut TestHost, conn: u32, msg: &str) -> Vec<serde_json::Value> {
        b.cdp_message(conn, msg, h);
        b.render(0.0, h);
        b.take_cdp_output()
            .into_iter()
            .map(|(_, t)| serde_json::from_str(&t).unwrap())
            .collect()
    }

    fn reply(out: &[serde_json::Value], id: i64) -> serde_json::Value {
        out.iter().find(|m| m["id"] == id).cloned().unwrap_or_else(|| panic!("no reply {id} in {out:?}"))
    }

    #[test]
    fn cdp_targets_and_discovery() {
        let (mut b, mut h) = browser();
        load(&mut b, &mut h, "https://cdp.test/", "<title>T</title><p>x</p>");
        let (status, list) = b.cdp_http("/json/list", 9222, &mut h);
        assert_eq!(status, 200);
        let list: serde_json::Value = serde_json::from_str(&list).unwrap();
        assert_eq!(list[0]["url"], "https://cdp.test/");
        assert_eq!(list[0]["title"], "T");
        let ws = list[0]["webSocketDebuggerUrl"].as_str().unwrap().to_string();
        assert!(ws.starts_with("ws://127.0.0.1:9222/devtools/page/"), "{ws}");
        assert_eq!(b.cdp_http("/json/nothing", 9222, &mut h).0, 404);
        let path = ws.trim_start_matches("ws://127.0.0.1:9222");
        assert!(b.cdp_open(1, path));
        assert!(!b.cdp_open(2, "/devtools/page/FFFF"));
        assert!(b.cdp_open(3, "/devtools/browser/rubrowser"));
        let out = cdp(&mut b, &mut h, 3, r#"{"id":1,"method":"Target.getTargets"}"#);
        assert_eq!(reply(&out, 1)["result"]["targetInfos"][0]["attached"], true);
        let out = cdp(&mut b, &mut h, 1, r#"{"id":2,"method":"Nope.nothing"}"#);
        assert_eq!(reply(&out, 2)["error"]["code"], -32601);
        let out = cdp(&mut b, &mut h, 1, "not json");
        assert_eq!(out[0]["error"]["code"], -32700);
        let out = cdp(&mut b, &mut h, 3, r#"{"id":4,"method":"Browser.close"}"#);
        assert!(reply(&out, 4)["result"].is_object());
        assert!(b.cdp_wants_close());
    }

    #[test]
    fn cdp_page_runtime_dom_and_events() {
        let (mut b, mut h) = browser();
        load(&mut b, &mut h, "https://cdp.test/", "<p id=a class=x>hi</p>");
        let tab = b.tabs[b.active].id;
        assert!(b.cdp_open(1, &format!("/devtools/page/{tab:032X}")));
        for (k, d) in ["Page", "Runtime", "Network", "DOM"].iter().enumerate() {
            let out = cdp(&mut b, &mut h, 1, &format!(r#"{{"id":{k},"method":"{d}.enable"}}"#));
            assert!(reply(&out, k as i64)["result"].is_object());
        }
        // Runtime.evaluate answers once the page's runtime has run it.
        let out = cdp(&mut b, &mut h, 1, r#"{"id":10,"method":"Runtime.evaluate","params":{"expression":"document.getElementById('a').textContent + 1"}}"#);
        assert_eq!(reply(&out, 10)["result"]["result"]["value"], "hi1");
        let out = cdp(&mut b, &mut h, 1, r#"{"id":11,"method":"Runtime.evaluate","params":{"expression":"({a: [1, 2]})","returnByValue":true}}"#);
        assert_eq!(reply(&out, 11)["result"]["result"]["value"]["a"][1], 2);
        let out = cdp(&mut b, &mut h, 1, r#"{"id":12,"method":"Runtime.evaluate","params":{"expression":"console.warn('w'); throw new TypeError('t')"}}"#);
        assert!(out.iter().any(|m| m["method"] == "Runtime.consoleAPICalled" && m["params"]["type"] == "warning"));
        assert_eq!(reply(&out, 12)["result"]["exceptionDetails"]["exception"]["subtype"], "error");
        // DOM.
        let out = cdp(&mut b, &mut h, 1, r#"{"id":20,"method":"DOM.getDocument","params":{"depth":-1}}"#);
        let root = reply(&out, 20)["result"]["root"]["nodeId"].as_u64().unwrap();
        let out = cdp(&mut b, &mut h, 1, &format!(r#"{{"id":21,"method":"DOM.querySelector","params":{{"nodeId":{root},"selector":".x"}}}}"#));
        let p = reply(&out, 21)["result"]["nodeId"].as_u64().unwrap();
        assert_eq!(p as usize, page_el(&b, "a") + 1);
        let out = cdp(&mut b, &mut h, 1, &format!(r#"{{"id":22,"method":"DOM.getOuterHTML","params":{{"nodeId":{p}}}}}"#));
        assert_eq!(reply(&out, 22)["result"]["outerHTML"], "<p id=\"a\" class=\"x\">hi</p>");
        let out = cdp(&mut b, &mut h, 1, &format!(r#"{{"id":23,"method":"DOM.getBoxModel","params":{{"nodeId":{p}}}}}"#));
        assert_eq!(reply(&out, 23)["result"]["model"]["content"].as_array().unwrap().len(), 8);
        // Navigation: events for the new document.
        let out = cdp(&mut b, &mut h, 1, r#"{"id":30,"method":"Page.navigate","params":{"url":"/next"}}"#);
        assert!(reply(&out, 30)["result"]["frameId"].is_string());
        assert!(out.iter().any(|m| m["method"] == "Network.requestWillBeSent" && m["params"]["request"]["url"] == "https://cdp.test/next"));
        let id = h.fetched.last().unwrap().0;
        b.fetch_done(id, Ok(b"<script>console.log('next')</script>".to_vec()), &mut h);
        b.render(0.0, &mut h);
        let events: Vec<serde_json::Value> = b.take_cdp_output().into_iter().map(|(_, t)| serde_json::from_str(&t).unwrap()).collect();
        let methods: Vec<&str> = events.iter().filter_map(|m| m["method"].as_str()).collect();
        for want in ["Network.loadingFinished", "Page.frameNavigated", "Runtime.executionContextCreated", "Runtime.consoleAPICalled", "Page.loadEventFired"] {
            assert!(methods.contains(&want), "{want} in {methods:?}");
        }
        let at = |m: &str| methods.iter().position(|x| *x == m).unwrap();
        assert!(at("Page.frameNavigated") < at("Page.loadEventFired"));
        // Screenshots are PNG.
        let out = cdp(&mut b, &mut h, 1, r#"{"id":40,"method":"Page.captureScreenshot"}"#);
        assert!(reply(&out, 40)["result"]["data"].as_str().unwrap().starts_with("iVBORw0KGgo"));
    }

    // ----- requests, storage, cookies, observers -----------------------------

    /// Records full requests and saved storage.
    #[derive(Default)]
    struct NetHost {
        inner: TestHost,
        sent: Vec<(u32, String, crate::input::HttpRequest)>,
        saved: HashMap<String, String>,
        logs: Vec<String>,
    }

    impl Host for NetHost {
        fn fetch(&mut self, id: u32, url: &str) {
            self.inner.fetch(id, url);
        }
        fn fetch_with(&mut self, id: u32, url: &str, r: &crate::input::HttpRequest) {
            self.inner.fetch(id, url);
            self.sent.push((id, url.into(), r.clone()));
        }
        fn clipboard_get(&mut self) -> Option<String> {
            None
        }
        fn clipboard_set(&mut self, _: &str) {}
        fn set_cursor(&mut self, _: Cursor) {}
        fn set_title(&mut self, _: &str) {}
        fn log(&mut self, m: &str) {
            self.logs.push(m.into());
        }
        fn load_storage(&mut self, origin: &str) -> Option<String> {
            self.saved.get(origin).cloned()
        }
        fn save_storage(&mut self, origin: &str, data: &str) {
            self.saved.insert(origin.into(), data.into());
        }
    }

    fn net_page(b: &mut Browser, h: &mut NetHost, url: &str, html: &str) {
        b.navigate(url, h);
        let id = h.inner.fetched.last().unwrap().0;
        b.fetch_done(id, Ok(html.as_bytes().to_vec()), h);
        b.render(0.0, h);
    }

    #[test]
    fn scripts_send_any_method_and_their_headers() {
        let mut h = NetHost::default();
        let mut b = Browser::new(1000, 700, 1.0);
        net_page(
            &mut b,
            &mut h,
            "https://api.test/",
            "<script>fetch('/items/1', { method: 'delete', headers: { Authorization: 'Bearer t', 'X-Req': 'a', Cookie: 'no', 'Sec-Thing': 'no' } });\
             fetch('/items/2', { method: 'PUT', body: '{}', headers: { 'content-type': 'application/json' } });\
             const x = new XMLHttpRequest(); x.open('PATCH', '/items/3'); x.setRequestHeader('X-A', '1'); x.setRequestHeader('X-A', '2'); x.send('p');</script>",
        );
        let find = |end: &str| h.sent.iter().find(|s| s.1.ends_with(end)).unwrap().2.clone();
        let del = find("/items/1");
        assert_eq!(del.method, "DELETE");
        // (Headers keeps names lowercase; HTTP does not care.)
        assert!(del.headers.contains(&("authorization".into(), "Bearer t".into())), "{:?}", del.headers);
        assert!(del.headers.contains(&("x-req".into(), "a".into())));
        assert!(!del.headers.iter().any(|(n, _)| n == "cookie" || n == "sec-thing"), "forbidden: {:?}", del.headers);
        let put = find("/items/2");
        assert_eq!((put.method.as_str(), put.content_type.as_str(), &put.body[..]), ("PUT", "application/json", &b"{}"[..]));
        let patch = find("/items/3");
        assert_eq!(patch.method, "PATCH");
        assert!(patch.headers.contains(&("x-a".into(), "1, 2".into())), "{:?}", patch.headers);
    }

    #[test]
    fn web_storage_is_kept_per_origin_and_tab() {
        let mut h = NetHost::default();
        h.saved.insert("https://a.test".into(), "theme\tdark\n".into());
        let mut b = Browser::new(1000, 700, 1.0);
        net_page(
            &mut b,
            &mut h,
            "https://a.test/one",
            "<script>console.log('saved', localStorage.getItem('theme'), localStorage.theme);\
             localStorage.setItem('n', '1'); localStorage.x = 'y'; sessionStorage.setItem('s', 'tab');\
             try { localStorage.setItem('big', 'x'.repeat(5000001)); } catch (e) { console.log('quota', e.name); }</script>",
        );
        assert!(h.logs.contains(&"saved dark dark".to_string()), "{:?}", h.logs);
        assert!(h.logs.contains(&"quota QuotaExceededError".to_string()), "{:?}", h.logs);
        assert_eq!(h.saved["https://a.test"], "theme\tdark\nn\t1\nx\ty\n", "saved through the host");
        // Same origin, next page of the tab: both areas are there.
        net_page(
            &mut b,
            &mut h,
            "https://a.test/two",
            "<script>console.log('two', localStorage.n, sessionStorage.getItem('s'), localStorage.length, Object.keys(localStorage).join(','));\
             delete localStorage.x;</script>",
        );
        assert!(h.logs.contains(&"two 1 tab 3 theme,n,x".to_string()), "{:?}", h.logs);
        assert_eq!(h.saved["https://a.test"], "theme\tdark\nn\t1\n");
        // Another origin sees nothing; a new tab has its own session storage.
        net_page(&mut b, &mut h, "https://b.test/", "<script>console.log('b', localStorage.length)</script>");
        assert!(h.logs.contains(&"b 0".to_string()), "{:?}", h.logs);
        b.new_tab("https://a.test/three", &mut h);
        let id = h.inner.fetched.last().unwrap().0;
        b.fetch_done(id, Ok(b"<script>console.log('tab2', localStorage.n, sessionStorage.length)</script>".to_vec()), &mut h);
        b.render(0.0, &mut h);
        assert!(h.logs.contains(&"tab2 1 0".to_string()), "{:?}", h.logs);
    }

    #[test]
    fn document_cookie_uses_the_shared_jar() {
        let mut h = NetHost::default();
        let mut b = Browser::new(1000, 700, 1.0);
        net_page(
            &mut b,
            &mut h,
            "https://cookies.test/",
            "<script>document.cookie = 'a=1; path=/'; document.cookie = 'b=2'; document.cookie = 'a=; Max-Age=0';\
             console.log('cookie', document.cookie, navigator.cookieEnabled);</script>",
        );
        assert!(h.logs.contains(&"cookie b=2 true".to_string()), "{:?}", h.logs);
        assert_eq!(crate::net::cookies::jar().document_cookie("https://cookies.test/x"), "b=2");
    }

    #[test]
    fn mutation_observers_see_script_changes() {
        let mut h = NetHost::default();
        let mut b = Browser::new(1000, 700, 1.0);
        net_page(
            &mut b,
            &mut h,
            "https://mo.test/",
            r#"<div id=root><p id=p class=a>text</p></div><script>
              const root = document.getElementById('root'), p = document.getElementById('p');
              const log = (rs) => console.log(rs.map((r) => [r.type, r.target.id || r.target.nodeName, r.attributeName, r.oldValue,
                r.addedNodes.map((n) => n.nodeName).join('+'), r.removedNodes.map((n) => n.nodeName).join('+')].join(':')).join(' | '));
              const mo = new MutationObserver(log);
              mo.observe(root, { childList: true, subtree: true, attributes: true, attributeOldValue: true, characterData: true, attributeFilter: ['class', 'title'] });
              p.className = 'b';
              p.setAttribute('data-x', '1');
              p.title = 't';
              root.appendChild(document.createElement('span'));
              p.remove();
              const other = new MutationObserver(() => console.log('other'));
              other.observe(p, { attributes: true });
              other.disconnect();
              p.id = 'gone';
              setTimeout(() => {
                const t = new MutationObserver(log);
                t.observe(root, { characterData: true, subtree: true, characterDataOldValue: true });
                root.appendChild(document.createTextNode('x'));
                root.lastChild.data = 'y';
                console.log('taken', t.takeRecords().length);
                root.innerHTML = '<b>new</b>';
              }, 5);
            </script>"#,
        );
        b.render(10.0, &mut h);
        let logs = h.logs.join("\n");
        // (records are delivered after the script: p's id is 'gone' by then)
        assert!(
            logs.contains("attributes:gone:class:a:: | attributes:gone:title::: | childList:root:::SPAN: | childList:root::::P"),
            "{logs}"
        );
        assert!(!logs.contains("other"), "a disconnected observer gets nothing: {logs}");
        // The second observer's record was taken; the first one (no old
        // values) still sees the text and innerHTML changes.
        assert!(logs.contains("taken 1"), "{logs}");
        assert!(logs.contains("childList:root:::#text: | characterData:#text:::: | childList:root:::B:SPAN+#text"), "{logs}");
    }

    #[test]
    fn intersection_and_resize_observers_follow_layout() {
        let mut h = NetHost::default();
        let mut b = Browser::new(1000, 700, 1.0);
        net_page(
            &mut b,
            &mut h,
            "https://io.test/",
            r#"<div style="height: 2000px"></div><div id=far style="height: 50px">far</div><div id=box style="width: 100px; padding: 5px">box</div><script>
              const io = new IntersectionObserver((es) => es.forEach((e) => console.log('io', e.target.id, e.isIntersecting, e.intersectionRatio)));
              io.observe(document.getElementById('far'));
              const ro = new ResizeObserver((es) => es.forEach((e) => console.log('ro', e.target.id, e.contentRect.width)));
              ro.observe(document.getElementById('box'));
              setTimeout(() => { document.getElementById('box').style.width = '150px'; }, 100);
            </script>"#,
        );
        b.render(50.0, &mut h);
        assert!(h.logs.contains(&"io far false 0".to_string()), "{:?}", h.logs);
        assert!(h.logs.contains(&"ro box 100".to_string()), "{:?}", h.logs);
        // Scrolling the page brings it into view.
        b.event(Event::MouseMove { x: 300, y: 300 }, 60.0, &mut h);
        for _ in 0..20 {
            b.event(Event::Wheel { dx: 0, dy: 120 }, 60.0, &mut h);
        }
        b.render(200.0, &mut h);
        b.render(300.0, &mut h);
        assert!(h.logs.contains(&"io far true 1".to_string()), "{:?}", h.logs);
        assert!(h.logs.contains(&"ro box 150".to_string()), "{:?}", h.logs);
    }

    // ----- CORS -------------------------------------------------------------

    /// Answers request `id` with `status`, `headers` and `body`.
    fn answer(b: &mut Browser, h: &mut NetHost, id: u32, status: u16, headers: &[(&str, &str)], body: &str) {
        let headers = headers.iter().map(|(n, v)| (n.to_string(), v.to_string())).collect();
        b.fetch_response(id, ResponseInfo { status, headers });
        b.fetch_done(id, Ok(body.as_bytes().to_vec()), h);
        b.render(0.0, h);
    }

    fn sent_to<'a>(h: &'a NetHost, end: &str) -> Vec<&'a (u32, String, crate::input::HttpRequest)> {
        h.sent.iter().filter(|s| s.1.ends_with(end)).collect()
    }

    #[test]
    fn cors_simple_requests_and_exposed_headers() {
        let mut h = NetHost::default();
        let mut b = Browser::new(1000, 700, 1.0);
        net_page(
            &mut b,
            &mut h,
            "https://app.test/",
            "<script>\
             fetch('https://api.test/open').then((r) => r.text().then((t) => console.log('open', r.status, t, r.headers.get('x-secret'), r.headers.get('x-shown'), r.headers.get('content-type'))), (e) => console.log('open failed', e.name));\
             fetch('https://api.test/closed').then(() => console.log('closed ok'), (e) => console.log('closed failed', e.name));\
             fetch('https://api.test/creds', { credentials: 'include' }).then(() => console.log('creds ok'), () => console.log('creds failed'));\
             fetch('https://api.test/opaque', { mode: 'no-cors' }).then((r) => console.log('opaque', r.type, r.status));\
             fetch('https://api.test/same', { mode: 'same-origin' }).catch((e) => console.log('same-origin mode', e.name));\
             fetch('/moved').then(() => console.log('moved ok'), () => console.log('moved failed'));</script>",
        );
        let open = sent_to(&h, "/open")[0].clone();
        assert_eq!(open.2.origin, "https://app.test", "cross-origin requests carry Origin");
        assert!(open.2.omit_credentials, "and no cookies by default");
        let creds = sent_to(&h, "/creds")[0].clone();
        assert!(!creds.2.omit_credentials);
        assert!(sent_to(&h, "/same").is_empty(), "mode same-origin is not sent");
        answer(&mut b, &mut h, open.0, 200, &[("access-control-allow-origin", "*"), ("x-secret", "s"), ("x-shown", "v"), ("access-control-expose-headers", "X-Shown"), ("content-type", "text/plain")], "hello");
        let closed = sent_to(&h, "/closed")[0].0;
        answer(&mut b, &mut h, closed, 200, &[], "secret");
        answer(&mut b, &mut h, creds.0, 200, &[("access-control-allow-origin", "*")], "x");
        let opaque = sent_to(&h, "/opaque")[0].0;
        answer(&mut b, &mut h, opaque, 200, &[], "hidden");
        // A same-origin request redirected to another origin is checked too.
        let moved = sent_to(&h, "/moved")[0].0;
        b.fetch_redirected(moved, "https://elsewhere.test/x");
        answer(&mut b, &mut h, moved, 200, &[], "x");
        b.render(10.0, &mut h);
        let logs = h.logs.join("\n");
        assert!(logs.contains("open 200 hello null v text/plain"), "{logs}");
        assert!(logs.contains("closed failed TypeError"), "{logs}");
        assert!(logs.contains("No 'Access-Control-Allow-Origin' header is present"), "{logs}");
        assert!(logs.contains("creds failed"), "{logs}");
        assert!(logs.contains("opaque opaque 0"), "{logs}");
        assert!(logs.contains("same-origin mode TypeError"), "{logs}");
        assert!(logs.contains("moved failed"), "{logs}");
    }

    #[test]
    fn cors_preflight() {
        let mut h = NetHost::default();
        let mut b = Browser::new(1000, 700, 1.0);
        net_page(
            &mut b,
            &mut h,
            "https://app.test/",
            "<script>\
             fetch('https://api.test/put', { method: 'PUT', body: '{}', headers: { 'content-type': 'application/json', 'X-Token': 't' }, credentials: 'include' })\
               .then((r) => r.text()).then((t) => console.log('put', t), (e) => console.log('put failed'));\
             fetch('https://api.test/denied', { method: 'DELETE' }).then(() => console.log('denied ok'), () => console.log('denied failed'));</script>",
        );
        let pre = sent_to(&h, "/put")[0].clone();
        assert_eq!(pre.2.method, "OPTIONS");
        assert!(pre.2.omit_credentials, "preflights send no cookies");
        assert!(pre.2.headers.contains(&("Access-Control-Request-Method".into(), "PUT".into())), "{:?}", pre.2.headers);
        assert!(pre.2.headers.contains(&("Access-Control-Request-Headers".into(), "content-type,x-token".into())), "{:?}", pre.2.headers);
        let allow = [
            ("access-control-allow-origin", "https://app.test"),
            ("access-control-allow-credentials", "true"),
            ("access-control-allow-methods", "PUT"),
            ("access-control-allow-headers", "Content-Type, X-Token"),
        ];
        answer(&mut b, &mut h, pre.0, 204, &allow, "");
        let actual = sent_to(&h, "/put")[1].clone();
        assert_eq!((actual.2.method.as_str(), &actual.2.body[..]), ("PUT", &b"{}"[..]));
        assert!(!actual.2.omit_credentials);
        answer(&mut b, &mut h, actual.0, 200, &allow[..2], "done");
        let denied = sent_to(&h, "/denied")[0].clone();
        assert_eq!(denied.2.method, "OPTIONS");
        answer(&mut b, &mut h, denied.0, 204, &[("access-control-allow-origin", "*"), ("access-control-allow-methods", "PUT")], "");
        assert_eq!(sent_to(&h, "/denied").len(), 1, "the request itself is not sent");
        b.render(10.0, &mut h);
        let logs = h.logs.join("\n");
        assert!(logs.contains("put done"), "{logs}");
        assert!(logs.contains("denied failed"), "{logs}");
        assert!(logs.contains("Method DELETE is not allowed"), "{logs}");
    }
}
