use crate::engine_outbox::OutboxRow;
use crate::model::{ENGINE_INPUT_SCHEMA_VERSION, MAX_MESSAGE_BYTES, NORMALIZED_SCHEMA_VERSION};
use async_nats::jetstream::consumer::{self, AckPolicy, DeliverPolicy, IntoConsumerConfig};
use async_nats::jetstream::context::Publish;
use async_nats::jetstream::stream::{
    Config as StreamConfig, DiscardPolicy, RetentionPolicy, StorageType,
};
use async_nats::jetstream::{self, stream::Stream};
use async_trait::async_trait;
use std::time::Duration;
use thiserror::Error;

pub const ENGINE_STREAM_NAME: &str = "PHOENIX_ENGINE_INPUT";
pub const ENGINE_SUBJECT: &str = "phoenix.engine.input";
pub const ENGINE_DURABLE_NAME: &str = "PHOENIX_ENGINE_SHADOW";
pub const ENGINE_STREAM_MAX_MESSAGES: i64 = 2_000_000;
pub const ENGINE_STREAM_MAX_BYTES: i64 = 1024 * 1024 * 1024;
pub const ENGINE_STREAM_MAX_AGE: Duration = Duration::from_secs(7 * 24 * 60 * 60);
pub const ENGINE_STREAM_MAX_MESSAGE_BYTES: i32 = 1024 * 1024;
pub const ENGINE_STREAM_DUPLICATE_WINDOW: Duration = Duration::from_secs(2 * 60);
pub const ENGINE_ACK_WAIT: Duration = Duration::from_secs(120);
pub const ENGINE_MAX_DELIVERIES: i64 = 20;
pub const ENGINE_MAX_ACK_PENDING: i64 = 512;
pub const ENGINE_PULL_BATCH: i64 = 64;
pub const ENGINE_FETCH_EXPIRY: Duration = Duration::from_secs(1);
pub const ENGINE_PUBLISH_TIMEOUT: Duration = Duration::from_secs(5);
pub const HEADER_SCHEMA_VERSION: &str = "Phoenix-Schema-Version";
pub const HEADER_SOURCE_IDENTITY: &str = "Phoenix-Source-Event-Identity";

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct EnginePublishReceipt {
    pub stream_sequence: u64,
    pub duplicate: bool,
}

#[derive(Clone, Debug, Error, PartialEq, Eq)]
pub enum EngineStreamError {
    #[error("required Engine JetStream stream is unavailable")]
    StreamUnavailable,
    #[error("existing Engine JetStream stream configuration is incompatible")]
    StreamIncompatible,
    #[error("required Engine durable consumer is unavailable")]
    ConsumerUnavailable,
    #[error("existing Engine durable consumer configuration is incompatible")]
    ConsumerIncompatible,
    #[error("Engine JetStream publication failed")]
    Publish,
    #[error("Engine outbox evidence failed integrity validation")]
    Integrity,
}

impl EngineStreamError {
    pub const fn class(&self) -> &'static str {
        match self {
            Self::StreamUnavailable => "stream_unavailable",
            Self::StreamIncompatible => "stream_incompatible",
            Self::ConsumerUnavailable => "consumer_unavailable",
            Self::ConsumerIncompatible => "consumer_incompatible",
            Self::Publish => "publish_failure",
            Self::Integrity => "event_integrity",
        }
    }

    pub const fn terminal(&self) -> bool {
        matches!(
            self,
            Self::StreamIncompatible | Self::ConsumerIncompatible | Self::Integrity
        )
    }
}

#[async_trait]
pub trait EnginePublisher: Send + Sync {
    async fn publish(&self, row: &OutboxRow) -> Result<EnginePublishReceipt, EngineStreamError>;
}

#[derive(Clone)]
pub struct JetStreamEnginePublisher {
    context: jetstream::Context,
}

impl JetStreamEnginePublisher {
    pub fn new(client: async_nats::Client) -> Self {
        let mut context = jetstream::new(client);
        context.set_timeout(ENGINE_PUBLISH_TIMEOUT);
        Self { context }
    }
}

#[async_trait]
impl EnginePublisher for JetStreamEnginePublisher {
    async fn publish(&self, row: &OutboxRow) -> Result<EnginePublishReceipt, EngineStreamError> {
        let payload = validate_and_encode(row)?;
        let publish = Publish::build()
            .payload(payload.into())
            .message_id(&row.outbox_id)
            .header(HEADER_SCHEMA_VERSION, row.schema_version.as_str())
            .header(HEADER_SOURCE_IDENTITY, row.source_event_identity.as_str());
        let acknowledgement = self
            .context
            .send_publish(ENGINE_SUBJECT, publish)
            .await
            .map_err(|_| EngineStreamError::Publish)?
            .await
            .map_err(|_| EngineStreamError::Publish)?;
        if acknowledgement.stream != ENGINE_STREAM_NAME || acknowledgement.sequence == 0 {
            return Err(EngineStreamError::Integrity);
        }
        Ok(EnginePublishReceipt {
            stream_sequence: acknowledgement.sequence,
            duplicate: acknowledgement.duplicate,
        })
    }
}

pub async fn ensure_engine_stream(
    client: &async_nats::Client,
) -> Result<Stream, EngineStreamError> {
    let context = jetstream::new(client.clone());
    let expected = engine_stream_config();
    let stream = context
        .get_or_create_stream(expected.clone())
        .await
        .map_err(|_| EngineStreamError::StreamUnavailable)?;
    if engine_stream_config_matches(&stream.cached_info().config, &expected) {
        Ok(stream)
    } else {
        Err(EngineStreamError::StreamIncompatible)
    }
}

pub async fn ensure_engine_pipeline(
    client: &async_nats::Client,
) -> Result<consumer::PullConsumer, EngineStreamError> {
    let stream = ensure_engine_stream(client).await?;
    let expected = engine_consumer_config();
    let expected_generic = expected.clone().into_consumer_config();
    let consumer = stream
        .get_or_create_consumer(ENGINE_DURABLE_NAME, expected)
        .await
        .map_err(|_| EngineStreamError::ConsumerUnavailable)?;
    if engine_consumer_config_matches(&consumer.cached_info().config, &expected_generic) {
        Ok(consumer)
    } else {
        Err(EngineStreamError::ConsumerIncompatible)
    }
}

pub fn engine_stream_config() -> StreamConfig {
    StreamConfig {
        name: ENGINE_STREAM_NAME.to_string(),
        description: Some("Durable recorded inputs for Phoenix Engine SHADOW evidence".to_string()),
        subjects: vec![ENGINE_SUBJECT.to_string()],
        retention: RetentionPolicy::WorkQueue,
        max_consumers: 1,
        max_messages: ENGINE_STREAM_MAX_MESSAGES,
        max_bytes: ENGINE_STREAM_MAX_BYTES,
        max_messages_per_subject: -1,
        discard: DiscardPolicy::New,
        max_age: ENGINE_STREAM_MAX_AGE,
        max_message_size: ENGINE_STREAM_MAX_MESSAGE_BYTES,
        storage: StorageType::File,
        num_replicas: 1,
        duplicate_window: ENGINE_STREAM_DUPLICATE_WINDOW,
        ..Default::default()
    }
}

pub fn engine_consumer_config() -> consumer::pull::Config {
    consumer::pull::Config {
        durable_name: Some(ENGINE_DURABLE_NAME.to_string()),
        name: Some(ENGINE_DURABLE_NAME.to_string()),
        description: Some("Phoenix Engine durable SHADOW evidence consumer".to_string()),
        deliver_policy: DeliverPolicy::All,
        ack_policy: AckPolicy::Explicit,
        ack_wait: ENGINE_ACK_WAIT,
        max_deliver: ENGINE_MAX_DELIVERIES,
        filter_subject: ENGINE_SUBJECT.to_string(),
        max_waiting: 2,
        max_ack_pending: ENGINE_MAX_ACK_PENDING,
        max_batch: ENGINE_PULL_BATCH,
        max_bytes: ENGINE_PULL_BATCH * ENGINE_STREAM_MAX_MESSAGE_BYTES as i64,
        max_expires: ENGINE_FETCH_EXPIRY,
        num_replicas: 1,
        memory_storage: false,
        ..Default::default()
    }
}

pub fn engine_stream_config_matches(actual: &StreamConfig, expected: &StreamConfig) -> bool {
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

pub fn engine_consumer_config_matches(
    actual: &consumer::Config,
    expected: &consumer::Config,
) -> bool {
    actual.durable_name == expected.durable_name
        && actual.name == expected.name
        && actual.deliver_policy == expected.deliver_policy
        && actual.ack_policy == expected.ack_policy
        && actual.ack_wait == expected.ack_wait
        && actual.max_deliver == expected.max_deliver
        && actual.filter_subject == expected.filter_subject
        && actual.max_waiting == expected.max_waiting
        && actual.max_ack_pending == expected.max_ack_pending
        && actual.max_batch == expected.max_batch
        && actual.max_bytes == expected.max_bytes
        && actual.max_expires == expected.max_expires
        && actual.num_replicas == expected.num_replicas
        && actual.memory_storage == expected.memory_storage
}

fn validate_and_encode(row: &OutboxRow) -> Result<Vec<u8>, EngineStreamError> {
    let expected_identity = format!(
        "{}:{}:{}",
        ENGINE_INPUT_SCHEMA_VERSION, row.source_sequence, row.tx_hash
    );
    if row.schema_version != ENGINE_INPUT_SCHEMA_VERSION
        || row.outbox_id != row.source_event_identity
        || row.source_event_identity != expected_identity
        || row.chain_id != 42161
        || row
            .payload
            .get("schema_version")
            .and_then(|value| value.as_str())
            != Some(NORMALIZED_SCHEMA_VERSION)
        || row.payload.get("sequence").and_then(|value| value.as_u64()) != Some(row.source_sequence)
        || row.payload.get("tx_hash").and_then(|value| value.as_str()) != Some(row.tx_hash.as_str())
        || row.payload.get("chain_id").and_then(|value| value.as_u64()) != Some(row.chain_id)
    {
        return Err(EngineStreamError::Integrity);
    }
    let payload = serde_json::to_vec(&row.payload).map_err(|_| EngineStreamError::Integrity)?;
    if payload.len() > MAX_MESSAGE_BYTES {
        return Err(EngineStreamError::Integrity);
    }
    Ok(payload)
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::Utc;
    use serde_json::json;

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

    #[test]
    fn stream_and_consumer_contracts_are_bounded_and_stable() {
        let stream = engine_stream_config();
        assert!(engine_stream_config_matches(&stream, &stream));
        assert_eq!(stream.name, ENGINE_STREAM_NAME);
        assert_eq!(stream.subjects, vec![ENGINE_SUBJECT]);
        assert_eq!(stream.retention, RetentionPolicy::WorkQueue);
        assert_eq!(stream.storage, StorageType::File);
        assert_eq!(stream.max_messages, ENGINE_STREAM_MAX_MESSAGES);
        assert_eq!(stream.max_bytes, ENGINE_STREAM_MAX_BYTES);
        assert_eq!(stream.max_age, ENGINE_STREAM_MAX_AGE);
        assert_eq!(stream.duplicate_window, ENGINE_STREAM_DUPLICATE_WINDOW);

        let consumer = engine_consumer_config();
        assert_eq!(consumer.durable_name.as_deref(), Some(ENGINE_DURABLE_NAME));
        assert_eq!(consumer.ack_policy, AckPolicy::Explicit);
        assert_eq!(consumer.ack_wait, ENGINE_ACK_WAIT);
        assert_eq!(consumer.max_deliver, ENGINE_MAX_DELIVERIES);
        assert_eq!(consumer.max_ack_pending, ENGINE_MAX_ACK_PENDING);
        assert_eq!(consumer.max_batch, ENGINE_PULL_BATCH);
        assert_eq!(consumer.max_expires, ENGINE_FETCH_EXPIRY);

        let server_config = include_str!("../../deploy/nats-server.conf");
        assert!(server_config.contains("max_payload: 1MB"));
        assert!(server_config.contains("duplicate_window: \"2m\""));
        assert_eq!(ENGINE_STREAM_MAX_MESSAGE_BYTES, 1024 * 1024);
        assert_eq!(
            ENGINE_STREAM_DUPLICATE_WINDOW,
            crate::jetstream::STREAM_DUPLICATE_WINDOW
        );
    }

    #[test]
    fn incompatible_stream_configuration_is_detected_without_mutation() {
        let expected = engine_stream_config();
        let mut actual = expected.clone();
        actual.retention = RetentionPolicy::Limits;
        assert!(!engine_stream_config_matches(&actual, &expected));

        let expected = engine_consumer_config().into_consumer_config();
        let mut actual = expected.clone();
        actual.ack_policy = AckPolicy::None;
        assert!(!engine_consumer_config_matches(&actual, &expected));
    }

    #[test]
    fn canonical_payload_and_identity_must_match_outbox_columns() {
        assert!(validate_and_encode(&row()).is_ok());
        let mut invalid = row();
        invalid.payload["sequence"] = serde_json::Value::from(8);
        assert_eq!(
            validate_and_encode(&invalid),
            Err(EngineStreamError::Integrity)
        );
    }

    #[test]
    fn stream_errors_are_sanitized() {
        for error in [
            EngineStreamError::StreamUnavailable,
            EngineStreamError::StreamIncompatible,
            EngineStreamError::ConsumerUnavailable,
            EngineStreamError::ConsumerIncompatible,
            EngineStreamError::Publish,
            EngineStreamError::Integrity,
        ] {
            let rendered = error.to_string().to_ascii_lowercase();
            assert!(!rendered.contains("nats://"));
            assert!(!rendered.contains("password"));
            assert!(error.class().len() <= 64);
        }
    }
}
