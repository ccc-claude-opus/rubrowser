//! The document tree: an arena of nodes addressed by [`NodeId`].
//!
//! Removed nodes stay in the arena (detached), so ids held by scripts never
//! dangle; they simply stop being part of the document.

mod parse;

pub use parse::{parse_html, parse_html_fragment};

/// Elements that never have children or an end tag.
const VOID: &[&str] = &[
    "area", "base", "br", "col", "embed", "hr", "img", "input", "link", "meta", "source", "track",
    "wbr",
];

pub type NodeId = usize;

#[derive(Clone, Debug, PartialEq)]
pub enum NodeData {
    Document,
    Element(Element),
    Text(String),
    Comment(String),
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct Element {
    /// Lowercase local name, e.g. `"div"`.
    pub name: String,
    pub attrs: Vec<(String, String)>,
}

impl Element {
    pub fn new(name: &str) -> Self {
        Element {
            name: name.to_ascii_lowercase(),
            attrs: Vec::new(),
        }
    }

    pub fn attr(&self, name: &str) -> Option<&str> {
        self.attrs
            .iter()
            .find(|(n, _)| n == name)
            .map(|(_, v)| v.as_str())
    }

    pub fn set_attr(&mut self, name: &str, value: &str) {
        match self.attrs.iter_mut().find(|(n, _)| n == name) {
            Some((_, v)) => *v = value.to_string(),
            None => self.attrs.push((name.to_string(), value.to_string())),
        }
    }

    pub fn remove_attr(&mut self, name: &str) {
        self.attrs.retain(|(n, _)| n != name);
    }

    pub fn id(&self) -> Option<&str> {
        self.attr("id")
    }

    pub fn classes(&self) -> impl Iterator<Item = &str> {
        self.attr("class").unwrap_or("").split_ascii_whitespace()
    }

    pub fn has_class(&self, class: &str) -> bool {
        self.classes().any(|c| c == class)
    }
}

#[derive(Clone, Debug)]
pub struct Node {
    pub parent: Option<NodeId>,
    pub children: Vec<NodeId>,
    pub data: NodeData,
}

#[derive(Clone, Debug)]
pub struct Dom {
    nodes: Vec<Node>,
    /// The document has no (or an old) doctype: quirks-mode rendering.
    pub quirks: bool,
}

impl Default for Dom {
    fn default() -> Self {
        Self::new()
    }
}

impl Dom {
    /// The document node.
    pub const ROOT: NodeId = 0;

    pub fn new() -> Self {
        Dom {
            nodes: vec![Node {
                parent: None,
                children: Vec::new(),
                data: NodeData::Document,
            }],
            quirks: false,
        }
    }

    pub fn len(&self) -> usize {
        self.nodes.len()
    }

    pub fn is_empty(&self) -> bool {
        self.nodes.is_empty()
    }

    pub fn node(&self, id: NodeId) -> &Node {
        &self.nodes[id]
    }

    pub fn get(&self, id: NodeId) -> Option<&Node> {
        self.nodes.get(id)
    }

    fn create(&mut self, data: NodeData) -> NodeId {
        self.nodes.push(Node {
            parent: None,
            children: Vec::new(),
            data,
        });
        self.nodes.len() - 1
    }

    pub fn create_element(&mut self, element: Element) -> NodeId {
        self.create(NodeData::Element(element))
    }

    pub fn create_text(&mut self, text: &str) -> NodeId {
        self.create(NodeData::Text(text.to_string()))
    }

    pub fn create_comment(&mut self, text: &str) -> NodeId {
        self.create(NodeData::Comment(text.to_string()))
    }

    pub fn parent(&self, id: NodeId) -> Option<NodeId> {
        self.nodes.get(id)?.parent
    }

    pub fn children(&self, id: NodeId) -> &[NodeId] {
        self.nodes.get(id).map_or(&[], |n| &n.children)
    }

    pub fn element(&self, id: NodeId) -> Option<&Element> {
        match &self.nodes.get(id)?.data {
            NodeData::Element(e) => Some(e),
            _ => None,
        }
    }

    pub fn element_mut(&mut self, id: NodeId) -> Option<&mut Element> {
        match &mut self.nodes.get_mut(id)?.data {
            NodeData::Element(e) => Some(e),
            _ => None,
        }
    }

    pub fn text(&self, id: NodeId) -> Option<&str> {
        match &self.nodes[id].data {
            NodeData::Text(t) => Some(t),
            _ => None,
        }
    }

    /// Local name of an element node.
    pub fn tag(&self, id: NodeId) -> Option<&str> {
        self.element(id).map(|e| e.name.as_str())
    }

    pub fn attr(&self, id: NodeId, name: &str) -> Option<&str> {
        self.element(id)?.attr(name)
    }

    /// Whether `ancestor` is `id` or contains it.
    pub fn is_inclusive_ancestor(&self, ancestor: NodeId, mut id: NodeId) -> bool {
        loop {
            if id == ancestor {
                return true;
            }
            match self.nodes[id].parent {
                Some(p) => id = p,
                None => return false,
            }
        }
    }

    /// Whether `id` is attached to the document.
    pub fn is_connected(&self, id: NodeId) -> bool {
        self.is_inclusive_ancestor(Self::ROOT, id)
    }

    /// Removes `id` from its parent; it stays alive, detached.
    pub fn detach(&mut self, id: NodeId) {
        if let Some(p) = self.nodes[id].parent.take() {
            self.nodes[p].children.retain(|&c| c != id);
        }
    }

    /// Appends `child` to `parent` (moving it if attached elsewhere).
    /// Returns false if that would create a cycle.
    pub fn append(&mut self, parent: NodeId, child: NodeId) -> bool {
        self.insert_before(parent, child, None)
    }

    /// Inserts `child` into `parent` before `before` (or last).
    pub fn insert_before(&mut self, parent: NodeId, child: NodeId, before: Option<NodeId>) -> bool {
        if self.is_inclusive_ancestor(child, parent) {
            return false;
        }
        self.detach(child);
        let kids = &mut self.nodes[parent].children;
        let at = before
            .and_then(|b| kids.iter().position(|&c| c == b))
            .unwrap_or(kids.len());
        kids.insert(at, child);
        self.nodes[child].parent = Some(parent);
        true
    }

    /// Appends text, merging with a preceding text node.
    pub fn append_text(&mut self, parent: NodeId, text: &str) {
        if let Some(&last) = self.nodes[parent].children.last()
            && let NodeData::Text(t) = &mut self.nodes[last].data
        {
            t.push_str(text);
            return;
        }
        let t = self.create_text(text);
        self.append(parent, t);
    }

    pub fn previous_sibling(&self, id: NodeId) -> Option<NodeId> {
        let p = self.nodes[id].parent?;
        let kids = &self.nodes[p].children;
        let i = kids.iter().position(|&c| c == id)?;
        i.checked_sub(1).map(|j| kids[j])
    }

    pub fn previous_element_sibling(&self, id: NodeId) -> Option<NodeId> {
        let p = self.nodes[id].parent?;
        let kids = &self.nodes[p].children;
        let i = kids.iter().position(|&c| c == id)?;
        kids[..i]
            .iter()
            .rev()
            .copied()
            .find(|&c| self.element(c).is_some())
    }

    /// `id` and all its descendants, in document order.
    pub fn descendants(&self, id: NodeId) -> Vec<NodeId> {
        let mut out = Vec::new();
        let mut stack = vec![id];
        while let Some(n) = stack.pop() {
            out.push(n);
            stack.extend(self.nodes[n].children.iter().rev());
        }
        out
    }

    /// First element named `tag` in document order under `id`.
    pub fn find_tag(&self, id: NodeId, tag: &str) -> Option<NodeId> {
        self.descendants(id)
            .into_iter()
            .find(|&n| self.tag(n) == Some(tag))
    }

    /// Element whose `id` attribute is `value`.
    pub fn element_by_id(&self, value: &str) -> Option<NodeId> {
        self.descendants(Self::ROOT)
            .into_iter()
            .find(|&n| self.element(n).and_then(|e| e.id()) == Some(value))
    }

    /// Concatenated text of all descendant text nodes.
    pub fn text_content(&self, id: NodeId) -> String {
        self.descendants(id)
            .into_iter()
            .filter_map(|n| self.text(n))
            .collect()
    }

    /// Replaces all children of `id` with one text node.
    pub fn set_text_content(&mut self, id: NodeId, text: &str) {
        for c in self.nodes[id].children.clone() {
            self.detach(c);
        }
        if !text.is_empty() {
            let t = self.create_text(text);
            self.append(id, t);
        }
    }

    /// Copies node `node` of `src` (with its subtree) into this tree,
    /// detached. Returns the new node.
    pub fn import(&mut self, src: &Dom, node: NodeId) -> NodeId {
        let copy = self.create(src.nodes[node].data.clone());
        for &c in &src.nodes[node].children {
            let child = self.import(src, c);
            self.append(copy, child);
        }
        copy
    }

    /// Serializes `id` as HTML: its children only (`innerHTML`), or with
    /// the element itself (`outerHTML`).
    pub fn to_html(&self, id: NodeId, outer: bool) -> String {
        let mut out = String::new();
        if outer {
            self.write_html(id, &mut out);
        } else {
            for &c in &self.nodes[id].children {
                self.write_html(c, &mut out);
            }
        }
        out
    }

    fn write_html(&self, id: NodeId, out: &mut String) {
        match &self.nodes[id].data {
            NodeData::Document => {
                for &c in &self.nodes[id].children {
                    self.write_html(c, out);
                }
            }
            NodeData::Text(t) => {
                let raw = self
                    .parent(id)
                    .and_then(|p| self.tag(p))
                    .is_some_and(|t| matches!(t, "style" | "script"));
                if raw {
                    out.push_str(t);
                } else {
                    out.push_str(&escape(t, false));
                }
            }
            NodeData::Comment(t) => {
                out.push_str("<!--");
                out.push_str(t);
                out.push_str("-->");
            }
            NodeData::Element(e) => {
                out.push('<');
                out.push_str(&e.name);
                for (n, v) in &e.attrs {
                    out.push(' ');
                    out.push_str(n);
                    out.push_str("=\"");
                    out.push_str(&escape(v, true));
                    out.push('"');
                }
                out.push('>');
                if VOID.contains(&e.name.as_str()) {
                    return;
                }
                for &c in &self.nodes[id].children {
                    self.write_html(c, out);
                }
                out.push_str("</");
                out.push_str(&e.name);
                out.push('>');
            }
        }
    }

    /// Replaces the children of `id` with nodes parsed from `html`.
    pub fn set_inner_html(&mut self, id: NodeId, html: &str) {
        let context = self.tag(id).unwrap_or("body").to_string();
        let (frag, top) = parse_html_fragment(html, &context);
        for c in self.nodes[id].children.clone() {
            self.detach(c);
        }
        for t in top {
            let n = self.import(&frag, t);
            self.append(id, n);
        }
    }

    /// The `<html>`, `<head>` and `<body>` elements, if present.
    pub fn html(&self) -> Option<NodeId> {
        self.children(Self::ROOT)
            .iter()
            .copied()
            .find(|&c| self.tag(c) == Some("html"))
    }

    pub fn body(&self) -> Option<NodeId> {
        let html = self.html()?;
        self.children(html)
            .iter()
            .copied()
            .find(|&c| self.tag(c) == Some("body"))
    }

    pub fn head(&self) -> Option<NodeId> {
        let html = self.html()?;
        self.children(html)
            .iter()
            .copied()
            .find(|&c| self.tag(c) == Some("head"))
    }

    /// Contents of `<title>`, trimmed.
    pub fn title(&self) -> Option<String> {
        let t = self.find_tag(Self::ROOT, "title")?;
        let s = self.text_content(t);
        let s = s.split_whitespace().collect::<Vec<_>>().join(" ");
        (!s.is_empty()).then_some(s)
    }
}

pub(crate) fn escape(s: &str, attr: bool) -> String {
    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
        match c {
            '&' => out.push_str("&amp;"),
            '<' if !attr => out.push_str("&lt;"),
            '>' if !attr => out.push_str("&gt;"),
            '"' if attr => out.push_str("&quot;"),
            '\u{a0}' => out.push_str("&nbsp;"),
            c => out.push(c),
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn serialize_and_inner_html() {
        let mut d = parse_html("<div id=a class=\"x\">1 &lt; 2<br><i>it</i></div>");
        let a = d.element_by_id("a").unwrap();
        assert_eq!(d.to_html(a, false), "1 &lt; 2<br><i>it</i>");
        assert_eq!(
            d.to_html(a, true),
            "<div id=\"a\" class=\"x\">1 &lt; 2<br><i>it</i></div>"
        );
        d.set_inner_html(a, "<b>new</b> text");
        assert_eq!(d.to_html(a, false), "<b>new</b> text");
        assert_eq!(d.text_content(a), "new text");
    }

    #[test]
    fn tree_operations() {
        let mut d = Dom::new();
        let a = d.create_element(Element::new("DIV"));
        let b = d.create_element(Element::new("span"));
        d.append(Dom::ROOT, a);
        d.append(a, b);
        d.append_text(b, "x");
        d.append_text(b, "y");
        assert_eq!(d.tag(a), Some("div"));
        assert_eq!(d.text_content(a), "xy");
        assert_eq!(d.children(b).len(), 1);
        assert!(!d.append(b, a), "cycle rejected");
        d.detach(b);
        assert!(!d.is_connected(b));
        assert_eq!(d.text_content(a), "");
    }

    #[test]
    fn insert_and_siblings() {
        let mut d = Dom::new();
        let p = d.create_element(Element::new("ul"));
        let x = d.create_element(Element::new("li"));
        let y = d.create_element(Element::new("li"));
        d.append(p, y);
        d.insert_before(p, x, Some(y));
        assert_eq!(d.children(p), &[x, y]);
        assert_eq!(d.previous_element_sibling(y), Some(x));
        assert_eq!(d.previous_element_sibling(x), None);
    }

    #[test]
    fn attributes_and_classes() {
        let mut e = Element::new("a");
        e.set_attr("class", " big  red ");
        e.set_attr("id", "x");
        e.set_attr("id", "y");
        assert!(e.has_class("red") && !e.has_class("bi"));
        assert_eq!(e.id(), Some("y"));
        e.remove_attr("id");
        assert_eq!(e.id(), None);
    }
}
