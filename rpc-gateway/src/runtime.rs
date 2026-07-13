use crate::budget::GlobalBudget;
use crate::cache::TtlCache;
use crate::economic::{
    compare_provider_results, MethodTimeouts, PinnedBlock, ProviderResult, RpcMethod,
};
use crate::metrics::RuntimeRpcMetrics;
use crate::providers::{ProviderConfig, ProviderLease, ProviderPool};
use crate::runtime_state::GatewayReadiness;
use crate::shadow_state::{
    canonical_block_hash, canonical_data, canonical_hash_bytes, GatewayErrorResponse,
    PoolStateResponse, RpcQualityEvidence, ShadowStateRequest, ShadowStateResponse,
    ARBITRUM_ONE_CHAIN_ID, MAX_GATEWAY_RESPONSE_BYTES, SHADOW_STATE_SCHEMA_VERSION,
};
use crate::transport::{JsonRpcClient, TransportError};
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
const MAX_STATE_RESPONSE_DATA_BYTES: usize = 4096;
const SHADOW_STATE_CACHE_CAPACITY: usize = 1024;
const SHADOW_STATE_CACHE_TTL: Duration = Duration::from_millis(100);
const MAX_IN_FLIGHT_REQUESTS: usize = 64;
const MAX_STATE_RESOLUTION: Duration = Duration::from_secs(25);
const MAX_COALESCE_WAIT: Duration = Duration::from_secs(26);

type SharedResult = Option<Result<ShadowStateResponse, GatewayError>>;

#[derive(Clone, Copy, Debug, Error, PartialEq, Eq)]
pub enum GatewayError {
    #[error("RPC Gateway request contract is invalid")]
    InvalidRequest,
    #[error("RPC Gateway request budget is exhausted")]
    BudgetExhausted,
    #[error("RPC Gateway has no eligible provider")]
    ProviderUnavailable,
    #[error("RPC Gateway provider evidence failed integrity validation")]
    ProviderIntegrity,
    #[error("RPC Gateway response exceeded the configured bound")]
    ResponseOversized,
}

impl GatewayError {
    pub const fn class(self) -> &'static str {
        match self {
            Self::InvalidRequest => "invalid_request",
            Self::BudgetExhausted => "request_budget_exhausted",
            Self::ProviderUnavailable => "provider_unavailable",
            Self::ProviderIntegrity => "provider_integrity_failure",
            Self::ResponseOversized => "gateway_response_oversized",
        }
    }

    pub const fn retryable(self) -> bool {
        matches!(self, Self::BudgetExhausted | Self::ProviderUnavailable)
    }

    pub const fn http_status(self) -> u16 {
        match self {
            Self::InvalidRequest => 400,
            Self::BudgetExhausted => 429,
            Self::ProviderUnavailable => 503,
            Self::ProviderIntegrity | Self::ResponseOversized => 502,
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
    budget: Arc<Mutex<GlobalBudget>>,
    cache: Arc<Mutex<TtlCache<ShadowStateResponse>>>,
    in_flight: Arc<Mutex<HashMap<String, watch::Receiver<SharedResult>>>>,
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
        let now = Instant::now();
        let global_rps = config.global_rps;
        Self {
            providers: Arc::new(Mutex::new(config.into_pool(now))),
            budget: Arc::new(Mutex::new(GlobalBudget::new(
                global_rps,
                Duration::from_secs(1),
                now,
            ))),
            cache: Arc::new(Mutex::new(TtlCache::new(SHADOW_STATE_CACHE_CAPACITY))),
            in_flight: Arc::new(Mutex::new(HashMap::new())),
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
        let provider_count = self.provider_count().await;
        let mut excluded = HashSet::with_capacity(provider_count);
        for retry_count in 0..provider_count {
            let Some(provider) = self.reserve_provider(&excluded).await else {
                break;
            };
            excluded.insert(provider.provider_id().to_string());
            self.metrics.provider_request();
            match self
                .client
                .call(
                    &provider,
                    RpcMethod::EthChainId,
                    json!([]),
                    self.timeouts.timeout_for(RpcMethod::EthChainId),
                )
                .await
            {
                Ok(result) if result.value.as_str() == Some(ARBITRUM_CHAIN_ID_HEX) => {
                    self.metrics.observe_latency(result.latency_ns);
                    self.mark_provider_success(provider.provider_id()).await;
                    self.readiness.set_provider_healthy(true);
                    return Ok(());
                }
                Ok(result) => {
                    self.metrics.observe_latency(result.latency_ns);
                    self.mark_provider_failure(provider.provider_id()).await;
                }
                Err(error) => {
                    self.observe_transport_failure(error);
                    self.mark_provider_failure(provider.provider_id()).await;
                }
            }
            if retry_count + 1 < provider_count {
                self.metrics.provider_retry();
            }
        }
        self.readiness.set_provider_healthy(false);
        Err(GatewayError::ProviderUnavailable)
    }

    pub async fn resolve_shadow_state(
        &self,
        request: ShadowStateRequest,
    ) -> Result<ShadowStateResponse, GatewayError> {
        request
            .validate()
            .map_err(|_| GatewayError::InvalidRequest)?;
        let request_hash = request
            .canonical_hash()
            .map_err(|_| GatewayError::InvalidRequest)?;
        self.metrics.request();
        if let Some(response) = self.cache.lock().await.get(&request_hash, Instant::now()) {
            self.metrics.cache_hit();
            return Ok(response);
        }

        match self.coalesce_role(&request_hash).await? {
            CoalesceRole::Follower(mut receiver) => {
                self.metrics.coalesced_request();
                tokio::time::timeout(MAX_COALESCE_WAIT, async move {
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
            CoalesceRole::Leader(sender) => {
                if let Some(response) = self.cache.lock().await.get(&request_hash, Instant::now()) {
                    self.metrics.cache_hit();
                    let result = Ok(response);
                    let _ = sender.send(Some(result.clone()));
                    self.in_flight.lock().await.remove(&request_hash);
                    return result;
                }
                let result = tokio::time::timeout(
                    MAX_STATE_RESOLUTION,
                    self.resolve_uncached(request, request_hash.clone()),
                )
                .await
                .unwrap_or(Err(GatewayError::ProviderUnavailable));
                if let Ok(response) = &result {
                    self.cache.lock().await.insert(
                        request_hash.clone(),
                        response.clone(),
                        SHADOW_STATE_CACHE_TTL,
                        Instant::now(),
                    );
                }
                let _ = sender.send(Some(result.clone()));
                self.in_flight.lock().await.remove(&request_hash);
                result
            }
        }
    }

    async fn resolve_uncached(
        &self,
        request: ShadowStateRequest,
        request_hash: String,
    ) -> Result<ShadowStateResponse, GatewayError> {
        let started = Instant::now();
        if !self.budget.lock().await.admit(Instant::now()) {
            self.metrics.rate_limited();
            self.metrics.budget_rejected();
            return Err(GatewayError::BudgetExhausted);
        }

        let mut primary_excluded = HashSet::new();
        let mut primary_failures = Vec::new();
        let primary = self
            .bundle_with_failover(&request, None, &mut primary_excluded, &mut primary_failures)
            .await?
            .ok_or(GatewayError::ProviderUnavailable)?;
        let mut quality = primary.quality.clone();
        self.readiness.set_provider_healthy(true);

        let mut secondary_excluded = HashSet::from([primary.provider_id.clone()]);
        let mut secondary_failures = Vec::new();
        let secondary = self
            .bundle_with_failover(
                &request,
                Some(primary.block.clone()),
                &mut secondary_excluded,
                &mut secondary_failures,
            )
            .await?;
        quality.extend(secondary_failures);
        let mut agreement_provider_id = None;
        let mut provider_agreement = false;
        if let Some(secondary) = secondary {
            let primary_result = primary.provider_result();
            let secondary_result = secondary.provider_result();
            provider_agreement =
                compare_provider_results(&primary.block, &primary_result, &secondary_result)
                    .is_ok();
            agreement_provider_id = Some(secondary.provider_id.clone());
            quality.extend(secondary.quality);
            if !provider_agreement {
                self.metrics.provider_disagreement();
                for entry in &mut quality {
                    if entry.success {
                        entry.disagreement = true;
                    }
                }
            }
        }

        let response = ShadowStateResponse {
            schema_version: SHADOW_STATE_SCHEMA_VERSION.to_string(),
            chain_id: ARBITRUM_ONE_CHAIN_ID,
            request_hash,
            block_number: primary.block.number,
            block_hash: primary.block.hash,
            pools: primary.pools,
            primary_provider_id: primary.provider_id,
            agreement_provider_id,
            provider_agreement,
            quality,
            resolved_at_unix_ms: unix_time_ms(),
        };
        let encoded = serde_json::to_vec(&response).map_err(|_| GatewayError::ProviderIntegrity)?;
        if encoded.len() > MAX_GATEWAY_RESPONSE_BYTES {
            return Err(GatewayError::ResponseOversized);
        }
        self.metrics.observe_latency(started.elapsed().as_nanos());
        Ok(response)
    }

    async fn coalesce_role(&self, request_hash: &str) -> Result<CoalesceRole, GatewayError> {
        let mut in_flight = self.in_flight.lock().await;
        if let Some(receiver) = in_flight.get(request_hash).cloned() {
            if receiver.has_changed().is_ok() {
                return Ok(CoalesceRole::Follower(receiver));
            }
            in_flight.remove(request_hash);
        }
        if in_flight.len() >= MAX_IN_FLIGHT_REQUESTS {
            self.metrics.rate_limited();
            self.metrics.budget_rejected();
            return Err(GatewayError::BudgetExhausted);
        }
        let (sender, receiver) = watch::channel(None);
        in_flight.insert(request_hash.to_string(), receiver);
        Ok(CoalesceRole::Leader(sender))
    }

    async fn bundle_with_failover(
        &self,
        request: &ShadowStateRequest,
        expected: Option<PinnedBlock>,
        excluded: &mut HashSet<String>,
        failed_evidence: &mut Vec<RpcQualityEvidence>,
    ) -> Result<Option<ProviderBundle>, GatewayError> {
        let provider_count = self.provider_count().await;
        let mut failed_quality = Vec::new();
        for retry_count in 0..provider_count {
            let Some(provider) = self.reserve_provider(excluded).await else {
                if excluded.len() < provider_count {
                    self.metrics.circuit_open();
                }
                break;
            };
            excluded.insert(provider.provider_id().to_string());
            match self
                .perform_bundle(&provider, request, expected.as_ref(), retry_count as u16)
                .await
            {
                Ok(mut bundle) => {
                    self.mark_provider_success(provider.provider_id()).await;
                    failed_quality.append(&mut bundle.quality);
                    bundle.quality = failed_quality;
                    return Ok(Some(bundle));
                }
                Err(mut failure) => {
                    self.mark_provider_failure(provider.provider_id()).await;
                    failed_quality.append(&mut failure.quality);
                    if retry_count + 1 < provider_count {
                        self.metrics.provider_retry();
                    }
                }
            }
        }
        failed_evidence.append(&mut failed_quality);
        if expected.is_some() {
            Ok(None)
        } else {
            self.readiness.set_provider_healthy(false);
            Err(GatewayError::ProviderUnavailable)
        }
    }

    async fn perform_bundle(
        &self,
        provider: &ProviderLease,
        request: &ShadowStateRequest,
        expected: Option<&PinnedBlock>,
        retry_count: u16,
    ) -> Result<ProviderBundle, BundleFailure> {
        let mut quality = Vec::with_capacity(4 + request.pools.len() * 2);
        let chain_id = self
            .recorded_call(
                provider,
                RpcMethod::EthChainId,
                json!([]),
                None,
                retry_count,
            )
            .await
            .map_err(|failure| failure.with_prior(quality.clone()))?;
        quality.push(chain_id.quality);
        if chain_id.value.as_str() != Some(ARBITRUM_CHAIN_ID_HEX) {
            return Err(BundleFailure::integrity(quality));
        }

        let block_tag = expected
            .map(|block| format_quantity(block.number))
            .unwrap_or_else(|| "latest".to_string());
        let block_call = self
            .recorded_call(
                provider,
                RpcMethod::EthGetBlockByNumber,
                json!([block_tag, false]),
                expected,
                retry_count,
            )
            .await
            .map_err(|failure| failure.with_prior(quality.clone()))?;
        quality.push(block_call.quality);
        let block = parse_block(&block_call.value)
            .ok_or_else(|| BundleFailure::integrity(quality.clone()))?;
        if expected.is_some_and(|expected| expected != &block) {
            return Err(BundleFailure::integrity(quality));
        }

        let mut pools = Vec::with_capacity(request.pools.len());
        for pool in &request.pools {
            let token0 = self
                .recorded_call(
                    provider,
                    RpcMethod::EthCall,
                    json!([
                        {"to": pool.address, "data": TOKEN0_SELECTOR},
                        format_quantity(block.number)
                    ]),
                    Some(&block),
                    retry_count,
                )
                .await
                .map_err(|failure| failure.with_prior(quality.clone()))?;
            quality.push(token0.quality);
            let token1 = self
                .recorded_call(
                    provider,
                    RpcMethod::EthCall,
                    json!([
                        {"to": pool.address, "data": TOKEN1_SELECTOR},
                        format_quantity(block.number)
                    ]),
                    Some(&block),
                    retry_count,
                )
                .await
                .map_err(|failure| failure.with_prior(quality.clone()))?;
            quality.push(token1.quality);
            let fee = self
                .recorded_call(
                    provider,
                    RpcMethod::EthCall,
                    json!([
                        {"to": pool.address, "data": FEE_SELECTOR},
                        format_quantity(block.number)
                    ]),
                    Some(&block),
                    retry_count,
                )
                .await
                .map_err(|failure| failure.with_prior(quality.clone()))?;
            quality.push(fee.quality);
            let slot0 = self
                .recorded_call(
                    provider,
                    RpcMethod::EthCall,
                    json!([
                        {"to": pool.address, "data": SLOT0_SELECTOR},
                        format_quantity(block.number)
                    ]),
                    Some(&block),
                    retry_count,
                )
                .await
                .map_err(|failure| failure.with_prior(quality.clone()))?;
            quality.push(slot0.quality);
            let liquidity = self
                .recorded_call(
                    provider,
                    RpcMethod::EthCall,
                    json!([
                        {"to": pool.address, "data": LIQUIDITY_SELECTOR},
                        format_quantity(block.number)
                    ]),
                    Some(&block),
                    retry_count,
                )
                .await
                .map_err(|failure| failure.with_prior(quality.clone()))?;
            quality.push(liquidity.quality);
            let Some(slot0) = normalize_state_data(slot0.value, 64) else {
                return Err(BundleFailure::integrity(quality));
            };
            let Some(liquidity) = normalize_state_data(liquidity.value, 32) else {
                return Err(BundleFailure::integrity(quality));
            };
            let Some(token0) = parse_address_result(&token0.value) else {
                return Err(BundleFailure::integrity(quality));
            };
            let Some(token1) = parse_address_result(&token1.value) else {
                return Err(BundleFailure::integrity(quality));
            };
            let Some(fee) = parse_u32_result(&fee.value) else {
                return Err(BundleFailure::integrity(quality));
            };
            if token0 != pool.token0 || token1 != pool.token1 || fee != pool.fee {
                return Err(BundleFailure::integrity(quality));
            }
            let state_material = serde_json::to_vec(&(
                &pool.pool_id,
                &pool.address,
                &pool.protocol,
                &token0,
                &token1,
                fee,
                &slot0,
                &liquidity,
            ))
            .map_err(|_| BundleFailure::integrity(quality.clone()))?;
            pools.push(PoolStateResponse {
                pool_id: pool.pool_id.clone(),
                address: pool.address.clone(),
                protocol: pool.protocol.clone(),
                token0,
                token1,
                fee,
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
                Some(&block),
                retry_count,
            )
            .await
            .map_err(|failure| failure.with_prior(quality.clone()))?;
        quality.push(verify.quality);
        if parse_block(&verify.value).as_ref() != Some(&block) {
            return Err(BundleFailure::integrity(quality));
        }

        let normalized =
            serde_json::to_vec(&pools).map_err(|_| BundleFailure::integrity(quality.clone()))?;
        Ok(ProviderBundle {
            provider_id: provider.provider_id().to_string(),
            block,
            pools,
            normalized_response_hash: canonical_hash_bytes(&normalized),
            quality,
        })
    }

    async fn recorded_call(
        &self,
        provider: &ProviderLease,
        method: RpcMethod,
        params: Value,
        block: Option<&PinnedBlock>,
        retry_count: u16,
    ) -> Result<RecordedCall, BundleFailure> {
        self.metrics.provider_request();
        let result = self
            .client
            .call(provider, method, params, self.timeouts.timeout_for(method))
            .await;
        match result {
            Ok(result) => {
                self.metrics.observe_latency(result.latency_ns);
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
            Err(error) => {
                self.observe_transport_failure(error);
                Err(BundleFailure {
                    quality: vec![RpcQualityEvidence {
                        provider_id: provider.provider_id().to_string(),
                        method: method.as_str().to_string(),
                        block_number: block.map(|value| value.number),
                        block_hash: block.map(|value| value.hash.clone()),
                        response_hash: None,
                        latency_ns: 0,
                        success: false,
                        stale_result: false,
                        disagreement: false,
                        timeout: error.timeout(),
                        retry_count,
                    }],
                })
            }
        }
    }

    fn observe_transport_failure(&self, error: TransportError) {
        if error.timeout() {
            self.metrics.provider_timeout();
        }
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

enum CoalesceRole {
    Leader(watch::Sender<SharedResult>),
    Follower(watch::Receiver<SharedResult>),
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
    normalized_response_hash: String,
    quality: Vec<RpcQualityEvidence>,
}

impl ProviderBundle {
    fn provider_result(&self) -> ProviderResult {
        ProviderResult {
            provider_id: self.provider_id.clone(),
            block: self.block.clone(),
            normalized_response_hash: self.normalized_response_hash.clone(),
            latency_ns: self
                .quality
                .iter()
                .map(|entry| entry.latency_ns as u128)
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
struct BundleFailure {
    quality: Vec<RpcQualityEvidence>,
}

impl BundleFailure {
    fn integrity(quality: Vec<RpcQualityEvidence>) -> Self {
        Self { quality }
    }

    fn with_prior(mut self, mut prior: Vec<RpcQualityEvidence>) -> Self {
        prior.append(&mut self.quality);
        self.quality = prior;
        self
    }
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

fn normalize_state_data(value: Value, minimum_bytes: usize) -> Option<String> {
    let value = value.as_str()?.to_ascii_lowercase();
    let bytes = value.strip_prefix("0x")?.len() / 2;
    if bytes < minimum_bytes || !canonical_data(&value, MAX_STATE_RESPONSE_DATA_BYTES) {
        None
    } else {
        Some(value)
    }
}

fn parse_address_result(value: &Value) -> Option<String> {
    let value = normalize_state_data(value.clone(), 32)?;
    if value.len() != 66 || value[2..26].bytes().any(|byte| byte != b'0') {
        return None;
    }
    Some(format!("0x{}", &value[26..66]))
}

fn parse_u32_result(value: &Value) -> Option<u32> {
    let value = normalize_state_data(value.clone(), 32)?;
    if value.len() != 66 || value[2..58].bytes().any(|byte| byte != b'0') {
        return None;
    }
    u32::from_str_radix(&value[58..66], 16).ok()
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
        .min(u64::MAX as u128) as u64
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::providers::parse_provider_config;
    use crate::shadow_state::{PoolStateRequest, SHADOW_STATE_SCHEMA_VERSION};
    use crate::transport::RpcCallResult;
    use async_trait::async_trait;
    use std::collections::{HashMap, VecDeque};
    use std::sync::Mutex as StdMutex;

    const BLOCK_HASH: &str = "0xaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";
    type ScriptKey = (String, RpcMethod);
    type ScriptResult = Result<Value, TransportError>;
    type ScriptQueue = HashMap<ScriptKey, VecDeque<ScriptResult>>;

    #[derive(Debug, Default)]
    struct ScriptedClient {
        responses: StdMutex<ScriptQueue>,
        calls: StdMutex<Vec<(String, RpcMethod, Value)>>,
        delay: Duration,
    }

    impl ScriptedClient {
        fn push(&self, provider: &str, method: RpcMethod, value: Result<Value, TransportError>) {
            self.responses
                .lock()
                .unwrap()
                .entry((provider.to_string(), method))
                .or_default()
                .push_back(value);
        }
    }

    #[async_trait]
    impl JsonRpcClient for ScriptedClient {
        async fn call(
            &self,
            provider: &ProviderLease,
            method: RpcMethod,
            params: Value,
            _timeout: Duration,
        ) -> Result<RpcCallResult, TransportError> {
            if !self.delay.is_zero() {
                tokio::time::sleep(self.delay).await;
            }
            self.calls
                .lock()
                .unwrap()
                .push((provider.provider_id().to_string(), method, params));
            self.responses
                .lock()
                .unwrap()
                .get_mut(&(provider.provider_id().to_string(), method))
                .and_then(VecDeque::pop_front)
                .unwrap_or(Err(TransportError::InvalidResponse))
                .map(|value| RpcCallResult {
                    value,
                    latency_ns: 100,
                })
        }
    }

    fn request() -> ShadowStateRequest {
        ShadowStateRequest {
            schema_version: SHADOW_STATE_SCHEMA_VERSION.to_string(),
            chain_id: ARBITRUM_ONE_CHAIN_ID,
            route_fingerprint: "route-v1".to_string(),
            pools: vec![PoolStateRequest {
                pool_id: "pool-a".to_string(),
                address: "0x1111111111111111111111111111111111111111".to_string(),
                protocol: "UniswapV3".to_string(),
                token0: "0x3333333333333333333333333333333333333333".to_string(),
                token1: "0x4444444444444444444444444444444444444444".to_string(),
                fee: 500,
            }],
        }
    }

    fn runtime(client: Arc<ScriptedClient>) -> GatewayRuntime {
        let config = parse_provider_config(
            "https://primary.example,https://secondary.example",
            "2,1",
            "20",
        )
        .unwrap();
        GatewayRuntime::new(
            config,
            client,
            MethodTimeouts {
                eth_call: Duration::from_secs(2),
                state_read: Duration::from_secs(2),
                logs: Duration::from_secs(5),
            },
            RuntimeRpcMetrics::default(),
            GatewayReadiness::new(true),
        )
    }

    fn script_bundle(client: &ScriptedClient, provider: &str, slot_byte: char) {
        client.push(
            provider,
            RpcMethod::EthChainId,
            Ok(json!(ARBITRUM_CHAIN_ID_HEX)),
        );
        client.push(
            provider,
            RpcMethod::EthGetBlockByNumber,
            Ok(json!({"number": "0x64", "hash": BLOCK_HASH})),
        );
        client.push(
            provider,
            RpcMethod::EthCall,
            Ok(json!(format!("0x{}{}", "0".repeat(24), "3".repeat(40)))),
        );
        client.push(
            provider,
            RpcMethod::EthCall,
            Ok(json!(format!("0x{}{}", "0".repeat(24), "4".repeat(40)))),
        );
        client.push(
            provider,
            RpcMethod::EthCall,
            Ok(json!(format!("0x{:064x}", 500))),
        );
        client.push(
            provider,
            RpcMethod::EthCall,
            Ok(json!(format!("0x{}", slot_byte.to_string().repeat(128)))),
        );
        client.push(
            provider,
            RpcMethod::EthCall,
            Ok(json!(format!("0x{}", "b".repeat(64)))),
        );
        client.push(
            provider,
            RpcMethod::EthGetBlockByNumber,
            Ok(json!({"number": "0x64", "hash": BLOCK_HASH})),
        );
    }

    #[tokio::test]
    async fn every_pool_read_is_pinned_and_two_provider_agreement_is_explicit() {
        let client = Arc::new(ScriptedClient::default());
        script_bundle(&client, "provider_0", 'a');
        script_bundle(&client, "provider_1", 'a');
        let response = runtime(client.clone())
            .resolve_shadow_state(request())
            .await
            .unwrap();
        assert_eq!(response.block_number, 100);
        assert_eq!(response.block_hash, BLOCK_HASH);
        assert!(response.provider_agreement);
        assert_eq!(
            response.agreement_provider_id.as_deref(),
            Some("provider_1")
        );
        let calls = client.calls.lock().unwrap();
        let pool_calls = calls
            .iter()
            .filter(|(_, method, _)| *method == RpcMethod::EthCall)
            .collect::<Vec<_>>();
        assert!(!pool_calls.is_empty());
        assert!(pool_calls.iter().all(|(_, _, params)| params[1] == "0x64"));
    }

    #[tokio::test]
    async fn same_block_state_disagreement_is_returned_as_fail_closed_evidence() {
        let client = Arc::new(ScriptedClient::default());
        script_bundle(&client, "provider_0", 'a');
        script_bundle(&client, "provider_1", 'c');
        let response = runtime(client)
            .resolve_shadow_state(request())
            .await
            .unwrap();
        assert!(!response.provider_agreement);
        assert!(response
            .quality
            .iter()
            .filter(|entry| entry.success)
            .all(|entry| entry.disagreement));
    }

    #[tokio::test]
    async fn failed_agreement_provider_attempt_remains_in_quality_evidence() {
        let client = Arc::new(ScriptedClient::default());
        script_bundle(&client, "provider_0", 'a');
        client.push(
            "provider_1",
            RpcMethod::EthChainId,
            Err(TransportError::Timeout),
        );
        let response = runtime(client)
            .resolve_shadow_state(request())
            .await
            .unwrap();
        assert!(!response.provider_agreement);
        assert!(response
            .quality
            .iter()
            .any(|entry| { entry.provider_id == "provider_1" && !entry.success && entry.timeout }));
    }

    #[tokio::test]
    async fn concurrent_identical_state_reads_share_one_pinned_result_and_short_cache() {
        let client = Arc::new(ScriptedClient {
            delay: Duration::from_millis(2),
            ..ScriptedClient::default()
        });
        script_bundle(&client, "provider_0", 'a');
        script_bundle(&client, "provider_1", 'a');
        let runtime = runtime(client.clone());
        let state_request = request();
        let (first, follower) = tokio::join!(
            runtime.resolve_shadow_state(state_request.clone()),
            runtime.resolve_shadow_state(state_request.clone())
        );
        let first = first.unwrap();
        assert_eq!(follower.unwrap(), first);
        assert_eq!(client.calls.lock().unwrap().len(), 16);

        let cached = runtime.resolve_shadow_state(state_request).await.unwrap();
        assert_eq!(cached, first);
        assert_eq!(client.calls.lock().unwrap().len(), 16);
        let rendered = runtime.metrics().render(&runtime.readiness());
        assert!(rendered.contains("rpc_coalesced_requests_total 1"));
        assert!(rendered.contains("rpc_cache_hits_total 1"));
    }

    #[tokio::test]
    async fn preferred_provider_failure_uses_next_provider_without_exposing_url() {
        let client = Arc::new(ScriptedClient::default());
        client.push(
            "provider_0",
            RpcMethod::EthChainId,
            Err(TransportError::Timeout),
        );
        script_bundle(&client, "provider_1", 'a');
        let response = runtime(client)
            .resolve_shadow_state(request())
            .await
            .unwrap();
        assert_eq!(response.primary_provider_id, "provider_1");
        let rendered = serde_json::to_string(&response).unwrap();
        assert!(!rendered.contains("primary.example"));
        assert!(!rendered.contains("secondary.example"));
    }

    #[test]
    fn quantity_and_block_parsing_reject_ambiguous_material() {
        assert!(canonical_quantity("0x0"));
        assert!(canonical_quantity("0xa4b1"));
        assert!(!canonical_quantity("latest"));
        assert!(!canonical_quantity("0x00"));
        assert!(parse_block(&json!({"number": "latest", "hash": BLOCK_HASH})).is_none());
    }

    #[test]
    fn gateway_errors_are_sanitized_and_statuses_are_stable() {
        for error in [
            GatewayError::InvalidRequest,
            GatewayError::BudgetExhausted,
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
