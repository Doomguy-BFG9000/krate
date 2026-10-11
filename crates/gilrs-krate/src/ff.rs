use std::time::Duration;

/// A gamepad's force feedback. krate:ui has no rumble yet: nothing happens.
#[derive(Debug)]
pub struct Device;

impl Device {
    pub fn set_ff_state(&mut self, _strong: u16, _weak: u16, _min_duration: Duration) {}
}
