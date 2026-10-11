//! The camera on Android (IC-918), through the player activity's Cam.java:
//! Camera2 frames as tight I420, turned here into straight RGBA the right
//! way up for the screen -- the sensor is mounted a quarter turn from the
//! phone held upright, and a front camera is a mirror.

/// I420 (Y plane, then U, then V at half size) to RGBA, turned clockwise by
/// `turn` quarter turns and mirrored left to right when `mirror`. Returns
/// the picture and its width and height after the turn.
pub fn i420_to_rgba(
    yuv: &[u8],
    width: usize,
    height: usize,
    turn: u32,
    mirror: bool,
) -> Option<(Vec<u8>, usize, usize)> {
    let cw = width.div_ceil(2);
    let ch = height.div_ceil(2);
    if width == 0 || height == 0 || yuv.len() < width * height + 2 * cw * ch {
        return None;
    }
    let (y_plane, rest) = yuv.split_at(width * height);
    let (u_plane, v_plane) = rest.split_at(cw * ch);
    let turn = turn % 4;
    let (ow, oh) = if turn % 2 == 1 {
        (height, width)
    } else {
        (width, height)
    };
    let mut out = vec![0u8; ow * oh * 4];
    for sy in 0..height {
        for sx in 0..width {
            let y = f32::from(y_plane[sy * width + sx]);
            let c = (sy / 2) * cw + sx / 2;
            let u = f32::from(u_plane[c]) - 128.0;
            let v = f32::from(v_plane[c]) - 128.0;
            // BT.601, the full range Camera2's YUV_420_888 uses.
            let r = (y + 1.402 * v).clamp(0.0, 255.0) as u8;
            let g = (y - 0.344_136 * u - 0.714_136 * v).clamp(0.0, 255.0) as u8;
            let b = (y + 1.772 * u).clamp(0.0, 255.0) as u8;
            // Where this pixel lands once turned clockwise.
            let (mut dx, dy) = match turn {
                1 => (height - 1 - sy, sx),
                2 => (width - 1 - sx, height - 1 - sy),
                3 => (sy, width - 1 - sx),
                _ => (sx, sy),
            };
            if mirror {
                dx = ow - 1 - dx;
            }
            let at = (dy * ow + dx) * 4;
            out[at..at + 4].copy_from_slice(&[r, g, b, 255]);
        }
    }
    Some((out, ow, oh))
}

/// Quarter turns clockwise that put a sensor mounted at `sensor` degrees
/// upright for a display turned `display` (0..3, Android's ROTATION_*).
pub fn turns_for(sensor: i32, display: i32, front: bool) -> u32 {
    let display_degrees = display.rem_euclid(4) * 90;
    let degrees = if front {
        (sensor + display_degrees).rem_euclid(360)
    } else {
        (sensor - display_degrees).rem_euclid(360)
    };
    (degrees / 90) as u32
}

#[cfg(target_os = "android")]
mod real {
    use super::*;
    use crate::java::{string, with_activity};
    use crate::phone::Asked;
    use jni::objects::{JByteArray, JIntArray, JObjectArray, JValue};

    /// The phone's camera question, asked before the first open.
    pub fn ask() -> Asked {
        crate::phone::ask("krateCameraAsk")
    }

    /// Each camera as (id, label), back cameras first.
    pub fn devices() -> Vec<(String, String)> {
        with_activity("listing cameras", |env, activity| {
            let all = env
                .call_method(activity, "krateCameras", "()[Ljava/lang/String;", &[])?
                .l()?;
            let all = JObjectArray::from(all);
            let n = env.get_array_length(&all)?;
            let mut out = Vec::new();
            for i in 0..n {
                let one = env.get_object_array_element(&all, i)?;
                if let Some(line) = string(env, one)? {
                    if let Some((id, label)) = line.split_once('\t') {
                        out.push((id.to_string(), label.to_string()));
                    }
                }
            }
            Ok(out)
        })
        .unwrap_or_default()
    }

    /// Open a camera near `width` x `height`: Ok with the size it chose.
    pub fn open(id: &str, width: u32, height: u32, fps: u32) -> Result<(u32, u32), String> {
        let why = with_activity("opening the camera", |env, activity| {
            let id = env.new_string(id)?;
            let why = env
                .call_method(
                    activity,
                    "krateCameraOpen",
                    "(Ljava/lang/String;III)Ljava/lang/String;",
                    &[
                        JValue::Object(&id),
                        JValue::Int(width as i32),
                        JValue::Int(height as i32),
                        JValue::Int(fps as i32),
                    ],
                )?
                .l()?;
            string(env, why)
        })
        .ok_or_else(|| "the camera could not be opened".to_string())?;
        if let Some(why) = why {
            return Err(why);
        }
        let info = info().ok_or_else(|| "the camera could not be opened".to_string())?;
        Ok((info[0] as u32, info[1] as u32))
    }

    /// {width, height, sensor orientation, front, frames so far}.
    fn info() -> Option<Vec<i32>> {
        with_activity("reading the camera", |env, activity| {
            let all = env
                .call_method(activity, "krateCameraInfo", "()[I", &[])?
                .l()?;
            let all = JIntArray::from(all);
            let mut out = vec![0i32; env.get_array_length(&all)? as usize];
            env.get_int_array_region(&all, 0, &mut out)?;
            Ok(out)
        })
    }

    fn display_rotation() -> i32 {
        with_activity("reading the display", |env, activity| {
            env.call_method(activity, "krateDisplayRotation", "()I", &[])?
                .i()
        })
        .unwrap_or(0)
    }

    /// The newest frame since the last ask, as RGBA the right way up.
    pub fn frame() -> Option<(Vec<u8>, u32, u32)> {
        let yuv = with_activity("taking a camera frame", |env, activity| {
            let frame = env
                .call_method(activity, "krateCameraFrame", "()[B", &[])?
                .l()?;
            if frame.is_null() {
                return Ok(None);
            }
            Ok(Some(env.convert_byte_array(JByteArray::from(frame))?))
        })
        .flatten()?;
        let info = info()?;
        let turn = turns_for(info[2], display_rotation(), info[3] == 1);
        let (rgba, w, h) =
            i420_to_rgba(&yuv, info[0] as usize, info[1] as usize, turn, info[3] == 1)?;
        Some((rgba, w as u32, h as u32))
    }

    /// Why the camera stopped, if it did.
    pub fn error() -> Option<String> {
        with_activity("reading the camera", |env, activity| {
            let why = env
                .call_method(activity, "krateCameraError", "()Ljava/lang/String;", &[])?
                .l()?;
            string(env, why)
        })
        .flatten()
    }

    pub fn close() {
        let _ = with_activity("closing the camera", |env, activity| {
            env.call_method(activity, "krateCameraClose", "()V", &[])?
                .v()
        });
    }
}

#[cfg(target_os = "android")]
pub use real::*;

#[cfg(test)]
mod tests {
    use super::*;

    /// A 2x2 picture: Y as given, neutral colour.
    fn grey(y: [u8; 4]) -> Vec<u8> {
        let mut v = y.to_vec();
        v.extend_from_slice(&[128, 128]);
        v
    }

    fn lumas(rgba: &[u8]) -> Vec<u8> {
        rgba.chunks(4).map(|p| p[0]).collect()
    }

    #[test]
    fn a_quarter_turn_puts_the_left_column_on_top() {
        // 2 wide, 2 tall:  10 20 / 30 40.
        let yuv = grey([10, 20, 30, 40]);
        let (same, w, h) = i420_to_rgba(&yuv, 2, 2, 0, false).unwrap();
        assert_eq!((w, h), (2, 2));
        assert_eq!(lumas(&same), vec![10, 20, 30, 40]);
        // Clockwise: the left column (10, 30) becomes the top row, read
        // from the bottom: 30 10 / 40 20.
        let (turned, _, _) = i420_to_rgba(&yuv, 2, 2, 1, false).unwrap();
        assert_eq!(lumas(&turned), vec![30, 10, 40, 20]);
        let (mirrored, _, _) = i420_to_rgba(&yuv, 2, 2, 0, true).unwrap();
        assert_eq!(lumas(&mirrored), vec![20, 10, 40, 30]);
    }

    #[test]
    fn a_wide_frame_turned_once_is_tall_and_a_short_one_is_refused() {
        let mut yuv = vec![100u8; 4 * 2];
        yuv.extend_from_slice(&[128; 4]);
        let (rgba, w, h) = i420_to_rgba(&yuv, 4, 2, 1, false).unwrap();
        assert_eq!((w, h, rgba.len()), (2, 4, 32));
        assert!(i420_to_rgba(&yuv[..5], 4, 2, 0, false).is_none());
        // Pure red in BT.601 stays red.
        let red = [76u8, 76, 76, 76, 85, 255];
        let (px, _, _) = i420_to_rgba(&red, 2, 2, 0, false).unwrap();
        assert!(px[0] > 240 && px[1] < 20 && px[2] < 20, "{:?}", &px[..4]);
    }

    #[test]
    fn a_back_sensor_at_90_is_turned_once_upright_and_not_in_landscape() {
        assert_eq!(turns_for(90, 0, false), 1);
        // Turned anticlockwise (ROTATION_90) the sensor is already level.
        assert_eq!(turns_for(90, 1, false), 0);
        assert_eq!(turns_for(90, 3, false), 2);
        // A front camera at 270 held upright.
        assert_eq!(turns_for(270, 0, true), 3);
    }
}
