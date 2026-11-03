//! `<canvas>` with the 2D context (HTML Canvas 2D): a premultiplied RGBA
//! bitmap per element, drawn with tiny-skia. Scripts send one command per
//! call (see [`Document::canvas_command`]).

use std::cell::RefCell;
use std::collections::HashMap;
use std::rc::Rc;

use resvg::tiny_skia::{
    self, BlendMode, Color, FillRule, GradientStop, LineCap, LineJoin, Mask, Paint, PathBuilder,
    Pixmap, PixmapPaint, Point, Shader, SpreadMode, Stroke, StrokeDash, Transform,
};

use super::Document;
use crate::css::values::Rgba;
use crate::dom::NodeId;
use crate::text::FontKey;

/// What fills or strokes: a color, a gradient or a pattern (by id).
#[derive(Clone, Debug)]
enum Style {
    Color(Rgba),
    Gradient(usize),
    Pattern(usize),
}

#[derive(Clone, Debug)]
enum Gradient {
    Linear([f32; 4]),
    Radial([f32; 6]),
    Conic([f32; 3]),
}

#[derive(Clone)]
struct State {
    transform: Transform,
    fill: Style,
    stroke: Style,
    line_width: f32,
    cap: LineCap,
    join: LineJoin,
    miter: f32,
    dash: Vec<f32>,
    dash_offset: f32,
    alpha: f32,
    blend: BlendMode,
    font: (FontKey, f32),
    align: String,
    baseline: String,
    clip: Option<Rc<Mask>>,
    /// `shadowColor`, `shadowBlur`, `shadowOffsetX/Y`.
    shadow: (Rgba, f32, f32, f32),
    filter: Rc<[crate::css::effects::Filter]>,
}

impl Default for State {
    fn default() -> Self {
        State {
            transform: Transform::identity(),
            fill: Style::Color(Rgba::BLACK),
            stroke: Style::Color(Rgba::BLACK),
            line_width: 1.0,
            cap: LineCap::Butt,
            join: LineJoin::Miter,
            miter: 10.0,
            dash: Vec::new(),
            dash_offset: 0.0,
            alpha: 1.0,
            blend: BlendMode::SourceOver,
            font: (FontKey::default(), 10.0),
            align: "start".into(),
            baseline: "alphabetic".into(),
            clip: None,
            shadow: (Rgba::TRANSPARENT, 0.0, 0.0, 0.0),
            filter: Rc::new([]),
        }
    }
}

/// A painted copy of a canvas: (version, width, height, `0xAARRGGBB`).
pub type Painted = (u64, u32, u32, Rc<Vec<u32>>);

/// One canvas's bitmap and drawing state.
pub struct Canvas2D {
    pixmap: Pixmap,
    state: State,
    stack: Vec<State>,
    /// The current path, in canvas pixels (already transformed).
    path: PathBuilder,
    gradients: Vec<(Gradient, Vec<(f32, Rgba)>)>,
    patterns: Vec<(Rc<Pixmap>, SpreadMode)>,
    /// Bumped on every change, for the painter's cache.
    pub version: u64,
    /// The bitmap as `0xAARRGGBB` at a painted size: (version, w, h, px).
    pub painted: RefCell<Option<Painted>>,
}

impl Canvas2D {
    fn new(w: u32, h: u32) -> Option<Canvas2D> {
        Some(Canvas2D {
            pixmap: Pixmap::new(w.max(1), h.max(1))?,
            state: State::default(),
            stack: Vec::new(),
            path: PathBuilder::new(),
            gradients: Vec::new(),
            patterns: Vec::new(),
            version: 0,
            painted: RefCell::new(None),
        })
    }

    pub fn size(&self) -> (u32, u32) {
        (self.pixmap.width(), self.pixmap.height())
    }

    /// The bitmap scaled to `w × h` as `0xAARRGGBB` (cached).
    pub fn argb(&self, w: u32, h: u32) -> Rc<Vec<u32>> {
        if let Some((v, cw, ch, px)) = &*self.painted.borrow()
            && (*v, *cw, *ch) == (self.version, w, h)
        {
            return px.clone();
        }
        let rgba =
            image::RgbaImage::from_raw(self.pixmap.width(), self.pixmap.height(), self.straight())
                .unwrap_or_default();
        let img = if (w, h) == (rgba.width(), rgba.height()) {
            rgba
        } else {
            image::imageops::resize(
                &rgba,
                w.max(1),
                h.max(1),
                image::imageops::FilterType::Triangle,
            )
        };
        let px: Vec<u32> = img
            .pixels()
            .map(|p| {
                let [r, g, b, a] = p.0;
                (a as u32) << 24 | (r as u32) << 16 | (g as u32) << 8 | b as u32
            })
            .collect();
        let px = Rc::new(px);
        *self.painted.borrow_mut() = Some((self.version, w, h, px.clone()));
        px
    }

    /// Straight (not premultiplied) RGBA bytes.
    fn straight(&self) -> Vec<u8> {
        self.pixmap
            .pixels()
            .iter()
            .flat_map(|p| {
                let c = p.demultiply();
                [c.red(), c.green(), c.blue(), c.alpha()]
            })
            .collect()
    }

    /// The paint for `style` (patterns borrow from `patterns`, a copy of
    /// this canvas's, so the bitmap stays free to draw into).
    fn paint<'a>(&self, style: &Style, patterns: &'a [(Rc<Pixmap>, SpreadMode)]) -> Paint<'a> {
        let mut paint = Paint {
            anti_alias: true,
            blend_mode: self.state.blend,
            ..Paint::default()
        };
        let a = self.state.alpha;
        let color = |c: Rgba| {
            Color::from_rgba8(
                c.r,
                c.g,
                c.b,
                (c.a as f32 * a).round().clamp(0.0, 255.0) as u8,
            )
        };
        paint.shader = match style {
            Style::Color(c) => Shader::SolidColor(color(*c)),
            Style::Gradient(id) => {
                let Some((g, stops)) = self.gradients.get(*id) else {
                    return paint;
                };
                let stops: Vec<GradientStop> = stops
                    .iter()
                    .map(|&(o, c)| GradientStop::new(o, color(c)))
                    .collect();
                let t = self.state.transform;
                let shader = match g {
                    Gradient::Linear([x0, y0, x1, y1]) => tiny_skia::LinearGradient::new(
                        Point::from_xy(*x0, *y0),
                        Point::from_xy(*x1, *y1),
                        stops,
                        SpreadMode::Pad,
                        t,
                    ),
                    Gradient::Radial([x0, y0, r0, x1, y1, r1]) => tiny_skia::RadialGradient::new(
                        Point::from_xy(*x0, *y0),
                        *r0,
                        Point::from_xy(*x1, *y1),
                        *r1,
                        stops,
                        SpreadMode::Pad,
                        t,
                    ),
                    Gradient::Conic([angle, x, y]) => tiny_skia::SweepGradient::new(
                        Point::from_xy(*x, *y),
                        angle.to_degrees(),
                        angle.to_degrees() + 360.0,
                        stops,
                        SpreadMode::Pad,
                        t,
                    ),
                };
                match shader {
                    Some(s) => s,
                    None => return paint,
                }
            }
            Style::Pattern(id) => {
                let Some((pix, spread)) = patterns.get(*id) else {
                    return paint;
                };
                tiny_skia::Pattern::new(
                    pix.as_ref().as_ref(),
                    *spread,
                    tiny_skia::FilterQuality::Bilinear,
                    a,
                    self.state.transform,
                )
            }
        };
        paint
    }

    /// The scale the transform applies to lengths (line widths, text).
    fn scale(&self) -> f32 {
        let t = self.state.transform;
        (t.sx * t.sy - t.kx * t.ky).abs().sqrt().max(1e-6)
    }

    fn map(&self, x: f32, y: f32) -> (f32, f32) {
        let mut p = Point::from_xy(x, y);
        self.state.transform.map_point(&mut p);
        (p.x, p.y)
    }

    fn move_to(&mut self, x: f32, y: f32) {
        let (x, y) = self.map(x, y);
        self.path.move_to(x, y);
    }

    fn line_to(&mut self, x: f32, y: f32) {
        let (x, y) = self.map(x, y);
        if self.path.is_empty() {
            self.path.move_to(x, y);
        } else {
            self.path.line_to(x, y);
        }
    }

    /// An arc of the circle or ellipse at `(cx, cy)`, as cubic Béziers.
    #[allow(clippy::too_many_arguments)]
    fn ellipse(
        &mut self,
        cx: f32,
        cy: f32,
        rx: f32,
        ry: f32,
        rot: f32,
        a0: f32,
        a1: f32,
        ccw: bool,
    ) {
        use std::f32::consts::TAU;
        let mut sweep = a1 - a0;
        if ccw {
            if sweep > 0.0 {
                sweep -= TAU * ((sweep / TAU).floor() + 1.0);
            }
            sweep = sweep.max(-TAU);
        } else {
            if sweep < 0.0 {
                sweep += TAU * ((-sweep / TAU).floor() + 1.0);
            }
            sweep = sweep.min(TAU);
        }
        let (sr, cr) = rot.sin_cos();
        let at = |t: f32| {
            let (s, c) = t.sin_cos();
            let (x, y) = (rx * c, ry * s);
            (cx + x * cr - y * sr, cy + x * sr + y * cr)
        };
        let (x0, y0) = at(a0);
        if self.path.is_empty() {
            self.move_to(x0, y0);
        } else {
            self.line_to(x0, y0);
        }
        let n = (sweep.abs() / (std::f32::consts::FRAC_PI_2))
            .ceil()
            .max(1.0) as usize;
        let step = sweep / n as f32;
        let k = 4.0 / 3.0 * (step / 4.0).tan();
        let mut t = a0;
        for _ in 0..n {
            let t2 = t + step;
            let (s1, c1) = t.sin_cos();
            let (s2, c2) = t2.sin_cos();
            let local = |x: f32, y: f32| (cx + x * cr - y * sr, cy + x * sr + y * cr);
            let p1 = local(rx * (c1 - k * s1), ry * (s1 + k * c1));
            let p2 = local(rx * (c2 + k * s2), ry * (s2 - k * c2));
            let p3 = local(rx * c2, ry * s2);
            let (p1, p2, p3) = (
                self.map(p1.0, p1.1),
                self.map(p2.0, p2.1),
                self.map(p3.0, p3.1),
            );
            self.path.cubic_to(p1.0, p1.1, p2.0, p2.1, p3.0, p3.1);
            t = t2;
        }
    }

    fn rect(&mut self, x: f32, y: f32, w: f32, h: f32) {
        self.move_to(x, y);
        self.line_to(x + w, y);
        self.line_to(x + w, y + h);
        self.line_to(x, y + h);
        self.path.close();
    }

    fn fill_path(&mut self, path: &tiny_skia::Path, rule: FillRule) {
        let layer = self.begin_layer();
        let patterns = self.patterns.clone();
        let paint = self.paint(&self.state.fill.clone(), &patterns);
        let clip = self.state.clip.clone();
        self.pixmap
            .fill_path(path, &paint, rule, Transform::identity(), clip.as_deref());
        self.end_layer(layer);
    }

    /// Whether drawing goes through a layer: a visible shadow or a filter.
    fn has_effects(&self) -> bool {
        let (color, blur, x, y) = self.state.shadow;
        let shadow = color.a > 0 && (blur > 0.0 || x != 0.0 || y != 0.0);
        shadow || !self.state.filter.is_empty()
    }

    /// With a shadow or filter, starts drawing into a transparent layer
    /// (plain source-over, unclipped); returns what [`Canvas2D::end_layer`]
    /// needs to put it on the canvas.
    fn begin_layer(&mut self) -> Option<(Pixmap, BlendMode, Option<Rc<Mask>>)> {
        if !self.has_effects() {
            return None;
        }
        let layer = Pixmap::new(self.pixmap.width(), self.pixmap.height())?;
        let canvas = std::mem::replace(&mut self.pixmap, layer);
        let blend = std::mem::replace(&mut self.state.blend, BlendMode::SourceOver);
        let clip = self.state.clip.take();
        Some((canvas, blend, clip))
    }

    /// Filters the layer, draws its shadow and then the layer onto the
    /// canvas (with the blend mode and clip), as the canvas spec orders.
    fn end_layer(&mut self, saved: Option<(Pixmap, BlendMode, Option<Rc<Mask>>)>) {
        use crate::css::effects::{self, Filter};
        let Some((canvas, blend, clip)) = saved else {
            return;
        };
        let mut layer = std::mem::replace(&mut self.pixmap, canvas);
        self.state.blend = blend;
        self.state.clip = clip;
        let (w, h) = (layer.width() as usize, layer.height() as usize);
        let s = self.scale();
        let to_f = |p: &tiny_skia::PremultipliedColorU8| {
            [p.red(), p.green(), p.blue(), p.alpha()].map(|v| v as f32 / 255.0)
        };
        let mut px: Vec<[f32; 4]> = layer.pixels().iter().map(to_f).collect();
        for f in self.state.filter.iter() {
            match *f {
                Filter::Blur(r) => effects::blur(&mut px, w, h, r * s),
                Filter::DropShadow { x, y, blur, color } => {
                    let sh = shadow_of(&px, w, h, color, x * s, y * s, blur * s / 2.0);
                    for (p, q) in px.iter_mut().zip(&sh) {
                        let keep = 1.0 - p[3];
                        for k in 0..4 {
                            p[k] += q[k] * keep;
                        }
                    }
                }
                ref f => {
                    let Some(m) = f.matrix() else { continue };
                    for p in px.iter_mut().filter(|p| p[3] > 0.0) {
                        let v = [p[0] / p[3], p[1] / p[3], p[2] / p[3], p[3]];
                        let row = |r: &[f32; 5]| {
                            (r[0] * v[0] + r[1] * v[1] + r[2] * v[2] + r[3] * v[3] + r[4])
                                .clamp(0.0, 1.0)
                        };
                        let a = row(&m[3]);
                        *p = [row(&m[0]) * a, row(&m[1]) * a, row(&m[2]) * a, a];
                    }
                }
            }
        }
        let put = |dst: &mut Pixmap, src: &[[f32; 4]]| {
            for (d, p) in dst.pixels_mut().iter_mut().zip(src) {
                let v = p.map(|c| (c.clamp(0.0, 1.0) * 255.0).round() as u8);
                *d = tiny_skia::PremultipliedColorU8::from_rgba(
                    v[0].min(v[3]),
                    v[1].min(v[3]),
                    v[2].min(v[3]),
                    v[3],
                )
                .unwrap_or(tiny_skia::PremultipliedColorU8::TRANSPARENT);
            }
        };
        let paint = PixmapPaint {
            opacity: 1.0,
            blend_mode: blend,
            quality: tiny_skia::FilterQuality::Nearest,
        };
        let clip = self.state.clip.clone();
        // Shadow offsets are in canvas units, unaffected by the transform.
        let (color, blur, sx, sy) = self.state.shadow;
        if color.a > 0 && (blur > 0.0 || sx != 0.0 || sy != 0.0) {
            let sh = shadow_of(&px, w, h, color, sx * s, sy * s, blur * s / 2.0);
            if let Some(mut shadow) = Pixmap::new(w as u32, h as u32) {
                put(&mut shadow, &sh);
                self.pixmap.draw_pixmap(
                    0,
                    0,
                    shadow.as_ref(),
                    &paint,
                    Transform::identity(),
                    clip.as_deref(),
                );
            }
        }
        put(&mut layer, &px);
        self.pixmap.draw_pixmap(
            0,
            0,
            layer.as_ref(),
            &paint,
            Transform::identity(),
            clip.as_deref(),
        );
    }

    fn stroke_path(&mut self, path: &tiny_skia::Path) {
        let layer = self.begin_layer();
        let s = self.scale();
        let stroke = Stroke {
            width: self.state.line_width * s,
            miter_limit: self.state.miter,
            line_cap: self.state.cap,
            line_join: self.state.join,
            dash: (!self.state.dash.is_empty())
                .then(|| {
                    let mut d: Vec<f32> = self.state.dash.iter().map(|v| v * s).collect();
                    if d.len() % 2 == 1 {
                        d.extend(d.clone());
                    }
                    StrokeDash::new(d, self.state.dash_offset * s)
                })
                .flatten(),
        };
        let patterns = self.patterns.clone();
        let paint = self.paint(&self.state.stroke.clone(), &patterns);
        let clip = self.state.clip.clone();
        self.pixmap.stroke_path(
            path,
            &paint,
            &stroke,
            Transform::identity(),
            clip.as_deref(),
        );
        self.end_layer(layer);
    }

    /// The path of `x, y, w, h` in user space, transformed.
    fn rect_path(&self, x: f32, y: f32, w: f32, h: f32) -> Option<tiny_skia::Path> {
        let mut pb = PathBuilder::new();
        let pts = [(x, y), (x + w, y), (x + w, y + h), (x, y + h)].map(|(a, b)| self.map(a, b));
        pb.move_to(pts[0].0, pts[0].1);
        for p in &pts[1..] {
            pb.line_to(p.0, p.1);
        }
        pb.close();
        pb.finish()
    }

    /// Whether `(x, y)` (canvas pixels) is inside `path`.
    fn contains(&self, path: &tiny_skia::Path, rule: FillRule, x: f32, y: f32) -> bool {
        let (w, h) = self.size();
        if x < 0.0 || y < 0.0 || x >= w as f32 || y >= h as f32 {
            return false;
        }
        let Some(mut mask) = Mask::new(w, h) else {
            return false;
        };
        mask.fill_path(path, rule, false, Transform::identity());
        mask.data()[y as usize * w as usize + x as usize] > 127
    }
}

/// Parses `fill` / `stroke` values sent by scripts: a CSS color, `g:<id>`
/// or `p:<id>`.
/// The shadow of premultiplied pixels `px` (`w × h`): their alpha in
/// `color`, moved by `(dx, dy)` and blurred with standard deviation `sigma`
/// (all in device pixels).
fn shadow_of(
    px: &[[f32; 4]],
    w: usize,
    h: usize,
    color: Rgba,
    dx: f32,
    dy: f32,
    sigma: f32,
) -> Vec<[f32; 4]> {
    let ca = color.a as f32 / 255.0;
    let rgb = [color.r, color.g, color.b].map(|v| v as f32 / 255.0 * ca);
    let (dx, dy) = (dx.round() as isize, dy.round() as isize);
    let mut out = vec![[0.0f32; 4]; w * h];
    for j in 0..h as isize {
        for i in 0..w as isize {
            let (si, sj) = (i - dx, j - dy);
            if si < 0 || sj < 0 || si >= w as isize || sj >= h as isize {
                continue;
            }
            let a = px[sj as usize * w + si as usize][3];
            out[j as usize * w + i as usize] = [rgb[0] * a, rgb[1] * a, rgb[2] * a, ca * a];
        }
    }
    crate::css::effects::blur(&mut out, w, h, sigma);
    out
}

fn style(v: &str) -> Option<Style> {
    if let Some(id) = v.strip_prefix("g:") {
        return id.parse().ok().map(Style::Gradient);
    }
    if let Some(id) = v.strip_prefix("p:") {
        return id.parse().ok().map(Style::Pattern);
    }
    crate::css::values::color(v, Rgba::BLACK).map(Style::Color)
}

fn rule(v: Option<&&str>) -> FillRule {
    if v.is_some_and(|r| *r == "evenodd") {
        FillRule::EvenOdd
    } else {
        FillRule::Winding
    }
}

fn blend(op: &str) -> Option<BlendMode> {
    Some(match op {
        "source-over" => BlendMode::SourceOver,
        "source-in" => BlendMode::SourceIn,
        "source-out" => BlendMode::SourceOut,
        "source-atop" => BlendMode::SourceAtop,
        "destination-over" => BlendMode::DestinationOver,
        "destination-in" => BlendMode::DestinationIn,
        "destination-out" => BlendMode::DestinationOut,
        "destination-atop" => BlendMode::DestinationAtop,
        "lighter" => BlendMode::Plus,
        "copy" => BlendMode::Source,
        "xor" => BlendMode::Xor,
        "multiply" => BlendMode::Multiply,
        "screen" => BlendMode::Screen,
        "overlay" => BlendMode::Overlay,
        "darken" => BlendMode::Darken,
        "lighten" => BlendMode::Lighten,
        "color-dodge" => BlendMode::ColorDodge,
        "color-burn" => BlendMode::ColorBurn,
        "hard-light" => BlendMode::HardLight,
        "soft-light" => BlendMode::SoftLight,
        "difference" => BlendMode::Difference,
        "exclusion" => BlendMode::Exclusion,
        "hue" => BlendMode::Hue,
        "saturation" => BlendMode::Saturation,
        "color" => BlendMode::Color,
        "luminosity" => BlendMode::Luminosity,
        _ => return None,
    })
}

/// Replays a `Path2D`'s commands (as sent by scripts: `;`-separated, each
/// a name and numbers) onto `c`'s path.
fn replay(c: &mut Canvas2D, cmds: &str) {
    for cmd in cmds.split(';').filter(|s| !s.is_empty()) {
        let mut parts = cmd.split(',');
        let name = parts.next().unwrap_or("");
        let n: Vec<f32> = parts.filter_map(|p| p.parse().ok()).collect();
        path_command(c, name, &n);
    }
}

/// A path-building command; false if `name` is not one.
fn path_command(c: &mut Canvas2D, name: &str, n: &[f32]) -> bool {
    let a = |i: usize| n.get(i).copied().unwrap_or(0.0);
    match name {
        "moveTo" => c.move_to(a(0), a(1)),
        "lineTo" => c.line_to(a(0), a(1)),
        "closePath" => c.path.close(),
        "quadraticCurveTo" => {
            let (p1, p2) = (c.map(a(0), a(1)), c.map(a(2), a(3)));
            if c.path.is_empty() {
                c.path.move_to(p1.0, p1.1);
            }
            c.path.quad_to(p1.0, p1.1, p2.0, p2.1);
        }
        "bezierCurveTo" => {
            let (p1, p2, p3) = (c.map(a(0), a(1)), c.map(a(2), a(3)), c.map(a(4), a(5)));
            if c.path.is_empty() {
                c.path.move_to(p1.0, p1.1);
            }
            c.path.cubic_to(p1.0, p1.1, p2.0, p2.1, p3.0, p3.1);
        }
        "arc" => c.ellipse(a(0), a(1), a(2), a(2), 0.0, a(3), a(4), a(5) != 0.0),
        "ellipse" => c.ellipse(a(0), a(1), a(2), a(3), a(4), a(5), a(6), a(7) != 0.0),
        "rect" => c.rect(a(0), a(1), a(2), a(3)),
        "roundRect" => {
            let (x, y, w, h) = (a(0), a(1), a(2), a(3));
            let r = a(4).min(w.abs() / 2.0).min(h.abs() / 2.0).max(0.0);
            if r == 0.0 {
                c.rect(x, y, w, h);
            } else {
                use std::f32::consts::{FRAC_PI_2, PI};
                c.move_to(x + r, y);
                c.ellipse(x + w - r, y + r, r, r, 0.0, -FRAC_PI_2, 0.0, false);
                c.ellipse(x + w - r, y + h - r, r, r, 0.0, 0.0, FRAC_PI_2, false);
                c.ellipse(x + r, y + h - r, r, r, 0.0, FRAC_PI_2, PI, false);
                c.ellipse(x + r, y + r, r, r, 0.0, PI, 1.5 * PI, false);
                c.path.close();
            }
        }
        "arcTo" => {
            // The arc of radius r tangent to both lines through (x1, y1).
            let Some(last) = c.path.last_point() else {
                c.move_to(a(0), a(1));
                return true;
            };
            let inv = c.state.transform.invert().unwrap_or_default();
            let mut p0 = last;
            inv.map_point(&mut p0);
            let (x1, y1, x2, y2, r) = (a(0), a(1), a(2), a(3), a(4));
            let (d0x, d0y) = (p0.x - x1, p0.y - y1);
            let (d2x, d2y) = (x2 - x1, y2 - y1);
            let (l0, l2) = (
                (d0x * d0x + d0y * d0y).sqrt(),
                (d2x * d2x + d2y * d2y).sqrt(),
            );
            let cross = d0x * d2y - d0y * d2x;
            if r == 0.0 || l0 == 0.0 || l2 == 0.0 || cross.abs() < 1e-6 {
                c.line_to(x1, y1);
                return true;
            }
            let angle = ((d0x * d2x + d0y * d2y) / (l0 * l2))
                .clamp(-1.0, 1.0)
                .acos();
            let t = r / (angle / 2.0).tan();
            let (ux0, uy0, ux2, uy2) = (d0x / l0, d0y / l0, d2x / l2, d2y / l2);
            let (tx0, ty0) = (x1 + ux0 * t, y1 + uy0 * t);
            let (tx2, ty2) = (x1 + ux2 * t, y1 + uy2 * t);
            let (bx, by) = (ux0 + ux2, uy0 + uy2);
            let bl = (bx * bx + by * by).sqrt();
            let dist = r / (angle / 2.0).sin();
            let (cx, cy) = (x1 + bx / bl * dist, y1 + by / bl * dist);
            let a0 = (ty0 - cy).atan2(tx0 - cx);
            let a1 = (ty2 - cy).atan2(tx2 - cx);
            c.line_to(tx0, ty0);
            c.ellipse(cx, cy, r, r, 0.0, a0, a1, cross > 0.0);
        }
        _ => return false,
    }
    true
}

fn b64(bytes: &[u8]) -> String {
    const T: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = String::with_capacity(bytes.len().div_ceil(3) * 4);
    for chunk in bytes.chunks(3) {
        let v = (chunk[0] as u32) << 16
            | (*chunk.get(1).unwrap_or(&0) as u32) << 8
            | *chunk.get(2).unwrap_or(&0) as u32;
        for k in 0..4 {
            if k <= chunk.len() {
                out.push(T[(v >> (18 - 6 * k) & 63) as usize] as char);
            } else {
                out.push('=');
            }
        }
    }
    out
}

fn unb64(s: &str) -> Vec<u8> {
    let val = |c: u8| match c {
        b'A'..=b'Z' => Some(c - b'A'),
        b'a'..=b'z' => Some(c - b'a' + 26),
        b'0'..=b'9' => Some(c - b'0' + 52),
        b'+' => Some(62),
        b'/' => Some(63),
        _ => None,
    };
    let mut out = Vec::with_capacity(s.len() / 4 * 3);
    let (mut acc, mut bits) = (0u32, 0);
    for v in s.bytes().filter_map(val) {
        acc = acc << 6 | v as u32;
        bits += 6;
        if bits >= 8 {
            bits -= 8;
            out.push((acc >> bits) as u8);
        }
    }
    out
}

impl Document {
    /// The canvas of `<canvas>` element `n`, made (or remade, which clears
    /// it) to match its `width` / `height` attributes.
    fn canvas_at(&mut self, n: NodeId) -> Option<&mut Canvas2D> {
        let (w, h) = canvas_size(&self.dom, n);
        let fresh = self.canvases.get(&n).is_none_or(|c| c.size() != (w, h));
        if fresh {
            self.canvases.insert(n, Canvas2D::new(w, h)?);
        }
        self.canvases.get_mut(&n)
    }

    /// Runs one 2D-context command on `<canvas>` `n`: its name and
    /// arguments separated by U+0001. Returns the result of queries
    /// (`measureText`, `getImageData`, `toDataURL`, `isPointInPath`).
    pub fn canvas_command(&mut self, n: NodeId, cmd: &str) -> Option<String> {
        let args: Vec<&str> = cmd.split('\u{1}').collect();
        let name = *args.first()?;
        let num = |i: usize| {
            args.get(i)
                .and_then(|v| v.parse::<f32>().ok())
                .unwrap_or(0.0)
        };
        // Commands that need other parts of the document.
        match name {
            "font" => {
                let mut s = crate::css::Style::default();
                s.apply("font", args.get(1)?, &crate::css::Style::default(), 16.0);
                let c = self.canvas_at(n)?;
                c.state.font = (s.font(), s.font_size);
                return None;
            }
            "drawImage" => {
                let source = args.get(1)?.parse::<NodeId>().ok()?;
                let pixmap = self.source_pixmap(source)?;
                let (iw, ih) = (pixmap.width() as f32, pixmap.height() as f32);
                let n_args = args.len() - 2;
                let (sx, sy, sw, sh, dx, dy, dw, dh) = match n_args {
                    2 => (0.0, 0.0, iw, ih, num(2), num(3), iw, ih),
                    4 => (0.0, 0.0, iw, ih, num(2), num(3), num(4), num(5)),
                    _ => (
                        num(2),
                        num(3),
                        num(4),
                        num(5),
                        num(6),
                        num(7),
                        num(8),
                        num(9),
                    ),
                };
                let c = self.canvas_at(n)?;
                if sw == 0.0 || sh == 0.0 {
                    return None;
                }
                let layer = c.begin_layer();
                let t = c
                    .state
                    .transform
                    .pre_translate(dx, dy)
                    .pre_scale(dw / sw, dh / sh)
                    .pre_translate(-sx, -sy);
                let paint = PixmapPaint {
                    opacity: c.state.alpha,
                    blend_mode: c.state.blend,
                    quality: tiny_skia::FilterQuality::Bilinear,
                };
                // Only the source rectangle is drawn.
                let mut clip = Mask::new(c.size().0, c.size().1)?;
                if let Some(p) = c.rect_path(dx, dy, dw, dh) {
                    clip.fill_path(&p, FillRule::Winding, true, Transform::identity());
                }
                if let Some(existing) = &c.state.clip {
                    for (a, b) in clip.data_mut().iter_mut().zip(existing.data()) {
                        *a = ((*a as u16 * *b as u16) / 255) as u8;
                    }
                }
                c.pixmap
                    .draw_pixmap(0, 0, pixmap.as_ref(), &paint, t, Some(&clip));
                c.end_layer(layer);
                c.version += 1;
                return None;
            }
            "createPattern" => {
                let source = args.get(1)?.parse::<NodeId>().ok()?;
                let pixmap = self.source_pixmap(source)?;
                let spread = match args.get(2).copied().unwrap_or("repeat") {
                    "no-repeat" => SpreadMode::Pad,
                    _ => SpreadMode::Repeat,
                };
                let c = self.canvas_at(n)?;
                c.patterns.push((Rc::new(pixmap), spread));
                return Some((c.patterns.len() - 1).to_string());
            }
            "fillText" | "strokeText" | "measureText" => {
                return self.canvas_text(n, name, &args);
            }
            _ => {}
        }
        let c = self.canvas_at(n)?;
        let mut changed = true;
        let result = match name {
            "save" => {
                c.stack.push(c.state.clone());
                changed = false;
                None
            }
            "restore" => {
                if let Some(s) = c.stack.pop() {
                    c.state = s;
                }
                changed = false;
                None
            }
            "reset" => {
                let (w, h) = c.size();
                *c = Canvas2D::new(w, h)?;
                None
            }
            "setTransform" => {
                c.state.transform =
                    Transform::from_row(num(1), num(2), num(3), num(4), num(5), num(6));
                changed = false;
                None
            }
            "transform" => {
                let t = Transform::from_row(num(1), num(2), num(3), num(4), num(5), num(6));
                c.state.transform = c.state.transform.pre_concat(t);
                changed = false;
                None
            }
            "fillStyle" | "strokeStyle" => {
                if let Some(s) = args.get(1).and_then(|v| style(v)) {
                    if name == "fillStyle" {
                        c.state.fill = s;
                    } else {
                        c.state.stroke = s;
                    }
                }
                changed = false;
                None
            }
            "lineWidth" => {
                let w = num(1);
                if w > 0.0 && w.is_finite() {
                    c.state.line_width = w;
                }
                changed = false;
                None
            }
            "lineCap" => {
                c.state.cap = match args.get(1).copied() {
                    Some("round") => LineCap::Round,
                    Some("square") => LineCap::Square,
                    _ => LineCap::Butt,
                };
                changed = false;
                None
            }
            "lineJoin" => {
                c.state.join = match args.get(1).copied() {
                    Some("round") => LineJoin::Round,
                    Some("bevel") => LineJoin::Bevel,
                    _ => LineJoin::Miter,
                };
                changed = false;
                None
            }
            "miterLimit" => {
                c.state.miter = num(1).max(1.0);
                changed = false;
                None
            }
            "setLineDash" => {
                c.state.dash = args[1..].iter().filter_map(|v| v.parse().ok()).collect();
                changed = false;
                None
            }
            "lineDashOffset" => {
                c.state.dash_offset = num(1);
                changed = false;
                None
            }
            "shadowColor" | "shadowBlur" | "shadowOffsetX" | "shadowOffsetY" | "filter" => {
                let v = args.get(1).copied().unwrap_or("");
                let sh = &mut c.state.shadow;
                match name {
                    "shadowColor" => {
                        if let Some(col) = crate::css::values::color(v, Rgba::BLACK) {
                            sh.0 = col;
                        }
                    }
                    "shadowBlur" if num(1) >= 0.0 => sh.1 = num(1),
                    "shadowOffsetX" if num(1).is_finite() => sh.2 = num(1),
                    "shadowOffsetY" if num(1).is_finite() => sh.3 = num(1),
                    "filter" => {
                        let fc = crate::css::values::FontContext {
                            em: 10.0,
                            rem: 16.0,
                            vw: 0.0,
                            vh: 0.0,
                        };
                        if let Some(f) = crate::css::effects::parse_filter(v, fc, Rgba::BLACK) {
                            c.state.filter = f.into();
                        }
                    }
                    _ => {}
                }
                changed = false;
                None
            }
            "globalAlpha" => {
                let a = num(1);
                if (0.0..=1.0).contains(&a) {
                    c.state.alpha = a;
                }
                changed = false;
                None
            }
            "globalCompositeOperation" => {
                if let Some(b) = args.get(1).and_then(|v| blend(v)) {
                    c.state.blend = b;
                }
                changed = false;
                None
            }
            "textAlign" | "textBaseline" => {
                let v = args.get(1).unwrap_or(&"").to_string();
                if name == "textAlign" {
                    c.state.align = v;
                } else {
                    c.state.baseline = v;
                }
                changed = false;
                None
            }
            "linearGradient" | "radialGradient" | "conicGradient" => {
                let g = match name {
                    "linearGradient" => Gradient::Linear([num(1), num(2), num(3), num(4)]),
                    "radialGradient" => {
                        Gradient::Radial([num(1), num(2), num(3), num(4), num(5), num(6)])
                    }
                    _ => Gradient::Conic([num(1), num(2), num(3)]),
                };
                c.gradients.push((g, Vec::new()));
                changed = false;
                Some((c.gradients.len() - 1).to_string())
            }
            "addColorStop" => {
                let id = num(1) as usize;
                let color = crate::css::values::color(args.get(3)?, Rgba::BLACK)?;
                if let Some((_, stops)) = c.gradients.get_mut(id) {
                    stops.push((num(2).clamp(0.0, 1.0), color));
                    stops.sort_by(|a, b| a.0.total_cmp(&b.0));
                }
                changed = false;
                None
            }
            "beginPath" => {
                c.path = PathBuilder::new();
                changed = false;
                None
            }
            "fillRect" | "strokeRect" | "clearRect" => {
                let path = c.rect_path(num(1), num(2), num(3), num(4))?;
                match name {
                    "fillRect" => c.fill_path(&path, FillRule::Winding),
                    "strokeRect" => c.stroke_path(&path),
                    _ => {
                        let paint = Paint {
                            blend_mode: BlendMode::Clear,
                            anti_alias: true,
                            ..Paint::default()
                        };
                        let clip = c.state.clip.clone();
                        c.pixmap.fill_path(
                            &path,
                            &paint,
                            FillRule::Winding,
                            Transform::identity(),
                            clip.as_deref(),
                        );
                    }
                }
                None
            }
            "fill" | "stroke" | "clip" | "isPointInPath" => {
                // An optional Path2D (as commands) replaces the current path.
                let (path, rule_arg) = match args.get(1) {
                    Some(cmds) if cmds.starts_with("path:") => {
                        let saved = std::mem::take(&mut c.path);
                        replay(c, &cmds[5..]);
                        let p = std::mem::replace(&mut c.path, saved).finish();
                        (p, args.get(2))
                    }
                    _ => (c.path.clone().finish(), args.get(1)),
                };
                let Some(path) = path else {
                    return (name == "isPointInPath").then(|| "0".into());
                };
                match name {
                    "fill" => c.fill_path(&path, rule(rule_arg)),
                    "stroke" => c.stroke_path(&path),
                    "clip" => {
                        let (w, h) = c.size();
                        let mut mask = Mask::new(w, h)?;
                        mask.fill_path(&path, rule(rule_arg), true, Transform::identity());
                        if let Some(old) = &c.state.clip {
                            for (a, b) in mask.data_mut().iter_mut().zip(old.data()) {
                                *a = ((*a as u16 * *b as u16) / 255) as u8;
                            }
                        }
                        c.state.clip = Some(Rc::new(mask));
                        changed = false;
                    }
                    _ => {
                        // isPointInPath(x, y): the point is in canvas units,
                        // not transformed.
                        let k = args.len();
                        let (x, y) = (num(k - 2), num(k - 1));
                        let inside = c.contains(&path, rule(rule_arg), x, y);
                        return Some(if inside { "1".into() } else { "0".into() });
                    }
                }
                None
            }
            "getImageData" => {
                let (x, y, w, h) = (num(1) as i64, num(2) as i64, num(3) as i64, num(4) as i64);
                let (cw, ch) = (c.size().0 as i64, c.size().1 as i64);
                let px = c.pixmap.pixels();
                let mut out = Vec::with_capacity((w.max(0) * h.max(0) * 4) as usize);
                for row in y..y + h {
                    for col in x..x + w {
                        if row < 0 || col < 0 || row >= ch || col >= cw {
                            out.extend([0, 0, 0, 0]);
                        } else {
                            let p = px[(row * cw + col) as usize].demultiply();
                            out.extend([p.red(), p.green(), p.blue(), p.alpha()]);
                        }
                    }
                }
                changed = false;
                Some(b64(&out))
            }
            "putImageData" => {
                let (x, y, w) = (num(1) as i64, num(2) as i64, num(3) as i64);
                let data = unb64(args.get(4)?);
                let (cw, ch) = (c.size().0 as i64, c.size().1 as i64);
                let px = c.pixmap.pixels_mut();
                for (k, rgba) in data.chunks_exact(4).enumerate() {
                    let (col, row) = (x + k as i64 % w.max(1), y + k as i64 / w.max(1));
                    if row < 0 || col < 0 || row >= ch || col >= cw {
                        continue;
                    }
                    let c = tiny_skia::ColorU8::from_rgba(rgba[0], rgba[1], rgba[2], rgba[3]);
                    px[(row * cw + col) as usize] = c.premultiply();
                }
                None
            }
            "toDataURL" => {
                let (w, h) = c.size();
                let img = image::RgbaImage::from_raw(w, h, c.straight())?;
                let mut png = Vec::new();
                img.write_to(&mut std::io::Cursor::new(&mut png), image::ImageFormat::Png)
                    .ok()?;
                changed = false;
                Some(format!("data:image/png;base64,{}", b64(&png)))
            }
            _ => {
                let n: Vec<f32> = args[1..].iter().filter_map(|v| v.parse().ok()).collect();
                path_command(c, name, &n);
                changed = false;
                None
            }
        };
        if changed {
            c.version += 1;
        }
        result
    }

    /// `fillText`, `strokeText` (drawn filled) and `measureText` (returns
    /// width, ascent and descent, comma-separated).
    fn canvas_text(&mut self, n: NodeId, name: &str, args: &[&str]) -> Option<String> {
        let fonts = self.fonts.clone()?;
        let c = self.canvas_at(n)?;
        let text = args.get(1).copied().unwrap_or("");
        let (key, size) = c.state.font;
        let width = fonts.measure(key, size, text);
        let m = fonts.line_metrics(key, size);
        if name == "measureText" {
            return Some(format!("{width},{},{}", m.ascent, m.descent));
        }
        let num = |i: usize| args.get(i).and_then(|v| v.parse::<f32>().ok());
        let (x, y) = (num(2).unwrap_or(0.0), num(3).unwrap_or(0.0));
        // Text wider than maxWidth is squeezed horizontally.
        let squeeze = num(4)
            .filter(|&mw| mw > 0.0 && width > mw)
            .map_or(1.0, |mw| mw / width);
        let rtl = false;
        let dx = match c.state.align.as_str() {
            "center" => -width / 2.0,
            "right" => -width,
            "end" if !rtl => -width,
            "start" if rtl => -width,
            _ => 0.0,
        };
        let dy = match c.state.baseline.as_str() {
            "top" | "hanging" => m.ascent,
            "middle" => (m.ascent - m.descent) / 2.0,
            "bottom" | "ideographic" => -m.descent,
            _ => 0.0,
        };
        // Rendered at the device scale, white on black: coverage.
        let s = c.scale();
        let (pw, ph) = (
            ((width * s).ceil() as usize + 2).max(1),
            ((m.ascent + m.descent) * s).ceil() as usize + 2,
        );
        if pw * ph > 16_000_000 {
            return None;
        }
        let mut layer = crate::gfx::Canvas::new(pw, ph);
        fonts.draw(
            &mut layer,
            key,
            size * s,
            1.0,
            1.0 + m.ascent * s,
            text,
            0xff_ffff,
        );
        let style = if name == "strokeText" {
            c.state.stroke.clone()
        } else {
            c.state.fill.clone()
        };
        let color = match style {
            Style::Color(col) => col,
            // Gradients and patterns on text: their first color is used.
            Style::Gradient(id) => c
                .gradients
                .get(id)
                .and_then(|g| g.1.first())
                .map_or(Rgba::BLACK, |s| s.1),
            Style::Pattern(_) => Rgba::BLACK,
        };
        let alpha = c.state.alpha * color.a as f32 / 255.0;
        let mut text_px = Pixmap::new(pw as u32, ph as u32)?;
        for (dst, src) in text_px.pixels_mut().iter_mut().zip(layer.pixels()) {
            let cov = ((src >> 16) & 0xff) as f32 / 255.0 * alpha;
            let a = (cov * 255.0).round() as u8;
            *dst = tiny_skia::ColorU8::from_rgba(color.r, color.g, color.b, a).premultiply();
        }
        let t = c
            .state
            .transform
            .pre_translate(x + dx * squeeze, y + dy)
            .pre_scale(squeeze / s, 1.0 / s)
            .pre_translate(-1.0, -1.0 - m.ascent * s);
        let paint = PixmapPaint {
            opacity: 1.0,
            blend_mode: c.state.blend,
            quality: tiny_skia::FilterQuality::Bilinear,
        };
        let layer = c.begin_layer();
        let paint = PixmapPaint {
            blend_mode: c.state.blend,
            ..paint
        };
        let clip = c.state.clip.clone();
        c.pixmap
            .draw_pixmap(0, 0, text_px.as_ref(), &paint, t, clip.as_deref());
        c.end_layer(layer);
        c.version += 1;
        None
    }

    /// An image source for `drawImage` / `createPattern`: an `<img>` (or
    /// SVG) or another canvas, as a premultiplied pixmap.
    fn source_pixmap(&self, source: NodeId) -> Option<Pixmap> {
        if let Some(c) = self.canvases.get(&source) {
            return Some(c.pixmap.clone());
        }
        let bitmap = self.images.get(source)?.bitmap()?;
        let (w, h) = (bitmap.width.max(1), bitmap.height.max(1));
        let px = self.images.scaled(source, w, h)?;
        let mut pixmap = Pixmap::new(w, h)?;
        for (dst, &p) in pixmap.pixels_mut().iter_mut().zip(px.iter()) {
            let c = tiny_skia::ColorU8::from_rgba(
                (p >> 16) as u8,
                (p >> 8) as u8,
                p as u8,
                (p >> 24) as u8,
            );
            *dst = c.premultiply();
        }
        Some(pixmap)
    }

    /// The canvas of element `n`, if it has been drawn on.
    pub fn canvas(&self, n: NodeId) -> Option<&Canvas2D> {
        self.canvases.get(&n)
    }
}

/// A `<canvas>`'s bitmap size: its `width` and `height` attributes
/// (default 300 × 150).
pub fn canvas_size(dom: &crate::dom::Dom, n: NodeId) -> (u32, u32) {
    let attr = |a: &str, d: u32| {
        dom.attr(n, a)
            .and_then(|v| v.trim().parse::<u32>().ok())
            .unwrap_or(d)
            .min(8192)
    };
    (attr("width", 300), attr("height", 150))
}

pub type Canvases = HashMap<NodeId, Canvas2D>;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn base64_round_trip() {
        for data in [&b""[..], b"a", b"ab", b"abc", b"abcd\x00\xff"] {
            assert_eq!(unb64(&b64(data)), data);
        }
        assert_eq!(b64(b"Man"), "TWFu");
    }
}
