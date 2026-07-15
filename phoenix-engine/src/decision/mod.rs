use std::collections::BTreeSet;

use crate::opportunity::{
    DecisionEvidence, Opportunity, RejectionReason, RiskFlag, ShadowDisposition, SignedAmount,
    SimulationClassification,
};

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ShadowPolicy {
    pub version: String,
    pub allowed_tokens: BTreeSet<String>,
    pub allowed_protocols: BTreeSet<String>,
    pub max_quote_age_ms: u64,
    pub max_simulation_age_ms: u64,
    pub max_gas_price_wei: u128,
    pub min_base_net_pnl: SignedAmount,
    pub min_conservative_net_pnl: SignedAmount,
    pub min_severe_net_pnl: SignedAmount,
    pub min_confidence_bps: u16,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct DecisionContext {
    pub now_unix_ms: u64,
    pub duplicate: bool,
    pub sequence_contiguous: bool,
    pub liquidity_sufficient: bool,
    pub rpc_state_agrees: bool,
    pub contract_path_available: bool,
    pub risk_budget_available: bool,
    pub confidence_bps: u16,
}

pub fn decide(
    opportunity: &Opportunity,
    policy: &ShadowPolicy,
    context: DecisionContext,
) -> DecisionEvidence {
    let mut reasons = Vec::new();
    let mut risk_flags = Vec::new();

    if context.duplicate {
        reasons.push(RejectionReason::DuplicateOpportunity);
    }
    if !context.sequence_contiguous {
        reasons.push(RejectionReason::SequenceDiscontinuity);
    }
    if !route_tokens_allowed(opportunity, policy) {
        reasons.push(RejectionReason::TokenNotAllowed);
    }
    if !route_protocols_allowed(opportunity, policy) {
        reasons.push(RejectionReason::ProtocolNotAllowed);
    }
    if context.now_unix_ms >= opportunity.outcome.opportunity_expires_at_unix_ms {
        reasons.push(RejectionReason::OpportunityExpired);
    }
    if opportunity.market.quote_age_ms > policy.max_quote_age_ms {
        reasons.push(RejectionReason::QuoteStale);
        risk_flags.push(RiskFlag::StaleQuote);
    }
    if !context.rpc_state_agrees {
        reasons.push(RejectionReason::RpcStateDisagreement);
        risk_flags.push(RiskFlag::RpcDisagreement);
    }
    if !context.liquidity_sufficient {
        reasons.push(RejectionReason::LiquidityInsufficient);
        risk_flags.push(RiskFlag::IncompleteLiquidity);
    }
    append_simulation_reason(opportunity, policy, context, &mut reasons, &mut risk_flags);
    if !context.contract_path_available {
        reasons.push(RejectionReason::ContractPathUnavailable);
        risk_flags.push(RiskFlag::ContractUnavailable);
    }
    if !context.risk_budget_available {
        reasons.push(RejectionReason::RiskBudgetExceeded);
    }
    if opportunity.economics.base.gas_price_wei > policy.max_gas_price_wei {
        reasons.push(RejectionReason::GasTooHigh);
    }
    if opportunity.economics.base.gross_spread <= SignedAmount(0) {
        reasons.push(RejectionReason::GrossSpreadInsufficient);
    }
    if opportunity.economics.base.expected_net_pnl < policy.min_base_net_pnl {
        reasons.push(RejectionReason::NetPnlNegative);
    }
    if opportunity.economics.conservative.expected_net_pnl < policy.min_conservative_net_pnl
        || opportunity.economics.severe.expected_net_pnl < policy.min_severe_net_pnl
    {
        reasons.push(RejectionReason::StressPnlNegative);
    }
    if context.confidence_bps < policy.min_confidence_bps {
        reasons.push(RejectionReason::ConfidenceTooLow);
    }

    let primary_rejection_reason = reasons.first().copied();
    let secondary_rejection_reasons = reasons.into_iter().skip(1).collect();
    DecisionEvidence {
        disposition: if primary_rejection_reason.is_none() {
            ShadowDisposition::Accepted
        } else {
            ShadowDisposition::Rejected
        },
        primary_rejection_reason,
        secondary_rejection_reasons,
        risk_flags,
        confidence_bps: context.confidence_bps,
        policy_version: policy.version.clone(),
        shadow_only: true,
        execution_eligible: false,
        execution_request_created: false,
        decided_at_unix_ms: context.now_unix_ms,
    }
}

fn route_tokens_allowed(opportunity: &Opportunity, policy: &ShadowPolicy) -> bool {
    opportunity
        .route
        .token_path
        .iter()
        .all(|token| policy.allowed_tokens.contains(token.0.as_str()))
}

fn route_protocols_allowed(opportunity: &Opportunity, policy: &ShadowPolicy) -> bool {
    opportunity
        .route
        .protocols
        .iter()
        .all(|protocol| policy.allowed_protocols.contains(protocol))
}

fn append_simulation_reason(
    opportunity: &Opportunity,
    policy: &ShadowPolicy,
    context: DecisionContext,
    reasons: &mut Vec<RejectionReason>,
    risk_flags: &mut Vec<RiskFlag>,
) {
    let simulation_age = context
        .now_unix_ms
        .saturating_sub(opportunity.simulation.simulated_at_unix_ms);
    if simulation_age > policy.max_simulation_age_ms {
        reasons.push(RejectionReason::SimulationEvidenceInsufficient);
        risk_flags.push(RiskFlag::SimulationUnavailable);
        return;
    }
    match opportunity.simulation.classification {
        SimulationClassification::Passed => {}
        SimulationClassification::Reverted => reasons.push(RejectionReason::SimulationReverted),
        SimulationClassification::ProviderDisagreement => {
            reasons.push(RejectionReason::RpcStateDisagreement);
            risk_flags.push(RiskFlag::RpcDisagreement);
        }
        SimulationClassification::StaleState => {
            reasons.push(RejectionReason::QuoteStale);
            risk_flags.push(RiskFlag::StaleQuote);
        }
        SimulationClassification::ContractUnavailable => {
            reasons.push(RejectionReason::ContractPathUnavailable);
            risk_flags.push(RiskFlag::ContractUnavailable);
        }
        SimulationClassification::AmbiguousBlock
        | SimulationClassification::UnsafeToken
        | SimulationClassification::NotRun => {
            reasons.push(RejectionReason::SimulationEvidenceInsufficient);
            risk_flags.push(RiskFlag::SimulationUnavailable);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::domain::{Address, Amount, OpportunityId, PoolId, RouteId, TokenAddress, TxHash};
    use crate::graph::{PoolEdge, Route};
    use crate::opportunity::{
        AgreementState, BasisPoints, CostBreakdown, MarketEvidence, OpportunityIdentity,
        OutcomeEvidence, PrimaryProfitabilityStatus, RouteEvidence, ScenarioEconomics,
        SimulationEvidence, SimulationKind, StateSource, Strategy, VerificationStatus,
        VerificationSkipReason, PROFITABILITY_MODEL_VERSION,
    };
    use crate::Direction;

    fn token(value: &str) -> TokenAddress {
        TokenAddress(Address::parse(value).unwrap())
    }

    fn opportunity() -> Opportunity {
        let token0 = token("0x1111111111111111111111111111111111111111");
        let token1 = token("0x2222222222222222222222222222222222222222");
        let leg = PoolEdge {
            pool_id: PoolId("pool-1".to_string()),
            protocol: "UniswapV3".to_string(),
            fee: 500,
            token_in: token0.clone(),
            token_out: token1.clone(),
            direction: Direction::ZeroForOne,
        };
        let economics = CostBreakdown {
            gross_spread: SignedAmount(100),
            gross_profit: SignedAmount(100),
            gas_price_wei: 10,
            contract_overhead: Amount(50),
            total_cost: Amount(50),
            expected_net_pnl: SignedAmount(50),
            expected_roi_bps: BasisPoints(500),
            ..CostBreakdown::default()
        };
        Opportunity {
            identity: OpportunityIdentity {
                opportunity_id: OpportunityId("op-1".to_string()),
                strategy: Strategy::TwoPoolV3Arbitrage,
                strategy_version: "v1".to_string(),
                detector_version: "v1".to_string(),
                code_version: "test".to_string(),
                config_version: "test".to_string(),
                chain_id: 42161,
                source_sequence: 1,
                origin_tx_hash: TxHash("0x01".to_string()),
                observed_block: 100,
                observed_at_unix_ms: 1_000,
                detected_at_unix_ms: 1_001,
            },
            route: RouteEvidence {
                route_id: RouteId("route-1".to_string()),
                route_fingerprint: "route-fingerprint".to_string(),
                token_path: vec![token0.clone(), token1.clone()],
                pools: vec![PoolId("pool-1".to_string())],
                protocols: vec!["UniswapV3".to_string()],
                input_token: token0,
                output_token: token1,
                input_amount: Amount(1_000),
                expected_output: Amount(1_100),
                exact_ordered_legs: Route {
                    route_id: RouteId("route-1".to_string()),
                    legs: vec![leg],
                }
                .legs,
            },
            market: MarketEvidence {
                pool_states: Vec::new(),
                state_block: 100,
                state_block_hash: Some("block-hash".to_string()),
                quote_block: 100,
                quote_age_ms: 10,
                state_source: StateSource::RecordedCheckpoint,
                primary_provider_id: None,
                primary_response_hash: Some("state-hash".to_string()),
                primary_state_hash: Some("primary-state-hash".to_string()),
                secondary_provider_id: None,
                secondary_state_hash: None,
                verification_status: VerificationStatus::HistoricalEvidence,
                agreement_state: AgreementState::NotChecked,
                verification_skip_reason: Some(VerificationSkipReason::HistoricalEvidence),
                feed_to_detection_latency_ns: 1,
            },
            economics: ScenarioEconomics {
                base: economics.clone(),
                conservative: economics.clone(),
                severe: economics,
                minimum_required_net_pnl: SignedAmount(1),
                primary_status: PrimaryProfitabilityStatus::MeetsMinimum,
                model_version: PROFITABILITY_MODEL_VERSION.to_string(),
            },
            simulation: SimulationEvidence {
                kind: SimulationKind::HistoricalReplay,
                block_number: 100,
                block_hash: Some("block-hash".to_string()),
                from_address: None,
                target_contract: Some("executor".to_string()),
                contract_code_hash: Some("code-hash".to_string()),
                calldata_hash: "calldata-hash".to_string(),
                value: Amount::ZERO,
                gas_estimate: Some(1),
                gas_used: Some(1),
                simulated_output: Some(Amount(1_100)),
                simulated_net_pnl: Some(SignedAmount(50)),
                revert_reason: None,
                state_overrides_hash: None,
                provider_id: None,
                simulated_at_unix_ms: 1_005,
                latency_ns: 1,
                state_drift_bps: BasisPoints(0),
                classification: SimulationClassification::Passed,
            },
            decision: DecisionEvidence {
                disposition: ShadowDisposition::Rejected,
                primary_rejection_reason: Some(RejectionReason::ConfidenceTooLow),
                secondary_rejection_reasons: Vec::new(),
                risk_flags: Vec::new(),
                confidence_bps: 0,
                policy_version: "pending".to_string(),
                shadow_only: true,
                execution_eligible: false,
                execution_request_created: false,
                decided_at_unix_ms: 0,
            },
            outcome: OutcomeEvidence {
                opportunity_expires_at_unix_ms: 2_000,
                ..OutcomeEvidence::default()
            },
        }
    }

    fn policy(opportunity: &Opportunity) -> ShadowPolicy {
        ShadowPolicy {
            version: "shadow-v1".to_string(),
            allowed_tokens: opportunity
                .route
                .token_path
                .iter()
                .map(|token| token.0.as_str().to_string())
                .collect(),
            allowed_protocols: ["UniswapV3".to_string()].into_iter().collect(),
            max_quote_age_ms: 100,
            max_simulation_age_ms: 100,
            max_gas_price_wei: 100,
            min_base_net_pnl: SignedAmount(1),
            min_conservative_net_pnl: SignedAmount(1),
            min_severe_net_pnl: SignedAmount(1),
            min_confidence_bps: 8_000,
        }
    }

    fn context() -> DecisionContext {
        DecisionContext {
            now_unix_ms: 1_010,
            duplicate: false,
            sequence_contiguous: true,
            liquidity_sufficient: true,
            rpc_state_agrees: true,
            contract_path_available: true,
            risk_budget_available: true,
            confidence_bps: 9_000,
        }
    }

    #[test]
    fn accepts_only_complete_shadow_evidence_and_never_enables_execution() {
        let candidate = opportunity();
        let result = decide(&candidate, &policy(&candidate), context());
        assert_eq!(result.disposition, ShadowDisposition::Accepted);
        assert_eq!(result.primary_rejection_reason, None);
        assert!(!result.execution_eligible);
    }

    #[test]
    fn rejection_order_is_deterministic_and_preserves_secondary_reasons() {
        let candidate = opportunity();
        let mut invalid = context();
        invalid.duplicate = true;
        invalid.sequence_contiguous = false;
        invalid.liquidity_sufficient = false;
        let result = decide(&candidate, &policy(&candidate), invalid);
        assert_eq!(
            result.primary_rejection_reason,
            Some(RejectionReason::DuplicateOpportunity)
        );
        assert!(result
            .secondary_rejection_reasons
            .contains(&RejectionReason::SequenceDiscontinuity));
        assert!(result
            .secondary_rejection_reasons
            .contains(&RejectionReason::LiquidityInsufficient));
    }

    #[test]
    fn stale_quote_and_failed_simulation_fail_closed() {
        let mut candidate = opportunity();
        candidate.market.quote_age_ms = 101;
        candidate.simulation.classification = SimulationClassification::Reverted;
        let result = decide(&candidate, &policy(&candidate), context());
        assert_eq!(
            result.primary_rejection_reason,
            Some(RejectionReason::QuoteStale)
        );
        assert!(result
            .secondary_rejection_reasons
            .contains(&RejectionReason::SimulationReverted));
    }

    #[test]
    fn failed_simulation_cannot_be_execution_eligible() {
        let mut candidate = opportunity();
        candidate.simulation.classification = SimulationClassification::NotRun;
        let result = decide(&candidate, &policy(&candidate), context());
        assert_eq!(result.disposition, ShadowDisposition::Rejected);
        assert!(!result.execution_eligible);
    }
}
