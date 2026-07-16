use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

#[derive(Clone, Debug)]
pub struct GatewayReadiness {
    inner: Arc<GatewayReadinessInner>,
}

#[derive(Debug)]
struct GatewayReadinessInner {
    event_loop_alive: AtomicBool,
    configuration_valid: AtomicBool,
    provider_healthy: AtomicBool,
}

impl GatewayReadiness {
    pub fn new(configuration_valid: bool) -> Self {
        Self {
            inner: Arc::new(GatewayReadinessInner {
                event_loop_alive: AtomicBool::new(true),
                configuration_valid: AtomicBool::new(configuration_valid),
                provider_healthy: AtomicBool::new(false),
            }),
        }
    }

    pub fn set_provider_healthy(&self, value: bool) {
        self.inner.provider_healthy.store(value, Ordering::Release);
    }

    pub fn stop_event_loop(&self) {
        self.inner.event_loop_alive.store(false, Ordering::Release);
    }

    pub fn healthy(&self) -> bool {
        self.inner.event_loop_alive.load(Ordering::Acquire)
    }

    pub fn ready(&self) -> Result<(), &'static str> {
        if !self.healthy() {
            return Err("RPC Gateway event loop stopped");
        }
        if !self.inner.configuration_valid.load(Ordering::Acquire) {
            return Err("RPC provider configuration is invalid");
        }
        if !self.inner.provider_healthy.load(Ordering::Acquire) {
            return Err("no RPC provider has passed a live Arbitrum probe");
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn readiness_requires_live_provider_evidence() {
        let readiness = GatewayReadiness::new(true);
        assert_eq!(
            readiness.ready(),
            Err("no RPC provider has passed a live Arbitrum probe")
        );
        readiness.set_provider_healthy(true);
        assert_eq!(readiness.ready(), Ok(()));
        readiness.stop_event_loop();
        assert_eq!(readiness.ready(), Err("RPC Gateway event loop stopped"));
    }
}
