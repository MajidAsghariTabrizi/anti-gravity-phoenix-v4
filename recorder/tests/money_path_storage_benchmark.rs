use async_trait::async_trait;
use ethabi::ethereum_types::{H160, U256};
use ethabi::{ParamType, Token};
use money_path_classifier::{
    IngressClassification, MoneyPathClassifier, ADMISSION_POLICY_VERSION,
    REVIEWED_ROUTER_ADDRESSES, SWAP_ROUTER_02_ADDRESS,
};
use phoenix_recorder::dispatcher::{
    dispatch_once, DispatchConfig, DispatcherMetrics, DispatcherReadiness,
};
use phoenix_recorder::engine_outbox::{OutboxStore, PostgresOutbox};
use phoenix_recorder::engine_stream::{EnginePublishReceipt, EnginePublisher, EngineStreamError};
use phoenix_recorder::ingress::{IngressBuffer, IngressBufferConfig};
use phoenix_recorder::model::{
    decode_message, ValidatedMessage, ARBITRUM_ONE_CHAIN_ID, NORMALIZED_SCHEMA_VERSION,
};
use phoenix_recorder::persistence::{EventStore, PostgresStore};
use serde::Serialize;
use serde_json::json;
use sqlx::postgres::{PgConnectOptions, PgPoolOptions};
use sqlx::PgPool;
use std::collections::BTreeMap;
use std::fs;
use std::str::FromStr;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

const ROUTES: &str = include_str!("../../fixtures/routes/weth_usdc_uniswap_v3.json");
const WETH: &str = "0x82af49447d8a07e3bd95bd0d56f35241523fbab1";
const USDC: &str = "0xaf88d065e77c8cc2239327c5edb3a432268e5831";
const UNKNOWN_DESTINATION: &str = "0x1111111111111111111111111111111111111111";
const FEED_INPUTS_PER_SECOND: f64 = 16.76;
const FREE_DISK_BYTES: f64 = 33.0 * 1024.0 * 1024.0 * 1024.0;
const CURRENT_DAILY_GROWTH_BYTES: f64 = 7.0 * 1024.0 * 1024.0 * 1024.0;
const RELEVANCE_BPS: [u64; 4] = [20, 100, 500, 1_000];

#[derive(Clone, Copy, Debug, Default)]
struct ProcessUsage {
    cpu_ticks: u64,
    resident_bytes: u64,
    peak_resident_bytes: u64,
}

#[derive(Debug, Serialize)]
struct BenchmarkResult {
    relevance_ratio_percent: f64,
    warmup_fixture_inputs: u64,
    fixture_inputs: u64,
    fixture_payload_bytes: usize,
    relevant_events: u64,
    unsupported_events: u64,
    irrelevant_events: u64,
    feed_inputs_per_second: f64,
    filtered_events_per_second: f64,
    relevant_commits_per_second: f64,
    outbox_rows_per_second: f64,
    dispatcher_rows_published_per_second: f64,
    postgres_relation_size_delta_bytes: u64,
    postgres_relation_size_delta_by_table: BTreeMap<String, u64>,
    postgres_bytes_per_feed_input: f64,
    postgres_bytes_per_relevant_event: f64,
    projected_mb_per_day_at_16_76_inputs_per_second: f64,
    projected_disk_runway_days_with_33_gib_free: f64,
    expected_storage_reduction_percent: f64,
    cpu_ticks_delta: u64,
    resident_memory_delta_bytes: i64,
    peak_resident_memory_bytes: u64,
    aggregate_rows: i64,
    bounded_sample_rows: i64,
    methodology: &'static str,
}

#[derive(Debug, Default)]
struct BenchmarkPublisher {
    sequence: AtomicU64,
}

#[async_trait]
impl EnginePublisher for BenchmarkPublisher {
    async fn publish(
        &self,
        _row: &phoenix_recorder::engine_outbox::OutboxRow,
    ) -> Result<EnginePublishReceipt, EngineStreamError> {
        Ok(EnginePublishReceipt {
            stream_sequence: self.sequence.fetch_add(1, Ordering::Relaxed) + 1,
            duplicate: false,
        })
    }
}

fn local_postgres_dsn() -> Option<String> {
    let dsn = std::env::var("PHOENIX_TEST_POSTGRES_DSN").ok()?;
    assert!(
        dsn.contains("@127.0.0.1:") || dsn.contains("@localhost:"),
        "benchmark PostgreSQL URL must be loopback-only"
    );
    Some(dsn)
}

fn fixture_inputs() -> u64 {
    let value = std::env::var("PHOENIX_STORAGE_BENCHMARK_INPUTS")
        .unwrap_or_else(|_| "100000".to_string())
        .parse::<u64>()
        .expect("benchmark fixture size must be an integer");
    assert!(
        (10_000..=100_000).contains(&value),
        "benchmark fixture size must be between 10,000 and 100,000"
    );
    value
}

fn address(value: &str) -> Token {
    Token::Address(H160::from_slice(
        &hex::decode(value.trim_start_matches("0x")).expect("fixture address"),
    ))
}

fn relevant_calldata() -> String {
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
        address(UNKNOWN_DESTINATION),
        Token::Uint(U256::from(1_000_000_u64)),
        Token::Uint(U256::from(1_u64)),
        Token::Uint(U256::zero()),
    ])]));
    format!("0x{}", hex::encode(bytes))
}

fn message(sequence: u64, calldata: &str) -> ValidatedMessage {
    let payload = serde_json::to_vec(&json!({
        "schema_version": NORMALIZED_SCHEMA_VERSION,
        "sequence": sequence,
        "timestamp_unix_ms": 1_700_000_000_000_u64 + sequence,
        "tx_hash": format!("0x{sequence:064x}"),
        "tx_type": "0x02",
        "chain_id": ARBITRUM_ONE_CHAIN_ID,
        "from": UNKNOWN_DESTINATION,
        "to": SWAP_ROUTER_02_ADDRESS,
        "nonce": sequence,
        "value": "0",
        "calldata": calldata,
        "gas_limit": "300000",
        "max_fee_per_gas": "100000000",
        "max_priority_fee_per_gas": "1000000",
        "raw_tx": "AQID",
        "ingested_at_unix_ns": 1_700_000_000_123_456_789_i64 + sequence as i64
    }))
    .expect("serialize benchmark event");
    decode_message(&payload).expect("decode benchmark event")
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
            .expect("apply benchmark migration");
    }
}

async fn measured_relation_bytes(pool: &PgPool) -> BTreeMap<String, u64> {
    let rows: Vec<(String, i64)> = sqlx::query_as(
        r#"
SELECT relation_name,
       pg_total_relation_size(relation_name::regclass)::bigint
FROM (
    VALUES
        ('origin_transactions'),
        ('feed_events'),
        ('engine_outbox'),
        ('money_path_ingress_daily'),
        ('money_path_ingress_samples')
) AS measured_relations(relation_name)
"#,
    )
    .fetch_all(pool)
    .await
    .expect("measure benchmark relations");
    rows.into_iter()
        .map(|(name, bytes)| {
            (
                name,
                u64::try_from(bytes).expect("relation size must be non-negative"),
            )
        })
        .collect()
}

async fn table_count(pool: &PgPool, table: &str) -> i64 {
    let query = format!("SELECT count(*) FROM {table}");
    sqlx::query_scalar(&query)
        .fetch_one(pool)
        .await
        .expect("count benchmark table")
}

fn process_usage() -> ProcessUsage {
    let stat = fs::read_to_string("/proc/self/stat").unwrap_or_default();
    let cpu_ticks = stat
        .rsplit_once(") ")
        .map(|(_, fields)| fields.split_whitespace().collect::<Vec<_>>())
        .and_then(|fields| {
            let user = fields.get(11)?.parse::<u64>().ok()?;
            let system = fields.get(12)?.parse::<u64>().ok()?;
            Some(user.saturating_add(system))
        })
        .unwrap_or_default();
    let status = fs::read_to_string("/proc/self/status").unwrap_or_default();
    let value = |name: &str| {
        status
            .lines()
            .find(|line| line.starts_with(name))
            .and_then(|line| line.split_whitespace().nth(1))
            .and_then(|nested| nested.parse::<u64>().ok())
            .unwrap_or_default()
            .saturating_mul(1024)
    };
    ProcessUsage {
        cpu_ticks,
        resident_bytes: value("VmRSS:"),
        peak_resident_bytes: value("VmHWM:"),
    }
}

struct BenchmarkWindow {
    fixture_payload_bytes: usize,
    relevant: u64,
    unsupported: u64,
    irrelevant: u64,
    persistence_seconds: f64,
    published: u64,
    dispatcher_seconds: f64,
    aggregate_rows_added: i64,
    sample_rows_added: i64,
}

async fn run_window(
    pool: &PgPool,
    store: &PostgresStore,
    classifier: &MoneyPathClassifier,
    total_inputs: u64,
    relevance_bps: u64,
    sequence_offset: u64,
    observed_at: chrono::DateTime<chrono::Utc>,
) -> BenchmarkWindow {
    let ingress = IngressBuffer::new(IngressBufferConfig {
        flush_after_events: 100_000,
        ..IngressBufferConfig::default()
    })
    .expect("construct benchmark aggregate buffer");
    let calldata = relevant_calldata();
    let fixture_payload_bytes =
        serde_json::to_vec(&message(sequence_offset + 1, &calldata).payload)
            .expect("encode benchmark payload")
            .len();
    let relevant_target = total_inputs.saturating_mul(relevance_bps) / 10_000;
    let origin_rows_before = table_count(pool, "origin_transactions").await;
    let feed_rows_before = table_count(pool, "feed_events").await;
    let outbox_rows_before = table_count(pool, "engine_outbox").await;
    let aggregate_rows_before = table_count(pool, "money_path_ingress_daily").await;
    let sample_rows_before = table_count(pool, "money_path_ingress_samples").await;
    let mut relevant_batch = Vec::with_capacity(256);
    let mut relevant = 0_u64;
    let mut unsupported = 0_u64;
    let mut irrelevant = 0_u64;
    let started = Instant::now();
    for index in 0..total_inputs {
        let (destination, input_calldata) = if index < relevant_target {
            (Some(SWAP_ROUTER_02_ADDRESS), calldata.as_str())
        } else if index % 20 == 0 {
            (Some(UNKNOWN_DESTINATION), "0x1234567890ab")
        } else {
            (Some(UNKNOWN_DESTINATION), "0x")
        };
        let classification = classifier
            .classify(ARBITRUM_ONE_CHAIN_ID, destination, input_calldata)
            .expect("classify benchmark input");
        ingress
            .record(
                &classification,
                observed_at + chrono::Duration::milliseconds(index as i64),
            )
            .expect("record benchmark aggregate");
        match classification.classification {
            IngressClassification::RelevantRouteInput => {
                relevant = relevant.saturating_add(1);
                relevant_batch.push(message(sequence_offset + index + 1, &calldata));
                if relevant_batch.len() == 256 {
                    let outcomes = store
                        .persist_batch(&relevant_batch)
                        .await
                        .expect("persist benchmark relevant batch");
                    assert!(outcomes.iter().all(|outcome| {
                        outcome.origin_transaction_inserted
                            && outcome.feed_event_inserted
                            && outcome.engine_outbox_inserted
                    }));
                    relevant_batch.clear();
                }
            }
            IngressClassification::UnsupportedInteresting => {
                unsupported = unsupported.saturating_add(1);
            }
            IngressClassification::Irrelevant => {
                irrelevant = irrelevant.saturating_add(1);
            }
        }
    }
    if !relevant_batch.is_empty() {
        let outcomes = store
            .persist_batch(&relevant_batch)
            .await
            .expect("persist final benchmark relevant batch");
        assert!(outcomes.iter().all(|outcome| {
            outcome.origin_transaction_inserted
                && outcome.feed_event_inserted
                && outcome.engine_outbox_inserted
        }));
    }
    let flush = ingress.take().expect("take benchmark aggregate batch");
    store
        .persist_ingress_evidence(&flush, 100)
        .await
        .expect("persist benchmark aggregate batch");
    let persistence_seconds = started.elapsed().as_secs_f64().max(f64::EPSILON);

    assert_eq!(relevant, relevant_target);
    assert_eq!(relevant + unsupported + irrelevant, total_inputs);
    assert_eq!(
        table_count(pool, "origin_transactions").await - origin_rows_before,
        relevant as i64
    );
    assert_eq!(
        table_count(pool, "feed_events").await - feed_rows_before,
        relevant as i64
    );
    assert_eq!(
        table_count(pool, "engine_outbox").await - outbox_rows_before,
        relevant as i64
    );
    assert_eq!(table_count(pool, "execution_attempts").await, 0);
    assert_eq!(table_count(pool, "executions").await, 0);
    assert_eq!(table_count(pool, "realized_pnl").await, 0);

    let outbox = PostgresOutbox::from_pool(pool.clone());
    outbox
        .verify_schema()
        .await
        .expect("verify benchmark outbox schema");
    let publisher = BenchmarkPublisher::default();
    let dispatcher_config = DispatchConfig {
        owner: format!("storage-benchmark-{relevance_bps}-{sequence_offset}"),
        ..DispatchConfig::default()
    }
    .validate()
    .expect("validate benchmark dispatcher");
    let dispatcher_started = Instant::now();
    let mut published = 0_u64;
    loop {
        let rows = dispatch_once(
            &outbox,
            &publisher,
            &dispatcher_config,
            &DispatcherReadiness::new(),
            &DispatcherMetrics::default(),
        )
        .await
        .expect("dispatch benchmark outbox");
        if rows == 0 {
            break;
        }
        published = published.saturating_add(rows as u64);
    }
    let dispatcher_seconds = dispatcher_started.elapsed().as_secs_f64().max(f64::EPSILON);
    assert_eq!(published, relevant);
    assert!(outbox
        .claim_batch("storage-benchmark-final", 64, Duration::from_secs(30))
        .await
        .expect("verify benchmark outbox drained")
        .is_empty());

    BenchmarkWindow {
        fixture_payload_bytes,
        relevant,
        unsupported,
        irrelevant,
        persistence_seconds,
        published,
        dispatcher_seconds,
        aggregate_rows_added: table_count(pool, "money_path_ingress_daily").await
            - aggregate_rows_before,
        sample_rows_added: table_count(pool, "money_path_ingress_samples").await
            - sample_rows_before,
    }
}

async fn run_scenario(
    admin_pool: &PgPool,
    base_options: &PgConnectOptions,
    total_inputs: u64,
    relevance_bps: u64,
) -> BenchmarkResult {
    let unique = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos()
        % 1_000_000_000;
    let database_name = format!(
        "mpv1_bench_{}_{}_{}",
        std::process::id(),
        relevance_bps,
        unique
    );
    sqlx::query(&format!("CREATE DATABASE {database_name}"))
        .execute(admin_pool)
        .await
        .expect("create isolated benchmark database");

    let options = base_options.clone().database(&database_name);
    let pool = PgPoolOptions::new()
        .max_connections(4)
        .connect_with(options)
        .await
        .expect("connect isolated benchmark database");
    apply_migrations(&pool).await;
    let store = PostgresStore::from_pool(pool.clone());
    store
        .verify_schema()
        .await
        .expect("verify benchmark schema");
    let classifier = MoneyPathClassifier::from_release(
        ADMISSION_POLICY_VERSION,
        &REVIEWED_ROUTER_ADDRESSES
            .iter()
            .map(|value| (*value).to_string())
            .collect::<Vec<_>>(),
        ROUTES,
    )
    .expect("construct benchmark classifier");
    let warmup_at = chrono::DateTime::parse_from_rfc3339("2026-07-18T00:00:00Z")
        .unwrap()
        .with_timezone(&chrono::Utc);
    let warmup = run_window(
        &pool,
        &store,
        &classifier,
        total_inputs,
        relevance_bps,
        0,
        warmup_at,
    )
    .await;
    assert_eq!(warmup.aggregate_rows_added, 3);
    assert_eq!(warmup.sample_rows_added, 100);
    // The synthetic burst is faster than autovacuum. Expose its dead claim/publish
    // versions before measuring the next window's ongoing allocation.
    sqlx::query("VACUUM (ANALYZE) engine_outbox")
        .execute(&pool)
        .await
        .expect("vacuum warmup outbox versions");
    let baseline_bytes = measured_relation_bytes(&pool).await;
    let usage_before = process_usage();
    let measured_at = chrono::DateTime::parse_from_rfc3339("2026-07-19T00:00:00Z")
        .unwrap()
        .with_timezone(&chrono::Utc);
    let measured = run_window(
        &pool,
        &store,
        &classifier,
        total_inputs,
        relevance_bps,
        total_inputs,
        measured_at,
    )
    .await;

    let final_bytes = measured_relation_bytes(&pool).await;
    let relation_deltas = final_bytes
        .iter()
        .map(|(name, bytes)| {
            let baseline = *baseline_bytes
                .get(name)
                .expect("every measured relation must have a baseline");
            (
                name.clone(),
                bytes
                    .checked_sub(baseline)
                    .expect("measured relation size must not regress below its baseline"),
            )
        })
        .collect::<BTreeMap<_, _>>();
    let relation_delta = relation_deltas.values().copied().sum();
    let bytes_per_input = relation_delta as f64 / total_inputs as f64;
    let bytes_per_relevant = relation_delta as f64 / measured.relevant.max(1) as f64;
    let projected_daily_bytes = bytes_per_input * FEED_INPUTS_PER_SECOND * 86_400.0;
    let current_inputs_per_day = FEED_INPUTS_PER_SECOND * 86_400.0;
    let current_bytes_per_input = CURRENT_DAILY_GROWTH_BYTES / current_inputs_per_day;
    let usage_after = process_usage();
    let result = BenchmarkResult {
        relevance_ratio_percent: relevance_bps as f64 / 100.0,
        warmup_fixture_inputs: total_inputs,
        fixture_inputs: total_inputs,
        fixture_payload_bytes: measured.fixture_payload_bytes,
        relevant_events: measured.relevant,
        unsupported_events: measured.unsupported,
        irrelevant_events: measured.irrelevant,
        feed_inputs_per_second: total_inputs as f64 / measured.persistence_seconds,
        filtered_events_per_second: (measured.unsupported + measured.irrelevant) as f64
            / measured.persistence_seconds,
        relevant_commits_per_second: measured.relevant as f64 / measured.persistence_seconds,
        outbox_rows_per_second: measured.relevant as f64 / measured.persistence_seconds,
        dispatcher_rows_published_per_second: measured.published as f64
            / measured.dispatcher_seconds,
        postgres_relation_size_delta_bytes: relation_delta,
        postgres_relation_size_delta_by_table: relation_deltas,
        postgres_bytes_per_feed_input: bytes_per_input,
        postgres_bytes_per_relevant_event: bytes_per_relevant,
        projected_mb_per_day_at_16_76_inputs_per_second: projected_daily_bytes / 1_000_000.0,
        projected_disk_runway_days_with_33_gib_free: FREE_DISK_BYTES / projected_daily_bytes,
        expected_storage_reduction_percent: (1.0 - bytes_per_input / current_bytes_per_input) * 100.0,
        cpu_ticks_delta: usage_after.cpu_ticks.saturating_sub(usage_before.cpu_ticks),
        resident_memory_delta_bytes: usage_after.resident_bytes as i64
            - usage_before.resident_bytes as i64,
        peak_resident_memory_bytes: usage_after.peak_resident_bytes,
        aggregate_rows: measured.aggregate_rows_added,
        bounded_sample_rows: measured.sample_rows_added,
        methodology: "fresh isolated PostgreSQL database per ratio; the first fixture window exercises actual Recorder inserts and Dispatcher claim/mark churn; ordinary non-rewriting VACUUM (ANALYZE) exposes reusable outbox space; reported pg_total_relation_size delta is the second UTC-day fixture window across origin_transactions, feed_events, engine_outbox, money_path_ingress_daily, and money_path_ingress_samples; each window uses actual migrations 001-011, actual Recorder three-row transactions, bounded aggregate/sample persistence, and the actual Dispatcher claim/mark loop with a deterministic in-process ACK stub",
    };
    assert_eq!(result.aggregate_rows, 3);
    assert_eq!(result.bounded_sample_rows, 100);

    pool.close().await;
    sqlx::query(&format!("DROP DATABASE {database_name}"))
        .execute(admin_pool)
        .await
        .expect("drop isolated benchmark database");
    result
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn measured_storage_acceptance_across_relevance_ratios() {
    let Some(dsn) = local_postgres_dsn() else {
        return;
    };
    let options = PgConnectOptions::from_str(&dsn).expect("parse benchmark PostgreSQL URL");
    let admin_pool = PgPoolOptions::new()
        .max_connections(1)
        .connect_with(options.clone())
        .await
        .expect("connect benchmark PostgreSQL");
    let total_inputs = fixture_inputs();
    let mut results = Vec::new();
    for relevance_bps in RELEVANCE_BPS {
        let result = run_scenario(&admin_pool, &options, total_inputs, relevance_bps).await;
        println!(
            "MONEY_PATH_STORAGE_BENCHMARK {}",
            serde_json::to_string(&result).expect("serialize benchmark result")
        );
        results.push(result);
    }
    admin_pool.close().await;

    let one_percent = results
        .iter()
        .find(|result| result.relevance_ratio_percent == 1.0)
        .expect("one-percent benchmark result");
    assert!(
        one_percent.postgres_bytes_per_feed_input <= 100.0,
        "one-percent PostgreSQL bytes per input exceeded acceptance target: {}",
        one_percent.postgres_bytes_per_feed_input
    );
    assert!(
        one_percent.projected_mb_per_day_at_16_76_inputs_per_second <= 100.0,
        "one-percent projected daily growth exceeded acceptance target: {}",
        one_percent.projected_mb_per_day_at_16_76_inputs_per_second
    );
}
