use crate::logging::LogSampler;
use crate::metrics::Metrics;
use crate::model::{decode_message, ValidatedMessage};
use crate::persistence::{EventStore, PersistOutcome};
use crate::state::Readiness;
use crate::NATS_SUBJECT;
use async_trait::async_trait;
use futures_util::{Stream, StreamExt};
use std::pin::Pin;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::Duration;
use thiserror::Error;
use tokio_util::sync::CancellationToken;

pub const NATS_SUBSCRIPTION_CAPACITY: usize = 256;

pub type MessageStream = Pin<Box<dyn Stream<Item = Vec<u8>> + Send>>;

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
    SubscriptionEnded,
}

#[derive(Clone, Debug, Error, PartialEq, Eq)]
pub enum SubscriptionError {
    #[error("Core NATS subscription failed")]
    Subscribe,
}

#[async_trait]
pub trait CoreSubscriber: Send + Sync {
    async fn subscribe_core(&self, subject: &str) -> Result<MessageStream, SubscriptionError>;
}

#[async_trait]
impl CoreSubscriber for async_nats::Client {
    async fn subscribe_core(&self, subject: &str) -> Result<MessageStream, SubscriptionError> {
        let subscriber = self
            .subscribe(subject.to_string())
            .await
            .map_err(|_| SubscriptionError::Subscribe)?;
        self.flush()
            .await
            .map_err(|_| SubscriptionError::Subscribe)?;
        Ok(Box::pin(subscriber.map(|message| message.payload.to_vec())))
    }
}

pub async fn activate_subscription(
    subscriber: &dyn CoreSubscriber,
    readiness: &Readiness,
) -> Result<MessageStream, SubscriptionError> {
    let stream = subscriber.subscribe_core(NATS_SUBJECT).await?;
    readiness.set_subscription_active(true);
    tracing::info!(
        event = "recorder_subject_subscribed",
        subject = NATS_SUBJECT,
        delivery = "core_nats_at_most_once"
    );
    Ok(stream)
}

pub async fn consume_messages(
    mut stream: MessageStream,
    store: Arc<dyn EventStore>,
    readiness: Readiness,
    metrics: Metrics,
    sampler: LogSampler,
    shutdown: CancellationToken,
    retry_policy: RetryPolicy,
) -> ConsumerExit {
    loop {
        let payload = tokio::select! {
            _ = shutdown.cancelled() => return ConsumerExit::Shutdown,
            payload = stream.next() => payload,
        };
        let Some(payload) = payload else {
            readiness.set_subscription_active(false);
            readiness.set_nats_connected(false);
            return ConsumerExit::SubscriptionEnded;
        };

        metrics.message_received();
        if let Some(suppressed) = sampler.sample("message_received") {
            tracing::info!(event = "recorder_message_received", suppressed);
        }

        let message = match decode_message(&payload) {
            Ok(message) => message,
            Err(error) => {
                metrics.decode_failure();
                if let Some(suppressed) = sampler.sample("decode_failure") {
                    tracing::warn!(
                        event = "recorder_malformed_payload",
                        error_class = %error,
                        suppressed
                    );
                }
                continue;
            }
        };

        if persist_with_retry(
            store.as_ref(),
            &message,
            &readiness,
            &metrics,
            &sampler,
            &shutdown,
            retry_policy,
        )
        .await
        .is_err()
        {
            return ConsumerExit::Shutdown;
        }
    }
}

async fn persist_with_retry(
    store: &dyn EventStore,
    message: &ValidatedMessage,
    readiness: &Readiness,
    metrics: &Metrics,
    sampler: &LogSampler,
    shutdown: &CancellationToken,
    retry_policy: RetryPolicy,
) -> Result<(), ()> {
    let mut delay = retry_policy.initial;
    loop {
        match store.persist(message).await {
            Ok(outcome) => {
                readiness.set_postgres_connected(true);
                readiness.set_persistence_healthy(true);
                record_persist_outcome(message, outcome, metrics, sampler);
                return Ok(());
            }
            Err(error) => {
                metrics.database_failure();
                readiness.set_postgres_connected(false);
                readiness.set_persistence_healthy(false);
                if let Some(suppressed) = sampler.sample("database_failure") {
                    tracing::error!(
                        event = "recorder_database_failure",
                        error_class = %error,
                        sequence = message.tx.sequence,
                        suppressed,
                        retry_delay_ms = delay.as_millis() as u64
                    );
                }
            }
        }

        tokio::select! {
            _ = shutdown.cancelled() => return Err(()),
            _ = tokio::time::sleep(delay) => {}
        }
        delay = delay.saturating_mul(2).min(retry_policy.maximum);
    }
}

fn record_persist_outcome(
    message: &ValidatedMessage,
    outcome: PersistOutcome,
    metrics: &Metrics,
    sampler: &LogSampler,
) {
    if outcome.feed_event_inserted || outcome.origin_transaction_inserted {
        metrics.message_persisted();
        metrics.set_last_persisted(message.tx.sequence, message.tx.timestamp_unix_ms);
    }
    if outcome.origin_transaction_inserted {
        metrics.transaction_persisted();
    }
    if !outcome.feed_event_inserted || !outcome.origin_transaction_inserted {
        metrics.duplicate_skip();
        if let Some(suppressed) = sampler.sample("duplicate_skip") {
            tracing::info!(
                event = "recorder_duplicate_skip",
                sequence = message.tx.sequence,
                tx_hash = %message.tx.tx_hash,
                suppressed
            );
        }
    } else if let Some(suppressed) = sampler.sample("rows_inserted") {
        tracing::info!(
            event = "recorder_rows_inserted",
            sequence = message.tx.sequence,
            tx_hash = %message.tx.tx_hash,
            rows = 2_u8,
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
    loop {
        tokio::select! {
            _ = shutdown.cancelled() => return,
            _ = ticker.tick() => {
                match store.ping().await {
                    Ok(()) => {
                        readiness.set_postgres_connected(true);
                        match store.verify_schema().await {
                            Ok(()) => readiness.set_schema_verified(true),
                            Err(error) => {
                                metrics.database_failure();
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
                        metrics.database_failure();
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
        .subscription_capacity(NATS_SUBSCRIPTION_CAPACITY)
        .connection_timeout(Duration::from_secs(5))
        .event_callback(move |event| {
            let readiness = readiness.clone();
            let metrics = metrics.clone();
            let sampler = sampler.clone();
            let disconnected = disconnected_since_last_connect.clone();
            async move {
                match event {
                    async_nats::Event::Connected => {
                        readiness.set_nats_connected(true);
                        if disconnected.swap(false, Ordering::AcqRel) {
                            metrics.nats_reconnect();
                            tracing::info!(event = "recorder_nats_reconnected");
                        }
                    }
                    async_nats::Event::Disconnected => {
                        disconnected.store(true, Ordering::Release);
                        readiness.set_nats_connected(false);
                        tracing::warn!(event = "recorder_nats_disconnected");
                    }
                    async_nats::Event::SlowConsumer(subscription_id) => {
                        readiness.mark_delivery_loss();
                        if let Some(suppressed) = sampler.sample("nats_slow_consumer") {
                            tracing::error!(
                                event = "recorder_nats_slow_consumer",
                                subscription_id,
                                suppressed,
                                delivery_risk = "core_nats_message_drop"
                            );
                        }
                    }
                    async_nats::Event::LameDuckMode
                    | async_nats::Event::ServerError(_)
                    | async_nats::Event::ClientError(_) => {
                        if let Some(suppressed) = sampler.sample("nats_lifecycle_warning") {
                            tracing::warn!(event = "recorder_nats_lifecycle_warning", suppressed);
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
    readiness.set_nats_connected(true);
    if disconnected_since_last_connect.swap(false, Ordering::AcqRel) {
        metrics.nats_reconnect();
        tracing::info!(event = "recorder_nats_reconnected");
    } else {
        tracing::info!(event = "recorder_nats_connected");
    }
}

pub fn mark_nats_disconnected(readiness: &Readiness, disconnected_since_last_connect: &AtomicBool) {
    disconnected_since_last_connect.store(true, Ordering::Release);
    readiness.set_nats_connected(false);
    readiness.set_subscription_active(false);
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{ARBITRUM_ONE_CHAIN_ID, NORMALIZED_SCHEMA_VERSION};
    use crate::persistence::StoreError;
    use std::collections::VecDeque;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::Mutex;

    #[derive(Debug)]
    struct FakeStore {
        outcomes: Mutex<VecDeque<Result<PersistOutcome, StoreError>>>,
        calls: AtomicUsize,
        in_flight: AtomicUsize,
        max_in_flight: AtomicUsize,
        delay: Duration,
    }

    impl FakeStore {
        fn new(outcomes: Vec<Result<PersistOutcome, StoreError>>) -> Self {
            Self {
                outcomes: Mutex::new(outcomes.into()),
                calls: AtomicUsize::new(0),
                in_flight: AtomicUsize::new(0),
                max_in_flight: AtomicUsize::new(0),
                delay: Duration::from_millis(1),
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

        async fn persist(&self, _message: &ValidatedMessage) -> Result<PersistOutcome, StoreError> {
            self.calls.fetch_add(1, Ordering::Relaxed);
            let current = self.in_flight.fetch_add(1, Ordering::SeqCst) + 1;
            self.max_in_flight.fetch_max(current, Ordering::SeqCst);
            tokio::time::sleep(self.delay).await;
            self.in_flight.fetch_sub(1, Ordering::SeqCst);
            self.outcomes
                .lock()
                .unwrap()
                .pop_front()
                .unwrap_or(Ok(PersistOutcome {
                    feed_event_inserted: true,
                    origin_transaction_inserted: true,
                }))
        }
    }

    struct FakeSubscriber {
        subjects: Arc<Mutex<Vec<String>>>,
        messages: Vec<Vec<u8>>,
    }

    #[async_trait]
    impl CoreSubscriber for FakeSubscriber {
        async fn subscribe_core(&self, subject: &str) -> Result<MessageStream, SubscriptionError> {
            self.subjects.lock().unwrap().push(subject.to_string());
            Ok(Box::pin(futures_util::stream::iter(self.messages.clone())))
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

    fn ready_state() -> Readiness {
        let readiness = Readiness::new();
        readiness.set_postgres_connected(true);
        readiness.set_schema_verified(true);
        readiness.set_nats_connected(true);
        readiness
    }

    #[tokio::test]
    async fn successful_subscription_uses_exact_subject_and_enables_readiness() {
        let readiness = ready_state();
        assert_eq!(readiness.ready(), Err("NATS subscription inactive"));
        let subjects = Arc::new(Mutex::new(Vec::new()));
        let subscriber = FakeSubscriber {
            subjects: subjects.clone(),
            messages: Vec::new(),
        };
        let _stream = activate_subscription(&subscriber, &readiness)
            .await
            .unwrap();
        assert_eq!(&*subjects.lock().unwrap(), &[NATS_SUBJECT.to_string()]);
        assert_eq!(readiness.ready(), Ok(()));
    }

    #[tokio::test]
    async fn normalized_messages_persist_sequentially_with_bounded_concurrency() {
        let store = Arc::new(FakeStore::new(vec![]));
        let readiness = ready_state();
        readiness.set_subscription_active(true);
        let metrics = Metrics::default();
        let exit = consume_messages(
            Box::pin(futures_util::stream::iter(vec![
                payload(9, 'a'),
                payload(9, 'b'),
            ])),
            store.clone(),
            readiness,
            metrics.clone(),
            LogSampler::default(),
            CancellationToken::new(),
            RetryPolicy {
                initial: Duration::from_millis(1),
                maximum: Duration::from_millis(2),
            },
        )
        .await;
        assert_eq!(exit, ConsumerExit::SubscriptionEnded);
        assert_eq!(store.calls.load(Ordering::Relaxed), 2);
        assert_eq!(store.max_in_flight.load(Ordering::Relaxed), 1);
        let rendered = metrics.render(&Readiness::new());
        assert!(rendered.contains("recorder_messages_received_total 2"));
        assert!(rendered.contains("recorder_transactions_persisted_total 2"));
    }

    #[tokio::test]
    async fn duplicate_delivery_is_skipped_idempotently() {
        let inserted = PersistOutcome {
            feed_event_inserted: true,
            origin_transaction_inserted: true,
        };
        let store = Arc::new(FakeStore::new(vec![
            Ok(inserted),
            Ok(PersistOutcome::default()),
        ]));
        let readiness = ready_state();
        readiness.set_subscription_active(true);
        let metrics = Metrics::default();
        let message = payload(5, 'a');
        consume_messages(
            Box::pin(futures_util::stream::iter(vec![message.clone(), message])),
            store,
            readiness,
            metrics.clone(),
            LogSampler::default(),
            CancellationToken::new(),
            RetryPolicy::default(),
        )
        .await;
        let rendered = metrics.render(&Readiness::new());
        assert!(rendered.contains("recorder_messages_persisted_total 1"));
        assert!(rendered.contains("recorder_duplicate_skips_total 1"));
    }

    #[tokio::test]
    async fn duplicate_transaction_hash_preserves_new_feed_event_without_new_origin() {
        let store = Arc::new(FakeStore::new(vec![Ok(PersistOutcome {
            feed_event_inserted: true,
            origin_transaction_inserted: false,
        })]));
        let readiness = ready_state();
        readiness.set_subscription_active(true);
        let metrics = Metrics::default();
        consume_messages(
            Box::pin(futures_util::stream::iter(vec![payload(6, 'a')])),
            store,
            readiness,
            metrics.clone(),
            LogSampler::default(),
            CancellationToken::new(),
            RetryPolicy::default(),
        )
        .await;
        let rendered = metrics.render(&Readiness::new());
        assert!(rendered.contains("recorder_messages_persisted_total 1"));
        assert!(rendered.contains("recorder_transactions_persisted_total 0"));
        assert!(rendered.contains("recorder_duplicate_skips_total 1"));
    }

    #[tokio::test]
    async fn malformed_payload_is_not_persisted() {
        let store = Arc::new(FakeStore::new(vec![]));
        let readiness = ready_state();
        readiness.set_subscription_active(true);
        let metrics = Metrics::default();
        consume_messages(
            Box::pin(futures_util::stream::iter(vec![b"not-json".to_vec()])),
            store.clone(),
            readiness,
            metrics.clone(),
            LogSampler::default(),
            CancellationToken::new(),
            RetryPolicy::default(),
        )
        .await;
        assert_eq!(store.calls.load(Ordering::Relaxed), 0);
        assert!(metrics
            .render(&Readiness::new())
            .contains("recorder_decode_failures_total 1"));
    }

    #[tokio::test]
    async fn database_failure_retries_same_message_without_silent_success() {
        let store = Arc::new(FakeStore::new(vec![
            Err(StoreError::Connection),
            Ok(PersistOutcome {
                feed_event_inserted: true,
                origin_transaction_inserted: true,
            }),
        ]));
        let readiness = ready_state();
        readiness.set_subscription_active(true);
        let metrics = Metrics::default();
        consume_messages(
            Box::pin(futures_util::stream::iter(vec![payload(7, 'a')])),
            store.clone(),
            readiness,
            metrics.clone(),
            LogSampler::default(),
            CancellationToken::new(),
            RetryPolicy {
                initial: Duration::from_millis(1),
                maximum: Duration::from_millis(2),
            },
        )
        .await;
        assert_eq!(store.calls.load(Ordering::Relaxed), 2);
        let rendered = metrics.render(&Readiness::new());
        assert!(rendered.contains("recorder_database_failures_total 1"));
        assert!(rendered.contains("recorder_messages_persisted_total 1"));
    }

    #[tokio::test]
    async fn graceful_shutdown_interrupts_database_retry() {
        let store = Arc::new(FakeStore::new(vec![Err(StoreError::Connection); 20]));
        let readiness = ready_state();
        readiness.set_subscription_active(true);
        let shutdown = CancellationToken::new();
        let cancel = shutdown.clone();
        tokio::spawn(async move {
            tokio::time::sleep(Duration::from_millis(5)).await;
            cancel.cancel();
        });
        let exit = consume_messages(
            Box::pin(futures_util::stream::iter(vec![payload(7, 'a')])),
            store,
            readiness,
            Metrics::default(),
            LogSampler::default(),
            shutdown,
            RetryPolicy {
                initial: Duration::from_secs(1),
                maximum: Duration::from_secs(1),
            },
        )
        .await;
        assert_eq!(exit, ConsumerExit::Shutdown);
    }

    #[test]
    fn nats_disconnect_and_reconnect_control_readiness_and_metrics() {
        let readiness = ready_state();
        readiness.set_subscription_active(true);
        let disconnected = AtomicBool::new(false);
        mark_nats_disconnected(&readiness, &disconnected);
        assert_eq!(readiness.ready(), Err("NATS disconnected"));
        assert!(disconnected.load(Ordering::Acquire));

        let metrics = Metrics::default();
        mark_nats_connected(&readiness, &metrics, &disconnected);
        readiness.set_subscription_active(true);
        assert_eq!(readiness.ready(), Ok(()));
        assert!(metrics
            .render(&readiness)
            .contains("recorder_nats_reconnects_total 1"));
    }
}
