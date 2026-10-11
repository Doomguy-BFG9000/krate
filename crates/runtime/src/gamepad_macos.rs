//! The Mac's own controller path: Apple's Game Controller framework (IC-936).
//!
//! gilrs reads controllers through IOKit HID on macOS, which misses the pads
//! macOS itself drives (Xbox Series and other GCController-only pads). The
//! Game Controller framework sees every controller the Mac supports (MFi,
//! Xbox, PlayStation, Switch), already laid out as Apple's "extended gamepad":
//! buttons by position, sticks with up and right positive, triggers 0 to 1 --
//! the same contract as krate:ui's gamepad calls, so this is a reading, not a
//! remapping.
//!
//! Only `+[GCController controllers]` and property reads: no delegates, no
//! notifications, nothing that has to run on a particular thread.

use std::collections::BTreeMap;

use objc2::msg_send;
use objc2::runtime::{AnyClass, AnyObject};

#[link(name = "GameController", kind = "framework")]
unsafe extern "C" {}

/// The first controller with an extended gamepad, read now: `None` when the
/// Mac has none.
pub(crate) fn read() -> Option<(BTreeMap<String, bool>, BTreeMap<String, f32>)> {
    let class = AnyClass::get(c"GCController")?;
    // SAFETY: documented class methods and properties of GameController;
    // every object is checked for nil before use and none is retained past
    // this function.
    unsafe {
        let list: *mut AnyObject = msg_send![class, controllers];
        if list.is_null() {
            return None;
        }
        let count: usize = msg_send![list, count];
        for i in 0..count {
            let controller: *mut AnyObject = msg_send![list, objectAtIndex: i];
            if controller.is_null() {
                continue;
            }
            let pad: *mut AnyObject = msg_send![controller, extendedGamepad];
            if pad.is_null() {
                continue;
            }
            return Some(read_pad(pad));
        }
        None
    }
}

unsafe fn pressed(pad: *mut AnyObject, sel: &str) -> bool {
    let input: *mut AnyObject = unsafe { property(pad, sel) };
    if input.is_null() {
        return false;
    }
    unsafe { msg_send![input, isPressed] }
}

unsafe fn value(input: *mut AnyObject) -> f32 {
    if input.is_null() {
        return 0.0;
    }
    unsafe { msg_send![input, value] }
}

unsafe fn property(object: *mut AnyObject, name: &str) -> *mut AnyObject {
    match name {
        "buttonA" => unsafe { msg_send![object, buttonA] },
        "buttonB" => unsafe { msg_send![object, buttonB] },
        "buttonX" => unsafe { msg_send![object, buttonX] },
        "buttonY" => unsafe { msg_send![object, buttonY] },
        "leftShoulder" => unsafe { msg_send![object, leftShoulder] },
        "rightShoulder" => unsafe { msg_send![object, rightShoulder] },
        "leftTrigger" => unsafe { msg_send![object, leftTrigger] },
        "rightTrigger" => unsafe { msg_send![object, rightTrigger] },
        "buttonMenu" => unsafe { msg_send![object, buttonMenu] },
        "buttonOptions" => unsafe { msg_send![object, buttonOptions] },
        "dpad" => unsafe { msg_send![object, dpad] },
        "up" => unsafe { msg_send![object, up] },
        "down" => unsafe { msg_send![object, down] },
        "left" => unsafe { msg_send![object, left] },
        "right" => unsafe { msg_send![object, right] },
        "leftThumbstick" => unsafe { msg_send![object, leftThumbstick] },
        "rightThumbstick" => unsafe { msg_send![object, rightThumbstick] },
        "xAxis" => unsafe { msg_send![object, xAxis] },
        "yAxis" => unsafe { msg_send![object, yAxis] },
        _ => std::ptr::null_mut(),
    }
}

/// krate:ui's names for the extended gamepad's elements.
const BUTTONS: [(&str, &str); 10] = [
    ("south", "buttonA"),
    ("east", "buttonB"),
    ("west", "buttonX"),
    ("north", "buttonY"),
    ("l1", "leftShoulder"),
    ("r1", "rightShoulder"),
    ("l2", "leftTrigger"),
    ("r2", "rightTrigger"),
    ("start", "buttonMenu"),
    ("select", "buttonOptions"),
];

unsafe fn read_pad(pad: *mut AnyObject) -> (BTreeMap<String, bool>, BTreeMap<String, f32>) {
    let mut held = BTreeMap::new();
    let mut axes = BTreeMap::new();
    for (name, sel) in BUTTONS {
        held.insert(name.to_string(), unsafe { pressed(pad, sel) });
    }
    let dpad = unsafe { property(pad, "dpad") };
    if !dpad.is_null() {
        for (name, sel) in [
            ("dpad-up", "up"),
            ("dpad-down", "down"),
            ("dpad-left", "left"),
            ("dpad-right", "right"),
        ] {
            held.insert(name.to_string(), unsafe { pressed(dpad, sel) });
        }
    }
    for (stick, x, y) in [
        ("leftThumbstick", "left-x", "left-y"),
        ("rightThumbstick", "right-x", "right-y"),
    ] {
        let s = unsafe { property(pad, stick) };
        if !s.is_null() {
            axes.insert(x.to_string(), unsafe { value(property(s, "xAxis")) });
            axes.insert(y.to_string(), unsafe { value(property(s, "yAxis")) });
        }
    }
    axes.insert("l2".to_string(), unsafe {
        value(property(pad, "leftTrigger"))
    });
    axes.insert("r2".to_string(), unsafe {
        value(property(pad, "rightTrigger"))
    });
    (held, axes)
}
