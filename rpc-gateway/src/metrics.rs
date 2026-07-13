use std::collections::BTreeMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;

use crate::runtime_state::GatewayReadiness;

#[derive(Clone, Debug, Default)]
pub struct RuntimeRpcMetrics {
    inner: Arc<RuntimeMetricValues>,
}

#[derive(Debug, Default)]
struct RuntimeMetricValues {
    requests: AtomicU64,
    provider_requests: AtomicU64,
    cache_hits: AtomicU64,
    coalesced_requests: AtomicU64,
    rate_limit: AtomicU64,
    circuit_open: AtomicU64,
    budget_rejected: AtomicU64,
    provider_disagreement: AtomicU64,
    provider_timeout: AtomicU64,
    provider_retries: AtomicU64,
    archive_unavailable: AtomicU64,
    latency_nanos: AtomicU64,
}

impl RuntimeRpcMetrics {
    pub fn request(&self) {
        self.inner.requests.fetch_add(1, Ordering::Relaxed);
    }

    pub fn provider_request(&self) {
        self.inner.provider_requests.fetch_add(1, Ordering::Relaxed);
    }

    pub fn cache_hit(&self) {
        self.inner.cache_hits.fetch_add(1, Ordering::Relaxed);
    }

    pub fn coalesced_request(&self) {
        self.inner
            .coalesced_requests
            .fetch_add(1, Ordering::Relaxed);
    }

    pub fn rate_limited(&self) {
        self.inner.rate_limit.fetch_add(1, Ordering::Relaxed);
    }

    pub fn circuit_open(&self) {
        self.inner.circuit_open.fetch_add(1, Ordering::Relaxed);
    }

    pub fn budget_rejected(&self) {
        self.inner.budget_rejected.fetch_add(1, Ordering::Relaxed);
    }

    pub fn provider_disagreement(&self) {
        self.inner
            .provider_disagreement
            .fetch_add(1, Ordering::Relaxed);
    }

    pub fn provider_timeout(&self) {
        self.inner.provider_timeout.fetch_add(1, Ordering::Relaxed);
    }

    pub fn provider_retry(&self) {
        self.inner.provider_retries.fetch_add(1, Ordering::Relaxed);
    }

    pub fn archive_unavailable(&self) {
        self.inner
            .archive_unavailable
            .fetch_add(1, Ordering::Relaxed);
    }

    pub fn observe_latency(&self, latency_ns: u128) {
        self.inner
            .latency_nanos
            .store(latency_ns.min(u64::MAX as u128) as u64, Ordering::Relaxed);
    }

    pub fn render(&self, readiness: &GatewayReadiness) -> String {
        format!(
            concat!(
                "# TYPE rpc_requests_total counter\n",
                "rpc_requests_total {}\n",
                "# TYPE rpc_provider_requests_total counter\n",
                "rpc_provider_requests_total {}\n",
                "# TYPE rpc_cache_hits_total counter\n",
                "rpc_cache_hits_total {}\n",
                "# TYPE rpc_coalesced_requests_total counter\n",
                "rpc_coalesced_requests_total {}\n",
                "# TYPE rpc_rate_limit_total counter\n",
                "rpc_rate_limit_total {}\n",
                "# TYPE rpc_circuit_open_total counter\n",
                "rpc_circuit_open_total {}\n",
                "# TYPE rpc_budget_rejected_total counter\n",
                "rpc_budget_rejected_total {}\n",
                "# TYPE rpc_provider_disagreement_total counter\n",
                "rpc_provider_disagreement_total {}\n",
                "# TYPE rpc_provider_timeout_total counter\n",
                "rpc_provider_timeout_total {}\n",
                "# TYPE rpc_provider_retries_total counter\n",
                "rpc_provider_retries_total {}\n",
                "# TYPE rpc_archive_unavailable_total counter\n",
                "rpc_archive_unavailable_total {}\n",
                "# TYPE rpc_latency_seconds gauge\n",
                "rpc_latency_seconds {:.9}\n",
                "# TYPE rpc_gateway_readiness gauge\n",
                "rpc_gateway_readiness {}\n"
            ),
            self.inner.requests.load(Ordering::Relaxed),
            self.inner.provider_requests.load(Ordering::Relaxed),
            self.inner.cache_hits.load(Ordering::Relaxed),
            self.inner.coalesced_requests.load(Ordering::Relaxed),
            self.inner.rate_limit.load(Ordering::Relaxed),
            self.inner.circuit_open.load(Ordering::Relaxed),
            self.inner.budget_rejected.load(Ordering::Relaxed),
            self.inner.provider_disagreement.load(Ordering::Relaxed),
            self.inner.provider_timeout.load(Ordering::Relaxed),
            self.inner.provider_retries.load(Ordering::Relaxed),
            self.inner.archive_unavailable.load(Ordering::Relaxed),
            self.inner.latency_nanos.load(Ordering::Relaxed) as f64 / 1_000_000_000.0,
            u8::from(readiness.ready().is_ok()),
        )
    }
}

#[derive(Clone, Debug, Default)]
pub struct RpcMetrics {
    counters: BTreeMap<&'static str, u64>,
}

impl RpcMetrics {
    pub fn inc(&mut self, name: &'static str) {
        *self.counters.entry(name).or_insert(0) += 1;
    }

    pub fn get(&self, name: &'static str) -> u64 {
        self.counters.get(name).copied().unwrap_or_default()
    }
}

pub const REQUIRED_RPC_METRICS: &[&str] = &[
    "rpc_requests_total",
    "rpc_provider_requests_total",
    "rpc_cache_hits_total",
    "rpc_coalesced_requests_total",
    "rpc_rate_limit_total",
    "rpc_circuit_open_total",
    "rpc_budget_rejected_total",
    "rpc_provider_disagreement_total",
    "rpc_provider_timeout_total",
    "rpc_provider_retries_total",
    "rpc_archive_unavailable_total",
    "rpc_latency_seconds",
];

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn runtime_metrics_are_exact_and_low_cardinality() {
        let metrics = RuntimeRpcMetrics::default();
        metrics.request();
        metrics.provider_request();
        metrics.provider_disagreement();
        metrics.provider_timeout();
        metrics.provider_retry();
        metrics.observe_latency(25_000_000);
        let rendered = metrics.render(&GatewayReadiness::new(true));
        for required in REQUIRED_RPC_METRICS {
            assert!(rendered.contains(required));
        }
        assert!(rendered.contains("rpc_latency_seconds 0.025000000"));
        for forbidden in ["provider_url=", "tx_hash=", "pool_address="] {
            assert!(!rendered.contains(forbidden));
        }
    }
}
