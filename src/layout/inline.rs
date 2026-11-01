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
    /// distance from its top to its baseline.
    Atomic {
        index: usize,
        width: f32,
        height: f32,
        baseline: f32,
    },
    /// A forced line break (`<br>`).
    Break { node: NodeId },
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
    },
    /// `y` is the top of the atomic box relative to the line top.
    Atomic { index: usize, x: f32, y: f32 },
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
    },
    Break,
}

fn style(styles: &[Option<Style>], node: NodeId) -> &Style {
    static DEFAULT: std::sync::OnceLock<Style> = std::sync::OnceLock::new();
    styles
        .get(node)
        .and_then(Option::as_ref)
        .unwrap_or_else(|| DEFAULT.get_or_init(Style::default))
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
            Item::Atomic {
                index,
                width,
                height,
                baseline,
            } => {
                out.push(Piece::Atomic {
                    index: *index,
                    width: *width,
                    height: *height,
                    baseline: *baseline,
                });
                after_space = false;
            }
            Item::Text { node, text } => {
                let st = style(styles, *node);
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
        if let Some(Placed::Text { width, text, .. }) = line.items.last_mut()
            && text.ends_with(' ')
        {
            *width -= *trailing;
            *text = text.trim_end_matches(' ').to_string();
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
                if !at_line_start && wraps && breakable && x + width - space > max_width {
                    finish(&mut line, &mut ext, &mut x, &mut trailing, &mut lines);
                    if text.trim().is_empty() {
                        continue;
                    }
                }
                let st = style(styles, node);
                let e = extents(st, fonts);
                ext = (ext.0.max(e.0), ext.1.max(e.1));
                // Merge with the previous run of the same text node.
                if let Some(Placed::Text {
                    node: n,
                    width: w,
                    text: t,
                    ..
                }) = line.items.last_mut()
                    && *n == node
                {
                    t.push_str(&text);
                    *w += width;
                } else {
                    line.items.push(Placed::Text {
                        node,
                        x,
                        width,
                        text,
                    });
                }
                x += width;
                trailing = space;
                breakable = space > 0.0;
            }
            Piece::Atomic {
                index,
                width,
                height,
                baseline,
            } => {
                if !line.items.is_empty() && x + width > max_width {
                    finish(&mut line, &mut ext, &mut x, &mut trailing, &mut lines);
                }
                trailing = 0.0;
                breakable = true;
                // The box's baseline sits on the line's baseline.
                ext = (ext.0.max(baseline), ext.1.max(height - baseline));
                line.items.push(Placed::Atomic {
                    index,
                    x,
                    y: baseline,
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
        Placed::Atomic { x, .. } => *x,
    }
}

/// Space a style's text needs above and below the baseline (with half-leading).
fn extents(st: &Style, fonts: &Fonts) -> (f32, f32) {
    let m = fonts.line_metrics(st.font(), st.font_size);
    let lh = st.line_height_px();
    let half = (lh - (m.ascent + m.descent)) / 2.0;
    (m.ascent + half, m.descent + half)
}

/// Shifts lines horizontally for `text-align` within `width`.
pub fn align(layout: &mut InlineLayout, align: TextAlign, width: f32) {
    let factor = match align {
        TextAlign::Left => return,
        TextAlign::Right => 1.0,
        TextAlign::Center => 0.5,
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::css::style::WhiteSpace;

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
        align(&mut l, TextAlign::Center, 200.0);
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
