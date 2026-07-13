use std::collections::BTreeMap;
use std::fmt::Write;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};

use crate::economic::RpcMethod;
use crate::runtime_state::GatewayReadiness;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ProviderSlot {
    Primary,
    Secondary,
    Probe,
}

impl ProviderSlot {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Primary => "primary",
            Self::Secondary => "secondary",
            Self::Probe => "probe",
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum UpstreamOutcome {
    Success,
    Timeout,
    RateLimited,
    Failure,
}

impl UpstreamOutcome {
    const fn as_str(self) -> &'static str {
        match self {
            Self::Success => "success",
            Self::Timeout => "timeout",
            Self::RateLimited => "rate_limited",
            Self::Failure => "failure",
        }
    }
}

#[derive(Clone, Debug, Default)]
pub struct RuntimeRpcMetrics {
    inner: Arc<RuntimeMetricValues>,
}

#[derive(Debug, Default)]
struct RuntimeMetricValues {
    state_requests: AtomicU64,
    state_request_budget_rejected: AtomicU64,
    upstream_call_budget_rejected: AtomicU64,
    multicall_requests: AtomicU64,
    multicall_inner_calls: AtomicU64,
    static_metadata_cache_hits: AtomicU64,
    route_block_cache_hits: AtomicU64,
    coalesced_requests: AtomicU64,
    secondary_verifications: AtomicU64,
    provider_rate_limited: AtomicU64,
    provider_cooldown: AtomicU64,
    probe_calls: AtomicU64,
    provider_disagreement: AtomicU64,
    upstream_calls: Mutex<BTreeMap<(&'static str, &'static str, &'static str), u64>>,
}

impl RuntimeRpcMetrics {
    pub fn state_request(&self) {
        self.inner.state_requests.fetch_add(1, Ordering::Relaxed);
    }

    pub fn state_request_budget_rejected(&self) {
        self.inner
            .state_request_budget_rejected
            .fetch_add(1, Ordering::Relaxed);
    }

    pub fn upstream_call_budget_rejected(&self) {
        self.inner
            .upstream_call_budget_rejected
            .fetch_add(1, Ordering::Relaxed);
    }

    pub fn upstream_call(&self, method: RpcMethod, outcome: UpstreamOutcome, slot: ProviderSlot) {
        let mut calls = self
            .inner
            .upstream_calls
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        *calls
            .entry((method.as_str(), outcome.as_str(), slot.as_str()))
            .or_insert(0) += 1;
    }

    pub fn multicall_request(&self, inner_calls: usize) {
        self.inner
            .multicall_requests
            .fetch_add(1, Ordering::Relaxed);
        self.inner
            .multicall_inner_calls
            .fetch_add(inner_calls as u64, Ordering::Relaxed);
    }

    pub fn static_metadata_cache_hit(&self) {
        self.inner
            .static_metadata_cache_hits
            .fetch_add(1, Ordering::Relaxed);
    }

    pub fn route_block_cache_hit(&self) {
        self.inner
            .route_block_cache_hits
            .fetch_add(1, Ordering::Relaxed);
    }

    pub fn coalesced_request(&self) {
        self.inner
            .coalesced_requests
            .fetch_add(1, Ordering::Relaxed);
    }

    pub fn secondary_verification(&self) {
        self.inner
            .secondary_verifications
            .fetch_add(1, Ordering::Relaxed);
    }

    pub fn provider_rate_limited(&self) {
        self.inner
            .provider_rate_limited
            .fetch_add(1, Ordering::Relaxed);
    }

    pub fn provider_cooldown(&self) {
        self.inner.provider_cooldown.fetch_add(1, Ordering::Relaxed);
    }

    pub fn probe_call(&self) {
        self.inner.probe_calls.fetch_add(1, Ordering::Relaxed);
    }

    pub fn provider_disagreement(&self) {
        self.inner
            .provider_disagreement
            .fetch_add(1, Ordering::Relaxed);
    }

    pub fn render(&self, readiness: &GatewayReadiness) -> String {
        let calls = self
            .inner
            .upstream_calls
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let mut output = String::new();
        output.push_str("# TYPE rpc_state_requests_total counter\n");
        let _ = writeln!(
            output,
            "rpc_state_requests_total {}",
            self.inner.state_requests.load(Ordering::Relaxed)
        );
        output.push_str("# TYPE rpc_state_request_budget_rejected_total counter\n");
        let _ = writeln!(
            output,
            "rpc_state_request_budget_rejected_total {}",
            self.inner
                .state_request_budget_rejected
                .load(Ordering::Relaxed)
        );
        output.push_str("# TYPE rpc_upstream_calls_total counter\n");
        for ((method, outcome, slot), value) in calls.iter() {
            let _ = writeln!(
                output,
                "rpc_upstream_calls_total{{method=\"{method}\",outcome=\"{outcome}\",provider_slot=\"{slot}\"}} {value}"
            );
        }
        output.push_str("# TYPE rpc_upstream_call_budget_rejected_total counter\n");
        let _ = writeln!(
            output,
            "rpc_upstream_call_budget_rejected_total {}",
            self.inner
                .upstream_call_budget_rejected
                .load(Ordering::Relaxed)
        );
        for (name, value) in [
            (
                "rpc_multicall_requests_total",
                self.inner.multicall_requests.load(Ordering::Relaxed),
            ),
            (
                "rpc_multicall_inner_calls_total",
                self.inner.multicall_inner_calls.load(Ordering::Relaxed),
            ),
            (
                "rpc_static_metadata_cache_hits_total",
                self.inner
                    .static_metadata_cache_hits
                    .load(Ordering::Relaxed),
            ),
            (
                "rpc_route_block_cache_hits_total",
                self.inner.route_block_cache_hits.load(Ordering::Relaxed),
            ),
            (
                "rpc_coalesced_requests_total",
                self.inner.coalesced_requests.load(Ordering::Relaxed),
            ),
            (
                "rpc_secondary_verifications_total",
                self.inner.secondary_verifications.load(Ordering::Relaxed),
            ),
            (
                "rpc_provider_rate_limited_total",
                self.inner.provider_rate_limited.load(Ordering::Relaxed),
            ),
            (
                "rpc_provider_cooldown_total",
                self.inner.provider_cooldown.load(Ordering::Relaxed),
            ),
            (
                "rpc_probe_calls_total",
                self.inner.probe_calls.load(Ordering::Relaxed),
            ),
            (
                "rpc_provider_disagreement_total",
                self.inner.provider_disagreement.load(Ordering::Relaxed),
            ),
        ] {
            let _ = writeln!(output, "# TYPE {name} counter");
            let _ = writeln!(output, "{name} {value}");
        }
        output.push_str("# TYPE rpc_gateway_readiness gauge\n");
        let _ = writeln!(
            output,
            "rpc_gateway_readiness {}",
            u8::from(readiness.ready().is_ok())
        );
        output
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
    "rpc_state_requests_total",
    "rpc_state_request_budget_rejected_total",
    "rpc_upstream_calls_total",
    "rpc_upstream_call_budget_rejected_total",
    "rpc_multicall_requests_total",
    "rpc_multicall_inner_calls_total",
    "rpc_static_metadata_cache_hits_total",
    "rpc_route_block_cache_hits_total",
    "rpc_coalesced_requests_total",
    "rpc_secondary_verifications_total",
    "rpc_provider_rate_limited_total",
    "rpc_provider_cooldown_total",
    "rpc_probe_calls_total",
    "rpc_provider_disagreement_total",
    "rpc_gateway_readiness",
];

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn runtime_metrics_are_exact_and_low_cardinality() {
        let metrics = RuntimeRpcMetrics::default();
        metrics.state_request();
        metrics.upstream_call(
            RpcMethod::EthCall,
            UpstreamOutcome::Success,
            ProviderSlot::Primary,
        );
        metrics.multicall_request(4);
        metrics.provider_disagreement();
        let readiness = GatewayReadiness::new(true);
        readiness.set_provider_healthy(true);
        let rendered = metrics.render(&readiness);
        for required in REQUIRED_RPC_METRICS {
            assert!(rendered.contains(required));
        }
        assert!(
            rendered.contains("method=\"eth_call\",outcome=\"success\",provider_slot=\"primary\"")
        );
        for forbidden in ["provider_url=", "tx_hash=", "pool_address=", "route="] {
            assert!(!rendered.contains(forbidden));
        }
    }
}
