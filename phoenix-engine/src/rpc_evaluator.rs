use crate::amm::v3::simulate_exact_input;
use crate::decision::{decide, DecisionContext, ShadowPolicy};
use crate::domain::{Amount, Direction, DomainError, Liquidity, OpportunityId, SqrtPriceX96, Tick};
use crate::economics::{evaluate_scenarios, EconomicInput};
use crate::engine_input::EngineInput;
use crate::opportunity::{
    BasisPoints, DecisionEvidence, MarketEvidence, Opportunity, OpportunityIdentity,
    OutcomeEvidence, PoolStateEvidence, RejectionReason, RouteEvidence, ScenarioEconomics,
    ShadowDisposition, SignedAmount, SimulationClassification, SimulationEvidence, SimulationKind,
    StateSource, Strategy,
};
use crate::optimizer::{optimize, CandidateEvaluation, OptimizerConfig};
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
    canonical_block_hash, canonical_data, canonical_hash_bytes, GatewayErrorResponse,
    PoolStateRequest, RpcQualityEvidence, ShadowStateRequest, ShadowStateResponse,
    ARBITRUM_ONE_CHAIN_ID, MAX_GATEWAY_REQUEST_BYTES, MAX_GATEWAY_RESPONSE_BYTES,
    SHADOW_STATE_SCHEMA_VERSION,
};
use serde_json::json;
use sha2::{Digest, Sha256};
use std::collections::BTreeSet;
use std::fmt;
use std::sync::Arc;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};
use thiserror::Error;

const DEFAULT_GATEWAY_TIMEOUT: Duration = Duration::from_secs(5);
const MAX_PROVIDER_QUALITY_RECORDS: usize = 512;
const MAX_CLOCK_SKEW_MS: u64 = 30_000;
const STATE_MODEL_CONFIDENCE_BPS: u16 = 7_000;
const STRATEGY_VERSION: &str = "two-pool-v3-block-state-v1";
const DETECTOR_VERSION: &str = "exact-input-single-v1";
const POLICY_VERSION: &str = "shadow-state-policy-v1";

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
        if !bounded_label(&code_version, 1, 128) {
            return Err("invalid Engine code version");
        }
        Ok(Self {
            client,
            code_version,
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
        let request = state_request(route)?;
        let requested_at = Instant::now();
        let response = self
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
        validate_response(&request, &response, now_ms)?;
        let pools = decode_pools(route, &response)?;
        let gas_price_wei = parse_decimal_u128(&input.normalized.max_fee_per_gas)
            .ok_or(EvaluationError::Terminal("economic_input_out_of_range"))?;
        let mut incomplete_state_seen = false;
        let optimized = optimize(
            OptimizerConfig {
                min_amount: route.strategy.min_input_amount,
                max_amount: route.strategy.max_input_amount,
                max_evaluations: route.strategy.max_evaluations,
                min_profit: route.strategy.minimum_net_profit,
            },
            |amount| match evaluate_amount(route, &pools, amount, gas_price_wei) {
                Ok(value) => Ok(CandidateEvaluation {
                    amount,
                    gross_profit: value.gross_profit,
                    flash_premium: value.economics.base.flash_loan_fee,
                    expected_execution_cost: value
                        .economics
                        .base
                        .arbitrum_execution_fee
                        .checked_add(value.economics.base.l1_data_fee)?
                        .checked_add(value.economics.base.contract_overhead)?,
                    expected_ordering_cost: value.economics.base.failure_cost_reserve,
                    uncertainty_reserve: value.economics.base.uncertainty_reserve,
                    expected_net_profit: Amount(
                        u128::try_from(value.economics.base.expected_net_pnl.0)
                            .map_err(|_| DomainError::ArithmeticUnderflow)?,
                    ),
                }),
                Err(ModelError::NotViable) => Err(DomainError::ArithmeticUnderflow),
                Err(ModelError::StateIncomplete) => {
                    incomplete_state_seen = true;
                    Err(DomainError::ArithmeticUnderflow)
                }
                Err(ModelError::Integrity) => Err(DomainError::ArithmeticOverflow),
            },
        )
        .map_err(|_| EvaluationError::Terminal("shadow_model_arithmetic_failure"))?;

        let response_hash =
            canonical_hash_bytes(&serde_json::to_vec(&response).map_err(|_| {
                EvaluationError::Terminal("rpc_gateway_response_integrity_failure")
            })?);
        let base_evidence = json!({
            "state_block": response.block_number,
            "state_block_hash": response.block_hash,
            "rpc_response_hash": response_hash,
            "primary_provider_id": response.primary_provider_id,
            "agreement_provider_id": response.agreement_provider_id,
            "provider_agreement": response.provider_agreement,
            "rpc_quality_record_count": response.quality.len(),
            "state_model_tick_crossings_supported": 0,
            "incomplete_state_seen": incomplete_state_seen
        });
        let Some(optimized) = optimized else {
            return Ok(CandidateBatch {
                evaluations: Vec::new(),
                evidence: base_evidence,
            });
        };
        let selected = evaluate_amount(route, &pools, optimized.best_amount, gas_price_wei)
            .map_err(|_| EvaluationError::Terminal("shadow_model_recalculation_failure"))?;
        let opportunity = build_opportunity(
            input,
            origin,
            route,
            &response,
            &pools,
            response_hash,
            selected,
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
                "optimizer_evaluated_amount_count": optimized.evaluated_amount_count,
                "selected_input_amount": optimized.best_amount.0.to_string()
            }),
        })
    }
}

fn state_request(route: &RuntimeRoute) -> Result<ShadowStateRequest, EvaluationError> {
    if route.state_targets.len() != route.route.legs.len() {
        return Err(EvaluationError::Terminal("route_state_target_mismatch"));
    }
    let pools = route
        .route
        .legs
        .iter()
        .zip(&route.state_targets)
        .map(|(leg, target)| {
            let (token0, token1) = match leg.direction {
                Direction::ZeroForOne => (&leg.token_in, &leg.token_out),
                Direction::OneForZero => (&leg.token_out, &leg.token_in),
            };
            PoolStateRequest {
                pool_id: leg.pool_id.0.clone(),
                address: target.as_str().to_string(),
                protocol: leg.protocol.clone(),
                token0: token0.0.as_str().to_string(),
                token1: token1.0.as_str().to_string(),
                fee: leg.fee,
            }
        })
        .collect();
    let request = ShadowStateRequest {
        schema_version: SHADOW_STATE_SCHEMA_VERSION.to_string(),
        chain_id: ARBITRUM_ONE_CHAIN_ID,
        route_fingerprint: route.fingerprint.clone(),
        pools,
    };
    request
        .validate()
        .map_err(|_| EvaluationError::Terminal("route_state_request_invalid"))?;
    Ok(request)
}

fn validate_response(
    request: &ShadowStateRequest,
    response: &ShadowStateResponse,
    now_ms: u64,
) -> Result<(), EvaluationError> {
    let request_hash = request
        .canonical_hash()
        .map_err(|_| EvaluationError::Terminal("route_state_request_invalid"))?;
    if response.schema_version != SHADOW_STATE_SCHEMA_VERSION
        || response.chain_id != ARBITRUM_ONE_CHAIN_ID
        || response.request_hash != request_hash
        || response.block_number == 0
        || !canonical_block_hash(&response.block_hash)
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
    for (expected, actual) in request.pools.iter().zip(&response.pools) {
        let state_material = serde_json::to_vec(&(
            &actual.pool_id,
            &actual.address,
            &actual.protocol,
            &actual.token0,
            &actual.token1,
            actual.fee,
            &actual.slot0,
            &actual.liquidity,
        ))
        .map_err(|_| EvaluationError::Terminal("pool_state_identity_mismatch"))?;
        if actual.pool_id != expected.pool_id
            || actual.address != expected.address
            || actual.protocol != expected.protocol
            || actual.token0 != expected.token0
            || actual.token1 != expected.token1
            || actual.fee != expected.fee
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
    if response.provider_agreement {
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
    route
        .route
        .legs
        .iter()
        .zip(&response.pools)
        .map(|(leg, state)| {
            let (sqrt_price_x96, tick) = decode_slot0(&state.slot0)?;
            let liquidity = decode_liquidity(&state.liquidity)?;
            let (token0, token1) = match leg.direction {
                Direction::ZeroForOne => (leg.token_in.clone(), leg.token_out.clone()),
                Direction::OneForZero => (leg.token_out.clone(), leg.token_in.clone()),
            };
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
    gross_profit: Amount,
    economics: ScenarioEconomics,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum ModelError {
    NotViable,
    StateIncomplete,
    Integrity,
}

fn evaluate_amount(
    route: &RuntimeRoute,
    pools: &[PoolState],
    amount: Amount,
    gas_price_wei: u128,
) -> Result<AmountEvaluation, ModelError> {
    let output = simulate_route(route, pools, amount, false)?;
    let no_fee_output = simulate_route(route, pools, amount, true)?;
    let pool_fees = no_fee_output
        .checked_sub(output)
        .map_err(|_| ModelError::Integrity)?;
    let gross_profit = no_fee_output
        .checked_sub(amount)
        .map_err(|_| ModelError::NotViable)?;
    let economics = evaluate_scenarios(&EconomicInput {
        principal: amount,
        gross_output: no_fee_output,
        protocol_fees: route.strategy.protocol_fees,
        pool_fees,
        price_impact: Amount::ZERO,
        minimum_slippage_buffer: bps_amount(no_fee_output, route.strategy.minimum_slippage_bps)?,
        flash_loan_fee: bps_amount(amount, route.strategy.flash_premium_bps)?,
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
    })
    .map_err(|_| ModelError::Integrity)?;
    if economics.base.expected_net_pnl <= SignedAmount(0) {
        return Err(ModelError::NotViable);
    }
    Ok(AmountEvaluation {
        input: amount,
        output,
        gross_profit,
        economics,
    })
}

fn simulate_route(
    route: &RuntimeRoute,
    pools: &[PoolState],
    amount: Amount,
    remove_fees: bool,
) -> Result<Amount, ModelError> {
    if pools.len() != route.route.legs.len() {
        return Err(ModelError::Integrity);
    }
    route
        .route
        .legs
        .iter()
        .zip(pools)
        .try_fold(amount, |current, (leg, pool)| {
            let mut pool = pool.clone();
            if remove_fees {
                pool.fee = 0;
            }
            simulate_exact_input(&pool, current, leg.direction, 0)
                .map(|simulation| simulation.amount_out)
                .map_err(|error| match error {
                    DomainError::StateIncomplete => ModelError::StateIncomplete,
                    DomainError::ArithmeticUnderflow => ModelError::NotViable,
                    _ => ModelError::Integrity,
                })
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
            protocols: route
                .route
                .legs
                .iter()
                .map(|leg| leg.protocol.clone())
                .collect(),
            input_token: route.route.legs[0].token_in.clone(),
            output_token: route.route.legs[1].token_out.clone(),
            input_amount: selected.input,
            expected_output: selected.output,
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
            quote_block: response.block_number,
            quote_age_ms: now_ms.saturating_sub(response.resolved_at_unix_ms),
            state_source: StateSource::BlockPinnedRpc,
            rpc_provider_id: Some(response.primary_provider_id.clone()),
            rpc_response_hash: Some(response_hash),
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
            } else if response.provider_agreement {
                SimulationClassification::Passed
            } else {
                SimulationClassification::ProviderDisagreement
            },
        },
        decision: DecisionEvidence {
            disposition: ShadowDisposition::Rejected,
            primary_rejection_reason: Some(RejectionReason::SimulationEvidenceInsufficient),
            secondary_rejection_reasons: Vec::new(),
            risk_flags: Vec::new(),
            confidence_bps: 0,
            policy_version: POLICY_VERSION.to_string(),
            execution_eligible: false,
            decided_at_unix_ms: now_ms,
        },
        outcome: OutcomeEvidence {
            opportunity_expires_at_unix_ms: response
                .resolved_at_unix_ms
                .saturating_add(route.strategy.max_quote_age_ms),
            ..OutcomeEvidence::default()
        },
    };
    let minimum = SignedAmount(route.strategy.minimum_net_profit.0 as i128);
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
            liquidity_sufficient: false,
            rpc_state_agrees: response.provider_agreement,
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
    use crate::persistence::{validate_record, ClassificationRecord};
    use crate::shadow_processor::RuntimeStrategy;
    use chrono::Utc;
    use rpc_gateway::shadow_state::PoolStateResponse;
    use std::sync::Mutex;

    const BLOCK_HASH: &str = "0xaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";
    const TOKEN0: &str = "0x1111111111111111111111111111111111111111";
    const TOKEN1: &str = "0x2222222222222222222222222222222222222222";

    #[derive(Clone, Copy, Debug)]
    enum FakeMode {
        Profitable { agreement: bool },
        Retryable,
    }

    #[derive(Debug)]
    struct FakeClient {
        mode: Mutex<FakeMode>,
    }

    #[async_trait]
    impl ShadowStateClient for FakeClient {
        async fn fetch(
            &self,
            request: &ShadowStateRequest,
        ) -> Result<ShadowStateResponse, GatewayClientError> {
            match *self.mode.lock().unwrap() {
                FakeMode::Retryable => Err(GatewayClientError::Retryable),
                FakeMode::Profitable { agreement } => Ok(response(request, agreement)),
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
            state_targets: vec![
                Address::parse("0x3333333333333333333333333333333333333333").unwrap(),
                Address::parse("0x4444444444444444444444444444444444444444").unwrap(),
            ],
            strategy: RuntimeStrategy {
                min_input_amount: Amount(100),
                max_input_amount: Amount(1_000),
                max_evaluations: 16,
                minimum_net_profit: Amount(1),
                flash_premium_bps: 0,
                minimum_slippage_bps: 0,
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
        }
    }

    fn response(request: &ShadowStateRequest, agreement: bool) -> ShadowStateResponse {
        let first_sqrt = U256::from(1_u8) << 96;
        let second_sqrt = U256::from(1_u8) << 95;
        let pools = request
            .pools
            .iter()
            .enumerate()
            .map(|(index, pool)| {
                let slot0 = format!(
                    "0x{:064x}{:064x}",
                    if index == 0 { first_sqrt } else { second_sqrt },
                    0
                );
                let liquidity = format!("0x{:064x}", 1_000_000_000_u128);
                let state_material = serde_json::to_vec(&(
                    &pool.pool_id,
                    &pool.address,
                    &pool.protocol,
                    &pool.token0,
                    &pool.token1,
                    pool.fee,
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
                    fee: pool.fee,
                    slot0,
                    liquidity,
                    state_hash: canonical_hash_bytes(&state_material),
                }
            })
            .collect();
        let quality = ["provider_0", "provider_1"]
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
                disagreement: !agreement,
                timeout: false,
                retry_count: 0,
            })
            .collect();
        ShadowStateResponse {
            schema_version: SHADOW_STATE_SCHEMA_VERSION.to_string(),
            chain_id: ARBITRUM_ONE_CHAIN_ID,
            request_hash: request.canonical_hash().unwrap(),
            block_number: 100,
            block_hash: BLOCK_HASH.to_string(),
            pools,
            primary_provider_id: "provider_0".to_string(),
            agreement_provider_id: Some("provider_1".to_string()),
            provider_agreement: agreement,
            quality,
            resolved_at_unix_ms: unix_time_ms(),
        }
    }

    fn evaluator(mode: FakeMode) -> RpcCandidateEvaluator {
        RpcCandidateEvaluator::new(
            Arc::new(FakeClient {
                mode: Mutex::new(mode),
            }),
            "test-code".to_string(),
        )
        .unwrap()
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
    fn deterministic_identity_is_uuid_shaped_and_input_sensitive() {
        let first = deterministic_opportunity_id("source", "route", "block", Amount(1), Amount(2));
        let second = deterministic_opportunity_id("source", "route", "block", Amount(2), Amount(2));
        assert_eq!(first.len(), 36);
        assert_ne!(first, second);
        assert_eq!(&first[14..15], "8");
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
            Some(RejectionReason::LiquidityInsufficient)
        );
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
