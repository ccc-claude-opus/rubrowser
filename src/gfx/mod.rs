//! Software rendering into a `u32` pixel buffer.
//!
//! [`Canvas`] works in physical pixels. [`Painter`] wraps a canvas with a
//! scale factor and an origin, so callers can draw in logical pixels.

pub mod font;

/// A color in `0x00RRGGBB` format.
pub type Color = u32;

/// A rectangle; physical pixels on a [`Canvas`], logical ones on a [`Painter`].
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub struct Rect {
    pub x: i32,
    pub y: i32,
    pub w: i32,
    pub h: i32,
}

impl Rect {
    pub const fn new(x: i32, y: i32, w: i32, h: i32) -> Self {
        Rect { x, y, w, h }
    }

    pub fn right(&self) -> i32 {
        self.x + self.w
    }

    pub fn bottom(&self) -> i32 {
        self.y + self.h
    }

    pub fn contains(&self, x: i32, y: i32) -> bool {
        x >= self.x && x < self.right() && y >= self.y && y < self.bottom()
    }

    pub fn intersect(&self, o: &Rect) -> Rect {
        let x0 = self.x.max(o.x);
        let y0 = self.y.max(o.y);
        let x1 = self.right().min(o.right());
        let y1 = self.bottom().min(o.bottom());
        Rect::new(x0, y0, (x1 - x0).max(0), (y1 - y0).max(0))
    }

    /// Shrinks the rectangle by `d` on every side.
    pub fn inset(&self, d: i32) -> Rect {
        Rect::new(
            self.x + d,
            self.y + d,
            (self.w - 2 * d).max(0),
            (self.h - 2 * d).max(0),
        )
    }
}

/// A pixel buffer that everything in the browser is drawn into.
pub struct Canvas {
    width: usize,
    height: usize,
    pixels: Vec<Color>,
    clip: Rect,
}

impl Canvas {
    pub fn new(width: usize, height: usize) -> Self {
        Canvas {
            width,
            height,
            pixels: vec![0; width * height],
            clip: Rect::new(0, 0, width as i32, height as i32),
        }
    }

    pub fn resize(&mut self, width: usize, height: usize) {
        self.width = width;
        self.height = height;
        self.pixels.resize(width * height, 0);
        self.reset_clip();
    }

    pub fn width(&self) -> usize {
        self.width
    }

    pub fn height(&self) -> usize {
        self.height
    }

    pub fn pixels(&self) -> &[Color] {
        &self.pixels
    }

    pub fn bounds(&self) -> Rect {
        Rect::new(0, 0, self.width as i32, self.height as i32)
    }

    pub fn clip(&self) -> Rect {
        self.clip
    }

    /// Restricts drawing to `r` (intersected with the canvas).
    pub fn set_clip(&mut self, r: Rect) {
        self.clip = r.intersect(&self.bounds());
    }

    pub fn reset_clip(&mut self) {
        self.clip = self.bounds();
    }

    pub fn clear(&mut self, color: Color) {
        self.pixels.fill(color);
    }

    /// Fills a rectangle, clipped to the clip rectangle.
    pub fn fill_rect(&mut self, x: i32, y: i32, w: i32, h: i32, color: Color) {
        let r = Rect::new(x, y, w.max(0), h.max(0)).intersect(&self.clip);
        if r.w == 0 || r.h == 0 {
            return;
        }
        for row in r.y..r.bottom() {
            let start = row as usize * self.width;
            self.pixels[start + r.x as usize..start + r.right() as usize].fill(color);
        }
    }

    pub fn set_pixel(&mut self, x: i32, y: i32, color: Color) {
        if self.clip.contains(x, y) {
            self.pixels[y as usize * self.width + x as usize] = color;
        }
    }

    /// Draws a line `thick` pixels wide (Bresenham, with square pen).
    pub fn line(&mut self, x0: i32, y0: i32, x1: i32, y1: i32, thick: i32, color: Color) {
        let (mut x, mut y) = (x0, y0);
        let dx = (x1 - x0).abs();
        let dy = -(y1 - y0).abs();
        let sx = if x0 < x1 { 1 } else { -1 };
        let sy = if y0 < y1 { 1 } else { -1 };
        let mut err = dx + dy;
        let t = thick.max(1);
        loop {
            self.fill_rect(x - (t - 1) / 2, y - (t - 1) / 2, t, t, color);
            if x == x1 && y == y1 {
                break;
            }
            let e2 = 2 * err;
            if e2 >= dy {
                err += dy;
                x += sx;
            }
            if e2 <= dx {
                err += dx;
                y += sy;
            }
        }
    }

    /// Draws one glyph with its top-left corner at `(x, y)`, each font pixel
    /// becoming a `k × k` block.
    pub fn draw_char(&mut self, x: i32, y: i32, ch: char, k: i32, color: Color) {
        let cell = font::CELL * k;
        let visible = Rect::new(x, y, cell, cell).intersect(&self.clip);
        if visible.w == 0 || visible.h == 0 {
            return;
        }
        for (row, bits) in font::glyph(ch).iter().enumerate() {
            for col in 0..8 {
                if bits & (1 << col) != 0 {
                    self.fill_rect(x + col * k, y + row as i32 * k, k, k, color);
                }
            }
        }
    }

    /// Copies `src` (`w × h` pixels) to `(x, y)`, clipped.
    pub fn blit(&mut self, x: i32, y: i32, w: usize, h: usize, src: &[Color]) {
        let r = Rect::new(x, y, w as i32, h as i32).intersect(&self.clip);
        if r.w == 0 || r.h == 0 || src.len() < w * h {
            return;
        }
        for row in r.y..r.bottom() {
            let sy = (row - y) as usize;
            let sx0 = (r.x - x) as usize;
            let src_row = &src[sy * w + sx0..sy * w + sx0 + r.w as usize];
            let start = row as usize * self.width + r.x as usize;
            self.pixels[start..start + r.w as usize].copy_from_slice(src_row);
        }
    }
}

/// Mixes `fg` over `bg` with coverage `alpha` (0–255).
pub fn blend(bg: Color, fg: Color, alpha: u8) -> Color {
    match alpha {
        0 => bg,
        255 => fg,
        a => {
            let a = a as u32;
            let mix = |shift: u32| {
                let b = (bg >> shift) & 0xff;
                let f = (fg >> shift) & 0xff;
                ((f * a + b * (255 - a) + 127) / 255) << shift
            };
            mix(16) | mix(8) | mix(0)
        }
    }
}

impl Canvas {
    /// Fills a rectangle with `color` at opacity `alpha`, clipped.
    pub fn fill_rect_alpha(&mut self, x: i32, y: i32, w: i32, h: i32, color: Color, alpha: u8) {
        if alpha == 255 {
            return self.fill_rect(x, y, w, h, color);
        }
        let r = Rect::new(x, y, w.max(0), h.max(0)).intersect(&self.clip);
        for row in r.y..r.bottom() {
            let start = row as usize * self.width;
            for p in &mut self.pixels[start + r.x as usize..start + r.right() as usize] {
                *p = blend(*p, color, alpha);
            }
        }
    }

    /// Draws `color` through a `w × h` coverage mask at `(x, y)`, clipped.
    pub fn draw_mask(&mut self, x: i32, y: i32, w: usize, h: usize, mask: &[u8], color: Color) {
        let r = Rect::new(x, y, w as i32, h as i32).intersect(&self.clip);
        if r.w == 0 || r.h == 0 || mask.len() < w * h {
            return;
        }
        for row in r.y..r.bottom() {
            let my = (row - y) as usize;
            let line = row as usize * self.width;
            for col in r.x..r.right() {
                let a = mask[my * w + (col - x) as usize];
                if a != 0 {
                    let p = &mut self.pixels[line + col as usize];
                    *p = blend(*p, color, a);
                }
            }
        }
    }

    /// Copies `src` (`w × h` pixels) stretched to fill `dst` (nearest
    /// neighbor), clipped.
    pub fn blit_scaled(&mut self, dst: Rect, w: usize, h: usize, src: &[Color]) {
        if dst.w <= 0 || dst.h <= 0 || w == 0 || h == 0 || src.len() < w * h {
            return;
        }
        let r = dst.intersect(&self.clip);
        for row in r.y..r.bottom() {
            let sy = ((row - dst.y) as usize * h) / dst.h as usize;
            let line = row as usize * self.width;
            for col in r.x..r.right() {
                let sx = ((col - dst.x) as usize * w) / dst.w as usize;
                self.pixels[line + col as usize] = src[sy * w + sx];
            }
        }
    }
}

/// Draws on a [`Canvas`] in logical pixels, relative to an origin.
pub struct Painter<'a> {
    canvas: &'a mut Canvas,
    scale: f32,
    origin: (i32, i32),
}

impl<'a> Painter<'a> {
    pub fn new(canvas: &'a mut Canvas, scale: f32) -> Self {
        Painter {
            canvas,
            scale,
            origin: (0, 0),
        }
    }

    pub fn scale(&self) -> f32 {
        self.scale
    }

    pub fn canvas(&mut self) -> &mut Canvas {
        self.canvas
    }

    /// Converts a logical coordinate (relative to the origin) to physical.
    fn px(&self, v: i32) -> i32 {
        (v as f32 * self.scale).round() as i32
    }

    /// Converts a logical rectangle to physical pixels.
    pub fn to_physical(&self, r: Rect) -> Rect {
        let x0 = self.px(self.origin.0 + r.x);
        let y0 = self.px(self.origin.1 + r.y);
        let x1 = self.px(self.origin.0 + r.right());
        let y1 = self.px(self.origin.1 + r.bottom());
        Rect::new(x0, y0, x1 - x0, y1 - y0)
    }

    /// Moves the origin by `(dx, dy)` logical pixels.
    pub fn translate(&mut self, dx: i32, dy: i32) {
        self.origin.0 += dx;
        self.origin.1 += dy;
    }

    pub fn origin(&self) -> (i32, i32) {
        self.origin
    }

    pub fn set_origin(&mut self, origin: (i32, i32)) {
        self.origin = origin;
    }

    /// Restricts drawing to the logical rectangle `r`.
    pub fn set_clip(&mut self, r: Rect) {
        let p = self.to_physical(r);
        self.canvas.set_clip(p);
    }

    pub fn reset_clip(&mut self) {
        self.canvas.reset_clip();
    }

    pub fn fill_rect(&mut self, r: Rect, color: Color) {
        let p = self.to_physical(r);
        self.canvas.fill_rect(p.x, p.y, p.w, p.h, color);
    }

    /// Draws a rectangle outline one logical pixel wide.
    pub fn stroke_rect(&mut self, r: Rect, color: Color) {
        self.fill_rect(Rect::new(r.x, r.y, r.w, 1), color);
        self.fill_rect(Rect::new(r.x, r.bottom() - 1, r.w, 1), color);
        self.fill_rect(Rect::new(r.x, r.y, 1, r.h), color);
        self.fill_rect(Rect::new(r.right() - 1, r.y, 1, r.h), color);
    }

    /// Draws a line `thick` logical pixels wide.
    pub fn line(&mut self, x0: i32, y0: i32, x1: i32, y1: i32, thick: i32, color: Color) {
        let (ox, oy) = self.origin;
        let t = self.px(thick).max(1);
        let (a, b) = (self.px(ox + x0), self.px(oy + y0));
        let (c, d) = (self.px(ox + x1), self.px(oy + y1));
        self.canvas.line(a, b, c, d, t, color);
    }

    /// Draws `text` with its top-left corner at `(x, y)`, `size` logical
    /// pixels tall. Returns the logical width drawn.
    pub fn text(&mut self, x: i32, y: i32, size: i32, text: &str, color: Color) -> i32 {
        let k = font::pixel_scale(size as f32, self.scale);
        let cell = font::CELL * k;
        let mut px = self.px(self.origin.0 + x);
        // Center the glyph cell vertically in the requested height.
        let py = self.px(self.origin.1 + y) + (self.px(size) - cell) / 2;
        for ch in text.chars() {
            self.canvas.draw_char(px, py, ch, k, color);
            px += cell;
        }
        self.text_width(size, text)
    }

    /// Logical width of `text` drawn at `size`.
    pub fn text_width(&self, size: i32, text: &str) -> i32 {
        text_width(size, self.scale, text)
    }

    /// Logical width of one character drawn at `size`.
    pub fn char_width(&self, size: i32) -> f32 {
        char_width(size, self.scale)
    }
}

/// Logical width of one character drawn at `size` with display `scale`.
pub fn char_width(size: i32, scale: f32) -> f32 {
    (font::CELL * font::pixel_scale(size as f32, scale)) as f32 / scale
}

/// Logical width of `text` drawn at `size` with display `scale`.
pub fn text_width(size: i32, scale: f32, text: &str) -> i32 {
    (text.chars().count() as f32 * char_width(size, scale)).round() as i32
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fill_rect_is_clipped() {
        let mut c = Canvas::new(4, 3);
        c.fill_rect(-2, 1, 4, 10, 7);
        assert_eq!(c.pixels(), &[0, 0, 0, 0, 7, 7, 0, 0, 7, 7, 0, 0]);
    }

    #[test]
    fn fill_rect_outside_does_nothing() {
        let mut c = Canvas::new(2, 2);
        c.fill_rect(5, 5, 3, 3, 1);
        c.fill_rect(0, 0, -1, 2, 1);
        assert!(c.pixels().iter().all(|&p| p == 0));
    }

    #[test]
    fn resize_keeps_length_consistent() {
        let mut c = Canvas::new(2, 2);
        c.resize(3, 5);
        assert_eq!(c.pixels().len(), 15);
        assert_eq!(c.clip(), Rect::new(0, 0, 3, 5));
    }

    #[test]
    fn clip_limits_drawing() {
        let mut c = Canvas::new(4, 1);
        c.set_clip(Rect::new(1, 0, 2, 1));
        c.fill_rect(0, 0, 4, 1, 9);
        assert_eq!(c.pixels(), &[0, 9, 9, 0]);
    }

    #[test]
    fn line_hits_both_ends() {
        let mut c = Canvas::new(5, 5);
        c.line(0, 0, 4, 4, 1, 1);
        for i in 0..5 {
            assert_eq!(c.pixels()[i * 5 + i], 1);
        }
    }

    #[test]
    fn blit_is_clipped() {
        let mut c = Canvas::new(3, 2);
        c.blit(1, 1, 3, 1, &[1, 2, 3]);
        assert_eq!(c.pixels(), &[0, 0, 0, 0, 1, 2]);
    }

    #[test]
    fn blending() {
        assert_eq!(blend(0x000000, 0xffffff, 255), 0xffffff);
        assert_eq!(blend(0x000000, 0xffffff, 0), 0);
        assert_eq!(blend(0x000000, 0xff0000, 128), 0x800000);
        let mut c = Canvas::new(2, 1);
        c.draw_mask(0, 0, 2, 1, &[255, 0], 0x123456);
        assert_eq!(c.pixels(), &[0x123456, 0]);
    }

    #[test]
    fn blit_scaled_doubles() {
        let mut c = Canvas::new(4, 2);
        c.blit_scaled(Rect::new(0, 0, 4, 2), 2, 1, &[1, 2]);
        assert_eq!(c.pixels(), &[1, 1, 2, 2, 1, 1, 2, 2]);
    }

    #[test]
    fn painter_scales_rects() {
        let mut c = Canvas::new(4, 4);
        let mut p = Painter::new(&mut c, 2.0);
        p.translate(1, 0);
        p.fill_rect(Rect::new(0, 1, 1, 1), 5);
        assert_eq!(&c.pixels()[8..12], &[0, 0, 5, 5]);
        assert_eq!(&c.pixels()[12..16], &[0, 0, 5, 5]);
    }

    #[test]
    fn text_draws_pixels_and_measures() {
        let mut c = Canvas::new(32, 16);
        let mut p = Painter::new(&mut c, 1.0);
        let w = p.text(0, 0, 16, "A", 1);
        assert_eq!(w, 16);
        assert!(c.pixels().contains(&1));
        assert_eq!(text_width(16, 2.0, "abc"), 48);
    }
}
