//! The browser itself: chrome (toolbar, tabs) and page area.

use crate::gfx::{Canvas, Color};

const BACKGROUND: Color = 0xf0f0f0;
const TOOLBAR: Color = 0x2b2d31;
const TOOLBAR_HEIGHT: i32 = 40;

/// Platform-independent browser state. Platforms feed it the window size and
/// put the rendered canvas on screen.
pub struct Browser {
    canvas: Canvas,
}

impl Browser {
    pub fn new(width: usize, height: usize) -> Self {
        Browser {
            canvas: Canvas::new(width, height),
        }
    }

    pub fn resize(&mut self, width: usize, height: usize) {
        self.canvas.resize(width, height);
    }

    /// Draws the whole browser and returns the result.
    pub fn render(&mut self) -> &Canvas {
        let c = &mut self.canvas;
        c.clear(BACKGROUND);
        c.fill_rect(0, 0, c.width() as i32, TOOLBAR_HEIGHT, TOOLBAR);
        c
    }
}
