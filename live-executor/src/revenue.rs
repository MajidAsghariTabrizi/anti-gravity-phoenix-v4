use crate::aave::{AaveLiquidationIdentity, AaveLiquidationRequest};
use crate::atlas::{AtlasError, AtlasGateway, AtlasSolution, AtlasSolverOperation};
use crate::config::SafetyLimits;
use crate::model::{CanonicalAddress, ValidatedLeg};
use crate::rpc::{ExecutionRpc, HttpExecutionRpc, IndexedRpcLog, RpcError};
use crate::signer::TransactionSigner;
use crate::{
    ARBITRUM_ATLAS_DAPP_CONTROL_ADDRESS, ARBITRUM_ATLAS_V1_6_4_ADDRESS,
    ARBITRUM_NATIVE_USDC_ADDRESS, ARBITRUM_UNISWAP_V3_FACTORY_ADDRESS, ARBITRUM_WETH_ADDRESS,
    CURRENT_ROUTE_POOL_3000_ADDRESS, CURRENT_ROUTE_POOL_500_ADDRESS,
};
use chrono::{DateTime, Utc};
use ethabi::{ParamType, Token};
use primitive_types::U256;
use serde::Deserialize;
use serde_json::Value;
use sha2::{Digest, Sha256};
use sqlx::{PgPool, Row};
use std::io::Cursor;
use thiserror::Error;

const ACTIVE_ATTEMPT_STATUSES: &str =
    "'claimed','nonce_allocated','submission_unknown','pending','timed_out'";
const ATLAS_ACTUAL_PATH_EVIDENCE_MODE: &str = "DUAL_PROVIDER_ATLAS_CALLBACK_FORK_VERIFIED";
const ATLAS_SUBMISSION_MARGIN_SECONDS: u64 = 15;

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum AtlasExecutionState {
    Disarmed,
    Idle,
    Submitted { auction_id: String },
    Rejected { auction_id: String },
    SubmissionUnknown { auction_id: String },
    Reconciled { auction_id: String },
}

#[derive(Clone)]
pub struct AtlasRevenueExecutor {
    pool: PgPool,
    gateway: AtlasGateway,
    signer: TransactionSigner,
    expected_solver: CanonicalAddress,
    expected_code_hash: String,
    rpc: HttpExecutionRpc,
    safety_limits: SafetyLimits,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct PreparedOperation {
    from: String,
    to: String,
    value: String,
    gas: u64,
    max_fee_per_gas: String,
    deadline: u64,
    solver: String,
    control: String,
    user_op_hash: String,
    bid_token: Option<String>,
    bid_amount: String,
    data: String,
}

#[derive(Clone)]
struct ClaimedAtlasRequest {
    auction_id: String,
    maximum_bid: u128,
    solver_gas_limit: u64,
    oracle_gas_price: u128,
    auction_deadline: u64,
    signal_retained_profit_floor: u128,
    daily_charged_exposure: u128,
    evidence_mode: String,
    lane_limits: AtlasLaneLimits,
    operation: AtlasSolverOperation,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct AtlasLaneLimits {
    maximum_input_amount: u128,
    maximum_gas_limit: u64,
    maximum_fee_per_gas: u128,
    maximum_atlas_bid: u128,
    daily_loss_limit: u128,
    retained_profit_floor: u128,
}

struct ActiveAtlasRequest {
    auction_id: String,
    retained_profit_floor: u128,
    operation: AtlasSolverOperation,
}

enum AtlasOutcome {
    Pending,
    Expired,
    Failed {
        transaction_hash: String,
    },
    Reconciled {
        transaction_hash: String,
        block_number: u64,
        settled_bid: u128,
        executor_balance_before: u128,
        executor_balance_after: u128,
        solver_bond_before: u128,
        solver_bond_after: u128,
        realized_net_pnl: i128,
        evidence_hash: String,
    },
}

impl AtlasRevenueExecutor {
    pub fn new(
        pool: PgPool,
        signer: TransactionSigner,
        expected_solver: CanonicalAddress,
        expected_code_hash: String,
        rpc: HttpExecutionRpc,
        safety_limits: SafetyLimits,
    ) -> Self {
        Self {
            pool,
            gateway: AtlasGateway,
            signer,
            expected_solver,
            expected_code_hash,
            rpc,
            safety_limits,
        }
    }

    pub async fn step(&self, now: DateTime<Utc>) -> Result<AtlasExecutionState, RevenueError> {
        if let Some(active) = self.active_request().await? {
            let auction_id = active.auction_id.clone();
            let outcome = match self.reconcile_active(&active).await {
                Ok(outcome) => outcome,
                Err(error) => {
                    self.kill_unknown(&auction_id, "atlas_outcome_unreconciled")
                        .await?;
                    return Err(error);
                }
            };
            return self.persist_outcome(active, outcome).await;
        }
        let Some(claimed) = self.claim(now).await? else {
            return Ok(if self.lane_enabled().await? {
                AtlasExecutionState::Idle
            } else {
                AtlasExecutionState::Disarmed
            });
        };
        if let Err(error) = claimed.operation.validate(
            self.signer.address(),
            self.expected_solver,
            claimed.solver_gas_limit,
            claimed.oracle_gas_price,
            claimed.auction_deadline,
            claimed.maximum_bid,
        ) {
            self.reject_and_unlock(&claimed.auction_id, "operation_validation")
                .await?;
            return Err(RevenueError::Atlas(error));
        }
        let (identity, reviewed_legs) =
            match validate_aave_atlas_safety(&claimed, &self.safety_limits, now) {
                Ok(validated) => validated,
                Err(error) => {
                    self.reject_and_unlock(&claimed.auction_id, "aave_atlas_safety")
                        .await?;
                    return Err(error);
                }
            };
        match self
            .preflight_is_ready(&claimed, &identity, &reviewed_legs)
            .await
        {
            Ok(true) => {}
            Ok(false) => {
                self.reject_and_unlock(&claimed.auction_id, "atlas_pre_sign_preflight")
                    .await?;
                return Err(RevenueError::Data);
            }
            Err(error) => {
                self.reject_and_unlock(&claimed.auction_id, "atlas_pre_sign_preflight_error")
                    .await?;
                return Err(error);
            }
        }
        let solution = match AtlasSolution::new(
            claimed.auction_id.clone(),
            &claimed.operation,
            &self.signer,
        ) {
            Ok(solution) => solution,
            Err(error) => {
                self.kill_unknown(&claimed.auction_id, "signing_failure")
                    .await?;
                return Err(RevenueError::Atlas(error));
            }
        };
        sqlx::query(
            "UPDATE live_canary.atlas_solver_requests
             SET status = 'signed', updated_at = now()
             WHERE auction_id = $1 AND status = 'claimed'",
        )
        .bind(&claimed.auction_id)
        .execute(&self.pool)
        .await?;
        if !self.lane_enabled().await? {
            self.reject_and_unlock(&claimed.auction_id, "kill_switch_before_submission")
                .await?;
            return Ok(AtlasExecutionState::Disarmed);
        }
        match self
            .preflight_is_ready(&claimed, &identity, &reviewed_legs)
            .await
        {
            Ok(true) => {}
            Ok(false) => {
                self.reject_and_unlock(&claimed.auction_id, "atlas_pre_submit_preflight")
                    .await?;
                return Ok(AtlasExecutionState::Rejected {
                    auction_id: claimed.auction_id,
                });
            }
            Err(error) => {
                self.reject_and_unlock(&claimed.auction_id, "atlas_pre_submit_preflight_error")
                    .await?;
                return Err(error);
            }
        }
        match self.gateway.submit(&solution).await {
            Ok(receipt) => {
                sqlx::query(
                    "UPDATE live_canary.atlas_solver_requests
                     SET status = 'submitted', submission_response_hash = $2, updated_at = now()
                     WHERE auction_id = $1 AND status = 'signed'",
                )
                .bind(&claimed.auction_id)
                .bind(receipt.response_hash)
                .execute(&self.pool)
                .await?;
                Ok(AtlasExecutionState::Submitted {
                    auction_id: claimed.auction_id,
                })
            }
            Err(AtlasError::SubmissionRejected) => {
                self.reject_and_unlock(&claimed.auction_id, "gateway_rejected")
                    .await?;
                Ok(AtlasExecutionState::Rejected {
                    auction_id: claimed.auction_id,
                })
            }
            Err(error) => {
                self.kill_unknown(&claimed.auction_id, "atlas_submission_unknown")
                    .await?;
                let _ = error;
                Ok(AtlasExecutionState::SubmissionUnknown {
                    auction_id: claimed.auction_id,
                })
            }
        }
    }

    async fn active_request(&self) -> Result<Option<ActiveAtlasRequest>, RevenueError> {
        let row = sqlx::query(
            "SELECT r.auction_id, r.selected_bid::text, r.solver_operation,
                    s.retained_profit_floor::text
             FROM live_canary.atlas_solver_requests r
             JOIN live_canary.revenue_hunting_signals s ON s.signal_id = r.signal_id
             WHERE r.status IN ('submitted','submission_unknown')
             ORDER BY r.updated_at LIMIT 1",
        )
        .fetch_optional(&self.pool)
        .await?;
        let Some(row) = row else {
            return Ok(None);
        };
        let selected_bid = parse_u128(row.try_get::<String, _>("selected_bid")?)?;
        Ok(Some(ActiveAtlasRequest {
            auction_id: row.try_get("auction_id")?,
            retained_profit_floor: parse_u128(row.try_get::<String, _>("retained_profit_floor")?)?,
            operation: parse_operation(row.try_get("solver_operation")?, selected_bid)?,
        }))
    }

    async fn reconcile_active(
        &self,
        active: &ActiveAtlasRequest,
    ) -> Result<AtlasOutcome, RevenueError> {
        let atlas = CanonicalAddress::parse(ARBITRUM_ATLAS_V1_6_4_ADDRESS)
            .map_err(|_| RevenueError::Data)?;
        let control = CanonicalAddress::parse(ARBITRUM_ATLAS_DAPP_CONTROL_ADDRESS)
            .map_err(|_| RevenueError::Data)?;
        let latest = self.rpc.latest_block_number().await?;
        let from_block = active.operation.deadline.saturating_sub(448);
        let to_block = latest.min(active.operation.deadline.saturating_add(64));
        let topics = [
            solver_result_topic(),
            address_topic(active.operation.solver),
            address_topic(active.operation.from),
            address_topic(control),
        ];
        let logs = self
            .rpc
            .exact_logs(atlas, &topics, from_block.min(to_block), to_block)
            .await?;
        let mut matching = Vec::new();
        for log in logs {
            let Some(input) = self
                .rpc
                .transaction_input(log.transaction_hash, atlas)
                .await?
            else {
                continue;
            };
            if metacall_contains_operation(&input, &active.operation)? {
                matching.push(log);
            }
        }
        if matching.is_empty() {
            return Ok(if latest > active.operation.deadline.saturating_add(64) {
                AtlasOutcome::Expired
            } else {
                AtlasOutcome::Pending
            });
        }
        if matching.len() != 1 {
            return Err(RevenueError::Data);
        }
        let indexed = matching.pop().ok_or(RevenueError::Data)?;
        let (settled_bid, executed, success) = decode_solver_result(&indexed)?;
        let receipt = self
            .rpc
            .transaction_receipt(indexed.transaction_hash)
            .await?
            .ok_or(RevenueError::Data)?;
        if receipt.status != 1 || receipt.block_number != indexed.block_number {
            return Err(RevenueError::Data);
        }
        let transaction_hash = indexed.transaction_hash.to_string();
        if settled_bid != active.operation.bid_amount || !executed || !success {
            return Ok(AtlasOutcome::Failed { transaction_hash });
        }
        require_successful_metacall(&receipt.logs)?;
        let identity = AaveLiquidationRequest::decode_encoded_identity(&active.operation.data)
            .map_err(|_| RevenueError::Data)?;
        if identity.maximum_atlas_bid < settled_bid
            || identity.debt_asset.to_string() != ARBITRUM_WETH_ADDRESS
        {
            return Err(RevenueError::Data);
        }
        let settlement = identity
            .decode_settlement(self.expected_solver, &receipt.logs)
            .map_err(|_| RevenueError::Data)?;
        if settlement.atlas_bid != settled_bid {
            return Err(RevenueError::Data);
        }
        let prior_block = receipt
            .block_number
            .checked_sub(1)
            .ok_or(RevenueError::Data)?;
        let weth =
            CanonicalAddress::parse(ARBITRUM_WETH_ADDRESS).map_err(|_| RevenueError::Data)?;
        let executor_balance_before = self
            .rpc
            .contract_uint_at(weth, "balanceOf", self.expected_solver, prior_block)
            .await?;
        let executor_balance_after = self
            .rpc
            .contract_uint_at(
                weth,
                "balanceOf",
                self.expected_solver,
                receipt.block_number,
            )
            .await?;
        let solver_bond_before = self
            .rpc
            .contract_uint_at(atlas, "balanceOfBonded", self.signer.address(), prior_block)
            .await?;
        let solver_bond_after = self
            .rpc
            .contract_uint_at(
                atlas,
                "balanceOfBonded",
                self.signer.address(),
                receipt.block_number,
            )
            .await?;
        let executor_delta = executor_balance_after
            .checked_sub(executor_balance_before)
            .ok_or(RevenueError::Data)?;
        let bond_cost = solver_bond_before
            .checked_sub(solver_bond_after)
            .ok_or(RevenueError::Data)?;
        if executor_delta != settlement.realized_profit {
            return Err(RevenueError::Data);
        }
        let realized_net = executor_delta
            .checked_sub(bond_cost)
            .ok_or(RevenueError::Data)?;
        if realized_net <= active.retained_profit_floor || realized_net > i128::MAX as u128 {
            return Err(RevenueError::Data);
        }
        let evidence_hash = outcome_evidence_hash(&AtlasOutcomeEvidence {
            transaction_hash: &transaction_hash,
            block_number: receipt.block_number,
            settled_bid,
            executor_before: executor_balance_before,
            executor_after: executor_balance_after,
            bond_before: solver_bond_before,
            bond_after: solver_bond_after,
            realized_net,
        });
        Ok(AtlasOutcome::Reconciled {
            transaction_hash,
            block_number: receipt.block_number,
            settled_bid,
            executor_balance_before,
            executor_balance_after,
            solver_bond_before,
            solver_bond_after,
            realized_net_pnl: realized_net as i128,
            evidence_hash,
        })
    }

    async fn persist_outcome(
        &self,
        active: ActiveAtlasRequest,
        outcome: AtlasOutcome,
    ) -> Result<AtlasExecutionState, RevenueError> {
        match outcome {
            AtlasOutcome::Pending => Ok(AtlasExecutionState::Submitted {
                auction_id: active.auction_id,
            }),
            AtlasOutcome::Expired => {
                let mut transaction = self.pool.begin().await?;
                sqlx::query("UPDATE live_canary.atlas_solver_requests SET status='expired', updated_at=now() WHERE auction_id=$1 AND status IN ('submitted','submission_unknown')")
                    .bind(&active.auction_id).execute(&mut *transaction).await?;
                sqlx::query("UPDATE live_canary.atlas_auction_ingress SET terminal_outcome='expired', updated_at=now() WHERE auction_id=$1")
                    .bind(&active.auction_id).execute(&mut *transaction).await?;
                release_lock(&mut transaction, &active.auction_id).await?;
                transaction.commit().await?;
                Ok(AtlasExecutionState::Rejected {
                    auction_id: active.auction_id,
                })
            }
            AtlasOutcome::Failed { transaction_hash } => {
                let mut transaction = self.pool.begin().await?;
                sqlx::query("UPDATE live_canary.atlas_solver_requests SET status='lost', inclusion_transaction_hash=$2, updated_at=now() WHERE auction_id=$1 AND status IN ('submitted','submission_unknown')")
                    .bind(&active.auction_id).bind(transaction_hash).execute(&mut *transaction).await?;
                sqlx::query("UPDATE live_canary.revenue_lane_controls SET armed=false, kill_switch=true, disarm_reason='atlas_inclusion_failed', control_epoch=control_epoch+1, updated_at=now() WHERE lane='atlas_solver'")
                    .execute(&mut *transaction).await?;
                release_lock(&mut transaction, &active.auction_id).await?;
                transaction.commit().await?;
                Ok(AtlasExecutionState::Rejected {
                    auction_id: active.auction_id,
                })
            }
            AtlasOutcome::Reconciled {
                transaction_hash,
                block_number,
                settled_bid,
                executor_balance_before,
                executor_balance_after,
                solver_bond_before,
                solver_bond_after,
                realized_net_pnl,
                evidence_hash,
            } => {
                let mut transaction = self.pool.begin().await?;
                sqlx::query(
                    "UPDATE live_canary.atlas_solver_requests
                     SET status='reconciled', inclusion_transaction_hash=$2,
                         inclusion_block_number=$3::numeric, settled_bid=$4::numeric,
                         executor_balance_before=$5::numeric, executor_balance_after=$6::numeric,
                         solver_bond_before=$7::numeric, solver_bond_after=$8::numeric,
                         realized_net_pnl=$9::numeric, outcome_evidence_hash=$10, updated_at=now()
                     WHERE auction_id=$1 AND status IN ('submitted','submission_unknown')",
                )
                .bind(&active.auction_id)
                .bind(transaction_hash)
                .bind(block_number.to_string())
                .bind(settled_bid.to_string())
                .bind(executor_balance_before.to_string())
                .bind(executor_balance_after.to_string())
                .bind(solver_bond_before.to_string())
                .bind(solver_bond_after.to_string())
                .bind(realized_net_pnl.to_string())
                .bind(evidence_hash)
                .execute(&mut *transaction)
                .await?;
                sqlx::query("UPDATE live_canary.atlas_auction_ingress SET terminal_outcome='settled', updated_at=now() WHERE auction_id=$1")
                    .bind(&active.auction_id).execute(&mut *transaction).await?;
                sqlx::query("UPDATE live_canary.revenue_hunting_signals SET terminal_outcome='settled', updated_at=now() WHERE auction_id=$1 AND source_lane='atlas_solver'")
                    .bind(&active.auction_id).execute(&mut *transaction).await?;
                release_lock(&mut transaction, &active.auction_id).await?;
                transaction.commit().await?;
                Ok(AtlasExecutionState::Reconciled {
                    auction_id: active.auction_id,
                })
            }
        }
    }

    async fn submission_is_fresh(
        &self,
        claimed: &ClaimedAtlasRequest,
        identity: &AaveLiquidationIdentity,
        now: DateTime<Utc>,
    ) -> Result<bool, RevenueError> {
        if !inner_deadline_is_fresh(identity.deadline, now) {
            return Ok(false);
        }
        let latest_block = self.rpc.latest_block_number().await?;
        Ok(latest_block < claimed.auction_deadline && latest_block < claimed.operation.deadline)
    }

    async fn preflight_is_ready(
        &self,
        claimed: &ClaimedAtlasRequest,
        identity: &AaveLiquidationIdentity,
        reviewed_legs: &[ValidatedLeg],
    ) -> Result<bool, RevenueError> {
        if !self.executor_is_ready(identity, reviewed_legs).await? {
            return Ok(false);
        }
        // Freshness is deliberately the final network-backed check. Contract
        // readiness performs several RPC reads and must not consume the inner
        // Unix or auction-block deadline margin unnoticed.
        self.submission_is_fresh(claimed, identity, Utc::now())
            .await
    }

    async fn executor_is_ready(
        &self,
        identity: &AaveLiquidationIdentity,
        reviewed_legs: &[ValidatedLeg],
    ) -> Result<bool, RevenueError> {
        self.rpc
            .atlas_execution_contract_ready(
                self.expected_solver,
                self.signer.address(),
                &self.expected_code_hash,
                identity.maximum_input_amount,
                identity.debt_asset,
                identity.collateral_asset,
                reviewed_legs,
            )
            .await
            .map_err(RevenueError::from)
    }

    async fn lane_enabled(&self) -> Result<bool, RevenueError> {
        Ok(sqlx::query_scalar(
            "SELECT armed AND NOT kill_switch
             FROM live_canary.revenue_lane_controls
             WHERE lane = 'atlas_solver'",
        )
        .fetch_one(&self.pool)
        .await?)
    }

    async fn claim(&self, now: DateTime<Utc>) -> Result<Option<ClaimedAtlasRequest>, RevenueError> {
        let mut transaction = self.pool.begin().await?;
        sqlx::query("SELECT pg_advisory_xact_lock(hashtextextended('phoenix-global-revenue-submission', 0))")
            .execute(&mut *transaction)
            .await?;
        let lane = sqlx::query(
            "SELECT armed, kill_switch, maximum_input_amount::text, maximum_gas_limit,
                    maximum_fee_per_gas::text, maximum_atlas_bid::text,
                    daily_loss_limit::text, retained_profit_floor::text
             FROM live_canary.revenue_lane_controls
             WHERE lane = 'atlas_solver' FOR UPDATE",
        )
        .fetch_one(&mut *transaction)
        .await?;
        if !lane.try_get::<bool, _>("armed")? || lane.try_get::<bool, _>("kill_switch")? {
            transaction.rollback().await?;
            return Ok(None);
        }
        let lane_limits = AtlasLaneLimits {
            maximum_input_amount: parse_u128(lane.try_get::<String, _>("maximum_input_amount")?)?,
            maximum_gas_limit: u64::try_from(lane.try_get::<i64, _>("maximum_gas_limit")?)
                .map_err(|_| RevenueError::Data)?,
            maximum_fee_per_gas: parse_u128(lane.try_get::<String, _>("maximum_fee_per_gas")?)?,
            maximum_atlas_bid: parse_u128(lane.try_get::<String, _>("maximum_atlas_bid")?)?,
            daily_loss_limit: parse_u128(lane.try_get::<String, _>("daily_loss_limit")?)?,
            retained_profit_floor: parse_u128(lane.try_get::<String, _>("retained_profit_floor")?)?,
        };
        let lock = sqlx::query(
            "SELECT active_lane FROM live_canary.global_revenue_submission_lock
             WHERE singleton FOR UPDATE",
        )
        .fetch_one(&mut *transaction)
        .await?;
        let active_lane: Option<String> = lock.try_get("active_lane")?;
        let active_transactions: i64 = sqlx::query_scalar(&format!(
            "SELECT count(*) FROM live_canary.execution_attempts WHERE status IN ({ACTIVE_ATTEMPT_STATUSES})"
        ))
        .fetch_one(&mut *transaction)
        .await?;
        if active_lane.is_some() || active_transactions != 0 {
            transaction.rollback().await?;
            return Ok(None);
        }
        // The daily cap is shared with direct execution losses. Unknown or
        // failed Atlas submissions retain their complete bounded solver
        // exposure for the UTC day, so an explicit lane re-arm cannot reset
        // the budget. Pre-submission rejections carry no submission/inclusion
        // hash and therefore consume no loss budget.
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
                     AND (
                       r.status IN ('signed', 'submitted', 'submission_unknown')
                       OR (
                         r.status = 'lost'
                         AND (r.submission_response_hash IS NOT NULL OR r.inclusion_transaction_hash IS NOT NULL)
                       )
                     )
                 )
                 SELECT (direct_loss.amount + atlas_loss.amount)::text FROM direct_loss, atlas_loss",
            )
            .bind(now)
            .fetch_one(&mut *transaction)
            .await?,
        )?;
        let row = sqlx::query(
            "SELECT r.auction_id, r.maximum_bid::text, r.selected_bid::text, r.solver_operation,
                    i.solver_gas_limit, i.oracle_gas_price_wei::text,
                    i.auction_deadline_block::text, s.retained_profit_floor::text AS signal_floor,
                    s.evidence_mode
             FROM live_canary.atlas_solver_requests r
             JOIN live_canary.revenue_hunting_signals s ON s.signal_id = r.signal_id
             JOIN live_canary.atlas_auction_ingress i ON i.auction_id = r.auction_id
             WHERE r.status = 'ready'
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
        .fetch_optional(&mut *transaction)
        .await?;
        let Some(row) = row else {
            transaction.rollback().await?;
            return Ok(None);
        };
        let auction_id: String = row.try_get("auction_id")?;
        let maximum_bid = parse_u128(row.try_get::<String, _>("maximum_bid")?)?;
        let selected_bid = parse_u128(row.try_get::<String, _>("selected_bid")?)?;
        let solver_gas_limit = u64::try_from(row.try_get::<i64, _>("solver_gas_limit")?)
            .map_err(|_| RevenueError::Data)?;
        let oracle_gas_price = parse_u128(row.try_get::<String, _>("oracle_gas_price_wei")?)?;
        let auction_deadline = row
            .try_get::<String, _>("auction_deadline_block")?
            .parse::<u64>()
            .map_err(|_| RevenueError::Data)?;
        let signal_retained_profit_floor = parse_u128(row.try_get::<String, _>("signal_floor")?)?;
        let evidence_mode = row
            .try_get::<Option<String>, _>("evidence_mode")?
            .ok_or(RevenueError::Data)?;
        let value: Value = row.try_get("solver_operation")?;
        let operation = parse_operation(value, selected_bid)?;
        sqlx::query(
            "UPDATE live_canary.atlas_solver_requests
             SET status = 'claimed', updated_at = $2
             WHERE auction_id = $1 AND status = 'ready'",
        )
        .bind(&auction_id)
        .bind(now)
        .execute(&mut *transaction)
        .await?;
        sqlx::query(
            "UPDATE live_canary.global_revenue_submission_lock
             SET active_lane = 'atlas_solver', active_identity = $1,
                 acquired_at = $2, control_epoch = control_epoch + 1
             WHERE singleton AND active_lane IS NULL",
        )
        .bind(&auction_id)
        .bind(now)
        .execute(&mut *transaction)
        .await?;
        transaction.commit().await?;
        Ok(Some(ClaimedAtlasRequest {
            auction_id,
            maximum_bid,
            solver_gas_limit,
            oracle_gas_price,
            auction_deadline,
            signal_retained_profit_floor,
            daily_charged_exposure,
            evidence_mode,
            lane_limits,
            operation,
        }))
    }

    async fn reject_and_unlock(
        &self,
        auction_id: &str,
        _reason: &'static str,
    ) -> Result<(), RevenueError> {
        let mut transaction = self.pool.begin().await?;
        sqlx::query(
            "UPDATE live_canary.atlas_solver_requests SET status = 'lost', updated_at = now()
             WHERE auction_id = $1 AND status IN ('claimed','signed','submitted','submission_unknown')",
        )
        .bind(auction_id)
        .execute(&mut *transaction)
        .await?;
        release_lock(&mut transaction, auction_id).await?;
        transaction.commit().await?;
        Ok(())
    }

    async fn kill_unknown(
        &self,
        auction_id: &str,
        reason: &'static str,
    ) -> Result<(), RevenueError> {
        let mut transaction = self.pool.begin().await?;
        sqlx::query(
            "UPDATE live_canary.atlas_solver_requests SET status = 'submission_unknown', updated_at = now()
             WHERE auction_id = $1 AND status IN ('claimed','signed')",
        )
        .bind(auction_id)
        .execute(&mut *transaction)
        .await?;
        sqlx::query(
            "UPDATE live_canary.revenue_lane_controls
             SET armed = false, kill_switch = true, disarm_reason = $1,
                 control_epoch = control_epoch + 1, updated_at = now()
             WHERE lane = 'atlas_solver'",
        )
        .bind(reason)
        .execute(&mut *transaction)
        .await?;
        transaction.commit().await?;
        Ok(())
    }
}

async fn release_lock(
    transaction: &mut sqlx::Transaction<'_, sqlx::Postgres>,
    identity: &str,
) -> Result<(), RevenueError> {
    sqlx::query(
        "UPDATE live_canary.global_revenue_submission_lock
         SET active_lane = NULL, active_identity = NULL, acquired_at = NULL,
             control_epoch = control_epoch + 1
         WHERE singleton AND active_identity = $1",
    )
    .bind(identity)
    .execute(&mut **transaction)
    .await?;
    Ok(())
}

fn parse_operation(value: Value, selected_bid: u128) -> Result<AtlasSolverOperation, RevenueError> {
    let raw: PreparedOperation = serde_json::from_value(value).map_err(|_| RevenueError::Data)?;
    if parse_u128(raw.bid_amount)? != selected_bid {
        return Err(RevenueError::Data);
    }
    let bid_token = raw
        .bid_token
        .filter(|value| value != &format!("0x{}", "0".repeat(40)))
        .map(|value| CanonicalAddress::parse(&value).map_err(|_| RevenueError::Data))
        .transpose()?;
    Ok(AtlasSolverOperation {
        from: CanonicalAddress::parse(&raw.from).map_err(|_| RevenueError::Data)?,
        to: CanonicalAddress::parse(&raw.to).map_err(|_| RevenueError::Data)?,
        value: parse_u256(&raw.value)?,
        gas: raw.gas,
        max_fee_per_gas: parse_u128(raw.max_fee_per_gas)?,
        deadline: raw.deadline,
        solver: CanonicalAddress::parse(&raw.solver).map_err(|_| RevenueError::Data)?,
        control: CanonicalAddress::parse(&raw.control).map_err(|_| RevenueError::Data)?,
        user_op_hash: parse_hash(&raw.user_op_hash)?,
        bid_token,
        bid_amount: selected_bid,
        data: parse_bytes(&raw.data)?,
    })
}

fn validate_aave_atlas_safety(
    claimed: &ClaimedAtlasRequest,
    safety: &SafetyLimits,
    now: DateTime<Utc>,
) -> Result<(AaveLiquidationIdentity, Vec<ValidatedLeg>), RevenueError> {
    let operation = &claimed.operation;
    let lane = claimed.lane_limits;

    // Atlas exposes one all-in oracle gas price in SolverOperation, not a
    // separate EIP-1559 priority field. Requiring that price to fit both
    // executor fee ceilings is the only fail-closed way to prevent the Atlas
    // lane from bypassing the configured priority cap.
    if operation.gas != claimed.solver_gas_limit
        || operation.gas > lane.maximum_gas_limit
        || operation.gas > safety.maximum_gas_limit
        || operation.max_fee_per_gas != claimed.oracle_gas_price
        || operation.max_fee_per_gas > lane.maximum_fee_per_gas
        || operation.max_fee_per_gas > safety.maximum_max_fee_per_gas
        || operation.max_fee_per_gas > safety.maximum_priority_fee_per_gas
        || claimed.maximum_bid == 0
        || claimed.maximum_bid > lane.maximum_atlas_bid
        || operation.bid_amount > claimed.maximum_bid
        || operation.bid_amount > lane.maximum_atlas_bid
        || operation.bid_token.is_some()
        || claimed.evidence_mode != ATLAS_ACTUAL_PATH_EVIDENCE_MODE
    {
        return Err(RevenueError::Data);
    }

    let identity = AaveLiquidationRequest::decode_encoded_identity(&operation.data)
        .map_err(|_| RevenueError::Data)?;
    let weth = CanonicalAddress::parse(ARBITRUM_WETH_ADDRESS).map_err(|_| RevenueError::Data)?;
    if identity.borrower.as_bytes() == &[0; 20]
        || identity.debt_asset != weth
        || identity.repay_amount > lane.maximum_input_amount
        || identity.repay_amount > safety.maximum_input_amount
        || identity.maximum_input_amount > lane.maximum_input_amount
        || identity.maximum_input_amount > safety.maximum_input_amount
        || identity.maximum_atlas_bid != operation.bid_amount
        || identity.route_id == [0; 32]
        || !inner_deadline_is_fresh(identity.deadline, now)
    {
        return Err(RevenueError::Data);
    }
    let reviewed_legs = reviewed_atlas_aave_legs(&identity)?;

    // The solver gas limit at the auction oracle price is the maximum bonded
    // Atlas gas liability. It is encoded once in minProfit; the exact bid is
    // already added independently by PhoenixExecutor and must not be added here.
    let solver_exposure = u128::from(claimed.solver_gas_limit)
        .checked_mul(claimed.oracle_gas_price)
        .ok_or(RevenueError::Data)?;
    // The claim transaction carries the durable UTC-day total of direct losses
    // plus prior failed/unknown Atlas exposures. Charge this attempt before
    // signing so an owner re-arm cannot reset either daily budget.
    let daily_exposure_after_claim = claimed
        .daily_charged_exposure
        .checked_add(solver_exposure)
        .ok_or(RevenueError::Data)?;
    if daily_exposure_after_claim > lane.daily_loss_limit
        || daily_exposure_after_claim > safety.maximum_daily_loss_wei
    {
        return Err(RevenueError::Data);
    }
    let retained_floor = claimed
        .signal_retained_profit_floor
        .max(lane.retained_profit_floor)
        .max(safety.minimum_expected_profit);
    let minimum_profit_with_exposure = retained_floor
        .checked_add(solver_exposure)
        .and_then(|value| value.checked_add(1))
        .ok_or(RevenueError::Data)?;
    if identity.minimum_profit < minimum_profit_with_exposure {
        return Err(RevenueError::Data);
    }
    Ok((identity, reviewed_legs))
}

fn inner_deadline_is_fresh(deadline: u64, now: DateTime<Utc>) -> bool {
    u64::try_from(now.timestamp())
        .ok()
        .and_then(|now| now.checked_add(ATLAS_SUBMISSION_MARGIN_SECONDS))
        .is_some_and(|minimum_deadline| deadline > minimum_deadline)
}

fn reviewed_atlas_aave_legs(
    identity: &AaveLiquidationIdentity,
) -> Result<Vec<ValidatedLeg>, RevenueError> {
    let weth = CanonicalAddress::parse(ARBITRUM_WETH_ADDRESS).map_err(|_| RevenueError::Data)?;
    if identity.collateral_asset == weth {
        return if identity.unwind_legs.is_empty() {
            Ok(Vec::new())
        } else {
            Err(RevenueError::Data)
        };
    }

    let native_usdc =
        CanonicalAddress::parse(ARBITRUM_NATIVE_USDC_ADDRESS).map_err(|_| RevenueError::Data)?;
    let factory = CanonicalAddress::parse(ARBITRUM_UNISWAP_V3_FACTORY_ADDRESS)
        .map_err(|_| RevenueError::Data)?;
    let [leg] = identity.unwind_legs.as_slice() else {
        return Err(RevenueError::Data);
    };
    let expected_pool = match leg.fee {
        500 => CURRENT_ROUTE_POOL_500_ADDRESS,
        3_000 => CURRENT_ROUTE_POOL_3000_ADDRESS,
        _ => return Err(RevenueError::Data),
    };
    if identity.collateral_asset != native_usdc
        || leg.pool != CanonicalAddress::parse(expected_pool).map_err(|_| RevenueError::Data)?
        || leg.token_in != native_usdc
        || leg.token_out != weth
        || leg.zero_for_one
        || leg.minimum_amount_out == 0
        || leg.minimum_amount_out != identity.minimum_unwind_output
    {
        return Err(RevenueError::Data);
    }
    Ok(vec![ValidatedLeg {
        pool: leg.pool,
        factory: Some(factory),
        token_in: leg.token_in,
        token_out: leg.token_out,
        fee: leg.fee,
        zero_for_one: leg.zero_for_one,
        min_amount_out: leg.minimum_amount_out,
    }])
}

fn parse_u128(value: impl AsRef<str>) -> Result<u128, RevenueError> {
    let value = value.as_ref();
    let parsed = if let Some(hex) = value.strip_prefix("0x") {
        u128::from_str_radix(hex, 16)
    } else {
        value.parse()
    };
    parsed.map_err(|_| RevenueError::Data)
}

fn parse_u256(value: &str) -> Result<U256, RevenueError> {
    if let Some(hex) = value.strip_prefix("0x") {
        U256::from_str_radix(hex, 16).map_err(|_| RevenueError::Data)
    } else {
        U256::from_dec_str(value).map_err(|_| RevenueError::Data)
    }
}

fn parse_hash(value: &str) -> Result<[u8; 32], RevenueError> {
    if value.len() != 66 || !value.starts_with("0x") {
        return Err(RevenueError::Data);
    }
    hex::decode(&value[2..])
        .map_err(|_| RevenueError::Data)?
        .try_into()
        .map_err(|_| RevenueError::Data)
}

fn parse_bytes(value: &str) -> Result<Vec<u8>, RevenueError> {
    let value = value.strip_prefix("0x").ok_or(RevenueError::Data)?;
    hex::decode(value).map_err(|_| RevenueError::Data)
}

fn solver_result_topic() -> [u8; 32] {
    ethabi::long_signature(
        "SolverTxResult",
        &[
            ParamType::Address,
            ParamType::Address,
            ParamType::Address,
            ParamType::Address,
            ParamType::Uint(256),
            ParamType::Bool,
            ParamType::Bool,
            ParamType::Uint(256),
        ],
    )
    .0
}

fn metacall_result_topic() -> [u8; 32] {
    ethabi::long_signature(
        "MetacallResult",
        &[
            ParamType::Address,
            ParamType::Address,
            ParamType::Bool,
            ParamType::Uint(256),
            ParamType::Uint(256),
        ],
    )
    .0
}

fn address_topic(address: CanonicalAddress) -> [u8; 32] {
    let mut topic = [0_u8; 32];
    topic[12..].copy_from_slice(address.as_bytes());
    topic
}

fn decode_solver_result(log: &IndexedRpcLog) -> Result<(u128, bool, bool), RevenueError> {
    if log.log.topics.len() != 4 || log.log.topics[0] != solver_result_topic() {
        return Err(RevenueError::Data);
    }
    let values = ethabi::decode(
        &[
            ParamType::Address,
            ParamType::Uint(256),
            ParamType::Bool,
            ParamType::Bool,
            ParamType::Uint(256),
        ],
        &log.log.data,
    )
    .map_err(|_| RevenueError::Data)?;
    if values.len() != 5 || values[0] != Token::Address(Default::default()) {
        return Err(RevenueError::Data);
    }
    let bid = token_u128(&values[1])?;
    let executed = values[2].clone().into_bool().ok_or(RevenueError::Data)?;
    let success = values[3].clone().into_bool().ok_or(RevenueError::Data)?;
    Ok((bid, executed, success))
}

fn require_successful_metacall(logs: &[crate::abi::RpcLog]) -> Result<(), RevenueError> {
    let atlas =
        CanonicalAddress::parse(ARBITRUM_ATLAS_V1_6_4_ADDRESS).map_err(|_| RevenueError::Data)?;
    let matches = logs
        .iter()
        .filter(|log| log.address == atlas && log.topics.first() == Some(&metacall_result_topic()))
        .collect::<Vec<_>>();
    if matches.len() != 1 || matches[0].topics.len() != 3 {
        return Err(RevenueError::Data);
    }
    let values = ethabi::decode(
        &[ParamType::Bool, ParamType::Uint(256), ParamType::Uint(256)],
        &matches[0].data,
    )
    .map_err(|_| RevenueError::Data)?;
    if values.len() != 3 || values[0] != Token::Bool(true) {
        return Err(RevenueError::Data);
    }
    Ok(())
}

fn metacall_contains_operation(
    input: &[u8],
    expected: &AtlasSolverOperation,
) -> Result<bool, RevenueError> {
    let contract = ethabi::Contract::load(Cursor::new(ATLAS_METACALL_ABI.as_bytes()))
        .map_err(|_| RevenueError::Data)?;
    let function = contract
        .function("metacall")
        .map_err(|_| RevenueError::Data)?;
    if input.len() < 4 || input[..4] != function.short_signature() {
        return Ok(false);
    }
    let decoded = function
        .decode_input(&input[4..])
        .map_err(|_| RevenueError::Data)?;
    let Token::Array(operations) = decoded.get(1).ok_or(RevenueError::Data)? else {
        return Err(RevenueError::Data);
    };
    let mut matches = 0_u8;
    for token in operations {
        let Token::Tuple(fields) = token else {
            return Err(RevenueError::Data);
        };
        if fields.len() != 13 {
            return Err(RevenueError::Data);
        }
        let bid_token = expected
            .bid_token
            .map(|address| ethabi::Address::from_slice(address.as_bytes()))
            .unwrap_or_default();
        let signature_valid =
            matches!(&fields[12], Token::Bytes(bytes) if !bytes.is_empty() && bytes.len() <= 256);
        let operation_matches = token_address(&fields[0])? == expected.from
            && token_address(&fields[1])? == expected.to
            && token_u256(&fields[2])? == expected.value
            && token_u128(&fields[3])? == u128::from(expected.gas)
            && token_u128(&fields[4])? == expected.max_fee_per_gas
            && token_u128(&fields[5])? == u128::from(expected.deadline)
            && token_address(&fields[6])? == expected.solver
            && token_address(&fields[7])? == expected.control
            && token_bytes32(&fields[8])? == expected.user_op_hash
            && matches!(&fields[9], Token::Address(address) if *address == bid_token)
            && token_u128(&fields[10])? == expected.bid_amount
            && matches!(&fields[11], Token::Bytes(data) if data == &expected.data)
            && signature_valid;
        if operation_matches {
            matches = matches.checked_add(1).ok_or(RevenueError::Data)?;
        }
    }
    Ok(matches == 1)
}

fn token_address(token: &Token) -> Result<CanonicalAddress, RevenueError> {
    let Token::Address(value) = token else {
        return Err(RevenueError::Data);
    };
    CanonicalAddress::parse(&format!("0x{}", hex::encode(value))).map_err(|_| RevenueError::Data)
}

fn token_u256(token: &Token) -> Result<U256, RevenueError> {
    token.clone().into_uint().ok_or(RevenueError::Data)
}

fn token_u128(token: &Token) -> Result<u128, RevenueError> {
    let value = token_u256(token)?;
    if value > U256::from(u128::MAX) {
        return Err(RevenueError::Data);
    }
    Ok(value.low_u128())
}

fn token_bytes32(token: &Token) -> Result<[u8; 32], RevenueError> {
    let Token::FixedBytes(value) = token else {
        return Err(RevenueError::Data);
    };
    value.as_slice().try_into().map_err(|_| RevenueError::Data)
}

struct AtlasOutcomeEvidence<'a> {
    transaction_hash: &'a str,
    block_number: u64,
    settled_bid: u128,
    executor_before: u128,
    executor_after: u128,
    bond_before: u128,
    bond_after: u128,
    realized_net: u128,
}

fn outcome_evidence_hash(evidence: &AtlasOutcomeEvidence<'_>) -> String {
    let AtlasOutcomeEvidence {
        transaction_hash,
        block_number,
        settled_bid,
        executor_before,
        executor_after,
        bond_before,
        bond_after,
        realized_net,
    } = evidence;
    hex::encode(Sha256::digest(format!(
        "phoenix.atlas-outcome.v1|{transaction_hash}|{block_number}|{settled_bid}|{executor_before}|{executor_after}|{bond_before}|{bond_after}|{realized_net}"
    )))
}

const ATLAS_METACALL_ABI: &str = r#"[{"type":"function","name":"metacall","stateMutability":"payable","inputs":[{"name":"userOp","type":"tuple","components":[{"name":"from","type":"address"},{"name":"to","type":"address"},{"name":"value","type":"uint256"},{"name":"gas","type":"uint256"},{"name":"maxFeePerGas","type":"uint256"},{"name":"nonce","type":"uint256"},{"name":"deadline","type":"uint256"},{"name":"dapp","type":"address"},{"name":"control","type":"address"},{"name":"callConfig","type":"uint32"},{"name":"dappGasLimit","type":"uint32"},{"name":"solverGasLimit","type":"uint32"},{"name":"bundlerSurchargeRate","type":"uint24"},{"name":"sessionKey","type":"address"},{"name":"data","type":"bytes"},{"name":"signature","type":"bytes"}]},{"name":"solverOps","type":"tuple[]","components":[{"name":"from","type":"address"},{"name":"to","type":"address"},{"name":"value","type":"uint256"},{"name":"gas","type":"uint256"},{"name":"maxFeePerGas","type":"uint256"},{"name":"deadline","type":"uint256"},{"name":"solver","type":"address"},{"name":"control","type":"address"},{"name":"userOpHash","type":"bytes32"},{"name":"bidToken","type":"address"},{"name":"bidAmount","type":"uint256"},{"name":"data","type":"bytes"},{"name":"signature","type":"bytes"}]},{"name":"dAppOp","type":"tuple","components":[{"name":"from","type":"address"},{"name":"to","type":"address"},{"name":"nonce","type":"uint256"},{"name":"deadline","type":"uint256"},{"name":"control","type":"address"},{"name":"bundler","type":"address"},{"name":"userOpHash","type":"bytes32"},{"name":"callChainHash","type":"bytes32"},{"name":"signature","type":"bytes"}]},{"name":"gasRefundBeneficiary","type":"address"}],"outputs":[{"name":"auctionWon","type":"bool"}]}]"#;

#[derive(Debug, Error)]
pub enum RevenueError {
    #[error("revenue database failure")]
    Database(#[from] sqlx::Error),
    #[error("revenue request data is invalid")]
    Data,
    #[error(transparent)]
    Atlas(#[from] AtlasError),
    #[error("revenue RPC evidence failure")]
    Rpc(#[from] RpcError),
}

impl RevenueError {
    pub fn code(&self) -> &'static str {
        match self {
            Self::Database(_) => "atlas_database",
            Self::Data => "atlas_evidence_integrity",
            Self::Atlas(_) => "atlas_gateway_or_operation",
            Self::Rpc(_) => "atlas_outcome_rpc",
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::Duration;
    use serde_json::json;

    fn address(value: &str) -> CanonicalAddress {
        CanonicalAddress::parse(value).expect("address")
    }

    fn safety_limits() -> SafetyLimits {
        SafetyLimits {
            maximum_gas_limit: 1_000,
            maximum_max_fee_per_gas: 20,
            maximum_priority_fee_per_gas: 10,
            maximum_input_amount: 1_000,
            minimum_expected_profit: 100,
            maximum_daily_loss_wei: 10_000,
        }
    }

    fn atlas_aave_request() -> AaveLiquidationRequest {
        let weth = address(ARBITRUM_WETH_ADDRESS);
        AaveLiquidationRequest {
            route_id: [7; 32],
            borrower: address("0x1111111111111111111111111111111111111111"),
            debt_asset: weth,
            collateral_asset: weth,
            repay_amount: 100,
            maximum_input_amount: 1_000,
            minimum_collateral_received: 100,
            minimum_unwind_output: 100,
            minimum_profit: 5_101,
            maximum_atlas_bid: 200,
            deadline: Utc::now() + Duration::minutes(1),
            unwind_legs: Vec::new(),
        }
    }

    fn atlas_native_usdc_request(fee: u32, pool: &str) -> AaveLiquidationRequest {
        let mut request = atlas_aave_request();
        let native_usdc = address(ARBITRUM_NATIVE_USDC_ADDRESS);
        request.collateral_asset = native_usdc;
        request.unwind_legs = vec![ValidatedLeg {
            pool: address(pool),
            factory: Some(address(ARBITRUM_UNISWAP_V3_FACTORY_ADDRESS)),
            token_in: native_usdc,
            token_out: address(ARBITRUM_WETH_ADDRESS),
            fee,
            zero_for_one: false,
            min_amount_out: request.minimum_unwind_output,
        }];
        request
    }

    fn claimed_with_request(request: &AaveLiquidationRequest) -> ClaimedAtlasRequest {
        ClaimedAtlasRequest {
            auction_id: "auction-1".to_string(),
            maximum_bid: 400,
            solver_gas_limit: 500,
            oracle_gas_price: 10,
            auction_deadline: 49_000_000,
            signal_retained_profit_floor: 100,
            daily_charged_exposure: 0,
            evidence_mode: ATLAS_ACTUAL_PATH_EVIDENCE_MODE.to_string(),
            lane_limits: AtlasLaneLimits {
                maximum_input_amount: 1_000,
                maximum_gas_limit: 1_000,
                maximum_fee_per_gas: 20,
                maximum_atlas_bid: 500,
                daily_loss_limit: 10_000,
                retained_profit_floor: 100,
            },
            operation: AtlasSolverOperation {
                from: address("0x1111111111111111111111111111111111111111"),
                to: address(ARBITRUM_ATLAS_V1_6_4_ADDRESS),
                value: U256::zero(),
                gas: 500,
                max_fee_per_gas: 10,
                deadline: 49_000_000,
                solver: address("0x2222222222222222222222222222222222222222"),
                control: address(ARBITRUM_ATLAS_DAPP_CONTROL_ADDRESS),
                user_op_hash: [0x33; 32],
                bid_token: None,
                bid_amount: 200,
                data: request.encoded_request().expect("Atlas payload"),
            },
        }
    }

    fn validate_test_request(
        claimed: &ClaimedAtlasRequest,
        limits: &SafetyLimits,
    ) -> Result<(AaveLiquidationIdentity, Vec<ValidatedLeg>), RevenueError> {
        validate_aave_atlas_safety(claimed, limits, Utc::now())
    }

    #[test]
    fn prepared_operation_binds_exact_bid_and_official_atlas() {
        let value = json!({
            "from":"0x1111111111111111111111111111111111111111",
            "to":crate::ARBITRUM_ATLAS_V1_6_4_ADDRESS,
            "value":"0x0", "gas":500000, "max_fee_per_gas":"5000000",
            "deadline":49000000,
            "solver":"0x2222222222222222222222222222222222222222",
            "control":crate::ARBITRUM_ATLAS_DAPP_CONTROL_ADDRESS,
            "user_op_hash":format!("0x{}", "33".repeat(32)),
            "bid_token":null, "bid_amount":"200000", "data":"0x0102"
        });
        let operation = parse_operation(value, 200_000).expect("operation");
        assert_eq!(operation.bid_amount, 200_000);
        assert_eq!(
            operation.to.to_string(),
            crate::ARBITRUM_ATLAS_V1_6_4_ADDRESS
        );
    }

    #[test]
    fn changed_bid_is_rejected_before_signing() {
        let value = json!({
            "from":"0x1111111111111111111111111111111111111111",
            "to":crate::ARBITRUM_ATLAS_V1_6_4_ADDRESS,
            "value":"0x0", "gas":500000, "max_fee_per_gas":"5000000",
            "deadline":49000000,
            "solver":"0x2222222222222222222222222222222222222222",
            "control":crate::ARBITRUM_ATLAS_DAPP_CONTROL_ADDRESS,
            "user_op_hash":format!("0x{}", "33".repeat(32)),
            "bid_token":null, "bid_amount":"200001", "data":"0x0102"
        });
        assert!(matches!(
            parse_operation(value, 200_000),
            Err(RevenueError::Data)
        ));
    }

    #[test]
    fn aave_atlas_acceptance_counts_solver_exposure_once() {
        let request = atlas_aave_request();
        let claimed = claimed_with_request(&request);

        // minProfit must retain at least one wei after the reviewed floor and
        // maximum solver exposure. The exact Atlas bid is enforced
        // independently by the contract and is not added a second time here.
        assert_eq!(request.minimum_profit, 100 + 500 * 10 + 1);
        validate_test_request(&claimed, &safety_limits()).expect("safe operation");
    }

    #[test]
    fn aave_atlas_acceptance_enforces_every_lane_cap() {
        let request = atlas_aave_request();
        let baseline = claimed_with_request(&request);
        let limits = safety_limits();

        let mut changed = baseline.clone();
        changed.lane_limits.maximum_gas_limit = 499;
        assert!(validate_test_request(&changed, &limits).is_err());

        changed = baseline.clone();
        changed.lane_limits.maximum_fee_per_gas = 9;
        assert!(validate_test_request(&changed, &limits).is_err());

        changed = baseline.clone();
        changed.maximum_bid = 501;
        assert!(validate_test_request(&changed, &limits).is_err());

        changed = baseline.clone();
        changed.operation.bid_token = Some(address(ARBITRUM_WETH_ADDRESS));
        assert!(validate_test_request(&changed, &limits).is_err());

        changed = baseline.clone();
        changed.evidence_mode = "DUAL_PROVIDER_FORK_VERIFIED".to_string();
        assert!(validate_test_request(&changed, &limits).is_err());

        changed = baseline.clone();
        changed.lane_limits.daily_loss_limit = 4_999;
        assert!(validate_test_request(&changed, &limits).is_err());

        changed = baseline.clone();
        changed.daily_charged_exposure = 5_001;
        assert!(validate_test_request(&changed, &limits).is_err());

        let mut changed_request = request.clone();
        changed_request.maximum_input_amount = 1_001;
        changed = claimed_with_request(&changed_request);
        assert!(validate_test_request(&changed, &limits).is_err());

        changed_request = request.clone();
        changed_request.minimum_profit = 5_100;
        changed = claimed_with_request(&changed_request);
        assert!(validate_test_request(&changed, &limits).is_err());

        changed_request = request.clone();
        changed_request.maximum_atlas_bid = 201;
        changed = claimed_with_request(&changed_request);
        assert!(validate_test_request(&changed, &limits).is_err());
    }

    #[test]
    fn aave_atlas_acceptance_enforces_global_priority_and_exact_auction_gas() {
        let request = atlas_aave_request();
        let baseline = claimed_with_request(&request);

        let mut gas_limited = safety_limits();
        gas_limited.maximum_gas_limit = 499;
        assert!(validate_test_request(&baseline, &gas_limited).is_err());

        let mut fee_limited = safety_limits();
        fee_limited.maximum_max_fee_per_gas = 9;
        assert!(validate_test_request(&baseline, &fee_limited).is_err());

        let mut priority_limited = safety_limits();
        priority_limited.maximum_priority_fee_per_gas = 9;
        assert!(validate_test_request(&baseline, &priority_limited).is_err());

        let mut input_limited = safety_limits();
        input_limited.maximum_input_amount = 999;
        assert!(validate_test_request(&baseline, &input_limited).is_err());

        let mut floor_raised = safety_limits();
        floor_raised.minimum_expected_profit = 101;
        assert!(validate_test_request(&baseline, &floor_raised).is_err());

        let mut loss_limited = safety_limits();
        loss_limited.maximum_daily_loss_wei = 4_999;
        assert!(validate_test_request(&baseline, &loss_limited).is_err());

        let mut changed = baseline;
        changed.operation.gas = 499;
        assert!(validate_test_request(&changed, &safety_limits()).is_err());
    }

    #[test]
    fn aave_atlas_acceptance_requires_fresh_inner_deadline_and_nonzero_identity() {
        let mut request = atlas_aave_request();
        request.deadline = Utc::now() + Duration::seconds(10);
        let claimed = claimed_with_request(&request);
        assert!(validate_test_request(&claimed, &safety_limits()).is_err());

        request = atlas_aave_request();
        request.route_id = [0; 32];
        let claimed = claimed_with_request(&request);
        assert!(validate_test_request(&claimed, &safety_limits()).is_err());
    }

    #[test]
    fn aave_atlas_acceptance_binds_exact_reviewed_collateral_route() {
        for (fee, pool) in [
            (500, CURRENT_ROUTE_POOL_500_ADDRESS),
            (3_000, CURRENT_ROUTE_POOL_3000_ADDRESS),
        ] {
            let request = atlas_native_usdc_request(fee, pool);
            validate_test_request(&claimed_with_request(&request), &safety_limits())
                .expect("reviewed native USDC route");
        }

        let mut request = atlas_native_usdc_request(500, CURRENT_ROUTE_POOL_500_ADDRESS);
        request.unwind_legs[0].pool = address(CURRENT_ROUTE_POOL_3000_ADDRESS);
        assert!(validate_test_request(&claimed_with_request(&request), &safety_limits()).is_err());

        request = atlas_native_usdc_request(500, CURRENT_ROUTE_POOL_500_ADDRESS);
        request.unwind_legs[0].zero_for_one = true;
        assert!(validate_test_request(&claimed_with_request(&request), &safety_limits()).is_err());

        request = atlas_native_usdc_request(500, CURRENT_ROUTE_POOL_500_ADDRESS);
        request.unwind_legs[0].min_amount_out = request.minimum_unwind_output - 1;
        assert!(validate_test_request(&claimed_with_request(&request), &safety_limits()).is_err());

        request = atlas_native_usdc_request(500, CURRENT_ROUTE_POOL_500_ADDRESS);
        let unreviewed = address("0x1111111111111111111111111111111111111111");
        request.collateral_asset = unreviewed;
        request.unwind_legs[0].token_in = unreviewed;
        assert!(validate_test_request(&claimed_with_request(&request), &safety_limits()).is_err());
    }

    #[test]
    fn public_metacall_is_bound_to_the_complete_solver_operation() {
        let operation = AtlasSolverOperation {
            from: CanonicalAddress::parse("0x1111111111111111111111111111111111111111")
                .expect("from"),
            to: CanonicalAddress::parse(ARBITRUM_ATLAS_V1_6_4_ADDRESS).expect("Atlas"),
            value: U256::zero(),
            gas: 500_000,
            max_fee_per_gas: 5_000_000,
            deadline: 49_000_000,
            solver: CanonicalAddress::parse("0x2222222222222222222222222222222222222222")
                .expect("solver"),
            control: CanonicalAddress::parse(ARBITRUM_ATLAS_DAPP_CONTROL_ADDRESS).expect("control"),
            user_op_hash: [0x33; 32],
            bid_token: None,
            bid_amount: 200_000,
            data: vec![1, 2, 3],
        };
        let address =
            |value: CanonicalAddress| Token::Address(ethabi::Address::from_slice(value.as_bytes()));
        let zero_address = Token::Address(Default::default());
        let user_op = Token::Tuple(vec![
            zero_address.clone(),
            zero_address.clone(),
            Token::Uint(U256::zero()),
            Token::Uint(U256::from(1)),
            Token::Uint(U256::from(1)),
            Token::Uint(U256::zero()),
            Token::Uint(U256::from(operation.deadline)),
            zero_address.clone(),
            address(operation.control),
            Token::Uint(U256::zero()),
            Token::Uint(U256::from(1)),
            Token::Uint(U256::from(1)),
            Token::Uint(U256::zero()),
            zero_address.clone(),
            Token::Bytes(vec![1]),
            Token::Bytes(vec![2]),
        ]);
        let solver_op = Token::Tuple(vec![
            address(operation.from),
            address(operation.to),
            Token::Uint(operation.value),
            Token::Uint(U256::from(operation.gas)),
            Token::Uint(U256::from(operation.max_fee_per_gas)),
            Token::Uint(U256::from(operation.deadline)),
            address(operation.solver),
            address(operation.control),
            Token::FixedBytes(operation.user_op_hash.to_vec()),
            zero_address.clone(),
            Token::Uint(U256::from(operation.bid_amount)),
            Token::Bytes(operation.data.clone()),
            Token::Bytes(vec![9; 65]),
        ]);
        let dapp_op = Token::Tuple(vec![
            zero_address.clone(),
            address(operation.to),
            Token::Uint(U256::zero()),
            Token::Uint(U256::from(operation.deadline)),
            address(operation.control),
            zero_address.clone(),
            Token::FixedBytes(operation.user_op_hash.to_vec()),
            Token::FixedBytes([4_u8; 32].to_vec()),
            Token::Bytes(vec![5]),
        ]);
        let contract =
            ethabi::Contract::load(Cursor::new(ATLAS_METACALL_ABI.as_bytes())).expect("ABI");
        let calldata = contract
            .function("metacall")
            .expect("metacall")
            .encode_input(&[
                user_op,
                Token::Array(vec![solver_op]),
                dapp_op,
                zero_address,
            ])
            .expect("calldata");
        assert!(metacall_contains_operation(&calldata, &operation).expect("match"));
        let mut changed = operation;
        changed.bid_amount += 1;
        assert!(!metacall_contains_operation(&calldata, &changed).expect("mismatch"));
    }
}
