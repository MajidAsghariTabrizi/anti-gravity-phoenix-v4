use serde::{Deserialize, Serialize};
use thiserror::Error;

pub const AAVE_SCREEN_REQUEST_SCHEMA: &str = "phoenix.rpc.aave-screen-request.v1";
pub const AAVE_SCREEN_RESPONSE_SCHEMA: &str = "phoenix.rpc.aave-screen-response.v1";
pub const AAVE_EXACT_REQUEST_SCHEMA: &str = "phoenix.rpc.aave-exact-request.v1";
pub const AAVE_EXACT_RESPONSE_SCHEMA: &str = "phoenix.rpc.aave-exact-response.v1";
pub const AAVE_SIMULATE_REQUEST_SCHEMA: &str = "phoenix.rpc.aave-simulate-request.v1";
pub const AAVE_SIMULATE_RESPONSE_SCHEMA: &str = "phoenix.rpc.aave-simulate-response.v1";
pub const AAVE_TAIL_REQUEST_SCHEMA: &str = "phoenix.rpc.aave-tail-request.v1";
pub const AAVE_TAIL_RESPONSE_SCHEMA: &str = "phoenix.rpc.aave-tail-response.v1";
pub const AAVE_V3_POOL_ARBITRUM: &str = "0x794a61358d6845594f94dc1db02a252b5b4814ad";
pub const MAX_AAVE_SCREEN_ADDRESSES: usize = 100;

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
    pub secondary: AaveProviderScreen,
    pub resolved_at_unix_ms: u64,
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct AaveExactRequest {
    pub schema_version: String,
    pub chain_id: u64,
    pub request_id: String,
    pub borrower: String,
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct AaveExactReserveState {
    pub asset: String,
    pub current_a_token_balance: String,
    pub current_stable_debt: String,
    pub current_variable_debt: String,
    pub usage_as_collateral_enabled: bool,
    pub configuration_data: String,
    pub a_token: String,
    pub stable_debt_token: String,
    pub variable_debt_token: String,
    pub oracle_price_base: String,
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct AaveExactProviderState {
    pub provider_id: String,
    pub pool_code_hash: String,
    pub pool_implementation: String,
    pub pool_implementation_code_hash: String,
    pub user_configuration: String,
    pub account: AaveAccountData,
    pub reserves: Vec<AaveExactReserveState>,
    pub liquidation: Option<AaveExactLiquidationState>,
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct AaveExactLiquidationState {
    pub debt_asset: String,
    pub collateral_asset: String,
    pub repay_amount: String,
    pub seized_collateral: String,
    pub protocol_fee_collateral: String,
    pub liquidator_collateral: String,
    pub uniswap_v3_fee_500_output_weth: String,
    pub uniswap_v3_fee_3000_output_weth: String,
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
    pub secondary: AaveExactProviderState,
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
    pub secondary_provider_id: String,
    pub evidence_mode: String,
    pub route_id: String,
    pub calldata_hex: String,
    pub calldata_hash: String,
    pub simulation_result_hash: String,
    pub realized_profit: String,
    pub conservative_net_pnl: String,
    pub deadline_unix_seconds: u64,
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
    pub secondary_provider_id: String,
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
            || self.primary_provider_id.is_empty()
            || self.secondary_provider_id.is_empty()
            || self.primary_provider_id == self.secondary_provider_id
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
        {
            return Err(AaveStateError::Invalid);
        }
        Ok(())
    }
}

impl AaveSimulateRequest {
    pub fn validate(&self) -> Result<(), AaveStateError> {
        let positive = [
            &self.repay_amount,
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
            || !canonical_address(&self.selected_pool)
            || !canonical_address(&self.selected_factory)
            || !canonical_digest(&self.executor_code_hash)
            || !canonical_digest(&self.release_sha)
            || !positive
            || self.selected_fee != 500 && self.selected_fee != 3_000
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
        Ok(())
    }
}

impl AaveSimulateResponse {
    pub fn validate(&self, request: &AaveSimulateRequest) -> Result<(), AaveStateError> {
        request.validate()?;
        if self.schema_version != AAVE_SIMULATE_RESPONSE_SCHEMA
            || self.chain_id != request.chain_id
            || self.request_id != request.request_id
            || self.block_number != request.block_number
            || self.block_hash != request.block_hash
            || self.state_root != request.state_root
            || self.primary_provider_id.is_empty()
            || self.secondary_provider_id.is_empty()
            || self.primary_provider_id == self.secondary_provider_id
            || self.evidence_mode != "DUAL_PROVIDER_FORK_VERIFIED"
            || !canonical_block_hash(&self.route_id)
            || !canonical_data(&self.calldata_hex)
            || !canonical_digest(&self.calldata_hash)
            || !canonical_digest(&self.simulation_result_hash)
            || self.deadline_unix_seconds != request.deadline_unix_seconds
            || self.realized_profit.parse::<u128>().ok().is_none()
            || self.conservative_net_pnl.parse::<u128>().ok().is_none()
        {
            return Err(AaveStateError::ProviderDisagreement);
        }
        Ok(())
    }
}

impl AaveExactResponse {
    pub fn validate(&self, request: &AaveExactRequest) -> Result<(), AaveStateError> {
        request.validate()?;
        let mut primary = self.primary.clone();
        let mut secondary = self.secondary.clone();
        primary.provider_id.clear();
        secondary.provider_id.clear();
        if self.schema_version != AAVE_EXACT_RESPONSE_SCHEMA
            || self.chain_id != request.chain_id
            || self.request_id != request.request_id
            || self.block_number == 0
            || !canonical_block_hash(&self.block_hash)
            || !canonical_block_hash(&self.state_root)
            || self.primary.provider_id.is_empty()
            || self.secondary.provider_id.is_empty()
            || self.primary.provider_id == self.secondary.provider_id
            || primary != secondary
            || self.primary.account.borrower != request.borrower
            || self.primary.reserves.len() != 2
        {
            return Err(AaveStateError::ProviderDisagreement);
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
            || self.primary.provider_id.is_empty()
            || self.secondary.provider_id.is_empty()
            || self.primary.provider_id == self.secondary.provider_id
            || self.primary.accounts.len() != request.borrowers.len()
            || self.secondary.accounts.len() != request.borrowers.len()
            || self.primary.accounts != self.secondary.accounts
            || self.primary.weth_price_base != self.secondary.weth_price_base
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
            release_sha: "4".repeat(64),
            borrower: "0x3333333333333333333333333333333333333333".to_string(),
            debt_asset: "0x82af49447d8a07e3bd95bd0d56f35241523fbab1".to_string(),
            collateral_asset: "0xaf88d065e77c8cc2239327c5edb3a432268e5831".to_string(),
            repay_amount: "1000000".to_string(),
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
}
