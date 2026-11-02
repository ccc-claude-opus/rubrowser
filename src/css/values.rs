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
    if s.len() > 10 && s[..10].eq_ignore_ascii_case("color-mix(") && s.ends_with(')') {
        return color_mix(&s[10..s.len() - 1], current);
    }
    let c = csscolorparser::parse(s).ok()?.to_rgba8();
    Some(Rgba::new(c[0], c[1], c[2], c[3]))
}

/// The arguments of `color-mix()` (CSS Color 5): `in <space>`, then two
/// colors with optional percentages. Mixed with premultiplied alpha.
fn color_mix(args: &str, current: Rgba) -> Option<Rgba> {
    use csscolorparser::Color;
    let parts = super::gradient::split_commas(args);
    let [space, a, b] = parts.as_slice() else {
        return None;
    };
    let space = space.trim().to_ascii_lowercase();
    let space = space.strip_prefix("in ")?.split_whitespace().next()?.to_string();
    // A color and its optional percentage, in either order.
    let item = |s: &str| -> Option<(Color, Option<f32>)> {
        let t = tokens(s);
        let (pct, rest): (Vec<&str>, Vec<&str>) = t.iter().partition(|w| w.ends_with('%'));
        let p = match pct.as_slice() {
            [] => None,
            [p] => Some(p.trim_end_matches('%').parse::<f32>().ok()?),
            _ => return None,
        };
        let c = color(&rest.join(" "), current)?;
        Some((Color::from_rgba8(c.r, c.g, c.b, c.a), p))
    };
    let ((c1, p1), (c2, p2)) = (item(a)?, item(b)?);
    let (p1, p2) = match (p1, p2) {
        (None, None) => (50.0, 50.0),
        (Some(p), None) => (p, 100.0 - p),
        (None, Some(p)) => (100.0 - p, p),
        (Some(x), Some(y)) => (x, y),
    };
    let sum = p1 + p2;
    if sum <= 0.0 {
        return None;
    }
    let t = p2 / sum;
    // Components in the mixing space (hue spaces are approximated by
    // their rectangular forms).
    let to = |c: &Color| -> [f32; 4] {
        match space.as_str() {
            "srgb-linear" => c.to_linear_rgba(),
            "oklab" | "oklch" => c.to_oklaba(),
            "lab" | "lch" => c.to_laba(),
            _ => [c.r, c.g, c.b, c.a],
        }
    };
    let from = |v: [f32; 4]| -> Color {
        match space.as_str() {
            "srgb-linear" => Color::from_linear_rgba(v[0], v[1], v[2], v[3]),
            "oklab" | "oklch" => Color::from_oklaba(v[0], v[1], v[2], v[3]),
            "lab" | "lch" => Color::from_laba(v[0], v[1], v[2], v[3]),
            _ => Color::new(v[0], v[1], v[2], v[3]),
        }
    };
    let (v1, v2) = (to(&c1), to(&c2));
    let alpha = v1[3] + t * (v2[3] - v1[3]);
    let mut out = [0.0; 4];
    for k in 0..3 {
        let premul = v1[k] * v1[3] + t * (v2[k] * v2[3] - v1[k] * v1[3]);
        out[k] = if alpha > 0.0 { premul / alpha } else { 0.0 };
    }
    // Percentages summing to less than 100% make the result transparent
    // in proportion.
    out[3] = alpha * (sum / 100.0).min(1.0);
    let c = from(out).clamp().to_rgba8();
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
    /// `min()` / `max()` / `clamp()` mixing pixels and percentages, which
    /// only the base decides (an index into the interned expressions).
    Expr(u32),
}

impl Dim {
    /// Pixels, treating `auto` as 0 and percentages against `base`.
    pub fn px_or_zero(&self, base: f32) -> f32 {
        match *self {
            Dim::Auto => 0.0,
            Dim::Px(v) => v,
            Dim::Percent(p) => base * p / 100.0,
            Dim::Calc(px, p) => px + base * p / 100.0,
            Dim::Expr(i) => EXPRS.with(|e| e.borrow().list[i as usize].eval(base)),
        }
    }

    /// Whether the value depends on a percentage base.
    pub fn has_percent(&self) -> bool {
        matches!(self, Dim::Percent(_) | Dim::Calc(..) | Dim::Expr(_))
    }
}

/// A math expression over lengths and percentages.
#[derive(Clone, Debug, PartialEq)]
enum Expr {
    Lin(Lin),
    Add(Box<Expr>, Box<Expr>),
    Scale(Box<Expr>, f32),
    Min(Vec<Expr>),
    Max(Vec<Expr>),
    Clamp(Box<[Expr; 3]>),
}

impl Expr {
    fn eval(&self, base: f32) -> f32 {
        match self {
            Expr::Lin(l) => l.px + l.pct * base / 100.0,
            Expr::Add(a, b) => a.eval(base) + b.eval(base),
            Expr::Scale(a, k) => a.eval(base) * k,
            Expr::Min(v) => v.iter().map(|e| e.eval(base)).fold(f32::INFINITY, f32::min),
            Expr::Max(v) => v.iter().map(|e| e.eval(base)).fold(f32::NEG_INFINITY, f32::max),
            Expr::Clamp(v) => {
                let [lo, val, hi] = &**v;
                let lo = lo.eval(base);
                val.eval(base).min(hi.eval(base)).max(lo)
            }
        }
    }

    fn lin(&self) -> Option<Lin> {
        match self {
            Expr::Lin(l) => Some(*l),
            _ => None,
        }
    }

    fn add(a: Expr, b: Expr, k: f32) -> Expr {
        match (a.lin(), b.lin()) {
            (Some(x), Some(y)) => Expr::Lin(Lin {
                px: x.px + k * y.px,
                pct: x.pct + k * y.pct,
                num: x.num && y.num,
            }),
            _ => Expr::Add(Box::new(a), Box::new(Expr::scale(b, k))),
        }
    }

    fn scale(a: Expr, k: f32) -> Expr {
        match a {
            Expr::Lin(l) => Expr::Lin(Lin {
                px: l.px * k,
                pct: l.pct * k,
                num: l.num,
            }),
            _ if k == 1.0 => a,
            _ => Expr::Scale(Box::new(a), k),
        }
    }

    /// The value as a [`Dim`], interning what is not linear.
    fn dim(self) -> Dim {
        if let Expr::Lin(l) = self {
            return l.dim();
        }
        EXPRS.with(|e| {
            let mut e = e.borrow_mut();
            let key = format!("{self:?}");
            if let Some(&i) = e.index.get(&key) {
                return Dim::Expr(i);
            }
            let i = e.list.len() as u32;
            e.list.push(self);
            e.index.insert(key, i);
            Dim::Expr(i)
        })
    }
}

#[derive(Default)]
struct Exprs {
    list: Vec<Expr>,
    index: std::collections::HashMap<String, u32>,
}

thread_local! {
    /// Expressions behind [`Dim::Expr`]; equal ones share an index.
    static EXPRS: std::cell::RefCell<Exprs> = Default::default();
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
        let expr = Calc {
            s: s.as_bytes(),
            i: 0,
            fc,
        }
        .expr()?;
        return Some(expr.dim());
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
        // Container query units: approximated by the viewport.
        "vw" | "cqw" | "cqi" | "svw" | "lvw" | "dvw" => v * fc.vw / 100.0,
        "vh" | "cqh" | "cqb" | "svh" | "lvh" | "dvh" => v * fc.vh / 100.0,
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

    fn expr(&mut self) -> Option<Expr> {
        self.ws();
        let mut acc = self.term()?;
        loop {
            self.ws();
            match self.s.get(self.i) {
                Some(b'+') | Some(b'-') => {
                    let k = if self.s[self.i] == b'-' { -1.0 } else { 1.0 };
                    self.i += 1;
                    let t = self.term()?;
                    acc = Expr::add(acc, t, k);
                }
                _ => return Some(acc),
            }
        }
    }

    fn term(&mut self) -> Option<Expr> {
        let mut acc = self.factor()?;
        loop {
            self.ws();
            match self.s.get(self.i) {
                Some(b'*') => {
                    self.i += 1;
                    let f = self.factor()?;
                    let number = |e: &Expr| e.lin().filter(|l| l.num).map(|l| l.px);
                    acc = match (number(&acc), number(&f)) {
                        (Some(k), _) => Expr::scale(f, k),
                        (_, Some(k)) => Expr::scale(acc, k),
                        _ => return None,
                    };
                }
                Some(b'/') => {
                    self.i += 1;
                    let f = self.factor()?.lin().filter(|l| l.num && l.px != 0.0)?;
                    acc = Expr::scale(acc, 1.0 / f.px);
                }
                _ => return Some(acc),
            }
        }
    }

    fn factor(&mut self) -> Option<Expr> {
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
                return pick(kind, args);
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
        let lin = |px, pct, num| Some(Expr::Lin(Lin { px, pct, num }));
        if let Ok(n) = tok.parse::<f32>() {
            return lin(n, 0.0, true);
        }
        match dim(tok, self.fc)? {
            Dim::Px(px) => lin(px, 0.0, false),
            Dim::Percent(p) => lin(0.0, p, false),
            _ => None,
        }
    }
}

/// `calc()` (`kind` 0), `min()`, `max()` or `clamp()` of `args`: folded
/// to a linear value when the base cannot change the answer.
fn pick(kind: u8, mut args: Vec<Expr>) -> Option<Expr> {
    let lins: Option<Vec<Lin>> = args.iter().map(Expr::lin).collect();
    // All pixels, or all pure percentages (whose order no base changes).
    let foldable = lins.as_ref().filter(|l| {
        l.iter().all(|a| a.pct == 0.0) || l.iter().all(|a| a.px == 0.0)
    });
    let fold = |f: fn(f32, f32) -> f32, start: f32, l: &[Lin]| {
        let (px, pct) = l
            .iter()
            .fold((start, start), |(px, pct), a| (f(px, a.px), f(pct, a.pct)));
        let all_px = l.iter().all(|a| a.pct == 0.0);
        Expr::Lin(Lin {
            px: if all_px { px } else { 0.0 },
            pct: if all_px { 0.0 } else { pct },
            num: l.iter().all(|a| a.num),
        })
    };
    match kind {
        0 if args.len() == 1 => args.pop(),
        1 if !args.is_empty() => Some(match foldable {
            Some(l) => fold(f32::min, f32::INFINITY, l),
            None => Expr::Min(args),
        }),
        2 if !args.is_empty() => Some(match foldable {
            Some(l) => fold(f32::max, f32::NEG_INFINITY, l),
            None => Expr::Max(args),
        }),
        3 if args.len() == 3 => Some(match foldable {
            Some(l) => {
                let (lo, val, hi) = (l[0], l[1], l[2]);
                let c = |a: f32, v: f32, h: f32| v.min(h).max(a);
                Expr::Lin(Lin {
                    px: c(lo.px, val.px, hi.px),
                    pct: c(lo.pct, val.pct, hi.pct),
                    num: false,
                })
            }
            None => {
                let [a, b, c]: [Expr; 3] = args.try_into().ok()?;
                Expr::Clamp(Box::new([a, b, c]))
            }
        }),
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
        let min = dim("min(100%, 600px)", FC).unwrap();
        assert_eq!((min.px_or_zero(400.0), min.px_or_zero(1000.0)), (400.0, 600.0));
        let clamp = dim("clamp(10px, 50%, 40px)", FC).unwrap();
        assert_eq!((clamp.px_or_zero(10.0), clamp.px_or_zero(800.0)), (10.0, 40.0));
        assert_eq!(dim("min(30%, 50%)", FC), Some(Dim::Percent(30.0)));
        let nested = dim("calc(min(100%, 600px) - 2rem)", FC).unwrap();
        assert_eq!(nested.px_or_zero(400.0), 368.0);
        assert_eq!(dim("min(100%, 600px)", FC), Some(min));
        assert_eq!(dim("calc(1px +)", FC), None);
    }

    #[test]
    fn color_mix_premultiplies() {
        let c = |s| color(s, Rgba::BLACK);
        assert_eq!(c("color-mix(in srgb, red, blue)"), Some(Rgba::new(128, 0, 128, 255)));
        assert_eq!(c("color-mix(in srgb, red 20%, transparent)"), Some(Rgba::new(255, 0, 0, 51)));
        assert_eq!(c("color-mix(in srgb, white 25%, black)"), Some(Rgba::new(64, 64, 64, 255)));
        assert_eq!(c("color-mix(in srgb, red 30%, blue 30%)").map(|c| c.a), Some(153));
        assert!(c("color-mix(in oklab, red, blue)").is_some());
        assert_eq!(c("color-mix(red, blue)"), None);
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
