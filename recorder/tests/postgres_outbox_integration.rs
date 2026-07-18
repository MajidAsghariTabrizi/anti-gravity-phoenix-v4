use phoenix_recorder::engine_outbox::{OutboxStore, PostgresOutbox};
use phoenix_recorder::model::{decode_message, ARBITRUM_ONE_CHAIN_ID, NORMALIZED_SCHEMA_VERSION};
use phoenix_recorder::persistence::{EventStore, PostgresStore, StoreError};
use serde_json::json;
use sqlx::{PgPool, Row};
use std::time::Duration;

fn local_postgres_dsn() -> Option<String> {
    let dsn = std::env::var("PHOENIX_TEST_POSTGRES_DSN").ok()?;
    assert!(
        dsn.contains("@127.0.0.1:") || dsn.contains("@localhost:"),
        "integration test PostgreSQL URL must be loopback-only"
    );
    Some(dsn)
}

fn message(sequence: u64, hash_byte: char) -> phoenix_recorder::model::ValidatedMessage {
    let payload = serde_json::to_vec(&json!({
        "schema_version": NORMALIZED_SCHEMA_VERSION,
        "sequence": sequence,
        "timestamp_unix_ms": 1_700_000_000_000_u64 + sequence,
        "tx_hash": format!("0x{}", hash_byte.to_string().repeat(64)),
        "tx_type": "0x02",
        "chain_id": ARBITRUM_ONE_CHAIN_ID,
        "from": "0x1111111111111111111111111111111111111111",
        "to": "0x2222222222222222222222222222222222222222",
        "nonce": sequence,
        "value": "0",
        "calldata": "0x1234",
        "gas_limit": "21000",
        "max_fee_per_gas": "100",
        "max_priority_fee_per_gas": "1",
        "raw_tx": "AQID",
        "ingested_at_unix_ns": 1_700_000_000_123_456_789_i64 + sequence as i64
    }))
    .expect("serialize integration payload");
    decode_message(&payload).expect("decode integration payload")
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

async fn row_count(pool: &PgPool, table: &str, tx_hash: &str) -> i64 {
    let query = format!("SELECT count(*) AS count FROM {table} WHERE tx_hash = $1");
    sqlx::query(&query)
        .bind(tx_hash)
        .fetch_one(pool)
        .await
        .expect("count integration rows")
        .try_get("count")
        .expect("decode integration count")
}

#[tokio::test]
async fn recorder_commit_outbox_recovery_and_rollback_are_atomic() {
    let Some(dsn) = local_postgres_dsn() else {
        return;
    };
    let pool = PgPool::connect(&dsn)
        .await
        .expect("connect integration PostgreSQL");
    apply_migrations(&pool).await;
    sqlx::query("TRUNCATE engine_outbox, feed_events, origin_transactions CASCADE")
        .execute(&pool)
        .await
        .expect("reset integration tables");

    let store = PostgresStore::connect(&dsn, "disable")
        .await
        .expect("connect Recorder store");
    store.verify_schema().await.expect("verify Recorder schema");

    let first = message(1, 'a');
    let inserted = store
        .persist_batch(std::slice::from_ref(&first))
        .await
        .expect("persist origin, feed event, and outbox");
    assert!(inserted[0].origin_transaction_inserted);
    assert!(inserted[0].feed_event_inserted);
    assert!(inserted[0].engine_outbox_inserted);
    assert_eq!(
        row_count(&pool, "origin_transactions", &first.tx.tx_hash).await,
        1
    );
    assert_eq!(row_count(&pool, "feed_events", &first.tx.tx_hash).await, 1);
    assert_eq!(
        row_count(&pool, "engine_outbox", &first.tx.tx_hash).await,
        1
    );

    let replay_store = PostgresStore::connect(&dsn, "disable")
        .await
        .expect("reconnect Recorder store");
    let duplicate = replay_store
        .persist_batch(std::slice::from_ref(&first))
        .await
        .expect("persist duplicate replay");
    assert!(duplicate[0].is_duplicate());

    sqlx::query("DELETE FROM engine_outbox WHERE tx_hash = $1")
        .bind(&first.tx.tx_hash)
        .execute(&pool)
        .await
        .expect("simulate historical missing outbox row");
    let repaired = replay_store
        .persist_batch(std::slice::from_ref(&first))
        .await
        .expect("repair missing outbox row");
    assert!(!repaired[0].origin_transaction_inserted);
    assert!(!repaired[0].feed_event_inserted);
    assert!(repaired[0].engine_outbox_inserted);

    let second = message(2, 'b');
    let mixed = replay_store
        .persist_batch(&[first.clone(), second.clone()])
        .await
        .expect("persist mixed duplicate and new batch");
    assert!(mixed[0].is_duplicate());
    assert!(mixed[1].origin_transaction_inserted);
    assert!(mixed[1].feed_event_inserted);
    assert!(mixed[1].engine_outbox_inserted);

    let outbox = PostgresOutbox::connect(&dsn, "disable")
        .await
        .expect("connect Dispatcher outbox store");
    outbox.verify_schema().await.expect("verify outbox schema");
    let owner_one = outbox
        .claim_batch("dispatcher-one", 1, Duration::from_secs(1))
        .await
        .expect("claim first outbox row");
    let owner_two = outbox
        .claim_batch("dispatcher-two", 1, Duration::from_secs(30))
        .await
        .expect("concurrently claim second outbox row");
    assert_eq!(owner_one.len(), 1);
    assert_eq!(owner_two.len(), 1);
    assert_ne!(owner_one[0].outbox_id, owner_two[0].outbox_id);

    outbox
        .mark_published(&owner_two[0].outbox_id, "dispatcher-two", 41)
        .await
        .expect("mark second row published after ACK");
    tokio::time::sleep(Duration::from_millis(1_100)).await;
    let recovered = outbox
        .claim_batch("dispatcher-recovery", 64, Duration::from_secs(30))
        .await
        .expect("reclaim expired lease");
    assert_eq!(recovered.len(), 1);
    assert_eq!(recovered[0].outbox_id, owner_one[0].outbox_id);
    outbox
        .mark_published(&recovered[0].outbox_id, "dispatcher-recovery", 42)
        .await
        .expect("mark recovered row published");
    assert!(outbox
        .claim_batch("dispatcher-final", 64, Duration::from_secs(30))
        .await
        .expect("verify published rows are not claimable")
        .is_empty());

    sqlx::raw_sql(
        r#"
CREATE OR REPLACE FUNCTION phoenix_test_reject_outbox() RETURNS trigger AS $$
BEGIN
    IF NEW.source_sequence = 99 THEN
        RAISE EXCEPTION 'forced integration rollback';
    END IF;
    RETURN NEW;
END;
$$ LANGUAGE plpgsql;
CREATE TRIGGER phoenix_test_reject_outbox_trigger
BEFORE INSERT ON engine_outbox
FOR EACH ROW EXECUTE FUNCTION phoenix_test_reject_outbox();
"#,
    )
    .execute(&pool)
    .await
    .expect("install integration rollback trigger");

    let rejected = message(99, 'c');
    let result = replay_store
        .persist_batch(std::slice::from_ref(&rejected))
        .await;
    assert_eq!(result, Err(StoreError::Transaction));
    assert_eq!(
        row_count(&pool, "origin_transactions", &rejected.tx.tx_hash).await,
        0
    );
    assert_eq!(
        row_count(&pool, "feed_events", &rejected.tx.tx_hash).await,
        0
    );
    assert_eq!(
        row_count(&pool, "engine_outbox", &rejected.tx.tx_hash).await,
        0
    );

    sqlx::raw_sql(
        r#"
DROP TRIGGER phoenix_test_reject_outbox_trigger ON engine_outbox;
DROP FUNCTION phoenix_test_reject_outbox();
"#,
    )
    .execute(&pool)
    .await
    .expect("remove integration rollback trigger");
}
