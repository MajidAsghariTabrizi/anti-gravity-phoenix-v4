use async_nats::jetstream::{self, consumer, AckKind};
use async_trait::async_trait;
use futures_util::StreamExt;
use phoenix_recorder::engine_stream::{
    EngineStreamError, ENGINE_FETCH_EXPIRY, ENGINE_PULL_BATCH, ENGINE_STREAM_MAX_MESSAGE_BYTES,
    HEADER_SCHEMA_VERSION, HEADER_SOURCE_IDENTITY,
};
use std::sync::Arc;
use std::time::Duration;
use thiserror::Error;

pub const RETRY_DELAY: Duration = Duration::from_secs(1);
pub const MAX_FETCH_BYTES: usize =
    ENGINE_PULL_BATCH as usize * ENGINE_STREAM_MAX_MESSAGE_BYTES as usize;

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct ConsumerState {
    pub pending: u64,
    pub ack_pending: u64,
    pub redelivered: u64,
}

#[derive(Clone, Debug, Error, PartialEq, Eq)]
pub enum PipelineError {
    #[error("Engine JetStream pipeline is unavailable")]
    Pipeline,
    #[error("Engine JetStream durable fetch failed")]
    Fetch,
    #[error("Engine JetStream message metadata is invalid")]
    Metadata,
    #[error("Engine JetStream acknowledgement failed")]
    Acknowledgement,
}

impl From<EngineStreamError> for PipelineError {
    fn from(_value: EngineStreamError) -> Self {
        Self::Pipeline
    }
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
    pub schema_header: Option<String>,
    pub identity_header: Option<String>,
    pub stream_sequence: u64,
    pub delivery_count: u64,
    pub acker: Arc<dyn DeliveryAcker>,
}

impl std::fmt::Debug for Delivery {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("Delivery")
            .field("payload_bytes", &self.payload.len())
            .field("schema_header_present", &self.schema_header.is_some())
            .field("identity_header_present", &self.identity_header.is_some())
            .field("stream_sequence", &self.stream_sequence)
            .field("delivery_count", &self.delivery_count)
            .finish_non_exhaustive()
    }
}

#[async_trait]
pub trait MessageFetcher: Send + Sync {
    async fn fetch_batch(&self) -> Result<Vec<Delivery>, PipelineError>;
    async fn state(&self) -> Result<ConsumerState, PipelineError>;
}

#[derive(Clone, Debug)]
pub struct JetStreamFetcher {
    consumer: consumer::PullConsumer,
}

impl JetStreamFetcher {
    pub fn new(consumer: consumer::PullConsumer) -> Self {
        Self { consumer }
    }
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
impl MessageFetcher for JetStreamFetcher {
    async fn fetch_batch(&self) -> Result<Vec<Delivery>, PipelineError> {
        let mut messages = self
            .consumer
            .batch()
            .max_messages(ENGINE_PULL_BATCH as usize)
            .max_bytes(MAX_FETCH_BYTES)
            .expires(ENGINE_FETCH_EXPIRY)
            .messages()
            .await
            .map_err(|_| PipelineError::Fetch)?;
        let mut deliveries = Vec::with_capacity(ENGINE_PULL_BATCH as usize);
        while let Some(result) = messages.next().await {
            let message = result.map_err(|_| PipelineError::Fetch)?;
            let info = message.info().map_err(|_| PipelineError::Metadata)?;
            if info.stream_sequence == 0 || info.delivered <= 0 {
                return Err(PipelineError::Metadata);
            }
            let schema_header = message
                .message
                .headers
                .as_ref()
                .and_then(|headers| headers.get(HEADER_SCHEMA_VERSION))
                .map(|value| value.as_str().to_string());
            let identity_header = message
                .message
                .headers
                .as_ref()
                .and_then(|headers| headers.get(HEADER_SOURCE_IDENTITY))
                .map(|value| value.as_str().to_string());
            let payload = message.message.payload.to_vec();
            deliveries.push(Delivery {
                payload,
                schema_header,
                identity_header,
                stream_sequence: info.stream_sequence,
                delivery_count: info.delivered as u64,
                acker: Arc::new(JetStreamAcker { message }),
            });
        }
        Ok(deliveries)
    }

    async fn state(&self) -> Result<ConsumerState, PipelineError> {
        let mut consumer = self.consumer.clone();
        let info = consumer.info().await.map_err(|_| PipelineError::Pipeline)?;
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
    use phoenix_recorder::engine_stream::ENGINE_MAX_DELIVERIES;

    #[test]
    fn pull_contract_stays_bounded_by_the_committed_consumer_contract() {
        assert_eq!(ENGINE_PULL_BATCH, 64);
        assert_eq!(ENGINE_MAX_DELIVERIES, 20);
        assert_eq!(MAX_FETCH_BYTES, 64 * 1024 * 1024);
        assert_eq!(RETRY_DELAY, Duration::from_secs(1));
    }

    #[test]
    fn pipeline_errors_do_not_expose_connection_or_message_material() {
        for error in [
            PipelineError::Pipeline,
            PipelineError::Fetch,
            PipelineError::Metadata,
            PipelineError::Acknowledgement,
        ] {
            let rendered = error.to_string().to_ascii_lowercase();
            for forbidden in ["nats://", "password", "raw_tx", "payload="] {
                assert!(!rendered.contains(forbidden));
            }
        }
    }
}
