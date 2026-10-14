//! `krate:store/kv` in a tab (IC-956 Phase 1): the desktop store's rules
//! (`krate_adapter_common::kv`) over a copy the page loaded from IndexedDB
//! before the app started.
//!
//! The worker cannot wait on IndexedDB itself (it is blocked in the app), so
//! a write is judged here first, then the page writes it through and
//! answers over the bridge, and only then does this copy take it: an app
//! never reads back a value that did not reach the browser's storage.

use std::collections::BTreeMap;

use krate_adapter_common::kv::{valid_key, MAX_TOTAL_BYTES, MAX_VALUE_BYTES};
use serde_json::{json, Value};

/// The WIT `store-error` cases, in jco's shape.
fn denied() -> Value {
    json!({ "tag": "denied" })
}
fn invalid_key() -> Value {
    json!({ "tag": "invalid-key" })
}
fn too_large() -> Value {
    json!({ "tag": "too-large" })
}

/// One app's store, as this run sees it.
#[derive(Debug, Default)]
pub struct WebKv {
    entries: BTreeMap<String, Vec<u8>>,
    granted: bool,
}

impl WebKv {
    /// `granted` comes from the session's grants, never from the app.
    pub fn new(granted: bool) -> Self {
        Self {
            entries: BTreeMap::new(),
            granted,
        }
    }

    /// One entry the page read from the browser's storage at start. An
    /// entry that breaks the rules (written by something else) is skipped.
    pub fn load(&mut self, key: &str, value: Vec<u8>) {
        if valid_key(key) && value.len() <= MAX_VALUE_BYTES {
            self.entries.insert(key.to_string(), value);
        }
    }

    fn require(&self, key: Option<&str>) -> Result<(), Value> {
        if !self.granted {
            return Err(denied());
        }
        match key {
            Some(key) if !valid_key(key) => Err(invalid_key()),
            _ => Ok(()),
        }
    }

    pub fn get(&self, key: &str) -> Result<Option<Vec<u8>>, Value> {
        self.require(Some(key))?;
        Ok(self.entries.get(key).cloned())
    }

    /// Whether `set(key, <len bytes>)` may go ahead: the grant, the key, the
    /// value's bound and the store's, counting the replacement rather than
    /// the addition so overwriting a large value with a small one is never
    /// refused.
    pub fn check_set(&self, key: &str, len: usize) -> Result<(), Value> {
        self.require(Some(key))?;
        if len > MAX_VALUE_BYTES {
            return Err(too_large());
        }
        let existing = self.entries.get(key).map(Vec::len).unwrap_or(0);
        let total: usize = self.entries.values().map(Vec::len).sum();
        if total - existing + len > MAX_TOTAL_BYTES {
            return Err(too_large());
        }
        Ok(())
    }

    /// The page wrote it through; this copy takes it.
    pub fn commit_set(&mut self, key: &str, value: Vec<u8>) {
        self.entries.insert(key.to_string(), value);
    }

    pub fn check_delete(&self, key: &str) -> Result<bool, Value> {
        self.require(Some(key))?;
        Ok(self.entries.contains_key(key))
    }

    pub fn commit_delete(&mut self, key: &str) {
        self.entries.remove(key);
    }

    /// Sorted, as the desktop's listing is.
    pub fn keys(&self) -> Result<Vec<String>, Value> {
        self.require(None)?;
        Ok(self.entries.keys().cloned().collect())
    }

    pub fn check_clear(&self) -> Result<(), Value> {
        self.require(None)
    }

    pub fn commit_clear(&mut self) {
        self.entries.clear();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_desktop_rules_hold_in_a_tab() {
        let mut kv = WebKv::new(true);
        kv.load("list", b"milk".to_vec());
        kv.load("a/b", b"skipped".to_vec());
        assert_eq!(kv.get("list"), Ok(Some(b"milk".to_vec())));
        assert_eq!(kv.keys(), Ok(vec!["list".to_string()]));
        assert_eq!(kv.get("a/b"), Err(json!({ "tag": "invalid-key" })));
        assert_eq!(
            kv.check_set("big", MAX_VALUE_BYTES + 1),
            Err(json!({ "tag": "too-large" }))
        );
        assert_eq!(kv.check_set("ok", 10), Ok(()));
        kv.commit_set("ok", vec![1; 10]);
        assert_eq!(kv.check_delete("ok"), Ok(true));
        kv.commit_delete("ok");
        assert_eq!(kv.get("ok"), Ok(None));

        let denied = WebKv::new(false);
        assert_eq!(denied.get("list"), Err(json!({ "tag": "denied" })));
        assert_eq!(denied.keys(), Err(json!({ "tag": "denied" })));
        assert_eq!(denied.check_set("x", 1), Err(json!({ "tag": "denied" })));
    }

    #[test]
    fn the_store_as_a_whole_is_bounded() {
        let mut kv = WebKv::new(true);
        for i in 0..16 {
            kv.commit_set(&format!("k{i}"), vec![0; MAX_VALUE_BYTES]);
        }
        assert_eq!(
            kv.check_set("one-more", 1),
            Err(json!({ "tag": "too-large" }))
        );
        // Replacing a value with a smaller one is never refused.
        assert_eq!(kv.check_set("k0", 1), Ok(()));
    }
}
