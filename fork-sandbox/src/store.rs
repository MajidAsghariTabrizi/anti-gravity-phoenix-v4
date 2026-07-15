use crate::model::{
    CounterfactualResult, PersistedOpportunity, SimulationStatus, UnsignedTransactionPlan,
    ARBITRUM_ONE_CHAIN_ID, PLAN_SCHEMA_VERSION, RESULT_SCHEMA_VERSION,
};
use serde::Serialize;
use serde_json::{to_value, Value};
use sqlx::postgres::{PgConnectOptions, PgPoolOptions, PgSslMode};
use sqlx::types::Json;
use sqlx::{PgPool, Row};
use std::str::FromStr;
use std::time::Duration;
use thiserror::Error;

#[derive(Clone, Debug, Error, PartialEq, Eq)]
pub enum StoreError {
    #[error("fork evidence store configuration is invalid")]
    Configuration,
    #[error("fork evidence store is unavailable")]
    Connection,
    #[error("fork evidence store operation failed")]
    Transaction,
    #[error("fork evidence store rejected invalid evidence")]
    Integrity,
    #[error("fork evidence opportunity was not found")]
    NotFound,
}

#[derive(Clone)]
pub struct ForkEvidenceStore {
    pool: PgPool,
}

impl ForkEvidenceStore {
    pub async fn connect(dsn: &str, ssl_mode: &str) -> Result<Self, StoreError> {
        if dsn.trim().is_empty() || dsn.len() > 4096 {
            return Err(StoreError::Configuration);
        }
        let options = PgConnectOptions::from_str(dsn)
            .map_err(|_| StoreError::Configuration)?
            .ssl_mode(parse_ssl_mode(ssl_mode)?);
        let pool = PgPoolOptions::new()
            .min_connections(0)
            .max_connections(2)
            .acquire_timeout(Duration::from_secs(5))
            .connect_with(options)
            .await
            .map_err(|_| StoreError::Connection)?;
        Ok(Self { pool })
    }

    pub async fn load_opportunity(
        &self,
        shadow_decision_id: &str,
    ) -> Result<PersistedOpportunity, StoreError> {
        if !uuid_shape(shadow_decision_id) {
            return Err(StoreError::Integrity);
        }
        let row = sqlx::query(
            r#"
SELECT shadow_decision_id::text AS shadow_decision_id,
       source_event_identity,
       chain_id,
       route_id,
       route_fingerprint,
       origin_router,
       token_path,
       pool_path,
       pool_address_path,
       protocol_path,
       direction_path,
       fee_path,
       expected_leg_outputs,
       pool_state_hash_path,
       input_amount::text AS input_amount,
       expected_output::text AS expected_output,
       gross_profit::text AS gross_profit,
       total_cost::text AS total_cost,
       expected_net_pnl::text AS expected_net_pnl,
       minimum_required_net_pnl::text AS minimum_required_net_pnl,
       execution_gas::text AS execution_gas,
       gas_price::text AS gas_price,
       detected_at,
       opportunity_expires_at,
       pinned_block_number::text AS pinned_block_number,
       pinned_block_hash,
       primary_state_hash,
       route_config_hash,
       primary_provider_id,
       secondary_provider_id,
       secondary_state_hash,
       secondary_block_number::text AS secondary_block_number,
       secondary_block_hash,
       secondary_route_config_hash,
       verification_status,
       independent_verification_status,
       agreement_state,
       model_version,
       policy_version,
       disposition,
       primary_profitability_status,
       evidence_completeness_status,
       fork_evidence_schema_version,
       shadow_only,
       execution_eligible,
       execution_request_created
FROM shadow_profitability_facts
WHERE shadow_decision_id = CAST($1 AS uuid)
"#,
        )
        .bind(shadow_decision_id)
        .fetch_optional(&self.pool)
        .await
        .map_err(classify_sqlx_error)?
        .ok_or(StoreError::NotFound)?;
        let chain_id = row
            .try_get::<i64, _>("chain_id")
            .ok()
            .and_then(|value| u64::try_from(value).ok())
            .ok_or(StoreError::Integrity)?;
        let pinned_block_number = parse_u64(&row.try_get::<String, _>("pinned_block_number")?)?;
        let secondary_block_number = row
            .try_get::<Option<String>, _>("secondary_block_number")?
            .map(|value| parse_u64(&value))
            .transpose()?;
        Ok(PersistedOpportunity {
            shadow_decision_id: row.try_get("shadow_decision_id")?,
            source_event_identity: row.try_get("source_event_identity")?,
            chain_id,
            route_id: row.try_get("route_id")?,
            route_fingerprint: row.try_get("route_fingerprint")?,
            origin_router: row.try_get("origin_router")?,
            token_path: json_column(&row, "token_path")?,
            pool_path: json_column(&row, "pool_path")?,
            pool_address_path: json_column(&row, "pool_address_path")?,
            protocol_path: json_column(&row, "protocol_path")?,
            direction_path: json_column(&row, "direction_path")?,
            fee_path: json_column(&row, "fee_path")?,
            expected_leg_outputs: json_column(&row, "expected_leg_outputs")?,
            pool_state_hash_path: json_column(&row, "pool_state_hash_path")?,
            input_amount: row.try_get("input_amount")?,
            expected_output: row.try_get("expected_output")?,
            gross_profit: row.try_get("gross_profit")?,
            total_cost: row.try_get("total_cost")?,
            expected_net_pnl: row.try_get("expected_net_pnl")?,
            minimum_required_net_pnl: row.try_get("minimum_required_net_pnl")?,
            execution_gas: row.try_get("execution_gas")?,
            gas_price: row.try_get("gas_price")?,
            detected_at: row.try_get("detected_at")?,
            opportunity_expires_at: row.try_get("opportunity_expires_at")?,
            pinned_block_number,
            pinned_block_hash: row.try_get("pinned_block_hash")?,
            primary_state_hash: row.try_get("primary_state_hash")?,
            route_config_hash: row.try_get("route_config_hash")?,
            primary_provider_id: row.try_get("primary_provider_id")?,
            secondary_provider_id: row.try_get("secondary_provider_id")?,
            secondary_state_hash: row.try_get("secondary_state_hash")?,
            secondary_block_number,
            secondary_block_hash: row.try_get("secondary_block_hash")?,
            secondary_route_config_hash: row.try_get("secondary_route_config_hash")?,
            verification_status: row.try_get("verification_status")?,
            independent_verification_status: row.try_get("independent_verification_status")?,
            agreement_state: row.try_get("agreement_state")?,
            model_version: row.try_get("model_version")?,
            policy_version: row.try_get("policy_version")?,
            disposition: row.try_get("disposition")?,
            primary_profitability_status: row.try_get("primary_profitability_status")?,
            evidence_completeness_status: row.try_get("evidence_completeness_status")?,
            fork_evidence_schema_version: row.try_get("fork_evidence_schema_version")?,
            shadow_only: row.try_get("shadow_only")?,
            execution_eligible: row.try_get("execution_eligible")?,
            execution_request_created: row.try_get("execution_request_created")?,
        })
    }

    pub async fn persist_result(
        &self,
        plan: &UnsignedTransactionPlan,
        result: &CounterfactualResult,
    ) -> Result<(), StoreError> {
        let plan_hash = plan.canonical_hash().map_err(|_| StoreError::Integrity)?;
        let result_hash = CounterfactualResult::from_body(result.body.clone())
            .map_err(|_| StoreError::Integrity)?
            .result_hash;
        if plan.schema_version != PLAN_SCHEMA_VERSION
            || result.body.schema_version != RESULT_SCHEMA_VERSION
            || result.body.plan_hash != plan_hash
            || result.result_hash != result_hash
            || result.body.shadow_decision_id != plan.shadow_decision_id
            || plan.chain_id != ARBITRUM_ONE_CHAIN_ID
            || result.body.fork.chain_id != plan.chain_id
            || result.body.fork.fork_block != plan.pinned_block
            || result.body.fork.local_block.number < plan.pinned_block.number
            || result.body.predicted_gross_profit != plan.predicted.gross_profit
            || result.body.predicted_total_cost != plan.predicted.total_cost
            || result.body.predicted_net_pnl != plan.predicted.net_pnl
            || result.body.model_version != plan.model_version
            || result.body.policy_version != plan.policy_version
            || result.body.evidence.target_code_hash != plan.target_code_hash
            || result.body.evidence.observed_aggregate_state_hash != plan.primary_state_hash
            || !plan.unsigned
            || !plan.fork_only
            || !plan.shadow_only
            || plan.live_execution
            || plan.execution_eligible
            || plan.execution_request_created
            || plan.public_broadcast
            || plan.signer_used
            || !result.body.fork_only
            || !result.body.shadow_only
            || result.body.live_execution
            || result.body.execution_eligible
            || result.body.execution_request_created
            || result.body.public_broadcast
            || result.body.signer_used
        {
            return Err(StoreError::Integrity);
        }
        let record = ResultRecord {
            result_hash: &result.result_hash,
            plan_hash: &result.body.plan_hash,
            shadow_decision_id: &result.body.shadow_decision_id,
            plan_schema_version: &plan.schema_version,
            result_schema_version: &result.body.schema_version,
            plan: to_value(plan).map_err(|_| StoreError::Integrity)?,
            evidence: to_value(&result.body.evidence).map_err(|_| StoreError::Integrity)?,
            status: match result.body.status {
                SimulationStatus::Passed => "passed",
                SimulationStatus::Reverted => "reverted",
            },
            predicted_gross_profit: &result.body.predicted_gross_profit,
            predicted_total_cost: &result.body.predicted_total_cost,
            predicted_net_pnl: &result.body.predicted_net_pnl,
            simulated_gross_profit: result.body.simulated_gross_profit.as_deref(),
            simulated_gas_cost: result.body.simulated_gas_cost.as_deref(),
            simulated_balance_delta: result.body.simulated_balance_delta.as_deref(),
            simulated_net_pnl: result.body.simulated_net_pnl.as_deref(),
            prediction_error: result.body.prediction_error.as_deref(),
            gas_estimate: result.body.gas_estimate.map(|value| value.to_string()),
            gas_used: result.body.gas_used.map(|value| value.to_string()),
            model_version: &result.body.model_version,
            policy_version: &result.body.policy_version,
            fork_chain_id: result.body.fork.chain_id,
            fork_block_number: result.body.fork.fork_block.number.to_string(),
            fork_block_hash: &result.body.fork.fork_block.hash,
            fork_instance_hash: &result.body.fork.fork_instance_hash,
            local_block_number: result.body.fork.local_block.number.to_string(),
            local_block_hash: &result.body.fork.local_block.hash,
            simulated_at: result.body.simulated_at,
            revert_reason: result.body.revert_reason.as_deref(),
            fork_only: true,
            shadow_only: true,
            live_execution: false,
            execution_eligible: false,
            execution_request_created: false,
            public_broadcast: false,
            signer_used: false,
        };
        let inserted = sqlx::query(
            r#"
INSERT INTO fork_simulation_results (
    result_hash, plan_hash, shadow_decision_id, plan_schema_version,
    result_schema_version, plan, evidence, status, predicted_gross_profit,
    predicted_total_cost, predicted_net_pnl, simulated_gross_profit,
    simulated_gas_cost, simulated_balance_delta, simulated_net_pnl,
    prediction_error, gas_estimate, gas_used, model_version, policy_version,
    fork_chain_id, fork_block_number, fork_block_hash, fork_instance_hash,
    local_block_number, local_block_hash, simulated_at, revert_reason,
    fork_only, shadow_only, live_execution, execution_eligible,
    execution_request_created, public_broadcast, signer_used
)
SELECT *
FROM jsonb_to_record($1) AS result(
    result_hash text, plan_hash text, shadow_decision_id uuid,
    plan_schema_version text, result_schema_version text, plan jsonb,
    evidence jsonb, status text, predicted_gross_profit numeric,
    predicted_total_cost numeric, predicted_net_pnl numeric,
    simulated_gross_profit numeric, simulated_gas_cost numeric,
    simulated_balance_delta numeric, simulated_net_pnl numeric,
    prediction_error numeric, gas_estimate numeric, gas_used numeric,
    model_version text, policy_version text, fork_chain_id bigint,
    fork_block_number numeric, fork_block_hash text, fork_instance_hash text,
    local_block_number numeric, local_block_hash text, simulated_at timestamptz,
    revert_reason text, fork_only boolean, shadow_only boolean,
    live_execution boolean, execution_eligible boolean,
    execution_request_created boolean, public_broadcast boolean,
    signer_used boolean
)
"#,
        )
        .bind(Json(record))
        .execute(&self.pool)
        .await
        .map_err(classify_sqlx_error)?;
        if inserted.rows_affected() == 1 {
            Ok(())
        } else {
            Err(StoreError::Transaction)
        }
    }
}

#[derive(Serialize)]
struct ResultRecord<'a> {
    result_hash: &'a str,
    plan_hash: &'a str,
    shadow_decision_id: &'a str,
    plan_schema_version: &'a str,
    result_schema_version: &'a str,
    plan: Value,
    evidence: Value,
    status: &'static str,
    predicted_gross_profit: &'a str,
    predicted_total_cost: &'a str,
    predicted_net_pnl: &'a str,
    simulated_gross_profit: Option<&'a str>,
    simulated_gas_cost: Option<&'a str>,
    simulated_balance_delta: Option<&'a str>,
    simulated_net_pnl: Option<&'a str>,
    prediction_error: Option<&'a str>,
    gas_estimate: Option<String>,
    gas_used: Option<String>,
    model_version: &'a str,
    policy_version: &'a str,
    fork_chain_id: u64,
    fork_block_number: String,
    fork_block_hash: &'a str,
    fork_instance_hash: &'a str,
    local_block_number: String,
    local_block_hash: &'a str,
    simulated_at: chrono::DateTime<chrono::Utc>,
    revert_reason: Option<&'a str>,
    fork_only: bool,
    shadow_only: bool,
    live_execution: bool,
    execution_eligible: bool,
    execution_request_created: bool,
    public_broadcast: bool,
    signer_used: bool,
}

fn json_column<T>(row: &sqlx::postgres::PgRow, column: &str) -> Result<T, StoreError>
where
    T: for<'de> serde::Deserialize<'de>,
{
    row.try_get::<Json<T>, _>(column)
        .map(|value| value.0)
        .map_err(|_| StoreError::Integrity)
}

fn parse_u64(value: &str) -> Result<u64, StoreError> {
    value.parse().map_err(|_| StoreError::Integrity)
}

fn uuid_shape(value: &str) -> bool {
    value.len() == 36
        && value.bytes().enumerate().all(|(index, byte)| match index {
            8 | 13 | 18 | 23 => byte == b'-',
            _ => byte.is_ascii_hexdigit(),
        })
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

impl From<sqlx::Error> for StoreError {
    fn from(error: sqlx::Error) -> Self {
        classify_sqlx_error(error)
    }
}
