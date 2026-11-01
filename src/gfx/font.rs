//! 8×8 bitmap font (from the `font8x8` crate), drawn at integer scales.
//!
//! Every glyph occupies an 8×8 cell, so text is monospaced and its width is
//! simply `chars × 8 × k` pixels, where `k` is the integer pixel scale.

use font8x8::legacy::{BASIC_LEGACY, BOX_LEGACY, GREEK_LEGACY, LATIN_LEGACY};

/// Glyph cell size in font pixels.
pub const CELL: i32 = 8;

/// Shown for characters the font does not have.
const MISSING: [u8; 8] = [0x7e, 0x42, 0x42, 0x42, 0x42, 0x42, 0x7e, 0x00];

/// Returns the bitmap of `ch`; bit 0 of each row is the leftmost pixel.
pub fn glyph(ch: char) -> &'static [u8; 8] {
    let c = ch as usize;
    match c {
        0x20..=0x7e => &BASIC_LEGACY[c],
        0xa0..=0xff => &LATIN_LEGACY[c - 0xa0],
        0x390..=0x3c9 => &GREEK_LEGACY[c - 0x390],
        0x2500..=0x257f => &BOX_LEGACY[c - 0x2500],
        _ => &MISSING,
    }
}

/// Integer scale for text `size` logical pixels tall on a display with
/// `scale` physical pixels per logical pixel.
pub fn pixel_scale(size: f32, scale: f32) -> i32 {
    ((size * scale / CELL as f32).round() as i32).max(1)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn glyph_ranges() {
        assert_eq!(glyph(' '), &[0; 8]);
        assert_ne!(glyph('A'), &MISSING);
        assert_ne!(glyph('é'), &MISSING);
        assert_eq!(glyph('中'), &MISSING);
    }

    #[test]
    fn pixel_scale_rounds_and_has_minimum() {
        assert_eq!(pixel_scale(16.0, 1.0), 2);
        assert_eq!(pixel_scale(16.0, 2.0), 4);
        assert_eq!(pixel_scale(12.0, 2.0), 3);
        assert_eq!(pixel_scale(1.0, 1.0), 1);
    }
}
