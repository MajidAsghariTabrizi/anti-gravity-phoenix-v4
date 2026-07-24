use crate::approval::insert_approved_request;
use crate::config::ExecutorConfig;
use crate::model::{canonical_digest, CanonicalAddress, ExecutionRequest, ValidatedLeg};
use crate::rpc::{HttpExecutionRpc, RpcErrorKind, TransactionQuote};
use crate::{APPROVAL_POLICY_VERSION, REQUEST_SCHEMA_VERSION};
use chrono::{DateTime, SecondsFormat, TimeZone, Utc};
use serde_json::{json, Map, Value};
use sha2::{Digest, Sha256};
use sqlx::postgres::PgPoolOptions;
use sqlx::types::Json;
use sqlx::{PgPool, Postgres, Row, Transaction};
use std::collections::BTreeMap;
use std::time::Duration;
use thiserror::Error;
use uuid::Uuid;

const SCHEMA_VERSION: &str = "phoenix.live-canary-schema.v4";
const QUOTE_SCHEMA: &str = "phoenix.submission-quote.v1";
const RISK_SCHEMA: &str = "phoenix.risk-snapshot.v1";
const APPROVAL_SCHEMA: &str = "phoenix.automatic-approval.v1";
const ROUTE_POLICY: &str = include_str!("../../config/phoenix-route-policy-v1.json");
const MAX_CONSECUTIVE_OUTCOMES: i64 = 1_000;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MaterializationState {
    Idle,
    Materialized {
        candidate_id: Uuid,
        request_id: Uuid,
    },
    Rejected {
        candidate_id: Uuid,
        reason: &'static str,
    },
}

#[derive(Clone)]
pub struct AutonomousMaterializer {
    pool: PgPool,
    config: ExecutorConfig,
    rpc: HttpExecutionRpc,
    route_policy: Value,
}

impl AutonomousMaterializer {
    pub async fn connect(
        config: ExecutorConfig,
        rpc: HttpExecutionRpc,
    ) -> Result<Self, AutonomousMaterializerError> {
        let pool = PgPoolOptions::new()
            .max_connections(4)
            .acquire_timeout(Duration::from_secs(5))
            .connect(&config.postgres_dsn)
            .await
            .map_err(database_error)?;
        let version: String = sqlx::query_scalar(
            "SELECT version FROM live_canary.schema_contract WHERE version = $1",
        )
        .bind(SCHEMA_VERSION)
        .fetch_one(&pool)
        .await
        .map_err(database_error)?;
        if version != SCHEMA_VERSION {
            return Err(AutonomousMaterializerError::Integrity);
        }
        let route_policy: Value = serde_json::from_str(ROUTE_POLICY)
            .map_err(|_| AutonomousMaterializerError::Integrity)?;
        verify_hash(
            &route_policy,
            "policy_hash",
            "route-policy",
            "phoenix.route-policy.v1",
        )?;
        if route_policy
            .get("enabled_for_autonomous_live")
            .and_then(Value::as_bool)
            != Some(true)
        {
            return Err(AutonomousMaterializerError::Configuration);
        }
        Ok(Self {
            pool,
            config,
            rpc,
            route_policy,
        })
    }

    pub async fn step(
        &self,
        now: DateTime<Utc>,
    ) -> Result<MaterializationState, AutonomousMaterializerError> {
        let now = canonical_time(now)?;
        self.expire_candidates(now).await?;
        let Some(candidate) = self.claim_candidate(now).await? else {
            return Ok(MaterializationState::Idle);
        };
        let quote = match self
            .rpc
            .quote_transaction(
                self.config.wallet_address,
                self.config.executor_address,
                &candidate.calldata,
            )
            .await
        {
            Ok(quote) => quote,
            Err(error)
                if matches!(
                    error.kind,
                    RpcErrorKind::Transport | RpcErrorKind::Timeout | RpcErrorKind::RemoteFailure
                ) =>
            {
                return Err(AutonomousMaterializerError::Dependency);
            }
            Err(_) => return Err(AutonomousMaterializerError::Integrity),
        };
        self.finalize_candidate(candidate, quote, now).await
    }

    async fn expire_candidates(
        &self,
        now: DateTime<Utc>,
    ) -> Result<(), AutonomousMaterializerError> {
        sqlx::query(
            "UPDATE live_canary.autonomous_candidates
             SET status = 'expired', updated_at = $1
             WHERE status IN ('materialized', 'approval_pending', 'approved')
               AND candidate_expires_at <= $1",
        )
        .bind(now)
        .execute(&self.pool)
        .await
        .map_err(database_error)?;
        Ok(())
    }

    async fn claim_candidate(
        &self,
        now: DateTime<Utc>,
    ) -> Result<Option<AutonomousCandidate>, AutonomousMaterializerError> {
        let mut transaction = self.pool.begin().await.map_err(database_error)?;
        let row = sqlx::query(
            "SELECT candidate_id
             FROM live_canary.autonomous_candidates
             WHERE status IN ('materialized', 'approval_pending')
               AND candidate_expires_at > $1
             ORDER BY candidate_created_at, candidate_id
             FOR UPDATE SKIP LOCKED
             LIMIT 1",
        )
        .bind(now)
        .fetch_optional(&mut *transaction)
        .await
        .map_err(database_error)?;
        let Some(row) = row else {
            transaction.commit().await.map_err(database_error)?;
            return Ok(None);
        };
        let candidate_id: Uuid = row.try_get("candidate_id").map_err(database_error)?;
        sqlx::query(
            "UPDATE live_canary.autonomous_candidates
             SET status = 'approval_pending', updated_at = $2
             WHERE candidate_id = $1
               AND status IN ('materialized', 'approval_pending')",
        )
        .bind(candidate_id)
        .bind(now)
        .execute(&mut *transaction)
        .await
        .map_err(database_error)?;
        transaction.commit().await.map_err(database_error)?;
        self.load_candidate(candidate_id).await.map(Some)
    }

    async fn load_candidate(
        &self,
        candidate_id: Uuid,
    ) -> Result<AutonomousCandidate, AutonomousMaterializerError> {
        let row = sqlx::query(
            "SELECT candidate_id, opportunity_id, origin_event_id, chain_id,
                    route_fingerprint, route_universe_hash, route_policy_hash,
                    state_block_number::text AS state_block_number,
                    state_block_hash, state_hash, selected_size::text AS selected_size,
                    predicted_gross_profit::text AS predicted_gross_profit,
                    plan_hash, calldata_hash, executor_address, executor_code_hash,
                    candidate_hash, candidate_created_at, candidate_expires_at,
                    candidate_contract, plan_contract, calldata_hex, state_contract
             FROM live_canary.autonomous_candidates
             WHERE candidate_id = $1",
        )
        .bind(candidate_id)
        .fetch_one(&self.pool)
        .await
        .map_err(database_error)?;
        decode_candidate(&row)
    }

    async fn finalize_candidate(
        &self,
        candidate: AutonomousCandidate,
        quote: TransactionQuote,
        now: DateTime<Utc>,
    ) -> Result<MaterializationState, AutonomousMaterializerError> {
        if candidate.chain_id != self.config.chain_id
            || candidate.executor_address != self.config.executor_address
            || candidate.executor_code_hash != self.config.executor_code_hash
            || candidate.route_policy_hash != policy_text(&self.route_policy, "policy_hash")?
            || candidate.route_fingerprint != policy_text(&self.route_policy, "route_fingerprint")?
            || candidate.expires_at <= now
        {
            return self
                .reject(candidate.candidate_id, "rejected_policy", "identity_policy")
                .await;
        }
        let maximum_state_age = policy_u64(&self.route_policy, "maximum_state_age_blocks")?;
        if quote.block_number < candidate.state_block_number
            || quote.block_number - candidate.state_block_number > maximum_state_age
        {
            return self
                .reject(candidate.candidate_id, "rejected_state", "state_stale")
                .await;
        }
        if quote.gas_limit > self.config.limits.maximum_gas_limit
            || quote.max_fee_per_gas > self.config.limits.maximum_max_fee_per_gas
            || quote.max_priority_fee_per_gas > self.config.limits.maximum_priority_fee_per_gas
        {
            return self
                .reject(candidate.candidate_id, "rejected_policy", "gas_cap")
                .await;
        }

        let economics = candidate
            .plan
            .get("economics")
            .and_then(Value::as_object)
            .ok_or(AutonomousMaterializerError::Integrity)?;
        let flash_premium = object_u128(economics, "flash_premium")?;
        let model_error_reserve = object_u128(economics, "model_error_reserve")?;
        let total_fee = u128::from(quote.gas_limit)
            .checked_mul(quote.max_fee_per_gas)
            .ok_or(AutonomousMaterializerError::Arithmetic)?;
        if total_fee > policy_u128(&self.route_policy, "per_transaction_maximum_loss")? {
            return self
                .reject(
                    candidate.candidate_id,
                    "rejected_policy",
                    "per_transaction_loss_cap",
                )
                .await;
        }
        let estimated_l1_cost = quote.estimated_l1_cost.min(total_fee);
        let estimated_gas_cost = total_fee
            .checked_sub(estimated_l1_cost)
            .ok_or(AutonomousMaterializerError::Arithmetic)?;
        let failure_reserve = total_fee / 10;
        let ordering_cost = 0_u128;
        let total_cost = flash_premium
            .checked_add(estimated_gas_cost)
            .and_then(|value| value.checked_add(estimated_l1_cost))
            .and_then(|value| value.checked_add(ordering_cost))
            .and_then(|value| value.checked_add(failure_reserve))
            .and_then(|value| value.checked_add(model_error_reserve))
            .ok_or(AutonomousMaterializerError::Arithmetic)?;
        let gross = i128::try_from(candidate.predicted_gross_profit)
            .map_err(|_| AutonomousMaterializerError::Arithmetic)?;
        let executable_net = gross
            .checked_sub(
                i128::try_from(total_cost).map_err(|_| AutonomousMaterializerError::Arithmetic)?,
            )
            .ok_or(AutonomousMaterializerError::Arithmetic)?;
        let minimum_retained_profit = policy_u128(&self.route_policy, "minimum_retained_profit")?;
        if executable_net
            <= i128::try_from(minimum_retained_profit)
                .map_err(|_| AutonomousMaterializerError::Arithmetic)?
        {
            return self
                .reject(
                    candidate.candidate_id,
                    "rejected_economics",
                    "executable_net_below_floor",
                )
                .await;
        }

        let maximum_quote_age_ms = policy_u64(&self.route_policy, "maximum_quote_age_ms")?;
        let quote_expires = now
            .checked_add_signed(chrono::Duration::milliseconds(
                i64::try_from(maximum_quote_age_ms)
                    .map_err(|_| AutonomousMaterializerError::Arithmetic)?,
            ))
            .ok_or(AutonomousMaterializerError::Arithmetic)?
            .min(candidate.expires_at);
        if quote_expires <= now {
            return self
                .reject(candidate.candidate_id, "expired", "quote_expired")
                .await;
        }
        let tick_crossing_gas_increment =
            tick_crossing_gas_increment(&candidate.plan, quote.max_fee_per_gas)?;
        let mut submission_quote = json!({
            "schema_version": QUOTE_SCHEMA,
            "candidate_hash": candidate.candidate_hash,
            "route_policy_hash": candidate.route_policy_hash,
            "logical_channel_id": "standard_rpc",
            "rpc_endpoint_identity": quote.endpoint_identity,
            "gas_limit": quote.gas_limit,
            "max_fee_per_gas": quote.max_fee_per_gas.to_string(),
            "max_priority_fee_per_gas": quote.max_priority_fee_per_gas.to_string(),
            "estimated_gas_cost": estimated_gas_cost.to_string(),
            "estimated_l1_cost": estimated_l1_cost.to_string(),
            "flash_premium": flash_premium.to_string(),
            "tick_crossing_gas_increment": tick_crossing_gas_increment.to_string(),
            "failure_reserve": failure_reserve.to_string(),
            "model_error_reserve": model_error_reserve.to_string(),
            "quote_block_number": quote.block_number,
            "quote_block_hash": quote.block_hash,
            "quote_created_at": timestamp_string(now),
            "quote_expires_at": timestamp_string(quote_expires),
            "maximum_ordering_payment": "0",
            "estimated_ordering_payment": "0",
            "minimum_retained_profit": minimum_retained_profit.to_string(),
            "expected_net_after_ordering": executable_net.to_string(),
            "fallback_allowed": false,
            "fallback_channel_id": Value::Null,
            "quote_evidence_hash": "0".repeat(64)
        });
        set_hash(
            &mut submission_quote,
            "quote_evidence_hash",
            "submission-quote",
            QUOTE_SCHEMA,
        )?;
        let quote_hash = policy_text(&submission_quote, "quote_evidence_hash")?.to_string();

        let mut transaction = self.pool.begin().await.map_err(database_error)?;
        let locked = lock_candidate(&mut transaction, candidate.candidate_id).await?;
        if locked != "approval_pending" {
            transaction.rollback().await.map_err(database_error)?;
            return Ok(MaterializationState::Idle);
        }
        let controls = load_controls(
            &mut transaction,
            &candidate.route_fingerprint,
            &candidate.route_policy_hash,
        )
        .await?;
        let risk_facts =
            load_risk_facts(&mut transaction, &candidate.route_fingerprint, now).await?;
        let route_loss_limit = policy_u128(&self.route_policy, "per_route_daily_loss")?;
        let consecutive_limit = policy_u64(&self.route_policy, "maximum_consecutive_losses")?;
        if risk_facts.route_daily_loss >= route_loss_limit
            || u64::from(risk_facts.consecutive_losses) >= consecutive_limit
        {
            sqlx::query(
                "UPDATE live_canary.autonomous_route_controls
                 SET enabled = false, kill_switch = true,
                     disarm_reason = $2, control_hash = NULL,
                     control_contract = NULL, updated_at = $3
                 WHERE route_fingerprint = $1",
            )
            .bind(&candidate.route_fingerprint)
            .bind(if risk_facts.route_daily_loss >= route_loss_limit {
                "route_daily_loss_budget"
            } else {
                "maximum_consecutive_losses"
            })
            .bind(now)
            .execute(&mut *transaction)
            .await
            .map_err(database_error)?;
            transaction.commit().await.map_err(database_error)?;
            return self
                .reject(candidate.candidate_id, "rejected_policy", "route_disarmed")
                .await;
        }
        if let Err(error) = validate_controls(
            &controls,
            &risk_facts,
            &candidate,
            &self.config,
            &self.route_policy,
            quote.block_number,
            now,
        ) {
            transaction.rollback().await.map_err(database_error)?;
            return if error == AutonomousMaterializerError::Policy {
                self.reject(candidate.candidate_id, "rejected_policy", "risk_policy")
                    .await
            } else {
                Err(error)
            };
        }
        let mut risk_snapshot = build_risk_snapshot(
            &controls,
            &risk_facts,
            &candidate,
            &self.config,
            &submission_quote,
            quote.block_number,
            now,
        )?;
        set_hash(
            &mut risk_snapshot,
            "risk_snapshot_hash",
            "risk-snapshot",
            RISK_SCHEMA,
        )?;
        let risk_hash = policy_text(&risk_snapshot, "risk_snapshot_hash")?.to_string();
        let approval_expires = quote_expires.min(candidate.expires_at);
        let mut approval = json!({
            "schema_version": APPROVAL_SCHEMA,
            "candidate_id": candidate.candidate_id,
            "candidate_hash": candidate.candidate_hash,
            "route_policy_hash": candidate.route_policy_hash,
            "route_universe_hash": candidate.route_universe_hash,
            "risk_snapshot_hash": risk_hash,
            "submission_quote_hash": quote_hash,
            "state_hash": candidate.state_hash,
            "plan_hash": candidate.plan_hash,
            "simulation_result_hash": candidate.state_hash,
            "calldata_hash": candidate.calldata_hash,
            "executor_address": candidate.executor_address.to_string(),
            "executor_code_hash": candidate.executor_code_hash,
            "approval_source": "autonomous_policy",
            "approval_created_at": timestamp_string(now),
            "approval_expires_at": timestamp_string(approval_expires),
            "automatic_approval_digest": "0".repeat(64)
        });
        set_hash(
            &mut approval,
            "automatic_approval_digest",
            "automatic-approval",
            APPROVAL_SCHEMA,
        )?;
        let automatic_digest = policy_text(&approval, "automatic_approval_digest")?.to_string();
        let request = build_execution_request(
            &candidate,
            &submission_quote,
            executable_net,
            minimum_retained_profit,
            quote,
            now,
            approval_expires,
        )?;

        insert_approval(
            &mut transaction,
            &candidate,
            &risk_snapshot,
            &submission_quote,
            &approval,
        )
        .await?;
        sqlx::query(
            "UPDATE live_canary.autonomous_candidates
             SET status = 'approved', risk_snapshot_hash = $2,
                 risk_snapshot_contract = $3, submission_quote_hash = $4,
                 submission_quote_contract = $5, approval_deadline = $6,
                 updated_at = $7
             WHERE candidate_id = $1 AND status = 'approval_pending'",
        )
        .bind(candidate.candidate_id)
        .bind(&risk_hash)
        .bind(Json(&risk_snapshot))
        .bind(&quote_hash)
        .bind(Json(&submission_quote))
        .bind(approval_expires)
        .bind(now)
        .execute(&mut *transaction)
        .await
        .map_err(database_error)?;
        insert_approved_request(&mut transaction, &request)
            .await
            .map_err(|_| AutonomousMaterializerError::Integrity)?;
        sqlx::query(
            "UPDATE live_canary.execution_requests
             SET candidate_id = $2, candidate_hash = $3,
                 automatic_approval_digest = $4, state_hash = $5,
                 submission_quote_contract = $6
             WHERE id = $1",
        )
        .bind(request.id)
        .bind(candidate.candidate_id)
        .bind(&candidate.candidate_hash)
        .bind(&automatic_digest)
        .bind(&candidate.state_hash)
        .bind(Json(&submission_quote))
        .execute(&mut *transaction)
        .await
        .map_err(database_error)?;
        sqlx::query(
            "UPDATE live_canary.autonomous_candidates
             SET status = 'request_materialized', execution_request_id = $2,
                 updated_at = $3
             WHERE candidate_id = $1 AND status = 'approved'",
        )
        .bind(candidate.candidate_id)
        .bind(request.id)
        .bind(now)
        .execute(&mut *transaction)
        .await
        .map_err(database_error)?;
        transaction.commit().await.map_err(database_error)?;
        Ok(MaterializationState::Materialized {
            candidate_id: candidate.candidate_id,
            request_id: request.id,
        })
    }

    async fn reject(
        &self,
        candidate_id: Uuid,
        status: &'static str,
        _reason: &'static str,
    ) -> Result<MaterializationState, AutonomousMaterializerError> {
        let result = sqlx::query(
            "UPDATE live_canary.autonomous_candidates
             SET status = $2, updated_at = now()
             WHERE candidate_id = $1
               AND status IN ('materialized', 'approval_pending')",
        )
        .bind(candidate_id)
        .bind(status)
        .execute(&self.pool)
        .await
        .map_err(database_error)?;
        if result.rows_affected() > 1 {
            return Err(AutonomousMaterializerError::Integrity);
        }
        Ok(MaterializationState::Rejected {
            candidate_id,
            reason: status,
        })
    }
}

#[derive(Clone, Debug)]
struct AutonomousCandidate {
    candidate_id: Uuid,
    opportunity_id: Uuid,
    origin_event_id: String,
    chain_id: u64,
    route_fingerprint: String,
    route_universe_hash: String,
    route_policy_hash: String,
    state_block_number: u64,
    state_block_hash: String,
    state_hash: String,
    selected_size: u128,
    predicted_gross_profit: u128,
    plan_hash: String,
    calldata_hash: String,
    executor_address: CanonicalAddress,
    executor_code_hash: String,
    candidate_hash: String,
    created_at: DateTime<Utc>,
    expires_at: DateTime<Utc>,
    contract: Value,
    plan: Value,
    calldata: Vec<u8>,
    state_contract: Value,
}

#[derive(Clone, Debug)]
struct Controls {
    global: Value,
    route: Value,
    global_maximum_input: u128,
    global_daily_loss_limit: u128,
    route_maximum_input: u128,
}

#[derive(Clone, Debug)]
struct RiskFacts {
    global_daily_loss: u128,
    route_daily_loss: u128,
    consecutive_losses: u32,
    active_attempts: u64,
    next_nonce: Option<u64>,
}

fn decode_candidate(
    row: &sqlx::postgres::PgRow,
) -> Result<AutonomousCandidate, AutonomousMaterializerError> {
    let contract: Json<Value> = row.try_get("candidate_contract").map_err(database_error)?;
    let plan: Json<Value> = row.try_get("plan_contract").map_err(database_error)?;
    let state_contract: Json<Value> = row.try_get("state_contract").map_err(database_error)?;
    let calldata_hex: String = row.try_get("calldata_hex").map_err(database_error)?;
    let calldata = calldata_hex
        .strip_prefix("0x")
        .and_then(|value| hex::decode(value).ok())
        .filter(|value| !value.is_empty() && value.len() <= 64 * 1024)
        .ok_or(AutonomousMaterializerError::Integrity)?;
    let candidate = AutonomousCandidate {
        candidate_id: row.try_get("candidate_id").map_err(database_error)?,
        opportunity_id: row.try_get("opportunity_id").map_err(database_error)?,
        origin_event_id: row.try_get("origin_event_id").map_err(database_error)?,
        chain_id: i64_to_u64(row.try_get("chain_id").map_err(database_error)?)?,
        route_fingerprint: row.try_get("route_fingerprint").map_err(database_error)?,
        route_universe_hash: row.try_get("route_universe_hash").map_err(database_error)?,
        route_policy_hash: row.try_get("route_policy_hash").map_err(database_error)?,
        state_block_number: decimal_u64(
            &row.try_get::<String, _>("state_block_number")
                .map_err(database_error)?,
        )?,
        state_block_hash: row.try_get("state_block_hash").map_err(database_error)?,
        state_hash: row.try_get("state_hash").map_err(database_error)?,
        selected_size: decimal_u128(
            &row.try_get::<String, _>("selected_size")
                .map_err(database_error)?,
        )?,
        predicted_gross_profit: decimal_u128(
            &row.try_get::<String, _>("predicted_gross_profit")
                .map_err(database_error)?,
        )?,
        plan_hash: row.try_get("plan_hash").map_err(database_error)?,
        calldata_hash: row.try_get("calldata_hash").map_err(database_error)?,
        executor_address: CanonicalAddress::parse(
            &row.try_get::<String, _>("executor_address")
                .map_err(database_error)?,
        )
        .map_err(|_| AutonomousMaterializerError::Integrity)?,
        executor_code_hash: row.try_get("executor_code_hash").map_err(database_error)?,
        candidate_hash: row.try_get("candidate_hash").map_err(database_error)?,
        created_at: row
            .try_get("candidate_created_at")
            .map_err(database_error)?,
        expires_at: row
            .try_get("candidate_expires_at")
            .map_err(database_error)?,
        contract: contract.0,
        plan: plan.0,
        calldata,
        state_contract: state_contract.0,
    };
    validate_candidate(&candidate)?;
    Ok(candidate)
}

fn validate_candidate(candidate: &AutonomousCandidate) -> Result<(), AutonomousMaterializerError> {
    verify_hash(
        &candidate.contract,
        "candidate_hash",
        "autonomous-candidate",
        "phoenix.autonomous-candidate.v1",
    )?;
    let expected_plan_hash = digest_value("phoenix.hunter-live-plan.v1", &candidate.plan)?;
    let executor_address = candidate.executor_address.to_string();
    let selected_size = candidate.selected_size.to_string();
    validate_state_contract(candidate)?;
    if candidate.chain_id != 42_161
        || candidate.origin_event_id.is_empty()
        || !canonical_digest(&candidate.candidate_hash)
        || !canonical_digest(&candidate.plan_hash)
        || !canonical_digest(&candidate.calldata_hash)
        || !canonical_digest(&candidate.state_hash)
        || hex::encode(Sha256::digest(&candidate.calldata)) != candidate.calldata_hash
        || candidate
            .contract
            .get("candidate_hash")
            .and_then(Value::as_str)
            != Some(candidate.candidate_hash.as_str())
        || candidate.contract.get("plan_hash").and_then(Value::as_str)
            != Some(candidate.plan_hash.as_str())
        || candidate.plan_hash != expected_plan_hash
        || candidate
            .contract
            .get("route_fingerprint")
            .and_then(Value::as_str)
            != Some(candidate.route_fingerprint.as_str())
        || candidate
            .contract
            .get("route_universe_hash")
            .and_then(Value::as_str)
            != Some(candidate.route_universe_hash.as_str())
        || candidate
            .contract
            .get("route_policy_hash")
            .and_then(Value::as_str)
            != Some(candidate.route_policy_hash.as_str())
        || candidate
            .contract
            .get("state_block_hash")
            .and_then(Value::as_str)
            != Some(candidate.state_block_hash.as_str())
        || candidate.contract.get("state_hash").and_then(Value::as_str)
            != Some(candidate.state_hash.as_str())
        || candidate.plan.get("state_hash").and_then(Value::as_str)
            != Some(candidate.state_hash.as_str())
        || candidate.plan.get("block_hash").and_then(Value::as_str)
            != Some(candidate.state_block_hash.as_str())
        || candidate
            .plan
            .get("route_policy_hash")
            .and_then(Value::as_str)
            != Some(candidate.route_policy_hash.as_str())
        || candidate
            .plan
            .get("route_universe_hash")
            .and_then(Value::as_str)
            != Some(candidate.route_universe_hash.as_str())
        || candidate
            .plan
            .get("executor_address")
            .and_then(Value::as_str)
            != Some(executor_address.as_str())
        || candidate
            .plan
            .get("executor_code_hash")
            .and_then(Value::as_str)
            != Some(candidate.executor_code_hash.as_str())
        || candidate.plan.get("selected_input").and_then(Value::as_str)
            != Some(selected_size.as_str())
        || candidate
            .plan
            .get("execution_eligible")
            .and_then(Value::as_bool)
            != Some(true)
        || candidate.plan.get("shadow_only").and_then(Value::as_bool) != Some(false)
        || candidate
            .state_contract
            .get("agreements")
            .and_then(Value::as_array)
            .is_none()
        || candidate.created_at >= candidate.expires_at
    {
        return Err(AutonomousMaterializerError::Integrity);
    }
    Ok(())
}

fn validate_state_contract(
    candidate: &AutonomousCandidate,
) -> Result<(), AutonomousMaterializerError> {
    if candidate
        .state_contract
        .get("schema_version")
        .and_then(Value::as_str)
        != Some("phoenix.rpc.hunter-state-response.v1")
        || candidate
            .state_contract
            .get("chain_id")
            .and_then(Value::as_u64)
            != Some(candidate.chain_id)
        || candidate
            .state_contract
            .get("block_number")
            .and_then(Value::as_u64)
            != Some(candidate.state_block_number)
        || candidate
            .state_contract
            .get("block_hash")
            .and_then(Value::as_str)
            != Some(candidate.state_block_hash.as_str())
    {
        return Err(AutonomousMaterializerError::Integrity);
    }
    let agreements = candidate
        .state_contract
        .get("agreements")
        .and_then(Value::as_array)
        .filter(|agreements| !agreements.is_empty() && agreements.len() <= 16)
        .ok_or(AutonomousMaterializerError::Integrity)?;
    for agreement in agreements {
        let primary_provider = agreement
            .get("primary_provider_id")
            .and_then(Value::as_str)
            .ok_or(AutonomousMaterializerError::Integrity)?;
        let secondary_provider = agreement
            .get("secondary_provider_id")
            .and_then(Value::as_str)
            .ok_or(AutonomousMaterializerError::Integrity)?;
        let primary = agreement
            .get("primary")
            .ok_or(AutonomousMaterializerError::Integrity)?;
        let secondary = agreement
            .get("secondary")
            .ok_or(AutonomousMaterializerError::Integrity)?;
        if primary_provider == secondary_provider
            || primary != secondary
            || primary.get("chain_id").and_then(Value::as_u64) != Some(candidate.chain_id)
            || primary.get("block_number").and_then(Value::as_u64)
                != Some(candidate.state_block_number)
            || primary.get("block_hash").and_then(Value::as_str)
                != Some(candidate.state_block_hash.as_str())
        {
            return Err(AutonomousMaterializerError::Integrity);
        }
        verify_hash(
            primary,
            "state_hash",
            "hunter-pinned-v3-state",
            "phoenix.hunter-pinned-v3-state.v1",
        )?;
    }
    Ok(())
}

async fn lock_candidate(
    transaction: &mut Transaction<'_, Postgres>,
    candidate_id: Uuid,
) -> Result<String, AutonomousMaterializerError> {
    sqlx::query_scalar(
        "SELECT status FROM live_canary.autonomous_candidates
         WHERE candidate_id = $1 FOR UPDATE",
    )
    .bind(candidate_id)
    .fetch_one(&mut **transaction)
    .await
    .map_err(database_error)
}

async fn load_controls(
    transaction: &mut Transaction<'_, Postgres>,
    route_fingerprint: &str,
    route_policy_hash: &str,
) -> Result<Controls, AutonomousMaterializerError> {
    let global = sqlx::query(
        "SELECT armed, kill_switch, execution_mode,
                maximum_input_amount::text AS maximum_input_amount,
                daily_loss_limit::text AS daily_loss_limit,
                control_hash, control_contract
         FROM live_canary.autonomous_global_control
         WHERE singleton FOR UPDATE",
    )
    .fetch_one(&mut **transaction)
    .await
    .map_err(database_error)?;
    let route = sqlx::query(
        "SELECT enabled, kill_switch, route_policy_hash,
                maximum_permitted_size::text AS maximum_permitted_size,
                cooldown_until, control_hash, control_contract
         FROM live_canary.autonomous_route_controls
         WHERE route_fingerprint = $1 FOR UPDATE",
    )
    .bind(route_fingerprint)
    .fetch_one(&mut **transaction)
    .await
    .map_err(database_error)?;
    if !global.try_get::<bool, _>("armed").map_err(database_error)?
        || global
            .try_get::<bool, _>("kill_switch")
            .map_err(database_error)?
        || global
            .try_get::<String, _>("execution_mode")
            .map_err(database_error)?
            != "live"
        || !route
            .try_get::<bool, _>("enabled")
            .map_err(database_error)?
        || route
            .try_get::<bool, _>("kill_switch")
            .map_err(database_error)?
        || route
            .try_get::<String, _>("route_policy_hash")
            .map_err(database_error)?
            != route_policy_hash
    {
        return Err(AutonomousMaterializerError::Policy);
    }
    let global_contract: Json<Value> =
        global.try_get("control_contract").map_err(database_error)?;
    let route_contract: Json<Value> = route.try_get("control_contract").map_err(database_error)?;
    verify_hash(
        &global_contract.0,
        "control_hash",
        "global-control",
        "phoenix.autonomous-global-control.v1",
    )?;
    verify_hash(
        &route_contract.0,
        "control_hash",
        "route-control",
        "phoenix.autonomous-route-control.v1",
    )?;
    if global
        .try_get::<String, _>("control_hash")
        .map_err(database_error)?
        != policy_text(&global_contract.0, "control_hash")?
        || route
            .try_get::<String, _>("control_hash")
            .map_err(database_error)?
            != policy_text(&route_contract.0, "control_hash")?
    {
        return Err(AutonomousMaterializerError::Integrity);
    }
    Ok(Controls {
        global: global_contract.0,
        route: route_contract.0,
        global_maximum_input: decimal_u128(
            &global
                .try_get::<String, _>("maximum_input_amount")
                .map_err(database_error)?,
        )?,
        global_daily_loss_limit: decimal_u128(
            &global
                .try_get::<String, _>("daily_loss_limit")
                .map_err(database_error)?,
        )?,
        route_maximum_input: decimal_u128(
            &route
                .try_get::<String, _>("maximum_permitted_size")
                .map_err(database_error)?,
        )?,
    })
}

async fn load_risk_facts(
    transaction: &mut Transaction<'_, Postgres>,
    route_fingerprint: &str,
    now: DateTime<Utc>,
) -> Result<RiskFacts, AutonomousMaterializerError> {
    let day = now.date_naive();
    let global_loss: String = sqlx::query_scalar(
        "SELECT COALESCE(SUM(CASE WHEN o.net_pnl_wei < 0 THEN -o.net_pnl_wei ELSE 0 END), 0)::text
         FROM live_canary.execution_outcomes o
         WHERE o.recorded_at >= $1::date
           AND o.recorded_at < ($1::date + INTERVAL '1 day')",
    )
    .bind(day)
    .fetch_one(&mut **transaction)
    .await
    .map_err(database_error)?;
    let route_loss: String = sqlx::query_scalar(
        "SELECT COALESCE(SUM(CASE WHEN o.net_pnl_wei < 0 THEN -o.net_pnl_wei ELSE 0 END), 0)::text
         FROM live_canary.execution_outcomes o
         JOIN live_canary.execution_requests r ON r.id = o.request_id
         WHERE r.route_fingerprint = $1
           AND o.recorded_at >= $2::date
           AND o.recorded_at < ($2::date + INTERVAL '1 day')",
    )
    .bind(route_fingerprint)
    .bind(day)
    .fetch_one(&mut **transaction)
    .await
    .map_err(database_error)?;
    let recent: Vec<String> = sqlx::query_scalar(
        "SELECT o.net_pnl_wei::text
         FROM live_canary.execution_outcomes o
         JOIN live_canary.execution_requests r ON r.id = o.request_id
         WHERE r.route_fingerprint = $1
         ORDER BY o.recorded_at DESC
         LIMIT $2",
    )
    .bind(route_fingerprint)
    .bind(MAX_CONSECUTIVE_OUTCOMES)
    .fetch_all(&mut **transaction)
    .await
    .map_err(database_error)?;
    let consecutive_losses = recent
        .iter()
        .take_while(|value| value.starts_with('-'))
        .count()
        .try_into()
        .map_err(|_| AutonomousMaterializerError::Arithmetic)?;
    let active_attempts: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM live_canary.execution_attempts
         WHERE status IN ('claimed', 'nonce_allocated', 'submission_unknown', 'pending', 'timed_out')",
    )
    .fetch_one(&mut **transaction)
    .await
    .map_err(database_error)?;
    let next_nonce: Option<String> = sqlx::query_scalar(
        "SELECT next_nonce::text FROM live_canary.nonce_state
         WHERE chain_id = 42161 LIMIT 1",
    )
    .fetch_optional(&mut **transaction)
    .await
    .map_err(database_error)?;
    Ok(RiskFacts {
        global_daily_loss: decimal_u128(&global_loss)?,
        route_daily_loss: decimal_u128(&route_loss)?,
        consecutive_losses,
        active_attempts: i64_to_u64(active_attempts)?,
        next_nonce: next_nonce.map(|value| decimal_u64(&value)).transpose()?,
    })
}

fn validate_controls(
    controls: &Controls,
    facts: &RiskFacts,
    candidate: &AutonomousCandidate,
    config: &ExecutorConfig,
    route_policy: &Value,
    quote_block: u64,
    now: DateTime<Utc>,
) -> Result<(), AutonomousMaterializerError> {
    let route_loss_limit = policy_u128(route_policy, "per_route_daily_loss")?;
    let maximum_consecutive = policy_u64(route_policy, "maximum_consecutive_losses")?;
    let cooldown = controls.route.get("cooldown_until");
    if candidate.selected_size > controls.global_maximum_input
        || candidate.selected_size > controls.route_maximum_input
        || candidate.selected_size > config.limits.maximum_input_amount
        || facts.global_daily_loss >= controls.global_daily_loss_limit
        || facts.route_daily_loss >= route_loss_limit
        || u64::from(facts.consecutive_losses) >= maximum_consecutive
        || facts.active_attempts != 0
        || quote_block < candidate.state_block_number
        || cooldown.is_some_and(|value| !value.is_null())
        || candidate.expires_at <= now
    {
        return Err(AutonomousMaterializerError::Policy);
    }
    Ok(())
}

fn build_risk_snapshot(
    controls: &Controls,
    facts: &RiskFacts,
    candidate: &AutonomousCandidate,
    config: &ExecutorConfig,
    quote: &Value,
    quote_block: u64,
    now: DateTime<Utc>,
) -> Result<Value, AutonomousMaterializerError> {
    let route_loss_limit = policy_u128(
        &serde_json::from_str(ROUTE_POLICY).map_err(|_| AutonomousMaterializerError::Integrity)?,
        "per_route_daily_loss",
    )?;
    Ok(json!({
        "schema_version": RISK_SCHEMA,
        "route_policy_hash": candidate.route_policy_hash,
        "candidate_hash": candidate.candidate_hash,
        "submission_quote_hash": policy_text(quote, "quote_evidence_hash")?,
        "wallet_address": config.wallet_address.to_string(),
        "executor_address": config.executor_address.to_string(),
        "executor_code_hash": config.executor_code_hash,
        "evaluated_at": timestamp_string(now),
        "global_control_state": controls.global,
        "route_control_state": controls.route,
        "daily_realized_loss": facts.global_daily_loss.to_string(),
        "route_daily_realized_loss": facts.route_daily_loss.to_string(),
        "consecutive_losses": facts.consecutive_losses,
        "active_execution_count": facts.active_attempts,
        "nonce_authority_state": {
            "chain_id": config.chain_id,
            "wallet_address": config.wallet_address.to_string(),
            "next_nonce": facts.next_nonce.map(|value| value.to_string())
        },
        "candidate_age_ms": now.signed_duration_since(candidate.created_at).num_milliseconds().max(0),
        "state_age_blocks": quote_block - candidate.state_block_number,
        "current_size_level": policy_text(&controls.route, "current_size_level")?,
        "maximum_permitted_size": controls.route_maximum_input.to_string(),
        "remaining_ordering_budget": "0",
        "remaining_daily_loss_budget": controls.global_daily_loss_limit.saturating_sub(facts.global_daily_loss).to_string(),
        "remaining_route_loss_budget": route_loss_limit.saturating_sub(facts.route_daily_loss).to_string(),
        "cooldown_until": controls.route.get("cooldown_until").cloned().unwrap_or(Value::Null),
        "downgrade_reason": Value::Null,
        "risk_snapshot_hash": "0".repeat(64)
    }))
}

fn build_execution_request(
    candidate: &AutonomousCandidate,
    _submission_quote: &Value,
    executable_net: i128,
    minimum_retained_profit: u128,
    quote: TransactionQuote,
    now: DateTime<Utc>,
    approval_expires: DateTime<Utc>,
) -> Result<ExecutionRequest, AutonomousMaterializerError> {
    let route = candidate
        .plan
        .get("route")
        .and_then(Value::as_object)
        .ok_or(AutonomousMaterializerError::Integrity)?;
    let route_hash = object_text(route, "semantic_hash")?;
    let route_id = decode_fixed::<32>(route_hash)?;
    let settlement_asset = CanonicalAddress::parse(object_text(route, "settlement_asset")?)
        .map_err(|_| AutonomousMaterializerError::Integrity)?;
    let route_legs = route
        .get("legs")
        .and_then(Value::as_array)
        .ok_or(AutonomousMaterializerError::Integrity)?;
    let simulations = candidate
        .plan
        .get("legs")
        .and_then(Value::as_array)
        .ok_or(AutonomousMaterializerError::Integrity)?;
    if route_legs.len() != simulations.len() || route_legs.is_empty() {
        return Err(AutonomousMaterializerError::Integrity);
    }
    let legs = route_legs
        .iter()
        .zip(simulations)
        .map(|(leg, simulation)| {
            let leg = leg
                .as_object()
                .ok_or(AutonomousMaterializerError::Integrity)?;
            let simulation = simulation
                .as_object()
                .ok_or(AutonomousMaterializerError::Integrity)?;
            Ok(ValidatedLeg {
                pool: CanonicalAddress::parse(object_text(leg, "pool_address")?)
                    .map_err(|_| AutonomousMaterializerError::Integrity)?,
                factory: Some(
                    CanonicalAddress::parse(object_text(leg, "factory_address")?)
                        .map_err(|_| AutonomousMaterializerError::Integrity)?,
                ),
                token_in: CanonicalAddress::parse(object_text(leg, "token_in")?)
                    .map_err(|_| AutonomousMaterializerError::Integrity)?,
                token_out: CanonicalAddress::parse(object_text(leg, "token_out")?)
                    .map_err(|_| AutonomousMaterializerError::Integrity)?,
                fee: object_u64(leg, "fee")?
                    .try_into()
                    .map_err(|_| AutonomousMaterializerError::Integrity)?,
                zero_for_one: object_text(leg, "direction")? == "zero_for_one",
                min_amount_out: object_u128(simulation, "minimum_output")?,
            })
        })
        .collect::<Result<Vec<_>, AutonomousMaterializerError>>()?;
    let token_path = std::iter::once(legs[0].token_in)
        .chain(legs.iter().map(|leg| leg.token_out))
        .collect::<Vec<_>>();
    let maximum_input = candidate
        .plan
        .get("maximum_input_amount")
        .and_then(Value::as_str)
        .ok_or(AutonomousMaterializerError::Integrity)
        .and_then(decimal_u128)?;
    let request_id = deterministic_uuid("request", &candidate.candidate_hash);
    let mut request = ExecutionRequest {
        id: request_id,
        opportunity_id: candidate.opportunity_id,
        schema_version: REQUEST_SCHEMA_VERSION.to_string(),
        chain_id: candidate.chain_id,
        route_id,
        route_fingerprint: candidate.route_fingerprint.clone(),
        selected_size: candidate.selected_size,
        token_path,
        origin_router: CanonicalAddress::parse(
            candidate
                .plan
                .get("origin_router")
                .and_then(Value::as_str)
                .ok_or(AutonomousMaterializerError::Integrity)?,
        )
        .map_err(|_| AutonomousMaterializerError::Integrity)?,
        executor_address: candidate.executor_address,
        executor_code_hash: candidate.executor_code_hash.clone(),
        calldata_hash: candidate.calldata_hash.clone(),
        simulation_result_hash: candidate.state_hash.clone(),
        plan_hash: candidate.plan_hash.clone(),
        pinned_block_number: candidate.state_block_number,
        pinned_block_hash: candidate.state_block_hash.clone(),
        flash_asset: settlement_asset,
        flash_amount: candidate.selected_size,
        maximum_input_amount: maximum_input,
        minimum_profit: minimum_retained_profit,
        expected_profit: u128::try_from(executable_net)
            .map_err(|_| AutonomousMaterializerError::Arithmetic)?,
        deadline: candidate.expires_at,
        legs,
        gas_limit: quote.gas_limit,
        max_fee_per_gas: quote.max_fee_per_gas,
        max_priority_fee_per_gas: quote.max_priority_fee_per_gas,
        approved_by: "autonomous_policy".to_string(),
        approved_at: now,
        approval_deadline: approval_expires,
        policy_version: APPROVAL_POLICY_VERSION.to_string(),
        approval_digest: "0".repeat(64),
    };
    request.approval_digest = request
        .canonical_approval_digest()
        .map_err(|_| AutonomousMaterializerError::Integrity)?;
    request
        .validate_current_route()
        .map_err(|_| AutonomousMaterializerError::Integrity)?;
    Ok(request)
}

async fn insert_approval(
    transaction: &mut Transaction<'_, Postgres>,
    candidate: &AutonomousCandidate,
    risk: &Value,
    quote: &Value,
    approval: &Value,
) -> Result<(), AutonomousMaterializerError> {
    sqlx::query(
        "INSERT INTO live_canary.autonomous_approvals(
            candidate_id, schema_version, candidate_hash, route_policy_hash,
            route_universe_hash, risk_snapshot_hash, submission_quote_hash,
            state_hash, plan_hash, simulation_result_hash, calldata_hash,
            executor_address, executor_code_hash, approval_source,
            approval_created_at, approval_expires_at,
            automatic_approval_digest, approval_contract
         ) VALUES (
            $1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11, $12, $13,
            'autonomous_policy', $14, $15, $16, $17
         )
         ON CONFLICT (candidate_id) DO NOTHING",
    )
    .bind(candidate.candidate_id)
    .bind(APPROVAL_SCHEMA)
    .bind(&candidate.candidate_hash)
    .bind(&candidate.route_policy_hash)
    .bind(&candidate.route_universe_hash)
    .bind(policy_text(risk, "risk_snapshot_hash")?)
    .bind(policy_text(quote, "quote_evidence_hash")?)
    .bind(&candidate.state_hash)
    .bind(&candidate.plan_hash)
    .bind(&candidate.state_hash)
    .bind(&candidate.calldata_hash)
    .bind(candidate.executor_address.to_string())
    .bind(&candidate.executor_code_hash)
    .bind(parse_timestamp(approval, "approval_created_at")?)
    .bind(parse_timestamp(approval, "approval_expires_at")?)
    .bind(policy_text(approval, "automatic_approval_digest")?)
    .bind(Json(approval))
    .execute(&mut **transaction)
    .await
    .map_err(database_error)?;
    Ok(())
}

fn tick_crossing_gas_increment(
    plan: &Value,
    max_fee_per_gas: u128,
) -> Result<u128, AutonomousMaterializerError> {
    let crossings = plan
        .get("legs")
        .and_then(Value::as_array)
        .ok_or(AutonomousMaterializerError::Integrity)?
        .iter()
        .map(|leg| {
            leg.get("ticks_crossed")
                .and_then(Value::as_u64)
                .ok_or(AutonomousMaterializerError::Integrity)
        })
        .try_fold(0_u64, |total, value| {
            total
                .checked_add(value?)
                .ok_or(AutonomousMaterializerError::Arithmetic)
        })?;
    u128::from(crossings)
        .checked_mul(15_000)
        .and_then(|value| value.checked_mul(max_fee_per_gas))
        .ok_or(AutonomousMaterializerError::Arithmetic)
}

fn verify_hash(
    value: &Value,
    field: &str,
    domain: &str,
    schema: &str,
) -> Result<(), AutonomousMaterializerError> {
    let actual = policy_text(value, field)?;
    if !canonical_digest(actual) || canonical_hash(value, field, domain, schema)? != actual {
        return Err(AutonomousMaterializerError::Integrity);
    }
    Ok(())
}

pub(crate) fn set_hash(
    value: &mut Value,
    field: &str,
    domain: &str,
    schema: &str,
) -> Result<(), AutonomousMaterializerError> {
    let hash = canonical_hash(value, field, domain, schema)?;
    value
        .as_object_mut()
        .ok_or(AutonomousMaterializerError::Integrity)?
        .insert(field.to_string(), Value::String(hash));
    Ok(())
}

fn canonical_hash(
    value: &Value,
    field: &str,
    domain: &str,
    schema: &str,
) -> Result<String, AutonomousMaterializerError> {
    let mut body = value.clone();
    body.as_object_mut()
        .ok_or(AutonomousMaterializerError::Integrity)?
        .remove(field)
        .ok_or(AutonomousMaterializerError::Integrity)?;
    let canonical = canonical_json(&body)?;
    let prefix = format!("phoenix.canonical-json.v1:{domain}:{schema}\n");
    Ok(hex::encode(Sha256::digest(
        [prefix.as_bytes(), canonical.as_slice()].concat(),
    )))
}

fn digest_value(domain: &str, value: &Value) -> Result<String, AutonomousMaterializerError> {
    let canonical = canonical_json(value)?;
    Ok(hex::encode(Sha256::digest(
        [domain.as_bytes(), b"\n", canonical.as_slice()].concat(),
    )))
}

fn canonical_json(value: &Value) -> Result<Vec<u8>, AutonomousMaterializerError> {
    match value {
        Value::Null | Value::Bool(_) | Value::String(_) | Value::Number(_) => {
            serde_json::to_vec(value).map_err(|_| AutonomousMaterializerError::Integrity)
        }
        Value::Array(values) => {
            let mut output = vec![b'['];
            for (index, child) in values.iter().enumerate() {
                if index > 0 {
                    output.push(b',');
                }
                output.extend(canonical_json(child)?);
            }
            output.push(b']');
            Ok(output)
        }
        Value::Object(values) => {
            let sorted = values.iter().collect::<BTreeMap<_, _>>();
            let mut output = vec![b'{'];
            for (index, (key, child)) in sorted.into_iter().enumerate() {
                if index > 0 {
                    output.push(b',');
                }
                output.extend(
                    serde_json::to_vec(key).map_err(|_| AutonomousMaterializerError::Integrity)?,
                );
                output.push(b':');
                output.extend(canonical_json(child)?);
            }
            output.push(b'}');
            Ok(output)
        }
    }
}

fn policy_text<'a>(value: &'a Value, field: &str) -> Result<&'a str, AutonomousMaterializerError> {
    value
        .get(field)
        .and_then(Value::as_str)
        .ok_or(AutonomousMaterializerError::Integrity)
}

fn policy_u128(value: &Value, field: &str) -> Result<u128, AutonomousMaterializerError> {
    decimal_u128(policy_text(value, field)?)
}

fn policy_u64(value: &Value, field: &str) -> Result<u64, AutonomousMaterializerError> {
    value
        .get(field)
        .and_then(Value::as_u64)
        .ok_or(AutonomousMaterializerError::Integrity)
}

fn object_text<'a>(
    value: &'a Map<String, Value>,
    field: &str,
) -> Result<&'a str, AutonomousMaterializerError> {
    value
        .get(field)
        .and_then(Value::as_str)
        .ok_or(AutonomousMaterializerError::Integrity)
}

fn object_u128(
    value: &Map<String, Value>,
    field: &str,
) -> Result<u128, AutonomousMaterializerError> {
    decimal_u128(object_text(value, field)?)
}

fn object_u64(value: &Map<String, Value>, field: &str) -> Result<u64, AutonomousMaterializerError> {
    value
        .get(field)
        .and_then(Value::as_u64)
        .ok_or(AutonomousMaterializerError::Integrity)
}

fn decimal_u128(value: &str) -> Result<u128, AutonomousMaterializerError> {
    if value.is_empty()
        || (value.len() > 1 && value.starts_with('0'))
        || !value.bytes().all(|byte| byte.is_ascii_digit())
    {
        return Err(AutonomousMaterializerError::Integrity);
    }
    value
        .parse()
        .map_err(|_| AutonomousMaterializerError::Integrity)
}

fn decimal_u64(value: &str) -> Result<u64, AutonomousMaterializerError> {
    decimal_u128(value)?
        .try_into()
        .map_err(|_| AutonomousMaterializerError::Integrity)
}

fn i64_to_u64(value: i64) -> Result<u64, AutonomousMaterializerError> {
    value
        .try_into()
        .map_err(|_| AutonomousMaterializerError::Integrity)
}

fn decode_fixed<const N: usize>(value: &str) -> Result<[u8; N], AutonomousMaterializerError> {
    if value.len() != N * 2 {
        return Err(AutonomousMaterializerError::Integrity);
    }
    hex::decode(value)
        .ok()
        .and_then(|bytes| bytes.try_into().ok())
        .ok_or(AutonomousMaterializerError::Integrity)
}

fn deterministic_uuid(domain: &str, seed: &str) -> Uuid {
    let digest = Sha256::digest(format!("{domain}:{seed}"));
    let mut bytes: [u8; 16] = digest[..16].try_into().expect("digest length");
    bytes[6] = (bytes[6] & 0x0f) | 0x40;
    bytes[8] = (bytes[8] & 0x3f) | 0x80;
    Uuid::from_bytes(bytes)
}

fn timestamp_string(value: DateTime<Utc>) -> String {
    value.to_rfc3339_opts(SecondsFormat::Secs, true)
}

fn parse_timestamp(
    value: &Value,
    field: &str,
) -> Result<DateTime<Utc>, AutonomousMaterializerError> {
    DateTime::parse_from_rfc3339(policy_text(value, field)?)
        .map(|value| value.with_timezone(&Utc))
        .map_err(|_| AutonomousMaterializerError::Integrity)
}

fn canonical_time(value: DateTime<Utc>) -> Result<DateTime<Utc>, AutonomousMaterializerError> {
    Utc.timestamp_opt(value.timestamp(), 0)
        .single()
        .ok_or(AutonomousMaterializerError::Arithmetic)
}

fn database_error(error: sqlx::Error) -> AutonomousMaterializerError {
    match error {
        sqlx::Error::Io(_)
        | sqlx::Error::Tls(_)
        | sqlx::Error::PoolTimedOut
        | sqlx::Error::PoolClosed
        | sqlx::Error::WorkerCrashed => AutonomousMaterializerError::Dependency,
        _ => AutonomousMaterializerError::Integrity,
    }
}

#[derive(Clone, Copy, Debug, Error, PartialEq, Eq)]
pub enum AutonomousMaterializerError {
    #[error("autonomous materializer configuration is invalid")]
    Configuration,
    #[error("autonomous materializer dependency is unavailable")]
    Dependency,
    #[error("autonomous materializer policy rejected the candidate")]
    Policy,
    #[error("autonomous materializer integrity failed")]
    Integrity,
    #[error("autonomous materializer arithmetic failed")]
    Arithmetic,
}
