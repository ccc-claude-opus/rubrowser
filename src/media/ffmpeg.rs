//! The FFmpeg playback backend (feature `media`).

use super::Player;

/// Opens `url` with FFmpeg.
pub fn open(_url: &str) -> Option<Box<dyn Player>> {
    None
}
