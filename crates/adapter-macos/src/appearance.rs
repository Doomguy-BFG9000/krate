//! How the person asked things to look on a Mac (IC-925): Dark appearance
//! and Increase contrast. A Mac has no system text size for apps, so the
//! scale is 1.

use objc2::rc::Retained;
use objc2::runtime::{AnyClass, AnyObject, Bool};
use objc2::{msg_send, MainThreadMarker};
use objc2_app_kit::NSApplication;
use objc2_foundation::{NSArray, NSString};

use krate_adapter_common::ui::Appearance;

fn read(mtm: MainThreadMarker) -> Appearance {
    let app = NSApplication::sharedApplication(mtm);
    // SAFETY: NSApplication's effectiveAppearance, matched against the two
    // system appearance names; NSWorkspace's contrast setting.
    unsafe {
        let appearance: Retained<AnyObject> = msg_send![&*app, effectiveAppearance];
        let names = NSArray::from_retained_slice(&[
            NSString::from_str("NSAppearanceNameAqua"),
            NSString::from_str("NSAppearanceNameDarkAqua"),
        ]);
        let best: Option<Retained<NSString>> =
            msg_send![&*appearance, bestMatchFromAppearancesWithNames: &*names];
        let dark = best.is_some_and(|name| name.to_string() == "NSAppearanceNameDarkAqua");
        let high_contrast = AnyClass::get(c"NSWorkspace").is_some_and(|class| {
            let workspace: Retained<AnyObject> = msg_send![class, sharedWorkspace];
            let more: Bool = msg_send![&*workspace, accessibilityDisplayShouldIncreaseContrast];
            more.as_bool()
        });
        Appearance {
            dark,
            high_contrast,
            text_scale: 1.0,
        }
    }
}

/// The look now, read on the main thread.
pub(crate) fn current() -> Appearance {
    if let Some(mtm) = MainThreadMarker::new() {
        return read(mtm);
    }
    let (tx, rx) = std::sync::mpsc::channel();
    dispatch2::DispatchQueue::main().exec_async(move || {
        // SAFETY: the main queue runs on the main thread.
        let mtm = unsafe { MainThreadMarker::new_unchecked() };
        let _ = tx.send(read(mtm));
    });
    rx.recv_timeout(std::time::Duration::from_millis(500))
        .unwrap_or_default()
}
