//! Touch input (Touch Events, Pointer Events with `pointerType: "touch"`):
//! a tap is a click (with the compatibility mouse events), a drag scrolls
//! what is under the finger and keeps sliding after it lifts, a long press
//! opens the context menu. Pages hear every finger; canceling `touchstart`
//! or `touchmove`, or `touch-action: none`, keeps the drag to the page.
use super::{Browser, EventInit, Target};
use crate::dom::NodeId;
use crate::input::{Host, TouchPhase};

/// Moving further than this (logical px) makes a touch a drag, not a tap.
const SLOP: f32 = 10.0;
/// Holding still this long (ms) is a long press.
pub(super) const LONG_PRESS_MS: f64 = 500.0;
/// How fast a fling slows down: velocity × e^(−t / FLING_DECAY_MS).
const FLING_DECAY_MS: f64 = 325.0;

/// A finger on the screen.
#[derive(Clone, Debug)]
pub(super) struct Finger {
    id: u64,
    start: (f32, f32),
    last: (f32, f32),
    t0: f64,
    target: Target,
    /// The page element it started on (touch events go there until it lifts).
    element: Option<NodeId>,
    primary: bool,
    /// The page took the gesture (canceled `touchstart` / `touchmove`, or
    /// `touch-action` without vertical panning).
    page_owns: bool,
    moved: bool,
    long_pressed: bool,
    /// Recent (time, y) positions while scrolling, for the fling speed.
    samples: Vec<(f64, f32)>,
}

/// The page sliding on after a drag.
#[derive(Clone, Copy, Debug)]
pub(super) struct Fling {
    /// Where in the page (viewport coordinates) the drag scrolled.
    at: (f32, f32),
    /// Pixels per millisecond (positive: towards the end of the page).
    velocity: f64,
    last: f64,
}

impl Browser {
    pub(super) fn touch(&mut self, id: u64, x: f32, y: f32, phase: TouchPhase, host: &mut dyn Host) {
        self.touch_used = true;
        match phase {
            TouchPhase::Start => self.touch_start(id, x, y, host),
            TouchPhase::Move => self.touch_move(id, x, y, host),
            TouchPhase::End | TouchPhase::Cancel => self.touch_end(id, x, y, phase == TouchPhase::Cancel, host),
        }
    }

    /// `"touch <id> <primary>"`, for the pointer events of a finger.
    fn finger_data(f: &Finger) -> String {
        format!("touch {} {}", f.id, f.primary as i32)
    }

    /// `"<touches>|<changed>"` for touch events (viewport coordinates).
    fn touch_data(&self, changed: &Finger) -> String {
        let vp = self.viewport_rect();
        let item = |f: &Finger| {
            let target = f.element.map_or(-1, |n| n as i64);
            format!("{},{},{},{target}", f.id, f.last.0 - vp.x, f.last.1 - vp.y)
        };
        let all: Vec<String> = self.fingers.iter().filter(|f| f.target == Target::Page).map(item).collect();
        format!("{}|{}", all.join(";"), item(changed))
    }

    fn touch_init(&self, f: &Finger, touch: String) -> EventInit {
        let vp = self.viewport_rect();
        EventInit { touch, ..EventInit::pointer(f.last.0 - vp.x, f.last.1 - vp.y, 0) }
    }

    fn touch_start(&mut self, id: u64, x: f32, y: f32, host: &mut dyn Host) {
        self.fling = None;
        let (t, lx, ly) = self.target_at(x, y);
        let element = (t == Target::Page).then(|| self.doc(t).hit_test(lx, ly).element).flatten();
        let mut f = Finger {
            id,
            start: (x, y),
            last: (x, y),
            t0: self.now,
            target: t,
            element,
            primary: self.fingers.is_empty(),
            page_owns: false,
            moved: false,
            long_pressed: false,
            samples: vec![(self.now, y)],
        };
        self.fingers.push(f.clone());
        if let Some(e) = element {
            self.tabs[self.active].doc.user_activated = true;
            let canceled_pointer = self.dispatch(e, "pointerdown", self.touch_init(&f, Self::finger_data(&f)), host);
            let canceled_touch = self.dispatch(e, "touchstart", self.touch_init(&f, self.touch_data(&f)), host);
            f.page_owns = canceled_pointer || canceled_touch || !self.pans(e);
            if let Some(g) = self.fingers.iter_mut().find(|g| g.id == id) {
                g.page_owns = f.page_owns;
            }
        }
    }

    /// Whether a vertical drag starting on `node` may scroll (`touch-action`).
    fn pans(&self, node: NodeId) -> bool {
        let doc = &self.tabs[self.active].doc;
        let mut n = Some(node);
        while let Some(e) = n {
            if doc.style(e).is_some_and(|s| !s.touch_pan_y) {
                return false;
            }
            n = doc.dom.parent(e);
        }
        true
    }

    fn touch_move(&mut self, id: u64, x: f32, y: f32, host: &mut dyn Host) {
        let Some(i) = self.fingers.iter().position(|f| f.id == id) else { return };
        let before = self.fingers[i].last;
        {
            let f = &mut self.fingers[i];
            f.last = (x, y);
            if (x - f.start.0).hypot(y - f.start.1) > SLOP {
                f.moved = true;
            }
        }
        let f = self.fingers[i].clone();
        if let Some(e) = f.element {
            self.dispatch(e, "pointermove", self.touch_init(&f, Self::finger_data(&f)), host);
            if self.dispatch(e, "touchmove", self.touch_init(&f, self.touch_data(&f)), host)
                && let Some(g) = self.fingers.get_mut(i)
            {
                g.page_owns = true;
            }
        }
        let Some(f) = self.fingers.get(i).cloned() else { return };
        // One finger, a real drag, on the page, not taken by the page: scroll.
        if f.target != Target::Page || !f.moved || f.page_owns || f.long_pressed || self.fingers.len() != 1 {
            return;
        }
        let dy = before.1 - y;
        let vp = self.viewport_rect();
        let at = (f.start.0 - vp.x, f.start.1 - vp.y);
        self.scroll_page_at(at, dy);
        let now = self.now;
        let g = &mut self.fingers[i];
        g.samples.push((now, y));
        g.samples.retain(|(t, _)| now - t <= 100.0);
    }

    /// Scrolls whatever scrolls under page point `at` by `dy` (a frame first).
    fn scroll_page_at(&mut self, at: (f32, f32), dy: f32) -> bool {
        let doc = &mut self.tabs[self.active].doc;
        let moved = if let Some((n, fx, fy)) = doc.frame_at(at.0, at.1)
            && doc.frame_mut(n).is_some_and(|f| f.scroll_by(fx, fy, dy))
        {
            true
        } else {
            doc.scroll_by(at.0, at.1, dy)
        };
        if moved {
            self.dirty = true;
        }
        moved
    }

    fn touch_end(&mut self, id: u64, x: f32, y: f32, cancel: bool, host: &mut dyn Host) {
        let Some(i) = self.fingers.iter().position(|f| f.id == id) else { return };
        let mut f = self.fingers.remove(i);
        f.last = (x, y);
        let mut canceled = false;
        if let Some(e) = f.element {
            let (pointer, touch) = if cancel { ("pointercancel", "touchcancel") } else { ("pointerup", "touchend") };
            self.dispatch(e, pointer, self.touch_init(&f, Self::finger_data(&f)), host);
            canceled = self.dispatch(e, touch, self.touch_init(&f, self.touch_data(&f)), host);
        }
        if cancel || f.long_pressed {
            return;
        }
        if !f.moved {
            // A tap: the compatibility mouse events and the click (Touch
            // Events §8), unless the page canceled the touch.
            if !f.page_owns && !canceled {
                self.from_touch = true;
                self.mouse_down(x, y, host);
                self.mouse_up(x, y, host);
                self.from_touch = false;
            }
            return;
        }
        // A drag: keep sliding at the finger's speed.
        if f.target == Target::Page && !f.page_owns {
            let (t1, y1) = *f.samples.last().unwrap_or(&(self.now, y));
            let (t0, y0) = *f.samples.first().unwrap_or(&(self.now, y));
            if t1 - t0 > 0.0 {
                let velocity = (y0 - y1) as f64 / (t1 - t0);
                if velocity.abs() > 0.1 {
                    let vp = self.viewport_rect();
                    self.fling = Some(Fling { at: (f.start.0 - vp.x, f.start.1 - vp.y), velocity, last: self.now });
                }
            }
        }
    }

    /// Time-driven touch work: long presses and flings. Called before each frame.
    pub(super) fn touch_tick(&mut self, host: &mut dyn Host) {
        let now = self.now;
        if let Some(i) = self.fingers.iter().position(|f| !f.moved && !f.long_pressed && now - f.t0 >= LONG_PRESS_MS) {
            self.fingers[i].long_pressed = true;
            let (x, y) = self.fingers[i].last;
            self.context_menu(x, y, host);
        }
        if let Some(mut fl) = self.fling {
            let dt = (now - fl.last).max(0.0);
            fl.last = now;
            let dy = fl.velocity * dt;
            fl.velocity *= (-dt / FLING_DECAY_MS).exp();
            let moved = self.scroll_page_at(fl.at, dy as f32);
            self.fling = (moved && fl.velocity.abs() > 0.02).then_some(fl);
        }
    }

    /// When touch input next needs a frame: a long press coming, or a fling.
    pub(super) fn touch_wakeup(&self) -> Option<f64> {
        let press = self
            .fingers
            .iter()
            .filter(|f| !f.moved && !f.long_pressed)
            .map(|f| f.t0 + LONG_PRESS_MS)
            .min_by(f64::total_cmp);
        let fling = self.fling.map(|f| f.last + 16.0);
        press.into_iter().chain(fling).min_by(f64::total_cmp)
    }
}
