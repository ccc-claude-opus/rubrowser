//! Audio and video playback behind `<video>` / `<audio>`: a [`Player`]
//! per element, from a backend (FFmpeg in native builds with the `media`
//! feature; none otherwise, so media elements report an error as the HTML
//! spec describes for unplayable resources).

#[cfg(feature = "media")]
mod ffmpeg;

use std::rc::Rc;

/// A decoded video frame in opaque `0x00RRGGBB` pixels.
#[derive(Debug)]
pub struct Frame {
    pub width: u32,
    pub height: u32,
    pub rgb: Vec<u32>,
}

/// What a player knows about its media.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Info {
    /// Seconds; `NaN` until known, `inf` for live streams.
    pub duration: f64,
    pub current_time: f64,
    pub paused: bool,
    pub ended: bool,
    /// The video's natural size (0 × 0 for audio).
    pub width: u32,
    pub height: u32,
    /// HTML `readyState`: 0 nothing, 1 metadata, 2 current data, 4 enough.
    pub ready: u8,
    /// A `MediaError` code (2 network, 3 decode, 4 not supported).
    pub error: Option<u8>,
}

/// One media resource being played.
pub trait Player {
    fn play(&mut self);
    fn pause(&mut self);
    fn seek(&mut self, seconds: f64);
    fn set_volume(&mut self, volume: f32, muted: bool);
    fn set_loop(&mut self, looping: bool);
    /// Catches up with the clock; returns the media events that happened
    /// (`loadedmetadata`, `playing`, `timeupdate`, `ended`, …).
    fn update(&mut self) -> Vec<&'static str>;
    fn info(&self) -> Info;
    /// The frame to show now, if any.
    fn frame(&self) -> Option<Rc<Frame>>;
}

/// Opens `url` for playback, if this build can play media.
pub fn open(url: &str) -> Option<Box<dyn Player>> {
    #[cfg(feature = "media")]
    {
        return ffmpeg::open(url);
    }
    #[allow(unreachable_code)]
    {
        let _ = url;
        None
    }
}

/// `canPlayType` for a MIME type (with optional `codecs`): `"probably"`,
/// `"maybe"` or `""`.
pub fn can_play(mime: &str) -> &'static str {
    if !cfg!(feature = "media") {
        return "";
    }
    let base = mime
        .split(';')
        .next()
        .unwrap_or("")
        .trim()
        .to_ascii_lowercase();
    let known = matches!(
        base.as_str(),
        "video/mp4"
            | "video/webm"
            | "video/ogg"
            | "video/quicktime"
            | "video/x-matroska"
            | "audio/mp4"
            | "audio/mpeg"
            | "audio/mp3"
            | "audio/aac"
            | "audio/wav"
            | "audio/wave"
            | "audio/x-wav"
            | "audio/webm"
            | "audio/ogg"
            | "audio/flac"
            | "audio/x-m4a"
            | "application/ogg"
    );
    match (known, mime.contains("codecs")) {
        (false, _) => "",
        (true, true) => "probably",
        (true, false) => "maybe",
    }
}
