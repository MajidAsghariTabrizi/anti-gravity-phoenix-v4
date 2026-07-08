use phoenix_engine::execution::ExecutionMode;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DeterministicClock {
    now_unix_ms: u64,
}

impl DeterministicClock {
    pub fn new(now_unix_ms: u64) -> Self {
        Self { now_unix_ms }
    }

    pub fn now_unix_ms(&self) -> u64 {
        self.now_unix_ms
    }

    pub fn advance_ms(&mut self, delta: u64) {
        self.now_unix_ms = self.now_unix_ms.saturating_add(delta);
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ReplayConfig {
    pub fixture: String,
    pub execution_mode: ExecutionMode,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn deterministic_clock_advances_explicitly() {
        let mut clock = DeterministicClock::new(100);
        clock.advance_ms(5);
        assert_eq!(clock.now_unix_ms(), 105);
    }
}

