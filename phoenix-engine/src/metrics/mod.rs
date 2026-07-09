use std::collections::BTreeMap;

#[derive(Clone, Debug, Default)]
pub struct Metrics {
    counters: BTreeMap<&'static str, u64>,
}

impl Metrics {
    pub fn inc(&mut self, name: &'static str) {
        *self.counters.entry(name).or_insert(0) += 1;
    }

    pub fn get(&self, name: &'static str) -> u64 {
        self.counters.get(name).copied().unwrap_or(0)
    }

    pub fn hot_path_external_rpc_calls_total(&self) -> u64 {
        self.get("hot_path_external_rpc_calls_total")
    }
}

pub const REQUIRED_COUNTERS: &[&str] = &[
    "feed_transactions_total",
    "supported_origins_total",
    "affected_routes_total",
    "route_simulations_total",
    "profitable_opportunities_total",
    "opportunities_submitted_total",
    "execution_receipt_success_total",
    "opportunities_settled_total",
    "realized_profit_total",
    "hot_path_external_rpc_calls_total",
];
