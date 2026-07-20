use alloy_primitives::keccak256;
use async_trait::async_trait;
use chrono::{DateTime, Duration as ChronoDuration, TimeZone, Utc};
use phoenix_live_executor::abi::RpcLog;
use phoenix_live_executor::config::{ExecutorConfig, SafetyLimits};
use phoenix_live_executor::engine::{DisarmReason, ExecutionState, LiveExecutor};
use phoenix_live_executor::model::{
    ActiveAttempt, AttemptStatus, CanonicalAddress, ExecutionRequest, ReceiptOutcome,
    TransactionHash, ValidatedLeg,
};
use phoenix_live_executor::rpc::{ExecutionRpc, RpcError, RpcErrorKind, TransactionReceipt};
use phoenix_live_executor::signer::TransactionSigner;
use phoenix_live_executor::store::{ControlState, ExecutorStore, StoreError};
use phoenix_live_executor::{ARBITRUM_ONE_CHAIN_ID, ARBITRUM_WETH_ADDRESS, REQUEST_SCHEMA_VERSION};
use primitive_types::U256;
use std::collections::VecDeque;
use std::sync::{Arc, Mutex};
use std::time::Duration;
use url::Url;
use uuid::Uuid;

#[derive(Clone)]
struct FakeStore {
    state: Arc<Mutex<FakeStoreState>>,
}

struct FakeStoreState {
    control: ControlState,
    requests: VecDeque<ExecutionRequest>,
    active: Option<ActiveAttempt>,
    terminal: Vec<(Uuid, AttemptStatus, Option<&'static str>)>,
    outcomes: Vec<ReceiptOutcome>,
    daily_loss: u128,
    disarm_reason: Option<&'static str>,
    next_nonce: u64,
}

impl FakeStore {
    fn new(requests: Vec<ExecutionRequest>) -> Self {
        Self {
            state: Arc::new(Mutex::new(FakeStoreState {
                control: ControlState {
                    armed: true,
                    kill_switch: false,
                },
                requests: requests.into(),
                active: None,
                terminal: Vec::new(),
                outcomes: Vec::new(),
                daily_loss: 0,
                disarm_reason: None,
                next_nonce: 0,
            })),
        }
    }

    fn set_kill_switch(&self) {
        self.state.lock().expect("state").control.kill_switch = true;
    }

    fn set_daily_loss(&self, value: u128) {
        self.state.lock().expect("state").daily_loss = value;
    }

    fn disarm_reason(&self) -> Option<&'static str> {
        self.state.lock().expect("state").disarm_reason
    }

    fn terminal_statuses(&self) -> Vec<AttemptStatus> {
        self.state
            .lock()
            .expect("state")
            .terminal
            .iter()
            .map(|(_, status, _)| *status)
            .collect()
    }

    fn daily_loss(&self) -> u128 {
        self.state.lock().expect("state").daily_loss
    }

    fn active_status(&self) -> Option<AttemptStatus> {
        self.state
            .lock()
            .expect("state")
            .active
            .as_ref()
            .map(|attempt| attempt.status)
    }

    fn next_nonce(&self) -> u64 {
        self.state.lock().expect("state").next_nonce
    }
}

#[async_trait]
impl ExecutorStore for FakeStore {
    async fn validate_schema(&self) -> Result<(), StoreError> {
        Ok(())
    }

    async fn control_state(&self) -> Result<ControlState, StoreError> {
        Ok(self.state.lock().expect("state").control)
    }

    async fn active_attempt(&self) -> Result<Option<ActiveAttempt>, StoreError> {
        Ok(self.state.lock().expect("state").active.clone())
    }

    async fn claim_approved(
        &self,
        _config: &ExecutorConfig,
        _now: DateTime<Utc>,
    ) -> Result<Option<ExecutionRequest>, StoreError> {
        let mut state = self.state.lock().expect("state");
        if !state.control.armed || state.control.kill_switch || state.active.is_some() {
            return Ok(None);
        }
        let Some(request) = state.requests.pop_front() else {
            return Ok(None);
        };
        state.active = Some(ActiveAttempt {
            request: request.clone(),
            status: AttemptStatus::Claimed,
            nonce: None,
            tx_hash: None,
            submitted_at: None,
        });
        Ok(Some(request))
    }

    async fn allocate_nonce(
        &self,
        request_id: Uuid,
        _config: &ExecutorConfig,
        network_pending_nonce: u64,
    ) -> Result<u64, StoreError> {
        let mut state = self.state.lock().expect("state");
        let nonce = state.next_nonce.max(network_pending_nonce);
        state.next_nonce = nonce.checked_add(1).ok_or(StoreError::Data)?;
        let active = state.active.as_mut().ok_or(StoreError::Invariant)?;
        if active.request.id != request_id || active.status != AttemptStatus::Claimed {
            return Err(StoreError::Invariant);
        }
        active.status = AttemptStatus::NonceAllocated;
        active.nonce = Some(nonce);
        Ok(nonce)
    }

    async fn mark_pending(
        &self,
        request_id: Uuid,
        tx_hash: TransactionHash,
        submitted_at: DateTime<Utc>,
    ) -> Result<(), StoreError> {
        let mut state = self.state.lock().expect("state");
        let active = state.active.as_mut().ok_or(StoreError::Invariant)?;
        if active.request.id != request_id || active.status != AttemptStatus::NonceAllocated {
            return Err(StoreError::Invariant);
        }
        active.status = AttemptStatus::Pending;
        active.tx_hash = Some(tx_hash);
        active.submitted_at = Some(submitted_at);
        Ok(())
    }

    async fn mark_submission_unknown(
        &self,
        request_id: Uuid,
        error_code: &'static str,
        _observed_at: DateTime<Utc>,
    ) -> Result<(), StoreError> {
        let mut state = self.state.lock().expect("state");
        let active = state.active.as_mut().ok_or(StoreError::Invariant)?;
        if active.request.id != request_id || active.status != AttemptStatus::NonceAllocated {
            return Err(StoreError::Invariant);
        }
        active.status = AttemptStatus::SubmissionUnknown;
        state.terminal.push((
            request_id,
            AttemptStatus::SubmissionUnknown,
            Some(error_code),
        ));
        Ok(())
    }

    async fn fail_unsubmitted(
        &self,
        request_id: Uuid,
        error_code: &'static str,
        _terminal_at: DateTime<Utc>,
    ) -> Result<(), StoreError> {
        let mut state = self.state.lock().expect("state");
        let active = state.active.take().ok_or(StoreError::Invariant)?;
        if active.request.id != request_id || active.status != AttemptStatus::NonceAllocated {
            return Err(StoreError::Invariant);
        }
        let nonce = active.nonce.ok_or(StoreError::Invariant)?;
        if state.next_nonce != nonce.checked_add(1).ok_or(StoreError::Data)? {
            return Err(StoreError::Invariant);
        }
        state.next_nonce = nonce;
        state
            .terminal
            .push((request_id, AttemptStatus::Failed, Some(error_code)));
        Ok(())
    }

    async fn mark_terminal(
        &self,
        request_id: Uuid,
        status: AttemptStatus,
        error_code: Option<&'static str>,
        receipt_outcome: Option<&ReceiptOutcome>,
        _terminal_at: DateTime<Utc>,
    ) -> Result<(), StoreError> {
        let mut state = self.state.lock().expect("state");
        let mut active = state.active.take().ok_or(StoreError::Invariant)?;
        if active.request.id != request_id {
            return Err(StoreError::Invariant);
        }
        state.terminal.push((request_id, status, error_code));
        if let Some(outcome) = receipt_outcome {
            if outcome.net_pnl_wei < 0 {
                state.daily_loss = state
                    .daily_loss
                    .checked_add(outcome.net_pnl_wei.unsigned_abs())
                    .ok_or(StoreError::Data)?;
            }
            state.outcomes.push(outcome.clone());
        }
        if status == AttemptStatus::TimedOut {
            active.status = AttemptStatus::TimedOut;
            state.active = Some(active);
        }
        Ok(())
    }

    async fn record_monitor_error(
        &self,
        request_id: Uuid,
        error_code: &'static str,
    ) -> Result<(), StoreError> {
        let mut state = self.state.lock().expect("state");
        let active = state.active.as_ref().ok_or(StoreError::Invariant)?;
        if active.request.id != request_id {
            return Err(StoreError::Invariant);
        }
        let status = active.status;
        state.terminal.push((request_id, status, Some(error_code)));
        Ok(())
    }

    async fn daily_loss_wei(&self, _now: DateTime<Utc>) -> Result<u128, StoreError> {
        Ok(self.state.lock().expect("state").daily_loss)
    }

    async fn disarm(&self, reason: &'static str) -> Result<(), StoreError> {
        let mut state = self.state.lock().expect("state");
        state.control.armed = false;
        state.control.kill_switch = true;
        state.disarm_reason = Some(reason);
        Ok(())
    }
}

#[derive(Clone)]
struct FakeRpc {
    state: Arc<Mutex<FakeRpcState>>,
}

struct FakeRpcState {
    chain_id: u64,
    pending_nonce: u64,
    send_count: usize,
    send_error: Option<RpcError>,
    last_hash: Option<TransactionHash>,
    receipt: Option<TransactionReceipt>,
    known: bool,
    receipt_error: Option<RpcError>,
}

impl FakeRpc {
    fn new() -> Self {
        Self {
            state: Arc::new(Mutex::new(FakeRpcState {
                chain_id: ARBITRUM_ONE_CHAIN_ID,
                pending_nonce: 7,
                send_count: 0,
                send_error: None,
                last_hash: None,
                receipt: None,
                known: true,
                receipt_error: None,
            })),
        }
    }

    fn send_count(&self) -> usize {
        self.state.lock().expect("state").send_count
    }

    fn last_hash(&self) -> TransactionHash {
        self.state
            .lock()
            .expect("state")
            .last_hash
            .expect("submitted hash")
    }

    fn set_receipt(&self, receipt: TransactionReceipt) {
        self.state.lock().expect("state").receipt = Some(receipt);
    }

    fn set_known(&self, known: bool) {
        self.state.lock().expect("state").known = known;
    }

    fn set_pending_nonce(&self, nonce: u64) {
        self.state.lock().expect("state").pending_nonce = nonce;
    }

    fn set_chain_id(&self, chain_id: u64) {
        self.state.lock().expect("state").chain_id = chain_id;
    }

    fn set_send_error(&self, kind: RpcErrorKind) {
        self.state.lock().expect("state").send_error = Some(RpcError {
            kind,
            remote_code: Some(-32_000),
        });
    }

    fn set_receipt_error(&self, kind: RpcErrorKind) {
        self.state.lock().expect("state").receipt_error = Some(RpcError {
            kind,
            remote_code: None,
        });
    }
}

#[async_trait]
impl ExecutionRpc for FakeRpc {
    async fn chain_id(&self) -> Result<u64, RpcError> {
        Ok(self.state.lock().expect("state").chain_id)
    }

    async fn pending_nonce(&self, _wallet: CanonicalAddress) -> Result<u64, RpcError> {
        Ok(self.state.lock().expect("state").pending_nonce)
    }

    async fn send_raw_transaction(
        &self,
        raw_transaction: &[u8],
    ) -> Result<TransactionHash, RpcError> {
        let mut state = self.state.lock().expect("state");
        state.send_count += 1;
        if let Some(error) = state.send_error.clone() {
            return Err(error);
        }
        let hash = TransactionHash::from_bytes(keccak256(raw_transaction).0);
        state.last_hash = Some(hash);
        Ok(hash)
    }

    async fn transaction_receipt(
        &self,
        _tx_hash: TransactionHash,
    ) -> Result<Option<TransactionReceipt>, RpcError> {
        let state = self.state.lock().expect("state");
        if let Some(error) = state.receipt_error.clone() {
            return Err(error);
        }
        Ok(state.receipt.clone())
    }

    async fn transaction_known(&self, _tx_hash: TransactionHash) -> Result<bool, RpcError> {
        Ok(self.state.lock().expect("state").known)
    }
}

struct Harness {
    executor: LiveExecutor<FakeStore, FakeRpc>,
    store: FakeStore,
    rpc: FakeRpc,
    request: ExecutionRequest,
    config: ExecutorConfig,
    now: DateTime<Utc>,
}

fn harness(request_count: usize) -> Harness {
    let now = Utc
        .with_ymd_and_hms(2026, 7, 20, 10, 0, 0)
        .single()
        .expect("time");
    let signer = TransactionSigner::from_secret(&hex::encode([11_u8; 32]), ARBITRUM_ONE_CHAIN_ID)
        .expect("signer");
    let pnl_asset = CanonicalAddress::parse(ARBITRUM_WETH_ADDRESS).expect("asset");
    let executor_address =
        CanonicalAddress::parse("0x3333333333333333333333333333333333333333").expect("executor");
    let config = ExecutorConfig {
        postgres_dsn: "postgres://localhost/test".to_string(),
        rpc_url: Url::parse("https://rpc.example.invalid").expect("url"),
        rpc_allowlist: vec![Url::parse("https://rpc.example.invalid").expect("url")],
        wallet_address: signer.address(),
        executor_address,
        pnl_asset_address: pnl_asset,
        chain_id: ARBITRUM_ONE_CHAIN_ID,
        limits: SafetyLimits {
            maximum_gas_limit: 500_000,
            maximum_max_fee_per_gas: 1_000,
            maximum_priority_fee_per_gas: 100,
            maximum_input_amount: 1_000_000,
            minimum_expected_profit: 100,
            maximum_daily_loss_wei: 1_000_000_000,
        },
        receipt_timeout: Duration::from_secs(5),
        poll_interval: Duration::from_millis(10),
        one_transaction_at_a_time: true,
    };
    let request = valid_request(now, pnl_asset);
    let requests = (0..request_count)
        .map(|index| {
            let mut request = request.clone();
            request.id = Uuid::from_u128(request.id.as_u128() + index as u128);
            request.approval_digest = request
                .canonical_approval_digest()
                .expect("approval digest");
            request
        })
        .collect();
    let store = FakeStore::new(requests);
    let rpc = FakeRpc::new();
    let executor = LiveExecutor::new(config.clone(), signer, store.clone(), rpc.clone());
    Harness {
        executor,
        store,
        rpc,
        request,
        config,
        now,
    }
}

fn valid_request(now: DateTime<Utc>, flash_asset: CanonicalAddress) -> ExecutionRequest {
    let token_b =
        CanonicalAddress::parse("0x2222222222222222222222222222222222222222").expect("token");
    let mut request = ExecutionRequest {
        id: Uuid::from_u128(1),
        opportunity_id: Uuid::from_u128(2),
        schema_version: REQUEST_SCHEMA_VERSION.to_string(),
        chain_id: ARBITRUM_ONE_CHAIN_ID,
        route_id: [4_u8; 32],
        origin_router: CanonicalAddress::parse("0x4444444444444444444444444444444444444444")
            .expect("router"),
        flash_asset,
        flash_amount: 1_000,
        maximum_input_amount: 1_000,
        minimum_profit: 100,
        expected_profit: 500,
        deadline: now + ChronoDuration::seconds(60),
        legs: vec![
            ValidatedLeg {
                pool: CanonicalAddress::parse("0x5555555555555555555555555555555555555555")
                    .expect("pool"),
                token_in: flash_asset,
                token_out: token_b,
                fee: 500,
                zero_for_one: true,
                min_amount_out: 900,
            },
            ValidatedLeg {
                pool: CanonicalAddress::parse("0x6666666666666666666666666666666666666666")
                    .expect("pool"),
                token_in: token_b,
                token_out: flash_asset,
                fee: 500,
                zero_for_one: false,
                min_amount_out: 1_100,
            },
        ],
        gas_limit: 400_000,
        max_fee_per_gas: 900,
        max_priority_fee_per_gas: 90,
        approved_by: "canary-reviewer".to_string(),
        approved_at: now - ChronoDuration::seconds(1),
        policy_version: "live-canary-v1".to_string(),
        approval_digest: String::new(),
    };
    request.approval_digest = request
        .canonical_approval_digest()
        .expect("approval digest");
    request
}

fn successful_receipt(
    request: &ExecutionRequest,
    executor: CanonicalAddress,
    tx_hash: TransactionHash,
    realized_profit: u128,
) -> TransactionReceipt {
    let signature = ethabi::long_signature(
        "OpportunitySettled",
        &[
            ethabi::ParamType::FixedBytes(32),
            ethabi::ParamType::Address,
            ethabi::ParamType::Uint(256),
            ethabi::ParamType::Uint(256),
            ethabi::ParamType::Uint(256),
        ],
    );
    let mut asset_topic = [0_u8; 32];
    asset_topic[12..].copy_from_slice(request.flash_asset.as_bytes());
    let data = ethabi::encode(&[
        ethabi::Token::Uint(U256::from(request.flash_amount)),
        ethabi::Token::Uint(U256::from(1_u8)),
        ethabi::Token::Uint(U256::from(realized_profit)),
    ]);
    TransactionReceipt {
        transaction_hash: tx_hash,
        status: 1,
        block_number: 100,
        gas_used: 10,
        effective_gas_price: 10,
        logs: vec![RpcLog {
            address: executor,
            topics: vec![signature.0, request.route_id, asset_topic],
            data,
        }],
    }
}

#[tokio::test]
async fn repeated_poll_is_idempotent_while_pending() {
    let harness = harness(2);
    let first = harness.executor.step(harness.now).await.expect("submit");
    assert!(matches!(first, ExecutionState::Pending { .. }));
    let second = harness
        .executor
        .step(harness.now + ChronoDuration::seconds(1))
        .await
        .expect("reconcile");
    assert!(matches!(second, ExecutionState::Pending { .. }));
    assert_eq!(harness.rpc.send_count(), 1);
}

#[tokio::test]
async fn receipt_success_reconciles_realized_pnl() {
    let harness = harness(1);
    harness.executor.step(harness.now).await.expect("submit");
    let tx_hash = harness.rpc.last_hash();
    harness.rpc.set_receipt(successful_receipt(
        &harness.request,
        harness.config.executor_address,
        tx_hash,
        1_000,
    ));
    let state = harness
        .executor
        .step(harness.now + ChronoDuration::seconds(1))
        .await
        .expect("receipt");
    assert!(matches!(
        state,
        ExecutionState::Confirmed {
            net_pnl_wei: 900,
            ..
        }
    ));
    assert_eq!(
        harness.store.terminal_statuses(),
        vec![AttemptStatus::Confirmed]
    );
}

#[tokio::test]
async fn revert_disarms_the_canary() {
    let harness = harness(1);
    harness.executor.step(harness.now).await.expect("submit");
    let tx_hash = harness.rpc.last_hash();
    harness.rpc.set_receipt(TransactionReceipt {
        transaction_hash: tx_hash,
        status: 0,
        block_number: 100,
        gas_used: 10,
        effective_gas_price: 10,
        logs: Vec::new(),
    });
    let state = harness
        .executor
        .step(harness.now + ChronoDuration::seconds(1))
        .await
        .expect("receipt");
    assert!(matches!(state, ExecutionState::Reverted { .. }));
    assert_eq!(harness.store.disarm_reason(), Some("transaction_reverted"));
    assert_eq!(harness.store.daily_loss(), 100);
}

#[tokio::test]
async fn invalid_settlement_disarms_and_preserves_the_submitted_hash() {
    let harness = harness(1);
    harness.executor.step(harness.now).await.expect("submit");
    let tx_hash = harness.rpc.last_hash();
    harness.rpc.set_receipt(TransactionReceipt {
        transaction_hash: tx_hash,
        status: 1,
        block_number: 100,
        gas_used: 10,
        effective_gas_price: 10,
        logs: Vec::new(),
    });
    let state = harness
        .executor
        .step(harness.now + ChronoDuration::seconds(1))
        .await
        .expect("invalid settlement");
    assert_eq!(
        state,
        ExecutionState::Disarmed {
            reason: DisarmReason::Settlement
        }
    );
    assert_eq!(harness.store.active_status(), Some(AttemptStatus::Pending));
    assert_eq!(harness.rpc.send_count(), 1);
}

#[tokio::test]
async fn missing_receipt_times_out_and_disarms() {
    let harness = harness(1);
    harness.executor.step(harness.now).await.expect("submit");
    let state = harness
        .executor
        .step(harness.now + ChronoDuration::seconds(6))
        .await
        .expect("timeout");
    assert!(matches!(state, ExecutionState::TimedOut { .. }));
    assert_eq!(harness.store.disarm_reason(), Some("receipt_timeout"));
}

#[tokio::test]
async fn timed_out_hash_is_reconciled_when_a_late_receipt_arrives() {
    let harness = harness(1);
    harness.executor.step(harness.now).await.expect("submit");
    let tx_hash = harness.rpc.last_hash();

    let timed_out = harness
        .executor
        .step(harness.now + ChronoDuration::seconds(6))
        .await
        .expect("timeout");
    assert!(matches!(timed_out, ExecutionState::TimedOut { .. }));

    let still_timed_out = harness
        .executor
        .step(harness.now + ChronoDuration::seconds(7))
        .await
        .expect("continued reconciliation");
    assert!(matches!(still_timed_out, ExecutionState::TimedOut { .. }));
    assert_eq!(
        harness.store.terminal_statuses(),
        vec![AttemptStatus::TimedOut]
    );

    harness.rpc.set_receipt(successful_receipt(
        &harness.request,
        harness.config.executor_address,
        tx_hash,
        1_000,
    ));
    let confirmed = harness
        .executor
        .step(harness.now + ChronoDuration::seconds(8))
        .await
        .expect("late receipt");
    assert!(matches!(
        confirmed,
        ExecutionState::Confirmed {
            net_pnl_wei: 900,
            ..
        }
    ));
    assert_eq!(
        harness.store.terminal_statuses(),
        vec![AttemptStatus::TimedOut, AttemptStatus::Confirmed]
    );
    assert_eq!(harness.rpc.send_count(), 1);
}

#[tokio::test]
async fn advanced_network_nonce_marks_replacement() {
    let harness = harness(1);
    harness.executor.step(harness.now).await.expect("submit");
    harness.rpc.set_known(false);
    harness.rpc.set_pending_nonce(8);
    let state = harness
        .executor
        .step(harness.now + ChronoDuration::seconds(1))
        .await
        .expect("replacement");
    assert!(matches!(state, ExecutionState::Replaced { .. }));
    assert_eq!(harness.store.disarm_reason(), Some("transaction_replaced"));
}

#[tokio::test]
async fn database_kill_switch_blocks_claim_and_submission() {
    let harness = harness(1);
    harness.store.set_kill_switch();
    let state = harness.executor.step(harness.now).await.expect("disabled");
    assert_eq!(state, ExecutionState::DisarmedShadow);
    assert_eq!(harness.rpc.send_count(), 0);
}

#[tokio::test]
async fn non_arbitrum_rpc_disarms_before_claim_or_submission() {
    let harness = harness(1);
    harness.rpc.set_chain_id(1);
    let state = harness
        .executor
        .step(harness.now)
        .await
        .expect("chain mismatch");
    assert_eq!(
        state,
        ExecutionState::Disarmed {
            reason: DisarmReason::ChainMismatch
        }
    );
    assert_eq!(harness.rpc.send_count(), 0);
    assert_eq!(harness.store.disarm_reason(), Some("rpc_chain_mismatch"));
}

#[tokio::test]
async fn daily_loss_budget_blocks_claim_and_disarms() {
    let harness = harness(1);
    harness.store.set_daily_loss(1_000_000_000);
    let state = harness.executor.step(harness.now).await.expect("budget");
    assert_eq!(
        state,
        ExecutionState::Disarmed {
            reason: DisarmReason::DailyLossBudget
        }
    );
    assert_eq!(harness.rpc.send_count(), 0);
}

#[tokio::test]
async fn remaining_daily_budget_must_cover_the_worst_case_fee() {
    let harness = harness(1);
    harness.store.set_daily_loss(640_000_001);
    let state = harness
        .executor
        .step(harness.now)
        .await
        .expect("remaining budget");
    assert_eq!(
        state,
        ExecutionState::Disarmed {
            reason: DisarmReason::DailyLossBudget
        }
    );
    assert_eq!(harness.rpc.send_count(), 0);
    assert_eq!(harness.store.next_nonce(), 7);
}

#[tokio::test]
async fn gas_and_amount_caps_fail_before_submission() {
    let gas_harness = harness(1);
    {
        let mut state = gas_harness.store.state.lock().expect("state");
        state.requests[0].gas_limit = 500_001;
        state.requests[0].approval_digest = state.requests[0]
            .canonical_approval_digest()
            .expect("approval digest");
    }
    let state = gas_harness
        .executor
        .step(gas_harness.now)
        .await
        .expect("gas cap");
    assert_eq!(
        state,
        ExecutionState::Disarmed {
            reason: DisarmReason::Policy
        }
    );
    assert_eq!(gas_harness.rpc.send_count(), 0);

    let amount_harness = harness(1);
    {
        let mut state = amount_harness.store.state.lock().expect("state");
        state.requests[0].maximum_input_amount = 1_000_001;
        state.requests[0].approval_digest = state.requests[0]
            .canonical_approval_digest()
            .expect("approval digest");
    }
    let state = amount_harness
        .executor
        .step(amount_harness.now)
        .await
        .expect("amount cap");
    assert_eq!(
        state,
        ExecutionState::Disarmed {
            reason: DisarmReason::Policy
        }
    );
    assert_eq!(amount_harness.rpc.send_count(), 0);

    let max_fee_harness = harness(1);
    {
        let mut state = max_fee_harness.store.state.lock().expect("state");
        state.requests[0].max_fee_per_gas = 1_001;
        state.requests[0].approval_digest = state.requests[0]
            .canonical_approval_digest()
            .expect("approval digest");
    }
    let state = max_fee_harness
        .executor
        .step(max_fee_harness.now)
        .await
        .expect("max fee cap");
    assert_eq!(
        state,
        ExecutionState::Disarmed {
            reason: DisarmReason::Policy
        }
    );
    assert_eq!(max_fee_harness.rpc.send_count(), 0);

    let priority_fee_harness = harness(1);
    {
        let mut state = priority_fee_harness.store.state.lock().expect("state");
        state.requests[0].max_priority_fee_per_gas = 101;
        state.requests[0].approval_digest = state.requests[0]
            .canonical_approval_digest()
            .expect("approval digest");
    }
    let state = priority_fee_harness
        .executor
        .step(priority_fee_harness.now)
        .await
        .expect("priority fee cap");
    assert_eq!(
        state,
        ExecutionState::Disarmed {
            reason: DisarmReason::Policy
        }
    );
    assert_eq!(priority_fee_harness.rpc.send_count(), 0);
}

#[tokio::test]
async fn nonce_conflict_disarms_without_retry() {
    let harness = harness(1);
    harness.rpc.set_send_error(RpcErrorKind::NonceConflict);
    let state = harness
        .executor
        .step(harness.now)
        .await
        .expect("nonce conflict");
    assert!(matches!(
        state,
        ExecutionState::SubmissionUnknown { nonce: 7, .. }
    ));
    assert_eq!(harness.rpc.send_count(), 1);
    assert_eq!(harness.store.disarm_reason(), Some("nonce_conflict"));
    assert_eq!(
        harness.store.active_status(),
        Some(AttemptStatus::SubmissionUnknown)
    );

    let recovered = harness
        .executor
        .step(harness.now + ChronoDuration::seconds(1))
        .await
        .expect("preserve unknown submission");
    assert!(matches!(
        recovered,
        ExecutionState::SubmissionUnknown { nonce: 7, .. }
    ));
    assert_eq!(harness.rpc.send_count(), 1);
}

#[tokio::test]
async fn receipt_rpc_failure_disarms_but_preserves_pending_attempt() {
    let harness = harness(1);
    harness.executor.step(harness.now).await.expect("submit");
    harness.rpc.set_receipt_error(RpcErrorKind::Transport);
    let state = harness
        .executor
        .step(harness.now + ChronoDuration::seconds(1))
        .await
        .expect("rpc failure");
    assert_eq!(
        state,
        ExecutionState::Disarmed {
            reason: DisarmReason::RpcFailure
        }
    );
    assert!(harness.store.state.lock().expect("state").active.is_some());
}
