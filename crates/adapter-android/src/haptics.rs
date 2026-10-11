//! The phone tapping back (krate:ui/haptics, IC-917) through the player
//! activity's Haptics.java: the vibrator's own predefined effects.

use krate_adapter_common::ui::HapticFeel;

/// The number Haptics.java gives each feel; -1 is an impact.
#[cfg_attr(not(target_os = "android"), allow(dead_code))]
pub(crate) fn feel_code(feel: Option<HapticFeel>) -> i32 {
    match feel {
        Some(HapticFeel::Light) => 0,
        Some(HapticFeel::Medium) => 1,
        Some(HapticFeel::Heavy) => 2,
        Some(HapticFeel::Selection) => 3,
        Some(HapticFeel::Success) => 4,
        Some(HapticFeel::Warning) => 5,
        Some(HapticFeel::Error) => 6,
        None => -1,
    }
}

/// An impact's strength held to 0..1, NaN to 0, as on iOS.
#[cfg_attr(not(target_os = "android"), allow(dead_code))]
pub(crate) fn held_strength(strength: f32) -> f32 {
    if strength.is_nan() {
        0.0
    } else {
        strength.clamp(0.0, 1.0)
    }
}

#[cfg(target_os = "android")]
mod real {
    use super::*;
    use crate::java::with_activity;
    use jni::objects::JValue;
    use std::sync::{Mutex, OnceLock};
    use std::time::{Duration, Instant};

    /// Whether the phone has a vibrator, asked once.
    pub fn available() -> bool {
        static HAS: OnceLock<bool> = OnceLock::new();
        *HAS.get_or_init(|| {
            with_activity("asking for a vibrator", |env, activity| {
                env.call_method(activity, "krateHapticsAvailable", "()Z", &[])?
                    .z()
            })
            .unwrap_or(false)
        })
    }

    /// When the last tap went out: taps closer than a vibrator can play
    /// are dropped, so a game may ask every frame.
    static LAST_TAP: Mutex<Option<Instant>> = Mutex::new(None);

    /// Tap back with `feel`, or an impact at `strength` when `feel` is None.
    pub fn play(feel: Option<HapticFeel>, strength: f32) -> bool {
        if !available() {
            return false;
        }
        {
            let mut last = LAST_TAP.lock().expect("tap lock");
            let now = Instant::now();
            if last.is_some_and(|at| now.duration_since(at) < Duration::from_millis(30)) {
                return true;
            }
            *last = Some(now);
        }
        let code = feel_code(feel);
        let strength = held_strength(strength);
        with_activity("tapping back", |env, activity| {
            env.call_method(
                activity,
                "krateHaptic",
                "(IF)Z",
                &[JValue::Int(code), JValue::Float(strength)],
            )?
            .z()
        })
        .unwrap_or(false)
    }
}

#[cfg(target_os = "android")]
pub use real::*;

#[cfg(not(target_os = "android"))]
mod stub {
    use krate_adapter_common::ui::HapticFeel;

    pub fn available() -> bool {
        false
    }

    pub fn play(_feel: Option<HapticFeel>, _strength: f32) -> bool {
        false
    }
}

#[cfg(not(target_os = "android"))]
pub use stub::*;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn each_feel_reaches_java_as_its_own_number() {
        let feels = [
            HapticFeel::Light,
            HapticFeel::Medium,
            HapticFeel::Heavy,
            HapticFeel::Selection,
            HapticFeel::Success,
            HapticFeel::Warning,
            HapticFeel::Error,
        ];
        let codes: Vec<i32> = feels.iter().map(|f| feel_code(Some(*f))).collect();
        assert_eq!(codes, vec![0, 1, 2, 3, 4, 5, 6]);
        assert_eq!(feel_code(None), -1);
    }

    #[test]
    fn an_impact_is_held_to_what_a_phone_can_play() {
        assert_eq!(held_strength(0.5), 0.5);
        assert_eq!(held_strength(7.0), 1.0);
        assert_eq!(held_strength(-1.0), 0.0);
        assert_eq!(held_strength(f32::NAN), 0.0);
    }
}
