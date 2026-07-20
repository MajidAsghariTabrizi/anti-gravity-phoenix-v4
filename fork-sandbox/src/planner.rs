use crate::abi::executor_contract;
use crate::model::{
    PersistedOpportunity, PinnedBlockEvidence, PredictedEconomics, RoutePlan,
    UnsignedTransactionPlan, VerificationEvidence, ARBITRUM_ONE_CHAIN_ID,
    FORK_EVIDENCE_SCHEMA_VERSION, PLAN_SCHEMA_VERSION,
};
use ethabi::{Address, Token};
use primitive_types::U256;
use rpc_gateway::shadow_state::{
    canonical_block_hash, canonical_digest, EvidenceRequest, PoolStateRequest, ShadowStateRequest,
    SHADOW_STATE_SCHEMA_VERSION,
};
use sha2::{Digest, Sha256};
use std::collections::{BTreeSet, HashSet};
use thiserror::Error;

const MAX_ROUTE_LEGS: usize = 4;
const MAX_CALLDATA_BYTES: usize = 128 * 1024;
const ARBITRUM_WETH: &str = "0x82af49447d8a07e3bd95bd0d56f35241523fbab1";
const ARBITRUM_NATIVE_USDC: &str = "0xaf88d065e77c8cc2239327c5edb3a432268e5831";

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PlanPolicy {
    pub allowed_tokens: BTreeSet<String>,
    pub allowed_pools: BTreeSet<String>,
    pub allowed_routers: BTreeSet<String>,
    pub allowed_protocols: BTreeSet<String>,
    pub target_contract: String,
    pub target_code_hash: String,
    pub simulation_from: String,
    pub minimum_net_pnl: u128,
    pub maximum_input_amount: u128,
    pub slippage_bps: u16,
    pub maximum_calldata_bytes: usize,
}

#[derive(Clone, Debug, Error, PartialEq, Eq)]
pub enum PlannerError {
    #[error("fork plan chain is unsupported")]
    WrongChain,
    #[error("fork plan token is not allowlisted")]
    UnsupportedToken,
    #[error("fork plan pool is not allowlisted")]
    UnsupportedPool,
    #[error("fork plan router is not allowlisted")]
    UnsupportedRouter,
    #[error("fork plan protocol is not allowlisted")]
    UnsupportedProtocol,
    #[error("fork plan opportunity is stale")]
    StaleOpportunity,
    #[error("fork plan independent verification is missing")]
    MissingVerification,
    #[error("fork plan providers disagree")]
    ProviderDisagreement,
    #[error("fork plan state evidence is missing")]
    MissingStateHash,
    #[error("fork plan route identity does not match persisted evidence")]
    RouteHashMismatch,
    #[error("fork plan expected net PnL is not positive")]
    NonPositiveExpectedNetPnl,
    #[error("fork plan expected net PnL is below policy")]
    BelowThreshold,
    #[error("fork plan calldata exceeds the bounded limit")]
    OversizedCalldata,
    #[error("fork plan input exceeds the bounded limit")]
    InputLimit,
    #[error("fork plan persisted evidence is invalid")]
    InvalidEvidence,
}

#[derive(Clone, Debug, Default)]
pub struct UnsignedPlanner;

impl UnsignedPlanner {
    pub fn build(
        &self,
        fact: &PersistedOpportunity,
        policy: &PlanPolicy,
        now_unix_ms: u64,
    ) -> Result<UnsignedTransactionPlan, PlannerError> {
        validate_policy(policy)?;
        validate_fact(fact, policy, now_unix_ms)?;
        let pools = pool_requests_from_fact(fact)?;
        let request = ShadowStateRequest {
            schema_version: SHADOW_STATE_SCHEMA_VERSION.to_string(),
            chain_id: fact.chain_id,
            route_fingerprint: fact.route_fingerprint.clone(),
            pools,
            evidence: EvidenceRequest::Primary,
        };
        let reconstructed_hash = request
            .route_config_hash()
            .map_err(|_| PlannerError::InvalidEvidence)?;
        if reconstructed_hash != fact.route_config_hash {
            return Err(PlannerError::RouteHashMismatch);
        }

        let input_amount = parse_u128(&fact.input_amount)?;
        let expected_output = parse_u128(&fact.expected_output)?;
        let gross_profit = parse_i128(&fact.gross_profit)?;
        let total_cost = parse_u128(&fact.total_cost)?;
        let expected_net_pnl = parse_i128(&fact.expected_net_pnl)?;
        let required_net_pnl = parse_i128(&fact.minimum_required_net_pnl)?;
        let gas_estimate = parse_u64(&fact.execution_gas)?;
        let gas_price = parse_u128(&fact.gas_price)?;
        let minimum_net = policy
            .minimum_net_pnl
            .max(u128::try_from(required_net_pnl).map_err(|_| PlannerError::InvalidEvidence)?);
        let minimum_profit = gas_price
            .checked_mul(gas_estimate as u128)
            .and_then(|gas_cost| gas_cost.checked_add(minimum_net))
            .ok_or(PlannerError::InvalidEvidence)?;
        let expected_leg_outputs = fact
            .expected_leg_outputs
            .iter()
            .map(|value| parse_u128(value))
            .collect::<Result<Vec<_>, _>>()?;
        let minimum_leg_outputs = expected_leg_outputs
            .iter()
            .map(|value| slippage_floor(*value, policy.slippage_bps))
            .collect::<Result<Vec<_>, _>>()?;
        let deadline = u64::try_from(fact.opportunity_expires_at.timestamp())
            .map_err(|_| PlannerError::InvalidEvidence)?;
        let calldata =
            encode_calldata(fact, policy, &minimum_leg_outputs, minimum_profit, deadline)?;
        if calldata.len() > policy.maximum_calldata_bytes {
            return Err(PlannerError::OversizedCalldata);
        }
        let calldata_hash = hex::encode(Sha256::digest(&calldata));
        let calldata = format!("0x{}", hex::encode(calldata));
        let secondary_provider_id = fact
            .secondary_provider_id
            .clone()
            .ok_or(PlannerError::MissingVerification)?;

        Ok(UnsignedTransactionPlan {
            schema_version: PLAN_SCHEMA_VERSION.to_string(),
            shadow_decision_id: fact.shadow_decision_id.clone(),
            source_event_identity: fact.source_event_identity.clone(),
            chain_id: fact.chain_id,
            route: RoutePlan {
                route_id: fact.route_id.clone(),
                route_fingerprint: fact.route_fingerprint.clone(),
                pool_ids: fact.pool_path.clone(),
                pool_addresses: fact.pool_address_path.clone(),
                protocols: fact.protocol_path.clone(),
                directions: fact.direction_path.clone(),
                fees: fact.fee_path.clone(),
            },
            token_path: fact.token_path.clone(),
            origin_router: fact.origin_router.clone(),
            input_amount: input_amount.to_string(),
            maximum_input_amount: policy.maximum_input_amount.to_string(),
            expected_output: expected_output.to_string(),
            expected_leg_outputs: expected_leg_outputs.iter().map(u128::to_string).collect(),
            minimum_output: minimum_leg_outputs
                .last()
                .ok_or(PlannerError::InvalidEvidence)?
                .to_string(),
            minimum_leg_outputs: minimum_leg_outputs.iter().map(u128::to_string).collect(),
            minimum_profit: minimum_profit.to_string(),
            calldata,
            calldata_hash,
            value: "0".to_string(),
            gas_estimate,
            gas_price_wei: gas_price.to_string(),
            deadline,
            target_contract: policy.target_contract.clone(),
            target_code_hash: policy.target_code_hash.clone(),
            simulation_from: policy.simulation_from.clone(),
            pinned_block: PinnedBlockEvidence {
                number: fact.pinned_block_number,
                hash: fact.pinned_block_hash.clone(),
            },
            route_hash: reconstructed_hash,
            primary_state_hash: fact.primary_state_hash.clone(),
            pool_state_hash_path: fact.pool_state_hash_path.clone(),
            verification: VerificationEvidence {
                verification_status: fact.verification_status.clone(),
                independent_verification_status: fact.independent_verification_status.clone(),
                agreement_state: fact.agreement_state.clone(),
                primary_provider_id: fact.primary_provider_id.clone(),
                secondary_provider_id,
            },
            predicted: PredictedEconomics {
                gross_profit: gross_profit.to_string(),
                total_cost: total_cost.to_string(),
                net_pnl: expected_net_pnl.to_string(),
                minimum_required_net_pnl: minimum_net.to_string(),
            },
            model_version: fact.model_version.clone(),
            policy_version: fact.policy_version.clone(),
            unsigned: true,
            fork_only: true,
            shadow_only: true,
            live_execution: false,
            execution_eligible: false,
            execution_request_created: false,
            public_broadcast: false,
            signer_used: false,
        })
    }
}

pub(crate) fn pool_requests_from_plan(
    plan: &UnsignedTransactionPlan,
) -> Result<Vec<PoolStateRequest>, PlannerError> {
    pool_requests(
        &plan.route.route_fingerprint,
        &plan.token_path,
        &plan.route.pool_ids,
        &plan.route.pool_addresses,
        &plan.route.protocols,
        &plan.route.directions,
        &plan.route.fees,
    )
}

fn pool_requests_from_fact(
    fact: &PersistedOpportunity,
) -> Result<Vec<PoolStateRequest>, PlannerError> {
    pool_requests(
        &fact.route_fingerprint,
        &fact.token_path,
        &fact.pool_path,
        &fact.pool_address_path,
        &fact.protocol_path,
        &fact.direction_path,
        &fact.fee_path,
    )
}

fn pool_requests(
    route_fingerprint: &str,
    token_path: &[String],
    pool_ids: &[String],
    pool_addresses: &[String],
    protocols: &[String],
    directions: &[String],
    fees: &[u32],
) -> Result<Vec<PoolStateRequest>, PlannerError> {
    if route_fingerprint.is_empty()
        || token_path.len() != pool_ids.len() + 1
        || [
            pool_addresses.len(),
            protocols.len(),
            directions.len(),
            fees.len(),
        ]
        .iter()
        .any(|length| *length != pool_ids.len())
    {
        return Err(PlannerError::InvalidEvidence);
    }
    (0..pool_ids.len())
        .map(|index| {
            let token_in = token_path[index].clone();
            let token_out = token_path[index + 1].clone();
            let (token0, token1) = match directions[index].as_str() {
                "zero_for_one" => (token_in, token_out),
                "one_for_zero" => (token_out, token_in),
                _ => return Err(PlannerError::InvalidEvidence),
            };
            if token0 != ARBITRUM_WETH || token1 != ARBITRUM_NATIVE_USDC {
                return Err(PlannerError::InvalidEvidence);
            }
            let tick_spacing = match fees[index] {
                500 => 10,
                3_000 => 60,
                _ => return Err(PlannerError::InvalidEvidence),
            };
            Ok(PoolStateRequest {
                pool_id: pool_ids[index].clone(),
                address: pool_addresses[index].clone(),
                protocol: protocols[index].clone(),
                token0,
                token1,
                token0_decimals: 18,
                token1_decimals: 6,
                fee: fees[index],
                tick_spacing,
            })
        })
        .collect()
}

fn validate_policy(policy: &PlanPolicy) -> Result<(), PlannerError> {
    if !canonical_address(&policy.target_contract)
        || !canonical_digest(&policy.target_code_hash)
        || !canonical_address(&policy.simulation_from)
        || policy.allowed_tokens.is_empty()
        || policy.allowed_pools.is_empty()
        || policy.allowed_routers.is_empty()
        || policy.allowed_protocols.is_empty()
        || policy.minimum_net_pnl == 0
        || policy.maximum_input_amount == 0
        || policy.slippage_bps == 0
        || policy.slippage_bps > 1_000
        || policy.maximum_calldata_bytes == 0
        || policy.maximum_calldata_bytes > MAX_CALLDATA_BYTES
    {
        return Err(PlannerError::InvalidEvidence);
    }
    if policy
        .allowed_tokens
        .iter()
        .chain(&policy.allowed_pools)
        .chain(&policy.allowed_routers)
        .any(|value| !canonical_address(value))
    {
        return Err(PlannerError::InvalidEvidence);
    }
    Ok(())
}

fn validate_fact(
    fact: &PersistedOpportunity,
    policy: &PlanPolicy,
    now_unix_ms: u64,
) -> Result<(), PlannerError> {
    if fact.chain_id != ARBITRUM_ONE_CHAIN_ID {
        return Err(PlannerError::WrongChain);
    }
    if fact.fork_evidence_schema_version != FORK_EVIDENCE_SCHEMA_VERSION
        || fact.disposition != "rejected"
        || fact.primary_rejection_reason.as_deref() != Some("contract_path_unavailable")
        || fact.primary_profitability_status != "meets_minimum"
        || fact.evidence_completeness_status != "complete"
        || !fact.shadow_only
        || fact.execution_eligible
        || fact.execution_request_created
    {
        return Err(PlannerError::InvalidEvidence);
    }
    if fact.verification_status == "disagreed"
        || fact.independent_verification_status == "disagreed"
        || fact.agreement_state == "disagreed"
    {
        return Err(PlannerError::ProviderDisagreement);
    }
    if fact.verification_status != "agreed"
        || fact.independent_verification_status != "agreed"
        || fact.agreement_state != "agreed"
    {
        return Err(PlannerError::MissingVerification);
    }
    let secondary_provider = fact
        .secondary_provider_id
        .as_deref()
        .ok_or(PlannerError::MissingVerification)?;
    if secondary_provider == fact.primary_provider_id
        || fact.secondary_state_hash.as_deref() != Some(fact.primary_state_hash.as_str())
        || fact.secondary_block_number != Some(fact.pinned_block_number)
        || fact.secondary_block_hash.as_deref() != Some(fact.pinned_block_hash.as_str())
        || fact.secondary_route_config_hash.as_deref() != Some(fact.route_config_hash.as_str())
    {
        return Err(PlannerError::ProviderDisagreement);
    }
    let now = i64::try_from(now_unix_ms).map_err(|_| PlannerError::InvalidEvidence)?;
    if fact.opportunity_expires_at.timestamp_millis() <= now
        || fact.opportunity_expires_at <= fact.detected_at
    {
        return Err(PlannerError::StaleOpportunity);
    }
    if fact.pool_path.is_empty()
        || fact.pool_path.len() > MAX_ROUTE_LEGS
        || fact.token_path.len() != fact.pool_path.len() + 1
        || fact.token_path.first() != fact.token_path.last()
    {
        return Err(PlannerError::InvalidEvidence);
    }
    if fact
        .token_path
        .iter()
        .any(|token| !canonical_address(token) || !policy.allowed_tokens.contains(token))
    {
        return Err(PlannerError::UnsupportedToken);
    }
    if fact
        .pool_address_path
        .iter()
        .any(|pool| !canonical_address(pool) || !policy.allowed_pools.contains(pool))
    {
        return Err(PlannerError::UnsupportedPool);
    }
    if !canonical_address(&fact.origin_router)
        || !policy.allowed_routers.contains(&fact.origin_router)
    {
        return Err(PlannerError::UnsupportedRouter);
    }
    if fact
        .protocol_path
        .iter()
        .any(|protocol| !policy.allowed_protocols.contains(protocol))
    {
        return Err(PlannerError::UnsupportedProtocol);
    }
    let unique_pools = fact.pool_address_path.iter().collect::<HashSet<_>>();
    if unique_pools.len() != fact.pool_address_path.len() {
        return Err(PlannerError::InvalidEvidence);
    }
    if !canonical_block_hash(&fact.pinned_block_hash)
        || !canonical_digest(&fact.primary_state_hash)
        || !canonical_digest(&fact.route_config_hash)
        || fact.pool_state_hash_path.len() != fact.pool_path.len()
        || fact
            .pool_state_hash_path
            .iter()
            .any(|hash| !canonical_digest(hash))
    {
        return Err(PlannerError::MissingStateHash);
    }
    let input_amount = parse_u128(&fact.input_amount)?;
    let expected_output = parse_u128(&fact.expected_output)?;
    let gross_profit = parse_i128(&fact.gross_profit)?;
    let expected_net = parse_i128(&fact.expected_net_pnl)?;
    let minimum_required = parse_i128(&fact.minimum_required_net_pnl)?;
    if input_amount == 0
        || input_amount > policy.maximum_input_amount
        || input_amount > i128::MAX as u128
    {
        return Err(PlannerError::InputLimit);
    }
    if expected_output <= input_amount || gross_profit <= 0 || expected_net <= 0 {
        return Err(PlannerError::NonPositiveExpectedNetPnl);
    }
    let expected_net = u128::try_from(expected_net).map_err(|_| PlannerError::InvalidEvidence)?;
    let minimum_required =
        u128::try_from(minimum_required).map_err(|_| PlannerError::InvalidEvidence)?;
    if expected_net < minimum_required || expected_net < policy.minimum_net_pnl {
        return Err(PlannerError::BelowThreshold);
    }
    if fact.expected_leg_outputs.len() != fact.pool_path.len()
        || fact
            .expected_leg_outputs
            .iter()
            .map(|value| parse_u128(value))
            .collect::<Result<Vec<_>, _>>()?
            .last()
            .copied()
            != Some(expected_output)
    {
        return Err(PlannerError::InvalidEvidence);
    }
    if parse_u64(&fact.execution_gas)? == 0 || parse_u128(&fact.gas_price)? == 0 {
        return Err(PlannerError::InvalidEvidence);
    }
    Ok(())
}

fn encode_calldata(
    fact: &PersistedOpportunity,
    policy: &PlanPolicy,
    minimum_leg_outputs: &[u128],
    minimum_profit: u128,
    deadline: u64,
) -> Result<Vec<u8>, PlannerError> {
    let route_hash =
        hex::decode(&fact.route_config_hash).map_err(|_| PlannerError::InvalidEvidence)?;
    if route_hash.len() != 32 {
        return Err(PlannerError::InvalidEvidence);
    }
    let legs = fact
        .pool_address_path
        .iter()
        .enumerate()
        .map(|(index, pool)| {
            Ok(Token::Tuple(vec![
                Token::Address(parse_address(pool)?),
                Token::Address(parse_address(&fact.token_path[index])?),
                Token::Address(parse_address(&fact.token_path[index + 1])?),
                Token::Uint(U256::from(fact.fee_path[index])),
                Token::Bool(match fact.direction_path[index].as_str() {
                    "zero_for_one" => true,
                    "one_for_zero" => false,
                    _ => return Err(PlannerError::InvalidEvidence),
                }),
                Token::Uint(U256::from(minimum_leg_outputs[index])),
            ]))
        })
        .collect::<Result<Vec<_>, PlannerError>>()?;
    let opportunity = Token::Tuple(vec![
        Token::FixedBytes(route_hash),
        Token::Address(parse_address(&fact.origin_router)?),
        Token::Address(parse_address(&policy.target_contract)?),
        Token::Address(parse_address(&fact.token_path[0])?),
        Token::Uint(
            U256::from_dec_str(&fact.input_amount).map_err(|_| PlannerError::InvalidEvidence)?,
        ),
        Token::Uint(U256::from(policy.maximum_input_amount)),
        Token::Uint(U256::from(minimum_profit)),
        Token::Uint(U256::from(deadline)),
        Token::Array(legs),
    ]);
    executor_contract()
        .and_then(|contract| contract.function("executeOpportunity").cloned())
        .and_then(|function| function.encode_input(&[opportunity]))
        .map_err(|_| PlannerError::InvalidEvidence)
}

fn parse_address(value: &str) -> Result<Address, PlannerError> {
    if !canonical_address(value) {
        return Err(PlannerError::InvalidEvidence);
    }
    let decoded = hex::decode(&value[2..]).map_err(|_| PlannerError::InvalidEvidence)?;
    Ok(Address::from_slice(&decoded))
}

fn canonical_address(value: &str) -> bool {
    value.len() == 42
        && value.starts_with("0x")
        && value[2..]
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
}

fn parse_u128(value: &str) -> Result<u128, PlannerError> {
    value
        .parse::<u128>()
        .map_err(|_| PlannerError::InvalidEvidence)
}

fn parse_i128(value: &str) -> Result<i128, PlannerError> {
    value
        .parse::<i128>()
        .map_err(|_| PlannerError::InvalidEvidence)
}

fn parse_u64(value: &str) -> Result<u64, PlannerError> {
    value
        .parse::<u64>()
        .map_err(|_| PlannerError::InvalidEvidence)
}

fn slippage_floor(value: u128, slippage_bps: u16) -> Result<u128, PlannerError> {
    let retained = 10_000u128
        .checked_sub(slippage_bps as u128)
        .ok_or(PlannerError::InvalidEvidence)?;
    let floor = value
        .checked_mul(retained)
        .map(|scaled| scaled / 10_000)
        .ok_or(PlannerError::InvalidEvidence)?;
    if floor == 0 {
        Err(PlannerError::InvalidEvidence)
    } else {
        Ok(floor)
    }
}
