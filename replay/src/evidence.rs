use std::collections::{BTreeMap, BTreeSet};
use std::fmt::Write;

use crate::{parse_cases, replay_cases, ReplayCase, ReplayDecision, ReplayError};

const BPS_DENOMINATOR: i128 = 10_000;
const BOOTSTRAP_ROUNDS: usize = 1_000;
const BOOTSTRAP_SEED: u64 = 0x5048_4f45_4e49_5801;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct EvidenceReport {
    pub sample_size: usize,
    pub independent_opportunity_count: usize,
    pub accepted_count: usize,
    pub rejected_count: usize,
    pub simulation_success_rate_bps: i128,
    pub mean_net_pnl: i128,
    pub median_net_pnl: i128,
    pub p25_net_pnl: i128,
    pub p75_net_pnl: i128,
    pub p95_net_pnl: i128,
    pub worst_case_pnl: i128,
    pub maximum_drawdown: i128,
    pub positive_outcome_rate_bps: i128,
    pub largest_opportunity_contribution_bps: i128,
    pub protocol_concentration_bps: i128,
    pub token_concentration_bps: i128,
    pub hourly_bucket_count: usize,
    pub daily_bucket_count: usize,
    pub base_aggregate_pnl: i128,
    pub conservative_aggregate_pnl: i128,
    pub severe_aggregate_pnl: i128,
    pub in_sample_median_pnl: i128,
    pub out_of_sample_median_pnl: i128,
    pub cluster_bootstrap_mean_ci_low: i128,
    pub cluster_bootstrap_mean_ci_high: i128,
    pub gas_sensitivity_delta: i128,
    pub slippage_sensitivity_delta: i128,
    pub latency_sensitivity_delta: i128,
}

impl EvidenceReport {
    pub fn render(&self) -> String {
        let mut output = String::new();
        let _ = writeln!(output, "evidence_schema=shadow-evidence-v1");
        let _ = writeln!(
            output,
            "financial_label=SHADOW_expected realization_status=not_realized"
        );
        let _ = writeln!(
            output,
            "sample_size={} independent_opportunities={} accepted={} rejected={} simulation_success_rate_bps={}",
            self.sample_size,
            self.independent_opportunity_count,
            self.accepted_count,
            self.rejected_count,
            self.simulation_success_rate_bps,
        );
        let _ = writeln!(
            output,
            "mean_net_pnl={} median_net_pnl={} p25_net_pnl={} p75_net_pnl={} p95_net_pnl={} worst_case_pnl={} maximum_drawdown={}",
            self.mean_net_pnl,
            self.median_net_pnl,
            self.p25_net_pnl,
            self.p75_net_pnl,
            self.p95_net_pnl,
            self.worst_case_pnl,
            self.maximum_drawdown,
        );
        let _ = writeln!(
            output,
            "positive_outcome_rate_bps={} largest_opportunity_contribution_bps={} protocol_concentration_bps={} token_concentration_bps={} hourly_buckets={} daily_buckets={}",
            self.positive_outcome_rate_bps,
            self.largest_opportunity_contribution_bps,
            self.protocol_concentration_bps,
            self.token_concentration_bps,
            self.hourly_bucket_count,
            self.daily_bucket_count,
        );
        let _ = writeln!(
            output,
            "base_aggregate_pnl={} conservative_aggregate_pnl={} severe_aggregate_pnl={} in_sample_median_pnl={} out_of_sample_median_pnl={}",
            self.base_aggregate_pnl,
            self.conservative_aggregate_pnl,
            self.severe_aggregate_pnl,
            self.in_sample_median_pnl,
            self.out_of_sample_median_pnl,
        );
        let _ = writeln!(
            output,
            "cluster_bootstrap_mean_ci_low={} cluster_bootstrap_mean_ci_high={} gas_sensitivity_delta={} slippage_sensitivity_delta={} latency_sensitivity_delta={}",
            self.cluster_bootstrap_mean_ci_low,
            self.cluster_bootstrap_mean_ci_high,
            self.gas_sensitivity_delta,
            self.slippage_sensitivity_delta,
            self.latency_sensitivity_delta,
        );
        output
    }
}

pub fn build(input: &str) -> Result<EvidenceReport, ReplayError> {
    let cases = parse_cases(input)?;
    let baseline = replay_cases(cases.clone())?;
    let decisions = &baseline.decisions;
    let base_values: Vec<i128> = decisions.iter().map(|item| item.base_net_pnl).collect();
    let counterfactual_values: Vec<i128> = decisions
        .iter()
        .map(|item| item.counterfactual_pnl)
        .collect();
    let split = if base_values.is_empty() {
        0
    } else {
        (base_values
            .len()
            .checked_mul(7)
            .ok_or(ReplayError::ArithmeticOverflow)?
            / 10)
            .clamp(1, base_values.len())
    };
    let cluster_values = cluster_pnl(decisions)?;
    let (ci_low, ci_high) = cluster_bootstrap_mean_ci(&cluster_values)?;

    Ok(EvidenceReport {
        sample_size: decisions.len(),
        independent_opportunity_count: cluster_values.len(),
        accepted_count: baseline.accepted_count(),
        rejected_count: baseline.rejected_count(),
        simulation_success_rate_bps: rate_bps(
            decisions
                .iter()
                .filter(|item| item.simulation_passed)
                .count(),
            decisions.len(),
        )?,
        mean_net_pnl: mean(&base_values)?,
        median_net_pnl: quantile(&base_values, 50)?,
        p25_net_pnl: quantile(&base_values, 25)?,
        p75_net_pnl: quantile(&base_values, 75)?,
        p95_net_pnl: quantile(&base_values, 95)?,
        worst_case_pnl: base_values.iter().copied().min().unwrap_or(0),
        maximum_drawdown: maximum_drawdown(&counterfactual_values)?,
        positive_outcome_rate_bps: rate_bps(
            counterfactual_values
                .iter()
                .filter(|value| **value > 0)
                .count(),
            counterfactual_values.len(),
        )?,
        largest_opportunity_contribution_bps: largest_contribution_bps(&counterfactual_values)?,
        protocol_concentration_bps: concentration_bps(
            decisions.iter().map(|item| item.protocol.as_str()),
        )?,
        token_concentration_bps: concentration_bps(
            decisions.iter().map(|item| item.token_pair.as_str()),
        )?,
        hourly_bucket_count: decisions
            .iter()
            .map(|item| item.observed_at_unix_ms / 3_600_000)
            .collect::<BTreeSet<_>>()
            .len(),
        daily_bucket_count: decisions
            .iter()
            .map(|item| item.observed_at_unix_ms / 86_400_000)
            .collect::<BTreeSet<_>>()
            .len(),
        base_aggregate_pnl: checked_sum(base_values.iter().copied())?,
        conservative_aggregate_pnl: checked_sum(
            decisions.iter().map(|item| item.conservative_net_pnl),
        )?,
        severe_aggregate_pnl: checked_sum(decisions.iter().map(|item| item.severe_net_pnl))?,
        in_sample_median_pnl: quantile(&base_values[..split], 50)?,
        out_of_sample_median_pnl: quantile(&base_values[split..], 50)?,
        cluster_bootstrap_mean_ci_low: ci_low,
        cluster_bootstrap_mean_ci_high: ci_high,
        gas_sensitivity_delta: sensitivity_delta(&cases, Sensitivity::Gas)?,
        slippage_sensitivity_delta: sensitivity_delta(&cases, Sensitivity::Slippage)?,
        latency_sensitivity_delta: sensitivity_delta(&cases, Sensitivity::Latency)?,
    })
}

#[derive(Clone, Copy)]
enum Sensitivity {
    Gas,
    Slippage,
    Latency,
}

fn sensitivity_delta(cases: &[ReplayCase], kind: Sensitivity) -> Result<i128, ReplayError> {
    let baseline = replay_cases(cases.to_vec())?
        .decisions
        .iter()
        .map(|item| item.base_net_pnl)
        .try_fold(0_i128, i128::checked_add)
        .ok_or(ReplayError::ArithmeticOverflow)?;
    let mut stressed = cases.to_vec();
    for case in &mut stressed {
        match kind {
            Sensitivity::Gas => case.gas_price_wei = scale_up(case.gas_price_wei, 12_500)?,
            Sensitivity::Slippage => case.slippage_buffer = scale_up(case.slippage_buffer, 15_000)?,
            Sensitivity::Latency => case.latency_reserve = scale_up(case.latency_reserve, 15_000)?,
        }
    }
    let stressed = replay_cases(stressed)?
        .decisions
        .iter()
        .map(|item| item.base_net_pnl)
        .try_fold(0_i128, i128::checked_add)
        .ok_or(ReplayError::ArithmeticOverflow)?;
    stressed
        .checked_sub(baseline)
        .ok_or(ReplayError::ArithmeticOverflow)
}

fn scale_up(value: u128, multiplier_bps: u128) -> Result<u128, ReplayError> {
    value
        .checked_mul(multiplier_bps)
        .and_then(|scaled| scaled.checked_add(BPS_DENOMINATOR as u128 - 1))
        .map(|scaled| scaled / BPS_DENOMINATOR as u128)
        .ok_or(ReplayError::ArithmeticOverflow)
}

fn cluster_pnl(decisions: &[ReplayDecision]) -> Result<Vec<i128>, ReplayError> {
    let mut clusters = BTreeMap::<(u64, &str), i128>::new();
    for decision in decisions {
        let value = clusters
            .entry((decision.observed_block, &decision.route_fingerprint))
            .or_insert(0);
        *value = value
            .checked_add(decision.counterfactual_pnl)
            .ok_or(ReplayError::ArithmeticOverflow)?;
    }
    Ok(clusters.into_values().collect())
}

fn cluster_bootstrap_mean_ci(clusters: &[i128]) -> Result<(i128, i128), ReplayError> {
    if clusters.is_empty() {
        return Ok((0, 0));
    }
    let mut rng = DeterministicRng::new(BOOTSTRAP_SEED);
    let mut means = Vec::with_capacity(BOOTSTRAP_ROUNDS);
    for _ in 0..BOOTSTRAP_ROUNDS {
        let mut sampled = Vec::with_capacity(clusters.len());
        for _ in 0..clusters.len() {
            sampled.push(clusters[rng.next_index(clusters.len())]);
        }
        means.push(mean(&sampled)?);
    }
    Ok((quantile(&means, 2)?, quantile(&means, 97)?))
}

struct DeterministicRng {
    state: u64,
}

impl DeterministicRng {
    fn new(seed: u64) -> Self {
        Self { state: seed }
    }

    fn next_index(&mut self, upper: usize) -> usize {
        self.state = self
            .state
            .wrapping_mul(6_364_136_223_846_793_005)
            .wrapping_add(1_442_695_040_888_963_407);
        (self.state as usize) % upper
    }
}

fn mean(values: &[i128]) -> Result<i128, ReplayError> {
    if values.is_empty() {
        Ok(0)
    } else {
        let denominator =
            i128::try_from(values.len()).map_err(|_| ReplayError::ArithmeticOverflow)?;
        Ok(checked_sum(values.iter().copied())? / denominator)
    }
}

fn checked_sum(mut values: impl Iterator<Item = i128>) -> Result<i128, ReplayError> {
    values
        .try_fold(0_i128, i128::checked_add)
        .ok_or(ReplayError::ArithmeticOverflow)
}

fn quantile(values: &[i128], percentile: usize) -> Result<i128, ReplayError> {
    if values.is_empty() {
        return Ok(0);
    }
    let mut ordered = values.to_vec();
    ordered.sort_unstable();
    let index = (ordered.len() - 1)
        .checked_mul(percentile)
        .ok_or(ReplayError::ArithmeticOverflow)?
        / 100;
    ordered
        .get(index)
        .copied()
        .ok_or(ReplayError::ArithmeticOverflow)
}

fn rate_bps(numerator: usize, denominator: usize) -> Result<i128, ReplayError> {
    if denominator == 0 {
        Ok(0)
    } else {
        let numerator = i128::try_from(numerator).map_err(|_| ReplayError::ArithmeticOverflow)?;
        let denominator =
            i128::try_from(denominator).map_err(|_| ReplayError::ArithmeticOverflow)?;
        numerator
            .checked_mul(BPS_DENOMINATOR)
            .map(|scaled| scaled / denominator)
            .ok_or(ReplayError::ArithmeticOverflow)
    }
}

fn maximum_drawdown(values: &[i128]) -> Result<i128, ReplayError> {
    let mut cumulative = 0i128;
    let mut peak = 0i128;
    let mut worst = 0i128;
    for value in values {
        cumulative = cumulative
            .checked_add(*value)
            .ok_or(ReplayError::ArithmeticOverflow)?;
        peak = peak.max(cumulative);
        worst = worst.max(
            peak.checked_sub(cumulative)
                .ok_or(ReplayError::ArithmeticOverflow)?,
        );
    }
    Ok(worst)
}

fn largest_contribution_bps(values: &[i128]) -> Result<i128, ReplayError> {
    let positives: Vec<i128> = values.iter().copied().filter(|value| *value > 0).collect();
    let total = checked_sum(positives.iter().copied())?;
    if total == 0 {
        Ok(0)
    } else {
        positives
            .iter()
            .copied()
            .max()
            .unwrap_or(0)
            .checked_mul(BPS_DENOMINATOR)
            .map(|scaled| scaled / total)
            .ok_or(ReplayError::ArithmeticOverflow)
    }
}

fn concentration_bps<'a>(values: impl Iterator<Item = &'a str>) -> Result<i128, ReplayError> {
    let mut counts = BTreeMap::<&str, usize>::new();
    for value in values {
        let count = counts.entry(value).or_insert(0);
        *count = count
            .checked_add(1)
            .ok_or(ReplayError::ArithmeticOverflow)?;
    }
    let total = counts
        .values()
        .try_fold(0_usize, |total, count| total.checked_add(*count))
        .ok_or(ReplayError::ArithmeticOverflow)?;
    rate_bps(counts.values().copied().max().unwrap_or(0), total)
}

#[cfg(test)]
mod tests {
    use super::*;

    const CASES: &str = include_str!("../../fixtures/replay/shadow_cases.ndjson");

    #[test]
    fn evidence_report_is_deterministic_and_clustered() {
        let first = build(CASES).unwrap();
        let second = build(CASES).unwrap();
        assert_eq!(first, second);
        assert_eq!(first.sample_size, 11);
        assert_eq!(first.independent_opportunity_count, 11);
        assert_eq!(first.accepted_count, 2);
        assert_eq!(first.rejected_count, 9);
        assert!(first.cluster_bootstrap_mean_ci_low <= first.cluster_bootstrap_mean_ci_high);
    }

    #[test]
    fn sensitivity_stress_never_improves_aggregate_pnl() {
        let report = build(CASES).unwrap();
        assert!(report.gas_sensitivity_delta < 0);
        assert!(report.slippage_sensitivity_delta < 0);
        assert!(report.latency_sensitivity_delta < 0);
    }

    #[test]
    fn financial_statistics_fail_on_integer_overflow() {
        assert_eq!(
            checked_sum([i128::MAX, 1].into_iter()),
            Err(ReplayError::ArithmeticOverflow)
        );
        assert_eq!(
            scale_up(u128::MAX, 12_500),
            Err(ReplayError::ArithmeticOverflow)
        );
    }
}
