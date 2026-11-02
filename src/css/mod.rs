//! CSS: stylesheets, selector matching, cascade and computed styles.
//!
//! Stylesheets are parsed once ([`syntax`]) and indexed by the key of each
//! selector's subject (id, class, tag) so an element only tests the rules
//! that could match it. Media queries are evaluated against the viewport
//! when the index is built. Custom properties are inherited and `var()` is
//! substituted when a declaration is applied.

pub mod animation;
pub mod computed;
pub mod gradient;
pub mod media;
pub mod selector;
pub mod style;
pub mod syntax;
pub mod transform;
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

/// Extra UA rules for quirks-mode documents (as in other browsers).
const QUIRKS_CSS: &str = "
table { white-space: normal; line-height: normal; font-weight: normal; font-size: medium;
  font-style: normal; text-align: start; }
";

thread_local! {
    static QUIRKS_SHEET: Rc<Sheet> = Rc::new(syntax::parse_stylesheet(QUIRKS_CSS, None));
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
    /// Size containers found by the last layout (for `@container`).
    pub containers: &'a Containers,
}

/// A size container (`container-type: size | inline-size`) after layout.
#[derive(Clone, Debug, PartialEq)]
pub struct Container {
    pub names: Vec<String>,
    /// Content box size.
    pub width: f32,
    pub height: f32,
}

pub type Containers = BTreeMap<NodeId, Container>;

/// No containers (before the first layout, and outside documents).
pub static NO_CONTAINERS: Containers = BTreeMap::new();

/// The prelude of an `@container` rule: an optional container name and a
/// size condition (media-query syntax over the container's size).
#[derive(Debug, PartialEq)]
pub struct ContainerQuery {
    name: Option<String>,
    condition: MediaList,
}

impl ContainerQuery {
    pub fn parse(prelude: &str) -> Option<ContainerQuery> {
        let p = prelude.trim();
        let first = p
            .split(|c: char| c.is_whitespace() || c == '(')
            .next()
            .unwrap_or("");
        let named = !first.is_empty() && !matches!(first.to_ascii_lowercase().as_str(), "not");
        let (name, rest) = if named {
            (Some(first.to_string()), p[first.len()..].trim())
        } else {
            (None, p)
        };
        // Logical sizes are the physical ones in horizontal writing.
        let rest = rest
            .to_ascii_lowercase()
            .replace("inline-size", "width")
            .replace("block-size", "height");
        Some(ContainerQuery {
            name,
            condition: MediaList::parse(&rest),
        })
    }

    /// Whether the nearest matching ancestor container of `n` satisfies
    /// the condition (false without one).
    fn matches(&self, cx: &Matcher, n: NodeId) -> bool {
        let mut at = cx.dom.parent(n);
        while let Some(a) = at {
            if let Some(c) = cx.containers.get(&a)
                && self.name.as_ref().is_none_or(|name| c.names.contains(name))
            {
                let env = MediaEnv {
                    width: c.width,
                    height: c.height,
                    scale: 1.0,
                    dark: false,
                };
                return self.condition.matches(&env);
            }
            at = cx.dom.parent(a);
        }
        false
    }
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

/// Scripts' selectors: `None` if `selector` is invalid (scripts throw a
/// `SyntaxError`), else which of `nodes` it matches, parsed once.
pub fn query(
    dom: &Dom,
    nodes: impl IntoIterator<Item = NodeId>,
    selector: &str,
    state: &ElementState,
) -> Option<Vec<NodeId>> {
    let list = selector::parse_list_strict(selector)?;
    let no = |_: NodeId| false;
    let cx = Matcher {
        dom,
        state: *state,
        checked: &no,
        containers: &NO_CONTAINERS,
    };
    Some(
        nodes
            .into_iter()
            .filter(|&n| {
                dom.element(n).is_some()
                    && list
                        .iter()
                        .any(|s| s.pseudo_element.is_none() && selector::matches(&cx, s, n))
            })
            .collect(),
    )
}

/// Whether `selector` matches element `id`. Used by scripts' `query`.
pub fn matches(dom: &Dom, id: NodeId, selector: &str, state: &ElementState) -> bool {
    let no = |_: NodeId| false;
    let cx = Matcher {
        dom,
        state: *state,
        checked: &no,
        containers: &NO_CONTAINERS,
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
    /// Cascade layer rank ([`UNLAYERED`] for rules outside layers).
    layer: u16,
    order: u32,
    /// The `@container` queries around the rule, all of which must hold.
    containers: Rc<[Rc<ContainerQuery>]>,
}

/// Layer rank of rules outside any `@layer` (they beat layered ones).
const UNLAYERED: u16 = u16::MAX;

/// Every rule that applies at one viewport, filed by selector key.
pub struct Cascade {
    rules: Vec<Indexed>,
    /// Layer names (dotted paths) in order of first appearance.
    layers: HashMap<String, u16>,
    /// Initial values of custom properties registered with `@property`.
    initial_vars: BTreeMap<String, String>,
    buckets: HashMap<Key, Vec<u32>>,
    /// The state-dependent compounds of all rules (see
    /// [`Selector::dynamic_parts`]).
    pub dynamic: Vec<selector::DynPart>,
    /// The `@container` queries enclosing the rules being added.
    within: Vec<Rc<ContainerQuery>>,
    /// Whether any rule depends on a container query.
    pub has_container_rules: bool,
    /// `@keyframes` by name (the last definition wins).
    pub keyframes: HashMap<String, Rc<[animation::Keyframe]>>,
}

impl Cascade {
    /// Indexes the UA sheet and `author` sheets (in order, with the media
    /// list they were included with) for `env`.
    pub fn new(author: &[(Rc<Sheet>, MediaList)], env: &MediaEnv) -> Cascade {
        Self::with_quirks(author, env, false)
    }

    /// Like [`Cascade::new`]; `quirks` adds the quirks-mode UA rules.
    pub fn with_quirks(author: &[(Rc<Sheet>, MediaList)], env: &MediaEnv, quirks: bool) -> Cascade {
        let mut c = Cascade {
            rules: Vec::new(),
            layers: HashMap::new(),
            initial_vars: BTreeMap::new(),
            buckets: HashMap::new(),
            dynamic: Vec::new(),
            within: Vec::new(),
            has_container_rules: false,
            keyframes: HashMap::new(),
        };
        let ua = ua_sheet();
        c.add(&ua.rules, 0, env, "");
        if quirks {
            let q = QUIRKS_SHEET.with(Rc::clone);
            c.add(&q.rules, 0, env, "");
        }
        for (sheet, media) in author {
            if media.matches(env) {
                c.add(&sheet.rules, 2, env, "");
            }
        }
        c
    }

    /// The rank of layer `path`, registering it on first sight.
    fn layer(&mut self, path: &str) -> u16 {
        let next = self.layers.len().min(UNLAYERED as usize - 1) as u16;
        *self.layers.entry(path.to_string()).or_insert(next)
    }

    /// Adds `rules` inside layer `layer` (a dotted path; "" = unlayered).
    fn add(&mut self, rules: &[Rule], origin: u8, env: &MediaEnv, layer: &str) {
        let rank = if layer.is_empty() {
            UNLAYERED
        } else {
            self.layer(layer)
        };
        let child = |name: &str| {
            if layer.is_empty() {
                name.to_string()
            } else {
                format!("{layer}.{name}")
            }
        };
        for r in rules {
            match r {
                Rule::Style { selectors, decls } => {
                    for sel in selectors.iter() {
                        self.dynamic.extend(sel.dynamic_parts());
                        let order = self.rules.len() as u32;
                        self.buckets.entry(sel.key()).or_default().push(order);
                        self.rules.push(Indexed {
                            selector: sel.clone(),
                            decls: decls.clone(),
                            origin,
                            layer: rank,
                            order,
                            containers: self.within.clone().into(),
                        });
                    }
                }
                Rule::Media(list, inner) => {
                    if list.matches(env) {
                        self.add(inner, origin, env, layer);
                    }
                }
                Rule::Layer(name, inner) => self.add(inner, origin, env, &child(name)),
                Rule::LayerOrder(names) => {
                    for n in names {
                        self.layer(&child(n));
                    }
                }
                Rule::Property(name, value) => {
                    self.initial_vars.insert(name.clone(), value.clone());
                }
                Rule::Container(query, inner) => {
                    self.has_container_rules = true;
                    self.within.push(query.clone());
                    self.add(inner, origin, env, layer);
                    self.within.pop();
                }
                Rule::Keyframes(name, frames) => {
                    self.keyframes.insert(name.clone(), frames.clone());
                }
                Rule::Import(..) | Rule::FontFace(_) => {}
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
    /// Generated content boxes. Box `i` has the synthetic node id
    /// `dom.len() + 2 * i` and its text `dom.len() + 2 * i + 1`; both have
    /// entries in `nodes`.
    pub generated: Vec<Generated>,
}

/// A `::before` / `::after` box with its evaluated `content`.
#[derive(Clone, Debug, PartialEq)]
pub struct Generated {
    pub owner: NodeId,
    pub after: bool,
    pub text: String,
}

/// One declaration in cascade order.
struct Matched<'a> {
    rank: (u8, u16, [u16; 3], u32),
    name: &'a str,
    value: &'a str,
}

/// Cascade rank: normal declarations by origin; `!important` ones above
/// all normal ones, with the user agent's on top. Within an origin, later
/// layers beat earlier ones and unlayered rules beat all layers; for
/// `!important` that order is reversed.
fn rank(
    important: bool,
    origin: u8,
    layer: u16,
    specificity: [u16; 3],
    order: u32,
) -> (u8, u16, [u16; 3], u32) {
    let level = if important {
        10 + match origin {
            0 => 3,
            3 => 2,
            _ => 1,
        }
    } else {
        origin
    };
    let layer = if important { UNLAYERED - layer } else { layer };
    (level, layer, specificity, order)
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
        generated: Vec::new(),
    };
    let mut w = Walker::new(dom, cascade, cx, env, 16.0);
    let root_style = Style {
        vars: Rc::new(cascade.initial_vars.clone()),
        ..Style::default()
    };
    let mut gens = Vec::new();
    let mut visited = Vec::new();
    w.walk(
        Dom::ROOT,
        Rc::new(root_style),
        &mut out.nodes,
        &mut out.pseudo,
        &mut gens,
        None,
        &mut visited,
    );
    for (g, st) in gens {
        // Drop boxes whose owner is not rendered (or has no box to hold them).
        let shown = out.nodes[g.owner].is_some();
        let mut text = st.clone();
        text.display = style::Display::Inline;
        out.nodes.push(shown.then_some(st));
        out.nodes.push(shown.then_some(text));
        out.generated.push(g);
    }
    out
}

/// Result of [`restyle_subtree`].
#[derive(Debug, PartialEq)]
pub enum Restyle {
    Unchanged,
    Changed,
    /// The subtree's generated content changed: compute everything.
    NeedsFull,
}

/// Recomputes the styles of `root` and its descendants in `styles` (after
/// an element-state change such as hover), reusing its parent's style.
pub fn restyle_subtree(
    dom: &Dom,
    cascade: &Cascade,
    cx: &Matcher,
    env: &MediaEnv,
    root: NodeId,
    styles: &mut Styles,
) -> Restyle {
    let Some(parent) = dom.parent(root) else {
        return Restyle::NeedsFull;
    };
    let Some(parent_style) = styles.nodes.get(parent).cloned().flatten() else {
        return Restyle::NeedsFull;
    };
    let root_font = dom
        .html()
        .and_then(|h| styles.nodes.get(h).cloned().flatten())
        .map_or(16.0, |s| s.font_size);
    let mut nodes: Vec<Option<Style>> = vec![None; dom.len()];
    let mut pseudo = HashMap::new();
    let mut gens = Vec::new();
    let mut subtree = Vec::new();
    let mut w = Walker::new(dom, cascade, cx, env, root_font);
    let prev = Some(&styles.nodes[..]);
    w.walk(
        root,
        Rc::new(parent_style),
        &mut nodes,
        &mut pseudo,
        &mut gens,
        prev,
        &mut subtree,
    );
    // Generated boxes must stay the same (they have fixed node ids).
    let in_tree: std::collections::HashSet<NodeId> = subtree.iter().copied().collect();
    let old: Vec<(usize, &Generated)> = styles
        .generated
        .iter()
        .enumerate()
        .filter(|(_, g)| in_tree.contains(&g.owner))
        .collect();
    if old.len() != gens.len() || old.iter().zip(&gens).any(|((_, a), (b, _))| *a != b) {
        return Restyle::NeedsFull;
    }
    let mut changed = false;
    for &n in &subtree {
        if styles.nodes[n] != nodes[n] {
            changed = true;
            styles.nodes[n] = nodes[n].take();
        }
        for pe in [
            PseudoElement::Before,
            PseudoElement::After,
            PseudoElement::Marker,
        ] {
            let new = pseudo.remove(&(n, pe));
            if styles.pseudo.get(&(n, pe)) != new.as_ref() {
                changed = true;
                match new {
                    Some(p) => styles.pseudo.insert((n, pe), p),
                    None => styles.pseudo.remove(&(n, pe)),
                };
            }
        }
    }
    let base = dom.len();
    for ((i, g), (_, st)) in old.iter().zip(gens) {
        let shown = styles.nodes[g.owner].is_some();
        let mut text = st.clone();
        text.display = style::Display::Inline;
        let (bi, ti) = (base + 2 * i, base + 2 * i + 1);
        let (b, t) = (shown.then_some(st), shown.then_some(text));
        if styles.nodes[bi] != b || styles.nodes[ti] != t {
            changed = true;
            styles.nodes[bi] = b;
            styles.nodes[ti] = t;
        }
    }
    if changed {
        Restyle::Changed
    } else {
        Restyle::Unchanged
    }
}

/// Walks a subtree in document order computing styles.
struct Walker<'a> {
    dom: &'a Dom,
    cascade: &'a Cascade,
    cx: &'a Matcher<'a>,
    venv: ValueEnv,
    counters: HashMap<String, i32>,
    candidates: Vec<u32>,
}

impl<'a> Walker<'a> {
    fn new(
        dom: &'a Dom,
        cascade: &'a Cascade,
        cx: &'a Matcher<'a>,
        env: &MediaEnv,
        root_font: f32,
    ) -> Self {
        Walker {
            dom,
            cascade,
            cx,
            venv: ValueEnv {
                root_font,
                vw: env.width,
                vh: env.height,
            },
            counters: HashMap::new(),
            candidates: Vec::new(),
        }
    }

    /// Styles `start` (whose parent's style is `parent`) and everything
    /// under it into `nodes` / `pseudo`; generated boxes go to `gens`.
    #[allow(clippy::too_many_arguments)]
    fn walk(
        &mut self,
        start: NodeId,
        parent: Rc<Style>,
        nodes: &mut [Option<Style>],
        pseudo: &mut HashMap<(NodeId, PseudoElement), Style>,
        gens: &mut Vec<(Generated, Style)>,
        prev: Option<&[Option<Style>]>,
        visited: &mut Vec<NodeId>,
    ) {
        let (dom, cascade, cx) = (self.dom, self.cascade, self.cx);
        // Document order, parents before children.
        let mut stack: Vec<(NodeId, Rc<Style>)> = vec![(start, parent)];
        while let Some((id, parent)) = stack.pop() {
            visited.push(id);
            let mut style = if dom.element(id).is_some() {
                cascade.candidates(dom, id, &mut self.candidates);
                let cands = &self.candidates;
                let s = cascade_element(dom, id, cascade, cands, cx, &parent, &self.venv, None);
                if dom.tag(id) == Some("html") {
                    self.venv.root_font = s.font_size;
                }
                if s.display != style::Display::None {
                    update_counters(&s, &mut self.counters);
                }
                for pe in [
                    PseudoElement::Before,
                    PseudoElement::After,
                    PseudoElement::Marker,
                ] {
                    let p = cascade_element(dom, id, cascade, cands, cx, &s, &self.venv, Some(pe));
                    if !p.has_rules {
                        continue;
                    }
                    if pe != PseudoElement::Marker && s.display != style::Display::None {
                        update_counters(&p, &mut self.counters);
                        let text = p
                            .content
                            .as_deref()
                            .and_then(|c| generated_text(c, dom, id, &self.counters));
                        if let Some(text) = text.filter(|_| p.display != style::Display::None) {
                            let g = Generated {
                                owner: id,
                                after: pe == PseudoElement::After,
                                text,
                            };
                            gens.push((g, p.clone()));
                        }
                    }
                    pseudo.insert((id, pe), p);
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
            // Restyling after a state change: an unchanged style leaves the
            // descendants as they were.
            let same = id != start && prev.is_some_and(|p| p[id].as_ref() == Some(&style));
            let shared = Rc::new(style);
            if !same {
                for &c in dom.children(id).iter().rev() {
                    stack.push((c, shared.clone()));
                }
            }
            nodes[id] = Some((*shared).clone());
        }
    }
}

/// Applies `counter-reset` then `counter-increment` (document order;
/// counters are not scoped to subtrees).
fn update_counters(s: &Style, counters: &mut HashMap<String, i32>) {
    for (list, default) in [(&s.counter_reset, 0), (&s.counter_increment, 1)] {
        let Some(list) = list.as_deref() else {
            continue;
        };
        let words: Vec<&str> = list.split_whitespace().collect();
        let mut i = 0;
        while i < words.len() {
            let name = words[i];
            if name == "none" {
                break;
            }
            let n = words.get(i + 1).and_then(|w| w.parse::<i32>().ok());
            i += if n.is_some() { 2 } else { 1 };
            let n = n.unwrap_or(default);
            if default == 0 {
                counters.insert(name.to_string(), n);
            } else {
                *counters.entry(name.to_string()).or_insert(0) += n;
            }
        }
    }
}

/// Evaluates a `content` value: strings, `attr()`, `counter()`,
/// `counters()` and quotes. `None` for `none` / `normal` (no box).
pub fn generated_text(
    value: &str,
    dom: &Dom,
    id: NodeId,
    counters: &HashMap<String, i32>,
) -> Option<String> {
    let v = value.trim();
    if v.eq_ignore_ascii_case("none") || v.eq_ignore_ascii_case("normal") || v.is_empty() {
        return None;
    }
    let mut out = String::new();
    let chars: Vec<char> = v.chars().collect();
    let mut i = 0;
    while i < chars.len() {
        let c = chars[i];
        if c.is_whitespace() {
            i += 1;
        } else if c == '"' || c == '\'' {
            i += 1;
            while i < chars.len() && chars[i] != c {
                if chars[i] == '\\' && i + 1 < chars.len() {
                    let start = i + 1;
                    let mut end = start;
                    while end < chars.len() && end - start < 6 && chars[end].is_ascii_hexdigit() {
                        end += 1;
                    }
                    if end > start {
                        let hex: String = chars[start..end].iter().collect();
                        if let Some(ch) =
                            u32::from_str_radix(&hex, 16).ok().and_then(char::from_u32)
                        {
                            out.push(ch);
                        }
                        i = end;
                        if chars.get(i) == Some(&' ') {
                            i += 1;
                        }
                    } else {
                        if chars[start] != '\n' {
                            out.push(chars[start]);
                        }
                        i = start + 1;
                    }
                } else {
                    out.push(chars[i]);
                    i += 1;
                }
            }
            i += 1;
        } else if c == '/' {
            break; // alternative text follows
        } else {
            let start = i;
            while i < chars.len() && !chars[i].is_whitespace() && chars[i] != '(' {
                i += 1;
            }
            let name: String = chars[start..i]
                .iter()
                .collect::<String>()
                .to_ascii_lowercase();
            let mut args = String::new();
            if chars.get(i) == Some(&'(') {
                let mut depth = 0;
                while i < chars.len() {
                    match chars[i] {
                        '(' => depth += 1,
                        ')' => depth -= 1,
                        _ => {}
                    }
                    if depth == 0 {
                        break;
                    }
                    if !(depth == 1 && chars[i] == '(') {
                        args.push(chars[i]);
                    }
                    i += 1;
                }
                i += 1;
            }
            let args: Vec<String> = args
                .split(',')
                .map(|a| a.trim().trim_matches(|c| c == '"' || c == '\'').to_string())
                .collect();
            match name.as_str() {
                "attr" => {
                    let attr = args[0].split_whitespace().next().unwrap_or("");
                    out.push_str(dom.attr(id, attr).unwrap_or(""));
                }
                "counter" | "counters" => {
                    let n = counters.get(args[0].as_str()).copied().unwrap_or(0);
                    let style = if name == "counter" {
                        args.get(1)
                    } else {
                        args.get(2)
                    };
                    out.push_str(&counter_text(n, style.map_or("decimal", |s| s.as_str())));
                }
                "open-quote" => out.push('\u{201C}'),
                "close-quote" => out.push('\u{201D}'),
                _ => {} // url(), no-open-quote, …
            }
        }
    }
    Some(out)
}

/// Formats a counter value in a `list-style-type`.
pub fn counter_text(n: i32, style: &str) -> String {
    let alpha = |base: u8| {
        let mut n = n.max(1) as u32;
        let mut s = Vec::new();
        while n > 0 {
            n -= 1;
            s.push((base + (n % 26) as u8) as char);
            n /= 26;
        }
        s.iter().rev().collect()
    };
    let roman = |upper: bool| {
        let mut n = n.clamp(1, 3999) as u32;
        let mut s = String::new();
        for (v, r) in [
            (1000, "m"),
            (900, "cm"),
            (500, "d"),
            (400, "cd"),
            (100, "c"),
            (90, "xc"),
            (50, "l"),
            (40, "xl"),
            (10, "x"),
            (9, "ix"),
            (5, "v"),
            (4, "iv"),
            (1, "i"),
        ] {
            while n >= v {
                s.push_str(r);
                n -= v;
            }
        }
        if upper { s.to_uppercase() } else { s }
    };
    match style.trim() {
        "none" => String::new(),
        "lower-alpha" | "lower-latin" => alpha(b'a'),
        "upper-alpha" | "upper-latin" => alpha(b'A'),
        "lower-roman" => roman(false),
        "upper-roman" => roman(true),
        "disc" => "\u{2022}".into(),
        "circle" => "\u{25E6}".into(),
        "square" => "\u{25AA}".into(),
        "decimal-leading-zero" => format!("{n:02}"),
        _ => n.to_string(),
    }
}

/// The style of an element that is not rendered (inside or under
/// `display: none`), for `getComputedStyle`: `chain` runs from just below
/// the nearest styled ancestor (whose style is `parent`) down to it.
pub fn unrendered_style(
    cascade: &Cascade,
    cx: &Matcher,
    venv: &ValueEnv,
    parent: &Style,
    chain: &[NodeId],
) -> Style {
    let mut candidates = Vec::new();
    let mut style = parent.clone();
    for &id in chain {
        if cx.dom.element(id).is_some() {
            cascade.candidates(cx.dom, id, &mut candidates);
            style = cascade_element(cx.dom, id, cascade, &candidates, cx, &style, venv, None);
        }
    }
    style
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
        if r.selector.pseudo_element != pe
            || !selector::matches(cx, &r.selector, id)
            || !r.containers.iter().all(|q| q.matches(cx, id))
        {
            continue;
        }
        for d in r.decls.iter() {
            decls.push(Matched {
                rank: rank(
                    d.important,
                    r.origin,
                    r.layer,
                    r.selector.specificity,
                    r.order,
                ),
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
                rank: rank(false, 1, UNLAYERED, [0; 3], base + k as u32),
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
                rank: rank(d.important, 3, UNLAYERED, [0; 3], base + 1000 + k as u32),
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
        containers: &NO_CONTAINERS,
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
        "img" | "video" | "canvas" | "iframe" | "table" | "td" | "th" | "col" | "svg"
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
        "td" | "th"
            | "tr"
            | "tbody"
            | "thead"
            | "div"
            | "p"
            | "h1"
            | "h2"
            | "h3"
            | "h4"
            | "h5"
            | "h6"
            | "caption"
    ) && let Some(a) = dom.attr(id, "align")
    {
        let a = a.trim().to_ascii_lowercase();
        let center_blocks = matches!(tag, "td" | "th" | "tr" | "tbody" | "thead" | "div");
        let a = if a == "center" && center_blocks {
            "-webkit-center".to_string()
        } else {
            a
        };
        out.push(("text-align", a));
    }
    // dir=rtl|ltr|auto (auto: from the first strong character).
    if let Some(d) = dom.attr(id, "dir") {
        let rtl = match d.trim().to_ascii_lowercase().as_str() {
            "rtl" => Some(true),
            "ltr" => Some(false),
            "auto" => dom
                .text_content(id)
                .chars()
                .find(|c| c.is_alphabetic())
                .map(crate::text::is_rtl),
            _ => None,
        };
        if let Some(rtl) = rtl {
            out.push(("direction", if rtl { "rtl" } else { "ltr" }.into()));
        }
    }
    if matches!(tag, "table" | "img" | "iframe")
        && let Some(a) = dom.attr(id, "align")
    {
        match a.trim().to_ascii_lowercase().as_str() {
            "center" if tag == "table" => {
                out.push(("margin-left", "auto".into()));
                out.push(("margin-right", "auto".into()));
            }
            "left" => out.push(("float", "left".into())),
            "right" => out.push(("float", "right".into())),
            _ => {}
        }
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
    fn cascade_layers() {
        let html = r#"<style>
            @layer base, theme;
            @layer theme { p { color: green; padding: 1px } }
            @layer base { p { color: red !important; padding: 2px } }
            p { padding: 3px }
            @layer base { #x { margin: 5px } }
            p { margin: 6px }
            </style><p id=x>a</p>"#;
        let dom = parse_html(html);
        let sheets = author_sheets(&dom, &[]);
        let refs: Vec<&str> = sheets.iter().map(String::as_str).collect();
        let st = compute_styles(&dom, &refs, &ElementState::default());
        let p = dom.find_tag(Dom::ROOT, "p").unwrap();
        let s = st[p].as_ref().unwrap();
        assert_eq!(s.padding[0], Dim::Px(3.0), "unlayered beats layers");
        assert_eq!(s.margin[0], Dim::Px(6.0), "even with lower specificity");
        assert_eq!(
            s.color,
            Rgba::new(255, 0, 0, 255),
            "!important: earlier layer wins"
        );
    }

    #[test]
    fn generated_content_and_counters() {
        let html = r#"<style>
            .q::before { content: "\201C" attr(data-x) " " }
            .q::after { content: none }
            ol { counter-reset: n 4 }
            li { counter-increment: n }
            li::before { content: counter(n, upper-roman) "." }
            </style><p class="q" data-x="hi">a</p><ol><li>x</li><li>y</li></ol>"#;
        let dom = parse_html(html);
        let env = MediaEnv::default();
        let sheet = Rc::new(syntax::parse_stylesheet(
            &author_sheets(&dom, &[]).join("\n"),
            None,
        ));
        let cascade = Cascade::new(&[(sheet, MediaList::default())], &env);
        let no = |_: NodeId| false;
        let cx = Matcher {
            dom: &dom,
            state: ElementState::default(),
            checked: &no,
            containers: &NO_CONTAINERS,
        };
        let st = compute(&dom, &cascade, &cx, &env);
        let texts: Vec<(bool, &str)> = st
            .generated
            .iter()
            .map(|g| (g.after, g.text.as_str()))
            .collect();
        assert_eq!(
            texts,
            [(false, "\u{201C}hi "), (false, "V."), (false, "VI.")]
        );
        assert_eq!(st.nodes.len(), dom.len() + 6);
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
            containers: &NO_CONTAINERS,
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
