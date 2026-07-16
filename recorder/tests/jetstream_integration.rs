use phoenix_recorder::jetstream::{
    ensure_durable_pipeline, MessageFetcher, DURABLE_CONSUMER_NAME, STREAM_NAME,
};
use phoenix_recorder::model::{ARBITRUM_ONE_CHAIN_ID, NORMALIZED_SCHEMA_VERSION};
use phoenix_recorder::NATS_SUBJECT;
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

fn payload(sequence: u64, hash_byte: char) -> Vec<u8> {
    serde_json::to_vec(&json!({
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
    .expect("serialize integration payload")
}

#[tokio::test]
async fn real_stream_consumer_publish_fetch_redelivery_and_ack() {
    let Some(url) = local_nats_url() else {
        return;
    };
    let client = async_nats::connect(url).await.expect("connect local NATS");
    let context = async_nats::jetstream::new(client.clone());
    let _ = context.delete_stream(STREAM_NAME).await;

    let first = ensure_durable_pipeline(&client)
        .await
        .expect("create durable pipeline");
    let second = ensure_durable_pipeline(&client)
        .await
        .expect("idempotently verify durable pipeline");
    assert_eq!(
        second
            .state()
            .await
            .expect("read durable consumer state")
            .pending,
        0
    );

    context
        .publish(NATS_SUBJECT, payload(1, 'a').into())
        .await
        .expect("send first publish")
        .await
        .expect("receive first persistence acknowledgement");
    let first_delivery = first
        .fetch_batch(10, Duration::from_millis(100))
        .await
        .expect("durable pull fetch")
        .pop()
        .expect("first delivery");
    assert_eq!(first_delivery.delivery_count, 1);
    first_delivery
        .acker
        .nak(Duration::from_millis(10))
        .await
        .expect("request redelivery");

    tokio::time::sleep(Duration::from_millis(25)).await;
    let replay = second
        .fetch_batch(10, Duration::from_millis(250))
        .await
        .expect("fetch redelivery")
        .pop()
        .expect("redelivered message");
    assert!(replay.delivery_count >= 2);
    replay
        .acker
        .ack_confirmed()
        .await
        .expect("confirm durable acknowledgement");

    tokio::time::sleep(Duration::from_millis(25)).await;
    let state = second.state().await.expect("read acknowledged state");
    assert_eq!(state.pending, 0);
    assert_eq!(state.ack_pending, 0);

    let stream = context
        .get_stream(STREAM_NAME)
        .await
        .expect("stream remains available");
    let consumer = stream
        .get_consumer::<async_nats::jetstream::consumer::pull::Config>(DURABLE_CONSUMER_NAME)
        .await
        .expect("durable consumer remains available");
    assert_eq!(
        consumer.cached_info().config.durable_name.as_deref(),
        Some(DURABLE_CONSUMER_NAME)
    );

    context
        .delete_stream(STREAM_NAME)
        .await
        .expect("clean local integration stream");
}
