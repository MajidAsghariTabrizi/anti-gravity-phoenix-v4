use crate::state::Readiness;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;

#[derive(Clone, Debug, Default)]
pub struct Metrics {
    inner: Arc<MetricValues>,
}

#[derive(Debug, Default)]
struct MetricValues {
    messages_received: AtomicU64,
    messages_persisted: AtomicU64,
    transactions_persisted: AtomicU64,
    duplicate_skips: AtomicU64,
    decode_failures: AtomicU64,
    database_failures: AtomicU64,
    nats_reconnects: AtomicU64,
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

    pub fn duplicate_skip(&self) {
        self.inner.duplicate_skips.fetch_add(1, Ordering::Relaxed);
    }

    pub fn decode_failure(&self) {
        self.inner.decode_failures.fetch_add(1, Ordering::Relaxed);
    }

    pub fn database_failure(&self) {
        self.inner.database_failures.fetch_add(1, Ordering::Relaxed);
    }

    pub fn nats_reconnect(&self) {
        self.inner.nats_reconnects.fetch_add(1, Ordering::Relaxed);
    }

    pub fn set_last_persisted(&self, sequence: u64, timestamp_ms: u64) {
        self.inner.last_sequence.store(sequence, Ordering::Relaxed);
        self.inner
            .last_timestamp_ms
            .store(timestamp_ms, Ordering::Relaxed);
    }

    pub fn render(&self, readiness: &Readiness) -> String {
        let ready = u8::from(readiness.ready().is_ok());
        format!(
            concat!(
                "# TYPE recorder_messages_received_total counter\n",
                "recorder_messages_received_total {}\n",
                "# TYPE recorder_messages_persisted_total counter\n",
                "recorder_messages_persisted_total {}\n",
                "# TYPE recorder_transactions_persisted_total counter\n",
                "recorder_transactions_persisted_total {}\n",
                "# TYPE recorder_duplicate_skips_total counter\n",
                "recorder_duplicate_skips_total {}\n",
                "# TYPE recorder_decode_failures_total counter\n",
                "recorder_decode_failures_total {}\n",
                "# TYPE recorder_database_failures_total counter\n",
                "recorder_database_failures_total {}\n",
                "# TYPE recorder_nats_reconnects_total counter\n",
                "recorder_nats_reconnects_total {}\n",
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
            self.inner.duplicate_skips.load(Ordering::Relaxed),
            self.inner.decode_failures.load(Ordering::Relaxed),
            self.inner.database_failures.load(Ordering::Relaxed),
            self.inner.nats_reconnects.load(Ordering::Relaxed),
            ready,
            self.inner.last_sequence.load(Ordering::Relaxed),
            self.inner.last_timestamp_ms.load(Ordering::Relaxed),
        )
    }
}
