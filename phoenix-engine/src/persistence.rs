use crate::engine_input::{EngineClassification, InputIdentity};
use async_trait::async_trait;
use chrono::{DateTime, Utc};
use serde_json::Value;
use sqlx::postgres::{PgConnectOptions, PgPoolOptions, PgSslMode};
use sqlx::types::Json;
use sqlx::{PgPool, Row};
use std::collections::{HashMap, HashSet};
use std::str::FromStr;
use std::time::Duration;
use thiserror::Error;

const MAX_EVIDENCE_BYTES: usize = 1024 * 1024;
const MAX_POOL_CONNECTIONS: u32 = 8;
const CONNECT_TIMEOUT: Duration = Duration::from_secs(5);

#[derive(Clone, Debug)]
pub struct ClassificationRecord {
    pub identity: InputIdentity,
    pub classification: EngineClassification,
    pub detail_class: Option<&'static str>,
    pub candidate_count: usize,
    pub decision_count: usize,
    pub delivery_attempt: u64,
    pub evidence: Value,
    pub first_received_at: DateTime<Utc>,
    pub completed_at: DateTime<Utc>,
    pub processing_latency_ns: u128,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PersistOutcome {
    Committed,
    AlreadyFinal,
}

#[derive(Clone, Debug, Error, PartialEq, Eq)]
pub enum StoreError {
    #[error("Engine PostgreSQL configuration is invalid")]
    Configuration,
    #[error("Engine PostgreSQL connection is unavailable")]
    Connection,
    #[error("Engine PostgreSQL schema is incompatible")]
    Schema,
    #[error("Engine PostgreSQL transaction failed")]
    Transaction,
    #[error("Engine classification evidence failed integrity validation")]
    Integrity,
}

#[async_trait]
pub trait ShadowStore: Send + Sync {
    async fn ping(&self) -> Result<(), StoreError>;
    async fn verify_schema(&self) -> Result<(), StoreError>;
    async fn final_classification(
        &self,
        source_event_identity: &str,
    ) -> Result<Option<EngineClassification>, StoreError>;
    async fn persist_classification(
        &self,
        record: &ClassificationRecord,
    ) -> Result<PersistOutcome, StoreError>;
}

#[derive(Clone, Debug)]
pub struct PostgresShadowStore {
    pool: PgPool,
}

impl PostgresShadowStore {
    pub async fn connect(dsn: &str, ssl_mode: &str) -> Result<Self, StoreError> {
        let options = PgConnectOptions::from_str(dsn)
            .map_err(|_| StoreError::Configuration)?
            .ssl_mode(parse_ssl_mode(ssl_mode)?);
        let pool = PgPoolOptions::new()
            .max_connections(MAX_POOL_CONNECTIONS)
            .acquire_timeout(CONNECT_TIMEOUT)
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
  AND table_name IN (
      'shadow_engine_classifications',
      'shadow_engine_processing_attempts'
  )
"#,
        )
        .fetch_all(&self.pool)
        .await
        .map_err(classify_sqlx_error)?;
        for row in rows {
            let table: String = row.try_get("table_name").map_err(classify_sqlx_error)?;
            let column: String = row.try_get("column_name").map_err(classify_sqlx_error)?;
            let data_type: String = row.try_get("data_type").map_err(classify_sqlx_error)?;
            let nullable: String = row.try_get("is_nullable").map_err(classify_sqlx_error)?;
            snapshot.columns.insert(
                (table, column),
                ColumnDefinition {
                    data_type,
                    nullable: nullable == "YES",
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
WHERE tc.constraint_schema = 'public'
  AND tc.table_name IN (
      'shadow_engine_classifications',
      'shadow_engine_processing_attempts'
  )
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
SELECT table_row.relname AS table_name,
       pg_get_constraintdef(constraint_row.oid) AS definition
FROM pg_constraint constraint_row
JOIN pg_class table_row ON table_row.oid = constraint_row.conrelid
JOIN pg_namespace namespace_row ON namespace_row.oid = table_row.relnamespace
WHERE namespace_row.nspname = 'public'
  AND table_row.relname IN (
      'shadow_engine_classifications',
      'shadow_engine_processing_attempts'
  )
  AND constraint_row.contype = 'c'
"#,
        )
        .fetch_all(&self.pool)
        .await
        .map_err(classify_sqlx_error)?;
        for row in rows {
            let table: String = row.try_get("table_name").map_err(classify_sqlx_error)?;
            let definition: String = row.try_get("definition").map_err(classify_sqlx_error)?;
            snapshot
                .check_constraints
                .entry(table)
                .or_default()
                .push(definition);
        }
        Ok(snapshot)
    }
}

#[async_trait]
impl ShadowStore for PostgresShadowStore {
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

    async fn final_classification(
        &self,
        source_event_identity: &str,
    ) -> Result<Option<EngineClassification>, StoreError> {
        if !valid_identity(source_event_identity) {
            return Err(StoreError::Integrity);
        }
        let row = sqlx::query(
            "SELECT classification FROM shadow_engine_classifications \
             WHERE source_event_identity = $1",
        )
        .bind(source_event_identity)
        .fetch_optional(&self.pool)
        .await
        .map_err(classify_sqlx_error)?;
        let Some(row) = row else {
            return Ok(None);
        };
        let value: String = row.try_get("classification").map_err(classify_sqlx_error)?;
        let classification = EngineClassification::parse(&value).ok_or(StoreError::Integrity)?;
        Ok(classification.is_final().then_some(classification))
    }

    async fn persist_classification(
        &self,
        record: &ClassificationRecord,
    ) -> Result<PersistOutcome, StoreError> {
        validate_record(record)?;
        let mut transaction = self.pool.begin().await.map_err(classify_sqlx_error)?;

        let existing = sqlx::query(
            "SELECT classification FROM shadow_engine_classifications \
             WHERE source_event_identity = $1 FOR UPDATE",
        )
        .bind(&record.identity.source_event_identity)
        .fetch_optional(&mut *transaction)
        .await
        .map_err(classify_sqlx_error)?;
        if let Some(row) = existing {
            let value: String = row.try_get("classification").map_err(classify_sqlx_error)?;
            let classification =
                EngineClassification::parse(&value).ok_or(StoreError::Integrity)?;
            if classification.is_final() {
                transaction.commit().await.map_err(classify_sqlx_error)?;
                return Ok(PersistOutcome::AlreadyFinal);
            }
        }

        sqlx::query(
            r#"
INSERT INTO shadow_engine_processing_attempts (
    source_event_identity,
    delivery_attempt,
    classification,
    error_class,
    evidence,
    started_at,
    completed_at,
    processing_latency_ns
) VALUES ($1, $2, $3, $4, $5, $6, $7, CAST($8 AS numeric))
ON CONFLICT (source_event_identity, delivery_attempt) DO NOTHING
"#,
        )
        .bind(&record.identity.source_event_identity)
        .bind(record.delivery_attempt as i64)
        .bind(record.classification.as_str())
        .bind(record.detail_class)
        .bind(Json(&record.evidence))
        .bind(record.first_received_at)
        .bind(record.completed_at)
        .bind(record.processing_latency_ns.to_string())
        .execute(&mut *transaction)
        .await
        .map_err(classify_sqlx_error)?;

        sqlx::query(
            r#"
INSERT INTO shadow_engine_classifications (
    source_event_identity,
    schema_version,
    source_sequence,
    tx_hash,
    chain_id,
    classification,
    detail_class,
    candidate_count,
    decision_count,
    delivery_attempts,
    evidence,
    first_received_at,
    classified_at,
    processing_latency_ns
) VALUES (
    $1,
    'phoenix.engine.input.v1',
    CAST($2 AS numeric),
    $3,
    $4,
    $5,
    $6,
    $7,
    $8,
    $9,
    $10,
    $11,
    $12,
    CAST($13 AS numeric)
)
ON CONFLICT (source_event_identity) DO UPDATE SET
    classification = EXCLUDED.classification,
    detail_class = EXCLUDED.detail_class,
    candidate_count = EXCLUDED.candidate_count,
    decision_count = EXCLUDED.decision_count,
    delivery_attempts = GREATEST(
        shadow_engine_classifications.delivery_attempts,
        EXCLUDED.delivery_attempts
    ),
    evidence = EXCLUDED.evidence,
    classified_at = EXCLUDED.classified_at,
    processing_latency_ns = EXCLUDED.processing_latency_ns,
    updated_at = now()
"#,
        )
        .bind(&record.identity.source_event_identity)
        .bind(record.identity.source_sequence.to_string())
        .bind(&record.identity.tx_hash)
        .bind(record.identity.chain_id as i64)
        .bind(record.classification.as_str())
        .bind(record.detail_class)
        .bind(record.candidate_count as i32)
        .bind(record.decision_count as i32)
        .bind(record.delivery_attempt as i32)
        .bind(Json(&record.evidence))
        .bind(record.first_received_at)
        .bind(record.completed_at)
        .bind(record.processing_latency_ns.to_string())
        .execute(&mut *transaction)
        .await
        .map_err(classify_sqlx_error)?;

        transaction.commit().await.map_err(classify_sqlx_error)?;
        Ok(PersistOutcome::Committed)
    }
}

#[derive(Clone, Debug, Default)]
pub struct SchemaSnapshot {
    columns: HashMap<(String, String), ColumnDefinition>,
    unique_constraints: HashMap<String, HashSet<Vec<String>>>,
    check_constraints: HashMap<String, Vec<String>>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct ColumnDefinition {
    data_type: String,
    nullable: bool,
}

const REQUIRED_COLUMNS: &[(&str, &str, &str, bool)] = &[
    (
        "shadow_engine_classifications",
        "source_event_identity",
        "text",
        false,
    ),
    (
        "shadow_engine_classifications",
        "schema_version",
        "text",
        false,
    ),
    (
        "shadow_engine_classifications",
        "source_sequence",
        "numeric",
        false,
    ),
    ("shadow_engine_classifications", "tx_hash", "text", false),
    ("shadow_engine_classifications", "chain_id", "bigint", false),
    (
        "shadow_engine_classifications",
        "classification",
        "text",
        false,
    ),
    (
        "shadow_engine_classifications",
        "detail_class",
        "text",
        true,
    ),
    (
        "shadow_engine_classifications",
        "candidate_count",
        "integer",
        false,
    ),
    (
        "shadow_engine_classifications",
        "decision_count",
        "integer",
        false,
    ),
    (
        "shadow_engine_classifications",
        "delivery_attempts",
        "integer",
        false,
    ),
    ("shadow_engine_classifications", "evidence", "jsonb", false),
    (
        "shadow_engine_classifications",
        "first_received_at",
        "timestamp with time zone",
        false,
    ),
    (
        "shadow_engine_classifications",
        "classified_at",
        "timestamp with time zone",
        false,
    ),
    (
        "shadow_engine_classifications",
        "processing_latency_ns",
        "numeric",
        false,
    ),
    ("shadow_engine_processing_attempts", "id", "bigint", false),
    (
        "shadow_engine_processing_attempts",
        "source_event_identity",
        "text",
        false,
    ),
    (
        "shadow_engine_processing_attempts",
        "delivery_attempt",
        "bigint",
        false,
    ),
    (
        "shadow_engine_processing_attempts",
        "classification",
        "text",
        false,
    ),
    (
        "shadow_engine_processing_attempts",
        "error_class",
        "text",
        true,
    ),
    (
        "shadow_engine_processing_attempts",
        "evidence",
        "jsonb",
        false,
    ),
    (
        "shadow_engine_processing_attempts",
        "started_at",
        "timestamp with time zone",
        false,
    ),
    (
        "shadow_engine_processing_attempts",
        "completed_at",
        "timestamp with time zone",
        false,
    ),
    (
        "shadow_engine_processing_attempts",
        "processing_latency_ns",
        "numeric",
        false,
    ),
];

pub fn validate_schema_snapshot(snapshot: &SchemaSnapshot) -> Result<(), StoreError> {
    for (table, column, data_type, nullable) in REQUIRED_COLUMNS {
        let actual = snapshot
            .columns
            .get(&(table.to_string(), column.to_string()));
        if actual
            != Some(&ColumnDefinition {
                data_type: data_type.to_string(),
                nullable: *nullable,
            })
        {
            return Err(StoreError::Schema);
        }
    }
    require_unique(
        snapshot,
        "shadow_engine_classifications",
        &["source_event_identity"],
    )?;
    require_unique(
        snapshot,
        "shadow_engine_classifications",
        &["source_sequence", "tx_hash"],
    )?;
    require_unique(
        snapshot,
        "shadow_engine_processing_attempts",
        &["source_event_identity", "delivery_attempt"],
    )?;

    let checks = snapshot
        .check_constraints
        .values()
        .flatten()
        .map(|value| value.to_ascii_lowercase().replace([' ', '(', ')'], ""))
        .collect::<Vec<_>>()
        .join(" ");
    for required in [
        "chain_id=42161",
        "classification=any",
        "octet_lengthevidence::text<=1048576",
        "delivery_attempt>=1",
    ] {
        if !checks.contains(required) {
            return Err(StoreError::Schema);
        }
    }
    Ok(())
}

fn require_unique(
    snapshot: &SchemaSnapshot,
    table: &str,
    columns: &[&str],
) -> Result<(), StoreError> {
    let columns = columns
        .iter()
        .map(|value| (*value).to_string())
        .collect::<Vec<_>>();
    if snapshot
        .unique_constraints
        .get(table)
        .is_some_and(|constraints| constraints.contains(&columns))
    {
        Ok(())
    } else {
        Err(StoreError::Schema)
    }
}

fn validate_record(record: &ClassificationRecord) -> Result<(), StoreError> {
    let evidence_bytes = serde_json::to_vec(&record.evidence).map_err(|_| StoreError::Integrity)?;
    if !valid_identity(&record.identity.source_event_identity)
        || !valid_tx_hash(&record.identity.tx_hash)
        || record.identity.chain_id != 42161
        || record.delivery_attempt == 0
        || record.delivery_attempt > i32::MAX as u64
        || record.candidate_count > i32::MAX as usize
        || record.decision_count > i32::MAX as usize
        || !record.evidence.is_object()
        || evidence_bytes.len() > MAX_EVIDENCE_BYTES
        || record
            .detail_class
            .is_some_and(|value| value.is_empty() || value.len() > 128)
        || record.completed_at < record.first_received_at
    {
        return Err(StoreError::Integrity);
    }
    Ok(())
}

fn valid_identity(value: &str) -> bool {
    !value.is_empty() && value.len() <= 200 && !value.chars().any(char::is_control)
}

fn valid_tx_hash(value: &str) -> bool {
    value.len() == 66
        && value.starts_with("0x")
        && value[2..]
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
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
    use serde_json::json;

    fn valid_snapshot() -> SchemaSnapshot {
        let mut snapshot = SchemaSnapshot::default();
        for (table, column, data_type, nullable) in REQUIRED_COLUMNS {
            snapshot.columns.insert(
                (table.to_string(), column.to_string()),
                ColumnDefinition {
                    data_type: data_type.to_string(),
                    nullable: *nullable,
                },
            );
        }
        snapshot
            .unique_constraints
            .entry("shadow_engine_classifications".to_string())
            .or_default()
            .extend([
                vec!["source_event_identity".to_string()],
                vec!["source_sequence".to_string(), "tx_hash".to_string()],
            ]);
        snapshot
            .unique_constraints
            .entry("shadow_engine_processing_attempts".to_string())
            .or_default()
            .insert(vec![
                "source_event_identity".to_string(),
                "delivery_attempt".to_string(),
            ]);
        snapshot.check_constraints.insert(
            "shadow_engine_classifications".to_string(),
            vec![
                "CHECK ((chain_id = 42161))".to_string(),
                "CHECK ((classification = ANY (...)))".to_string(),
                "CHECK ((octet_length((evidence)::text) <= 1048576))".to_string(),
            ],
        );
        snapshot.check_constraints.insert(
            "shadow_engine_processing_attempts".to_string(),
            vec!["CHECK ((delivery_attempt >= 1))".to_string()],
        );
        snapshot
    }

    fn record() -> ClassificationRecord {
        let now = Utc::now();
        ClassificationRecord {
            identity: InputIdentity {
                source_event_identity: format!("phoenix.engine.input.v1:7:0x{}", "a".repeat(64)),
                source_sequence: 7,
                tx_hash: format!("0x{}", "a".repeat(64)),
                chain_id: 42161,
            },
            classification: EngineClassification::NoRelevantRoute,
            detail_class: Some("irrelevant_origin"),
            candidate_count: 0,
            decision_count: 0,
            delivery_attempt: 1,
            evidence: json!({"origin_classification": "irrelevant"}),
            first_received_at: now,
            completed_at: now,
            processing_latency_ns: 1,
        }
    }

    #[test]
    fn exact_runtime_schema_contract_is_required() {
        assert_eq!(validate_schema_snapshot(&valid_snapshot()), Ok(()));
        let mut invalid = valid_snapshot();
        invalid.columns.remove(&(
            "shadow_engine_classifications".to_string(),
            "classification".to_string(),
        ));
        assert_eq!(validate_schema_snapshot(&invalid), Err(StoreError::Schema));
    }

    #[test]
    fn classification_evidence_is_bounded_and_identity_checked() {
        assert_eq!(validate_record(&record()), Ok(()));
        let mut invalid = record();
        invalid.identity.tx_hash = "0xWRONG".to_string();
        assert_eq!(validate_record(&invalid), Err(StoreError::Integrity));

        let mut oversized = record();
        oversized.evidence = json!({"bounded": "x".repeat(MAX_EVIDENCE_BYTES)});
        assert_eq!(validate_record(&oversized), Err(StoreError::Integrity));
    }

    #[test]
    fn database_errors_and_configuration_do_not_echo_secrets() {
        for error in [
            StoreError::Configuration,
            StoreError::Connection,
            StoreError::Schema,
            StoreError::Transaction,
            StoreError::Integrity,
        ] {
            let rendered = error.to_string().to_ascii_lowercase();
            assert!(!rendered.contains("postgres://"));
            assert!(!rendered.contains("password"));
        }
        assert!(matches!(
            parse_ssl_mode("verify-full"),
            Ok(PgSslMode::VerifyFull)
        ));
        assert!(matches!(
            parse_ssl_mode("wrong"),
            Err(StoreError::Configuration)
        ));
    }

    #[test]
    fn committed_migration_has_atomic_classification_and_attempt_ledgers() {
        let migration = include_str!("../../migrations/004_shadow_engine_runtime.sql");
        for required in [
            "CREATE TABLE IF NOT EXISTS shadow_engine_classifications",
            "CREATE TABLE IF NOT EXISTS shadow_engine_processing_attempts",
            "UNIQUE (source_event_identity, delivery_attempt)",
            "processing_latency_ns NUMERIC(78,0) NOT NULL",
        ] {
            assert!(migration.contains(required));
        }
    }
}
