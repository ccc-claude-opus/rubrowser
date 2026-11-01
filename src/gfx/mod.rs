//! Software rendering into a `u32` pixel buffer.
//!
//! [`Canvas`] works in physical pixels; callers convert from logical
//! pixels with the display scale.

/// A color in `0x00RRGGBB` format.
pub type Color = u32;

/// A rectangle in pixels.
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
    /// Opacity applied to translucent drawing (fills with alpha, glyphs,
    /// images, rounded shapes): 255 is opaque.
    global_alpha: u8,
    /// Rounded rectangle (x, y, w, h, radius) that images are clipped to.
    round_clip: Option<[f32; 5]>,
}

impl Canvas {
    pub fn round_clip(&self) -> Option<[f32; 5]> {
        self.round_clip
    }

    /// Clips images to a rounded rectangle (`None` to stop).
    pub fn set_round_clip(&mut self, r: Option<[f32; 5]>) {
        self.round_clip = r;
    }

    /// Coverage (0–255) of pixel `(px, py)` by the rounded clip.
    fn round_coverage(&self, px: i32, py: i32) -> u8 {
        let Some([x, y, w, h, r]) = self.round_clip else {
            return 255;
        };
        let (cx, cy) = (px as f32 + 0.5, py as f32 + 0.5);
        if cx < x || cy < y || cx > x + w || cy > y + h {
            return 0;
        }
        let r = r.min(w / 2.0).min(h / 2.0);
        let dx = (x + r - cx).max(cx - (x + w - r)).max(0.0);
        let dy = (y + r - cy).max(cy - (y + h - r)).max(0.0);
        if dx == 0.0 || dy == 0.0 {
            return 255;
        }
        let d = (dx * dx + dy * dy).sqrt() - r;
        ((0.5 - d).clamp(0.0, 1.0) * 255.0) as u8
    }
    pub fn new(width: usize, height: usize) -> Self {
        Canvas {
            width,
            height,
            pixels: vec![0; width * height],
            clip: Rect::new(0, 0, width as i32, height as i32),
            global_alpha: 255,
            round_clip: None,
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

    pub fn global_alpha(&self) -> u8 {
        self.global_alpha
    }

    /// Sets the opacity (CSS `opacity`) for subsequent drawing.
    pub fn set_global_alpha(&mut self, alpha: u8) {
        self.global_alpha = alpha;
    }

    /// `alpha` scaled by the global opacity.
    fn effective(&self, alpha: u8) -> u8 {
        ((alpha as u32 * self.global_alpha as u32 + 127) / 255) as u8
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

/// Coverage (0–1) of pixel center `(px, py)` by the rounded rectangle
/// `(x, y, w, h, radius)`, from its signed distance.
fn coverage((x, y, w, h, r): (f32, f32, f32, f32, f32), px: f32, py: f32) -> f32 {
    if w <= 0.0 || h <= 0.0 {
        return 0.0;
    }
    let r = r.min(w / 2.0).min(h / 2.0).max(0.0);
    let (hw, hh) = (w / 2.0, h / 2.0);
    let qx = (px - (x + hw)).abs() - (hw - r);
    let qy = (py - (y + hh)).abs() - (hh - r);
    let outside = (qx.max(0.0).powi(2) + qy.max(0.0).powi(2)).sqrt();
    let d = outside + qx.max(qy).min(0.0) - r;
    (0.5 - d).clamp(0.0, 1.0)
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
        let alpha = self.effective(alpha);
        if alpha == 0 {
            return;
        }
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
                let a = if self.global_alpha == 255 {
                    a
                } else {
                    self.effective(a)
                };
                if a != 0 {
                    let p = &mut self.pixels[line + col as usize];
                    *p = blend(*p, color, a);
                }
            }
        }
    }

    /// Fills a rounded rectangle (physical pixels, fractional allowed) with
    /// anti-aliased corners. With `inner`, only the ring between the two
    /// rounded rectangles is filled (used for rounded borders).
    pub fn fill_rounded(
        &mut self,
        outer: (f32, f32, f32, f32, f32),
        inner: Option<(f32, f32, f32, f32, f32)>,
        color: Color,
        alpha: u8,
    ) {
        let (x, y, w, h, _) = outer;
        let bounds = Rect::new(
            x.floor() as i32,
            y.floor() as i32,
            (x + w).ceil() as i32 - x.floor() as i32,
            (y + h).ceil() as i32 - y.floor() as i32,
        )
        .intersect(&self.clip);
        for row in bounds.y..bounds.bottom() {
            let line = row as usize * self.width;
            for col in bounds.x..bounds.right() {
                let (px, py) = (col as f32 + 0.5, row as f32 + 0.5);
                let mut cov = coverage(outer, px, py);
                if let Some(i) = inner {
                    cov -= coverage(i, px, py);
                }
                if cov > 0.0 {
                    let a = (cov.min(1.0) * self.effective(alpha) as f32).round() as u8;
                    let p = &mut self.pixels[line + col as usize];
                    *p = blend(*p, color, a);
                }
            }
        }
    }

    /// Draws `w × h` pixels in `0xAARRGGBB` at `(x, y)`, blending by their
    /// alpha, clipped.
    pub fn draw_argb(&mut self, x: i32, y: i32, w: usize, h: usize, src: &[u32]) {
        let r = Rect::new(x, y, w as i32, h as i32).intersect(&self.clip);
        if r.w == 0 || r.h == 0 || src.len() < w * h {
            return;
        }
        for row in r.y..r.bottom() {
            let sy = (row - y) as usize;
            let line = row as usize * self.width;
            for col in r.x..r.right() {
                let p = src[sy * w + (col - x) as usize];
                let mut a = self.effective((p >> 24) as u8);
                if self.round_clip.is_some() {
                    a = (a as u32 * self.round_coverage(col, row) as u32 / 255) as u8;
                }
                if a != 0 {
                    let d = &mut self.pixels[line + col as usize];
                    *d = blend(*d, p & 0xff_ffff, a);
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
    fn rounded_corners_are_cut() {
        let mut c = Canvas::new(10, 10);
        c.fill_rounded((0.0, 0.0, 10.0, 10.0, 5.0), None, 0xffffff, 255);
        assert_eq!(c.pixels()[0], 0, "corner is outside");
        assert_eq!(c.pixels()[5 * 10 + 5], 0xffffff, "center is inside");
        let mut c = Canvas::new(10, 10);
        c.fill_rounded(
            (0.0, 0.0, 10.0, 10.0, 0.0),
            Some((2.0, 2.0, 6.0, 6.0, 0.0)),
            0xffffff,
            255,
        );
        assert_eq!(c.pixels()[10 + 1], 0xffffff, "ring");
        assert_eq!(c.pixels()[5 * 10 + 5], 0, "hole");
    }

    #[test]
    fn global_alpha_fades_drawing() {
        let mut c = Canvas::new(1, 1);
        c.set_global_alpha(128);
        c.fill_rect_alpha(0, 0, 1, 1, 0xffffff, 255);
        assert_eq!(c.pixels()[0], 0x808080);
    }

    #[test]
    fn argb_images_blend() {
        let mut c = Canvas::new(3, 1);
        c.clear(0x000000);
        c.draw_argb(0, 0, 3, 1, &[0xff_ff0000, 0x00_00ff00, 0x80_ffffff]);
        assert_eq!(c.pixels(), &[0xff0000, 0, 0x808080]);
    }

    #[test]
    fn blit_scaled_doubles() {
        let mut c = Canvas::new(4, 2);
        c.blit_scaled(Rect::new(0, 0, 4, 2), 2, 1, &[1, 2]);
        assert_eq!(c.pixels(), &[1, 1, 2, 2, 1, 1, 2, 2]);
    }
}
