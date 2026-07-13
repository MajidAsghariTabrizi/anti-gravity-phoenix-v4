use crate::engine_input::{decode_engine_input, EngineClassification, InputFailureKind};
use crate::engine_jetstream::{Delivery, MessageFetcher, PipelineError, RETRY_DELAY};
use crate::metrics::RuntimeMetrics;
use crate::opportunity::ShadowDisposition;
use crate::persistence::{ClassificationRecord, PersistOutcome, ShadowStore, StoreError};
use crate::runtime_state::RuntimeReadiness;
use crate::shadow_processor::{ProcessResult, ProcessingAction, ShadowProcessor};
use chrono::Utc;
use phoenix_recorder::engine_stream::ENGINE_MAX_DELIVERIES;
use phoenix_recorder::logging::LogSampler;
use serde_json::json;
use std::sync::Arc;
use std::time::{Duration, Instant};
use tokio_util::sync::CancellationToken;

const CONSUMER_STATE_REFRESH_INTERVAL: Duration = Duration::from_secs(1);

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
            metrics.processing_failure();
            readiness.set_persistence_healthy(false);
            sampled_store_failure(sampler, "engine_final_lookup_failure", error);
            return progress_without_ack(delivery, readiness, sampler).await;
        }
    }

    let mut result = match decoded {
        Ok(input) => processor.process(&input).await,
        Err(failure) => decode_failure_result(failure.kind, failure.evidence),
    };
    if result.action == ProcessingAction::Retry
        && delivery.delivery_count >= ENGINE_MAX_DELIVERIES as u64
    {
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
        };
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
            metrics.processing_failure();
            readiness.set_persistence_healthy(false);
            sampled_store_failure(sampler, "engine_persist_failure", error);
            return progress_without_ack(delivery, readiness, sampler).await;
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
        },
        InputFailureKind::Malformed => ProcessResult {
            classification: EngineClassification::MalformedInternalEvent,
            detail_class: "malformed_engine_input",
            candidate_count: 0,
            decision_count: 0,
            evidence,
            evaluations: Vec::new(),
            action: ProcessingAction::Retry,
        },
    }
}

fn record_result_metrics(metrics: &RuntimeMetrics, result: &ProcessResult) {
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
        EngineClassification::MalformedInternalEvent
        | EngineClassification::UnsupportedSchema
        | EngineClassification::TransientDependencyFailure
        | EngineClassification::TerminalIntegrityFailure => metrics.processing_failure(),
    }
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
    use crate::persistence::ClassificationRecord;
    use crate::shadow_processor::{RouteRegistry, UnavailableEvaluator};
    use async_trait::async_trait;
    use phoenix_recorder::model::{
        decode_message, engine_event_identity, ARBITRUM_ONE_CHAIN_ID, ENGINE_INPUT_SCHEMA_VERSION,
        NORMALIZED_SCHEMA_VERSION,
    };
    use serde_json::json;
    use std::sync::Mutex;

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
            _source_event_identity: &str,
        ) -> Result<Option<EngineClassification>, StoreError> {
            Ok(*self.final_value.lock().unwrap())
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
            persist_error,
            records: Mutex::new(Vec::new()),
        }
    }

    async fn run(delivery: Delivery, store: &FakeStore) -> DeliveryDisposition {
        process_delivery(
            delivery,
            store,
            &processor(),
            &RuntimeReadiness::new(),
            &RuntimeMetrics::default(),
            &LogSampler::new(Duration::ZERO),
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
