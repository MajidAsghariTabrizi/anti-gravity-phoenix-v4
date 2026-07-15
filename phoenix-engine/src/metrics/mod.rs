use std::collections::BTreeMap;
use std::fmt::Write;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use crate::opportunity::{
    Opportunity, PrimaryProfitabilityStatus, ShadowDisposition, SimulationClassification,
};
use crate::origin::OriginMetricKind;
use crate::runtime_state::RuntimeReadiness;

#[derive(Clone, Debug, Default)]
pub struct RuntimeMetrics {
    inner: Arc<RuntimeMetricValues>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RuntimeExitMetric {
    Shutdown,
    FetchFailed,
    StoreFailed,
    AcknowledgementFailed,
    IntegrityFailure,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RouteExclusionMetric {
    IneligibleOrigin,
    UnsupportedOrigin,
    NoAffectedRoute,
    NotProfitable,
    PolicyRejected,
    DependencyUnavailable,
    IntegrityFailure,
}

impl RouteExclusionMetric {
    const fn as_str(self) -> &'static str {
        match self {
            Self::IneligibleOrigin => "ineligible_origin",
            Self::UnsupportedOrigin => "unsupported_origin",
            Self::NoAffectedRoute => "no_affected_route",
            Self::NotProfitable => "not_profitable",
            Self::PolicyRejected => "policy_rejected",
            Self::DependencyUnavailable => "dependency_unavailable",
            Self::IntegrityFailure => "integrity_failure",
        }
    }
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
    dependency_exhausted: AtomicU64,
    redeliveries: AtomicU64,
    duplicate_skips: AtomicU64,
    rpc_primary_screen_rejected: AtomicU64,
    rpc_secondary_skipped: AtomicU64,
    official_router_inputs: AtomicU64,
    supported_exact_input_inputs: AtomicU64,
    malformed_inputs: AtomicU64,
    configured_route_matches: AtomicU64,
    route_discovery_eligible: AtomicU64,
    origin_supported_direct_v3: AtomicU64,
    origin_supported_multicall: AtomicU64,
    origin_supported_universal_router_v3_exact_in: AtomicU64,
    origin_unsupported_exact_output: AtomicU64,
    origin_ambiguous_multi_swap: AtomicU64,
    origin_malformed_router_calldata: AtomicU64,
    origin_unknown_official_router_command: AtomicU64,
    consumer_pending: AtomicU64,
    consumer_ack_pending: AtomicU64,
    processing_latency_nanos: AtomicU64,
    persistence_latency_nanos: AtomicU64,
    retries: AtomicU64,
    recovered_retries: AtomicU64,
    terminal_integrity: AtomicU64,
    later_message_progress_after_quarantine: AtomicU64,
    quarantine_pending_progress: AtomicBool,
    runtime_exit_shutdown: AtomicU64,
    runtime_exit_fetch_failed: AtomicU64,
    runtime_exit_store_failed: AtomicU64,
    runtime_exit_acknowledgement_failed: AtomicU64,
    runtime_exit_integrity_failure: AtomicU64,
    bounded: Mutex<BoundedMetricValues>,
}

#[derive(Debug, Default)]
struct BoundedMetricValues {
    route_exclusions: BTreeMap<&'static str, u64>,
    profitability_rejections: BTreeMap<&'static str, u64>,
    primary_profitable: u64,
    primary_not_profitable: u64,
    primary_incomplete: u64,
    near_profitable: u64,
    expected_pnl_buckets: [u64; 4],
    conservative_pnl_buckets: [u64; 4],
    severe_pnl_buckets: [u64; 4],
    estimated_execution_gas_total: u128,
}

impl RuntimeMetrics {
    pub fn input_received(&self, redelivery: bool) {
        self.inner.inputs_received.fetch_add(1, Ordering::Relaxed);
        if redelivery {
            self.inner.redeliveries.fetch_add(1, Ordering::Relaxed);
            self.inner.retries.fetch_add(1, Ordering::Relaxed);
        }
    }

    pub fn input_processed(&self, latency: Duration) {
        self.inner.inputs_processed.fetch_add(1, Ordering::Relaxed);
        self.inner.processing_latency_nanos.store(
            latency.as_nanos().min(u64::MAX as u128) as u64,
            Ordering::Relaxed,
        );
        if self
            .inner
            .quarantine_pending_progress
            .swap(false, Ordering::AcqRel)
        {
            self.inner
                .later_message_progress_after_quarantine
                .fetch_add(1, Ordering::Relaxed);
        }
    }

    pub fn persistence_observed(&self, latency: Duration) {
        self.inner.persistence_latency_nanos.store(
            latency.as_nanos().min(u64::MAX as u128) as u64,
            Ordering::Relaxed,
        );
    }

    pub fn retry_recovered(&self) {
        self.inner
            .recovered_retries
            .fetch_add(1, Ordering::Relaxed);
    }

    pub fn candidates(&self, count: usize) {
        self.inner
            .candidates
            .fetch_add(count as u64, Ordering::Relaxed);
        self.inner
            .configured_route_matches
            .fetch_add(count as u64, Ordering::Relaxed);
        if count > 0 {
            self.inner
                .route_discovery_eligible
                .fetch_add(1, Ordering::Relaxed);
        }
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

    pub fn dependency_exhausted(&self) {
        self.inner
            .dependency_exhausted
            .fetch_add(1, Ordering::Relaxed);
        self.inner
            .quarantine_pending_progress
            .store(true, Ordering::Release);
    }

    pub fn terminal_integrity(&self) {
        self.inner
            .terminal_integrity
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

    pub fn origin_classified(&self, kind: OriginMetricKind) {
        self.inner
            .official_router_inputs
            .fetch_add(1, Ordering::Relaxed);
        let counter = match kind {
            OriginMetricKind::SupportedDirectV3 => {
                self.inner
                    .supported_exact_input_inputs
                    .fetch_add(1, Ordering::Relaxed);
                &self.inner.origin_supported_direct_v3
            }
            OriginMetricKind::SupportedMulticall => {
                self.inner
                    .supported_exact_input_inputs
                    .fetch_add(1, Ordering::Relaxed);
                &self.inner.origin_supported_multicall
            }
            OriginMetricKind::SupportedUniversalRouterV3ExactIn => {
                self.inner
                    .supported_exact_input_inputs
                    .fetch_add(1, Ordering::Relaxed);
                &self.inner.origin_supported_universal_router_v3_exact_in
            }
            OriginMetricKind::UnsupportedExactOutput => &self.inner.origin_unsupported_exact_output,
            OriginMetricKind::AmbiguousMultiSwap => &self.inner.origin_ambiguous_multi_swap,
            OriginMetricKind::MalformedRouterCalldata => {
                self.inner.malformed_inputs.fetch_add(1, Ordering::Relaxed);
                &self.inner.origin_malformed_router_calldata
            }
            OriginMetricKind::UnknownOfficialRouterCommand => {
                &self.inner.origin_unknown_official_router_command
            }
        };
        counter.fetch_add(1, Ordering::Relaxed);
    }

    pub fn route_ranking_exclusion(&self, reason: RouteExclusionMetric) {
        let mut bounded = self
            .inner
            .bounded
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        *bounded
            .route_exclusions
            .entry(reason.as_str())
            .or_insert(0) += 1;
    }

    pub fn profitability_without_candidate(&self, incomplete: bool) {
        let mut bounded = self
            .inner
            .bounded
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        if incomplete {
            bounded.primary_incomplete += 1;
        } else {
            bounded.primary_not_profitable += 1;
        }
    }

    pub fn record_profitability(&self, opportunity: &Opportunity) {
        let mut bounded = self
            .inner
            .bounded
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        match opportunity.economics.primary_status {
            PrimaryProfitabilityStatus::MeetsMinimum => bounded.primary_profitable += 1,
            PrimaryProfitabilityStatus::BelowMinimum => {
                bounded.primary_not_profitable += 1;
                if near_profitable(opportunity) {
                    bounded.near_profitable += 1;
                }
            }
            PrimaryProfitabilityStatus::Incomplete => bounded.primary_incomplete += 1,
        }
        if let Some(reason) = opportunity.decision.primary_rejection_reason {
            *bounded
                .profitability_rejections
                .entry(reason.as_str())
                .or_insert(0) += 1;
        }
        for reason in &opportunity.decision.secondary_rejection_reasons {
            *bounded
                .profitability_rejections
                .entry(reason.as_str())
                .or_insert(0) += 1;
        }
        let minimum = opportunity.economics.minimum_required_net_pnl.0;
        record_pnl_bucket(
            &mut bounded.expected_pnl_buckets,
            opportunity.economics.base.expected_net_pnl.0,
            minimum,
        );
        record_pnl_bucket(
            &mut bounded.conservative_pnl_buckets,
            opportunity.economics.conservative.expected_net_pnl.0,
            minimum,
        );
        record_pnl_bucket(
            &mut bounded.severe_pnl_buckets,
            opportunity.economics.severe.expected_net_pnl.0,
            minimum,
        );
        bounded.estimated_execution_gas_total = bounded
            .estimated_execution_gas_total
            .saturating_add(opportunity.economics.base.estimated_execution_gas as u128);
    }

    pub fn runtime_exit(&self, class: RuntimeExitMetric) {
        let counter = match class {
            RuntimeExitMetric::Shutdown => &self.inner.runtime_exit_shutdown,
            RuntimeExitMetric::FetchFailed => &self.inner.runtime_exit_fetch_failed,
            RuntimeExitMetric::StoreFailed => &self.inner.runtime_exit_store_failed,
            RuntimeExitMetric::AcknowledgementFailed => {
                &self.inner.runtime_exit_acknowledgement_failed
            }
            RuntimeExitMetric::IntegrityFailure => &self.inner.runtime_exit_integrity_failure,
        };
        counter.fetch_add(1, Ordering::Relaxed);
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
        let mut output = String::new();
        for (name, value) in [
            (
                "phoenix_engine_inputs_received_total",
                self.inner.inputs_received.load(Ordering::Relaxed),
            ),
            (
                "phoenix_engine_inputs_processed_total",
                self.inner.inputs_processed.load(Ordering::Relaxed),
            ),
            (
                "phoenix_engine_candidates_total",
                self.inner.candidates.load(Ordering::Relaxed),
            ),
            (
                "phoenix_engine_no_route_total",
                self.inner.no_route.load(Ordering::Relaxed),
            ),
            (
                "phoenix_engine_shadow_accepted_total",
                self.inner.shadow_accepted.load(Ordering::Relaxed),
            ),
            (
                "phoenix_engine_shadow_rejected_total",
                self.inner.shadow_rejected.load(Ordering::Relaxed),
            ),
            (
                "phoenix_engine_processing_failures_total",
                self.inner.processing_failures.load(Ordering::Relaxed),
            ),
            (
                "phoenix_engine_dependency_exhausted_total",
                self.inner.dependency_exhausted.load(Ordering::Relaxed),
            ),
            (
                "phoenix_engine_redeliveries_total",
                self.inner.redeliveries.load(Ordering::Relaxed),
            ),
            (
                "phoenix_engine_retries_total",
                self.inner.retries.load(Ordering::Relaxed),
            ),
            (
                "phoenix_engine_recovered_retries_total",
                self.inner.recovered_retries.load(Ordering::Relaxed),
            ),
            (
                "phoenix_engine_terminal_integrity_total",
                self.inner.terminal_integrity.load(Ordering::Relaxed),
            ),
            (
                "phoenix_engine_later_message_progress_after_quarantine_total",
                self.inner
                    .later_message_progress_after_quarantine
                    .load(Ordering::Relaxed),
            ),
            (
                "phoenix_engine_duplicate_skips_total",
                self.inner.duplicate_skips.load(Ordering::Relaxed),
            ),
            (
                "rpc_primary_screen_rejected_total",
                self.inner
                    .rpc_primary_screen_rejected
                    .load(Ordering::Relaxed),
            ),
            (
                "rpc_secondary_skipped_total",
                self.inner.rpc_secondary_skipped.load(Ordering::Relaxed),
            ),
            (
                "phoenix_official_router_inputs_total",
                self.inner.official_router_inputs.load(Ordering::Relaxed),
            ),
            (
                "phoenix_supported_exact_input_inputs_total",
                self.inner
                    .supported_exact_input_inputs
                    .load(Ordering::Relaxed),
            ),
            (
                "phoenix_malformed_inputs_total",
                self.inner.malformed_inputs.load(Ordering::Relaxed),
            ),
            (
                "phoenix_configured_route_matches_total",
                self.inner.configured_route_matches.load(Ordering::Relaxed),
            ),
            (
                "phoenix_route_discovery_eligible_total",
                self.inner.route_discovery_eligible.load(Ordering::Relaxed),
            ),
            (
                "phoenix_origin_supported_direct_v3_total",
                self.inner
                    .origin_supported_direct_v3
                    .load(Ordering::Relaxed),
            ),
            (
                "phoenix_origin_supported_multicall_total",
                self.inner
                    .origin_supported_multicall
                    .load(Ordering::Relaxed),
            ),
            (
                "phoenix_origin_supported_universal_router_v3_exact_in_total",
                self.inner
                    .origin_supported_universal_router_v3_exact_in
                    .load(Ordering::Relaxed),
            ),
            (
                "phoenix_origin_unsupported_exact_output_total",
                self.inner
                    .origin_unsupported_exact_output
                    .load(Ordering::Relaxed),
            ),
            (
                "phoenix_origin_ambiguous_multi_swap_total",
                self.inner
                    .origin_ambiguous_multi_swap
                    .load(Ordering::Relaxed),
            ),
            (
                "phoenix_origin_malformed_router_calldata_total",
                self.inner
                    .origin_malformed_router_calldata
                    .load(Ordering::Relaxed),
            ),
            (
                "phoenix_origin_unknown_official_router_command_total",
                self.inner
                    .origin_unknown_official_router_command
                    .load(Ordering::Relaxed),
            ),
        ] {
            write_counter(&mut output, name, value);
        }

        output.push_str("# TYPE phoenix_engine_runtime_exits_total counter\n");
        for (class, value) in [
            (
                "shutdown",
                self.inner.runtime_exit_shutdown.load(Ordering::Relaxed),
            ),
            (
                "fetch_failed",
                self.inner.runtime_exit_fetch_failed.load(Ordering::Relaxed),
            ),
            (
                "store_failed",
                self.inner.runtime_exit_store_failed.load(Ordering::Relaxed),
            ),
            (
                "acknowledgement_failed",
                self.inner
                    .runtime_exit_acknowledgement_failed
                    .load(Ordering::Relaxed),
            ),
            (
                "integrity_failure",
                self.inner
                    .runtime_exit_integrity_failure
                    .load(Ordering::Relaxed),
            ),
        ] {
            let _ = writeln!(
                output,
                "phoenix_engine_runtime_exits_total{{class=\"{class}\"}} {value}"
            );
        }

        let bounded = self
            .inner
            .bounded
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        output.push_str("# TYPE phoenix_route_ranking_exclusions_total counter\n");
        for (reason, value) in &bounded.route_exclusions {
            let _ = writeln!(
                output,
                "phoenix_route_ranking_exclusions_total{{reason=\"{reason}\"}} {value}"
            );
        }
        output.push_str("# TYPE phoenix_profitability_primary_total counter\n");
        for (status, value) in [
            ("profitable", bounded.primary_profitable),
            ("not_profitable", bounded.primary_not_profitable),
            ("incomplete", bounded.primary_incomplete),
        ] {
            let _ = writeln!(
                output,
                "phoenix_profitability_primary_total{{status=\"{status}\"}} {value}"
            );
        }
        write_counter(
            &mut output,
            "phoenix_profitability_near_profitable_total",
            bounded.near_profitable,
        );
        output.push_str("# TYPE phoenix_profitability_rejections_total counter\n");
        for (reason, value) in &bounded.profitability_rejections {
            let _ = writeln!(
                output,
                "phoenix_profitability_rejections_total{{reason=\"{reason}\"}} {value}"
            );
        }
        output.push_str("# TYPE phoenix_profitability_pnl_bucket_total counter\n");
        for (scenario, values) in [
            ("expected", bounded.expected_pnl_buckets),
            ("conservative", bounded.conservative_pnl_buckets),
            ("severe", bounded.severe_pnl_buckets),
        ] {
            for (bucket, value) in PNL_BUCKET_NAMES.iter().zip(values) {
                let _ = writeln!(
                    output,
                    "phoenix_profitability_pnl_bucket_total{{scenario=\"{scenario}\",bucket=\"{bucket}\"}} {value}"
                );
            }
        }
        output.push_str("# TYPE phoenix_profitability_estimated_execution_gas_total counter\n");
        let _ = writeln!(
            output,
            "phoenix_profitability_estimated_execution_gas_total {}",
            bounded.estimated_execution_gas_total
        );
        drop(bounded);

        write_integer_gauge(
            &mut output,
            "phoenix_engine_consumer_pending",
            self.inner.consumer_pending.load(Ordering::Relaxed),
        );
        write_integer_gauge(
            &mut output,
            "phoenix_engine_consumer_ack_pending",
            self.inner.consumer_ack_pending.load(Ordering::Relaxed),
        );
        write_gauge(
            &mut output,
            "phoenix_engine_processing_latency_seconds",
            self.inner.processing_latency_nanos.load(Ordering::Relaxed) as f64
                / 1_000_000_000.0,
        );
        write_gauge(
            &mut output,
            "phoenix_engine_persistence_latency_seconds",
            self.inner.persistence_latency_nanos.load(Ordering::Relaxed) as f64
                / 1_000_000_000.0,
        );
        write_integer_gauge(
            &mut output,
            "phoenix_engine_readiness",
            u64::from(u8::from(readiness.ready().is_ok())),
        );
        output
    }
}

const PNL_BUCKET_NAMES: [&str; 4] = [
    "non_positive",
    "below_minimum",
    "minimum_to_two_x",
    "at_least_two_x",
];

fn near_profitable(opportunity: &Opportunity) -> bool {
    let expected = opportunity.economics.base.expected_net_pnl.0;
    let minimum = opportunity.economics.minimum_required_net_pnl.0;
    let halfway = minimum / 2 + minimum % 2;
    minimum > 0 && expected > 0 && expected < minimum && expected >= halfway
}

fn record_pnl_bucket(buckets: &mut [u64; 4], value: i128, minimum: i128) {
    let index = if value <= 0 {
        0
    } else if minimum > 0 && value < minimum {
        1
    } else if minimum > 0 && minimum.checked_mul(2).is_some_and(|twice| value < twice) {
        2
    } else {
        3
    };
    buckets[index] = buckets[index].saturating_add(1);
}

fn write_counter(output: &mut String, name: &str, value: u64) {
    let _ = writeln!(output, "# TYPE {name} counter");
    let _ = writeln!(output, "{name} {value}");
}

fn write_integer_gauge(output: &mut String, name: &str, value: u64) {
    let _ = writeln!(output, "# TYPE {name} gauge");
    let _ = writeln!(output, "{name} {value}");
}

fn write_gauge(output: &mut String, name: &str, value: f64) {
    let _ = writeln!(output, "# TYPE {name} gauge");
    let _ = writeln!(output, "{name} {value:.9}");
}

#[derive(Clone, Debug, Default)]
pub struct Metrics {
    counters: BTreeMap<&'static str, u64>,
    gauges: BTreeMap<&'static str, f64>,
    financial_gauges: BTreeMap<&'static str, i128>,
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

    fn set_financial_gauge(&mut self, name: &'static str, value: i128) {
        self.financial_gauges.insert(name, value);
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
        self.set_financial_gauge(
            "phoenix_expected_gross_pnl",
            opportunity.economics.base.gross_spread.0,
        );
        self.set_financial_gauge(
            "phoenix_expected_net_pnl",
            opportunity.economics.base.expected_net_pnl.0,
        );
        self.set_financial_gauge(
            "phoenix_conservative_net_pnl",
            opportunity.economics.conservative.expected_net_pnl.0,
        );
        self.set_financial_gauge(
            "phoenix_severe_net_pnl",
            opportunity.economics.severe.expected_net_pnl.0,
        );
        self.set_financial_gauge(
            "phoenix_counterfactual_pnl",
            opportunity
                .outcome
                .replay_pnl
                .map(|value| value.0)
                .unwrap_or(0),
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
            if let Some(value) = self.financial_gauges.get(name) {
                let _ = writeln!(output, "{name} {value}");
            } else {
                let _ = writeln!(output, "{name} {}", self.gauge(name));
            }
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
    "phoenix_counterfactual_pnl",
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
    fn financial_gauges_preserve_integer_precision() {
        let mut metrics = Metrics::default();
        metrics.set_financial_gauge("phoenix_expected_net_pnl", 9_007_199_254_740_993);
        assert!(metrics
            .render()
            .contains("phoenix_expected_net_pnl 9007199254740993"));
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
        metrics.dependency_exhausted();
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
            "phoenix_engine_dependency_exhausted_total 1",
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

    #[test]
    fn runtime_renderer_exposes_only_bounded_origin_counters() {
        let metrics = RuntimeMetrics::default();
        for kind in [
            OriginMetricKind::SupportedDirectV3,
            OriginMetricKind::SupportedMulticall,
            OriginMetricKind::SupportedUniversalRouterV3ExactIn,
            OriginMetricKind::UnsupportedExactOutput,
            OriginMetricKind::AmbiguousMultiSwap,
            OriginMetricKind::MalformedRouterCalldata,
            OriginMetricKind::UnknownOfficialRouterCommand,
        ] {
            metrics.origin_classified(kind);
        }

        let rendered = metrics.render(&RuntimeReadiness::new());
        for expected in [
            "phoenix_origin_supported_direct_v3_total 1",
            "phoenix_origin_supported_multicall_total 1",
            "phoenix_origin_supported_universal_router_v3_exact_in_total 1",
            "phoenix_origin_unsupported_exact_output_total 1",
            "phoenix_origin_ambiguous_multi_swap_total 1",
            "phoenix_origin_malformed_router_calldata_total 1",
            "phoenix_origin_unknown_official_router_command_total 1",
        ] {
            assert!(rendered.contains(expected), "missing metric: {expected}");
        }
        for forbidden in ["router=", "selector=", "command=", "tx_hash=", "calldata="] {
            assert!(!rendered.contains(forbidden));
        }
    }

    #[test]
    fn runtime_renderer_exposes_bounded_money_path_lifecycle_metrics() {
        let metrics = RuntimeMetrics::default();
        metrics.input_received(true);
        metrics.retry_recovered();
        metrics.candidates(3);
        metrics.origin_classified(OriginMetricKind::SupportedDirectV3);
        metrics.profitability_without_candidate(true);
        metrics.profitability_without_candidate(false);
        metrics.route_ranking_exclusion(RouteExclusionMetric::NotProfitable);
        metrics.dependency_exhausted();
        metrics.input_processed(Duration::from_millis(5));
        metrics.persistence_observed(Duration::from_millis(2));
        metrics.runtime_exit(RuntimeExitMetric::FetchFailed);

        let rendered = metrics.render(&RuntimeReadiness::new());
        for expected in [
            "phoenix_engine_retries_total 1",
            "phoenix_engine_recovered_retries_total 1",
            "phoenix_engine_later_message_progress_after_quarantine_total 1",
            "phoenix_official_router_inputs_total 1",
            "phoenix_supported_exact_input_inputs_total 1",
            "phoenix_configured_route_matches_total 3",
            "phoenix_route_discovery_eligible_total 1",
            "phoenix_profitability_primary_total{status=\"not_profitable\"} 1",
            "phoenix_profitability_primary_total{status=\"incomplete\"} 1",
            "phoenix_route_ranking_exclusions_total{reason=\"not_profitable\"} 1",
            "phoenix_engine_runtime_exits_total{class=\"fetch_failed\"} 1",
            "phoenix_engine_persistence_latency_seconds 0.002000000",
        ] {
            assert!(rendered.contains(expected), "missing metric: {expected}");
        }
        for forbidden in [
            "tx_hash=",
            "route_id=",
            "provider_url=",
            "pool_address=",
            "source_event_identity=",
        ] {
            assert!(!rendered.contains(forbidden));
        }
    }

    #[test]
    fn pnl_buckets_are_fixed_and_relative_to_the_reviewed_minimum() {
        let mut buckets = [0_u64; 4];
        for value in [-1, 49, 100, 199, 200] {
            record_pnl_bucket(&mut buckets, value, 100);
        }
        assert_eq!(buckets, [1, 1, 2, 1]);
    }
}
