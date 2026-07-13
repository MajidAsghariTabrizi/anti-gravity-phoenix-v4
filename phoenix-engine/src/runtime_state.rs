use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

#[derive(Clone, Debug)]
pub struct RuntimeReadiness {
    inner: Arc<ReadinessInner>,
}

#[derive(Debug)]
struct ReadinessInner {
    event_loop_alive: AtomicBool,
    postgres_connected: AtomicBool,
    schema_verified: AtomicBool,
    nats_connected: AtomicBool,
    stream_ready: AtomicBool,
    consumer_ready: AtomicBool,
    fetching_active: AtomicBool,
    persistence_healthy: AtomicBool,
    acknowledgements_healthy: AtomicBool,
    strategy_configured: AtomicBool,
    evaluation_dependencies_ready: AtomicBool,
    integrity_healthy: AtomicBool,
}

impl Default for RuntimeReadiness {
    fn default() -> Self {
        Self::new()
    }
}

impl RuntimeReadiness {
    pub fn new() -> Self {
        Self {
            inner: Arc::new(ReadinessInner {
                event_loop_alive: AtomicBool::new(true),
                postgres_connected: AtomicBool::new(false),
                schema_verified: AtomicBool::new(false),
                nats_connected: AtomicBool::new(false),
                stream_ready: AtomicBool::new(false),
                consumer_ready: AtomicBool::new(false),
                fetching_active: AtomicBool::new(false),
                persistence_healthy: AtomicBool::new(true),
                acknowledgements_healthy: AtomicBool::new(true),
                strategy_configured: AtomicBool::new(false),
                evaluation_dependencies_ready: AtomicBool::new(false),
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
            self.inner
                .persistence_healthy
                .store(false, Ordering::Release);
        }
    }

    pub fn set_schema_verified(&self, value: bool) {
        self.inner.schema_verified.store(value, Ordering::Release);
    }

    pub fn set_nats_connected(&self, value: bool) {
        self.inner.nats_connected.store(value, Ordering::Release);
        if !value {
            self.inner.stream_ready.store(false, Ordering::Release);
            self.inner.consumer_ready.store(false, Ordering::Release);
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

    pub fn set_strategy_configured(&self, value: bool) {
        self.inner
            .strategy_configured
            .store(value, Ordering::Release);
    }

    pub fn set_evaluation_dependencies_ready(&self, value: bool) {
        self.inner
            .evaluation_dependencies_ready
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
            return Err("Engine event loop stopped");
        }
        if !self.inner.postgres_connected.load(Ordering::Acquire) {
            return Err("PostgreSQL unavailable");
        }
        if !self.inner.schema_verified.load(Ordering::Acquire) {
            return Err("PostgreSQL schema not verified");
        }
        if !self.inner.nats_connected.load(Ordering::Acquire) {
            return Err("JetStream disconnected");
        }
        if !self.inner.stream_ready.load(Ordering::Acquire) {
            return Err("required Engine stream unavailable");
        }
        if !self.inner.consumer_ready.load(Ordering::Acquire) {
            return Err("durable Engine consumer unavailable");
        }
        if !self.inner.fetching_active.load(Ordering::Acquire) {
            return Err("Engine message fetching inactive");
        }
        if !self.inner.persistence_healthy.load(Ordering::Acquire) {
            return Err("Engine persistence unavailable");
        }
        if !self.inner.acknowledgements_healthy.load(Ordering::Acquire) {
            return Err("Engine acknowledgement failures persist");
        }
        if !self.inner.strategy_configured.load(Ordering::Acquire) {
            return Err("SHADOW route registry is empty");
        }
        if !self
            .inner
            .evaluation_dependencies_ready
            .load(Ordering::Acquire)
        {
            return Err("SHADOW evaluation dependencies unavailable");
        }
        if !self.inner.integrity_healthy.load(Ordering::Acquire) {
            return Err("terminal Engine integrity condition detected");
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ready_state() -> RuntimeReadiness {
        let readiness = RuntimeReadiness::new();
        readiness.set_postgres_connected(true);
        readiness.set_schema_verified(true);
        readiness.set_nats_connected(true);
        readiness.set_stream_ready(true);
        readiness.set_consumer_ready(true);
        readiness.set_fetching_active(true);
        readiness.set_persistence_healthy(true);
        readiness.set_strategy_configured(true);
        readiness.set_evaluation_dependencies_ready(true);
        readiness
    }

    #[test]
    fn readiness_is_fail_closed_until_every_runtime_dependency_is_real() {
        let readiness = RuntimeReadiness::new();
        assert_eq!(readiness.ready(), Err("PostgreSQL unavailable"));
        assert_eq!(ready_state().ready(), Ok(()));
    }

    #[test]
    fn transient_dependencies_recover_but_integrity_loss_is_sticky() {
        let readiness = ready_state();
        readiness.set_nats_connected(false);
        assert_eq!(readiness.ready(), Err("JetStream disconnected"));
        readiness.set_nats_connected(true);
        readiness.set_stream_ready(true);
        readiness.set_consumer_ready(true);
        readiness.set_fetching_active(true);
        assert_eq!(readiness.ready(), Ok(()));

        readiness.mark_integrity_loss();
        assert_eq!(
            readiness.ready(),
            Err("terminal Engine integrity condition detected")
        );
        readiness.set_evaluation_dependencies_ready(false);
        readiness.set_evaluation_dependencies_ready(true);
        assert_eq!(
            readiness.ready(),
            Err("terminal Engine integrity condition detected")
        );
    }
}
