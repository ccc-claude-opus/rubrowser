//! Fonts and text: measuring and drawing with fontdue and DejaVu.
//!
//! Sizes passed in are logical pixels; drawing rasterizes at the physical
//! size (`size × scale`) so text stays sharp on HiDPI displays.

use std::cell::{OnceCell, RefCell};
use std::collections::HashMap;

use fontdue::{Font, FontSettings};

use crate::gfx::{Canvas, Color};

/// Generic font families available to CSS.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub enum Family {
    #[default]
    Sans,
    Mono,
}

/// Selects one of the bundled font faces.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub struct FontKey {
    pub family: Family,
    pub bold: bool,
    pub italic: bool,
}

impl FontKey {
    fn index(&self) -> usize {
        (self.family as usize) * 4 + (self.bold as usize) * 2 + self.italic as usize
    }

    fn data(&self) -> &'static [u8] {
        use dejavu::{sans, sans_mono};
        match (self.family, self.bold, self.italic) {
            (Family::Sans, false, false) => sans::regular(),
            (Family::Sans, true, false) => sans::bold(),
            (Family::Sans, false, true) => sans::oblique(),
            (Family::Sans, true, true) => sans::bold_oblique(),
            (Family::Mono, false, false) => sans_mono::regular(),
            (Family::Mono, true, false) => sans_mono::bold(),
            (Family::Mono, false, true) => sans_mono::oblique(),
            (Family::Mono, true, true) => sans_mono::bold_oblique(),
        }
    }
}

/// Vertical metrics of a font at a size, in the same units as the size.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct LineMetrics {
    pub ascent: f32,
    pub descent: f32,
}

struct Glyph {
    xmin: i32,
    ymin: i32,
    width: usize,
    height: usize,
    coverage: Vec<u8>,
}

/// The bundled fonts, parsed on first use, plus a glyph bitmap cache.
#[derive(Default)]
pub struct Fonts {
    faces: [OnceCell<Font>; 8],
    /// (face, char, physical size in 1/4 px) → rasterized glyph.
    glyphs: RefCell<HashMap<(usize, char, u32), Glyph>>,
}

const MAX_CACHED_GLYPHS: usize = 8192;

impl Fonts {
    pub fn new() -> Self {
        Self::default()
    }

    fn face(&self, key: FontKey) -> &Font {
        self.faces[key.index()].get_or_init(|| {
            Font::from_bytes(key.data(), FontSettings::default()).expect("bundled font is valid")
        })
    }

    pub fn line_metrics(&self, key: FontKey, size: f32) -> LineMetrics {
        match self.face(key).horizontal_line_metrics(size) {
            Some(m) => LineMetrics {
                ascent: m.ascent,
                descent: -m.descent,
            },
            None => LineMetrics {
                ascent: size * 0.8,
                descent: size * 0.2,
            },
        }
    }

    /// Advance of each char of `text` at `size`, including kerning.
    pub fn advances(&self, key: FontKey, size: f32, text: &str) -> Vec<f32> {
        let font = self.face(key);
        let mut out = Vec::with_capacity(text.len());
        let mut prev: Option<char> = None;
        for c in text.chars() {
            let kern = prev
                .and_then(|p| font.horizontal_kern(p, c, size))
                .unwrap_or(0.0);
            if let Some(last) = out.last_mut() {
                *last += kern;
            }
            out.push(font.metrics(c, size).advance_width);
            prev = Some(c);
        }
        out
    }

    /// Width of `text` at `size`.
    pub fn measure(&self, key: FontKey, size: f32, text: &str) -> f32 {
        self.advances(key, size, text).iter().sum()
    }

    /// Draws `text` with its baseline starting at physical `(x, baseline)`;
    /// `size` is in physical pixels. Returns the advance in physical pixels.
    pub fn draw(
        &self,
        canvas: &mut Canvas,
        key: FontKey,
        size: f32,
        x: f32,
        baseline: f32,
        text: &str,
        color: Color,
    ) -> f32 {
        let face = key.index();
        let size_q = (size * 4.0).round() as u32;
        let advances = self.advances(key, size, text);
        let mut pen = x;
        let mut cache = self.glyphs.borrow_mut();
        if cache.len() > MAX_CACHED_GLYPHS {
            cache.clear();
        }
        for (c, adv) in text.chars().zip(advances) {
            if !c.is_whitespace() {
                let g = cache.entry((face, c, size_q)).or_insert_with(|| {
                    let (m, coverage) = self.face(key).rasterize(c, size);
                    Glyph {
                        xmin: m.xmin,
                        ymin: m.ymin,
                        width: m.width,
                        height: m.height,
                        coverage,
                    }
                });
                let gx = pen.round() as i32 + g.xmin;
                let gy = baseline.round() as i32 - g.ymin - g.height as i32;
                canvas.draw_mask(gx, gy, g.width, g.height, &g.coverage, color);
            }
            pen += adv;
        }
        pen - x
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const SANS: FontKey = FontKey {
        family: Family::Sans,
        bold: false,
        italic: false,
    };

    #[test]
    fn measuring_scales_with_size_and_is_proportional() {
        let f = Fonts::new();
        let w16 = f.measure(SANS, 16.0, "Hello");
        let w32 = f.measure(SANS, 32.0, "Hello");
        assert!((w32 / w16 - 2.0).abs() < 0.05, "{w16} {w32}");
        assert!(f.measure(SANS, 16.0, "iii") < f.measure(SANS, 16.0, "WWW"));
        let mono = FontKey {
            family: Family::Mono,
            ..SANS
        };
        assert_eq!(f.measure(mono, 16.0, "iii"), f.measure(mono, 16.0, "WWW"));
    }

    #[test]
    fn bold_is_wider() {
        let f = Fonts::new();
        let bold = FontKey { bold: true, ..SANS };
        assert!(f.measure(bold, 16.0, "Hello") > f.measure(SANS, 16.0, "Hello"));
    }

    #[test]
    fn drawing_puts_ink_above_baseline() {
        let f = Fonts::new();
        let mut c = Canvas::new(40, 30);
        let adv = f.draw(&mut c, SANS, 20.0, 2.0, 20.0, "H", 0xffffff);
        assert!(adv > 5.0);
        let ink = |row: usize| c.pixels()[row * 40..row * 40 + 40].iter().any(|&p| p != 0);
        assert!(ink(10));
        assert!(!ink(25));
        let m = f.line_metrics(SANS, 20.0);
        assert!(m.ascent > 10.0 && m.descent > 0.0);
    }
}
