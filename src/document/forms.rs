//! Form controls: kinds, checked/selected state, labels and form data.

use std::collections::HashMap;

use crate::dom::{Dom, NodeId};

/// What kind of form control an element is.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Control {
    Text,
    Password,
    TextArea,
    Checkbox,
    Radio,
    Select,
    /// `<input type=submit>` / `<button>` (default type) / `type=image`.
    Submit,
    Reset,
    /// A button that does nothing by itself (`type=button`).
    Button,
    Hidden,
    /// `type=range`: a slider.
    Range,
    /// `type=color`: a color swatch.
    Color,
    /// `type=file` (no file can be chosen here).
    File,
}

impl Control {
    /// Controls whose value is edited as text.
    pub fn is_text(self) -> bool {
        matches!(self, Control::Text | Control::Password | Control::TextArea)
    }

    /// `<input>`s drawn as push buttons (their label is their value).
    pub fn is_input_button(self) -> bool {
        matches!(self, Control::Submit | Control::Reset | Control::Button)
    }
}

/// The control kind of element `node`, if it is one.
pub fn control(dom: &Dom, node: NodeId) -> Option<Control> {
    let ty = dom
        .attr(node, "type")
        .unwrap_or("")
        .trim()
        .to_ascii_lowercase();
    Some(match dom.tag(node)? {
        "input" => match ty.as_str() {
            "checkbox" => Control::Checkbox,
            "radio" => Control::Radio,
            "submit" | "image" => Control::Submit,
            "reset" => Control::Reset,
            "button" => Control::Button,
            "hidden" => Control::Hidden,
            "password" => Control::Password,
            "range" => Control::Range,
            "color" => Control::Color,
            "file" => Control::File,
            _ => Control::Text,
        },
        "textarea" => Control::TextArea,
        "select" => Control::Select,
        "button" => match ty.as_str() {
            "reset" => Control::Reset,
            "button" => Control::Button,
            _ => Control::Submit,
        },
        _ => return None,
    })
}

/// Checked and selected state of the document's controls (text values
/// live in the document's text fields).
#[derive(Debug, Default)]
pub struct FormState {
    checked: HashMap<NodeId, bool>,
    selected: HashMap<NodeId, usize>,
    /// `<select>` whose option list is open.
    pub open_select: Option<NodeId>,
}

impl FormState {
    pub fn checked(&self, dom: &Dom, node: NodeId) -> bool {
        self.checked
            .get(&node)
            .copied()
            .unwrap_or_else(|| dom.attr(node, "checked").is_some())
    }

    /// Checks or unchecks `node`; checking a radio button unchecks the
    /// others in its group (same name, same form).
    pub fn set_checked(&mut self, dom: &Dom, node: NodeId, value: bool) {
        if value && control(dom, node) == Some(Control::Radio) {
            let name = dom.attr(node, "name").unwrap_or("");
            let form = form_of(dom, node);
            for other in dom.descendants(Dom::ROOT) {
                if other != node
                    && control(dom, other) == Some(Control::Radio)
                    && !name.is_empty()
                    && dom.attr(other, "name") == Some(name)
                    && form_of(dom, other) == form
                {
                    self.checked.insert(other, false);
                }
            }
        }
        self.checked.insert(node, value);
    }

    pub fn selected_index(&self, dom: &Dom, select: NodeId) -> Option<usize> {
        let options = options(dom, select);
        if options.is_empty() {
            return None;
        }
        let i = self.selected.get(&select).copied().unwrap_or_else(|| {
            options
                .iter()
                .position(|&o| dom.attr(o, "selected").is_some())
                .unwrap_or(0)
        });
        Some(i.min(options.len() - 1))
    }

    pub fn set_selected(&mut self, select: NodeId, index: usize) {
        self.selected.insert(select, index);
    }

    /// Restores every control in `form` to its initial state.
    pub fn reset(&mut self, dom: &Dom, form: NodeId) {
        for n in dom.descendants(form) {
            self.checked.remove(&n);
            self.selected.remove(&n);
        }
    }
}

/// `<option>`s of a `<select>` (including those in `<optgroup>`s).
pub fn options(dom: &Dom, select: NodeId) -> Vec<NodeId> {
    dom.descendants(select)
        .into_iter()
        .filter(|&n| dom.tag(n) == Some("option"))
        .collect()
}

/// The value of an `<option>`: its `value` attribute or its text.
pub fn option_value(dom: &Dom, option: NodeId) -> String {
    dom.attr(option, "value")
        .map(str::to_string)
        .unwrap_or_else(|| option_label(dom, option))
}

/// The text shown for an `<option>`.
pub fn option_label(dom: &Dom, option: NodeId) -> String {
    let text = dom.text_content(option);
    text.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// The `<form>` containing `node`.
pub fn form_of(dom: &Dom, node: NodeId) -> Option<NodeId> {
    let mut n = dom.parent(node);
    while let Some(p) = n {
        if dom.tag(p) == Some("form") {
            return Some(p);
        }
        n = dom.parent(p);
    }
    None
}

/// The control a `<label>` is for: its `for` target or the first control
/// inside it.
pub fn label_target(dom: &Dom, label: NodeId) -> Option<NodeId> {
    if let Some(id) = dom.attr(label, "for") {
        return dom.element_by_id(id).filter(|&t| control(dom, t).is_some());
    }
    dom.descendants(label)
        .into_iter()
        .find(|&n| n != label && control(dom, n).is_some())
}

/// `application/x-www-form-urlencoded` encoding of name/value pairs.
pub fn urlencode(pairs: &[(String, String)]) -> String {
    let enc = |s: &str| {
        let mut out = String::new();
        for b in s.bytes() {
            match b {
                b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'*' => {
                    out.push(b as char)
                }
                b' ' => out.push('+'),
                b => out.push_str(&format!("%{b:02X}")),
            }
        }
        out
    };
    pairs
        .iter()
        .map(|(n, v)| format!("{}={}", enc(n), enc(v)))
        .collect::<Vec<_>>()
        .join("&")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::dom::parse_html;

    #[test]
    fn kinds_radio_groups_and_options() {
        let d = parse_html(
            r#"<form><input id=t><input id=c type=checkbox checked>
               <input id=r1 type=radio name=g checked><input id=r2 type=radio name=g>
               <select id=s><option>One<option value=2 selected>Two</select>
               <button id=b>Go</button><label id=l for=t>T</label></form>"#,
        );
        let id = |s: &str| d.element_by_id(s).unwrap();
        assert_eq!(control(&d, id("t")), Some(Control::Text));
        assert_eq!(control(&d, id("b")), Some(Control::Submit));
        let mut f = FormState::default();
        assert!(f.checked(&d, id("c")));
        assert!(f.checked(&d, id("r1")));
        f.set_checked(&d, id("r2"), true);
        assert!(!f.checked(&d, id("r1")), "radio group is exclusive");
        assert_eq!(f.selected_index(&d, id("s")), Some(1));
        assert_eq!(option_value(&d, options(&d, id("s"))[0]), "One");
        assert_eq!(label_target(&d, id("l")), Some(id("t")));
        assert_eq!(form_of(&d, id("t")), d.find_tag(Dom::ROOT, "form"));
        f.reset(&d, d.find_tag(Dom::ROOT, "form").unwrap());
        assert!(f.checked(&d, id("r1")));
    }

    #[test]
    fn url_encoding() {
        let pairs = vec![("q".into(), "a b&c".into()), ("x".into(), "é".into())];
        assert_eq!(urlencode(&pairs), "q=a+b%26c&x=%C3%A9");
    }
}
