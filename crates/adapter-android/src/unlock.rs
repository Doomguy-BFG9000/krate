//! Asking the person to prove it is them (krate:ui/unlock, IC-924): the
//! system's BiometricPrompt -- a fingerprint or face, with the screen lock
//! as the other way in -- through the player activity's Unlock.java.

/// What the phone's sheet answered.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Answer {
    Yes,
    No,
    Unavailable(String),
}

#[cfg(target_os = "android")]
mod real {
    use super::Answer;
    use crate::java::{string, wait_for, with_activity};
    use jni::objects::JValue;
    use std::time::Duration;

    pub fn available() -> bool {
        with_activity("asking if the phone can check", |env, activity| {
            env.call_method(activity, "krateUnlockAvailable", "()Z", &[])?
                .z()
        })
        .unwrap_or(false)
    }

    /// Show the sheet and wait for the person.
    pub fn check(reason: &str) -> Answer {
        let started = with_activity("asking it is you", |env, activity| {
            let reason = env.new_string(reason)?;
            env.call_method(
                activity,
                "krateUnlockCheck",
                "(Ljava/lang/String;)V",
                &[JValue::Object(&reason)],
            )?
            .v()
        });
        if started.is_none() {
            return Answer::Unavailable("the check could not start".to_string());
        }
        let code = wait_for(Duration::from_secs(5 * 60), || {
            match with_activity("reading the check", |env, activity| {
                env.call_method(activity, "krateUnlockAnswer", "()I", &[])?
                    .i()
            }) {
                Some(-1) => None,
                Some(code) => Some(code),
                None => Some(1),
            }
        })
        .unwrap_or(1);
        match code {
            0 => Answer::Yes,
            1 => Answer::No,
            _ => Answer::Unavailable(
                with_activity("reading why", |env, activity| {
                    let why = env
                        .call_method(activity, "krateUnlockWhy", "()Ljava/lang/String;", &[])?
                        .l()?;
                    string(env, why)
                })
                .flatten()
                .unwrap_or_else(|| "this phone cannot check it is you".to_string()),
            ),
        }
    }
}

#[cfg(target_os = "android")]
pub use real::*;
