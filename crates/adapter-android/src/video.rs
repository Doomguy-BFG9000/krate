//! Video playback on Android (krate:gfx/video, IC-922): MediaPlayer into an
//! ImageReader, through the player activity's Video.java. Frames come as
//! I420 and are turned to RGBA here, as the camera's are.

#[cfg(target_os = "android")]
mod real {
    use crate::java::{string, with_activity};
    use jni::objects::{JByteArray, JLongArray, JValue};

    /// Start loading a file path or an https address: the video's handle.
    pub fn open(source: &str) -> Result<u64, String> {
        with_activity("opening a video", |env, activity| {
            let source = env.new_string(source)?;
            env.call_method(
                activity,
                "krateVideoOpen",
                "(Ljava/lang/String;)J",
                &[JValue::Object(&source)],
            )?
            .j()
        })
        .map(|id| id as u64)
        .ok_or_else(|| "the video could not be opened".to_string())
    }

    /// kind 0 play, 1 pause, 2 seek (value ms), 3 loop (1/0), 4 mute (1/0).
    pub fn control(video: u64, kind: i32, value: i64) -> Result<(), String> {
        let why = with_activity("controlling a video", |env, activity| {
            let why = env
                .call_method(
                    activity,
                    "krateVideoControl",
                    "(JIJ)Ljava/lang/String;",
                    &[
                        JValue::Long(video as i64),
                        JValue::Int(kind),
                        JValue::Long(value),
                    ],
                )?
                .l()?;
            string(env, why)
        })
        .ok_or_else(|| "the video could not be reached".to_string())?;
        why.map_or(Ok(()), Err)
    }

    /// {position, duration, width, height, ready, playing, ended} and the
    /// problem, if any.
    pub fn state(video: u64) -> Result<([i64; 7], String), String> {
        with_activity("reading a video", |env, activity| {
            let all = env
                .call_method(
                    activity,
                    "krateVideoState",
                    "(J)[J",
                    &[JValue::Long(video as i64)],
                )?
                .l()?;
            if all.is_null() {
                return Ok(None);
            }
            let all = JLongArray::from(all);
            let mut out = [0i64; 7];
            env.get_long_array_region(&all, 0, &mut out)?;
            let why = env
                .call_method(
                    activity,
                    "krateVideoProblem",
                    "(J)Ljava/lang/String;",
                    &[JValue::Long(video as i64)],
                )?
                .l()?;
            Ok(Some((out, string(env, why)?.unwrap_or_default())))
        })
        .flatten()
        .ok_or_else(|| "no such video".to_string())
    }

    /// The newest frame since the last ask, as RGBA.
    pub fn frame(video: u64) -> Option<(Vec<u8>, u32, u32)> {
        let (yuv, w, h) = with_activity("taking a video frame", |env, activity| {
            let size = env.new_int_array(2)?;
            let frame = env
                .call_method(
                    activity,
                    "krateVideoFrame",
                    "(J[I)[B",
                    &[JValue::Long(video as i64), JValue::Object(&size)],
                )?
                .l()?;
            if frame.is_null() {
                return Ok(None);
            }
            let mut dims = [0i32; 2];
            env.get_int_array_region(&size, 0, &mut dims)?;
            let bytes = env.convert_byte_array(JByteArray::from(frame))?;
            Ok(Some((bytes, dims[0] as usize, dims[1] as usize)))
        })
        .flatten()?;
        let (rgba, w, h) = crate::camera::i420_to_rgba(&yuv, w, h, 0, false)?;
        Some((rgba, w as u32, h as u32))
    }

    pub fn close(video: u64) {
        let _ = with_activity("closing a video", |env, activity| {
            env.call_method(
                activity,
                "krateVideoClose",
                "(J)V",
                &[JValue::Long(video as i64)],
            )?
            .v()
        });
    }
}

#[cfg(target_os = "android")]
pub use real::*;
