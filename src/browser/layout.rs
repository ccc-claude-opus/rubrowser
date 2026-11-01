//! Where every piece of the chrome goes, in logical pixels.
//!
//! The same layout is used for drawing and for hit-testing mouse input.

use super::theme::*;
use crate::gfx::Rect;

pub struct Layout {
    pub tab_bar: Rect,
    pub tabs: Vec<Rect>,
    pub new_tab: Rect,
    pub toolbar: Rect,
    pub back: Rect,
    pub forward: Rect,
    pub reload: Rect,
    pub address: Rect,
    pub content: Rect,
}

/// What lies under a point.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Hit {
    Tab(usize),
    TabClose(usize),
    NewTab,
    Back,
    Forward,
    Reload,
    Address,
    Content,
    Nothing,
}

impl Layout {
    pub fn new(width: i32, height: i32, tab_count: usize) -> Layout {
        let tab_bar = Rect::new(0, 0, width, TAB_BAR_HEIGHT);
        let n = tab_count.max(1) as i32;
        let room = width - 2 * PAD - BUTTON_SIZE - PAD;
        let tab_w = (room / n).clamp(TAB_MIN_WIDTH, TAB_MAX_WIDTH);
        let tab_y = TAB_BAR_HEIGHT - BUTTON_SIZE;
        let tabs = (0..tab_count as i32)
            .map(|i| Rect::new(PAD + i * tab_w, tab_y, tab_w - 2, BUTTON_SIZE))
            .collect::<Vec<_>>();
        let new_tab_x = PAD + n.min(tab_count as i32) * tab_w;
        let new_tab = Rect::new(new_tab_x, tab_y, BUTTON_SIZE, BUTTON_SIZE);

        let toolbar = Rect::new(0, TAB_BAR_HEIGHT, width, TOOLBAR_HEIGHT);
        let by = toolbar.y + (TOOLBAR_HEIGHT - BUTTON_SIZE) / 2;
        let button = |i: i32| Rect::new(PAD + i * (BUTTON_SIZE + 2), by, BUTTON_SIZE, BUTTON_SIZE);
        let (back, forward, reload) = (button(0), button(1), button(2));
        let ax = reload.right() + PAD;
        let address = Rect::new(ax, by, (width - PAD - ax).max(0), BUTTON_SIZE);

        let content = Rect::new(0, CHROME_HEIGHT, width, (height - CHROME_HEIGHT).max(0));
        Layout {
            tab_bar,
            tabs,
            new_tab,
            toolbar,
            back,
            forward,
            reload,
            address,
            content,
        }
    }

    /// The close button inside tab `i`.
    pub fn tab_close(&self, i: usize) -> Rect {
        let t = self.tabs[i];
        let s = 16;
        Rect::new(t.right() - s - PAD, t.y + (t.h - s) / 2, s, s)
    }

    pub fn hit(&self, x: i32, y: i32) -> Hit {
        for i in 0..self.tabs.len() {
            if self.tab_close(i).contains(x, y) {
                return Hit::TabClose(i);
            }
            if self.tabs[i].contains(x, y) {
                return Hit::Tab(i);
            }
        }
        let targets = [
            (self.new_tab, Hit::NewTab),
            (self.back, Hit::Back),
            (self.forward, Hit::Forward),
            (self.reload, Hit::Reload),
            (self.address, Hit::Address),
            (self.content, Hit::Content),
        ];
        targets
            .iter()
            .find(|(r, _)| r.contains(x, y))
            .map_or(Hit::Nothing, |&(_, h)| h)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hit_testing() {
        let l = Layout::new(800, 600, 2);
        let c = l.tab_close(1);
        assert_eq!(l.hit(c.x + 1, c.y + 1), Hit::TabClose(1));
        assert_eq!(l.hit(l.tabs[0].x + 2, l.tabs[0].y + 2), Hit::Tab(0));
        assert_eq!(l.hit(l.back.x + 1, l.back.y + 1), Hit::Back);
        assert_eq!(l.hit(l.address.x + 50, l.address.y + 5), Hit::Address);
        assert_eq!(l.hit(10, 500), Hit::Content);
        assert!(l.new_tab.x >= l.tabs[1].right());
    }

    #[test]
    fn tiny_window_does_not_panic() {
        let l = Layout::new(10, 10, 3);
        assert_eq!(l.content.h, 0);
        assert_eq!(l.address.w, 0);
    }
}
