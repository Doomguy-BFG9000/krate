//! The rules `krate:store/kv` holds an app to, wherever the store lives: a
//! file the desktop runtime keeps, or IndexedDB in the browser Player
//! (IC-956). One place, so a key one host accepts is never refused by the
//! other and the bounds cannot drift.

/// Longest key an app may use. Long enough for a namespaced name like
/// `window.main.size`, short enough that a key cannot become a payload.
pub const MAX_KEY_BYTES: usize = 256;

/// Largest single value. Bounded because the store is for state, not for bulk
/// data: an app with megabytes to write wants a file capability and the user's
/// explicit consent to a folder, not a settings store.
pub const MAX_VALUE_BYTES: usize = 1024 * 1024;

/// Largest the whole store may grow, so a loop cannot fill the user's disk.
pub const MAX_TOTAL_BYTES: usize = 16 * 1024 * 1024;

/// A key must be a short, printable, path-free name.
///
/// Path separators and `..` are rejected even though keys never become paths on
/// their own, because the store file format and any future per-key layout should
/// not have to care whether a key could be read as a path.
pub fn valid_key(key: &str) -> bool {
    if key.is_empty() || key.len() > MAX_KEY_BYTES {
        return false;
    }
    if key.contains('/') || key.contains('\\') || key.contains("..") {
        return false;
    }
    !key.chars()
        .any(|c| c.is_control() || c == '\n' || c == '\t')
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn keys_are_short_printable_and_path_free() {
        assert!(valid_key("window.main.size"));
        assert!(!valid_key(""));
        assert!(!valid_key("a/b"));
        assert!(!valid_key("a\\b"));
        assert!(!valid_key(".."));
        assert!(!valid_key("a\tb"));
        assert!(!valid_key(&"k".repeat(MAX_KEY_BYTES + 1)));
        assert!(valid_key(&"k".repeat(MAX_KEY_BYTES)));
    }
}
