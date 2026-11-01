//! A single-line text field: caret, selection, scrolling, clipboard.

use crate::abi::key;
use crate::gfx::{Color, Painter, Rect, char_width};
use crate::input::{Host, Modifiers};

/// What a key press did to the field.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Action {
    /// The key was not for the text field.
    Ignored,
    /// The key was handled (text, caret or selection may have changed).
    Handled,
    /// Enter was pressed.
    Submit,
    /// Escape was pressed.
    Cancel,
}

/// Text is stored as chars, so the caret and selection are char indices.
#[derive(Default)]
pub struct TextField {
    chars: Vec<char>,
    caret: usize,
    anchor: usize,
    /// Horizontal scroll in logical pixels.
    scroll: f32,
}

impl TextField {
    pub fn text(&self) -> String {
        self.chars.iter().collect()
    }

    pub fn set_text(&mut self, text: &str) {
        self.chars = text.chars().collect();
        self.caret = self.chars.len();
        self.anchor = self.caret;
        self.scroll = 0.0;
    }

    pub fn select_all(&mut self) {
        self.anchor = 0;
        self.caret = self.chars.len();
    }

    pub fn caret(&self) -> usize {
        self.caret
    }

    /// The selected char range, `start..end`.
    pub fn selection(&self) -> (usize, usize) {
        (self.caret.min(self.anchor), self.caret.max(self.anchor))
    }

    pub fn selected_text(&self) -> String {
        let (s, e) = self.selection();
        self.chars[s..e].iter().collect()
    }

    fn delete_selection(&mut self) -> bool {
        let (s, e) = self.selection();
        if s == e {
            return false;
        }
        self.chars.drain(s..e);
        self.caret = s;
        self.anchor = s;
        true
    }

    /// Replaces the selection with `text` (control characters dropped).
    pub fn insert(&mut self, text: &str) {
        self.delete_selection();
        let new: Vec<char> = text.chars().filter(|c| !c.is_control()).collect();
        let n = new.len();
        self.chars.splice(self.caret..self.caret, new);
        self.caret += n;
        self.anchor = self.caret;
    }

    fn move_to(&mut self, pos: usize, extend: bool) {
        self.caret = pos.min(self.chars.len());
        if !extend {
            self.anchor = self.caret;
        }
    }

    /// Start of the word before `pos`.
    fn word_left(&self, mut pos: usize) -> usize {
        while pos > 0 && !self.chars[pos - 1].is_alphanumeric() {
            pos -= 1;
        }
        while pos > 0 && self.chars[pos - 1].is_alphanumeric() {
            pos -= 1;
        }
        pos
    }

    /// End of the word after `pos`.
    fn word_right(&self, mut pos: usize) -> usize {
        let n = self.chars.len();
        while pos < n && !self.chars[pos].is_alphanumeric() {
            pos += 1;
        }
        while pos < n && self.chars[pos].is_alphanumeric() {
            pos += 1;
        }
        pos
    }

    pub fn key(&mut self, code: i32, mods: Modifiers, host: &mut dyn Host) -> Action {
        let shift = mods.shift;
        let word = mods.alt || (mods.ctrl && !mods.meta);
        let line = mods.meta;
        match code {
            key::ENTER => return Action::Submit,
            key::ESCAPE => return Action::Cancel,
            key::LEFT => {
                let (s, e) = self.selection();
                let to = if line {
                    0
                } else if word {
                    self.word_left(self.caret)
                } else if s != e && !shift {
                    s
                } else {
                    self.caret.saturating_sub(1)
                };
                self.move_to(to, shift);
            }
            key::RIGHT => {
                let (s, e) = self.selection();
                let to = if line {
                    self.chars.len()
                } else if word {
                    self.word_right(self.caret)
                } else if s != e && !shift {
                    e
                } else {
                    self.caret + 1
                };
                self.move_to(to, shift);
            }
            key::HOME | key::UP => self.move_to(0, shift),
            key::END | key::DOWN => self.move_to(self.chars.len(), shift),
            key::BACKSPACE => {
                if !self.delete_selection() && self.caret > 0 {
                    let from = if word {
                        self.word_left(self.caret)
                    } else if line {
                        0
                    } else {
                        self.caret - 1
                    };
                    self.chars.drain(from..self.caret);
                    self.move_to(from, false);
                }
            }
            key::DELETE => {
                if !self.delete_selection() && self.caret < self.chars.len() {
                    let to = if word {
                        self.word_right(self.caret)
                    } else {
                        self.caret + 1
                    };
                    self.chars.drain(self.caret..to);
                    self.anchor = self.caret;
                }
            }
            c if mods.command() && c == 'A' as i32 => self.select_all(),
            c if mods.command() && (c == 'C' as i32 || c == 'X' as i32) => {
                let (s, e) = self.selection();
                if s != e {
                    host.clipboard_set(&self.selected_text());
                    if c == 'X' as i32 {
                        self.delete_selection();
                    }
                }
            }
            c if mods.command() && c == 'V' as i32 => {
                // A host without clipboard access delivers pastes as text events.
                if let Some(text) = host.clipboard_get() {
                    self.insert(text.lines().next().unwrap_or(""));
                }
            }
            _ => return Action::Ignored,
        }
        Action::Handled
    }

    /// Char index nearest to `x` (logical, relative to the text start).
    pub fn index_at(&self, x: i32, cw: f32) -> usize {
        (((x as f32 + self.scroll) / cw).round().max(0.0) as usize).min(self.chars.len())
    }

    /// Mouse press at `x` relative to the field's text start.
    pub fn click(&mut self, x: i32, cw: f32, extend: bool) {
        let i = self.index_at(x, cw);
        self.move_to(i, extend);
    }

    /// Mouse drag to `x` relative to the field's text start.
    pub fn drag(&mut self, x: i32, cw: f32) {
        let i = self.index_at(x, cw);
        self.move_to(i, true);
    }

    /// Keeps the caret visible in a text area `width` logical pixels wide.
    fn scroll_to_caret(&mut self, width: f32, cw: f32) {
        let caret_x = self.caret as f32 * cw;
        if caret_x - self.scroll > width - cw {
            self.scroll = caret_x - width + cw;
        }
        if caret_x < self.scroll {
            self.scroll = caret_x;
        }
        let max = (self.chars.len() as f32 * cw - width + cw).max(0.0);
        self.scroll = self.scroll.clamp(0.0, max);
    }

    /// Draws the text in `area` (the text area, without padding).
    pub fn draw(&mut self, p: &mut Painter, area: Rect, style: &Style, caret_visible: bool) {
        let cw = char_width(style.size, p.scale());
        self.scroll_to_caret(area.w as f32, cw);
        p.set_clip(area);
        let x0 = area.x as f32 - self.scroll;
        let ty = area.y + (area.h - style.size) / 2;
        let (s, e) = self.selection();
        if style.focused && s != e {
            let sx = (x0 + s as f32 * cw).round() as i32;
            let ex = (x0 + e as f32 * cw).round() as i32;
            p.fill_rect(Rect::new(sx, ty, ex - sx, style.size), style.selection);
        }
        let text: String = self.chars.iter().collect();
        p.text(x0.round() as i32, ty, style.size, &text, style.color);
        if style.focused && caret_visible {
            let cx = (x0 + self.caret as f32 * cw).round() as i32;
            p.fill_rect(Rect::new(cx, ty, 2, style.size), style.caret);
        }
        p.reset_clip();
    }
}

/// How a [`TextField`] is drawn.
pub struct Style {
    pub size: i32,
    pub color: Color,
    pub caret: Color,
    pub selection: Color,
    pub focused: bool,
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::input::Cursor;

    #[derive(Default)]
    struct TestHost {
        clipboard: Option<String>,
    }

    impl Host for TestHost {
        fn fetch(&mut self, _: u32, _: &str) {}
        fn clipboard_get(&mut self) -> Option<String> {
            self.clipboard.clone()
        }
        fn clipboard_set(&mut self, text: &str) {
            self.clipboard = Some(text.into());
        }
        fn set_cursor(&mut self, _: Cursor) {}
        fn set_title(&mut self, _: &str) {}
    }

    const NONE: Modifiers = Modifiers {
        shift: false,
        ctrl: false,
        alt: false,
        meta: false,
    };
    const SHIFT: Modifiers = Modifiers {
        shift: true,
        ..NONE
    };
    const CMD: Modifiers = Modifiers { meta: true, ..NONE };
    const ALT: Modifiers = Modifiers { alt: true, ..NONE };

    #[test]
    fn typing_and_deleting() {
        let mut h = TestHost::default();
        let mut f = TextField::default();
        f.insert("hello");
        f.key(key::BACKSPACE, NONE, &mut h);
        assert_eq!(f.text(), "hell");
        f.key(key::HOME, NONE, &mut h);
        f.key(key::DELETE, NONE, &mut h);
        assert_eq!(f.text(), "ell");
        f.insert("y");
        assert_eq!(f.text(), "yell");
        assert_eq!(f.caret(), 1);
    }

    #[test]
    fn selection_replace_and_clipboard() {
        let mut h = TestHost::default();
        let mut f = TextField::default();
        f.set_text("abc def");
        f.key(key::LEFT, SHIFT, &mut h);
        f.key(key::LEFT, SHIFT, &mut h);
        assert_eq!(f.selected_text(), "ef");
        f.key('X' as i32, CMD, &mut h);
        assert_eq!(f.text(), "abc d");
        assert_eq!(h.clipboard.as_deref(), Some("ef"));
        f.key('A' as i32, CMD, &mut h);
        f.key('V' as i32, CMD, &mut h);
        assert_eq!(f.text(), "ef");
    }

    #[test]
    fn word_movement() {
        let mut h = TestHost::default();
        let mut f = TextField::default();
        f.set_text("https://example.com/a.wasm");
        f.key(key::LEFT, ALT, &mut h);
        assert_eq!(f.caret(), 22);
        f.key(key::BACKSPACE, ALT, &mut h);
        assert_eq!(f.text(), "https://example.com/wasm");
    }

    #[test]
    fn collapse_selection_with_arrows() {
        let mut h = TestHost::default();
        let mut f = TextField::default();
        f.set_text("abcd");
        f.select_all();
        f.key(key::LEFT, NONE, &mut h);
        assert_eq!((f.caret(), f.selection()), (0, (0, 0)));
    }

    #[test]
    fn click_maps_to_index() {
        let mut f = TextField::default();
        f.set_text("abcd");
        f.click(17, 8.0, false);
        assert_eq!(f.caret(), 2);
        f.drag(100, 8.0);
        assert_eq!(f.selection(), (2, 4));
        assert_eq!(
            f.key(key::ENTER, NONE, &mut TestHost::default()),
            Action::Submit
        );
    }

    #[test]
    fn unicode_is_char_based() {
        let mut h = TestHost::default();
        let mut f = TextField::default();
        f.insert("中文");
        f.key(key::BACKSPACE, NONE, &mut h);
        assert_eq!(f.text(), "中");
    }
}
