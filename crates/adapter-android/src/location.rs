//! Where the phone is (krate:ui/location, IC-917): the platform's own
//! LocationManager through the player activity's Where.java, no Google Play
//! services. The runtime has already checked `location.read`; the phone's
//! own prompt comes up on the first ask.

use krate_adapter_common::ui::{LocationAnswer, Position};

/// One Where.java answer: status (0 here, 1 not allowed, 2 unavailable,
/// 3 finding), latitude, longitude, accuracy, altitude, speed, course (NaN
/// when not known) and age in ms.
#[cfg_attr(not(target_os = "android"), allow(dead_code))]
pub(crate) fn answer_from(raw: &[f64]) -> LocationAnswer {
    if raw.len() < 8 {
        return LocationAnswer::Unavailable;
    }
    let known = |v: f64| (!v.is_nan()).then_some(v as f32);
    match raw[0] as i32 {
        0 => LocationAnswer::Here(Position {
            latitude: raw[1],
            longitude: raw[2],
            accuracy: raw[3] as f32,
            altitude: known(raw[4]),
            speed: known(raw[5]),
            course: known(raw[6]),
            age_ms: raw[7].max(0.0) as u64,
        }),
        1 => LocationAnswer::NotAllowed,
        3 => LocationAnswer::Finding,
        _ => LocationAnswer::Unavailable,
    }
}

#[cfg(target_os = "android")]
mod real {
    use super::*;
    use crate::java::with_activity;
    use jni::objects::JDoubleArray;

    pub fn available() -> bool {
        with_activity("asking for location", |env, activity| {
            env.call_method(activity, "krateLocationAvailable", "()Z", &[])?
                .z()
        })
        .unwrap_or(false)
    }

    /// The latest fix, starting the search (and the phone's prompt) on the
    /// first ask.
    pub fn read() -> LocationAnswer {
        let raw = with_activity("reading location", |env, activity| {
            let array = env
                .call_method(activity, "krateLocation", "()[D", &[])?
                .l()?;
            let array = JDoubleArray::from(array);
            let len = env.get_array_length(&array)? as usize;
            let mut out = vec![0f64; len];
            env.get_double_array_region(&array, 0, &mut out)?;
            Ok(out)
        });
        raw.map_or(LocationAnswer::Unavailable, |raw| answer_from(&raw))
    }

    /// The app closed: stop looking, so the location indicator goes away.
    pub fn stop() {
        let _ = with_activity("stopping location", |env, activity| {
            env.call_method(activity, "krateLocationStop", "()V", &[])?
                .v()
        });
    }
}

#[cfg(target_os = "android")]
pub use real::*;

#[cfg(not(target_os = "android"))]
mod stub {
    use krate_adapter_common::ui::LocationAnswer;

    pub fn available() -> bool {
        false
    }

    pub fn read() -> LocationAnswer {
        LocationAnswer::Unavailable
    }

    pub fn stop() {}
}

#[cfg(not(target_os = "android"))]
pub use stub::*;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_fix_comes_through_with_what_the_phone_does_not_know_left_out() {
        let nan = f64::NAN;
        let here = answer_from(&[0.0, 51.5, -0.12, 8.0, nan, 1.5, nan, 1200.0]);
        assert_eq!(
            here,
            LocationAnswer::Here(Position {
                latitude: 51.5,
                longitude: -0.12,
                accuracy: 8.0,
                altitude: None,
                speed: Some(1.5),
                course: None,
                age_ms: 1200,
            })
        );
    }

    #[test]
    fn each_reason_for_no_fix_is_its_own() {
        let rest = [0.0; 7];
        let with = |status: f64| {
            let mut raw = vec![status];
            raw.extend_from_slice(&rest);
            answer_from(&raw)
        };
        assert_eq!(with(1.0), LocationAnswer::NotAllowed);
        assert_eq!(with(2.0), LocationAnswer::Unavailable);
        assert_eq!(with(3.0), LocationAnswer::Finding);
        assert_eq!(answer_from(&[0.0]), LocationAnswer::Unavailable);
    }
}
