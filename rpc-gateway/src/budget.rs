use std::time::{Duration, Instant};

const TOKEN_SCALE: u128 = 1_000_000_000;

#[derive(Clone, Debug)]
pub struct TokenBucket {
    capacity_scaled: u128,
    tokens_scaled: u128,
    refill_tokens: u32,
    refill_period: Duration,
    last_refill: Instant,
}

impl TokenBucket {
    pub fn new(capacity: u32, refill_tokens: u32, refill_period: Duration, now: Instant) -> Self {
        let capacity_scaled = u128::from(capacity.max(1)) * TOKEN_SCALE;
        Self {
            capacity_scaled,
            tokens_scaled: capacity_scaled,
            refill_tokens: refill_tokens.max(1),
            refill_period: refill_period.max(Duration::from_nanos(1)),
            last_refill: now,
        }
    }

    pub fn try_take(&mut self, now: Instant) -> bool {
        self.refill(now);
        if self.tokens_scaled < TOKEN_SCALE {
            return false;
        }
        self.tokens_scaled -= TOKEN_SCALE;
        true
    }

    pub fn refill(&mut self, now: Instant) {
        let elapsed = now.saturating_duration_since(self.last_refill);
        let period_nanos = self.refill_period.as_nanos();
        let added = elapsed
            .as_nanos()
            .saturating_mul(u128::from(self.refill_tokens))
            .saturating_mul(TOKEN_SCALE)
            / period_nanos;
        if added > 0 {
            self.tokens_scaled = self
                .tokens_scaled
                .saturating_add(added)
                .min(self.capacity_scaled);
            self.last_refill = now;
        }
    }

    pub fn available(&self) -> u32 {
        (self.tokens_scaled / TOKEN_SCALE).min(u128::from(u32::MAX)) as u32
    }
}

#[derive(Clone, Debug)]
pub struct GlobalBudget {
    bucket: TokenBucket,
}

impl GlobalBudget {
    pub fn new(capacity: u32, refill_tokens: u32, refill_period: Duration, now: Instant) -> Self {
        Self {
            bucket: TokenBucket::new(capacity, refill_tokens, refill_period, now),
        }
    }

    pub fn admit(&mut self, now: Instant) -> bool {
        self.bucket.try_take(now)
    }

    pub fn available(&mut self, now: Instant) -> u32 {
        self.bucket.refill(now);
        self.bucket.available()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn burst_capacity_and_sustained_rate_are_independent() {
        let now = Instant::now();
        let mut budget = GlobalBudget::new(4, 1, Duration::from_secs(1), now);
        for _ in 0..4 {
            assert!(budget.admit(now));
        }
        assert!(!budget.admit(now));
        assert!(budget.admit(now + Duration::from_secs(1)));
        assert!(!budget.admit(now + Duration::from_secs(1)));
    }

    #[test]
    fn per_minute_budget_refills_continuously() {
        let now = Instant::now();
        let mut budget = GlobalBudget::new(12, 12, Duration::from_secs(60), now);
        for _ in 0..12 {
            assert!(budget.admit(now));
        }
        assert!(!budget.admit(now));
        assert!(budget.admit(now + Duration::from_secs(5)));
    }
}
