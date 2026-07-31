use crate::persistence::{PostgresShadowStore, StoreError};
use crate::rpc_evaluator::{GatewayClientError, RpcGatewayClient};
use phoenix_recorder::logging::LogSampler;
use rpc_gateway::source_state::{
    SourceEvidenceRequest, SourceEvidenceResponse, SOURCE_EVIDENCE_REQUEST_SCHEMA,
};
use sqlx::types::Json;
use sqlx::{Postgres, Row, Transaction};
use std::sync::Arc;
use std::time::Duration;
use tokio_util::sync::CancellationToken;

const ENRICHMENT_INTERVAL: Duration = Duration::from_secs(15);
const MAX_ENRICHMENT_ATTEMPTS: i32 = 3;

#[derive(Clone, Debug, PartialEq, Eq)]
struct PendingSourceIdentity {
    source_event_identity: String,
    source_identity_hash: String,
    source_transaction_hash: String,
    source_router: String,
    source_factory: String,
    source_feed_sequence: u64,
    source_feed_order_position: u64,
    source_command_index: u16,
    source_pool_path: Vec<String>,
    source_token_path: Vec<String>,
    source_encoded_token_path: String,
    source_fee_path: Vec<u32>,
    state_reconstruction_required: bool,
    next_attempt: i32,
}

pub async fn run_source_enrichment(
    store: PostgresShadowStore,
    client: Arc<RpcGatewayClient>,
    sampler: LogSampler,
    shutdown: CancellationToken,
) {
    loop {
        tokio::select! {
            _ = shutdown.cancelled() => return,
            _ = tokio::time::sleep(ENRICHMENT_INTERVAL) => {}
        }
        let pending = match next_pending(&store).await {
            Ok(value) => value,
            Err(_) => {
                sampled_warning(
                    &sampler,
                    "source_enrichment_store_failure",
                    "phoenix_source_enrichment_store_unavailable",
                );
                continue;
            }
        };
        let Some(pending) = pending else {
            continue;
        };
        let request = SourceEvidenceRequest {
            schema_version: SOURCE_EVIDENCE_REQUEST_SCHEMA.to_string(),
            source_event_identity: pending.source_event_identity.clone(),
            source_identity_hash: pending.source_identity_hash.clone(),
            source_transaction_hash: pending.source_transaction_hash.clone(),
            source_router: pending.source_router.clone(),
            source_factory: pending.source_factory.clone(),
            source_feed_sequence: pending.source_feed_sequence,
            source_feed_order_position: pending.source_feed_order_position,
            source_command_index: pending.source_command_index,
            source_pool_path: pending.source_pool_path.clone(),
            source_token_path: pending.source_token_path.clone(),
            source_encoded_token_path: pending.source_encoded_token_path.clone(),
            source_fee_path: pending.source_fee_path.clone(),
            state_reconstruction_required: pending.state_reconstruction_required,
        };
        match client.fetch_source_evidence(&request).await {
            Ok(response) => {
                if persist_response(&store, &pending, &response).await.is_err() {
                    sampled_warning(
                        &sampler,
                        "source_enrichment_persist_failure",
                        "phoenix_source_enrichment_persist_failed",
                    );
                }
            }
            Err(error) => {
                let (result, reason) = match error {
                    GatewayClientError::Retryable => (
                        "retryable_failure",
                        "source_gateway_temporarily_unavailable",
                    ),
                    GatewayClientError::Integrity => {
                        ("terminal_failure", "source_gateway_integrity_failure")
                    }
                };
                if persist_failure(&store, &pending, result, reason)
                    .await
                    .is_err()
                {
                    sampled_warning(
                        &sampler,
                        "source_enrichment_attempt_failure",
                        "phoenix_source_enrichment_attempt_not_recorded",
                    );
                }
            }
        }
    }
}

async fn next_pending(
    store: &PostgresShadowStore,
) -> Result<Option<PendingSourceIdentity>, StoreError> {
    let row = sqlx::query(
        r#"
SELECT identity.source_event_identity,
       identity.source_identity_hash,
       identity.source_transaction_hash,
       identity.source_router,
       identity.source_factory,
       identity.source_feed_sequence::text AS source_feed_sequence,
       identity.source_feed_order_position::text AS source_feed_order_position,
       identity.source_command_index,
       identity.source_pool_path,
       identity.source_token_path,
       identity.source_encoded_token_path,
       identity.source_fee_path,
       true AS state_reconstruction_required,
       COALESCE(attempts.attempt_count, 0) + 1 AS next_attempt
FROM source_event_identities identity
LEFT JOIN source_block_enrichments enrichment
  ON enrichment.source_event_identity = identity.source_event_identity
LEFT JOIN LATERAL (
    SELECT count(*)::integer AS attempt_count,
           max(attempted_at) AS last_attempted_at
    FROM source_enrichment_attempts attempt
    WHERE attempt.source_event_identity = identity.source_event_identity
) attempts ON true
WHERE enrichment.source_event_identity IS NULL
  AND identity.source_feed_order_position IS NOT NULL
  AND identity.recorded_at >= now() - interval '15 minutes'
  AND COALESCE(attempts.attempt_count, 0) < $1
  AND (
      attempts.last_attempted_at IS NULL
      OR attempts.last_attempted_at <= now() - interval '5 seconds'
  )
ORDER BY identity.recorded_at,
         identity.source_feed_sequence,
         identity.source_feed_order_position
LIMIT 1
"#,
    )
    .bind(MAX_ENRICHMENT_ATTEMPTS)
    .fetch_optional(&store.pool())
    .await
    .map_err(|_| StoreError::Transaction)?;
    let Some(row) = row else {
        return Ok(None);
    };
    let source_feed_sequence = parse_u64(
        &row.try_get::<String, _>("source_feed_sequence")
            .map_err(|_| StoreError::Integrity)?,
    )?;
    let source_feed_order_position = parse_u64(
        &row.try_get::<String, _>("source_feed_order_position")
            .map_err(|_| StoreError::Integrity)?,
    )?;
    let source_command_index = row
        .try_get::<i32, _>("source_command_index")
        .ok()
        .and_then(|value| u16::try_from(value).ok())
        .ok_or(StoreError::Integrity)?;
    let source_pool_path = row
        .try_get::<Json<Vec<String>>, _>("source_pool_path")
        .map_err(|_| StoreError::Integrity)?
        .0;
    let source_token_path = row
        .try_get::<Json<Vec<String>>, _>("source_token_path")
        .map_err(|_| StoreError::Integrity)?
        .0;
    let source_fee_path = row
        .try_get::<Json<Vec<u32>>, _>("source_fee_path")
        .map_err(|_| StoreError::Integrity)?
        .0;
    let next_attempt = row
        .try_get::<i32, _>("next_attempt")
        .map_err(|_| StoreError::Integrity)?;
    Ok(Some(PendingSourceIdentity {
        source_event_identity: row
            .try_get("source_event_identity")
            .map_err(|_| StoreError::Integrity)?,
        source_identity_hash: row
            .try_get("source_identity_hash")
            .map_err(|_| StoreError::Integrity)?,
        source_transaction_hash: row
            .try_get("source_transaction_hash")
            .map_err(|_| StoreError::Integrity)?,
        source_router: row
            .try_get("source_router")
            .map_err(|_| StoreError::Integrity)?,
        source_factory: row
            .try_get("source_factory")
            .map_err(|_| StoreError::Integrity)?,
        source_feed_sequence,
        source_feed_order_position,
        source_command_index,
        source_pool_path,
        source_token_path,
        source_encoded_token_path: row
            .try_get("source_encoded_token_path")
            .map_err(|_| StoreError::Integrity)?,
        source_fee_path,
        state_reconstruction_required: row
            .try_get("state_reconstruction_required")
            .map_err(|_| StoreError::Integrity)?,
        next_attempt,
    }))
}

async fn persist_response(
    store: &PostgresShadowStore,
    pending: &PendingSourceIdentity,
    response: &SourceEvidenceResponse,
) -> Result<(), StoreError> {
    if response.source_event_identity != pending.source_event_identity
        || response.source_identity_hash != pending.source_identity_hash
        || response.source_transaction_hash != pending.source_transaction_hash
    {
        return Err(StoreError::Integrity);
    }
    let mut transaction = store
        .pool()
        .begin()
        .await
        .map_err(|_| StoreError::Transaction)?;
    lock_identity(&mut transaction, &pending.source_event_identity).await?;
    sqlx::query(
        r#"
INSERT INTO source_block_enrichments (
    enrichment_hash, source_event_identity, source_identity_hash, source_chain_id,
    source_transaction_hash, source_block_number, source_block_hash,
    source_transaction_index, source_event_index, source_pool_addresses,
    transaction_status, provider_id, provider_response_hash
) VALUES (
    $1, $2, $3, $4, $5, CAST($6 AS numeric), $7, CAST($8 AS numeric),
    CAST($9 AS numeric), $10, $11, $12, $13
)
ON CONFLICT (source_event_identity) DO NOTHING
"#,
    )
    .bind(&response.enrichment_hash)
    .bind(&response.source_event_identity)
    .bind(&response.source_identity_hash)
    .bind(response.source_chain_id as i64)
    .bind(&response.source_transaction_hash)
    .bind(response.source_block_number.to_string())
    .bind(&response.source_block_hash)
    .bind(response.source_transaction_index.to_string())
    .bind(response.source_event_index.map(|value| value.to_string()))
    .bind(Json(&response.source_pool_addresses))
    .bind(&response.transaction_status)
    .bind(&response.provider_id)
    .bind(&response.provider_response_hash)
    .execute(&mut *transaction)
    .await
    .map_err(|_| StoreError::Transaction)?;
    require_matching_hash(
        &mut transaction,
        "SELECT enrichment_hash FROM source_block_enrichments WHERE source_event_identity = $1",
        &pending.source_event_identity,
        &response.enrichment_hash,
    )
    .await?;

    sqlx::query(
        r#"
INSERT INTO transaction_boundary_state_evidence (
    evidence_hash, source_event_identity, source_identity_hash, enrichment_hash,
    source_block_number, source_block_hash, source_transaction_hash,
    source_transaction_index, parent_block_number, parent_block_hash,
    reconstruction_method, prestate_hash, state_diff_hash,
    post_initiating_state_hash, completeness_status, failure_reason,
    provider_id, provider_response_hash, evidence
) VALUES (
    $1, $2, $3, $4, CAST($5 AS numeric), $6, $7, CAST($8 AS numeric),
    CAST($9 AS numeric), $10, $11, $12, $13, $14, $15, $16, $17, $18, $19
)
ON CONFLICT (source_event_identity) DO NOTHING
"#,
    )
    .bind(&response.evidence_hash)
    .bind(&response.source_event_identity)
    .bind(&response.source_identity_hash)
    .bind(&response.enrichment_hash)
    .bind(response.source_block_number.to_string())
    .bind(&response.source_block_hash)
    .bind(&response.source_transaction_hash)
    .bind(response.source_transaction_index.to_string())
    .bind(response.parent_block_number.to_string())
    .bind(&response.parent_block_hash)
    .bind(&response.reconstruction_method)
    .bind(
        response
            .state_evidence
            .get("prestate_hash")
            .and_then(serde_json::Value::as_str),
    )
    .bind(
        response
            .state_evidence
            .get("state_diff_hash")
            .and_then(serde_json::Value::as_str),
    )
    .bind(response.post_initiating_state_hash.as_deref())
    .bind(&response.completeness_status)
    .bind(response.failure_reason.as_deref())
    .bind(&response.provider_id)
    .bind(&response.provider_response_hash)
    .bind(Json(&response.state_evidence))
    .execute(&mut *transaction)
    .await
    .map_err(|_| StoreError::Transaction)?;
    require_matching_hash(
        &mut transaction,
        "SELECT evidence_hash FROM transaction_boundary_state_evidence WHERE source_event_identity = $1",
        &pending.source_event_identity,
        &response.evidence_hash,
    )
    .await?;
    insert_attempt(&mut transaction, pending, "completed", None).await?;
    transaction
        .commit()
        .await
        .map_err(|_| StoreError::Transaction)
}

async fn persist_failure(
    store: &PostgresShadowStore,
    pending: &PendingSourceIdentity,
    result: &str,
    reason: &str,
) -> Result<(), StoreError> {
    let mut transaction = store
        .pool()
        .begin()
        .await
        .map_err(|_| StoreError::Transaction)?;
    lock_identity(&mut transaction, &pending.source_event_identity).await?;
    insert_attempt(&mut transaction, pending, result, Some(reason)).await?;
    transaction
        .commit()
        .await
        .map_err(|_| StoreError::Transaction)
}

async fn lock_identity(
    transaction: &mut Transaction<'_, Postgres>,
    identity: &str,
) -> Result<(), StoreError> {
    sqlx::query("SELECT pg_advisory_xact_lock(hashtextextended($1, 0))")
        .bind(identity)
        .execute(&mut **transaction)
        .await
        .map_err(|_| StoreError::Transaction)?;
    Ok(())
}

async fn insert_attempt(
    transaction: &mut Transaction<'_, Postgres>,
    pending: &PendingSourceIdentity,
    result: &str,
    reason: Option<&str>,
) -> Result<(), StoreError> {
    sqlx::query(
        r#"
INSERT INTO source_enrichment_attempts (
    source_event_identity, attempt_number, result, failure_reason
) VALUES ($1, $2, $3, $4)
ON CONFLICT (source_event_identity, attempt_number) DO NOTHING
"#,
    )
    .bind(&pending.source_event_identity)
    .bind(pending.next_attempt)
    .bind(result)
    .bind(reason)
    .execute(&mut **transaction)
    .await
    .map_err(|_| StoreError::Transaction)?;
    let row = sqlx::query(
        "SELECT result, failure_reason FROM source_enrichment_attempts \
         WHERE source_event_identity = $1 AND attempt_number = $2",
    )
    .bind(&pending.source_event_identity)
    .bind(pending.next_attempt)
    .fetch_one(&mut **transaction)
    .await
    .map_err(|_| StoreError::Transaction)?;
    let stored_result: String = row.try_get("result").map_err(|_| StoreError::Integrity)?;
    let stored_reason: Option<String> = row
        .try_get("failure_reason")
        .map_err(|_| StoreError::Integrity)?;
    if stored_result != result || stored_reason.as_deref() != reason {
        return Err(StoreError::Integrity);
    }
    Ok(())
}

async fn require_matching_hash(
    transaction: &mut Transaction<'_, Postgres>,
    query: &str,
    identity: &str,
    expected: &str,
) -> Result<(), StoreError> {
    let actual: String = sqlx::query_scalar(query)
        .bind(identity)
        .fetch_one(&mut **transaction)
        .await
        .map_err(|_| StoreError::Transaction)?;
    if actual != expected {
        return Err(StoreError::Integrity);
    }
    Ok(())
}

fn parse_u64(value: &str) -> Result<u64, StoreError> {
    value.parse().map_err(|_| StoreError::Integrity)
}

fn sampled_warning(sampler: &LogSampler, class: &'static str, event: &'static str) {
    if let Some(suppressed) = sampler.sample(class) {
        tracing::warn!(event, failure_class = class, suppressed);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::domain::Address;
    use crate::engine_jetstream::{JetStreamFetcher, MessageFetcher as EngineMessageFetcher};
    use crate::metrics::RuntimeMetrics;
    use crate::persistence::ShadowStore;
    use crate::runtime::{process_delivery, DeliveryDisposition, DependencyRetryPolicy};
    use crate::runtime_state::RuntimeReadiness;
    use crate::shadow_processor::{RouteRegistry, ShadowProcessor, UnavailableEvaluator};
    use async_nats::jetstream::context::Publish;
    use phoenix_recorder::engine_outbox::{OutboxStore, PostgresOutbox};
    use phoenix_recorder::engine_stream::{
        ensure_engine_pipeline, EnginePublisher, JetStreamEnginePublisher, ENGINE_STREAM_NAME,
    };
    use phoenix_recorder::jetstream::{
        ensure_durable_pipeline, MessageFetcher as RecorderMessageFetcher, STREAM_NAME,
    };
    use phoenix_recorder::model::{decode_message, engine_event_identity};
    use phoenix_recorder::persistence::{EventStore, PostgresStore};
    use phoenix_recorder::NATS_SUBJECT;
    use rpc_gateway::source_state::{
        expected_uniswap_v3_pool_addresses, hash_json, SOURCE_EVIDENCE_RESPONSE_SCHEMA,
    };
    use serde_json::{json, Value};
    use sqlx::{PgPool, Row};
    use std::fs;
    use std::time::Duration;

    fn local_service_parity() -> Option<(String, String, String)> {
        let dsn = std::env::var("PHOENIX_TEST_POSTGRES_DSN").ok()?;
        let nats = std::env::var("PHOENIX_TEST_NATS_URL").ok()?;
        let fixture = std::env::var("PHOENIX_SOURCE_IDENTITY_FIXTURE").ok()?;
        assert!(
            dsn.contains("@127.0.0.1:") || dsn.contains("@localhost:"),
            "exact source identity E2E PostgreSQL must be loopback-only"
        );
        assert!(
            nats.starts_with("nats://127.0.0.1:") || nats.starts_with("nats://localhost:"),
            "exact source identity E2E NATS must be loopback-only"
        );
        Some((dsn, nats, fixture))
    }

    async fn apply_exact_source_migrations(pool: &PgPool) {
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
            include_str!("../../migrations/012_live_economic_truth.sql"),
            include_str!("../../migrations/013_economic_loss_ledger.sql"),
            include_str!("../../migrations/014_exact_source_identity.sql"),
        ] {
            sqlx::raw_sql(migration)
                .execute(pool)
                .await
                .expect("apply exact source identity E2E migration");
        }
    }

    fn request_for(pending: &PendingSourceIdentity) -> SourceEvidenceRequest {
        SourceEvidenceRequest {
            schema_version: SOURCE_EVIDENCE_REQUEST_SCHEMA.to_string(),
            source_event_identity: pending.source_event_identity.clone(),
            source_identity_hash: pending.source_identity_hash.clone(),
            source_transaction_hash: pending.source_transaction_hash.clone(),
            source_router: pending.source_router.clone(),
            source_factory: pending.source_factory.clone(),
            source_feed_sequence: pending.source_feed_sequence,
            source_feed_order_position: pending.source_feed_order_position,
            source_command_index: pending.source_command_index,
            source_pool_path: pending.source_pool_path.clone(),
            source_token_path: pending.source_token_path.clone(),
            source_encoded_token_path: pending.source_encoded_token_path.clone(),
            source_fee_path: pending.source_fee_path.clone(),
            state_reconstruction_required: true,
        }
    }

    fn complete_response(request: &SourceEvidenceRequest) -> SourceEvidenceResponse {
        let pool_addresses = expected_uniswap_v3_pool_addresses(
            &request.source_factory,
            &request.source_token_path,
            &request.source_fee_path,
        )
        .expect("derive exact Uniswap V3 pool address");
        let transitions = pool_addresses
            .iter()
            .map(|address| {
                (
                    address.clone(),
                    json!({
                        "pre": {"slot0": "0x01"},
                        "post": {"slot0": "0x02"},
                        "diff_pre": {"slot0": "0x01"},
                        "diff_post": {"slot0": "0x02"}
                    }),
                )
            })
            .collect::<serde_json::Map<String, Value>>();
        let prestate_hash = "4".repeat(64);
        let state_diff_hash = "5".repeat(64);
        let trace_response_hash = hash_json(&json!({
            "prestate_hash": prestate_hash,
            "state_diff_hash": state_diff_hash
        }))
        .expect("hash deterministic trace binding");
        let source_block_number = 12_345_u64;
        let source_block_hash = format!("0x{}", "1".repeat(64));
        let source_transaction_index = 7_u64;
        let parent_block_number = source_block_number - 1;
        let parent_block_hash = format!("0x{}", "2".repeat(64));
        let post_initiating_state_hash = hash_json(&json!({
            "schema_version": "phoenix.post-initiating-state.v1",
            "source_event_identity": request.source_event_identity,
            "source_identity_hash": request.source_identity_hash,
            "source_transaction_hash": request.source_transaction_hash,
            "source_feed_sequence": request.source_feed_sequence,
            "source_feed_order_position": request.source_feed_order_position,
            "source_command_index": request.source_command_index,
            "source_block_number": source_block_number,
            "source_block_hash": source_block_hash,
            "source_transaction_index": source_transaction_index,
            "parent_block_number": parent_block_number,
            "parent_block_hash": parent_block_hash,
            "source_factory": request.source_factory,
            "source_pool_path": request.source_pool_path,
            "source_token_path": request.source_token_path,
            "source_encoded_token_path": request.source_encoded_token_path,
            "source_fee_path": request.source_fee_path,
            "source_pool_addresses": pool_addresses,
            "prestate_hash": prestate_hash,
            "state_diff_hash": state_diff_hash,
            "pool_state_transitions": transitions
        }))
        .expect("hash deterministic post-initiating state");
        let state_evidence = json!({
            "schema_version": "phoenix.transaction-boundary-state.v1",
            "complete": true,
            "source_transaction_hash": request.source_transaction_hash,
            "source_block_number": source_block_number,
            "source_block_hash": source_block_hash,
            "source_transaction_index": source_transaction_index,
            "source_feed_sequence": request.source_feed_sequence,
            "source_feed_order_position": request.source_feed_order_position,
            "source_command_index": request.source_command_index,
            "parent_block_number": parent_block_number,
            "parent_block_hash": parent_block_hash,
            "source_factory": request.source_factory,
            "source_pool_path": request.source_pool_path,
            "source_token_path": request.source_token_path,
            "source_encoded_token_path": request.source_encoded_token_path,
            "source_fee_path": request.source_fee_path,
            "source_pool_addresses": pool_addresses,
            "prestate_hash": prestate_hash,
            "state_diff_hash": state_diff_hash,
            "trace_response_hash": trace_response_hash,
            "pool_state_transitions": transitions
        });
        let mut response = SourceEvidenceResponse {
            schema_version: SOURCE_EVIDENCE_RESPONSE_SCHEMA.to_string(),
            source_event_identity: request.source_event_identity.clone(),
            source_identity_hash: request.source_identity_hash.clone(),
            source_chain_id: 42161,
            source_transaction_hash: request.source_transaction_hash.clone(),
            source_feed_sequence: request.source_feed_sequence,
            source_feed_order_position: request.source_feed_order_position,
            source_command_index: request.source_command_index,
            source_pool_path: request.source_pool_path.clone(),
            source_token_path: request.source_token_path.clone(),
            source_encoded_token_path: request.source_encoded_token_path.clone(),
            source_fee_path: request.source_fee_path.clone(),
            source_block_number,
            source_block_hash,
            source_transaction_index,
            source_event_index: Some(11),
            source_pool_addresses: pool_addresses,
            transaction_status: "success".to_string(),
            parent_block_number,
            parent_block_hash,
            provider_id: "source-parity-provider".to_string(),
            provider_response_hash: "3".repeat(64),
            enrichment_hash: "0".repeat(64),
            reconstruction_method: "debug_trace_transaction_prestate_diff".to_string(),
            post_initiating_state_hash: Some(post_initiating_state_hash),
            completeness_status: "complete".to_string(),
            failure_reason: None,
            state_evidence,
            evidence_hash: "0".repeat(64),
        };
        response.enrichment_hash = response
            .canonical_enrichment_hash()
            .expect("hash deterministic block enrichment");
        response.evidence_hash = response
            .canonical_evidence_hash()
            .expect("hash deterministic state evidence");
        response
            .validate(request)
            .expect("complete response satisfies exact source contract");
        response
    }

    #[test]
    fn enrichment_is_bounded_and_never_expands_execution_authority() {
        assert_eq!(MAX_ENRICHMENT_ATTEMPTS, 3);
        assert!(ENRICHMENT_INTERVAL >= Duration::from_secs(10));
    }

    #[tokio::test]
    async fn nitro_to_engine_identity_and_append_only_enrichment_are_exact() {
        let Some((dsn, nats_url, fixture_path)) = local_service_parity() else {
            return;
        };
        let fixture = fs::read(&fixture_path).expect("read Go-generated Nitro fixture");
        let validated = decode_message(&fixture).expect("decode normalized Nitro fixture");
        assert_eq!(validated.tx.source_feed_order_position, Some(1));

        let pool = PgPool::connect(&dsn)
            .await
            .expect("connect exact source identity E2E PostgreSQL");
        apply_exact_source_migrations(&pool).await;
        sqlx::query(
            "TRUNCATE transaction_boundary_state_evidence, source_enrichment_attempts, \
             source_block_enrichments, source_event_identities, shadow_engine_processing_attempts, \
             shadow_engine_classifications, shadow_decisions, fork_simulation_results, \
             engine_outbox, feed_events, origin_transactions, execution_attempts, executions \
             RESTART IDENTITY CASCADE",
        )
        .execute(&pool)
        .await
        .expect("reset isolated exact source identity E2E state");

        let nats = async_nats::connect(&nats_url)
            .await
            .expect("connect exact source identity E2E NATS");
        let context = async_nats::jetstream::new(nats.clone());
        let _ = context.delete_stream(STREAM_NAME).await;
        let _ = context.delete_stream(ENGINE_STREAM_NAME).await;

        let recorder_consumer = ensure_durable_pipeline(&nats)
            .await
            .expect("create Recorder durable pipeline");
        let publication = Publish::build()
            .message_id(format!(
                "{}:{}",
                validated.tx.sequence, validated.tx.tx_hash
            ))
            .payload(fixture.clone().into());
        context
            .send_publish(NATS_SUBJECT, publication)
            .await
            .expect("publish normalized Nitro fixture")
            .await
            .expect("ack normalized Nitro fixture");
        let mut recorder_deliveries = recorder_consumer
            .fetch_batch(1, Duration::from_secs(1))
            .await
            .expect("fetch normalized Nitro fixture");
        assert_eq!(recorder_deliveries.len(), 1);
        let recorder_delivery = recorder_deliveries.pop().expect("Recorder delivery");
        let recorded =
            decode_message(&recorder_delivery.payload).expect("Recorder validates Nitro fixture");
        let recorder_store = PostgresStore::connect(&dsn, "disable")
            .await
            .expect("connect Recorder PostgreSQL store");
        recorder_store
            .verify_schema()
            .await
            .expect("verify Recorder schema");
        recorder_store
            .persist_batch(std::slice::from_ref(&recorded))
            .await
            .expect("atomically persist Recorder origin/feed/outbox");
        recorder_delivery
            .acker
            .ack_confirmed()
            .await
            .expect("ack Recorder delivery after persistence");

        let outbox = PostgresOutbox::connect(&dsn, "disable")
            .await
            .expect("connect Recorder outbox");
        let rows = outbox
            .claim_batch("source-identity-e2e", 1, Duration::from_secs(30))
            .await
            .expect("claim Recorder Engine outbox");
        assert_eq!(rows.len(), 1);
        assert_eq!(
            rows[0].payload["source_feed_order_position"],
            json!(validated.tx.source_feed_order_position)
        );
        assert_eq!(rows[0].payload["tx_hash"], json!(validated.tx.tx_hash));
        let engine_consumer = ensure_engine_pipeline(&nats)
            .await
            .expect("create Engine durable pipeline");
        let engine_publisher = JetStreamEnginePublisher::new(nats.clone());
        let receipt = engine_publisher
            .publish(&rows[0])
            .await
            .expect("publish recorded fixture to Engine stream");
        outbox
            .mark_published(
                &rows[0].outbox_id,
                "source-identity-e2e",
                receipt.stream_sequence,
            )
            .await
            .expect("commit Engine publication acknowledgement");

        let fetcher = JetStreamFetcher::new(engine_consumer);
        let mut engine_deliveries = fetcher
            .fetch_batch()
            .await
            .expect("fetch recorded Engine input");
        assert_eq!(engine_deliveries.len(), 1);
        let processor = ShadowProcessor::new(
            vec![Address::parse("0x68b3465833fb72a70ecdf485e0e4c7bd8665fc45")
                .expect("reviewed Router02 address")],
            RouteRegistry::from_json("[]").expect("empty E2E route registry"),
            Arc::new(UnavailableEvaluator),
        )
        .expect("construct exact source identity processor");
        let engine_store = PostgresShadowStore::connect(&dsn, "disable")
            .await
            .expect("connect Engine PostgreSQL store");
        engine_store
            .verify_schema()
            .await
            .expect("verify Engine source identity schema");
        let disposition = process_delivery(
            engine_deliveries.pop().expect("Engine delivery"),
            &engine_store,
            &processor,
            &RuntimeReadiness::new(),
            &RuntimeMetrics::default(),
            &LogSampler::new(Duration::ZERO),
            DependencyRetryPolicy::engine_default().expect("bounded Engine retry policy"),
        )
        .await;
        assert_eq!(disposition, DeliveryDisposition::Continue);

        let identity = sqlx::query(
            "SELECT source_transaction_hash, source_feed_sequence::text, \
                    source_feed_order_position::text, source_block_number::text, \
                    source_block_hash, source_transaction_index::text, source_event_index::text, \
                    source_command_index, source_router, source_pool_path, source_token_path, \
                    source_encoded_token_path, source_fee_path, source_identity_hash \
             FROM source_event_identities WHERE source_event_identity = $1",
        )
        .bind(engine_event_identity(&validated.tx))
        .fetch_one(&pool)
        .await
        .expect("load persisted exact source identity");
        assert_eq!(
            identity
                .try_get::<String, _>("source_transaction_hash")
                .unwrap(),
            validated.tx.tx_hash
        );
        assert_eq!(
            identity
                .try_get::<String, _>("source_feed_sequence")
                .unwrap(),
            validated.tx.sequence.to_string()
        );
        assert_eq!(
            identity
                .try_get::<String, _>("source_feed_order_position")
                .unwrap(),
            "1"
        );
        for field in [
            "source_block_number",
            "source_block_hash",
            "source_transaction_index",
            "source_event_index",
        ] {
            assert!(
                identity
                    .try_get::<Option<String>, _>(field)
                    .unwrap()
                    .is_none(),
                "{field} must remain unavailable before canonical enrichment"
            );
        }
        assert_eq!(
            identity.try_get::<i32, _>("source_command_index").unwrap(),
            0
        );
        assert_eq!(
            identity.try_get::<String, _>("source_router").unwrap(),
            "0x68b3465833fb72a70ecdf485e0e4c7bd8665fc45"
        );
        assert_eq!(
            identity
                .try_get::<Json<Vec<String>>, _>("source_pool_path")
                .unwrap()
                .0,
            vec![concat!(
                "0x82af49447d8a07e3bd95bd0d56f35241523fbab1:",
                "0xda10009cbd5d07dd0cecc66161fc93d7c9000da1:500"
            )
            .to_string()]
        );
        assert_eq!(
            identity
                .try_get::<Json<Vec<String>>, _>("source_token_path")
                .unwrap()
                .0,
            vec![
                "0x82af49447d8a07e3bd95bd0d56f35241523fbab1".to_string(),
                "0xda10009cbd5d07dd0cecc66161fc93d7c9000da1".to_string(),
            ]
        );
        assert_eq!(
            identity
                .try_get::<String, _>("source_encoded_token_path")
                .unwrap(),
            concat!(
                "0x82af49447d8a07e3bd95bd0d56f35241523fbab10001f4",
                "da10009cbd5d07dd0cecc66161fc93d7c9000da1"
            )
        );
        assert_eq!(
            identity
                .try_get::<Json<Vec<u32>>, _>("source_fee_path")
                .unwrap()
                .0,
            vec![500]
        );

        let pending = next_pending(&engine_store)
            .await
            .expect("load pending exact source identity")
            .expect("source identity awaits canonical enrichment");
        assert_eq!(
            identity
                .try_get::<String, _>("source_identity_hash")
                .unwrap(),
            pending.source_identity_hash
        );
        let request = request_for(&pending);
        let response = complete_response(&request);
        persist_response(&engine_store, &pending, &response)
            .await
            .expect("atomically append canonical block and state enrichment");
        let stored = sqlx::query(
            "SELECT enrichment.source_transaction_index::text, \
                    enrichment.source_event_index::text, state.completeness_status, \
                    state.post_initiating_state_hash, attempt.result \
             FROM source_block_enrichments enrichment \
             JOIN transaction_boundary_state_evidence state USING (source_event_identity) \
             JOIN source_enrichment_attempts attempt USING (source_event_identity) \
             WHERE enrichment.source_event_identity = $1",
        )
        .bind(&pending.source_event_identity)
        .fetch_one(&pool)
        .await
        .expect("load append-only exact source evidence");
        assert_eq!(
            stored
                .try_get::<String, _>("source_transaction_index")
                .unwrap(),
            response.source_transaction_index.to_string()
        );
        assert_eq!(
            stored.try_get::<String, _>("source_event_index").unwrap(),
            response.source_event_index.unwrap().to_string()
        );
        assert_eq!(
            stored.try_get::<String, _>("completeness_status").unwrap(),
            "complete"
        );
        assert_eq!(stored.try_get::<String, _>("result").unwrap(), "completed");
        assert_eq!(
            stored
                .try_get::<String, _>("post_initiating_state_hash")
                .unwrap(),
            response.post_initiating_state_hash.unwrap()
        );

        assert!(
            sqlx::query(
                "UPDATE source_event_identities SET unavailable_reason = 'changed' \
                 WHERE source_event_identity = $1",
            )
            .bind(&pending.source_event_identity)
            .execute(&pool)
            .await
            .is_err(),
            "source identity mutation must fail closed"
        );
        assert!(
            sqlx::query("DELETE FROM source_block_enrichments WHERE source_event_identity = $1")
                .bind(&pending.source_event_identity)
                .execute(&pool)
                .await
                .is_err(),
            "canonical block enrichment deletion must fail closed"
        );
        for table in [
            "shadow_decisions",
            "fork_simulation_results",
            "execution_attempts",
            "executions",
        ] {
            let query = format!("SELECT count(*) FROM {table}");
            let count: i64 = sqlx::query_scalar(&query)
                .fetch_one(&pool)
                .await
                .expect("count execution-adjacent E2E rows");
            assert_eq!(count, 0, "{table} must remain empty");
        }
    }
}
