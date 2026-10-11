//! The system's share sheet (krate:ui/share, IC-919): the app's files are
//! written to a fresh folder in Krate's share space, which the player's own
//! provider (Shared.java) serves read-only to the one app the person picks;
//! the text and link go as the shared text. The runtime has already refused
//! nothing to share, a name with a path, or too much.

/// The text a share sends: the app's text, then its link on a line of its
/// own, either alone, or nothing.
#[cfg_attr(not(target_os = "android"), allow(dead_code))]
pub(crate) fn shared_text(text: Option<&str>, url: Option<&str>) -> String {
    let parts: Vec<&str> = [text, url]
        .into_iter()
        .flatten()
        .map(str::trim)
        .filter(|part| !part.is_empty())
        .collect();
    parts.join("\n")
}

#[cfg(target_os = "android")]
mod real {
    use super::shared_text;
    use crate::java::{wait_for, with_activity};
    use jni::objects::{JObject, JValue};
    use std::time::Duration;

    /// Open the sheet and wait for the person: true when sent to an app.
    pub fn share(
        text: Option<&str>,
        url: Option<&str>,
        files: &[(String, Vec<u8>)],
    ) -> Result<bool, String> {
        static SHARES: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
        let n = SHARES.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        // Kept until Krate next starts: the app picked may read a file well
        // after the sheet closes (a mail being written).
        let folder = std::env::temp_dir()
            .join("krate-share")
            .join(format!("{}-{n}", std::process::id()));
        let mut paths = Vec::new();
        if !files.is_empty() {
            std::fs::create_dir_all(&folder).map_err(|e| format!("could not stage: {e}"))?;
            for (name, bytes) in files {
                let path = folder.join(name);
                std::fs::write(&path, bytes).map_err(|e| format!("could not stage {name}: {e}"))?;
                paths.push(path.to_string_lossy().into_owned());
            }
        }
        let body = shared_text(text, url);
        with_activity("opening the share sheet", |env, activity| {
            let body = env.new_string(&body)?;
            let list =
                env.new_object_array(paths.len() as i32, "java/lang/String", JObject::null())?;
            for (i, path) in paths.iter().enumerate() {
                let path = env.new_string(path)?;
                env.set_object_array_element(&list, i as i32, path)?;
            }
            env.call_method(
                activity,
                "krateShare",
                "(Ljava/lang/String;[Ljava/lang/String;)V",
                &[JValue::Object(&body), JValue::Object(&list)],
            )?
            .v()
        })
        .ok_or_else(|| "the share sheet could not be opened".to_string())?;
        let answer = wait_for(Duration::from_secs(30 * 60), || {
            match with_activity("reading the share sheet", |env, activity| {
                env.call_method(activity, "krateShareAnswer", "()I", &[])?
                    .i()
            }) {
                Some(-1) => None,
                Some(answer) => Some(answer == 1),
                None => Some(false),
            }
        });
        let sent = answer.unwrap_or(false);
        log::info!("share: {}", if sent { "sent" } else { "cancelled" });
        Ok(sent)
    }
}

#[cfg(target_os = "android")]
pub use real::*;

#[cfg(test)]
mod tests {
    use super::shared_text;

    #[test]
    fn the_text_and_link_go_as_one_message() {
        assert_eq!(
            shared_text(Some("A note"), Some("https://krate.tech")),
            "A note\nhttps://krate.tech"
        );
        assert_eq!(
            shared_text(None, Some("https://krate.tech")),
            "https://krate.tech"
        );
        assert_eq!(shared_text(Some("  "), None), "");
    }
}
