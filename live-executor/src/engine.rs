use crate::abi::{decode_settlement, encode_execute_opportunity, AbiError};
use crate::config::ExecutorConfig;
use crate::model::{
    ActiveAttempt, AttemptStatus, ExecutionRequest, ReceiptOutcome, Settlement, TransactionHash,
};
use crate::rpc::{ExecutionRpc, RpcError, RpcErrorKind, TransactionReceipt};
use crate::signer::{SignerError, TransactionDraft, TransactionSigner};
use crate::store::{ExecutorStore, StoreError};
use crate::APPROVAL_POLICY_VERSION;
use chrono::{DateTime, Utc};
use sha2::{Digest, Sha256};
use thiserror::Error;
use uuid::Uuid;

pub struct LiveExecutor<S, R> {
    config: ExecutorConfig,
    signer: TransactionSigner,
    store: S,
    rpc: R,
}

impl<S, R> LiveExecutor<S, R>
where
    S: ExecutorStore,
    R: ExecutionRpc,
{
    pub fn new(config: ExecutorConfig, signer: TransactionSigner, store: S, rpc: R) -> Self {
        Self {
            config,
            signer,
            store,
            rpc,
        }
    }

    pub const fn poll_interval(&self) -> std::time::Duration {
        self.config.poll_interval
    }

    pub async fn step(&self, now: DateTime<Utc>) -> Result<ExecutionState, EngineError> {
        if let Some(active) = self.store.active_attempt().await? {
            return self.reconcile_active(active, now).await;
        }

        let control = self.store.control_state().await?;
        if !control.armed || control.kill_switch {
            return Ok(ExecutionState::DisarmedShadow);
        }
        let daily_loss = self.store.daily_loss_wei(now).await?;
        if daily_loss >= self.config.limits.maximum_daily_loss_wei {
            self.store.disarm("daily_loss_budget").await?;
            return Ok(ExecutionState::Disarmed {
                reason: DisarmReason::DailyLossBudget,
            });
        }
        let chain_id = match self.rpc.chain_id().await {
            Ok(chain_id) => chain_id,
            Err(error) => return self.disarm_for_rpc_error(error, None, now).await,
        };
        if chain_id != self.config.chain_id {
            self.store.disarm("rpc_chain_mismatch").await?;
            return Ok(ExecutionState::Disarmed {
                reason: DisarmReason::ChainMismatch,
            });
        }

        let Some(request) = self.store.claim_approved(&self.config, now).await? else {
            return Ok(ExecutionState::ArmedIdle);
        };
        match self
            .rpc
            .execution_contract_ready(
                &request,
                self.config.wallet_address,
                &self.config.executor_code_hash,
            )
            .await
        {
            Ok(true) => {}
            Ok(false) => {
                self.store
                    .mark_terminal(
                        request.id,
                        AttemptStatus::Failed,
                        Some("unexpected_executor_state"),
                        None,
                        now,
                    )
                    .await?;
                self.store.disarm("unexpected_executor_state").await?;
                return Ok(ExecutionState::Disarmed {
                    reason: DisarmReason::Policy,
                });
            }
            Err(_error) => {
                self.store
                    .mark_terminal(
                        request.id,
                        AttemptStatus::Failed,
                        Some("executor_state_rpc_failure"),
                        None,
                        now,
                    )
                    .await?;
                return Ok(ExecutionState::ArmedIdle);
            }
        }
        let calldata = match validate_and_encode(&request, &self.config, now) {
            Ok(calldata) => calldata,
            Err(error) => {
                self.store
                    .mark_terminal(
                        request.id,
                        AttemptStatus::Failed,
                        Some(error.code()),
                        None,
                        now,
                    )
                    .await?;
                self.store.disarm(error.code()).await?;
                return Ok(ExecutionState::Disarmed {
                    reason: DisarmReason::Policy,
                });
            }
        };

        let network_nonce = match self.rpc.pending_nonce(self.config.wallet_address).await {
            Ok(nonce) => nonce,
            Err(error) => {
                return self
                    .disarm_for_rpc_error(error, Some(request.id), now)
                    .await
            }
        };
        let nonce = self
            .store
            .allocate_nonce(request.id, &self.config, network_nonce)
            .await?;
        let signed = match self.signer.sign(TransactionDraft {
            chain_id: self.config.chain_id,
            nonce,
            gas_limit: request.gas_limit,
            max_fee_per_gas: request.max_fee_per_gas,
            max_priority_fee_per_gas: request.max_priority_fee_per_gas,
            to: self.config.executor_address,
            calldata,
        }) {
            Ok(signed) => signed,
            Err(error) => {
                self.store
                    .fail_unsubmitted(request.id, "transaction_signing_failure", now)
                    .await?;
                self.store.disarm("transaction_signing_failure").await?;
                return Err(EngineError::Signer(error));
            }
        };
        self.store.mark_signed(request.id, now).await?;

        let control = self.store.control_state().await?;
        if !control.armed || control.kill_switch {
            self.store
                .fail_unsubmitted(request.id, "kill_switch_before_submission", now)
                .await?;
            return Ok(ExecutionState::DisarmedShadow);
        }
        let current_daily_loss = self.store.daily_loss_wei(now).await?;
        let worst_case_fee = u128::from(request.gas_limit)
            .checked_mul(request.max_fee_per_gas)
            .ok_or(EngineError::Arithmetic)?;
        if current_daily_loss
            .checked_add(worst_case_fee)
            .ok_or(EngineError::Arithmetic)?
            > self.config.limits.maximum_daily_loss_wei
        {
            self.store
                .fail_unsubmitted(request.id, "daily_loss_budget", now)
                .await?;
            self.store.disarm("daily_loss_budget").await?;
            return Ok(ExecutionState::Disarmed {
                reason: DisarmReason::DailyLossBudget,
            });
        }

        let returned_hash = match self.rpc.send_raw_transaction(signed.raw_bytes()).await {
            Ok(tx_hash) => tx_hash,
            Err(error) => {
                return self
                    .disarm_for_submission_error(error, request.id, nonce, now)
                    .await
            }
        };
        if returned_hash != signed.tx_hash() {
            self.store
                .mark_submission_unknown(request.id, "submission_hash_mismatch", now)
                .await?;
            self.store.disarm("submission_hash_mismatch").await?;
            return Ok(ExecutionState::SubmissionUnknown {
                request_id: request.id,
                nonce,
            });
        }

        if let Err(error) = self
            .store
            .mark_pending(request.id, returned_hash, now)
            .await
        {
            let _ = self
                .store
                .mark_submission_unknown(request.id, "hash_persistence_failure", now)
                .await;
            let _ = self.store.disarm("hash_persistence_failure").await;
            return Err(EngineError::HashPersistence(error));
        }
        Ok(ExecutionState::Pending {
            request_id: request.id,
            tx_hash: returned_hash,
        })
    }

    async fn reconcile_active(
        &self,
        active: ActiveAttempt,
        now: DateTime<Utc>,
    ) -> Result<ExecutionState, EngineError> {
        match active.status {
            AttemptStatus::Claimed => {
                self.store
                    .mark_terminal(
                        active.request.id,
                        AttemptStatus::Failed,
                        Some("restart_before_nonce_allocation"),
                        None,
                        now,
                    )
                    .await?;
                self.store.disarm("restart_before_nonce_allocation").await?;
                return Ok(ExecutionState::Disarmed {
                    reason: DisarmReason::SubmissionIntegrity,
                });
            }
            AttemptStatus::NonceAllocated => {
                let nonce = active.nonce.ok_or(EngineError::ActiveAttemptInvariant)?;
                self.store
                    .mark_submission_unknown(
                        active.request.id,
                        "restart_after_nonce_allocation",
                        now,
                    )
                    .await?;
                self.store.disarm("submission_unknown").await?;
                return Ok(ExecutionState::SubmissionUnknown {
                    request_id: active.request.id,
                    nonce,
                });
            }
            AttemptStatus::SubmissionUnknown => {
                let nonce = active.nonce.ok_or(EngineError::ActiveAttemptInvariant)?;
                self.store.disarm("submission_unknown").await?;
                return Ok(ExecutionState::SubmissionUnknown {
                    request_id: active.request.id,
                    nonce,
                });
            }
            AttemptStatus::Pending | AttemptStatus::TimedOut => {}
            _ => return Err(EngineError::ActiveAttemptInvariant),
        }
        let tx_hash = active.tx_hash.ok_or(EngineError::ActiveAttemptInvariant)?;
        let nonce = active.nonce.ok_or(EngineError::ActiveAttemptInvariant)?;
        let submitted_at = active
            .submitted_at
            .ok_or(EngineError::ActiveAttemptInvariant)?;
        let receipt = match self.rpc.transaction_receipt(tx_hash).await {
            Ok(receipt) => receipt,
            Err(error) => {
                self.store
                    .record_monitor_error(active.request.id, rpc_error_code(error.kind))
                    .await?;
                return self.disarm_for_rpc_error(error, None, now).await;
            }
        };
        if let Some(receipt) = receipt {
            return self
                .complete_receipt(active.request, tx_hash, receipt, now)
                .await;
        }

        let known = match self.rpc.transaction_known(tx_hash).await {
            Ok(known) => known,
            Err(error) => {
                self.store
                    .record_monitor_error(active.request.id, rpc_error_code(error.kind))
                    .await?;
                return self.disarm_for_rpc_error(error, None, now).await;
            }
        };
        if !known {
            let pending_nonce = match self.rpc.pending_nonce(self.config.wallet_address).await {
                Ok(pending_nonce) => pending_nonce,
                Err(error) => {
                    self.store
                        .record_monitor_error(active.request.id, rpc_error_code(error.kind))
                        .await?;
                    return self.disarm_for_rpc_error(error, None, now).await;
                }
            };
            if pending_nonce > nonce {
                self.store
                    .mark_terminal(
                        active.request.id,
                        AttemptStatus::Replaced,
                        Some("transaction_replaced"),
                        None,
                        now,
                    )
                    .await?;
                self.store.disarm("transaction_replaced").await?;
                return Ok(ExecutionState::Replaced {
                    request_id: active.request.id,
                    tx_hash,
                });
            }
        }

        let elapsed = now.signed_duration_since(submitted_at);
        if elapsed
            >= chrono::Duration::from_std(self.config.receipt_timeout)
                .map_err(|_| EngineError::Time)?
        {
            if active.status != AttemptStatus::TimedOut {
                self.store
                    .mark_terminal(
                        active.request.id,
                        AttemptStatus::TimedOut,
                        Some("receipt_timeout"),
                        None,
                        now,
                    )
                    .await?;
                self.store.disarm("receipt_timeout").await?;
            }
            return Ok(ExecutionState::TimedOut {
                request_id: active.request.id,
                tx_hash,
            });
        }

        Ok(ExecutionState::Pending {
            request_id: active.request.id,
            tx_hash,
        })
    }

    async fn complete_receipt(
        &self,
        request: ExecutionRequest,
        tx_hash: TransactionHash,
        receipt: TransactionReceipt,
        now: DateTime<Utc>,
    ) -> Result<ExecutionState, EngineError> {
        if receipt.transaction_hash != tx_hash {
            self.store
                .record_monitor_error(request.id, "receipt_hash_mismatch")
                .await?;
            self.store.disarm("receipt_hash_mismatch").await?;
            return Ok(ExecutionState::Disarmed {
                reason: DisarmReason::SubmissionIntegrity,
            });
        }
        if receipt.block_number == 0
            || receipt.gas_used == 0
            || receipt.gas_used > request.gas_limit
            || receipt.effective_gas_price == 0
            || receipt.effective_gas_price > request.max_fee_per_gas
        {
            self.store
                .record_monitor_error(request.id, "receipt_economics_invalid")
                .await?;
            self.store.disarm("receipt_economics_invalid").await?;
            return Ok(ExecutionState::Disarmed {
                reason: DisarmReason::Settlement,
            });
        }
        let actual_fee_wei = u128::from(receipt.gas_used)
            .checked_mul(receipt.effective_gas_price)
            .ok_or(EngineError::Arithmetic)?;
        if receipt.l1_fee > actual_fee_wei {
            self.store
                .record_monitor_error(request.id, "receipt_l1_economics_invalid")
                .await?;
            self.store.disarm("receipt_l1_economics_invalid").await?;
            return Ok(ExecutionState::Disarmed {
                reason: DisarmReason::Settlement,
            });
        }
        let fee = i128::try_from(actual_fee_wei).map_err(|_| EngineError::Arithmetic)?;
        if receipt.status == 0 {
            let outcome = ReceiptOutcome {
                receipt_status: 0,
                settled_event_found: false,
                block_number: receipt.block_number,
                gas_used: receipt.gas_used,
                effective_gas_price: receipt.effective_gas_price,
                actual_fee_wei,
                actual_l1_cost_wei: receipt.l1_fee,
                settlement: Settlement {
                    asset: self.config.pnl_asset_address,
                    flash_amount: request.flash_amount,
                    premium: 0,
                    realized_profit: 0,
                },
                net_pnl_wei: fee.checked_neg().ok_or(EngineError::Arithmetic)?,
            };
            self.store
                .mark_terminal(
                    request.id,
                    AttemptStatus::Reverted,
                    Some("transaction_reverted"),
                    Some(&outcome),
                    now,
                )
                .await?;
            return Ok(ExecutionState::Reverted {
                request_id: request.id,
                tx_hash,
            });
        }
        if receipt.status != 1 {
            self.store
                .record_monitor_error(request.id, "receipt_status_invalid")
                .await?;
            self.store.disarm("receipt_status_invalid").await?;
            return Ok(ExecutionState::Disarmed {
                reason: DisarmReason::Settlement,
            });
        }
        let settlement =
            match decode_settlement(&request, self.config.executor_address, &receipt.logs) {
                Ok(settlement) => settlement,
                Err(_) => {
                    self.store
                        .record_monitor_error(request.id, "settlement_evidence_invalid")
                        .await?;
                    self.store.disarm("settlement_evidence_invalid").await?;
                    return Ok(ExecutionState::Disarmed {
                        reason: DisarmReason::Settlement,
                    });
                }
            };
        let realized_profit =
            i128::try_from(settlement.realized_profit).map_err(|_| EngineError::Arithmetic)?;
        let outcome = ReceiptOutcome {
            receipt_status: 1,
            settled_event_found: true,
            block_number: receipt.block_number,
            gas_used: receipt.gas_used,
            effective_gas_price: receipt.effective_gas_price,
            actual_fee_wei,
            actual_l1_cost_wei: receipt.l1_fee,
            settlement,
            net_pnl_wei: realized_profit
                .checked_sub(fee)
                .ok_or(EngineError::Arithmetic)?,
        };
        self.store
            .mark_terminal(
                request.id,
                AttemptStatus::Confirmed,
                None,
                Some(&outcome),
                now,
            )
            .await?;
        let daily_loss = self.store.daily_loss_wei(now).await?;
        if daily_loss >= self.config.limits.maximum_daily_loss_wei {
            self.store.disarm("daily_loss_budget").await?;
            return Ok(ExecutionState::ConfirmedAndDisarmed {
                request_id: request.id,
                tx_hash,
                net_pnl_wei: outcome.net_pnl_wei,
            });
        }
        Ok(ExecutionState::Confirmed {
            request_id: request.id,
            tx_hash,
            net_pnl_wei: outcome.net_pnl_wei,
        })
    }

    async fn disarm_for_rpc_error(
        &self,
        error: RpcError,
        request_id: Option<Uuid>,
        now: DateTime<Utc>,
    ) -> Result<ExecutionState, EngineError> {
        let code = rpc_error_code(error.kind);
        if let Some(request_id) = request_id {
            self.store
                .mark_terminal(request_id, AttemptStatus::Failed, Some(code), None, now)
                .await?;
        }
        self.store.disarm(code).await?;
        Ok(ExecutionState::Disarmed {
            reason: if error.kind == RpcErrorKind::NonceConflict {
                DisarmReason::NonceConflict
            } else {
                DisarmReason::RpcFailure
            },
        })
    }

    async fn disarm_for_submission_error(
        &self,
        error: RpcError,
        request_id: Uuid,
        nonce: u64,
        now: DateTime<Utc>,
    ) -> Result<ExecutionState, EngineError> {
        let code = rpc_error_code(error.kind);
        self.store
            .mark_submission_unknown(request_id, code, now)
            .await?;
        self.store.disarm(code).await?;
        Ok(ExecutionState::SubmissionUnknown { request_id, nonce })
    }
}

fn validate_and_encode(
    request: &ExecutionRequest,
    config: &ExecutorConfig,
    now: DateTime<Utc>,
) -> Result<Vec<u8>, PolicyError> {
    request
        .validate_current_route()
        .map_err(|_| PolicyError::Route)?;
    if request
        .canonical_approval_digest()
        .map_err(|_| PolicyError::Approval)?
        != request.approval_digest
    {
        return Err(PolicyError::Approval);
    }
    if request.policy_version != APPROVAL_POLICY_VERSION {
        return Err(PolicyError::ApprovalPolicy);
    }
    if request.chain_id != config.chain_id
        || request.approved_at > now
        || request.approval_deadline <= now
        || request.approval_deadline > request.deadline
        || request.deadline <= now
    {
        return Err(PolicyError::Boundary);
    }
    if request.executor_address != config.executor_address
        || request.executor_code_hash != config.executor_code_hash
    {
        return Err(PolicyError::ExecutorIdentity);
    }
    if request.flash_asset != config.pnl_asset_address {
        return Err(PolicyError::ProfitAsset);
    }
    if request.flash_amount > request.maximum_input_amount
        || request.maximum_input_amount > config.limits.maximum_input_amount
    {
        return Err(PolicyError::InputCap);
    }
    if request.gas_limit > config.limits.maximum_gas_limit
        || request.max_fee_per_gas > config.limits.maximum_max_fee_per_gas
        || request.max_priority_fee_per_gas > config.limits.maximum_priority_fee_per_gas
        || request.max_priority_fee_per_gas > request.max_fee_per_gas
    {
        return Err(PolicyError::GasCap);
    }
    if request.minimum_profit < config.limits.minimum_expected_profit
        || request.expected_profit < config.limits.minimum_expected_profit
        || request.expected_profit < request.minimum_profit
    {
        return Err(PolicyError::ProfitFloor);
    }
    let worst_case_fee = u128::from(request.gas_limit)
        .checked_mul(request.max_fee_per_gas)
        .ok_or(PolicyError::LossCap)?;
    if worst_case_fee > config.limits.maximum_daily_loss_wei {
        return Err(PolicyError::LossCap);
    }
    if !config.one_transaction_at_a_time {
        return Err(PolicyError::Concurrency);
    }
    let calldata = encode_execute_opportunity(request, request.executor_address)
        .map_err(|_| PolicyError::Calldata)?;
    if hex::encode(Sha256::digest(&calldata)) != request.calldata_hash {
        return Err(PolicyError::Calldata);
    }
    Ok(calldata)
}

fn rpc_error_code(kind: RpcErrorKind) -> &'static str {
    match kind {
        RpcErrorKind::NonceConflict => "nonce_conflict",
        RpcErrorKind::ChainMismatch => "rpc_chain_mismatch",
        RpcErrorKind::Timeout => "rpc_timeout",
        RpcErrorKind::Transport => "rpc_transport_failure",
        RpcErrorKind::ResponseTooLarge => "rpc_response_too_large",
        RpcErrorKind::MalformedResponse => "rpc_malformed_response",
        RpcErrorKind::RemoteFailure => "rpc_remote_failure",
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DisarmReason {
    RpcFailure,
    NonceConflict,
    ChainMismatch,
    Policy,
    SubmissionIntegrity,
    Settlement,
    DailyLossBudget,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ExecutionState {
    DisarmedShadow,
    ArmedIdle,
    Pending {
        request_id: Uuid,
        tx_hash: TransactionHash,
    },
    SubmissionUnknown {
        request_id: Uuid,
        nonce: u64,
    },
    Confirmed {
        request_id: Uuid,
        tx_hash: TransactionHash,
        net_pnl_wei: i128,
    },
    ConfirmedAndDisarmed {
        request_id: Uuid,
        tx_hash: TransactionHash,
        net_pnl_wei: i128,
    },
    Reverted {
        request_id: Uuid,
        tx_hash: TransactionHash,
    },
    Replaced {
        request_id: Uuid,
        tx_hash: TransactionHash,
    },
    TimedOut {
        request_id: Uuid,
        tx_hash: TransactionHash,
    },
    Disarmed {
        reason: DisarmReason,
    },
}

impl ExecutionState {
    pub const fn code(&self) -> &'static str {
        match self {
            Self::DisarmedShadow => "disarmed_shadow",
            Self::ArmedIdle => "armed_idle",
            Self::Pending { .. } => "pending",
            Self::SubmissionUnknown { .. } => "submission_unknown",
            Self::Confirmed { .. } => "confirmed",
            Self::ConfirmedAndDisarmed { .. } => "confirmed_disarmed",
            Self::Reverted { .. } => "reverted",
            Self::Replaced { .. } => "replaced",
            Self::TimedOut { .. } => "timed_out",
            Self::Disarmed { .. } => "disarmed",
        }
    }
}

#[derive(Clone, Copy, Debug, Error, PartialEq, Eq)]
enum PolicyError {
    #[error("request route is outside the reviewed canary contract")]
    Route,
    #[error("request approval digest is invalid")]
    Approval,
    #[error("request approval policy is unsupported")]
    ApprovalPolicy,
    #[error("request boundary is invalid")]
    Boundary,
    #[error("request executor identity is invalid")]
    ExecutorIdentity,
    #[error("request calldata binding is invalid")]
    Calldata,
    #[error("request profit asset is unsupported")]
    ProfitAsset,
    #[error("request input exceeds cap")]
    InputCap,
    #[error("request gas exceeds cap")]
    GasCap,
    #[error("request profit is below floor")]
    ProfitFloor,
    #[error("request worst-case fee exceeds the daily loss cap")]
    LossCap,
    #[error("concurrent canary mode is forbidden")]
    Concurrency,
}

impl PolicyError {
    const fn code(self) -> &'static str {
        match self {
            Self::Route => "route_contract_mismatch",
            Self::Approval => "approval_digest_mismatch",
            Self::ApprovalPolicy => "approval_policy_mismatch",
            Self::Boundary => "request_boundary_invalid",
            Self::ExecutorIdentity => "executor_identity_mismatch",
            Self::Calldata => "calldata_hash_mismatch",
            Self::ProfitAsset => "profit_asset_invalid",
            Self::InputCap => "input_cap_exceeded",
            Self::GasCap => "gas_cap_exceeded",
            Self::ProfitFloor => "profit_floor_not_met",
            Self::LossCap => "daily_loss_bound_exceeded",
            Self::Concurrency => "concurrent_canary_forbidden",
        }
    }
}

#[derive(Debug, Error)]
pub enum EngineError {
    #[error("live executor store failed")]
    Store(#[from] StoreError),
    #[error("transaction hash could not be persisted after submission")]
    HashPersistence(StoreError),
    #[error("PhoenixExecutor ABI operation failed")]
    Abi(#[from] AbiError),
    #[error("transaction signing failed")]
    Signer(#[from] SignerError),
    #[error("active attempt is internally inconsistent")]
    ActiveAttemptInvariant,
    #[error("receipt arithmetic overflowed")]
    Arithmetic,
    #[error("receipt time calculation failed")]
    Time,
}
