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
    nats_connected: AtomicBool,
    subscription_active: AtomicBool,
    persistence_healthy: AtomicBool,
    delivery_healthy: AtomicBool,
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
                nats_connected: AtomicBool::new(false),
                subscription_active: AtomicBool::new(false),
                persistence_healthy: AtomicBool::new(true),
                delivery_healthy: AtomicBool::new(true),
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

    pub fn set_nats_connected(&self, value: bool) {
        self.inner.nats_connected.store(value, Ordering::Release);
    }

    pub fn set_subscription_active(&self, value: bool) {
        self.inner
            .subscription_active
            .store(value, Ordering::Release);
    }

    pub fn set_persistence_healthy(&self, value: bool) {
        self.inner
            .persistence_healthy
            .store(value, Ordering::Release);
    }

    pub fn mark_delivery_loss(&self) {
        self.inner.delivery_healthy.store(false, Ordering::Release);
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
        if !self.inner.nats_connected.load(Ordering::Acquire) {
            return Err("NATS disconnected");
        }
        if !self.inner.subscription_active.load(Ordering::Acquire) {
            return Err("NATS subscription inactive");
        }
        if !self.inner.delivery_healthy.load(Ordering::Acquire) {
            return Err("Core NATS delivery loss detected");
        }
        if !self.inner.persistence_healthy.load(Ordering::Acquire) {
            return Err("PostgreSQL persistence unavailable");
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn readiness_requires_database_schema_nats_and_subscription() {
        let readiness = Readiness::new();
        assert_eq!(readiness.ready(), Err("PostgreSQL unavailable"));
        readiness.set_postgres_connected(true);
        readiness.set_schema_verified(true);
        readiness.set_nats_connected(true);
        assert_eq!(readiness.ready(), Err("NATS subscription inactive"));
        readiness.set_subscription_active(true);
        assert_eq!(readiness.ready(), Ok(()));
    }

    #[test]
    fn dependency_disconnects_clear_readiness_and_recover() {
        let readiness = Readiness::new();
        readiness.set_postgres_connected(true);
        readiness.set_schema_verified(true);
        readiness.set_nats_connected(true);
        readiness.set_subscription_active(true);
        assert_eq!(readiness.ready(), Ok(()));

        readiness.set_nats_connected(false);
        assert_eq!(readiness.ready(), Err("NATS disconnected"));
        readiness.set_nats_connected(true);
        assert_eq!(readiness.ready(), Ok(()));

        readiness.set_postgres_connected(false);
        assert_eq!(readiness.ready(), Err("PostgreSQL unavailable"));
        readiness.set_postgres_connected(true);
        readiness.set_schema_verified(true);
        assert_eq!(readiness.ready(), Ok(()));
    }

    #[test]
    fn detected_core_nats_delivery_loss_stays_fail_closed() {
        let readiness = Readiness::new();
        readiness.set_postgres_connected(true);
        readiness.set_schema_verified(true);
        readiness.set_nats_connected(true);
        readiness.set_subscription_active(true);
        readiness.mark_delivery_loss();
        assert_eq!(readiness.ready(), Err("Core NATS delivery loss detected"));

        readiness.set_nats_connected(false);
        readiness.set_nats_connected(true);
        assert_eq!(readiness.ready(), Err("Core NATS delivery loss detected"));
    }
}
