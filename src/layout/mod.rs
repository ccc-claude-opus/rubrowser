//! Layout: turns a styled DOM into positioned boxes.
//!
//! Block and flex layout are done by `taffy`. Runs of inline content become
//! taffy leaves whose size comes from our own inline layout ([`inline`]).
//! `inline-block` elements are laid out first as separate taffy trees and
//! then placed in lines like big glyphs.

pub mod inline;

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
}

impl LBox {
    /// Content box relative to this box's border box.
    pub fn content_offset(&self) -> (f32, f32) {
        (
            self.border[cs::LEFT] + self.padding[cs::LEFT],
            self.border[cs::TOP] + self.padding[cs::TOP],
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
}

struct Builder<'a> {
    dom: &'a Dom,
    styles: &'a [Option<Css>],
    fonts: &'a Fonts,
    viewport: (f32, f32),
    boxes: Vec<LBox>,
}

/// Lays out the document in a viewport of `width × height` logical pixels.
pub fn layout(
    dom: &Dom,
    styles: &[Option<Css>],
    fonts: &Fonts,
    width: f32,
    height: f32,
) -> LayoutTree {
    let mut b = Builder {
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
    });
    if let Some(html) = dom.html().filter(|&h| styles[h].is_some()) {
        let mut tree: TaffyTree<Ctx> = TaffyTree::new();
        let root_style = Style {
            display: Display::Block,
            size: Size {
                width: length(width),
                height: length(height),
            },
            ..Default::default()
        };
        let html_node = b.build(&mut tree, html);
        let root = tree.new_with_children(root_style, &[html_node]).unwrap();
        b.compute(&mut tree, root, width, AvailableSpace::Definite(height));
        let html_box = b.collect(&tree, html_node);
        let content_h = b.boxes[html_box].rect.h.max(height);
        let vp = &mut b.boxes[LayoutTree::VIEWPORT];
        vp.children.push(html_box);
        vp.content_h = content_h;
    }
    LayoutTree { boxes: b.boxes }
}

impl Builder<'_> {
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
                            return Size {
                                width: known.width.unwrap_or(w),
                                height: known.height.unwrap_or(h),
                            };
                        }
                        let Some(content) = ctx.and_then(|c| c.inline.as_ref()) else {
                            return Size::ZERO;
                        };
                        let max_w = known.width.unwrap_or(match avail.width {
                            AvailableSpace::Definite(w) => w,
                            AvailableSpace::MinContent => 0.0,
                            AvailableSpace::MaxContent => f32::INFINITY,
                        });
                        let l = inline::layout(content, max_w, styles, fonts);
                        Size {
                            width: known.width.unwrap_or(l.width.min(max_w)),
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
        let css = self.style(id).cloned().unwrap_or_default();
        let is_flex = matches!(css.display, CDisplay::Flex | CDisplay::InlineFlex);
        let mut tstyle = convert(&css);
        tstyle.display = if is_flex {
            Display::Flex
        } else {
            Display::Block
        };

        if self.dom.tag(id) == Some("input") {
            let ctx = Ctx {
                node: Some(id),
                style_node: id,
                inline: None,
                intrinsic: Some((0.0, css.line_height_px())),
            };
            return tree.new_leaf_with_context(tstyle, ctx).unwrap();
        }

        // Split children into block-level boxes and runs of inline content.
        let mut parts: Vec<Part> = Vec::new();
        self.collect_children(tree, id, is_flex, &mut parts);

        let all_inline = parts.iter().all(|p| matches!(p, Part::Inline(_)));
        if !is_flex && all_inline {
            let items: Vec<Item> = parts
                .into_iter()
                .flat_map(|p| match p {
                    Part::Inline(items) => items,
                    Part::Block(_) => Vec::new(),
                })
                .collect();
            let ctx = Ctx {
                node: Some(id),
                style_node: id,
                inline: Some(Content { owner: id, items }),
                intrinsic: None,
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
                        ..Default::default()
                    };
                    let ctx = Ctx {
                        node: None,
                        style_node: id,
                        inline: Some(content),
                        intrinsic: None,
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
        for &c in self.dom.children(parent) {
            let Some(css) = self.style(c) else { continue };
            if self.dom.text(c).is_some() {
                push_inline(
                    parts,
                    Item::Text {
                        node: c,
                        text: self.dom.text(c).unwrap_or("").to_string(),
                    },
                );
                continue;
            }
            if self.dom.element(c).is_none() {
                continue;
            }
            let block_level =
                flex || css.position == cs::Position::Absolute || !css.display.is_inline_level();
            if self.dom.tag(c) == Some("br") && !flex {
                push_inline(parts, Item::Break { node: c });
            } else if block_level {
                let child = self.build(tree, c);
                parts.push(Part::Block(child));
            } else if css.display == CDisplay::Inline {
                self.collect_children(tree, c, false, parts);
            } else {
                // inline-block / inline-flex: an atomic inline box.
                let (index, width, height, baseline) = self.atomic(c);
                push_inline(
                    parts,
                    Item::Atomic {
                        index,
                        width,
                        height,
                        baseline,
                    },
                );
            }
        }
    }

    /// Lays out an inline-block element on its own and returns its box
    /// index and margin-box size.
    fn atomic(&mut self, id: NodeId) -> (usize, f32, f32, f32) {
        let mut tree: TaffyTree<Ctx> = TaffyTree::new();
        let node = self.build(&mut tree, id);
        // Shrink-to-fit: lay out at max-content, capped by the viewport.
        let wrapper = tree
            .new_with_children(
                Style {
                    display: Display::Flex,
                    align_items: AlignItems::START,
                    ..Default::default()
                },
                &[node],
            )
            .unwrap();
        self.compute(
            &mut tree,
            wrapper,
            self.viewport.0,
            AvailableSpace::MaxContent,
        );
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
        let inline = ctx.and_then(|c| c.inline.as_ref()).map(|content| {
            let w = l.size.width - border[1] - border[3] - padding[1] - padding[3];
            let mut il = inline::layout(content, w.max(0.0), self.styles, self.fonts);
            if let Some(owner) = self.style(content.owner) {
                inline::align(&mut il, owner.text_align, w);
            }
            il
        });
        let absolute = dom_node
            .and_then(|n| self.style(n))
            .is_some_and(|s| s.position == cs::Position::Absolute);
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
        });
        let kids: Vec<usize> = tree
            .children(node)
            .unwrap()
            .into_iter()
            .map(|c| self.collect(tree, c))
            .collect();
        self.boxes[index].children = kids;
        index
    }
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

fn dim(d: Dim) -> Dimension {
    match d {
        Dim::Auto => Dimension::auto(),
        Dim::Px(v) => Dimension::length(v),
        Dim::Percent(p) => Dimension::percent(p / 100.0),
    }
}

fn lpa(d: Dim) -> LengthPercentageAuto {
    match d {
        Dim::Auto => LengthPercentageAuto::auto(),
        Dim::Px(v) => LengthPercentageAuto::length(v),
        Dim::Percent(p) => LengthPercentageAuto::percent(p / 100.0),
    }
}

fn lp(d: Dim) -> LengthPercentage {
    match d {
        Dim::Auto => LengthPercentage::length(0.0),
        Dim::Px(v) => LengthPercentage::length(v),
        Dim::Percent(p) => LengthPercentage::percent(p / 100.0),
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
        box_sizing: match s.box_sizing {
            cs::BoxSizing::BorderBox => BoxSizing::BorderBox,
            cs::BoxSizing::ContentBox => BoxSizing::ContentBox,
        },
        position: match s.position {
            cs::Position::Absolute => Position::Absolute,
            _ => Position::Relative,
        },
        inset: rect4(s.inset.map(lpa)),
        overflow: Point {
            x: overflow,
            y: overflow,
        },
        scrollbar_width: 0.0,
        size: Size {
            width: dim(s.width),
            height: dim(s.height),
        },
        min_size: Size {
            width: lpa(s.min_width),
            height: lpa(s.min_height),
        },
        max_size: Size {
            width: lpa(s.max_width),
            height: lpa(s.max_height),
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
        let tree = layout(&dom, &styles, &Fonts::new(), w, h);
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
                            inline::Placed::Atomic { index, x, y } => {
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
