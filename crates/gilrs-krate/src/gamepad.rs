use std::collections::VecDeque;
use std::fmt::{Display, Formatter, Result as FmtResult};
use std::time::Duration;

use krate::bindings::krate::ui::events as k;
use uuid::Uuid;

use super::FfDevice;
use crate::{AxisInfo, Event, EventType, PlatformError, PowerInfo};

/// krate:ui's button names, with the gilrs codes they stand for.
const BUTTONS: [(&str, EvCode); 14] = [
    ("south", native_ev_codes::BTN_SOUTH),
    ("east", native_ev_codes::BTN_EAST),
    ("west", native_ev_codes::BTN_WEST),
    ("north", native_ev_codes::BTN_NORTH),
    ("l1", native_ev_codes::BTN_LT),
    ("r1", native_ev_codes::BTN_RT),
    ("l2", native_ev_codes::BTN_LT2),
    ("r2", native_ev_codes::BTN_RT2),
    ("start", native_ev_codes::BTN_START),
    ("select", native_ev_codes::BTN_SELECT),
    ("dpad-up", native_ev_codes::BTN_DPAD_UP),
    ("dpad-down", native_ev_codes::BTN_DPAD_DOWN),
    ("dpad-left", native_ev_codes::BTN_DPAD_LEFT),
    ("dpad-right", native_ev_codes::BTN_DPAD_RIGHT),
];

/// krate:ui's axis names (sticks -1..1, triggers 0..1), with their codes.
const AXES: [(&str, EvCode); 6] = [
    ("left-x", native_ev_codes::AXIS_LSTICKX),
    ("left-y", native_ev_codes::AXIS_LSTICKY),
    ("right-x", native_ev_codes::AXIS_RSTICKX),
    ("right-y", native_ev_codes::AXIS_RSTICKY),
    ("l2", native_ev_codes::AXIS_LT2),
    ("r2", native_ev_codes::AXIS_RT2),
];

const STICK: AxisInfo = AxisInfo {
    min: -32768,
    max: 32767,
    deadzone: None,
};
const TRIGGER: AxisInfo = AxisInfo {
    min: 0,
    max: 32767,
    deadzone: None,
};

fn axis_value(name: &str, v: f32) -> i32 {
    if name == "l2" || name == "r2" {
        (v.clamp(0.0, 1.0) * 32767.0) as i32
    } else {
        (v.clamp(-1.0, 1.0) * 32767.0) as i32
    }
}

#[derive(Debug)]
pub struct Gilrs {
    pad: Gamepad,
    queue: VecDeque<Event>,
}

impl Gilrs {
    pub(crate) fn new() -> Result<Self, PlatformError> {
        let mut gilrs = Gilrs {
            pad: Gamepad {
                connected: false,
                held: [false; 14],
                axes: [0; 6],
                buttons: BUTTONS.iter().map(|(_, c)| *c).collect(),
                axis_codes: AXES.iter().map(|(_, c)| *c).collect(),
            },
            queue: VecDeque::new(),
        };
        gilrs.poll();
        Ok(gilrs)
    }

    /// Ask Krate for the pad's state and queue what changed.
    fn poll(&mut self) {
        let connected = k::gamepad_connected();
        if connected != self.pad.connected {
            self.pad.connected = connected;
            self.queue.push_back(Event::new(
                0,
                if connected {
                    EventType::Connected
                } else {
                    EventType::Disconnected
                },
            ));
        }
        if !connected {
            return;
        }
        for (i, (name, code)) in BUTTONS.iter().enumerate() {
            let down = k::gamepad_held(name);
            if down != self.pad.held[i] {
                self.pad.held[i] = down;
                self.queue.push_back(Event::new(
                    0,
                    if down {
                        EventType::ButtonPressed(crate::EvCode(*code))
                    } else {
                        EventType::ButtonReleased(crate::EvCode(*code))
                    },
                ));
            }
        }
        for (i, (name, code)) in AXES.iter().enumerate() {
            let v = axis_value(name, k::gamepad_axis(name));
            if v != self.pad.axes[i] {
                self.pad.axes[i] = v;
                self.queue.push_back(Event::new(
                    0,
                    EventType::AxisValueChanged(v, crate::EvCode(*code)),
                ));
            }
        }
    }

    pub(crate) fn next_event(&mut self) -> Option<Event> {
        if self.queue.is_empty() {
            self.poll();
        }
        self.queue.pop_front()
    }

    pub(crate) fn next_event_blocking(&mut self, timeout: Option<Duration>) -> Option<Event> {
        let start = std::time::Instant::now();
        loop {
            if let Some(e) = self.next_event() {
                return Some(e);
            }
            if timeout.is_some_and(|t| start.elapsed() >= t) {
                return None;
            }
            std::thread::sleep(Duration::from_millis(4));
        }
    }

    pub fn gamepad(&self, id: usize) -> Option<&Gamepad> {
        (id == 0).then_some(&self.pad)
    }

    /// Returns index greater than index of last connected gamepad.
    pub fn last_gamepad_hint(&self) -> usize {
        1
    }
}

#[derive(Debug)]
pub struct Gamepad {
    connected: bool,
    held: [bool; 14],
    axes: [i32; 6],
    buttons: Vec<EvCode>,
    axis_codes: Vec<EvCode>,
}

impl Gamepad {
    pub fn name(&self) -> &str {
        "Krate gamepad"
    }

    /// Nil: gilrs then maps by these codes, which already are its own.
    pub fn uuid(&self) -> Uuid {
        Uuid::nil()
    }

    pub fn vendor_id(&self) -> Option<u16> {
        None
    }

    pub fn product_id(&self) -> Option<u16> {
        None
    }

    pub fn power_info(&self) -> PowerInfo {
        PowerInfo::Unknown
    }

    pub fn is_ff_supported(&self) -> bool {
        false
    }

    pub fn ff_device(&self) -> Option<FfDevice> {
        Some(FfDevice)
    }

    pub fn buttons(&self) -> &[EvCode] {
        &self.buttons
    }

    pub fn axes(&self) -> &[EvCode] {
        &self.axis_codes
    }

    pub(crate) fn axis_info(&self, nec: EvCode) -> Option<&AxisInfo> {
        if nec == native_ev_codes::AXIS_LT2 || nec == native_ev_codes::AXIS_RT2 {
            Some(&TRIGGER)
        } else if self.axis_codes.contains(&nec) {
            Some(&STICK)
        } else {
            None
        }
    }

    pub fn is_connected(&self) -> bool {
        self.connected
    }
}

#[cfg(feature = "serde-serialize")]
use serde::{Deserialize, Serialize};

#[cfg_attr(feature = "serde-serialize", derive(Serialize, Deserialize))]
#[derive(Copy, Clone, Debug, Eq, PartialEq, Ord, PartialOrd, Hash)]
pub struct EvCode(u16);

impl EvCode {
    pub fn into_u32(self) -> u32 {
        self.0 as u32
    }
}

impl Display for EvCode {
    fn fmt(&self, f: &mut Formatter) -> FmtResult {
        self.0.fmt(f)
    }
}

/// The same codes gilrs' default backend uses, so its default mapping applies.
pub mod native_ev_codes {
    use super::EvCode;

    pub const AXIS_LSTICKX: EvCode = EvCode(0);
    pub const AXIS_LSTICKY: EvCode = EvCode(1);
    pub const AXIS_LEFTZ: EvCode = EvCode(2);
    pub const AXIS_RSTICKX: EvCode = EvCode(3);
    pub const AXIS_RSTICKY: EvCode = EvCode(4);
    pub const AXIS_RIGHTZ: EvCode = EvCode(5);
    pub const AXIS_DPADX: EvCode = EvCode(6);
    pub const AXIS_DPADY: EvCode = EvCode(7);
    pub const AXIS_RT: EvCode = EvCode(8);
    pub const AXIS_LT: EvCode = EvCode(9);
    pub const AXIS_RT2: EvCode = EvCode(10);
    pub const AXIS_LT2: EvCode = EvCode(11);

    pub const BTN_SOUTH: EvCode = EvCode(12);
    pub const BTN_EAST: EvCode = EvCode(13);
    pub const BTN_C: EvCode = EvCode(14);
    pub const BTN_NORTH: EvCode = EvCode(15);
    pub const BTN_WEST: EvCode = EvCode(16);
    pub const BTN_Z: EvCode = EvCode(17);
    pub const BTN_LT: EvCode = EvCode(18);
    pub const BTN_RT: EvCode = EvCode(19);
    pub const BTN_LT2: EvCode = EvCode(20);
    pub const BTN_RT2: EvCode = EvCode(21);
    pub const BTN_SELECT: EvCode = EvCode(22);
    pub const BTN_START: EvCode = EvCode(23);
    pub const BTN_MODE: EvCode = EvCode(24);
    pub const BTN_LTHUMB: EvCode = EvCode(25);
    pub const BTN_RTHUMB: EvCode = EvCode(26);

    pub const BTN_DPAD_UP: EvCode = EvCode(27);
    pub const BTN_DPAD_DOWN: EvCode = EvCode(28);
    pub const BTN_DPAD_LEFT: EvCode = EvCode(29);
    pub const BTN_DPAD_RIGHT: EvCode = EvCode(30);
}
