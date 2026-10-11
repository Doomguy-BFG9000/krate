use std::cell::Cell;
use std::collections::{HashMap, VecDeque};
use std::marker::PhantomData;
use std::sync::{mpsc, Arc, Mutex};
use std::time::Instant;

use krate::bindings::krate::gpu::gpu as g;
use krate::ui::{events, pointer, types as kt};
use smol_str::SmolStr;

use crate::error::EventLoopError;
use crate::event::{self, Modifiers, StartCause};
use crate::event_loop::{self, ControlFlow, DeviceEvents};
use crate::keyboard::{
    Key, KeyCode, KeyLocation, ModifiersKeys, ModifiersState, NamedKey, NativeKey, NativeKeyCode,
    PhysicalKey,
};
use crate::window::{
    CustomCursor as RootCustomCursor, CustomCursorSource, Theme, WindowId as RootWindowId,
};

use super::{DeviceId, KeyEventExtra, MonitorHandle, PlatformSpecificEventLoopAttributes, WindowId};

/// What the windows of one event loop share with it: each window's size
/// probe (a krate:gpu surface on its canvas, asked only for its pixel size)
/// and its logical size.
#[derive(Default)]
pub(crate) struct Shared {
    windows: Mutex<HashMap<u64, (u64, (u32, u32))>>,
}

impl Shared {
    pub(crate) fn add(&self, window: u64, probe: u64, logical: (u32, u32)) {
        self.windows.lock().unwrap().insert(window, (probe, logical));
    }

    fn set_logical(&self, window: u64, logical: (u32, u32)) {
        if let Some(w) = self.windows.lock().unwrap().get_mut(&window) {
            w.1 = logical;
        }
    }

    fn ids(&self) -> Vec<u64> {
        self.windows.lock().unwrap().keys().copied().collect()
    }

    fn remove(&self, window: u64) {
        self.windows.lock().unwrap().remove(&window);
    }

    /// The canvas's size in pixels.
    pub(crate) fn pixel_size(&self, window: u64) -> (u32, u32) {
        let entry = self.windows.lock().unwrap().get(&window).copied();
        match entry {
            Some((probe, logical)) => g::surface_pixel_size(probe).unwrap_or(logical),
            None => (1, 1),
        }
    }

    /// Pixels per logical pixel.
    pub(crate) fn scale(&self, window: u64) -> f64 {
        let entry = self.windows.lock().unwrap().get(&window).copied();
        match entry {
            Some((probe, (lw, _))) => match g::surface_pixel_size(probe) {
                Ok((pw, _)) if lw > 0 => (pw as f64 / lw as f64).max(1.0),
                _ => 1.0,
            },
            None => 1.0,
        }
    }
}

/// Krate's portable key names ("a", "Space", "ArrowUp", "F1") as winit keys.
fn convert_key(name: &str) -> (PhysicalKey, Key, Option<SmolStr>) {
    let named = |code: KeyCode, key: NamedKey, text: Option<&str>| {
        (PhysicalKey::Code(code), Key::Named(key), text.map(SmolStr::new))
    };
    match name {
        "Enter" => return named(KeyCode::Enter, NamedKey::Enter, Some("\r")),
        "Tab" => return named(KeyCode::Tab, NamedKey::Tab, Some("\t")),
        "Space" => return named(KeyCode::Space, NamedKey::Space, Some(" ")),
        "Backspace" => return named(KeyCode::Backspace, NamedKey::Backspace, None),
        "Escape" => return named(KeyCode::Escape, NamedKey::Escape, None),
        "Home" => return named(KeyCode::Home, NamedKey::Home, None),
        "End" => return named(KeyCode::End, NamedKey::End, None),
        "PageUp" => return named(KeyCode::PageUp, NamedKey::PageUp, None),
        "PageDown" => return named(KeyCode::PageDown, NamedKey::PageDown, None),
        "Delete" => return named(KeyCode::Delete, NamedKey::Delete, None),
        "ArrowLeft" => return named(KeyCode::ArrowLeft, NamedKey::ArrowLeft, None),
        "ArrowRight" => return named(KeyCode::ArrowRight, NamedKey::ArrowRight, None),
        "ArrowUp" => return named(KeyCode::ArrowUp, NamedKey::ArrowUp, None),
        "ArrowDown" => return named(KeyCode::ArrowDown, NamedKey::ArrowDown, None),
        _ => {},
    }
    if let Some(n) = name.strip_prefix('F').and_then(|n| n.parse::<u8>().ok()) {
        let pairs = [
            (KeyCode::F1, NamedKey::F1),
            (KeyCode::F2, NamedKey::F2),
            (KeyCode::F3, NamedKey::F3),
            (KeyCode::F4, NamedKey::F4),
            (KeyCode::F5, NamedKey::F5),
            (KeyCode::F6, NamedKey::F6),
            (KeyCode::F7, NamedKey::F7),
            (KeyCode::F8, NamedKey::F8),
            (KeyCode::F9, NamedKey::F9),
            (KeyCode::F10, NamedKey::F10),
            (KeyCode::F11, NamedKey::F11),
            (KeyCode::F12, NamedKey::F12),
        ];
        if (1..=12).contains(&n) {
            let (c, k) = pairs[n as usize - 1];
            return named(c, k, None);
        }
    }
    let mut chars = name.chars();
    let (Some(ch), None) = (chars.next(), chars.next()) else {
        return (
            PhysicalKey::Unidentified(NativeKeyCode::Unidentified),
            Key::Unidentified(NativeKey::Unidentified),
            None,
        );
    };
    let code = match ch.to_ascii_lowercase() {
        'a' => Some(KeyCode::KeyA),
        'b' => Some(KeyCode::KeyB),
        'c' => Some(KeyCode::KeyC),
        'd' => Some(KeyCode::KeyD),
        'e' => Some(KeyCode::KeyE),
        'f' => Some(KeyCode::KeyF),
        'g' => Some(KeyCode::KeyG),
        'h' => Some(KeyCode::KeyH),
        'i' => Some(KeyCode::KeyI),
        'j' => Some(KeyCode::KeyJ),
        'k' => Some(KeyCode::KeyK),
        'l' => Some(KeyCode::KeyL),
        'm' => Some(KeyCode::KeyM),
        'n' => Some(KeyCode::KeyN),
        'o' => Some(KeyCode::KeyO),
        'p' => Some(KeyCode::KeyP),
        'q' => Some(KeyCode::KeyQ),
        'r' => Some(KeyCode::KeyR),
        's' => Some(KeyCode::KeyS),
        't' => Some(KeyCode::KeyT),
        'u' => Some(KeyCode::KeyU),
        'v' => Some(KeyCode::KeyV),
        'w' => Some(KeyCode::KeyW),
        'x' => Some(KeyCode::KeyX),
        'y' => Some(KeyCode::KeyY),
        'z' => Some(KeyCode::KeyZ),
        '0' | ')' => Some(KeyCode::Digit0),
        '1' | '!' => Some(KeyCode::Digit1),
        '2' | '@' => Some(KeyCode::Digit2),
        '3' | '#' => Some(KeyCode::Digit3),
        '4' | '$' => Some(KeyCode::Digit4),
        '5' | '%' => Some(KeyCode::Digit5),
        '6' | '^' => Some(KeyCode::Digit6),
        '7' | '&' => Some(KeyCode::Digit7),
        '8' | '*' => Some(KeyCode::Digit8),
        '9' | '(' => Some(KeyCode::Digit9),
        '-' | '_' => Some(KeyCode::Minus),
        '=' | '+' => Some(KeyCode::Equal),
        '[' | '{' => Some(KeyCode::BracketLeft),
        ']' | '}' => Some(KeyCode::BracketRight),
        '\\' | '|' => Some(KeyCode::Backslash),
        ';' | ':' => Some(KeyCode::Semicolon),
        '\'' | '"' => Some(KeyCode::Quote),
        ',' | '<' => Some(KeyCode::Comma),
        '.' | '>' => Some(KeyCode::Period),
        '/' | '?' => Some(KeyCode::Slash),
        '`' | '~' => Some(KeyCode::Backquote),
        _ => None,
    };
    let physical = match code {
        Some(c) => PhysicalKey::Code(c),
        None => PhysicalKey::Unidentified(NativeKeyCode::Unidentified),
    };
    let text = SmolStr::new(name);
    (physical, Key::Character(text.clone()), Some(text))
}

fn element_state(pressed: bool) -> event::ElementState {
    if pressed {
        event::ElementState::Pressed
    } else {
        event::ElementState::Released
    }
}

/// The modifier keys, worked out from the modifier state Krate attaches to
/// every key and pointer event (Krate sends no event for a modifier alone).
#[derive(Default, Clone, Copy, PartialEq)]
struct Mods {
    shift: bool,
    control: bool,
    alt: bool,
    meta: bool,
}

impl Mods {
    fn of(m: &kt::Modifiers) -> Mods {
        Mods { shift: m.shift, control: m.control, alt: m.alt, meta: m.meta }
    }

    fn winit(self) -> Modifiers {
        let mut state = ModifiersState::empty();
        let mut keys = ModifiersKeys::empty();
        state.set(ModifiersState::SHIFT, self.shift);
        keys.set(ModifiersKeys::LSHIFT, self.shift);
        state.set(ModifiersState::CONTROL, self.control);
        keys.set(ModifiersKeys::LCONTROL, self.control);
        state.set(ModifiersState::ALT, self.alt);
        keys.set(ModifiersKeys::LALT, self.alt);
        state.set(ModifiersState::SUPER, self.meta);
        keys.set(ModifiersKeys::LSUPER, self.meta);
        Modifiers { state, pressed_mods: keys }
    }
}

struct EventState<T> {
    /// User events taken while waiting, handed over on the next turn.
    user: VecDeque<T>,
    mods: Mods,
    /// Last cursor position per window, logical.
    cursor: HashMap<u64, (f32, f32)>,
    /// Last pixel size per window, to see the canvas resize.
    size: HashMap<u64, (u32, u32)>,
    /// An event `wait` returned, handled on the next turn.
    pending: VecDeque<kt::Event>,
}

impl<T> Default for EventState<T> {
    fn default() -> Self {
        Self {
            user: VecDeque::new(),
            mods: Mods::default(),
            cursor: HashMap::new(),
            size: HashMap::new(),
            pending: VecDeque::new(),
        }
    }
}

pub struct EventLoop<T: 'static> {
    window_target: event_loop::ActiveEventLoop,
    user_events_sender: mpsc::Sender<T>,
    user_events_receiver: mpsc::Receiver<T>,
    state: EventState<T>,
}

impl<T: 'static> EventLoop<T> {
    pub(crate) fn new(_: &PlatformSpecificEventLoopAttributes) -> Result<Self, EventLoopError> {
        let (user_events_sender, user_events_receiver) = mpsc::channel();
        Ok(Self {
            window_target: event_loop::ActiveEventLoop {
                p: ActiveEventLoop {
                    control_flow: Cell::new(ControlFlow::default()),
                    exit: Cell::new(false),
                    creates: Mutex::new(VecDeque::new()),
                    redraws: Arc::new(Mutex::new(VecDeque::new())),
                    destroys: Arc::new(Mutex::new(VecDeque::new())),
                    shared: Arc::new(Shared::default()),
                },
                _marker: PhantomData,
            },
            user_events_sender,
            user_events_receiver,
            state: EventState::default(),
        })
    }

    fn modifiers<F>(state: &mut EventState<T>, window: u64, now: Mods, handler: &mut F)
    where
        F: FnMut(event::Event<T>),
    {
        let before = state.mods;
        if before == now {
            return;
        }
        let keys = [
            (before.shift, now.shift, KeyCode::ShiftLeft, NamedKey::Shift),
            (before.control, now.control, KeyCode::ControlLeft, NamedKey::Control),
            (before.alt, now.alt, KeyCode::AltLeft, NamedKey::Alt),
            (before.meta, now.meta, KeyCode::SuperLeft, NamedKey::Super),
        ];
        for (was, is, code, named) in keys {
            if was != is {
                handler(event::Event::WindowEvent {
                    window_id: RootWindowId(WindowId { fd: window }),
                    event: event::WindowEvent::KeyboardInput {
                        device_id: event::DeviceId(DeviceId),
                        event: event::KeyEvent {
                            physical_key: PhysicalKey::Code(code),
                            logical_key: Key::Named(named),
                            text: None,
                            location: KeyLocation::Left,
                            state: element_state(is),
                            repeat: false,
                            platform_specific: KeyEventExtra {
                                key_without_modifiers: Key::Named(named),
                                text_with_all_modifiers: None,
                            },
                        },
                        is_synthetic: false,
                    },
                });
            }
        }
        state.mods = now;
        handler(event::Event::WindowEvent {
            window_id: RootWindowId(WindowId { fd: window }),
            event: event::WindowEvent::ModifiersChanged(now.winit()),
        });
    }

    fn process_event<F>(shared: &Shared, ev: kt::Event, state: &mut EventState<T>, mut handler: F)
    where
        F: FnMut(event::Event<T>),
    {
        let wid = |w: u64| RootWindowId(WindowId { fd: w });
        match ev {
            kt::Event::CloseRequested(w) => handler(event::Event::WindowEvent {
                window_id: wid(w),
                event: event::WindowEvent::CloseRequested,
            }),
            kt::Event::RedrawRequested(w) => handler(event::Event::WindowEvent {
                window_id: wid(w),
                event: event::WindowEvent::RedrawRequested,
            }),
            kt::Event::Resized(size) => {
                // Krate reports the logical size; the pixel size is read from
                // the canvas each turn and sent as `Resized` when it moves.
                if let Some(w) = shared.ids().first().copied() {
                    shared.set_logical(w, (size.width, size.height));
                }
            },
            kt::Event::Key(k) => {
                Self::modifiers(state, k.window, Mods::of(&k.modifiers), &mut handler);
                let (physical_key, logical_key, text) = convert_key(&k.key);
                if matches!(physical_key, PhysicalKey::Unidentified(_))
                    && matches!(logical_key, Key::Unidentified(_))
                {
                    return;
                }
                let without = match &logical_key {
                    Key::Character(c) => Key::Character(SmolStr::new(c.to_lowercase())),
                    other => other.clone(),
                };
                handler(event::Event::WindowEvent {
                    window_id: wid(k.window),
                    event: event::WindowEvent::KeyboardInput {
                        device_id: event::DeviceId(DeviceId),
                        event: event::KeyEvent {
                            physical_key,
                            logical_key,
                            text: if k.pressed { text.clone() } else { None },
                            location: KeyLocation::Standard,
                            state: element_state(k.pressed),
                            repeat: false,
                            platform_specific: KeyEventExtra {
                                key_without_modifiers: without,
                                text_with_all_modifiers: if k.pressed { text } else { None },
                            },
                        },
                        is_synthetic: false,
                    },
                });
            },
            kt::Event::Pointer(p) => {
                Self::modifiers(state, p.window, Mods::of(&p.modifiers), &mut handler);
                let scale = shared.scale(p.window);
                if state.cursor.get(&p.window) != Some(&(p.x, p.y)) {
                    state.cursor.insert(p.window, (p.x, p.y));
                    handler(event::Event::WindowEvent {
                        window_id: wid(p.window),
                        event: event::WindowEvent::CursorMoved {
                            device_id: event::DeviceId(DeviceId),
                            position: (p.x as f64 * scale, p.y as f64 * scale).into(),
                        },
                    });
                }
                if let Some(button) = p.button {
                    let button = match button {
                        kt::PointerButton::Primary => event::MouseButton::Left,
                        kt::PointerButton::Secondary => event::MouseButton::Right,
                        kt::PointerButton::Middle => event::MouseButton::Middle,
                        kt::PointerButton::Other => event::MouseButton::Other(3),
                    };
                    handler(event::Event::WindowEvent {
                        window_id: wid(p.window),
                        event: event::WindowEvent::MouseInput {
                            device_id: event::DeviceId(DeviceId),
                            state: element_state(p.pressed),
                            button,
                        },
                    });
                }
            },
            kt::Event::Wheel(wh) => {
                Self::modifiers(state, wh.window, Mods::of(&wh.modifiers), &mut handler);
                let scale = shared.scale(wh.window);
                // Krate's positive dy moves further into the content; winit's
                // positive y is the wheel turned away from the person.
                handler(event::Event::WindowEvent {
                    window_id: wid(wh.window),
                    event: event::WindowEvent::MouseWheel {
                        device_id: event::DeviceId(DeviceId),
                        delta: event::MouseScrollDelta::PixelDelta(
                            (-(wh.dx as f64) * scale, -(wh.dy as f64) * scale).into(),
                        ),
                        phase: event::TouchPhase::Moved,
                    },
                });
            },
            _ => {},
        }
    }

    pub fn run<F>(mut self, mut event_handler_inner: F) -> Result<(), EventLoopError>
    where
        F: FnMut(event::Event<T>, &event_loop::ActiveEventLoop),
    {
        let mut start_cause = StartCause::Init;
        let shared = self.window_target.p.shared.clone();
        loop {
            event_handler_inner(event::Event::NewEvents(start_cause), &self.window_target);
            if start_cause == StartCause::Init {
                event_handler_inner(event::Event::Resumed, &self.window_target);
            }

            // New windows: their first size, focus.
            while let Some(window_id) = self.window_target.p.creates.lock().unwrap().pop_front() {
                let (w, h) = shared.pixel_size(window_id.fd);
                self.state.size.insert(window_id.fd, (w, h));
                event_handler_inner(
                    event::Event::WindowEvent {
                        window_id: RootWindowId(window_id),
                        event: event::WindowEvent::Resized((w, h).into()),
                    },
                    &self.window_target,
                );
                event_handler_inner(
                    event::Event::WindowEvent {
                        window_id: RootWindowId(window_id),
                        event: event::WindowEvent::Focused(true),
                    },
                    &self.window_target,
                );
            }

            while let Some(destroy_id) = self.window_target.p.destroys.lock().unwrap().pop_front() {
                shared.remove(destroy_id.fd);
                self.state.size.remove(&destroy_id.fd);
                event_handler_inner(
                    event::Event::WindowEvent {
                        window_id: RootWindowId(destroy_id),
                        event: event::WindowEvent::Destroyed,
                    },
                    &self.window_target,
                );
            }

            // Krate's events.
            loop {
                let next = self.state.pending.pop_front().or_else(events::poll);
                let Some(ev) = next else { break };
                let target = &self.window_target;
                Self::process_event(&shared, ev, &mut self.state, |e| event_handler_inner(e, target));
            }

            // Each window: a canvas that changed size, and the held pointer's
            // raw motion.
            for window in shared.ids() {
                let now = shared.pixel_size(window);
                if self.state.size.get(&window) != Some(&now) {
                    let old_scale = self.state.size.get(&window).copied();
                    self.state.size.insert(window, now);
                    if old_scale.is_some() {
                        event_handler_inner(
                            event::Event::WindowEvent {
                                window_id: RootWindowId(WindowId { fd: window }),
                                event: event::WindowEvent::Resized(now.into()),
                            },
                            &self.window_target,
                        );
                    }
                }
                if pointer::captured(window) {
                    let (dx, dy) = pointer::take_motion(window);
                    if dx != 0.0 || dy != 0.0 {
                        event_handler_inner(
                            event::Event::DeviceEvent {
                                device_id: event::DeviceId(DeviceId),
                                event: event::DeviceEvent::MouseMotion {
                                    delta: (dx as f64, dy as f64),
                                },
                            },
                            &self.window_target,
                        );
                    }
                }
            }

            while let Some(event) =
                self.state.user.pop_front().or_else(|| self.user_events_receiver.try_recv().ok())
            {
                event_handler_inner(event::Event::UserEvent(event), &self.window_target);
            }

            loop {
                let next = self.window_target.p.redraws.lock().unwrap().pop_front();
                let Some(window_id) = next else { break };
                event_handler_inner(
                    event::Event::WindowEvent {
                        window_id: RootWindowId(window_id),
                        event: event::WindowEvent::RedrawRequested,
                    },
                    &self.window_target,
                );
            }

            event_handler_inner(event::Event::AboutToWait, &self.window_target);

            if self.window_target.p.exiting() {
                break;
            }

            let requested_resume = match self.window_target.p.control_flow() {
                ControlFlow::Poll => {
                    start_cause = StartCause::Poll;
                    continue;
                },
                ControlFlow::Wait => None,
                ControlFlow::WaitUntil(instant) => Some(instant),
            };
            // Wait in short steps: an event from another thread (a proxy)
            // cannot wake Krate's wait, so the queue is looked at every
            // 16 ms. A step that ends with nothing to report keeps waiting;
            // the app hears only of a real event or its own deadline.
            let start = Instant::now();
            if !self.window_target.p.redraws.lock().unwrap().is_empty() {
                start_cause = StartCause::Poll;
                continue;
            }
            start_cause = loop {
                let step = requested_resume
                    .map(|r| r.saturating_duration_since(Instant::now()).as_millis().min(16) as u32)
                    .unwrap_or(16);
                if let Some(ev) = events::wait(Some(step)) {
                    self.state.pending.push_back(ev);
                    break StartCause::WaitCancelled { start, requested_resume };
                }
                if let Ok(user) = self.user_events_receiver.try_recv() {
                    self.state.user.push_back(user);
                    break StartCause::WaitCancelled { start, requested_resume };
                }
                if let Some(r) = requested_resume {
                    if Instant::now() >= r {
                        break StartCause::ResumeTimeReached { start, requested_resume: r };
                    }
                }
                if !self.window_target.p.redraws.lock().unwrap().is_empty() {
                    break StartCause::WaitCancelled { start, requested_resume };
                }
            };
        }

        event_handler_inner(event::Event::LoopExiting, &self.window_target);
        Ok(())
    }

    pub fn window_target(&self) -> &event_loop::ActiveEventLoop {
        &self.window_target
    }

    pub fn create_proxy(&self) -> EventLoopProxy<T> {
        EventLoopProxy { user_events_sender: self.user_events_sender.clone() }
    }
}

pub struct EventLoopProxy<T: 'static> {
    user_events_sender: mpsc::Sender<T>,
}

impl<T> EventLoopProxy<T> {
    pub fn send_event(&self, event: T) -> Result<(), event_loop::EventLoopClosed<T>> {
        self.user_events_sender
            .send(event)
            .map_err(|mpsc::SendError(x)| event_loop::EventLoopClosed(x))
    }
}

impl<T> Clone for EventLoopProxy<T> {
    fn clone(&self) -> Self {
        Self { user_events_sender: self.user_events_sender.clone() }
    }
}

impl<T> Unpin for EventLoopProxy<T> {}

pub struct ActiveEventLoop {
    control_flow: Cell<ControlFlow>,
    exit: Cell<bool>,
    pub(super) creates: Mutex<VecDeque<WindowId>>,
    pub(super) redraws: Arc<Mutex<VecDeque<WindowId>>>,
    pub(super) destroys: Arc<Mutex<VecDeque<WindowId>>>,
    pub(super) shared: Arc<Shared>,
}

impl ActiveEventLoop {
    pub fn create_custom_cursor(&self, source: CustomCursorSource) -> RootCustomCursor {
        let _ = source.inner;
        RootCustomCursor { inner: super::PlatformCustomCursor }
    }

    pub fn primary_monitor(&self) -> Option<MonitorHandle> {
        Some(MonitorHandle)
    }

    pub fn available_monitors(&self) -> VecDeque<MonitorHandle> {
        let mut v = VecDeque::with_capacity(1);
        v.push_back(MonitorHandle);
        v
    }

    #[inline]
    pub fn listen_device_events(&self, _allowed: DeviceEvents) {}

    #[cfg(feature = "rwh_05")]
    #[inline]
    pub fn raw_display_handle_rwh_05(&self) -> rwh_05::RawDisplayHandle {
        rwh_05::RawDisplayHandle::Web(rwh_05::WebDisplayHandle::empty())
    }

    #[inline]
    pub fn system_theme(&self) -> Option<Theme> {
        None
    }

    #[cfg(feature = "rwh_06")]
    #[inline]
    pub fn raw_display_handle_rwh_06(
        &self,
    ) -> Result<rwh_06::RawDisplayHandle, rwh_06::HandleError> {
        Ok(rwh_06::RawDisplayHandle::Web(rwh_06::WebDisplayHandle::new()))
    }

    pub fn set_control_flow(&self, control_flow: ControlFlow) {
        self.control_flow.set(control_flow)
    }

    pub fn control_flow(&self) -> ControlFlow {
        self.control_flow.get()
    }

    pub(crate) fn exit(&self) {
        self.exit.set(true);
    }

    pub(crate) fn exiting(&self) -> bool {
        self.exit.get()
    }

    pub(crate) fn owned_display_handle(&self) -> OwnedDisplayHandle {
        OwnedDisplayHandle
    }
}

#[derive(Clone)]
pub(crate) struct OwnedDisplayHandle;

impl OwnedDisplayHandle {
    #[cfg(feature = "rwh_05")]
    #[inline]
    pub fn raw_display_handle_rwh_05(&self) -> rwh_05::RawDisplayHandle {
        rwh_05::WebDisplayHandle::empty().into()
    }

    #[cfg(feature = "rwh_06")]
    #[inline]
    pub fn raw_display_handle_rwh_06(
        &self,
    ) -> Result<rwh_06::RawDisplayHandle, rwh_06::HandleError> {
        Ok(rwh_06::WebDisplayHandle::new().into())
    }
}
