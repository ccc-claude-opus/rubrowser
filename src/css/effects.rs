//! `filter` functions and `clip-path` basic shapes (CSS Filter Effects 1,
//! CSS Masking 1).

use super::gradient::split_commas;
use super::values::{self, Dim, FontContext, Rgba};

/// One `filter` function. Amounts are fractions (`50%` is 0.5).
#[derive(Clone, Debug, PartialEq)]
pub enum Filter {
    Blur(f32),
    Brightness(f32),
    Contrast(f32),
    Grayscale(f32),
    /// Radians.
    HueRotate(f32),
    Invert(f32),
    Opacity(f32),
    Saturate(f32),
    Sepia(f32),
    DropShadow {
        x: f32,
        y: f32,
        blur: f32,
        color: Rgba,
    },
}

/// Parses a `filter` value; `None` for `none` or an invalid list (which
/// the cascade ignores). `url()` references are not supported and make
/// the whole value invalid, as for a filter that does not exist.
pub fn parse_filter(v: &str, fc: FontContext, color: Rgba) -> Option<Vec<Filter>> {
    let v = v.trim();
    if v.eq_ignore_ascii_case("none") {
        return Some(Vec::new());
    }
    let mut out = Vec::new();
    for f in values::tokens(v) {
        let open = f.find('(')?;
        let name = f[..open].trim().to_ascii_lowercase();
        let args = f[open + 1..].strip_suffix(')')?.trim();
        // `50%` or `0.5`; empty gives the default.
        let amount = |default: f32| -> Option<f32> {
            if args.is_empty() {
                return Some(default);
            }
            match args.strip_suffix('%') {
                Some(p) => p.trim().parse::<f32>().ok().map(|p| p / 100.0),
                None => args.parse::<f32>().ok(),
            }
            .filter(|a| *a >= 0.0)
        };
        // Amounts above 1 are clamped for these.
        let unit = |default: f32| amount(default).map(|a| a.min(1.0));
        out.push(match name.as_str() {
            "blur" => Filter::Blur(if args.is_empty() {
                0.0
            } else {
                values::px(args, fc).filter(|b| *b >= 0.0)?
            }),
            "brightness" => Filter::Brightness(amount(1.0)?),
            "contrast" => Filter::Contrast(amount(1.0)?),
            "grayscale" => Filter::Grayscale(unit(1.0)?),
            "hue-rotate" => Filter::HueRotate(if args.is_empty() {
                0.0
            } else {
                super::transform::angle(args)?
            }),
            "invert" => Filter::Invert(unit(1.0)?),
            "opacity" => Filter::Opacity(unit(1.0)?),
            "saturate" => Filter::Saturate(amount(1.0)?),
            "sepia" => Filter::Sepia(unit(1.0)?),
            "drop-shadow" => {
                let mut lens = Vec::new();
                let mut c = color;
                for t in values::tokens(args) {
                    if let Some(px) = values::px(t, fc) {
                        lens.push(px);
                    } else {
                        c = values::color(t, color)?;
                    }
                }
                if !(2..=3).contains(&lens.len()) {
                    return None;
                }
                Filter::DropShadow {
                    x: lens[0],
                    y: lens[1],
                    blur: lens.get(2).copied().unwrap_or(0.0).max(0.0),
                    color: c,
                }
            }
            _ => return None,
        });
    }
    Some(out)
}

/// A 4 × 5 color matrix over straight (unpremultiplied) RGBA in 0..1, as
/// the filter functions are defined.
pub type ColorMatrix = [[f32; 5]; 4];

impl Filter {
    /// The color matrix of a filter that only changes colors.
    pub fn matrix(&self) -> Option<ColorMatrix> {
        let id = |r: [f32; 3], g: [f32; 3], b: [f32; 3]| -> ColorMatrix {
            [
                [r[0], r[1], r[2], 0.0, 0.0],
                [g[0], g[1], g[2], 0.0, 0.0],
                [b[0], b[1], b[2], 0.0, 0.0],
                [0.0, 0.0, 0.0, 1.0, 0.0],
            ]
        };
        Some(match *self {
            Filter::Brightness(a) => id([a, 0.0, 0.0], [0.0, a, 0.0], [0.0, 0.0, a]),
            Filter::Contrast(a) => {
                let o = 0.5 - 0.5 * a;
                [
                    [a, 0.0, 0.0, 0.0, o],
                    [0.0, a, 0.0, 0.0, o],
                    [0.0, 0.0, a, 0.0, o],
                    [0.0, 0.0, 0.0, 1.0, 0.0],
                ]
            }
            Filter::Grayscale(a) => {
                let k = 1.0 - a;
                id(
                    [
                        0.2126 + 0.7874 * k,
                        0.7152 - 0.7152 * k,
                        0.0722 - 0.0722 * k,
                    ],
                    [
                        0.2126 - 0.2126 * k,
                        0.7152 + 0.2848 * k,
                        0.0722 - 0.0722 * k,
                    ],
                    [
                        0.2126 - 0.2126 * k,
                        0.7152 - 0.7152 * k,
                        0.0722 + 0.9278 * k,
                    ],
                )
            }
            Filter::Sepia(a) => {
                let k = 1.0 - a;
                id(
                    [0.393 + 0.607 * k, 0.769 - 0.769 * k, 0.189 - 0.189 * k],
                    [0.349 - 0.349 * k, 0.686 + 0.314 * k, 0.168 - 0.168 * k],
                    [0.272 - 0.272 * k, 0.534 - 0.534 * k, 0.131 + 0.869 * k],
                )
            }
            Filter::Saturate(s) => id(
                [0.213 + 0.787 * s, 0.715 - 0.715 * s, 0.072 - 0.072 * s],
                [0.213 - 0.213 * s, 0.715 + 0.285 * s, 0.072 - 0.072 * s],
                [0.213 - 0.213 * s, 0.715 - 0.715 * s, 0.072 + 0.928 * s],
            ),
            Filter::HueRotate(a) => {
                let (c, s) = (a.cos(), a.sin());
                id(
                    [
                        0.213 + c * 0.787 - s * 0.213,
                        0.715 - c * 0.715 - s * 0.715,
                        0.072 - c * 0.072 + s * 0.928,
                    ],
                    [
                        0.213 - c * 0.213 + s * 0.143,
                        0.715 + c * 0.285 + s * 0.140,
                        0.072 - c * 0.072 - s * 0.283,
                    ],
                    [
                        0.213 - c * 0.213 - s * 0.787,
                        0.715 - c * 0.715 + s * 0.715,
                        0.072 + c * 0.928 + s * 0.072,
                    ],
                )
            }
            Filter::Invert(a) => {
                let k = 1.0 - 2.0 * a;
                [
                    [k, 0.0, 0.0, 0.0, a],
                    [0.0, k, 0.0, 0.0, a],
                    [0.0, 0.0, k, 0.0, a],
                    [0.0, 0.0, 0.0, 1.0, 0.0],
                ]
            }
            Filter::Opacity(a) => [
                [1.0, 0.0, 0.0, 0.0, 0.0],
                [0.0, 1.0, 0.0, 0.0, 0.0],
                [0.0, 0.0, 1.0, 0.0, 0.0],
                [0.0, 0.0, 0.0, a, 0.0],
            ],
            Filter::Blur(_) | Filter::DropShadow { .. } => return None,
        })
    }

    /// How far (CSS px) the filter can spread paint beyond the element.
    pub fn outset(&self) -> f32 {
        match *self {
            Filter::Blur(b) => b * 3.0,
            Filter::DropShadow { x, y, blur, .. } => x.abs().max(y.abs()) + blur * 3.0,
            _ => 0.0,
        }
    }
}

/// A `clip-path` basic shape, in its reference box (the border box).
#[derive(Clone, Debug, PartialEq)]
pub enum ClipPath {
    /// Top, right, bottom, left insets and a corner radius.
    Inset([Dim; 4], Dim),
    /// Radius (`None`: `closest-side`), center.
    Circle(Option<Dim>, (Dim, Dim)),
    Ellipse(Option<(Dim, Dim)>, (Dim, Dim)),
    Polygon(Vec<(Dim, Dim)>),
}

/// Parses a `clip-path` value (`none` and unsupported values give `None`).
pub fn parse_clip_path(v: &str, fc: FontContext) -> Option<ClipPath> {
    let v = v.trim();
    let open = v.find('(')?;
    let name = v[..open].trim().to_ascii_lowercase();
    let close = v.rfind(')')?;
    let args = v[open + 1..close].trim();
    let len = |t: &str| values::dim(t, fc).filter(|d| *d != Dim::Auto);
    // `at <position>`: keywords or lengths.
    let position = |s: &str| -> Option<(Dim, Dim)> {
        let t = values::tokens(s);
        let word = |w: &str| match w.to_ascii_lowercase().as_str() {
            "left" | "top" => Some(Dim::Percent(0.0)),
            "center" => Some(Dim::Percent(50.0)),
            "right" | "bottom" => Some(Dim::Percent(100.0)),
            _ => len(w),
        };
        match t.as_slice() {
            [] => Some((Dim::Percent(50.0), Dim::Percent(50.0))),
            [a] => {
                let d = word(a)?;
                if a.eq_ignore_ascii_case("top") || a.eq_ignore_ascii_case("bottom") {
                    Some((Dim::Percent(50.0), d))
                } else {
                    Some((d, Dim::Percent(50.0)))
                }
            }
            [a, b] => {
                let (a, b) = if a.eq_ignore_ascii_case("top") || a.eq_ignore_ascii_case("bottom") {
                    (b, a)
                } else {
                    (a, b)
                };
                Some((word(a)?, word(b)?))
            }
            _ => None,
        }
    };
    let split_at = |s: &str| -> (String, String) {
        let t = values::tokens(s);
        match t.iter().position(|w| w.eq_ignore_ascii_case("at")) {
            Some(i) => (t[..i].join(" "), t[i + 1..].join(" ")),
            None => (t.join(" "), String::new()),
        }
    };
    match name.as_str() {
        "inset" => {
            let t = values::tokens(args);
            let r = t.iter().position(|w| w.eq_ignore_ascii_case("round"));
            let sides: Vec<Dim> = t[..r.unwrap_or(t.len())]
                .iter()
                .map(|w| len(w))
                .collect::<Option<_>>()?;
            let radius = r
                .and_then(|i| t.get(i + 1))
                .and_then(|w| len(w))
                .unwrap_or(Dim::Px(0.0));
            let s = |i: usize| sides[i.min(sides.len() - 1)];
            let sides = match sides.len() {
                1 => [s(0); 4],
                2 => [s(0), s(1), s(0), s(1)],
                3 => [s(0), s(1), s(2), s(1)],
                4 => [s(0), s(1), s(2), s(3)],
                _ => return None,
            };
            Some(ClipPath::Inset(sides, radius))
        }
        "circle" => {
            let (r, at) = split_at(args);
            let r = match r.to_ascii_lowercase().as_str() {
                "" | "closest-side" => None,
                "farthest-side" => Some(Dim::Percent(f32::INFINITY)),
                r => Some(len(r)?),
            };
            Some(ClipPath::Circle(r, position(&at)?))
        }
        "ellipse" => {
            let (r, at) = split_at(args);
            let t = values::tokens(&r);
            let radii = match t.as_slice() {
                [] => None,
                [a, b] => Some((len(a)?, len(b)?)),
                _ => return None,
            };
            Some(ClipPath::Ellipse(radii, position(&at)?))
        }
        "polygon" => {
            let mut pts = Vec::new();
            for (i, p) in split_commas(args).into_iter().enumerate() {
                let t = values::tokens(p);
                // A leading fill rule.
                if i == 0 && t.len() == 1 && matches!(t[0], "nonzero" | "evenodd") {
                    continue;
                }
                let [x, y] = t.as_slice() else { return None };
                pts.push((len(x)?, len(y)?));
            }
            (pts.len() >= 3).then_some(ClipPath::Polygon(pts))
        }
        _ => None,
    }
}

impl ClipPath {
    /// The shape for a `w × h` reference box, as a coverage test: whether
    /// point `(x, y)` (relative to the box) is inside.
    pub fn resolve(&self, w: f32, h: f32) -> Shape {
        let diag = (w * w + h * h).sqrt() / std::f32::consts::SQRT_2;
        match self {
            ClipPath::Inset(s, r) => Shape::Rect {
                x0: s[3].px_or_zero(w),
                y0: s[0].px_or_zero(h),
                x1: w - s[1].px_or_zero(w),
                y1: h - s[2].px_or_zero(h),
                r: r.px_or_zero(w),
            },
            ClipPath::Circle(r, (cx, cy)) => {
                let (cx, cy) = (cx.px_or_zero(w), cy.px_or_zero(h));
                let r = match r {
                    Some(Dim::Percent(p)) if p.is_infinite() => cx.max(w - cx).max(cy.max(h - cy)),
                    Some(r) => r.px_or_zero(diag),
                    None => cx.min(w - cx).min(cy.min(h - cy)),
                };
                Shape::Ellipse {
                    cx,
                    cy,
                    rx: r,
                    ry: r,
                }
            }
            ClipPath::Ellipse(radii, (cx, cy)) => {
                let (cx, cy) = (cx.px_or_zero(w), cy.px_or_zero(h));
                let (rx, ry) = match radii {
                    Some((a, b)) => (a.px_or_zero(w), b.px_or_zero(h)),
                    None => (cx.min(w - cx), cy.min(h - cy)),
                };
                Shape::Ellipse { cx, cy, rx, ry }
            }
            ClipPath::Polygon(p) => Shape::Polygon(
                p.iter()
                    .map(|(x, y)| (x.px_or_zero(w), y.px_or_zero(h)))
                    .collect(),
            ),
        }
    }
}

/// A resolved clip shape in CSS px from the reference box's top left.
#[derive(Clone, Debug, PartialEq)]
pub enum Shape {
    Rect {
        x0: f32,
        y0: f32,
        x1: f32,
        y1: f32,
        r: f32,
    },
    Ellipse {
        cx: f32,
        cy: f32,
        rx: f32,
        ry: f32,
    },
    Polygon(Vec<(f32, f32)>),
}

impl Shape {
    pub fn contains(&self, x: f32, y: f32) -> bool {
        match self {
            Shape::Rect { x0, y0, x1, y1, r } => {
                if x < *x0 || x >= *x1 || y < *y0 || y >= *y1 {
                    return false;
                }
                let r = r.min((x1 - x0) / 2.0).min((y1 - y0) / 2.0);
                let cx = x.clamp(x0 + r, x1 - r);
                let cy = y.clamp(y0 + r, y1 - r);
                (x - cx).powi(2) + (y - cy).powi(2) <= r * r
            }
            Shape::Ellipse { cx, cy, rx, ry } => {
                *rx > 0.0 && *ry > 0.0 && ((x - cx) / rx).powi(2) + ((y - cy) / ry).powi(2) <= 1.0
            }
            Shape::Polygon(p) => {
                // Nonzero winding.
                let mut winding = 0;
                for i in 0..p.len() {
                    let (a, b) = (p[i], p[(i + 1) % p.len()]);
                    let side = (b.0 - a.0) * (y - a.1) - (x - a.0) * (b.1 - a.1);
                    if a.1 <= y {
                        if b.1 > y && side > 0.0 {
                            winding += 1;
                        }
                    } else if b.1 <= y && side < 0.0 {
                        winding -= 1;
                    }
                }
                winding != 0
            }
        }
    }

    /// Coverage (0..=1) of a 1 × 1 physical pixel at `(x, y)` (its top
    /// left, CSS px) at scale `s`, from 4 × 4 samples.
    pub fn coverage(&self, x: f32, y: f32, s: f32) -> f32 {
        let mut n = 0;
        for j in 0..4 {
            for i in 0..4 {
                let (px, py) = (
                    x + (i as f32 + 0.5) / 4.0 / s,
                    y + (j as f32 + 0.5) / 4.0 / s,
                );
                n += self.contains(px, py) as u32;
            }
        }
        n as f32 / 16.0
    }
}

/// Blurs premultiplied RGBA pixels (`w × h`) with a Gaussian of standard
/// deviation `sigma` (physical px), approximated by three box blurs.
pub fn blur(px: &mut [[f32; 4]], w: usize, h: usize, sigma: f32) {
    if sigma < 0.3 || w == 0 || h == 0 {
        return;
    }
    // Box width d with 3·(d²−1)/12 = σ².
    let d = (4.0 * sigma * sigma + 1.0).sqrt();
    let r = ((d - 1.0) / 2.0).round().max(1.0) as usize;
    let mut tmp = vec![[0.0f32; 4]; w.max(h)];
    for _ in 0..3 {
        for y in 0..h {
            box_line(px, y * w, 1, w, r, &mut tmp);
        }
        for x in 0..w {
            box_line(px, x, w, h, r, &mut tmp);
        }
    }
}

/// One box-blur pass over `n` pixels starting at `start`, `step` apart.
fn box_line(
    px: &mut [[f32; 4]],
    start: usize,
    step: usize,
    n: usize,
    r: usize,
    tmp: &mut [[f32; 4]],
) {
    let norm = 1.0 / (2 * r + 1) as f32;
    let at = |i: isize| -> [f32; 4] {
        if i < 0 || i as usize >= n {
            [0.0; 4]
        } else {
            px[start + i as usize * step]
        }
    };
    let mut sum = [0.0f32; 4];
    for i in -(r as isize)..=(r as isize) {
        let p = at(i);
        for k in 0..4 {
            sum[k] += p[k];
        }
    }
    for (i, t) in tmp.iter_mut().enumerate().take(n) {
        *t = sum.map(|v| v * norm);
        let (add, sub) = (at(i as isize + r as isize + 1), at(i as isize - r as isize));
        for k in 0..4 {
            sum[k] += add[k] - sub[k];
        }
    }
    for (i, t) in tmp.iter().enumerate().take(n) {
        px[start + i * step] = *t;
    }
}

/// Parses a `text-shadow` list (offsets, optional blur, optional color).
pub fn parse_text_shadow(
    v: &str,
    fc: FontContext,
    color: Rgba,
) -> Option<Vec<super::style::Shadow>> {
    if v.trim().eq_ignore_ascii_case("none") {
        return Some(Vec::new());
    }
    let mut out = Vec::new();
    for part in split_commas(v) {
        let mut lens = Vec::new();
        let mut c = color;
        for t in values::tokens(part) {
            if let Some(px) = values::px(t, fc) {
                lens.push(px);
            } else {
                c = values::color(t, color)?;
            }
        }
        if !(2..=3).contains(&lens.len()) {
            return None;
        }
        out.push(super::style::Shadow {
            x: lens[0],
            y: lens[1],
            blur: lens.get(2).copied().unwrap_or(0.0).max(0.0),
            spread: 0.0,
            color: c,
            inset: false,
        });
    }
    Some(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fc() -> FontContext {
        FontContext {
            em: 16.0,
            rem: 16.0,
            vw: 1000.0,
            vh: 800.0,
        }
    }

    #[test]
    fn parses_filters() {
        let f = parse_filter(
            "blur(2px) grayscale(50%) drop-shadow(1px 2px 3px red)",
            fc(),
            Rgba::BLACK,
        )
        .unwrap();
        assert_eq!(f.len(), 3);
        assert_eq!(f[0], Filter::Blur(2.0));
        assert_eq!(f[1], Filter::Grayscale(0.5));
        assert!(matches!(
            f[2],
            Filter::DropShadow {
                x: 1.0,
                y: 2.0,
                blur: 3.0,
                ..
            }
        ));
        assert!(parse_filter("url(#f)", fc(), Rgba::BLACK).is_none());
        assert_eq!(parse_filter("none", fc(), Rgba::BLACK), Some(vec![]));
        // Invert(1) maps white to black.
        let m = Filter::Invert(1.0).matrix().unwrap();
        assert_eq!(m[0][0] + m[0][4], 0.0);
    }

    #[test]
    fn clip_shapes() {
        let c = parse_clip_path("circle(50% at 50% 50%)", fc())
            .unwrap()
            .resolve(100.0, 100.0);
        assert!(c.contains(50.0, 50.0) && !c.contains(2.0, 2.0));
        let p = parse_clip_path("polygon(50% 0, 100% 100%, 0 100%)", fc())
            .unwrap()
            .resolve(100.0, 100.0);
        assert!(p.contains(50.0, 60.0) && !p.contains(5.0, 5.0));
        let i = parse_clip_path("inset(10px 20%)", fc())
            .unwrap()
            .resolve(100.0, 100.0);
        assert!(i.contains(25.0, 15.0) && !i.contains(15.0, 15.0));
        let e = parse_clip_path("ellipse(at left)", fc())
            .unwrap()
            .resolve(100.0, 50.0);
        assert!(!e.contains(50.0, 25.0));
    }
}
