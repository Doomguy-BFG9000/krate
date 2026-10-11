//! window_clipboard's Krate platform (IC-932): the person's clipboard
//! through krate:ui/clipboard, behind the `ui.clipboard:read` and
//! `ui.clipboard:write` grants. Copied in as `platform/krate.rs` by
//! crates/clipboard-krate/make-clipboard.py.

use crate::ClipboardProvider;

use raw_window_handle::HasDisplayHandle;

struct Krate;

pub fn connect<W: HasDisplayHandle>(
    _window: &W,
) -> Result<Box<dyn ClipboardProvider>, Box<dyn std::error::Error>> {
    Ok(Box::new(Krate))
}

impl ClipboardProvider for Krate {
    fn read(&self) -> Result<String, Box<dyn std::error::Error>> {
        krate::ui::clipboard::read_text().map_err(|e| format!("the clipboard: {e:?}").into())
    }

    fn write(&mut self, contents: String) -> Result<(), Box<dyn std::error::Error>> {
        krate::ui::clipboard::write_text(&contents)
            .map_err(|e| format!("the clipboard: {e:?}").into())
    }
}
