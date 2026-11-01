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
    /// `calc()` mixing both: `px + percent% of the base`.
    Calc(f32, f32),
}

impl Dim {
    /// Pixels, treating `auto` as 0 and percentages against `base`.
    pub fn px_or_zero(&self, base: f32) -> f32 {
        match *self {
            Dim::Auto => 0.0,
            Dim::Px(v) => v,
            Dim::Percent(p) => base * p / 100.0,
            Dim::Calc(px, p) => px + base * p / 100.0,
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
    /// Viewport size, for `vw` / `vh`.
    pub vw: f32,
    pub vh: f32,
}

/// Parses a number with an optional unit into a [`Dim`].
pub fn dim(s: &str, fc: FontContext) -> Option<Dim> {
    let s = s.trim().to_ascii_lowercase();
    if s == "auto" || s == "none" {
        return Some(Dim::Auto);
    }
    if s.ends_with(')')
        && (s.starts_with("calc(")
            || s.starts_with("min(")
            || s.starts_with("max(")
            || s.starts_with("clamp("))
    {
        let lin = Calc {
            s: s.as_bytes(),
            i: 0,
            fc,
        }
        .expr()?;
        return Some(lin.dim());
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
        "vw" => v * fc.vw / 100.0,
        "vh" => v * fc.vh / 100.0,
        "vmin" => v * fc.vw.min(fc.vh) / 100.0,
        "vmax" => v * fc.vw.max(fc.vh) / 100.0,
        "q" => v * 96.0 / 101.6,
        _ => return None,
    };
    Some(Dim::Px(px))
}

/// A linear length `px + pct%` (or a plain number) inside `calc()`.
#[derive(Clone, Copy, Debug, PartialEq)]
struct Lin {
    px: f32,
    pct: f32,
    /// A unitless number (multiplier).
    num: bool,
}

impl Lin {
    fn dim(self) -> Dim {
        if self.pct == 0.0 {
            Dim::Px(self.px)
        } else if self.px == 0.0 {
            Dim::Percent(self.pct)
        } else {
            Dim::Calc(self.px, self.pct)
        }
    }
}

/// A recursive-descent evaluator for `calc()`, `min()`, `max()`, `clamp()`.
struct Calc<'a> {
    s: &'a [u8],
    i: usize,
    fc: FontContext,
}

impl Calc<'_> {
    fn ws(&mut self) {
        while self.s.get(self.i).is_some_and(|c| c.is_ascii_whitespace()) {
            self.i += 1;
        }
    }

    fn expr(&mut self) -> Option<Lin> {
        self.ws();
        let mut acc = self.term()?;
        loop {
            self.ws();
            match self.s.get(self.i) {
                Some(b'+') | Some(b'-') => {
                    let neg = self.s[self.i] == b'-';
                    self.i += 1;
                    let t = self.term()?;
                    let k = if neg { -1.0 } else { 1.0 };
                    acc = Lin {
                        px: acc.px + k * t.px,
                        pct: acc.pct + k * t.pct,
                        num: acc.num && t.num,
                    };
                }
                _ => return Some(acc),
            }
        }
    }

    fn term(&mut self) -> Option<Lin> {
        let mut acc = self.factor()?;
        loop {
            self.ws();
            match self.s.get(self.i) {
                Some(b'*') => {
                    self.i += 1;
                    let f = self.factor()?;
                    acc = if acc.num {
                        Lin {
                            px: f.px * acc.px,
                            pct: f.pct * acc.px,
                            num: f.num,
                        }
                    } else if f.num {
                        Lin {
                            px: acc.px * f.px,
                            pct: acc.pct * f.px,
                            num: false,
                        }
                    } else {
                        return None;
                    };
                }
                Some(b'/') => {
                    self.i += 1;
                    let f = self.factor()?;
                    if !f.num || f.px == 0.0 {
                        return None;
                    }
                    acc = Lin {
                        px: acc.px / f.px,
                        pct: acc.pct / f.px,
                        num: acc.num,
                    };
                }
                _ => return Some(acc),
            }
        }
    }

    fn factor(&mut self) -> Option<Lin> {
        self.ws();
        let rest = std::str::from_utf8(&self.s[self.i..]).ok()?;
        for (name, kind) in [
            ("calc(", 0),
            ("min(", 1),
            ("max(", 2),
            ("clamp(", 3),
            ("(", 0),
        ] {
            if rest.starts_with(name) {
                self.i += name.len();
                let mut args = vec![self.expr()?];
                self.ws();
                while self.s.get(self.i) == Some(&b',') {
                    self.i += 1;
                    args.push(self.expr()?);
                    self.ws();
                }
                if self.s.get(self.i) != Some(&b')') {
                    return None;
                }
                self.i += 1;
                return pick(kind, &args);
            }
        }
        // A number with an optional unit.
        let start = self.i;
        if matches!(self.s.get(self.i), Some(b'+') | Some(b'-')) {
            self.i += 1;
        }
        while self
            .s
            .get(self.i)
            .is_some_and(|c| c.is_ascii_alphanumeric() || *c == b'.' || *c == b'%')
        {
            self.i += 1;
        }
        let tok = std::str::from_utf8(&self.s[start..self.i]).ok()?;
        if let Ok(n) = tok.parse::<f32>() {
            return Some(Lin {
                px: n,
                pct: 0.0,
                num: true,
            });
        }
        match dim(tok, self.fc)? {
            Dim::Px(px) => Some(Lin {
                px,
                pct: 0.0,
                num: false,
            }),
            Dim::Percent(p) => Some(Lin {
                px: 0.0,
                pct: p,
                num: false,
            }),
            _ => None,
        }
    }
}

/// `min`/`max`/`clamp` of linear values; exact when all are pixels,
/// otherwise the percentage-free argument wins where it can.
fn pick(kind: u8, args: &[Lin]) -> Option<Lin> {
    let all_px = args.iter().all(|a| a.pct == 0.0);
    match kind {
        0 => args.first().copied(),
        1 | 2 if all_px => {
            let v = args.iter().map(|a| a.px);
            let px = if kind == 1 {
                v.fold(f32::INFINITY, f32::min)
            } else {
                v.fold(f32::NEG_INFINITY, f32::max)
            };
            Some(Lin {
                px,
                pct: 0.0,
                num: args.iter().all(|a| a.num),
            })
        }
        // Mixed: prefer the percentage term for min() (responsive widths
        // such as min(100%, 600px)), the pixel term for max().
        1 => args.iter().find(|a| a.pct != 0.0).copied(),
        2 => args.iter().find(|a| a.pct == 0.0).copied(),
        3 if args.len() == 3 => {
            if all_px {
                Some(Lin {
                    px: args[1].px.clamp(args[0].px, args[2].px.max(args[0].px)),
                    pct: 0.0,
                    num: false,
                })
            } else {
                Some(args[1])
            }
        }
        _ => None,
    }
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
        vw: 1000.0,
        vh: 500.0,
    };

    #[test]
    fn calc_and_viewport_units() {
        assert_eq!(dim("10vw", FC), Some(Dim::Px(100.0)));
        assert_eq!(dim("calc(2em + 5px)", FC), Some(Dim::Px(25.0)));
        assert_eq!(dim("calc(100% - 20px)", FC), Some(Dim::Calc(-20.0, 100.0)));
        assert_eq!(dim("calc(50% * 2)", FC), Some(Dim::Percent(100.0)));
        assert_eq!(dim("calc((1rem + 4px) / 2)", FC), Some(Dim::Px(10.0)));
        assert_eq!(dim("min(10px, 2em)", FC), Some(Dim::Px(10.0)));
        assert_eq!(dim("max(10px, 2em)", FC), Some(Dim::Px(20.0)));
        assert_eq!(dim("clamp(10px, 50px, 30px)", FC), Some(Dim::Px(30.0)));
        assert_eq!(dim("min(100%, 600px)", FC), Some(Dim::Percent(100.0)));
        assert_eq!(dim("calc(1px +)", FC), None);
    }

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
