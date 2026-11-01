//! Colors and sizes of the browser chrome, in logical pixels.

use crate::gfx::Color;

pub const TAB_BAR_HEIGHT: i32 = 34;
pub const TOOLBAR_HEIGHT: i32 = 40;
pub const CHROME_HEIGHT: i32 = TAB_BAR_HEIGHT + TOOLBAR_HEIGHT;
pub const TAB_MAX_WIDTH: i32 = 220;
pub const TAB_MIN_WIDTH: i32 = 60;
pub const BUTTON_SIZE: i32 = 28;
pub const PAD: i32 = 6;
pub const TEXT_SIZE: i32 = 16;

pub const TAB_BAR_BG: Color = 0x1e1f22;
pub const TAB_BG: Color = 0x2b2d31;
pub const TAB_HOVER_BG: Color = 0x35373c;
pub const TAB_ACTIVE_BG: Color = 0x3c3f45;
pub const TOOLBAR_BG: Color = 0x3c3f45;
pub const ADDRESS_BG: Color = 0x1e1f22;
pub const ADDRESS_FOCUS: Color = 0x5b8def;
pub const SELECTION: Color = 0x34548a;
pub const BUTTON_HOVER: Color = 0x4e5158;
pub const BUTTON_PRESSED: Color = 0x5d6068;
pub const TEXT: Color = 0xe8e8e8;
pub const TEXT_DIM: Color = 0x8a8d93;
pub const PAGE_BG: Color = 0xffffff;
pub const PAGE_TEXT: Color = 0x202124;
pub const SCROLLBAR: Color = 0xb0b3b8;
pub const ERROR: Color = 0xc5221f;
