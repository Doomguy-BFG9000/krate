//! How the person asked things to look (krate:ui/appearance, IC-925): dark
//! mode, more contrast and text size, from Android's configuration through
//! the player activity. Krate keeps the activity through a change of either
//! (configChanges uiMode|fontScale), so an app reads the new look on its
//! next ask instead of being torn down.

use krate_adapter_common::ui::Appearance;

/// The activity's {dark, contrast, font scale} as an [`Appearance`].
#[cfg_attr(not(target_os = "android"), allow(dead_code))]
pub(crate) fn from_parts(parts: &[f32]) -> Appearance {
    match parts {
        [dark, contrast, scale, ..] => Appearance {
            dark: *dark != 0.0,
            high_contrast: *contrast != 0.0,
            text_scale: if scale.is_finite() {
                scale.clamp(0.5, 4.0)
            } else {
                1.0
            },
        },
        _ => Appearance::default(),
    }
}

#[cfg(target_os = "android")]
pub fn read() -> Appearance {
    use jni::objects::JFloatArray;
    crate::java::with_activity("reading the look", |env, activity| {
        let all = env
            .call_method(activity, "krateAppearance", "()[F", &[])?
            .l()?;
        let all = JFloatArray::from(all);
        let mut out = vec![0f32; env.get_array_length(&all)? as usize];
        env.get_float_array_region(&all, 0, &mut out)?;
        Ok(from_parts(&out))
    })
    .unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::from_parts;

    #[test]
    fn the_configuration_reads_as_the_persons_look() {
        let look = from_parts(&[1.0, 0.0, 2.0]);
        assert!(look.dark && !look.high_contrast);
        assert_eq!(look.text_scale, 2.0);
        assert_eq!(from_parts(&[0.0, 1.0, f32::NAN]).text_scale, 1.0);
        assert!(from_parts(&[0.0, 1.0, 9.0]).high_contrast);
        assert_eq!(from_parts(&[0.0, 0.0, 9.0]).text_scale, 4.0);
        assert!(!from_parts(&[]).dark);
    }
}
