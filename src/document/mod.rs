//! A live document: DOM + styles + layout, with hover, scrolling and
//! hit-testing. Used for both web pages and the browser's own chrome.

use crate::css::Restyle;
use crate::css::media::{MediaEnv, MediaList};
use crate::css::selector::DynScope;
use crate::css::selector::PseudoElement;
use crate::css::style::Cursor as CssCursor;
use crate::css::syntax::{Rule, Sheet, parse_stylesheet};
use crate::css::values::Dim;
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
    /// `@font-face` sources already requested.
    requested_fonts: std::collections::HashSet<String>,
    /// The cascade index for the current sheets and viewport.
    cascade: Option<(Vec<*const Sheet>, MediaEnv, Rc<Cascade>)>,
    styles: Vec<Option<Style>>,
    /// `::before` / `::after` / `::marker` styles.
    pub pseudo: HashMap<(NodeId, PseudoElement), Style>,
    /// `::before` / `::after` boxes (see [`crate::css::Styles::generated`]).
    pub generated: Vec<crate::css::Generated>,
    tree: LayoutTree,
    state: ElementState,
    scroll: ScrollOffsets,
    /// Fragment (`#id`) to keep scrolled into view while the page loads,
    /// until the user scrolls.
    fragment: Option<String>,
    size: (f32, f32),
    needs_style: bool,
    /// After element-state changes only: the subtrees to restyle.
    restyle_roots: Vec<NodeId>,
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
            requested_fonts: Default::default(),
            cascade: None,
            styles: Vec::new(),
            pseudo: HashMap::new(),
            generated: Vec::new(),
            tree: LayoutTree::default(),
            state: ElementState::default(),
            scroll: ScrollOffsets::new(),
            fragment: url
                .split_once('#')
                .map(|(_, f)| f.to_string())
                .filter(|f| !f.is_empty()),
            size: (0.0, 0.0),
            needs_style: true,
            restyle_roots: Vec::new(),
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
        self.needs_style || self.needs_layout || !self.restyle_roots.is_empty()
    }

    /// The viewport size in logical pixels.
    pub fn viewport_size(&self) -> (f32, f32) {
        self.size
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

    /// `<link>` elements whose sheet to load, `rel=stylesheet` or
    /// `rel=preload as=style` (which pages turn into stylesheets when it
    /// loads): (node, href).
    pub fn stylesheet_links(&self) -> Vec<(NodeId, String)> {
        self.dom
            .descendants(Dom::ROOT)
            .into_iter()
            .filter(|&n| self.loads_sheet(n))
            .filter_map(|n| Some((n, self.dom.attr(n, "href")?.to_string())))
            .collect()
    }

    /// Whether `n` is a `<link>` whose stylesheet to load.
    pub fn loads_sheet(&self, n: NodeId) -> bool {
        self.dom.tag(n) == Some("link")
            && (self.link_rel(n, "stylesheet")
                || (self.link_rel(n, "preload")
                    && self
                        .dom
                        .attr(n, "as")
                        .is_some_and(|a| a.eq_ignore_ascii_case("style"))))
    }

    fn link_rel(&self, n: NodeId, rel: &str) -> bool {
        self.dom.attr(n, "rel").is_some_and(|r| {
            r.split_whitespace().any(|w| w.eq_ignore_ascii_case(rel))
        })
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

    /// `@font-face` faces not requested yet that this page can use.
    pub fn font_requests(&mut self) -> Vec<FontRequest> {
        let env = MediaEnv {
            width: self.size.0,
            height: self.size.1,
            scale: 1.0,
            dark: false,
        };
        fn walk(rules: &[Rule], env: &MediaEnv, out: &mut Vec<FontRequest>) {
            for r in rules {
                match r {
                    Rule::FontFace(decls) => out.extend(FontRequest::from_decls(decls)),
                    Rule::Media(m, inner) if m.matches(env) => walk(inner, env, out),
                    Rule::Layer(_, inner) => walk(inner, env, out),
                    _ => {}
                }
            }
        }
        let mut all = Vec::new();
        for (sheet, media) in self.author_sheets() {
            if media.matches(&env) {
                walk(&sheet.rules, &env, &mut all);
            }
        }
        all.into_iter()
            .filter(|f| {
                self.requested_fonts
                    .insert(format!("{}|{}", f.family, f.url))
            })
            .collect()
    }

    /// A web font loaded (or failed): styles may now resolve to it.
    pub fn fonts_changed(&mut self) {
        self.needs_style = true;
        // A new face of a known family changes text widths, not styles.
        self.needs_layout = true;
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
                    Some(sheet)
                        if self.dom.attr(n, "disabled").is_none()
                            && self.link_rel(n, "stylesheet") =>
                    {
                        sheet.clone()
                    }
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
        let t0 = profiling().then(std::time::Instant::now);
        self.update_style();
        if self.needs_layout {
            let replaced = self.replaced_sizes(fonts);
            self.tree = layout(
                &self.dom,
                &self.styles,
                &self.generated,
                fonts,
                self.size.0,
                self.size.1,
                &replaced,
            );
            self.needs_layout = false;
            self.clamp_scroll();
            self.apply_fragment();
            if let Some(t) = t0 {
                eprintln!(
                    "[profile] style+layout {:.1} ms",
                    t.elapsed().as_secs_f64() * 1000.0
                );
            }
        }
    }

    /// The value of CSS property `prop` for element `n`, as
    /// `getComputedStyle` reports it (styles are brought up to date; sizes
    /// come from the last layout).
    pub fn computed_value(&mut self, n: NodeId, prop: &str) -> Option<String> {
        self.update_style();
        self.dom.element(n)?;
        let unrendered;
        let style = match self.style(n) {
            Some(s) => s,
            None => {
                unrendered = self.unrendered_style(n)?;
                &unrendered
            }
        };
        let content = self.box_rects(n).map(|(_, c)| (c.w, c.h));
        let base = self
            .dom
            .parent(n)
            .and_then(|p| self.box_rects(p))
            .map_or(self.size.0, |(_, c)| c.w);
        let used = crate::css::computed::Used { content, base };
        Some(crate::css::computed::value(style, prop, used))
    }

    /// The style element `n` would have although it is not rendered.
    fn unrendered_style(&self, n: NodeId) -> Option<Style> {
        let (_, env, cascade) = self.cascade.as_ref()?;
        let mut chain = vec![n];
        let mut at = n;
        let parent = loop {
            match self.dom.parent(at) {
                Some(p) => match self.style(p) {
                    Some(s) => break s.clone(),
                    None => {
                        chain.push(p);
                        at = p;
                    }
                },
                None => break Style::default(),
            }
        };
        chain.reverse();
        let root_font = self
            .dom
            .html()
            .and_then(|h| self.style(h))
            .map_or(16.0, |s| s.font_size);
        let venv = crate::css::ValueEnv {
            root_font,
            vw: env.width,
            vh: env.height,
        };
        let forms = &self.forms;
        let dom = &self.dom;
        let checked = |n: NodeId| forms.checked(dom, n);
        let cx = Matcher {
            dom,
            state: self.state,
            checked: &checked,
        };
        Some(crate::css::unrendered_style(cascade, &cx, &venv, &parent, &chain))
    }

    /// Whether media query list `query` matches the viewport (`matchMedia`).
    pub fn media_matches(&self, query: &str) -> bool {
        let env = MediaEnv {
            width: self.size.0,
            height: self.size.1,
            scale: 1.0,
            dark: false,
        };
        MediaList::parse(query).matches(&env)
    }

    /// Recomputes styles if needed (not layout).
    pub fn update_style(&mut self) {
        let t0 = profiling().then(std::time::Instant::now);
        if self.needs_style || !self.restyle_roots.is_empty() {
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
                    let c = Rc::new(Cascade::with_quirks(&sheets, &env, self.dom.quirks));
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
            let roots = std::mem::take(&mut self.restyle_roots);
            let mut done = !self.needs_style;
            if done && !roots.is_empty() {
                let mut st = crate::css::Styles {
                    nodes: std::mem::take(&mut self.styles),
                    pseudo: std::mem::take(&mut self.pseudo),
                    generated: std::mem::take(&mut self.generated),
                };
                for &root in &roots {
                    match crate::css::restyle_subtree(dom, &cascade, &cx, &env, root, &mut st) {
                        Restyle::Changed => self.needs_layout = true,
                        Restyle::Unchanged => {}
                        Restyle::NeedsFull => {
                            if profiling() {
                                eprintln!("[profile] generated content changed: full restyle");
                            }
                            done = false;
                            break;
                        }
                    }
                }
                self.styles = st.nodes;
                self.pseudo = st.pseudo;
                self.generated = st.generated;
            }
            if !done {
                let styles = compute(dom, &cascade, &cx, &env);
                if styles.nodes != self.styles
                    || styles.pseudo != self.pseudo
                    || styles.generated != self.generated
                {
                    self.styles = styles.nodes;
                    self.pseudo = styles.pseudo;
                    self.generated = styles.generated;
                    self.needs_layout = true;
                }
            }
            self.needs_style = false;
            self.sync_inline_svgs();
            if let Some(t) = t0 {
                eprintln!(
                    "[profile] style {:.1} ms",
                    t.elapsed().as_secs_f64() * 1000.0
                );
            }
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
            view_top: Default::default(),
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
    /// Parses each rendered outermost `<svg>` whose markup changed.
    fn sync_inline_svgs(&mut self) {
        for n in self.dom.descendants(Dom::ROOT) {
            if self.dom.tag(n) != Some("svg") {
                continue;
            }
            let Some(color) = self.style(n).map(|s| s.color) else {
                continue;
            };
            let mut up = self.dom.parent(n);
            let mut nested = false;
            while let Some(p) = up {
                nested |= self.dom.tag(p) == Some("svg");
                up = self.dom.parent(p);
            }
            if nested {
                continue; // drawn as part of the outer <svg>
            }
            let markup = image::svg_markup(&self.dom, &self.styles, n, color);
            if self.images.get(n).is_some_and(|s| s.src() == markup) {
                continue;
            }
            let state = match Bitmap::svg(markup.as_bytes()) {
                Ok(b) => ImageState::Ready(markup, std::rc::Rc::new(b)),
                Err(_) => ImageState::Failed(markup),
            };
            self.images.set(n, state);
            self.needs_layout = true;
        }
    }

    fn replaced_sizes(&self, fonts: &Fonts) -> Replaced {
        let mut out = Replaced::new();
        for n in self.dom.descendants(Dom::ROOT) {
            if let Some(size) = self.control_size(n, fonts) {
                out.insert(n, size);
                continue;
            }
            if !matches!(self.dom.tag(n), Some("img" | "svg")) {
                continue;
            }
            let size = match self.images.get(n) {
                Some(ImageState::Ready(_, b)) => {
                    let d = self.images.density.get(&n).copied().unwrap_or(1.0);
                    (b.width as f32 / d, b.height as f32 / d)
                }
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
            // About 20 characters, like other browsers' default.
            Control::Text | Control::Password | Control::Hidden => {
                (20.0 * fonts.measure(st.font(), st.font_size, "n"), lh)
            }
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
                .map(|s| {
                    format!(
                        "{:?} {:?} w={:?} m={:?} p={:?} vis={} op={} color=#{:06x}{}",
                        s.display,
                        s.position,
                        s.width,
                        s.margin,
                        s.padding,
                        s.visible,
                        s.opacity,
                        s.color.rgb(),
                        if s.clipped_away { " clipped" } else { "" }
                    )
                })
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
            for url in [&st.background_image, &st.mask.image].into_iter().flatten() {
                if !self.images.urls.contains_key(url) {
                    self.images.set_url(url, ImageState::Loading(url.clone()));
                    out.push((None, url.clone()));
                }
            }
        }
        let env = MediaEnv {
            width: self.size.0,
            height: self.size.1,
            scale: 1.0,
            dark: false,
        };
        for n in self.dom.descendants(Dom::ROOT) {
            if self.dom.tag(n) != Some("img") {
                continue;
            }
            let (src, density) = image::current_source(&self.dom, n, &env);
            if self.images.get(n).is_some_and(|s| s.src() == src) {
                continue;
            }
            self.images.density.insert(n, density);
            if profiling() {
                eprintln!(
                    "[profile] img {src} (density {density}, viewport {:?})",
                    self.size
                );
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
        let result = result.and_then(|b| Bitmap::decode(&b));
        if profiling() {
            eprintln!(
                "[profile] image {src}: {:?}",
                result.as_ref().map(|b| (b.width, b.height))
            );
        }
        let state = match result {
            Ok(b) => ImageState::Ready(src.to_string(), std::rc::Rc::new(b)),
            Err(_) => ImageState::Failed(src.to_string()),
        };
        match node {
            Some(n) => {
                if self.images.get(n).is_none_or(|s| s.src() != src) {
                    return; // src changed since the request
                }
                self.images.set(n, state);
                // An image whose box CSS sizes completely does not move
                // anything when it arrives: repaint only.
                let sized = self.style(n).is_some_and(|s| {
                    s.width != Dim::Auto && (s.height != Dim::Auto || s.aspect_ratio.is_some())
                });
                if !sized {
                    self.needs_layout = true;
                }
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
        let old = std::mem::replace(&mut self.state, state);
        let Some((_, _, cascade)) = self.cascade.clone().filter(|_| !self.needs_style) else {
            if profiling() {
                eprintln!("[profile] full restyle: no cascade yet or style pending");
            }
            self.needs_style = true;
            return true;
        };
        // Elements whose state changed: the symmetric difference of the
        // old and new ancestor chains.
        let chain = |n: Option<NodeId>| {
            let mut v = Vec::new();
            let mut cur = n;
            while let Some(c) = cur {
                v.push(c);
                cur = self.dom.parent(c);
            }
            v
        };
        use crate::css::selector::{ACTIVE, FOCUS, HOVER};
        let mut changed: Vec<(NodeId, u8)> = Vec::new();
        for (a, b, bit) in [
            (old.hover, state.hover, HOVER),
            (old.active, state.active, ACTIVE),
            (old.focus, state.focus, FOCUS),
        ] {
            let (ca, cb) = (chain(a), chain(b));
            changed.extend(ca.iter().filter(|n| !cb.contains(n)).map(|&n| (n, bit)));
            changed.extend(cb.iter().filter(|n| !ca.contains(n)).map(|&n| (n, bit)));
        }
        // Only where a state-dependent compound matches does anything
        // restyle (the state conditions themselves are left out).
        let forms = &self.forms;
        let dom = &self.dom;
        let checked = |n: NodeId| forms.checked(dom, n);
        let cx = Matcher {
            dom,
            state,
            checked: &checked,
        };
        let mut roots = Vec::new();
        for &(n, bit) in &changed {
            if self.dom.element(n).is_none() {
                continue;
            }
            for part in cascade.dynamic.iter().filter(|p| p.bits & bit != 0) {
                if !crate::css::selector::matches(&cx, &part.context, n) {
                    continue;
                }
                let within = match part.scope {
                    DynScope::Subtree => n,
                    DynScope::Parent => self.dom.parent(n).unwrap_or(n),
                };
                match &part.subject {
                    // The changed element is the subject: it (and what
                    // inherits from it) restyles.
                    None => roots.push(n),
                    // Find the subjects it can affect.
                    Some(subject) => {
                        roots.extend(self.dom.descendants(within).into_iter().filter(|&d| {
                            self.dom.element(d).is_some()
                                && crate::css::selector::matches(&cx, subject, d)
                        }))
                    }
                }
            }
        }
        roots.sort_unstable();
        roots.dedup();
        if profiling() {
            let sizes: Vec<usize> = roots
                .iter()
                .map(|&r| self.dom.descendants(r).len())
                .collect();
            eprintln!(
                "[profile] {} restyle roots, sizes {:?}",
                roots.len(),
                &sizes[..sizes.len().min(12)]
            );
        }
        self.restyle_roots.extend(roots);
        true
    }

    /// What is at logical point `(x, y)` (relative to the viewport).
    /// Maps a generated (`::before` / `::after`) node to its element.
    fn real_node(&self, id: NodeId) -> Option<NodeId> {
        match id.checked_sub(self.dom.len()) {
            Some(i) => self.generated.get(i / 2).map(|g| g.owner),
            None => Some(id),
        }
    }

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
        let style = self.style(b.style_node);
        let (tx, ty) = style.map_or((0.0, 0.0), |s| b.translation(s));
        let r = RectF {
            x: ox + b.rect.x + tx,
            y: oy + b.rect.y + ty,
            ..b.rect
        };
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
            let cb = &self.tree.boxes[c];
            let bottom = r.y + r.h - b.padding[2] - b.border[2];
            let shift = cb.sticky_shift(r.y - sy + cb.rect.y, 0.0, bottom);
            if self.hit_box(c, x, y, r.x, r.y - sy + shift, hit) {
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
                                hit.element = self.real_node(*node).and_then(|n| {
                                    if n == *node {
                                        self.dom.parent(n)
                                    } else {
                                        Some(n)
                                    }
                                });
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
            hit.element = self.real_node(b.node.unwrap_or(b.style_node));
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

    /// Scrolls the page to the element named by `fragment` (`id`, or
    /// `<a name>`), or to the top for an empty fragment / `top`.
    pub fn scroll_to_fragment(&mut self, fragment: &str) {
        self.fragment = Some(fragment.to_string());
        self.apply_fragment();
    }

    fn apply_fragment(&mut self) {
        let Some(f) = self.fragment.clone() else {
            return;
        };
        let f = String::from_utf8_lossy(&crate::net::url::percent_decode(&f)).into_owned();
        if f.is_empty() || f.eq_ignore_ascii_case("top") {
            self.scroll.insert(None, 0.0);
            return;
        }
        let target = self.dom.element_by_id(&f).or_else(|| {
            self.dom
                .descendants(Dom::ROOT)
                .into_iter()
                .find(|&n| self.dom.tag(n) == Some("a") && self.dom.attr(n, "name") == Some(&f))
        });
        // Inline elements have no box: use the nearest ancestor's.
        let mut node = target;
        let rect = loop {
            let Some(n) = node else { return };
            if let Some((r, _)) = self.box_rects(n) {
                break r;
            }
            node = self.dom.parent(n);
        };
        let r = rect;
        let y = r.y + self.scroll_of(None);
        let y = y.clamp(0.0, self.max_scroll(None));
        self.scroll.insert(None, y);
    }

    /// Scrolls the innermost scroller under `(x, y)` that can move by `dy`.
    /// Returns true if something scrolled.
    pub fn scroll_by(&mut self, x: f32, y: f32, dy: f32) -> bool {
        self.fragment = None;
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

/// One `@font-face` to download.
#[derive(Clone, Debug, PartialEq)]
pub struct FontRequest {
    pub family: String,
    pub url: String,
    /// `font-weight` (the low end of a range).
    pub weight: u16,
    pub italic: bool,
}

impl FontRequest {
    /// The face described by `@font-face` descriptors, if it has a usable
    /// source and covers Latin text (subsets for other scripts are skipped).
    fn from_decls(decls: &[crate::css::syntax::Decl]) -> Option<FontRequest> {
        let get = |name: &str| {
            decls
                .iter()
                .rev()
                .find(|d| d.name == name)
                .map(|d| d.value.trim().to_string())
        };
        let family = get("font-family")?.trim_matches(['"', '\'']).to_string();
        if let Some(range) = get("unicode-range") {
            let covers_latin = range.split(',').any(|r| {
                let r = r
                    .trim()
                    .trim_start_matches(['U', 'u'])
                    .trim_start_matches('+');
                let (lo, hi) = match r.split_once('-') {
                    Some((a, b)) => (a.to_string(), b.to_string()),
                    None if r.contains('?') => (r.replace('?', "0"), r.replace('?', "F")),
                    None => (r.to_string(), r.to_string()),
                };
                let parse = |h: &str| u32::from_str_radix(h, 16).unwrap_or(0);
                (parse(&lo)..=parse(&hi)).contains(&0x61)
            });
            if !covers_latin {
                return None;
            }
        }
        // The first source in a supported format.
        let url = crate::css::gradient::split_commas(&get("src")?)
            .into_iter()
            .find_map(|src| {
                let lower = src.to_ascii_lowercase();
                let url_start = lower.find("url(")?;
                let rest = &src[url_start + 4..];
                let url = rest[..rest.find(')')?].trim().trim_matches(['"', '\'']);
                let ok = match lower.find("format(") {
                    Some(i) => ["woff2", "woff", "truetype", "opentype"]
                        .iter()
                        .any(|f| lower[i..].contains(f) && !lower[i..].contains("embedded")),
                    None => true,
                };
                ok.then(|| url.to_string())
            })?;
        let weight = get("font-weight").unwrap_or_default();
        let weight = match weight.split_whitespace().next() {
            Some("bold" | "bolder") => 700,
            Some(n) => n
                .parse::<f32>()
                .map_or(400, |w| w.clamp(1.0, 1000.0) as u16),
            None => 400,
        };
        let italic =
            get("font-style").is_some_and(|s| s.contains("italic") || s.contains("oblique"));
        Some(FontRequest {
            family,
            url,
            weight,
            italic,
        })
    }
}

/// Whether `RUBROWSER_PROFILE` is set (prints style/layout timings).
fn profiling() -> bool {
    #[cfg(not(target_arch = "wasm32"))]
    {
        static ON: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
        *ON.get_or_init(|| std::env::var_os("RUBROWSER_PROFILE").is_some())
    }
    #[cfg(target_arch = "wasm32")]
    false
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
    fn font_face_requests() {
        let html = r#"<style>
            @font-face { font-family: "Inter"; src: local(Inter), url(inter.eot) format("embedded-opentype"), url(inter.woff2) format("woff2"); font-weight: 600 }
            @font-face { font-family: Inter; src: url(cyr.woff2); unicode-range: U+0400-045F }
            @font-face { font-family: Icons; font-style: italic; src: url(i.ttf) }
            </style>"#;
        let (mut d, _) = doc(html, 400.0, 300.0);
        let reqs = d.font_requests();
        assert_eq!(reqs.len(), 2, "{reqs:?}");
        assert_eq!(
            (
                reqs[0].family.as_str(),
                reqs[0].url.as_str(),
                reqs[0].weight
            ),
            ("Inter", "inter.woff2", 600)
        );
        assert!(reqs[1].italic);
        assert!(d.font_requests().is_empty(), "requested once");
    }

    #[test]
    fn partial_restyle_matches_full_restyle() {
        let html = r#"<style>
            .a:hover .b { color: red }
            .a:hover + .c { color: blue }
            .a:hover::after { content: "!" }
            .d:hover { padding: 3px; color: green }
            :hover > .f { margin: 1px }
            :focus { outline: 1px solid }
            </style><div><p class=a>x <span class=b>y</span></p><p class=c>z</p>
            <p class=d>w <i>inherits</i> <span class=f>f</span></p></div>"#;
        let (mut d, fonts) = doc(html, 400.0, 300.0);
        let a = d
            .dom
            .descendants(Dom::ROOT)
            .into_iter()
            .find(|&n| d.dom.attr(n, "class") == Some("a"))
            .unwrap();
        let dd = d
            .dom
            .descendants(Dom::ROOT)
            .into_iter()
            .find(|&n| d.dom.attr(n, "class") == Some("d"))
            .unwrap();
        for hover in [Some(a), Some(dd), None, Some(a)] {
            d.set_state(ElementState {
                hover,
                ..Default::default()
            });
            d.update(&fonts);
            let partial = (d.styles.clone(), d.generated.clone());
            d.needs_style = true;
            d.update(&fonts);
            assert_eq!(
                partial,
                (d.styles.clone(), d.generated.clone()),
                "hover {hover:?}"
            );
        }
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
