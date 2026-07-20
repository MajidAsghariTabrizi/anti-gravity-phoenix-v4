use crate::abi::encode_execute_opportunity;
use crate::model::{
    canonical_block_hash, canonical_digest, CanonicalAddress, ExecutionRequest, ValidatedLeg,
    MAX_APPROVER_BYTES, MAX_ROUTE_FINGERPRINT_BYTES, MAX_ROUTE_LEGS,
};
use crate::{
    APPROVAL_POLICY_VERSION, ARBITRUM_NATIVE_USDC_ADDRESS, ARBITRUM_ONE_CHAIN_ID,
    ARBITRUM_WETH_ADDRESS, CURRENT_ROUTE_FINGERPRINT, CURRENT_ROUTE_POOL_3000_ADDRESS,
    CURRENT_ROUTE_POOL_500_ADDRESS, REQUEST_SCHEMA_VERSION,
};
use chrono::{DateTime, Duration, Timelike, Utc};
use phoenix_fork_sandbox::model::{
    CounterfactualResult, CounterfactualResultBody, ForkIdentity, PinnedBlockEvidence,
    SimulationEvidence, SimulationStatus, UnsignedTransactionPlan,
};
use serde::Serialize;
use sha2::{Digest, Sha256};
use sqlx::postgres::PgPoolOptions;
use sqlx::types::Json;
use sqlx::{PgPool, Postgres, Row, Transaction};
use thiserror::Error;
use uuid::Uuid;

pub const APPROVAL_CONFIRMATION: &str = "APPROVE_ONE_SIMULATED_PHOENIX_CANARY";
pub const MAX_APPROVAL_TTL_SECONDS: u64 = 900;
const CURRENT_ROUTE_POOL_500_ID: &str =
    "0x82af49447d8a07e3bd95bd0d56f35241523fbab1:0xaf88d065e77c8cc2239327c5edb3a432268e5831:500";
const CURRENT_ROUTE_POOL_3000_ID: &str =
    "0x82af49447d8a07e3bd95bd0d56f35241523fbab1:0xaf88d065e77c8cc2239327c5edb3a432268e5831:3000";

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ApprovalInput {
    pub simulation_result_hash: String,
    pub approved_by: String,
    pub approval_ttl_seconds: u64,
    pub max_priority_fee_per_gas: u128,
}

#[derive(Clone, Debug, Serialize, PartialEq, Eq)]
pub struct ApprovalOutcome {
    pub request_id: Uuid,
    pub simulation_result_hash: String,
    pub plan_hash: String,
    pub approval_digest: String,
    pub approval_deadline: DateTime<Utc>,
    pub created: bool,
}

#[derive(Clone)]
pub struct ApprovalMaterializer {
    pool: PgPool,
}

impl ApprovalMaterializer {
    pub async fn connect(dsn: &str) -> Result<Self, ApprovalError> {
        let pool = PgPoolOptions::new()
            .max_connections(2)
            .acquire_timeout(std::time::Duration::from_secs(5))
            .connect(dsn)
            .await
            .map_err(|_| ApprovalError::Database)?;
        Ok(Self { pool })
    }

    pub fn from_pool(pool: PgPool) -> Self {
        Self { pool }
    }

    pub async fn materialize(
        &self,
        input: ApprovalInput,
        now: DateTime<Utc>,
    ) -> Result<ApprovalOutcome, ApprovalError> {
        validate_input(&input)?;
        let now = now
            .with_nanosecond(0)
            .ok_or(ApprovalError::InvalidCandidate)?;
        let mut transaction = self.pool.begin().await.map_err(database_error)?;
        let schema_valid: bool = sqlx::query_scalar(
            "SELECT EXISTS(
                SELECT 1
                FROM live_canary.schema_contract
                WHERE version = 'phoenix.live-canary-schema.v2'
             )",
        )
        .fetch_one(&mut *transaction)
        .await
        .map_err(database_error)?;
        if !schema_valid {
            return Err(ApprovalError::Database);
        }
        sqlx::query("SELECT pg_advisory_xact_lock(hashtextextended($1, 0))")
            .bind(format!(
                "live-canary-approval:{}",
                input.simulation_result_hash
            ))
            .execute(&mut *transaction)
            .await
            .map_err(database_error)?;
        let control = sqlx::query(
            "SELECT armed, kill_switch
             FROM live_canary.control
             WHERE singleton
             FOR UPDATE",
        )
        .fetch_one(&mut *transaction)
        .await
        .map_err(database_error)?;
        if control
            .try_get::<bool, _>("armed")
            .map_err(database_error)?
            || !control
                .try_get::<bool, _>("kill_switch")
                .map_err(database_error)?
        {
            return Err(ApprovalError::UnsafeControlState);
        }

        let stored = load_simulation(&mut transaction, &input.simulation_result_hash).await?;
        let request = build_request(&stored.plan, &stored.result, &input, now)?;
        if let Some(existing) =
            load_existing_request(&mut transaction, &input.simulation_result_hash).await?
        {
            ensure_same_evidence(&existing, &request)?;
            transaction.commit().await.map_err(database_error)?;
            return Ok(outcome(existing, false));
        }

        insert_approved_request(&mut transaction, &request).await?;
        transaction.commit().await.map_err(database_error)?;
        Ok(outcome(request, true))
    }
}

struct StoredSimulation {
    plan: UnsignedTransactionPlan,
    result: CounterfactualResult,
}

async fn load_simulation(
    transaction: &mut Transaction<'_, Postgres>,
    result_hash: &str,
) -> Result<StoredSimulation, ApprovalError> {
    let row = sqlx::query(
        "SELECT
            result_hash, plan_hash, shadow_decision_id::text AS shadow_decision_id,
            plan_schema_version, result_schema_version, plan, evidence, status,
            predicted_gross_profit::text AS predicted_gross_profit,
            predicted_total_cost::text AS predicted_total_cost,
            predicted_net_pnl::text AS predicted_net_pnl,
            simulated_gross_profit::text AS simulated_gross_profit,
            simulated_gas_cost::text AS simulated_gas_cost,
            simulated_balance_delta::text AS simulated_balance_delta,
            simulated_net_pnl::text AS simulated_net_pnl,
            prediction_error::text AS prediction_error,
            gas_estimate::text AS gas_estimate,
            gas_used::text AS gas_used,
            model_version, policy_version, fork_chain_id,
            fork_block_number::text AS fork_block_number, fork_block_hash,
            fork_instance_hash, local_block_number::text AS local_block_number,
            local_block_hash, simulated_at, revert_reason,
            fork_only, shadow_only, live_execution, execution_eligible,
            execution_request_created, public_broadcast, signer_used
         FROM public.fork_simulation_results
         WHERE result_hash = $1
         FOR SHARE",
    )
    .bind(result_hash)
    .fetch_optional(&mut **transaction)
    .await
    .map_err(database_error)?
    .ok_or(ApprovalError::SimulationNotFound)?;
    let plan: Json<UnsignedTransactionPlan> = row.try_get("plan").map_err(database_error)?;
    let plan_schema_version: String = row.try_get("plan_schema_version").map_err(database_error)?;
    if plan_schema_version != plan.0.schema_version {
        return Err(ApprovalError::InvalidCandidate);
    }
    let evidence: Json<SimulationEvidence> = row.try_get("evidence").map_err(database_error)?;
    let status: String = row.try_get("status").map_err(database_error)?;
    let body = CounterfactualResultBody {
        schema_version: row
            .try_get("result_schema_version")
            .map_err(database_error)?,
        plan_hash: row.try_get("plan_hash").map_err(database_error)?,
        shadow_decision_id: row.try_get("shadow_decision_id").map_err(database_error)?,
        status: match status.as_str() {
            "passed" => SimulationStatus::Passed,
            "reverted" => SimulationStatus::Reverted,
            _ => return Err(ApprovalError::InvalidCandidate),
        },
        predicted_gross_profit: row
            .try_get("predicted_gross_profit")
            .map_err(database_error)?,
        predicted_total_cost: row
            .try_get("predicted_total_cost")
            .map_err(database_error)?,
        predicted_net_pnl: row.try_get("predicted_net_pnl").map_err(database_error)?,
        simulated_gross_profit: row
            .try_get("simulated_gross_profit")
            .map_err(database_error)?,
        simulated_gas_cost: row.try_get("simulated_gas_cost").map_err(database_error)?,
        simulated_balance_delta: row
            .try_get("simulated_balance_delta")
            .map_err(database_error)?,
        simulated_net_pnl: row.try_get("simulated_net_pnl").map_err(database_error)?,
        prediction_error: row.try_get("prediction_error").map_err(database_error)?,
        gas_estimate: optional_u64(&row, "gas_estimate")?,
        gas_used: optional_u64(&row, "gas_used")?,
        model_version: row.try_get("model_version").map_err(database_error)?,
        policy_version: row.try_get("policy_version").map_err(database_error)?,
        fork: ForkIdentity {
            chain_id: positive_u64(&row, "fork_chain_id")?,
            fork_block: PinnedBlockEvidence {
                number: positive_numeric_u64(&row, "fork_block_number")?,
                hash: row.try_get("fork_block_hash").map_err(database_error)?,
            },
            fork_instance_hash: row.try_get("fork_instance_hash").map_err(database_error)?,
            local_block: PinnedBlockEvidence {
                number: positive_numeric_u64(&row, "local_block_number")?,
                hash: row.try_get("local_block_hash").map_err(database_error)?,
            },
        },
        simulated_at: row.try_get("simulated_at").map_err(database_error)?,
        revert_reason: row.try_get("revert_reason").map_err(database_error)?,
        evidence: evidence.0,
        fork_only: row.try_get("fork_only").map_err(database_error)?,
        shadow_only: row.try_get("shadow_only").map_err(database_error)?,
        live_execution: row.try_get("live_execution").map_err(database_error)?,
        execution_eligible: row.try_get("execution_eligible").map_err(database_error)?,
        execution_request_created: row
            .try_get("execution_request_created")
            .map_err(database_error)?,
        public_broadcast: row.try_get("public_broadcast").map_err(database_error)?,
        signer_used: row.try_get("signer_used").map_err(database_error)?,
    };
    Ok(StoredSimulation {
        plan: plan.0,
        result: CounterfactualResult {
            result_hash: row.try_get("result_hash").map_err(database_error)?,
            body,
        },
    })
}

fn build_request(
    plan: &UnsignedTransactionPlan,
    result: &CounterfactualResult,
    input: &ApprovalInput,
    now: DateTime<Utc>,
) -> Result<ExecutionRequest, ApprovalError> {
    result
        .validate_plan_binding(plan)
        .map_err(|_| ApprovalError::InvalidCandidate)?;
    if result.body.status != SimulationStatus::Passed
        || result.result_hash != input.simulation_result_hash
        || plan.verification.verification_status != "agreed"
        || plan.verification.independent_verification_status != "agreed"
        || plan.verification.agreement_state != "agreed"
        || plan.verification.primary_provider_id.trim().is_empty()
        || plan.verification.secondary_provider_id.trim().is_empty()
        || plan.verification.primary_provider_id == plan.verification.secondary_provider_id
        || plan.chain_id != ARBITRUM_ONE_CHAIN_ID
        || plan.route.pool_addresses.is_empty()
        || plan.route.pool_addresses.len() > MAX_ROUTE_LEGS
        || plan.route.pool_addresses.len() != plan.route.pool_ids.len()
        || plan.route.pool_addresses.len() != plan.route.protocols.len()
        || plan.route.pool_addresses.len() != plan.route.fees.len()
        || plan.route.pool_addresses.len() != plan.route.directions.len()
        || plan.route.pool_addresses.len() != plan.minimum_leg_outputs.len()
        || plan
            .route
            .fees
            .iter()
            .any(|fee| *fee == 0 || *fee >= 1_000_000)
        || plan
            .route
            .pool_ids
            .iter()
            .any(|value| value.trim().is_empty())
        || plan
            .route
            .protocols
            .iter()
            .any(|value| value.trim().is_empty())
        || plan.token_path.len() != plan.route.pool_addresses.len() + 1
        || plan.token_path.first() != plan.token_path.last()
        || plan.route.route_fingerprint.trim().is_empty()
        || plan.route.route_fingerprint.len() > MAX_ROUTE_FINGERPRINT_BYTES
        || plan.route.route_fingerprint.chars().any(char::is_control)
        || !matches_current_route(plan)
        || plan.gas_estimate == 0
        || plan.value != "0"
        || result.body.simulated_at > now
    {
        return Err(ApprovalError::InvalidCandidate);
    }
    let simulated_gross_profit = positive_u128(result.body.simulated_gross_profit.as_deref())?;
    let simulated_gas_cost = positive_u128(result.body.simulated_gas_cost.as_deref())?;
    let simulated_balance_delta = positive_u128(result.body.simulated_balance_delta.as_deref())?;
    let simulated_net_pnl = positive_u128(result.body.simulated_net_pnl.as_deref())?;
    let minimum_required_net_pnl = plan
        .predicted
        .minimum_required_net_pnl
        .parse::<u128>()
        .ok()
        .filter(|value| *value > 0)
        .ok_or(ApprovalError::InvalidCandidate)?;
    let minimum_profit = plan
        .minimum_profit
        .parse::<u128>()
        .ok()
        .filter(|value| *value > 0)
        .ok_or(ApprovalError::InvalidCandidate)?;
    if simulated_net_pnl < minimum_required_net_pnl || simulated_gross_profit < minimum_profit {
        return Err(ApprovalError::NotProfitable);
    }
    let selected_size = plan
        .input_amount
        .parse::<u128>()
        .ok()
        .filter(|value| *value > 0)
        .ok_or(ApprovalError::InvalidCandidate)?;
    let maximum_input_amount = plan
        .maximum_input_amount
        .parse::<u128>()
        .ok()
        .filter(|value| *value >= selected_size)
        .ok_or(ApprovalError::InvalidCandidate)?;
    let max_fee_per_gas = plan
        .gas_price_wei
        .parse::<u128>()
        .ok()
        .filter(|value| *value >= input.max_priority_fee_per_gas)
        .ok_or(ApprovalError::InvalidCandidate)?;
    let simulation_gas_estimate = result
        .body
        .gas_estimate
        .filter(|value| *value > 0 && *value <= plan.gas_estimate)
        .ok_or(ApprovalError::InvalidCandidate)?;
    let simulation_gas_used = result
        .body
        .gas_used
        .filter(|value| *value > 0 && *value <= simulation_gas_estimate)
        .ok_or(ApprovalError::InvalidCandidate)?;
    let recomputed_gas_cost = max_fee_per_gas
        .checked_mul(u128::from(simulation_gas_used))
        .ok_or(ApprovalError::InvalidCandidate)?;
    if simulated_balance_delta != simulated_gross_profit
        || simulated_gas_cost != recomputed_gas_cost
        || simulated_gross_profit.checked_sub(simulated_gas_cost) != Some(simulated_net_pnl)
        || result.body.evidence.settled_route_hash.as_deref() != Some(plan.route_hash.as_str())
        || !matches!(
            result.body.evidence.call_output_hash.as_deref(),
            Some(hash) if canonical_digest(hash)
        )
        || !matches!(
            result.body.evidence.trace_hash.as_deref(),
            Some(hash) if canonical_digest(hash)
        )
    {
        return Err(ApprovalError::InvalidCandidate);
    }
    let transaction_deadline = i64::try_from(plan.deadline)
        .ok()
        .and_then(|seconds| DateTime::from_timestamp(seconds, 0))
        .ok_or(ApprovalError::InvalidCandidate)?;
    let requested_deadline = now
        .checked_add_signed(Duration::seconds(
            i64::try_from(input.approval_ttl_seconds).map_err(|_| ApprovalError::InvalidInput)?,
        ))
        .ok_or(ApprovalError::InvalidInput)?;
    let approval_deadline = requested_deadline.min(transaction_deadline);
    if approval_deadline <= now {
        return Err(ApprovalError::ExpiredCandidate);
    }
    let route_id = decode_route_id(&plan.route_hash)?;
    let token_path = plan
        .token_path
        .iter()
        .map(|address| CanonicalAddress::parse(address))
        .collect::<Result<Vec<_>, _>>()
        .map_err(|_| ApprovalError::InvalidCandidate)?;
    let legs = plan
        .route
        .pool_addresses
        .iter()
        .enumerate()
        .map(|(index, pool)| {
            Ok(ValidatedLeg {
                pool: CanonicalAddress::parse(pool).map_err(|_| ApprovalError::InvalidCandidate)?,
                token_in: token_path[index],
                token_out: token_path[index + 1],
                fee: plan.route.fees[index],
                zero_for_one: match plan.route.directions[index].as_str() {
                    "zero_for_one" => true,
                    "one_for_zero" => false,
                    _ => return Err(ApprovalError::InvalidCandidate),
                },
                min_amount_out: plan.minimum_leg_outputs[index]
                    .parse::<u128>()
                    .ok()
                    .filter(|value| *value > 0)
                    .ok_or(ApprovalError::InvalidCandidate)?,
            })
        })
        .collect::<Result<Vec<_>, ApprovalError>>()?;
    let executor_address = CanonicalAddress::parse(&plan.target_contract)
        .map_err(|_| ApprovalError::InvalidCandidate)?;
    if !canonical_digest(&plan.target_code_hash)
        || !canonical_digest(&plan.calldata_hash)
        || !canonical_block_hash(&plan.pinned_block.hash)
        || plan.pinned_block.number == 0
    {
        return Err(ApprovalError::InvalidCandidate);
    }
    let mut request = ExecutionRequest {
        id: Uuid::new_v4(),
        opportunity_id: Uuid::parse_str(&plan.shadow_decision_id)
            .map_err(|_| ApprovalError::InvalidCandidate)?,
        schema_version: REQUEST_SCHEMA_VERSION.to_string(),
        chain_id: plan.chain_id,
        route_id,
        route_fingerprint: plan.route.route_fingerprint.clone(),
        selected_size,
        token_path,
        origin_router: CanonicalAddress::parse(&plan.origin_router)
            .map_err(|_| ApprovalError::InvalidCandidate)?,
        executor_address,
        executor_code_hash: plan.target_code_hash.clone(),
        calldata_hash: plan.calldata_hash.clone(),
        simulation_result_hash: result.result_hash.clone(),
        plan_hash: result.body.plan_hash.clone(),
        pinned_block_number: plan.pinned_block.number,
        pinned_block_hash: plan.pinned_block.hash.clone(),
        flash_asset: CanonicalAddress::parse(&plan.token_path[0])
            .map_err(|_| ApprovalError::InvalidCandidate)?,
        flash_amount: selected_size,
        maximum_input_amount,
        minimum_profit,
        expected_profit: simulated_gross_profit,
        deadline: transaction_deadline,
        legs,
        gas_limit: plan.gas_estimate,
        max_fee_per_gas,
        max_priority_fee_per_gas: input.max_priority_fee_per_gas,
        approved_by: input.approved_by.clone(),
        approved_at: now,
        approval_deadline,
        policy_version: APPROVAL_POLICY_VERSION.to_string(),
        approval_digest: String::new(),
    };
    request
        .validate_current_route()
        .map_err(|_| ApprovalError::InvalidCandidate)?;
    let calldata = encode_execute_opportunity(&request, request.executor_address)
        .map_err(|_| ApprovalError::InvalidCandidate)?;
    if hex::encode(Sha256::digest(calldata)) != request.calldata_hash {
        return Err(ApprovalError::CalldataMismatch);
    }
    request.approval_digest = request
        .canonical_approval_digest()
        .map_err(|_| ApprovalError::InvalidCandidate)?;
    Ok(request)
}

async fn load_existing_request(
    transaction: &mut Transaction<'_, Postgres>,
    result_hash: &str,
) -> Result<Option<ExecutionRequest>, ApprovalError> {
    let query = format!(
        "{} WHERE r.simulation_result_hash = $1 FOR UPDATE OF r",
        crate::store::request_select()
    );
    let row = sqlx::query(&query)
        .bind(result_hash)
        .fetch_optional(&mut **transaction)
        .await
        .map_err(database_error)?;
    row.map(|row| crate::store::decode_request(&row).map_err(|_| ApprovalError::Database))
        .transpose()
}

async fn insert_approved_request(
    transaction: &mut Transaction<'_, Postgres>,
    request: &ExecutionRequest,
) -> Result<(), ApprovalError> {
    sqlx::query(
        "INSERT INTO live_canary.execution_requests(
            id, opportunity_id, schema_version, chain_id, route_id,
            route_fingerprint, selected_size, token_path, origin_router,
            executor_address, executor_code_hash, calldata_hash,
            simulation_result_hash, plan_hash, pinned_block_number,
            pinned_block_hash, flash_asset, flash_amount, maximum_input_amount,
            minimum_profit, expected_profit, deadline, legs, gas_limit,
            max_fee_per_gas, max_priority_fee_per_gas, approved_by, approved_at,
            approval_deadline, policy_version, approval_digest, status
         )
         VALUES (
            $1, $2, $3, $4, $5, $6, $7::numeric, $8, $9, $10, $11, $12,
            $13, $14, $15::numeric, $16, $17, $18::numeric, $19::numeric,
            $20::numeric, $21::numeric, $22, $23, $24, $25::numeric,
            $26::numeric, $27, $28, $29, $30, $31, 'approved'
         )",
    )
    .bind(request.id)
    .bind(request.opportunity_id)
    .bind(&request.schema_version)
    .bind(i64::try_from(request.chain_id).map_err(|_| ApprovalError::InvalidCandidate)?)
    .bind(format!("0x{}", hex::encode(request.route_id)))
    .bind(&request.route_fingerprint)
    .bind(request.selected_size.to_string())
    .bind(Json(
        request
            .token_path
            .iter()
            .map(ToString::to_string)
            .collect::<Vec<_>>(),
    ))
    .bind(request.origin_router.to_string())
    .bind(request.executor_address.to_string())
    .bind(&request.executor_code_hash)
    .bind(&request.calldata_hash)
    .bind(&request.simulation_result_hash)
    .bind(&request.plan_hash)
    .bind(request.pinned_block_number.to_string())
    .bind(&request.pinned_block_hash)
    .bind(request.flash_asset.to_string())
    .bind(request.flash_amount.to_string())
    .bind(request.maximum_input_amount.to_string())
    .bind(request.minimum_profit.to_string())
    .bind(request.expected_profit.to_string())
    .bind(request.deadline)
    .bind(Json(&request.legs))
    .bind(i64::try_from(request.gas_limit).map_err(|_| ApprovalError::InvalidCandidate)?)
    .bind(request.max_fee_per_gas.to_string())
    .bind(request.max_priority_fee_per_gas.to_string())
    .bind(&request.approved_by)
    .bind(request.approved_at)
    .bind(request.approval_deadline)
    .bind(&request.policy_version)
    .bind(&request.approval_digest)
    .execute(&mut **transaction)
    .await
    .map_err(database_error)?;
    Ok(())
}

fn ensure_same_evidence(
    existing: &ExecutionRequest,
    candidate: &ExecutionRequest,
) -> Result<(), ApprovalError> {
    if existing.simulation_result_hash != candidate.simulation_result_hash
        || existing.plan_hash != candidate.plan_hash
        || existing.opportunity_id != candidate.opportunity_id
        || existing.schema_version != candidate.schema_version
        || existing.chain_id != candidate.chain_id
        || existing.route_id != candidate.route_id
        || existing.route_fingerprint != candidate.route_fingerprint
        || existing.selected_size != candidate.selected_size
        || existing.token_path != candidate.token_path
        || existing.origin_router != candidate.origin_router
        || existing.executor_address != candidate.executor_address
        || existing.executor_code_hash != candidate.executor_code_hash
        || existing.calldata_hash != candidate.calldata_hash
        || existing.pinned_block_number != candidate.pinned_block_number
        || existing.pinned_block_hash != candidate.pinned_block_hash
        || existing.flash_asset != candidate.flash_asset
        || existing.flash_amount != candidate.flash_amount
        || existing.maximum_input_amount != candidate.maximum_input_amount
        || existing.minimum_profit != candidate.minimum_profit
        || existing.expected_profit != candidate.expected_profit
        || existing.deadline != candidate.deadline
        || existing.legs != candidate.legs
        || existing.gas_limit != candidate.gas_limit
        || existing.max_fee_per_gas != candidate.max_fee_per_gas
        || existing.max_priority_fee_per_gas != candidate.max_priority_fee_per_gas
        || existing.policy_version != candidate.policy_version
    {
        return Err(ApprovalError::DuplicateConflict);
    }
    Ok(())
}

fn matches_current_route(plan: &UnsignedTransactionPlan) -> bool {
    plan.route.route_fingerprint == CURRENT_ROUTE_FINGERPRINT
        && exact_strings(
            &plan.token_path,
            &[
                ARBITRUM_WETH_ADDRESS,
                ARBITRUM_NATIVE_USDC_ADDRESS,
                ARBITRUM_WETH_ADDRESS,
            ],
        )
        && exact_strings(
            &plan.route.pool_ids,
            &[CURRENT_ROUTE_POOL_500_ID, CURRENT_ROUTE_POOL_3000_ID],
        )
        && exact_strings(
            &plan.route.pool_addresses,
            &[
                CURRENT_ROUTE_POOL_500_ADDRESS,
                CURRENT_ROUTE_POOL_3000_ADDRESS,
            ],
        )
        && exact_strings(&plan.route.protocols, &["UniswapV3", "UniswapV3"])
        && exact_strings(&plan.route.directions, &["zero_for_one", "one_for_zero"])
        && plan.route.fees == [500, 3_000]
}

fn exact_strings(actual: &[String], expected: &[&str]) -> bool {
    actual.len() == expected.len()
        && actual
            .iter()
            .zip(expected)
            .all(|(actual, expected)| actual == expected)
}

fn outcome(request: ExecutionRequest, created: bool) -> ApprovalOutcome {
    ApprovalOutcome {
        request_id: request.id,
        simulation_result_hash: request.simulation_result_hash,
        plan_hash: request.plan_hash,
        approval_digest: request.approval_digest,
        approval_deadline: request.approval_deadline,
        created,
    }
}

fn validate_input(input: &ApprovalInput) -> Result<(), ApprovalError> {
    if !canonical_digest(&input.simulation_result_hash)
        || input.approved_by.trim().is_empty()
        || input.approved_by.len() > MAX_APPROVER_BYTES
        || input.approved_by.chars().any(char::is_control)
        || input.approval_ttl_seconds == 0
        || input.approval_ttl_seconds > MAX_APPROVAL_TTL_SECONDS
        || input.max_priority_fee_per_gas == 0
    {
        return Err(ApprovalError::InvalidInput);
    }
    Ok(())
}

fn decode_route_id(value: &str) -> Result<[u8; 32], ApprovalError> {
    if !canonical_digest(value) {
        return Err(ApprovalError::InvalidCandidate);
    }
    hex::decode(value)
        .ok()
        .and_then(|decoded| decoded.try_into().ok())
        .ok_or(ApprovalError::InvalidCandidate)
}

fn positive_u128(value: Option<&str>) -> Result<u128, ApprovalError> {
    value
        .and_then(|raw| raw.parse::<u128>().ok())
        .filter(|parsed| *parsed > 0)
        .ok_or(ApprovalError::NotProfitable)
}

fn optional_u64(row: &sqlx::postgres::PgRow, name: &str) -> Result<Option<u64>, ApprovalError> {
    row.try_get::<Option<String>, _>(name)
        .map_err(database_error)?
        .map(|value| {
            value
                .parse::<u64>()
                .map_err(|_| ApprovalError::InvalidCandidate)
        })
        .transpose()
}

fn positive_numeric_u64(row: &sqlx::postgres::PgRow, name: &str) -> Result<u64, ApprovalError> {
    row.try_get::<String, _>(name)
        .map_err(database_error)?
        .parse::<u64>()
        .ok()
        .filter(|value| *value > 0)
        .ok_or(ApprovalError::InvalidCandidate)
}

fn positive_u64(row: &sqlx::postgres::PgRow, name: &str) -> Result<u64, ApprovalError> {
    u64::try_from(row.try_get::<i64, _>(name).map_err(database_error)?)
        .ok()
        .filter(|value| *value > 0)
        .ok_or(ApprovalError::InvalidCandidate)
}

fn database_error(_: sqlx::Error) -> ApprovalError {
    ApprovalError::Database
}

#[derive(Clone, Copy, Debug, Error, PartialEq, Eq)]
pub enum ApprovalError {
    #[error("approval input is invalid")]
    InvalidInput,
    #[error("simulation evidence was not found")]
    SimulationNotFound,
    #[error("simulation evidence is not an approvable candidate")]
    InvalidCandidate,
    #[error("simulation evidence is no longer within its deadline")]
    ExpiredCandidate,
    #[error("simulation did not prove the required profit")]
    NotProfitable,
    #[error("reconstructed calldata does not match simulation")]
    CalldataMismatch,
    #[error("an existing approval conflicts with this evidence")]
    DuplicateConflict,
    #[error("approval database operation failed")]
    Database,
    #[error("approval requires a disarmed database control with kill switch engaged")]
    UnsafeControlState,
}

impl ApprovalError {
    pub const fn code(self) -> &'static str {
        match self {
            Self::InvalidInput => "invalid_input",
            Self::SimulationNotFound => "simulation_not_found",
            Self::InvalidCandidate => "invalid_candidate",
            Self::ExpiredCandidate => "expired_candidate",
            Self::NotProfitable => "not_profitable",
            Self::CalldataMismatch => "calldata_mismatch",
            Self::DuplicateConflict => "duplicate_conflict",
            Self::Database => "database_error",
            Self::UnsafeControlState => "unsafe_control_state",
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::TimeZone;
    use phoenix_fork_sandbox::model::{
        PredictedEconomics, RoutePlan, VerificationEvidence, PLAN_SCHEMA_VERSION,
        RESULT_SCHEMA_VERSION,
    };

    const WETH: &str = "0x82af49447d8a07e3bd95bd0d56f35241523fbab1";
    const USDC: &str = "0xaf88d065e77c8cc2239327c5edb3a432268e5831";
    const EXECUTOR: &str = "0x3333333333333333333333333333333333333333";

    #[test]
    fn independently_simulated_plan_materializes_all_approval_bindings() {
        let now = fixture_time();
        let (plan, result) = fixture(now);
        let request = build_request(&plan, &result, &input(&result), now)
            .expect("materialize approved request");
        assert_eq!(request.route_fingerprint, plan.route.route_fingerprint);
        assert_eq!(request.selected_size, 1_000_000_000_000_000);
        assert_eq!(
            request
                .token_path
                .iter()
                .map(ToString::to_string)
                .collect::<Vec<_>>(),
            plan.token_path
        );
        assert_eq!(request.executor_address.to_string(), plan.target_contract);
        assert_eq!(request.executor_code_hash, plan.target_code_hash);
        assert_eq!(request.calldata_hash, plan.calldata_hash);
        assert_eq!(request.simulation_result_hash, result.result_hash);
        assert_eq!(request.plan_hash, result.body.plan_hash);
        assert_eq!(request.pinned_block_number, plan.pinned_block.number);
        assert_eq!(request.pinned_block_hash, plan.pinned_block.hash);
        assert_eq!(
            request.canonical_approval_digest().expect("digest"),
            request.approval_digest
        );
    }

    #[test]
    fn arbitrary_calldata_cannot_be_materialized() {
        let now = fixture_time();
        let (mut plan, _) = fixture(now);
        plan.calldata = "0x00".to_string();
        plan.calldata_hash = hex::encode(Sha256::digest([0_u8]));
        let result = result_for_plan(&plan, now, "230000000");
        assert_eq!(
            build_request(&plan, &result, &input(&result), now),
            Err(ApprovalError::CalldataMismatch)
        );
    }

    #[test]
    fn independent_provider_identity_is_mandatory() {
        let now = fixture_time();
        let (mut plan, _) = fixture(now);
        plan.verification
            .secondary_provider_id
            .clone_from(&plan.verification.primary_provider_id);
        let result = result_for_plan(&plan, now, "230000000");
        assert_eq!(
            build_request(&plan, &result, &input(&result), now),
            Err(ApprovalError::InvalidCandidate)
        );
    }

    #[test]
    fn simulation_pool_state_must_match_the_approved_plan() {
        let now = fixture_time();
        let (plan, result) = fixture(now);
        let mut body = result.body;
        body.evidence.observed_pool_state_hashes[0] = "9".repeat(64);
        let tampered = CounterfactualResult::from_body(body).expect("tampered result");
        assert_eq!(
            build_request(&plan, &tampered, &input(&tampered), now),
            Err(ApprovalError::InvalidCandidate)
        );
    }

    #[test]
    fn plans_outside_the_reviewed_current_route_cannot_be_materialized() {
        let now = fixture_time();

        let (mut fingerprint_plan, _) = fixture(now);
        fingerprint_plan.route.route_fingerprint = "alternate-route".to_string();
        let result = result_for_plan(&fingerprint_plan, now, "230000000");
        assert_eq!(
            build_request(&fingerprint_plan, &result, &input(&result), now),
            Err(ApprovalError::InvalidCandidate)
        );

        let (mut pool_plan, _) = fixture(now);
        pool_plan.route.pool_addresses[0] =
            "0x5555555555555555555555555555555555555555".to_string();
        let result = result_for_plan(&pool_plan, now, "230000000");
        assert_eq!(
            build_request(&pool_plan, &result, &input(&result), now),
            Err(ApprovalError::InvalidCandidate)
        );

        let (mut protocol_plan, _) = fixture(now);
        protocol_plan.route.protocols[1] = "SushiSwapV3".to_string();
        let result = result_for_plan(&protocol_plan, now, "230000000");
        assert_eq!(
            build_request(&protocol_plan, &result, &input(&result), now),
            Err(ApprovalError::InvalidCandidate)
        );
    }

    #[test]
    fn simulated_profit_must_clear_the_reviewed_floor() {
        let now = fixture_time();
        let (plan, _) = fixture(now);
        let result = result_for_plan(&plan, now, "0");
        assert_eq!(
            build_request(&plan, &result, &input(&result), now),
            Err(ApprovalError::NotProfitable)
        );
    }

    #[test]
    fn expired_plan_cannot_be_approved() {
        let now = fixture_time();
        let (mut plan, _) = fixture(now);
        plan.deadline = u64::try_from(now.timestamp()).expect("timestamp");
        let request = encoded_request(now, &plan);
        let calldata =
            encode_execute_opportunity(&request, request.executor_address).expect("calldata");
        plan.calldata_hash = hex::encode(Sha256::digest(&calldata));
        plan.calldata = format!("0x{}", hex::encode(calldata));
        let result = result_for_plan(&plan, now, "230000000");
        assert_eq!(
            build_request(&plan, &result, &input(&result), now),
            Err(ApprovalError::ExpiredCandidate)
        );
    }

    #[test]
    fn duplicate_simulation_is_idempotent_but_conflicting_evidence_is_rejected() {
        let now = fixture_time();
        let (plan, result) = fixture(now);
        let existing =
            build_request(&plan, &result, &input(&result), now).expect("existing request");
        let candidate =
            build_request(&plan, &result, &input(&result), now).expect("candidate request");
        ensure_same_evidence(&existing, &candidate).expect("same simulation is idempotent");
        let duplicate = outcome(existing.clone(), false);
        assert!(!duplicate.created);
        assert_eq!(duplicate.request_id, existing.id);

        let mut conflicting = candidate;
        conflicting.max_priority_fee_per_gas += 1;
        assert_eq!(
            ensure_same_evidence(&existing, &conflicting),
            Err(ApprovalError::DuplicateConflict)
        );
    }

    fn fixture(now: DateTime<Utc>) -> (UnsignedTransactionPlan, CounterfactualResult) {
        let mut plan = UnsignedTransactionPlan {
            schema_version: PLAN_SCHEMA_VERSION.to_string(),
            shadow_decision_id: "11111111-1111-8111-8111-111111111111".to_string(),
            source_event_identity: "sequence:123".to_string(),
            chain_id: ARBITRUM_ONE_CHAIN_ID,
            route: RoutePlan {
                route_id: "fixture-route".to_string(),
                route_fingerprint: CURRENT_ROUTE_FINGERPRINT.to_string(),
                pool_ids: vec![
                    CURRENT_ROUTE_POOL_500_ID.to_string(),
                    CURRENT_ROUTE_POOL_3000_ID.to_string(),
                ],
                pool_addresses: vec![
                    CURRENT_ROUTE_POOL_500_ADDRESS.to_string(),
                    CURRENT_ROUTE_POOL_3000_ADDRESS.to_string(),
                ],
                protocols: vec!["UniswapV3".to_string(), "UniswapV3".to_string()],
                directions: vec!["zero_for_one".to_string(), "one_for_zero".to_string()],
                fees: vec![500, 3_000],
            },
            token_path: vec![WETH.to_string(), USDC.to_string(), WETH.to_string()],
            origin_router: "0x4444444444444444444444444444444444444444".to_string(),
            input_amount: "1000000000000000".to_string(),
            maximum_input_amount: "1000000000000000".to_string(),
            expected_output: "1000000500000000".to_string(),
            expected_leg_outputs: vec!["900000000".to_string(), "1000000500000000".to_string()],
            minimum_output: "1000000460000000".to_string(),
            minimum_leg_outputs: vec!["890000000".to_string(), "1000000460000000".to_string()],
            minimum_profit: "460000000".to_string(),
            calldata: String::new(),
            calldata_hash: String::new(),
            value: "0".to_string(),
            gas_estimate: 400_000,
            gas_price_wei: "900".to_string(),
            deadline: u64::try_from((now + Duration::minutes(5)).timestamp()).expect("deadline"),
            target_contract: EXECUTOR.to_string(),
            target_code_hash: "a".repeat(64),
            simulation_from: "0x7777777777777777777777777777777777777777".to_string(),
            pinned_block: PinnedBlockEvidence {
                number: 123_456,
                hash: format!("0x{}", "d".repeat(64)),
            },
            route_hash: hex::encode([4_u8; 32]),
            primary_state_hash: "e".repeat(64),
            pool_state_hash_path: vec!["1".repeat(64), "2".repeat(64)],
            verification: VerificationEvidence {
                verification_status: "agreed".to_string(),
                independent_verification_status: "agreed".to_string(),
                agreement_state: "agreed".to_string(),
                primary_provider_id: "primary-rpc".to_string(),
                secondary_provider_id: "secondary-rpc".to_string(),
            },
            predicted: PredictedEconomics {
                gross_profit: "500000000".to_string(),
                total_cost: "360000000".to_string(),
                net_pnl: "140000000".to_string(),
                minimum_required_net_pnl: "100000000".to_string(),
            },
            model_version: "shadow-profitability-scale-v1".to_string(),
            policy_version: "fork-policy-v1".to_string(),
            unsigned: true,
            fork_only: true,
            shadow_only: true,
            live_execution: false,
            execution_eligible: false,
            execution_request_created: false,
            public_broadcast: false,
            signer_used: false,
        };
        let request = encoded_request(now, &plan);
        let calldata =
            encode_execute_opportunity(&request, request.executor_address).expect("calldata");
        plan.calldata_hash = hex::encode(Sha256::digest(&calldata));
        plan.calldata = format!("0x{}", hex::encode(calldata));
        let result = result_for_plan(&plan, now, "230000000");
        (plan, result)
    }

    fn encoded_request(now: DateTime<Utc>, plan: &UnsignedTransactionPlan) -> ExecutionRequest {
        let weth = CanonicalAddress::parse(WETH).expect("WETH");
        let usdc = CanonicalAddress::parse(USDC).expect("USDC");
        ExecutionRequest {
            id: Uuid::from_u128(1),
            opportunity_id: Uuid::from_u128(2),
            schema_version: REQUEST_SCHEMA_VERSION.to_string(),
            chain_id: ARBITRUM_ONE_CHAIN_ID,
            route_id: [4_u8; 32],
            route_fingerprint: plan.route.route_fingerprint.clone(),
            selected_size: 1_000_000_000_000_000,
            token_path: vec![weth, usdc, weth],
            origin_router: CanonicalAddress::parse(&plan.origin_router).expect("router"),
            executor_address: CanonicalAddress::parse(EXECUTOR).expect("executor"),
            executor_code_hash: plan.target_code_hash.clone(),
            calldata_hash: String::new(),
            simulation_result_hash: "b".repeat(64),
            plan_hash: "c".repeat(64),
            pinned_block_number: plan.pinned_block.number,
            pinned_block_hash: plan.pinned_block.hash.clone(),
            flash_asset: weth,
            flash_amount: 1_000_000_000_000_000,
            maximum_input_amount: 1_000_000_000_000_000,
            minimum_profit: 460_000_000,
            expected_profit: 500_000_000,
            deadline: DateTime::from_timestamp(i64::try_from(plan.deadline).expect("deadline"), 0)
                .expect("deadline"),
            legs: vec![
                ValidatedLeg {
                    pool: CanonicalAddress::parse(&plan.route.pool_addresses[0]).expect("pool"),
                    token_in: weth,
                    token_out: usdc,
                    fee: 500,
                    zero_for_one: true,
                    min_amount_out: 890_000_000,
                },
                ValidatedLeg {
                    pool: CanonicalAddress::parse(&plan.route.pool_addresses[1]).expect("pool"),
                    token_in: usdc,
                    token_out: weth,
                    fee: 3_000,
                    zero_for_one: false,
                    min_amount_out: 1_000_000_460_000_000,
                },
            ],
            gas_limit: 400_000,
            max_fee_per_gas: 900,
            max_priority_fee_per_gas: 90,
            approved_by: "fixture".to_string(),
            approved_at: now,
            approval_deadline: now + Duration::minutes(1),
            policy_version: APPROVAL_POLICY_VERSION.to_string(),
            approval_digest: String::new(),
        }
    }

    fn result_for_plan(
        plan: &UnsignedTransactionPlan,
        now: DateTime<Utc>,
        simulated_net_pnl: &str,
    ) -> CounterfactualResult {
        let simulated_net = simulated_net_pnl.parse::<u128>().expect("simulated net");
        let simulated_gross = 270_000_000_u128
            .checked_add(simulated_net)
            .expect("simulated gross");
        CounterfactualResult::from_body(CounterfactualResultBody {
            schema_version: RESULT_SCHEMA_VERSION.to_string(),
            plan_hash: plan.canonical_hash().expect("plan hash"),
            shadow_decision_id: plan.shadow_decision_id.clone(),
            status: SimulationStatus::Passed,
            predicted_gross_profit: plan.predicted.gross_profit.clone(),
            predicted_total_cost: plan.predicted.total_cost.clone(),
            predicted_net_pnl: plan.predicted.net_pnl.clone(),
            simulated_gross_profit: Some(simulated_gross.to_string()),
            simulated_gas_cost: Some("270000000".to_string()),
            simulated_balance_delta: Some(simulated_gross.to_string()),
            simulated_net_pnl: Some(simulated_net_pnl.to_string()),
            prediction_error: Some(
                simulated_net_pnl
                    .parse::<i128>()
                    .expect("simulated net")
                    .checked_sub(140_000_000)
                    .expect("prediction error")
                    .to_string(),
            ),
            gas_estimate: Some(400_000),
            gas_used: Some(300_000),
            model_version: plan.model_version.clone(),
            policy_version: plan.policy_version.clone(),
            fork: ForkIdentity {
                chain_id: ARBITRUM_ONE_CHAIN_ID,
                fork_block: plan.pinned_block.clone(),
                fork_instance_hash: "f".repeat(64),
                local_block: PinnedBlockEvidence {
                    number: plan.pinned_block.number + 1,
                    hash: format!("0x{}", "9".repeat(64)),
                },
            },
            simulated_at: now,
            revert_reason: None,
            evidence: SimulationEvidence {
                rpc_methods: vec!["eth_call".to_string()],
                target_code_hash: plan.target_code_hash.clone(),
                observed_pool_state_hashes: plan.pool_state_hash_path.clone(),
                observed_aggregate_state_hash: plan.primary_state_hash.clone(),
                call_output_hash: Some("7".repeat(64)),
                trace_hash: Some("8".repeat(64)),
                settled_route_hash: Some(plan.route_hash.clone()),
            },
            fork_only: true,
            shadow_only: true,
            live_execution: false,
            execution_eligible: false,
            execution_request_created: false,
            public_broadcast: false,
            signer_used: false,
        })
        .expect("result")
    }

    fn input(result: &CounterfactualResult) -> ApprovalInput {
        ApprovalInput {
            simulation_result_hash: result.result_hash.clone(),
            approved_by: "canary-reviewer".to_string(),
            approval_ttl_seconds: 300,
            max_priority_fee_per_gas: 90,
        }
    }

    fn fixture_time() -> DateTime<Utc> {
        Utc.with_ymd_and_hms(2026, 7, 20, 12, 0, 0)
            .single()
            .expect("fixture time")
    }
}
