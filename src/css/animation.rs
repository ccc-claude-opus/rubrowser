//! CSS Animations and Transitions: `@keyframes`, the `animation-*` and
//! `transition-*` properties, timing functions and interpolation of the
//! properties that only affect painting (opacity, transforms, colors).

use std::rc::Rc;

use super::style::Style;
use super::syntax::Decl;
use super::transform::Op;
use super::values::{Dim, Rgba};

/// One keyframe of `@keyframes`: its offset (0–1) and declarations.
#[derive(Clone, Debug)]
pub struct Keyframe {
    pub offset: f32,
    pub decls: Rc<[Decl]>,
}

/// An easing function.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Timing {
    /// `cubic-bezier(x1, y1, x2, y2)` (`ease`, `linear`, … are these).
    Bezier(f32, f32, f32, f32),
    /// `steps(n, jump-start | jump-end | jump-none | jump-both)`.
    Steps(u32, Jump),
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Jump {
    Start,
    End,
    None,
    Both,
}

pub const EASE: Timing = Timing::Bezier(0.25, 0.1, 0.25, 1.0);

impl Timing {
    pub fn parse(s: &str) -> Option<Timing> {
        let s = s.trim().to_ascii_lowercase();
        Some(match s.as_str() {
            "ease" => EASE,
            "linear" => Timing::Bezier(0.0, 0.0, 1.0, 1.0),
            "ease-in" => Timing::Bezier(0.42, 0.0, 1.0, 1.0),
            "ease-out" => Timing::Bezier(0.0, 0.0, 0.58, 1.0),
            "ease-in-out" => Timing::Bezier(0.42, 0.0, 0.58, 1.0),
            "step-start" => Timing::Steps(1, Jump::Start),
            "step-end" => Timing::Steps(1, Jump::End),
            _ => {
                let (name, args) = s.strip_suffix(')')?.split_once('(')?;
                let args: Vec<&str> = args.split(',').map(str::trim).collect();
                match name.trim() {
                    "cubic-bezier" => {
                        let n: Vec<f32> =
                            args.iter().map(|a| a.parse().ok()).collect::<Option<_>>()?;
                        let [x1, y1, x2, y2] = n[..] else { return None };
                        Timing::Bezier(x1.clamp(0.0, 1.0), y1, x2.clamp(0.0, 1.0), y2)
                    }
                    "steps" => {
                        let n = args.first()?.parse::<u32>().ok().filter(|&n| n > 0)?;
                        let jump = match args.get(1).copied() {
                            Some("jump-start" | "start") => Jump::Start,
                            Some("jump-none") => Jump::None,
                            Some("jump-both") => Jump::Both,
                            _ => Jump::End,
                        };
                        Timing::Steps(n, jump)
                    }
                    // `linear(…)` with stops: approximated as linear.
                    "linear" => Timing::Bezier(0.0, 0.0, 1.0, 1.0),
                    _ => return None,
                }
            }
        })
    }

    /// The eased progress for input progress `t` (0–1).
    pub fn apply(&self, t: f32) -> f32 {
        match *self {
            Timing::Bezier(x1, y1, x2, y2) => {
                if x1 == y1 && x2 == y2 {
                    return t;
                }
                // Solve x(s) = t for s by bisection, then return y(s).
                let curve = |a: f32, b: f32, s: f32| {
                    3.0 * a * s * (1.0 - s).powi(2) + 3.0 * b * s * s * (1.0 - s) + s.powi(3)
                };
                let (mut lo, mut hi) = (0.0f32, 1.0f32);
                for _ in 0..30 {
                    let mid = (lo + hi) / 2.0;
                    if curve(x1, x2, mid) < t {
                        lo = mid;
                    } else {
                        hi = mid;
                    }
                }
                curve(y1, y2, (lo + hi) / 2.0)
            }
            Timing::Steps(n, jump) => {
                let n = n as f32;
                let mut step = (t * n).floor();
                if matches!(jump, Jump::Start | Jump::Both) {
                    step += 1.0;
                }
                if t >= 1.0 && !matches!(jump, Jump::Start | Jump::Both) {
                    step = n;
                }
                let jumps = match jump {
                    Jump::None => n - 1.0,
                    Jump::Both => n + 1.0,
                    _ => n,
                };
                (step / jumps.max(1.0)).clamp(0.0, 1.0)
            }
        }
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub enum Direction {
    #[default]
    Normal,
    Reverse,
    Alternate,
    AlternateReverse,
}

/// The `animation-*` properties: one list per property, applied to the
/// animations by index (shorter lists repeat).
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Animations {
    pub names: Vec<String>,
    pub durations: Vec<f32>,
    pub delays: Vec<f32>,
    pub iterations: Vec<f32>,
    pub directions: Vec<Direction>,
    /// (backwards, forwards) of `animation-fill-mode`.
    pub fills: Vec<(bool, bool)>,
    pub timings: Vec<Timing>,
    pub paused: Vec<bool>,
}

/// One animation, resolved from [`Animations`].
#[derive(Clone, Debug, PartialEq)]
pub struct Animation {
    pub name: String,
    /// Milliseconds.
    pub duration: f32,
    pub delay: f32,
    pub iterations: f32,
    pub direction: Direction,
    pub fill: (bool, bool),
    pub timing: Timing,
    pub paused: bool,
}

fn at<T: Copy>(v: &[T], i: usize, default: T) -> T {
    if v.is_empty() {
        default
    } else {
        v[i % v.len()]
    }
}

impl Animations {
    pub fn list(&self) -> Vec<Animation> {
        self.names
            .iter()
            .enumerate()
            .filter(|(_, n)| n.as_str() != "none")
            .map(|(i, name)| Animation {
                name: name.clone(),
                duration: at(&self.durations, i, 0.0),
                delay: at(&self.delays, i, 0.0),
                iterations: at(&self.iterations, i, 1.0),
                direction: at(&self.directions, i, Direction::Normal),
                fill: at(&self.fills, i, (false, false)),
                timing: at(&self.timings, i, EASE),
                paused: at(&self.paused, i, false),
            })
            .collect()
    }

    /// Applies an `animation` or `animation-*` declaration.
    pub fn set(&mut self, name: &str, value: &str) -> bool {
        let items: Vec<&str> = super::gradient::split_commas(value);
        match name {
            "animation" => {
                *self = Animations::default();
                for item in items {
                    self.shorthand(item);
                }
            }
            "animation-name" => self.names = items.iter().map(|s| s.trim().to_string()).collect(),
            "animation-duration" => self.durations = items.iter().filter_map(|s| time(s)).collect(),
            "animation-delay" => self.delays = items.iter().filter_map(|s| time(s)).collect(),
            "animation-iteration-count" => {
                self.iterations = items.iter().filter_map(|s| iterations(s)).collect()
            }
            "animation-direction" => {
                self.directions = items.iter().filter_map(|s| direction(s)).collect()
            }
            "animation-fill-mode" => self.fills = items.iter().filter_map(|s| fill(s)).collect(),
            "animation-timing-function" => {
                self.timings = items.iter().filter_map(|s| Timing::parse(s)).collect()
            }
            "animation-play-state" => {
                self.paused = items.iter().map(|s| s.trim() == "paused").collect()
            }
            _ => return false,
        }
        true
    }

    /// One comma-separated item of the `animation` shorthand.
    fn shorthand(&mut self, item: &str) {
        let (mut name, mut times) = (None, Vec::new());
        let (mut iter, mut dir, mut fl, mut timing, mut paused) = (None, None, None, None, false);
        for t in super::values::tokens(item) {
            if let Some(v) = time(t) {
                times.push(v);
            } else if let Some(v) = Timing::parse(t).filter(|_| timing.is_none()) {
                timing = Some(v);
            } else if let Some(v) = iterations(t).filter(|_| iter.is_none()) {
                iter = Some(v);
            } else if let Some(v) = direction(t).filter(|_| dir.is_none()) {
                dir = Some(v);
            } else if let Some(v) = fill(t).filter(|_| fl.is_none() && t != "none") {
                fl = Some(v);
            } else if t == "paused" || t == "running" {
                paused = t == "paused";
            } else if name.is_none() {
                name = Some(t.to_string());
            }
        }
        self.names.push(name.unwrap_or_else(|| "none".into()));
        self.durations.push(times.first().copied().unwrap_or(0.0));
        self.delays.push(times.get(1).copied().unwrap_or(0.0));
        self.iterations.push(iter.unwrap_or(1.0));
        self.directions.push(dir.unwrap_or_default());
        self.fills.push(fl.unwrap_or((false, false)));
        self.timings.push(timing.unwrap_or(EASE));
        self.paused.push(paused);
    }
}

/// The `transition-*` properties.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Transitions {
    pub properties: Vec<String>,
    pub durations: Vec<f32>,
    pub delays: Vec<f32>,
    pub timings: Vec<Timing>,
}

impl Transitions {
    /// The duration, delay and timing for property `name`, if it
    /// transitions.
    pub fn of(&self, name: &str) -> Option<(f32, f32, Timing)> {
        let i = self
            .properties
            .iter()
            .rposition(|p| p == name || p == "all" || covers(p, name))?;
        let d = at(&self.durations, i, 0.0);
        (d > 0.0).then(|| (d, at(&self.delays, i, 0.0), at(&self.timings, i, EASE)))
    }

    pub fn set(&mut self, name: &str, value: &str) -> bool {
        let items: Vec<&str> = super::gradient::split_commas(value);
        match name {
            "transition" => {
                *self = Transitions::default();
                for item in items {
                    let (mut prop, mut times, mut timing) = (None, Vec::new(), None);
                    for t in super::values::tokens(item) {
                        if let Some(v) = time(t) {
                            times.push(v);
                        } else if let Some(v) = Timing::parse(t) {
                            timing = Some(v);
                        } else if prop.is_none() {
                            prop = Some(t.to_ascii_lowercase());
                        }
                    }
                    self.properties.push(prop.unwrap_or_else(|| "all".into()));
                    self.durations.push(times.first().copied().unwrap_or(0.0));
                    self.delays.push(times.get(1).copied().unwrap_or(0.0));
                    self.timings.push(timing.unwrap_or(EASE));
                }
            }
            "transition-property" => {
                self.properties = items
                    .iter()
                    .map(|s| s.trim().to_ascii_lowercase())
                    .collect()
            }
            "transition-duration" => {
                self.durations = items.iter().filter_map(|s| time(s)).collect()
            }
            "transition-delay" => self.delays = items.iter().filter_map(|s| time(s)).collect(),
            "transition-timing-function" => {
                self.timings = items.iter().filter_map(|s| Timing::parse(s)).collect()
            }
            _ => return false,
        }
        true
    }
}

/// Whether shorthand transition property `p` covers `name`.
fn covers(p: &str, name: &str) -> bool {
    match p {
        "background" => name == "background-color",
        "border" | "border-color" => name == "border-color",
        _ => false,
    }
}

/// A `<time>` in milliseconds.
fn time(s: &str) -> Option<f32> {
    let s = s.trim().to_ascii_lowercase();
    if let Some(v) = s.strip_suffix("ms") {
        v.parse().ok()
    } else {
        s.strip_suffix('s')?.parse::<f32>().ok().map(|v| v * 1000.0)
    }
}

fn iterations(s: &str) -> Option<f32> {
    let s = s.trim();
    if s == "infinite" {
        return Some(f32::INFINITY);
    }
    s.parse::<f32>().ok().filter(|v| *v >= 0.0)
}

fn direction(s: &str) -> Option<Direction> {
    Some(match s.trim() {
        "normal" => Direction::Normal,
        "reverse" => Direction::Reverse,
        "alternate" => Direction::Alternate,
        "alternate-reverse" => Direction::AlternateReverse,
        _ => return None,
    })
}

fn fill(s: &str) -> Option<(bool, bool)> {
    Some(match s.trim() {
        "none" => (false, false),
        "forwards" => (false, true),
        "backwards" => (true, false),
        "both" => (true, true),
        _ => return None,
    })
}

/// Where animation `a` is `elapsed` ms after it started: the iteration
/// progress (0–1, eased) or `None` when it has no effect (before its delay
/// without backwards fill, or after its end without forwards fill), and
/// whether it is still running.
pub fn progress(a: &Animation, elapsed: f32) -> (Option<f32>, bool) {
    let local = elapsed - a.delay;
    let total = a.duration * a.iterations;
    if local < 0.0 {
        let p = a.fill.0.then(|| directed(a, 0.0, 0.0));
        return (p.map(|p| a.timing.apply(p)), true);
    }
    if a.duration <= 0.0 || local >= total {
        if !a.fill.1 {
            return (None, false);
        }
        // The end of the last iteration.
        let iters = if a.iterations.is_finite() {
            a.iterations
        } else {
            1.0
        };
        let last = (iters.ceil() - 1.0).max(0.0);
        let frac = iters - last;
        let p = directed(a, frac, last);
        return (Some(a.timing.apply(p)), false);
    }
    let iteration = (local / a.duration).floor();
    let frac = local / a.duration - iteration;
    let p = directed(a, frac, iteration);
    (Some(a.timing.apply(p)), true)
}

/// Iteration progress `frac` of iteration `i` in `a`'s direction.
fn directed(a: &Animation, frac: f32, i: f32) -> f32 {
    let odd = i as i64 % 2 == 1;
    let reverse = match a.direction {
        Direction::Normal => false,
        Direction::Reverse => true,
        Direction::Alternate => odd,
        Direction::AlternateReverse => !odd,
    };
    if reverse { 1.0 - frac } else { frac }
}

/// The properties animations and transitions affect here (they change
/// painting only, so no relayout is needed).
pub const PROPERTIES: &[&str] = &[
    "opacity",
    "transform",
    "translate",
    "rotate",
    "scale",
    "color",
    "background-color",
    "border-color",
    "visibility",
];

/// `a` with the animatable properties moved fraction `t` towards `b`.
pub fn interpolate(a: &Style, b: &Style, t: f32) -> Style {
    let mut out = a.clone();
    out.opacity = lerp(a.opacity, b.opacity, t);
    out.color = rgba(a.color, b.color, t);
    out.background = rgba(a.background, b.background, t);
    for i in 0..4 {
        out.border_color[i] = rgba(a.border_color[i], b.border_color[i], t);
    }
    out.translate = (
        dim(a.translate.0, b.translate.0, t),
        dim(a.translate.1, b.translate.1, t),
    );
    out.rotate = lerp(a.rotate, b.rotate, t);
    out.scale = (lerp(a.scale.0, b.scale.0, t), lerp(a.scale.1, b.scale.1, t));
    out.transform = transform(&a.transform, &b.transform, t);
    // Discrete, but `visible` wins anywhere strictly between the ends.
    out.visible = if t <= 0.0 {
        a.visible
    } else if t >= 1.0 {
        b.visible
    } else {
        a.visible || b.visible
    };
    out
}

/// Copies the animatable properties of `from` into `to`.
pub fn copy_animated(to: &mut Style, from: &Style) {
    to.opacity = from.opacity;
    to.color = from.color;
    to.background = from.background;
    to.border_color = from.border_color;
    to.translate = from.translate;
    to.rotate = from.rotate;
    to.scale = from.scale;
    to.transform = from.transform.clone();
    to.visible = from.visible;
}

/// Whether property `name` differs between `a` and `b`.
pub fn differs(a: &Style, b: &Style, name: &str) -> bool {
    match name {
        "opacity" => a.opacity != b.opacity,
        "transform" => a.transform != b.transform,
        "translate" => a.translate != b.translate,
        "rotate" => a.rotate != b.rotate,
        "scale" => a.scale != b.scale,
        "color" => a.color != b.color,
        "background-color" => a.background != b.background,
        "border-color" => a.border_color != b.border_color,
        "visibility" => a.visible != b.visible,
        _ => false,
    }
}

/// `to` with only property `name` taken from `from` (the others stay).
pub fn take(to: &mut Style, from: &Style, name: &str) {
    match name {
        "opacity" => to.opacity = from.opacity,
        "transform" => to.transform = from.transform.clone(),
        "translate" => to.translate = from.translate,
        "rotate" => to.rotate = from.rotate,
        "scale" => to.scale = from.scale,
        "color" => to.color = from.color,
        "background-color" => to.background = from.background,
        "border-color" => to.border_color = from.border_color,
        "visibility" => to.visible = from.visible,
        _ => {}
    }
}

fn lerp(a: f32, b: f32, t: f32) -> f32 {
    a + (b - a) * t
}

fn rgba(a: Rgba, b: Rgba, t: f32) -> Rgba {
    // Premultiplied, so fading to `transparent` keeps the hue.
    let (aa, ba) = (a.a as f32 / 255.0, b.a as f32 / 255.0);
    let alpha = lerp(aa, ba, t);
    let ch = |x: u8, y: u8| {
        if alpha <= 0.0 {
            return 0;
        }
        (lerp(x as f32 * aa, y as f32 * ba, t) / alpha)
            .round()
            .clamp(0.0, 255.0) as u8
    };
    Rgba::new(
        ch(a.r, b.r),
        ch(a.g, b.g),
        ch(a.b, b.b),
        (alpha * 255.0).round() as u8,
    )
}

fn parts(d: Dim) -> Option<(f32, f32)> {
    match d {
        Dim::Px(v) => Some((v, 0.0)),
        Dim::Percent(p) => Some((0.0, p)),
        Dim::Calc(px, p) => Some((px, p)),
        Dim::Auto | Dim::Expr(_) => None,
    }
}

fn dim(a: Dim, b: Dim, t: f32) -> Dim {
    match (parts(a), parts(b)) {
        (Some((ap, aq)), Some((bp, bq))) => {
            let (px, pct) = (lerp(ap, bp, t), lerp(aq, bq, t));
            if pct == 0.0 {
                Dim::Px(px)
            } else if px == 0.0 {
                Dim::Percent(pct)
            } else {
                Dim::Calc(px, pct)
            }
        }
        _ if t < 0.5 => a,
        _ => b,
    }
}

/// Interpolates transform lists function by function when they match
/// (a missing list counts as identity functions); otherwise their
/// matrices, component-wise.
fn transform(a: &Rc<[Op]>, b: &Rc<[Op]>, t: f32) -> Rc<[Op]> {
    let identity = |op: &Op| match op {
        Op::Translate(..) => Op::Translate(Dim::Px(0.0), Dim::Px(0.0)),
        Op::Scale(..) => Op::Scale(1.0, 1.0),
        Op::Rotate(_) => Op::Rotate(0.0),
        Op::Skew(..) => Op::Skew(0.0, 0.0),
        Op::Matrix(_) => Op::Matrix([1.0, 0.0, 0.0, 1.0, 0.0, 0.0]),
    };
    let (a, b): (Vec<Op>, Vec<Op>) = match (a.is_empty(), b.is_empty()) {
        (true, true) => return a.clone(),
        (true, false) => (b.iter().map(identity).collect(), b.to_vec()),
        (false, true) => (a.to_vec(), a.iter().map(identity).collect()),
        _ => (a.to_vec(), b.to_vec()),
    };
    let same = a.len() == b.len()
        && a.iter()
            .zip(&b)
            .all(|(x, y)| std::mem::discriminant(x) == std::mem::discriminant(y));
    if !same {
        let m = |ops: &[Op]| {
            super::transform::matrix(
                (Dim::Px(0.0), Dim::Px(0.0)),
                0.0,
                (1.0, 1.0),
                ops,
                (0.0, 0.0),
                (0.0, 0.0),
            )
            .0
        };
        let (ma, mb) = (m(&a), m(&b));
        return Rc::new([Op::Matrix(std::array::from_fn(|i| lerp(ma[i], mb[i], t)))]);
    }
    a.iter()
        .zip(&b)
        .map(|(x, y)| match (*x, *y) {
            (Op::Translate(x1, y1), Op::Translate(x2, y2)) => {
                Op::Translate(dim(x1, x2, t), dim(y1, y2, t))
            }
            (Op::Scale(x1, y1), Op::Scale(x2, y2)) => Op::Scale(lerp(x1, x2, t), lerp(y1, y2, t)),
            (Op::Rotate(r1), Op::Rotate(r2)) => Op::Rotate(lerp(r1, r2, t)),
            (Op::Skew(x1, y1), Op::Skew(x2, y2)) => Op::Skew(lerp(x1, x2, t), lerp(y1, y2, t)),
            (Op::Matrix(m1), Op::Matrix(m2)) => {
                Op::Matrix(std::array::from_fn(|i| lerp(m1[i], m2[i], t)))
            }
            _ => *x,
        })
        .collect()
}

/// Parses the body of `@keyframes`: offsets (`from`, `to`, percentages)
/// with declarations, sorted by offset.
pub fn parse_keyframes(blocks: &[(&str, Vec<Decl>)]) -> Rc<[Keyframe]> {
    let mut out = Vec::new();
    for (prelude, decls) in blocks {
        let decls: Rc<[Decl]> = decls.clone().into();
        for sel in prelude.split(',') {
            let offset = match sel.trim().to_ascii_lowercase().as_str() {
                "from" => Some(0.0),
                "to" => Some(1.0),
                p => p
                    .strip_suffix('%')
                    .and_then(|v| v.trim().parse::<f32>().ok())
                    .map(|v| v / 100.0),
            };
            if let Some(offset) = offset.filter(|o| (0.0..=1.0).contains(o)) {
                out.push(Keyframe {
                    offset,
                    decls: decls.clone(),
                });
            }
        }
    }
    out.sort_by(|a, b| a.offset.total_cmp(&b.offset));
    out.into()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn timing_and_progress() {
        assert!((Timing::parse("linear").unwrap().apply(0.3) - 0.3).abs() < 1e-4);
        let ease = Timing::parse("ease").unwrap();
        assert!(ease.apply(0.5) > 0.7, "ease is fast in the middle");
        assert_eq!(Timing::parse("steps(4)").unwrap().apply(0.3), 0.25);
        assert_eq!(Timing::parse("step-start").unwrap().apply(0.1), 1.0);

        let mut anims = Animations::default();
        anims.set("animation", "fade 1s linear 0.5s 2 alternate forwards");
        let a = &anims.list()[0];
        assert_eq!(
            (a.name.as_str(), a.duration, a.delay, a.iterations),
            ("fade", 1000.0, 500.0, 2.0)
        );
        assert_eq!(
            progress(a, 0.0),
            (None, true),
            "before the delay, no backwards fill"
        );
        assert_eq!(progress(a, 750.0).0, Some(0.25));
        assert_eq!(
            progress(a, 1750.0).0,
            Some(0.75),
            "second iteration runs backwards"
        );
        assert_eq!(
            progress(a, 9000.0),
            (Some(0.0), false),
            "fills with the end of the last iteration"
        );

        let mut tr = Transitions::default();
        tr.set("transition", "opacity .2s ease-in, transform 300ms");
        assert_eq!(tr.of("opacity").map(|t| t.0), Some(200.0));
        assert_eq!(tr.of("color"), None);
    }

    #[test]
    fn interpolates_colors_and_transforms() {
        let a = Style {
            opacity: 0.0,
            color: Rgba::new(255, 0, 0, 255),
            ..Style::default()
        };
        let b = Style {
            opacity: 1.0,
            color: Rgba::new(0, 0, 255, 255),
            transform: Rc::new([Op::Rotate(1.0)]),
            ..Style::default()
        };
        let m = interpolate(&a, &b, 0.5);
        assert_eq!(m.opacity, 0.5);
        assert_eq!(m.color, Rgba::new(128, 0, 128, 255));
        assert_eq!(&*m.transform, &[Op::Rotate(0.5)]);
    }
}
