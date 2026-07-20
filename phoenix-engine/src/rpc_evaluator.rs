use crate::amm::v3::{
    amount_less_fee, current_range_input_capacity, quote_spot_exact_input,
    simulate_current_range_exact_input,
};
use crate::decision::{decide, DecisionContext, ShadowPolicy};
use crate::domain::{Amount, Direction, DomainError, Liquidity, OpportunityId, SqrtPriceX96, Tick};
use crate::economics::{evaluate_scenarios, EconomicInput};
use crate::engine_input::EngineInput;
use crate::metrics::RuntimeMetrics;
use crate::opportunity::{
    AgreementState, BasisPoints, DecisionEvidence,
    IndependentVerificationStatus as OpportunityIndependentVerificationStatus, MarketEvidence,
    MonetaryUnit, Opportunity, OpportunityIdentity, OutcomeEvidence, PoolStateEvidence,
    PrimaryProfitabilityStatus, RejectionReason, RouteEvidence, ScenarioEconomics,
    ShadowDisposition, SignedAmount, SimulationClassification, SimulationEvidence, SimulationKind,
    StateSource, Strategy, VerificationSkipReason,
    VerificationStatus as OpportunityVerificationStatus,
};
use crate::optimizer::{
    calculate_profit_threshold, generate_candidate_sizes, ProfitThreshold, ProfitThresholdConfig,
    SizeLadderConfig,
};
use crate::origin::OriginEvent;
use crate::shadow_processor::{
    CandidateBatch, CandidateEvaluator, EvaluatedOpportunity, EvaluationError, RuntimeRoute,
};
use crate::state::{PoolState, StateCompleteness};
use async_trait::async_trait;
use futures_util::StreamExt;
use primitive_types::U256;
use reqwest::{Client, StatusCode, Url};
use rpc_gateway::shadow_state::{
    canonical_block_hash, canonical_data, canonical_digest, canonical_hash_bytes, EvidenceRequest,
    GatewayErrorResponse, IndependentVerificationStatus as GatewayIndependentVerificationStatus,
    PoolStateRequest, RpcQualityEvidence, ShadowStateRequest, ShadowStateResponse,
    VerificationStatus as GatewayVerificationStatus, ARBITRUM_ONE_CHAIN_ID,
    MAX_GATEWAY_REQUEST_BYTES, MAX_GATEWAY_RESPONSE_BYTES, SHADOW_STATE_SCHEMA_VERSION,
};
use serde::Serialize;
use serde_json::json;
use sha2::{Digest, Sha256};
use std::collections::BTreeSet;
use std::fmt;
use std::sync::Arc;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};
use thiserror::Error;
use tokio::sync::Semaphore;

const DEFAULT_GATEWAY_TIMEOUT: Duration = Duration::from_secs(5);
const MAX_PROVIDER_QUALITY_RECORDS: usize = 512;
const MAX_CLOCK_SKEW_MS: u64 = 30_000;
const MAX_EVALUATION_CONCURRENCY: usize = 8;
const STATE_MODEL_CONFIDENCE_BPS: u16 = 7_000;
const STRATEGY_VERSION: &str = "two-pool-v3-profitability-scale-v1";
const DETECTOR_VERSION: &str = "exact-input-single-v1";
const POLICY_VERSION: &str = "shadow-profitability-scale-v1";
const ARBITRUM_WETH: &str = "0x82af49447d8a07e3bd95bd0d56f35241523fbab1";
const PROFITABILITY_EVIDENCE_SCHEMA_VERSION: &str = "phoenix.profitability_scale.v1";

#[derive(Clone, Debug, Serialize)]
struct LegLiquidityEvidence {
    leg_index: usize,
    pool_id: String,
    input_asset: String,
    input_decimals: u8,
    input_amount: String,
    input_amount_less_fee: String,
    current_range_input_capacity: Option<String>,
    utilization_bps: Option<String>,
    complete: bool,
}

#[derive(Clone, Debug, Serialize)]
struct FeeComponentEvidence {
    protocol_fees: String,
    pool_fees: String,
    price_impact: String,
    slippage_allowance: String,
    flash_loan_premium: String,
    arbitrum_execution_fee: String,
    l1_data_fee: String,
    contract_overhead: String,
    failed_attempt_reserve: String,
    stale_state_reserve: String,
    ordering_reserve: String,
    state_drift_reserve: String,
    latency_reserve: String,
    uncertainty_reserve: String,
    base_total_cost: String,
    conservative_total_cost: String,
    severe_total_cost: String,
}

#[derive(Clone, Debug, Serialize)]
struct ProfitThresholdEvidence {
    configured_absolute_minimum: String,
    configured_input_relative_minimum: String,
    conservative_cost_safety_buffer: String,
    required: String,
}

#[derive(Clone, Debug, Serialize)]
struct CandidateSizeEvidence {
    candidate_size: String,
    settlement_asset: String,
    settlement_asset_decimals: u8,
    monetary_unit: &'static str,
    status: &'static str,
    selected: bool,
    liquidity_evidence_complete: bool,
    liquidity: Vec<LegLiquidityEvidence>,
    maximum_liquidity_utilization_bps: Option<u16>,
    spot_output: Option<String>,
    no_fee_curve_output: Option<String>,
    expected_output: Option<String>,
    price_impact: Option<String>,
    price_impact_bps: Option<u16>,
    slippage_bps: Option<u16>,
    fee_components: Option<FeeComponentEvidence>,
    expected_net_pnl: Option<String>,
    conservative_net_pnl: Option<String>,
    severe_net_pnl: Option<String>,
    minimum_required_net_pnl: Option<String>,
    threshold_components: Option<ProfitThresholdEvidence>,
    rejection_reason: Option<&'static str>,
}

#[derive(Clone, Debug)]
struct CandidateAttempt {
    evidence: CandidateSizeEvidence,
    evaluation: Option<AmountEvaluation>,
    economic_gate_passed: bool,
}

#[derive(Clone, Debug)]
struct LadderEvaluation {
    attempts: Vec<CandidateAttempt>,
    selected_index: Option<usize>,
    economic_fallback_index: Option<usize>,
    incomplete_state_seen: bool,
}

#[derive(Clone, Copy, Debug, Error, PartialEq, Eq)]
pub enum GatewayClientError {
    #[error("RPC Gateway transport is temporarily unavailable")]
    Retryable,
    #[error("RPC Gateway response failed integrity validation")]
    Integrity,
}

#[async_trait]
pub trait ShadowStateClient: Send + Sync {
    async fn fetch(
        &self,
        request: &ShadowStateRequest,
    ) -> Result<ShadowStateResponse, GatewayClientError>;

    async fn ready(&self) -> Result<bool, GatewayClientError>;
}

#[derive(Clone)]
pub struct RpcGatewayClient {
    client: Client,
    state_endpoint: Url,
    readiness_endpoint: Url,
}

impl fmt::Debug for RpcGatewayClient {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("RpcGatewayClient")
            .finish_non_exhaustive()
    }
}

impl RpcGatewayClient {
    pub fn new(base_url: &str) -> Result<Self, GatewayClientError> {
        let mut base = Url::parse(base_url).map_err(|_| GatewayClientError::Integrity)?;
        if !matches!(base.scheme(), "http" | "https")
            || base.host_str().is_none()
            || !base.username().is_empty()
            || base.password().is_some()
            || base.query().is_some()
            || base.fragment().is_some()
        {
            return Err(GatewayClientError::Integrity);
        }
        base.set_path("/");
        let state_endpoint = base
            .join("v1/shadow/state")
            .map_err(|_| GatewayClientError::Integrity)?;
        let readiness_endpoint = base
            .join("readyz")
            .map_err(|_| GatewayClientError::Integrity)?;
        let client = Client::builder()
            .redirect(reqwest::redirect::Policy::none())
            .connect_timeout(DEFAULT_GATEWAY_TIMEOUT)
            .timeout(DEFAULT_GATEWAY_TIMEOUT)
            .pool_max_idle_per_host(8)
            .build()
            .map_err(|_| GatewayClientError::Integrity)?;
        Ok(Self {
            client,
            state_endpoint,
            readiness_endpoint,
        })
    }

    async fn bounded_body(
        response: reqwest::Response,
        maximum: usize,
    ) -> Result<Vec<u8>, GatewayClientError> {
        if response
            .content_length()
            .is_some_and(|length| length > maximum as u64)
        {
            return Err(GatewayClientError::Integrity);
        }
        let mut body = Vec::new();
        let mut chunks = response.bytes_stream();
        while let Some(chunk) = chunks.next().await {
            let chunk = chunk.map_err(|_| GatewayClientError::Retryable)?;
            if body.len().saturating_add(chunk.len()) > maximum {
                return Err(GatewayClientError::Integrity);
            }
            body.extend_from_slice(&chunk);
        }
        Ok(body)
    }
}

#[async_trait]
impl ShadowStateClient for RpcGatewayClient {
    async fn fetch(
        &self,
        request: &ShadowStateRequest,
    ) -> Result<ShadowStateResponse, GatewayClientError> {
        let encoded = serde_json::to_vec(request).map_err(|_| GatewayClientError::Integrity)?;
        if encoded.len() > MAX_GATEWAY_REQUEST_BYTES {
            return Err(GatewayClientError::Integrity);
        }
        let response = self
            .client
            .post(self.state_endpoint.clone())
            .header("content-type", "application/json")
            .body(encoded)
            .send()
            .await
            .map_err(|_| GatewayClientError::Retryable)?;
        let status = response.status();
        let body = Self::bounded_body(response, MAX_GATEWAY_RESPONSE_BYTES).await?;
        if status == StatusCode::OK {
            return serde_json::from_slice(&body).map_err(|_| GatewayClientError::Integrity);
        }
        let failure: GatewayErrorResponse =
            serde_json::from_slice(&body).map_err(|_| GatewayClientError::Integrity)?;
        if failure.schema_version != SHADOW_STATE_SCHEMA_VERSION
            || failure.error_class.is_empty()
            || failure.error_class.len() > 64
            || failure.error_class.chars().any(char::is_control)
        {
            return Err(GatewayClientError::Integrity);
        }
        if failure.retryable && matches!(status.as_u16(), 429 | 503) {
            Err(GatewayClientError::Retryable)
        } else {
            Err(GatewayClientError::Integrity)
        }
    }

    async fn ready(&self) -> Result<bool, GatewayClientError> {
        let response = self
            .client
            .get(self.readiness_endpoint.clone())
            .send()
            .await
            .map_err(|_| GatewayClientError::Retryable)?;
        let status = response.status();
        let body = Self::bounded_body(response, 1024).await?;
        Ok(status == StatusCode::OK && body == b"ready\n")
    }
}

#[derive(Clone)]
pub struct RpcCandidateEvaluator {
    client: Arc<dyn ShadowStateClient>,
    code_version: String,
    metrics: RuntimeMetrics,
    evaluation_permits: Arc<Semaphore>,
}

impl fmt::Debug for RpcCandidateEvaluator {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("RpcCandidateEvaluator")
            .field("code_version", &self.code_version)
            .finish_non_exhaustive()
    }
}

impl RpcCandidateEvaluator {
    pub fn new(
        client: Arc<dyn ShadowStateClient>,
        code_version: String,
    ) -> Result<Self, &'static str> {
        Self::with_metrics(client, code_version, RuntimeMetrics::default())
    }

    pub fn with_metrics(
        client: Arc<dyn ShadowStateClient>,
        code_version: String,
        metrics: RuntimeMetrics,
    ) -> Result<Self, &'static str> {
        Self::with_metrics_and_concurrency(client, code_version, metrics, 1)
    }

    pub fn with_metrics_and_concurrency(
        client: Arc<dyn ShadowStateClient>,
        code_version: String,
        metrics: RuntimeMetrics,
        max_evaluation_concurrency: usize,
    ) -> Result<Self, &'static str> {
        if !bounded_label(&code_version, 1, 128)
            || !(1..=MAX_EVALUATION_CONCURRENCY).contains(&max_evaluation_concurrency)
        {
            return Err("invalid Engine code version");
        }
        Ok(Self {
            client,
            code_version,
            metrics,
            evaluation_permits: Arc::new(Semaphore::new(max_evaluation_concurrency)),
        })
    }
}

#[async_trait]
impl CandidateEvaluator for RpcCandidateEvaluator {
    async fn evaluate(
        &self,
        input: &EngineInput,
        origin: &OriginEvent,
        route: &RuntimeRoute,
    ) -> Result<CandidateBatch, EvaluationError> {
        let _permit = self
            .evaluation_permits
            .acquire()
            .await
            .map_err(|_| EvaluationError::Terminal("evaluation_concurrency_closed"))?;
        let request = state_request(route)?;
        let requested_at = Instant::now();
        let primary_response = self
            .client
            .fetch(&request)
            .await
            .map_err(|error| match error {
                GatewayClientError::Retryable => {
                    EvaluationError::Transient("rpc_gateway_unavailable")
                }
                GatewayClientError::Integrity => {
                    EvaluationError::Terminal("rpc_gateway_response_integrity_failure")
                }
            })?;
        let now_ms = unix_time_ms();
        validate_response(&request, &primary_response, now_ms)?;
        let primary_response_hash =
            canonical_hash_bytes(&serde_json::to_vec(&primary_response).map_err(|_| {
                EvaluationError::Terminal("rpc_gateway_response_integrity_failure")
            })?);
        let pools = decode_pools(route, &primary_response)?;
        let gas_price_wei = parse_decimal_u128(&input.normalized.max_fee_per_gas)
            .ok_or(EvaluationError::Terminal("economic_input_out_of_range"))?;
        let primary_ladder = evaluate_ladder(route, &pools, gas_price_wei)?;
        let primary_profitability = profitability_scale_evidence(route, &primary_ladder);
        let Some(primary_selected_index) = primary_ladder.selected_index else {
            self.metrics.rpc_primary_screen_rejected();
            self.metrics.rpc_secondary_skipped();
            let fallback = primary_ladder
                .economic_fallback_index
                .and_then(|index| primary_ladder.attempts[index].evaluation.clone());
            if fallback.is_none() {
                self.metrics
                    .profitability_without_candidate(primary_ladder.incomplete_state_seen);
            }
            let evaluations = fallback
                .map(|selected| {
                    build_opportunity(
                        input,
                        origin,
                        route,
                        &primary_response,
                        &pools,
                        primary_response_hash.clone(),
                        selected,
                        true,
                        Some(VerificationSkipReason::PrimaryScreenNoProfitableCandidate),
                        requested_at.elapsed(),
                        now_ms,
                        &self.code_version,
                    )
                    .map(|opportunity| {
                        vec![EvaluatedOpportunity {
                            opportunity,
                            rpc_quality: primary_response.quality.clone(),
                        }]
                    })
                })
                .transpose()?
                .unwrap_or_default();
            return Ok(CandidateBatch {
                evaluations,
                evidence: json!({
                    "state": {
                        "state_block": primary_response.block_number,
                        "state_block_hash": &primary_response.block_hash,
                        "state_hash": &primary_response.state_hash,
                        "route_config_hash": &primary_response.route_config_hash,
                        "primary_response_hash": primary_response_hash,
                        "primary_provider_id": &primary_response.primary_provider_id,
                        "verification_status": primary_response.verification_status,
                        "independent_verification_status": "not_requested",
                        "independent_verification_lifecycle": ["not_requested"],
                        "verification_skip_reason": "primary_screen_no_profitable_candidate",
                        "rpc_quality_record_count": primary_response.quality.len(),
                        "state_model_scope": "verified_current_tick_range_only",
                        "incomplete_state_seen": primary_ladder.incomplete_state_seen,
                        "primary_screen_rejected": true,
                        "secondary_skipped": true
                    },
                    "profitability_scale": {
                        "primary": primary_profitability,
                        "verified": null
                    }
                }),
            });
        };
        let primary_selected_amount = primary_ladder.attempts[primary_selected_index]
            .evaluation
            .as_ref()
            .ok_or(EvaluationError::Terminal(
                "shadow_model_selection_integrity_failure",
            ))?
            .input;
        let verification_request = verification_request(&request, &primary_response)?;
        let response = match self.client.fetch(&verification_request).await {
            Ok(response) => response,
            Err(GatewayClientError::Retryable) => synthesize_failed_verification(
                &primary_response,
                &verification_request,
                GatewayIndependentVerificationStatus::ProviderUnavailable,
            )?,
            Err(GatewayClientError::Integrity) => synthesize_failed_verification(
                &primary_response,
                &verification_request,
                GatewayIndependentVerificationStatus::IntegrityFailure,
            )?,
        };
        let now_ms = unix_time_ms();
        validate_response(&verification_request, &response, now_ms)?;
        let verified_pools = decode_pools(route, &response)?;
        let verified_ladder = evaluate_ladder(route, &verified_pools, gas_price_wei)?;
        let verified_profitability = profitability_scale_evidence(route, &verified_ladder);
        let verification_response_hash =
            canonical_hash_bytes(&serde_json::to_vec(&response).map_err(|_| {
                EvaluationError::Terminal("rpc_gateway_response_integrity_failure")
            })?);
        let base_evidence = json!({
            "state_block": response.block_number,
            "state_block_hash": response.block_hash,
            "state_hash": response.state_hash,
            "route_config_hash": response.route_config_hash,
            "primary_response_hash": primary_response_hash,
            "verification_response_hash": verification_response_hash,
            "primary_provider_id": response.primary_provider_id,
            "agreement_provider_id": response.agreement_provider_id,
            "secondary_state_hash": response.secondary_state_hash,
            "secondary_block_number": response.secondary_block_number,
            "secondary_block_hash": response.secondary_block_hash,
            "secondary_route_config_hash": response.secondary_route_config_hash,
            "provider_agreement": response.provider_agreement,
            "verification_status": response.verification_status,
            "independent_verification_status": response.independent_verification_status,
            "independent_verification_lifecycle": [
                "requested",
                response.independent_verification_status
            ],
            "rpc_quality_record_count": response.quality.len(),
            "state_model_scope": "verified_current_tick_range_only",
            "incomplete_state_seen": verified_ladder.incomplete_state_seen
        });
        let selected_index = verified_ladder
            .selected_index
            .or(verified_ladder.economic_fallback_index);
        let Some(selected_index) = selected_index else {
            self.metrics
                .profitability_without_candidate(verified_ladder.incomplete_state_seen);
            return Ok(CandidateBatch {
                evaluations: Vec::new(),
                evidence: json!({
                    "state": base_evidence,
                    "profitability_scale": {
                        "primary_selected_input_amount": primary_selected_amount.0.to_string(),
                        "primary": primary_profitability,
                        "verified": verified_profitability
                    }
                }),
            });
        };
        let selected = verified_ladder.attempts[selected_index]
            .evaluation
            .clone()
            .ok_or(EvaluationError::Terminal(
                "shadow_model_selection_integrity_failure",
            ))?;
        let opportunity = build_opportunity(
            input,
            origin,
            route,
            &response,
            &verified_pools,
            primary_response_hash,
            selected,
            true,
            None,
            requested_at.elapsed(),
            now_ms,
            &self.code_version,
        )?;
        Ok(CandidateBatch {
            evaluations: vec![EvaluatedOpportunity {
                opportunity,
                rpc_quality: response.quality,
            }],
            evidence: json!({
                "state": base_evidence,
                "profitability_scale": {
                    "primary_selected_input_amount": primary_selected_amount.0.to_string(),
                    "primary": primary_profitability,
                    "verified": verified_profitability
                }
            }),
        })
    }
}

fn state_request(route: &RuntimeRoute) -> Result<ShadowStateRequest, EvaluationError> {
    if route.state_targets.len() != route.route.legs.len()
        || route.leg_units.len() != route.route.legs.len()
    {
        return Err(EvaluationError::Terminal("route_state_target_mismatch"));
    }
    let pools = route
        .route
        .legs
        .iter()
        .zip(route.state_targets.iter().zip(&route.leg_units))
        .map(|(leg, (target, units))| {
            let (token0, token1, token0_decimals, token1_decimals) = match leg.direction {
                Direction::ZeroForOne => (
                    &leg.token_in,
                    &leg.token_out,
                    units.token_in_decimals,
                    units.token_out_decimals,
                ),
                Direction::OneForZero => (
                    &leg.token_out,
                    &leg.token_in,
                    units.token_out_decimals,
                    units.token_in_decimals,
                ),
            };
            PoolStateRequest {
                pool_id: leg.pool_id.0.clone(),
                address: target.as_str().to_string(),
                protocol: leg.protocol.clone(),
                token0: token0.0.as_str().to_string(),
                token1: token1.0.as_str().to_string(),
                token0_decimals,
                token1_decimals,
                fee: leg.fee,
                tick_spacing: units.tick_spacing,
            }
        })
        .collect();
    let request = ShadowStateRequest {
        schema_version: SHADOW_STATE_SCHEMA_VERSION.to_string(),
        chain_id: ARBITRUM_ONE_CHAIN_ID,
        route_fingerprint: route.fingerprint.clone(),
        pools,
        evidence: EvidenceRequest::Primary,
    };
    request
        .validate()
        .map_err(|_| EvaluationError::Terminal("route_state_request_invalid"))?;
    Ok(request)
}

fn verification_request(
    primary_request: &ShadowStateRequest,
    primary_response: &ShadowStateResponse,
) -> Result<ShadowStateRequest, EvaluationError> {
    let mut request = primary_request.clone();
    request.evidence = EvidenceRequest::Verify {
        block_number: primary_response.block_number,
        block_hash: primary_response.block_hash.clone(),
        primary_state_hash: primary_response.state_hash.clone(),
    };
    request
        .validate()
        .map_err(|_| EvaluationError::Terminal("route_state_request_invalid"))?;
    Ok(request)
}

fn synthesize_failed_verification(
    primary: &ShadowStateResponse,
    request: &ShadowStateRequest,
    status: GatewayIndependentVerificationStatus,
) -> Result<ShadowStateResponse, EvaluationError> {
    if !matches!(
        status,
        GatewayIndependentVerificationStatus::ProviderUnavailable
            | GatewayIndependentVerificationStatus::IntegrityFailure
    ) {
        return Err(EvaluationError::Terminal(
            "rpc_gateway_response_integrity_failure",
        ));
    }
    let mut response = primary.clone();
    response.request_hash = request
        .canonical_hash()
        .map_err(|_| EvaluationError::Terminal("route_state_request_invalid"))?;
    response.agreement_provider_id = None;
    response.secondary_state_hash = None;
    response.secondary_block_number = None;
    response.secondary_block_hash = None;
    response.secondary_route_config_hash = None;
    response.provider_agreement = false;
    response.verification_status = GatewayVerificationStatus::SecondaryUnavailable;
    response.independent_verification_status = status;
    Ok(response)
}

fn validate_response(
    request: &ShadowStateRequest,
    response: &ShadowStateResponse,
    now_ms: u64,
) -> Result<(), EvaluationError> {
    let request_hash = request
        .canonical_hash()
        .map_err(|_| EvaluationError::Terminal("route_state_request_invalid"))?;
    let route_config_hash = request
        .route_config_hash()
        .map_err(|_| EvaluationError::Terminal("route_state_request_invalid"))?;
    if response.schema_version != SHADOW_STATE_SCHEMA_VERSION
        || response.chain_id != ARBITRUM_ONE_CHAIN_ID
        || response.request_hash != request_hash
        || response.route_config_hash != route_config_hash
        || response.block_number == 0
        || !canonical_block_hash(&response.block_hash)
        || !canonical_digest(&response.state_hash)
        || response.pools.len() != request.pools.len()
        || !bounded_label(&response.primary_provider_id, 1, 128)
        || response.primary_provider_id.contains("://")
        || response.resolved_at_unix_ms == 0
        || response.resolved_at_unix_ms > now_ms.saturating_add(MAX_CLOCK_SKEW_MS)
        || response.quality.is_empty()
        || response.quality.len() > MAX_PROVIDER_QUALITY_RECORDS
    {
        return Err(EvaluationError::Terminal(
            "rpc_gateway_response_integrity_failure",
        ));
    }
    let pools_hash = canonical_hash_bytes(
        &serde_json::to_vec(&response.pools)
            .map_err(|_| EvaluationError::Terminal("pool_state_identity_mismatch"))?,
    );
    if response.state_hash != pools_hash {
        return Err(EvaluationError::Terminal("pool_state_identity_mismatch"));
    }
    let stage_valid = match &request.evidence {
        EvidenceRequest::Primary => {
            response.verification_status == GatewayVerificationStatus::PrimaryOnly
                && response.independent_verification_status
                    == GatewayIndependentVerificationStatus::NotRequested
                && !response.provider_agreement
                && response.agreement_provider_id.is_none()
                && response.secondary_state_hash.is_none()
                && response.secondary_block_number.is_none()
                && response.secondary_block_hash.is_none()
                && response.secondary_route_config_hash.is_none()
        }
        EvidenceRequest::Verify {
            block_number,
            block_hash,
            primary_state_hash,
        } => {
            response.block_number == *block_number
                && response.block_hash == *block_hash
                && response.state_hash == *primary_state_hash
                && response.verification_status != GatewayVerificationStatus::PrimaryOnly
                && !matches!(
                    response.independent_verification_status,
                    GatewayIndependentVerificationStatus::NotRequested
                        | GatewayIndependentVerificationStatus::Requested
                )
        }
    };
    let secondary_identity_matches = response.secondary_block_number == Some(response.block_number)
        && response.secondary_block_hash.as_deref() == Some(response.block_hash.as_str())
        && response.secondary_route_config_hash.as_deref()
            == Some(response.route_config_hash.as_str());
    let verification_valid = match response.independent_verification_status {
        GatewayIndependentVerificationStatus::NotRequested => {
            response.verification_status == GatewayVerificationStatus::PrimaryOnly
                && !response.provider_agreement
                && response.agreement_provider_id.is_none()
                && response.secondary_state_hash.is_none()
                && response.secondary_block_number.is_none()
                && response.secondary_block_hash.is_none()
                && response.secondary_route_config_hash.is_none()
        }
        GatewayIndependentVerificationStatus::Requested => false,
        GatewayIndependentVerificationStatus::ProviderUnavailable
        | GatewayIndependentVerificationStatus::IntegrityFailure => {
            response.verification_status == GatewayVerificationStatus::SecondaryUnavailable
                && !response.provider_agreement
                && response.agreement_provider_id.is_none()
                && response.secondary_state_hash.is_none()
                && response.secondary_block_number.is_none()
                && response.secondary_block_hash.is_none()
                && response.secondary_route_config_hash.is_none()
        }
        GatewayIndependentVerificationStatus::Agreed => {
            response.verification_status == GatewayVerificationStatus::Agreed
                && secondary_identity_matches
                && response.provider_agreement
                && response.agreement_provider_id.is_some()
                && response.secondary_state_hash.as_deref() == Some(response.state_hash.as_str())
        }
        GatewayIndependentVerificationStatus::Disagreed => {
            response.verification_status == GatewayVerificationStatus::Disagreed
                && secondary_identity_matches
                && !response.provider_agreement
                && response.agreement_provider_id.is_some()
                && response
                    .secondary_state_hash
                    .as_deref()
                    .is_some_and(|value| value != response.state_hash.as_str())
        }
    };
    if !stage_valid || !verification_valid {
        return Err(EvaluationError::Terminal(
            "rpc_gateway_response_integrity_failure",
        ));
    }
    if let Some(agreement_provider) = response.agreement_provider_id.as_deref() {
        if agreement_provider == response.primary_provider_id
            || !bounded_label(agreement_provider, 1, 128)
            || agreement_provider.contains("://")
        {
            return Err(EvaluationError::Terminal(
                "rpc_gateway_response_integrity_failure",
            ));
        }
    } else if response.provider_agreement {
        return Err(EvaluationError::Terminal(
            "rpc_gateway_response_integrity_failure",
        ));
    }
    if response
        .secondary_state_hash
        .as_deref()
        .is_some_and(|value| !canonical_digest(value))
        || response
            .secondary_block_hash
            .as_deref()
            .is_some_and(|value| !canonical_block_hash(value))
        || response
            .secondary_route_config_hash
            .as_deref()
            .is_some_and(|value| !canonical_digest(value))
    {
        return Err(EvaluationError::Terminal(
            "rpc_gateway_response_integrity_failure",
        ));
    }
    for (expected, actual) in request.pools.iter().zip(&response.pools) {
        let state_material = serde_json::to_vec(&(
            &actual.pool_id,
            &actual.address,
            &actual.protocol,
            &actual.token0,
            &actual.token1,
            actual.token0_decimals,
            actual.token1_decimals,
            actual.fee,
            actual.tick_spacing,
            &actual.slot0,
            &actual.liquidity,
        ))
        .map_err(|_| EvaluationError::Terminal("pool_state_identity_mismatch"))?;
        if actual.pool_id != expected.pool_id
            || actual.address != expected.address
            || actual.protocol != expected.protocol
            || actual.token0 != expected.token0
            || actual.token1 != expected.token1
            || actual.token0_decimals != expected.token0_decimals
            || actual.token1_decimals != expected.token1_decimals
            || actual.fee != expected.fee
            || actual.tick_spacing != expected.tick_spacing
            || !canonical_data(&actual.slot0, 4096)
            || !canonical_data(&actual.liquidity, 4096)
            || !canonical_hash(&actual.state_hash)
            || actual.state_hash != canonical_hash_bytes(&state_material)
        {
            return Err(EvaluationError::Terminal("pool_state_identity_mismatch"));
        }
    }
    for quality in &response.quality {
        if !valid_quality(quality, response) {
            return Err(EvaluationError::Terminal(
                "rpc_quality_evidence_integrity_failure",
            ));
        }
    }
    if matches!(
        response.verification_status,
        GatewayVerificationStatus::Agreed | GatewayVerificationStatus::Disagreed
    ) {
        let agreement_provider = response
            .agreement_provider_id
            .as_deref()
            .unwrap_or_default();
        for provider in [response.primary_provider_id.as_str(), agreement_provider] {
            if !response
                .quality
                .iter()
                .any(|quality| quality.provider_id == provider && quality.success)
            {
                return Err(EvaluationError::Terminal(
                    "rpc_quality_evidence_integrity_failure",
                ));
            }
        }
    }
    Ok(())
}

fn valid_quality(quality: &RpcQualityEvidence, response: &ShadowStateResponse) -> bool {
    bounded_label(&quality.provider_id, 1, 128)
        && !quality.provider_id.contains("://")
        && matches!(
            quality.method.as_str(),
            "eth_chainId" | "eth_getBlockByNumber" | "eth_call"
        )
        && quality
            .block_number
            .map_or(true, |block| block == response.block_number)
        && quality
            .block_hash
            .as_deref()
            .map_or(true, |hash| hash == response.block_hash)
        && quality
            .response_hash
            .as_deref()
            .map_or(true, canonical_hash)
        && quality.success == quality.response_hash.is_some()
        && !(quality.success && quality.timeout)
        && !(response.provider_agreement && quality.success && quality.disagreement)
}

fn decode_pools(
    route: &RuntimeRoute,
    response: &ShadowStateResponse,
) -> Result<Vec<PoolState>, EvaluationError> {
    if route.route.legs.len() != route.leg_units.len()
        || route.route.legs.len() != response.pools.len()
    {
        return Err(EvaluationError::Terminal("pool_state_identity_mismatch"));
    }
    route
        .route
        .legs
        .iter()
        .zip(route.leg_units.iter().zip(&response.pools))
        .map(|(leg, (units, state))| {
            let (sqrt_price_x96, tick) = decode_slot0(&state.slot0)?;
            let liquidity = decode_liquidity(&state.liquidity)?;
            let (token0, token1, token0_decimals, token1_decimals) = match leg.direction {
                Direction::ZeroForOne => (
                    leg.token_in.clone(),
                    leg.token_out.clone(),
                    units.token_in_decimals,
                    units.token_out_decimals,
                ),
                Direction::OneForZero => (
                    leg.token_out.clone(),
                    leg.token_in.clone(),
                    units.token_out_decimals,
                    units.token_in_decimals,
                ),
            };
            if state.token0 != token0.0.as_str()
                || state.token1 != token1.0.as_str()
                || state.token0_decimals != token0_decimals
                || state.token1_decimals != token1_decimals
                || state.tick_spacing != units.tick_spacing
            {
                return Err(EvaluationError::Terminal("pool_state_identity_mismatch"));
            }
            Ok(PoolState {
                pool_id: leg.pool_id.clone(),
                token0,
                token1,
                fee: leg.fee,
                tick,
                liquidity,
                sqrt_price_x96,
                completeness: StateCompleteness {
                    min_tick: tick,
                    max_tick: tick,
                },
                last_reconciled_block: response.block_number,
            })
        })
        .collect()
}

fn decode_slot0(value: &str) -> Result<(SqrtPriceX96, Tick), EvaluationError> {
    let body = value
        .strip_prefix("0x")
        .ok_or(EvaluationError::Terminal("pool_state_decode_failure"))?;
    if body.len() < 128 {
        return Err(EvaluationError::Terminal("pool_state_decode_failure"));
    }
    let sqrt = U256::from_str_radix(&body[..64], 16)
        .map_err(|_| EvaluationError::Terminal("pool_state_decode_failure"))?;
    if sqrt.is_zero() || sqrt.bits() > 160 {
        return Err(EvaluationError::Terminal("pool_state_decode_failure"));
    }
    let tick_word = &body[64..128];
    let raw = u32::from_str_radix(&tick_word[58..64], 16)
        .map_err(|_| EvaluationError::Terminal("pool_state_decode_failure"))?;
    let negative = raw & 0x80_0000 != 0;
    let expected_prefix = if negative { b'f' } else { b'0' };
    if tick_word[..58].bytes().any(|byte| byte != expected_prefix) {
        return Err(EvaluationError::Terminal("pool_state_decode_failure"));
    }
    let tick = if negative {
        raw as i32 - (1_i32 << 24)
    } else {
        raw as i32
    };
    if !(-887_272..=887_272).contains(&tick) {
        return Err(EvaluationError::Terminal("pool_state_decode_failure"));
    }
    Ok((SqrtPriceX96(sqrt), Tick(tick)))
}

fn decode_liquidity(value: &str) -> Result<Liquidity, EvaluationError> {
    let body = value
        .strip_prefix("0x")
        .ok_or(EvaluationError::Terminal("pool_state_decode_failure"))?;
    if body.len() != 64 || body[..32].bytes().any(|byte| byte != b'0') {
        return Err(EvaluationError::Terminal("pool_state_decode_failure"));
    }
    let liquidity = u128::from_str_radix(&body[32..64], 16)
        .map_err(|_| EvaluationError::Terminal("pool_state_decode_failure"))?;
    if liquidity == 0 {
        return Err(EvaluationError::Terminal("pool_state_decode_failure"));
    }
    Ok(Liquidity(liquidity))
}

#[derive(Clone, Debug)]
struct AmountEvaluation {
    input: Amount,
    output: Amount,
    leg_outputs: Vec<Amount>,
    spot_output: Amount,
    no_fee_curve_output: Amount,
    price_impact: Amount,
    price_impact_bps: u16,
    slippage_bps: u16,
    liquidity: Vec<LegLiquidityEvidence>,
    maximum_liquidity_utilization_bps: u16,
    threshold: ProfitThreshold,
    economics: ScenarioEconomics,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum ModelError {
    NotViable,
    StateIncomplete,
    Integrity,
}

#[derive(Clone, Debug)]
struct ModelFailure {
    kind: ModelError,
    liquidity: Vec<LegLiquidityEvidence>,
}

fn evaluate_ladder(
    route: &RuntimeRoute,
    pools: &[PoolState],
    gas_price_wei: u128,
) -> Result<LadderEvaluation, EvaluationError> {
    if route.settlement_asset.0.as_str() != ARBITRUM_WETH || route.settlement_asset_decimals != 18 {
        return Err(EvaluationError::Terminal("settlement_asset_unit_mismatch"));
    }
    let sizes = generate_candidate_sizes(&SizeLadderConfig {
        min_amount: route.strategy.min_input_amount,
        max_amount: route.strategy.max_input_amount,
        max_evaluations: route.strategy.max_evaluations,
        explicit_sizes: route.strategy.candidate_sizes.clone(),
        geometric_step_bps: route.strategy.geometric_step_bps,
    })
    .map_err(|_| EvaluationError::Terminal("shadow_size_ladder_invalid"))?;
    let mut attempts = Vec::with_capacity(sizes.len());
    let mut selected_index = None;
    let mut economic_fallback_index = None;
    let mut incomplete_state_seen = false;

    for amount in sizes {
        let evaluation = match evaluate_amount(route, pools, amount, gas_price_wei) {
            Ok(evaluation) => evaluation,
            Err(failure) if failure.kind == ModelError::Integrity => {
                return Err(EvaluationError::Terminal("shadow_model_arithmetic_failure"));
            }
            Err(failure) => {
                incomplete_state_seen |= failure.kind == ModelError::StateIncomplete;
                let rejection_reason = match failure.kind {
                    ModelError::StateIncomplete
                        if failure.liquidity.is_empty()
                            || failure
                                .liquidity
                                .iter()
                                .any(|leg| leg.current_range_input_capacity.is_none()) =>
                    {
                        "liquidity_unknown"
                    }
                    ModelError::StateIncomplete => "liquidity_insufficient",
                    ModelError::NotViable => "quote_incomplete",
                    ModelError::Integrity => unreachable!(),
                };
                attempts.push(CandidateAttempt {
                    evidence: incomplete_candidate_evidence(
                        route,
                        amount,
                        failure.liquidity,
                        rejection_reason,
                    ),
                    evaluation: None,
                    economic_gate_passed: false,
                });
                continue;
            }
        };
        let rejection_reason = if gas_price_wei > route.strategy.max_gas_price_wei {
            Some("gas_price_limit_exceeded")
        } else if route.strategy.estimated_execution_gas > route.strategy.maximum_execution_gas {
            Some("gas_limit_exceeded")
        } else if evaluation.maximum_liquidity_utilization_bps
            > route.strategy.maximum_pool_depth_utilization_bps
        {
            Some("liquidity_insufficient")
        } else if evaluation.price_impact_bps > route.strategy.maximum_price_impact_bps {
            Some("price_impact_limit_exceeded")
        } else if evaluation.slippage_bps > route.strategy.maximum_slippage_bps {
            Some("slippage_limit_exceeded")
        } else if evaluation.economics.conservative.expected_net_pnl
            <= SignedAmount(
                i128::try_from(evaluation.threshold.required.0)
                    .map_err(|_| EvaluationError::Terminal("shadow_model_arithmetic_failure"))?,
            )
        {
            Some("conservative_net_pnl_below_threshold")
        } else {
            None
        };
        let economic_gate_passed = rejection_reason.is_none();
        let attempt_index = attempts.len();
        attempts.push(CandidateAttempt {
            evidence: completed_candidate_evidence(route, &evaluation, rejection_reason),
            evaluation: Some(evaluation),
            economic_gate_passed,
        });

        if economic_gate_passed
            && selected_index.map_or(true, |index| {
                candidate_is_better(&attempts[attempt_index], &attempts[index])
            })
        {
            selected_index = Some(attempt_index);
        }
        if rejection_reason == Some("conservative_net_pnl_below_threshold")
            && economic_fallback_index.map_or(true, |index| {
                candidate_is_better(&attempts[attempt_index], &attempts[index])
            })
        {
            economic_fallback_index = Some(attempt_index);
        }
    }

    if let Some(index) = selected_index {
        for (attempt_index, attempt) in attempts.iter_mut().enumerate() {
            if attempt.economic_gate_passed {
                if attempt_index == index {
                    attempt.evidence.status = "selected";
                    attempt.evidence.selected = true;
                    attempt.evidence.rejection_reason = None;
                } else {
                    attempt.evidence.status = "rejected";
                    attempt.evidence.rejection_reason = Some("lower_conservative_net_pnl");
                }
            }
        }
    }
    Ok(LadderEvaluation {
        attempts,
        selected_index,
        economic_fallback_index,
        incomplete_state_seen,
    })
}

fn candidate_is_better(candidate: &CandidateAttempt, current: &CandidateAttempt) -> bool {
    let candidate = candidate
        .evaluation
        .as_ref()
        .expect("completed candidate has an evaluation");
    let current = current
        .evaluation
        .as_ref()
        .expect("completed candidate has an evaluation");
    candidate.economics.conservative.expected_net_pnl
        > current.economics.conservative.expected_net_pnl
}

fn evaluate_amount(
    route: &RuntimeRoute,
    pools: &[PoolState],
    amount: Amount,
    gas_price_wei: u128,
) -> Result<AmountEvaluation, ModelFailure> {
    let (output, leg_outputs, liquidity) = simulate_actual_route(route, pools, amount)?;
    let (no_fee_curve_output, _) = simulate_complete_route(route, pools, amount, true, &liquidity)?;
    let spot_output = quote_spot_route(route, pools, amount, &liquidity)?;
    let pool_fees = no_fee_curve_output
        .checked_sub(output)
        .map_err(|_| model_failure(ModelError::Integrity, &liquidity))?;
    let price_impact = spot_output
        .checked_sub(no_fee_curve_output)
        .map_err(|_| model_failure(ModelError::Integrity, &liquidity))?;
    let slippage = spot_output
        .checked_sub(output)
        .map_err(|_| model_failure(ModelError::Integrity, &liquidity))?;
    let price_impact_bps = ratio_bps_ceil(price_impact, spot_output)
        .map_err(|kind| model_failure(kind, &liquidity))?;
    let slippage_bps =
        ratio_bps_ceil(slippage, spot_output).map_err(|kind| model_failure(kind, &liquidity))?;
    let mut economics = evaluate_scenarios(&EconomicInput {
        settlement_asset: route.settlement_asset.clone(),
        settlement_asset_decimals: route.settlement_asset_decimals,
        monetary_unit: MonetaryUnit::SettlementAssetBaseUnits,
        principal: amount,
        gross_output: spot_output,
        protocol_fees: route.strategy.protocol_fees,
        pool_fees,
        price_impact,
        minimum_slippage_buffer: bps_amount(spot_output, route.strategy.minimum_slippage_bps)
            .map_err(|kind| model_failure(kind, &liquidity))?,
        flash_loan_fee: bps_amount(amount, route.strategy.flash_premium_bps)
            .map_err(|kind| model_failure(kind, &liquidity))?,
        estimated_execution_gas: route.strategy.estimated_execution_gas,
        gas_price_wei,
        l1_data_fee: route.strategy.l1_data_fee,
        contract_overhead: route.strategy.contract_overhead,
        failed_attempt_gas_cost: route.strategy.failed_attempt_gas_cost,
        failure_probability_bps: route.strategy.failure_probability_bps,
        stale_state_loss: route.strategy.stale_state_loss,
        stale_quote_probability_bps: route.strategy.stale_quote_probability_bps,
        state_drift_reserve: route.strategy.state_drift_reserve,
        latency_reserve: route.strategy.latency_reserve,
        uncertainty_reserve: route.strategy.uncertainty_reserve,
        replacement_transaction_cost: route.strategy.replacement_transaction_cost,
        probability_of_success_bps: route.strategy.probability_of_success_bps,
        minimum_required_net_pnl: SignedAmount(0),
    })
    .map_err(|_| model_failure(ModelError::Integrity, &liquidity))?;
    let threshold = calculate_profit_threshold(
        amount,
        economics.conservative.total_cost,
        ProfitThresholdConfig {
            absolute_minimum: route.strategy.minimum_net_profit,
            input_relative_minimum_bps: route.strategy.minimum_net_profit_bps,
            conservative_cost_multiplier_bps: route.strategy.conservative_cost_multiplier_bps,
        },
    )
    .map_err(|_| model_failure(ModelError::Integrity, &liquidity))?;
    let required = SignedAmount(
        i128::try_from(threshold.required.0)
            .map_err(|_| model_failure(ModelError::Integrity, &liquidity))?,
    );
    economics.minimum_required_net_pnl = required;
    economics.primary_status = if economics.conservative.expected_net_pnl > required {
        PrimaryProfitabilityStatus::MeetsMinimum
    } else {
        PrimaryProfitabilityStatus::BelowMinimum
    };
    let maximum_liquidity_utilization_bps = liquidity
        .iter()
        .filter_map(|leg| {
            leg.utilization_bps
                .as_deref()
                .and_then(|value| value.parse::<u16>().ok())
        })
        .max()
        .ok_or_else(|| model_failure(ModelError::StateIncomplete, &liquidity))?;
    Ok(AmountEvaluation {
        input: amount,
        output,
        leg_outputs,
        spot_output,
        no_fee_curve_output,
        price_impact,
        price_impact_bps,
        slippage_bps,
        liquidity,
        maximum_liquidity_utilization_bps,
        threshold,
        economics,
    })
}

fn simulate_actual_route(
    route: &RuntimeRoute,
    pools: &[PoolState],
    amount: Amount,
) -> Result<(Amount, Vec<Amount>, Vec<LegLiquidityEvidence>), ModelFailure> {
    if pools.len() != route.route.legs.len() || pools.len() != route.leg_units.len() {
        return Err(model_failure(ModelError::Integrity, &[]));
    }
    let mut current = amount;
    let mut leg_outputs = Vec::with_capacity(route.route.legs.len());
    let mut liquidity = Vec::with_capacity(route.route.legs.len());
    for (index, ((leg, pool), units)) in route
        .route
        .legs
        .iter()
        .zip(pools)
        .zip(&route.leg_units)
        .enumerate()
    {
        let amount_in_less_fee = amount_less_fee(current, pool.fee)
            .map_err(|error| domain_model_failure(error, &liquidity))?;
        let capacity = match current_range_input_capacity(pool, leg.direction, units.tick_spacing) {
            Ok(capacity) => capacity,
            Err(error) => {
                liquidity.push(LegLiquidityEvidence {
                    leg_index: index,
                    pool_id: leg.pool_id.0.clone(),
                    input_asset: leg.token_in.0.as_str().to_string(),
                    input_decimals: units.token_in_decimals,
                    input_amount: current.0.to_string(),
                    input_amount_less_fee: amount_in_less_fee.0.to_string(),
                    current_range_input_capacity: None,
                    utilization_bps: None,
                    complete: false,
                });
                return Err(domain_model_failure(error, &liquidity));
            }
        };
        let utilization_bps = ratio_bps_string(amount_in_less_fee, capacity);
        if amount_in_less_fee.0 >= capacity.0 {
            liquidity.push(LegLiquidityEvidence {
                leg_index: index,
                pool_id: leg.pool_id.0.clone(),
                input_asset: leg.token_in.0.as_str().to_string(),
                input_decimals: units.token_in_decimals,
                input_amount: current.0.to_string(),
                input_amount_less_fee: amount_in_less_fee.0.to_string(),
                current_range_input_capacity: Some(capacity.0.to_string()),
                utilization_bps: Some(utilization_bps),
                complete: false,
            });
            return Err(model_failure(ModelError::StateIncomplete, &liquidity));
        }
        let simulation =
            simulate_current_range_exact_input(pool, current, leg.direction, units.tick_spacing)
                .map_err(|error| domain_model_failure(error, &liquidity))?;
        liquidity.push(LegLiquidityEvidence {
            leg_index: index,
            pool_id: leg.pool_id.0.clone(),
            input_asset: leg.token_in.0.as_str().to_string(),
            input_decimals: units.token_in_decimals,
            input_amount: current.0.to_string(),
            input_amount_less_fee: simulation.amount_in_less_fee.0.to_string(),
            current_range_input_capacity: Some(simulation.current_range_capacity.0.to_string()),
            utilization_bps: Some(simulation.utilization_bps.to_string()),
            complete: true,
        });
        current = simulation.amount_out;
        leg_outputs.push(current);
    }
    Ok((current, leg_outputs, liquidity))
}

fn simulate_complete_route(
    route: &RuntimeRoute,
    pools: &[PoolState],
    amount: Amount,
    remove_fees: bool,
    liquidity: &[LegLiquidityEvidence],
) -> Result<(Amount, Vec<Amount>), ModelFailure> {
    let mut current = amount;
    let mut leg_outputs = Vec::with_capacity(route.route.legs.len());
    for ((leg, pool), units) in route.route.legs.iter().zip(pools).zip(&route.leg_units) {
        let mut pool = pool.clone();
        if remove_fees {
            pool.fee = 0;
        }
        current =
            simulate_current_range_exact_input(&pool, current, leg.direction, units.tick_spacing)
                .map(|simulation| simulation.amount_out)
                .map_err(|error| domain_model_failure(error, liquidity))?;
        leg_outputs.push(current);
    }
    Ok((current, leg_outputs))
}

fn quote_spot_route(
    route: &RuntimeRoute,
    pools: &[PoolState],
    amount: Amount,
    liquidity: &[LegLiquidityEvidence],
) -> Result<Amount, ModelFailure> {
    let mut current = amount;
    for (leg, pool) in route.route.legs.iter().zip(pools) {
        current = quote_spot_exact_input(pool, current, leg.direction)
            .map_err(|error| domain_model_failure(error, liquidity))?;
        if current.0 == 0 {
            return Err(model_failure(ModelError::NotViable, liquidity));
        }
    }
    Ok(current)
}

fn domain_model_failure(error: DomainError, liquidity: &[LegLiquidityEvidence]) -> ModelFailure {
    let kind = match error {
        DomainError::StateIncomplete => ModelError::StateIncomplete,
        DomainError::ArithmeticUnderflow => ModelError::NotViable,
        _ => ModelError::Integrity,
    };
    model_failure(kind, liquidity)
}

fn model_failure(kind: ModelError, liquidity: &[LegLiquidityEvidence]) -> ModelFailure {
    ModelFailure {
        kind,
        liquidity: liquidity.to_vec(),
    }
}

fn ratio_bps_ceil(value: Amount, denominator: Amount) -> Result<u16, ModelError> {
    if denominator.0 == 0 || value > denominator {
        return Err(ModelError::Integrity);
    }
    let numerator = U256::from(value.0)
        .checked_mul(U256::from(10_000_u16))
        .ok_or(ModelError::Integrity)?;
    let denominator = U256::from(denominator.0);
    let quotient = numerator / denominator;
    let rounded = quotient
        .checked_add(U256::from(u8::from(
            numerator % denominator != U256::zero(),
        )))
        .ok_or(ModelError::Integrity)?;
    u16::try_from(rounded.low_u32()).map_err(|_| ModelError::Integrity)
}

fn ratio_bps_string(value: Amount, denominator: Amount) -> String {
    if denominator.0 == 0 {
        return "0".to_string();
    }
    let numerator = U256::from(value.0) * U256::from(10_000_u16);
    let denominator = U256::from(denominator.0);
    let quotient = numerator / denominator;
    (quotient + U256::from(u8::from(numerator % denominator != U256::zero()))).to_string()
}

fn incomplete_candidate_evidence(
    route: &RuntimeRoute,
    amount: Amount,
    liquidity: Vec<LegLiquidityEvidence>,
    rejection_reason: &'static str,
) -> CandidateSizeEvidence {
    let liquidity_evidence_complete = !liquidity.is_empty()
        && liquidity
            .iter()
            .all(|leg| leg.current_range_input_capacity.is_some());
    CandidateSizeEvidence {
        candidate_size: amount.0.to_string(),
        settlement_asset: route.settlement_asset.0.as_str().to_string(),
        settlement_asset_decimals: route.settlement_asset_decimals,
        monetary_unit: "settlement_asset_base_units",
        status: "rejected",
        selected: false,
        liquidity_evidence_complete,
        liquidity,
        maximum_liquidity_utilization_bps: None,
        spot_output: None,
        no_fee_curve_output: None,
        expected_output: None,
        price_impact: None,
        price_impact_bps: None,
        slippage_bps: None,
        fee_components: None,
        expected_net_pnl: None,
        conservative_net_pnl: None,
        severe_net_pnl: None,
        minimum_required_net_pnl: None,
        threshold_components: None,
        rejection_reason: Some(rejection_reason),
    }
}

fn completed_candidate_evidence(
    route: &RuntimeRoute,
    evaluation: &AmountEvaluation,
    rejection_reason: Option<&'static str>,
) -> CandidateSizeEvidence {
    let base = &evaluation.economics.base;
    CandidateSizeEvidence {
        candidate_size: evaluation.input.0.to_string(),
        settlement_asset: route.settlement_asset.0.as_str().to_string(),
        settlement_asset_decimals: route.settlement_asset_decimals,
        monetary_unit: "settlement_asset_base_units",
        status: if rejection_reason.is_some() {
            "rejected"
        } else {
            "eligible"
        },
        selected: false,
        liquidity_evidence_complete: evaluation.liquidity.iter().all(|leg| leg.complete),
        liquidity: evaluation.liquidity.clone(),
        maximum_liquidity_utilization_bps: Some(evaluation.maximum_liquidity_utilization_bps),
        spot_output: Some(evaluation.spot_output.0.to_string()),
        no_fee_curve_output: Some(evaluation.no_fee_curve_output.0.to_string()),
        expected_output: Some(evaluation.output.0.to_string()),
        price_impact: Some(evaluation.price_impact.0.to_string()),
        price_impact_bps: Some(evaluation.price_impact_bps),
        slippage_bps: Some(evaluation.slippage_bps),
        fee_components: Some(FeeComponentEvidence {
            protocol_fees: base.protocol_fees.0.to_string(),
            pool_fees: base.pool_fees.0.to_string(),
            price_impact: base.price_impact.0.to_string(),
            slippage_allowance: base.slippage_allowance.0.to_string(),
            flash_loan_premium: base.flash_loan_fee.0.to_string(),
            arbitrum_execution_fee: base.arbitrum_execution_fee.0.to_string(),
            l1_data_fee: base.l1_data_fee.0.to_string(),
            contract_overhead: base.contract_overhead.0.to_string(),
            failed_attempt_reserve: base.failure_cost_reserve.0.to_string(),
            stale_state_reserve: base.stale_state_penalty.0.to_string(),
            ordering_reserve: base.ordering_reserve.0.to_string(),
            state_drift_reserve: base.state_drift_reserve.0.to_string(),
            latency_reserve: base.latency_reserve.0.to_string(),
            uncertainty_reserve: base.uncertainty_reserve.0.to_string(),
            base_total_cost: base.total_cost.0.to_string(),
            conservative_total_cost: evaluation.economics.conservative.total_cost.0.to_string(),
            severe_total_cost: evaluation.economics.severe.total_cost.0.to_string(),
        }),
        expected_net_pnl: Some(evaluation.economics.base.expected_net_pnl.0.to_string()),
        conservative_net_pnl: Some(
            evaluation
                .economics
                .conservative
                .expected_net_pnl
                .0
                .to_string(),
        ),
        severe_net_pnl: Some(evaluation.economics.severe.expected_net_pnl.0.to_string()),
        minimum_required_net_pnl: Some(evaluation.threshold.required.0.to_string()),
        threshold_components: Some(ProfitThresholdEvidence {
            configured_absolute_minimum: evaluation.threshold.absolute_minimum.0.to_string(),
            configured_input_relative_minimum: evaluation
                .threshold
                .input_relative_minimum
                .0
                .to_string(),
            conservative_cost_safety_buffer: evaluation
                .threshold
                .conservative_cost_safety_buffer
                .0
                .to_string(),
            required: evaluation.threshold.required.0.to_string(),
        }),
        rejection_reason,
    }
}

fn profitability_scale_evidence(
    route: &RuntimeRoute,
    ladder: &LadderEvaluation,
) -> serde_json::Value {
    let selected_best_size = ladder.selected_index.and_then(|index| {
        ladder.attempts[index]
            .evaluation
            .as_ref()
            .map(|evaluation| evaluation.input.0.to_string())
    });
    let attempts = ladder
        .attempts
        .iter()
        .map(|attempt| &attempt.evidence)
        .collect::<Vec<_>>();
    json!({
        "schema_version": PROFITABILITY_EVIDENCE_SCHEMA_VERSION,
        "route_id": route.route.route_id.0,
        "route_fingerprint": route.fingerprint,
        "settlement_asset": route.settlement_asset.0.as_str(),
        "settlement_asset_decimals": route.settlement_asset_decimals,
        "monetary_unit": "settlement_asset_base_units",
        "sizing_policy": {
            "minimum_size": route.strategy.min_input_amount.0.to_string(),
            "maximum_size": route.strategy.max_input_amount.0.to_string(),
            "maximum_evaluations": route.strategy.max_evaluations,
            "candidate_sizes": route.strategy.candidate_sizes.as_ref().map(|sizes| {
                sizes.iter().map(|amount| amount.0.to_string()).collect::<Vec<_>>()
            }),
            "geometric_step_bps": route.strategy.geometric_step_bps,
            "maximum_pool_depth_utilization_bps":
                route.strategy.maximum_pool_depth_utilization_bps,
            "maximum_slippage_bps": route.strategy.maximum_slippage_bps,
            "maximum_price_impact_bps": route.strategy.maximum_price_impact_bps,
            "maximum_execution_gas": route.strategy.maximum_execution_gas,
            "minimum_absolute_net_profit":
                route.strategy.minimum_net_profit.0.to_string(),
            "minimum_net_profit_bps": route.strategy.minimum_net_profit_bps,
            "conservative_cost_multiplier_bps":
                route.strategy.conservative_cost_multiplier_bps
        },
        "attempted_size_count": attempts.len(),
        "selected_best_size": selected_best_size,
        "candidate_results": attempts
    })
}

fn bps_amount(amount: Amount, bps: u16) -> Result<Amount, ModelError> {
    amount
        .0
        .checked_mul(bps as u128)
        .and_then(|value| value.checked_add(9_999))
        .map(|value| Amount(value / 10_000))
        .ok_or(ModelError::Integrity)
}

#[allow(clippy::too_many_arguments)]
fn build_opportunity(
    input: &EngineInput,
    origin: &OriginEvent,
    route: &RuntimeRoute,
    response: &ShadowStateResponse,
    pools: &[PoolState],
    response_hash: String,
    selected: AmountEvaluation,
    liquidity_sufficient: bool,
    verification_skip_reason: Option<VerificationSkipReason>,
    evaluation_latency: Duration,
    now_ms: u64,
    code_version: &str,
) -> Result<Opportunity, EvaluationError> {
    let opportunity_id = deterministic_opportunity_id(
        &input.identity.source_event_identity,
        &route.fingerprint,
        &response.block_hash,
        selected.input,
        selected.output,
    );
    let feed_latency_ns = if input.ingested_at_unix_ns > 0 {
        unix_time_ns().saturating_sub(input.ingested_at_unix_ns as u128)
    } else {
        0
    };
    let base_net_pnl = selected.economics.base.expected_net_pnl;
    let mut opportunity = Opportunity {
        identity: OpportunityIdentity {
            opportunity_id: OpportunityId(opportunity_id),
            strategy: Strategy::TwoPoolV3Arbitrage,
            strategy_version: STRATEGY_VERSION.to_string(),
            detector_version: DETECTOR_VERSION.to_string(),
            code_version: code_version.to_string(),
            config_version: route.fingerprint.clone(),
            chain_id: ARBITRUM_ONE_CHAIN_ID,
            source_sequence: input.identity.source_sequence,
            origin_tx_hash: origin.origin_tx_hash.clone(),
            origin_router: origin.router.clone(),
            observed_block: response.block_number,
            observed_at_unix_ms: input.observed_at_unix_ms,
            detected_at_unix_ms: now_ms,
        },
        route: RouteEvidence {
            route_id: route.route.route_id.clone(),
            route_fingerprint: route.fingerprint.clone(),
            token_path: vec![
                route.route.legs[0].token_in.clone(),
                route.route.legs[0].token_out.clone(),
                route.route.legs[1].token_out.clone(),
            ],
            pools: route
                .route
                .legs
                .iter()
                .map(|leg| leg.pool_id.clone())
                .collect(),
            pool_addresses: route.state_targets.clone(),
            protocols: route
                .route
                .legs
                .iter()
                .map(|leg| leg.protocol.clone())
                .collect(),
            settlement_asset: route.settlement_asset.clone(),
            settlement_asset_decimals: route.settlement_asset_decimals,
            monetary_unit: MonetaryUnit::SettlementAssetBaseUnits,
            input_token: route.route.legs[0].token_in.clone(),
            output_token: route.route.legs[1].token_out.clone(),
            input_amount: selected.input,
            flash_loan_amount: selected.input,
            expected_output: selected.output,
            expected_leg_outputs: selected.leg_outputs,
            exact_ordered_legs: route.route.legs.clone(),
        },
        market: MarketEvidence {
            pool_states: response
                .pools
                .iter()
                .zip(pools)
                .map(|(raw, decoded)| PoolStateEvidence {
                    pool: decoded.pool_id.clone(),
                    state_hash: raw.state_hash.clone(),
                    reserve_or_liquidity_summary: format!(
                        "sqrt_price_x96={};tick={};liquidity={}",
                        decoded.sqrt_price_x96.0, decoded.tick.0, decoded.liquidity.0
                    ),
                })
                .collect(),
            state_block: response.block_number,
            state_block_hash: Some(response.block_hash.clone()),
            route_config_hash: Some(response.route_config_hash.clone()),
            quote_block: response.block_number,
            quote_age_ms: now_ms.saturating_sub(response.resolved_at_unix_ms),
            state_source: StateSource::BlockPinnedRpc,
            primary_provider_id: Some(response.primary_provider_id.clone()),
            primary_response_hash: Some(response_hash),
            primary_state_hash: Some(response.state_hash.clone()),
            secondary_provider_id: response.agreement_provider_id.clone(),
            secondary_state_hash: response.secondary_state_hash.clone(),
            secondary_block_number: response.secondary_block_number,
            secondary_block_hash: response.secondary_block_hash.clone(),
            secondary_route_config_hash: response.secondary_route_config_hash.clone(),
            verification_status: opportunity_verification_status(response.verification_status),
            independent_verification_status: opportunity_independent_verification_status(
                response.independent_verification_status,
            ),
            independent_verification_lifecycle: independent_verification_lifecycle(
                response.independent_verification_status,
            ),
            agreement_state: opportunity_agreement_state(response.verification_status),
            verification_skip_reason,
            feed_to_detection_latency_ns: feed_latency_ns,
        },
        economics: selected.economics,
        simulation: SimulationEvidence {
            kind: SimulationKind::StateBased,
            block_number: response.block_number,
            block_hash: Some(response.block_hash.clone()),
            from_address: None,
            target_contract: None,
            contract_code_hash: None,
            calldata_hash: canonical_hash_bytes(input.normalized.calldata.as_bytes()),
            value: Amount::ZERO,
            gas_estimate: Some(route.strategy.estimated_execution_gas),
            gas_used: None,
            simulated_output: Some(selected.output),
            simulated_net_pnl: Some(base_net_pnl),
            revert_reason: None,
            state_overrides_hash: None,
            provider_id: Some(response.primary_provider_id.clone()),
            simulated_at_unix_ms: now_ms,
            latency_ns: evaluation_latency.as_nanos(),
            state_drift_bps: BasisPoints(0),
            classification: if response
                .quality
                .iter()
                .any(|quality| quality.success && quality.stale_result)
            {
                SimulationClassification::StaleState
            } else {
                match response.verification_status {
                    GatewayVerificationStatus::PrimaryOnly => SimulationClassification::NotRun,
                    GatewayVerificationStatus::Agreed => SimulationClassification::Passed,
                    GatewayVerificationStatus::Disagreed
                    | GatewayVerificationStatus::SecondaryUnavailable => {
                        SimulationClassification::ProviderDisagreement
                    }
                }
            },
        },
        decision: DecisionEvidence {
            disposition: ShadowDisposition::Rejected,
            primary_rejection_reason: Some(RejectionReason::SimulationEvidenceInsufficient),
            secondary_rejection_reasons: Vec::new(),
            risk_flags: Vec::new(),
            confidence_bps: 0,
            policy_version: POLICY_VERSION.to_string(),
            shadow_only: true,
            execution_eligible: false,
            execution_request_created: false,
            decided_at_unix_ms: now_ms,
        },
        outcome: OutcomeEvidence {
            opportunity_expires_at_unix_ms: response
                .resolved_at_unix_ms
                .saturating_add(route.strategy.max_quote_age_ms),
            ..OutcomeEvidence::default()
        },
    };
    let minimum = opportunity.economics.minimum_required_net_pnl;
    let policy = ShadowPolicy {
        version: POLICY_VERSION.to_string(),
        allowed_tokens: opportunity
            .route
            .token_path
            .iter()
            .map(|token| token.0.as_str().to_string())
            .collect::<BTreeSet<_>>(),
        allowed_protocols: opportunity.route.protocols.iter().cloned().collect(),
        max_quote_age_ms: route.strategy.max_quote_age_ms,
        max_simulation_age_ms: route.strategy.max_simulation_age_ms,
        max_gas_price_wei: route.strategy.max_gas_price_wei,
        min_base_net_pnl: minimum,
        min_conservative_net_pnl: minimum,
        min_severe_net_pnl: minimum,
        min_confidence_bps: route.strategy.min_confidence_bps,
    };
    opportunity.decision = decide(
        &opportunity,
        &policy,
        DecisionContext {
            now_unix_ms: now_ms,
            duplicate: false,
            sequence_contiguous: true,
            liquidity_known: true,
            liquidity_sufficient,
            rpc_state_agrees: !matches!(
                response.verification_status,
                GatewayVerificationStatus::Disagreed
                    | GatewayVerificationStatus::SecondaryUnavailable
            ),
            contract_path_available: false,
            risk_budget_available: false,
            confidence_bps: STATE_MODEL_CONFIDENCE_BPS,
        },
    );
    opportunity
        .validate_traceability()
        .map_err(|_| EvaluationError::Terminal("opportunity_traceability_failure"))?;
    Ok(opportunity)
}

fn opportunity_verification_status(
    status: GatewayVerificationStatus,
) -> OpportunityVerificationStatus {
    match status {
        GatewayVerificationStatus::PrimaryOnly => OpportunityVerificationStatus::PrimaryOnly,
        GatewayVerificationStatus::Agreed => OpportunityVerificationStatus::Agreed,
        GatewayVerificationStatus::Disagreed => OpportunityVerificationStatus::Disagreed,
        GatewayVerificationStatus::SecondaryUnavailable => {
            OpportunityVerificationStatus::SecondaryUnavailable
        }
    }
}

fn opportunity_independent_verification_status(
    status: GatewayIndependentVerificationStatus,
) -> OpportunityIndependentVerificationStatus {
    match status {
        GatewayIndependentVerificationStatus::NotRequested => {
            OpportunityIndependentVerificationStatus::NotRequested
        }
        GatewayIndependentVerificationStatus::Requested => {
            OpportunityIndependentVerificationStatus::Requested
        }
        GatewayIndependentVerificationStatus::Agreed => {
            OpportunityIndependentVerificationStatus::Agreed
        }
        GatewayIndependentVerificationStatus::Disagreed => {
            OpportunityIndependentVerificationStatus::Disagreed
        }
        GatewayIndependentVerificationStatus::ProviderUnavailable => {
            OpportunityIndependentVerificationStatus::ProviderUnavailable
        }
        GatewayIndependentVerificationStatus::IntegrityFailure => {
            OpportunityIndependentVerificationStatus::IntegrityFailure
        }
    }
}

fn independent_verification_lifecycle(
    status: GatewayIndependentVerificationStatus,
) -> Vec<OpportunityIndependentVerificationStatus> {
    let final_status = opportunity_independent_verification_status(status);
    if status == GatewayIndependentVerificationStatus::NotRequested {
        vec![final_status]
    } else {
        vec![
            OpportunityIndependentVerificationStatus::Requested,
            final_status,
        ]
    }
}

fn opportunity_agreement_state(status: GatewayVerificationStatus) -> AgreementState {
    match status {
        GatewayVerificationStatus::PrimaryOnly => AgreementState::NotChecked,
        GatewayVerificationStatus::Agreed => AgreementState::Agreed,
        GatewayVerificationStatus::Disagreed => AgreementState::Disagreed,
        GatewayVerificationStatus::SecondaryUnavailable => AgreementState::Unavailable,
    }
}

fn deterministic_opportunity_id(
    source_identity: &str,
    route_fingerprint: &str,
    block_hash: &str,
    input: Amount,
    output: Amount,
) -> String {
    let material = format!(
        "phoenix.shadow.opportunity.v1\n{source_identity}\n{route_fingerprint}\n{block_hash}\n{}\n{}",
        input.0, output.0
    );
    let digest = Sha256::digest(material.as_bytes());
    let mut bytes = [0_u8; 16];
    bytes.copy_from_slice(&digest[..16]);
    bytes[6] = (bytes[6] & 0x0f) | 0x80;
    bytes[8] = (bytes[8] & 0x3f) | 0x80;
    format!(
        "{:02x}{:02x}{:02x}{:02x}-{:02x}{:02x}-{:02x}{:02x}-{:02x}{:02x}-{:02x}{:02x}{:02x}{:02x}{:02x}{:02x}",
        bytes[0],
        bytes[1],
        bytes[2],
        bytes[3],
        bytes[4],
        bytes[5],
        bytes[6],
        bytes[7],
        bytes[8],
        bytes[9],
        bytes[10],
        bytes[11],
        bytes[12],
        bytes[13],
        bytes[14],
        bytes[15]
    )
}

fn canonical_hash(value: &str) -> bool {
    value.len() == 64
        && value
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
}

fn parse_decimal_u128(value: &str) -> Option<u128> {
    if value.is_empty()
        || value.len() > 39
        || !value.bytes().all(|byte| byte.is_ascii_digit())
        || (value.len() > 1 && value.starts_with('0'))
    {
        return None;
    }
    value.parse().ok()
}

fn bounded_label(value: &str, minimum: usize, maximum: usize) -> bool {
    value.len() >= minimum && value.len() <= maximum && !value.chars().any(char::is_control)
}

fn unix_time_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis()
        .min(u64::MAX as u128) as u64
}

fn unix_time_ns() -> u128 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::domain::{Address, ChainId, PoolId, RouteId, SequenceNumber, TokenAddress, TxHash};
    use crate::graph::{PoolEdge, Route};
    use crate::messaging::NormalizedTx;
    use crate::opportunity::PrimaryProfitabilityStatus;
    use crate::origin::{
        DecodedSwapKind, OriginEvidence, OuterSelectorKind, RouterKind, UnsupportedReason,
        WrapperKind,
    };
    use crate::persistence::{validate_record, ClassificationRecord};
    use crate::shadow_processor::RuntimeStrategy;
    use chrono::Utc;
    use rpc_gateway::shadow_state::PoolStateResponse;
    use std::sync::atomic::{AtomicU64, Ordering};
    use std::sync::Mutex;

    const BLOCK_HASH: &str = "0xaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";
    const TOKEN0: &str = ARBITRUM_WETH;
    const TOKEN1: &str = "0xaf88d065e77c8cc2239327c5edb3a432268e5831";

    #[derive(Clone, Copy, Debug)]
    enum FakeMode {
        Profitable { agreement: bool },
        Unprofitable,
        Retryable,
        SecondaryRetryable,
        SecondaryIntegrity,
    }

    #[derive(Debug)]
    struct FakeClient {
        mode: Mutex<FakeMode>,
        calls: AtomicU64,
    }

    #[async_trait]
    impl ShadowStateClient for FakeClient {
        async fn fetch(
            &self,
            request: &ShadowStateRequest,
        ) -> Result<ShadowStateResponse, GatewayClientError> {
            self.calls.fetch_add(1, Ordering::Relaxed);
            match *self.mode.lock().unwrap() {
                FakeMode::Retryable => Err(GatewayClientError::Retryable),
                FakeMode::SecondaryRetryable
                    if matches!(request.evidence, EvidenceRequest::Verify { .. }) =>
                {
                    Err(GatewayClientError::Retryable)
                }
                FakeMode::SecondaryIntegrity
                    if matches!(request.evidence, EvidenceRequest::Verify { .. }) =>
                {
                    Err(GatewayClientError::Integrity)
                }
                FakeMode::SecondaryRetryable => Ok(response(request, false, true)),
                FakeMode::SecondaryIntegrity => Ok(response(request, false, true)),
                FakeMode::Unprofitable => Ok(response(request, false, false)),
                FakeMode::Profitable { agreement } => Ok(response(request, agreement, true)),
            }
        }

        async fn ready(&self) -> Result<bool, GatewayClientError> {
            Ok(true)
        }
    }

    fn token(value: &str) -> TokenAddress {
        TokenAddress(Address::parse(value).unwrap())
    }

    fn route() -> RuntimeRoute {
        RuntimeRoute {
            route: Route {
                route_id: RouteId("two-pool".to_string()),
                legs: vec![
                    PoolEdge {
                        pool_id: PoolId("origin-pool".to_string()),
                        protocol: "UniswapV3".to_string(),
                        fee: 500,
                        token_in: token(TOKEN0),
                        token_out: token(TOKEN1),
                        direction: Direction::ZeroForOne,
                    },
                    PoolEdge {
                        pool_id: PoolId("comparison-pool".to_string()),
                        protocol: "SushiSwapV3".to_string(),
                        fee: 500,
                        token_in: token(TOKEN1),
                        token_out: token(TOKEN0),
                        direction: Direction::OneForZero,
                    },
                ],
            },
            fingerprint: "two-pool-v1".to_string(),
            settlement_asset: token(TOKEN0),
            settlement_asset_decimals: 18,
            state_targets: vec![
                Address::parse("0x3333333333333333333333333333333333333333").unwrap(),
                Address::parse("0x4444444444444444444444444444444444444444").unwrap(),
            ],
            leg_units: vec![
                crate::shadow_processor::RuntimeLegUnits {
                    token_in_decimals: 18,
                    token_out_decimals: 6,
                    tick_spacing: 10,
                },
                crate::shadow_processor::RuntimeLegUnits {
                    token_in_decimals: 6,
                    token_out_decimals: 18,
                    tick_spacing: 10,
                },
            ],
            strategy: RuntimeStrategy {
                min_input_amount: Amount(100),
                max_input_amount: Amount(1_000),
                max_evaluations: 4,
                candidate_sizes: Some(vec![Amount(100), Amount(250), Amount(500), Amount(1_000)]),
                geometric_step_bps: None,
                minimum_net_profit: Amount(1),
                minimum_net_profit_bps: 1,
                conservative_cost_multiplier_bps: 10_000,
                maximum_pool_depth_utilization_bps: 10_000,
                maximum_slippage_bps: 10_000,
                maximum_price_impact_bps: 10_000,
                maximum_execution_gas: 1,
                flash_premium_bps: 0,
                minimum_slippage_bps: 1,
                protocol_fees: Amount::ZERO,
                estimated_execution_gas: 1,
                l1_data_fee: Amount::ZERO,
                contract_overhead: Amount::ZERO,
                failed_attempt_gas_cost: Amount::ZERO,
                failure_probability_bps: 0,
                stale_state_loss: Amount::ZERO,
                stale_quote_probability_bps: 0,
                state_drift_reserve: Amount::ZERO,
                latency_reserve: Amount::ZERO,
                uncertainty_reserve: Amount::ZERO,
                replacement_transaction_cost: Amount::ZERO,
                probability_of_success_bps: 10_000,
                max_gas_price_wei: 1_000,
                max_quote_age_ms: 2_000,
                max_simulation_age_ms: 2_000,
                min_confidence_bps: 9_000,
            },
        }
    }

    fn input(now_ms: u64) -> EngineInput {
        EngineInput {
            identity: crate::engine_input::InputIdentity {
                source_event_identity: format!("phoenix.engine.input.v1:1:0x{}", "b".repeat(64)),
                source_sequence: 1,
                tx_hash: format!("0x{}", "b".repeat(64)),
                chain_id: ARBITRUM_ONE_CHAIN_ID,
            },
            normalized: NormalizedTx {
                sequence: SequenceNumber(1),
                tx_hash: TxHash(format!("0x{}", "b".repeat(64))),
                tx_type: "0x02".to_string(),
                chain_id: ChainId(ARBITRUM_ONE_CHAIN_ID),
                from: Address::parse("0x5555555555555555555555555555555555555555").unwrap(),
                to: Some(Address::parse("0x6666666666666666666666666666666666666666").unwrap()),
                nonce: 1,
                value: "0".to_string(),
                calldata: "0x1234".to_string(),
                gas_limit: "300000".to_string(),
                max_fee_per_gas: "0".to_string(),
                max_priority_fee_per_gas: "0".to_string(),
            },
            observed_at_unix_ms: now_ms.saturating_sub(1),
            ingested_at_unix_ns: (now_ms.saturating_sub(1) as i64) * 1_000_000,
            canonical_payload: json!({}),
        }
    }

    fn origin() -> OriginEvent {
        OriginEvent {
            origin_tx_hash: TxHash(format!("0x{}", "b".repeat(64))),
            origin_sequence: SequenceNumber(1),
            router: Address::parse("0x6666666666666666666666666666666666666666").unwrap(),
            decoded_commands: vec!["exactInputSingle".to_string()],
            swap_path: vec![token(TOKEN0), token(TOKEN1)],
            exact_in: true,
            amount: Amount(500),
            candidate_touched_pools: vec![PoolId("origin-pool".to_string())],
            classification_evidence: OriginEvidence {
                router_kind: Some(RouterKind::SwapRouter02),
                outer_selector_kind: OuterSelectorKind::SwapRouter02ExactInputSingle,
                wrapper_kind: WrapperKind::Direct,
                decoded_swap_kind: DecodedSwapKind::V3ExactInputSingle,
                command_count: 1,
                v3_hop_count: 1,
                exact_in: Some(true),
                supported: true,
                unsupported_reason: UnsupportedReason::None,
            },
        }
    }

    fn response(
        request: &ShadowStateRequest,
        agreement: bool,
        profitable: bool,
    ) -> ShadowStateResponse {
        let q96 = U256::from(1_u8) << 96;
        let first_sqrt = q96 * U256::from(11_u8) / U256::from(10_u8);
        let second_sqrt = q96 * U256::from(9_u8) / U256::from(10_u8);
        let pools: Vec<PoolStateResponse> = request
            .pools
            .iter()
            .enumerate()
            .map(|(index, pool)| {
                let tick = if index == 0 || !profitable {
                    1_906
                } else {
                    -2_108
                };
                let tick_word = if tick < 0 {
                    format!("{}{:06x}", "f".repeat(58), (1_i32 << 24) + tick)
                } else {
                    format!("{tick:064x}")
                };
                let slot0 = format!(
                    "0x{:064x}{tick_word}",
                    if index == 0 || !profitable {
                        first_sqrt
                    } else {
                        second_sqrt
                    }
                );
                let liquidity = format!("0x{:064x}", 1_000_000_000_000_u128);
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
                .unwrap();
                PoolStateResponse {
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
                }
            })
            .collect();
        let primary_only = matches!(request.evidence, EvidenceRequest::Primary);
        let providers = if primary_only {
            vec!["provider_0"]
        } else {
            vec!["provider_0", "provider_1"]
        };
        let quality = providers
            .into_iter()
            .map(|provider| RpcQualityEvidence {
                provider_id: provider.to_string(),
                method: "eth_call".to_string(),
                block_number: Some(100),
                block_hash: Some(BLOCK_HASH.to_string()),
                response_hash: Some("e".repeat(64)),
                latency_ns: 100,
                success: true,
                stale_result: false,
                disagreement: !primary_only && !agreement,
                timeout: false,
                retry_count: 0,
            })
            .collect();
        let state_hash = canonical_hash_bytes(&serde_json::to_vec(&pools).unwrap());
        let secondary_state_hash = (!primary_only).then(|| {
            if agreement {
                state_hash.clone()
            } else {
                "f".repeat(64)
            }
        });
        ShadowStateResponse {
            schema_version: SHADOW_STATE_SCHEMA_VERSION.to_string(),
            chain_id: ARBITRUM_ONE_CHAIN_ID,
            request_hash: request.canonical_hash().unwrap(),
            route_config_hash: request.route_config_hash().unwrap(),
            block_number: 100,
            block_hash: BLOCK_HASH.to_string(),
            state_hash,
            pools,
            primary_provider_id: "provider_0".to_string(),
            agreement_provider_id: (!primary_only).then(|| "provider_1".to_string()),
            secondary_state_hash,
            secondary_block_number: (!primary_only).then_some(100),
            secondary_block_hash: (!primary_only).then(|| BLOCK_HASH.to_string()),
            secondary_route_config_hash: (!primary_only)
                .then(|| request.route_config_hash().unwrap()),
            provider_agreement: !primary_only && agreement,
            verification_status: if primary_only {
                GatewayVerificationStatus::PrimaryOnly
            } else if agreement {
                GatewayVerificationStatus::Agreed
            } else {
                GatewayVerificationStatus::Disagreed
            },
            independent_verification_status: if primary_only {
                GatewayIndependentVerificationStatus::NotRequested
            } else if agreement {
                GatewayIndependentVerificationStatus::Agreed
            } else {
                GatewayIndependentVerificationStatus::Disagreed
            },
            quality,
            resolved_at_unix_ms: unix_time_ms(),
        }
    }

    fn evaluator(mode: FakeMode) -> RpcCandidateEvaluator {
        evaluator_with_client(mode).0
    }

    fn evaluator_with_client(
        mode: FakeMode,
    ) -> (RpcCandidateEvaluator, Arc<FakeClient>, RuntimeMetrics) {
        let client = Arc::new(FakeClient {
            mode: Mutex::new(mode),
            calls: AtomicU64::new(0),
        });
        let metrics = RuntimeMetrics::default();
        let evaluator = RpcCandidateEvaluator::with_metrics(
            client.clone(),
            "test-code".to_string(),
            metrics.clone(),
        )
        .unwrap();
        (evaluator, client, metrics)
    }

    #[test]
    fn slot0_decoder_accepts_canonical_signed_ticks_and_uint160_prices() {
        let sqrt = U256::from(1_u8) << 96;
        let positive = format!("0x{sqrt:064x}{:064x}", 123_u32);
        assert_eq!(decode_slot0(&positive), Ok((SqrtPriceX96(sqrt), Tick(123))));
        let negative_word = format!("{}{:06x}", "f".repeat(58), (1_u32 << 24) - 123);
        let negative = format!("0x{sqrt:064x}{negative_word}");
        assert_eq!(
            decode_slot0(&negative),
            Ok((SqrtPriceX96(sqrt), Tick(-123)))
        );
    }

    #[test]
    fn malformed_sign_extension_and_zero_liquidity_fail_closed() {
        let sqrt = U256::from(1_u8) << 96;
        let malformed = format!("0x{sqrt:064x}{}{:06x}", "0".repeat(58), 0x80_0001);
        assert!(decode_slot0(&malformed).is_err());
        assert!(decode_liquidity(&format!("0x{:064x}", 0)).is_err());
    }

    #[test]
    fn independent_verification_requires_distinct_provider_and_identical_context() {
        let primary_request = state_request(&route()).unwrap();
        let primary = response(&primary_request, false, true);
        let verify_request = verification_request(&primary_request, &primary).unwrap();
        let now = unix_time_ms();

        let mut self_verified = response(&verify_request, true, true);
        self_verified.agreement_provider_id = Some(self_verified.primary_provider_id.clone());
        assert!(validate_response(&verify_request, &self_verified, now).is_err());

        let mut wrong_block = response(&verify_request, true, true);
        wrong_block.secondary_block_number = Some(wrong_block.block_number + 1);
        assert!(validate_response(&verify_request, &wrong_block, now).is_err());

        let mut wrong_hash = response(&verify_request, true, true);
        wrong_hash.secondary_block_hash = Some(format!("0x{}", "b".repeat(64)));
        assert!(validate_response(&verify_request, &wrong_hash, now).is_err());

        let mut wrong_route = response(&verify_request, true, true);
        wrong_route.secondary_route_config_hash = Some("b".repeat(64));
        assert!(validate_response(&verify_request, &wrong_route, now).is_err());
    }

    #[test]
    fn transient_requested_status_is_not_accepted_as_final_evidence() {
        let primary_request = state_request(&route()).unwrap();
        let primary = response(&primary_request, false, true);
        let verify_request = verification_request(&primary_request, &primary).unwrap();
        let mut requested = response(&verify_request, true, true);
        requested.independent_verification_status = GatewayIndependentVerificationStatus::Requested;
        assert!(validate_response(&verify_request, &requested, unix_time_ms()).is_err());
    }

    #[test]
    fn deterministic_identity_is_uuid_shaped_and_input_sensitive() {
        let first = deterministic_opportunity_id("source", "route", "block", Amount(1), Amount(2));
        let second = deterministic_opportunity_id("source", "route", "block", Amount(2), Amount(2));
        assert_eq!(first.len(), 36);
        assert_ne!(first, second);
        assert_eq!(&first[14..15], "8");
    }

    #[tokio::test]
    async fn primary_economic_rejection_skips_secondary_verification() {
        let now = unix_time_ms();
        let (evaluator, client, metrics) = evaluator_with_client(FakeMode::Unprofitable);
        let batch = evaluator
            .evaluate(&input(now), &origin(), &route())
            .await
            .unwrap();
        assert_eq!(batch.evaluations.len(), 1);
        assert_eq!(client.calls.load(Ordering::Relaxed), 1);
        let opportunity = &batch.evaluations[0].opportunity;
        assert_eq!(
            opportunity.economics.primary_status,
            PrimaryProfitabilityStatus::BelowMinimum
        );
        assert_eq!(
            opportunity.market.verification_status,
            OpportunityVerificationStatus::PrimaryOnly
        );
        assert_eq!(
            opportunity.market.agreement_state,
            AgreementState::NotChecked
        );
        assert_eq!(
            opportunity.market.verification_skip_reason,
            Some(VerificationSkipReason::PrimaryScreenNoProfitableCandidate)
        );
        assert_eq!(
            opportunity.market.independent_verification_status,
            OpportunityIndependentVerificationStatus::NotRequested
        );
        assert_eq!(
            opportunity.market.independent_verification_lifecycle,
            [OpportunityIndependentVerificationStatus::NotRequested]
        );
        assert_eq!(
            opportunity.simulation.classification,
            SimulationClassification::NotRun
        );
        assert!(opportunity.decision.shadow_only);
        assert!(!opportunity.decision.execution_eligible);
        assert!(!opportunity.decision.execution_request_created);
        let rendered = metrics.render(&crate::runtime_state::RuntimeReadiness::new());
        assert!(rendered.contains("rpc_primary_screen_rejected_total 1"));
        assert!(rendered.contains("rpc_secondary_skipped_total 1"));
    }

    #[tokio::test]
    async fn promising_primary_evidence_requests_secondary_exactly_once() {
        let now = unix_time_ms();
        let (evaluator, client, _) =
            evaluator_with_client(FakeMode::Profitable { agreement: true });
        let batch = evaluator
            .evaluate(&input(now), &origin(), &route())
            .await
            .unwrap();
        assert_eq!(batch.evaluations.len(), 1);
        assert_eq!(client.calls.load(Ordering::Relaxed), 2);
        let market = &batch.evaluations[0].opportunity.market;
        assert_eq!(
            market.verification_status,
            OpportunityVerificationStatus::Agreed
        );
        assert_eq!(market.agreement_state, AgreementState::Agreed);
        assert_eq!(market.secondary_state_hash, market.primary_state_hash);
        assert_eq!(market.secondary_block_number, Some(market.state_block));
        assert_eq!(market.secondary_block_hash, market.state_block_hash);
        assert_eq!(market.secondary_route_config_hash, market.route_config_hash);
        assert_eq!(
            market.independent_verification_lifecycle,
            [
                OpportunityIndependentVerificationStatus::Requested,
                OpportunityIndependentVerificationStatus::Agreed,
            ]
        );
    }

    #[tokio::test]
    async fn secondary_transport_unavailability_is_persistable_fail_closed_evidence() {
        let now = unix_time_ms();
        let (evaluator, client, _) = evaluator_with_client(FakeMode::SecondaryRetryable);
        let batch = evaluator
            .evaluate(&input(now), &origin(), &route())
            .await
            .unwrap();
        assert_eq!(client.calls.load(Ordering::Relaxed), 2);
        assert_eq!(batch.evaluations.len(), 1);
        let opportunity = &batch.evaluations[0].opportunity;
        assert_eq!(
            opportunity.simulation.classification,
            SimulationClassification::ProviderDisagreement
        );
        assert_eq!(
            opportunity.decision.primary_rejection_reason,
            Some(RejectionReason::RpcStateDisagreement)
        );
        assert_eq!(
            opportunity.market.verification_status,
            OpportunityVerificationStatus::SecondaryUnavailable
        );
        assert_eq!(
            opportunity.market.agreement_state,
            AgreementState::Unavailable
        );
        assert!(opportunity.market.secondary_state_hash.is_none());
        assert_eq!(
            opportunity.market.independent_verification_status,
            OpportunityIndependentVerificationStatus::ProviderUnavailable
        );
        assert_eq!(
            opportunity.market.independent_verification_lifecycle,
            [
                OpportunityIndependentVerificationStatus::Requested,
                OpportunityIndependentVerificationStatus::ProviderUnavailable,
            ]
        );
    }

    #[tokio::test]
    async fn secondary_integrity_failure_is_persistable_fail_closed_evidence() {
        let now = unix_time_ms();
        let (evaluator, client, _) = evaluator_with_client(FakeMode::SecondaryIntegrity);
        let batch = evaluator
            .evaluate(&input(now), &origin(), &route())
            .await
            .unwrap();
        assert_eq!(client.calls.load(Ordering::Relaxed), 2);
        let opportunity = &batch.evaluations[0].opportunity;
        assert_eq!(
            opportunity.market.independent_verification_status,
            OpportunityIndependentVerificationStatus::IntegrityFailure
        );
        assert_eq!(
            opportunity.decision.primary_rejection_reason,
            Some(RejectionReason::RpcStateDisagreement)
        );
        assert!(!opportunity.decision.execution_request_created);
        assert_eq!(
            opportunity.market.independent_verification_lifecycle,
            [
                OpportunityIndependentVerificationStatus::Requested,
                OpportunityIndependentVerificationStatus::IntegrityFailure,
            ]
        );
    }

    #[tokio::test]
    async fn pinned_state_generates_real_economics_but_state_only_evidence_rejects() {
        let now = unix_time_ms();
        let batch = evaluator(FakeMode::Profitable { agreement: true })
            .evaluate(&input(now), &origin(), &route())
            .await
            .unwrap();
        assert_eq!(batch.evaluations.len(), 1);
        let evaluation = &batch.evaluations[0];
        assert!(
            evaluation.opportunity.route.expected_output
                > evaluation.opportunity.route.input_amount
        );
        assert_eq!(
            evaluation.opportunity.simulation.kind,
            SimulationKind::StateBased
        );
        assert_eq!(
            evaluation.opportunity.decision.primary_rejection_reason,
            Some(RejectionReason::ContractPathUnavailable)
        );
        assert!(!evaluation
            .opportunity
            .decision
            .secondary_rejection_reasons
            .contains(&RejectionReason::LiquidityInsufficient));
        assert!(!evaluation.opportunity.decision.execution_eligible);
        assert_eq!(evaluation.rpc_quality.len(), 2);
        evaluation.opportunity.validate_traceability().unwrap();
    }

    #[tokio::test]
    async fn provider_disagreement_is_persistable_rejection_evidence() {
        let now = unix_time_ms();
        let batch = evaluator(FakeMode::Profitable { agreement: false })
            .evaluate(&input(now), &origin(), &route())
            .await
            .unwrap();
        let opportunity = &batch.evaluations[0].opportunity;
        assert_eq!(
            opportunity.simulation.classification,
            SimulationClassification::ProviderDisagreement
        );
        assert_eq!(
            opportunity.decision.primary_rejection_reason,
            Some(RejectionReason::RpcStateDisagreement)
        );
        assert_eq!(
            opportunity.decision.disposition,
            ShadowDisposition::Rejected
        );
        assert_eq!(
            opportunity.market.verification_status,
            OpportunityVerificationStatus::Disagreed
        );
        assert_eq!(
            opportunity.market.agreement_state,
            AgreementState::Disagreed
        );
        assert_ne!(
            opportunity.market.secondary_state_hash,
            opportunity.market.primary_state_hash
        );
    }

    #[tokio::test]
    async fn gateway_transport_failure_is_retryable_without_synthetic_fallback() {
        let now = unix_time_ms();
        let result = evaluator(FakeMode::Retryable)
            .evaluate(&input(now), &origin(), &route())
            .await;
        assert!(matches!(
            result,
            Err(EvaluationError::Transient("rpc_gateway_unavailable"))
        ));
    }

    #[tokio::test]
    async fn evaluated_decision_satisfies_atomic_store_integrity_contract() {
        let now_ms = unix_time_ms();
        let engine_input = input(now_ms);
        let batch = evaluator(FakeMode::Profitable { agreement: true })
            .evaluate(&engine_input, &origin(), &route())
            .await
            .unwrap();
        let now = Utc::now();
        let record = ClassificationRecord {
            identity: engine_input.identity,
            classification: crate::engine_input::EngineClassification::CandidateRejected,
            detail_class: Some("shadow_policy_rejected"),
            candidate_count: 1,
            decision_count: 1,
            delivery_attempt: 1,
            evidence: json!({"evaluation": "block_pinned_rpc_state"}),
            first_received_at: now,
            completed_at: now,
            processing_latency_ns: 1,
            evaluations: batch.evaluations,
        };
        assert_eq!(validate_record(&record), Ok(()));
    }
}
