//! Native window: winit for the window and input, softbuffer to show pixels,
//! ureq and the file system for loading, arboard for the clipboard.

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
use crate::browser::Browser;
use crate::input::{Cursor, Event, Host, Modifiers};
use crate::net::url;
use crate::text::Fonts;

/// Largest response body the browser accepts.
const MAX_BODY: u64 = 32 << 20;

/// Opens the browser window (showing `url` if given) and runs until closed.
pub fn run(url: Option<String>) -> Result<(), Box<dyn Error>> {
    let event_loop = EventLoop::<Fetched>::with_user_event().build()?;
    let proxy = event_loop.create_proxy();
    let mut app = App {
        state: None,
        url,
        proxy,
        start: Instant::now(),
    };
    event_loop.run_app(&mut app)?;
    Ok(())
}

/// A finished fetch, sent from a loader thread to the event loop.
struct Fetched {
    id: u32,
    result: Result<Vec<u8>, String>,
    /// The URL after redirects, if it changed.
    final_url: Option<String>,
}

struct App {
    state: Option<State>,
    url: Option<String>,
    proxy: EventLoopProxy<Fetched>,
    start: Instant,
}

struct State {
    window: Rc<Window>,
    surface: Surface<Rc<Window>, Rc<Window>>,
    browser: Browser,
    host: NativeHost,
    mods: Modifiers,
    scale: f64,
    /// Last pointer position in logical pixels (button events have none).
    pointer: (i32, i32),
}

struct NativeHost {
    window: Rc<Window>,
    proxy: EventLoopProxy<Fetched>,
    clipboard: Option<arboard::Clipboard>,
    cursor: Cursor,
    title: String,
}

impl Host for NativeHost {
    fn fetch(&mut self, id: u32, url: &str) {
        let proxy = self.proxy.clone();
        let url = url.to_string();
        std::thread::spawn(move || {
            let (result, final_url) = match load(&url) {
                Ok((bytes, f)) => (Ok(bytes), f),
                Err(e) => (Err(e), None),
            };
            let _ = proxy.send_event(Fetched {
                id,
                result,
                final_url,
            });
        });
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

/// The HTTP client shared by all loads.
fn agent() -> &'static ureq::Agent {
    static AGENT: std::sync::OnceLock<ureq::Agent> = std::sync::OnceLock::new();
    AGENT.get_or_init(|| {
        ureq::Agent::config_builder()
            .user_agent(concat!(
                "Mozilla/5.0 (compatible; rubrowser/",
                env!("CARGO_PKG_VERSION"),
                "; +https://github.com/ccc-claude-opus/rubrowser)"
            ))
            .timeout_global(Some(Duration::from_secs(30)))
            .http_status_as_error(false)
            .build()
            .into()
    })
}

/// Loads `url` over HTTP(S) or from the file system. Returns the bytes and,
/// if redirects led elsewhere, the final URL.
pub fn load(url: &str) -> Result<(Vec<u8>, Option<String>), String> {
    use ureq::ResponseExt;
    match url::scheme(url) {
        Some("http") | Some("https") => {
            let mut resp = agent().get(url).call().map_err(|e| e.to_string())?;
            let status = resp.status();
            let final_url = resp.get_uri().to_string();
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
            Ok((body, (final_url != url).then_some(final_url)))
        }
        Some("file") => {
            let path = url.trim_start_matches("file://");
            std::fs::read(path)
                .map(|b| (b, None))
                .map_err(|e| format!("{path}: {e}"))
        }
        Some(other) => Err(format!("unsupported scheme `{other}:`")),
        None => std::fs::read(url)
            .map(|b| (b, None))
            .map_err(|e| format!("{url}: {e}")),
    }
}

/// Options for [`screenshot`].
pub struct Shot {
    pub width: u32,
    pub height: u32,
    pub scale: f32,
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
    }
    impl Host for Headless {
        fn fetch(&mut self, id: u32, url: &str) {
            self.pending += 1;
            let tx = self.tx.clone();
            let url = url.to_string();
            std::thread::spawn(move || {
                let (result, final_url) = match load(&url) {
                    Ok((b, f)) => (Ok(b), f),
                    Err(e) => (Err(e), None),
                };
                let _ = tx.send(Fetched {
                    id,
                    result,
                    final_url,
                });
            });
        }
        fn clipboard_get(&mut self) -> Option<String> {
            None
        }
        fn clipboard_set(&mut self, _: &str) {}
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
    let mut host = Headless { tx, pending: 0 };
    let (pw, ph) = (
        (shot.width as f32 * shot.scale) as usize,
        (shot.height as f32 * shot.scale) as usize,
    );
    let mut browser = Browser::with_fonts(pw, ph, shot.scale, Fonts::with_system_fonts());
    let start = Instant::now();
    let now = |s: &Instant| s.elapsed().as_secs_f64() * 1000.0;
    browser.navigate(&url::from_input(url), &mut host);
    let deadline = start + shot.timeout;
    loop {
        browser.render(now(&start), &mut host);
        if host.pending == 0 {
            break;
        }
        let wait = deadline.saturating_duration_since(Instant::now());
        match rx.recv_timeout(wait) {
            Ok(f) => {
                host.pending -= 1;
                if let Some(u) = &f.final_url {
                    browser.fetch_redirected(f.id, u);
                }
                browser.fetch_done(f.id, f.result, &mut host);
            }
            Err(_) => {
                eprintln!(
                    "rubrowser: gave up waiting for {} resource(s)",
                    host.pending
                );
                break;
            }
        }
    }
    browser.render(now(&start), &mut host);
    if let Ok(sel) = std::env::var("RUBROWSER_DUMP") {
        eprint!("{}", browser.dump_layout(&sel, 12));
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
        let mut host = NativeHost {
            window: window.clone(),
            proxy: self.proxy.clone(),
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
            scale,
            pointer: (-1, -1),
        });
    }

    fn user_event(&mut self, _event_loop: &ActiveEventLoop, f: Fetched) {
        if let Some(s) = self.state.as_mut() {
            if let Some(u) = &f.final_url {
                s.browser.fetch_redirected(f.id, u);
            }
            s.browser.fetch_done(f.id, f.result, &mut s.host);
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
                let code = key_code(&event.logical_key);
                if event.state == ElementState::Pressed {
                    if code != 0 {
                        let e = Event::KeyDown {
                            key: code,
                            mods: s.mods,
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
