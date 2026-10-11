//! winit on Krate: a backend for apps that run inside Krate (IC-930).
//!
//! A winit window is a Krate window (`krate:ui/window`) holding one canvas
//! widget; its events are Krate's (`krate:ui/events`), its raw window handle
//! names that canvas the way wgpu-krate expects (a Web handle whose id is
//! `window * 256 + canvas`), and its pixel size and scale come from the
//! canvas's surface (`krate:gpu`). Everything else in winit is unchanged:
//! `make-winit.py` copies winit 0.30.13 and adds this directory as
//! `platform_impl/krate`, selected for `target_os = "wasi"`.

#![cfg(target_os = "wasi")]

use std::fmt::{self, Display, Formatter};
use std::sync::Arc;

use smol_str::SmolStr;

use crate::dpi::{PhysicalPosition, PhysicalSize};
use crate::keyboard::Key;

// Answers the program's WASI calls inside it, as every Krate app's are.
use krate_c_runtime as _;

pub(crate) use self::event_loop::{ActiveEventLoop, EventLoop, EventLoopProxy, OwnedDisplayHandle};
mod event_loop;

pub use self::window::Window;
mod window;

/// The canvas widget every Krate winit window draws on.
pub(crate) const CANVAS: u64 = 2;

/// The raw-window-handle id that names `window`'s canvas for wgpu-krate.
pub(crate) fn web_id(window: u64) -> u32 {
    (window as u32) * 256 + CANVAS as u32
}

#[derive(Default, Debug, Copy, Clone, PartialEq, Eq, Hash)]
pub(crate) struct PlatformSpecificEventLoopAttributes {}

#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct WindowId {
    pub(crate) fd: u64,
}

impl WindowId {
    pub const fn dummy() -> Self {
        WindowId { fd: u64::MAX }
    }
}

impl From<WindowId> for u64 {
    fn from(id: WindowId) -> Self {
        id.fd
    }
}

impl From<u64> for WindowId {
    fn from(fd: u64) -> Self {
        Self { fd }
    }
}

#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct DeviceId;

impl DeviceId {
    pub const fn dummy() -> Self {
        DeviceId
    }
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct PlatformSpecificWindowAttributes;

#[derive(Clone, Debug)]
pub struct OsError(Arc<String>);

impl OsError {
    pub(crate) fn new(error: impl std::fmt::Debug) -> Self {
        Self(Arc::new(format!("{error:?}")))
    }
}

impl Display for OsError {
    fn fmt(&self, fmt: &mut Formatter<'_>) -> Result<(), fmt::Error> {
        self.0.fmt(fmt)
    }
}

pub(crate) use crate::cursor::{
    NoCustomCursor as PlatformCustomCursor, NoCustomCursor as PlatformCustomCursorSource,
};
pub(crate) use crate::icon::NoIcon as PlatformIcon;

#[derive(Clone, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct MonitorHandle;

impl MonitorHandle {
    pub fn name(&self) -> Option<String> {
        Some("Krate screen".to_owned())
    }

    pub fn size(&self) -> PhysicalSize<u32> {
        PhysicalSize::new(0, 0) // TODO
    }

    pub fn position(&self) -> PhysicalPosition<i32> {
        (0, 0).into()
    }

    pub fn scale_factor(&self) -> f64 {
        1.0 // TODO
    }

    pub fn refresh_rate_millihertz(&self) -> Option<u32> {
        // FIXME no way to get real refresh rate for now.
        None
    }

    pub fn video_modes(&self) -> impl Iterator<Item = VideoModeHandle> {
        let size = self.size().into();
        // FIXME this is not the real refresh rate
        // (it is guaranteed to support 32 bit color though)
        std::iter::once(VideoModeHandle {
            size,
            bit_depth: 32,
            refresh_rate_millihertz: 60000,
            monitor: self.clone(),
        })
    }
}

#[derive(Clone, Debug, Eq, Hash, PartialEq)]
pub struct VideoModeHandle {
    size: (u32, u32),
    bit_depth: u16,
    refresh_rate_millihertz: u32,
    monitor: MonitorHandle,
}

impl VideoModeHandle {
    pub fn size(&self) -> PhysicalSize<u32> {
        self.size.into()
    }

    pub fn bit_depth(&self) -> u16 {
        self.bit_depth
    }

    pub fn refresh_rate_millihertz(&self) -> u32 {
        self.refresh_rate_millihertz
    }

    pub fn monitor(&self) -> MonitorHandle {
        self.monitor.clone()
    }
}

#[derive(Debug, Clone, Eq, PartialEq, Hash)]
pub struct KeyEventExtra {
    pub key_without_modifiers: Key,
    pub text_with_all_modifiers: Option<SmolStr>,
}
