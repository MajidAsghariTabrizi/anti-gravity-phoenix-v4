use money_path_classifier::{
    MoneyPathClassifier, ADMISSION_POLICY_VERSION, REVIEWED_ROUTER_ADDRESSES,
    SWAP_ROUTER_02_ADDRESS,
};
use phoenix_recorder::ingress::{IngressBuffer, IngressBufferConfig};
use phoenix_recorder::jetstream::{
    ensure_durable_pipeline, MessageFetcher, DURABLE_CONSUMER_NAME, STREAM_NAME,
};
use phoenix_recorder::logging::LogSampler;
use phoenix_recorder::metrics::Metrics;
use phoenix_recorder::model::{ARBITRUM_ONE_CHAIN_ID, NORMALIZED_SCHEMA_VERSION};
use phoenix_recorder::persistence::{EventStore, PostgresStore};
use phoenix_recorder::runtime::{
    consume_durable_messages, BatchConfig, ConsumerExit, PrePersistenceClassifier, RetryPolicy,
};
use phoenix_recorder::state::Readiness;
use phoenix_recorder::NATS_SUBJECT;
use serde_json::json;
use sqlx::{PgPool, Row};
use std::sync::{Arc, OnceLock};
use std::time::Duration;
use tokio::sync::Mutex;
use tokio_util::sync::CancellationToken;

const ROUTES: &str = include_str!("../../fixtures/routes/weth_usdc_uniswap_v3.json");
const WETH: &str = "0x82af49447d8a07e3bd95bd0d56f35241523fbab1";
const USDC: &str = "0xaf88d065e77c8cc2239327c5edb3a432268e5831";

fn integration_lock() -> &'static Mutex<()> {
    static LOCK: OnceLock<Mutex<()>> = OnceLock::new();
    LOCK.get_or_init(|| Mutex::new(()))
}

fn local_nats_url() -> Option<String> {
    let url = std::env::var("PHOENIX_TEST_NATS_URL").ok()?;
    assert!(
        url.starts_with("nats://127.0.0.1:") || url.starts_with("nats://localhost:"),
        "integration test NATS URL must be loopback-only"
    );
    Some(url)
}

fn local_postgres_dsn() -> Option<String> {
    let dsn = std::env::var("PHOENIX_TEST_POSTGRES_DSN").ok()?;
    assert!(
        dsn.contains("@127.0.0.1:") || dsn.contains("@localhost:"),
        "integration test PostgreSQL URL must be loopback-only"
    );
    Some(dsn)
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

fn address(value: &str) -> ethabi::Token {
    ethabi::Token::Address(ethabi::ethereum_types::H160::from_slice(
        &hex::decode(value.trim_start_matches("0x")).expect("fixture address"),
    ))
}

fn relevant_calldata() -> String {
    use ethabi::ethereum_types::U256;
    use ethabi::{ParamType, Token};

    let tuple = ParamType::Tuple(vec![
        ParamType::Address,
        ParamType::Address,
        ParamType::Uint(24),
        ParamType::Address,
        ParamType::Uint(256),
        ParamType::Uint(256),
        ParamType::Uint(160),
    ]);
    let mut bytes = ethabi::short_signature("exactInputSingle", &[tuple]).to_vec();
    bytes.extend(ethabi::encode(&[Token::Tuple(vec![
        address(WETH),
        address(USDC),
        Token::Uint(U256::from(500_u64)),
        address("0x1111111111111111111111111111111111111111"),
        Token::Uint(U256::from(1_000_000_u64)),
        Token::Uint(U256::from(1_u64)),
        Token::Uint(U256::zero()),
    ])]));
    format!("0x{}", hex::encode(bytes))
}

fn relevant_payload(sequence: u64, hash_byte: char) -> Vec<u8> {
    serde_json::to_vec(&json!({
        "schema_version": NORMALIZED_SCHEMA_VERSION,
        "sequence": sequence,
        "timestamp_unix_ms": 1_700_000_000_000_u64,
        "tx_hash": format!("0x{}", hash_byte.to_string().repeat(64)),
        "tx_type": "0x02",
        "chain_id": ARBITRUM_ONE_CHAIN_ID,
        "from": "0x1111111111111111111111111111111111111111",
        "to": SWAP_ROUTER_02_ADDRESS,
        "nonce": sequence,
        "value": "0",
        "calldata": relevant_calldata(),
        "gas_limit": "300000",
        "max_fee_per_gas": "100000000",
        "max_priority_fee_per_gas": "1000000",
        "raw_tx": "AQID",
        "ingested_at_unix_ns": 1_700_000_000_123_456_789_i64
    }))
    .expect("serialize relevant integration payload")
}

async fn apply_migrations(pool: &PgPool) {
    for migration in [
        include_str!("../../migrations/001_init.sql"),
        include_str!("../../migrations/002_event_signatures.sql"),
        include_str!("../../migrations/003_shadow_profitability_evidence.sql"),
        include_str!("../../migrations/004_shadow_engine_runtime.sql"),
        include_str!("../../migrations/005_shadow_decision_identity.sql"),
        include_str!("../../migrations/006_dependency_exhaustion_quarantine.sql"),
        include_str!("../../migrations/007_canonical_profitability_truth.sql"),
        include_str!("../../migrations/008_shadow_route_discovery_indexes.sql"),
        include_str!("../../migrations/009_profit_triggered_secondary_verification.sql"),
        include_str!("../../migrations/010_fork_simulation_evidence.sql"),
        include_str!("../../migrations/011_money_path_selective_persistence.sql"),
    ] {
        sqlx::raw_sql(migration)
            .execute(pool)
            .await
            .expect("apply integration migration");
    }
}

async fn table_count(pool: &PgPool, table: &str) -> i64 {
    let query = format!("SELECT count(*) AS count FROM {table}");
    sqlx::query(&query)
        .fetch_one(pool)
        .await
        .expect("count integration table")
        .try_get("count")
        .expect("decode integration table count")
}

#[tokio::test]
async fn real_stream_consumer_publish_fetch_redelivery_and_ack() {
    let _guard = integration_lock().lock().await;
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

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn relevant_source_ack_waits_for_visible_three_row_commit() {
    let _guard = integration_lock().lock().await;
    let (Some(nats_url), Some(postgres_dsn)) = (local_nats_url(), local_postgres_dsn()) else {
        return;
    };
    let pool = PgPool::connect(&postgres_dsn)
        .await
        .expect("connect integration PostgreSQL");
    apply_migrations(&pool).await;
    sqlx::query(
        "TRUNCATE money_path_ingress_samples, money_path_ingress_daily, \
         engine_outbox, feed_events, origin_transactions CASCADE",
    )
    .execute(&pool)
    .await
    .expect("reset integration tables");
    sqlx::raw_sql(
        r#"
CREATE OR REPLACE FUNCTION phoenix_test_delay_relevant_commit() RETURNS trigger AS $$
BEGIN
    PERFORM pg_sleep(2);
    RETURN NEW;
END;
$$ LANGUAGE plpgsql;
CREATE TRIGGER phoenix_test_delay_relevant_commit_trigger
BEFORE INSERT ON engine_outbox
FOR EACH ROW EXECUTE FUNCTION phoenix_test_delay_relevant_commit();
"#,
    )
    .execute(&pool)
    .await
    .expect("install delayed commit trigger");

    let client = async_nats::connect(nats_url)
        .await
        .expect("connect integration NATS");
    let context = async_nats::jetstream::new(client.clone());
    let _ = context.delete_stream(STREAM_NAME).await;
    let consumer = ensure_durable_pipeline(&client)
        .await
        .expect("create integration durable pipeline");
    let observer = ensure_durable_pipeline(&client)
        .await
        .expect("open integration observer");
    let store = PostgresStore::connect(&postgres_dsn, "disable")
        .await
        .expect("connect integration Recorder store");
    store
        .verify_schema()
        .await
        .expect("verify integration schema");
    let classifier: Arc<dyn PrePersistenceClassifier> = Arc::new(
        MoneyPathClassifier::from_release(
            ADMISSION_POLICY_VERSION,
            &REVIEWED_ROUTER_ADDRESSES
                .iter()
                .map(|value| (*value).to_string())
                .collect::<Vec<_>>(),
            ROUTES,
        )
        .expect("construct integration classifier"),
    );
    let shutdown = CancellationToken::new();
    let runtime_shutdown = shutdown.clone();
    let runtime = consume_durable_messages(
        Arc::new(consumer),
        Arc::new(store),
        classifier,
        IngressBuffer::new(IngressBufferConfig::default())
            .expect("construct integration ingress buffer"),
        Readiness::new(),
        Metrics::default(),
        LogSampler::default(),
        runtime_shutdown,
        BatchConfig::default(),
        RetryPolicy::default(),
    );

    let verification = async {
        context
            .publish(NATS_SUBJECT, relevant_payload(77, 'd').into())
            .await
            .expect("publish relevant integration input")
            .await
            .expect("confirm relevant stream publish");

        let pending_deadline = tokio::time::Instant::now() + Duration::from_secs(5);
        loop {
            let state = observer
                .state()
                .await
                .expect("observe pending relevant ACK");
            if state.ack_pending == 1 {
                break;
            }
            assert!(
                tokio::time::Instant::now() < pending_deadline,
                "relevant input never became ACK-pending"
            );
            tokio::time::sleep(Duration::from_millis(25)).await;
        }
        assert_eq!(table_count(&pool, "origin_transactions").await, 0);
        assert_eq!(table_count(&pool, "feed_events").await, 0);
        assert_eq!(table_count(&pool, "engine_outbox").await, 0);

        let commit_deadline = tokio::time::Instant::now() + Duration::from_secs(8);
        loop {
            let state = observer
                .state()
                .await
                .expect("observe committed relevant ACK");
            if state.ack_pending == 0 && table_count(&pool, "engine_outbox").await == 1 {
                break;
            }
            assert!(
                tokio::time::Instant::now() < commit_deadline,
                "relevant commit and confirmed ACK did not complete"
            );
            tokio::time::sleep(Duration::from_millis(25)).await;
        }
        assert_eq!(table_count(&pool, "origin_transactions").await, 1);
        assert_eq!(table_count(&pool, "feed_events").await, 1);
        assert_eq!(table_count(&pool, "engine_outbox").await, 1);
        assert_eq!(table_count(&pool, "execution_attempts").await, 0);

        shutdown.cancel();
    };
    let (runtime_result, ()) = tokio::join!(
        tokio::time::timeout(Duration::from_secs(15), runtime),
        verification
    );
    assert_eq!(
        runtime_result.expect("Recorder runtime shutdown timeout"),
        ConsumerExit::Shutdown
    );
    context
        .delete_stream(STREAM_NAME)
        .await
        .expect("clean integration stream");
    sqlx::raw_sql(
        r#"
DROP TRIGGER phoenix_test_delay_relevant_commit_trigger ON engine_outbox;
DROP FUNCTION phoenix_test_delay_relevant_commit();
"#,
    )
    .execute(&pool)
    .await
    .expect("remove delayed commit trigger");
}
