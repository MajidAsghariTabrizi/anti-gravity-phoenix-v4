use crate::engine_input::{EngineClassification, EngineInput};
use crate::hunter::{
    CandidateBindings, CandidateSink, HunterBounds, HunterCore, HunterError, HunterEvent,
    HunterProcessResult, HunterRouteGraph, MaterializedCandidate,
};
use crate::origin::OriginEvent;
use crate::shadow_processor::{ProcessResult, ProcessingAction};
use async_trait::async_trait;
use chrono::{DateTime, Utc};
use rpc_gateway::hunter_state::{
    HunterPoolRequest, HunterStateRequest, HunterStateResponse, ProviderStateAgreement,
    HUNTER_STATE_REQUEST_SCHEMA,
};
use rpc_gateway::shadow_state::GatewayErrorResponse;
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use sqlx::postgres::PgPoolOptions;
use sqlx::types::Json;
use sqlx::PgPool;
use std::collections::{BTreeMap, BTreeSet};
use std::sync::Arc;
use std::time::Duration;
use thiserror::Error;
use tokio::sync::Mutex;
use uuid::Uuid;

const AUTONOMOUS_SCHEMA_VERSION: &str = "phoenix.live-canary-schema.v4";
const MAX_GATEWAY_RESPONSE_BYTES: usize = 2 * 1024 * 1024;
const ZERO_DIGEST: &str = "0000000000000000000000000000000000000000000000000000000000000000";

#[derive(Clone)]
pub struct RpcHunterStateClient {
    client: reqwest::Client,
    endpoint: reqwest::Url,
}

impl RpcHunterStateClient {
    pub fn new(base_url: &str) -> Result<Self, AutonomousError> {
        let mut endpoint =
            reqwest::Url::parse(base_url).map_err(|_| AutonomousError::Configuration)?;
        if !matches!(endpoint.scheme(), "http" | "https")
            || endpoint.host_str().is_none()
            || !endpoint.username().is_empty()
            || endpoint.password().is_some()
        {
            return Err(AutonomousError::Configuration);
        }
        endpoint.set_path("/v1/hunter/state");
        endpoint.set_query(None);
        endpoint.set_fragment(None);
        let client = reqwest::Client::builder()
            .connect_timeout(Duration::from_secs(2))
            .timeout(Duration::from_secs(30))
            .redirect(reqwest::redirect::Policy::none())
            .build()
            .map_err(|_| AutonomousError::Configuration)?;
        Ok(Self { client, endpoint })
    }
}

#[async_trait]
pub trait HunterStateProvider: Send + Sync {
    async fn state(
        &self,
        request: HunterStateRequest,
    ) -> Result<HunterStateResponse, AutonomousError>;
}

#[async_trait]
pub trait AutonomousEventProcessor: Send + Sync {
    async fn process(&self, input: &EngineInput, origin: &OriginEvent) -> ProcessResult;
}

#[async_trait]
impl HunterStateProvider for RpcHunterStateClient {
    async fn state(
        &self,
        request: HunterStateRequest,
    ) -> Result<HunterStateResponse, AutonomousError> {
        let response = self
            .client
            .post(self.endpoint.clone())
            .json(&request)
            .send()
            .await
            .map_err(|_| AutonomousError::Dependency)?;
        if !response.status().is_success() {
            let body = response
                .json::<GatewayErrorResponse>()
                .await
                .map_err(|_| AutonomousError::Dependency)?;
            return Err(match body.error_class.as_str() {
                "provider_disagreement" => AutonomousError::ProviderDisagreement,
                "state_incomplete" => AutonomousError::StateIncomplete,
                "provider_integrity_failure" => event_integrity(
                    "hunter_state_response_validation",
                    "provider_integrity_failure",
                    None,
                ),
                _ if body.retryable => AutonomousError::Dependency,
                _ => event_integrity(
                    "hunter_state_request",
                    "hunter_state_request_rejected",
                    None,
                ),
            });
        }
        if response
            .content_length()
            .is_some_and(|length| length > MAX_GATEWAY_RESPONSE_BYTES as u64)
        {
            return Err(event_integrity(
                "hunter_state_response_validation",
                "hunter_state_response_too_large",
                None,
            ));
        }
        response.json::<HunterStateResponse>().await.map_err(|_| {
            event_integrity(
                "hunter_state_response_validation",
                "hunter_state_response_invalid_json",
                None,
            )
        })
    }
}

#[derive(Clone)]
pub struct PostgresAutonomousCandidateStore {
    pool: PgPool,
}

impl PostgresAutonomousCandidateStore {
    pub async fn connect(dsn: &str) -> Result<Self, AutonomousError> {
        let pool = PgPoolOptions::new()
            .max_connections(4)
            .acquire_timeout(Duration::from_secs(5))
            .connect(dsn)
            .await
            .map_err(|error| classify_database(error, "autonomous_candidate_persistence"))?;
        let store = Self { pool };
        store.validate_schema().await?;
        Ok(store)
    }

    pub async fn validate_schema(&self) -> Result<(), AutonomousError> {
        let version: String = sqlx::query_scalar(
            "SELECT version FROM live_canary.schema_contract WHERE version = $1",
        )
        .bind(AUTONOMOUS_SCHEMA_VERSION)
        .fetch_one(&self.pool)
        .await
        .map_err(|error| classify_database(error, "autonomous_schema_validation"))?;
        if version != AUTONOMOUS_SCHEMA_VERSION {
            return Err(process_integrity(
                "autonomous_schema_validation",
                "autonomous_schema_incompatible",
                None,
            ));
        }
        Ok(())
    }

    pub async fn materialize(
        &self,
        artifact: &MaterializedCandidate,
        state_contract: &Value,
    ) -> Result<bool, AutonomousError> {
        validate_artifact(artifact)?;
        let contract = &artifact.contract;
        let candidate_id = parse_uuid(contract, "candidate_id")?;
        let opportunity_id = parse_uuid(contract, "opportunity_id")?;
        let candidate_hash = text(contract, "candidate_hash")?;
        let created_at = timestamp(contract, "candidate_created_at")?;
        let expires_at = timestamp(contract, "candidate_expires_at")?;
        let result = sqlx::query(
            "INSERT INTO live_canary.autonomous_candidates(
                candidate_id, opportunity_id, origin_event_id, schema_version, chain_id,
                route_fingerprint, route_universe_hash, route_policy_hash, risk_policy_hash,
                state_block_number, state_block_hash, state_hash, selected_size,
                predicted_gross_profit, predicted_total_cost, conservative_predicted_net_pnl,
                plan_hash, calldata_hash, executor_address, executor_code_hash,
                submission_channel, submission_quote_hash, risk_snapshot_hash,
                risk_snapshot_contract, submission_quote_contract, candidate_hash,
                candidate_contract, status, candidate_created_at, candidate_expires_at,
                plan_contract, calldata_hex, state_contract
             )
             VALUES (
                $1, $2, $3, $4, $5, $6, $7, $8, $9,
                $10::numeric, $11, $12, $13::numeric,
                $14::numeric, $15::numeric, $16::numeric,
                $17, $18, $19, $20, $21, NULL, NULL, NULL, NULL, $22,
                $23, 'materialized', $24, $25, $26, $27, $28
             )
             ON CONFLICT (candidate_hash) DO NOTHING",
        )
        .bind(candidate_id)
        .bind(opportunity_id)
        .bind(text(contract, "origin_event_id")?)
        .bind(text(contract, "schema_version")?)
        .bind(i64::try_from(unsigned(contract, "chain_id")?).map_err(|_| {
            event_integrity(
                "autonomous_candidate_validation",
                "candidate_chain_id_out_of_range",
                None,
            )
        })?)
        .bind(text(contract, "route_fingerprint")?)
        .bind(text(contract, "route_universe_hash")?)
        .bind(text(contract, "route_policy_hash")?)
        .bind(text(contract, "risk_policy_hash")?)
        .bind(unsigned_text(contract, "state_block_number")?)
        .bind(text(contract, "state_block_hash")?)
        .bind(text(contract, "state_hash")?)
        .bind(text(contract, "selected_size")?)
        .bind(text(contract, "predicted_gross_profit")?)
        .bind(text(contract, "predicted_total_cost")?)
        .bind(text(contract, "conservative_predicted_net_pnl")?)
        .bind(text(contract, "plan_hash")?)
        .bind(text(contract, "calldata_hash")?)
        .bind(text(contract, "executor_address")?)
        .bind(text(contract, "executor_code_hash")?)
        .bind(text(contract, "submission_channel")?)
        .bind(candidate_hash)
        .bind(Json(contract))
        .bind(created_at)
        .bind(expires_at)
        .bind(Json(&artifact.plan))
        .bind(format!("0x{}", hex::encode(&artifact.calldata)))
        .bind(Json(state_contract))
        .execute(&self.pool)
        .await
        .map_err(|error| classify_database(error, "autonomous_candidate_persistence"))?;
        if result.rows_affected() == 1 {
            return Ok(true);
        }
        let existing: Option<(Uuid, String)> = sqlx::query_as(
            "SELECT candidate_id, candidate_contract::text
             FROM live_canary.autonomous_candidates
             WHERE candidate_hash = $1",
        )
        .bind(candidate_hash)
        .fetch_optional(&self.pool)
        .await
        .map_err(|error| classify_database(error, "autonomous_candidate_persistence"))?;
        let Some((existing_id, existing_contract)) = existing else {
            return Err(process_integrity(
                "autonomous_candidate_persistence",
                "candidate_conflict_row_missing",
                None,
            ));
        };
        let expected = serde_json::to_value(contract).map_err(|_| {
            event_integrity(
                "autonomous_candidate_validation",
                "candidate_contract_serialization",
                None,
            )
        })?;
        let actual: Value = serde_json::from_str(&existing_contract).map_err(|_| {
            process_integrity(
                "autonomous_candidate_persistence",
                "persisted_candidate_contract_invalid",
                None,
            )
        })?;
        if existing_id != candidate_id || actual != expected {
            return Err(process_integrity(
                "autonomous_candidate_persistence",
                "candidate_hash_conflict",
                None,
            ));
        }
        Ok(false)
    }
}

pub struct AutonomousHunterProcessor {
    graph: HunterRouteGraph,
    core: Mutex<HunterCore>,
    bounds: HunterBounds,
    bindings: CandidateBindings,
    state_provider: Arc<dyn HunterStateProvider>,
    store: PostgresAutonomousCandidateStore,
}

impl AutonomousHunterProcessor {
    pub fn new(
        graph: HunterRouteGraph,
        core: HunterCore,
        bounds: HunterBounds,
        bindings: CandidateBindings,
        state_provider: Arc<dyn HunterStateProvider>,
        store: PostgresAutonomousCandidateStore,
    ) -> Self {
        Self {
            graph,
            core: Mutex::new(core),
            bounds,
            bindings,
            state_provider,
            store,
        }
    }

    async fn process_event(&self, input: &EngineInput, origin: &OriginEvent) -> ProcessResult {
        match self.process_inner(input, origin).await {
            Ok(result) => result,
            Err(AutonomousError::Dependency) => ProcessResult::transient(
                "autonomous_state_dependency_unavailable",
                0,
                json!({"dependency_failure_class": "hunter_state_unavailable"}),
            ),
            Err(AutonomousError::ProviderDisagreement) => rejected("provider_disagreement"),
            Err(AutonomousError::StateIncomplete) => rejected("state_incomplete"),
            Err(AutonomousError::Economic) => rejected("no_profitable_candidate"),
            Err(AutonomousError::EventIntegrity(context)) => ProcessResult::terminal(
                "autonomous_event_integrity_quarantined",
                0,
                context.evidence("event"),
            ),
            Err(AutonomousError::Configuration) => ProcessResult::process_fatal(
                "autonomous_configuration_integrity_failure",
                0,
                AutonomousIntegrityContext {
                    stage: "autonomous_configuration",
                    error_class: "autonomous_configuration_invalid",
                    hunter_error_code: None,
                }
                .evidence("process"),
            ),
            Err(AutonomousError::ProcessIntegrity(context)) => ProcessResult::process_fatal(
                "autonomous_integrity_failure",
                0,
                context.evidence("process"),
            ),
        }
    }

    async fn process_inner(
        &self,
        input: &EngineInput,
        origin: &OriginEvent,
    ) -> Result<ProcessResult, AutonomousError> {
        let touched = origin
            .candidate_touched_pools
            .iter()
            .map(|pool| pool.0.clone())
            .collect::<Vec<_>>();
        let routes = self
            .graph
            .affected_routes_for_pools(&touched, self.bounds.maximum_affected_routes_per_event)
            .map_err(map_hunter_error)?;
        if routes.is_empty() {
            return Ok(ProcessResult::no_route(
                "no_affected_hunter_route",
                json!({"origin_classification": "supported_swap_origin"}),
            ));
        }
        let mut pools = BTreeMap::new();
        for route in &routes {
            for leg in &route.legs {
                pools
                    .entry(leg.pool_address.clone())
                    .or_insert_with(|| HunterPoolRequest {
                        pool_id: leg.pool_id.clone(),
                        pool_address: leg.pool_address.clone(),
                        factory_address: leg.factory_address.clone(),
                        protocol_id: leg.protocol_id.clone(),
                        token0: if leg.token_in < leg.token_out {
                            leg.token_in.clone()
                        } else {
                            leg.token_out.clone()
                        },
                        token1: if leg.token_in < leg.token_out {
                            leg.token_out.clone()
                        } else {
                            leg.token_in.clone()
                        },
                        fee: leg.fee,
                        tick_spacing: leg.tick_spacing,
                    });
            }
        }
        let request = HunterStateRequest {
            schema_version: HUNTER_STATE_REQUEST_SCHEMA.to_string(),
            chain_id: input.identity.chain_id,
            request_id: input.identity.source_event_identity.clone(),
            pools: pools.into_values().collect(),
            maximum_tick_words_per_pool: self.bounds.maximum_tick_words_per_pool,
            maximum_initialized_ticks: self.bounds.maximum_initialized_ticks,
        };
        let response = self.state_provider.state(request.clone()).await?;
        response.validate(&request).map_err(|_| {
            event_integrity(
                "hunter_state_response_validation",
                "hunter_state_response_invalid",
                None,
            )
        })?;
        let states = response
            .agreements
            .iter()
            .map(|agreement| {
                Ok((
                    agreement
                        .agreed()
                        .map_err(|_| AutonomousError::ProviderDisagreement)?
                        .pool_address
                        .clone(),
                    agreement.clone(),
                ))
            })
            .collect::<Result<BTreeMap<String, ProviderStateAgreement>, AutonomousError>>()?;
        let evaluated_at = Utc::now().timestamp_millis();
        let event = HunterEvent {
            origin_event_id: input.identity.source_event_identity.clone(),
            origin_router: origin.router.0.clone(),
            chain_id: input.identity.chain_id,
            block_number: response.block_number,
            block_hash: response.block_hash.clone(),
            observed_at_unix_ms: input.observed_at_unix_ms,
            evaluated_at_unix_ms: u64::try_from(evaluated_at).map_err(|_| {
                event_integrity(
                    "hunter_event_validation",
                    "hunter_evaluation_clock_invalid",
                    None,
                )
            })?,
            touched_pool_addresses: touched,
        };
        let mut collector = ArtifactCollector::default();
        let result = self
            .core
            .lock()
            .await
            .process_event(&event, &states, &self.bindings, &mut collector)
            .map_err(map_hunter_error)?;
        if result.candidates.is_empty() {
            return Err(AutonomousError::Economic);
        }
        let state_contract = serde_json::to_value(&response).map_err(|_| {
            event_integrity(
                "hunter_state_response_validation",
                "hunter_state_response_serialization",
                None,
            )
        })?;
        for artifact in &collector.artifacts {
            validate_artifact(artifact)?;
        }
        let mut materialized = 0_usize;
        for artifact in &collector.artifacts {
            if self.store.materialize(artifact, &state_contract).await? {
                materialized += 1;
            }
        }
        Ok(candidate_result(result, materialized))
    }
}

#[async_trait]
impl AutonomousEventProcessor for AutonomousHunterProcessor {
    async fn process(&self, input: &EngineInput, origin: &OriginEvent) -> ProcessResult {
        self.process_event(input, origin).await
    }
}

#[derive(Default)]
struct ArtifactCollector {
    artifacts: Vec<MaterializedCandidate>,
    hashes: BTreeSet<String>,
}

impl CandidateSink for ArtifactCollector {
    fn materialize(&mut self, candidate: MaterializedCandidate) -> Result<bool, HunterError> {
        let hash = candidate
            .contract
            .get("candidate_hash")
            .and_then(Value::as_str)
            .ok_or(HunterError::CandidateIntegrity)?
            .to_string();
        if !self.hashes.insert(hash) {
            return Ok(false);
        }
        self.artifacts.push(candidate);
        Ok(true)
    }
}

fn candidate_result(result: HunterProcessResult, materialized: usize) -> ProcessResult {
    ProcessResult {
        classification: EngineClassification::CandidateGenerated,
        detail_class: if materialized > 0 {
            "autonomous_candidate_materialized"
        } else {
            "autonomous_candidate_duplicate"
        },
        candidate_count: result.affected_route_fingerprints.len(),
        decision_count: materialized,
        evidence: json!({
            "route_count": result.affected_route_fingerprints.len(),
            "candidate_count": result.candidates.len(),
            "materialized_count": materialized,
            "execution_mode": "LIVE"
        }),
        evaluations: Vec::new(),
        action: ProcessingAction::Ack,
        origin_metric: None,
    }
}

fn rejected(class: &'static str) -> ProcessResult {
    ProcessResult {
        classification: EngineClassification::CandidateRejected,
        detail_class: class,
        candidate_count: 0,
        decision_count: 0,
        evidence: json!({"rejection_class": class, "execution_mode": "LIVE"}),
        evaluations: Vec::new(),
        action: ProcessingAction::Ack,
        origin_metric: None,
    }
}

fn validate_artifact(artifact: &MaterializedCandidate) -> Result<(), AutonomousError> {
    let contract = &artifact.contract;
    if text(contract, "schema_version")? != "phoenix.autonomous-candidate.v1"
        || text(contract, "status")? != "materialized"
        || text(contract, "submission_quote_hash")? != ZERO_DIGEST
        || text(contract, "risk_snapshot_hash")? != ZERO_DIGEST
        || text(contract, "calldata_hash")? != hex::encode(Sha256::digest(&artifact.calldata))
        || artifact.plan.get("calldata_hash").and_then(Value::as_str)
            != contract.get("calldata_hash").and_then(Value::as_str)
        || artifact
            .plan
            .get("execution_eligible")
            .and_then(Value::as_bool)
            != Some(true)
        || artifact.plan.get("shadow_only").and_then(Value::as_bool) != Some(false)
    {
        return Err(event_integrity(
            "autonomous_candidate_validation",
            "autonomous_candidate_contract_invalid",
            None,
        ));
    }
    Ok(())
}

fn text<'a>(value: &'a Value, field: &str) -> Result<&'a str, AutonomousError> {
    value.get(field).and_then(Value::as_str).ok_or_else(|| {
        event_integrity(
            "autonomous_candidate_validation",
            "autonomous_candidate_field_invalid",
            None,
        )
    })
}

fn unsigned(value: &Value, field: &str) -> Result<u64, AutonomousError> {
    value.get(field).and_then(Value::as_u64).ok_or_else(|| {
        event_integrity(
            "autonomous_candidate_validation",
            "autonomous_candidate_field_invalid",
            None,
        )
    })
}

fn unsigned_text(value: &Value, field: &str) -> Result<String, AutonomousError> {
    unsigned(value, field).map(|value| value.to_string())
}

fn parse_uuid(value: &Value, field: &str) -> Result<Uuid, AutonomousError> {
    Uuid::parse_str(text(value, field)?).map_err(|_| {
        event_integrity(
            "autonomous_candidate_validation",
            "autonomous_candidate_uuid_invalid",
            None,
        )
    })
}

fn timestamp(value: &Value, field: &str) -> Result<DateTime<Utc>, AutonomousError> {
    DateTime::parse_from_rfc3339(text(value, field)?)
        .map(|value| value.with_timezone(&Utc))
        .map_err(|_| {
            event_integrity(
                "autonomous_candidate_validation",
                "autonomous_candidate_timestamp_invalid",
                None,
            )
        })
}

fn map_hunter_error(error: HunterError) -> AutonomousError {
    let error_code = error.code();
    match error {
        HunterError::InvalidBounds => process_integrity(
            "hunter_route_selection",
            "hunter_invalid_bounds",
            Some(error_code),
        ),
        HunterError::InvalidUniverse => process_integrity(
            "hunter_route_selection",
            "hunter_invalid_universe",
            Some(error_code),
        ),
        HunterError::InvalidPolicy => process_integrity(
            "hunter_route_selection",
            "hunter_invalid_policy",
            Some(error_code),
        ),
        HunterError::PolicyRouteMismatch => process_integrity(
            "hunter_route_selection",
            "hunter_policy_route_mismatch",
            Some(error_code),
        ),
        HunterError::InvalidRoute => process_integrity(
            "hunter_route_selection",
            "hunter_invalid_route",
            Some(error_code),
        ),
        HunterError::DuplicateRoute => process_integrity(
            "hunter_route_selection",
            "hunter_duplicate_route",
            Some(error_code),
        ),
        HunterError::RouteLimit => process_integrity(
            "hunter_route_selection",
            "hunter_route_limit",
            Some(error_code),
        ),
        HunterError::AffectedRouteLimit => event_integrity(
            "hunter_route_selection",
            "hunter_affected_route_limit",
            Some(error_code),
        ),
        HunterError::InvalidEvent => event_integrity(
            "hunter_event_validation",
            "hunter_invalid_event",
            Some(error_code),
        ),
        HunterError::StateIncomplete => AutonomousError::StateIncomplete,
        HunterError::StateIntegrity => AutonomousError::ProviderDisagreement,
        HunterError::EconomicInfeasible => AutonomousError::Economic,
        HunterError::Arithmetic => event_integrity(
            "hunter_route_simulation",
            "hunter_arithmetic",
            Some(error_code),
        ),
        HunterError::CandidateIntegrity => event_integrity(
            "hunter_candidate_materialization",
            "hunter_candidate_integrity",
            Some(error_code),
        ),
        HunterError::PlanIntegrity => event_integrity(
            "hunter_candidate_materialization",
            "hunter_plan_integrity",
            Some(error_code),
        ),
    }
}

fn classify_database(error: sqlx::Error, stage: &'static str) -> AutonomousError {
    match error {
        sqlx::Error::Io(_)
        | sqlx::Error::Tls(_)
        | sqlx::Error::PoolTimedOut
        | sqlx::Error::PoolClosed
        | sqlx::Error::WorkerCrashed => AutonomousError::Dependency,
        _ => process_integrity(stage, "autonomous_database_integrity", None),
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct AutonomousIntegrityContext {
    pub stage: &'static str,
    pub error_class: &'static str,
    pub hunter_error_code: Option<&'static str>,
}

impl AutonomousIntegrityContext {
    fn evidence(self, failure_scope: &'static str) -> Value {
        json!({
            "stage": self.stage,
            "error_class": self.error_class,
            "hunter_error_code": self.hunter_error_code,
            "failure_scope": failure_scope,
            "execution_mode": "LIVE",
            "execution_eligible": false,
            "execution_request_created": false
        })
    }
}

fn event_integrity(
    stage: &'static str,
    error_class: &'static str,
    hunter_error_code: Option<&'static str>,
) -> AutonomousError {
    AutonomousError::EventIntegrity(AutonomousIntegrityContext {
        stage,
        error_class,
        hunter_error_code,
    })
}

fn process_integrity(
    stage: &'static str,
    error_class: &'static str,
    hunter_error_code: Option<&'static str>,
) -> AutonomousError {
    AutonomousError::ProcessIntegrity(AutonomousIntegrityContext {
        stage,
        error_class,
        hunter_error_code,
    })
}

#[derive(Clone, Copy, Debug, Error, PartialEq, Eq)]
pub enum AutonomousError {
    #[error("autonomous Hunter configuration is invalid")]
    Configuration,
    #[error("autonomous Hunter dependency is unavailable")]
    Dependency,
    #[error("autonomous Hunter providers disagree")]
    ProviderDisagreement,
    #[error("autonomous Hunter state is incomplete")]
    StateIncomplete,
    #[error("autonomous Hunter economics rejected the route")]
    Economic,
    #[error("autonomous Hunter event integrity check failed")]
    EventIntegrity(AutonomousIntegrityContext),
    #[error("autonomous Hunter process integrity check failed")]
    ProcessIntegrity(AutonomousIntegrityContext),
}

#[cfg(test)]
mod tests {
    use super::*;

    fn event_context(error: HunterError) -> AutonomousIntegrityContext {
        match map_hunter_error(error) {
            AutonomousError::EventIntegrity(context) => context,
            other => panic!("expected event integrity, got {other:?}"),
        }
    }

    #[test]
    fn every_hunter_error_has_an_explicit_safe_classification() {
        for error in [
            HunterError::InvalidBounds,
            HunterError::InvalidUniverse,
            HunterError::InvalidPolicy,
            HunterError::PolicyRouteMismatch,
            HunterError::InvalidRoute,
            HunterError::DuplicateRoute,
            HunterError::RouteLimit,
            HunterError::AffectedRouteLimit,
            HunterError::InvalidEvent,
            HunterError::StateIncomplete,
            HunterError::StateIntegrity,
            HunterError::EconomicInfeasible,
            HunterError::Arithmetic,
            HunterError::CandidateIntegrity,
            HunterError::PlanIntegrity,
        ] {
            let code = error.code();
            let mapped = map_hunter_error(error);
            match mapped {
                AutonomousError::EventIntegrity(context)
                | AutonomousError::ProcessIntegrity(context) => {
                    assert_eq!(context.hunter_error_code, Some(code));
                }
                AutonomousError::StateIncomplete
                | AutonomousError::ProviderDisagreement
                | AutonomousError::Economic => {}
                other => panic!("unexpected Hunter classification: {other:?}"),
            }
        }
    }

    #[test]
    fn event_integrity_stages_and_hunter_codes_are_preserved() {
        for (error, stage, code) in [
            (
                HunterError::AffectedRouteLimit,
                "hunter_route_selection",
                "hunter_affected_route_limit",
            ),
            (
                HunterError::InvalidEvent,
                "hunter_event_validation",
                "hunter_invalid_event",
            ),
            (
                HunterError::Arithmetic,
                "hunter_route_simulation",
                "hunter_arithmetic",
            ),
            (
                HunterError::CandidateIntegrity,
                "hunter_candidate_materialization",
                "hunter_candidate_integrity",
            ),
            (
                HunterError::PlanIntegrity,
                "hunter_candidate_materialization",
                "hunter_plan_integrity",
            ),
        ] {
            let context = event_context(error);
            assert_eq!(context.stage, stage);
            assert_eq!(context.error_class, code);
            assert_eq!(context.hunter_error_code, Some(code));
            let result = ProcessResult::terminal(
                "autonomous_event_integrity_quarantined",
                99,
                context.evidence("event"),
            );
            assert_eq!(result.candidate_count, 0);
            assert_eq!(result.decision_count, 0);
            assert!(result.evaluations.is_empty());
            assert_eq!(result.action, ProcessingAction::Quarantine);
        }
    }

    #[test]
    fn invalid_state_response_and_candidate_validation_use_exact_bounded_evidence() {
        let state = event_integrity(
            "hunter_state_response_validation",
            "hunter_state_response_invalid",
            None,
        );
        let candidate = event_integrity(
            "autonomous_candidate_validation",
            "autonomous_candidate_contract_invalid",
            None,
        );
        for error in [state, candidate] {
            let AutonomousError::EventIntegrity(context) = error else {
                panic!("expected event integrity");
            };
            let evidence = context.evidence("event");
            assert_eq!(evidence["failure_scope"], "event");
            assert_eq!(evidence["execution_mode"], "LIVE");
            assert_eq!(evidence["execution_eligible"], false);
            assert_eq!(evidence["execution_request_created"], false);
            let rendered = evidence.to_string();
            assert!(rendered.len() < 1024);
            assert!(!rendered.contains("http://"));
            assert!(!rendered.contains("private_key"));
        }
    }

    #[test]
    fn autonomous_database_integrity_is_process_scoped() {
        assert_eq!(
            classify_database(sqlx::Error::RowNotFound, "autonomous_candidate_persistence"),
            process_integrity(
                "autonomous_candidate_persistence",
                "autonomous_database_integrity",
                None,
            )
        );
    }
}
