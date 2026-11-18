//! The on-screen keyboard: part of the browser's chrome (`#osk`), below the
//! page, which gets shorter while it is up. It comes up for a text field the
//! user touched (or always / never, see [`KeyboardMode`]) and sends the same
//! key events a physical keyboard would (`keydown`, `input`, `keyup`). The
//! layout follows the field: `type`, `inputmode` and `enterkeyhint` (HTML).
use super::{Browser, Target};
use crate::abi::key;
use crate::dom::{Element, NodeId};
use crate::input::{Event, Host, Modifiers};

/// When the on-screen keyboard comes up for a focused text field.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum KeyboardMode {
    /// When the user touched the field (and has not typed on a physical
    /// keyboard since).
    #[default]
    Auto,
    Always,
    Never,
}

impl KeyboardMode {
    pub fn parse(s: &str) -> Option<KeyboardMode> {
        match s {
            "auto" => Some(KeyboardMode::Auto),
            "always" => Some(KeyboardMode::Always),
            "never" => Some(KeyboardMode::Never),
            _ => None,
        }
    }
}

/// Which keys: letters, symbols (two pages), digits, phone.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(super) enum Page {
    #[default]
    Letters,
    Symbols,
    More,
}

/// What the field takes, from its `type` / `inputmode`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Kind {
    Text,
    Url,
    Email,
    Numeric,
    Decimal,
    Tel,
}

#[derive(Debug, Default)]
pub(super) struct Osk {
    pub mode: KeyboardMode,
    page: Page,
    /// 0 off, 1 the next letter, 2 locked (Caps Lock).
    shift: u8,
    last_shift: f64,
    /// The field the user hid the keyboard for (it stays down until focus moves).
    dismissed: Option<(Target, NodeId)>,
    /// The layout shown, to rebuild only when it changes.
    shown: String,
}

const LETTERS: [&str; 3] = ["qwertyuiop", "asdfghjkl", "zxcvbnm"];
const SYMBOLS: [&str; 3] = ["1234567890", "-/:;()$&@\"", ".,?!'"];
const MORE: [&str; 3] = ["[]{}#%^*+=", "_\\|~<>€£¥•", ".,?!'"];

impl Browser {
    /// Sets when the on-screen keyboard comes up.
    pub fn set_keyboard_mode(&mut self, mode: KeyboardMode) {
        self.osk.mode = mode;
        self.dirty = true;
    }

    /// The focused text field the keyboard would type into.
    fn osk_field(&self) -> Option<(Target, NodeId)> {
        self.focused().filter(|&(t, n)| self.doc(t).is_text_input(n))
    }

    fn osk_kind(&self, t: Target, n: NodeId) -> (Kind, String) {
        let dom = &self.doc(t).dom;
        if t == Target::Chrome {
            return (Kind::Url, "go".into());
        }
        let attr = |a: &str| dom.attr(n, a).unwrap_or("").to_ascii_lowercase();
        let kind = match (attr("inputmode").as_str(), attr("type").as_str()) {
            ("numeric", _) => Kind::Numeric,
            ("decimal", _) => Kind::Decimal,
            ("tel", _) | ("", "tel") => Kind::Tel,
            ("email", _) | ("", "email") => Kind::Email,
            ("url", _) | ("", "url") => Kind::Url,
            ("", "number") => Kind::Decimal,
            ("search", _) | ("text", _) | ("", _) => Kind::Text,
            _ => Kind::Text,
        };
        let enter = match attr("enterkeyhint").as_str() {
            "" if attr("type") == "search" || attr("inputmode") == "search" => "search".to_string(),
            "" if dom.tag(n) == Some("textarea") => "enter".to_string(),
            "" => "go".to_string(),
            h => h.to_string(),
        };
        (kind, enter)
    }

    /// Shows, hides or relays out the keyboard (part of `sync_chrome`).
    pub(super) fn sync_osk(&mut self) {
        let Some(osk) = self.chrome_el("osk") else { return };
        let field = self.osk_field();
        if self.osk.dismissed.is_some() && self.osk.dismissed != field {
            self.osk.dismissed = None;
        }
        let up = field.is_some()
            && self.osk.dismissed.is_none()
            && match self.osk.mode {
                KeyboardMode::Never => false,
                KeyboardMode::Always => true,
                KeyboardMode::Auto => self.touch_used,
            };
        let layout = match field.filter(|_| up) {
            Some((t, n)) => {
                let (kind, enter) = self.osk_kind(t, n);
                let page = if matches!(kind, Kind::Numeric | Kind::Decimal | Kind::Tel) { Page::Letters } else { self.osk.page };
                self.osk_html(kind, page, &enter)
            }
            None => String::new(),
        };
        if layout == self.osk.shown {
            return;
        }
        let was_up = !self.osk.shown.is_empty();
        self.osk.shown = layout.clone();
        self.build_osk(osk, &layout);
        if !was_up && up {
            // the field must stay in sight above the keyboard
            self.reveal_focus = true;
        }
    }

    /// The keyboard's rows: `key` items as `(data-key, label, class)`.
    fn osk_html(&self, kind: Kind, page: Page, enter: &str) -> String {
        let shift = self.osk.shift;
        let enter_label = match enter {
            "go" => "前往",
            "search" => "搜尋",
            "send" => "傳送",
            "next" => "下一個",
            "previous" => "上一個",
            "done" => "完成",
            _ => "↵",
        };
        let mut rows: Vec<Vec<(String, String, &str)>> = Vec::new();
        let chars = |s: &str, upper: bool| -> Vec<(String, String, &'static str)> {
            s.chars()
                .map(|c| {
                    let c = if upper { c.to_ascii_uppercase() } else { c };
                    (c.to_string(), c.to_string(), "")
                })
                .collect()
        };
        let back = ("backspace".to_string(), "⌫".to_string(), "fn");
        let enter_key = ("enter".to_string(), enter_label.to_string(), "fn enter");
        let hide = ("hide".to_string(), "⌨︎▾".to_string(), "fn");
        match kind {
            Kind::Numeric | Kind::Decimal | Kind::Tel => {
                for r in ["123", "456", "789"] {
                    rows.push(chars(r, false));
                }
                let last = match kind {
                    Kind::Tel => "*0#",
                    Kind::Decimal => ".0-",
                    _ => "-0,",
                };
                let mut row = chars(last, false);
                row.push(back);
                rows.push(row);
                let mut bottom = Vec::new();
                if kind == Kind::Tel {
                    bottom.push(("+".to_string(), "+".to_string(), ""));
                }
                bottom.extend([enter_key, hide]);
                rows.push(bottom);
            }
            _ => {
                let set = match page {
                    Page::Letters => LETTERS,
                    Page::Symbols => SYMBOLS,
                    Page::More => MORE,
                };
                let upper = page == Page::Letters && shift > 0;
                rows.push(chars(set[0], upper));
                rows.push(chars(set[1], upper));
                let mut third = Vec::new();
                match page {
                    Page::Letters => {
                        let class = match shift {
                            2 => "fn on lock",
                            1 => "fn on",
                            _ => "fn",
                        };
                        third.push(("shift".to_string(), "⇧".to_string(), class));
                    }
                    Page::Symbols => third.push(("page:more".to_string(), "#+=".to_string(), "fn")),
                    Page::More => third.push(("page:symbols".to_string(), "123".to_string(), "fn")),
                }
                third.extend(chars(set[2], upper));
                third.push(back);
                rows.push(third);
                let switch = match page {
                    Page::Letters => ("page:symbols".to_string(), "123".to_string(), "fn"),
                    _ => ("page:letters".to_string(), "ABC".to_string(), "fn"),
                };
                let mut bottom = vec![switch];
                match kind {
                    Kind::Url => bottom.extend([("/".into(), "/".into(), ""), (".com".into(), ".com".into(), "fn")]),
                    Kind::Email => bottom.extend([("@".into(), "@".into(), ""), (".".into(), ".".into(), "")]),
                    _ => bottom.push((",".into(), ",".into(), "")),
                }
                bottom.push((" ".into(), "空白".into(), "space"));
                if kind == Kind::Text {
                    bottom.push((".".into(), ".".into(), ""));
                }
                bottom.extend([enter_key, hide]);
                rows.push(bottom);
            }
        }
        let esc = |v: &str| v.replace('&', "&amp;").replace('<', "&lt;").replace('"', "&quot;");
        rows.iter()
            .map(|r| {
                r.iter()
                    .map(|(k, label, class)| format!("{}\u{1f}{}\u{1f}{}", esc(k), esc(label), class))
                    .collect::<Vec<_>>()
                    .join("\u{1e}")
            })
            .collect::<Vec<_>>()
            .join("\u{1d}")
    }

    /// Replaces `#osk`'s content with `layout` (rows of keys), or hides it.
    fn build_osk(&mut self, osk: NodeId, layout: &str) {
        let dom = self.chrome.dom_mut();
        for c in dom.children(osk).to_vec() {
            dom.detach(c);
        }
        if let Some(e) = dom.element_mut(osk) {
            if layout.is_empty() {
                e.set_attr("hidden", "");
            } else {
                e.remove_attr("hidden");
            }
        }
        for row in layout.split('\u{1d}').filter(|r| !r.is_empty()) {
            let mut r = Element::new("div");
            r.set_attr("class", "krow");
            let r = dom.create_element(r);
            for item in row.split('\u{1e}') {
                let mut parts = item.split('\u{1f}');
                let (k, label, class) = (parts.next().unwrap_or(""), parts.next().unwrap_or(""), parts.next().unwrap_or(""));
                let unesc = |v: &str| v.replace("&quot;", "\"").replace("&lt;", "<").replace("&amp;", "&");
                let mut e = Element::new("div");
                e.set_attr("class", &format!("key {class}"));
                e.set_attr("data-key", &unesc(k));
                let e = dom.create_element(e);
                dom.append_text(e, &unesc(label));
                dom.append(r, e);
            }
            dom.append(osk, r);
        }
        self.chrome.dom_changed();
        self.dirty = true;
    }

    /// The key under chrome node `node`, if it is one of the keyboard's.
    pub(super) fn osk_key_at(&self, node: Option<NodeId>) -> Option<String> {
        let osk = self.chrome_el("osk")?;
        let dom = &self.chrome.dom;
        let mut n = node;
        while let Some(e) = n {
            if let Some(k) = dom.attr(e, "data-key") {
                return dom.is_inclusive_ancestor(osk, e).then(|| k.to_string());
            }
            n = dom.parent(e);
        }
        None
    }

    /// A key of the on-screen keyboard was pressed.
    pub(super) fn osk_press(&mut self, k: &str, host: &mut dyn Host) {
        let none = Modifiers::default();
        match k {
            "shift" => {
                self.osk.shift = match self.osk.shift {
                    0 => 1,
                    1 if self.now - self.osk.last_shift < 400.0 => 2,
                    _ => 0,
                };
                self.osk.last_shift = self.now;
            }
            "page:letters" => self.osk.page = Page::Letters,
            "page:symbols" => self.osk.page = Page::Symbols,
            "page:more" => self.osk.page = Page::More,
            "hide" => self.osk.dismissed = self.osk_field(),
            "backspace" => self.osk_key(key::BACKSPACE, none, None, host),
            "enter" => self.osk_key(key::ENTER, none, None, host),
            text => {
                // Shift is for single letters (not ".com")
                let upper = self.osk.shift > 0 && text.chars().count() == 1;
                for c in text.chars() {
                    let c = if upper { c.to_ascii_uppercase() } else { c };
                    let code = match c {
                        ' ' => key::SPACE,
                        c if c.is_ascii_alphanumeric() => c.to_ascii_uppercase() as i32,
                        c => key::CHAR_BASE + c as i32,
                    };
                    let mods = Modifiers { shift: c.is_ascii_uppercase(), ..none };
                    self.osk_key(code, mods, Some(c.to_string()), host);
                }
                if self.osk.shift == 1 {
                    self.osk.shift = 0;
                }
            }
        }
        self.dirty = true;
    }

    /// One key, as a physical keyboard sends it: down, the text, up.
    fn osk_key(&mut self, code: i32, mods: Modifiers, text: Option<String>, host: &mut dyn Host) {
        self.osk_typing = true;
        let now = self.now;
        self.event(Event::KeyDown { key: code, mods, repeat: false }, now, host);
        if let Some(t) = text {
            self.event(Event::Text(t), now, host);
        }
        self.event(Event::KeyUp { key: code, mods }, now, host);
        self.osk_typing = false;
    }
}
