use crate::NATS_SUBJECT;
use async_nats::jetstream::consumer::{self, AckPolicy, DeliverPolicy, IntoConsumerConfig};
use async_nats::jetstream::stream::{
    Config as StreamConfig, DiscardPolicy, RetentionPolicy, StorageType,
};
use async_nats::jetstream::{self, AckKind};
use async_trait::async_trait;
use futures_util::StreamExt;
use std::sync::Arc;
use std::time::Duration;
use thiserror::Error;

pub const STREAM_NAME: &str = "PHOENIX_FEED_TX";
pub const DURABLE_CONSUMER_NAME: &str = "PHOENIX_RECORDER";
pub const STREAM_MAX_MESSAGES: i64 = 5_000_000;
pub const STREAM_MAX_BYTES: i64 = 2 * 1024 * 1024 * 1024;
pub const STREAM_MAX_AGE: Duration = Duration::from_secs(24 * 60 * 60);
pub const STREAM_MAX_MESSAGE_BYTES: i32 = 1024 * 1024;
pub const STREAM_DUPLICATE_WINDOW: Duration = Duration::from_secs(2 * 60);
pub const CONSUMER_ACK_WAIT: Duration = Duration::from_secs(60);
pub const CONSUMER_MAX_DELIVERIES: i64 = 5;
pub const CONSUMER_MAX_ACK_PENDING: i64 = 1024;
pub const CONSUMER_MAX_BATCH: i64 = 256;
pub const CONSUMER_MAX_BATCH_BYTES: i64 = 32 * 1024 * 1024;
pub const CONSUMER_MAX_EXPIRES: Duration = Duration::from_secs(1);
pub const POISON_REDELIVERY_DELAY: Duration = Duration::from_secs(1);

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct ConsumerState {
    pub pending: u64,
    pub ack_pending: u64,
    pub redelivered: u64,
}

#[derive(Clone, Debug, Error, PartialEq, Eq)]
pub enum PipelineError {
    #[error("required JetStream stream is unavailable")]
    StreamUnavailable,
    #[error("required JetStream durable consumer is unavailable")]
    ConsumerUnavailable,
    #[error("JetStream durable fetch failed")]
    Fetch,
    #[error("JetStream message metadata is invalid")]
    Metadata,
    #[error("JetStream acknowledgement failed")]
    Acknowledgement,
}

#[async_trait]
pub trait DeliveryAcker: Send + Sync {
    async fn ack_confirmed(&self) -> Result<(), PipelineError>;
    async fn nak(&self, delay: Duration) -> Result<(), PipelineError>;
    async fn progress(&self) -> Result<(), PipelineError>;
    async fn term(&self) -> Result<(), PipelineError>;
}

#[derive(Clone)]
pub struct Delivery {
    pub payload: Vec<u8>,
    pub delivery_count: u64,
    pub acker: Arc<dyn DeliveryAcker>,
}

impl std::fmt::Debug for Delivery {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("Delivery")
            .field("payload_bytes", &self.payload.len())
            .field("delivery_count", &self.delivery_count)
            .finish_non_exhaustive()
    }
}

#[async_trait]
pub trait MessageFetcher: Send + Sync {
    async fn fetch_batch(
        &self,
        max_messages: usize,
        max_wait: Duration,
    ) -> Result<Vec<Delivery>, PipelineError>;
    async fn state(&self) -> Result<ConsumerState, PipelineError>;
}

#[derive(Clone, Debug)]
pub struct JetStreamPullConsumer {
    consumer: consumer::PullConsumer,
}

pub async fn ensure_durable_pipeline(
    client: &async_nats::Client,
) -> Result<JetStreamPullConsumer, PipelineError> {
    let context = jetstream::new(client.clone());
    let expected_stream = stream_config();
    let mut stream = context
        .get_or_create_stream(expected_stream.clone())
        .await
        .map_err(|_| PipelineError::StreamUnavailable)?;

    if !stream_config_matches(&stream.cached_info().config, &expected_stream) {
        context
            .update_stream(&expected_stream)
            .await
            .map_err(|_| PipelineError::StreamUnavailable)?;
        stream = context
            .get_stream(STREAM_NAME)
            .await
            .map_err(|_| PipelineError::StreamUnavailable)?;
    }

    let expected_consumer = consumer_config();
    let expected_generic = expected_consumer.clone().into_consumer_config();
    let mut consumer = stream
        .get_or_create_consumer(DURABLE_CONSUMER_NAME, expected_consumer.clone())
        .await
        .map_err(|_| PipelineError::ConsumerUnavailable)?;
    if consumer.cached_info().config != expected_generic {
        consumer = stream
            .update_consumer(expected_consumer)
            .await
            .map_err(|_| PipelineError::ConsumerUnavailable)?;
    }

    Ok(JetStreamPullConsumer { consumer })
}

pub fn stream_config() -> StreamConfig {
    StreamConfig {
        name: STREAM_NAME.to_string(),
        description: Some(
            "Durable normalized Arbitrum transactions for the Phoenix Recorder".to_string(),
        ),
        subjects: vec![NATS_SUBJECT.to_string()],
        retention: RetentionPolicy::WorkQueue,
        max_consumers: 1,
        max_messages: STREAM_MAX_MESSAGES,
        max_bytes: STREAM_MAX_BYTES,
        max_messages_per_subject: -1,
        discard: DiscardPolicy::New,
        max_age: STREAM_MAX_AGE,
        max_message_size: STREAM_MAX_MESSAGE_BYTES,
        storage: StorageType::File,
        num_replicas: 1,
        duplicate_window: STREAM_DUPLICATE_WINDOW,
        ..Default::default()
    }
}

pub fn consumer_config() -> consumer::pull::Config {
    consumer::pull::Config {
        durable_name: Some(DURABLE_CONSUMER_NAME.to_string()),
        name: Some(DURABLE_CONSUMER_NAME.to_string()),
        description: Some("Phoenix Recorder durable PostgreSQL persistence consumer".to_string()),
        deliver_policy: DeliverPolicy::All,
        ack_policy: AckPolicy::Explicit,
        ack_wait: CONSUMER_ACK_WAIT,
        max_deliver: CONSUMER_MAX_DELIVERIES,
        filter_subject: NATS_SUBJECT.to_string(),
        max_waiting: 2,
        max_ack_pending: CONSUMER_MAX_ACK_PENDING,
        max_batch: CONSUMER_MAX_BATCH,
        max_bytes: CONSUMER_MAX_BATCH_BYTES,
        max_expires: CONSUMER_MAX_EXPIRES,
        num_replicas: 1,
        memory_storage: false,
        ..Default::default()
    }
}

fn stream_config_matches(actual: &StreamConfig, expected: &StreamConfig) -> bool {
    actual.name == expected.name
        && actual.description == expected.description
        && actual.subjects == expected.subjects
        && actual.retention == expected.retention
        && actual.max_consumers == expected.max_consumers
        && actual.max_messages == expected.max_messages
        && actual.max_bytes == expected.max_bytes
        && actual.max_messages_per_subject == expected.max_messages_per_subject
        && actual.discard == expected.discard
        && actual.max_age == expected.max_age
        && actual.max_message_size == expected.max_message_size
        && actual.storage == expected.storage
        && actual.num_replicas == expected.num_replicas
        && actual.duplicate_window == expected.duplicate_window
}

#[derive(Clone, Debug)]
struct JetStreamAcker {
    message: jetstream::Message,
}

#[async_trait]
impl DeliveryAcker for JetStreamAcker {
    async fn ack_confirmed(&self) -> Result<(), PipelineError> {
        self.message
            .double_ack()
            .await
            .map_err(|_| PipelineError::Acknowledgement)
    }

    async fn nak(&self, delay: Duration) -> Result<(), PipelineError> {
        self.message
            .ack_with(AckKind::Nak(Some(delay)))
            .await
            .map_err(|_| PipelineError::Acknowledgement)
    }

    async fn progress(&self) -> Result<(), PipelineError> {
        self.message
            .ack_with(AckKind::Progress)
            .await
            .map_err(|_| PipelineError::Acknowledgement)
    }

    async fn term(&self) -> Result<(), PipelineError> {
        self.message
            .ack_with(AckKind::Term)
            .await
            .map_err(|_| PipelineError::Acknowledgement)
    }
}

#[async_trait]
impl MessageFetcher for JetStreamPullConsumer {
    async fn fetch_batch(
        &self,
        max_messages: usize,
        max_wait: Duration,
    ) -> Result<Vec<Delivery>, PipelineError> {
        if max_messages == 0 || max_messages > CONSUMER_MAX_BATCH as usize {
            return Err(PipelineError::Fetch);
        }
        let mut messages = self
            .consumer
            .batch()
            .max_messages(max_messages)
            .max_bytes(CONSUMER_MAX_BATCH_BYTES as usize)
            .expires(max_wait)
            .messages()
            .await
            .map_err(|_| PipelineError::Fetch)?;
        let mut deliveries = Vec::with_capacity(max_messages);
        while let Some(result) = messages.next().await {
            let message = result.map_err(|_| PipelineError::Fetch)?;
            let delivery_count = message
                .info()
                .map_err(|_| PipelineError::Metadata)?
                .delivered;
            if delivery_count <= 0 {
                return Err(PipelineError::Metadata);
            }
            let payload = message.message.payload.to_vec();
            deliveries.push(Delivery {
                payload,
                delivery_count: delivery_count as u64,
                acker: Arc::new(JetStreamAcker { message }),
            });
        }
        Ok(deliveries)
    }

    async fn state(&self) -> Result<ConsumerState, PipelineError> {
        let mut consumer = self.consumer.clone();
        let info = consumer
            .info()
            .await
            .map_err(|_| PipelineError::ConsumerUnavailable)?;
        Ok(ConsumerState {
            pending: info.num_pending,
            ack_pending: info.num_ack_pending as u64,
            redelivered: info.num_redelivered as u64,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn stream_configuration_is_stable_bounded_and_file_backed() {
        let first = stream_config();
        let second = stream_config();
        assert_eq!(first, second);
        assert!(stream_config_matches(&first, &second));
        assert_eq!(first.name, STREAM_NAME);
        assert_eq!(first.subjects, vec![NATS_SUBJECT]);
        assert_eq!(first.retention, RetentionPolicy::WorkQueue);
        assert_eq!(first.discard, DiscardPolicy::New);
        assert_eq!(first.storage, StorageType::File);
        assert_eq!(first.max_messages, STREAM_MAX_MESSAGES);
        assert_eq!(first.max_bytes, STREAM_MAX_BYTES);
        assert_eq!(first.max_age, STREAM_MAX_AGE);
        assert_eq!(first.duplicate_window, STREAM_DUPLICATE_WINDOW);
    }

    #[test]
    fn durable_consumer_configuration_is_stable_for_idempotent_updates() {
        let first = consumer_config();
        let second = consumer_config();
        assert_eq!(first, second);
        assert_eq!(first.durable_name.as_deref(), Some(DURABLE_CONSUMER_NAME));
        assert_eq!(first.filter_subject, NATS_SUBJECT);
        assert_eq!(first.ack_policy, AckPolicy::Explicit);
        assert_eq!(first.ack_wait, CONSUMER_ACK_WAIT);
        assert_eq!(first.max_deliver, CONSUMER_MAX_DELIVERIES);
        assert_eq!(first.max_ack_pending, CONSUMER_MAX_ACK_PENDING);
        assert_eq!(first.max_batch, CONSUMER_MAX_BATCH);
        assert_eq!(first.max_bytes, CONSUMER_MAX_BATCH_BYTES);
    }

    #[test]
    fn pipeline_errors_are_sanitized() {
        for error in [
            PipelineError::StreamUnavailable,
            PipelineError::ConsumerUnavailable,
            PipelineError::Fetch,
            PipelineError::Metadata,
            PipelineError::Acknowledgement,
        ] {
            let rendered = error.to_string().to_ascii_lowercase();
            assert!(!rendered.contains("nats://"));
            assert!(!rendered.contains("password"));
            assert!(!rendered.contains("raw_tx"));
        }
    }

    #[test]
    fn live_smoke_contains_durable_replay_and_failure_gates() {
        let script = include_str!("../../scripts/recorder-live-smoke.sh");
        for required in [
            "RECORDER_SMOKE_OBSERVATION_SECONDS",
            "feed_jetstream_publish_success_total",
            "recorder_jetstream_ack_failures_total",
            "jetstream_value stream_exists",
            "jetstream_value consumer_exists",
            "compose stop recorder",
            "queued JetStream messages were not replayed",
            "duplicate_group_count origin_transactions",
            "slow consumer|core_nats_message_drop",
        ] {
            assert!(script.contains(required), "missing smoke gate: {required}");
        }
    }
}
