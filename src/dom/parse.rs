//! Builds a [`Dom`] with html5ever through its `TreeSink` interface.

use std::borrow::Cow;
use std::cell::RefCell;
use std::rc::Rc;

use html5ever::tendril::{StrTendril, TendrilSink};
use html5ever::tree_builder::{ElementFlags, NodeOrText, QuirksMode, TreeSink};
use html5ever::{Attribute, LocalName, Namespace, QualName, ns, parse_document, parse_fragment};

use super::{Dom, Element, NodeId};

/// Parses an HTML document. Never fails: html5ever recovers from any input.
pub fn parse_html(html: &str) -> Dom {
    parse_document(Sink::default(), Default::default()).one(html)
}

/// Parses `html` as the contents of a `<context>` element (for
/// `innerHTML`). Returns the parsed tree and the top-level nodes.
pub fn parse_html_fragment(html: &str, context: &str) -> (Dom, Vec<NodeId>) {
    let name = QualName::new(None, ns!(html), LocalName::from(context));
    let dom =
        parse_fragment(Sink::default(), Default::default(), name, Vec::new(), false).one(html);
    // html5ever puts the fragment's nodes under a synthetic <html> root.
    let top = dom
        .children(Dom::ROOT)
        .first()
        .map(|&h| dom.children(h).to_vec())
        .unwrap_or_default();
    (dom, top)
}

/// A node id plus, for elements, the qualified name html5ever asks back for.
#[derive(Clone)]
struct Handle {
    id: NodeId,
    name: Option<Rc<QualName>>,
}

struct Sink {
    dom: RefCell<Dom>,
    /// Name returned for non-element handles (html5ever never asks for it).
    none: QualName,
}

impl Default for Sink {
    fn default() -> Self {
        Sink {
            dom: RefCell::new(Dom::new()),
            none: QualName::new(None, Namespace::from(""), LocalName::from("")),
        }
    }
}

fn to_element(name: &QualName, attrs: Vec<Attribute>) -> Element {
    let mut e = Element::new(&name.local);
    for a in attrs {
        e.set_attr(&a.name.local, &a.value);
    }
    e
}

impl Sink {
    fn add(&self, parent: NodeId, child: NodeOrText<Handle>, before: Option<NodeId>) {
        let mut dom = self.dom.borrow_mut();
        match child {
            NodeOrText::AppendNode(h) => {
                dom.insert_before(parent, h.id, before);
            }
            NodeOrText::AppendText(text) => match before {
                None => dom.append_text(parent, &text),
                Some(b) => {
                    let prev = dom.previous_sibling(b);
                    if let Some(p) = prev
                        && let super::NodeData::Text(t) = &mut dom.nodes[p].data
                    {
                        t.push_str(&text);
                        return;
                    }
                    let t = dom.create_text(&text);
                    dom.insert_before(parent, t, Some(b));
                }
            },
        }
    }
}

impl TreeSink for Sink {
    type Handle = Handle;
    type Output = Dom;
    type ElemName<'a> = &'a QualName;

    fn finish(self) -> Dom {
        self.dom.into_inner()
    }

    fn parse_error(&self, _msg: Cow<'static, str>) {}

    fn get_document(&self) -> Handle {
        Handle {
            id: Dom::ROOT,
            name: None,
        }
    }

    fn elem_name<'a>(&'a self, target: &'a Handle) -> &'a QualName {
        target.name.as_deref().unwrap_or(&self.none)
    }

    fn create_element(&self, name: QualName, attrs: Vec<Attribute>, _: ElementFlags) -> Handle {
        let id = self
            .dom
            .borrow_mut()
            .create_element(to_element(&name, attrs));
        Handle {
            id,
            name: Some(Rc::new(name)),
        }
    }

    fn create_comment(&self, text: StrTendril) -> Handle {
        let id = self.dom.borrow_mut().create_comment(&text);
        Handle { id, name: None }
    }

    fn create_pi(&self, _target: StrTendril, data: StrTendril) -> Handle {
        self.create_comment(data)
    }

    fn append(&self, parent: &Handle, child: NodeOrText<Handle>) {
        self.add(parent.id, child, None);
    }

    fn append_based_on_parent_node(
        &self,
        element: &Handle,
        prev_element: &Handle,
        child: NodeOrText<Handle>,
    ) {
        if self.dom.borrow().parent(element.id).is_some() {
            self.append_before_sibling(element, child);
        } else {
            self.append(prev_element, child);
        }
    }

    fn append_doctype_to_document(&self, _: StrTendril, _: StrTendril, _: StrTendril) {}

    fn get_template_contents(&self, target: &Handle) -> Handle {
        // Template contents are kept as ordinary (unrendered) children.
        target.clone()
    }

    fn same_node(&self, x: &Handle, y: &Handle) -> bool {
        x.id == y.id
    }

    fn set_quirks_mode(&self, _: QuirksMode) {}

    fn append_before_sibling(&self, sibling: &Handle, new_node: NodeOrText<Handle>) {
        let parent = self.dom.borrow().parent(sibling.id);
        if let Some(p) = parent {
            self.add(p, new_node, Some(sibling.id));
        }
    }

    fn add_attrs_if_missing(&self, target: &Handle, attrs: Vec<Attribute>) {
        let mut dom = self.dom.borrow_mut();
        if let Some(e) = dom.element_mut(target.id) {
            for a in attrs {
                if e.attr(&a.name.local).is_none() {
                    e.set_attr(&a.name.local, &a.value);
                }
            }
        }
    }

    fn remove_from_parent(&self, target: &Handle) {
        self.dom.borrow_mut().detach(target.id);
    }

    fn reparent_children(&self, node: &Handle, new_parent: &Handle) {
        let mut dom = self.dom.borrow_mut();
        for c in dom.children(node.id).to_vec() {
            dom.append(new_parent.id, c);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn builds_implied_structure() {
        let d = parse_html("<title>Hi  there</title><p class=x>One<p>Two &amp; <b>three</b>");
        let body = d.body().unwrap();
        let ps: Vec<_> = d
            .children(body)
            .iter()
            .copied()
            .filter(|&c| d.tag(c) == Some("p"))
            .collect();
        assert_eq!(ps.len(), 2, "unclosed <p> is closed by the next one");
        assert_eq!(d.attr(ps[0], "class"), Some("x"));
        assert_eq!(d.text_content(ps[1]), "Two & three");
        assert_eq!(d.title().as_deref(), Some("Hi there"));
        assert!(d.head().is_some());
    }

    #[test]
    fn style_and_script_are_raw_text() {
        let d = parse_html("<style>a < b { color: red }</style><script>if (a<b) {}</script>");
        let style = d.find_tag(Dom::ROOT, "style").unwrap();
        assert_eq!(d.text_content(style), "a < b { color: red }");
        let script = d.find_tag(Dom::ROOT, "script").unwrap();
        assert_eq!(d.text_content(script), "if (a<b) {}");
    }

    #[test]
    fn misnested_tags_are_repaired() {
        let d = parse_html("<b>1<i>2</b>3</i>");
        let body = d.body().unwrap();
        assert_eq!(d.text_content(body), "123");
        // Adoption agency: "3" ends up in an <i> outside the <b>.
        let last = *d.children(body).last().unwrap();
        assert_eq!(d.tag(last), Some("i"));
    }

    #[test]
    fn fragments() {
        let (d, top) = parse_html_fragment("a<b>bold</b><li>x", "ul");
        assert_eq!(top.len(), 3);
        assert_eq!(d.text(top[0]), Some("a"));
        assert_eq!(d.tag(top[1]), Some("b"));
        assert_eq!(d.tag(top[2]), Some("li"));
    }

    #[test]
    fn tables_foster_parent_text() {
        let d = parse_html("<table>x<tr><td>y</td></tr></table>");
        let body = d.body().unwrap();
        let first = d.children(body)[0];
        assert_eq!(d.text(first), Some("x"));
    }
}
