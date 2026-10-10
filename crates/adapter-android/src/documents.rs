//! Files, folders and saves through Android's own document picker (K-1032).
//!
//! Android hands a pick over as a content URI, and Krate's runtime works with
//! paths, so each pick becomes a file of Krate's own: a picked file is
//! copied in (as iOS does); a picked folder is copied in whole and what the
//! app changes in it is put back when the app closes; a save is written in
//! Krate first and then copied out to the place the person picks in the
//! system's "save to" picker. The copying is the activity's Java
//! (Documents.java); this side starts the picker and waits for the person.

use std::path::PathBuf;

/// The picker's MIME type for an app's filter ("png,jpg"): one family when
/// every extension is in it, so the picker shows only those; anything else
/// is every file.
#[cfg_attr(not(target_os = "android"), allow(dead_code))]
pub(crate) fn mime_for_filter(filter: &str) -> &'static str {
    let filter = filter.to_ascii_lowercase();
    if ["image", "photo", "picture"]
        .iter()
        .any(|word| filter.contains(word))
    {
        return "image/*";
    }
    let family = |ext: &str| match ext {
        "png" | "jpg" | "jpeg" | "gif" | "webp" | "heic" | "heif" | "bmp" => Some("image/*"),
        "mp3" | "wav" | "ogg" | "oga" | "m4a" | "aac" | "flac" | "opus" => Some("audio/*"),
        "mp4" | "mov" | "webm" | "mkv" | "m4v" => Some("video/*"),
        "txt" | "md" | "csv" | "tsv" | "log" => Some("text/*"),
        _ => None,
    };
    let mut families = filter
        .split(',')
        .map(|ext| ext.trim().trim_start_matches('.'))
        .filter(|ext| !ext.is_empty())
        .map(family);
    let Some(Some(first)) = families.next() else {
        return "*/*";
    };
    if families.all(|f| f == Some(first)) {
        first
    } else {
        "*/*"
    }
}

/// A fresh folder for one pick under the app's temporary space, so two picks
/// of one name never meet.
#[cfg_attr(not(target_os = "android"), allow(dead_code))]
fn pick_folder() -> PathBuf {
    static PICKS: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
    let n = PICKS.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    std::env::temp_dir()
        .join("krate-picked")
        .join(format!("{}-{n}", std::process::id()))
}

#[cfg(target_os = "android")]
mod real {
    use super::*;
    use crate::java::{off_thread, string, wait_for, with_activity};
    use jni::objects::JValue;
    use std::path::Path;
    use std::sync::Mutex;
    use std::time::Duration;

    /// A picked folder copied in may hold this much at most: the copy is
    /// made before the app sees the folder, and the app waits while it is.
    const FOLDER_LIMIT: i64 = 1024 * 1024 * 1024;

    /// How long a picker may stay up before Krate stops waiting for it.
    const PICKER_LIMIT: Duration = Duration::from_secs(30 * 60);

    /// Folders copied in this run, to be put back when the app closes.
    static MIRRORS: Mutex<Vec<PathBuf>> = Mutex::new(Vec::new());

    /// Show the system's picker and wait for the person: the picked URI,
    /// or None when they cancelled.
    fn pick(kind: i32, mime: &str, name: &str) -> Option<String> {
        with_activity("starting the picker", |env, activity| {
            let mime = env.new_string(mime)?;
            let name = env.new_string(name)?;
            env.call_method(
                activity,
                "kratePick",
                "(ILjava/lang/String;Ljava/lang/String;)V",
                &[
                    JValue::Int(kind),
                    JValue::Object(&mime),
                    JValue::Object(&name),
                ],
            )?
            .v()
        })?;
        let answer = wait_for(PICKER_LIMIT, || {
            with_activity("reading the picker", |env, activity| {
                let answer = env
                    .call_method(activity, "kratePickAnswer", "()Ljava/lang/String;", &[])?
                    .l()?;
                string(env, answer)
            })
            // A failed call is no answer to wait for: treat it as cancelled.
            .unwrap_or(Some(String::new()))
        })?;
        (!answer.is_empty()).then_some(answer)
    }

    /// Call one of the activity's (String, String...) -> String copies, off
    /// Krate's thread so the activity stays answered while it copies.
    fn copy_call(
        method: &'static str,
        sig: &'static str,
        uri: &str,
        dir: &Path,
        limit: Option<i64>,
    ) -> Option<PathBuf> {
        let dir = dir.to_string_lossy().into_owned();
        let uri = uri.to_string();
        off_thread(method, move |env, activity| {
            let uri = env.new_string(&uri)?;
            let dir = env.new_string(&dir)?;
            let made = match limit {
                Some(limit) => env.call_method(
                    activity,
                    method,
                    sig,
                    &[
                        JValue::Object(&uri),
                        JValue::Object(&dir),
                        JValue::Long(limit),
                    ],
                )?,
                None => env.call_method(
                    activity,
                    method,
                    sig,
                    &[JValue::Object(&uri), JValue::Object(&dir)],
                )?,
            }
            .l()?;
            string(env, made)
        })
        .flatten()
        .map(PathBuf::from)
    }

    /// A file (`folder` false) or folder the person picks, as a path of
    /// Krate's own; None when they cancel or it could not be copied.
    pub fn choose(folder: bool, filter: &str) -> Option<PathBuf> {
        if folder {
            let tree = pick(1, "", "")?;
            let copy = copy_call(
                "krateMirror",
                "(Ljava/lang/String;Ljava/lang/String;J)Ljava/lang/String;",
                &tree,
                &pick_folder(),
                Some(FOLDER_LIMIT),
            )?;
            log::info!("picked folder copied in at {}", copy.display());
            MIRRORS.lock().expect("mirror lock").push(copy.clone());
            Some(copy)
        } else {
            let uri = pick(0, mime_for_filter(filter), "")?;
            let copy = copy_call(
                "krateCopyIn",
                "(Ljava/lang/String;Ljava/lang/String;)Ljava/lang/String;",
                &uri,
                &pick_folder(),
                None,
            )?;
            log::info!("picked file copied in at {}", copy.display());
            Some(copy)
        }
    }

    /// Ask where a file the app wrote should go and put it there: the
    /// system's "save to" picker, named as the app named it. The file is
    /// moved: Krate's copy is removed once it is placed, or when the person
    /// cancels.
    pub fn export(path: &Path) -> bool {
        if !path.is_file() {
            return false;
        }
        let name = path
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_else(|| "Untitled".to_string());
        let placed = match pick(2, "", &name) {
            Some(uri) => {
                let from = path.to_string_lossy().into_owned();
                let done = off_thread("saving to the picked place", move |env, activity| {
                    let from = env.new_string(&from)?;
                    let uri = env.new_string(&uri)?;
                    env.call_method(
                        activity,
                        "krateCopyOut",
                        "(Ljava/lang/String;Ljava/lang/String;)Z",
                        &[JValue::Object(&from), JValue::Object(&uri)],
                    )?
                    .z()
                })
                .unwrap_or(false);
                log::info!("saved {name}: {done}");
                done
            }
            None => {
                log::info!("save of {name} cancelled");
                false
            }
        };
        let _ = std::fs::remove_file(path);
        if let Some(dir) = path.parent() {
            let _ = std::fs::remove_dir(dir);
        }
        placed
    }

    /// Put back what the app changed in every folder it was given. Safe to
    /// call more than once: only what changed since is written.
    pub fn put_back_folders() {
        let roots = MIRRORS.lock().expect("mirror lock").clone();
        for root in roots {
            let shown = root.to_string_lossy().into_owned();
            let path = shown.clone();
            let changed = off_thread("putting a folder back", move |env, activity| {
                let root = env.new_string(&path)?;
                env.call_method(
                    activity,
                    "krateSyncBack",
                    "(Ljava/lang/String;)I",
                    &[JValue::Object(&root)],
                )?
                .i()
            });
            log::info!("folder {shown} put back: {changed:?} changed");
        }
    }
}

#[cfg(target_os = "android")]
pub use real::*;

#[cfg(not(target_os = "android"))]
mod stub {
    use std::path::{Path, PathBuf};

    pub fn choose(_folder: bool, _filter: &str) -> Option<PathBuf> {
        None
    }

    pub fn export(_path: &Path) -> bool {
        false
    }

    pub fn put_back_folders() {}
}

#[cfg(not(target_os = "android"))]
pub use stub::*;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_picker_shows_only_what_the_app_can_open() {
        assert_eq!(mime_for_filter("png,jpg"), "image/*");
        assert_eq!(mime_for_filter(".mp3, .m4a"), "audio/*");
        assert_eq!(mime_for_filter("Images"), "image/*");
        // Mixed families, an unknown kind, or no filter at all: every file.
        assert_eq!(mime_for_filter("png,mp3"), "*/*");
        assert_eq!(mime_for_filter("krate"), "*/*");
        assert_eq!(mime_for_filter(""), "*/*");
    }

    #[test]
    fn two_picks_never_share_a_folder() {
        assert_ne!(pick_folder(), pick_folder());
        assert!(pick_folder().starts_with(std::env::temp_dir().join("krate-picked")));
    }
}
