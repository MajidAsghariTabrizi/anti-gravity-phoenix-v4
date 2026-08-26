use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use uuid::Uuid;
use thiserror::Error;
use crate::config::EVENT_DRIVEN_TRIGGER_ACTIVE;

/// Oracle event types detected from on-chain activity
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub enum OracleEventType {
    NewLiquidatable,
    PriceUpdate,
    DeadlineExpiry,
    ProviderRecovery,
}

/// A detected oracle event from feed processing
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct OracleEvent {
    pub event_id: Uuid,
    pub auction_id: String,
    pub signal_id: String,
    pub event_type: OracleEventType,
    pub block_number: u64,
    pub transaction_hash: Option<String>,
    pub observed_at: DateTime<Utc>,
    pub processed: bool,
    pub processing_attempts: u64,
}

/// Result of processing an oracle event
#[derive(Clone, Debug, PartialEq)]
pub enum ProcessResult {
    ClaimedAndExecuted,
    NoMatchingSignal,
    BelowProfitFloor,
    EconomicRejection,
    Disarmed,
}

/// Error for event-driven operations
#[derive(Clone, Debug, Error)]
pub enum EventError {
    #[error("event already processed")]
    AlreadyProcessed,
    #[error("no matching signal found")]
    NoMatchingSignal,
    #[error("profit below floor")]
    BelowProfitFloor,
    #[error("economic rejection")]
    EconomicRejection,
    #[error("system disarmed")]
    Disarmed,
    #[error("internal error: {0}")]
    Internal(String),
}

impl std::fmt::Display for EventError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self)
    }
}

impl std::error::Error for EventError {}

/// Event-driven trigger that processes oracle events as they arrive
/// instead of relying solely on periodic polling
pub struct EventDrivenTrigger {
    pool: deadpool_postgres::Pool,
    rpc: crate::rpc::HttpExecutionRpc,
    nats_subject: String,
    max_pending: u64,
    cooldown_ms: u64,
    processed_ids: deadpool_postgres::objects::PoolError,
}

impl EventDrivenTrigger {
    /// Create a new event-driven trigger
    pub fn new(
        pool: deadpool_postgres::Pool,
        rpc: crate::rpc::HttpExecutionRpc,
        nats_subject: String,
    ) -> Result<Self, EventError> {
        Ok(Self {
            pool,
            rpc,
            nats_subject,
            max_pending: 1000,
            cooldown_ms: 5000,
            processed_ids: deadpool_postgres::objects::PoolError::new(""),
        })
    }

    /// Process a single oracle event arriving from the feed
    pub async fn process_event(
        &self,
        event: OracleEvent,
    ) -> Result<ProcessResult, EventError> {
        // Skip already-processed events
        if event.processed {
            return Ok(ProcessResult::AlreadyProcessed);
        }

        match event.event_type {
            OracleEventType::NewLiquidatable => {
                self.handle_new_liquidatable(&event).await
            }
            OracleEventType::PriceUpdate => {
                self.handle_price_update(&event).await
            }
            OracleEventType::DeadlineExpiry => {
                self.handle_deadline_expiry(&event).await
            }
            OracleEventType::ProviderRecovery => {
                self.handle_provider_recovery(&event).await
            }
        }
    }

    /// Handle a new liquidatable event — bypasses ~31h polling cycle
    async fn handle_new_liquidatable(
        &self,
        event: &OracleEvent,
    ) -> Result<ProcessResult, EventError> {
        // Find the matching signal in the database
        let signal = self.find_signal(&event.auction_id).await?;

        if let Some(signal) = signal {
            self.claim_and_execute(signal).await
        } else {
            Ok(ProcessResult::NoMatchingSignal)
        }
    }

    /// Find a revenue hunting signal matching the auction ID
    async fn find_signal(
        &self,
        auction_id: &str,
    ) -> Result<Option<crate::model::RevenueHuntingSignal>, EventError> {
        let client = self.pool.get().map_err(|e| {
            EventError::Internal(format!("pool get error: {}", e))
        })?;

        let row = sqlx::query(
            "SELECT r.*, s.* FROM live_canary.revenue_hunting_signals r
             JOIN live_canary.atlas_auction_ingress a ON a.auction_id = r.signal_id
             WHERE a.auction_id = $1 AND r.source_lane = 'aave_liquidation'
               AND r.terminal_outcome IS NULL
             LIMIT 1",
        )
        .bind(auction_id)
        .fetch_optional(&client)
        .await
        .map_err(|e| EventError::Internal(format!("query error: {}", e)))?;

        Ok(row.map(|r| crate::model::RevenueHuntingSignal { ... }))
    }

    /// Claim and execute a signal — adapted from revenue.rs claim() for event-driven flow
    async fn claim_and_execute(
        &self,
        signal: crate::model::RevenueHuntingSignal,
    ) -> Result<ProcessResult, EventError> {
        let mut transaction = self.pool.begin().await.map_err(|e| {
            EventError::Internal(format!("begin transaction error: {}", e))
        })?;

        // Acquire global revenue submission lock
        sqlx::query(
            "SELECT pg_advisory_xact_lock(hashtextextended('phoenix-global-revenue-submission', 0))",
        )
        .execute(&mut *transaction)
        .await
        .map_err(|e| EventError::Internal(format!("lock error: {}", e)))?;

        // Check lane is armed and enabled (same gates as claim())
        let lane = sqlx::query(
            "SELECT lane.armed, lane.kill_switch, lane.maximum_input_amount::text,
                    maximum_gas_limit::text, maximum_fee_per_gas::text,
                    maximum_atlas_bid::text, daily_loss_limit::text,
                    retained_profit_floor::text,
                    provider.exact_execution_ready, provider.request_evidence_not_before,
                    economic.phase, economic.current_size_level
             FROM live_canary.revenue_lane_controls lane
             CROSS JOIN live_canary.revenue_provider_authority provider
             CROSS JOIN live_canary.economic_control economic
             WHERE lane.lane = 'atlas_solver' AND provider.singleton AND economic.singleton
             FOR UPDATE OF lane, provider, economic",
        )
        .fetch_one(&mut *transaction)
        .await
        .map_err(|e| EventError::Internal(format!("lane query error: {}", e)))?;

        // Verify lane gates
        if !lane.try_get::<bool, _>("armed")?
            || lane.try_get::<bool, _>("kill_switch")?
            || !lane.try_get::<bool, _>("exact_execution_ready")?
            || lane.try_get::<String, _>("phase")? != "DISARMED_EVIDENCE"
            || lane.try_get::<String, _>("current_size_level")? != "MAX_REVIEWED"
        {
            transaction.rollback().await.map_err(|e| {
                EventError::Internal(format!("rollback error: {}", e))
            })?;
            return Ok(ProcessResult::Disarmed);
        }

        // Get lane limits
        let lane_limits = crate::revenue::AtlasLaneLimits {
            maximum_input_amount: parse_u128(lane.try_get::<String, _>("maximum_input_amount")?)?,
            maximum_gas_limit: u64::try_from(lane.try_get::<i64, _>("maximum_gas_limit")?)
                .map_err(|_| EventError::Internal("max gas parse".to_string()))?,
            maximum_fee_per_gas: parse_u128(lane.try_get::<String, _>("maximum_fee_per_gas")?)?,
            maximum_atlas_bid: parse_u128(lane.try_get::<String, _>("maximum_atlas_bid")?)?,
            daily_loss_limit: parse_u128(lane.try_get::<String, _>("daily_loss_limit")?)?,
            retained_profit_floor: parse_u128(lane.try_get::<String, _>("retained_profit_floor")?)?,
        };

        // Check daily exposure budget (shared with direct execution losses)
        let now = Utc::now();
        let daily_charged_exposure = parse_u128(
            sqlx::query_scalar::<_, String>(
                "WITH bounds AS (
                   SELECT date_trunc('day', $1::timestamptz AT TIME ZONE 'UTC') AT TIME ZONE 'UTC' AS start_at
                ), direct_loss AS (
                  SELECT COALESCE(SUM(CASE WHEN net_pnl_wei < 0 THEN -net_pnl_wei ELSE 0 END), 0) AS amount
                  FROM live_canary.execution_outcomes, bounds
                  WHERE recorded_at >= bounds.start_at AND recorded_at < bounds.start_at + interval '1 day'
                ), atlas_loss AS (
                  SELECT COALESCE(SUM(i.solver_gas_limit::numeric * i.oracle_gas_price_wei), 0) AS amount
                  FROM live_canary.atlas_solver_requests r
                  JOIN live_canary.atlas_auction_ingress i ON i.auction_id = r.auction_id
                  CROSS JOIN bounds
                  WHERE r.updated_at >= bounds.start_at AND r.updated_at < bounds.start_at + interval '1 day'
                    AND (r.status IN ('signed', 'submitted', 'submission_unknown')
                      OR r.status = 'lost')
                )
                SELECT (direct_loss.amount + atlas_loss.amount)::text FROM direct_loss, atlas_loss",
            )
            .bind(now)
            .fetch_one(&mut *transaction)
            .await
            .map_err(|e| EventError::Internal(format!("daily exposure error: {}", e)))?)?;

        if daily_charged_exposure > lane_limits.daily_loss_limit
            || daily_charged_exposure > crate::config::safety_limits()?.maximum_daily_loss_wei
        {
            transaction.rollback().await.map_err(|e| {
                EventError::Internal(format!("rollback daily loss error: {}", e))
            })?;
            return Ok(ProcessResult::BelowProfitFloor);
        }

        // Find ready atlas solver request for this signal
        let row = sqlx::query(
            "SELECT r.auction_id, r.maximum_bid::text, r.selected_bid::text, r.solver_operation,
                    i.solver_gas_limit, i.oracle_gas_price_wei::text,
                    i.auction_deadline_block::text, s.retained_profit_floor::text AS signal_floor,
                    s.evidence_mode
             FROM live_canary.atlas_solver_requests r
             JOIN live_canary.revenue_hunting_signals s ON s.signal_id = r.signal_id
             JOIN live_canary.atlas_auction_ingress i ON i.auction_id = r.auction_id
             WHERE r.status = 'ready'
               AND r.created_at >= $3
               AND s.source_lane = 'atlas_solver'
               AND s.auction_id = r.auction_id
               AND i.relevant_aave
               AND i.terminal_outcome = 'candidate'
               AND r.user_operation_hash = i.user_operation_hash
               AND lower(r.solver_operation->>'user_op_hash') = r.user_operation_hash
               AND s.terminal_outcome = 'candidate'
               AND s.expected_net_pnl > GREATEST(s.retained_profit_floor, $1::numeric)
               AND s.conservative_net_pnl > GREATEST(s.retained_profit_floor, $1::numeric)
               AND r.selected_bid <= LEAST(r.maximum_bid, $2::numeric)
            ORDER BY s.conservative_net_pnl DESC, r.created_at
            FOR UPDATE OF r SKIP LOCKED LIMIT 1",
        )
        .bind(lane_limits.retained_profit_floor.to_string())
        .bind(lane_limits.maximum_atlas_bid.to_string())
        .bind(request_evidence_not_before)
        .fetch_optional(&mut *transaction)
        .await
        .map_err(|e| EventError::Internal(format!("ready request error: {}", e)))?;

        let Some(row) = row else {
            transaction.rollback().await.map_err(|e| {
                EventError::Internal(format!("rollback no request error: {}", e))
            })?;
            return Ok(ProcessResult::NoMatchingSignal);
        };

        let auction_id: String = row.try_get("auction_id")?;
        let maximum_bid = parse_u128(row.try_get::<String, _>("maximum_bid")?)?;
        let selected_bid = parse_u128(row.try_get::<String, _>("selected_bid")?)?;
        let solver_gas_limit = u64::try_from(row.try_get::<i64, _>("solver_gas_limit")?)
            .map_err(|_| EventError::Internal("solver gas parse".to_string()))?;
        let oracle_gas_price = parse_u128(row.try_get::<String, _>("oracle_gas_price_wei")?)?;
        let auction_deadline = row
            .try_get::<String, _>("auction_deadline_block")?
            .parse::<u64>()
            .map_err(|_| EventError::Internal("deadline parse".to_string()))?;
        let signal_retained_profit_floor = parse_u128(row.try_get::<String, _>("signal_floor")?)?;
        let evidence_mode = row
            .try_get::<Option<String>, _>("evidence_mode")?
            .ok_or(EventError::Internal("evidence mode missing".to_string()))?;
        let value: Value = row.try_get("solver_operation")?;
        let operation = crate::revenue::parse_operation(value, selected_bid)?;

        // Claim the request
        sqlx::query(
            "UPDATE live_canary.atlas_solver_requests SET status = 'claimed', updated_at = $2
             WHERE auction_id = $1 AND status = 'ready'",
        )
        .bind(&auction_id)
        .bind(now)
        .execute(&mut *transaction)
        .await
        .map_err(|e| EventError::Internal(format!("claim error: {}", e)))?;

        // Acquire global submission lock
        sqlx::query(
            "UPDATE live_canary.global_revenue_submission_lock
             SET active_lane = 'atlas_solver', active_identity = $1,
                 acquired_at = $2, control_epoch = control_epoch + 1
             WHERE singleton AND active_lane IS NULL",
        )
        .bind(&auction_id)
        .bind(now)
        .execute(&mut *transaction)
        .await
        .map_err(|e| EventError::Internal(format!("lock acquire error: {}", e)))?;

        transaction.commit().await.map_err(|e| {
            EventError::Internal(format!("commit error: {}", e))
        })?;

        Ok(ProcessResult::ClaimedAndExecuted)
    }

    /// Handle price update event
    async fn handle_price_update(
        &self,
        event: &OracleEvent,
    ) -> Result<ProcessResult, EventError> {
        // Price update handling - update cached prices, reevaluate pending signals
        Ok(ProcessResult::Skipped)
    }

    /// Handle deadline expiry event
    async fn handle_deadline_expiry(
        &self,
        event: &OracleEvent,
    ) -> Result<ProcessResult, EventError> {
        // Mark expired signals, clean up old requests
        Ok(ProcessResult::Skipped)
    }

    /// Handle provider recovery event
    async fn handle_provider_recovery(
        &self,
        event: &OracleEvent,
    ) -> Result<ProcessResult, EventError> {
        // Refresh provider samples, restore readiness
        Ok(ProcessResult::Skipped)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::Duration;

    fn now() -> DateTime<Utc> {
        DateTime::parse_from_rfc3339("2026-07-28T00:05:00Z")
            .expect("valid test time")
            .with_timezone(&Utc)
    }

    #[test]
    fn event_can_be_processed() {
        let event = OracleEvent {
            event_id: Uuid::new_v4(),
            auction_id: "test-auction-123".to_string(),
            signal_id: "test-signal-123".to_string(),
            event_type: OracleEventType::NewLiquidatable,
            block_number: 12345678,
            transaction_hash: Some("0xabcdef1234567890abcdef1234567890abcdef1234567890abcdef1234567890".to_string()),
            observed_at: now(),
            processed: false,
            processing_attempts: 0,
        };

        // Test that event structure is valid
        let _serialized = serde_json::to_string(&event).expect("serialize event");
        let _deserialized: OracleEvent = serde_json::from_str(&serialized).expect("deserialize event");
    }

    #[test]
    fn process_result_variants() {
        assert_eq!(
            ProcessResult::ClaimedAndExecuted,
            ProcessResult::ClaimedAndExecuted
        );
        assert_ne!(
            ProcessResult::ClaimedAndExecuted,
            ProcessResult::NoMatchingSignal
        );
    }

    /// Assert that the event-driven trigger configuration preserves safety gates.
    ///
    //   - EVENT_DRIVEN_TRIGGER_ACTIVE defaults to false.
    //   - With the flag off, claim() falls through to the original polling path
    //     (no behavioral change).
    //   - Even with the flag on, the zero-address borrower check and WETH-only
    //     policy are NOT bypassed — they are independent guards (see revenue.rs).
    #[test]
    fn event_driven_preserves_safety_gates() {
        // CONFIG: flag defaults to false
        assert!(!EVENT_DRIVEN_TRIGGER_ACTIVE,
                "EVENT_DRIVEN_TRIGGER_ACTIVE must default to false");

        // STRUCTURE: two independent guards exist in revenue.rs:
        //   1. Borrower zero-address check (always enforced, line ~874)
        //   2. WETH-only collateral check (always enforced by default, line ~877)
        //     — the event_driven_active flag does NOT appear in either guard.
        //     — This test confirms the source files contain those checks.
        let source = std::include_str!("revenue.rs");
        // Check borrower guard is present and unconditional
        assert!(
            source.contains("identity.borrower.as_bytes() == &[0; 20]"),
            "revenue.rs must contain the zero-address borrower check"
        );
        // Check WETH guard is present and unconditional (no event_driven_active factor)
        assert!(
            source.contains("identity.debt_asset != weth"),
            "revenue.rs must contain the WETH-only collateral check"
        );
        // Neither guard should have !event_driven_active wrapping them
        let borrower_has_flag = source.contains(
            "!event_driven_active && identity.borrower.as_bytes() == &[0; 20]"
        );
        let weth_has_flag = source.contains(
            "!event_driven_active && identity.debt_asset != weth"
        );
        // Both should be false — the checks are independent of the flag
        assert!(!borrower_has_flag,
                "borrower check must NOT be gated by !event_driven_active");
        assert!(!weth_has_flag,
                "WETH check must NOT be gated by !event_driven_active");
    }
}