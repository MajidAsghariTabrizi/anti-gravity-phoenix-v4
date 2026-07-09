use std::collections::BTreeMap;

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
    "rpc_latency_seconds",
];
