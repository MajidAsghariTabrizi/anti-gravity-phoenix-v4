use crate::{
    AAVE_LIQUIDATION_ROUTE_FINGERPRINT, ARBITRUM_AAVE_V3_POOL_ADDRESS,
    ARBITRUM_NATIVE_USDC_ADDRESS, ARBITRUM_ONE_CHAIN_ID, ARBITRUM_USDC_E_ADDRESS,
    ARBITRUM_WETH_ADDRESS, CURRENT_ROUTE_FINGERPRINT, CURRENT_ROUTE_POOL_3000_ADDRESS,
    CURRENT_ROUTE_POOL_500_ADDRESS, REQUEST_SCHEMA_VERSION, REVERSE_ROUTE_FINGERPRINT,
    REVIEWED_WETH_USDC_E_POOL_500_ADDRESS,
};
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::fmt;
use thiserror::Error;
use uuid::Uuid;

pub const MAX_ROUTE_LEGS: usize = 4;
pub const MAX_APPROVER_BYTES: usize = 128;
pub const MAX_POLICY_VERSION_BYTES: usize = 128;
pub const MAX_ROUTE_FINGERPRINT_BYTES: usize = 256;

#[derive(Clone, Copy, PartialEq, Eq, Hash, Serialize)]
pub struct CanonicalAddress([u8; 20]);

impl CanonicalAddress {
    pub fn parse(value: &str) -> Result<Self, ModelError> {
        if value.len() != 42
            || !value.starts_with("0x")
            || !value[2..]
                .bytes()
                .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
        {
            return Err(ModelError::InvalidAddress);
        }
        let decoded = hex::decode(&value[2..]).map_err(|_| ModelError::InvalidAddress)?;
        let bytes: [u8; 20] = decoded.try_into().map_err(|_| ModelError::InvalidAddress)?;
        if bytes == [0; 20] {
            return Err(ModelError::InvalidAddress);
        }
        Ok(Self(bytes))
    }

    pub const fn as_bytes(&self) -> &[u8; 20] {
        &self.0
    }
}

impl fmt::Debug for CanonicalAddress {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        fmt::Display::fmt(self, formatter)
    }
}

impl fmt::Display for CanonicalAddress {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "0x{}", hex::encode(self.0))
    }
}

#[derive(Clone, Copy, PartialEq, Eq, Hash)]
pub struct TransactionHash([u8; 32]);

impl TransactionHash {
    pub fn parse(value: &str) -> Result<Self, ModelError> {
        if value.len() != 66 || !value.starts_with("0x") {
            return Err(ModelError::InvalidTransactionHash);
        }
        let decoded = hex::decode(&value[2..]).map_err(|_| ModelError::InvalidTransactionHash)?;
        let bytes: [u8; 32] = decoded
            .try_into()
            .map_err(|_| ModelError::InvalidTransactionHash)?;
        Ok(Self(bytes))
    }

    pub const fn from_bytes(bytes: [u8; 32]) -> Self {
        Self(bytes)
    }

    pub const fn as_bytes(&self) -> &[u8; 32] {
        &self.0
    }
}

impl fmt::Debug for TransactionHash {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        fmt::Display::fmt(self, formatter)
    }
}

impl fmt::Display for TransactionHash {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "0x{}", hex::encode(self.0))
    }
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct ExecutionLeg {
    pub pool: String,
    #[serde(default)]
    pub factory: Option<String>,
    pub token_in: String,
    pub token_out: String,
    pub fee: u32,
    pub zero_for_one: bool,
    pub min_amount_out: String,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ValidatedLeg {
    pub pool: CanonicalAddress,
    pub factory: Option<CanonicalAddress>,
    pub token_in: CanonicalAddress,
    pub token_out: CanonicalAddress,
    pub fee: u32,
    pub zero_for_one: bool,
    pub min_amount_out: u128,
}

#[derive(Clone, Copy, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum ExecutionRouteType {
    PhoenixDexV1,
    AaveLiquidationV1,
}

impl ExecutionRouteType {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::PhoenixDexV1 => "PHOENIX_DEX_V1",
            Self::AaveLiquidationV1 => AAVE_LIQUIDATION_ROUTE_FINGERPRINT,
        }
    }
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct RawAaveLiquidationRoute {
    pub borrower: String,
    pub debt_asset: String,
    pub collateral_asset: String,
    pub debt_asset_decimals: u8,
    pub debt_asset_price_base: String,
    pub weth_price_base: String,
    pub maximum_input_weth_wei: String,
    pub minimum_profit_weth_wei: String,
    pub receive_a_token: bool,
    pub minimum_collateral_received: String,
    pub minimum_unwind_output: String,
    pub maximum_atlas_bid: String,
    pub evidence_mode: String,
    pub state_root: String,
    pub release_sha: String,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AaveLiquidationRoute {
    pub borrower: CanonicalAddress,
    pub debt_asset: CanonicalAddress,
    pub collateral_asset: CanonicalAddress,
    pub debt_asset_decimals: u8,
    pub debt_asset_price_base: u128,
    pub weth_price_base: u128,
    pub maximum_input_weth_wei: u128,
    pub minimum_profit_weth_wei: u128,
    pub receive_a_token: bool,
    pub minimum_collateral_received: u128,
    pub minimum_unwind_output: u128,
    pub maximum_atlas_bid: u128,
    pub evidence_mode: String,
    pub state_root: String,
    pub release_sha: String,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ExecutionRequest {
    pub id: Uuid,
    pub opportunity_id: Uuid,
    pub schema_version: String,
    pub chain_id: u64,
    pub route_id: [u8; 32],
    pub route_fingerprint: String,
    pub route_type: ExecutionRouteType,
    pub aave_liquidation: Option<AaveLiquidationRoute>,
    pub selected_size: u128,
    pub token_path: Vec<CanonicalAddress>,
    pub origin_router: CanonicalAddress,
    pub executor_address: CanonicalAddress,
    pub executor_code_hash: String,
    pub calldata_hash: String,
    pub simulation_result_hash: String,
    pub plan_hash: String,
    pub pinned_block_number: u64,
    pub pinned_block_hash: String,
    pub flash_asset: CanonicalAddress,
    pub flash_amount: u128,
    pub maximum_input_amount: u128,
    pub minimum_profit: u128,
    pub expected_profit: u128,
    pub deadline: DateTime<Utc>,
    pub legs: Vec<ValidatedLeg>,
    pub gas_limit: u64,
    pub max_fee_per_gas: u128,
    pub max_priority_fee_per_gas: u128,
    pub approved_by: String,
    pub approved_at: DateTime<Utc>,
    pub approval_deadline: DateTime<Utc>,
    pub policy_version: String,
    pub approval_digest: String,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RawExecutionRequest {
    pub id: Uuid,
    pub opportunity_id: Uuid,
    pub schema_version: String,
    pub chain_id: i64,
    pub route_id: String,
    pub route_fingerprint: String,
    pub route_type: String,
    pub route_payload: Option<serde_json::Value>,
    pub selected_size: String,
    pub token_path: Vec<String>,
    pub origin_router: String,
    pub executor_address: String,
    pub executor_code_hash: String,
    pub calldata_hash: String,
    pub simulation_result_hash: String,
    pub plan_hash: String,
    pub pinned_block_number: i64,
    pub pinned_block_hash: String,
    pub flash_asset: String,
    pub flash_amount: String,
    pub maximum_input_amount: String,
    pub minimum_profit: String,
    pub expected_profit: String,
    pub deadline: DateTime<Utc>,
    pub legs: Vec<ExecutionLeg>,
    pub gas_limit: i64,
    pub max_fee_per_gas: String,
    pub max_priority_fee_per_gas: String,
    pub approved_by: String,
    pub approved_at: DateTime<Utc>,
    pub approval_deadline: DateTime<Utc>,
    pub policy_version: String,
    pub approval_digest: String,
}

#[derive(Serialize)]
struct ApprovalBody<'a> {
    schema_version: &'a str,
    request_id: String,
    opportunity_id: String,
    chain_id: u64,
    route_id: String,
    route_fingerprint: &'a str,
    route_type: &'a str,
    route_payload: Option<RawAaveLiquidationRoute>,
    selected_size: String,
    token_path: Vec<String>,
    origin_router: String,
    executor_address: String,
    executor_code_hash: &'a str,
    calldata_hash: &'a str,
    simulation_result_hash: &'a str,
    plan_hash: &'a str,
    pinned_block_number: u64,
    pinned_block_hash: &'a str,
    flash_asset: String,
    flash_amount: String,
    maximum_input_amount: String,
    minimum_profit: String,
    expected_profit: String,
    deadline_unix_seconds: i64,
    legs: &'a [ValidatedLeg],
    gas_limit: u64,
    max_fee_per_gas: String,
    max_priority_fee_per_gas: String,
    approved_by: &'a str,
    approved_at: String,
    approval_deadline: String,
    policy_version: &'a str,
}

impl Serialize for ValidatedLeg {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: serde::Serializer,
    {
        ExecutionLeg {
            pool: self.pool.to_string(),
            factory: self.factory.map(|address| address.to_string()),
            token_in: self.token_in.to_string(),
            token_out: self.token_out.to_string(),
            fee: self.fee,
            zero_for_one: self.zero_for_one,
            min_amount_out: self.min_amount_out.to_string(),
        }
        .serialize(serializer)
    }
}

impl RawExecutionRequest {
    pub fn validate(self) -> Result<ExecutionRequest, ModelError> {
        if self.schema_version != REQUEST_SCHEMA_VERSION {
            return Err(ModelError::SchemaVersion);
        }
        let chain_id = u64::try_from(self.chain_id).map_err(|_| ModelError::WrongChain)?;
        if chain_id != ARBITRUM_ONE_CHAIN_ID {
            return Err(ModelError::WrongChain);
        }
        let route_id = parse_fixed_hex::<32>(&self.route_id).ok_or(ModelError::InvalidRoute)?;
        let pinned_block_number = u64::try_from(self.pinned_block_number)
            .ok()
            .filter(|value| *value > 0)
            .ok_or(ModelError::InvalidApproval)?;
        let selected_size = parse_positive_u128(&self.selected_size)?;
        let flash_amount = parse_positive_u128(&self.flash_amount)?;
        let maximum_input_amount = parse_positive_u128(&self.maximum_input_amount)?;
        let minimum_profit = parse_positive_u128(&self.minimum_profit)?;
        let expected_profit = parse_positive_u128(&self.expected_profit)?;
        let gas_limit = u64::try_from(self.gas_limit)
            .ok()
            .filter(|value| *value > 0)
            .ok_or(ModelError::InvalidGas)?;
        let max_fee_per_gas = parse_positive_u128(&self.max_fee_per_gas)?;
        let max_priority_fee_per_gas = parse_positive_u128(&self.max_priority_fee_per_gas)?;
        if max_priority_fee_per_gas > max_fee_per_gas {
            return Err(ModelError::InvalidGas);
        }
        if self.legs.len() > MAX_ROUTE_LEGS {
            return Err(ModelError::InvalidLegs);
        }
        let legs = self
            .legs
            .into_iter()
            .map(ValidatedLeg::try_from)
            .collect::<Result<Vec<_>, _>>()?;
        let flash_asset = CanonicalAddress::parse(&self.flash_asset)?;
        let token_path = self
            .token_path
            .iter()
            .map(|address| CanonicalAddress::parse(address))
            .collect::<Result<Vec<_>, _>>()?;
        let (route_type, aave_liquidation) = match self.route_type.as_str() {
            "PHOENIX_DEX_V1" => {
                if self.route_payload.is_some() || legs.is_empty() {
                    return Err(ModelError::InvalidLegs);
                }
                validate_route(&legs, flash_asset)?;
                validate_token_path(&token_path, &legs, flash_asset)?;
                (ExecutionRouteType::PhoenixDexV1, None)
            }
            AAVE_LIQUIDATION_ROUTE_FINGERPRINT => {
                let raw: RawAaveLiquidationRoute = serde_json::from_value(
                    self.route_payload.clone().ok_or(ModelError::InvalidLegs)?,
                )
                .map_err(|_| ModelError::InvalidLegs)?;
                if raw.receive_a_token
                    || raw.evidence_mode != "EIP1186_VERIFIED"
                        && raw.evidence_mode != "DUAL_PROVIDER_FORK_VERIFIED"
                        && raw.evidence_mode != "SINGLE_PRIMARY_FORK_VERIFIED"
                    || !canonical_block_hash(&raw.state_root)
                    || !canonical_digest(&raw.release_sha)
                {
                    return Err(ModelError::InvalidApproval);
                }
                let route = AaveLiquidationRoute {
                    borrower: CanonicalAddress::parse(&raw.borrower)?,
                    debt_asset: CanonicalAddress::parse(&raw.debt_asset)?,
                    collateral_asset: CanonicalAddress::parse(&raw.collateral_asset)?,
                    debt_asset_decimals: raw.debt_asset_decimals,
                    debt_asset_price_base: parse_positive_u128(&raw.debt_asset_price_base)?,
                    weth_price_base: parse_positive_u128(&raw.weth_price_base)?,
                    maximum_input_weth_wei: parse_positive_u128(&raw.maximum_input_weth_wei)?,
                    minimum_profit_weth_wei: parse_positive_u128(&raw.minimum_profit_weth_wei)?,
                    receive_a_token: raw.receive_a_token,
                    minimum_collateral_received: parse_positive_u128(
                        &raw.minimum_collateral_received,
                    )?,
                    minimum_unwind_output: parse_positive_u128(&raw.minimum_unwind_output)?,
                    maximum_atlas_bid: raw
                        .maximum_atlas_bid
                        .parse::<u128>()
                        .map_err(|_| ModelError::InvalidAmount)?,
                    evidence_mode: raw.evidence_mode,
                    state_root: raw.state_root,
                    release_sha: raw.release_sha,
                };
                validate_aave_route(&route, &legs, &token_path, flash_asset)?;
                (ExecutionRouteType::AaveLiquidationV1, Some(route))
            }
            _ => return Err(ModelError::InvalidRoute),
        };
        if self.approved_by.trim().is_empty()
            || self.approved_by.len() > MAX_APPROVER_BYTES
            || self.policy_version.trim().is_empty()
            || self.policy_version.len() > MAX_POLICY_VERSION_BYTES
            || self.route_fingerprint.trim().is_empty()
            || self.route_fingerprint.len() > MAX_ROUTE_FINGERPRINT_BYTES
            || self.route_fingerprint.chars().any(char::is_control)
            || !canonical_digest(&self.executor_code_hash)
            || !canonical_digest(&self.calldata_hash)
            || !canonical_digest(&self.simulation_result_hash)
            || !canonical_digest(&self.plan_hash)
            || !canonical_block_hash(&self.pinned_block_hash)
            || !canonical_digest(&self.approval_digest)
            || selected_size != flash_amount
            || maximum_input_amount < selected_size
            || self.approved_at >= self.approval_deadline
            || self.approval_deadline > self.deadline
        {
            return Err(ModelError::InvalidApproval);
        }
        let request = ExecutionRequest {
            id: self.id,
            opportunity_id: self.opportunity_id,
            schema_version: self.schema_version,
            chain_id,
            route_id,
            route_fingerprint: self.route_fingerprint,
            route_type,
            aave_liquidation,
            selected_size,
            token_path,
            origin_router: CanonicalAddress::parse(&self.origin_router)?,
            executor_address: CanonicalAddress::parse(&self.executor_address)?,
            executor_code_hash: self.executor_code_hash,
            calldata_hash: self.calldata_hash,
            simulation_result_hash: self.simulation_result_hash,
            plan_hash: self.plan_hash,
            pinned_block_number,
            pinned_block_hash: self.pinned_block_hash,
            flash_asset,
            flash_amount,
            maximum_input_amount,
            minimum_profit,
            expected_profit,
            deadline: self.deadline,
            legs,
            gas_limit,
            max_fee_per_gas,
            max_priority_fee_per_gas,
            approved_by: self.approved_by,
            approved_at: self.approved_at,
            approval_deadline: self.approval_deadline,
            policy_version: self.policy_version,
            approval_digest: self.approval_digest,
        };
        request.validate_current_route()?;
        if request.canonical_approval_digest()? != request.approval_digest {
            return Err(ModelError::ApprovalDigestMismatch);
        }
        Ok(request)
    }
}

impl ExecutionRequest {
    pub fn validate_current_route(&self) -> Result<(), ModelError> {
        if self.route_type == ExecutionRouteType::AaveLiquidationV1 {
            let route = self
                .aave_liquidation
                .as_ref()
                .ok_or(ModelError::InvalidLegs)?;
            if self.route_fingerprint != AAVE_LIQUIDATION_ROUTE_FINGERPRINT
                || self.origin_router != CanonicalAddress::parse(ARBITRUM_AAVE_V3_POOL_ADDRESS)?
                || route.debt_asset != self.flash_asset
                || self.selected_size != self.flash_amount
                || route.maximum_atlas_bid != 0
            {
                return Err(ModelError::InvalidLegs);
            }
            return validate_aave_route(route, &self.legs, &self.token_path, self.flash_asset);
        }
        if self.route_type != ExecutionRouteType::PhoenixDexV1 || self.aave_liquidation.is_some() {
            return Err(ModelError::InvalidRoute);
        }
        let weth = CanonicalAddress::parse(ARBITRUM_WETH_ADDRESS)?;
        let usdc = CanonicalAddress::parse(ARBITRUM_NATIVE_USDC_ADDRESS)?;
        let pool_500 = CanonicalAddress::parse(CURRENT_ROUTE_POOL_500_ADDRESS)?;
        let pool_3000 = CanonicalAddress::parse(CURRENT_ROUTE_POOL_3000_ADDRESS)?;
        let route_matches = self.legs.len() == 2
            && ((self.route_fingerprint == CURRENT_ROUTE_FINGERPRINT
                && self.legs[0].pool == pool_500
                && self.legs[0].fee == 500
                && self.legs[1].pool == pool_3000
                && self.legs[1].fee == 3_000)
                || (self.route_fingerprint == REVERSE_ROUTE_FINGERPRINT
                    && self.legs[0].pool == pool_3000
                    && self.legs[0].fee == 3_000
                    && self.legs[1].pool == pool_500
                    && self.legs[1].fee == 500));
        if self.flash_asset != weth
            || self.token_path.as_slice() != [weth, usdc, weth]
            || self.legs.len() != 2
            || self.legs[0].token_in != weth
            || self.legs[0].token_out != usdc
            || !self.legs[0].zero_for_one
            || self.legs[1].token_in != usdc
            || self.legs[1].token_out != weth
            || self.legs[1].zero_for_one
            || !route_matches
        {
            return Err(ModelError::InvalidLegs);
        }
        Ok(())
    }

    pub fn canonical_approval_digest(&self) -> Result<String, ModelError> {
        let body = ApprovalBody {
            schema_version: &self.schema_version,
            request_id: self.id.to_string(),
            opportunity_id: self.opportunity_id.to_string(),
            chain_id: self.chain_id,
            route_id: format!("0x{}", hex::encode(self.route_id)),
            route_fingerprint: &self.route_fingerprint,
            route_type: self.route_type.as_str(),
            route_payload: self
                .aave_liquidation
                .as_ref()
                .map(|route| RawAaveLiquidationRoute {
                    borrower: route.borrower.to_string(),
                    debt_asset: route.debt_asset.to_string(),
                    collateral_asset: route.collateral_asset.to_string(),
                    debt_asset_decimals: route.debt_asset_decimals,
                    debt_asset_price_base: route.debt_asset_price_base.to_string(),
                    weth_price_base: route.weth_price_base.to_string(),
                    maximum_input_weth_wei: route.maximum_input_weth_wei.to_string(),
                    minimum_profit_weth_wei: route.minimum_profit_weth_wei.to_string(),
                    receive_a_token: route.receive_a_token,
                    minimum_collateral_received: route.minimum_collateral_received.to_string(),
                    minimum_unwind_output: route.minimum_unwind_output.to_string(),
                    maximum_atlas_bid: route.maximum_atlas_bid.to_string(),
                    evidence_mode: route.evidence_mode.clone(),
                    state_root: route.state_root.clone(),
                    release_sha: route.release_sha.clone(),
                }),
            selected_size: self.selected_size.to_string(),
            token_path: self.token_path.iter().map(ToString::to_string).collect(),
            origin_router: self.origin_router.to_string(),
            executor_address: self.executor_address.to_string(),
            executor_code_hash: &self.executor_code_hash,
            calldata_hash: &self.calldata_hash,
            simulation_result_hash: &self.simulation_result_hash,
            plan_hash: &self.plan_hash,
            pinned_block_number: self.pinned_block_number,
            pinned_block_hash: &self.pinned_block_hash,
            flash_asset: self.flash_asset.to_string(),
            flash_amount: self.flash_amount.to_string(),
            maximum_input_amount: self.maximum_input_amount.to_string(),
            minimum_profit: self.minimum_profit.to_string(),
            expected_profit: self.expected_profit.to_string(),
            deadline_unix_seconds: self.deadline.timestamp(),
            legs: &self.legs,
            gas_limit: self.gas_limit,
            max_fee_per_gas: self.max_fee_per_gas.to_string(),
            max_priority_fee_per_gas: self.max_priority_fee_per_gas.to_string(),
            approved_by: &self.approved_by,
            approved_at: self
                .approved_at
                .to_rfc3339_opts(chrono::SecondsFormat::Secs, true),
            approval_deadline: self
                .approval_deadline
                .to_rfc3339_opts(chrono::SecondsFormat::Secs, true),
            policy_version: &self.policy_version,
        };
        let encoded = serde_json::to_vec(&body).map_err(|_| ModelError::InvalidApproval)?;
        Ok(hex::encode(Sha256::digest(encoded)))
    }
}

fn validate_aave_route(
    route: &AaveLiquidationRoute,
    legs: &[ValidatedLeg],
    token_path: &[CanonicalAddress],
    flash_asset: CanonicalAddress,
) -> Result<(), ModelError> {
    if route.receive_a_token || route.debt_asset != flash_asset {
        return Err(ModelError::InvalidLegs);
    }
    let weth = CanonicalAddress::parse(ARBITRUM_WETH_ADDRESS)?;
    let usdc_e = CanonicalAddress::parse(ARBITRUM_USDC_E_ADDRESS)?;
    let unit_valid = (route.debt_asset == weth
        && route.debt_asset_decimals == 18
        && route.debt_asset_price_base == route.weth_price_base)
        || (route.debt_asset == usdc_e && route.debt_asset_decimals == 6);
    if !unit_valid || route.maximum_input_weth_wei == 0 || route.minimum_profit_weth_wei == 0 {
        return Err(ModelError::InvalidAmount);
    }
    if route.debt_asset == usdc_e {
        let reviewed_pool = CanonicalAddress::parse(REVIEWED_WETH_USDC_E_POOL_500_ADDRESS)?;
        let reviewed_factory =
            CanonicalAddress::parse("0x1f98431c8ad98523631ae4a59f267346ea31f984")?;
        let [leg] = legs else {
            return Err(ModelError::InvalidLegs);
        };
        if route.collateral_asset != weth
            || token_path != [weth, usdc_e]
            || leg.pool != reviewed_pool
            || leg.factory != Some(reviewed_factory)
            || leg.token_in != weth
            || leg.token_out != usdc_e
            || leg.fee != 500
            || !leg.zero_for_one
            || leg.min_amount_out != route.minimum_unwind_output
        {
            return Err(ModelError::InvalidLegs);
        }
        return Ok(());
    }
    if route.collateral_asset == route.debt_asset {
        if !legs.is_empty() || token_path != [route.debt_asset] {
            return Err(ModelError::InvalidLegs);
        }
        return Ok(());
    }
    if legs.is_empty()
        || token_path.len() != legs.len() + 1
        || token_path.first() != Some(&route.collateral_asset)
        || token_path.last() != Some(&route.debt_asset)
    {
        return Err(ModelError::InvalidLegs);
    }
    for (index, leg) in legs.iter().enumerate() {
        if leg.factory.is_none()
            || leg.token_in != token_path[index]
            || leg.token_out != token_path[index + 1]
            || leg.token_in == leg.token_out
            || leg.zero_for_one != (leg.token_in.as_bytes() < leg.token_out.as_bytes())
        {
            return Err(ModelError::InvalidLegs);
        }
    }
    Ok(())
}

impl AaveLiquidationRoute {
    fn debt_unit(&self) -> Result<u128, ModelError> {
        10_u128
            .checked_pow(u32::from(self.debt_asset_decimals))
            .ok_or(ModelError::InvalidAmount)
    }

    pub fn weth_to_debt_floor(&self, weth_wei: u128) -> Result<u128, ModelError> {
        if self.debt_asset_decimals == 18 && self.debt_asset_price_base == self.weth_price_base {
            return Ok(weth_wei);
        }
        let numerator = weth_wei
            .checked_mul(self.weth_price_base)
            .and_then(|value| value.checked_mul(self.debt_unit().ok()?))
            .ok_or(ModelError::InvalidAmount)?;
        let denominator = 1_000_000_000_000_000_000_u128
            .checked_mul(self.debt_asset_price_base)
            .ok_or(ModelError::InvalidAmount)?;
        Ok(numerator / denominator)
    }

    pub fn weth_to_debt_ceil(&self, weth_wei: u128) -> Result<u128, ModelError> {
        if self.debt_asset_decimals == 18 && self.debt_asset_price_base == self.weth_price_base {
            return Ok(weth_wei);
        }
        let numerator = weth_wei
            .checked_mul(self.weth_price_base)
            .and_then(|value| value.checked_mul(self.debt_unit().ok()?))
            .ok_or(ModelError::InvalidAmount)?;
        let denominator = 1_000_000_000_000_000_000_u128
            .checked_mul(self.debt_asset_price_base)
            .ok_or(ModelError::InvalidAmount)?;
        numerator
            .checked_add(denominator - 1)
            .map(|value| value / denominator)
            .ok_or(ModelError::InvalidAmount)
    }

    pub fn debt_to_weth_floor(&self, debt_raw: u128) -> Result<u128, ModelError> {
        if self.debt_asset_decimals == 18 && self.debt_asset_price_base == self.weth_price_base {
            return Ok(debt_raw);
        }
        let numerator = debt_raw
            .checked_mul(self.debt_asset_price_base)
            .and_then(|value| value.checked_mul(1_000_000_000_000_000_000_u128))
            .ok_or(ModelError::InvalidAmount)?;
        let denominator = self
            .debt_unit()?
            .checked_mul(self.weth_price_base)
            .ok_or(ModelError::InvalidAmount)?;
        Ok(numerator / denominator)
    }
}

#[cfg(test)]
mod aave_unit_tests {
    use super::*;

    fn usdc_e_weth_route() -> (AaveLiquidationRoute, ValidatedLeg, Vec<CanonicalAddress>) {
        let weth = CanonicalAddress::parse(ARBITRUM_WETH_ADDRESS).unwrap();
        let usdc_e = CanonicalAddress::parse(ARBITRUM_USDC_E_ADDRESS).unwrap();
        let route = AaveLiquidationRoute {
            borrower: CanonicalAddress::parse("0x1111111111111111111111111111111111111111")
                .unwrap(),
            debt_asset: usdc_e,
            collateral_asset: weth,
            debt_asset_decimals: 6,
            debt_asset_price_base: 100_000_000,
            weth_price_base: 200_000_000_000,
            maximum_input_weth_wei: 10_000_000_000_000_000,
            minimum_profit_weth_wei: 1_000_000_000_001,
            receive_a_token: false,
            minimum_collateral_received: 1,
            minimum_unwind_output: 2_001,
            maximum_atlas_bid: 1,
            evidence_mode: "DUAL_PROVIDER_FORK_VERIFIED".to_string(),
            state_root: format!("0x{}", "1".repeat(64)),
            release_sha: "2".repeat(40),
        };
        let leg = ValidatedLeg {
            pool: CanonicalAddress::parse(REVIEWED_WETH_USDC_E_POOL_500_ADDRESS).unwrap(),
            factory: Some(
                CanonicalAddress::parse("0x1f98431c8ad98523631ae4a59f267346ea31f984").unwrap(),
            ),
            token_in: weth,
            token_out: usdc_e,
            fee: 500,
            zero_for_one: true,
            min_amount_out: route.minimum_unwind_output,
        };
        (route, leg, vec![weth, usdc_e])
    }

    #[test]
    fn usdc_e_route_binds_units_rounding_and_reviewed_pool() {
        let (route, leg, token_path) = usdc_e_weth_route();
        assert_eq!(
            route
                .weth_to_debt_floor(route.maximum_input_weth_wei)
                .unwrap(),
            20_000_000
        );
        assert_eq!(
            route
                .weth_to_debt_ceil(route.minimum_profit_weth_wei)
                .unwrap(),
            2_001
        );
        assert_eq!(route.debt_to_weth_floor(3_900).unwrap(), 1_950_000_000_000);
        validate_aave_route(
            &route,
            std::slice::from_ref(&leg),
            &token_path,
            route.debt_asset,
        )
        .unwrap();

        let mut wrong_pool = leg;
        wrong_pool.pool = CanonicalAddress::parse(CURRENT_ROUTE_POOL_500_ADDRESS).unwrap();
        assert_eq!(
            validate_aave_route(
                &route,
                std::slice::from_ref(&wrong_pool),
                &token_path,
                route.debt_asset,
            ),
            Err(ModelError::InvalidLegs)
        );
    }
}

impl TryFrom<ExecutionLeg> for ValidatedLeg {
    type Error = ModelError;

    fn try_from(value: ExecutionLeg) -> Result<Self, Self::Error> {
        if value.fee == 0 || value.fee >= 1_000_000 {
            return Err(ModelError::InvalidLegs);
        }
        Ok(Self {
            pool: CanonicalAddress::parse(&value.pool)?,
            factory: value
                .factory
                .map(|address| CanonicalAddress::parse(&address))
                .transpose()?,
            token_in: CanonicalAddress::parse(&value.token_in)?,
            token_out: CanonicalAddress::parse(&value.token_out)?,
            fee: value.fee,
            zero_for_one: value.zero_for_one,
            min_amount_out: parse_positive_u128(&value.min_amount_out)?,
        })
    }
}

fn validate_route(legs: &[ValidatedLeg], flash_asset: CanonicalAddress) -> Result<(), ModelError> {
    let mut expected_input = flash_asset;
    for leg in legs {
        if leg.token_in != expected_input
            || leg.token_in == leg.token_out
            || leg.zero_for_one != (leg.token_in.as_bytes() < leg.token_out.as_bytes())
        {
            return Err(ModelError::InvalidLegs);
        }
        expected_input = leg.token_out;
    }
    if expected_input != flash_asset {
        return Err(ModelError::InvalidLegs);
    }
    Ok(())
}

fn validate_token_path(
    token_path: &[CanonicalAddress],
    legs: &[ValidatedLeg],
    flash_asset: CanonicalAddress,
) -> Result<(), ModelError> {
    if token_path.len() != legs.len() + 1
        || token_path.first() != Some(&flash_asset)
        || token_path.last() != Some(&flash_asset)
        || legs.iter().enumerate().any(|(index, leg)| {
            token_path[index] != leg.token_in || token_path[index + 1] != leg.token_out
        })
    {
        return Err(ModelError::InvalidLegs);
    }
    Ok(())
}

fn parse_positive_u128(value: &str) -> Result<u128, ModelError> {
    value
        .parse::<u128>()
        .ok()
        .filter(|parsed| *parsed > 0)
        .ok_or(ModelError::InvalidAmount)
}

fn parse_fixed_hex<const N: usize>(value: &str) -> Option<[u8; N]> {
    if value.len() != 2 + N * 2 || !value.starts_with("0x") {
        return None;
    }
    let decoded = hex::decode(&value[2..]).ok()?;
    decoded.try_into().ok()
}

pub fn canonical_digest(value: &str) -> bool {
    value.len() == 64
        && value
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
}

pub fn canonical_block_hash(value: &str) -> bool {
    value.len() == 66
        && value.starts_with("0x")
        && value[2..]
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AttemptStatus {
    Claimed,
    NonceAllocated,
    SubmissionUnknown,
    Pending,
    Confirmed,
    Reverted,
    Replaced,
    TimedOut,
    Failed,
}

impl AttemptStatus {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Claimed => "claimed",
            Self::NonceAllocated => "nonce_allocated",
            Self::SubmissionUnknown => "submission_unknown",
            Self::Pending => "pending",
            Self::Confirmed => "confirmed",
            Self::Reverted => "reverted",
            Self::Replaced => "replaced",
            Self::TimedOut => "timed_out",
            Self::Failed => "failed",
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ActiveAttempt {
    pub request: ExecutionRequest,
    pub status: AttemptStatus,
    pub nonce: Option<u64>,
    pub tx_hash: Option<TransactionHash>,
    pub submitted_at: Option<DateTime<Utc>>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Settlement {
    pub asset: CanonicalAddress,
    pub flash_amount: u128,
    pub premium: u128,
    pub realized_profit: u128,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ReceiptOutcome {
    pub receipt_status: u64,
    pub settled_event_found: bool,
    pub block_number: u64,
    pub gas_used: u64,
    pub effective_gas_price: u128,
    pub actual_fee_wei: u128,
    pub actual_l1_cost_wei: u128,
    pub settlement: Settlement,
    pub canonical_realized_profit_wei: u128,
    pub net_pnl_wei: i128,
}

#[derive(Clone, Debug, Error, PartialEq, Eq)]
pub enum ModelError {
    #[error("invalid canonical address")]
    InvalidAddress,
    #[error("invalid transaction hash")]
    InvalidTransactionHash,
    #[error("unsupported request schema")]
    SchemaVersion,
    #[error("unsupported chain")]
    WrongChain,
    #[error("invalid route identity")]
    InvalidRoute,
    #[error("invalid amount")]
    InvalidAmount,
    #[error("invalid gas policy")]
    InvalidGas,
    #[error("invalid route legs")]
    InvalidLegs,
    #[error("invalid approval")]
    InvalidApproval,
    #[error("approval digest mismatch")]
    ApprovalDigestMismatch,
}
