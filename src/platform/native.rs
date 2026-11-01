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
            let result = load(&url);
            let _ = proxy.send_event(Fetched { id, result });
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
}

/// Loads `url` over HTTP(S) or from the file system.
fn load(url: &str) -> Result<Vec<u8>, String> {
    match url::scheme(url) {
        Some("http") | Some("https") => {
            let mut resp = ureq::get(url).call().map_err(|e| e.to_string())?;
            resp.body_mut()
                .with_config()
                .limit(MAX_BODY)
                .read_to_vec()
                .map_err(|e| e.to_string())
        }
        Some("file") => {
            let path = url.trim_start_matches("file://");
            std::fs::read(path).map_err(|e| format!("{path}: {e}"))
        }
        Some(other) => Err(format!("unsupported scheme `{other}:`")),
        None => std::fs::read(url).map_err(|e| format!("{url}: {e}")),
    }
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
        let mut browser = Browser::new(size.width as usize, size.height as usize, scale as f32);
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
