//! Bounded LRU cache.
//!
//! Ports `upstream resolution/lru-cache.ts`. The upstream backs its cache
//! with JavaScript's insertion-ordered `Map`; we replicate the same eviction
//! semantics without an insertion-ordered map: a `HashMap` from each key to
//! its value and the stamp of its last use, plus a `BTreeMap` from stamp to
//! key, so a use and an eviction are O(log n) rather than a scan of a recency
//! queue (W5-08). On `set`, when full, the least-recently-used key (the
//! oldest stamp) is evicted (`lru-cache.ts:46-57`); `get` refreshes recency
//! by giving the key a new stamp (`lru-cache.ts:29-40`).

use std::borrow::Borrow;
use std::collections::{BTreeMap, HashMap};
use std::hash::Hash;

/// A plain LRU cache bounded to `max` entries.
///
/// Ports the upstream `LRUCache<K, V>` class (`lru-cache.ts:14-62`).
pub struct LruCache<K, V> {
    max: usize,
    store: HashMap<K, (V, u64)>,
    order: BTreeMap<u64, K>,
    /// The stamp the next use takes.
    next: u64,
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
            order: BTreeMap::new(),
            next: 0,
        }
    }

    /// Current entry count (`get size()`, `lru-cache.ts:25-27`).
    pub fn size(&self) -> usize {
        self.store.len()
    }

    /// Whether `key` is present (`has`, `lru-cache.ts:42-44`).
    pub fn has<Q>(&self, key: &Q) -> bool
    where
        K: Borrow<Q>,
        Q: Hash + Eq + ?Sized,
    {
        self.store.contains_key(key)
    }

    /// Get `key`, refreshing recency (`get`, `lru-cache.ts:29-40`). A
    /// borrowed key (`&str` for a `String` key) needs no allocation.
    pub fn get<Q>(&mut self, key: &Q) -> Option<V>
    where
        K: Borrow<Q>,
        Q: Hash + Eq + ?Sized,
    {
        let stamp = self.next;
        let (value, last) = self.store.get_mut(key)?;
        let value = value.clone();
        let previous = std::mem::replace(last, stamp);
        self.next += 1;
        if let Some(key) = self.order.remove(&previous) {
            self.order.insert(stamp, key);
        }
        Some(value)
    }

    /// Insert/update `key`, evicting the oldest entry when full
    /// (`set`, `lru-cache.ts:46-57`).
    pub fn set(&mut self, key: K, value: V) {
        let stamp = self.next;
        self.next += 1;
        if let Some((_, previous)) = self.store.get(&key) {
            self.order.remove(previous);
        } else if self.store.len() >= self.max {
            // Evict the oldest entry: the smallest stamp.
            if let Some((_, oldest)) = self.order.pop_first() {
                self.store.remove(&oldest);
            }
        }
        self.order.insert(stamp, key.clone());
        self.store.insert(key, (value, stamp));
    }

    /// Drop all entries (`clear`, `lru-cache.ts:59-61`).
    pub fn clear(&mut self) {
        self.store.clear();
        self.order.clear();
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

    /// W5-08: a `set` of a present key refreshes its recency as a `get`
    /// does, so the untouched key goes first.
    #[test]
    fn set_of_a_present_key_refreshes_recency() {
        let mut cache: LruCache<&str, i32> = LruCache::new(2);
        cache.set("a", 1);
        cache.set("b", 2);
        cache.set("a", 3);
        cache.set("c", 4); // evicts "b"
        assert!(!cache.has(&"b"));
        assert_eq!(cache.get(&"a"), Some(3));
        assert_eq!(cache.get(&"c"), Some(4));
    }

    /// W5-08: over many uses the evicted keys are always the least recently
    /// used ones, as a recency queue gives them.
    #[test]
    fn evicts_in_least_recent_use_order() {
        let mut cache: LruCache<u32, u32> = LruCache::new(3);
        let mut queue: std::collections::VecDeque<u32> = std::collections::VecDeque::new();
        let touch = |queue: &mut std::collections::VecDeque<u32>, key: u32| {
            queue.retain(|k| *k != key);
            queue.push_back(key);
        };
        for step in 0..200u32 {
            let key = (step * 7 + step / 3) % 8;
            if step % 3 == 0 {
                let hit = cache.get(&key);
                assert_eq!(hit.is_some(), queue.contains(&key), "step {step}");
                if hit.is_some() {
                    touch(&mut queue, key);
                }
            } else {
                if !queue.contains(&key) && queue.len() == 3 {
                    queue.pop_front();
                }
                touch(&mut queue, key);
                cache.set(key, step);
            }
            for key in 0..8 {
                assert_eq!(
                    cache.has(&key),
                    queue.contains(&key),
                    "step {step} key {key}"
                );
            }
        }
        cache.clear();
        assert_eq!(cache.size(), 0);
        cache.set(1, 1);
        assert_eq!(cache.get(&1), Some(1));
    }

    #[test]
    #[should_panic(expected = "positive")]
    fn rejects_zero_capacity() {
        let _ = LruCache::<&str, i32>::new(0);
    }
}
