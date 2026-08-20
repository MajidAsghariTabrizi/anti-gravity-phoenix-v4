use crate::aave_state::{
    AaveAccountData, AaveExactLiquidationState, AaveExactProviderState, AaveExactRequest,
    AaveExactReserveState, AaveExactResponse, AaveExactUnwindQuoteState, AavePrimaryScreenResponse,
    AaveProviderScreen, AaveScreenRequest, AaveScreenResponse, AaveSimulateBatchError,
    AaveSimulateBatchRequest, AaveSimulateBatchResponse, AaveSimulateBatchResult,
    AaveSimulateRequest, AaveSimulateResponse, AaveTailRequest, AaveTailResponse, SizeLevel,
    AAVE_EXACT_RESPONSE_SCHEMA, AAVE_PRIMARY_PROVIDER_ID, AAVE_PRIMARY_SCREEN_RESPONSE_SCHEMA,
    AAVE_SCREEN_RESPONSE_SCHEMA, AAVE_SIMULATE_BATCH_RESPONSE_SCHEMA,
    AAVE_SIMULATE_RESPONSE_SCHEMA, AAVE_TAIL_RESPONSE_SCHEMA, AAVE_V3_POOL_ARBITRUM,
    MAXIMUM_REVIEWED_INPUT_WEI, SINGLE_PRIMARY_ATLAS_CALLBACK_FORK_EVIDENCE,
    SINGLE_PRIMARY_COUNTERFACTUAL_FORK_EVIDENCE, SINGLE_PRIMARY_FORK_EVIDENCE,
};
use crate::budget::GlobalBudget;
use crate::cache::TtlCache;
use crate::economic::{
    compare_provider_results, MethodTimeouts, PinnedBlock, ProviderResult, RpcMethod,
};
use crate::hunter_state::{
    HunterStateError, HunterStateRequest, HunterStateResponse, InitializedTick, PinnedV3PoolState,
    ProviderStateAgreement, TickBitmapWord, HUNTER_STATE_RESPONSE_SCHEMA,
};
use crate::metrics::{ProviderSlot, RuntimeRpcMetrics, UpstreamOutcome};
use crate::multicall::{decode_aggregate3, encode_aggregate3, EthCall, MULTICALL3_ADDRESS};
use crate::providers::{ProviderConfig, ProviderLease, ProviderPool};
use crate::runtime_state::GatewayReadiness;
use crate::shadow_state::{
    canonical_block_hash, canonical_data, canonical_hash_bytes, EvidenceRequest,
    GatewayErrorResponse, IndependentVerificationStatus, PoolStateResponse, RpcQualityEvidence,
    ShadowStateRequest, ShadowStateResponse, VerificationStatus, ARBITRUM_ONE_CHAIN_ID,
    MAX_GATEWAY_RESPONSE_BYTES, SHADOW_STATE_SCHEMA_VERSION,
};
use crate::source_state::{
    expected_uniswap_v3_pool_addresses, hash_json, SourceEvidenceRequest, SourceEvidenceResponse,
    SOURCE_EVIDENCE_RESPONSE_SCHEMA,
};
use crate::transport::{JsonRpcClient, RpcCallResult, TransportError};
use ethabi::{ParamType, Token};
use primitive_types::U256;
use serde::Deserialize;
use serde_json::{json, Value};
use std::collections::{HashMap, HashSet};
use std::sync::Arc;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};
use thiserror::Error;
use tokio::sync::{watch, Mutex, Semaphore};

const ARBITRUM_CHAIN_ID_HEX: &str = "0xa4b1";
const AAVE_EXACT_OPERATION_CONCURRENCY: usize = 12;
const AAVE_FORK_OPERATION_CONCURRENCY: usize = 2;
const AAVE_GET_USER_ACCOUNT_DATA_SELECTOR: &str = "0xbf92857c";
const AAVE_GET_USER_CONFIGURATION_SELECTOR: &str = "0x4417a583";
const AAVE_GET_CONFIGURATION_SELECTOR: &str = "0xc44b11f7";
const AAVE_GET_USER_RESERVE_DATA_SELECTOR: &str = "0x28dd2d01";
const AAVE_GET_RESERVE_TOKENS_SELECTOR: &str = "0xd2493b6c";
const AAVE_ORACLE_GET_ASSET_PRICE_SELECTOR: &str = "0xb3596f07";
const AAVE_GET_RESERVES_LIST_SELECTOR: &str = "0xd1946dbc";
const AAVE_GET_RESERVE_ADDRESS_BY_ID_SELECTOR: &str = "0x52751797";
const AAVE_GET_LIQUIDATION_GRACE_PERIOD_SELECTOR: &str = "0x5c9a8b18";
const AAVE_GET_USER_EMODE_SELECTOR: &str = "0xeddf1b79";
const AAVE_GET_EMODE_COLLATERAL_CONFIG_SELECTOR: &str = "0xb286f467";
const AAVE_GET_EMODE_COLLATERAL_BITMAP_SELECTOR: &str = "0xb0771dba";
const AAVE_FLASHLOAN_PREMIUM_TOTAL_SELECTOR: &str = "0x074b2e43";
const AAVE_BORROW_TOPIC: &str =
    "0xb3d084820fb1a9decffb176436bd02558d15fac9b0ddfed8c465bc7359d7dce0";
const AAVE_REPAY_TOPIC: &str = "0xa534c8dbe71f871f9f3530e97a74601fea17b426cae02e1c5aee42c96c784051";
const AAVE_LIQUIDATION_TOPIC: &str =
    "0xe413a321e8681d831f4dbccbca790d2952b56f977908e45be37335533e005286";
const AAVE_DATA_PROVIDER_ARBITRUM: &str = "0x243aa95cac2a25651eda86e80bee66114413c43b";
const AAVE_ORACLE_ARBITRUM: &str = "0xb56c2f0b653b2e0b10c9b928c8580ac5df02c7c7";
const AAVE_POOL_IMPLEMENTATION_ARBITRUM: &str = "0xf05fd3cc911b4c5e36e53c00354f645e22922c9a";
const EIP1967_IMPLEMENTATION_SLOT: &str =
    "0x360894a13ba1a3210667c828492db98dca3e2076cc3735a920a3ca505d382bbc";
const ARBITRUM_WETH: &str = "0x82af49447d8a07e3bd95bd0d56f35241523fbab1";
const ARBITRUM_NATIVE_USDC: &str = "0xaf88d065e77c8cc2239327c5edb3a432268e5831";
const ARBITRUM_USDC_E: &str = "0xff970a61a04b1ca14834a43f5de4533ebddb5cc8";
const REVIEWED_WETH_USDC_E_POOL_500: &str = "0xc31e54c7a869b9fcbecc14363cf510d1c41fa443";
const ARBITRUM_NODE_INTERFACE: &str = "0x00000000000000000000000000000000000000c8";
const ZERO_ADDRESS: &str = "0x0000000000000000000000000000000000000000";
const UNISWAP_V3_QUOTER_V2_ARBITRUM: &str = "0x61ffe014ba17989e743c5f6cb21bf9697530b21e";
const UNISWAP_V3_FACTORY_ARBITRUM: &str = "0x1f98431c8ad98523631ae4a59f267346ea31f984";
const PHOENIX_EXECUTOR_PACKED_CONFIG_SLOT: &str =
    "0x0000000000000000000000000000000000000000000000000000000000000002";
const PHOENIX_EXECUTOR_MAXIMUM_INPUT_SLOT: &str =
    "0x0000000000000000000000000000000000000000000000000000000000000008";
const REVIEWED_ROUTE_REGISTRY: &str =
    include_str!("../../fixtures/routes/weth_usdc_uniswap_v3.json");
const REVIEWED_POOL_PROOFS: &str =
    include_str!("../../fixtures/routes/arbitrum_uniswap_v3_pool_proofs.json");
const UNISWAP_QUOTE_EXACT_INPUT_SINGLE_SELECTOR: &str = "0xc6a5026a";
const GAS_ESTIMATE_COMPONENTS_SELECTOR: &str = "0xc94e6eeb";
const PERCENTAGE_FACTOR: u64 = 10_000;
const HALF_PERCENTAGE_FACTOR: u64 = 5_000;
const DEFAULT_LIQUIDATION_CLOSE_FACTOR_BPS: u64 = 5_000;
const CLOSE_FACTOR_HF_THRESHOLD_WAD: u128 = 950_000_000_000_000_000;
const MIN_BASE_MAX_CLOSE_FACTOR_THRESHOLD: u64 = 2_000 * 100_000_000;
const MIN_LEFTOVER_BASE: u64 = MIN_BASE_MAX_CLOSE_FACTOR_THRESHOLD / 2;
const FIXED_REVIEWED_SIZE: &str = "fixed_reviewed_size";
const TERMINAL_SIZE_REQUIRED: &str = "terminal_size_required";
const BELOW_MIN_REVIEWED_SIZE: &str = "below_min_reviewed_size";
const DUST_PARTIAL_INVALID: &str = "dust_partial_invalid";
const GAS_ESTIMATE_HEADROOM_BPS: u64 = 12_000;
const SLOT0_SELECTOR: &str = "0x3850c7bd";
const LIQUIDITY_SELECTOR: &str = "0x1a686502";
const TOKEN0_SELECTOR: &str = "0x0dfe1681";
const TOKEN1_SELECTOR: &str = "0xd21220a7";
const FEE_SELECTOR: &str = "0xddca3f43";
const TICK_SPACING_SELECTOR: &str = "0xd0c93a7c";
const FACTORY_SELECTOR: &str = "0xc45a0155";
const DECIMALS_SELECTOR: &str = "0x313ce567";
const MAX_STATE_RESPONSE_DATA_BYTES: usize = 4096;
const MAX_MULTICALL_CODE_BYTES: usize = 1024 * 1024;
const CACHE_CAPACITY: usize = 1024;
const ROUTE_BLOCK_CACHE_TTL: Duration = Duration::from_secs(30);
const STATIC_METADATA_CACHE_TTL: Duration = Duration::from_secs(365 * 24 * 60 * 60);
const HEAD_MAX_AGE: Duration = Duration::from_secs(2);
const MAX_IN_FLIGHT_REQUESTS: usize = 64;
const MAX_STATE_RESOLUTION: Duration = Duration::from_secs(25);
const MAX_COALESCE_WAIT: Duration = Duration::from_secs(26);
const HUNTER_STATE_CACHE_TTL: Duration = Duration::from_secs(5);
const AAVE_SIMULATION_CONTEXT_TTL: Duration = Duration::from_secs(120);
const AAVE_EXACT_STATIC_CONTEXT_TTL: Duration = Duration::from_secs(120);
const MAX_SOURCE_STATE_EVIDENCE_BYTES: usize = 512 * 1024;

type SharedBundleResult = Option<Result<ProviderBundle, GatewayError>>;
type SharedVerificationResult = Option<Result<VerificationEvidence, GatewayError>>;
type SharedHeadResult = Option<Result<HeadSnapshot, GatewayError>>;

type AaveExactProviderResult = (
    AaveExactProviderState,
    String,
    Option<(String, AaveExactStaticContext)>,
);

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct AaveSimulationEvidence {
    realized_profit: U256,
    total_gas: u64,
    l1_gas: u64,
    base_fee_per_gas: U256,
    flash_premium_bps: u16,
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct AaveSimulationContext {
    packed_executor_config: String,
    maximum_input_amount: U256,
    flash_premium_bps: u16,
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct AaveExactStaticContext {
    pool_code_hash: String,
    pool_implementation: String,
    pool_implementation_code_hash: String,
    reserve_ids: Vec<u16>,
    state_root: String,
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct PinnedBlockState {
    block: PinnedBlock,
    state_root: String,
}

#[derive(Clone, Debug, Deserialize)]
struct ReviewedRouteRegistryEntry {
    legs: Vec<ReviewedRouteRegistryLeg>,
    strategy: ReviewedRouteRegistryStrategy,
}

#[derive(Clone, Debug, Deserialize)]
struct ReviewedRouteRegistryLeg {
    state_target: String,
    protocol: String,
    fee: u32,
    token_in: String,
    token_out: String,
    direction: String,
}

#[derive(Clone, Debug, Deserialize)]
struct ReviewedRouteRegistryStrategy {
    candidate_sizes: Vec<String>,
}

#[derive(Clone, Debug, Deserialize)]
struct ReviewedPoolProofs {
    schema_version: String,
    chain_id: u64,
    protocol: String,
    factory: String,
    pools: Vec<ReviewedPoolProof>,
}

#[derive(Clone, Debug, Deserialize)]
struct ReviewedPoolProof {
    token0: String,
    token1: String,
    fee: u32,
    pool_address: String,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ReviewedAaveUnwindRoute {
    pub pool: String,
    pub factory: String,
    pub token0: String,
    pub token1: String,
    pub fee: u32,
    pub zero_for_one: bool,
}

pub fn reviewed_aave_unwind_routes() -> Result<Vec<ReviewedAaveUnwindRoute>, GatewayError> {
    let registry: Vec<ReviewedRouteRegistryEntry> = serde_json::from_str(REVIEWED_ROUTE_REGISTRY)
        .map_err(|_| GatewayError::ProviderIntegrity)?;
    let proofs: ReviewedPoolProofs =
        serde_json::from_str(REVIEWED_POOL_PROOFS).map_err(|_| GatewayError::ProviderIntegrity)?;
    let reviewed_sizes = SizeLevel::ALL
        .iter()
        .map(|level| level.amount_wei().to_string())
        .collect::<Vec<_>>();
    if proofs.schema_version != "phoenix.route.pool-proofs.v1"
        || proofs.chain_id != ARBITRUM_ONE_CHAIN_ID
        || proofs.protocol != "UniswapV3"
        || proofs.factory != UNISWAP_V3_FACTORY_ARBITRUM
        || registry.is_empty()
        || registry
            .iter()
            .any(|route| route.strategy.candidate_sizes != reviewed_sizes)
    {
        return Err(GatewayError::ProviderIntegrity);
    }
    let reviewed_pools = registry
        .into_iter()
        .flat_map(|route| route.legs)
        .filter(|leg| {
            leg.protocol == "UniswapV3"
                && leg.token_in == ARBITRUM_NATIVE_USDC
                && leg.token_out == ARBITRUM_WETH
                && matches!(leg.fee, 100 | 500 | 3_000)
        })
        .map(|leg| (leg.state_target, leg.fee, leg.direction))
        .collect::<HashSet<_>>();
    let mut routes = proofs
        .pools
        .into_iter()
        .filter(|proof| {
            if proof.pool_address == REVIEWED_WETH_USDC_E_POOL_500 {
                return proof.token0 == ARBITRUM_WETH
                    && proof.token1 == ARBITRUM_USDC_E
                    && proof.fee == 500;
            }
            let Some(zero_for_one) =
                uniswap_zero_for_one(ARBITRUM_NATIVE_USDC, &proof.token0, &proof.token1)
            else {
                return false;
            };
            let direction = if zero_for_one {
                "zero_for_one"
            } else {
                "one_for_zero"
            };
            reviewed_pools.contains(&(proof.pool_address.clone(), proof.fee, direction.to_string()))
        })
        .map(|proof| ReviewedAaveUnwindRoute {
            zero_for_one: proof.token0 == ARBITRUM_NATIVE_USDC
                || proof.pool_address == REVIEWED_WETH_USDC_E_POOL_500,
            pool: proof.pool_address,
            factory: proofs.factory.clone(),
            token0: proof.token0,
            token1: proof.token1,
            fee: proof.fee,
        })
        .collect::<Vec<_>>();
    routes.sort_by_key(|route| route.fee);
    if routes.len() != 4
        || routes.len() > 4
        || routes.iter().any(|route| {
            !matches!(route.fee, 100 | 500 | 3_000)
                || route.pool.len() != 42
                || !route.pool.starts_with("0x")
        })
        || routes.windows(2).any(|pair| pair[0].pool == pair[1].pool)
    {
        return Err(GatewayError::ProviderIntegrity);
    }
    Ok(routes)
}

fn uniswap_zero_for_one(token_in: &str, token0: &str, token1: &str) -> Option<bool> {
    if token_in == token0 && token0 != token1 {
        Some(true)
    } else if token_in == token1 && token0 != token1 {
        Some(false)
    } else {
        None
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct GatewayLimits {
    pub state_requests_per_minute: u32,
    pub upstream_calls_per_second: u32,
    pub upstream_call_burst: u32,
}

impl Default for GatewayLimits {
    fn default() -> Self {
        Self {
            state_requests_per_minute: 12,
            upstream_calls_per_second: 1,
            upstream_call_burst: 4,
        }
    }
}

#[derive(Clone, Copy, Debug, Error, PartialEq, Eq)]
pub enum GatewayError {
    #[error("RPC Gateway request contract is invalid")]
    InvalidRequest,
    #[error("RPC Gateway state request budget is exhausted")]
    RequestBudgetExhausted,
    #[error("RPC Gateway upstream call budget is exhausted")]
    UpstreamBudgetExhausted,
    #[error("RPC Gateway has no eligible provider")]
    ProviderUnavailable,
    #[error("RPC Gateway provider evidence failed integrity validation")]
    ProviderIntegrity,
    #[error("RPC Gateway providers disagree on canonical Hunter state")]
    ProviderDisagreement,
    #[error("RPC Gateway Hunter state coverage is incomplete")]
    StateIncomplete,
    #[error("RPC Gateway response exceeded the configured bound")]
    ResponseOversized,
    #[error("simulated EVM execution reverted")]
    ExecutionReverted { reason: Option<[u8; 64]> },
}

impl GatewayError {
    pub const fn class(self) -> &'static str {
        match self {
            Self::InvalidRequest => "invalid_request",
            Self::RequestBudgetExhausted => "state_request_budget_exhausted",
            Self::UpstreamBudgetExhausted => "upstream_call_budget_exhausted",
            Self::ProviderUnavailable => "provider_unavailable",
            Self::ProviderIntegrity => "provider_integrity_failure",
            Self::ProviderDisagreement => "provider_disagreement",
            Self::StateIncomplete => "state_incomplete",
            Self::ResponseOversized => "gateway_response_oversized",
            Self::ExecutionReverted { .. } => "execution_reverted",
        }
    }

    pub const fn retryable(self) -> bool {
        matches!(
            self,
            Self::RequestBudgetExhausted
                | Self::UpstreamBudgetExhausted
                | Self::ProviderUnavailable
        )
    }

    pub const fn http_status(self) -> u16 {
        match self {
            Self::InvalidRequest => 400,
            Self::RequestBudgetExhausted | Self::UpstreamBudgetExhausted => 429,
            Self::ProviderUnavailable => 503,
            Self::ProviderIntegrity
            | Self::ProviderDisagreement
            | Self::StateIncomplete
            | Self::ResponseOversized
            | Self::ExecutionReverted { .. } => 502,
        }
    }

    pub fn response(self) -> GatewayErrorResponse {
        GatewayErrorResponse {
            schema_version: SHADOW_STATE_SCHEMA_VERSION.to_string(),
            error_class: self.class().to_string(),
            retryable: self.retryable(),
        }
    }
}

struct ExactOperationMetricGuard(RuntimeRpcMetrics);

impl Drop for ExactOperationMetricGuard {
    fn drop(&mut self) {
        self.0.exact_operation_finished();
    }
}

struct ForkOperationMetricGuard(RuntimeRpcMetrics);

impl Drop for ForkOperationMetricGuard {
    fn drop(&mut self) {
        self.0.fork_operation_finished();
    }
}

#[derive(Clone)]
pub struct GatewayRuntime {
    providers: Arc<Mutex<ProviderPool>>,
    request_budget: Arc<Mutex<GlobalBudget>>,
    upstream_budget: Arc<Mutex<GlobalBudget>>,
    static_cache: Arc<Mutex<TtlCache<()>>>,
    route_cache: Arc<Mutex<TtlCache<ProviderBundle>>>,
    verification_cache: Arc<Mutex<TtlCache<VerificationEvidence>>>,
    hunter_state_cache: Arc<Mutex<TtlCache<Vec<ProviderStateAgreement>>>>,
    aave_simulation_context_cache: Arc<Mutex<TtlCache<AaveSimulationContext>>>,
    aave_exact_static_context_cache: Arc<Mutex<TtlCache<AaveExactStaticContext>>>,
    primary_in_flight: Arc<Mutex<HashMap<String, watch::Receiver<SharedBundleResult>>>>,
    verification_in_flight: Arc<Mutex<HashMap<String, watch::Receiver<SharedVerificationResult>>>>,
    head: Arc<Mutex<Option<HeadSnapshot>>>,
    head_in_flight: Arc<Mutex<Option<watch::Receiver<SharedHeadResult>>>>,
    chain_verified: Arc<Mutex<HashSet<String>>>,
    multicall_verified: Arc<Mutex<HashSet<String>>>,
    provider_verification_locks: Arc<Mutex<HashMap<String, Arc<Mutex<()>>>>>,
    upstream_operation_lock: Arc<Mutex<()>>,
    aave_exact_operation_permits: Arc<Semaphore>,
    aave_fork_operation_permits: Arc<Semaphore>,
    client: Arc<dyn JsonRpcClient>,
    timeouts: MethodTimeouts,
    metrics: RuntimeRpcMetrics,
    readiness: GatewayReadiness,
    upstream_refill_interval: Duration,
}

impl std::fmt::Debug for GatewayRuntime {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("GatewayRuntime")
            .field("timeouts", &self.timeouts)
            .finish_non_exhaustive()
    }
}

impl GatewayRuntime {
    pub fn new(
        config: ProviderConfig,
        client: Arc<dyn JsonRpcClient>,
        timeouts: MethodTimeouts,
        metrics: RuntimeRpcMetrics,
        readiness: GatewayReadiness,
    ) -> Self {
        Self::with_limits(
            config,
            client,
            timeouts,
            metrics,
            readiness,
            GatewayLimits::default(),
        )
    }

    pub fn with_limits(
        config: ProviderConfig,
        client: Arc<dyn JsonRpcClient>,
        timeouts: MethodTimeouts,
        metrics: RuntimeRpcMetrics,
        readiness: GatewayReadiness,
        limits: GatewayLimits,
    ) -> Self {
        let now = Instant::now();
        Self {
            providers: Arc::new(Mutex::new(config.into_pool(now))),
            request_budget: Arc::new(Mutex::new(GlobalBudget::new(
                limits.state_requests_per_minute,
                limits.state_requests_per_minute,
                Duration::from_secs(60),
                now,
            ))),
            upstream_budget: Arc::new(Mutex::new(GlobalBudget::new(
                limits.upstream_call_burst,
                limits.upstream_calls_per_second,
                Duration::from_secs(1),
                now,
            ))),
            static_cache: Arc::new(Mutex::new(TtlCache::new(CACHE_CAPACITY))),
            route_cache: Arc::new(Mutex::new(TtlCache::new(CACHE_CAPACITY))),
            verification_cache: Arc::new(Mutex::new(TtlCache::new(CACHE_CAPACITY))),
            hunter_state_cache: Arc::new(Mutex::new(TtlCache::new(CACHE_CAPACITY))),
            aave_simulation_context_cache: Arc::new(Mutex::new(TtlCache::new(CACHE_CAPACITY))),
            aave_exact_static_context_cache: Arc::new(Mutex::new(TtlCache::new(CACHE_CAPACITY))),
            primary_in_flight: Arc::new(Mutex::new(HashMap::new())),
            verification_in_flight: Arc::new(Mutex::new(HashMap::new())),
            head: Arc::new(Mutex::new(None)),
            head_in_flight: Arc::new(Mutex::new(None)),
            chain_verified: Arc::new(Mutex::new(HashSet::new())),
            multicall_verified: Arc::new(Mutex::new(HashSet::new())),
            provider_verification_locks: Arc::new(Mutex::new(HashMap::new())),
            upstream_operation_lock: Arc::new(Mutex::new(())),
            aave_exact_operation_permits: Arc::new(Semaphore::new(
                AAVE_EXACT_OPERATION_CONCURRENCY,
            )),
            aave_fork_operation_permits: Arc::new(Semaphore::new(AAVE_FORK_OPERATION_CONCURRENCY)),
            client,
            timeouts,
            metrics,
            readiness,
            upstream_refill_interval: Duration::from_secs_f64(
                1.0 / f64::from(limits.upstream_calls_per_second.max(1)),
            ),
        }
    }

    pub fn metrics(&self) -> RuntimeRpcMetrics {
        self.metrics.clone()
    }

    pub fn readiness(&self) -> GatewayReadiness {
        self.readiness.clone()
    }

    pub async fn probe(&self) -> Result<(), GatewayError> {
        self.refresh_head_shared(true).await.map(|_| ())
    }

    pub async fn resolve_source_evidence(
        &self,
        request: SourceEvidenceRequest,
    ) -> Result<SourceEvidenceResponse, GatewayError> {
        request
            .validate()
            .map_err(|_| GatewayError::InvalidRequest)?;
        if !self.request_budget.lock().await.admit(Instant::now()) {
            self.metrics.state_request_budget_rejected();
            return Err(GatewayError::RequestBudgetExhausted);
        }
        let _operation_guard = self.upstream_operation_lock.lock().await;
        let resolved = self.resolve_source_inclusion(&request).await?;
        let provider = resolved.provider;
        let inclusion = resolved.inclusion;
        let block_number = inclusion.block_number;
        let block_hash = inclusion.block_hash;
        let transaction_index = inclusion.transaction_index;
        let parent_block_number = inclusion.parent_block_number;
        let parent_block_hash = inclusion.parent_block_hash;
        let status = inclusion.transaction_status;
        let source_event_index = inclusion.source_event_index;
        let source_pool_addresses = inclusion.source_pool_addresses;
        let provider_response_hash = resolved.provider_response_hash;

        let (method, post_state_hash, completeness, failure_reason, state_evidence) =
            if request.state_reconstruction_required {
                let prestate_trace = self
                    .upstream_call(
                        &provider,
                        RpcMethod::DebugTraceTransaction,
                        json!([
                            request.source_transaction_hash,
                            {
                                "tracer": "prestateTracer",
                                "tracerConfig": {"disableCode": true}
                            }
                        ]),
                        ProviderSlot::Primary,
                        None,
                        false,
                    )
                    .await;
                let diff_trace = self
                    .upstream_call(
                        &provider,
                        RpcMethod::DebugTraceTransaction,
                        json!([
                            request.source_transaction_hash,
                            {
                                "tracer": "prestateTracer",
                                "tracerConfig": {"diffMode": true, "disableCode": true}
                            }
                        ]),
                        ProviderSlot::Primary,
                        None,
                        false,
                    )
                    .await;
                source_trace_evidence(
                    prestate_trace,
                    diff_trace,
                    &source_pool_addresses,
                    SourceTraceContext {
                        request: &request,
                        block_number,
                        block_hash: &block_hash,
                        transaction_index,
                        parent_block_number,
                        parent_block_hash: &parent_block_hash,
                    },
                )?
            } else {
                (
                    "unavailable".to_string(),
                    None,
                    "incomplete".to_string(),
                    Some("state_reconstruction_not_selected".to_string()),
                    json!({
                        "schema_version": "phoenix.transaction-boundary-state.v1",
                        "complete": false,
                        "failure_reason": "state_reconstruction_not_selected",
                        "source_transaction_hash": request.source_transaction_hash,
                        "source_block_number": block_number,
                        "source_block_hash": block_hash,
                        "source_transaction_index": transaction_index,
                        "source_feed_sequence": request.source_feed_sequence,
                        "source_feed_order_position": request.source_feed_order_position,
                        "source_command_index": request.source_command_index,
                        "parent_block_number": parent_block_number,
                        "parent_block_hash": parent_block_hash,
                        "source_factory": request.source_factory,
                        "source_pool_path": request.source_pool_path,
                        "source_token_path": request.source_token_path,
                        "source_encoded_token_path": request.source_encoded_token_path,
                        "source_fee_path": request.source_fee_path,
                        "source_pool_addresses": source_pool_addresses
                    }),
                )
            };

        let mut response = SourceEvidenceResponse {
            schema_version: SOURCE_EVIDENCE_RESPONSE_SCHEMA.to_string(),
            source_event_identity: request.source_event_identity.clone(),
            source_identity_hash: request.source_identity_hash.clone(),
            source_chain_id: 42161,
            source_transaction_hash: request.source_transaction_hash.clone(),
            source_feed_sequence: request.source_feed_sequence,
            source_feed_order_position: request.source_feed_order_position,
            source_command_index: request.source_command_index,
            source_pool_path: request.source_pool_path.clone(),
            source_token_path: request.source_token_path.clone(),
            source_encoded_token_path: request.source_encoded_token_path.clone(),
            source_fee_path: request.source_fee_path.clone(),
            source_block_number: block_number,
            source_block_hash: block_hash,
            source_transaction_index: transaction_index,
            source_event_index,
            source_pool_addresses,
            transaction_status: status.to_string(),
            parent_block_number,
            parent_block_hash,
            provider_id: provider.provider_id().to_string(),
            provider_response_hash,
            enrichment_hash: "0".repeat(64),
            reconstruction_method: method,
            post_initiating_state_hash: post_state_hash,
            completeness_status: completeness,
            failure_reason,
            state_evidence,
            evidence_hash: "0".repeat(64),
        };
        response.enrichment_hash = response
            .canonical_enrichment_hash()
            .map_err(|_| GatewayError::ProviderIntegrity)?;
        response.evidence_hash = response
            .canonical_evidence_hash()
            .map_err(|_| GatewayError::ProviderIntegrity)?;
        response
            .validate(&request)
            .map_err(|_| GatewayError::ProviderIntegrity)?;
        Ok(response)
    }

    async fn resolve_source_inclusion(
        &self,
        request: &SourceEvidenceRequest,
    ) -> Result<ResolvedSourceInclusion, GatewayError> {
        let provider_count = self.provider_count().await;
        let mut excluded = HashSet::with_capacity(provider_count);
        let mut integrity_failure = false;
        for _ in 0..provider_count {
            let Some(provider) = self.reserve_provider(&excluded).await else {
                break;
            };
            excluded.insert(provider.provider_id().to_string());
            let required_calls = self.provider_setup_call_count(provider.provider_id()).await
                + if request.state_reconstruction_required {
                    5
                } else {
                    3
                };
            if !self.admit_upstream_sequence(required_calls).await {
                return Err(GatewayError::UpstreamBudgetExhausted);
            }
            if let Err(failure) = self.ensure_provider_verified(&provider).await {
                if failure == CallFailure::Budget {
                    return Err(GatewayError::UpstreamBudgetExhausted);
                }
                integrity_failure |= failure == CallFailure::Integrity;
                self.apply_provider_failure(provider.provider_id(), failure)
                    .await;
                continue;
            }
            match self
                .source_inclusion_from_provider(&provider, request)
                .await
            {
                Ok(inclusion) => {
                    self.mark_provider_success(provider.provider_id()).await;
                    return Ok(inclusion);
                }
                Err(CallFailure::Budget) => {
                    return Err(GatewayError::UpstreamBudgetExhausted);
                }
                Err(failure) => {
                    integrity_failure |= failure == CallFailure::Integrity;
                    self.apply_provider_failure(provider.provider_id(), failure)
                        .await;
                }
            }
        }
        if integrity_failure {
            Err(GatewayError::ProviderIntegrity)
        } else {
            Err(GatewayError::ProviderUnavailable)
        }
    }

    async fn source_inclusion_from_provider(
        &self,
        provider: &ProviderLease,
        request: &SourceEvidenceRequest,
    ) -> Result<ResolvedSourceInclusion, CallFailure> {
        let transaction = self
            .upstream_call(
                provider,
                RpcMethod::EthGetTransactionByHash,
                json!([request.source_transaction_hash]),
                ProviderSlot::Primary,
                None,
                false,
            )
            .await?
            .value;
        let receipt = self
            .upstream_call(
                provider,
                RpcMethod::EthGetTransactionReceipt,
                json!([request.source_transaction_hash]),
                ProviderSlot::Primary,
                None,
                false,
            )
            .await?
            .value;
        let block_number =
            required_quantity(&transaction, "blockNumber").map_err(|_| CallFailure::Integrity)?;
        let block = self
            .upstream_call(
                provider,
                RpcMethod::EthGetBlockByNumber,
                json!([format_quantity(block_number), false]),
                ProviderSlot::Primary,
                None,
                false,
            )
            .await?
            .value;
        let inclusion = verify_source_inclusion(request, &transaction, &receipt, &block)
            .map_err(|_| CallFailure::Integrity)?;
        let provider_response_hash = hash_json(&json!({
            "transaction": transaction,
            "receipt": receipt,
            "block": block
        }))
        .map_err(|_| CallFailure::Integrity)?;
        Ok(ResolvedSourceInclusion {
            provider: provider.clone(),
            inclusion,
            provider_response_hash,
        })
    }

    pub async fn resolve_shadow_state(
        &self,
        request: ShadowStateRequest,
    ) -> Result<ShadowStateResponse, GatewayError> {
        let started = Instant::now();
        let result = self.resolve_shadow_state_inner(request).await;
        self.metrics.state_request_latency(started.elapsed());
        if matches!(result.as_ref(), Err(GatewayError::ProviderUnavailable)) {
            self.metrics.provider_unavailable();
        }
        result
    }

    pub async fn resolve_hunter_state(
        &self,
        request: HunterStateRequest,
    ) -> Result<HunterStateResponse, GatewayError> {
        request
            .validate()
            .map_err(|_| GatewayError::InvalidRequest)?;
        if !self.request_budget.lock().await.admit(Instant::now()) {
            return Err(GatewayError::RequestBudgetExhausted);
        }
        let head = self.current_head().await?;
        let request_bytes =
            serde_json::to_vec(&request).map_err(|_| GatewayError::InvalidRequest)?;
        let request_hash = canonical_hash_bytes(&request_bytes);
        let cache_key = format!("{request_hash}:{}:{}", head.block.number, head.block.hash);
        if let Some(agreements) = self
            .hunter_state_cache
            .lock()
            .await
            .get(&cache_key, Instant::now())
        {
            let response = HunterStateResponse {
                schema_version: HUNTER_STATE_RESPONSE_SCHEMA.to_string(),
                chain_id: ARBITRUM_ONE_CHAIN_ID,
                request_id: request.request_id.clone(),
                block_number: head.block.number,
                block_hash: head.block.hash,
                agreements,
                resolved_at_unix_ms: unix_time_ms(),
            };
            response
                .validate(&request)
                .map_err(map_hunter_contract_error)?;
            return Ok(response);
        }

        let _operation_guard = self.upstream_operation_lock.lock().await;
        let primary = self
            .reserve_named_provider(&head.provider_id)
            .await
            .ok_or(GatewayError::ProviderUnavailable)?;
        self.ensure_provider_verified(&primary)
            .await
            .map_err(map_call_failure)?;
        let excluded = HashSet::from([primary.provider_id().to_string()]);
        let secondary = self
            .reserve_provider(&excluded)
            .await
            .ok_or(GatewayError::ProviderUnavailable)?;
        self.ensure_provider_verified(&secondary)
            .await
            .map_err(map_call_failure)?;

        let primary_states = self
            .perform_hunter_state_bundle(&primary, &request, &head.block, ProviderSlot::Primary)
            .await?;
        let secondary_states = self
            .perform_hunter_state_bundle(&secondary, &request, &head.block, ProviderSlot::Secondary)
            .await?;
        self.mark_provider_success(primary.provider_id()).await;

        let agreements = primary_states
            .into_iter()
            .zip(secondary_states)
            .map(|(primary_state, secondary_state)| {
                let agreement = ProviderStateAgreement {
                    primary_provider_id: primary.provider_id().to_string(),
                    secondary_provider_id: secondary.provider_id().to_string(),
                    primary: primary_state,
                    secondary: secondary_state,
                };
                agreement.agreed().map_err(map_hunter_contract_error)?;
                Ok(agreement)
            })
            .collect::<Result<Vec<_>, _>>()?;
        self.hunter_state_cache.lock().await.insert(
            cache_key,
            agreements.clone(),
            HUNTER_STATE_CACHE_TTL,
            Instant::now(),
        );
        let response = HunterStateResponse {
            schema_version: HUNTER_STATE_RESPONSE_SCHEMA.to_string(),
            chain_id: ARBITRUM_ONE_CHAIN_ID,
            request_id: request.request_id.clone(),
            block_number: head.block.number,
            block_hash: head.block.hash,
            agreements,
            resolved_at_unix_ms: unix_time_ms(),
        };
        response
            .validate(&request)
            .map_err(map_hunter_contract_error)?;
        Ok(response)
    }

    pub async fn resolve_aave_screen(
        &self,
        request: AaveScreenRequest,
    ) -> Result<AaveScreenResponse, GatewayError> {
        request
            .validate()
            .map_err(|_| GatewayError::InvalidRequest)?;
        if !self.request_budget.lock().await.admit(Instant::now()) {
            return Err(GatewayError::RequestBudgetExhausted);
        }
        let _operation_guard = self.upstream_operation_lock.lock().await;
        let primary = self
            .reserve_named_provider(AAVE_PRIMARY_PROVIDER_ID)
            .await
            .ok_or(GatewayError::ProviderUnavailable)?;
        let required_calls = self.provider_setup_call_count(primary.provider_id()).await + 2;
        if !self.admit_upstream_sequence(required_calls).await {
            return Err(GatewayError::UpstreamBudgetExhausted);
        }
        self.ensure_provider_verified(&primary)
            .await
            .map_err(map_call_failure)?;
        let block = self
            .provider_block(&primary, "finalized", ProviderSlot::Primary)
            .await?;
        let (primary_accounts, primary_weth_price) = self
            .perform_aave_screen(&primary, &request, &block, ProviderSlot::Primary)
            .await?;
        self.mark_provider_success(primary.provider_id()).await;
        let response = AaveScreenResponse {
            schema_version: AAVE_SCREEN_RESPONSE_SCHEMA.to_string(),
            chain_id: ARBITRUM_ONE_CHAIN_ID,
            request_id: request.request_id.clone(),
            block_number: block.number,
            block_hash: block.hash,
            primary: AaveProviderScreen {
                provider_id: primary.provider_id().to_string(),
                weth_price_base: primary_weth_price,
                accounts: primary_accounts,
            },
            confirmation: None,
            quorum: 1,
            resolved_at_unix_ms: unix_time_ms(),
        };
        response
            .validate(&request)
            .map_err(|_| GatewayError::ProviderDisagreement)?;
        Ok(response)
    }

    /// Returns bounded discovery data from the highest-priority healthy
    /// provider only.  This endpoint has no execution authority: callers must
    /// obtain a fresh single-primary Aave Exact result before a candidate can be
    /// persisted or submitted.
    pub async fn resolve_aave_primary_screen(
        &self,
        request: AaveScreenRequest,
    ) -> Result<AavePrimaryScreenResponse, GatewayError> {
        request
            .validate()
            .map_err(|_| GatewayError::InvalidRequest)?;
        if !self.request_budget.lock().await.admit(Instant::now()) {
            return Err(GatewayError::RequestBudgetExhausted);
        }
        let _operation_guard = self.upstream_operation_lock.lock().await;
        let primary = self
            .reserve_named_provider(AAVE_PRIMARY_PROVIDER_ID)
            .await
            .ok_or(GatewayError::ProviderUnavailable)?;
        let required_calls = self.provider_setup_call_count(primary.provider_id()).await + 2;
        if !self.admit_upstream_sequence(required_calls).await {
            return Err(GatewayError::UpstreamBudgetExhausted);
        }
        self.ensure_provider_verified(&primary)
            .await
            .map_err(map_call_failure)?;
        let block = self
            .provider_block(&primary, "finalized", ProviderSlot::Primary)
            .await?;
        let (accounts, weth_price_base) = self
            .perform_aave_screen(&primary, &request, &block, ProviderSlot::Primary)
            .await?;
        self.mark_provider_success(primary.provider_id()).await;
        let response = AavePrimaryScreenResponse {
            schema_version: AAVE_PRIMARY_SCREEN_RESPONSE_SCHEMA.to_string(),
            chain_id: ARBITRUM_ONE_CHAIN_ID,
            request_id: request.request_id.clone(),
            block_number: block.number,
            block_hash: block.hash,
            primary: AaveProviderScreen {
                provider_id: primary.provider_id().to_string(),
                weth_price_base,
                accounts,
            },
            resolved_at_unix_ms: unix_time_ms(),
        };
        response
            .validate(&request)
            .map_err(|_| GatewayError::InvalidRequest)?;
        Ok(response)
    }

    pub async fn resolve_aave_exact(
        &self,
        request: AaveExactRequest,
    ) -> Result<AaveExactResponse, GatewayError> {
        request
            .validate()
            .map_err(|_| GatewayError::InvalidRequest)?;
        if !self.request_budget.lock().await.admit(Instant::now()) {
            return Err(GatewayError::RequestBudgetExhausted);
        }
        let queue_started = Instant::now();
        let _operation_permit = self
            .aave_exact_operation_permits
            .acquire()
            .await
            .map_err(|_| GatewayError::ProviderUnavailable)?;
        self.metrics
            .exact_operation_started(queue_started.elapsed());
        let _metric_guard = ExactOperationMetricGuard(self.metrics.clone());
        let pacing_deadline = Instant::now() + MAX_STATE_RESOLUTION;
        let primary = self
            .reserve_named_provider(AAVE_PRIMARY_PROVIDER_ID)
            .await
            .ok_or(GatewayError::ProviderUnavailable)?;
        self.ensure_provider_verified_paced(&primary, pacing_deadline)
            .await
            .map_err(map_call_failure)?;
        let finalized = self
            .provider_block_state_paced(
                &primary,
                "finalized",
                ProviderSlot::Primary,
                pacing_deadline,
            )
            .await?;
        let (primary_state, primary_root, primary_pending_context) = self
            .perform_aave_exact(
                &primary,
                &request,
                &finalized.block,
                &finalized.state_root,
                ProviderSlot::Primary,
                pacing_deadline,
            )
            .await?;
        self.mark_provider_success(primary.provider_id()).await;
        let response = AaveExactResponse {
            schema_version: AAVE_EXACT_RESPONSE_SCHEMA.to_string(),
            chain_id: ARBITRUM_ONE_CHAIN_ID,
            request_id: request.request_id.clone(),
            block_number: finalized.block.number,
            block_hash: finalized.block.hash,
            state_root: primary_root,
            primary: primary_state,
            confirmation: None,
            quorum: 1,
            resolved_at_unix_ms: unix_time_ms(),
        };
        response
            .validate(&request)
            .map_err(|_| GatewayError::ProviderDisagreement)?;
        let now = Instant::now();
        if let Some((key, context)) = primary_pending_context {
            self.aave_exact_static_context_cache.lock().await.insert(
                key,
                context,
                AAVE_EXACT_STATIC_CONTEXT_TTL,
                now,
            );
        }
        Ok(response)
    }

    pub async fn resolve_aave_tail(
        &self,
        request: AaveTailRequest,
    ) -> Result<AaveTailResponse, GatewayError> {
        request
            .validate()
            .map_err(|_| GatewayError::InvalidRequest)?;
        if !self.request_budget.lock().await.admit(Instant::now()) {
            return Err(GatewayError::RequestBudgetExhausted);
        }
        let _operation_guard = self.upstream_operation_lock.lock().await;
        let primary = self
            .reserve_named_provider(AAVE_PRIMARY_PROVIDER_ID)
            .await
            .ok_or(GatewayError::ProviderUnavailable)?;
        self.ensure_provider_verified(&primary)
            .await
            .map_err(map_call_failure)?;
        let finalized = self
            .provider_block(&primary, "finalized", ProviderSlot::Primary)
            .await?;
        let (from_block, to_block, borrowers) = if request.from_block == 0
            || request.from_block == finalized.number.saturating_add(1)
        {
            (
                finalized.number.saturating_add(1),
                finalized.number,
                Vec::new(),
            )
        } else {
            if request.from_block > finalized.number {
                return Err(GatewayError::InvalidRequest);
            }
            let to_block = request.from_block.saturating_add(255).min(finalized.number);
            let primary_logs = self
                .aave_tail_logs(
                    &primary,
                    request.from_block,
                    to_block,
                    ProviderSlot::Primary,
                )
                .await?;
            let mut borrowers = primary_logs
                .into_iter()
                .map(|event| event.borrower)
                .collect::<Vec<_>>();
            borrowers.sort();
            borrowers.dedup();
            (request.from_block, to_block, borrowers)
        };
        self.mark_provider_success(primary.provider_id()).await;
        let response = AaveTailResponse {
            schema_version: AAVE_TAIL_RESPONSE_SCHEMA.to_string(),
            chain_id: ARBITRUM_ONE_CHAIN_ID,
            request_id: request.request_id.clone(),
            finalized_block_number: finalized.number,
            finalized_block_hash: finalized.hash,
            from_block,
            to_block,
            next_block: to_block.saturating_add(1),
            primary_provider_id: primary.provider_id().to_string(),
            confirmation_provider_id: None,
            quorum: 1,
            borrowers,
            resolved_at_unix_ms: unix_time_ms(),
        };
        response
            .validate(&request)
            .map_err(|_| GatewayError::ProviderDisagreement)?;
        Ok(response)
    }

    pub async fn simulate_aave_liquidation(
        &self,
        request: AaveSimulateRequest,
    ) -> Result<AaveSimulateResponse, GatewayError> {
        request
            .validate()
            .map_err(|_| GatewayError::InvalidRequest)?;
        validate_aave_simulation_identity(&request)?;
        if !self.request_budget.lock().await.admit(Instant::now()) {
            return Err(GatewayError::RequestBudgetExhausted);
        }
        let _operation_guard = self.upstream_operation_lock.lock().await;
        let expected_block = PinnedBlock {
            number: request.block_number,
            hash: request.block_hash.clone(),
        };
        let primary = self
            .reserve_named_provider(AAVE_PRIMARY_PROVIDER_ID)
            .await
            .ok_or(GatewayError::ProviderUnavailable)?;
        self.ensure_provider_verified(&primary)
            .await
            .map_err(map_call_failure)?;
        let route_id = aave_simulation_route_id(&request)?;
        let calldata = encode_aave_liquidation_call(&request, &route_id)?;
        let primary_evidence = self
            .perform_aave_simulation(
                &primary,
                &request,
                &expected_block,
                &calldata,
                ProviderSlot::Primary,
            )
            .await?;
        let response = build_aave_simulation_response(
            &request,
            &route_id,
            &calldata,
            primary.provider_id(),
            primary_evidence,
        )?;
        self.mark_provider_success(primary.provider_id()).await;
        Ok(response)
    }

    pub async fn simulate_aave_liquidations_batch(
        &self,
        request: AaveSimulateBatchRequest,
    ) -> Result<AaveSimulateBatchResponse, GatewayError> {
        request
            .validate()
            .map_err(|_| GatewayError::InvalidRequest)?;
        for simulation in &request.simulations {
            validate_aave_simulation_identity(simulation)?;
        }
        if !self.request_budget.lock().await.admit(Instant::now()) {
            self.metrics.state_request_budget_rejected();
            return Err(GatewayError::RequestBudgetExhausted);
        }
        let queue_started = Instant::now();
        let _operation_permit = self
            .aave_fork_operation_permits
            .acquire()
            .await
            .map_err(|_| GatewayError::ProviderUnavailable)?;
        self.metrics.fork_operation_started(queue_started.elapsed());
        let _metric_guard = ForkOperationMetricGuard(self.metrics.clone());
        let first = &request.simulations[0];
        let remaining = first
            .deadline_unix_seconds
            .saturating_sub(unix_time_seconds());
        if remaining == 0 {
            return Err(GatewayError::InvalidRequest);
        }
        let pacing_deadline = Instant::now() + Duration::from_secs(remaining);
        let expected_block = PinnedBlock {
            number: first.block_number,
            hash: first.block_hash.clone(),
        };
        let primary = self
            .reserve_named_provider(AAVE_PRIMARY_PROVIDER_ID)
            .await
            .ok_or(GatewayError::ProviderUnavailable)?;
        self.ensure_provider_verified_paced(&primary, pacing_deadline)
            .await
            .map_err(map_call_failure)?;
        self.verify_aave_simulation_pin(
            &primary,
            first,
            &expected_block,
            ProviderSlot::Primary,
            pacing_deadline,
        )
        .await?;
        let (primary_context, primary_pending_key) = self
            .aave_simulation_context(
                &primary,
                first,
                &expected_block,
                ProviderSlot::Primary,
                pacing_deadline,
            )
            .await?;
        let mut results = Vec::with_capacity(request.simulations.len());
        for simulation in &request.simulations {
            let result = self
                .simulate_aave_batch_item(
                    &primary,
                    simulation,
                    &expected_block,
                    &primary_context,
                    pacing_deadline,
                )
                .await;
            results.push(match result {
                Ok(response) => AaveSimulateBatchResult {
                    request_id: simulation.request_id.clone(),
                    response: Some(response),
                    error: None,
                },
                Err(error) => {
                    let error_class = error.class().to_string();
                    let retryable = error.retryable();
                    let mut revert_reason = match error {
                        GatewayError::ExecutionReverted { reason } => reason,
                        _ => None,
                    };
                    if revert_reason.is_none()
                        && matches!(error, GatewayError::ExecutionReverted { .. })
                    {
                        if let Some(paused_under_override) = self
                            .aave_pause_state_under_override(
                                &primary,
                                simulation,
                                &expected_block,
                                &primary_context,
                                pacing_deadline,
                            )
                            .await
                        {
                            let mut reason = [0_u8; 64];
                            let marker: &[u8] = if paused_under_override {
                                b"paused_gate_override_ignored"
                            } else {
                                b"economics_or_guard_revert"
                            };
                            reason[..marker.len()].copy_from_slice(marker);
                            revert_reason = Some(reason);
                        }
                    }
                    AaveSimulateBatchResult {
                        request_id: simulation.request_id.clone(),
                        response: None,
                        error: Some(AaveSimulateBatchError {
                            error_class,
                            retryable,
                            revert_reason: revert_reason.map(|reason| {
                                reason
                                    .iter()
                                    .take_while(|byte| **byte != 0)
                                    .map(|byte| *byte as char)
                                    .collect::<String>()
                            }),
                        }),
                    }
                }
            });
        }

        self.verify_aave_simulation_pin(
            &primary,
            first,
            &expected_block,
            ProviderSlot::Primary,
            pacing_deadline,
        )
        .await?;
        let now = Instant::now();
        if let Some(key) = primary_pending_key {
            self.aave_simulation_context_cache.lock().await.insert(
                key,
                primary_context,
                AAVE_SIMULATION_CONTEXT_TTL,
                now,
            );
        }
        self.mark_provider_success(primary.provider_id()).await;
        let response = AaveSimulateBatchResponse {
            schema_version: AAVE_SIMULATE_BATCH_RESPONSE_SCHEMA.to_string(),
            chain_id: ARBITRUM_ONE_CHAIN_ID,
            request_id: request.request_id.clone(),
            block_number: first.block_number,
            block_hash: first.block_hash.clone(),
            state_root: first.state_root.clone(),
            primary_provider_id: primary.provider_id().to_string(),
            confirmation_provider_id: None,
            quorum: 1,
            evidence_mode: if first.atlas_mode {
                SINGLE_PRIMARY_ATLAS_CALLBACK_FORK_EVIDENCE.to_string()
            } else if first.counterfactual {
                SINGLE_PRIMARY_COUNTERFACTUAL_FORK_EVIDENCE.to_string()
            } else {
                SINGLE_PRIMARY_FORK_EVIDENCE.to_string()
            },
            results,
            resolved_at_unix_ms: unix_time_ms(),
        };
        response
            .validate(&request)
            .map_err(|_| GatewayError::ProviderDisagreement)?;
        Ok(response)
    }

    #[allow(clippy::too_many_arguments)]
    async fn simulate_aave_batch_item(
        &self,
        primary: &ProviderLease,
        request: &AaveSimulateRequest,
        block: &PinnedBlock,
        primary_context: &AaveSimulationContext,
        pacing_deadline: Instant,
    ) -> Result<AaveSimulateResponse, GatewayError> {
        let route_id = aave_simulation_route_id(request)?;
        let calldata = encode_aave_liquidation_call(request, &route_id)?;
        let primary_evidence = self
            .perform_aave_simulation_with_context(
                primary,
                request,
                block,
                &calldata,
                primary_context,
                ProviderSlot::Primary,
                pacing_deadline,
            )
            .await?;
        build_aave_simulation_response(
            request,
            &route_id,
            &calldata,
            primary.provider_id(),
            primary_evidence,
        )
    }

    #[allow(clippy::too_many_arguments)]
    async fn aave_pause_state_under_override(
        &self,
        provider: &ProviderLease,
        request: &AaveSimulateRequest,
        block: &PinnedBlock,
        context: &AaveSimulationContext,
        pacing_deadline: Instant,
    ) -> Option<bool> {
        let executor_state_diff = aave_executor_simulation_state_diff(
            request,
            &context.packed_executor_config,
            context.maximum_input_amount,
        )
        .ok()?;
        let result = self
            .paced_recorded_call(
                provider,
                RpcMethod::EthCall,
                json!([
                    {"to": request.executor_address, "data": "0x5c975abb"},
                    format_quantity(block.number),
                    {
                        request.executor_address.clone(): {
                            "stateDiff": executor_state_diff.clone()
                        }
                    }
                ]),
                Some(block),
                0,
                ProviderSlot::Primary,
                None,
                false,
                pacing_deadline,
            )
            .await
            .ok()?;
        let paused = result.value.as_str().and_then(parse_hex_u256_word)?;
        Some(paused != U256::zero())
    }

    async fn provider_block(
        &self,
        provider: &ProviderLease,
        tag: &str,
        slot: ProviderSlot,
    ) -> Result<PinnedBlock, GatewayError> {
        let result = self
            .recorded_call(
                provider,
                RpcMethod::EthGetBlockByNumber,
                json!([tag, false]),
                None,
                0,
                slot,
                None,
                false,
            )
            .await
            .map_err(|failure| map_call_failure(failure.cause))?;
        parse_block(&result.value).ok_or(GatewayError::ProviderIntegrity)
    }

    async fn provider_block_state_paced(
        &self,
        provider: &ProviderLease,
        tag: &str,
        slot: ProviderSlot,
        pacing_deadline: Instant,
    ) -> Result<PinnedBlockState, GatewayError> {
        let result = self
            .paced_recorded_call(
                provider,
                RpcMethod::EthGetBlockByNumber,
                json!([tag, false]),
                None,
                0,
                slot,
                None,
                false,
                pacing_deadline,
            )
            .await
            .map_err(|failure| map_call_failure(failure.cause))?;
        let block = parse_block(&result.value).ok_or(GatewayError::ProviderIntegrity)?;
        let state_root =
            parse_block_state_root(&result.value).ok_or(GatewayError::ProviderIntegrity)?;
        Ok(PinnedBlockState { block, state_root })
    }

    async fn aave_tail_logs(
        &self,
        provider: &ProviderLease,
        from_block: u64,
        to_block: u64,
        slot: ProviderSlot,
    ) -> Result<Vec<NormalizedAaveTailLog>, GatewayError> {
        let result = self
            .recorded_call(
                provider,
                RpcMethod::EthGetLogs,
                json!([{
                    "address": AAVE_V3_POOL_ARBITRUM,
                    "fromBlock": format_quantity(from_block),
                    "toBlock": format_quantity(to_block),
                    "topics": [[AAVE_BORROW_TOPIC, AAVE_REPAY_TOPIC, AAVE_LIQUIDATION_TOPIC]]
                }]),
                None,
                0,
                slot,
                None,
                false,
            )
            .await
            .map_err(|failure| map_call_failure(failure.cause))?;
        normalize_aave_tail_logs(&result.value, from_block, to_block)
    }

    async fn perform_aave_simulation(
        &self,
        provider: &ProviderLease,
        request: &AaveSimulateRequest,
        block: &PinnedBlock,
        calldata: &[u8],
        slot: ProviderSlot,
    ) -> Result<AaveSimulationEvidence, GatewayError> {
        let block_quantity = format_quantity(block.number);
        let block_evidence = self
            .recorded_call(
                provider,
                RpcMethod::EthGetBlockByNumber,
                json!([block_quantity.clone(), false]),
                Some(block),
                0,
                slot,
                None,
                false,
            )
            .await
            .map_err(|failure| map_call_failure(failure.cause))?;
        if parse_block(&block_evidence.value).as_ref() != Some(block)
            || block_evidence
                .value
                .get("stateRoot")
                .and_then(Value::as_str)
                .map(str::to_ascii_lowercase)
                .as_deref()
                != Some(request.state_root.as_str())
        {
            return Err(GatewayError::ProviderDisagreement);
        }
        if self
            .exact_code_hash(provider, &request.executor_address, block, slot)
            .await?
            != request.executor_code_hash
        {
            return Err(GatewayError::ProviderIntegrity);
        }
        let packed = self
            .recorded_call(
                provider,
                RpcMethod::EthGetStorageAt,
                json!([
                    request.executor_address,
                    PHOENIX_EXECUTOR_PACKED_CONFIG_SLOT,
                    block_quantity.clone()
                ]),
                Some(block),
                0,
                slot,
                None,
                false,
            )
            .await
            .map_err(|failure| map_call_failure(failure.cause))?
            .value
            .as_str()
            .and_then(decode_executor_packed_config)
            .ok_or(GatewayError::ProviderIntegrity)?;
        let maximum_input_amount = self
            .recorded_call(
                provider,
                RpcMethod::EthGetStorageAt,
                json!([
                    request.executor_address,
                    PHOENIX_EXECUTOR_MAXIMUM_INPUT_SLOT,
                    block_quantity.clone()
                ]),
                Some(block),
                0,
                slot,
                None,
                false,
            )
            .await
            .map_err(|failure| map_call_failure(failure.cause))?
            .value
            .as_str()
            .and_then(parse_hex_u256_word)
            .ok_or(GatewayError::ProviderIntegrity)?;
        let executor_state_diff =
            aave_executor_simulation_state_diff(request, &packed, maximum_input_amount)?;
        let result = self
            .recorded_call(
                provider,
                RpcMethod::EthCall,
                json!([
                    {
                        "from": request.caller_address,
                        "to": request.executor_address,
                        "gas": format_quantity(request.gas_limit),
                        "data": format!("0x{}", hex::encode(calldata))
                    },
                    block_quantity.clone(),
                    {
                        request.executor_address.clone(): {
                            "stateDiff": executor_state_diff.clone()
                        }
                    }
                ]),
                Some(block),
                0,
                slot,
                None,
                false,
            )
            .await
            .map_err(|failure| map_call_failure(failure.cause))?
            .value
            .as_str()
            .and_then(parse_hex_u256_word)
            .ok_or(GatewayError::ProviderIntegrity)?;
        let gas_components = self
            .recorded_call(
                provider,
                RpcMethod::EthCall,
                json!([
                    {
                        "from": request.caller_address,
                        "to": ARBITRUM_NODE_INTERFACE,
                        "data": encode_gas_estimate_components(&request.executor_address, calldata)?
                    },
                    block_quantity.clone(),
                    {
                        request.executor_address.clone(): {
                            "stateDiff": executor_state_diff
                        }
                    }
                ]),
                Some(block),
                0,
                slot,
                None,
                false,
            )
            .await
            .map_err(|failure| map_call_failure(failure.cause))?
            .value
            .as_str()
            .and_then(decode_gas_estimate_components)
            .ok_or(GatewayError::ProviderIntegrity)?;
        let premium_bps = self
            .recorded_call(
                provider,
                RpcMethod::EthCall,
                json!([{
                    "to": AAVE_V3_POOL_ARBITRUM,
                    "data": AAVE_FLASHLOAN_PREMIUM_TOTAL_SELECTOR
                }, block_quantity]),
                Some(block),
                0,
                slot,
                None,
                false,
            )
            .await
            .map_err(|failure| map_call_failure(failure.cause))?
            .value
            .as_str()
            .and_then(parse_hex_u256_word)
            .and_then(|value| u16::try_from(value).ok())
            .filter(|value| u64::from(*value) <= PERCENTAGE_FACTOR)
            .ok_or(GatewayError::ProviderIntegrity)?;
        Ok(AaveSimulationEvidence {
            realized_profit: result,
            total_gas: gas_components.0,
            l1_gas: gas_components.1,
            base_fee_per_gas: gas_components.2,
            flash_premium_bps: premium_bps,
        })
    }

    async fn verify_aave_simulation_pin(
        &self,
        provider: &ProviderLease,
        request: &AaveSimulateRequest,
        block: &PinnedBlock,
        slot: ProviderSlot,
        pacing_deadline: Instant,
    ) -> Result<(), GatewayError> {
        let evidence = self
            .paced_recorded_call(
                provider,
                RpcMethod::EthGetBlockByNumber,
                json!([format_quantity(block.number), false]),
                Some(block),
                0,
                slot,
                None,
                false,
                pacing_deadline,
            )
            .await
            .map_err(|failure| map_call_failure(failure.cause))?;
        if parse_block(&evidence.value).as_ref() != Some(block)
            || evidence
                .value
                .get("stateRoot")
                .and_then(Value::as_str)
                .map(str::to_ascii_lowercase)
                .as_deref()
                != Some(request.state_root.as_str())
        {
            return Err(GatewayError::ProviderDisagreement);
        }
        Ok(())
    }

    async fn aave_simulation_context(
        &self,
        provider: &ProviderLease,
        request: &AaveSimulateRequest,
        block: &PinnedBlock,
        slot: ProviderSlot,
        pacing_deadline: Instant,
    ) -> Result<(AaveSimulationContext, Option<String>), GatewayError> {
        let key = format!(
            "{}:{}:{}:{}:{}:{}",
            provider.provider_id(),
            block.number,
            block.hash,
            request.state_root,
            request.executor_address,
            request.executor_code_hash
        );
        if let Some(context) = self
            .aave_simulation_context_cache
            .lock()
            .await
            .get(&key, Instant::now())
        {
            return Ok((context, None));
        }
        let code = self
            .paced_recorded_call(
                provider,
                RpcMethod::EthGetCode,
                json!([request.executor_address, format_quantity(block.number)]),
                Some(block),
                0,
                slot,
                None,
                false,
                pacing_deadline,
            )
            .await
            .map_err(|failure| map_call_failure(failure.cause))?
            .value
            .as_str()
            .map(str::to_ascii_lowercase)
            .filter(|value| value != "0x" && canonical_data(value, MAX_MULTICALL_CODE_BYTES))
            .ok_or(GatewayError::ProviderIntegrity)?;
        let code_hash = canonical_hash_bytes(
            &hex::decode(&code[2..]).map_err(|_| GatewayError::ProviderIntegrity)?,
        );
        if code_hash != request.executor_code_hash {
            return Err(GatewayError::ProviderIntegrity);
        }
        let block_quantity = format_quantity(block.number);
        let packed_executor_config = self
            .paced_recorded_call(
                provider,
                RpcMethod::EthGetStorageAt,
                json!([
                    request.executor_address,
                    PHOENIX_EXECUTOR_PACKED_CONFIG_SLOT,
                    block_quantity.clone()
                ]),
                Some(block),
                0,
                slot,
                None,
                false,
                pacing_deadline,
            )
            .await
            .map_err(|failure| map_call_failure(failure.cause))?
            .value
            .as_str()
            .and_then(decode_executor_packed_config)
            .ok_or(GatewayError::ProviderIntegrity)?;
        let maximum_input_amount = self
            .paced_recorded_call(
                provider,
                RpcMethod::EthGetStorageAt,
                json!([
                    request.executor_address,
                    PHOENIX_EXECUTOR_MAXIMUM_INPUT_SLOT,
                    block_quantity.clone()
                ]),
                Some(block),
                0,
                slot,
                None,
                false,
                pacing_deadline,
            )
            .await
            .map_err(|failure| map_call_failure(failure.cause))?
            .value
            .as_str()
            .and_then(parse_hex_u256_word)
            .ok_or(GatewayError::ProviderIntegrity)?;
        aave_executor_simulation_state_diff(
            request,
            &packed_executor_config,
            maximum_input_amount,
        )?;
        let flash_premium_bps = self
            .paced_recorded_call(
                provider,
                RpcMethod::EthCall,
                json!([{
                    "to": AAVE_V3_POOL_ARBITRUM,
                    "data": AAVE_FLASHLOAN_PREMIUM_TOTAL_SELECTOR
                }, block_quantity]),
                Some(block),
                0,
                slot,
                None,
                false,
                pacing_deadline,
            )
            .await
            .map_err(|failure| map_call_failure(failure.cause))?
            .value
            .as_str()
            .and_then(parse_hex_u256_word)
            .and_then(|value| u16::try_from(value).ok())
            .filter(|value| u64::from(*value) <= PERCENTAGE_FACTOR)
            .ok_or(GatewayError::ProviderIntegrity)?;
        Ok((
            AaveSimulationContext {
                packed_executor_config,
                maximum_input_amount,
                flash_premium_bps,
            },
            Some(key),
        ))
    }

    #[allow(clippy::too_many_arguments)]
    async fn perform_aave_simulation_with_context(
        &self,
        provider: &ProviderLease,
        request: &AaveSimulateRequest,
        block: &PinnedBlock,
        calldata: &[u8],
        context: &AaveSimulationContext,
        slot: ProviderSlot,
        pacing_deadline: Instant,
    ) -> Result<AaveSimulationEvidence, GatewayError> {
        let block_quantity = format_quantity(block.number);
        let executor_state_diff = aave_executor_simulation_state_diff(
            request,
            &context.packed_executor_config,
            context.maximum_input_amount,
        )?;
        let result = self
            .paced_recorded_call(
                provider,
                RpcMethod::EthCall,
                json!([
                    {
                        "from": request.caller_address,
                        "to": request.executor_address,
                        "gas": format_quantity(request.gas_limit),
                        "data": format!("0x{}", hex::encode(calldata))
                    },
                    block_quantity.clone(),
                    {
                        request.executor_address.clone(): {
                            "stateDiff": executor_state_diff.clone()
                        }
                    }
                ]),
                Some(block),
                0,
                slot,
                None,
                false,
                pacing_deadline,
            )
            .await
            .map_err(|failure| map_call_failure(failure.cause))?
            .value
            .as_str()
            .and_then(parse_hex_u256_word)
            .ok_or(GatewayError::ProviderIntegrity)?;
        let gas_components = self
            .paced_recorded_call(
                provider,
                RpcMethod::EthCall,
                json!([
                    {
                        "from": request.caller_address,
                        "to": ARBITRUM_NODE_INTERFACE,
                        "data": encode_gas_estimate_components(&request.executor_address, calldata)?
                    },
                    block_quantity,
                    {
                        request.executor_address.clone(): {
                            "stateDiff": executor_state_diff
                        }
                    }
                ]),
                Some(block),
                0,
                slot,
                None,
                false,
                pacing_deadline,
            )
            .await
            .map_err(|failure| map_call_failure(failure.cause))?
            .value
            .as_str()
            .and_then(decode_gas_estimate_components)
            .ok_or(GatewayError::ProviderIntegrity)?;
        Ok(AaveSimulationEvidence {
            realized_profit: result,
            total_gas: gas_components.0,
            l1_gas: gas_components.1,
            base_fee_per_gas: gas_components.2,
            flash_premium_bps: context.flash_premium_bps,
        })
    }

    async fn perform_aave_exact(
        &self,
        provider: &ProviderLease,
        request: &AaveExactRequest,
        block: &PinnedBlock,
        expected_state_root: &str,
        slot: ProviderSlot,
        pacing_deadline: Instant,
    ) -> Result<AaveExactProviderResult, GatewayError> {
        let block_quantity = format_quantity(block.number);
        let supported_assets = [ARBITRUM_WETH, ARBITRUM_NATIVE_USDC, ARBITRUM_USDC_E];
        let static_key =
            aave_exact_static_context_key(provider.provider_id(), block, expected_state_root);
        let cached_context = self
            .aave_exact_static_context_cache
            .lock()
            .await
            .get(&static_key, Instant::now());
        let (
            pool_code_hash,
            implementation_word,
            implementation_code_hash,
            reserve_ids,
            pending_context,
        ) = if let Some(context) = cached_context {
            if context.state_root != expected_state_root
                || context.reserve_ids.len() != supported_assets.len()
            {
                return Err(GatewayError::ProviderIntegrity);
            }
            (
                context.pool_code_hash,
                context.pool_implementation,
                context.pool_implementation_code_hash,
                context.reserve_ids,
                None,
            )
        } else {
            let pool_code_hash = self
                .exact_code_hash_paced(
                    provider,
                    AAVE_V3_POOL_ARBITRUM,
                    block,
                    slot,
                    pacing_deadline,
                )
                .await?;
            let implementation_word = self
                .paced_recorded_call(
                    provider,
                    RpcMethod::EthGetStorageAt,
                    json!([
                        AAVE_V3_POOL_ARBITRUM,
                        EIP1967_IMPLEMENTATION_SLOT,
                        block_quantity.clone()
                    ]),
                    Some(block),
                    0,
                    slot,
                    None,
                    false,
                    pacing_deadline,
                )
                .await
                .map_err(|failure| map_call_failure(failure.cause))?
                .value
                .as_str()
                .and_then(parse_storage_address)
                .filter(|value| value == AAVE_POOL_IMPLEMENTATION_ARBITRUM)
                .ok_or(GatewayError::ProviderIntegrity)?;
            let implementation_code_hash = self
                .exact_code_hash_paced(provider, &implementation_word, block, slot, pacing_deadline)
                .await?;
            let reserve_list = self
                .hunter_multicall_paced(
                    provider,
                    block,
                    slot,
                    &[EthCall {
                        target: AAVE_V3_POOL_ARBITRUM.to_string(),
                        calldata: AAVE_GET_RESERVES_LIST_SELECTOR.to_string(),
                    }],
                    pacing_deadline,
                )
                .await?
                .into_iter()
                .next()
                .and_then(|value| decode_address_array(&value))
                .ok_or(GatewayError::ProviderIntegrity)?;
            let reserve_ids = supported_assets
                .iter()
                .map(|asset| {
                    reserve_list
                        .iter()
                        .position(|candidate| candidate == asset)
                        .and_then(|index| u16::try_from(index).ok())
                        .ok_or(GatewayError::ProviderIntegrity)
                })
                .collect::<Result<Vec<_>, _>>()?;
            let pending = AaveExactStaticContext {
                pool_code_hash: pool_code_hash.clone(),
                pool_implementation: implementation_word.clone(),
                pool_implementation_code_hash: implementation_code_hash.clone(),
                reserve_ids: reserve_ids.clone(),
                state_root: expected_state_root.to_string(),
            };
            (
                pool_code_hash,
                implementation_word,
                implementation_code_hash,
                reserve_ids,
                Some((static_key.clone(), pending)),
            )
        };

        let account_call =
            encode_one_address(AAVE_GET_USER_ACCOUNT_DATA_SELECTOR, &request.borrower);
        let user_configuration_call =
            encode_one_address(AAVE_GET_USER_CONFIGURATION_SELECTOR, &request.borrower);
        let user_emode_call = encode_one_address(AAVE_GET_USER_EMODE_SELECTOR, &request.borrower);
        let mut calls = vec![
            EthCall {
                target: AAVE_V3_POOL_ARBITRUM.to_string(),
                calldata: account_call,
            },
            EthCall {
                target: AAVE_V3_POOL_ARBITRUM.to_string(),
                calldata: user_configuration_call,
            },
            EthCall {
                target: AAVE_V3_POOL_ARBITRUM.to_string(),
                calldata: user_emode_call,
            },
            EthCall {
                target: AAVE_V3_POOL_ARBITRUM.to_string(),
                calldata: AAVE_FLASHLOAN_PREMIUM_TOTAL_SELECTOR.to_string(),
            },
        ];
        for (asset, reserve_id) in supported_assets.iter().zip(&reserve_ids) {
            calls.push(EthCall {
                target: AAVE_DATA_PROVIDER_ARBITRUM.to_string(),
                calldata: encode_two_addresses(
                    AAVE_GET_USER_RESERVE_DATA_SELECTOR,
                    asset,
                    &request.borrower,
                ),
            });
            calls.push(EthCall {
                target: AAVE_V3_POOL_ARBITRUM.to_string(),
                calldata: encode_one_address(AAVE_GET_CONFIGURATION_SELECTOR, asset),
            });
            calls.push(EthCall {
                target: AAVE_DATA_PROVIDER_ARBITRUM.to_string(),
                calldata: encode_one_address(AAVE_GET_RESERVE_TOKENS_SELECTOR, asset),
            });
            calls.push(EthCall {
                target: AAVE_ORACLE_ARBITRUM.to_string(),
                calldata: encode_one_address(AAVE_ORACLE_GET_ASSET_PRICE_SELECTOR, asset),
            });
            calls.push(EthCall {
                target: AAVE_V3_POOL_ARBITRUM.to_string(),
                calldata: encode_one_address(AAVE_GET_LIQUIDATION_GRACE_PERIOD_SELECTOR, asset),
            });
            calls.push(EthCall {
                target: AAVE_V3_POOL_ARBITRUM.to_string(),
                calldata: encode_one_u256(
                    AAVE_GET_RESERVE_ADDRESS_BY_ID_SELECTOR,
                    U256::from(*reserve_id),
                ),
            });
        }
        let results = self
            .hunter_multicall_paced(provider, block, slot, &calls, pacing_deadline)
            .await?;
        if results.len() != 22
            || results[0].len() != 32 * 6
            || results[1].len() != 32
            || results[2].len() != 32
            || results[3].len() != 32
        {
            return Err(GatewayError::ProviderIntegrity);
        }
        let account_words = results[0]
            .chunks_exact(32)
            .map(U256::from_big_endian)
            .collect::<Vec<_>>();
        let account = AaveAccountData {
            borrower: request.borrower.clone(),
            total_collateral_base: account_words[0].to_string(),
            total_debt_base: account_words[1].to_string(),
            available_borrows_base: account_words[2].to_string(),
            current_liquidation_threshold_bps: account_words[3].to_string(),
            loan_to_value_bps: account_words[4].to_string(),
            health_factor_wad: account_words[5].to_string(),
        };
        let user_configuration = U256::from_big_endian(&results[1]).to_string();
        let user_emode_category = u8::try_from(U256::from_big_endian(&results[2]))
            .map_err(|_| GatewayError::ProviderIntegrity)?;
        let flash_premium_bps = u16::try_from(U256::from_big_endian(&results[3]))
            .ok()
            .filter(|value| u64::from(*value) <= PERCENTAGE_FACTOR)
            .ok_or(GatewayError::ProviderIntegrity)?;
        let mut reserves = Vec::with_capacity(3);
        for (index, asset) in supported_assets.iter().enumerate() {
            let offset = 4 + index * 6;
            if results[offset].len() != 32 * 9
                || results[offset + 1].len() != 32
                || results[offset + 2].len() != 32 * 3
                || results[offset + 3].len() != 32
                || results[offset + 4].len() != 32
                || results[offset + 5].len() != 32
            {
                return Err(GatewayError::ProviderIntegrity);
            }
            let user_words = results[offset]
                .chunks_exact(32)
                .map(U256::from_big_endian)
                .collect::<Vec<_>>();
            let token_words = results[offset + 2]
                .chunks_exact(32)
                .map(|word| parse_address_bytes(word).ok_or(GatewayError::ProviderIntegrity))
                .collect::<Result<Vec<_>, _>>()?;
            let oracle_price = U256::from_big_endian(&results[offset + 3]);
            if oracle_price.is_zero() {
                return Err(GatewayError::ProviderIntegrity);
            }
            let configuration = U256::from_big_endian(&results[offset + 1]);
            let decimals = u8::try_from((configuration >> 48).low_u32() & 0xff)
                .ok()
                .filter(|value| *value <= 36)
                .ok_or(GatewayError::ProviderIntegrity)?;
            let grace_period = u64::try_from(U256::from_big_endian(&results[offset + 4]))
                .map_err(|_| GatewayError::ProviderIntegrity)?;
            if parse_address_bytes(&results[offset + 5]).as_deref() != Some(*asset) {
                return Err(GatewayError::ProviderIntegrity);
            }
            reserves.push(AaveExactReserveState {
                asset: (*asset).to_string(),
                reserve_id: reserve_ids[index],
                decimals,
                current_a_token_balance: user_words[0].to_string(),
                current_stable_debt: user_words[1].to_string(),
                current_variable_debt: user_words[2].to_string(),
                usage_as_collateral_enabled: !user_words[8].is_zero(),
                configuration_data: configuration.to_string(),
                a_token: token_words[0].clone(),
                stable_debt_token: token_words[1].clone(),
                variable_debt_token: token_words[2].clone(),
                oracle_price_base: oracle_price.to_string(),
                liquidation_grace_period_until: grace_period,
            });
        }

        let (emode_liquidation_bonus_bps, emode_collateral_bitmap) = if user_emode_category == 0 {
            (0_u16, U256::zero())
        } else {
            let category = U256::from(user_emode_category);
            let emode_results = self
                .hunter_multicall_paced(
                    provider,
                    block,
                    slot,
                    &[
                        EthCall {
                            target: AAVE_V3_POOL_ARBITRUM.to_string(),
                            calldata: encode_one_u256(
                                AAVE_GET_EMODE_COLLATERAL_CONFIG_SELECTOR,
                                category,
                            ),
                        },
                        EthCall {
                            target: AAVE_V3_POOL_ARBITRUM.to_string(),
                            calldata: encode_one_u256(
                                AAVE_GET_EMODE_COLLATERAL_BITMAP_SELECTOR,
                                category,
                            ),
                        },
                    ],
                    pacing_deadline,
                )
                .await?;
            if emode_results.len() != 2
                || emode_results[0].len() != 96
                || emode_results[1].len() != 32
            {
                return Err(GatewayError::ProviderIntegrity);
            }
            let bonus = u16::try_from(U256::from_big_endian(&emode_results[0][64..96]))
                .ok()
                .filter(|value| (10_000..=20_000).contains(value))
                .ok_or(GatewayError::ProviderIntegrity)?;
            (bonus, U256::from_big_endian(&emode_results[1]))
        };

        let maximum_input = decimal_u256(&request.maximum_input_amount)?;
        let mut liquidations = supported_aave_liquidations(
            &account,
            &reserves,
            maximum_input,
            emode_collateral_bitmap,
            emode_liquidation_bonus_bps,
            flash_premium_bps,
        )?;
        let reviewed_routes = reviewed_aave_unwind_routes()?;
        let mut quote_specs = Vec::new();
        for (index, opportunity) in liquidations.iter().enumerate() {
            if opportunity.collateral_asset == opportunity.debt_asset {
                continue;
            }
            for route in &reviewed_routes {
                let pair_matches = (route.token0 == opportunity.collateral_asset
                    && route.token1 == opportunity.debt_asset)
                    || (route.token1 == opportunity.collateral_asset
                        && route.token0 == opportunity.debt_asset);
                if pair_matches {
                    quote_specs.push((index, route.clone()));
                }
            }
        }
        if !quote_specs.is_empty() {
            let quote_calls = quote_specs
                .iter()
                .map(|(index, route)| {
                    let amount = decimal_u256(&liquidations[*index].liquidator_collateral)
                        .expect("validated liquidation collateral");
                    EthCall {
                        target: UNISWAP_V3_QUOTER_V2_ARBITRUM.to_string(),
                        calldata: encode_uniswap_quote(
                            &liquidations[*index].collateral_asset,
                            &liquidations[*index].debt_asset,
                            amount,
                            route.fee,
                        ),
                    }
                })
                .collect::<Vec<_>>();
            let quotes = self
                .hunter_multicall_paced(provider, block, slot, &quote_calls, pacing_deadline)
                .await?;
            if quotes.len() != quote_calls.len() || quotes.iter().any(|value| value.len() < 32) {
                return Err(GatewayError::ProviderIntegrity);
            }
            for (quote_index, (liquidation_index, route)) in quote_specs.into_iter().enumerate() {
                let output_debt_asset = U256::from_big_endian(&quotes[quote_index][..32]);
                let debt_price =
                    decimal_u256(&liquidations[liquidation_index].debt_asset_price_base)?;
                let weth_price = decimal_u256(&liquidations[liquidation_index].weth_price_base)?;
                let debt_unit = asset_unit(liquidations[liquidation_index].debt_asset_decimals)?;
                let output_weth =
                    debt_to_weth_floor(output_debt_asset, debt_price, weth_price, debt_unit)?;
                liquidations[liquidation_index]
                    .unwind_quotes
                    .push(AaveExactUnwindQuoteState {
                        pool: route.pool,
                        factory: route.factory,
                        token0: route.token0,
                        token1: route.token1,
                        fee: route.fee,
                        zero_for_one: route.zero_for_one,
                        output_weth: output_weth.to_string(),
                        output_debt_asset: output_debt_asset.to_string(),
                    });
            }
        }
        let verify = self
            .paced_recorded_call(
                provider,
                RpcMethod::EthGetBlockByNumber,
                json!([block_quantity, false]),
                Some(block),
                0,
                slot,
                None,
                false,
                pacing_deadline,
            )
            .await
            .map_err(|failure| map_call_failure(failure.cause))?;
        if parse_block(&verify.value).as_ref() != Some(block) {
            return Err(GatewayError::ProviderIntegrity);
        }
        let block_timestamp = verify
            .value
            .get("timestamp")
            .and_then(Value::as_str)
            .and_then(|value| u64::from_str_radix(value.trim_start_matches("0x"), 16).ok())
            .ok_or(GatewayError::ProviderIntegrity)?;
        liquidations.retain(|opportunity| {
            let debt_grace = reserves
                .iter()
                .find(|reserve| reserve.asset == opportunity.debt_asset)
                .map(|reserve| reserve.liquidation_grace_period_until)
                .unwrap_or(u64::MAX);
            let collateral_grace = reserves
                .iter()
                .find(|reserve| reserve.asset == opportunity.collateral_asset)
                .map(|reserve| reserve.liquidation_grace_period_until)
                .unwrap_or(u64::MAX);
            aave_liquidation_grace_elapsed(debt_grace, collateral_grace, block_timestamp)
        });
        let state_root = verify
            .value
            .get("stateRoot")
            .and_then(Value::as_str)
            .map(str::to_ascii_lowercase)
            .filter(|value| canonical_block_hash(value))
            .ok_or(GatewayError::ProviderIntegrity)?;
        if state_root != expected_state_root {
            return Err(GatewayError::ProviderIntegrity);
        }
        Ok((
            AaveExactProviderState {
                provider_id: provider.provider_id().to_string(),
                pool_code_hash,
                pool_implementation: implementation_word,
                pool_implementation_code_hash: implementation_code_hash,
                user_configuration,
                user_emode_category,
                emode_collateral_bitmap: emode_collateral_bitmap.to_string(),
                emode_liquidation_bonus_bps,
                flash_premium_bps,
                account,
                reserves,
                liquidations,
            },
            state_root,
            pending_context,
        ))
    }

    async fn exact_code_hash(
        &self,
        provider: &ProviderLease,
        address: &str,
        block: &PinnedBlock,
        slot: ProviderSlot,
    ) -> Result<String, GatewayError> {
        let code = self
            .recorded_call(
                provider,
                RpcMethod::EthGetCode,
                json!([address, format_quantity(block.number)]),
                Some(block),
                0,
                slot,
                None,
                false,
            )
            .await
            .map_err(|failure| map_call_failure(failure.cause))?
            .value
            .as_str()
            .map(str::to_ascii_lowercase)
            .filter(|value| value != "0x" && canonical_data(value, MAX_MULTICALL_CODE_BYTES))
            .ok_or(GatewayError::ProviderIntegrity)?;
        let bytes = hex::decode(&code[2..]).map_err(|_| GatewayError::ProviderIntegrity)?;
        Ok(canonical_hash_bytes(&bytes))
    }

    async fn exact_code_hash_paced(
        &self,
        provider: &ProviderLease,
        address: &str,
        block: &PinnedBlock,
        slot: ProviderSlot,
        pacing_deadline: Instant,
    ) -> Result<String, GatewayError> {
        let code = self
            .paced_recorded_call(
                provider,
                RpcMethod::EthGetCode,
                json!([address, format_quantity(block.number)]),
                Some(block),
                0,
                slot,
                None,
                false,
                pacing_deadline,
            )
            .await
            .map_err(|failure| map_call_failure(failure.cause))?
            .value
            .as_str()
            .map(str::to_ascii_lowercase)
            .filter(|value| value != "0x" && canonical_data(value, MAX_MULTICALL_CODE_BYTES))
            .ok_or(GatewayError::ProviderIntegrity)?;
        let bytes = hex::decode(&code[2..]).map_err(|_| GatewayError::ProviderIntegrity)?;
        Ok(canonical_hash_bytes(&bytes))
    }

    async fn perform_aave_screen(
        &self,
        provider: &ProviderLease,
        request: &AaveScreenRequest,
        block: &PinnedBlock,
        slot: ProviderSlot,
    ) -> Result<(Vec<AaveAccountData>, String), GatewayError> {
        let mut calls = request
            .borrowers
            .iter()
            .map(|borrower| EthCall {
                target: AAVE_V3_POOL_ARBITRUM.to_string(),
                calldata: format!(
                    "{}{}{}",
                    AAVE_GET_USER_ACCOUNT_DATA_SELECTOR,
                    "0".repeat(24),
                    &borrower[2..]
                ),
            })
            .collect::<Vec<_>>();
        calls.push(EthCall {
            target: "0xb56c2f0b653b2e0b10c9b928c8580ac5df02c7c7".to_string(),
            calldata: format!(
                "0xb3596f07{}{}",
                "0".repeat(24),
                &"0x82af49447d8a07e3bd95bd0d56f35241523fbab1"[2..]
            ),
        });
        let results = self.hunter_multicall(provider, block, slot, &calls).await?;
        if results.len() != request.borrowers.len() + 1 {
            return Err(GatewayError::ProviderIntegrity);
        }
        let weth_price = results
            .last()
            .filter(|value| value.len() == 32)
            .map(|value| U256::from_big_endian(value).to_string())
            .filter(|value| value != "0")
            .ok_or(GatewayError::ProviderIntegrity)?;
        let accounts = request
            .borrowers
            .iter()
            .zip(results.into_iter().take(request.borrowers.len()))
            .map(|(borrower, bytes)| {
                if bytes.len() != 32 * 6 {
                    return Err(GatewayError::ProviderIntegrity);
                }
                let words = bytes
                    .chunks_exact(32)
                    .map(U256::from_big_endian)
                    .collect::<Vec<_>>();
                Ok(AaveAccountData {
                    borrower: borrower.clone(),
                    total_collateral_base: words[0].to_string(),
                    total_debt_base: words[1].to_string(),
                    available_borrows_base: words[2].to_string(),
                    current_liquidation_threshold_bps: words[3].to_string(),
                    loan_to_value_bps: words[4].to_string(),
                    health_factor_wad: words[5].to_string(),
                })
            })
            .collect::<Result<Vec<_>, _>>()?;
        // The exact finalized number and hash were already read and required to
        // agree independently before either pinned call. Re-reading that same
        // finalized block after each eth_call adds no identity evidence, while
        // making the minimum single-primary screen exceed the reviewed call
        // transport burst (two heads plus two Multicalls).
        Ok((accounts, weth_price))
    }

    async fn perform_hunter_state_bundle(
        &self,
        provider: &ProviderLease,
        request: &HunterStateRequest,
        block: &PinnedBlock,
        slot: ProviderSlot,
    ) -> Result<Vec<PinnedV3PoolState>, GatewayError> {
        let block_quantity = format_quantity(block.number);
        let mut code_hashes = Vec::with_capacity(request.pools.len());
        for pool in &request.pools {
            let code = self
                .recorded_call(
                    provider,
                    RpcMethod::EthGetCode,
                    json!([pool.pool_address, block_quantity.clone()]),
                    Some(block),
                    0,
                    slot,
                    None,
                    false,
                )
                .await
                .map_err(|failure| map_call_failure(failure.cause))?;
            let code = code
                .value
                .as_str()
                .map(str::to_ascii_lowercase)
                .filter(|value| {
                    value != "0x"
                        && canonical_data(value, MAX_MULTICALL_CODE_BYTES)
                        && value[2..].bytes().any(|byte| byte != b'0')
                })
                .ok_or(GatewayError::ProviderIntegrity)?;
            let bytes = hex::decode(&code[2..]).map_err(|_| GatewayError::ProviderIntegrity)?;
            code_hashes.push(canonical_hash_bytes(&bytes));
        }

        let mut identity_calls = Vec::with_capacity(request.pools.len() * 7);
        for pool in &request.pools {
            for selector in [
                FACTORY_SELECTOR,
                TOKEN0_SELECTOR,
                TOKEN1_SELECTOR,
                FEE_SELECTOR,
                TICK_SPACING_SELECTOR,
                SLOT0_SELECTOR,
                LIQUIDITY_SELECTOR,
            ] {
                identity_calls.push(EthCall {
                    target: pool.pool_address.clone(),
                    calldata: selector.to_string(),
                });
            }
        }
        let identity_results = self
            .hunter_multicall(provider, block, slot, &identity_calls)
            .await?;
        let mut interim = Vec::with_capacity(request.pools.len());
        for (pool_index, pool) in request.pools.iter().enumerate() {
            let offset = pool_index * 7;
            let factory = parse_address_bytes(&identity_results[offset])
                .ok_or(GatewayError::ProviderIntegrity)?;
            let token0 = parse_address_bytes(&identity_results[offset + 1])
                .ok_or(GatewayError::ProviderIntegrity)?;
            let token1 = parse_address_bytes(&identity_results[offset + 2])
                .ok_or(GatewayError::ProviderIntegrity)?;
            let fee = parse_u32_bytes(&identity_results[offset + 3])
                .ok_or(GatewayError::ProviderIntegrity)?;
            let tick_spacing = parse_i24_bytes(&identity_results[offset + 4])
                .ok_or(GatewayError::ProviderIntegrity)?;
            let (sqrt_price_x96, tick) = parse_slot0_bytes(&identity_results[offset + 5])
                .ok_or(GatewayError::ProviderIntegrity)?;
            let liquidity = parse_u128_word(&identity_results[offset + 6])
                .filter(|value| *value > 0)
                .ok_or(GatewayError::StateIncomplete)?;
            if factory != pool.factory_address
                || token0 != pool.token0
                || token1 != pool.token1
                || fee != pool.fee
                || tick_spacing != pool.tick_spacing
            {
                return Err(GatewayError::ProviderIntegrity);
            }
            interim.push(HunterPoolInterim {
                sqrt_price_x96,
                tick,
                liquidity,
                word_positions: centered_word_positions(
                    tick,
                    tick_spacing,
                    request.maximum_tick_words_per_pool,
                )?,
            });
        }

        let mut bitmap_calls = Vec::new();
        for (pool, state) in request.pools.iter().zip(&interim) {
            for position in &state.word_positions {
                bitmap_calls.push(EthCall {
                    target: pool.pool_address.clone(),
                    calldata: encode_signed_call("tickBitmap", 16, i128::from(*position)),
                });
            }
        }
        let bitmap_results = self
            .hunter_multicall(provider, block, slot, &bitmap_calls)
            .await?;
        let mut bitmap_offset = 0;
        let mut bitmaps_by_pool = Vec::with_capacity(request.pools.len());
        let mut initialized_by_pool = Vec::with_capacity(request.pools.len());
        let mut total_initialized = 0_usize;
        for state in &interim {
            let mut words = Vec::with_capacity(state.word_positions.len());
            let mut ticks = Vec::new();
            for position in &state.word_positions {
                let bitmap = parse_u256_word(&bitmap_results[bitmap_offset])
                    .ok_or(GatewayError::ProviderIntegrity)?;
                bitmap_offset += 1;
                words.push(TickBitmapWord {
                    word_position: *position,
                    bitmap: format!("0x{bitmap:064x}"),
                });
                for bit in 0..256_usize {
                    if bitmap.bit(bit) {
                        let compressed = i32::from(*position)
                            .checked_mul(256)
                            .and_then(|value| value.checked_add(bit as i32))
                            .ok_or(GatewayError::ProviderIntegrity)?;
                        let tick = compressed
                            .checked_mul(request.pools[bitmaps_by_pool.len()].tick_spacing)
                            .ok_or(GatewayError::ProviderIntegrity)?;
                        if (-887_272..=887_272).contains(&tick) {
                            ticks.push(tick);
                        }
                    }
                }
            }
            total_initialized = total_initialized.saturating_add(ticks.len());
            if total_initialized > request.maximum_initialized_ticks {
                return Err(GatewayError::StateIncomplete);
            }
            bitmaps_by_pool.push(words);
            initialized_by_pool.push(ticks);
        }

        let mut tick_calls = Vec::with_capacity(total_initialized);
        for (pool, ticks) in request.pools.iter().zip(&initialized_by_pool) {
            for tick in ticks {
                tick_calls.push(EthCall {
                    target: pool.pool_address.clone(),
                    calldata: encode_signed_call("ticks", 24, i128::from(*tick)),
                });
            }
        }
        let tick_results = if tick_calls.is_empty() {
            Vec::new()
        } else {
            self.hunter_multicall(provider, block, slot, &tick_calls)
                .await?
        };
        let mut tick_offset = 0;
        let mut states = Vec::with_capacity(request.pools.len());
        for (index, pool) in request.pools.iter().enumerate() {
            let initialized_ticks = initialized_by_pool[index]
                .iter()
                .map(|tick| {
                    let result = tick_results
                        .get(tick_offset)
                        .ok_or(GatewayError::ProviderIntegrity)?;
                    tick_offset += 1;
                    let (liquidity_gross, liquidity_net) =
                        parse_tick_bytes(result).ok_or(GatewayError::ProviderIntegrity)?;
                    if liquidity_gross == 0 {
                        return Err(GatewayError::ProviderIntegrity);
                    }
                    Ok(InitializedTick {
                        tick: *tick,
                        liquidity_gross: liquidity_gross.to_string(),
                        liquidity_net: liquidity_net.to_string(),
                    })
                })
                .collect::<Result<Vec<_>, GatewayError>>()?;
            let first_word = interim[index]
                .word_positions
                .first()
                .copied()
                .ok_or(GatewayError::StateIncomplete)?;
            let last_word = interim[index]
                .word_positions
                .last()
                .copied()
                .ok_or(GatewayError::StateIncomplete)?;
            let coverage_min_tick =
                (i64::from(first_word) * 256 * i64::from(pool.tick_spacing)).max(-887_272) as i32;
            let coverage_max_tick = (((i64::from(last_word) + 1) * 256 - 1)
                * i64::from(pool.tick_spacing))
            .min(887_272) as i32;
            let mut state = PinnedV3PoolState {
                schema_version: crate::hunter_state::PINNED_V3_STATE_SCHEMA.to_string(),
                chain_id: ARBITRUM_ONE_CHAIN_ID,
                block_number: block.number,
                block_hash: block.hash.clone(),
                pool_id: pool.pool_id.clone(),
                pool_address: pool.pool_address.clone(),
                pool_code_hash: code_hashes[index].clone(),
                factory_address: pool.factory_address.clone(),
                protocol_id: pool.protocol_id.clone(),
                token0: pool.token0.clone(),
                token1: pool.token1.clone(),
                fee: pool.fee,
                tick_spacing: pool.tick_spacing,
                sqrt_price_x96: interim[index].sqrt_price_x96.to_string(),
                tick: interim[index].tick,
                liquidity: interim[index].liquidity.to_string(),
                coverage_min_tick,
                coverage_max_tick,
                tick_bitmap_words: bitmaps_by_pool[index].clone(),
                initialized_ticks,
                state_hash: "0".repeat(64),
            };
            state.state_hash = state.canonical_hash().map_err(map_hunter_contract_error)?;
            state.validate().map_err(map_hunter_contract_error)?;
            states.push(state);
        }

        let verify = self
            .recorded_call(
                provider,
                RpcMethod::EthGetBlockByNumber,
                json!([block_quantity, false]),
                Some(block),
                0,
                slot,
                None,
                false,
            )
            .await
            .map_err(|failure| map_call_failure(failure.cause))?;
        if parse_block(&verify.value).as_ref() != Some(block) {
            return Err(GatewayError::ProviderIntegrity);
        }
        Ok(states)
    }

    async fn hunter_multicall(
        &self,
        provider: &ProviderLease,
        block: &PinnedBlock,
        slot: ProviderSlot,
        calls: &[EthCall],
    ) -> Result<Vec<Vec<u8>>, GatewayError> {
        if calls.is_empty() || calls.len() > 1_024 {
            return Err(GatewayError::InvalidRequest);
        }
        let calldata = encode_aggregate3(calls).map_err(|_| GatewayError::ProviderIntegrity)?;
        let response = self
            .recorded_call(
                provider,
                RpcMethod::EthCall,
                json!([
                    {"to": MULTICALL3_ADDRESS, "data": calldata},
                    format_quantity(block.number)
                ]),
                Some(block),
                0,
                slot,
                Some(calls.len()),
                false,
            )
            .await
            .map_err(|failure| map_call_failure(failure.cause))?;
        response
            .value
            .as_str()
            .ok_or(GatewayError::ProviderIntegrity)
            .and_then(|value| {
                decode_aggregate3(value, calls.len()).map_err(|_| GatewayError::ProviderIntegrity)
            })
    }

    async fn hunter_multicall_paced(
        &self,
        provider: &ProviderLease,
        block: &PinnedBlock,
        slot: ProviderSlot,
        calls: &[EthCall],
        pacing_deadline: Instant,
    ) -> Result<Vec<Vec<u8>>, GatewayError> {
        if calls.is_empty() || calls.len() > 1_024 {
            return Err(GatewayError::InvalidRequest);
        }
        let calldata = encode_aggregate3(calls).map_err(|_| GatewayError::ProviderIntegrity)?;
        let response = self
            .paced_recorded_call(
                provider,
                RpcMethod::EthCall,
                json!([
                    {"to": MULTICALL3_ADDRESS, "data": calldata},
                    format_quantity(block.number)
                ]),
                Some(block),
                0,
                slot,
                Some(calls.len()),
                false,
                pacing_deadline,
            )
            .await
            .map_err(|failure| map_call_failure(failure.cause))?;
        response
            .value
            .as_str()
            .ok_or(GatewayError::ProviderIntegrity)
            .and_then(|value| {
                decode_aggregate3(value, calls.len()).map_err(|_| GatewayError::ProviderIntegrity)
            })
    }

    async fn resolve_shadow_state_inner(
        &self,
        request: ShadowStateRequest,
    ) -> Result<ShadowStateResponse, GatewayError> {
        request
            .validate()
            .map_err(|_| GatewayError::InvalidRequest)?;
        let request_hash = request
            .canonical_hash()
            .map_err(|_| GatewayError::InvalidRequest)?;
        let route_config_hash = request
            .route_config_hash()
            .map_err(|_| GatewayError::InvalidRequest)?;
        self.metrics.state_request();
        if !self.request_budget.lock().await.admit(Instant::now()) {
            self.metrics.state_request_budget_rejected();
            return Err(GatewayError::RequestBudgetExhausted);
        }

        match request.evidence.clone() {
            EvidenceRequest::Primary => {
                let head = self.current_head().await?;
                self.metrics.state_freshness(head.observed_at.elapsed());
                let cache_key = route_block_key(&route_config_hash, &head.block);
                let primary = self
                    .resolve_primary(&request, &route_config_hash, &cache_key, &head)
                    .await?;
                self.metrics.primary_success();
                self.build_response(request_hash, route_config_hash, primary, None)
            }
            EvidenceRequest::Verify {
                block_number,
                block_hash,
                primary_state_hash,
            } => {
                self.metrics.secondary_verification();
                let block = PinnedBlock {
                    number: block_number,
                    hash: block_hash,
                };
                let cache_key = route_block_key(&route_config_hash, &block);
                let primary = self
                    .route_cache
                    .lock()
                    .await
                    .get(&cache_key, Instant::now())
                    .ok_or(GatewayError::ProviderUnavailable)?;
                self.metrics.route_block_cache_hit();
                if primary.block != block || primary.state_hash != primary_state_hash {
                    return Err(GatewayError::ProviderIntegrity);
                }
                let verification_key =
                    format!("{cache_key}:{}:{}", primary.provider_id, primary.state_hash);
                let verification = self
                    .resolve_verification(&request, &route_config_hash, &verification_key, &primary)
                    .await?;
                self.build_response(request_hash, route_config_hash, primary, Some(verification))
            }
        }
    }

    async fn resolve_primary(
        &self,
        request: &ShadowStateRequest,
        route_config_hash: &str,
        cache_key: &str,
        head: &HeadSnapshot,
    ) -> Result<ProviderBundle, GatewayError> {
        if let Some(bundle) = self.route_cache.lock().await.get(cache_key, Instant::now()) {
            self.metrics.route_block_cache_hit();
            return Ok(bundle);
        }
        match self.primary_role(cache_key).await? {
            BundleRole::Follower(mut receiver) => {
                self.metrics.coalesced_request();
                wait_for_watch(&mut receiver).await
            }
            BundleRole::Leader(sender) => {
                if let Some(bundle) = self.route_cache.lock().await.get(cache_key, Instant::now()) {
                    self.metrics.route_block_cache_hit();
                    let result = Ok(bundle);
                    let _ = sender.send(Some(result.clone()));
                    self.primary_in_flight.lock().await.remove(cache_key);
                    return result;
                }
                let result = tokio::time::timeout(
                    MAX_STATE_RESOLUTION,
                    self.resolve_primary_uncached(request, route_config_hash, head),
                )
                .await
                .unwrap_or(Err(GatewayError::ProviderUnavailable));
                if let Ok(bundle) = &result {
                    self.route_cache.lock().await.insert(
                        cache_key.to_string(),
                        bundle.clone(),
                        ROUTE_BLOCK_CACHE_TTL,
                        Instant::now(),
                    );
                }
                let _ = sender.send(Some(result.clone()));
                self.primary_in_flight.lock().await.remove(cache_key);
                result
            }
        }
    }

    async fn resolve_primary_uncached(
        &self,
        request: &ShadowStateRequest,
        route_config_hash: &str,
        head: &HeadSnapshot,
    ) -> Result<ProviderBundle, GatewayError> {
        let resolution = self
            .bundle_with_failover(
                request,
                route_config_hash,
                &head.block,
                ProviderSlot::Primary,
                Some(head.provider_id.as_str()),
                HashSet::new(),
            )
            .await?;
        let Some(bundle) = resolution.bundle else {
            self.readiness.set_provider_healthy(false);
            return Err(GatewayError::ProviderUnavailable);
        };
        self.readiness.set_provider_healthy(true);
        Ok(bundle)
    }

    async fn resolve_verification(
        &self,
        request: &ShadowStateRequest,
        route_config_hash: &str,
        verification_key: &str,
        primary: &ProviderBundle,
    ) -> Result<VerificationEvidence, GatewayError> {
        if let Some(evidence) = self
            .verification_cache
            .lock()
            .await
            .get(verification_key, Instant::now())
        {
            self.metrics.route_block_cache_hit();
            return Ok(evidence);
        }
        match self.verification_role(verification_key).await? {
            VerificationRole::Follower(mut receiver) => {
                self.metrics.coalesced_request();
                wait_for_watch(&mut receiver).await
            }
            VerificationRole::Leader(sender) => {
                let result = tokio::time::timeout(
                    MAX_STATE_RESOLUTION,
                    self.resolve_verification_uncached(request, route_config_hash, primary),
                )
                .await
                .unwrap_or(Err(GatewayError::ProviderUnavailable));
                if let Ok(evidence) = &result {
                    self.verification_cache.lock().await.insert(
                        verification_key.to_string(),
                        evidence.clone(),
                        ROUTE_BLOCK_CACHE_TTL,
                        Instant::now(),
                    );
                }
                let _ = sender.send(Some(result.clone()));
                self.verification_in_flight
                    .lock()
                    .await
                    .remove(verification_key);
                result
            }
        }
    }

    async fn resolve_verification_uncached(
        &self,
        request: &ShadowStateRequest,
        route_config_hash: &str,
        primary: &ProviderBundle,
    ) -> Result<VerificationEvidence, GatewayError> {
        let excluded = HashSet::from([primary.provider_id.clone()]);
        let resolution = self
            .bundle_with_failover(
                request,
                route_config_hash,
                &primary.block,
                ProviderSlot::Secondary,
                None,
                excluded,
            )
            .await?;
        let mut quality = primary.quality.clone();
        let Some(secondary) = resolution.bundle else {
            self.metrics.secondary_unavailable();
            quality.extend(resolution.failed_quality);
            return Ok(VerificationEvidence {
                agreement_provider_id: None,
                secondary_state_hash: None,
                secondary_block_number: None,
                secondary_block_hash: None,
                secondary_route_config_hash: None,
                provider_agreement: false,
                status: VerificationStatus::SecondaryUnavailable,
                independent_status: if resolution.integrity_failure_observed {
                    IndependentVerificationStatus::IntegrityFailure
                } else {
                    IndependentVerificationStatus::ProviderUnavailable
                },
                quality,
            });
        };
        quality.extend(secondary.quality.clone());
        let agreement = compare_provider_results(
            &primary.block,
            &primary.provider_result(),
            &secondary.provider_result(),
        )
        .is_ok();
        if !agreement {
            self.metrics.secondary_disagreed();
            self.metrics.provider_disagreement();
            for entry in &mut quality {
                if entry.success {
                    entry.disagreement = true;
                }
            }
        } else {
            self.metrics.secondary_agreed();
        }
        Ok(VerificationEvidence {
            agreement_provider_id: Some(secondary.provider_id),
            secondary_state_hash: Some(secondary.state_hash),
            secondary_block_number: Some(secondary.block.number),
            secondary_block_hash: Some(secondary.block.hash),
            secondary_route_config_hash: Some(route_config_hash.to_string()),
            provider_agreement: agreement,
            status: if agreement {
                VerificationStatus::Agreed
            } else {
                VerificationStatus::Disagreed
            },
            independent_status: if agreement {
                IndependentVerificationStatus::Agreed
            } else {
                IndependentVerificationStatus::Disagreed
            },
            quality,
        })
    }

    fn build_response(
        &self,
        request_hash: String,
        route_config_hash: String,
        primary: ProviderBundle,
        verification: Option<VerificationEvidence>,
    ) -> Result<ShadowStateResponse, GatewayError> {
        let (
            agreement_provider_id,
            secondary_state_hash,
            secondary_block_number,
            secondary_block_hash,
            secondary_route_config_hash,
            provider_agreement,
            verification_status,
            independent_verification_status,
            quality,
        ) = match verification {
            Some(verification) => (
                verification.agreement_provider_id,
                verification.secondary_state_hash,
                verification.secondary_block_number,
                verification.secondary_block_hash,
                verification.secondary_route_config_hash,
                verification.provider_agreement,
                verification.status,
                verification.independent_status,
                verification.quality,
            ),
            None => (
                None,
                None,
                None,
                None,
                None,
                false,
                VerificationStatus::PrimaryOnly,
                IndependentVerificationStatus::NotRequested,
                primary.quality.clone(),
            ),
        };
        let response = ShadowStateResponse {
            schema_version: SHADOW_STATE_SCHEMA_VERSION.to_string(),
            chain_id: ARBITRUM_ONE_CHAIN_ID,
            request_hash,
            route_config_hash,
            block_number: primary.block.number,
            block_hash: primary.block.hash,
            state_hash: primary.state_hash,
            pools: primary.pools,
            primary_provider_id: primary.provider_id,
            agreement_provider_id,
            secondary_state_hash,
            secondary_block_number,
            secondary_block_hash,
            secondary_route_config_hash,
            provider_agreement,
            verification_status,
            independent_verification_status,
            quality,
            resolved_at_unix_ms: unix_time_ms(),
        };
        let encoded = serde_json::to_vec(&response).map_err(|_| GatewayError::ProviderIntegrity)?;
        if encoded.len() > MAX_GATEWAY_RESPONSE_BYTES {
            return Err(GatewayError::ResponseOversized);
        }
        Ok(response)
    }

    async fn bundle_with_failover(
        &self,
        request: &ShadowStateRequest,
        route_config_hash: &str,
        block: &PinnedBlock,
        slot: ProviderSlot,
        preferred_provider: Option<&str>,
        mut excluded: HashSet<String>,
    ) -> Result<BundleResolution, GatewayError> {
        let _operation_guard = self.upstream_operation_lock.lock().await;
        let provider_count = self.provider_count().await;
        let mut failed_quality = Vec::new();
        let mut integrity_failure_observed = false;
        let mut preferred = preferred_provider.map(str::to_string);
        for retry_count in 0..provider_count {
            let provider = if let Some(provider_id) = preferred.take() {
                match self.reserve_named_provider(&provider_id).await {
                    Some(provider) => Some(provider),
                    None => self.reserve_provider(&excluded).await,
                }
            } else {
                self.reserve_provider(&excluded).await
            };
            let Some(provider) = provider else {
                break;
            };
            if !excluded.insert(provider.provider_id().to_string()) {
                continue;
            }
            let required_calls = self.provider_setup_call_count(provider.provider_id()).await + 2;
            if !self.admit_upstream_sequence(required_calls).await {
                return Err(GatewayError::UpstreamBudgetExhausted);
            }
            if let Err(failure) = self.ensure_provider_verified(&provider).await {
                if failure == CallFailure::Budget {
                    return Err(GatewayError::UpstreamBudgetExhausted);
                }
                integrity_failure_observed |= failure == CallFailure::Integrity;
                self.apply_provider_failure(provider.provider_id(), failure)
                    .await;
                continue;
            }
            match self
                .perform_state_bundle(
                    &provider,
                    request,
                    route_config_hash,
                    block,
                    slot,
                    retry_count as u16,
                )
                .await
            {
                Ok(mut bundle) => {
                    self.mark_provider_success(provider.provider_id()).await;
                    failed_quality.append(&mut bundle.quality);
                    bundle.quality = failed_quality;
                    return Ok(BundleResolution {
                        bundle: Some(bundle),
                        failed_quality: Vec::new(),
                        integrity_failure_observed,
                    });
                }
                Err(mut failure) => {
                    failed_quality.append(&mut failure.quality);
                    if failure.cause == CallFailure::Budget {
                        return Err(GatewayError::UpstreamBudgetExhausted);
                    }
                    integrity_failure_observed |= failure.cause == CallFailure::Integrity;
                    self.apply_provider_failure(provider.provider_id(), failure.cause)
                        .await;
                }
            }
        }
        Ok(BundleResolution {
            bundle: None,
            failed_quality,
            integrity_failure_observed,
        })
    }

    async fn perform_state_bundle(
        &self,
        provider: &ProviderLease,
        request: &ShadowStateRequest,
        route_config_hash: &str,
        block: &PinnedBlock,
        slot: ProviderSlot,
        retry_count: u16,
    ) -> Result<ProviderBundle, BundleFailure> {
        let static_key = format!("{}:{route_config_hash}", provider.provider_id());
        let static_cached = self
            .static_cache
            .lock()
            .await
            .get(&static_key, Instant::now())
            .is_some();
        if static_cached {
            self.metrics.static_metadata_cache_hit();
        }

        let mut calls = Vec::with_capacity(request.pools.len() * if static_cached { 2 } else { 8 });
        if !static_cached {
            for pool in &request.pools {
                for selector in [
                    TOKEN0_SELECTOR,
                    TOKEN1_SELECTOR,
                    FEE_SELECTOR,
                    TICK_SPACING_SELECTOR,
                ] {
                    calls.push(EthCall {
                        target: pool.address.clone(),
                        calldata: selector.to_string(),
                    });
                }
                for token in [&pool.token0, &pool.token1] {
                    calls.push(EthCall {
                        target: token.clone(),
                        calldata: DECIMALS_SELECTOR.to_string(),
                    });
                }
            }
        }
        for pool in &request.pools {
            for selector in [SLOT0_SELECTOR, LIQUIDITY_SELECTOR] {
                calls.push(EthCall {
                    target: pool.address.clone(),
                    calldata: selector.to_string(),
                });
            }
        }
        let calldata =
            encode_aggregate3(&calls).map_err(|_| BundleFailure::integrity(Vec::new()))?;
        let mut quality = Vec::with_capacity(2);
        let aggregate = self
            .recorded_call(
                provider,
                RpcMethod::EthCall,
                json!([
                    {"to": MULTICALL3_ADDRESS, "data": calldata},
                    format_quantity(block.number)
                ]),
                Some(block),
                retry_count,
                slot,
                Some(calls.len()),
                false,
            )
            .await
            .map_err(|failure| failure.with_prior(quality.clone()))?;
        quality.push(aggregate.quality);
        let aggregate_value = aggregate
            .value
            .as_str()
            .ok_or_else(|| BundleFailure::integrity(quality.clone()))?;
        let results = decode_aggregate3(aggregate_value, calls.len())
            .map_err(|_| BundleFailure::integrity(quality.clone()))?;
        let mut offset = 0;
        if !static_cached {
            for pool in &request.pools {
                let token0 = parse_address_bytes(&results[offset])
                    .ok_or_else(|| BundleFailure::integrity(quality.clone()))?;
                let token1 = parse_address_bytes(&results[offset + 1])
                    .ok_or_else(|| BundleFailure::integrity(quality.clone()))?;
                let fee = parse_u32_bytes(&results[offset + 2])
                    .ok_or_else(|| BundleFailure::integrity(quality.clone()))?;
                let tick_spacing = parse_i24_bytes(&results[offset + 3])
                    .ok_or_else(|| BundleFailure::integrity(quality.clone()))?;
                let token0_decimals = parse_u8_bytes(&results[offset + 4])
                    .ok_or_else(|| BundleFailure::integrity(quality.clone()))?;
                let token1_decimals = parse_u8_bytes(&results[offset + 5])
                    .ok_or_else(|| BundleFailure::integrity(quality.clone()))?;
                if token0 != pool.token0
                    || token1 != pool.token1
                    || fee != pool.fee
                    || tick_spacing != pool.tick_spacing
                    || token0_decimals != pool.token0_decimals
                    || token1_decimals != pool.token1_decimals
                {
                    return Err(BundleFailure::integrity(quality));
                }
                offset += 6;
            }
        }

        let mut pools = Vec::with_capacity(request.pools.len());
        for pool in &request.pools {
            let slot0 = normalize_state_bytes(&results[offset], 64, None)
                .ok_or_else(|| BundleFailure::integrity(quality.clone()))?;
            let liquidity = normalize_state_bytes(&results[offset + 1], 32, Some(32))
                .ok_or_else(|| BundleFailure::integrity(quality.clone()))?;
            offset += 2;
            let state_material = serde_json::to_vec(&(
                &pool.pool_id,
                &pool.address,
                &pool.protocol,
                &pool.token0,
                &pool.token1,
                pool.token0_decimals,
                pool.token1_decimals,
                pool.fee,
                pool.tick_spacing,
                &slot0,
                &liquidity,
            ))
            .map_err(|_| BundleFailure::integrity(quality.clone()))?;
            pools.push(PoolStateResponse {
                pool_id: pool.pool_id.clone(),
                address: pool.address.clone(),
                protocol: pool.protocol.clone(),
                token0: pool.token0.clone(),
                token1: pool.token1.clone(),
                token0_decimals: pool.token0_decimals,
                token1_decimals: pool.token1_decimals,
                fee: pool.fee,
                tick_spacing: pool.tick_spacing,
                slot0,
                liquidity,
                state_hash: canonical_hash_bytes(&state_material),
            });
        }
        let verify = self
            .recorded_call(
                provider,
                RpcMethod::EthGetBlockByNumber,
                json!([format_quantity(block.number), false]),
                Some(block),
                retry_count,
                slot,
                None,
                false,
            )
            .await
            .map_err(|failure| failure.with_prior(quality.clone()))?;
        quality.push(verify.quality);
        if parse_block(&verify.value).as_ref() != Some(block) {
            return Err(BundleFailure::integrity(quality));
        }
        if !static_cached {
            self.static_cache.lock().await.insert(
                static_key,
                (),
                STATIC_METADATA_CACHE_TTL,
                Instant::now(),
            );
        }
        let normalized =
            serde_json::to_vec(&pools).map_err(|_| BundleFailure::integrity(quality.clone()))?;
        Ok(ProviderBundle {
            provider_id: provider.provider_id().to_string(),
            block: block.clone(),
            pools,
            state_hash: canonical_hash_bytes(&normalized),
            quality,
        })
    }

    #[allow(clippy::too_many_arguments)]
    async fn recorded_call(
        &self,
        provider: &ProviderLease,
        method: RpcMethod,
        params: Value,
        block: Option<&PinnedBlock>,
        retry_count: u16,
        slot: ProviderSlot,
        multicall_inner: Option<usize>,
        probe: bool,
    ) -> Result<RecordedCall, BundleFailure> {
        let result = self
            .upstream_call(provider, method, params, slot, multicall_inner, probe)
            .await;
        match result {
            Ok(result) => {
                let encoded = serde_json::to_vec(&result.value)
                    .map_err(|_| BundleFailure::integrity(Vec::new()))?;
                Ok(RecordedCall {
                    value: result.value,
                    quality: RpcQualityEvidence {
                        provider_id: provider.provider_id().to_string(),
                        method: method.as_str().to_string(),
                        block_number: block.map(|value| value.number),
                        block_hash: block.map(|value| value.hash.clone()),
                        response_hash: Some(canonical_hash_bytes(&encoded)),
                        latency_ns: result.latency_ns.min(u64::MAX as u128) as u64,
                        success: true,
                        stale_result: false,
                        disagreement: false,
                        timeout: false,
                        retry_count,
                    },
                })
            }
            Err(cause) => {
                let quality = if cause == CallFailure::Budget {
                    Vec::new()
                } else {
                    vec![RpcQualityEvidence {
                        provider_id: provider.provider_id().to_string(),
                        method: method.as_str().to_string(),
                        block_number: block.map(|value| value.number),
                        block_hash: block.map(|value| value.hash.clone()),
                        response_hash: None,
                        latency_ns: 0,
                        success: false,
                        stale_result: false,
                        disagreement: false,
                        timeout: matches!(cause, CallFailure::Transport(TransportError::Timeout)),
                        retry_count,
                    }]
                };
                Err(BundleFailure { quality, cause })
            }
        }
    }

    #[allow(clippy::too_many_arguments)]
    async fn paced_recorded_call(
        &self,
        provider: &ProviderLease,
        method: RpcMethod,
        params: Value,
        block: Option<&PinnedBlock>,
        retry_count: u16,
        slot: ProviderSlot,
        multicall_inner: Option<usize>,
        probe: bool,
        pacing_deadline: Instant,
    ) -> Result<RecordedCall, BundleFailure> {
        let result = self
            .paced_upstream_call(
                provider,
                method,
                params,
                slot,
                multicall_inner,
                probe,
                pacing_deadline,
            )
            .await;
        match result {
            Ok(result) => {
                let encoded = serde_json::to_vec(&result.value)
                    .map_err(|_| BundleFailure::integrity(Vec::new()))?;
                Ok(RecordedCall {
                    value: result.value,
                    quality: RpcQualityEvidence {
                        provider_id: provider.provider_id().to_string(),
                        method: method.as_str().to_string(),
                        block_number: block.map(|value| value.number),
                        block_hash: block.map(|value| value.hash.clone()),
                        response_hash: Some(canonical_hash_bytes(&encoded)),
                        latency_ns: result.latency_ns.min(u64::MAX as u128) as u64,
                        success: true,
                        stale_result: false,
                        disagreement: false,
                        timeout: false,
                        retry_count,
                    },
                })
            }
            Err(cause) => {
                let quality = if cause == CallFailure::Budget {
                    Vec::new()
                } else {
                    vec![RpcQualityEvidence {
                        provider_id: provider.provider_id().to_string(),
                        method: method.as_str().to_string(),
                        block_number: block.map(|value| value.number),
                        block_hash: block.map(|value| value.hash.clone()),
                        response_hash: None,
                        latency_ns: 0,
                        success: false,
                        stale_result: false,
                        disagreement: false,
                        timeout: matches!(cause, CallFailure::Transport(TransportError::Timeout)),
                        retry_count,
                    }]
                };
                Err(BundleFailure { quality, cause })
            }
        }
    }

    async fn upstream_call(
        &self,
        provider: &ProviderLease,
        method: RpcMethod,
        params: Value,
        slot: ProviderSlot,
        multicall_inner: Option<usize>,
        probe: bool,
    ) -> Result<RpcCallResult, CallFailure> {
        if !self.upstream_budget.lock().await.admit(Instant::now()) {
            self.metrics.upstream_call_budget_rejected();
            return Err(CallFailure::Budget);
        }
        self.call_upstream_transport(provider, method, params, slot, multicall_inner, probe)
            .await
    }

    #[allow(clippy::too_many_arguments)]
    async fn paced_upstream_call(
        &self,
        provider: &ProviderLease,
        method: RpcMethod,
        params: Value,
        slot: ProviderSlot,
        multicall_inner: Option<usize>,
        probe: bool,
        pacing_deadline: Instant,
    ) -> Result<RpcCallResult, CallFailure> {
        loop {
            let now = Instant::now();
            if self.upstream_budget.lock().await.admit(now) {
                break;
            }
            if now >= pacing_deadline {
                self.metrics.upstream_call_budget_rejected();
                return Err(CallFailure::Budget);
            }
            tokio::time::sleep(
                self.upstream_refill_interval
                    .min(pacing_deadline.saturating_duration_since(now)),
            )
            .await;
        }
        self.call_upstream_transport(provider, method, params, slot, multicall_inner, probe)
            .await
    }

    async fn call_upstream_transport(
        &self,
        provider: &ProviderLease,
        method: RpcMethod,
        params: Value,
        slot: ProviderSlot,
        multicall_inner: Option<usize>,
        probe: bool,
    ) -> Result<RpcCallResult, CallFailure> {
        if probe {
            self.metrics.probe_call();
        }
        if let Some(inner_calls) = multicall_inner {
            self.metrics.multicall_request(inner_calls);
        }
        let started = Instant::now();
        let result = self
            .client
            .call(provider, method, params, self.timeouts.timeout_for(method))
            .await;
        let outcome = classify_upstream_outcome(&result);
        self.metrics.upstream_call(method, outcome, slot);
        let latency = result
            .as_ref()
            .map(|value| Duration::from_nanos(value.latency_ns.min(u64::MAX as u128) as u64))
            .unwrap_or_else(|_| started.elapsed());
        self.metrics
            .upstream_call_latency(method, outcome, slot, latency);
        result.map_err(CallFailure::Transport)
    }

    async fn ensure_provider_verified(&self, provider: &ProviderLease) -> Result<(), CallFailure> {
        let provider_id = provider.provider_id().to_string();
        let verification_lock = {
            let mut locks = self.provider_verification_locks.lock().await;
            locks
                .entry(provider_id.clone())
                .or_insert_with(|| Arc::new(Mutex::new(())))
                .clone()
        };
        let _guard = verification_lock.lock().await;
        if !self.chain_verified.lock().await.contains(&provider_id) {
            let chain_id = self
                .upstream_call(
                    provider,
                    RpcMethod::EthChainId,
                    json!([]),
                    ProviderSlot::Probe,
                    None,
                    true,
                )
                .await?;
            if chain_id.value.as_str() != Some(ARBITRUM_CHAIN_ID_HEX) {
                return Err(CallFailure::Integrity);
            }
            self.chain_verified.lock().await.insert(provider_id.clone());
        }
        if !self.multicall_verified.lock().await.contains(&provider_id) {
            let code = self
                .upstream_call(
                    provider,
                    RpcMethod::EthGetCode,
                    json!([MULTICALL3_ADDRESS, "latest"]),
                    ProviderSlot::Probe,
                    None,
                    true,
                )
                .await?;
            let Some(code) = code.value.as_str().map(str::to_ascii_lowercase) else {
                return Err(CallFailure::Integrity);
            };
            if code == "0x"
                || !canonical_data(&code, MAX_MULTICALL_CODE_BYTES)
                || code[2..].bytes().all(|byte| byte == b'0')
            {
                return Err(CallFailure::Integrity);
            }
            self.multicall_verified.lock().await.insert(provider_id);
        }
        Ok(())
    }

    async fn ensure_provider_verified_paced(
        &self,
        provider: &ProviderLease,
        pacing_deadline: Instant,
    ) -> Result<(), CallFailure> {
        let provider_id = provider.provider_id().to_string();
        let verification_lock = {
            let mut locks = self.provider_verification_locks.lock().await;
            locks
                .entry(provider_id.clone())
                .or_insert_with(|| Arc::new(Mutex::new(())))
                .clone()
        };
        let _guard = verification_lock.lock().await;
        if !self.chain_verified.lock().await.contains(&provider_id) {
            let chain_id = self
                .paced_upstream_call(
                    provider,
                    RpcMethod::EthChainId,
                    json!([]),
                    ProviderSlot::Probe,
                    None,
                    true,
                    pacing_deadline,
                )
                .await?;
            if chain_id.value.as_str() != Some(ARBITRUM_CHAIN_ID_HEX) {
                return Err(CallFailure::Integrity);
            }
            self.chain_verified.lock().await.insert(provider_id.clone());
        }
        if !self.multicall_verified.lock().await.contains(&provider_id) {
            let code = self
                .paced_upstream_call(
                    provider,
                    RpcMethod::EthGetCode,
                    json!([MULTICALL3_ADDRESS, "latest"]),
                    ProviderSlot::Probe,
                    None,
                    true,
                    pacing_deadline,
                )
                .await?;
            let Some(code) = code.value.as_str().map(str::to_ascii_lowercase) else {
                return Err(CallFailure::Integrity);
            };
            if code == "0x"
                || !canonical_data(&code, MAX_MULTICALL_CODE_BYTES)
                || code[2..].bytes().all(|byte| byte == b'0')
            {
                return Err(CallFailure::Integrity);
            }
            self.multicall_verified.lock().await.insert(provider_id);
        }
        Ok(())
    }

    async fn current_head(&self) -> Result<HeadSnapshot, GatewayError> {
        if let Some(head) = self.head.lock().await.clone() {
            if head.observed_at.elapsed() <= HEAD_MAX_AGE {
                return Ok(head);
            }
        }
        self.refresh_head_shared(false).await
    }

    async fn refresh_head_shared(&self, force: bool) -> Result<HeadSnapshot, GatewayError> {
        if !force {
            if let Some(head) = self.head.lock().await.clone() {
                if head.observed_at.elapsed() <= HEAD_MAX_AGE {
                    return Ok(head);
                }
            }
        }
        let role = {
            let mut in_flight = self.head_in_flight.lock().await;
            if let Some(receiver) = in_flight.as_ref() {
                HeadRole::Follower(receiver.clone())
            } else {
                let (sender, receiver) = watch::channel(None);
                *in_flight = Some(receiver);
                HeadRole::Leader(sender)
            }
        };
        match role {
            HeadRole::Follower(mut receiver) => {
                self.metrics.coalesced_request();
                wait_for_watch(&mut receiver).await
            }
            HeadRole::Leader(sender) => {
                let result = self.refresh_head_uncached().await;
                let _ = sender.send(Some(result.clone()));
                *self.head_in_flight.lock().await = None;
                result
            }
        }
    }

    async fn refresh_head_uncached(&self) -> Result<HeadSnapshot, GatewayError> {
        let _operation_guard = self.upstream_operation_lock.lock().await;
        let provider_count = self.provider_count().await;
        let mut excluded = HashSet::with_capacity(provider_count);
        for _ in 0..provider_count {
            let Some(provider) = self.reserve_provider(&excluded).await else {
                break;
            };
            excluded.insert(provider.provider_id().to_string());
            let required_calls = self.provider_setup_call_count(provider.provider_id()).await + 1;
            if !self.admit_upstream_sequence(required_calls).await {
                return Err(GatewayError::UpstreamBudgetExhausted);
            }
            if let Err(failure) = self.ensure_provider_verified(&provider).await {
                if failure == CallFailure::Budget {
                    return Err(GatewayError::UpstreamBudgetExhausted);
                }
                self.apply_provider_failure(provider.provider_id(), failure)
                    .await;
                continue;
            }
            let result = self
                .upstream_call(
                    &provider,
                    RpcMethod::EthGetBlockByNumber,
                    json!(["latest", false]),
                    ProviderSlot::Probe,
                    None,
                    true,
                )
                .await;
            match result {
                Ok(result) => {
                    let Some(block) = parse_block(&result.value) else {
                        self.apply_provider_failure(provider.provider_id(), CallFailure::Integrity)
                            .await;
                        continue;
                    };
                    let snapshot = HeadSnapshot {
                        provider_id: provider.provider_id().to_string(),
                        block,
                        observed_at: Instant::now(),
                    };
                    self.update_head(snapshot.clone()).await;
                    self.mark_provider_success(provider.provider_id()).await;
                    self.readiness.set_provider_healthy(true);
                    return Ok(snapshot);
                }
                Err(CallFailure::Budget) => {
                    return Err(GatewayError::UpstreamBudgetExhausted);
                }
                Err(failure) => {
                    self.apply_provider_failure(provider.provider_id(), failure)
                        .await;
                }
            }
        }
        self.readiness.set_provider_healthy(false);
        Err(GatewayError::ProviderUnavailable)
    }

    async fn provider_setup_call_count(&self, provider_id: &str) -> u32 {
        let chain = u32::from(!self.chain_verified.lock().await.contains(provider_id));
        let multicall = u32::from(!self.multicall_verified.lock().await.contains(provider_id));
        chain + multicall
    }

    async fn admit_upstream_sequence(&self, required_calls: u32) -> bool {
        if self.upstream_budget.lock().await.available(Instant::now()) >= required_calls {
            return true;
        }
        self.metrics.upstream_call_budget_rejected();
        false
    }

    async fn update_head(&self, snapshot: HeadSnapshot) {
        let changed_identity = self.head.lock().await.as_ref().is_some_and(|current| {
            current.block.number == snapshot.block.number
                && current.block.hash != snapshot.block.hash
        });
        if changed_identity {
            let block = snapshot.block.clone();
            self.route_cache.lock().await.retain(|_, bundle| {
                bundle.block.number != block.number || bundle.block.hash == block.hash
            });
            self.verification_cache.lock().await.retain(|_, _| false);
        }
        *self.head.lock().await = Some(snapshot);
    }

    async fn apply_provider_failure(&self, provider_id: &str, failure: CallFailure) {
        match failure {
            CallFailure::Transport(TransportError::RateLimited { retry_after }) => {
                self.metrics.provider_rate_limited();
                self.metrics.provider_cooldown();
                let _ = self.providers.lock().await.record_cooldown(
                    provider_id,
                    Instant::now(),
                    retry_after,
                );
            }
            CallFailure::Budget => {}
            CallFailure::Transport(_) | CallFailure::Integrity => {
                self.mark_provider_failure(provider_id).await;
            }
        }
    }

    async fn primary_role(&self, key: &str) -> Result<BundleRole, GatewayError> {
        let mut in_flight = self.primary_in_flight.lock().await;
        if let Some(receiver) = in_flight.get(key) {
            return Ok(BundleRole::Follower(receiver.clone()));
        }
        if in_flight.len() >= MAX_IN_FLIGHT_REQUESTS {
            self.metrics.state_request_budget_rejected();
            return Err(GatewayError::RequestBudgetExhausted);
        }
        let (sender, receiver) = watch::channel(None);
        in_flight.insert(key.to_string(), receiver);
        Ok(BundleRole::Leader(sender))
    }

    async fn verification_role(&self, key: &str) -> Result<VerificationRole, GatewayError> {
        let mut in_flight = self.verification_in_flight.lock().await;
        if let Some(receiver) = in_flight.get(key) {
            return Ok(VerificationRole::Follower(receiver.clone()));
        }
        if in_flight.len() >= MAX_IN_FLIGHT_REQUESTS {
            self.metrics.state_request_budget_rejected();
            return Err(GatewayError::RequestBudgetExhausted);
        }
        let (sender, receiver) = watch::channel(None);
        in_flight.insert(key.to_string(), receiver);
        Ok(VerificationRole::Leader(sender))
    }

    async fn provider_count(&self) -> usize {
        self.providers.lock().await.len()
    }

    async fn reserve_provider(&self, excluded: &HashSet<String>) -> Option<ProviderLease> {
        self.providers
            .lock()
            .await
            .reserve_best(Instant::now(), excluded)
    }

    async fn reserve_named_provider(&self, provider_id: &str) -> Option<ProviderLease> {
        self.providers
            .lock()
            .await
            .reserve_named(Instant::now(), provider_id)
    }

    async fn mark_provider_success(&self, provider_id: &str) {
        let _ = self.providers.lock().await.record_success(provider_id);
    }

    async fn mark_provider_failure(&self, provider_id: &str) {
        let _ = self
            .providers
            .lock()
            .await
            .record_failure(provider_id, Instant::now());
    }
}

fn classify_upstream_outcome(result: &Result<RpcCallResult, TransportError>) -> UpstreamOutcome {
    match result {
        Ok(_) => UpstreamOutcome::Success,
        Err(TransportError::ExecutionReverted { .. }) => UpstreamOutcome::Reverted,
        Err(TransportError::Timeout) => UpstreamOutcome::Timeout,
        Err(TransportError::RateLimited { .. }) => UpstreamOutcome::RateLimited,
        Err(_) => UpstreamOutcome::Failure,
    }
}

#[derive(Clone, Debug)]
struct HeadSnapshot {
    provider_id: String,
    block: PinnedBlock,
    observed_at: Instant,
}

#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord)]
struct NormalizedAaveTailLog {
    block_number: u64,
    transaction_index: u64,
    log_index: u64,
    block_hash: String,
    transaction_hash: String,
    topic: String,
    data_hash: String,
    borrower: String,
}

#[derive(Clone, Debug)]
struct HunterPoolInterim {
    sqrt_price_x96: U256,
    tick: i32,
    liquidity: u128,
    word_positions: Vec<i16>,
}

enum HeadRole {
    Leader(watch::Sender<SharedHeadResult>),
    Follower(watch::Receiver<SharedHeadResult>),
}

enum BundleRole {
    Leader(watch::Sender<SharedBundleResult>),
    Follower(watch::Receiver<SharedBundleResult>),
}

enum VerificationRole {
    Leader(watch::Sender<SharedVerificationResult>),
    Follower(watch::Receiver<SharedVerificationResult>),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum CallFailure {
    Budget,
    Transport(TransportError),
    Integrity,
}

#[derive(Clone, Debug)]
struct RecordedCall {
    value: Value,
    quality: RpcQualityEvidence,
}

#[derive(Clone, Debug)]
struct ProviderBundle {
    provider_id: String,
    block: PinnedBlock,
    pools: Vec<PoolStateResponse>,
    state_hash: String,
    quality: Vec<RpcQualityEvidence>,
}

impl ProviderBundle {
    fn provider_result(&self) -> ProviderResult {
        ProviderResult {
            provider_id: self.provider_id.clone(),
            block: self.block.clone(),
            normalized_response_hash: self.state_hash.clone(),
            latency_ns: self
                .quality
                .iter()
                .map(|entry| u128::from(entry.latency_ns))
                .sum(),
            retry_count: self
                .quality
                .iter()
                .map(|entry| entry.retry_count)
                .max()
                .unwrap_or(0),
        }
    }
}

#[derive(Clone, Debug)]
struct VerificationEvidence {
    agreement_provider_id: Option<String>,
    secondary_state_hash: Option<String>,
    secondary_block_number: Option<u64>,
    secondary_block_hash: Option<String>,
    secondary_route_config_hash: Option<String>,
    provider_agreement: bool,
    status: VerificationStatus,
    independent_status: IndependentVerificationStatus,
    quality: Vec<RpcQualityEvidence>,
}

#[derive(Clone, Debug)]
struct BundleResolution {
    bundle: Option<ProviderBundle>,
    failed_quality: Vec<RpcQualityEvidence>,
    integrity_failure_observed: bool,
}

#[derive(Clone, Debug)]
struct BundleFailure {
    quality: Vec<RpcQualityEvidence>,
    cause: CallFailure,
}

impl BundleFailure {
    fn integrity(quality: Vec<RpcQualityEvidence>) -> Self {
        Self {
            quality,
            cause: CallFailure::Integrity,
        }
    }

    fn with_prior(mut self, mut prior: Vec<RpcQualityEvidence>) -> Self {
        prior.append(&mut self.quality);
        self.quality = prior;
        self
    }
}

async fn wait_for_watch<T: Clone>(
    receiver: &mut watch::Receiver<Option<Result<T, GatewayError>>>,
) -> Result<T, GatewayError> {
    tokio::time::timeout(MAX_COALESCE_WAIT, async {
        loop {
            if let Some(result) = receiver.borrow().clone() {
                return result;
            }
            receiver
                .changed()
                .await
                .map_err(|_| GatewayError::ProviderUnavailable)?;
        }
    })
    .await
    .map_err(|_| GatewayError::ProviderUnavailable)?
}

fn route_block_key(route_config_hash: &str, block: &PinnedBlock) -> String {
    format!("{route_config_hash}:{}:{}", block.number, block.hash)
}

fn aave_exact_static_context_key(
    provider_id: &str,
    block: &PinnedBlock,
    state_root: &str,
) -> String {
    format!(
        "aave-exact:{provider_id}:{}:{}:{state_root}:{AAVE_V3_POOL_ARBITRUM}:{ARBITRUM_WETH}:{ARBITRUM_NATIVE_USDC}:{ARBITRUM_USDC_E}",
        block.number, block.hash
    )
}

fn parse_block(value: &Value) -> Option<PinnedBlock> {
    let number = value.get("number")?.as_str()?;
    let hash = value.get("hash")?.as_str()?.to_ascii_lowercase();
    if !canonical_quantity(number) || !canonical_block_hash(&hash) {
        return None;
    }
    Some(PinnedBlock {
        number: u64::from_str_radix(number.strip_prefix("0x")?, 16).ok()?,
        hash,
    })
}

fn parse_block_state_root(value: &Value) -> Option<String> {
    value
        .get("stateRoot")?
        .as_str()
        .map(str::to_ascii_lowercase)
        .filter(|root| canonical_block_hash(root))
}

fn normalize_state_bytes(
    value: &[u8],
    minimum_bytes: usize,
    exact_bytes: Option<usize>,
) -> Option<String> {
    if value.len() < minimum_bytes
        || value.len() > MAX_STATE_RESPONSE_DATA_BYTES
        || exact_bytes.is_some_and(|expected| value.len() != expected)
    {
        return None;
    }
    Some(format!("0x{}", hex::encode(value)))
}

fn parse_address_bytes(value: &[u8]) -> Option<String> {
    if value.len() != 32 || value[..12].iter().any(|byte| *byte != 0) {
        return None;
    }
    Some(format!("0x{}", hex::encode(&value[12..])))
}

fn encode_one_address(selector: &str, address: &str) -> String {
    format!("{}{}{}", selector, "0".repeat(24), &address[2..])
}

fn encode_two_addresses(selector: &str, first: &str, second: &str) -> String {
    format!(
        "{}{}{}{}{}",
        selector,
        "0".repeat(24),
        &first[2..],
        "0".repeat(24),
        &second[2..]
    )
}

fn encode_one_u256(selector: &str, value: U256) -> String {
    let mut word = [0_u8; 32];
    value.to_big_endian(&mut word);
    format!("{}{}", selector, hex::encode(word))
}

fn decode_address_array(value: &[u8]) -> Option<Vec<String>> {
    if value.len() < 64
        || value.len() % 32 != 0
        || U256::from_big_endian(&value[..32]) != U256::from(32)
    {
        return None;
    }
    let length = usize::try_from(U256::from_big_endian(&value[32..64])).ok()?;
    if length == 0 || value.len() != 64_usize.checked_add(length.checked_mul(32)?)? {
        return None;
    }
    value[64..]
        .chunks_exact(32)
        .map(parse_address_bytes)
        .collect()
}

fn build_aave_simulation_response(
    request: &AaveSimulateRequest,
    route_id: &str,
    calldata: &[u8],
    primary_provider_id: &str,
    evidence: AaveSimulationEvidence,
) -> Result<AaveSimulateResponse, GatewayError> {
    let maximum_fee = decimal_u256(&request.max_fee_per_gas)?;
    let priority_fee = decimal_u256(&request.max_priority_fee_per_gas)?;
    let (
        estimated_gas_limit,
        estimated_max_fee_per_gas,
        estimated_execution_cost,
        estimated_l1_cost,
    ) = bounded_aave_gas_quote(
        evidence.total_gas,
        evidence.l1_gas,
        evidence.base_fee_per_gas,
        maximum_fee,
        priority_fee,
        request.gas_limit,
    )?;
    let flash_premium_debt_asset = percent_mul(
        decimal_u256(&request.repay_amount)?,
        U256::from(evidence.flash_premium_bps),
    )?;
    if evidence.realized_profit < decimal_u256(&request.minimum_profit)? {
        return Err(GatewayError::StateIncomplete);
    }
    let debt_price = decimal_u256(&request.debt_asset_price_base)?;
    let weth_price = decimal_u256(&request.weth_price_base)?;
    let debt_unit = asset_unit(request.debt_asset_decimals)?;
    let realized_profit =
        debt_to_weth_floor(evidence.realized_profit, debt_price, weth_price, debt_unit)?;
    let flash_premium =
        debt_to_weth_floor(flash_premium_debt_asset, debt_price, weth_price, debt_unit)?;
    let conservative_net = aave_conservative_simulation_net(
        realized_profit,
        estimated_execution_cost,
        decimal_u256(&request.atlas_bid)?,
    )?;
    let calldata_hex = format!("0x{}", hex::encode(calldata));
    let calldata_hash = canonical_hash_bytes(calldata);
    let simulation_result_hash = canonical_hash_bytes(
        &serde_json::to_vec(&json!({
            "request": request,
            "route_id": route_id,
            "calldata_hash": calldata_hash,
            "realized_profit": realized_profit.to_string(),
            "realized_profit_debt_asset": evidence.realized_profit.to_string(),
            "conservative_net_pnl": conservative_net.to_string(),
            "estimated_gas_limit": estimated_gas_limit,
            "estimated_max_fee_per_gas_wei": estimated_max_fee_per_gas.to_string(),
            "estimated_execution_cost_wei": estimated_execution_cost.to_string(),
            "estimated_l1_cost_wei": estimated_l1_cost.to_string(),
            "flash_premium_wei": flash_premium.to_string(),
            "flash_premium_debt_asset": flash_premium_debt_asset.to_string(),
            "atlas_mode": request.atlas_mode,
            "atlas_bid": request.atlas_bid,
            "provider": primary_provider_id
        }))
        .map_err(|_| GatewayError::ProviderIntegrity)?,
    );
    let response = AaveSimulateResponse {
        schema_version: AAVE_SIMULATE_RESPONSE_SCHEMA.to_string(),
        chain_id: ARBITRUM_ONE_CHAIN_ID,
        request_id: request.request_id.clone(),
        block_number: request.block_number,
        block_hash: request.block_hash.clone(),
        state_root: request.state_root.clone(),
        primary_provider_id: primary_provider_id.to_string(),
        confirmation_provider_id: None,
        quorum: 1,
        evidence_mode: if request.atlas_mode {
            SINGLE_PRIMARY_ATLAS_CALLBACK_FORK_EVIDENCE.to_string()
        } else if request.counterfactual {
            SINGLE_PRIMARY_COUNTERFACTUAL_FORK_EVIDENCE.to_string()
        } else {
            SINGLE_PRIMARY_FORK_EVIDENCE.to_string()
        },
        route_id: route_id.to_string(),
        calldata_hex,
        calldata_hash,
        simulation_result_hash,
        realized_profit: realized_profit.to_string(),
        realized_profit_debt_asset: evidence.realized_profit.to_string(),
        conservative_net_pnl: conservative_net.to_string(),
        estimated_gas_limit,
        estimated_max_fee_per_gas_wei: estimated_max_fee_per_gas.to_string(),
        estimated_execution_cost_wei: estimated_execution_cost.to_string(),
        estimated_l1_cost_wei: estimated_l1_cost.to_string(),
        flash_premium_wei: flash_premium.to_string(),
        flash_premium_debt_asset: flash_premium_debt_asset.to_string(),
        deadline_unix_seconds: request.deadline_unix_seconds,
        resolved_at_unix_ms: unix_time_ms(),
    };
    response
        .validate(request)
        .map_err(|_| GatewayError::ProviderDisagreement)?;
    Ok(response)
}

fn validate_aave_simulation_identity(request: &AaveSimulateRequest) -> Result<(), GatewayError> {
    let route_valid =
        if request.debt_asset == ARBITRUM_WETH && request.collateral_asset == ARBITRUM_WETH {
            request.selected_pool == ZERO_ADDRESS
                && request.selected_factory == ZERO_ADDRESS
                && request.selected_fee == 0
                && !request.zero_for_one
        } else if (request.debt_asset == ARBITRUM_WETH
            && request.collateral_asset == ARBITRUM_NATIVE_USDC)
            || (request.debt_asset == ARBITRUM_USDC_E && request.collateral_asset == ARBITRUM_WETH)
        {
            reviewed_aave_unwind_routes()?.iter().any(|route| {
                request.selected_factory == route.factory
                    && request.selected_pool == route.pool
                    && request.selected_fee == route.fee
                    && request.zero_for_one == route.zero_for_one
                    && ((route.token0 == request.collateral_asset
                        && route.token1 == request.debt_asset)
                        || (route.token1 == request.collateral_asset
                            && route.token0 == request.debt_asset))
            })
        } else {
            false
        };
    let atlas_bid = decimal_u256(&request.atlas_bid)?;
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_err(|_| GatewayError::InvalidRequest)?
        .as_secs();
    if !matches!(request.debt_asset.as_str(), ARBITRUM_WETH | ARBITRUM_USDC_E)
        || !route_valid
        || request.atlas_mode == atlas_bid.is_zero()
        || request.deadline_unix_seconds <= now
        || request.deadline_unix_seconds > now.saturating_add(120)
    {
        return Err(GatewayError::InvalidRequest);
    }
    Ok(())
}

fn aave_simulation_route_id(request: &AaveSimulateRequest) -> Result<String, GatewayError> {
    let encoded = serde_json::to_vec(&json!({
        "schema": "phoenix.aave-liquidation-route-identity.v1",
        "block_number": request.block_number,
        "block_hash": request.block_hash,
        "state_root": request.state_root,
        "executor_address": request.executor_address,
        "executor_code_hash": request.executor_code_hash,
        "borrower": request.borrower,
        "debt_asset": request.debt_asset,
        "collateral_asset": request.collateral_asset,
        "debt_asset_decimals": request.debt_asset_decimals,
        "debt_asset_price_base": request.debt_asset_price_base,
        "weth_price_base": request.weth_price_base,
        "repay_amount": request.repay_amount,
        "maximum_input_amount": request.maximum_input_amount,
        "live_maximum_input_amount": request.live_maximum_input_amount,
        "maximum_input_weth_wei": request.maximum_input_weth_wei,
        "live_maximum_input_weth_wei": request.live_maximum_input_weth_wei,
        "counterfactual": request.counterfactual,
        "minimum_collateral_received": request.minimum_collateral_received,
        "minimum_unwind_output": request.minimum_unwind_output,
        "minimum_profit": request.minimum_profit,
        "minimum_profit_weth_wei": request.minimum_profit_weth_wei,
        "selected_pool": request.selected_pool,
        "selected_factory": request.selected_factory,
        "selected_fee": request.selected_fee,
        "zero_for_one": request.zero_for_one,
        "deadline": request.deadline_unix_seconds,
        "atlas_mode": request.atlas_mode,
        "atlas_bid": request.atlas_bid,
        "release_sha": request.release_sha
    }))
    .map_err(|_| GatewayError::ProviderIntegrity)?;
    Ok(format!("0x{}", canonical_hash_bytes(&encoded)))
}

fn encode_aave_liquidation_call(
    request: &AaveSimulateRequest,
    route_id: &str,
) -> Result<Vec<u8>, GatewayError> {
    let address = |value: &str| -> Result<ethabi::Address, GatewayError> {
        let bytes = hex::decode(&value[2..]).map_err(|_| GatewayError::InvalidRequest)?;
        Ok(ethabi::Address::from_slice(&bytes))
    };
    let uint = |value: &str| decimal_u256(value);
    let route = hex::decode(&route_id[2..]).map_err(|_| GatewayError::ProviderIntegrity)?;
    let legs = if request.collateral_asset == request.debt_asset {
        Vec::new()
    } else {
        vec![Token::Tuple(vec![
            Token::Address(address(&request.selected_pool)?),
            Token::Address(address(&request.collateral_asset)?),
            Token::Address(address(&request.debt_asset)?),
            Token::Uint(U256::from(request.selected_fee)),
            Token::Bool(request.zero_for_one),
            Token::Uint(uint(&request.minimum_unwind_output)?),
        ])]
    };
    let liquidation = Token::Tuple(vec![
        Token::FixedBytes(route),
        Token::Address(address(&request.borrower)?),
        Token::Address(address(&request.debt_asset)?),
        Token::Address(address(&request.collateral_asset)?),
        Token::Uint(uint(&request.repay_amount)?),
        Token::Bool(false),
        Token::Uint(uint(&request.maximum_input_amount)?),
        Token::Uint(uint(&request.minimum_collateral_received)?),
        Token::Uint(uint(&request.minimum_unwind_output)?),
        Token::Uint(uint(&request.minimum_profit)?),
        Token::Uint(uint(&request.atlas_bid)?),
        Token::Uint(U256::from(request.deadline_unix_seconds)),
        Token::Array(legs),
    ]);
    let request_type = ParamType::Tuple(vec![
        ParamType::FixedBytes(32),
        ParamType::Address,
        ParamType::Address,
        ParamType::Address,
        ParamType::Uint(256),
        ParamType::Bool,
        ParamType::Uint(256),
        ParamType::Uint(256),
        ParamType::Uint(256),
        ParamType::Uint(256),
        ParamType::Uint(256),
        ParamType::Uint(256),
        ParamType::Array(Box::new(ParamType::Tuple(vec![
            ParamType::Address,
            ParamType::Address,
            ParamType::Address,
            ParamType::Uint(24),
            ParamType::Bool,
            ParamType::Uint(256),
        ]))),
    ]);
    let mut encoded = ethabi::short_signature("executeAaveLiquidation", &[request_type]).to_vec();
    encoded.extend_from_slice(&ethabi::encode(&[liquidation]));
    Ok(encoded)
}

fn encode_gas_estimate_components(
    executor_address: &str,
    calldata: &[u8],
) -> Result<String, GatewayError> {
    let executor = hex::decode(
        executor_address
            .strip_prefix("0x")
            .ok_or(GatewayError::InvalidRequest)?,
    )
    .ok()
    .filter(|value| value.len() == 20)
    .ok_or(GatewayError::InvalidRequest)?;
    let mut encoded = hex::decode(&GAS_ESTIMATE_COMPONENTS_SELECTOR[2..])
        .map_err(|_| GatewayError::ProviderIntegrity)?;
    encoded.extend(ethabi::encode(&[
        Token::Address(ethabi::Address::from_slice(&executor)),
        Token::Bool(false),
        Token::Bytes(calldata.to_vec()),
    ]));
    Ok(format!("0x{}", hex::encode(encoded)))
}

fn decode_gas_estimate_components(value: &str) -> Option<(u64, u64, U256)> {
    let bytes = hex::decode(value.strip_prefix("0x")?).ok()?;
    let decoded = ethabi::decode(
        &[
            ParamType::Uint(64),
            ParamType::Uint(64),
            ParamType::Uint(256),
            ParamType::Uint(256),
        ],
        &bytes,
    )
    .ok()?;
    let total_gas = decoded.first()?.clone().into_uint()?.try_into().ok()?;
    let l1_gas = decoded.get(1)?.clone().into_uint()?.try_into().ok()?;
    let base_fee_per_gas = decoded.get(2)?.clone().into_uint()?;
    let l1_base_fee_estimate = decoded.get(3)?.clone().into_uint()?;
    if base_fee_per_gas.is_zero() || l1_base_fee_estimate.is_zero() {
        return None;
    }
    Some((total_gas, l1_gas, base_fee_per_gas))
}

fn bounded_aave_gas_quote(
    total_gas: u64,
    l1_gas: u64,
    base_fee_per_gas: U256,
    maximum_fee_per_gas: U256,
    priority_fee_per_gas: U256,
    maximum_gas_limit: u64,
) -> Result<(u64, U256, U256, U256), GatewayError> {
    if total_gas == 0 || total_gas < l1_gas || base_fee_per_gas.is_zero() {
        return Err(GatewayError::ProviderIntegrity);
    }
    let estimated_gas_limit = u64::try_from(percent_mul_ceil(
        U256::from(total_gas),
        U256::from(GAS_ESTIMATE_HEADROOM_BPS),
    )?)
    .map_err(|_| GatewayError::ProviderIntegrity)?;
    if estimated_gas_limit > maximum_gas_limit {
        return Err(GatewayError::StateIncomplete);
    }
    let quoted_fee = checked_mul(base_fee_per_gas, U256::from(2))?
        .checked_add(priority_fee_per_gas)
        .ok_or(GatewayError::ProviderIntegrity)?;
    if quoted_fee > maximum_fee_per_gas {
        return Err(GatewayError::StateIncomplete);
    }
    if quoted_fee.is_zero() {
        return Err(GatewayError::ProviderIntegrity);
    }
    Ok((
        estimated_gas_limit,
        quoted_fee,
        checked_mul(U256::from(estimated_gas_limit), quoted_fee)?,
        checked_mul(U256::from(l1_gas), quoted_fee)?,
    ))
}

fn aave_conservative_simulation_net(
    realized_profit: U256,
    estimated_execution_cost: U256,
    atlas_bid: U256,
) -> Result<U256, GatewayError> {
    realized_profit
        .checked_sub(estimated_execution_cost)
        .and_then(|value| value.checked_sub(atlas_bid))
        .ok_or(GatewayError::StateIncomplete)
}

fn decode_executor_packed_config(value: &str) -> Option<String> {
    if value.len() != 66 || !value.starts_with("0x") {
        return None;
    }
    let mut bytes = hex::decode(&value[2..]).ok()?;
    if bytes.len() != 32
        || bytes[..10].iter().any(|byte| *byte != 0)
        || bytes[10] != 0
        || bytes[11] > 1
        || format!("0x{}", hex::encode(&bytes[12..])) != AAVE_V3_POOL_ARBITRUM
    {
        return None;
    }
    bytes[11] = 0;
    Some(format!("0x{}", hex::encode(bytes)))
}

fn aave_executor_simulation_state_diff(
    request: &AaveSimulateRequest,
    packed_executor_config: &str,
    onchain_maximum_input: U256,
) -> Result<Value, GatewayError> {
    let live_maximum_input = decimal_u256(&request.live_maximum_input_amount)?;
    let reviewed_maximum_input = U256::from(MAXIMUM_REVIEWED_INPUT_WEI);
    if onchain_maximum_input.is_zero()
        || onchain_maximum_input > reviewed_maximum_input
        || live_maximum_input > onchain_maximum_input
    {
        return Err(GatewayError::ProviderIntegrity);
    }
    let mut state_diff = serde_json::Map::new();
    state_diff.insert(
        PHOENIX_EXECUTOR_PACKED_CONFIG_SLOT.to_string(),
        Value::String(packed_executor_config.to_string()),
    );
    if request.counterfactual {
        state_diff.insert(
            PHOENIX_EXECUTOR_MAXIMUM_INPUT_SLOT.to_string(),
            Value::String(u256_storage_word(reviewed_maximum_input)),
        );
    }
    Ok(Value::Object(state_diff))
}

fn u256_storage_word(value: U256) -> String {
    let mut word = [0_u8; 32];
    value.to_big_endian(&mut word);
    format!("0x{}", hex::encode(word))
}

fn parse_hex_u256_word(value: &str) -> Option<U256> {
    if value.len() != 66 || !value.starts_with("0x") {
        return None;
    }
    let bytes = hex::decode(&value[2..]).ok()?;
    Some(U256::from_big_endian(&bytes))
}

fn parse_storage_address(value: &str) -> Option<String> {
    let value = value.strip_prefix("0x")?;
    if value.len() != 64 || !value.bytes().all(|byte| byte.is_ascii_hexdigit()) {
        return None;
    }
    let address = value[24..].to_ascii_lowercase();
    if address.bytes().all(|byte| byte == b'0') {
        return None;
    }
    Some(format!("0x{address}"))
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct AaveLiquidationAmounts {
    actual_repay: U256,
    seized_before_fee: U256,
    protocol_fee: U256,
    liquidator_collateral: U256,
}

fn supported_aave_liquidations(
    account: &AaveAccountData,
    reserves: &[AaveExactReserveState],
    maximum_input_weth: U256,
    emode_collateral_bitmap: U256,
    emode_liquidation_bonus_bps: u16,
    flash_premium_bps: u16,
) -> Result<Vec<AaveExactLiquidationState>, GatewayError> {
    let weth_price = reserves
        .iter()
        .find(|reserve| reserve.asset == ARBITRUM_WETH)
        .ok_or(GatewayError::ProviderIntegrity)
        .and_then(|reserve| decimal_u256(&reserve.oracle_price_base))?;
    let mut variants = supported_aave_debt_liquidations(
        account,
        reserves,
        ARBITRUM_WETH,
        &[ARBITRUM_WETH, ARBITRUM_NATIVE_USDC],
        maximum_input_weth,
        weth_price,
        emode_collateral_bitmap,
        emode_liquidation_bonus_bps,
        flash_premium_bps,
    )?;
    variants.extend(supported_aave_debt_liquidations(
        account,
        reserves,
        ARBITRUM_USDC_E,
        &[ARBITRUM_WETH],
        maximum_input_weth,
        weth_price,
        emode_collateral_bitmap,
        emode_liquidation_bonus_bps,
        flash_premium_bps,
    )?);
    Ok(variants)
}

#[allow(clippy::too_many_arguments)]
fn supported_aave_debt_liquidations(
    account: &AaveAccountData,
    reserves: &[AaveExactReserveState],
    debt_asset: &str,
    collateral_assets: &[&str],
    maximum_input_weth: U256,
    weth_price: U256,
    emode_collateral_bitmap: U256,
    emode_liquidation_bonus_bps: u16,
    flash_premium_bps: u16,
) -> Result<Vec<AaveExactLiquidationState>, GatewayError> {
    let health_factor = U256::from_dec_str(&account.health_factor_wad)
        .map_err(|_| GatewayError::ProviderIntegrity)?;
    if health_factor >= U256::exp10(18) || maximum_input_weth.is_zero() {
        return Ok(Vec::new());
    }
    let debt = reserves
        .iter()
        .find(|reserve| reserve.asset == debt_asset)
        .ok_or(GatewayError::ProviderIntegrity)?;
    let debt_configuration = U256::from_dec_str(&debt.configuration_data)
        .map_err(|_| GatewayError::ProviderIntegrity)?;
    let debt_active = debt_configuration.bit(56);
    let debt_paused = debt_configuration.bit(60);
    if !debt_active || debt_paused {
        return Ok(Vec::new());
    }
    // The deployed Aave Origin LiquidationLogic reads the variable-debt token
    // balance. Stable debt is not part of that execution path, so any non-zero
    // stable balance makes this borrower unsupported for safe sizing. Return
    // no variants (instead of failing the shared screen batch) so the observer
    // can persist a borrower-scoped unsupported_stable_weth_debt diagnosis.
    if !decimal_u256(&debt.current_stable_debt)?.is_zero() {
        return Ok(Vec::new());
    }
    let total_debt = decimal_u256(&debt.current_variable_debt)?;
    if total_debt.is_zero() {
        return Ok(Vec::new());
    }
    let debt_price = decimal_u256(&debt.oracle_price_base)?;
    let debt_unit = asset_unit(debt.decimals)?;
    if debt_price.is_zero() || weth_price.is_zero() {
        return Err(GatewayError::ProviderIntegrity);
    }
    let maximum_input = weth_to_debt_floor(maximum_input_weth, weth_price, debt_price, debt_unit)?;
    if maximum_input.is_zero() {
        return Ok(Vec::new());
    }
    let debt_reserve_base = mul_div_ceil(total_debt, debt_price, debt_unit)?;
    let total_account_debt_base = decimal_u256(&account.total_debt_base)?;
    let mut variants = Vec::with_capacity((SizeLevel::ALL.len() + 1) * collateral_assets.len());

    for collateral in reserves
        .iter()
        .filter(|reserve| collateral_assets.contains(&reserve.asset.as_str()))
    {
        let collateral_configuration = U256::from_dec_str(&collateral.configuration_data)
            .map_err(|_| GatewayError::ProviderIntegrity)?;
        let collateral_threshold = (collateral_configuration >> 16).low_u32() & 0xffff;
        if !collateral.usage_as_collateral_enabled
            || collateral_threshold == 0
            || !collateral_configuration.bit(56)
            || collateral_configuration.bit(60)
        {
            continue;
        }
        let collateral_balance = decimal_u256(&collateral.current_a_token_balance)?;
        if collateral_balance.is_zero() {
            continue;
        }
        let collateral_price = decimal_u256(&collateral.oracle_price_base)?;
        let collateral_unit = asset_unit(collateral.decimals)?;
        if collateral_price.is_zero() {
            return Err(GatewayError::ProviderIntegrity);
        }
        let collateral_reserve_base =
            mul_div_floor(collateral_balance, collateral_price, collateral_unit)?;
        let mut close_capacity = total_debt;
        if collateral_reserve_base >= U256::from(MIN_BASE_MAX_CLOSE_FACTOR_THRESHOLD)
            && debt_reserve_base >= U256::from(MIN_BASE_MAX_CLOSE_FACTOR_THRESHOLD)
            && health_factor > U256::from(CLOSE_FACTOR_HF_THRESHOLD_WAD)
        {
            let default_base = percent_mul(
                total_account_debt_base,
                U256::from(DEFAULT_LIQUIDATION_CLOSE_FACTOR_BPS),
            )?;
            if debt_reserve_base > default_base {
                close_capacity = mul_div_floor(default_base, debt_unit, debt_price)?;
            }
        }
        let requested_capacity = if close_capacity < maximum_input {
            close_capacity
        } else {
            maximum_input
        };
        if requested_capacity.is_zero() {
            continue;
        }
        let configured_bonus = (collateral_configuration >> 32).low_u32() & 0xffff;
        let in_emode = emode_liquidation_bonus_bps != 0
            && emode_collateral_bitmap.bit(usize::from(collateral.reserve_id));
        let liquidation_bonus = if in_emode {
            u32::from(emode_liquidation_bonus_bps)
        } else {
            configured_bonus
        };
        let protocol_fee = (collateral_configuration >> 152).low_u32() & 0xffff;
        if !(10_000..=20_000).contains(&liquidation_bonus) || protocol_fee > 10_000 {
            return Err(GatewayError::ProviderIntegrity);
        }
        let maximum = calculate_aave_liquidation_amounts(
            requested_capacity,
            collateral_balance,
            debt_price,
            collateral_price,
            debt_unit,
            collateral_unit,
            liquidation_bonus,
            protocol_fee,
        )?;
        if maximum.actual_repay.is_zero() {
            continue;
        }
        let fixed_grid =
            liquidation_size_grid(maximum.actual_repay, weth_price, debt_price, debt_unit)?;
        let mut fixed_failed_only_for_dust = !fixed_grid.is_empty();
        let mut accepted_fixed = false;
        for requested_repay in fixed_grid.iter().copied() {
            let amounts = calculate_aave_liquidation_amounts(
                requested_repay,
                collateral_balance,
                debt_price,
                collateral_price,
                debt_unit,
                collateral_unit,
                liquidation_bonus,
                protocol_fee,
            )?;
            if amounts.actual_repay != requested_repay {
                fixed_failed_only_for_dust = false;
                continue;
            }
            if !aave_liquidation_dust_valid(
                total_debt,
                collateral_balance,
                amounts,
                debt_price,
                collateral_price,
                debt_unit,
                collateral_unit,
            )? {
                continue;
            }
            fixed_failed_only_for_dust = false;
            accepted_fixed = true;
            variants.push(aave_liquidation_state(
                debt_asset,
                &collateral.asset,
                requested_repay,
                maximum_input,
                reviewed_weth_size_for_debt(requested_repay, weth_price, debt_price, debt_unit)?,
                amounts,
                collateral_price,
                debt_price,
                weth_price,
                collateral_unit,
                debt_unit,
                debt.decimals,
                flash_premium_bps,
                FIXED_REVIEWED_SIZE,
                "",
            )?);
        }
        let terminal_reason = if fixed_grid.is_empty() {
            Some(BELOW_MIN_REVIEWED_SIZE)
        } else if fixed_failed_only_for_dust {
            Some(DUST_PARTIAL_INVALID)
        } else {
            None
        };
        if !accepted_fixed {
            if let Some(reason) = terminal_reason {
                let terminal_size = maximum.actual_repay.min(maximum_input);
                if !terminal_size.is_zero() && !fixed_grid.contains(&terminal_size) {
                    let amounts = calculate_aave_liquidation_amounts(
                        terminal_size,
                        collateral_balance,
                        debt_price,
                        collateral_price,
                        debt_unit,
                        collateral_unit,
                        liquidation_bonus,
                        protocol_fee,
                    )?;
                    if amounts.actual_repay == terminal_size
                        && aave_liquidation_dust_valid(
                            total_debt,
                            collateral_balance,
                            amounts,
                            debt_price,
                            collateral_price,
                            debt_unit,
                            collateral_unit,
                        )?
                    {
                        variants.push(aave_liquidation_state(
                            debt_asset,
                            &collateral.asset,
                            terminal_size,
                            maximum_input,
                            debt_to_weth_floor(terminal_size, debt_price, weth_price, debt_unit)?,
                            amounts,
                            collateral_price,
                            debt_price,
                            weth_price,
                            collateral_unit,
                            debt_unit,
                            debt.decimals,
                            flash_premium_bps,
                            TERMINAL_SIZE_REQUIRED,
                            reason,
                        )?);
                    }
                }
            }
        }
    }
    Ok(variants)
}

#[allow(clippy::too_many_arguments)]
fn aave_liquidation_state(
    debt_asset: &str,
    collateral_asset: &str,
    requested_repay: U256,
    maximum_repay: U256,
    reviewed_size_weth: U256,
    amounts: AaveLiquidationAmounts,
    collateral_price: U256,
    debt_price: U256,
    weth_price: U256,
    collateral_unit: U256,
    debt_unit: U256,
    debt_decimals: u8,
    flash_premium_bps: u16,
    size_classification: &str,
    terminal_size_reason: &str,
) -> Result<AaveExactLiquidationState, GatewayError> {
    let flash_premium = percent_mul(amounts.actual_repay, U256::from(flash_premium_bps))?;
    let oracle_unwind_output = mul_div_floor(
        checked_mul(amounts.liquidator_collateral, collateral_price)?,
        debt_unit,
        checked_mul(collateral_unit, debt_price)?,
    )?;
    let oracle_unwind_output_weth =
        debt_to_weth_floor(oracle_unwind_output, debt_price, weth_price, debt_unit)?;
    Ok(AaveExactLiquidationState {
        debt_asset: debt_asset.to_string(),
        collateral_asset: collateral_asset.to_string(),
        debt_asset_decimals: debt_decimals,
        debt_asset_price_base: debt_price.to_string(),
        weth_price_base: weth_price.to_string(),
        maximum_repay_amount: maximum_repay.to_string(),
        reviewed_size_weth_wei: reviewed_size_weth.to_string(),
        debt_asset_review: if debt_asset == ARBITRUM_USDC_E {
            "usdc_e_debt_reviewed"
        } else {
            "weth_debt_reviewed"
        }
        .to_string(),
        size_classification: size_classification.to_string(),
        terminal_size_reason: terminal_size_reason.to_string(),
        requested_repay_amount: requested_repay.to_string(),
        actual_repay_amount: amounts.actual_repay.to_string(),
        repay_amount: amounts.actual_repay.to_string(),
        flash_premium_amount: flash_premium.to_string(),
        seized_collateral: amounts.seized_before_fee.to_string(),
        protocol_fee_collateral: amounts.protocol_fee.to_string(),
        liquidator_collateral: amounts.liquidator_collateral.to_string(),
        oracle_unwind_output_weth: oracle_unwind_output_weth.to_string(),
        oracle_unwind_output_debt_asset: oracle_unwind_output.to_string(),
        unwind_quotes: Vec::new(),
    })
}

#[allow(clippy::too_many_arguments)]
fn calculate_aave_liquidation_amounts(
    requested_repay: U256,
    collateral_balance: U256,
    debt_price: U256,
    collateral_price: U256,
    debt_unit: U256,
    collateral_unit: U256,
    liquidation_bonus: u32,
    protocol_fee: u32,
) -> Result<AaveLiquidationAmounts, GatewayError> {
    let numerator = checked_mul(checked_mul(debt_price, requested_repay)?, collateral_unit)?;
    let denominator = checked_mul(collateral_price, debt_unit)?;
    if denominator.is_zero() {
        return Err(GatewayError::ProviderIntegrity);
    }
    let base_collateral = numerator / denominator;
    let maximum_collateral = percent_mul_floor(base_collateral, U256::from(liquidation_bonus))?;
    let (seized_before_fee, actual_repay) = if maximum_collateral > collateral_balance {
        let debt_numerator = checked_mul(
            checked_mul(collateral_price, collateral_balance)?,
            debt_unit,
        )?;
        let debt_denominator = checked_mul(debt_price, collateral_unit)?;
        if debt_denominator.is_zero() {
            return Err(GatewayError::ProviderIntegrity);
        }
        let base_debt_needed = debt_numerator / debt_denominator;
        (
            collateral_balance,
            percent_div_ceil(base_debt_needed, U256::from(liquidation_bonus))?,
        )
    } else {
        (maximum_collateral, requested_repay)
    };
    let base_collateral_without_bonus =
        percent_div_floor(seized_before_fee, U256::from(liquidation_bonus))?;
    let bonus_collateral = seized_before_fee
        .checked_sub(base_collateral_without_bonus)
        .ok_or(GatewayError::ProviderIntegrity)?;
    let protocol_fee = percent_mul_ceil(bonus_collateral, U256::from(protocol_fee))?;
    let liquidator_collateral = seized_before_fee
        .checked_sub(protocol_fee)
        .ok_or(GatewayError::ProviderIntegrity)?;
    Ok(AaveLiquidationAmounts {
        actual_repay,
        seized_before_fee,
        protocol_fee,
        liquidator_collateral,
    })
}

#[allow(clippy::too_many_arguments)]
fn aave_liquidation_dust_valid(
    total_debt: U256,
    collateral_balance: U256,
    amounts: AaveLiquidationAmounts,
    debt_price: U256,
    collateral_price: U256,
    debt_unit: U256,
    collateral_unit: U256,
) -> Result<bool, GatewayError> {
    let remaining_debt = total_debt
        .checked_sub(amounts.actual_repay)
        .ok_or(GatewayError::ProviderIntegrity)?;
    let remaining_collateral = collateral_balance
        .checked_sub(amounts.seized_before_fee)
        .ok_or(GatewayError::ProviderIntegrity)?;
    if remaining_debt.is_zero() || remaining_collateral.is_zero() {
        return Ok(true);
    }
    let remaining_debt_base = mul_div_ceil(remaining_debt, debt_price, debt_unit)?;
    let remaining_collateral_base =
        mul_div_floor(remaining_collateral, collateral_price, collateral_unit)?;
    Ok(remaining_debt_base >= U256::from(MIN_LEFTOVER_BASE)
        && remaining_collateral_base >= U256::from(MIN_LEFTOVER_BASE))
}

fn aave_liquidation_grace_elapsed(
    debt_grace_period_until: u64,
    collateral_grace_period_until: u64,
    block_timestamp: u64,
) -> bool {
    debt_grace_period_until < block_timestamp && collateral_grace_period_until < block_timestamp
}

fn liquidation_size_grid(
    maximum_repay: U256,
    weth_price: U256,
    debt_price: U256,
    debt_unit: U256,
) -> Result<Vec<U256>, GatewayError> {
    SizeLevel::ALL
        .into_iter()
        .map(|level| {
            weth_to_debt_floor(
                U256::from(level.amount_wei()),
                weth_price,
                debt_price,
                debt_unit,
            )
        })
        .filter_map(|result| match result {
            Ok(amount) if !amount.is_zero() && amount <= maximum_repay => Some(Ok(amount)),
            Ok(_) => None,
            Err(error) => Some(Err(error)),
        })
        .collect()
}

fn reviewed_weth_size_for_debt(
    debt_raw: U256,
    weth_price: U256,
    debt_price: U256,
    debt_unit: U256,
) -> Result<U256, GatewayError> {
    for level in SizeLevel::ALL {
        if weth_to_debt_floor(
            U256::from(level.amount_wei()),
            weth_price,
            debt_price,
            debt_unit,
        )? == debt_raw
        {
            return Ok(U256::from(level.amount_wei()));
        }
    }
    Err(GatewayError::ProviderIntegrity)
}

fn asset_unit(decimals: u8) -> Result<U256, GatewayError> {
    if decimals > 36 {
        return Err(GatewayError::ProviderIntegrity);
    }
    Ok(U256::exp10(usize::from(decimals)))
}

fn mul_div_floor(value: U256, multiplier: U256, divisor: U256) -> Result<U256, GatewayError> {
    if divisor.is_zero() {
        return Err(GatewayError::ProviderIntegrity);
    }
    Ok(checked_mul(value, multiplier)? / divisor)
}

fn mul_div_ceil(value: U256, multiplier: U256, divisor: U256) -> Result<U256, GatewayError> {
    if divisor.is_zero() {
        return Err(GatewayError::ProviderIntegrity);
    }
    let product = checked_mul(value, multiplier)?;
    Ok(product
        .checked_add(divisor - U256::one())
        .ok_or(GatewayError::ProviderIntegrity)?
        / divisor)
}

fn weth_to_debt_floor(
    weth_wei: U256,
    weth_price: U256,
    debt_price: U256,
    debt_unit: U256,
) -> Result<U256, GatewayError> {
    mul_div_floor(
        checked_mul(weth_wei, weth_price)?,
        debt_unit,
        checked_mul(U256::exp10(18), debt_price)?,
    )
}

fn debt_to_weth_floor(
    debt_raw: U256,
    debt_price: U256,
    weth_price: U256,
    debt_unit: U256,
) -> Result<U256, GatewayError> {
    mul_div_floor(
        checked_mul(debt_raw, debt_price)?,
        U256::exp10(18),
        checked_mul(debt_unit, weth_price)?,
    )
}

fn percent_mul(value: U256, percentage: U256) -> Result<U256, GatewayError> {
    Ok(checked_mul(value, percentage)?
        .checked_add(U256::from(HALF_PERCENTAGE_FACTOR))
        .ok_or(GatewayError::ProviderIntegrity)?
        / U256::from(PERCENTAGE_FACTOR))
}

fn percent_mul_floor(value: U256, percentage: U256) -> Result<U256, GatewayError> {
    Ok(checked_mul(value, percentage)? / U256::from(PERCENTAGE_FACTOR))
}

fn percent_mul_ceil(value: U256, percentage: U256) -> Result<U256, GatewayError> {
    let product = checked_mul(value, percentage)?;
    Ok(product
        .checked_add(U256::from(PERCENTAGE_FACTOR - 1))
        .ok_or(GatewayError::ProviderIntegrity)?
        / U256::from(PERCENTAGE_FACTOR))
}

fn percent_div_floor(value: U256, percentage: U256) -> Result<U256, GatewayError> {
    if percentage.is_zero() {
        return Err(GatewayError::ProviderIntegrity);
    }
    Ok(checked_mul(value, U256::from(PERCENTAGE_FACTOR))? / percentage)
}

fn percent_div_ceil(value: U256, percentage: U256) -> Result<U256, GatewayError> {
    if percentage.is_zero() {
        return Err(GatewayError::ProviderIntegrity);
    }
    let numerator = checked_mul(value, U256::from(PERCENTAGE_FACTOR))?;
    Ok(numerator
        .checked_add(percentage - U256::one())
        .ok_or(GatewayError::ProviderIntegrity)?
        / percentage)
}

fn decimal_u256(value: &str) -> Result<U256, GatewayError> {
    U256::from_dec_str(value).map_err(|_| GatewayError::ProviderIntegrity)
}

fn checked_mul(left: U256, right: U256) -> Result<U256, GatewayError> {
    left.checked_mul(right)
        .ok_or(GatewayError::ProviderIntegrity)
}

fn encode_uniswap_quote(token_in: &str, token_out: &str, amount: U256, fee: u32) -> String {
    let mut encoded = Vec::with_capacity(32 * 5);
    encoded.extend_from_slice(&[0_u8; 12]);
    encoded.extend_from_slice(&hex::decode(&token_in[2..]).expect("pinned token address"));
    encoded.extend_from_slice(&[0_u8; 12]);
    encoded.extend_from_slice(&hex::decode(&token_out[2..]).expect("pinned token address"));
    let mut amount_word = [0_u8; 32];
    amount.to_big_endian(&mut amount_word);
    encoded.extend_from_slice(&amount_word);
    let mut fee_word = [0_u8; 32];
    fee_word[28..].copy_from_slice(&fee.to_be_bytes());
    encoded.extend_from_slice(&fee_word);
    encoded.extend_from_slice(&[0_u8; 32]);
    format!(
        "{}{}",
        UNISWAP_QUOTE_EXACT_INPUT_SINGLE_SELECTOR,
        hex::encode(encoded)
    )
}

fn parse_u32_bytes(value: &[u8]) -> Option<u32> {
    if value.len() != 32 || value[..28].iter().any(|byte| *byte != 0) {
        return None;
    }
    Some(u32::from_be_bytes(value[28..].try_into().ok()?))
}

fn parse_u8_bytes(value: &[u8]) -> Option<u8> {
    let value = parse_u32_bytes(value)?;
    u8::try_from(value).ok()
}

fn parse_i24_bytes(value: &[u8]) -> Option<i32> {
    if value.len() != 32 {
        return None;
    }
    let negative = value[29] & 0x80 != 0;
    let expected_prefix = if negative { 0xff } else { 0x00 };
    if value[..29].iter().any(|byte| *byte != expected_prefix) {
        return None;
    }
    let raw = u32::from_be_bytes([0, value[29], value[30], value[31]]);
    Some(if negative {
        raw as i32 - (1_i32 << 24)
    } else {
        raw as i32
    })
}

fn parse_u256_word(value: &[u8]) -> Option<U256> {
    if value.len() != 32 {
        return None;
    }
    Some(U256::from_big_endian(value))
}

fn parse_u128_word(value: &[u8]) -> Option<u128> {
    if value.len() != 32 || value[..16].iter().any(|byte| *byte != 0) {
        return None;
    }
    Some(u128::from_be_bytes(value[16..].try_into().ok()?))
}

fn parse_slot0_bytes(value: &[u8]) -> Option<(U256, i32)> {
    if value.len() < 64 || value.len() > MAX_STATE_RESPONSE_DATA_BYTES {
        return None;
    }
    let sqrt_price_x96 = parse_u256_word(&value[..32])?;
    if sqrt_price_x96.is_zero() || sqrt_price_x96.bits() > 160 {
        return None;
    }
    let tick = parse_i24_bytes(&value[32..64])?;
    if !(-887_272..=887_272).contains(&tick) {
        return None;
    }
    Some((sqrt_price_x96, tick))
}

fn parse_tick_bytes(value: &[u8]) -> Option<(u128, i128)> {
    if value.len() < 64 || value.len() > MAX_STATE_RESPONSE_DATA_BYTES {
        return None;
    }
    let gross = parse_u128_word(&value[..32])?;
    let signed = &value[32..64];
    let negative = signed[16] & 0x80 != 0;
    let prefix = if negative { 0xff } else { 0x00 };
    if signed[..16].iter().any(|byte| *byte != prefix) {
        return None;
    }
    let net = u128::from_be_bytes(signed[16..].try_into().ok()?) as i128;
    Some((gross, net))
}

fn centered_word_positions(
    tick: i32,
    tick_spacing: i32,
    maximum_words: usize,
) -> Result<Vec<i16>, GatewayError> {
    if tick_spacing <= 0 || maximum_words == 0 || maximum_words > 32 {
        return Err(GatewayError::InvalidRequest);
    }
    let mut compressed = tick / tick_spacing;
    if tick < 0 && tick % tick_spacing != 0 {
        compressed -= 1;
    }
    let center = compressed >> 8;
    let left = i32::try_from((maximum_words - 1) / 2).map_err(|_| GatewayError::InvalidRequest)?;
    let start = center
        .checked_sub(left)
        .ok_or(GatewayError::StateIncomplete)?;
    (0..maximum_words)
        .map(|offset| {
            start
                .checked_add(i32::try_from(offset).map_err(|_| GatewayError::InvalidRequest)?)
                .and_then(|value| i16::try_from(value).ok())
                .ok_or(GatewayError::StateIncomplete)
        })
        .collect()
}

fn encode_signed_call(name: &str, bits: usize, value: i128) -> String {
    let encoded = if value < 0 {
        U256::MAX - U256::from(value.unsigned_abs()) + U256::one()
    } else {
        U256::from(value as u128)
    };
    let mut data = ethabi::short_signature(name, &[ParamType::Int(bits)]).to_vec();
    data.extend(ethabi::encode(&[Token::Int(encoded)]));
    format!("0x{}", hex::encode(data))
}

fn map_call_failure(failure: CallFailure) -> GatewayError {
    match failure {
        CallFailure::Budget => GatewayError::UpstreamBudgetExhausted,
        CallFailure::Transport(TransportError::ExecutionReverted { reason }) => {
            GatewayError::ExecutionReverted { reason }
        }
        CallFailure::Transport(_) => GatewayError::ProviderUnavailable,
        CallFailure::Integrity => GatewayError::ProviderIntegrity,
    }
}

fn map_hunter_contract_error(error: HunterStateError) -> GatewayError {
    match error {
        HunterStateError::ProviderDisagreement => GatewayError::ProviderDisagreement,
        HunterStateError::StateIncomplete | HunterStateError::LimitExceeded => {
            GatewayError::StateIncomplete
        }
        HunterStateError::InvalidContract | HunterStateError::HashMismatch => {
            GatewayError::ProviderIntegrity
        }
    }
}

fn canonical_quantity(value: &str) -> bool {
    let Some(body) = value.strip_prefix("0x") else {
        return false;
    };
    !body.is_empty()
        && body.len() <= 16
        && (body == "0" || !body.starts_with('0'))
        && body
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
}

fn format_quantity(value: u64) -> String {
    format!("0x{value:x}")
}

fn normalize_aave_tail_logs(
    value: &Value,
    from_block: u64,
    to_block: u64,
) -> Result<Vec<NormalizedAaveTailLog>, GatewayError> {
    let logs = value.as_array().ok_or(GatewayError::ProviderIntegrity)?;
    if logs.len() > 4_096 {
        return Err(GatewayError::ResponseOversized);
    }
    let mut normalized = Vec::with_capacity(logs.len());
    for log in logs {
        if log.get("removed").and_then(Value::as_bool) != Some(false)
            || log
                .get("address")
                .and_then(Value::as_str)
                .map(str::to_ascii_lowercase)
                .as_deref()
                != Some(AAVE_V3_POOL_ARBITRUM)
        {
            return Err(GatewayError::ProviderIntegrity);
        }
        let block_number = required_quantity(log, "blockNumber")?;
        if block_number < from_block || block_number > to_block {
            return Err(GatewayError::ProviderIntegrity);
        }
        let transaction_index = required_quantity(log, "transactionIndex")?;
        let log_index = required_quantity(log, "logIndex")?;
        let block_hash = required_lower_hex(log, "blockHash", 32)?;
        let transaction_hash = required_lower_hex(log, "transactionHash", 32)?;
        let topics = log
            .get("topics")
            .and_then(Value::as_array)
            .filter(|topics| topics.len() == 4)
            .ok_or(GatewayError::ProviderIntegrity)?;
        let topic = topics[0]
            .as_str()
            .map(str::to_ascii_lowercase)
            .ok_or(GatewayError::ProviderIntegrity)?;
        let borrower_index = match topic.as_str() {
            AAVE_BORROW_TOPIC | AAVE_REPAY_TOPIC => 2,
            AAVE_LIQUIDATION_TOPIC => 3,
            _ => return Err(GatewayError::ProviderIntegrity),
        };
        let borrower_topic = topics[borrower_index]
            .as_str()
            .map(str::to_ascii_lowercase)
            .filter(|value| {
                value.len() == 66
                    && value.starts_with("0x000000000000000000000000")
                    && value[26..].bytes().all(|byte| byte.is_ascii_hexdigit())
            })
            .ok_or(GatewayError::ProviderIntegrity)?;
        let borrower = format!("0x{}", &borrower_topic[26..]);
        if borrower == "0x0000000000000000000000000000000000000000" {
            return Err(GatewayError::ProviderIntegrity);
        }
        let data = log
            .get("data")
            .and_then(Value::as_str)
            .map(str::to_ascii_lowercase)
            .filter(|value| canonical_data(value, MAX_STATE_RESPONSE_DATA_BYTES))
            .ok_or(GatewayError::ProviderIntegrity)?;
        normalized.push(NormalizedAaveTailLog {
            block_number,
            transaction_index,
            log_index,
            block_hash,
            transaction_hash,
            topic,
            data_hash: canonical_hash_bytes(data.as_bytes()),
            borrower,
        });
    }
    normalized.sort();
    if normalized.windows(2).any(|events| {
        events[0].block_number == events[1].block_number
            && events[0].log_index == events[1].log_index
    }) {
        return Err(GatewayError::ProviderIntegrity);
    }
    Ok(normalized)
}

fn required_quantity(value: &Value, field: &str) -> Result<u64, GatewayError> {
    let encoded = value
        .get(field)
        .and_then(Value::as_str)
        .and_then(|value| value.strip_prefix("0x"))
        .filter(|value| !value.is_empty() && value.len() <= 16)
        .ok_or(GatewayError::ProviderIntegrity)?;
    u64::from_str_radix(encoded, 16).map_err(|_| GatewayError::ProviderIntegrity)
}

fn required_lower_hex(value: &Value, field: &str, bytes: usize) -> Result<String, GatewayError> {
    let encoded = value
        .get(field)
        .and_then(Value::as_str)
        .ok_or(GatewayError::ProviderIntegrity)?;
    if encoded.len() != 2 + bytes * 2
        || !encoded.starts_with("0x")
        || encoded[2..]
            .bytes()
            .any(|byte| !byte.is_ascii_digit() && !(b'a'..=b'f').contains(&byte))
    {
        return Err(GatewayError::ProviderIntegrity);
    }
    Ok(encoded.to_string())
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct VerifiedSourceInclusion {
    block_number: u64,
    block_hash: String,
    transaction_index: u64,
    parent_block_number: u64,
    parent_block_hash: String,
    transaction_status: &'static str,
    source_event_index: Option<u64>,
    source_pool_addresses: Vec<String>,
}

#[derive(Clone, Debug)]
struct ResolvedSourceInclusion {
    provider: ProviderLease,
    inclusion: VerifiedSourceInclusion,
    provider_response_hash: String,
}

fn verify_source_inclusion(
    request: &SourceEvidenceRequest,
    transaction: &Value,
    receipt: &Value,
    block: &Value,
) -> Result<VerifiedSourceInclusion, GatewayError> {
    let block_number = required_quantity(transaction, "blockNumber")?;
    let block_hash = required_lower_hex(transaction, "blockHash", 32)?;
    let transaction_index = required_quantity(transaction, "transactionIndex")?;
    if required_lower_hex(transaction, "hash", 32)? != request.source_transaction_hash
        || required_lower_hex(transaction, "to", 20)? != request.source_router
        || required_lower_hex(receipt, "transactionHash", 32)? != request.source_transaction_hash
        || required_quantity(receipt, "blockNumber")? != block_number
        || required_lower_hex(receipt, "blockHash", 32)? != block_hash
        || required_quantity(receipt, "transactionIndex")? != transaction_index
        || required_quantity(block, "number")? != block_number
        || required_lower_hex(block, "hash", 32)? != block_hash
    {
        return Err(GatewayError::ProviderIntegrity);
    }
    let transactions = block
        .get("transactions")
        .and_then(Value::as_array)
        .ok_or(GatewayError::ProviderIntegrity)?;
    if transactions
        .get(usize::try_from(transaction_index).map_err(|_| GatewayError::ProviderIntegrity)?)
        .and_then(Value::as_str)
        != Some(request.source_transaction_hash.as_str())
    {
        return Err(GatewayError::ProviderIntegrity);
    }
    let transaction_status = match required_quantity(receipt, "status")? {
        1 => "success",
        0 => "reverted",
        _ => return Err(GatewayError::ProviderIntegrity),
    };
    let expected_pool_addresses = expected_uniswap_v3_pool_addresses(
        &request.source_factory,
        &request.source_token_path,
        &request.source_fee_path,
    )
    .map_err(|_| GatewayError::InvalidRequest)?;
    let (source_event_index, source_pool_addresses) =
        source_swap_logs(receipt, transaction_status, &expected_pool_addresses)?;
    Ok(VerifiedSourceInclusion {
        block_number,
        block_hash,
        transaction_index,
        parent_block_number: block_number
            .checked_sub(1)
            .ok_or(GatewayError::ProviderIntegrity)?,
        parent_block_hash: required_lower_hex(block, "parentHash", 32)?,
        transaction_status,
        source_event_index,
        source_pool_addresses,
    })
}

fn source_swap_logs(
    receipt: &Value,
    transaction_status: &str,
    expected_pool_addresses: &[String],
) -> Result<(Option<u64>, Vec<String>), GatewayError> {
    let swap_topic = format!(
        "0x{}",
        hex::encode(ethabi::long_signature(
            "Swap",
            &[
                ParamType::Address,
                ParamType::Address,
                ParamType::Int(256),
                ParamType::Int(256),
                ParamType::Uint(160),
                ParamType::Uint(128),
                ParamType::Int(24),
            ],
        ))
    );
    let logs = receipt
        .get("logs")
        .and_then(Value::as_array)
        .ok_or(GatewayError::ProviderIntegrity)?;
    let mut matches = Vec::new();
    for log in logs {
        let is_swap = log
            .get("topics")
            .and_then(Value::as_array)
            .and_then(|topics| topics.first())
            .and_then(Value::as_str)
            == Some(swap_topic.as_str());
        if !is_swap {
            continue;
        }
        let address = required_lower_hex(log, "address", 20)?;
        let index = required_quantity(log, "logIndex")?;
        matches.push((index, address));
    }
    if matches.is_empty() {
        return if transaction_status == "reverted" {
            Ok((None, Vec::new()))
        } else {
            Err(GatewayError::ProviderIntegrity)
        };
    }
    if transaction_status != "success" || matches.len() != expected_pool_addresses.len() {
        return Err(GatewayError::ProviderIntegrity);
    }
    let selected = matches;
    if selected.windows(2).any(|pair| pair[0].0 >= pair[1].0)
        || selected
            .iter()
            .map(|(_, address)| address)
            .ne(expected_pool_addresses.iter())
    {
        return Err(GatewayError::ProviderIntegrity);
    }
    Ok((
        selected.first().map(|(index, _)| *index),
        selected.into_iter().map(|(_, address)| address).collect(),
    ))
}

type SourceTraceEvidence = (String, Option<String>, String, Option<String>, Value);

#[derive(Clone, Copy)]
struct SourceTraceContext<'a> {
    request: &'a SourceEvidenceRequest,
    block_number: u64,
    block_hash: &'a str,
    transaction_index: u64,
    parent_block_number: u64,
    parent_block_hash: &'a str,
}

fn source_trace_evidence(
    prestate_trace: Result<RpcCallResult, CallFailure>,
    diff_trace: Result<RpcCallResult, CallFailure>,
    pool_addresses: &[String],
    context: SourceTraceContext<'_>,
) -> Result<SourceTraceEvidence, GatewayError> {
    let unavailable = |reason: &str| {
        Ok((
            "unavailable".to_string(),
            None,
            "incomplete".to_string(),
            Some(reason.to_string()),
            json!({
                "schema_version": "phoenix.transaction-boundary-state.v1",
                "complete": false,
                "failure_reason": reason,
                "source_transaction_hash": context.request.source_transaction_hash,
                "source_block_number": context.block_number,
                "source_block_hash": context.block_hash,
                "source_transaction_index": context.transaction_index,
                "source_feed_sequence": context.request.source_feed_sequence,
                "source_feed_order_position": context.request.source_feed_order_position,
                "source_command_index": context.request.source_command_index,
                "parent_block_number": context.parent_block_number,
                "parent_block_hash": context.parent_block_hash,
                "source_factory": context.request.source_factory,
                "source_pool_path": context.request.source_pool_path,
                "source_token_path": context.request.source_token_path,
                "source_encoded_token_path": context.request.source_encoded_token_path,
                "source_fee_path": context.request.source_fee_path,
                "source_pool_addresses": pool_addresses
            }),
        ))
    };
    let prestate_trace = match prestate_trace {
        Ok(value) => value,
        Err(failure) => return unavailable(source_trace_failure_reason(failure)),
    };
    let diff_trace = match diff_trace {
        Ok(value) => value,
        Err(failure) => return unavailable(source_trace_failure_reason(failure)),
    };
    if pool_addresses.is_empty() {
        return unavailable("source_pool_logs_unavailable");
    }
    let prestate_hash =
        hash_json(&prestate_trace.value).map_err(|_| GatewayError::ProviderIntegrity)?;
    let state_diff_hash =
        hash_json(&diff_trace.value).map_err(|_| GatewayError::ProviderIntegrity)?;
    let trace_response_hash = hash_json(&json!({
        "prestate_hash": prestate_hash,
        "state_diff_hash": state_diff_hash
    }))
    .map_err(|_| GatewayError::ProviderIntegrity)?;
    let prestate = prestate_trace
        .value
        .as_object()
        .ok_or(GatewayError::ProviderIntegrity)?;
    let diff_pre = diff_trace
        .value
        .get("pre")
        .and_then(Value::as_object)
        .ok_or(GatewayError::ProviderIntegrity)?;
    let diff_post = diff_trace
        .value
        .get("post")
        .and_then(Value::as_object)
        .ok_or(GatewayError::ProviderIntegrity)?;
    let mut transitions = serde_json::Map::new();
    for address in pool_addresses {
        let Some(before) = prestate.get(address).cloned() else {
            return unavailable("source_pool_trace_state_unavailable");
        };
        let changed_before = diff_pre.get(address).cloned().unwrap_or(Value::Null);
        let changed_after = diff_post.get(address).cloned().unwrap_or(Value::Null);
        if changed_before.is_null() && changed_after.is_null() {
            return unavailable("source_pool_trace_diff_unavailable");
        }
        let Some(after) = apply_account_diff(&before, &changed_before, &changed_after) else {
            return Err(GatewayError::ProviderIntegrity);
        };
        transitions.insert(
            address.clone(),
            json!({
                "pre": before,
                "post": after,
                "diff_pre": changed_before,
                "diff_post": changed_after
            }),
        );
    }
    let transitions = Value::Object(transitions);
    let post_state_hash = hash_json(&json!({
        "schema_version": "phoenix.post-initiating-state.v1",
        "source_event_identity": context.request.source_event_identity,
        "source_identity_hash": context.request.source_identity_hash,
        "source_transaction_hash": context.request.source_transaction_hash,
        "source_feed_sequence": context.request.source_feed_sequence,
        "source_feed_order_position": context.request.source_feed_order_position,
        "source_command_index": context.request.source_command_index,
        "source_block_number": context.block_number,
        "source_block_hash": context.block_hash,
        "source_transaction_index": context.transaction_index,
        "parent_block_number": context.parent_block_number,
        "parent_block_hash": context.parent_block_hash,
        "source_factory": context.request.source_factory,
        "source_pool_path": context.request.source_pool_path,
        "source_token_path": context.request.source_token_path,
        "source_encoded_token_path": context.request.source_encoded_token_path,
        "source_fee_path": context.request.source_fee_path,
        "source_pool_addresses": pool_addresses,
        "prestate_hash": prestate_hash,
        "state_diff_hash": state_diff_hash,
        "pool_state_transitions": transitions
    }))
    .map_err(|_| GatewayError::ProviderIntegrity)?;
    let evidence = json!({
        "schema_version": "phoenix.transaction-boundary-state.v1",
        "complete": true,
        "trace_response_hash": trace_response_hash,
        "prestate_hash": prestate_hash,
        "state_diff_hash": state_diff_hash,
        "source_transaction_hash": context.request.source_transaction_hash,
        "source_block_number": context.block_number,
        "source_block_hash": context.block_hash,
        "source_transaction_index": context.transaction_index,
        "source_feed_sequence": context.request.source_feed_sequence,
        "source_feed_order_position": context.request.source_feed_order_position,
        "source_command_index": context.request.source_command_index,
        "parent_block_number": context.parent_block_number,
        "parent_block_hash": context.parent_block_hash,
        "source_factory": context.request.source_factory,
        "source_pool_path": context.request.source_pool_path,
        "source_token_path": context.request.source_token_path,
        "source_encoded_token_path": context.request.source_encoded_token_path,
        "source_fee_path": context.request.source_fee_path,
        "source_pool_addresses": pool_addresses,
        "pool_state_transitions": transitions
    });
    if serde_json::to_vec(&evidence)
        .map_err(|_| GatewayError::ProviderIntegrity)?
        .len()
        > MAX_SOURCE_STATE_EVIDENCE_BYTES
    {
        return unavailable("transaction_trace_evidence_oversized");
    }
    Ok((
        "debug_trace_transaction_prestate_diff".to_string(),
        Some(post_state_hash),
        "complete".to_string(),
        None,
        evidence,
    ))
}

fn source_trace_failure_reason(failure: CallFailure) -> &'static str {
    match failure {
        CallFailure::Budget => "transaction_trace_budget_unavailable",
        CallFailure::Integrity => "transaction_trace_provider_integrity_failure",
        CallFailure::Transport(TransportError::Timeout) => "transaction_trace_timeout",
        CallFailure::Transport(TransportError::Oversized) => "transaction_trace_response_oversized",
        CallFailure::Transport(TransportError::MethodUnsupported) => {
            "transaction_trace_method_unsupported"
        }
        CallFailure::Transport(TransportError::HistoricalStateUnavailable) => {
            "transaction_trace_historical_state_unavailable"
        }
        CallFailure::Transport(_) => "transaction_trace_provider_unavailable",
    }
}

fn apply_account_diff(
    before: &Value,
    changed_before: &Value,
    changed_after: &Value,
) -> Option<Value> {
    let mut after = before.as_object()?.clone();
    let changed_before = changed_before.as_object()?;
    let changed_after = changed_after.as_object()?;
    let cleared_storage = changed_before
        .get("storage")
        .and_then(Value::as_object)
        .into_iter()
        .flat_map(|storage| storage.keys())
        .filter(|slot| {
            !changed_after
                .get("storage")
                .and_then(Value::as_object)
                .is_some_and(|storage| storage.contains_key(*slot))
        })
        .cloned()
        .collect::<Vec<_>>();
    if !cleared_storage.is_empty() {
        let storage = after.get_mut("storage")?.as_object_mut()?;
        for slot in cleared_storage {
            storage.remove(&slot);
        }
    }
    for (field, value) in changed_after {
        if field == "storage" {
            let storage_changes = value.as_object()?;
            let storage = after
                .entry("storage".to_string())
                .or_insert_with(|| Value::Object(serde_json::Map::new()))
                .as_object_mut()?;
            for (slot, slot_value) in storage_changes {
                if slot_value.is_null() {
                    storage.remove(slot);
                } else {
                    storage.insert(slot.clone(), slot_value.clone());
                }
            }
        } else if value.is_null() {
            after.remove(field);
        } else {
            after.insert(field.clone(), value.clone());
        }
    }
    Some(Value::Object(after))
}

fn unix_time_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis()
        .min(u128::from(u64::MAX)) as u64
}

fn unix_time_seconds() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::providers::parse_provider_config;
    use crate::shadow_state::{PoolStateRequest, SHADOW_STATE_SCHEMA_VERSION};
    use async_trait::async_trait;
    use ethabi::{decode, encode, ParamType, Token};
    use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
    use std::sync::Mutex as StdMutex;
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    use tokio::net::TcpListener;

    const BLOCK_HASH: &str = "0xaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";
    const REORG_HASH: &str = "0xbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb";
    const NEXT_HASH: &str = "0xcccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccc";

    #[test]
    fn upstream_outcomes_distinguish_reverts_without_weakening_failures() {
        let success = Ok(RpcCallResult {
            value: Value::Null,
            latency_ns: 1,
        });
        assert_eq!(
            classify_upstream_outcome(&success),
            UpstreamOutcome::Success
        );
        assert_eq!(
            classify_upstream_outcome(&Err(TransportError::ExecutionReverted { reason: None })),
            UpstreamOutcome::Reverted
        );
        assert_eq!(
            classify_upstream_outcome(&Err(TransportError::Timeout)),
            UpstreamOutcome::Timeout
        );
        assert_eq!(
            classify_upstream_outcome(&Err(TransportError::RateLimited {
                retry_after: Duration::from_secs(1),
            })),
            UpstreamOutcome::RateLimited
        );
        for failure in [
            TransportError::Http,
            TransportError::Oversized,
            TransportError::InvalidResponse,
            TransportError::MethodUnsupported,
            TransportError::HistoricalStateUnavailable,
            TransportError::ProviderError,
        ] {
            assert_eq!(
                classify_upstream_outcome(&Err(failure)),
                UpstreamOutcome::Failure
            );
        }
        let mapped = map_call_failure(CallFailure::Transport(TransportError::ExecutionReverted {
            reason: None,
        }));
        assert_eq!(mapped.class(), "execution_reverted");
        assert!(!mapped.retryable());
        let generic = map_call_failure(CallFailure::Transport(TransportError::ProviderError));
        assert_eq!(generic.class(), "provider_unavailable");
    }

    #[derive(Clone, Debug)]
    struct CallRecord {
        provider_id: String,
        method: RpcMethod,
        params: Value,
    }

    #[derive(Debug)]
    struct ModelClient {
        calls: StdMutex<Vec<CallRecord>>,
        head: StdMutex<PinnedBlock>,
        state_root: StdMutex<String>,
        rate_limit_once: StdMutex<HashSet<String>>,
        source_receipt_failure: StdMutex<HashSet<String>>,
        disagreement: AtomicBool,
        exact_primary_disagreement: AtomicBool,
        exact_disagreement: AtomicBool,
        exact_availability_disagreement: AtomicBool,
        finalized_disagreement: AtomicBool,
        malformed_multicall: AtomicBool,
        delay_multicall: Duration,
        active_eth_calls: AtomicU64,
        maximum_active_eth_calls: AtomicU64,
    }

    impl Default for ModelClient {
        fn default() -> Self {
            Self {
                calls: StdMutex::new(Vec::new()),
                head: StdMutex::new(PinnedBlock {
                    number: 100,
                    hash: BLOCK_HASH.to_string(),
                }),
                state_root: StdMutex::new(format!("0x{}", "d".repeat(64))),
                rate_limit_once: StdMutex::new(HashSet::new()),
                source_receipt_failure: StdMutex::new(HashSet::new()),
                disagreement: AtomicBool::new(false),
                exact_primary_disagreement: AtomicBool::new(false),
                exact_disagreement: AtomicBool::new(false),
                exact_availability_disagreement: AtomicBool::new(false),
                finalized_disagreement: AtomicBool::new(false),
                malformed_multicall: AtomicBool::new(false),
                delay_multicall: Duration::ZERO,
                active_eth_calls: AtomicU64::new(0),
                maximum_active_eth_calls: AtomicU64::new(0),
            }
        }
    }

    impl ModelClient {
        fn with_delay(delay_multicall: Duration) -> Self {
            Self {
                delay_multicall,
                ..Self::default()
            }
        }

        fn set_head(&self, number: u64, hash: &str) {
            *self.head.lock().unwrap() = PinnedBlock {
                number,
                hash: hash.to_string(),
            };
        }

        fn set_state_root(&self, state_root: &str) {
            *self.state_root.lock().unwrap() = state_root.to_string();
        }

        fn calls(&self) -> Vec<CallRecord> {
            self.calls.lock().unwrap().clone()
        }

        fn rate_limit_next_multicall(&self, provider_id: &str) {
            self.rate_limit_once
                .lock()
                .unwrap()
                .insert(provider_id.to_string());
        }

        fn fail_source_receipt_for(&self, provider_id: &str) {
            self.source_receipt_failure
                .lock()
                .unwrap()
                .insert(provider_id.to_string());
        }

        fn reset_eth_call_concurrency(&self) {
            self.active_eth_calls.store(0, Ordering::Relaxed);
            self.maximum_active_eth_calls.store(0, Ordering::Relaxed);
        }

        fn block_for_tag(&self, tag: &str) -> PinnedBlock {
            let head = self.head.lock().unwrap().clone();
            if tag == "latest" || tag == "finalized" {
                return head;
            }
            let number = u64::from_str_radix(tag.trim_start_matches("0x"), 16).unwrap();
            let hash = if number == head.number {
                head.hash
            } else if number == 100 {
                BLOCK_HASH.to_string()
            } else {
                NEXT_HASH.to_string()
            };
            PinnedBlock { number, hash }
        }

        fn multicall_response(&self, provider_id: &str, params: &Value) -> Value {
            let calldata = params[0]["data"].as_str().unwrap();
            let encoded = hex::decode(calldata.trim_start_matches("0x")).unwrap();
            assert_eq!(&encoded[..4], &[0x82, 0xad, 0x56, 0xcb]);
            let decoded = decode(
                &[ParamType::Array(Box::new(ParamType::Tuple(vec![
                    ParamType::Address,
                    ParamType::Bool,
                    ParamType::Bytes,
                ])))],
                &encoded[4..],
            )
            .unwrap();
            let Token::Array(calls) = &decoded[0] else {
                panic!("aggregate3 call array missing");
            };
            let outputs = calls
                .iter()
                .map(|call| {
                    let Token::Tuple(values) = call else {
                        panic!("aggregate3 call tuple missing");
                    };
                    let Token::Bytes(calldata) = &values[2] else {
                        panic!("aggregate3 inner calldata missing");
                    };
                    let argument_address =
                        || format!("0x{}", hex::encode(&calldata[calldata.len() - 20..]));
                    let output = match calldata.as_slice() {
                        [0x0d, 0xfe, 0x16, 0x81] => address_word(0x33),
                        [0xd2, 0x12, 0x20, 0xa7] => address_word(0x44),
                        [0xdd, 0xca, 0x3f, 0x43] => u32_word(500),
                        [0xd0, 0xc9, 0x3a, 0x7c] => u32_word(10),
                        [0x31, 0x3c, 0xe5, 0x67] => {
                            if values[0].clone().into_address().unwrap().as_bytes()[19] == 0x33 {
                                u32_word(18)
                            } else {
                                u32_word(6)
                            }
                        }
                        [0x38, 0x50, 0xc7, 0xbd] => {
                            let marker = if self.disagreement.load(Ordering::Relaxed)
                                && provider_id == "provider_1"
                            {
                                2
                            } else {
                                1
                            };
                            let mut value = vec![0_u8; 64];
                            value[31] = marker;
                            value
                        }
                        [0x1a, 0x68, 0x65, 0x02] => {
                            let mut value = vec![0_u8; 32];
                            value[31] = 1;
                            value
                        }
                        [0xbf, 0x92, 0x85, 0x7c, ..] => {
                            let mut output = vec![0_u8; 32 * 6];
                            output[31] = 2;
                            output[63] = if self.exact_primary_disagreement.load(Ordering::Relaxed)
                                && matches!(
                                    provider_id,
                                    "provider_0" | "production-nownodes-arbitrum"
                                ) {
                                2
                            } else if self.exact_availability_disagreement.load(Ordering::Relaxed)
                                && provider_id == "availability-slot-1"
                            {
                                3
                            } else if self.exact_disagreement.load(Ordering::Relaxed)
                                && matches!(provider_id, "provider_1" | "production-slot-0")
                            {
                                2
                            } else {
                                1
                            };
                            output[127] = 0x1f;
                            output[128 + 31] = 0x1d;
                            U256::from(900_000_000_000_000_000_u64)
                                .to_big_endian(&mut output[160..192]);
                            output
                        }
                        [0x44, 0x17, 0xa5, 0x83, ..]
                        | [0xed, 0xdf, 0x1b, 0x79, ..]
                        | [0x5c, 0x9a, 0x8b, 0x18, ..] => u32_word(0),
                        [0x07, 0x4b, 0x2e, 0x43] => u32_word(5),
                        [0xd1, 0x94, 0x6d, 0xbc] => encode(&[Token::Array(vec![
                            Token::Address(ARBITRUM_WETH.parse().unwrap()),
                            Token::Address(ARBITRUM_NATIVE_USDC.parse().unwrap()),
                            Token::Address(ARBITRUM_USDC_E.parse().unwrap()),
                        ])]),
                        [0x28, 0xdd, 0x2d, 0x01, ..] => vec![0_u8; 32 * 9],
                        [0xc4, 0x4b, 0x11, 0xf7, ..] => {
                            let decimals = if argument_address() == ARBITRUM_WETH {
                                18
                            } else {
                                6
                            };
                            u256_word(
                                U256::from_dec_str(&aave_configuration(
                                    decimals, 10_500, 1_000, true, false,
                                ))
                                .unwrap(),
                            )
                        }
                        [0xd2, 0x49, 0x3b, 0x6c, ..] => {
                            let mut output = Vec::with_capacity(96);
                            output.extend(address_word(0x11));
                            output.extend(address_word(0x22));
                            output.extend(address_word(0x33));
                            output
                        }
                        [0xb3, 0x59, 0x6f, 0x07, ..] => {
                            let mut output = vec![0_u8; 32];
                            output[31] = 1;
                            output
                        }
                        [0x52, 0x75, 0x17, 0x97, ..] => {
                            if calldata[calldata.len() - 1] == 0 {
                                address_bytes(ARBITRUM_WETH)
                            } else if calldata[calldata.len() - 1] == 1 {
                                address_bytes(ARBITRUM_NATIVE_USDC)
                            } else {
                                address_bytes(ARBITRUM_USDC_E)
                            }
                        }
                        _ => panic!("unexpected inner selector"),
                    };
                    Token::Tuple(vec![Token::Bool(true), Token::Bytes(output)])
                })
                .collect();
            json!(format!(
                "0x{}",
                hex::encode(encode(&[Token::Array(outputs)]))
            ))
        }
    }

    #[async_trait]
    impl JsonRpcClient for ModelClient {
        async fn call(
            &self,
            provider: &ProviderLease,
            method: RpcMethod,
            params: Value,
            _timeout: Duration,
        ) -> Result<RpcCallResult, TransportError> {
            self.calls.lock().unwrap().push(CallRecord {
                provider_id: provider.provider_id().to_string(),
                method,
                params: params.clone(),
            });
            let value = match method {
                RpcMethod::EthChainId => json!(ARBITRUM_CHAIN_ID_HEX),
                RpcMethod::EthGetCode => json!("0x60006000"),
                RpcMethod::EthGetStorageAt => {
                    if params.get(1).and_then(Value::as_str)
                        == Some(PHOENIX_EXECUTOR_MAXIMUM_INPUT_SLOT)
                    {
                        json!(u256_storage_word(U256::from(MAXIMUM_REVIEWED_INPUT_WEI)))
                    } else if params.get(1).and_then(Value::as_str)
                        == Some(EIP1967_IMPLEMENTATION_SLOT)
                    {
                        json!(format!(
                            "0x{}{}",
                            "00".repeat(12),
                            &AAVE_POOL_IMPLEMENTATION_ARBITRUM[2..]
                        ))
                    } else {
                        json!(test_executor_packed_config())
                    }
                }
                RpcMethod::EthGetBlockByNumber => {
                    let tag = params[0].as_str().unwrap();
                    let mut block = self.block_for_tag(tag);
                    if tag == "finalized"
                        && matches!(provider.provider_id(), "provider_1" | "production-slot-0")
                        && self.finalized_disagreement.load(Ordering::Relaxed)
                    {
                        block.number = block.number.saturating_sub(1);
                        block.hash = NEXT_HASH.to_string();
                    }
                    let state_root = self.state_root.lock().unwrap().clone();
                    if block.number == 100 && block.hash == BLOCK_HASH {
                        let mut value = source_block_fixture();
                        value["stateRoot"] = json!(state_root);
                        value
                    } else {
                        json!({
                            "number": format_quantity(block.number),
                            "hash": block.hash,
                            "stateRoot": state_root,
                            "timestamp": "0x1"
                        })
                    }
                }
                RpcMethod::EthGetTransactionByHash => source_transaction_fixture(),
                RpcMethod::EthGetTransactionReceipt => {
                    if self
                        .source_receipt_failure
                        .lock()
                        .unwrap()
                        .contains(provider.provider_id())
                    {
                        return Err(TransportError::Http);
                    }
                    source_receipt_fixture()
                }
                RpcMethod::EthCall => {
                    if self
                        .rate_limit_once
                        .lock()
                        .unwrap()
                        .remove(provider.provider_id())
                    {
                        return Err(TransportError::RateLimited {
                            retry_after: Duration::from_secs(30),
                        });
                    }
                    if !self.delay_multicall.is_zero() {
                        let current = self.active_eth_calls.fetch_add(1, Ordering::Relaxed) + 1;
                        self.maximum_active_eth_calls
                            .fetch_max(current, Ordering::Relaxed);
                        tokio::time::sleep(self.delay_multicall).await;
                        self.active_eth_calls.fetch_sub(1, Ordering::Relaxed);
                    }
                    let destination = params[0]["to"].as_str().unwrap_or_default();
                    if destination.eq_ignore_ascii_case(MULTICALL3_ADDRESS) {
                        if self.malformed_multicall.load(Ordering::Relaxed) {
                            json!("0x1234")
                        } else {
                            self.multicall_response(provider.provider_id(), &params)
                        }
                    } else if destination.eq_ignore_ascii_case(AAVE_V3_POOL_ARBITRUM) {
                        json!(test_u256_hex(U256::from(5)))
                    } else if destination.eq_ignore_ascii_case(ARBITRUM_NODE_INTERFACE) {
                        json!(format!(
                            "0x{}",
                            hex::encode(encode(&[
                                Token::Uint(U256::from(100_000)),
                                Token::Uint(U256::from(20_000)),
                                Token::Uint(U256::from(1)),
                                Token::Uint(U256::from(1)),
                            ]))
                        ))
                    } else {
                        json!(test_u256_hex(U256::from(1_000_000_000_u64)))
                    }
                }
                _ => return Err(TransportError::InvalidResponse),
            };
            Ok(RpcCallResult {
                value,
                latency_ns: 100,
            })
        }
    }

    fn address_word(byte: u8) -> Vec<u8> {
        let mut value = vec![0_u8; 32];
        value[12..].fill(byte);
        value
    }

    fn u32_word(value: u32) -> Vec<u8> {
        let mut word = vec![0_u8; 32];
        word[28..].copy_from_slice(&value.to_be_bytes());
        word
    }

    fn u256_word(value: U256) -> Vec<u8> {
        let mut word = vec![0_u8; 32];
        value.to_big_endian(&mut word);
        word
    }

    fn address_bytes(value: &str) -> Vec<u8> {
        let decoded = hex::decode(value.trim_start_matches("0x")).unwrap();
        let mut word = vec![0_u8; 32];
        word[12..].copy_from_slice(&decoded);
        word
    }

    fn test_u256_hex(value: U256) -> String {
        let mut word = [0_u8; 32];
        value.to_big_endian(&mut word);
        format!("0x{}", hex::encode(word))
    }

    fn test_executor_packed_config() -> String {
        format!("0x{}00{}", "00".repeat(11), &AAVE_V3_POOL_ARBITRUM[2..])
    }

    fn request() -> ShadowStateRequest {
        ShadowStateRequest {
            schema_version: SHADOW_STATE_SCHEMA_VERSION.to_string(),
            chain_id: ARBITRUM_ONE_CHAIN_ID,
            route_fingerprint: "route-v1".to_string(),
            pools: vec![
                PoolStateRequest {
                    pool_id: "pool-a".to_string(),
                    address: "0x1111111111111111111111111111111111111111".to_string(),
                    protocol: "UniswapV3".to_string(),
                    token0: "0x3333333333333333333333333333333333333333".to_string(),
                    token1: "0x4444444444444444444444444444444444444444".to_string(),
                    token0_decimals: 18,
                    token1_decimals: 6,
                    fee: 500,
                    tick_spacing: 10,
                },
                PoolStateRequest {
                    pool_id: "pool-b".to_string(),
                    address: "0x2222222222222222222222222222222222222222".to_string(),
                    protocol: "SushiSwapV3".to_string(),
                    token0: "0x3333333333333333333333333333333333333333".to_string(),
                    token1: "0x4444444444444444444444444444444444444444".to_string(),
                    token0_decimals: 18,
                    token1_decimals: 6,
                    fee: 500,
                    tick_spacing: 10,
                },
            ],
            evidence: EvidenceRequest::Primary,
        }
    }

    fn verification_request(
        primary_request: &ShadowStateRequest,
        primary_response: &ShadowStateResponse,
    ) -> ShadowStateRequest {
        let mut request = primary_request.clone();
        request.evidence = EvidenceRequest::Verify {
            block_number: primary_response.block_number,
            block_hash: primary_response.block_hash.clone(),
            primary_state_hash: primary_response.state_hash.clone(),
        };
        request
    }

    fn runtime(client: Arc<ModelClient>) -> GatewayRuntime {
        runtime_with_limits(
            client,
            GatewayLimits {
                state_requests_per_minute: 1_000,
                upstream_calls_per_second: 1_000,
                upstream_call_burst: 1_000,
            },
        )
    }

    fn runtime_with_limits(client: Arc<ModelClient>, limits: GatewayLimits) -> GatewayRuntime {
        let mut config =
            parse_provider_config("https://primary.example,https://secondary.example", "2,1")
                .unwrap();
        config.providers[0].name = AAVE_PRIMARY_PROVIDER_ID.to_string();
        GatewayRuntime::with_limits(
            config,
            client,
            MethodTimeouts {
                eth_call: Duration::from_secs(2),
                state_read: Duration::from_secs(2),
                logs: Duration::from_secs(5),
            },
            RuntimeRpcMetrics::default(),
            GatewayReadiness::new(true),
            limits,
        )
    }

    async fn mark_test_providers_verified(runtime: &GatewayRuntime) {
        runtime.chain_verified.lock().await.extend([
            AAVE_PRIMARY_PROVIDER_ID.to_string(),
            "provider_1".to_string(),
        ]);
        runtime.multicall_verified.lock().await.extend([
            AAVE_PRIMARY_PROVIDER_ID.to_string(),
            "provider_1".to_string(),
        ]);
    }

    fn aave_screen_request() -> AaveScreenRequest {
        AaveScreenRequest {
            schema_version: "phoenix.rpc.aave-screen-request.v1".to_string(),
            chain_id: ARBITRUM_ONE_CHAIN_ID,
            request_id: "aave-screen-test".to_string(),
            borrowers: vec!["0x1111111111111111111111111111111111111111".to_string()],
        }
    }

    fn aave_exact_request(suffix: &str) -> AaveExactRequest {
        AaveExactRequest {
            schema_version: crate::aave_state::AAVE_EXACT_REQUEST_SCHEMA.to_string(),
            chain_id: ARBITRUM_ONE_CHAIN_ID,
            request_id: format!("aave-exact-{suffix}"),
            borrower: "0x4444444444444444444444444444444444444444".to_string(),
            maximum_input_amount: MAXIMUM_REVIEWED_INPUT_WEI.to_string(),
        }
    }

    fn aave_simulation_batch_request(size: usize, suffix: &str) -> AaveSimulateBatchRequest {
        let executor_code_hash = canonical_hash_bytes(&hex::decode("60006000").unwrap());
        let deadline = unix_time_seconds() + 60;
        let simulations = (0..size)
            .map(|index| AaveSimulateRequest {
                schema_version: crate::aave_state::AAVE_SIMULATE_REQUEST_SCHEMA.to_string(),
                chain_id: ARBITRUM_ONE_CHAIN_ID,
                request_id: format!("aave-batch-{suffix}-{index}"),
                block_number: 100,
                block_hash: BLOCK_HASH.to_string(),
                state_root: format!("0x{}", "d".repeat(64)),
                executor_address: "0x2222222222222222222222222222222222222222".to_string(),
                executor_code_hash: executor_code_hash.clone(),
                caller_address: "0x3333333333333333333333333333333333333333".to_string(),
                release_sha: "4".repeat(40),
                borrower: "0x4444444444444444444444444444444444444444".to_string(),
                debt_asset: ARBITRUM_WETH.to_string(),
                collateral_asset: ARBITRUM_WETH.to_string(),
                debt_asset_decimals: 18,
                debt_asset_price_base: "200000000000".to_string(),
                weth_price_base: "200000000000".to_string(),
                repay_amount: ((index + 1) * 1_000).to_string(),
                maximum_input_amount: "10000".to_string(),
                live_maximum_input_amount: "10000".to_string(),
                maximum_input_weth_wei: "10000".to_string(),
                live_maximum_input_weth_wei: "10000".to_string(),
                counterfactual: false,
                minimum_collateral_received: "1000".to_string(),
                minimum_unwind_output: "1".to_string(),
                minimum_profit: "1".to_string(),
                minimum_profit_weth_wei: "1".to_string(),
                expected_profit: "1000000000".to_string(),
                retained_profit_floor: "1".to_string(),
                selected_pool: "0x0000000000000000000000000000000000000000".to_string(),
                selected_factory: "0x0000000000000000000000000000000000000000".to_string(),
                selected_fee: 0,
                zero_for_one: false,
                gas_limit: 200_000,
                max_fee_per_gas: "10".to_string(),
                max_priority_fee_per_gas: "1".to_string(),
                deadline_unix_seconds: deadline,
                atlas_mode: false,
                atlas_bid: "0".to_string(),
            })
            .collect();
        AaveSimulateBatchRequest {
            schema_version: crate::aave_state::AAVE_SIMULATE_BATCH_REQUEST_SCHEMA.to_string(),
            chain_id: ARBITRUM_ONE_CHAIN_ID,
            request_id: format!("aave-batch-{suffix}"),
            simulations,
        }
    }

    #[tokio::test]
    async fn aave_exact_uses_only_the_named_primary_and_reuses_static_context() {
        let client = Arc::new(ModelClient::default());
        let mut config = ProviderConfig {
            providers: Vec::new(),
        };
        crate::providers::append_header_authenticated_provider(
            &mut config,
            AAVE_PRIMARY_PROVIDER_ID,
            "https://primary.example",
            100,
            "api-key",
            "test-only",
        )
        .unwrap();
        let runtime = GatewayRuntime::with_limits(
            config,
            client.clone(),
            MethodTimeouts {
                eth_call: Duration::from_secs(2),
                state_read: Duration::from_secs(2),
                logs: Duration::from_secs(5),
            },
            RuntimeRpcMetrics::default(),
            GatewayReadiness::new(true),
            GatewayLimits {
                state_requests_per_minute: 1_000,
                upstream_calls_per_second: 1_000,
                upstream_call_burst: 1_000,
            },
        );
        mark_test_providers_verified(&runtime).await;

        let first = runtime
            .resolve_aave_exact(aave_exact_request("cold"))
            .await
            .unwrap();
        let cold_calls = client.calls();
        assert_eq!(
            runtime.aave_exact_static_context_cache.lock().await.len(),
            1
        );
        assert!(cold_calls
            .iter()
            .all(|call| call.provider_id == AAVE_PRIMARY_PROVIDER_ID));
        assert_eq!(first.primary.provider_id, AAVE_PRIMARY_PROVIDER_ID);
        assert_eq!(first.confirmation, None);
        assert_eq!(first.quorum, 1);

        let second = runtime
            .resolve_aave_exact(aave_exact_request("warm"))
            .await
            .unwrap();
        let all_calls = client.calls();
        let warm_calls = &all_calls[cold_calls.len()..];
        assert!(warm_calls.iter().all(|call| !matches!(
            call.method,
            RpcMethod::EthGetCode | RpcMethod::EthGetStorageAt
        )));
        assert!(all_calls
            .iter()
            .all(|call| call.provider_id == AAVE_PRIMARY_PROVIDER_ID));
        assert_eq!(first.block_number, second.block_number);
        assert_eq!(first.block_hash, second.block_hash);
        assert_eq!(first.state_root, second.state_root);
    }

    #[tokio::test]
    async fn aave_exact_lane_is_bounded_and_work_conserving() {
        let client = Arc::new(ModelClient::with_delay(Duration::from_millis(20)));
        let runtime = runtime(client.clone());
        mark_test_providers_verified(&runtime).await;
        runtime
            .resolve_aave_exact(aave_exact_request("warm-concurrency"))
            .await
            .unwrap();
        client.reset_eth_call_concurrency();

        let mut tasks = Vec::new();
        for index in 0..24_u8 {
            let runtime = runtime.clone();
            tasks.push(tokio::spawn(async move {
                let mut request = aave_exact_request(&format!("concurrent-{index}"));
                request.borrower = format!("0x{:040x}", u64::from(index) + 1);
                runtime.resolve_aave_exact(request).await
            }));
        }
        for task in tasks {
            let response = task.await.unwrap().unwrap();
            assert_eq!(response.primary.provider_id, AAVE_PRIMARY_PROVIDER_ID);
            assert_eq!(response.confirmation, None);
            assert_eq!(response.quorum, 1);
        }

        let maximum = client.maximum_active_eth_calls.load(Ordering::Relaxed);
        assert_eq!(maximum, AAVE_EXACT_OPERATION_CONCURRENCY as u64);
        let metrics = runtime.metrics.render(&runtime.readiness);
        assert!(metrics.contains("rpc_aave_exact_operations_in_flight 0"));
        assert!(metrics.contains("rpc_aave_exact_operation_queue_seconds_count 25"));
    }

    #[tokio::test]
    async fn aave_exact_static_context_misses_on_each_pinned_identity_component() {
        for identity in ["number", "hash", "state_root"] {
            let client = Arc::new(ModelClient::default());
            let runtime = runtime(client.clone());
            mark_test_providers_verified(&runtime).await;
            runtime
                .resolve_aave_exact(aave_exact_request("initial"))
                .await
                .unwrap();
            let initial_call_count = client.calls().len();
            match identity {
                "number" => client.set_head(101, BLOCK_HASH),
                "hash" => client.set_head(100, REORG_HASH),
                "state_root" => client.set_state_root(&format!("0x{}", "e".repeat(64))),
                _ => unreachable!(),
            }

            runtime
                .resolve_aave_exact(aave_exact_request(identity))
                .await
                .unwrap();
            let all_calls = client.calls();
            let miss_calls = &all_calls[initial_call_count..];
            assert_eq!(miss_calls.len(), 7, "identity={identity}");
            assert_eq!(
                miss_calls
                    .iter()
                    .filter(|call| call.method == RpcMethod::EthGetCode)
                    .count(),
                2,
                "identity={identity}"
            );
            assert_eq!(
                miss_calls
                    .iter()
                    .filter(|call| call.method == RpcMethod::EthGetStorageAt)
                    .count(),
                1,
                "identity={identity}"
            );
            assert_eq!(
                multicall_inner_counts(miss_calls),
                vec![1, 22],
                "identity={identity}"
            );
        }
    }

    #[tokio::test]
    async fn secondary_exact_disagreement_does_not_affect_single_primary_cache() {
        let client = Arc::new(ModelClient::default());
        let runtime = runtime(client.clone());
        mark_test_providers_verified(&runtime).await;
        client.exact_disagreement.store(true, Ordering::Relaxed);

        let response = runtime
            .resolve_aave_exact(aave_exact_request("secondary-noise"))
            .await
            .unwrap();
        assert_eq!(response.primary.provider_id, AAVE_PRIMARY_PROVIDER_ID);
        assert_eq!(response.confirmation, None);
        assert_eq!(response.quorum, 1);
        assert_eq!(
            runtime.aave_exact_static_context_cache.lock().await.len(),
            1
        );
        let first_call_count = client.calls().len();
        runtime
            .resolve_aave_exact(aave_exact_request("retry"))
            .await
            .unwrap();
        let all_calls = client.calls();
        assert_eq!(all_calls[first_call_count..].len(), 3);
        assert_eq!(
            runtime.aave_exact_static_context_cache.lock().await.len(),
            1
        );
    }

    #[tokio::test]
    async fn production_limits_pace_eight_item_batch_without_order_bias_and_preserve_pins() {
        let client = Arc::new(ModelClient::default());
        let limits = GatewayLimits {
            state_requests_per_minute: 12,
            upstream_calls_per_second: 1,
            upstream_call_burst: 16,
        };
        let mut runtime = runtime_with_limits(client.clone(), limits);
        // Preserve the production token ratios while accelerating the refill
        // period so this regression does not spend a minute in wall time.
        let accelerated_refill = Duration::from_millis(1);
        runtime.upstream_budget = Arc::new(Mutex::new(GlobalBudget::new(
            limits.upstream_call_burst,
            limits.upstream_calls_per_second,
            accelerated_refill,
            Instant::now(),
        )));
        runtime.upstream_refill_interval = accelerated_refill;

        let first = runtime
            .simulate_aave_liquidations_batch(aave_simulation_batch_request(8, "cold"))
            .await
            .unwrap();
        assert_eq!(first.results.len(), 8);
        assert!(first
            .results
            .iter()
            .all(|result| result.response.is_some() && result.error.is_none()));
        let cold_calls = client.calls();
        assert_eq!(cold_calls.len(), 24);
        assert_eq!(
            cold_calls
                .iter()
                .filter(|call| call.method == RpcMethod::EthGetBlockByNumber)
                .count(),
            2
        );
        assert_eq!(
            cold_calls
                .iter()
                .filter(|call| call.method == RpcMethod::EthGetStorageAt)
                .count(),
            2
        );

        let second = runtime
            .simulate_aave_liquidations_batch(aave_simulation_batch_request(8, "warm"))
            .await
            .unwrap();
        assert!(second
            .results
            .iter()
            .all(|result| result.response.is_some() && result.error.is_none()));
        let all_calls = client.calls();
        let warm_calls = &all_calls[cold_calls.len()..];
        assert_eq!(warm_calls.len(), 18);
        assert_eq!(
            warm_calls
                .iter()
                .filter(|call| call.method == RpcMethod::EthGetBlockByNumber)
                .count(),
            2,
            "a context-cache hit must still verify the pin before and after"
        );
        assert_eq!(
            warm_calls
                .iter()
                .filter(|call| call.method == RpcMethod::EthGetStorageAt)
                .count(),
            0,
            "warm static context was unexpectedly fetched again"
        );
        let metrics = runtime.metrics.render(&runtime.readiness);
        assert!(metrics.contains("rpc_aave_fork_operations_in_flight 0"));
        assert!(metrics.contains("rpc_aave_fork_operation_queue_seconds_count 2"));
    }

    fn multicall_inner_counts(calls: &[CallRecord]) -> Vec<usize> {
        calls
            .iter()
            .filter(|call| call.method == RpcMethod::EthCall)
            .map(|call| {
                let calldata = call.params[0]["data"].as_str().unwrap();
                let encoded = hex::decode(calldata.trim_start_matches("0x")).unwrap();
                let decoded = decode(
                    &[ParamType::Array(Box::new(ParamType::Tuple(vec![
                        ParamType::Address,
                        ParamType::Bool,
                        ParamType::Bytes,
                    ])))],
                    &encoded[4..],
                )
                .unwrap();
                match &decoded[0] {
                    Token::Array(values) => values.len(),
                    _ => 0,
                }
            })
            .collect()
    }

    #[tokio::test]
    async fn aave_screen_fits_the_reviewed_warm_four_call_burst() {
        let client = Arc::new(ModelClient::default());
        let runtime = runtime_with_limits(
            client.clone(),
            GatewayLimits {
                state_requests_per_minute: 100,
                upstream_calls_per_second: 1,
                upstream_call_burst: 4,
            },
        );
        mark_test_providers_verified(&runtime).await;

        let response = runtime
            .resolve_aave_screen(aave_screen_request())
            .await
            .unwrap();

        assert_eq!(response.block_number, 100);
        assert_eq!(response.block_hash, BLOCK_HASH);
        let calls = client.calls();
        assert_eq!(calls.len(), 2);
        assert_eq!(
            calls
                .iter()
                .filter(|call| call.method == RpcMethod::EthGetBlockByNumber)
                .count(),
            1
        );
        assert_eq!(
            calls
                .iter()
                .filter(|call| call.method == RpcMethod::EthCall)
                .count(),
            1
        );
    }

    #[tokio::test]
    async fn cold_single_primary_aave_screen_atomically_admits_four_calls() {
        let client = Arc::new(ModelClient::default());
        let runtime = runtime_with_limits(
            client.clone(),
            GatewayLimits {
                state_requests_per_minute: 100,
                upstream_calls_per_second: 1,
                upstream_call_burst: 4,
            },
        );

        let response = runtime
            .resolve_aave_screen(aave_screen_request())
            .await
            .unwrap();

        assert_eq!(response.block_number, 100);
        assert_eq!(response.block_hash, BLOCK_HASH);
        let calls = client.calls();
        assert_eq!(calls.len(), 4);
        assert_eq!(
            calls
                .iter()
                .filter(|call| call.method == RpcMethod::EthChainId)
                .count(),
            1
        );
        assert_eq!(
            calls
                .iter()
                .filter(|call| call.method == RpcMethod::EthGetCode)
                .count(),
            1
        );
        assert_eq!(
            calls
                .iter()
                .filter(|call| call.method == RpcMethod::EthGetBlockByNumber)
                .count(),
            1
        );
        assert_eq!(
            calls
                .iter()
                .filter(|call| call.method == RpcMethod::EthCall)
                .count(),
            1
        );
    }

    #[tokio::test]
    async fn aave_screen_budget_rejection_does_not_consume_a_partial_sequence() {
        let client = Arc::new(ModelClient::default());
        let runtime = runtime_with_limits(
            client.clone(),
            GatewayLimits {
                state_requests_per_minute: 100,
                upstream_calls_per_second: 1,
                upstream_call_burst: 3,
            },
        );

        assert_eq!(
            runtime.resolve_aave_screen(aave_screen_request()).await,
            Err(GatewayError::UpstreamBudgetExhausted)
        );
        assert!(client.calls().is_empty());
    }

    #[tokio::test]
    async fn secondary_finalized_head_noise_does_not_affect_single_primary_screen() {
        let client = Arc::new(ModelClient::default());
        client.finalized_disagreement.store(true, Ordering::Relaxed);
        let runtime = runtime(client);
        mark_test_providers_verified(&runtime).await;

        let response = runtime
            .resolve_aave_screen(aave_screen_request())
            .await
            .unwrap();
        assert_eq!(response.primary.provider_id, AAVE_PRIMARY_PROVIDER_ID);
        assert_eq!(response.confirmation, None);
        assert_eq!(response.quorum, 1);
    }

    #[tokio::test]
    async fn two_pool_primary_uses_one_multicall_and_caches_static_metadata() {
        let client = Arc::new(ModelClient::default());
        let runtime = runtime(client.clone());
        let state_request = request();
        let expected_route_hash = state_request.route_config_hash().unwrap();
        let primary = runtime.resolve_shadow_state(state_request).await.unwrap();
        assert_eq!(primary.verification_status, VerificationStatus::PrimaryOnly);
        assert_eq!(
            primary.independent_verification_status,
            IndependentVerificationStatus::NotRequested
        );
        assert_eq!(primary.route_config_hash, expected_route_hash);
        assert!(!primary.provider_agreement);
        let calls = client.calls();
        assert_eq!(multicall_inner_counts(&calls), vec![16]);
        assert_eq!(
            calls
                .iter()
                .filter(|call| call.method == RpcMethod::EthCall)
                .count(),
            1
        );
        assert!(calls
            .iter()
            .filter(|call| call.method == RpcMethod::EthCall)
            .all(|call| call.params[0]["to"] == MULTICALL3_ADDRESS));

        client.set_head(101, NEXT_HASH);
        runtime
            .update_head(HeadSnapshot {
                provider_id: "provider_0".to_string(),
                block: PinnedBlock {
                    number: 101,
                    hash: NEXT_HASH.to_string(),
                },
                observed_at: Instant::now(),
            })
            .await;
        runtime.resolve_shadow_state(request()).await.unwrap();
        let calls = client.calls();
        assert_eq!(multicall_inner_counts(&calls), vec![16, 4]);
        assert_eq!(
            calls
                .iter()
                .filter(|call| call.method == RpcMethod::EthChainId)
                .count(),
            1
        );
        assert_eq!(
            calls
                .iter()
                .filter(|call| call.method == RpcMethod::EthGetCode)
                .count(),
            1
        );
    }

    #[tokio::test]
    async fn promising_route_uses_one_secondary_and_regresses_the_old_twenty_six_calls() {
        let client = Arc::new(ModelClient::default());
        let runtime = runtime(client.clone());
        let state_request = request();
        let primary = runtime
            .resolve_shadow_state(state_request.clone())
            .await
            .unwrap();
        let verified = runtime
            .resolve_shadow_state(verification_request(&state_request, &primary))
            .await
            .unwrap();
        assert_eq!(verified.verification_status, VerificationStatus::Agreed);
        assert_eq!(
            verified.independent_verification_status,
            IndependentVerificationStatus::Agreed
        );
        assert!(verified.provider_agreement);
        assert_ne!(
            verified.primary_provider_id,
            verified.agreement_provider_id.clone().unwrap()
        );
        assert_eq!(
            verified.secondary_state_hash.as_deref(),
            Some(verified.state_hash.as_str())
        );
        assert_eq!(verified.block_number, 100);
        assert_eq!(verified.block_hash, BLOCK_HASH);
        assert_eq!(verified.secondary_block_number, Some(verified.block_number));
        assert_eq!(
            verified.secondary_block_hash.as_deref(),
            Some(verified.block_hash.as_str())
        );
        assert_eq!(
            verified.secondary_route_config_hash.as_deref(),
            Some(verified.route_config_hash.as_str())
        );
        let calls = client.calls();
        assert_eq!(multicall_inner_counts(&calls), vec![16, 16]);
        assert_eq!(calls.len(), 9, "cold path must stay below the old 26 calls");
        assert_eq!(
            calls
                .iter()
                .filter(|call| call.method == RpcMethod::EthCall)
                .count(),
            2
        );
        assert!(calls
            .iter()
            .filter(|call| call.method == RpcMethod::EthCall)
            .all(|call| call.params[1] == "0x64"));
        let rendered = runtime.metrics().render(&runtime.readiness());
        assert!(rendered.contains("rpc_primary_success_total 1"));
        assert!(rendered.contains("rpc_secondary_requested_total 1"));
        assert!(rendered.contains("rpc_secondary_agreed_total 1"));
    }

    #[tokio::test]
    async fn route_block_cache_hit_performs_zero_upstream_calls() {
        let client = Arc::new(ModelClient::default());
        let runtime = runtime(client.clone());
        let first = runtime.resolve_shadow_state(request()).await.unwrap();
        let call_count = client.calls().len();
        let cached = runtime.resolve_shadow_state(request()).await.unwrap();
        assert_eq!(cached.state_hash, first.state_hash);
        assert_eq!(client.calls().len(), call_count);
        assert!(runtime
            .metrics()
            .render(&runtime.readiness())
            .contains("rpc_route_block_cache_hits_total 1"));
    }

    #[tokio::test]
    async fn concurrent_identical_primary_reads_are_single_flight_coalesced() {
        let client = Arc::new(ModelClient::with_delay(Duration::from_millis(20)));
        let runtime = runtime(client.clone());
        let state_request = request();
        let (first, second) = tokio::join!(
            runtime.resolve_shadow_state(state_request.clone()),
            runtime.resolve_shadow_state(state_request)
        );
        assert_eq!(first.unwrap().state_hash, second.unwrap().state_hash);
        assert_eq!(
            client
                .calls()
                .iter()
                .filter(|call| call.method == RpcMethod::EthCall)
                .count(),
            1
        );
        assert!(runtime
            .metrics()
            .render(&runtime.readiness())
            .contains("rpc_coalesced_requests_total 1"));
    }

    #[tokio::test]
    async fn request_and_transport_budgets_are_enforced_independently() {
        let request_limited_client = Arc::new(ModelClient::default());
        let request_limited = runtime_with_limits(
            request_limited_client,
            GatewayLimits {
                state_requests_per_minute: 1,
                upstream_calls_per_second: 100,
                upstream_call_burst: 100,
            },
        );
        request_limited
            .resolve_shadow_state(request())
            .await
            .unwrap();
        assert_eq!(
            request_limited.resolve_shadow_state(request()).await,
            Err(GatewayError::RequestBudgetExhausted)
        );

        let upstream_limited_client = Arc::new(ModelClient::default());
        let upstream_limited = runtime_with_limits(
            upstream_limited_client.clone(),
            GatewayLimits {
                state_requests_per_minute: 100,
                upstream_calls_per_second: 1,
                upstream_call_burst: 1,
            },
        );
        upstream_limited.readiness().set_provider_healthy(true);
        assert_eq!(
            upstream_limited.resolve_shadow_state(request()).await,
            Err(GatewayError::UpstreamBudgetExhausted)
        );
        assert!(upstream_limited.readiness().ready().is_ok());
        assert!(upstream_limited_client.calls().is_empty());
        let rendered = upstream_limited
            .metrics()
            .render(&upstream_limited.readiness());
        assert!(rendered.contains("rpc_upstream_call_budget_rejected_total 1"));
    }

    #[tokio::test]
    async fn default_budget_cold_path_retries_without_repeating_partial_calls() {
        let client = Arc::new(ModelClient::default());
        let runtime = runtime_with_limits(client.clone(), GatewayLimits::default());
        assert_eq!(
            runtime.resolve_shadow_state(request()).await,
            Err(GatewayError::UpstreamBudgetExhausted)
        );
        let initial_calls = client.calls();
        assert_eq!(initial_calls.len(), 3);
        assert!(initial_calls
            .iter()
            .all(|call| call.method != RpcMethod::EthCall));

        tokio::time::sleep(Duration::from_millis(1_050)).await;
        let response = runtime.resolve_shadow_state(request()).await.unwrap();
        assert_eq!(
            response.verification_status,
            VerificationStatus::PrimaryOnly
        );
        let calls = client.calls();
        assert_eq!(calls.len(), 5);
        assert_eq!(multicall_inner_counts(&calls), vec![16]);
    }

    #[tokio::test]
    async fn provider_probes_are_transport_budgeted() {
        let client = Arc::new(ModelClient::default());
        let runtime = runtime_with_limits(
            client.clone(),
            GatewayLimits {
                state_requests_per_minute: 100,
                upstream_calls_per_second: 1,
                upstream_call_burst: 1,
            },
        );
        assert_eq!(
            runtime.probe().await,
            Err(GatewayError::UpstreamBudgetExhausted)
        );
        assert!(client.calls().is_empty());
        let rendered = runtime.metrics().render(&runtime.readiness());
        assert!(rendered.contains("rpc_probe_calls_total 0"));
        assert!(rendered.contains("rpc_upstream_call_budget_rejected_total 1"));
    }

    #[tokio::test]
    async fn http_429_cools_provider_and_fails_over_without_same_provider_retry() {
        let client = Arc::new(ModelClient::default());
        client.rate_limit_next_multicall(AAVE_PRIMARY_PROVIDER_ID);
        let runtime = runtime(client.clone());
        let response = runtime.resolve_shadow_state(request()).await.unwrap();
        assert_eq!(response.primary_provider_id, "provider_1");
        let calls = client.calls();
        assert_eq!(
            calls
                .iter()
                .filter(|call| {
                    call.provider_id == AAVE_PRIMARY_PROVIDER_ID
                        && call.method == RpcMethod::EthCall
                })
                .count(),
            1
        );
        let rendered = runtime.metrics().render(&runtime.readiness());
        assert!(rendered.contains("rpc_provider_rate_limited_total 1"));
        assert!(rendered.contains("rpc_provider_cooldown_total 1"));
        assert!(!rendered.contains("primary.example"));
        assert!(!rendered.contains("secondary.example"));
    }

    #[tokio::test]
    async fn same_block_provider_disagreement_is_explicitly_fail_closed() {
        let client = Arc::new(ModelClient::default());
        let runtime = runtime(client.clone());
        let state_request = request();
        let primary = runtime
            .resolve_shadow_state(state_request.clone())
            .await
            .unwrap();
        client.disagreement.store(true, Ordering::Relaxed);
        let verified = runtime
            .resolve_shadow_state(verification_request(&state_request, &primary))
            .await
            .unwrap();
        assert_eq!(verified.verification_status, VerificationStatus::Disagreed);
        assert_eq!(
            verified.independent_verification_status,
            IndependentVerificationStatus::Disagreed
        );
        assert!(!verified.provider_agreement);
        assert!(verified.secondary_state_hash.is_some());
        assert_ne!(
            verified.secondary_state_hash.as_deref(),
            Some(verified.state_hash.as_str())
        );
        assert!(verified
            .quality
            .iter()
            .filter(|quality| quality.success)
            .all(|quality| quality.disagreement));
        let rendered = runtime.metrics().render(&runtime.readiness());
        assert!(rendered.contains("rpc_secondary_requested_total 1"));
        assert!(rendered.contains("rpc_secondary_disagreed_total 1"));
        assert!(rendered.contains("rpc_provider_disagreement_total 1"));
    }

    #[tokio::test]
    async fn secondary_integrity_failures_are_distinct_from_provider_unavailability() {
        let client = Arc::new(ModelClient::default());
        let runtime = runtime(client.clone());
        let state_request = request();
        let primary = runtime
            .resolve_shadow_state(state_request.clone())
            .await
            .unwrap();
        client.malformed_multicall.store(true, Ordering::Relaxed);
        let verified = runtime
            .resolve_shadow_state(verification_request(&state_request, &primary))
            .await
            .unwrap();
        assert_eq!(
            verified.verification_status,
            VerificationStatus::SecondaryUnavailable
        );
        assert_eq!(
            verified.independent_verification_status,
            IndependentVerificationStatus::IntegrityFailure
        );
        assert!(verified.agreement_provider_id.is_none());
        assert!(verified.secondary_state_hash.is_none());
        assert!(verified.secondary_block_number.is_none());
        assert!(verified.secondary_block_hash.is_none());
        assert!(verified.secondary_route_config_hash.is_none());
        assert!(!verified.provider_agreement);
        assert!(runtime
            .metrics()
            .render(&runtime.readiness())
            .contains("rpc_secondary_unavailable_total 1"));
    }

    #[tokio::test]
    async fn secondary_transport_exhaustion_reports_provider_unavailable() {
        let client = Arc::new(ModelClient::default());
        let runtime = runtime(client.clone());
        let state_request = request();
        let primary = runtime
            .resolve_shadow_state(state_request.clone())
            .await
            .unwrap();
        client.rate_limit_next_multicall("provider_1");
        let verified = runtime
            .resolve_shadow_state(verification_request(&state_request, &primary))
            .await
            .unwrap();
        assert_eq!(
            verified.independent_verification_status,
            IndependentVerificationStatus::ProviderUnavailable
        );
        assert_eq!(
            verified.verification_status,
            VerificationStatus::SecondaryUnavailable
        );
        assert!(verified.agreement_provider_id.is_none());
        assert!(!verified.provider_agreement);
    }

    #[tokio::test]
    async fn malformed_multicall_output_never_produces_state() {
        let client = Arc::new(ModelClient::default());
        client.malformed_multicall.store(true, Ordering::Relaxed);
        let runtime = runtime(client);
        assert_eq!(
            runtime.resolve_shadow_state(request()).await,
            Err(GatewayError::ProviderUnavailable)
        );
        assert!(runtime.readiness().ready().is_err());
    }

    #[tokio::test]
    async fn canonical_hash_change_invalidates_same_number_route_cache() {
        let client = Arc::new(ModelClient::default());
        let runtime = runtime(client.clone());
        let first = runtime.resolve_shadow_state(request()).await.unwrap();
        assert_eq!(first.block_hash, BLOCK_HASH);
        client.set_head(100, REORG_HASH);
        runtime
            .update_head(HeadSnapshot {
                provider_id: "provider_0".to_string(),
                block: PinnedBlock {
                    number: 100,
                    hash: REORG_HASH.to_string(),
                },
                observed_at: Instant::now(),
            })
            .await;
        let second = runtime.resolve_shadow_state(request()).await.unwrap();
        assert_eq!(second.block_hash, REORG_HASH);
        assert_eq!(multicall_inner_counts(&client.calls()), vec![16, 4]);
    }

    #[tokio::test]
    async fn route_configuration_hash_change_forces_static_revalidation() {
        let client = Arc::new(ModelClient::default());
        let runtime = runtime(client.clone());
        runtime.resolve_shadow_state(request()).await.unwrap();
        let mut changed = request();
        changed.route_fingerprint = "route-v2".to_string();
        runtime.resolve_shadow_state(changed).await.unwrap();
        assert_eq!(multicall_inner_counts(&client.calls()), vec![16, 16]);
    }

    #[tokio::test]
    async fn real_loopback_json_rpc_executes_the_multicall_primary_path() {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let model = Arc::new(ModelClient::default());
        let server_model = model.clone();
        let server = tokio::spawn(async move {
            for _ in 0..5 {
                let (mut stream, _) = listener.accept().await.unwrap();
                let mut bytes = Vec::new();
                let (body_start, content_length) = loop {
                    let mut chunk = [0_u8; 4096];
                    let read = stream.read(&mut chunk).await.unwrap();
                    assert!(read > 0);
                    bytes.extend_from_slice(&chunk[..read]);
                    let Some(header_end) = bytes.windows(4).position(|value| value == b"\r\n\r\n")
                    else {
                        continue;
                    };
                    let body_start = header_end + 4;
                    let headers = std::str::from_utf8(&bytes[..header_end]).unwrap();
                    let content_length = headers
                        .lines()
                        .find_map(|line| {
                            let (name, value) = line.split_once(':')?;
                            name.eq_ignore_ascii_case("content-length")
                                .then(|| value.trim().parse::<usize>().ok())
                                .flatten()
                        })
                        .unwrap();
                    if bytes.len() >= body_start + content_length {
                        break (body_start, content_length);
                    }
                };
                let request: Value =
                    serde_json::from_slice(&bytes[body_start..body_start + content_length])
                        .unwrap();
                let method = request["method"].as_str().unwrap();
                let params = &request["params"];
                let result = match method {
                    "eth_chainId" => json!(ARBITRUM_CHAIN_ID_HEX),
                    "eth_getCode" => json!("0x60006000"),
                    "eth_getBlockByNumber" => {
                        json!({"number": "0x64", "hash": BLOCK_HASH})
                    }
                    "eth_call" => server_model.multicall_response("provider_0", params),
                    _ => panic!("unexpected loopback method"),
                };
                let body = serde_json::to_vec(&json!({
                    "jsonrpc": "2.0",
                    "id": 1,
                    "result": result
                }))
                .unwrap();
                let headers = format!(
                    "HTTP/1.1 200 OK\r\ncontent-type: application/json\r\ncontent-length: {}\r\nconnection: close\r\n\r\n",
                    body.len()
                );
                stream.write_all(headers.as_bytes()).await.unwrap();
                stream.write_all(&body).await.unwrap();
            }
        });
        let config = parse_provider_config(&format!("http://{address}"), "1").unwrap();
        let runtime = GatewayRuntime::with_limits(
            config,
            Arc::new(crate::transport::ReqwestJsonRpcClient::new().unwrap()),
            MethodTimeouts {
                eth_call: Duration::from_secs(2),
                state_read: Duration::from_secs(2),
                logs: Duration::from_secs(5),
            },
            RuntimeRpcMetrics::default(),
            GatewayReadiness::new(true),
            GatewayLimits {
                state_requests_per_minute: 100,
                upstream_calls_per_second: 100,
                upstream_call_burst: 100,
            },
        );
        let response = runtime.resolve_shadow_state(request()).await.unwrap();
        assert_eq!(
            response.verification_status,
            VerificationStatus::PrimaryOnly
        );
        assert_eq!(response.pools.len(), 2);
        server.await.unwrap();
    }

    fn source_evidence_request_fixture() -> SourceEvidenceRequest {
        const WETH: &str = "0x82af49447d8a07e3bd95bd0d56f35241523fbab1";
        const USDC: &str = "0xaf88d065e77c8cc2239327c5edb3a432268e5831";
        SourceEvidenceRequest {
            schema_version: crate::source_state::SOURCE_EVIDENCE_REQUEST_SCHEMA.to_string(),
            source_event_identity: "source-event".to_string(),
            source_identity_hash: "1".repeat(64),
            source_transaction_hash: format!("0x{}", "2".repeat(64)),
            source_router: "0x1111111111111111111111111111111111111111".to_string(),
            source_factory: crate::source_state::UNISWAP_V3_FACTORY_ARBITRUM.to_string(),
            source_feed_sequence: 7,
            source_feed_order_position: 2,
            source_command_index: 0,
            source_pool_path: vec![format!("{WETH}:{USDC}:500")],
            source_token_path: vec![WETH.to_string(), USDC.to_string()],
            source_encoded_token_path: format!("{WETH}0001f4{}", &USDC[2..]),
            source_fee_path: vec![500],
            state_reconstruction_required: true,
        }
    }

    fn source_receipt_fixture() -> Value {
        let swap_topic = format!(
            "0x{}",
            hex::encode(ethabi::long_signature(
                "Swap",
                &[
                    ParamType::Address,
                    ParamType::Address,
                    ParamType::Int(256),
                    ParamType::Int(256),
                    ParamType::Uint(160),
                    ParamType::Uint(128),
                    ParamType::Int(24),
                ],
            ))
        );
        json!({
            "transactionHash": format!("0x{}", "2".repeat(64)),
            "blockNumber": "0x64",
            "blockHash": BLOCK_HASH,
            "transactionIndex": "0x2",
            "status": "0x1",
            "logs": [{
                "address": "0xc6962004f452be9203591991d15f6b388e09e8d0",
                "logIndex": "0x7",
                "topics": [swap_topic]
            }]
        })
    }

    fn source_transaction_fixture() -> Value {
        json!({
            "hash": format!("0x{}", "2".repeat(64)),
            "to": "0x1111111111111111111111111111111111111111",
            "blockNumber": "0x64",
            "blockHash": BLOCK_HASH,
            "transactionIndex": "0x2"
        })
    }

    fn source_block_fixture() -> Value {
        json!({
            "number": "0x64",
            "hash": BLOCK_HASH,
            "stateRoot": format!("0x{}", "d".repeat(64)),
            "parentHash": REORG_HASH,
            "timestamp": "0x1",
            "transactions": [
                format!("0x{}", "8".repeat(64)),
                format!("0x{}", "9".repeat(64)),
                format!("0x{}", "2".repeat(64))
            ]
        })
    }

    #[tokio::test]
    async fn source_inclusion_fails_over_as_one_bounded_provider_sequence() {
        let client = Arc::new(ModelClient::default());
        client.fail_source_receipt_for(AAVE_PRIMARY_PROVIDER_ID);
        let runtime = runtime(client.clone());
        let mut request = source_evidence_request_fixture();
        request.state_reconstruction_required = false;

        let response = runtime.resolve_source_evidence(request).await.unwrap();

        assert_eq!(response.provider_id, "provider_1");
        assert_eq!(response.source_block_number, 100);
        assert_eq!(response.source_transaction_index, 2);
        assert_eq!(response.completeness_status, "incomplete");
        assert_eq!(
            response.failure_reason.as_deref(),
            Some("state_reconstruction_not_selected")
        );
        let receipt_providers = client
            .calls()
            .into_iter()
            .filter(|call| call.method == RpcMethod::EthGetTransactionReceipt)
            .map(|call| call.provider_id)
            .collect::<Vec<_>>();
        assert_eq!(
            receipt_providers,
            vec![AAVE_PRIMARY_PROVIDER_ID, "provider_1"]
        );
    }

    #[test]
    fn source_receipt_and_trace_bind_the_exact_transaction_boundary() {
        const POOL_500: &str = "0xc6962004f452be9203591991d15f6b388e09e8d0";
        let source_request = source_evidence_request_fixture();
        let receipt = source_receipt_fixture();
        assert_eq!(
            source_swap_logs(&receipt, "success", &[POOL_500.to_string()]),
            Ok((Some(7), vec![POOL_500.to_string()]))
        );
        assert!(verify_source_inclusion(
            &source_request,
            &source_transaction_fixture(),
            &receipt,
            &source_block_fixture()
        )
        .is_ok());
        assert_eq!(
            source_swap_logs(
                &receipt,
                "success",
                &["0x2222222222222222222222222222222222222222".to_string()]
            ),
            Err(GatewayError::ProviderIntegrity)
        );

        let prestate_trace = RpcCallResult {
            value: json!({
                "0xc6962004f452be9203591991d15f6b388e09e8d0": {
                    "storage": {"0x00": "0x01", "0x01": "0x03"}
                }
            }),
            latency_ns: 1,
        };
        let diff_trace = RpcCallResult {
            value: json!({
                "pre": {
                    "0xc6962004f452be9203591991d15f6b388e09e8d0": {
                        "storage": {"0x00": "0x01"}
                    }
                },
                "post": {
                    "0xc6962004f452be9203591991d15f6b388e09e8d0": {
                        "storage": {"0x00": "0x02"}
                    }
                }
            }),
            latency_ns: 1,
        };
        let complete = source_trace_evidence(
            Ok(prestate_trace),
            Ok(diff_trace),
            &[POOL_500.to_string()],
            SourceTraceContext {
                request: &source_request,
                block_number: 100,
                block_hash: BLOCK_HASH,
                transaction_index: 2,
                parent_block_number: 99,
                parent_block_hash: REORG_HASH,
            },
        )
        .unwrap();
        assert_eq!(complete.0, "debug_trace_transaction_prestate_diff");
        assert!(complete.1.is_some());
        assert_eq!(complete.2, "complete");
        assert_eq!(complete.3, None);
        assert_eq!(
            complete.4["pool_state_transitions"][POOL_500]["post"]["storage"]["0x01"],
            "0x03"
        );

        let unavailable = source_trace_evidence(
            Err(CallFailure::Transport(TransportError::ProviderError)),
            Err(CallFailure::Transport(TransportError::ProviderError)),
            &[POOL_500.to_string()],
            SourceTraceContext {
                request: &source_request,
                block_number: 100,
                block_hash: BLOCK_HASH,
                transaction_index: 2,
                parent_block_number: 99,
                parent_block_hash: REORG_HASH,
            },
        )
        .unwrap();
        assert_eq!(unavailable.0, "unavailable");
        assert_eq!(
            unavailable.3.as_deref(),
            Some("transaction_trace_provider_unavailable")
        );
    }

    #[test]
    fn trace_diff_removes_cleared_storage_and_preserves_unchanged_storage() {
        let reconstructed = apply_account_diff(
            &json!({
                "balance": "0x1",
                "storage": {
                    "0x00": "0x01",
                    "0x01": "0x02",
                    "0x02": "0x03"
                }
            }),
            &json!({
                "balance": "0x1",
                "storage": {
                    "0x00": "0x01",
                    "0x01": "0x02"
                }
            }),
            &json!({
                "storage": {
                    "0x00": "0x09"
                }
            }),
        )
        .expect("apply canonical prestate diff");
        assert_eq!(reconstructed["storage"]["0x00"], "0x09");
        assert!(reconstructed["storage"].get("0x01").is_none());
        assert_eq!(reconstructed["storage"]["0x02"], "0x03");
        assert_eq!(reconstructed["balance"], "0x1");
    }

    #[test]
    fn source_inclusion_rejects_wrong_transaction_hash() {
        let request = source_evidence_request_fixture();
        let mut receipt = source_receipt_fixture();
        receipt["transactionHash"] = json!(format!("0x{}", "3".repeat(64)));
        assert_eq!(
            verify_source_inclusion(
                &request,
                &source_transaction_fixture(),
                &receipt,
                &source_block_fixture()
            ),
            Err(GatewayError::ProviderIntegrity)
        );
    }

    #[test]
    fn source_inclusion_rejects_wrong_block_hash_and_mixed_block_state() {
        let request = source_evidence_request_fixture();
        let mut block = source_block_fixture();
        block["hash"] = json!(NEXT_HASH);
        assert_eq!(
            verify_source_inclusion(
                &request,
                &source_transaction_fixture(),
                &source_receipt_fixture(),
                &block
            ),
            Err(GatewayError::ProviderIntegrity)
        );
    }

    #[test]
    fn source_inclusion_rejects_reordered_transaction() {
        let request = source_evidence_request_fixture();
        let mut block = source_block_fixture();
        block["transactions"] = json!([
            format!("0x{}", "8".repeat(64)),
            format!("0x{}", "2".repeat(64)),
            format!("0x{}", "9".repeat(64))
        ]);
        assert_eq!(
            verify_source_inclusion(
                &request,
                &source_transaction_fixture(),
                &source_receipt_fixture(),
                &block
            ),
            Err(GatewayError::ProviderIntegrity)
        );
    }

    #[test]
    fn source_logs_reject_partial_or_reordered_touched_pool_evidence() {
        const WETH: &str = "0x82af49447d8a07e3bd95bd0d56f35241523fbab1";
        const USDC: &str = "0xaf88d065e77c8cc2239327c5edb3a432268e5831";
        const POOL_500: &str = "0xc6962004f452be9203591991d15f6b388e09e8d0";
        const POOL_3000: &str = "0xc473e2aee3441bf9240be85eb122abb059a3b57c";
        let swap_topic = format!(
            "0x{}",
            hex::encode(ethabi::long_signature(
                "Swap",
                &[
                    ParamType::Address,
                    ParamType::Address,
                    ParamType::Int(256),
                    ParamType::Int(256),
                    ParamType::Uint(160),
                    ParamType::Uint(128),
                    ParamType::Int(24),
                ],
            ))
        );
        let partial = json!({
            "logs": [{
                "address": POOL_500,
                "logIndex": "0x7",
                "topics": [swap_topic.clone()]
            }]
        });
        let expected = vec![POOL_500.to_string(), POOL_3000.to_string()];
        assert_eq!(
            source_swap_logs(&partial, "success", &expected),
            Err(GatewayError::ProviderIntegrity)
        );
        let reordered = json!({
            "logs": [
                {"address": POOL_3000, "logIndex": "0x7", "topics": [swap_topic.clone()]},
                {"address": POOL_500, "logIndex": "0x8", "topics": [swap_topic.clone()]}
            ]
        });
        assert_eq!(
            source_swap_logs(&reordered, "success", &expected),
            Err(GatewayError::ProviderIntegrity)
        );
        let non_monotonic = json!({
            "logs": [
                {"address": POOL_500, "logIndex": "0x8", "topics": [swap_topic.clone()]},
                {"address": POOL_3000, "logIndex": "0x7", "topics": [swap_topic]}
            ]
        });
        assert_eq!(
            source_swap_logs(&non_monotonic, "success", &expected),
            Err(GatewayError::ProviderIntegrity)
        );
        let request = SourceEvidenceRequest {
            source_token_path: vec![WETH.to_string(), USDC.to_string(), WETH.to_string()],
            source_fee_path: vec![500, 3000],
            source_pool_path: vec![format!("{WETH}:{USDC}:500"), format!("{WETH}:{USDC}:3000")],
            source_encoded_token_path: format!("{WETH}0001f4{}000bb8{}", &USDC[2..], &WETH[2..]),
            ..source_evidence_request_fixture()
        };
        assert_eq!(request.validate(), Ok(()));
    }

    #[test]
    fn partial_touched_pool_trace_evidence_is_incomplete() {
        const WETH: &str = "0x82af49447d8a07e3bd95bd0d56f35241523fbab1";
        const USDC: &str = "0xaf88d065e77c8cc2239327c5edb3a432268e5831";
        const POOL_500: &str = "0xc6962004f452be9203591991d15f6b388e09e8d0";
        const POOL_3000: &str = "0xc473e2aee3441bf9240be85eb122abb059a3b57c";
        let request = SourceEvidenceRequest {
            source_token_path: vec![WETH.to_string(), USDC.to_string(), WETH.to_string()],
            source_fee_path: vec![500, 3000],
            source_pool_path: vec![format!("{WETH}:{USDC}:500"), format!("{WETH}:{USDC}:3000")],
            source_encoded_token_path: format!("{WETH}0001f4{}000bb8{}", &USDC[2..], &WETH[2..]),
            ..source_evidence_request_fixture()
        };
        let evidence = source_trace_evidence(
            Ok(RpcCallResult {
                value: json!({
                    POOL_500: {"storage": {"0x00": "0x01"}}
                }),
                latency_ns: 1,
            }),
            Ok(RpcCallResult {
                value: json!({
                    "pre": {POOL_500: {"storage": {"0x00": "0x01"}}},
                    "post": {POOL_500: {"storage": {"0x00": "0x02"}}}
                }),
                latency_ns: 1,
            }),
            &[POOL_500.to_string(), POOL_3000.to_string()],
            SourceTraceContext {
                request: &request,
                block_number: 100,
                block_hash: BLOCK_HASH,
                transaction_index: 2,
                parent_block_number: 99,
                parent_block_hash: REORG_HASH,
            },
        )
        .unwrap();
        assert_eq!(evidence.0, "unavailable");
        assert_eq!(evidence.2, "incomplete");
        assert_eq!(
            evidence.3.as_deref(),
            Some("source_pool_trace_state_unavailable")
        );
        assert_eq!(evidence.4["complete"], false);
    }

    #[test]
    fn trace_capability_failures_remain_distinct_and_incomplete() {
        const POOL_500: &str = "0xc6962004f452be9203591991d15f6b388e09e8d0";
        let request = source_evidence_request_fixture();
        let context = SourceTraceContext {
            request: &request,
            block_number: 100,
            block_hash: BLOCK_HASH,
            transaction_index: 2,
            parent_block_number: 99,
            parent_block_hash: REORG_HASH,
        };
        for (failure, expected) in [
            (
                TransportError::MethodUnsupported,
                "transaction_trace_method_unsupported",
            ),
            (
                TransportError::HistoricalStateUnavailable,
                "transaction_trace_historical_state_unavailable",
            ),
            (TransportError::Timeout, "transaction_trace_timeout"),
        ] {
            let evidence = source_trace_evidence(
                Err(CallFailure::Transport(failure)),
                Err(CallFailure::Transport(failure)),
                &[POOL_500.to_string()],
                context,
            )
            .unwrap();
            assert_eq!(evidence.0, "unavailable");
            assert_eq!(evidence.2, "incomplete");
            assert_eq!(evidence.3.as_deref(), Some(expected));
            assert_eq!(evidence.4["complete"], false);
        }
    }

    #[test]
    fn oversized_trace_evidence_is_explicitly_incomplete() {
        const POOL_500: &str = "0xc6962004f452be9203591991d15f6b388e09e8d0";
        let request = source_evidence_request_fixture();
        let mut storage = serde_json::Map::new();
        for index in 0..8_000_u64 {
            storage.insert(
                format!("0x{index:064x}"),
                Value::String(format!("0x{:064x}", index + 1)),
            );
        }
        let prestate = RpcCallResult {
            value: json!({POOL_500: {"storage": storage}}),
            latency_ns: 1,
        };
        let diff = RpcCallResult {
            value: json!({
                "pre": {POOL_500: {"storage": {"0x00": "0x01"}}},
                "post": {POOL_500: {"storage": {"0x00": "0x02"}}}
            }),
            latency_ns: 1,
        };
        let evidence = source_trace_evidence(
            Ok(prestate),
            Ok(diff),
            &[POOL_500.to_string()],
            SourceTraceContext {
                request: &request,
                block_number: 100,
                block_hash: BLOCK_HASH,
                transaction_index: 2,
                parent_block_number: 99,
                parent_block_hash: REORG_HASH,
            },
        )
        .unwrap();
        assert_eq!(evidence.0, "unavailable");
        assert_eq!(
            evidence.3.as_deref(),
            Some("transaction_trace_evidence_oversized")
        );
    }

    #[test]
    fn parsers_reject_ambiguous_quantities_and_malformed_state_words() {
        assert!(canonical_quantity("0x0"));
        assert!(canonical_quantity("0xa4b1"));
        assert!(!canonical_quantity("latest"));
        assert!(!canonical_quantity("0x00"));
        assert!(parse_block(&json!({"number": "latest", "hash": BLOCK_HASH})).is_none());
        assert!(parse_address_bytes(&[0_u8; 31]).is_none());
        assert!(parse_u32_bytes(&[0_u8; 31]).is_none());
    }

    #[test]
    fn aave_simulation_calldata_and_pause_override_are_exactly_bound() {
        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_secs();
        let request = AaveSimulateRequest {
            schema_version: crate::aave_state::AAVE_SIMULATE_REQUEST_SCHEMA.to_string(),
            chain_id: ARBITRUM_ONE_CHAIN_ID,
            request_id: "aave-sim-1".to_string(),
            block_number: 100,
            block_hash: BLOCK_HASH.to_string(),
            state_root: REORG_HASH.to_string(),
            executor_address: "0x1111111111111111111111111111111111111111".to_string(),
            executor_code_hash: "1".repeat(64),
            caller_address: "0x2222222222222222222222222222222222222222".to_string(),
            release_sha: "2".repeat(40),
            borrower: "0x3333333333333333333333333333333333333333".to_string(),
            debt_asset: ARBITRUM_WETH.to_string(),
            collateral_asset: ARBITRUM_NATIVE_USDC.to_string(),
            debt_asset_decimals: 18,
            debt_asset_price_base: "200000000000".to_string(),
            weth_price_base: "200000000000".to_string(),
            repay_amount: "1000000".to_string(),
            maximum_input_amount: "2000000".to_string(),
            live_maximum_input_amount: "2000000".to_string(),
            maximum_input_weth_wei: "2000000".to_string(),
            live_maximum_input_weth_wei: "2000000".to_string(),
            counterfactual: false,
            minimum_collateral_received: "2000000".to_string(),
            minimum_unwind_output: "1100000".to_string(),
            minimum_profit: "50001000".to_string(),
            minimum_profit_weth_wei: "50001000".to_string(),
            expected_profit: "100000000".to_string(),
            retained_profit_floor: "1000".to_string(),
            selected_pool: "0xc6962004f452be9203591991d15f6b388e09e8d0".to_string(),
            selected_factory: UNISWAP_V3_FACTORY_ARBITRUM.to_string(),
            selected_fee: 500,
            zero_for_one: false,
            gas_limit: 500_000,
            max_fee_per_gas: "100".to_string(),
            max_priority_fee_per_gas: "10".to_string(),
            deadline_unix_seconds: now + 60,
            atlas_mode: false,
            atlas_bid: "0".to_string(),
        };
        validate_aave_simulation_identity(&request).unwrap();
        let route_id = aave_simulation_route_id(&request).unwrap();
        let calldata = encode_aave_liquidation_call(&request, &route_id).unwrap();
        assert_eq!(
            &calldata[..4],
            &ethabi::short_signature(
                "executeAaveLiquidation",
                &[ParamType::Tuple(vec![
                    ParamType::FixedBytes(32),
                    ParamType::Address,
                    ParamType::Address,
                    ParamType::Address,
                    ParamType::Uint(256),
                    ParamType::Bool,
                    ParamType::Uint(256),
                    ParamType::Uint(256),
                    ParamType::Uint(256),
                    ParamType::Uint(256),
                    ParamType::Uint(256),
                    ParamType::Uint(256),
                    ParamType::Array(Box::new(ParamType::Tuple(vec![
                        ParamType::Address,
                        ParamType::Address,
                        ParamType::Address,
                        ParamType::Uint(24),
                        ParamType::Bool,
                        ParamType::Uint(256),
                    ]))),
                ])]
            )
        );
        let packed = format!("0x{}0001{}", "00".repeat(10), &AAVE_V3_POOL_ARBITRUM[2..]);
        let unpaused = decode_executor_packed_config(&packed).unwrap();
        assert_eq!(
            unpaused,
            format!("0x{}0000{}", "00".repeat(10), &AAVE_V3_POOL_ARBITRUM[2..])
        );
        let direct_state_diff =
            aave_executor_simulation_state_diff(&request, &unpaused, U256::from(2_000_000_u64))
                .unwrap();
        assert_eq!(
            direct_state_diff
                .get(PHOENIX_EXECUTOR_PACKED_CONFIG_SLOT)
                .and_then(Value::as_str),
            Some(unpaused.as_str())
        );
        assert!(direct_state_diff
            .get(PHOENIX_EXECUTOR_MAXIMUM_INPUT_SLOT)
            .is_none());

        let mut counterfactual = request.clone();
        counterfactual.repay_amount = "3000000".to_string();
        counterfactual.maximum_input_amount = MAXIMUM_REVIEWED_INPUT_WEI.to_string();
        counterfactual.maximum_input_weth_wei = MAXIMUM_REVIEWED_INPUT_WEI.to_string();
        counterfactual.counterfactual = true;
        counterfactual.validate().unwrap();
        let counterfactual_state_diff = aave_executor_simulation_state_diff(
            &counterfactual,
            &unpaused,
            U256::from(2_000_000_u64),
        )
        .unwrap();
        assert_eq!(
            counterfactual_state_diff
                .get(PHOENIX_EXECUTOR_MAXIMUM_INPUT_SLOT)
                .and_then(Value::as_str),
            Some(u256_storage_word(U256::from(MAXIMUM_REVIEWED_INPUT_WEI)).as_str())
        );

        let request_type = ParamType::Tuple(vec![
            ParamType::FixedBytes(32),
            ParamType::Address,
            ParamType::Address,
            ParamType::Address,
            ParamType::Uint(256),
            ParamType::Bool,
            ParamType::Uint(256),
            ParamType::Uint(256),
            ParamType::Uint(256),
            ParamType::Uint(256),
            ParamType::Uint(256),
            ParamType::Uint(256),
            ParamType::Array(Box::new(ParamType::Tuple(vec![
                ParamType::Address,
                ParamType::Address,
                ParamType::Address,
                ParamType::Uint(24),
                ParamType::Bool,
                ParamType::Uint(256),
            ]))),
        ]);
        let decoded = ethabi::decode(std::slice::from_ref(&request_type), &calldata[4..]).unwrap();
        let Token::Tuple(fields) = &decoded[0] else {
            panic!("liquidation tuple")
        };
        assert_eq!(fields[4].clone().into_uint(), Some(U256::from(1_000_000)));
        assert_eq!(fields[6].clone().into_uint(), Some(U256::from(2_000_000)));
        assert_eq!(fields[12].clone().into_array().unwrap().len(), 1);

        let mut identity = request;
        identity.collateral_asset = ARBITRUM_WETH.to_string();
        identity.selected_pool = ZERO_ADDRESS.to_string();
        identity.selected_factory = ZERO_ADDRESS.to_string();
        identity.selected_fee = 0;
        validate_aave_simulation_identity(&identity).unwrap();
        let identity_route = aave_simulation_route_id(&identity).unwrap();
        let identity_calldata = encode_aave_liquidation_call(&identity, &identity_route).unwrap();
        let decoded = ethabi::decode(&[request_type], &identity_calldata[4..]).unwrap();
        let Token::Tuple(fields) = &decoded[0] else {
            panic!("identity liquidation tuple")
        };
        assert!(fields[12].clone().into_array().unwrap().is_empty());
    }

    #[test]
    fn reviewed_aave_routes_cover_every_verified_fee_and_derive_both_directions() {
        let routes = reviewed_aave_unwind_routes().unwrap();
        assert_eq!(
            routes.iter().map(|route| route.fee).collect::<Vec<_>>(),
            vec![100, 500, 500, 3_000]
        );
        assert!(routes
            .iter()
            .filter(|route| route.token1 == ARBITRUM_NATIVE_USDC)
            .all(|route| {
                route.factory == UNISWAP_V3_FACTORY_ARBITRUM
                    && route.token0 == ARBITRUM_WETH
                    && !route.zero_for_one
            }));
        let usdc_e = routes
            .iter()
            .find(|route| route.token1 == ARBITRUM_USDC_E)
            .expect("reviewed USDC.e route");
        assert_eq!(usdc_e.pool, REVIEWED_WETH_USDC_E_POOL_500);
        assert_eq!(usdc_e.factory, UNISWAP_V3_FACTORY_ARBITRUM);
        assert_eq!(usdc_e.token0, ARBITRUM_WETH);
        assert_eq!(usdc_e.fee, 500);
        assert!(usdc_e.zero_for_one);
        assert_eq!(
            uniswap_zero_for_one(ARBITRUM_NATIVE_USDC, ARBITRUM_NATIVE_USDC, ARBITRUM_WETH),
            Some(true)
        );
        assert_eq!(
            uniswap_zero_for_one(ARBITRUM_NATIVE_USDC, ARBITRUM_WETH, ARBITRUM_NATIVE_USDC),
            Some(false)
        );
        assert_eq!(
            uniswap_zero_for_one(
                ARBITRUM_NATIVE_USDC,
                ARBITRUM_WETH,
                "0x1111111111111111111111111111111111111111"
            ),
            None
        );
    }

    fn aave_configuration(
        decimals: u8,
        liquidation_bonus: u16,
        protocol_fee: u16,
        active: bool,
        paused: bool,
    ) -> String {
        let mut configuration = U256::from(8_000_u64) << 16;
        configuration |= U256::from(liquidation_bonus) << 32;
        configuration |= U256::from(decimals) << 48;
        if active {
            configuration |= U256::one() << 56;
        }
        if paused {
            configuration |= U256::one() << 60;
        }
        configuration |= U256::from(protocol_fee) << 152;
        configuration.to_string()
    }

    #[allow(clippy::too_many_arguments)]
    fn aave_reserve(
        asset: &str,
        reserve_id: u16,
        decimals: u8,
        supplied: U256,
        stable_debt: U256,
        variable_debt: U256,
        price: U256,
        liquidation_bonus: u16,
        protocol_fee: u16,
    ) -> AaveExactReserveState {
        AaveExactReserveState {
            asset: asset.to_string(),
            reserve_id,
            decimals,
            current_a_token_balance: supplied.to_string(),
            current_stable_debt: stable_debt.to_string(),
            current_variable_debt: variable_debt.to_string(),
            usage_as_collateral_enabled: !supplied.is_zero(),
            configuration_data: aave_configuration(
                decimals,
                liquidation_bonus,
                protocol_fee,
                true,
                false,
            ),
            a_token: "0x1111111111111111111111111111111111111111".to_string(),
            stable_debt_token: "0x2222222222222222222222222222222222222222".to_string(),
            variable_debt_token: "0x3333333333333333333333333333333333333333".to_string(),
            oracle_price_base: price.to_string(),
            liquidation_grace_period_until: 0,
        }
    }

    fn exact_aave_fixture(health_factor: u128) -> (AaveAccountData, Vec<AaveExactReserveState>) {
        let weth_unit = U256::exp10(18);
        let usdc_unit = U256::exp10(6);
        let weth_price = U256::from(2_000_u64 * 100_000_000);
        let stable_debt = U256::zero();
        let variable_debt = U256::from(10) * weth_unit;
        let account = AaveAccountData {
            borrower: "0x4444444444444444444444444444444444444444".to_string(),
            total_collateral_base: (U256::from(60_000_u64 * 100_000_000)).to_string(),
            total_debt_base: (U256::from(20_000_u64 * 100_000_000)).to_string(),
            available_borrows_base: "0".to_string(),
            current_liquidation_threshold_bps: "8000".to_string(),
            loan_to_value_bps: "7500".to_string(),
            health_factor_wad: health_factor.to_string(),
        };
        let reserves = vec![
            aave_reserve(
                ARBITRUM_WETH,
                0,
                18,
                U256::from(15) * weth_unit,
                stable_debt,
                variable_debt,
                weth_price,
                10_500,
                1_000,
            ),
            aave_reserve(
                ARBITRUM_NATIVE_USDC,
                1,
                6,
                U256::from(30_000) * usdc_unit,
                U256::zero(),
                U256::zero(),
                U256::from(100_000_000_u64),
                10_500,
                1_000,
            ),
            aave_reserve(
                ARBITRUM_USDC_E,
                2,
                6,
                U256::zero(),
                U256::zero(),
                U256::zero(),
                U256::from(100_000_000_u64),
                10_500,
                1_000,
            ),
        ];
        (account, reserves)
    }

    fn usdc_e_weth_exact_fixture(
        stable_debt: U256,
    ) -> (AaveAccountData, Vec<AaveExactReserveState>) {
        let weth_unit = U256::exp10(18);
        let usdc_e_unit = U256::exp10(6);
        let weth_price = U256::from(2_000_u64 * 100_000_000);
        let account = AaveAccountData {
            borrower: "0x4444444444444444444444444444444444444444".to_string(),
            total_collateral_base: (U256::from(30_000_u64 * 100_000_000)).to_string(),
            total_debt_base: (U256::from(20_000_u64 * 100_000_000)).to_string(),
            available_borrows_base: "0".to_string(),
            current_liquidation_threshold_bps: "8000".to_string(),
            loan_to_value_bps: "7500".to_string(),
            health_factor_wad: (CLOSE_FACTOR_HF_THRESHOLD_WAD - 1).to_string(),
        };
        let reserves = vec![
            aave_reserve(
                ARBITRUM_WETH,
                0,
                18,
                U256::from(15) * weth_unit,
                U256::zero(),
                U256::zero(),
                weth_price,
                10_500,
                1_000,
            ),
            aave_reserve(
                ARBITRUM_NATIVE_USDC,
                1,
                6,
                U256::zero(),
                U256::zero(),
                U256::zero(),
                U256::from(100_000_000_u64),
                10_500,
                1_000,
            ),
            aave_reserve(
                ARBITRUM_USDC_E,
                2,
                6,
                U256::zero(),
                stable_debt,
                U256::from(100_000) * usdc_e_unit,
                U256::from(100_000_000_u64),
                10_500,
                1_000,
            ),
        ];
        (account, reserves)
    }

    fn usdc_e_simulation_request() -> AaveSimulateRequest {
        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_secs();
        AaveSimulateRequest {
            schema_version: crate::aave_state::AAVE_SIMULATE_REQUEST_SCHEMA.to_string(),
            chain_id: ARBITRUM_ONE_CHAIN_ID,
            request_id: "aave-usdc-e-sim".to_string(),
            block_number: 100,
            block_hash: BLOCK_HASH.to_string(),
            state_root: REORG_HASH.to_string(),
            executor_address: "0x1111111111111111111111111111111111111111".to_string(),
            executor_code_hash: "1".repeat(64),
            caller_address: "0x2222222222222222222222222222222222222222".to_string(),
            release_sha: "2".repeat(40),
            borrower: "0x3333333333333333333333333333333333333333".to_string(),
            debt_asset: ARBITRUM_USDC_E.to_string(),
            collateral_asset: ARBITRUM_WETH.to_string(),
            debt_asset_decimals: 6,
            debt_asset_price_base: "100000000".to_string(),
            weth_price_base: "200000000000".to_string(),
            repay_amount: "10000000".to_string(),
            maximum_input_amount: "20000000".to_string(),
            live_maximum_input_amount: "20000000".to_string(),
            maximum_input_weth_wei: MAXIMUM_REVIEWED_INPUT_WEI.to_string(),
            live_maximum_input_weth_wei: MAXIMUM_REVIEWED_INPUT_WEI.to_string(),
            counterfactual: false,
            minimum_collateral_received: "5000000000000000".to_string(),
            minimum_unwind_output: "10000001".to_string(),
            minimum_profit: "2000".to_string(),
            minimum_profit_weth_wei: "1000000000000".to_string(),
            expected_profit: "1250000000000".to_string(),
            retained_profit_floor: "1000000000000".to_string(),
            selected_pool: REVIEWED_WETH_USDC_E_POOL_500.to_string(),
            selected_factory: UNISWAP_V3_FACTORY_ARBITRUM.to_string(),
            selected_fee: 500,
            zero_for_one: true,
            gas_limit: 500_000,
            max_fee_per_gas: "100".to_string(),
            max_priority_fee_per_gas: "10".to_string(),
            deadline_unix_seconds: now + 60,
            atlas_mode: false,
            atlas_bid: "0".to_string(),
        }
    }

    #[test]
    fn usdc_e_variable_debt_weth_collateral_is_bounded_in_weth_value() {
        let (account, reserves) = usdc_e_weth_exact_fixture(U256::zero());
        let variants = supported_aave_liquidations(
            &account,
            &reserves,
            U256::from(MAXIMUM_REVIEWED_INPUT_WEI),
            U256::zero(),
            0,
            5,
        )
        .unwrap();
        assert_eq!(variants.len(), SizeLevel::ALL.len());
        assert!(variants.iter().all(|variant| {
            variant.debt_asset == ARBITRUM_USDC_E
                && variant.collateral_asset == ARBITRUM_WETH
                && variant.debt_asset_decimals == 6
                && variant.debt_asset_review == "usdc_e_debt_reviewed"
                && variant.maximum_repay_amount == "20000000"
        }));
        assert_eq!(variants.last().unwrap().repay_amount, "20000000");
        assert_ne!(
            variants.last().unwrap().repay_amount,
            MAXIMUM_REVIEWED_INPUT_WEI.to_string()
        );

        let routes = reviewed_aave_unwind_routes().unwrap();
        let route = routes
            .iter()
            .find(|route| route.pool == REVIEWED_WETH_USDC_E_POOL_500)
            .expect("reviewed WETH/USDC.e route");
        assert_eq!(route.factory, UNISWAP_V3_FACTORY_ARBITRUM);
        assert_eq!(route.token0, ARBITRUM_WETH);
        assert_eq!(route.token1, ARBITRUM_USDC_E);
        assert_eq!(route.fee, 500);
        assert!(route.zero_for_one);
    }

    #[test]
    fn stable_usdc_e_debt_and_unit_mixing_fail_closed() {
        let (account, reserves) = usdc_e_weth_exact_fixture(U256::one());
        assert!(supported_aave_liquidations(
            &account,
            &reserves,
            U256::from(MAXIMUM_REVIEWED_INPUT_WEI),
            U256::zero(),
            0,
            5,
        )
        .unwrap()
        .is_empty());

        let mut request = usdc_e_simulation_request();
        request.validate().unwrap();
        validate_aave_simulation_identity(&request).unwrap();
        request.maximum_input_amount = MAXIMUM_REVIEWED_INPUT_WEI.to_string();
        assert!(request.validate().is_err());
        request = usdc_e_simulation_request();
        request.minimum_profit = "1999".to_string();
        assert!(request.validate().is_err());
    }

    #[test]
    fn usdc_e_simulation_keeps_raw_flash_and_canonical_weth_economics_separate() {
        let request = usdc_e_simulation_request();
        let route_id = aave_simulation_route_id(&request).unwrap();
        let response = build_aave_simulation_response(
            &request,
            &route_id,
            &[1, 2, 3, 4, 5],
            AAVE_PRIMARY_PROVIDER_ID,
            AaveSimulationEvidence {
                realized_profit: U256::from(2_500),
                total_gas: 100_000,
                l1_gas: 1_000,
                base_fee_per_gas: U256::one(),
                flash_premium_bps: 5,
            },
        )
        .unwrap();
        assert_eq!(response.realized_profit_debt_asset, "2500");
        assert_eq!(response.realized_profit, "1250000000000");
        assert_eq!(response.flash_premium_debt_asset, "5000");
        assert_eq!(response.flash_premium_wei, "2500000000000");
        response.validate(&request).unwrap();
    }

    #[test]
    fn aave_size_grid_is_bounded_deduplicated_and_clamped() {
        assert!(liquidation_size_grid(
            U256::from(3),
            U256::from(1),
            U256::from(1),
            U256::exp10(18),
        )
        .unwrap()
        .is_empty());
        let (account, reserves) = exact_aave_fixture(CLOSE_FACTOR_HF_THRESHOLD_WAD);
        let cap = U256::from(3) * U256::exp10(18);
        let variants =
            supported_aave_liquidations(&account, &reserves, cap, U256::zero(), 0, 5).unwrap();
        for collateral in [ARBITRUM_WETH, ARBITRUM_NATIVE_USDC] {
            let collateral_variants = variants
                .iter()
                .filter(|variant| variant.collateral_asset == collateral)
                .collect::<Vec<_>>();
            assert!(collateral_variants.iter().all(|variant| {
                variant.size_classification == FIXED_REVIEWED_SIZE
                    && variant.terminal_size_reason.is_empty()
            }));
            let sizes = collateral_variants
                .iter()
                .map(|variant| decimal_u256(&variant.actual_repay_amount).unwrap())
                .collect::<Vec<_>>();
            assert_eq!(sizes.len(), SizeLevel::ALL.len());
            assert_eq!(sizes.last(), Some(&U256::from(MAXIMUM_REVIEWED_INPUT_WEI)));
            assert!(sizes.windows(2).all(|pair| pair[0] < pair[1]));
            assert!(sizes
                .iter()
                .all(|size| *size <= U256::from(MAXIMUM_REVIEWED_INPUT_WEI)));
        }
    }

    #[test]
    fn aave_terminal_size_covers_below_minimum_and_dust_full_close() {
        let maximum_input = U256::from(MAXIMUM_REVIEWED_INPUT_WEI);
        let below_minimum = U256::from(SizeLevel::Min.amount_wei() / 2);
        let (account, mut reserves) = exact_aave_fixture(CLOSE_FACTOR_HF_THRESHOLD_WAD);
        reserves[0].current_variable_debt = below_minimum.to_string();
        let variants =
            supported_aave_liquidations(&account, &reserves, maximum_input, U256::zero(), 0, 5)
                .unwrap();
        for collateral in [ARBITRUM_WETH, ARBITRUM_NATIVE_USDC] {
            let terminal = variants
                .iter()
                .filter(|variant| variant.collateral_asset == collateral)
                .collect::<Vec<_>>();
            assert_eq!(terminal.len(), 1);
            assert_eq!(terminal[0].actual_repay_amount, below_minimum.to_string());
            assert_eq!(terminal[0].size_classification, TERMINAL_SIZE_REQUIRED);
            assert_eq!(terminal[0].terminal_size_reason, BELOW_MIN_REVIEWED_SIZE);
        }

        let dusty_full_close = U256::from(SizeLevel::Min.amount_wei() + 10_000_000_000_000);
        let (account, mut reserves) = exact_aave_fixture(CLOSE_FACTOR_HF_THRESHOLD_WAD);
        reserves[0].current_variable_debt = dusty_full_close.to_string();
        let variants =
            supported_aave_liquidations(&account, &reserves, maximum_input, U256::zero(), 0, 5)
                .unwrap();
        for collateral in [ARBITRUM_WETH, ARBITRUM_NATIVE_USDC] {
            let terminal = variants
                .iter()
                .filter(|variant| variant.collateral_asset == collateral)
                .collect::<Vec<_>>();
            assert_eq!(terminal.len(), 1);
            assert_eq!(
                terminal[0].actual_repay_amount,
                dusty_full_close.to_string()
            );
            assert_eq!(terminal[0].size_classification, TERMINAL_SIZE_REQUIRED);
            assert_eq!(terminal[0].terminal_size_reason, DUST_PARTIAL_INVALID);
            assert!(decimal_u256(&terminal[0].actual_repay_amount).unwrap() <= maximum_input);
        }
    }

    #[test]
    fn aave_close_factor_boundary_uses_variable_debt_only() {
        let maximum_input = U256::from(20) * U256::exp10(18);
        let (at_boundary, reserves) = exact_aave_fixture(CLOSE_FACTOR_HF_THRESHOLD_WAD);
        let full =
            supported_aave_liquidations(&at_boundary, &reserves, maximum_input, U256::zero(), 0, 5)
                .unwrap();
        let full_usdc = full
            .iter()
            .rfind(|variant| variant.collateral_asset == ARBITRUM_NATIVE_USDC)
            .unwrap();
        assert_eq!(
            decimal_u256(&full_usdc.actual_repay_amount).unwrap(),
            U256::from(MAXIMUM_REVIEWED_INPUT_WEI)
        );

        let (above_boundary, reserves) = exact_aave_fixture(CLOSE_FACTOR_HF_THRESHOLD_WAD + 1);
        let half = supported_aave_liquidations(
            &above_boundary,
            &reserves,
            maximum_input,
            U256::zero(),
            0,
            5,
        )
        .unwrap();
        let half_usdc = half
            .iter()
            .rfind(|variant| variant.collateral_asset == ARBITRUM_NATIVE_USDC)
            .unwrap();
        assert_eq!(
            decimal_u256(&half_usdc.actual_repay_amount).unwrap(),
            U256::from(MAXIMUM_REVIEWED_INPUT_WEI)
        );
        assert_eq!(
            percent_mul(U256::one(), U256::from(5_000)).unwrap(),
            U256::one()
        );
        assert_eq!(
            percent_mul_floor(U256::one(), U256::from(5_000)).unwrap(),
            U256::zero()
        );

        let (account, mut reserves) = exact_aave_fixture(CLOSE_FACTOR_HF_THRESHOLD_WAD);
        reserves[0].current_stable_debt = U256::one().to_string();
        assert!(supported_aave_liquidations(
            &account,
            &reserves,
            maximum_input,
            U256::zero(),
            0,
            5,
        )
        .unwrap()
        .is_empty());
    }

    #[test]
    fn aave_decimals_collateral_capacity_emode_and_protocol_fee_are_exact() {
        assert_eq!(asset_unit(6).unwrap(), U256::from(1_000_000));
        assert_eq!(asset_unit(8).unwrap(), U256::from(100_000_000));
        assert_eq!(asset_unit(18).unwrap(), U256::exp10(18));

        let capped = calculate_aave_liquidation_amounts(
            U256::from(100),
            U256::from(105),
            U256::one(),
            U256::one(),
            U256::one(),
            U256::one(),
            11_000,
            0,
        )
        .unwrap();
        assert_eq!(capped.actual_repay, U256::from(96));
        let exact = calculate_aave_liquidation_amounts(
            capped.actual_repay,
            U256::from(105),
            U256::one(),
            U256::one(),
            U256::one(),
            U256::one(),
            11_000,
            0,
        )
        .unwrap();
        assert_eq!(exact.actual_repay, capped.actual_repay);
        assert_eq!(exact.seized_before_fee, U256::from(105));

        let fee = calculate_aave_liquidation_amounts(
            U256::one(),
            U256::from(2),
            U256::one(),
            U256::one(),
            U256::one(),
            U256::one(),
            20_000,
            1,
        )
        .unwrap();
        assert_eq!(fee.protocol_fee, U256::one());
        assert_eq!(fee.liquidator_collateral, U256::one());

        let (account, reserves) = exact_aave_fixture(CLOSE_FACTOR_HF_THRESHOLD_WAD);
        let cap = U256::from(MAXIMUM_REVIEWED_INPUT_WEI);
        let normal =
            supported_aave_liquidations(&account, &reserves, cap, U256::zero(), 0, 5).unwrap();
        let emode =
            supported_aave_liquidations(&account, &reserves, cap, U256::one() << 1, 10_100, 5)
                .unwrap();
        let normal_seized = normal
            .iter()
            .find(|variant| {
                variant.collateral_asset == ARBITRUM_NATIVE_USDC
                    && variant.actual_repay_amount == cap.to_string()
            })
            .map(|variant| decimal_u256(&variant.seized_collateral).unwrap())
            .unwrap();
        let emode_seized = emode
            .iter()
            .find(|variant| {
                variant.collateral_asset == ARBITRUM_NATIVE_USDC
                    && variant.actual_repay_amount == cap.to_string()
            })
            .map(|variant| decimal_u256(&variant.seized_collateral).unwrap())
            .unwrap();
        assert!(emode_seized < normal_seized);
    }

    #[test]
    fn aave_reserve_flags_grace_and_dust_fail_closed() {
        let cap = U256::exp10(18);
        let (healthy, reserves) = exact_aave_fixture(1_000_000_000_000_000_000);
        assert!(
            supported_aave_liquidations(&healthy, &reserves, cap, U256::zero(), 0, 5)
                .unwrap()
                .is_empty()
        );

        let (account, mut reserves) = exact_aave_fixture(CLOSE_FACTOR_HF_THRESHOLD_WAD);
        reserves[0].configuration_data = aave_configuration(18, 10_500, 1_000, false, false);
        assert!(
            supported_aave_liquidations(&account, &reserves, cap, U256::zero(), 0, 5)
                .unwrap()
                .is_empty()
        );

        let (account, mut reserves) = exact_aave_fixture(CLOSE_FACTOR_HF_THRESHOLD_WAD);
        reserves[1].configuration_data = aave_configuration(6, 10_500, 1_000, true, true);
        let variants =
            supported_aave_liquidations(&account, &reserves, cap, U256::zero(), 0, 5).unwrap();
        assert!(variants
            .iter()
            .all(|variant| variant.collateral_asset != ARBITRUM_NATIVE_USDC));
        assert!(aave_liquidation_grace_elapsed(0, 9, 10));
        assert!(!aave_liquidation_grace_elapsed(0, 10, 10));

        let (account, mut reserves) = exact_aave_fixture(CLOSE_FACTOR_HF_THRESHOLD_WAD);
        reserves[0].current_a_token_balance = "0".to_string();
        reserves[1].current_a_token_balance = "0".to_string();
        assert!(
            supported_aave_liquidations(&account, &reserves, cap, U256::zero(), 0, 5)
                .unwrap()
                .is_empty()
        );

        let dusty = AaveLiquidationAmounts {
            actual_repay: U256::from(99),
            seized_before_fee: U256::from(99),
            protocol_fee: U256::zero(),
            liquidator_collateral: U256::from(99),
        };
        assert!(!aave_liquidation_dust_valid(
            U256::from(100),
            U256::from(100),
            dusty,
            U256::one(),
            U256::one(),
            U256::one(),
            U256::one(),
        )
        .unwrap());
    }

    #[test]
    fn aave_gas_quote_is_bounded_and_atlas_bid_is_subtracted_once() {
        let encoded = format!(
            "0x{}",
            hex::encode(ethabi::encode(&[
                Token::Uint(U256::from(100_000)),
                Token::Uint(U256::from(20_000)),
                Token::Uint(U256::from(100)),
                Token::Uint(U256::from(1)),
            ]))
        );
        assert_eq!(
            decode_gas_estimate_components(&encoded),
            Some((100_000, 20_000, U256::from(100)))
        );
        let (gas_limit, quoted_fee, total_cost, l1_cost) = bounded_aave_gas_quote(
            100_000,
            20_000,
            U256::from(100),
            U256::from(1_000),
            U256::from(10),
            120_000,
        )
        .unwrap();
        assert_eq!(gas_limit, 120_000);
        assert_eq!(quoted_fee, U256::from(210));
        assert_eq!(total_cost, U256::from(25_200_000));
        assert_eq!(l1_cost, U256::from(4_200_000));
        assert_eq!(
            aave_conservative_simulation_net(
                U256::from(30_000_000),
                total_cost,
                U256::from(1_000_000),
            )
            .unwrap(),
            U256::from(3_800_000)
        );
        assert_eq!(
            bounded_aave_gas_quote(
                100_000,
                20_000,
                U256::from(100),
                U256::from(1_000),
                U256::from(10),
                119_999,
            ),
            Err(GatewayError::StateIncomplete)
        );
        assert_eq!(
            bounded_aave_gas_quote(
                100_000,
                20_000,
                U256::from(100),
                U256::from(209),
                U256::from(10),
                120_000,
            ),
            Err(GatewayError::StateIncomplete)
        );
    }

    #[test]
    fn gateway_errors_are_sanitized_and_retryability_is_bounded() {
        for error in [
            GatewayError::InvalidRequest,
            GatewayError::RequestBudgetExhausted,
            GatewayError::UpstreamBudgetExhausted,
            GatewayError::ProviderUnavailable,
            GatewayError::ProviderIntegrity,
            GatewayError::ResponseOversized,
        ] {
            assert!(!error.to_string().contains("https://"));
            assert!(error.class().len() <= 64);
            assert_eq!(error.response().error_class, error.class());
        }
    }

    #[test]
    fn aave_tail_decodes_only_exact_pool_event_borrowers() {
        let borrower = "3333333333333333333333333333333333333333";
        let logs = json!([{
            "address": AAVE_V3_POOL_ARBITRUM,
            "blockNumber": "0x64",
            "blockHash": BLOCK_HASH,
            "transactionIndex": "0x1",
            "transactionHash": REORG_HASH,
            "logIndex": "0x2",
            "removed": false,
            "topics": [
                AAVE_BORROW_TOPIC,
                format!("0x{}", "0".repeat(64)),
                format!("0x{}{}", "0".repeat(24), borrower),
                format!("0x{}", "0".repeat(64))
            ],
            "data": "0x01"
        }]);
        let normalized = normalize_aave_tail_logs(&logs, 100, 100).unwrap();
        assert_eq!(normalized.len(), 1);
        assert_eq!(normalized[0].borrower, format!("0x{borrower}"));
        let mut removed = logs;
        removed[0]["removed"] = json!(true);
        assert_eq!(
            normalize_aave_tail_logs(&removed, 100, 100),
            Err(GatewayError::ProviderIntegrity)
        );
    }
}
