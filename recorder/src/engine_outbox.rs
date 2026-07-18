use async_trait::async_trait;
use chrono::{DateTime, Utc};
use serde_json::Value;
use sqlx::postgres::{PgConnectOptions, PgPoolOptions, PgSslMode};
use sqlx::types::Json;
use sqlx::{PgPool, Row};
use std::str::FromStr;
use std::time::Duration;
use thiserror::Error;

pub const MAX_CLAIM_BATCH: usize = 64;
pub const MAX_OWNER_BYTES: usize = 128;
pub const MAX_TELEMETRY_STATEMENT_TIMEOUT: Duration = Duration::from_secs(5);

pub const PENDING_ROWS_ESTIMATE_SQL: &str = r#"
SELECT GREATEST(COALESCE(index_relation.reltuples, 0), 0)::double precision
           AS pending_rows_estimate
FROM pg_class AS index_relation
JOIN pg_namespace AS namespace
  ON namespace.oid = index_relation.relnamespace
WHERE namespace.nspname = 'public'
  AND index_relation.relname = 'engine_outbox_pending_idx'
"#;

pub const OLDEST_CLAIMABLE_SQL: &str = r#"
SELECT GREATEST(
           EXTRACT(EPOCH FROM (now() - created_at)),
           0
       )::double precision AS oldest_claimable_age_seconds
FROM engine_outbox
WHERE published_at IS NULL
  AND available_at <= now()
  AND (claim_expires_at IS NULL OR claim_expires_at <= now())
ORDER BY available_at, created_at, outbox_id
LIMIT 1
"#;

pub const CLAIM_BATCH_SQL: &str = r#"
WITH claimable AS (
    SELECT outbox_id
    FROM engine_outbox
    WHERE published_at IS NULL
      AND available_at <= now()
      AND (claim_expires_at IS NULL OR claim_expires_at <= now())
    ORDER BY available_at, created_at, outbox_id
    FOR UPDATE SKIP LOCKED
    LIMIT $1
)
UPDATE engine_outbox AS outbox
SET claim_owner = $2,
    claimed_at = now(),
    claim_expires_at = now() + ($3 * interval '1 second'),
    publish_attempts = outbox.publish_attempts + 1
FROM claimable
WHERE outbox.outbox_id = claimable.outbox_id
RETURNING outbox.outbox_id,
          outbox.schema_version,
          outbox.source_event_identity,
          outbox.source_sequence::text AS source_sequence,
          outbox.tx_hash,
          outbox.chain_id,
          outbox.payload,
          outbox.created_at,
          outbox.publish_attempts
"#;

#[derive(Clone, Debug, PartialEq)]
pub struct OutboxRow {
    pub outbox_id: String,
    pub schema_version: String,
    pub source_event_identity: String,
    pub source_sequence: u64,
    pub tx_hash: String,
    pub chain_id: u64,
    pub payload: Value,
    pub created_at: DateTime<Utc>,
    pub publish_attempts: u32,
}

#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct BacklogTelemetry {
    pub pending_rows_estimate: u64,
    pub oldest_claimable_age_seconds: f64,
}

#[derive(Clone, Debug, Error, PartialEq, Eq)]
pub enum OutboxError {
    #[error("outbox PostgreSQL configuration is invalid")]
    Configuration,
    #[error("outbox PostgreSQL connection is unavailable")]
    Connection,
    #[error("required outbox schema is missing or incompatible")]
    Schema,
    #[error("outbox PostgreSQL transaction failed")]
    Transaction,
    #[error("outbox row contains invalid canonical evidence")]
    Integrity,
    #[error("outbox claim ownership was lost")]
    ClaimLost,
}

impl OutboxError {
    pub const fn class(&self) -> &'static str {
        match self {
            Self::Configuration => "postgres_configuration",
            Self::Connection => "postgres_connection",
            Self::Schema => "postgres_schema",
            Self::Transaction => "postgres_transaction",
            Self::Integrity => "outbox_integrity",
            Self::ClaimLost => "claim_lost",
        }
    }
}

#[async_trait]
pub trait OutboxStore: Send + Sync {
    async fn ping(&self) -> Result<(), OutboxError>;
    async fn verify_schema(&self) -> Result<(), OutboxError>;
    async fn claim_batch(
        &self,
        owner: &str,
        max_rows: usize,
        lease: Duration,
    ) -> Result<Vec<OutboxRow>, OutboxError>;
    async fn mark_published(
        &self,
        outbox_id: &str,
        owner: &str,
        ack_sequence: u64,
    ) -> Result<(), OutboxError>;
    async fn release_for_retry(
        &self,
        outbox_id: &str,
        owner: &str,
        error_class: &'static str,
        delay: Duration,
    ) -> Result<(), OutboxError>;
    async fn backlog_telemetry(
        &self,
        statement_timeout: Duration,
    ) -> Result<BacklogTelemetry, OutboxError>;
}

#[derive(Clone, Debug)]
pub struct PostgresOutbox {
    pool: PgPool,
}

impl PostgresOutbox {
    pub async fn connect(dsn: &str, ssl_mode: &str) -> Result<Self, OutboxError> {
        let options = PgConnectOptions::from_str(dsn)
            .map_err(|_| OutboxError::Configuration)?
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
}

#[async_trait]
impl OutboxStore for PostgresOutbox {
    async fn ping(&self) -> Result<(), OutboxError> {
        sqlx::query("SELECT 1")
            .execute(&self.pool)
            .await
            .map_err(classify_sqlx_error)?;
        Ok(())
    }

    async fn verify_schema(&self) -> Result<(), OutboxError> {
        let (table_present, pending_index_present, retry_index_present): (bool, bool, bool) =
            sqlx::query_as(
                r#"
SELECT to_regclass('public.engine_outbox') IS NOT NULL,
       to_regclass('public.engine_outbox_pending_idx') IS NOT NULL,
       to_regclass('public.engine_outbox_retry_idx') IS NOT NULL
"#,
            )
            .fetch_one(&self.pool)
            .await
            .map_err(classify_sqlx_error)?;
        if table_present && pending_index_present && retry_index_present {
            Ok(())
        } else {
            Err(OutboxError::Schema)
        }
    }

    async fn claim_batch(
        &self,
        owner: &str,
        max_rows: usize,
        lease: Duration,
    ) -> Result<Vec<OutboxRow>, OutboxError> {
        validate_claim(owner, max_rows, lease)?;
        let rows = sqlx::query(CLAIM_BATCH_SQL)
            .bind(max_rows as i64)
            .bind(owner)
            .bind(lease.as_secs() as i64)
            .fetch_all(&self.pool)
            .await
            .map_err(classify_sqlx_error)?;
        rows.into_iter().map(decode_row).collect()
    }

    async fn mark_published(
        &self,
        outbox_id: &str,
        owner: &str,
        ack_sequence: u64,
    ) -> Result<(), OutboxError> {
        let result = sqlx::query(
            r#"
UPDATE engine_outbox
SET published_at = now(),
    jetstream_ack_sequence = $3::numeric,
    claim_owner = NULL,
    claimed_at = NULL,
    claim_expires_at = NULL,
    last_error_class = NULL,
    last_error_at = NULL
WHERE outbox_id = $1
  AND claim_owner = $2
  AND published_at IS NULL
"#,
        )
        .bind(outbox_id)
        .bind(owner)
        .bind(ack_sequence.to_string())
        .execute(&self.pool)
        .await
        .map_err(classify_sqlx_error)?;
        require_single_claim(result.rows_affected())
    }

    async fn release_for_retry(
        &self,
        outbox_id: &str,
        owner: &str,
        error_class: &'static str,
        delay: Duration,
    ) -> Result<(), OutboxError> {
        if error_class.is_empty() || error_class.len() > 64 || delay.is_zero() {
            return Err(OutboxError::Configuration);
        }
        let result = sqlx::query(
            r#"
UPDATE engine_outbox
SET available_at = now() + ($3 * interval '1 millisecond'),
    claim_owner = NULL,
    claimed_at = NULL,
    claim_expires_at = NULL,
    last_error_class = $4,
    last_error_at = now()
WHERE outbox_id = $1
  AND claim_owner = $2
  AND published_at IS NULL
"#,
        )
        .bind(outbox_id)
        .bind(owner)
        .bind(delay.as_millis().min(i64::MAX as u128) as i64)
        .bind(error_class)
        .execute(&self.pool)
        .await
        .map_err(classify_sqlx_error)?;
        require_single_claim(result.rows_affected())
    }

    async fn backlog_telemetry(
        &self,
        statement_timeout: Duration,
    ) -> Result<BacklogTelemetry, OutboxError> {
        if statement_timeout.is_zero() || statement_timeout > MAX_TELEMETRY_STATEMENT_TIMEOUT {
            return Err(OutboxError::Configuration);
        }
        let timeout_millis = statement_timeout.as_millis().min(u64::MAX as u128) as u64;
        let mut transaction = self.pool.begin().await.map_err(classify_sqlx_error)?;
        sqlx::query("SELECT set_config('statement_timeout', $1, true)")
            .bind(format!("{timeout_millis}ms"))
            .execute(&mut *transaction)
            .await
            .map_err(classify_sqlx_error)?;

        let estimate: f64 = sqlx::query(PENDING_ROWS_ESTIMATE_SQL)
            .fetch_optional(&mut *transaction)
            .await
            .map_err(classify_sqlx_error)?
            .ok_or(OutboxError::Schema)?
            .try_get("pending_rows_estimate")
            .map_err(classify_sqlx_error)?;
        let oldest_claimable_age_seconds = sqlx::query(OLDEST_CLAIMABLE_SQL)
            .fetch_optional(&mut *transaction)
            .await
            .map_err(classify_sqlx_error)?
            .map(|row| {
                row.try_get::<f64, _>("oldest_claimable_age_seconds")
                    .map_err(classify_sqlx_error)
            })
            .transpose()?
            .unwrap_or_default();
        transaction.commit().await.map_err(classify_sqlx_error)?;

        Ok(BacklogTelemetry {
            pending_rows_estimate: estimate.max(0.0).round().min(u64::MAX as f64) as u64,
            oldest_claimable_age_seconds: oldest_claimable_age_seconds.max(0.0),
        })
    }
}

fn decode_row(row: sqlx::postgres::PgRow) -> Result<OutboxRow, OutboxError> {
    let source_sequence = row
        .try_get::<String, _>("source_sequence")
        .map_err(classify_sqlx_error)?
        .parse::<u64>()
        .map_err(|_| OutboxError::Integrity)?;
    let chain_id = row
        .try_get::<i64, _>("chain_id")
        .map_err(classify_sqlx_error)?;
    let publish_attempts = row
        .try_get::<i32, _>("publish_attempts")
        .map_err(classify_sqlx_error)?;
    if chain_id < 0 || publish_attempts < 1 {
        return Err(OutboxError::Integrity);
    }
    let payload: Json<Value> = row.try_get("payload").map_err(classify_sqlx_error)?;
    Ok(OutboxRow {
        outbox_id: row.try_get("outbox_id").map_err(classify_sqlx_error)?,
        schema_version: row.try_get("schema_version").map_err(classify_sqlx_error)?,
        source_event_identity: row
            .try_get("source_event_identity")
            .map_err(classify_sqlx_error)?,
        source_sequence,
        tx_hash: row.try_get("tx_hash").map_err(classify_sqlx_error)?,
        chain_id: chain_id as u64,
        payload: payload.0,
        created_at: row.try_get("created_at").map_err(classify_sqlx_error)?,
        publish_attempts: publish_attempts as u32,
    })
}

fn validate_claim(owner: &str, max_rows: usize, lease: Duration) -> Result<(), OutboxError> {
    if owner.is_empty()
        || owner.len() > MAX_OWNER_BYTES
        || max_rows == 0
        || max_rows > MAX_CLAIM_BATCH
        || lease.is_zero()
        || lease > Duration::from_secs(5 * 60)
    {
        Err(OutboxError::Configuration)
    } else {
        Ok(())
    }
}

fn require_single_claim(rows_affected: u64) -> Result<(), OutboxError> {
    if rows_affected == 1 {
        Ok(())
    } else {
        Err(OutboxError::ClaimLost)
    }
}

fn parse_ssl_mode(value: &str) -> Result<PgSslMode, OutboxError> {
    match value.trim().to_ascii_lowercase().as_str() {
        "disable" => Ok(PgSslMode::Disable),
        "allow" => Ok(PgSslMode::Allow),
        "prefer" | "" => Ok(PgSslMode::Prefer),
        "require" => Ok(PgSslMode::Require),
        "verify-ca" => Ok(PgSslMode::VerifyCa),
        "verify-full" => Ok(PgSslMode::VerifyFull),
        _ => Err(OutboxError::Configuration),
    }
}

fn classify_sqlx_error(error: sqlx::Error) -> OutboxError {
    match error {
        sqlx::Error::Configuration(_) => OutboxError::Configuration,
        sqlx::Error::Io(_)
        | sqlx::Error::Tls(_)
        | sqlx::Error::PoolTimedOut
        | sqlx::Error::PoolClosed
        | sqlx::Error::WorkerCrashed => OutboxError::Connection,
        _ => OutboxError::Transaction,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn claim_query_is_concurrent_bounded_and_non_destructive() {
        assert!(CLAIM_BATCH_SQL.contains("FOR UPDATE SKIP LOCKED"));
        assert!(CLAIM_BATCH_SQL.contains("LIMIT $1"));
        assert!(CLAIM_BATCH_SQL.contains("claim_expires_at <= now()"));
        for forbidden in ["DELETE", "TRUNCATE", "DROP"] {
            assert!(!CLAIM_BATCH_SQL.contains(forbidden));
        }
    }

    #[test]
    fn backlog_queries_are_bounded_estimates_without_table_aggregates() {
        assert!(PENDING_ROWS_ESTIMATE_SQL.contains("reltuples"));
        assert!(PENDING_ROWS_ESTIMATE_SQL.contains("engine_outbox_pending_idx"));
        assert!(!PENDING_ROWS_ESTIMATE_SQL
            .to_ascii_uppercase()
            .contains("COUNT("));
        assert!(!PENDING_ROWS_ESTIMATE_SQL
            .to_ascii_uppercase()
            .contains("MIN("));
        assert!(OLDEST_CLAIMABLE_SQL.contains("ORDER BY available_at, created_at, outbox_id"));
        assert!(OLDEST_CLAIMABLE_SQL.contains("LIMIT 1"));
        assert!(!OLDEST_CLAIMABLE_SQL.to_ascii_uppercase().contains("COUNT("));
        assert!(!OLDEST_CLAIMABLE_SQL.to_ascii_uppercase().contains("MIN("));
    }

    #[test]
    fn claim_configuration_is_bounded() {
        assert!(validate_claim("dispatcher-1", 64, Duration::from_secs(30)).is_ok());
        assert_eq!(
            validate_claim("dispatcher-1", 65, Duration::from_secs(30)),
            Err(OutboxError::Configuration)
        );
        assert_eq!(
            validate_claim("", 1, Duration::from_secs(30)),
            Err(OutboxError::Configuration)
        );
    }

    #[test]
    fn errors_are_sanitized_and_bounded() {
        for error in [
            OutboxError::Configuration,
            OutboxError::Connection,
            OutboxError::Schema,
            OutboxError::Transaction,
            OutboxError::Integrity,
            OutboxError::ClaimLost,
        ] {
            let rendered = error.to_string().to_ascii_lowercase();
            assert!(!rendered.contains("postgres://"));
            assert!(!rendered.contains("password"));
            assert!(error.class().len() <= 64);
        }
    }
}
