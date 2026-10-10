//! Record sleeves, made once from each record's two colours and a pattern
//! of its own -- rings, stripes, soft light, waves, a low sun, a dot grid --
//! with a little grain so they read as printed card rather than flat fills.
//! Kept as pictures (`krate:gfx/canvas-images`) and drawn by handle, so a
//! scrolling grid of them costs nothing per frame.

use alloc::vec::Vec;

use krate::math::F32Ext;

/// Sleeve pictures are this many pixels square: sharp at the sizes a phone
/// shows them, cheap to make when the app starts.
pub const SIZE: u32 = 192;

fn mix(a: f32, b: f32, t: f32) -> f32 {
    a + (b - a) * t
}

/// One sleeve, straight RGBA.
pub fn sleeve(pattern: usize, top: (f32, f32, f32), bottom: (f32, f32, f32)) -> Vec<u8> {
    let n = SIZE as usize;
    let mut out = Vec::with_capacity(n * n * 4);
    let mut grain: u32 = 0x9e37_79b9 ^ (pattern as u32 * 2_654_435_761);
    for py in 0..n {
        for px in 0..n {
            let (x, y) = (px as f32 / n as f32, py as f32 / n as f32);
            // A diagonal blend of the record's two colours.
            let t = ((x + y) * 0.5).clamp(0.0, 1.0);
            let mut c = [mix(top.0, bottom.0, t), mix(top.1, bottom.1, t), mix(top.2, bottom.2, t)];
            // How much lighter (positive) or darker (negative) the pattern
            // makes this pixel.
            let light = match pattern % 6 {
                0 => {
                    // Rings around a point high on the right.
                    let d = ((x - 0.72).powi(2) + (y - 0.28).powi(2)).sqrt();
                    if (d * 14.0) as i32 % 2 == 0 { 0.10 } else { -0.02 }
                }
                1 => {
                    // Broad diagonal stripes.
                    if ((x - y + 2.0) * 7.0) as i32 % 2 == 0 { 0.08 } else { -0.04 }
                }
                2 => {
                    // Three soft lights.
                    let blob = |cx: f32, cy: f32, r: f32| {
                        let d = ((x - cx).powi(2) + (y - cy).powi(2)).sqrt() / r;
                        (1.0 - d).max(0.0).powi(2)
                    };
                    0.35 * (blob(0.25, 0.3, 0.45) + blob(0.8, 0.65, 0.35) + 0.6 * blob(0.45, 0.9, 0.3))
                }
                3 => {
                    // Waves across the lower half.
                    let w = (y * 9.0 + (x * 6.28 * 1.5).sin() * 0.35) * 3.14;
                    if y > 0.35 { 0.07 * w.sin() } else { 0.0 }
                }
                4 => {
                    // A low sun over a horizon.
                    let d = ((x - 0.5).powi(2) + (y - 0.62).powi(2)).sqrt();
                    if y > 0.7 {
                        -0.12
                    } else if d < 0.24 {
                        0.32
                    } else {
                        0.18 * (1.0 - (d - 0.24) * 3.0).max(0.0)
                    }
                }
                _ => {
                    // A grid of dots.
                    let (gx, gy) = ((x * 9.0).fract() - 0.5, (y * 9.0).fract() - 0.5);
                    if (gx * gx + gy * gy).sqrt() < 0.18 { 0.14 } else { 0.0 }
                }
            };
            // Grain, and a soft vignette.
            grain = grain.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
            let g = ((grain >> 24) as f32 / 255.0 - 0.5) * 0.045;
            let v = -0.10 * ((x - 0.5).powi(2) + (y - 0.5).powi(2)) * 2.0;
            for ch in c.iter_mut() {
                let lifted = if light > 0.0 { *ch + (1.0 - *ch) * light } else { *ch * (1.0 + light) };
                *ch = (lifted + g + v).clamp(0.0, 1.0);
            }
            out.extend_from_slice(&[
                (c[0] * 255.0) as u8,
                (c[1] * 255.0) as u8,
                (c[2] * 255.0) as u8,
                255,
            ]);
        }
    }
    out
}
