//! Computed style of an element and how declarations change it.

use std::collections::BTreeMap;
use std::rc::Rc;

use super::values::{self, Dim, FontContext, Rgba, four, tokens};
use crate::text::{Family, FontKey};

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Display {
    None,
    Block,
    #[default]
    Inline,
    InlineBlock,
    Flex,
    InlineFlex,
    ListItem,
    Grid,
    InlineGrid,
    /// `display: contents`: the box is replaced by its children.
    Contents,
    Table,
    TableRowGroup,
    TableRow,
    TableCell,
    TableCaption,
}

impl Display {
    /// Participates in inline layout (flows with text).
    pub fn is_inline_level(&self) -> bool {
        matches!(
            self,
            Display::Inline | Display::InlineBlock | Display::InlineFlex | Display::InlineGrid
        )
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Position {
    #[default]
    Static,
    Relative,
    Absolute,
    /// Positioned against the viewport; does not scroll.
    Fixed,
    /// In flow, then kept inside the viewport while its parent is visible.
    Sticky,
}

/// How a border or outline line is drawn.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum LineStyle {
    #[default]
    None,
    Solid,
    Dashed,
    Dotted,
    Double,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum TextTransform {
    #[default]
    None,
    Uppercase,
    Lowercase,
    Capitalize,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Float {
    #[default]
    None,
    Left,
    Right,
}

/// `background-size`.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub enum BackgroundSize {
    #[default]
    Auto,
    Cover,
    Contain,
    Size(Dim, Dim),
}

/// `background-repeat`: (repeat x, repeat y).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Repeat(pub bool, pub bool);

/// One `box-shadow`.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Shadow {
    pub x: f32,
    pub y: f32,
    pub blur: f32,
    pub spread: f32,
    pub color: Rgba,
    pub inset: bool,
}

/// `mask-image` and its size, position and repeat (the image's alpha
/// masks the element's background color; used for icons).
#[derive(Clone, Debug, PartialEq)]
pub struct Mask {
    pub image: Option<String>,
    pub size: BackgroundSize,
    pub repeat: Repeat,
    pub position: (Dim, Dim),
}

impl Default for Mask {
    fn default() -> Self {
        Mask {
            image: None,
            size: BackgroundSize::Auto,
            repeat: Repeat(true, true),
            position: (Dim::Percent(0.0), Dim::Percent(0.0)),
        }
    }
}

impl Default for Repeat {
    fn default() -> Self {
        Repeat(true, true)
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum BoxSizing {
    #[default]
    ContentBox,
    BorderBox,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Overflow {
    #[default]
    Visible,
    Hidden,
    Auto,
    Scroll,
}

impl Overflow {
    pub fn scrolls(&self) -> bool {
        matches!(self, Overflow::Auto | Overflow::Scroll)
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum TextAlign {
    /// `start`: left, or right in right-to-left text.
    #[default]
    Start,
    End,
    Left,
    Right,
    Center,
    /// `-webkit-center` (`<center>`, `align=center`): centers text and
    /// block children too.
    WebkitCenter,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum WhiteSpace {
    #[default]
    Normal,
    NoWrap,
    Pre,
    PreWrap,
}

impl WhiteSpace {
    pub fn collapses(&self) -> bool {
        matches!(self, WhiteSpace::Normal | WhiteSpace::NoWrap)
    }
    pub fn wraps(&self) -> bool {
        matches!(self, WhiteSpace::Normal | WhiteSpace::PreWrap)
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub enum LineHeight {
    #[default]
    Normal,
    Number(f32),
    Px(f32),
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Cursor {
    #[default]
    Auto,
    Default,
    Pointer,
    Text,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum ListStyle {
    #[default]
    Disc,
    Circle,
    Square,
    Decimal,
    None,
    /// `<summary>` of a closed / open `<details>`.
    DisclosureClosed,
    DisclosureOpen,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum VerticalAlign {
    #[default]
    Baseline,
    Top,
    Middle,
    Bottom,
    Sub,
    Super,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum FlexDirection {
    #[default]
    Row,
    RowReverse,
    Column,
    ColumnReverse,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Align {
    /// `normal` / `auto`: the container's default.
    #[default]
    Normal,
    Start,
    End,
    Center,
    Stretch,
    Baseline,
    SpaceBetween,
    SpaceAround,
    SpaceEvenly,
}

/// Grid properties as written (with `var()` already substituted).
#[derive(Clone, Debug, Default, PartialEq)]
pub struct GridProps {
    pub template_columns: Option<String>,
    pub template_rows: Option<String>,
    pub template_areas: Option<String>,
    pub auto_columns: Option<String>,
    pub auto_rows: Option<String>,
    pub auto_flow: Option<String>,
    pub row_start: Option<String>,
    pub row_end: Option<String>,
    pub column_start: Option<String>,
    pub column_end: Option<String>,
}

/// Index into four-sided properties.
pub const TOP: usize = 0;
pub const RIGHT: usize = 1;
pub const BOTTOM: usize = 2;
pub const LEFT: usize = 3;

/// The computed style of one element. Text nodes use their parent's.
#[derive(Clone, Debug, PartialEq)]
pub struct Style {
    pub display: Display,
    pub position: Position,
    pub inset: [Dim; 4],
    pub margin: [Dim; 4],
    pub padding: [Dim; 4],
    pub border_width: [f32; 4],
    pub border_color: [Rgba; 4],
    pub border_style: [LineStyle; 4],
    pub border_radius: f32,
    pub width: Dim,
    pub height: Dim,
    pub min_width: Dim,
    pub min_height: Dim,
    pub max_width: Dim,
    pub max_height: Dim,
    pub box_sizing: BoxSizing,
    pub background: Rgba,
    pub overflow: Overflow,
    pub flex_direction: FlexDirection,
    pub flex_wrap: bool,
    pub justify_content: Align,
    pub align_items: Align,
    pub align_self: Align,
    pub row_gap: Dim,
    pub column_gap: Dim,
    pub flex_grow: f32,
    pub flex_shrink: f32,
    pub flex_basis: Dim,
    pub vertical_align: VerticalAlign,
    pub opacity: f32,
    /// `translate` / `transform: translate…()` (percentages of the border
    /// box); applied when painting and hit-testing.
    pub translate: (Dim, Dim),
    /// `aspect-ratio` (width / height).
    pub aspect_ratio: Option<f32>,
    /// `clip` / `clip-path` that leave nothing visible (the usual
    /// "visually hidden" pattern): the box and its contents are not drawn.
    pub clipped_away: bool,
    pub z_index: Option<i32>,
    pub float: Float,
    /// `clear`: (left, right).
    pub clear: (bool, bool),
    pub outline_width: f32,
    pub outline_style: LineStyle,
    pub outline_color: Rgba,
    pub outline_offset: f32,
    pub background_image: Option<String>,
    /// A gradient background layer, and whether it is listed before (so
    /// painted above) `background_image`.
    pub background_gradient: Option<(Rc<super::gradient::Gradient>, bool)>,
    pub box_shadow: Rc<[Shadow]>,
    pub background_size: BackgroundSize,
    pub background_repeat: Repeat,
    /// `background-position` as fractions (0 = left/top, 1 = right/bottom)
    /// or pixel offsets.
    pub background_position: (Dim, Dim),
    pub mask: Rc<Mask>,
    /// Grid container and item properties, kept as CSS text and parsed at
    /// layout time.
    pub grid: Rc<GridProps>,
    pub justify_items: Align,
    pub justify_self: Align,
    pub align_content: Align,
    // Inherited:
    pub text_transform: TextTransform,
    pub border_spacing: f32,
    pub border_collapse: bool,
    pub color: Rgba,
    pub font_size: f32,
    pub bold: bool,
    pub italic: bool,
    pub family: Family,
    pub line_height: LineHeight,
    pub text_align: TextAlign,
    pub white_space: WhiteSpace,
    pub underline: bool,
    pub line_through: bool,
    pub cursor: Cursor,
    pub list_style: ListStyle,
    /// `direction: rtl` (inherited).
    pub rtl: bool,
    pub visible: bool,
    /// Custom properties (`--name`), inherited.
    pub vars: Rc<BTreeMap<String, String>>,
    /// Whether any declaration applied (used for pseudo-elements).
    pub has_rules: bool,
    /// Raw `content`, `counter-reset` and `counter-increment` values
    /// (evaluated by [`super::generated_text`] and the counter walk).
    pub content: Option<Rc<str>>,
    pub counter_reset: Option<Rc<str>>,
    pub counter_increment: Option<Rc<str>>,
}

impl Default for Style {
    fn default() -> Self {
        Style {
            display: Display::Inline,
            position: Position::Static,
            inset: [Dim::Auto; 4],
            margin: [Dim::Px(0.0); 4],
            padding: [Dim::Px(0.0); 4],
            border_width: [3.0; 4],
            border_color: [Rgba::BLACK; 4],
            border_style: [LineStyle::None; 4],
            border_radius: 0.0,
            width: Dim::Auto,
            height: Dim::Auto,
            min_width: Dim::Auto,
            min_height: Dim::Auto,
            max_width: Dim::Auto,
            max_height: Dim::Auto,
            box_sizing: BoxSizing::ContentBox,
            background: Rgba::TRANSPARENT,
            overflow: Overflow::Visible,
            flex_direction: FlexDirection::Row,
            flex_wrap: false,
            justify_content: Align::Normal,
            align_items: Align::Normal,
            align_self: Align::Normal,
            row_gap: Dim::Px(0.0),
            column_gap: Dim::Px(0.0),
            flex_grow: 0.0,
            flex_shrink: 1.0,
            flex_basis: Dim::Auto,
            vertical_align: VerticalAlign::Baseline,
            opacity: 1.0,
            translate: (Dim::Px(0.0), Dim::Px(0.0)),
            aspect_ratio: None,
            clipped_away: false,
            z_index: None,
            float: Float::None,
            clear: (false, false),
            outline_width: 3.0,
            outline_style: LineStyle::None,
            outline_color: Rgba::BLACK,
            outline_offset: 0.0,
            background_image: None,
            background_gradient: None,
            box_shadow: Rc::new([]),
            background_size: BackgroundSize::Auto,
            background_repeat: Repeat(true, true),
            background_position: (Dim::Percent(0.0), Dim::Percent(0.0)),
            mask: Rc::default(),
            grid: Rc::default(),
            justify_items: Align::Normal,
            justify_self: Align::Normal,
            align_content: Align::Normal,
            text_transform: TextTransform::None,
            border_spacing: 0.0,
            border_collapse: false,
            color: Rgba::BLACK,
            font_size: 16.0,
            bold: false,
            italic: false,
            family: Family::Sans,
            line_height: LineHeight::Normal,
            text_align: TextAlign::Start,
            white_space: WhiteSpace::Normal,
            underline: false,
            line_through: false,
            cursor: Cursor::Auto,
            list_style: ListStyle::Disc,
            rtl: false,
            visible: true,
            vars: Rc::default(),
            has_rules: false,
            content: None,
            counter_reset: None,
            counter_increment: None,
        }
    }
}

impl Style {
    /// Initial values, with inherited properties taken from `parent`.
    pub fn inherit(parent: &Style) -> Style {
        Style {
            color: parent.color,
            font_size: parent.font_size,
            bold: parent.bold,
            italic: parent.italic,
            family: parent.family,
            line_height: parent.line_height,
            text_align: parent.text_align,
            white_space: parent.white_space,
            underline: parent.underline,
            line_through: parent.line_through,
            cursor: parent.cursor,
            list_style: parent.list_style,
            rtl: parent.rtl,
            // Clipping to nothing hides everything inside too.
            clipped_away: parent.clipped_away,
            visible: parent.visible,
            border_spacing: parent.border_spacing,
            border_collapse: parent.border_collapse,
            text_transform: parent.text_transform,
            vars: parent.vars.clone(),
            ..Style::default()
        }
    }

    pub fn font(&self) -> FontKey {
        FontKey {
            family: self.family,
            bold: self.bold,
            italic: self.italic,
        }
    }

    /// Line height in pixels.
    pub fn line_height_px(&self) -> f32 {
        match self.line_height {
            LineHeight::Normal => self.font_size * 1.2,
            LineHeight::Number(n) => self.font_size * n,
            LineHeight::Px(v) => v,
        }
    }

    /// Border width of a side, zero if its style draws nothing.
    pub fn border(&self, side: usize) -> f32 {
        if self.border_style[side] != LineStyle::None {
            self.border_width[side]
        } else {
            0.0
        }
    }

    /// Applies one declaration. `parent` serves `inherit`; `root_font_size`
    /// serves `rem`. Unknown properties and invalid values are ignored.
    pub fn apply(&mut self, name: &str, value: &str, parent: &Style, root_font_size: f32) {
        let env = super::ValueEnv {
            root_font: root_font_size,
            vw: 1280.0,
            vh: 800.0,
        };
        self.apply_in(name, value, parent, &env);
    }

    /// Applies one declaration with the viewport known (for `vw`/`vh`).
    pub fn apply_in(&mut self, name: &str, value: &str, parent: &Style, env: &super::ValueEnv) {
        if let Some(physical) = logical(name, value) {
            for (n, v) in physical {
                self.apply_in(&n, &v, parent, env);
            }
            return;
        }
        let root_font_size = env.root_font;
        let value = value.trim();
        let lower = value.to_ascii_lowercase();
        match lower.as_str() {
            "inherit" => return self.copy_from(name, parent),
            "initial" => return self.copy_from(name, &Style::default()),
            "unset" => {
                let from = if is_inherited(name) {
                    parent.clone()
                } else {
                    Style::default()
                };
                return self.copy_from(name, &from);
            }
            _ => {}
        }
        // `em` in font-size refers to the parent's size, elsewhere to our own.
        let fc = FontContext {
            em: self.font_size,
            rem: root_font_size,
            vw: env.vw,
            vh: env.vh,
        };
        let v = lower.as_str();
        let dim = |s: &str| values::dim(s, fc);
        let px = |s: &str| values::px(s, fc);
        match name {
            "display" => {
                self.display = match v {
                    "none" => Display::None,
                    "block" | "flow-root" => Display::Block,
                    "table" | "inline-table" => Display::Table,
                    "table-row-group" | "table-header-group" | "table-footer-group" => {
                        Display::TableRowGroup
                    }
                    "table-row" => Display::TableRow,
                    "table-cell" => Display::TableCell,
                    "table-caption" => Display::TableCaption,
                    "table-column" | "table-column-group" => Display::None,
                    "inline" => Display::Inline,
                    "inline-block" => Display::InlineBlock,
                    "flex" | "-webkit-flex" => Display::Flex,
                    "inline-flex" | "-webkit-inline-flex" => Display::InlineFlex,
                    "grid" => Display::Grid,
                    "inline-grid" => Display::InlineGrid,
                    "contents" => Display::Contents,
                    "list-item" => Display::ListItem,
                    _ => return,
                }
            }
            "position" => {
                self.position = match v {
                    "static" => Position::Static,
                    "relative" => Position::Relative,
                    "sticky" | "-webkit-sticky" => Position::Sticky,
                    "absolute" => Position::Absolute,
                    "fixed" => Position::Fixed,
                    _ => return,
                }
            }
            "top" | "right" | "bottom" | "left" => {
                if let Some(d) = dim(v) {
                    self.inset[side(name)] = d;
                }
            }
            "inset" => set4(&mut self.inset, v, dim),
            "margin" => set4(&mut self.margin, v, dim),
            "padding" => set4(&mut self.padding, v, dim),
            "margin-top" | "margin-right" | "margin-bottom" | "margin-left" => {
                if let Some(d) = dim(v) {
                    self.margin[side(name)] = d;
                }
            }
            "padding-top" | "padding-right" | "padding-bottom" | "padding-left" => {
                if let Some(d) = dim(v).filter(|d| *d != Dim::Auto) {
                    self.padding[side(name)] = d;
                }
            }
            "border" => {
                for s in 0..4 {
                    self.border_side(s, value, fc);
                }
            }
            "border-top" | "border-right" | "border-bottom" | "border-left" => {
                self.border_side(side(name), value, fc)
            }
            "border-width" => set4(&mut self.border_width, v, |t| border_width(t, fc)),
            "border-style" => set4(&mut self.border_style, v, border_style),
            "border-color" => {
                let current = self.color;
                set4(&mut self.border_color, value, |t| values::color(t, current))
            }
            "border-top-width"
            | "border-right-width"
            | "border-bottom-width"
            | "border-left-width" => {
                if let Some(w) = border_width(v, fc) {
                    self.border_width[side(name)] = w;
                }
            }
            "border-top-style"
            | "border-right-style"
            | "border-bottom-style"
            | "border-left-style" => {
                if let Some(s) = border_style(v) {
                    self.border_style[side(name)] = s;
                }
            }
            "border-top-color"
            | "border-right-color"
            | "border-bottom-color"
            | "border-left-color" => {
                if let Some(c) = values::color(value, self.color) {
                    self.border_color[side(name)] = c;
                }
            }
            "border-radius" => {
                if let Some(r) = tokens(v).first().and_then(|t| px(t)) {
                    self.border_radius = r;
                }
            }
            "width" => set(&mut self.width, dim(v)),
            "height" => set(&mut self.height, dim(v)),
            "min-width" => set(&mut self.min_width, dim(v)),
            "min-height" => set(&mut self.min_height, dim(v)),
            "max-width" => set(&mut self.max_width, dim(v)),
            "max-height" => set(&mut self.max_height, dim(v)),
            "box-sizing" => {
                self.box_sizing = match v {
                    "border-box" => BoxSizing::BorderBox,
                    "content-box" => BoxSizing::ContentBox,
                    _ => return,
                }
            }
            "background-color" => set(&mut self.background, values::color(value, self.color)),
            "background" => {
                self.background = Rgba::TRANSPARENT;
                self.background_image = None;
                self.background_gradient = None;
                let toks: Vec<&str> = super::gradient::split_commas(value)
                    .into_iter()
                    .flat_map(tokens)
                    .collect();
                let mut positions = Vec::new();
                let mut after_slash = false;
                for t in &toks {
                    let lt = t.to_ascii_lowercase();
                    let t = &t.trim_end_matches(',');
                    let lt = lt.trim_end_matches(',').to_string();
                    if let Some(u) = url_value(t) {
                        if self.background_image.is_none() {
                            self.background_image = Some(u);
                        }
                    } else if lt.contains("gradient(") {
                        if self.background_gradient.is_none()
                            && let Some(g) = super::gradient::parse(t, self.color, fc)
                        {
                            let on_top = self.background_image.is_none();
                            self.background_gradient = Some((Rc::new(g), on_top));
                        }
                    } else if let Some(r) = repeat(&lt) {
                        self.background_repeat = r;
                    } else if *t == "/" {
                        after_slash = true;
                    } else if after_slash {
                        if let Some(sz) = background_size(&lt, fc) {
                            self.background_size = sz;
                        }
                    } else if let Some(c) = values::color(t, self.color) {
                        self.background = c;
                    } else {
                        positions.push(lt);
                    }
                }
                if !positions.is_empty() {
                    let p: Vec<&str> = positions.iter().map(String::as_str).collect();
                    if let Some(pos) = background_position(&p, fc) {
                        self.background_position = pos;
                    }
                }
            }
            "background-image" => {
                let layers: Vec<&str> = super::gradient::split_commas(value);
                self.background_image = layers.iter().find_map(|t| url_value(t));
                self.background_gradient = layers.iter().enumerate().find_map(|(i, t)| {
                    let g = super::gradient::parse(t, self.color, fc)?;
                    let on_top = !layers[..i].iter().any(|t| url_value(t).is_some());
                    Some((Rc::new(g), on_top))
                });
            }
            "mask-image" | "-webkit-mask-image" => {
                Rc::make_mut(&mut self.mask).image = url_value(value)
            }
            "mask" | "-webkit-mask" => {
                let m = Rc::make_mut(&mut self.mask);
                m.image = tokens(value).iter().find_map(|t| url_value(t));
            }
            "mask-repeat" | "-webkit-mask-repeat" => {
                if let Some(r) = repeat(v) {
                    Rc::make_mut(&mut self.mask).repeat = r;
                }
            }
            "mask-size" | "-webkit-mask-size" => {
                if let Some(sz) = background_size(v, fc) {
                    Rc::make_mut(&mut self.mask).size = sz;
                }
            }
            "mask-position" | "-webkit-mask-position" => {
                let t = tokens(v);
                if let Some(p) = background_position(&t, fc) {
                    Rc::make_mut(&mut self.mask).position = p;
                }
            }
            "background-repeat" => set(&mut self.background_repeat, repeat(v)),
            "background-size" => set(&mut self.background_size, background_size(v, fc)),
            "background-position" => {
                let t = tokens(v);
                set(&mut self.background_position, background_position(&t, fc));
            }
            "box-shadow" => {
                let mut out = Vec::new();
                if v != "none" {
                    for part in super::gradient::split_commas(value) {
                        let mut sh = Shadow {
                            x: 0.0,
                            y: 0.0,
                            blur: 0.0,
                            spread: 0.0,
                            color: self.color,
                            inset: false,
                        };
                        let mut lens = Vec::new();
                        for t in tokens(part) {
                            if t.eq_ignore_ascii_case("inset") {
                                sh.inset = true;
                            } else if let Some(px) = values::px(t, fc) {
                                lens.push(px);
                            } else if let Some(c) = values::color(t, self.color) {
                                sh.color = c;
                            }
                        }
                        if lens.len() < 2 {
                            continue;
                        }
                        sh.x = lens[0];
                        sh.y = lens[1];
                        sh.blur = lens.get(2).copied().unwrap_or(0.0);
                        sh.spread = lens.get(3).copied().unwrap_or(0.0);
                        out.push(sh);
                    }
                }
                self.box_shadow = out.into();
            }
            "clip" => {
                // rect(0 0 0 0), rect(1px, 1px, 1px, 1px): an empty area.
                self.clipped_away = v.strip_prefix("rect(").is_some_and(|r| {
                    let n: Vec<f32> = r
                        .trim_end_matches(')')
                        .split([',', ' '])
                        .filter(|t| !t.is_empty())
                        .filter_map(|t| values::px(t, fc))
                        .collect();
                    n.len() == 4 && (n[2] - n[0] <= 1.0 || n[1] - n[3] <= 1.0)
                });
            }
            "clip-path" => {
                self.clipped_away = v.starts_with("inset(50%")
                    || v.starts_with("inset(100%")
                    || v.replace(' ', "") == "polygon(0000,0000,0000)"
                    || v.replace(' ', "").starts_with("circle(0");
            }
            "aspect-ratio" => {
                // `auto`, `16 / 9`, `1.5`, `auto 4 / 3`
                let r = v.trim_start_matches("auto").trim();
                let mut parts = r.split('/').map(|p| p.trim().parse::<f32>().ok());
                self.aspect_ratio = match (parts.next().flatten(), parts.next()) {
                    (Some(w), None) if w > 0.0 => Some(w),
                    (Some(w), Some(Some(h))) if w > 0.0 && h > 0.0 => Some(w / h),
                    _ => None,
                };
            }
            "translate" => {
                let t = tokens(value);
                let x = t.first().and_then(|v| dim(v)).unwrap_or(Dim::Px(0.0));
                let y = t.get(1).and_then(|v| dim(v)).unwrap_or(Dim::Px(0.0));
                self.translate = if v == "none" {
                    (Dim::Px(0.0), Dim::Px(0.0))
                } else {
                    (x, y)
                };
            }
            "transform" => self.translate = transform_translate(value, |v| dim(v)),
            "opacity" => {
                if let Some(o) = values::number(v).or_else(|| {
                    v.strip_suffix('%')
                        .and_then(values::number)
                        .map(|p| p / 100.0)
                }) {
                    self.opacity = o.clamp(0.0, 1.0);
                }
            }
            "z-index" => self.z_index = v.parse().ok(),
            "float" => {
                self.float = match v {
                    "left" | "inline-start" => Float::Left,
                    "right" | "inline-end" => Float::Right,
                    "none" => Float::None,
                    _ => return,
                }
            }
            "clear" => {
                self.clear = match v {
                    "left" => (true, false),
                    "right" => (false, true),
                    "both" => (true, true),
                    "none" => (false, false),
                    _ => return,
                }
            }
            "outline" => {
                self.outline_style = LineStyle::None;
                self.outline_width = 3.0;
                self.outline_color = self.color;
                for t in tokens(value) {
                    let lt = t.to_ascii_lowercase();
                    if let Some(w) = border_width(&lt, fc) {
                        self.outline_width = w;
                    } else if let Some(st) = border_style(&lt) {
                        self.outline_style = st;
                    } else if let Some(c) = values::color(t, self.color) {
                        self.outline_color = c;
                    }
                }
            }
            "outline-width" => set(&mut self.outline_width, border_width(v, fc)),
            "outline-style" => set(&mut self.outline_style, border_style(v)),
            "outline-color" => set(&mut self.outline_color, values::color(value, self.color)),
            "outline-offset" => set(&mut self.outline_offset, px(v)),
            "text-transform" => {
                self.text_transform = match v {
                    "uppercase" => TextTransform::Uppercase,
                    "lowercase" => TextTransform::Lowercase,
                    "capitalize" => TextTransform::Capitalize,
                    "none" => TextTransform::None,
                    _ => return,
                }
            }
            "overflow" | "overflow-y" => {
                let t = tokens(v);
                let last = t.last().copied().unwrap_or("");
                self.overflow = match last {
                    "visible" => Overflow::Visible,
                    "hidden" | "clip" => Overflow::Hidden,
                    "auto" => Overflow::Auto,
                    "scroll" => Overflow::Scroll,
                    _ => return,
                }
            }
            "flex-direction" => set(&mut self.flex_direction, flex_direction(v)),
            "flex-wrap" => self.flex_wrap = v == "wrap" || v == "wrap-reverse",
            "flex-flow" => {
                for t in tokens(v) {
                    if let Some(d) = flex_direction(t) {
                        self.flex_direction = d;
                    } else {
                        self.flex_wrap = t == "wrap" || t == "wrap-reverse";
                    }
                }
            }
            "justify-content" => set(&mut self.justify_content, align(v)),
            "justify-items" => set(
                &mut self.justify_items,
                align(v.trim_start_matches("legacy ")),
            ),
            "justify-self" => set(&mut self.justify_self, align(v)),
            "align-content" => set(&mut self.align_content, align(v)),
            "place-items" | "place-self" | "place-content" => {
                let t = tokens(v);
                let a = t.first().copied().unwrap_or("");
                let j = t.get(1).or(t.first()).copied().unwrap_or("");
                match name {
                    "place-items" => {
                        set(&mut self.align_items, align(a));
                        set(&mut self.justify_items, align(j));
                    }
                    "place-self" => {
                        set(&mut self.align_self, align(a));
                        set(&mut self.justify_self, align(j));
                    }
                    _ => {
                        set(&mut self.align_content, align(a));
                        set(&mut self.justify_content, align(j));
                    }
                }
            }
            n if n.starts_with("grid") && !n.ends_with("gap") => self.grid_property(n, value),
            "align-items" => set(&mut self.align_items, align(v)),
            "align-self" => set(&mut self.align_self, align(v)),
            "gap" | "grid-gap" => {
                let t: Vec<_> = tokens(v).into_iter().filter_map(dim).collect();
                match t[..] {
                    [g] => (self.row_gap, self.column_gap) = (g, g),
                    [r, c] => (self.row_gap, self.column_gap) = (r, c),
                    _ => {}
                }
            }
            "row-gap" | "grid-row-gap" => set(&mut self.row_gap, dim(v)),
            "column-gap" | "grid-column-gap" => set(&mut self.column_gap, dim(v)),
            "flex-grow" => set(&mut self.flex_grow, values::number(v)),
            "flex-shrink" => set(&mut self.flex_shrink, values::number(v)),
            "flex-basis" => set(&mut self.flex_basis, dim(v)),
            "flex" => self.flex(v, fc),
            "color" => {
                let parent_color = parent.color;
                set(&mut self.color, values::color(value, parent_color))
            }
            "font-size" => {
                let pfc = FontContext {
                    em: parent.font_size,
                    rem: root_font_size,
                    vw: env.vw,
                    vh: env.vh,
                };
                if let Some(s) = font_size(v, parent.font_size, pfc) {
                    self.font_size = s;
                }
            }
            "font-weight" => set(&mut self.bold, font_weight(v, parent.bold)),
            "font-style" => self.italic = v == "italic" || v.starts_with("oblique"),
            "font-family" => self.family = font_family(v),
            "font" => self.font_shorthand(v, parent, env),
            "line-height" => set(&mut self.line_height, line_height(v, fc)),
            "text-align" => {
                self.text_align = match v {
                    "start" | "justify" => TextAlign::Start,
                    "end" => TextAlign::End,
                    "left" => TextAlign::Left,
                    "right" => TextAlign::Right,
                    "center" => TextAlign::Center,
                    "-webkit-center" | "-moz-center" => TextAlign::WebkitCenter,
                    _ => return,
                }
            }
            "white-space" => {
                self.white_space = match v {
                    "normal" => WhiteSpace::Normal,
                    "nowrap" => WhiteSpace::NoWrap,
                    "pre" => WhiteSpace::Pre,
                    "pre-wrap" | "pre-line" | "break-spaces" => WhiteSpace::PreWrap,
                    _ => return,
                }
            }
            "text-decoration" | "text-decoration-line" => {
                let t = tokens(v);
                self.underline = t.contains(&"underline");
                self.line_through = t.contains(&"line-through");
            }
            "cursor" => {
                self.cursor = match v {
                    "auto" => Cursor::Auto,
                    "pointer" => Cursor::Pointer,
                    "text" => Cursor::Text,
                    _ => Cursor::Default,
                }
            }
            "list-style" | "list-style-type" => {
                for t in tokens(v) {
                    let l = match t {
                        "disc" => ListStyle::Disc,
                        "circle" => ListStyle::Circle,
                        "square" => ListStyle::Square,
                        "decimal" => ListStyle::Decimal,
                        "disclosure-closed" => ListStyle::DisclosureClosed,
                        "disclosure-open" => ListStyle::DisclosureOpen,
                        "none" => ListStyle::None,
                        _ => continue,
                    };
                    self.list_style = l;
                }
            }
            "visibility" => self.visible = v != "hidden" && v != "collapse",
            "direction" => match v {
                "rtl" => self.rtl = true,
                "ltr" => self.rtl = false,
                _ => {}
            },
            "content" => self.content = Some(value.into()),
            "counter-reset" => self.counter_reset = Some(value.into()),
            "counter-increment" => self.counter_increment = Some(value.into()),
            "vertical-align" => {
                self.vertical_align = match v {
                    "baseline" => VerticalAlign::Baseline,
                    "top" | "text-top" => VerticalAlign::Top,
                    "middle" => VerticalAlign::Middle,
                    "bottom" | "text-bottom" => VerticalAlign::Bottom,
                    "sub" => VerticalAlign::Sub,
                    "super" => VerticalAlign::Super,
                    _ => return,
                }
            }
            "border-spacing" => {
                if let Some(px) = tokens(v).first().and_then(|t| px(t)) {
                    self.border_spacing = px;
                }
            }
            "border-collapse" => self.border_collapse = v == "collapse",
            _ => {}
        }
    }

    fn grid_property(&mut self, name: &str, value: &str) {
        let g = Rc::make_mut(&mut self.grid);
        let v = Some(value.trim().to_string());
        let parts: Vec<String> = value.split('/').map(|p| p.trim().to_string()).collect();
        // A bare name (`grid-row: main`) also sets the end to that name.
        let is_name = |s: &str| {
            s.chars().any(|c| c.is_alphabetic())
                && !s.starts_with("span")
                && s != "auto"
                && !s.contains(' ')
        };
        match name {
            "grid-template-columns" => g.template_columns = v,
            "grid-template-rows" => g.template_rows = v,
            "grid-template-areas" => g.template_areas = v,
            "grid-auto-columns" => g.auto_columns = v,
            "grid-auto-rows" => g.auto_rows = v,
            "grid-auto-flow" => g.auto_flow = v,
            "grid-row-start" => g.row_start = v,
            "grid-row-end" => g.row_end = v,
            "grid-column-start" => g.column_start = v,
            "grid-column-end" => g.column_end = v,
            "grid-row" | "grid-column" => {
                let start = parts.first().cloned();
                let end = parts
                    .get(1)
                    .cloned()
                    .or_else(|| start.clone().filter(|s| is_name(s)));
                if name == "grid-row" {
                    (g.row_start, g.row_end) = (start, end);
                } else {
                    (g.column_start, g.column_end) = (start, end);
                }
            }
            "grid-area" => {
                let first = parts.first().cloned();
                let named = first.clone().filter(|s| is_name(s));
                g.row_start = first;
                g.column_start = parts.get(1).cloned().or(named.clone());
                g.row_end = parts.get(2).cloned().or(named.clone());
                g.column_end = parts.get(3).cloned().or(named);
            }
            "grid-template" => {
                if value.contains('"') {
                    // "areas" row-size … / columns
                    let (rows_part, cols) = match value.rsplit_once('/') {
                        Some((r, c)) if !c.contains('"') => (r, Some(c.trim().to_string())),
                        _ => (value, None),
                    };
                    let (mut areas, mut sizes) = (Vec::new(), Vec::new());
                    let mut rest = rows_part.trim();
                    while let Some(start) = rest.find('"') {
                        let size = rest[..start].trim();
                        if !areas.is_empty() {
                            sizes.push(if size.is_empty() { "auto" } else { size }.to_string());
                        }
                        let end = rest[start + 1..]
                            .find('"')
                            .map_or(rest.len() - 1, |e| start + 1 + e);
                        areas.push(rest[start..=end].to_string());
                        rest = &rest[end + 1..];
                    }
                    let last = rest.trim();
                    sizes.push(if last.is_empty() { "auto" } else { last }.to_string());
                    g.template_areas = Some(areas.join(" "));
                    g.template_rows = Some(sizes.join(" "));
                    g.template_columns = cols;
                } else if let Some((r, c)) = value.split_once('/') {
                    g.template_rows = Some(r.trim().to_string());
                    g.template_columns = Some(c.trim().to_string());
                }
            }
            _ => {}
        }
    }

    fn border_side(&mut self, side: usize, value: &str, fc: FontContext) {
        // `border: none` and friends reset width and style.
        self.border_width[side] = 3.0;
        self.border_style[side] = LineStyle::None;
        self.border_color[side] = self.color;
        for t in tokens(value) {
            let lt = t.to_ascii_lowercase();
            if let Some(w) = border_width(&lt, fc) {
                self.border_width[side] = w;
            } else if let Some(s) = border_style(&lt) {
                self.border_style[side] = s;
            } else if let Some(c) = values::color(t, self.color) {
                self.border_color[side] = c;
            }
        }
    }

    fn flex(&mut self, v: &str, fc: FontContext) {
        let t = tokens(v);
        match t[..] {
            ["none"] => (self.flex_grow, self.flex_shrink, self.flex_basis) = (0.0, 0.0, Dim::Auto),
            ["auto"] => (self.flex_grow, self.flex_shrink, self.flex_basis) = (1.0, 1.0, Dim::Auto),
            [one] => {
                if let Some(n) = values::number(one) {
                    (self.flex_grow, self.flex_shrink, self.flex_basis) =
                        (n, 1.0, Dim::Percent(0.0));
                } else if let Some(d) = values::dim(one, fc) {
                    (self.flex_grow, self.flex_shrink, self.flex_basis) = (1.0, 1.0, d);
                }
            }
            [g, two] => {
                let Some(g) = values::number(g) else { return };
                self.flex_grow = g;
                if let Some(s) = values::number(two) {
                    self.flex_shrink = s;
                    self.flex_basis = Dim::Percent(0.0);
                } else if let Some(d) = values::dim(two, fc) {
                    self.flex_shrink = 1.0;
                    self.flex_basis = d;
                }
            }
            [g, s, b] => {
                if let (Some(g), Some(s), Some(b)) =
                    (values::number(g), values::number(s), values::dim(b, fc))
                {
                    (self.flex_grow, self.flex_shrink, self.flex_basis) = (g, s, b);
                }
            }
            _ => {}
        }
    }

    /// `font: [style] [weight] size[/line-height] family`.
    fn font_shorthand(&mut self, v: &str, parent: &Style, env: &super::ValueEnv) {
        let root_font_size = env.root_font;
        let t = tokens(v);
        let pfc = FontContext {
            em: parent.font_size,
            rem: root_font_size,
            vw: env.vw,
            vh: env.vh,
        };
        let Some(size_at) = t.iter().position(|s| {
            let size = s.split('/').next().unwrap_or("");
            font_size(size, parent.font_size, pfc).is_some()
        }) else {
            return;
        };
        self.italic = false;
        self.bold = false;
        self.line_height = LineHeight::Normal;
        for s in &t[..size_at] {
            if *s == "italic" || *s == "oblique" {
                self.italic = true;
            } else if let Some(b) = font_weight(s, parent.bold) {
                self.bold = b;
            }
        }
        let mut parts = t[size_at].split('/');
        if let Some(size) = parts
            .next()
            .and_then(|s| font_size(s, parent.font_size, pfc))
        {
            self.font_size = size;
        }
        let fc = FontContext {
            em: self.font_size,
            rem: root_font_size,
            vw: env.vw,
            vh: env.vh,
        };
        if let Some(lh) = parts.next().and_then(|s| line_height(s, fc)) {
            self.line_height = lh;
        }
        self.family = font_family(&t[size_at + 1..].join(" "));
    }

    /// Copies the property `name` (and its longhands) from `from`.
    fn copy_from(&mut self, name: &str, from: &Style) {
        let base = name
            .trim_end_matches("-top")
            .trim_end_matches("-right")
            .trim_end_matches("-bottom")
            .trim_end_matches("-left");
        match base {
            "display" => self.display = from.display,
            "position" => self.position = from.position,
            "inset" | "top" | "right" | "bottom" | "left" => self.inset = from.inset,
            "margin" => self.margin = from.margin,
            "padding" => self.padding = from.padding,
            n if n.starts_with("border") => {
                self.border_width = from.border_width;
                self.border_style = from.border_style;
                self.border_color = from.border_color;
                self.border_radius = from.border_radius;
            }
            "width" => self.width = from.width,
            "height" => self.height = from.height,
            "min-width" => self.min_width = from.min_width,
            "min-height" => self.min_height = from.min_height,
            "max-width" => self.max_width = from.max_width,
            "max-height" => self.max_height = from.max_height,
            "box-sizing" => self.box_sizing = from.box_sizing,
            "background" | "background-color" => self.background = from.background,
            "opacity" => self.opacity = from.opacity,
            "text-transform" => self.text_transform = from.text_transform,
            "overflow" | "overflow-y" => self.overflow = from.overflow,
            "color" => self.color = from.color,
            "font-size" => self.font_size = from.font_size,
            "font-weight" => self.bold = from.bold,
            "font-style" => self.italic = from.italic,
            "font-family" => self.family = from.family,
            "font" => {
                self.font_size = from.font_size;
                self.bold = from.bold;
                self.italic = from.italic;
                self.family = from.family;
                self.line_height = from.line_height;
            }
            "line-height" => self.line_height = from.line_height,
            "text-align" => self.text_align = from.text_align,
            "white-space" => self.white_space = from.white_space,
            "text-decoration" | "text-decoration-line" => {
                self.underline = from.underline;
                self.line_through = from.line_through;
            }
            "cursor" => self.cursor = from.cursor,
            "list-style" | "list-style-type" => self.list_style = from.list_style,
            "direction" => self.rtl = from.rtl,
            "visibility" => self.visible = from.visible,
            "vertical-align" => self.vertical_align = from.vertical_align,
            "border-spacing" => self.border_spacing = from.border_spacing,
            "border-collapse" => self.border_collapse = from.border_collapse,
            "flex-grow" => self.flex_grow = from.flex_grow,
            "flex-shrink" => self.flex_shrink = from.flex_shrink,
            "flex-basis" => self.flex_basis = from.flex_basis,
            "flex" => {
                self.flex_grow = from.flex_grow;
                self.flex_shrink = from.flex_shrink;
                self.flex_basis = from.flex_basis;
            }
            _ => {}
        }
    }
}

/// Maps a logical property (`margin-inline`, `padding-block-start`,
/// `inset-inline-end`, `inline-size`, …) to physical ones, for horizontal
/// left-to-right text.
fn logical(name: &str, value: &str) -> Option<Vec<(String, String)>> {
    let sizes = [
        ("inline-size", "width"),
        ("block-size", "height"),
        ("min-inline-size", "min-width"),
        ("min-block-size", "min-height"),
        ("max-inline-size", "max-width"),
        ("max-block-size", "max-height"),
    ];
    if let Some((_, p)) = sizes.iter().find(|(l, _)| *l == name) {
        return Some(vec![(p.to_string(), value.to_string())]);
    }
    // prefix, physical name for a side, and whether it is `border-*`
    let (prefix, rest) = ["margin", "padding", "inset", "border"]
        .iter()
        .find_map(|p| name.strip_prefix(p).map(|r| (*p, r)))?;
    let rest = rest.strip_prefix('-')?;
    let (axis, rest) = match rest.strip_prefix("inline") {
        Some(r) => ("inline", r),
        None => ("block", rest.strip_prefix("block")?),
    };
    let (start, end) = if axis == "inline" {
        ("left", "right")
    } else {
        ("top", "bottom")
    };
    let side = |s: &str, suffix: &str| match prefix {
        "inset" => s.to_string(),
        _ => format!("{prefix}-{s}{suffix}"),
    };
    // `-start` / `-end`, possibly followed by `-width` etc. for borders.
    for (which, phys) in [("-start", start), ("-end", end)] {
        if let Some(suffix) = rest.strip_prefix(which) {
            return Some(vec![(side(phys, suffix), value.to_string())]);
        }
    }
    // Both sides: `margin-inline: a b`, `border-inline-width: a b`,
    // `border-block: 1px solid` (same value on both).
    let suffix = rest;
    let parts = values::tokens(value);
    let two = prefix != "border" || !suffix.is_empty();
    let (a, b) = match parts.as_slice() {
        [a, b] if two => (a.to_string(), b.to_string()),
        _ => (value.to_string(), value.to_string()),
    };
    Some(vec![(side(start, suffix), a), (side(end, suffix), b)])
}

/// The translation part of a `transform` list (other functions ignored).
fn transform_translate(value: &str, dim: impl Fn(&str) -> Option<Dim>) -> (Dim, Dim) {
    let zero = Dim::Px(0.0);
    let (mut x, mut y) = (zero, zero);
    for t in values::tokens(value) {
        let lower = t.to_ascii_lowercase();
        let Some((name, args)) = lower.split_once('(') else {
            continue;
        };
        let args: Vec<&str> = args
            .trim_end_matches(')')
            .split(',')
            .map(str::trim)
            .collect();
        let d = |i: usize| args.get(i).and_then(|a| dim(a)).unwrap_or(zero);
        match name {
            "translate" | "translate3d" => (x, y) = (d(0), d(1)),
            "translatex" => x = d(0),
            "translatey" => y = d(0),
            _ => {}
        }
    }
    (x, y)
}

fn is_inherited(name: &str) -> bool {
    name.starts_with("font")
        || matches!(
            name,
            "color"
                | "line-height"
                | "text-align"
                | "white-space"
                | "cursor"
                | "list-style"
                | "list-style-type"
                | "visibility"
                | "direction"
                | "border-spacing"
                | "border-collapse"
                | "text-transform"
        )
}

fn side(name: &str) -> usize {
    if name.contains("top") {
        TOP
    } else if name.contains("right") {
        RIGHT
    } else if name.contains("bottom") {
        BOTTOM
    } else {
        LEFT
    }
}

fn set<T>(field: &mut T, v: Option<T>) {
    if let Some(v) = v {
        *field = v;
    }
}

fn set4<T: Copy>(field: &mut [T; 4], v: &str, parse: impl Fn(&str) -> Option<T>) {
    let parsed: Option<Vec<T>> = tokens(v).into_iter().map(parse).collect();
    if let Some(sides) = parsed.as_deref().and_then(four) {
        *field = sides;
    }
}

fn border_width(v: &str, fc: FontContext) -> Option<f32> {
    match v {
        "thin" => Some(1.0),
        "medium" => Some(3.0),
        "thick" => Some(5.0),
        _ => values::px(v, fc),
    }
}

fn border_style(v: &str) -> Option<LineStyle> {
    Some(match v {
        "none" | "hidden" => LineStyle::None,
        "solid" | "groove" | "ridge" | "inset" | "outset" => LineStyle::Solid,
        "dashed" => LineStyle::Dashed,
        "dotted" => LineStyle::Dotted,
        "double" => LineStyle::Double,
        _ => return None,
    })
}

/// The URL in `url(...)`.
fn url_value(t: &str) -> Option<String> {
    let inner = t.trim().strip_prefix("url(")?.strip_suffix(')')?.trim();
    Some(inner.trim_matches(['"', '\'']).to_string())
}

fn repeat(v: &str) -> Option<Repeat> {
    Some(match v {
        "repeat" => Repeat(true, true),
        "no-repeat" => Repeat(false, false),
        "repeat-x" => Repeat(true, false),
        "repeat-y" => Repeat(false, true),
        _ => return None,
    })
}

fn background_size(v: &str, fc: FontContext) -> Option<BackgroundSize> {
    Some(match v {
        "cover" => BackgroundSize::Cover,
        "contain" => BackgroundSize::Contain,
        "auto" => BackgroundSize::Auto,
        _ => {
            let t = tokens(v);
            let w = values::dim(t.first()?, fc)?;
            let h = t
                .get(1)
                .and_then(|h| values::dim(h, fc))
                .unwrap_or(Dim::Auto);
            BackgroundSize::Size(w, h)
        }
    })
}

fn background_position(t: &[&str], fc: FontContext) -> Option<(Dim, Dim)> {
    let keyword = |k: &str| -> Option<(Option<Dim>, Option<Dim>)> {
        Some(match k {
            "left" => (Some(Dim::Percent(0.0)), None),
            "right" => (Some(Dim::Percent(100.0)), None),
            "top" => (None, Some(Dim::Percent(0.0))),
            "bottom" => (None, Some(Dim::Percent(100.0))),
            "center" => (None, None),
            _ => return None,
        })
    };
    let (mut x, mut y) = (Dim::Percent(50.0), Dim::Percent(50.0));
    match t {
        [a] => match keyword(a) {
            Some((kx, ky)) => {
                x = kx.unwrap_or(x);
                y = ky.unwrap_or(y);
            }
            None => x = values::dim(a, fc)?,
        },
        [a, b, ..] => match (keyword(a), keyword(b)) {
            (Some((ax, ay)), Some((bx, by))) => {
                x = ax.or(bx).unwrap_or(x);
                y = ay.or(by).unwrap_or(y);
            }
            _ => {
                x = keyword(a)
                    .and_then(|k| k.0)
                    .or_else(|| values::dim(a, fc))?;
                y = keyword(b)
                    .and_then(|k| k.1)
                    .or_else(|| values::dim(b, fc))?;
            }
        },
        [] => return None,
    }
    Some((x, y))
}

fn flex_direction(v: &str) -> Option<FlexDirection> {
    Some(match v {
        "row" => FlexDirection::Row,
        "row-reverse" => FlexDirection::RowReverse,
        "column" => FlexDirection::Column,
        "column-reverse" => FlexDirection::ColumnReverse,
        _ => return None,
    })
}

fn align(v: &str) -> Option<Align> {
    Some(match v {
        "normal" | "auto" => Align::Normal,
        "start" | "flex-start" | "left" | "self-start" => Align::Start,
        "end" | "flex-end" | "right" | "self-end" => Align::End,
        "center" => Align::Center,
        "stretch" => Align::Stretch,
        "baseline" => Align::Baseline,
        "space-between" => Align::SpaceBetween,
        "space-around" => Align::SpaceAround,
        "space-evenly" => Align::SpaceEvenly,
        _ => return None,
    })
}

fn font_size(v: &str, parent: f32, pfc: FontContext) -> Option<f32> {
    let k = match v {
        "xx-small" => 9.0,
        "x-small" => 10.0,
        "small" => 13.0,
        "medium" => 16.0,
        "large" => 18.0,
        "x-large" => 24.0,
        "xx-large" => 32.0,
        "smaller" => parent / 1.2,
        "larger" => parent * 1.2,
        _ => {
            return match values::dim(v, pfc)? {
                Dim::Auto => None,
                d => Some(d.px_or_zero(parent)),
            };
        }
    };
    Some(k)
}

fn font_weight(v: &str, parent_bold: bool) -> Option<bool> {
    match v {
        "normal" | "lighter" => Some(false),
        "bold" => Some(true),
        "bolder" => Some(true),
        _ => v
            .parse::<f32>()
            .ok()
            .map(|n| n >= 600.0)
            .or(if v == "inherit" {
                Some(parent_bold)
            } else {
                None
            }),
    }
}

/// The first family in the list that maps to a bundled face.
fn font_family(v: &str) -> Family {
    let v = v.to_ascii_lowercase();
    let mono = ["mono", "courier", "consolas", "menlo", "monaco", "code"];
    let serif = [
        "serif",
        "georgia",
        "times",
        "libertine",
        "garamond",
        "palatino",
        "cambria",
        "baskerville",
        "charter",
        "book",
        "merriweather",
    ];
    for name in v.split(',') {
        let name = name.trim().trim_matches(|c| c == '"' || c == '\'');
        if let Some(web) = crate::text::web_family(name) {
            return web;
        }
        if mono.iter().any(|m| name.contains(m)) {
            return Family::Mono;
        }
        if name.contains("sans") {
            return Family::Sans;
        }
        if serif.iter().any(|m| name.contains(m)) {
            return Family::Serif;
        }
    }
    Family::Sans
}

fn line_height(v: &str, fc: FontContext) -> Option<LineHeight> {
    if v == "normal" {
        return Some(LineHeight::Normal);
    }
    if let Some(n) = values::number(v) {
        return Some(LineHeight::Number(n));
    }
    match values::dim(v, fc)? {
        Dim::Auto => None,
        d => Some(LineHeight::Px(d.px_or_zero(fc.em))),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn styled(decls: &[(&str, &str)]) -> Style {
        let parent = Style::default();
        let mut s = Style::inherit(&parent);
        for (n, v) in decls {
            s.apply(n, v, &parent, 16.0);
        }
        s
    }

    #[test]
    fn background_layers() {
        let s = styled(&[("background", "url(a.png) no-repeat #fff")]);
        assert_eq!(s.background_image.as_deref(), Some("a.png"));
        let s = styled(&[("background", "linear-gradient(red, blue),url(b.png)")]);
        assert_eq!(s.background_image.as_deref(), Some("b.png"));
        assert!(s.background_gradient.as_ref().is_some_and(|(_, top)| *top));
        let s = styled(&[(
            "box-shadow",
            "0 1px 2px rgba(0,0,0,.2), inset 0 0 0 1px red",
        )]);
        assert_eq!(s.box_shadow.len(), 2);
        assert!(s.box_shadow[1].inset);
    }

    #[test]
    fn logical_properties() {
        let s = styled(&[
            ("margin-inline", "1px 2px"),
            ("padding-block", "3px"),
            ("padding-inline-start", "4px"),
            ("inset-inline-end", "5px"),
            ("border-block-start", "2px solid red"),
            ("inline-size", "50px"),
        ]);
        assert_eq!(s.margin[LEFT], Dim::Px(1.0));
        assert_eq!(s.margin[RIGHT], Dim::Px(2.0));
        assert_eq!(s.padding[TOP], Dim::Px(3.0));
        assert_eq!(s.padding[BOTTOM], Dim::Px(3.0));
        assert_eq!(s.padding[LEFT], Dim::Px(4.0));
        assert_eq!(s.inset[RIGHT], Dim::Px(5.0));
        assert_eq!(s.border(TOP), 2.0);
        assert_eq!(s.width, Dim::Px(50.0));
    }

    #[test]
    fn box_shorthands() {
        let s = styled(&[
            ("margin", "1px 2px"),
            ("padding", "1em"),
            ("margin-left", "auto"),
        ]);
        assert_eq!(
            s.margin,
            [Dim::Px(1.0), Dim::Px(2.0), Dim::Px(1.0), Dim::Auto]
        );
        assert_eq!(s.padding, [Dim::Px(16.0); 4]);
    }

    #[test]
    fn borders() {
        let s = styled(&[("border", "2px solid #00f"), ("border-left", "none")]);
        assert_eq!(s.border(TOP), 2.0);
        assert_eq!(s.border(LEFT), 0.0);
        assert_eq!(s.border_color[TOP], Rgba::new(0, 0, 255, 255));
        let s = styled(&[("border-style", "solid")]);
        assert_eq!(s.border(TOP), 3.0, "initial border-width is medium");
        assert_eq!(styled(&[]).border(TOP), 0.0, "no border without a style");
    }

    #[test]
    fn fonts() {
        let s = styled(&[("font", "italic bold 20px/1.5 Menlo, monospace")]);
        assert!(s.italic && s.bold);
        assert_eq!(s.font_size, 20.0);
        assert_eq!(s.line_height_px(), 30.0);
        assert_eq!(s.family, Family::Mono);
        let s = styled(&[("font-size", "2em"), ("padding", "1em")]);
        assert_eq!(s.font_size, 32.0, "em in font-size refers to the parent");
        assert_eq!(
            s.padding[0],
            Dim::Px(32.0),
            "em elsewhere refers to own size"
        );
    }

    #[test]
    fn flex_shorthand() {
        let s = styled(&[("flex", "1")]);
        assert_eq!(
            (s.flex_grow, s.flex_shrink, s.flex_basis),
            (1.0, 1.0, Dim::Percent(0.0))
        );
        let s = styled(&[("flex", "0 0 100px")]);
        assert_eq!(s.flex_basis, Dim::Px(100.0));
        let s = styled(&[
            ("display", "flex"),
            ("flex-flow", "column wrap"),
            ("gap", "4px 8px"),
        ]);
        assert_eq!(s.flex_direction, FlexDirection::Column);
        assert!(s.flex_wrap);
        assert_eq!((s.row_gap, s.column_gap), (Dim::Px(4.0), Dim::Px(8.0)));
    }

    #[test]
    fn keywords_inherit_and_initial() {
        let mut parent = Style::default();
        parent.apply("color", "red", &Style::default(), 16.0);
        parent.apply("width", "10px", &Style::default(), 16.0);
        let mut s = Style::inherit(&parent);
        assert_eq!(s.color, Rgba::new(255, 0, 0, 255), "color is inherited");
        assert_eq!(s.width, Dim::Auto, "width is not");
        s.apply("width", "inherit", &parent, 16.0);
        assert_eq!(s.width, Dim::Px(10.0));
        s.apply("color", "initial", &parent, 16.0);
        assert_eq!(s.color, Rgba::BLACK);
    }

    #[test]
    fn invalid_values_are_ignored() {
        let s = styled(&[("width", "10px"), ("width", "wide"), ("display", "bogus")]);
        assert_eq!(s.width, Dim::Px(10.0));
        assert_eq!(s.display, Display::Inline);
    }
}
