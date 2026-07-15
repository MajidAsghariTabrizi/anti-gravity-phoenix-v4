use crate::domain::{Amount, OpportunityId, PoolId, RouteId, TokenAddress, TxHash};
use crate::graph::PoolEdge;
use serde::Serialize;

pub const PROFITABILITY_MODEL_VERSION: &str = "shadow-profitability-v2";

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, PartialOrd, Ord, Serialize)]
#[serde(transparent)]
pub struct SignedAmount(pub i128);

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, PartialOrd, Ord, Serialize)]
#[serde(transparent)]
pub struct BasisPoints(pub i32);

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Strategy {
    TwoPoolV3Arbitrage,
}

impl Strategy {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::TwoPoolV3Arbitrage => "two_pool_v3_arbitrage",
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct OpportunityIdentity {
    pub opportunity_id: OpportunityId,
    pub strategy: Strategy,
    pub strategy_version: String,
    pub detector_version: String,
    pub code_version: String,
    pub config_version: String,
    pub chain_id: u64,
    pub source_sequence: u64,
    pub origin_tx_hash: TxHash,
    pub observed_block: u64,
    pub observed_at_unix_ms: u64,
    pub detected_at_unix_ms: u64,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct RouteEvidence {
    pub route_id: RouteId,
    pub route_fingerprint: String,
    pub token_path: Vec<TokenAddress>,
    pub pools: Vec<PoolId>,
    pub protocols: Vec<String>,
    pub input_token: TokenAddress,
    pub output_token: TokenAddress,
    pub input_amount: Amount,
    pub expected_output: Amount,
    pub exact_ordered_legs: Vec<PoolEdge>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum StateSource {
    RecordedCheckpoint,
    BlockPinnedRpc,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct PoolStateEvidence {
    pub pool: PoolId,
    pub state_hash: String,
    pub reserve_or_liquidity_summary: String,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct MarketEvidence {
    pub pool_states: Vec<PoolStateEvidence>,
    pub state_block: u64,
    pub state_block_hash: Option<String>,
    pub route_config_hash: Option<String>,
    pub quote_block: u64,
    pub quote_age_ms: u64,
    pub state_source: StateSource,
    pub primary_provider_id: Option<String>,
    pub primary_response_hash: Option<String>,
    pub primary_state_hash: Option<String>,
    pub secondary_provider_id: Option<String>,
    pub secondary_state_hash: Option<String>,
    pub secondary_block_number: Option<u64>,
    pub secondary_block_hash: Option<String>,
    pub secondary_route_config_hash: Option<String>,
    pub verification_status: VerificationStatus,
    pub independent_verification_status: IndependentVerificationStatus,
    pub independent_verification_lifecycle: Vec<IndependentVerificationStatus>,
    pub agreement_state: AgreementState,
    pub verification_skip_reason: Option<VerificationSkipReason>,
    pub feed_to_detection_latency_ns: u128,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum IndependentVerificationStatus {
    NotRequested,
    Requested,
    Agreed,
    Disagreed,
    ProviderUnavailable,
    IntegrityFailure,
}

impl IndependentVerificationStatus {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::NotRequested => "not_requested",
            Self::Requested => "requested",
            Self::Agreed => "agreed",
            Self::Disagreed => "disagreed",
            Self::ProviderUnavailable => "provider_unavailable",
            Self::IntegrityFailure => "integrity_failure",
        }
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum VerificationStatus {
    #[default]
    Incomplete,
    PrimaryOnly,
    Agreed,
    Disagreed,
    SecondaryUnavailable,
    HistoricalEvidence,
}

impl VerificationStatus {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Incomplete => "incomplete",
            Self::PrimaryOnly => "primary_only",
            Self::Agreed => "agreed",
            Self::Disagreed => "disagreed",
            Self::SecondaryUnavailable => "secondary_unavailable",
            Self::HistoricalEvidence => "historical_evidence",
        }
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum AgreementState {
    #[default]
    NotChecked,
    Agreed,
    Disagreed,
    Unavailable,
}

impl AgreementState {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::NotChecked => "not_checked",
            Self::Agreed => "agreed",
            Self::Disagreed => "disagreed",
            Self::Unavailable => "unavailable",
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum VerificationSkipReason {
    PrimaryScreenNoProfitableCandidate,
    HistoricalEvidence,
}

impl VerificationSkipReason {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::PrimaryScreenNoProfitableCandidate => "primary_screen_no_profitable_candidate",
            Self::HistoricalEvidence => "historical_evidence",
        }
    }
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize)]
pub struct CostBreakdown {
    pub gross_spread: SignedAmount,
    pub gross_profit: SignedAmount,
    pub protocol_fees: Amount,
    pub pool_fees: Amount,
    pub price_impact: Amount,
    pub slippage_allowance: Amount,
    pub flash_loan_fee: Amount,
    pub estimated_execution_gas: u64,
    pub gas_price_wei: u128,
    pub arbitrum_execution_fee: Amount,
    pub l1_data_fee: Amount,
    pub contract_overhead: Amount,
    pub failure_cost_reserve: Amount,
    pub stale_state_penalty: Amount,
    pub ordering_reserve: Amount,
    pub state_drift_reserve: Amount,
    pub latency_reserve: Amount,
    pub uncertainty_reserve: Amount,
    pub total_cost: Amount,
    pub expected_net_pnl: SignedAmount,
    pub expected_roi_bps: BasisPoints,
    pub probability_of_success_bps: u16,
    pub expected_value_after_success_probability: SignedAmount,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct ScenarioEconomics {
    pub base: CostBreakdown,
    pub conservative: CostBreakdown,
    pub severe: CostBreakdown,
    pub minimum_required_net_pnl: SignedAmount,
    pub primary_status: PrimaryProfitabilityStatus,
    pub model_version: String,
}

impl Default for ScenarioEconomics {
    fn default() -> Self {
        Self {
            base: CostBreakdown::default(),
            conservative: CostBreakdown::default(),
            severe: CostBreakdown::default(),
            minimum_required_net_pnl: SignedAmount::default(),
            primary_status: PrimaryProfitabilityStatus::Incomplete,
            model_version: PROFITABILITY_MODEL_VERSION.to_string(),
        }
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum PrimaryProfitabilityStatus {
    #[default]
    Incomplete,
    MeetsMinimum,
    BelowMinimum,
}

impl PrimaryProfitabilityStatus {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Incomplete => "incomplete",
            Self::MeetsMinimum => "meets_minimum",
            Self::BelowMinimum => "below_minimum",
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum SimulationKind {
    StaticQuote,
    StateBased,
    EthCall,
    ContractCall,
    Fork,
    HistoricalReplay,
    HypotheticalInclusion,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum SimulationClassification {
    Passed,
    Reverted,
    AmbiguousBlock,
    ProviderDisagreement,
    StaleState,
    ContractUnavailable,
    UnsafeToken,
    NotRun,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct SimulationEvidence {
    pub kind: SimulationKind,
    pub block_number: u64,
    pub block_hash: Option<String>,
    pub from_address: Option<String>,
    pub target_contract: Option<String>,
    pub contract_code_hash: Option<String>,
    pub calldata_hash: String,
    pub value: Amount,
    pub gas_estimate: Option<u64>,
    pub gas_used: Option<u64>,
    pub simulated_output: Option<Amount>,
    pub simulated_net_pnl: Option<SignedAmount>,
    pub revert_reason: Option<String>,
    pub state_overrides_hash: Option<String>,
    pub provider_id: Option<String>,
    pub simulated_at_unix_ms: u64,
    pub latency_ns: u128,
    pub state_drift_bps: BasisPoints,
    pub classification: SimulationClassification,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum RejectionReason {
    GrossSpreadInsufficient,
    NetPnlNegative,
    StressPnlNegative,
    LiquidityInsufficient,
    QuoteStale,
    SimulationReverted,
    SimulationEvidenceInsufficient,
    GasTooHigh,
    TokenNotAllowed,
    ProtocolNotAllowed,
    RpcStateDisagreement,
    ConfidenceTooLow,
    OpportunityExpired,
    DuplicateOpportunity,
    RiskBudgetExceeded,
    ContractPathUnavailable,
    SequenceDiscontinuity,
}

impl RejectionReason {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::GrossSpreadInsufficient => "gross_spread_insufficient",
            Self::NetPnlNegative => "net_pnl_negative",
            Self::StressPnlNegative => "stress_pnl_negative",
            Self::LiquidityInsufficient => "liquidity_insufficient",
            Self::QuoteStale => "quote_stale",
            Self::SimulationReverted => "simulation_reverted",
            Self::SimulationEvidenceInsufficient => "simulation_evidence_insufficient",
            Self::GasTooHigh => "gas_too_high",
            Self::TokenNotAllowed => "token_not_allowed",
            Self::ProtocolNotAllowed => "protocol_not_allowed",
            Self::RpcStateDisagreement => "rpc_state_disagreement",
            Self::ConfidenceTooLow => "confidence_too_low",
            Self::OpportunityExpired => "opportunity_expired",
            Self::DuplicateOpportunity => "duplicate_opportunity",
            Self::RiskBudgetExceeded => "risk_budget_exceeded",
            Self::ContractPathUnavailable => "contract_path_unavailable",
            Self::SequenceDiscontinuity => "sequence_discontinuity",
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum RiskFlag {
    StaleQuote,
    StateDrift,
    RpcDisagreement,
    IncompleteLiquidity,
    SimulationUnavailable,
    ContractUnavailable,
    ConcentrationUnknown,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ShadowDisposition {
    Accepted,
    Rejected,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct DecisionEvidence {
    pub disposition: ShadowDisposition,
    pub primary_rejection_reason: Option<RejectionReason>,
    pub secondary_rejection_reasons: Vec<RejectionReason>,
    pub risk_flags: Vec<RiskFlag>,
    pub confidence_bps: u16,
    pub policy_version: String,
    pub shadow_only: bool,
    pub execution_eligible: bool,
    pub execution_request_created: bool,
    pub decided_at_unix_ms: u64,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize)]
pub struct OutcomeEvidence {
    pub hypothetical_execution_at_unix_ms: Option<u64>,
    pub hypothetical_inclusion_block: Option<u64>,
    pub replay_pnl: Option<SignedAmount>,
    pub opportunity_expires_at_unix_ms: u64,
    pub post_opportunity_market_movement_bps: Option<BasisPoints>,
    pub missed_opportunity_reason: Option<RejectionReason>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct Opportunity {
    pub identity: OpportunityIdentity,
    pub route: RouteEvidence,
    pub market: MarketEvidence,
    pub economics: ScenarioEconomics,
    pub simulation: SimulationEvidence,
    pub decision: DecisionEvidence,
    pub outcome: OutcomeEvidence,
}

impl Opportunity {
    pub fn validate_traceability(&self) -> Result<(), &'static str> {
        if self.identity.chain_id != 42161 {
            return Err("unsupported chain");
        }
        if self.route.token_path.len() < 2 || self.route.exact_ordered_legs.is_empty() {
            return Err("route evidence incomplete");
        }
        if self.market.state_block == 0 || self.market.quote_block == 0 {
            return Err("block context missing");
        }
        let lifecycle = self.market.independent_verification_lifecycle.as_slice();
        let lifecycle_valid = match self.market.independent_verification_status {
            IndependentVerificationStatus::NotRequested => {
                lifecycle == [IndependentVerificationStatus::NotRequested]
            }
            IndependentVerificationStatus::Requested => false,
            final_status => lifecycle == [IndependentVerificationStatus::Requested, final_status],
        };
        if !lifecycle_valid {
            return Err("independent verification lifecycle invalid");
        }
        if self.decision.disposition == ShadowDisposition::Rejected
            && self.decision.primary_rejection_reason.is_none()
        {
            return Err("rejected decision missing primary reason");
        }
        if !self.decision.shadow_only
            || self.decision.execution_eligible
            || self.decision.execution_request_created
        {
            return Err("shadow opportunity violated execution safety");
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rejection_reason_labels_are_bounded() {
        assert_eq!(
            RejectionReason::RpcStateDisagreement.as_str(),
            "rpc_state_disagreement"
        );
        assert!(!RejectionReason::DuplicateOpportunity
            .as_str()
            .contains("0x"));
    }

    #[test]
    fn signed_amount_represents_loss() {
        assert!(SignedAmount(-1) < SignedAmount(0));
    }
}
