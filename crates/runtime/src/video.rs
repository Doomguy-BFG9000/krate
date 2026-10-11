//! Video playback behind `krate:gfx/video` (IC-922).
//!
//! The platform decodes and plays the sound; the app only asks for the
//! current frame by handle when it draws. Each platform supplies `open`,
//! `control`, `state`, `frame` and `close`; a platform without one answers
//! every call with the sentence `UNSUPPORTED`, so an app can say so instead
//! of showing a black rectangle.

use std::collections::BTreeSet;
use std::path::PathBuf;

#[cfg(not(any(target_os = "macos", target_os = "ios", target_os = "android")))]
use krate_adapter_common::ui::ImagePixels;

/// What to play.
#[derive(Debug, Clone)]
pub(crate) enum Source {
    /// A file inside the app's bundled assets, already resolved and checked.
    File(PathBuf),
    /// An `https://` address the app was granted `net.connect` to.
    Url(String),
}

/// One thing to tell a player.
#[derive(Debug, Clone, Copy)]
pub(crate) enum Control {
    Play,
    Pause,
    Seek(u64),
    Looping(bool),
    Muted(bool),
}

/// Where a video is and what it is doing; the WIT `video-state`.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(crate) struct State {
    pub position_millis: u64,
    pub duration_millis: u64,
    pub width: u32,
    pub height: u32,
    pub ready: bool,
    pub playing: bool,
    pub ended: bool,
    pub problem: String,
}

#[cfg(any(target_os = "macos", target_os = "ios"))]
pub(crate) use crate::video_apple::{close, control, frame, open, state};

// MediaPlayer into an ImageReader, through the Android player (IC-922).
#[cfg(target_os = "android")]
pub(crate) use crate::video_android::{close, control, frame, open, state};

#[cfg(not(any(target_os = "macos", target_os = "ios", target_os = "android")))]
const UNSUPPORTED: &str = "video playback is not on this system yet";

#[cfg(not(any(target_os = "macos", target_os = "ios", target_os = "android")))]
pub(crate) fn open(_source: Source) -> Result<u64, String> {
    Err(UNSUPPORTED.to_string())
}

#[cfg(not(any(target_os = "macos", target_os = "ios", target_os = "android")))]
pub(crate) fn control(_video: u64, _control: Control) -> Result<(), String> {
    Err(UNSUPPORTED.to_string())
}

#[cfg(not(any(target_os = "macos", target_os = "ios", target_os = "android")))]
pub(crate) fn state(_video: u64) -> Result<State, String> {
    Err(UNSUPPORTED.to_string())
}

/// The frame to show now, or `None` when the one shown last is still right.
#[cfg(not(any(target_os = "macos", target_os = "ios", target_os = "android")))]
pub(crate) fn frame(_video: u64) -> Result<Option<ImagePixels>, String> {
    Err(UNSUPPORTED.to_string())
}

#[cfg(not(any(target_os = "macos", target_os = "ios", target_os = "android")))]
pub(crate) fn close(_video: u64) {}

/// The videos one run opened. A run controls only its own, and every one it
/// left open stops when the run ends, so a closed app is never heard.
#[derive(Debug, Default)]
pub(crate) struct Videos {
    open: BTreeSet<u64>,
}

impl Videos {
    pub(crate) fn add(&mut self, video: u64) {
        self.open.insert(video);
    }

    pub(crate) fn owns(&self, video: u64) -> bool {
        self.open.contains(&video)
    }

    pub(crate) fn remove(&mut self, video: u64) -> bool {
        self.open.remove(&video)
    }
}

impl Drop for Videos {
    fn drop(&mut self) {
        for video in std::mem::take(&mut self.open) {
            close(video);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_run_owns_only_the_videos_it_opened() {
        let mut videos = Videos::default();
        videos.add(7);
        assert!(videos.owns(7));
        assert!(!videos.owns(8), "another run's video is not this run's");
        assert!(videos.remove(7));
        assert!(!videos.owns(7), "a closed video is gone");
        assert!(!videos.remove(7));
    }
}
