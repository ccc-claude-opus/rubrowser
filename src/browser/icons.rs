//! Toolbar icons drawn with lines, centered in a button rectangle.

use crate::gfx::{Color, Painter, Rect};

fn center(r: Rect) -> (i32, i32) {
    (r.x + r.w / 2, r.y + r.h / 2)
}

/// An arrow pointing left (`dir = -1`) or right (`dir = 1`).
pub fn arrow(p: &mut Painter, r: Rect, dir: i32, color: Color) {
    let (cx, cy) = center(r);
    let s = 6;
    p.line(cx - s, cy, cx + s, cy, 2, color);
    let tip = cx + dir * s;
    p.line(tip, cy, tip - dir * 5, cy - 5, 2, color);
    p.line(tip, cy, tip - dir * 5, cy + 5, 2, color);
}

/// A circular arrow.
pub fn reload(p: &mut Painter, r: Rect, color: Color) {
    let (cx, cy) = center(r);
    let rad = 7.0f32;
    let steps = 16;
    // Clockwise (y grows downward) from just right of the top, leaving a gap.
    let start = -1.2f32;
    let end = start + 4.9;
    let pt = |a: f32| {
        (
            cx + (rad * a.cos()).round() as i32,
            cy + (rad * a.sin()).round() as i32,
        )
    };
    let mut prev = pt(start);
    for i in 1..=steps {
        let next = pt(start + (end - start) * i as f32 / steps as f32);
        p.line(prev.0, prev.1, next.0, next.1, 2, color);
        prev = next;
    }
    // Arrow head at the end of the arc, along the clockwise tangent.
    let (tx, ty) = (-end.sin(), end.cos());
    let (hx, hy) = pt(end);
    for side in [-1.0f32, 1.0] {
        let (bx, by) = (-tx, -ty);
        let (c, s) = (0.6f32.cos(), side * 0.6f32.sin());
        let (dx, dy) = (bx * c - by * s, bx * s + by * c);
        p.line(
            hx,
            hy,
            hx + (dx * 5.0).round() as i32,
            hy + (dy * 5.0).round() as i32,
            2,
            color,
        );
    }
}

/// A plus sign.
pub fn plus(p: &mut Painter, r: Rect, color: Color) {
    let (cx, cy) = center(r);
    p.line(cx - 6, cy, cx + 6, cy, 2, color);
    p.line(cx, cy - 6, cx, cy + 6, 2, color);
}

/// A small × mark.
pub fn close(p: &mut Painter, r: Rect, color: Color) {
    let (cx, cy) = center(r);
    p.line(cx - 4, cy - 4, cx + 4, cy + 4, 1, color);
    p.line(cx - 4, cy + 4, cx + 4, cy - 4, 1, color);
}
