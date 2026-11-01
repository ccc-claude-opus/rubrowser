//! The browser itself: chrome (tabs, toolbar, address bar) and page area.

mod icons;
mod layout;
mod theme;

use crate::gfx::{Canvas, Painter, Rect};
use layout::Layout;
use theme::*;

struct Tab {
    title: String,
    url: String,
}

/// Platform-independent browser state. Platforms feed it the window size and
/// put the rendered canvas on screen.
pub struct Browser {
    canvas: Canvas,
    scale: f32,
    tabs: Vec<Tab>,
    active: usize,
}

impl Browser {
    /// `width` and `height` are physical pixels; `scale` is physical pixels
    /// per logical pixel (the display's DPI factor).
    pub fn new(width: usize, height: usize, scale: f32) -> Self {
        Browser {
            canvas: Canvas::new(width, height),
            scale: sanitize_scale(scale),
            tabs: vec![Tab {
                title: "Home".into(),
                url: "about:home".into(),
            }],
            active: 0,
        }
    }

    pub fn resize(&mut self, width: usize, height: usize, scale: f32) {
        self.canvas.resize(width, height);
        self.scale = sanitize_scale(scale);
    }

    /// Window size in logical pixels.
    fn logical_size(&self) -> (i32, i32) {
        (
            (self.canvas.width() as f32 / self.scale) as i32,
            (self.canvas.height() as f32 / self.scale) as i32,
        )
    }

    fn layout(&self) -> Layout {
        let (w, h) = self.logical_size();
        Layout::new(w, h, self.tabs.len())
    }

    /// Draws the whole browser and returns the result.
    pub fn render(&mut self) -> &Canvas {
        let layout = self.layout();
        let mut p = Painter::new(&mut self.canvas, self.scale);
        p.reset_clip();
        p.canvas().clear(PAGE_BG);
        draw_tab_bar(&mut p, &layout, &self.tabs, self.active);
        draw_toolbar(&mut p, &layout, &self.tabs[self.active].url);
        draw_content(&mut p, layout.content);
        &self.canvas
    }
}

fn sanitize_scale(scale: f32) -> f32 {
    if scale.is_finite() && scale > 0.0 {
        scale
    } else {
        1.0
    }
}

/// Draws `text` clipped to `r`, vertically centered, starting `PAD` from the left.
fn label(p: &mut Painter, r: Rect, text: &str, color: crate::gfx::Color) {
    p.set_clip(r);
    p.text(
        r.x + PAD,
        r.y + (r.h - TEXT_SIZE) / 2,
        TEXT_SIZE,
        text,
        color,
    );
    p.reset_clip();
}

fn draw_tab_bar(p: &mut Painter, l: &Layout, tabs: &[Tab], active: usize) {
    p.fill_rect(l.tab_bar, TAB_BAR_BG);
    for (i, tab) in tabs.iter().enumerate() {
        let r = l.tabs[i];
        p.fill_rect(r, if i == active { TAB_ACTIVE_BG } else { TAB_BG });
        let close = l.tab_close(i);
        let text_area = Rect::new(r.x, r.y, close.x - r.x, r.h);
        label(
            p,
            text_area,
            &tab.title,
            if i == active { TEXT } else { TEXT_DIM },
        );
        icons::close(p, close, TEXT_DIM);
    }
    icons::plus(p, l.new_tab, TEXT_DIM);
}

fn draw_toolbar(p: &mut Painter, l: &Layout, url: &str) {
    p.fill_rect(l.toolbar, TOOLBAR_BG);
    icons::arrow(p, l.back, -1, TEXT_DIM);
    icons::arrow(p, l.forward, 1, TEXT_DIM);
    icons::reload(p, l.reload, TEXT);
    p.fill_rect(l.address, ADDRESS_BG);
    label(p, l.address, url, TEXT);
}

fn draw_content(p: &mut Painter, r: Rect) {
    p.set_clip(r);
    p.text(r.x + 24, r.y + 24, 32, "rubrowser", PAGE_TEXT);
    p.text(
        r.x + 24,
        r.y + 72,
        TEXT_SIZE,
        "Pages are Rust programs compiled to WebAssembly.",
        PAGE_TEXT,
    );
    p.reset_clip();
}
