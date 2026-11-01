//! Software rendering into a `u32` pixel buffer.

/// A color in `0x00RRGGBB` format.
pub type Color = u32;

/// A pixel buffer that everything in the browser is drawn into.
pub struct Canvas {
    width: usize,
    height: usize,
    pixels: Vec<Color>,
}

impl Canvas {
    pub fn new(width: usize, height: usize) -> Self {
        Canvas {
            width,
            height,
            pixels: vec![0; width * height],
        }
    }

    pub fn resize(&mut self, width: usize, height: usize) {
        self.width = width;
        self.height = height;
        self.pixels.resize(width * height, 0);
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

    pub fn clear(&mut self, color: Color) {
        self.pixels.fill(color);
    }

    /// Fills a rectangle, clipped to the canvas.
    pub fn fill_rect(&mut self, x: i32, y: i32, w: i32, h: i32, color: Color) {
        let x0 = x.clamp(0, self.width as i32) as usize;
        let y0 = y.clamp(0, self.height as i32) as usize;
        let x1 = x.saturating_add(w).clamp(0, self.width as i32) as usize;
        let y1 = y.saturating_add(h).clamp(0, self.height as i32) as usize;
        if x0 >= x1 {
            return;
        }
        for row in y0..y1 {
            let start = row * self.width;
            self.pixels[start + x0..start + x1].fill(color);
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
    }
}
