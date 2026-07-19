use crate::ingress::{IngressAggregate, IngressFlushBatch, IngressSample, INGRESS_SCHEMA_VERSION};
use crate::model::{
    engine_event_identity, ValidatedMessage, ENGINE_INPUT_SCHEMA_VERSION, ORIGIN_CLASSIFICATION,
};
use async_trait::async_trait;
use sha2::{Digest, Sha256};
use sqlx::postgres::{PgConnectOptions, PgPoolOptions, PgSslMode};
use sqlx::types::Json;
use sqlx::{PgPool, Postgres, QueryBuilder, Row};
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
    ("engine_outbox", "outbox_id", "text", false),
    ("engine_outbox", "schema_version", "text", false),
    ("engine_outbox", "source_event_identity", "text", false),
    ("engine_outbox", "source_sequence", "numeric", false),
    ("engine_outbox", "tx_hash", "text", false),
    ("engine_outbox", "chain_id", "bigint", false),
    ("engine_outbox", "payload", "jsonb", false),
    (
        "engine_outbox",
        "created_at",
        "timestamp with time zone",
        false,
    ),
    (
        "engine_outbox",
        "available_at",
        "timestamp with time zone",
        false,
    ),
    ("engine_outbox", "publish_attempts", "integer", false),
    (
        "engine_outbox",
        "published_at",
        "timestamp with time zone",
        true,
    ),
    ("engine_outbox", "jetstream_ack_sequence", "numeric", true),
    ("engine_outbox", "last_error_class", "text", true),
    (
        "engine_outbox",
        "last_error_at",
        "timestamp with time zone",
        true,
    ),
    ("engine_outbox", "claim_owner", "text", true),
    (
        "engine_outbox",
        "claimed_at",
        "timestamp with time zone",
        true,
    ),
    (
        "engine_outbox",
        "claim_expires_at",
        "timestamp with time zone",
        true,
    ),
    ("money_path_ingress_daily", "bucket_date", "date", false),
    ("money_path_ingress_daily", "classification", "text", false),
    ("money_path_ingress_daily", "detail_class", "text", false),
    ("money_path_ingress_daily", "router_kind", "text", false),
    ("money_path_ingress_daily", "wrapper_kind", "text", false),
    ("money_path_ingress_daily", "selector_kind", "text", false),
    ("money_path_ingress_daily", "event_count", "bigint", false),
    (
        "money_path_ingress_daily",
        "first_seen_at",
        "timestamp with time zone",
        false,
    ),
    (
        "money_path_ingress_daily",
        "last_seen_at",
        "timestamp with time zone",
        false,
    ),
    ("money_path_ingress_daily", "schema_version", "text", false),
    ("money_path_ingress_samples", "bucket_date", "date", false),
    (
        "money_path_ingress_samples",
        "classification",
        "text",
        false,
    ),
    ("money_path_ingress_samples", "detail_class", "text", false),
    ("money_path_ingress_samples", "router_kind", "text", false),
    ("money_path_ingress_samples", "wrapper_kind", "text", false),
    ("money_path_ingress_samples", "selector_kind", "text", false),
    (
        "money_path_ingress_samples",
        "sample_ordinal",
        "smallint",
        false,
    ),
    (
        "money_path_ingress_samples",
        "sample_fingerprint",
        "bytea",
        false,
    ),
    (
        "money_path_ingress_samples",
        "safe_decoder_summary",
        "jsonb",
        false,
    ),
    (
        "money_path_ingress_samples",
        "observed_at",
        "timestamp with time zone",
        false,
    ),
    (
        "money_path_ingress_samples",
        "schema_version",
        "text",
        false,
    ),
];

const ORIGIN_BATCH_INSERT_PREFIX: &str = r#"INSERT INTO origin_transactions (
    tx_hash, sequence_number, chain_id, router, classification, calldata, seen_at, metadata
) "#;

const FEED_EVENT_BATCH_INSERT_PREFIX: &str =
    "INSERT INTO feed_events (sequence_number, tx_hash, payload, recorded_at) ";

const ENGINE_OUTBOX_BATCH_INSERT_PREFIX: &str = r#"INSERT INTO engine_outbox (
    outbox_id, schema_version, source_event_identity, source_sequence,
    tx_hash, chain_id, payload
) "#;

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct SchemaSnapshot {
    pub columns: BTreeMap<String, BTreeMap<String, ColumnDefinition>>,
    pub unique_constraints: BTreeMap<String, BTreeSet<Vec<String>>>,
    pub origin_chain_checks: Vec<String>,
    pub outbox_checks: Vec<String>,
    pub indexes: BTreeSet<String>,
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
    pub engine_outbox_inserted: bool,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct IngressPersistOutcome {
    pub aggregate_rows_upserted: u64,
    pub samples_inserted: u64,
    pub sample_limit_reached: u64,
}

impl PersistOutcome {
    pub fn is_duplicate(&self) -> bool {
        !self.feed_event_inserted
            && !self.origin_transaction_inserted
            && !self.engine_outbox_inserted
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
    async fn persist_batch(
        &self,
        messages: &[ValidatedMessage],
    ) -> Result<Vec<PersistOutcome>, StoreError>;
    async fn persist_ingress_evidence(
        &self,
        _batch: &IngressFlushBatch,
        _sample_limit: usize,
    ) -> Result<IngressPersistOutcome, StoreError> {
        Ok(IngressPersistOutcome::default())
    }
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

    #[doc(hidden)]
    pub fn from_pool(pool: PgPool) -> Self {
        Self { pool }
    }

    async fn load_schema_snapshot(&self) -> Result<SchemaSnapshot, StoreError> {
        let mut snapshot = SchemaSnapshot::default();
        let rows = sqlx::query(
            r#"
SELECT table_name, column_name, data_type, is_nullable
FROM information_schema.columns
WHERE table_schema = 'public'
  AND table_name IN (
      'feed_events',
      'origin_transactions',
      'engine_outbox',
      'money_path_ingress_daily',
      'money_path_ingress_samples'
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
  AND tc.table_name IN (
      'feed_events',
      'origin_transactions',
      'engine_outbox',
      'money_path_ingress_daily',
      'money_path_ingress_samples'
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
  AND table_row.relname IN ('origin_transactions', 'engine_outbox')
  AND constraint_row.contype = 'c'
"#,
        )
        .fetch_all(&self.pool)
        .await
        .map_err(classify_sqlx_error)?;
        for row in rows {
            let table: String = row.try_get("table_name").map_err(classify_sqlx_error)?;
            let definition: String = row.try_get("definition").map_err(classify_sqlx_error)?;
            if table == "origin_transactions" {
                snapshot.origin_chain_checks.push(definition);
            } else {
                snapshot.outbox_checks.push(definition);
            }
        }

        let rows = sqlx::query(
            r#"
SELECT indexname
FROM pg_indexes
WHERE schemaname = 'public'
  AND tablename = 'engine_outbox'
"#,
        )
        .fetch_all(&self.pool)
        .await
        .map_err(classify_sqlx_error)?;
        for row in rows {
            snapshot
                .indexes
                .insert(row.try_get("indexname").map_err(classify_sqlx_error)?);
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

    async fn persist_batch(
        &self,
        messages: &[ValidatedMessage],
    ) -> Result<Vec<PersistOutcome>, StoreError> {
        if messages.is_empty() {
            return Ok(Vec::new());
        }
        let mut transaction = self.pool.begin().await.map_err(classify_sqlx_error)?;

        let mut origin_query = QueryBuilder::<Postgres>::new(ORIGIN_BATCH_INSERT_PREFIX);
        origin_query.push_values(messages, |mut row, message| {
            let router = (!message.tx.to.is_empty()).then_some(message.tx.to.as_str());
            row.push_bind(&message.tx.tx_hash)
                .push_bind(message.tx.sequence.to_string())
                .push_unseparated("::numeric")
                .push_bind(message.tx.chain_id as i64)
                .push_bind(router)
                .push_bind(ORIGIN_CLASSIFICATION)
                .push_bind(&message.calldata)
                .push_bind(message.seen_at)
                .push_bind(Json(&message.metadata));
        });
        origin_query.push(" ON CONFLICT (tx_hash) DO NOTHING RETURNING tx_hash");
        let inserted_origins = origin_query
            .build_query_as::<(String,)>()
            .fetch_all(&mut *transaction)
            .await
            .map_err(classify_sqlx_error)?
            .into_iter()
            .map(|(tx_hash,)| tx_hash)
            .collect::<BTreeSet<_>>();

        let mut event_query = QueryBuilder::<Postgres>::new(FEED_EVENT_BATCH_INSERT_PREFIX);
        event_query.push_values(messages, |mut row, message| {
            row.push_bind(message.tx.sequence.to_string())
                .push_unseparated("::numeric")
                .push_bind(&message.tx.tx_hash)
                .push_bind(Json(&message.payload))
                .push_bind(message.seen_at);
        });
        event_query.push(
            " ON CONFLICT (sequence_number, tx_hash) DO NOTHING \
             RETURNING sequence_number::text, tx_hash",
        );
        let inserted_events = event_query
            .build_query_as::<(String, String)>()
            .fetch_all(&mut *transaction)
            .await
            .map_err(classify_sqlx_error)?
            .into_iter()
            .collect::<BTreeSet<_>>();

        let mut outbox_query = QueryBuilder::<Postgres>::new(ENGINE_OUTBOX_BATCH_INSERT_PREFIX);
        outbox_query.push_values(messages, |mut row, message| {
            let identity = engine_event_identity(&message.tx);
            row.push_bind(identity.clone())
                .push_bind(ENGINE_INPUT_SCHEMA_VERSION)
                .push_bind(identity)
                .push_bind(message.tx.sequence.to_string())
                .push_unseparated("::numeric")
                .push_bind(&message.tx.tx_hash)
                .push_bind(message.tx.chain_id as i64)
                .push_bind(Json(&message.payload));
        });
        outbox_query.push(
            " ON CONFLICT (source_event_identity) DO NOTHING \
             RETURNING source_event_identity",
        );
        let inserted_outbox = outbox_query
            .build_query_as::<(String,)>()
            .fetch_all(&mut *transaction)
            .await
            .map_err(classify_sqlx_error)?
            .into_iter()
            .map(|(identity,)| identity)
            .collect::<BTreeSet<_>>();

        transaction.commit().await.map_err(classify_sqlx_error)?;
        Ok(build_outcomes(
            messages,
            inserted_origins,
            inserted_events,
            inserted_outbox,
        ))
    }

    async fn persist_ingress_evidence(
        &self,
        batch: &IngressFlushBatch,
        sample_limit: usize,
    ) -> Result<IngressPersistOutcome, StoreError> {
        if batch.is_empty() {
            return Ok(IngressPersistOutcome::default());
        }
        if !(1..=1_000).contains(&sample_limit)
            || batch
                .aggregates
                .iter()
                .any(|aggregate| aggregate.event_count > i64::MAX as u64)
        {
            return Err(StoreError::Configuration);
        }
        let mut transaction = self.pool.begin().await.map_err(classify_sqlx_error)?;
        if !batch.aggregates.is_empty() {
            persist_aggregates(&mut transaction, &batch.aggregates).await?;
        }
        let mut samples_inserted = 0_u64;
        let mut sample_limit_reached = 0_u64;
        for sample in &batch.samples {
            match persist_sample(&mut transaction, sample, sample_limit).await? {
                SamplePersistOutcome::Inserted => {
                    samples_inserted = samples_inserted.saturating_add(1);
                }
                SamplePersistOutcome::Duplicate => {}
                SamplePersistOutcome::LimitReached => {
                    sample_limit_reached = sample_limit_reached.saturating_add(1);
                }
            }
        }
        transaction.commit().await.map_err(classify_sqlx_error)?;
        Ok(IngressPersistOutcome {
            aggregate_rows_upserted: batch.aggregates.len() as u64,
            samples_inserted,
            sample_limit_reached,
        })
    }
}

async fn persist_aggregates(
    transaction: &mut sqlx::Transaction<'_, Postgres>,
    aggregates: &[IngressAggregate],
) -> Result<(), StoreError> {
    let mut query = QueryBuilder::<Postgres>::new(
        r#"INSERT INTO money_path_ingress_daily (
    bucket_date, classification, detail_class, router_kind, wrapper_kind,
    selector_kind, event_count, first_seen_at, last_seen_at, schema_version
) "#,
    );
    query.push_values(aggregates, |mut row, aggregate| {
        row.push_bind(aggregate.key.bucket_date)
            .push_bind(&aggregate.key.classification)
            .push_bind(&aggregate.key.detail_class)
            .push_bind(&aggregate.key.router_kind)
            .push_bind(&aggregate.key.wrapper_kind)
            .push_bind(&aggregate.key.selector_kind)
            .push_bind(aggregate.event_count as i64)
            .push_bind(aggregate.first_seen_at)
            .push_bind(aggregate.last_seen_at)
            .push_bind(INGRESS_SCHEMA_VERSION);
    });
    query.push(
        r#" ON CONFLICT (
    bucket_date, classification, detail_class, router_kind, wrapper_kind, selector_kind
) DO UPDATE SET
    event_count = money_path_ingress_daily.event_count + EXCLUDED.event_count,
    first_seen_at = LEAST(money_path_ingress_daily.first_seen_at, EXCLUDED.first_seen_at),
    last_seen_at = GREATEST(money_path_ingress_daily.last_seen_at, EXCLUDED.last_seen_at)"#,
    );
    query
        .build()
        .execute(&mut **transaction)
        .await
        .map_err(classify_sqlx_error)?;
    Ok(())
}

async fn persist_sample(
    transaction: &mut sqlx::Transaction<'_, Postgres>,
    sample: &IngressSample,
    sample_limit: usize,
) -> Result<SamplePersistOutcome, StoreError> {
    let fingerprint = sample_fingerprint(sample)?;
    let lock_key = format!("{}:{}", sample.key.bucket_date, sample.key.detail_class);
    sqlx::query("SELECT pg_advisory_xact_lock(hashtextextended($1, 0))")
        .bind(lock_key)
        .execute(&mut **transaction)
        .await
        .map_err(classify_sqlx_error)?;
    let status: String = sqlx::query_scalar(
        r#"
WITH existing AS (
    SELECT 1
    FROM money_path_ingress_samples
    WHERE bucket_date = $1
      AND detail_class = $3
      AND sample_fingerprint = $7
), next_sample AS (
    SELECT COALESCE(MAX(sample_ordinal), 0) + 1 AS sample_ordinal
    FROM money_path_ingress_samples
    WHERE bucket_date = $1
      AND detail_class = $3
), inserted AS (
INSERT INTO money_path_ingress_samples (
    bucket_date, classification, detail_class, router_kind, wrapper_kind,
    selector_kind, sample_ordinal, sample_fingerprint, safe_decoder_summary,
    observed_at, schema_version
)
SELECT $1, $2, $3, $4, $5, $6, next_sample.sample_ordinal,
       $7, $8, $9, $10
FROM next_sample
WHERE NOT EXISTS (SELECT 1 FROM existing)
  AND next_sample.sample_ordinal <= $11
ON CONFLICT DO NOTHING
RETURNING 1
)
SELECT CASE
    WHEN EXISTS (SELECT 1 FROM inserted) THEN 'inserted'
    WHEN EXISTS (SELECT 1 FROM existing) THEN 'duplicate'
    ELSE 'limit_reached'
END
"#,
    )
    .bind(sample.key.bucket_date)
    .bind(&sample.key.classification)
    .bind(&sample.key.detail_class)
    .bind(&sample.key.router_kind)
    .bind(&sample.key.wrapper_kind)
    .bind(&sample.key.selector_kind)
    .bind(fingerprint.as_slice())
    .bind(Json(&sample.safe_decoder_summary))
    .bind(sample.observed_at)
    .bind(INGRESS_SCHEMA_VERSION)
    .bind(sample_limit as i16)
    .fetch_one(&mut **transaction)
    .await
    .map_err(classify_sqlx_error)?;
    match status.as_str() {
        "inserted" => Ok(SamplePersistOutcome::Inserted),
        "duplicate" => Ok(SamplePersistOutcome::Duplicate),
        "limit_reached" => Ok(SamplePersistOutcome::LimitReached),
        _ => Err(StoreError::Transaction),
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum SamplePersistOutcome {
    Inserted,
    Duplicate,
    LimitReached,
}

fn sample_fingerprint(sample: &IngressSample) -> Result<[u8; 32], StoreError> {
    let encoded = serde_json::to_vec(sample).map_err(|_| StoreError::Configuration)?;
    Ok(Sha256::digest(encoded).into())
}

fn build_outcomes(
    messages: &[ValidatedMessage],
    mut inserted_origins: BTreeSet<String>,
    mut inserted_events: BTreeSet<(String, String)>,
    mut inserted_outbox: BTreeSet<String>,
) -> Vec<PersistOutcome> {
    messages
        .iter()
        .map(|message| {
            let event_key = (message.tx.sequence.to_string(), message.tx.tx_hash.clone());
            PersistOutcome {
                feed_event_inserted: inserted_events.remove(&event_key),
                origin_transaction_inserted: inserted_origins.remove(&message.tx.tx_hash),
                engine_outbox_inserted: inserted_outbox.remove(&engine_event_identity(&message.tx)),
            }
        })
        .collect()
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
    require_unique(snapshot, "engine_outbox", &["outbox_id"])?;
    require_unique(snapshot, "engine_outbox", &["source_event_identity"])?;
    require_unique(
        snapshot,
        "money_path_ingress_daily",
        &[
            "bucket_date",
            "classification",
            "detail_class",
            "router_kind",
            "wrapper_kind",
            "selector_kind",
        ],
    )?;
    require_unique(
        snapshot,
        "money_path_ingress_samples",
        &["bucket_date", "detail_class", "sample_ordinal"],
    )?;
    require_unique(
        snapshot,
        "money_path_ingress_samples",
        &["bucket_date", "detail_class", "sample_fingerprint"],
    )?;

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
    let normalized_outbox_checks = snapshot
        .outbox_checks
        .iter()
        .map(|definition| definition.to_ascii_lowercase().replace(['(', ')'], ""))
        .collect::<Vec<_>>()
        .join(" ");
    for required in [
        "phoenix.engine.input.v1",
        "chain_id = 42161",
        "octet_lengthpayload::text <= 1048576",
        "publish_attempts >= 0",
    ] {
        if !normalized_outbox_checks
            .split_whitespace()
            .collect::<Vec<_>>()
            .join(" ")
            .replace(' ', "")
            .contains(&required.replace(' ', ""))
        {
            return Err(StoreError::Schema(format!(
                "engine_outbox required check is missing: {required}"
            )));
        }
    }
    for index in ["engine_outbox_pending_idx", "engine_outbox_retry_idx"] {
        if !snapshot.indexes.contains(index) {
            return Err(StoreError::Schema(format!(
                "engine_outbox required index is missing: {index}"
            )));
        }
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
            .unique_constraints
            .entry("engine_outbox".to_string())
            .or_default()
            .extend([
                vec!["outbox_id".to_string()],
                vec!["source_event_identity".to_string()],
            ]);
        snapshot
            .unique_constraints
            .entry("money_path_ingress_daily".to_string())
            .or_default()
            .insert(
                [
                    "bucket_date",
                    "classification",
                    "detail_class",
                    "router_kind",
                    "wrapper_kind",
                    "selector_kind",
                ]
                .into_iter()
                .map(str::to_string)
                .collect(),
            );
        snapshot
            .unique_constraints
            .entry("money_path_ingress_samples".to_string())
            .or_default()
            .extend([
                ["bucket_date", "detail_class", "sample_ordinal"]
                    .into_iter()
                    .map(str::to_string)
                    .collect(),
                ["bucket_date", "detail_class", "sample_fingerprint"]
                    .into_iter()
                    .map(str::to_string)
                    .collect(),
            ]);
        snapshot
            .origin_chain_checks
            .push("CHECK ((chain_id = 42161))".to_string());
        snapshot.outbox_checks.extend([
            "CHECK ((schema_version = 'phoenix.engine.input.v1'))".to_string(),
            "CHECK ((chain_id = 42161))".to_string(),
            "CHECK ((octet_length((payload)::text) <= 1048576))".to_string(),
            "CHECK ((publish_attempts >= 0))".to_string(),
        ]);
        snapshot.indexes.extend([
            "engine_outbox_pending_idx".to_string(),
            "engine_outbox_retry_idx".to_string(),
        ]);
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

        let mut snapshot = valid_snapshot();
        snapshot.indexes.remove("engine_outbox_pending_idx");
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
        assert!(ORIGIN_BATCH_INSERT_PREFIX.contains("origin_transactions"));
        assert!(FEED_EVENT_BATCH_INSERT_PREFIX.contains("feed_events"));
        assert!(ENGINE_OUTBOX_BATCH_INSERT_PREFIX.contains("engine_outbox"));
    }

    #[test]
    fn committed_migration_contains_required_recorder_constraints() {
        let migration = include_str!("../../migrations/001_init.sql");
        assert!(migration.contains("tx_hash TEXT NOT NULL UNIQUE"));
        assert!(migration.contains("UNIQUE (sequence_number, tx_hash)"));
        assert!(migration.contains("CHECK (chain_id = 42161)"));

        let outbox = include_str!("../../migrations/004_shadow_engine_runtime.sql");
        for required in [
            "CREATE TABLE IF NOT EXISTS engine_outbox",
            "FOREIGN KEY (source_sequence, tx_hash)",
            "source_event_identity TEXT NOT NULL UNIQUE",
            "octet_length(payload::text) <= 1048576",
            "engine_outbox_pending_idx",
            "engine_outbox_retry_idx",
        ] {
            assert!(outbox.contains(required), "migration missing {required}");
        }
        for destructive in ["DROP TABLE", "TRUNCATE", "DELETE FROM"] {
            assert!(!outbox.contains(destructive));
        }

        let ingress = include_str!("../../migrations/011_money_path_selective_persistence.sql");
        for required in [
            "money_path_ingress_daily",
            "money_path_ingress_samples",
            "sample_ordinal BETWEEN 1 AND 1000",
            "UNIQUE (bucket_date, detail_class, sample_fingerprint)",
            "safe_decoder_summary - ARRAY[",
            "money_path.ingress.v1",
        ] {
            assert!(ingress.contains(required), "migration missing {required}");
        }
        for destructive in ["DROP TABLE", "TRUNCATE", "DELETE FROM", "VACUUM FULL"] {
            assert!(!ingress.contains(destructive));
        }
    }

    #[test]
    fn duplicate_outcome_requires_both_rows_to_exist() {
        assert!(PersistOutcome::default().is_duplicate());
        assert!(!PersistOutcome {
            feed_event_inserted: true,
            origin_transaction_inserted: false,
            engine_outbox_inserted: false,
        }
        .is_duplicate());
    }

    #[test]
    fn database_errors_do_not_include_connection_strings() {
        let display = StoreError::Connection.to_string();
        assert!(!display.contains("postgres://"));
        assert!(!display.to_ascii_lowercase().contains("password"));
    }

    #[test]
    fn sample_fingerprint_is_stable_without_raw_identity_material() {
        let sample = IngressSample {
            key: crate::ingress::IngressAggregateKey {
                bucket_date: chrono::NaiveDate::from_ymd_opt(2026, 7, 19).unwrap(),
                classification: "unsupported_interesting".to_string(),
                detail_class: "known_router_unsupported_exact_output".to_string(),
                router_kind: "legacy_swap_router".to_string(),
                wrapper_kind: "direct".to_string(),
                selector_kind: "legacy_exact_output_single".to_string(),
            },
            safe_decoder_summary: serde_json::json!({
                "router_kind": "legacy_swap_router",
                "outer_selector_kind": "legacy_exact_output_single",
                "wrapper_kind": "direct",
                "decoded_swap_kind": "none",
                "unsupported_reason": "exact_output",
                "command_count": 1,
                "v3_hop_count": 0,
                "reviewed_pool_matches": 0
            }),
            observed_at: chrono::DateTime::parse_from_rfc3339("2026-07-19T00:00:00Z")
                .unwrap()
                .with_timezone(&chrono::Utc),
        };
        let first = sample_fingerprint(&sample).unwrap();
        let second = sample_fingerprint(&sample).unwrap();
        assert_eq!(first, second);
        assert_eq!(first.len(), 32);
        let encoded = serde_json::to_string(&sample).unwrap();
        for forbidden in ["tx_hash", "calldata", "source_event_identity", "http://"] {
            assert!(!encoded.contains(forbidden));
        }
    }

    #[test]
    fn batch_outcomes_count_each_returned_row_once() {
        let first = crate::model::decode_message(&crate::model::tests::sample_payload(7, 'a'))
            .expect("valid first message");
        let second = crate::model::decode_message(&crate::model::tests::sample_payload(8, 'a'))
            .expect("valid duplicate transaction message");
        let messages = vec![first, second];
        let outcomes = build_outcomes(
            &messages,
            BTreeSet::from([messages[0].tx.tx_hash.clone()]),
            BTreeSet::from([
                ("7".to_string(), messages[0].tx.tx_hash.clone()),
                ("8".to_string(), messages[1].tx.tx_hash.clone()),
            ]),
            BTreeSet::from([
                engine_event_identity(&messages[0].tx),
                engine_event_identity(&messages[1].tx),
            ]),
        );
        assert_eq!(outcomes.len(), 2);
        assert!(outcomes[0].origin_transaction_inserted);
        assert!(!outcomes[1].origin_transaction_inserted);
        assert!(outcomes.iter().all(|outcome| outcome.feed_event_inserted));
        assert!(outcomes
            .iter()
            .all(|outcome| outcome.engine_outbox_inserted));
    }
}
