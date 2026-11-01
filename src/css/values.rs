//! Parsing CSS component values: lengths, colors, numbers.

/// A color with alpha.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub struct Rgba {
    pub r: u8,
    pub g: u8,
    pub b: u8,
    pub a: u8,
}

impl Rgba {
    pub const TRANSPARENT: Rgba = Rgba::new(0, 0, 0, 0);
    pub const BLACK: Rgba = Rgba::new(0, 0, 0, 255);

    pub const fn new(r: u8, g: u8, b: u8, a: u8) -> Self {
        Rgba { r, g, b, a }
    }

    /// `0x00RRGGBB` for the canvas.
    pub fn rgb(&self) -> u32 {
        (self.r as u32) << 16 | (self.g as u32) << 8 | self.b as u32
    }

    pub fn is_visible(&self) -> bool {
        self.a > 0
    }
}

/// Parses a color; `current` is used for `currentcolor`.
pub fn color(s: &str, current: Rgba) -> Option<Rgba> {
    let s = s.trim();
    if s.eq_ignore_ascii_case("currentcolor") {
        return Some(current);
    }
    let c = csscolorparser::parse(s).ok()?.to_rgba8();
    Some(Rgba::new(c[0], c[1], c[2], c[3]))
}

/// A length before layout: resolved to pixels except for percentages.
#[derive(Clone, Copy, Debug, PartialEq, Default)]
pub enum Dim {
    #[default]
    Auto,
    Px(f32),
    Percent(f32),
}

impl Dim {
    /// Pixels, treating `auto` as 0 and percentages against `base`.
    pub fn px_or_zero(&self, base: f32) -> f32 {
        match *self {
            Dim::Auto => 0.0,
            Dim::Px(v) => v,
            Dim::Percent(p) => base * p / 100.0,
        }
    }
}

/// Font sizes needed to resolve relative units.
#[derive(Clone, Copy, Debug)]
pub struct FontContext {
    /// Font size that `em` refers to.
    pub em: f32,
    /// Root element font size, for `rem`.
    pub rem: f32,
}

/// Parses a number with an optional unit into a [`Dim`].
pub fn dim(s: &str, fc: FontContext) -> Option<Dim> {
    let s = s.trim().to_ascii_lowercase();
    if s == "auto" || s == "none" {
        return Some(Dim::Auto);
    }
    if let Some(n) = s.strip_suffix('%') {
        return n.trim().parse().ok().map(Dim::Percent);
    }
    let split = s.find(|c: char| c.is_ascii_alphabetic()).unwrap_or(s.len());
    let (num, unit) = s.split_at(split);
    let v: f32 = num.trim().parse().ok()?;
    let px = match unit {
        "" if v == 0.0 => 0.0,
        "" => return None,
        "px" => v,
        "em" => v * fc.em,
        "rem" => v * fc.rem,
        "pt" => v * 4.0 / 3.0,
        "pc" => v * 16.0,
        "in" => v * 96.0,
        "cm" => v * 96.0 / 2.54,
        "mm" => v * 96.0 / 25.4,
        "ex" | "ch" => v * fc.em * 0.5,
        _ => return None,
    };
    Some(Dim::Px(px))
}

/// Parses a length that must resolve to pixels (no `auto`, no `%`).
pub fn px(s: &str, fc: FontContext) -> Option<f32> {
    match dim(s, fc)? {
        Dim::Px(v) => Some(v),
        _ => None,
    }
}

pub fn number(s: &str) -> Option<f32> {
    s.trim().parse().ok()
}

/// Splits a value on whitespace, keeping `rgb(…)`-style groups together.
pub fn tokens(s: &str) -> Vec<&str> {
    let mut out = Vec::new();
    let mut depth = 0;
    let mut start: Option<usize> = None;
    for (i, c) in s.char_indices() {
        match c {
            '(' => depth += 1,
            ')' => depth -= 1,
            _ => {}
        }
        if c.is_whitespace() && depth == 0 {
            if let Some(st) = start.take() {
                out.push(&s[st..i]);
            }
        } else if start.is_none() {
            start = Some(i);
        }
    }
    if let Some(st) = start {
        out.push(&s[st..]);
    }
    out
}

/// Expands 1–4 values into top, right, bottom, left.
pub fn four<T: Copy>(v: &[T]) -> Option<[T; 4]> {
    match *v {
        [a] => Some([a, a, a, a]),
        [a, b] => Some([a, b, a, b]),
        [a, b, c] => Some([a, b, c, b]),
        [a, b, c, d] => Some([a, b, c, d]),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const FC: FontContext = FontContext {
        em: 10.0,
        rem: 16.0,
    };

    #[test]
    fn lengths() {
        assert_eq!(dim("12px", FC), Some(Dim::Px(12.0)));
        assert_eq!(dim("1.5em", FC), Some(Dim::Px(15.0)));
        assert_eq!(dim("2rem", FC), Some(Dim::Px(32.0)));
        assert_eq!(dim("50%", FC), Some(Dim::Percent(50.0)));
        assert_eq!(dim("0", FC), Some(Dim::Px(0.0)));
        assert_eq!(dim("auto", FC), Some(Dim::Auto));
        assert_eq!(dim("12", FC), None);
        assert_eq!(dim("red", FC), None);
    }

    #[test]
    fn colors() {
        let cur = Rgba::new(1, 2, 3, 255);
        assert_eq!(color("#f00", cur), Some(Rgba::new(255, 0, 0, 255)));
        assert_eq!(
            color("rgba(0, 0, 255, 0.5)", cur),
            Some(Rgba::new(0, 0, 255, 128))
        );
        assert_eq!(color("currentColor", cur), Some(cur));
        assert_eq!(color("transparent", cur), Some(Rgba::new(0, 0, 0, 0)));
        assert_eq!(color("nonsense", cur), None);
        assert_eq!(Rgba::new(0x12, 0x34, 0x56, 255).rgb(), 0x123456);
    }

    #[test]
    fn token_splitting() {
        assert_eq!(
            tokens("1px solid rgb(1, 2, 3)"),
            vec!["1px", "solid", "rgb(1, 2, 3)"]
        );
        assert_eq!(four(&[1, 2]), Some([1, 2, 1, 2]));
        assert_eq!(four(&[1, 2, 3]), Some([1, 2, 3, 2]));
    }
}
