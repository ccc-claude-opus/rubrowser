//! Color emoji: a system emoji font whose glyphs are bitmaps (`sbix` /
//! `CBDT`), memory-mapped, shaped with rustybuzz (flags, ZWJ sequences,
//! skin tones) and drawn from its PNG images.

use std::collections::HashMap;
use std::ops::Range;
use std::rc::Rc;

use crate::gfx::Canvas;

/// The emoji font's data.
pub struct EmojiFont {
    data: Box<dyn AsRef<[u8]>>,
}

/// One emoji glyph rendered at a size: ARGB pixels and where they go
/// relative to the pen position on the baseline.
pub struct EmojiGlyph {
    dx: i32,
    /// From the baseline to the image's top (negative: above).
    dy: i32,
    w: usize,
    h: usize,
    argb: Vec<u32>,
}

pub type EmojiCache = HashMap<(u16, u32), Option<Rc<EmojiGlyph>>>;

impl EmojiFont {
    /// The system's color emoji font, if there is one.
    pub fn load() -> Option<EmojiFont> {
        #[cfg(feature = "native")]
        {
            for path in [
                "/System/Library/Fonts/Apple Color Emoji.ttc",
                "/usr/share/fonts/truetype/noto/NotoColorEmoji.ttf",
                "/usr/share/fonts/noto/NotoColorEmoji.ttf",
                "/usr/share/fonts/google-noto-emoji/NotoColorEmoji.ttf",
                "C:\\Windows\\Fonts\\seguiemj.ttf",
            ] {
                let Ok(file) = std::fs::File::open(path) else { continue };
                // SAFETY: system font files are not modified while mapped.
                let Ok(map) = (unsafe { memmap2::Mmap::map(&file) }) else { continue };
                let font = EmojiFont {
                    data: Box::new(map),
                };
                if font.face().is_some() {
                    return Some(font);
                }
            }
        }
        None
    }

    fn face(&self) -> Option<rustybuzz::Face<'_>> {
        rustybuzz::Face::from_slice((*self.data).as_ref(), 0)
    }

    /// Glyphs of emoji run `text` at `size` px: (glyph id, advance).
    pub fn shape(&self, size: f32, text: &str) -> Vec<(u16, f32)> {
        let Some(face) = self.face() else {
            return Vec::new();
        };
        let mut buffer = rustybuzz::UnicodeBuffer::new();
        buffer.push_str(text);
        buffer.set_direction(rustybuzz::Direction::LeftToRight);
        buffer.guess_segment_properties();
        let out = rustybuzz::shape(&face, &[], buffer);
        let scale = size / face.units_per_em() as f32;
        out.glyph_infos()
            .iter()
            .zip(out.glyph_positions())
            .map(|(i, p)| (i.glyph_id as u16, p.x_advance as f32 * scale))
            .collect()
    }

    /// Glyph `gid` at `size` px, decoded and scaled (cached).
    pub fn glyph(&self, cache: &mut EmojiCache, gid: u16, size: f32) -> Option<Rc<EmojiGlyph>> {
        let key = (gid, (size * 4.0).round() as u32);
        if let Some(g) = cache.get(&key) {
            return g.clone();
        }
        let g = self.render(gid, size).map(Rc::new);
        if cache.len() > 2048 {
            cache.clear();
        }
        cache.insert(key, g.clone());
        g
    }

    fn render(&self, gid: u16, size: f32) -> Option<EmojiGlyph> {
        let face = self.face()?;
        let ppem = size.ceil().clamp(1.0, u16::MAX as f32) as u16;
        let img = face.glyph_raster_image(rustybuzz::ttf_parser::GlyphId(gid), ppem)?;
        let decoded = image::load_from_memory(img.data).ok()?.to_rgba8();
        let k = size / img.pixels_per_em.max(1) as f32;
        let w = ((decoded.width() as f32) * k).round().max(1.0) as u32;
        let h = ((decoded.height() as f32) * k).round().max(1.0) as u32;
        let scaled = image::imageops::resize(&decoded, w, h, image::imageops::FilterType::Triangle);
        let argb = scaled
            .pixels()
            .map(|p| {
                let [r, g, b, a] = p.0;
                (a as u32) << 24 | (r as u32) << 16 | (g as u32) << 8 | b as u32
            })
            .collect();
        Some(EmojiGlyph {
            dx: (img.x as f32 * k).round() as i32,
            dy: -((img.y as f32 + decoded.height() as f32) * k).round() as i32,
            w: w as usize,
            h: h as usize,
            argb,
        })
    }
}

impl EmojiGlyph {
    pub fn draw(&self, canvas: &mut Canvas, pen: f32, baseline: f32) {
        let x = pen.round() as i32 + self.dx;
        let y = baseline.round() as i32 + self.dy;
        canvas.draw_argb(x, y, self.w, self.h, &self.argb);
    }
}

/// Whether `c` is shown as an emoji by default (Unicode's
/// Emoji_Presentation, roughly).
pub fn is_emoji(c: char) -> bool {
    let u = c as u32;
    matches!(u,
        0x1F1E6..=0x1F1FF
        | 0x1F300..=0x1F64F
        | 0x1F680..=0x1F6FF
        | 0x1F7E0..=0x1F7EB
        | 0x1F90C..=0x1F9FF
        | 0x1FA70..=0x1FAFF
        | 0x1F004 | 0x1F0CF | 0x1F18E | 0x1F191..=0x1F19A
        | 0x1F201 | 0x1F21A | 0x1F22F | 0x1F232..=0x1F23A | 0x1F250 | 0x1F251
        | 0x231A | 0x231B | 0x23E9..=0x23EC | 0x23F0 | 0x23F3 | 0x25FD | 0x25FE
        | 0x2614 | 0x2615 | 0x2648..=0x2653 | 0x267F | 0x2693 | 0x26A1 | 0x26AA | 0x26AB
        | 0x26BD | 0x26BE | 0x26C4 | 0x26C5 | 0x26CE | 0x26D4 | 0x26EA | 0x26F2 | 0x26F3
        | 0x26F5 | 0x26FA | 0x26FD | 0x2705 | 0x270A | 0x270B | 0x2728 | 0x274C | 0x274E
        | 0x2753..=0x2755 | 0x2757 | 0x2795..=0x2797 | 0x27B0 | 0x27BF | 0x2B1B | 0x2B1C
        | 0x2B50 | 0x2B55)
}

/// Characters that continue an emoji cluster: variation selector 16,
/// zero-width joiner, skin tones, keycap, tags.
fn continues(c: char) -> bool {
    matches!(c as u32, 0xFE0F | 0x200D | 0x1F3FB..=0x1F3FF | 0x20E3 | 0xE0020..=0xE007F)
}

/// Whether `text` has anything to draw as emoji.
pub fn has_emoji(text: &str) -> bool {
    text.chars().any(|c| is_emoji(c) || c == '\u{FE0F}')
}

/// Splits `text` into runs: (is emoji, byte range). A character followed
/// by U+FE0F is an emoji too (e.g. ❤️).
pub fn runs(text: &str) -> Vec<(bool, Range<usize>)> {
    let chars: Vec<(usize, char)> = text.char_indices().collect();
    let mut out: Vec<(bool, Range<usize>)> = Vec::new();
    let mut joined = false;
    for (k, &(i, c)) in chars.iter().enumerate() {
        let next = chars.get(k + 1).map(|&(_, n)| n);
        let in_emoji = out.last().is_some_and(|r| r.0);
        let emoji = is_emoji(c)
            || next == Some('\u{FE0F}')
            || next == Some('\u{20E3}')
            || (in_emoji && (continues(c) || joined));
        joined = c == '\u{200D}';
        let end = i + c.len_utf8();
        match out.last_mut() {
            Some((e, r)) if *e == emoji => r.end = end,
            _ => out.push((emoji, i..end)),
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn splits_emoji_runs() {
        let r = runs("Hi 👍🏽 ok ❤️ 🇹🇼!");
        let kinds: Vec<(bool, &str)> = r.iter().map(|(e, r)| (*e, &"Hi 👍🏽 ok ❤️ 🇹🇼!"[r.clone()])).collect();
        assert_eq!(
            kinds,
            vec![(false, "Hi "), (true, "👍🏽"), (false, " ok "), (true, "❤️"), (false, " "), (true, "🇹🇼"), (false, "!")]
        );
        assert_eq!(runs("👨‍👩‍👧").len(), 1, "a ZWJ family is one run");
        assert!(!has_emoji("plain ❤ text"));
    }
}
