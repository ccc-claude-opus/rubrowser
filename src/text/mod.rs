//! Fonts and text: measuring and drawing with fontdue and DejaVu.
//!
//! Sizes passed in are logical pixels; drawing rasterizes at the physical
//! size (`size × scale`) so text stays sharp on HiDPI displays.

use std::cell::{Cell, OnceCell, RefCell};
use std::collections::HashMap;
use std::rc::Rc;

use fontdue::{Font, FontSettings};

use crate::gfx::{Canvas, Color};

mod emoji;

/// Generic font families available to CSS.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub enum Family {
    #[default]
    Sans,
    Mono,
    Serif,
    /// A downloaded `@font-face` family (index in the web font registry).
    Web(u16),
}

/// Most web font faces kept (they are never unloaded).
const MAX_WEB_FACES: usize = 128;

/// Fixed slots for web font faces (a `Vec` that never grows, so
/// references to its fonts stay valid).
struct WebSlots(Vec<OnceCell<Font>>);

impl Default for WebSlots {
    fn default() -> Self {
        WebSlots((0..MAX_WEB_FACES).map(|_| OnceCell::new()).collect())
    }
}

impl std::ops::Index<usize> for WebSlots {
    type Output = OnceCell<Font>;
    fn index(&self, i: usize) -> &OnceCell<Font> {
        &self.0[i]
    }
}

/// One loaded `@font-face` face: its style and slot in [`Fonts`].
#[derive(Clone, Copy, Debug)]
struct WebFace {
    /// `font-weight` (100–900).
    weight: u16,
    italic: bool,
    slot: usize,
}

thread_local! {
    /// Web font families by lowercase name, in registration order.
    static WEB_FAMILIES: RefCell<Vec<(String, Vec<WebFace>)>> = const { RefCell::new(Vec::new()) };
}

/// The registered web font family called `name`, if any face of it loaded.
pub fn web_family(name: &str) -> Option<Family> {
    let name = name.trim().trim_matches(['"', '\'']).to_ascii_lowercase();
    WEB_FAMILIES.with(|w| {
        w.borrow()
            .iter()
            .position(|(n, _)| *n == name)
            .map(|i| Family::Web(i as u16))
    })
}

/// The slot of the face of web family `g` closest to `bold` / `italic`.
fn web_slot(g: u16, bold: bool, italic: bool) -> Option<usize> {
    WEB_FAMILIES.with(|w| {
        let w = w.borrow();
        let faces = &w.get(g as usize)?.1;
        faces
            .iter()
            .min_by_key(|f| {
                let want: i32 = if bold { 700 } else { 400 };
                ((f.italic != italic) as i32 * 1000) + (f.weight as i32 - want).abs()
            })
            .map(|f| f.slot)
    })
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
        let family = match self.family {
            Family::Sans => 0,
            Family::Mono => 1,
            Family::Serif => 2,
            Family::Web(_) => 0, // drawn by `Fonts::web` slots (see face_for)
        };
        family * 4 + (self.bold as usize) * 2 + self.italic as usize
    }

    /// The same style in a bundled family (web fonts → sans-serif).
    fn bundled_key(self) -> FontKey {
        match self.family {
            Family::Web(_) => FontKey {
                family: Family::Sans,
                ..self
            },
            _ => self,
        }
    }

    fn data(&self) -> &'static [u8] {
        use dejavu::{sans, sans_mono, serif};
        match (self.bundled_key().family, self.bold, self.italic) {
            (Family::Sans, false, false) => sans::regular(),
            (Family::Sans, true, false) => sans::bold(),
            (Family::Sans, false, true) => sans::oblique(),
            (Family::Sans, true, true) => sans::bold_oblique(),
            (Family::Mono, false, false) => sans_mono::regular(),
            (Family::Mono, true, false) => sans_mono::bold(),
            (Family::Mono, false, true) => sans_mono::oblique(),
            (Family::Mono, true, true) => sans_mono::bold_oblique(),
            (Family::Serif, false, false) => serif::regular(),
            (Family::Serif, true, false) => serif::bold(),
            (Family::Serif, false, true) => serif::italic(),
            (Family::Serif, true, true) => serif::bold_italic(),
            (Family::Web(_), ..) => unreachable!("bundled_key maps web fonts"),
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
    faces: [OnceCell<Font>; 12],
    /// DejaVu faces when `faces` holds system fonts (for symbols they lack).
    bundled: [OnceCell<Font>; 12],
    /// System fonts for characters DejaVu lacks (Chinese, Japanese,
    /// Korean, …), tried in order of [`FALLBACK_FONTS`], each loaded on
    /// first use.
    fallbacks: [OnceCell<Option<Font>>; FALLBACK_SLOTS],
    /// Glyph ids in the fallback fonts by (glyph-cache face, char), from
    /// their Unicode character maps only (fontdue also reads legacy ones,
    /// such as a Big5 map whose codes collide with Hangul).
    fallback_gids: RefCell<HashMap<(usize, char), u16>>,
    /// (face, char, physical size in 1/4 px) → rasterized glyph.
    glyphs: RefCell<HashMap<(usize, char, u32), Glyph>>,
    /// Prefer installed Arial / Georgia (or metric-compatible Liberation
    /// fonts) over DejaVu for sans-serif and serif, so text widths match
    /// other browsers. Off by default (tests use the bundled fonts).
    system: bool,
    /// Downloaded web font faces (see [`Fonts::add_web_font`]).
    web: WebSlots,
    web_count: Cell<usize>,
    /// Raw font data by glyph-cache face index, for shaping.
    face_bytes: RefCell<HashMap<usize, Rc<[u8]>>>,
    /// Shaped right-to-left runs: (face, text, size in 1/4 px) → glyphs.
    shaped: RefCell<HashMap<(usize, String, u32), Rc<Shaped>>>,
    /// The system's color emoji font, loaded on first use.
    emoji: OnceCell<Option<emoji::EmojiFont>>,
    emoji_glyphs: RefCell<emoji::EmojiCache>,
}

/// A shaped run: the face that draws it and its glyphs in visual order.
struct Shaped {
    face: usize,
    /// (glyph id, advance, x offset, y offset), in pixels at the size.
    glyphs: Vec<(u16, f32, f32, f32)>,
}

/// Whether `c` is written right to left (Hebrew, Arabic, Syriac, Thaana,
/// N'Ko and their presentation forms).
pub fn is_rtl(c: char) -> bool {
    matches!(c as u32, 0x0590..=0x08FF | 0xFB1D..=0xFDFF | 0xFE70..=0xFEFF)
}

/// Whether `text` contains right-to-left characters.
pub fn has_rtl(text: &str) -> bool {
    text.chars().any(is_rtl)
}

const MAX_CACHED_GLYPHS: usize = 8192;

impl Fonts {
    pub fn new() -> Self {
        Self::default()
    }

    /// Fonts that use the system's Arial / Georgia when installed.
    pub fn with_system_fonts() -> Self {
        Fonts {
            system: true,
            ..Self::default()
        }
    }

    /// Registers a downloaded font (TTF, OTF, WOFF or WOFF2 bytes) as a
    /// face of `family`. Returns false if it cannot be used.
    pub fn add_web_font(&self, family: &str, weight: u16, italic: bool, bytes: &[u8]) -> bool {
        let slot = self.web_count.get();
        if slot >= MAX_WEB_FACES {
            return false;
        }
        let Some(font) = decode_font(bytes) else {
            return false;
        };
        let _ = self.web[slot].set(font.0);
        self.face_bytes
            .borrow_mut()
            .insert(Self::WEB_BASE + slot, font.1.into());
        self.web_count.set(slot + 1);
        let name = family.trim().trim_matches(['"', '\'']).to_ascii_lowercase();
        WEB_FAMILIES.with(|w| {
            let mut w = w.borrow_mut();
            let face = WebFace {
                weight,
                italic,
                slot,
            };
            match w.iter_mut().find(|(n, _)| *n == name) {
                Some((_, faces)) => faces.push(face),
                None => w.push((name, vec![face])),
            }
        });
        true
    }

    /// The web font face for `key`, if it is a loaded web family.
    fn web_face(&self, key: FontKey) -> Option<(usize, &Font)> {
        let Family::Web(g) = key.family else {
            return None;
        };
        let slot = web_slot(g, key.bold, key.italic)?;
        Some((Self::WEB_BASE + slot, self.web[slot].get()?))
    }

    /// Glyph-cache index of web font slot 0.
    const WEB_BASE: usize = 1000;

    fn face(&self, key: FontKey) -> &Font {
        if let Some((_, f)) = self.web_face(key) {
            return f;
        }
        let key = key.bundled_key();
        self.faces[key.index()].get_or_init(|| {
            if self.system
                && let Some((f, bytes)) = system_face(key)
            {
                self.face_bytes
                    .borrow_mut()
                    .insert(key.index(), bytes.into());
                return f;
            }
            Font::from_bytes(key.data(), FontSettings::default()).expect("bundled font is valid")
        })
    }

    /// Font data of glyph-cache face `index` (see [`Fonts::face_for`]).
    fn bytes_of(&self, index: usize) -> Option<Rc<[u8]>> {
        if let Some(b) = self.face_bytes.borrow().get(&index) {
            return Some(b.clone());
        }
        // Bundled faces: their static data.
        let local = match index {
            i if i < Self::FALLBACK && !self.system => i,
            i if i > Self::FALLBACK && i <= Self::FALLBACK + 12 => i - Self::FALLBACK - 1,
            _ => return None,
        };
        let family = match local / 4 {
            0 => Family::Sans,
            1 => Family::Mono,
            _ => Family::Serif,
        };
        let key = FontKey {
            family,
            bold: local & 2 != 0,
            italic: local & 1 != 0,
        };
        Some(Rc::from(key.data()))
    }

    /// Whether `text` should be shaped: right-to-left text, or text in a
    /// web font that has all its characters (ligatures such as icon fonts'
    /// `arrow_drop_down`, and kerning).
    fn needs_shaping(&self, key: FontKey, text: &str) -> bool {
        if has_rtl(text) {
            return true;
        }
        matches!(key.family, Family::Web(_))
            && text.chars().any(|c| c.is_ascii_alphabetic())
            && self.web_face(key).is_some_and(|(_, f)| {
                text.chars()
                    .all(|c| c.is_whitespace() || f.lookup_glyph_index(c) != 0)
            })
    }

    /// Shapes a run with the face that draws its first right-to-left (or
    /// else first) character. `None` if that face's data is not known.
    fn shape(&self, key: FontKey, size: f32, text: &str) -> Option<Rc<Shaped>> {
        let rtl = has_rtl(text);
        let first = text
            .chars()
            .find(|&c| if rtl { is_rtl(c) } else { !c.is_whitespace() })?;
        let (face, _) = self.face_for(key, first);
        let size_q = (size * 4.0).round() as u32;
        let cache_key = (face, text.to_string(), size_q);
        if let Some(s) = self.shaped.borrow().get(&cache_key) {
            return Some(s.clone());
        }
        let bytes = self.bytes_of(face)?;
        let rb_face = rustybuzz::Face::from_slice(&bytes, 0)?;
        let mut buffer = rustybuzz::UnicodeBuffer::new();
        buffer.push_str(text);
        buffer.set_direction(if rtl {
            rustybuzz::Direction::RightToLeft
        } else {
            rustybuzz::Direction::LeftToRight
        });
        buffer.guess_segment_properties();
        let out = rustybuzz::shape(&rb_face, &[], buffer);
        let scale = size / rb_face.units_per_em() as f32;
        let glyphs = out
            .glyph_infos()
            .iter()
            .zip(out.glyph_positions())
            .map(|(i, p)| {
                (
                    i.glyph_id as u16,
                    p.x_advance as f32 * scale,
                    p.x_offset as f32 * scale,
                    p.y_offset as f32 * scale,
                )
            })
            .collect();
        let shaped = Rc::new(Shaped { face, glyphs });
        let mut cache = self.shaped.borrow_mut();
        if cache.len() > 4096 {
            cache.clear();
        }
        cache.insert(cache_key, shaped.clone());
        Some(shaped)
    }

    /// The font behind glyph-cache face `index`.
    fn font_at(&self, index: usize) -> Option<&Font> {
        if index >= Self::WEB_BASE {
            return self.web[index - Self::WEB_BASE].get();
        }
        if (Self::FALLBACK_BASE..Self::FALLBACK_BASE + FALLBACK_SLOTS).contains(&index) {
            return self.fallback(index - Self::FALLBACK_BASE);
        }
        if index > Self::FALLBACK {
            return self.bundled[index - Self::FALLBACK - 1].get();
        }
        self.faces[index].get()
    }

    /// Bundled faces standing in for system ones have glyph-cache indexes
    /// after this one.
    const FALLBACK: usize = 12;

    fn bundled(&self, key: FontKey) -> &Font {
        if !self.system {
            return self.face(key);
        }
        let key = key.bundled_key();
        self.bundled[key.index()].get_or_init(|| {
            Font::from_bytes(key.data(), FontSettings::default()).expect("bundled font is valid")
        })
    }

    /// The glyph for `c` in `font`, glyph-cache face `index`.
    fn glyph_id(&self, index: usize, font: &Font, c: char) -> u16 {
        if !(Self::FALLBACK_BASE..Self::FALLBACK_BASE + FALLBACK_SLOTS).contains(&index) {
            return font.lookup_glyph_index(c);
        }
        if let Some(&g) = self.fallback_gids.borrow().get(&(index, c)) {
            return g;
        }
        let g = self
            .face_bytes
            .borrow()
            .get(&index)
            .and_then(|b| {
                rustybuzz::ttf_parser::Face::parse(b, 0)
                    .ok()?
                    .glyph_index(c)
            })
            .map_or(0, |g| g.0);
        self.fallback_gids.borrow_mut().insert((index, c), g);
        g
    }

    /// Glyph-cache indexes of the fallback fonts.
    const FALLBACK_BASE: usize = 500;

    /// Fallback font `i` (see [`FALLBACK_FONTS`]), if installed.
    fn fallback(&self, i: usize) -> Option<&Font> {
        self.fallbacks[i]
            .get_or_init(|| {
                let bytes = fallback_bytes(i)?;
                let font = Font::from_bytes(&bytes[..], FontSettings::default()).ok()?;
                self.face_bytes
                    .borrow_mut()
                    .insert(Self::FALLBACK_BASE + i, bytes.into());
                Some(font)
            })
            .as_ref()
    }

    /// The face that draws `c`: the requested one, else regular DejaVu Sans
    /// (same weight/style), else the system fallback font.
    fn face_for(&self, key: FontKey, c: char) -> (usize, &Font) {
        if let Some((index, font)) = self.web_face(key)
            && (c.is_whitespace() || c.is_control() || font.lookup_glyph_index(c) != 0)
        {
            return (index, font);
        }
        let key = key.bundled_key();
        let font = self.face(key);
        if c.is_whitespace() || c.is_control() || font.lookup_glyph_index(c) != 0 {
            return (key.index(), font);
        }
        let sans = FontKey {
            family: Family::Sans,
            ..key
        };
        let offset = if self.system { Self::FALLBACK + 1 } else { 0 };
        for k in [key, sans] {
            let f = self.bundled(k);
            if f.lookup_glyph_index(c) != 0 {
                return (offset + k.index(), f);
            }
        }
        for i in 0..FALLBACK_SLOTS {
            let index = Self::FALLBACK_BASE + i;
            if let Some(f) = self.fallback(i).filter(|f| self.glyph_id(index, f, c) != 0) {
                return (index, f);
            }
        }
        (key.index(), font)
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

    /// The color emoji font, if `text` needs it and there is one.
    fn emoji_for(&self, text: &str) -> Option<&emoji::EmojiFont> {
        if !emoji::has_emoji(text) {
            return None;
        }
        self.emoji.get_or_init(emoji::EmojiFont::load).as_ref()
    }

    /// Advance of each char of `text` at `size`, including kerning.
    pub fn advances(&self, key: FontKey, size: f32, text: &str) -> Vec<f32> {
        if let Some(font) = self.emoji_for(text) {
            // An emoji run's whole advance goes to its first character.
            let mut out = Vec::new();
            for (is_emoji, r) in emoji::runs(text) {
                let part = &text[r];
                if is_emoji {
                    let w: f32 = font.shape(size, part).iter().map(|g| g.1).sum();
                    out.push(w);
                    out.extend(std::iter::repeat_n(0.0, part.chars().count() - 1));
                } else {
                    out.extend(self.plain_advances(key, size, part));
                }
            }
            return out;
        }
        self.plain_advances(key, size, text)
    }

    fn plain_advances(&self, key: FontKey, size: f32, text: &str) -> Vec<f32> {
        let mut out = Vec::with_capacity(text.len());
        let mut prev: Option<(char, usize)> = None;
        for c in text.chars() {
            let (face, font) = self.face_for(key, c);
            let kern = prev
                .filter(|&(_, f)| f == face && face < Self::FALLBACK_BASE)
                .and_then(|(p, _)| font.horizontal_kern(p, c, size))
                .unwrap_or(0.0);
            if let Some(last) = out.last_mut() {
                *last += kern;
            }
            let gid = self.glyph_id(face, font, c);
            out.push(font.metrics_indexed(gid, size).advance_width);
            prev = Some((c, face));
        }
        out
    }

    /// Width of `text` at `size`.
    pub fn measure(&self, key: FontKey, size: f32, text: &str) -> f32 {
        if self.emoji_for(text).is_some() {
            return self.advances(key, size, text).iter().sum();
        }
        if self.needs_shaping(key, text)
            && let Some(s) = self.shape(key, size, text)
        {
            return s.glyphs.iter().map(|g| g.1).sum();
        }
        self.advances(key, size, text).iter().sum()
    }

    #[allow(clippy::too_many_arguments)]
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
        if let Some(font) = self.emoji_for(text) {
            let mut pen = x;
            for (is_emoji, r) in emoji::runs(text) {
                let part = &text[r];
                if !is_emoji {
                    pen += self.draw(canvas, key, size, pen, baseline, part, color);
                    continue;
                }
                let mut cache = self.emoji_glyphs.borrow_mut();
                for (gid, adv) in font.shape(size, part) {
                    if let Some(g) = font.glyph(&mut cache, gid, size) {
                        g.draw(canvas, pen, baseline);
                    }
                    pen += adv;
                }
            }
            return pen - x;
        }
        let size_q = (size * 4.0).round() as u32;
        if self.needs_shaping(key, text)
            && let Some(shaped) = self.shape(key, size, text)
            && let Some(font) = self.font_at(shaped.face)
        {
            // Shaped glyphs, already in visual order.
            let mut cache = self.glyphs.borrow_mut();
            if cache.len() > MAX_CACHED_GLYPHS {
                cache.clear();
            }
            let mut pen = x;
            for &(gid, adv, dx, dy) in &shaped.glyphs {
                // Glyph ids are cached under private-use code points.
                let id = char::from_u32(0xF0000 + gid as u32).unwrap_or('\u{FFFD}');
                let g = cache.entry((shaped.face, id, size_q)).or_insert_with(|| {
                    let (m, coverage) = font.rasterize_indexed(gid, size);
                    Glyph {
                        xmin: m.xmin,
                        ymin: m.ymin,
                        width: m.width,
                        height: m.height,
                        coverage,
                    }
                });
                let gx = (pen + dx).round() as i32 + g.xmin;
                let gy = (baseline - dy).round() as i32 - g.ymin - g.height as i32;
                canvas.draw_mask(gx, gy, g.width, g.height, &g.coverage, color);
                pen += adv;
            }
            return pen - x;
        }
        let advances = self.advances(key, size, text);
        let mut pen = x;
        let mut cache = self.glyphs.borrow_mut();
        if cache.len() > MAX_CACHED_GLYPHS {
            cache.clear();
        }
        for (c, adv) in text.chars().zip(advances) {
            if !c.is_whitespace() {
                let (face, font) = self.face_for(key, c);
                let gid = self.glyph_id(face, font, c);
                let g = cache.entry((face, c, size_q)).or_insert_with(|| {
                    let (m, coverage) = font.rasterize_indexed(gid, size);
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

/// Parses font bytes, unwrapping WOFF / WOFF2. Returns the font and its
/// (unwrapped) data.
fn decode_font(bytes: &[u8]) -> Option<(Font, Vec<u8>)> {
    let data = match bytes.get(..4)? {
        b"wOFF" => wuff::decompress_woff1(bytes).ok()?,
        b"wOF2" => wuff::decompress_woff2(bytes).ok()?,
        _ => bytes.to_vec(),
    };
    let font = Font::from_bytes(&data[..], FontSettings::default()).ok()?;
    Some((font, data))
}

/// An installed face metric-compatible with what other browsers use for
/// `key` (Arial for sans-serif, Georgia for serif).
fn system_face(key: FontKey) -> Option<(Font, Vec<u8>)> {
    #[cfg(not(target_arch = "wasm32"))]
    {
        let style = key.bold as usize * 2 + key.italic as usize;
        let candidates: &[[&str; 4]] = match key.family {
            Family::Sans => &[
                [
                    "/System/Library/Fonts/Supplemental/Arial.ttf",
                    "/System/Library/Fonts/Supplemental/Arial Italic.ttf",
                    "/System/Library/Fonts/Supplemental/Arial Bold.ttf",
                    "/System/Library/Fonts/Supplemental/Arial Bold Italic.ttf",
                ],
                [
                    "/usr/share/fonts/truetype/liberation/LiberationSans-Regular.ttf",
                    "/usr/share/fonts/truetype/liberation/LiberationSans-Italic.ttf",
                    "/usr/share/fonts/truetype/liberation/LiberationSans-Bold.ttf",
                    "/usr/share/fonts/truetype/liberation/LiberationSans-BoldItalic.ttf",
                ],
                [
                    "C:\\Windows\\Fonts\\arial.ttf",
                    "C:\\Windows\\Fonts\\ariali.ttf",
                    "C:\\Windows\\Fonts\\arialbd.ttf",
                    "C:\\Windows\\Fonts\\arialbi.ttf",
                ],
            ],
            Family::Serif => &[
                [
                    "/System/Library/Fonts/Supplemental/Georgia.ttf",
                    "/System/Library/Fonts/Supplemental/Georgia Italic.ttf",
                    "/System/Library/Fonts/Supplemental/Georgia Bold.ttf",
                    "/System/Library/Fonts/Supplemental/Georgia Bold Italic.ttf",
                ],
                [
                    "/usr/share/fonts/truetype/liberation/LiberationSerif-Regular.ttf",
                    "/usr/share/fonts/truetype/liberation/LiberationSerif-Italic.ttf",
                    "/usr/share/fonts/truetype/liberation/LiberationSerif-Bold.ttf",
                    "/usr/share/fonts/truetype/liberation/LiberationSerif-BoldItalic.ttf",
                ],
                [
                    "C:\\Windows\\Fonts\\georgia.ttf",
                    "C:\\Windows\\Fonts\\georgiai.ttf",
                    "C:\\Windows\\Fonts\\georgiab.ttf",
                    "C:\\Windows\\Fonts\\georgiaz.ttf",
                ],
            ],
            Family::Mono | Family::Web(_) => &[],
        };
        for set in candidates {
            if let Ok(bytes) = std::fs::read(set[style])
                && let Ok(font) = Font::from_bytes(&bytes[..], FontSettings::default())
            {
                return Some((font, bytes));
            }
        }
    }
    let _ = key;
    None
}

/// System fonts for characters the bundled ones lack, most likely first:
/// Chinese, Japanese and Korean faces, then wide-coverage ones, for macOS,
/// Linux and Windows (those not installed are skipped).
/// `RUBROWSER_FALLBACK_FONT` names one to try before them.
const FALLBACK_FONTS: [&[&str]; 7] = [
    &[
        "/System/Library/Fonts/STHeiti Medium.ttc",
        "/usr/share/fonts/opentype/noto/NotoSansCJK-Regular.ttc",
        "/usr/share/fonts/noto-cjk/NotoSansCJK-Regular.ttc",
        "/usr/share/fonts/google-noto-cjk/NotoSansCJK-Regular.ttc",
        "C:\\Windows\\Fonts\\msjh.ttc",
        "C:\\Windows\\Fonts\\msyh.ttc",
    ],
    &[
        "/System/Library/Fonts/ヒラギノ角ゴシック W3.ttc",
        "/System/Library/Fonts/Hiragino Sans GB.ttc",
        "C:\\Windows\\Fonts\\meiryo.ttc",
        "C:\\Windows\\Fonts\\YuGothM.ttc",
    ],
    &[
        "/System/Library/Fonts/AppleSDGothicNeo.ttc",
        "/System/Library/Fonts/Supplemental/AppleGothic.ttf",
        "/usr/share/fonts/truetype/nanum/NanumGothic.ttf",
        "C:\\Windows\\Fonts\\malgun.ttf",
    ],
    &[
        "/System/Library/Fonts/Supplemental/Arial Unicode.ttf",
        "/usr/share/fonts/truetype/wqy/wqy-microhei.ttc",
        "/usr/share/fonts/wenquanyi/wqy-microhei/wqy-microhei.ttc",
        "C:\\Windows\\Fonts\\simsun.ttc",
    ],
    &[
        "/System/Library/Fonts/ThonburiUI.ttc",
        "/usr/share/fonts/truetype/noto/NotoSansThai-Regular.ttf",
        "C:\\Windows\\Fonts\\tahoma.ttf",
    ],
    &[
        "/System/Library/Fonts/Kohinoor.ttc",
        "/usr/share/fonts/truetype/noto/NotoSansDevanagari-Regular.ttf",
        "C:\\Windows\\Fonts\\Nirmala.ttc",
    ],
    &[
        "/System/Library/Fonts/Apple Symbols.ttf",
        "/usr/share/fonts/truetype/noto/NotoSansSymbols-Regular.ttf",
        "C:\\Windows\\Fonts\\seguisym.ttf",
    ],
];

const FALLBACK_SLOTS: usize = FALLBACK_FONTS.len();

/// The data of fallback font `i`: the first installed file of that slot.
fn fallback_bytes(i: usize) -> Option<Vec<u8>> {
    #[cfg(not(target_arch = "wasm32"))]
    {
        // A custom font takes the first slot.
        if i == 0
            && let Some(bytes) = std::env::var("RUBROWSER_FALLBACK_FONT")
                .ok()
                .and_then(|p| std::fs::read(p).ok())
        {
            return Some(bytes);
        }
        return FALLBACK_FONTS[i].iter().find_map(|p| std::fs::read(p).ok());
    }
    #[allow(unreachable_code)]
    {
        let _ = i;
        None
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
    fn fallback_fonts_are_chosen_by_their_unicode_maps() {
        // STHeiti also has a Big5 character map whose codes collide with
        // Hangul: Korean goes to the Korean font, not STHeiti's 猓.
        let f = Fonts::new();
        let korean = std::path::Path::new(FALLBACK_FONTS[2][0]).exists();
        if korean && std::path::Path::new(FALLBACK_FONTS[0][0]).exists() {
            let (face, _) = f.face_for(SANS, '\u{B274}');
            assert_eq!(face, Fonts::FALLBACK_BASE + 2);
        }
        let (face, font) = f.face_for(SANS, '\u{B274}');
        if face >= Fonts::FALLBACK_BASE {
            assert_ne!(f.glyph_id(face, font, '\u{B274}'), 0);
        }
    }

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
