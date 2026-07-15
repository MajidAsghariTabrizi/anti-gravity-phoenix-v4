use crate::engine_input::{EngineClassification, InputIdentity};
use crate::opportunity::{
    AgreementState, CostBreakdown, Opportunity, PrimaryProfitabilityStatus, ShadowDisposition,
    SimulationKind, StateSource, VerificationSkipReason, VerificationStatus,
    PROFITABILITY_MODEL_VERSION,
};
use crate::shadow_processor::EvaluatedOpportunity;
use async_trait::async_trait;
use chrono::{DateTime, Utc};
use serde::Serialize;
use serde_json::Value;
use sqlx::postgres::{PgConnectOptions, PgPoolOptions, PgSslMode};
use sqlx::types::Json;
use sqlx::{PgPool, Postgres, Row, Transaction};
use std::collections::{HashMap, HashSet};
use std::str::FromStr;
use std::time::Duration;
use thiserror::Error;

pub(crate) const MAX_EVIDENCE_BYTES: usize = 1024 * 1024;
const MAX_RPC_QUALITY_RECORDS: usize = 512;
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
    pub evaluations: Vec<EvaluatedOpportunity>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PersistOutcome {
    Committed,
    AlreadyFinal,
}

#[derive(Clone, Debug, PartialEq)]
pub struct DependencyFailureContext {
    pub classification: EngineClassification,
    pub detail_class: Option<String>,
    pub evidence: Value,
    pub started_at: DateTime<Utc>,
    pub delivery_attempt: u64,
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
    async fn dependency_failure_context(
        &self,
        source_event_identity: &str,
    ) -> Result<Option<DependencyFailureContext>, StoreError>;
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
      'shadow_engine_processing_attempts',
      'shadow_decisions',
      'rpc_quality_records',
      'shadow_profitability_facts'
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
      'shadow_engine_processing_attempts',
      'shadow_decisions',
      'rpc_quality_records',
      'shadow_profitability_facts'
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
      'shadow_engine_processing_attempts',
      'shadow_decisions',
      'rpc_quality_records',
      'shadow_profitability_facts'
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

        let rows = sqlx::query(
            r#"
SELECT tablename AS table_name, indexdef AS definition
FROM pg_indexes
WHERE schemaname = 'public'
  AND tablename IN (
      'shadow_decisions',
      'rpc_quality_records',
      'shadow_profitability_facts'
  )
"#,
        )
        .fetch_all(&self.pool)
        .await
        .map_err(classify_sqlx_error)?;
        for row in rows {
            let table: String = row.try_get("table_name").map_err(classify_sqlx_error)?;
            let definition: String = row.try_get("definition").map_err(classify_sqlx_error)?;
            snapshot.indexes.entry(table).or_default().push(definition);
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

    async fn dependency_failure_context(
        &self,
        source_event_identity: &str,
    ) -> Result<Option<DependencyFailureContext>, StoreError> {
        if !valid_identity(source_event_identity) {
            return Err(StoreError::Integrity);
        }
        let row = sqlx::query(
            r#"
SELECT classification, error_class, evidence, started_at, delivery_attempt
FROM shadow_engine_processing_attempts
WHERE source_event_identity = $1
ORDER BY delivery_attempt ASC, id ASC
LIMIT 1
"#,
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
        let detail_class: Option<String> =
            row.try_get("error_class").map_err(classify_sqlx_error)?;
        let Json(evidence): Json<Value> = row.try_get("evidence").map_err(classify_sqlx_error)?;
        let started_at: DateTime<Utc> = row.try_get("started_at").map_err(classify_sqlx_error)?;
        let delivery_attempt: i64 = row
            .try_get("delivery_attempt")
            .map_err(classify_sqlx_error)?;
        let evidence_bytes = serde_json::to_vec(&evidence).map_err(|_| StoreError::Integrity)?;
        if classification != EngineClassification::TransientDependencyFailure
            || detail_class
                .as_deref()
                .is_some_and(|value| !bounded_text(value, 1, 128))
            || !evidence.is_object()
            || evidence_bytes.len() > MAX_EVIDENCE_BYTES
            || delivery_attempt < 1
        {
            return Err(StoreError::Integrity);
        }
        Ok(Some(DependencyFailureContext {
            classification,
            detail_class,
            evidence,
            started_at,
            delivery_attempt: delivery_attempt as u64,
        }))
    }

    async fn persist_classification(
        &self,
        record: &ClassificationRecord,
    ) -> Result<PersistOutcome, StoreError> {
        validate_record(record)?;
        let mut transaction = self.pool.begin().await.map_err(classify_sqlx_error)?;

        sqlx::query("SELECT pg_advisory_xact_lock(hashtextextended($1, 0))")
            .bind(&record.identity.source_event_identity)
            .execute(&mut *transaction)
            .await
            .map_err(classify_sqlx_error)?;

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

        for evaluation in &record.evaluations {
            persist_decision(&mut transaction, record, evaluation).await?;
        }

        transaction.commit().await.map_err(classify_sqlx_error)?;
        Ok(PersistOutcome::Committed)
    }
}

async fn persist_decision(
    transaction: &mut Transaction<'_, Postgres>,
    record: &ClassificationRecord,
    evaluation: &EvaluatedOpportunity,
) -> Result<(), StoreError> {
    let opportunity = &evaluation.opportunity;
    let observed_at = timestamp_from_millis(opportunity.identity.observed_at_unix_ms)?;
    let detected_at = timestamp_from_millis(opportunity.identity.detected_at_unix_ms)?;
    let decided_at = timestamp_from_millis(opportunity.decision.decided_at_unix_ms)?;
    let identity_evidence =
        serde_json::to_value(&opportunity.identity).map_err(|_| StoreError::Integrity)?;
    let route_evidence =
        serde_json::to_value(&opportunity.route).map_err(|_| StoreError::Integrity)?;
    let market_evidence =
        serde_json::to_value(&opportunity.market).map_err(|_| StoreError::Integrity)?;
    let economics_evidence =
        serde_json::to_value(&opportunity.economics).map_err(|_| StoreError::Integrity)?;
    let simulation_evidence =
        serde_json::to_value(&opportunity.simulation).map_err(|_| StoreError::Integrity)?;
    let decision_evidence =
        serde_json::to_value(&opportunity.decision).map_err(|_| StoreError::Integrity)?;
    let outcome_evidence =
        serde_json::to_value(&opportunity.outcome).map_err(|_| StoreError::Integrity)?;
    let secondary_reasons = serde_json::to_value(&opportunity.decision.secondary_rejection_reasons)
        .map_err(|_| StoreError::Integrity)?;
    let risk_flags = serde_json::to_value(&opportunity.decision.risk_flags)
        .map_err(|_| StoreError::Integrity)?;
    let disposition = match opportunity.decision.disposition {
        ShadowDisposition::Accepted => "accepted",
        ShadowDisposition::Rejected => "rejected",
    };

    let inserted = sqlx::query(
        r#"
INSERT INTO shadow_decisions (
    id,
    opportunity_id,
    strategy,
    strategy_version,
    detector_version,
    code_version,
    config_version,
    policy_version,
    chain_id,
    source_sequence,
    observed_block,
    state_block,
    quote_block,
    route_fingerprint,
    disposition,
    primary_rejection_reason,
    confidence_bps,
    execution_eligible,
    base_net_pnl,
    conservative_net_pnl,
    severe_net_pnl,
    identity_evidence,
    route_evidence,
    market_evidence,
    economics_evidence,
    simulation_evidence,
    decision_evidence,
    outcome_evidence,
    observed_at,
    detected_at,
    decided_at,
    source_event_identity,
    secondary_rejection_reasons,
    risk_flags,
    processing_latency_ns
) VALUES (
    CAST($1 AS uuid),
    NULL,
    $2,
    $3,
    $4,
    $5,
    $6,
    $7,
    $8,
    CAST($9 AS numeric),
    CAST($10 AS numeric),
    CAST($11 AS numeric),
    CAST($12 AS numeric),
    $13,
    $14,
    $15,
    $16,
    false,
    CAST($17 AS numeric),
    CAST($18 AS numeric),
    CAST($19 AS numeric),
    $20,
    $21,
    $22,
    $23,
    $24,
    $25,
    $26,
    $27,
    $28,
    $29,
    $30,
    $31,
    $32,
    CAST($33 AS numeric)
)
ON CONFLICT (id) DO NOTHING
"#,
    )
    .bind(&opportunity.identity.opportunity_id.0)
    .bind(opportunity.identity.strategy.as_str())
    .bind(&opportunity.identity.strategy_version)
    .bind(&opportunity.identity.detector_version)
    .bind(&opportunity.identity.code_version)
    .bind(&opportunity.identity.config_version)
    .bind(&opportunity.decision.policy_version)
    .bind(opportunity.identity.chain_id as i64)
    .bind(opportunity.identity.source_sequence.to_string())
    .bind(opportunity.identity.observed_block.to_string())
    .bind(opportunity.market.state_block.to_string())
    .bind(opportunity.market.quote_block.to_string())
    .bind(&opportunity.route.route_fingerprint)
    .bind(disposition)
    .bind(
        opportunity
            .decision
            .primary_rejection_reason
            .map(|reason| reason.as_str()),
    )
    .bind(opportunity.decision.confidence_bps as i32)
    .bind(opportunity.economics.base.expected_net_pnl.0.to_string())
    .bind(
        opportunity
            .economics
            .conservative
            .expected_net_pnl
            .0
            .to_string(),
    )
    .bind(opportunity.economics.severe.expected_net_pnl.0.to_string())
    .bind(Json(identity_evidence))
    .bind(Json(route_evidence))
    .bind(Json(market_evidence))
    .bind(Json(economics_evidence))
    .bind(Json(simulation_evidence))
    .bind(Json(decision_evidence))
    .bind(Json(outcome_evidence))
    .bind(observed_at)
    .bind(detected_at)
    .bind(decided_at)
    .bind(&record.identity.source_event_identity)
    .bind(Json(secondary_reasons))
    .bind(Json(risk_flags))
    .bind(record.processing_latency_ns.to_string())
    .execute(&mut **transaction)
    .await
    .map_err(classify_sqlx_error)?;
    if inserted.rows_affected() != 1 {
        return Err(StoreError::Integrity);
    }
    persist_profitability_fact(transaction, record, evaluation, decided_at).await?;

    let block_hash = opportunity
        .market
        .state_block_hash
        .as_deref()
        .ok_or(StoreError::Integrity)?;
    for quality in &evaluation.rpc_quality {
        sqlx::query(
            r#"
INSERT INTO rpc_quality_records (
    shadow_decision_id,
    provider_id,
    method,
    block_number,
    block_hash,
    response_hash,
    latency_ns,
    success,
    stale_result,
    disagreement,
    timeout,
    retry_count
) VALUES (
    CAST($1 AS uuid),
    $2,
    $3,
    CAST($4 AS numeric),
    $5,
    $6,
    CAST($7 AS numeric),
    $8,
    $9,
    $10,
    $11,
    $12
)
"#,
        )
        .bind(&opportunity.identity.opportunity_id.0)
        .bind(&quality.provider_id)
        .bind(&quality.method)
        .bind(
            quality
                .block_number
                .unwrap_or(opportunity.market.state_block)
                .to_string(),
        )
        .bind(quality.block_hash.as_deref().unwrap_or(block_hash))
        .bind(quality.response_hash.as_deref())
        .bind(quality.latency_ns.to_string())
        .bind(quality.success)
        .bind(quality.stale_result)
        .bind(quality.disagreement)
        .bind(quality.timeout)
        .bind(quality.retry_count as i32)
        .execute(&mut **transaction)
        .await
        .map_err(classify_sqlx_error)?;
    }
    Ok(())
}

#[derive(Serialize)]
struct ProfitabilityFactRecord<'a> {
    shadow_decision_id: &'a str,
    source_event_identity: &'a str,
    source_sequence: String,
    transaction_hash: &'a str,
    chain_id: u64,
    route_id: &'a str,
    route_fingerprint: &'a str,
    detected_at: DateTime<Utc>,
    evaluated_at: DateTime<Utc>,
    pinned_block_number: String,
    pinned_block_hash: &'a str,
    primary_state_hash: &'a str,
    token_path: Value,
    pool_path: Value,
    fee_path: Value,
    input_amount: String,
    expected_output: String,
    gross_spread: String,
    gross_profit: String,
    dex_fees: String,
    price_impact: String,
    execution_gas: String,
    gas_price: String,
    arbitrum_execution_fee: String,
    l1_data_fee: String,
    flash_loan_premium: String,
    protocol_fees: String,
    failed_attempt_reserve: String,
    ordering_reserve: String,
    slippage_reserve: String,
    stale_state_reserve: String,
    state_drift_reserve: String,
    latency_reserve: String,
    uncertainty_reserve: String,
    contract_overhead: String,
    total_cost: String,
    expected_net_pnl: String,
    conservative_net_pnl: String,
    severe_net_pnl: String,
    minimum_required_net_pnl: String,
    primary_profitability_status: &'static str,
    disposition: &'static str,
    final_rejection_reason: Option<&'static str>,
    secondary_rejection_reasons: Value,
    model_version: &'a str,
    policy_version: &'a str,
    detector_version: &'a str,
    code_version: &'a str,
    primary_provider_id: &'a str,
    primary_response_hash: &'a str,
    secondary_provider_id: Option<&'a str>,
    secondary_state_hash: Option<&'a str>,
    verification_status: &'static str,
    agreement_state: &'static str,
    verification_skip_reason: Option<&'static str>,
    shadow_only: bool,
    execution_eligible: bool,
    execution_request_created: bool,
    evidence_completeness_status: &'static str,
}

async fn persist_profitability_fact(
    transaction: &mut Transaction<'_, Postgres>,
    record: &ClassificationRecord,
    evaluation: &EvaluatedOpportunity,
    evaluated_at: DateTime<Utc>,
) -> Result<(), StoreError> {
    let opportunity = &evaluation.opportunity;
    let base = &opportunity.economics.base;
    let token_path =
        serde_json::to_value(&opportunity.route.token_path).map_err(|_| StoreError::Integrity)?;
    let pool_path =
        serde_json::to_value(&opportunity.route.pools).map_err(|_| StoreError::Integrity)?;
    let fee_path = serde_json::to_value(
        opportunity
            .route
            .exact_ordered_legs
            .iter()
            .map(|leg| leg.fee)
            .collect::<Vec<_>>(),
    )
    .map_err(|_| StoreError::Integrity)?;
    let secondary_reasons = serde_json::to_value(&opportunity.decision.secondary_rejection_reasons)
        .map_err(|_| StoreError::Integrity)?;
    let disposition = match opportunity.decision.disposition {
        ShadowDisposition::Accepted => "accepted",
        ShadowDisposition::Rejected => "rejected",
    };
    let fact = ProfitabilityFactRecord {
        shadow_decision_id: &opportunity.identity.opportunity_id.0,
        source_event_identity: &record.identity.source_event_identity,
        source_sequence: opportunity.identity.source_sequence.to_string(),
        transaction_hash: &opportunity.identity.origin_tx_hash.0,
        chain_id: opportunity.identity.chain_id,
        route_id: &opportunity.route.route_id.0,
        route_fingerprint: &opportunity.route.route_fingerprint,
        detected_at: timestamp_from_millis(opportunity.identity.detected_at_unix_ms)?,
        evaluated_at,
        pinned_block_number: opportunity.market.state_block.to_string(),
        pinned_block_hash: opportunity
            .market
            .state_block_hash
            .as_deref()
            .ok_or(StoreError::Integrity)?,
        primary_state_hash: opportunity
            .market
            .primary_state_hash
            .as_deref()
            .ok_or(StoreError::Integrity)?,
        token_path,
        pool_path,
        fee_path,
        input_amount: opportunity.route.input_amount.0.to_string(),
        expected_output: opportunity.route.expected_output.0.to_string(),
        gross_spread: base.gross_spread.0.to_string(),
        gross_profit: base.gross_profit.0.to_string(),
        dex_fees: base.pool_fees.0.to_string(),
        price_impact: base.price_impact.0.to_string(),
        execution_gas: base.estimated_execution_gas.to_string(),
        gas_price: base.gas_price_wei.to_string(),
        arbitrum_execution_fee: base.arbitrum_execution_fee.0.to_string(),
        l1_data_fee: base.l1_data_fee.0.to_string(),
        flash_loan_premium: base.flash_loan_fee.0.to_string(),
        protocol_fees: base.protocol_fees.0.to_string(),
        failed_attempt_reserve: base.failure_cost_reserve.0.to_string(),
        ordering_reserve: base.ordering_reserve.0.to_string(),
        slippage_reserve: base.slippage_allowance.0.to_string(),
        stale_state_reserve: base.stale_state_penalty.0.to_string(),
        state_drift_reserve: base.state_drift_reserve.0.to_string(),
        latency_reserve: base.latency_reserve.0.to_string(),
        uncertainty_reserve: base.uncertainty_reserve.0.to_string(),
        contract_overhead: base.contract_overhead.0.to_string(),
        total_cost: base.total_cost.0.to_string(),
        expected_net_pnl: base.expected_net_pnl.0.to_string(),
        conservative_net_pnl: opportunity.economics.conservative.expected_net_pnl.0.to_string(),
        severe_net_pnl: opportunity.economics.severe.expected_net_pnl.0.to_string(),
        minimum_required_net_pnl: opportunity.economics.minimum_required_net_pnl.0.to_string(),
        primary_profitability_status: opportunity.economics.primary_status.as_str(),
        disposition,
        final_rejection_reason: opportunity
            .decision
            .primary_rejection_reason
            .map(|reason| reason.as_str()),
        secondary_rejection_reasons: secondary_reasons,
        model_version: &opportunity.economics.model_version,
        policy_version: &opportunity.decision.policy_version,
        detector_version: &opportunity.identity.detector_version,
        code_version: &opportunity.identity.code_version,
        primary_provider_id: opportunity
            .market
            .primary_provider_id
            .as_deref()
            .ok_or(StoreError::Integrity)?,
        primary_response_hash: opportunity
            .market
            .primary_response_hash
            .as_deref()
            .ok_or(StoreError::Integrity)?,
        secondary_provider_id: opportunity.market.secondary_provider_id.as_deref(),
        secondary_state_hash: opportunity.market.secondary_state_hash.as_deref(),
        verification_status: opportunity.market.verification_status.as_str(),
        agreement_state: opportunity.market.agreement_state.as_str(),
        verification_skip_reason: opportunity
            .market
            .verification_skip_reason
            .map(|reason| reason.as_str()),
        shadow_only: opportunity.decision.shadow_only,
        execution_eligible: opportunity.decision.execution_eligible,
        execution_request_created: opportunity.decision.execution_request_created,
        evidence_completeness_status: "complete",
    };
    let inserted = sqlx::query(
        r#"
INSERT INTO shadow_profitability_facts (
    shadow_decision_id,
    source_event_identity,
    source_sequence,
    transaction_hash,
    chain_id,
    route_id,
    route_fingerprint,
    detected_at,
    evaluated_at,
    pinned_block_number,
    pinned_block_hash,
    primary_state_hash,
    token_path,
    pool_path,
    fee_path,
    input_amount,
    expected_output,
    gross_spread,
    gross_profit,
    dex_fees,
    price_impact,
    execution_gas,
    gas_price,
    arbitrum_execution_fee,
    l1_data_fee,
    flash_loan_premium,
    protocol_fees,
    failed_attempt_reserve,
    ordering_reserve,
    slippage_reserve,
    stale_state_reserve,
    state_drift_reserve,
    latency_reserve,
    uncertainty_reserve,
    contract_overhead,
    total_cost,
    expected_net_pnl,
    conservative_net_pnl,
    severe_net_pnl,
    minimum_required_net_pnl,
    primary_profitability_status,
    disposition,
    final_rejection_reason,
    secondary_rejection_reasons,
    model_version,
    policy_version,
    detector_version,
    code_version,
    primary_provider_id,
    primary_response_hash,
    secondary_provider_id,
    secondary_state_hash,
    verification_status,
    agreement_state,
    verification_skip_reason,
    shadow_only,
    execution_eligible,
    execution_request_created,
    evidence_completeness_status
)
SELECT *
FROM jsonb_to_record($1) AS fact(
    shadow_decision_id uuid,
    source_event_identity text,
    source_sequence numeric,
    transaction_hash text,
    chain_id bigint,
    route_id text,
    route_fingerprint text,
    detected_at timestamptz,
    evaluated_at timestamptz,
    pinned_block_number numeric,
    pinned_block_hash text,
    primary_state_hash text,
    token_path jsonb,
    pool_path jsonb,
    fee_path jsonb,
    input_amount numeric,
    expected_output numeric,
    gross_spread numeric,
    gross_profit numeric,
    dex_fees numeric,
    price_impact numeric,
    execution_gas numeric,
    gas_price numeric,
    arbitrum_execution_fee numeric,
    l1_data_fee numeric,
    flash_loan_premium numeric,
    protocol_fees numeric,
    failed_attempt_reserve numeric,
    ordering_reserve numeric,
    slippage_reserve numeric,
    stale_state_reserve numeric,
    state_drift_reserve numeric,
    latency_reserve numeric,
    uncertainty_reserve numeric,
    contract_overhead numeric,
    total_cost numeric,
    expected_net_pnl numeric,
    conservative_net_pnl numeric,
    severe_net_pnl numeric,
    minimum_required_net_pnl numeric,
    primary_profitability_status text,
    disposition text,
    final_rejection_reason text,
    secondary_rejection_reasons jsonb,
    model_version text,
    policy_version text,
    detector_version text,
    code_version text,
    primary_provider_id text,
    primary_response_hash text,
    secondary_provider_id text,
    secondary_state_hash text,
    verification_status text,
    agreement_state text,
    verification_skip_reason text,
    shadow_only boolean,
    execution_eligible boolean,
    execution_request_created boolean,
    evidence_completeness_status text
)
"#,
    )
    .bind(Json(fact))
    .execute(&mut **transaction)
    .await
    .map_err(classify_sqlx_error)?;
    if inserted.rows_affected() != 1 {
        return Err(StoreError::Integrity);
    }
    Ok(())
}

fn timestamp_from_millis(value: u64) -> Result<DateTime<Utc>, StoreError> {
    i64::try_from(value)
        .ok()
        .and_then(DateTime::<Utc>::from_timestamp_millis)
        .ok_or(StoreError::Integrity)
}

#[derive(Clone, Debug, Default)]
pub struct SchemaSnapshot {
    columns: HashMap<(String, String), ColumnDefinition>,
    unique_constraints: HashMap<String, HashSet<Vec<String>>>,
    check_constraints: HashMap<String, Vec<String>>,
    indexes: HashMap<String, Vec<String>>,
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
    ("shadow_decisions", "id", "uuid", false),
    ("shadow_decisions", "opportunity_id", "uuid", true),
    ("shadow_decisions", "strategy", "text", false),
    ("shadow_decisions", "strategy_version", "text", false),
    ("shadow_decisions", "detector_version", "text", false),
    ("shadow_decisions", "code_version", "text", false),
    ("shadow_decisions", "config_version", "text", false),
    ("shadow_decisions", "policy_version", "text", false),
    ("shadow_decisions", "chain_id", "bigint", false),
    ("shadow_decisions", "source_sequence", "numeric", false),
    ("shadow_decisions", "observed_block", "numeric", false),
    ("shadow_decisions", "state_block", "numeric", false),
    ("shadow_decisions", "quote_block", "numeric", false),
    ("shadow_decisions", "route_fingerprint", "text", false),
    ("shadow_decisions", "disposition", "text", false),
    ("shadow_decisions", "primary_rejection_reason", "text", true),
    ("shadow_decisions", "confidence_bps", "integer", false),
    ("shadow_decisions", "execution_eligible", "boolean", false),
    ("shadow_decisions", "base_net_pnl", "numeric", false),
    ("shadow_decisions", "conservative_net_pnl", "numeric", false),
    ("shadow_decisions", "severe_net_pnl", "numeric", false),
    ("shadow_decisions", "identity_evidence", "jsonb", false),
    ("shadow_decisions", "route_evidence", "jsonb", false),
    ("shadow_decisions", "market_evidence", "jsonb", false),
    ("shadow_decisions", "economics_evidence", "jsonb", false),
    ("shadow_decisions", "simulation_evidence", "jsonb", false),
    ("shadow_decisions", "decision_evidence", "jsonb", false),
    ("shadow_decisions", "outcome_evidence", "jsonb", false),
    (
        "shadow_decisions",
        "observed_at",
        "timestamp with time zone",
        false,
    ),
    (
        "shadow_decisions",
        "detected_at",
        "timestamp with time zone",
        false,
    ),
    (
        "shadow_decisions",
        "decided_at",
        "timestamp with time zone",
        false,
    ),
    ("shadow_decisions", "source_event_identity", "text", true),
    (
        "shadow_decisions",
        "secondary_rejection_reasons",
        "jsonb",
        false,
    ),
    ("shadow_decisions", "risk_flags", "jsonb", false),
    ("shadow_decisions", "processing_latency_ns", "numeric", true),
    ("rpc_quality_records", "id", "bigint", false),
    ("rpc_quality_records", "shadow_decision_id", "uuid", true),
    ("rpc_quality_records", "provider_id", "text", false),
    ("rpc_quality_records", "method", "text", false),
    ("rpc_quality_records", "block_number", "numeric", false),
    ("rpc_quality_records", "block_hash", "text", false),
    ("rpc_quality_records", "response_hash", "text", true),
    ("rpc_quality_records", "latency_ns", "numeric", false),
    ("rpc_quality_records", "success", "boolean", false),
    ("rpc_quality_records", "stale_result", "boolean", false),
    ("rpc_quality_records", "disagreement", "boolean", false),
    ("rpc_quality_records", "timeout", "boolean", false),
    ("rpc_quality_records", "retry_count", "integer", false),
    (
        "shadow_profitability_facts",
        "shadow_decision_id",
        "uuid",
        false,
    ),
    (
        "shadow_profitability_facts",
        "source_event_identity",
        "text",
        true,
    ),
    (
        "shadow_profitability_facts",
        "source_sequence",
        "numeric",
        true,
    ),
    (
        "shadow_profitability_facts",
        "transaction_hash",
        "text",
        true,
    ),
    ("shadow_profitability_facts", "chain_id", "bigint", true),
    ("shadow_profitability_facts", "route_id", "text", true),
    (
        "shadow_profitability_facts",
        "route_fingerprint",
        "text",
        true,
    ),
    (
        "shadow_profitability_facts",
        "detected_at",
        "timestamp with time zone",
        true,
    ),
    (
        "shadow_profitability_facts",
        "evaluated_at",
        "timestamp with time zone",
        false,
    ),
    (
        "shadow_profitability_facts",
        "pinned_block_number",
        "numeric",
        true,
    ),
    (
        "shadow_profitability_facts",
        "pinned_block_hash",
        "text",
        true,
    ),
    (
        "shadow_profitability_facts",
        "primary_state_hash",
        "text",
        true,
    ),
    ("shadow_profitability_facts", "token_path", "jsonb", true),
    ("shadow_profitability_facts", "pool_path", "jsonb", true),
    ("shadow_profitability_facts", "fee_path", "jsonb", true),
    (
        "shadow_profitability_facts",
        "input_amount",
        "numeric",
        true,
    ),
    (
        "shadow_profitability_facts",
        "expected_output",
        "numeric",
        true,
    ),
    (
        "shadow_profitability_facts",
        "gross_spread",
        "numeric",
        true,
    ),
    (
        "shadow_profitability_facts",
        "gross_profit",
        "numeric",
        true,
    ),
    ("shadow_profitability_facts", "dex_fees", "numeric", true),
    (
        "shadow_profitability_facts",
        "price_impact",
        "numeric",
        true,
    ),
    (
        "shadow_profitability_facts",
        "execution_gas",
        "numeric",
        true,
    ),
    ("shadow_profitability_facts", "gas_price", "numeric", true),
    (
        "shadow_profitability_facts",
        "arbitrum_execution_fee",
        "numeric",
        true,
    ),
    ("shadow_profitability_facts", "l1_data_fee", "numeric", true),
    (
        "shadow_profitability_facts",
        "flash_loan_premium",
        "numeric",
        true,
    ),
    (
        "shadow_profitability_facts",
        "protocol_fees",
        "numeric",
        true,
    ),
    (
        "shadow_profitability_facts",
        "failed_attempt_reserve",
        "numeric",
        true,
    ),
    (
        "shadow_profitability_facts",
        "ordering_reserve",
        "numeric",
        true,
    ),
    (
        "shadow_profitability_facts",
        "slippage_reserve",
        "numeric",
        true,
    ),
    (
        "shadow_profitability_facts",
        "stale_state_reserve",
        "numeric",
        true,
    ),
    (
        "shadow_profitability_facts",
        "state_drift_reserve",
        "numeric",
        true,
    ),
    (
        "shadow_profitability_facts",
        "latency_reserve",
        "numeric",
        true,
    ),
    (
        "shadow_profitability_facts",
        "uncertainty_reserve",
        "numeric",
        true,
    ),
    (
        "shadow_profitability_facts",
        "contract_overhead",
        "numeric",
        true,
    ),
    ("shadow_profitability_facts", "total_cost", "numeric", true),
    (
        "shadow_profitability_facts",
        "expected_net_pnl",
        "numeric",
        true,
    ),
    (
        "shadow_profitability_facts",
        "conservative_net_pnl",
        "numeric",
        true,
    ),
    (
        "shadow_profitability_facts",
        "severe_net_pnl",
        "numeric",
        true,
    ),
    (
        "shadow_profitability_facts",
        "minimum_required_net_pnl",
        "numeric",
        true,
    ),
    (
        "shadow_profitability_facts",
        "primary_profitability_status",
        "text",
        false,
    ),
    ("shadow_profitability_facts", "disposition", "text", true),
    (
        "shadow_profitability_facts",
        "final_rejection_reason",
        "text",
        true,
    ),
    (
        "shadow_profitability_facts",
        "secondary_rejection_reasons",
        "jsonb",
        false,
    ),
    ("shadow_profitability_facts", "model_version", "text", true),
    ("shadow_profitability_facts", "policy_version", "text", true),
    (
        "shadow_profitability_facts",
        "detector_version",
        "text",
        true,
    ),
    ("shadow_profitability_facts", "code_version", "text", true),
    (
        "shadow_profitability_facts",
        "primary_provider_id",
        "text",
        true,
    ),
    (
        "shadow_profitability_facts",
        "primary_response_hash",
        "text",
        true,
    ),
    (
        "shadow_profitability_facts",
        "secondary_provider_id",
        "text",
        true,
    ),
    (
        "shadow_profitability_facts",
        "secondary_state_hash",
        "text",
        true,
    ),
    (
        "shadow_profitability_facts",
        "verification_status",
        "text",
        false,
    ),
    (
        "shadow_profitability_facts",
        "agreement_state",
        "text",
        false,
    ),
    (
        "shadow_profitability_facts",
        "verification_skip_reason",
        "text",
        true,
    ),
    (
        "shadow_profitability_facts",
        "shadow_only",
        "boolean",
        false,
    ),
    (
        "shadow_profitability_facts",
        "execution_eligible",
        "boolean",
        false,
    ),
    (
        "shadow_profitability_facts",
        "execution_request_created",
        "boolean",
        false,
    ),
    (
        "shadow_profitability_facts",
        "evidence_completeness_status",
        "text",
        false,
    ),
    (
        "shadow_profitability_facts",
        "created_at",
        "timestamp with time zone",
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
    require_unique(snapshot, "shadow_decisions", &["id"])?;
    require_unique(snapshot, "rpc_quality_records", &["id"])?;
    require_unique(
        snapshot,
        "shadow_profitability_facts",
        &["shadow_decision_id"],
    )?;
    require_index_fragment(
        snapshot,
        "shadow_decisions",
        "(source_event_identity,strategy_version,route_fingerprint)",
    )?;
    require_index_fragment(
        snapshot,
        "shadow_profitability_facts",
        "(evaluated_atdesc,shadow_decision_iddesc)",
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
        "'dependency_exhausted'::text",
        "octet_lengthevidence::text<=1048576",
        "delivery_attempt>=1",
        "execution_eligible=false",
        "shadow_only=true",
        "execution_request_created=false",
        "primary_profitability_status=any",
        "verification_status=any",
        "verification_skip_reason='primary_below_minimum'::text",
        "gross_profit=gross_spread-protocol_fees-dex_fees-price_impact",
        "arbitrum_execution_fee=execution_gas*gas_price",
        "expected_net_pnl=gross_spread-total_cost",
        "jsonb_array_lengthtoken_path=jsonb_array_lengthpool_path+1",
        "retry_count>=0",
        "jsonb_typeofsecondary_rejection_reasons='array'::text",
        "jsonb_typeofrisk_flags='array'::text",
    ] {
        if !checks.contains(required) {
            return Err(StoreError::Schema);
        }
    }
    Ok(())
}

fn require_index_fragment(
    snapshot: &SchemaSnapshot,
    table: &str,
    required: &str,
) -> Result<(), StoreError> {
    let normalized = |value: &str| value.to_ascii_lowercase().replace([' ', '(', ')', '"'], "");
    let required = normalized(required);
    if snapshot.indexes.get(table).is_some_and(|indexes| {
        indexes
            .iter()
            .any(|value| normalized(value).contains(&required))
    }) {
        Ok(())
    } else {
        Err(StoreError::Schema)
    }
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

pub(crate) fn validate_record(record: &ClassificationRecord) -> Result<(), StoreError> {
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
        || record.decision_count != record.evaluations.len()
    {
        return Err(StoreError::Integrity);
    }
    let accepted = record
        .evaluations
        .iter()
        .filter(|value| value.opportunity.decision.disposition == ShadowDisposition::Accepted)
        .count();
    if (record.classification == EngineClassification::ShadowAccepted && accepted == 0)
        || (record.classification == EngineClassification::CandidateRejected && accepted > 0)
        || (!matches!(
            record.classification,
            EngineClassification::ShadowAccepted | EngineClassification::CandidateRejected
        ) && !record.evaluations.is_empty())
    {
        return Err(StoreError::Integrity);
    }
    if record.classification == EngineClassification::DependencyExhausted {
        validate_dependency_exhausted_evidence(record)?;
    }
    let mut opportunity_ids = HashSet::new();
    let mut route_fingerprints = HashSet::new();
    for evaluation in &record.evaluations {
        validate_evaluation(record, evaluation)?;
        if !opportunity_ids.insert(&evaluation.opportunity.identity.opportunity_id.0)
            || !route_fingerprints.insert(&evaluation.opportunity.route.route_fingerprint)
        {
            return Err(StoreError::Integrity);
        }
    }
    Ok(())
}

fn validate_evaluation(
    record: &ClassificationRecord,
    evaluation: &EvaluatedOpportunity,
) -> Result<(), StoreError> {
    let opportunity = &evaluation.opportunity;
    let encoded = serde_json::to_vec(opportunity).map_err(|_| StoreError::Integrity)?;
    let block_hash = opportunity
        .market
        .state_block_hash
        .as_deref()
        .ok_or(StoreError::Integrity)?;
    if opportunity.validate_traceability().is_err()
        || encoded.len() > MAX_EVIDENCE_BYTES
        || !valid_uuid(&opportunity.identity.opportunity_id.0)
        || opportunity.identity.source_sequence != record.identity.source_sequence
        || opportunity.identity.origin_tx_hash.0 != record.identity.tx_hash
        || opportunity.identity.chain_id != record.identity.chain_id
        || opportunity.identity.observed_at_unix_ms == 0
        || opportunity.decision.decided_at_unix_ms < opportunity.identity.detected_at_unix_ms
        || opportunity.market.state_source != StateSource::BlockPinnedRpc
        || opportunity.market.state_block != opportunity.identity.observed_block
        || opportunity.market.quote_block != opportunity.market.state_block
        || !valid_block_hash(block_hash)
        || opportunity.simulation.block_number != opportunity.market.state_block
        || opportunity.simulation.block_hash.as_deref() != Some(block_hash)
        || opportunity.route.input_token != opportunity.route.output_token
        || opportunity.market.pool_states.len() != opportunity.route.pools.len()
        || opportunity
            .market
            .pool_states
            .iter()
            .any(|state| !valid_evidence_hash(&state.state_hash))
        || opportunity
            .market
            .primary_response_hash
            .as_deref()
            .map_or(true, |value| !valid_evidence_hash(value))
        || opportunity
            .market
            .primary_state_hash
            .as_deref()
            .map_or(true, |value| !valid_evidence_hash(value))
        || opportunity
            .market
            .primary_provider_id
            .as_deref()
            .map_or(true, |value| {
                !bounded_text(value, 1, 128) || value.contains("://")
            })
        || !valid_verification_evidence(opportunity)
        || !valid_profitability_evidence(opportunity)
        || !bounded_text(&opportunity.route.route_fingerprint, 1, 256)
        || !bounded_text(&opportunity.identity.strategy_version, 1, 128)
        || !bounded_text(&opportunity.identity.detector_version, 1, 128)
        || !bounded_text(&opportunity.identity.code_version, 1, 128)
        || !bounded_text(&opportunity.identity.config_version, 1, 256)
        || !bounded_text(&opportunity.decision.policy_version, 1, 128)
        || !opportunity.decision.shadow_only
        || opportunity.decision.execution_eligible
        || opportunity.decision.execution_request_created
        || (opportunity.decision.disposition == ShadowDisposition::Accepted
            && opportunity.simulation.kind == SimulationKind::StateBased)
        || evaluation.rpc_quality.is_empty()
        || evaluation.rpc_quality.len() > MAX_RPC_QUALITY_RECORDS
    {
        return Err(StoreError::Integrity);
    }
    for quality in &evaluation.rpc_quality {
        if !bounded_text(&quality.provider_id, 1, 128)
            || quality.provider_id.contains("://")
            || !matches!(
                quality.method.as_str(),
                "eth_chainId" | "eth_getBlockByNumber" | "eth_call"
            )
            || quality
                .block_number
                .is_some_and(|value| value != opportunity.market.state_block)
            || quality
                .block_hash
                .as_deref()
                .is_some_and(|value| value != block_hash)
            || quality
                .response_hash
                .as_deref()
                .is_some_and(|value| !valid_evidence_hash(value))
        {
            return Err(StoreError::Integrity);
        }
    }
    Ok(())
}

fn valid_verification_evidence(opportunity: &Opportunity) -> bool {
    let market = &opportunity.market;
    let primary_provider = market.primary_provider_id.as_deref().unwrap_or_default();
    let secondary_provider_valid = market
        .secondary_provider_id
        .as_deref()
        .map_or(true, |value| {
            bounded_text(value, 1, 128) && !value.contains("://") && value != primary_provider
        });
    let secondary_hash_valid = market
        .secondary_state_hash
        .as_deref()
        .map_or(true, valid_evidence_hash);
    if !secondary_provider_valid || !secondary_hash_valid {
        return false;
    }
    match market.verification_status {
        VerificationStatus::PrimaryOnly => {
            market.agreement_state == AgreementState::NotChecked
                && market.secondary_provider_id.is_none()
                && market.secondary_state_hash.is_none()
                && market.verification_skip_reason
                    == Some(VerificationSkipReason::PrimaryBelowMinimum)
                && opportunity.economics.primary_status == PrimaryProfitabilityStatus::BelowMinimum
        }
        VerificationStatus::Agreed => {
            market.agreement_state == AgreementState::Agreed
                && market.secondary_provider_id.is_some()
                && market.secondary_state_hash == market.primary_state_hash
                && market.verification_skip_reason.is_none()
        }
        VerificationStatus::Disagreed => {
            market.agreement_state == AgreementState::Disagreed
                && market.secondary_provider_id.is_some()
                && market.secondary_state_hash.is_some()
                && market.secondary_state_hash != market.primary_state_hash
                && market.verification_skip_reason.is_none()
        }
        VerificationStatus::SecondaryUnavailable => {
            market.agreement_state == AgreementState::Unavailable
                && market.secondary_provider_id.is_none()
                && market.secondary_state_hash.is_none()
                && market.verification_skip_reason.is_none()
        }
        VerificationStatus::Incomplete | VerificationStatus::HistoricalEvidence => false,
    }
}

fn valid_profitability_evidence(opportunity: &Opportunity) -> bool {
    let economics = &opportunity.economics;
    if economics.model_version != PROFITABILITY_MODEL_VERSION
        || economics.minimum_required_net_pnl.0 < 0
        || economics.primary_status == PrimaryProfitabilityStatus::Incomplete
        || economics.base.expected_net_pnl < economics.conservative.expected_net_pnl
        || economics.conservative.expected_net_pnl < economics.severe.expected_net_pnl
    {
        return false;
    }
    let expected_status = if economics.base.expected_net_pnl >= economics.minimum_required_net_pnl {
        PrimaryProfitabilityStatus::MeetsMinimum
    } else {
        PrimaryProfitabilityStatus::BelowMinimum
    };
    let base_execution_fee = u128::from(economics.base.estimated_execution_gas)
        .checked_mul(economics.base.gas_price_wei);
    economics.primary_status == expected_status
        && base_execution_fee == Some(economics.base.arbitrum_execution_fee.0)
        && valid_cost_breakdown(&economics.base)
        && valid_cost_breakdown(&economics.conservative)
        && valid_cost_breakdown(&economics.severe)
}

fn valid_cost_breakdown(costs: &CostBreakdown) -> bool {
    let total = [
        costs.protocol_fees.0,
        costs.pool_fees.0,
        costs.price_impact.0,
        costs.slippage_allowance.0,
        costs.flash_loan_fee.0,
        costs.arbitrum_execution_fee.0,
        costs.l1_data_fee.0,
        costs.contract_overhead.0,
        costs.failure_cost_reserve.0,
        costs.stale_state_penalty.0,
        costs.ordering_reserve.0,
        costs.state_drift_reserve.0,
        costs.latency_reserve.0,
        costs.uncertainty_reserve.0,
    ]
    .into_iter()
    .try_fold(0_u128, u128::checked_add);
    let market_cost = [
        costs.protocol_fees.0,
        costs.pool_fees.0,
        costs.price_impact.0,
    ]
    .into_iter()
    .try_fold(0_u128, u128::checked_add);
    let (Some(total), Some(market_cost)) = (total, market_cost) else {
        return false;
    };
    let (Ok(total_signed), Ok(market_cost_signed)) =
        (i128::try_from(total), i128::try_from(market_cost))
    else {
        return false;
    };
    let Some(gross_profit) = costs.gross_spread.0.checked_sub(market_cost_signed) else {
        return false;
    };
    let Some(expected_net_pnl) = costs.gross_spread.0.checked_sub(total_signed) else {
        return false;
    };
    costs.total_cost.0 == total
        && costs.gross_profit.0 == gross_profit
        && costs.expected_net_pnl.0 == expected_net_pnl
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

fn valid_uuid(value: &str) -> bool {
    value.len() == 36
        && value.bytes().enumerate().all(|(index, byte)| match index {
            8 | 13 | 18 | 23 => byte == b'-',
            _ => byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte),
        })
}

fn valid_block_hash(value: &str) -> bool {
    value.len() == 66 && value.starts_with("0x") && valid_evidence_hash(&value[2..])
}

fn valid_evidence_hash(value: &str) -> bool {
    value.len() == 64
        && value
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
}

fn bounded_text(value: &str, minimum: usize, maximum: usize) -> bool {
    value.len() >= minimum && value.len() <= maximum && !value.chars().any(char::is_control)
}

fn validate_dependency_exhausted_evidence(record: &ClassificationRecord) -> Result<(), StoreError> {
    let evidence = &record.evidence;
    let first_failure_at = evidence
        .get("first_failure_at")
        .and_then(Value::as_str)
        .and_then(|value| DateTime::parse_from_rfc3339(value).ok())
        .ok_or(StoreError::Integrity)?;
    let final_failure_at = evidence
        .get("final_failure_at")
        .and_then(Value::as_str)
        .and_then(|value| DateTime::parse_from_rfc3339(value).ok())
        .ok_or(StoreError::Integrity)?;
    let provider_identifier = evidence
        .get("provider_identifier")
        .and_then(Value::as_str)
        .ok_or(StoreError::Integrity)?;
    let route_fingerprints = evidence
        .get("route_fingerprints")
        .and_then(Value::as_array)
        .ok_or(StoreError::Integrity)?;
    let original_evidence_retained = evidence
        .get("original_evidence")
        .is_some_and(Value::is_object)
        || evidence
            .get("original_evidence_reference")
            .and_then(Value::as_object)
            .is_some_and(|reference| {
                reference.get("ledger").and_then(Value::as_str)
                    == Some("shadow_engine_processing_attempts")
                    && reference
                        .get("delivery_attempt")
                        .and_then(Value::as_u64)
                        .is_some_and(|attempt| attempt >= 1)
            });
    if record.detail_class != Some("dependency_retries_exhausted")
        || evidence
            .get("source_event_identity")
            .and_then(Value::as_str)
            != Some(record.identity.source_event_identity.as_str())
        || evidence.get("source_sequence").and_then(Value::as_u64)
            != Some(record.identity.source_sequence)
        || evidence.get("tx_hash").and_then(Value::as_str) != Some(record.identity.tx_hash.as_str())
        || evidence
            .get("original_classification")
            .and_then(Value::as_str)
            != Some(EngineClassification::TransientDependencyFailure.as_str())
        || evidence
            .get("original_failure_class")
            .and_then(Value::as_str)
            .map_or(true, |value| !bounded_text(value, 1, 128))
        || evidence
            .get("final_failure_class")
            .and_then(Value::as_str)
            .map_or(true, |value| !bounded_text(value, 1, 128))
        || first_failure_at > final_failure_at
        || evidence
            .get("first_failure_delivery_attempt")
            .and_then(Value::as_u64)
            .map_or(true, |attempt| {
                attempt < 1 || attempt > record.delivery_attempt
            })
        || evidence.get("delivery_attempts").and_then(Value::as_u64)
            != Some(record.delivery_attempt)
        || evidence.get("retry_count").and_then(Value::as_u64)
            != Some(record.delivery_attempt.saturating_sub(1))
        || evidence
            .get("exhaustion_limit")
            .and_then(Value::as_u64)
            .map_or(true, |limit| limit < 2 || limit > record.delivery_attempt)
        || evidence.get("quarantine_reason").and_then(Value::as_str)
            != Some("bounded_dependency_retries_exhausted")
        || !bounded_text(provider_identifier, 1, 128)
        || provider_identifier.contains("://")
        || evidence
            .get("bounded_error_hash")
            .and_then(Value::as_str)
            .map_or(true, |value| !valid_evidence_hash(value))
        || evidence.get("shadow_only").and_then(Value::as_bool) != Some(true)
        || evidence.get("execution_mode").and_then(Value::as_str) != Some("SHADOW")
        || evidence.get("execution_eligible").and_then(Value::as_bool) != Some(false)
        || evidence
            .get("execution_request_created")
            .and_then(Value::as_bool)
            != Some(false)
        || route_fingerprints.len() > 256
        || route_fingerprints.iter().any(|value| {
            value
                .as_str()
                .map_or(true, |fingerprint| !bounded_text(fingerprint, 1, 256))
        })
        || !original_evidence_retained
    {
        return Err(StoreError::Integrity);
    }
    Ok(())
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
        sqlx::Error::Database(database)
            if database.code().is_some_and(|code| code.starts_with("23")) =>
        {
            StoreError::Integrity
        }
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
        snapshot
            .unique_constraints
            .entry("shadow_decisions".to_string())
            .or_default()
            .insert(vec!["id".to_string()]);
        snapshot
            .unique_constraints
            .entry("rpc_quality_records".to_string())
            .or_default()
            .insert(vec!["id".to_string()]);
        snapshot
            .unique_constraints
            .entry("shadow_profitability_facts".to_string())
            .or_default()
            .insert(vec!["shadow_decision_id".to_string()]);
        snapshot.check_constraints.insert(
            "shadow_engine_classifications".to_string(),
            vec![
                "CHECK ((chain_id = 42161))".to_string(),
                "CHECK ((classification = ANY (... 'dependency_exhausted'::text ...)))".to_string(),
                "CHECK ((octet_length((evidence)::text) <= 1048576))".to_string(),
            ],
        );
        snapshot.check_constraints.insert(
            "shadow_engine_processing_attempts".to_string(),
            vec!["CHECK ((delivery_attempt >= 1))".to_string()],
        );
        snapshot.check_constraints.insert(
            "shadow_decisions".to_string(),
            vec![
                "CHECK ((execution_eligible = false))".to_string(),
                "CHECK ((jsonb_typeof(secondary_rejection_reasons) = 'array'::text))".to_string(),
                "CHECK ((jsonb_typeof(risk_flags) = 'array'::text))".to_string(),
            ],
        );
        snapshot.check_constraints.insert(
            "rpc_quality_records".to_string(),
            vec!["CHECK ((retry_count >= 0))".to_string()],
        );
        snapshot.check_constraints.insert(
            "shadow_profitability_facts".to_string(),
            vec![
                "CHECK ((shadow_only = true))".to_string(),
                "CHECK ((execution_eligible = false))".to_string(),
                "CHECK ((execution_request_created = false))".to_string(),
                "CHECK ((primary_profitability_status = ANY (...)))".to_string(),
                "CHECK ((verification_status = ANY (...)))".to_string(),
                "CHECK ((verification_skip_reason = 'primary_below_minimum'::text))".to_string(),
                "CHECK ((gross_profit = gross_spread - protocol_fees - dex_fees - price_impact))"
                    .to_string(),
                "CHECK ((arbitrum_execution_fee = execution_gas * gas_price))".to_string(),
                "CHECK ((expected_net_pnl = gross_spread - total_cost))".to_string(),
                "CHECK ((jsonb_array_length(token_path) = jsonb_array_length(pool_path) + 1))"
                    .to_string(),
            ],
        );
        snapshot.indexes.insert(
            "shadow_decisions".to_string(),
            vec![
                "CREATE UNIQUE INDEX shadow_decisions_source_event_route_idx ON public.shadow_decisions USING btree (source_event_identity, strategy_version, route_fingerprint) WHERE (source_event_identity IS NOT NULL)".to_string(),
            ],
        );
        snapshot.indexes.insert(
            "shadow_profitability_facts".to_string(),
            vec![
                "CREATE INDEX shadow_profitability_evaluated_idx ON public.shadow_profitability_facts USING btree (evaluated_at DESC, shadow_decision_id DESC)".to_string(),
            ],
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
            evaluations: Vec::new(),
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
        let identity_migration = include_str!("../../migrations/005_shadow_decision_identity.sql");
        assert!(identity_migration.contains(
            "UNIQUE (strategy_version, route_fingerprint, source_sequence, observed_block)"
        ));
        assert!(identity_migration.contains("DROP CONSTRAINT"));
        assert!(identity_migration.contains("shadow_decisions_source_event_route_idx"));
        let exhaustion_migration =
            include_str!("../../migrations/006_dependency_exhaustion_quarantine.sql");
        assert!(exhaustion_migration.contains("'dependency_exhausted'"));
        assert!(exhaustion_migration.contains("shadow_engine_classification_value_check"));
        assert!(exhaustion_migration.contains("shadow_engine_attempt_classification_check"));
        let profitability_migration =
            include_str!("../../migrations/007_canonical_profitability_truth.sql");
        for required in [
            "CREATE TABLE IF NOT EXISTS shadow_profitability_facts",
            "evidence_completeness_status <> 'complete'",
            "shadow_only = true",
            "execution_eligible = false",
            "execution_request_created = false",
            "CREATE OR REPLACE VIEW shadow_profitability_report_rows",
            "shadow_profitability_evaluated_idx",
        ] {
            assert!(profitability_migration.contains(required));
        }
    }
}
