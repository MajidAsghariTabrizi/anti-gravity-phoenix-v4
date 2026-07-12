use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

#[derive(Clone, Debug)]
pub struct Readiness {
    inner: Arc<ReadinessInner>,
}

#[derive(Debug)]
struct ReadinessInner {
    event_loop_alive: AtomicBool,
    postgres_connected: AtomicBool,
    schema_verified: AtomicBool,
    jetstream_connected: AtomicBool,
    stream_ready: AtomicBool,
    consumer_ready: AtomicBool,
    fetching_active: AtomicBool,
    persistence_healthy: AtomicBool,
    acknowledgements_healthy: AtomicBool,
    integrity_healthy: AtomicBool,
}

impl Default for Readiness {
    fn default() -> Self {
        Self::new()
    }
}

impl Readiness {
    pub fn new() -> Self {
        Self {
            inner: Arc::new(ReadinessInner {
                event_loop_alive: AtomicBool::new(true),
                postgres_connected: AtomicBool::new(false),
                schema_verified: AtomicBool::new(false),
                jetstream_connected: AtomicBool::new(false),
                stream_ready: AtomicBool::new(false),
                consumer_ready: AtomicBool::new(false),
                fetching_active: AtomicBool::new(false),
                persistence_healthy: AtomicBool::new(true),
                acknowledgements_healthy: AtomicBool::new(true),
                integrity_healthy: AtomicBool::new(true),
            }),
        }
    }

    pub fn set_postgres_connected(&self, value: bool) {
        self.inner
            .postgres_connected
            .store(value, Ordering::Release);
        if !value {
            self.inner.schema_verified.store(false, Ordering::Release);
        }
    }

    pub fn set_schema_verified(&self, value: bool) {
        self.inner.schema_verified.store(value, Ordering::Release);
    }

    pub fn set_jetstream_connected(&self, value: bool) {
        self.inner
            .jetstream_connected
            .store(value, Ordering::Release);
        if !value {
            self.inner.fetching_active.store(false, Ordering::Release);
        }
    }

    pub fn set_stream_ready(&self, value: bool) {
        self.inner.stream_ready.store(value, Ordering::Release);
    }

    pub fn set_consumer_ready(&self, value: bool) {
        self.inner.consumer_ready.store(value, Ordering::Release);
        if !value {
            self.inner.fetching_active.store(false, Ordering::Release);
        }
    }

    pub fn set_fetching_active(&self, value: bool) {
        self.inner.fetching_active.store(value, Ordering::Release);
    }

    pub fn set_persistence_healthy(&self, value: bool) {
        self.inner
            .persistence_healthy
            .store(value, Ordering::Release);
    }

    pub fn set_acknowledgements_healthy(&self, value: bool) {
        self.inner
            .acknowledgements_healthy
            .store(value, Ordering::Release);
    }

    pub fn mark_integrity_loss(&self) {
        self.inner.integrity_healthy.store(false, Ordering::Release);
    }

    pub fn stop_event_loop(&self) {
        self.inner.event_loop_alive.store(false, Ordering::Release);
    }

    pub fn healthy(&self) -> bool {
        self.inner.event_loop_alive.load(Ordering::Acquire)
    }

    pub fn ready(&self) -> Result<(), &'static str> {
        if !self.healthy() {
            return Err("recorder event loop stopped");
        }
        if !self.inner.postgres_connected.load(Ordering::Acquire) {
            return Err("PostgreSQL unavailable");
        }
        if !self.inner.schema_verified.load(Ordering::Acquire) {
            return Err("PostgreSQL schema not verified");
        }
        if !self.inner.jetstream_connected.load(Ordering::Acquire) {
            return Err("JetStream disconnected");
        }
        if !self.inner.stream_ready.load(Ordering::Acquire) {
            return Err("required JetStream stream unavailable");
        }
        if !self.inner.consumer_ready.load(Ordering::Acquire) {
            return Err("durable JetStream consumer unavailable");
        }
        if !self.inner.fetching_active.load(Ordering::Acquire) {
            return Err("JetStream message fetching inactive");
        }
        if !self.inner.persistence_healthy.load(Ordering::Acquire) {
            return Err("PostgreSQL persistence unavailable");
        }
        if !self.inner.acknowledgements_healthy.load(Ordering::Acquire) {
            return Err("JetStream acknowledgement failures persist");
        }
        if !self.inner.integrity_healthy.load(Ordering::Acquire) {
            return Err("terminal Recorder integrity condition detected");
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ready_state() -> Readiness {
        let readiness = Readiness::new();
        readiness.set_postgres_connected(true);
        readiness.set_schema_verified(true);
        readiness.set_jetstream_connected(true);
        readiness.set_stream_ready(true);
        readiness.set_consumer_ready(true);
        readiness.set_fetching_active(true);
        readiness
    }

    #[test]
    fn readiness_requires_database_stream_consumer_and_active_fetching() {
        let readiness = Readiness::new();
        assert_eq!(readiness.ready(), Err("PostgreSQL unavailable"));
        readiness.set_postgres_connected(true);
        readiness.set_schema_verified(true);
        readiness.set_jetstream_connected(true);
        readiness.set_stream_ready(true);
        readiness.set_consumer_ready(true);
        assert_eq!(
            readiness.ready(),
            Err("JetStream message fetching inactive")
        );
        readiness.set_fetching_active(true);
        assert_eq!(readiness.ready(), Ok(()));
    }

    #[test]
    fn transient_dependency_failures_recover_automatically() {
        let readiness = ready_state();
        readiness.set_jetstream_connected(false);
        assert_eq!(readiness.ready(), Err("JetStream disconnected"));
        readiness.set_jetstream_connected(true);
        readiness.set_fetching_active(true);
        assert_eq!(readiness.ready(), Ok(()));

        readiness.set_postgres_connected(false);
        assert_eq!(readiness.ready(), Err("PostgreSQL unavailable"));
        readiness.set_postgres_connected(true);
        readiness.set_schema_verified(true);
        assert_eq!(readiness.ready(), Ok(()));

        readiness.set_acknowledgements_healthy(false);
        assert_eq!(
            readiness.ready(),
            Err("JetStream acknowledgement failures persist")
        );
        readiness.set_acknowledgements_healthy(true);
        assert_eq!(readiness.ready(), Ok(()));
    }

    #[test]
    fn terminal_integrity_loss_stays_fail_closed() {
        let readiness = ready_state();
        readiness.mark_integrity_loss();
        assert_eq!(
            readiness.ready(),
            Err("terminal Recorder integrity condition detected")
        );
        readiness.set_jetstream_connected(false);
        readiness.set_jetstream_connected(true);
        readiness.set_fetching_active(true);
        assert_eq!(
            readiness.ready(),
            Err("terminal Recorder integrity condition detected")
        );
    }
}
