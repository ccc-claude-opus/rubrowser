//! Layout: turns a styled DOM into positioned boxes.
//!
//! Block and flex layout are done by `taffy`. Runs of inline content become
//! taffy leaves whose size comes from our own inline layout ([`inline`]).
//! `inline-block` elements are laid out first as separate taffy trees and
//! then placed in lines like big glyphs.

pub(crate) mod columns;
pub mod inline;

use std::collections::HashMap;

use crate::css::Generated;
use taffy::prelude::*;
use taffy::{
    AvailableSpace, LayoutInput, LayoutOutput, Overflow as TOverflow, Point, compute_leaf_layout,
};

use crate::css::Style as Css;
use crate::css::style::{self as cs, Align, Display as CDisplay};
use crate::css::values::Dim;
use crate::dom::{Dom, NodeId};
use crate::text::Fonts;
use inline::{Content, InlineLayout, Item};

/// A rectangle in logical pixels.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct RectF {
    pub x: f32,
    pub y: f32,
    pub w: f32,
    pub h: f32,
}

impl RectF {
    pub fn contains(&self, x: f32, y: f32) -> bool {
        x >= self.x && x < self.x + self.w && y >= self.y && y < self.y + self.h
    }
}

/// One laid-out box.
#[derive(Clone, Debug)]
pub struct LBox {
    /// The element, or `None` for anonymous boxes and the viewport.
    pub node: Option<NodeId>,
    /// Node whose style applies (the parent element for anonymous boxes).
    pub style_node: NodeId,
    /// Border box, relative to the parent box's border box (or, for
    /// atomic inlines, to their position in the line).
    pub rect: RectF,
    pub border: [f32; 4],
    pub padding: [f32; 4],
    pub children: Vec<usize>,
    pub inline: Option<InlineLayout>,
    /// Size of the content, for scrolling.
    pub content_w: f32,
    pub content_h: f32,
    pub absolute: bool,
    /// `position: fixed`: placed in the viewport and not scrolled.
    pub fixed: bool,
    /// Paint order among positioned siblings (`z-index`).
    pub z: i32,
    /// Downward shift of the content (table cells with `vertical-align`).
    pub content_shift: f32,
    /// `position: sticky` with a `top` offset (px).
    pub sticky_top: Option<f32>,
    /// A multi-column container's column count, width and gap.
    pub columns: Option<(u32, f32, f32)>,
    /// Vertical inline content (`writing-mode`): its lines are columns,
    /// right to left (`Some(true)`) or left to right.
    pub vertical: Option<bool>,
}

impl LBox {
    /// The `translate` offset of this box's own element (percentages of
    /// its border box).
    pub fn translation(&self, style: &Css) -> (f32, f32) {
        if self.node != Some(self.style_node) {
            return (0.0, 0.0);
        }
        // Other transforms are applied when painting (see `paint`).
        let m = style.transform_matrix(self.rect.w, self.rect.h);
        if m.is_translation() {
            (m.0[4], m.0[5])
        } else {
            (0.0, 0.0)
        }
    }

    /// How far a sticky box moves down so its top stays `sticky_top` below
    /// `view_top`, without leaving `parent_bottom`. `y` is where it was laid
    /// out (same coordinates as the other two).
    pub fn sticky_shift(&self, y: f32, view_top: f32, parent_bottom: f32) -> f32 {
        let Some(top) = self.sticky_top else {
            return 0.0;
        };
        let room = (parent_bottom - (y + self.rect.h)).max(0.0);
        (view_top + top - y).clamp(0.0, room)
    }

    /// Content box relative to this box's border box.
    pub fn content_offset(&self) -> (f32, f32) {
        (
            self.border[cs::LEFT] + self.padding[cs::LEFT],
            self.border[cs::TOP] + self.padding[cs::TOP] + self.content_shift,
        )
    }

    /// Padding box relative to this box's border box.
    pub fn padding_rect(&self) -> RectF {
        let b = self.border;
        RectF {
            x: b[cs::LEFT],
            y: b[cs::TOP],
            w: (self.rect.w - b[cs::LEFT] - b[cs::RIGHT]).max(0.0),
            h: (self.rect.h - b[cs::TOP] - b[cs::BOTTOM]).max(0.0),
        }
    }
}

/// All boxes of a document. Box 0 is the viewport.
#[derive(Clone, Debug, Default)]
pub struct LayoutTree {
    pub boxes: Vec<LBox>,
}

impl LayoutTree {
    pub const VIEWPORT: usize = 0;
}

/// What each taffy node stands for.
struct Ctx {
    node: Option<NodeId>,
    style_node: NodeId,
    inline: Option<Content>,
    /// Content size of a replaced element such as `<input>`.
    intrinsic: Option<(f32, f32)>,
    /// Whether that size has a fixed aspect ratio (images, not controls).
    keeps_ratio: bool,
}

impl Ctx {
    /// Whether the inline content runs vertically (see [`LBox::vertical`]).
    fn vertical(&self, styles: &[Option<Css>]) -> Option<bool> {
        self.inline.as_ref()?;
        styles.get(self.style_node)?.as_ref()?.vertical
    }
}

struct Builder<'a> {
    /// Whether the element being built sits in a block container (not a
    /// flex or grid item).
    in_block: bool,
    /// `position: fixed` elements, attached to the viewport.
    fixed: Vec<taffy::NodeId>,
    dom: &'a Dom,
    styles: &'a [Option<Css>],
    /// `::before` / `::after` boxes, with ids from `dom.len()` on.
    generated: &'a [Generated],
    /// Generated box index of each element's `::before` and `::after`.
    gen_of: HashMap<NodeId, [Option<usize>; 2]>,
    fonts: &'a Fonts,
    viewport: (f32, f32),
    boxes: Vec<LBox>,
    replaced: &'a Replaced,
    /// Widths that inline-blocks must shrink to (their container's width),
    /// learned from a previous pass.
    caps: HashMap<NodeId, f32>,
    /// Inline-blocks found wider than their container in this pass.
    overflow: HashMap<NodeId, f32>,
    /// Containing block sizes (the percentage bases of mixed `calc()`
    /// values) found by a previous pass; others assume the viewport.
    bases: HashMap<NodeId, (f32, f32)>,
    /// Elements with mixed `calc()` values, with the bases assumed.
    calc_nodes: Vec<(NodeId, (f32, f32))>,
    /// Multi-column containers' plans from the previous pass.
    plans: HashMap<NodeId, columns::Plan>,
    /// Multi-column containers in this pass, with the padding added to
    /// narrow their content to one column.
    multicol: Vec<(NodeId, f32)>,
    /// Positioned elements being built (innermost last), each with the
    /// absolutely positioned descendants hoisted to it as its children.
    abs_stack: Vec<(NodeId, Vec<taffy::NodeId>)>,
}

/// Containing block sizes (width, height) by element.
type Bases = HashMap<NodeId, (f32, f32)>;

/// Natural sizes of replaced elements (`<img>`), by node.
pub type Replaced = std::collections::HashMap<NodeId, (f32, f32)>;

/// Lays out the document in a viewport of `width × height` logical pixels.
pub fn layout(
    dom: &Dom,
    styles: &[Option<Css>],
    generated: &[Generated],
    fonts: &Fonts,
    width: f32,
    height: f32,
    replaced: &Replaced,
) -> LayoutTree {
    CALC_BASE.with(|b| b.set((width, height)));
    VIEWPORT.with(|v| v.set((width, height)));
    let mut hints = Hints::default();
    // Shrink-to-fit needs the container width, mixed `calc()` values their
    // containing block's size, and multi-column containers their width and
    // then their content's height, which are only known after layout:
    // re-run with what the previous pass found.
    for _ in 0..4 {
        let (tree, found) = layout_pass(
            dom,
            styles,
            generated,
            fonts,
            width,
            height,
            replaced,
            hints.clone(),
        );
        if found.is_empty() {
            return tree;
        }
        hints.caps.extend(found.caps);
        hints.bases.extend(found.bases);
        hints.plans.extend(found.plans);
    }
    layout_pass(
        dom, styles, generated, fonts, width, height, replaced, hints,
    )
    .0
}

/// What layout passes learn for the next one (see [`layout`]).
#[derive(Clone, Default)]
struct Hints {
    /// Widths inline-blocks shrink to.
    caps: HashMap<NodeId, f32>,
    /// Containing block sizes of elements with mixed `calc()` values.
    bases: Bases,
    /// Multi-column containers' widths and column heights.
    plans: HashMap<NodeId, columns::Plan>,
}

impl Hints {
    fn is_empty(&self) -> bool {
        self.caps.is_empty() && self.bases.is_empty() && self.plans.is_empty()
    }
}

#[allow(clippy::too_many_arguments)]
fn layout_pass(
    dom: &Dom,
    styles: &[Option<Css>],
    generated: &[Generated],
    fonts: &Fonts,
    width: f32,
    height: f32,
    replaced: &Replaced,
    hints: Hints,
) -> (LayoutTree, Hints) {
    let mut gen_of: HashMap<NodeId, [Option<usize>; 2]> = HashMap::new();
    for (i, g) in generated.iter().enumerate() {
        gen_of.entry(g.owner).or_default()[g.after as usize] = Some(i);
    }
    let mut b = Builder {
        generated,
        gen_of,
        caps: hints.caps,
        overflow: HashMap::new(),
        bases: hints.bases,
        calc_nodes: Vec::new(),
        plans: hints.plans,
        multicol: Vec::new(),
        abs_stack: Vec::new(),
        in_block: true,
        fixed: Vec::new(),
        replaced,
        dom,
        styles,
        fonts,
        viewport: (width, height),
        boxes: Vec::new(),
    };
    // Reserve index 0 for the viewport.
    b.boxes.push(LBox {
        node: None,
        style_node: Dom::ROOT,
        rect: RectF {
            x: 0.0,
            y: 0.0,
            w: width,
            h: height,
        },
        border: [0.0; 4],
        padding: [0.0; 4],
        children: Vec::new(),
        inline: None,
        content_w: width,
        content_h: height,
        absolute: false,
        fixed: false,
        z: 0,
        content_shift: 0.0,
        sticky_top: None,
        columns: None,
        vertical: None,
    });
    if let Some(html) = dom.html().filter(|&h| styles[h].is_some()) {
        let mut tree: TaffyTree<Ctx> = TaffyTree::new();
        // Fractional sizes: rounding would cut text boxes short and wrap.
        tree.disable_rounding();
        let root_style = Style {
            display: Display::Block,
            size: Size {
                width: length(width),
                height: length(height),
            },
            ..Default::default()
        };
        let html_node = b.build(&mut tree, html);
        let mut top = vec![html_node];
        top.extend(std::mem::take(&mut b.fixed));
        let root = tree.new_with_children(root_style, &top).unwrap();
        b.compute(&mut tree, root, width, AvailableSpace::Definite(height));
        let html_box = b.collect(&tree, html_node);
        // The page scrolls over everything that overflows <html> too
        // (sites often set `html { height: 100% }`).
        let hb = &b.boxes[html_box];
        let content_h = hb.rect.h.max(hb.content_h + hb.rect.y).max(height);
        let fixed: Vec<usize> = top[1..].iter().map(|&n| b.collect(&tree, n)).collect();
        let vp = &mut b.boxes[LayoutTree::VIEWPORT];
        vp.children.push(html_box);
        vp.children.extend(fixed);
        vp.content_h = content_h;
    }
    let found = Hints {
        caps: std::mem::take(&mut b.overflow),
        bases: b.wrong_bases(),
        plans: b.lay_columns(),
    };
    (LayoutTree { boxes: b.boxes }, found)
}

impl Builder<'_> {
    /// Elements whose mixed `calc()` values assumed a containing block
    /// size other than the one they got, with the real one.
    fn wrong_bases(&self) -> HashMap<NodeId, (f32, f32)> {
        let mut at: HashMap<NodeId, usize> = HashMap::new();
        for (i, b) in self.boxes.iter().enumerate() {
            if let Some(n) = b.node {
                at.entry(n).or_insert(i);
            }
        }
        let mut out = HashMap::new();
        for &(id, assumed) in &self.calc_nodes {
            let mut p = self.dom.parent(id);
            while let Some(n) = p {
                if let Some(&i) = at.get(&n) {
                    let b = &self.boxes[i];
                    let size = (
                        b.rect.w
                            - b.padding[cs::LEFT]
                            - b.padding[cs::RIGHT]
                            - b.border[cs::LEFT]
                            - b.border[cs::RIGHT],
                        b.rect.h
                            - b.padding[cs::TOP]
                            - b.padding[cs::BOTTOM]
                            - b.border[cs::TOP]
                            - b.border[cs::BOTTOM],
                    );
                    if (size.0 - assumed.0).abs() > 0.5 || (size.1 - assumed.1).abs() > 0.5 {
                        out.insert(id, size);
                    }
                    break;
                }
                p = self.dom.parent(n);
            }
        }
        out
    }

    /// Lays a multi-column container's content out as one column (extra
    /// right padding), as tall as its balanced columns once known.
    fn narrow_to_column(&mut self, id: NodeId, css: &Css, t: &mut Style) {
        let Some(plan) = self.plans.get(&id).copied() else {
            self.multicol.push((id, 0.0));
            return;
        };
        let (_, width) = columns::columns(css, plan.avail);
        let extra = (plan.avail - width).max(0.0);
        let right = css.padding[cs::RIGHT].px_or_zero(plan.avail);
        t.padding.right = LengthPercentage::length(right + extra);
        let content_box = css.box_sizing == cs::BoxSizing::ContentBox;
        if let (Dim::Px(w), true) = (css.width, content_box) {
            t.size.width = Dimension::length((w - extra).max(0.0));
        }
        if let Some(h) = plan.height {
            let around = if content_box {
                0.0
            } else {
                css.padding[cs::TOP].px_or_zero(plan.avail)
                    + css.padding[cs::BOTTOM].px_or_zero(plan.avail)
                    + css.border(cs::TOP)
                    + css.border(cs::BOTTOM)
            };
            t.size.height = Dimension::length(h + around);
        }
        self.multicol.push((id, extra));
    }

    /// After a pass: measures multi-column containers, and once their
    /// width and height are settled moves their content into columns.
    /// Returns the plans that changed (another pass is needed).
    fn lay_columns(&mut self) -> HashMap<NodeId, columns::Plan> {
        let mut changed = HashMap::new();
        for (id, extra) in std::mem::take(&mut self.multicol) {
            let Some(index) = self.boxes.iter().position(|b| b.node == Some(id)) else {
                continue;
            };
            let Some(css) = self.style(id).cloned() else {
                continue;
            };
            let b = &self.boxes[index];
            let avail = b.rect.w
                - b.padding[cs::LEFT]
                - b.padding[cs::RIGHT]
                - b.border[cs::LEFT]
                - b.border[cs::RIGHT]
                + extra;
            let (n, width) = columns::columns(&css, avail);
            let gap = columns::gap(&css, avail);
            let plan = self.plans.get(&id).copied();
            match plan {
                Some(p) if (p.avail - avail).abs() <= 0.5 => match p.height {
                    None => {
                        let height = columns::balance(&self.boxes, index, n);
                        changed.insert(
                            id,
                            columns::Plan {
                                avail,
                                height: Some(height),
                            },
                        );
                    }
                    Some(height) => {
                        columns::distribute(&mut self.boxes, index, n, width, gap, height);
                        self.boxes[index].columns = Some((n, width, gap));
                    }
                },
                _ => {
                    changed.insert(
                        id,
                        columns::Plan {
                            avail,
                            height: None,
                        },
                    );
                }
            }
        }
        changed
    }

    /// Whether element `id` has a percentage (or calc with %) width.
    fn percent_width(&self, id: NodeId) -> bool {
        self.style(id).is_some_and(|s| s.width.has_percent())
    }

    /// Children of `id` including generated `::before` / `::after` boxes.
    fn kids(&self, id: NodeId) -> Vec<NodeId> {
        let base = self.dom.len();
        if id >= base {
            // A generated box holds its text node.
            return if (id - base) % 2 == 0 {
                vec![id + 1]
            } else {
                Vec::new()
            };
        }
        let gens = self.gen_of.get(&id).copied().unwrap_or_default();
        let mut out = Vec::with_capacity(self.dom.children(id).len() + 2);
        out.extend(gens[0].map(|g| base + 2 * g));
        out.extend_from_slice(self.dom.children(id));
        out.extend(gens[1].map(|g| base + 2 * g));
        out
    }

    /// Text of a DOM text node or a generated text node.
    fn text_of(&self, id: NodeId) -> Option<&str> {
        let base = self.dom.len();
        if id >= base {
            let i = id - base;
            return (i % 2 == 1).then(|| self.generated[i / 2].text.as_str());
        }
        self.dom.text(id)
    }

    fn style(&self, id: NodeId) -> Option<&Css> {
        self.styles.get(id).and_then(Option::as_ref)
    }

    fn compute(&self, tree: &mut TaffyTree<Ctx>, root: taffy::NodeId, w: f32, h: AvailableSpace) {
        let styles = self.styles;
        let fonts = self.fonts;
        tree.compute_layout_with_measure(
            root,
            Size {
                width: AvailableSpace::Definite(w),
                height: h,
            },
            |input: LayoutInput, _id, ctx: Option<&mut Ctx>, style: &Style| -> LayoutOutput {
                compute_leaf_layout(
                    input,
                    style,
                    |_, _| 0.0,
                    |known, avail| {
                        if let Some((w, h)) = ctx.as_ref().and_then(|c| c.intrinsic) {
                            // Keep the natural aspect ratio when one side is set.
                            let keeps = ctx.as_ref().is_some_and(|c| c.keeps_ratio);
                            let ratio = (keeps && w > 0.0 && h > 0.0).then(|| h / w);
                            return match (known.width, known.height, ratio) {
                                (Some(kw), None, Some(r)) => Size {
                                    width: kw,
                                    height: kw * r,
                                },
                                (None, Some(kh), Some(r)) => Size {
                                    width: kh / r,
                                    height: kh,
                                },
                                _ => Size {
                                    width: known.width.unwrap_or(w),
                                    height: known.height.unwrap_or(h),
                                },
                            };
                        }
                        let vertical = ctx.as_ref().and_then(|c| c.vertical(styles));
                        let Some(content) = ctx.and_then(|c| c.inline.as_ref()) else {
                            return Size::ZERO;
                        };
                        if vertical.is_some() {
                            // Lines run down a column as long as the box
                            // is tall (if set); columns stack sideways.
                            let length = known.height.unwrap_or(f32::INFINITY);
                            let l = inline::layout(content, length, styles, fonts);
                            return Size {
                                width: known.width.unwrap_or(l.height),
                                height: known.height.unwrap_or(l.width),
                            };
                        }
                        let max_w = known.width.unwrap_or(match avail.width {
                            AvailableSpace::Definite(w) => w,
                            AvailableSpace::MinContent => 0.0,
                            AvailableSpace::MaxContent => f32::INFINITY,
                        });
                        let l = inline::layout(content, max_w, styles, fonts);
                        // Min-content probes with width 0; report the real
                        // widest line (an unbreakable word or image).
                        let fit = match avail.width {
                            AvailableSpace::Definite(w) => l.width.min(w),
                            _ => l.width,
                        };
                        Size {
                            width: known.width.unwrap_or(fit),
                            height: known.height.unwrap_or(l.height),
                        }
                    },
                )
            },
        )
        .expect("layout");
    }

    /// Creates the taffy node for element `id` and its subtree.
    fn build(&mut self, tree: &mut TaffyTree<Ctx>, id: NodeId) -> taffy::NodeId {
        let positioned = self.is_positioned(id);
        if positioned {
            self.abs_stack.push((id, Vec::new()));
        }
        let node = self.build_box(tree, id);
        if positioned && let Some((_, hoisted)) = self.abs_stack.pop() {
            for h in hoisted {
                tree.add_child(node, h).unwrap();
            }
        }
        node
    }

    /// Whether `id` is the containing block of absolutely positioned
    /// descendants.
    fn is_positioned(&self, id: NodeId) -> bool {
        self.style(id).is_some_and(|s| {
            s.position != cs::Position::Static
                || s.translate.0 != Dim::Px(0.0)
                || s.translate.1 != Dim::Px(0.0)
        })
    }

    /// Whether absolutely positioned `id` should be laid out in its
    /// containing block rather than its parent: when its place does not
    /// depend on where it would have been in the flow (insets on both axes,
    /// or a percentage width and height).
    fn hoistable(&self, id: NodeId) -> bool {
        let Some(s) = self.style(id) else {
            return false;
        };
        let set = |d: Dim| d != Dim::Auto;
        let pct = |d: Dim| d.has_percent();
        let horizontal = set(s.inset[cs::LEFT]) || set(s.inset[cs::RIGHT]);
        let vertical = set(s.inset[cs::TOP]) || set(s.inset[cs::BOTTOM]);
        (horizontal && vertical) || (pct(s.width) && pct(s.height))
    }

    fn build_box(&mut self, tree: &mut TaffyTree<Ctx>, id: NodeId) -> taffy::NodeId {
        // Read before building children, which reset it.
        let in_block = std::mem::replace(&mut self.in_block, true);
        let css = self.style(id).cloned().unwrap_or_default();
        let is_flex = matches!(css.display, CDisplay::Flex | CDisplay::InlineFlex);
        let is_grid = matches!(css.display, CDisplay::Grid | CDisplay::InlineGrid);
        let base = self.bases.get(&id).copied().unwrap_or(self.viewport);
        CALC_BASE.with(|b| b.set(base));
        if has_calc(&css) {
            self.calc_nodes.push((id, base));
        }
        let mut tstyle = convert(&css);
        if columns::is_multicol(&css) {
            self.narrow_to_column(id, &css, &mut tstyle);
        }
        // `<center>` / `align=center`: block children with no horizontal
        // margins are centered.
        let centered = in_block
            && !css.display.is_inline_level()
            && css.float == cs::Float::None
            && self
                .dom
                .parent(id)
                .and_then(|p| self.style(p))
                .is_some_and(|p| p.text_align == cs::TextAlign::WebkitCenter);
        if centered && css.margin[cs::LEFT] == Dim::Px(0.0) && css.margin[cs::RIGHT] == Dim::Px(0.0)
        {
            tstyle.margin.left = LengthPercentageAuto::auto();
            tstyle.margin.right = LengthPercentageAuto::auto();
        }
        tstyle.display = if is_flex {
            Display::Flex
        } else if is_grid {
            grid_style(&css.grid, &mut tstyle);
            Display::Grid
        } else {
            Display::Block
        };
        if css.display == CDisplay::Table {
            return self.build_table(tree, id, &css, tstyle);
        }

        if matches!(
            self.dom.tag(id),
            Some("input" | "textarea" | "select" | "iframe" | "progress" | "meter")
        ) {
            // Form controls (drawn by the browser) and frames (their own
            // document) are replaced elements.
            let size = self
                .replaced
                .get(&id)
                .copied()
                .unwrap_or((0.0, css.line_height_px()));
            let ctx = Ctx {
                node: Some(id),
                style_node: id,
                inline: None,
                intrinsic: Some(size),
                keeps_ratio: false,
            };
            return tree.new_leaf_with_context(tstyle, ctx).unwrap();
        }
        if matches!(self.dom.tag(id), Some("img" | "svg")) {
            let size = self.replaced.get(&id).copied().unwrap_or((0.0, 0.0));
            if size.0 > 0.0 && size.1 > 0.0 {
                tstyle.aspect_ratio = Some(size.0 / size.1);
            }
            let ctx = Ctx {
                node: Some(id),
                style_node: id,
                inline: None,
                intrinsic: Some(size),
                keeps_ratio: true,
            };
            return tree.new_leaf_with_context(tstyle, ctx).unwrap();
        }

        // Split children into block-level boxes and runs of inline content.
        let mut parts: Vec<Part> = Vec::new();
        self.collect_children(tree, id, is_flex || is_grid, &mut parts);

        let all_inline = parts.iter().all(|p| matches!(p, Part::Inline(_)));
        if !is_flex && !is_grid && all_inline {
            let items: Vec<Item> = parts
                .into_iter()
                .flat_map(|p| match p {
                    Part::Inline(items) => items,
                    Part::Block(_) => Vec::new(),
                })
                .collect();
            let content = Content { owner: id, items };
            if in_block && !content.is_blank(self.styles) {
                // Text flows beside floats: as a new block formatting
                // context, taffy places the box next to them. (Empty boxes
                // stay ordinary so their margins collapse through.)
                tstyle.overflow = avoid_floats(tstyle.overflow);
            }
            let items = content.items;
            let ctx = Ctx {
                node: Some(id),
                style_node: id,
                inline: Some(Content { owner: id, items }),
                intrinsic: None,
                keeps_ratio: false,
            };
            return tree.new_leaf_with_context(tstyle, ctx).unwrap();
        }
        let mut kids = Vec::new();
        for p in parts {
            match p {
                Part::Block(c) => kids.push(c),
                Part::Inline(items) => {
                    let content = Content { owner: id, items };
                    if content.is_blank(self.styles) {
                        continue;
                    }
                    let anon = Style {
                        display: Display::Block,
                        overflow: if is_flex || is_grid {
                            Point {
                                x: TOverflow::Visible,
                                y: TOverflow::Visible,
                            }
                        } else {
                            avoid_floats(Point {
                                x: TOverflow::Visible,
                                y: TOverflow::Visible,
                            })
                        },
                        ..Default::default()
                    };
                    let ctx = Ctx {
                        node: None,
                        style_node: id,
                        inline: Some(content),
                        intrinsic: None,
                        keeps_ratio: false,
                    };
                    kids.push(tree.new_leaf_with_context(anon, ctx).unwrap());
                }
            }
        }
        let node = tree.new_with_children(tstyle, &kids).unwrap();
        tree.set_node_context(
            node,
            Some(Ctx {
                node: Some(id),
                style_node: id,
                inline: None,
                intrinsic: None,
                keeps_ratio: false,
            }),
        )
        .unwrap();
        node
    }

    /// Classifies the children of `parent` (flattening inline elements).
    fn collect_children(
        &mut self,
        tree: &mut TaffyTree<Ctx>,
        parent: NodeId,
        flex: bool,
        parts: &mut Vec<Part>,
    ) {
        for c in self.kids(parent) {
            let Some(css) = self.style(c) else { continue };
            if css.display == CDisplay::Contents {
                // No box of its own: its children take its place.
                self.collect_children(tree, c, flex, parts);
                continue;
            }
            if let Some(text) = self.text_of(c) {
                push_inline(
                    parts,
                    Item::Text {
                        node: c,
                        text: text.to_string(),
                    },
                );
                continue;
            }
            if self.dom.element(c).is_none() && c < self.dom.len() {
                continue;
            }
            let block_level = flex
                || css.position == cs::Position::Absolute
                || css.float != cs::Float::None
                || !css.display.is_inline_level();
            if css.position == cs::Position::Fixed {
                // Fixed boxes are positioned against the viewport.
                let node = self.build(tree, c);
                self.fixed.push(node);
                continue;
            }
            if self.dom.tag(c) == Some("br") && !flex {
                push_inline(parts, Item::Break { node: c });
            } else if block_level {
                let absolute = css.position == cs::Position::Absolute;
                self.in_block = !flex;
                let child = self.build(tree, c);
                // An absolutely positioned box whose containing block is an
                // outer positioned element goes there.
                if absolute && self.hoistable(c) {
                    let mut up = self.dom.parent(c);
                    while let Some(a) = up {
                        if self.is_positioned(a) {
                            break;
                        }
                        up = self.dom.parent(a);
                    }
                    let target = up.and_then(|a| self.abs_stack.iter().position(|(n, _)| *n == a));
                    if let Some(t) = target.filter(|_| !self.is_positioned(parent)) {
                        self.abs_stack[t].1.push(child);
                        continue;
                    }
                }
                parts.push(Part::Block(child));
            } else if css.display == CDisplay::Inline {
                // Margin + border + padding at the start and end.
                let side = |i: usize| {
                    css.margin[i].px_or_zero(0.0).max(0.0)
                        + css.border(i)
                        + css.padding[i].px_or_zero(0.0)
                };
                let (lead, trail) = (side(cs::LEFT), side(cs::RIGHT));
                if lead > 0.0 {
                    push_inline(parts, Item::Gap { width: lead });
                }
                self.collect_children(tree, c, false, parts);
                if trail > 0.0 {
                    push_inline(parts, Item::Gap { width: trail });
                }
            } else {
                // inline-block / inline-flex: an atomic inline box.
                let wraps = self.style(parent).is_none_or(|p| p.white_space.wraps());
                let (index, width, height, baseline) = self.atomic(c);
                push_inline(
                    parts,
                    Item::Atomic {
                        index,
                        width,
                        height,
                        baseline,
                        wraps,
                    },
                );
            }
        }
    }

    /// Lays out an inline-block element on its own and returns its box
    /// index and margin-box size.
    fn atomic(&mut self, id: NodeId) -> (usize, f32, f32, f32) {
        let mut tree: TaffyTree<Ctx> = TaffyTree::new();
        // Fractional sizes: rounding would cut text boxes short and wrap.
        tree.disable_rounding();
        // Boxes in this separate tree cannot be hoisted out of it.
        let outer = std::mem::take(&mut self.abs_stack);
        let node = self.build(&mut tree, id);
        self.abs_stack = outer;
        // A percentage width needs the container's width, known only in
        // the next pass; until then the box takes its intrinsic width.
        if self.percent_width(id) && !self.caps.contains_key(&id) {
            let mut st = tree.style(node).unwrap().clone();
            st.size.width = Dimension::auto();
            tree.set_style(node, st).unwrap();
        }
        // Shrink-to-fit: lay out at max-content, capped by the container
        // width (once known) or the viewport.
        let avail = self.caps.get(&id).copied().unwrap_or(self.viewport.0);
        let wrapper = tree
            .new_with_children(
                Style {
                    display: Display::Flex,
                    align_items: AlignItems::START,
                    size: Size {
                        width: length(avail),
                        height: Dimension::auto(),
                    },
                    ..Default::default()
                },
                &[node],
            )
            .unwrap();
        self.compute(&mut tree, wrapper, avail, AvailableSpace::MaxContent);
        let index = self.collect(&tree, node);
        let l = tree.layout(node).unwrap();
        let css = self.style(id).cloned().unwrap_or_default();
        let m = |d: Dim| d.px_or_zero(self.viewport.0);
        let (ml, mr, mt, mb) = (
            m(css.margin[cs::LEFT]),
            m(css.margin[cs::RIGHT]),
            m(css.margin[cs::TOP]),
            m(css.margin[cs::BOTTOM]),
        );
        let b = &mut self.boxes[index];
        b.rect.x = ml;
        b.rect.y = mt;
        let height = l.size.height + mt + mb;
        // CSS: an inline-block's baseline is that of its last line box (or
        // its bottom margin edge if it has none).
        let baseline = self.last_baseline(index).map_or(height, |bl| mt + bl);
        (index, l.size.width + ml + mr, height, baseline)
    }

    /// Distance from the top of box `i` to its last line's baseline.
    fn last_baseline(&self, i: usize) -> Option<f32> {
        let b = &self.boxes[i];
        let (_, cy) = b.content_offset();
        if let Some(line) = b.inline.as_ref().and_then(|il| il.lines.last()) {
            return Some(cy + line.top + line.baseline);
        }
        if let Some(n) = b.node.filter(|&n| self.dom.tag(n) == Some("input")) {
            // Single-line text centered in the content box, as painted.
            let st = self.style(n)?;
            let m = self.fonts.line_metrics(st.font(), st.font_size);
            let content_h = b.rect.h - cy - b.border[cs::BOTTOM] - b.padding[cs::BOTTOM];
            return Some(cy + (content_h - (m.ascent + m.descent)) / 2.0 + m.ascent);
        }
        b.children
            .iter()
            .rev()
            .filter(|&&c| !self.boxes[c].absolute)
            .find_map(|&c| Some(self.boxes[c].rect.y + self.last_baseline(c)?))
    }

    /// Copies taffy results into [`LBox`]es, returning the box index of `node`.
    fn collect(&mut self, tree: &TaffyTree<Ctx>, node: taffy::NodeId) -> usize {
        let l = tree.layout(node).unwrap();
        let ctx = tree.get_node_context(node);
        let (dom_node, style_node) = ctx.map_or((None, Dom::ROOT), |c| (c.node, c.style_node));
        let side = |r: taffy::Rect<f32>| [r.top, r.right, r.bottom, r.left];
        let border = side(l.border);
        let padding = side(l.padding);
        let vertical = ctx.and_then(|c| c.vertical(self.styles));
        let inline = ctx.and_then(|c| c.inline.as_ref()).map(|content| {
            let w = if vertical.is_some() {
                l.size.height - border[0] - border[2] - padding[0] - padding[2]
            } else {
                l.size.width - border[1] - border[3] - padding[1] - padding[3]
            };
            for item in &content.items {
                if let inline::Item::Atomic { index, width, .. } = *item {
                    let node = self.boxes[index].node;
                    let Some(n) = node.filter(|_| w > 0.0) else {
                        continue;
                    };
                    let wants = width > w + 0.5 || self.percent_width(n);
                    if wants && !self.caps.contains_key(&n) {
                        self.overflow.insert(n, w);
                    }
                }
            }
            let mut il = inline::layout(content, w.max(0.0), self.styles, self.fonts);
            if let Some(owner) = self.style(content.owner) {
                inline::align(&mut il, owner.text_align, w, owner.rtl);
            }
            il
        });
        let positioned = dom_node
            .and_then(|n| self.style(n))
            .map(|s| (s.position, s.z_index));
        let absolute = positioned
            .is_some_and(|(p, _)| matches!(p, cs::Position::Absolute | cs::Position::Fixed));
        let fixed = positioned.is_some_and(|(p, _)| p == cs::Position::Fixed);
        let z = positioned.and_then(|(_, z)| z).unwrap_or(0);
        let sticky_top = dom_node
            .and_then(|n| self.style(n))
            .filter(|s| s.position == cs::Position::Sticky)
            .and_then(|s| match s.inset[cs::TOP] {
                Dim::Px(v) => Some(v),
                _ => None,
            });
        let overflow = l.scrollable_overflow_rect;
        let index = self.boxes.len();
        self.boxes.push(LBox {
            node: dom_node,
            style_node,
            rect: RectF {
                x: l.location.x,
                y: l.location.y,
                w: l.size.width,
                h: l.size.height,
            },
            border,
            padding,
            children: Vec::new(),
            content_w: overflow.right.max(l.size.width),
            content_h: overflow
                .bottom
                .max(
                    inline
                        .as_ref()
                        .map_or(0.0, |i| i.height + padding[0] + border[0]),
                )
                .max(l.size.height),
            inline,
            absolute,
            fixed,
            z,
            content_shift: 0.0,
            sticky_top,
            columns: None,
            vertical,
        });
        let kids: Vec<usize> = tree
            .children(node)
            .unwrap()
            .into_iter()
            .map(|c| self.collect(tree, c))
            .collect();
        self.boxes[index].children = kids;
        if let Some(n) = dom_node {
            self.align_cell(index, n);
        }
        index
    }

    /// Shifts a table cell's content for `vertical-align: middle/bottom`.
    fn align_cell(&mut self, index: usize, node: NodeId) {
        let Some(css) = self.style(node) else { return };
        if css.display != CDisplay::TableCell {
            return;
        }
        let factor = match css.vertical_align {
            cs::VerticalAlign::Middle => 0.5,
            cs::VerticalAlign::Bottom => 1.0,
            _ => return,
        };
        let b = &self.boxes[index];
        let (_, top) = b.content_offset();
        let inner = b.rect.h - top - b.border[cs::BOTTOM] - b.padding[cs::BOTTOM];
        let used = match &b.inline {
            Some(il) => il.height,
            None => b
                .children
                .iter()
                .map(|&c| self.boxes[c].rect.y + self.boxes[c].rect.h - top)
                .fold(0.0, f32::max),
        };
        let shift = ((inner - used) * factor).max(0.0);
        if shift > 0.0 {
            let kids = self.boxes[index].children.clone();
            for c in kids {
                self.boxes[c].rect.y += shift;
            }
            self.boxes[index].content_shift = shift;
        }
    }

    /// Column tracks: a column with a fixed-width cell keeps that width;
    /// the others are `auto` (sharing any extra room).
    fn table_columns(cols: &[Option<f32>]) -> Vec<GridTemplateComponent<String>> {
        cols.iter()
            .map(|&fixed| GridTemplateComponent::Single(fixed.map_or_else(auto, length)))
            .collect()
    }

    /// Lays out a `display: table` element as a CSS grid: one grid row per
    /// table row, cells placed with their `colspan` / `rowspan`.
    fn build_table(
        &mut self,
        tree: &mut TaffyTree<Ctx>,
        id: NodeId,
        css: &Css,
        mut tstyle: Style,
    ) -> taffy::NodeId {
        // Rows in display order: header groups, bodies, footer groups.
        let mut captions = Vec::new();
        let (mut head, mut body, mut foot) = (Vec::new(), Vec::new(), Vec::new());
        for &c in self.dom.children(id) {
            let Some(cs) = self.style(c) else { continue };
            match cs.display {
                CDisplay::TableCaption => captions.push(c),
                CDisplay::TableRow => body.push(c),
                CDisplay::TableRowGroup => {
                    let rows: Vec<NodeId> = self
                        .dom
                        .children(c)
                        .iter()
                        .copied()
                        .filter(|&r| {
                            self.style(r)
                                .is_some_and(|s| s.display == CDisplay::TableRow)
                        })
                        .collect();
                    match self.dom.tag(c) {
                        Some("thead") => head.extend(rows),
                        Some("tfoot") => foot.extend(rows),
                        _ => body.extend(rows),
                    }
                }
                _ => {}
            }
        }
        let rows: Vec<NodeId> = head.into_iter().chain(body).chain(foot).collect();

        // Place cells, skipping slots taken by rowspans from above.
        let mut taken: Vec<Vec<bool>> = Vec::new();
        let mut cells = Vec::new(); // (cell, row, col, rowspan, colspan)
        let span = |n: NodeId, attr: &str| {
            self.dom
                .attr(n, attr)
                .and_then(|v| v.trim().parse::<usize>().ok())
                .unwrap_or(1)
                .clamp(1, 1000)
        };
        for (r, &row) in rows.iter().enumerate() {
            let mut col = 0;
            for &cell in self.dom.children(row) {
                if !self
                    .style(cell)
                    .is_some_and(|s| s.display == CDisplay::TableCell)
                {
                    continue;
                }
                let (rs, cs) = (span(cell, "rowspan"), span(cell, "colspan"));
                while taken
                    .get(r)
                    .and_then(|t| t.get(col))
                    .copied()
                    .unwrap_or(false)
                {
                    col += 1;
                }
                for rr in r..r + rs {
                    if taken.len() <= rr {
                        taken.resize(rr + 1, Vec::new());
                    }
                    if taken[rr].len() < col + cs {
                        taken[rr].resize(col + cs, false);
                    }
                    for slot in &mut taken[rr][col..col + cs] {
                        *slot = true;
                    }
                }
                cells.push((cell, r, col, rs, cs));
                col += cs;
            }
        }
        let ncols = taken.iter().map(Vec::len).max().unwrap_or(0).max(1);
        let first = captions.len();
        let line = |i: usize| taffy::style_helpers::line::<GridPlacement>(i as i16 + 1);
        let spanning = |n: usize| taffy::style_helpers::span::<GridPlacement>(n as u16);
        let whole_row = |r: usize| {
            (
                taffy::Line {
                    start: line(r),
                    end: spanning(1),
                },
                taffy::Line {
                    start: line(0),
                    end: spanning(ncols),
                },
            )
        };

        let mut kids = Vec::new();
        for (i, &cap) in captions.iter().enumerate() {
            let node = self.build(tree, cap);
            let mut st = tree.style(node).unwrap().clone();
            (st.grid_row, st.grid_column) = whole_row(i);
            tree.set_style(node, st).unwrap();
            kids.push(node);
        }
        // Row backgrounds: an empty item behind the row's cells.
        for (r, &row) in rows.iter().enumerate() {
            if self.style(row).is_some_and(|s| s.background.is_visible()) {
                let (grid_row, grid_column) = whole_row(first + r);
                let st = Style {
                    grid_row,
                    grid_column,
                    ..Default::default()
                };
                let ctx = Ctx {
                    node: Some(row),
                    style_node: row,
                    inline: None,
                    intrinsic: None,
                    keeps_ratio: false,
                };
                kids.push(tree.new_leaf_with_context(st, ctx).unwrap());
            }
        }
        // Per column: a fixed border-box width from a cell's `width`.
        let mut col_widths: Vec<Option<f32>> = vec![None; ncols];
        for &(cell, _, col, _, span) in &cells {
            let Some(st) = self.style(cell) else { continue };
            if let (1, Dim::Px(w)) = (span, st.width) {
                let pad = st.padding[cs::LEFT].px_or_zero(0.0)
                    + st.padding[cs::RIGHT].px_or_zero(0.0)
                    + st.border(cs::LEFT)
                    + st.border(cs::RIGHT);
                let w = match st.box_sizing {
                    cs::BoxSizing::BorderBox => w,
                    cs::BoxSizing::ContentBox => w + pad,
                };
                col_widths[col] = Some(col_widths[col].map_or(w, |o: f32| o.max(w)));
            }
        }
        let collapse = css.border_collapse;
        for (cell, r, col, rs, cs) in cells {
            let node = self.build(tree, cell);
            let mut st = tree.style(node).unwrap().clone();
            st.grid_row = taffy::Line {
                start: line(first + r),
                end: spanning(rs),
            };
            st.grid_column = taffy::Line {
                start: line(col),
                end: spanning(cs),
            };
            if collapse {
                // Adjacent cells share one border: overlap by its width.
                if r > 0 {
                    st.margin.top = LengthPercentageAuto::length(-border_px(&st.border.top));
                }
                if col > 0 {
                    st.margin.left = LengthPercentageAuto::length(-border_px(&st.border.left));
                }
            }
            tree.set_style(node, st).unwrap();
            kids.push(node);
        }

        let gap = if collapse { 0.0 } else { css.border_spacing };
        tstyle.display = Display::Grid;
        tstyle.item_is_table = true;
        tstyle.grid_template_columns = Self::table_columns(&col_widths);
        tstyle.gap = Size {
            width: LengthPercentage::length(gap),
            height: LengthPercentage::length(gap),
        };
        tstyle.padding = taffy::Rect {
            left: LengthPercentage::length(gap),
            right: LengthPercentage::length(gap),
            top: LengthPercentage::length(gap),
            bottom: LengthPercentage::length(gap),
        };
        let node = tree.new_with_children(tstyle, &kids).unwrap();
        tree.set_node_context(
            node,
            Some(Ctx {
                node: Some(id),
                style_node: id,
                inline: None,
                intrinsic: None,
                keeps_ratio: false,
            }),
        )
        .unwrap();
        node
    }
}

/// A grid line placement (`2`, `-1`, `span 2`, `main`, `auto`).
fn placement(v: Option<&str>) -> GridPlacement {
    v.and_then(|v| v.trim().parse::<GridPlacement>().ok())
        .unwrap_or(GridPlacement::Auto)
}

/// Splits a value on whitespace at the top level (not inside parentheses,
/// brackets or quotes).
fn top_tokens(s: &str) -> Vec<&str> {
    let (mut out, mut depth, mut start, mut quote) = (Vec::new(), 0i32, None, None);
    for (i, c) in s.char_indices() {
        match (quote, c) {
            (Some(q), c) if c == q => quote = None,
            (Some(_), _) => continue,
            (None, '"' | '\'') => quote = Some(c),
            (None, '(' | '[') => depth += 1,
            (None, ')' | ']') => depth -= 1,
            _ => {}
        }
        if c.is_whitespace() && depth == 0 && quote.is_none() {
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

/// One track size, parsed by taffy or, for lengths taffy cannot read
/// (calc() and friends), by our own value parser.
fn track(tok: &str) -> Option<TrackSizingFunction> {
    if let Ok(t) = zero_px(tok).parse::<TrackSizingFunction>() {
        return Some(t);
    }
    let fc = crate::css::values::FontContext {
        em: 16.0,
        rem: 16.0,
        vw: VIEWPORT.with(|v| v.get().0),
        vh: VIEWPORT.with(|v| v.get().1),
    };
    let lp = lp(crate::css::values::dim(tok, fc)?);
    Some(minmax(lp.into(), lp.into()))
}

/// Layout-only `overflow: hidden` so an inline-content box establishes a
/// block formatting context (painting still uses the CSS `overflow`).
fn avoid_floats(o: Point<TOverflow>) -> Point<TOverflow> {
    let f = |v: TOverflow| {
        if v == TOverflow::Visible {
            TOverflow::Hidden
        } else {
            v
        }
    };
    Point {
        x: f(o.x),
        y: f(o.y),
    }
}

/// Rewrites lengths for taffy's parser, which only knows `px`, `%` and
/// `fr`: unitless zeros become `0px`, `rem`/`em`/`vw`/`vh`/`pt` become px.
fn zero_px(v: &str) -> String {
    let base = VIEWPORT.with(|v| v.get().0);
    let mut out = String::with_capacity(v.len() + 8);
    let mut rest = v;
    while !rest.is_empty() {
        let starts_number = rest.as_bytes()[0].is_ascii_digit()
            || (rest.as_bytes()[0] == b'.'
                && rest.as_bytes().get(1).is_some_and(u8::is_ascii_digit));
        let prev = out.as_bytes().last().copied();
        if starts_number && prev.is_none_or(|p| matches!(p, b'(' | b',' | b' ' | b'-')) {
            let num_end = rest
                .find(|c: char| !(c.is_ascii_digit() || c == '.'))
                .unwrap_or(rest.len());
            let unit_end = rest[num_end..]
                .find(|c: char| !c.is_ascii_alphabetic() && c != '%')
                .map_or(rest.len(), |e| num_end + e);
            let n: f32 = rest[..num_end].parse().unwrap_or(0.0);
            let unit = &rest[num_end..unit_end];
            let px = match unit {
                "" if n == 0.0 => Some(0.0),
                "rem" | "em" => Some(n * 16.0),
                "vw" => Some(n * base / 100.0),
                "vh" => Some(n * 8.0),
                "pt" => Some(n * 4.0 / 3.0),
                _ => None,
            };
            match px {
                Some(px) => out.push_str(&format!("{px}px")),
                None => out.push_str(&rest[..unit_end]),
            }
            rest = &rest[unit_end..];
            continue;
        }
        let ch = rest.chars().next().unwrap_or(' ');
        out.push(ch);
        rest = &rest[ch.len_utf8()..];
    }
    out
}

/// A `grid-template-columns` / `-rows` track list (line names ignored).
/// A track list and its line names (`[a b] 1fr [c]`: one list of names per
/// line, so one more than there are tracks).
type TrackList = (Vec<GridTemplateComponent<String>>, Vec<Vec<String>>);

fn track_list(v: &str) -> TrackList {
    if v.trim() == "none" {
        return (Vec::new(), Vec::new());
    }
    let v = zero_px(v);
    let mut tracks = Vec::new();
    let mut names: Vec<Vec<String>> = vec![Vec::new()];
    for t in top_tokens(&v) {
        if let Some(list) = t.strip_prefix('[') {
            let list = list.trim_end_matches(']');
            names
                .last_mut()
                .unwrap()
                .extend(list.split_whitespace().map(str::to_string));
            continue;
        }
        let component = if t.starts_with("repeat(") {
            t.parse::<GridTemplateComponent<String>>().ok()
        } else {
            track(t).map(GridTemplateComponent::Single)
        };
        if let Some(c) = component {
            tracks.push(c);
            names.push(Vec::new());
        }
    }
    (tracks, names)
}

/// `grid-template-areas: "a a" "b c"`.
fn template_areas(v: &str) -> Option<taffy::GridTemplateAreas<String>> {
    let rows: Vec<Vec<&str>> = top_tokens(v)
        .into_iter()
        .filter(|t| t.starts_with('"') || t.starts_with('\''))
        .map(|t| t.trim_matches(['"', '\'']).split_whitespace().collect())
        .collect();
    let columns = rows.iter().map(Vec::len).max()?;
    let mut areas: Vec<taffy::GridTemplateArea<String>> = Vec::new();
    for (r, row) in rows.iter().enumerate() {
        for (c, name) in row.iter().enumerate() {
            if name.chars().all(|ch| ch == '.') {
                continue;
            }
            let (r, c) = (r as u16 + 1, c as u16 + 1);
            match areas.iter_mut().find(|a| a.name == *name) {
                Some(a) => {
                    a.row_start = a.row_start.min(r);
                    a.row_end = a.row_end.max(r + 1);
                    a.column_start = a.column_start.min(c);
                    a.column_end = a.column_end.max(c + 1);
                }
                None => areas.push(taffy::GridTemplateArea {
                    name: name.to_string(),
                    row_start: r,
                    row_end: r + 1,
                    column_start: c,
                    column_end: c + 1,
                }),
            }
        }
    }
    Some(taffy::GridTemplateAreas {
        areas,
        row_count: rows.len() as u16,
        column_count: columns as u16,
    })
}

/// Grid container properties.
fn grid_style(g: &crate::css::style::GridProps, ts: &mut Style) {
    if let Some(v) = &g.template_columns {
        (ts.grid_template_columns, ts.grid_template_column_names) = track_list(v);
    }
    if let Some(v) = &g.template_rows {
        (ts.grid_template_rows, ts.grid_template_row_names) = track_list(v);
    }
    if let Some(v) = &g.template_areas {
        ts.grid_template_areas = template_areas(v);
    }
    let autos = |v: &str| {
        top_tokens(&zero_px(v))
            .into_iter()
            .filter_map(track)
            .collect::<Vec<_>>()
    };
    if let Some(v) = &g.auto_columns {
        ts.grid_auto_columns = autos(v);
    }
    if let Some(v) = &g.auto_rows {
        ts.grid_auto_rows = autos(v);
    }
    if let Some(flow) = g.auto_flow.as_deref().and_then(|v| v.parse().ok()) {
        ts.grid_auto_flow = flow;
    }
}

/// Width of a border given in pixels (taffy stores it as a length).
fn border_px(l: &LengthPercentage) -> f32 {
    l.into_raw().value()
}

enum Part {
    Block(taffy::NodeId),
    Inline(Vec<Item>),
}

fn push_inline(parts: &mut Vec<Part>, item: Item) {
    if let Some(Part::Inline(items)) = parts.last_mut() {
        items.push(item);
    } else {
        parts.push(Part::Inline(vec![item]));
    }
}

thread_local! {
    /// The containing block size (width, height) of the element whose
    /// style is being converted: taffy has no `calc()`, so mixed values
    /// become pixels against it (see [`Builder::wrong_bases`]).
    static CALC_BASE: std::cell::Cell<(f32, f32)> = const { std::cell::Cell::new((1280.0, 800.0)) };
    /// The viewport size, for `vw` / `vh` in grid templates.
    static VIEWPORT: std::cell::Cell<(f32, f32)> = const { std::cell::Cell::new((1280.0, 800.0)) };
}

/// Pixels of a mixed `calc()` value: percentages of the containing
/// block's height for `vertical` properties, else of its width.
fn calc_px(d: Dim, vertical: bool) -> f32 {
    let (w, h) = CALC_BASE.with(|b| b.get());
    d.px_or_zero(if vertical { h } else { w })
}

/// Whether a style has mixed `calc()` values (see [`calc_px`]).
fn has_calc(s: &Css) -> bool {
    let mixed = |d: &Dim| matches!(d, Dim::Calc(..) | Dim::Expr(_));
    [
        s.width,
        s.height,
        s.min_width,
        s.min_height,
        s.max_width,
        s.max_height,
    ]
    .iter()
    .chain(&s.margin)
    .chain(&s.padding)
    .chain(&s.inset)
    .chain([&s.row_gap, &s.column_gap, &s.flex_basis])
    .any(mixed)
}

fn dim_on(d: Dim, vertical: bool) -> Dimension {
    match d {
        Dim::Auto => Dimension::auto(),
        Dim::Px(v) => Dimension::length(v),
        Dim::Percent(p) => Dimension::percent(p / 100.0),
        Dim::Calc(..) | Dim::Expr(_) => Dimension::length(calc_px(d, vertical)),
    }
}

fn dim(d: Dim) -> Dimension {
    dim_on(d, false)
}

fn lpa_on(d: Dim, vertical: bool) -> LengthPercentageAuto {
    match d {
        Dim::Auto => LengthPercentageAuto::auto(),
        Dim::Px(v) => LengthPercentageAuto::length(v),
        Dim::Percent(p) => LengthPercentageAuto::percent(p / 100.0),
        Dim::Calc(..) | Dim::Expr(_) => LengthPercentageAuto::length(calc_px(d, vertical)),
    }
}

fn lpa(d: Dim) -> LengthPercentageAuto {
    lpa_on(d, false)
}

fn lp(d: Dim) -> LengthPercentage {
    match d {
        Dim::Auto => LengthPercentage::length(0.0),
        Dim::Px(v) => LengthPercentage::length(v),
        Dim::Percent(p) => LengthPercentage::percent(p / 100.0),
        Dim::Calc(..) | Dim::Expr(_) => LengthPercentage::length(calc_px(d, false)),
    }
}

fn rect4<T>(v: [T; 4]) -> taffy::Rect<T> {
    let [top, right, bottom, left] = v;
    taffy::Rect {
        left,
        right,
        top,
        bottom,
    }
}

fn align_items(a: Align) -> AlignItems {
    match a {
        Align::Normal => AlignItems::NORMAL,
        Align::Start | Align::SpaceBetween | Align::SpaceAround | Align::SpaceEvenly => {
            AlignItems::FLEX_START
        }
        Align::End => AlignItems::FLEX_END,
        Align::Center => AlignItems::CENTER,
        Align::Stretch => AlignItems::STRETCH,
        Align::Baseline => AlignItems::BASELINE,
    }
}

fn justify(a: Align) -> JustifyContent {
    match a {
        Align::Normal => JustifyContent::NORMAL,
        Align::Start | Align::Baseline => JustifyContent::FLEX_START,
        Align::End => JustifyContent::FLEX_END,
        Align::Center => JustifyContent::CENTER,
        Align::Stretch => JustifyContent::STRETCH,
        Align::SpaceBetween => JustifyContent::SPACE_BETWEEN,
        Align::SpaceAround => JustifyContent::SPACE_AROUND,
        Align::SpaceEvenly => JustifyContent::SPACE_EVENLY,
    }
}

/// Converts our computed style to taffy's (display is set by the caller).
fn convert(s: &Css) -> Style {
    let overflow = match s.overflow {
        cs::Overflow::Visible => TOverflow::Visible,
        cs::Overflow::Hidden => TOverflow::Hidden,
        cs::Overflow::Auto | cs::Overflow::Scroll => TOverflow::Scroll,
    };
    let border = std::array::from_fn(|i| LengthPercentage::length(s.border(i)));
    Style {
        aspect_ratio: s.aspect_ratio,
        box_sizing: match s.box_sizing {
            cs::BoxSizing::BorderBox => BoxSizing::BorderBox,
            cs::BoxSizing::ContentBox => BoxSizing::ContentBox,
        },
        position: match s.position {
            cs::Position::Absolute | cs::Position::Fixed => Position::Absolute,
            _ => Position::Relative,
        },
        float: match s.float {
            cs::Float::Left => taffy::Float::Left,
            cs::Float::Right => taffy::Float::Right,
            cs::Float::None => taffy::Float::None,
        },
        clear: match s.clear {
            (true, true) => taffy::Clear::Both,
            (true, false) => taffy::Clear::Left,
            (false, true) => taffy::Clear::Right,
            _ => taffy::Clear::None,
        },
        // Sticky offsets apply at paint time, not in layout.
        inset: if s.position == cs::Position::Sticky {
            rect4([Dim::Auto; 4].map(lpa))
        } else {
            let [t, r, b, l] = s.inset;
            rect4([lpa_on(t, true), lpa(r), lpa_on(b, true), lpa(l)])
        },
        overflow: Point {
            x: overflow,
            y: overflow,
        },
        scrollbar_width: 0.0,
        size: Size {
            width: dim(s.width),
            height: dim_on(s.height, true),
        },
        min_size: Size {
            width: lpa(s.min_width),
            height: lpa_on(s.min_height, true),
        },
        max_size: Size {
            width: lpa(s.max_width),
            height: lpa_on(s.max_height, true),
        },
        margin: rect4(s.margin.map(lpa)),
        padding: rect4(s.padding.map(lp)),
        border: rect4(border),
        flex_direction: match s.flex_direction {
            cs::FlexDirection::Row => FlexDirection::Row,
            cs::FlexDirection::RowReverse => FlexDirection::RowReverse,
            cs::FlexDirection::Column => FlexDirection::Column,
            cs::FlexDirection::ColumnReverse => FlexDirection::ColumnReverse,
        },
        flex_wrap: if s.flex_wrap {
            FlexWrap::Wrap
        } else {
            FlexWrap::NoWrap
        },
        justify_content: justify(s.justify_content),
        align_items: align_items(s.align_items),
        align_self: (s.align_self != Align::Normal).then(|| align_items(s.align_self)),
        justify_items: align_items(s.justify_items),
        justify_self: (s.justify_self != Align::Normal).then(|| align_items(s.justify_self)),
        align_content: justify(s.align_content),
        grid_row: taffy::Line {
            start: placement(s.grid.row_start.as_deref()),
            end: placement(s.grid.row_end.as_deref()),
        },
        grid_column: taffy::Line {
            start: placement(s.grid.column_start.as_deref()),
            end: placement(s.grid.column_end.as_deref()),
        },
        gap: Size {
            width: lp(s.column_gap),
            height: lp(s.row_gap),
        },
        flex_grow: s.flex_grow,
        flex_shrink: s.flex_shrink,
        flex_basis: dim(s.flex_basis),
        ..Default::default()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::css::{ElementState, author_sheets, compute_styles};
    use crate::dom::parse_html;

    struct Laid {
        dom: Dom,
        tree: LayoutTree,
    }

    fn lay(html: &str, w: f32, h: f32) -> Laid {
        let dom = parse_html(html);
        let sheets = author_sheets(&dom, &[]);
        let refs: Vec<&str> = sheets.iter().map(String::as_str).collect();
        let styles = compute_styles(&dom, &refs, &ElementState::default());
        let tree = layout(&dom, &styles, &[], &Fonts::new(), w, h, &Replaced::new());
        Laid { dom, tree }
    }

    impl Laid {
        /// Absolute border box of the element with `id`.
        fn rect(&self, id: &str) -> RectF {
            let target = self.dom.element_by_id(id).unwrap();
            fn walk(t: &LayoutTree, i: usize, ox: f32, oy: f32, target: NodeId) -> Option<RectF> {
                let b = &t.boxes[i];
                let r = RectF {
                    x: ox + b.rect.x,
                    y: oy + b.rect.y,
                    ..b.rect
                };
                if b.node == Some(target) {
                    return Some(r);
                }
                let (cx, cy) = b.content_offset();
                let atomics = b.inline.iter().flat_map(|il| {
                    il.lines.iter().flat_map(move |line| {
                        line.items.iter().filter_map(move |p| match p {
                            inline::Placed::Atomic { index, x, y, .. } => {
                                Some((*index, r.x + cx + x, r.y + cy + line.top + y))
                            }
                            _ => None,
                        })
                    })
                });
                for (i, ax, ay) in atomics {
                    if let Some(found) = walk(t, i, ax, ay, target) {
                        return Some(found);
                    }
                }
                b.children
                    .iter()
                    .find_map(|&c| walk(t, c, r.x, r.y, target))
            }
            walk(&self.tree, 0, 0.0, 0.0, target).unwrap()
        }

        fn lines(&self, id: &str) -> usize {
            let target = self.dom.element_by_id(id).unwrap();
            let b = self
                .tree
                .boxes
                .iter()
                .find(|b| b.node == Some(target))
                .unwrap();
            b.inline.as_ref().map_or(0, |i| i.lines.len())
        }
    }

    #[test]
    fn blocks_stack_with_margins() {
        let l = lay(
            r#"<style>body{margin:0} div{height:20px;margin:10px 0}</style>
               <div id=a></div><div id=b></div>"#,
            300.0,
            200.0,
        );
        let (a, b) = (l.rect("a"), l.rect("b"));
        assert_eq!(a.w, 300.0);
        assert_eq!(a.y, 10.0);
        assert_eq!(b.y, 40.0, "adjacent vertical margins collapse");
    }

    #[test]
    fn vertical_writing_swaps_the_axes() {
        let text = "字".repeat(12);
        let html = format!(
            "<style>body{{margin:0}} #v{{writing-mode:vertical-rl;height:100px;font-size:20px;line-height:30px;display:inline-block}}</style><div id=v>{text}</div>"
        );
        let l = lay(&html, 800.0, 600.0);
        let r = l.rect("v");
        // 12 characters of 20px in columns 100px long: 3 columns of 30px.
        assert_eq!(l.lines("v"), 3);
        assert_eq!((r.w, r.h), (90.0, 100.0));
    }

    #[test]
    fn multi_column_layout_balances_lines() {
        let text = "word ".repeat(60);
        let html = format!(
            "<style>body{{margin:0}} #m{{column-count:3;column-gap:20px;width:320px}}</style>\
             <div id=m>{text}</div><p id=after style='margin:0'>x</p>"
        );
        let l = lay(&html, 800.0, 600.0);
        let target = l.dom.element_by_id("m").unwrap();
        let b = l
            .tree
            .boxes
            .iter()
            .find(|b| b.node == Some(target))
            .unwrap();
        assert_eq!(b.columns, Some((3, 280.0 / 3.0, 20.0)));
        let lines = &b.inline.as_ref().unwrap().lines;
        let in_col = |k: usize| {
            let x0 = k as f32 * (280.0 / 3.0 + 20.0);
            lines
                .iter()
                .filter(|l| match l.items.first() {
                    Some(inline::Placed::Text { x, .. }) => *x >= x0 - 1.0 && *x < x0 + 50.0,
                    _ => false,
                })
                .count()
        };
        let counts = [in_col(0), in_col(1), in_col(2)];
        assert_eq!(counts.iter().sum::<usize>(), lines.len());
        assert!(
            counts.iter().max().unwrap() - counts.iter().min().unwrap() <= 1,
            "{counts:?}"
        );
        let tallest = lines.iter().map(|l| l.top + l.height).fold(0.0, f32::max);
        assert!(
            (b.rect.h - tallest).abs() < 1.0,
            "the box is as tall as a column"
        );
        assert_eq!(l.rect("after").y, b.rect.h);
    }

    #[test]
    fn calc_and_min_use_the_containing_block() {
        let l = lay(
            r#"<style>body{margin:0} #box{width:400px;height:200px}
               #a{width:calc(100% - 100px);height:calc(50% - 10px)} #b{width:min(100%, 600px)}
               #c{width:clamp(10px, 50%, 150px)} #d{width:max(10%, 50px);margin-left:calc(25% - 20px)}</style>
               <div id=box><div id=a></div><div id=b></div><div id=c></div><div id=d></div></div>"#,
            1000.0,
            600.0,
        );
        assert_eq!((l.rect("a").w, l.rect("a").h), (300.0, 90.0));
        assert_eq!(l.rect("b").w, 400.0);
        assert_eq!(l.rect("c").w, 150.0);
        assert_eq!((l.rect("d").w, l.rect("d").x), (50.0, 80.0));
    }

    #[test]
    fn text_wraps_to_container_width() {
        let text = "word ".repeat(40);
        let html = format!("<style>body{{margin:0}}</style><p id=p style='margin:0'>{text}</p>");
        let wide = lay(&html, 2000.0, 100.0);
        let narrow = lay(&html, 200.0, 100.0);
        assert_eq!(wide.lines("p"), 1);
        assert!(narrow.lines("p") > 3);
        assert!(narrow.rect("p").h > wide.rect("p").h);
    }

    #[test]
    fn flex_row_distributes_space() {
        let l = lay(
            r#"<style>body{margin:0} #row{display:flex;gap:10px;height:30px}
               #fixed{width:50px} #grow{flex:1}</style>
               <div id=row><div id=fixed></div><div id=grow></div><span id=s>x</span></div>"#,
            400.0,
            100.0,
        );
        let (f, g, s) = (l.rect("fixed"), l.rect("grow"), l.rect("s"));
        assert_eq!(f.w, 50.0);
        assert_eq!(g.x, 60.0);
        assert!(s.w > 0.0 && s.x + s.w <= 400.0 + 0.5);
        assert!((g.w - (400.0 - 50.0 - 20.0 - s.w)).abs() < 1.0);
        assert_eq!(g.h, 30.0, "items stretch to the row height");
    }

    #[test]
    fn mixed_inline_and_block_children() {
        let l = lay(
            "<style>body{margin:0}</style><div id=d>before<p id=p style='margin:0'>para</p>after</div>",
            300.0,
            100.0,
        );
        let d = l.rect("d");
        let p = l.rect("p");
        assert!(p.y > d.y, "anonymous block for 'before' comes first");
        assert!(d.h > p.h * 2.5, "three lines tall");
    }

    #[test]
    fn flex_items_do_not_shrink_below_unbreakable_content() {
        let l = lay(
            r#"<style>body{margin:0} #row{display:flex;width:100px}
               .box{display:inline-block;width:80px;height:10px}</style>
               <div id=row><div id=a><span class=box></span></div><div id=b><span class=box></span></div></div>"#,
            300.0,
            100.0,
        );
        assert!(l.rect("a").w >= 80.0, "{:?}", l.rect("a"));
        assert!(l.rect("b").x >= 80.0);
    }

    #[test]
    fn tables_place_cells_in_a_grid() {
        let l = lay(
            r#"<style>body{margin:0} table{border-spacing:0} td{padding:0}
               .tall{height:60px}</style>
               <table id=t>
                 <caption id=cap>Caption</caption>
                 <thead><tr><th id=h1>Name</th><th id=h2>Value</th><th id=h3>Unit</th></tr></thead>
                 <tr><td id=a rowspan=2 class=tall>A</td><td id=b colspan=2>B wide</td></tr>
                 <tr><td id=c>C</td><td id=d>D</td></tr>
                 <tfoot><tr><td id=f colspan=3>footer</td></tr></tfoot>
               </table><p id=after style="margin:0">after</p>"#,
            600.0,
            400.0,
        );
        let (cap, h1, h2) = (l.rect("cap"), l.rect("h1"), l.rect("h2"));
        let (a, b, c, d, f) = (
            l.rect("a"),
            l.rect("b"),
            l.rect("c"),
            l.rect("d"),
            l.rect("f"),
        );
        let t = l.rect("t");
        assert!(t.w < 600.0, "tables shrink to fit: {t:?}");
        assert!(cap.y < h1.y, "caption above the header");
        assert_eq!(h1.y, h2.y);
        assert!(h2.x > h1.x);
        assert_eq!(a.x, h1.x, "first column");
        assert_eq!(b.x, h2.x, "B starts in column 2");
        assert!(
            (b.w - (l.rect("h3").x + l.rect("h3").w - h2.x)).abs() < 1.0,
            "B spans 2 columns"
        );
        assert_eq!(c.x, h2.x, "C skips the rowspan slot");
        assert_eq!(d.x, l.rect("h3").x);
        assert!((a.h - (c.y + c.h - b.y)).abs() < 1.0, "A spans 2 rows");
        assert!(f.y > c.y, "footer last");
        assert!(l.rect("after").y >= t.y + t.h);
    }

    #[test]
    fn table_attributes_and_vertical_align() {
        let l = lay(
            r#"<style>body{margin:0}</style>
               <table border=1 cellpadding=4 cellspacing=0>
                 <tr><td id=x style="height:50px">x</td><td id=y valign=top>y</td></tr>
               </table>"#,
            400.0,
            200.0,
        );
        let target = l.dom.element_by_id("x").unwrap();
        let b = l
            .tree
            .boxes
            .iter()
            .find(|b| b.node == Some(target))
            .unwrap();
        assert_eq!(b.border[0], 1.0);
        assert_eq!(b.padding[0], 4.0);
        assert!(
            b.content_shift > 10.0,
            "middle-aligned: {}",
            b.content_shift
        );
        let target = l.dom.element_by_id("y").unwrap();
        let b = l
            .tree
            .boxes
            .iter()
            .find(|b| b.node == Some(target))
            .unwrap();
        assert_eq!(b.content_shift, 0.0);
        assert!(b.rect.h >= 50.0, "cells stretch to the row");
    }

    #[test]
    fn text_avoids_floats() {
        let text = "word ".repeat(60);
        let l = lay(
            &format!(
                r#"<style>body{{margin:0}} p{{margin:0}}</style><div style="width:300px">
                <div id=f style="float:right;width:100px;height:100px"></div>
                <p id=p>{text}</p></div>"#
            ),
            800.0,
            600.0,
        );
        let (f, p) = (l.rect("f"), l.rect("p"));
        assert_eq!(f.x, 200.0);
        assert!(p.x + p.w <= 200.5, "text block beside the float: {p:?}");
    }

    #[test]
    fn grid_template_areas_and_tracks() {
        let l = lay(
            r#"<style>body{margin:0}
               #g{display:grid;grid-template-columns:100px 1fr 50px;
                  grid-template-areas:"head head head" "nav main side";gap:10px;width:400px}
               #h{grid-area:head;height:20px} #n{grid-area:nav} #m{grid-area:main} #s{grid-area:side}
               #r{display:grid;grid-template-columns:repeat(3,minmax(0,1fr));width:300px}
               </style>
               <div id=g><div id=m>m</div><div id=s>s</div><div id=h></div><div id=n>n</div></div>
               <div id=r><div id=r1>1</div><div id=r2>2</div><div id=r3>3</div><div id=r4>4</div></div>"#,
            800.0,
            600.0,
        );
        let (h, n, m, s) = (l.rect("h"), l.rect("n"), l.rect("m"), l.rect("s"));
        assert_eq!((h.x, h.w), (0.0, 400.0));
        assert_eq!((n.x, n.w), (0.0, 100.0));
        assert_eq!(m.x, 110.0);
        assert_eq!(s.x, 350.0);
        assert!(n.y > h.y);
        assert_eq!(l.rect("r2").x, 100.0);
        assert_eq!(
            zero_px("minmax(0,59.25rem) min-content"),
            "minmax(0px,948px) min-content"
        );
        assert!(
            l.rect("r4").y > l.rect("r1").y,
            "auto-placed into the next row"
        );
    }

    #[test]
    fn inline_block_is_atomic() {
        let l = lay(
            "<style>body{margin:0}</style><div id=d>a <button id=b>Click</button> c</div>",
            300.0,
            100.0,
        );
        assert_eq!(l.lines("d"), 1);
        let b = l.rect("b");
        assert!(b.w > 20.0 && b.h > 10.0);
    }

    #[test]
    fn percent_height_and_absolute_position() {
        let l = lay(
            r#"<style>html,body{height:100%;margin:0} #full{height:100%;position:relative}
               #abs{position:absolute;right:0;bottom:0;width:10px;height:10px}</style>
               <div id=full><div id=abs></div></div>"#,
            200.0,
            150.0,
        );
        assert_eq!(l.rect("full").h, 150.0);
        let a = l.rect("abs");
        assert_eq!((a.x, a.y), (190.0, 140.0));
    }

    #[test]
    fn overflow_reports_content_size() {
        let l = lay(
            r#"<style>body{margin:0} #s{height:50px;overflow:auto} #c{height:200px}</style>
               <div id=s><div id=c></div></div>"#,
            100.0,
            100.0,
        );
        let target = l.dom.element_by_id("s").unwrap();
        let b = l
            .tree
            .boxes
            .iter()
            .find(|b| b.node == Some(target))
            .unwrap();
        assert_eq!(b.rect.h, 50.0);
        assert!(b.content_h >= 200.0);
    }
}
