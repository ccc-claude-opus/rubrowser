//! `<img>` support: loading state, decoding and resized copies.

use std::cell::RefCell;
use std::collections::HashMap;
use std::io::Cursor;
use std::rc::Rc;

use resvg::{tiny_skia, usvg};

use crate::css::media::{MediaEnv, MediaList};
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

/// A rendered gradient: (CSS text, width, height).
type GradientKey = (String, u32, u32);

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
    /// Pixel density of the source chosen from `srcset` (natural size is
    /// the bitmap size divided by it).
    pub density: HashMap<NodeId, f32>,
    /// Rendered gradients by (CSS text, width, height).
    gradients: RefCell<HashMap<GradientKey, Rc<Vec<u32>>>>,
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

    /// Pixels of `g` rendered at `w × h`, cached.
    pub fn gradient(
        &self,
        g: &crate::css::gradient::Gradient,
        w: u32,
        h: u32,
        scale: f32,
    ) -> Rc<Vec<u32>> {
        let key = (g.key.clone(), w, h);
        let mut cache = self.gradients.borrow_mut();
        if let Some(px) = cache.get(&key) {
            return px.clone();
        }
        if cache.len() > 256 {
            cache.clear();
        }
        let px = Rc::new(g.render(w, h, scale));
        cache.insert(key, px.clone());
        px
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

/// One `srcset` candidate: URL with a width (`480w`) or density (`2x`).
#[derive(Clone, Debug, PartialEq)]
struct Candidate {
    url: String,
    w: Option<f32>,
    x: f32,
}

/// Parses a `srcset` value (URLs may contain commas; descriptors follow
/// whitespace).
fn parse_srcset(s: &str) -> Vec<Candidate> {
    let mut out = Vec::new();
    let mut rest = s.trim_start_matches(|c: char| c.is_whitespace() || c == ',');
    while !rest.is_empty() {
        let end = rest.find(char::is_whitespace).unwrap_or(rest.len());
        let mut url = &rest[..end];
        rest = &rest[end..];
        let mut descriptors = "";
        if let Some(u) = url.strip_suffix(',') {
            url = u;
        } else {
            let end = rest.find(',').unwrap_or(rest.len());
            descriptors = &rest[..end];
            rest = &rest[end..];
        }
        let mut c = Candidate {
            url: url.to_string(),
            w: None,
            x: 1.0,
        };
        for d in descriptors.split_whitespace() {
            if let Some(w) = d.strip_suffix('w').and_then(|n| n.parse::<f32>().ok()) {
                c.w = Some(w);
            } else if let Some(x) = d.strip_suffix('x').and_then(|n| n.parse::<f32>().ok()) {
                c.x = x;
            }
        }
        if !c.url.is_empty() {
            out.push(c);
        }
        rest = rest.trim_start_matches(|c: char| c.is_whitespace() || c == ',');
    }
    out
}

/// The slot width in CSS px from a `sizes` attribute (default `100vw`).
fn slot_width(sizes: Option<&str>, env: &MediaEnv) -> f32 {
    let fc = crate::css::values::FontContext {
        em: 16.0,
        rem: 16.0,
        vw: env.width,
        vh: env.height,
    };
    for entry in sizes.unwrap_or("").split(',') {
        let entry = entry.trim();
        // `(media) length` or a final `length`.
        let (media, len) = match entry.rfind(|c: char| c == ')' || c.is_whitespace()) {
            Some(i) if entry.starts_with('(') || entry.contains(' ') => {
                (Some(&entry[..=i]), entry[i + 1..].trim())
            }
            _ => (None, entry),
        };
        let matches = media.is_none_or(|m| MediaList::parse(m).matches(env));
        if matches && let Some(px) = crate::css::values::px(len, fc) {
            return px.max(1.0);
        }
    }
    env.width
}

/// Image types this browser decodes.
fn supported_type(t: &str) -> bool {
    matches!(
        t.trim().to_ascii_lowercase().as_str(),
        "" | "image/png"
            | "image/jpeg"
            | "image/jpg"
            | "image/gif"
            | "image/webp"
            | "image/bmp"
            | "image/svg+xml"
    )
}

/// The URL `<img>` element `img` should show and its pixel density:
/// a matching `<picture>` `<source>`, else the best `srcset` candidate,
/// else `src`.
pub fn current_source(dom: &Dom, img: NodeId, env: &MediaEnv) -> (String, f32) {
    let choose = |srcset: &str, sizes: Option<&str>| -> Option<(String, f32)> {
        let cands = parse_srcset(srcset);
        if cands.is_empty() {
            return None;
        }
        let slot = slot_width(sizes, env);
        let density = |c: &Candidate| c.w.map_or(c.x, |w| w / slot);
        let want = env.scale.max(1.0);
        let best = cands
            .iter()
            .filter(|c| density(c) >= want)
            .min_by(|a, b| density(a).total_cmp(&density(b)))
            .or_else(|| {
                cands
                    .iter()
                    .max_by(|a, b| density(a).total_cmp(&density(b)))
            })?;
        Some((best.url.clone(), density(best).max(0.01)))
    };
    if let Some(p) = dom.parent(img).filter(|&p| dom.tag(p) == Some("picture")) {
        for &s in dom.children(p) {
            if s == img {
                break;
            }
            if dom.tag(s) != Some("source") {
                continue;
            }
            let media_ok = dom
                .attr(s, "media")
                .is_none_or(|m| MediaList::parse(m).matches(env));
            if !media_ok || !supported_type(dom.attr(s, "type").unwrap_or("")) {
                continue;
            }
            if let Some(found) = dom
                .attr(s, "srcset")
                .and_then(|set| choose(set, dom.attr(s, "sizes")))
            {
                return found;
            }
        }
    }
    if let Some(found) = dom
        .attr(img, "srcset")
        .and_then(|set| choose(set, dom.attr(img, "sizes")))
    {
        return found;
    }
    (dom.attr(img, "src").unwrap_or("").trim().to_string(), 1.0)
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
pub fn svg_markup(
    dom: &Dom,
    styles: &[Option<crate::css::style::Style>],
    id: NodeId,
    color: Rgba,
) -> String {
    fn write(
        dom: &Dom,
        styles: &[Option<crate::css::style::Style>],
        id: NodeId,
        out: &mut String,
        root: bool,
        color: Rgba,
    ) {
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
        // CSS fill / stroke set on this element (not just inherited from
        // its parent) override presentation attributes.
        let style = styles.get(id).and_then(Option::as_ref);
        let parent_style = dom
            .parent(id)
            .and_then(|p| styles.get(p))
            .and_then(Option::as_ref);
        let own = |pick: fn(&crate::css::style::Style) -> &Option<std::rc::Rc<str>>| {
            let v = style.and_then(|s| pick(s).clone())?;
            let inherited = parent_style.is_some_and(|p| pick(p).as_deref() == Some(&*v));
            (!inherited || root).then_some(v)
        };
        let css = [
            ("fill", own(|s| &s.svg_fill)),
            ("stroke", own(|s| &s.svg_stroke)),
        ];
        out.push('<');
        out.push_str(name);
        for (n, v) in &e.attrs {
            if root && (n == "xmlns" || n == "color") {
                continue;
            }
            if css.iter().any(|(c, v)| c == n && v.is_some()) {
                continue;
            }
            // Presentation attributes may use CSS variables.
            let resolved;
            let v = match style.filter(|_| v.contains("var(")) {
                Some(st) => {
                    resolved = crate::css::substitute_vars(v, &st.vars, 0).unwrap_or_default();
                    &resolved
                }
                None => v,
            };
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
        if let Some(st) = style {
            let hex = |c: Rgba| format!("#{:06x}", c.rgb());
            for (name, v) in &css {
                let Some(v) = v else { continue };
                let v = if v.eq_ignore_ascii_case("currentcolor") {
                    hex(st.color)
                } else {
                    crate::css::values::color(v, st.color).map_or_else(|| v.to_string(), hex)
                };
                out.push_str(&format!(" {name}=\"{}\"", crate::dom::escape(&v, true)));
            }
        }
        out.push('>');
        for &c in dom.children(id) {
            write(dom, styles, c, out, false, color);
        }
        out.push_str("</");
        out.push_str(name);
        out.push('>');
    }
    let mut out = String::new();
    write(dom, styles, id, &mut out, true, color);
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
    fn srcset_selection() {
        let c = parse_srcset("a.jpg 240w, b,c.jpg 480w,d.jpg 960w");
        assert_eq!(c.len(), 3);
        assert_eq!(c[1].url, "b,c.jpg");
        let dom = crate::dom::parse_html(
            r#"<img id=i sizes="(min-width: 768px) 50vw, 100vw" srcset="s.jpg 240w, m.jpg 480w, l.jpg 960w" src=x.jpg>
            <picture><source media="(max-width: 600px)" srcset="small.webp"><source type="image/avif" srcset="a.avif"><img id=p src=fallback.jpg></picture>"#,
        );
        let env = |w: f32| MediaEnv {
            width: w,
            height: 800.0,
            scale: 1.0,
            dark: false,
        };
        let i = dom.element_by_id("i").unwrap();
        assert_eq!(current_source(&dom, i, &env(1280.0)), ("l.jpg".into(), 1.5));
        assert_eq!(current_source(&dom, i, &env(400.0)), ("m.jpg".into(), 1.2));
        let p = dom.element_by_id("p").unwrap();
        assert_eq!(current_source(&dom, p, &env(500.0)).0, "small.webp");
        assert_eq!(current_source(&dom, p, &env(1000.0)).0, "fallback.jpg");
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
