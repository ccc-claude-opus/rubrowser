//! A live document: DOM + styles + layout, with hover, scrolling and
//! hit-testing. Used for both web pages and the browser's own chrome.

use crate::css::style::Cursor as CssCursor;
use crate::css::{ElementState, Style, author_sheets, compute_styles};
use crate::dom::{Dom, NodeId, parse_html};
use crate::gfx::Canvas;
use crate::input::Cursor;
use crate::layout::inline::Placed;
use crate::layout::{LayoutTree, RectF, layout};
use crate::paint::{Painter, ScrollOffsets};
use crate::text::Fonts;

/// Result of a hit test.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Hit {
    /// Innermost element under the point.
    pub element: Option<NodeId>,
    /// Scroll containers under the point, innermost first; `None` is the
    /// viewport.
    pub scrollers: Vec<Option<NodeId>>,
}

pub struct Document {
    pub url: String,
    pub dom: Dom,
    /// Contents of loaded `<link rel=stylesheet>` elements.
    linked_css: Vec<(NodeId, String)>,
    styles: Vec<Option<Style>>,
    tree: LayoutTree,
    state: ElementState,
    scroll: ScrollOffsets,
    size: (f32, f32),
    needs_style: bool,
    needs_layout: bool,
}

impl Document {
    pub fn new(dom: Dom, url: &str) -> Self {
        Document {
            url: url.to_string(),
            dom,
            linked_css: Vec::new(),
            styles: Vec::new(),
            tree: LayoutTree::default(),
            state: ElementState::default(),
            scroll: ScrollOffsets::new(),
            size: (0.0, 0.0),
            needs_style: true,
            needs_layout: true,
        }
    }

    pub fn from_html(html: &str, url: &str) -> Self {
        Self::new(parse_html(html), url)
    }

    pub fn title(&self) -> Option<String> {
        self.dom.title()
    }

    /// Marks the DOM as changed (by a script or the browser).
    pub fn dom_changed(&mut self) {
        self.needs_style = true;
    }

    pub fn dom_mut(&mut self) -> &mut Dom {
        self.needs_style = true;
        &mut self.dom
    }

    pub fn needs_update(&self) -> bool {
        self.needs_style || self.needs_layout
    }

    /// Viewport size in logical pixels.
    pub fn set_viewport(&mut self, w: f32, h: f32) {
        if self.size != (w, h) {
            self.size = (w, h);
            self.needs_layout = true;
        }
    }

    pub fn viewport(&self) -> (f32, f32) {
        self.size
    }

    /// `<link rel=stylesheet href>` elements: (node, href).
    pub fn stylesheet_links(&self) -> Vec<(NodeId, String)> {
        self.dom
            .descendants(Dom::ROOT)
            .into_iter()
            .filter(|&n| {
                self.dom.tag(n) == Some("link")
                    && self.dom.attr(n, "rel").is_some_and(|r| {
                        r.split_whitespace()
                            .any(|w| w.eq_ignore_ascii_case("stylesheet"))
                    })
            })
            .filter_map(|n| Some((n, self.dom.attr(n, "href")?.to_string())))
            .collect()
    }

    pub fn add_stylesheet(&mut self, link: NodeId, css: String) {
        self.linked_css.push((link, css));
        self.needs_style = true;
    }

    pub fn style(&self, id: NodeId) -> Option<&Style> {
        self.styles.get(id).and_then(Option::as_ref)
    }

    /// Recomputes styles and layout if needed.
    pub fn update(&mut self, fonts: &Fonts) {
        if self.needs_style {
            let sheets = author_sheets(&self.dom, &self.linked_css);
            let refs: Vec<&str> = sheets.iter().map(String::as_str).collect();
            let styles = compute_styles(&self.dom, &refs, &self.state);
            if styles != self.styles {
                self.styles = styles;
                self.needs_layout = true;
            }
            self.needs_style = false;
        }
        if self.needs_layout {
            self.tree = layout(&self.dom, &self.styles, fonts, self.size.0, self.size.1);
            self.needs_layout = false;
            self.clamp_scroll();
        }
    }

    pub fn paint(&self, canvas: &mut Canvas, origin: (f32, f32), scale: f32, fonts: &Fonts) {
        Painter {
            dom: &self.dom,
            styles: &self.styles,
            fonts,
            tree: &self.tree,
            scroll: &self.scroll,
            scale,
        }
        .paint(canvas, origin);
    }

    pub fn state(&self) -> ElementState {
        self.state
    }

    /// Updates hover/active/focus. Returns true if anything changed.
    pub fn set_state(&mut self, state: ElementState) -> bool {
        if state == self.state {
            return false;
        }
        self.state = state;
        self.needs_style = true;
        true
    }

    /// What is at logical point `(x, y)` (relative to the viewport).
    pub fn hit_test(&self, x: f32, y: f32) -> Hit {
        let mut hit = Hit::default();
        if self.tree.boxes.is_empty() {
            return hit;
        }
        let vp = &self.tree.boxes[LayoutTree::VIEWPORT];
        if !vp.rect.contains(x, y) {
            return hit;
        }
        let sy = self.scroll_of(None);
        for &c in vp.children.iter().rev() {
            if self.hit_box(c, x, y, 0.0, -sy, &mut hit) {
                break;
            }
        }
        if vp.content_h > vp.rect.h {
            hit.scrollers.push(None);
        }
        hit.scrollers.reverse();
        hit.scrollers.sort_by_key(|s| s.is_none());
        hit
    }

    /// Records into `hit` and returns true if the point is inside box `i`
    /// (positioned at offset `ox, oy`). Scroll containers are pushed
    /// outermost first; the caller reverses them.
    fn hit_box(&self, i: usize, x: f32, y: f32, ox: f32, oy: f32, hit: &mut Hit) -> bool {
        let b = &self.tree.boxes[i];
        let r = RectF {
            x: ox + b.rect.x,
            y: oy + b.rect.y,
            ..b.rect
        };
        let style = self.style(b.style_node);
        let clips = b.node.is_some()
            && style.is_some_and(|s| s.overflow != crate::css::style::Overflow::Visible);
        let inside = r.contains(x, y);
        if clips && !inside {
            return false;
        }
        let scrolls =
            clips && style.is_some_and(|s| s.overflow.scrolls()) && b.content_h > b.rect.h;
        let sy = if clips { self.scroll_of(b.node) } else { 0.0 };
        if inside && scrolls {
            hit.scrollers.push(b.node);
        }
        // Children on top first: absolute ones, then in-flow in reverse.
        for pass in [true, false] {
            for &c in b.children.iter().rev() {
                if self.tree.boxes[c].absolute == pass && self.hit_box(c, x, y, r.x, r.y - sy, hit)
                {
                    return true;
                }
            }
        }
        if let Some(il) = &b.inline {
            let (cx, cy) = b.content_offset();
            let (bx, by) = (r.x + cx, r.y + cy - sy);
            for line in &il.lines {
                for item in &line.items {
                    match item {
                        Placed::Text {
                            node, x: tx, width, ..
                        } => {
                            let rect = RectF {
                                x: bx + tx,
                                y: by + line.top,
                                w: *width,
                                h: line.height,
                            };
                            if rect.contains(x, y) {
                                hit.element = self.dom.parent(*node);
                                return true;
                            }
                        }
                        Placed::Atomic {
                            index,
                            x: ax,
                            y: ay,
                        } => {
                            if self.hit_box(*index, x, y, bx + ax, by + line.top + ay, hit) {
                                return true;
                            }
                        }
                    }
                }
            }
        }
        if inside {
            hit.element = Some(b.node.unwrap_or(b.style_node));
            return true;
        }
        false
    }

    fn scroll_of(&self, node: Option<NodeId>) -> f32 {
        self.scroll.get(&node).copied().unwrap_or(0.0)
    }

    /// Maximum scroll offset of a scroll container.
    fn max_scroll(&self, node: Option<NodeId>) -> f32 {
        let b = match node {
            None => self.tree.boxes.first(),
            Some(n) => self.tree.boxes.iter().find(|b| b.node == Some(n)),
        };
        b.map_or(0.0, |b| {
            let pr = b.padding_rect();
            let visible = if node.is_none() { b.rect.h } else { pr.h };
            (b.content_h - visible).max(0.0)
        })
    }

    /// Scrolls the innermost scroller under `(x, y)` that can move by `dy`.
    /// Returns true if something scrolled.
    pub fn scroll_by(&mut self, x: f32, y: f32, dy: f32) -> bool {
        for s in self.hit_test(x, y).scrollers {
            let cur = self.scroll_of(s);
            let next = (cur + dy).clamp(0.0, self.max_scroll(s));
            if (next - cur).abs() > 0.01 {
                self.scroll.insert(s, next);
                return true;
            }
        }
        false
    }

    fn clamp_scroll(&mut self) {
        let keys: Vec<_> = self.scroll.keys().copied().collect();
        for k in keys {
            let max = self.max_scroll(k);
            if let Some(v) = self.scroll.get_mut(&k) {
                *v = v.clamp(0.0, max);
            }
        }
    }

    /// The nearest `<a href>` at or above `node`.
    pub fn link(&self, node: NodeId) -> Option<String> {
        let mut n = Some(node);
        while let Some(id) = n {
            if self.dom.tag(id) == Some("a")
                && let Some(href) = self.dom.attr(id, "href")
            {
                return Some(href.to_string());
            }
            n = self.dom.parent(id);
        }
        None
    }

    /// Mouse cursor for the element under the pointer.
    pub fn cursor(&self, element: Option<NodeId>) -> Cursor {
        let Some(st) = element.and_then(|e| self.style(e)) else {
            return Cursor::Default;
        };
        match st.cursor {
            CssCursor::Pointer => Cursor::Pointer,
            CssCursor::Text => Cursor::Text,
            CssCursor::Default => Cursor::Default,
            CssCursor::Auto => {
                // Text is selectable-looking; everything else is an arrow.
                let over_text = element.is_some_and(|e| {
                    self.dom
                        .children(e)
                        .iter()
                        .any(|&c| self.dom.text(c).is_some_and(|t| !t.trim().is_empty()))
                });
                if over_text {
                    Cursor::Text
                } else {
                    Cursor::Default
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn doc(html: &str, w: f32, h: f32) -> (Document, Fonts) {
        let fonts = Fonts::new();
        let mut d = Document::from_html(html, "test.html");
        d.set_viewport(w, h);
        d.update(&fonts);
        (d, fonts)
    }

    #[test]
    fn hit_testing_finds_innermost_element() {
        let (d, _) = doc(
            r#"<style>body{margin:0} #box{width:100px;height:50px}</style>
               <div id=box></div><p style="margin:0"><a href="x.html" id=a>link</a> text</p>"#,
            300.0,
            200.0,
        );
        let boxed = d.dom.element_by_id("box");
        assert_eq!(d.hit_test(10.0, 10.0).element, boxed);
        let a = d.dom.element_by_id("a");
        let hit = d.hit_test(5.0, 58.0);
        assert_eq!(hit.element, a);
        assert_eq!(d.link(a.unwrap()).as_deref(), Some("x.html"));
        assert_eq!(d.cursor(a), Cursor::Pointer);
    }

    #[test]
    fn viewport_and_element_scrolling() {
        let (mut d, fonts) = doc(
            r#"<style>body{margin:0} #s{height:50px;overflow:auto} #in{height:300px}
               #tall{height:1000px}</style>
               <div id=s><div id=in></div></div><div id=tall></div>"#,
            200.0,
            200.0,
        );
        let s = d.dom.element_by_id("s");
        let hit = d.hit_test(10.0, 10.0);
        assert_eq!(hit.scrollers, vec![s, None]);
        assert!(d.scroll_by(10.0, 10.0, 100.0));
        assert_eq!(d.scroll_of(s), 100.0);
        assert!(d.scroll_by(10.0, 10.0, 1000.0));
        assert_eq!(d.scroll_of(s), 250.0, "clamped to content");
        assert!(d.scroll_by(10.0, 10.0, 50.0), "then the viewport scrolls");
        assert_eq!(d.scroll_of(None), 50.0);
        d.set_viewport(200.0, 2000.0);
        d.update(&fonts);
        assert_eq!(d.scroll_of(None), 0.0, "re-clamped after relayout");
    }

    #[test]
    fn hover_restyles() {
        let (mut d, fonts) = doc(
            "<style>p:hover{font-size:40px}</style><p id=p>x</p>",
            200.0,
            200.0,
        );
        let p = d.dom.element_by_id("p").unwrap();
        let before = d.style(p).unwrap().font_size;
        assert!(d.set_state(ElementState {
            hover: Some(p),
            ..Default::default()
        }));
        d.update(&fonts);
        assert_eq!(before, 16.0);
        assert_eq!(d.style(p).unwrap().font_size, 40.0);
    }

    #[test]
    fn painting_draws_backgrounds_and_text() {
        let (d, fonts) = doc(
            r#"<body style="margin:0;background:#00ff00"><h1 style="margin:0">Hi</h1></body>"#,
            50.0,
            50.0,
        );
        let mut c = Canvas::new(100, 100);
        d.paint(&mut c, (0.0, 0.0), 2.0, &fonts);
        assert_eq!(c.pixels()[99 * 100 + 99], 0x00ff00);
        assert!(c.pixels().iter().any(|&p| p == 0));
    }
}
