use std::collections::{HashMap, HashSet};

#[derive(Clone, Debug, Default)]
pub struct Coalescer {
    in_flight: HashSet<String>,
    waiters: HashMap<String, u32>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CoalesceDecision {
    Leader,
    Follower,
}

impl Coalescer {
    pub fn enter(&mut self, key: &str) -> CoalesceDecision {
        if self.in_flight.insert(key.to_string()) {
            CoalesceDecision::Leader
        } else {
            *self.waiters.entry(key.to_string()).or_insert(0) += 1;
            CoalesceDecision::Follower
        }
    }

    pub fn finish(&mut self, key: &str) -> u32 {
        self.in_flight.remove(key);
        self.waiters.remove(key).unwrap_or(0)
    }
}
