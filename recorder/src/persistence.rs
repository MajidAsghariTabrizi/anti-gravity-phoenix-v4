use crate::model::{ValidatedMessage, ORIGIN_CLASSIFICATION};
use async_trait::async_trait;
use sqlx::postgres::{PgConnectOptions, PgPoolOptions, PgSslMode};
use sqlx::types::Json;
use sqlx::{PgPool, Row};
use std::collections::{BTreeMap, BTreeSet};
use std::str::FromStr;
use std::time::Duration;
use thiserror::Error;

const REQUIRED_COLUMNS: &[(&str, &str, &str, bool)] = &[
    ("feed_events", "sequence_number", "numeric", false),
    ("feed_events", "tx_hash", "text", true),
    ("feed_events", "payload", "jsonb", false),
    (
        "feed_events",
        "recorded_at",
        "timestamp with time zone",
        false,
    ),
    ("origin_transactions", "tx_hash", "text", false),
    ("origin_transactions", "sequence_number", "numeric", false),
    ("origin_transactions", "chain_id", "bigint", false),
    ("origin_transactions", "router", "text", true),
    ("origin_transactions", "classification", "text", false),
    ("origin_transactions", "calldata", "bytea", true),
    (
        "origin_transactions",
        "seen_at",
        "timestamp with time zone",
        false,
    ),
    ("origin_transactions", "metadata", "jsonb", false),
];

const ORIGIN_INSERT_SQL: &str = r#"
INSERT INTO origin_transactions (
    tx_hash, sequence_number, chain_id, router, classification, calldata, seen_at, metadata
)
VALUES ($1, $2::numeric, $3, $4, $5, $6, $7, $8)
ON CONFLICT (tx_hash) DO NOTHING
"#;

const FEED_EVENT_INSERT_SQL: &str = r#"
INSERT INTO feed_events (sequence_number, tx_hash, payload, recorded_at)
VALUES ($1::numeric, $2, $3, $4)
ON CONFLICT (sequence_number, tx_hash) DO NOTHING
"#;

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct SchemaSnapshot {
    pub columns: BTreeMap<String, BTreeMap<String, ColumnDefinition>>,
    pub unique_constraints: BTreeMap<String, BTreeSet<Vec<String>>>,
    pub origin_chain_checks: Vec<String>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ColumnDefinition {
    pub data_type: String,
    pub nullable: bool,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct PersistOutcome {
    pub feed_event_inserted: bool,
    pub origin_transaction_inserted: bool,
}

impl PersistOutcome {
    pub fn is_duplicate(&self) -> bool {
        !self.feed_event_inserted && !self.origin_transaction_inserted
    }
}

#[derive(Clone, Debug, Error, PartialEq, Eq)]
pub enum StoreError {
    #[error("PostgreSQL configuration is invalid")]
    Configuration,
    #[error("PostgreSQL connection is unavailable")]
    Connection,
    #[error("required PostgreSQL schema is missing or incompatible: {0}")]
    Schema(String),
    #[error("PostgreSQL transaction failed")]
    Transaction,
}

#[async_trait]
pub trait EventStore: Send + Sync {
    async fn ping(&self) -> Result<(), StoreError>;
    async fn verify_schema(&self) -> Result<(), StoreError>;
    async fn persist(&self, message: &ValidatedMessage) -> Result<PersistOutcome, StoreError>;
}

#[derive(Clone, Debug)]
pub struct PostgresStore {
    pool: PgPool,
}

impl PostgresStore {
    pub async fn connect(dsn: &str, ssl_mode: &str) -> Result<Self, StoreError> {
        let options = PgConnectOptions::from_str(dsn)
            .map_err(|_| StoreError::Configuration)?
            .ssl_mode(parse_ssl_mode(ssl_mode)?);
        let pool = PgPoolOptions::new()
            .max_connections(4)
            .min_connections(1)
            .acquire_timeout(Duration::from_secs(5))
            .connect_with(options)
            .await
            .map_err(classify_sqlx_error)?;
        Ok(Self { pool })
    }

    async fn load_schema_snapshot(&self) -> Result<SchemaSnapshot, StoreError> {
        let mut snapshot = SchemaSnapshot::default();
        let rows = sqlx::query(
            r#"
SELECT table_name, column_name, data_type, is_nullable
FROM information_schema.columns
WHERE table_schema = 'public'
  AND table_name IN ('feed_events', 'origin_transactions')
"#,
        )
        .fetch_all(&self.pool)
        .await
        .map_err(classify_sqlx_error)?;
        for row in rows {
            let table: String = row.try_get("table_name").map_err(classify_sqlx_error)?;
            let column: String = row.try_get("column_name").map_err(classify_sqlx_error)?;
            let data_type: String = row.try_get("data_type").map_err(classify_sqlx_error)?;
            let is_nullable: String = row.try_get("is_nullable").map_err(classify_sqlx_error)?;
            snapshot.columns.entry(table).or_default().insert(
                column,
                ColumnDefinition {
                    data_type,
                    nullable: is_nullable == "YES",
                },
            );
        }

        let rows = sqlx::query(
            r#"
SELECT tc.table_name,
       array_agg(kcu.column_name ORDER BY kcu.ordinal_position)::text[] AS columns
FROM information_schema.table_constraints tc
JOIN information_schema.key_column_usage kcu
  ON tc.constraint_schema = kcu.constraint_schema
 AND tc.constraint_name = kcu.constraint_name
 AND tc.table_name = kcu.table_name
WHERE tc.table_schema = 'public'
  AND tc.table_name IN ('feed_events', 'origin_transactions')
  AND tc.constraint_type IN ('PRIMARY KEY', 'UNIQUE')
GROUP BY tc.table_name, tc.constraint_name
"#,
        )
        .fetch_all(&self.pool)
        .await
        .map_err(classify_sqlx_error)?;
        for row in rows {
            let table: String = row.try_get("table_name").map_err(classify_sqlx_error)?;
            let columns: Vec<String> = row.try_get("columns").map_err(classify_sqlx_error)?;
            snapshot
                .unique_constraints
                .entry(table)
                .or_default()
                .insert(columns);
        }

        let rows = sqlx::query(
            r#"
SELECT pg_get_constraintdef(constraint_row.oid) AS definition
FROM pg_constraint constraint_row
JOIN pg_class table_row ON table_row.oid = constraint_row.conrelid
JOIN pg_namespace namespace_row ON namespace_row.oid = table_row.relnamespace
WHERE namespace_row.nspname = 'public'
  AND table_row.relname = 'origin_transactions'
  AND constraint_row.contype = 'c'
"#,
        )
        .fetch_all(&self.pool)
        .await
        .map_err(classify_sqlx_error)?;
        for row in rows {
            snapshot
                .origin_chain_checks
                .push(row.try_get("definition").map_err(classify_sqlx_error)?);
        }
        Ok(snapshot)
    }
}

#[async_trait]
impl EventStore for PostgresStore {
    async fn ping(&self) -> Result<(), StoreError> {
        sqlx::query("SELECT 1")
            .execute(&self.pool)
            .await
            .map_err(classify_sqlx_error)?;
        Ok(())
    }

    async fn verify_schema(&self) -> Result<(), StoreError> {
        validate_schema_snapshot(&self.load_schema_snapshot().await?)
    }

    async fn persist(&self, message: &ValidatedMessage) -> Result<PersistOutcome, StoreError> {
        let mut transaction = self.pool.begin().await.map_err(classify_sqlx_error)?;
        let sequence = message.tx.sequence.to_string();
        let router = (!message.tx.to.is_empty()).then_some(message.tx.to.as_str());

        let origin = sqlx::query(ORIGIN_INSERT_SQL)
            .bind(&message.tx.tx_hash)
            .bind(&sequence)
            .bind(message.tx.chain_id as i64)
            .bind(router)
            .bind(ORIGIN_CLASSIFICATION)
            .bind(&message.calldata)
            .bind(message.seen_at)
            .bind(Json(&message.metadata))
            .execute(&mut *transaction)
            .await
            .map_err(classify_sqlx_error)?;

        let event = sqlx::query(FEED_EVENT_INSERT_SQL)
            .bind(&sequence)
            .bind(&message.tx.tx_hash)
            .bind(Json(&message.payload))
            .bind(message.seen_at)
            .execute(&mut *transaction)
            .await
            .map_err(classify_sqlx_error)?;

        transaction.commit().await.map_err(classify_sqlx_error)?;
        Ok(PersistOutcome {
            feed_event_inserted: event.rows_affected() == 1,
            origin_transaction_inserted: origin.rows_affected() == 1,
        })
    }
}

pub fn validate_schema_snapshot(snapshot: &SchemaSnapshot) -> Result<(), StoreError> {
    for (table, column, expected_type, expected_nullable) in REQUIRED_COLUMNS {
        let actual = snapshot
            .columns
            .get(*table)
            .and_then(|columns| columns.get(*column));
        if actual.map(|definition| (definition.data_type.as_str(), definition.nullable))
            != Some((*expected_type, *expected_nullable))
        {
            return Err(StoreError::Schema(format!(
                "{table}.{column} type or nullability is incompatible"
            )));
        }
    }

    require_unique(snapshot, "origin_transactions", &["tx_hash"])?;
    require_unique(snapshot, "feed_events", &["sequence_number", "tx_hash"])?;

    let chain_check_present = snapshot.origin_chain_checks.iter().any(|definition| {
        let normalized = definition.to_ascii_lowercase().replace(['(', ')'], "");
        normalized
            .split_whitespace()
            .collect::<Vec<_>>()
            .join(" ")
            .contains("chain_id = 42161")
    });
    if !chain_check_present {
        return Err(StoreError::Schema(
            "origin_transactions chain_id check is missing".to_string(),
        ));
    }
    Ok(())
}

fn require_unique(
    snapshot: &SchemaSnapshot,
    table: &str,
    expected: &[&str],
) -> Result<(), StoreError> {
    let expected = expected
        .iter()
        .map(|column| (*column).to_string())
        .collect::<Vec<_>>();
    if snapshot
        .unique_constraints
        .get(table)
        .is_some_and(|constraints| constraints.contains(&expected))
    {
        Ok(())
    } else {
        Err(StoreError::Schema(format!(
            "{table} unique constraint on {} is missing",
            expected.join(", ")
        )))
    }
}

fn parse_ssl_mode(value: &str) -> Result<PgSslMode, StoreError> {
    match value.trim().to_ascii_lowercase().as_str() {
        "disable" => Ok(PgSslMode::Disable),
        "allow" => Ok(PgSslMode::Allow),
        "prefer" | "" => Ok(PgSslMode::Prefer),
        "require" => Ok(PgSslMode::Require),
        "verify-ca" => Ok(PgSslMode::VerifyCa),
        "verify-full" => Ok(PgSslMode::VerifyFull),
        _ => Err(StoreError::Configuration),
    }
}

fn classify_sqlx_error(error: sqlx::Error) -> StoreError {
    match error {
        sqlx::Error::Configuration(_) => StoreError::Configuration,
        sqlx::Error::Io(_)
        | sqlx::Error::Tls(_)
        | sqlx::Error::PoolTimedOut
        | sqlx::Error::PoolClosed
        | sqlx::Error::WorkerCrashed => StoreError::Connection,
        _ => StoreError::Transaction,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn valid_snapshot() -> SchemaSnapshot {
        let mut snapshot = SchemaSnapshot::default();
        for (table, column, data_type, nullable) in REQUIRED_COLUMNS {
            snapshot
                .columns
                .entry((*table).to_string())
                .or_default()
                .insert(
                    (*column).to_string(),
                    ColumnDefinition {
                        data_type: (*data_type).to_string(),
                        nullable: *nullable,
                    },
                );
        }
        snapshot
            .unique_constraints
            .entry("origin_transactions".to_string())
            .or_default()
            .insert(vec!["tx_hash".to_string()]);
        snapshot
            .unique_constraints
            .entry("feed_events".to_string())
            .or_default()
            .insert(vec!["sequence_number".to_string(), "tx_hash".to_string()]);
        snapshot
            .origin_chain_checks
            .push("CHECK ((chain_id = 42161))".to_string());
        snapshot
    }

    #[test]
    fn schema_verification_accepts_exact_tables_and_constraints() {
        assert_eq!(validate_schema_snapshot(&valid_snapshot()), Ok(()));
    }

    #[test]
    fn schema_verification_rejects_missing_table_or_column() {
        let mut missing_table = valid_snapshot();
        missing_table.columns.remove("feed_events");
        assert!(matches!(
            validate_schema_snapshot(&missing_table),
            Err(StoreError::Schema(_))
        ));

        let mut missing_column = valid_snapshot();
        missing_column
            .columns
            .get_mut("origin_transactions")
            .unwrap()
            .remove("metadata");
        assert!(matches!(
            validate_schema_snapshot(&missing_column),
            Err(StoreError::Schema(_))
        ));

        let mut wrong_nullability = valid_snapshot();
        wrong_nullability
            .columns
            .get_mut("origin_transactions")
            .unwrap()
            .get_mut("tx_hash")
            .unwrap()
            .nullable = true;
        assert!(matches!(
            validate_schema_snapshot(&wrong_nullability),
            Err(StoreError::Schema(_))
        ));
    }

    #[test]
    fn schema_verification_requires_idempotency_constraints() {
        let mut snapshot = valid_snapshot();
        snapshot.unique_constraints.remove("feed_events");
        assert!(matches!(
            validate_schema_snapshot(&snapshot),
            Err(StoreError::Schema(_))
        ));
    }

    #[test]
    fn pgsslmode_values_are_explicitly_supported() {
        assert!(matches!(parse_ssl_mode("disable"), Ok(PgSslMode::Disable)));
        assert!(matches!(
            parse_ssl_mode("verify-full"),
            Ok(PgSslMode::VerifyFull)
        ));
        assert!(matches!(
            parse_ssl_mode("invalid"),
            Err(StoreError::Configuration)
        ));
    }

    #[test]
    fn inserts_are_transactional_and_idempotent() {
        assert!(ORIGIN_INSERT_SQL.contains("ON CONFLICT (tx_hash) DO NOTHING"));
        assert!(FEED_EVENT_INSERT_SQL.contains("ON CONFLICT (sequence_number, tx_hash) DO NOTHING"));
    }

    #[test]
    fn committed_migration_contains_required_recorder_constraints() {
        let migration = include_str!("../../migrations/001_init.sql");
        assert!(migration.contains("tx_hash TEXT NOT NULL UNIQUE"));
        assert!(migration.contains("UNIQUE (sequence_number, tx_hash)"));
        assert!(migration.contains("CHECK (chain_id = 42161)"));
    }

    #[test]
    fn duplicate_outcome_requires_both_rows_to_exist() {
        assert!(PersistOutcome::default().is_duplicate());
        assert!(!PersistOutcome {
            feed_event_inserted: true,
            origin_transaction_inserted: false,
        }
        .is_duplicate());
    }

    #[test]
    fn database_errors_do_not_include_connection_strings() {
        let display = StoreError::Connection.to_string();
        assert!(!display.contains("postgres://"));
        assert!(!display.to_ascii_lowercase().contains("password"));
    }
}
