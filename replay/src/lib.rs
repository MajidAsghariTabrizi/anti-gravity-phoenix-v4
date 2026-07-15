use std::collections::BTreeSet;
use std::fmt::Write;

use phoenix_engine::decision::{decide, DecisionContext, ShadowPolicy};
use phoenix_engine::domain::{
    Address, Amount, Direction, OpportunityId, PoolId, RouteId, TokenAddress, TxHash,
};
use phoenix_engine::economics::{evaluate_scenarios, EconomicError, EconomicInput};
use phoenix_engine::graph::PoolEdge;
use phoenix_engine::opportunity::{
    AgreementState, BasisPoints, DecisionEvidence, IndependentVerificationStatus, MarketEvidence,
    Opportunity, OpportunityIdentity, OutcomeEvidence, PoolStateEvidence, RejectionReason,
    RouteEvidence, ShadowDisposition, SimulationClassification, SimulationEvidence, SimulationKind,
    StateSource, Strategy, VerificationSkipReason, VerificationStatus,
};
use serde::Deserialize;

pub mod evidence;

pub const REPLAY_SCHEMA_VERSION: &str = "shadow-replay-v1";
pub const STRATEGY_VERSION: &str = "two-pool-v3-v1";
pub const POLICY_VERSION: &str = "shadow-policy-v1";

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DeterministicClock {
    now_unix_ms: u64,
}

impl DeterministicClock {
    pub fn new(now_unix_ms: u64) -> Self {
        Self { now_unix_ms }
    }

    pub fn now_unix_ms(&self) -> u64 {
        self.now_unix_ms
    }

    pub fn advance_ms(&mut self, delta: u64) {
        self.now_unix_ms = self.now_unix_ms.saturating_add(delta);
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ReplayConfig {
    pub fixture: String,
    pub code_version: String,
    pub config_version: String,
}

#[derive(Clone, Debug, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct ReplayCase {
    pub case_id: String,
    pub source_sequence: u64,
    pub observed_block: u64,
    pub state_block_hash: String,
    pub state_hash: String,
    pub rpc_provider_id: String,
    pub rpc_response_hash: String,
    pub observed_at_unix_ms: u64,
    pub detected_at_unix_ms: u64,
    pub decided_at_unix_ms: u64,
    pub expires_at_unix_ms: u64,
    pub hypothetical_inclusion_block: u64,
    pub quote_age_ms: u64,
    pub principal: u128,
    pub gross_output: u128,
    pub protocol_fees: u128,
    pub pool_fees: u128,
    pub price_impact: u128,
    pub slippage_buffer: u128,
    pub flash_loan_fee: u128,
    pub estimated_execution_gas: u64,
    pub gas_price_wei: u128,
    pub l1_data_fee: u128,
    pub contract_overhead: u128,
    pub failed_attempt_gas_cost: u128,
    pub failure_probability_bps: u16,
    pub stale_state_loss: u128,
    pub stale_quote_probability_bps: u16,
    pub state_drift_reserve: u128,
    pub latency_reserve: u128,
    pub uncertainty_reserve: u128,
    pub replacement_transaction_cost: u128,
    pub probability_of_success_bps: u16,
    pub simulation: String,
    pub duplicate: bool,
    pub sequence_contiguous: bool,
    pub liquidity_sufficient: bool,
    pub rpc_state_agrees: bool,
    pub contract_path_available: bool,
    pub risk_budget_available: bool,
    pub confidence_bps: u16,
    pub post_inclusion_adverse_cost: u128,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ReplayDecision {
    pub case_id: String,
    pub observed_block: u64,
    pub source_sequence: u64,
    pub observed_at_unix_ms: u64,
    pub route_fingerprint: String,
    pub protocol: String,
    pub token_pair: String,
    pub simulation_passed: bool,
    pub decision: DecisionEvidence,
    pub base_net_pnl: i128,
    pub conservative_net_pnl: i128,
    pub severe_net_pnl: i128,
    pub counterfactual_pnl: i128,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ReplayReport {
    pub decisions: Vec<ReplayDecision>,
}

impl ReplayReport {
    pub fn accepted_count(&self) -> usize {
        self.decisions
            .iter()
            .filter(|decision| decision.decision.disposition == ShadowDisposition::Accepted)
            .count()
    }

    pub fn rejected_count(&self) -> usize {
        self.decisions.len().saturating_sub(self.accepted_count())
    }

    pub fn render(&self, code_version: &str, config_version: &str) -> String {
        let mut output = String::new();
        let _ = writeln!(
            output,
            "schema={REPLAY_SCHEMA_VERSION} code_version={code_version} config_version={config_version} strategy_version={STRATEGY_VERSION} policy_version={POLICY_VERSION} financial_label=SHADOW_expected realization_status=not_realized"
        );
        for decision in &self.decisions {
            let reason = decision
                .decision
                .primary_rejection_reason
                .map(RejectionReason::as_str)
                .unwrap_or("none");
            let disposition = match decision.decision.disposition {
                ShadowDisposition::Accepted => "accepted",
                ShadowDisposition::Rejected => "rejected",
            };
            let _ = writeln!(
                output,
                "case={} block={} sequence={} disposition={} primary_reason={} base_net_pnl={} conservative_net_pnl={} severe_net_pnl={} counterfactual_pnl={} realization_status=not_realized",
                decision.case_id,
                decision.observed_block,
                decision.source_sequence,
                disposition,
                reason,
                decision.base_net_pnl,
                decision.conservative_net_pnl,
                decision.severe_net_pnl,
                decision.counterfactual_pnl,
            );
        }
        let _ = writeln!(
            output,
            "summary candidates={} accepted={} rejected={}",
            self.decisions.len(),
            self.accepted_count(),
            self.rejected_count(),
        );
        output
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ReplayError {
    InvalidFixture { line: usize, detail: String },
    InvalidAddress,
    InvalidSimulation(String),
    Economics(EconomicError),
    ArithmeticOverflow,
}

pub fn parse_cases(input: &str) -> Result<Vec<ReplayCase>, ReplayError> {
    input
        .lines()
        .enumerate()
        .filter(|(_, line)| !line.trim().is_empty())
        .map(|(index, line)| {
            serde_json::from_str::<ReplayCase>(line).map_err(|error| ReplayError::InvalidFixture {
                line: index + 1,
                detail: error.to_string(),
            })
        })
        .collect()
}

pub fn replay(input: &str) -> Result<ReplayReport, ReplayError> {
    replay_cases(parse_cases(input)?)
}

pub fn replay_cases(mut cases: Vec<ReplayCase>) -> Result<ReplayReport, ReplayError> {
    cases.sort_by(|left, right| {
        (left.observed_block, left.source_sequence, &left.case_id).cmp(&(
            right.observed_block,
            right.source_sequence,
            &right.case_id,
        ))
    });
    let mut decisions = Vec::with_capacity(cases.len());
    for case in cases {
        decisions.push(evaluate_case(&case)?);
    }
    Ok(ReplayReport { decisions })
}

fn evaluate_case(case: &ReplayCase) -> Result<ReplayDecision, ReplayError> {
    let economics = evaluate_scenarios(&EconomicInput {
        principal: Amount(case.principal),
        gross_output: Amount(case.gross_output),
        protocol_fees: Amount(case.protocol_fees),
        pool_fees: Amount(case.pool_fees),
        price_impact: Amount(case.price_impact),
        minimum_slippage_buffer: Amount(case.slippage_buffer),
        flash_loan_fee: Amount(case.flash_loan_fee),
        estimated_execution_gas: case.estimated_execution_gas,
        gas_price_wei: case.gas_price_wei,
        l1_data_fee: Amount(case.l1_data_fee),
        contract_overhead: Amount(case.contract_overhead),
        failed_attempt_gas_cost: Amount(case.failed_attempt_gas_cost),
        failure_probability_bps: case.failure_probability_bps,
        stale_state_loss: Amount(case.stale_state_loss),
        stale_quote_probability_bps: case.stale_quote_probability_bps,
        state_drift_reserve: Amount(case.state_drift_reserve),
        latency_reserve: Amount(case.latency_reserve),
        uncertainty_reserve: Amount(case.uncertainty_reserve),
        replacement_transaction_cost: Amount(case.replacement_transaction_cost),
        probability_of_success_bps: case.probability_of_success_bps,
        minimum_required_net_pnl: phoenix_engine::opportunity::SignedAmount(1),
    })
    .map_err(ReplayError::Economics)?;
    let mut opportunity = build_opportunity(case, economics)?;
    let policy = replay_policy(&opportunity);
    opportunity.decision = decide(
        &opportunity,
        &policy,
        DecisionContext {
            now_unix_ms: case.decided_at_unix_ms,
            duplicate: case.duplicate,
            sequence_contiguous: case.sequence_contiguous,
            liquidity_sufficient: case.liquidity_sufficient,
            rpc_state_agrees: case.rpc_state_agrees,
            contract_path_available: case.contract_path_available,
            risk_budget_available: case.risk_budget_available,
            confidence_bps: case.confidence_bps,
        },
    );
    let counterfactual_pnl = opportunity
        .economics
        .base
        .expected_net_pnl
        .0
        .checked_sub(
            i128::try_from(case.post_inclusion_adverse_cost)
                .map_err(|_| ReplayError::ArithmeticOverflow)?,
        )
        .ok_or(ReplayError::ArithmeticOverflow)?;

    Ok(ReplayDecision {
        case_id: case.case_id.clone(),
        observed_block: case.observed_block,
        source_sequence: case.source_sequence,
        observed_at_unix_ms: case.observed_at_unix_ms,
        route_fingerprint: opportunity.route.route_fingerprint,
        protocol: opportunity.route.protocols[0].clone(),
        token_pair: format!(
            "{}:{}",
            opportunity.route.input_token.0.as_str(),
            opportunity.route.output_token.0.as_str()
        ),
        simulation_passed: opportunity.simulation.classification
            == SimulationClassification::Passed,
        decision: opportunity.decision,
        base_net_pnl: opportunity.economics.base.expected_net_pnl.0,
        conservative_net_pnl: opportunity.economics.conservative.expected_net_pnl.0,
        severe_net_pnl: opportunity.economics.severe.expected_net_pnl.0,
        counterfactual_pnl,
    })
}

fn build_opportunity(
    case: &ReplayCase,
    economics: phoenix_engine::opportunity::ScenarioEconomics,
) -> Result<Opportunity, ReplayError> {
    let token0 = token("0x1111111111111111111111111111111111111111")?;
    let token1 = token("0x2222222222222222222222222222222222222222")?;
    let pool = PoolId("fixture-pool".to_string());
    let leg = PoolEdge {
        pool_id: pool.clone(),
        protocol: "UniswapV3".to_string(),
        fee: 500,
        token_in: token0.clone(),
        token_out: token1.clone(),
        direction: Direction::ZeroForOne,
    };
    Ok(Opportunity {
        identity: OpportunityIdentity {
            opportunity_id: OpportunityId(case.case_id.clone()),
            strategy: Strategy::TwoPoolV3Arbitrage,
            strategy_version: STRATEGY_VERSION.to_string(),
            detector_version: "replay-detector-v1".to_string(),
            code_version: "recorded-by-runner".to_string(),
            config_version: "recorded-by-runner".to_string(),
            chain_id: 42161,
            source_sequence: case.source_sequence,
            origin_tx_hash: TxHash(format!("fixture-{}", case.case_id)),
            observed_block: case.observed_block,
            observed_at_unix_ms: case.observed_at_unix_ms,
            detected_at_unix_ms: case.detected_at_unix_ms,
        },
        route: RouteEvidence {
            route_id: RouteId("fixture-two-pool-route".to_string()),
            route_fingerprint: "fixture-two-pool-route-v1".to_string(),
            token_path: vec![token0.clone(), token1.clone()],
            pools: vec![pool.clone()],
            protocols: vec!["UniswapV3".to_string()],
            input_token: token0,
            output_token: token1,
            input_amount: Amount(case.principal),
            expected_output: Amount(case.gross_output),
            exact_ordered_legs: vec![leg],
        },
        market: MarketEvidence {
            pool_states: vec![PoolStateEvidence {
                pool,
                state_hash: case.state_hash.clone(),
                reserve_or_liquidity_summary: "fixture-recorded-liquidity".to_string(),
            }],
            state_block: case.observed_block,
            state_block_hash: Some(case.state_block_hash.clone()),
            route_config_hash: None,
            quote_block: case.observed_block,
            quote_age_ms: case.quote_age_ms,
            state_source: StateSource::RecordedCheckpoint,
            primary_provider_id: Some(case.rpc_provider_id.clone()),
            primary_response_hash: Some(case.rpc_response_hash.clone()),
            primary_state_hash: Some(case.state_hash.clone()),
            secondary_provider_id: None,
            secondary_state_hash: None,
            secondary_block_number: None,
            secondary_block_hash: None,
            secondary_route_config_hash: None,
            verification_status: VerificationStatus::HistoricalEvidence,
            independent_verification_status: IndependentVerificationStatus::NotRequested,
            independent_verification_lifecycle: vec![IndependentVerificationStatus::NotRequested],
            agreement_state: AgreementState::NotChecked,
            verification_skip_reason: Some(VerificationSkipReason::HistoricalEvidence),
            feed_to_detection_latency_ns: (case
                .detected_at_unix_ms
                .saturating_sub(case.observed_at_unix_ms)
                as u128)
                * 1_000_000,
        },
        economics,
        simulation: SimulationEvidence {
            kind: SimulationKind::HistoricalReplay,
            block_number: case.observed_block,
            block_hash: Some(case.state_block_hash.clone()),
            from_address: None,
            target_contract: Some("recorded-executor".to_string()),
            contract_code_hash: Some("fixture-code-hash".to_string()),
            calldata_hash: format!("fixture-calldata-{}", case.case_id),
            value: Amount::ZERO,
            gas_estimate: Some(case.estimated_execution_gas),
            gas_used: Some(case.estimated_execution_gas),
            simulated_output: Some(Amount(case.gross_output)),
            simulated_net_pnl: None,
            revert_reason: if case.simulation == "reverted" {
                Some("fixture revert".to_string())
            } else {
                None
            },
            state_overrides_hash: None,
            provider_id: Some(case.rpc_provider_id.clone()),
            simulated_at_unix_ms: case.detected_at_unix_ms,
            latency_ns: 1_000_000,
            state_drift_bps: BasisPoints(0),
            classification: parse_simulation(&case.simulation)?,
        },
        decision: pending_decision(case.decided_at_unix_ms),
        outcome: OutcomeEvidence {
            hypothetical_execution_at_unix_ms: Some(case.decided_at_unix_ms),
            hypothetical_inclusion_block: Some(case.hypothetical_inclusion_block),
            replay_pnl: None,
            opportunity_expires_at_unix_ms: case.expires_at_unix_ms,
            post_opportunity_market_movement_bps: None,
            missed_opportunity_reason: None,
        },
    })
}

fn pending_decision(now_unix_ms: u64) -> DecisionEvidence {
    DecisionEvidence {
        disposition: ShadowDisposition::Rejected,
        primary_rejection_reason: Some(RejectionReason::SimulationEvidenceInsufficient),
        secondary_rejection_reasons: Vec::new(),
        risk_flags: Vec::new(),
        confidence_bps: 0,
        policy_version: POLICY_VERSION.to_string(),
        shadow_only: true,
        execution_eligible: false,
        execution_request_created: false,
        decided_at_unix_ms: now_unix_ms,
    }
}

fn replay_policy(opportunity: &Opportunity) -> ShadowPolicy {
    ShadowPolicy {
        version: POLICY_VERSION.to_string(),
        allowed_tokens: opportunity
            .route
            .token_path
            .iter()
            .map(|token| token.0.as_str().to_string())
            .collect::<BTreeSet<_>>(),
        allowed_protocols: ["UniswapV3".to_string()].into_iter().collect(),
        max_quote_age_ms: 500,
        max_simulation_age_ms: 500,
        max_gas_price_wei: 1_000,
        min_base_net_pnl: phoenix_engine::opportunity::SignedAmount(1),
        min_conservative_net_pnl: phoenix_engine::opportunity::SignedAmount(1),
        min_severe_net_pnl: phoenix_engine::opportunity::SignedAmount(1),
        min_confidence_bps: 8_000,
    }
}

fn token(value: &str) -> Result<TokenAddress, ReplayError> {
    Address::parse(value)
        .map(TokenAddress)
        .map_err(|_| ReplayError::InvalidAddress)
}

fn parse_simulation(value: &str) -> Result<SimulationClassification, ReplayError> {
    match value {
        "passed" => Ok(SimulationClassification::Passed),
        "reverted" => Ok(SimulationClassification::Reverted),
        "provider_disagreement" => Ok(SimulationClassification::ProviderDisagreement),
        "stale_state" => Ok(SimulationClassification::StaleState),
        "contract_unavailable" => Ok(SimulationClassification::ContractUnavailable),
        "not_run" => Ok(SimulationClassification::NotRun),
        other => Err(ReplayError::InvalidSimulation(other.to_string())),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const CASES: &str = include_str!("../../fixtures/replay/shadow_cases.ndjson");

    #[test]
    fn deterministic_clock_advances_explicitly() {
        let mut clock = DeterministicClock::new(100);
        clock.advance_ms(5);
        assert_eq!(clock.now_unix_ms(), 105);
    }

    #[test]
    fn replay_fixture_covers_required_outcomes() {
        let report = replay(CASES).unwrap();
        assert_eq!(report.decisions.len(), 11);
        for required in [
            "fee-negative",
            "slippage-negative",
            "stale-state",
            "insufficient-liquidity",
            "simulation-revert",
            "duplicate",
            "expired",
            "valid-net-positive",
            "rpc-disagreement",
            "sequence-discontinuity",
            "pre-inclusion-movement",
        ] {
            assert!(report.decisions.iter().any(|case| case.case_id == required));
        }
    }

    #[test]
    fn replay_output_is_byte_deterministic() {
        let first = replay(CASES).unwrap().render("test-code", "test-config");
        let second = replay(CASES).unwrap().render("test-code", "test-config");
        assert_eq!(first, second);
    }

    #[test]
    fn required_failures_have_exact_primary_reasons() {
        let report = replay(CASES).unwrap();
        let reason = |id: &str| {
            report
                .decisions
                .iter()
                .find(|decision| decision.case_id == id)
                .unwrap()
                .decision
                .primary_rejection_reason
        };
        assert_eq!(
            reason("duplicate"),
            Some(RejectionReason::DuplicateOpportunity)
        );
        assert_eq!(reason("stale-state"), Some(RejectionReason::QuoteStale));
        assert_eq!(
            reason("rpc-disagreement"),
            Some(RejectionReason::RpcStateDisagreement)
        );
        assert_eq!(
            reason("sequence-discontinuity"),
            Some(RejectionReason::SequenceDiscontinuity)
        );
    }

    #[test]
    fn adverse_pre_inclusion_movement_is_not_hidden() {
        let report = replay(CASES).unwrap();
        let movement = report
            .decisions
            .iter()
            .find(|decision| decision.case_id == "pre-inclusion-movement")
            .unwrap();
        assert_eq!(movement.decision.disposition, ShadowDisposition::Accepted);
        assert!(movement.counterfactual_pnl < 0);
    }
}
