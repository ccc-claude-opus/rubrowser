//! `<video>` and `<audio>` elements: which resource each plays (its `src`,
//! or the first `<source>` this browser can play), its player, and the
//! media events to fire.

use std::collections::HashMap;
use std::rc::Rc;

use super::Document;
use crate::dom::{Dom, NodeId};
use crate::media::{self, Frame, Info, Player};

/// A media element's resource and player.
pub struct MediaElement {
    /// The resolved URL being played (empty: none).
    pub url: String,
    pub player: Option<Box<dyn Player>>,
    /// `volume` and `muted` as scripts set them.
    pub volume: f32,
    pub muted: bool,
    /// Whether the user or a script started it (shows the frame over the
    /// poster).
    pub started: bool,
}

pub type MediaElements = HashMap<NodeId, MediaElement>;

impl MediaElement {
    /// What scripts see: the player's state, or an error / empty one.
    pub fn info(&self) -> Info {
        match &self.player {
            Some(p) => p.info(),
            None if self.url.is_empty() => Info {
                duration: f64::NAN,
                paused: true,
                ..Info::default()
            },
            // Not playable here: MEDIA_ERR_SRC_NOT_SUPPORTED.
            None => Info {
                duration: f64::NAN,
                paused: true,
                error: Some(4),
                ..Info::default()
            },
        }
    }

    pub fn frame(&self) -> Option<Rc<Frame>> {
        self.player.as_ref()?.frame()
    }
}

/// Height of the control bar (`controls`).
pub const CONTROLS_HEIGHT: f32 = 32.0;

/// Where the progress track of the controls runs (x from, x to) in a media
/// element's content box `area`.
pub fn progress_track(area: crate::layout::RectF) -> (f32, f32) {
    (area.x + 130.0, area.x + area.w - 40.0)
}

/// Whether `n` is a media element.
pub fn is_media(dom: &Dom, n: NodeId) -> bool {
    matches!(dom.tag(n), Some("video" | "audio"))
}

/// The resource `n` plays (HTML's resource selection): its `src`, else the
/// first `<source>` child whose `type` this browser may play.
fn source(dom: &Dom, n: NodeId) -> Option<String> {
    if let Some(src) = dom.attr(n, "src").filter(|s| !s.trim().is_empty()) {
        return Some(src.trim().to_string());
    }
    dom.children(n).iter().find_map(|&c| {
        if dom.tag(c) != Some("source") {
            return None;
        }
        let src = dom.attr(c, "src").filter(|s| !s.trim().is_empty())?;
        let playable = dom
            .attr(c, "type")
            .is_none_or(|t| !media::can_play(t).is_empty());
        playable.then(|| src.trim().to_string())
    })
}

impl Document {
    /// Starts players for media elements whose resource is new or changed.
    pub(super) fn find_media(&mut self) {
        let base = self.base_url();
        let nodes: Vec<NodeId> = self
            .dom
            .descendants(Dom::ROOT)
            .into_iter()
            .filter(|&n| is_media(&self.dom, n))
            .collect();
        for n in nodes {
            let url = source(&self.dom, n)
                .map(|s| crate::net::url::resolve(&base, &s))
                .unwrap_or_default();
            if self.media.get(&n).is_some_and(|m| m.url == url) {
                continue;
            }
            let muted = self.dom.attr(n, "muted").is_some();
            let mut el = MediaElement {
                url: url.clone(),
                player: None,
                volume: 1.0,
                muted,
                started: false,
            };
            if !url.is_empty() {
                el.player = media::open(&url);
                match el.player.as_mut() {
                    Some(p) => {
                        p.set_volume(1.0, muted);
                        p.set_loop(self.dom.attr(n, "loop").is_some());
                        // Autoplay is allowed muted (as in Chrome).
                        if self.dom.attr(n, "autoplay").is_some() && muted {
                            p.play();
                            el.started = true;
                        }
                    }
                    None => self.media_events.push((n, "error")),
                }
            }
            self.media.insert(n, el);
            self.needs_layout = true;
        }
    }

    /// Advances the players; their events wait in `media_events`.
    pub(super) fn update_media(&mut self) {
        for (&n, el) in self.media.iter_mut() {
            let Some(p) = el.player.as_mut() else {
                continue;
            };
            let before = p.info();
            for e in p.update() {
                self.media_events.push((n, e));
            }
            let after = p.info();
            if (after.width, after.height) != (before.width, before.height) {
                self.needs_layout = true;
            }
        }
    }

    /// Media events to fire at elements since the last call.
    pub fn take_media_events(&mut self) -> Vec<(NodeId, &'static str)> {
        std::mem::take(&mut self.media_events)
    }

    /// Whether some media is playing (frames keep coming).
    pub fn media_playing(&self) -> bool {
        self.media
            .values()
            .any(|m| m.player.as_ref().is_some_and(|p| !p.info().paused))
    }

    /// Starts or pauses media element `n` (its controls were clicked).
    pub fn toggle_media(&mut self, n: NodeId) {
        let playing = self.media.get(&n).is_some_and(|m| !m.info().paused);
        let cmd = if playing { "pause" } else { "play" };
        self.media_command(n, cmd, true);
    }

    /// A script's (or the controls') request to media element `n`: `play`,
    /// `pause`, `seek␁t`, `volume␁v`, `muted␁0|1`, `loop␁0|1`, `load`, or
    /// `info` (returns its state, comma-separated). `gesture`: the user
    /// asked (playing with sound is then allowed).
    pub fn media_command(&mut self, n: NodeId, cmd: &str, gesture: bool) -> Option<String> {
        if !self.media.contains_key(&n) {
            self.find_media();
        }
        let args: Vec<&str> = cmd.split('\u{1}').collect();
        let num = |i: usize| args.get(i).and_then(|v| v.parse::<f64>().ok());
        if args[0] == "load" {
            self.media.remove(&n);
            self.find_media();
            return None;
        }
        let el = self.media.get_mut(&n)?;
        match args[0] {
            "play" => {
                let Some(p) = el.player.as_mut() else {
                    return Some("NotSupportedError".into());
                };
                // Chrome's autoplay policy: sound needs a user gesture.
                if !el.muted && !gesture && !self.user_activated {
                    return Some("NotAllowedError".into());
                }
                let was_paused = p.info().paused;
                p.play();
                el.started = true;
                if was_paused {
                    self.media_events.push((n, "play"));
                }
                return Some("ok".into());
            }
            "pause" => {
                if let Some(p) = el.player.as_mut()
                    && !p.info().paused
                {
                    p.pause();
                    self.media_events.push((n, "pause"));
                }
            }
            "seek" => {
                if let (Some(p), Some(t)) = (el.player.as_mut(), num(1)) {
                    p.seek(t.max(0.0));
                    el.started = true;
                    self.media_events
                        .extend([(n, "seeking"), (n, "timeupdate"), (n, "seeked")]);
                }
            }
            "volume" | "muted" => {
                if args[0] == "volume" {
                    el.volume = num(1).unwrap_or(1.0).clamp(0.0, 1.0) as f32;
                } else {
                    el.muted = num(1).is_some_and(|v| v != 0.0);
                }
                let (v, m) = (el.volume, el.muted);
                if let Some(p) = el.player.as_mut() {
                    p.set_volume(v, m);
                }
                self.media_events.push((n, "volumechange"));
            }
            "loop" => {
                let on = num(1).is_some_and(|v| v != 0.0);
                if let Some(p) = el.player.as_mut() {
                    p.set_loop(on);
                }
            }
            "info" => {
                let i = el.info();
                return Some(format!(
                    "{},{},{},{},{},{},{},{},{},{},{}",
                    i.paused as u8,
                    i.ended as u8,
                    i.current_time,
                    i.duration,
                    i.ready,
                    el.volume,
                    el.muted as u8,
                    i.width,
                    i.height,
                    i.error.unwrap_or(0),
                    el.url,
                ));
            }
            _ => {}
        }
        None
    }

    /// The media element of `n`, if any.
    pub fn media(&self, n: NodeId) -> Option<&MediaElement> {
        self.media.get(&n)
    }
}
