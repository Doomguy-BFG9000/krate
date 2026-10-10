//! The clipboard on Android (IC-918): ClipboardManager's primary clip,
//! through the player activity. Off Android there is none.

use krate_adapter_common::ui::UiAdapterError;

pub(crate) fn read_text() -> Result<String, UiAdapterError> {
    crate::phone::read_clipboard().map_err(UiAdapterError::Unsupported)
}

pub(crate) fn write_text(text: &str) -> Result<(), UiAdapterError> {
    crate::phone::write_clipboard(text).map_err(UiAdapterError::Unsupported)
}
