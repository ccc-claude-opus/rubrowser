//! Stylesheet syntax: rules, at-rules and declarations.
//!
//! A forgiving parser in the spirit of CSS Syntax Level 3: it skips what it
//! does not understand and keeps going. `@media` and `@supports` blocks are
//! kept (media is evaluated per viewport, supports at parse time), `@layer`
//! blocks are unwrapped, `@import` is recorded, other at-rules are dropped.

use std::rc::Rc;

use super::media::MediaList;
use super::selector::{Selector, parse_list};

/// One `name: value` declaration.
#[derive(Clone, Debug, PartialEq)]
pub struct Decl {
    /// Lowercase name; custom properties (`--x`) keep their case.
    pub name: String,
    pub value: String,
    pub important: bool,
}

#[derive(Clone, Debug)]
pub enum Rule {
    Style {
        selectors: Rc<[Selector]>,
        decls: Rc<[Decl]>,
    },
    Media(MediaList, Vec<Rule>),
    Import(String, MediaList),
    /// `@layer name { … }` (anonymous layers get a unique name). Nested
    /// layer names are relative to the enclosing one.
    Layer(String, Vec<Rule>),
    /// `@layer a, b;`: declares layer order.
    LayerOrder(Vec<String>),
    /// `@property --name { initial-value: … }`.
    Property(String, String),
    /// `@font-face { … }` descriptors.
    FontFace(Rc<[Decl]>),
    /// `@container [name] <condition> { … }`.
    Container(Rc<super::ContainerQuery>, Vec<Rule>),
    /// `@keyframes name { … }`.
    Keyframes(String, Rc<[super::animation::Keyframe]>),
}

/// A parsed stylesheet.
#[derive(Clone, Debug, Default)]
pub struct Sheet {
    pub rules: Vec<Rule>,
}

struct Scanner<'a> {
    src: &'a str,
    b: &'a [u8],
    i: usize,
}

impl<'a> Scanner<'a> {
    fn new(src: &'a str) -> Self {
        Scanner {
            src,
            b: src.as_bytes(),
            i: 0,
        }
    }

    fn skip_ws(&mut self) {
        loop {
            if self.i >= self.b.len() {
                self.i = self.b.len();
                return;
            }
            while self.i < self.b.len() && self.b[self.i].is_ascii_whitespace() {
                self.i += 1;
            }
            if self.src[self.i..].starts_with("/*") {
                self.i = self.src[self.i + 2..]
                    .find("*/")
                    .map_or(self.b.len(), |e| self.i + e + 4);
            } else if self.src[self.i..].starts_with("<!--") {
                self.i += 4;
            } else if self.src[self.i..].starts_with("-->") {
                self.i += 3;
            } else {
                return;
            }
        }
    }

    /// Skips a string starting at the current quote.
    fn skip_string(&mut self) {
        let q = self.b[self.i];
        self.i += 1;
        while self.i < self.b.len() {
            match self.b[self.i] {
                b'\\' => self.i += 2,
                c if c == q => {
                    self.i += 1;
                    return;
                }
                b'\n' => return,
                _ => self.i += 1,
            }
        }
    }

    /// Text up to (not including) one of `stops` at nesting depth 0.
    fn until(&mut self, stops: &[u8]) -> &'a str {
        let start = self.i;
        let mut depth = 0i32;
        while self.i < self.b.len() {
            let c = self.b[self.i];
            match c {
                b'"' | b'\'' => {
                    self.skip_string();
                    continue;
                }
                b'\\' => {
                    self.i += 2;
                    continue;
                }
                b'/' if self.b.get(self.i + 1) == Some(&b'*') => {
                    self.i = self.src[self.i + 2..]
                        .find("*/")
                        .map_or(self.b.len(), |e| self.i + e + 4);
                    continue;
                }
                b'(' | b'[' => depth += 1,
                b')' | b']' => depth -= 1,
                _ if depth <= 0 && stops.contains(&c) => break,
                _ => {}
            }
            self.i += 1;
        }
        let end = self.i.min(self.b.len());
        &self.src[start..end]
    }

    /// The contents of a `{…}` block (the scanner is on the `{`).
    fn block(&mut self) -> &'a str {
        self.i += 1;
        let start = self.i;
        let mut depth = 1;
        while self.i < self.b.len() {
            match self.b[self.i] {
                b'"' | b'\'' => {
                    self.skip_string();
                    continue;
                }
                b'\\' => {
                    self.i += 2;
                    continue;
                }
                b'/' if self.b.get(self.i + 1) == Some(&b'*') => {
                    self.i = self.src[self.i + 2..]
                        .find("*/")
                        .map_or(self.b.len(), |e| self.i + e + 4);
                    continue;
                }
                b'{' => depth += 1,
                b'}' => {
                    depth -= 1;
                    if depth == 0 {
                        let inner = &self.src[start..self.i];
                        self.i += 1;
                        return inner;
                    }
                }
                _ => {}
            }
            self.i += 1;
        }
        &self.src[start.min(self.b.len())..]
    }
}

/// Parses a stylesheet. `base` (the sheet's URL) resolves `url(…)` values.
pub fn parse_stylesheet(src: &str, base: Option<&str>) -> Sheet {
    Sheet {
        rules: parse_rules(src, base),
    }
}

fn parse_rules(src: &str, base: Option<&str>) -> Vec<Rule> {
    let mut out = Vec::new();
    let mut s = Scanner::new(src);
    loop {
        s.skip_ws();
        if s.i >= s.b.len() {
            break;
        }
        if s.b[s.i] == b'}' || s.b[s.i] == b';' {
            s.i += 1;
            continue;
        }
        if s.b[s.i] == b'@' {
            s.i += 1;
            let name_end = s.src[s.i..]
                .find(|c: char| !(c.is_ascii_alphanumeric() || c == '-'))
                .map_or(s.b.len(), |e| s.i + e);
            let name = s.src[s.i..name_end].to_ascii_lowercase();
            s.i = name_end;
            let prelude = s.until(b"{;").trim();
            let has_block = s.i < s.b.len() && s.b[s.i] == b'{';
            let body = if has_block {
                s.block()
            } else {
                s.i += 1;
                ""
            };
            match name.as_str() {
                "media" if has_block => out.push(Rule::Media(
                    MediaList::parse(prelude),
                    parse_rules(body, base),
                )),
                "supports" if has_block => {
                    if super::media::supports(prelude) {
                        out.extend(parse_rules(body, base));
                    }
                }
                "layer" if has_block => {
                    let name = if prelude.is_empty() {
                        use std::sync::atomic::{AtomicU32, Ordering};
                        static ANON: AtomicU32 = AtomicU32::new(0);
                        format!("\u{0}{}", ANON.fetch_add(1, Ordering::Relaxed))
                    } else {
                        prelude.to_string()
                    };
                    out.push(Rule::Layer(name, parse_rules(body, base)));
                }
                "layer" => out.push(Rule::LayerOrder(
                    prelude.split(',').map(|n| n.trim().to_string()).collect(),
                )),
                "font-face" if has_block => {
                    let (decls, _) = parse_block(body, base);
                    out.push(Rule::FontFace(decls.into()));
                }
                "property" if has_block => {
                    let (decls, _) = parse_block(body, base);
                    if let Some(d) = decls.iter().rev().find(|d| d.name == "initial-value") {
                        out.push(Rule::Property(prelude.to_string(), d.value.to_string()));
                    }
                }
                "keyframes" | "-webkit-keyframes" if has_block => {
                    let (_, blocks) = parse_block(body, base);
                    let blocks: Vec<(&str, Vec<Decl>)> = blocks
                        .into_iter()
                        .map(|(sel, decls)| (sel, parse_block(decls, base).0))
                        .collect();
                    let name = prelude.trim().trim_matches(['"', '\'']).to_string();
                    out.push(Rule::Keyframes(name, super::animation::parse_keyframes(&blocks)));
                }
                "container" if has_block => {
                    if let Some(q) = super::ContainerQuery::parse(prelude) {
                        out.push(Rule::Container(Rc::new(q), parse_rules(body, base)));
                    }
                }
                "scope" | "-moz-document" if has_block => {
                    out.extend(parse_rules(body, base));
                }
                "import" => {
                    if let Some((url, rest)) = import_target(prelude) {
                        let url = resolve_url(&url, base);
                        out.push(Rule::Import(url, MediaList::parse(rest)));
                    }
                }
                _ => {}
            }
            continue;
        }
        let prelude = s.until(b"{");
        if s.i >= s.b.len() {
            break;
        }
        let body = s.block();
        let Some(selectors) = parse_list(prelude.trim()) else {
            continue; // invalid selector: the whole rule is dropped
        };
        let (decls, nested) = parse_block(body, base);
        if !decls.is_empty() {
            out.push(Rule::Style {
                selectors: selectors.clone().into(),
                decls: decls.into(),
            });
        }
        // CSS nesting: `a { & b {…} }` or `a { b {…} }`.
        for (sel, inner) in nested {
            let combined = nest_selector(prelude.trim(), sel);
            for mut r in parse_rules(&format!("{combined} {{{inner}}}"), base) {
                if let Rule::Style { .. } = &mut r {
                    out.push(r);
                }
            }
        }
    }
    out
}

/// Prefixes nested selectors with the parent (`&` replaced by it).
fn nest_selector(parent: &str, nested: &str) -> String {
    nested
        .split(',')
        .map(|n| {
            let n = n.trim();
            if n.contains('&') {
                n.replace('&', &format!(":is({parent})"))
            } else {
                format!(":is({parent}) {n}")
            }
        })
        .collect::<Vec<_>>()
        .join(", ")
}

/// `url("x") screen` / `"x" screen` → (x, "screen").
fn import_target(prelude: &str) -> Option<(String, &str)> {
    let p = prelude.trim();
    if let Some(rest) = p.strip_prefix("url(") {
        let end = rest.find(')')?;
        let url = rest[..end].trim().trim_matches(['"', '\'']).to_string();
        return Some((url, &rest[end + 1..]));
    }
    let q = p.chars().next()?;
    if q == '"' || q == '\'' {
        let end = p[1..].find(q)? + 1;
        return Some((p[1..end].to_string(), &p[end + 1..]));
    }
    None
}

/// Parses a declaration block (or a `style` attribute).
pub fn parse_declarations(src: &str, base: Option<&str>) -> Vec<Decl> {
    parse_block(src, base).0
}

/// Declarations and nested rules (selector, body) of a block.
fn parse_block<'a>(src: &'a str, base: Option<&str>) -> (Vec<Decl>, Vec<(&'a str, &'a str)>) {
    let mut decls = Vec::new();
    let mut nested = Vec::new();
    let mut s = Scanner::new(src);
    loop {
        s.skip_ws();
        if s.i >= s.b.len() {
            break;
        }
        let item = s.until(b";{}");
        if s.i < s.b.len() && s.b[s.i] == b'{' {
            let body = s.block();
            if !item.trim_start().starts_with('@') {
                nested.push((item.trim(), body));
            }
            continue;
        }
        s.i += 1;
        if let Some(d) = declaration(item, base) {
            decls.push(d);
        }
    }
    (decls, nested)
}

fn declaration(item: &str, base: Option<&str>) -> Option<Decl> {
    let colon = item.find(':')?;
    let raw_name = item[..colon].trim();
    if raw_name.is_empty() || raw_name.contains(char::is_whitespace) {
        return None;
    }
    let name = if raw_name.starts_with("--") {
        raw_name.to_string()
    } else {
        raw_name.to_ascii_lowercase()
    };
    let mut value = item[colon + 1..].trim();
    let mut important = false;
    if let Some(bang) = value.rfind('!') {
        let tail = value[bang + 1..].trim();
        if tail.eq_ignore_ascii_case("important") {
            important = true;
            value = value[..bang].trim_end();
        }
    }
    let value = if value.contains("url(") {
        rewrite_urls(value, base)
    } else {
        value.to_string()
    };
    Some(Decl {
        name,
        value,
        important,
    })
}

/// Makes every `url(…)` in `value` absolute against `base`.
fn rewrite_urls(value: &str, base: Option<&str>) -> String {
    let mut out = String::with_capacity(value.len());
    let mut rest = value;
    while let Some(k) = rest.find("url(") {
        out.push_str(&rest[..k + 4]);
        rest = &rest[k + 4..];
        let end = rest.find(')').unwrap_or(rest.len());
        let inner = rest[..end].trim().trim_matches(['"', '\'']);
        out.push('"');
        out.push_str(&resolve_url(inner, base).replace('"', "%22"));
        out.push('"');
        rest = &rest[end..];
    }
    out.push_str(rest);
    out
}

fn resolve_url(url: &str, base: Option<&str>) -> String {
    match base {
        Some(b) if !url.starts_with("data:") && !url.starts_with('#') => {
            crate::net::url::resolve(b, url)
        }
        _ => url.to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn style_rules(s: &Sheet) -> Vec<(String, Vec<Decl>)> {
        fn walk(rules: &[Rule], out: &mut Vec<(String, Vec<Decl>)>) {
            for r in rules {
                match r {
                    Rule::Style { selectors, decls } => {
                        out.push((format!("{} selector(s)", selectors.len()), decls.to_vec()))
                    }
                    Rule::Media(_, inner) | Rule::Layer(_, inner) | Rule::Container(_, inner) => {
                        walk(inner, out)
                    }
                    Rule::Import(..)
                    | Rule::LayerOrder(_)
                    | Rule::Property(..)
                    | Rule::FontFace(_)
                    | Rule::Keyframes(..) => {}
                }
            }
        }
        let mut out = Vec::new();
        walk(&s.rules, &mut out);
        out
    }

    #[test]
    fn rules_at_rules_and_comments() {
        let css = r#"
            /* comment { not a rule } */
            @charset "utf-8";
            @import url("base.css") screen;
            a, b { color: red; background: url(img/x.png) no-repeat }
            @media (min-width: 100px) { p { margin: 0 !important } }
            @supports (display: grid) { .g { display: grid } }
            @supports (-webkit-nonsense: 1) { .no { color: blue } }
            @layer base { em { font-style: normal } }
            @font-face { font-family: X; src: url(x.woff) }
            @keyframes spin { from { opacity: 0 } to { opacity: 1 } }
            .x::-webkit-scrollbar { width: 0 }
            li:bogus(1) { color: red }
            li..x { color: red }
            div { content: "a;b}c"; --Brand-Color: #123 }
        "#;
        let sheet = parse_stylesheet(css, Some("https://site.test/css/main.css"));
        assert!(
            matches!(&sheet.rules[0], Rule::Import(u, _) if u == "https://site.test/css/base.css")
        );
        let rules = style_rules(&sheet);
        let names: Vec<Vec<&str>> = rules
            .iter()
            .map(|(_, d)| d.iter().map(|d| d.name.as_str()).collect())
            .collect();
        assert_eq!(rules[0].0, "2 selector(s)");
        assert_eq!(
            rules[0].1[1].value,
            r#"url("https://site.test/css/img/x.png") no-repeat"#
        );
        assert!(rules[1].1[0].important);
        assert!(names.contains(&vec!["display"]), "@supports grid kept");
        assert!(
            !rules
                .iter()
                .any(|(_, d)| d.iter().any(|d| d.value == "blue"))
        );
        assert!(names.contains(&vec!["font-style"]), "@layer unwrapped");
        assert!(
            !names.iter().any(|n| n.contains(&"src")),
            "@font-face dropped"
        );
        let last = rules.last().unwrap();
        assert_eq!(last.1[0].value, "\"a;b}c\"");
        assert_eq!(last.1[1].name, "--Brand-Color");
    }

    #[test]
    fn nesting_and_inline_styles() {
        let sheet = parse_stylesheet(
            ".card { color: red; & .t { color: blue } p { margin: 0 } }",
            None,
        );
        assert_eq!(style_rules(&sheet).len(), 3);
        let d = parse_declarations("color: red; width:10px ;; bad; x:", None);
        assert_eq!(d.len(), 3);
        assert_eq!(d[1].value, "10px");
    }
}
