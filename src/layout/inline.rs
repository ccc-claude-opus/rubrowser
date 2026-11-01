//! Inline formatting: whitespace handling, line breaking, baselines.
//!
//! An inline formatting context is a flat list of [`Item`]s (text runs,
//! atomic boxes such as `inline-block`, and `<br>`), produced by flattening
//! inline elements. Positions are relative to the content box of the block
//! that owns the context.

use crate::css::Style;
use crate::css::style::{TextAlign, WhiteSpace};
use crate::dom::NodeId;
use crate::text::Fonts;

#[derive(Clone, Debug)]
pub enum Item {
    /// Text of DOM text node `node` (styled by `styles[node]`).
    Text { node: NodeId, text: String },
    /// An atomic inline (layout box index) with its margin-box size and the
    /// distance from its top to its baseline. `wraps`: whether a line may
    /// break around it (its parent's `white-space`).
    Atomic {
        index: usize,
        width: f32,
        height: f32,
        baseline: f32,
        wraps: bool,
    },
    /// A forced line break (`<br>`).
    Break { node: NodeId },
    /// Horizontal space from an inline element's margin, border and
    /// padding at its start or end.
    Gap { width: f32 },
}

/// The items of one inline formatting context and the block that owns it.
#[derive(Clone, Debug)]
pub struct Content {
    pub owner: NodeId,
    pub items: Vec<Item>,
}

impl Content {
    /// True if the content would produce no lines (collapsible spaces only).
    pub fn is_blank(&self, styles: &[Option<Style>]) -> bool {
        self.items.iter().all(|i| match i {
            Item::Text { node, text } => {
                styles[*node]
                    .as_ref()
                    .is_none_or(|s| s.white_space.collapses())
                    && text.chars().all(char::is_whitespace)
            }
            _ => false,
        })
    }
}

#[derive(Clone, Debug, PartialEq)]
pub enum Placed {
    Text {
        node: NodeId,
        x: f32,
        width: f32,
        text: String,
        /// Vertical offset from the line's baseline (`sub` / `super`).
        shift: f32,
        /// Width of the trailing spaces included in `width`.
        space: f32,
    },
    /// `y` is the top of the atomic box relative to the line top.
    Atomic {
        index: usize,
        x: f32,
        y: f32,
        width: f32,
    },
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct Line {
    pub top: f32,
    pub height: f32,
    /// Baseline offset from the line top.
    pub baseline: f32,
    pub width: f32,
    pub items: Vec<Placed>,
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct InlineLayout {
    pub lines: Vec<Line>,
    /// Widest line.
    pub width: f32,
    pub height: f32,
}

/// A breakable unit: a word with its trailing spaces, an atomic box, or a break.
enum Piece {
    Word {
        node: NodeId,
        text: String,
        width: f32,
        /// Width of trailing spaces, which hang at a line end.
        space: f32,
        wraps: bool,
    },
    Atomic {
        index: usize,
        width: f32,
        height: f32,
        baseline: f32,
        wraps: bool,
    },
    Break,
    Gap(f32),
}

fn style(styles: &[Option<Style>], node: NodeId) -> &Style {
    thread_local! {
        static DEFAULT: &'static Style = Box::leak(Box::new(Style::default()));
    }
    styles
        .get(node)
        .and_then(Option::as_ref)
        .unwrap_or_else(|| DEFAULT.with(|d| *d))
}

/// Splits items into pieces, applying `white-space` rules.
fn pieces(content: &Content, styles: &[Option<Style>], fonts: &Fonts) -> Vec<Piece> {
    let mut out = Vec::new();
    // At the start of a block, leading collapsible spaces are dropped.
    let mut after_space = true;
    for item in &content.items {
        match item {
            Item::Break { .. } => {
                out.push(Piece::Break);
                after_space = true;
            }
            Item::Gap { width } => out.push(Piece::Gap(*width)),
            Item::Atomic {
                index,
                width,
                height,
                baseline,
                wraps,
            } => {
                out.push(Piece::Atomic {
                    index: *index,
                    width: *width,
                    height: *height,
                    baseline: *baseline,
                    wraps: *wraps,
                });
                after_space = false;
            }
            Item::Text { node, text } => {
                let st = style(styles, *node);
                let transformed = transform(text, st.text_transform);
                let text = &transformed;
                let ws = st.white_space;
                let font = st.font();
                let measure = |s: &str| fonts.measure(font, st.font_size, s);
                let node = *node;
                let push_word = |out: &mut Vec<Piece>, word: &str, spaces: &str| {
                    if word.is_empty() && spaces.is_empty() {
                        return;
                    }
                    let width = measure(word);
                    let space = if spaces.is_empty() {
                        0.0
                    } else {
                        measure(spaces)
                    };
                    out.push(Piece::Word {
                        node,
                        text: format!("{word}{spaces}"),
                        width: width + space,
                        space,
                        wraps: ws.wraps(),
                    });
                };
                if ws.collapses() {
                    let mut word = String::new();
                    let mut pending_space = false;
                    for c in text.chars() {
                        if c.is_whitespace() {
                            if !after_space && !word.is_empty() {
                                pending_space = true;
                            } else if !after_space && word.is_empty() {
                                // Space right after a previous item.
                                push_word(&mut out, "", " ");
                                after_space = true;
                            }
                        } else {
                            if pending_space {
                                push_word(&mut out, &word, " ");
                                word.clear();
                                pending_space = false;
                            }
                            // CJK text breaks between characters (but not
                            // before closing punctuation).
                            if (is_cjk(c) || word.chars().last().is_some_and(is_cjk))
                                && !word.is_empty()
                                && !no_break_before(c)
                            {
                                push_word(&mut out, &word, "");
                                word.clear();
                            }
                            word.push(c);
                            after_space = false;
                        }
                    }
                    if pending_space {
                        push_word(&mut out, &word, " ");
                        after_space = true;
                    } else if !word.is_empty() {
                        push_word(&mut out, &word, "");
                    }
                } else {
                    // Preserved whitespace: newlines force breaks; with
                    // pre-wrap, lines may still wrap after spaces.
                    let text = text.replace('\t', "    ");
                    for (i, line) in text.split('\n').enumerate() {
                        if i > 0 {
                            out.push(Piece::Break);
                        }
                        if ws == WhiteSpace::PreWrap {
                            let mut rest = line;
                            while !rest.is_empty() {
                                let word_end = rest.find(' ').unwrap_or(rest.len());
                                let space_end = rest[word_end..]
                                    .find(|c| c != ' ')
                                    .map_or(rest.len(), |j| word_end + j);
                                push_word(&mut out, &rest[..word_end], &rest[word_end..space_end]);
                                rest = &rest[space_end..];
                            }
                        } else {
                            push_word(&mut out, line, "");
                        }
                    }
                    after_space = text.ends_with([' ', '\n']);
                }
            }
        }
    }
    out
}

/// Lays out `content` in lines at most `max_width` wide (`f32::INFINITY`
/// for max-content, `0` for min-content).
pub fn layout(
    content: &Content,
    max_width: f32,
    styles: &[Option<Style>],
    fonts: &Fonts,
) -> InlineLayout {
    let rtl_para = style(styles, content.owner).rtl;
    if content.is_blank(styles) {
        return InlineLayout::default();
    }
    let owner = style(styles, content.owner);
    let mut lines: Vec<Line> = Vec::new();
    let mut line = Line::default();
    // Per-line vertical extents: (above baseline, below baseline).
    let strut = extents(owner, fonts);
    let mut ext = strut;
    let mut x = 0.0f32;

    // Width of the spaces ending the last word placed on the line.
    let mut trailing = 0.0f32;
    let finish = |line: &mut Line,
                  ext: &mut (f32, f32),
                  x: &mut f32,
                  trailing: &mut f32,
                  lines: &mut Vec<Line>| {
        // Trailing spaces hang: they do not count toward the line width.
        if let Some(Placed::Text {
            width, text, space, ..
        }) = line.items.last_mut()
            && text.ends_with(' ')
        {
            *width -= *trailing;
            *text = text.trim_end_matches(' ').to_string();
            *space = 0.0;
        }
        *trailing = 0.0;
        line.width = line.items.iter().map(right_edge).fold(0.0, f32::max);
        line.baseline = ext.0;
        line.height = ext.0 + ext.1;
        line.top = lines.last().map_or(0.0, |l| l.top + l.height);
        lines.push(std::mem::take(line));
        *ext = strut;
        *x = 0.0;
    };

    // Lines may only break where there is a space (or around atomics).
    let mut breakable = false;
    for piece in pieces(content, styles, fonts) {
        match piece {
            Piece::Break => {
                finish(&mut line, &mut ext, &mut x, &mut trailing, &mut lines);
            }
            Piece::Gap(w) => {
                x += w;
                trailing = 0.0;
            }
            Piece::Word {
                node,
                text,
                width,
                space,
                wraps,
            } => {
                let at_line_start = line.items.is_empty();
                if at_line_start && text.trim().is_empty() {
                    continue; // collapsible space at line start
                }
                let cjk_start = text
                    .chars()
                    .next()
                    .is_some_and(|c| is_cjk(c) && !no_break_before(c));
                if !at_line_start
                    && wraps
                    && (breakable || cjk_start)
                    && x + width - space > max_width + 0.01
                {
                    finish(&mut line, &mut ext, &mut x, &mut trailing, &mut lines);
                    if text.trim().is_empty() {
                        continue;
                    }
                }
                let st = style(styles, node);
                let shift = baseline_shift(st);
                let e = extents(st, fonts);
                ext = (ext.0.max(e.0 - shift), ext.1.max(e.1 + shift));
                // Merge with the previous run of the same text node.
                if let Some(Placed::Text {
                    node: n,
                    width: w,
                    text: t,
                    space: sp,
                    ..
                }) = line.items.last_mut()
                    && *n == node
                    && !rtl_para
                    && !crate::text::has_rtl(t)
                    && !crate::text::has_rtl(&text)
                {
                    t.push_str(&text);
                    *w += width;
                    *sp = space;
                } else {
                    line.items.push(Placed::Text {
                        node,
                        x,
                        width,
                        text,
                        shift,
                        space,
                    });
                }
                x += width;
                trailing = space;
                // After a space, or between CJK characters.
                breakable = space > 0.0 || text_ends_cjk(line.items.last());
            }
            Piece::Atomic {
                index,
                width,
                height,
                baseline,
                wraps,
            } => {
                if wraps && !line.items.is_empty() && x + width > max_width + 0.01 {
                    finish(&mut line, &mut ext, &mut x, &mut trailing, &mut lines);
                }
                trailing = 0.0;
                breakable = wraps;
                // The box's baseline sits on the line's baseline.
                ext = (ext.0.max(baseline), ext.1.max(height - baseline));
                line.items.push(Placed::Atomic {
                    index,
                    x,
                    y: baseline,
                    width,
                });
                x += width;
            }
        }
    }
    if !line.items.is_empty() {
        finish(&mut line, &mut ext, &mut x, &mut trailing, &mut lines);
    }
    // Now that baselines are known, turn atomic baselines into top offsets.
    for l in &mut lines {
        for item in &mut l.items {
            if let Placed::Atomic { y, .. } = item {
                *y = l.baseline - *y;
            }
        }
    }
    let width = lines.iter().map(|l| l.width).fold(0.0, f32::max);
    let height = lines.last().map_or(0.0, |l| l.top + l.height);
    InlineLayout {
        lines,
        width,
        height,
    }
}

fn right_edge(p: &Placed) -> f32 {
    match p {
        Placed::Text { x, width, .. } => x + width,
        Placed::Atomic { x, width, .. } => x + width,
    }
}

/// Space a style's text needs above and below the baseline (with half-leading).
/// Baseline offset for `vertical-align: sub / super` (positive is down).
fn baseline_shift(st: &Style) -> f32 {
    use crate::css::style::VerticalAlign;
    match st.vertical_align {
        VerticalAlign::Super => -st.font_size * 0.5,
        VerticalAlign::Sub => st.font_size * 0.3,
        _ => 0.0,
    }
}

/// Applies `text-transform`.
fn transform(text: &str, t: crate::css::style::TextTransform) -> String {
    use crate::css::style::TextTransform;
    match t {
        TextTransform::None => text.to_string(),
        TextTransform::Uppercase => text.to_uppercase(),
        TextTransform::Lowercase => text.to_lowercase(),
        TextTransform::Capitalize => {
            let mut out = String::with_capacity(text.len());
            let mut start = true;
            for c in text.chars() {
                if start && c.is_alphabetic() {
                    out.extend(c.to_uppercase());
                } else {
                    out.push(c);
                }
                start = c.is_whitespace();
            }
            out
        }
    }
}

fn extents(st: &Style, fonts: &Fonts) -> (f32, f32) {
    let m = fonts.line_metrics(st.font(), st.font_size);
    let lh = st.line_height_px();
    let half = (lh - (m.ascent + m.descent)) / 2.0;
    (m.ascent + half, m.descent + half)
}

/// Shifts lines horizontally for `text-align` within `width`.
pub fn align(layout: &mut InlineLayout, align: TextAlign, width: f32, rtl: bool) {
    if rtl || layout.lines.iter().any(|l| l.items.iter().any(item_rtl)) {
        for line in &mut layout.lines {
            reorder(line, rtl);
        }
    }
    let factor = match (align, rtl) {
        (TextAlign::Left, _) | (TextAlign::Start, false) | (TextAlign::End, true) => return,
        (TextAlign::Right, _) | (TextAlign::Start, true) | (TextAlign::End, false) => 1.0,
        (TextAlign::Center | TextAlign::WebkitCenter, _) => 0.5,
    };
    for line in &mut layout.lines {
        let dx = ((width - line.width) * factor).max(0.0);
        for item in &mut line.items {
            match item {
                Placed::Text { x, .. } | Placed::Atomic { x, .. } => *x += dx,
            }
        }
    }
}

/// Whether `c` is Chinese, Japanese or Korean text (lines may break
/// between such characters).
fn is_cjk(c: char) -> bool {
    matches!(c as u32,
        0x1100..=0x11FF | 0x2E80..=0x2FDF | 0x3000..=0x303F | 0x3040..=0x30FF
        | 0x3100..=0x31FF | 0x3400..=0x4DBF | 0x4E00..=0x9FFF | 0xA960..=0xA97F
        | 0xAC00..=0xD7AF | 0xF900..=0xFAFF | 0xFE30..=0xFE4F | 0xFF00..=0xFFEF
        | 0x20000..=0x2FA1F)
}

/// Closing punctuation that may not start a line.
fn no_break_before(c: char) -> bool {
    "，。、！？：；）」』】》〉…・ー,.!?:;)]}%".contains(c)
}

fn text_ends_cjk(item: Option<&Placed>) -> bool {
    matches!(item, Some(Placed::Text { text, .. }) if text.chars().last().is_some_and(is_cjk))
}

fn item_rtl(item: &Placed) -> bool {
    matches!(item, Placed::Text { text, .. } if crate::text::has_rtl(text))
}

/// Bidirectional reordering of one line (simplified Unicode bidi, word by
/// word): items get embedding levels from their direction and are reversed
/// per rule L2; they then sit side by side from the line start.
fn reorder(line: &mut Line, rtl: bool) {
    // Direction of each item: Some(true) RTL, Some(false) LTR, None neutral.
    let dir = |item: &Placed| match item {
        Placed::Text { text, .. } => {
            if crate::text::has_rtl(text) {
                Some(true)
            } else if text.chars().any(char::is_alphanumeric) {
                Some(false)
            } else {
                None
            }
        }
        Placed::Atomic { .. } => Some(false),
    };
    // Trailing spaces become separate (neutral) items so they land between
    // words of either direction.
    let mut items = Vec::with_capacity(line.items.len() * 2);
    for item in std::mem::take(&mut line.items) {
        match item {
            Placed::Text {
                node,
                x,
                width,
                text,
                shift,
                space,
            } if space > 0.0 && text.ends_with(' ') && text.trim_end().len() < text.len() => {
                let word = text.trim_end().to_string();
                let spaces = text[word.len()..].to_string();
                items.push(Placed::Text {
                    node,
                    x,
                    width: width - space,
                    text: word,
                    shift,
                    space: 0.0,
                });
                items.push(Placed::Text {
                    node,
                    x: x + width - space,
                    width: space,
                    text: spaces,
                    shift,
                    space,
                });
            }
            other => items.push(other),
        }
    }
    line.items = items;
    let dirs: Vec<Option<bool>> = line.items.iter().map(dir).collect();
    let n = dirs.len();
    // Neutrals take the direction of their neighbors when both agree,
    // else the paragraph's.
    let resolved: Vec<bool> = (0..n)
        .map(|i| {
            dirs[i].unwrap_or_else(|| {
                let before = dirs[..i].iter().rev().find_map(|d| *d);
                let after = dirs[i + 1..].iter().find_map(|d| *d);
                match (before, after) {
                    (Some(a), Some(b)) if a == b => a,
                    _ => rtl,
                }
            })
        })
        .collect();
    // Embedding levels: the paragraph's is 0 (LTR) or 1 (RTL); RTL items
    // get the next odd level, LTR items in an RTL paragraph level 2.
    let levels: Vec<u8> = resolved
        .iter()
        .map(|&r| match (rtl, r) {
            (false, false) => 0,
            (_, true) => 1,
            (true, false) => 2,
        })
        .collect();
    // Rule L2: from the highest level down to the lowest odd one, reverse
    // every run at that level or higher.
    let mut order: Vec<usize> = (0..n).collect();
    let max = levels.iter().copied().max().unwrap_or(0);
    for level in (1..=max).rev() {
        let mut k = 0;
        while k < n {
            if levels[order[k]] >= level {
                let j = (k..n).find(|&m| levels[order[m]] < level).unwrap_or(n);
                order[k..j].reverse();
                k = j;
            } else {
                k += 1;
            }
        }
    }
    let start = line
        .items
        .iter()
        .map(|it| match it {
            Placed::Text { x, .. } | Placed::Atomic { x, .. } => *x,
        })
        .fold(f32::INFINITY, f32::min);
    let start = if start.is_finite() { start } else { 0.0 };
    let old = std::mem::take(&mut line.items);
    let mut slots: Vec<Option<Placed>> = old.into_iter().map(Some).collect();
    let mut x = start;
    for idx in order {
        let Some(mut item) = slots[idx].take() else {
            continue;
        };
        match &mut item {
            Placed::Text { x: ix, width, .. } | Placed::Atomic { x: ix, width, .. } => {
                *ix = x;
                x += *width;
            }
        }
        line.items.push(item);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::css::style::WhiteSpace;

    fn words(line: &Line) -> Vec<String> {
        line.items
            .iter()
            .filter_map(|i| match i {
                Placed::Text { text, .. } if !text.trim().is_empty() => {
                    Some(text.trim().to_string())
                }
                _ => None,
            })
            .collect()
    }

    fn line_of(texts: &[&str]) -> Line {
        let mut x = 0.0;
        let items = texts
            .iter()
            .map(|t| {
                let p = Placed::Text {
                    node: 0,
                    x,
                    width: 10.0,
                    text: t.to_string(),
                    shift: 0.0,
                    space: if t.ends_with(' ') { 2.0 } else { 0.0 },
                };
                x += 10.0;
                p
            })
            .collect();
        Line {
            items,
            ..Default::default()
        }
    }

    #[test]
    fn bidi_reordering() {
        // LTR paragraph: the Hebrew run is reversed in place.
        let mut l = line_of(&["a ", "אב ", "גד ", "b"]);
        reorder(&mut l, false);
        assert_eq!(words(&l), ["a", "גד", "אב", "b"]);
        // RTL paragraph: the line flips, the Latin run keeps its order.
        let mut l = line_of(&["אב ", "Rust ", "1.86 ", "גד"]);
        reorder(&mut l, true);
        assert_eq!(words(&l), ["גד", "Rust", "1.86", "אב"]);
    }

    fn setup(text: &str, ws: WhiteSpace) -> (Content, Vec<Option<Style>>) {
        let st = Style {
            white_space: ws,
            ..Style::default()
        };
        let styles = vec![Some(st.clone()), Some(st)];
        let content = Content {
            owner: 0,
            items: vec![Item::Text {
                node: 1,
                text: text.into(),
            }],
        };
        (content, styles)
    }

    fn texts(l: &InlineLayout) -> Vec<String> {
        l.lines
            .iter()
            .map(|line| {
                line.items
                    .iter()
                    .map(|p| match p {
                        Placed::Text { text, .. } => text.clone(),
                        Placed::Atomic { .. } => "[]".into(),
                    })
                    .collect()
            })
            .collect()
    }

    #[test]
    fn collapses_whitespace_and_wraps() {
        let f = Fonts::new();
        let (c, s) = setup("  hello \n  wide   world  ", WhiteSpace::Normal);
        let one = layout(&c, f32::INFINITY, &s, &f);
        assert_eq!(texts(&one), vec!["hello wide world"]);
        let narrow = layout(
            &c,
            f.measure(Default::default(), 16.0, "hello wide") + 1.0,
            &s,
            &f,
        );
        assert_eq!(texts(&narrow), vec!["hello wide", "world"]);
        assert!(narrow.height > one.height);
        let min = layout(&c, 0.0, &s, &f);
        assert_eq!(min.lines.len(), 3);
        assert!(min.width < one.width);
    }

    #[test]
    fn no_break_without_a_space_between_runs() {
        let f = Fonts::new();
        let (mut c, mut s) = setup("aaa", WhiteSpace::Normal);
        s.push(s[1].clone());
        c.items.push(Item::Text {
            node: 2,
            text: ", bbb".into(),
        });
        let l = layout(&c, f.measure(Default::default(), 16.0, "aaa") + 2.0, &s, &f);
        assert_eq!(texts(&l)[0], "aaa,");
    }

    #[test]
    fn text_transform_and_superscript() {
        use crate::css::style::{TextTransform, VerticalAlign};
        assert_eq!(
            transform("hello big world", TextTransform::Capitalize),
            "Hello Big World"
        );
        assert_eq!(transform("Mixed", TextTransform::Uppercase), "MIXED");
        let f = Fonts::new();
        let (c, mut s) = setup("x", WhiteSpace::Normal);
        s[1].as_mut().unwrap().vertical_align = VerticalAlign::Super;
        let l = layout(&c, f32::INFINITY, &s, &f);
        match &l.lines[0].items[0] {
            Placed::Text { shift, .. } => assert!(*shift < 0.0),
            _ => unreachable!(),
        }
    }

    #[test]
    fn pre_keeps_spaces_and_newlines() {
        let f = Fonts::new();
        let (c, s) = setup("a  b\nc", WhiteSpace::Pre);
        let l = layout(&c, 1.0, &s, &f);
        assert_eq!(texts(&l), vec!["a  b", "c"]);
    }

    #[test]
    fn blank_content_has_no_lines() {
        let f = Fonts::new();
        let (c, s) = setup(" \n  ", WhiteSpace::Normal);
        assert!(c.is_blank(&s));
        assert_eq!(layout(&c, 100.0, &s, &f).height, 0.0);
    }

    #[test]
    fn line_height_and_alignment() {
        let f = Fonts::new();
        let (c, mut s) = setup("hi", WhiteSpace::Normal);
        for st in s.iter_mut().flatten() {
            st.line_height = crate::css::style::LineHeight::Px(30.0);
        }
        let mut l = layout(&c, 200.0, &s, &f);
        assert_eq!(l.height, 30.0);
        let w = l.width;
        align(&mut l, TextAlign::Center, 200.0, false);
        match &l.lines[0].items[0] {
            Placed::Text { x, .. } => assert!((x - (200.0 - w) / 2.0).abs() < 0.01),
            _ => unreachable!(),
        }
    }

    #[test]
    fn atomic_boxes_sit_on_the_baseline() {
        let f = Fonts::new();
        let (mut c, s) = setup("a ", WhiteSpace::Normal);
        c.items.push(Item::Atomic {
            index: 7,
            width: 50.0,
            height: 40.0,
            baseline: 40.0,
            wraps: true,
        });
        let l = layout(&c, f32::INFINITY, &s, &f);
        assert_eq!(texts(&l), vec!["a []"]);
        let line = &l.lines[0];
        assert!(line.baseline >= 40.0);
        assert!(
            matches!(line.items[1], Placed::Atomic { index: 7, y, .. } if (y - (line.baseline - 40.0)).abs() < 0.01)
        );
    }
}
