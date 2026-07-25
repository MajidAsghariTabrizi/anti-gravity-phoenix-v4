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
use crate::transport::{JsonRpcClient, RpcCallResult, TransportError};
use ethabi::{ParamType, Token};
use primitive_types::U256;
use serde_json::{json, Value};
use std::collections::{HashMap, HashSet};
use std::sync::Arc;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};
use thiserror::Error;
use tokio::sync::{watch, Mutex};

const ARBITRUM_CHAIN_ID_HEX: &str = "0xa4b1";
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

type SharedBundleResult = Option<Result<ProviderBundle, GatewayError>>;
type SharedVerificationResult = Option<Result<VerificationEvidence, GatewayError>>;
type SharedHeadResult = Option<Result<HeadSnapshot, GatewayError>>;

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
            | Self::ResponseOversized => 502,
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

#[derive(Clone)]
pub struct GatewayRuntime {
    providers: Arc<Mutex<ProviderPool>>,
    request_budget: Arc<Mutex<GlobalBudget>>,
    upstream_budget: Arc<Mutex<GlobalBudget>>,
    static_cache: Arc<Mutex<TtlCache<()>>>,
    route_cache: Arc<Mutex<TtlCache<ProviderBundle>>>,
    verification_cache: Arc<Mutex<TtlCache<VerificationEvidence>>>,
    hunter_state_cache: Arc<Mutex<TtlCache<Vec<ProviderStateAgreement>>>>,
    primary_in_flight: Arc<Mutex<HashMap<String, watch::Receiver<SharedBundleResult>>>>,
    verification_in_flight: Arc<Mutex<HashMap<String, watch::Receiver<SharedVerificationResult>>>>,
    head: Arc<Mutex<Option<HeadSnapshot>>>,
    head_in_flight: Arc<Mutex<Option<watch::Receiver<SharedHeadResult>>>>,
    chain_verified: Arc<Mutex<HashSet<String>>>,
    multicall_verified: Arc<Mutex<HashSet<String>>>,
    provider_verification_locks: Arc<Mutex<HashMap<String, Arc<Mutex<()>>>>>,
    upstream_operation_lock: Arc<Mutex<()>>,
    client: Arc<dyn JsonRpcClient>,
    timeouts: MethodTimeouts,
    metrics: RuntimeRpcMetrics,
    readiness: GatewayReadiness,
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
            primary_in_flight: Arc::new(Mutex::new(HashMap::new())),
            verification_in_flight: Arc::new(Mutex::new(HashMap::new())),
            head: Arc::new(Mutex::new(None)),
            head_in_flight: Arc::new(Mutex::new(None)),
            chain_verified: Arc::new(Mutex::new(HashSet::new())),
            multicall_verified: Arc::new(Mutex::new(HashSet::new())),
            provider_verification_locks: Arc::new(Mutex::new(HashMap::new())),
            upstream_operation_lock: Arc::new(Mutex::new(())),
            client,
            timeouts,
            metrics,
            readiness,
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
        if self.provider_count().await < 2 {
            return Err(GatewayError::ProviderUnavailable);
        }
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
        self.mark_provider_success(secondary.provider_id()).await;

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
        if probe {
            self.metrics.probe_call();
        }
        if let Some(inner_calls) = multicall_inner {
            self.metrics.multicall_request(inner_calls);
        }
        let result = self
            .client
            .call(provider, method, params, self.timeouts.timeout_for(method))
            .await;
        let outcome = match result {
            Ok(_) => UpstreamOutcome::Success,
            Err(TransportError::Timeout) => UpstreamOutcome::Timeout,
            Err(TransportError::RateLimited { .. }) => UpstreamOutcome::RateLimited,
            Err(_) => UpstreamOutcome::Failure,
        };
        self.metrics.upstream_call(method, outcome, slot);
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

#[derive(Clone, Debug)]
struct HeadSnapshot {
    provider_id: String,
    block: PinnedBlock,
    observed_at: Instant,
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

fn unix_time_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis()
        .min(u128::from(u64::MAX)) as u64
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::providers::parse_provider_config;
    use crate::shadow_state::{PoolStateRequest, SHADOW_STATE_SCHEMA_VERSION};
    use async_trait::async_trait;
    use ethabi::{decode, encode, ParamType, Token};
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::sync::Mutex as StdMutex;
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    use tokio::net::TcpListener;

    const BLOCK_HASH: &str = "0xaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";
    const REORG_HASH: &str = "0xbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb";
    const NEXT_HASH: &str = "0xcccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccc";

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
        rate_limit_once: StdMutex<HashSet<String>>,
        disagreement: AtomicBool,
        malformed_multicall: AtomicBool,
        delay_multicall: Duration,
    }

    impl Default for ModelClient {
        fn default() -> Self {
            Self {
                calls: StdMutex::new(Vec::new()),
                head: StdMutex::new(PinnedBlock {
                    number: 100,
                    hash: BLOCK_HASH.to_string(),
                }),
                rate_limit_once: StdMutex::new(HashSet::new()),
                disagreement: AtomicBool::new(false),
                malformed_multicall: AtomicBool::new(false),
                delay_multicall: Duration::ZERO,
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

        fn calls(&self) -> Vec<CallRecord> {
            self.calls.lock().unwrap().clone()
        }

        fn rate_limit_next_multicall(&self, provider_id: &str) {
            self.rate_limit_once
                .lock()
                .unwrap()
                .insert(provider_id.to_string());
        }

        fn block_for_tag(&self, tag: &str) -> PinnedBlock {
            let head = self.head.lock().unwrap().clone();
            if tag == "latest" {
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
                RpcMethod::EthGetBlockByNumber => {
                    let block = self.block_for_tag(params[0].as_str().unwrap());
                    json!({"number": format_quantity(block.number), "hash": block.hash})
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
                        tokio::time::sleep(self.delay_multicall).await;
                    }
                    if self.malformed_multicall.load(Ordering::Relaxed) {
                        json!("0x1234")
                    } else {
                        self.multicall_response(provider.provider_id(), &params)
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
        let config =
            parse_provider_config("https://primary.example,https://secondary.example", "2,1")
                .unwrap();
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
        client.rate_limit_next_multicall("provider_0");
        let runtime = runtime(client.clone());
        let response = runtime.resolve_shadow_state(request()).await.unwrap();
        assert_eq!(response.primary_provider_id, "provider_1");
        let calls = client.calls();
        assert_eq!(
            calls
                .iter()
                .filter(|call| {
                    call.provider_id == "provider_0" && call.method == RpcMethod::EthCall
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
}
