//! The screen the person came from, small and blurred, for Krate's
//! permission sheet to sit over (F-314). The sheet reads `behind.rgba` from
//! its own folder: an 8-byte header of width and height (little-endian),
//! then RGBA. On Android the screen Krate can see is its own -- the app the
//! person was in when they chose Restart or Ask again from Krate's menu --
//! so that is the picture kept; a cold start, from another app, has none
//! and the sheet sits on its soft wash.

use std::sync::Mutex;

/// The last picture kept, ready to write.
static KEPT: Mutex<Option<Vec<u8>>> = Mutex::new(None);

/// A frame of 0x00RRGGBB pixels shrunk by `factor` (each output pixel the
/// average of a factor-square block), as RGBA.
#[cfg_attr(not(target_os = "android"), allow(dead_code))]
pub(crate) fn shrink(xrgb: &[u32], w: usize, h: usize, factor: usize) -> (usize, usize, Vec<u8>) {
    let factor = factor.max(1);
    let (ow, oh) = ((w / factor).max(1), (h / factor).max(1));
    let mut out = Vec::with_capacity(ow * oh * 4);
    for oy in 0..oh {
        for ox in 0..ow {
            let mut sum = [0u32; 3];
            let mut n = 0u32;
            for y in oy * factor..((oy + 1) * factor).min(h) {
                for x in ox * factor..((ox + 1) * factor).min(w) {
                    let px = xrgb.get(y * w + x).copied().unwrap_or(0);
                    sum[0] += (px >> 16) & 0xff;
                    sum[1] += (px >> 8) & 0xff;
                    sum[2] += px & 0xff;
                    n += 1;
                }
            }
            let n = n.max(1);
            out.extend_from_slice(&[
                (sum[0] / n) as u8,
                (sum[1] / n) as u8,
                (sum[2] / n) as u8,
                255,
            ]);
        }
    }
    (ow, oh, out)
}

/// A box blur of `radius`, across then down, as the iPhone's.
#[cfg_attr(not(target_os = "android"), allow(dead_code))]
pub(crate) fn box_blur(rgba: &mut [u8], w: usize, h: usize, radius: usize) {
    if w == 0 || h == 0 || rgba.len() < w * h * 4 {
        return;
    }
    let mut tmp = vec![0u8; rgba.len()];
    let pass =
        |src: &[u8], dst: &mut [u8], along: usize, across: usize, step: usize, line: usize| {
            for a in 0..across {
                for i in 0..along {
                    let lo = i.saturating_sub(radius);
                    let hi = (i + radius).min(along - 1);
                    let mut sum = [0u32; 3];
                    for j in lo..=hi {
                        let at = (a * line + j * step) * 4;
                        for (c, s) in sum.iter_mut().enumerate() {
                            *s += u32::from(src[at + c]);
                        }
                    }
                    let n = (hi - lo + 1) as u32;
                    let at = (a * line + i * step) * 4;
                    for (c, s) in sum.iter().enumerate() {
                        dst[at + c] = (s / n) as u8;
                    }
                    dst[at + 3] = 255;
                }
            }
        };
    pass(rgba, &mut tmp, w, h, 1, w);
    pass(&tmp, rgba, h, w, w, 1);
}

/// The sheet's picture of a frame: a third of its size in points, blurred,
/// with the 8-byte header.
#[cfg_attr(not(target_os = "android"), allow(dead_code))]
pub(crate) fn picture(xrgb: &[u32], w: usize, h: usize, scale: f32) -> Vec<u8> {
    let factor = (scale.max(1.0) * 3.0).round() as usize;
    let (ow, oh, mut rgba) = shrink(xrgb, w, h, factor);
    for _ in 0..3 {
        box_blur(&mut rgba, ow, oh, 4);
    }
    let mut out = Vec::with_capacity(8 + rgba.len());
    out.extend_from_slice(&(ow as u32).to_le_bytes());
    out.extend_from_slice(&(oh as u32).to_le_bytes());
    out.extend_from_slice(&rgba);
    out
}

/// Keep a picture of this frame for the next sheet.
#[cfg_attr(not(target_os = "android"), allow(dead_code))]
pub(crate) fn remember(xrgb: &[u32], w: usize, h: usize, scale: f32) {
    if w == 0 || h == 0 || xrgb.len() < w * h {
        return;
    }
    *KEPT.lock().expect("behind lock") = Some(picture(xrgb, w, h, scale));
}

/// Write the picture kept, if any, where the sheet will look for it.
pub fn write(path: &std::path::Path) -> bool {
    let Some(bytes) = KEPT.lock().expect("behind lock").take() else {
        return false;
    };
    if let Some(dir) = path.parent() {
        let _ = std::fs::create_dir_all(dir);
    }
    let part = path.with_extension("part");
    std::fs::write(&part, &bytes).is_ok() && std::fs::rename(&part, path).is_ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_frame_shrinks_to_the_average_of_its_blocks() {
        // 2x2 of red and blue, shrunk by 2: one purple pixel.
        let frame = [0x00ff0000, 0x000000ff, 0x00ff0000, 0x000000ff];
        let (w, h, rgba) = shrink(&frame, 2, 2, 2);
        assert_eq!((w, h), (1, 1));
        assert_eq!(rgba, vec![127, 0, 127, 255]);
    }

    #[test]
    fn the_picture_is_small_enough_for_the_sheet_and_says_its_size() {
        // A Pixel 7 frame at 2.625: about 137 x 305, far under the sheet's
        // 512 x 512 limit.
        let (w, h) = (1080usize, 2400usize);
        let frame = vec![0x00808080u32; w * h];
        let out = picture(&frame, w, h, 2.625);
        let pw = u32::from_le_bytes(out[0..4].try_into().unwrap()) as usize;
        let ph = u32::from_le_bytes(out[4..8].try_into().unwrap()) as usize;
        assert!(pw <= 512 && ph <= 512, "{pw}x{ph}");
        assert_eq!(out.len(), 8 + pw * ph * 4);
        assert_eq!(&out[8..12], &[128, 128, 128, 255], "a flat grey stays grey");
    }
}
