//! Serializing computed values for `getComputedStyle` (CSSOM): colors as
//! `rgb()` / `rgba()`, lengths in `px`, keywords as written.

use super::style::*;
use super::values::{Dim, Rgba};

/// Layout results some properties report instead of their computed value.
#[derive(Clone, Copy, Debug, Default)]
pub struct Used {
    /// Content box size, if the element was laid out.
    pub content: Option<(f32, f32)>,
    /// Width of the containing block (for percentages of margins etc.).
    pub base: f32,
}

/// The value of property `prop` in `style`, or `""` for a property this
/// engine does not know (as browsers do for unknown names).
pub fn value(style: &Style, prop: &str, used: Used) -> String {
    let s = style;
    if prop.starts_with("--") {
        return s.vars.get(prop).cloned().unwrap_or_default();
    }
    let side = |name: &str| match name {
        "top" => Some(TOP),
        "right" => Some(RIGHT),
        "bottom" => Some(BOTTOM),
        "left" => Some(LEFT),
        _ => None,
    };
    let len = |d: Dim| dim(d, used.base);
    let border_width = |i: usize| {
        if s.border_style[i] == LineStyle::None {
            0.0
        } else {
            s.border_width[i]
        }
    };
    if let Some(rest) = prop.strip_prefix("border-") {
        if let Some((name, what)) = rest.split_once('-')
            && let Some(i) = side(name)
        {
            return match what {
                "width" => px(border_width(i)),
                "style" => line_style(s.border_style[i]).into(),
                "color" => color(s.border_color[i]),
                _ => String::new(),
            };
        }
    }
    for (prefix, values) in [("margin-", &s.margin), ("padding-", &s.padding)] {
        if let Some(i) = prop.strip_prefix(prefix).and_then(side) {
            return len(values[i]);
        }
    }
    if let Some(i) = side(prop) {
        return len(s.inset[i]);
    }
    match prop {
        "display" => display(s.display).into(),
        "position" => match s.position {
            Position::Static => "static",
            Position::Relative => "relative",
            Position::Absolute => "absolute",
            Position::Fixed => "fixed",
            Position::Sticky => "sticky",
        }
        .into(),
        "visibility" => if s.visible { "visible" } else { "hidden" }.into(),
        "opacity" => number(s.opacity),
        "color" => color(s.color),
        "background-color" => color(s.background),
        "background-image" => match &s.background_image {
            Some(url) => format!("url(\"{url}\")"),
            None => "none".into(),
        },
        "width" | "height" => {
            let i = usize::from(prop == "height");
            match used.content {
                // Rendered boxes report their used size.
                Some(size) if s.display != Display::Inline => px([size.0, size.1][i]),
                _ => len([s.width, s.height][i]),
            }
        }
        "min-width" => len(s.min_width),
        "min-height" => len(s.min_height),
        "max-width" => none_if_auto(s.max_width, used.base),
        "max-height" => none_if_auto(s.max_height, used.base),
        "margin" => four(s.margin.map(len)),
        "padding" => four(s.padding.map(len)),
        "border-width" => four([0, 1, 2, 3].map(|i| px(border_width(i)))),
        "border-radius" => px(s.border_radius),
        "box-sizing" => match s.box_sizing {
            BoxSizing::ContentBox => "content-box",
            BoxSizing::BorderBox => "border-box",
        }
        .into(),
        "overflow" | "overflow-x" | "overflow-y" => match s.overflow {
            Overflow::Visible => "visible",
            Overflow::Hidden => "hidden",
            Overflow::Auto => "auto",
            Overflow::Scroll => "scroll",
        }
        .into(),
        "float" => match s.float {
            Float::None => "none",
            Float::Left => "left",
            Float::Right => "right",
        }
        .into(),
        "clear" => match s.clear {
            (true, true) => "both",
            (true, false) => "left",
            (false, true) => "right",
            _ => "none",
        }
        .into(),
        "z-index" => s.z_index.map_or("auto".into(), |z| z.to_string()),
        "flex-direction" => match s.flex_direction {
            FlexDirection::Row => "row",
            FlexDirection::RowReverse => "row-reverse",
            FlexDirection::Column => "column",
            FlexDirection::ColumnReverse => "column-reverse",
        }
        .into(),
        "flex-wrap" => if s.flex_wrap { "wrap" } else { "nowrap" }.into(),
        "flex-grow" => number(s.flex_grow),
        "flex-shrink" => number(s.flex_shrink),
        "flex-basis" => len(s.flex_basis),
        "justify-content" => align(s.justify_content).into(),
        "align-items" => align(s.align_items).into(),
        "align-self" => match s.align_self {
            Align::Normal => "auto",
            a => align(a),
        }
        .into(),
        "align-content" => align(s.align_content).into(),
        "row-gap" => gap(s.row_gap, used.base),
        "column-gap" => gap(s.column_gap, used.base),
        "font-size" => px(s.font_size),
        "font-weight" => if s.bold { "700" } else { "400" }.into(),
        "font-style" => if s.italic { "italic" } else { "normal" }.into(),
        "font-family" => match s.family {
            crate::text::Family::Mono => "monospace",
            crate::text::Family::Serif => "serif",
            _ => "sans-serif",
        }
        .into(),
        "line-height" => match s.line_height {
            LineHeight::Normal => "normal".into(),
            LineHeight::Number(n) => px(n * s.font_size),
            LineHeight::Px(v) => px(v),
        },
        "text-align" => match s.text_align {
            TextAlign::Start => "start",
            TextAlign::End => "end",
            TextAlign::Left => "left",
            TextAlign::Right => "right",
            TextAlign::Center => "center",
            TextAlign::WebkitCenter => "-webkit-center",
        }
        .into(),
        "text-transform" => match s.text_transform {
            TextTransform::None => "none",
            TextTransform::Uppercase => "uppercase",
            TextTransform::Lowercase => "lowercase",
            TextTransform::Capitalize => "capitalize",
        }
        .into(),
        "text-decoration-line" | "text-decoration" => match (s.underline, s.line_through) {
            (false, false) => "none",
            (true, false) => "underline",
            (false, true) => "line-through",
            (true, true) => "underline line-through",
        }
        .into(),
        "white-space" => match s.white_space {
            WhiteSpace::Normal => "normal",
            WhiteSpace::NoWrap => "nowrap",
            WhiteSpace::Pre => "pre",
            WhiteSpace::PreWrap => "pre-wrap",
        }
        .into(),
        "vertical-align" => match s.vertical_align {
            VerticalAlign::Baseline => "baseline",
            VerticalAlign::Top => "top",
            VerticalAlign::Middle => "middle",
            VerticalAlign::Bottom => "bottom",
            VerticalAlign::Sub => "sub",
            VerticalAlign::Super => "super",
        }
        .into(),
        "object-fit" => match s.object_fit {
            ObjectFit::Fill => "fill",
            ObjectFit::Contain => "contain",
            ObjectFit::Cover => "cover",
            ObjectFit::None => "none",
            ObjectFit::ScaleDown => "scale-down",
        }
        .into(),
        "text-overflow" => if s.ellipsis { "ellipsis" } else { "clip" }.into(),
        "direction" => if s.rtl { "rtl" } else { "ltr" }.into(),
        "cursor" => match s.cursor {
            Cursor::Auto => "auto",
            Cursor::Default => "default",
            Cursor::Pointer => "pointer",
            Cursor::Text => "text",
        }
        .into(),
        "list-style-type" => match s.list_style {
            ListStyle::Disc => "disc",
            ListStyle::Circle => "circle",
            ListStyle::Square => "square",
            ListStyle::Decimal => "decimal",
            ListStyle::None => "none",
            ListStyle::DisclosureClosed => "disclosure-closed",
            ListStyle::DisclosureOpen => "disclosure-open",
        }
        .into(),
        "outline-width" => px(if s.outline_style == LineStyle::None {
            0.0
        } else {
            s.outline_width
        }),
        "outline-style" => line_style(s.outline_style).into(),
        "outline-color" => color(s.outline_color),
        "transform" if s.transform.is_empty() => "none".into(),
        "transform" => {
            // The `transform` functions alone, as one matrix.
            let (w, h) = used.content.unwrap_or_default();
            let plain = Style {
                translate: (Dim::Px(0.0), Dim::Px(0.0)),
                rotate: 0.0,
                scale: (1.0, 1.0),
                transform_origin: (Dim::Px(0.0), Dim::Px(0.0)),
                ..s.clone()
            };
            let m = plain.transform_matrix(w, h).0.map(number);
            format!("matrix({})", m.join(", "))
        }
        "content" => s.content.as_deref().unwrap_or("normal").into(),
        "box-shadow" if s.box_shadow.is_empty() => "none".into(),
        _ => String::new(),
    }
}

fn display(d: Display) -> &'static str {
    match d {
        Display::None => "none",
        Display::Block => "block",
        Display::Inline => "inline",
        Display::InlineBlock => "inline-block",
        Display::Flex => "flex",
        Display::InlineFlex => "inline-flex",
        Display::ListItem => "list-item",
        Display::Grid => "grid",
        Display::InlineGrid => "inline-grid",
        Display::Contents => "contents",
        Display::Table => "table",
        Display::TableRowGroup => "table-row-group",
        Display::TableRow => "table-row",
        Display::TableCell => "table-cell",
        Display::TableCaption => "table-caption",
    }
}

fn line_style(l: LineStyle) -> &'static str {
    match l {
        LineStyle::None => "none",
        LineStyle::Solid => "solid",
        LineStyle::Dashed => "dashed",
        LineStyle::Dotted => "dotted",
        LineStyle::Double => "double",
    }
}

fn align(a: Align) -> &'static str {
    match a {
        Align::Normal => "normal",
        Align::Start => "flex-start",
        Align::End => "flex-end",
        Align::Center => "center",
        Align::Stretch => "stretch",
        Align::Baseline => "baseline",
        Align::SpaceBetween => "space-between",
        Align::SpaceAround => "space-around",
        Align::SpaceEvenly => "space-evenly",
    }
}

/// `rgb(r, g, b)`, or `rgba(r, g, b, a)` when not opaque.
pub fn color(c: Rgba) -> String {
    if c.a == 255 {
        format!("rgb({}, {}, {})", c.r, c.g, c.b)
    } else {
        let a = (c.a as f32 / 255.0 * 1000.0).round() / 1000.0;
        format!("rgba({}, {}, {}, {})", c.r, c.g, c.b, number(a))
    }
}

/// A number the way CSSOM prints it: no trailing `.0`.
fn number(v: f32) -> String {
    let r = (v * 1000.0).round() / 1000.0;
    if r == r.trunc() {
        format!("{}", r as i64)
    } else {
        format!("{r}")
    }
}

fn px(v: f32) -> String {
    format!("{}px", number(v))
}

fn dim(d: Dim, base: f32) -> String {
    match d {
        Dim::Auto => "auto".into(),
        Dim::Px(v) => px(v),
        Dim::Percent(p) if base > 0.0 => px(base * p / 100.0),
        Dim::Percent(p) => format!("{}%", number(p)),
        Dim::Calc(..) | Dim::Expr(_) => px(d.px_or_zero(base)),
    }
}

fn none_if_auto(d: Dim, base: f32) -> String {
    if d == Dim::Auto { "none".into() } else { dim(d, base) }
}

fn gap(d: Dim, base: f32) -> String {
    if d == Dim::Auto { "normal".into() } else { dim(d, base) }
}

/// The shorthand serialization of four sides.
fn four(v: [String; 4]) -> String {
    let [t, r, b, l] = v;
    if r == l {
        if t == b {
            if t == r { t } else { format!("{t} {r}") }
        } else {
            format!("{t} {r} {b}")
        }
    } else {
        format!("{t} {r} {b} {l}")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn serializes_like_cssom() {
        let mut s = Style {
            display: Display::Flex,
            color: Rgba::new(255, 0, 0, 255),
            background: Rgba::new(0, 0, 0, 128),
            opacity: 0.5,
            ..Style::default()
        };
        s.margin = [Dim::Px(1.0), Dim::Px(2.0), Dim::Px(1.0), Dim::Px(2.0)];
        let v = |s: &Style, p| value(s, p, Used::default());
        assert_eq!(v(&s, "display"), "flex");
        assert_eq!(v(&s, "color"), "rgb(255, 0, 0)");
        assert_eq!(v(&s, "background-color"), "rgba(0, 0, 0, 0.502)");
        assert_eq!(v(&s, "opacity"), "0.5");
        assert_eq!(v(&s, "margin"), "1px 2px");
        assert_eq!(v(&s, "margin-left"), "2px");
        assert_eq!(v(&s, "border-top-width"), "0px");
        assert_eq!(v(&s, "no-such-property"), "");
        let used = Used {
            content: Some((120.0, 30.5)),
            base: 400.0,
        };
        assert_eq!(value(&s, "width", used), "120px");
        assert_eq!(value(&s, "height", used), "30.5px");
    }
}
