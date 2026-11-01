//! CSS selectors: parsing, specificity and matching (Selectors Level 3,
//! plus `:not()`, `:is()`, `:where()`, `:has()` and `:nth-child(… of S)`).

use crate::dom::{Dom, NodeId};

/// What matching needs to know besides the tree.
pub trait Context {
    fn dom(&self) -> &Dom;
    fn is_hovered(&self, node: NodeId) -> bool;
    fn is_active(&self, node: NodeId) -> bool;
    fn is_focused(&self, node: NodeId) -> bool;
    fn is_checked(&self, node: NodeId) -> bool;
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Combinator {
    Descendant,
    Child,
    Next,
    Subsequent,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum PseudoElement {
    Before,
    After,
    Marker,
    Placeholder,
    /// Anything else (`::selection`, `::-webkit-scrollbar`, …): never drawn.
    Other,
}

#[derive(Clone, Debug, PartialEq)]
enum AttrOp {
    Exists,
    Equals(String),
    Includes(String),
    DashMatch(String),
    Prefix(String),
    Suffix(String),
    Substring(String),
}

#[derive(Clone, Debug, PartialEq)]
struct AttrSel {
    name: String,
    op: AttrOp,
    case_insensitive: bool,
}

#[derive(Clone, Debug, PartialEq)]
enum Pseudo {
    /// `:nth-child(an+b [of S])`, `:nth-last-child`, `*-of-type`.
    Nth {
        a: i32,
        b: i32,
        last: bool,
        of_type: bool,
        of: Option<Vec<Selector>>,
    },
    Root,
    Empty,
    Not(Vec<Selector>),
    Is(Vec<Selector>),
    Where(Vec<Selector>),
    Has(Vec<(Combinator, Selector)>),
    Hover,
    Active,
    Focus,
    FocusWithin,
    Link,
    Checked,
    Disabled,
    Enabled,
    Required,
    Optional,
    PlaceholderShown,
    /// Valid syntax this engine does not support: never matches.
    Never,
}

#[derive(Clone, Debug, Default, PartialEq)]
struct Compound {
    tag: Option<String>,
    id: Option<String>,
    classes: Vec<String>,
    attrs: Vec<AttrSel>,
    pseudos: Vec<Pseudo>,
}

/// A complex selector, stored right to left: `parts[0]` is the subject.
#[derive(Clone, Debug, PartialEq)]
pub struct Selector {
    parts: Vec<(Compound, Option<Combinator>)>,
    pub pseudo_element: Option<PseudoElement>,
    pub specificity: [u16; 3],
}

impl Selector {
    /// Bucket keys for indexing: the subject's id, first class and tag.
    pub fn key(&self) -> Key {
        let c = &self.parts[0].0;
        if let Some(id) = &c.id {
            Key::Id(id.clone())
        } else if let Some(cl) = c.classes.first() {
            Key::Class(cl.clone())
        } else if let Some(t) = &c.tag {
            Key::Tag(t.clone())
        } else {
            Key::Any
        }
    }
}

/// Where a rule is filed in the cascade index.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub enum Key {
    Id(String),
    Class(String),
    Tag(String),
    Any,
}

// ----- parsing -------------------------------------------------------------

struct Parser<'a> {
    s: &'a [u8],
    src: &'a str,
    i: usize,
}

fn is_name(c: u8) -> bool {
    c.is_ascii_alphanumeric() || c == b'-' || c == b'_' || c >= 0x80 || c == b'\\'
}

impl<'a> Parser<'a> {
    fn peek(&self) -> Option<u8> {
        self.s.get(self.i).copied()
    }

    fn ws(&mut self) -> bool {
        let start = self.i;
        while let Some(c) = self.peek() {
            if c.is_ascii_whitespace() {
                self.i += 1;
            } else if c == b'/' && self.s.get(self.i + 1) == Some(&b'*') {
                match self.src[self.i + 2..].find("*/") {
                    Some(e) => self.i += e + 4,
                    None => self.i = self.s.len(),
                }
            } else {
                break;
            }
        }
        self.i > start
    }

    fn eat(&mut self, c: u8) -> bool {
        if self.peek() == Some(c) {
            self.i += 1;
            true
        } else {
            false
        }
    }

    /// An identifier, with CSS escapes decoded.
    fn ident(&mut self) -> Option<String> {
        let mut out = String::new();
        while let Some(c) = self.peek() {
            if c == b'\\' {
                self.i += 1;
                let hex_start = self.i;
                while self.i - hex_start < 6 && self.peek().is_some_and(|c| c.is_ascii_hexdigit()) {
                    self.i += 1;
                }
                if self.i > hex_start {
                    let code = u32::from_str_radix(&self.src[hex_start..self.i], 16).ok()?;
                    out.push(char::from_u32(code).unwrap_or('\u{fffd}'));
                    if self.peek().is_some_and(|c| c.is_ascii_whitespace()) {
                        self.i += 1;
                    }
                } else {
                    let ch = self.src[self.i..].chars().next()?;
                    out.push(ch);
                    self.i += ch.len_utf8();
                }
            } else if is_name(c) {
                let ch = self.src[self.i..].chars().next()?;
                out.push(ch);
                self.i += ch.len_utf8();
            } else {
                break;
            }
        }
        (!out.is_empty()).then_some(out)
    }

    fn string(&mut self) -> Option<String> {
        let q = self.peek()?;
        if q != b'"' && q != b'\'' {
            return self.ident();
        }
        self.i += 1;
        let mut out = String::new();
        while let Some(c) = self.peek() {
            self.i += 1;
            if c == q {
                return Some(out);
            }
            if c == b'\\' {
                if let Some(ch) = self.src[self.i..].chars().next() {
                    out.push(ch);
                    self.i += ch.len_utf8();
                }
            } else {
                let ch = self.src[self.i - 1..].chars().next()?;
                out.push(ch);
                self.i += ch.len_utf8() - 1;
            }
        }
        None
    }

    /// Text up to the matching `)`.
    fn parenthesized(&mut self) -> Option<&'a str> {
        let start = self.i;
        let mut depth = 1;
        while let Some(c) = self.peek() {
            match c {
                b'(' => depth += 1,
                b')' => {
                    depth -= 1;
                    if depth == 0 {
                        let inner = &self.src[start..self.i];
                        self.i += 1;
                        return Some(inner);
                    }
                }
                b'"' | b'\'' => {
                    self.string()?;
                    continue;
                }
                _ => {}
            }
            self.i += 1;
        }
        None
    }

    fn attr(&mut self) -> Option<AttrSel> {
        self.ws();
        let name = self.ident()?.to_ascii_lowercase();
        self.ws();
        if self.eat(b']') {
            return Some(AttrSel {
                name,
                op: AttrOp::Exists,
                case_insensitive: false,
            });
        }
        let op_char = self.peek()?;
        let kind = if op_char == b'=' {
            self.i += 1;
            '='
        } else {
            self.i += 1;
            if !self.eat(b'=') {
                return None;
            }
            op_char as char
        };
        self.ws();
        let value = self.string()?;
        self.ws();
        let mut case_insensitive = false;
        if matches!(self.peek(), Some(b'i' | b'I')) {
            case_insensitive = true;
            self.i += 1;
            self.ws();
        } else if matches!(self.peek(), Some(b's' | b'S')) {
            self.i += 1;
            self.ws();
        }
        if !self.eat(b']') {
            return None;
        }
        let op = match kind {
            '=' => AttrOp::Equals(value),
            '~' => AttrOp::Includes(value),
            '|' => AttrOp::DashMatch(value),
            '^' => AttrOp::Prefix(value),
            '$' => AttrOp::Suffix(value),
            '*' => AttrOp::Substring(value),
            _ => return None,
        };
        Some(AttrSel {
            name,
            op,
            case_insensitive,
        })
    }

    /// A pseudo-class after `:` (or a pseudo-element after `::`).
    fn pseudo(
        &mut self,
        compound: &mut Compound,
        element: &mut Option<PseudoElement>,
    ) -> Option<()> {
        let double = self.eat(b':');
        let name = self.ident()?.to_ascii_lowercase();
        let legacy_element = matches!(
            name.as_str(),
            "before" | "after" | "first-line" | "first-letter"
        );
        if double || legacy_element {
            *element = Some(match name.as_str() {
                "before" => PseudoElement::Before,
                "after" => PseudoElement::After,
                "marker" => PseudoElement::Marker,
                "placeholder" => PseudoElement::Placeholder,
                _ => PseudoElement::Other,
            });
            if self.eat(b'(') {
                self.parenthesized()?;
            }
            return Some(());
        }
        let p = if self.eat(b'(') {
            let arg = self.parenthesized()?;
            match name.as_str() {
                "not" => Pseudo::Not(parse_list(arg)?),
                "is" | "matches" | "-webkit-any" | "-moz-any" => {
                    Pseudo::Is(parse_list_forgiving(arg))
                }
                "where" => Pseudo::Where(parse_list_forgiving(arg)),
                "has" => Pseudo::Has(parse_relative(arg)?),
                "nth-child" | "nth-last-child" | "nth-of-type" | "nth-last-of-type" => {
                    let (expr, of) = match arg.find(" of ") {
                        Some(k) if name.ends_with("child") => {
                            (&arg[..k], Some(parse_list(&arg[k + 4..])?))
                        }
                        _ => (arg, None),
                    };
                    let (a, b) = parse_nth(expr)?;
                    Pseudo::Nth {
                        a,
                        b,
                        last: name.contains("last"),
                        of_type: name.ends_with("of-type"),
                        of,
                    }
                }
                _ => Pseudo::Never,
            }
        } else {
            let nth = |a, b, last, of_type| Pseudo::Nth {
                a,
                b,
                last,
                of_type,
                of: None,
            };
            match name.as_str() {
                "first-child" => nth(0, 1, false, false),
                "last-child" => nth(0, 1, true, false),
                "first-of-type" => nth(0, 1, false, true),
                "last-of-type" => nth(0, 1, true, true),
                "only-child" => {
                    compound.pseudos.push(nth(0, 1, false, false));
                    nth(0, 1, true, false)
                }
                "only-of-type" => {
                    compound.pseudos.push(nth(0, 1, false, true));
                    nth(0, 1, true, true)
                }
                "root" => Pseudo::Root,
                "empty" => Pseudo::Empty,
                "hover" => Pseudo::Hover,
                "active" => Pseudo::Active,
                "focus" | "focus-visible" => Pseudo::Focus,
                "focus-within" => Pseudo::FocusWithin,
                "link" | "any-link" | "-webkit-any-link" => Pseudo::Link,
                "visited" | "target" | "indeterminate" | "invalid" | "fullscreen" | "modal"
                | "popover-open" | "open" | "defined-never" => Pseudo::Never,
                "checked" => Pseudo::Checked,
                "disabled" => Pseudo::Disabled,
                "enabled" => Pseudo::Enabled,
                "required" => Pseudo::Required,
                "optional" => Pseudo::Optional,
                "placeholder-shown" => Pseudo::PlaceholderShown,
                "valid" | "defined" | "read-write" | "scope" => return Some(()),
                _ => Pseudo::Never,
            }
        };
        compound.pseudos.push(p);
        Some(())
    }

    fn compound(&mut self, element: &mut Option<PseudoElement>) -> Option<Compound> {
        let mut c = Compound::default();
        let start = self.i;
        if self.eat(b'*') {
            // universal
        } else if self.peek().is_some_and(is_name) {
            let t = self.ident()?;
            c.tag = Some(t.to_ascii_lowercase());
        }
        // Namespace prefix `ns|tag` / `*|tag`: drop the namespace.
        if self.peek() == Some(b'|') && self.s.get(self.i + 1) != Some(&b'=') {
            self.i += 1;
            c.tag = if self.eat(b'*') {
                None
            } else {
                Some(self.ident()?.to_ascii_lowercase())
            };
        }
        loop {
            match self.peek() {
                Some(b'#') => {
                    self.i += 1;
                    c.id = Some(self.ident()?);
                }
                Some(b'.') => {
                    self.i += 1;
                    c.classes.push(self.ident()?);
                }
                Some(b'[') => {
                    self.i += 1;
                    c.attrs.push(self.attr()?);
                }
                Some(b':') => {
                    self.i += 1;
                    self.pseudo(&mut c, element)?;
                }
                _ => break,
            }
        }
        (self.i > start).then_some(c)
    }

    /// One complex selector; stops at `,` or the end.
    fn complex(&mut self) -> Option<Selector> {
        let mut parts: Vec<(Compound, Option<Combinator>)> = Vec::new();
        let mut element = None;
        self.ws();
        let mut pending: Option<Combinator> = None;
        loop {
            if element.is_some() && self.peek().is_some_and(|c| c != b',') {
                // Nothing may follow a pseudo-element except more pseudos.
                let spaced = self.ws();
                if self.peek().is_some_and(|c| c != b',') || spaced && self.peek().is_some() {
                    return None;
                }
            }
            let c = self.compound(&mut element)?;
            parts.push((c, pending.take()));
            let spaced = self.ws();
            match self.peek() {
                None | Some(b',') | Some(b')') => break,
                Some(b'>') => {
                    self.i += 1;
                    pending = Some(Combinator::Child);
                }
                Some(b'+') => {
                    self.i += 1;
                    pending = Some(Combinator::Next);
                }
                Some(b'~') => {
                    self.i += 1;
                    pending = Some(Combinator::Subsequent);
                }
                Some(_) if spaced => pending = Some(Combinator::Descendant),
                Some(_) => return None,
            }
            self.ws();
        }
        if pending.is_some() || parts.is_empty() {
            return None;
        }
        // Store right to left; each part keeps the combinator to its left.
        parts.reverse();
        let n = parts.len();
        let combinators: Vec<Option<Combinator>> = parts.iter().map(|p| p.1).collect();
        for k in 0..n {
            // After reversing, the combinator linking part k to part k+1
            // (further left) was stored on part k (it was on its right).
            parts[k].1 = if k + 1 < n { combinators[k] } else { None };
        }
        let specificity = specificity(&parts, element.is_some());
        Some(Selector {
            parts,
            pseudo_element: element,
            specificity,
        })
    }
}

fn specificity(parts: &[(Compound, Option<Combinator>)], element: bool) -> [u16; 3] {
    let mut s = [0u16; 3];
    for (c, _) in parts {
        let add = |s: &mut [u16; 3], o: [u16; 3]| {
            for i in 0..3 {
                s[i] = s[i].saturating_add(o[i]);
            }
        };
        if c.id.is_some() {
            s[0] += 1;
        }
        s[1] = s[1].saturating_add((c.classes.len() + c.attrs.len()) as u16);
        if c.tag.is_some() {
            s[2] += 1;
        }
        for p in &c.pseudos {
            let max =
                |list: &[Selector]| list.iter().map(|x| x.specificity).max().unwrap_or([0; 3]);
            match p {
                Pseudo::Where(_) => {}
                Pseudo::Not(l) | Pseudo::Is(l) => add(&mut s, max(l)),
                Pseudo::Has(l) => add(
                    &mut s,
                    l.iter().map(|(_, x)| x.specificity).max().unwrap_or([0; 3]),
                ),
                Pseudo::Nth { of: Some(l), .. } => {
                    add(&mut s, max(l));
                    s[1] += 1;
                }
                _ => s[1] += 1,
            }
        }
    }
    if element {
        s[2] += 1;
    }
    s
}

/// Parses `an+b` (also `odd`, `even`, `3`, `-n+2`).
fn parse_nth(expr: &str) -> Option<(i32, i32)> {
    let e: String = expr
        .chars()
        .filter(|c| !c.is_whitespace())
        .collect::<String>()
        .to_ascii_lowercase();
    match e.as_str() {
        "odd" => return Some((2, 1)),
        "even" => return Some((2, 0)),
        _ => {}
    }
    match e.find('n') {
        None => Some((0, e.parse().ok()?)),
        Some(k) => {
            let a = match &e[..k] {
                "" | "+" => 1,
                "-" => -1,
                n => n.parse().ok()?,
            };
            let rest = &e[k + 1..];
            let b = if rest.is_empty() {
                0
            } else {
                rest.parse().ok()?
            };
            Some((a, b))
        }
    }
}

/// Parses a selector list; `None` if any selector is invalid.
pub fn parse_list(src: &str) -> Option<Vec<Selector>> {
    let mut p = Parser {
        s: src.as_bytes(),
        src,
        i: 0,
    };
    let mut out = Vec::new();
    loop {
        out.push(p.complex()?);
        p.ws();
        if !p.eat(b',') {
            break;
        }
    }
    p.ws();
    (p.i == src.len()).then_some(out)
}

/// Like [`parse_list`], but drops invalid selectors (`:is`, `:where`).
fn parse_list_forgiving(src: &str) -> Vec<Selector> {
    split_top_level(src)
        .into_iter()
        .filter_map(|s| parse_list(s).and_then(|mut v| v.pop()))
        .collect()
}

/// `:has()` arguments: relative selectors such as `> img` or `.x`.
fn parse_relative(src: &str) -> Option<Vec<(Combinator, Selector)>> {
    let mut out = Vec::new();
    for part in split_top_level(src) {
        let t = part.trim();
        let (comb, rest) = match t.chars().next()? {
            '>' => (Combinator::Child, &t[1..]),
            '+' => (Combinator::Next, &t[1..]),
            '~' => (Combinator::Subsequent, &t[1..]),
            _ => (Combinator::Descendant, t),
        };
        out.push((comb, parse_list(rest)?.pop()?));
    }
    Some(out)
}

/// Splits on commas that are not inside parentheses, brackets or strings.
fn split_top_level(src: &str) -> Vec<&str> {
    let mut out = Vec::new();
    let (mut depth, mut start, mut quote) = (0i32, 0, None);
    for (i, c) in src.char_indices() {
        match (quote, c) {
            (Some(q), c) if c == q => quote = None,
            (Some(_), _) => {}
            (None, '"' | '\'') => quote = Some(c),
            (None, '(' | '[') => depth += 1,
            (None, ')' | ']') => depth -= 1,
            (None, ',') if depth == 0 => {
                out.push(&src[start..i]);
                start = i + 1;
            }
            _ => {}
        }
    }
    out.push(&src[start..]);
    out
}

// ----- matching --------------------------------------------------------------

fn attr_matches(dom: &Dom, n: NodeId, a: &AttrSel) -> bool {
    let Some(v) = dom.attr(n, &a.name) else {
        return false;
    };
    let eq = |x: &str, y: &str| {
        if a.case_insensitive {
            x.eq_ignore_ascii_case(y)
        } else {
            x == y
        }
    };
    let lower = |s: &str| {
        if a.case_insensitive {
            s.to_ascii_lowercase()
        } else {
            s.to_string()
        }
    };
    match &a.op {
        AttrOp::Exists => true,
        AttrOp::Equals(w) => eq(v, w),
        AttrOp::Includes(w) => v.split_ascii_whitespace().any(|p| eq(p, w)),
        AttrOp::DashMatch(w) => eq(v, w) || lower(v).starts_with(&format!("{}-", lower(w))),
        AttrOp::Prefix(w) => !w.is_empty() && lower(v).starts_with(&lower(w)),
        AttrOp::Suffix(w) => !w.is_empty() && lower(v).ends_with(&lower(w)),
        AttrOp::Substring(w) => !w.is_empty() && lower(v).contains(&lower(w)),
    }
}

fn element_siblings(dom: &Dom, n: NodeId) -> Vec<NodeId> {
    match dom.parent(n) {
        Some(p) => dom
            .children(p)
            .iter()
            .copied()
            .filter(|&c| dom.element(c).is_some())
            .collect(),
        None => vec![n],
    }
}

fn is_control(dom: &Dom, n: NodeId) -> bool {
    matches!(
        dom.tag(n),
        Some("input" | "button" | "select" | "textarea" | "option" | "fieldset")
    )
}

fn compound_matches<C: Context>(cx: &C, c: &Compound, n: NodeId) -> bool {
    let dom = cx.dom();
    let Some(e) = dom.element(n) else {
        return false;
    };
    if let Some(t) = &c.tag
        && e.name != *t
    {
        return false;
    }
    if let Some(id) = &c.id
        && e.id() != Some(id.as_str())
    {
        return false;
    }
    if !c.classes.iter().all(|cl| e.has_class(cl)) {
        return false;
    }
    if !c.attrs.iter().all(|a| attr_matches(dom, n, a)) {
        return false;
    }
    c.pseudos.iter().all(|p| pseudo_matches(cx, p, n))
}

fn pseudo_matches<C: Context>(cx: &C, p: &Pseudo, n: NodeId) -> bool {
    let dom = cx.dom();
    match p {
        Pseudo::Nth {
            a,
            b,
            last,
            of_type,
            of,
        } => {
            let mut sibs = element_siblings(dom, n);
            if *of_type {
                let tag = dom.tag(n);
                sibs.retain(|&s| dom.tag(s) == tag);
            }
            if let Some(list) = of {
                sibs.retain(|&s| list.iter().any(|sel| matches(cx, sel, s)));
            }
            if *last {
                sibs.reverse();
            }
            let Some(pos) = sibs.iter().position(|&s| s == n) else {
                return false;
            };
            let k = pos as i32 + 1;
            if *a == 0 {
                k == *b
            } else {
                (k - b) % a == 0 && (k - b) / a >= 0
            }
        }
        Pseudo::Root => dom.parent(n) == Some(Dom::ROOT),
        Pseudo::Empty => dom
            .children(n)
            .iter()
            .all(|&c| dom.element(c).is_none() && dom.text(c).is_none_or(str::is_empty)),
        Pseudo::Not(list) => !list.iter().any(|s| matches(cx, s, n)),
        Pseudo::Is(list) | Pseudo::Where(list) => list.iter().any(|s| matches(cx, s, n)),
        Pseudo::Has(list) => list.iter().any(|(comb, sel)| has(cx, *comb, sel, n)),
        Pseudo::Hover => cx.is_hovered(n),
        Pseudo::Active => cx.is_active(n),
        Pseudo::Focus => cx.is_focused(n),
        Pseudo::FocusWithin => dom.descendants(n).into_iter().any(|d| cx.is_focused(d)),
        Pseudo::Link => matches!(dom.tag(n), Some("a" | "area")) && dom.attr(n, "href").is_some(),
        Pseudo::Checked => {
            cx.is_checked(n) || (dom.tag(n) == Some("option") && dom.attr(n, "selected").is_some())
        }
        Pseudo::Disabled => is_control(dom, n) && dom.attr(n, "disabled").is_some(),
        Pseudo::Enabled => is_control(dom, n) && dom.attr(n, "disabled").is_none(),
        Pseudo::Required => is_control(dom, n) && dom.attr(n, "required").is_some(),
        Pseudo::Optional => is_control(dom, n) && dom.attr(n, "required").is_none(),
        Pseudo::PlaceholderShown => {
            dom.attr(n, "placeholder").is_some() && dom.attr(n, "value").is_none_or(str::is_empty)
        }
        Pseudo::Never => false,
    }
}

/// `:has(comb sel)` relative to `n`.
fn has<C: Context>(cx: &C, comb: Combinator, sel: &Selector, n: NodeId) -> bool {
    let dom = cx.dom();
    match comb {
        Combinator::Descendant => dom
            .descendants(n)
            .into_iter()
            .skip(1)
            .any(|d| matches(cx, sel, d)),
        Combinator::Child => dom.children(n).iter().any(|&c| matches(cx, sel, c)),
        Combinator::Next | Combinator::Subsequent => {
            let sibs = element_siblings(dom, n);
            let Some(i) = sibs.iter().position(|&s| s == n) else {
                return false;
            };
            let after = &sibs[i + 1..];
            if comb == Combinator::Next {
                after.first().is_some_and(|&s| matches(cx, sel, s))
            } else {
                after.iter().any(|&s| matches(cx, sel, s))
            }
        }
    }
}

/// What a change of hover/active/focus on an element can restyle.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum DynScope {
    /// The element and its descendants.
    Subtree,
    /// Its parent's subtree (sibling combinators follow).
    Parent,
}

/// State bits for [`Selector::dynamic_parts`].
pub const HOVER: u8 = 1;
pub const ACTIVE: u8 = 2;
pub const FOCUS: u8 = 4;

/// Which element states (`HOVER` | `ACTIVE` | `FOCUS`) `p` depends on.
fn state_bits(p: &Pseudo) -> u8 {
    let sel_bits = |s: &Selector| {
        s.parts
            .iter()
            .flat_map(|(c, _)| c.pseudos.iter())
            .fold(0, |m, p| m | state_bits(p))
    };
    match p {
        Pseudo::Hover => HOVER,
        Pseudo::Active => ACTIVE,
        Pseudo::Focus | Pseudo::FocusWithin => FOCUS,
        Pseudo::Not(l) | Pseudo::Is(l) | Pseudo::Where(l) => {
            l.iter().fold(0, |m, s| m | sel_bits(s))
        }
        Pseudo::Has(l) => l.iter().fold(0, |m, (_, s)| m | sel_bits(s)),
        _ => 0,
    }
}

fn is_dynamic(p: &Pseudo) -> bool {
    state_bits(p) != 0
}

/// A state-dependent compound of a selector (see
/// [`Selector::dynamic_parts`]).
#[derive(Clone, Debug)]
pub struct DynPart {
    /// The compound and its left context, state conditions removed: an
    /// element whose state changed matters only if this matches it.
    pub context: Selector,
    /// For a compound left of the subject: the whole selector without
    /// state conditions, to find the elements that may restyle.
    pub subject: Option<Selector>,
    pub scope: DynScope,
    /// `HOVER` | `ACTIVE` | `FOCUS`.
    pub bits: u8,
}

impl Selector {
    /// For each compound that depends on element state: that compound
    /// without the state conditions (to test which elements' state changes
    /// matter) and how far the effect reaches.
    pub fn dynamic_parts(&self) -> Vec<DynPart> {
        let mut out = Vec::new();
        for (i, (c, _)) in self.parts.iter().enumerate() {
            let bits = c.pseudos.iter().fold(0, |m, p| m | state_bits(p));
            if bits == 0 {
                continue;
            }
            let strip = |c: &Compound| Compound {
                pseudos: c
                    .pseudos
                    .iter()
                    .filter(|p| !is_dynamic(p))
                    .cloned()
                    .collect(),
                ..c.clone()
            };
            // The compound with its context to the left (its ancestors and
            // preceding siblings), every state condition removed.
            let context: Vec<(Compound, Option<Combinator>)> = self.parts[i..]
                .iter()
                .map(|(c, comb)| (strip(c), *comb))
                .collect();
            // Combinators between this compound and the subject.
            let siblings = self.parts[..i]
                .iter()
                .any(|(_, comb)| matches!(comb, Some(Combinator::Next | Combinator::Subsequent)));
            let has = c.pseudos.iter().any(|p| matches!(p, Pseudo::Has(_)));
            let scope = if siblings || has {
                DynScope::Parent
            } else {
                DynScope::Subtree
            };
            let subject = (i > 0).then(|| Selector {
                parts: self
                    .parts
                    .iter()
                    .map(|(c, comb)| (strip(c), *comb))
                    .collect(),
                pseudo_element: None,
                specificity: [0; 3],
            });
            out.push(DynPart {
                context: Selector {
                    parts: context,
                    pseudo_element: None,
                    specificity: [0; 3],
                },
                subject,
                scope,
                bits,
            });
        }
        out
    }
}

/// Whether `sel` matches element `n` (ignoring any pseudo-element part).
pub fn matches<C: Context>(cx: &C, sel: &Selector, n: NodeId) -> bool {
    match_from(cx, &sel.parts, 0, n)
}

fn match_from<C: Context>(
    cx: &C,
    parts: &[(Compound, Option<Combinator>)],
    k: usize,
    n: NodeId,
) -> bool {
    let (compound, comb) = &parts[k];
    if !compound_matches(cx, compound, n) {
        return false;
    }
    let Some(comb) = comb else {
        return true;
    };
    let dom = cx.dom();
    match comb {
        Combinator::Child => dom
            .parent(n)
            .is_some_and(|p| match_from(cx, parts, k + 1, p)),
        Combinator::Descendant => {
            let mut p = dom.parent(n);
            while let Some(a) = p {
                if dom.element(a).is_none() {
                    return false;
                }
                if match_from(cx, parts, k + 1, a) {
                    return true;
                }
                p = dom.parent(a);
            }
            false
        }
        Combinator::Next => dom
            .previous_element_sibling(n)
            .is_some_and(|s| match_from(cx, parts, k + 1, s)),
        Combinator::Subsequent => {
            let mut s = dom.previous_element_sibling(n);
            while let Some(x) = s {
                if match_from(cx, parts, k + 1, x) {
                    return true;
                }
                s = dom.previous_element_sibling(x);
            }
            false
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::dom::parse_html;

    struct Cx<'a>(&'a Dom, Option<NodeId>);

    impl Context for Cx<'_> {
        fn dom(&self) -> &Dom {
            self.0
        }
        fn is_hovered(&self, n: NodeId) -> bool {
            self.1.is_some_and(|h| self.0.is_inclusive_ancestor(n, h))
        }
        fn is_active(&self, _: NodeId) -> bool {
            false
        }
        fn is_focused(&self, _: NodeId) -> bool {
            false
        }
        fn is_checked(&self, n: NodeId) -> bool {
            self.0.attr(n, "checked").is_some()
        }
    }

    fn check(html: &str, sel: &str, id: &str) -> bool {
        let d = parse_html(html);
        let n = d.element_by_id(id).unwrap();
        let list = parse_list(sel).unwrap_or_else(|| panic!("parse {sel}"));
        list.iter().any(|s| matches(&Cx(&d, None), s, n))
    }

    const DOC: &str = r#"<div id=root class="a b"><ul id=list>
        <li id=l1 class=item>1</li><li id=l2 class="item odd">2</li>
        <li id=l3 class=item data-x="foo-bar">3</li><li id=l4>4</li></ul>
        <p id=p1>t</p><span id=s1></span><a id=a1 href="https://x.org/a.PDF">x</a>
        <input id=i1 type=checkbox checked><input id=i2 disabled></div>"#;

    #[test]
    fn combinators() {
        assert!(check(DOC, "div ul > li", "l1"));
        assert!(!check(DOC, "div > li", "l1"));
        assert!(check(DOC, "#l1 + li", "l2"));
        assert!(check(DOC, "#l1 ~ li", "l4"));
        assert!(!check(DOC, "#l2 ~ li", "l1"));
        assert!(check(DOC, "ul ~ p", "p1"));
        assert!(check(DOC, ".a.b li.item", "l3"));
    }

    #[test]
    fn attributes() {
        assert!(check(DOC, "[data-x]", "l3"));
        assert!(check(DOC, "[data-x|=foo]", "l3"));
        assert!(check(DOC, "[data-x^=fo]", "l3"));
        assert!(check(DOC, "[data-x$='bar']", "l3"));
        assert!(check(DOC, "[data-x*=o-b]", "l3"));
        assert!(check(DOC, "a[href$='.pdf' i]", "a1"));
        assert!(!check(DOC, "a[href$='.pdf']", "a1"));
        assert!(check(DOC, "[class~=odd]", "l2"));
    }

    #[test]
    fn structural_pseudo_classes() {
        assert!(check(DOC, "li:first-child", "l1"));
        assert!(check(DOC, "li:last-child", "l4"));
        assert!(check(DOC, "li:nth-child(2n)", "l4"));
        assert!(check(DOC, "li:nth-child(odd)", "l3"));
        assert!(check(DOC, "li:nth-last-child(1)", "l4"));
        assert!(check(DOC, "li:nth-child(2 of .item)", "l2"));
        assert!(check(DOC, "span:empty", "s1"));
        assert!(check(DOC, "p:first-of-type", "p1"));
        assert!(check(DOC, ":root > body > div", "root"));
        assert!(!check(DOC, "li:only-child", "l1"));
    }

    #[test]
    fn logical_pseudo_classes() {
        assert!(check(DOC, "li:not(.odd)", "l1"));
        assert!(!check(DOC, "li:not(.odd, #l1)", "l1"));
        assert!(check(DOC, ":is(p, li).item", "l1"));
        assert!(check(DOC, "li:where(.nope, .odd)", "l2"));
        assert!(check(DOC, "ul:has(> li.odd)", "list"));
        assert!(check(DOC, "div:has(a[href])", "root"));
        assert!(!check(DOC, "ul:has(+ span)", "list"));
        assert!(check(DOC, "input:checked", "i1"));
        assert!(check(DOC, "input:disabled", "i2"));
        assert!(check(DOC, "input:enabled", "i1"));
        assert!(!check(DOC, "li:unknown-thing", "l1"));
    }

    #[test]
    fn specificity_and_pseudo_elements() {
        let s = |x: &str| parse_list(x).unwrap()[0].clone();
        assert_eq!(s("#a .b c").specificity, [1, 1, 1]);
        assert_eq!(s("li:not(#x)").specificity, [1, 0, 1]);
        assert_eq!(s(":where(#x) p").specificity, [0, 0, 1]);
        assert_eq!(s("a::before").pseudo_element, Some(PseudoElement::Before));
        assert_eq!(s("a:after").pseudo_element, Some(PseudoElement::After));
        assert_eq!(
            s("::-webkit-scrollbar").pseudo_element,
            Some(PseudoElement::Other)
        );
        assert_eq!(s(".x").key(), Key::Class("x".into()));
        assert_eq!(s(r".md\:flex").key(), Key::Class("md:flex".into()));
        assert!(parse_list("a >").is_none());
        assert!(parse_list("a::before span").is_none());
    }
}
