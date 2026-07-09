use std::time::{Duration, Instant};

#[derive(Clone, Debug)]
pub struct TokenBucket {
    capacity: u32,
    tokens: u32,
    refill_every: Duration,
    last_refill: Instant,
}

impl TokenBucket {
    pub fn new(capacity: u32, refill_every: Duration, now: Instant) -> Self {
        Self {
            capacity,
            tokens: capacity,
            refill_every,
            last_refill: now,
        }
    }

    pub fn try_take(&mut self, now: Instant) -> bool {
        self.refill(now);
        if self.tokens == 0 {
            return false;
        }
        self.tokens -= 1;
        true
    }

    pub fn refill(&mut self, now: Instant) {
        if now.duration_since(self.last_refill) >= self.refill_every {
            self.tokens = self.capacity;
            self.last_refill = now;
        }
    }

    pub fn available(&self) -> u32 {
        self.tokens
    }
}

#[derive(Clone, Debug)]
pub struct GlobalBudget {
    bucket: TokenBucket,
}

impl GlobalBudget {
    pub fn new(max_per_window: u32, refill_every: Duration, now: Instant) -> Self {
        Self {
            bucket: TokenBucket::new(max_per_window, refill_every, now),
        }
    }

    pub fn admit(&mut self, now: Instant) -> bool {
        self.bucket.try_take(now)
    }
}
