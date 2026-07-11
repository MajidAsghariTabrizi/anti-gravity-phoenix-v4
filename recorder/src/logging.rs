use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

#[derive(Clone, Debug)]
pub struct LogSampler {
    interval: Duration,
    states: Arc<Mutex<HashMap<&'static str, LogState>>>,
}

#[derive(Clone, Copy, Debug)]
struct LogState {
    last_emitted: Instant,
    suppressed: u64,
}

impl Default for LogSampler {
    fn default() -> Self {
        Self::new(Duration::from_secs(30))
    }
}

impl LogSampler {
    pub fn new(interval: Duration) -> Self {
        Self {
            interval,
            states: Arc::new(Mutex::new(HashMap::with_capacity(16))),
        }
    }

    pub fn sample(&self, class: &'static str) -> Option<u64> {
        let now = Instant::now();
        let mut states = self.states.lock().expect("log sampler mutex poisoned");
        match states.get_mut(class) {
            Some(state) if now.duration_since(state.last_emitted) < self.interval => {
                state.suppressed = state.suppressed.saturating_add(1);
                None
            }
            Some(state) => {
                let suppressed = state.suppressed;
                *state = LogState {
                    last_emitted: now,
                    suppressed: 0,
                };
                Some(suppressed)
            }
            None => {
                states.insert(
                    class,
                    LogState {
                        last_emitted: now,
                        suppressed: 0,
                    },
                );
                Some(0)
            }
        }
    }

    #[cfg(test)]
    pub fn class_count(&self) -> usize {
        self.states.lock().unwrap().len()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn identical_events_are_sampled_without_unbounded_state() {
        let sampler = LogSampler::new(Duration::from_secs(60));
        assert_eq!(sampler.sample("decode_failure"), Some(0));
        for _ in 0..100 {
            assert_eq!(sampler.sample("decode_failure"), None);
        }
        assert_eq!(sampler.class_count(), 1);
    }
}
