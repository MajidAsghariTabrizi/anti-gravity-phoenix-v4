use serde::{Deserialize, Serialize};
use thiserror::Error;

pub const AAVE_SCREEN_REQUEST_SCHEMA: &str = "phoenix.rpc.aave-screen-request.v1";
pub const AAVE_SCREEN_RESPONSE_SCHEMA: &str = "phoenix.rpc.aave-screen-response.v2";
pub const AAVE_EXACT_REQUEST_SCHEMA: &str = "phoenix.rpc.aave-exact-request.v3";
pub const AAVE_EXACT_RESPONSE_SCHEMA: &str = "phoenix.rpc.aave-exact-response.v5";
pub const AAVE_SIMULATE_REQUEST_SCHEMA: &str = "phoenix.rpc.aave-simulate-request.v4";
pub const AAVE_SIMULATE_RESPONSE_SCHEMA: &str = "phoenix.rpc.aave-simulate-response.v5";
pub const AAVE_SIMULATE_BATCH_REQUEST_SCHEMA: &str = "phoenix.rpc.aave-simulate-batch-request.v3";
pub const AAVE_SIMULATE_BATCH_RESPONSE_SCHEMA: &str = "phoenix.rpc.aave-simulate-batch-response.v4";
pub const AAVE_TAIL_REQUEST_SCHEMA: &str = "phoenix.rpc.aave-tail-request.v1";
pub const AAVE_TAIL_RESPONSE_SCHEMA: &str = "phoenix.rpc.aave-tail-response.v2";
pub const AAVE_PRIMARY_PROVIDER_ID: &str = "production-nownodes-arbitrum";
pub const SINGLE_PRIMARY_FORK_EVIDENCE: &str = "SINGLE_PRIMARY_FORK_VERIFIED";
pub const SINGLE_PRIMARY_COUNTERFACTUAL_FORK_EVIDENCE: &str =
    "SINGLE_PRIMARY_COUNTERFACTUAL_FORK_VERIFIED";
pub const SINGLE_PRIMARY_ATLAS_CALLBACK_FORK_EVIDENCE: &str =
    "SINGLE_PRIMARY_ATLAS_CALLBACK_FORK_VERIFIED";
pub const AAVE_V3_POOL_ARBITRUM: &str = "0x794a61358d6845594f94dc1db02a252b5b4814ad";
pub const ARBITRUM_WETH: &str = "0x82af49447d8a07e3bd95bd0d56f35241523fbab1";
pub const ARBITRUM_NATIVE_USDC: &str = "0xaf88d065e77c8cc2239327c5edb3a432268e5831";
pub const ARBITRUM_USDC_E: &str = "0xff970a61a04b1ca14834a43f5de4533ebddb5cc8";
const UNISWAP_V3_FACTORY_ARBITRUM: &str = "0x1f98431c8ad98523631ae4a59f267346ea31f984";
const WETH_NATIVE_USDC_POOL_100: &str = "0x6f38e884725a116c9c7fbf208e79fe8828a2595f";
const WETH_NATIVE_USDC_POOL_500: &str = "0xc6962004f452be9203591991d15f6b388e09e8d0";
const WETH_NATIVE_USDC_POOL_3000: &str = "0xc473e2aee3441bf9240be85eb122abb059a3b57c";
const WETH_USDC_E_POOL_500: &str = "0xc31e54c7a869b9fcbecc14363cf510d1c41fa443";
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
    pub debt_asset_decimals: u8,
    pub debt_asset_price_base: String,
    pub weth_price_base: String,
    pub maximum_repay_amount: String,
    pub reviewed_size_weth_wei: String,
    pub debt_asset_review: String,
    pub size_classification: String,
    pub terminal_size_reason: String,
    pub requested_repay_amount: String,
    pub actual_repay_amount: String,
    pub repay_amount: String,
    pub flash_premium_amount: String,
    pub seized_collateral: String,
    pub protocol_fee_collateral: String,
    pub liquidator_collateral: String,
    pub oracle_unwind_output_weth: String,
    pub oracle_unwind_output_debt_asset: String,
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
    pub output_debt_asset: String,
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
    pub debt_asset_decimals: u8,
    pub debt_asset_price_base: String,
    pub weth_price_base: String,
    pub repay_amount: String,
    pub maximum_input_amount: String,
    pub live_maximum_input_amount: String,
    pub maximum_input_weth_wei: String,
    pub live_maximum_input_weth_wei: String,
    pub counterfactual: bool,
    pub minimum_collateral_received: String,
    pub minimum_unwind_output: String,
    pub minimum_profit: String,
    pub minimum_profit_weth_wei: String,
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
    pub realized_profit_debt_asset: String,
    pub conservative_net_pnl: String,
    pub estimated_gas_limit: u64,
    pub estimated_max_fee_per_gas_wei: String,
    pub estimated_execution_cost_wei: String,
    pub estimated_l1_cost_wei: String,
    pub flash_premium_wei: String,
    pub flash_premium_debt_asset: String,
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
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub revert_reason: Option<String>,
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
            &self.maximum_input_weth_wei,
            &self.live_maximum_input_weth_wei,
            &self.debt_asset_price_base,
            &self.weth_price_base,
            &self.minimum_collateral_received,
            &self.minimum_unwind_output,
            &self.minimum_profit,
            &self.minimum_profit_weth_wei,
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
        let maximum_input_weth = parse_positive_u128(&self.maximum_input_weth_wei)?;
        let live_maximum_input_weth = parse_positive_u128(&self.live_maximum_input_weth_wei)?;
        let debt_price = parse_positive_u128(&self.debt_asset_price_base)?;
        let weth_price = parse_positive_u128(&self.weth_price_base)?;
        let minimum_profit = parse_positive_u128(&self.minimum_profit)?;
        let minimum_profit_weth = parse_positive_u128(&self.minimum_profit_weth_wei)?;
        let supported_debt = (self.debt_asset == ARBITRUM_WETH
            && self.debt_asset_decimals == 18
            && debt_price == weth_price)
            || (self.debt_asset == ARBITRUM_USDC_E && self.debt_asset_decimals == 6);
        let derived_maximum = weth_to_debt_floor(
            maximum_input_weth,
            weth_price,
            debt_price,
            self.debt_asset_decimals,
        )?;
        let derived_live_maximum = weth_to_debt_floor(
            live_maximum_input_weth,
            weth_price,
            debt_price,
            self.debt_asset_decimals,
        )?;
        let derived_minimum_profit = weth_to_debt_ceil(
            minimum_profit_weth,
            weth_price,
            debt_price,
            self.debt_asset_decimals,
        )?;
        if repay > maximum_input
            || !supported_debt
            || maximum_input_weth > MAXIMUM_REVIEWED_INPUT_WEI
            || maximum_input != derived_maximum
            || live_maximum_input > maximum_input
            || live_maximum_input_weth > maximum_input_weth
            || live_maximum_input != derived_live_maximum
            || minimum_profit != derived_minimum_profit
            || self.counterfactual != (repay > live_maximum_input)
            || (!self.counterfactual
                && (maximum_input != live_maximum_input
                    || maximum_input_weth != live_maximum_input_weth))
            || (self.counterfactual && maximum_input_weth != MAXIMUM_REVIEWED_INPUT_WEI)
            || self.atlas_mode && self.debt_asset != ARBITRUM_WETH
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

fn parse_positive_u128(value: &str) -> Result<u128, AaveStateError> {
    value
        .parse::<u128>()
        .ok()
        .filter(|parsed| *parsed > 0)
        .ok_or(AaveStateError::Invalid)
}

fn asset_unit_u128(decimals: u8) -> Result<u128, AaveStateError> {
    10_u128
        .checked_pow(u32::from(decimals))
        .ok_or(AaveStateError::Invalid)
}

fn weth_to_debt_floor(
    weth_wei: u128,
    weth_price: u128,
    debt_price: u128,
    debt_decimals: u8,
) -> Result<u128, AaveStateError> {
    if debt_decimals == 18 && debt_price == weth_price {
        return Ok(weth_wei);
    }
    let numerator = weth_wei
        .checked_mul(weth_price)
        .and_then(|value| value.checked_mul(asset_unit_u128(debt_decimals).ok()?))
        .ok_or(AaveStateError::Invalid)?;
    let denominator = 1_000_000_000_000_000_000_u128
        .checked_mul(debt_price)
        .ok_or(AaveStateError::Invalid)?;
    Ok(numerator / denominator)
}

fn weth_to_debt_ceil(
    weth_wei: u128,
    weth_price: u128,
    debt_price: u128,
    debt_decimals: u8,
) -> Result<u128, AaveStateError> {
    if debt_decimals == 18 && debt_price == weth_price {
        return Ok(weth_wei);
    }
    let numerator = weth_wei
        .checked_mul(weth_price)
        .and_then(|value| value.checked_mul(asset_unit_u128(debt_decimals).ok()?))
        .ok_or(AaveStateError::Invalid)?;
    let denominator = 1_000_000_000_000_000_000_u128
        .checked_mul(debt_price)
        .ok_or(AaveStateError::Invalid)?;
    numerator
        .checked_add(denominator - 1)
        .map(|value| value / denominator)
        .ok_or(AaveStateError::Invalid)
}

fn debt_to_weth_floor(
    debt_raw: u128,
    debt_price: u128,
    weth_price: u128,
    debt_decimals: u8,
) -> Result<u128, AaveStateError> {
    if debt_decimals == 18 && debt_price == weth_price {
        return Ok(debt_raw);
    }
    let numerator = debt_raw
        .checked_mul(debt_price)
        .and_then(|value| value.checked_mul(1_000_000_000_000_000_000_u128))
        .ok_or(AaveStateError::Invalid)?;
    let denominator = asset_unit_u128(debt_decimals)?
        .checked_mul(weth_price)
        .ok_or(AaveStateError::Invalid)?;
    Ok(numerator / denominator)
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
            || self
                .realized_profit_debt_asset
                .parse::<u128>()
                .ok()
                .is_none()
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
            || self.flash_premium_debt_asset.parse::<u128>().ok().is_none()
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
        let debt_price = parse_positive_u128(&request.debt_asset_price_base)?;
        let weth_price = parse_positive_u128(&request.weth_price_base)?;
        let realized_raw = self
            .realized_profit_debt_asset
            .parse::<u128>()
            .map_err(|_| AaveStateError::ProviderDisagreement)?;
        let premium_raw = self
            .flash_premium_debt_asset
            .parse::<u128>()
            .map_err(|_| AaveStateError::ProviderDisagreement)?;
        if self.realized_profit.parse::<u128>().ok()
            != Some(debt_to_weth_floor(
                realized_raw,
                debt_price,
                weth_price,
                request.debt_asset_decimals,
            )?)
            || self.flash_premium_wei.parse::<u128>().ok()
                != Some(debt_to_weth_floor(
                    premium_raw,
                    debt_price,
                    weth_price,
                    request.debt_asset_decimals,
                )?)
        {
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
                || simulation.debt_asset_decimals != first.debt_asset_decimals
                || simulation.debt_asset_price_base != first.debt_asset_price_base
                || simulation.weth_price_base != first.weth_price_base
                || simulation.live_maximum_input_amount != first.live_maximum_input_amount
                || simulation.live_maximum_input_weth_wei != first.live_maximum_input_weth_wei
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
        let maximum_input_weth = request
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
            || self.primary.reserves.len() != 3
            || self.primary.liquidations.len() > SizeLevel::ALL.len() * 3
        {
            return Err(AaveStateError::ProviderDisagreement);
        }
        let weth_reserve = self
            .primary
            .reserves
            .iter()
            .find(|reserve| reserve.asset == ARBITRUM_WETH)
            .ok_or(AaveStateError::ProviderDisagreement)?;
        let weth_price = parse_positive_u128(&weth_reserve.oracle_price_base)?;
        if weth_reserve.decimals != 18
            || self
                .primary
                .reserves
                .iter()
                .map(|reserve| reserve.asset.as_str())
                .collect::<std::collections::HashSet<_>>()
                != std::collections::HashSet::from([
                    ARBITRUM_WETH,
                    ARBITRUM_NATIVE_USDC,
                    ARBITRUM_USDC_E,
                ])
        {
            return Err(AaveStateError::ProviderDisagreement);
        }
        let mut seen = std::collections::HashSet::new();
        let mut terminal_collateral = std::collections::HashSet::new();
        let mut fixed_collateral = std::collections::HashSet::new();
        for liquidation in &self.primary.liquidations {
            let debt_reserve = self
                .primary
                .reserves
                .iter()
                .find(|reserve| reserve.asset == liquidation.debt_asset)
                .ok_or(AaveStateError::ProviderDisagreement)?;
            let debt_price = parse_positive_u128(&debt_reserve.oracle_price_base)?;
            let supported_pair = (liquidation.debt_asset == ARBITRUM_WETH
                && matches!(
                    liquidation.collateral_asset.as_str(),
                    ARBITRUM_WETH | "0xaf88d065e77c8cc2239327c5edb3a432268e5831"
                ))
                || (liquidation.debt_asset == ARBITRUM_USDC_E
                    && liquidation.collateral_asset == ARBITRUM_WETH);
            let expected_review = if liquidation.debt_asset == ARBITRUM_USDC_E {
                "usdc_e_debt_reviewed"
            } else {
                "weth_debt_reviewed"
            };
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
            let oracle_unwind_output_raw = liquidation
                .oracle_unwind_output_debt_asset
                .parse::<u128>()
                .map_err(|_| AaveStateError::ProviderDisagreement)?;
            let maximum_repay = liquidation
                .maximum_repay_amount
                .parse::<u128>()
                .map_err(|_| AaveStateError::ProviderDisagreement)?;
            let reviewed_size_weth = liquidation
                .reviewed_size_weth_wei
                .parse::<u128>()
                .map_err(|_| AaveStateError::ProviderDisagreement)?;
            let pair_key = format!(
                "{}|{}",
                liquidation.debt_asset, liquidation.collateral_asset
            );
            let expected_premium = repay
                .checked_mul(u128::from(self.primary.flash_premium_bps))
                .and_then(|value| value.checked_add(5_000))
                .map(|value| value / 10_000)
                .ok_or(AaveStateError::ProviderDisagreement)?;
            let classification_invalid = match liquidation.size_classification.as_str() {
                "fixed_reviewed_size" => {
                    let conflicts = terminal_collateral.contains(&pair_key);
                    fixed_collateral.insert(pair_key.clone());
                    !liquidation.terminal_size_reason.is_empty()
                        || !SizeLevel::ALL
                            .iter()
                            .any(|level| level.amount_wei() == reviewed_size_weth)
                        || weth_to_debt_floor(
                            reviewed_size_weth,
                            weth_price,
                            debt_price,
                            debt_reserve.decimals,
                        )? != repay
                        || conflicts
                }
                "terminal_size_required" => {
                    !matches!(
                        liquidation.terminal_size_reason.as_str(),
                        "below_min_reviewed_size" | "dust_partial_invalid"
                    ) || SizeLevel::ALL
                        .iter()
                        .any(|level| level.amount_wei() == reviewed_size_weth)
                        || debt_to_weth_floor(repay, debt_price, weth_price, debt_reserve.decimals)?
                            != reviewed_size_weth
                        || fixed_collateral.contains(&pair_key)
                        || !terminal_collateral.insert(pair_key.clone())
                }
                _ => true,
            };
            if requested == 0
                || requested != actual
                || actual != repay
                || !supported_pair
                || debt_reserve.decimals != liquidation.debt_asset_decimals
                || debt_reserve.oracle_price_base != liquidation.debt_asset_price_base
                || weth_reserve.oracle_price_base != liquidation.weth_price_base
                || liquidation.debt_asset_review != expected_review
                || maximum_repay
                    != weth_to_debt_floor(
                        maximum_input_weth,
                        weth_price,
                        debt_price,
                        debt_reserve.decimals,
                    )?
                || repay > maximum_repay
                || classification_invalid
                || liquidation.flash_premium_amount.parse::<u128>().ok() != Some(expected_premium)
                || !canonical_address(&liquidation.debt_asset)
                || !canonical_address(&liquidation.collateral_asset)
                || liquidator_collateral == 0
                || oracle_unwind_output_raw == 0
                || oracle_unwind_output == 0
                || oracle_unwind_output
                    != debt_to_weth_floor(
                        oracle_unwind_output_raw,
                        debt_price,
                        weth_price,
                        debt_reserve.decimals,
                    )?
                || protocol_fee_collateral.checked_add(liquidator_collateral)
                    != Some(seized_collateral)
                || !seen.insert((pair_key, repay))
            {
                return Err(AaveStateError::ProviderDisagreement);
            }
            if liquidation.collateral_asset == liquidation.debt_asset {
                if oracle_unwind_output_raw != liquidator_collateral
                    || !liquidation.unwind_quotes.is_empty()
                {
                    return Err(AaveStateError::ProviderDisagreement);
                }
                continue;
            }
            let mut quoted_pools = std::collections::HashSet::new();
            let expected_quote_count = if liquidation.debt_asset == ARBITRUM_USDC_E {
                1
            } else {
                3
            };
            if liquidation.unwind_quotes.is_empty()
                || liquidation.unwind_quotes.len() != expected_quote_count
                || liquidation.unwind_quotes.iter().any(|quote| {
                    let tokens_match = (quote.token0 == liquidation.collateral_asset
                        && quote.token1 == liquidation.debt_asset)
                        || (quote.token1 == liquidation.collateral_asset
                            && quote.token0 == liquidation.debt_asset);
                    let reviewed_route = if liquidation.debt_asset == ARBITRUM_USDC_E {
                        quote.pool == WETH_USDC_E_POOL_500
                            && quote.factory == UNISWAP_V3_FACTORY_ARBITRUM
                            && quote.token0 == ARBITRUM_WETH
                            && quote.token1 == ARBITRUM_USDC_E
                            && quote.fee == 500
                            && quote.zero_for_one
                    } else {
                        quote.factory == UNISWAP_V3_FACTORY_ARBITRUM
                            && quote.token0 == ARBITRUM_WETH
                            && quote.token1 == ARBITRUM_NATIVE_USDC
                            && !quote.zero_for_one
                            && matches!(
                                (quote.pool.as_str(), quote.fee),
                                (WETH_NATIVE_USDC_POOL_100, 100)
                                    | (WETH_NATIVE_USDC_POOL_500, 500)
                                    | (WETH_NATIVE_USDC_POOL_3000, 3_000)
                            )
                    };
                    !canonical_address(&quote.pool)
                        || !canonical_address(&quote.factory)
                        || !canonical_address(&quote.token0)
                        || !canonical_address(&quote.token1)
                        || !tokens_match
                        || !reviewed_route
                        || quote.zero_for_one != (quote.token0 == liquidation.collateral_asset)
                        || quote
                            .output_debt_asset
                            .parse::<u128>()
                            .ok()
                            .map_or(true, |raw| {
                                raw == 0
                                    || quote.output_weth.parse::<u128>().ok()
                                        != debt_to_weth_floor(
                                            raw,
                                            debt_price,
                                            weth_price,
                                            debt_reserve.decimals,
                                        )
                                        .ok()
                            })
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
            debt_asset_decimals: 18,
            debt_asset_price_base: "200000000000".to_string(),
            weth_price_base: "200000000000".to_string(),
            repay_amount: "1000000".to_string(),
            maximum_input_amount: "2000000".to_string(),
            live_maximum_input_amount: "2000000".to_string(),
            maximum_input_weth_wei: "2000000".to_string(),
            live_maximum_input_weth_wei: "2000000".to_string(),
            counterfactual: false,
            minimum_collateral_received: "2000000".to_string(),
            minimum_unwind_output: "1100000".to_string(),
            minimum_profit: "10000".to_string(),
            minimum_profit_weth_wei: "10000".to_string(),
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
            debt_asset_decimals: 18,
            debt_asset_price_base: "200000000000".to_string(),
            weth_price_base: "200000000000".to_string(),
            repay_amount: "1000000".to_string(),
            maximum_input_amount: "2000000".to_string(),
            live_maximum_input_amount: "2000000".to_string(),
            maximum_input_weth_wei: "2000000".to_string(),
            live_maximum_input_weth_wei: "2000000".to_string(),
            counterfactual: false,
            minimum_collateral_received: "2000000".to_string(),
            minimum_unwind_output: "1100000".to_string(),
            minimum_profit: "10000".to_string(),
            minimum_profit_weth_wei: "10000".to_string(),
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
            realized_profit_debt_asset: "20000".to_string(),
            conservative_net_pnl: "15000".to_string(),
            estimated_gas_limit: 100_000,
            estimated_max_fee_per_gas_wei: "100".to_string(),
            estimated_execution_cost_wei: "10000000".to_string(),
            estimated_l1_cost_wei: "1000".to_string(),
            flash_premium_wei: "500".to_string(),
            flash_premium_debt_asset: "500".to_string(),
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

    #[test]
    fn usdc_e_weth_conversion_rounds_maximum_down_and_profit_floor_up() {
        let weth_price = 200_000_000_000_u128;
        let usdc_e_price = 100_000_000_u128;
        assert_eq!(
            weth_to_debt_floor(MAXIMUM_REVIEWED_INPUT_WEI, weth_price, usdc_e_price, 6,).unwrap(),
            20_000_000
        );
        assert_eq!(
            weth_to_debt_ceil(1_000_000_000_000, weth_price, usdc_e_price, 6).unwrap(),
            2_000
        );
        assert_eq!(
            weth_to_debt_floor(1_000_000_000_000, 200_000_000_000, 300_000_000, 6).unwrap(),
            666
        );
        assert_eq!(
            weth_to_debt_ceil(1_000_000_000_000, 200_000_000_000, 300_000_000, 6).unwrap(),
            667
        );
    }
}
