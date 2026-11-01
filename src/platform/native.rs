//! Native window: winit for the window and input, softbuffer to show pixels.

use std::error::Error;
use std::num::NonZeroU32;
use std::rc::Rc;

use softbuffer::{Context, Surface};
use winit::application::ApplicationHandler;
use winit::dpi::LogicalSize;
use winit::event::WindowEvent;
use winit::event_loop::{ActiveEventLoop, EventLoop};
use winit::window::{Window, WindowId};

use crate::browser::Browser;

/// Opens the browser window and runs until it is closed.
pub fn run() -> Result<(), Box<dyn Error>> {
    let event_loop = EventLoop::new()?;
    let mut app = App { state: None };
    event_loop.run_app(&mut app)?;
    Ok(())
}

struct App {
    state: Option<State>,
}

struct State {
    window: Rc<Window>,
    surface: Surface<Rc<Window>, Rc<Window>>,
    browser: Browser,
}

impl ApplicationHandler for App {
    fn resumed(&mut self, event_loop: &ActiveEventLoop) {
        if self.state.is_some() {
            return;
        }
        let attrs = Window::default_attributes()
            .with_title("rubrowser")
            .with_inner_size(LogicalSize::new(1024.0, 768.0));
        let window = Rc::new(event_loop.create_window(attrs).expect("create window"));
        let context = Context::new(window.clone()).expect("create softbuffer context");
        let surface = Surface::new(&context, window.clone()).expect("create softbuffer surface");
        let size = window.inner_size();
        let browser = Browser::new(size.width as usize, size.height as usize);
        self.state = Some(State {
            window,
            surface,
            browser,
        });
    }

    fn window_event(&mut self, event_loop: &ActiveEventLoop, _id: WindowId, event: WindowEvent) {
        let Some(state) = self.state.as_mut() else {
            return;
        };
        match event {
            WindowEvent::CloseRequested => event_loop.exit(),
            WindowEvent::Resized(size) => {
                state.browser.resize(size.width as usize, size.height as usize);
                state.window.request_redraw();
            }
            WindowEvent::RedrawRequested => state.redraw(),
            _ => {}
        }
    }
}

impl State {
    fn redraw(&mut self) {
        let size = self.window.inner_size();
        let (Some(w), Some(h)) = (NonZeroU32::new(size.width), NonZeroU32::new(size.height)) else {
            return;
        };
        self.surface.resize(w, h).expect("resize surface");
        let canvas = self.browser.render();
        let mut buffer = self.surface.buffer_mut().expect("get surface buffer");
        // The canvas may lag one resize behind the window; copy what overlaps.
        let n = buffer.len().min(canvas.pixels().len());
        buffer[..n].copy_from_slice(&canvas.pixels()[..n]);
        buffer.present().expect("present surface");
    }
}
