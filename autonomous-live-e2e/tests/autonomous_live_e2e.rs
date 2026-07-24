use async_trait::async_trait;
use chrono::{Duration as ChronoDuration, Utc};
use futures_util::StreamExt;
use phoenix_engine::amm::v3::sqrt_ratio_at_tick;
use phoenix_engine::autonomous::PostgresAutonomousCandidateStore;
use phoenix_engine::hunter::{
    CandidateBindings, HunterBounds, HunterCore, HunterEconomicConfig, HunterEvent, HunterMode,
    HunterRouteGraph, InMemoryCandidateSink,
};
use phoenix_live_executor::autonomous::{AutonomousMaterializer, MaterializationState};
use phoenix_live_executor::config::{ExecutorConfig, SafetyLimits};
use phoenix_live_executor::engine::{ExecutionState, LiveExecutor};
use phoenix_live_executor::model::{CanonicalAddress, TransactionHash};
use phoenix_live_executor::rpc::{ExecutionRpc, HttpExecutionRpc, RpcError, TransactionReceipt};
use phoenix_live_executor::signer::TransactionSigner;
use phoenix_live_executor::store::PostgresExecutorStore;
use phoenix_live_executor::{
    ARBITRUM_NATIVE_USDC_ADDRESS, ARBITRUM_ONE_CHAIN_ID, ARBITRUM_WETH_ADDRESS,
    CURRENT_ROUTE_POOL_3000_ADDRESS, CURRENT_ROUTE_POOL_500_ADDRESS,
};
use rpc_gateway::hunter_state::{
    HunterStateResponse, PinnedV3PoolState, ProviderStateAgreement, HUNTER_STATE_RESPONSE_SCHEMA,
    PINNED_V3_STATE_SCHEMA,
};
use serde_json::{json, Value};
use sqlx::{PgPool, Row};
use std::collections::BTreeMap;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;
use std::time::Duration;
use url::Url;

const FACTORY: &str = "0x1f98431c8ad98523631ae4a59f267346ea31f984";
const ROUTER: &str = "0x68b3465833fb72a70ecdf485e0e4c7bd8665fc45";

#[derive(Clone)]
struct ReadyAnvilRpc(HttpExecutionRpc);

#[async_trait]
impl ExecutionRpc for ReadyAnvilRpc {
    async fn chain_id(&self) -> Result<u64, RpcError> {
        self.0.chain_id().await
    }

    async fn execution_contract_ready(
        &self,
        _request: &phoenix_live_executor::model::ExecutionRequest,
        _wallet: CanonicalAddress,
        _expected_code_hash: &str,
    ) -> Result<bool, RpcError> {
        Ok(true)
    }

    async fn pending_nonce(&self, wallet: CanonicalAddress) -> Result<u64, RpcError> {
        self.0.pending_nonce(wallet).await
    }

    async fn send_raw_transaction(
        &self,
        raw_transaction: &[u8],
    ) -> Result<TransactionHash, RpcError> {
        self.0.send_raw_transaction(raw_transaction).await
    }

    async fn transaction_receipt(
        &self,
        tx_hash: TransactionHash,
    ) -> Result<Option<TransactionReceipt>, RpcError> {
        self.0.transaction_receipt(tx_hash).await
    }

    async fn transaction_known(&self, tx_hash: TransactionHash) -> Result<bool, RpcError> {
        self.0.transaction_known(tx_hash).await
    }
}

#[derive(Clone)]
struct UnknownSubmissionRpc {
    inner: HttpExecutionRpc,
    send_count: Arc<AtomicUsize>,
}

#[async_trait]
impl ExecutionRpc for UnknownSubmissionRpc {
    async fn chain_id(&self) -> Result<u64, RpcError> {
        self.inner.chain_id().await
    }

    async fn execution_contract_ready(
        &self,
        _request: &phoenix_live_executor::model::ExecutionRequest,
        _wallet: CanonicalAddress,
        _expected_code_hash: &str,
    ) -> Result<bool, RpcError> {
        Ok(true)
    }

    async fn pending_nonce(&self, wallet: CanonicalAddress) -> Result<u64, RpcError> {
        self.inner.pending_nonce(wallet).await
    }

    async fn send_raw_transaction(
        &self,
        _raw_transaction: &[u8],
    ) -> Result<TransactionHash, RpcError> {
        self.send_count.fetch_add(1, Ordering::SeqCst);
        Err(RpcError {
            kind: phoenix_live_executor::rpc::RpcErrorKind::NonceConflict,
            remote_code: Some(-32_000),
        })
    }

    async fn transaction_receipt(
        &self,
        tx_hash: TransactionHash,
    ) -> Result<Option<TransactionReceipt>, RpcError> {
        self.inner.transaction_receipt(tx_hash).await
    }

    async fn transaction_known(&self, tx_hash: TransactionHash) -> Result<bool, RpcError> {
        self.inner.transaction_known(tx_hash).await
    }
}

#[tokio::test]
async fn event_to_hunter_to_persisted_outcome_is_exactly_once() {
    let Some(dsn) = std::env::var("PHOENIX_TEST_POSTGRES_DSN").ok() else {
        eprintln!("PHOENIX_TEST_POSTGRES_DSN is unset; skipping autonomous E2E");
        return;
    };
    let rpc_url = required("PHOENIX_TEST_QUOTE_PROXY_RPC_URL");
    let nats_url = required("PHOENIX_TEST_NATS_URL");
    let executor_address =
        CanonicalAddress::parse(&required("PHOENIX_TEST_EXECUTOR_ADDRESS").to_ascii_lowercase())
            .expect("executor address");
    let executor_code_hash = required("PHOENIX_TEST_EXECUTOR_CODE_HASH");
    let block_number = required("PHOENIX_TEST_BLOCK_NUMBER")
        .parse::<u64>()
        .expect("block number");
    let block_hash = required("PHOENIX_TEST_BLOCK_HASH").to_ascii_lowercase();
    let mut signer_secret = required("PHOENIX_TEST_ISOLATED_FORK_SIGNER_KEY");
    let signer =
        TransactionSigner::from_secret(&signer_secret, ARBITRUM_ONE_CHAIN_ID).expect("signer");
    let restarted_signer =
        TransactionSigner::from_secret(&signer_secret, ARBITRUM_ONE_CHAIN_ID).expect("signer");
    let kill_switch_signer =
        TransactionSigner::from_secret(&signer_secret, ARBITRUM_ONE_CHAIN_ID).expect("signer");
    let unknown_submission_signer =
        TransactionSigner::from_secret(&signer_secret, ARBITRUM_ONE_CHAIN_ID).expect("signer");
    zeroize::Zeroize::zeroize(&mut signer_secret);
    let rpc = HttpExecutionRpc::new_isolated_fork(
        Url::parse(&rpc_url).expect("quote proxy URL"),
        "CONFIRMED_LOCAL_ANVIL",
    )
    .expect("isolated RPC");
    let config = ExecutorConfig {
        postgres_dsn: dsn.clone(),
        rpc_url: Url::parse(&rpc_url).expect("RPC URL"),
        rpc_allowlist: Vec::new(),
        wallet_address: signer.address(),
        executor_address,
        executor_code_hash: executor_code_hash.clone(),
        pnl_asset_address: CanonicalAddress::parse(ARBITRUM_WETH_ADDRESS).expect("PnL asset"),
        chain_id: ARBITRUM_ONE_CHAIN_ID,
        limits: SafetyLimits {
            maximum_gas_limit: 500_000,
            maximum_max_fee_per_gas: 10_000_000_000,
            maximum_priority_fee_per_gas: 2_000_000_000,
            maximum_input_amount: 10_000_000_000_000_000,
            minimum_expected_profit: 1,
            maximum_daily_loss_wei: 10_000_000_000_000_000,
        },
        receipt_timeout: Duration::from_secs(10),
        poll_interval: Duration::from_millis(10),
        one_transaction_at_a_time: true,
    };
    let pool = PgPool::connect(&dsn).await.expect("PostgreSQL");
    sqlx::raw_sql(
        "TRUNCATE
            live_canary.autonomous_candidates,
            live_canary.execution_requests
         CASCADE",
    )
    .execute(&pool)
    .await
    .expect("reset autonomous E2E history");

    let nats = async_nats::connect(&nats_url).await.expect("NATS");
    let subject = "phoenix.test.autonomous-live-e2e";
    let mut subscriber = nats.subscribe(subject).await.expect("subscribe");
    let event_value = json!({
        "origin_event_id": format!("phoenix.engine.input.v1:{block_number}:autonomous-e2e"),
        "origin_router": ROUTER,
        "chain_id": 42161,
        "block_number": block_number,
        "block_hash": block_hash,
        "observed_at_unix_ms": Utc::now().timestamp_millis(),
        "touched_pool_addresses": [CURRENT_ROUTE_POOL_500_ADDRESS]
    });
    nats.publish(subject, serde_json::to_vec(&event_value).unwrap().into())
        .await
        .expect("publish event");
    nats.flush().await.expect("flush event");
    let message = tokio::time::timeout(Duration::from_secs(2), subscriber.next())
        .await
        .expect("event timeout")
        .expect("event");
    let received: Value = serde_json::from_slice(&message.payload).expect("event JSON");
    let now_ms = Utc::now().timestamp_millis();
    let event = HunterEvent {
        origin_event_id: text(&received, "origin_event_id"),
        origin_router: text(&received, "origin_router"),
        chain_id: received["chain_id"].as_u64().expect("chain id"),
        block_number,
        block_hash: text(&received, "block_hash"),
        observed_at_unix_ms: u64::try_from(now_ms).expect("observed timestamp"),
        evaluated_at_unix_ms: u64::try_from(now_ms).expect("evaluated timestamp"),
        touched_pool_addresses: vec![CURRENT_ROUTE_POOL_500_ADDRESS.to_string()],
    };

    let bounds = HunterBounds::default();
    let graph = HunterRouteGraph::from_contracts(
        include_str!("../../config/phoenix-route-universe-v1.json"),
        &[include_str!("../../config/phoenix-route-policy-v1.json")],
        bounds,
    )
    .expect("route graph");
    let mut core =
        HunterCore::new(HunterMode::Live, graph, bounds, profitable_economics()).expect("Hunter");
    let states = states(block_number, &block_hash);
    let bindings = CandidateBindings {
        risk_snapshot_hash: "0".repeat(64),
        submission_quote_hash: "0".repeat(64),
        executor_address: executor_address.to_string(),
        executor_code_hash,
        submission_channel: "standard_rpc".to_string(),
    };
    let mut sink = InMemoryCandidateSink::default();
    let first = core
        .process_event(&event, &states, &bindings, &mut sink)
        .expect("Hunter event");
    assert_eq!(first.candidates.len(), 1);
    let duplicate = core
        .process_event(&event, &states, &bindings, &mut sink)
        .expect("duplicate Hunter event");
    assert!(duplicate.candidates.is_empty());
    assert_eq!(sink.len(), 1);

    let initial_state_contract = state_contract(&event, &states);
    let candidate_store = PostgresAutonomousCandidateStore::connect(&dsn)
        .await
        .expect("candidate store");
    let artifact = sink.artifacts().next().expect("candidate artifact");
    assert!(candidate_store
        .materialize(artifact, &initial_state_contract)
        .await
        .expect("materialize candidate"));
    assert!(!candidate_store
        .materialize(artifact, &initial_state_contract)
        .await
        .expect("deduplicate candidate"));
    sqlx::query(
        "UPDATE live_canary.autonomous_candidates
         SET candidate_expires_at = $1
         WHERE candidate_id::text = $2",
    )
    .bind(Utc::now() - ChronoDuration::seconds(1))
    .bind(text(&artifact.contract, "candidate_id"))
    .execute(&pool)
    .await
    .expect("expire candidate");

    let materializer = AutonomousMaterializer::connect(config.clone(), rpc.clone())
        .await
        .expect("materializer");
    assert_eq!(
        materializer.step(Utc::now()).await.expect("expire step"),
        MaterializationState::Idle
    );
    let expired_status: String = sqlx::query_scalar(
        "SELECT status FROM live_canary.autonomous_candidates
         WHERE candidate_id::text = $1",
    )
    .bind(text(&artifact.contract, "candidate_id"))
    .fetch_one(&pool)
    .await
    .expect("expired status");
    assert_eq!(expired_status, "expired");
    let expired_request_count: i64 =
        sqlx::query_scalar("SELECT count(*) FROM live_canary.execution_requests")
            .fetch_one(&pool)
            .await
            .expect("expired request count");
    assert_eq!(expired_request_count, 0);

    let negative_event = HunterEvent {
        origin_event_id: format!("{}:negative", event.origin_event_id),
        ..event.clone()
    };
    let mut negative_core = HunterCore::new(
        HunterMode::Live,
        HunterRouteGraph::from_contracts(
            include_str!("../../config/phoenix-route-universe-v1.json"),
            &[include_str!("../../config/phoenix-route-policy-v1.json")],
            bounds,
        )
        .expect("negative route graph"),
        bounds,
        HunterEconomicConfig {
            gas_cost: u128::MAX / 4,
            ..profitable_economics()
        },
    )
    .expect("negative Hunter");
    let mut negative_sink = InMemoryCandidateSink::default();
    let negative = negative_core
        .process_event(&negative_event, &states, &bindings, &mut negative_sink)
        .expect("negative economics");
    assert!(negative.candidates.is_empty());

    let executable_event = HunterEvent {
        origin_event_id: format!("{}:executable", event.origin_event_id),
        observed_at_unix_ms: u64::try_from(Utc::now().timestamp_millis())
            .expect("observed timestamp"),
        evaluated_at_unix_ms: u64::try_from(Utc::now().timestamp_millis())
            .expect("evaluated timestamp"),
        ..event.clone()
    };
    let mut executable_sink = InMemoryCandidateSink::default();
    let executable = core
        .process_event(&executable_event, &states, &bindings, &mut executable_sink)
        .expect("executable Hunter event");
    assert_eq!(executable.candidates.len(), 1);
    let executable_duplicate = core
        .process_event(&executable_event, &states, &bindings, &mut executable_sink)
        .expect("duplicate executable event");
    assert!(executable_duplicate.candidates.is_empty());
    assert!(candidate_store
        .materialize(
            executable_sink
                .artifacts()
                .next()
                .expect("executable candidate"),
            &state_contract(&executable_event, &states),
        )
        .await
        .expect("materialize executable candidate"));
    let materialized = materializer.step(Utc::now()).await.expect("approval");
    assert!(matches!(
        materialized,
        MaterializationState::Materialized { .. }
    ));
    let approved_count: i64 =
        sqlx::query_scalar("SELECT count(*) FROM live_canary.execution_requests")
            .fetch_one(&pool)
            .await
            .expect("approved request count");
    assert_eq!(approved_count, 1);

    let nonce_before = rpc
        .pending_nonce(signer.address())
        .await
        .expect("nonce before submission");
    let store = PostgresExecutorStore::from_pool(pool.clone());
    let executor = LiveExecutor::new(
        config.clone(),
        signer,
        PostgresExecutorStore::from_pool(pool.clone()),
        ReadyAnvilRpc(rpc.clone()),
    );
    assert!(matches!(
        executor.step(Utc::now()).await.expect("submission"),
        ExecutionState::Pending { .. }
    ));
    let restarted = LiveExecutor::new(
        config.clone(),
        restarted_signer,
        store,
        ReadyAnvilRpc(rpc.clone()),
    );
    let terminal = restarted
        .step(Utc::now() + ChronoDuration::seconds(1))
        .await
        .expect("restart reconciliation");
    assert!(matches!(terminal, ExecutionState::Reverted { .. }));
    assert_eq!(
        rpc.pending_nonce(
            CanonicalAddress::parse(&required("PHOENIX_TEST_WALLET_ADDRESS")).expect("wallet")
        )
        .await
        .expect("nonce after submission"),
        nonce_before + 1
    );
    let outcome = sqlx::query(
        "SELECT outcome_status, realized_chain_net_pnl::text AS pnl
         FROM live_canary.autonomous_outcome_attributions",
    )
    .fetch_one(&pool)
    .await
    .expect("OutcomeV1");
    assert_eq!(
        outcome.try_get::<String, _>("outcome_status").unwrap(),
        "reverted"
    );
    assert!(
        outcome
            .try_get::<String, _>("pnl")
            .unwrap()
            .parse::<i128>()
            .unwrap()
            < 0
    );

    let kill_event = HunterEvent {
        origin_event_id: format!("{}:kill-switch", event.origin_event_id),
        observed_at_unix_ms: u64::try_from(Utc::now().timestamp_millis())
            .expect("observed timestamp"),
        evaluated_at_unix_ms: u64::try_from(Utc::now().timestamp_millis())
            .expect("evaluated timestamp"),
        ..event.clone()
    };
    let mut kill_sink = InMemoryCandidateSink::default();
    assert_eq!(
        core.process_event(&kill_event, &states, &bindings, &mut kill_sink)
            .expect("kill-switch Hunter event")
            .candidates
            .len(),
        1
    );
    assert!(candidate_store
        .materialize(
            kill_sink.artifacts().next().expect("kill-switch candidate"),
            &state_contract(&kill_event, &states),
        )
        .await
        .expect("materialize kill-switch candidate"));
    assert!(matches!(
        materializer
            .step(Utc::now())
            .await
            .expect("kill-switch approval"),
        MaterializationState::Materialized { .. }
    ));
    let pending_nonce_before_kill = rpc
        .pending_nonce(
            CanonicalAddress::parse(&required("PHOENIX_TEST_WALLET_ADDRESS")).expect("wallet"),
        )
        .await
        .expect("nonce before kill switch");
    sqlx::query(
        "UPDATE live_canary.autonomous_global_control
         SET armed = false, kill_switch = true, execution_mode = 'disarmed',
             disarm_reason = 'e2e_kill_switch', control_hash = NULL,
             control_contract = NULL",
    )
    .execute(&pool)
    .await
    .expect("kill switch");
    let approved_before_kill: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM live_canary.execution_requests WHERE status = 'approved'",
    )
    .fetch_one(&pool)
    .await
    .expect("approved before kill");
    assert_eq!(approved_before_kill, 1);
    let killed_executor = LiveExecutor::new(
        config.clone(),
        kill_switch_signer,
        PostgresExecutorStore::from_pool(pool.clone()),
        ReadyAnvilRpc(rpc.clone()),
    );
    assert!(matches!(
        killed_executor.step(Utc::now()).await.expect("kill switch"),
        ExecutionState::DisarmedShadow
    ));
    assert_eq!(
        rpc.pending_nonce(
            CanonicalAddress::parse(&required("PHOENIX_TEST_WALLET_ADDRESS")).expect("wallet"),
        )
        .await
        .expect("nonce after kill switch"),
        pending_nonce_before_kill
    );
    let attempts_after_kill: i64 =
        sqlx::query_scalar("SELECT count(*) FROM live_canary.execution_attempts")
            .fetch_one(&pool)
            .await
            .expect("attempts after kill");
    assert_eq!(attempts_after_kill, 1);

    sqlx::query(
        "UPDATE live_canary.autonomous_global_control
         SET armed = true, kill_switch = false, execution_mode = 'live',
             disarm_reason = NULL
         WHERE singleton",
    )
    .execute(&pool)
    .await
    .expect("rearm unknown-submission proof");
    let unknown_send_count = Arc::new(AtomicUsize::new(0));
    let unknown_executor = LiveExecutor::new(
        config,
        unknown_submission_signer,
        PostgresExecutorStore::from_pool(pool.clone()),
        UnknownSubmissionRpc {
            inner: rpc.clone(),
            send_count: Arc::clone(&unknown_send_count),
        },
    );
    assert!(matches!(
        unknown_executor
            .step(Utc::now())
            .await
            .expect("unknown submission"),
        ExecutionState::SubmissionUnknown { .. }
    ));
    assert_eq!(unknown_send_count.load(Ordering::SeqCst), 1);
    assert_eq!(
        rpc.pending_nonce(
            CanonicalAddress::parse(&required("PHOENIX_TEST_WALLET_ADDRESS")).expect("wallet"),
        )
        .await
        .expect("nonce after unknown submission"),
        pending_nonce_before_kill
    );
    let unknown_status: String = sqlx::query_scalar(
        "SELECT status FROM live_canary.execution_attempts
         ORDER BY id DESC LIMIT 1",
    )
    .fetch_one(&pool)
    .await
    .expect("unknown attempt status");
    assert_eq!(unknown_status, "submission_unknown");
    let global_control: (bool, bool, Option<String>) = sqlx::query_as(
        "SELECT armed, kill_switch, disarm_reason
         FROM live_canary.autonomous_global_control WHERE singleton",
    )
    .fetch_one(&pool)
    .await
    .expect("unknown global control");
    assert_eq!(
        global_control,
        (false, true, Some("nonce_conflict".to_string()))
    );
}

fn profitable_economics() -> HunterEconomicConfig {
    HunterEconomicConfig {
        flash_premium_bps: 5,
        gas_cost: 1,
        tick_crossing_gas_cost: 1,
        ordering_cost_reserve: 0,
        model_error_reserve_bps: 10,
        shadow_maximum_input: 10_000_000_000_000_000,
    }
}

fn state(
    block_number: u64,
    block_hash: &str,
    pool_id: &str,
    pool_address: &str,
    fee: u32,
    spacing: i32,
    tick: i32,
) -> PinnedV3PoolState {
    let mut value = PinnedV3PoolState {
        schema_version: PINNED_V3_STATE_SCHEMA.to_string(),
        chain_id: 42_161,
        block_number,
        block_hash: block_hash.to_string(),
        pool_id: pool_id.to_string(),
        pool_address: pool_address.to_string(),
        pool_code_hash: "b".repeat(64),
        factory_address: FACTORY.to_string(),
        protocol_id: "uniswap-v3".to_string(),
        token0: ARBITRUM_WETH_ADDRESS.to_string(),
        token1: ARBITRUM_NATIVE_USDC_ADDRESS.to_string(),
        fee,
        tick_spacing: spacing,
        sqrt_price_x96: sqrt_ratio_at_tick(tick).expect("sqrt price").to_string(),
        tick,
        liquidity: "1000000000000000000000000000000".to_string(),
        coverage_min_tick: tick - spacing * 4,
        coverage_max_tick: tick + spacing * 4,
        tick_bitmap_words: Vec::new(),
        initialized_ticks: Vec::new(),
        state_hash: "0".repeat(64),
    };
    value.state_hash = value.canonical_hash().expect("state hash");
    value
}

fn states(block_number: u64, block_hash: &str) -> BTreeMap<String, ProviderStateAgreement> {
    let mut states = BTreeMap::new();
    for (pool_id, address, fee, spacing, tick) in [
        (
            "uniswap-v3-weth-usdc-500",
            CURRENT_ROUTE_POOL_500_ADDRESS,
            500,
            10,
            0,
        ),
        (
            "uniswap-v3-weth-usdc-3000",
            CURRENT_ROUTE_POOL_3000_ADDRESS,
            3000,
            60,
            -300,
        ),
    ] {
        let state = state(
            block_number,
            block_hash,
            pool_id,
            address,
            fee,
            spacing,
            tick,
        );
        states.insert(
            address.to_string(),
            ProviderStateAgreement {
                primary_provider_id: "e2e-primary".to_string(),
                secondary_provider_id: "e2e-secondary".to_string(),
                primary: state.clone(),
                secondary: state,
            },
        );
    }
    states
}

fn state_contract(event: &HunterEvent, states: &BTreeMap<String, ProviderStateAgreement>) -> Value {
    serde_json::to_value(HunterStateResponse {
        schema_version: HUNTER_STATE_RESPONSE_SCHEMA.to_string(),
        chain_id: event.chain_id,
        request_id: event.origin_event_id.clone(),
        block_number: event.block_number,
        block_hash: event.block_hash.clone(),
        agreements: vec![
            states[CURRENT_ROUTE_POOL_500_ADDRESS].clone(),
            states[CURRENT_ROUTE_POOL_3000_ADDRESS].clone(),
        ],
        resolved_at_unix_ms: event.evaluated_at_unix_ms,
    })
    .expect("state contract")
}

fn text(value: &Value, field: &str) -> String {
    value[field].as_str().expect("event field").to_string()
}

fn required(name: &str) -> String {
    std::env::var(name).unwrap_or_else(|_| panic!("{name} is required"))
}
