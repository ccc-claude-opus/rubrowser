//! A single-line text field: caret, selection, scrolling, clipboard.

use crate::abi::key;
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

    /// Caret x positions (relative to the text start) for every index
    /// `0..=len`, given the advance of each char.
    pub fn positions(advances: &[f32]) -> Vec<f32> {
        let mut xs = Vec::with_capacity(advances.len() + 1);
        let mut x = 0.0;
        xs.push(x);
        for a in advances {
            x += a;
            xs.push(x);
        }
        xs
    }

    /// Char index nearest to `x` (relative to the text start, unscrolled).
    pub fn index_at(&self, x: f32, xs: &[f32]) -> usize {
        let x = x + self.scroll;
        let mut best = 0;
        for (i, &px) in xs.iter().enumerate().take(self.chars.len() + 1) {
            if (px - x).abs() < (xs[best] - x).abs() {
                best = i;
            }
        }
        best
    }

    /// Mouse press at `x` relative to the field's text start.
    pub fn click(&mut self, x: f32, xs: &[f32], extend: bool) {
        let i = self.index_at(x, xs);
        self.move_to(i, extend);
    }

    /// Mouse drag to `x` relative to the field's text start.
    pub fn drag(&mut self, x: f32, xs: &[f32]) {
        let i = self.index_at(x, xs);
        self.move_to(i, true);
    }

    /// Horizontal scroll of the text, in logical pixels.
    pub fn scroll(&self) -> f32 {
        self.scroll
    }

    /// Keeps the caret visible in a text area `width` wide.
    pub fn scroll_to_caret(&mut self, width: f32, xs: &[f32]) {
        let caret_x = xs.get(self.caret).copied().unwrap_or(0.0);
        let end = xs.last().copied().unwrap_or(0.0);
        if caret_x - self.scroll > width - 2.0 {
            self.scroll = caret_x - width + 2.0;
        }
        if caret_x < self.scroll {
            self.scroll = caret_x;
        }
        self.scroll = self.scroll.clamp(0.0, (end - width + 2.0).max(0.0));
    }
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
        let xs = TextField::positions(&[8.0, 2.0, 8.0, 8.0]);
        assert_eq!(xs, vec![0.0, 8.0, 10.0, 18.0, 26.0]);
        f.click(9.5, &xs, false);
        assert_eq!(f.caret(), 2);
        f.drag(100.0, &xs);
        assert_eq!(f.selection(), (2, 4));
        assert_eq!(
            f.key(key::ENTER, NONE, &mut TestHost::default()),
            Action::Submit
        );
    }

    #[test]
    fn scrolls_to_keep_caret_visible() {
        let mut f = TextField::default();
        f.set_text("abcdefghij");
        let xs = TextField::positions(&[10.0; 10]);
        f.scroll_to_caret(50.0, &xs);
        assert_eq!(f.scroll(), 52.0);
        f.key(key::HOME, NONE, &mut TestHost::default());
        f.scroll_to_caret(50.0, &xs);
        assert_eq!(f.scroll(), 0.0);
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
