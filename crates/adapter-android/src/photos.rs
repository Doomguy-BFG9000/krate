//! The photo library (krate:ui/photos, IC-920): the system's photo picker
//! and MediaStore, through the player activity's Photos.java. The runtime
//! has already checked `photos.add` and refused anything but a PNG or JPEG.

#[cfg(target_os = "android")]
mod real {
    use crate::java::{off_thread, string, wait_for, with_activity};
    use jni::objects::{JObjectArray, JValue};
    use std::path::PathBuf;
    use std::time::Duration;

    /// Show the photo picker for up to `limit` photos: the photos picked,
    /// copied in; empty when the person cancelled.
    pub fn pick(limit: u32) -> Result<Vec<PathBuf>, String> {
        with_activity("opening the photo picker", |env, activity| {
            env.call_method(
                activity,
                "kratePickPhotos",
                "(I)V",
                &[JValue::Int(limit.max(1) as i32)],
            )?
            .v()
        })
        .ok_or_else(|| "the photo picker could not be opened".to_string())?;
        let uris = wait_for(Duration::from_secs(30 * 60), || {
            with_activity("reading the photo picker", |env, activity| {
                let all = env
                    .call_method(activity, "kratePhotosAnswer", "()[Ljava/lang/String;", &[])?
                    .l()?;
                if all.is_null() {
                    return Ok(None);
                }
                let all = JObjectArray::from(all);
                let mut out = Vec::new();
                for i in 0..env.get_array_length(&all)? {
                    let one = env.get_object_array_element(&all, i)?;
                    out.extend(string(env, one)?);
                }
                Ok(Some(out))
            })
            .unwrap_or(Some(Vec::new()))
        })
        .unwrap_or_default();
        if uris.is_empty() {
            return Ok(Vec::new());
        }
        static PICKS: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
        let n = PICKS.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        let dir = std::env::temp_dir()
            .join("krate-picked")
            .join(format!("photos-{}-{n}", std::process::id()))
            .to_string_lossy()
            .into_owned();
        let picked = off_thread("copying the photos in", move |env, activity| {
            let mut out = Vec::new();
            for uri in &uris {
                let uri = env.new_string(uri)?;
                let dir = env.new_string(&dir)?;
                let path = env
                    .call_method(
                        activity,
                        "kratePhotoCopy",
                        "(Ljava/lang/String;Ljava/lang/String;)Ljava/lang/String;",
                        &[JValue::Object(&uri), JValue::Object(&dir)],
                    )?
                    .l()?;
                out.extend(string(env, path)?.map(PathBuf::from));
            }
            Ok(out)
        })
        .unwrap_or_default();
        if picked.is_empty() {
            // Photos picked and none readable is not a cancel.
            return Err("none of the photos picked could be read".to_string());
        }
        log::info!("photos: {} picked", picked.len());
        Ok(picked)
    }

    /// Add a PNG or JPEG to the library: Err(None) when the person said no.
    pub fn save(data: &[u8]) -> Result<(), Option<String>> {
        if crate::phone::ask("krateSavePhotoAsk") == crate::phone::Asked::Refused {
            return Err(None);
        }
        let data = data.to_vec();
        let why = off_thread("adding to Photos", move |env, activity| {
            let bytes = env.byte_array_from_slice(&data)?;
            let why = env
                .call_method(
                    activity,
                    "krateSavePhoto",
                    "([B)Ljava/lang/String;",
                    &[JValue::Object(&bytes)],
                )?
                .l()?;
            string(env, why)
        })
        .ok_or_else(|| Some("the photo library could not be reached".to_string()))?;
        why.map_or(Ok(()), |why| Err(Some(why)))
    }
}

#[cfg(target_os = "android")]
pub use real::*;
