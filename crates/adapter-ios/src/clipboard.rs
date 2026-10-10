//! The clipboard on iOS: UIPasteboard's general pasteboard, text only
//! (IC-918).
//!
//! It answered Unsupported until 2026-10-10, so an app made in Studio with
//! Copy and Paste buttons did nothing on an iPhone. The pasteboard is not
//! thread-safe, so both directions run on the main thread and the app's
//! thread waits for the answer. Reading shows iOS's own "pasted from"
//! banner, which is the person's signal that an app read it.

use krate_adapter_common::ui::UiAdapterError;

#[cfg(target_os = "ios")]
fn on_main<T: Send + 'static>(f: impl FnOnce() -> T + Send + 'static) -> Result<T, UiAdapterError> {
    let (tx, rx) = std::sync::mpsc::channel();
    dispatch2::DispatchQueue::main().exec_async(move || {
        let _ = tx.send(f());
    });
    rx.recv_timeout(std::time::Duration::from_secs(5))
        .map_err(|_| UiAdapterError::Unsupported("the clipboard did not answer".to_string()))
}

#[cfg(target_os = "ios")]
pub(crate) fn read_text() -> Result<String, UiAdapterError> {
    on_main(|| {
        let board = objc2_ui_kit::UIPasteboard::generalPasteboard();
        // SAFETY: read on the main thread, where UIKit expects it.
        unsafe { board.string() }
            .map(|s| s.to_string())
            .unwrap_or_default()
    })
}

#[cfg(target_os = "ios")]
pub(crate) fn write_text(text: &str) -> Result<(), UiAdapterError> {
    let text = text.to_string();
    on_main(move || {
        let board = objc2_ui_kit::UIPasteboard::generalPasteboard();
        let value = objc2_foundation::NSString::from_str(&text);
        // SAFETY: written on the main thread, where UIKit expects it.
        unsafe { board.setString(Some(&value)) };
    })
}

#[cfg(not(target_os = "ios"))]
pub(crate) fn read_text() -> Result<String, UiAdapterError> {
    Err(UiAdapterError::Unsupported(
        "the iOS clipboard exists only on iOS".to_string(),
    ))
}

#[cfg(not(target_os = "ios"))]
pub(crate) fn write_text(_text: &str) -> Result<(), UiAdapterError> {
    Err(UiAdapterError::Unsupported(
        "the iOS clipboard exists only on iOS".to_string(),
    ))
}
