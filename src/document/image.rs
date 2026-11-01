//! `<img>` support: loading state, decoding and resized copies.

use std::cell::RefCell;
use std::collections::HashMap;
use std::io::Cursor;
use std::rc::Rc;

use crate::dom::NodeId;

/// Largest image (each side) the browser decodes.
const MAX_SIDE: u32 = 8192;

/// A decoded image in `0xAARRGGBB` pixels.
#[derive(Debug, PartialEq)]
pub struct Bitmap {
    pub width: u32,
    pub height: u32,
    pub pixels: Vec<u32>,
}

impl Bitmap {
    /// Decodes PNG, JPEG, GIF (first frame), WebP or BMP.
    pub fn decode(bytes: &[u8]) -> Result<Bitmap, String> {
        let mut limits = image::Limits::default();
        limits.max_image_width = Some(MAX_SIDE);
        limits.max_image_height = Some(MAX_SIDE);
        limits.max_alloc = Some(512 << 20);
        let mut reader = image::ImageReader::new(Cursor::new(bytes))
            .with_guessed_format()
            .map_err(|e| e.to_string())?;
        reader.limits(limits);
        let img = reader.decode().map_err(|e| e.to_string())?.to_rgba8();
        Ok(Self::from_rgba(img))
    }

    fn from_rgba(img: image::RgbaImage) -> Bitmap {
        let (width, height) = img.dimensions();
        let pixels = img
            .pixels()
            .map(|p| {
                let [r, g, b, a] = p.0;
                (a as u32) << 24 | (r as u32) << 16 | (g as u32) << 8 | b as u32
            })
            .collect();
        Bitmap {
            width,
            height,
            pixels,
        }
    }

    /// A copy resized to `w × h` (smooth filtering).
    pub fn resized(&self, w: u32, h: u32) -> Vec<u32> {
        if (w, h) == (self.width, self.height) {
            return self.pixels.clone();
        }
        let rgba: Vec<u8> = self
            .pixels
            .iter()
            .flat_map(|&p| [(p >> 16) as u8, (p >> 8) as u8, p as u8, (p >> 24) as u8])
            .collect();
        let img = image::RgbaImage::from_raw(self.width, self.height, rgba)
            .expect("pixel count matches size");
        let filter = if w < self.width / 2 {
            image::imageops::FilterType::Triangle
        } else {
            image::imageops::FilterType::CatmullRom
        };
        Self::from_rgba(image::imageops::resize(&img, w.max(1), h.max(1), filter)).pixels
    }
}

/// Load state of one `<img>` element.
#[derive(Clone, Debug)]
pub enum ImageState {
    /// Requested `src`, not loaded yet.
    Loading(String),
    Ready(String, Rc<Bitmap>),
    Failed(String),
}

impl ImageState {
    pub fn src(&self) -> &str {
        match self {
            ImageState::Loading(s) | ImageState::Ready(s, _) | ImageState::Failed(s) => s,
        }
    }

    pub fn bitmap(&self) -> Option<&Bitmap> {
        match self {
            ImageState::Ready(_, b) => Some(b),
            _ => None,
        }
    }
}

/// A resized copy of an image: (width, height, pixels).
type Scaled = (u32, u32, Rc<Vec<u32>>);

/// All images of a document, plus resized copies for painting.
#[derive(Default)]
pub struct Images {
    pub states: HashMap<NodeId, ImageState>,
    /// node → (width, height, pixels) at the last painted size.
    scaled: RefCell<HashMap<NodeId, Scaled>>,
}

impl Images {
    pub fn get(&self, node: NodeId) -> Option<&ImageState> {
        self.states.get(&node)
    }

    /// Pixels of image `node` resized to `w × h`, cached.
    pub fn scaled(&self, node: NodeId, w: u32, h: u32) -> Option<Rc<Vec<u32>>> {
        let bitmap = self.states.get(&node)?.bitmap()?;
        let mut cache = self.scaled.borrow_mut();
        if let Some((cw, ch, px)) = cache.get(&node)
            && (*cw, *ch) == (w, h)
        {
            return Some(px.clone());
        }
        let px = Rc::new(bitmap.resized(w, h));
        cache.insert(node, (w, h, px.clone()));
        Some(px)
    }

    pub fn set(&mut self, node: NodeId, state: ImageState) {
        self.scaled.borrow_mut().remove(&node);
        self.states.insert(node, state);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A 2×1 PNG: one opaque red pixel, one half-transparent blue one.
    fn png() -> Vec<u8> {
        let img = image::RgbaImage::from_raw(2, 1, vec![255, 0, 0, 255, 0, 0, 255, 128]).unwrap();
        let mut out = Vec::new();
        image::DynamicImage::ImageRgba8(img)
            .write_to(&mut Cursor::new(&mut out), image::ImageFormat::Png)
            .unwrap();
        out
    }

    #[test]
    fn decodes_and_resizes() {
        let b = Bitmap::decode(&png()).unwrap();
        assert_eq!((b.width, b.height), (2, 1));
        assert_eq!(b.pixels, vec![0xff_ff0000, 0x80_0000ff]);
        assert_eq!(b.resized(4, 2).len(), 8);
        assert!(Bitmap::decode(b"not an image").is_err());
    }
}
