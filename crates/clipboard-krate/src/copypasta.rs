//! copypasta's Krate clipboard (IC-932): the person's clipboard through
//! krate:ui/clipboard, behind the `ui.clipboard:read` and `ui.clipboard:write`
//! grants. Copied in as `krate_clipboard.rs` by
//! crates/clipboard-krate/make-clipboard.py.

use crate::common::{ClipboardProvider, Result};

pub struct KrateClipboardContext;

impl KrateClipboardContext {
    pub fn new() -> Result<KrateClipboardContext> {
        Ok(KrateClipboardContext)
    }
}

impl ClipboardProvider for KrateClipboardContext {
    fn get_contents(&mut self) -> Result<String> {
        krate::ui::clipboard::read_text().map_err(|e| format!("the clipboard: {e:?}").into())
    }

    fn set_contents(&mut self, contents: String) -> Result<()> {
        krate::ui::clipboard::write_text(&contents)
            .map_err(|e| format!("the clipboard: {e:?}").into())
    }
}
