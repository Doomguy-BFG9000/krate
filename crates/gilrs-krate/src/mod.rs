//! gilrs-core's backend for Krate apps (IC-936): the person's gamepad through
//! krate:ui/events' gamepad calls.
//!
//! Krate reports state ("is south held", "where is left-y"); gilrs wants
//! events. Each poll asks for the state of every documented button and axis
//! and emits what changed, as a native backend's driver would.
//!
//! One pad, the first connected (krate:ui has no player index yet). No force
//! feedback (krate:ui has no rumble yet): `is_ff_supported` says so.

mod ff;
mod gamepad;

pub use self::ff::Device as FfDevice;
pub use self::gamepad::{native_ev_codes, EvCode, Gamepad, Gilrs};

// Krate reports up as positive, which is gilrs' own convention.
pub const IS_Y_AXIS_REVERSED: bool = false;
