use std::collections::BTreeMap;
use std::fmt::Write;

use crate::opportunity::{Opportunity, ShadowDisposition, SimulationClassification};

#[derive(Clone, Debug, Default)]
pub struct Metrics {
    counters: BTreeMap<&'static str, u64>,
    gauges: BTreeMap<&'static str, f64>,
    rejection_reasons: BTreeMap<&'static str, u64>,
}

impl Metrics {
    pub fn inc(&mut self, name: &'static str) {
        *self.counters.entry(name).or_insert(0) += 1;
    }

    pub fn get(&self, name: &'static str) -> u64 {
        self.counters.get(name).copied().unwrap_or(0)
    }

    pub fn set_gauge(&mut self, name: &'static str, value: f64) {
        self.gauges.insert(name, value);
    }

    pub fn gauge(&self, name: &'static str) -> f64 {
        self.gauges.get(name).copied().unwrap_or(0.0)
    }

    pub fn record_candidate(&mut self, opportunity: &Opportunity) {
        self.inc("phoenix_candidates_total");
        self.inc("phoenix_opportunities_total");
        self.inc("phoenix_simulations_total");
        if opportunity.simulation.classification != SimulationClassification::Passed {
            self.inc("phoenix_simulation_failures_total");
        }
        match opportunity.decision.disposition {
            ShadowDisposition::Accepted => self.inc("phoenix_shadow_accepted_total"),
            ShadowDisposition::Rejected => {
                self.inc("phoenix_shadow_rejected_total");
                if let Some(reason) = opportunity.decision.primary_rejection_reason {
                    *self.rejection_reasons.entry(reason.as_str()).or_insert(0) += 1;
                }
            }
        }
        self.set_gauge(
            "phoenix_expected_gross_pnl",
            opportunity.economics.base.gross_spread.0 as f64,
        );
        self.set_gauge(
            "phoenix_expected_net_pnl",
            opportunity.economics.base.expected_net_pnl.0 as f64,
        );
        self.set_gauge(
            "phoenix_conservative_net_pnl",
            opportunity.economics.conservative.expected_net_pnl.0 as f64,
        );
        self.set_gauge(
            "phoenix_severe_net_pnl",
            opportunity.economics.severe.expected_net_pnl.0 as f64,
        );
        self.set_gauge(
            "phoenix_hypothetical_realized_pnl",
            opportunity
                .outcome
                .replay_pnl
                .map(|value| value.0 as f64)
                .unwrap_or(0.0),
        );
        self.set_gauge(
            "phoenix_opportunity_age_seconds",
            opportunity
                .decision
                .decided_at_unix_ms
                .saturating_sub(opportunity.identity.observed_at_unix_ms) as f64
                / 1_000.0,
        );
        self.set_gauge(
            "phoenix_detection_latency_seconds",
            opportunity.market.feed_to_detection_latency_ns as f64 / 1_000_000_000.0,
        );
        self.set_gauge(
            "phoenix_simulation_latency_seconds",
            opportunity.simulation.latency_ns as f64 / 1_000_000_000.0,
        );
        self.set_gauge(
            "phoenix_quote_staleness_seconds",
            opportunity.market.quote_age_ms as f64 / 1_000.0,
        );
    }

    pub fn hot_path_external_rpc_calls_total(&self) -> u64 {
        self.get("hot_path_external_rpc_calls_total")
    }

    pub fn render(&self) -> String {
        let mut output = String::new();
        for name in REQUIRED_COUNTERS {
            let _ = writeln!(output, "{name} {}", self.get(name));
        }
        for name in REQUIRED_GAUGES {
            let _ = writeln!(output, "{name} {}", self.gauge(name));
        }
        for (reason, count) in &self.rejection_reasons {
            let _ = writeln!(
                output,
                "phoenix_rejection_reason_total{{reason=\"{reason}\"}} {count}"
            );
        }
        output
    }
}

pub const REQUIRED_COUNTERS: &[&str] = &[
    "feed_normalized_transactions_total",
    "supported_origins_total",
    "affected_routes_total",
    "route_simulations_total",
    "profitable_opportunities_total",
    "opportunities_submitted_total",
    "execution_receipt_success_total",
    "opportunities_settled_total",
    "realized_profit_total",
    "hot_path_external_rpc_calls_total",
    "phoenix_candidates_total",
    "phoenix_opportunities_total",
    "phoenix_shadow_accepted_total",
    "phoenix_shadow_rejected_total",
    "phoenix_simulations_total",
    "phoenix_simulation_failures_total",
];

pub const REQUIRED_GAUGES: &[&str] = &[
    "phoenix_expected_gross_pnl",
    "phoenix_expected_net_pnl",
    "phoenix_conservative_net_pnl",
    "phoenix_severe_net_pnl",
    "phoenix_hypothetical_realized_pnl",
    "phoenix_opportunity_age_seconds",
    "phoenix_detection_latency_seconds",
    "phoenix_simulation_latency_seconds",
    "phoenix_rpc_latency_seconds",
    "phoenix_quote_staleness_seconds",
    "phoenix_strategy_readiness",
    "phoenix_shadow_readiness",
];

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn required_profitability_metrics_are_declared() {
        for required in [
            "phoenix_candidates_total",
            "phoenix_shadow_accepted_total",
            "phoenix_shadow_rejected_total",
            "phoenix_simulation_failures_total",
        ] {
            assert!(REQUIRED_COUNTERS.contains(&required));
        }
        for required in [
            "phoenix_expected_net_pnl",
            "phoenix_conservative_net_pnl",
            "phoenix_severe_net_pnl",
            "phoenix_shadow_readiness",
        ] {
            assert!(REQUIRED_GAUGES.contains(&required));
        }
    }

    #[test]
    fn renderer_has_no_high_cardinality_identity_labels() {
        let rendered = Metrics::default().render();
        for forbidden in ["tx_hash=", "wallet=", "opportunity_id=", "pool_address="] {
            assert!(!rendered.contains(forbidden));
        }
    }
}
