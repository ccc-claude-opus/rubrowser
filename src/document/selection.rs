//! Text selection in the page: drag to select, double-click a word,
//! select all, and the selected text (for copying). Positions are taken
//! from the laid-out text: the runs (`Placed::Text`) in document order and
//! a character offset in a run, found by measuring with the run's font.
use super::Document;
use crate::dom::NodeId;
use crate::layout::inline::Placed;
use crate::layout::{LayoutTree, RectF};
use crate::text::Fonts;
use std::collections::HashMap;

/// Which run of laid-out text: its text node, and the line top and x of
/// the run inside its block (the same values the painter sees).
pub type RunKey = (NodeId, u32, u32);

fn key(node: NodeId, line_top: f32, x: f32) -> RunKey {
    (node, line_top.to_bits(), x.to_bits())
}

/// One run of text where it is on screen (viewport coordinates).
#[derive(Clone, Debug)]
pub struct TextRun {
    pub key: RunKey,
    pub rect: RectF,
    pub text: String,
    /// It starts a new line (a different line box than the run before).
    pub new_line: bool,
}

/// A point in the text: a run and a character offset in it.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct TextPoint {
    pub run: RunKey,
    pub ch: usize,
}

/// The selection: where it started and where it ends now (either order).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Selection {
    pub anchor: TextPoint,
    pub focus: TextPoint,
}

/// Characters selected in each run: run → (start, end) char offsets.
pub type Highlights = HashMap<RunKey, (usize, usize)>;

impl Document {
    /// Every run of text in the page in document order, where it is now.
    pub fn text_runs(&self) -> Vec<TextRun> {
        let mut out = Vec::new();
        if self.tree.boxes.is_empty() {
            return out;
        }
        let vp = &self.tree.boxes[LayoutTree::VIEWPORT];
        let sy = self.scroll_of(None);
        let mut last_line = None;
        for &c in &vp.children {
            let dy = if self.tree.boxes[c].fixed { 0.0 } else { sy };
            self.runs_of(c, 0.0, -dy, &mut out, &mut last_line);
        }
        out
    }

    fn runs_of(&self, i: usize, ox: f32, oy: f32, out: &mut Vec<TextRun>, last: &mut Option<(usize, usize)>) {
        let b = &self.tree.boxes[i];
        let style = self.style(b.style_node);
        if style.is_some_and(|s| !s.visible || s.clipped_away) && b.children.is_empty() && b.inline.is_none() {
            return;
        }
        let (tx, ty) = style.map_or((0.0, 0.0), |s| b.translation(s));
        let r = RectF {
            x: ox + b.rect.x + tx,
            y: oy + b.rect.y + ty,
            ..b.rect
        };
        let clips = b.node.is_some()
            && style.is_some_and(|s| s.overflow != crate::css::style::Overflow::Visible);
        let sy = if clips { self.scroll_of(b.node) } else { 0.0 };
        for &c in &b.children {
            let cb = &self.tree.boxes[c];
            let bottom = r.y + r.h - b.padding[2] - b.border[2];
            let shift = cb.sticky_shift(r.y - sy + cb.rect.y, 0.0, bottom);
            self.runs_of(c, r.x, r.y - sy + shift, out, last);
        }
        if let Some(il) = &b.inline {
            let (cx, cy) = b.content_offset();
            let (bx, by) = (r.x + cx, r.y + cy - sy);
            for (li, line) in il.lines.iter().enumerate() {
                for item in &line.items {
                    match item {
                        Placed::Text { node, x, width, text, .. } => {
                            if self.style(*node).is_some_and(|s| !s.visible || s.clipped_away) {
                                continue;
                            }
                            let here = (i, li);
                            out.push(TextRun {
                                key: key(*node, line.top, *x),
                                rect: RectF { x: bx + x, y: by + line.top, w: *width, h: line.height },
                                text: text.clone(),
                                new_line: last.is_some_and(|l| l != here),
                            });
                            *last = Some(here);
                        }
                        Placed::Atomic { index, x, y, .. } => {
                            self.runs_of(*index, bx + x, by + line.top + y, out, last);
                        }
                    }
                }
            }
        }
    }

    /// The text point nearest to viewport point `(x, y)`: on the line under
    /// it (or the nearest line), the character boundary nearest to `x`.
    pub fn text_point_at(&self, runs: &[TextRun], x: f32, y: f32, fonts: &Fonts) -> Option<TextPoint> {
        let dist_y = |r: &RectF| if y < r.y { r.y - y } else if y > r.y + r.h { y - r.y - r.h } else { 0.0 };
        let dist_x = |r: &RectF| if x < r.x { r.x - x } else if x > r.x + r.w { x - r.x - r.w } else { 0.0 };
        let best = runs.iter().min_by(|a, b| {
            (dist_y(&a.rect), dist_x(&a.rect))
                .partial_cmp(&(dist_y(&b.rect), dist_x(&b.rect)))
                .unwrap_or(std::cmp::Ordering::Equal)
        })?;
        let ch = self.char_at(best, x - best.rect.x, fonts);
        Some(TextPoint { run: best.key, ch })
    }

    /// The character boundary in `run` nearest to `dx` from its left edge.
    fn char_at(&self, run: &TextRun, dx: f32, fonts: &Fonts) -> usize {
        let Some(st) = self.style(run.key.0) else { return 0 };
        // the byte index after each character
        let ends: Vec<usize> = run.text.char_indices().map(|(b, _)| b).skip(1).chain([run.text.len()]).collect();
        let mut prev = 0.0;
        for (i, &end) in ends.iter().enumerate() {
            let w = fonts.measure(st.font(), st.font_size, &run.text[..end]);
            if dx < (prev + w) / 2.0 {
                return i;
            }
            prev = w;
        }
        ends.len()
    }

    pub fn set_selection(&mut self, sel: Option<Selection>) {
        if self.selection != sel {
            self.selection = sel;
            self.highlights = self.compute_highlights();
        }
    }

    /// Selects the word (letters, digits, CJK or other non-space characters
    /// together; a run of spaces) around `p`.
    pub fn select_word(&mut self, runs: &[TextRun], p: TextPoint) {
        let Some(run) = runs.iter().find(|r| r.key == p.run) else { return };
        let chars: Vec<char> = run.text.chars().collect();
        if chars.is_empty() {
            return;
        }
        let at = p.ch.min(chars.len() - 1);
        let class = |c: char| if c.is_whitespace() { 0 } else if c.is_alphanumeric() || c == '_' { 1 } else { 2 };
        let k = class(chars[at]);
        let mut s = at;
        while s > 0 && class(chars[s - 1]) == k {
            s -= 1;
        }
        let mut e = at + 1;
        while e < chars.len() && class(chars[e]) == k {
            e += 1;
        }
        self.set_selection(Some(Selection {
            anchor: TextPoint { run: p.run, ch: s },
            focus: TextPoint { run: p.run, ch: e },
        }));
    }

    /// Selects all the text in the page.
    pub fn select_all_text(&mut self) {
        let runs = self.text_runs();
        let (Some(first), Some(last)) = (runs.first(), runs.last()) else { return };
        let sel = Selection {
            anchor: TextPoint { run: first.key, ch: 0 },
            focus: TextPoint { run: last.key, ch: last.text.chars().count() },
        };
        self.set_selection(Some(sel));
    }

    /// The selection as (start, end) indices into `runs` with char offsets,
    /// in document order; None when nothing (or nothing on screen) is selected.
    fn ordered(&self, runs: &[TextRun]) -> Option<((usize, usize), (usize, usize))> {
        let sel = self.selection?;
        let find = |p: TextPoint| runs.iter().position(|r| r.key == p.run).map(|i| (i, p.ch));
        let (a, f) = (find(sel.anchor)?, find(sel.focus)?);
        let (s, e) = if a <= f { (a, f) } else { (f, a) };
        (s != e).then_some((s, e))
    }

    fn compute_highlights(&self) -> Highlights {
        let runs = self.text_runs();
        let mut out = Highlights::new();
        if let Some(((si, sc), (ei, ec))) = self.ordered(&runs) {
            for (i, r) in runs.iter().enumerate().take(ei + 1).skip(si) {
                let n = r.text.chars().count();
                let from = if i == si { sc.min(n) } else { 0 };
                let to = if i == ei { ec.min(n) } else { n };
                if to > from {
                    out.insert(r.key, (from, to));
                }
            }
        }
        out
    }

    /// Runs moved or changed (new layout): keep the highlights in step, or
    /// drop a selection whose text is gone.
    pub fn refresh_selection(&mut self, before: &[TextRun]) {
        let Some(mut sel) = self.selection else { return };
        let runs = self.text_runs();
        // A run whose position changed: the same text node's run with the
        // same ordinal (the 3rd run of that node is still its 3rd run).
        let remap = |p: &mut TextPoint| {
            if runs.iter().any(|r| r.key == p.run) {
                return true;
            }
            let node = p.run.0;
            let Some(k) = before.iter().filter(|r| r.key.0 == node).position(|r| r.key == p.run) else {
                return false;
            };
            match runs.iter().filter(|r| r.key.0 == node).nth(k) {
                Some(r) => {
                    p.run = r.key;
                    true
                }
                None => false,
            }
        };
        if remap(&mut sel.anchor) && remap(&mut sel.focus) {
            self.selection = Some(sel);
        } else {
            self.selection = None;
        }
        self.highlights = self.compute_highlights();
    }

    /// The selected text, lines joined with newlines.
    pub fn selected_text(&self) -> String {
        let runs = self.text_runs();
        let Some(((si, sc), (ei, ec))) = self.ordered(&runs) else {
            return String::new();
        };
        let mut out = String::new();
        for (i, r) in runs.iter().enumerate().take(ei + 1).skip(si) {
            if i > si && r.new_line {
                // trailing spaces of a line are not part of the copied text
                while out.ends_with(' ') {
                    out.pop();
                }
                out.push('\n');
            }
            let chars: Vec<char> = r.text.chars().collect();
            let from = if i == si { sc.min(chars.len()) } else { 0 };
            let to = if i == ei { ec.min(chars.len()) } else { chars.len() };
            out.extend(&chars[from..to.max(from)]);
        }
        out.trim_end().to_string()
    }

    pub fn has_selection(&self) -> bool {
        !self.highlights.is_empty()
    }
}
