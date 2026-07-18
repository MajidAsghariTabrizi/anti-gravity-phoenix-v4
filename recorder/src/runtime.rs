use crate::ingress::{IngressBuffer, IngressError};
use crate::jetstream::{
    Delivery, MessageFetcher, PipelineError, CONSUMER_MAX_BATCH, CONSUMER_MAX_DELIVERIES,
    POISON_REDELIVERY_DELAY,
};
use crate::logging::LogSampler;
use crate::metrics::Metrics;
use crate::model::{decode_message, ValidatedMessage};
use crate::persistence::{EventStore, PersistOutcome};
use crate::state::Readiness;
use futures_util::{stream, StreamExt};
use money_path_classifier::{
    ClassificationResult, ClassifierError, IngressClassification, MoneyPathClassifier,
};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};
use thiserror::Error;
use tokio_util::sync::CancellationToken;

pub const NATS_CLIENT_SUBSCRIPTION_CAPACITY: usize = 1024;
pub const DEFAULT_BATCH_SIZE: usize = 256;
pub const DEFAULT_BATCH_WAIT: Duration = Duration::from_millis(100);
pub const MAX_BATCH_WAIT: Duration = Duration::from_secs(1);
pub const ACK_FAILURE_READINESS_THRESHOLD: u64 = 3;
pub const MAX_CONCURRENT_ACKS: usize = 32;
const CONSUMER_STATE_REFRESH_INTERVAL: Duration = Duration::from_secs(1);

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct BatchConfig {
    pub max_size: usize,
    pub max_wait: Duration,
}

impl Default for BatchConfig {
    fn default() -> Self {
        Self {
            max_size: DEFAULT_BATCH_SIZE,
            max_wait: DEFAULT_BATCH_WAIT,
        }
    }
}

impl BatchConfig {
    pub fn validate(self) -> Result<Self, RuntimeConfigError> {
        if self.max_size == 0 || self.max_size > CONSUMER_MAX_BATCH as usize {
            return Err(RuntimeConfigError::BatchSize);
        }
        if self.max_wait.is_zero() || self.max_wait > MAX_BATCH_WAIT {
            return Err(RuntimeConfigError::BatchWait);
        }
        Ok(self)
    }
}

#[derive(Clone, Copy, Debug)]
pub struct RetryPolicy {
    pub initial: Duration,
    pub maximum: Duration,
}

impl Default for RetryPolicy {
    fn default() -> Self {
        Self {
            initial: Duration::from_secs(1),
            maximum: Duration::from_secs(30),
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ConsumerExit {
    Shutdown,
    FetchFailed,
    IntegrityFailure,
}

#[derive(Clone, Debug, Error, PartialEq, Eq)]
pub enum RuntimeConfigError {
    #[error("RECORDER_BATCH_MAX_SIZE must be between 1 and 256")]
    BatchSize,
    #[error("RECORDER_BATCH_MAX_WAIT_MS must be between 1 and 1000")]
    BatchWait,
}

pub trait PrePersistenceClassifier: Send + Sync {
    fn classify(&self, message: &ValidatedMessage)
        -> Result<ClassificationResult, ClassifierError>;
}

impl PrePersistenceClassifier for MoneyPathClassifier {
    fn classify(
        &self,
        message: &ValidatedMessage,
    ) -> Result<ClassificationResult, ClassifierError> {
        MoneyPathClassifier::classify(
            self,
            message.tx.chain_id,
            (!message.tx.to.is_empty()).then_some(message.tx.to.as_str()),
            &message.tx.calldata,
        )
    }
}

#[derive(Default)]
struct AckHealthTracker {
    consecutive_failures: u64,
}

impl AckHealthTracker {
    fn observe(
        &mut self,
        result: Result<(), PipelineError>,
        readiness: &Readiness,
        metrics: &Metrics,
    ) -> bool {
        match result {
            Ok(()) => {
                self.consecutive_failures = 0;
                readiness.set_acknowledgements_healthy(true);
                true
            }
            Err(_) => {
                metrics.jetstream_ack_failure();
                self.consecutive_failures = self.consecutive_failures.saturating_add(1);
                if self.consecutive_failures >= ACK_FAILURE_READINESS_THRESHOLD {
                    readiness.set_acknowledgements_healthy(false);
                }
                false
            }
        }
    }
}

#[allow(clippy::too_many_arguments)]
pub async fn consume_durable_messages(
    fetcher: Arc<dyn MessageFetcher>,
    store: Arc<dyn EventStore>,
    classifier: Arc<dyn PrePersistenceClassifier>,
    ingress: IngressBuffer,
    readiness: Readiness,
    metrics: Metrics,
    sampler: LogSampler,
    shutdown: CancellationToken,
    batch_config: BatchConfig,
    retry_policy: RetryPolicy,
) -> ConsumerExit {
    let mut ack_health = AckHealthTracker::default();
    let mut last_state_refresh = Instant::now()
        .checked_sub(CONSUMER_STATE_REFRESH_INTERVAL)
        .unwrap_or_else(Instant::now);

    loop {
        let deliveries = tokio::select! {
            _ = shutdown.cancelled() => return ConsumerExit::Shutdown,
            result = fetcher.fetch_batch(batch_config.max_size, batch_config.max_wait) => result,
        };
        let deliveries = match deliveries {
            Ok(deliveries) => {
                readiness.set_fetching_active(true);
                deliveries
            }
            Err(error) => {
                metrics.jetstream_fetch_failure();
                readiness.set_fetching_active(false);
                readiness.set_consumer_ready(false);
                if let Some(suppressed) = sampler.sample("jetstream_fetch_failure") {
                    tracing::warn!(
                        event = "recorder_jetstream_fetch_failed",
                        error_class = %error,
                        suppressed
                    );
                }
                return ConsumerExit::FetchFailed;
            }
        };

        if last_state_refresh.elapsed() >= CONSUMER_STATE_REFRESH_INTERVAL {
            match fetcher.state().await {
                Ok(state) => {
                    metrics.set_consumer_state(state);
                    readiness.set_consumer_ready(true);
                }
                Err(error) => {
                    metrics.jetstream_fetch_failure();
                    readiness.set_consumer_ready(false);
                    readiness.set_fetching_active(false);
                    if let Some(suppressed) = sampler.sample("consumer_state_failure") {
                        tracing::warn!(
                            event = "recorder_jetstream_consumer_state_failed",
                            error_class = %error,
                            suppressed
                        );
                    }
                    return ConsumerExit::FetchFailed;
                }
            }
            last_state_refresh = Instant::now();
        }

        if deliveries.is_empty() {
            continue;
        }
        match process_delivery_batch(
            deliveries,
            store.as_ref(),
            classifier.as_ref(),
            &ingress,
            &readiness,
            &metrics,
            &sampler,
            &shutdown,
            retry_policy,
            &mut ack_health,
        )
        .await
        {
            BatchDisposition::Continue => {}
            BatchDisposition::Shutdown => return ConsumerExit::Shutdown,
            BatchDisposition::IntegrityFailure => return ConsumerExit::IntegrityFailure,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum BatchDisposition {
    Continue,
    Shutdown,
    IntegrityFailure,
}

#[allow(clippy::too_many_arguments)]
async fn process_delivery_batch(
    deliveries: Vec<Delivery>,
    store: &dyn EventStore,
    classifier: &dyn PrePersistenceClassifier,
    ingress: &IngressBuffer,
    readiness: &Readiness,
    metrics: &Metrics,
    sampler: &LogSampler,
    shutdown: &CancellationToken,
    retry_policy: RetryPolicy,
    ack_health: &mut AckHealthTracker,
) -> BatchDisposition {
    if shutdown.is_cancelled() {
        return BatchDisposition::Shutdown;
    }

    let mut relevant_deliveries = Vec::with_capacity(deliveries.len());
    let mut relevant_messages = Vec::with_capacity(deliveries.len());
    let mut filtered_deliveries = Vec::with_capacity(deliveries.len());
    for delivery in deliveries {
        metrics.message_received();
        if delivery.delivery_count > 1 {
            metrics.jetstream_redelivery();
        }
        match decode_message(&delivery.payload) {
            Ok(message) => {
                let classification = match classifier.classify(&message) {
                    Ok(classification) => classification,
                    Err(error) => {
                        readiness.mark_integrity_loss();
                        tracing::error!(
                            event = "recorder_money_path_classifier_failed",
                            error_class = %error,
                            delivery_count = delivery.delivery_count
                        );
                        return BatchDisposition::IntegrityFailure;
                    }
                };
                metrics.classified(classification.classification);
                match ingress.record(&classification, message.seen_at) {
                    Ok(outcome) => {
                        if outcome.sample_limit_reached {
                            metrics.sample_limit_reached(1);
                        }
                    }
                    Err(IngressError::SampleOversized) => {
                        metrics.bounded_sample_failure();
                        if let Some(suppressed) = sampler.sample("bounded_sample_failure") {
                            tracing::warn!(
                                event = "recorder_bounded_sample_rejected",
                                error_class = "sample_oversized",
                                suppressed
                            );
                        }
                    }
                    Err(error) => {
                        readiness.mark_integrity_loss();
                        tracing::error!(
                            event = "recorder_ingress_evidence_invariant_failed",
                            error_class = %error
                        );
                        return BatchDisposition::IntegrityFailure;
                    }
                }
                if classification.classification == IngressClassification::RelevantRouteInput {
                    relevant_deliveries.push(delivery);
                    relevant_messages.push(message);
                } else {
                    filtered_deliveries.push(delivery);
                }
            }
            Err(error) => {
                metrics.decode_failure();
                if let Some(suppressed) = sampler.sample("decode_failure") {
                    tracing::warn!(
                        event = "recorder_malformed_payload",
                        error_class = %error,
                        delivery_count = delivery.delivery_count,
                        suppressed
                    );
                }
                if delivery.delivery_count >= CONSUMER_MAX_DELIVERIES as u64 {
                    let acknowledged =
                        ack_health.observe(delivery.acker.term().await, readiness, metrics);
                    metrics.poison_message();
                    readiness.mark_integrity_loss();
                    if let Some(suppressed) = sampler.sample("terminal_poison_message") {
                        tracing::error!(
                            event = "recorder_poison_message_terminated",
                            delivery_count = delivery.delivery_count,
                            term_sent = acknowledged,
                            suppressed
                        );
                    }
                } else {
                    let acknowledged = ack_health.observe(
                        delivery.acker.nak(POISON_REDELIVERY_DELAY).await,
                        readiness,
                        metrics,
                    );
                    if !acknowledged {
                        sampled_ack_failure(sampler, "poison_nak_failure");
                    }
                }
            }
        }
    }

    acknowledge_filtered(
        &filtered_deliveries,
        readiness,
        metrics,
        sampler,
        ack_health,
    )
    .await;

    if relevant_messages.is_empty() {
        return BatchDisposition::Continue;
    }

    let outcomes = match persist_batch_with_retry(
        store,
        &relevant_messages,
        &relevant_deliveries,
        readiness,
        metrics,
        sampler,
        shutdown,
        retry_policy,
        ack_health,
    )
    .await
    {
        Some(outcomes) => outcomes,
        None => return BatchDisposition::Shutdown,
    };

    if outcomes.len() != relevant_messages.len() {
        readiness.mark_integrity_loss();
        tracing::error!(
            event = "recorder_batch_outcome_cardinality_mismatch",
            messages = relevant_messages.len(),
            outcomes = outcomes.len()
        );
        return BatchDisposition::IntegrityFailure;
    }

    record_persist_outcomes(&relevant_messages, &outcomes, metrics);
    let ack_results = stream::iter(relevant_deliveries.iter().map(|delivery| {
        let acker = delivery.acker.clone();
        async move { acker.ack_confirmed().await }
    }))
    .buffer_unordered(MAX_CONCURRENT_ACKS)
    .collect::<Vec<_>>()
    .await;
    for result in ack_results {
        let acknowledged = ack_health.observe(result, readiness, metrics);
        if !acknowledged {
            sampled_ack_failure(sampler, "confirmed_ack_failure");
        }
    }

    if shutdown.is_cancelled() {
        BatchDisposition::Shutdown
    } else {
        BatchDisposition::Continue
    }
}

#[allow(clippy::too_many_arguments)]
async fn persist_batch_with_retry(
    store: &dyn EventStore,
    messages: &[ValidatedMessage],
    deliveries: &[Delivery],
    readiness: &Readiness,
    metrics: &Metrics,
    sampler: &LogSampler,
    shutdown: &CancellationToken,
    retry_policy: RetryPolicy,
    ack_health: &mut AckHealthTracker,
) -> Option<Vec<PersistOutcome>> {
    let mut delay = retry_policy.initial;
    let mut failed_attempts = 0_u64;
    loop {
        let started = Instant::now();
        match store.persist_batch(messages).await {
            Ok(outcomes) => {
                readiness.set_postgres_connected(true);
                readiness.set_persistence_healthy(true);
                if failed_attempts > 0 {
                    metrics.database_retry_recovered();
                }
                metrics.batch_persisted(messages.len(), started.elapsed());
                if let Some(suppressed) = sampler.sample("batch_persisted") {
                    tracing::info!(
                        event = "recorder_batch_persisted",
                        messages = messages.len(),
                        suppressed
                    );
                }
                return Some(outcomes);
            }
            Err(error) => {
                failed_attempts = failed_attempts.saturating_add(1);
                metrics.database_failure();
                metrics.database_retry();
                metrics.relevant_transaction_failure();
                readiness.set_postgres_connected(false);
                readiness.set_persistence_healthy(false);
                if let Some(suppressed) = sampler.sample("database_failure") {
                    tracing::error!(
                        event = "recorder_database_failure",
                        error_class = %error,
                        batch_messages = messages.len(),
                        suppressed,
                        retry_delay_ms = delay.as_millis() as u64
                    );
                }
                for delivery in deliveries {
                    let progressed =
                        ack_health.observe(delivery.acker.progress().await, readiness, metrics);
                    if !progressed {
                        sampled_ack_failure(sampler, "progress_ack_failure");
                    }
                }
            }
        }

        tokio::select! {
            _ = shutdown.cancelled() => return None,
            _ = tokio::time::sleep(delay) => {}
        }
        delay = delay.saturating_mul(2).min(retry_policy.maximum);
    }
}

fn record_persist_outcomes(
    messages: &[ValidatedMessage],
    outcomes: &[PersistOutcome],
    metrics: &Metrics,
) {
    for (message, outcome) in messages.iter().zip(outcomes) {
        metrics.relevant_transaction_committed();
        if outcome.feed_event_inserted
            || outcome.origin_transaction_inserted
            || outcome.engine_outbox_inserted
        {
            metrics.message_persisted();
            metrics.set_last_persisted(message.tx.sequence, message.tx.timestamp_unix_ms);
        }
        if outcome.origin_transaction_inserted {
            metrics.transaction_persisted();
        }
        if outcome.engine_outbox_inserted {
            metrics.engine_outbox_inserted();
        }
        if !outcome.feed_event_inserted
            || !outcome.origin_transaction_inserted
            || !outcome.engine_outbox_inserted
        {
            metrics.duplicate_skip();
        }
    }
}

async fn acknowledge_filtered(
    deliveries: &[Delivery],
    readiness: &Readiness,
    metrics: &Metrics,
    sampler: &LogSampler,
    ack_health: &mut AckHealthTracker,
) {
    let ack_results = stream::iter(deliveries.iter().map(|delivery| {
        let acker = delivery.acker.clone();
        async move { acker.ack_confirmed().await }
    }))
    .buffer_unordered(MAX_CONCURRENT_ACKS)
    .collect::<Vec<_>>()
    .await;
    for result in ack_results {
        if !ack_health.observe(result, readiness, metrics) {
            sampled_ack_failure(sampler, "filtered_confirmed_ack_failure");
        }
    }
}

pub async fn flush_ingress_evidence(
    store: Arc<dyn EventStore>,
    ingress: IngressBuffer,
    metrics: Metrics,
    sampler: LogSampler,
    shutdown: CancellationToken,
) {
    let interval = ingress.config().flush_interval;
    let mut ticker = tokio::time::interval(interval);
    ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    ticker.tick().await;
    loop {
        tokio::select! {
            _ = shutdown.cancelled() => {
                let _ = flush_ingress_once(store.as_ref(), &ingress, &metrics, &sampler).await;
                return;
            }
            _ = ticker.tick() => {}
            _ = ingress.wait_for_flush_request() => {}
        }
        let _ = flush_ingress_once(store.as_ref(), &ingress, &metrics, &sampler).await;
    }
}

async fn flush_ingress_once(
    store: &dyn EventStore,
    ingress: &IngressBuffer,
    metrics: &Metrics,
    sampler: &LogSampler,
) -> Result<(), ()> {
    let batch = match ingress.take() {
        Ok(batch) if !batch.is_empty() => batch,
        Ok(_) => return Ok(()),
        Err(_) => {
            metrics.aggregate_flush_failure();
            return Err(());
        }
    };
    match store
        .persist_ingress_evidence(&batch, ingress.config().max_samples_per_detail_per_day)
        .await
    {
        Ok(outcome) => {
            metrics.aggregate_flush();
            metrics.bounded_samples(outcome.samples_inserted);
            metrics.sample_limit_reached(outcome.sample_limit_reached);
            Ok(())
        }
        Err(error) => {
            metrics.aggregate_flush_failure();
            if ingress.restore(batch).is_err() {
                metrics.bounded_sample_failure();
            }
            if let Some(suppressed) = sampler.sample("ingress_evidence_flush_failure") {
                tracing::warn!(
                    event = "recorder_ingress_evidence_flush_failed",
                    error_class = %error,
                    suppressed
                );
            }
            Err(())
        }
    }
}

fn sampled_ack_failure(sampler: &LogSampler, class: &'static str) {
    if let Some(suppressed) = sampler.sample(class) {
        tracing::warn!(
            event = "recorder_jetstream_ack_failed",
            ack_class = class,
            suppressed
        );
    }
}

pub async fn monitor_database(
    store: Arc<dyn EventStore>,
    readiness: Readiness,
    metrics: Metrics,
    sampler: LogSampler,
    shutdown: CancellationToken,
    interval: Duration,
) {
    let mut ticker = tokio::time::interval(interval);
    ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    let mut database_unhealthy = false;
    loop {
        tokio::select! {
            _ = shutdown.cancelled() => return,
            _ = ticker.tick() => {
                match store.ping().await {
                    Ok(()) => {
                        readiness.set_postgres_connected(true);
                        match store.verify_schema().await {
                            Ok(()) => {
                                readiness.set_schema_verified(true);
                                if database_unhealthy {
                                    metrics.database_retry_recovered();
                                    database_unhealthy = false;
                                }
                            }
                            Err(error) => {
                                database_unhealthy = true;
                                metrics.database_failure();
                                metrics.database_retry();
                                readiness.set_schema_verified(false);
                                if let Some(suppressed) = sampler.sample("schema_verification_failure") {
                                    tracing::error!(
                                        event = "recorder_schema_verification_failed",
                                        error_class = %error,
                                        suppressed
                                    );
                                }
                            }
                        }
                    }
                    Err(error) => {
                        database_unhealthy = true;
                        metrics.database_failure();
                        metrics.database_retry();
                        readiness.set_postgres_connected(false);
                        if let Some(suppressed) = sampler.sample("postgres_unavailable") {
                            tracing::warn!(
                                event = "recorder_postgres_unavailable",
                                error_class = %error,
                                suppressed
                            );
                        }
                    }
                }
            }
        }
    }
}

pub fn nats_connect_options(
    readiness: Readiness,
    metrics: Metrics,
    sampler: LogSampler,
    disconnected_since_last_connect: Arc<AtomicBool>,
) -> async_nats::ConnectOptions {
    async_nats::ConnectOptions::new()
        .name("phoenix-recorder")
        .subscription_capacity(NATS_CLIENT_SUBSCRIPTION_CAPACITY)
        .connection_timeout(Duration::from_secs(5))
        .event_callback(move |event| {
            let readiness = readiness.clone();
            let metrics = metrics.clone();
            let sampler = sampler.clone();
            let disconnected = disconnected_since_last_connect.clone();
            async move {
                match event {
                    async_nats::Event::Connected => {
                        readiness.set_jetstream_connected(true);
                        if disconnected.swap(false, Ordering::AcqRel) {
                            metrics.nats_reconnect();
                            tracing::info!(event = "recorder_jetstream_reconnected");
                        }
                    }
                    async_nats::Event::Disconnected => {
                        disconnected.store(true, Ordering::Release);
                        readiness.set_jetstream_connected(false);
                        tracing::warn!(event = "recorder_jetstream_disconnected");
                    }
                    async_nats::Event::SlowConsumer(subscription_id) => {
                        metrics.jetstream_fetch_failure();
                        readiness.set_fetching_active(false);
                        if let Some(suppressed) = sampler.sample("nats_slow_consumer") {
                            tracing::warn!(
                                event = "recorder_jetstream_client_slow_consumer",
                                subscription_id,
                                suppressed,
                                delivery = "jetstream_redeliverable"
                            );
                        }
                    }
                    async_nats::Event::LameDuckMode
                    | async_nats::Event::ServerError(_)
                    | async_nats::Event::ClientError(_) => {
                        if let Some(suppressed) = sampler.sample("nats_lifecycle_warning") {
                            tracing::warn!(
                                event = "recorder_jetstream_lifecycle_warning",
                                suppressed
                            );
                        }
                    }
                }
            }
        })
}

pub fn mark_nats_connected(
    readiness: &Readiness,
    metrics: &Metrics,
    disconnected_since_last_connect: &AtomicBool,
) {
    readiness.set_jetstream_connected(true);
    if disconnected_since_last_connect.swap(false, Ordering::AcqRel) {
        metrics.nats_reconnect();
        tracing::info!(event = "recorder_jetstream_reconnected");
    } else {
        tracing::info!(event = "recorder_jetstream_connected");
    }
}

pub fn mark_nats_disconnected(readiness: &Readiness, disconnected_since_last_connect: &AtomicBool) {
    disconnected_since_last_connect.store(true, Ordering::Release);
    readiness.set_jetstream_connected(false);
    readiness.set_stream_ready(false);
    readiness.set_consumer_ready(false);
    readiness.set_fetching_active(false);
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ingress::IngressBufferConfig;
    use crate::jetstream::{ConsumerState, DeliveryAcker};
    use crate::model::{ARBITRUM_ONE_CHAIN_ID, NORMALIZED_SCHEMA_VERSION};
    use crate::persistence::StoreError;
    use async_trait::async_trait;
    use money_path_classifier::{
        DecodedSwapKind, OuterSelectorKind, SafeDecoderSummary, UnsupportedReason, WrapperKind,
    };
    use std::collections::VecDeque;
    use std::sync::atomic::AtomicUsize;
    use std::sync::Mutex;
    use tokio::sync::Notify;

    #[derive(Clone, Debug, Default)]
    struct PersistGate {
        started: Arc<Notify>,
        release: Arc<Notify>,
    }

    #[derive(Debug)]
    struct FakeStore {
        outcomes: Mutex<VecDeque<Result<Vec<PersistOutcome>, StoreError>>>,
        calls: AtomicUsize,
        batch_sizes: Mutex<Vec<usize>>,
        delay: Duration,
        persist_gate: Option<PersistGate>,
    }

    #[derive(Clone, Debug)]
    struct FixedClassifier {
        result: Result<ClassificationResult, ClassifierError>,
    }

    impl PrePersistenceClassifier for FixedClassifier {
        fn classify(
            &self,
            _message: &ValidatedMessage,
        ) -> Result<ClassificationResult, ClassifierError> {
            self.result.clone()
        }
    }

    fn fixed_classifier(classification: IngressClassification) -> FixedClassifier {
        FixedClassifier {
            result: Ok(ClassificationResult {
                classification,
                detail_class: match classification {
                    IngressClassification::Irrelevant => "irrelevant_origin",
                    IngressClassification::UnsupportedInteresting => {
                        "known_router_unsupported_command"
                    }
                    IngressClassification::RelevantRouteInput => "reviewed_route_touched",
                },
                summary: SafeDecoderSummary {
                    router_kind: None,
                    outer_selector_kind: OuterSelectorKind::Unknown,
                    wrapper_kind: WrapperKind::None,
                    decoded_swap_kind: DecodedSwapKind::None,
                    unsupported_reason: UnsupportedReason::None,
                    command_count: 0,
                    v3_hop_count: 0,
                    reviewed_pool_matches: usize::from(
                        classification == IngressClassification::RelevantRouteInput,
                    ),
                },
            }),
        }
    }

    fn relevant_classifier() -> FixedClassifier {
        fixed_classifier(IngressClassification::RelevantRouteInput)
    }

    fn ingress_buffer() -> IngressBuffer {
        IngressBuffer::new(IngressBufferConfig::default()).unwrap()
    }

    impl FakeStore {
        fn new(outcomes: Vec<Result<Vec<PersistOutcome>, StoreError>>) -> Self {
            Self {
                outcomes: Mutex::new(outcomes.into()),
                calls: AtomicUsize::new(0),
                batch_sizes: Mutex::new(Vec::new()),
                delay: Duration::from_millis(1),
                persist_gate: None,
            }
        }
    }

    #[async_trait]
    impl EventStore for FakeStore {
        async fn ping(&self) -> Result<(), StoreError> {
            Ok(())
        }

        async fn verify_schema(&self) -> Result<(), StoreError> {
            Ok(())
        }

        async fn persist_batch(
            &self,
            messages: &[ValidatedMessage],
        ) -> Result<Vec<PersistOutcome>, StoreError> {
            self.calls.fetch_add(1, Ordering::Relaxed);
            self.batch_sizes.lock().unwrap().push(messages.len());
            if let Some(gate) = &self.persist_gate {
                gate.started.notify_one();
                gate.release.notified().await;
            } else {
                tokio::time::sleep(self.delay).await;
            }
            self.outcomes
                .lock()
                .unwrap()
                .pop_front()
                .unwrap_or_else(|| {
                    Ok(vec![
                        PersistOutcome {
                            feed_event_inserted: true,
                            origin_transaction_inserted: true,
                            engine_outbox_inserted: true,
                        };
                        messages.len()
                    ])
                })
        }
    }

    #[derive(Debug, Default)]
    struct FakeAcker {
        ack: AtomicUsize,
        nak: AtomicUsize,
        progress: AtomicUsize,
        term: AtomicUsize,
        ack_failures: AtomicUsize,
    }

    impl FakeAcker {
        fn fail_acks(count: usize) -> Self {
            Self {
                ack_failures: AtomicUsize::new(count),
                ..Default::default()
            }
        }

        fn maybe_fail(&self) -> Result<(), PipelineError> {
            self.ack_failures.fetch_sub(1, Ordering::SeqCst);
            Err(PipelineError::Acknowledgement)
        }
    }

    #[async_trait]
    impl DeliveryAcker for FakeAcker {
        async fn ack_confirmed(&self) -> Result<(), PipelineError> {
            self.ack.fetch_add(1, Ordering::Relaxed);
            if self.ack_failures.load(Ordering::Relaxed) > 0 {
                self.maybe_fail()
            } else {
                Ok(())
            }
        }

        async fn nak(&self, _delay: Duration) -> Result<(), PipelineError> {
            self.nak.fetch_add(1, Ordering::Relaxed);
            Ok(())
        }

        async fn progress(&self) -> Result<(), PipelineError> {
            self.progress.fetch_add(1, Ordering::Relaxed);
            Ok(())
        }

        async fn term(&self) -> Result<(), PipelineError> {
            self.term.fetch_add(1, Ordering::Relaxed);
            Ok(())
        }
    }

    #[derive(Debug)]
    struct FakeFetcher {
        batches: Mutex<VecDeque<Result<Vec<Delivery>, PipelineError>>>,
        state: Mutex<Result<ConsumerState, PipelineError>>,
        requests: Mutex<Vec<(usize, Duration)>>,
    }

    #[async_trait]
    impl MessageFetcher for FakeFetcher {
        async fn fetch_batch(
            &self,
            max_messages: usize,
            max_wait: Duration,
        ) -> Result<Vec<Delivery>, PipelineError> {
            self.requests.lock().unwrap().push((max_messages, max_wait));
            self.batches
                .lock()
                .unwrap()
                .pop_front()
                .unwrap_or(Err(PipelineError::Fetch))
        }

        async fn state(&self) -> Result<ConsumerState, PipelineError> {
            self.state.lock().unwrap().clone()
        }
    }

    fn payload(sequence: u64, hash_byte: char) -> Vec<u8> {
        serde_json::to_vec(&serde_json::json!({
            "schema_version": NORMALIZED_SCHEMA_VERSION,
            "sequence": sequence,
            "timestamp_unix_ms": 1_700_000_000_000_u64,
            "tx_hash": format!("0x{}", hash_byte.to_string().repeat(64)),
            "tx_type": "0x02",
            "chain_id": ARBITRUM_ONE_CHAIN_ID,
            "from": "0x1111111111111111111111111111111111111111",
            "to": "0x2222222222222222222222222222222222222222",
            "nonce": 1,
            "value": "0",
            "calldata": "0x1234",
            "gas_limit": "21000",
            "max_fee_per_gas": "100",
            "max_priority_fee_per_gas": "1",
            "raw_tx": "AQID",
            "ingested_at_unix_ns": 1_700_000_000_123_456_789_i64
        }))
        .unwrap()
    }

    fn delivery(
        sequence: u64,
        hash_byte: char,
        delivery_count: u64,
        acker: Arc<FakeAcker>,
    ) -> Delivery {
        Delivery {
            payload: payload(sequence, hash_byte),
            delivery_count,
            acker,
        }
    }

    fn malformed(delivery_count: u64, acker: Arc<FakeAcker>) -> Delivery {
        Delivery {
            payload: b"not-json".to_vec(),
            delivery_count,
            acker,
        }
    }

    fn ready_state() -> Readiness {
        let readiness = Readiness::new();
        readiness.set_postgres_connected(true);
        readiness.set_schema_verified(true);
        readiness.set_jetstream_connected(true);
        readiness.set_stream_ready(true);
        readiness.set_consumer_ready(true);
        readiness.set_fetching_active(true);
        readiness
    }

    #[tokio::test]
    async fn successful_batch_persists_once_then_confirms_each_ack() {
        let store = FakeStore::new(vec![]);
        let first_ack = Arc::new(FakeAcker::default());
        let second_ack = Arc::new(FakeAcker::default());
        let metrics = Metrics::default();
        let mut ack_health = AckHealthTracker::default();
        let result = process_delivery_batch(
            vec![
                delivery(1, 'a', 1, first_ack.clone()),
                delivery(2, 'b', 1, second_ack.clone()),
            ],
            &store,
            &relevant_classifier(),
            &ingress_buffer(),
            &ready_state(),
            &metrics,
            &LogSampler::default(),
            &CancellationToken::new(),
            RetryPolicy::default(),
            &mut ack_health,
        )
        .await;
        assert_eq!(result, BatchDisposition::Continue);
        assert_eq!(store.calls.load(Ordering::Relaxed), 1);
        assert_eq!(&*store.batch_sizes.lock().unwrap(), &[2]);
        assert_eq!(first_ack.ack.load(Ordering::Relaxed), 1);
        assert_eq!(second_ack.ack.load(Ordering::Relaxed), 1);
        let rendered = metrics.render(&Readiness::new());
        assert!(rendered.contains("recorder_batches_persisted_total 1"));
        assert!(rendered.contains("recorder_messages_persisted_total 2"));
    }

    #[tokio::test]
    async fn filtered_classifications_ack_without_raw_persistence() {
        let store = FakeStore::new(vec![]);
        let irrelevant_ack = Arc::new(FakeAcker::default());
        let unsupported_ack = Arc::new(FakeAcker::default());
        let metrics = Metrics::default();
        let readiness = ready_state();
        let mut ack_health = AckHealthTracker::default();

        let irrelevant = process_delivery_batch(
            vec![delivery(1, 'a', 1, irrelevant_ack.clone())],
            &store,
            &fixed_classifier(IngressClassification::Irrelevant),
            &ingress_buffer(),
            &readiness,
            &metrics,
            &LogSampler::default(),
            &CancellationToken::new(),
            RetryPolicy::default(),
            &mut ack_health,
        )
        .await;
        let unsupported = process_delivery_batch(
            vec![delivery(2, 'b', 1, unsupported_ack.clone())],
            &store,
            &fixed_classifier(IngressClassification::UnsupportedInteresting),
            &ingress_buffer(),
            &readiness,
            &metrics,
            &LogSampler::default(),
            &CancellationToken::new(),
            RetryPolicy::default(),
            &mut ack_health,
        )
        .await;

        assert_eq!(irrelevant, BatchDisposition::Continue);
        assert_eq!(unsupported, BatchDisposition::Continue);
        assert_eq!(store.calls.load(Ordering::Relaxed), 0);
        assert_eq!(irrelevant_ack.ack.load(Ordering::Relaxed), 1);
        assert_eq!(unsupported_ack.ack.load(Ordering::Relaxed), 1);
        let rendered = metrics.render(&readiness);
        assert!(rendered.contains("recorder_irrelevant_filtered_total 1"));
        assert!(rendered.contains("recorder_unsupported_interesting_total 1"));
        assert!(rendered.contains("recorder_raw_rows_avoided_total 6"));
        assert!(rendered.contains("recorder_relevant_route_inputs_total 0"));
    }

    #[tokio::test]
    async fn internal_classifier_failure_never_persists_or_acknowledges() {
        let store = FakeStore::new(vec![]);
        let acker = Arc::new(FakeAcker::default());
        let classifier = FixedClassifier {
            result: Err(ClassifierError::Invariant),
        };
        let readiness = ready_state();
        let mut ack_health = AckHealthTracker::default();
        let result = process_delivery_batch(
            vec![delivery(1, 'a', 1, acker.clone())],
            &store,
            &classifier,
            &ingress_buffer(),
            &readiness,
            &Metrics::default(),
            &LogSampler::default(),
            &CancellationToken::new(),
            RetryPolicy::default(),
            &mut ack_health,
        )
        .await;

        assert_eq!(result, BatchDisposition::IntegrityFailure);
        assert_eq!(store.calls.load(Ordering::Relaxed), 0);
        assert_eq!(acker.ack.load(Ordering::Relaxed), 0);
        assert_eq!(acker.nak.load(Ordering::Relaxed), 0);
        assert_eq!(
            readiness.ready(),
            Err("terminal Recorder integrity condition detected")
        );
    }

    #[tokio::test]
    async fn postgres_failure_makes_progress_without_ack_then_recovers() {
        let inserted = PersistOutcome {
            feed_event_inserted: true,
            origin_transaction_inserted: true,
            engine_outbox_inserted: true,
        };
        let store = FakeStore::new(vec![Err(StoreError::Connection), Ok(vec![inserted])]);
        let acker = Arc::new(FakeAcker::default());
        let readiness = ready_state();
        let metrics = Metrics::default();
        let mut ack_health = AckHealthTracker::default();
        let result = process_delivery_batch(
            vec![delivery(1, 'a', 1, acker.clone())],
            &store,
            &relevant_classifier(),
            &ingress_buffer(),
            &readiness,
            &metrics,
            &LogSampler::default(),
            &CancellationToken::new(),
            RetryPolicy {
                initial: Duration::from_millis(1),
                maximum: Duration::from_millis(2),
            },
            &mut ack_health,
        )
        .await;
        assert_eq!(result, BatchDisposition::Continue);
        assert_eq!(store.calls.load(Ordering::Relaxed), 2);
        assert_eq!(acker.progress.load(Ordering::Relaxed), 1);
        assert_eq!(acker.ack.load(Ordering::Relaxed), 1);
        let rendered = metrics.render(&readiness);
        assert!(rendered.contains("recorder_database_retries_total 1"));
        assert!(rendered.contains("recorder_database_retry_recoveries_total 1"));
    }

    #[tokio::test]
    async fn duplicate_restart_replay_is_committed_idempotently_before_ack() {
        let store = FakeStore::new(vec![
            Ok(vec![PersistOutcome {
                feed_event_inserted: true,
                origin_transaction_inserted: true,
                engine_outbox_inserted: true,
            }]),
            Ok(vec![PersistOutcome::default()]),
        ]);
        let failed_ack = Arc::new(FakeAcker::fail_acks(1));
        let replay_ack = Arc::new(FakeAcker::default());
        let readiness = ready_state();
        let metrics = Metrics::default();
        let mut ack_health = AckHealthTracker::default();

        process_delivery_batch(
            vec![delivery(7, 'a', 1, failed_ack)],
            &store,
            &relevant_classifier(),
            &ingress_buffer(),
            &readiness,
            &metrics,
            &LogSampler::default(),
            &CancellationToken::new(),
            RetryPolicy::default(),
            &mut ack_health,
        )
        .await;
        process_delivery_batch(
            vec![delivery(7, 'a', 2, replay_ack.clone())],
            &store,
            &relevant_classifier(),
            &ingress_buffer(),
            &readiness,
            &metrics,
            &LogSampler::default(),
            &CancellationToken::new(),
            RetryPolicy::default(),
            &mut ack_health,
        )
        .await;

        assert_eq!(store.calls.load(Ordering::Relaxed), 2);
        assert_eq!(replay_ack.ack.load(Ordering::Relaxed), 1);
        let rendered = metrics.render(&readiness);
        assert!(rendered.contains("recorder_duplicate_skips_total 1"));
        assert!(rendered.contains("recorder_jetstream_redeliveries_total 1"));
    }

    #[tokio::test]
    async fn poison_policy_is_bounded_and_valid_siblings_continue() {
        let store = FakeStore::new(vec![]);
        let retry_ack = Arc::new(FakeAcker::default());
        let terminal_ack = Arc::new(FakeAcker::default());
        let valid_ack = Arc::new(FakeAcker::default());
        let readiness = ready_state();
        let mut ack_health = AckHealthTracker::default();
        let result = process_delivery_batch(
            vec![
                malformed(1, retry_ack.clone()),
                malformed(CONSUMER_MAX_DELIVERIES as u64, terminal_ack.clone()),
                delivery(9, 'b', 1, valid_ack.clone()),
            ],
            &store,
            &relevant_classifier(),
            &ingress_buffer(),
            &readiness,
            &Metrics::default(),
            &LogSampler::default(),
            &CancellationToken::new(),
            RetryPolicy::default(),
            &mut ack_health,
        )
        .await;
        assert_eq!(result, BatchDisposition::Continue);
        assert_eq!(retry_ack.nak.load(Ordering::Relaxed), 1);
        assert_eq!(terminal_ack.term.load(Ordering::Relaxed), 1);
        assert_eq!(valid_ack.ack.load(Ordering::Relaxed), 1);
        assert_eq!(store.calls.load(Ordering::Relaxed), 1);
        assert_eq!(
            readiness.ready(),
            Err("terminal Recorder integrity condition detected")
        );
    }

    #[tokio::test]
    async fn graceful_shutdown_before_persistence_safely_abandons_batch() {
        let store = FakeStore::new(vec![]);
        let acker = Arc::new(FakeAcker::default());
        let shutdown = CancellationToken::new();
        shutdown.cancel();
        let mut ack_health = AckHealthTracker::default();
        let result = process_delivery_batch(
            vec![delivery(1, 'a', 1, acker.clone())],
            &store,
            &relevant_classifier(),
            &ingress_buffer(),
            &ready_state(),
            &Metrics::default(),
            &LogSampler::default(),
            &shutdown,
            RetryPolicy::default(),
            &mut ack_health,
        )
        .await;
        assert_eq!(result, BatchDisposition::Shutdown);
        assert_eq!(store.calls.load(Ordering::Relaxed), 0);
        assert_eq!(acker.ack.load(Ordering::Relaxed), 0);
    }

    #[tokio::test]
    async fn graceful_shutdown_during_commit_finishes_batch_then_acks() {
        let mut store = FakeStore::new(vec![]);
        let persist_gate = PersistGate::default();
        store.persist_gate = Some(persist_gate.clone());
        let acker = Arc::new(FakeAcker::default());
        let shutdown = CancellationToken::new();
        let cancel = shutdown.clone();
        let cancel_task = tokio::spawn(async move {
            persist_gate.started.notified().await;
            cancel.cancel();
            persist_gate.release.notify_one();
        });
        let mut ack_health = AckHealthTracker::default();
        let result = process_delivery_batch(
            vec![delivery(1, 'a', 1, acker.clone())],
            &store,
            &relevant_classifier(),
            &ingress_buffer(),
            &ready_state(),
            &Metrics::default(),
            &LogSampler::default(),
            &shutdown,
            RetryPolicy::default(),
            &mut ack_health,
        )
        .await;
        cancel_task.await.expect("cancellation task must complete");
        assert_eq!(result, BatchDisposition::Shutdown);
        assert_eq!(store.calls.load(Ordering::Relaxed), 1);
        assert_eq!(acker.ack.load(Ordering::Relaxed), 1);
    }

    #[tokio::test]
    async fn persistent_acknowledgement_failures_clear_readiness() {
        let store = FakeStore::new(vec![]);
        let readiness = ready_state();
        let metrics = Metrics::default();
        let mut ack_health = AckHealthTracker::default();
        let deliveries = vec![
            delivery(1, 'a', 1, Arc::new(FakeAcker::fail_acks(1))),
            delivery(2, 'b', 1, Arc::new(FakeAcker::fail_acks(1))),
            delivery(3, 'c', 1, Arc::new(FakeAcker::fail_acks(1))),
        ];
        let result = process_delivery_batch(
            deliveries,
            &store,
            &relevant_classifier(),
            &ingress_buffer(),
            &readiness,
            &metrics,
            &LogSampler::default(),
            &CancellationToken::new(),
            RetryPolicy::default(),
            &mut ack_health,
        )
        .await;
        assert_eq!(result, BatchDisposition::Continue);
        assert_eq!(
            readiness.ready(),
            Err("JetStream acknowledgement failures persist")
        );
        assert!(metrics
            .render(&readiness)
            .contains("recorder_jetstream_ack_failures_total 3"));
    }

    #[tokio::test]
    async fn bounded_pull_uses_configured_batch_size_wait_and_lag_metrics() {
        let config = BatchConfig::default().validate().unwrap();
        let fetcher = Arc::new(FakeFetcher {
            batches: Mutex::new(VecDeque::from([Ok(Vec::new()), Err(PipelineError::Fetch)])),
            state: Mutex::new(Ok(ConsumerState {
                pending: 44,
                ack_pending: 3,
                redelivered: 0,
            })),
            requests: Mutex::new(Vec::new()),
        });
        let metrics = Metrics::default();
        let exit = consume_durable_messages(
            fetcher.clone(),
            Arc::new(FakeStore::new(vec![])),
            Arc::new(relevant_classifier()),
            ingress_buffer(),
            ready_state(),
            metrics.clone(),
            LogSampler::default(),
            CancellationToken::new(),
            config,
            RetryPolicy::default(),
        )
        .await;
        assert_eq!(exit, ConsumerExit::FetchFailed);
        assert_eq!(
            fetcher.requests.lock().unwrap()[0],
            (256, DEFAULT_BATCH_WAIT)
        );
        let rendered = metrics.render(&Readiness::new());
        assert!(rendered.contains("recorder_consumer_pending_messages 44"));
        assert!(rendered.contains("recorder_consumer_ack_pending 3"));
    }

    #[test]
    fn batch_configuration_rejects_unbounded_values() {
        assert_eq!(
            BatchConfig {
                max_size: 0,
                max_wait: DEFAULT_BATCH_WAIT,
            }
            .validate(),
            Err(RuntimeConfigError::BatchSize)
        );
        assert_eq!(
            BatchConfig {
                max_size: 257,
                max_wait: DEFAULT_BATCH_WAIT,
            }
            .validate(),
            Err(RuntimeConfigError::BatchSize)
        );
        assert_eq!(
            BatchConfig {
                max_size: 1,
                max_wait: Duration::from_secs(2),
            }
            .validate(),
            Err(RuntimeConfigError::BatchWait)
        );
    }

    #[test]
    fn nats_disconnect_and_reconnect_control_readiness_and_metrics() {
        let readiness = ready_state();
        let disconnected = AtomicBool::new(false);
        mark_nats_disconnected(&readiness, &disconnected);
        assert_eq!(readiness.ready(), Err("JetStream disconnected"));
        assert!(disconnected.load(Ordering::Acquire));

        let metrics = Metrics::default();
        mark_nats_connected(&readiness, &metrics, &disconnected);
        readiness.set_stream_ready(true);
        readiness.set_consumer_ready(true);
        readiness.set_fetching_active(true);
        assert_eq!(readiness.ready(), Ok(()));
        assert!(metrics
            .render(&readiness)
            .contains("recorder_nats_reconnects_total 1"));
    }
}
