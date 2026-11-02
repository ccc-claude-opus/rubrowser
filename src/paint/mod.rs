//! Painting a laid-out document onto a [`Canvas`].
//!
//! Boxes are painted in tree order (backgrounds, borders, then content);
//! absolutely positioned boxes are painted after their in-flow siblings.
//! Coordinates are logical; `scale` converts to physical pixels.

use std::collections::HashMap;

use crate::css::Style;
use crate::css::style::{self as cs, Display, ListStyle};
use crate::css::transform::Affine;
use crate::css::values::{Dim, Rgba};
use crate::document::{
    Control, FormState, ImageState, Images, button_label, control, option_label, options,
};
use crate::dom::{Dom, NodeId};
use crate::gfx::{Canvas, Rect};
use crate::layout::inline::Placed;
use crate::layout::{LBox, LayoutTree, RectF};
use crate::text::Fonts;
use crate::ui::TextField;

/// Vertical scroll offsets: `None` is the viewport, `Some(id)` an element.
pub type ScrollOffsets = HashMap<Option<NodeId>, f32>;

pub struct Painter<'a> {
    pub dom: &'a Dom,
    pub styles: &'a [Option<Style>],
    pub fonts: &'a Fonts,
    pub tree: &'a LayoutTree,
    pub scroll: &'a ScrollOffsets,
    pub scale: f32,
    /// Editing state of `<input>` elements.
    pub inputs: &'a HashMap<NodeId, TextField>,
    pub focus: Option<NodeId>,
    /// Whether the blinking caret is currently shown.
    pub caret: bool,
    pub images: &'a Images,
    pub forms: &'a FormState,
    /// Top of the page viewport while painting (for sticky boxes).
    pub view_top: std::cell::Cell<f32>,
    /// The transformed box being painted into its layer (painted without
    /// its transform there).
    pub flat: std::cell::Cell<Option<usize>>,
    /// Documents of `<iframe>` elements.
    pub frames: &'a HashMap<NodeId, crate::document::Frame>,
    /// Animated styles, in place of `styles`.
    pub overlay: &'a HashMap<NodeId, Style>,
    /// Bitmaps of `<canvas>` elements.
    pub canvases: &'a crate::document::canvas::Canvases,
    /// Players of `<video>` / `<audio>` elements.
    pub media: &'a crate::document::media::MediaElements,
}

/// What an image layer paints.
#[derive(Clone, Copy)]
enum Layer<'a> {
    Url(&'a str),
    Gradient(&'a crate::css::gradient::Gradient),
}

const SELECTION: Rgba = Rgba::new(51, 132, 255, 90);
const PLACEHOLDER: Rgba = Rgba::new(117, 117, 117, 255);

const SCROLLBAR: Rgba = Rgba::new(0, 0, 0, 90);

impl Painter<'_> {
    fn style(&self, id: NodeId) -> Option<&Style> {
        self.overlay
            .get(&id)
            .or_else(|| self.styles.get(id).and_then(Option::as_ref))
    }

    fn phys(&self, r: RectF) -> Rect {
        let s = self.scale;
        let x0 = (r.x * s).round() as i32;
        let y0 = (r.y * s).round() as i32;
        let x1 = ((r.x + r.w) * s).round() as i32;
        let y1 = ((r.y + r.h) * s).round() as i32;
        Rect::new(x0, y0, x1 - x0, y1 - y0)
    }

    fn fill(&self, c: &mut Canvas, r: RectF, color: Rgba) {
        if color.is_visible() && r.w > 0.0 && r.h > 0.0 {
            let p = self.phys(r);
            c.fill_rect_alpha(p.x, p.y, p.w, p.h, color.rgb(), color.a);
        }
    }

    /// Paints the whole document with the viewport's top-left at `origin`.
    pub fn paint(&self, c: &mut Canvas, origin: (f32, f32)) {
        // Not laid out yet (a frame whose document just arrived).
        let Some(vp) = self.tree.boxes.get(LayoutTree::VIEWPORT) else {
            return;
        };
        let clip = self.phys(RectF {
            x: origin.0,
            y: origin.1,
            ..vp.rect
        });
        let saved = c.clip();
        c.set_clip(clip.intersect(&saved));
        // The root's (or else body's) background covers the whole canvas.
        let (bg, _) = self.canvas_background();
        let bg = if bg.is_visible() {
            bg
        } else {
            Rgba::new(255, 255, 255, 255)
        };
        c.fill_rect(clip.x, clip.y, clip.w, clip.h, bg.rgb());
        let sy = self.scroll.get(&None).copied().unwrap_or(0.0);
        self.view_top.set(origin.1);
        // The page, then fixed boxes (which do not scroll) by z-index.
        let mut kids = vp.children.clone();
        kids.sort_by_key(|&k| (self.tree.boxes[k].fixed, self.tree.boxes[k].z));
        for child in kids {
            let dy = if self.tree.boxes[child].fixed {
                0.0
            } else {
                sy
            };
            self.paint_box(c, child, origin.0, origin.1 - dy);
        }
        self.scrollbar(
            c,
            RectF {
                x: origin.0,
                y: origin.1,
                ..vp.rect
            },
            vp.content_h,
            sy,
        );
        c.set_clip(saved);
    }

    /// Background propagated to the canvas and the element it came from.
    fn canvas_background(&self) -> (Rgba, Option<NodeId>) {
        for n in [self.dom.html(), self.dom.body()].into_iter().flatten() {
            if let Some(s) = self.style(n)
                && s.background.is_visible()
            {
                return (s.background, Some(n));
            }
        }
        (Rgba::TRANSPARENT, None)
    }

    fn paint_box(&self, c: &mut Canvas, index: usize, ox: f32, oy: f32) {
        let b = &self.tree.boxes[index];
        let Some(style) = self.style(b.style_node) else {
            return;
        };
        if style.clipped_away {
            return;
        }
        if b.node == Some(b.style_node) && style.is_transformed() && self.flat.get() != Some(index)
        {
            let m = style.transform_matrix(b.rect.w, b.rect.h);
            if !m.is_translation() {
                self.transformed(c, index, ox, oy, m);
                return;
            }
        }
        let (tx, ty) = b.translation(style);
        let r = RectF {
            x: ox + b.rect.x + tx,
            y: oy + b.rect.y + ty,
            ..b.rect
        };
        let own = b.node.is_some();
        let alpha = c.global_alpha();
        if own && style.opacity < 1.0 {
            c.set_global_alpha((alpha as f32 * style.opacity).round() as u8);
        }
        if own && style.visible {
            let propagated = self.canvas_background().1 == b.node;
            if propagated {
                let plain = Style {
                    background: Rgba::TRANSPARENT,
                    ..style.clone()
                };
                self.decorations(c, b, &plain, r);
            } else if let Some(legend) = self.legend_rect(b, r) {
                self.fieldset(c, b, style, r, legend);
            } else {
                self.decorations(c, b, style, r);
            }
            if style.display == Display::ListItem {
                self.marker(c, b, style, r);
            }
            if let Some((n, width, gap)) = b.columns {
                // Rules between the columns, as tall as the content box.
                let area = Self::content_area(b, r);
                let (w, _, color) = style.column_rule;
                for x in crate::layout::columns::rules(style, n, width, gap) {
                    let rule = RectF {
                        x: area.x + x - w / 2.0,
                        w,
                        ..area
                    };
                    self.fill(c, rule, color);
                }
            }
        }
        // Content and children, clipped if the box hides overflow.
        let saved = c.clip();
        let clips = own && style.overflow != cs::Overflow::Visible;
        if clips {
            let pr = b.padding_rect();
            let clip = self.phys(RectF {
                x: r.x + pr.x,
                y: r.y + pr.y,
                ..pr
            });
            c.set_clip(clip.intersect(&saved));
        }
        let sy = if clips {
            self.scroll.get(&b.node).copied().unwrap_or(0.0)
        } else {
            0.0
        };
        // Images inside a rounded clipping box (avatars) are rounded too.
        let saved_round = c.round_clip();
        let radius = style.radius(r.w, r.h);
        if clips && radius > 0.0 {
            let pr = b.padding_rect();
            let p = RectF {
                x: r.x + pr.x,
                y: r.y + pr.y,
                ..pr
            };
            let s = self.scale;
            c.set_round_clip(Some([p.x * s, p.y * s, p.w * s, p.h * s, radius * s]));
        }
        if let (Some(il), Some(rl)) = (&b.inline, b.vertical) {
            let area = Self::content_area(b, r);
            self.vertical_inline(
                c,
                il,
                RectF {
                    y: area.y - sy,
                    ..area
                },
                rl,
            );
        } else if let Some(il) = &b.inline {
            let (cx, cy) = b.content_offset();
            // `text-overflow: ellipsis` cuts lines at the content box's end.
            let end = (clips && style.ellipsis && !style.rtl).then(|| {
                let content_w = b.rect.w
                    - b.padding[cs::LEFT]
                    - b.padding[cs::RIGHT]
                    - b.border[cs::LEFT]
                    - b.border[cs::RIGHT];
                (r.x + cx + content_w, style)
            });
            self.inline(c, il, r.x + cx, r.y + cy - sy, end);
        }
        // Children in CSS 2.1 Appendix E order (within this box): positioned
        // ones with negative z-index, then the flow, then positioned ones by
        // z-index (tree order among equals: the sort is stable).
        let bottom = r.y + r.h - b.padding[cs::BOTTOM] - b.border[cs::BOTTOM];
        let place = |child: usize| {
            let cb = &self.tree.boxes[child];
            let shift = if cb.absolute {
                0.0
            } else {
                cb.sticky_shift(r.y - sy + cb.rect.y, self.view_top.get(), bottom)
            };
            (r.x, r.y - sy + shift)
        };
        let (mut positioned, flow): (Vec<usize>, Vec<usize>) =
            b.children.iter().partition(|&&k| self.is_positioned(k));
        positioned.sort_by_key(|&k| self.tree.boxes[k].z);
        let (below, above): (Vec<usize>, Vec<usize>) =
            positioned.iter().partition(|&&k| self.tree.boxes[k].z < 0);
        for child in below.into_iter().chain(flow).chain(above) {
            let (x, y) = place(child);
            self.paint_box(c, child, x, y);
        }
        if let Some(node) = b.node
            && self.dom.tag(node) != Some("button")
            && style.visible
            && let Some(kind) = control(self.dom, node)
        {
            match kind {
                Control::Text | Control::Password => self.input(c, b, node, style, r),
                Control::TextArea => self.textarea(c, b, node, style, r),
                Control::Checkbox | Control::Radio => self.check(c, b, node, kind, r),
                Control::Select => self.select(c, b, node, style, r),
                Control::Submit | Control::Reset | Control::Button => {
                    let label = button_label(self.dom, node, kind);
                    self.centered_label(c, b, style, r, &label);
                }
                Control::Range => self.range(c, b, node, r),
                Control::Color => self.color_swatch(c, b, node, r),
                Control::File => self.file_input(c, b, style, r),
                Control::Hidden => {}
            }
        }
        if let Some(node) = b.node.filter(|_| style.visible) {
            match self.dom.tag(node) {
                Some("canvas") => {
                    if let Some(cv) = self.canvases.get(&node) {
                        let area = Self::content_area(b, r);
                        let (w, h) = cv.size();
                        let d = self.phys(object_rect(style, area, (w as f32, h as f32)));
                        if d.w > 0 && d.h > 0 {
                            let px = cv.argb(d.w as u32, d.h as u32);
                            c.draw_argb(d.x, d.y, d.w as usize, d.h as usize, &px);
                        }
                    }
                }
                Some("video" | "audio") => self.media_element(c, b, node, style, r),
                Some("progress") => self.bar(c, b, node, r, true),
                Some("meter") => self.bar(c, b, node, r, false),
                _ => {}
            }
        }
        if let Some(node) = b
            .node
            .filter(|&n| matches!(self.dom.tag(n), Some("img" | "svg")))
            && style.visible
        {
            let outer = c.round_clip();
            if radius > 0.0 && outer.is_none() {
                let s = self.scale;
                c.set_round_clip(Some([r.x * s, r.y * s, r.w * s, r.h * s, radius * s]));
            }
            self.image(c, b, node, style, r);
            c.set_round_clip(outer);
        }
        if let Some(frame) = b
            .node
            .filter(|_| style.visible)
            .and_then(|n| self.frames.get(&n)?.doc.as_deref())
        {
            let (cx, cy) = b.content_offset();
            frame.paint(c, (r.x + cx, r.y + cy), self.scale, self.fonts);
        }
        if own && style.overflow.scrolls() {
            self.scrollbar(c, r, b.content_h, sy);
        }
        c.set_round_clip(saved_round);
        c.set_clip(saved);
        if own && style.visible && style.outline_style != cs::LineStyle::None {
            self.outline(c, style, r);
        }
        c.set_global_alpha(alpha);
    }

    /// Paints box `index` (placed at `ox, oy`) through transform `m` (in px
    /// from its border box's top left, not just a translation): the subtree
    /// is painted into a layer twice, over white and over black, which gives
    /// its colors and coverage; the layer is then resampled into `c`.
    fn transformed(&self, c: &mut Canvas, index: usize, ox: f32, oy: f32, m: Affine) {
        let b = &self.tree.boxes[index];
        let (x0, y0) = (ox + b.rect.x, oy + b.rect.y);
        let s = self.scale;
        // Physical pixels to physical pixels.
        let full = Affine([s, 0.0, 0.0, s, 0.0, 0.0])
            .then(&Affine::translate(x0, y0))
            .then(&m)
            .then(&Affine::translate(-x0, -y0))
            .then(&Affine([1.0 / s, 0.0, 0.0, 1.0 / s, 0.0, 0.0]));
        // A singular matrix (`scale(0)`) leaves nothing to see.
        let Some(inverse) = full.invert() else { return };
        let bounds = self.phys(self.subtree_bounds(index, ox, oy));
        if bounds.w <= 0 || bounds.h <= 0 {
            return;
        }
        let previous = self.flat.replace(Some(index));
        if bounds.w > 4096 || bounds.h > 4096 || bounds.w as i64 * bounds.h as i64 > 8_000_000 {
            // Too large to buffer: painted untransformed.
            self.paint_box(c, index, ox, oy);
            self.flat.set(previous);
            return;
        }
        let (w, h) = (bounds.w as usize, bounds.h as usize);
        let (lx, ly) = (ox - bounds.x as f32 / s, oy - bounds.y as f32 / s);
        let mut white = Canvas::new(w, h);
        white.clear(0xff_ffff);
        let mut black = Canvas::new(w, h);
        self.paint_box(&mut white, index, lx, ly);
        self.paint_box(&mut black, index, lx, ly);
        self.flat.set(previous);
        // Where the layer lands.
        let (bx0, by0) = (bounds.x as f32, bounds.y as f32);
        let (bx1, by1) = (bx0 + w as f32, by0 + h as f32);
        let corners =
            [(bx0, by0), (bx1, by0), (bx0, by1), (bx1, by1)].map(|(x, y)| full.apply(x, y));
        let min = |f: fn(&(f32, f32)) -> f32| corners.iter().map(f).fold(f32::MAX, f32::min);
        let max = |f: fn(&(f32, f32)) -> f32| corners.iter().map(f).fold(f32::MIN, f32::max);
        let (dx0, dy0) = (min(|p| p.0).floor() as i32, min(|p| p.1).floor() as i32);
        let (dx1, dy1) = (max(|p| p.0).ceil() as i32, max(|p| p.1).ceil() as i32);
        let dst = Rect::new(dx0, dy0, dx1 - dx0, dy1 - dy0).intersect(&c.clip());
        for y in dst.y..dst.bottom() {
            for x in dst.x..dst.right() {
                let (sx, sy) = inverse.apply(x as f32 + 0.5, y as f32 + 0.5);
                let (sx, sy) = (sx - bx0 - 0.5, sy - by0 - 0.5);
                if sx < -1.0 || sy < -1.0 || sx > w as f32 || sy > h as f32 {
                    continue;
                }
                let on_white = bilinear(&white, sx, sy, 0xff_ffff);
                let on_black = bilinear(&black, sx, sy, 0);
                let cover = 255.0 - (0..3).map(|k| on_white[k] - on_black[k]).sum::<f32>() / 3.0;
                let alpha = cover.round().clamp(0.0, 255.0) as u8;
                if alpha == 0 {
                    continue;
                }
                let channel = |k: usize| on_black[k].round().clamp(0.0, 255.0) as u32;
                let color = channel(0) << 16 | channel(1) << 8 | channel(2);
                c.composite_premul(x, y, color, alpha);
            }
        }
    }

    /// The area box `index` (placed at `ox, oy`) and its descendants paint,
    /// roughly: border boxes, unless an ancestor clips them.
    fn subtree_bounds(&self, index: usize, ox: f32, oy: f32) -> RectF {
        let b = &self.tree.boxes[index];
        let r = RectF {
            x: ox + b.rect.x,
            y: oy + b.rect.y,
            ..b.rect
        };
        let clips = b.node.is_some()
            && self
                .style(b.style_node)
                .is_some_and(|s| s.overflow != cs::Overflow::Visible);
        let mut out = r;
        if !clips {
            for &child in &b.children {
                let k = self.subtree_bounds(child, r.x, r.y);
                if k.w > 0.0 && k.h > 0.0 {
                    let (x1, y1) = (
                        (out.x + out.w).max(k.x + k.w),
                        (out.y + out.h).max(k.y + k.h),
                    );
                    out.x = out.x.min(k.x);
                    out.y = out.y.min(k.y);
                    out.w = x1 - out.x;
                    out.h = y1 - out.y;
                }
            }
        }
        // Glyphs and antialiasing reach a little past the boxes.
        RectF {
            x: out.x - 2.0,
            y: out.y - 2.0,
            w: out.w + 4.0,
            h: out.h + 4.0,
        }
    }

    /// The outline, outside the border box.
    fn outline(&self, c: &mut Canvas, style: &Style, r: RectF) {
        let w = style.outline_width;
        let o = style.outline_offset;
        let outer = RectF {
            x: r.x - o - w,
            y: r.y - o - w,
            w: r.w + 2.0 * (o + w),
            h: r.h + 2.0 * (o + w),
        };
        let (ls, col) = (style.outline_style, style.outline_color);
        let bottom = RectF {
            y: outer.y + outer.h - w,
            h: w,
            ..outer
        };
        let right = RectF {
            x: outer.x + outer.w - w,
            w,
            ..outer
        };
        self.line_side(c, RectF { h: w, ..outer }, true, ls, col);
        self.line_side(c, bottom, true, ls, col);
        self.line_side(c, RectF { w, ..outer }, false, ls, col);
        self.line_side(c, right, false, ls, col);
    }

    /// One border (or outline) side: a strip drawn solid, dashed, dotted
    /// or double.
    fn line_side(
        &self,
        c: &mut Canvas,
        side: RectF,
        horizontal: bool,
        ls: cs::LineStyle,
        col: Rgba,
    ) {
        let thick = if horizontal { side.h } else { side.w };
        let len = if horizontal { side.w } else { side.h };
        if thick <= 0.0 || len <= 0.0 {
            return;
        }
        let seg = |c: &mut Canvas, from: f32, to: f32| {
            let part = if horizontal {
                RectF {
                    x: side.x + from,
                    w: to - from,
                    ..side
                }
            } else {
                RectF {
                    y: side.y + from,
                    h: to - from,
                    ..side
                }
            };
            self.fill(c, part, col);
        };
        match ls {
            cs::LineStyle::None => {}
            cs::LineStyle::Solid => seg(c, 0.0, len),
            cs::LineStyle::Dashed | cs::LineStyle::Dotted => {
                let (on, off) = if ls == cs::LineStyle::Dashed {
                    ((thick * 3.0).max(4.0), (thick * 2.0).max(3.0))
                } else {
                    (thick, thick)
                };
                let mut at = 0.0;
                while at < len {
                    seg(c, at, (at + on).min(len));
                    at += on + off;
                }
            }
            cs::LineStyle::Double => {
                let t = (thick / 3.0).max(1.0);
                let (a, z) = if horizontal {
                    (
                        RectF { h: t, ..side },
                        RectF {
                            y: side.y + side.h - t,
                            h: t,
                            ..side
                        },
                    )
                } else {
                    (
                        RectF { w: t, ..side },
                        RectF {
                            x: side.x + side.w - t,
                            w: t,
                            ..side
                        },
                    )
                };
                self.fill(c, a, col);
                self.fill(c, z, col);
            }
        }
    }

    /// `background-image`, tiled and positioned in the padding box.
    fn background_image(&self, c: &mut Canvas, b: &LBox, style: &Style, r: RectF) {
        let layer = cs::Mask {
            image: None,
            size: style.background_size,
            repeat: style.background_repeat,
            position: style.background_position,
        };
        let gradient = style.background_gradient.as_ref();
        let url = style.background_image.as_deref();
        // Layers listed first are painted last (on top).
        let on_top = gradient.is_some_and(|(_, top)| *top);
        if !on_top && let Some((g, _)) = gradient {
            self.image_layer(c, b, r, Layer::Gradient(g), &layer, None);
        }
        if let Some(url) = url {
            self.image_layer(c, b, r, Layer::Url(url), &layer, None);
        }
        if on_top && let Some((g, _)) = gradient {
            self.image_layer(c, b, r, Layer::Gradient(g), &layer, None);
        }
    }

    /// Paints image `url` sized, positioned and tiled per `layer` in the
    /// padding box of `b`. With `mask`, the image's alpha is used to paint
    /// that color instead (`mask-image`).
    fn image_layer(
        &self,
        c: &mut Canvas,
        b: &LBox,
        r: RectF,
        source: Layer,
        layer: &cs::Mask,
        mask: Option<Rgba>,
    ) {
        let pr = b.padding_rect();
        let area = RectF {
            x: r.x + pr.x,
            y: r.y + pr.y,
            ..pr
        };
        // Gradients have no natural size: they fill the area.
        let (iw, ih) = match source {
            Layer::Url(url) => {
                let Some(bm) = self.images.urls.get(url).and_then(|s| s.bitmap()) else {
                    return;
                };
                (bm.width as f32, bm.height as f32)
            }
            Layer::Gradient(_) => (area.w, area.h),
        };
        if iw <= 0.0 || ih <= 0.0 || area.w <= 0.0 || area.h <= 0.0 {
            return;
        }
        let (tw, th) = match layer.size {
            cs::BackgroundSize::Auto => (iw, ih),
            cs::BackgroundSize::Cover => {
                let k = (area.w / iw).max(area.h / ih);
                (iw * k, ih * k)
            }
            cs::BackgroundSize::Contain => {
                let k = (area.w / iw).min(area.h / ih);
                (iw * k, ih * k)
            }
            cs::BackgroundSize::Size(w, h) => {
                let w = (w != Dim::Auto).then(|| w.px_or_zero(area.w));
                let h = (h != Dim::Auto).then(|| h.px_or_zero(area.h));
                match (w, h) {
                    (Some(w), Some(h)) => (w, h),
                    (Some(w), None) => (w, w * ih / iw),
                    (None, Some(h)) => (h * iw / ih, h),
                    (None, None) => (iw, ih),
                }
            }
        };
        if tw < 0.5 || th < 0.5 {
            return;
        }
        let place = |d: Dim, room: f32| match d {
            Dim::Percent(p) => room * p / 100.0,
            other => other.px_or_zero(room),
        };
        let x0 = area.x + place(layer.position.0, area.w - tw);
        let y0 = area.y + place(layer.position.1, area.h - th);
        let s = self.scale;
        let (pw, ph) = (
            (tw * s).round().max(1.0) as u32,
            (th * s).round().max(1.0) as u32,
        );
        let px = match source {
            Layer::Url(url) => match self.images.scaled_url(url, pw, ph) {
                Some(px) => px,
                None => return,
            },
            Layer::Gradient(g) => self.images.gradient(g, pw, ph, s),
        };
        let saved = c.clip();
        c.set_clip(self.phys(r).intersect(&saved));
        let alpha: Option<Vec<u8>> = mask.map(|_| px.iter().map(|&p| (p >> 24) as u8).collect());
        let cs::Repeat(rx, ry) = layer.repeat;
        let start = |p0: f32, size: f32, lo: f32, rep: bool| {
            if rep {
                p0 - ((p0 - lo) / size).ceil().max(0.0) * size
            } else {
                p0
            }
        };
        let mut y = start(y0, th, r.y, ry);
        while y < r.y + r.h {
            let mut x = start(x0, tw, r.x, rx);
            while x < r.x + r.w {
                let (dx, dy) = ((x * s).round() as i32, (y * s).round() as i32);
                match (&alpha, mask) {
                    (Some(a), Some(col)) => {
                        c.draw_mask(dx, dy, pw as usize, ph as usize, a, col.rgb())
                    }
                    _ => c.draw_argb(dx, dy, pw as usize, ph as usize, &px),
                }
                if !rx {
                    break;
                }
                x += tw;
            }
            if !ry {
                break;
            }
            y += th;
        }
        c.set_clip(saved);
    }

    /// Content box of `b` placed at `r`.
    fn content_area(b: &LBox, r: RectF) -> RectF {
        let (cx, cy) = b.content_offset();
        RectF {
            x: r.x + cx,
            y: r.y + cy,
            w: (r.w - cx - b.border[cs::RIGHT] - b.padding[cs::RIGHT]).max(0.0),
            h: (r.h - cy - b.border[cs::BOTTOM] - b.padding[cs::BOTTOM]).max(0.0),
        }
    }

    /// Baseline for one line of `style` text centered in `area`.
    fn centered_baseline(&self, style: &Style, area: RectF) -> f32 {
        let m = self.fonts.line_metrics(style.font(), style.font_size);
        area.y + (area.h - (m.ascent + m.descent)) / 2.0 + m.ascent
    }

    fn centered_label(&self, c: &mut Canvas, b: &LBox, style: &Style, r: RectF, label: &str) {
        let area = Self::content_area(b, r);
        let w = self.fonts.measure(style.font(), style.font_size, label);
        let baseline = self.centered_baseline(style, area);
        self.text(
            c,
            style,
            area.x + (area.w - w) / 2.0,
            baseline,
            label,
            style.color,
        );
    }

    /// Whether box `k` is positioned (not `static`), which puts it in the
    /// positioned painting layers.
    fn is_positioned(&self, k: usize) -> bool {
        let b = &self.tree.boxes[k];
        b.absolute
            || (b.node == Some(b.style_node)
                && self
                    .style(b.style_node)
                    .is_some_and(|s| s.position != cs::Position::Static))
    }

    /// Where the legend of `<fieldset>` box `b` (at `r`) is, if it has one.
    fn legend_rect(&self, b: &LBox, r: RectF) -> Option<RectF> {
        if b.node.and_then(|n| self.dom.tag(n)) != Some("fieldset") {
            return None;
        }
        let k = b.children.iter().find(|&&k| {
            let kb = &self.tree.boxes[k];
            kb.node.and_then(|n| self.dom.tag(n)) == Some("legend") && !kb.absolute
        })?;
        let kb = &self.tree.boxes[*k];
        Some(RectF {
            x: r.x + kb.rect.x,
            y: r.y + kb.rect.y,
            ..kb.rect
        })
    }

    /// A fieldset's background and border start at the middle of its
    /// legend, and the top border leaves a gap for it (HTML rendering).
    fn fieldset(&self, c: &mut Canvas, b: &LBox, style: &Style, r: RectF, legend: RectF) {
        let top = (legend.y + legend.h / 2.0 - style.border(cs::TOP) / 2.0).max(r.y);
        let shown = RectF {
            y: top,
            h: (r.y + r.h - top).max(0.0),
            ..r
        };
        let saved = c.clip();
        let band = self.phys(RectF {
            x: r.x,
            y: top,
            w: r.w,
            h: style.border(cs::TOP),
        });
        let gap = self.phys(RectF {
            x: legend.x - 2.0,
            w: legend.w + 4.0,
            ..legend
        });
        // Everywhere but the part of the top border behind the legend.
        let parts = [
            Rect::new(
                saved.x,
                band.bottom(),
                saved.w,
                saved.bottom() - band.bottom(),
            ),
            Rect::new(saved.x, band.y, gap.x - saved.x, band.h),
            Rect::new(gap.right(), band.y, saved.right() - gap.right(), band.h),
        ];
        for part in parts {
            c.set_clip(part.intersect(&saved));
            self.decorations(c, b, style, shown);
        }
        c.set_clip(saved);
    }

    /// The value of an input as typed or set, else its `value` attribute.
    fn raw_value(&self, node: NodeId) -> String {
        self.inputs.get(&node).map_or_else(
            || self.dom.attr(node, "value").unwrap_or("").to_string(),
            |f| f.text(),
        )
    }

    /// A range slider: a track filled up to the value, and a round thumb.
    fn range(&self, c: &mut Canvas, b: &LBox, node: NodeId, r: RectF) {
        let area = Self::content_area(b, r);
        let s = self.scale;
        let (lo, hi) = crate::document::range_bounds(self.dom, node);
        let v = crate::document::range_value(self.dom, node, &self.raw_value(node));
        let f = if hi > lo {
            ((v - lo) / (hi - lo)) as f32
        } else {
            0.0
        };
        let thumb = area.h.min(16.0);
        let track_h = 4.0;
        let ty = area.y + (area.h - track_h) / 2.0;
        let (x0, x1) = (area.x + thumb / 2.0, area.x + area.w - thumb / 2.0);
        let at = x0 + (x1 - x0) * f;
        let disabled = self.dom.attr(node, "disabled").is_some();
        let accent = if disabled { 0x9aa0a6 } else { 0x0075ff };
        let track = (
            area.x * s,
            ty * s,
            area.w * s,
            track_h * s,
            track_h / 2.0 * s,
        );
        c.fill_rounded(track, None, 0xefefef, 255);
        let edge = (
            track.0 + 0.5 * s,
            track.1 + 0.5 * s,
            track.2 - s,
            track.3 - s,
            track.4,
        );
        c.fill_rounded(track, Some(edge), 0xb2b2b2, 255);
        let filled = (
            area.x * s,
            ty * s,
            (at - area.x) * s,
            track_h * s,
            track_h / 2.0 * s,
        );
        c.fill_rounded(filled, None, accent, 255);
        let knob = (
            (at - thumb / 2.0) * s,
            (area.y + (area.h - thumb) / 2.0) * s,
            thumb * s,
            thumb * s,
            thumb / 2.0 * s,
        );
        c.fill_rounded(knob, None, accent, 255);
    }

    /// A color input: its color in a swatch.
    fn color_swatch(&self, c: &mut Canvas, b: &LBox, node: NodeId, r: RectF) {
        let area = Self::content_area(b, r);
        let raw = self.raw_value(node);
        let color = crate::css::values::color(&raw, Rgba::BLACK)
            .filter(|_| raw.len() == 7 && raw.starts_with('#'))
            .unwrap_or(Rgba::BLACK);
        let inner = RectF {
            x: area.x + 2.0,
            y: area.y + 2.0,
            w: (area.w - 4.0).max(0.0),
            h: (area.h - 4.0).max(0.0),
        };
        self.fill(c, inner, Rgba::new(118, 118, 118, 255));
        let swatch = RectF {
            x: inner.x + 1.0,
            y: inner.y + 1.0,
            w: (inner.w - 2.0).max(0.0),
            h: (inner.h - 2.0).max(0.0),
        };
        self.fill(c, swatch, Rgba { a: 255, ..color });
    }

    /// A file input: a "Choose File" button and the (empty) file name.
    fn file_input(&self, c: &mut Canvas, b: &LBox, style: &Style, r: RectF) {
        use crate::document::{FILE_BUTTON, NO_FILE};
        let area = Self::content_area(b, r);
        let s = self.scale;
        let w = self
            .fonts
            .measure(style.font(), style.font_size, FILE_BUTTON)
            + 12.0;
        let button = (area.x * s, area.y * s, w * s, area.h * s, 2.0 * s);
        c.fill_rounded(button, None, 0xefefef, 255);
        let edge = (
            button.0 + s,
            button.1 + s,
            button.2 - 2.0 * s,
            button.3 - 2.0 * s,
            s,
        );
        c.fill_rounded(button, Some(edge), 0x767676, 255);
        let baseline = self.centered_baseline(style, area);
        self.text(c, style, area.x + 6.0, baseline, FILE_BUTTON, style.color);
        self.text(c, style, area.x + w + 6.0, baseline, NO_FILE, style.color);
    }

    /// A video's current frame (once it was started, or when it has no
    /// poster) or else its poster, and the control bar of `controls`.
    fn media_element(&self, c: &mut Canvas, b: &LBox, node: NodeId, style: &Style, r: RectF) {
        let el = self.media.get(&node);
        let video = self.dom.tag(node) == Some("video");
        let has_poster = matches!(self.images.get(node), Some(ImageState::Ready(..)));
        let frame = el
            .filter(|m| m.started || !has_poster)
            .and_then(|m| m.frame())
            .filter(|_| video);
        let area = Self::content_area(b, r);
        if let Some(f) = frame {
            let d = self.phys(object_rect(style, area, (f.width as f32, f.height as f32)));
            let saved = c.clip();
            c.set_clip(self.phys(area).intersect(&saved));
            c.blit_scaled(d, f.width as usize, f.height as usize, &f.rgb);
            c.set_clip(saved);
        } else if video && has_poster {
            self.image(c, b, node, style, r);
        }
        if self.dom.attr(node, "controls").is_some() {
            let info = el.map(|m| m.info()).unwrap_or_default();
            self.media_controls(c, area, &info, el.is_some_and(|m| m.muted), video);
        }
    }

    /// The control bar at the bottom of `area` (all of it for audio):
    /// play / pause, time, a progress track and the speaker.
    fn media_controls(
        &self,
        c: &mut Canvas,
        area: RectF,
        info: &crate::media::Info,
        muted: bool,
        video: bool,
    ) {
        let s = self.scale;
        let h = crate::document::media::CONTROLS_HEIGHT.min(area.h);
        let bar = RectF {
            y: area.y + area.h - h,
            h,
            ..area
        };
        let (fg, bg) = if video {
            (Rgba::new(255, 255, 255, 255), Rgba::new(0, 0, 0, 140))
        } else {
            (Rgba::new(32, 33, 36, 255), Rgba::new(241, 243, 244, 255))
        };
        let round = if video { 0.0 } else { h / 2.0 };
        c.fill_rounded(
            (bar.x * s, bar.y * s, bar.w * s, bar.h * s, round * s),
            None,
            bg.rgb(),
            bg.a,
        );
        let mid = bar.y + h / 2.0;
        // Play (a triangle) or pause (two bars).
        let (px, size) = (bar.x + 14.0, 12.0);
        if info.paused {
            for k in 0..(size as i32) {
                let y = mid - size / 2.0 + k as f32;
                let w = size * (1.0 - (2.0 * k as f32 / size - 1.0).abs()) * 0.85;
                self.fill(
                    c,
                    RectF {
                        x: px,
                        y,
                        w,
                        h: 1.0,
                    },
                    fg,
                );
            }
        } else {
            for dx in [0.0, 7.0] {
                self.fill(
                    c,
                    RectF {
                        x: px + dx,
                        y: mid - size / 2.0,
                        w: 4.0,
                        h: size,
                    },
                    fg,
                );
            }
        }
        // Time: current / total.
        let clock = |t: f64| {
            if !t.is_finite() {
                return "0:00".to_string();
            }
            let t = t.max(0.0) as u64;
            format!("{}:{:02}", t / 60, t % 60)
        };
        let label = format!("{} / {}", clock(info.current_time), clock(info.duration));
        let text_style = Style {
            font_size: 12.0,
            color: fg,
            ..Style::default()
        };
        let m = self.fonts.line_metrics(text_style.font(), 12.0);
        let tx = px + size + 12.0;
        self.text(
            c,
            &text_style,
            tx,
            mid + (m.ascent - m.descent) / 2.0,
            &label,
            fg,
        );
        // Progress track.
        let (x0, x1) = crate::document::media::progress_track(area);
        if x1 > x0 {
            let track = RectF {
                x: x0,
                y: mid - 2.0,
                w: x1 - x0,
                h: 4.0,
            };
            let faint = Rgba { a: 90, ..fg };
            c.fill_rounded(
                (track.x * s, track.y * s, track.w * s, track.h * s, 2.0 * s),
                None,
                faint.rgb(),
                faint.a,
            );
            let f = if info.duration.is_finite() && info.duration > 0.0 {
                (info.current_time / info.duration).clamp(0.0, 1.0) as f32
            } else {
                0.0
            };
            c.fill_rounded(
                (
                    track.x * s,
                    track.y * s,
                    track.w * f * s,
                    track.h * s,
                    2.0 * s,
                ),
                None,
                fg.rgb(),
                fg.a,
            );
        }
        // Speaker, crossed when muted.
        let sx = area.x + area.w - 26.0;
        self.fill(
            c,
            RectF {
                x: sx,
                y: mid - 3.0,
                w: 4.0,
                h: 6.0,
            },
            fg,
        );
        for k in 0..6 {
            let k = k as f32;
            self.fill(
                c,
                RectF {
                    x: sx + 4.0 + k,
                    y: mid - 3.0 - k,
                    w: 1.0,
                    h: 6.0 + 2.0 * k,
                },
                fg,
            );
        }
        if muted {
            let (ax, ay) = (((sx + 12.0) * s) as i32, ((mid - 4.0) * s) as i32);
            let d = (8.0 * s) as i32;
            c.line(ax, ay, ax + d, ay + d, s.max(1.0) as i32 + 1, fg.rgb());
            c.line(ax, ay + d, ax + d, ay, s.max(1.0) as i32 + 1, fg.rgb());
        }
    }

    /// `<progress>` (`progress`) or `<meter>`: a rounded track, filled to
    /// the value; a meter's fill is green, yellow or red by how close the
    /// value is to its optimum region.
    fn bar(&self, c: &mut Canvas, b: &LBox, node: NodeId, r: RectF, progress: bool) {
        let area = Self::content_area(b, r);
        let s = self.scale;
        let num = |a: &str| {
            self.dom
                .attr(node, a)
                .and_then(|v| v.trim().parse::<f32>().ok())
        };
        let (min, max) = if progress {
            (0.0, num("max").filter(|m| *m > 0.0).unwrap_or(1.0))
        } else {
            let min = num("min").unwrap_or(0.0);
            (min, num("max").unwrap_or(1.0).max(min))
        };
        let value = num("value").map(|v| v.clamp(min, max));
        let radius = (area.h / 2.0).min(4.0);
        let track = (area.x * s, area.y * s, area.w * s, area.h * s, radius * s);
        c.fill_rounded(track, None, 0xefefef, 255);
        let edge = (
            track.0 + 0.5 * s,
            track.1 + 0.5 * s,
            track.2 - s,
            track.3 - s,
            track.4,
        );
        c.fill_rounded(track, Some(edge), 0xb2b2b2, 255);
        // An indeterminate progress bar (no value) stays empty.
        let Some(v) = value.or((!progress).then_some(min)) else {
            return;
        };
        let f = if max > min {
            (v - min) / (max - min)
        } else {
            0.0
        };
        let color = if progress {
            0x0075ff
        } else {
            let low = num("low").unwrap_or(min).clamp(min, max);
            let high = num("high").unwrap_or(max).clamp(low, max);
            let optimum = num("optimum").unwrap_or((min + max) / 2.0);
            let region = |x: f32| -> i32 {
                if x < low {
                    0
                } else if x > high {
                    2
                } else {
                    1
                }
            };
            match (region(optimum) - region(v)).abs() {
                0 => 0x107c10,
                1 => 0xffb900,
                _ => 0xd13438,
            }
        };
        let fill = (
            area.x * s,
            area.y * s,
            area.w * f * s,
            area.h * s,
            radius * s,
        );
        c.fill_rounded(fill, None, color, 255);
    }

    /// A checkbox or radio button.
    fn check(&self, c: &mut Canvas, b: &LBox, node: NodeId, kind: Control, r: RectF) {
        let area = Self::content_area(b, r);
        let s = self.scale;
        let size = area.w.min(area.h).max(8.0);
        let (x, y) = (
            area.x + (area.w - size) / 2.0,
            area.y + (area.h - size) / 2.0,
        );
        let on = self.forms.checked(self.dom, node);
        let disabled = self.dom.attr(node, "disabled").is_some();
        let accent = if disabled { 0x9aa0a6 } else { 0x0060df };
        let round = if kind == Control::Radio {
            size / 2.0
        } else {
            2.5
        };
        let outer = (x * s, y * s, size * s, size * s, round * s);
        let t = s.max(1.0);
        let inner = (
            outer.0 + t,
            outer.1 + t,
            outer.2 - 2.0 * t,
            outer.3 - 2.0 * t,
            (round * s - t).max(0.0),
        );
        let filled = on && kind == Control::Checkbox;
        c.fill_rounded(outer, None, if filled { accent } else { 0xffffff }, 255);
        let edge = if on { accent } else { 0x767676 };
        c.fill_rounded(outer, Some(inner), edge, 255);
        if on && kind == Control::Checkbox {
            let p = |fx: f32, fy: f32| (((x + fx * size) * s) as i32, ((y + fy * size) * s) as i32);
            let (a, m, z) = (p(0.22, 0.52), p(0.42, 0.72), p(0.78, 0.3));
            let w = (size * s / 7.0).max(1.5) as i32;
            c.line(a.0, a.1, m.0, m.1, w, 0xffffff);
            c.line(m.0, m.1, z.0, z.1, w, 0xffffff);
        } else if on {
            let d = size * 0.5;
            let dot = (
                (x + (size - d) / 2.0) * s,
                (y + (size - d) / 2.0) * s,
                d * s,
                d * s,
                d * s / 2.0,
            );
            c.fill_rounded(dot, None, accent, 255);
        }
    }

    /// A `<select>`: the chosen option and a drop-down arrow.
    fn select(&self, c: &mut Canvas, b: &LBox, node: NodeId, style: &Style, r: RectF) {
        let area = Self::content_area(b, r);
        let label = self
            .forms
            .selected_index(self.dom, node)
            .map(|i| option_label(self.dom, options(self.dom, node)[i]))
            .unwrap_or_default();
        let baseline = self.centered_baseline(style, area);
        let saved = c.clip();
        c.set_clip(self.phys(area).intersect(&saved));
        self.text(c, style, area.x, baseline, &label, style.color);
        c.set_clip(saved);
        let arrow_w = self
            .fonts
            .measure(style.font(), style.font_size, "\u{25be}");
        self.text(
            c,
            style,
            area.x + area.w - arrow_w,
            baseline,
            "\u{25be}",
            style.color,
        );
    }

    /// A multi-line `<textarea>` with selection and caret.
    fn textarea(&self, c: &mut Canvas, b: &LBox, node: NodeId, style: &Style, r: RectF) {
        let area = Self::content_area(b, r);
        let field = self.inputs.get(&node);
        let text = field.map_or_else(
            || {
                let t = self.dom.text_content(node);
                t.strip_prefix('\n').unwrap_or(&t).to_string()
            },
            TextField::text,
        );
        let chars: Vec<char> = text.chars().collect();
        let lh = style.line_height_px();
        let m = self.fonts.line_metrics(style.font(), style.font_size);
        let scroll_y = field.map_or(0.0, TextField::scroll_y);
        let focused = self.focus == Some(node);
        let (sel_s, sel_e) = field.map_or((0, 0), TextField::selection);
        let caret = field.map_or(chars.len(), TextField::caret);
        let saved = c.clip();
        c.set_clip(
            self.phys(RectF {
                x: area.x - 1.0,
                w: area.w + 2.0,
                ..area
            })
            .intersect(&saved),
        );
        if chars.is_empty() && !focused {
            if let Some(ph) = self.dom.attr(node, "placeholder") {
                let baseline = area.y + (lh - (m.ascent + m.descent)) / 2.0 + m.ascent;
                self.text(c, style, area.x, baseline, ph, PLACEHOLDER);
            }
        }
        let mut start = 0;
        for (i, line) in text.split('\n').enumerate() {
            let n = line.chars().count();
            let top = area.y - scroll_y + i as f32 * lh;
            let baseline = top + (lh - (m.ascent + m.descent)) / 2.0 + m.ascent;
            if top + lh >= area.y && top <= area.y + area.h {
                let xs =
                    TextField::positions(&self.fonts.advances(style.font(), style.font_size, line));
                if focused && sel_s != sel_e {
                    let (a, z) = (sel_s.max(start), sel_e.min(start + n));
                    if a < z || (sel_s <= start + n && sel_e > start + n) {
                        let x0 = xs[a.saturating_sub(start).min(n)];
                        let x1 = if sel_e > start + n {
                            xs[n] + 4.0
                        } else {
                            xs[z - start]
                        };
                        let sel = RectF {
                            x: area.x + x0,
                            y: top,
                            w: x1 - x0,
                            h: lh,
                        };
                        self.fill(c, sel, SELECTION);
                    }
                }
                self.text(c, style, area.x, baseline, line, style.color);
                if focused && self.caret && caret >= start && caret <= start + n {
                    let bar = RectF {
                        x: area.x + xs[caret - start],
                        y: baseline - m.ascent,
                        w: 1.0,
                        h: m.ascent + m.descent,
                    };
                    self.fill(c, bar, style.color);
                }
            }
            start += n + 1;
        }
        c.set_clip(saved);
    }

    /// Draws an input's value (or placeholder), selection and caret.
    fn input(&self, c: &mut Canvas, b: &LBox, node: NodeId, style: &Style, r: RectF) {
        let (cx, cy) = b.content_offset();
        let area = RectF {
            x: r.x + cx,
            y: r.y + cy,
            w: (r.w - cx - b.border[cs::RIGHT] - b.padding[cs::RIGHT]).max(0.0),
            h: (r.h - cy - b.border[cs::BOTTOM] - b.padding[cs::BOTTOM]).max(0.0),
        };
        let field = self.inputs.get(&node);
        let focused = self.focus == Some(node);
        let value = field.map_or_else(
            || self.dom.attr(node, "value").unwrap_or("").to_string(),
            TextField::text,
        );
        let shown = display_value(self.dom, node, &value);
        let m = self.fonts.line_metrics(style.font(), style.font_size);
        let baseline = area.y + (area.h - (m.ascent + m.descent)) / 2.0 + m.ascent;
        let saved = c.clip();
        // Let the caret show at the very edges.
        let clip = self.phys(RectF {
            x: area.x - 1.0,
            w: area.w + 2.0,
            ..area
        });
        c.set_clip(clip.intersect(&saved));
        if shown.is_empty() {
            if let Some(ph) = self.dom.attr(node, "placeholder") {
                self.text(c, style, area.x, baseline, ph, PLACEHOLDER);
            }
        } else {
            let x0 = area.x - field.map_or(0.0, TextField::scroll);
            if let Some(f) = field.filter(|_| focused) {
                let xs = TextField::positions(&self.fonts.advances(
                    style.font(),
                    style.font_size,
                    &shown,
                ));
                let (s, e) = f.selection();
                if s != e {
                    let sel = RectF {
                        x: x0 + xs[s],
                        y: baseline - m.ascent,
                        w: xs[e] - xs[s],
                        h: m.ascent + m.descent,
                    };
                    self.fill(c, sel, SELECTION);
                }
            }
            self.text(c, style, x0, baseline, &shown, style.color);
        }
        if focused && self.caret {
            let xs =
                TextField::positions(&self.fonts.advances(style.font(), style.font_size, &shown));
            let caret = field.map_or(0, TextField::caret).min(xs.len() - 1);
            let x0 = area.x - field.map_or(0.0, TextField::scroll);
            let bar = RectF {
                x: x0 + xs[caret],
                y: baseline - m.ascent,
                w: 1.0,
                h: m.ascent + m.descent,
            };
            self.fill(c, bar, style.color);
        }
        c.set_clip(saved);
    }

    /// Draws an image scaled to the content box, or its `alt` text.
    fn image(&self, c: &mut Canvas, b: &LBox, node: NodeId, style: &Style, r: RectF) {
        let (cx, cy) = b.content_offset();
        let area = RectF {
            x: r.x + cx,
            y: r.y + cy,
            w: (r.w - cx - b.border[cs::RIGHT] - b.padding[cs::RIGHT]).max(0.0),
            h: (r.h - cy - b.border[cs::BOTTOM] - b.padding[cs::BOTTOM]).max(0.0),
        };
        let p = self.phys(area);
        match self.images.get(node) {
            Some(ImageState::Ready(_, bitmap)) => {
                let density = self.images.density.get(&node).copied().unwrap_or(1.0);
                let natural = (
                    bitmap.width as f32 / density,
                    bitmap.height as f32 / density,
                );
                let d = self.phys(object_rect(style, area, natural));
                if d.w > 0
                    && d.h > 0
                    && let Some(px) = self.images.scaled(node, d.w as u32, d.h as u32)
                {
                    // The content box clips what does not fit (`cover`).
                    let saved = c.clip();
                    c.set_clip(p.intersect(&saved));
                    c.draw_argb(d.x, d.y, d.w as usize, d.h as usize, &px);
                    c.set_clip(saved);
                }
            }
            Some(ImageState::Failed(_)) | None => {
                let Some(alt) = self.dom.attr(node, "alt").filter(|a| !a.is_empty()) else {
                    return;
                };
                let gray = Rgba::new(160, 160, 160, 255);
                for side in [
                    RectF { h: 1.0, ..area },
                    RectF {
                        y: area.y + area.h - 1.0,
                        h: 1.0,
                        ..area
                    },
                    RectF { w: 1.0, ..area },
                    RectF {
                        x: area.x + area.w - 1.0,
                        w: 1.0,
                        ..area
                    },
                ] {
                    self.fill(c, side, gray);
                }
                let m = self.fonts.line_metrics(style.font(), style.font_size);
                let saved = c.clip();
                c.set_clip(p.intersect(&saved));
                let baseline = area.y + (area.h - (m.ascent + m.descent)) / 2.0 + m.ascent;
                self.text(c, style, area.x + 4.0, baseline, alt, style.color);
                c.set_clip(saved);
            }
            Some(ImageState::Loading(_)) => {}
        }
    }

    fn decorations(&self, c: &mut Canvas, b: &LBox, style: &Style, r: RectF) {
        let s = self.scale;
        let bw = [0, 1, 2, 3].map(|i| style.border(i));
        let radius = style.radius(r.w, r.h);
        let uniform = bw.iter().all(|&w| w == bw[0])
            && style
                .border_color
                .iter()
                .all(|&col| col == style.border_color[0])
            && style
                .border_style
                .iter()
                .all(|&ls| matches!(ls, cs::LineStyle::Solid | cs::LineStyle::None));
        if let Some(url) = &style.mask.image {
            // An icon: the mask shapes the background color.
            if style.background.is_visible() {
                self.image_layer(
                    c,
                    b,
                    r,
                    Layer::Url(url),
                    &style.mask,
                    Some(style.background),
                );
            }
            return;
        }
        for shadow in style.box_shadow.iter() {
            self.box_shadow(c, shadow, r, radius);
        }
        let has_layers = style.background_image.is_some() || style.background_gradient.is_some();
        if has_layers && radius <= 0.0 {
            self.fill(c, r, style.background);
            self.background_image(c, b, style, r);
            return self.borders(c, style, r, bw);
        }
        if radius > 0.0 {
            let outer = (r.x * s, r.y * s, r.w * s, r.h * s, radius * s);
            if style.background.is_visible() {
                c.fill_rounded(outer, None, style.background.rgb(), style.background.a);
            }
            if has_layers {
                let saved = c.round_clip();
                c.set_round_clip(Some([outer.0, outer.1, outer.2, outer.3, outer.4]));
                self.background_image(c, b, style, r);
                c.set_round_clip(saved);
            }
            if bw[0] > 0.0 && uniform {
                let w = bw[0];
                let inner = (
                    (r.x + w) * s,
                    (r.y + w) * s,
                    (r.w - 2.0 * w) * s,
                    (r.h - 2.0 * w) * s,
                    (radius - w).max(0.0) * s,
                );
                let col = style.border_color[0];
                c.fill_rounded(outer, Some(inner), col.rgb(), col.a);
            }
            if uniform {
                return;
            }
            let solid = style
                .border_style
                .iter()
                .all(|&ls| matches!(ls, cs::LineStyle::Solid | cs::LineStyle::None));
            if solid {
                // Per-side widths and colors around rounded corners.
                let inner = (
                    (r.x + bw[cs::LEFT]) * s,
                    (r.y + bw[cs::TOP]) * s,
                    (r.w - bw[cs::LEFT] - bw[cs::RIGHT]) * s,
                    (r.h - bw[cs::TOP] - bw[cs::BOTTOM]) * s,
                    (radius - bw.iter().copied().fold(0.0, f32::max)).max(0.0) * s,
                );
                let colors = style.border_color.map(|c| (c.rgb(), c.a));
                c.fill_rounded_sides(outer, inner, colors, bw.map(|w| w * s));
                return;
            }
        } else {
            self.fill(c, r, style.background);
        }
        self.borders(c, style, r, bw);
    }

    /// An outer `box-shadow`: the box shape offset and spread, blurred by
    /// stacking translucent rounded rectangles.
    fn box_shadow(&self, c: &mut Canvas, sh: &cs::Shadow, r: RectF, radius: f32) {
        if sh.inset || !sh.color.is_visible() {
            return;
        }
        let s = self.scale;
        let base = RectF {
            x: r.x + sh.x - sh.spread,
            y: r.y + sh.y - sh.spread,
            w: r.w + 2.0 * sh.spread,
            h: r.h + 2.0 * sh.spread,
        };
        let blur = sh.blur.max(0.0);
        let steps = if blur < 1.0 {
            1
        } else {
            (blur * s).ceil().min(12.0) as usize
        };
        // Each ring adds a little alpha; together they fade out over `blur`.
        let alpha = sh.color.a as f32 / steps as f32;
        for i in 0..steps {
            let grow = if steps == 1 {
                0.0
            } else {
                blur * (i as f32 / (steps - 1) as f32 - 0.5)
            };
            let g = RectF {
                x: base.x - grow,
                y: base.y - grow,
                w: base.w + 2.0 * grow,
                h: base.h + 2.0 * grow,
            };
            if g.w <= 0.0 || g.h <= 0.0 {
                continue;
            }
            let rr = (radius + sh.spread + grow).max(0.0);
            // Shadows only show outside the border box.
            c.fill_rounded(
                (g.x * s, g.y * s, g.w * s, g.h * s, rr * s),
                Some((r.x * s, r.y * s, r.w * s, r.h * s, radius * s)),
                sh.color.rgb(),
                alpha.round().max(1.0) as u8,
            );
        }
    }

    /// Four border sides (square corners).
    fn borders(&self, c: &mut Canvas, style: &Style, r: RectF, bw: [f32; 4]) {
        let sides = [
            RectF { h: bw[0], ..r },
            RectF {
                x: r.x + r.w - bw[1],
                w: bw[1],
                ..r
            },
            RectF {
                y: r.y + r.h - bw[2],
                h: bw[2],
                ..r
            },
            RectF { w: bw[3], ..r },
        ];
        for (i, side) in sides.into_iter().enumerate() {
            if bw[i] > 0.0 {
                let ls = style.border_style[i];
                self.line_side(c, side, i % 2 == 0, ls, style.border_color[i]);
            }
        }
    }

    fn marker(&self, c: &mut Canvas, b: &LBox, style: &Style, r: RectF) {
        let (cx, cy) = b.content_offset();
        let m = self.fonts.line_metrics(style.font(), style.font_size);
        let first_baseline = b
            .inline
            .as_ref()
            .and_then(|il| il.lines.first())
            .map_or(m.ascent, |l| l.top + l.baseline);
        let baseline = r.y + cy + first_baseline;
        let x = r.x + cx;
        let fs = style.font_size;
        let color = style.color;
        match style.list_style {
            ListStyle::None => {}
            ListStyle::DisclosureClosed | ListStyle::DisclosureOpen => {
                let arrow = if style.list_style == ListStyle::DisclosureOpen {
                    "\u{25be} "
                } else {
                    "\u{25b8} "
                };
                let w = self.fonts.measure(style.font(), fs, arrow);
                self.text(c, style, x - w, baseline, arrow, color);
            }
            ListStyle::Decimal => {
                let n = self.list_index(b.node);
                let text = format!("{n}. ");
                let w = self.fonts.measure(style.font(), fs, &text);
                self.text(c, style, x - w, baseline, &text, color);
            }
            shape => {
                let d = (fs * 0.35).max(4.0);
                let sq = RectF {
                    x: x - fs * 0.9,
                    y: baseline - m.ascent * 0.35 - d / 2.0,
                    w: d,
                    h: d,
                };
                let s = self.scale;
                let rr = (sq.x * s, sq.y * s, sq.w * s, sq.h * s);
                let round = if shape == ListStyle::Square {
                    0.0
                } else {
                    d * s / 2.0
                };
                let outer = (rr.0, rr.1, rr.2, rr.3, round);
                let inner = (shape == ListStyle::Circle).then(|| {
                    let t = s.max(1.0);
                    (
                        rr.0 + t,
                        rr.1 + t,
                        rr.2 - 2.0 * t,
                        rr.3 - 2.0 * t,
                        (round - t).max(0.0),
                    )
                });
                c.fill_rounded(outer, inner, color.rgb(), color.a);
            }
        }
    }

    /// 1-based position of a list item among its element siblings.
    fn list_index(&self, node: Option<NodeId>) -> usize {
        let Some(mut n) = node else { return 1 };
        let mut i = 1;
        while let Some(prev) = self.dom.previous_element_sibling(n) {
            if self.dom.tag(prev) == Some("li") {
                i += 1;
            }
            n = prev;
        }
        i
    }

    /// Paints inline content at `x, y`. With `ellipsis` (the end edge and
    /// the block's style), lines reaching past the edge are cut and end in
    /// `…`.
    fn inline(
        &self,
        c: &mut Canvas,
        il: &crate::layout::inline::InlineLayout,
        x: f32,
        y: f32,
        ellipsis: Option<(f32, &Style)>,
    ) {
        for line in &il.lines {
            let baseline = y + line.top + line.baseline;
            let cut;
            let items = match ellipsis {
                Some((end, block)) if x + line.width > end + 0.5 => {
                    cut = self.cut_line(&line.items, x, end, block);
                    let (items, at) = &cut;
                    self.text(c, block, *at, baseline, "…", block.color);
                    items
                }
                _ => &line.items,
            };
            for item in items {
                match item {
                    Placed::Text {
                        node,
                        x: tx,
                        width,
                        text,
                        shift,
                        ..
                    } => {
                        let baseline = baseline + shift;
                        let Some(st) = self.style(*node) else {
                            continue;
                        };
                        if !st.visible || st.clipped_away {
                            continue;
                        }
                        let m = self.fonts.line_metrics(st.font(), st.font_size);
                        // Background of the inline element around this text.
                        if let Some(parent) = self.dom.parent(*node)
                            && let Some(ps) = self.style(parent)
                            && ps.display == Display::Inline
                            && ps.background.is_visible()
                        {
                            let bg = RectF {
                                x: x + tx,
                                y: baseline - m.ascent,
                                w: *width,
                                h: m.ascent + m.descent,
                            };
                            self.fill(c, bg, ps.background);
                        }
                        self.text(c, st, x + tx, baseline, text, st.color);
                        let thick = (st.font_size / 14.0).max(1.0);
                        if st.underline {
                            let u = RectF {
                                x: x + tx,
                                y: baseline + m.descent * 0.35,
                                w: *width,
                                h: thick,
                            };
                            self.fill(c, u, st.color);
                        }
                        if st.line_through {
                            let u = RectF {
                                x: x + tx,
                                y: baseline - m.ascent * 0.3,
                                w: *width,
                                h: thick,
                            };
                            self.fill(c, u, st.color);
                        }
                    }
                    Placed::Atomic {
                        index,
                        x: ax,
                        y: ay,
                        ..
                    } => {
                        self.paint_box(c, *index, x + ax, y + line.top + ay);
                    }
                }
            }
        }
    }

    /// Paints vertical inline content in content box `area`: each line is
    /// a column (right to left if `rl`); CJK characters stand upright and
    /// other runs are turned 90° clockwise (`text-orientation: mixed`).
    fn vertical_inline(
        &self,
        c: &mut Canvas,
        il: &crate::layout::inline::InlineLayout,
        area: RectF,
        rl: bool,
    ) {
        for line in &il.lines {
            let col_x = if rl {
                area.x + area.w - line.top - line.height
            } else {
                area.x + line.top
            };
            for item in &line.items {
                let Placed::Text {
                    node, x: tx, text, ..
                } = item
                else {
                    continue;
                };
                let Some(st) = self.style(*node) else {
                    continue;
                };
                if !st.visible || st.clipped_away {
                    continue;
                }
                let m = self.fonts.line_metrics(st.font(), st.font_size);
                let mut y = area.y + tx;
                // Runs of upright and sideways characters.
                let mut run = String::new();
                let mut upright_run = None;
                let mut flush = |run: &mut String, upright: bool, y: &mut f32| {
                    if run.is_empty() {
                        return;
                    }
                    let w = self.fonts.measure(st.font(), st.font_size, run);
                    if upright {
                        for ch in run.chars() {
                            let s = ch.to_string();
                            let cw = self.fonts.measure(st.font(), st.font_size, &s);
                            let x = col_x + (line.height - cw) / 2.0;
                            self.text(c, st, x, *y + m.ascent, &s, st.color);
                            *y += cw;
                        }
                    } else {
                        let thick = m.ascent + m.descent;
                        let x = col_x + (line.height - thick) / 2.0;
                        self.sideways(c, st, run, x, *y, w, m.ascent, thick);
                        *y += w;
                    }
                    run.clear();
                };
                for ch in text.chars() {
                    let upright = crate::layout::inline::is_cjk(ch);
                    if upright_run.is_some_and(|u| u != upright) {
                        flush(&mut run, upright_run.unwrap(), &mut y);
                    }
                    upright_run = Some(upright);
                    run.push(ch);
                }
                if let Some(u) = upright_run {
                    flush(&mut run, u, &mut y);
                }
            }
        }
    }

    /// Draws `text` turned 90° clockwise with its top-left at `(x, y)`
    /// (it is `w` long and `thick` across; `ascent` above its baseline).
    #[allow(clippy::too_many_arguments)]
    fn sideways(
        &self,
        c: &mut Canvas,
        st: &Style,
        text: &str,
        x: f32,
        y: f32,
        w: f32,
        ascent: f32,
        thick: f32,
    ) {
        let s = self.scale;
        let (pw, ph) = ((w * s).ceil() as usize + 1, (thick * s).ceil() as usize + 1);
        if pw == 0 || ph == 0 || pw * ph > 4_000_000 {
            return;
        }
        // Over white and over black, which gives color and coverage.
        let mut white = Canvas::new(pw, ph);
        white.clear(0xff_ffff);
        let mut black = Canvas::new(pw, ph);
        for layer in [&mut white, &mut black] {
            self.fonts.draw(
                layer,
                st.font(),
                st.font_size * s,
                0.0,
                ascent * s,
                text,
                st.color.rgb(),
            );
        }
        // Turned clockwise: column i of the run becomes row i, row j
        // becomes column (ph - 1 - j).
        let mut argb = vec![0u32; pw * ph];
        for j in 0..ph {
            for i in 0..pw {
                let (wp, bp) = (white.pixels()[j * pw + i], black.pixels()[j * pw + i]);
                let ch = |p: u32, k: u32| ((p >> k) & 0xff) as i32;
                let cover = 255 - (0..3).map(|k| ch(wp, k * 8) - ch(bp, k * 8)).sum::<i32>() / 3;
                if cover <= 0 {
                    continue;
                }
                let un = |k: u32| ((ch(bp, k) * 255 / cover).clamp(0, 255)) as u32;
                let color = (cover as u32) << 24 | un(16) << 16 | un(8) << 8 | un(0);
                argb[i * ph + (ph - 1 - j)] = color;
            }
        }
        c.draw_argb(
            (x * s).round() as i32,
            (y * s).round() as i32,
            ph,
            pw,
            &argb,
        );
    }

    /// The items of a line cut so `…` (in `block`'s font) fits before
    /// `end`, and where the `…` goes.
    fn cut_line(&self, items: &[Placed], x: f32, end: f32, block: &Style) -> (Vec<Placed>, f32) {
        let mark = self.fonts.measure(block.font(), block.font_size, "…");
        let limit = end - mark;
        let mut out = Vec::new();
        let mut at = x;
        for item in items {
            match item {
                Placed::Text {
                    node,
                    x: tx,
                    width,
                    text,
                    shift,
                    ..
                } => {
                    if x + tx + width <= limit {
                        out.push(item.clone());
                        at = x + tx + width;
                        continue;
                    }
                    let st = self.style(*node).unwrap_or(block);
                    let mut kept = String::new();
                    let mut w = 0.0;
                    for ch in text.chars() {
                        let mut next = kept.clone();
                        next.push(ch);
                        let nw = self.fonts.measure(st.font(), st.font_size, &next);
                        if x + tx + nw > limit {
                            break;
                        }
                        (kept, w) = (next, nw);
                    }
                    // Spaces before the ellipsis are dropped.
                    let trimmed = kept.trim_end();
                    if trimmed.len() < kept.len() {
                        w = self.fonts.measure(st.font(), st.font_size, trimmed);
                    }
                    if !trimmed.is_empty() {
                        out.push(Placed::Text {
                            node: *node,
                            x: *tx,
                            width: w,
                            text: trimmed.to_string(),
                            shift: *shift,
                            space: 0.0,
                        });
                    }
                    return (out, x + tx + w);
                }
                Placed::Atomic { x: ax, width, .. } => {
                    if x + ax + width > limit {
                        return (out, at);
                    }
                    out.push(item.clone());
                    at = x + ax + width;
                }
            }
        }
        (out, at)
    }

    fn text(&self, c: &mut Canvas, st: &Style, x: f32, baseline: f32, text: &str, color: Rgba) {
        let s = self.scale;
        self.fonts.draw(
            c,
            st.font(),
            st.font_size * s,
            x * s,
            baseline * s,
            text,
            color.rgb(),
        );
    }

    /// A thin overlay scrollbar on the right of `r` if content overflows.
    fn scrollbar(&self, c: &mut Canvas, r: RectF, content_h: f32, sy: f32) {
        if content_h <= r.h + 0.5 || r.h <= 0.0 {
            return;
        }
        let track = r.h - 4.0;
        let thumb_h = (track * r.h / content_h).max(20.0).min(track);
        let max_scroll = content_h - r.h;
        let t = if max_scroll > 0.0 {
            sy / max_scroll
        } else {
            0.0
        };
        let thumb = RectF {
            x: r.x + r.w - 8.0,
            y: r.y + 2.0 + (track - thumb_h) * t.clamp(0.0, 1.0),
            w: 6.0,
            h: thumb_h,
        };
        let s = self.scale;
        c.fill_rounded(
            (thumb.x * s, thumb.y * s, thumb.w * s, thumb.h * s, 3.0 * s),
            None,
            SCROLLBAR.rgb(),
            SCROLLBAR.a,
        );
    }
}

/// The text an input shows for `value` (bullets for passwords).
pub fn display_value(dom: &Dom, node: NodeId, value: &str) -> String {
    if dom
        .attr(node, "type")
        .is_some_and(|t| t.eq_ignore_ascii_case("password"))
    {
        "\u{2022}".repeat(value.chars().count())
    } else {
        value.to_string()
    }
}

/// Draws the option list of an open `<select>` over the page.
#[allow(clippy::too_many_arguments)]
pub fn select_popup(
    c: &mut Canvas,
    fonts: &Fonts,
    scale: f32,
    origin: (f32, f32),
    rect: RectF,
    items: &[String],
    selected: Option<usize>,
    hover: Option<usize>,
    style: &Style,
) {
    let s = scale;
    let r = RectF {
        x: rect.x + origin.0,
        y: rect.y + origin.1,
        ..rect
    };
    let px = |v: RectF| (v.x * s, v.y * s, v.w * s, v.h * s);
    let (x, y, w, h) = px(r);
    // Soft shadow, frame, background.
    c.fill_rounded(
        (x + 1.0 * s, y + 2.0 * s, w, h, 4.0 * s),
        None,
        0x000000,
        40,
    );
    c.fill_rounded((x, y, w, h, 4.0 * s), None, 0x8f8f9d, 255);
    c.fill_rounded(
        (x + s, y + s, w - 2.0 * s, h - 2.0 * s, 3.0 * s),
        None,
        0xffffff,
        255,
    );
    let row = (r.h - 2.0) / items.len().max(1) as f32;
    let m = fonts.line_metrics(style.font(), style.font_size);
    for (i, label) in items.iter().enumerate() {
        let top = r.y + 1.0 + i as f32 * row;
        let bg = if hover == Some(i) {
            Some(0xd2e3fc)
        } else if selected == Some(i) {
            Some(0xe8f0fe)
        } else {
            None
        };
        if let Some(bg) = bg {
            let (bx, by, bw, bh) = px(RectF {
                x: r.x + 1.0,
                y: top,
                w: r.w - 2.0,
                h: row,
            });
            c.fill_rect(bx as i32, by as i32, bw.ceil() as i32, bh.ceil() as i32, bg);
        }
        let baseline = top + (row - (m.ascent + m.descent)) / 2.0 + m.ascent;
        fonts.draw(
            c,
            style.font(),
            style.font_size * s,
            (r.x + 8.0) * s,
            baseline * s,
            label,
            style.color.rgb(),
        );
    }
}

/// The color of `canvas` at fractional pixel `(x, y)` (pixel centers at
/// whole numbers), as `[r, g, b]`; outside it is `outside`.
fn bilinear(canvas: &Canvas, x: f32, y: f32, outside: u32) -> [f32; 3] {
    let (w, h) = (canvas.width() as i32, canvas.height() as i32);
    let px = canvas.pixels();
    let at = |x: i32, y: i32| {
        if x < 0 || y < 0 || x >= w || y >= h {
            outside
        } else {
            px[(y * w + x) as usize]
        }
    };
    let (x0, y0) = (x.floor(), y.floor());
    let (fx, fy) = (x - x0, y - y0);
    let (x0, y0) = (x0 as i32, y0 as i32);
    let p = [
        at(x0, y0),
        at(x0 + 1, y0),
        at(x0, y0 + 1),
        at(x0 + 1, y0 + 1),
    ];
    let weights = [
        (1.0 - fx) * (1.0 - fy),
        fx * (1.0 - fy),
        (1.0 - fx) * fy,
        fx * fy,
    ];
    let mut out = [0.0; 3];
    for (c, wgt) in p.iter().zip(weights) {
        for (k, shift) in [16, 8, 0].into_iter().enumerate() {
            out[k] += ((c >> shift) & 0xff) as f32 * wgt;
        }
    }
    out
}

/// Where a replaced element's content goes in its content box `area`
/// (`object-fit` / `object-position`), given its natural size.
fn object_rect(style: &Style, area: RectF, (nw, nh): (f32, f32)) -> RectF {
    let (w, h) = if nw <= 0.0 || nh <= 0.0 {
        (area.w, area.h)
    } else {
        let contain = (area.w / nw).min(area.h / nh);
        match style.object_fit {
            cs::ObjectFit::Fill => (area.w, area.h),
            cs::ObjectFit::Contain => (nw * contain, nh * contain),
            cs::ObjectFit::Cover => {
                let k = (area.w / nw).max(area.h / nh);
                (nw * k, nh * k)
            }
            cs::ObjectFit::None => (nw, nh),
            cs::ObjectFit::ScaleDown => {
                let k = contain.min(1.0);
                (nw * k, nh * k)
            }
        }
    };
    // Percentages place the same point of image and box together.
    let (px, py) = style.object_position;
    RectF {
        x: area.x + px.px_or_zero(area.w - w),
        y: area.y + py.px_or_zero(area.h - h),
        w,
        h,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn object_fit_places_the_image() {
        let area = RectF {
            x: 0.0,
            y: 0.0,
            w: 100.0,
            h: 100.0,
        };
        let at = |fit, position| {
            let style = Style {
                object_fit: fit,
                object_position: position,
                ..Style::default()
            };
            let r = object_rect(&style, area, (200.0, 100.0));
            (r.x, r.y, r.w, r.h)
        };
        let center = (Dim::Percent(50.0), Dim::Percent(50.0));
        assert_eq!(at(cs::ObjectFit::Fill, center), (0.0, 0.0, 100.0, 100.0));
        assert_eq!(at(cs::ObjectFit::Contain, center), (0.0, 25.0, 100.0, 50.0));
        assert_eq!(at(cs::ObjectFit::Cover, center), (-50.0, 0.0, 200.0, 100.0));
        assert_eq!(at(cs::ObjectFit::None, center), (-50.0, 0.0, 200.0, 100.0));
        assert_eq!(
            at(cs::ObjectFit::ScaleDown, center),
            (0.0, 25.0, 100.0, 50.0)
        );
        let left = (Dim::Percent(0.0), Dim::Percent(50.0));
        assert_eq!(at(cs::ObjectFit::Cover, left), (0.0, 0.0, 200.0, 100.0));
    }
}
