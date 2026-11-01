//! Painting a laid-out document onto a [`Canvas`].
//!
//! Boxes are painted in tree order (backgrounds, borders, then content);
//! absolutely positioned boxes are painted after their in-flow siblings.
//! Coordinates are logical; `scale` converts to physical pixels.

use std::collections::HashMap;

use crate::css::Style;
use crate::css::style::{self as cs, Display, ListStyle};
use crate::css::values::Rgba;
use crate::dom::{Dom, NodeId};
use crate::gfx::{Canvas, Rect};
use crate::layout::inline::Placed;
use crate::layout::{LBox, LayoutTree, RectF};
use crate::text::Fonts;

/// Vertical scroll offsets: `None` is the viewport, `Some(id)` an element.
pub type ScrollOffsets = HashMap<Option<NodeId>, f32>;

pub struct Painter<'a> {
    pub dom: &'a Dom,
    pub styles: &'a [Option<Style>],
    pub fonts: &'a Fonts,
    pub tree: &'a LayoutTree,
    pub scroll: &'a ScrollOffsets,
    pub scale: f32,
}

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
        for &child in &vp.children {
            self.paint_box(c, child, origin.0, origin.1 - sy);
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
        if own && style.visible {
            let propagated = self.canvas_background().1 == b.node;
            if propagated {
                let plain = Style {
                    background: Rgba::TRANSPARENT,
                    ..style.clone()
                };
                self.decorations(c, &plain, r);
            } else {
                self.decorations(c, style, r);
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
        // In-flow children first, then absolutely positioned ones on top.
        for pass in [false, true] {
            for &child in &b.children {
                if self.tree.boxes[child].absolute == pass {
                    self.paint_box(c, child, r.x, r.y - sy);
                }
            }
        }
        if own && style.overflow.scrolls() {
            self.scrollbar(c, r, b.content_h, sy);
        }
        c.set_clip(saved);
    }

    fn decorations(&self, c: &mut Canvas, style: &Style, r: RectF) {
        let s = self.scale;
        let bw = [0, 1, 2, 3].map(|i| style.border(i));
        let radius = style.border_radius;
        let uniform = bw.iter().all(|&w| w == bw[0])
            && style
                .border_color
                .iter()
                .all(|&col| col == style.border_color[0]);
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
                self.fill(c, side, style.border_color[i]);
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
                    } => {
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
