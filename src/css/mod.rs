//! CSS: stylesheets, selector matching, cascade and computed styles.
//!
//! Stylesheets are parsed once ([`syntax`]) and indexed by the key of each
//! selector's subject (id, class, tag) so an element only tests the rules
//! that could match it. Media queries are evaluated against the viewport
//! when the index is built. Custom properties are inherited and `var()` is
//! substituted when a declaration is applied.

pub mod media;
pub mod selector;
pub mod style;
pub mod syntax;
pub mod values;

use std::collections::{BTreeMap, HashMap};
use std::rc::Rc;

use crate::dom::{Dom, NodeId};
use media::{MediaEnv, MediaList};
use selector::{Context, Key, PseudoElement, Selector};
pub use style::Style;
use syntax::{Decl, Rule, Sheet};

/// The browser's default stylesheet.
pub const UA_CSS: &str = include_str!("ua.css");

thread_local! {
    static UA_SHEET: Rc<Sheet> = Rc::new(syntax::parse_stylesheet(UA_CSS, None));
}

/// The parsed UA stylesheet.
pub fn ua_sheet() -> Rc<Sheet> {
    UA_SHEET.with(Rc::clone)
}

/// Dynamic element state that pseudo-classes depend on.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct ElementState {
    pub hover: Option<NodeId>,
    pub active: Option<NodeId>,
    pub focus: Option<NodeId>,
}

/// Matching context: the DOM, element state and checked controls.
pub struct Matcher<'a> {
    pub dom: &'a Dom,
    pub state: ElementState,
    pub checked: &'a dyn Fn(NodeId) -> bool,
}

impl Context for Matcher<'_> {
    fn dom(&self) -> &Dom {
        self.dom
    }
    fn is_hovered(&self, n: NodeId) -> bool {
        self.state
            .hover
            .is_some_and(|h| self.dom.is_inclusive_ancestor(n, h))
    }
    fn is_active(&self, n: NodeId) -> bool {
        self.state
            .active
            .is_some_and(|h| self.dom.is_inclusive_ancestor(n, h))
    }
    fn is_focused(&self, n: NodeId) -> bool {
        self.state.focus == Some(n)
    }
    fn is_checked(&self, n: NodeId) -> bool {
        (self.checked)(n)
    }
}

/// Whether `selector` matches element `id`. Used by scripts' `query`.
pub fn matches(dom: &Dom, id: NodeId, selector: &str, state: &ElementState) -> bool {
    let no = |_: NodeId| false;
    let cx = Matcher {
        dom,
        state: *state,
        checked: &no,
    };
    dom.element(id).is_some()
        && selector::parse_list(selector).is_some_and(|list| {
            list.iter()
                .any(|s| s.pseudo_element.is_none() && selector::matches(&cx, s, id))
        })
}

/// One rule of the index: a single selector with its declarations.
struct Indexed {
    selector: Selector,
    decls: Rc<[Decl]>,
    /// 0 = user agent, 2 = author (1 and 3 are hints and inline styles).
    origin: u8,
    order: u32,
}

/// Every rule that applies at one viewport, filed by selector key.
pub struct Cascade {
    rules: Vec<Indexed>,
    buckets: HashMap<Key, Vec<u32>>,
    /// Whether any rule depends on :hover/:active/:focus.
    pub dynamic: bool,
}

impl Cascade {
    /// Indexes the UA sheet and `author` sheets (in order, with the media
    /// list they were included with) for `env`.
    pub fn new(author: &[(Rc<Sheet>, MediaList)], env: &MediaEnv) -> Cascade {
        let mut c = Cascade {
            rules: Vec::new(),
            buckets: HashMap::new(),
            dynamic: false,
        };
        let ua = ua_sheet();
        c.add(&ua.rules, 0, env);
        for (sheet, media) in author {
            if media.matches(env) {
                c.add(&sheet.rules, 2, env);
            }
        }
        c
    }

    fn add(&mut self, rules: &[Rule], origin: u8, env: &MediaEnv) {
        for r in rules {
            match r {
                Rule::Style { selectors, decls } => {
                    for sel in selectors.iter() {
                        let order = self.rules.len() as u32;
                        self.buckets.entry(sel.key()).or_default().push(order);
                        self.rules.push(Indexed {
                            selector: sel.clone(),
                            decls: decls.clone(),
                            origin,
                            order,
                        });
                    }
                }
                Rule::Media(list, inner) => {
                    if list.matches(env) {
                        self.add(inner, origin, env);
                    }
                }
                Rule::Import(..) => {}
            }
        }
    }

    /// Indices of rules whose subject key fits element `n`.
    fn candidates(&self, dom: &Dom, n: NodeId, out: &mut Vec<u32>) {
        out.clear();
        let Some(e) = dom.element(n) else { return };
        let mut push = |k: Key| {
            if let Some(v) = self.buckets.get(&k) {
                out.extend_from_slice(v);
            }
        };
        if let Some(id) = e.id() {
            push(Key::Id(id.to_string()));
        }
        for c in e.classes() {
            push(Key::Class(c.to_string()));
        }
        push(Key::Tag(e.name.clone()));
        push(Key::Any);
    }
}

/// Computed styles of a document.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Styles {
    /// One entry per node: elements and text (their parent's style);
    /// `None` when not rendered.
    pub nodes: Vec<Option<Style>>,
    /// `::before` / `::after` / `::marker` styles of elements that have them.
    pub pseudo: HashMap<(NodeId, PseudoElement), Style>,
}

/// One declaration in cascade order.
struct Matched<'a> {
    rank: (u8, [u16; 3], u32),
    name: &'a str,
    value: &'a str,
}

/// Cascade rank: normal declarations by origin; `!important` ones above
/// all normal ones, with the user agent's on top.
fn rank(important: bool, origin: u8, specificity: [u16; 3], order: u32) -> (u8, [u16; 3], u32) {
    let level = if important {
        10 + match origin {
            0 => 3,
            3 => 2,
            _ => 1,
        }
    } else {
        origin
    };
    (level, specificity, order)
}

/// Inputs that `em`, `rem`, `vw` and `vh` depend on.
#[derive(Clone, Copy, Debug)]
pub struct ValueEnv {
    pub root_font: f32,
    pub vw: f32,
    pub vh: f32,
}

/// Computes the style of every node.
pub fn compute(dom: &Dom, cascade: &Cascade, cx: &Matcher, env: &MediaEnv) -> Styles {
    let mut out = Styles {
        nodes: vec![None; dom.len()],
        pseudo: HashMap::new(),
    };
    let mut venv = ValueEnv {
        root_font: 16.0,
        vw: env.width,
        vh: env.height,
    };
    let mut candidates = Vec::new();
    let root_style = Style::default();
    // Document order, parents before children.
    let mut stack: Vec<(NodeId, Rc<Style>)> = vec![(Dom::ROOT, Rc::new(root_style))];
    while let Some((id, parent)) = stack.pop() {
        let mut style = if dom.element(id).is_some() {
            cascade.candidates(dom, id, &mut candidates);
            let s = cascade_element(dom, id, cascade, &candidates, cx, &parent, &venv, None);
            if dom.tag(id) == Some("html") {
                venv.root_font = s.font_size;
            }
            for pe in [
                PseudoElement::Before,
                PseudoElement::After,
                PseudoElement::Marker,
            ] {
                let p = cascade_element(dom, id, cascade, &candidates, cx, &s, &venv, Some(pe));
                if p.has_rules {
                    out.pseudo.insert((id, pe), p);
                }
            }
            s
        } else if dom.text(id).is_some() || id == Dom::ROOT {
            (*parent).clone()
        } else {
            continue;
        };
        style.has_rules = false;
        // <details>: only the summary shows until it is opened.
        if let Some(p) = dom.parent(id).filter(|&p| dom.tag(p) == Some("details")) {
            let open = dom.attr(p, "open").is_some();
            if dom.tag(id) == Some("summary") {
                style.list_style = if open {
                    style::ListStyle::DisclosureOpen
                } else {
                    style::ListStyle::DisclosureClosed
                };
            } else if !open && dom.element(id).is_some() {
                style.display = style::Display::None;
            } else if !open && dom.text(id).is_some_and(|t| !t.trim().is_empty()) {
                continue;
            }
        }
        if style.display == style::Display::None {
            continue;
        }
        let shared = Rc::new(style);
        for &c in dom.children(id).iter().rev() {
            stack.push((c, shared.clone()));
        }
        out.nodes[id] = Some((*shared).clone());
    }
    out
}

/// Cascades element `id` (or its pseudo-element `pe`).
#[allow(clippy::too_many_arguments)]
fn cascade_element(
    dom: &Dom,
    id: NodeId,
    cascade: &Cascade,
    candidates: &[u32],
    cx: &Matcher,
    parent: &Style,
    venv: &ValueEnv,
    pe: Option<PseudoElement>,
) -> Style {
    let mut decls: Vec<Matched> = Vec::new();
    for &i in candidates {
        let r = &cascade.rules[i as usize];
        if r.selector.pseudo_element != pe || !selector::matches(cx, &r.selector, id) {
            continue;
        }
        for d in r.decls.iter() {
            decls.push(Matched {
                rank: rank(d.important, r.origin, r.selector.specificity, r.order),
                name: &d.name,
                value: &d.value,
            });
        }
    }
    let mut style = Style::inherit(parent);
    if pe.is_some() {
        if decls.is_empty() {
            return style;
        }
        style.display = style::Display::Inline;
    }
    let hints;
    let inline;
    if pe.is_none() {
        let base = cascade.rules.len() as u32;
        hints = presentational_hints(dom, id);
        for (k, (name, value)) in hints.iter().enumerate() {
            decls.push(Matched {
                rank: rank(false, 1, [0; 3], base + k as u32),
                name,
                value,
            });
        }
        inline = dom
            .attr(id, "style")
            .map(|s| syntax::parse_declarations(s, None))
            .unwrap_or_default();
        for (k, d) in inline.iter().enumerate() {
            decls.push(Matched {
                rank: rank(d.important, 3, [0; 3], base + 1000 + k as u32),
                name: &d.name,
                value: &d.value,
            });
        }
    }
    decls.sort_by_key(|d| d.rank);
    style.has_rules = !decls.is_empty();

    // Custom properties first (later ones win), inherited from the parent.
    if decls.iter().any(|d| d.name.starts_with("--")) {
        let mut vars: BTreeMap<String, String> = (*parent.vars).clone();
        for d in decls.iter().filter(|d| d.name.starts_with("--")) {
            vars.insert(d.name.to_string(), d.value.to_string());
        }
        style.vars = Rc::new(vars);
    }
    let apply = |style: &mut Style, d: &Matched| {
        if d.name.starts_with("--") {
            return;
        }
        if d.value.contains("var(") {
            match substitute_vars(d.value, &style.vars, 0) {
                Some(v) => style.apply_in(d.name, &v, parent, venv),
                // Invalid at computed-value time: behaves as `unset`.
                None => style.apply_in(d.name, "unset", parent, venv),
            }
        } else {
            style.apply_in(d.name, d.value, parent, venv);
        }
    };
    // font-size first: other lengths in `em` depend on it.
    for d in decls
        .iter()
        .filter(|d| d.name == "font-size" || d.name == "font")
    {
        apply(&mut style, d);
    }
    for d in decls
        .iter()
        .filter(|d| d.name != "font-size" && d.name != "font")
    {
        apply(&mut style, d);
    }
    style
}

/// Replaces `var(--name[, fallback])` using `vars`; `None` if a variable
/// is missing and has no fallback.
pub fn substitute_vars(value: &str, vars: &BTreeMap<String, String>, depth: u32) -> Option<String> {
    if depth > 16 {
        return None;
    }
    let mut out = String::with_capacity(value.len());
    let mut rest = value;
    while let Some(k) = rest.find("var(") {
        out.push_str(&rest[..k]);
        let after = &rest[k + 4..];
        // Find the matching `)`.
        let mut d = 1;
        let mut end = None;
        for (i, c) in after.char_indices() {
            match c {
                '(' => d += 1,
                ')' => {
                    d -= 1;
                    if d == 0 {
                        end = Some(i);
                        break;
                    }
                }
                _ => {}
            }
        }
        let end = end?;
        let inner = &after[..end];
        let (name, fallback) = match inner.find(',') {
            Some(c) => (inner[..c].trim(), Some(inner[c + 1..].trim())),
            None => (inner.trim(), None),
        };
        let replacement = match vars.get(name) {
            Some(v) if !v.trim().is_empty() || fallback.is_none() => Some(v.clone()),
            _ => fallback.map(str::to_string),
        }?;
        out.push_str(&substitute_vars(&replacement, vars, depth + 1)?);
        rest = &after[end + 1..];
    }
    out.push_str(rest);
    Some(out)
}

/// Convenience: computes styles from stylesheet source text (the UA sheet
/// is added automatically). Used by tests.
pub fn compute_styles(dom: &Dom, sheets: &[&str], state: &ElementState) -> Vec<Option<Style>> {
    let parsed: Vec<(Rc<Sheet>, MediaList)> = sheets
        .iter()
        .map(|s| {
            (
                Rc::new(syntax::parse_stylesheet(s, None)),
                MediaList::default(),
            )
        })
        .collect();
    let env = MediaEnv::default();
    let cascade = Cascade::new(&parsed, &env);
    let no = |_: NodeId| false;
    let cx = Matcher {
        dom,
        state: *state,
        checked: &no,
    };
    compute(dom, &cascade, &cx, &env).nodes
}

/// Styling attributes from older HTML (`<img width>`, `<td bgcolor>`, …),
/// which act like author rules with zero specificity.
fn presentational_hints(dom: &Dom, id: NodeId) -> Vec<(&'static str, String)> {
    let mut out = Vec::new();
    let Some(tag) = dom.tag(id) else {
        return out;
    };
    let length = |v: &str| {
        let v = v.trim();
        if v.ends_with('%') {
            Some(v.to_string())
        } else {
            v.parse::<f32>().ok().map(|n| format!("{n}px"))
        }
    };
    if matches!(
        tag,
        "img" | "video" | "canvas" | "iframe" | "table" | "td" | "th" | "col"
    ) {
        for dim in ["width", "height"] {
            if let Some(v) = dom.attr(id, dim).and_then(length) {
                out.push((dim, v));
            }
        }
    }
    if matches!(
        tag,
        "table" | "tr" | "td" | "th" | "body" | "thead" | "tbody" | "tfoot"
    ) && let Some(c) = dom.attr(id, "bgcolor")
    {
        out.push(("background-color", c.to_string()));
    }
    if matches!(
        tag,
        "td" | "th" | "tr" | "div" | "p" | "h1" | "h2" | "h3" | "caption"
    ) && let Some(a) = dom.attr(id, "align")
    {
        out.push(("text-align", a.to_string()));
    }
    if matches!(tag, "td" | "th" | "tr")
        && let Some(v) = dom.attr(id, "valign")
    {
        out.push(("vertical-align", v.to_string()));
    }
    // Cells take `border` and `cellpadding` from their table.
    let table = if tag == "table" {
        Some(id)
    } else if matches!(tag, "td" | "th") {
        let mut n = dom.parent(id);
        while let Some(p) = n.filter(|&p| dom.tag(p) != Some("table")) {
            n = dom.parent(p);
        }
        n
    } else {
        None
    };
    if let Some(t) = table {
        let border = dom
            .attr(t, "border")
            .map(|b| b.trim().parse::<f32>().unwrap_or(1.0));
        if tag == "table" {
            if let Some(b) = border.filter(|&b| b > 0.0) {
                out.push(("border", format!("{b}px outset #808080")));
            }
            if let Some(s) = dom.attr(t, "cellspacing").and_then(length) {
                out.push(("border-spacing", s));
            }
        } else {
            if border.is_some_and(|b| b > 0.0) {
                out.push(("border", "1px inset #808080".to_string()));
            }
            if let Some(p) = dom.attr(t, "cellpadding").and_then(length) {
                out.push(("padding", p));
            }
        }
    }
    out
}

/// Collects author stylesheet text from `<style>` elements and the given
/// already-loaded `<link rel=stylesheet>` contents (by node id).
pub fn author_sheets(dom: &Dom, linked: &[(NodeId, String)]) -> Vec<String> {
    let mut out = Vec::new();
    for id in dom.descendants(Dom::ROOT) {
        match dom.tag(id) {
            Some("style") => out.push(dom.text_content(id)),
            Some("link") => {
                if let Some((_, css)) = linked.iter().find(|(n, _)| *n == id) {
                    out.push(css.clone());
                }
            }
            _ => {}
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::style::*;
    use super::values::{Dim, Rgba};
    use super::*;
    use crate::dom::parse_html;

    fn styles(html: &str, state: ElementState) -> (Dom, Vec<Option<Style>>) {
        let dom = parse_html(html);
        let sheets = author_sheets(&dom, &[]);
        let refs: Vec<&str> = sheets.iter().map(String::as_str).collect();
        let s = compute_styles(&dom, &refs, &state);
        (dom, s)
    }

    fn by_id<'a>(dom: &Dom, s: &'a [Option<Style>], id: &str) -> &'a Style {
        s[dom.element_by_id(id).unwrap()].as_ref().unwrap()
    }

    const RED: Rgba = Rgba::new(255, 0, 0, 255);
    const BLUE: Rgba = Rgba::new(0, 0, 255, 255);

    #[test]
    fn cascade_order_and_specificity() {
        let (d, s) = styles(
            r#"<style>
                p { color: red }
                .x { color: blue }
                #a { color: green }
                p { color: blue }
                .imp { color: red !important }
            </style>
            <p id=a class=x>1</p><p id=b>2</p><p id=c class=x>3</p>
            <p id=d class=imp style="color: blue">4</p><p id=e style="color: red">5</p>"#,
            ElementState::default(),
        );
        assert_eq!(by_id(&d, &s, "a").color, Rgba::new(0, 128, 0, 255));
        assert_eq!(
            by_id(&d, &s, "b").color,
            BLUE,
            "later rule wins at equal specificity"
        );
        assert_eq!(by_id(&d, &s, "c").color, BLUE);
        assert_eq!(
            by_id(&d, &s, "d").color,
            RED,
            "!important beats inline style"
        );
        assert_eq!(by_id(&d, &s, "e").color, RED, "inline style beats sheets");
    }

    #[test]
    fn ua_defaults_and_inheritance() {
        let (d, s) = styles(
            r#"<body style="font-size: 20px"><h1 id=h>T</h1><p id=p>x <b id=b>y</b></p>
               <ul><li id=li>i</li></ul><head><title>t</title></head>"#,
            ElementState::default(),
        );
        let h = by_id(&d, &s, "h");
        assert_eq!(h.display, Display::Block);
        assert_eq!(h.font_size, 40.0);
        assert!(h.bold);
        assert_eq!(h.margin[TOP], Dim::Px(0.67 * 40.0));
        assert!(by_id(&d, &s, "b").bold);
        assert_eq!(by_id(&d, &s, "b").font_size, 20.0);
        assert_eq!(by_id(&d, &s, "li").display, Display::ListItem);
        let title = d.find_tag(Dom::ROOT, "title").unwrap();
        assert!(s[title].is_none(), "display:none subtrees get no style");
        let text = d.children(d.element_by_id("p").unwrap())[0];
        assert_eq!(s[text].as_ref().unwrap().font_size, 20.0);
    }

    #[test]
    fn selectors_and_pseudo_classes() {
        let html = r#"<style>
            div > span { color: red }
            ul li:first-child { color: blue }
            a:hover { color: red }
            input:focus { color: blue }
            [data-x="1"] + p { color: red }
          </style>
          <div><span id=s>a</span></div><div><p><span id=t>b</span></p></div>
          <ul><li id=l1>1</li><li id=l2>2</li></ul>
          <a id=a href=#><span id=in>x</span></a><input id=i>
          <p data-x=1></p><p id=next></p>"#;
        let (d, s) = styles(html, ElementState::default());
        assert_eq!(by_id(&d, &s, "s").color, RED);
        assert_eq!(by_id(&d, &s, "t").color, Rgba::BLACK);
        assert_eq!(by_id(&d, &s, "l1").color, BLUE);
        assert_eq!(by_id(&d, &s, "l2").color, Rgba::BLACK);
        assert_eq!(by_id(&d, &s, "next").color, RED);
        assert_ne!(by_id(&d, &s, "a").color, RED);

        let dom = parse_html(html);
        let state = ElementState {
            hover: dom.element_by_id("in"),
            focus: dom.element_by_id("i"),
            active: None,
        };
        let (d, s) = styles(html, state);
        assert_eq!(
            by_id(&d, &s, "a").color,
            RED,
            "hovering a child hovers the link"
        );
        assert_eq!(by_id(&d, &s, "i").color, BLUE);
    }

    #[test]
    fn custom_properties_media_and_pseudo_elements() {
        let html = r#"<style>
            :root { --brand: #ff0000; --gap: 8px; --pad: var(--gap) }
            .card { color: var(--brand); padding: var(--pad); margin: var(--missing, 3px) }
            .alt { --brand: blue }
            .bad { width: 10px; width: var(--nope) }
            @media (min-width: 2000px) { .card { color: green } }
            @media (max-width: 2000px) { .card { font-size: 20px } }
            .card::before { content: "x"; color: blue }
          </style>
          <div id=c class=card>a</div><div class=alt><p id=n class=card>b</p></div>
          <div id=b class=bad></div>"#;
        let dom = parse_html(html);
        let sheets = author_sheets(&dom, &[]);
        let refs: Vec<&str> = sheets.iter().map(String::as_str).collect();
        let parsed: Vec<_> = refs
            .iter()
            .map(|s| {
                (
                    Rc::new(syntax::parse_stylesheet(s, None)),
                    MediaList::default(),
                )
            })
            .collect();
        let env = MediaEnv::default();
        let cascade = Cascade::new(&parsed, &env);
        let no = |_: NodeId| false;
        let cx = Matcher {
            dom: &dom,
            state: ElementState::default(),
            checked: &no,
        };
        let st = compute(&dom, &cascade, &cx, &env);
        let get = |id: &str| st.nodes[dom.element_by_id(id).unwrap()].as_ref().unwrap();
        let c = get("c");
        assert_eq!(c.color, RED);
        assert_eq!(c.padding[0], Dim::Px(8.0), "nested var()");
        assert_eq!(c.margin[0], Dim::Px(3.0), "fallback");
        assert_eq!(c.font_size, 20.0, "matching @media applies");
        assert_eq!(
            get("n").color,
            BLUE,
            "custom properties inherit and override"
        );
        assert_eq!(
            get("b").width,
            Dim::Auto,
            "invalid var() makes the property unset"
        );
        let before = st.pseudo.get(&(
            dom.element_by_id("c").unwrap(),
            selector::PseudoElement::Before,
        ));
        assert_eq!(before.map(|s| s.color), Some(BLUE));
    }

    #[test]
    fn query_matching() {
        let d = parse_html("<p class='a b'>x</p><div id=z></div>");
        let p = d.find_tag(Dom::ROOT, "p").unwrap();
        let st = ElementState::default();
        assert!(matches(&d, p, ".b", &st));
        assert!(matches(&d, p, "div, p.a", &st));
        assert!(!matches(&d, p, "#z", &st));
    }
}
