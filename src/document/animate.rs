//! Running CSS animations and transitions: each update, the animated values
//! go into an overlay over the cascade's styles (paint-only properties, so
//! no relayout).

use std::collections::{HashMap, HashSet};

use super::Document;
use crate::css::Style;
use crate::css::animation::{self, Timing};
use crate::dom::NodeId;

/// A running transition of one property of one element.
struct Transition {
    from: Style,
    to: Style,
    start: f64,
    duration: f32,
    delay: f32,
    timing: Timing,
}

#[derive(Default)]
pub struct Animator {
    /// The document's time in milliseconds (set by the browser).
    now: f64,
    /// When each animation started, by element and animation name.
    started: HashMap<(NodeId, String), f64>,
    transitions: HashMap<(NodeId, &'static str), Transition>,
    /// Animated styles, in place of the cascade's.
    pub overlay: HashMap<NodeId, Style>,
    /// Whether something is still moving (more frames are needed).
    pub running: bool,
    /// Styles shown before a restyle, of elements that transition.
    before: Vec<(NodeId, Style)>,
}

impl Document {
    /// Sets the document's clock (milliseconds) for animations.
    pub fn set_time(&mut self, now: f64) {
        self.anim.now = now;
    }

    pub(super) fn anim_time(&self) -> f64 {
        self.anim.now
    }

    /// Whether animations or transitions are running.
    pub fn animating(&self) -> bool {
        self.anim.running
    }

    /// Before a restyle: remembers what transitioning elements show.
    pub(super) fn before_restyle(&mut self) {
        self.anim.before = self
            .styles
            .iter()
            .enumerate()
            .filter_map(|(n, s)| {
                let s = s.as_ref().filter(|s| !s.transitions.properties.is_empty())?;
                Some((n, self.anim.overlay.get(&n).unwrap_or(s).clone()))
            })
            .collect();
    }

    /// After a restyle: starts transitions for properties that changed.
    pub(super) fn after_restyle(&mut self) {
        let now = self.anim.now;
        for (n, old) in std::mem::take(&mut self.anim.before) {
            let Some(new) = self.styles.get(n).and_then(Option::as_ref) else {
                continue;
            };
            for &prop in animation::PROPERTIES {
                if !animation::differs(&old, new, prop) {
                    continue;
                }
                // A running transition towards the same value goes on.
                if let Some(t) = self.anim.transitions.get(&(n, prop))
                    && !animation::differs(&t.to, new, prop)
                {
                    continue;
                }
                match new.transitions.of(prop) {
                    Some((duration, delay, timing)) => {
                        let t = Transition {
                            from: old.clone(),
                            to: new.clone(),
                            start: now,
                            duration,
                            delay,
                            timing,
                        };
                        self.anim.transitions.insert((n, prop), t);
                    }
                    None => {
                        self.anim.transitions.remove(&(n, prop));
                    }
                }
            }
        }
    }

    /// Computes the animated styles for the current time.
    pub(super) fn animate(&mut self) {
        let now = self.anim.now;
        let keyframes = self.cascade.as_ref().map(|c| c.2.keyframes.clone());
        let mut overlay = HashMap::new();
        let mut running = false;
        let mut seen = HashSet::new();
        let venv = self.value_env();
        for (n, s) in self.styles.iter().enumerate() {
            let Some(base) = s.as_ref() else { continue };
            let mut out: Option<Style> = None;
            if !base.animations.names.is_empty()
                && let Some(keyframes) = &keyframes
            {
                let parent = self
                    .dom
                    .parent(n)
                    .and_then(|p| self.styles.get(p)?.as_ref())
                    .cloned()
                    .unwrap_or_default();
                for a in base.animations.list() {
                    let Some(frames) = keyframes.get(&a.name) else { continue };
                    let key = (n, a.name.clone());
                    let start = *self.anim.started.entry(key.clone()).or_insert(now);
                    seen.insert(key);
                    if a.paused {
                        continue;
                    }
                    let (p, live) = animation::progress(&a, (now - start) as f32);
                    running |= live;
                    let Some(p) = p else { continue };
                    let under = out.clone().unwrap_or_else(|| base.clone());
                    let value = sample(frames, p, &under, &parent, &venv);
                    let mut cur = under;
                    animation::copy_animated(&mut cur, &value);
                    out = Some(cur);
                }
            }
            for &prop in animation::PROPERTIES {
                let Some(t) = self.anim.transitions.get(&(n, prop)) else { continue };
                let local = ((now - t.start) as f32 - t.delay) / t.duration;
                let p = t.timing.apply(local.clamp(0.0, 1.0));
                running |= local < 1.0;
                let value = animation::interpolate(&t.from, &t.to, p);
                let cur = out.get_or_insert_with(|| base.clone());
                animation::take(cur, &value, prop);
            }
            if let Some(o) = out {
                overlay.insert(n, o);
            }
        }
        // Finished transitions and animations of elements that lost them.
        self.anim.transitions.retain(|_, t| {
            ((now - t.start) as f32) < t.delay + t.duration
        });
        self.anim.started.retain(|k, _| seen.contains(k));
        if !overlay.is_empty() || !self.anim.overlay.is_empty() {
            self.anim.overlay = overlay;
        }
        self.anim.running = running || !self.anim.transitions.is_empty();
    }

    /// Units for resolving keyframe values.
    fn value_env(&self) -> crate::css::ValueEnv {
        crate::css::ValueEnv {
            root_font: self
                .dom
                .html()
                .and_then(|h| self.styles.get(h)?.as_ref())
                .map_or(16.0, |s| s.font_size),
            vw: self.size.0,
            vh: self.size.1,
        }
    }
}

/// The style at progress `p` of keyframes `frames` over `base` (missing
/// `from` / `to` frames are `base` itself).
fn sample(
    frames: &[animation::Keyframe],
    p: f32,
    base: &Style,
    parent: &Style,
    venv: &crate::css::ValueEnv,
) -> Style {
    let style_at = |k: Option<&animation::Keyframe>| {
        let mut s = base.clone();
        if let Some(k) = k {
            for d in k.decls.iter() {
                let v = if d.value.contains("var(") {
                    crate::css::substitute_vars(&d.value, &s.vars, 0).unwrap_or_default()
                } else {
                    d.value.clone()
                };
                s.apply_in(&d.name, &v, parent, venv);
            }
        }
        s
    };
    let below = frames.iter().rev().find(|k| k.offset <= p);
    let above = frames.iter().find(|k| k.offset >= p);
    let (o1, o2) = (below.map_or(0.0, |k| k.offset), above.map_or(1.0, |k| k.offset));
    let (a, b) = (style_at(below), style_at(above));
    let t = if o2 > o1 { (p - o1) / (o2 - o1) } else { 1.0 };
    animation::interpolate(&a, &b, t)
}
