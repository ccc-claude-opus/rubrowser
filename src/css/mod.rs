//! CSS: stylesheets, selector matching, cascade and computed styles.
//!
//! Parsing and selector matching use `simplecss`. Stylesheets are kept as
//! source text and parsed on each restyle (fast, and avoids self-referential
//! storage of borrowed rules).

pub mod style;
pub mod values;

use simplecss::{AttributeOperator, DeclarationTokenizer, PseudoClass, StyleSheet};

use crate::dom::{Dom, NodeId};
pub use style::Style;

/// The browser's default stylesheet.
pub const UA_CSS: &str = include_str!("ua.css");

/// Dynamic element state that pseudo-classes depend on.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct ElementState {
    pub hover: Option<NodeId>,
    pub active: Option<NodeId>,
    pub focus: Option<NodeId>,
}

/// An element as seen by `simplecss` selector matching.
#[derive(Clone, Copy)]
struct El<'a> {
    dom: &'a Dom,
    id: NodeId,
    state: &'a ElementState,
}

impl simplecss::Element for El<'_> {
    fn parent_element(&self) -> Option<Self> {
        let p = self.dom.parent(self.id)?;
        self.dom.element(p)?;
        Some(El { id: p, ..*self })
    }

    fn prev_sibling_element(&self) -> Option<Self> {
        let s = self.dom.previous_element_sibling(self.id)?;
        Some(El { id: s, ..*self })
    }

    fn has_local_name(&self, name: &str) -> bool {
        self.dom
            .tag(self.id)
            .is_some_and(|t| t.eq_ignore_ascii_case(name))
    }

    fn attribute_matches(&self, name: &str, op: AttributeOperator<'_>) -> bool {
        match self.dom.attr(self.id, name) {
            Some(v) if name == "class" => match op {
                // Classes may be separated by any whitespace.
                AttributeOperator::Contains(c) => v.split_ascii_whitespace().any(|x| x == c),
                _ => op.matches(v),
            },
            Some(v) => op.matches(v),
            None => false,
        }
    }

    fn pseudo_class_matches(&self, class: PseudoClass<'_>) -> bool {
        let within = |target: Option<NodeId>| {
            target.is_some_and(|t| self.dom.is_inclusive_ancestor(self.id, t))
        };
        match class {
            PseudoClass::FirstChild => self.dom.previous_element_sibling(self.id).is_none(),
            PseudoClass::Link => {
                self.has_local_name("a") && self.dom.attr(self.id, "href").is_some()
            }
            PseudoClass::Visited => false,
            PseudoClass::Hover => within(self.state.hover),
            PseudoClass::Active => within(self.state.active),
            PseudoClass::Focus => self.state.focus == Some(self.id),
            PseudoClass::Lang(_) => false,
        }
    }
}

/// Whether `selector` matches element `id`. Used by scripts' `query`.
pub fn matches(dom: &Dom, id: NodeId, selector: &str, state: &ElementState) -> bool {
    dom.element(id).is_some()
        && selector.split(',').any(|s| {
            simplecss::Selector::parse(s.trim())
                .is_some_and(|sel| sel.matches(&El { dom, id, state }))
        })
}

/// One matched declaration, with what decides its cascade order.
struct Matched<'a> {
    important: bool,
    /// 0 = user agent, 1 = presentational attributes, 2 = author
    /// sheet, 3 = `style` attribute.
    origin: u8,
    specificity: [u8; 3],
    order: usize,
    name: &'a str,
    value: &'a str,
}

/// Computes the style of every node. `sheets` are author stylesheets in
/// document order (the UA sheet is added automatically). Returns one entry
/// per node id; text nodes get their parent's style, others `None` when
/// they are not rendered.
pub fn compute_styles(dom: &Dom, sheets: &[&str], state: &ElementState) -> Vec<Option<Style>> {
    let parsed: Vec<StyleSheet> = std::iter::once(UA_CSS)
        .chain(sheets.iter().copied())
        .map(StyleSheet::parse)
        .collect();
    let mut out: Vec<Option<Style>> = vec![None; dom.len()];
    let root_style = Style::default();
    let mut root_font = root_style.font_size;
    // Walk in document order so parents are styled before children.
    let mut stack: Vec<(NodeId, Style)> = vec![(Dom::ROOT, root_style)];
    while let Some((id, parent_style)) = stack.pop() {
        let style = if dom.element(id).is_some() {
            let s = cascade(dom, id, &parsed, state, &parent_style, root_font);
            if dom.tag(id) == Some("html") {
                root_font = s.font_size;
            }
            s
        } else if dom.text(id).is_some() || id == Dom::ROOT {
            parent_style.clone()
        } else {
            continue;
        };
        if style.display == style::Display::None {
            continue;
        }
        for &c in dom.children(id).iter().rev() {
            stack.push((c, style.clone()));
        }
        out[id] = Some(style);
    }
    out
}

fn cascade(
    dom: &Dom,
    id: NodeId,
    sheets: &[StyleSheet],
    state: &ElementState,
    parent: &Style,
    root_font: f32,
) -> Style {
    let el = El { dom, id, state };
    let mut decls: Vec<Matched> = Vec::new();
    let mut order = 0;
    for (i, sheet) in sheets.iter().enumerate() {
        for rule in &sheet.rules {
            order += 1;
            if !rule.selector.matches(&el) {
                continue;
            }
            for d in &rule.declarations {
                decls.push(Matched {
                    important: d.important,
                    origin: if i == 0 { 0 } else { 2 },
                    specificity: rule.selector.specificity(),
                    order,
                    name: d.name,
                    value: d.value,
                });
            }
        }
    }
    let hints = presentational_hints(dom, id);
    for (name, value) in &hints {
        order += 1;
        decls.push(Matched {
            important: false,
            origin: 1,
            specificity: [0; 3],
            order,
            name,
            value,
        });
    }
    let inline = dom.attr(id, "style").unwrap_or("");
    for d in DeclarationTokenizer::from(inline) {
        order += 1;
        decls.push(Matched {
            important: d.important,
            origin: 3,
            specificity: [0; 3],
            order,
            name: d.name,
            value: d.value,
        });
    }
    decls.sort_by_key(|d| (d.important, d.origin, d.specificity, d.order));
    let mut style = Style::inherit(parent);
    // font-size first: other lengths in `em` depend on it.
    for d in decls
        .iter()
        .filter(|d| d.name == "font-size" || d.name == "font")
    {
        style.apply(d.name, d.value, parent, root_font);
    }
    for d in decls
        .iter()
        .filter(|d| d.name != "font-size" && d.name != "font")
    {
        style.apply(d.name, d.value, parent, root_font);
    }
    style
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
    fn query_matching() {
        let d = parse_html("<p class='a b'>x</p><div id=z></div>");
        let p = d.find_tag(Dom::ROOT, "p").unwrap();
        let st = ElementState::default();
        assert!(matches(&d, p, ".b", &st));
        assert!(matches(&d, p, "div, p.a", &st));
        assert!(!matches(&d, p, "#z", &st));
    }
}
