//! CSS Transforms: `transform` functions, the `rotate` / `scale` properties
//! and the 2D matrix they make around `transform-origin`.

use super::values::{self, Dim};

/// One transform function. Lengths stay [`Dim`]s until the box size is
/// known (percentages of `translate` refer to the border box).
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Op {
    Translate(Dim, Dim),
    Scale(f32, f32),
    /// Radians, clockwise (y points down).
    Rotate(f32),
    Skew(f32, f32),
    Matrix([f32; 6]),
}

/// A 2D affine matrix `[a b c d e f]`: `x' = a x + c y + e`,
/// `y' = b x + d y + f` (the order of CSS `matrix()`).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Affine(pub [f32; 6]);

impl Affine {
    pub const IDENTITY: Affine = Affine([1.0, 0.0, 0.0, 1.0, 0.0, 0.0]);

    pub fn translate(x: f32, y: f32) -> Affine {
        Affine([1.0, 0.0, 0.0, 1.0, x, y])
    }

    /// `self` then `o`'s coordinate system: the matrix product `self × o`.
    pub fn then(&self, o: &Affine) -> Affine {
        let [a, b, c, d, e, f] = self.0;
        let [a2, b2, c2, d2, e2, f2] = o.0;
        Affine([
            a * a2 + c * b2,
            b * a2 + d * b2,
            a * c2 + c * d2,
            b * c2 + d * d2,
            a * e2 + c * f2 + e,
            b * e2 + d * f2 + f,
        ])
    }

    pub fn apply(&self, x: f32, y: f32) -> (f32, f32) {
        let [a, b, c, d, e, f] = self.0;
        (a * x + c * y + e, b * x + d * y + f)
    }

    pub fn invert(&self) -> Option<Affine> {
        let [a, b, c, d, e, f] = self.0;
        let det = a * d - b * c;
        if det.abs() < 1e-9 {
            return None;
        }
        let (ia, ib, ic, id) = (d / det, -b / det, -c / det, a / det);
        Some(Affine([ia, ib, ic, id, -(ia * e + ic * f), -(ib * e + id * f)]))
    }

    /// Only moves (no scaling, rotation or skew).
    pub fn is_translation(&self) -> bool {
        let [a, b, c, d, ..] = self.0;
        a == 1.0 && b == 0.0 && c == 0.0 && d == 1.0
    }
}

impl Op {
    fn matrix(&self, w: f32, h: f32) -> Affine {
        match *self {
            Op::Translate(x, y) => Affine::translate(x.px_or_zero(w), y.px_or_zero(h)),
            Op::Scale(x, y) => Affine([x, 0.0, 0.0, y, 0.0, 0.0]),
            Op::Rotate(t) => {
                let (s, c) = t.sin_cos();
                Affine([c, s, -s, c, 0.0, 0.0])
            }
            Op::Skew(x, y) => Affine([1.0, y.tan(), x.tan(), 1.0, 0.0, 0.0]),
            Op::Matrix(m) => Affine(m),
        }
    }
}

/// An angle in radians: `deg`, `rad`, `grad`, `turn`, or a bare `0`.
pub fn angle(s: &str) -> Option<f32> {
    let s = s.trim().to_ascii_lowercase();
    let num = |n: &str| n.trim().parse::<f32>().ok();
    if let Some(n) = s.strip_suffix("deg") {
        num(n).map(f32::to_radians)
    } else if let Some(n) = s.strip_suffix("grad") {
        num(n).map(|g| g * std::f32::consts::PI / 200.0)
    } else if let Some(n) = s.strip_suffix("rad") {
        num(n)
    } else if let Some(n) = s.strip_suffix("turn") {
        num(n).map(|t| t * std::f32::consts::TAU)
    } else {
        num(&s).filter(|&n| n == 0.0)
    }
}

/// Parses a `transform` value; `None` for `none` or an invalid list (which
/// browsers ignore as a whole).
pub fn parse(value: &str, dim: impl Fn(&str) -> Option<Dim>) -> Option<Vec<Op>> {
    let mut ops = Vec::new();
    for t in values::tokens(value) {
        let lower = t.to_ascii_lowercase();
        if lower == "none" {
            return None;
        }
        let (name, args) = lower.split_once('(')?;
        let args: Vec<&str> = args
            .trim_end_matches(')')
            .split(',')
            .flat_map(|a| a.split_whitespace())
            .filter(|a| !a.is_empty())
            .collect();
        let len = |i: usize| args.get(i).map_or(Some(Dim::Px(0.0)), |a| dim(a));
        let num = |i: usize| args.get(i).and_then(|a| a.parse::<f32>().ok());
        let ang = |i: usize| args.get(i).map_or(Some(0.0), |a| angle(a));
        let op = match name {
            "translate" | "translate3d" => Op::Translate(len(0)?, len(1)?),
            "translatex" => Op::Translate(len(0)?, Dim::Px(0.0)),
            "translatey" => Op::Translate(Dim::Px(0.0), len(0)?),
            "translatez" | "perspective" => continue,
            "scale" | "scale3d" => {
                let x = num(0)?;
                Op::Scale(x, num(1).unwrap_or(x))
            }
            "scalex" => Op::Scale(num(0)?, 1.0),
            "scaley" => Op::Scale(1.0, num(0)?),
            "scalez" => continue,
            "rotate" | "rotatez" => Op::Rotate(ang(0)?),
            // Without perspective, a turn around the x / y axis squashes the
            // element along the other one.
            "rotatex" => Op::Scale(1.0, ang(0)?.cos()),
            "rotatey" => Op::Scale(ang(0)?.cos(), 1.0),
            "skew" => Op::Skew(ang(0)?, ang(1)?),
            "skewx" => Op::Skew(ang(0)?, 0.0),
            "skewy" => Op::Skew(0.0, ang(0)?),
            "matrix" => {
                let m: Vec<f32> = (0..6).map(num).collect::<Option<_>>()?;
                Op::Matrix([m[0], m[1], m[2], m[3], m[4], m[5]])
            }
            "matrix3d" => {
                let m: Vec<f32> = (0..16).map(num).collect::<Option<_>>()?;
                Op::Matrix([m[0], m[1], m[4], m[5], m[12], m[13]])
            }
            _ => return None,
        };
        ops.push(op);
    }
    Some(ops)
}

/// The `rotate` property: `none`, an angle, or `z <angle>`.
pub fn parse_rotate(value: &str) -> Option<f32> {
    let v = value.trim().to_ascii_lowercase();
    if v == "none" {
        return Some(0.0);
    }
    angle(v.strip_prefix('z').unwrap_or(&v))
}

/// The `scale` property: `none`, or one or two numbers / percentages.
pub fn parse_scale(value: &str) -> Option<(f32, f32)> {
    let v = value.trim();
    if v.eq_ignore_ascii_case("none") {
        return Some((1.0, 1.0));
    }
    let n = |s: &str| match s.strip_suffix('%') {
        Some(p) => p.parse::<f32>().ok().map(|p| p / 100.0),
        None => s.parse::<f32>().ok(),
    };
    let parts: Vec<f32> = v.split_whitespace().map(n).collect::<Option<_>>()?;
    match parts[..] {
        [x] => Some((x, x)),
        [x, y, ..] => Some((x, y)),
        _ => None,
    }
}

/// The matrix of a `w × h` border box: the `translate`, `rotate` and
/// `scale` properties, then `transform`, all around `origin` (in px from the
/// box's top left).
pub fn matrix(
    translate: (Dim, Dim),
    rotate: f32,
    scale: (f32, f32),
    ops: &[Op],
    origin: (f32, f32),
    (w, h): (f32, f32),
) -> Affine {
    let mut m = Affine::translate(origin.0, origin.1);
    m = m.then(&Op::Translate(translate.0, translate.1).matrix(w, h));
    m = m.then(&Op::Rotate(rotate).matrix(w, h));
    m = m.then(&Op::Scale(scale.0, scale.1).matrix(w, h));
    for op in ops {
        m = m.then(&op.matrix(w, h));
    }
    m.then(&Affine::translate(-origin.0, -origin.1))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn px(s: &str) -> Option<Dim> {
        s.strip_suffix("px")?.parse().ok().map(Dim::Px)
    }

    #[test]
    fn parses_and_composes() {
        let ops = parse("translateX(10px) rotate(90deg) scale(2)", px).unwrap();
        assert_eq!(ops.len(), 3);
        let m = matrix((Dim::Px(0.0), Dim::Px(0.0)), 0.0, (1.0, 1.0), &ops, (0.0, 0.0), (10.0, 10.0));
        let (x, y) = m.apply(1.0, 0.0);
        assert!((x - 10.0).abs() < 1e-4 && (y - 2.0).abs() < 1e-4, "{x} {y}");
        assert!(parse("none", px).is_none());
        assert!(parse("wobble(3)", px).is_none());
        assert!((angle("0.5turn").unwrap() - std::f32::consts::PI).abs() < 1e-6);
        assert_eq!(parse_scale("50%"), Some((0.5, 0.5)));
        // Rotating 180° around the center maps the corners onto each other.
        let r = matrix((Dim::Px(0.0), Dim::Px(0.0)), angle("180deg").unwrap(), (1.0, 1.0), &[], (5.0, 5.0), (10.0, 10.0));
        let (x, y) = r.apply(0.0, 0.0);
        assert!((x - 10.0).abs() < 1e-4 && (y - 10.0).abs() < 1e-4);
        let inv = r.invert().unwrap();
        let (x, y) = inv.apply(10.0, 10.0);
        assert!(x.abs() < 1e-4 && y.abs() < 1e-4);
    }
}
