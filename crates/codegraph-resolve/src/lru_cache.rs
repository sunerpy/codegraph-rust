//! Bounded LRU cache.
//!
//! Ports `upstream resolution/lru-cache.ts`. The upstream backs its cache
//! with JavaScript's insertion-ordered `Map`; we replicate the same eviction
//! semantics with an `IndexMap`-free approach: a `HashMap` for O(1) lookup plus
//! a `VecDeque` recency queue. On `set`, when full, the least-recently-used key
//! (front of the queue) is evicted (`lru-cache.ts:46-57`); `get` refreshes
//! recency by moving the key to the back (`lru-cache.ts:29-40`).

use std::collections::{HashMap, VecDeque};
use std::hash::Hash;

/// A plain LRU cache bounded to `max` entries.
///
/// Ports the upstream `LRUCache<K, V>` class (`lru-cache.ts:14-62`).
pub struct LruCache<K, V> {
    max: usize,
    store: HashMap<K, V>,
    order: VecDeque<K>,
}

impl<K, V> LruCache<K, V>
where
    K: Eq + Hash + Clone,
    V: Clone,
{
    /// Create a cache holding at most `max` entries.
    ///
    /// Mirrors the upstream constructor guard (`lru-cache.ts:18-23`): `max` must be
    /// a positive finite number.
    ///
    /// # Panics
    /// Panics when `max == 0`, matching the upstream thrown error.
    pub fn new(max: usize) -> Self {
        assert!(max > 0, "LruCache max must be a positive number, got {max}");
        Self {
            max,
            store: HashMap::new(),
            order: VecDeque::new(),
        }
    }

    /// Current entry count (`get size()`, `lru-cache.ts:25-27`).
    pub fn size(&self) -> usize {
        self.store.len()
    }

    /// Whether `key` is present (`has`, `lru-cache.ts:42-44`).
    pub fn has(&self, key: &K) -> bool {
        self.store.contains_key(key)
    }

    /// Get `key`, refreshing recency (`get`, `lru-cache.ts:29-40`).
    pub fn get(&mut self, key: &K) -> Option<V> {
        if let Some(value) = self.store.get(key).cloned() {
            self.touch(key);
            Some(value)
        } else {
            None
        }
    }

    /// Insert/update `key`, evicting the oldest entry when full
    /// (`set`, `lru-cache.ts:46-57`).
    pub fn set(&mut self, key: K, value: V) {
        if self.store.contains_key(&key) {
            self.touch(&key);
        } else if self.store.len() >= self.max {
            // Evict the oldest entry — front of the recency queue.
            while let Some(oldest) = self.order.pop_front() {
                if self.store.remove(&oldest).is_some() {
                    break;
                }
            }
            self.order.push_back(key.clone());
        } else {
            self.order.push_back(key.clone());
        }
        self.store.insert(key, value);
    }

    /// Drop all entries (`clear`, `lru-cache.ts:59-61`).
    pub fn clear(&mut self) {
        self.store.clear();
        self.order.clear();
    }

    /// Move `key` to the most-recently-used position.
    fn touch(&mut self, key: &K) {
        if let Some(pos) = self.order.iter().position(|k| k == key) {
            self.order.remove(pos);
        }
        self.order.push_back(key.clone());
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn evicts_least_recently_used_on_overflow() {
        let mut cache: LruCache<&str, i32> = LruCache::new(2);
        cache.set("a", 1);
        cache.set("b", 2);
        // Touch "a" so "b" becomes least-recently-used.
        assert_eq!(cache.get(&"a"), Some(1));
        cache.set("c", 3); // evicts "b"
        assert!(!cache.has(&"b"));
        assert!(cache.has(&"a"));
        assert!(cache.has(&"c"));
        assert_eq!(cache.size(), 2);
    }

    #[test]
    fn set_updates_existing_without_growing() {
        let mut cache: LruCache<&str, i32> = LruCache::new(2);
        cache.set("a", 1);
        cache.set("a", 9);
        assert_eq!(cache.get(&"a"), Some(9));
        assert_eq!(cache.size(), 1);
    }

    #[test]
    #[should_panic(expected = "positive")]
    fn rejects_zero_capacity() {
        let _ = LruCache::<&str, i32>::new(0);
    }
}
