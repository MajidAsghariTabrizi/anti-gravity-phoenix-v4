use serde::{Deserialize, Serialize};
use thiserror::Error;

pub const AAVE_SCREEN_REQUEST_SCHEMA: &str = "phoenix.rpc.aave-screen-request.v1";
pub const AAVE_SCREEN_RESPONSE_SCHEMA: &str = "phoenix.rpc.aave-screen-response.v2";
pub const AAVE_EXACT_REQUEST_SCHEMA: &str = "phoenix.rpc.aave-exact-request.v3";
pub const AAVE_EXACT_RESPONSE_SCHEMA: &str = "phoenix.rpc.aave-exact-response.v4";
pub const AAVE_SIMULATE_REQUEST_SCHEMA: &str = "phoenix.rpc.aave-simulate-request.v3";
pub const AAVE_SIMULATE_RESPONSE_SCHEMA: &str = "phoenix.rpc.aave-simulate-response.v4";
pub const AAVE_SIMULATE_BATCH_REQUEST_SCHEMA: &str = "phoenix.rpc.aave-simulate-batch-request.v2";
pub const AAVE_SIMULATE_BATCH_RESPONSE_SCHEMA: &str = "phoenix.rpc.aave-simulate-batch-response.v3";
pub const AAVE_TAIL_REQUEST_SCHEMA: &str = "phoenix.rpc.aave-tail-request.v1";
pub const AAVE_TAIL_RESPONSE_SCHEMA: &str = "phoenix.rpc.aave-tail-response.v2";
pub const AAVE_PRIMARY_PROVIDER_ID: &str = "production-nownodes-arbitrum";
pub const SINGLE_PRIMARY_FORK_EVIDENCE: &str = "SINGLE_PRIMARY_FORK_VERIFIED";
pub const SINGLE_PRIMARY_COUNTERFACTUAL_FORK_EVIDENCE: &str =
    "SINGLE_PRIMARY_COUNTERFACTUAL_FORK_VERIFIED";
pub const SINGLE_PRIMARY_ATLAS_CALLBACK_FORK_EVIDENCE: &str =
    "SINGLE_PRIMARY_ATLAS_CALLBACK_FORK_VERIFIED";
pub const AAVE_V3_POOL_ARBITRUM: &str = "0x794a61358d6845594f94dc1db02a252b5b4814ad";
pub const MAX_AAVE_SCREEN_ADDRESSES: usize = 100;
pub const MAX_AAVE_SIMULATION_BATCH: usize = 8;
pub const MAXIMUM_REVIEWED_INPUT_WEI: u128 = 10_000_000_000_000_000;

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum EconomicPhase {
    DisarmedDeploy,
    DisarmedEvidence,
    CanaryReady,
    LiveCanaryMin,
    LiveScaleL1,
    LiveScaleL2,
    LiveScaleL3,
    LiveScaleL4,
    LiveScaleL5,
    LiveMaxReviewed,
    Cooldown,
    DisarmedFailure,
}

impl EconomicPhase {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::DisarmedDeploy => "DISARMED_DEPLOY",
            Self::DisarmedEvidence => "DISARMED_EVIDENCE",
            Self::CanaryReady => "CANARY_READY",
            Self::LiveCanaryMin => "LIVE_CANARY_MIN",
            Self::LiveScaleL1 => "LIVE_SCALE_L1",
            Self::LiveScaleL2 => "LIVE_SCALE_L2",
            Self::LiveScaleL3 => "LIVE_SCALE_L3",
            Self::LiveScaleL4 => "LIVE_SCALE_L4",
            Self::LiveScaleL5 => "LIVE_SCALE_L5",
            Self::LiveMaxReviewed => "LIVE_MAX_REVIEWED",
            Self::Cooldown => "COOLDOWN",
            Self::DisarmedFailure => "DISARMED_FAILURE",
        }
    }

    pub const fn is_live(self) -> bool {
        matches!(
            self,
            Self::LiveCanaryMin
                | Self::LiveScaleL1
                | Self::LiveScaleL2
                | Self::LiveScaleL3
                | Self::LiveScaleL4
                | Self::LiveScaleL5
                | Self::LiveMaxReviewed
        )
    }
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, Ord, PartialEq, PartialOrd, Serialize)]
pub enum SizeLevel {
    #[serde(rename = "MIN")]
    Min,
    #[serde(rename = "L1")]
    L1,
    #[serde(rename = "L2")]
    L2,
    #[serde(rename = "L3")]
    L3,
    #[serde(rename = "L4")]
    L4,
    #[serde(rename = "L5")]
    L5,
    #[serde(rename = "MAX_REVIEWED")]
    MaxReviewed,
}

impl SizeLevel {
    pub const ALL: [Self; 7] = [
        Self::Min,
        Self::L1,
        Self::L2,
        Self::L3,
        Self::L4,
        Self::L5,
        Self::MaxReviewed,
    ];

    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Min => "MIN",
            Self::L1 => "L1",
            Self::L2 => "L2",
            Self::L3 => "L3",
            Self::L4 => "L4",
            Self::L5 => "L5",
            Self::MaxReviewed => "MAX_REVIEWED",
        }
    }

    pub const fn amount_wei(self) -> u128 {
        match self {
            Self::Min => 100_000_000_000_000,
            Self::L1 => 250_000_000_000_000,
            Self::L2 => 500_000_000_000_000,
            Self::L3 => 1_000_000_000_000_000,
            Self::L4 => 2_500_000_000_000_000,
            Self::L5 => 5_000_000_000_000_000,
            Self::MaxReviewed => MAXIMUM_REVIEWED_INPUT_WEI,
        }
    }

    pub const fn phase(self) -> EconomicPhase {
        match self {
            Self::Min => EconomicPhase::LiveCanaryMin,
            Self::L1 => EconomicPhase::LiveScaleL1,
            Self::L2 => EconomicPhase::LiveScaleL2,
            Self::L3 => EconomicPhase::LiveScaleL3,
            Self::L4 => EconomicPhase::LiveScaleL4,
            Self::L5 => EconomicPhase::LiveScaleL5,
            Self::MaxReviewed => EconomicPhase::LiveMaxReviewed,
        }
    }

    pub const fn next(self) -> Option<Self> {
        match self {
            Self::Min => Some(Self::L1),
            Self::L1 => Some(Self::L2),
            Self::L2 => Some(Self::L3),
            Self::L3 => Some(Self::L4),
            Self::L4 => Some(Self::L5),
            Self::L5 => Some(Self::MaxReviewed),
            Self::MaxReviewed => None,
        }
    }

    pub const fn previous(self) -> Self {
        match self {
            Self::Min | Self::L1 => Self::Min,
            Self::L2 => Self::L1,
            Self::L3 => Self::L2,
            Self::L4 => Self::L3,
            Self::L5 => Self::L4,
            Self::MaxReviewed => Self::L5,
        }
    }
}

impl TryFrom<&str> for SizeLevel {
    type Error = ();

    fn try_from(value: &str) -> Result<Self, Self::Error> {
        match value {
            "MIN" => Ok(Self::Min),
            "L1" => Ok(Self::L1),
            "L2" => Ok(Self::L2),
            "L3" => Ok(Self::L3),
            "L4" => Ok(Self::L4),
            "L5" => Ok(Self::L5),
            "MAX_REVIEWED" => Ok(Self::MaxReviewed),
            _ => Err(()),
        }
    }
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct AaveScreenRequest {
    pub schema_version: String,
    pub chain_id: u64,
    pub request_id: String,
    pub borrowers: Vec<String>,
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct AaveAccountData {
    pub borrower: String,
    pub total_collateral_base: String,
    pub total_debt_base: String,
    pub available_borrows_base: String,
    pub current_liquidation_threshold_bps: String,
    pub loan_to_value_bps: String,
    pub health_factor_wad: String,
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct AaveProviderScreen {
    pub provider_id: String,
    pub weth_price_base: String,
    pub accounts: Vec<AaveAccountData>,
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct AaveScreenResponse {
    pub schema_version: String,
    pub chain_id: u64,
    pub request_id: String,
    pub block_number: u64,
    pub block_hash: String,
    pub primary: AaveProviderScreen,
    pub confirmation: Option<AaveProviderScreen>,
    pub quorum: u8,
    pub resolved_at_unix_ms: u64,
}

// A primary screen is discovery-only input.  It deliberately has no
// independent-provider assertion and therefore must never be used to create
// execution authority.  The observer always obtains a fresh AaveExactResponse
// before it can materialize a candidate.
pub const AAVE_PRIMARY_SCREEN_RESPONSE_SCHEMA: &str = "phoenix.rpc.aave-primary-screen-response.v1";

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct AavePrimaryScreenResponse {
    pub schema_version: String,
    pub chain_id: u64,
    pub request_id: String,
    pub block_number: u64,
    pub block_hash: String,
    pub primary: AaveProviderScreen,
    pub resolved_at_unix_ms: u64,
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct AaveExactRequest {
    pub schema_version: String,
    pub chain_id: u64,
    pub request_id: String,
    pub borrower: String,
    pub maximum_input_amount: String,
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct AaveExactReserveState {
    pub asset: String,
    pub reserve_id: u16,
    pub decimals: u8,
    pub current_a_token_balance: String,
    pub current_stable_debt: String,
    pub current_variable_debt: String,
    pub usage_as_collateral_enabled: bool,
    pub configuration_data: String,
    pub a_token: String,
    pub stable_debt_token: String,
    pub variable_debt_token: String,
    pub oracle_price_base: String,
    pub liquidation_grace_period_until: u64,
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct AaveExactProviderState {
    pub provider_id: String,
    pub pool_code_hash: String,
    pub pool_implementation: String,
    pub pool_implementation_code_hash: String,
    pub user_configuration: String,
    pub user_emode_category: u8,
    pub emode_collateral_bitmap: String,
    pub emode_liquidation_bonus_bps: u16,
    pub flash_premium_bps: u16,
    pub account: AaveAccountData,
    pub reserves: Vec<AaveExactReserveState>,
    pub liquidations: Vec<AaveExactLiquidationState>,
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct AaveExactLiquidationState {
    pub debt_asset: String,
    pub collateral_asset: String,
    pub requested_repay_amount: String,
    pub actual_repay_amount: String,
    pub repay_amount: String,
    pub flash_premium_amount: String,
    pub seized_collateral: String,
    pub protocol_fee_collateral: String,
    pub liquidator_collateral: String,
    pub oracle_unwind_output_weth: String,
    pub unwind_quotes: Vec<AaveExactUnwindQuoteState>,
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct AaveExactUnwindQuoteState {
    pub pool: String,
    pub factory: String,
    pub token0: String,
    pub token1: String,
    pub fee: u32,
    pub zero_for_one: bool,
    pub output_weth: String,
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct AaveExactResponse {
    pub schema_version: String,
    pub chain_id: u64,
    pub request_id: String,
    pub block_number: u64,
    pub block_hash: String,
    pub state_root: String,
    pub primary: AaveExactProviderState,
    pub confirmation: Option<AaveExactProviderState>,
    pub quorum: u8,
    pub resolved_at_unix_ms: u64,
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct AaveSimulateRequest {
    pub schema_version: String,
    pub chain_id: u64,
    pub request_id: String,
    pub block_number: u64,
    pub block_hash: String,
    pub state_root: String,
    pub executor_address: String,
    pub executor_code_hash: String,
    pub caller_address: String,
    pub release_sha: String,
    pub borrower: String,
    pub debt_asset: String,
    pub collateral_asset: String,
    pub repay_amount: String,
    pub maximum_input_amount: String,
    pub live_maximum_input_amount: String,
    pub counterfactual: bool,
    pub minimum_collateral_received: String,
    pub minimum_unwind_output: String,
    pub minimum_profit: String,
    pub expected_profit: String,
    pub retained_profit_floor: String,
    pub selected_pool: String,
    pub selected_factory: String,
    pub selected_fee: u32,
    pub zero_for_one: bool,
    pub gas_limit: u64,
    pub max_fee_per_gas: String,
    pub max_priority_fee_per_gas: String,
    pub deadline_unix_seconds: u64,
    pub atlas_mode: bool,
    pub atlas_bid: String,
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct AaveSimulateResponse {
    pub schema_version: String,
    pub chain_id: u64,
    pub request_id: String,
    pub block_number: u64,
    pub block_hash: String,
    pub state_root: String,
    pub primary_provider_id: String,
    pub confirmation_provider_id: Option<String>,
    pub quorum: u8,
    pub evidence_mode: String,
    pub route_id: String,
    pub calldata_hex: String,
    pub calldata_hash: String,
    pub simulation_result_hash: String,
    pub realized_profit: String,
    pub conservative_net_pnl: String,
    pub estimated_gas_limit: u64,
    pub estimated_max_fee_per_gas_wei: String,
    pub estimated_execution_cost_wei: String,
    pub estimated_l1_cost_wei: String,
    pub flash_premium_wei: String,
    pub deadline_unix_seconds: u64,
    pub resolved_at_unix_ms: u64,
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct AaveSimulateBatchRequest {
    pub schema_version: String,
    pub chain_id: u64,
    pub request_id: String,
    pub simulations: Vec<AaveSimulateRequest>,
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct AaveSimulateBatchError {
    pub error_class: String,
    pub retryable: bool,
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct AaveSimulateBatchResult {
    pub request_id: String,
    pub response: Option<AaveSimulateResponse>,
    pub error: Option<AaveSimulateBatchError>,
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct AaveSimulateBatchResponse {
    pub schema_version: String,
    pub chain_id: u64,
    pub request_id: String,
    pub block_number: u64,
    pub block_hash: String,
    pub state_root: String,
    pub primary_provider_id: String,
    pub confirmation_provider_id: Option<String>,
    pub quorum: u8,
    pub evidence_mode: String,
    pub results: Vec<AaveSimulateBatchResult>,
    pub resolved_at_unix_ms: u64,
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct AaveTailRequest {
    pub schema_version: String,
    pub chain_id: u64,
    pub request_id: String,
    pub from_block: u64,
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct AaveTailResponse {
    pub schema_version: String,
    pub chain_id: u64,
    pub request_id: String,
    pub finalized_block_number: u64,
    pub finalized_block_hash: String,
    pub from_block: u64,
    pub to_block: u64,
    pub next_block: u64,
    pub primary_provider_id: String,
    pub confirmation_provider_id: Option<String>,
    pub quorum: u8,
    pub borrowers: Vec<String>,
    pub resolved_at_unix_ms: u64,
}

impl AaveTailRequest {
    pub fn validate(&self) -> Result<(), AaveStateError> {
        if self.schema_version != AAVE_TAIL_REQUEST_SCHEMA
            || self.chain_id != 42_161
            || self.request_id.is_empty()
            || self.request_id.len() > 256
            || self.request_id.chars().any(char::is_control)
        {
            return Err(AaveStateError::Invalid);
        }
        Ok(())
    }
}

impl AaveTailResponse {
    pub fn validate(&self, request: &AaveTailRequest) -> Result<(), AaveStateError> {
        request.validate()?;
        if self.schema_version != AAVE_TAIL_RESPONSE_SCHEMA
            || self.chain_id != request.chain_id
            || self.request_id != request.request_id
            || self.finalized_block_number == 0
            || !canonical_block_hash(&self.finalized_block_hash)
            || self.next_block != self.to_block.saturating_add(1)
            || self.to_block > self.finalized_block_number
            || self.primary_provider_id != AAVE_PRIMARY_PROVIDER_ID
            || self.confirmation_provider_id.is_some()
            || self.quorum != 1
            || self.borrowers.len() > 1_024
            || self.borrowers.iter().any(|value| !canonical_address(value))
            || self
                .borrowers
                .windows(2)
                .any(|values| values[0] >= values[1])
        {
            return Err(AaveStateError::ProviderDisagreement);
        }
        if request.from_block == 0 {
            if self.from_block != self.finalized_block_number.saturating_add(1)
                || self.to_block != self.finalized_block_number
                || !self.borrowers.is_empty()
            {
                return Err(AaveStateError::ProviderDisagreement);
            }
        } else {
            if self.from_block != request.from_block {
                return Err(AaveStateError::ProviderDisagreement);
            }
            let checkpoint = self.from_block == self.finalized_block_number.saturating_add(1)
                && self.to_block == self.finalized_block_number
                && self.borrowers.is_empty();
            if !checkpoint
                && (self.to_block < self.from_block
                    || self.to_block.saturating_sub(self.from_block) >= 256)
            {
                return Err(AaveStateError::ProviderDisagreement);
            }
        }
        Ok(())
    }
}

impl AaveScreenRequest {
    pub fn validate(&self) -> Result<(), AaveStateError> {
        if self.schema_version != AAVE_SCREEN_REQUEST_SCHEMA
            || self.chain_id != 42_161
            || self.request_id.is_empty()
            || self.request_id.len() > 256
            || self.request_id.chars().any(char::is_control)
            || self.borrowers.is_empty()
            || self.borrowers.len() > MAX_AAVE_SCREEN_ADDRESSES
        {
            return Err(AaveStateError::Invalid);
        }
        let mut seen = std::collections::HashSet::new();
        if self
            .borrowers
            .iter()
            .any(|address| !canonical_address(address) || !seen.insert(address))
        {
            return Err(AaveStateError::Invalid);
        }
        Ok(())
    }
}

impl AaveExactRequest {
    pub fn validate(&self) -> Result<(), AaveStateError> {
        if self.schema_version != AAVE_EXACT_REQUEST_SCHEMA
            || self.chain_id != 42_161
            || self.request_id.is_empty()
            || self.request_id.len() > 256
            || self.request_id.chars().any(char::is_control)
            || !canonical_address(&self.borrower)
            || self.maximum_input_amount.parse::<u128>().ok() != Some(MAXIMUM_REVIEWED_INPUT_WEI)
        {
            return Err(AaveStateError::Invalid);
        }
        Ok(())
    }
}

impl AaveSimulateRequest {
    pub fn validate(&self) -> Result<(), AaveStateError> {
        let zero_leg = self.debt_asset == self.collateral_asset;
        let route_identity_valid = if zero_leg {
            self.selected_pool == "0x0000000000000000000000000000000000000000"
                && self.selected_factory == "0x0000000000000000000000000000000000000000"
                && self.selected_fee == 0
                && !self.zero_for_one
        } else {
            canonical_address(&self.selected_pool)
                && canonical_address(&self.selected_factory)
                && matches!(self.selected_fee, 100 | 500 | 3_000)
        };
        let positive = [
            &self.repay_amount,
            &self.maximum_input_amount,
            &self.live_maximum_input_amount,
            &self.minimum_collateral_received,
            &self.minimum_unwind_output,
            &self.minimum_profit,
            &self.expected_profit,
            &self.retained_profit_floor,
            &self.max_fee_per_gas,
            &self.max_priority_fee_per_gas,
        ]
        .into_iter()
        .all(|value| value.parse::<u128>().ok().is_some_and(|parsed| parsed > 0));
        if self.schema_version != AAVE_SIMULATE_REQUEST_SCHEMA
            || self.chain_id != 42_161
            || self.request_id.is_empty()
            || self.request_id.len() > 256
            || self.request_id.chars().any(char::is_control)
            || self.block_number == 0
            || !canonical_block_hash(&self.block_hash)
            || !canonical_block_hash(&self.state_root)
            || !canonical_address(&self.executor_address)
            || !canonical_address(&self.caller_address)
            || !canonical_address(&self.borrower)
            || !canonical_address(&self.debt_asset)
            || !canonical_address(&self.collateral_asset)
            || !route_identity_valid
            || !canonical_digest(&self.executor_code_hash)
            || !canonical_release_sha(&self.release_sha)
            || !positive
            || self.gas_limit == 0
            || self.deadline_unix_seconds == 0
            || self.atlas_bid.parse::<u128>().ok().is_none()
        {
            return Err(AaveStateError::Invalid);
        }
        let priority = self
            .max_priority_fee_per_gas
            .parse::<u128>()
            .map_err(|_| AaveStateError::Invalid)?;
        let maximum = self
            .max_fee_per_gas
            .parse::<u128>()
            .map_err(|_| AaveStateError::Invalid)?;
        if priority > maximum {
            return Err(AaveStateError::Invalid);
        }
        let repay = self
            .repay_amount
            .parse::<u128>()
            .map_err(|_| AaveStateError::Invalid)?;
        let maximum_input = self
            .maximum_input_amount
            .parse::<u128>()
            .map_err(|_| AaveStateError::Invalid)?;
        let live_maximum_input = self
            .live_maximum_input_amount
            .parse::<u128>()
            .map_err(|_| AaveStateError::Invalid)?;
        if repay > maximum_input
            || maximum_input > MAXIMUM_REVIEWED_INPUT_WEI
            || live_maximum_input > maximum_input
            || self.counterfactual != (repay > live_maximum_input)
            || (!self.counterfactual && maximum_input != live_maximum_input)
            || (self.counterfactual && maximum_input != MAXIMUM_REVIEWED_INPUT_WEI)
        {
            return Err(AaveStateError::Invalid);
        }
        Ok(())
    }
}

fn canonical_release_sha(value: &str) -> bool {
    value.len() == 40
        && value
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
}

impl AaveSimulateResponse {
    pub fn validate(&self, request: &AaveSimulateRequest) -> Result<(), AaveStateError> {
        request.validate()?;
        let expected_evidence_mode = if request.atlas_mode {
            SINGLE_PRIMARY_ATLAS_CALLBACK_FORK_EVIDENCE
        } else if request.counterfactual {
            SINGLE_PRIMARY_COUNTERFACTUAL_FORK_EVIDENCE
        } else {
            SINGLE_PRIMARY_FORK_EVIDENCE
        };
        if self.schema_version != AAVE_SIMULATE_RESPONSE_SCHEMA
            || self.chain_id != request.chain_id
            || self.request_id != request.request_id
            || self.block_number != request.block_number
            || self.block_hash != request.block_hash
            || self.state_root != request.state_root
            || self.primary_provider_id != AAVE_PRIMARY_PROVIDER_ID
            || self.confirmation_provider_id.is_some()
            || self.quorum != 1
            || self.evidence_mode != expected_evidence_mode
            || !canonical_block_hash(&self.route_id)
            || !canonical_data(&self.calldata_hex)
            || !canonical_digest(&self.calldata_hash)
            || !canonical_digest(&self.simulation_result_hash)
            || self.deadline_unix_seconds != request.deadline_unix_seconds
            || self.realized_profit.parse::<u128>().ok().is_none()
            || self.conservative_net_pnl.parse::<u128>().ok().is_none()
            || self.estimated_gas_limit == 0
            || self.estimated_gas_limit > request.gas_limit
            || self
                .estimated_max_fee_per_gas_wei
                .parse::<u128>()
                .ok()
                .map_or(true, |value| value == 0)
            || self
                .estimated_max_fee_per_gas_wei
                .parse::<u128>()
                .ok()
                .zip(request.max_fee_per_gas.parse::<u128>().ok())
                .map_or(true, |(estimated, maximum)| estimated > maximum)
            || self
                .estimated_execution_cost_wei
                .parse::<u128>()
                .ok()
                .map_or(true, |value| value == 0)
            || self.estimated_l1_cost_wei.parse::<u128>().ok().is_none()
            || self.flash_premium_wei.parse::<u128>().ok().is_none()
        {
            return Err(AaveStateError::ProviderDisagreement);
        }
        let quoted_fee = self
            .estimated_max_fee_per_gas_wei
            .parse::<u128>()
            .map_err(|_| AaveStateError::ProviderDisagreement)?;
        let execution_cost = self
            .estimated_execution_cost_wei
            .parse::<u128>()
            .map_err(|_| AaveStateError::ProviderDisagreement)?;
        if u128::from(self.estimated_gas_limit).checked_mul(quoted_fee) != Some(execution_cost) {
            return Err(AaveStateError::ProviderDisagreement);
        }
        Ok(())
    }
}

impl AaveSimulateBatchRequest {
    pub fn validate(&self) -> Result<(), AaveStateError> {
        if self.schema_version != AAVE_SIMULATE_BATCH_REQUEST_SCHEMA
            || self.chain_id != 42_161
            || self.request_id.is_empty()
            || self.request_id.len() > 256
            || self.request_id.chars().any(char::is_control)
            || self.simulations.is_empty()
            || self.simulations.len() > MAX_AAVE_SIMULATION_BATCH
        {
            return Err(AaveStateError::Invalid);
        }
        let first = &self.simulations[0];
        let mut request_ids = std::collections::HashSet::new();
        for simulation in &self.simulations {
            simulation.validate()?;
            if simulation.chain_id != self.chain_id
                || !request_ids.insert(simulation.request_id.as_str())
                || simulation.block_number != first.block_number
                || simulation.block_hash != first.block_hash
                || simulation.state_root != first.state_root
                || simulation.executor_address != first.executor_address
                || simulation.executor_code_hash != first.executor_code_hash
                || simulation.caller_address != first.caller_address
                || simulation.release_sha != first.release_sha
                || simulation.borrower != first.borrower
                || simulation.debt_asset != first.debt_asset
                || simulation.live_maximum_input_amount != first.live_maximum_input_amount
                || simulation.retained_profit_floor != first.retained_profit_floor
                || simulation.gas_limit != first.gas_limit
                || simulation.max_fee_per_gas != first.max_fee_per_gas
                || simulation.max_priority_fee_per_gas != first.max_priority_fee_per_gas
                || simulation.deadline_unix_seconds != first.deadline_unix_seconds
                || simulation.atlas_mode != first.atlas_mode
            {
                return Err(AaveStateError::Invalid);
            }
        }
        Ok(())
    }
}

impl AaveSimulateBatchResponse {
    pub fn validate(&self, request: &AaveSimulateBatchRequest) -> Result<(), AaveStateError> {
        request.validate()?;
        let first = &request.simulations[0];
        let expected_evidence_mode = if first.atlas_mode {
            SINGLE_PRIMARY_ATLAS_CALLBACK_FORK_EVIDENCE
        } else if first.counterfactual {
            SINGLE_PRIMARY_COUNTERFACTUAL_FORK_EVIDENCE
        } else {
            SINGLE_PRIMARY_FORK_EVIDENCE
        };
        if self.schema_version != AAVE_SIMULATE_BATCH_RESPONSE_SCHEMA
            || self.chain_id != request.chain_id
            || self.request_id != request.request_id
            || self.block_number != first.block_number
            || self.block_hash != first.block_hash
            || self.state_root != first.state_root
            || self.primary_provider_id != AAVE_PRIMARY_PROVIDER_ID
            || self.confirmation_provider_id.is_some()
            || self.quorum != 1
            || self.evidence_mode != expected_evidence_mode
            || self.results.len() != request.simulations.len()
        {
            return Err(AaveStateError::ProviderDisagreement);
        }
        for (result, simulation) in self.results.iter().zip(&request.simulations) {
            if result.request_id != simulation.request_id {
                return Err(AaveStateError::ProviderDisagreement);
            }
            match (&result.response, &result.error) {
                (Some(response), None) => response.validate(simulation)?,
                (None, Some(error))
                    if !error.error_class.is_empty()
                        && error.error_class.len() <= 64
                        && error
                            .error_class
                            .bytes()
                            .all(|byte| byte.is_ascii_lowercase() || byte == b'_') => {}
                _ => return Err(AaveStateError::ProviderDisagreement),
            }
        }
        Ok(())
    }
}

impl AaveExactResponse {
    pub fn validate(&self, request: &AaveExactRequest) -> Result<(), AaveStateError> {
        request.validate()?;
        let maximum_input = request
            .maximum_input_amount
            .parse::<u128>()
            .map_err(|_| AaveStateError::Invalid)?;
        if self.schema_version != AAVE_EXACT_RESPONSE_SCHEMA
            || self.chain_id != request.chain_id
            || self.request_id != request.request_id
            || self.block_number == 0
            || !canonical_block_hash(&self.block_hash)
            || !canonical_block_hash(&self.state_root)
            || self.primary.provider_id != AAVE_PRIMARY_PROVIDER_ID
            || self.confirmation.is_some()
            || self.quorum != 1
            || self.primary.account.borrower != request.borrower
            || self.primary.reserves.len() != 2
            || self.primary.liquidations.len() > SizeLevel::ALL.len() * 2
        {
            return Err(AaveStateError::ProviderDisagreement);
        }
        let mut seen = std::collections::HashSet::new();
        for liquidation in &self.primary.liquidations {
            let requested = liquidation
                .requested_repay_amount
                .parse::<u128>()
                .map_err(|_| AaveStateError::ProviderDisagreement)?;
            let actual = liquidation
                .actual_repay_amount
                .parse::<u128>()
                .map_err(|_| AaveStateError::ProviderDisagreement)?;
            let repay = liquidation
                .repay_amount
                .parse::<u128>()
                .map_err(|_| AaveStateError::ProviderDisagreement)?;
            let seized_collateral = liquidation
                .seized_collateral
                .parse::<u128>()
                .map_err(|_| AaveStateError::ProviderDisagreement)?;
            let protocol_fee_collateral = liquidation
                .protocol_fee_collateral
                .parse::<u128>()
                .map_err(|_| AaveStateError::ProviderDisagreement)?;
            let liquidator_collateral = liquidation
                .liquidator_collateral
                .parse::<u128>()
                .map_err(|_| AaveStateError::ProviderDisagreement)?;
            let oracle_unwind_output = liquidation
                .oracle_unwind_output_weth
                .parse::<u128>()
                .map_err(|_| AaveStateError::ProviderDisagreement)?;
            let expected_premium = repay
                .checked_mul(u128::from(self.primary.flash_premium_bps))
                .and_then(|value| value.checked_add(5_000))
                .map(|value| value / 10_000)
                .ok_or(AaveStateError::ProviderDisagreement)?;
            if requested == 0
                || requested != actual
                || actual != repay
                || repay > maximum_input
                || liquidation.flash_premium_amount.parse::<u128>().ok() != Some(expected_premium)
                || !canonical_address(&liquidation.debt_asset)
                || !canonical_address(&liquidation.collateral_asset)
                || liquidator_collateral == 0
                || oracle_unwind_output == 0
                || protocol_fee_collateral.checked_add(liquidator_collateral)
                    != Some(seized_collateral)
                || !seen.insert((liquidation.collateral_asset.clone(), repay))
            {
                return Err(AaveStateError::ProviderDisagreement);
            }
            if liquidation.collateral_asset == liquidation.debt_asset {
                if oracle_unwind_output != liquidator_collateral
                    || !liquidation.unwind_quotes.is_empty()
                {
                    return Err(AaveStateError::ProviderDisagreement);
                }
                continue;
            }
            let mut quoted_pools = std::collections::HashSet::new();
            if liquidation.unwind_quotes.is_empty()
                || liquidation.unwind_quotes.len() > 4
                || liquidation.unwind_quotes.iter().any(|quote| {
                    let tokens_match = (quote.token0 == liquidation.collateral_asset
                        && quote.token1 == liquidation.debt_asset)
                        || (quote.token1 == liquidation.collateral_asset
                            && quote.token0 == liquidation.debt_asset);
                    !canonical_address(&quote.pool)
                        || !canonical_address(&quote.factory)
                        || !canonical_address(&quote.token0)
                        || !canonical_address(&quote.token1)
                        || !tokens_match
                        || !matches!(quote.fee, 100 | 500 | 3_000)
                        || quote.zero_for_one != (quote.token0 == liquidation.collateral_asset)
                        || quote
                            .output_weth
                            .parse::<u128>()
                            .ok()
                            .map_or(true, |value| value == 0)
                        || !quoted_pools.insert((quote.pool.clone(), quote.fee))
                })
            {
                return Err(AaveStateError::ProviderDisagreement);
            }
        }
        Ok(())
    }
}

impl AaveScreenResponse {
    pub fn validate(&self, request: &AaveScreenRequest) -> Result<(), AaveStateError> {
        request.validate()?;
        if self.schema_version != AAVE_SCREEN_RESPONSE_SCHEMA
            || self.chain_id != request.chain_id
            || self.request_id != request.request_id
            || self.block_number == 0
            || !canonical_block_hash(&self.block_hash)
            || self.primary.provider_id != AAVE_PRIMARY_PROVIDER_ID
            || self.confirmation.is_some()
            || self.quorum != 1
            || self.primary.accounts.len() != request.borrowers.len()
            || self.primary.weth_price_base == "0"
            || self
                .primary
                .accounts
                .iter()
                .zip(&request.borrowers)
                .any(|(account, borrower)| account.borrower != *borrower)
        {
            return Err(AaveStateError::ProviderDisagreement);
        }
        Ok(())
    }
}

impl AavePrimaryScreenResponse {
    pub fn validate(&self, request: &AaveScreenRequest) -> Result<(), AaveStateError> {
        request.validate()?;
        if self.schema_version != AAVE_PRIMARY_SCREEN_RESPONSE_SCHEMA
            || self.chain_id != request.chain_id
            || self.request_id != request.request_id
            || self.block_number == 0
            || !canonical_block_hash(&self.block_hash)
            || self.primary.provider_id.is_empty()
            || self.primary.accounts.len() != request.borrowers.len()
            || self.primary.weth_price_base == "0"
            || self
                .primary
                .accounts
                .iter()
                .zip(&request.borrowers)
                .any(|(account, borrower)| account.borrower != *borrower)
        {
            return Err(AaveStateError::Invalid);
        }
        Ok(())
    }
}

fn canonical_address(value: &str) -> bool {
    value.len() == 42
        && value.starts_with("0x")
        && value[2..]
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
        && value[2..].bytes().any(|byte| byte != b'0')
}

fn canonical_block_hash(value: &str) -> bool {
    value.len() == 66
        && value.starts_with("0x")
        && value[2..]
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
}

fn canonical_digest(value: &str) -> bool {
    value.len() == 64
        && value
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
}

fn canonical_data(value: &str) -> bool {
    value.len() >= 10
        && value.len() % 2 == 0
        && value.starts_with("0x")
        && value[2..].bytes().all(|byte| byte.is_ascii_hexdigit())
}

#[derive(Clone, Copy, Debug, Error, PartialEq, Eq)]
pub enum AaveStateError {
    #[error("Aave screen request is invalid")]
    Invalid,
    #[error("Aave screen providers disagree")]
    ProviderDisagreement,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn batch_is_bounded_unique_and_canonical() {
        let request = AaveScreenRequest {
            schema_version: AAVE_SCREEN_REQUEST_SCHEMA.to_string(),
            chain_id: 42_161,
            request_id: "batch-1100".to_string(),
            borrowers: vec!["0x1111111111111111111111111111111111111111".to_string()],
        };
        assert_eq!(request.validate(), Ok(()));
        let mut duplicate = request.clone();
        duplicate.borrowers.push(duplicate.borrowers[0].clone());
        assert_eq!(duplicate.validate(), Err(AaveStateError::Invalid));
    }

    #[test]
    fn simulation_request_binds_exact_execution_identity() {
        let mut request = AaveSimulateRequest {
            schema_version: AAVE_SIMULATE_REQUEST_SCHEMA.to_string(),
            chain_id: 42_161,
            request_id: "candidate-1".to_string(),
            block_number: 49_000_000,
            block_hash: format!("0x{}", "1".repeat(64)),
            state_root: format!("0x{}", "2".repeat(64)),
            executor_address: "0x1111111111111111111111111111111111111111".to_string(),
            executor_code_hash: "3".repeat(64),
            caller_address: "0x2222222222222222222222222222222222222222".to_string(),
            release_sha: "4".repeat(40),
            borrower: "0x3333333333333333333333333333333333333333".to_string(),
            debt_asset: "0x82af49447d8a07e3bd95bd0d56f35241523fbab1".to_string(),
            collateral_asset: "0xaf88d065e77c8cc2239327c5edb3a432268e5831".to_string(),
            repay_amount: "1000000".to_string(),
            maximum_input_amount: "2000000".to_string(),
            live_maximum_input_amount: "2000000".to_string(),
            counterfactual: false,
            minimum_collateral_received: "2000000".to_string(),
            minimum_unwind_output: "1100000".to_string(),
            minimum_profit: "10000".to_string(),
            expected_profit: "20000".to_string(),
            retained_profit_floor: "1000".to_string(),
            selected_pool: "0xc6962004f452be9203591991d15f6b388e09e8d0".to_string(),
            selected_factory: "0x1f98431c8ad98523631ae4a59f267346ea31f984".to_string(),
            selected_fee: 500,
            zero_for_one: false,
            gas_limit: 500_000,
            max_fee_per_gas: "100".to_string(),
            max_priority_fee_per_gas: "10".to_string(),
            deadline_unix_seconds: 1_900_000_000,
            atlas_mode: false,
            atlas_bid: "0".to_string(),
        };
        assert_eq!(request.validate(), Ok(()));
        request.executor_code_hash = format!("0x{}", "3".repeat(64));
        assert_eq!(request.validate(), Err(AaveStateError::Invalid));
    }

    #[test]
    fn atlas_callback_evidence_is_single_primary() {
        let mut request = AaveSimulateRequest {
            schema_version: AAVE_SIMULATE_REQUEST_SCHEMA.to_string(),
            chain_id: 42_161,
            request_id: "atlas-candidate-1".to_string(),
            block_number: 49_000_000,
            block_hash: format!("0x{}", "1".repeat(64)),
            state_root: format!("0x{}", "2".repeat(64)),
            executor_address: "0x1111111111111111111111111111111111111111".to_string(),
            executor_code_hash: "3".repeat(64),
            caller_address: "0x2222222222222222222222222222222222222222".to_string(),
            release_sha: "4".repeat(40),
            borrower: "0x3333333333333333333333333333333333333333".to_string(),
            debt_asset: "0x82af49447d8a07e3bd95bd0d56f35241523fbab1".to_string(),
            collateral_asset: "0xaf88d065e77c8cc2239327c5edb3a432268e5831".to_string(),
            repay_amount: "1000000".to_string(),
            maximum_input_amount: "2000000".to_string(),
            live_maximum_input_amount: "2000000".to_string(),
            counterfactual: false,
            minimum_collateral_received: "2000000".to_string(),
            minimum_unwind_output: "1100000".to_string(),
            minimum_profit: "10000".to_string(),
            expected_profit: "20000".to_string(),
            retained_profit_floor: "1000".to_string(),
            selected_pool: "0xc6962004f452be9203591991d15f6b388e09e8d0".to_string(),
            selected_factory: "0x1f98431c8ad98523631ae4a59f267346ea31f984".to_string(),
            selected_fee: 500,
            zero_for_one: false,
            gas_limit: 500_000,
            max_fee_per_gas: "100".to_string(),
            max_priority_fee_per_gas: "10".to_string(),
            deadline_unix_seconds: 1_900_000_000,
            atlas_mode: true,
            atlas_bid: "1".to_string(),
        };
        let response = AaveSimulateResponse {
            schema_version: AAVE_SIMULATE_RESPONSE_SCHEMA.to_string(),
            chain_id: request.chain_id,
            request_id: request.request_id.clone(),
            block_number: request.block_number,
            block_hash: request.block_hash.clone(),
            state_root: request.state_root.clone(),
            primary_provider_id: AAVE_PRIMARY_PROVIDER_ID.to_string(),
            confirmation_provider_id: None,
            quorum: 1,
            evidence_mode: SINGLE_PRIMARY_ATLAS_CALLBACK_FORK_EVIDENCE.to_string(),
            route_id: format!("0x{}", "5".repeat(64)),
            calldata_hex: "0x12345678".to_string(),
            calldata_hash: "6".repeat(64),
            simulation_result_hash: "7".repeat(64),
            realized_profit: "20000".to_string(),
            conservative_net_pnl: "15000".to_string(),
            estimated_gas_limit: 100_000,
            estimated_max_fee_per_gas_wei: "100".to_string(),
            estimated_execution_cost_wei: "10000000".to_string(),
            estimated_l1_cost_wei: "1000".to_string(),
            flash_premium_wei: "500".to_string(),
            deadline_unix_seconds: request.deadline_unix_seconds,
            resolved_at_unix_ms: 1,
        };
        assert_eq!(response.validate(&request), Ok(()));
        request.atlas_mode = false;
        request.atlas_bid = "0".to_string();
        assert_eq!(
            response.validate(&request),
            Err(AaveStateError::ProviderDisagreement)
        );
    }
}
