use std::collections::VecDeque;
use std::sync::{Arc, Mutex};

use krate::bindings::krate::gpu::gpu as g;
use krate::ui::{pointer, tree, types as kt, window as kw};

use crate::cursor::Cursor;
use crate::dpi::{LogicalSize, PhysicalPosition, PhysicalSize, Position, Size};
use crate::platform_impl::Fullscreen;
use crate::window::ImePurpose;
use crate::{error, window};

use super::event_loop::Shared;
use super::{ActiveEventLoop, MonitorHandle, OsError, WindowId, CANVAS};

fn node(id: u64, parent: Option<u64>, kind: kt::WidgetKind) -> kt::WidgetNode {
    kt::WidgetNode {
        id,
        parent,
        kind,
        label: None,
        role: None,
        style: kt::Style {
            width: None,
            height: None,
            grow: 1.0,
            padding: 0.0,
            text: None,
            place: None,
            box_: None,
        },
        checked: None,
        value: None,
        selected: None,
        text_cursor: None,
    }
}

/// A Krate window holding one canvas.
pub struct Window {
    id: u64,
    shared: Arc<Shared>,
    title: Mutex<String>,
    redraws: Arc<Mutex<VecDeque<WindowId>>>,
    destroys: Arc<Mutex<VecDeque<WindowId>>>,
    fullscreen: Mutex<Option<Fullscreen>>,
}

impl Window {
    pub(crate) fn new(
        el: &ActiveEventLoop,
        attrs: window::WindowAttributes,
    ) -> Result<Self, error::OsError> {
        let logical: LogicalSize<u32> = attrs
            .inner_size
            .map(|s| s.to_logical(1.0))
            .unwrap_or(LogicalSize::new(1024, 768));
        let id = kw::create(
            &attrs.title,
            kt::WindowSize {
                width: logical.width.max(1),
                height: logical.height.max(1),
            },
        )
        .map_err(|e| os_error!(OsError::new(e)))?;
        tree::set_root(id, &node(1, None, kt::WidgetKind::Stack))
            .map_err(|e| os_error!(OsError::new(e)))?;
        tree::upsert_node(id, &node(CANVAS, Some(1), kt::WidgetKind::Canvas))
            .map_err(|e| os_error!(OsError::new(e)))?;
        let probe = g::create_surface(id, CANVAS).map_err(|e| os_error!(OsError::new(e)))?;
        el.shared.add(id, probe, (logical.width, logical.height));
        if attrs.fullscreen.is_some() {
            let _ = kw::set_state(id, kw::WindowState::Fullscreen);
        } else if attrs.maximized {
            let _ = kw::set_state(id, kw::WindowState::Maximized);
        }
        el.creates.lock().unwrap().push_back(WindowId { fd: id });
        Ok(Self {
            id,
            shared: el.shared.clone(),
            title: Mutex::new(attrs.title.clone()),
            redraws: el.redraws.clone(),
            destroys: el.destroys.clone(),
            fullscreen: Mutex::new(attrs.fullscreen.map(Into::into)),
        })
    }

    pub(crate) fn maybe_queue_on_main(&self, f: impl FnOnce(&Self) + Send + 'static) {
        f(self)
    }

    pub(crate) fn maybe_wait_on_main<R: Send>(&self, f: impl FnOnce(&Self) -> R + Send) -> R {
        f(self)
    }

    #[inline]
    pub fn id(&self) -> WindowId {
        WindowId { fd: self.id }
    }

    #[inline]
    pub fn primary_monitor(&self) -> Option<MonitorHandle> {
        Some(MonitorHandle)
    }

    #[inline]
    pub fn available_monitors(&self) -> VecDeque<MonitorHandle> {
        let mut v = VecDeque::with_capacity(1);
        v.push_back(MonitorHandle);
        v
    }

    #[inline]
    pub fn current_monitor(&self) -> Option<MonitorHandle> {
        Some(MonitorHandle)
    }

    #[inline]
    pub fn scale_factor(&self) -> f64 {
        self.shared.scale(self.id)
    }

    #[inline]
    pub fn request_redraw(&self) {
        let window_id = self.id();
        let mut redraws = self.redraws.lock().unwrap();
        if !redraws.contains(&window_id) {
            redraws.push_back(window_id);
        }
    }

    #[inline]
    pub fn pre_present_notify(&self) {}

    #[inline]
    pub fn reset_dead_keys(&self) {}

    #[inline]
    pub fn inner_position(&self) -> Result<PhysicalPosition<i32>, error::NotSupportedError> {
        Ok((0, 0).into())
    }

    #[inline]
    pub fn outer_position(&self) -> Result<PhysicalPosition<i32>, error::NotSupportedError> {
        Ok((0, 0).into())
    }

    #[inline]
    pub fn set_outer_position(&self, _position: Position) {}

    #[inline]
    pub fn inner_size(&self) -> PhysicalSize<u32> {
        let (w, h) = self.shared.pixel_size(self.id);
        PhysicalSize::new(w, h)
    }

    #[inline]
    pub fn request_inner_size(&self, size: Size) -> Option<PhysicalSize<u32>> {
        let logical: LogicalSize<u32> = size.to_logical(self.scale_factor());
        let _ = kw::set_size(
            self.id,
            kt::WindowSize {
                width: logical.width.max(1),
                height: logical.height.max(1),
            },
        );
        None
    }

    #[inline]
    pub fn outer_size(&self) -> PhysicalSize<u32> {
        self.inner_size()
    }

    #[inline]
    pub fn set_min_inner_size(&self, _: Option<Size>) {}

    #[inline]
    pub fn set_max_inner_size(&self, _: Option<Size>) {}

    #[inline]
    pub fn title(&self) -> String {
        self.title.lock().unwrap().clone()
    }

    #[inline]
    pub fn set_title(&self, title: &str) {
        *self.title.lock().unwrap() = title.to_string();
        let _ = kw::set_title(self.id, title);
    }

    #[inline]
    pub fn set_transparent(&self, _transparent: bool) {}

    #[inline]
    pub fn set_blur(&self, _blur: bool) {}

    #[inline]
    pub fn set_visible(&self, visible: bool) {
        if visible {
            let _ = kw::show(self.id);
        }
    }

    #[inline]
    pub fn is_visible(&self) -> Option<bool> {
        Some(true)
    }

    #[inline]
    pub fn resize_increments(&self) -> Option<PhysicalSize<u32>> {
        None
    }

    #[inline]
    pub fn set_resize_increments(&self, _increments: Option<Size>) {}

    #[inline]
    pub fn set_resizable(&self, _resizeable: bool) {}

    #[inline]
    pub fn is_resizable(&self) -> bool {
        true
    }

    #[inline]
    pub fn set_minimized(&self, minimized: bool) {
        let _ = kw::set_state(
            self.id,
            if minimized {
                kw::WindowState::Minimized
            } else {
                kw::WindowState::Normal
            },
        );
    }

    #[inline]
    pub fn is_minimized(&self) -> Option<bool> {
        None
    }

    #[inline]
    pub fn set_maximized(&self, maximized: bool) {
        let _ = kw::set_state(
            self.id,
            if maximized {
                kw::WindowState::Maximized
            } else {
                kw::WindowState::Normal
            },
        );
    }

    #[inline]
    pub fn is_maximized(&self) -> bool {
        false
    }

    #[inline]
    pub(crate) fn set_fullscreen(&self, monitor: Option<Fullscreen>) {
        let state = if monitor.is_some() {
            kw::WindowState::Fullscreen
        } else {
            kw::WindowState::Normal
        };
        let _ = kw::set_state(self.id, state);
        *self.fullscreen.lock().unwrap() = monitor;
    }

    #[inline]
    pub(crate) fn fullscreen(&self) -> Option<Fullscreen> {
        self.fullscreen.lock().unwrap().clone()
    }

    #[inline]
    pub fn set_decorations(&self, _decorations: bool) {}

    #[inline]
    pub fn is_decorated(&self) -> bool {
        true
    }

    #[inline]
    pub fn set_window_level(&self, _level: window::WindowLevel) {}

    #[inline]
    pub fn set_window_icon(&self, _window_icon: Option<crate::icon::Icon>) {}

    #[inline]
    pub fn set_ime_cursor_area(&self, _position: Position, _size: Size) {}

    #[inline]
    pub fn set_ime_allowed(&self, _allowed: bool) {}

    #[inline]
    pub fn set_ime_purpose(&self, _purpose: ImePurpose) {}

    #[inline]
    pub fn focus_window(&self) {}

    #[inline]
    pub fn request_user_attention(&self, _request_type: Option<window::UserAttentionType>) {}

    #[inline]
    pub fn set_cursor(&self, _: Cursor) {}

    #[inline]
    pub fn set_cursor_position(&self, _: Position) -> Result<(), error::ExternalError> {
        Err(error::ExternalError::NotSupported(error::NotSupportedError::new()))
    }

    /// Confined and Locked both hold the pointer (`krate:ui/pointer`
    /// capture): hidden, and reported as raw motion.
    #[inline]
    pub fn set_cursor_grab(&self, mode: window::CursorGrabMode) -> Result<(), error::ExternalError> {
        match mode {
            window::CursorGrabMode::None => {
                pointer::release(self.id);
                Ok(())
            },
            window::CursorGrabMode::Confined | window::CursorGrabMode::Locked => {
                pointer::capture(self.id).map_err(|e| error::ExternalError::Os(os_error!(OsError::new(e))))
            },
        }
    }

    /// Krate hides the pointer while it is held and shows it otherwise.
    #[inline]
    pub fn set_cursor_visible(&self, _visible: bool) {}

    #[inline]
    pub fn drag_window(&self) -> Result<(), error::ExternalError> {
        Err(error::ExternalError::NotSupported(error::NotSupportedError::new()))
    }

    #[inline]
    pub fn drag_resize_window(
        &self,
        _direction: window::ResizeDirection,
    ) -> Result<(), error::ExternalError> {
        Err(error::ExternalError::NotSupported(error::NotSupportedError::new()))
    }

    #[inline]
    pub fn show_window_menu(&self, _position: Position) {}

    #[inline]
    pub fn set_cursor_hittest(&self, _hittest: bool) -> Result<(), error::ExternalError> {
        Err(error::ExternalError::NotSupported(error::NotSupportedError::new()))
    }

    /// The canvas, named as wgpu-krate reads it: a Web handle whose id is
    /// `window * 256 + canvas`.
    #[cfg(feature = "rwh_06")]
    #[inline]
    pub fn raw_window_handle_rwh_06(&self) -> Result<rwh_06::RawWindowHandle, rwh_06::HandleError> {
        Ok(rwh_06::RawWindowHandle::Web(rwh_06::WebWindowHandle::new(super::web_id(self.id))))
    }

    #[cfg(feature = "rwh_06")]
    #[inline]
    pub fn raw_display_handle_rwh_06(
        &self,
    ) -> Result<rwh_06::RawDisplayHandle, rwh_06::HandleError> {
        Ok(rwh_06::RawDisplayHandle::Web(rwh_06::WebDisplayHandle::new()))
    }

    #[cfg(feature = "rwh_05")]
    #[inline]
    pub fn raw_window_handle_rwh_05(&self) -> rwh_05::RawWindowHandle {
        let mut handle = rwh_05::WebWindowHandle::empty();
        handle.id = super::web_id(self.id);
        rwh_05::RawWindowHandle::Web(handle)
    }

    #[cfg(feature = "rwh_05")]
    #[inline]
    pub fn raw_display_handle_rwh_05(&self) -> rwh_05::RawDisplayHandle {
        rwh_05::RawDisplayHandle::Web(rwh_05::WebDisplayHandle::empty())
    }

    #[cfg(feature = "rwh_04")]
    #[inline]
    pub fn raw_window_handle_rwh_04(&self) -> rwh_04::RawWindowHandle {
        let mut handle = rwh_04::WebHandle::empty();
        handle.id = super::web_id(self.id);
        rwh_04::RawWindowHandle::Web(handle)
    }

    #[inline]
    pub fn set_enabled_buttons(&self, _buttons: window::WindowButtons) {}

    #[inline]
    pub fn enabled_buttons(&self) -> window::WindowButtons {
        window::WindowButtons::all()
    }

    #[inline]
    pub fn theme(&self) -> Option<window::Theme> {
        None
    }

    #[inline]
    pub fn has_focus(&self) -> bool {
        true
    }

    #[inline]
    pub fn set_theme(&self, _theme: Option<window::Theme>) {}

    pub fn set_content_protected(&self, _protected: bool) {}
}

impl Drop for Window {
    fn drop(&mut self) {
        let _ = kw::close(self.id);
        self.destroys.lock().unwrap().push_back(self.id());
    }
}
