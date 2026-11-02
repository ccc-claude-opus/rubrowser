//! Multi-column layout (CSS Multicol): the content is laid out as one
//! column of the column width, a balanced column height is chosen, and the
//! lines and boxes are then moved into their columns. Each step needs the
//! previous layout pass (see [`super::layout`]).

use super::LBox;
use super::inline::Placed;
use crate::css::style::{self as cs, Style};

/// What a pass learned about a multi-column element.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Plan {
    /// Width of its content box (all columns and gaps).
    pub avail: f32,
    /// The balanced column height, once known.
    pub height: Option<f32>,
}

/// Whether `s` makes a multi-column container.
pub fn is_multicol(s: &Style) -> bool {
    s.column_count.is_some_and(|n| n > 1) || s.column_width.is_some()
}

/// The gap between columns (`normal` is 1em).
pub fn gap(s: &Style, avail: f32) -> f32 {
    match s.column_gap {
        crate::css::values::Dim::Auto => s.font_size,
        d => d.px_or_zero(avail),
    }
}

/// Column count and width in `avail` px (CSS Multicol's pseudo-algorithm).
pub fn columns(s: &Style, avail: f32) -> (u32, f32) {
    let g = gap(s, avail);
    let n = match (s.column_count, s.column_width) {
        (Some(n), None) => n,
        (count, Some(w)) => {
            let fit = ((avail + g) / (w + g)).floor().max(1.0) as u32;
            count.map_or(fit, |n| n.min(fit))
        }
        (None, None) => 1,
    }
    .max(1);
    let width = ((avail - g * (n - 1) as f32) / n as f32).max(0.0);
    (n, width)
}

/// A piece of the flow: a child box, or a line (of the container or of a
/// child with only inline content), with its top and bottom relative to
/// the container's content box.
struct Piece {
    child: Option<usize>,
    line: Option<usize>,
    top: f32,
    bottom: f32,
}

fn pieces(boxes: &[LBox], index: usize) -> Vec<Piece> {
    let b = &boxes[index];
    let (_, cy) = b.content_offset();
    let mut out = Vec::new();
    if let Some(il) = &b.inline {
        for (k, l) in il.lines.iter().enumerate() {
            out.push(Piece {
                child: None,
                line: Some(k),
                top: l.top,
                bottom: l.top + l.height,
            });
        }
    }
    for &k in &b.children {
        let kb = &boxes[k];
        if kb.absolute || kb.fixed {
            continue;
        }
        let top = kb.rect.y - cy;
        match &kb.inline {
            Some(il) if !il.lines.is_empty() => {
                let (_, kcy) = kb.content_offset();
                for (n, l) in il.lines.iter().enumerate() {
                    out.push(Piece {
                        child: Some(k),
                        line: Some(n),
                        top: top + kcy + l.top,
                        bottom: top + kcy + l.top + l.height,
                    });
                }
            }
            _ => out.push(Piece {
                child: Some(k),
                line: None,
                top,
                bottom: top + kb.rect.h,
            }),
        }
    }
    out.sort_by(|a, b| a.top.total_cmp(&b.top));
    out
}

/// The column each piece goes to with columns `height` tall, and each
/// column's starting offset; `None` if more than `n` columns are needed.
fn assign(pieces: &[Piece], height: f32, n: u32) -> Option<(Vec<usize>, Vec<f32>)> {
    let first = pieces.first()?.top;
    let mut starts = vec![first];
    let mut cols = Vec::with_capacity(pieces.len());
    for p in pieces {
        let start = *starts.last().unwrap();
        if p.bottom - start > height + 0.5 && p.top > start {
            if starts.len() as u32 >= n {
                return None;
            }
            starts.push(p.top);
        }
        cols.push(starts.len() - 1);
    }
    Some((cols, starts))
}

/// The balanced column height for the single-column content of `index`.
pub fn balance(boxes: &[LBox], index: usize, n: u32) -> f32 {
    let pieces = pieces(boxes, index);
    let Some(first) = pieces.first().map(|p| p.top) else {
        return 0.0;
    };
    let total = pieces.iter().map(|p| p.bottom).fold(0.0, f32::max) - first;
    let tallest = pieces.iter().map(|p| p.bottom - p.top).fold(0.0, f32::max);
    let mut h = (total / n as f32).max(tallest).ceil();
    for _ in 0..200 {
        if assign(&pieces, h, n).is_some() {
            return h;
        }
        h += 2.0;
    }
    total
}

/// Moves the pieces of `index` into `n` columns `width` wide, `gap`
/// apart, `height` tall.
pub fn distribute(boxes: &mut [LBox], index: usize, n: u32, width: f32, gap: f32, height: f32) {
    let pieces = pieces(boxes, index);
    let Some((cols, starts)) = assign(&pieces, height, n) else {
        return;
    };
    for (p, &col) in pieces.iter().zip(&cols) {
        if col == 0 {
            continue;
        }
        let dx = col as f32 * (width + gap);
        let dy = starts[0] - starts[col];
        let shift = |line: &mut super::inline::Line| {
            line.top += dy;
            for item in &mut line.items {
                match item {
                    Placed::Text { x, .. } | Placed::Atomic { x, .. } => *x += dx,
                }
            }
        };
        match (p.child, p.line) {
            (None, Some(l)) => {
                if let Some(il) = boxes[index].inline.as_mut() {
                    shift(&mut il.lines[l]);
                }
            }
            (Some(k), Some(l)) => {
                if let Some(il) = boxes[k].inline.as_mut() {
                    shift(&mut il.lines[l]);
                }
            }
            (Some(k), None) => {
                boxes[k].rect.x += dx;
                boxes[k].rect.y += dy;
            }
            (None, None) => {}
        }
    }
}

/// The x positions (relative to the content box) of the rules between
/// columns, if `s` draws them.
pub fn rules(s: &Style, n: u32, width: f32, gap: f32) -> Vec<f32> {
    if s.column_rule.1 == cs::LineStyle::None || s.column_rule.0 <= 0.0 {
        return Vec::new();
    }
    (1..n).map(|k| k as f32 * (width + gap) - gap / 2.0).collect()
}
