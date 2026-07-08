use std::collections::HashMap;
use std::time::{Duration, Instant};

#[derive(Clone, Debug)]
pub struct CacheEntry {
    pub value: String,
    expires_at: Instant,
}

#[derive(Clone, Debug, Default)]
pub struct TtlCache {
    entries: HashMap<String, CacheEntry>,
}

impl TtlCache {
    pub fn insert(&mut self, key: String, value: String, ttl: Duration, now: Instant) {
        self.entries.insert(
            key,
            CacheEntry {
                value,
                expires_at: now + ttl,
            },
        );
    }

    pub fn get(&mut self, key: &str, now: Instant) -> Option<String> {
        let expired = self
            .entries
            .get(key)
            .map(|entry| now >= entry.expires_at)
            .unwrap_or(false);
        if expired {
            self.entries.remove(key);
            return None;
        }
        self.entries.get(key).map(|entry| entry.value.clone())
    }
}

