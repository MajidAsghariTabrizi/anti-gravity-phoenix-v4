use crate::config::ExecutorConfig;
use crate::model::{
    ActiveAttempt, AttemptStatus, ExecutionLeg, ExecutionRequest, RawExecutionRequest,
    ReceiptOutcome, TransactionHash,
};
use async_trait::async_trait;
use chrono::{DateTime, Utc};
use serde_json::{json, Value};
use sqlx::postgres::PgPoolOptions;
use sqlx::types::Json;
use sqlx::{PgPool, Postgres, Row, Transaction};
use thiserror::Error;
use uuid::Uuid;

const SCHEMA_VERSION: &str = "phoenix.live-canary-schema.v4";
const ACTIVE_STATUSES: &str =
    "'claimed', 'nonce_allocated', 'submission_unknown', 'pending', 'timed_out'";

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ControlState {
    pub armed: bool,
    pub kill_switch: bool,
}

#[async_trait]
pub trait ExecutorStore: Send + Sync {
    async fn validate_schema(&self) -> Result<(), StoreError>;
    async fn control_state(&self) -> Result<ControlState, StoreError>;
    async fn active_attempt(&self) -> Result<Option<ActiveAttempt>, StoreError>;
    async fn claim_approved(
        &self,
        config: &ExecutorConfig,
        now: DateTime<Utc>,
    ) -> Result<Option<ExecutionRequest>, StoreError>;
    async fn allocate_nonce(
        &self,
        request_id: Uuid,
        config: &ExecutorConfig,
        network_pending_nonce: u64,
    ) -> Result<u64, StoreError>;
    async fn mark_signed(
        &self,
        _request_id: Uuid,
        _signed_at: DateTime<Utc>,
    ) -> Result<(), StoreError> {
        Ok(())
    }
    async fn mark_submission_unknown(
        &self,
        request_id: Uuid,
        error_code: &'static str,
        observed_at: DateTime<Utc>,
    ) -> Result<(), StoreError>;
    async fn fail_unsubmitted(
        &self,
        request_id: Uuid,
        error_code: &'static str,
        terminal_at: DateTime<Utc>,
    ) -> Result<(), StoreError>;
    async fn mark_pending(
        &self,
        request_id: Uuid,
        tx_hash: TransactionHash,
        submitted_at: DateTime<Utc>,
    ) -> Result<(), StoreError>;
    async fn mark_terminal(
        &self,
        request_id: Uuid,
        status: AttemptStatus,
        error_code: Option<&'static str>,
        receipt_outcome: Option<&ReceiptOutcome>,
        terminal_at: DateTime<Utc>,
    ) -> Result<(), StoreError>;
    async fn record_monitor_error(
        &self,
        request_id: Uuid,
        error_code: &'static str,
    ) -> Result<(), StoreError>;
    async fn daily_loss_wei(&self, now: DateTime<Utc>) -> Result<u128, StoreError>;
    async fn disarm(&self, reason: &'static str) -> Result<(), StoreError>;
}

#[derive(Clone)]
pub struct PostgresExecutorStore {
    pool: PgPool,
}

impl PostgresExecutorStore {
    pub async fn connect(dsn: &str) -> Result<Self, StoreError> {
        let pool = PgPoolOptions::new()
            .max_connections(4)
            .acquire_timeout(std::time::Duration::from_secs(5))
            .connect(dsn)
            .await
            .map_err(StoreError::from)?;
        Ok(Self { pool })
    }

    pub fn from_pool(pool: PgPool) -> Self {
        Self { pool }
    }
}

#[async_trait]
impl ExecutorStore for PostgresExecutorStore {
    async fn validate_schema(&self) -> Result<(), StoreError> {
        let version: String = sqlx::query_scalar(
            "SELECT version FROM live_canary.schema_contract WHERE version = $1",
        )
        .bind(SCHEMA_VERSION)
        .fetch_one(&self.pool)
        .await
        .map_err(StoreError::from)?;
        if version != SCHEMA_VERSION {
            return Err(StoreError::Schema);
        }
        let controls: i64 = sqlx::query_scalar(
            "SELECT
                (SELECT count(*) FROM live_canary.control WHERE singleton)
                + (SELECT count(*) FROM live_canary.autonomous_global_control WHERE singleton)",
        )
        .fetch_one(&self.pool)
        .await
        .map_err(StoreError::from)?;
        if controls != 2 {
            return Err(StoreError::Schema);
        }
        Ok(())
    }

    async fn control_state(&self) -> Result<ControlState, StoreError> {
        let row = sqlx::query(
            "SELECT c.armed AND a.armed AND a.execution_mode = 'live' AS armed,
                    c.kill_switch OR a.kill_switch AS kill_switch
             FROM live_canary.control c
             CROSS JOIN live_canary.autonomous_global_control a
             WHERE c.singleton AND a.singleton",
        )
        .fetch_one(&self.pool)
        .await
        .map_err(StoreError::from)?;
        Ok(ControlState {
            armed: row.try_get("armed").map_err(StoreError::from)?,
            kill_switch: row.try_get("kill_switch").map_err(StoreError::from)?,
        })
    }

    async fn active_attempt(&self) -> Result<Option<ActiveAttempt>, StoreError> {
        let query = format!(
            "{} WHERE a.status IN ({ACTIVE_STATUSES}) ORDER BY a.id LIMIT 2",
            active_attempt_select()
        );
        let rows = sqlx::query(&query)
            .fetch_all(&self.pool)
            .await
            .map_err(StoreError::from)?;
        match rows.len() {
            0 => Ok(None),
            1 => decode_active_attempt(&rows[0]).map(Some),
            _ => Err(StoreError::Invariant),
        }
    }

    async fn claim_approved(
        &self,
        config: &ExecutorConfig,
        now: DateTime<Utc>,
    ) -> Result<Option<ExecutionRequest>, StoreError> {
        let mut transaction = self.pool.begin().await.map_err(StoreError::from)?;
        let control = sqlx::query(
            "SELECT c.armed AND a.armed AND a.execution_mode = 'live' AS armed,
                    c.kill_switch OR a.kill_switch AS kill_switch
             FROM live_canary.control c
             CROSS JOIN live_canary.autonomous_global_control a
             WHERE c.singleton AND a.singleton
             FOR UPDATE OF c, a",
        )
        .fetch_one(&mut *transaction)
        .await
        .map_err(StoreError::from)?;
        let armed: bool = control.try_get("armed").map_err(StoreError::from)?;
        let kill_switch: bool = control.try_get("kill_switch").map_err(StoreError::from)?;
        if !armed || kill_switch {
            transaction.commit().await.map_err(StoreError::from)?;
            return Ok(None);
        }

        let active: i64 = sqlx::query_scalar(&format!(
            "SELECT count(*) FROM live_canary.execution_attempts
             WHERE status IN ({ACTIVE_STATUSES})"
        ))
        .fetch_one(&mut *transaction)
        .await
        .map_err(StoreError::from)?;
        if active != 0 {
            transaction.commit().await.map_err(StoreError::from)?;
            return Ok(None);
        }

        let row = sqlx::query(&format!(
            "{} WHERE r.status = 'approved'
                 AND r.schema_version = $2
                 AND r.approved_at IS NOT NULL
                 AND r.approved_by IS NOT NULL
                 AND r.policy_version IS NOT NULL
                 AND r.approval_digest IS NOT NULL
                 AND r.deadline > $1
                 AND r.approval_deadline > $1
             ORDER BY r.approved_at, r.id
             FOR UPDATE OF r SKIP LOCKED
             LIMIT 1",
            request_select()
        ))
        .bind(now)
        .bind(crate::REQUEST_SCHEMA_VERSION)
        .fetch_optional(&mut *transaction)
        .await
        .map_err(StoreError::from)?;
        let Some(row) = row else {
            transaction.commit().await.map_err(StoreError::from)?;
            return Ok(None);
        };
        let request = decode_request(&row)?;
        let updated = sqlx::query(
            "UPDATE live_canary.execution_requests
             SET status = 'claimed', updated_at = $2
             WHERE id = $1 AND status = 'approved'",
        )
        .bind(request.id)
        .bind(now)
        .execute(&mut *transaction)
        .await
        .map_err(StoreError::from)?;
        if updated.rows_affected() != 1 {
            return Err(StoreError::Invariant);
        }
        sqlx::query(
            "INSERT INTO live_canary.execution_attempts(
                request_id, chain_id, wallet_address, executor_address, status, claimed_at
             )
             VALUES ($1, $2, $3, $4, 'claimed', $5)",
        )
        .bind(request.id)
        .bind(i64::try_from(config.chain_id).map_err(|_| StoreError::Invariant)?)
        .bind(config.wallet_address.to_string())
        .bind(config.executor_address.to_string())
        .bind(now)
        .execute(&mut *transaction)
        .await
        .map_err(StoreError::from)?;
        update_candidate_status_for_request(
            &mut transaction,
            request.id,
            "request_materialized",
            "claimed",
        )
        .await?;
        transaction.commit().await.map_err(StoreError::from)?;
        Ok(Some(request))
    }

    async fn allocate_nonce(
        &self,
        request_id: Uuid,
        config: &ExecutorConfig,
        network_pending_nonce: u64,
    ) -> Result<u64, StoreError> {
        let mut transaction = self.pool.begin().await.map_err(StoreError::from)?;
        sqlx::query("SELECT pg_advisory_xact_lock(hashtextextended($1, 0))")
            .bind(format!(
                "live-canary-nonce:{}:{}",
                config.chain_id, config.wallet_address
            ))
            .execute(&mut *transaction)
            .await
            .map_err(StoreError::from)?;
        let stored = sqlx::query_scalar::<_, String>(
            "SELECT next_nonce::text
             FROM live_canary.nonce_state
             WHERE chain_id = $1 AND wallet_address = $2
             FOR UPDATE",
        )
        .bind(i64::try_from(config.chain_id).map_err(|_| StoreError::Invariant)?)
        .bind(config.wallet_address.to_string())
        .fetch_optional(&mut *transaction)
        .await
        .map_err(StoreError::from)?
        .map(|value| value.parse::<u64>().map_err(|_| StoreError::Data))
        .transpose()?;
        let nonce = stored
            .unwrap_or(network_pending_nonce)
            .max(network_pending_nonce);
        let next_nonce = nonce.checked_add(1).ok_or(StoreError::Data)?;
        sqlx::query(
            "INSERT INTO live_canary.nonce_state(
                chain_id, wallet_address, next_nonce, updated_at
             )
             VALUES ($1, $2, $3::numeric, now())
             ON CONFLICT (chain_id, wallet_address)
             DO UPDATE SET next_nonce = EXCLUDED.next_nonce, updated_at = now()",
        )
        .bind(i64::try_from(config.chain_id).map_err(|_| StoreError::Invariant)?)
        .bind(config.wallet_address.to_string())
        .bind(next_nonce.to_string())
        .execute(&mut *transaction)
        .await
        .map_err(StoreError::from)?;
        let updated = sqlx::query(
            "UPDATE live_canary.execution_attempts
             SET nonce = $2::numeric, status = 'nonce_allocated', updated_at = now()
             WHERE request_id = $1 AND status = 'claimed'",
        )
        .bind(request_id)
        .bind(nonce.to_string())
        .execute(&mut *transaction)
        .await
        .map_err(StoreError::from)?;
        if updated.rows_affected() != 1 {
            return Err(StoreError::Invariant);
        }
        sqlx::query(
            "UPDATE live_canary.execution_requests
             SET status = 'nonce_allocated', updated_at = now()
             WHERE id = $1 AND status = 'claimed'",
        )
        .bind(request_id)
        .execute(&mut *transaction)
        .await
        .map_err(StoreError::from)?;
        transaction.commit().await.map_err(StoreError::from)?;
        Ok(nonce)
    }

    async fn mark_signed(
        &self,
        request_id: Uuid,
        signed_at: DateTime<Utc>,
    ) -> Result<(), StoreError> {
        let mut transaction = self.pool.begin().await.map_err(StoreError::from)?;
        let attempt: i64 = sqlx::query_scalar(
            "SELECT count(*) FROM live_canary.execution_attempts
             WHERE request_id = $1
               AND status = 'nonce_allocated'
               AND nonce IS NOT NULL
               AND tx_hash IS NULL",
        )
        .bind(request_id)
        .fetch_one(&mut *transaction)
        .await
        .map_err(StoreError::from)?;
        if attempt != 1 {
            return Err(StoreError::Invariant);
        }
        let updated = sqlx::query(
            "UPDATE live_canary.autonomous_candidates
             SET status = 'signed', updated_at = $2
             WHERE execution_request_id = $1 AND status = 'claimed'",
        )
        .bind(request_id)
        .bind(signed_at)
        .execute(&mut *transaction)
        .await
        .map_err(StoreError::from)?;
        if updated.rows_affected() > 1 {
            return Err(StoreError::Invariant);
        }
        transaction.commit().await.map_err(StoreError::from)
    }

    async fn mark_submission_unknown(
        &self,
        request_id: Uuid,
        error_code: &'static str,
        observed_at: DateTime<Utc>,
    ) -> Result<(), StoreError> {
        if error_code.is_empty() || error_code.len() > 128 {
            return Err(StoreError::Invariant);
        }
        let mut transaction = self.pool.begin().await.map_err(StoreError::from)?;
        let updated = sqlx::query(
            "UPDATE live_canary.execution_attempts
             SET status = 'submission_unknown', error_code = $2, updated_at = $3
             WHERE request_id = $1
               AND status = 'nonce_allocated'
               AND nonce IS NOT NULL
               AND tx_hash IS NULL",
        )
        .bind(request_id)
        .bind(error_code)
        .bind(observed_at)
        .execute(&mut *transaction)
        .await
        .map_err(StoreError::from)?;
        if updated.rows_affected() != 1 {
            return Err(StoreError::Invariant);
        }
        let request_updated = sqlx::query(
            "UPDATE live_canary.execution_requests
             SET status = 'submission_unknown', updated_at = $2
             WHERE id = $1 AND status = 'nonce_allocated'",
        )
        .bind(request_id)
        .bind(observed_at)
        .execute(&mut *transaction)
        .await
        .map_err(StoreError::from)?;
        if request_updated.rows_affected() != 1 {
            return Err(StoreError::Invariant);
        }
        let candidate_updated = sqlx::query(
            "UPDATE live_canary.autonomous_candidates
             SET status = 'submission_unknown', updated_at = $2
             WHERE execution_request_id = $1
               AND status IN ('claimed', 'signed', 'submitted')",
        )
        .bind(request_id)
        .bind(observed_at)
        .execute(&mut *transaction)
        .await
        .map_err(StoreError::from)?;
        if candidate_updated.rows_affected() > 1 {
            return Err(StoreError::Invariant);
        }
        transaction.commit().await.map_err(StoreError::from)
    }

    async fn fail_unsubmitted(
        &self,
        request_id: Uuid,
        error_code: &'static str,
        terminal_at: DateTime<Utc>,
    ) -> Result<(), StoreError> {
        if error_code.is_empty() || error_code.len() > 128 {
            return Err(StoreError::Invariant);
        }
        let mut transaction = self.pool.begin().await.map_err(StoreError::from)?;
        let row = sqlx::query(
            "SELECT chain_id, wallet_address, nonce::text AS nonce
             FROM live_canary.execution_attempts
             WHERE request_id = $1
               AND status = 'nonce_allocated'
               AND nonce IS NOT NULL
               AND tx_hash IS NULL
             FOR UPDATE",
        )
        .bind(request_id)
        .fetch_one(&mut *transaction)
        .await
        .map_err(StoreError::from)?;
        let chain_id: i64 = row.try_get("chain_id").map_err(StoreError::from)?;
        let wallet_address: String = row.try_get("wallet_address").map_err(StoreError::from)?;
        let nonce = row
            .try_get::<String, _>("nonce")
            .map_err(StoreError::from)?
            .parse::<u64>()
            .map_err(|_| StoreError::Data)?;
        let reserved_next = nonce.checked_add(1).ok_or(StoreError::Data)?;
        let nonce_updated = sqlx::query(
            "UPDATE live_canary.nonce_state
             SET next_nonce = $4::numeric, updated_at = $5
             WHERE chain_id = $1
               AND wallet_address = $2
               AND next_nonce = $3::numeric",
        )
        .bind(chain_id)
        .bind(&wallet_address)
        .bind(reserved_next.to_string())
        .bind(nonce.to_string())
        .bind(terminal_at)
        .execute(&mut *transaction)
        .await
        .map_err(StoreError::from)?;
        if nonce_updated.rows_affected() != 1 {
            return Err(StoreError::Invariant);
        }
        let attempt_updated = sqlx::query(
            "UPDATE live_canary.execution_attempts
             SET status = 'failed', error_code = $2, terminal_at = $3, updated_at = $3
             WHERE request_id = $1 AND status = 'nonce_allocated' AND tx_hash IS NULL",
        )
        .bind(request_id)
        .bind(error_code)
        .bind(terminal_at)
        .execute(&mut *transaction)
        .await
        .map_err(StoreError::from)?;
        if attempt_updated.rows_affected() != 1 {
            return Err(StoreError::Invariant);
        }
        update_request_status(&mut transaction, request_id, "nonce_allocated", "failed").await?;
        let candidate_updated = sqlx::query(
            "UPDATE live_canary.autonomous_candidates
             SET status = 'submission_failed_known', updated_at = $2
             WHERE execution_request_id = $1 AND status IN ('claimed', 'signed')",
        )
        .bind(request_id)
        .bind(terminal_at)
        .execute(&mut *transaction)
        .await
        .map_err(StoreError::from)?;
        if candidate_updated.rows_affected() > 1 {
            return Err(StoreError::Invariant);
        }
        transaction.commit().await.map_err(StoreError::from)
    }

    async fn mark_pending(
        &self,
        request_id: Uuid,
        tx_hash: TransactionHash,
        submitted_at: DateTime<Utc>,
    ) -> Result<(), StoreError> {
        let mut transaction = self.pool.begin().await.map_err(StoreError::from)?;
        let updated = sqlx::query(
            "UPDATE live_canary.execution_attempts
             SET tx_hash = $2, status = 'pending', submitted_at = $3, updated_at = $3
             WHERE request_id = $1 AND status = 'nonce_allocated' AND tx_hash IS NULL",
        )
        .bind(request_id)
        .bind(tx_hash.to_string())
        .bind(submitted_at)
        .execute(&mut *transaction)
        .await
        .map_err(StoreError::from)?;
        if updated.rows_affected() != 1 {
            return Err(StoreError::Invariant);
        }
        update_request_status(&mut transaction, request_id, "nonce_allocated", "pending").await?;
        update_candidate_status_for_request(&mut transaction, request_id, "signed", "submitted")
            .await?;
        transaction.commit().await.map_err(StoreError::from)
    }

    async fn mark_terminal(
        &self,
        request_id: Uuid,
        status: AttemptStatus,
        error_code: Option<&'static str>,
        receipt_outcome: Option<&ReceiptOutcome>,
        terminal_at: DateTime<Utc>,
    ) -> Result<(), StoreError> {
        if !matches!(
            status,
            AttemptStatus::Confirmed
                | AttemptStatus::Reverted
                | AttemptStatus::Replaced
                | AttemptStatus::TimedOut
                | AttemptStatus::Failed
        ) || matches!(status, AttemptStatus::Confirmed | AttemptStatus::Reverted)
            != receipt_outcome.is_some()
        {
            return Err(StoreError::Invariant);
        }
        if let Some(outcome) = receipt_outcome {
            let fee = i128::try_from(outcome.actual_fee_wei).map_err(|_| StoreError::Invariant)?;
            let realized_profit = i128::try_from(outcome.settlement.realized_profit)
                .map_err(|_| StoreError::Invariant)?;
            let valid_receipt_state = match status {
                AttemptStatus::Confirmed => {
                    outcome.receipt_status == 1
                        && outcome.settled_event_found
                        && realized_profit.checked_sub(fee) == Some(outcome.net_pnl_wei)
                }
                AttemptStatus::Reverted => {
                    outcome.receipt_status == 0
                        && !outcome.settled_event_found
                        && outcome.settlement.premium == 0
                        && outcome.settlement.realized_profit == 0
                        && fee.checked_neg() == Some(outcome.net_pnl_wei)
                }
                _ => false,
            };
            if !valid_receipt_state
                || outcome.block_number == 0
                || outcome.gas_used == 0
                || outcome.effective_gas_price == 0
                || outcome.actual_fee_wei == 0
                || outcome.actual_l1_cost_wei > outcome.actual_fee_wei
            {
                return Err(StoreError::Invariant);
            }
        }
        let mut transaction = self.pool.begin().await.map_err(StoreError::from)?;
        let row = sqlx::query(
            "SELECT tx_hash, nonce::text AS nonce, submitted_at
             FROM live_canary.execution_attempts
             WHERE request_id = $1
               AND status IN ('claimed', 'nonce_allocated', 'pending', 'timed_out')
             FOR UPDATE",
        )
        .bind(request_id)
        .fetch_one(&mut *transaction)
        .await
        .map_err(StoreError::from)?;
        let tx_hash: Option<String> = row.try_get("tx_hash").map_err(StoreError::from)?;
        let nonce: Option<String> = row.try_get("nonce").map_err(StoreError::from)?;
        let submitted_at: Option<DateTime<Utc>> =
            row.try_get("submitted_at").map_err(StoreError::from)?;
        if status != AttemptStatus::Failed && tx_hash.is_none() {
            return Err(StoreError::Invariant);
        }
        if let Some(outcome) = receipt_outcome {
            let tx_hash = tx_hash.as_deref().ok_or(StoreError::Invariant)?;
            let receipt_status =
                i16::try_from(outcome.receipt_status).map_err(|_| StoreError::Invariant)?;
            sqlx::query(
                "INSERT INTO live_canary.execution_outcomes(
                    request_id, tx_hash, outcome_status, receipt_status,
                    settled_event_found, block_number, gas_used, effective_gas_price,
                    actual_fee_wei, asset, flash_amount, premium,
                    realized_profit, net_pnl_wei, recorded_at, l1_cost_wei,
                    ordering_cost_wei, allocated_infrastructure_cost_wei,
                    submitted_at, submission_channel
                 )
                 VALUES (
                    $1, $2, $3, $4, $5, $6::numeric, $7::numeric,
                    $8::numeric, $9::numeric, $10, $11::numeric,
                    $12::numeric, $13::numeric, $14::numeric, $15,
                    $16::numeric, 0, 0, $17, 'standard_rpc'
                 )",
            )
            .bind(request_id)
            .bind(tx_hash)
            .bind(status.as_str())
            .bind(receipt_status)
            .bind(outcome.settled_event_found)
            .bind(outcome.block_number.to_string())
            .bind(outcome.gas_used.to_string())
            .bind(outcome.effective_gas_price.to_string())
            .bind(outcome.actual_fee_wei.to_string())
            .bind(outcome.settlement.asset.to_string())
            .bind(outcome.settlement.flash_amount.to_string())
            .bind(outcome.settlement.premium.to_string())
            .bind(outcome.settlement.realized_profit.to_string())
            .bind(outcome.net_pnl_wei.to_string())
            .bind(terminal_at)
            .bind(outcome.actual_l1_cost_wei.to_string())
            .bind(submitted_at)
            .execute(&mut *transaction)
            .await
            .map_err(StoreError::from)?;
            insert_autonomous_outcome(
                &mut transaction,
                request_id,
                tx_hash,
                nonce.as_deref(),
                submitted_at,
                status,
                outcome,
                terminal_at,
            )
            .await?;
        }
        let updated = sqlx::query(
            "UPDATE live_canary.execution_attempts
             SET status = $2, error_code = $3, terminal_at = $4, updated_at = $4
             WHERE request_id = $1
               AND status IN ('claimed', 'nonce_allocated', 'pending', 'timed_out')",
        )
        .bind(request_id)
        .bind(status.as_str())
        .bind(error_code)
        .bind(terminal_at)
        .execute(&mut *transaction)
        .await
        .map_err(StoreError::from)?;
        if updated.rows_affected() != 1 {
            return Err(StoreError::Invariant);
        }
        let request_updated = sqlx::query(
            "UPDATE live_canary.execution_requests
             SET status = $2, updated_at = $3
             WHERE id = $1
               AND status IN ('claimed', 'nonce_allocated', 'pending', 'timed_out')",
        )
        .bind(request_id)
        .bind(status.as_str())
        .bind(terminal_at)
        .execute(&mut *transaction)
        .await
        .map_err(StoreError::from)?;
        if request_updated.rows_affected() != 1 {
            return Err(StoreError::Invariant);
        }
        let candidate_status = match status {
            AttemptStatus::Confirmed => {
                if receipt_outcome.is_some_and(|outcome| outcome.net_pnl_wei > 0) {
                    "confirmed_profitable"
                } else {
                    "confirmed_unprofitable"
                }
            }
            AttemptStatus::Reverted => "reverted",
            AttemptStatus::Failed => "submission_failed_known",
            AttemptStatus::Replaced | AttemptStatus::TimedOut => "disarmed",
            _ => return Err(StoreError::Invariant),
        };
        let candidate_updated = sqlx::query(
            "UPDATE live_canary.autonomous_candidates
             SET status = $2, updated_at = $3
             WHERE execution_request_id = $1
               AND status IN ('claimed', 'signed', 'submitted')",
        )
        .bind(request_id)
        .bind(candidate_status)
        .bind(terminal_at)
        .execute(&mut *transaction)
        .await
        .map_err(StoreError::from)?;
        if candidate_updated.rows_affected() > 1 {
            return Err(StoreError::Invariant);
        }
        apply_autonomous_risk_feedback(&mut transaction, request_id, terminal_at).await?;
        transaction.commit().await.map_err(StoreError::from)
    }

    async fn record_monitor_error(
        &self,
        request_id: Uuid,
        error_code: &'static str,
    ) -> Result<(), StoreError> {
        let updated = sqlx::query(
            "UPDATE live_canary.execution_attempts
             SET error_code = $2, updated_at = now()
             WHERE request_id = $1 AND status IN ('pending', 'timed_out')",
        )
        .bind(request_id)
        .bind(error_code)
        .execute(&self.pool)
        .await
        .map_err(StoreError::from)?;
        if updated.rows_affected() != 1 {
            return Err(StoreError::Invariant);
        }
        Ok(())
    }

    async fn daily_loss_wei(&self, now: DateTime<Utc>) -> Result<u128, StoreError> {
        let value: String = sqlx::query_scalar(
            "WITH bounds AS (
                SELECT (
                    date_trunc('day', $1::timestamptz AT TIME ZONE 'UTC')
                    AT TIME ZONE 'UTC'
                ) AS start_at
             )
             SELECT COALESCE(
                SUM(CASE WHEN net_pnl_wei < 0 THEN -net_pnl_wei ELSE 0 END),
                0
             )::text
             FROM live_canary.execution_outcomes, bounds
             WHERE recorded_at >= bounds.start_at
               AND recorded_at < bounds.start_at + interval '1 day'",
        )
        .bind(now)
        .fetch_one(&self.pool)
        .await
        .map_err(StoreError::from)?;
        value.parse::<u128>().map_err(|_| StoreError::Data)
    }

    async fn disarm(&self, reason: &'static str) -> Result<(), StoreError> {
        if reason.is_empty() || reason.len() > 128 {
            return Err(StoreError::Invariant);
        }
        let mut transaction = self.pool.begin().await.map_err(StoreError::from)?;
        let updated = sqlx::query(
            "UPDATE live_canary.control
             SET armed = false, kill_switch = true, disarm_reason = $1, updated_at = now()
             WHERE singleton",
        )
        .bind(reason)
        .execute(&mut *transaction)
        .await
        .map_err(StoreError::from)?;
        if updated.rows_affected() != 1 {
            return Err(StoreError::Invariant);
        }
        let autonomous = sqlx::query(
            "UPDATE live_canary.autonomous_global_control
             SET armed = false, kill_switch = true, execution_mode = 'disarmed',
                 disarm_reason = $1, control_hash = NULL,
                 control_contract = NULL, updated_at = now()
             WHERE singleton",
        )
        .bind(reason)
        .execute(&mut *transaction)
        .await
        .map_err(StoreError::from)?;
        if autonomous.rows_affected() != 1 {
            return Err(StoreError::Invariant);
        }
        sqlx::query(
            "UPDATE live_canary.autonomous_candidates
             SET status = 'disarmed', updated_at = now()
             WHERE status IN ('materialized', 'approval_pending', 'approved', 'request_materialized',
                              'claimed', 'signed')",
        )
        .execute(&mut *transaction)
        .await
        .map_err(StoreError::from)?;
        transaction.commit().await.map_err(StoreError::from)
    }
}

async fn update_request_status(
    transaction: &mut Transaction<'_, Postgres>,
    request_id: Uuid,
    from: &'static str,
    to: &'static str,
) -> Result<(), StoreError> {
    let updated = sqlx::query(
        "UPDATE live_canary.execution_requests
         SET status = $3, updated_at = now()
         WHERE id = $1 AND status = $2",
    )
    .bind(request_id)
    .bind(from)
    .bind(to)
    .execute(&mut **transaction)
    .await
    .map_err(StoreError::from)?;
    if updated.rows_affected() != 1 {
        return Err(StoreError::Invariant);
    }
    Ok(())
}

#[allow(clippy::too_many_arguments)]
async fn insert_autonomous_outcome(
    transaction: &mut Transaction<'_, Postgres>,
    request_id: Uuid,
    tx_hash: &str,
    nonce: Option<&str>,
    submitted_at: Option<DateTime<Utc>>,
    status: AttemptStatus,
    outcome: &ReceiptOutcome,
    terminal_at: DateTime<Utc>,
) -> Result<(), StoreError> {
    let row = sqlx::query(
        "SELECT c.candidate_id, c.opportunity_id, c.route_fingerprint,
                c.route_universe_hash, c.route_policy_hash,
                c.risk_snapshot_hash, c.submission_quote_hash,
                c.candidate_hash, c.state_hash, c.plan_hash, c.calldata_hash,
                c.executor_code_hash,
                c.predicted_gross_profit::text AS predicted_gross_profit,
                (
                    c.predicted_gross_profit
                    - (c.submission_quote_contract->>'expected_net_after_ordering')::numeric
                )::text AS predicted_total_cost,
                (c.submission_quote_contract->>'expected_net_after_ordering')::text
                    AS conservative_predicted_net_pnl,
                a.automatic_approval_digest
         FROM live_canary.autonomous_candidates c
         JOIN live_canary.autonomous_approvals a ON a.candidate_id = c.candidate_id
         WHERE c.execution_request_id = $1
         FOR UPDATE OF c",
    )
    .bind(request_id)
    .fetch_optional(&mut **transaction)
    .await
    .map_err(StoreError::from)?;
    let Some(row) = row else {
        return Ok(());
    };
    let nonce = nonce
        .ok_or(StoreError::Invariant)?
        .parse::<u64>()
        .map_err(|_| StoreError::Data)?;
    let submitted_at = submitted_at.ok_or(StoreError::Invariant)?;
    let predicted_gross_profit: String = row
        .try_get("predicted_gross_profit")
        .map_err(StoreError::from)?;
    let predicted_total_cost: String = row
        .try_get("predicted_total_cost")
        .map_err(StoreError::from)?;
    let conservative_predicted_net_pnl: String = row
        .try_get("conservative_predicted_net_pnl")
        .map_err(StoreError::from)?;
    let conservative = conservative_predicted_net_pnl
        .parse::<i128>()
        .map_err(|_| StoreError::Data)?;
    let realized_gross_profit =
        i128::try_from(outcome.settlement.realized_profit).map_err(|_| StoreError::Data)?;
    let actual_l1_cost = outcome.actual_l1_cost_wei.min(outcome.actual_fee_wei);
    let actual_gas_cost = outcome
        .actual_fee_wei
        .checked_sub(actual_l1_cost)
        .ok_or(StoreError::Data)?;
    let prediction_error = outcome
        .net_pnl_wei
        .checked_sub(conservative)
        .ok_or(StoreError::Data)?;
    let outcome_class = match status {
        AttemptStatus::Confirmed if outcome.net_pnl_wei > 0 => "confirmed_profitable",
        AttemptStatus::Confirmed => "confirmed_negative",
        AttemptStatus::Reverted => "reverted",
        _ => return Err(StoreError::Invariant),
    };
    let candidate_id: Uuid = row.try_get("candidate_id").map_err(StoreError::from)?;
    let opportunity_id: Uuid = row.try_get("opportunity_id").map_err(StoreError::from)?;
    let route_fingerprint: String = row.try_get("route_fingerprint").map_err(StoreError::from)?;
    let route_universe_hash: String = row
        .try_get("route_universe_hash")
        .map_err(StoreError::from)?;
    let route_policy_hash: String = row.try_get("route_policy_hash").map_err(StoreError::from)?;
    let risk_snapshot_hash: String = row
        .try_get("risk_snapshot_hash")
        .map_err(StoreError::from)?;
    let submission_quote_hash: String = row
        .try_get("submission_quote_hash")
        .map_err(StoreError::from)?;
    let automatic_approval_digest: String = row
        .try_get("automatic_approval_digest")
        .map_err(StoreError::from)?;
    let candidate_hash: String = row.try_get("candidate_hash").map_err(StoreError::from)?;
    let state_hash: String = row.try_get("state_hash").map_err(StoreError::from)?;
    let plan_hash: String = row.try_get("plan_hash").map_err(StoreError::from)?;
    let calldata_hash: String = row.try_get("calldata_hash").map_err(StoreError::from)?;
    let executor_code_hash: String = row
        .try_get("executor_code_hash")
        .map_err(StoreError::from)?;
    let mut contract = json!({
        "schema_version": "phoenix.outcome.v1",
        "candidate_id": candidate_id,
        "opportunity_id": opportunity_id,
        "route_fingerprint": route_fingerprint,
        "route_universe_hash": route_universe_hash,
        "route_policy_hash": route_policy_hash,
        "risk_snapshot_hash": risk_snapshot_hash,
        "submission_quote_hash": submission_quote_hash,
        "automatic_approval_digest": automatic_approval_digest,
        "candidate_hash": candidate_hash,
        "state_hash": state_hash,
        "plan_hash": plan_hash,
        "calldata_hash": calldata_hash,
        "executor_code_hash": executor_code_hash,
        "outcome_class": outcome_class,
        "transaction_hash": tx_hash,
        "nonce": nonce,
        "submission_channel": "standard_rpc",
        "submitted_at": submitted_at.to_rfc3339_opts(chrono::SecondsFormat::Secs, true),
        "block_number": outcome.block_number,
        "receipt_status": outcome.receipt_status,
        "gas_used": outcome.gas_used,
        "effective_gas_price": outcome.effective_gas_price.to_string(),
        "predicted_gross_profit": predicted_gross_profit,
        "predicted_total_cost": predicted_total_cost,
        "conservative_predicted_net_pnl": conservative_predicted_net_pnl,
        "realized_gross_profit": realized_gross_profit.to_string(),
        "actual_flash_premium": outcome.settlement.premium.to_string(),
        "actual_gas_cost": actual_gas_cost.to_string(),
        "actual_l1_cost": actual_l1_cost.to_string(),
        "actual_ordering_cost": "0",
        "realized_chain_net_pnl": outcome.net_pnl_wei.to_string(),
        "allocated_infrastructure_cost": "0",
        "realized_business_net_pnl": outcome.net_pnl_wei.to_string(),
        "prediction_error": prediction_error.to_string(),
        "failure_reason": if status == AttemptStatus::Reverted {
            Value::String("transaction_reverted".to_string())
        } else {
            Value::Null
        },
        "terminal_at": terminal_at.to_rfc3339_opts(chrono::SecondsFormat::Secs, true),
        "attributed_at": terminal_at.to_rfc3339_opts(chrono::SecondsFormat::Secs, true),
        "outcome_hash": "0".repeat(64)
    });
    crate::autonomous::set_hash(
        &mut contract,
        "outcome_hash",
        "outcome",
        "phoenix.outcome.v1",
    )
    .map_err(|_| StoreError::Data)?;
    let outcome_hash = contract
        .get("outcome_hash")
        .and_then(Value::as_str)
        .ok_or(StoreError::Data)?;
    sqlx::query(
        "INSERT INTO live_canary.autonomous_outcome_attributions(
            candidate_id, schema_version, outcome_class, transaction_hash,
            block_number, receipt_status, predicted_gross_profit,
            predicted_total_cost, conservative_predicted_net_pnl,
            realized_gross_profit, actual_gas_cost, actual_ordering_cost,
            realized_chain_net_pnl, allocated_infrastructure_cost,
            realized_business_net_pnl, terminal_at, attributed_at,
            outcome_hash, outcome_contract, nonce, submission_channel,
            submitted_at, gas_used, effective_gas_price, actual_l1_cost,
            actual_flash_premium, prediction_error, failure_reason
         ) VALUES (
            $1, 'phoenix.outcome.v1', $2, $3, $4::numeric, $5,
            $6::numeric, $7::numeric, $8::numeric, $9::numeric,
            $10::numeric, 0, $11::numeric, 0, $12::numeric, $13, $13,
            $14, $15, $16::numeric, 'standard_rpc', $17, $18::numeric,
            $19::numeric, $20::numeric, $21::numeric, $22::numeric, $23
         )",
    )
    .bind(candidate_id)
    .bind(outcome_class)
    .bind(tx_hash)
    .bind(outcome.block_number.to_string())
    .bind(i16::try_from(outcome.receipt_status).map_err(|_| StoreError::Data)?)
    .bind(&predicted_gross_profit)
    .bind(&predicted_total_cost)
    .bind(&conservative_predicted_net_pnl)
    .bind(realized_gross_profit.to_string())
    .bind(actual_gas_cost.to_string())
    .bind(outcome.net_pnl_wei.to_string())
    .bind(outcome.net_pnl_wei.to_string())
    .bind(terminal_at)
    .bind(outcome_hash)
    .bind(Json(&contract))
    .bind(nonce.to_string())
    .bind(submitted_at)
    .bind(outcome.gas_used.to_string())
    .bind(outcome.effective_gas_price.to_string())
    .bind(actual_l1_cost.to_string())
    .bind(outcome.settlement.premium.to_string())
    .bind(prediction_error.to_string())
    .bind(if status == AttemptStatus::Reverted {
        Some("transaction_reverted")
    } else {
        None
    })
    .execute(&mut **transaction)
    .await
    .map_err(StoreError::from)?;
    Ok(())
}

async fn apply_autonomous_risk_feedback(
    transaction: &mut Transaction<'_, Postgres>,
    request_id: Uuid,
    terminal_at: DateTime<Utc>,
) -> Result<(), StoreError> {
    let route_fingerprint: String = sqlx::query_scalar(
        "SELECT route_fingerprint
         FROM live_canary.execution_requests
         WHERE id = $1",
    )
    .bind(request_id)
    .fetch_one(&mut **transaction)
    .await
    .map_err(StoreError::from)?;
    let global: (String, String) = sqlx::query_as(
        "WITH bounds AS (
            SELECT (
                date_trunc('day', $1::timestamptz AT TIME ZONE 'UTC')
                AT TIME ZONE 'UTC'
            ) AS start_at
         )
         SELECT
            COALESCE(SUM(
                CASE WHEN o.net_pnl_wei < 0 THEN -o.net_pnl_wei ELSE 0 END
            ), 0)::text,
            c.daily_loss_limit::text
         FROM live_canary.autonomous_global_control c
         CROSS JOIN bounds
         LEFT JOIN live_canary.execution_outcomes o
           ON o.recorded_at >= bounds.start_at
          AND o.recorded_at < bounds.start_at + interval '1 day'
         WHERE c.singleton
         GROUP BY c.daily_loss_limit",
    )
    .bind(terminal_at)
    .fetch_one(&mut **transaction)
    .await
    .map_err(StoreError::from)?;
    let global_loss = global.0.parse::<u128>().map_err(|_| StoreError::Data)?;
    let global_limit = global.1.parse::<u128>().map_err(|_| StoreError::Data)?;

    let policy: Value =
        serde_json::from_str(include_str!("../../config/phoenix-route-policy-v1.json"))
            .map_err(|_| StoreError::Data)?;
    let route_limit = policy
        .get("per_route_daily_loss")
        .and_then(Value::as_str)
        .ok_or(StoreError::Data)?
        .parse::<u128>()
        .map_err(|_| StoreError::Data)?;
    let consecutive_limit = policy
        .get("maximum_consecutive_losses")
        .and_then(Value::as_u64)
        .ok_or(StoreError::Data)?;
    let route_loss: String = sqlx::query_scalar(
        "WITH bounds AS (
            SELECT (
                date_trunc('day', $2::timestamptz AT TIME ZONE 'UTC')
                AT TIME ZONE 'UTC'
            ) AS start_at
         )
         SELECT COALESCE(SUM(
             CASE WHEN o.net_pnl_wei < 0 THEN -o.net_pnl_wei ELSE 0 END
         ), 0)::text
         FROM live_canary.execution_outcomes o
         JOIN live_canary.execution_requests r ON r.id = o.request_id
         CROSS JOIN bounds
         WHERE r.route_fingerprint = $1
           AND o.recorded_at >= bounds.start_at
           AND o.recorded_at < bounds.start_at + interval '1 day'",
    )
    .bind(&route_fingerprint)
    .bind(terminal_at)
    .fetch_one(&mut **transaction)
    .await
    .map_err(StoreError::from)?;
    let recent: Vec<String> = sqlx::query_scalar(
        "SELECT o.net_pnl_wei::text
         FROM live_canary.execution_outcomes o
         JOIN live_canary.execution_requests r ON r.id = o.request_id
         WHERE r.route_fingerprint = $1
         ORDER BY o.recorded_at DESC, o.request_id DESC
         LIMIT 1000",
    )
    .bind(&route_fingerprint)
    .fetch_all(&mut **transaction)
    .await
    .map_err(StoreError::from)?;
    let route_loss = route_loss.parse::<u128>().map_err(|_| StoreError::Data)?;
    let consecutive_losses = recent
        .iter()
        .take_while(|value| value.starts_with('-'))
        .count() as u64;

    if route_loss >= route_limit || consecutive_losses >= consecutive_limit {
        let reason = if route_loss >= route_limit {
            "route_daily_loss_budget"
        } else {
            "maximum_consecutive_losses"
        };
        sqlx::query(
            "UPDATE live_canary.autonomous_route_controls
             SET enabled = false, kill_switch = true, disarm_reason = $2,
                 control_hash = NULL, control_contract = NULL, updated_at = $3
             WHERE route_fingerprint = $1",
        )
        .bind(&route_fingerprint)
        .bind(reason)
        .bind(terminal_at)
        .execute(&mut **transaction)
        .await
        .map_err(StoreError::from)?;
    }
    if global_loss >= global_limit {
        sqlx::query(
            "UPDATE live_canary.control
             SET armed = false, kill_switch = true,
                 disarm_reason = 'daily_loss_budget', updated_at = $1
             WHERE singleton",
        )
        .bind(terminal_at)
        .execute(&mut **transaction)
        .await
        .map_err(StoreError::from)?;
        sqlx::query(
            "UPDATE live_canary.autonomous_global_control
             SET armed = false, kill_switch = true, execution_mode = 'disarmed',
                 disarm_reason = 'daily_loss_budget', control_hash = NULL,
                 control_contract = NULL, updated_at = $1
             WHERE singleton",
        )
        .bind(terminal_at)
        .execute(&mut **transaction)
        .await
        .map_err(StoreError::from)?;
        sqlx::query(
            "UPDATE live_canary.autonomous_candidates
             SET status = 'disarmed', updated_at = $1
             WHERE status IN (
                 'materialized', 'approval_pending', 'approved',
                 'request_materialized', 'claimed', 'signed'
             )",
        )
        .bind(terminal_at)
        .execute(&mut **transaction)
        .await
        .map_err(StoreError::from)?;
    }
    Ok(())
}

async fn update_candidate_status_for_request(
    transaction: &mut Transaction<'_, Postgres>,
    request_id: Uuid,
    from: &'static str,
    to: &'static str,
) -> Result<(), StoreError> {
    let updated = sqlx::query(
        "UPDATE live_canary.autonomous_candidates
         SET status = $3, updated_at = now()
         WHERE execution_request_id = $1 AND status = $2",
    )
    .bind(request_id)
    .bind(from)
    .bind(to)
    .execute(&mut **transaction)
    .await
    .map_err(StoreError::from)?;
    if updated.rows_affected() > 1 {
        return Err(StoreError::Invariant);
    }
    Ok(())
}

pub(crate) fn request_select() -> &'static str {
    "SELECT
        r.id,
        r.opportunity_id,
        r.schema_version,
        r.chain_id,
        r.route_id,
        r.route_fingerprint,
        r.selected_size::text AS selected_size,
        r.token_path,
        r.origin_router,
        r.executor_address,
        r.executor_code_hash,
        r.calldata_hash,
        r.simulation_result_hash,
        r.plan_hash,
        r.pinned_block_number::text AS pinned_block_number,
        r.pinned_block_hash,
        r.flash_asset,
        r.flash_amount::text AS flash_amount,
        r.maximum_input_amount::text AS maximum_input_amount,
        r.minimum_profit::text AS minimum_profit,
        r.expected_profit::text AS expected_profit,
        r.deadline,
        r.legs,
        r.gas_limit,
        r.max_fee_per_gas::text AS max_fee_per_gas,
        r.max_priority_fee_per_gas::text AS max_priority_fee_per_gas,
        r.approved_by,
        r.approved_at,
        r.approval_deadline,
        r.policy_version,
        r.approval_digest
     FROM live_canary.execution_requests r"
}

fn active_attempt_select() -> String {
    format!(
        "{}, r.status AS request_status, a.status AS attempt_status, a.nonce::text AS attempt_nonce,
         a.tx_hash AS attempt_tx_hash, a.submitted_at AS attempt_submitted_at
         FROM live_canary.execution_attempts a
         JOIN live_canary.execution_requests r ON r.id = a.request_id",
        request_select()
            .strip_suffix(" FROM live_canary.execution_requests r")
            .expect("static request query suffix")
    )
}

pub(crate) fn decode_request(row: &sqlx::postgres::PgRow) -> Result<ExecutionRequest, StoreError> {
    let legs: Json<Vec<ExecutionLeg>> = row.try_get("legs").map_err(StoreError::from)?;
    let token_path: Json<Vec<String>> = row.try_get("token_path").map_err(StoreError::from)?;
    RawExecutionRequest {
        id: row.try_get("id").map_err(StoreError::from)?,
        opportunity_id: row.try_get("opportunity_id").map_err(StoreError::from)?,
        schema_version: row.try_get("schema_version").map_err(StoreError::from)?,
        chain_id: row.try_get("chain_id").map_err(StoreError::from)?,
        route_id: row.try_get("route_id").map_err(StoreError::from)?,
        route_fingerprint: row.try_get("route_fingerprint").map_err(StoreError::from)?,
        selected_size: row.try_get("selected_size").map_err(StoreError::from)?,
        token_path: token_path.0,
        origin_router: row.try_get("origin_router").map_err(StoreError::from)?,
        executor_address: row.try_get("executor_address").map_err(StoreError::from)?,
        executor_code_hash: row
            .try_get("executor_code_hash")
            .map_err(StoreError::from)?,
        calldata_hash: row.try_get("calldata_hash").map_err(StoreError::from)?,
        simulation_result_hash: row
            .try_get("simulation_result_hash")
            .map_err(StoreError::from)?,
        plan_hash: row.try_get("plan_hash").map_err(StoreError::from)?,
        pinned_block_number: row
            .try_get::<String, _>("pinned_block_number")
            .map_err(StoreError::from)?
            .parse::<i64>()
            .map_err(|_| StoreError::Data)?,
        pinned_block_hash: row.try_get("pinned_block_hash").map_err(StoreError::from)?,
        flash_asset: row.try_get("flash_asset").map_err(StoreError::from)?,
        flash_amount: row.try_get("flash_amount").map_err(StoreError::from)?,
        maximum_input_amount: row
            .try_get("maximum_input_amount")
            .map_err(StoreError::from)?,
        minimum_profit: row.try_get("minimum_profit").map_err(StoreError::from)?,
        expected_profit: row.try_get("expected_profit").map_err(StoreError::from)?,
        deadline: row.try_get("deadline").map_err(StoreError::from)?,
        legs: legs.0,
        gas_limit: row.try_get("gas_limit").map_err(StoreError::from)?,
        max_fee_per_gas: row.try_get("max_fee_per_gas").map_err(StoreError::from)?,
        max_priority_fee_per_gas: row
            .try_get("max_priority_fee_per_gas")
            .map_err(StoreError::from)?,
        approved_by: row.try_get("approved_by").map_err(StoreError::from)?,
        approved_at: row.try_get("approved_at").map_err(StoreError::from)?,
        approval_deadline: row.try_get("approval_deadline").map_err(StoreError::from)?,
        policy_version: row.try_get("policy_version").map_err(StoreError::from)?,
        approval_digest: row.try_get("approval_digest").map_err(StoreError::from)?,
    }
    .validate()
    .map_err(|_| StoreError::Data)
}

fn decode_active_attempt(row: &sqlx::postgres::PgRow) -> Result<ActiveAttempt, StoreError> {
    let status: String = row.try_get("attempt_status").map_err(StoreError::from)?;
    let request_status: String = row.try_get("request_status").map_err(StoreError::from)?;
    if request_status != status {
        return Err(StoreError::Invariant);
    }
    let status = match status.as_str() {
        "claimed" => AttemptStatus::Claimed,
        "nonce_allocated" => AttemptStatus::NonceAllocated,
        "submission_unknown" => AttemptStatus::SubmissionUnknown,
        "pending" => AttemptStatus::Pending,
        "timed_out" => AttemptStatus::TimedOut,
        _ => return Err(StoreError::Data),
    };
    let nonce = row
        .try_get::<Option<String>, _>("attempt_nonce")
        .map_err(StoreError::from)?
        .map(|value| value.parse::<u64>().map_err(|_| StoreError::Data))
        .transpose()?;
    let tx_hash = row
        .try_get::<Option<String>, _>("attempt_tx_hash")
        .map_err(StoreError::from)?
        .map(|value| TransactionHash::parse(&value).map_err(|_| StoreError::Data))
        .transpose()?;
    Ok(ActiveAttempt {
        request: decode_request(row)?,
        status,
        nonce,
        tx_hash,
        submitted_at: row
            .try_get("attempt_submitted_at")
            .map_err(StoreError::from)?,
    })
}

#[derive(Clone, Copy, Debug, Error, PartialEq, Eq)]
pub enum StoreError {
    #[error("live executor database configuration failed")]
    Configuration,
    #[error("live executor database connection failed")]
    Connection,
    #[error("live executor schema is unavailable or invalid")]
    Schema,
    #[error("live executor database data is invalid")]
    Data,
    #[error("live executor database invariant failed")]
    Invariant,
}

impl From<sqlx::Error> for StoreError {
    fn from(error: sqlx::Error) -> Self {
        match error {
            sqlx::Error::Configuration(_) => Self::Configuration,
            sqlx::Error::Io(_)
            | sqlx::Error::Tls(_)
            | sqlx::Error::PoolTimedOut
            | sqlx::Error::PoolClosed
            | sqlx::Error::WorkerCrashed => Self::Connection,
            sqlx::Error::RowNotFound => Self::Schema,
            sqlx::Error::ColumnNotFound(_)
            | sqlx::Error::ColumnDecode { .. }
            | sqlx::Error::Decode(_) => Self::Data,
            sqlx::Error::Database(_) => Self::Invariant,
            _ => Self::Invariant,
        }
    }
}
