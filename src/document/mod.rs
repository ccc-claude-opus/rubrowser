//! A live document: DOM + styles + layout, with hover, scrolling and
//! hit-testing. Used for both web pages and the browser's own chrome.

use crate::css::media::{MediaEnv, MediaList};
use crate::css::selector::PseudoElement;
use crate::css::style::Cursor as CssCursor;
use crate::css::syntax::{Rule, Sheet, parse_stylesheet};
use crate::css::{Cascade, ElementState, Matcher, Style, compute};
use crate::dom::{Dom, NodeId, parse_html};
use crate::gfx::Canvas;
use crate::input::Cursor;
use crate::layout::inline::Placed;
use crate::layout::{LayoutTree, RectF, Replaced, layout};
use crate::paint::{Painter, ScrollOffsets};
use crate::text::Fonts;
use crate::ui::TextField;
use std::collections::HashMap;
use std::rc::Rc;

mod forms;
mod image;

pub use forms::{
    Control, FormState, control, form_of, label_target, option_label, options, urlencode,
};
pub use image::{Bitmap, ImageState, Images};

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
    linked_css: HashMap<NodeId, Rc<Sheet>>,
    /// Parsed `<style>` elements: node → (text, sheet).
    inline_css: HashMap<NodeId, (String, Rc<Sheet>)>,
    /// Sheets loaded through `@import`, by URL.
    imported: HashMap<String, Rc<Sheet>>,
    /// `@import` URLs already requested.
    requested_imports: std::collections::HashSet<String>,
    /// The cascade index for the current sheets and viewport.
    cascade: Option<(Vec<*const Sheet>, MediaEnv, Rc<Cascade>)>,
    styles: Vec<Option<Style>>,
    /// `::before` / `::after` / `::marker` styles.
    pub pseudo: HashMap<(NodeId, PseudoElement), Style>,
    tree: LayoutTree,
    state: ElementState,
    scroll: ScrollOffsets,
    size: (f32, f32),
    needs_style: bool,
    needs_layout: bool,
    /// Editing state of `<input>` elements, created on first use.
    inputs: HashMap<NodeId, TextField>,
    /// Whether the caret is in its visible blink phase.
    pub caret_on: bool,
    /// Option under the pointer in the open `<select>` list.
    pub popup_hover: Option<usize>,
    pub images: Images,
    pub forms: FormState,
}

impl Document {
    pub fn new(dom: Dom, url: &str) -> Self {
        Document {
            url: url.to_string(),
            dom,
            linked_css: HashMap::new(),
            inline_css: HashMap::new(),
            imported: HashMap::new(),
            requested_imports: Default::default(),
            cascade: None,
            styles: Vec::new(),
            pseudo: HashMap::new(),
            tree: LayoutTree::default(),
            state: ElementState::default(),
            scroll: ScrollOffsets::new(),
            size: (0.0, 0.0),
            needs_style: true,
            needs_layout: true,
            inputs: HashMap::new(),
            caret_on: true,
            popup_hover: None,
            images: Images::default(),
            forms: FormState::default(),
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
            // Media queries and viewport units may change.
            self.needs_style = true;
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

    /// Delivers the text of a `<link rel=stylesheet>` loaded from `url`.
    pub fn add_stylesheet(&mut self, link: NodeId, css: String, url: &str) {
        self.linked_css
            .insert(link, Rc::new(parse_stylesheet(&css, Some(url))));
        self.needs_style = true;
    }

    /// Delivers a sheet requested by `@import`.
    pub fn add_import(&mut self, url: &str, css: String) {
        self.imported
            .insert(url.to_string(), Rc::new(parse_stylesheet(&css, Some(url))));
        self.needs_style = true;
    }

    /// `@import` URLs of loaded sheets not requested yet.
    pub fn import_requests(&mut self) -> Vec<String> {
        let mut out = Vec::new();
        let sheets: Vec<Rc<Sheet>> = self
            .linked_css
            .values()
            .chain(self.inline_css.values().map(|(_, s)| s))
            .chain(self.imported.values())
            .cloned()
            .collect();
        for sheet in sheets {
            for r in &sheet.rules {
                if let Rule::Import(url, _) = r
                    && self.requested_imports.insert(url.clone())
                {
                    out.push(url.clone());
                }
            }
        }
        out
    }

    /// Author stylesheets in cascade order, with the media they apply to.
    fn author_sheets(&mut self) -> Vec<(Rc<Sheet>, MediaList)> {
        let base = self.base_url();
        let mut out = Vec::new();
        for n in self.dom.descendants(Dom::ROOT) {
            let media = MediaList::parse(self.dom.attr(n, "media").unwrap_or(""));
            let sheet = match self.dom.tag(n) {
                Some("style") => {
                    let text = self.dom.text_content(n);
                    match self.inline_css.get(&n) {
                        Some((t, sheet)) if *t == text => sheet.clone(),
                        _ => {
                            let sheet = Rc::new(parse_stylesheet(&text, Some(&base)));
                            self.inline_css.insert(n, (text, sheet.clone()));
                            sheet
                        }
                    }
                }
                Some("link") => match self.linked_css.get(&n) {
                    Some(sheet) if self.dom.attr(n, "disabled").is_none() => sheet.clone(),
                    _ => continue,
                },
                _ => continue,
            };
            self.push_with_imports(&sheet, media, &mut out, 0);
        }
        out
    }

    /// Adds `sheet` after the sheets it imports.
    fn push_with_imports(
        &self,
        sheet: &Rc<Sheet>,
        media: MediaList,
        out: &mut Vec<(Rc<Sheet>, MediaList)>,
        depth: u32,
    ) {
        if depth < 8 {
            for r in &sheet.rules {
                if let Rule::Import(url, m) = r
                    && let Some(imported) = self.imported.get(url)
                {
                    self.push_with_imports(imported, m.clone(), out, depth + 1);
                }
            }
        }
        out.push((sheet.clone(), media));
    }

    pub fn style(&self, id: NodeId) -> Option<&Style> {
        self.styles.get(id).and_then(Option::as_ref)
    }

    /// Recomputes styles and layout if needed.
    pub fn update(&mut self, fonts: &Fonts) {
        if self.needs_style {
            let sheets = self.author_sheets();
            let env = MediaEnv {
                width: self.size.0,
                height: self.size.1,
                scale: 1.0,
                dark: false,
            };
            let keys: Vec<*const Sheet> = sheets.iter().map(|(s, _)| Rc::as_ptr(s)).collect();
            let cascade = match &self.cascade {
                Some((k, e, c)) if *k == keys && *e == env => c.clone(),
                _ => {
                    let c = Rc::new(Cascade::new(&sheets, &env));
                    self.cascade = Some((keys, env, c.clone()));
                    c
                }
            };
            let forms = &self.forms;
            let dom = &self.dom;
            let checked = |n: NodeId| forms.checked(dom, n);
            let cx = Matcher {
                dom,
                state: self.state,
                checked: &checked,
            };
            let styles = compute(dom, &cascade, &cx, &env);
            if styles.nodes != self.styles || styles.pseudo != self.pseudo {
                self.styles = styles.nodes;
                self.pseudo = styles.pseudo;
                self.needs_layout = true;
            }
            self.needs_style = false;
        }
        if self.needs_layout {
            let replaced = self.replaced_sizes(fonts);
            self.tree = layout(
                &self.dom,
                &self.styles,
                fonts,
                self.size.0,
                self.size.1,
                &replaced,
            );
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
            inputs: &self.inputs,
            focus: self.state.focus,
            caret: self.caret_on,
            images: &self.images,
            forms: &self.forms,
        }
        .paint(canvas, origin);
        if let Some((rect, items)) = self.select_popup(fonts) {
            let open = self.forms.open_select.unwrap_or(Dom::ROOT);
            let selected = self.forms.selected_index(&self.dom, open);
            let style = self.style(open).cloned().unwrap_or_default();
            let hover = self.popup_hover;
            crate::paint::select_popup(
                canvas, fonts, scale, origin, rect, &items, selected, hover, &style,
            );
        }
    }

    /// The open `<select>`'s option list: its rectangle (viewport
    /// coordinates) and option labels.
    pub fn select_popup(&self, fonts: &Fonts) -> Option<(RectF, Vec<String>)> {
        let select = self.forms.open_select?;
        let (border, _) = self.box_rects(select)?;
        let st = self.style(select)?;
        let items: Vec<String> = options(&self.dom, select)
            .into_iter()
            .map(|o| option_label(&self.dom, o))
            .collect();
        let row = st.line_height_px() + 6.0;
        let widest = items
            .iter()
            .map(|t| fonts.measure(st.font(), st.font_size, t))
            .fold(0.0, f32::max);
        let w = border.w.max(widest + 24.0);
        let h = row * items.len() as f32 + 2.0;
        // Below the control, or above it if that does not fit.
        let below = border.y + border.h;
        let y = if below + h > self.size.1 && border.y - h >= 0.0 {
            border.y - h
        } else {
            below
        };
        Some((
            RectF {
                x: border.x,
                y,
                w,
                h,
            },
            items,
        ))
    }

    /// Which option of the open popup is at `(x, y)`: `Some(Some(i))` on an
    /// option, `Some(None)` elsewhere inside it, `None` outside.
    pub fn popup_hit(&self, x: f32, y: f32, fonts: &Fonts) -> Option<Option<usize>> {
        let (r, items) = self.select_popup(fonts)?;
        if !r.contains(x, y) {
            return None;
        }
        let row = (r.h - 2.0) / items.len().max(1) as f32;
        let i = ((y - r.y - 1.0) / row).floor();
        Some((i >= 0.0 && (i as usize) < items.len()).then_some(i as usize))
    }

    /// Natural sizes of images: the bitmap's, or room for the `alt` text
    /// when there is no image to show.
    fn replaced_sizes(&self, fonts: &Fonts) -> Replaced {
        let mut out = Replaced::new();
        for n in self.dom.descendants(Dom::ROOT) {
            if let Some(size) = self.control_size(n, fonts) {
                out.insert(n, size);
                continue;
            }
            if self.dom.tag(n) != Some("img") {
                continue;
            }
            let size = match self.images.get(n) {
                Some(ImageState::Ready(_, b)) => (b.width as f32, b.height as f32),
                Some(ImageState::Loading(_)) => (0.0, 0.0),
                _ => match (self.dom.attr(n, "alt"), self.style(n)) {
                    (Some(alt), Some(st)) if !alt.is_empty() => (
                        fonts.measure(st.font(), st.font_size, alt) + 8.0,
                        st.line_height_px() + 4.0,
                    ),
                    _ => (0.0, 0.0),
                },
            };
            out.insert(n, size);
        }
        out
    }

    /// Content size of a form control drawn by the browser.
    fn control_size(&self, n: NodeId, fonts: &Fonts) -> Option<(f32, f32)> {
        let kind = control(&self.dom, n)?;
        if self.dom.tag(n) == Some("button") {
            return None; // laid out from its children
        }
        let st = self.style(n)?;
        let lh = st.line_height_px();
        let num = |attr: &str, default: f32| {
            self.dom
                .attr(n, attr)
                .and_then(|v| v.trim().parse::<f32>().ok())
                .filter(|v| *v > 0.0)
                .unwrap_or(default)
        };
        Some(match kind {
            Control::Text | Control::Password | Control::Hidden => (0.0, lh),
            Control::TextArea => (
                num("cols", 20.0) * fonts.measure(st.font(), st.font_size, "n"),
                num("rows", 2.0) * lh,
            ),
            Control::Checkbox | Control::Radio => (13.0, 13.0),
            Control::Select => {
                let widest = options(&self.dom, n)
                    .into_iter()
                    .map(|o| fonts.measure(st.font(), st.font_size, &option_label(&self.dom, o)))
                    .fold(0.0, f32::max);
                (widest + 18.0, lh)
            }
            Control::Submit | Control::Reset | Control::Button => (
                fonts.measure(st.font(), st.font_size, &button_label(&self.dom, n, kind)),
                lh,
            ),
        })
    }

    /// A text dump of the boxes under the first element matching
    /// `selector` (for debugging layout: `RUBROWSER_DUMP` in screenshots).
    pub fn dump_layout(&self, selector: &str, max_depth: usize) -> String {
        let Some(target) = self
            .dom
            .descendants(Dom::ROOT)
            .into_iter()
            .find(|&n| crate::css::matches(&self.dom, n, selector, &self.state))
        else {
            return format!("no element matches {selector}\n");
        };
        let mut out = String::new();
        let mut stack = vec![(target, 0usize)];
        while let Some((n, depth)) = stack.pop() {
            if depth > max_depth {
                continue;
            }
            let label = match self.dom.element(n) {
                Some(e) => {
                    let mut l = e.name.clone();
                    if let Some(id) = e.id() {
                        l.push_str(&format!("#{id}"));
                    }
                    for c in e.classes().take(3) {
                        l.push_str(&format!(".{c}"));
                    }
                    l
                }
                None => match self.dom.text(n) {
                    Some(t) if !t.trim().is_empty() => {
                        format!("\"{}\"", t.trim().chars().take(30).collect::<String>())
                    }
                    _ => continue,
                },
            };
            let geo = self
                .box_rects(n)
                .map(|(b, _)| format!("{:.0},{:.0} {:.0}x{:.0}", b.x, b.y, b.w, b.h))
                .unwrap_or_else(|| "-".into());
            let disp = self
                .style(n)
                .map(|s| format!("{:?} w={:?}", s.display, s.width))
                .unwrap_or_else(|| "none".into());
            out.push_str(&format!("{}{label}  [{geo}]  {disp}\n", "  ".repeat(depth)));
            for &c in self.dom.children(n).iter().rev() {
                stack.push((c, depth + 1));
            }
        }
        out
    }

    /// The URL relative links resolve against: `<base href>` or the page.
    pub fn base_url(&self) -> String {
        match self
            .dom
            .find_tag(Dom::ROOT, "base")
            .and_then(|b| self.dom.attr(b, "href"))
        {
            Some(href) => crate::net::url::resolve(&self.url, href),
            None => self.url.clone(),
        }
    }

    /// Images not requested yet: `<img>` elements with a new `src` (marked
    /// loading) and CSS background images. Returns (node, src); background
    /// images have no node.
    pub fn image_requests(&mut self) -> Vec<(Option<NodeId>, String)> {
        let mut out = Vec::new();
        for st in self.styles.iter().flatten() {
            if let Some(url) = &st.background_image
                && !self.images.urls.contains_key(url)
            {
                self.images.set_url(url, ImageState::Loading(url.clone()));
                out.push((None, url.clone()));
            }
        }
        for n in self.dom.descendants(Dom::ROOT) {
            if self.dom.tag(n) != Some("img") {
                continue;
            }
            let src = self.dom.attr(n, "src").unwrap_or("").trim().to_string();
            if self.images.get(n).is_some_and(|s| s.src() == src) {
                continue;
            }
            if src.is_empty() {
                self.images.set(n, ImageState::Failed(src));
                self.needs_layout = true;
            } else {
                self.images.set(n, ImageState::Loading(src.clone()));
                out.push((Some(n), src));
            }
        }
        out
    }

    /// Delivers the bytes (or error) for an image requested with `src`
    /// (for `<img>` `node`, or a background image if `None`).
    pub fn image_loaded(
        &mut self,
        node: Option<NodeId>,
        src: &str,
        result: Result<Vec<u8>, String>,
    ) {
        let state = match result.and_then(|b| Bitmap::decode(&b)) {
            Ok(b) => ImageState::Ready(src.to_string(), std::rc::Rc::new(b)),
            Err(_) => ImageState::Failed(src.to_string()),
        };
        match node {
            Some(n) => {
                if self.images.get(n).is_none_or(|s| s.src() != src) {
                    return; // src changed since the request
                }
                self.images.set(n, state);
                self.needs_layout = true;
            }
            None => self.images.set_url(src, state),
        }
    }

    /// The editing state of text control `node`, initialized from its
    /// `value` (or, for `<textarea>`, its text).
    pub fn input(&mut self, node: NodeId) -> &mut TextField {
        let dom = &self.dom;
        self.inputs.entry(node).or_insert_with(|| {
            if dom.tag(node) == Some("textarea") {
                let mut f = TextField::multiline();
                let text = dom.text_content(node);
                // A newline right after <textarea> is not part of the value.
                f.set_text(text.strip_prefix('\n').unwrap_or(&text));
                f
            } else {
                let mut f = TextField::default();
                f.set_text(dom.attr(node, "value").unwrap_or(""));
                f
            }
        })
    }

    /// The current value of a form control.
    pub fn input_value(&self, node: NodeId) -> String {
        match control(&self.dom, node) {
            Some(Control::Select) => self
                .forms
                .selected_index(&self.dom, node)
                .map(|i| forms::option_value(&self.dom, options(&self.dom, node)[i]))
                .unwrap_or_default(),
            Some(Control::TextArea) if !self.inputs.contains_key(&node) => {
                let text = self.dom.text_content(node);
                text.strip_prefix('\n').unwrap_or(&text).to_string()
            }
            Some(Control::Checkbox | Control::Radio) => {
                self.dom.attr(node, "value").unwrap_or("on").to_string()
            }
            _ => self.inputs.get(&node).map_or_else(
                || self.dom.attr(node, "value").unwrap_or("").to_string(),
                TextField::text,
            ),
        }
    }

    pub fn set_input_value(&mut self, node: NodeId, value: &str) {
        match control(&self.dom, node) {
            Some(Control::Select) => {
                let found = options(&self.dom, node)
                    .into_iter()
                    .position(|o| forms::option_value(&self.dom, o) == value);
                if let Some(i) = found {
                    self.forms.set_selected(node, i);
                }
            }
            Some(k) if k.is_text() => self.input(node).set_text(value),
            _ => {
                if let Some(e) = self.dom_mut().element_mut(node) {
                    e.set_attr("value", value);
                }
            }
        }
        self.needs_layout = true;
    }

    pub fn is_text_input(&self, node: NodeId) -> bool {
        control(&self.dom, node).is_some_and(Control::is_text)
            && self.dom.attr(node, "disabled").is_none()
    }

    /// Whether `node` can take keyboard focus.
    pub fn is_focusable(&self, node: NodeId) -> bool {
        if let Some(kind) = control(&self.dom, node) {
            return kind != Control::Hidden && self.dom.attr(node, "disabled").is_none();
        }
        match self.dom.tag(node) {
            Some("a") => self.dom.attr(node, "href").is_some(),
            Some(_) => self.dom.attr(node, "tabindex").is_some(),
            None => false,
        }
    }

    /// Name/value pairs a form submits (with the button that submitted it).
    pub fn form_data(&self, form: NodeId, submitter: Option<NodeId>) -> Vec<(String, String)> {
        let mut out = Vec::new();
        for n in self.dom.descendants(form) {
            let (Some(kind), Some(name)) = (control(&self.dom, n), self.dom.attr(n, "name")) else {
                continue;
            };
            if name.is_empty() || self.dom.attr(n, "disabled").is_some() {
                continue;
            }
            let include = match kind {
                Control::Checkbox | Control::Radio => self.forms.checked(&self.dom, n),
                Control::Submit | Control::Reset | Control::Button => {
                    Some(n) == submitter && kind == Control::Submit
                }
                _ => true,
            };
            if include {
                out.push((name.to_string(), self.input_value(n)));
            }
        }
        out
    }

    /// Restores the controls of `form` to their initial values.
    pub fn reset_form(&mut self, form: NodeId) {
        self.forms.reset(&self.dom, form);
        for n in self.dom.descendants(form) {
            self.inputs.remove(&n);
        }
        self.needs_layout = true;
    }

    /// The nearest focusable element at or above `node`.
    pub fn focusable_ancestor(&self, node: NodeId) -> Option<NodeId> {
        let mut n = Some(node);
        while let Some(id) = n {
            if self.is_focusable(id) {
                return Some(id);
            }
            n = self.dom.parent(id);
        }
        None
    }

    /// Next (or previous) rendered focusable element after `from`.
    pub fn next_focusable(&self, from: Option<NodeId>, backwards: bool) -> Option<NodeId> {
        let mut all: Vec<NodeId> = self
            .dom
            .descendants(Dom::ROOT)
            .into_iter()
            .filter(|&n| self.is_focusable(n) && self.style(n).is_some())
            .collect();
        if backwards {
            all.reverse();
        }
        match from.and_then(|f| all.iter().position(|&n| n == f)) {
            Some(i) => all.get(i + 1).or(all.first()).copied(),
            None => all.first().copied(),
        }
    }

    /// Absolute content box of element `node` (viewport coordinates,
    /// scrolling applied), if it is laid out.
    pub fn content_box(&self, node: NodeId) -> Option<RectF> {
        self.box_rects(node).map(|(_, content)| content)
    }

    /// Absolute border box and content box of element `node`.
    pub fn box_rects(&self, node: NodeId) -> Option<(RectF, RectF)> {
        fn walk(
            d: &Document,
            i: usize,
            ox: f32,
            oy: f32,
            target: NodeId,
        ) -> Option<(RectF, RectF)> {
            let b = &d.tree.boxes[i];
            let r = RectF {
                x: ox + b.rect.x,
                y: oy + b.rect.y,
                ..b.rect
            };
            let (cx, cy) = b.content_offset();
            if b.node == Some(target) {
                let content = RectF {
                    x: r.x + cx,
                    y: r.y + cy,
                    w: (r.w - cx - b.border[1] - b.padding[1]).max(0.0),
                    h: (r.h - cy - b.border[2] - b.padding[2]).max(0.0),
                };
                return Some((r, content));
            }
            let clips = b.node.is_some()
                && d.style(b.style_node)
                    .is_some_and(|s| s.overflow != crate::css::style::Overflow::Visible);
            let sy = if clips { d.scroll_of(b.node) } else { 0.0 };
            if let Some(il) = &b.inline {
                for line in &il.lines {
                    for item in &line.items {
                        if let Placed::Atomic { index, x, y, .. } = item
                            && let Some(found) = walk(
                                d,
                                *index,
                                r.x + cx + x,
                                r.y + cy - sy + line.top + y,
                                target,
                            )
                        {
                            return Some(found);
                        }
                    }
                }
            }
            b.children
                .iter()
                .find_map(|&c| walk(d, c, r.x, r.y - sy, target))
        }
        if self.tree.boxes.is_empty() {
            return None;
        }
        walk(self, LayoutTree::VIEWPORT, 0.0, -self.scroll_of(None), node)
    }

    /// Caret positions of an input's shown text, and its content box.
    fn input_geometry(&self, node: NodeId, fonts: &Fonts) -> Option<(Vec<f32>, RectF)> {
        let st = self.style(node)?;
        let shown = crate::paint::display_value(&self.dom, node, &self.input_value(node));
        let xs = TextField::positions(&fonts.advances(st.font(), st.font_size, &shown));
        Some((xs, self.content_box(node)?))
    }

    /// Places the caret of text control `node` at viewport point `(x, y)`
    /// (mouse press or drag).
    pub fn input_pointer(
        &mut self,
        node: NodeId,
        x: f32,
        y: f32,
        drag: bool,
        extend: bool,
        fonts: &Fonts,
    ) {
        if self.dom.tag(node) == Some("textarea") {
            let (Some(st), Some(area)) = (self.style(node).cloned(), self.content_box(node)) else {
                return;
            };
            let lh = st.line_height_px();
            let f = self.input(node);
            let line = ((y - area.y + f.scroll_y()) / lh).floor().max(0.0) as usize;
            let lines = f.lines();
            let (s, e) = lines[line.min(lines.len() - 1)];
            let text: String = f.text().chars().skip(s).take(e - s).collect();
            let xs = TextField::positions(&fonts.advances(st.font(), st.font_size, &text));
            f.click_line(line, x - area.x, &xs, drag || extend);
            return;
        }
        let Some((xs, area)) = self.input_geometry(node, fonts) else {
            return;
        };
        let f = self.input(node);
        if drag {
            f.drag(x - area.x, &xs);
        } else {
            f.click(x - area.x, &xs, extend);
        }
    }

    /// Scrolls every text control so its caret is visible.
    pub fn scroll_inputs(&mut self, fonts: &Fonts) {
        let nodes: Vec<NodeId> = self.inputs.keys().copied().collect();
        for n in nodes {
            if self.dom.tag(n) == Some("textarea") {
                if let (Some(st), Some(area)) = (self.style(n).cloned(), self.content_box(n)) {
                    // Lines are clipped, not scrolled sideways.
                    self.input(n).scroll_to_caret_y(area.h, st.line_height_px());
                }
                continue;
            }
            if let Some((xs, area)) = self.input_geometry(n, fonts) {
                self.input(n).scroll_to_caret(area.w, &xs);
            }
        }
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
        // Topmost first: fixed boxes by z-index, then the page.
        let mut kids = vp.children.clone();
        kids.sort_by_key(|&k| (self.tree.boxes[k].fixed, self.tree.boxes[k].z));
        for c in kids.into_iter().rev() {
            let dy = if self.tree.boxes[c].fixed { 0.0 } else { sy };
            if self.hit_box(c, x, y, 0.0, -dy, &mut hit) {
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
        // Children on top first: positioned ones by z-index, then in-flow
        // ones in reverse.
        let mut positioned: Vec<usize> = b
            .children
            .iter()
            .copied()
            .filter(|&c| self.tree.boxes[c].absolute)
            .collect();
        positioned.sort_by_key(|&c| std::cmp::Reverse(self.tree.boxes[c].z));
        let in_flow = b
            .children
            .iter()
            .rev()
            .copied()
            .filter(|&c| !self.tree.boxes[c].absolute);
        for c in positioned.into_iter().chain(in_flow) {
            if self.hit_box(c, x, y, r.x, r.y - sy, hit) {
                return true;
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
                            ..
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

    /// Vertical scroll offset of the page.
    pub fn scroll_y(&self) -> f32 {
        self.scroll_of(None)
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

/// The label of an `<input>` push button.
pub fn button_label(dom: &Dom, node: NodeId, kind: Control) -> String {
    dom.attr(node, "value")
        .map(str::to_string)
        .unwrap_or_else(|| {
            match kind {
                Control::Submit => "Submit",
                Control::Reset => "Reset",
                _ => "",
            }
            .to_string()
        })
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
        assert!(c.pixels().contains(&0));
    }
}
