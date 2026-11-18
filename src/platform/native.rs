//! Native window: winit for the window and input, softbuffer to show pixels,
//! ureq and the file system for loading, arboard for the clipboard, rfd for
//! the file dialog of `<input type=file>`.

use std::error::Error;
use std::num::NonZeroU32;
use std::rc::Rc;
use std::time::{Duration, Instant};

use softbuffer::{Context, Surface};
use winit::application::ApplicationHandler;
use winit::dpi::LogicalSize;
use winit::event::{ElementState, MouseButton, MouseScrollDelta, StartCause, WindowEvent};
use winit::event_loop::{ActiveEventLoop, ControlFlow, EventLoop, EventLoopProxy};
use winit::keyboard::{Key, NamedKey};
use winit::window::{CursorIcon, Window, WindowId};

use crate::abi::key;
use crate::browser::{Browser, ResponseInfo};
use crate::document::FileData;
use crate::input::{Cursor, Event, Host, HttpRequest, Modifiers, WsEvent};
use crate::net::url;
use crate::text::Fonts;

/// Largest response body the browser accepts.
const MAX_BODY: u64 = 32 << 20;

/// Opens the browser window (showing `url` if given) and runs until closed.
/// Opens the browser window at `url` (or the home page). `javascript`:
/// whether web pages may run scripts.
pub fn run(url: Option<String>, javascript: bool) -> Result<(), Box<dyn Error>> {
    let event_loop = EventLoop::<Fetched>::with_user_event().build()?;
    let proxy = event_loop.create_proxy();
    let mut app = App {
        state: None,
        url,
        javascript,
        proxy,
        start: Instant::now(),
    };
    event_loop.run_app(&mut app)?;
    Ok(())
}

/// A finished fetch, sent from a loader thread to the event loop (or, with
/// `ws`, what happened on WebSocket connection `id`).
struct Fetched {
    id: u32,
    ws: Option<WsEvent>,
    result: Result<Vec<u8>, String>,
    /// The URL after redirects, if it changed.
    final_url: Option<String>,
    /// Status and headers (HTTP responses).
    info: Option<ResponseInfo>,
}

impl Fetched {
    fn new(id: u32, loaded: Result<Loaded, String>) -> Fetched {
        match loaded {
            Ok(l) => Fetched {
                id,
                ws: None,
                result: Ok(l.body),
                final_url: l.final_url,
                info: l.info,
            },
            Err(e) => Fetched {
                id,
                ws: None,
                result: Err(e),
                final_url: None,
                info: None,
            },
        }
    }

    fn ws(id: u32, event: WsEvent) -> Fetched {
        Fetched { id, ws: Some(event), result: Ok(Vec::new()), final_url: None, info: None }
    }

    /// Hands the fetch to `browser`.
    fn deliver(self, browser: &mut Browser, host: &mut dyn Host) {
        if let Some(event) = self.ws {
            return browser.ws_event(self.id, event, host);
        }
        if let Some(u) = &self.final_url {
            browser.fetch_redirected(self.id, u);
        }
        if let Some(info) = self.info {
            browser.fetch_response(self.id, info);
        }
        browser.fetch_done(self.id, self.result, host);
    }
}

/// A loaded resource.
pub struct Loaded {
    pub body: Vec<u8>,
    /// The URL after redirects, if it changed.
    pub final_url: Option<String>,
    /// Status and headers of an HTTP response.
    pub info: Option<ResponseInfo>,
}

struct App {
    state: Option<State>,
    url: Option<String>,
    javascript: bool,
    proxy: EventLoopProxy<Fetched>,
    start: Instant,
}

struct State {
    window: Rc<Window>,
    surface: Surface<Rc<Window>, Rc<Window>>,
    browser: Browser,
    host: NativeHost,
    mods: Modifiers,
    /// A Ctrl-click (macOS secondary click) is held down.
    ctrl_click: bool,
    scale: f64,
    /// Last pointer position in logical pixels (button events have none).
    pointer: (i32, i32),
}

struct NativeHost {
    window: Rc<Window>,
    proxy: EventLoopProxy<Fetched>,
    sockets: Sockets,
    clipboard: Option<arboard::Clipboard>,
    cursor: Cursor,
    title: String,
}

/// Open WebSocket connections, by id.
type Sockets = std::collections::HashMap<u32, std::sync::mpsc::Sender<super::ws::Command>>;

fn ws_send(sockets: &Sockets, id: u32, data: &[u8], text: bool) {
    if let Some(tx) = sockets.get(&id) {
        let _ = tx.send(super::ws::Command::Send(data.to_vec(), text));
    }
}

fn ws_close(sockets: &mut Sockets, id: u32, code: u16, reason: &str) {
    if let Some(tx) = sockets.remove(&id) {
        let _ = tx.send(super::ws::Command::Close(code, reason.to_string()));
    }
}

impl Host for NativeHost {
    fn fetch(&mut self, id: u32, url: &str) {
        self.fetch_with(id, url, &HttpRequest::default());
    }

    fn ws_open(&mut self, id: u32, url: &str, protocols: &[String], origin: &str) {
        let proxy = self.proxy.clone();
        let tx = super::ws::open(url, protocols, origin, &user_agent(), move |e| {
            let _ = proxy.send_event(Fetched::ws(id, e));
        });
        self.sockets.insert(id, tx);
    }

    fn ws_send(&mut self, id: u32, data: &[u8], text: bool) {
        ws_send(&self.sockets, id, data, text);
    }

    fn ws_close(&mut self, id: u32, code: u16, reason: &str) {
        ws_close(&mut self.sockets, id, code, reason);
    }

    fn fetch_with(&mut self, id: u32, url: &str, request: &HttpRequest) {
        let proxy = self.proxy.clone();
        let url = url.to_string();
        let request = (!request.method.is_empty() || !request.referrer.is_empty()).then(|| request.clone());
        std::thread::spawn(move || {
            let _ = proxy.send_event(Fetched::new(id, load_full(&url, request.as_ref())));
        });
    }

    fn pick_files(&mut self, multiple: bool, accept: &str) -> Option<Vec<FileData>> {
        let mut dialog = rfd::FileDialog::new().set_parent(&*self.window);
        let exts = accept_extensions(accept);
        if !exts.is_empty() {
            dialog = dialog.add_filter("Accepted files", &exts);
        }
        let paths = if multiple {
            dialog.pick_files().unwrap_or_default()
        } else {
            dialog.pick_file().into_iter().collect()
        };
        Some(paths.iter().filter_map(|p| read_file(p, self)).collect())
    }

    fn save_download(&mut self, name: &str, bytes: &[u8]) -> Result<String, String> {
        save_download(name, bytes)
    }

    fn clipboard_get(&mut self) -> Option<String> {
        self.clipboard.as_mut()?.get_text().ok()
    }

    fn clipboard_set(&mut self, text: &str) {
        if let Some(c) = self.clipboard.as_mut() {
            let _ = c.set_text(text.to_string());
        }
    }

    fn set_cursor(&mut self, cursor: Cursor) {
        if cursor != self.cursor {
            self.cursor = cursor;
            self.window.set_cursor(match cursor {
                Cursor::Default => CursorIcon::Default,
                Cursor::Pointer => CursorIcon::Pointer,
                Cursor::Text => CursorIcon::Text,
            });
        }
    }

    fn set_title(&mut self, title: &str) {
        if title != self.title {
            self.title = title.to_string();
            self.window.set_title(title);
        }
    }

    fn log(&mut self, message: &str) {
        eprintln!("[console] {message}");
    }

    fn wall_clock(&mut self) -> Option<f64> {
        let t = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .ok()?;
        Some(t.as_secs_f64() * 1000.0)
    }
}

/// A User-Agent in the format sites expect from current browsers (the
/// usual compatibility tokens), naming rubrowser.
fn user_agent() -> String {
    let platform = if cfg!(target_os = "macos") {
        "Macintosh; Intel Mac OS X 10_15_7"
    } else if cfg!(target_os = "windows") {
        "Windows NT 10.0; Win64; x64"
    } else {
        "X11; Linux x86_64"
    };
    format!(
        "Mozilla/5.0 ({platform}) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/130.0.0.0 Safari/537.36 rubrowser/{}",
        env!("CARGO_PKG_VERSION")
    )
}

/// The HTTP client shared by all loads.
fn agent() -> &'static ureq::Agent {
    static AGENT: std::sync::OnceLock<ureq::Agent> = std::sync::OnceLock::new();
    AGENT.get_or_init(|| {
        ureq::Agent::config_builder()
            .user_agent(user_agent())
            .timeout_global(Some(Duration::from_secs(30)))
            .http_status_as_error(false)
            .build()
            .into()
    })
}

/// Requests to one host at a time, as browsers limit their connections
/// per host (HTTP/1.1); more wait for a free slot.
const PER_HOST: usize = 6;

/// Requests in flight per host, and a signal when one ends.
fn host_slots() -> &'static (
    std::sync::Mutex<std::collections::HashMap<String, usize>>,
    std::sync::Condvar,
) {
    static SLOTS: std::sync::OnceLock<(
        std::sync::Mutex<std::collections::HashMap<String, usize>>,
        std::sync::Condvar,
    )> = std::sync::OnceLock::new();
    SLOTS.get_or_init(Default::default)
}

/// A request slot for one host, released when dropped.
struct HostSlot(String);

impl HostSlot {
    fn take(url: &str) -> HostSlot {
        let host = url
            .split_once("://")
            .map_or(url, |(_, rest)| rest)
            .split(['/', '?', '#'])
            .next()
            .unwrap_or("")
            .to_ascii_lowercase();
        let (busy, freed) = host_slots();
        let mut busy = busy.lock().unwrap_or_else(|e| e.into_inner());
        while busy.get(&host).copied().unwrap_or(0) >= PER_HOST {
            busy = freed.wait(busy).unwrap_or_else(|e| e.into_inner());
        }
        *busy.entry(host.clone()).or_default() += 1;
        HostSlot(host)
    }
}

impl Drop for HostSlot {
    fn drop(&mut self) {
        let (busy, freed) = host_slots();
        let mut busy = busy.lock().unwrap_or_else(|e| e.into_inner());
        if let Some(n) = busy.get_mut(&self.0) {
            *n = n.saturating_sub(1);
        }
        freed.notify_all();
    }
}

/// Extensions for an `accept` attribute (`.png`, `image/*`, `text/csv`),
/// for the file dialog's filter; empty when anything goes.
fn accept_extensions(accept: &str) -> Vec<String> {
    let mut exts = Vec::new();
    for token in accept.split(',').map(str::trim).filter(|t| !t.is_empty()) {
        let found: Vec<String> = match token.strip_prefix('.') {
            Some(ext) => vec![ext.to_ascii_lowercase()],
            None => crate::document::extensions_for_mime(token)
                .into_iter()
                .map(String::from)
                .collect(),
        };
        if found.is_empty() {
            // A type this browser has no extensions for: do not hide files.
            return Vec::new();
        }
        exts.extend(found);
    }
    exts.dedup();
    exts
}

/// Reads a file chosen in the file dialog.
fn read_file(path: &std::path::Path, host: &mut dyn Host) -> Option<FileData> {
    let bytes = match std::fs::read(path) {
        Ok(b) => b,
        Err(e) => {
            host.log(&format!("{}: {e}", path.display()));
            return None;
        }
    };
    let modified = std::fs::metadata(path)
        .and_then(|m| m.modified())
        .ok()
        .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
        .map_or(0.0, |d| d.as_millis() as f64);
    let name = path.file_name()?.to_string_lossy();
    Some(FileData::new(&name, bytes, modified))
}

/// The folder downloads are saved in: `RUBROWSER_DOWNLOAD_DIR`, else the
/// user's download folder (XDG's on Linux), else `~/Downloads`.
pub fn download_dir() -> std::path::PathBuf {
    use std::path::PathBuf;
    if let Some(d) = std::env::var_os("RUBROWSER_DOWNLOAD_DIR").filter(|d| !d.is_empty()) {
        return PathBuf::from(d);
    }
    let home = std::env::var_os("HOME").map(PathBuf::from);
    if cfg!(target_os = "linux") {
        if let Some(d) = std::env::var_os("XDG_DOWNLOAD_DIR").filter(|d| !d.is_empty()) {
            return PathBuf::from(d);
        }
        // user-dirs.dirs: XDG_DOWNLOAD_DIR="$HOME/Downloads"
        let config = std::env::var_os("XDG_CONFIG_HOME")
            .map(PathBuf::from)
            .or_else(|| home.as_ref().map(|h| h.join(".config")));
        if let (Some(config), Some(home)) = (config, &home)
            && let Ok(text) = std::fs::read_to_string(config.join("user-dirs.dirs"))
        {
            for line in text.lines() {
                if let Some(v) = line.trim().strip_prefix("XDG_DOWNLOAD_DIR=") {
                    let v = v.trim().trim_matches('"');
                    let v = v.replace("$HOME", &home.to_string_lossy());
                    if !v.is_empty() {
                        return PathBuf::from(v);
                    }
                }
            }
        }
    }
    home.map_or_else(|| PathBuf::from("."), |h| h.join("Downloads"))
}

/// Writes a download named `name` (a safe file name) into
/// [`download_dir`] without replacing a file: `name (1).ext` and so on.
/// Returns the path.
pub fn save_download(name: &str, bytes: &[u8]) -> Result<String, String> {
    use std::io::Write;
    let dir = download_dir();
    std::fs::create_dir_all(&dir).map_err(|e| format!("{}: {e}", dir.display()))?;
    let (stem, ext) = match name.rsplit_once('.') {
        Some((s, e)) if !s.is_empty() => (s, format!(".{e}")),
        _ => (name, String::new()),
    };
    for n in 0..1000 {
        let file = if n == 0 {
            name.to_string()
        } else {
            format!("{stem} ({n}){ext}")
        };
        let path = dir.join(file);
        match std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&path)
        {
            Ok(mut f) => {
                f.write_all(bytes)
                    .map_err(|e| format!("{}: {e}", path.display()))?;
                return Ok(path.display().to_string());
            }
            Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => continue,
            Err(e) => return Err(format!("{}: {e}", path.display())),
        }
    }
    Err(format!("too many files named {name}"))
}

/// Loads `url` over HTTP(S) or from the file system. Returns the bytes and,
/// if redirects led elsewhere, the final URL.
pub fn load(url: &str) -> Result<(Vec<u8>, Option<String>), String> {
    load_with(url, None)
}

/// Like [`load`], with a script's method and body.
pub fn load_with(
    url: &str,
    request: Option<&HttpRequest>,
) -> Result<(Vec<u8>, Option<String>), String> {
    load_full(url, request).map(|l| (l.body, l.final_url))
}

/// Like [`load_with`], with the response's status and headers.
pub fn load_full(url: &str, request: Option<&HttpRequest>) -> Result<Loaded, String> {
    let file = |b: Vec<u8>| Loaded {
        body: b,
        final_url: None,
        info: None,
    };
    // The fragment is for the browser, not the server.
    let url = url.split('#').next().unwrap_or(url);
    match url::scheme(url) {
        Some("http") | Some("https") => {
            let _slot = HostSlot::take(url);
            let get = request.is_none_or(|r| r.method == "GET");
            match http(url, request) {
                // A dropped connection (a server closing one it kept open,
                // say): a GET is safe to try once more, as browsers do.
                Err(e) if get && is_transport_error(&e) => http(url, request),
                r => r,
            }
        }
        Some("file") => {
            let path = url.trim_start_matches("file://");
            let path = path.split('?').next().unwrap_or(path);
            let path = String::from_utf8_lossy(&url::percent_decode(path)).into_owned();
            std::fs::read(&path)
                .map(file)
                .map_err(|e| format!("{path}: {e}"))
        }
        Some(other) => Err(format!("unsupported scheme `{other}:`")),
        None => std::fs::read(url)
            .map(file)
            .map_err(|e| format!("{url}: {e}")),
    }
}

/// Whether a load failed on the connection itself (not a timeout or an
/// HTTP status).
fn is_transport_error(e: &str) -> bool {
    e.starts_with("io:") || e.contains("onnection")
}

/// One HTTP(S) request.
fn http(url: &str, request: Option<&HttpRequest>) -> Result<Loaded, String> {
    use ureq::ResponseExt;
    {
        {
            let accept = "text/html,application/xhtml+xml,application/xml;q=0.9,image/avif,image/webp,*/*;q=0.8";
            let referrer = request.map_or("", |r| r.referrer.as_str());
            let mut resp = match request.filter(|r| r.method != "GET" && !r.method.is_empty()) {
                Some(r) => {
                    let ctype = if r.content_type.is_empty() {
                        "text/plain;charset=UTF-8"
                    } else {
                        &r.content_type
                    };
                    let mut req = agent()
                        .post(url)
                        .header("Accept", "*/*")
                        .header("Accept-Language", crate::net::accept_language())
                        .header("Content-Type", ctype);
                    if !referrer.is_empty() {
                        req = req.header("Referer", referrer);
                    }
                    req.send(&r.body[..])
                }
                None => {
                    // script requests (fetch, XHR) accept anything
                    let script = request.is_some_and(|r| !r.method.is_empty());
                    let mut req = agent()
                        .get(url)
                        .header("Accept", if script { "*/*" } else { accept })
                        .header("Accept-Language", crate::net::accept_language());
                    if !referrer.is_empty() {
                        req = req.header("Referer", referrer);
                    }
                    req.call()
                }
            }
            .map_err(|e| e.to_string())?;
            let status = resp.status();
            let final_url = resp.get_uri().to_string();
            let info = ResponseInfo {
                status: status.as_u16(),
                headers: resp
                    .headers()
                    .iter()
                    .map(|(n, v)| {
                        (
                            n.as_str().to_string(),
                            String::from_utf8_lossy(v.as_bytes()).into_owned(),
                        )
                    })
                    .collect(),
            };
            let body = resp
                .body_mut()
                .with_config()
                .limit(MAX_BODY)
                .read_to_vec()
                .map_err(|e| e.to_string())?;
            if status.is_client_error() || status.is_server_error() {
                // Error pages are still pages; resources fail.
                if !status.is_success() && body.is_empty() {
                    return Err(format!("HTTP {status}"));
                }
            }
            Ok(Loaded {
                body,
                final_url: (final_url != url).then_some(final_url),
                info: Some(info),
            })
        }
    }
}

/// Options for [`screenshot`].
pub struct Shot {
    pub width: u32,
    pub height: u32,
    pub scale: f32,
    /// Run page scripts.
    pub javascript: bool,
    /// Give up waiting for resources after this long.
    pub timeout: Duration,
}

/// Loads `url` without a window, waits for its resources, and writes the
/// rendered window (chrome and page) to the PNG file `out`.
pub fn screenshot(url: &str, out: &str, shot: &Shot) -> Result<(), Box<dyn Error>> {
    use std::sync::mpsc;
    struct Headless {
        tx: mpsc::Sender<Fetched>,
        pending: usize,
        sockets: Sockets,
    }
    impl Host for Headless {
        fn fetch(&mut self, id: u32, url: &str) {
            self.fetch_with(id, url, &HttpRequest::default());
        }
        fn ws_open(&mut self, id: u32, url: &str, protocols: &[String], origin: &str) {
            let tx = self.tx.clone();
            let socket = super::ws::open(url, protocols, origin, &user_agent(), move |e| {
                let _ = tx.send(Fetched::ws(id, e));
            });
            self.sockets.insert(id, socket);
        }
        fn ws_send(&mut self, id: u32, data: &[u8], text: bool) {
            ws_send(&self.sockets, id, data, text);
        }
        fn ws_close(&mut self, id: u32, code: u16, reason: &str) {
            ws_close(&mut self.sockets, id, code, reason);
        }
        fn fetch_with(&mut self, id: u32, url: &str, request: &HttpRequest) {
            self.pending += 1;
            let tx = self.tx.clone();
            let url = url.to_string();
            let request = (!request.method.is_empty() || !request.referrer.is_empty()).then(|| request.clone());
            std::thread::spawn(move || {
                let _ = tx.send(Fetched::new(id, load_full(&url, request.as_ref())));
            });
        }
        // No window: file inputs stay empty (the default `pick_files`).
        fn save_download(&mut self, name: &str, bytes: &[u8]) -> Result<String, String> {
            save_download(name, bytes)
        }
        fn clipboard_get(&mut self) -> Option<String> {
            None
        }
        fn clipboard_set(&mut self, text: &str) {
            eprintln!("[clipboard] {text}");
        }
        fn set_cursor(&mut self, _: Cursor) {}
        fn set_title(&mut self, _: &str) {}
        fn log(&mut self, message: &str) {
            eprintln!("[console] {message}");
        }
        fn wall_clock(&mut self) -> Option<f64> {
            let t = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .ok()?;
            Some(t.as_secs_f64() * 1000.0)
        }
    }
    let (tx, rx) = mpsc::channel();
    let mut host = Headless { tx, pending: 0, sockets: Sockets::new() };
    let (pw, ph) = (
        (shot.width as f32 * shot.scale) as usize,
        (shot.height as f32 * shot.scale) as usize,
    );
    let mut browser = Browser::with_fonts(pw, ph, shot.scale, Fonts::with_system_fonts());
    browser.set_javascript(shot.javascript);
    let start = Instant::now();
    let now = |s: &Instant| s.elapsed().as_secs_f64() * 1000.0;
    browser.navigate(&url::from_input(url), &mut host);
    let deadline = start + shot.timeout;
    // Loads until nothing is pending (or the deadline).
    let pump = |browser: &mut Browser, host: &mut Headless| loop {
        browser.render(now(&start), host);
        if host.pending == 0 {
            // WebSocket messages that arrived meanwhile.
            let mut any = false;
            while let Ok(f) = rx.try_recv() {
                any = true;
                if f.ws.is_none() {
                    host.pending -= 1;
                }
                f.deliver(browser, host);
            }
            if any {
                continue;
            }
            break;
        }
        let wait = deadline.saturating_duration_since(Instant::now());
        match rx.recv_timeout(wait) {
            Ok(first) => {
                // Deliver everything that has arrived, then render once.
                let mut next = Some(first);
                while let Some(f) = next {
                    if f.ws.is_none() {
                        host.pending -= 1;
                    }
                    f.deliver(browser, host);
                    next = rx.try_recv().ok();
                }
            }
            Err(_) => {
                eprintln!(
                    "rubrowser: gave up waiting for {} resource(s)",
                    host.pending
                );
                break;
            }
        }
    };
    pump(&mut browser, &mut host);
    browser.render(now(&start), &mut host);
    // A debugging aid: input to play before the screenshot, e.g.
    // RUBROWSER_INPUT="down 300 400; move 360 420; up 360 420; key C cmd; keydown a; keyup a; type 文字; js console.log(1); wait 500"
    // (window logical px; keys are letters or escape/enter/tab/left/right/up/down).
    if let Ok(script) = std::env::var("RUBROWSER_INPUT") {
        for step in script.split(';').map(str::trim).filter(|s| !s.is_empty()) {
            let w: Vec<&str> = step.split_whitespace().collect();
            let num = |i: usize| w.get(i).and_then(|v| v.parse::<i32>().ok()).unwrap_or(0);
            let t = now(&start);
            match w[0] {
                "down" => browser.event(Event::MouseDown { x: num(1), y: num(2), button: 0 }, t, &mut host),
                "right" => browser.event(Event::MouseDown { x: num(1), y: num(2), button: 2 }, t, &mut host),
                "up" => browser.event(Event::MouseUp { x: num(1), y: num(2), button: 0 }, t, &mut host),
                "move" => browser.event(Event::MouseMove { x: num(1), y: num(2) }, t, &mut host),
                "wheel" => {
                    browser.event(Event::MouseMove { x: num(1), y: num(2) }, t, &mut host);
                    browser.event(Event::Wheel { dx: 0, dy: num(3) }, t, &mut host);
                }
                "click" => {
                    browser.event(Event::MouseMove { x: num(1), y: num(2) }, t, &mut host);
                    browser.event(Event::MouseDown { x: num(1), y: num(2), button: 0 }, t, &mut host);
                    browser.event(Event::MouseUp { x: num(1), y: num(2), button: 0 }, t, &mut host);
                }
                "key" | "keydown" | "keyup" => {
                    let k = w.get(1).copied().unwrap_or("");
                    let code = match k {
                        "escape" => key::ESCAPE,
                        "enter" => key::ENTER,
                        "tab" => key::TAB,
                        "left" => key::LEFT,
                        "right" => key::RIGHT,
                        "up" => key::UP,
                        "down" => key::DOWN,
                        "[" => key::LEFT_BRACKET,
                        "]" => key::RIGHT_BRACKET,
                        k => k.chars().next().map_or(0, |c| c.to_ascii_uppercase() as i32),
                    };
                    let has = |m: &str| w[2..].contains(&m);
                    let mods = Modifiers { shift: has("shift"), ctrl: has("ctrl"), alt: has("alt"), meta: has("cmd") };
                    if w[0] != "keyup" {
                        browser.event(Event::KeyDown { key: code, mods, repeat: false }, t, &mut host);
                    }
                    if w[0] != "keydown" {
                        browser.event(Event::KeyUp { key: code, mods }, t, &mut host);
                    }
                }
                "wait" => std::thread::sleep(Duration::from_millis(num(1).max(0) as u64)),
                "type" => browser.event(Event::Text(step[4..].trim_start().to_string()), t, &mut host),
                "js" => browser.run_script(&step[2..], &mut host),
                other => eprintln!("rubrowser: RUBROWSER_INPUT: unknown step {other}"),
            }
            pump(&mut browser, &mut host);
        }
        browser.render(now(&start), &mut host);
    }
    // Let animations that run on load (fade-ins) settle, as a person
    // looking at the page would.
    let settle = Instant::now() + std::time::Duration::from_secs(2);
    while browser.animating() && Instant::now() < settle {
        std::thread::sleep(std::time::Duration::from_millis(16));
        browser.render(now(&start), &mut host);
    }
    if let Ok(sel) = std::env::var("RUBROWSER_DUMP") {
        eprint!("{}", browser.dump_layout(&sel, 12));
    }
    if std::env::var_os("RUBROWSER_PROFILE").is_some() {
        // Time hover restyles: move the mouse over a few points.
        for (x, y) in [(400, 400), (420, 410), (700, 300), (100, 300)] {
            eprintln!("[profile] mouse move to {x},{y}");
            browser.event(Event::MouseMove { x, y }, now(&start), &mut host);
            browser.render(now(&start), &mut host);
        }
    }
    let canvas = browser.render(now(&start), &mut host);
    let rgba: Vec<u8> = canvas
        .pixels()
        .iter()
        .flat_map(|&p| [(p >> 16) as u8, (p >> 8) as u8, p as u8, 255])
        .collect();
    let img = image::RgbaImage::from_raw(canvas.width() as u32, canvas.height() as u32, rgba)
        .ok_or("bad canvas size")?;
    img.save(out)?;
    eprintln!(
        "rubrowser: wrote {out} ({}x{}) in {:.1}s",
        canvas.width(),
        canvas.height(),
        start.elapsed().as_secs_f64()
    );
    Ok(())
}

impl App {
    fn now(&self) -> f64 {
        self.start.elapsed().as_secs_f64() * 1000.0
    }
}

impl ApplicationHandler<Fetched> for App {
    fn resumed(&mut self, event_loop: &ActiveEventLoop) {
        if self.state.is_some() {
            return;
        }
        let attrs = Window::default_attributes()
            .with_title("rubrowser")
            .with_inner_size(LogicalSize::new(1100.0, 800.0));
        let window = Rc::new(event_loop.create_window(attrs).expect("create window"));
        let context = Context::new(window.clone()).expect("create softbuffer context");
        let surface = Surface::new(&context, window.clone()).expect("create softbuffer surface");
        let size = window.inner_size();
        let scale = window.scale_factor();
        let mut browser = Browser::with_fonts(
            size.width as usize,
            size.height as usize,
            scale as f32,
            Fonts::with_system_fonts(),
        );
        browser.set_javascript(self.javascript);
        let mut host = NativeHost {
            window: window.clone(),
            proxy: self.proxy.clone(),
            sockets: Sockets::new(),
            clipboard: arboard::Clipboard::new().ok(),
            cursor: Cursor::Default,
            title: String::new(),
        };
        if let Some(u) = self.url.take() {
            browser.navigate(&url::from_input(&u), &mut host);
        }
        self.state = Some(State {
            window,
            surface,
            browser,
            host,
            mods: Modifiers::default(),
            ctrl_click: false,
            scale,
            pointer: (-1, -1),
        });
    }

    fn user_event(&mut self, _event_loop: &ActiveEventLoop, f: Fetched) {
        if let Some(s) = self.state.as_mut() {
            f.deliver(&mut s.browser, &mut s.host);
            s.window.request_redraw();
        }
    }

    fn new_events(&mut self, _event_loop: &ActiveEventLoop, cause: StartCause) {
        if let (StartCause::ResumeTimeReached { .. }, Some(s)) = (cause, self.state.as_ref()) {
            s.window.request_redraw();
        }
    }

    fn about_to_wait(&mut self, event_loop: &ActiveEventLoop) {
        let now = self.now();
        let Some(s) = self.state.as_ref() else {
            return;
        };
        if s.browser.needs_redraw(now) {
            s.window.request_redraw();
        }
        match s.browser.next_wakeup(now) {
            Some(t) if t > now => {
                let at = Instant::now() + Duration::from_secs_f64((t - now) / 1000.0);
                event_loop.set_control_flow(ControlFlow::WaitUntil(at));
            }
            _ => event_loop.set_control_flow(ControlFlow::Wait),
        }
    }

    fn window_event(&mut self, event_loop: &ActiveEventLoop, _id: WindowId, event: WindowEvent) {
        let now = self.now();
        let Some(s) = self.state.as_mut() else {
            return;
        };
        let input = match event {
            WindowEvent::CloseRequested => {
                event_loop.exit();
                None
            }
            WindowEvent::Resized(size) => {
                s.scale = s.window.scale_factor();
                s.browser
                    .resize(size.width as usize, size.height as usize, s.scale as f32);
                s.window.request_redraw();
                None
            }
            WindowEvent::ScaleFactorChanged { scale_factor, .. } => {
                s.scale = scale_factor;
                None
            }
            WindowEvent::RedrawRequested => {
                s.redraw(now);
                None
            }
            WindowEvent::ModifiersChanged(m) => {
                let st = m.state();
                s.mods = Modifiers {
                    shift: st.shift_key(),
                    ctrl: st.control_key(),
                    alt: st.alt_key(),
                    meta: st.super_key(),
                };
                None
            }
            WindowEvent::CursorMoved { position, .. } => {
                let (x, y) = (
                    (position.x / s.scale).round() as i32,
                    (position.y / s.scale).round() as i32,
                );
                s.pointer = (x, y);
                Some(Event::MouseMove { x, y })
            }
            WindowEvent::CursorLeft { .. } => Some(Event::MouseLeave),
            WindowEvent::MouseInput { state, button, .. } => {
                let button = match button {
                    // Ctrl-click is the secondary click on macOS (the button
                    // stays secondary until it is released).
                    MouseButton::Left if cfg!(target_os = "macos") && (s.mods.ctrl || (state == ElementState::Released && s.ctrl_click)) => {
                        s.ctrl_click = state == ElementState::Pressed;
                        2
                    }
                    MouseButton::Left => 0,
                    MouseButton::Middle => 1,
                    MouseButton::Right => 2,
                    _ => 3,
                };
                let (x, y) = s.pointer;
                Some(match state {
                    ElementState::Pressed => Event::MouseDown { x, y, button },
                    ElementState::Released => Event::MouseUp { x, y, button },
                })
            }
            WindowEvent::MouseWheel { delta, .. } => {
                let (dx, dy) = match delta {
                    MouseScrollDelta::LineDelta(x, y) => ((-x * 40.0) as i32, (-y * 40.0) as i32),
                    MouseScrollDelta::PixelDelta(p) => (
                        (-p.x / s.scale).round() as i32,
                        (-p.y / s.scale).round() as i32,
                    ),
                };
                Some(Event::Wheel { dx, dy })
            }
            WindowEvent::KeyboardInput { event, .. } => {
                let code = shortcut_code(&event.logical_key, &event.physical_key, s.mods);
                if std::env::var_os("RUBROWSER_KEYS").is_some() && event.state == ElementState::Pressed {
                    eprintln!("[keys] {:?} {:?} mods {:?} -> {code}", event.logical_key, event.physical_key, s.mods);
                }
                if event.state == ElementState::Pressed {
                    if code != 0 {
                        let e = Event::KeyDown {
                            key: code,
                            mods: s.mods,
                            repeat: event.repeat,
                        };
                        s.browser.event(e, now, &mut s.host);
                    }
                    event
                        .text
                        .as_ref()
                        .filter(|t| !t.chars().any(char::is_control))
                        .filter(|_| !s.mods.command())
                        .map(|t| Event::Text(t.to_string()))
                } else {
                    (code != 0).then_some(Event::KeyUp {
                        key: code,
                        mods: s.mods,
                    })
                }
            }
            _ => None,
        };
        if let Some(e) = input {
            s.browser.event(e, now, &mut s.host);
        }
        if s.browser.needs_redraw(now) {
            s.window.request_redraw();
        }
    }
}

/// The `abi::key` code of a key press. Shortcuts with a non-Latin layout
/// (Zhuyin, Pinyin, Cyrillic…): Cmd/Ctrl+C gives "ㄏ" or "с" as the
/// character; like other browsers, take the key's place on a US keyboard.
fn shortcut_code(logical: &Key, physical: &winit::keyboard::PhysicalKey, mods: Modifiers) -> i32 {
    let code = key_code(logical);
    if mods.command()
        && !(65..=90).contains(&code)
        && !(48..=57).contains(&code)
        && let Some(c) = physical_code(physical)
    {
        return c;
    }
    code
}

/// The `abi::key` code of a letter, digit or bracket key by its position
/// (US layout), for shortcuts typed with another layout.
fn physical_code(k: &winit::keyboard::PhysicalKey) -> Option<i32> {
    use winit::keyboard::{KeyCode, PhysicalKey};
    let PhysicalKey::Code(c) = k else { return None };
    let letters = [
        KeyCode::KeyA, KeyCode::KeyB, KeyCode::KeyC, KeyCode::KeyD, KeyCode::KeyE, KeyCode::KeyF, KeyCode::KeyG,
        KeyCode::KeyH, KeyCode::KeyI, KeyCode::KeyJ, KeyCode::KeyK, KeyCode::KeyL, KeyCode::KeyM, KeyCode::KeyN,
        KeyCode::KeyO, KeyCode::KeyP, KeyCode::KeyQ, KeyCode::KeyR, KeyCode::KeyS, KeyCode::KeyT, KeyCode::KeyU,
        KeyCode::KeyV, KeyCode::KeyW, KeyCode::KeyX, KeyCode::KeyY, KeyCode::KeyZ,
    ];
    let digits = [
        KeyCode::Digit0, KeyCode::Digit1, KeyCode::Digit2, KeyCode::Digit3, KeyCode::Digit4,
        KeyCode::Digit5, KeyCode::Digit6, KeyCode::Digit7, KeyCode::Digit8, KeyCode::Digit9,
    ];
    if let Some(i) = letters.iter().position(|k| k == c) {
        return Some('A' as i32 + i as i32);
    }
    if let Some(i) = digits.iter().position(|k| k == c) {
        return Some('0' as i32 + i as i32);
    }
    match c {
        KeyCode::BracketLeft => Some(key::LEFT_BRACKET),
        KeyCode::BracketRight => Some(key::RIGHT_BRACKET),
        _ => None,
    }
}

/// Maps a winit key to an `abi::key` code (0 if unmapped).
fn key_code(k: &Key) -> i32 {
    match k {
        Key::Named(n) => match n {
            NamedKey::Backspace => key::BACKSPACE,
            NamedKey::Tab => key::TAB,
            NamedKey::Enter => key::ENTER,
            NamedKey::Escape => key::ESCAPE,
            NamedKey::Space => key::SPACE,
            NamedKey::PageUp => key::PAGE_UP,
            NamedKey::PageDown => key::PAGE_DOWN,
            NamedKey::End => key::END,
            NamedKey::Home => key::HOME,
            NamedKey::ArrowLeft => key::LEFT,
            NamedKey::ArrowUp => key::UP,
            NamedKey::ArrowRight => key::RIGHT,
            NamedKey::ArrowDown => key::DOWN,
            NamedKey::Delete => key::DELETE,
            NamedKey::F5 => key::F5,
            _ => 0,
        },
        Key::Character(s) => {
            let mut chars = s.chars();
            match (chars.next(), chars.next()) {
                (Some('['), None) => key::LEFT_BRACKET,
                (Some(']'), None) => key::RIGHT_BRACKET,
                (Some(' '), None) => key::SPACE,
                (Some(c), None) if c.is_ascii_alphanumeric() => c.to_ascii_uppercase() as i32,
                (Some(c), None) if !c.is_control() => key::CHAR_BASE + c as i32,
                _ => 0,
            }
        }
        _ => 0,
    }
}

impl State {
    fn redraw(&mut self, now: f64) {
        let size = self.window.inner_size();
        let (Some(w), Some(h)) = (NonZeroU32::new(size.width), NonZeroU32::new(size.height)) else {
            return;
        };
        self.surface.resize(w, h).expect("resize surface");
        let canvas = self.browser.render(now, &mut self.host);
        let mut buffer = self.surface.buffer_mut().expect("get surface buffer");
        // The canvas may lag one resize behind the window; copy what overlaps.
        let n = buffer.len().min(canvas.pixels().len());
        buffer[..n].copy_from_slice(&canvas.pixels()[..n]);
        buffer.present().expect("present surface");
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use winit::keyboard::{KeyCode, PhysicalKey};

    #[test]
    fn shortcuts_work_with_any_keyboard_layout() {
        let cmd = Modifiers { meta: true, ..Default::default() };
        let ch = |s: &str| Key::Character(s.into());
        // US layout
        assert_eq!(shortcut_code(&ch("c"), &PhysicalKey::Code(KeyCode::KeyC), cmd), 'C' as i32);
        // Zhuyin: the C key types ㄏ; Cmd+C is still copy
        assert_eq!(shortcut_code(&ch("ㄏ"), &PhysicalKey::Code(KeyCode::KeyC), cmd), 'C' as i32);
        assert_eq!(shortcut_code(&ch("ㄐ"), &PhysicalKey::Code(KeyCode::KeyR), cmd), 'R' as i32);
        assert_eq!(shortcut_code(&ch("」"), &PhysicalKey::Code(KeyCode::BracketRight), cmd), key::RIGHT_BRACKET);
        // without Cmd/Ctrl the character stays what it is
        assert_eq!(shortcut_code(&ch("ㄏ"), &PhysicalKey::Code(KeyCode::KeyC), Modifiers::default()), key::CHAR_BASE + 'ㄏ' as i32);
        // a Dvorak-style layout keeps its own letters (the logical key wins)
        assert_eq!(shortcut_code(&ch("j"), &PhysicalKey::Code(KeyCode::KeyC), cmd), 'J' as i32);
    }
}
