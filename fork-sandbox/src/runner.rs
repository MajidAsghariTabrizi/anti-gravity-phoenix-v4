use crate::abi::executor_contract;
use crate::model::{
    CounterfactualResult, CounterfactualResultBody, ForkIdentity, PinnedBlockEvidence,
    SimulationEvidence, SimulationStatus, UnsignedTransactionPlan, ARBITRUM_ONE_CHAIN_ID,
    PLAN_SCHEMA_VERSION, RESULT_SCHEMA_VERSION,
};
use crate::planner::{pool_requests_from_plan, PlannerError};
use crate::rpc::{ForkRpc, RpcError, SimulationCall, TraceObservation, ALLOWED_RPC_METHODS};
use chrono::{DateTime, Utc};
use ethabi::{ethereum_types::H256, ParamType, RawLog, Token};
use rpc_gateway::shadow_state::{canonical_hash_bytes, PoolStateResponse};
use serde_json::to_vec;
use sha2::{Digest, Sha256};
use thiserror::Error;

const MAX_SIMULATION_GAS: u64 = 30_000_000;

#[derive(Clone, Debug, Error, PartialEq, Eq)]
pub enum RunnerError {
    #[error("fork runner plan boundary failed closed")]
    PlanBoundary,
    #[error("fork runner is not attached to the pinned Arbitrum fork")]
    ForkIdentity,
    #[error("fork runner observed state drift")]
    StateDrift,
    #[error("fork runner target contract is unavailable")]
    ContractUnavailable,
    #[error("fork runner target bytecode differs from reviewed policy")]
    ContractMismatch,
    #[error("fork runner response failed integrity validation")]
    Integrity,
    #[error(transparent)]
    Rpc(#[from] RpcError),
    #[error(transparent)]
    Planner(#[from] PlannerError),
}

#[derive(Clone, Debug, Default)]
pub struct ForkRunner;

impl ForkRunner {
    pub async fn run<R: ForkRpc>(
        &self,
        plan: &UnsignedTransactionPlan,
        rpc: &R,
        simulated_at: DateTime<Utc>,
    ) -> Result<CounterfactualResult, RunnerError> {
        validate_plan_boundary(plan, simulated_at)?;
        let plan_hash = plan.canonical_hash().map_err(|_| RunnerError::Integrity)?;
        let metadata = rpc.metadata().await?;
        if metadata.chain_id != ARBITRUM_ONE_CHAIN_ID
            || metadata.fork_block_number != plan.pinned_block.number
            || metadata.fork_block_hash != plan.pinned_block.hash
        {
            return Err(RunnerError::ForkIdentity);
        }
        let local_block = rpc.latest_block().await?;
        if local_block.number < plan.pinned_block.number {
            return Err(RunnerError::ForkIdentity);
        }
        let code = rpc.code(&plan.target_contract).await?;
        if code == "0x" {
            return Err(RunnerError::ContractUnavailable);
        }
        let code_bytes = decode_data(&code, 512 * 1024)?;
        let target_code_hash = hex::encode(Sha256::digest(code_bytes));
        if target_code_hash != plan.target_code_hash {
            return Err(RunnerError::ContractMismatch);
        }

        let requests = pool_requests_from_plan(plan)?;
        let mut observed_pools = Vec::with_capacity(requests.len());
        let mut observed_hashes = Vec::with_capacity(requests.len());
        for request in requests {
            let observed = rpc.observe_pool(&request).await?;
            if observed.token0 != request.token0
                || observed.token1 != request.token1
                || observed.token0_decimals != request.token0_decimals
                || observed.token1_decimals != request.token1_decimals
                || observed.fee != request.fee
                || observed.tick_spacing != request.tick_spacing
            {
                return Err(RunnerError::StateDrift);
            }
            let state_material = to_vec(&(
                &request.pool_id,
                &request.address,
                &request.protocol,
                &request.token0,
                &request.token1,
                request.token0_decimals,
                request.token1_decimals,
                request.fee,
                request.tick_spacing,
                &observed.slot0,
                &observed.liquidity,
            ))
            .map_err(|_| RunnerError::Integrity)?;
            let state_hash = canonical_hash_bytes(&state_material);
            observed_hashes.push(state_hash.clone());
            observed_pools.push(PoolStateResponse {
                pool_id: request.pool_id,
                address: request.address,
                protocol: request.protocol,
                token0: request.token0,
                token1: request.token1,
                token0_decimals: request.token0_decimals,
                token1_decimals: request.token1_decimals,
                fee: request.fee,
                tick_spacing: request.tick_spacing,
                slot0: observed.slot0,
                liquidity: observed.liquidity,
                state_hash,
            });
        }
        let aggregate_state_hash =
            canonical_hash_bytes(&to_vec(&observed_pools).map_err(|_| RunnerError::Integrity)?);
        if observed_hashes != plan.pool_state_hash_path
            || aggregate_state_hash != plan.primary_state_hash
        {
            return Err(RunnerError::StateDrift);
        }

        let mut call = SimulationCall {
            from: plan.simulation_from.clone(),
            to: plan.target_contract.clone(),
            data: plan.calldata.clone(),
            value: plan.value.clone(),
            gas: plan.gas_estimate.saturating_mul(2).min(MAX_SIMULATION_GAS),
        };
        let estimate = match rpc.estimate_gas(&call).await {
            Ok(value) if value > 0 && value <= MAX_SIMULATION_GAS => value,
            Ok(_) => return Err(RunnerError::Integrity),
            Err(RpcError::Reverted { reason, data }) => {
                return reverted_result(
                    plan,
                    plan_hash,
                    metadata.instance_hash,
                    local_block,
                    simulated_at,
                    target_code_hash,
                    observed_hashes,
                    aggregate_state_hash,
                    None,
                    decode_revert(&reason, data.as_deref()),
                )
            }
            Err(error) => return Err(error.into()),
        };
        call.gas = estimate;
        let call_output = match rpc.call(&call).await {
            Ok(output) => output,
            Err(RpcError::Reverted { reason, data }) => {
                return reverted_result(
                    plan,
                    plan_hash,
                    metadata.instance_hash,
                    local_block,
                    simulated_at,
                    target_code_hash,
                    observed_hashes,
                    aggregate_state_hash,
                    Some(estimate),
                    decode_revert(&reason, data.as_deref()),
                )
            }
            Err(error) => return Err(error.into()),
        };
        let trace = match rpc.trace_call(&call).await {
            Ok(trace) => trace,
            Err(RpcError::Reverted { reason, data }) => {
                return reverted_result(
                    plan,
                    plan_hash,
                    metadata.instance_hash,
                    local_block,
                    simulated_at,
                    target_code_hash,
                    observed_hashes,
                    aggregate_state_hash,
                    Some(estimate),
                    decode_revert(&reason, data.as_deref()),
                )
            }
            Err(error) => return Err(error.into()),
        };
        if let Some(reason) = &trace.revert_reason {
            return reverted_result(
                plan,
                plan_hash,
                metadata.instance_hash,
                local_block,
                simulated_at,
                target_code_hash,
                observed_hashes,
                aggregate_state_hash,
                Some(estimate),
                decode_revert(reason, Some(&trace.output)),
            );
        }
        if trace.gas_used == 0 || trace.gas_used > estimate {
            return Err(RunnerError::Integrity);
        }
        let settlement = decode_settlement(plan, &trace)?;
        let gas_price = plan
            .gas_price_wei
            .parse::<u128>()
            .map_err(|_| RunnerError::Integrity)?;
        let gas_cost = gas_price
            .checked_mul(trace.gas_used as u128)
            .ok_or(RunnerError::Integrity)?;
        let balance_delta = settlement.realized_profit;
        let simulated_net = signed_difference(balance_delta, gas_cost)?;
        let predicted_net = plan
            .predicted
            .net_pnl
            .parse::<i128>()
            .map_err(|_| RunnerError::Integrity)?;
        let prediction_error = simulated_net
            .checked_sub(predicted_net)
            .ok_or(RunnerError::Integrity)?;
        let evidence = SimulationEvidence {
            rpc_methods: ALLOWED_RPC_METHODS
                .iter()
                .map(|method| (*method).to_string())
                .collect(),
            target_code_hash,
            observed_pool_state_hashes: observed_hashes,
            observed_aggregate_state_hash: aggregate_state_hash,
            call_output_hash: Some(hex::encode(Sha256::digest(call_output.as_bytes()))),
            trace_hash: Some(trace.trace_hash),
            settled_route_hash: Some(settlement.route_hash),
        };
        CounterfactualResult::from_body(CounterfactualResultBody {
            schema_version: RESULT_SCHEMA_VERSION.to_string(),
            plan_hash,
            shadow_decision_id: plan.shadow_decision_id.clone(),
            status: SimulationStatus::Passed,
            predicted_gross_profit: plan.predicted.gross_profit.clone(),
            predicted_total_cost: plan.predicted.total_cost.clone(),
            predicted_net_pnl: plan.predicted.net_pnl.clone(),
            simulated_gross_profit: Some(balance_delta.to_string()),
            simulated_gas_cost: Some(gas_cost.to_string()),
            simulated_balance_delta: Some(balance_delta.to_string()),
            simulated_net_pnl: Some(simulated_net.to_string()),
            prediction_error: Some(prediction_error.to_string()),
            gas_estimate: Some(estimate),
            gas_used: Some(trace.gas_used),
            model_version: plan.model_version.clone(),
            policy_version: plan.policy_version.clone(),
            fork: ForkIdentity {
                chain_id: metadata.chain_id,
                fork_block: plan.pinned_block.clone(),
                fork_instance_hash: metadata.instance_hash,
                local_block: PinnedBlockEvidence {
                    number: local_block.number,
                    hash: local_block.hash,
                },
            },
            simulated_at,
            revert_reason: None,
            evidence,
            fork_only: true,
            shadow_only: true,
            live_execution: false,
            execution_eligible: false,
            execution_request_created: false,
            public_broadcast: false,
            signer_used: false,
        })
        .map_err(|_| RunnerError::Integrity)
    }
}

#[allow(clippy::too_many_arguments)]
fn reverted_result(
    plan: &UnsignedTransactionPlan,
    plan_hash: String,
    instance_hash: String,
    local_block: crate::rpc::BlockObservation,
    simulated_at: DateTime<Utc>,
    target_code_hash: String,
    observed_hashes: Vec<String>,
    aggregate_state_hash: String,
    gas_estimate: Option<u64>,
    revert_reason: String,
) -> Result<CounterfactualResult, RunnerError> {
    CounterfactualResult::from_body(CounterfactualResultBody {
        schema_version: RESULT_SCHEMA_VERSION.to_string(),
        plan_hash,
        shadow_decision_id: plan.shadow_decision_id.clone(),
        status: SimulationStatus::Reverted,
        predicted_gross_profit: plan.predicted.gross_profit.clone(),
        predicted_total_cost: plan.predicted.total_cost.clone(),
        predicted_net_pnl: plan.predicted.net_pnl.clone(),
        simulated_gross_profit: None,
        simulated_gas_cost: None,
        simulated_balance_delta: None,
        simulated_net_pnl: None,
        prediction_error: None,
        gas_estimate,
        gas_used: None,
        model_version: plan.model_version.clone(),
        policy_version: plan.policy_version.clone(),
        fork: ForkIdentity {
            chain_id: plan.chain_id,
            fork_block: plan.pinned_block.clone(),
            fork_instance_hash: instance_hash,
            local_block: PinnedBlockEvidence {
                number: local_block.number,
                hash: local_block.hash,
            },
        },
        simulated_at,
        revert_reason: Some(revert_reason),
        evidence: SimulationEvidence {
            rpc_methods: ALLOWED_RPC_METHODS
                .iter()
                .map(|method| (*method).to_string())
                .collect(),
            target_code_hash,
            observed_pool_state_hashes: observed_hashes,
            observed_aggregate_state_hash: aggregate_state_hash,
            call_output_hash: None,
            trace_hash: None,
            settled_route_hash: None,
        },
        fork_only: true,
        shadow_only: true,
        live_execution: false,
        execution_eligible: false,
        execution_request_created: false,
        public_broadcast: false,
        signer_used: false,
    })
    .map_err(|_| RunnerError::Integrity)
}

fn validate_plan_boundary(
    plan: &UnsignedTransactionPlan,
    simulated_at: DateTime<Utc>,
) -> Result<(), RunnerError> {
    if plan.schema_version != PLAN_SCHEMA_VERSION
        || plan.chain_id != ARBITRUM_ONE_CHAIN_ID
        || !plan.unsigned
        || !plan.fork_only
        || !plan.shadow_only
        || plan.live_execution
        || plan.execution_eligible
        || plan.execution_request_created
        || plan.public_broadcast
        || plan.signer_used
        || plan.value != "0"
        || u64::try_from(simulated_at.timestamp()).map_err(|_| RunnerError::PlanBoundary)?
            >= plan.deadline
        || plan.calldata.len() < 10
        || !plan.calldata.starts_with("0x")
        || hex::encode(Sha256::digest(decode_data(&plan.calldata, 128 * 1024)?))
            != plan.calldata_hash
    {
        return Err(RunnerError::PlanBoundary);
    }
    Ok(())
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct Settlement {
    route_hash: String,
    realized_profit: u128,
}

fn decode_settlement(
    plan: &UnsignedTransactionPlan,
    trace: &TraceObservation,
) -> Result<Settlement, RunnerError> {
    let event = executor_contract()
        .and_then(|contract| contract.event("OpportunitySettled").cloned())
        .map_err(|_| RunnerError::Integrity)?;
    let mut settlements = Vec::new();
    for log in &trace.logs {
        if log.address != plan.target_contract {
            continue;
        }
        let topics = log
            .topics
            .iter()
            .map(|topic| topic.parse::<H256>().map_err(|_| RunnerError::Integrity))
            .collect::<Result<Vec<_>, _>>()?;
        if topics.first() != Some(&event.signature()) {
            continue;
        }
        let raw = RawLog {
            topics,
            data: decode_data(&log.data, 4096)?,
        };
        let decoded = event.parse_log(raw).map_err(|_| RunnerError::Integrity)?;
        let route_hash = log_param(&decoded.params, "routeId")
            .and_then(|token| match token {
                Token::FixedBytes(value) if value.len() == 32 => Some(hex::encode(value)),
                _ => None,
            })
            .ok_or(RunnerError::Integrity)?;
        let asset = log_param(&decoded.params, "asset")
            .and_then(|token| match token {
                Token::Address(value) => Some(format!("0x{}", hex::encode(value))),
                _ => None,
            })
            .ok_or(RunnerError::Integrity)?;
        let flash_amount = uint_param(&decoded.params, "flashAmount")?;
        let realized_profit = uint_param(&decoded.params, "realizedProfit")?;
        if route_hash != plan.route_hash
            || asset != plan.token_path[0]
            || flash_amount
                != plan
                    .input_amount
                    .parse::<u128>()
                    .map_err(|_| RunnerError::Integrity)?
        {
            return Err(RunnerError::Integrity);
        }
        settlements.push(Settlement {
            route_hash,
            realized_profit,
        });
    }
    if settlements.len() == 1 {
        settlements.pop().ok_or(RunnerError::Integrity)
    } else {
        Err(RunnerError::Integrity)
    }
}

fn log_param<'a>(params: &'a [ethabi::LogParam], name: &str) -> Option<&'a Token> {
    params
        .iter()
        .find(|parameter| parameter.name == name)
        .map(|parameter| &parameter.value)
}

fn uint_param(params: &[ethabi::LogParam], name: &str) -> Result<u128, RunnerError> {
    let value = match log_param(params, name) {
        Some(Token::Uint(value)) => *value,
        _ => return Err(RunnerError::Integrity),
    };
    if value > primitive_types::U256::from(u128::MAX) {
        return Err(RunnerError::Integrity);
    }
    Ok(value.low_u128())
}

fn decode_revert(fallback: &str, data: Option<&str>) -> String {
    let Some(data) = data.and_then(|value| decode_data(value, 4096).ok()) else {
        return bounded_reason(fallback);
    };
    if data.len() < 4 {
        return bounded_reason(fallback);
    }
    let selector = &data[..4];
    if selector == [0x08, 0xc3, 0x79, 0xa0] {
        if let Ok(tokens) = ethabi::decode(&[ParamType::String], &data[4..]) {
            if let Some(Token::String(reason)) = tokens.first() {
                return bounded_reason(reason);
            }
        }
    }
    for name in [
        "Unauthorized",
        "Paused",
        "Reentrant",
        "ZeroAddress",
        "ZeroAmount",
        "InvalidLeg",
        "Expired",
        "CallbackSpoof",
        "NoActiveExecution",
        "MalformedLegs",
        "TransferFailed",
    ] {
        if selector == ethabi::short_signature(name, &[]) {
            return name.to_string();
        }
    }
    for (name, parameters) in [
        ("UnsupportedAsset", vec![ParamType::Address]),
        ("InvalidRouter", vec![ParamType::Address]),
        ("InvalidFactory", vec![ParamType::Address]),
        ("InvalidPool", vec![ParamType::Address]),
        ("InvalidRecipient", vec![ParamType::Address]),
        (
            "InputLimit",
            vec![ParamType::Uint(256), ParamType::Uint(256)],
        ),
        (
            "MinProfit",
            vec![ParamType::Uint(256), ParamType::Uint(256)],
        ),
    ] {
        if selector == ethabi::short_signature(name, &parameters) {
            return name.to_string();
        }
    }
    bounded_reason(fallback)
}

fn decode_data(value: &str, maximum_bytes: usize) -> Result<Vec<u8>, RunnerError> {
    let body = value.strip_prefix("0x").ok_or(RunnerError::Integrity)?;
    if body.len() % 2 != 0 || body.len() / 2 > maximum_bytes {
        return Err(RunnerError::Integrity);
    }
    hex::decode(body).map_err(|_| RunnerError::Integrity)
}

fn signed_difference(left: u128, right: u128) -> Result<i128, RunnerError> {
    if left >= right {
        i128::try_from(left - right).map_err(|_| RunnerError::Integrity)
    } else {
        i128::try_from(right - left)
            .ok()
            .and_then(i128::checked_neg)
            .ok_or(RunnerError::Integrity)
    }
}

fn bounded_reason(value: &str) -> String {
    let value = value
        .chars()
        .filter(|character| !character.is_control())
        .take(1024)
        .collect::<String>();
    if value.is_empty() {
        "execution reverted".to_string()
    } else {
        value
    }
}
