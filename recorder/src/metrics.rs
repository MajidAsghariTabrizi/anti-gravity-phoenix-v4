use crate::jetstream::ConsumerState;
use crate::state::Readiness;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use std::time::Duration;

#[derive(Clone, Debug, Default)]
pub struct Metrics {
    inner: Arc<MetricValues>,
}

#[derive(Debug, Default)]
struct MetricValues {
    messages_received: AtomicU64,
    messages_persisted: AtomicU64,
    transactions_persisted: AtomicU64,
    engine_outbox_inserted: AtomicU64,
    duplicate_skips: AtomicU64,
    decode_failures: AtomicU64,
    database_failures: AtomicU64,
    database_retries: AtomicU64,
    database_retry_recoveries: AtomicU64,
    nats_reconnects: AtomicU64,
    jetstream_fetch_failures: AtomicU64,
    jetstream_ack_failures: AtomicU64,
    jetstream_redeliveries: AtomicU64,
    poison_messages: AtomicU64,
    batches_persisted: AtomicU64,
    batch_messages_latest: AtomicU64,
    batch_messages_total: AtomicU64,
    batch_persist_latency_nanos: AtomicU64,
    consumer_pending: AtomicU64,
    consumer_ack_pending: AtomicU64,
    last_sequence: AtomicU64,
    last_timestamp_ms: AtomicU64,
}

impl Metrics {
    pub fn message_received(&self) {
        self.inner.messages_received.fetch_add(1, Ordering::Relaxed);
    }

    pub fn message_persisted(&self) {
        self.inner
            .messages_persisted
            .fetch_add(1, Ordering::Relaxed);
    }

    pub fn transaction_persisted(&self) {
        self.inner
            .transactions_persisted
            .fetch_add(1, Ordering::Relaxed);
    }

    pub fn engine_outbox_inserted(&self) {
        self.inner
            .engine_outbox_inserted
            .fetch_add(1, Ordering::Relaxed);
    }

    pub fn duplicate_skip(&self) {
        self.inner.duplicate_skips.fetch_add(1, Ordering::Relaxed);
    }

    pub fn decode_failure(&self) {
        self.inner.decode_failures.fetch_add(1, Ordering::Relaxed);
    }

    pub fn database_failure(&self) {
        self.inner.database_failures.fetch_add(1, Ordering::Relaxed);
    }

    pub fn database_retry(&self) {
        self.inner.database_retries.fetch_add(1, Ordering::Relaxed);
    }

    pub fn database_retry_recovered(&self) {
        self.inner
            .database_retry_recoveries
            .fetch_add(1, Ordering::Relaxed);
    }

    pub fn nats_reconnect(&self) {
        self.inner.nats_reconnects.fetch_add(1, Ordering::Relaxed);
    }

    pub fn jetstream_fetch_failure(&self) {
        self.inner
            .jetstream_fetch_failures
            .fetch_add(1, Ordering::Relaxed);
    }

    pub fn jetstream_ack_failure(&self) {
        self.inner
            .jetstream_ack_failures
            .fetch_add(1, Ordering::Relaxed);
    }

    pub fn jetstream_redelivery(&self) {
        self.inner
            .jetstream_redeliveries
            .fetch_add(1, Ordering::Relaxed);
    }

    pub fn poison_message(&self) {
        self.inner.poison_messages.fetch_add(1, Ordering::Relaxed);
    }

    pub fn batch_persisted(&self, messages: usize, elapsed: Duration) {
        self.inner.batches_persisted.fetch_add(1, Ordering::Relaxed);
        self.inner
            .batch_messages_latest
            .store(messages as u64, Ordering::Relaxed);
        self.inner
            .batch_messages_total
            .fetch_add(messages as u64, Ordering::Relaxed);
        self.inner.batch_persist_latency_nanos.store(
            elapsed.as_nanos().min(u64::MAX as u128) as u64,
            Ordering::Relaxed,
        );
    }

    pub fn set_consumer_state(&self, state: ConsumerState) {
        self.inner
            .consumer_pending
            .store(state.pending, Ordering::Relaxed);
        self.inner
            .consumer_ack_pending
            .store(state.ack_pending, Ordering::Relaxed);
    }

    pub fn set_last_persisted(&self, sequence: u64, timestamp_ms: u64) {
        self.inner.last_sequence.store(sequence, Ordering::Relaxed);
        self.inner
            .last_timestamp_ms
            .store(timestamp_ms, Ordering::Relaxed);
    }

    pub fn render(&self, readiness: &Readiness) -> String {
        let ready = u8::from(readiness.ready().is_ok());
        let latency_seconds = self
            .inner
            .batch_persist_latency_nanos
            .load(Ordering::Relaxed) as f64
            / 1_000_000_000.0;
        format!(
            concat!(
                "# TYPE recorder_messages_received_total counter\n",
                "recorder_messages_received_total {}\n",
                "# TYPE recorder_messages_persisted_total counter\n",
                "recorder_messages_persisted_total {}\n",
                "# TYPE recorder_transactions_persisted_total counter\n",
                "recorder_transactions_persisted_total {}\n",
                "# TYPE recorder_engine_outbox_inserted_total counter\n",
                "recorder_engine_outbox_inserted_total {}\n",
                "# TYPE recorder_duplicate_skips_total counter\n",
                "recorder_duplicate_skips_total {}\n",
                "# TYPE recorder_decode_failures_total counter\n",
                "recorder_decode_failures_total {}\n",
                "# TYPE recorder_database_failures_total counter\n",
                "recorder_database_failures_total {}\n",
                "# TYPE recorder_database_retries_total counter\n",
                "recorder_database_retries_total {}\n",
                "# TYPE recorder_database_retry_recoveries_total counter\n",
                "recorder_database_retry_recoveries_total {}\n",
                "# TYPE recorder_nats_reconnects_total counter\n",
                "recorder_nats_reconnects_total {}\n",
                "# TYPE recorder_jetstream_fetch_failures_total counter\n",
                "recorder_jetstream_fetch_failures_total {}\n",
                "# TYPE recorder_jetstream_ack_failures_total counter\n",
                "recorder_jetstream_ack_failures_total {}\n",
                "# TYPE recorder_jetstream_redeliveries_total counter\n",
                "recorder_jetstream_redeliveries_total {}\n",
                "# TYPE recorder_poison_messages_total counter\n",
                "recorder_poison_messages_total {}\n",
                "# TYPE recorder_batches_persisted_total counter\n",
                "recorder_batches_persisted_total {}\n",
                "# TYPE recorder_batch_messages gauge\n",
                "recorder_batch_messages {}\n",
                "# TYPE recorder_batch_messages_total counter\n",
                "recorder_batch_messages_total {}\n",
                "# TYPE recorder_batch_persist_latency gauge\n",
                "recorder_batch_persist_latency {:.9}\n",
                "# TYPE recorder_batch_persist_latency_seconds gauge\n",
                "recorder_batch_persist_latency_seconds {:.9}\n",
                "# TYPE recorder_consumer_pending_messages gauge\n",
                "recorder_consumer_pending_messages {}\n",
                "# TYPE recorder_consumer_ack_pending gauge\n",
                "recorder_consumer_ack_pending {}\n",
                "# TYPE recorder_readiness gauge\n",
                "recorder_readiness {}\n",
                "# TYPE recorder_last_persisted_feed_sequence gauge\n",
                "recorder_last_persisted_feed_sequence {}\n",
                "# TYPE recorder_last_persisted_feed_timestamp_ms gauge\n",
                "recorder_last_persisted_feed_timestamp_ms {}\n"
            ),
            self.inner.messages_received.load(Ordering::Relaxed),
            self.inner.messages_persisted.load(Ordering::Relaxed),
            self.inner.transactions_persisted.load(Ordering::Relaxed),
            self.inner.engine_outbox_inserted.load(Ordering::Relaxed),
            self.inner.duplicate_skips.load(Ordering::Relaxed),
            self.inner.decode_failures.load(Ordering::Relaxed),
            self.inner.database_failures.load(Ordering::Relaxed),
            self.inner.database_retries.load(Ordering::Relaxed),
            self.inner.database_retry_recoveries.load(Ordering::Relaxed),
            self.inner.nats_reconnects.load(Ordering::Relaxed),
            self.inner.jetstream_fetch_failures.load(Ordering::Relaxed),
            self.inner.jetstream_ack_failures.load(Ordering::Relaxed),
            self.inner.jetstream_redeliveries.load(Ordering::Relaxed),
            self.inner.poison_messages.load(Ordering::Relaxed),
            self.inner.batches_persisted.load(Ordering::Relaxed),
            self.inner.batch_messages_latest.load(Ordering::Relaxed),
            self.inner.batch_messages_total.load(Ordering::Relaxed),
            latency_seconds,
            latency_seconds,
            self.inner.consumer_pending.load(Ordering::Relaxed),
            self.inner.consumer_ack_pending.load(Ordering::Relaxed),
            ready,
            self.inner.last_sequence.load(Ordering::Relaxed),
            self.inner.last_timestamp_ms.load(Ordering::Relaxed),
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn renders_durable_delivery_batch_and_lag_metrics() {
        let metrics = Metrics::default();
        metrics.jetstream_fetch_failure();
        metrics.jetstream_ack_failure();
        metrics.jetstream_redelivery();
        metrics.database_retry();
        metrics.database_retry_recovered();
        metrics.batch_persisted(17, Duration::from_millis(25));
        metrics.set_consumer_state(ConsumerState {
            pending: 31,
            ack_pending: 7,
            redelivered: 1,
        });
        let rendered = metrics.render(&Readiness::new());
        assert!(rendered.contains("recorder_jetstream_fetch_failures_total 1"));
        assert!(rendered.contains("recorder_jetstream_ack_failures_total 1"));
        assert!(rendered.contains("recorder_jetstream_redeliveries_total 1"));
        assert!(rendered.contains("recorder_database_retries_total 1"));
        assert!(rendered.contains("recorder_database_retry_recoveries_total 1"));
        assert!(rendered.contains("recorder_batches_persisted_total 1"));
        assert!(rendered.contains("recorder_batch_messages 17"));
        assert!(rendered.contains("recorder_batch_persist_latency 0.025000000"));
        assert!(rendered.contains("recorder_batch_persist_latency_seconds 0.025000000"));
        assert!(rendered.contains("recorder_consumer_pending_messages 31"));
        assert!(rendered.contains("recorder_consumer_ack_pending 7"));
    }
}
