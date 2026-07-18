use crate::engine_outbox::{BacklogTelemetry, OutboxError, OutboxStore, MAX_CLAIM_BATCH};
use crate::engine_stream::{EnginePublisher, EngineStreamError};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};
use thiserror::Error;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DispatchConfig {
    pub owner: String,
    pub batch_size: usize,
    pub lease: Duration,
    pub retry_initial: Duration,
    pub retry_maximum: Duration,
}

impl DispatchConfig {
    pub fn validate(self) -> Result<Self, DispatcherError> {
        if self.owner.is_empty()
            || self.owner.len() > 128
            || self.batch_size == 0
            || self.batch_size > MAX_CLAIM_BATCH
            || self.lease < Duration::from_secs(5)
            || self.lease > Duration::from_secs(5 * 60)
            || self.retry_initial.is_zero()
            || self.retry_maximum < self.retry_initial
            || self.retry_maximum > Duration::from_secs(5 * 60)
        {
            Err(DispatcherError::Configuration)
        } else {
            Ok(self)
        }
    }
}

impl Default for DispatchConfig {
    fn default() -> Self {
        Self {
            owner: "shadow-dispatcher".to_string(),
            batch_size: MAX_CLAIM_BATCH,
            lease: Duration::from_secs(30),
            retry_initial: Duration::from_secs(1),
            retry_maximum: Duration::from_secs(60),
        }
    }
}

#[derive(Clone, Debug, Error, PartialEq, Eq)]
pub enum DispatcherError {
    #[error("Shadow Dispatcher configuration is invalid")]
    Configuration,
    #[error("Shadow Dispatcher outbox operation failed: {0}")]
    Outbox(OutboxError),
    #[error("Shadow Dispatcher JetStream operation failed: {0}")]
    Stream(EngineStreamError),
    #[error("Shadow Dispatcher stopped on a terminal integrity condition")]
    TerminalIntegrity,
}

#[derive(Clone, Debug, Default)]
pub struct DispatcherMetrics {
    inner: Arc<MetricValues>,
}

#[derive(Debug, Default)]
struct MetricValues {
    rows_claimed: AtomicU64,
    publish_success: AtomicU64,
    publish_failures: AtomicU64,
    retries: AtomicU64,
    retry_recoveries: AtomicU64,
    terminal_integrity_failures: AtomicU64,
    pending_rows_estimate: AtomicU64,
    oldest_claimable_age_nanos: AtomicU64,
    backlog_refresh_total: AtomicU64,
    backlog_refresh_failures: AtomicU64,
    backlog_last_success_unix_seconds: AtomicU64,
    batch_size: AtomicU64,
    batch_cycle_nanos: AtomicU64,
    publish_latency_nanos: AtomicU64,
}

impl DispatcherMetrics {
    pub fn rows_claimed(&self, rows: usize) {
        self.inner
            .rows_claimed
            .fetch_add(rows as u64, Ordering::Relaxed);
        self.inner.batch_size.store(rows as u64, Ordering::Relaxed);
    }

    pub fn publish_success(&self, latency: Duration) {
        self.inner.publish_success.fetch_add(1, Ordering::Relaxed);
        self.inner.publish_latency_nanos.store(
            latency.as_nanos().min(u64::MAX as u128) as u64,
            Ordering::Relaxed,
        );
    }

    pub fn publish_failure(&self, latency: Duration) {
        self.inner.publish_failures.fetch_add(1, Ordering::Relaxed);
        self.inner.publish_latency_nanos.store(
            latency.as_nanos().min(u64::MAX as u128) as u64,
            Ordering::Relaxed,
        );
    }

    pub fn retry(&self) {
        self.inner.retries.fetch_add(1, Ordering::Relaxed);
    }

    pub fn retry_recovered(&self) {
        self.inner.retry_recoveries.fetch_add(1, Ordering::Relaxed);
    }

    pub fn terminal_integrity_failure(&self) {
        self.inner
            .terminal_integrity_failures
            .fetch_add(1, Ordering::Relaxed);
    }

    pub fn batch_cycle(&self, duration: Duration) {
        self.inner.batch_cycle_nanos.store(
            duration.as_nanos().min(u64::MAX as u128) as u64,
            Ordering::Relaxed,
        );
    }

    pub fn backlog_refresh_succeeded(&self, state: BacklogTelemetry) {
        self.inner
            .backlog_refresh_total
            .fetch_add(1, Ordering::Relaxed);
        self.inner
            .pending_rows_estimate
            .store(state.pending_rows_estimate, Ordering::Relaxed);
        let nanos = (state.oldest_claimable_age_seconds.max(0.0) * 1_000_000_000.0)
            .min(u64::MAX as f64) as u64;
        self.inner
            .oldest_claimable_age_nanos
            .store(nanos, Ordering::Relaxed);
        self.inner
            .backlog_last_success_unix_seconds
            .store(unix_seconds(), Ordering::Relaxed);
    }

    pub fn backlog_refresh_failed(&self) {
        self.inner
            .backlog_refresh_total
            .fetch_add(1, Ordering::Relaxed);
        self.inner
            .backlog_refresh_failures
            .fetch_add(1, Ordering::Relaxed);
    }

    pub fn render(&self, readiness: &DispatcherReadiness) -> String {
        let last_success = self
            .inner
            .backlog_last_success_unix_seconds
            .load(Ordering::Relaxed);
        let backlog_stale_seconds = unix_seconds().saturating_sub(last_success);
        format!(
            concat!(
                "# TYPE shadow_dispatcher_rows_claimed_total counter\n",
                "shadow_dispatcher_rows_claimed_total {}\n",
                "# TYPE shadow_dispatcher_rows_published_total counter\n",
                "shadow_dispatcher_rows_published_total {}\n",
                "# TYPE shadow_dispatcher_publish_success_total counter\n",
                "shadow_dispatcher_publish_success_total {}\n",
                "# TYPE shadow_dispatcher_publish_failures_total counter\n",
                "shadow_dispatcher_publish_failures_total {}\n",
                "# TYPE shadow_dispatcher_retries_total counter\n",
                "shadow_dispatcher_retries_total {}\n",
                "# TYPE shadow_dispatcher_retry_recoveries_total counter\n",
                "shadow_dispatcher_retry_recoveries_total {}\n",
                "# TYPE shadow_dispatcher_terminal_integrity_failures_total counter\n",
                "shadow_dispatcher_terminal_integrity_failures_total {}\n",
                "# HELP shadow_dispatcher_pending_rows_estimate Estimated unpublished rows from PostgreSQL partial-index statistics.\n",
                "# TYPE shadow_dispatcher_pending_rows_estimate gauge\n",
                "shadow_dispatcher_pending_rows_estimate {}\n",
                "# TYPE shadow_dispatcher_oldest_claimable_age_seconds gauge\n",
                "shadow_dispatcher_oldest_claimable_age_seconds {:.9}\n",
                "# TYPE shadow_dispatcher_backlog_refresh_total counter\n",
                "shadow_dispatcher_backlog_refresh_total {}\n",
                "# TYPE shadow_dispatcher_backlog_refresh_failures_total counter\n",
                "shadow_dispatcher_backlog_refresh_failures_total {}\n",
                "# TYPE shadow_dispatcher_backlog_stale_seconds gauge\n",
                "shadow_dispatcher_backlog_stale_seconds {}\n",
                "# TYPE shadow_dispatcher_batch_size gauge\n",
                "shadow_dispatcher_batch_size {}\n",
                "# TYPE shadow_dispatcher_batch_cycle_seconds gauge\n",
                "shadow_dispatcher_batch_cycle_seconds {:.9}\n",
                "# TYPE shadow_dispatcher_publish_latency_seconds gauge\n",
                "shadow_dispatcher_publish_latency_seconds {:.9}\n",
                "# TYPE shadow_dispatcher_readiness gauge\n",
                "shadow_dispatcher_readiness {}\n"
            ),
            self.inner.rows_claimed.load(Ordering::Relaxed),
            self.inner.publish_success.load(Ordering::Relaxed),
            self.inner.publish_success.load(Ordering::Relaxed),
            self.inner.publish_failures.load(Ordering::Relaxed),
            self.inner.retries.load(Ordering::Relaxed),
            self.inner.retry_recoveries.load(Ordering::Relaxed),
            self.inner
                .terminal_integrity_failures
                .load(Ordering::Relaxed),
            self.inner.pending_rows_estimate.load(Ordering::Relaxed),
            self.inner
                .oldest_claimable_age_nanos
                .load(Ordering::Relaxed) as f64
                / 1_000_000_000.0,
            self.inner.backlog_refresh_total.load(Ordering::Relaxed),
            self.inner
                .backlog_refresh_failures
                .load(Ordering::Relaxed),
            backlog_stale_seconds,
            self.inner.batch_size.load(Ordering::Relaxed),
            self.inner.batch_cycle_nanos.load(Ordering::Relaxed) as f64 / 1_000_000_000.0,
            self.inner.publish_latency_nanos.load(Ordering::Relaxed) as f64 / 1_000_000_000.0,
            u8::from(readiness.ready().is_ok()),
        )
    }
}

#[derive(Clone, Debug)]
pub struct DispatcherReadiness {
    inner: Arc<ReadinessValues>,
}

#[derive(Debug)]
struct ReadinessValues {
    event_loop_alive: AtomicBool,
    postgres_connected: AtomicBool,
    schema_verified: AtomicBool,
    jetstream_connected: AtomicBool,
    stream_compatible: AtomicBool,
    publisher_active: AtomicBool,
    integrity_healthy: AtomicBool,
}

impl Default for DispatcherReadiness {
    fn default() -> Self {
        Self::new()
    }
}

impl DispatcherReadiness {
    pub fn new() -> Self {
        Self {
            inner: Arc::new(ReadinessValues {
                event_loop_alive: AtomicBool::new(true),
                postgres_connected: AtomicBool::new(false),
                schema_verified: AtomicBool::new(false),
                jetstream_connected: AtomicBool::new(false),
                stream_compatible: AtomicBool::new(false),
                publisher_active: AtomicBool::new(false),
                integrity_healthy: AtomicBool::new(true),
            }),
        }
    }

    pub fn set_postgres_connected(&self, value: bool) {
        self.inner
            .postgres_connected
            .store(value, Ordering::Release);
        if !value {
            self.inner.schema_verified.store(false, Ordering::Release);
            self.inner.publisher_active.store(false, Ordering::Release);
        }
    }

    pub fn set_schema_verified(&self, value: bool) {
        self.inner.schema_verified.store(value, Ordering::Release);
    }

    pub fn set_jetstream_connected(&self, value: bool) {
        self.inner
            .jetstream_connected
            .store(value, Ordering::Release);
        if !value {
            self.inner.stream_compatible.store(false, Ordering::Release);
            self.inner.publisher_active.store(false, Ordering::Release);
        }
    }

    pub fn set_stream_compatible(&self, value: bool) {
        self.inner.stream_compatible.store(value, Ordering::Release);
    }

    pub fn set_publisher_active(&self, value: bool) {
        self.inner.publisher_active.store(value, Ordering::Release);
    }

    pub fn mark_terminal_integrity(&self) {
        self.inner.integrity_healthy.store(false, Ordering::Release);
        self.inner.publisher_active.store(false, Ordering::Release);
    }

    pub fn stop_event_loop(&self) {
        self.inner.event_loop_alive.store(false, Ordering::Release);
        self.inner.publisher_active.store(false, Ordering::Release);
    }

    pub fn healthy(&self) -> bool {
        self.inner.event_loop_alive.load(Ordering::Acquire)
    }

    pub fn ready(&self) -> Result<(), &'static str> {
        if !self.healthy() {
            return Err("Shadow Dispatcher event loop stopped");
        }
        if !self.inner.integrity_healthy.load(Ordering::Acquire) {
            return Err("terminal Shadow Dispatcher integrity condition detected");
        }
        if !self.inner.postgres_connected.load(Ordering::Acquire) {
            return Err("PostgreSQL unavailable");
        }
        if !self.inner.schema_verified.load(Ordering::Acquire) {
            return Err("outbox schema not verified");
        }
        if !self.inner.jetstream_connected.load(Ordering::Acquire) {
            return Err("JetStream disconnected");
        }
        if !self.inner.stream_compatible.load(Ordering::Acquire) {
            return Err("Engine input stream unavailable or incompatible");
        }
        if !self.inner.publisher_active.load(Ordering::Acquire) {
            return Err("Shadow Dispatcher publisher loop inactive");
        }
        Ok(())
    }
}

pub async fn dispatch_once(
    store: &dyn OutboxStore,
    publisher: &dyn EnginePublisher,
    config: &DispatchConfig,
    readiness: &DispatcherReadiness,
    metrics: &DispatcherMetrics,
) -> Result<usize, DispatcherError> {
    let cycle_started = Instant::now();
    let rows = match store
        .claim_batch(&config.owner, config.batch_size, config.lease)
        .await
    {
        Ok(rows) => {
            readiness.set_postgres_connected(true);
            rows
        }
        Err(error) => {
            readiness.set_postgres_connected(false);
            return Err(DispatcherError::Outbox(error));
        }
    };
    metrics.rows_claimed(rows.len());

    for row in &rows {
        let started = Instant::now();
        match publisher.publish(row).await {
            Ok(receipt) => {
                if let Err(error) = store
                    .mark_published(&row.outbox_id, &config.owner, receipt.stream_sequence)
                    .await
                {
                    metrics.publish_failure(started.elapsed());
                    readiness.set_postgres_connected(false);
                    return Err(DispatcherError::Outbox(error));
                }
                metrics.publish_success(started.elapsed());
                if row.publish_attempts > 1 {
                    metrics.retry_recovered();
                }
            }
            Err(error) if error.terminal() => {
                metrics.publish_failure(started.elapsed());
                metrics.terminal_integrity_failure();
                readiness.mark_terminal_integrity();
                return Err(DispatcherError::TerminalIntegrity);
            }
            Err(error) => {
                metrics.publish_failure(started.elapsed());
                let delay = retry_delay(
                    config.retry_initial,
                    config.retry_maximum,
                    row.publish_attempts,
                );
                if let Err(store_error) = store
                    .release_for_retry(&row.outbox_id, &config.owner, error.class(), delay)
                    .await
                {
                    readiness.set_postgres_connected(false);
                    return Err(DispatcherError::Outbox(store_error));
                }
                metrics.retry();
                readiness.set_publisher_active(false);
                return Err(DispatcherError::Stream(error));
            }
        }
    }

    readiness.set_postgres_connected(true);
    readiness.set_publisher_active(true);
    metrics.batch_cycle(cycle_started.elapsed());
    Ok(rows.len())
}

pub async fn refresh_backlog_telemetry(
    store: &dyn OutboxStore,
    metrics: &DispatcherMetrics,
    statement_timeout: Duration,
) -> Result<(), OutboxError> {
    match store.backlog_telemetry(statement_timeout).await {
        Ok(state) => {
            metrics.backlog_refresh_succeeded(state);
            Ok(())
        }
        Err(error) => {
            metrics.backlog_refresh_failed();
            Err(error)
        }
    }
}

fn unix_seconds() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
}

pub fn retry_delay(initial: Duration, maximum: Duration, attempt: u32) -> Duration {
    let shift = attempt.saturating_sub(1).min(31);
    let multiplier = 1_u32.checked_shl(shift).unwrap_or(u32::MAX);
    initial.saturating_mul(multiplier).min(maximum)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::engine_outbox::OutboxRow;
    use crate::engine_stream::{EnginePublishReceipt, EngineStreamError};
    use crate::model::{ENGINE_INPUT_SCHEMA_VERSION, NORMALIZED_SCHEMA_VERSION};
    use async_trait::async_trait;
    use chrono::Utc;
    use serde_json::json;
    use std::collections::VecDeque;
    use std::sync::Mutex;

    #[derive(Debug)]
    struct FakeStore {
        claims: Mutex<VecDeque<Result<Vec<OutboxRow>, OutboxError>>>,
        mark_result: Mutex<Result<(), OutboxError>>,
        release_result: Mutex<Result<(), OutboxError>>,
        events: Arc<Mutex<Vec<&'static str>>>,
    }

    impl FakeStore {
        fn new(rows: Vec<OutboxRow>, events: Arc<Mutex<Vec<&'static str>>>) -> Self {
            Self {
                claims: Mutex::new(VecDeque::from([Ok(rows)])),
                mark_result: Mutex::new(Ok(())),
                release_result: Mutex::new(Ok(())),
                events,
            }
        }
    }

    #[async_trait]
    impl OutboxStore for FakeStore {
        async fn ping(&self) -> Result<(), OutboxError> {
            Ok(())
        }

        async fn verify_schema(&self) -> Result<(), OutboxError> {
            Ok(())
        }

        async fn claim_batch(
            &self,
            _owner: &str,
            _max_rows: usize,
            _lease: Duration,
        ) -> Result<Vec<OutboxRow>, OutboxError> {
            self.events.lock().unwrap().push("claim");
            self.claims
                .lock()
                .unwrap()
                .pop_front()
                .unwrap_or_else(|| Ok(Vec::new()))
        }

        async fn mark_published(
            &self,
            _outbox_id: &str,
            _owner: &str,
            _ack_sequence: u64,
        ) -> Result<(), OutboxError> {
            self.events.lock().unwrap().push("mark");
            self.mark_result.lock().unwrap().clone()
        }

        async fn release_for_retry(
            &self,
            _outbox_id: &str,
            _owner: &str,
            _error_class: &'static str,
            _delay: Duration,
        ) -> Result<(), OutboxError> {
            self.events.lock().unwrap().push("release");
            self.release_result.lock().unwrap().clone()
        }

        async fn backlog_telemetry(
            &self,
            _statement_timeout: Duration,
        ) -> Result<BacklogTelemetry, OutboxError> {
            self.events.lock().unwrap().push("telemetry");
            Ok(BacklogTelemetry {
                pending_rows_estimate: 3,
                oldest_claimable_age_seconds: 2.5,
            })
        }
    }

    #[derive(Debug)]
    struct FakePublisher {
        result: Result<EnginePublishReceipt, EngineStreamError>,
        events: Arc<Mutex<Vec<&'static str>>>,
    }

    #[async_trait]
    impl EnginePublisher for FakePublisher {
        async fn publish(
            &self,
            _row: &OutboxRow,
        ) -> Result<EnginePublishReceipt, EngineStreamError> {
            self.events.lock().unwrap().push("publish_ack");
            self.result.clone()
        }
    }

    fn row() -> OutboxRow {
        let tx_hash = format!("0x{}", "a".repeat(64));
        OutboxRow {
            outbox_id: format!("{ENGINE_INPUT_SCHEMA_VERSION}:7:{tx_hash}"),
            schema_version: ENGINE_INPUT_SCHEMA_VERSION.to_string(),
            source_event_identity: format!("{ENGINE_INPUT_SCHEMA_VERSION}:7:{tx_hash}"),
            source_sequence: 7,
            tx_hash: tx_hash.clone(),
            chain_id: 42161,
            payload: json!({
                "schema_version": NORMALIZED_SCHEMA_VERSION,
                "sequence": 7,
                "tx_hash": tx_hash,
                "chain_id": 42161
            }),
            created_at: Utc::now(),
            publish_attempts: 1,
        }
    }

    fn ready_dependencies(readiness: &DispatcherReadiness) {
        readiness.set_postgres_connected(true);
        readiness.set_schema_verified(true);
        readiness.set_jetstream_connected(true);
        readiness.set_stream_compatible(true);
    }

    #[tokio::test]
    async fn persistence_ack_precedes_published_mark() {
        let events = Arc::new(Mutex::new(Vec::new()));
        let store = FakeStore::new(vec![row()], events.clone());
        let publisher = FakePublisher {
            result: Ok(EnginePublishReceipt {
                stream_sequence: 11,
                duplicate: false,
            }),
            events: events.clone(),
        };
        let readiness = DispatcherReadiness::new();
        ready_dependencies(&readiness);
        let metrics = DispatcherMetrics::default();
        assert_eq!(
            dispatch_once(
                &store,
                &publisher,
                &DispatchConfig::default(),
                &readiness,
                &metrics,
            )
            .await,
            Ok(1)
        );
        assert_eq!(&*events.lock().unwrap(), &["claim", "publish_ack", "mark"]);
        assert!(readiness.ready().is_ok());
        let rendered = metrics.render(&readiness);
        assert!(rendered.contains("shadow_dispatcher_publish_success_total 1"));
        assert!(!events.lock().unwrap().contains(&"telemetry"));
        assert!(rendered.contains("shadow_dispatcher_batch_cycle_seconds"));
    }

    #[tokio::test]
    async fn telemetry_refresh_is_separate_and_failure_does_not_change_readiness() {
        let events = Arc::new(Mutex::new(Vec::new()));
        let store = FakeStore::new(Vec::new(), events.clone());
        let readiness = DispatcherReadiness::new();
        ready_dependencies(&readiness);
        readiness.set_publisher_active(true);
        let metrics = DispatcherMetrics::default();

        refresh_backlog_telemetry(&store, &metrics, Duration::from_secs(2))
            .await
            .unwrap();

        assert_eq!(&*events.lock().unwrap(), &["telemetry"]);
        assert!(readiness.ready().is_ok());
        let rendered = metrics.render(&readiness);
        assert!(rendered.contains("shadow_dispatcher_pending_rows_estimate 3"));
        assert!(rendered.contains("shadow_dispatcher_oldest_claimable_age_seconds 2.500000000"));
        assert!(rendered.contains("shadow_dispatcher_backlog_refresh_total 1"));
        assert!(rendered.contains("shadow_dispatcher_backlog_refresh_failures_total 0"));
    }

    #[tokio::test]
    async fn failed_publish_releases_for_bounded_retry_without_database_mark() {
        let events = Arc::new(Mutex::new(Vec::new()));
        let store = FakeStore::new(vec![row()], events.clone());
        let publisher = FakePublisher {
            result: Err(EngineStreamError::Publish),
            events: events.clone(),
        };
        let readiness = DispatcherReadiness::new();
        ready_dependencies(&readiness);
        let result = dispatch_once(
            &store,
            &publisher,
            &DispatchConfig::default(),
            &readiness,
            &DispatcherMetrics::default(),
        )
        .await;
        assert!(matches!(result, Err(DispatcherError::Stream(_))));
        assert_eq!(
            &*events.lock().unwrap(),
            &["claim", "publish_ack", "release"]
        );
        assert_eq!(
            readiness.ready(),
            Err("Shadow Dispatcher publisher loop inactive")
        );
    }

    #[tokio::test]
    async fn crash_window_after_ack_never_releases_or_marks_without_database_success() {
        let events = Arc::new(Mutex::new(Vec::new()));
        let store = FakeStore::new(vec![row()], events.clone());
        *store.mark_result.lock().unwrap() = Err(OutboxError::Connection);
        let publisher = FakePublisher {
            result: Ok(EnginePublishReceipt {
                stream_sequence: 12,
                duplicate: false,
            }),
            events: events.clone(),
        };
        let result = dispatch_once(
            &store,
            &publisher,
            &DispatchConfig::default(),
            &DispatcherReadiness::new(),
            &DispatcherMetrics::default(),
        )
        .await;
        assert!(matches!(result, Err(DispatcherError::Outbox(_))));
        assert_eq!(&*events.lock().unwrap(), &["claim", "publish_ack", "mark"]);
    }

    #[tokio::test]
    async fn terminal_event_integrity_stays_fail_closed_without_row_loss() {
        let events = Arc::new(Mutex::new(Vec::new()));
        let store = FakeStore::new(vec![row()], events.clone());
        let publisher = FakePublisher {
            result: Err(EngineStreamError::Integrity),
            events: events.clone(),
        };
        let readiness = DispatcherReadiness::new();
        ready_dependencies(&readiness);
        let metrics = DispatcherMetrics::default();
        assert_eq!(
            dispatch_once(
                &store,
                &publisher,
                &DispatchConfig::default(),
                &readiness,
                &metrics,
            )
            .await,
            Err(DispatcherError::TerminalIntegrity)
        );
        assert_eq!(&*events.lock().unwrap(), &["claim", "publish_ack"]);
        assert_eq!(
            readiness.ready(),
            Err("terminal Shadow Dispatcher integrity condition detected")
        );
        assert!(metrics
            .render(&readiness)
            .contains("shadow_dispatcher_terminal_integrity_failures_total 1"));
    }

    #[tokio::test]
    async fn postgres_outage_recovers_without_acknowledging_or_losing_work() {
        let events = Arc::new(Mutex::new(Vec::new()));
        let store = FakeStore {
            claims: Mutex::new(VecDeque::from([
                Err(OutboxError::Connection),
                Ok(vec![row()]),
            ])),
            mark_result: Mutex::new(Ok(())),
            release_result: Mutex::new(Ok(())),
            events: events.clone(),
        };
        let publisher = FakePublisher {
            result: Ok(EnginePublishReceipt {
                stream_sequence: 13,
                duplicate: false,
            }),
            events: events.clone(),
        };
        let readiness = DispatcherReadiness::new();
        ready_dependencies(&readiness);
        let metrics = DispatcherMetrics::default();
        assert!(matches!(
            dispatch_once(
                &store,
                &publisher,
                &DispatchConfig::default(),
                &readiness,
                &metrics,
            )
            .await,
            Err(DispatcherError::Outbox(OutboxError::Connection))
        ));
        assert_eq!(readiness.ready(), Err("PostgreSQL unavailable"));

        ready_dependencies(&readiness);
        assert_eq!(
            dispatch_once(
                &store,
                &publisher,
                &DispatchConfig::default(),
                &readiness,
                &metrics,
            )
            .await,
            Ok(1)
        );
        assert!(readiness.ready().is_ok());
        assert_eq!(
            &*events.lock().unwrap(),
            &["claim", "claim", "publish_ack", "mark"]
        );
    }

    #[tokio::test]
    async fn nats_outage_retries_the_same_claimed_identity_then_recovers() {
        let events = Arc::new(Mutex::new(Vec::new()));
        let event = row();
        let mut retried_event = event.clone();
        retried_event.publish_attempts = 2;
        let store = FakeStore {
            claims: Mutex::new(VecDeque::from([Ok(vec![event]), Ok(vec![retried_event])])),
            mark_result: Mutex::new(Ok(())),
            release_result: Mutex::new(Ok(())),
            events: events.clone(),
        };
        let readiness = DispatcherReadiness::new();
        ready_dependencies(&readiness);
        let metrics = DispatcherMetrics::default();
        let unavailable = FakePublisher {
            result: Err(EngineStreamError::Publish),
            events: events.clone(),
        };
        assert!(matches!(
            dispatch_once(
                &store,
                &unavailable,
                &DispatchConfig::default(),
                &readiness,
                &metrics,
            )
            .await,
            Err(DispatcherError::Stream(EngineStreamError::Publish))
        ));

        ready_dependencies(&readiness);
        let recovered = FakePublisher {
            result: Ok(EnginePublishReceipt {
                stream_sequence: 14,
                duplicate: true,
            }),
            events: events.clone(),
        };
        assert_eq!(
            dispatch_once(
                &store,
                &recovered,
                &DispatchConfig::default(),
                &readiness,
                &metrics,
            )
            .await,
            Ok(1)
        );
        assert_eq!(
            &*events.lock().unwrap(),
            &[
                "claim",
                "publish_ack",
                "release",
                "claim",
                "publish_ack",
                "mark"
            ]
        );
        let rendered = metrics.render(&readiness);
        assert!(rendered.contains("shadow_dispatcher_publish_failures_total 1"));
        assert!(rendered.contains("shadow_dispatcher_retries_total 1"));
        assert!(rendered.contains("shadow_dispatcher_retry_recoveries_total 1"));
        assert!(rendered.contains("shadow_dispatcher_publish_success_total 1"));
    }

    #[test]
    fn retry_backoff_is_exponential_and_bounded() {
        let initial = Duration::from_secs(1);
        let maximum = Duration::from_secs(60);
        assert_eq!(retry_delay(initial, maximum, 1), Duration::from_secs(1));
        assert_eq!(retry_delay(initial, maximum, 4), Duration::from_secs(8));
        assert_eq!(retry_delay(initial, maximum, 99), maximum);
        assert!(DispatchConfig::default().validate().is_ok());
        assert_eq!(
            DispatchConfig {
                batch_size: MAX_CLAIM_BATCH + 1,
                ..DispatchConfig::default()
            }
            .validate(),
            Err(DispatcherError::Configuration)
        );
    }

    #[test]
    fn readiness_recovers_after_transient_postgres_and_nats_outages() {
        let readiness = DispatcherReadiness::new();
        ready_dependencies(&readiness);
        readiness.set_publisher_active(true);
        assert!(readiness.ready().is_ok());

        readiness.set_postgres_connected(false);
        assert_eq!(readiness.ready(), Err("PostgreSQL unavailable"));
        readiness.set_postgres_connected(true);
        readiness.set_schema_verified(true);
        readiness.set_publisher_active(true);
        assert!(readiness.ready().is_ok());

        readiness.set_jetstream_connected(false);
        assert_eq!(readiness.ready(), Err("JetStream disconnected"));
        readiness.set_jetstream_connected(true);
        readiness.set_stream_compatible(true);
        readiness.set_publisher_active(true);
        assert!(readiness.ready().is_ok());
    }

    #[test]
    fn metrics_are_bounded_and_have_no_identity_labels() {
        let rendered = DispatcherMetrics::default().render(&DispatcherReadiness::new());
        for required in [
            "shadow_dispatcher_rows_claimed_total",
            "shadow_dispatcher_rows_published_total",
            "shadow_dispatcher_publish_success_total",
            "shadow_dispatcher_publish_failures_total",
            "shadow_dispatcher_retries_total",
            "shadow_dispatcher_retry_recoveries_total",
            "shadow_dispatcher_terminal_integrity_failures_total",
            "shadow_dispatcher_pending_rows_estimate",
            "shadow_dispatcher_oldest_claimable_age_seconds",
            "shadow_dispatcher_backlog_refresh_total",
            "shadow_dispatcher_backlog_refresh_failures_total",
            "shadow_dispatcher_backlog_stale_seconds",
            "shadow_dispatcher_batch_size",
            "shadow_dispatcher_batch_cycle_seconds",
            "shadow_dispatcher_publish_latency_seconds",
            "shadow_dispatcher_readiness",
        ] {
            assert!(rendered.contains(required));
        }
        for forbidden in ["tx_hash=", "outbox_id=", "pool=", "token="] {
            assert!(!rendered.contains(forbidden));
        }
    }
}
