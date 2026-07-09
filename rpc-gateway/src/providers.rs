use std::time::{Duration, Instant};

use crate::budget::TokenBucket;

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum CircuitState {
    Closed,
    Open { until: Instant },
}

#[derive(Clone, Debug)]
pub struct Provider {
    pub name: String,
    pub url: String,
    pub weight: u32,
    pub health_score: i32,
    pub circuit: CircuitState,
    pub bucket: TokenBucket,
    pub consecutive_failures: u32,
}

impl Provider {
    pub fn new(name: String, url: String, weight: u32, now: Instant) -> Self {
        Self {
            name,
            url,
            weight: weight.max(1),
            health_score: 100,
            circuit: CircuitState::Closed,
            bucket: TokenBucket::new(weight.max(1), Duration::from_secs(1), now),
            consecutive_failures: 0,
        }
    }

    pub fn available(&mut self, now: Instant) -> bool {
        match self.circuit {
            CircuitState::Open { until } if now < until => return false,
            CircuitState::Open { .. } => self.circuit = CircuitState::Closed,
            CircuitState::Closed => {}
        }
        self.bucket.try_take(now)
    }

    pub fn record_success(&mut self) {
        self.consecutive_failures = 0;
        self.health_score = (self.health_score + 1).min(100);
    }

    pub fn record_failure(&mut self, now: Instant) {
        self.consecutive_failures += 1;
        self.health_score = (self.health_score - 20).max(0);
        if self.consecutive_failures >= 3 {
            self.circuit = CircuitState::Open {
                until: now + Duration::from_secs(30),
            };
        }
    }
}

#[derive(Clone, Debug, Default)]
pub struct ProviderPool {
    providers: Vec<Provider>,
}

impl ProviderPool {
    pub fn new(providers: Vec<Provider>) -> Self {
        Self { providers }
    }

    pub fn choose(&mut self, now: Instant) -> Option<&mut Provider> {
        let mut best_idx: Option<usize> = None;
        let mut best_score = i32::MIN;
        for (idx, provider) in self.providers.iter_mut().enumerate() {
            if !provider.available(now) {
                continue;
            }
            let score = provider.health_score + provider.weight as i32;
            if score > best_score {
                best_score = score;
                best_idx = Some(idx);
            }
        }
        best_idx.map(|idx| &mut self.providers[idx])
    }
}
