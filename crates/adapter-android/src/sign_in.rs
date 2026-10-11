//! Signing the person in through the browser (krate:ui/sign-in, IC-923),
//! through the player activity's SignIn.java: a Custom Tab returning to
//! Krate's one shared scheme, krate-signin, or Chrome's Auth Tab for an
//! app's own scheme where the installed Chrome has it. The runtime has
//! already refused http addresses and schemes no app may own.

#[cfg(target_os = "android")]
mod real {
    use crate::java::{string, wait_for, with_activity};
    use jni::objects::JValue;
    use std::time::Duration;

    /// The address the service sent the person back to, None when they
    /// closed the page.
    pub fn sign_in(url: &str, scheme: &str) -> Result<Option<String>, String> {
        let why = with_activity("opening the sign-in page", |env, activity| {
            let url = env.new_string(url)?;
            let scheme = env.new_string(scheme)?;
            let why = env
                .call_method(
                    activity,
                    "krateSignIn",
                    "(Ljava/lang/String;Ljava/lang/String;)Ljava/lang/String;",
                    &[JValue::Object(&url), JValue::Object(&scheme)],
                )?
                .l()?;
            string(env, why)
        })
        .ok_or_else(|| "the sign-in page could not be opened".to_string())?;
        if let Some(why) = why {
            return Err(why);
        }
        let back = wait_for(Duration::from_secs(15 * 60), || {
            with_activity("waiting for the sign-in", |env, activity| {
                let back = env
                    .call_method(activity, "krateSignInAnswer", "()Ljava/lang/String;", &[])?
                    .l()?;
                string(env, back)
            })
            .unwrap_or(Some(String::new()))
        })
        .unwrap_or_default();
        Ok((!back.is_empty()).then_some(back))
    }
}

#[cfg(target_os = "android")]
pub use real::*;
