use crate::engine_input::{decode_engine_input, EngineClassification, InputFailureKind};
use crate::engine_jetstream::{Delivery, MessageFetcher, PipelineError, RETRY_DELAY};
use crate::metrics::RuntimeMetrics;
use crate::opportunity::ShadowDisposition;
use crate::persistence::{
    ClassificationRecord, DependencyFailureContext, PersistOutcome, ShadowStore, StoreError,
    MAX_EVIDENCE_BYTES,
};
use crate::runtime_state::RuntimeReadiness;
use crate::shadow_processor::{ProcessResult, ProcessingAction, ShadowProcessor};
use chrono::{SecondsFormat, Utc};
use phoenix_recorder::engine_stream::ENGINE_MAX_DELIVERIES;
use phoenix_recorder::logging::LogSampler;
use serde_json::json;
use sha2::{Digest, Sha256};
use std::sync::Arc;
use std::time::{Duration, Instant};
use tokio_util::sync::CancellationToken;

const CONSUMER_STATE_REFRESH_INTERVAL: Duration = Duration::from_secs(1);
const MAX_CONFIGURED_DEPENDENCY_DELIVERIES: i64 = 100;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct DependencyRetryPolicy {
    max_deliveries: u64,
}

impl DependencyRetryPolicy {
    pub fn bounded(max_deliveries: i64) -> Result<Self, &'static str> {
        if !(2..=MAX_CONFIGURED_DEPENDENCY_DELIVERIES).contains(&max_deliveries) {
            return Err("dependency retry delivery limit must be bounded between 2 and 100");
        }
        Ok(Self {
            max_deliveries: max_deliveries as u64,
        })
    }

    pub fn engine_default() -> Result<Self, &'static str> {
        Self::bounded(ENGINE_MAX_DELIVERIES as i64)
    }

    pub const fn max_deliveries(self) -> u64 {
        self.max_deliveries
    }

    const fn exhausted(self, delivery_attempt: u64) -> bool {
        delivery_attempt >= self.max_deliveries
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RuntimeExit {
    Shutdown,
    FetchFailed,
    StoreFailed,
    AcknowledgementFailed,
    IntegrityFailure,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DeliveryDisposition {
    Continue,
    StoreFailed,
    AcknowledgementFailed,
    IntegrityFailure,
}

pub async fn consume_engine_messages(
    fetcher: Arc<dyn MessageFetcher>,
    store: Arc<dyn ShadowStore>,
    processor: Arc<ShadowProcessor>,
    readiness: RuntimeReadiness,
    metrics: RuntimeMetrics,
    sampler: LogSampler,
    retry_policy: DependencyRetryPolicy,
    shutdown: CancellationToken,
) -> RuntimeExit {
    let mut last_state_refresh = Instant::now()
        .checked_sub(CONSUMER_STATE_REFRESH_INTERVAL)
        .unwrap_or_else(Instant::now);
    loop {
        let deliveries = tokio::select! {
            _ = shutdown.cancelled() => return RuntimeExit::Shutdown,
            result = fetcher.fetch_batch() => result,
        };
        let deliveries = match deliveries {
            Ok(deliveries) => {
                readiness.set_fetching_active(true);
                deliveries
            }
            Err(error) => {
                readiness.set_fetching_active(false);
                readiness.set_consumer_ready(false);
                sampled_pipeline_failure(&sampler, "engine_fetch_failure", error);
                return RuntimeExit::FetchFailed;
            }
        };

        if last_state_refresh.elapsed() >= CONSUMER_STATE_REFRESH_INTERVAL {
            match fetcher.state().await {
                Ok(state) => {
                    metrics.set_consumer_state(state.pending, state.ack_pending);
                    readiness.set_consumer_ready(true);
                }
                Err(error) => {
                    readiness.set_fetching_active(false);
                    readiness.set_consumer_ready(false);
                    sampled_pipeline_failure(&sampler, "engine_consumer_state_failure", error);
                    return RuntimeExit::FetchFailed;
                }
            }
            last_state_refresh = Instant::now();
        }

        for delivery in deliveries {
            if shutdown.is_cancelled() {
                return RuntimeExit::Shutdown;
            }
            match process_delivery(
                delivery,
                store.as_ref(),
                processor.as_ref(),
                &readiness,
                &metrics,
                &sampler,
                retry_policy,
            )
            .await
            {
                DeliveryDisposition::Continue => {}
                DeliveryDisposition::StoreFailed => return RuntimeExit::StoreFailed,
                DeliveryDisposition::AcknowledgementFailed => {
                    return RuntimeExit::AcknowledgementFailed;
                }
                DeliveryDisposition::IntegrityFailure => return RuntimeExit::IntegrityFailure,
            }
        }
    }
}

pub async fn process_delivery(
    delivery: Delivery,
    store: &dyn ShadowStore,
    processor: &ShadowProcessor,
    readiness: &RuntimeReadiness,
    metrics: &RuntimeMetrics,
    sampler: &LogSampler,
    retry_policy: DependencyRetryPolicy,
) -> DeliveryDisposition {
    let started_at = Utc::now();
    let started = Instant::now();
    metrics.input_received(delivery.delivery_count > 1);

    let decoded = decode_engine_input(
        &delivery.payload,
        delivery.schema_header.as_deref(),
        delivery.identity_header.as_deref(),
        delivery.stream_sequence,
    );
    let identity = match &decoded {
        Ok(input) => input.identity.clone(),
        Err(failure) => failure.identity.clone(),
    };

    match store
        .final_classification(&identity.source_event_identity)
        .await
    {
        Ok(Some(_)) => {
            metrics.duplicate_skip();
            return acknowledge(delivery, ProcessingAction::Ack, readiness, sampler, false).await;
        }
        Ok(None) => {}
        Err(error) => {
            return handle_store_failure(
                delivery,
                readiness,
                metrics,
                sampler,
                "engine_final_lookup_failure",
                error,
            )
            .await;
        }
    }

    let mut result = match decoded {
        Ok(input) => processor.process(&input).await,
        Err(failure) => decode_failure_result(failure.kind, failure.evidence),
    };
    if result.action == ProcessingAction::Retry && retry_policy.exhausted(delivery.delivery_count) {
        if result.classification == EngineClassification::TransientDependencyFailure {
            let first_failure = match store
                .dependency_failure_context(&identity.source_event_identity)
                .await
            {
                Ok(context) => context,
                Err(error) => {
                    return handle_store_failure(
                        delivery,
                        readiness,
                        metrics,
                        sampler,
                        "engine_dependency_context_failure",
                        error,
                    )
                    .await;
                }
            };
            let completed_at = Utc::now();
            result = match dependency_exhausted_result(
                result,
                &identity,
                delivery.delivery_count,
                retry_policy,
                first_failure,
                started_at,
                completed_at,
            ) {
                Ok(result) => result,
                Err(error) => {
                    return handle_store_failure(
                        delivery,
                        readiness,
                        metrics,
                        sampler,
                        "engine_dependency_evidence_failure",
                        error,
                    )
                    .await;
                }
            };
        } else {
            result = ProcessResult {
                classification: EngineClassification::TerminalIntegrityFailure,
                detail_class: "engine_retries_exhausted",
                candidate_count: result.candidate_count,
                decision_count: 0,
                evidence: json!({
                    "prior_classification": result.classification.as_str(),
                    "prior_detail_class": result.detail_class,
                    "delivery_attempts_exhausted": delivery.delivery_count,
                    "prior_evidence": result.evidence
                }),
                evaluations: Vec::new(),
                action: ProcessingAction::Terminate,
                origin_metric: result.origin_metric,
            };
        }
    }

    let completed_at = Utc::now();
    let elapsed = started.elapsed();
    let record = ClassificationRecord {
        identity,
        classification: result.classification,
        detail_class: Some(result.detail_class),
        candidate_count: result.candidate_count,
        decision_count: result.decision_count,
        delivery_attempt: delivery.delivery_count,
        evidence: result.evidence.clone(),
        first_received_at: started_at,
        completed_at,
        processing_latency_ns: elapsed.as_nanos(),
        evaluations: result.evaluations.clone(),
    };
    let outcome = match store.persist_classification(&record).await {
        Ok(outcome) => outcome,
        Err(error) => {
            return handle_store_failure(
                delivery,
                readiness,
                metrics,
                sampler,
                "engine_persist_failure",
                error,
            )
            .await;
        }
    };
    readiness.set_persistence_healthy(true);
    if outcome == PersistOutcome::AlreadyFinal {
        metrics.duplicate_skip();
        return acknowledge(delivery, ProcessingAction::Ack, readiness, sampler, false).await;
    }

    metrics.input_processed(elapsed);
    record_result_metrics(metrics, &result);
    let terminal = result.action == ProcessingAction::Terminate;
    acknowledge(delivery, result.action, readiness, sampler, terminal).await
}

fn decode_failure_result(kind: InputFailureKind, evidence: serde_json::Value) -> ProcessResult {
    match kind {
        InputFailureKind::UnsupportedSchema => ProcessResult {
            classification: EngineClassification::UnsupportedSchema,
            detail_class: "unsupported_engine_schema",
            candidate_count: 0,
            decision_count: 0,
            evidence,
            evaluations: Vec::new(),
            action: ProcessingAction::Terminate,
            origin_metric: None,
        },
        InputFailureKind::Malformed => ProcessResult {
            classification: EngineClassification::MalformedInternalEvent,
            detail_class: "malformed_engine_input",
            candidate_count: 0,
            decision_count: 0,
            evidence,
            evaluations: Vec::new(),
            action: ProcessingAction::Retry,
            origin_metric: None,
        },
    }
}

fn record_result_metrics(metrics: &RuntimeMetrics, result: &ProcessResult) {
    if let Some(kind) = result.origin_metric {
        metrics.origin_classified(kind);
    }
    metrics.candidates(result.candidate_count);
    match result.classification {
        EngineClassification::NoRelevantRoute => metrics.no_route(),
        EngineClassification::ShadowAccepted | EngineClassification::CandidateRejected => {
            let accepted = result
                .evaluations
                .iter()
                .filter(|value| {
                    value.opportunity.decision.disposition == ShadowDisposition::Accepted
                })
                .count();
            let rejected = result.evaluations.len().saturating_sub(accepted);
            metrics.shadow_accepted(accepted);
            metrics.shadow_rejected(rejected.max(usize::from(
                result.classification == EngineClassification::CandidateRejected
                    && result.evaluations.is_empty(),
            )));
        }
        EngineClassification::CandidateGenerated => {}
        EngineClassification::DependencyExhausted => {
            metrics.dependency_exhausted();
            metrics.processing_failure();
        }
        EngineClassification::MalformedInternalEvent
        | EngineClassification::UnsupportedSchema
        | EngineClassification::TransientDependencyFailure
        | EngineClassification::TerminalIntegrityFailure => metrics.processing_failure(),
    }
}

fn dependency_exhausted_result(
    result: ProcessResult,
    identity: &crate::engine_input::InputIdentity,
    delivery_attempt: u64,
    retry_policy: DependencyRetryPolicy,
    first_failure: Option<DependencyFailureContext>,
    current_started_at: chrono::DateTime<Utc>,
    completed_at: chrono::DateTime<Utc>,
) -> Result<ProcessResult, StoreError> {
    let first_started_at = first_failure
        .as_ref()
        .map_or(current_started_at, |context| context.started_at.clone());
    let first_delivery_attempt = first_failure
        .as_ref()
        .map_or(delivery_attempt, |context| context.delivery_attempt);
    let original_classification = first_failure
        .as_ref()
        .map_or(result.classification, |context| context.classification);
    let original_failure_class = first_failure
        .as_ref()
        .and_then(|context| context.detail_class.as_deref())
        .unwrap_or(result.detail_class);
    let original_evidence = first_failure.as_ref().map_or_else(
        || result.evidence.clone(),
        |context| context.evidence.clone(),
    );
    let route_fingerprints = result
        .evidence
        .get("route_fingerprints")
        .cloned()
        .or_else(|| original_evidence.get("route_fingerprints").cloned())
        .unwrap_or_else(|| json!([]));
    let hash_input = serde_json::to_vec(&json!({
        "classification": result.classification.as_str(),
        "detail_class": result.detail_class,
        "evidence": &result.evidence
    }))
    .map_err(|_| StoreError::Integrity)?;
    let bounded_error_hash = hex::encode(Sha256::digest(hash_input));
    let mut evidence = json!({
        "source_event_identity": &identity.source_event_identity,
        "source_sequence": identity.source_sequence,
        "tx_hash": &identity.tx_hash,
        "route_fingerprints": route_fingerprints,
        "original_classification": original_classification.as_str(),
        "original_failure_class": original_failure_class,
        "final_failure_class": result.detail_class,
        "first_failure_at": first_started_at.to_rfc3339_opts(SecondsFormat::Nanos, true),
        "final_failure_at": completed_at.to_rfc3339_opts(SecondsFormat::Nanos, true),
        "first_failure_delivery_attempt": first_delivery_attempt,
        "delivery_attempts": delivery_attempt,
        "retry_count": delivery_attempt.saturating_sub(1),
        "exhaustion_limit": retry_policy.max_deliveries(),
        "quarantine_reason": "bounded_dependency_retries_exhausted",
        "provider_identifier": "rpc-gateway",
        "bounded_error_hash": bounded_error_hash,
        "execution_mode": "SHADOW",
        "shadow_only": true,
        "execution_eligible": false,
        "execution_request_created": false
    });
    evidence
        .as_object_mut()
        .ok_or(StoreError::Integrity)?
        .insert("original_evidence".to_string(), original_evidence);
    if serde_json::to_vec(&evidence)
        .map_err(|_| StoreError::Integrity)?
        .len()
        > MAX_EVIDENCE_BYTES
    {
        if first_failure.is_none() {
            return Err(StoreError::Integrity);
        }
        let object = evidence.as_object_mut().ok_or(StoreError::Integrity)?;
        object.remove("original_evidence");
        object.insert(
            "original_evidence_reference".to_string(),
            json!({
                "ledger": "shadow_engine_processing_attempts",
                "delivery_attempt": first_delivery_attempt
            }),
        );
    }
    if serde_json::to_vec(&evidence)
        .map_err(|_| StoreError::Integrity)?
        .len()
        > MAX_EVIDENCE_BYTES
    {
        return Err(StoreError::Integrity);
    }
    Ok(ProcessResult {
        classification: EngineClassification::DependencyExhausted,
        detail_class: "dependency_retries_exhausted",
        candidate_count: result.candidate_count,
        decision_count: 0,
        evidence,
        evaluations: Vec::new(),
        action: ProcessingAction::Ack,
        origin_metric: result.origin_metric,
    })
}

async fn acknowledge(
    delivery: Delivery,
    action: ProcessingAction,
    readiness: &RuntimeReadiness,
    sampler: &LogSampler,
    terminal: bool,
) -> DeliveryDisposition {
    let result = match action {
        ProcessingAction::Ack => delivery.acker.ack_confirmed().await,
        ProcessingAction::Retry => delivery.acker.nak(RETRY_DELAY).await,
        ProcessingAction::Terminate => delivery.acker.term().await,
    };
    if terminal {
        readiness.mark_integrity_loss();
    }
    match result {
        Ok(()) => {
            readiness.set_acknowledgements_healthy(true);
            if terminal {
                DeliveryDisposition::IntegrityFailure
            } else {
                DeliveryDisposition::Continue
            }
        }
        Err(error) => {
            readiness.set_acknowledgements_healthy(false);
            sampled_pipeline_failure(sampler, "engine_ack_failure", error);
            if terminal {
                DeliveryDisposition::IntegrityFailure
            } else {
                DeliveryDisposition::AcknowledgementFailed
            }
        }
    }
}

async fn progress_without_ack(
    delivery: Delivery,
    readiness: &RuntimeReadiness,
    sampler: &LogSampler,
) -> DeliveryDisposition {
    if let Err(error) = delivery.acker.progress().await {
        readiness.set_acknowledgements_healthy(false);
        sampled_pipeline_failure(sampler, "engine_progress_failure", error);
    }
    DeliveryDisposition::StoreFailed
}

async fn handle_store_failure(
    delivery: Delivery,
    readiness: &RuntimeReadiness,
    metrics: &RuntimeMetrics,
    sampler: &LogSampler,
    failure_class: &'static str,
    error: StoreError,
) -> DeliveryDisposition {
    metrics.processing_failure();
    readiness.set_persistence_healthy(false);
    let terminal = matches!(
        error,
        StoreError::Configuration | StoreError::Schema | StoreError::Integrity
    );
    sampled_store_failure(sampler, failure_class, error);
    if terminal {
        acknowledge(
            delivery,
            ProcessingAction::Terminate,
            readiness,
            sampler,
            true,
        )
        .await
    } else {
        progress_without_ack(delivery, readiness, sampler).await
    }
}

fn sampled_pipeline_failure(sampler: &LogSampler, class: &'static str, error: PipelineError) {
    if let Some(suppressed) = sampler.sample(class) {
        tracing::warn!(
            event = "phoenix_engine_jetstream_failure",
            error_class = %error,
            failure_class = class,
            suppressed
        );
    }
}

fn sampled_store_failure(sampler: &LogSampler, class: &'static str, error: StoreError) {
    if let Some(suppressed) = sampler.sample(class) {
        tracing::warn!(
            event = "phoenix_engine_postgres_failure",
            error_class = %error,
            failure_class = class,
            suppressed
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::domain::Address;
    use crate::engine_jetstream::{ConsumerState, DeliveryAcker};
    use crate::persistence::{validate_record, ClassificationRecord};
    use crate::shadow_processor::{
        CandidateBatch, CandidateEvaluator, EvaluationError, RouteRegistry, RuntimeRoute,
        UnavailableEvaluator,
    };
    use async_trait::async_trait;
    use phoenix_recorder::model::{
        decode_message, engine_event_identity, ARBITRUM_ONE_CHAIN_ID, ENGINE_INPUT_SCHEMA_VERSION,
        NORMALIZED_SCHEMA_VERSION,
    };
    use serde::Deserialize;
    use serde_json::json;
    use std::collections::{HashMap, HashSet, VecDeque};
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::Mutex;

    const ROUTER: &str = "0x68b3465833fb72a70ecdf485e0e4c7bd8665fc45";
    const WETH: &str = "0x82af49447d8a07e3bd95bd0d56f35241523fbab1";
    const USDC: &str = "0xaf88d065e77c8cc2239327c5edb3a432268e5831";

    #[derive(Debug)]
    struct ScriptedEvaluator {
        outcomes: Mutex<VecDeque<Result<CandidateBatch, EvaluationError>>>,
        calls: AtomicUsize,
    }

    impl ScriptedEvaluator {
        fn new(outcomes: Vec<Result<CandidateBatch, EvaluationError>>) -> Self {
            Self {
                outcomes: Mutex::new(outcomes.into()),
                calls: AtomicUsize::new(0),
            }
        }
    }

    #[async_trait]
    impl CandidateEvaluator for ScriptedEvaluator {
        async fn evaluate(
            &self,
            _input: &crate::engine_input::EngineInput,
            _origin: &crate::origin::OriginEvent,
            _route: &RuntimeRoute,
        ) -> Result<CandidateBatch, EvaluationError> {
            self.calls.fetch_add(1, Ordering::Relaxed);
            self.outcomes
                .lock()
                .unwrap()
                .pop_front()
                .unwrap_or_else(|| {
                    Ok(CandidateBatch {
                        evaluations: Vec::new(),
                        evidence: json!({"evaluation": "complete"}),
                    })
                })
        }
    }

    #[derive(Debug)]
    struct SoakEvaluator {
        recover_once: HashSet<u64>,
        exhausted_sequence: u64,
        calls: Mutex<HashMap<u64, usize>>,
    }

    #[async_trait]
    impl CandidateEvaluator for SoakEvaluator {
        async fn evaluate(
            &self,
            input: &crate::engine_input::EngineInput,
            _origin: &crate::origin::OriginEvent,
            _route: &RuntimeRoute,
        ) -> Result<CandidateBatch, EvaluationError> {
            let sequence = input.identity.source_sequence;
            let mut calls = self.calls.lock().unwrap();
            let calls_for_sequence = calls.entry(sequence).or_insert(0);
            *calls_for_sequence += 1;
            if sequence == self.exhausted_sequence
                || (self.recover_once.contains(&sequence) && *calls_for_sequence == 1)
            {
                return Err(EvaluationError::Transient("rpc_gateway_unavailable"));
            }
            Ok(CandidateBatch {
                evaluations: Vec::new(),
                evidence: json!({"evaluation": "deterministic_no_candidate"}),
            })
        }
    }

    #[derive(Debug, Deserialize)]
    struct DependencySoakFixture {
        schema_version: String,
        normal_before: u64,
        route_candidates: u64,
        recovered_failures: u64,
        exhaustion_limit: i64,
        normal_after: u64,
        duplicate_exhausted: bool,
        expected_deliveries: usize,
        expected_persisted_attempts: usize,
    }

    #[derive(Debug)]
    struct FakeAcker {
        events: Arc<Mutex<Vec<&'static str>>>,
        fail_ack: bool,
    }

    #[async_trait]
    impl DeliveryAcker for FakeAcker {
        async fn ack_confirmed(&self) -> Result<(), PipelineError> {
            self.events.lock().unwrap().push("ack");
            if self.fail_ack {
                Err(PipelineError::Acknowledgement)
            } else {
                Ok(())
            }
        }

        async fn nak(&self, _delay: Duration) -> Result<(), PipelineError> {
            self.events.lock().unwrap().push("nak");
            Ok(())
        }

        async fn progress(&self) -> Result<(), PipelineError> {
            self.events.lock().unwrap().push("progress");
            Ok(())
        }

        async fn term(&self) -> Result<(), PipelineError> {
            self.events.lock().unwrap().push("term");
            Ok(())
        }
    }

    #[derive(Debug)]
    struct FakeStore {
        events: Arc<Mutex<Vec<&'static str>>>,
        final_value: Mutex<Option<EngineClassification>>,
        final_records: Mutex<HashMap<String, EngineClassification>>,
        persist_error: bool,
        records: Mutex<Vec<ClassificationRecord>>,
    }

    #[async_trait]
    impl ShadowStore for FakeStore {
        async fn ping(&self) -> Result<(), StoreError> {
            Ok(())
        }

        async fn verify_schema(&self) -> Result<(), StoreError> {
            Ok(())
        }

        async fn final_classification(
            &self,
            source_event_identity: &str,
        ) -> Result<Option<EngineClassification>, StoreError> {
            if let Some(value) = *self.final_value.lock().unwrap() {
                return Ok(Some(value));
            }
            Ok(self
                .final_records
                .lock()
                .unwrap()
                .get(source_event_identity)
                .copied())
        }

        async fn dependency_failure_context(
            &self,
            source_event_identity: &str,
        ) -> Result<Option<DependencyFailureContext>, StoreError> {
            Ok(self
                .records
                .lock()
                .unwrap()
                .iter()
                .filter(|record| {
                    record.identity.source_event_identity == source_event_identity
                        && record.classification == EngineClassification::TransientDependencyFailure
                })
                .min_by_key(|record| record.delivery_attempt)
                .map(|record| DependencyFailureContext {
                    classification: record.classification,
                    detail_class: record.detail_class.map(str::to_string),
                    evidence: record.evidence.clone(),
                    started_at: record.first_received_at.clone(),
                    delivery_attempt: record.delivery_attempt,
                }))
        }

        async fn persist_classification(
            &self,
            record: &ClassificationRecord,
        ) -> Result<PersistOutcome, StoreError> {
            self.events.lock().unwrap().push("persist");
            if self.persist_error {
                Err(StoreError::Transaction)
            } else {
                self.records.lock().unwrap().push(record.clone());
                if record.classification.is_final() {
                    self.final_records.lock().unwrap().insert(
                        record.identity.source_event_identity.clone(),
                        record.classification,
                    );
                }
                Ok(PersistOutcome::Committed)
            }
        }
    }

    fn processor() -> ShadowProcessor {
        ShadowProcessor::new(
            vec![Address::parse("0x68b3465833fb72a70ecdf485e0e4c7bd8665fc45").unwrap()],
            RouteRegistry::from_json("[]").unwrap(),
            Arc::new(UnavailableEvaluator),
        )
        .unwrap()
    }

    fn route_processor(evaluator: Arc<dyn CandidateEvaluator>) -> ShadowProcessor {
        ShadowProcessor::new(
            vec![Address::parse(ROUTER).unwrap()],
            RouteRegistry::from_json(include_str!(
                "../../fixtures/routes/weth_usdc_uniswap_v3.json"
            ))
            .unwrap(),
            evaluator,
        )
        .unwrap()
    }

    fn slot_address(address: &str) -> String {
        format!(
            "000000000000000000000000{}",
            address.trim_start_matches("0x")
        )
    }

    fn slot_u(value: u128) -> String {
        format!("{value:064x}")
    }

    fn route_payload(sequence: u64) -> Vec<u8> {
        let calldata = format!(
            "0x04e45aaf{}{}{}{}{}{}{}",
            slot_address(WETH),
            slot_address(USDC),
            slot_u(500),
            slot_address("0x1111111111111111111111111111111111111111"),
            slot_u(1_000_000),
            slot_u(0),
            slot_u(0)
        );
        serde_json::to_vec(&json!({
            "schema_version": NORMALIZED_SCHEMA_VERSION,
            "sequence": sequence,
            "timestamp_unix_ms": 1_700_000_000_000_u64 + sequence,
            "tx_hash": format!("0x{sequence:064x}"),
            "tx_type": "0x02",
            "chain_id": ARBITRUM_ONE_CHAIN_ID,
            "from": "0x1111111111111111111111111111111111111111",
            "to": ROUTER,
            "nonce": sequence,
            "value": "0",
            "calldata": calldata,
            "gas_limit": "300000",
            "max_fee_per_gas": "100",
            "max_priority_fee_per_gas": "2",
            "raw_tx": "AQID",
            "ingested_at_unix_ns": 1_700_000_000_123_456_789_i64
        }))
        .unwrap()
    }

    fn normal_payload(sequence: u64) -> Vec<u8> {
        let mut value: serde_json::Value =
            serde_json::from_slice(&route_payload(sequence)).unwrap();
        value["to"] = json!("0x2222222222222222222222222222222222222222");
        value["calldata"] = json!("0x1234");
        serde_json::to_vec(&value).unwrap()
    }

    fn payload() -> Vec<u8> {
        serde_json::to_vec(&json!({
            "schema_version": NORMALIZED_SCHEMA_VERSION,
            "sequence": 7,
            "timestamp_unix_ms": 1_700_000_000_000_u64,
            "tx_hash": format!("0x{}", "a".repeat(64)),
            "tx_type": "0x02",
            "chain_id": ARBITRUM_ONE_CHAIN_ID,
            "from": "0x1111111111111111111111111111111111111111",
            "to": "0x2222222222222222222222222222222222222222",
            "nonce": 7,
            "value": "3",
            "calldata": "0x1234",
            "gas_limit": "21000",
            "max_fee_per_gas": "100",
            "max_priority_fee_per_gas": "2",
            "raw_tx": "AQID",
            "ingested_at_unix_ns": 1_700_000_000_123_456_789_i64
        }))
        .unwrap()
    }

    fn delivery(
        events: Arc<Mutex<Vec<&'static str>>>,
        payload: Vec<u8>,
        attempt: u64,
        fail_ack: bool,
    ) -> Delivery {
        let identity = decode_message(&payload)
            .ok()
            .map(|value| engine_event_identity(&value.tx));
        Delivery {
            payload,
            schema_header: Some(ENGINE_INPUT_SCHEMA_VERSION.to_string()),
            identity_header: identity,
            stream_sequence: 11,
            delivery_count: attempt,
            acker: Arc::new(FakeAcker { events, fail_ack }),
        }
    }

    fn store(
        events: Arc<Mutex<Vec<&'static str>>>,
        final_value: Option<EngineClassification>,
        persist_error: bool,
    ) -> FakeStore {
        FakeStore {
            events,
            final_value: Mutex::new(final_value),
            final_records: Mutex::new(HashMap::new()),
            persist_error,
            records: Mutex::new(Vec::new()),
        }
    }

    fn ready_state() -> RuntimeReadiness {
        let readiness = RuntimeReadiness::new();
        readiness.set_postgres_connected(true);
        readiness.set_schema_verified(true);
        readiness.set_nats_connected(true);
        readiness.set_stream_ready(true);
        readiness.set_consumer_ready(true);
        readiness.set_fetching_active(true);
        readiness.set_persistence_healthy(true);
        readiness.set_strategy_configured(true);
        readiness.set_evaluation_dependencies_ready(true);
        readiness
    }

    async fn run(delivery: Delivery, store: &FakeStore) -> DeliveryDisposition {
        run_with(
            delivery,
            store,
            &processor(),
            &RuntimeReadiness::new(),
            &RuntimeMetrics::default(),
            DependencyRetryPolicy::engine_default().unwrap(),
        )
        .await
    }

    async fn run_with(
        delivery: Delivery,
        store: &FakeStore,
        processor: &ShadowProcessor,
        readiness: &RuntimeReadiness,
        metrics: &RuntimeMetrics,
        retry_policy: DependencyRetryPolicy,
    ) -> DeliveryDisposition {
        process_delivery(
            delivery,
            store,
            processor,
            readiness,
            metrics,
            &LogSampler::new(Duration::ZERO),
            retry_policy,
        )
        .await
    }

    #[tokio::test]
    async fn final_classification_is_committed_before_confirmed_ack() {
        let events = Arc::new(Mutex::new(Vec::new()));
        let store = store(events.clone(), None, false);
        assert_eq!(
            run(delivery(events.clone(), payload(), 1, false), &store).await,
            DeliveryDisposition::Continue
        );
        assert_eq!(*events.lock().unwrap(), vec!["persist", "ack"]);
        assert_eq!(
            store.records.lock().unwrap()[0].classification,
            EngineClassification::NoRelevantRoute
        );
    }

    #[tokio::test]
    async fn malformed_input_is_persisted_before_nak_and_terminated_at_delivery_limit() {
        let events = Arc::new(Mutex::new(Vec::new()));
        let store = store(events.clone(), None, false);
        assert_eq!(
            run(delivery(events.clone(), b"bad".to_vec(), 1, false), &store).await,
            DeliveryDisposition::Continue
        );
        assert_eq!(*events.lock().unwrap(), vec!["persist", "nak"]);
        assert_eq!(
            store.records.lock().unwrap()[0].classification,
            EngineClassification::MalformedInternalEvent
        );

        events.lock().unwrap().clear();
        assert_eq!(
            run(
                delivery(
                    events.clone(),
                    b"bad".to_vec(),
                    ENGINE_MAX_DELIVERIES as u64,
                    false,
                ),
                &store,
            )
            .await,
            DeliveryDisposition::IntegrityFailure
        );
        assert_eq!(*events.lock().unwrap(), vec!["persist", "term"]);
        assert_eq!(
            store.records.lock().unwrap()[1].classification,
            EngineClassification::TerminalIntegrityFailure
        );
    }

    #[tokio::test]
    async fn retryable_dependency_failure_naks_without_integrity_loss() {
        let events = Arc::new(Mutex::new(Vec::new()));
        let store = store(events.clone(), None, false);
        let evaluator = Arc::new(ScriptedEvaluator::new(vec![Err(
            EvaluationError::Transient("rpc_gateway_unavailable"),
        )]));
        let processor = route_processor(evaluator);
        let readiness = ready_state();
        let metrics = RuntimeMetrics::default();
        assert_eq!(
            run_with(
                delivery(events.clone(), route_payload(101), 1, false),
                &store,
                &processor,
                &readiness,
                &metrics,
                DependencyRetryPolicy::bounded(3).unwrap(),
            )
            .await,
            DeliveryDisposition::Continue
        );
        assert_eq!(*events.lock().unwrap(), vec!["persist", "nak"]);
        assert_eq!(
            store.records.lock().unwrap()[0].classification,
            EngineClassification::TransientDependencyFailure
        );
        assert_eq!(readiness.ready(), Ok(()));
    }

    #[tokio::test]
    async fn recovered_dependency_failure_proceeds_without_synthetic_profitability() {
        let events = Arc::new(Mutex::new(Vec::new()));
        let store = store(events.clone(), None, false);
        let evaluator = Arc::new(ScriptedEvaluator::new(vec![
            Err(EvaluationError::Transient("rpc_gateway_unavailable")),
            Ok(CandidateBatch {
                evaluations: Vec::new(),
                evidence: json!({"evaluation": "recovered"}),
            }),
        ]));
        let processor = route_processor(evaluator);
        let readiness = ready_state();
        let metrics = RuntimeMetrics::default();
        let policy = DependencyRetryPolicy::bounded(3).unwrap();
        assert_eq!(
            run_with(
                delivery(events.clone(), route_payload(102), 1, false),
                &store,
                &processor,
                &readiness,
                &metrics,
                policy,
            )
            .await,
            DeliveryDisposition::Continue
        );
        events.lock().unwrap().clear();
        assert_eq!(
            run_with(
                delivery(events.clone(), route_payload(102), 2, false),
                &store,
                &processor,
                &readiness,
                &metrics,
                policy,
            )
            .await,
            DeliveryDisposition::Continue
        );
        assert_eq!(*events.lock().unwrap(), vec!["persist", "ack"]);
        let records = store.records.lock().unwrap();
        assert_eq!(records.len(), 2);
        assert_eq!(
            records[1].classification,
            EngineClassification::CandidateRejected
        );
        assert!(records[1].evaluations.is_empty());
        assert_eq!(records[1].decision_count, 0);
        assert_eq!(readiness.ready(), Ok(()));
    }

    #[tokio::test]
    async fn exhausted_dependency_is_quarantined_with_bounded_original_evidence() {
        let events = Arc::new(Mutex::new(Vec::new()));
        let store = store(events.clone(), None, false);
        let evaluator = Arc::new(ScriptedEvaluator::new(vec![
            Err(EvaluationError::Transient("rpc_gateway_unavailable")),
            Err(EvaluationError::Transient("rpc_gateway_unavailable")),
        ]));
        let processor = route_processor(evaluator.clone());
        let readiness = ready_state();
        let metrics = RuntimeMetrics::default();
        let policy = DependencyRetryPolicy::bounded(2).unwrap();
        assert_eq!(
            run_with(
                delivery(events.clone(), route_payload(103), 1, false),
                &store,
                &processor,
                &readiness,
                &metrics,
                policy,
            )
            .await,
            DeliveryDisposition::Continue
        );
        events.lock().unwrap().clear();
        assert_eq!(
            run_with(
                delivery(events.clone(), route_payload(103), 2, false),
                &store,
                &processor,
                &readiness,
                &metrics,
                policy,
            )
            .await,
            DeliveryDisposition::Continue
        );
        assert_eq!(*events.lock().unwrap(), vec!["persist", "ack"]);
        assert_eq!(evaluator.calls.load(Ordering::Relaxed), 2);
        let records = store.records.lock().unwrap();
        let exhausted = records.last().unwrap();
        assert_eq!(
            exhausted.classification,
            EngineClassification::DependencyExhausted
        );
        assert_eq!(exhausted.detail_class, Some("dependency_retries_exhausted"));
        assert_eq!(exhausted.decision_count, 0);
        assert!(exhausted.evaluations.is_empty());
        assert_eq!(exhausted.evidence["source_sequence"], 103);
        assert_eq!(
            exhausted.evidence["original_failure_class"],
            "rpc_gateway_unavailable"
        );
        assert_eq!(exhausted.evidence["delivery_attempts"], 2);
        assert_eq!(exhausted.evidence["retry_count"], 1);
        assert_eq!(exhausted.evidence["exhaustion_limit"], 2);
        assert_eq!(exhausted.evidence["provider_identifier"], "rpc-gateway");
        assert_eq!(exhausted.evidence["execution_mode"], "SHADOW");
        assert_eq!(exhausted.evidence["execution_eligible"], false);
        assert_eq!(exhausted.evidence["execution_request_created"], false);
        assert_eq!(
            exhausted.evidence["original_evidence"]["dependency_failure_class"],
            "rpc_gateway_unavailable"
        );
        assert!(!exhausted.evidence.to_string().contains("profit"));
        assert!(serde_json::to_vec(&exhausted.evidence).unwrap().len() <= MAX_EVIDENCE_BYTES);
        assert_eq!(validate_record(exhausted), Ok(()));
        assert_eq!(readiness.ready(), Ok(()));
        let rendered = metrics.render(&readiness);
        assert!(rendered.contains("phoenix_engine_dependency_exhausted_total 1"));
        assert!(!rendered.contains("source_event_identity="));
    }

    #[tokio::test]
    async fn duplicate_exhausted_delivery_is_acked_without_reevaluation() {
        let events = Arc::new(Mutex::new(Vec::new()));
        let store = store(events.clone(), None, false);
        let evaluator = Arc::new(ScriptedEvaluator::new(vec![
            Err(EvaluationError::Transient("rpc_gateway_unavailable")),
            Err(EvaluationError::Transient("rpc_gateway_unavailable")),
        ]));
        let processor = route_processor(evaluator.clone());
        let readiness = ready_state();
        let metrics = RuntimeMetrics::default();
        let policy = DependencyRetryPolicy::bounded(2).unwrap();
        for attempt in 1..=2 {
            assert_eq!(
                run_with(
                    delivery(events.clone(), route_payload(104), attempt, false),
                    &store,
                    &processor,
                    &readiness,
                    &metrics,
                    policy,
                )
                .await,
                DeliveryDisposition::Continue
            );
        }
        let record_count = store.records.lock().unwrap().len();
        events.lock().unwrap().clear();
        assert_eq!(
            run_with(
                delivery(events.clone(), route_payload(104), 3, false),
                &store,
                &processor,
                &readiness,
                &metrics,
                policy,
            )
            .await,
            DeliveryDisposition::Continue
        );
        assert_eq!(*events.lock().unwrap(), vec!["ack"]);
        assert_eq!(store.records.lock().unwrap().len(), record_count);
        assert_eq!(evaluator.calls.load(Ordering::Relaxed), 2);
        assert_eq!(readiness.ready(), Ok(()));
    }

    #[test]
    fn dependency_retry_policy_rejects_zero_unbounded_and_oversized_limits() {
        for invalid in [i64::MIN, -1, 0, 1, 101, i64::MAX] {
            assert!(DependencyRetryPolicy::bounded(invalid).is_err());
        }
        assert_eq!(
            DependencyRetryPolicy::bounded(2).unwrap().max_deliveries(),
            2
        );
        assert_eq!(
            DependencyRetryPolicy::bounded(100)
                .unwrap()
                .max_deliveries(),
            100
        );
        assert_eq!(
            DependencyRetryPolicy::engine_default()
                .unwrap()
                .max_deliveries(),
            ENGINE_MAX_DELIVERIES as u64
        );
    }

    #[tokio::test]
    async fn deterministic_dependency_soak_keeps_engine_alive_after_exhaustion() {
        let fixture: DependencySoakFixture = serde_json::from_str(include_str!(
            "../../fixtures/engine/dependency_exhaustion_soak.json"
        ))
        .unwrap();
        assert_eq!(fixture.schema_version, "phoenix.engine.dependency-soak.v1");
        assert!(fixture.duplicate_exhausted);
        let recovered_start = 30_000_u64;
        let exhausted_sequence = 40_000_u64;
        let recover_once = (0..fixture.recovered_failures)
            .map(|offset| recovered_start + offset)
            .collect::<HashSet<_>>();
        let evaluator = Arc::new(SoakEvaluator {
            recover_once,
            exhausted_sequence,
            calls: Mutex::new(HashMap::new()),
        });
        let processor = route_processor(evaluator);
        let events = Arc::new(Mutex::new(Vec::new()));
        let store = store(events.clone(), None, false);
        let readiness = ready_state();
        let metrics = RuntimeMetrics::default();
        let policy = DependencyRetryPolicy::bounded(fixture.exhaustion_limit).unwrap();

        for offset in 0..fixture.normal_before {
            let disposition = run_with(
                delivery(events.clone(), normal_payload(10_000 + offset), 1, false),
                &store,
                &processor,
                &readiness,
                &metrics,
                policy,
            )
            .await;
            assert_eq!(disposition, DeliveryDisposition::Continue);
        }
        for offset in 0..fixture.route_candidates {
            let disposition = run_with(
                delivery(events.clone(), route_payload(20_000 + offset), 1, false),
                &store,
                &processor,
                &readiness,
                &metrics,
                policy,
            )
            .await;
            assert_eq!(disposition, DeliveryDisposition::Continue);
        }
        for offset in 0..fixture.recovered_failures {
            for attempt in 1..=2 {
                let disposition = run_with(
                    delivery(
                        events.clone(),
                        route_payload(recovered_start + offset),
                        attempt,
                        false,
                    ),
                    &store,
                    &processor,
                    &readiness,
                    &metrics,
                    policy,
                )
                .await;
                assert_eq!(disposition, DeliveryDisposition::Continue);
            }
        }
        for attempt in 1..=policy.max_deliveries() {
            let disposition = run_with(
                delivery(
                    events.clone(),
                    route_payload(exhausted_sequence),
                    attempt,
                    false,
                ),
                &store,
                &processor,
                &readiness,
                &metrics,
                policy,
            )
            .await;
            assert_eq!(disposition, DeliveryDisposition::Continue);
        }
        for offset in 0..fixture.normal_after {
            let disposition = run_with(
                delivery(events.clone(), normal_payload(50_000 + offset), 1, false),
                &store,
                &processor,
                &readiness,
                &metrics,
                policy,
            )
            .await;
            assert_eq!(disposition, DeliveryDisposition::Continue);
        }
        let disposition = run_with(
            delivery(
                events.clone(),
                route_payload(exhausted_sequence),
                policy.max_deliveries() + 1,
                false,
            ),
            &store,
            &processor,
            &readiness,
            &metrics,
            policy,
        )
        .await;
        assert_eq!(disposition, DeliveryDisposition::Continue);

        let records = store.records.lock().unwrap();
        assert_eq!(records.len(), fixture.expected_persisted_attempts);
        let actual = records
            .iter()
            .map(|record| record.classification)
            .collect::<Vec<_>>();
        let mut expected =
            vec![EngineClassification::NoRelevantRoute; fixture.normal_before as usize];
        expected.extend(vec![
            EngineClassification::CandidateRejected;
            fixture.route_candidates as usize
        ]);
        for _ in 0..fixture.recovered_failures {
            expected.push(EngineClassification::TransientDependencyFailure);
            expected.push(EngineClassification::CandidateRejected);
        }
        expected.push(EngineClassification::TransientDependencyFailure);
        expected.push(EngineClassification::DependencyExhausted);
        expected.extend(vec![
            EngineClassification::NoRelevantRoute;
            fixture.normal_after as usize
        ]);
        assert_eq!(actual, expected);
        drop(records);

        let events = events.lock().unwrap();
        assert_eq!(
            events.iter().filter(|event| **event == "persist").count(),
            210
        );
        assert_eq!(events.iter().filter(|event| **event == "ack").count(), 186);
        assert_eq!(events.iter().filter(|event| **event == "nak").count(), 25);
        assert_eq!(events.iter().filter(|event| **event == "term").count(), 0);
        drop(events);
        assert_eq!(fixture.expected_deliveries, 211);
        assert_eq!(readiness.ready(), Ok(()));
        let rendered = metrics.render(&readiness);
        assert!(rendered.contains("phoenix_engine_inputs_received_total 211"));
        assert!(rendered.contains("phoenix_engine_inputs_processed_total 210"));
        assert!(rendered.contains("phoenix_engine_dependency_exhausted_total 1"));
        assert!(rendered.contains("phoenix_engine_duplicate_skips_total 1"));
    }

    #[tokio::test]
    async fn database_failure_sends_progress_and_never_acknowledges() {
        let events = Arc::new(Mutex::new(Vec::new()));
        let store = store(events.clone(), None, true);
        assert_eq!(
            run(delivery(events.clone(), payload(), 1, false), &store).await,
            DeliveryDisposition::StoreFailed
        );
        assert_eq!(*events.lock().unwrap(), vec!["persist", "progress"]);
    }

    #[tokio::test]
    async fn final_redelivery_skips_duplicate_evaluation_and_retries_ack() {
        let events = Arc::new(Mutex::new(Vec::new()));
        let store = store(
            events.clone(),
            Some(EngineClassification::NoRelevantRoute),
            false,
        );
        assert_eq!(
            run(delivery(events.clone(), payload(), 2, false), &store).await,
            DeliveryDisposition::Continue
        );
        assert_eq!(*events.lock().unwrap(), vec!["ack"]);
        assert!(store.records.lock().unwrap().is_empty());
    }

    #[tokio::test]
    async fn ack_failure_after_commit_leaves_final_record_for_redelivery_skip() {
        let events = Arc::new(Mutex::new(Vec::new()));
        let store = store(events.clone(), None, false);
        assert_eq!(
            run(delivery(events.clone(), payload(), 1, true), &store).await,
            DeliveryDisposition::AcknowledgementFailed
        );
        assert_eq!(*events.lock().unwrap(), vec!["persist", "ack"]);
        assert_eq!(store.records.lock().unwrap().len(), 1);
    }

    #[derive(Debug)]
    struct UnusedFetcher;

    #[async_trait]
    impl MessageFetcher for UnusedFetcher {
        async fn fetch_batch(&self) -> Result<Vec<Delivery>, PipelineError> {
            Ok(Vec::new())
        }

        async fn state(&self) -> Result<ConsumerState, PipelineError> {
            Ok(ConsumerState::default())
        }
    }

    #[derive(Debug)]
    struct OneBatchFetcher {
        batch: Mutex<Option<Vec<Delivery>>>,
        shutdown: CancellationToken,
    }

    #[async_trait]
    impl MessageFetcher for OneBatchFetcher {
        async fn fetch_batch(&self) -> Result<Vec<Delivery>, PipelineError> {
            if let Some(batch) = self.batch.lock().unwrap().take() {
                return Ok(batch);
            }
            self.shutdown.cancel();
            std::future::pending::<Result<Vec<Delivery>, PipelineError>>().await
        }

        async fn state(&self) -> Result<ConsumerState, PipelineError> {
            Ok(ConsumerState::default())
        }
    }

    #[tokio::test]
    async fn exhausted_message_does_not_restart_consumer_and_later_message_is_processed() {
        let events = Arc::new(Mutex::new(Vec::new()));
        let store = Arc::new(store(events.clone(), None, false));
        let evaluator = Arc::new(ScriptedEvaluator::new(vec![
            Err(EvaluationError::Transient("rpc_gateway_unavailable")),
            Err(EvaluationError::Transient("rpc_gateway_unavailable")),
        ]));
        let processor = Arc::new(route_processor(evaluator));
        let readiness = ready_state();
        let metrics = RuntimeMetrics::default();
        let policy = DependencyRetryPolicy::bounded(2).unwrap();

        assert_eq!(
            run_with(
                delivery(events.clone(), route_payload(105), 1, false),
                store.as_ref(),
                processor.as_ref(),
                &readiness,
                &metrics,
                policy,
            )
            .await,
            DeliveryDisposition::Continue
        );
        events.lock().unwrap().clear();

        let shutdown = CancellationToken::new();
        let fetcher = Arc::new(OneBatchFetcher {
            batch: Mutex::new(Some(vec![
                delivery(events.clone(), route_payload(105), 2, false),
                delivery(events.clone(), payload(), 1, false),
            ])),
            shutdown: shutdown.clone(),
        });
        let exit = consume_engine_messages(
            fetcher,
            store.clone(),
            processor,
            readiness.clone(),
            metrics.clone(),
            LogSampler::new(Duration::ZERO),
            policy,
            shutdown,
        )
        .await;

        assert_eq!(exit, RuntimeExit::Shutdown);
        assert_eq!(
            *events.lock().unwrap(),
            vec!["persist", "ack", "persist", "ack"]
        );
        let classifications = store
            .records
            .lock()
            .unwrap()
            .iter()
            .map(|record| record.classification)
            .collect::<Vec<_>>();
        assert!(classifications.contains(&EngineClassification::DependencyExhausted));
        assert_eq!(
            classifications.last(),
            Some(&EngineClassification::NoRelevantRoute)
        );
        assert_eq!(readiness.ready(), Ok(()));
        assert!(metrics
            .render(&readiness)
            .contains("phoenix_engine_dependency_exhausted_total 1"));
    }

    #[tokio::test]
    async fn true_internal_contract_failure_still_returns_integrity_exit() {
        let events = Arc::new(Mutex::new(Vec::new()));
        let store = Arc::new(store(events.clone(), None, false));
        let readiness = ready_state();
        let shutdown = CancellationToken::new();
        let fetcher = Arc::new(OneBatchFetcher {
            batch: Mutex::new(Some(vec![delivery(
                events.clone(),
                b"bad".to_vec(),
                2,
                false,
            )])),
            shutdown: shutdown.clone(),
        });
        let exit = consume_engine_messages(
            fetcher,
            store,
            Arc::new(processor()),
            readiness.clone(),
            RuntimeMetrics::default(),
            LogSampler::new(Duration::ZERO),
            DependencyRetryPolicy::bounded(2).unwrap(),
            shutdown,
        )
        .await;
        assert_eq!(exit, RuntimeExit::IntegrityFailure);
        assert_eq!(*events.lock().unwrap(), vec!["persist", "term"]);
        assert_eq!(
            readiness.ready(),
            Err("terminal Engine integrity condition detected")
        );
    }

    #[tokio::test]
    async fn cancellation_stops_an_idle_consumer() {
        let shutdown = CancellationToken::new();
        shutdown.cancel();
        let events = Arc::new(Mutex::new(Vec::new()));
        let exit = consume_engine_messages(
            Arc::new(UnusedFetcher),
            Arc::new(store(events, None, false)),
            Arc::new(processor()),
            RuntimeReadiness::new(),
            RuntimeMetrics::default(),
            LogSampler::default(),
            DependencyRetryPolicy::engine_default().unwrap(),
            shutdown,
        )
        .await;
        assert_eq!(exit, RuntimeExit::Shutdown);
    }

    #[test]
    fn runtime_source_does_not_add_submission_or_payload_logging() {
        let source = include_str!("runtime.rs");
        for forbidden in [
            ["send", "_raw", "_transaction"].concat(),
            ["sign", "_transaction"].concat(),
            ["payload", " ="].concat(),
        ] {
            assert!(!source.contains(&forbidden));
        }
    }
}
