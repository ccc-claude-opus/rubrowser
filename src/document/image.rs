//! `<img>` support: loading state, decoding and resized copies.

use std::cell::RefCell;
use std::collections::HashMap;
use std::io::Cursor;
use std::rc::Rc;

use resvg::{tiny_skia, usvg};

use crate::css::values::Rgba;
use crate::dom::{Dom, NodeId};

/// Largest image (each side) the browser decodes.
const MAX_SIDE: u32 = 8192;

/// A decoded image in `0xAARRGGBB` pixels, or an SVG drawing that is
/// rasterized at whatever size it is painted.
#[derive(Debug, PartialEq)]
pub struct Bitmap {
    /// Natural size in CSS pixels.
    pub width: u32,
    pub height: u32,
    /// Empty for SVG.
    pub pixels: Vec<u32>,
    pub svg: Option<Svg>,
}

/// A parsed SVG document.
#[derive(Clone)]
pub struct Svg(pub Rc<usvg::Tree>);

impl PartialEq for Svg {
    fn eq(&self, other: &Svg) -> bool {
        Rc::ptr_eq(&self.0, &other.0)
    }
}

impl std::fmt::Debug for Svg {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let s = self.0.size();
        write!(f, "Svg({}x{})", s.width(), s.height())
    }
}

/// Whether `bytes` look like an SVG document.
fn is_svg(bytes: &[u8]) -> bool {
    let head = &bytes[..bytes.len().min(1024)];
    let head = String::from_utf8_lossy(head);
    let t = head.trim_start_matches('\u{feff}').trim_start();
    (t.starts_with('<') && head.contains("<svg")) || t.starts_with("<svg")
}

impl Bitmap {
    /// Parses an SVG document (text is not rendered: no fonts are loaded).
    pub fn svg(bytes: &[u8]) -> Result<Bitmap, String> {
        let tree =
            usvg::Tree::from_data(bytes, &usvg::Options::default()).map_err(|e| e.to_string())?;
        let size = tree.size();
        Ok(Bitmap {
            width: size.width().ceil() as u32,
            height: size.height().ceil() as u32,
            pixels: Vec::new(),
            svg: Some(Svg(Rc::new(tree))),
        })
    }

    /// Rasterizes an SVG at `w × h`.
    fn render_svg(svg: &Svg, w: u32, h: u32) -> Vec<u32> {
        let Some(mut pm) = tiny_skia::Pixmap::new(w.max(1), h.max(1)) else {
            return Vec::new();
        };
        let size = svg.0.size();
        let ts = tiny_skia::Transform::from_scale(
            w as f32 / size.width().max(1e-3),
            h as f32 / size.height().max(1e-3),
        );
        resvg::render(&svg.0, ts, &mut pm.as_mut());
        pm.pixels()
            .iter()
            .map(|p| {
                let c = p.demultiply();
                (c.alpha() as u32) << 24
                    | (c.red() as u32) << 16
                    | (c.green() as u32) << 8
                    | c.blue() as u32
            })
            .collect()
    }

    /// Decodes PNG, JPEG, GIF (first frame), WebP, BMP or SVG.
    pub fn decode(bytes: &[u8]) -> Result<Bitmap, String> {
        if is_svg(bytes) {
            return Self::svg(bytes);
        }
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
            svg: None,
        }
    }

    /// A copy resized to `w × h` (smooth filtering).
    pub fn resized(&self, w: u32, h: u32) -> Vec<u32> {
        if let Some(svg) = &self.svg {
            return Self::render_svg(svg, w, h);
        }
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
    /// CSS `background-image` URLs (as written in the stylesheet).
    pub urls: HashMap<String, ImageState>,
    scaled_urls: RefCell<HashMap<String, Scaled>>,
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

    /// Pixels of background image `url` resized to `w × h`, cached.
    pub fn scaled_url(&self, url: &str, w: u32, h: u32) -> Option<Rc<Vec<u32>>> {
        let bitmap = self.urls.get(url)?.bitmap()?;
        let mut cache = self.scaled_urls.borrow_mut();
        if let Some((cw, ch, px)) = cache.get(url)
            && (*cw, *ch) == (w, h)
        {
            return Some(px.clone());
        }
        let px = Rc::new(bitmap.resized(w, h));
        cache.insert(url.to_string(), (w, h, px.clone()));
        Some(px)
    }

    pub fn set_url(&mut self, url: &str, state: ImageState) {
        self.scaled_urls.borrow_mut().remove(url);
        self.urls.insert(url.to_string(), state);
    }

    pub fn set(&mut self, node: NodeId, state: ImageState) {
        self.scaled.borrow_mut().remove(&node);
        self.states.insert(node, state);
    }
}

/// SVG element names that HTML parsing lowercased.
const SVG_NAMES: &[&str] = &[
    "altGlyph",
    "animateMotion",
    "animateTransform",
    "clipPath",
    "feBlend",
    "feColorMatrix",
    "feComponentTransfer",
    "feComposite",
    "feConvolveMatrix",
    "feDiffuseLighting",
    "feDisplacementMap",
    "feDistantLight",
    "feDropShadow",
    "feFlood",
    "feFuncA",
    "feFuncB",
    "feFuncG",
    "feFuncR",
    "feGaussianBlur",
    "feImage",
    "feMerge",
    "feMergeNode",
    "feMorphology",
    "feOffset",
    "fePointLight",
    "feSpecularLighting",
    "feSpotLight",
    "feTile",
    "feTurbulence",
    "foreignObject",
    "linearGradient",
    "radialGradient",
    "textPath",
];

/// Serializes inline `<svg>` element `id` as a standalone SVG document,
/// with `currentColor` resolved to `color`.
pub fn svg_markup(dom: &Dom, id: NodeId, color: Rgba) -> String {
    fn write(dom: &Dom, id: NodeId, out: &mut String, root: bool, color: Rgba) {
        if let Some(t) = dom.text(id) {
            out.push_str(&crate::dom::escape(t, false));
            return;
        }
        let Some(e) = dom.element(id) else { return };
        let name = SVG_NAMES
            .iter()
            .find(|n| n.eq_ignore_ascii_case(&e.name))
            .copied()
            .unwrap_or(&e.name);
        out.push('<');
        out.push_str(name);
        for (n, v) in &e.attrs {
            if root && (n == "xmlns" || n == "color") {
                continue;
            }
            out.push(' ');
            out.push_str(if n == "href" { "xlink:href" } else { n });
            out.push_str("=\"");
            out.push_str(&crate::dom::escape(v, true));
            out.push('"');
        }
        if root {
            out.push_str(&format!(
                " xmlns=\"http://www.w3.org/2000/svg\" xmlns:xlink=\"http://www.w3.org/1999/xlink\" color=\"#{:06x}\"",
                color.rgb()
            ));
        }
        out.push('>');
        for &c in dom.children(id) {
            write(dom, c, out, false, color);
        }
        out.push_str("</");
        out.push_str(name);
        out.push('>');
    }
    let mut out = String::new();
    write(dom, id, &mut out, true, color);
    out
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

    #[test]
    fn svg_renders_at_any_size() {
        let svg = br#"<?xml version="1.0"?><svg xmlns="http://www.w3.org/2000/svg" width="20" height="10"><rect width="10" height="10" fill="red"/></svg>"#;
        let b = Bitmap::decode(svg).unwrap();
        assert_eq!((b.width, b.height), (20, 10));
        let px = b.resized(40, 20);
        assert_eq!(px.len(), 800);
        assert_eq!(px[0], 0xff_ff0000);
        assert_eq!(px[39] >> 24, 0, "right half is transparent");
    }
}
