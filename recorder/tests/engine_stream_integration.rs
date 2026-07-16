use chrono::Utc;
use futures_util::StreamExt;
use phoenix_recorder::engine_outbox::OutboxRow;
use phoenix_recorder::engine_stream::{
    engine_stream_config, ensure_engine_pipeline, ensure_engine_stream, EnginePublisher,
    EngineStreamError, JetStreamEnginePublisher, ENGINE_DURABLE_NAME, ENGINE_STREAM_NAME,
    ENGINE_SUBJECT, HEADER_SCHEMA_VERSION, HEADER_SOURCE_IDENTITY,
};
use phoenix_recorder::model::{ENGINE_INPUT_SCHEMA_VERSION, NORMALIZED_SCHEMA_VERSION};
use serde_json::json;
use std::time::Duration;

fn local_nats_url() -> Option<String> {
    let url = std::env::var("PHOENIX_TEST_NATS_URL").ok()?;
    assert!(
        url.starts_with("nats://127.0.0.1:") || url.starts_with("nats://localhost:"),
        "integration test NATS URL must be loopback-only"
    );
    Some(url)
}

fn row() -> OutboxRow {
    let tx_hash = format!("0x{}", "d".repeat(64));
    let identity = format!("{ENGINE_INPUT_SCHEMA_VERSION}:77:{tx_hash}");
    OutboxRow {
        outbox_id: identity.clone(),
        schema_version: ENGINE_INPUT_SCHEMA_VERSION.to_string(),
        source_event_identity: identity,
        source_sequence: 77,
        tx_hash: tx_hash.clone(),
        chain_id: 42161,
        payload: json!({
            "schema_version": NORMALIZED_SCHEMA_VERSION,
            "sequence": 77,
            "tx_hash": tx_hash,
            "chain_id": 42161
        }),
        created_at: Utc::now(),
        publish_attempts: 1,
    }
}

#[tokio::test]
async fn engine_stream_creation_publication_deduplication_and_validation_are_real() {
    let Some(url) = local_nats_url() else {
        return;
    };
    let client = async_nats::connect(url).await.expect("connect local NATS");
    let context = async_nats::jetstream::new(client.clone());
    let _ = context.delete_stream(ENGINE_STREAM_NAME).await;

    ensure_engine_stream(&client)
        .await
        .expect("create Engine input stream");
    ensure_engine_stream(&client)
        .await
        .expect("idempotently verify Engine input stream");
    let consumer = ensure_engine_pipeline(&client)
        .await
        .expect("create Engine durable consumer");
    ensure_engine_pipeline(&client)
        .await
        .expect("idempotently verify Engine durable consumer");

    let publisher = JetStreamEnginePublisher::new(client.clone());
    let event = row();
    let first = publisher
        .publish(&event)
        .await
        .expect("publish Engine input");
    assert!(!first.duplicate);
    let duplicate = publisher
        .publish(&event)
        .await
        .expect("republish deterministic Engine input");
    assert!(duplicate.duplicate);
    assert_eq!(duplicate.stream_sequence, first.stream_sequence);

    let mut messages = consumer
        .batch()
        .max_messages(1)
        .expires(Duration::from_secs(1))
        .messages()
        .await
        .expect("fetch Engine input");
    let message = messages
        .next()
        .await
        .expect("Engine input delivery")
        .expect("valid Engine input delivery");
    assert_eq!(message.message.subject.as_str(), ENGINE_SUBJECT);
    let headers = message
        .message
        .headers
        .as_ref()
        .expect("Engine input headers");
    assert_eq!(
        headers
            .get(HEADER_SCHEMA_VERSION)
            .expect("schema header")
            .as_str(),
        ENGINE_INPUT_SCHEMA_VERSION
    );
    assert_eq!(
        headers
            .get(HEADER_SOURCE_IDENTITY)
            .expect("identity header")
            .as_str(),
        event.source_event_identity
    );
    message.double_ack().await.expect("ack Engine input");

    let stream = context
        .get_stream(ENGINE_STREAM_NAME)
        .await
        .expect("Engine stream remains available");
    let consumer = stream
        .get_consumer::<async_nats::jetstream::consumer::pull::Config>(ENGINE_DURABLE_NAME)
        .await
        .expect("Engine consumer remains available");
    assert_eq!(consumer.cached_info().num_pending, 0);

    context
        .delete_stream(ENGINE_STREAM_NAME)
        .await
        .expect("delete compatible integration stream");
    let mut incompatible = engine_stream_config();
    incompatible.max_bytes -= 1;
    context
        .create_stream(incompatible)
        .await
        .expect("create incompatible integration stream");
    assert_eq!(
        ensure_engine_stream(&client).await.unwrap_err(),
        EngineStreamError::StreamIncompatible
    );
    context
        .delete_stream(ENGINE_STREAM_NAME)
        .await
        .expect("clean integration stream");
}
