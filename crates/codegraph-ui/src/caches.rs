//! Payload memos keyed on the index's revision.
//!
//! Upstream memoises the blast scale, the entry points and the map (8 entries,
//! keyed on `lastIndexedAt` plus edge/file counts) because each costs a scan the
//! index has to change to invalidate. The same keys apply here; a re-index or a
//! sync that only moved edges changes the key, so a stale denominator is never
//! served. Flow, screens and source are never cached: their answers depend on
//! the bytes on disk, which change without the index changing.

use std::collections::VecDeque;
use std::sync::Mutex;

use serde_json::Value;

/// Entries kept per memo.
const CAPACITY: usize = 8;

/// One small LRU of `key → payload`.
#[derive(Default)]
pub struct Memo {
    entries: Mutex<VecDeque<(String, Value)>>,
}

impl Memo {
    pub fn get(&self, key: &str) -> Option<Value> {
        let mut entries = self.entries.lock().ok()?;
        let at = entries.iter().position(|(k, _)| k == key)?;
        let entry = entries.remove(at)?;
        let value = entry.1.clone();
        entries.push_front(entry);
        Some(value)
    }

    pub fn put(&self, key: String, value: Value) {
        if let Ok(mut entries) = self.entries.lock() {
            entries.retain(|(k, _)| k != &key);
            entries.push_front((key, value));
            entries.truncate(CAPACITY);
        }
    }

    pub fn clear(&self) {
        if let Ok(mut entries) = self.entries.lock() {
            entries.clear();
        }
    }
}

/// Every memo the API keeps.
#[derive(Default)]
pub struct Caches {
    pub blast_scale: Memo,
    pub frameworks: Memo,
    pub entrypoints: Memo,
    pub map: Memo,
    pub highlight: crate::highlight::HighlightCache,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn keeps_the_most_recent_entries() {
        let memo = Memo::default();
        for i in 0..10 {
            memo.put(format!("k{i}"), Value::from(i));
        }
        assert!(memo.get("k0").is_none());
        assert_eq!(memo.get("k9"), Some(Value::from(9)));
        assert_eq!(memo.get("k2"), Some(Value::from(2)));
    }
}
