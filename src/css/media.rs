//! Media queries (`@media`, `<link media>`) and `@supports` conditions.

/// What media queries are evaluated against.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct MediaEnv {
    /// Viewport size in CSS pixels.
    pub width: f32,
    pub height: f32,
    /// Device pixels per CSS pixel.
    pub scale: f32,
    pub dark: bool,
}

impl Default for MediaEnv {
    fn default() -> Self {
        MediaEnv {
            width: 1280.0,
            height: 800.0,
            scale: 1.0,
            dark: false,
        }
    }
}

/// A comma-separated media query list, kept as text and evaluated on
/// demand (they are short).
#[derive(Clone, Debug, Default, PartialEq)]
pub struct MediaList(String);

impl MediaList {
    pub fn parse(src: &str) -> MediaList {
        MediaList(src.trim().to_ascii_lowercase())
    }

    pub fn matches(&self, env: &MediaEnv) -> bool {
        let s = self.0.trim();
        s.is_empty() || split_commas(s).iter().any(|q| query(q.trim(), env))
    }
}

fn split_commas(s: &str) -> Vec<&str> {
    let (mut out, mut depth, mut start) = (Vec::new(), 0, 0);
    for (i, c) in s.char_indices() {
        match c {
            '(' => depth += 1,
            ')' => depth -= 1,
            ',' if depth == 0 => {
                out.push(&s[start..i]);
                start = i + 1;
            }
            _ => {}
        }
    }
    out.push(&s[start..]);
    out
}

/// One media query: `[not|only] type [and (feature)]…` or a condition.
fn query(q: &str, env: &MediaEnv) -> bool {
    let (negate, rest) = match q.strip_prefix("not ") {
        Some(r) => (true, r.trim()),
        None => (false, q.strip_prefix("only ").unwrap_or(q).trim()),
    };
    let result = if rest.starts_with('(') {
        condition(rest, env)
    } else {
        let (ty, cond) = match rest.find(" and ") {
            Some(k) => (&rest[..k], Some(&rest[k + 5..])),
            None => (rest, None),
        };
        let type_ok = matches!(ty.trim(), "all" | "screen");
        type_ok && cond.is_none_or(|c| condition(c, env))
    };
    result != negate
}

/// `(a) and (b)`, `(a) or (b)`, `not (a)`, nested parentheses.
fn condition(c: &str, env: &MediaEnv) -> bool {
    let c = c.trim();
    if let Some(rest) = c.strip_prefix("not ") {
        return !condition(rest, env);
    }
    let parts = split_top(c);
    if parts.len() > 1 {
        let mut result = in_parens(parts[0], env);
        let mut k = 1;
        while k + 1 < parts.len() {
            let rhs = in_parens(parts[k + 1], env);
            result = if parts[k] == "or" {
                result || rhs
            } else {
                result && rhs
            };
            k += 2;
        }
        return result;
    }
    in_parens(c, env)
}

/// Splits `(a) and (b) or (c)` into operands and operators.
fn split_top(c: &str) -> Vec<&str> {
    let mut out = Vec::new();
    let (mut depth, mut start) = (0, None::<usize>);
    let b = c.as_bytes();
    let mut i = 0;
    while i < b.len() {
        match b[i] {
            b'(' => {
                if depth == 0 {
                    start = Some(i);
                }
                depth += 1;
            }
            b')' => {
                depth -= 1;
                if depth == 0
                    && let Some(s) = start.take()
                {
                    out.push(&c[s..=i]);
                }
            }
            _ if depth == 0 && b[i].is_ascii_alphabetic() => {
                let end = c[i..]
                    .find(|ch: char| !(ch.is_ascii_alphabetic() || ch == '-'))
                    .map_or(c.len(), |e| i + e);
                if c[end..].starts_with('(') {
                    // A function such as `selector(…)`: one operand.
                    let mut d = 0;
                    let mut j = end;
                    for (k, ch) in c[end..].char_indices() {
                        match ch {
                            '(' => d += 1,
                            ')' => {
                                d -= 1;
                                if d == 0 {
                                    j = end + k + 1;
                                    break;
                                }
                            }
                            _ => {}
                        }
                    }
                    out.push(&c[i..j]);
                    i = j;
                    continue;
                }
                out.push(&c[i..end]);
                i = end;
                continue;
            }
            _ => {}
        }
        i += 1;
    }
    out
}

fn in_parens(c: &str, env: &MediaEnv) -> bool {
    let c = c.trim();
    match c.strip_prefix('(').and_then(|x| x.strip_suffix(')')) {
        Some(inner)
            if inner.trim_start().starts_with('(')
                || inner.contains(" and ")
                || inner.contains(" or ")
                || inner.trim_start().starts_with("not ") =>
        {
            condition(inner, env)
        }
        Some(inner) => feature(inner.trim(), env),
        None => false,
    }
}

/// A length in px (for media features: em = 16px).
fn length(v: &str) -> Option<f32> {
    let v = v.trim();
    for (unit, k) in [("px", 1.0), ("em", 16.0), ("rem", 16.0), ("pt", 4.0 / 3.0)] {
        if let Some(n) = v.strip_suffix(unit) {
            return n.trim().parse::<f32>().ok().map(|n| n * k);
        }
    }
    v.parse::<f32>().ok()
}

fn resolution(v: &str) -> Option<f32> {
    let v = v.trim();
    if let Some(n) = v.strip_suffix("dppx").or_else(|| v.strip_suffix('x')) {
        return n.parse().ok();
    }
    if let Some(n) = v.strip_suffix("dpi") {
        return n.parse::<f32>().ok().map(|d| d / 96.0);
    }
    v.parse().ok()
}

/// One feature: `min-width: 600px`, `prefers-color-scheme: dark`,
/// `width >= 600px`, `400px < width < 800px`, `hover`, …
fn feature(f: &str, env: &MediaEnv) -> bool {
    // Range syntax.
    for op in ["<=", ">=", "<", ">", "="] {
        if f.contains(op) && !f.contains(':') {
            return range(f, env);
        }
    }
    let (name, value) = match f.split_once(':') {
        Some((n, v)) => (n.trim(), Some(v.trim())),
        None => (f.trim(), None),
    };
    let (prefix, base) = if let Some(b) = name.strip_prefix("min-") {
        ("min", b)
    } else if let Some(b) = name.strip_prefix("max-") {
        ("max", b)
    } else {
        ("", name)
    };
    let base = base
        .trim_start_matches("-webkit-")
        .trim_start_matches("device-");
    let cmp = |actual: f32, wanted: Option<f32>| match (prefix, wanted) {
        (_, None) => actual > 0.0,
        ("min", Some(w)) => actual >= w,
        ("max", Some(w)) => actual <= w,
        (_, Some(w)) => (actual - w).abs() < 0.5,
    };
    match base {
        "width" => cmp(env.width, value.and_then(length)),
        "height" => cmp(env.height, value.and_then(length)),
        "aspect-ratio" => {
            let wanted = value.and_then(|v| {
                let (a, b) = v.split_once('/')?;
                Some(a.trim().parse::<f32>().ok()? / b.trim().parse::<f32>().ok()?)
            });
            cmp(env.width / env.height.max(1.0), wanted)
        }
        "resolution" | "pixel-ratio" => cmp(env.scale, value.and_then(resolution)),
        "orientation" => {
            let landscape = env.width >= env.height;
            value.is_some_and(|v| (v == "landscape") == landscape)
        }
        "prefers-color-scheme" => value == Some(if env.dark { "dark" } else { "light" }),
        "prefers-reduced-motion" | "prefers-reduced-transparency" | "prefers-reduced-data" => {
            value == Some("no-preference")
        }
        "prefers-contrast" => value == Some("no-preference"),
        "forced-colors" | "inverted-colors" => value == Some("none"),
        "hover" | "any-hover" => value.is_none_or(|v| v == "hover"),
        "pointer" | "any-pointer" => value.is_none_or(|v| v == "fine"),
        "color" => true,
        "monochrome" => value.is_some_and(|v| v == "0"),
        "display-mode" => value == Some("browser"),
        "scripting" => value.is_none_or(|v| v == "enabled"),
        "update" => value.is_none_or(|v| v == "fast"),
        _ => false,
    }
}

/// `width >= 600px` and `600px <= width < 900px`.
fn range(f: &str, env: &MediaEnv) -> bool {
    let tokens: Vec<&str> = {
        let mut out = Vec::new();
        let mut rest = f.trim();
        while !rest.is_empty() {
            // Earliest operator; at the same place the longer one (`<=`).
            let op = ["<=", ">=", "<", ">", "="]
                .iter()
                .filter_map(|op| rest.find(op).map(|k| (k, std::cmp::Reverse(op.len()), *op)))
                .min()
                .map(|(k, _, op)| (k, op));
            match op {
                Some((k, op)) => {
                    out.push(rest[..k].trim());
                    out.push(op);
                    rest = &rest[k + op.len()..];
                }
                None => {
                    out.push(rest.trim());
                    break;
                }
            }
        }
        out
    };
    let value = |t: &str| -> Option<f32> {
        match t {
            "width" => Some(env.width),
            "height" => Some(env.height),
            "aspect-ratio" => Some(env.width / env.height.max(1.0)),
            "resolution" => Some(env.scale),
            _ => length(t).or_else(|| resolution(t)),
        }
    };
    let holds = |a: f32, op: &str, b: f32| match op {
        "<" => a < b,
        "<=" => a <= b,
        ">" => a > b,
        ">=" => a >= b,
        _ => (a - b).abs() < 0.5,
    };
    let mut k = 0;
    while k + 2 < tokens.len() {
        let (Some(a), Some(b)) = (value(tokens[k]), value(tokens[k + 2])) else {
            return false;
        };
        if !holds(a, tokens[k + 1], b) {
            return false;
        }
        k += 2;
    }
    k > 0
}

/// Evaluates an `@supports` condition.
pub fn supports(cond: &str) -> bool {
    let c = cond.trim().to_ascii_lowercase();
    if let Some(rest) = c.strip_prefix("not ") {
        return !supports(rest);
    }
    let parts = split_top(&c);
    if parts.len() > 1 {
        let mut result = supports_one(parts[0]);
        let mut k = 1;
        while k + 1 < parts.len() {
            let rhs = supports_one(parts[k + 1]);
            result = if parts[k] == "or" {
                result || rhs
            } else {
                result && rhs
            };
            k += 2;
        }
        return result;
    }
    supports_one(&c)
}

fn supports_one(c: &str) -> bool {
    let c = c.trim();
    if let Some(sel) = c
        .strip_prefix("selector(")
        .and_then(|s| s.strip_suffix(')'))
    {
        return super::selector::parse_list(sel).is_some();
    }
    let Some(inner) = c.strip_prefix('(').and_then(|s| s.strip_suffix(')')) else {
        return false;
    };
    let inner = inner.trim();
    if inner.starts_with('(') || inner.starts_with("not ") || inner.starts_with("selector(") {
        return supports(inner);
    }
    let Some((prop, value)) = inner.split_once(':') else {
        return false;
    };
    let (prop, value) = (prop.trim(), value.trim());
    if prop.starts_with('-') && !prop.starts_with("--") {
        return false; // vendor-prefixed properties
    }
    const UNSUPPORTED: &[&str] = &[
        "backdrop-filter",
        "container-type",
        "content-visibility",
        "transform-style",
        "anchor-name",
        "view-transition-name",
        "text-wrap",
        "field-sizing",
        "interpolate-size",
    ];
    if UNSUPPORTED.contains(&prop) {
        return false;
    }
    !(prop == "display" && matches!(value, "ruby" | "math" | "-webkit-box"))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn env(width: f32) -> MediaEnv {
        MediaEnv {
            width,
            ..Default::default()
        }
    }

    #[test]
    fn media_queries() {
        let m = |q: &str, w: f32| MediaList::parse(q).matches(&env(w));
        assert!(m("", 100.0));
        assert!(m("screen", 100.0));
        assert!(!m("print", 100.0));
        assert!(m("(min-width: 720px)", 1000.0));
        assert!(!m("(min-width: 720px)", 500.0));
        assert!(m("screen and (max-width: 40em)", 600.0));
        assert!(m("(min-width: 1000px), print", 1200.0));
        assert!(m("not print", 1.0));
        assert!(m("(prefers-color-scheme: light)", 1.0));
        assert!(!m("(prefers-color-scheme: dark)", 1.0));
        assert!(m("(width >= 600px)", 800.0));
        assert!(m("(400px <= width < 700px)", 500.0));
        assert!(!m("(400px <= width < 700px)", 700.0));
        assert!(m("(min-width: 100px) and (max-width: 200px)", 150.0));
        assert!(m("((min-width: 100px) or (hover: none))", 150.0));
        assert!(m("(hover: hover) and (pointer: fine)", 1.0));
        assert!(!m("(unknown-feature)", 1.0));
    }

    #[test]
    fn supports_conditions() {
        assert!(supports("(display: grid)"));
        assert!(supports("(display: flex) and (gap: 1px)"));
        assert!(!supports("(-webkit-appearance: none)"));
        assert!(supports("not (-webkit-appearance: none)"));
        assert!(supports(
            "(mask-image: url(x)) or (-webkit-mask-image: url(x))"
        ));
        assert!(supports("selector(:has(a))"));
        assert!(!supports("(backdrop-filter: blur(1px))"));
    }
}
