//! Painting a laid-out document onto a [`Canvas`].
//!
//! Boxes are painted in tree order (backgrounds, borders, then content);
//! absolutely positioned boxes are painted after their in-flow siblings.
//! Coordinates are logical; `scale` converts to physical pixels.

use std::collections::HashMap;

use crate::css::Style;
use crate::css::style::{self as cs, Display, ListStyle};
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
}

const SELECTION: Rgba = Rgba::new(51, 132, 255, 90);
const PLACEHOLDER: Rgba = Rgba::new(117, 117, 117, 255);

const SCROLLBAR: Rgba = Rgba::new(0, 0, 0, 90);

impl Painter<'_> {
    fn style(&self, id: NodeId) -> Option<&Style> {
        self.styles.get(id).and_then(Option::as_ref)
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
        let vp = &self.tree.boxes[LayoutTree::VIEWPORT];
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
        let r = RectF {
            x: ox + b.rect.x,
            y: oy + b.rect.y,
            ..b.rect
        };
        let Some(style) = self.style(b.style_node) else {
            return;
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
            } else {
                self.decorations(c, b, style, r);
            }
            if style.display == Display::ListItem {
                self.marker(c, b, style, r);
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
        if let Some(il) = &b.inline {
            let (cx, cy) = b.content_offset();
            self.inline(c, il, r.x + cx, r.y + cy - sy);
        }
        // In-flow children first, then positioned ones on top by z-index.
        let mut positioned = Vec::new();
        for &child in &b.children {
            if self.tree.boxes[child].absolute {
                positioned.push(child);
            } else {
                self.paint_box(c, child, r.x, r.y - sy);
            }
        }
        positioned.sort_by_key(|&k| self.tree.boxes[k].z);
        for child in positioned {
            self.paint_box(c, child, r.x, r.y - sy);
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
                Control::Hidden => {}
            }
        }
        if let Some(node) = b
            .node
            .filter(|&n| matches!(self.dom.tag(n), Some("img" | "svg")))
            && style.visible
        {
            self.image(c, b, node, style, r);
        }
        if own && style.overflow.scrolls() {
            self.scrollbar(c, r, b.content_h, sy);
        }
        c.set_clip(saved);
        if own && style.visible && style.outline_style != cs::LineStyle::None {
            self.outline(c, style, r);
        }
        c.set_global_alpha(alpha);
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
        let Some(url) = &style.background_image else {
            return;
        };
        let layer = cs::Mask {
            image: None,
            size: style.background_size,
            repeat: style.background_repeat,
            position: style.background_position,
        };
        self.image_layer(c, b, r, url, &layer, None);
    }

    /// Paints image `url` sized, positioned and tiled per `layer` in the
    /// padding box of `b`. With `mask`, the image's alpha is used to paint
    /// that color instead (`mask-image`).
    fn image_layer(
        &self,
        c: &mut Canvas,
        b: &LBox,
        r: RectF,
        url: &str,
        layer: &cs::Mask,
        mask: Option<Rgba>,
    ) {
        let Some(bm) = self.images.urls.get(url).and_then(|s| s.bitmap()) else {
            return;
        };
        let pr = b.padding_rect();
        let area = RectF {
            x: r.x + pr.x,
            y: r.y + pr.y,
            ..pr
        };
        let (iw, ih) = (bm.width as f32, bm.height as f32);
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
        let Some(px) = self.images.scaled_url(url, pw, ph) else {
            return;
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
            Some(ImageState::Ready(..)) => {
                if p.w > 0
                    && p.h > 0
                    && let Some(px) = self.images.scaled(node, p.w as u32, p.h as u32)
                {
                    c.draw_argb(p.x, p.y, p.w as usize, p.h as usize, &px);
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
        let radius = style.border_radius;
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
                self.image_layer(c, b, r, url, &style.mask, Some(style.background));
            }
            return;
        }
        if style.background_image.is_some() && radius <= 0.0 {
            self.fill(c, r, style.background);
            self.background_image(c, b, style, r);
            return self.borders(c, style, r, bw);
        }
        if radius > 0.0 {
            let outer = (r.x * s, r.y * s, r.w * s, r.h * s, radius * s);
            if style.background.is_visible() {
                c.fill_rounded(outer, None, style.background.rgb(), style.background.a);
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
        } else {
            self.fill(c, r, style.background);
        }
        self.borders(c, style, r, bw);
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

    fn inline(&self, c: &mut Canvas, il: &crate::layout::inline::InlineLayout, x: f32, y: f32) {
        for line in &il.lines {
            let baseline = y + line.top + line.baseline;
            for item in &line.items {
                match item {
                    Placed::Text {
                        node,
                        x: tx,
                        width,
                        text,
                        shift,
                    } => {
                        let baseline = baseline + shift;
                        let Some(st) = self.style(*node) else {
                            continue;
                        };
                        if !st.visible {
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
