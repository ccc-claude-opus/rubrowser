//! CSS gradients: `linear-gradient()`, `radial-gradient()` and their
//! `repeating-` forms, parsed into [`Gradient`] and rendered to pixels.

use super::values::{self, Dim, FontContext, Rgba};

/// A parsed gradient image.
#[derive(Clone, Debug, PartialEq)]
pub struct Gradient {
    /// The CSS text (cache key for rendered pixels).
    pub key: String,
    pub kind: Kind,
    pub repeating: bool,
    /// Colors with optional positions along the gradient line.
    pub stops: Vec<(Rgba, Option<Dim>)>,
}

#[derive(Clone, Debug, PartialEq)]
pub enum Kind {
    /// Direction: an angle in degrees (0 = to top, 90 = to right), or a
    /// corner as (x, y) signs (`to top right` = (1, -1)).
    Linear {
        angle: Option<f32>,
        corner: Option<(f32, f32)>,
    },
    Radial {
        circle: bool,
        /// `closest-side` etc.; farthest-corner when false.
        closest: bool,
        /// Explicit radii (`circle 40px`, `ellipse 50% 20%`).
        size: Option<(Dim, Dim)>,
        at: (Dim, Dim),
    },
}

/// Splits on commas outside parentheses.
pub fn split_commas(s: &str) -> Vec<&str> {
    split_args(s)
}

fn split_args(s: &str) -> Vec<&str> {
    let mut out = Vec::new();
    let (mut depth, mut start) = (0, 0);
    for (i, c) in s.char_indices() {
        match c {
            '(' => depth += 1,
            ')' => depth -= 1,
            ',' if depth == 0 => {
                out.push(s[start..i].trim());
                start = i + 1;
            }
            _ => {}
        }
    }
    out.push(s[start..].trim());
    out
}

fn position(words: &[&str], fc: FontContext) -> (Dim, Dim) {
    let kw = |w: &str| match w {
        "left" | "top" => Some(Dim::Percent(0.0)),
        "center" => Some(Dim::Percent(50.0)),
        "right" | "bottom" => Some(Dim::Percent(100.0)),
        _ => None,
    };
    let center = Dim::Percent(50.0);
    let val = |w: &str| kw(w).or_else(|| values::dim(w, fc));
    match words {
        [] => (center, center),
        [one] => match *one {
            "top" | "bottom" => (center, val(one).unwrap_or(center)),
            _ => (val(one).unwrap_or(center), center),
        },
        [a, b, ..] => {
            // `top left` is allowed too.
            if matches!(*a, "top" | "bottom") || matches!(*b, "left" | "right") {
                (val(b).unwrap_or(center), val(a).unwrap_or(center))
            } else {
                (val(a).unwrap_or(center), val(b).unwrap_or(center))
            }
        }
    }
}

fn angle(s: &str) -> Option<f32> {
    let s = s.trim();
    let num = |suffix: &str| {
        s.strip_suffix(suffix)
            .and_then(|n| n.trim().parse::<f32>().ok())
    };
    num("deg")
        .or_else(|| num("grad").map(|g| g * 0.9))
        .or_else(|| num("rad").map(f32::to_degrees))
        .or_else(|| num("turn").map(|t| t * 360.0))
        .or_else(|| (s == "0").then_some(0.0))
}

/// Parses a gradient function, or `None` if `v` is not one.
pub fn parse(v: &str, current: Rgba, fc: FontContext) -> Option<Gradient> {
    let v = v.trim();
    let lower = v.to_ascii_lowercase();
    let (name, rest) = lower.split_once('(')?;
    let name = name.trim().trim_start_matches("-webkit-");
    let (repeating, name) = match name.strip_prefix("repeating-") {
        Some(n) => (true, n),
        None => (false, name),
    };
    let body = rest.trim_end().strip_suffix(')')?;
    let mut args = split_args(body);
    let kind = match name {
        "linear-gradient" => {
            let first = args.first().copied().unwrap_or("");
            let mut kind = Kind::Linear {
                angle: Some(180.0),
                corner: None,
            };
            if let Some(dir) = first.strip_prefix("to ") {
                let words: Vec<&str> = dir.split_whitespace().collect();
                let has = |w: &str| words.contains(&w);
                let x = if has("left") {
                    -1.0
                } else if has("right") {
                    1.0
                } else {
                    0.0
                };
                let y = if has("top") {
                    -1.0
                } else if has("bottom") {
                    1.0
                } else {
                    0.0
                };
                kind = if x != 0.0 && y != 0.0 {
                    Kind::Linear {
                        angle: None,
                        corner: Some((x, y)),
                    }
                } else {
                    Kind::Linear {
                        angle: Some(match (x as i32, y as i32) {
                            (0, -1) => 0.0,
                            (1, 0) => 90.0,
                            (-1, 0) => 270.0,
                            _ => 180.0,
                        }),
                        corner: None,
                    }
                };
                args.remove(0);
            } else if let Some(a) = angle(first) {
                kind = Kind::Linear {
                    angle: Some(a),
                    corner: None,
                };
                args.remove(0);
            }
            kind
        }
        "radial-gradient" => {
            let first = args.first().copied().unwrap_or("");
            let shape_words = [
                "circle",
                "ellipse",
                "closest-side",
                "closest-corner",
                "farthest-side",
                "farthest-corner",
                "at",
            ];
            let words: Vec<&str> = first.split_whitespace().collect();
            let is_shape = words.first().is_some_and(|w| {
                shape_words.contains(w)
                    || (values::dim(w, fc).is_some()
                        && words.len() > 1
                        && values::color(first, current).is_none())
            });
            let mut kind = Kind::Radial {
                circle: false,
                closest: false,
                size: None,
                at: (Dim::Percent(50.0), Dim::Percent(50.0)),
            };
            if is_shape {
                let (shape, at) = match first.split_once(" at ") {
                    Some((s, a)) => (s, Some(a)),
                    None if first.starts_with("at ") => ("", Some(&first[3..])),
                    None => (first, None),
                };
                let sw: Vec<&str> = shape.split_whitespace().collect();
                let lengths: Vec<Dim> = sw.iter().filter_map(|w| values::dim(w, fc)).collect();
                let circle =
                    sw.contains(&"circle") || (lengths.len() == 1 && !sw.contains(&"ellipse"));
                kind = Kind::Radial {
                    circle,
                    closest: sw.iter().any(|w| w.starts_with("closest")),
                    size: match lengths.as_slice() {
                        [r] => Some((*r, *r)),
                        [a, b, ..] => Some((*a, *b)),
                        [] => None,
                    },
                    at: position(
                        &at.map(|a| a.split_whitespace().collect::<Vec<_>>())
                            .unwrap_or_default(),
                        fc,
                    ),
                };
                args.remove(0);
            }
            kind
        }
        _ => return None,
    };
    // The original text keeps colors' case-insensitive names fine, but
    // color functions are case-insensitive too, so the lowercase is used.
    let mut stops = Vec::new();
    for a in args {
        let toks = values::tokens(a);
        let Some(color) = toks.first().and_then(|t| values::color(t, current)) else {
            // A color hint (bare position) or something unsupported.
            if toks.len() == 1 && values::dim(toks[0], fc).is_some() {
                continue;
            }
            return None;
        };
        let positions: Vec<Dim> = toks[1..]
            .iter()
            .filter_map(|t| values::dim(t, fc))
            .collect();
        if positions.is_empty() {
            stops.push((color, None));
        }
        for p in positions {
            stops.push((color, Some(p)));
        }
    }
    if stops.is_empty() {
        return None;
    }
    if stops.len() == 1 {
        stops.push(stops[0]);
    }
    Some(Gradient {
        key: v.to_string(),
        kind,
        repeating,
        stops,
    })
}

/// Premultiplied RGBA as floats.
fn premul(c: Rgba) -> [f32; 4] {
    let a = c.a as f32 / 255.0;
    [c.r as f32 * a, c.g as f32 * a, c.b as f32 * a, a]
}

impl Gradient {
    /// Renders the gradient at `w × h` pixels (`0xAARRGGBB`); `scale`
    /// converts CSS px stop positions to pixels.
    pub fn render(&self, w: u32, h: u32, scale: f32) -> Vec<u32> {
        let (wf, hf) = (w as f32, h as f32);
        // Gradient line length and a function from pixel to distance.
        let (len, dist): (f32, Box<dyn Fn(f32, f32) -> f32>) = match &self.kind {
            Kind::Linear { angle, corner } => {
                let (dx, dy) = match (angle, corner) {
                    (_, Some((sx, sy))) => {
                        // Perpendicular to the diagonal between the other
                        // two corners, pointing to the named corner.
                        let (vx, vy) = (sx * hf, sy * wf);
                        let n = (vx * vx + vy * vy).sqrt().max(1e-3);
                        (vx / n, vy / n)
                    }
                    (Some(a), None) => {
                        let r = a.to_radians();
                        (r.sin(), -r.cos())
                    }
                    _ => (0.0, 1.0),
                };
                let len = (wf * dx).abs() + (hf * dy).abs();
                let (cx, cy) = (wf / 2.0, hf / 2.0);
                (
                    len.max(1e-3),
                    Box::new(move |x, y| (x - cx) * dx + (y - cy) * dy + len / 2.0),
                )
            }
            Kind::Radial {
                circle,
                closest,
                size,
                at,
            } => {
                let cx = at.0.px_or_zero(wf / scale) * scale;
                let cy = at.1.px_or_zero(hf / scale) * scale;
                let (sx, sy) = ((cx).max(wf - cx), (cy).max(hf - cy));
                let (nx, ny) = ((cx).min(wf - cx), (cy).min(hf - cy));
                let (mut rx, mut ry) = match size {
                    Some((a, b)) => (
                        a.px_or_zero(wf / scale) * scale,
                        b.px_or_zero(hf / scale) * scale,
                    ),
                    None if *closest => (nx, ny),
                    None => (sx * std::f32::consts::SQRT_2, sy * std::f32::consts::SQRT_2),
                };
                if *circle {
                    let r = match size {
                        Some(_) => rx,
                        None if *closest => nx.min(ny),
                        None => (sx * sx + sy * sy).sqrt(),
                    };
                    (rx, ry) = (r, r);
                }
                let (rx, ry) = (rx.max(1e-3), ry.max(1e-3));
                (
                    rx,
                    Box::new(move |x, y| {
                        let (u, v) = ((x - cx) / rx, (y - cy) / ry);
                        (u * u + v * v).sqrt() * rx
                    }),
                )
            }
        };
        // Stop positions in pixels along the line, filled in per CSS.
        let mut pos: Vec<Option<f32>> = self
            .stops
            .iter()
            .map(|(_, p)| {
                p.map(|d| match d {
                    Dim::Percent(p) => p / 100.0 * len,
                    other => other.px_or_zero(len / scale) * scale,
                })
            })
            .collect();
        let n = pos.len();
        if pos[0].is_none() {
            pos[0] = Some(0.0);
        }
        if pos[n - 1].is_none() {
            pos[n - 1] = Some(len);
        }
        let mut last = pos[0].unwrap();
        for p in pos.iter_mut().flatten() {
            *p = p.max(last);
            last = *p;
        }
        let mut i = 0;
        while i < n {
            if pos[i].is_some() {
                i += 1;
                continue;
            }
            let start = i - 1;
            let end = (i..n).find(|&j| pos[j].is_some()).unwrap();
            let (a, b) = (pos[start].unwrap(), pos[end].unwrap());
            for (k, slot) in pos[start + 1..end].iter_mut().enumerate() {
                *slot = Some(a + (b - a) * (k + 1) as f32 / (end - start) as f32);
            }
            i = end;
        }
        let pos: Vec<f32> = pos.into_iter().map(Option::unwrap).collect();
        let cols: Vec<[f32; 4]> = self.stops.iter().map(|(c, _)| premul(*c)).collect();
        let (first, period) = (pos[0], pos[n - 1] - pos[0]);
        let color_at = |mut d: f32| -> u32 {
            if self.repeating && period > 0.0 {
                d = first + (d - first).rem_euclid(period);
            }
            let c = if d <= pos[0] {
                cols[0]
            } else if d >= pos[n - 1] {
                cols[n - 1]
            } else {
                let j = pos.iter().position(|&p| p > d).unwrap_or(n - 1);
                let (p0, p1) = (pos[j - 1], pos[j]);
                let t = if p1 > p0 { (d - p0) / (p1 - p0) } else { 1.0 };
                let (a, b) = (cols[j - 1], cols[j]);
                std::array::from_fn(|k| a[k] + (b[k] - a[k]) * t)
            };
            let a = c[3];
            if a <= 0.0 {
                return 0;
            }
            let ch = |v: f32| (v / a).round().clamp(0.0, 255.0) as u32;
            ((a * 255.0).round() as u32) << 24 | ch(c[0]) << 16 | ch(c[1]) << 8 | ch(c[2])
        };
        let mut out = Vec::with_capacity((w * h) as usize);
        for y in 0..h {
            for x in 0..w {
                out.push(color_at(dist(x as f32 + 0.5, y as f32 + 0.5)));
            }
        }
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const FC: FontContext = FontContext {
        em: 16.0,
        rem: 16.0,
        vw: 1000.0,
        vh: 800.0,
    };

    #[test]
    fn linear_and_radial() {
        let g = parse("linear-gradient(to right, red, blue)", Rgba::BLACK, FC).unwrap();
        let px = g.render(100, 1, 1.0);
        assert!(px[0] >> 16 & 0xff > 250, "starts red");
        assert!(px[99] & 0xff > 250, "ends blue");
        let g = parse(
            "linear-gradient(#fff 50%, rgba(0, 0, 0, 0) 50%)",
            Rgba::BLACK,
            FC,
        )
        .unwrap();
        let px = g.render(1, 10, 1.0);
        assert_eq!(px[0], 0xffffffff);
        assert_eq!(px[9] >> 24, 0, "hard stop to transparent");
        let g = parse("radial-gradient(circle, white, black)", Rgba::BLACK, FC).unwrap();
        let px = g.render(11, 11, 1.0);
        assert!(px[5 * 11 + 5] & 0xff > 240, "white center");
        assert!(px[0] & 0xff < 40, "dark corner");
        let g = parse(
            "repeating-linear-gradient(90deg, red 0 10px, blue 10px 20px)",
            Rgba::BLACK,
            FC,
        )
        .unwrap();
        let px = g.render(40, 1, 1.0);
        assert_eq!(px[25], px[5]);
        assert!(parse("url(a.png)", Rgba::BLACK, FC).is_none());
    }
}
