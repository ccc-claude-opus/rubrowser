//! Numbers shared by the browser and pages (ABI v0).
//!
//! A page receives events through `rb_event(kind, a, b, c)`; see
//! `doc/abi.md` for the full interface.

/// Event kinds passed as the first argument of `rb_event`.
pub mod event {
    /// `a = x, b = y` (logical pixels, document coordinates).
    pub const MOUSE_MOVE: i32 = 1;
    /// `a = x, b = y, c = button`.
    pub const MOUSE_DOWN: i32 = 2;
    /// `a = x, b = y, c = button`.
    pub const MOUSE_UP: i32 = 3;
    /// `a = dx, b = dy` (logical pixels; positive `dy` scrolls down).
    pub const WHEEL: i32 = 4;
    /// `a = key code, b = modifiers`.
    pub const KEY_DOWN: i32 = 5;
    /// `a = key code, b = modifiers`.
    pub const KEY_UP: i32 = 6;
    /// `a = Unicode scalar value` of typed text.
    pub const CHAR: i32 = 7;
    /// The page gained keyboard focus.
    pub const FOCUS: i32 = 8;
    /// The page lost keyboard focus.
    pub const BLUR: i32 = 9;
}

/// Mouse buttons.
pub mod button {
    pub const LEFT: i32 = 0;
    pub const MIDDLE: i32 = 1;
    pub const RIGHT: i32 = 2;
}

/// Modifier bit flags.
pub mod modifier {
    pub const SHIFT: i32 = 1;
    pub const CTRL: i32 = 2;
    pub const ALT: i32 = 4;
    pub const META: i32 = 8;
}

/// Key codes. Letters are `'A'..='Z'` and digits `'0'..='9'` as ASCII.
pub mod key {
    pub const BACKSPACE: i32 = 8;
    pub const TAB: i32 = 9;
    pub const ENTER: i32 = 13;
    pub const ESCAPE: i32 = 27;
    pub const SPACE: i32 = 32;
    pub const PAGE_UP: i32 = 33;
    pub const PAGE_DOWN: i32 = 34;
    pub const END: i32 = 35;
    pub const HOME: i32 = 36;
    pub const LEFT: i32 = 37;
    pub const UP: i32 = 38;
    pub const RIGHT: i32 = 39;
    pub const DOWN: i32 = 40;
    pub const DELETE: i32 = 46;
    pub const F5: i32 = 116;
    pub const LEFT_BRACKET: i32 = 219;
    pub const RIGHT_BRACKET: i32 = 221;
    /// Other printable characters: `CHAR_BASE + Unicode scalar value`.
    pub const CHAR_BASE: i32 = 1000;
}

/// DOM-style `KeyboardEvent.key` name for a key code.
pub fn key_name(code: i32, shift: bool) -> String {
    let named = match code {
        0 => "",
        key::BACKSPACE => "Backspace",
        key::TAB => "Tab",
        key::ENTER => "Enter",
        key::ESCAPE => "Escape",
        key::SPACE => " ",
        key::PAGE_UP => "PageUp",
        key::PAGE_DOWN => "PageDown",
        key::END => "End",
        key::HOME => "Home",
        key::LEFT => "ArrowLeft",
        key::UP => "ArrowUp",
        key::RIGHT => "ArrowRight",
        key::DOWN => "ArrowDown",
        key::DELETE => "Delete",
        key::F5 => "F5",
        key::LEFT_BRACKET => "[",
        key::RIGHT_BRACKET => "]",
        c if c >= key::CHAR_BASE => {
            return char::from_u32((c - key::CHAR_BASE) as u32)
                .map_or_else(|| "Unidentified".to_string(), |ch| ch.to_string());
        }
        c => {
            return match char::from_u32(c as u32) {
                Some(ch) if ch.is_ascii_alphabetic() && !shift => {
                    ch.to_ascii_lowercase().to_string()
                }
                Some(ch) if ch.is_ascii_alphanumeric() => ch.to_string(),
                _ => "Unidentified".to_string(),
            };
        }
    };
    named.to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn key_names() {
        assert_eq!(key_name('A' as i32, false), "a");
        assert_eq!(key_name('A' as i32, true), "A");
        assert_eq!(key_name(key::ENTER, false), "Enter");
        assert_eq!(key_name(key::CHAR_BASE + '+' as i32, true), "+");
    }
}
