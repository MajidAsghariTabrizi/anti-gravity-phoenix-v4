use async_trait::async_trait;
use chrono::{DateTime, Duration as ChronoDuration, SecondsFormat, TimeZone, Utc};
use futures_util::StreamExt;
use phoenix_engine::amm::v3::sqrt_ratio_at_tick;
use phoenix_engine::autonomous::PostgresAutonomousCandidateStore;
use phoenix_engine::domain::Direction;
use phoenix_engine::hunter::{
    CandidateBindings, HunterBounds, HunterCore, HunterEconomicConfig, HunterEvent, HunterMode,
    HunterRouteGraph, InMemoryCandidateSink, MaterializedCandidate,
};
use phoenix_fork_sandbox::model::{
    CounterfactualResult, CounterfactualResultBody, ForkIdentity, PinnedBlockEvidence,
    PredictedEconomics, RoutePlan, SimulationEvidence, SimulationStatus, UnsignedTransactionPlan,
    VerificationEvidence, PLAN_SCHEMA_VERSION, RESULT_SCHEMA_VERSION,
};
use phoenix_live_executor::abi::encode_execute_opportunity;
use phoenix_live_executor::autonomous::{AutonomousMaterializer, MaterializationState};
use phoenix_live_executor::config::{ExecutorConfig, SafetyLimits};
use phoenix_live_executor::engine::{DisarmReason, ExecutionState, LiveExecutor};
use phoenix_live_executor::model::{
    CanonicalAddress, ExecutionLeg, ExecutionRequest, RawExecutionRequest, TransactionHash,
};
use phoenix_live_executor::rpc::{ExecutionRpc, HttpExecutionRpc, RpcError, TransactionReceipt};
use phoenix_live_executor::signer::TransactionSigner;
use phoenix_live_executor::store::{ExecutorStore, PostgresExecutorStore};
use phoenix_live_executor::{
    ARBITRUM_NATIVE_USDC_ADDRESS, ARBITRUM_ONE_CHAIN_ID, ARBITRUM_WETH_ADDRESS,
    CURRENT_ROUTE_POOL_3000_ADDRESS, CURRENT_ROUTE_POOL_500_ADDRESS, REVERSE_ROUTE_FINGERPRINT,
};
use rpc_gateway::hunter_state::{
    HunterStateResponse, PinnedV3PoolState, ProviderStateAgreement, HUNTER_STATE_RESPONSE_SCHEMA,
    PINNED_V3_STATE_SCHEMA,
};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use sqlx::types::Json;
use sqlx::{PgPool, Row};
use std::collections::BTreeMap;
use std::error::Error;
use std::io;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, OnceLock};
use std::time::Duration;
use tokio::sync::{Mutex, OnceCell};
use url::Url;
use uuid::Uuid;
use zeroize::Zeroize;

const FACTORY: &str = "0x1f98431c8ad98523631ae4a59f267346ea31f984";
const ROUTER: &str = "0x68b3465833fb72a70ecdf485e0e4c7bd8665fc45";
const RETAINED_PROFIT: u128 = 1_000_000_000_000;
const GLOBAL_LOSS_LIMIT: u128 = 10_000_000_000_000_000;
const ROUTE_LOSS_LIMIT: u128 = 10_000_000_000_000_000;
const CANDIDATE_TTL_SECONDS: i64 = 30;
const QUOTE_TTL_SECONDS: i64 = 20;
const PREAPPROVAL_ZERO_DIGEST: &str =
    "0000000000000000000000000000000000000000000000000000000000000000";

type TestResult<T = ()> = Result<T, Box<dyn Error + Send + Sync>>;

fn fixed_clock(at: DateTime<Utc>) -> impl Fn() -> DateTime<Utc> + Send + Sync + 'static {
    move || at
}

static SERVICE_LOCK: Mutex<()> = Mutex::const_new(());
static CONTROL_SNAPSHOT: OnceCell<ControlSnapshot> = OnceCell::const_new();
static BASE_EPOCH: OnceLock<i64> = OnceLock::new();

#[derive(Clone)]
struct ReadyAnvilRpc {
    inner: HttpExecutionRpc,
    pinned_block_number: u64,
    pinned_block_hash: String,
}

impl ReadyAnvilRpc {
    fn for_prepared(inner: HttpExecutionRpc, prepared: &Prepared) -> Self {
        Self {
            inner,
            pinned_block_number: prepared.bundle.event.block_number,
            pinned_block_hash: prepared.bundle.event.block_hash.clone(),
        }
    }
}

#[async_trait]
impl ExecutionRpc for ReadyAnvilRpc {
    async fn chain_id(&self) -> Result<u64, RpcError> {
        self.inner.chain_id().await
    }

    async fn finalized_block_identity(&self) -> Result<(u64, String), RpcError> {
        Ok((self.pinned_block_number, self.pinned_block_hash.clone()))
    }

    async fn execution_contract_ready(
        &self,
        _request: &ExecutionRequest,
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
        raw_transaction: &[u8],
    ) -> Result<TransactionHash, RpcError> {
        self.inner.send_raw_transaction(raw_transaction).await
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

#[derive(Clone)]
struct UnknownSubmissionRpc {
    inner: HttpExecutionRpc,
    send_count: Arc<AtomicUsize>,
    pinned_block_number: u64,
    pinned_block_hash: String,
}

#[async_trait]
impl ExecutionRpc for UnknownSubmissionRpc {
    async fn chain_id(&self) -> Result<u64, RpcError> {
        self.inner.chain_id().await
    }

    async fn finalized_block_identity(&self) -> Result<(u64, String), RpcError> {
        Ok((self.pinned_block_number, self.pinned_block_hash.clone()))
    }

    async fn execution_contract_ready(
        &self,
        _request: &ExecutionRequest,
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

#[derive(Clone)]
struct ControlSnapshot {
    legacy_armed: bool,
    legacy_kill_switch: bool,
    legacy_reason: Option<String>,
    global_armed: bool,
    global_kill_switch: bool,
    global_mode: String,
    global_reason: Option<String>,
    global_hash: Option<String>,
    global_contract: Option<Json<Value>>,
    route_enabled: bool,
    route_kill_switch: bool,
    route_fingerprint: String,
    route_reason: Option<String>,
    route_hash: Option<String>,
    route_contract: Option<Json<Value>>,
    economic_control: Json<Value>,
}

impl ControlSnapshot {
    async fn load(pool: &PgPool) -> TestResult<Self> {
        let legacy = sqlx::query(
            "SELECT armed, kill_switch, disarm_reason
             FROM live_canary.control WHERE singleton",
        )
        .fetch_one(pool)
        .await?;
        let global = sqlx::query(
            "SELECT armed, kill_switch, execution_mode, disarm_reason,
                    control_hash, control_contract
             FROM live_canary.autonomous_global_control WHERE singleton",
        )
        .fetch_one(pool)
        .await?;
        let economic_control: Json<Value> = sqlx::query_scalar(
            "SELECT to_jsonb(economic)
             FROM live_canary.economic_control economic
             WHERE singleton",
        )
        .fetch_one(pool)
        .await?;
        let route_fingerprint = economic_control
            .0
            .get("route_fingerprint")
            .and_then(Value::as_str)
            .ok_or_else(|| failure("economic route fingerprint is missing"))?
            .to_string();
        let route = sqlx::query(
            "SELECT enabled, kill_switch, disarm_reason, control_hash, control_contract
             FROM live_canary.autonomous_route_controls
             WHERE route_fingerprint = $1",
        )
        .bind(&route_fingerprint)
        .fetch_one(pool)
        .await?;
        Ok(Self {
            legacy_armed: legacy.try_get("armed")?,
            legacy_kill_switch: legacy.try_get("kill_switch")?,
            legacy_reason: legacy.try_get("disarm_reason")?,
            global_armed: global.try_get("armed")?,
            global_kill_switch: global.try_get("kill_switch")?,
            global_mode: global.try_get("execution_mode")?,
            global_reason: global.try_get("disarm_reason")?,
            global_hash: global.try_get("control_hash")?,
            global_contract: global.try_get("control_contract")?,
            route_enabled: route.try_get("enabled")?,
            route_kill_switch: route.try_get("kill_switch")?,
            route_fingerprint,
            route_reason: route.try_get("disarm_reason")?,
            route_hash: route.try_get("control_hash")?,
            route_contract: route.try_get("control_contract")?,
            economic_control,
        })
    }

    async fn restore(&self, pool: &PgPool, restored_at: DateTime<Utc>) -> TestResult {
        sqlx::query(
            "UPDATE live_canary.control
             SET armed = $1, kill_switch = $2, disarm_reason = $3, updated_at = $4
             WHERE singleton",
        )
        .bind(self.legacy_armed)
        .bind(self.legacy_kill_switch)
        .bind(&self.legacy_reason)
        .bind(restored_at)
        .execute(pool)
        .await?;
        sqlx::query(
            "UPDATE live_canary.autonomous_global_control
             SET armed = $1, kill_switch = $2, execution_mode = $3,
                 disarm_reason = $4, control_hash = $5, control_contract = $6,
                 updated_at = $7
             WHERE singleton",
        )
        .bind(self.global_armed)
        .bind(self.global_kill_switch)
        .bind(&self.global_mode)
        .bind(&self.global_reason)
        .bind(&self.global_hash)
        .bind(&self.global_contract)
        .bind(restored_at)
        .execute(pool)
        .await?;
        sqlx::query(
            "UPDATE live_canary.autonomous_route_controls
             SET enabled = $2, kill_switch = $3, disarm_reason = $4,
                 control_hash = $5, control_contract = $6, updated_at = $7
             WHERE route_fingerprint = $1",
        )
        .bind(&self.route_fingerprint)
        .bind(self.route_enabled)
        .bind(self.route_kill_switch)
        .bind(&self.route_reason)
        .bind(&self.route_hash)
        .bind(&self.route_contract)
        .bind(restored_at)
        .execute(pool)
        .await?;
        sqlx::query(
            "WITH snapshot AS (
                SELECT *
                FROM jsonb_populate_record(
                    NULL::live_canary.economic_control,
                    $1::jsonb
                )
             )
             UPDATE live_canary.economic_control economic
             SET schema_version = snapshot.schema_version,
                 phase = snapshot.phase,
                 route_fingerprint = snapshot.route_fingerprint,
                 current_size_level = snapshot.current_size_level,
                 current_input_wei = snapshot.current_input_wei,
                 maximum_reviewed_input_wei = snapshot.maximum_reviewed_input_wei,
                 release_sha = snapshot.release_sha,
                 engine_image_digest = snapshot.engine_image_digest,
                 route_universe_hash = snapshot.route_universe_hash,
                 route_policy_hash = snapshot.route_policy_hash,
                 risk_policy_hash = snapshot.risk_policy_hash,
                 executor_code_hash = snapshot.executor_code_hash,
                 readiness_id = snapshot.readiness_id,
                 authorization_id = snapshot.authorization_id,
                 cooldown_until = snapshot.cooldown_until,
                 gas_reserve_wei = snapshot.gas_reserve_wei,
                 gas_reserve_floor_wei = snapshot.gas_reserve_floor_wei,
                 control_epoch = snapshot.control_epoch,
                 last_transition_reason = snapshot.last_transition_reason,
                 state_hash = snapshot.state_hash,
                 updated_at = snapshot.updated_at
             FROM snapshot
             WHERE economic.singleton",
        )
        .bind(&self.economic_control)
        .execute(pool)
        .await?;
        Ok(())
    }
}

struct Fixture {
    scenario: &'static str,
    seed: u64,
    base: DateTime<Utc>,
    dsn: String,
    pool: PgPool,
    rpc: HttpExecutionRpc,
    config: ExecutorConfig,
    controls: ControlSnapshot,
}

#[derive(Clone)]
struct CandidateBundle {
    event: HunterEvent,
    states: BTreeMap<String, ProviderStateAgreement>,
    artifact: MaterializedCandidate,
}

struct Prepared {
    bundle: CandidateBundle,
    candidate_id: Uuid,
    request_id: Uuid,
    approval_time: DateTime<Utc>,
}

impl Fixture {
    async fn new(scenario: &'static str, seed: u64) -> TestResult<Option<Self>> {
        let Some(dsn) = std::env::var("PHOENIX_TEST_POSTGRES_DSN").ok() else {
            eprintln!("PHOENIX_TEST_POSTGRES_DSN is unset; skipping {scenario}");
            return Ok(None);
        };
        let rpc_url = required("PHOENIX_TEST_QUOTE_PROXY_RPC_URL")?;
        let executor_address = CanonicalAddress::parse(
            &required("PHOENIX_TEST_EXECUTOR_ADDRESS")?.to_ascii_lowercase(),
        )
        .map_err(boxed)?;
        let executor_code_hash = required("PHOENIX_TEST_EXECUTOR_CODE_HASH")?;
        let signer = isolated_signer()?;
        let rpc =
            HttpExecutionRpc::new_isolated_fork(Url::parse(&rpc_url)?, "CONFIRMED_LOCAL_ANVIL")
                .map_err(boxed)?;
        let config = ExecutorConfig {
            postgres_dsn: dsn.clone(),
            rpc_url: Url::parse(&rpc_url)?,
            rpc_allowlist: Vec::new(),
            wallet_address: signer.address(),
            executor_address,
            executor_code_hash,
            pnl_asset_address: CanonicalAddress::parse(ARBITRUM_WETH_ADDRESS).map_err(boxed)?,
            chain_id: ARBITRUM_ONE_CHAIN_ID,
            limits: SafetyLimits {
                maximum_gas_limit: 500_000,
                maximum_max_fee_per_gas: 10_000_000_000,
                maximum_priority_fee_per_gas: 2_000_000_000,
                maximum_input_amount: 10_000_000_000_000_000,
                minimum_expected_profit: 1,
                maximum_daily_loss_wei: GLOBAL_LOSS_LIMIT,
            },
            receipt_timeout: Duration::from_secs(10),
            poll_interval: Duration::from_millis(10),
            one_transaction_at_a_time: true,
        };
        let pool = PgPool::connect(&dsn).await?;
        let controls = if let Some(snapshot) = CONTROL_SNAPSHOT.get() {
            snapshot.clone()
        } else {
            let loaded = ControlSnapshot::load(&pool).await?;
            CONTROL_SNAPSHOT
                .set(loaded.clone())
                .map_err(|_| failure("control snapshot initialized twice"))?;
            loaded
        };
        let suite_base = *BASE_EPOCH.get_or_init(|| Utc::now().timestamp() + 300);
        let base = Utc
            .timestamp_opt(
                suite_base
                    .checked_add(
                        i64::try_from(seed)
                            .map_err(boxed)?
                            .checked_mul(60)
                            .ok_or_else(|| failure("scenario clock overflow"))?,
                    )
                    .ok_or_else(|| failure("scenario clock overflow"))?,
                0,
            )
            .single()
            .ok_or_else(|| failure("scenario clock is invalid"))?;
        reset_history(&pool).await?;
        controls.restore(&pool, base).await?;
        validate_control_budgets(&pool, &controls.route_fingerprint).await?;
        Ok(Some(Self {
            scenario,
            seed,
            base,
            dsn,
            pool,
            rpc,
            config,
            controls,
        }))
    }

    fn time(&self, variant: u64) -> TestResult<DateTime<Utc>> {
        let offset = i64::try_from(variant)
            .map_err(boxed)?
            .checked_mul(10)
            .ok_or_else(|| failure("variant clock overflow"))?;
        Ok(self.base + ChronoDuration::seconds(offset))
    }

    async fn hunter_input(
        &self,
        variant: u64,
    ) -> TestResult<(
        HunterEvent,
        BTreeMap<String, ProviderStateAgreement>,
        CandidateBindings,
    )> {
        let marker = u8::try_from((self.seed + variant) % 255 + 1).map_err(boxed)?;
        let anchor = self
            .rpc
            .quote_transaction(
                self.config.wallet_address,
                self.config.executor_address,
                &[marker],
            )
            .await
            .map_err(boxed)?;
        let at = self.time(variant)?;
        let event = HunterEvent {
            origin_event_id: format!(
                "phoenix.engine.input.v1:{}:e2e-{}-{}",
                anchor.block_number, self.seed, variant
            ),
            origin_router: ROUTER.to_string(),
            chain_id: ARBITRUM_ONE_CHAIN_ID,
            block_number: anchor.block_number,
            block_hash: anchor.block_hash.clone(),
            observed_at_unix_ms: u64::try_from(at.timestamp_millis()).map_err(boxed)?,
            evaluated_at_unix_ms: u64::try_from(at.timestamp_millis()).map_err(boxed)?,
            touched_pool_addresses: vec![if self.controls.route_fingerprint
                == REVERSE_ROUTE_FINGERPRINT
            {
                CURRENT_ROUTE_POOL_3000_ADDRESS.to_string()
            } else {
                CURRENT_ROUTE_POOL_500_ADDRESS.to_string()
            }],
            initiating_swap_direction: Some(Direction::ZeroForOne),
        };
        let states = states(
            anchor.block_number,
            &anchor.block_hash,
            self.seed
                .checked_mul(100)
                .and_then(|value| value.checked_add(variant))
                .ok_or_else(|| failure("state seed overflow"))?,
            &self.controls.route_fingerprint,
        );
        let bindings = CandidateBindings {
            risk_snapshot_hash: PREAPPROVAL_ZERO_DIGEST.to_string(),
            submission_quote_hash: PREAPPROVAL_ZERO_DIGEST.to_string(),
            executor_address: self.config.executor_address.to_string(),
            executor_code_hash: self.config.executor_code_hash.clone(),
            submission_channel: "standard_rpc".to_string(),
        };
        assert_pre_persistence_invariant(self.scenario, &bindings)?;
        Ok((event, states, bindings))
    }

    async fn candidate(&self, variant: u64) -> TestResult<CandidateBundle> {
        let (event, states, bindings) = self.hunter_input(variant).await?;
        let bounds = HunterBounds::default();
        let policy = if self.controls.route_fingerprint == REVERSE_ROUTE_FINGERPRINT {
            include_str!("../../config/phoenix-route-policy-3000-500-v1.json")
        } else {
            include_str!("../../config/phoenix-route-policy-v1.json")
        };
        let graph = HunterRouteGraph::from_contracts(
            include_str!("../../config/phoenix-route-universe-v1.json"),
            &[policy],
            bounds,
        )
        .map_err(boxed)?;
        let mut core = HunterCore::new(HunterMode::Live, graph, bounds, profitable_economics())
            .map_err(boxed)?;
        let mut sink = InMemoryCandidateSink::default();
        let first = core
            .process_event(&event, &states, &bindings, &mut sink)
            .map_err(boxed)?;
        require(
            first.candidates.len() == 1 && sink.len() == 1,
            "Hunter did not produce exactly one candidate",
        )?;
        let duplicate = core
            .process_event(&event, &states, &bindings, &mut sink)
            .map_err(boxed)?;
        require(
            duplicate.candidates.is_empty() && sink.len() == 1,
            "Hunter candidate deduplication failed",
        )?;
        let artifact = sink
            .artifacts()
            .next()
            .cloned()
            .ok_or_else(|| failure("candidate artifact is missing"))?;
        let created = text(&artifact.contract, "candidate_created_at")?;
        let expires = text(&artifact.contract, "candidate_expires_at")?;
        require(
            created == timestamp(at_whole_second(&event)?),
            "candidate creation time is not canonical",
        )?;
        require(
            expires
                == timestamp(
                    at_whole_second(&event)? + ChronoDuration::seconds(CANDIDATE_TTL_SECONDS),
                ),
            "candidate expiry is not the canonical deadline",
        )?;
        Ok(CandidateBundle {
            event,
            states,
            artifact,
        })
    }

    async fn store_candidate(&self, bundle: &CandidateBundle) -> TestResult {
        let store = PostgresAutonomousCandidateStore::connect(&self.dsn)
            .await
            .map_err(boxed)?;
        let contract = state_contract(&bundle.event, &bundle.states);
        require(
            store
                .materialize(&bundle.artifact, &contract)
                .await
                .map_err(boxed)?,
            "candidate was not inserted",
        )?;
        require(
            !store
                .materialize(&bundle.artifact, &contract)
                .await
                .map_err(boxed)?,
            "candidate database deduplication failed",
        )
    }

    async fn approved(&self, variant: u64) -> TestResult<Prepared> {
        let bundle = self.candidate(variant).await?;
        self.store_candidate(&bundle).await?;
        let approval_time = at_whole_second(&bundle.event)? + ChronoDuration::seconds(1);
        persist_candidate_fork_pass(&self.pool, &self.config, &bundle, approval_time).await?;
        let materializer = AutonomousMaterializer::connect(self.config.clone(), self.rpc.clone())
            .await
            .map_err(boxed)?;
        self.diagnose(
            "candidate approval and request materialization",
            "candidate persisted",
        )
        .await;
        let state = materializer.step(approval_time).await.map_err(boxed)?;
        let (candidate_id, request_id) = match state {
            MaterializationState::Materialized {
                candidate_id,
                request_id,
            } => (candidate_id, request_id),
            actual => {
                self.diagnose("approval materialization", &format!("{actual:?}"))
                    .await;
                return Err(failure("candidate was not approved"));
            }
        };
        let request = load_request(&self.pool, request_id).await?;
        assert_calldata_binding(&bundle, &request)?;
        require(
            request.minimum_profit
                == RETAINED_PROFIT
                    .checked_add(profitable_economics().gas_cost)
                    .ok_or_else(|| failure("minimum profit overflow"))?,
            "request minimum_profit does not preserve Hunter semantics",
        )?;
        require(
            request.deadline
                == at_whole_second(&bundle.event)? + ChronoDuration::seconds(CANDIDATE_TTL_SECONDS),
            "request deadline differs from candidate expiry",
        )?;
        require(
            request.deadline > approval_time + ChronoDuration::seconds(15),
            "request deadline does not preserve the signer inclusion margin",
        )?;
        require(
            request.approved_at == approval_time
                && request.approval_deadline
                    == approval_time + ChronoDuration::seconds(QUOTE_TTL_SECONDS),
            "approval window is not derived from the canonical clock",
        )?;
        Ok(Prepared {
            bundle,
            candidate_id,
            request_id,
            approval_time,
        })
    }

    async fn diagnose(&self, expected: &str, actual: &str) {
        let candidate =
            sqlx::query_as::<_, (String, String, String, String, String, String, String)>(
                "SELECT candidate_id::text, candidate_hash, plan_hash, status,
                    candidate_created_at::text, candidate_expires_at::text,
                    predicted_gross_profit::text
             FROM live_canary.autonomous_candidates
             ORDER BY created_at DESC LIMIT 1",
            )
            .fetch_optional(&self.pool)
            .await
            .ok()
            .flatten();
        let request = sqlx::query_as::<
            _,
            (
                String,
                String,
                String,
                String,
                String,
                String,
                String,
                String,
            ),
        >(
            "SELECT id::text, status, approved_at::text, approval_deadline::text,
                    deadline::text, minimum_profit::text, expected_profit::text,
                    (gas_limit::numeric * max_fee_per_gas)::text
             FROM live_canary.execution_requests
             ORDER BY created_at DESC LIMIT 1",
        )
        .fetch_optional(&self.pool)
        .await
        .ok()
        .flatten();
        let attempt = sqlx::query_as::<
            _,
            (
                String,
                String,
                Option<String>,
                Option<String>,
                Option<String>,
            ),
        >(
            "SELECT id::text, status, error_code, nonce::text, tx_hash
             FROM live_canary.execution_attempts ORDER BY id DESC LIMIT 1",
        )
        .fetch_optional(&self.pool)
        .await
        .ok()
        .flatten();
        let controls = sqlx::query_as::<_, (bool, bool, bool, bool, String, bool, bool)>(
            "SELECT c.armed, c.kill_switch, g.armed, g.kill_switch,
                        g.execution_mode, r.enabled, r.kill_switch
                 FROM live_canary.control c
                 CROSS JOIN live_canary.autonomous_global_control g
                 JOIN live_canary.autonomous_route_controls r
                   ON r.route_fingerprint = $1
                 WHERE c.singleton AND g.singleton",
        )
        .bind(&self.controls.route_fingerprint)
        .fetch_optional(&self.pool)
        .await
        .ok()
        .flatten();
        let economic =
            sqlx::query_as::<_, (String, String, String, Option<String>, String, String)>(
                "SELECT phase, current_size_level, current_input_wei::text,
                        cooldown_until::text, last_transition_reason,
                        updated_at::text
                 FROM live_canary.economic_control
                 WHERE singleton",
            )
            .fetch_optional(&self.pool)
            .await
            .ok()
            .flatten();
        eprintln!(
            "SCENARIO_DIAGNOSTIC scenario={} canonical_base={} expected={} actual={} retained_profit={} candidate={candidate:?} request={request:?} attempt={attempt:?} controls={controls:?} economic={economic:?}",
            self.scenario,
            timestamp(self.base),
            expected,
            actual,
            RETAINED_PROFIT,
        );
    }
}

fn assert_pre_persistence_invariant(scenario: &str, bindings: &CandidateBindings) -> TestResult {
    require(
        bindings.risk_snapshot_hash == PREAPPROVAL_ZERO_DIGEST,
        format!("{scenario}: pre-approval risk_snapshot_hash must be the zero digest"),
    )?;
    require(
        bindings.submission_quote_hash == PREAPPROVAL_ZERO_DIGEST,
        format!("{scenario}: pre-approval submission_quote_hash must be the zero digest"),
    )
}

#[tokio::test(flavor = "current_thread")]
async fn scenario_01_expired_candidate() -> TestResult {
    let _guard = SERVICE_LOCK.lock().await;
    let Some(fixture) = Fixture::new("expired_candidate", 1).await? else {
        return Ok(());
    };
    let bundle = fixture.candidate(0).await?;
    round_trip_nats_event(&bundle.event, fixture.seed).await?;
    fixture.store_candidate(&bundle).await?;
    let materializer = AutonomousMaterializer::connect(fixture.config.clone(), fixture.rpc.clone())
        .await
        .map_err(boxed)?;
    let expiry = at_whole_second(&bundle.event)? + ChronoDuration::seconds(CANDIDATE_TTL_SECONDS);
    fixture
        .diagnose("candidate expiry without request", "candidate persisted")
        .await;
    let actual = materializer.step(expiry).await.map_err(boxed)?;
    fixture
        .diagnose("Idle with candidate expired", &format!("{actual:?}"))
        .await;
    require(
        actual == MaterializationState::Idle,
        "expiry step was not idle",
    )?;
    let status: String = sqlx::query_scalar(
        "SELECT status FROM live_canary.autonomous_candidates
         WHERE candidate_hash = $1",
    )
    .bind(text(&bundle.artifact.contract, "candidate_hash")?)
    .fetch_one(&fixture.pool)
    .await?;
    require(status == "expired", "candidate was not expired")?;
    let requests: i64 = sqlx::query_scalar("SELECT count(*) FROM live_canary.execution_requests")
        .fetch_one(&fixture.pool)
        .await?;
    require(requests == 0, "expired candidate created a request")
}

#[tokio::test(flavor = "current_thread")]
async fn scenario_02_economics_rejection() -> TestResult {
    let _guard = SERVICE_LOCK.lock().await;
    let Some(fixture) = Fixture::new("economics_rejection", 2).await? else {
        return Ok(());
    };
    let (event, states, bindings) = fixture.hunter_input(0).await?;
    let bounds = HunterBounds::default();
    let graph = HunterRouteGraph::from_contracts(
        include_str!("../../config/phoenix-route-universe-v1.json"),
        &[include_str!("../../config/phoenix-route-policy-v1.json")],
        bounds,
    )
    .map_err(boxed)?;
    let mut core = HunterCore::new(
        HunterMode::Live,
        graph,
        bounds,
        HunterEconomicConfig {
            gas_cost: u128::MAX / 4,
            ..profitable_economics()
        },
    )
    .map_err(boxed)?;
    let mut sink = InMemoryCandidateSink::default();
    let result = core
        .process_event(&event, &states, &bindings, &mut sink)
        .map_err(boxed)?;
    fixture
        .diagnose("no candidate", &format!("candidate_count={}", sink.len()))
        .await;
    require(
        result.candidates.is_empty() && sink.is_empty(),
        "negative economics produced a candidate",
    )?;
    let rows: i64 = sqlx::query_scalar(
        "SELECT
            (SELECT count(*) FROM live_canary.autonomous_candidates)
            + (SELECT count(*) FROM live_canary.execution_requests)",
    )
    .fetch_one(&fixture.pool)
    .await?;
    require(rows == 0, "economics rejection mutated execution state")
}

#[tokio::test(flavor = "current_thread")]
async fn scenario_03_approval_materialization() -> TestResult {
    let _guard = SERVICE_LOCK.lock().await;
    let Some(fixture) = Fixture::new("approval_materialization", 3).await? else {
        return Ok(());
    };
    let prepared = fixture.approved(0).await?;
    fixture
        .diagnose(
            "approved request and request_materialized candidate",
            "materialized",
        )
        .await;
    let candidate_status: String = sqlx::query_scalar(
        "SELECT status FROM live_canary.autonomous_candidates WHERE candidate_id = $1",
    )
    .bind(prepared.candidate_id)
    .fetch_one(&fixture.pool)
    .await?;
    let request_status: String =
        sqlx::query_scalar("SELECT status FROM live_canary.execution_requests WHERE id = $1")
            .bind(prepared.request_id)
            .fetch_one(&fixture.pool)
            .await?;
    let approvals: i64 =
        sqlx::query_scalar("SELECT count(*) FROM live_canary.autonomous_approvals")
            .fetch_one(&fixture.pool)
            .await?;
    require(
        candidate_status == "request_materialized"
            && request_status == "approved"
            && approvals == 1,
        "approval lifecycle rows are inconsistent",
    )
}

#[tokio::test(flavor = "current_thread")]
async fn scenario_04_executor_eligibility_and_claim() -> TestResult {
    let _guard = SERVICE_LOCK.lock().await;
    let Some(fixture) = Fixture::new("executor_eligibility_and_claim", 4).await? else {
        return Ok(());
    };
    let prepared = fixture.approved(0).await?;
    let store = PostgresExecutorStore::from_pool(fixture.pool.clone());
    fixture
        .diagnose("eligible request claim", "request approved")
        .await;
    let request = store
        .claim_approved(&fixture.config, prepared.approval_time)
        .await
        .map_err(boxed)?
        .ok_or_else(|| failure("eligible request was not claimed"))?;
    fixture
        .diagnose("request/candidate/attempt claimed", "claimed")
        .await;
    require(
        request.id == prepared.request_id,
        "wrong request was claimed",
    )?;
    assert_calldata_binding(&prepared.bundle, &request)?;
    let statuses: (String, String, String) = sqlx::query_as(
        "SELECT r.status, a.status, c.status
         FROM live_canary.execution_requests r
         JOIN live_canary.execution_attempts a ON a.request_id = r.id
         JOIN live_canary.autonomous_candidates c ON c.execution_request_id = r.id
         WHERE r.id = $1",
    )
    .bind(prepared.request_id)
    .fetch_one(&fixture.pool)
    .await?;
    require(
        statuses
            == (
                "claimed".to_string(),
                "claimed".to_string(),
                "claimed".to_string(),
            ),
        "claim lifecycle transition is inconsistent",
    )
}

#[tokio::test(flavor = "current_thread")]
async fn scenario_05_signing_and_submission() -> TestResult {
    let _guard = SERVICE_LOCK.lock().await;
    let Some(fixture) = Fixture::new("signing_and_submission", 5).await? else {
        return Ok(());
    };
    let prepared = fixture.approved(0).await?;
    let nonce_before = fixture
        .rpc
        .pending_nonce(fixture.config.wallet_address)
        .await
        .map_err(boxed)?;
    let executor = LiveExecutor::new_with_clock(
        fixture.config.clone(),
        isolated_signer()?,
        PostgresExecutorStore::from_pool(fixture.pool.clone()),
        ReadyAnvilRpc::for_prepared(fixture.rpc.clone(), &prepared),
        fixed_clock(prepared.approval_time),
    );
    fixture
        .diagnose("sign and submit exactly once", "request approved")
        .await;
    let actual = executor.step(prepared.approval_time).await.map_err(boxed)?;
    fixture
        .diagnose("ExecutionState::Pending", &format!("{actual:?}"))
        .await;
    require(
        matches!(actual, ExecutionState::Pending { .. }),
        "submission did not become pending",
    )?;
    let statuses: (String, String, String) = sqlx::query_as(
        "SELECT r.status, a.status, c.status
         FROM live_canary.execution_requests r
         JOIN live_canary.execution_attempts a ON a.request_id = r.id
         JOIN live_canary.autonomous_candidates c ON c.execution_request_id = r.id
         WHERE r.id = $1",
    )
    .bind(prepared.request_id)
    .fetch_one(&fixture.pool)
    .await?;
    require(
        statuses
            == (
                "pending".to_string(),
                "pending".to_string(),
                "submitted".to_string(),
            ),
        "submission lifecycle rows are inconsistent",
    )?;
    let nonce_after = fixture
        .rpc
        .pending_nonce(fixture.config.wallet_address)
        .await
        .map_err(boxed)?;
    require(
        nonce_after == nonce_before + 1,
        "network nonce did not advance exactly once",
    )
}

#[tokio::test(flavor = "current_thread")]
async fn scenario_06_ambiguous_unknown_submission() -> TestResult {
    let _guard = SERVICE_LOCK.lock().await;
    let Some(fixture) = Fixture::new("ambiguous_unknown_submission", 6).await? else {
        return Ok(());
    };
    let prepared = fixture.approved(0).await?;
    let nonce_before = fixture
        .rpc
        .pending_nonce(fixture.config.wallet_address)
        .await
        .map_err(boxed)?;
    let send_count = Arc::new(AtomicUsize::new(0));
    let executor = LiveExecutor::new_with_clock(
        fixture.config.clone(),
        isolated_signer()?,
        PostgresExecutorStore::from_pool(fixture.pool.clone()),
        UnknownSubmissionRpc {
            inner: fixture.rpc.clone(),
            send_count: Arc::clone(&send_count),
            pinned_block_number: prepared.bundle.event.block_number,
            pinned_block_hash: prepared.bundle.event.block_hash.clone(),
        },
        fixed_clock(prepared.approval_time),
    );
    fixture
        .diagnose(
            "preserve ambiguous submission and disarm",
            "request approved",
        )
        .await;
    let actual = executor.step(prepared.approval_time).await.map_err(boxed)?;
    fixture
        .diagnose("ExecutionState::SubmissionUnknown", &format!("{actual:?}"))
        .await;
    require(
        matches!(actual, ExecutionState::SubmissionUnknown { .. }),
        "ambiguous submission was not preserved as unknown",
    )?;
    require(
        send_count.load(Ordering::SeqCst) == 1,
        "ambiguous submission retried",
    )?;
    require(
        fixture
            .rpc
            .pending_nonce(fixture.config.wallet_address)
            .await
            .map_err(boxed)?
            == nonce_before,
        "unknown submission changed the network nonce",
    )?;
    let attempt: (String, Option<String>) = sqlx::query_as(
        "SELECT status, error_code FROM live_canary.execution_attempts
         WHERE request_id = $1",
    )
    .bind(prepared.request_id)
    .fetch_one(&fixture.pool)
    .await?;
    let controls: (bool, bool, Option<String>) = sqlx::query_as(
        "SELECT armed, kill_switch, disarm_reason
         FROM live_canary.autonomous_global_control WHERE singleton",
    )
    .fetch_one(&fixture.pool)
    .await?;
    require(
        attempt
            == (
                "submission_unknown".to_string(),
                Some("nonce_conflict".to_string()),
            )
            && controls == (false, true, Some("nonce_conflict".to_string())),
        "unknown-submission fail-closed state is inconsistent",
    )
}

#[tokio::test(flavor = "current_thread")]
async fn scenario_07_receipt_reconciliation() -> TestResult {
    let _guard = SERVICE_LOCK.lock().await;
    let Some(fixture) = Fixture::new("receipt_reconciliation", 7).await? else {
        return Ok(());
    };
    let prepared = fixture.approved(0).await?;
    let executor = LiveExecutor::new_with_clock(
        fixture.config.clone(),
        isolated_signer()?,
        PostgresExecutorStore::from_pool(fixture.pool.clone()),
        ReadyAnvilRpc::for_prepared(fixture.rpc.clone(), &prepared),
        fixed_clock(prepared.approval_time),
    );
    let pending = executor.step(prepared.approval_time).await.map_err(boxed)?;
    require(
        matches!(pending, ExecutionState::Pending { .. }),
        "receipt scenario did not submit",
    )?;
    fixture
        .diagnose("reconcile mined receipt", "request pending")
        .await;
    let actual = executor
        .step(prepared.approval_time + ChronoDuration::seconds(1))
        .await
        .map_err(boxed)?;
    fixture
        .diagnose("ExecutionState::Reverted", &format!("{actual:?}"))
        .await;
    require(
        matches!(actual, ExecutionState::Reverted { .. }),
        "reverted receipt was not reconciled",
    )?;
    let outcome: (String, String) = sqlx::query_as(
        "SELECT outcome_status, actual_fee_wei::text
         FROM live_canary.execution_outcomes WHERE request_id = $1",
    )
    .bind(prepared.request_id)
    .fetch_one(&fixture.pool)
    .await?;
    require(
        outcome.0 == "reverted" && outcome.1.parse::<u128>()? > 0,
        "receipt economics were not persisted",
    )
}

#[tokio::test(flavor = "current_thread")]
async fn scenario_08_outcome_v1_and_realized_pnl() -> TestResult {
    let _guard = SERVICE_LOCK.lock().await;
    let Some(fixture) = Fixture::new("outcome_v1_and_realized_pnl", 8).await? else {
        return Ok(());
    };
    let prepared = fixture.approved(0).await?;
    let executor = LiveExecutor::new_with_clock(
        fixture.config.clone(),
        isolated_signer()?,
        PostgresExecutorStore::from_pool(fixture.pool.clone()),
        ReadyAnvilRpc::for_prepared(fixture.rpc.clone(), &prepared),
        fixed_clock(prepared.approval_time),
    );
    require(
        matches!(
            executor.step(prepared.approval_time).await.map_err(boxed)?,
            ExecutionState::Pending { .. }
        ),
        "OutcomeV1 scenario did not submit",
    )?;
    fixture
        .diagnose("persist OutcomeV1 and realized PnL", "request pending")
        .await;
    let terminal = executor
        .step(prepared.approval_time + ChronoDuration::seconds(1))
        .await
        .map_err(boxed)?;
    fixture
        .diagnose(
            "reverted OutcomeV1 with negative PnL",
            &format!("{terminal:?}"),
        )
        .await;
    require(
        matches!(terminal, ExecutionState::Reverted { .. }),
        "OutcomeV1 scenario did not reconcile",
    )?;
    let row = sqlx::query(
        "SELECT outcome_class, realized_chain_net_pnl::text AS pnl,
                actual_gas_cost::text, actual_l1_cost::text,
                outcome_hash, outcome_contract
         FROM live_canary.autonomous_outcome_attributions
         WHERE candidate_id = $1",
    )
    .bind(prepared.candidate_id)
    .fetch_one(&fixture.pool)
    .await?;
    let outcome_class: String = row.try_get("outcome_class")?;
    let pnl: i128 = row.try_get::<String, _>("pnl")?.parse()?;
    let gas: i128 = row.try_get::<String, _>("actual_gas_cost")?.parse()?;
    let l1: i128 = row.try_get::<String, _>("actual_l1_cost")?.parse()?;
    let outcome_hash: String = row.try_get("outcome_hash")?;
    let contract: Json<Value> = row.try_get("outcome_contract")?;
    require(
        outcome_class == "reverted"
            && pnl < 0
            && pnl == -(gas + l1)
            && contract.0["outcome_hash"] == outcome_hash
            && contract.0["candidate_hash"]
                == text(&prepared.bundle.artifact.contract, "candidate_hash")?,
        "OutcomeV1 binding or realized PnL is invalid",
    )?;
    let cooldown: (bool, bool, Option<String>, String) = sqlx::query_as(
        "SELECT route.enabled, route.kill_switch, route.disarm_reason, economic.phase
         FROM live_canary.autonomous_route_controls route
         CROSS JOIN live_canary.economic_control economic
         WHERE route.route_fingerprint = $1 AND economic.singleton",
    )
    .bind(&fixture.controls.route_fingerprint)
    .fetch_one(&fixture.pool)
    .await?;
    require(
        cooldown
            == (
                false,
                true,
                Some("realized_negative_cooldown".to_string()),
                "COOLDOWN".to_string(),
            ),
        "one realized loss did not immediately enter route cooldown",
    )
}

#[tokio::test(flavor = "current_thread")]
async fn scenario_09_disarm_kill_switch_and_valid_rearm() -> TestResult {
    let _guard = SERVICE_LOCK.lock().await;
    let Some(fixture) = Fixture::new("disarm_kill_switch_and_valid_rearm", 9).await? else {
        return Ok(());
    };
    let prepared = fixture.approved(0).await?;
    let nonce_before = fixture
        .rpc
        .pending_nonce(fixture.config.wallet_address)
        .await
        .map_err(boxed)?;
    sqlx::query(
        "UPDATE live_canary.autonomous_global_control
         SET armed = false, kill_switch = true, execution_mode = 'disarmed',
             disarm_reason = 'e2e_kill_switch', control_hash = NULL,
             control_contract = NULL, updated_at = $1
         WHERE singleton",
    )
    .bind(prepared.approval_time)
    .execute(&fixture.pool)
    .await?;
    let executor = LiveExecutor::new_with_clock(
        fixture.config.clone(),
        isolated_signer()?,
        PostgresExecutorStore::from_pool(fixture.pool.clone()),
        ReadyAnvilRpc::for_prepared(fixture.rpc.clone(), &prepared),
        fixed_clock(prepared.approval_time),
    );
    fixture
        .diagnose("kill switch blocks claim", "global control disarmed")
        .await;
    let actual = executor.step(prepared.approval_time).await.map_err(boxed)?;
    fixture
        .diagnose("ExecutionState::DisarmedShadow", &format!("{actual:?}"))
        .await;
    require(
        actual == ExecutionState::DisarmedShadow,
        "kill switch did not block claim",
    )?;
    let attempts: i64 = sqlx::query_scalar("SELECT count(*) FROM live_canary.execution_attempts")
        .fetch_one(&fixture.pool)
        .await?;
    require(attempts == 0, "kill switch created an attempt")?;
    require(
        fixture
            .rpc
            .pending_nonce(fixture.config.wallet_address)
            .await
            .map_err(boxed)?
            == nonce_before,
        "kill switch changed the network nonce",
    )?;
    fixture
        .controls
        .restore(
            &fixture.pool,
            prepared.approval_time + ChronoDuration::seconds(1),
        )
        .await?;
    let restored: (bool, bool, bool, bool, String, bool, bool) = sqlx::query_as(
        "SELECT c.armed, c.kill_switch, g.armed, g.kill_switch,
                g.execution_mode, g.control_hash IS NOT NULL,
                g.control_contract IS NOT NULL
         FROM live_canary.control c
         CROSS JOIN live_canary.autonomous_global_control g
         WHERE c.singleton AND g.singleton",
    )
    .fetch_one(&fixture.pool)
    .await?;
    require(
        restored == (true, false, true, false, "live".to_string(), true, true),
        "rearm did not restore both valid control contracts",
    )
}

#[tokio::test(flavor = "current_thread")]
async fn scenario_10_restart_and_nonce_recovery() -> TestResult {
    let _guard = SERVICE_LOCK.lock().await;
    let Some(fixture) = Fixture::new("restart_and_nonce_recovery", 10).await? else {
        return Ok(());
    };
    let prepared = fixture.approved(0).await?;
    let nonce_before = fixture
        .rpc
        .pending_nonce(fixture.config.wallet_address)
        .await
        .map_err(boxed)?;
    let executor = LiveExecutor::new_with_clock(
        fixture.config.clone(),
        isolated_signer()?,
        PostgresExecutorStore::from_pool(fixture.pool.clone()),
        ReadyAnvilRpc::for_prepared(fixture.rpc.clone(), &prepared),
        fixed_clock(prepared.approval_time),
    );
    require(
        matches!(
            executor.step(prepared.approval_time).await.map_err(boxed)?,
            ExecutionState::Pending { .. }
        ),
        "restart scenario did not submit",
    )?;
    drop(executor);
    let restarted = LiveExecutor::new_with_clock(
        fixture.config.clone(),
        isolated_signer()?,
        PostgresExecutorStore::from_pool(fixture.pool.clone()),
        ReadyAnvilRpc::for_prepared(fixture.rpc.clone(), &prepared),
        fixed_clock(prepared.approval_time),
    );
    fixture
        .diagnose(
            "restart reconciles pending nonce ownership",
            "request pending",
        )
        .await;
    let actual = restarted
        .step(prepared.approval_time + ChronoDuration::seconds(1))
        .await
        .map_err(boxed)?;
    fixture
        .diagnose("restart reconciliation to Reverted", &format!("{actual:?}"))
        .await;
    require(
        matches!(actual, ExecutionState::Reverted { .. }),
        "restart did not recover the pending attempt",
    )?;
    let nonce_state: String = sqlx::query_scalar(
        "SELECT next_nonce::text FROM live_canary.nonce_state
         WHERE chain_id = $1 AND wallet_address = $2",
    )
    .bind(i64::try_from(ARBITRUM_ONE_CHAIN_ID).map_err(boxed)?)
    .bind(fixture.config.wallet_address.to_string())
    .fetch_one(&fixture.pool)
    .await?;
    require(
        nonce_state.parse::<u64>()? == nonce_before + 1
            && fixture
                .rpc
                .pending_nonce(fixture.config.wallet_address)
                .await
                .map_err(boxed)?
                == nonce_before + 1,
        "restart nonce authority diverged from the network",
    )
}

#[tokio::test(flavor = "current_thread")]
async fn scenario_11_route_risk_feedback_at_threshold() -> TestResult {
    let _guard = SERVICE_LOCK.lock().await;
    let Some(fixture) = Fixture::new("route_risk_feedback_at_threshold", 11).await? else {
        return Ok(());
    };
    let mut losses = Vec::new();
    for loss_number in 1_u64..=3 {
        losses.push(fixture.approved(loss_number).await?);
    }
    for (index, prepared) in losses.iter().enumerate() {
        let loss_number = u64::try_from(index + 1).map_err(boxed)?;
        insert_synthetic_loss(
            &fixture.pool,
            prepared,
            1,
            fixture
                .seed
                .checked_mul(100)
                .and_then(|value| value.checked_add(loss_number))
                .ok_or_else(|| failure("route loss hash seed overflow"))?,
        )
        .await?;
        fixture
            .diagnose(
                "three independent reconciled route losses",
                &format!("route loss {loss_number} persisted"),
            )
            .await;
    }
    let threshold_bundle = fixture.candidate(4).await?;
    fixture.store_candidate(&threshold_bundle).await?;
    let threshold_time = at_whole_second(&threshold_bundle.event)? + ChronoDuration::seconds(1);
    persist_candidate_fork_pass(
        &fixture.pool,
        &fixture.config,
        &threshold_bundle,
        threshold_time,
    )
    .await?;
    let materializer = AutonomousMaterializer::connect(fixture.config.clone(), fixture.rpc.clone())
        .await
        .map_err(boxed)?;
    let actual = materializer.step(threshold_time).await.map_err(boxed)?;
    fixture
        .diagnose(
            "route disarmed at exactly three consecutive losses",
            &format!("{actual:?}"),
        )
        .await;
    require(
        matches!(
            actual,
            MaterializationState::Rejected {
                reason: "rejected_policy",
                ..
            }
        ),
        "three consecutive losses did not reject the next candidate",
    )?;
    let route: (bool, bool, Option<String>, String) = sqlx::query_as(
        "SELECT route.enabled, route.kill_switch, route.disarm_reason, economic.phase
         FROM live_canary.autonomous_route_controls route
         CROSS JOIN live_canary.economic_control economic
         WHERE route.route_fingerprint = $1 AND economic.singleton",
    )
    .bind(&fixture.controls.route_fingerprint)
    .fetch_one(&fixture.pool)
    .await?;
    require(
        route
            == (
                false,
                true,
                Some("maximum_consecutive_losses".to_string()),
                "DISARMED_FAILURE".to_string(),
            ),
        "route and economic state did not fail closed at the consecutive-loss threshold",
    )?;
    let global: (bool, bool) = sqlx::query_as(
        "SELECT armed, kill_switch
         FROM live_canary.autonomous_global_control WHERE singleton",
    )
    .fetch_one(&fixture.pool)
    .await?;
    require(
        global == (true, false),
        "route threshold incorrectly disarmed global control",
    )
}

#[tokio::test(flavor = "current_thread")]
async fn scenario_12_global_risk_feedback_at_threshold() -> TestResult {
    let _guard = SERVICE_LOCK.lock().await;
    let Some(fixture) = Fixture::new("global_risk_feedback_at_threshold", 12).await? else {
        return Ok(());
    };
    let prepared = fixture.approved(0).await?;
    insert_synthetic_loss(&fixture.pool, &prepared, GLOBAL_LOSS_LIMIT, fixture.seed).await?;
    let executor = LiveExecutor::new_with_clock(
        fixture.config.clone(),
        isolated_signer()?,
        PostgresExecutorStore::from_pool(fixture.pool.clone()),
        ReadyAnvilRpc::for_prepared(fixture.rpc.clone(), &prepared),
        fixed_clock(prepared.approval_time),
    );
    fixture
        .diagnose(
            "global exact-threshold loss blocks claim",
            "request approved with threshold outcome",
        )
        .await;
    let actual = executor.step(prepared.approval_time).await.map_err(boxed)?;
    fixture
        .diagnose(
            "ExecutionState::Disarmed(DailyLossBudget) at exact global threshold",
            &format!("{actual:?}"),
        )
        .await;
    require(
        actual
            == ExecutionState::Disarmed {
                reason: DisarmReason::DailyLossBudget,
            },
        "global control did not disarm at the exact loss threshold",
    )?;
    let attempts: i64 = sqlx::query_scalar("SELECT count(*) FROM live_canary.execution_attempts")
        .fetch_one(&fixture.pool)
        .await?;
    let controls: (bool, bool, bool, bool, Option<String>) = sqlx::query_as(
        "SELECT c.armed, c.kill_switch, g.armed, g.kill_switch, g.disarm_reason
         FROM live_canary.control c
         CROSS JOIN live_canary.autonomous_global_control g
         WHERE c.singleton AND g.singleton",
    )
    .fetch_one(&fixture.pool)
    .await?;
    require(
        attempts == 0
            && controls
                == (
                    false,
                    true,
                    false,
                    true,
                    Some("daily_loss_budget".to_string()),
                ),
        "global threshold did not fail closed before claim",
    )
}

async fn reset_history(pool: &PgPool) -> TestResult {
    sqlx::raw_sql(
        "TRUNCATE
            live_canary.execution_requests,
            live_canary.autonomous_candidates,
            live_canary.nonce_state
         RESTART IDENTITY CASCADE",
    )
    .execute(pool)
    .await?;
    let released = sqlx::query(
        "UPDATE live_canary.global_revenue_submission_lock
         SET active_lane = NULL, active_identity = NULL, acquired_at = NULL,
             control_epoch = control_epoch + 1
         WHERE singleton",
    )
    .execute(pool)
    .await?;
    require(
        released.rows_affected() == 1,
        "global revenue submission lock fixture row is missing",
    )?;
    Ok(())
}

async fn insert_synthetic_loss(
    pool: &PgPool,
    prepared: &Prepared,
    amount: u128,
    hash_seed: u64,
) -> TestResult {
    let synthetic_hash = format!("0x{hash_seed:064x}");
    sqlx::query(
        "INSERT INTO live_canary.execution_outcomes(
            request_id, tx_hash, outcome_status, receipt_status,
            settled_event_found, block_number, gas_used, effective_gas_price,
            actual_fee_wei, asset, flash_amount, premium, realized_profit,
            net_pnl_wei, recorded_at
         )
         SELECT id, $2, 'reverted', 0, false, 1, 1, $3::numeric,
                $3::numeric, flash_asset, flash_amount, 0, 0,
                -($3::numeric), $4
         FROM live_canary.execution_requests WHERE id = $1",
    )
    .bind(prepared.request_id)
    .bind(synthetic_hash)
    .bind(amount.to_string())
    .bind(prepared.approval_time)
    .execute(pool)
    .await?;
    Ok(())
}

async fn persist_candidate_fork_pass(
    pool: &PgPool,
    config: &ExecutorConfig,
    bundle: &CandidateBundle,
    simulated_at: DateTime<Utc>,
) -> TestResult {
    let candidate = &bundle.artifact.contract;
    let hunter_plan = &bundle.artifact.plan;
    let route = hunter_plan
        .get("route")
        .and_then(Value::as_object)
        .ok_or_else(|| failure("fork fixture route is missing"))?;
    let route_legs = route
        .get("legs")
        .and_then(Value::as_array)
        .ok_or_else(|| failure("fork fixture route legs are missing"))?;
    let simulations = hunter_plan
        .get("legs")
        .and_then(Value::as_array)
        .ok_or_else(|| failure("fork fixture simulations are missing"))?;
    require(
        !route_legs.is_empty() && route_legs.len() == simulations.len(),
        "fork fixture route and simulation legs differ",
    )?;
    let pool_ids = route_legs
        .iter()
        .map(|leg| text(leg, "pool_id"))
        .collect::<Result<Vec<_>, _>>()?;
    let pool_addresses = route_legs
        .iter()
        .map(|leg| text(leg, "pool_address"))
        .collect::<Result<Vec<_>, _>>()?;
    let protocols = route_legs
        .iter()
        .map(|leg| text(leg, "protocol_id"))
        .collect::<Result<Vec<_>, _>>()?;
    let directions = route_legs
        .iter()
        .map(|leg| text(leg, "direction"))
        .collect::<Result<Vec<_>, _>>()?;
    let fees = route_legs
        .iter()
        .map(|leg| {
            leg.get("fee")
                .and_then(Value::as_u64)
                .and_then(|value| u32::try_from(value).ok())
                .ok_or_else(|| failure("fork fixture fee is invalid"))
        })
        .collect::<Result<Vec<_>, _>>()?;
    let expected_leg_outputs = simulations
        .iter()
        .map(|leg| text(leg, "amount_out"))
        .collect::<Result<Vec<_>, _>>()?;
    let minimum_leg_outputs = simulations
        .iter()
        .map(|leg| text(leg, "minimum_output"))
        .collect::<Result<Vec<_>, _>>()?;
    let pool_state_hash_path = simulations
        .iter()
        .map(|leg| text(leg, "pool_state_hash"))
        .collect::<Result<Vec<_>, _>>()?;
    let token_path = std::iter::once(text(&route_legs[0], "token_in")?)
        .chain(
            route_legs
                .iter()
                .map(|leg| text(leg, "token_out"))
                .collect::<Result<Vec<_>, _>>()?,
        )
        .collect::<Vec<_>>();
    let decision_id = text(candidate, "opportunity_id")?;
    let route_fingerprint = text(candidate, "route_fingerprint")?;
    let state_block_number = candidate
        .get("state_block_number")
        .and_then(Value::as_u64)
        .ok_or_else(|| failure("fork fixture block number is invalid"))?;
    let selected_size = text(candidate, "selected_size")?;
    let predicted_gross = text(candidate, "predicted_gross_profit")?;
    let predicted_total_cost = text(candidate, "predicted_total_cost")?;
    let predicted_net = text(candidate, "conservative_predicted_net_pnl")?;
    let predicted_net_value = predicted_net.parse::<i128>()?;
    require(
        predicted_net_value > i128::try_from(RETAINED_PROFIT)?,
        "fork fixture predicted net is not positive",
    )?;
    let deadline = DateTime::parse_from_rfc3339(&text(candidate, "candidate_expires_at")?)?
        .timestamp()
        .try_into()?;
    let route_semantic_hash = route
        .get("semantic_hash")
        .and_then(Value::as_str)
        .ok_or_else(|| failure("fork fixture route semantic hash is missing"))?;
    let plan = UnsignedTransactionPlan {
        schema_version: PLAN_SCHEMA_VERSION.to_string(),
        shadow_decision_id: decision_id.to_string(),
        source_event_identity: bundle.event.origin_event_id.clone(),
        chain_id: ARBITRUM_ONE_CHAIN_ID,
        route: RoutePlan {
            route_id: route_semantic_hash.to_string(),
            route_fingerprint: route_fingerprint.to_string(),
            pool_ids: pool_ids.clone(),
            pool_addresses: pool_addresses.clone(),
            protocols: protocols.clone(),
            directions: directions.clone(),
            fees: fees.clone(),
        },
        token_path: token_path.clone(),
        origin_router: bundle.event.origin_router.clone(),
        input_amount: selected_size.to_string(),
        maximum_input_amount: text(hunter_plan, "maximum_input_amount")?.to_string(),
        expected_output: text(hunter_plan, "final_output")?.to_string(),
        expected_leg_outputs: expected_leg_outputs.clone(),
        minimum_output: minimum_leg_outputs
            .last()
            .cloned()
            .ok_or_else(|| failure("fork fixture minimum output is missing"))?,
        minimum_leg_outputs: minimum_leg_outputs.clone(),
        minimum_profit: RETAINED_PROFIT
            .checked_add(profitable_economics().gas_cost)
            .ok_or_else(|| failure("fork fixture minimum profit overflow"))?
            .to_string(),
        calldata: format!("0x{}", hex::encode(&bundle.artifact.calldata)),
        calldata_hash: text(candidate, "calldata_hash")?.to_string(),
        value: "0".to_string(),
        gas_estimate: 400_000,
        gas_price_wei: "1".to_string(),
        deadline,
        target_contract: config.executor_address.to_string(),
        target_code_hash: config.executor_code_hash.clone(),
        simulation_from: config.wallet_address.to_string(),
        pinned_block: PinnedBlockEvidence {
            number: state_block_number,
            hash: text(candidate, "state_block_hash")?.to_string(),
        },
        route_hash: route_semantic_hash.to_string(),
        primary_state_hash: text(candidate, "state_hash")?.to_string(),
        pool_state_hash_path: pool_state_hash_path.clone(),
        verification: VerificationEvidence {
            verification_status: "agreed".to_string(),
            independent_verification_status: "agreed".to_string(),
            agreement_state: "agreed".to_string(),
            primary_provider_id: "isolated-primary".to_string(),
            secondary_provider_id: "isolated-secondary".to_string(),
        },
        predicted: PredictedEconomics {
            gross_profit: predicted_gross.to_string(),
            total_cost: predicted_total_cost.to_string(),
            net_pnl: predicted_net.to_string(),
            minimum_required_net_pnl: RETAINED_PROFIT.to_string(),
        },
        model_version: "autonomous-live-e2e".to_string(),
        policy_version: text(candidate, "route_policy_hash")?.to_string(),
        unsigned: true,
        fork_only: true,
        shadow_only: true,
        live_execution: false,
        execution_eligible: false,
        execution_request_created: false,
        public_broadcast: false,
        signer_used: false,
    };
    let simulated_gross = predicted_net_value
        .checked_add(1)
        .ok_or_else(|| failure("fork fixture simulated gross overflow"))?;
    let result = CounterfactualResult::from_body(CounterfactualResultBody {
        schema_version: RESULT_SCHEMA_VERSION.to_string(),
        plan_hash: plan.canonical_hash()?,
        shadow_decision_id: decision_id.to_string(),
        status: SimulationStatus::Passed,
        predicted_gross_profit: plan.predicted.gross_profit.clone(),
        predicted_total_cost: plan.predicted.total_cost.clone(),
        predicted_net_pnl: plan.predicted.net_pnl.clone(),
        simulated_gross_profit: Some(simulated_gross.to_string()),
        simulated_gas_cost: Some("1".to_string()),
        simulated_balance_delta: Some(simulated_gross.to_string()),
        simulated_net_pnl: Some(predicted_net.to_string()),
        prediction_error: Some("0".to_string()),
        gas_estimate: Some(plan.gas_estimate),
        gas_used: Some(1),
        model_version: plan.model_version.clone(),
        policy_version: plan.policy_version.clone(),
        fork: ForkIdentity {
            chain_id: ARBITRUM_ONE_CHAIN_ID,
            fork_block: plan.pinned_block.clone(),
            fork_instance_hash: hex::encode(Sha256::digest(format!("fork-instance-{decision_id}"))),
            local_block: plan.pinned_block.clone(),
        },
        simulated_at,
        revert_reason: None,
        evidence: SimulationEvidence {
            rpc_methods: vec![
                "eth_call".to_string(),
                "eth_estimateGas".to_string(),
                "debug_traceCall".to_string(),
            ],
            target_code_hash: plan.target_code_hash.clone(),
            observed_pool_state_hashes: plan.pool_state_hash_path.clone(),
            observed_aggregate_state_hash: plan.primary_state_hash.clone(),
            call_output_hash: Some("1".repeat(64)),
            trace_hash: Some("2".repeat(64)),
            settled_route_hash: Some(plan.route_hash.clone()),
        },
        fork_only: true,
        shadow_only: true,
        live_execution: false,
        execution_eligible: false,
        execution_request_created: false,
        public_broadcast: false,
        signer_used: false,
    })?;
    result.validate_plan_binding(&plan).map_err(boxed)?;

    sqlx::query(
        r#"
INSERT INTO shadow_decisions (
    id, strategy, strategy_version, detector_version, code_version,
    config_version, policy_version, chain_id, source_sequence,
    observed_block, state_block, quote_block, route_fingerprint,
    disposition, primary_rejection_reason, confidence_bps, execution_eligible,
    base_net_pnl, conservative_net_pnl, severe_net_pnl, identity_evidence,
    route_evidence, market_evidence, economics_evidence, simulation_evidence,
    decision_evidence, outcome_evidence, observed_at, detected_at, decided_at,
    source_event_identity, secondary_rejection_reasons, risk_flags,
    processing_latency_ns
) VALUES (
    CAST($1 AS uuid), 'two_pool_v3_arbitrage', 'autonomous-live-e2e',
    'autonomous-live-e2e', 'autonomous-live-e2e', 'autonomous-live-e2e',
    $2, 42161, 1, $3::numeric, $3::numeric, $3::numeric, $4,
    'rejected', 'contract_path_unavailable', 10000, false,
    $5::numeric, $5::numeric, $5::numeric, '{}'::jsonb, '{}'::jsonb,
    '{}'::jsonb, '{}'::jsonb, '{}'::jsonb, '{}'::jsonb, '{}'::jsonb,
    $6, $6, $6, $7, '[]'::jsonb, '[]'::jsonb, 1
)
ON CONFLICT (id) DO NOTHING
"#,
    )
    .bind(&decision_id)
    .bind(&plan.policy_version)
    .bind(state_block_number.to_string())
    .bind(route_fingerprint)
    .bind(predicted_net)
    .bind(simulated_at)
    .bind(&bundle.event.origin_event_id)
    .execute(pool)
    .await?;
    sqlx::query(
        r#"
INSERT INTO shadow_profitability_facts (
    shadow_decision_id, source_event_identity, source_sequence,
    transaction_hash, origin_router, chain_id, route_id, route_fingerprint,
    detected_at, evaluated_at, pinned_block_number, pinned_block_hash,
    primary_state_hash, token_path, pool_path, fee_path, pool_address_path,
    protocol_path, direction_path, expected_leg_outputs, pool_state_hash_path,
    opportunity_expires_at, fork_evidence_schema_version, input_amount,
    expected_output, gross_spread, gross_profit, dex_fees, price_impact,
    execution_gas, gas_price, arbitrum_execution_fee, l1_data_fee,
    flash_loan_premium, protocol_fees, failed_attempt_reserve,
    ordering_reserve, slippage_reserve, stale_state_reserve,
    state_drift_reserve, latency_reserve, uncertainty_reserve,
    contract_overhead, total_cost, expected_net_pnl, conservative_net_pnl,
    severe_net_pnl, minimum_required_net_pnl, primary_profitability_status,
    disposition, final_rejection_reason, secondary_rejection_reasons,
    model_version, policy_version, detector_version, code_version,
    primary_provider_id, primary_response_hash, route_config_hash,
    secondary_provider_id, secondary_state_hash, secondary_block_number,
    secondary_block_hash, secondary_route_config_hash, verification_status,
    independent_verification_status, independent_verification_lifecycle,
    agreement_state, shadow_only, execution_eligible,
    execution_request_created, evidence_completeness_status
) VALUES (
    CAST($1 AS uuid), $2, 1, $3, $4, 42161, $5, $6, $7, $7,
    $8::numeric, $9, $10, $11, $12, $13, $14, $15, $16, $17, $18,
    to_timestamp($19::double precision), 'phoenix.fork-evidence.v1', $20::numeric,
    $21::numeric, $22::numeric, $22::numeric, 0, 0, 1, $23::numeric, $23::numeric, 0,
    0, 0, 0, 0, 0, 0, 0, 0, 0, 0, $23::numeric, $24::numeric,
    $24::numeric, $24::numeric, $25::numeric, 'meets_minimum', 'rejected',
    'contract_path_unavailable', '[]'::jsonb, $26, $27,
    'autonomous-live-e2e', 'autonomous-live-e2e', 'isolated-primary',
    $28, $29, 'isolated-secondary', $10, $8::numeric, $9, $29, 'agreed',
    'agreed', '["requested","agreed"]'::jsonb, 'agreed', true, false,
    false, 'complete'
)
ON CONFLICT (shadow_decision_id) DO NOTHING
"#,
    )
    .bind(&decision_id)
    .bind(&plan.source_event_identity)
    .bind(&plan.pinned_block.hash)
    .bind(&plan.origin_router)
    .bind(&plan.route.route_id)
    .bind(&plan.route.route_fingerprint)
    .bind(simulated_at)
    .bind(plan.pinned_block.number.to_string())
    .bind(&plan.pinned_block.hash)
    .bind(&plan.primary_state_hash)
    .bind(Json(&plan.token_path))
    .bind(Json(&plan.route.pool_ids))
    .bind(Json(&plan.route.fees))
    .bind(Json(&plan.route.pool_addresses))
    .bind(Json(&plan.route.protocols))
    .bind(Json(&plan.route.directions))
    .bind(Json(&plan.expected_leg_outputs))
    .bind(Json(&plan.pool_state_hash_path))
    .bind(plan.deadline.to_string())
    .bind(&plan.input_amount)
    .bind(&plan.expected_output)
    .bind(&plan.predicted.gross_profit)
    .bind(&plan.predicted.total_cost)
    .bind(&plan.predicted.net_pnl)
    .bind(&plan.predicted.minimum_required_net_pnl)
    .bind(&plan.model_version)
    .bind(&plan.policy_version)
    .bind("3".repeat(64))
    .bind(&plan.route_hash)
    .execute(pool)
    .await?;
    sqlx::query(
        r#"
INSERT INTO fork_simulation_results (
    result_hash, plan_hash, shadow_decision_id, plan_schema_version,
    result_schema_version, plan, evidence, status, predicted_gross_profit,
    predicted_total_cost, predicted_net_pnl, simulated_gross_profit,
    simulated_gas_cost, simulated_balance_delta, simulated_net_pnl,
    prediction_error, gas_estimate, gas_used, model_version, policy_version,
    fork_chain_id, fork_block_number, fork_block_hash, fork_instance_hash,
    local_block_number, local_block_hash, simulated_at, revert_reason,
    fork_only, shadow_only, live_execution, execution_eligible,
    execution_request_created, public_broadcast, signer_used
) VALUES (
    $1, $2, CAST($3 AS uuid), $4, $5, $6, $7, 'passed',
    $8::numeric, $9::numeric, $10::numeric, $11::numeric, 1, $11::numeric,
    $10::numeric, 0, $12::numeric, 1, $13, $14, 42161, $15::numeric,
    $16, $17, $15::numeric, $16, $18, NULL,
    true, true, false, false, false, false, false
)
ON CONFLICT (result_hash) DO NOTHING
"#,
    )
    .bind(&result.result_hash)
    .bind(&result.body.plan_hash)
    .bind(&decision_id)
    .bind(&plan.schema_version)
    .bind(&result.body.schema_version)
    .bind(Json(&plan))
    .bind(Json(&result.body.evidence))
    .bind(&result.body.predicted_gross_profit)
    .bind(&result.body.predicted_total_cost)
    .bind(&result.body.predicted_net_pnl)
    .bind(
        result
            .body
            .simulated_gross_profit
            .as_deref()
            .ok_or_else(|| failure("fork fixture gross result is missing"))?,
    )
    .bind(result.body.gas_estimate.unwrap_or_default().to_string())
    .bind(&result.body.model_version)
    .bind(&result.body.policy_version)
    .bind(result.body.fork.fork_block.number.to_string())
    .bind(&result.body.fork.fork_block.hash)
    .bind(&result.body.fork.fork_instance_hash)
    .bind(result.body.simulated_at)
    .execute(pool)
    .await?;
    Ok(())
}

async fn validate_control_budgets(pool: &PgPool, route_fingerprint: &str) -> TestResult {
    let global: (String, String) = sqlx::query_as(
        "SELECT daily_loss_limit::text, maximum_input_amount::text
         FROM live_canary.autonomous_global_control WHERE singleton",
    )
    .fetch_one(pool)
    .await?;
    let route: Json<Value> = sqlx::query_scalar(
        "SELECT control_contract FROM live_canary.autonomous_route_controls
         WHERE route_fingerprint = $1",
    )
    .bind(route_fingerprint)
    .fetch_one(pool)
    .await?;
    let route_loss = route
        .0
        .get("daily_loss_limit")
        .and_then(Value::as_str)
        .ok_or_else(|| failure("route loss budget is missing"))?
        .parse::<u128>()?;
    require(
        global.0.parse::<u128>()? == GLOBAL_LOSS_LIMIT
            && global.1.parse::<u128>()? > 0
            && route_loss == ROUTE_LOSS_LIMIT,
        "control budgets are zero or differ from the reviewed thresholds",
    )
}

async fn load_request(pool: &PgPool, request_id: Uuid) -> TestResult<ExecutionRequest> {
    let row = sqlx::query(
        "SELECT id, opportunity_id, schema_version, chain_id, route_id,
                route_fingerprint, route_type, route_payload,
                selected_size::text AS selected_size,
                token_path, origin_router, executor_address, executor_code_hash,
                calldata_hash, simulation_result_hash, plan_hash,
                pinned_block_number::text AS pinned_block_number,
                pinned_block_hash, flash_asset, flash_amount::text AS flash_amount,
                maximum_input_amount::text AS maximum_input_amount,
                minimum_profit::text AS minimum_profit,
                expected_profit::text AS expected_profit, deadline, legs,
                gas_limit, max_fee_per_gas::text AS max_fee_per_gas,
                max_priority_fee_per_gas::text AS max_priority_fee_per_gas,
                approved_by, approved_at, approval_deadline, policy_version,
                approval_digest
         FROM live_canary.execution_requests WHERE id = $1",
    )
    .bind(request_id)
    .fetch_one(pool)
    .await?;
    let token_path: Json<Vec<String>> = row.try_get("token_path")?;
    let legs: Json<Vec<ExecutionLeg>> = row.try_get("legs")?;
    RawExecutionRequest {
        id: row.try_get("id")?,
        opportunity_id: row.try_get("opportunity_id")?,
        schema_version: row.try_get("schema_version")?,
        chain_id: row.try_get("chain_id")?,
        route_id: row.try_get("route_id")?,
        route_fingerprint: row.try_get("route_fingerprint")?,
        route_type: row.try_get("route_type")?,
        route_payload: row.try_get("route_payload")?,
        selected_size: row.try_get("selected_size")?,
        token_path: token_path.0,
        origin_router: row.try_get("origin_router")?,
        executor_address: row.try_get("executor_address")?,
        executor_code_hash: row.try_get("executor_code_hash")?,
        calldata_hash: row.try_get("calldata_hash")?,
        simulation_result_hash: row.try_get("simulation_result_hash")?,
        plan_hash: row.try_get("plan_hash")?,
        pinned_block_number: row.try_get::<String, _>("pinned_block_number")?.parse()?,
        pinned_block_hash: row.try_get("pinned_block_hash")?,
        flash_asset: row.try_get("flash_asset")?,
        flash_amount: row.try_get("flash_amount")?,
        maximum_input_amount: row.try_get("maximum_input_amount")?,
        minimum_profit: row.try_get("minimum_profit")?,
        expected_profit: row.try_get("expected_profit")?,
        deadline: row.try_get("deadline")?,
        legs: legs.0,
        gas_limit: row.try_get("gas_limit")?,
        max_fee_per_gas: row.try_get("max_fee_per_gas")?,
        max_priority_fee_per_gas: row.try_get("max_priority_fee_per_gas")?,
        approved_by: row.try_get("approved_by")?,
        approved_at: row.try_get("approved_at")?,
        approval_deadline: row.try_get("approval_deadline")?,
        policy_version: row.try_get("policy_version")?,
        approval_digest: row.try_get("approval_digest")?,
    }
    .validate()
    .map_err(boxed)
}

fn assert_calldata_binding(bundle: &CandidateBundle, request: &ExecutionRequest) -> TestResult {
    let rebuilt = encode_execute_opportunity(request, request.executor_address).map_err(boxed)?;
    let rebuilt_hash = hex::encode(Sha256::digest(&rebuilt));
    let candidate_hash = text(&bundle.artifact.contract, "calldata_hash")?;
    require(
        rebuilt == bundle.artifact.calldata
            && rebuilt_hash == candidate_hash
            && request.calldata_hash == candidate_hash,
        "Hunter and materializer calldata differ",
    )
}

async fn round_trip_nats_event(event: &HunterEvent, seed: u64) -> TestResult {
    let nats = async_nats::connect(required("PHOENIX_TEST_NATS_URL")?).await?;
    let subject = format!("phoenix.test.autonomous-live-e2e.{seed}");
    let mut subscriber = nats.subscribe(subject.clone()).await?;
    let event_value = json!({
        "origin_event_id": event.origin_event_id,
        "origin_router": event.origin_router,
        "chain_id": event.chain_id,
        "block_number": event.block_number,
        "block_hash": event.block_hash,
        "observed_at_unix_ms": event.observed_at_unix_ms,
        "touched_pool_addresses": event.touched_pool_addresses,
    });
    nats.publish(subject, serde_json::to_vec(&event_value)?.into())
        .await?;
    nats.flush().await?;
    let message = tokio::time::timeout(Duration::from_secs(2), subscriber.next())
        .await?
        .ok_or_else(|| failure("NATS event stream ended"))?;
    let received: Value = serde_json::from_slice(&message.payload)?;
    require(
        received == event_value,
        "NATS event changed during round trip",
    )
}

fn isolated_signer() -> TestResult<TransactionSigner> {
    let mut secret = required("PHOENIX_TEST_ISOLATED_FORK_SIGNER_KEY")?;
    let signer = TransactionSigner::from_secret(&secret, ARBITRUM_ONE_CHAIN_ID).map_err(boxed);
    secret.zeroize();
    signer
}

fn profitable_economics() -> HunterEconomicConfig {
    HunterEconomicConfig {
        flash_premium_bps: 5,
        gas_cost: 1,
        tick_crossing_gas_cost: 1,
        ordering_cost_reserve: 0,
        model_error_reserve_bps: 10,
        shadow_maximum_input: 100_000_000_000_000,
    }
}

fn state(
    block_number: u64,
    block_hash: &str,
    seed: u64,
    (pool_id, pool_address, fee, spacing, tick): (&str, &str, u32, i32, i32),
) -> PinnedV3PoolState {
    let mut value = PinnedV3PoolState {
        schema_version: PINNED_V3_STATE_SCHEMA.to_string(),
        chain_id: ARBITRUM_ONE_CHAIN_ID,
        block_number,
        block_hash: block_hash.to_string(),
        pool_id: pool_id.to_string(),
        pool_address: pool_address.to_string(),
        pool_code_hash: format!("{seed:064x}"),
        factory_address: FACTORY.to_string(),
        protocol_id: "uniswap-v3".to_string(),
        token0: ARBITRUM_WETH_ADDRESS.to_string(),
        token1: ARBITRUM_NATIVE_USDC_ADDRESS.to_string(),
        fee,
        tick_spacing: spacing,
        sqrt_price_x96: sqrt_ratio_at_tick(tick)
            .expect("reviewed fixture tick")
            .to_string(),
        tick,
        liquidity: "1000000000000000000000000000000".to_string(),
        coverage_min_tick: tick - spacing * 4,
        coverage_max_tick: tick + spacing * 4,
        tick_bitmap_words: Vec::new(),
        initialized_ticks: Vec::new(),
        state_hash: "0".repeat(64),
    };
    value.state_hash = value.canonical_hash().expect("canonical state hash");
    value
}

fn states(
    block_number: u64,
    block_hash: &str,
    seed: u64,
    route_fingerprint: &str,
) -> BTreeMap<String, ProviderStateAgreement> {
    let reverse = route_fingerprint == REVERSE_ROUTE_FINGERPRINT;
    let mut states = BTreeMap::new();
    for (index, pool_id, address, fee, spacing, tick) in [
        (
            1_u64,
            "uniswap-v3-weth-usdc-500",
            CURRENT_ROUTE_POOL_500_ADDRESS,
            500,
            10,
            if reverse { -300 } else { 0 },
        ),
        (
            2_u64,
            "uniswap-v3-weth-usdc-3000",
            CURRENT_ROUTE_POOL_3000_ADDRESS,
            3000,
            60,
            if reverse { 0 } else { -300 },
        ),
    ] {
        let state = state(
            block_number,
            block_hash,
            seed + index,
            (pool_id, address, fee, spacing, tick),
        );
        states.insert(
            address.to_string(),
            ProviderStateAgreement {
                primary_provider_id: format!("e2e-primary-{seed}-{index}"),
                secondary_provider_id: format!("e2e-secondary-{seed}-{index}"),
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

fn at_whole_second(event: &HunterEvent) -> TestResult<DateTime<Utc>> {
    let seconds = i64::try_from(event.observed_at_unix_ms / 1_000).map_err(boxed)?;
    Utc.timestamp_opt(seconds, 0)
        .single()
        .ok_or_else(|| failure("event time is invalid"))
}

fn timestamp(value: DateTime<Utc>) -> String {
    value.to_rfc3339_opts(SecondsFormat::Secs, true)
}

fn text(value: &Value, field: &str) -> TestResult<String> {
    value
        .get(field)
        .and_then(Value::as_str)
        .map(str::to_string)
        .ok_or_else(|| failure(format!("{field} is missing")))
}

fn required(name: &str) -> TestResult<String> {
    std::env::var(name).map_err(|_| failure(format!("{name} is required")))
}

fn require(condition: bool, message: impl Into<String>) -> TestResult {
    if condition {
        Ok(())
    } else {
        Err(failure(message))
    }
}

fn failure(message: impl Into<String>) -> Box<dyn Error + Send + Sync> {
    Box::new(io::Error::other(message.into()))
}

fn boxed(error: impl Error + Send + Sync + 'static) -> Box<dyn Error + Send + Sync> {
    Box::new(error)
}
