//! How the phone is held (krate:ui/motion, IC-917): SensorManager through
//! the player activity's Motion.java, turned into the screen's frame.

use krate_adapter_common::ui::{device_to_screen, heading_to_screen, MotionReading, ScreenTurn};

/// Standard gravity: Android reports metres a second squared, Krate g.
const G: f64 = 9.80665;

/// Android's Display rotation (0..3) as the way the interface is turned.
/// ROTATION_90 is the phone turned anticlockwise, its top to the left.
#[cfg_attr(not(target_os = "android"), allow(dead_code))]
pub(crate) fn turn_of(rotation: f32) -> ScreenTurn {
    match rotation as i32 {
        1 => ScreenTurn::TopLeft,
        2 => ScreenTurn::UpsideDown,
        3 => ScreenTurn::TopRight,
        _ => ScreenTurn::Upright,
    }
}

/// One Motion.java reading -- gravity sensor, linear acceleration, gyro
/// (all in the phone's frame: x right, y up, z out of the glass), azimuth
/// or -1, display rotation -- in the screen's frame. Android's gravity
/// sensor points up, away from the earth (it reads as the accelerometer
/// does at rest); Krate's gravity is which way is down, so it is turned
/// round. Linear acceleration is the push itself.
#[cfg_attr(not(target_os = "android"), allow(dead_code))]
pub(crate) fn reading_from(raw: &[f32]) -> Option<MotionReading> {
    if raw.len() < 11 {
        return None;
    }
    let turn = turn_of(raw[10]);
    let v = |i: usize, scale: f64| {
        (
            f64::from(raw[i]) * scale,
            f64::from(raw[i + 1]) * scale,
            f64::from(raw[i + 2]) * scale,
        )
    };
    let degrees = 180.0 / std::f64::consts::PI;
    Some(MotionReading {
        gravity: device_to_screen(turn, v(0, -1.0 / G)),
        acceleration: device_to_screen(turn, v(3, 1.0 / G)),
        rotation: device_to_screen(turn, v(6, degrees)),
        heading: (raw[9] >= 0.0).then(|| heading_to_screen(turn, f64::from(raw[9]))),
    })
}

#[cfg(target_os = "android")]
mod real {
    use super::*;
    use crate::java::with_activity;
    use jni::objects::JFloatArray;
    use std::sync::OnceLock;

    pub fn available() -> bool {
        static HAS: OnceLock<bool> = OnceLock::new();
        *HAS.get_or_init(|| {
            with_activity("asking for motion sensors", |env, activity| {
                env.call_method(activity, "krateMotionAvailable", "()Z", &[])?
                    .z()
            })
            .unwrap_or(false)
        })
    }

    /// The latest reading, starting the sensors on the first ask.
    pub fn read() -> Option<MotionReading> {
        let raw = with_activity("reading motion", |env, activity| {
            let array = env.call_method(activity, "krateMotion", "()[F", &[])?.l()?;
            if array.is_null() {
                return Ok(None);
            }
            let array = JFloatArray::from(array);
            let len = env.get_array_length(&array)? as usize;
            let mut out = vec![0f32; len];
            env.get_float_array_region(&array, 0, &mut out)?;
            Ok(Some(out))
        })
        .flatten()?;
        reading_from(&raw)
    }

    /// The app closed: the sensors stop, so they cost nothing.
    pub fn stop() {
        let _ = with_activity("stopping motion", |env, activity| {
            env.call_method(activity, "krateMotionStop", "()V", &[])?
                .v()
        });
    }
}

#[cfg(target_os = "android")]
pub use real::*;

#[cfg(not(target_os = "android"))]
mod stub {
    use krate_adapter_common::ui::MotionReading;

    pub fn available() -> bool {
        false
    }

    pub fn read() -> Option<MotionReading> {
        None
    }

    pub fn stop() {}
}

#[cfg(not(target_os = "android"))]
pub use stub::*;

#[cfg(test)]
mod tests {
    use super::*;

    fn raw(gravity: (f32, f32, f32), rotation: f32) -> Vec<f32> {
        vec![
            gravity.0, gravity.1, gravity.2, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0, rotation,
        ]
    }

    #[test]
    fn down_is_down_the_screen_upright_and_turned_either_way() {
        // Android's gravity sensor reads +9.8 along the axis pointing up.
        // Upright: the phone's top is up.
        let upright = reading_from(&raw((0.0, 9.80665, 0.0), 0.0)).unwrap();
        assert_eq!(upright.gravity, (0.0, 1.0, 0.0));
        // Turned anticlockwise (ROTATION_90): the phone's right edge is up.
        let left = reading_from(&raw((9.80665, 0.0, 0.0), 1.0)).unwrap();
        assert_eq!(left.gravity, (0.0, 1.0, 0.0));
        // Turned clockwise (ROTATION_270): its left edge is up.
        let right = reading_from(&raw((-9.80665, 0.0, 0.0), 3.0)).unwrap();
        assert_eq!(right.gravity, (0.0, 1.0, 0.0));
        // Lying face up: down is through the back, away from the person.
        let flat = reading_from(&raw((0.0, 0.0, 9.80665), 0.0)).unwrap();
        assert_eq!(flat.gravity, (0.0, 0.0, -1.0));
    }

    #[test]
    fn tipping_the_right_edge_down_leans_down_to_the_right() {
        // Upright, right edge lowered: the sensor's up vector leans left.
        let tipped = reading_from(&raw((-4.9, 8.5, 0.0), 0.0)).unwrap();
        assert!(tipped.gravity.0 > 0.4, "{:?}", tipped.gravity);
    }

    #[test]
    fn the_heading_is_the_screen_tops_and_none_without_a_compass() {
        let mut r = raw((0.0, 0.0, 9.8), 1.0);
        r[9] = 0.0;
        assert_eq!(reading_from(&r).unwrap().heading, Some(90.0));
        r[9] = -1.0;
        assert_eq!(reading_from(&r).unwrap().heading, None);
        assert!(reading_from(&r[..5]).is_none());
    }
}
