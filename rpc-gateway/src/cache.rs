use std::collections::HashMap;
use std::time::{Duration, Instant};

#[derive(Clone, Debug)]
pub struct CacheEntry {
    pub value: String,
    expires_at: Instant,
    generation: u64,
}

#[derive(Clone, Debug)]
pub struct TtlCache {
    entries: HashMap<String, CacheEntry>,
    capacity: usize,
    next_generation: u64,
}

impl Default for TtlCache {
    fn default() -> Self {
        Self::new(1024)
    }
}

impl TtlCache {
    pub fn new(capacity: usize) -> Self {
        Self {
            entries: HashMap::with_capacity(capacity.max(1)),
            capacity: capacity.max(1),
            next_generation: 0,
        }
    }

    pub fn insert(&mut self, key: String, value: String, ttl: Duration, now: Instant) {
        self.entries.retain(|_, entry| now < entry.expires_at);
        if !self.entries.contains_key(&key) && self.entries.len() >= self.capacity {
            let oldest = self
                .entries
                .iter()
                .min_by(|(left_key, left), (right_key, right)| {
                    left.expires_at
                        .cmp(&right.expires_at)
                        .then_with(|| left.generation.cmp(&right.generation))
                        .then_with(|| left_key.cmp(right_key))
                })
                .map(|(entry_key, _)| entry_key.clone());
            if let Some(oldest) = oldest {
                self.entries.remove(&oldest);
            }
        }
        self.next_generation = self.next_generation.saturating_add(1);
        self.entries.insert(
            key,
            CacheEntry {
                value,
                expires_at: now + ttl,
                generation: self.next_generation,
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

    pub fn len(&self) -> usize {
        self.entries.len()
    }

    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }
}
