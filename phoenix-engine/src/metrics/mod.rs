use std::collections::BTreeMap;
use std::fmt::Write;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use std::time::Duration;

use crate::opportunity::{Opportunity, ShadowDisposition, SimulationClassification};
use crate::runtime_state::RuntimeReadiness;

#[derive(Clone, Debug, Default)]
pub struct RuntimeMetrics {
    inner: Arc<RuntimeMetricValues>,
}

#[derive(Debug, Default)]
struct RuntimeMetricValues {
    inputs_received: AtomicU64,
    inputs_processed: AtomicU64,
    candidates: AtomicU64,
    no_route: AtomicU64,
    shadow_accepted: AtomicU64,
    shadow_rejected: AtomicU64,
    processing_failures: AtomicU64,
    redeliveries: AtomicU64,
    duplicate_skips: AtomicU64,
    rpc_primary_screen_rejected: AtomicU64,
    rpc_secondary_skipped: AtomicU64,
    consumer_pending: AtomicU64,
    consumer_ack_pending: AtomicU64,
    processing_latency_nanos: AtomicU64,
}

impl RuntimeMetrics {
    pub fn input_received(&self, redelivery: bool) {
        self.inner.inputs_received.fetch_add(1, Ordering::Relaxed);
        if redelivery {
            self.inner.redeliveries.fetch_add(1, Ordering::Relaxed);
        }
    }

    pub fn input_processed(&self, latency: Duration) {
        self.inner.inputs_processed.fetch_add(1, Ordering::Relaxed);
        self.inner.processing_latency_nanos.store(
            latency.as_nanos().min(u64::MAX as u128) as u64,
            Ordering::Relaxed,
        );
    }

    pub fn candidates(&self, count: usize) {
        self.inner
            .candidates
            .fetch_add(count as u64, Ordering::Relaxed);
    }

    pub fn no_route(&self) {
        self.inner.no_route.fetch_add(1, Ordering::Relaxed);
    }

    pub fn shadow_accepted(&self, count: usize) {
        self.inner
            .shadow_accepted
            .fetch_add(count as u64, Ordering::Relaxed);
    }

    pub fn shadow_rejected(&self, count: usize) {
        self.inner
            .shadow_rejected
            .fetch_add(count as u64, Ordering::Relaxed);
    }

    pub fn processing_failure(&self) {
        self.inner
            .processing_failures
            .fetch_add(1, Ordering::Relaxed);
    }

    pub fn duplicate_skip(&self) {
        self.inner.duplicate_skips.fetch_add(1, Ordering::Relaxed);
    }

    pub fn rpc_primary_screen_rejected(&self) {
        self.inner
            .rpc_primary_screen_rejected
            .fetch_add(1, Ordering::Relaxed);
    }

    pub fn rpc_secondary_skipped(&self) {
        self.inner
            .rpc_secondary_skipped
            .fetch_add(1, Ordering::Relaxed);
    }

    pub fn set_consumer_state(&self, pending: u64, ack_pending: u64) {
        self.inner
            .consumer_pending
            .store(pending, Ordering::Relaxed);
        self.inner
            .consumer_ack_pending
            .store(ack_pending, Ordering::Relaxed);
    }

    pub fn render(&self, readiness: &RuntimeReadiness) -> String {
        let latency =
            self.inner.processing_latency_nanos.load(Ordering::Relaxed) as f64 / 1_000_000_000.0;
        format!(
            concat!(
                "# TYPE phoenix_engine_inputs_received_total counter\n",
                "phoenix_engine_inputs_received_total {}\n",
                "# TYPE phoenix_engine_inputs_processed_total counter\n",
                "phoenix_engine_inputs_processed_total {}\n",
                "# TYPE phoenix_engine_candidates_total counter\n",
                "phoenix_engine_candidates_total {}\n",
                "# TYPE phoenix_engine_no_route_total counter\n",
                "phoenix_engine_no_route_total {}\n",
                "# TYPE phoenix_engine_shadow_accepted_total counter\n",
                "phoenix_engine_shadow_accepted_total {}\n",
                "# TYPE phoenix_engine_shadow_rejected_total counter\n",
                "phoenix_engine_shadow_rejected_total {}\n",
                "# TYPE phoenix_engine_processing_failures_total counter\n",
                "phoenix_engine_processing_failures_total {}\n",
                "# TYPE phoenix_engine_redeliveries_total counter\n",
                "phoenix_engine_redeliveries_total {}\n",
                "# TYPE phoenix_engine_duplicate_skips_total counter\n",
                "phoenix_engine_duplicate_skips_total {}\n",
                "# TYPE rpc_primary_screen_rejected_total counter\n",
                "rpc_primary_screen_rejected_total {}\n",
                "# TYPE rpc_secondary_skipped_total counter\n",
                "rpc_secondary_skipped_total {}\n",
                "# TYPE phoenix_engine_consumer_pending gauge\n",
                "phoenix_engine_consumer_pending {}\n",
                "# TYPE phoenix_engine_consumer_ack_pending gauge\n",
                "phoenix_engine_consumer_ack_pending {}\n",
                "# TYPE phoenix_engine_processing_latency_seconds gauge\n",
                "phoenix_engine_processing_latency_seconds {:.9}\n",
                "# TYPE phoenix_engine_readiness gauge\n",
                "phoenix_engine_readiness {}\n"
            ),
            self.inner.inputs_received.load(Ordering::Relaxed),
            self.inner.inputs_processed.load(Ordering::Relaxed),
            self.inner.candidates.load(Ordering::Relaxed),
            self.inner.no_route.load(Ordering::Relaxed),
            self.inner.shadow_accepted.load(Ordering::Relaxed),
            self.inner.shadow_rejected.load(Ordering::Relaxed),
            self.inner.processing_failures.load(Ordering::Relaxed),
            self.inner.redeliveries.load(Ordering::Relaxed),
            self.inner.duplicate_skips.load(Ordering::Relaxed),
            self.inner
                .rpc_primary_screen_rejected
                .load(Ordering::Relaxed),
            self.inner.rpc_secondary_skipped.load(Ordering::Relaxed),
            self.inner.consumer_pending.load(Ordering::Relaxed),
            self.inner.consumer_ack_pending.load(Ordering::Relaxed),
            latency,
            u8::from(readiness.ready().is_ok()),
        )
    }
}

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

    #[test]
    fn runtime_renderer_declares_exact_durable_consumer_metrics() {
        let metrics = RuntimeMetrics::default();
        metrics.input_received(true);
        metrics.input_processed(Duration::from_millis(25));
        metrics.candidates(2);
        metrics.no_route();
        metrics.shadow_accepted(1);
        metrics.shadow_rejected(1);
        metrics.processing_failure();
        metrics.duplicate_skip();
        metrics.set_consumer_state(7, 3);
        let rendered = metrics.render(&RuntimeReadiness::new());
        for expected in [
            "phoenix_engine_inputs_received_total 1",
            "phoenix_engine_inputs_processed_total 1",
            "phoenix_engine_candidates_total 2",
            "phoenix_engine_no_route_total 1",
            "phoenix_engine_shadow_accepted_total 1",
            "phoenix_engine_shadow_rejected_total 1",
            "phoenix_engine_processing_failures_total 1",
            "phoenix_engine_redeliveries_total 1",
            "phoenix_engine_duplicate_skips_total 1",
            "phoenix_engine_consumer_pending 7",
            "phoenix_engine_consumer_ack_pending 3",
            "phoenix_engine_processing_latency_seconds 0.025000000",
            "phoenix_engine_readiness 0",
        ] {
            assert!(rendered.contains(expected), "missing metric: {expected}");
        }
        for forbidden in ["tx_hash=", "source_event_identity=", "pool_address="] {
            assert!(!rendered.contains(forbidden));
        }
    }
}
